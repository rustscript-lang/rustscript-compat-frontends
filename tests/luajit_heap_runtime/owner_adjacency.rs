//! Owner-chain witnesses that the old full-vector index could not express.
//!
//! Keep/reclaim identity, shared long `__mode` parse, and logical-byte deltas
//! stay in the existing heap tests. These cover order, gap reuse, rollback,
//! and the non-table field 0 that must not become a mode pointer.

use super::{HeapLimits, LuaHeap, TableRef};
use vm::Value;

fn next_keys(heap: &LuaHeap, owner: TableRef) -> Vec<Value> {
    let mut keys = Vec::new();
    let mut cursor = Value::Null;
    while let Some((key, _)) = heap.next_guest(owner, &cursor).expect("next") {
        keys.push(key.clone());
        cursor = key;
    }
    keys
}

fn limits(nodes: u32, roots: u32, edges: u32) -> HeapLimits {
    HeapLimits {
        nodes,
        roots,
        edges,
        max_generation: 4,
    }
}

#[test]
fn fan_out_mark_walks_each_owner_edge_once() {
    let edges = 2048;
    let mut heap = LuaHeap::new(limits(1, 1, edges));
    let owner = heap.alloc().expect("owner");
    let frame = heap.enter_roots(1).expect("root");
    heap.write_root(frame, 0, owner).expect("root owner");
    for key in 1..=edges {
        heap.set_edge(owner, key, owner).expect("fan-out edge");
    }
    assert!(heap.alive(owner));
    heap.collect();
    assert!(heap.alive(owner));
    assert_eq!(heap.edge(owner, 1).expect("read"), Some(owner));
    assert_eq!(heap.edge(owner, edges).expect("read"), Some(owner));
    let meter = heap.take_scan_meter();
    assert_eq!(meter.outgoing_edge_checks, u64::from(edges));
    assert_eq!(meter.outgoing_gap_checks, 0);
    assert_eq!(meter.sweep_edge_checks, 0);
    assert_eq!(meter.owner_head_checks, 1);
    assert_eq!(meter.weak_mode_edge_checks, 0);
}

#[test]
fn same_key_gap_stays_in_place_and_deleted_key_appends() {
    let mut heap = LuaHeap::new(limits(8, 4, 8));
    let mt = heap.alloc().expect("mt");
    let owner = heap.alloc().expect("owner");
    let kept = heap.alloc().expect("kept");
    let dead = heap.alloc().expect("dead");
    let frame = heap.enter_roots(3).expect("roots");
    heap.write_root(frame, 0, mt).expect("root mt");
    heap.write_root(frame, 1, owner).expect("root owner");
    heap.write_root(frame, 2, kept).expect("root kept");
    heap.issue_carrier(kept).expect("carrier");
    heap.set_metatable(owner, Some(mt)).expect("metatable");
    heap.set_mode_scalar(mt, Value::string("v")).expect("mode");
    heap.set_named_edge(owner, "a", dead).expect("a");
    heap.set_named_edge(owner, "b", kept).expect("b");
    let occupied = heap.stats().edge_slots;
    heap.collect();
    assert!(!heap.alive(dead));
    assert_eq!(next_keys(&heap, owner), vec![Value::string("b")]);
    heap.set_named_edge(owner, "a", kept).expect("rewrite gap");
    assert_eq!(
        heap.stats().edge_slots,
        occupied,
        "the same key reuses the gap record"
    );
    assert_eq!(
        next_keys(&heap, owner),
        vec![Value::string("a"), Value::string("b")]
    );
    heap.set_named_scalar(owner, "a", Value::Null)
        .expect("delete a");
    assert_eq!(heap.stats().edge_slots, occupied - 1);
    heap.set_named_edge(owner, "a", kept).expect("reinsert a");
    assert_eq!(heap.stats().edge_slots, occupied);
    assert_eq!(
        next_keys(&heap, owner),
        vec![Value::string("b"), Value::string("a")]
    );
}

#[test]
fn cross_owner_gap_reclaim_appends_without_reordering() {
    let mut heap = LuaHeap::new(limits(8, 4, 4));
    let mt = heap.alloc().expect("mt");
    let donor = heap.alloc().expect("donor");
    let survivor = heap.alloc().expect("survivor");
    let kept = heap.alloc().expect("kept");
    let dead = heap.alloc().expect("dead");
    let frame = heap.enter_roots(4).expect("roots");
    heap.write_root(frame, 0, mt).expect("root mt");
    heap.write_root(frame, 1, donor).expect("root donor");
    heap.write_root(frame, 2, survivor).expect("root survivor");
    heap.write_root(frame, 3, kept).expect("root kept");
    heap.issue_carrier(kept).expect("carrier");
    heap.set_metatable(donor, Some(mt)).expect("metatable");
    heap.set_mode_scalar(mt, Value::string("v")).expect("mode");
    heap.set_named_edge(donor, "stay", kept).expect("stay");
    heap.set_named_edge(donor, "gap", dead).expect("gap");
    heap.set_named_edge(survivor, "x", kept).expect("x");
    assert_eq!(heap.stats().edge_slots, 4);
    heap.collect();
    assert!(!heap.alive(dead));
    assert_eq!(
        heap.stats().edge_slots,
        4,
        "the gap still occupies a record"
    );
    assert_eq!(heap.stats().live_edges, 3);
    heap.set_named_edge(survivor, "y", kept)
        .expect("reuse the other owner's gap");
    assert_eq!(
        heap.stats().edge_slots,
        4,
        "the new key reused a freed record instead of growing occupied storage"
    );
    assert_eq!(
        next_keys(&heap, survivor),
        vec![Value::string("x"), Value::string("y")]
    );
    assert_eq!(next_keys(&heap, donor), vec![Value::string("stay")]);
    assert!(heap.named_edge(donor, "gap").expect("read").is_none());
}

#[test]
fn rollback_reissue_starts_from_an_empty_chain() {
    let mut heap = LuaHeap::new(limits(2, 1, 4));
    let owner = heap.alloc().expect("owner");
    let other = heap.alloc().expect("other");
    heap.issue_carrier(other).expect("carrier");
    heap.set_named_edge(owner, "old", other).expect("old edge");
    let slot = owner.slot();
    let generation = owner.generation();
    heap.rollback(owner).expect("rollback");
    assert!(!heap.alive(owner));
    let again = heap.alloc().expect("reuse");
    assert_eq!(again.slot(), slot);
    assert_ne!(again.generation(), generation);
    assert!(
        heap.next_guest(again, &Value::Null)
            .expect("next")
            .is_none()
    );
    heap.set_named_edge(again, "new", other).expect("new edge");
    assert_eq!(next_keys(&heap, again), vec![Value::string("new")]);
    assert!(heap.named_edge(again, "old").expect("read").is_none());
    assert_eq!(heap.stats().live_edges, 1);
}

#[test]
fn deleted_mode_and_reused_owner_do_not_keep_the_old_rule() {
    let mut heap = LuaHeap::new(limits(8, 4, 8));
    let mt = heap.alloc().expect("mt");
    let owner = heap.alloc().expect("owner");
    let dead = heap.alloc().expect("dead");
    let frame = heap.enter_roots(2).expect("roots");
    heap.write_root(frame, 0, mt).expect("root mt");
    heap.write_root(frame, 1, owner).expect("root owner");
    heap.set_metatable(owner, Some(mt)).expect("metatable");
    heap.set_mode_scalar(mt, Value::string("v")).expect("mode");
    heap.set_named_edge(owner, "w", dead).expect("weak edge");
    heap.set_mode_scalar(mt, Value::Null).expect("delete mode");
    heap.collect();
    assert!(heap.alive(dead), "a deleted mode leaves the edge strong");
    let meter = heap.take_scan_meter();
    assert_eq!(meter.weak_mode_edge_checks, 0);
    assert_eq!(meter.weak_mode_bytes, 0);
    assert_eq!(meter.weak_mode_lookups, 0);

    let slot = owner.slot();
    heap.rollback(owner).expect("free owner");
    let reused = heap.alloc().expect("reuse owner slot");
    assert_eq!(reused.slot(), slot);
    let orphan = heap.alloc().expect("orphan");
    heap.write_root(frame, 1, reused).expect("root reused");
    heap.set_named_edge(reused, "w", orphan)
        .expect("strong edge");
    heap.collect();
    assert!(
        heap.alive(orphan),
        "the reused slot does not keep the previous weak rule"
    );
    let meter = heap.take_scan_meter();
    assert_eq!(meter.weak_mode_edge_checks, 0);
    assert_eq!(meter.weak_mode_bytes, 0);
}

#[test]
fn upvalue_field_zero_is_not_parsed_as_a_mode() {
    let mut heap = LuaHeap::new(limits(4, 2, 4));
    let mt = heap.alloc().expect("mt");
    let owner = heap.alloc().expect("owner");
    let cell = heap.alloc_upvalue().expect("cell");
    let frame = heap.enter_roots(2).expect("roots");
    heap.write_root(frame, 0, mt).expect("root mt");
    heap.write_root(frame, 1, owner).expect("root owner");
    heap.set_metatable(owner, Some(mt)).expect("metatable");
    let text = "v".repeat(64);
    heap.write_scalar_upvalue(cell, Value::string(&text))
        .expect("field 0 scalar");
    heap.collect();
    let meter = heap.take_scan_meter();
    assert!(meter.metatable_edge_checks >= 1, "mode parsing is enabled");
    assert!(
        meter.sweep_edge_checks >= 1,
        "the dead upvalue record is visited by sweep"
    );
    assert_eq!(meter.weak_mode_edge_checks, 0);
    assert_eq!(meter.weak_mode_bytes, 0);
    assert_eq!(meter.weak_mode_lookups, 0);
    assert_eq!(meter.scratch.weak_modes_inited, 0);
}
