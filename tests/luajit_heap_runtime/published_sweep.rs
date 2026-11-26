//! Published sweep tail.
//!
//! Publish clears weak values and closes their tokens, then saves one owned
//! release tail. Resume spends budget on that tail and must free the still
//! physical dead slot. The ordinary sweep uses the same release and root steps.
//!
//! Commit gates, each checked after handle or capacity validation and before
//! the first write or chain walk. Low-level `unlink_at`, `rewrite_gap`, and
//! `release_slot` stay ungated; sweep calls those directly.
//!
//! - alloc table/closure/upvalue, including a free-list reuse: `alloc_slot`
//! - rollback: after `require_live`
//! - `issue_carrier`: after the existing-carrier check
//! - table, closure, upvalue, and mode writes: `write_from`
//! - table, object, number, closure-key, and upvalue deletes: `unlink_key`
//! - metatable replace: `set_metatable`
//! - `enter_roots`, `store_root` (table, closure, upvalue), `clear_root`, `leave_roots`
//! - `bind_token`, `unbind_token`
//! - `heap::publish`: the alloc gate, so no token is pushed
//! - `heap::collect`, `collect_observing`, `resume_private_cycle`: before mark or close
//! - `ParkedMark::start`, `begin_private_cycle`: before a mark is installed
//!
//! `clear_exact_token` is the close primitive. Host collect is refused before
//! it runs, so a saved tail never reaches it.

use super::{MarkSession, ParkedMark};
use vm::Value;

use crate::heap::{
    self, HeapError, HeapLimits, LuaHeap, PublishedBoundary, PublishedResume, ScanMeter, TableRef,
    UserdataRef,
};

fn limits() -> HeapLimits {
    HeapLimits {
        nodes: 8,
        roots: 4,
        edges: 8,
        max_generation: 8,
    }
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

#[test]
fn published_resume_frees_the_closed_dead_slot() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let meta = heap::with_heap(&mut host, LuaHeap::alloc).expect("meta");
        let (owner, owner_token) = publish(&mut host);
        let (live, live_token) = publish(&mut host);
        let (dead, dead_token) = publish(&mut host);
        let resources_before = host.resource_count();
        let original_generation = dead.generation();
        heap::with_heap(&mut host, |heap| {
            heap.set_mode_scalar(meta, Value::string("v"))?;
            heap.set_metatable(owner, Some(meta))?;
            heap.set_named_edge(owner, "stay", live)?;
            heap.set_named_edge(owner, "gone", dead)?;
            heap.set_number_edge(owner, 1.0, dead)?;
            heap.set_named_edge(dead, "self", dead)?;
            let live_carrier = heap.issue_carrier(live)?;
            let frame = heap.enter_roots(3)?;
            heap.write_root(frame, 0, meta)?;
            heap.write_root(frame, 1, owner)?;
            heap.write_root(frame, 2, live)?;
            assert_eq!(heap.raw_sequence_len(owner)?, 1.0);
            Ok(live_carrier)
        })
        .expect("graph");
        let live_carrier =
            heap::with_heap(&mut host, |heap| heap.issued_value(live)).expect("carrier");
        let _ = heap::take_scan_meter(&mut host).expect("clear meter");

        let published = heap::collect_until_published(&mut host).expect("publish");
        let PublishedBoundary::Published {
            window_heads,
            window_edges,
        } = published
        else {
            panic!("publish must stop before release, got {published:?}");
        };
        assert_eq!(window_heads, 4, "one owner head per slot");
        assert_eq!(window_edges, 5, "mode, stay, gone, number, self");
        let stats = heap::stats(&mut host).expect("stats");
        assert_eq!(stats.collections, 0, "publish is not a completion report");
        assert_eq!(stats.live_nodes, 4, "dead node still occupies a slot");
        assert_eq!(
            stats.live_edges, 2,
            "weak values became gaps; unmarked self edge dropped"
        );
        assert!(!token_open(&mut host, dead_token), "dead token closed");
        assert!(token_open(&mut host, live_token));
        assert!(token_open(&mut host, owner_token));
        assert_eq!(host.resource_count(), resources_before - 1);
        heap::with_heap(&mut host, |heap| {
            assert!(heap.alive(dead), "dead slot still physical");
            assert_eq!(slot_generation(heap, dead), original_generation);
            assert!(heap.alive(live));
            assert!(heap.alive(owner));
            assert_eq!(heap.named_edge(owner, "gone")?, None, "weak value cleared");
            assert_eq!(heap.named_edge(owner, "stay")?, Some(live));
            assert_eq!(heap.raw_sequence_len(owner)?, 0.0, "rawlen ignores the gap");
            let started = heap.next_guest(owner, &Value::Null)?.expect("stay");
            assert_eq!(started.0, Value::string("stay"));
            assert_eq!(started.1, live_carrier);
            assert_eq!(
                heap.next_guest(owner, &Value::string("gone"))?,
                None,
                "cleared key remains a cursor"
            );
            Ok(())
        })
        .expect("published graph");
        let publish_meter = heap::take_scan_meter(&mut host).expect("publish meter");
        assert_eq!(
            publish_meter.owner_head_checks,
            window_heads + 3,
            "mark reads the three rooted nodes; sweep reads one head per slot"
        );
        assert_eq!(publish_meter.sweep_edge_checks, window_edges);
        assert_eq!(publish_meter.scratch.dead_slots_pushed, 1);
        assert_eq!(publish_meter.scratch.sweep_bitmap_inited, window_heads);
        assert_eq!(publish_meter.token_closes, 1);
        assert_eq!(publish_meter.mark_passes, 1);

        let zero = heap::resume_published_sweep(&mut host, 0).expect("saved tail accepts zero");
        assert!(
            matches!(zero, PublishedResume::Paused),
            "zero budget pauses without scanning, got {zero:?}"
        );
        let meter = heap::take_scan_meter(&mut host).expect("zero meter");
        assert_eq!(meter, ScanMeter::default(), "zero budget does not scan");
        let still = heap::with_heap(&mut host, |heap| Ok(heap.alive(dead))).expect("alive");
        assert!(still, "zero budget does not release");
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 0);

        let one = heap::resume_published_sweep(&mut host, 1);
        let (released, generation) = heap::with_heap(&mut host, |heap| {
            Ok((!heap.alive(dead), slot_generation(heap, dead)))
        })
        .expect("resume facts");
        let completed = matches!(one, Ok(PublishedResume::Completed(_)));
        let collections = heap::stats(&mut host).expect("stats").collections;
        assert!(
            released && generation != original_generation && !completed,
            "resume left the dead node physical and produced no completion report; released={released} completed={completed} generation={generation} original={original_generation} collections={collections} outcome={one:?}"
        );

        let resume_meter = heap::take_scan_meter(&mut host).expect("release meter");
        assert_eq!(resume_meter.node_slot_visits, 1, "one release visit");
        assert_eq!(resume_meter.root_slot_visits, 0);
        assert_eq!(
            resume_meter.owner_head_checks, 0,
            "resume does not repeat Owners"
        );
        assert_eq!(resume_meter.sweep_edge_checks, 0);
        assert_eq!(
            resume_meter.scratch.dead_slots_pushed, 0,
            "no re-enumeration"
        );
        assert_eq!(resume_meter.scratch.sweep_bitmap_inited, 0);
        assert_eq!(resume_meter.mark_passes, 0, "no remark");
        assert_eq!(resume_meter.token_closes, 0);
        assert_eq!(resume_meter.scratch.pending_close_pushed, 0);
        assert_eq!(collections, 0, "one release is not the completion report");
        assert_eq!(host.resource_count(), resources_before - 1);
        assert!(token_open(&mut host, live_token));
        assert!(token_open(&mut host, owner_token));

        let rest = heap::resume_published_sweep(&mut host, 8).expect("finish roots");
        let PublishedResume::Completed(report) = rest else {
            panic!("remaining budget must finish once, got {rest:?}");
        };
        assert_eq!(report.live_nodes, 3);
        assert_eq!(report.marked_nodes, 3);
        assert_eq!(report.live_roots, 3);
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 1);
        let finish_meter = heap::take_scan_meter(&mut host).expect("finish meter");
        assert_eq!(finish_meter.root_slot_visits, 3);
        assert_eq!(finish_meter.node_slot_visits, 3);
        assert_eq!(finish_meter.owner_head_checks, 0);
        assert_eq!(finish_meter.scratch.dead_slots_pushed, 0);
        assert_eq!(finish_meter.mark_passes, 0);
        let again = heap::resume_published_sweep(&mut host, 8);
        assert_eq!(again.unwrap_err(), HeapError::SweepDenied);
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 1);
        assert_eq!(
            heap::take_scan_meter(&mut host).expect("denied meter"),
            ScanMeter::default()
        );
        let reused = heap::with_heap(&mut host, LuaHeap::alloc).expect("reuse");
        assert_eq!(reused.slot(), dead.slot());
        assert_eq!(reused.generation(), original_generation + 1);
    });
}

fn publish(host: &mut vm::HostContext<'_>) -> (TableRef, i64) {
    let raw = crate::publish_raw(host);
    (crate::table_ref(host, raw), raw)
}

fn assert_pending(error: HeapError) {
    assert_eq!(error, HeapError::SweepPending);
}

#[test]
fn published_resume_splits_before_the_uncharged_finish() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let meta = heap::with_heap(&mut host, LuaHeap::alloc).expect("meta");
        let (owner, _) = publish(&mut host);
        let (live, _) = publish(&mut host);
        let (dead, dead_token) = publish(&mut host);
        heap::with_heap(&mut host, |heap| {
            heap.set_mode_scalar(meta, Value::string("v"))?;
            heap.set_metatable(owner, Some(meta))?;
            heap.set_named_edge(owner, "stay", live)?;
            heap.set_named_edge(owner, "gone", dead)?;
            let frame = heap.enter_roots(4)?;
            heap.write_root(frame, 0, meta)?;
            heap.write_root(frame, 1, owner)?;
            heap.write_root(frame, 2, live)?;
            Ok(frame)
        })
        .expect("graph");
        let _ = heap::take_scan_meter(&mut host);
        let published = heap::collect_until_published(&mut host).expect("publish");
        assert!(matches!(published, PublishedBoundary::Published { .. }));
        let _ = heap::take_scan_meter(&mut host);

        let zero = heap::resume_published_sweep(&mut host, 0).expect("zero");
        assert!(matches!(zero, PublishedResume::Paused));
        assert_eq!(
            heap::take_scan_meter(&mut host).expect("zero meter"),
            ScanMeter::default()
        );

        let released = heap::resume_published_sweep(&mut host, 1).expect("release");
        assert!(matches!(released, PublishedResume::Paused));
        let release_meter = heap::take_scan_meter(&mut host).expect("release meter");
        assert_eq!(release_meter.node_slot_visits, 1);
        assert_eq!(release_meter.root_slot_visits, 0);
        assert!(!token_open(&mut host, dead_token));

        let first_roots = heap::resume_published_sweep(&mut host, 2).expect("two roots");
        assert!(matches!(first_roots, PublishedResume::Paused));
        let first_meter = heap::take_scan_meter(&mut host).expect("first roots");
        assert_eq!(first_meter.root_slot_visits, 2);
        assert_eq!(first_meter.node_slot_visits, 2);
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 0);

        let third = heap::resume_published_sweep(&mut host, 1).expect("third root");
        assert!(matches!(third, PublishedResume::Paused));
        assert_eq!(
            heap::take_scan_meter(&mut host)
                .expect("third")
                .root_slot_visits,
            1
        );

        let last = heap::resume_published_sweep(&mut host, 1).expect("last charged root");
        assert!(
            matches!(last, PublishedResume::Paused),
            "the last charged root does not finish, got {last:?}"
        );
        let last_meter = heap::take_scan_meter(&mut host).expect("last meter");
        assert_eq!(last_meter.root_slot_visits, 1);
        assert_eq!(
            last_meter.node_slot_visits, 0,
            "the empty root slot charges without a node"
        );
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 0);

        let finished = heap::resume_published_sweep(&mut host, 1).expect("uncharged finish");
        let PublishedResume::Completed(report) = finished else {
            panic!("the call after the last charge only records the report, got {finished:?}");
        };
        let finish_meter = heap::take_scan_meter(&mut host).expect("finish meter");
        assert_eq!(finish_meter, ScanMeter::default(), "finish does not charge");
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 1);
        assert_eq!(report.marked_nodes, 3);

        let again = heap::resume_published_sweep(&mut host, 1).expect_err("tail is gone");
        assert_eq!(again, HeapError::SweepDenied);
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 1);
        assert_eq!(
            heap::take_scan_meter(&mut host).expect("denied"),
            ScanMeter::default()
        );
    });
}

#[test]
fn published_pause_refuses_commits_and_keeps_validation_first() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let meta = heap::with_heap(&mut host, LuaHeap::alloc).expect("meta");
        let (owner, owner_token) = publish(&mut host);
        let (live, live_token) = publish(&mut host);
        let (dead, _) = publish(&mut host);
        let (stale, closure, cell, bare, frame) = heap::with_heap(&mut host, |heap| {
            heap.set_mode_scalar(meta, Value::string("v"))?;
            heap.set_metatable(owner, Some(meta))?;
            heap.set_named_edge(owner, "stay", live)?;
            heap.set_named_edge(owner, "gone", dead)?;
            heap.set_number_edge(owner, 1.0, dead)?;
            heap.set_object_edge(owner, live, live)?;
            let closure = heap.alloc_closure()?;
            let cell = heap.alloc_upvalue()?;
            heap.write_upvalue(cell, live)?;
            heap.set_closure_key_edge(owner, closure, live)?;
            heap.set_closure_edge(owner, 9, closure)?;
            let stale = heap.alloc()?;
            heap.rollback(stale)?;
            let bare = heap.alloc()?;
            let frame = heap.enter_roots(3)?;
            heap.write_root(frame, 0, meta)?;
            heap.write_root(frame, 1, owner)?;
            heap.write_root(frame, 2, live)?;
            Ok((stale, closure, cell, bare, frame))
        })
        .expect("graph");
        let resources_before = host.resource_count();
        let _ = heap::take_scan_meter(&mut host);
        assert!(matches!(
            heap::collect_until_published(&mut host).expect("publish"),
            PublishedBoundary::Published { .. }
        ));
        let _ = heap::take_scan_meter(&mut host);
        let stats_before = heap::stats(&mut host).expect("stats");
        let generation =
            heap::with_heap(&mut host, |heap| Ok(slot_generation(heap, dead))).expect("gen");

        heap::with_heap(&mut host, |heap| {
            assert_eq!(
                heap.clear_number_edge(owner, f64::NAN),
                Err(HeapError::NanIndex)
            );
            assert_eq!(
                heap.set_named_edge(stale, "x", live),
                Err(HeapError::StaleHandle)
            );
            assert_eq!(heap.bind_token(live, 1), Err(HeapError::StaleHandle));
            assert_eq!(heap.enter_roots(2), Err(HeapError::RootCapacity));
            assert_eq!(heap.enter_roots(0), Err(HeapError::StaleFrame));
            let mut other = LuaHeap::new(limits());
            let foreign = other.alloc()?;
            assert_eq!(
                heap.set_named_edge(foreign, "x", live),
                Err(HeapError::WrongDomain)
            );
            let created = other.alloc()?;
            assert!(other.alive(created), "a heap with no tail still allocates");

            assert_pending(heap.alloc().unwrap_err());
            assert_pending(heap.alloc_closure().unwrap_err());
            assert_pending(heap.alloc_upvalue().unwrap_err());
            assert_pending(heap.rollback(bare).unwrap_err());
            assert_pending(heap.issue_carrier(bare).unwrap_err());
            assert_pending(heap.set_named_edge(dead, "x", live).unwrap_err());
            assert_pending(heap.set_named_edge(owner, "extra", live).unwrap_err());
            assert_pending(
                heap.set_named_scalar(owner, "stay", Value::Null)
                    .unwrap_err(),
            );
            assert_pending(heap.clear_object_edge(owner, live).unwrap_err());
            assert_pending(heap.clear_number_edge(owner, 1.0).unwrap_err());
            assert_pending(heap.clear_closure_key(owner, closure).unwrap_err());
            assert_pending(heap.clear_upvalue(cell).unwrap_err());
            assert_pending(heap.write_upvalue(cell, owner).unwrap_err());
            assert_pending(heap.set_closure_edge(owner, 9, closure).unwrap_err());
            assert_pending(heap.set_mode_scalar(meta, Value::string("k")).unwrap_err());
            assert_pending(heap.set_metatable(owner, None).unwrap_err());
            assert_pending(heap.enter_roots(1).unwrap_err());
            assert_pending(heap.write_root(frame, 0, live).unwrap_err());
            assert_pending(heap.write_closure_root(frame, 0, closure).unwrap_err());
            assert_pending(heap.write_upvalue_root(frame, 0, cell).unwrap_err());
            assert_pending(heap.clear_root(frame, 0).unwrap_err());
            assert_pending(heap.leave_roots(frame).unwrap_err());
            assert_pending(heap.bind_token(bare, 7).unwrap_err());
            assert_pending(heap.unbind_token(live).unwrap_err());
            assert_eq!(heap.named_edge(owner, "stay")?, Some(live));
            assert_eq!(
                heap.current_table(cell)?,
                None,
                "the unrooted cell was swept; the refused write did not restore it"
            );
            assert!(heap.alive(bare));
            assert!(heap.alive(dead));
            assert_eq!(slot_generation(heap, dead), generation);
            assert_eq!(heap.current_frame(), Some(frame));
            Ok(())
        })
        .expect("refused commits");
        assert_eq!(
            heap::publish(&mut host).unwrap_err(),
            HeapError::SweepPending
        );
        assert_eq!(host.resource_count(), resources_before - 1);
        assert!(token_open(&mut host, owner_token));
        assert!(token_open(&mut host, live_token));
        assert_eq!(heap::stats(&mut host).expect("stats"), stats_before);
        assert_eq!(
            heap::take_scan_meter(&mut host).expect("refused meter"),
            ScanMeter::default()
        );
        let still = heap::resume_published_sweep(&mut host, 0).expect("progress unchanged");
        assert!(matches!(still, PublishedResume::Paused));
        let one = heap::resume_published_sweep(&mut host, 1).expect("same dead slot");
        assert!(matches!(one, PublishedResume::Paused));
        let released = heap::with_heap(&mut host, |heap| Ok(!heap.alive(dead))).expect("released");
        assert!(released, "refusals did not consume the release");
    });
}

#[test]
fn node_capacity_wins_while_a_tail_is_saved() {
    let mut heap = LuaHeap::new(HeapLimits {
        nodes: 3,
        roots: 2,
        edges: 4,
        max_generation: 4,
    });
    let meta = heap.alloc().expect("meta");
    let owner = heap.alloc().expect("owner");
    let dead = heap.alloc().expect("dead");
    heap.set_mode_scalar(meta, Value::string("v"))
        .expect("mode");
    heap.set_metatable(owner, Some(meta)).expect("mt");
    heap.set_named_edge(owner, "gone", dead).expect("weak");
    let frame = heap.enter_roots(2).expect("frame");
    heap.write_root(frame, 0, meta).expect("meta");
    heap.write_root(frame, 1, owner).expect("owner");
    let marked = heap.mark_and_doomed_tokens();
    assert!(matches!(
        heap.halt_at_publish(marked.modes),
        super::PublishHalt::Published { .. }
    ));
    assert_eq!(heap.alloc().expect_err("full"), HeapError::NodeCapacity);
    assert_eq!(heap.stats().node_slots, 3);
    assert!(heap.has_sweep_tail());
}

#[test]
fn published_close_failure_saves_no_tail() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let unrelated = host
            .push_resource(crate::UnrelatedHostResource)
            .expect("unrelated");
        let unrelated_raw = unrelated.handle().raw();
        let (keeper, _) = publish(&mut host);
        heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(1)?;
            heap.write_root(frame, 0, keeper)
        })
        .expect("root");
        let (prefix, prefix_token) = publish(&mut host);
        let (_other, other_token) = publish(&mut host);
        let doomed = heap::with_heap(&mut host, |heap| {
            let node = heap.alloc()?;
            heap.bind_token(node, unrelated_raw)?;
            Ok(node)
        })
        .expect("foreign token");
        let (suffix, suffix_token) = publish(&mut host);
        let resources_before = host.resource_count();
        let _ = heap::take_scan_meter(&mut host);

        let error = heap::collect_until_published(&mut host).expect_err("type mismatch");
        crate::assert_type_mismatch(&error);
        let failed = heap::take_scan_meter(&mut host).expect("failed meter");
        assert_eq!(
            failed.sweep_edge_checks, 0,
            "close failure does not publish"
        );
        assert_eq!(failed.scratch.dead_slots_pushed, 0);
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 0);
        assert!(!token_open(&mut host, prefix_token), "prefix close is real");
        assert!(!token_open(&mut host, other_token));
        assert!(token_open(&mut host, suffix_token), "suffix stays open");
        assert_eq!(
            heap::resume_published_sweep(&mut host, 1).expect_err("no tail"),
            HeapError::SweepDenied
        );
        let allocated = heap::with_heap(&mut host, LuaHeap::alloc).expect("no tail, alloc works");
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(allocated))).expect("alive"));
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(prefix))).expect("prefix"));
        assert_eq!(host.resource_count(), resources_before - 2);

        heap::with_heap(&mut host, |heap| heap.unbind_token(doomed)).expect("remove obstacle");
        let retried = heap::collect_until_published(&mut host).expect("retry publishes");
        assert!(
            matches!(retried, PublishedBoundary::Published { .. }),
            "dead slots remain, so retry saves a tail, got {retried:?}"
        );
        assert!(
            !token_open(&mut host, suffix_token),
            "suffix closes on the retry"
        );
        let finished = heap::resume_published_sweep(&mut host, 8).expect("retry finishes");
        assert!(matches!(finished, PublishedResume::Completed(_)));
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 1);
        assert!(!heap::with_heap(&mut host, |heap| Ok(heap.alive(suffix))).expect("suffix"));
        let _ = keeper;
    });
}

#[test]
fn published_with_no_dead_slot_finishes_without_a_tail() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let (keeper, token) = publish(&mut host);
        heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(1)?;
            heap.write_root(frame, 0, keeper)
        })
        .expect("root");
        let finished = heap::collect_until_published(&mut host).expect("no dead slot");
        let PublishedBoundary::Completed(report) = finished else {
            panic!("no dead slot must finish, got {finished:?}");
        };
        assert!(report.live_nodes >= 1);
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 1);
        assert!(token_open(&mut host, token));
        assert_eq!(
            heap::resume_published_sweep(&mut host, 1).expect_err("no tail"),
            HeapError::SweepDenied
        );
    });
}

#[test]
fn saved_mark_still_refuses_a_new_publish() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        heap::with_heap(&mut host, |heap| {
            let parked = ParkedMark::start(heap).expect("start");
            parked.park().expect("park");
            Ok(())
        })
        .expect("park");
        let error = heap::collect_until_published(&mut host).expect_err("mark active");
        assert_eq!(error, HeapError::MarkActive);
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 0);
        heap::with_heap(&mut host, |heap| {
            assert!(ParkedMark::resume(heap).is_some(), "the mark stays");
            assert!(!heap.has_sweep_tail());
            Ok(())
        })
        .expect("mark remains");
        assert_eq!(
            heap::take_scan_meter(&mut host).expect("meter"),
            ScanMeter::default()
        );
    });
}

#[test]
fn host_collect_refuses_while_the_tail_can_still_finish() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let meta = heap::with_heap(&mut host, LuaHeap::alloc).expect("meta");
        let (owner, _) = publish(&mut host);
        let (dead, _) = publish(&mut host);
        heap::with_heap(&mut host, |heap| {
            heap.set_mode_scalar(meta, Value::string("v"))?;
            heap.set_metatable(owner, Some(meta))?;
            heap.set_named_edge(owner, "gone", dead)?;
            let frame = heap.enter_roots(2)?;
            heap.write_root(frame, 0, meta)?;
            heap.write_root(frame, 1, owner)?;
            Ok(())
        })
        .expect("graph");
        let _ = heap::take_scan_meter(&mut host);
        assert!(matches!(
            heap::collect_until_published(&mut host).expect("publish"),
            PublishedBoundary::Published { .. }
        ));
        let _ = heap::take_scan_meter(&mut host);
        let stats = heap::stats(&mut host).expect("stats");

        assert_eq!(
            heap::collect(&mut host).expect_err("full"),
            HeapError::SweepPending
        );
        assert_eq!(
            heap::collect_observing(&mut host, |_| ()).expect_err("private"),
            HeapError::SweepPending
        );
        assert_eq!(
            crate::heap::resume_private_cycle(&mut host, 1, &[], |_| ()).expect_err("resume"),
            HeapError::SweepPending
        );
        heap::with_heap(&mut host, |heap| {
            assert_eq!(ParkedMark::start(heap).err(), Some(HeapError::SweepPending));
            assert_eq!(
                heap.begin_private_cycle().err(),
                Some(HeapError::SweepPending)
            );
            assert!(!heap.has_saved_mark(), "a refused start installs no mark");
            Ok(())
        })
        .expect("starts refused");
        assert_eq!(heap::stats(&mut host).expect("stats"), stats);
        assert_eq!(
            heap::take_scan_meter(&mut host).expect("meter"),
            ScanMeter::default()
        );

        let finished = heap::resume_published_sweep(&mut host, 8).expect("tail still finishes");
        assert!(matches!(finished, PublishedResume::Completed(_)));
        assert!(!heap::with_heap(&mut host, |heap| Ok(heap.alive(dead))).expect("dead"));
    });
}

#[test]
fn stateless_publish_and_zero_resume_do_not_install_a_heap() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        assert!(host.scope_state::<LuaHeap>().is_none());
        assert_eq!(
            heap::collect_until_published(&mut host).expect_err("no heap"),
            HeapError::SweepDenied
        );
        assert_eq!(
            heap::resume_published_sweep(&mut host, 0).expect_err("zero"),
            HeapError::SweepDenied
        );
        assert_eq!(
            heap::resume_published_sweep(&mut host, 4).expect_err("positive"),
            HeapError::SweepDenied
        );
        assert!(
            host.scope_state::<LuaHeap>().is_none(),
            "no heap was installed"
        );
    });
}

#[test]
fn pure_heap_collect_drains_the_tail_then_collects_fresh() {
    let mut heap = LuaHeap::new(limits());
    let meta = heap.alloc().expect("meta");
    let owner = heap.alloc().expect("owner");
    let dead = heap.alloc().expect("dead");
    heap.set_mode_scalar(meta, Value::string("v"))
        .expect("mode");
    heap.set_metatable(owner, Some(meta)).expect("mt");
    heap.set_named_edge(owner, "gone", dead).expect("weak");
    let frame = heap.enter_roots(2).expect("frame");
    heap.write_root(frame, 0, meta).expect("meta");
    heap.write_root(frame, 1, owner).expect("owner");
    let marked = heap.mark_and_doomed_tokens();
    assert!(matches!(
        heap.halt_at_publish(marked.modes),
        super::PublishHalt::Published { .. }
    ));
    assert!(heap.alive(dead));
    assert!(heap.has_sweep_tail());
    let report = heap.collect();
    assert!(!heap.has_sweep_tail(), "drain consumed the tail");
    assert!(!heap.alive(dead), "drain released the dead slot");
    assert_eq!(
        heap.stats().collections,
        2,
        "drain records one report, then a fresh cycle"
    );
    assert!(
        report.live_nodes >= 2,
        "the fresh cycle marked the rooted nodes"
    );

    let mut idle = LuaHeap::new(limits());
    let idle_report = idle.collect();
    assert_eq!(
        idle.stats().collections,
        1,
        "no tail is still one fresh collect"
    );
    assert_eq!(idle_report.live_nodes, 0);
}

#[test]
fn published_resume_matches_a_full_collect() {
    fn build(host: &mut vm::HostContext<'_>) -> (TableRef, TableRef, TableRef, i64) {
        heap::install(host, limits()).expect("install");
        let meta = heap::with_heap(host, LuaHeap::alloc).expect("meta");
        let (owner, _) = publish(host);
        let (live, live_token) = publish(host);
        let (dead, _) = publish(host);
        heap::with_heap(host, |heap| {
            heap.set_mode_scalar(meta, Value::string("v"))?;
            heap.set_metatable(owner, Some(meta))?;
            heap.set_named_edge(owner, "stay", live)?;
            heap.set_named_edge(owner, "gone", dead)?;
            heap.set_number_edge(owner, 1.0, dead)?;
            heap.issue_carrier(live)?;
            let frame = heap.enter_roots(3)?;
            heap.write_root(frame, 0, meta)?;
            heap.write_root(frame, 1, owner)?;
            heap.write_root(frame, 2, live)?;
            Ok(())
        })
        .expect("graph");
        (owner, live, dead, live_token)
    }
    fn witness(
        host: &mut vm::HostContext<'_>,
        owner: TableRef,
        live: TableRef,
        dead: TableRef,
    ) -> (f64, bool, bool, bool, u32) {
        heap::with_heap(host, |heap| {
            let started = heap.next_guest(owner, &Value::Null)?;
            let stay = started.is_some_and(|(key, _)| key == Value::string("stay"));
            let gone_cursor = heap.next_guest(owner, &Value::string("gone"))?.is_none();
            Ok((
                heap.raw_sequence_len(owner)?,
                stay,
                gone_cursor,
                heap.named_edge(owner, "stay")? == Some(live)
                    && heap.named_edge(owner, "gone")?.is_none(),
                slot_generation(heap, dead),
            ))
        })
        .expect("witness")
    }

    let resumed = crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let (owner, live, dead, live_token) = build(&mut host);
        assert!(matches!(
            heap::collect_until_published(&mut host).expect("publish"),
            PublishedBoundary::Published { .. }
        ));
        assert!(matches!(
            heap::resume_published_sweep(&mut host, 8).expect("resume"),
            PublishedResume::Completed(_)
        ));
        assert!(token_open(&mut host, live_token));
        witness(&mut host, owner, live, dead)
    });
    let collected = crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let (owner, live, dead, live_token) = build(&mut host);
        heap::collect(&mut host).expect("full collect");
        assert!(token_open(&mut host, live_token));
        witness(&mut host, owner, live, dead)
    });
    assert_eq!(
        resumed, collected,
        "publish plus resume matches a full collect"
    );
    assert_eq!(resumed.0, 0.0, "rawlen ignores the cleared weak number");
    assert!(resumed.1 && resumed.2 && resumed.3);
    assert_ne!(resumed.4, 1, "both paths released the dead slot");
}

fn publish_userdata_resource(host: &mut vm::HostContext<'_>) -> (UserdataRef, u64) {
    let userdata = heap::publish_userdata(host).expect("typed userdata");
    let raw = heap::with_heap(host, |heap| heap.published_userdata_token(userdata))
        .expect("bound userdata token");
    (userdata, raw)
}

fn userdata_token_open(host: &vm::HostContext<'_>, raw: u64) -> bool {
    let handle = vm::ResourceHandle::from_raw(raw).expect("valid userdata handle");
    host.typed_resource_with_key::<heap::LuaUserdataResource>(
        handle,
        &heap::LuaUserdataResource::key(),
    )
    .is_ok()
}

/// The shared Select boundary also covers `MarkSession::sweep`, a pure-heap
/// bypass that never runs the host close step. The refusal must leave the
/// binding, generation, slot, and both real Core resources intact.
#[test]
fn pure_mark_session_sweep_refuses_a_bound_userdata_token() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let (control, control_raw) = publish_userdata_resource(&mut host);
        let (doomed, doomed_raw) = publish_userdata_resource(&mut host);
        let doomed_slot = doomed.slot();
        let doomed_generation = doomed.generation();
        let frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(1)?;
            heap.write_userdata_root(frame, 0, control)?;
            Ok(frame)
        })
        .expect("root control");
        let resources_before = host.resource_count();

        let refused = heap::with_heap(&mut host, |heap| {
            let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut mark = MarkSession::start(heap);
                mark.drain();
                let _ = mark.sweep();
            }))
            .is_err();
            Ok(refused)
        })
        .expect("pure mark session");
        assert!(
            refused,
            "a dead slot with a bound token must be refused before release"
        );
        assert_eq!(
            host.resource_count(),
            resources_before,
            "the pure sweep did not close either resource"
        );
        assert!(userdata_token_open(&host, control_raw));
        assert!(userdata_token_open(&host, doomed_raw));
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.userdata_alive(doomed)))
                .expect("doomed physical"),
            "refusal must not release the slot"
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(doomed_slot_of(heap, doomed_slot)))
                .expect("generation"),
            doomed_generation
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.published_userdata_token(doomed))
                .expect("binding intact"),
            doomed_raw
        );
        assert_eq!(
            heap::stats(&mut host).expect("stats").collections,
            0,
            "refusal must not record a collection"
        );
        assert!(!heap::saved_sweep_tail(&mut host).expect("no tail"));
        let _ = frame;
    });
}

/// `ParkedMark::complete` is the second pure-heap route to `sweep_marked`.
/// It must refuse for the same real bound token and leave the same state.
#[test]
fn pure_parked_mark_complete_refuses_a_bound_userdata_token() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let (control, control_raw) = publish_userdata_resource(&mut host);
        let (doomed, doomed_raw) = publish_userdata_resource(&mut host);
        let doomed_slot = doomed.slot();
        let doomed_generation = doomed.generation();
        let frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(1)?;
            heap.write_userdata_root(frame, 0, control)?;
            Ok(frame)
        })
        .expect("root control");
        let resources_before = host.resource_count();

        let refused = heap::with_heap(&mut host, |heap| {
            let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut parked = ParkedMark::start(heap).expect("parked mark");
                for _ in 0..64 {
                    if parked.pulse(1).finished {
                        break;
                    }
                }
                let _ = parked.complete();
            }))
            .is_err();
            Ok(refused)
        })
        .expect("parked complete");
        assert!(
            refused,
            "ParkedMark::complete must not release a bound dead slot"
        );
        assert_eq!(host.resource_count(), resources_before);
        assert!(userdata_token_open(&host, control_raw));
        assert!(userdata_token_open(&host, doomed_raw));
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.userdata_alive(doomed)))
                .expect("doomed physical")
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(doomed_slot_of(heap, doomed_slot)))
                .expect("generation"),
            doomed_generation
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.published_userdata_token(doomed))
                .expect("binding intact"),
            doomed_raw
        );
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 0);
        let _ = frame;
    });
}

/// Host collection is the positive control: it closes the doomed userdata
/// token before the shared Select, so the same slot is accepted for release.
#[test]
fn host_collect_closes_bound_userdata_before_the_shared_select() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let (control, control_raw) = publish_userdata_resource(&mut host);
        let (doomed, doomed_raw) = publish_userdata_resource(&mut host);
        let doomed_slot = doomed.slot();
        let doomed_generation = doomed.generation();
        let (owner, frame) = heap::with_heap(&mut host, |heap| {
            let meta = heap.alloc()?;
            heap.set_mode_scalar(meta, Value::string("v"))?;
            let owner = heap.alloc()?;
            heap.set_metatable(owner, Some(meta))?;
            heap.set_number_userdata(owner, 1.0, doomed)?;
            let frame = heap.enter_roots(3)?;
            heap.write_root(frame, 0, meta)?;
            heap.write_root(frame, 1, owner)?;
            heap.write_userdata_root(frame, 2, control)?;
            Ok((owner, frame))
        })
        .expect("fixture");
        let resources_before = host.resource_count();
        let _ = heap::take_scan_meter(&mut host);

        let report = heap::collect(&mut host).expect("host collect closes first");
        assert_eq!(report.live_nodes, 3, "meta, owner, and the rooted control");
        assert_eq!(
            heap::take_scan_meter(&mut host)
                .expect("meter")
                .token_closes,
            1,
            "one close for the doomed userdata"
        );
        assert!(!userdata_token_open(&host, doomed_raw));
        assert!(userdata_token_open(&host, control_raw));
        assert_eq!(host.resource_count(), resources_before - 1);
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.userdata_alive(doomed))).expect("released")
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(doomed_slot_of(heap, doomed_slot)))
                .expect("next generation"),
            doomed_generation + 1
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.number_userdata(owner, 1.0))
                .expect("weak cleared"),
            None
        );
        let _ = frame;
    });
}

/// An unregistered empty reservation owns no Core token and is not retained.
/// A normal collection releases its slot and clears the record, so the next
/// reservation is accepted at the next generation instead of being blocked
/// by an empty record that could never be closed.
#[test]
fn unregistered_empty_userdata_reservation_is_collected_and_cleared() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let empty =
            heap::with_heap(&mut host, |heap| heap.reserve_userdata()).expect("empty reservation");
        let slot = empty.slot();
        let generation = empty.generation();
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.userdata_alive(empty))).expect("live"));
        let record = heap::with_heap(&mut host, |heap| heap.pending_userdata_reservation())
            .expect("record read")
            .expect("stored record");
        assert_eq!(record.0, empty);
        assert_eq!(record.1, None, "the reservation is not registered");

        let report = heap::with_heap(&mut host, |heap| Ok(heap.collect())).expect("pure collect");
        assert_eq!(report.live_nodes, 0);
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.userdata_alive(empty))).expect("collected")
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap
                .slot_generation(slot)
                .expect("slot")))
            .expect("generation"),
            generation + 1
        );
        assert!(
            heap::with_heap(&mut host, |heap| heap.pending_userdata_reservation())
                .expect("record read")
                .is_none(),
            "the collection clears the empty reservation record"
        );

        let next =
            heap::with_heap(&mut host, |heap| heap.reserve_userdata()).expect("next reservation");
        assert_eq!(next.slot(), slot);
        assert_eq!(next.generation(), generation + 1);
    });
}

fn doomed_slot_of(heap: &LuaHeap, slot: u32) -> u32 {
    heap.slot_generation(slot).expect("slot exists")
}
