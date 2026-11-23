//! Pausable sweep after a finished mark. Pulses share one borrow.
//!
//! Guest `next` and raw reads run only after `drain`. A zero budget does not
//! scan. These tests do not claim a guest-visible atomic publication.

use super::{GuestClass, HeapLimits, LuaHeap, MarkSession, ScanMeter, SweepSession, TableRef};
use vm::Value;

fn limits(nodes: u32, roots: u32, edges: u32) -> HeapLimits {
    HeapLimits {
        nodes,
        roots,
        edges,
        max_generation: 8,
    }
}

fn root_one(heap: &mut LuaHeap, table: TableRef) {
    let frame = heap.enter_roots(1).expect("frame");
    heap.write_root(frame, 0, table).expect("root");
}

fn root_all(heap: &mut LuaHeap, tables: &[TableRef]) {
    let frame = heap.enter_roots(tables.len() as u32).expect("frame");
    for (index, table) in tables.iter().copied().enumerate() {
        heap.write_root(frame, index as u32, table).expect("root");
    }
}

fn next_keys(heap: &LuaHeap, owner: TableRef) -> Vec<Value> {
    let mut keys = Vec::new();
    let mut cursor = Value::Null;
    while let Some((key, _)) = heap.next_guest(owner, &cursor).expect("next") {
        keys.push(key.clone());
        cursor = key;
    }
    keys
}

fn pulse_checked(session: &mut SweepSession<'_>, budget: u64) -> bool {
    let before = session.meter();
    let view = session.pulse(budget);
    assert!(
        view.spent.total() <= budget,
        "pulse spent {} over budget {budget}",
        view.spent.total()
    );
    assert!(view.meter.sweep_edge_checks - before.sweep_edge_checks <= budget);
    assert!(view.meter.scratch.sweep_bitmap_inited - before.scratch.sweep_bitmap_inited <= budget);
    view.finished
}

fn drive(session: &mut SweepSession<'_>, budget: u64) -> ScanMeter {
    let mut pulses = 0u32;
    loop {
        if pulse_checked(session, budget) {
            return session.meter();
        }
        pulses += 1;
        assert!(pulses < 100_000, "sweep did not finish");
    }
}

/// One saturated owner plus slot and root prep must not finish in one unit.
#[test]
fn saturated_owner_budget_one_stops_inside_prep() {
    let edges = 64u32;
    let mut heap = LuaHeap::new(limits(2, 1, edges));
    let owner = heap.alloc().expect("owner");
    let dead = heap.alloc().expect("dead");
    root_one(&mut heap, owner);
    for key in 1..=edges {
        heap.set_edge(owner, key, owner).expect("edge");
    }
    let modes = heap.drain_mark();
    let mut session = SweepSession::start(&mut heap, modes);
    let before = session.meter();
    let view = session.pulse(1);
    assert!(
        !view.finished && view.spent.total() <= 1,
        "budget 1 finished the batch: finished={} spent={}",
        view.finished,
        view.spent.total()
    );
    assert!(view.meter.sweep_edge_checks - before.sweep_edge_checks <= 1);
    let meter = drive(&mut session, 1);
    assert_eq!(
        meter.sweep_edge_checks - before.sweep_edge_checks,
        u64::from(edges)
    );
    let report = session.report();
    drop(session);
    assert_eq!(report.live_nodes, 1);
    assert!(heap.alive(owner));
    assert!(!heap.alive(dead));
    assert_eq!(heap.stats().edge_slots, edges);
    assert_eq!(heap.edge(owner, 1).expect("edge"), Some(owner));
}

/// Prep that fits the budget exactly leaves the first edge for the next pulse.
#[test]
fn exact_budget_exhaustion_continues_on_the_next_pulse() {
    let edges = 8u32;
    let mut heap = LuaHeap::new(limits(2, 1, edges));
    let owner = heap.alloc().expect("owner");
    let dead = heap.alloc().expect("dead");
    root_one(&mut heap, owner);
    for key in 1..=edges {
        heap.set_edge(owner, key, owner).expect("edge");
    }
    let modes = heap.drain_mark();
    let mut session = SweepSession::start(&mut heap, modes);
    let before = session.meter();
    // select 2 + snapshot 2 + first owner head.
    let prep = session.pulse(5);
    assert!(!prep.finished);
    assert_eq!(prep.spent.total(), 5);
    assert_eq!(prep.meter.sweep_edge_checks, before.sweep_edge_checks);
    let one = session.pulse(1);
    assert_eq!(one.spent.total(), 1);
    assert_eq!(one.spent.edges, 1);
    assert_eq!(one.meter.sweep_edge_checks - before.sweep_edge_checks, 1);
    drive(&mut session, 1);
    drop(session);
    assert!(heap.alive(owner));
    assert!(!heap.alive(dead));
}

/// A zero budget, including one issued in the middle, does not scan or mutate.
#[test]
fn zero_budget_does_not_scan_or_mutate() {
    let mut heap = LuaHeap::new(limits(4, 2, 3));
    let owner = heap.alloc().expect("owner");
    let dead = heap.alloc().expect("dead");
    let mt = heap.alloc().expect("mt");
    root_all(&mut heap, &[owner, mt]);
    heap.set_metatable(owner, Some(mt)).expect("mt");
    heap.set_mode_scalar(mt, Value::string("v")).expect("mode");
    heap.set_named_edge(owner, "stay", owner).expect("edge");
    heap.set_named_edge(owner, "gone", dead).expect("edge");
    let modes = heap.drain_mark();
    let mut session = SweepSession::start(&mut heap, modes);
    let marked = session.meter();
    let zero = session.pulse(0);
    assert!(!zero.finished);
    assert_eq!(zero.spent.total(), 0);
    assert_eq!(zero.meter, marked);
    assert_eq!(session.stats().collections, 0);
    assert!(!pulse_checked(&mut session, 3));
    let mid = session.meter();
    let edges = session.stats().edge_slots;
    let again = session.pulse(0);
    assert!(!again.finished);
    assert_eq!(again.spent.total(), 0);
    assert_eq!(again.meter, mid);
    assert_eq!(session.stats().edge_slots, edges);
    assert_eq!(session.stats().collections, 0);
    drive(&mut session, 1);
    drop(session);
    assert!(heap.named_edge(owner, "stay").expect("stay").is_some());
    assert!(heap.named_edge(owner, "gone").expect("gone").is_none());
}

/// An empty graph finishes on a positive budget and spends nothing.
#[test]
fn empty_graph_finishes_without_a_scan() {
    let mut heap = LuaHeap::new(limits(1, 1, 1));
    let modes = heap.drain_mark();
    let mut session = SweepSession::start(&mut heap, modes);
    let zero = session.pulse(0);
    assert!(!zero.finished);
    assert_eq!(zero.spent.total(), 0);
    let done = session.pulse(1);
    assert!(done.finished);
    assert_eq!(done.spent.total(), 0);
    let report = session.report();
    assert_eq!(report.live_nodes, 0);
    assert_eq!(session.stats().collections, 1);
    let extra = session.pulse(1);
    assert!(extra.finished);
    assert_eq!(extra.spent.total(), 0);
    assert_eq!(session.stats().collections, 1);
}

/// Every live node marked: no snapshot, no edge walk, no sweep heads.
#[test]
fn all_live_skips_edges_and_snapshot() {
    let mut heap = LuaHeap::new(limits(1, 1, 1));
    let owner = heap.alloc().expect("owner");
    root_one(&mut heap, owner);
    heap.set_edge(owner, 1, owner).expect("edge");
    let modes = heap.drain_mark();
    let marked = heap.take_scan_meter();
    let mut session = SweepSession::start(&mut heap, modes);
    let mut spent = 0u64;
    loop {
        let view = session.pulse(1);
        spent += view.spent.total();
        assert!(view.spent.total() <= 1);
        if view.finished {
            break;
        }
    }
    let sweep = session.meter();
    drop(session);
    assert_eq!(sweep.scratch.sweep_bitmap_inited, 0);
    assert_eq!(sweep.sweep_edge_checks, 0);
    assert_eq!(sweep.owner_head_checks, 0);
    assert_eq!(marked.owner_head_checks, 1);
    assert_eq!(spent, 2);
    assert!(heap.alive(owner));
    assert_eq!(heap.edge(owner, 1).expect("edge"), Some(owner));
}

/// A dense dead region visits each occupied record once and keeps the live root.
#[test]
fn dense_dead_zone_visits_each_occupied_edge_once() {
    let dead_nodes = 32u32;
    let edges = dead_nodes - 1;
    let mut heap = LuaHeap::new(limits(dead_nodes + 1, 1, edges));
    let live = heap.alloc().expect("live");
    root_one(&mut heap, live);
    let mut previous = heap.alloc().expect("dead");
    for _ in 1..dead_nodes {
        let next = heap.alloc().expect("dead");
        heap.set_edge(previous, 1, next).expect("edge");
        previous = next;
    }
    let modes = heap.drain_mark();
    let before = heap.take_scan_meter();
    let mut session = SweepSession::start(&mut heap, modes);
    let meter = drive(&mut session, 1);
    drop(session);
    assert_eq!(
        meter.sweep_edge_checks - before.sweep_edge_checks,
        u64::from(edges)
    );
    assert_eq!(
        meter.scratch.sweep_bitmap_inited - before.scratch.sweep_bitmap_inited,
        u64::from(dead_nodes + 1)
    );
    assert!(heap.alive(live));
    assert_eq!(heap.stats().live_nodes, 1);
    assert_eq!(heap.stats().edge_slots, 0);
}

/// Two weak groups are fully visible only after the sweep drains.
///
/// The 32 owners are the qualification shape: one shared metatable and one
/// metatable each. This does not claim the guest can observe an atomic
/// publication of that clear.
#[test]
fn two_weak_groups_are_visible_only_after_drain() {
    let group = 16u32;
    let owners = group * 2;
    let nodes = 1 + group + owners + owners + owners;
    let roots = 1 + group + owners + owners;
    let edges = owners * 2 + 1 + group;
    let mut heap = LuaHeap::new(limits(nodes, roots, edges));
    let shared = heap.alloc().expect("shared mt");
    heap.set_mode_scalar(shared, Value::string("v"))
        .expect("mode");
    let mut separate = Vec::new();
    for _ in 0..group {
        let mt = heap.alloc().expect("mt");
        heap.set_mode_scalar(mt, Value::string("v")).expect("mode");
        separate.push(mt);
    }
    let mut kept = Vec::new();
    let mut doomed = Vec::new();
    let mut tables = Vec::new();
    for index in 0..owners {
        let owner = heap.alloc().expect("owner");
        let stay = heap.alloc().expect("stay");
        let gone = heap.alloc().expect("gone");
        let mt = if index < group {
            shared
        } else {
            separate[(index - group) as usize]
        };
        heap.set_metatable(owner, Some(mt)).expect("mt");
        heap.set_named_edge(owner, "gone", gone).expect("weak");
        heap.set_named_edge(owner, "stay", stay).expect("keep");
        tables.push(owner);
        kept.push((owner, stay));
        doomed.push((owner, gone));
    }
    let mut rooted = vec![shared];
    rooted.extend(separate.iter().copied());
    rooted.extend(tables.iter().copied());
    rooted.extend(kept.iter().map(|(_, stay)| *stay));
    root_all(&mut heap, &rooted);
    let modes = heap.drain_mark();
    let mut session = SweepSession::start(&mut heap, modes);
    assert!(!pulse_checked(&mut session, 1));
    assert_eq!(session.stats().collections, 0);
    drive(&mut session, 7);
    assert_eq!(session.stats().collections, 1);
    drop(session);
    for (owner, stay) in kept {
        assert_eq!(heap.named_edge(owner, "stay").expect("stay"), Some(stay));
        assert!(heap.alive(stay));
    }
    for (owner, gone) in doomed {
        assert!(heap.named_edge(owner, "gone").expect("gone").is_none());
        assert!(!heap.alive(gone));
    }
}

/// A dropped middle field edge leaves both named neighbors, in order.
///
/// The field key is not a guest cursor, so a weak dead value is removed
/// rather than turned into a gap. The live target of a dead owner is kept.
#[test]
fn dead_edge_keeps_both_neighbor_records() {
    let mut heap = LuaHeap::new(limits(6, 4, 5));
    let mt = heap.alloc().expect("mt");
    let owner = heap.alloc().expect("owner");
    let left = heap.alloc().expect("left");
    let mid = heap.alloc().expect("mid");
    let right = heap.alloc().expect("right");
    let dead_owner = heap.alloc().expect("dead owner");
    root_all(&mut heap, &[mt, owner, left, right]);
    heap.set_metatable(owner, Some(mt)).expect("mt");
    heap.set_mode_scalar(mt, Value::string("v")).expect("mode");
    heap.issue_carrier(left).expect("left carrier");
    heap.issue_carrier(right).expect("right carrier");
    heap.set_named_edge(owner, "left", left).expect("left");
    heap.set_edge(owner, 1, mid).expect("field");
    heap.set_named_edge(owner, "right", right).expect("right");
    heap.set_named_edge(dead_owner, "out", left).expect("out");
    let modes = heap.drain_mark();
    let before = heap.take_scan_meter();
    let mut session = SweepSession::start(&mut heap, modes);
    drive(&mut session, 1);
    let sweep_edges = session.meter().sweep_edge_checks - before.sweep_edge_checks;
    drop(session);
    assert_eq!(sweep_edges, 5);
    assert_eq!(
        next_keys(&heap, owner),
        vec![Value::string("left"), Value::string("right")]
    );
    assert!(heap.edge(owner, 1).expect("field").is_none());
    assert_eq!(heap.named_edge(owner, "left").expect("left"), Some(left));
    assert_eq!(heap.named_edge(owner, "right").expect("right"), Some(right));
    assert!(heap.alive(left));
    assert!(heap.alive(right));
    assert!(!heap.alive(mid));
    assert!(!heap.alive(dead_owner));
    assert!(heap.named_edge(dead_owner, "out").is_err());
}

/// Weak gaps in two groups stay reachable as cursors and are not raw values.
#[test]
fn weak_gaps_keep_cursor_continuation() {
    let mut heap = LuaHeap::new(limits(8, 6, 8));
    let shared = heap.alloc().expect("shared");
    let separate = heap.alloc().expect("separate");
    heap.set_mode_scalar(shared, Value::string("v"))
        .expect("mode");
    heap.set_mode_scalar(separate, Value::string("v"))
        .expect("mode");
    let mut owners = Vec::new();
    for mt in [shared, separate] {
        let owner = heap.alloc().expect("owner");
        let gone = heap.alloc().expect("gone");
        let stay = heap.alloc().expect("stay");
        heap.set_metatable(owner, Some(mt)).expect("mt");
        heap.issue_carrier(stay).expect("carrier");
        heap.set_named_edge(owner, "gap", gone).expect("gap");
        heap.set_named_edge(owner, "stay", stay).expect("stay");
        owners.push((owner, stay));
        root_all(&mut heap, &[mt, owner, stay]);
    }
    let modes = heap.drain_mark();
    let lookups = heap.scan.weak_mode_lookups;
    let bytes = heap.scan.weak_mode_bytes;
    let mut session = SweepSession::start(&mut heap, modes);
    drive(&mut session, 1);
    assert_eq!(session.meter().weak_mode_lookups, lookups);
    assert_eq!(session.meter().weak_mode_bytes, bytes);
    drop(session);
    for (owner, stay) in owners {
        assert!(heap.named_edge(owner, "gap").expect("gap").is_none());
        assert_eq!(heap.named_edge(owner, "stay").expect("stay"), Some(stay));
        let continued = heap
            .next_guest(owner, &Value::string("gap"))
            .expect("cursor");
        let (key, value) = continued.expect("continuation");
        assert_eq!(key, Value::string("stay"));
        assert_eq!(heap.guest_class(&value), GuestClass::Table(stay));
        assert_eq!(next_keys(&heap, owner), vec![Value::string("stay")]);
    }
}

/// Completion increments the collection count once, then further pulses do not.
#[test]
fn completion_report_is_issued_once() {
    let mut heap = LuaHeap::new(limits(2, 1, 1));
    let owner = heap.alloc().expect("owner");
    let _dead = heap.alloc().expect("dead");
    root_one(&mut heap, owner);
    heap.set_edge(owner, 1, owner).expect("edge");
    let modes = heap.drain_mark();
    let mut session = SweepSession::start(&mut heap, modes);
    drive(&mut session, 1);
    let report = session.report();
    assert_eq!(report.marked_nodes, 1);
    assert_eq!(session.stats().collections, 1);
    session.pulse(4);
    assert_eq!(session.stats().collections, 1);

    let mut twin = LuaHeap::new(limits(2, 1, 1));
    let twin_owner = twin.alloc().expect("owner");
    let _twin_dead = twin.alloc().expect("dead");
    root_one(&mut twin, twin_owner);
    twin.set_edge(twin_owner, 1, twin_owner).expect("edge");
    let mut mark = MarkSession::start(&mut twin);
    mark.drain();
    let via_mark = mark.sweep();
    assert_eq!(via_mark, report);
    assert_eq!(twin.stats().collections, 1);
    assert_eq!(twin.take_scan_meter().sweep_edge_checks, 1);
}

/// A freed slot can be reissued. The survivor's identity does not depend on color.
#[test]
fn reissue_after_drain_keeps_survivor_identity() {
    let mut heap = LuaHeap::new(limits(3, 2, 1));
    let owner = heap.alloc().expect("owner");
    let stay = heap.alloc().expect("stay");
    let dead = heap.alloc().expect("dead");
    let slot = dead.slot();
    let generation = dead.generation();
    root_all(&mut heap, &[owner, stay]);
    heap.set_named_edge(owner, "keep", stay).expect("keep");
    let carrier = heap.issue_carrier(stay).expect("carrier");
    let modes = heap.drain_mark();
    let mut session = SweepSession::start(&mut heap, modes);
    drive(&mut session, 1);
    drop(session);
    assert!(!heap.alive(dead));
    let again = heap.alloc().expect("reissue");
    assert_eq!(again.slot(), slot);
    assert_ne!(again.generation(), generation);
    assert!(
        heap.next_guest(again, &Value::Null)
            .expect("next")
            .is_none()
    );
    assert_eq!(heap.named_edge(owner, "keep").expect("keep"), Some(stay));
    assert_eq!(heap.guest_class(&carrier), GuestClass::Table(stay));
    assert_eq!(heap.issued_value(stay).expect("issued"), carrier);
}

/// `collect` and `MarkSession::sweep` finish on the same sweep meters.
#[test]
fn collect_and_mark_session_share_the_sweep_core() {
    fn build() -> LuaHeap {
        let mut heap = LuaHeap::new(limits(3, 1, 2));
        let owner = heap.alloc().expect("owner");
        let dead = heap.alloc().expect("dead");
        root_one(&mut heap, owner);
        heap.set_named_edge(owner, "keep", owner).expect("keep");
        heap.set_named_edge(owner, "drop", dead).expect("drop");
        heap
    }
    let mut direct = build();
    let report = direct.collect();
    let direct_meter = direct.take_scan_meter();
    let mut paused = build();
    let mut mark = MarkSession::start(&mut paused);
    mark.drain();
    let paused_report = mark.sweep();
    assert_eq!(paused_report, report);
    assert_eq!(paused.take_scan_meter(), direct_meter);
}

/// Dropping a session that has only selected a slot does not finish the sweep.
#[test]
fn drop_does_not_sweep() {
    let mut heap = LuaHeap::new(limits(2, 1, 1));
    let owner = heap.alloc().expect("owner");
    let dead = heap.alloc().expect("dead");
    root_one(&mut heap, owner);
    heap.set_edge(owner, 1, dead).expect("edge");
    let modes = heap.drain_mark();
    let mut session = SweepSession::start(&mut heap, modes);
    assert!(!session.pulse(1).finished);
    drop(session);
    assert_eq!(heap.stats().collections, 0);
    assert!(heap.alive(dead));
    assert_eq!(heap.stats().edge_slots, 1);
}
