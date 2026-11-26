//! Pausable pending-close enumeration after a finished mark.
//!
//! A pulse spends one scan unit before it checks one slot. Budget 0 checks
//! nothing and appends nothing. `Vec` growth is not a scan unit and is not a
//! wall-clock bound. The session holds the heap borrow. `finish` is the only
//! handoff of tokens and the original mark snapshot. An unfinished session
//! cannot commit, and drop does not sweep.

use super::{HeapLimits, LuaHeap, PendingCloseSession, PendingCloseView, ScanMeter, TableRef};

fn limits(nodes: u32) -> HeapLimits {
    HeapLimits {
        nodes,
        roots: 4,
        edges: 8,
        max_generation: 8,
    }
}

fn root_one(heap: &mut LuaHeap, table: TableRef) {
    let frame = heap.enter_roots(1).expect("frame");
    heap.write_root(frame, 0, table).expect("root");
}

fn bind(heap: &mut LuaHeap, table: TableRef, raw: u64) {
    heap.bind_token(table, raw).expect("bind");
}

fn assert_pulse_meter(before: &ScanMeter, view: &PendingCloseView) {
    assert_eq!(
        view.meter.node_slot_visits - before.node_slot_visits,
        view.spent,
        "node_slot_visits must rise by the slots this pulse checked"
    );
    assert_eq!(
        view.meter.scratch.pending_close_pushed - before.scratch.pending_close_pushed,
        view.pushed,
        "pending_close_pushed must rise by the tokens this pulse appended"
    );
    assert_eq!(
        view.meter.mark_passes, before.mark_passes,
        "enum must not remark"
    );
    assert_eq!(
        view.meter.weak_mode_slot_checks, before.weak_mode_slot_checks,
        "enum must not rescan mode slots"
    );
    assert_eq!(view.meter.root_slot_visits, before.root_slot_visits);
    assert_eq!(view.meter.token_closes, before.token_closes);
    assert_eq!(view.meter.token_slot_checks, before.token_slot_checks);
    assert!(view.spent <= view.meter.node_slot_visits);
}

/// Budget 1 on a wide slot vector checks the first slot and stops.
#[test]
fn budget_one_checks_one_slot_on_a_wide_vector() {
    let mut heap = LuaHeap::new(limits(8));
    let mut tables = Vec::new();
    for raw in 1..=5 {
        let table = heap.alloc().expect("table");
        bind(&mut heap, table, raw);
        tables.push(table);
    }
    let modes = heap.drain_mark();
    let mut session = PendingCloseSession::start(&mut heap, modes);
    let before = session.meter();
    let view = session.pulse(1);
    assert_pulse_meter(&before, &view);
    assert!(
        !view.finished && view.spent == 1 && view.cursor == 1 && view.pushed == 1,
        "budget 1 walked {} slots, pushed {}, finished={}, cursor={}",
        view.spent,
        view.pushed,
        view.finished,
        view.cursor
    );
    assert_eq!(view.meter.node_slot_visits - before.node_slot_visits, 1);
}

/// An empty slot vector finishes on a positive budget and spends nothing.
#[test]
fn empty_vector_finishes_without_a_slot_check() {
    let mut heap = LuaHeap::new(limits(1));
    let modes = heap.drain_mark();
    let mut session = PendingCloseSession::start(&mut heap, modes);
    let before = session.meter();
    let zero = session.pulse(0);
    assert!(!zero.finished);
    assert_eq!(zero.spent, 0);
    assert_eq!(zero.pushed, 0);
    assert_eq!(zero.cursor, 0);
    assert_eq!(zero.meter, before);
    let done = session.pulse(1);
    assert!(done.finished);
    assert_eq!(done.spent, 0);
    assert_eq!(done.pushed, 0);
    assert_eq!(done.cursor, 0);
    assert_eq!(done.meter, before);
    let committed = session.finish();
    assert!(committed.tokens.is_empty());
}

/// Budget 0, including one issued mid-scan, checks no slot and appends nothing.
#[test]
fn zero_budget_checks_no_slot_and_appends_nothing() {
    let mut heap = LuaHeap::new(limits(4));
    let first = heap.alloc().expect("first");
    let second = heap.alloc().expect("second");
    bind(&mut heap, first, 11);
    bind(&mut heap, second, 22);
    let modes = heap.drain_mark();
    let mut session = PendingCloseSession::start(&mut heap, modes);
    let marked = session.meter();
    let zero = session.pulse(0);
    assert!(!zero.finished);
    assert_eq!(zero.spent, 0);
    assert_eq!(zero.pushed, 0);
    assert_eq!(zero.cursor, 0);
    assert_eq!(zero.meter, marked);
    let one = session.pulse(1);
    assert!(!one.finished);
    assert_eq!(one.spent, 1);
    assert_eq!(one.cursor, 1);
    let mid = session.meter();
    let cursor = one.cursor;
    let again = session.pulse(0);
    assert!(!again.finished);
    assert_eq!(again.spent, 0);
    assert_eq!(again.pushed, 0);
    assert_eq!(again.cursor, cursor);
    assert_eq!(again.meter, mid);
    let rest = session.pulse(1);
    assert!(rest.finished);
    assert_eq!(rest.cursor, 2);
    let done = session.finish();
    assert_eq!(done.tokens.len(), 2);
    assert_eq!(done.tokens[0].raw, 11);
    assert_eq!(done.tokens[1].raw, 22);
}

/// A free hole is visited and is not a pending token. Doomed tokens stay in slot order.
#[test]
fn sparse_free_hole_is_visited_and_skipped_for_tokens() {
    let mut heap = LuaHeap::new(limits(4));
    let left = heap.alloc().expect("left");
    let hole = heap.alloc().expect("hole");
    let right = heap.alloc().expect("right");
    bind(&mut heap, left, 3);
    bind(&mut heap, right, 9);
    heap.rollback(hole).expect("free the middle slot");
    assert_eq!(left.slot(), 0);
    assert_eq!(hole.slot(), 1);
    assert_eq!(right.slot(), 2);
    let modes = heap.drain_mark();
    let before_nodes = heap.stats().live_nodes;
    let mut session = PendingCloseSession::start(&mut heap, modes);
    let before = session.meter();
    let view = session.pulse(8);
    assert_pulse_meter(&before, &view);
    assert!(view.finished);
    assert_eq!(view.spent, 3, "free hole is still one slot check");
    assert_eq!(view.cursor, 3);
    assert_eq!(view.pushed, 2);
    assert_eq!(view.meter.mark_passes, before.mark_passes);
    let done = session.finish();
    assert_eq!(done.tokens.len(), 2);
    assert_eq!(done.tokens[0].raw, 3);
    assert_eq!(done.tokens[0].slot, 0);
    assert_eq!(done.tokens[0].generation, left.generation());
    assert_eq!(done.tokens[1].raw, 9);
    assert_eq!(done.tokens[1].slot, 2);
    assert_eq!(done.tokens[1].generation, right.generation());
    // Both raw values were numeric stand-ins, not Core resources. The shared
    // selection now refuses a dead slot that still carries a token, so the
    // simulated bindings are withdrawn after the enumeration witness and
    // before the sweep. The real negative control lives on a typed userdata.
    heap.unbind_token(left).expect("withdraw left stand-in");
    heap.unbind_token(right).expect("withdraw right stand-in");
    let report = heap.sweep_marked(done.modes);
    assert_eq!(report.live_nodes, 0);
    assert_eq!(heap.stats().live_nodes, 0);
    assert_eq!(before_nodes, 2);
    assert!(!heap.alive(left));
    assert!(!heap.alive(right));
}

/// A live slot with no token is checked and is not appended.
#[test]
fn tokenless_live_slot_is_checked_and_not_appended() {
    let mut heap = LuaHeap::new(limits(4));
    let doomed = heap.alloc().expect("doomed");
    let bare = heap.alloc().expect("no token");
    let closure = heap.alloc_closure().expect("closure");
    bind(&mut heap, doomed, 17);
    let modes = heap.drain_mark();
    let mut session = PendingCloseSession::start(&mut heap, modes);
    let before = session.meter();
    let view = session.pulse(3);
    assert_pulse_meter(&before, &view);
    assert!(view.finished);
    assert_eq!(view.spent, 3);
    assert_eq!(view.pushed, 1);
    assert_eq!(view.cursor, 3);
    let done = session.finish();
    assert_eq!(done.tokens.len(), 1);
    assert_eq!(done.tokens[0].raw, 17);
    assert_eq!(done.tokens[0].slot, doomed.slot());
    assert_eq!(done.tokens[0].generation, doomed.generation());
    assert!(heap.closure_alive(closure));
    assert!(heap.alive(bare));
    drop(done);
    assert!(
        heap.alive(bare),
        "enumeration does not release a tokenless node"
    );
}

/// A marked survivor keeps its token out of the pending-close list.
#[test]
fn marked_survivor_token_is_not_pending_close() {
    let mut heap = LuaHeap::new(limits(4));
    let survivor = heap.alloc().expect("survivor");
    let doomed = heap.alloc().expect("doomed");
    bind(&mut heap, survivor, 100);
    bind(&mut heap, doomed, 200);
    root_one(&mut heap, survivor);
    let modes = heap.drain_mark();
    let mut session = PendingCloseSession::start(&mut heap, modes);
    let before = session.meter();
    let view = session.pulse(2);
    assert_pulse_meter(&before, &view);
    assert!(view.finished);
    assert_eq!(view.spent, 2);
    assert_eq!(view.pushed, 1);
    assert_eq!(view.meter.mark_passes, before.mark_passes);
    let done = session.finish();
    assert_eq!(done.tokens.len(), 1);
    assert_eq!(done.tokens[0].raw, 200);
    assert_eq!(done.tokens[0].slot, doomed.slot());
    assert_eq!(done.tokens[0].generation, doomed.generation());
    // The doomed raw was a numeric stand-in. The rooted survivor keeps its
    // 100 binding; only the simulated dead binding is withdrawn so the shared
    // selection can release the tokenless dead slot.
    heap.unbind_token(doomed).expect("withdraw doomed stand-in");
    let report = heap.sweep_marked(done.modes);
    assert_eq!(report.live_nodes, 1);
    assert!(heap.alive(survivor));
    assert!(!heap.alive(doomed));
    assert_eq!(
        heap.published_token(survivor)
            .expect("survivor token stays"),
        100
    );
}

/// A budget equal to the slot count finishes on that boundary. One less stops on the cursor.
#[test]
fn exact_budget_stops_on_the_cursor_boundary() {
    let mut heap = LuaHeap::new(limits(4));
    let mut tables = Vec::new();
    for raw in [7u64, 8, 9] {
        let table = heap.alloc().expect("table");
        bind(&mut heap, table, raw);
        tables.push(table);
    }
    let modes = heap.drain_mark();
    let mut session = PendingCloseSession::start(&mut heap, modes);
    let before = session.meter();
    let short = session.pulse(2);
    assert_pulse_meter(&before, &short);
    assert!(!short.finished);
    assert_eq!(short.spent, 2);
    assert_eq!(short.cursor, 2);
    assert_eq!(short.pushed, 2);
    let mid = session.meter();
    let last = session.pulse(1);
    assert_pulse_meter(&mid, &last);
    assert!(last.finished);
    assert_eq!(last.spent, 1);
    assert_eq!(last.cursor, 3);
    assert_eq!(last.pushed, 1);
    let done = session.finish();
    assert_eq!(
        done.tokens
            .iter()
            .map(|token| (token.raw, token.slot, token.generation))
            .collect::<Vec<_>>(),
        vec![
            (7, tables[0].slot(), tables[0].generation()),
            (8, tables[1].slot(), tables[1].generation()),
            (9, tables[2].slot(), tables[2].generation()),
        ]
    );

    let mut heap = LuaHeap::new(limits(4));
    for raw in [1u64, 2, 3, 4] {
        let table = heap.alloc().expect("table");
        bind(&mut heap, table, raw);
    }
    let modes = heap.drain_mark();
    let mut session = PendingCloseSession::start(&mut heap, modes);
    let before = session.meter();
    let exact = session.pulse(4);
    assert_pulse_meter(&before, &exact);
    assert!(exact.finished, "budget equal to the slot count finishes");
    assert_eq!(exact.spent, 4);
    assert_eq!(exact.cursor, 4);
    assert_eq!(exact.pushed, 4);
    let extra = session.pulse(1);
    assert!(extra.finished);
    assert_eq!(extra.spent, 0);
    assert_eq!(extra.pushed, 0);
    assert_eq!(extra.cursor, 4);
    let _ = session.finish();
}

/// One-slot pulses keep the cursor and emit tokens in ascending slot order.
#[test]
fn resumed_pulses_keep_cursor_and_token_order() {
    let mut heap = LuaHeap::new(limits(8));
    let early = heap.alloc().expect("early");
    heap.rollback(early).expect("bump generation");
    let reused = heap.alloc().expect("reused");
    assert_eq!(reused.slot(), early.slot());
    assert_eq!(reused.generation(), early.generation() + 1);
    bind(&mut heap, reused, 41);
    let bare = heap.alloc().expect("bare");
    let later = heap.alloc().expect("later");
    bind(&mut heap, later, 42);
    let rooted = heap.alloc().expect("rooted");
    bind(&mut heap, rooted, 99);
    root_one(&mut heap, rooted);
    let modes = heap.drain_mark();
    let slots = heap.stats().node_slots;
    let mut session = PendingCloseSession::start(&mut heap, modes);
    let visits_before = session.meter().node_slot_visits;
    let mut cursor = 0u32;
    let mut pushed = 0u64;
    for _ in 0..slots {
        let before = session.meter();
        let view = session.pulse(1);
        assert_pulse_meter(&before, &view);
        assert_eq!(view.spent, 1);
        assert_eq!(view.cursor, cursor + 1);
        cursor = view.cursor;
        pushed += view.pushed;
        if view.finished {
            break;
        }
    }
    assert_eq!(cursor, slots);
    assert_eq!(pushed, 2);
    assert_eq!(
        session.meter().node_slot_visits - visits_before,
        u64::from(slots),
        "one visit per slot, with no second pass"
    );
    let done = session.finish();
    assert_eq!(done.tokens.len(), 2);
    assert_eq!(done.tokens[0].raw, 41);
    assert_eq!(done.tokens[0].slot, reused.slot());
    assert_eq!(done.tokens[0].generation, reused.generation());
    assert_eq!(done.tokens[1].raw, 42);
    assert_eq!(done.tokens[1].slot, later.slot());
    assert_eq!(done.tokens[1].generation, later.generation());
    assert!(
        done.tokens
            .windows(2)
            .all(|pair| pair[0].slot < pair[1].slot)
    );
    assert!(done.tokens.iter().all(|token| token.raw != 99));
    assert!(heap.alive(bare));
    assert!(heap.alive(rooted));
}

/// An unfinished scan cannot hand tokens or the snapshot to sweep.
#[test]
fn unfinished_enumeration_cannot_be_committed() {
    let mut heap = LuaHeap::new(limits(4));
    let first = heap.alloc().expect("first");
    let second = heap.alloc().expect("second");
    bind(&mut heap, first, 5);
    bind(&mut heap, second, 6);
    let modes = heap.drain_mark();
    let mut session = PendingCloseSession::start(&mut heap, modes);
    let view = session.pulse(1);
    assert!(!view.finished);
    assert_eq!(view.cursor, 1);
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| session.finish()));
    assert!(caught.is_err(), "unfinished enumeration must not commit");
    assert_eq!(heap.stats().collections, 0, "refusal must not sweep");
    assert!(heap.alive(first));
    assert!(heap.alive(second));
    assert_eq!(heap.published_token(first).expect("token stays"), 5);
    assert_eq!(heap.published_token(second).expect("token stays"), 6);
}
