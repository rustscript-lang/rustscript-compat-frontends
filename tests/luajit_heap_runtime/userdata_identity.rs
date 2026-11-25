//! Bare userdata identity on the private heap.
//!
//! These tests use typed handles and the collector. They do not call guest
//! `rawget` or `next`, and they do not install `newproxy`.

use super::{
    EdgeKey, HandleKind, HeapError, HeapLimits, LuaHeap, ParkedMark, PublishHalt, RootFrameId,
    TableRef, UserdataRef, Value,
};

fn room() -> HeapLimits {
    HeapLimits {
        nodes: 24,
        roots: 16,
        edges: 32,
        max_generation: 8,
    }
}

fn raw(handle: UserdataRef) -> (u64, u32, u32, HandleKind) {
    (
        handle.domain(),
        handle.slot(),
        handle.generation(),
        handle.kind(),
    )
}

struct Side {
    owner: TableRef,
    loose: UserdataRef,
    held: UserdataRef,
    value_side: bool,
}

fn arm(heap: &mut LuaHeap, frame: RootFrameId, base: u32, mode: &str, value_side: bool) -> Side {
    let meta = heap.alloc().expect("mode table");
    heap.set_mode_scalar(meta, Value::string(mode))
        .expect("mode");
    heap.write_root(frame, base, meta).expect("mode root");
    let owner = heap.alloc().expect("owner");
    heap.set_metatable(owner, Some(meta)).expect("metatable");
    heap.write_root(frame, base + 1, owner).expect("owner root");
    let loose = heap.alloc_userdata().expect("loose");
    let held = heap.alloc_userdata().expect("held");
    heap.write_userdata_root(frame, base + 2, held)
        .expect("held root");
    if value_side {
        heap.set_number_userdata(owner, 1.0, loose)
            .expect("loose value");
        heap.set_number_userdata(owner, 2.0, held)
            .expect("held value");
    } else {
        heap.set_userdata_key_scalar(owner, loose, Value::string("loose"))
            .expect("loose key");
        heap.set_userdata_key_scalar(owner, held, Value::string("held"))
            .expect("held key");
    }
    Side {
        owner,
        loose,
        held,
        value_side,
    }
}

fn seed(heap: &mut LuaHeap) -> (RootFrameId, [Side; 4]) {
    let frame = heap.enter_roots(12).expect("roots");
    let sides = [
        arm(heap, frame, 0, "k", false),
        arm(heap, frame, 3, "v", true),
        arm(heap, frame, 6, "kv", false),
        arm(heap, frame, 9, "kv", true),
    ];
    (frame, sides)
}

fn assert_distinct(sides: &[Side; 4]) {
    let mut seen = Vec::new();
    for side in sides {
        let loose = raw(side.loose);
        let held = raw(side.held);
        assert_eq!(loose.3, HandleKind::Userdata);
        assert_eq!(held.3, HandleKind::Userdata);
        assert_ne!(loose, held, "two userdata keys must not share an identity");
        assert!(!seen.contains(&loose));
        assert!(!seen.contains(&held));
        seen.push(loose);
        seen.push(held);
    }
}

fn assert_kept(heap: &LuaHeap, side: &Side) {
    assert!(
        heap.userdata_alive(side.loose),
        "loose userdata still issued"
    );
    assert!(heap.userdata_alive(side.held), "held userdata still issued");
    if side.value_side {
        assert_eq!(
            heap.number_userdata(side.owner, 1.0)
                .expect("loose value")
                .map(raw),
            Some(raw(side.loose))
        );
        assert_eq!(
            heap.number_userdata(side.owner, 2.0)
                .expect("held value")
                .map(raw),
            Some(raw(side.held))
        );
    } else {
        assert_eq!(
            heap.userdata_key_value(side.owner, side.loose)
                .expect("loose key"),
            Value::string("loose")
        );
        assert_eq!(
            heap.userdata_key_value(side.owner, side.held)
                .expect("held key"),
            Value::string("held")
        );
    }
}

fn assert_loose_cleared(heap: &LuaHeap, side: &Side) {
    assert!(
        !heap.userdata_alive(side.loose),
        "weak edge must reclaim the loose userdata"
    );
    assert!(
        heap.userdata_alive(side.held),
        "held root must keep its userdata"
    );
    if side.value_side {
        assert_eq!(
            heap.number_userdata(side.owner, 1.0)
                .expect("cleared value"),
            None
        );
        assert_eq!(
            heap.number_userdata(side.owner, 2.0)
                .expect("held value")
                .map(raw),
            Some(raw(side.held))
        );
    } else {
        assert_eq!(
            heap.userdata_key_value(side.owner, side.loose),
            Err(HeapError::StaleHandle)
        );
        assert_eq!(
            heap.userdata_key_value(side.owner, side.held)
                .expect("held key stays"),
            Value::string("held")
        );
    }
}

fn clear_held(heap: &mut LuaHeap, frame: RootFrameId) {
    for slot in [2, 5, 8, 11] {
        heap.clear_root(frame, slot).expect("clear held");
    }
}

#[test]
fn bare_weak_identity_collect_omit_and_clear() {
    let mut collected = LuaHeap::new(room());
    let (frame, sides) = seed(&mut collected);
    assert_distinct(&sides);
    assert_eq!(collected.stats().live_nodes, 16);
    assert_eq!(collected.stats().live_roots, 12);
    let before = collected.stats().live_nodes;
    let report = collected.collect();
    let swept = before - report.live_nodes;
    assert_eq!(swept, 4, "one full collect sweeps the four loose userdata");
    assert_eq!(report.live_nodes, 12);
    assert_eq!(report.live_roots, 12);
    for side in &sides {
        assert_loose_cleared(&collected, side);
    }

    clear_held(&mut collected, frame);
    let before_next = collected.stats().live_nodes;
    let next = collected.collect();
    assert_eq!(
        before_next - next.live_nodes,
        4,
        "the next collect sweeps userdata whose held roots were cleared"
    );
    for side in &sides {
        assert!(!collected.userdata_alive(side.held));
        assert!(!collected.userdata_alive(side.loose));
    }
    assert_eq!(next.live_roots, 8);

    let mut omitted_first = LuaHeap::new(room());
    let (_frame, pending) = seed(&mut omitted_first);
    let stats = omitted_first.stats();
    assert_eq!(stats.collections, 0);
    assert_eq!(stats.live_nodes, 16);
    for side in &pending {
        assert_kept(&omitted_first, side);
    }

    let mut omitted_second = LuaHeap::new(room());
    let (frame, pending) = seed(&mut omitted_second);
    let first = omitted_second.collect();
    assert_eq!(first.live_nodes, 12);
    clear_held(&mut omitted_second, frame);
    let parked = omitted_second.stats();
    assert_eq!(
        parked.live_nodes, 12,
        "clearing a root does not sweep by itself"
    );
    assert_eq!(parked.collections, 1);
    for side in &pending {
        assert!(!omitted_second.userdata_alive(side.loose));
        assert!(
            omitted_second.userdata_alive(side.held),
            "omit-second leaves the held userdata until a real collect"
        );
    }
}

#[test]
fn bare_strong_mode_keeps_value_and_table_identity() {
    let mut heap = LuaHeap::new(room());
    let frame = heap.enter_roots(2).expect("roots");
    let meta = heap.alloc().expect("mode");
    heap.set_mode_scalar(meta, Value::string("strong"))
        .expect("strong has neither k nor v");
    heap.write_root(frame, 0, meta).expect("mode root");
    let owner = heap.alloc().expect("owner");
    heap.set_metatable(owner, Some(meta)).expect("metatable");
    heap.write_root(frame, 1, owner).expect("owner root");

    let loose_key = heap.alloc_userdata().expect("key");
    let loose_value = heap.alloc_userdata().expect("value");
    let table_key = heap.alloc().expect("table key");
    let table_value = heap.alloc().expect("table value");
    heap.set_userdata_key_scalar(owner, loose_key, Value::string("key"))
        .expect("userdata key");
    heap.set_object_scalar(owner, table_key, Value::string("table"))
        .expect("table key");
    heap.set_number_userdata(owner, 1.0, loose_value)
        .expect("userdata value");
    heap.set_number_edge(owner, 3.0, table_value)
        .expect("table value");

    let before = heap.stats().live_nodes;
    let report = heap.collect();
    assert_eq!(
        before - report.live_nodes,
        0,
        "a strong owner keeps unrooted keys and values"
    );
    assert!(
        heap.userdata_alive(loose_value),
        "strong value edge must keep the bare userdata"
    );
    assert!(heap.userdata_alive(loose_key));
    assert!(heap.alive(table_key));
    assert!(heap.alive(table_value));
    assert_ne!(raw(loose_key).1, table_key.slot());
    assert_eq!(
        heap.userdata_key_value(owner, loose_key).expect("key"),
        Value::string("key")
    );
    assert_eq!(
        heap.object_value(owner, table_key).expect("table key"),
        Value::string("table")
    );
    assert_eq!(
        heap.number_value(owner, 1.0),
        Err(HeapError::WrongKind),
        "a userdata value is not a Core number"
    );
    assert_eq!(
        heap.read_table_edge(owner, |stored| matches!(stored, EdgeKey::Number(_))),
        Err(HeapError::WrongKind),
        "a userdata value is not a Core table"
    );
    assert_eq!(
        heap.number_userdata(owner, 3.0),
        Err(HeapError::WrongKind),
        "a table value is not userdata"
    );
}

#[test]
fn bare_kind_domain_stale_and_capacity() {
    let mut heap = LuaHeap::new(room());
    let frame = heap.enter_roots(2).expect("roots");
    let owner = heap.alloc().expect("owner");
    heap.write_root(frame, 0, owner).expect("owner");
    let key = heap.alloc_userdata().expect("key");
    heap.write_userdata_root(frame, 1, key).expect("key root");
    heap.set_number_userdata(owner, -0.0, key)
        .expect("normalized zero");
    assert_eq!(
        heap.number_userdata(owner, 0.0)
            .expect("plus zero")
            .map(raw),
        Some(raw(key))
    );

    let forged = UserdataRef::userdata(owner.domain(), owner.slot(), owner.generation());
    let before = heap.stats();
    assert_eq!(
        heap.set_userdata_key_scalar(owner, forged, Value::string("no")),
        Err(HeapError::WrongKind)
    );
    assert_eq!(heap.stats(), before);

    let as_table = TableRef::table(key.domain(), key.slot(), key.generation());
    assert_eq!(
        heap.write_root(frame, 1, as_table),
        Err(HeapError::WrongKind)
    );
    assert!(heap.userdata_alive(key));

    let mut other = LuaHeap::new(room());
    let foreign = other.alloc_userdata().expect("foreign");
    assert_eq!(
        heap.set_userdata_key_scalar(owner, foreign, Value::string("no")),
        Err(HeapError::WrongDomain)
    );
    assert_eq!(
        heap.set_number_userdata(owner, 4.0, foreign),
        Err(HeapError::WrongDomain)
    );
    assert_eq!(heap.stats(), before);

    let mut reused = LuaHeap::new(HeapLimits {
        nodes: 4,
        roots: 2,
        edges: 4,
        max_generation: 8,
    });
    let old = reused.alloc_userdata().expect("old");
    let old_slot = old.slot();
    let old_generation = old.generation();
    let report = reused.collect();
    assert_eq!(report.live_nodes, 0);
    assert!(!reused.userdata_alive(old));
    let fresh = reused.alloc_userdata().expect("reused slot");
    assert_eq!(fresh.slot(), old_slot);
    assert_ne!(fresh.generation(), old_generation);
    let owner = reused.alloc().expect("owner");
    assert_eq!(
        reused.set_userdata_key_scalar(owner, old, Value::string("stale")),
        Err(HeapError::StaleHandle)
    );
    assert_eq!(reused.stats().live_edges, 0);
    reused
        .set_userdata_key_scalar(owner, fresh, Value::string("fresh"))
        .expect("new generation");
    assert_eq!(
        reused.userdata_key_value(owner, fresh).expect("fresh"),
        Value::string("fresh")
    );

    let mut full = LuaHeap::new(HeapLimits {
        nodes: 1,
        roots: 1,
        edges: 1,
        max_generation: 4,
    });
    let only = full.alloc().expect("only node");
    let full_before = full.stats();
    assert_eq!(full.alloc_userdata(), Err(HeapError::NodeCapacity));
    assert_eq!(full.stats(), full_before);
    assert!(full.alive(only));

    let mut edges = LuaHeap::new(HeapLimits {
        nodes: 4,
        roots: 2,
        edges: 1,
        max_generation: 4,
    });
    let owner = edges.alloc().expect("owner");
    let first = edges.alloc_userdata().expect("first");
    let second = edges.alloc_userdata().expect("second");
    edges
        .set_userdata_key_scalar(owner, first, Value::string("kept"))
        .expect("one edge");
    let edge_before = edges.stats();
    assert_eq!(
        edges.set_number_userdata(owner, 1.0, second),
        Err(HeapError::EdgeCapacity)
    );
    assert_eq!(edges.stats(), edge_before);
    assert_eq!(
        edges.userdata_key_value(owner, first).expect("unchanged"),
        Value::string("kept")
    );
}

fn pause_in_gray(heap: &mut LuaHeap) {
    let mut parked = ParkedMark::start(heap).expect("start");
    for _ in 0..32 {
        if parked.phase_name() == "gray" && parked.mutation_open() {
            parked.park().expect("keep the gray mark");
            return;
        }
        let view = parked.pulse(1);
        assert!(!view.finished, "gray must open before the mark finishes");
    }
    panic!("paused mark did not reach gray");
}

#[test]
fn bare_paused_gray_and_published_tail_refuse_writes() {
    let mut heap = LuaHeap::new(room());
    let frame = heap.enter_roots(4).expect("roots");
    let owner = heap.alloc().expect("owner");
    let child = heap.alloc().expect("child");
    heap.set_edge(owner, 0, child).expect("edge");
    heap.write_root(frame, 0, owner).expect("owner");
    heap.write_root(frame, 1, child).expect("child");
    let key = heap.alloc_userdata().expect("key");
    let value = heap.alloc_userdata().expect("value");
    heap.write_userdata_root(frame, 2, key).expect("key");
    heap.write_userdata_root(frame, 3, value).expect("value");
    pause_in_gray(&mut heap);
    let saved = heap.stats();
    let edge = heap.edge(owner, 0).expect("child edge");
    assert_eq!(
        heap.alloc_userdata(),
        Err(HeapError::MarkDenied),
        "paused gray refuses a userdata allocation"
    );
    assert_eq!(
        heap.set_userdata_key_scalar(owner, key, Value::string("no")),
        Err(HeapError::MarkDenied)
    );
    assert_eq!(
        heap.set_number_userdata(owner, 1.0, value),
        Err(HeapError::MarkDenied)
    );
    let stale = UserdataRef::userdata(key.domain(), key.slot(), key.generation().wrapping_add(1));
    assert_eq!(
        heap.set_userdata_key_scalar(owner, stale, Value::string("no")),
        Err(HeapError::StaleHandle),
        "handle checks run before the gray refusal"
    );
    assert_eq!(heap.stats(), saved);
    assert_eq!(heap.edge(owner, 0).expect("unchanged"), edge);
    let resumed = ParkedMark::resume(&mut heap).expect("mark still saved");
    assert_eq!(resumed.phase_name(), "gray");
    assert!(resumed.mutation_open());
    drop(resumed);

    let mut tail = LuaHeap::new(HeapLimits {
        nodes: 8,
        roots: 4,
        edges: 8,
        max_generation: 8,
    });
    let frame = tail.enter_roots(3).expect("roots");
    let meta = tail.alloc().expect("mode");
    let owner = tail.alloc().expect("owner");
    let dead = tail.alloc().expect("dead");
    tail.set_mode_scalar(meta, Value::string("v"))
        .expect("mode");
    tail.set_metatable(owner, Some(meta)).expect("metatable");
    tail.set_named_edge(owner, "gone", dead)
        .expect("weak value");
    tail.write_root(frame, 0, meta).expect("mode");
    tail.write_root(frame, 1, owner).expect("owner");
    let kept = tail.alloc_userdata().expect("kept");
    tail.write_userdata_root(frame, 2, kept).expect("kept");
    let marked = tail.mark_and_doomed_tokens();
    assert!(matches!(
        tail.halt_at_publish(marked.modes),
        PublishHalt::Published { .. }
    ));
    let saved_tail = tail.saved_tail_state();
    let saved_stats = tail.stats();
    assert!(saved_tail.is_some());
    assert_eq!(tail.alloc_userdata(), Err(HeapError::SweepPending));
    assert_eq!(
        tail.set_userdata_key_scalar(owner, kept, Value::string("no")),
        Err(HeapError::SweepPending)
    );
    assert_eq!(
        tail.set_number_userdata(owner, 1.0, kept),
        Err(HeapError::SweepPending)
    );
    assert_eq!(tail.saved_tail_state(), saved_tail);
    assert_eq!(tail.stats(), saved_stats);
    assert!(tail.userdata_alive(kept));
}

fn cursor_gap(heap: &LuaHeap, owner: TableRef, key: &EdgeKey) -> bool {
    heap.find_key(owner.slot(), key).is_some_and(|index| {
        heap.edge_slots[index as usize]
            .edge
            .as_ref()
            .is_some_and(|edge| edge.value.is_cursor_gap())
    })
}

/// A missing nil is not a write. It must not allocate, meter, or steal a gap.
#[test]
fn bare_userdata_key_nil_miss_changes_no_meter() {
    let mut heap = LuaHeap::new(room());
    let frame = heap.enter_roots(3).expect("roots");
    let owner = heap.alloc().expect("owner");
    heap.write_root(frame, 0, owner).expect("owner");
    let key = heap.alloc_userdata().expect("key");
    let peer = heap.alloc_userdata().expect("peer");
    heap.write_userdata_root(frame, 1, key).expect("key");
    heap.write_userdata_root(frame, 2, peer).expect("peer");
    heap.set_userdata_key_scalar(owner, peer, Value::string("peer"))
        .expect("peer");
    let before = heap.stats();
    let bytes = heap.logical_bytes();
    let peer_key = EdgeKey::userdata_key(peer);
    let peer_index = heap.find_key(owner.slot(), &peer_key).expect("peer record");
    assert_eq!(
        heap.set_userdata_key_scalar(owner, key, Value::Null),
        Ok(())
    );
    assert_eq!(
        heap.stats(),
        before,
        "a missing nil must not change occupancy"
    );
    assert_eq!(heap.logical_bytes(), bytes);
    assert!(
        heap.find_key(owner.slot(), &EdgeKey::userdata_key(key))
            .is_none()
    );
    assert_eq!(heap.find_key(owner.slot(), &peer_key), Some(peer_index));
    assert_eq!(
        heap.userdata_key_value(owner, peer).expect("peer"),
        Value::string("peer")
    );
    assert_eq!(
        heap.userdata_key_value(owner, key).expect("miss"),
        Value::Null
    );
    assert!(heap.userdata_alive(key));
}

/// At the edge cap, nil still must not reclaim a real cursor or be refused.
#[test]
fn bare_userdata_key_nil_miss_does_not_reclaim_a_cursor_gap() {
    let mut heap = LuaHeap::new(HeapLimits {
        nodes: 8,
        roots: 4,
        edges: 8,
        max_generation: 8,
    });
    let frame = heap.enter_roots(3).expect("roots");
    let meta = heap.alloc().expect("mode");
    heap.set_mode_scalar(meta, Value::string("v"))
        .expect("mode");
    heap.write_root(frame, 0, meta).expect("mode root");
    let owner = heap.alloc().expect("owner");
    heap.set_metatable(owner, Some(meta)).expect("metatable");
    heap.write_root(frame, 1, owner).expect("owner root");
    let gone = heap.alloc().expect("dead value");
    heap.set_named_edge(owner, "gap", gone).expect("weak value");
    let key = heap.alloc_userdata().expect("key");
    heap.write_userdata_root(frame, 2, key).expect("key root");
    heap.collect();
    let gap = EdgeKey::Utf8("gap".to_string());
    assert!(
        cursor_gap(&heap, owner, &gap),
        "setup must leave the weak cursor, not delete it"
    );
    assert!(heap.userdata_alive(key));
    heap.limits.edges = heap.occupied_edges;
    let occupied = heap.occupied_edges;
    let live = heap.stats().live_edges;
    let bytes = heap.logical_bytes();
    assert_eq!(
        heap.set_userdata_key_scalar(owner, key, Value::Null),
        Ok(())
    );
    assert!(
        cursor_gap(&heap, owner, &gap),
        "nil must not reclaim another cursor to make room"
    );
    assert!(
        heap.find_key(owner.slot(), &EdgeKey::userdata_key(key))
            .is_none()
    );
    assert_eq!(heap.occupied_edges, occupied);
    assert_eq!(heap.stats().live_edges, live);
    assert_eq!(heap.logical_bytes(), bytes);
    assert_eq!(
        heap.userdata_key_value(owner, key).expect("miss"),
        Value::Null
    );
}

#[test]
fn bare_userdata_key_nil_miss_ignores_edge_cap() {
    let mut heap = LuaHeap::new(HeapLimits {
        nodes: 4,
        roots: 3,
        edges: 1,
        max_generation: 4,
    });
    let frame = heap.enter_roots(3).expect("roots");
    let owner = heap.alloc().expect("owner");
    let kept = heap.alloc_userdata().expect("kept");
    let other = heap.alloc_userdata().expect("other");
    heap.write_root(frame, 0, owner).expect("owner");
    heap.write_userdata_root(frame, 1, kept).expect("kept");
    heap.write_userdata_root(frame, 2, other).expect("other");
    heap.set_userdata_key_scalar(owner, kept, Value::string("kept"))
        .expect("only edge");
    let before = heap.stats();
    let kept_key = EdgeKey::userdata_key(kept);
    let kept_index = heap.find_key(owner.slot(), &kept_key).expect("kept record");
    assert_eq!(
        heap.set_userdata_key_scalar(owner, other, Value::Null),
        Ok(()),
        "a missing nil is not constrained by the edge cap"
    );
    assert_eq!(heap.stats(), before);
    assert_eq!(heap.find_key(owner.slot(), &kept_key), Some(kept_index));
    assert!(
        heap.find_key(owner.slot(), &EdgeKey::userdata_key(other))
            .is_none()
    );
    assert_eq!(
        heap.userdata_key_value(owner, kept).expect("kept"),
        Value::string("kept")
    );
    assert!(!cursor_gap(&heap, owner, &kept_key));
}

#[test]
fn bare_userdata_key_nil_delete_releases_the_only_slot() {
    let mut heap = LuaHeap::new(HeapLimits {
        nodes: 6,
        roots: 4,
        edges: 1,
        max_generation: 8,
    });
    let frame = heap.enter_roots(3).expect("roots");
    let owner = heap.alloc().expect("owner");
    let key = heap.alloc_userdata().expect("key");
    let other = heap.alloc_userdata().expect("other");
    heap.write_root(frame, 0, owner).expect("owner");
    heap.write_userdata_root(frame, 1, key).expect("key");
    heap.write_userdata_root(frame, 2, other).expect("other");
    heap.set_userdata_key_scalar(owner, key, Value::string("kept"))
        .expect("only edge");
    assert_eq!(heap.occupied_edges, 1);
    assert_eq!(heap.stats().live_edges, 1);
    assert_eq!(
        heap.set_userdata_key_scalar(owner, key, Value::Null),
        Ok(())
    );
    assert_eq!(
        heap.userdata_key_value(owner, key).expect("deleted"),
        Value::Null
    );
    assert_eq!(heap.occupied_edges, 0, "occupied and live both drop");
    assert_eq!(heap.stats().live_edges, 0, "occupied and live both drop");
    assert!(
        heap.find_key(owner.slot(), &EdgeKey::userdata_key(key))
            .is_none(),
        "nil deletes the record instead of leaving a cursor gap"
    );
    heap.set_userdata_key_scalar(owner, other, Value::string("reused"))
        .expect("the freed slot accepts one new edge");
    assert_eq!(heap.occupied_edges, 1);
    assert_eq!(heap.stats().live_edges, 1);
    assert_eq!(
        heap.userdata_key_value(owner, other).expect("reused"),
        Value::string("reused")
    );
    assert_eq!(
        heap.set_userdata_key_scalar(owner, other, Value::Null),
        Ok(())
    );
    assert!(
        heap.find_key(owner.slot(), &EdgeKey::userdata_key(other))
            .is_none()
    );
    assert_eq!(heap.occupied_edges, 0);
    assert_eq!(heap.stats().live_edges, 0);
    let _report = heap.collect();
    assert_eq!(
        heap.stats().live_edges,
        0,
        "collect leaves no phantom live edges"
    );
    assert_eq!(heap.occupied_edges, 0);
    assert!(heap.userdata_alive(key));
    assert!(heap.userdata_alive(other));
}

#[test]
fn bare_userdata_key_scalar_rejects_int() {
    let mut heap = LuaHeap::new(room());
    let frame = heap.enter_roots(2).expect("roots");
    let owner = heap.alloc().expect("owner");
    let key = heap.alloc_userdata().expect("key");
    heap.write_root(frame, 0, owner).expect("owner");
    heap.write_userdata_root(frame, 1, key).expect("key");
    heap.set_userdata_key_scalar(owner, key, Value::Bool(false))
        .expect("bool");
    assert_eq!(
        heap.userdata_key_value(owner, key).expect("bool"),
        Value::Bool(false)
    );
    heap.set_userdata_key_scalar(owner, key, Value::Float(-0.0))
        .expect("float");
    assert_eq!(
        heap.userdata_key_value(owner, key).expect("float"),
        Value::Float(-0.0)
    );
    let raw = vec![0xff, 0x00];
    heap.set_userdata_key_scalar(owner, key, Value::bytes(raw.clone()))
        .expect("bytes");
    assert_eq!(
        heap.userdata_key_value(owner, key).expect("bytes"),
        Value::bytes(raw)
    );
    heap.set_userdata_key_scalar(owner, key, Value::string("kept"))
        .expect("string");
    assert_eq!(
        heap.userdata_key_value(owner, key).expect("string"),
        Value::string("kept")
    );
    let before = heap.stats();
    let occupied = heap.occupied_edges;
    let stored = EdgeKey::userdata_key(key);
    let index = heap.find_key(owner.slot(), &stored).expect("string record");
    assert_eq!(
        heap.set_userdata_key_scalar(owner, key, Value::Int(41)),
        Err(HeapError::WrongKind)
    );
    assert_eq!(
        heap.userdata_key_value(owner, key).expect("string stays"),
        Value::string("kept")
    );
    assert_eq!(heap.find_key(owner.slot(), &stored), Some(index));
    assert_eq!(heap.stats(), before);
    assert_eq!(heap.occupied_edges, occupied);
}

#[test]
fn bare_userdata_key_scalar_rejects_array() {
    let mut heap = LuaHeap::new(room());
    let frame = heap.enter_roots(2).expect("roots");
    let owner = heap.alloc().expect("owner");
    let fresh = heap.alloc_userdata().expect("fresh");
    heap.write_root(frame, 0, owner).expect("owner");
    heap.write_userdata_root(frame, 1, fresh).expect("fresh");
    let before = heap.stats();
    let occupied = heap.occupied_edges;
    let bytes = heap.logical_bytes();
    let array = Value::array(vec![Value::Int(7), Value::Bool(true)]);
    assert_eq!(
        heap.set_userdata_key_scalar(owner, fresh, array),
        Err(HeapError::WrongKind)
    );
    assert!(
        heap.find_key(owner.slot(), &EdgeKey::userdata_key(fresh))
            .is_none()
    );
    assert_eq!(
        heap.userdata_key_value(owner, fresh).expect("miss"),
        Value::Null
    );
    assert_eq!(heap.stats(), before);
    assert_eq!(heap.occupied_edges, occupied);
    assert_eq!(heap.logical_bytes(), bytes);
    assert!(heap.userdata_alive(fresh));
}

#[test]
fn bare_userdata_key_nil_obeys_handle_and_phase_gates() {
    let mut heap = LuaHeap::new(room());
    let frame = heap.enter_roots(4).expect("roots");
    let owner = heap.alloc().expect("owner");
    let child = heap.alloc().expect("child");
    heap.set_edge(owner, 0, child).expect("edge");
    heap.write_root(frame, 0, owner).expect("owner");
    heap.write_root(frame, 1, child).expect("child");
    let key = heap.alloc_userdata().expect("key");
    let missing = heap.alloc_userdata().expect("missing");
    heap.write_userdata_root(frame, 2, key).expect("key");
    heap.write_userdata_root(frame, 3, missing)
        .expect("missing");
    heap.set_userdata_key_scalar(owner, key, Value::string("kept"))
        .expect("scalar before the mark");
    pause_in_gray(&mut heap);
    let saved = heap.stats();
    let stored = EdgeKey::userdata_key(key);
    assert_eq!(
        heap.set_userdata_key_scalar(owner, key, Value::Null),
        Err(HeapError::MarkDenied),
        "nil still stops at the parked mark"
    );
    assert_eq!(
        heap.userdata_key_value(owner, key).expect("kept"),
        Value::string("kept")
    );
    assert!(heap.find_key(owner.slot(), &stored).is_some());
    assert!(!cursor_gap(&heap, owner, &stored));
    assert_eq!(
        heap.set_userdata_key_scalar(owner, missing, Value::Null),
        Err(HeapError::MarkDenied),
        "a missing nil does not unlink during gray"
    );
    assert!(
        heap.find_key(owner.slot(), &EdgeKey::userdata_key(missing))
            .is_none()
    );
    let stale = UserdataRef::userdata(key.domain(), key.slot(), key.generation().wrapping_add(1));
    assert_eq!(
        heap.set_userdata_key_scalar(owner, stale, Value::Null),
        Err(HeapError::StaleHandle),
        "a bad handle is reported before the gray gate"
    );
    let forged = UserdataRef::userdata(owner.domain(), owner.slot(), owner.generation());
    assert_eq!(
        heap.set_userdata_key_scalar(owner, forged, Value::Null),
        Err(HeapError::WrongKind)
    );
    assert_eq!(heap.stats(), saved);

    let mut tail = LuaHeap::new(HeapLimits {
        nodes: 8,
        roots: 4,
        edges: 8,
        max_generation: 8,
    });
    let frame = tail.enter_roots(4).expect("roots");
    let meta = tail.alloc().expect("mode");
    let owner = tail.alloc().expect("owner");
    let dead = tail.alloc().expect("dead");
    tail.set_mode_scalar(meta, Value::string("v"))
        .expect("mode");
    tail.set_metatable(owner, Some(meta)).expect("metatable");
    tail.set_named_edge(owner, "gone", dead)
        .expect("weak value");
    tail.write_root(frame, 0, meta).expect("mode");
    tail.write_root(frame, 1, owner).expect("owner");
    let kept = tail.alloc_userdata().expect("kept");
    let absent = tail.alloc_userdata().expect("absent");
    tail.write_userdata_root(frame, 2, kept).expect("kept");
    tail.write_userdata_root(frame, 3, absent).expect("absent");
    tail.set_userdata_key_scalar(owner, kept, Value::string("kept"))
        .expect("before publish");
    let marked = tail.mark_and_doomed_tokens();
    assert!(matches!(
        tail.halt_at_publish(marked.modes),
        PublishHalt::Published { .. }
    ));
    let saved_tail = tail.saved_tail_state();
    let saved_stats = tail.stats();
    assert!(saved_tail.is_some());
    assert_eq!(
        tail.set_userdata_key_scalar(owner, kept, Value::Null),
        Err(HeapError::SweepPending)
    );
    assert_eq!(
        tail.set_userdata_key_scalar(owner, absent, Value::Null),
        Err(HeapError::SweepPending)
    );
    let stale = UserdataRef::userdata(
        kept.domain(),
        kept.slot(),
        kept.generation().wrapping_add(1),
    );
    assert_eq!(
        tail.set_userdata_key_scalar(owner, stale, Value::Null),
        Err(HeapError::StaleHandle),
        "a bad handle is reported before the published tail"
    );
    assert_eq!(tail.saved_tail_state(), saved_tail);
    assert_eq!(tail.stats(), saved_stats);
    assert_eq!(
        tail.userdata_key_value(owner, kept).expect("kept"),
        Value::string("kept")
    );
    assert!(
        tail.find_key(owner.slot(), &EdgeKey::userdata_key(absent))
            .is_none()
    );
}
