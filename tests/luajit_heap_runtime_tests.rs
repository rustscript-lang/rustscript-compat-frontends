//! Acceptance for the unwired Lua private table heap.
//!
//! The production files are compiled through `#[path]` and are not registered
//! from `lua/mod.rs`, `lib.rs`, or the `.lua` entry. Weak tables and the
//! LuaJIT `ordered.lua` sample stay in the later integration stage.

#[path = "../src/frontends/lua/heap.rs"]
mod heap;

#[path = "luajit_heap_runtime/mod.rs"]
mod support;

use heap::{CollectReport, HandleKind, HeapLimits, LuaHeap, RootFrameId};
use support::{Probe, alive_entries, body_entries, run_probe};
use vm::Value;

fn tight_limits() -> HeapLimits {
    HeapLimits {
        nodes: 4,
        roots: 2,
        edges: 2,
        max_generation: 4,
    }
}

#[test]
fn alias_write_collect_reuse_and_hard_caps_stay_inside_one_heap() {
    let mut heap = LuaHeap::new(tight_limits());
    let left = heap.alloc().expect("first table");
    assert_eq!(left.kind(), HandleKind::Table);
    let right = heap.alloc().expect("second table");
    let alias = left;
    heap.set_edge(left, 1, right).expect("edge");
    assert_eq!(heap.edge(alias, 1).expect("read alias"), Some(right));
    assert!(heap.alive(left) && heap.alive(right));

    let unrooted = heap.collect();
    assert_eq!(unrooted.live_nodes, 0, "an edge is not a root");
    assert!(!heap.alive(left));
    assert!(!heap.alive(right));
    assert!(heap.edge(left, 1).is_err(), "a stale source is not a readable edge");

    let reused = heap.alloc().expect("reuse freed slot");
    assert_eq!(reused.slot(), right.slot());
    assert_ne!(reused.generation(), right.generation());
    assert!(!heap.alive(right), "stale generation stays dead");
    assert_eq!(heap.stats().node_slots, 2, "reuse must not grow slot storage");

    let dropped = heap.alloc().expect("second reuse");
    let frame: RootFrameId = heap.enter_roots(1).expect("frame");
    heap.write_root(frame, 0, reused).expect("root reused");
    heap.set_edge(reused, 1, dropped).expect("reachable edge");
    let kept: CollectReport = heap.collect();
    assert_eq!(kept.live_nodes, 2, "a rooted source keeps its target");
    assert!(heap.alive(reused));
    assert!(heap.alive(dropped));

    heap.leave_roots(frame).expect("leave");
    let after_leave = heap.collect();
    assert_eq!(after_leave.live_nodes, 0);
    assert_eq!(heap.stats().live_roots, 0);
}

#[test]
fn failed_alloc_and_rollback_do_not_grow_live_storage() {
    let mut heap = LuaHeap::new(HeapLimits {
        nodes: 1,
        roots: 1,
        edges: 1,
        max_generation: 2,
    });
    let first = heap.alloc().expect("only slot");
    let before = heap.stats();
    assert!(heap.alloc().is_err(), "node cap");
    assert_eq!(heap.stats(), before);

    heap.rollback(first).expect("rollback unpublished");
    assert!(!heap.alive(first));
    assert_eq!(heap.stats().live_nodes, 0);
    assert_eq!(
        heap.stats().node_slots, 1,
        "the slot stays for reuse, but it is not a live node"
    );
    let again = heap.alloc().expect("reuse after rollback");
    assert_ne!(again.generation(), first.generation());
    assert!(!heap.alive(first));
    assert!(heap.set_edge(again, 1, again).is_ok());
    let edges = heap.stats().edge_slots;
    assert!(heap.set_edge(again, 2, again).is_err(), "edge cap");
    assert_eq!(heap.stats().live_edges, 1);
    assert_eq!(heap.stats().edge_slots, edges);

    let root = heap.enter_roots(1).expect("one root");
    assert!(heap.enter_roots(1).is_err(), "root cap");
    assert_eq!(heap.stats().root_slots, 1);
    heap.leave_roots(root).expect("leave");
}

#[test]
fn generation_exhaustion_retires_the_slot_instead_of_wrapping() {
    let mut heap = LuaHeap::new(HeapLimits {
        nodes: 1,
        roots: 1,
        edges: 1,
        max_generation: 1,
    });
    let once = heap.alloc().expect("generation 1");
    heap.rollback(once).expect("free");
    assert!(heap.alloc().is_err(), "retired slot is not reused");
    assert!(!heap.alive(once));
    assert_eq!(heap.stats().live_nodes, 0);
    assert_eq!(heap.stats().node_slots, 1, "retired storage remains, but is not live");
}

#[test]
fn cyclic_create_and_collect_tracks_live_nodes_and_roots() {
    let mut heap = LuaHeap::new(HeapLimits {
        nodes: 8,
        roots: 4,
        edges: 8,
        max_generation: 8,
    });
    for round in 0..3 {
        let frame = heap.enter_roots(1).expect("frame");
        let kept = heap.alloc().expect("kept");
        let linked = heap.alloc().expect("linked");
        heap.set_edge(kept, 1, linked).expect("forward");
        heap.set_edge(linked, 1, kept).expect("back edge");
        heap.write_root(frame, 0, kept).expect("root");
        let orphan = heap.alloc().expect("orphan");
        let orphan_link = heap.alloc().expect("orphan link");
        heap.set_edge(orphan, 1, orphan_link).expect("orphan forward");
        heap.set_edge(orphan_link, 1, orphan).expect("orphan back");
        let marked = heap.collect();
        assert_eq!(
            marked.live_nodes, 2,
            "round {round}: rooted cycle stays, orphan cycle is collected"
        );
        assert_eq!(heap.stats().live_roots, 1);
        assert!(heap.alive(kept) && heap.alive(linked));
        assert!(!heap.alive(orphan) && !heap.alive(orphan_link));
        heap.leave_roots(frame).expect("leave");
        assert_eq!(heap.collect().live_nodes, 0);
        assert_eq!(heap.stats().live_roots, 0);
    }
    assert_eq!(heap.stats().node_slots, 4, "later rounds reuse the first four slots");
    assert!(heap.stats().peak_roots <= 4);
}

#[test]
fn a_handle_from_another_heap_is_rejected() {
    let mut left = LuaHeap::new(tight_limits());
    let mut right = LuaHeap::new(tight_limits());
    let handle = left.alloc().expect("left table");
    assert!(!right.alive(handle));
    assert!(right.set_edge(handle, 1, handle).is_err());
}

#[test]
fn typed_resource_boundary_rejects_int_foreign_and_expired_values() {
    let transfer = run_probe(Probe::Transfer).expect("transfer");
    assert_eq!(transfer.stack, vec![Value::Int(1)]);
    assert!(transfer.body_entries > 0, "typed host body ran for a published table");

    let alias = run_probe(Probe::Alias).expect("alias");
    assert_eq!(alias.stack, vec![Value::Int(1)], "same table write is visible through the other handle");

    let before_int = body_entries();
    let rejected_int = run_probe(Probe::RejectInt);
    assert!(rejected_int.is_err(), "ordinary int is not a table: {rejected_int:?}");
    assert_eq!(body_entries(), before_int, "int rejection happens before the host body");

    let published = run_probe(Probe::Publish).expect("publish handle");
    assert_eq!(published.published_raw, published.stack.first().and_then(|value| match value {
        Value::Int(raw) => Some(*raw),
        _ => None,
    }));
    let raw = match published.stack.as_slice() {
        [Value::Int(raw)] => *raw,
        other => panic!("publish did not return one handle: {other:?}"),
    };
    let before_foreign = alive_entries();
    let foreign = run_probe(Probe::RejectForeign { raw });
    assert!(foreign.is_err(), "cross-vm handle is rejected: {foreign:?}");
    assert_eq!(alive_entries(), before_foreign, "foreign rejection is before the host body");

    let before_expired = alive_entries();
    let expired = run_probe(Probe::Expired);
    assert!(expired.is_err(), "expired handle is rejected: {expired:?}");
    assert_eq!(
        alive_entries(),
        before_expired,
        "close runs; the later alive call must not"
    );

    let named = run_probe(Probe::NamedCarrier).expect("named carrier");
    assert_eq!(named.stack, vec![Value::Int(1)]);
    assert_eq!(named.forged_slot, Some(7));
    assert_eq!(named.forged_generation, Some(9));
    assert!(
        rejected_int.is_err() && named.stack == vec![Value::Int(1)],
        "a named record of copyable ints is not provenance; typed resource is the carrier"
    );
}

#[test]
fn nested_argument_root_keeps_the_first_argument_across_collect() {
    let rooted = run_probe(Probe::NestedRooted).expect("rooted nest");
    assert_eq!(rooted.stack, vec![Value::Int(1)]);
    let unrooted = run_probe(Probe::NestedUnrooted).expect("unrooted nest");
    assert_eq!(unrooted.stack, vec![Value::Int(0)]);
}

#[test]
fn publish_failure_against_the_resource_table_rolls_back_the_node() {
    let stats = support::fill_resource_table_then_fail_publish();
    assert_eq!(stats.before_live, stats.after_live);
    assert_eq!(stats.before_slots, stats.after_slots);
    assert!(stats.publish_failed);
}
