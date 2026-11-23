//! Parked mark on a private heap. The driver keeps the only `&mut LuaHeap`.
//!
//! A pulse of one stops on a real pending record, not on a guessed step count.
//! The bound below is only a harness limit. Sweep stays outside the pulse.

use super::{
    HeapError, HeapLimits, HeapStats, LuaHeap, ParkedMark, PausedOp, PendingScan, RootFrameId,
    ScanMeter, TableRef,
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

const KEY_KEPT: u32 = 1;
const KEY_REPLACED: u32 = 3;
const KEY_SUCCESSOR: u32 = 4;
const PASSED_SLOT: u32 = 1;

#[derive(Clone, Copy)]
struct Cycle {
    mt: TableRef,
    watch: TableRef,
    p: TableRef,
    q: TableRef,
    r: TableRef,
    orphan: TableRef,
}

fn plant_cycle(heap: &mut LuaHeap) -> Cycle {
    let mt = heap.alloc().expect("metatable");
    let watch = heap.alloc().expect("weak watch");
    let p = heap.alloc().expect("P");
    let q = heap.alloc().expect("Q");
    let r = heap.alloc().expect("R");
    let orphan = heap.alloc().expect("orphan");
    heap.set_mode_scalar(mt, Value::string("v\0"))
        .expect("weak values");
    heap.set_metatable(watch, Some(mt))
        .expect("watch metatable");
    heap.set_edge(watch, KEY_KEPT, p).expect("weak P");
    heap.set_edge(watch, KEY_REPLACED, orphan)
        .expect("weak orphan");
    heap.set_edge(p, KEY_KEPT, q).expect("P to Q");
    heap.set_edge(q, KEY_KEPT, r).expect("Q to R");
    heap.set_edge(r, KEY_KEPT, p).expect("R to P");
    Cycle {
        mt,
        watch,
        p,
        q,
        r,
        orphan,
    }
}

fn pause_after_root_visits(parked: &mut ParkedMark<'_>, visits: u64) {
    for _ in 0..HARNESS_BOUND {
        let view = parked.pulse(1);
        assert!(!view.finished, "mark finished before {visits} root visits");
        assert_eq!(view.meter.mark_passes, 1);
        if parked.phase_name() == "roots" && parked.meter().root_slot_visits == visits {
            return;
        }
    }
    panic!(
        "did not stop after {visits} root visits, phase {}",
        parked.phase_name()
    );
}

fn pulse_until_gray(parked: &mut ParkedMark<'_>) {
    for _ in 0..HARNESS_BOUND {
        if parked.phase_name() == "gray" {
            return;
        }
        let view = parked.pulse(1);
        assert!(!view.finished, "mark finished before gray");
        assert_eq!(view.meter.mark_passes, 1);
    }
    panic!("did not enter gray, phase {}", parked.phase_name());
}

fn charge_pending(parked: &mut ParkedMark<'_>) -> u32 {
    let owner = parked.pending().expect("edge still pending").owner;
    let view = parked.pulse(1);
    assert!(
        !view.finished,
        "charging the last pending edge finished the mark"
    );
    assert_eq!(view.meter.mark_passes, 1);
    assert!(parked.pending().is_none(), "owner still has a pending edge");
    assert_eq!(parked.scanning_owner(), Some(owner));
    assert_eq!(parked.phase_name(), "gray");
    owner
}

fn same_pending(left: Option<PendingScan>, right: Option<PendingScan>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => left.owner == right.owner && left.cursor == right.cursor,
        _ => false,
    }
}

fn assert_write_did_not_scan(before: &ScanMeter, after: &ScanMeter) {
    assert_eq!(after.mark_passes, 1);
    assert_eq!(after.mark_passes, before.mark_passes);
    assert_eq!(after.outgoing_edge_checks, before.outgoing_edge_checks);
    assert_eq!(after.outgoing_gap_checks, before.outgoing_gap_checks);
    assert_eq!(after.owner_head_checks, before.owner_head_checks);
    assert_eq!(after.root_slot_visits, before.root_slot_visits);
    assert_eq!(after.parked_cursor_repairs, before.parked_cursor_repairs);
}

fn assert_one_more_pulse_unit(parked: &mut ParkedMark<'_>) {
    let before = parked.meter();
    let view = parked.pulse(1);
    assert!(!view.finished);
    assert_eq!(view.meter.mark_passes, 1);
    let edges = (view.meter.outgoing_edge_checks + view.meter.outgoing_gap_checks)
        - (before.outgoing_edge_checks + before.outgoing_gap_checks);
    let roots = view.meter.root_slot_visits - before.root_slot_visits;
    let heads = view.meter.owner_head_checks - before.owner_head_checks;
    assert!(
        edges + roots + heads <= 1,
        "pulse(1) charged edges {edges} roots {roots} heads {heads}"
    );
}

fn cycle_kept(heap: &LuaHeap, cycle: &Cycle) -> bool {
    heap.alive(cycle.p) && heap.alive(cycle.q) && heap.alive(cycle.r)
}

fn cycle_lost_message(heap: &LuaHeap, cycle: &Cycle, index: u32) -> String {
    format!(
        "record {index} stores P but the cycle was collected; P {} Q {} R {} orphan {} barrier_marks {} edge_writes {}",
        heap.alive(cycle.p),
        heap.alive(cycle.q),
        heap.alive(cycle.r),
        heap.alive(cycle.orphan),
        heap.scan.parked_barrier_marks,
        heap.scan.parked_edge_writes,
    )
}

fn assert_cycle_identity(heap: &LuaHeap, cycle: &Cycle) {
    assert_eq!(heap.edge(cycle.p, KEY_KEPT).expect("P to Q"), Some(cycle.q));
    assert_eq!(heap.edge(cycle.q, KEY_KEPT).expect("Q to R"), Some(cycle.r));
    assert_eq!(heap.edge(cycle.r, KEY_KEPT).expect("R to P"), Some(cycle.p));
    assert!(
        !heap.alive(cycle.orphan),
        "independent weak orphan is collected"
    );
}

struct StrongOwner {
    later: TableRef,
    child: TableRef,
    owner: TableRef,
    kept: TableRef,
    successor: Option<TableRef>,
    cycle: Cycle,
}

fn plant_strong_owner(heap: &mut LuaHeap, with_successor: bool) -> StrongOwner {
    let later = heap.alloc().expect("unprocessed owner");
    let child = heap.alloc().expect("unprocessed child");
    heap.set_edge(later, KEY_KEPT, child)
        .expect("unprocessed edge");
    let owner = heap.alloc().expect("owner");
    let kept = heap.alloc().expect("kept successor");
    let old = heap.alloc().expect("old value");
    heap.set_edge(owner, KEY_KEPT, kept).expect("kept field");
    heap.set_edge(owner, KEY_REPLACED, old)
        .expect("field to replace");
    let successor = if with_successor {
        let successor = heap.alloc().expect("pending successor");
        heap.set_edge(owner, KEY_SUCCESSOR, successor)
            .expect("pending field");
        Some(successor)
    } else {
        None
    };
    let cycle = plant_cycle(heap);
    let frame = heap.enter_roots(4).expect("frame");
    heap.write_root(frame, 0, later).expect("root later");
    heap.write_root(frame, 1, owner).expect("root owner");
    heap.write_root(frame, 2, cycle.watch).expect("root watch");
    heap.write_root(frame, 3, cycle.mt).expect("root metatable");
    StrongOwner {
        later,
        child,
        owner,
        kept,
        successor,
        cycle,
    }
}

fn pause_on_owner_edge<'a>(
    heap: &'a mut LuaHeap,
    owner: TableRef,
    key: u32,
) -> (ParkedMark<'a>, u32) {
    let mut parked = ParkedMark::start(heap).expect("park");
    let index = parked.field_index(owner, key).expect("owner field");
    pause_at(&mut parked, owner.slot(), index);
    (parked, index)
}

#[test]
fn barrier_black_owner_keeps_the_cycle_until_release() {
    let mut heap = LuaHeap::new(limits(24, 8, 24));
    let graph = plant_strong_owner(&mut heap, false);
    let (mut parked, index) = pause_on_owner_edge(&mut heap, graph.owner, KEY_REPLACED);
    let scanning = charge_pending(&mut parked);
    assert_eq!(scanning, graph.owner.slot());
    assert!(parked.marked(graph.owner), "black owner is already marked");
    assert!(
        !parked.marked(graph.child),
        "the unprocessed child is not marked by setup"
    );
    assert!(!parked.marked(graph.cycle.p), "P is only weakly reachable");
    let pending = parked.pending();
    let before = parked.meter();
    parked
        .write_field(graph.owner, KEY_REPLACED, graph.cycle.p)
        .expect("in-place replace on a black owner");
    assert_eq!(parked.field_index(graph.owner, KEY_REPLACED), Some(index));
    assert_eq!(
        parked.edge(graph.owner, KEY_REPLACED).expect("replaced"),
        Some(graph.cycle.p)
    );
    assert_eq!(
        parked
            .edge(graph.owner, KEY_KEPT)
            .expect("original successor"),
        Some(graph.kept)
    );
    assert!(
        same_pending(parked.pending(), pending),
        "the owner edge cursor moved"
    );
    assert_eq!(parked.scanning_owner(), Some(graph.owner.slot()));
    assert_eq!(
        parked.meter().parked_edge_writes,
        before.parked_edge_writes + 1
    );
    assert_write_did_not_scan(&before, &parked.meter());
    assert!(
        !parked.marked(graph.child),
        "the write marked an unprocessed child"
    );
    assert_one_more_pulse_unit(&mut parked);
    let view = finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert!(heap.alive(graph.owner) && heap.alive(graph.kept) && heap.alive(graph.child));
    assert!(
        cycle_kept(&heap, &graph.cycle) && heap.scan.parked_barrier_marks == 1,
        "{}",
        cycle_lost_message(&heap, &graph.cycle, index)
    );
    assert_cycle_identity(&heap, &graph.cycle);
    assert_eq!(view.mark_passes, 1);
    assert_eq!(heap.scan.mark_passes, 1);

    heap.clear_edge(graph.owner, &super::EdgeKey::Field(KEY_REPLACED));
    heap.collect();
    assert!(
        !heap.alive(graph.cycle.p) && !heap.alive(graph.cycle.q) && !heap.alive(graph.cycle.r),
        "clearing the replaced field did not release the cycle"
    );
    assert!(heap.alive(graph.owner) && heap.alive(graph.kept));
}

#[test]
fn barrier_gray_owner_keeps_the_cycle_and_the_pending_successor() {
    let mut heap = LuaHeap::new(limits(24, 8, 24));
    let graph = plant_strong_owner(&mut heap, true);
    let successor = graph.successor.expect("pending successor");
    let (mut parked, index) = pause_on_owner_edge(&mut heap, graph.owner, KEY_SUCCESSOR);
    assert_eq!(parked.phase_name(), "gray");
    assert!(parked.marked(graph.owner));
    assert!(!parked.marked(graph.cycle.p));
    assert!(
        !parked.marked(successor),
        "the pending target is not marked yet"
    );
    let pending = parked.pending();
    let before = parked.meter();
    parked
        .write_field(graph.owner, KEY_REPLACED, graph.cycle.p)
        .expect("in-place replace on the current gray owner");
    let replaced = parked
        .field_index(graph.owner, KEY_REPLACED)
        .expect("replaced record");
    assert_ne!(
        Some(replaced),
        pending.map(|cursor| cursor.cursor),
        "the replace reused the pending successor record"
    );
    assert!(
        same_pending(parked.pending(), pending),
        "the owner edge cursor moved"
    );
    assert_eq!(parked.scanning_owner(), Some(graph.owner.slot()));
    assert_eq!(
        parked
            .edge(graph.owner, KEY_SUCCESSOR)
            .expect("pending edge"),
        Some(successor)
    );
    assert_eq!(
        parked
            .edge(graph.owner, KEY_KEPT)
            .expect("original successor"),
        Some(graph.kept)
    );
    assert_eq!(
        parked.meter().parked_edge_writes,
        before.parked_edge_writes + 1
    );
    assert_write_did_not_scan(&before, &parked.meter());
    assert!(
        !parked.marked(successor),
        "the write marked the pending successor"
    );
    assert_one_more_pulse_unit(&mut parked);
    let view = finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert_eq!(
        heap.edge(graph.owner, KEY_SUCCESSOR).expect("successor"),
        Some(successor)
    );
    assert!(heap.alive(successor) && heap.alive(graph.kept));
    assert!(
        cycle_kept(&heap, &graph.cycle) && heap.scan.parked_barrier_marks == 1,
        "{}",
        cycle_lost_message(&heap, &graph.cycle, index)
    );
    assert_cycle_identity(&heap, &graph.cycle);
    assert_eq!(view.mark_passes, 1);
    assert_eq!(heap.scan.mark_passes, 1);
}

#[test]
fn barrier_omitted_write_collects_the_shared_cycle() {
    let mut heap = LuaHeap::new(limits(24, 8, 24));
    let graph = plant_strong_owner(&mut heap, false);
    let (mut parked, _) = pause_on_owner_edge(&mut heap, graph.owner, KEY_REPLACED);
    charge_pending(&mut parked);
    assert_eq!(parked.meter().parked_edge_writes, 0);
    assert_eq!(parked.meter().parked_barrier_marks, 0);
    finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert!(
        !cycle_kept(&heap, &graph.cycle),
        "omitting the write kept the weakly held cycle"
    );
    assert!(!heap.alive(graph.cycle.orphan));
    assert_eq!(heap.scan.parked_barrier_marks, 0);
    assert_eq!(heap.scan.parked_edge_writes, 0);
    assert_eq!(heap.scan.mark_passes, 1);
}

#[test]
fn barrier_weak_owner_replace_does_not_keep_the_cycle() {
    let mut heap = LuaHeap::new(limits(24, 8, 24));
    let owner = heap.alloc().expect("weak owner");
    let old = heap.alloc().expect("old");
    let successor = heap.alloc().expect("successor");
    let cycle = plant_cycle(&mut heap);
    heap.set_metatable(owner, Some(cycle.mt))
        .expect("weak owner");
    heap.set_edge(owner, KEY_REPLACED, old).expect("weak field");
    heap.set_edge(owner, KEY_SUCCESSOR, successor)
        .expect("weak successor");
    let frame = heap.enter_roots(4).expect("frame");
    heap.write_root(frame, 0, owner).expect("root owner");
    heap.write_root(frame, 1, cycle.watch).expect("root watch");
    heap.write_root(frame, 2, cycle.mt).expect("root metatable");
    heap.write_root(frame, 3, successor)
        .expect("keep the successor readable");
    let (mut parked, index) = pause_on_owner_edge(&mut heap, owner, KEY_SUCCESSOR);
    let pending = parked.pending();
    parked
        .write_field(owner, KEY_REPLACED, cycle.p)
        .expect("in-place replace on a weak owner");
    assert_eq!(
        parked.edge(owner, KEY_REPLACED).expect("replaced"),
        Some(cycle.p)
    );
    assert_eq!(
        parked
            .edge(owner, KEY_SUCCESSOR)
            .expect("successor still readable"),
        Some(successor)
    );
    assert!(
        same_pending(parked.pending(), pending),
        "the owner edge cursor moved"
    );
    let replaced = parked
        .field_index(owner, KEY_REPLACED)
        .expect("replaced record");
    assert_ne!(replaced, index, "the replace reused the pending record");
    finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert!(
        !cycle_kept(&heap, &cycle),
        "a weak-value replace kept the cycle at record {index}"
    );
    assert!(!heap.alive(cycle.orphan));
    assert!(heap.alive(owner) && heap.alive(successor));
    assert_eq!(heap.scan.parked_barrier_marks, 0);
    assert_eq!(heap.scan.parked_edge_writes, 1);
    assert_eq!(heap.scan.mark_passes, 1);
}

#[test]
fn barrier_already_marked_target_is_not_enqueued_again() {
    fn run(write: bool) -> (u64, u64, u64) {
        let mut heap = LuaHeap::new(limits(24, 8, 24));
        let graph = plant_strong_owner(&mut heap, false);
        let (mut parked, _) = pause_on_owner_edge(&mut heap, graph.owner, KEY_REPLACED);
        charge_pending(&mut parked);
        assert!(
            parked.marked(graph.later),
            "Later was rooted before the write"
        );
        let before = parked.meter();
        if write {
            parked
                .write_field(graph.owner, KEY_REPLACED, graph.later)
                .expect("replace with an already marked target");
            assert_eq!(
                parked.meter().scratch.work_pushed,
                before.scratch.work_pushed
            );
            assert_eq!(
                parked.meter().parked_barrier_marks,
                before.parked_barrier_marks
            );
        }
        finish(&mut parked);
        let meter = parked.meter();
        parked.cancel();
        (
            meter.outgoing_edge_checks,
            meter.owner_head_checks,
            meter.scratch.work_pushed,
        )
    }

    let written = run(true);
    let omitted = run(false);
    assert_eq!(
        written.0, omitted.0,
        "an already marked target was scanned again"
    );
    assert_eq!(
        written.1, omitted.1,
        "an already marked target opened another owner"
    );
    assert_eq!(
        written.2, omitted.2,
        "an already marked target was queued again"
    );
}

#[test]
fn barrier_full_edges_replace_leaves_the_other_gap() {
    let mut heap = LuaHeap::new(limits(8, 8, 3));
    let mt = heap.alloc().expect("metatable");
    let gap_owner = heap.alloc().expect("gap owner");
    let key_obj = heap.alloc().expect("object key");
    let dead = heap.alloc().expect("dead value");
    let owner = heap.alloc().expect("owner");
    let old = heap.alloc().expect("old value");
    heap.set_mode_scalar(mt, Value::string("v\0"))
        .expect("weak values");
    heap.set_metatable(gap_owner, Some(mt))
        .expect("gap metatable");
    heap.set_object_edge(gap_owner, key_obj, dead)
        .expect("object edge");
    heap.set_edge(owner, KEY_REPLACED, old).expect("full field");
    let frame = heap.enter_roots(4).expect("frame");
    heap.write_root(frame, 0, mt).expect("root metatable");
    heap.write_root(frame, 1, gap_owner)
        .expect("root gap owner");
    heap.write_root(frame, 2, key_obj).expect("root key");
    heap.write_root(frame, 3, owner).expect("root owner");
    heap.collect();
    assert!(!heap.alive(dead));
    assert!(heap.alive(gap_owner) && heap.alive(key_obj) && heap.alive(owner));
    assert_eq!(
        heap.stats().edge_slots,
        3,
        "mode, gap, and field fill the edge cap"
    );
    let _ = heap.take_scan_meter();
    let target = heap.alloc().expect("replacement");
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let field = parked
        .field_index(owner, KEY_REPLACED)
        .expect("field at the cap");
    let gap = parked
        .object_index(gap_owner, key_obj)
        .expect("other owner's gap");
    assert!(parked.record_is_gap(gap), "prepared record is not a gap");
    assert_ne!(field, gap);
    pause_at(&mut parked, owner.slot(), field);
    charge_pending(&mut parked);
    assert!(parked.marked(owner));
    assert!(!parked.marked(target));
    let occupied = parked.meter();
    parked
        .write_field(owner, KEY_REPLACED, target)
        .expect("in-place replace at the edge cap");
    assert_eq!(parked.field_index(owner, KEY_REPLACED), Some(field));
    assert_eq!(parked.object_index(gap_owner, key_obj), Some(gap));
    assert!(
        parked.record_is_gap(gap),
        "the other gap was reclaimed or rewritten"
    );
    assert_eq!(
        parked.stats().edge_slots,
        3,
        "the replace allocated another edge"
    );
    assert_eq!(
        parked.meter().parked_cursor_repairs,
        occupied.parked_cursor_repairs
    );
    assert_eq!(
        parked.meter().parked_edge_writes,
        occupied.parked_edge_writes + 1
    );
    finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert!(
        heap.alive(target) && heap.scan.parked_barrier_marks == 1,
        "record {field} stores the target but it was collected; barrier_marks {} gap {gap}",
        heap.scan.parked_barrier_marks
    );
    assert!(heap.alive(gap_owner) && heap.alive(key_obj));
    assert_eq!(heap.scan.mark_passes, 1);
}

struct PassedSlot {
    frame: RootFrameId,
    child: TableRef,
    cycle: Cycle,
}

fn pause_passed_slot(heap: &mut LuaHeap, gray: bool) -> (ParkedMark<'_>, PassedSlot) {
    let later = heap.alloc().expect("later");
    let child = heap.alloc().expect("later child");
    heap.set_edge(later, KEY_KEPT, child).expect("later edge");
    let tail = heap.alloc().expect("tail");
    let cycle = plant_cycle(heap);
    let frame = heap.enter_roots(4).expect("frame");
    heap.write_root(frame, 0, later).expect("root later");
    heap.write_root(frame, 2, tail).expect("root tail");
    heap.write_root(frame, 3, cycle.watch).expect("root watch");
    let mut parked = ParkedMark::start(heap).expect("park");
    if gray {
        pause_after_root_visits(&mut parked, 4);
        let orphan = parked
            .field_index(cycle.watch, KEY_REPLACED)
            .expect("watch orphan edge");
        pulse_until_gray(&mut parked);
        pause_at(&mut parked, cycle.watch.slot(), orphan);
        charge_pending(&mut parked);
        assert_eq!(parked.phase_name(), "gray");
        assert_eq!(parked.meter().root_slot_visits, 4);
    } else {
        pause_after_root_visits(&mut parked, 2);
        assert_eq!(parked.phase_name(), "roots");
    }
    assert!(
        parked.meter().root_slot_visits > u64::from(PASSED_SLOT),
        "the empty root slot has not been passed"
    );
    assert_eq!(
        parked.root_table(frame, PASSED_SLOT).expect("slot"),
        None,
        "the passed slot was not empty"
    );
    assert!(!parked.marked(cycle.p), "P is marked before the root write");
    (
        parked,
        PassedSlot {
            frame,
            child,
            cycle,
        },
    )
}

fn assert_table_root_kept(gray: bool) {
    let mut heap = LuaHeap::new(limits(24, 8, 24));
    let (mut parked, slot) = pause_passed_slot(&mut heap, gray);
    let before = parked.meter();
    let visits = before.root_slot_visits;
    let pending = parked.pending();
    let scanning = parked.scanning_owner();
    parked
        .write_table_root(slot.frame, PASSED_SLOT, slot.cycle.p)
        .expect("fixed table root update");
    assert_eq!(
        parked.root_table(slot.frame, PASSED_SLOT).expect("stored"),
        Some(slot.cycle.p)
    );
    assert_eq!(parked.meter().root_slot_visits, visits);
    assert_eq!(
        parked.meter().parked_root_writes,
        before.parked_root_writes + 1
    );
    assert!(
        same_pending(parked.pending(), pending),
        "the owner edge cursor moved"
    );
    assert_eq!(parked.scanning_owner(), scanning);
    assert_write_did_not_scan(&before, &parked.meter());
    assert_one_more_pulse_unit(&mut parked);
    let view = finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert!(
        cycle_kept(&heap, &slot.cycle) && heap.scan.parked_barrier_marks == 1,
        "{}",
        cycle_lost_message(&heap, &slot.cycle, PASSED_SLOT)
    );
    assert_cycle_identity(&heap, &slot.cycle);
    assert!(
        heap.alive(slot.child),
        "the resumed gray queue lost Later's child"
    );
    assert_eq!(view.mark_passes, 1);
    assert_eq!(heap.scan.parked_root_writes, 1);
    assert_eq!(heap.scan.mark_passes, 1);
    heap.clear_root(slot.frame, PASSED_SLOT)
        .expect("clear the retaining root");
    heap.collect();
    assert!(
        !heap.alive(slot.cycle.p),
        "clearing the root did not release P"
    );
}

#[test]
fn barrier_roots_passed_slot_keeps_the_table() {
    assert_table_root_kept(false);
}

#[test]
fn barrier_gray_passed_slot_keeps_the_table() {
    assert_table_root_kept(true);
}

#[test]
fn barrier_passed_slot_keeps_the_closure_chain() {
    let mut heap = LuaHeap::new(limits(16, 8, 16));
    let later = heap.alloc().expect("later");
    let tail = heap.alloc().expect("tail");
    let mt = heap.alloc().expect("metatable");
    let watch = heap.alloc().expect("watch");
    let orphan = heap.alloc().expect("orphan");
    let closure = heap.alloc_closure().expect("closure");
    let cell = heap.alloc_upvalue().expect("upvalue");
    let successor = heap.alloc().expect("successor");
    heap.set_mode_scalar(mt, Value::string("v\0"))
        .expect("mode");
    heap.set_metatable(watch, Some(mt))
        .expect("watch metatable");
    heap.set_upvalue_edge(closure, cell).expect("closure cell");
    heap.write_upvalue(cell, successor).expect("cell current");
    heap.set_closure_edge(watch, KEY_KEPT, closure)
        .expect("weak closure");
    heap.set_edge(watch, KEY_REPLACED, orphan)
        .expect("weak orphan");
    let frame = heap.enter_roots(4).expect("frame");
    heap.write_root(frame, 0, later).expect("root later");
    heap.write_root(frame, 2, tail).expect("root tail");
    heap.write_root(frame, 3, watch).expect("root watch");
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    pause_after_root_visits(&mut parked, 2);
    assert_eq!(parked.phase_name(), "roots");
    let before = parked.meter();
    parked
        .write_closure_root(frame, PASSED_SLOT, closure)
        .expect("fixed closure root update");
    assert_eq!(
        parked.root_closure(frame, PASSED_SLOT).expect("stored"),
        Some(closure)
    );
    assert_eq!(parked.meter().root_slot_visits, before.root_slot_visits);
    assert_eq!(parked.meter().parked_root_writes, 1);
    assert_write_did_not_scan(&before, &parked.meter());
    finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert!(
        heap.closure_alive(closure)
            && heap.upvalue_alive(cell)
            && heap.alive(successor)
            && heap.scan.parked_barrier_marks == 1,
        "closure chain collected after a successful root write; closure {} cell {} successor {} barrier_marks {}",
        heap.closure_alive(closure),
        heap.upvalue_alive(cell),
        heap.alive(successor),
        heap.scan.parked_barrier_marks,
    );
    assert_eq!(heap.read_upvalue(closure).expect("chain"), successor);
    assert!(!heap.alive(orphan), "weak orphan was retained");
    assert_eq!(heap.scan.mark_passes, 1);
    assert_eq!(heap.scan.parked_root_writes, 1);
}

#[test]
fn barrier_passed_slot_keeps_the_upvalue_chain() {
    let mut heap = LuaHeap::new(limits(16, 8, 16));
    let later = heap.alloc().expect("later");
    let tail = heap.alloc().expect("tail");
    let mt = heap.alloc().expect("metatable");
    let watch = heap.alloc().expect("watch");
    let orphan = heap.alloc().expect("orphan");
    let cell = heap.alloc_upvalue().expect("upvalue");
    let successor = heap.alloc().expect("successor");
    heap.set_mode_scalar(mt, Value::string("v\0"))
        .expect("mode");
    heap.set_metatable(watch, Some(mt))
        .expect("watch metatable");
    heap.write_upvalue(cell, successor).expect("cell current");
    heap.set_cell_edge(watch, cell).expect("weak cell");
    heap.set_edge(watch, KEY_REPLACED, orphan)
        .expect("weak orphan");
    let frame = heap.enter_roots(4).expect("frame");
    heap.write_root(frame, 0, later).expect("root later");
    heap.write_root(frame, 2, tail).expect("root tail");
    heap.write_root(frame, 3, watch).expect("root watch");
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    pause_after_root_visits(&mut parked, 4);
    let orphan_edge = parked
        .field_index(watch, KEY_REPLACED)
        .expect("orphan edge");
    pulse_until_gray(&mut parked);
    pause_at(&mut parked, watch.slot(), orphan_edge);
    charge_pending(&mut parked);
    assert_eq!(parked.phase_name(), "gray");
    assert!(
        !parked.marked(successor),
        "weak scan marked the cell current"
    );
    let before = parked.meter();
    let scanning = parked.scanning_owner();
    parked
        .write_upvalue_root(frame, PASSED_SLOT, cell)
        .expect("fixed upvalue root update");
    assert_eq!(
        parked.root_upvalue(frame, PASSED_SLOT).expect("stored"),
        Some(cell)
    );
    assert_eq!(parked.meter().root_slot_visits, before.root_slot_visits);
    assert_eq!(parked.scanning_owner(), scanning);
    assert_eq!(parked.meter().parked_root_writes, 1);
    assert_write_did_not_scan(&before, &parked.meter());
    finish(&mut parked);
    parked.complete().expect("sweep");
    drop(parked);
    assert!(
        heap.upvalue_alive(cell) && heap.alive(successor) && heap.scan.parked_barrier_marks == 1,
        "upvalue chain collected after a successful root write; cell {} successor {} barrier_marks {}",
        heap.upvalue_alive(cell),
        heap.alive(successor),
        heap.scan.parked_barrier_marks,
    );
    assert_eq!(heap.current_table(cell).expect("current"), Some(successor));
    assert!(!heap.alive(orphan));
    assert_eq!(heap.scan.mark_passes, 1);
    assert_eq!(heap.scan.parked_root_writes, 1);
}

#[test]
fn barrier_refused_root_write_leaves_the_slot_and_meter() {
    let mut heap = LuaHeap::new(limits(4, 4, 4));
    let frame = heap.enter_roots(2).expect("frame");
    let target = heap.alloc().expect("target");
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    let before = picture(&parked);
    let err = parked
        .write_table_root(frame, 0, target)
        .expect_err("closed phase");
    assert_eq!(err, HeapError::MarkDenied);
    assert_same(&picture(&parked), &before);
    assert_eq!(parked.meter().parked_root_writes, 0);
    assert_eq!(parked.meter().parked_barrier_marks, 0);
    parked.cancel();

    let mut heap = LuaHeap::new(limits(8, 4, 4));
    let stale = heap.alloc().expect("stale");
    heap.rollback(stale).expect("rollback");
    let later = heap.alloc().expect("later");
    let tail = heap.alloc().expect("tail");
    let frame = heap.enter_roots(3).expect("frame");
    heap.write_root(frame, 0, later).expect("root later");
    heap.write_root(frame, 2, tail).expect("root tail");
    let mut parked = ParkedMark::start(&mut heap).expect("park");
    pause_after_root_visits(&mut parked, 2);
    let before = picture(&parked);
    let stale_err = parked
        .write_table_root(frame, PASSED_SLOT, stale)
        .expect_err("stale handle");
    assert_eq!(stale_err, HeapError::StaleHandle);
    assert_same(&picture(&parked), &before);
    assert_eq!(parked.root_table(frame, PASSED_SLOT).expect("slot"), None);
    let slot_err = parked
        .write_table_root(frame, 3, later)
        .expect_err("invalid slot");
    assert_eq!(slot_err, HeapError::StaleFrame);
    assert_same(&picture(&parked), &before);
    assert_eq!(parked.root_table(frame, PASSED_SLOT).expect("slot"), None);
    assert_eq!(parked.meter().parked_root_writes, 0);
    assert_eq!(parked.meter().parked_barrier_marks, 0);
    assert_eq!(parked.meter().mark_passes, 1);
}
