//! Parked mark on a private heap. The driver keeps the only `&mut LuaHeap`.
//!
//! A pulse of one stops on a real pending record, not on a guessed step count.
//! The bound below is only a harness limit. Sweep stays outside the pulse.

use super::{
    HeapError, HeapLimits, HeapStats, LuaHeap, ParkedMark, PausedOp, PendingScan, ScanMeter,
    TableRef,
};
use vm::Value;

const KEY_X: u32 = 1;
const KEY_Y: u32 = 2;
const KEY_Z: u32 = 3;
const KEY_W: u32 = 1;
const HARNESS_BOUND: u32 = 64;

fn limits(nodes: u32, roots: u32, edges: u32) -> HeapLimits {
    HeapLimits {
        nodes,
        roots,
        edges,
        max_generation: 8,
    }
}

struct Chain {
    a: TableRef,
    x: TableRef,
    y: TableRef,
    z: TableRef,
    d: TableRef,
    w: TableRef,
}

fn chain(heap: &mut LuaHeap) -> Chain {
    let a = heap.alloc().expect("A");
    let x = heap.alloc().expect("X");
    let y = heap.alloc().expect("Y");
    let z = heap.alloc().expect("Z");
    let d = heap.alloc().expect("D");
    let w = heap.alloc().expect("W");
    let frame = heap.enter_roots(1).expect("frame");
    heap.write_root(frame, 0, a).expect("root A");
    heap.set_edge(a, KEY_X, x).expect("A.X");
    heap.set_edge(a, KEY_Y, y).expect("A.Y");
    heap.set_edge(a, KEY_Z, z).expect("A.Z");
    Chain { a, x, y, z, d, w }
}

fn pause_at(parked: &mut ParkedMark<'_>, owner: u32, cursor: u32) -> PendingScan {
    for _ in 0..HARNESS_BOUND {
        let view = parked.pulse(1);
        assert!(
            !view.finished,
            "mark finished before owner {owner} record {cursor} was pending"
        );
        assert_eq!(view.meter.sweep_edge_checks, 0, "a pulse must not sweep");
        assert_eq!(
            view.meter.mark_passes, 1,
            "pause stays in the original pass"
        );
        if let Some(pending) = parked.pending()
            && pending.owner == owner
            && pending.cursor == cursor
        {
            assert_eq!(parked.phase_name(), "gray");
            assert!(parked.mutation_open());
            return pending;
        }
    }
    panic!("pending record {cursor} on owner {owner} was not observed");
}

fn finish(parked: &mut ParkedMark<'_>) -> ScanMeter {
    for _ in 0..HARNESS_BOUND {
        let view = parked.pulse(1);
        assert_eq!(view.meter.sweep_edge_checks, 0, "a pulse must not sweep");
        assert_eq!(view.meter.mark_passes, 1);
        if view.finished {
            return view.meter;
        }
    }
    panic!("mark did not finish within the harness bound");
}

fn string_bytes(value: &Value) -> Option<&[u8]> {
    match value {
        Value::String(text) => Some(text.as_bytes()),
        _ => None,
    }
}

struct Picture {
    stats: HeapStats,
    meter: ScanMeter,
    bytes: u64,
}

fn picture(parked: &ParkedMark<'_>) -> Picture {
    Picture {
        stats: parked.stats(),
        meter: parked.meter(),
        bytes: parked.logical_bytes(),
    }
}

fn assert_same(before: &Picture, after: &Picture) {
    assert_eq!(before.stats, after.stats, "refusal changed occupancy");
    assert_eq!(before.meter, after.meter, "refusal changed the scan meter");
    assert_eq!(before.bytes, after.bytes, "refusal changed payload");
}

#[test]
fn same_slot_reuse_keeps_the_owner_suffix() {
    let mut heap = LuaHeap::new(limits(8, 2, 8));
    let chain = chain(&mut heap);
    let x_slot = chain.x.slot();
    let x_generation = chain.x.generation();
    let z_slot = chain.z.slot();
    let z_generation = chain.z.generation();
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let y_index = parked.field_index(chain.a, KEY_Y).expect("Y record");
    let z_index = parked.field_index(chain.a, KEY_Z).expect("Z record");
    let pending = pause_at(&mut parked, chain.a.slot(), y_index);
    assert_eq!(pending.owner, chain.a.slot());
    assert_eq!(pending.cursor, y_index);
    assert!(!parked.marked(chain.z), "Z is still ahead of the cursor");
    assert!(!parked.marked(chain.w), "W has no root");
    assert!(
        parked.write_allowed(chain.d),
        "D is live, unmarked, and the phase accepts a field write"
    );
    assert_ne!(chain.d.slot(), chain.a.slot());

    let before = parked.meter();
    parked.delete_field(chain.a, KEY_Y).expect("delete Y");
    parked
        .write_field(chain.d, KEY_W, chain.w)
        .expect("reuse the freed record");
    assert_eq!(
        parked.field_index(chain.d, KEY_W),
        Some(y_index),
        "D must reuse Y's record, not a new slot"
    );
    assert_eq!(parked.field_index(chain.a, KEY_Y), None);
    assert_eq!(parked.edge(chain.a, KEY_Y).expect("Y edge"), None);
    let after = parked.meter();
    assert_eq!(after.outgoing_edge_checks, before.outgoing_edge_checks);
    assert_eq!(after.parked_edge_writes, before.parked_edge_writes + 1);
    let pending_after = parked.pending();
    let repairs = after.parked_cursor_repairs;

    let _meter = finish(&mut parked);
    let report = parked.complete().expect("sweep the finished mark");
    drop(parked);
    assert!(
        heap.alive(chain.z),
        "Z must stay reachable from A; pending after the edit was {pending_after:?}, Y record {y_index}, Z record {z_index}, repairs {repairs}"
    );
    assert!(
        !heap.alive(chain.w),
        "W must not be marked through the reused slot; pending after the edit was {pending_after:?}, repairs {repairs}"
    );
    assert_eq!(heap.edge(chain.a, KEY_X).expect("X edge"), Some(chain.x));
    assert_eq!(chain.x.slot(), x_slot);
    assert_eq!(chain.x.generation(), x_generation);
    assert_eq!(heap.edge(chain.a, KEY_Z).expect("Z edge"), Some(chain.z));
    assert_eq!(chain.z.slot(), z_slot);
    assert_eq!(chain.z.generation(), z_generation);
    assert!(!heap.alive(chain.y));
    assert!(!heap.alive(chain.d));
    assert_eq!(
        pending_after.map(|scan| (scan.owner, scan.cursor)),
        Some((chain.a.slot(), z_index)),
        "unlink advances A's cursor to Z"
    );
    let meter = heap.take_scan_meter();
    assert_eq!(meter.mark_passes, 1, "one mark round");
    assert_eq!(
        meter.parked_cursor_repairs, 1,
        "the delete repairs the cursor once"
    );
    assert_eq!(meter.parked_edge_writes, 1);
    assert_eq!(report.live_nodes, 3, "A, X, and Z remain");
    assert_eq!(heap.stats().collections, 1);
}

#[test]
fn delete_only_keeps_the_successor() {
    let mut heap = LuaHeap::new(limits(8, 2, 8));
    let chain = chain(&mut heap);
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let y_index = parked.field_index(chain.a, KEY_Y).expect("Y record");
    let z_index = parked.field_index(chain.a, KEY_Z).expect("Z record");
    pause_at(&mut parked, chain.a.slot(), y_index);
    parked.delete_field(chain.a, KEY_Y).expect("delete Y");
    assert_eq!(parked.field_index(chain.a, KEY_Y), None);
    let pending_after = parked.pending();
    let repairs = parked.meter().parked_cursor_repairs;

    // Without a repair the next charge reads the freed record and hits the
    // occupied-record check. That is the behavior failure, not a compile error.
    let _meter = finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert!(
        heap.alive(chain.z),
        "deleting Y must keep Z; pending was {pending_after:?}, Z record {z_index}, repairs {repairs}"
    );
    assert_eq!(heap.edge(chain.a, KEY_X).expect("X edge"), Some(chain.x));
    assert_eq!(heap.edge(chain.a, KEY_Z).expect("Z edge"), Some(chain.z));
    assert!(!heap.alive(chain.y));
    assert!(!heap.alive(chain.w));
    assert_eq!(
        pending_after.map(|scan| (scan.owner, scan.cursor)),
        Some((chain.a.slot(), z_index))
    );
    let meter = heap.take_scan_meter();
    assert_eq!(meter.mark_passes, 1);
    assert_eq!(meter.parked_cursor_repairs, 1);
    assert_eq!(meter.parked_edge_writes, 0);
    assert_eq!(heap.stats().collections, 1);
}

#[test]
fn omitted_pause_keeps_the_pending_edge() {
    let mut heap = LuaHeap::new(limits(8, 2, 8));
    let chain = chain(&mut heap);
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let y_index = parked.field_index(chain.a, KEY_Y).expect("Y record");
    pause_at(&mut parked, chain.a.slot(), y_index);
    assert_eq!(parked.meter().parked_cursor_repairs, 0);
    assert_eq!(parked.meter().parked_edge_writes, 0);
    finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert_eq!(heap.edge(chain.a, KEY_Y).expect("Y edge"), Some(chain.y));
    assert!(heap.alive(chain.y));
    assert!(heap.alive(chain.x));
    assert!(heap.alive(chain.z));
    assert!(!heap.alive(chain.d));
    assert!(!heap.alive(chain.w));
    let meter = heap.take_scan_meter();
    assert_eq!(meter.parked_cursor_repairs, 0);
    assert_eq!(meter.parked_edge_writes, 0);
    assert_eq!(meter.mark_passes, 1);
}

#[test]
fn tail_slot_reuse_does_not_keep_the_new_target() {
    let mut heap = LuaHeap::new(limits(8, 2, 8));
    let a = heap.alloc().expect("A");
    let x = heap.alloc().expect("X");
    let y = heap.alloc().expect("Y");
    let d = heap.alloc().expect("D");
    let w = heap.alloc().expect("W");
    let frame = heap.enter_roots(1).expect("frame");
    heap.write_root(frame, 0, a).expect("root A");
    heap.set_edge(a, KEY_X, x).expect("A.X");
    heap.set_edge(a, KEY_Y, y).expect("A.Y");
    let x_slot = x.slot();
    let x_generation = x.generation();
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let y_index = parked.field_index(a, KEY_Y).expect("tail record");
    pause_at(&mut parked, a.slot(), y_index);
    parked.delete_field(a, KEY_Y).expect("delete tail");
    parked
        .write_field(d, KEY_W, w)
        .expect("reuse the tail record");
    assert_eq!(parked.field_index(d, KEY_W), Some(y_index));
    assert_ne!(d.slot(), a.slot());
    let pending_after = parked.pending();
    let repairs = parked.meter().parked_cursor_repairs;
    finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert!(
        !heap.alive(w),
        "a tail cursor must not follow the reused slot; pending was {pending_after:?}, repairs {repairs}"
    );
    assert_eq!(heap.edge(a, KEY_X).expect("X edge"), Some(x));
    assert_eq!(x.slot(), x_slot);
    assert_eq!(x.generation(), x_generation);
    assert!(!heap.alive(y));
    assert!(pending_after.is_none(), "no successor ends A's scan");
    let meter = heap.take_scan_meter();
    assert_eq!(meter.parked_cursor_repairs, 1);
    assert_eq!(meter.mark_passes, 1);
    assert_eq!(heap.stats().collections, 1);
}

#[test]
fn closed_phase_refuses_before_a_write() {
    let mut heap = LuaHeap::new(limits(4, 2, 4));
    let owner = heap.alloc().expect("owner");
    let frame = heap.enter_roots(1).expect("frame");
    heap.write_root(frame, 0, owner).expect("root");
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let idle = parked.pulse(0);
    assert!(!idle.finished);
    assert_eq!(idle.meter, ScanMeter::default());
    assert_eq!(parked.phase_name(), "start");
    let before = picture(&parked);
    assert_eq!(
        parked.delete_field(owner, KEY_X),
        Err(HeapError::MarkDenied)
    );
    assert_eq!(
        parked.write_field(owner, KEY_X, owner),
        Err(HeapError::MarkDenied)
    );
    assert_same(&before, &picture(&parked));
    let step = parked.pulse(1);
    assert!(!step.finished);
    assert_eq!(parked.phase_name(), "reset");
    let before = picture(&parked);
    assert_eq!(
        parked.delete_field(owner, KEY_X),
        Err(HeapError::MarkDenied)
    );
    assert_same(&before, &picture(&parked));
    parked.cancel();

    let mut heap = LuaHeap::new(limits(4, 2, 4));
    let mt = heap.alloc().expect("metatable");
    let owner = heap.alloc().expect("owner");
    let mode = "v\0".to_string();
    let frame = heap.enter_roots(2).expect("frame");
    heap.write_root(frame, 0, mt).expect("root mt");
    heap.write_root(frame, 1, owner).expect("root owner");
    heap.set_metatable(owner, Some(mt)).expect("metatable");
    heap.set_mode_scalar(mt, Value::string(mode.clone()))
        .expect("mode");
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let mut saw_mode = false;
    for _ in 0..HARNESS_BOUND {
        let view = parked.pulse(1);
        assert!(!view.finished);
        if matches!(
            parked.phase_name(),
            "mode-open" | "mode-bytes" | "mode-init"
        ) {
            saw_mode = true;
            break;
        }
    }
    assert!(saw_mode, "a mode string must open a mode phase");
    assert!(!parked.mutation_open());
    let before = picture(&parked);
    let stored = parked.field_scalar(mt, LuaHeap::MODE_FIELD).expect("mode");
    assert_eq!(string_bytes(&stored), Some(mode.as_bytes()));
    let mt_ref = parked.metatable(owner).expect("mt read");
    assert_eq!(
        parked.delete_field(owner, KEY_X),
        Err(HeapError::MarkDenied)
    );
    assert_eq!(
        parked.write_field(owner, KEY_X, owner),
        Err(HeapError::MarkDenied)
    );
    assert_eq!(parked.deny(PausedOp::Mode), Err(HeapError::MarkDenied));
    assert_same(&before, &picture(&parked));
    let stored = parked.field_scalar(mt, LuaHeap::MODE_FIELD).expect("mode");
    assert_eq!(string_bytes(&stored), Some(mode.as_bytes()));
    assert_eq!(parked.metatable(owner).expect("mt read"), mt_ref);
    parked.cancel();
}

#[test]
fn open_phase_refuses_marked_owner_and_forbidden_ops() {
    let mut heap = LuaHeap::new(limits(8, 2, 8));
    let chain = chain(&mut heap);
    let extra = heap.alloc().expect("extra");
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let y_index = parked.field_index(chain.a, KEY_Y).expect("Y record");
    pause_at(&mut parked, chain.a.slot(), y_index);
    assert!(parked.marked(chain.a));
    assert!(!parked.write_allowed(chain.a));
    let before = picture(&parked);
    let y_edge = parked.edge(chain.a, KEY_Y).expect("Y edge");
    assert_eq!(
        parked.write_field(chain.a, 4, extra),
        Err(HeapError::MarkDenied),
        "a marked owner cannot publish a new edge"
    );
    assert_eq!(
        parked.write_field(chain.a, LuaHeap::MODE_FIELD, extra),
        Err(HeapError::MarkDenied)
    );
    assert_eq!(
        parked.delete_field(chain.a, LuaHeap::MODE_FIELD),
        Err(HeapError::MarkDenied)
    );
    for op in [
        PausedOp::Alloc,
        PausedOp::Rollback,
        PausedOp::Metatable,
        PausedOp::Mode,
        PausedOp::Root,
        PausedOp::Token,
    ] {
        assert_eq!(parked.deny(op), Err(HeapError::MarkDenied));
    }
    assert_eq!(
        parked.ordinary_collect().expect_err("private collect"),
        HeapError::MarkActive
    );
    assert_eq!(
        parked.restart().expect_err("second start"),
        HeapError::MarkActive
    );
    assert_same(&before, &picture(&parked));
    assert_eq!(parked.edge(chain.a, KEY_Y).expect("Y edge"), y_edge);
    assert_eq!(parked.edge(chain.a, 4).expect("new edge"), None);
    assert_eq!(parked.edge(extra, KEY_X).expect("extra"), None);
    parked.cancel();
    assert_eq!(heap.stats().collections, 0);
    assert!(heap.alive(chain.a));
    assert!(heap.alive(chain.y));
    assert!(heap.alive(extra));
}

#[test]
fn cancel_restarts_a_full_collect() {
    let mut heap = LuaHeap::new(limits(8, 2, 8));
    let chain = chain(&mut heap);
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let y_index = parked.field_index(chain.a, KEY_Y).expect("Y record");
    pause_at(&mut parked, chain.a.slot(), y_index);
    parked.cancel();
    assert_eq!(heap.stats().collections, 0);
    assert!(heap.alive(chain.y), "cancel does not sweep");
    let _partial = heap.take_scan_meter();
    heap.collect();
    let meter = heap.take_scan_meter();
    assert_eq!(meter.mark_passes, 1, "the next collect starts a new pass");
    assert_eq!(meter.parked_cursor_repairs, 0);
    assert_eq!(meter.parked_edge_writes, 0);
    assert!(heap.alive(chain.a));
    assert!(heap.alive(chain.x));
    assert!(heap.alive(chain.y));
    assert!(heap.alive(chain.z));
    assert!(!heap.alive(chain.d));
    assert!(!heap.alive(chain.w));
    assert_eq!(heap.edge(chain.a, KEY_Y).expect("Y edge"), Some(chain.y));

    let mut again = ParkedMark::start(&mut heap).expect("restart after cancel");
    let idle = again.pulse(0);
    assert!(!idle.finished);
    assert_eq!(idle.meter.mark_passes, 0);
    assert_eq!(idle.meter, ScanMeter::default());
    again.cancel();
}

#[test]
fn zero_budget_does_not_read_or_finish() {
    let mut heap = LuaHeap::new(limits(4, 2, 4));
    let owner = heap.alloc().expect("owner");
    let frame = heap.enter_roots(1).expect("frame");
    heap.write_root(frame, 0, owner).expect("root");
    heap.set_edge(owner, KEY_X, owner).expect("edge");
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let idle = parked.pulse(0);
    assert!(!idle.finished);
    assert_eq!(idle.meter, ScanMeter::default());
    assert_eq!(parked.phase_name(), "start");
    assert!(parked.pending().is_none());
    assert_eq!(parked.complete(), Err(HeapError::MarkDenied));
    assert_eq!(parked.stats().collections, 0);
    assert_eq!(parked.meter().sweep_edge_checks, 0);
    parked.cancel();
}

#[test]
fn small_budget_stays_inside_the_pulse() {
    let edges = 8u32;
    let budget = 3u64;
    let mut heap = LuaHeap::new(limits(2, 1, edges));
    let owner = heap.alloc().expect("owner");
    let frame = heap.enter_roots(1).expect("frame");
    heap.write_root(frame, 0, owner).expect("root");
    for key in 1..=edges {
        heap.set_edge(owner, key, owner).expect("edge");
    }
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let first = parked.pulse(budget);
    assert!(!first.finished, "budget {budget} must not walk the owner");
    assert!(first.meter.outgoing_edge_checks <= budget);
    assert_eq!(first.meter.sweep_edge_checks, 0);
    assert_eq!(parked.complete(), Err(HeapError::MarkDenied));
    assert_eq!(parked.stats().collections, 0);
    let mut pulses = 0u32;
    loop {
        let before = parked.meter();
        let view = parked.pulse(budget);
        let edge_delta = (view.meter.outgoing_edge_checks + view.meter.outgoing_gap_checks)
            - (before.outgoing_edge_checks + before.outgoing_gap_checks);
        assert!(edge_delta <= budget);
        assert_eq!(view.meter.sweep_edge_checks, 0);
        assert_eq!(view.meter.mark_passes, 1);
        assert_eq!(view.meter.parked_edge_writes, 0);
        assert_eq!(view.meter.parked_cursor_repairs, 0);
        if view.finished {
            break;
        }
        pulses += 1;
        assert!(pulses < HARNESS_BOUND);
    }
    let report = parked.complete().expect("sweep after the mark");
    drop(parked);
    let parked_meter = heap.take_scan_meter();
    assert_eq!(parked_meter.outgoing_edge_checks, u64::from(edges));
    assert_eq!(parked_meter.mark_passes, 1);
    assert_eq!(parked_meter.parked_edge_writes, 0);
    assert_eq!(parked_meter.parked_cursor_repairs, 0);
    assert_eq!(report.live_nodes, 1);

    let mut twin = LuaHeap::new(limits(2, 1, edges));
    let twin_owner = twin.alloc().expect("twin");
    let frame = twin.enter_roots(1).expect("frame");
    twin.write_root(frame, 0, twin_owner).expect("root");
    for key in 1..=edges {
        twin.set_edge(twin_owner, key, twin_owner).expect("edge");
    }
    let twin_report = twin.collect();
    let twin_meter = twin.take_scan_meter();
    assert_eq!(parked_meter, twin_meter);
    assert_eq!(report.marked_nodes, twin_report.marked_nodes);
    assert_eq!(report.live_nodes, twin_report.live_nodes);
}

#[test]
fn weak_graph_budget_one_matches_full_collect() {
    fn build(heap: &mut LuaHeap) -> (TableRef, TableRef) {
        let mt = heap.alloc().expect("metatable");
        let owner = heap.alloc().expect("owner");
        let orphan = heap.alloc().expect("orphan");
        let frame = heap.enter_roots(2).expect("frame");
        heap.write_root(frame, 0, mt).expect("root mt");
        heap.write_root(frame, 1, owner).expect("root owner");
        heap.set_metatable(owner, Some(mt)).expect("metatable");
        heap.set_mode_scalar(mt, Value::string("v\0".to_string()))
            .expect("mode");
        heap.set_edge(owner, KEY_X, orphan).expect("weak value");
        heap.set_edge(orphan, KEY_X, orphan).expect("cycle");
        (owner, orphan)
    }

    let mut heap = LuaHeap::new(limits(8, 4, 8));
    let (owner, orphan) = build(&mut heap);
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let mark = finish(&mut parked);
    assert_eq!(mark.parked_edge_writes, 0);
    assert_eq!(mark.parked_cursor_repairs, 0);
    assert_eq!(mark.weak_mode_bytes, 2, "v and the NUL, not a second walk");
    let report = parked.complete().expect("sweep");
    drop(parked);
    assert!(heap.alive(owner));
    assert!(
        !heap.alive(orphan),
        "a weak value with no other root is collected"
    );
    let parked_meter = heap.take_scan_meter();

    let mut twin = LuaHeap::new(limits(8, 4, 8));
    let (twin_owner, twin_orphan) = build(&mut twin);
    let twin_report = twin.collect();
    assert!(twin.alive(twin_owner));
    assert!(!twin.alive(twin_orphan));
    assert_eq!(twin.take_scan_meter(), parked_meter);
    assert_eq!(twin_report.live_nodes, report.live_nodes);
    assert_eq!(twin_report.marked_nodes, report.marked_nodes);
    assert_eq!(parked_meter.parked_cursor_repairs, 0);
    assert_eq!(parked_meter.parked_edge_writes, 0);
    assert_eq!(parked_meter.mark_passes, 1);
}
