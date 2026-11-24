//! One private host step cycle.
//!
//! `open_private_step_cycle` saves a mark and does not scan. `step_private_cycle`
//! spends the call budget on that mark, or on an already published tail. The
//! batch is borrowed for the call only. Pending-close enumeration, token close,
//! and the Owners walk stay on their own meters and are not capped by the budget.

use std::sync::Arc;

use super::ParkedMark;
use vm::Value;

use crate::heap::{
    self, HeapError, HeapLimits, LuaHeap, ScanMeter, StepOutcome, TableRef,
    open_private_step_cycle, step_private_cycle,
};

const BOUND: u32 = 64;

fn limits() -> HeapLimits {
    HeapLimits {
        nodes: 8,
        roots: 4,
        edges: 8,
        max_generation: 8,
    }
}

fn publish(host: &mut vm::HostContext<'_>) -> (TableRef, i64) {
    let raw = crate::publish_raw(host);
    (crate::table_ref(host, raw), raw)
}

fn token_open(host: &mut vm::HostContext<'_>, raw: i64) -> bool {
    host.typed_resource_with_key::<heap::LuaTableResource>(
        crate::resource_handle(raw),
        &heap::LuaTableResource::key(),
    )
    .is_ok()
}

fn slot_generation(heap: &LuaHeap, table: TableRef) -> u32 {
    heap.slots[table.slot() as usize].generation
}

fn clear_meter(host: &mut vm::HostContext<'_>) -> ScanMeter {
    heap::take_scan_meter(host).expect("meter")
}

fn phase_name(host: &mut vm::HostContext<'_>) -> String {
    heap::with_heap(host, |heap| {
        let parked = ParkedMark::resume(heap).expect("saved mark");
        let name = parked.phase_name().to_string();
        let open = parked.mutation_open();
        parked.park().expect("park");
        assert!(!open || name == "roots" || name == "gray");
        Ok(name)
    })
    .expect("phase")
}

fn has_tail(host: &mut vm::HostContext<'_>) -> bool {
    heap::with_heap(host, |heap| Ok(heap.has_sweep_tail())).expect("tail")
}

fn marked(host: &mut vm::HostContext<'_>, table: TableRef) -> bool {
    heap::with_heap(host, |heap| {
        let parked = ParkedMark::resume(heap).expect("saved mark");
        let marked = parked.marked(table);
        parked.park().expect("park");
        Ok(marked)
    })
    .expect("marked")
}

fn foreign_table() -> TableRef {
    let mut other = LuaHeap::new(HeapLimits {
        nodes: 2,
        roots: 1,
        edges: 1,
        max_generation: 2,
    });
    other.alloc().expect("foreign")
}

fn finish(host: &mut vm::HostContext<'_>, batch: &[super::Rooted]) -> heap::CollectReport {
    for _ in 0..BOUND {
        match step_private_cycle(host, 1, batch).expect("step") {
            StepOutcome::Paused { .. } | StepOutcome::Published { .. } => {}
            StepOutcome::Completed(report) => return report,
        }
    }
    panic!("bounded loop did not finish the cycle");
}

#[test]
fn budget_one_pauses_mark_then_one_tail_unit_finishes_once() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let meta = heap::with_heap(&mut host, LuaHeap::alloc).expect("meta");
        let (owner, owner_token) = publish(&mut host);
        let (live, live_token) = publish(&mut host);
        let (weak, weak_token) = publish(&mut host);
        let (extra, extra_token) = publish(&mut host);
        let (orphan, orphan_token) = publish(&mut host);
        let weak_generation = weak.generation();
        let extra_generation = extra.generation();
        let orphan_generation = orphan.generation();
        let (carrier, frame) = heap::with_heap(&mut host, |heap| {
            heap.set_mode_scalar(meta, Value::string("v"))?;
            heap.set_metatable(owner, Some(meta))?;
            heap.set_named_edge(owner, "gone", weak)?;
            let frame = heap.enter_roots(3)?;
            heap.write_root(frame, 0, meta)?;
            heap.write_root(frame, 1, owner)?;
            heap.write_root(frame, 2, live)?;
            let carrier = heap.issue_carrier(live)?;
            Ok((carrier, frame))
        })
        .expect("graph");
        let Value::Array(carrier) = carrier else {
            panic!("live carrier is the issued array");
        };
        let resources_before = host.resource_count();
        clear_meter(&mut host);
        open_private_step_cycle(&mut host).expect("open");

        let illegal = [super::Rooted::Table(foreign_table())];
        let zero =
            step_private_cycle(&mut host, 0, &illegal).expect("zero does not read the batch");
        assert!(matches!(zero, StepOutcome::Paused { tail: false }));
        assert_eq!(clear_meter(&mut host), ScanMeter::default());
        assert_eq!(phase_name(&mut host), "start");
        assert!(!has_tail(&mut host));

        let first = step_private_cycle(&mut host, 1, &[]).expect("budget 1");
        assert!(
            matches!(first, StepOutcome::Paused { tail: false }),
            "budget 1 only advances mark, got {first:?}"
        );
        let started = clear_meter(&mut host);
        assert_eq!(started.mark_passes, 1, "one mark start");
        assert_eq!(started.parked_root_batch_accepts, 0);
        assert_eq!(started.token_closes, 0);
        assert_eq!(started.scratch.pending_close_pushed, 0);
        assert_eq!(started.sweep_edge_checks, 0);
        assert_eq!(started.scratch.dead_slots_pushed, 0);
        assert_eq!(phase_name(&mut host), "mode-open");
        assert!(!has_tail(&mut host));
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 0);

        let again = step_private_cycle(&mut host, 0, &illegal).expect("zero after a pause");
        assert!(matches!(again, StepOutcome::Paused { tail: false }));
        assert_eq!(clear_meter(&mut host), ScanMeter::default());
        assert_eq!(phase_name(&mut host), "mode-open");

        let mut published = None;
        let mut publish_meter = None;
        for _ in 0..BOUND {
            clear_meter(&mut host);
            let outcome = step_private_cycle(&mut host, 1, &[]).expect("mark step");
            if matches!(outcome, StepOutcome::Published { .. }) {
                published = Some(outcome);
                publish_meter = Some(clear_meter(&mut host));
                break;
            }
            assert!(
                matches!(outcome, StepOutcome::Paused { tail: false }),
                "small budgets stay in mark until publish, got {outcome:?}"
            );
        }
        let StepOutcome::Published {
            window_heads,
            window_edges,
        } = published.expect("publish is a phase, not a step-count guess")
        else {
            unreachable!("loop only stores Published");
        };
        assert_eq!(window_heads, 6, "one owner head per slot");
        assert_eq!(window_edges, 2, "mode edge and the weak named edge");
        let publish_meter = publish_meter.expect("publish meter");
        assert_eq!(
            publish_meter.mark_passes, 0,
            "publish does not start another mark"
        );
        assert_eq!(
            publish_meter.scratch.pending_close_pushed, 3,
            "pending-close enumeration is listed apart from the budget"
        );
        assert_eq!(
            publish_meter.token_closes, 3,
            "every doomed token closes in this call, not one per budget"
        );
        assert_eq!(publish_meter.sweep_edge_checks, window_edges);
        assert!(
            publish_meter.owner_head_checks >= window_heads,
            "Owners walks every slot; budget 1 does not cap that walk"
        );
        assert_eq!(publish_meter.scratch.dead_slots_pushed, 3);
        assert_eq!(publish_meter.scratch.sweep_bitmap_inited, window_heads);
        assert_eq!(
            publish_meter.parked_root_batch_accepts, 0,
            "the orphan was not admitted"
        );
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 0);
        assert!(has_tail(&mut host));
        assert!(!token_open(&mut host, weak_token));
        assert!(!token_open(&mut host, extra_token));
        assert!(!token_open(&mut host, orphan_token));
        assert!(token_open(&mut host, live_token));
        assert!(token_open(&mut host, owner_token));
        assert_eq!(host.resource_count(), resources_before - 3);
        heap::with_heap(&mut host, |heap| {
            assert_eq!(heap.named_edge(owner, "gone")?, None, "weak value is nil");
            assert!(heap.alive(weak) && heap.alive(extra) && heap.alive(orphan));
            assert_eq!(slot_generation(heap, weak), weak_generation);
            assert_eq!(slot_generation(heap, extra), extra_generation);
            assert_eq!(slot_generation(heap, orphan), orphan_generation);
            assert!(heap.alive(live));
            Ok(())
        })
        .expect("published occupancy");

        clear_meter(&mut host);
        let tail_zero =
            step_private_cycle(&mut host, 0, &illegal).expect("tail zero ignores batch");
        assert!(matches!(tail_zero, StepOutcome::Paused { tail: true }));
        assert_eq!(clear_meter(&mut host), ScanMeter::default());
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(weak))).expect("still physical"));

        let roots_before = heap::stats(&mut host).expect("roots").root_slots;
        let entered = heap::with_heap(&mut host, |heap| heap.enter_roots(1));
        assert_eq!(entered, Err(HeapError::SweepPending));
        let left = heap::with_heap(&mut host, |heap| heap.leave_roots(frame));
        assert_eq!(left, Err(HeapError::SweepPending));
        heap::with_heap(&mut host, |heap| {
            assert_eq!(heap.current_frame(), Some(frame));
            assert_eq!(heap.stats().root_slots, roots_before);
            Ok(())
        })
        .expect("frame stays");
        assert_eq!(
            open_private_step_cycle(&mut host),
            Err(HeapError::SweepPending)
        );
        assert!(has_tail(&mut host));

        clear_meter(&mut host);
        let one =
            step_private_cycle(&mut host, 1, &illegal).expect("tail ignores an illegal batch");
        assert!(matches!(one, StepOutcome::Paused { tail: true }));
        let release = clear_meter(&mut host);
        assert_eq!(release.node_slot_visits, 1);
        assert_eq!(release.root_slot_visits, 0);
        assert_eq!(release.owner_head_checks, 0);
        assert_eq!(release.sweep_edge_checks, 0);
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 0);
        heap::with_heap(&mut host, |heap| {
            assert!(!heap.alive(weak), "budget 1 releases the first dead slot");
            assert_ne!(slot_generation(heap, weak), weak_generation);
            assert!(heap.alive(extra) && heap.alive(orphan));
            assert_eq!(slot_generation(heap, extra), extra_generation);
            Ok(())
        })
        .expect("one release");

        let mut completed = None;
        for _ in 0..BOUND {
            match step_private_cycle(&mut host, 1, &illegal).expect("tail") {
                StepOutcome::Paused { tail: true } => {}
                StepOutcome::Completed(report) => {
                    completed = Some(report);
                    break;
                }
                other => panic!("tail does not publish again, got {other:?}"),
            }
        }
        let report = completed.expect("one completion report");
        assert_eq!(report.live_nodes, 3);
        assert_eq!(report.live_roots, 3);
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 1);
        assert!(!has_tail(&mut host));
        let Value::Array(after) =
            heap::with_heap(&mut host, |heap| heap.issued_value(live)).expect("carrier")
        else {
            panic!("survivor carrier is still an array");
        };
        assert!(
            Arc::ptr_eq(&carrier, &after),
            "survivor carrier pointer is unchanged"
        );
        heap::with_heap(&mut host, |heap| {
            assert!(heap.alive(live) && heap.alive(owner) && heap.alive(meta));
            assert!(!heap.alive(weak) && !heap.alive(extra) && !heap.alive(orphan));
            Ok(())
        })
        .expect("released");
        assert!(matches!(
            step_private_cycle(&mut host, 1, &[]),
            Err(HeapError::MarkDenied)
        ));
    });
}

#[test]
fn no_dead_slot_completes_without_a_published_tail() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let (live, _) = publish(&mut host);
        heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(1)?;
            heap.write_root(frame, 0, live)?;
            Ok(())
        })
        .expect("root");
        open_private_step_cycle(&mut host).expect("open");
        let mut published = false;
        let mut report = None;
        for _ in 0..BOUND {
            match step_private_cycle(&mut host, 1, &[]).expect("step") {
                StepOutcome::Paused { tail: false } => {}
                StepOutcome::Published { .. } => published = true,
                StepOutcome::Completed(done) => {
                    report = Some(done);
                    break;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(!published, "no dead slot must not publish a tail");
        assert_eq!(report.expect("completed").live_nodes, 1);
        assert!(!has_tail(&mut host));
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 1);
        assert!(matches!(
            step_private_cycle(&mut host, 1, &[]),
            Err(HeapError::MarkDenied)
        ));
    });
}

#[test]
fn dead_slots_without_weak_edges_still_publish_a_tail() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let (live, _) = publish(&mut host);
        let (dead, dead_token) = publish(&mut host);
        let (other, other_token) = publish(&mut host);
        heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(1)?;
            heap.write_root(frame, 0, live)?;
            Ok(())
        })
        .expect("root");
        open_private_step_cycle(&mut host).expect("open");
        let mut published = None;
        for _ in 0..BOUND {
            match step_private_cycle(&mut host, 1, &[]).expect("step") {
                StepOutcome::Paused { tail: false } => {}
                StepOutcome::Published {
                    window_heads,
                    window_edges,
                } => {
                    published = Some((window_heads, window_edges));
                    break;
                }
                other => panic!("zero edges must not be treated as a pause, got {other:?}"),
            }
        }
        let (heads, edges) = published.expect("published tail");
        assert_eq!(heads, 3);
        assert_eq!(edges, 0, "a zero edge count is still the published phase");
        assert!(has_tail(&mut host));
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 0);
        assert!(!token_open(&mut host, dead_token));
        assert!(!token_open(&mut host, other_token));
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.alive(dead) && heap.alive(other)))
                .expect("physical")
        );
        let paused = step_private_cycle(&mut host, 0, &[]).expect("tail saved");
        assert!(matches!(paused, StepOutcome::Paused { tail: true }));
    });
}

#[test]
fn reset_budget_keeps_the_next_batch_and_the_fixed_frame_until_the_next_cycle() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(
            &mut host,
            HeapLimits {
                nodes: 8,
                roots: 4,
                edges: 4,
                max_generation: 8,
            },
        )
        .expect("install");
        let (frame_root, _) = publish(&mut host);
        let (first, _) = publish(&mut host);
        let (second, _) = publish(&mut host);
        let frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(1)?;
            heap.write_root(frame, 0, frame_root)?;
            Ok(frame)
        })
        .expect("frame");
        open_private_step_cycle(&mut host).expect("open");

        let paused = step_private_cycle(&mut host, 1, &[super::Rooted::Table(first)]).expect("A");
        assert!(matches!(paused, StepOutcome::Paused { tail: false }));
        let meter = clear_meter(&mut host);
        assert_eq!(meter.mark_passes, 1);
        assert_eq!(meter.parked_root_batch_accepts, 0, "A was not admitted");
        assert_eq!(phase_name(&mut host), "reset");
        assert!(!marked(&mut host, first));
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(frame_root))).expect("frame root"));

        let mut admitted = false;
        for _ in 0..BOUND {
            let outcome =
                step_private_cycle(&mut host, 1, &[super::Rooted::Table(second)]).expect("B");
            let meter = clear_meter(&mut host);
            if meter.parked_root_batch_accepts == 1 {
                assert!(matches!(
                    outcome,
                    StepOutcome::Paused { tail: false } | StepOutcome::Published { .. }
                ));
                let name = phase_name(&mut host);
                assert!(
                    name == "roots" || name == "gray",
                    "B is admitted in roots/gray, got {name}"
                );
                assert!(marked(&mut host, second));
                assert!(!marked(&mut host, first));
                admitted = true;
                break;
            }
            assert!(matches!(outcome, StepOutcome::Paused { tail: false }));
        }
        assert!(admitted, "B must be admitted before the cycle ends");
        let report = finish(&mut host, &[super::Rooted::Table(second)]);
        assert_eq!(report.live_nodes, 2, "frame root and B stay");
        heap::with_heap(&mut host, |heap| {
            assert!(heap.alive(frame_root) && heap.alive(second));
            assert!(!heap.alive(first), "A was never admitted");
            heap.leave_roots(frame)?;
            Ok(())
        })
        .expect("leave after completion");

        open_private_step_cycle(&mut host).expect("next cycle");
        let _ = finish(&mut host, &[]);
        assert!(!heap::with_heap(&mut host, |heap| Ok(heap.alive(frame_root))).expect("collected"));
    });
}

#[test]
fn omitted_collection_keeps_the_weak_value_and_token() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let meta = heap::with_heap(&mut host, LuaHeap::alloc).expect("meta");
        let (owner, _) = publish(&mut host);
        let (weak, weak_token) = publish(&mut host);
        heap::with_heap(&mut host, |heap| {
            heap.set_mode_scalar(meta, Value::string("v"))?;
            heap.set_metatable(owner, Some(meta))?;
            heap.set_named_edge(owner, "gone", weak)?;
            let frame = heap.enter_roots(2)?;
            heap.write_root(frame, 0, meta)?;
            heap.write_root(frame, 1, owner)?;
            assert_eq!(heap.named_edge(owner, "gone")?, Some(weak));
            Ok(())
        })
        .expect("control");
        assert!(token_open(&mut host, weak_token));
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 0);
        assert!(
            host.scope_state::<LuaHeap>()
                .is_some_and(|heap| heap.mark.is_none())
        );
    });
}

#[test]
fn missing_heap_refuses_open_and_step_without_installing() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        assert!(host.scope_state::<LuaHeap>().is_none());
        assert_eq!(
            open_private_step_cycle(&mut host),
            Err(HeapError::SweepDenied)
        );
        assert!(host.scope_state::<LuaHeap>().is_none());
        assert!(matches!(
            step_private_cycle(&mut host, 1, &[]),
            Err(HeapError::SweepDenied)
        ));
        assert!(host.scope_state::<LuaHeap>().is_none());
    });
}

#[test]
fn repeat_open_refuses_a_saved_mark() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        open_private_step_cycle(&mut host).expect("open");
        assert_eq!(phase_name(&mut host), "start");
        assert_eq!(
            open_private_step_cycle(&mut host),
            Err(HeapError::MarkActive)
        );
        assert_eq!(phase_name(&mut host), "start");
        assert!(!has_tail(&mut host));
    });
}

#[test]
fn illegal_batch_is_refused_before_the_scan() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let (live, _) = publish(&mut host);
        let stale = heap::with_heap(&mut host, |heap| {
            let stale = heap.alloc()?;
            heap.rollback(stale)?;
            Ok(stale)
        })
        .expect("stale");
        open_private_step_cycle(&mut host).expect("open");
        clear_meter(&mut host);

        let foreign = step_private_cycle(
            &mut host,
            1,
            &[
                super::Rooted::Table(live),
                super::Rooted::Table(foreign_table()),
            ],
        );
        assert!(matches!(foreign, Err(HeapError::WrongDomain)));
        assert_eq!(clear_meter(&mut host), ScanMeter::default());
        assert_eq!(phase_name(&mut host), "start");
        assert!(!marked(&mut host, live));

        let stale = step_private_cycle(
            &mut host,
            1,
            &[super::Rooted::Table(live), super::Rooted::Table(stale)],
        );
        assert!(matches!(stale, Err(HeapError::StaleHandle)));
        assert_eq!(clear_meter(&mut host), ScanMeter::default());
        assert_eq!(phase_name(&mut host), "start");
        assert!(!marked(&mut host, live));
    });

    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(
            &mut host,
            HeapLimits {
                nodes: 4,
                roots: 1,
                edges: 1,
                max_generation: 4,
            },
        )
        .expect("install");
        let (first, _) = publish(&mut host);
        let (second, _) = publish(&mut host);
        open_private_step_cycle(&mut host).expect("open");
        clear_meter(&mut host);
        let over = step_private_cycle(
            &mut host,
            1,
            &[super::Rooted::Table(first), super::Rooted::Table(second)],
        );
        assert!(matches!(over, Err(HeapError::RootCapacity)));
        assert_eq!(clear_meter(&mut host), ScanMeter::default());
        assert_eq!(phase_name(&mut host), "start");
        assert!(!marked(&mut host, first));
        assert!(!marked(&mut host, second));
    });
}

#[test]
fn close_obstacle_keeps_the_prefix_and_reopen_fails_until_removed() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, crate::sweep_limits()).expect("install");
        let unrelated = host
            .push_resource(crate::UnrelatedHostResource)
            .expect("unrelated");
        let unrelated_raw = unrelated.handle().raw();
        let (keeper, _) = publish(&mut host);
        crate::root_table(&mut host, keeper);
        let (prefix, prefix_raw) = publish(&mut host);
        let (other, other_raw) = publish(&mut host);
        let doomed = heap::with_heap(&mut host, |heap| {
            let node = heap.alloc()?;
            heap.bind_token(node, unrelated_raw)?;
            Ok(node)
        })
        .expect("foreign token");
        let (suffix, suffix_raw) = publish(&mut host);
        let before_resources = host.resource_count();
        open_private_step_cycle(&mut host).expect("open");

        let (error, meter) = step_until_close_fails(&mut host);
        crate::assert_type_mismatch(&error);
        assert_eq!(
            meter.token_closes, 3,
            "prefix, other, and the foreign attempt"
        );
        assert_eq!(meter.scratch.pending_close_pushed, 4);
        assert_eq!(meter.sweep_edge_checks, 0, "close failure does not publish");
        assert_eq!(meter.scratch.dead_slots_pushed, 0);
        assert!(!has_tail(&mut host));
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 0);
        assert!(host.resource(&unrelated).is_ok());
        assert!(!token_open(&mut host, prefix_raw));
        assert!(!token_open(&mut host, other_raw));
        assert!(token_open(&mut host, suffix_raw));
        let resources_after_failure = host.resource_count();
        assert_eq!(resources_after_failure, before_resources - 2);
        heap::with_heap(&mut host, |heap| {
            assert!(
                ParkedMark::resume(heap).is_none(),
                "taken mark is not restored"
            );
            assert!(heap.alive(prefix) && heap.alive(doomed) && heap.alive(suffix));
            Ok(())
        })
        .expect("prefix stays physical");

        open_private_step_cycle(&mut host).expect("explicit reopen starts a new cycle");
        let (again, again_meter) = step_until_close_fails(&mut host);
        crate::assert_type_mismatch(&again);
        assert_eq!(
            again_meter.token_closes, 1,
            "only the foreign token is attempted"
        );
        assert_eq!(again_meter.scratch.pending_close_pushed, 2);
        assert_eq!(again_meter.sweep_edge_checks, 0);
        assert_eq!(
            host.resource_count(),
            resources_after_failure,
            "closed prefix quota is not returned again"
        );
        assert!(token_open(&mut host, suffix_raw));
        assert!(!has_tail(&mut host));
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 0);

        heap::with_heap(&mut host, |heap| heap.unbind_token(doomed)).expect("fixture unbind");
        crate::root_table(&mut host, prefix);
        open_private_step_cycle(&mut host).expect("recover");
        let report = finish(&mut host, &[]);
        assert_eq!(
            report.live_nodes, 2,
            "keeper and the newly rooted prefix stay"
        );
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 1);
        assert!(!token_open(&mut host, suffix_raw));
        assert_eq!(host.resource_count(), resources_after_failure - 1);
        assert!(host.resource(&unrelated).is_ok());
        heap::with_heap(&mut host, |heap| {
            assert!(heap.alive(keeper) && heap.alive(prefix));
            assert!(!heap.alive(other) && !heap.alive(doomed) && !heap.alive(suffix));
            Ok(())
        })
        .expect("recovered");
    });
}

fn step_until_close_fails(host: &mut vm::HostContext<'_>) -> (HeapError, ScanMeter) {
    for _ in 0..BOUND {
        clear_meter(host);
        match step_private_cycle(host, 1, &[]) {
            Err(error) => return (error, clear_meter(host)),
            Ok(StepOutcome::Paused { tail: false }) => {}
            Ok(other) => panic!("close obstacle must not publish, got {other:?}"),
        }
    }
    panic!("bounded loop did not reach the close obstacle");
}
