//! Host continuation of a mark this scope already saved.
//!
//! The batch is borrowed for that call only. A finished mark closes tokens
//! and publishes the weak graph in the same call. An unfinished or refused
//! call does not sweep. This is not a guest step.

use std::cell::Cell;

use super::super::{SavedMarkResume, resume_private_cycle};
use super::{HeapError, HeapLimits, LuaHeap, ParkedMark, Rooted, ScanMeter, TableRef};
use vm::Value;

const BUDGET: u64 = 64;
const BOUND: u32 = 64;

struct Arm {
    meta: TableRef,
    anchor: TableRef,
    anchor_raw: i64,
    weak: TableRef,
    weak_raw: i64,
    target: TableRef,
    target_raw: i64,
    successor: TableRef,
    successor_raw: i64,
}

struct AtPublish {
    cleared: bool,
    weak_physical: bool,
    weak_judged: bool,
    anchor_alive: bool,
    target_judged: bool,
    target_physical: bool,
    successor_judged: bool,
    successor_physical: bool,
    release_started: bool,
    finished: bool,
}

struct Finished {
    report_nodes: u32,
    meter: ScanMeter,
    collections: u32,
    root_slots: u32,
    live_roots: u32,
    frame: Option<super::RootFrameId>,
    events: u32,
    at: AtPublish,
    anchor_alive: bool,
    weak_alive: bool,
    target_alive: bool,
    successor_alive: bool,
    anchor_open: bool,
    weak_open: bool,
    target_open: bool,
    successor_open: bool,
}

fn limits(roots: u32) -> HeapLimits {
    HeapLimits {
        nodes: 8,
        roots,
        edges: 8,
        max_generation: 8,
    }
}

fn alloc_node(host: &mut vm::HostContext<'_>) -> TableRef {
    crate::heap::with_heap(host, LuaHeap::alloc).expect("node")
}

fn publish_node(host: &mut vm::HostContext<'_>) -> (TableRef, i64) {
    let raw = crate::publish_raw(host);
    (crate::table_ref(host, raw), raw)
}

fn token_open(host: &vm::HostContext<'_>, raw: i64) -> bool {
    host.typed_resource_with_key::<crate::heap::LuaTableResource>(
        crate::resource_handle(raw),
        &crate::heap::LuaTableResource::key(),
    )
    .is_ok()
}

fn alive(host: &mut vm::HostContext<'_>, table: TableRef) -> bool {
    crate::heap::with_heap(host, |heap| Ok(heap.alive(table))).expect("alive")
}

fn meter(host: &mut vm::HostContext<'_>) -> ScanMeter {
    crate::heap::with_heap(host, |heap| Ok(heap.scan)).expect("meter")
}

fn saved_phase(host: &mut vm::HostContext<'_>) -> &'static str {
    crate::heap::with_heap(host, |heap| {
        let parked = ParkedMark::resume(heap).expect("saved mark");
        let name = parked.phase_name();
        parked.park().expect("park");
        Ok(name)
    })
    .expect("phase")
}

fn save_at_start(host: &mut vm::HostContext<'_>) {
    crate::heap::with_heap(host, |heap| {
        let parked = ParkedMark::start(heap).expect("start");
        assert_eq!(parked.phase_name(), "start");
        assert_eq!(parked.meter().mark_passes, 0);
        assert_eq!(parked.stats().root_slots, 0);
        parked.park().expect("park");
        Ok(())
    })
    .expect("save start");
}

fn save_reset_tail(host: &mut vm::HostContext<'_>) {
    crate::heap::with_heap(host, |heap| {
        let mut parked = ParkedMark::start(heap).expect("start");
        let slots = u64::from(parked.stats().node_slots);
        assert!(slots > 0, "reset tail needs existing nodes");
        for _ in 0..BOUND {
            if parked.phase_name() == "reset" && parked.meter().weak_mode_slot_checks == slots {
                assert!(!parked.mutation_open());
                assert_eq!(parked.meter().mark_passes, 1);
                assert_eq!(parked.meter().root_slot_visits, 0);
                assert_eq!(parked.meter().parked_root_batch_accepts, 0);
                parked.park().expect("park");
                return Ok(());
            }
            let view = parked.pulse(1);
            assert!(
                !view.finished,
                "ordinary pulse finished before reset tail, phase {}",
                parked.phase_name()
            );
        }
        panic!(
            "reset tail was not observed, phase {} checks {}",
            parked.phase_name(),
            parked.meter().weak_mode_slot_checks
        );
    })
    .expect("reset tail");
}

fn build_arm(host: &mut vm::HostContext<'_>, roots: u32) -> Arm {
    crate::heap::install(host, limits(roots)).expect("limits");
    let meta = alloc_node(host);
    let (anchor, anchor_raw) = publish_node(host);
    let (weak, weak_raw) = publish_node(host);
    let (target, target_raw) = publish_node(host);
    let (successor, successor_raw) = publish_node(host);
    crate::heap::with_heap(host, |heap| {
        heap.set_mode_scalar(meta, Value::string("v"))
            .expect("mode");
        heap.set_metatable(anchor, Some(meta)).expect("meta");
        heap.set_named_edge(anchor, "w", weak).expect("weak edge");
        heap.set_named_edge(target, "s", successor)
            .expect("strong edge");
        assert_eq!(heap.stats().root_slots, 0);
        assert_eq!(heap.stats().live_roots, 0);
        assert_eq!(heap.current_frame(), None);
        Ok(())
    })
    .expect("graph");
    Arm {
        meta,
        anchor,
        anchor_raw,
        weak,
        weak_raw,
        target,
        target_raw,
        successor,
        successor_raw,
    }
}

fn read_publish(view: &crate::heap::PublishedRead<'_>, arm: &Arm) -> AtPublish {
    AtPublish {
        cleared: view.named(arm.anchor, "w").expect("A.w").is_none(),
        weak_physical: view.still_physical(arm.weak),
        weak_judged: matches!(
            view.alive(arm.weak),
            Err(crate::heap::ObserveFault::JudgedDead)
        ),
        anchor_alive: matches!(view.alive(arm.anchor), Ok(true)),
        target_judged: matches!(
            view.alive(arm.target),
            Err(crate::heap::ObserveFault::JudgedDead)
        ),
        target_physical: view.still_physical(arm.target),
        successor_judged: matches!(
            view.alive(arm.successor),
            Err(crate::heap::ObserveFault::JudgedDead)
        ),
        successor_physical: view.still_physical(arm.successor),
        release_started: view.release_started(),
        finished: view.finished(),
    }
}

fn finish_arm(host: &mut vm::HostContext<'_>, arm: &Arm, batch: &[Rooted]) -> Finished {
    let events = Cell::new(0u32);
    let outcome = resume_private_cycle(host, BUDGET, batch, |view| {
        events.set(events.get() + 1);
        read_publish(view, arm)
    })
    .expect("resume");
    let SavedMarkResume::Completed(report, seen) = outcome else {
        panic!("expected one completed report, not a pause");
    };
    let at = seen.expect("published observation missing");
    assert_eq!(events.get(), 1, "one published observation");
    assert!(at.cleared, "A.w cleared while W was still physical");
    assert!(at.weak_physical, "W still physical at publish");
    assert!(at.weak_judged, "W was judged dead before release");
    assert!(at.anchor_alive, "A was alive at publish");
    assert!(!at.release_started, "release has not started at publish");
    assert!(!at.finished, "observation is not a finished sweep");
    let stats = crate::heap::stats(host).expect("stats");
    let done = Finished {
        report_nodes: report.live_nodes,
        meter: meter(host),
        collections: stats.collections,
        root_slots: stats.root_slots,
        live_roots: stats.live_roots,
        frame: crate::heap::with_heap(host, |heap| Ok(heap.current_frame())).expect("frame"),
        events: events.get(),
        at,
        anchor_alive: alive(host, arm.anchor),
        weak_alive: alive(host, arm.weak),
        target_alive: alive(host, arm.target),
        successor_alive: alive(host, arm.successor),
        anchor_open: token_open(host, arm.anchor_raw),
        weak_open: token_open(host, arm.weak_raw),
        target_open: token_open(host, arm.target_raw),
        successor_open: token_open(host, arm.successor_raw),
    };
    assert!(done.anchor_alive, "A must stay alive");
    assert!(done.anchor_open, "A token stays open");
    assert!(!done.weak_alive, "W is released after the call");
    assert!(!done.weak_open, "W token is closed");
    assert!(alive(host, arm.meta), "the metatable stays with A");
    assert_eq!(
        done.meter.mark_passes, 1,
        "same saved pass, not a remark and not skipped"
    );
    assert_eq!(done.collections, 1, "one sweep in this call");
    assert_eq!(
        done.root_slots, 0,
        "the batch was not written to a fixed slot"
    );
    assert_eq!(done.live_roots, 0);
    assert!(done.frame.is_none(), "no hidden root frame");
    assert_eq!(done.meter.parked_root_writes, 0);
    done
}

fn keep_failure(done: &Finished) -> String {
    format!(
        "K/S should survive the [A, K] batch but were collected; mark_passes={} accepts={} token_closes={} collections={} events={} cleared={} w_physical={} w_alive={} k_judged_dead_at_publish={} k_physical_at_publish={} s_judged_dead_at_publish={} s_physical_at_publish={} live_nodes={} root_slots={}",
        done.meter.mark_passes,
        done.meter.parked_root_batch_accepts,
        done.meter.token_closes,
        done.collections,
        done.events,
        done.at.cleared,
        done.at.weak_physical,
        done.weak_alive,
        done.at.target_judged,
        done.at.target_physical,
        done.at.successor_judged,
        done.at.successor_physical,
        done.report_nodes,
        done.root_slots
    )
}

/// Same graph as the omitted-target arm. This arm hands `[A, K]`.
#[test]
fn saved_resume_keeps_batch_target_and_sweeps_weak_value() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let arm = build_arm(&mut host, 2);
        save_reset_tail(&mut host);
        let done = finish_arm(
            &mut host,
            &arm,
            &[Rooted::Table(arm.anchor), Rooted::Table(arm.target)],
        );
        assert!(
            done.target_alive && done.successor_alive && done.target_open && done.successor_open,
            "{}",
            keep_failure(&done)
        );
        assert_eq!(done.meter.parked_root_batch_accepts, 2);
        assert_eq!(done.meter.token_closes, 1, "only W is closed");
        assert_eq!(done.report_nodes, 4, "M, A, K, and S stay");
        assert_eq!(done.meter.scratch.dead_slots_pushed, 1);
    });
}

/// Same graph as the keep arm. This arm hands only `[A]`.
#[test]
fn saved_resume_omitted_target_is_collected_with_the_weak_value() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let arm = build_arm(&mut host, 2);
        save_reset_tail(&mut host);
        let done = finish_arm(&mut host, &arm, &[Rooted::Table(arm.anchor)]);
        assert!(
            !done.target_alive && !done.successor_alive,
            "K/S must be collected when the batch omits K"
        );
        assert!(!done.target_open && !done.successor_open);
        assert_eq!(done.meter.parked_root_batch_accepts, 1);
        assert_eq!(done.meter.token_closes, 3, "W, K, and S close");
        assert_eq!(done.report_nodes, 2, "only M and A stay");
        assert_eq!(done.meter.scratch.dead_slots_pushed, 3);
    });
}

#[test]
fn saved_resume_without_saved_state_is_denied_and_does_not_install() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let events = Cell::new(0u32);
        let missing = resume_private_cycle(&mut host, 0, &[], |_| {
            events.set(events.get() + 1);
        })
        .expect_err("missing heap");
        assert!(matches!(missing, HeapError::MarkDenied));
        assert!(host.scope_state::<LuaHeap>().is_none());
        let missing = resume_private_cycle(&mut host, 4, &[], |_| {
            events.set(events.get() + 1);
        })
        .expect_err("missing heap, positive budget");
        assert!(matches!(missing, HeapError::MarkDenied));
        assert!(
            host.scope_state::<LuaHeap>().is_none(),
            "denial must not install a heap"
        );
        assert_eq!(events.get(), 0);

        crate::heap::install(&mut host, limits(1)).expect("install");
        let denied = resume_private_cycle(&mut host, 0, &[], |_| {
            events.set(events.get() + 1);
        })
        .expect_err("installed heap has no saved mark");
        assert!(matches!(denied, HeapError::MarkDenied));
        let denied = resume_private_cycle(&mut host, 4, &[], |_| {
            events.set(events.get() + 1);
        })
        .expect_err("positive budget does not start a mark");
        assert!(matches!(denied, HeapError::MarkDenied));
        assert_eq!(events.get(), 0);
        assert_eq!(meter(&mut host).mark_passes, 0);
        assert_eq!(crate::heap::stats(&mut host).expect("stats").collections, 0);
        assert_eq!(crate::heap::stats(&mut host).expect("stats").live_nodes, 0);
        assert!(
            !crate::heap::with_heap(&mut host, |heap| Ok(heap.has_saved_mark())).expect("saved")
        );
    });
}

#[test]
fn saved_resume_refuses_the_batch_before_it_advances_then_finishes() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let arm = build_arm(&mut host, 1);
        let stale = crate::heap::with_heap(&mut host, |heap| {
            let stale = heap.alloc()?;
            heap.rollback(stale)?;
            Ok(stale)
        })
        .expect("stale");
        save_reset_tail(&mut host);
        let before = meter(&mut host);
        let phase = saved_phase(&mut host);
        assert_eq!(phase, "reset");
        let events = Cell::new(0u32);

        let paused = resume_private_cycle(&mut host, 0, &[Rooted::Table(stale)], |_| {
            events.set(events.get() + 1);
        })
        .expect("zero budget with a saved mark pauses");
        assert!(matches!(paused, SavedMarkResume::Paused));
        assert_eq!(
            meter(&mut host),
            before,
            "zero budget does not read the batch"
        );
        assert_eq!(saved_phase(&mut host), phase);

        let stale_error = resume_private_cycle(&mut host, 8, &[Rooted::Table(stale)], |_| {
            events.set(events.get() + 1);
        })
        .expect_err("stale handle");
        assert!(matches!(stale_error, HeapError::StaleHandle));
        assert_eq!(
            meter(&mut host),
            before,
            "a refused batch does not move the mark"
        );
        assert_eq!(saved_phase(&mut host), phase);

        let over = resume_private_cycle(
            &mut host,
            8,
            &[
                Rooted::Table(arm.anchor),
                Rooted::Table(arm.target),
                Rooted::Table(arm.weak),
            ],
            |_| {
                events.set(events.get() + 1);
            },
        )
        .expect_err("batch plus fixed slots exceeds the root quota");
        assert!(matches!(over, HeapError::RootCapacity));
        assert_eq!(meter(&mut host), before);
        assert_eq!(saved_phase(&mut host), phase);
        assert_eq!(events.get(), 0);
        assert_eq!(crate::heap::stats(&mut host).expect("stats").collections, 0);

        let done = finish_arm(&mut host, &arm, &[Rooted::Table(arm.anchor)]);
        assert_eq!(done.meter.mark_passes, 1, "the same saved pass finished");
        assert_eq!(done.meter.parked_root_batch_accepts, 1);
        assert!(!done.target_alive, "K was not in the finishing batch");
    });
}

#[test]
fn saved_resume_exhausted_before_roots_keeps_the_next_batch() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        crate::heap::install(
            &mut host,
            HeapLimits {
                nodes: 8,
                roots: 1,
                edges: 4,
                max_generation: 8,
            },
        )
        .expect("limits");
        let (anchor, anchor_raw) = publish_node(&mut host);
        let (anchor_next, anchor_next_raw) = publish_node(&mut host);
        let (other, other_raw) = publish_node(&mut host);
        let (other_next, other_next_raw) = publish_node(&mut host);
        crate::heap::with_heap(&mut host, |heap| {
            heap.set_named_edge(anchor, "s", anchor_next)?;
            heap.set_named_edge(other, "s", other_next)?;
            assert_eq!(heap.stats().root_slots, 0);
            Ok(())
        })
        .expect("edges");
        let resources = host.resource_count();
        save_at_start(&mut host);

        let first = resume_private_cycle(&mut host, 1, &[Rooted::Table(anchor)], |_| ())
            .expect("budget ends before roots");
        assert!(matches!(first, SavedMarkResume::Paused));
        let paused = meter(&mut host);
        assert_eq!(
            paused.mark_passes, 1,
            "the saved pass started and was not replaced"
        );
        assert_eq!(paused.parked_root_batch_accepts, 0, "A was not submitted");
        assert_eq!(paused.token_closes, 0);
        assert_eq!(paused.sweep_edge_checks, 0);
        assert_eq!(host.resource_count(), resources);
        crate::heap::with_heap(&mut host, |heap| {
            let parked = ParkedMark::resume(heap).expect("still saved");
            assert!(!parked.mutation_open(), "budget ended before roots");
            assert_ne!(parked.phase_name(), "roots");
            assert_ne!(parked.phase_name(), "gray");
            assert_ne!(parked.phase_name(), "done");
            assert!(!parked.marked(anchor), "A was not marked");
            assert_eq!(parked.meter().weak_mode_slot_checks, 1);
            parked.park().expect("park");
            Ok(())
        })
        .expect("cursor");

        let events = Cell::new(0u32);
        let outcome = resume_private_cycle(&mut host, BUDGET, &[Rooted::Table(other)], |view| {
            events.set(events.get() + 1);
            assert!(!view.release_started());
            assert!(!view.finished());
        })
        .expect("next batch");
        let SavedMarkResume::Completed(report, seen) = outcome else {
            panic!("the later batch should finish the same mark");
        };
        assert!(seen.is_some(), "A was released, so the sweep published");
        assert_eq!(events.get(), 1);
        let done = meter(&mut host);
        assert_eq!(
            done.mark_passes, 1,
            "the later call did not start another mark"
        );
        assert_eq!(done.parked_root_batch_accepts, 1, "only B was accepted");
        assert_eq!(done.token_closes, 2, "A and its successor close");
        assert!(alive(&mut host, other) && alive(&mut host, other_next));
        assert!(token_open(&host, other_raw) && token_open(&host, other_next_raw));
        assert!(!alive(&mut host, anchor) && !alive(&mut host, anchor_next));
        assert!(!token_open(&host, anchor_raw) && !token_open(&host, anchor_next_raw));
        assert_eq!(report.live_nodes, 2);
        assert_eq!(crate::heap::stats(&mut host).expect("stats").collections, 1);
        assert_eq!(crate::heap::stats(&mut host).expect("stats").root_slots, 0);
    });
}

#[test]
fn saved_resume_close_failure_keeps_the_prefix_and_retries_from_roots() {
    crate::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let unrelated = host
            .push_resource(crate::UnrelatedHostResource)
            .expect("unrelated");
        let unrelated_raw = unrelated.handle().raw();
        crate::heap::install(
            &mut host,
            HeapLimits {
                nodes: 16,
                roots: 4,
                edges: 4,
                max_generation: 16,
            },
        )
        .expect("limits");
        let (keeper, keeper_raw) = publish_node(&mut host);
        crate::root_table(&mut host, keeper);
        let (prefix, prefix_raw) = publish_node(&mut host);
        let foreign = crate::heap::with_heap(&mut host, |heap| {
            let node = heap.alloc()?;
            heap.bind_token(node, unrelated_raw)?;
            Ok(node)
        })
        .expect("foreign token");
        let (suffix, suffix_raw) = publish_node(&mut host);
        let meta = alloc_node(&mut host);
        crate::root_table(&mut host, meta);
        let watch = alloc_node(&mut host);
        crate::root_table(&mut host, watch);
        let (weak, weak_raw) = publish_node(&mut host);
        crate::heap::with_heap(&mut host, |heap| {
            heap.set_mode_scalar(meta, Value::string("v"))?;
            heap.set_metatable(watch, Some(meta))?;
            heap.set_named_edge(watch, "gone", weak)?;
            Ok(())
        })
        .expect("weak edge");
        assert!(prefix.slot() < foreign.slot() && foreign.slot() < suffix.slot());
        let roots = crate::heap::stats(&mut host).expect("roots").root_slots;
        assert_eq!(roots, 3);
        let resources = host.resource_count();
        save_reset_tail(&mut host);
        let events = Cell::new(0u32);

        let error = resume_private_cycle(&mut host, BUDGET, &[], |_| {
            events.set(events.get() + 1);
        })
        .expect_err("foreign close");
        crate::assert_type_mismatch(&error);
        let failed = crate::heap::take_scan_meter(&mut host).expect("failed meter");
        assert_eq!(events.get(), 0, "close failure does not publish");
        assert_eq!(failed.mark_passes, 1, "the saved pass was not replaced");
        assert_eq!(failed.token_closes, 2, "prefix closed, foreign attempted");
        assert_eq!(failed.sweep_edge_checks, 0);
        assert_eq!(failed.scratch.dead_slots_pushed, 0);
        assert_eq!(failed.scratch.sweep_bitmap_inited, 0);
        assert_eq!(
            crate::heap::stats(&mut host).expect("stats").collections,
            0,
            "an incomplete resume is not a collection"
        );
        assert_eq!(
            crate::heap::stats(&mut host).expect("stats").root_slots,
            roots
        );
        assert!(host.resource(&unrelated).is_ok(), "foreign resource stays");
        assert!(
            !token_open(&host, prefix_raw),
            "the closed prefix stays closed"
        );
        assert!(token_open(&host, suffix_raw), "the suffix stays open");
        assert!(token_open(&host, weak_raw), "the weak token is not closed");
        assert!(token_open(&host, keeper_raw));
        assert_eq!(host.resource_count(), resources - 1);
        assert!(alive(&mut host, prefix));
        assert!(alive(&mut host, foreign));
        assert!(alive(&mut host, suffix));
        assert!(alive(&mut host, weak));
        assert_eq!(
            crate::heap::with_heap(&mut host, |heap| heap.named_edge(watch, "gone")).expect("edge"),
            Some(weak),
            "the weak edge is unchanged"
        );
        assert!(
            !crate::heap::with_heap(&mut host, |heap| Ok(heap.has_saved_mark())).expect("saved"),
            "the finished snapshot was dropped"
        );

        let again = resume_private_cycle(&mut host, BUDGET, &[], |_| {
            events.set(events.get() + 1);
        })
        .expect_err("a later continuation has no saved mark");
        assert!(matches!(again, HeapError::MarkDenied));
        assert_eq!(events.get(), 0);
        assert_eq!(host.resource_count(), resources - 1);
        assert!(token_open(&host, suffix_raw));
        assert_eq!(
            crate::heap::with_heap(&mut host, |heap| heap.named_edge(watch, "gone")).expect("edge"),
            Some(weak)
        );

        let retried = crate::heap::collect_observing(&mut host, |_| {
            events.set(events.get() + 1);
        })
        .expect_err("the same obstacle fails a fresh mark");
        crate::assert_type_mismatch(&retried);
        let retried_meter = crate::heap::take_scan_meter(&mut host).expect("retry meter");
        assert_eq!(events.get(), 0);
        assert_eq!(
            retried_meter.mark_passes, 1,
            "the retry marks from the current roots"
        );
        assert_eq!(retried_meter.sweep_edge_checks, 0);
        assert_eq!(
            retried_meter.token_closes, 1,
            "only the foreign token is attempted"
        );
        assert_eq!(crate::heap::stats(&mut host).expect("stats").collections, 0);
        assert!(token_open(&host, suffix_raw));
        assert!(token_open(&host, weak_raw));
        assert_eq!(
            crate::heap::with_heap(&mut host, |heap| heap.named_edge(watch, "gone")).expect("edge"),
            Some(weak)
        );

        crate::heap::with_heap(&mut host, |heap| heap.unbind_token(foreign))
            .expect("fixture drops the foreign binding");
        let (report, seen) = crate::heap::collect_observing(&mut host, |view| {
            events.set(events.get() + 1);
            assert!(view.still_physical(weak));
            assert!(!view.release_started());
            assert!(!view.finished());
            view.named(watch, "gone").expect("gone").is_none()
        })
        .expect("collect finishes after the fixture removes the obstacle");
        assert_eq!(events.get(), 1, "one published observation");
        assert_eq!(seen, Some(true), "the weak edge is cleared at publish");
        assert_eq!(report.live_nodes, 3, "keeper, metatable, and watch stay");
        assert!(!alive(&mut host, weak));
        assert!(!alive(&mut host, suffix));
        assert!(!alive(&mut host, prefix));
        assert!(!alive(&mut host, foreign));
        assert!(alive(&mut host, keeper));
        assert!(alive(&mut host, watch));
        assert!(alive(&mut host, meta));
        assert!(!token_open(&host, weak_raw));
        assert!(!token_open(&host, suffix_raw));
        assert!(!token_open(&host, prefix_raw));
        assert!(token_open(&host, keeper_raw));
        assert!(host.resource(&unrelated).is_ok());
        assert_eq!(host.resource_count(), resources - 3);
        assert_eq!(crate::heap::stats(&mut host).expect("stats").collections, 1);
        let recovered = crate::heap::take_scan_meter(&mut host).expect("recovered meter");
        assert_eq!(recovered.mark_passes, 1);
        assert!(recovered.sweep_edge_checks > 0, "the recovery sweep ran");
    });
}
