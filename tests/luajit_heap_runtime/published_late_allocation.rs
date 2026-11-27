//! One private table allocated after a published sweep tail is saved.
//!
//! Not a guest step and not a second allocator. The late node is unmarked,
//! gets one real `resource<lua.table>` token, and is absent from the tail's
//! fixed dead suffix. The next host collection closes that token.

use vm::Value;

use super::SlotState;
use crate::heap::{
    self, HeapError, HeapLimits, LuaHeap, PublishedBoundary, PublishedResume, ScanMeter, TableRef,
};

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

fn handle_open(host: &mut vm::HostContext<'_>, handle: vm::ResourceHandle) -> bool {
    host.typed_resource_with_key::<heap::LuaTableResource>(handle, &heap::LuaTableResource::key())
        .is_ok()
}

fn raw_token(handle: vm::ResourceHandle) -> i64 {
    i64::try_from(handle.raw()).expect("resource handle fits a positive integer")
}

struct WeakPair {
    owner: TableRef,
    live: TableRef,
    dead_a: TableRef,
    dead_b: TableRef,
    dead_a_token: i64,
    dead_b_token: i64,
    dead_a_generation: u32,
    dead_b_generation: u32,
}

fn install_pair(host: &mut vm::HostContext<'_>, limits: HeapLimits) -> WeakPair {
    heap::install(host, limits).expect("install");
    let meta = heap::with_heap(host, LuaHeap::alloc).expect("meta");
    let (owner, _) = publish(host);
    let (live, _) = publish(host);
    let (dead_a, dead_a_token) = publish(host);
    let (dead_b, dead_b_token) = publish(host);
    let (dead_a_generation, dead_b_generation) = heap::with_heap(host, |heap| {
        heap.set_mode_scalar(meta, Value::string("v"))?;
        heap.set_metatable(owner, Some(meta))?;
        heap.set_named_edge(owner, "stay", live)?;
        heap.set_named_edge(owner, "a", dead_a)?;
        heap.set_named_edge(owner, "b", dead_b)?;
        let frame = heap.enter_roots(3)?;
        heap.write_root(frame, 0, meta)?;
        heap.write_root(frame, 1, owner)?;
        heap.write_root(frame, 2, live)?;
        Ok((
            heap.slots[dead_a.slot() as usize].generation,
            heap.slots[dead_b.slot() as usize].generation,
        ))
    })
    .expect("graph");
    WeakPair {
        owner,
        live,
        dead_a,
        dead_b,
        dead_a_token,
        dead_b_token,
        dead_a_generation,
        dead_b_generation,
    }
}

fn publish_boundary(host: &mut vm::HostContext<'_>) -> WeakPair {
    let graph = install_pair(host, limits());
    let published = heap::collect_until_published(host).expect("publish");
    assert!(
        matches!(published, PublishedBoundary::Published { .. }),
        "publish must save a tail, got {published:?}"
    );
    assert!(
        !token_open(host, graph.dead_a_token),
        "first dead token closed"
    );
    assert!(
        !token_open(host, graph.dead_b_token),
        "second dead token closed"
    );
    heap::with_heap(host, |heap| {
        assert!(heap.alive(graph.dead_a), "first dead slot still physical");
        assert!(heap.alive(graph.dead_b), "second dead slot still physical");
        assert_eq!(
            heap.slots[graph.dead_a.slot() as usize].generation,
            graph.dead_a_generation
        );
        assert_eq!(
            heap.slots[graph.dead_b.slot() as usize].generation,
            graph.dead_b_generation
        );
        assert!(heap.has_sweep_tail());
        Ok(())
    })
    .expect("published graph");
    graph
}

fn tail_cursor(heap: &LuaHeap) -> String {
    let text = format!("{:?}", heap.sweep_tail);
    let marker = "late_nodes: ";
    let Some(start) = text.find(marker) else {
        return text;
    };
    let rest = &text[start + marker.len()..];
    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    let mut cursor = String::new();
    cursor.push_str(&text[..start]);
    cursor.push_str("late_nodes: _");
    cursor.push_str(&rest[digits..]);
    cursor
}

fn drive_tail(
    host: &mut vm::HostContext<'_>,
    mut after_first: impl FnMut(&mut vm::HostContext<'_>),
) -> (heap::CollectReport, ScanMeter) {
    let _ = heap::take_scan_meter(host).expect("clear tail meter");
    assert!(
        matches!(
            heap::resume_published_sweep(host, 0).expect("zero budget"),
            PublishedResume::Paused
        ),
        "zero budget does not finish the tail"
    );
    let mut saw_pause = false;
    let mut report = None;
    for _ in 0..16 {
        match heap::resume_published_sweep(host, 1).expect("one unit") {
            PublishedResume::Paused => {
                if !saw_pause {
                    saw_pause = true;
                    after_first(host);
                }
            }
            PublishedResume::Completed(done) => {
                assert!(report.is_none(), "the tail records one report");
                report = Some(done);
                break;
            }
        }
    }
    assert!(saw_pause, "the first release must pause before the report");
    let report = report.expect("the tail finishes once");
    let again = heap::resume_published_sweep(host, 1);
    assert!(
        matches!(again, Err(HeapError::SweepDenied)),
        "a finished tail is not resumed again, got {again:?}"
    );
    let meter = heap::take_scan_meter(host).expect("tail meter");
    (report, meter)
}

fn refuse_ordinary_publish(host: &mut vm::HostContext<'_>) {
    let before = heap::stats(host).expect("stats");
    let resources = host.resource_count();
    let refused = heap::publish(host);
    assert_eq!(refused, Err(HeapError::SweepPending));
    let after = heap::stats(host).expect("stats after ordinary publish");
    assert_eq!(after.live_nodes, before.live_nodes);
    assert_eq!(after.node_slots, before.node_slots);
    assert_eq!(after.peak_nodes, before.peak_nodes);
    assert_eq!(host.resource_count(), resources);
}

#[test]
fn published_late_allocation_keeps_an_unmarked_token_until_the_next_collect() {
    let (late_report, late_meter, late_token, late_slot) = crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = publish_boundary(&mut host);
        refuse_ordinary_publish(&mut host);
        let before = heap::stats(&mut host).expect("stats");
        let _ = heap::take_scan_meter(&mut host).expect("clear publish meter");
        let (late, handle) = heap::publish_late(&mut host).expect("late table");
        let leaked = heap::take_scan_meter(&mut host).expect("late alloc meter");
        assert_eq!(leaked, ScanMeter::default(), "late alloc is not a scan");
        assert_eq!(
            late.slot(),
            before.node_slots,
            "growth, not a dead suffix slot"
        );
        assert_ne!(late.slot(), graph.dead_a.slot());
        assert_ne!(late.slot(), graph.dead_b.slot());
        heap::with_heap(&mut host, |heap| {
            assert!(matches!(
                heap.slots[late.slot() as usize].state,
                SlotState::Live { marked: false }
            ));
            assert_eq!(heap.slots[late.slot() as usize].token, Some(handle.raw()));
            assert!(heap.alive(graph.dead_a));
            assert!(heap.alive(graph.dead_b));
            Ok(())
        })
        .expect("late slot");
        assert!(handle_open(&mut host, handle));
        let resources = host.resource_count();
        let (report, meter) = drive_tail(&mut host, |host| {
            heap::with_heap(host, |heap| {
                assert!(!heap.alive(graph.dead_a), "first dead released");
                assert!(heap.alive(graph.dead_b), "second dead still pending");
                assert!(heap.alive(late), "suffix does not visit the late node");
                assert_eq!(heap.slots[late.slot() as usize].token, Some(handle.raw()));
                Ok(())
            })
            .expect("first release");
            assert!(handle_open(host, handle));
        });
        assert_eq!(report.marked_nodes, 3);
        assert_eq!(report.live_nodes, report.marked_nodes + 1);
        assert_eq!(report.live_roots, 3);
        let stats = heap::stats(&mut host).expect("finished stats");
        assert_eq!(stats.live_nodes, report.live_nodes);
        assert_eq!(stats.last_marked, report.marked_nodes);
        assert_eq!(stats.collections, 1);
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(late))).expect("alive"));
        assert_eq!(host.resource_count(), resources);
        let again = heap::collect(&mut host).expect("next collect");
        assert!(
            !handle_open(&mut host, handle),
            "next collect closes the late token"
        );
        assert!(!token_open(&mut host, raw_token(handle)));
        assert!(!heap::with_heap(&mut host, |heap| Ok(heap.alive(late))).expect("swept"));
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(graph.owner))).expect("owner"));
        assert_eq!(again.live_nodes, 3);
        assert_eq!(again.marked_nodes, 3);
        (report, meter, raw_token(handle), late.slot())
    });
    let _ = (late_token, late_slot);

    let (control_report, control_meter) = crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = publish_boundary(&mut host);
        drive_tail(&mut host, |host| {
            heap::with_heap(host, |heap| {
                assert!(!heap.alive(graph.dead_a));
                assert!(heap.alive(graph.dead_b));
                Ok(())
            })
            .expect("control first release");
        })
    });
    assert_eq!(
        late_meter, control_meter,
        "Release and Roots metering is unchanged"
    );
    assert_eq!(late_report.marked_nodes, control_report.marked_nodes);
    assert_eq!(late_report.live_roots, control_report.live_roots);
    assert_eq!(late_report.live_nodes, control_report.live_nodes + 1);
}

#[test]
fn published_late_allocation_reuses_only_a_released_slot() {
    let (same_size_meter, same_size_slots) = crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let meta = heap::with_heap(&mut host, LuaHeap::alloc).expect("meta");
        let (owner, _) = publish(&mut host);
        let (live, _) = publish(&mut host);
        let (dead_a, dead_a_token) = publish(&mut host);
        let (dead_b, dead_b_token) = publish(&mut host);
        let spare = heap::with_heap(&mut host, LuaHeap::alloc).expect("spare");
        heap::with_heap(&mut host, |heap| heap.rollback(spare)).expect("prior free slot");
        let spare_generation = heap::with_heap(&mut host, |heap| {
            Ok(heap.slots[spare.slot() as usize].generation)
        })
        .expect("spare generation");
        heap::with_heap(&mut host, |heap| {
            heap.set_mode_scalar(meta, Value::string("v"))?;
            heap.set_metatable(owner, Some(meta))?;
            heap.set_named_edge(owner, "stay", live)?;
            heap.set_named_edge(owner, "a", dead_a)?;
            heap.set_named_edge(owner, "b", dead_b)?;
            let frame = heap.enter_roots(3)?;
            heap.write_root(frame, 0, meta)?;
            heap.write_root(frame, 1, owner)?;
            heap.write_root(frame, 2, live)?;
            Ok(())
        })
        .expect("graph");
        let published = heap::collect_until_published(&mut host).expect("publish");
        assert!(matches!(published, PublishedBoundary::Published { .. }));
        assert!(!token_open(&mut host, dead_a_token));
        assert!(!token_open(&mut host, dead_b_token));
        let before_slots = heap::stats(&mut host).expect("stats").node_slots;
        let cursor = heap::with_heap(&mut host, |heap| Ok(tail_cursor(heap))).expect("cursor");
        let (late, handle) = heap::publish_late(&mut host).expect("reuse prior free slot");
        assert_eq!(late.slot(), spare.slot());
        assert_eq!(late.generation(), spare_generation);
        assert_ne!(late.slot(), dead_a.slot());
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(dead_a))).expect("pending"));
        let after_cursor =
            heap::with_heap(&mut host, |heap| Ok(tail_cursor(heap))).expect("cursor");
        assert_eq!(after_cursor, cursor, "late alloc does not walk the suffix");
        assert_eq!(
            heap::stats(&mut host).expect("stats").node_slots,
            before_slots
        );
        assert!(handle_open(&mut host, handle));
        let (report, meter) = drive_tail(&mut host, |_| {});
        assert_eq!(report.live_nodes, report.marked_nodes + 1);
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(late))).expect("survives tail"));
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(owner))).expect("owner"));
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(live))).expect("live"));
        assert!(!heap::with_heap(&mut host, |heap| Ok(heap.alive(dead_b))).expect("released"));
        (meter, before_slots)
    });

    let (control_meter, control_slots) = crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let meta = heap::with_heap(&mut host, LuaHeap::alloc).expect("meta");
        let (owner, _) = publish(&mut host);
        let (live, _) = publish(&mut host);
        let (dead_a, _) = publish(&mut host);
        let (dead_b, _) = publish(&mut host);
        let spare = heap::with_heap(&mut host, LuaHeap::alloc).expect("spare");
        heap::with_heap(&mut host, |heap| heap.rollback(spare)).expect("same free slot");
        heap::with_heap(&mut host, |heap| {
            heap.set_mode_scalar(meta, Value::string("v"))?;
            heap.set_metatable(owner, Some(meta))?;
            heap.set_named_edge(owner, "stay", live)?;
            heap.set_named_edge(owner, "a", dead_a)?;
            heap.set_named_edge(owner, "b", dead_b)?;
            let frame = heap.enter_roots(3)?;
            heap.write_root(frame, 0, meta)?;
            heap.write_root(frame, 1, owner)?;
            heap.write_root(frame, 2, live)?;
            Ok(())
        })
        .expect("control graph");
        let published = heap::collect_until_published(&mut host).expect("publish");
        assert!(matches!(published, PublishedBoundary::Published { .. }));
        let slots = heap::stats(&mut host).expect("stats").node_slots;
        let (_report, meter) = drive_tail(&mut host, |_| {});
        (meter, slots)
    });
    assert_eq!(same_size_slots, control_slots, "same slot vector");
    assert_eq!(
        same_size_meter, control_meter,
        "same-size tail metering is unchanged"
    );

    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = publish_boundary(&mut host);
        let first = heap::resume_published_sweep(&mut host, 1).expect("release first dead");
        assert!(matches!(first, PublishedResume::Paused));
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.alive(graph.dead_a))).expect("released")
        );
        let released_generation = graph.dead_a.generation();
        let (late, handle) = heap::publish_late(&mut host).expect("reuse released index");
        assert_eq!(late.slot(), graph.dead_a.slot());
        assert_ne!(late.generation(), released_generation);
        assert_ne!(late, graph.dead_a, "old handle is stale");
        assert!(heap::with_heap(&mut host, |heap| Ok(!heap.alive(graph.dead_a))).expect("stale"));
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(graph.dead_b))).expect("pending"));
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(late))).expect("reused live"));
        let cursor = heap::with_heap(&mut host, |heap| Ok(tail_cursor(heap))).expect("cursor");
        let root_refused = heap::with_heap(&mut host, |heap| heap.enter_roots(1));
        assert_eq!(root_refused, Err(HeapError::SweepPending));
        let write_refused = heap::with_heap(&mut host, |heap| {
            heap.set_named_edge(graph.live, "extra", graph.owner)
        });
        assert_eq!(write_refused, Err(HeapError::SweepPending));
        let after = heap::with_heap(&mut host, |heap| Ok(tail_cursor(heap))).expect("cursor");
        assert_eq!(after, cursor);
        let (report, _) = drive_tail(&mut host, |_| {});
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.alive(late))).expect("suffix skipped reuse")
        );
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.alive(graph.dead_b))).expect("other dead")
        );
        assert_eq!(report.live_nodes, report.marked_nodes + 1);
        assert!(handle_open(&mut host, handle));
    });
}

struct QuotaFill;

impl vm::resource::HostResource for QuotaFill {}

#[test]
fn published_late_allocation_rolls_back_capacity_failures() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let tight = HeapLimits {
            nodes: 5,
            roots: 4,
            edges: 8,
            max_generation: 8,
        };
        let _graph = install_pair(&mut host, tight);
        let published = heap::collect_until_published(&mut host).expect("publish");
        assert!(matches!(published, PublishedBoundary::Published { .. }));
        let before = heap::stats(&mut host).expect("stats");
        let resources = host.resource_count();
        let refused = heap::publish_late(&mut host);
        assert_eq!(refused, Err(HeapError::NodeCapacity));
        let after = heap::stats(&mut host).expect("stats");
        assert_eq!(after.live_nodes, before.live_nodes);
        assert_eq!(after.node_slots, before.node_slots);
        assert_eq!(after.peak_nodes, before.peak_nodes);
        assert_eq!(host.resource_count(), resources);
        let (report, _) = drive_tail(&mut host, |_| {});
        assert_eq!(report.live_nodes, report.marked_nodes);
        assert_eq!(
            heap::stats(&mut host).expect("done").live_nodes,
            report.live_nodes
        );
    });

    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = publish_boundary(&mut host);
        while host.resource_count() < vm::resource::handle::DEFAULT_MAX_RESOURCES {
            host.push_resource(QuotaFill)
                .expect("fill the real resource table");
        }
        let before = heap::stats(&mut host).expect("stats");
        let resources = host.resource_count();
        let cursor = heap::with_heap(&mut host, |heap| Ok(tail_cursor(heap))).expect("cursor");
        let refused = heap::publish_late(&mut host);
        let after = heap::stats(&mut host).expect("stats after");
        let error = refused.expect_err("a full resource table refuses the late token");
        assert_eq!(after.node_slots, before.node_slots + 1, "high water stays");
        assert_eq!(after.peak_nodes, before.peak_nodes + 1);
        assert_eq!(
            after.live_nodes, before.live_nodes,
            "the empty reservation is released"
        );
        assert_eq!(host.resource_count(), resources, "no new open token");
        match &error {
            HeapError::Scope(message) if message.contains("resource_limit_exceeded") => {}
            other => panic!("refusal must be the real push_resource limit, got {other:?}"),
        }
        heap::with_heap(&mut host, |heap| {
            let index = before.node_slots;
            assert!(matches!(heap.slots[index as usize].state, SlotState::Free));
            assert_eq!(heap.slots[index as usize].generation, 2);
            assert_eq!(heap.slots[index as usize].token, None);
            assert_eq!(heap.free_list.last().copied(), Some(index));
            assert_eq!(tail_cursor(heap), cursor);
            assert!(heap.alive(graph.dead_a));
            Ok(())
        })
        .expect("rolled back slot");
        let (report, _) = drive_tail(&mut host, |_| {});
        assert_eq!(report.live_nodes, report.marked_nodes);
        assert_eq!(report.marked_nodes, 3);
        let done = heap::stats(&mut host).expect("done");
        assert_eq!(done.live_nodes, report.live_nodes);
        assert_eq!(done.last_marked, report.marked_nodes);
        assert_eq!(done.node_slots, before.node_slots + 1);
        assert_eq!(done.peak_nodes, before.peak_nodes + 1);
    });
}

#[test]
fn published_late_allocation_does_not_install_without_a_tail() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let refused = heap::publish_late(&mut host);
        assert_eq!(refused, Err(HeapError::SweepDenied));
        assert!(host.scope_state::<heap::LuaHeap>().is_none());
        assert_eq!(host.resource_count(), 0);
    });
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let table = heap::with_heap(&mut host, LuaHeap::alloc).expect("table");
        let before = heap::stats(&mut host).expect("stats");
        let refused = heap::publish_late(&mut host);
        assert_eq!(refused, Err(HeapError::SweepDenied));
        let after = heap::stats(&mut host).expect("stats");
        assert_eq!(after, before);
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(table))).expect("alive"));
        assert!(!heap::with_heap(&mut host, |heap| Ok(heap.has_sweep_tail())).expect("no tail"));
        assert_eq!(host.resource_count(), 0);
    });
}

#[test]
fn published_late_allocation_rejects_delete_without_moving_the_tail() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = publish_boundary(&mut host);
        let (late, handle) = heap::publish_late(&mut host).expect("late table");
        let cursor = heap::with_heap(&mut host, |heap| Ok(tail_cursor(heap))).expect("cursor");
        let rejected = heap::delete_published_named(&mut host, late, "entry");
        assert_eq!(rejected, Err(HeapError::SweepPending));
        let removed = heap::delete_published_named(&mut host, graph.owner, "stay");
        assert_eq!(removed, Ok(true), "a marked ordinary field still deletes");
        let missing = heap::delete_published_named(&mut host, graph.owner, "stay");
        assert_eq!(missing, Ok(false));
        heap::with_heap(&mut host, |heap| {
            assert_eq!(tail_cursor(heap), cursor);
            assert!(heap.alive(late));
            assert!(matches!(
                heap.slots[late.slot() as usize].state,
                SlotState::Live { marked: false }
            ));
            assert_eq!(heap.slots[late.slot() as usize].token, Some(handle.raw()));
            assert_eq!(heap.named_edge(graph.owner, "stay")?, None);
            Ok(())
        })
        .expect("delete left the tail");
        assert!(handle_open(&mut host, handle));
        let (report, _) = drive_tail(&mut host, |_| {});
        assert_eq!(report.live_nodes, report.marked_nodes + 1);
        assert!(handle_open(&mut host, handle));
    });
}

#[test]
fn published_late_allocation_drain_leaves_the_late_node() {
    let (late_meter, late_marked, late_live) = crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = publish_boundary(&mut host);
        let _ = heap::take_scan_meter(&mut host).expect("clear publish meter");
        let (late, handle) = heap::publish_late(&mut host).expect("late table");
        let leaked = heap::take_scan_meter(&mut host).expect("late meter");
        assert_eq!(leaked, ScanMeter::default());
        heap::with_heap(&mut host, |heap| {
            heap.drain_sweep_tail();
            assert!(!heap.has_sweep_tail());
            assert!(heap.alive(late));
            assert!(!heap.alive(graph.dead_a));
            assert!(!heap.alive(graph.dead_b));
            assert_eq!(heap.slots[late.slot() as usize].token, Some(handle.raw()));
            Ok(())
        })
        .expect("drain");
        assert!(handle_open(&mut host, handle));
        let stats = heap::stats(&mut host).expect("after drain");
        assert_eq!(stats.collections, 1);
        assert_eq!(stats.last_marked, 3);
        assert_eq!(stats.live_nodes, stats.last_marked + 1);
        let meter = heap::take_scan_meter(&mut host).expect("drain meter");
        let again = heap::collect(&mut host).expect("next collect");
        assert!(!handle_open(&mut host, handle));
        assert_eq!(again.live_nodes, 3);
        assert!(!heap::with_heap(&mut host, |heap| Ok(heap.alive(late))).expect("swept"));
        (meter, stats.last_marked, stats.live_nodes)
    });
    let _ = (late_marked, late_live);

    let control_meter = crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = publish_boundary(&mut host);
        let _ = heap::take_scan_meter(&mut host).expect("clear");
        heap::with_heap(&mut host, |heap| {
            heap.drain_sweep_tail();
            assert!(!heap.alive(graph.dead_a));
            assert!(!heap.alive(graph.dead_b));
            Ok(())
        })
        .expect("control drain");
        let stats = heap::stats(&mut host).expect("control");
        assert_eq!(stats.collections, 1);
        assert_eq!(stats.last_marked, 3);
        assert_eq!(stats.live_nodes, 3);
        heap::take_scan_meter(&mut host).expect("control meter")
    });
    assert_eq!(
        late_meter, control_meter,
        "drain uses the same Release and Roots steps"
    );
}
