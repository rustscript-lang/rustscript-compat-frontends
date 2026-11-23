//! First Roots boundary for the current safepoint root batch.
//!
//! Zero fixed root slots and an empty gray queue let one ordinary positive
//! pulse cross Roots and Gray straight to Done. The handoff must commit the
//! borrowed batch inside that pulse, after the stepper enters Roots and before
//! any root or gray step. The slice is not saved on the mark. A later call,
//! including one after park and resume, reads the batch passed to that call.

use super::{HeapError, HeapLimits, LuaHeap, ParkedMark, RootFrameId, Rooted, ScanMeter, TableRef};
use vm::Value;

const KEY_CHILD: u32 = 1;
const KEY_NEXT: u32 = 6;
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
    target: TableRef,
    successor: TableRef,
    orphan: TableRef,
    other: TableRef,
    other_child: TableRef,
}

fn chain(heap: &mut LuaHeap) -> Chain {
    let target = heap.alloc().expect("target");
    let successor = heap.alloc().expect("successor");
    let orphan = heap.alloc().expect("orphan");
    let other = heap.alloc().expect("other");
    let other_child = heap.alloc().expect("other child");
    heap.set_edge(target, KEY_NEXT, successor)
        .expect("target successor");
    heap.set_edge(other, KEY_CHILD, other_child)
        .expect("other child");
    Chain {
        target,
        successor,
        orphan,
        other,
        other_child,
    }
}

fn chain_with_mode(heap: &mut LuaHeap) -> Chain {
    let graph = chain(heap);
    let mode = heap.alloc().expect("mode table");
    let watch = heap.alloc().expect("watch");
    heap.set_mode_scalar(mode, Value::string("v\0"))
        .expect("mode string");
    heap.set_metatable(watch, Some(mode)).expect("metatable");
    graph
}

struct Framed {
    frame: RootFrameId,
    later: TableRef,
    child: TableRef,
    tail: TableRef,
    target: TableRef,
    successor: TableRef,
    orphan: TableRef,
}

fn framed(heap: &mut LuaHeap) -> Framed {
    let later = heap.alloc().expect("later");
    let child = heap.alloc().expect("child");
    heap.set_edge(later, KEY_CHILD, child).expect("later edge");
    let tail = heap.alloc().expect("tail");
    let target = heap.alloc().expect("target");
    let successor = heap.alloc().expect("successor");
    let orphan = heap.alloc().expect("orphan");
    heap.set_edge(target, KEY_NEXT, successor)
        .expect("target successor");
    let frame = heap.enter_roots(4).expect("frame");
    heap.write_root(frame, 0, later).expect("root later");
    heap.write_root(frame, 2, tail).expect("root tail");
    Framed {
        frame,
        later,
        child,
        tail,
        target,
        successor,
        orphan,
    }
}

fn assert_no_fixed_roots(heap: &LuaHeap) {
    assert_eq!(
        heap.stats().root_slots,
        0,
        "the handoff opened a root frame"
    );
    assert_eq!(heap.stats().live_roots, 0);
    assert_eq!(heap.current_frame(), None);
    assert_eq!(heap.scan.parked_root_writes, 0);
}

fn assert_frame_untouched(parked: &ParkedMark<'_>, graph: &Framed) {
    assert_eq!(parked.stats().root_slots, 4);
    assert_eq!(parked.stats().live_roots, 2);
    assert_eq!(parked.meter().parked_root_writes, 0);
    assert_eq!(
        parked.root_table(graph.frame, 0).expect("later slot"),
        Some(graph.later)
    );
    assert_eq!(parked.root_table(graph.frame, 1).expect("empty slot"), None);
    assert_eq!(
        parked.root_table(graph.frame, 2).expect("tail slot"),
        Some(graph.tail)
    );
    assert_eq!(parked.root_table(graph.frame, 3).expect("empty tail"), None);
}

fn park_at_reset_tail(heap: &mut LuaHeap) -> ParkedMark<'_> {
    let mut parked = ParkedMark::start(heap).expect("start");
    let slots = u64::from(parked.stats().node_slots);
    assert!(slots > 0, "reset tail needs existing nodes");
    assert_eq!(parked.stats().root_slots, 0);
    for _ in 0..HARNESS_BOUND {
        if parked.phase_name() == "reset" && parked.meter().weak_mode_slot_checks == slots {
            assert!(!parked.mutation_open());
            assert_eq!(parked.meter().mark_passes, 1);
            assert_eq!(parked.meter().root_slot_visits, 0);
            assert_eq!(parked.meter().parked_root_batch_accepts, 0);
            return parked;
        }
        let view = parked.pulse(1);
        assert!(
            !view.finished,
            "ordinary pulse finished before reset tail, phase {}",
            parked.phase_name()
        );
        assert!(
            !parked.mutation_open(),
            "mutation opened before reset tail, phase {}",
            parked.phase_name()
        );
    }
    panic!(
        "reset tail was not observed, phase {} checks {}",
        parked.phase_name(),
        parked.meter().weak_mode_slot_checks
    );
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

fn pause_at_gray(parked: &mut ParkedMark<'_>, owner: TableRef) {
    let cursor = parked.field_index(owner, KEY_CHILD).expect("child edge");
    for _ in 0..HARNESS_BOUND {
        let view = parked.pulse(1);
        assert!(!view.finished, "mark finished before the pending edge");
        if let Some(pending) = parked.pending()
            && pending.owner == owner.slot()
            && pending.cursor == cursor
        {
            assert_eq!(parked.phase_name(), "gray");
            return;
        }
    }
    panic!("pending child edge was not observed");
}

fn finish_ordinary(parked: &mut ParkedMark<'_>) -> ScanMeter {
    for _ in 0..HARNESS_BOUND {
        let view = parked.pulse(1);
        if view.finished {
            return view.meter;
        }
    }
    panic!(
        "ordinary pulse did not finish, phase {}",
        parked.phase_name()
    );
}

fn sweep(parked: &mut ParkedMark<'_>) {
    parked.complete().expect("sweep");
}

#[test]
fn ordinary_pulse_from_reset_tail_reaches_done_with_no_root_slots() {
    let mut heap = LuaHeap::new(limits(8, 4, 8));
    let graph = chain(&mut heap);
    let target_slot = graph.target.slot();
    let target_generation = graph.target.generation();
    let mut parked = park_at_reset_tail(&mut heap);
    let before = parked.meter();
    let view = parked.pulse(1);
    assert!(
        view.finished,
        "a positive pulse from the reset tail did not cross the empty roots window, phase {}",
        parked.phase_name()
    );
    assert_eq!(view.meter.mark_passes, 1);
    assert_eq!(view.meter.root_slot_visits, before.root_slot_visits);
    assert_eq!(view.meter.parked_root_batch_accepts, 0);
    assert_eq!(view.meter.parked_barrier_marks, 0);
    sweep(&mut parked);
    drop(parked);
    assert!(
        !heap.alive(graph.target) && !heap.alive(graph.successor) && !heap.alive(graph.orphan),
        "an unrooted pulse kept a node"
    );
    assert_eq!(graph.target.slot(), target_slot);
    assert_eq!(graph.target.generation(), target_generation);
    assert_no_fixed_roots(&heap);
}

#[test]
fn reset_tail_handoff_keeps_target_and_strong_successor() {
    let mut heap = LuaHeap::new(limits(8, 1, 8));
    let graph = chain(&mut heap);
    let target_slot = graph.target.slot();
    let target_generation = graph.target.generation();
    let successor_slot = graph.successor.slot();
    let successor_generation = graph.successor.generation();
    let mut parked = park_at_reset_tail(&mut heap);
    let before = parked.meter();
    let view = parked
        .pulse_with_root_batch(u64::from(HARNESS_BOUND), &[Rooted::Table(graph.target)])
        .expect("handoff");
    assert!(
        view.finished,
        "the handoff budget did not finish after entering roots, phase {}",
        parked.phase_name()
    );
    assert_eq!(view.meter.mark_passes, 1);
    let marked = view.meter;
    sweep(&mut parked);
    let swept_roots = parked.meter().root_slot_visits;
    let swept_accepts = parked.meter().parked_root_batch_accepts;
    drop(parked);

    assert!(
        heap.alive(graph.target) && heap.alive(graph.successor),
        "handoff returned success but collected the target and its successor; target {} successor {} accepts {}",
        heap.alive(graph.target),
        heap.alive(graph.successor),
        heap.scan.parked_root_batch_accepts
    );
    assert!(
        !heap.alive(graph.orphan),
        "the independent orphan survived the same sweep"
    );
    assert_eq!(graph.target.slot(), target_slot);
    assert_eq!(graph.target.generation(), target_generation);
    assert_eq!(graph.successor.slot(), successor_slot);
    assert_eq!(graph.successor.generation(), successor_generation);
    assert_eq!(
        heap.edge(graph.target, KEY_NEXT).expect("successor edge"),
        Some(graph.successor),
        "the surviving target lost its original successor"
    );
    assert_eq!(marked.mark_passes, before.mark_passes);
    assert_eq!(marked.parked_root_batch_accepts, 1);
    assert_eq!(marked.parked_barrier_marks, 1);
    assert_eq!(marked.scratch.work_pushed, before.scratch.work_pushed + 2);
    assert_eq!(
        marked.root_slot_visits, before.root_slot_visits,
        "the batch was charged as a root-slot scan"
    );
    assert_eq!(
        swept_roots, marked.root_slot_visits,
        "sweep root visits were attributed to the batch"
    );
    assert_eq!(swept_accepts, marked.parked_root_batch_accepts);
    assert_no_fixed_roots(&heap);

    heap.collect();
    assert!(
        !heap.alive(graph.target) && !heap.alive(graph.successor),
        "the next cycle retained a batch or a hidden root frame"
    );
    assert_no_fixed_roots(&heap);
}

#[test]
fn omitted_target_is_collected_when_the_batch_names_another() {
    let mut heap = LuaHeap::new(limits(8, 1, 8));
    let graph = chain(&mut heap);
    let mut parked = park_at_reset_tail(&mut heap);
    let view = parked
        .pulse_with_root_batch(u64::from(HARNESS_BOUND), &[Rooted::Table(graph.other)])
        .expect("other handoff");
    assert!(view.finished);
    assert_eq!(view.meter.mark_passes, 1);
    assert_eq!(view.meter.parked_root_batch_accepts, 1);
    sweep(&mut parked);
    drop(parked);
    assert!(
        heap.alive(graph.other) && heap.alive(graph.other_child),
        "the named batch root was collected; other {} child {}",
        heap.alive(graph.other),
        heap.alive(graph.other_child)
    );
    assert!(
        !heap.alive(graph.target) && !heap.alive(graph.successor),
        "omitting the target still kept it; target {} successor {}",
        heap.alive(graph.target),
        heap.alive(graph.successor)
    );
    assert!(!heap.alive(graph.orphan));
    assert_eq!(
        heap.edge(graph.other, KEY_CHILD).expect("other edge"),
        Some(graph.other_child)
    );
    assert_no_fixed_roots(&heap);
}

#[test]
fn exhausted_window_does_not_publish_and_the_next_batch_is_the_one_kept() {
    let mut heap = LuaHeap::new(limits(8, 2, 8));
    let graph = chain(&mut heap);
    let mut parked = ParkedMark::start(&mut heap).expect("start");
    let slots = u64::from(parked.stats().node_slots);
    let mut saw_closed = false;
    for _ in 0..HARNESS_BOUND {
        if parked.phase_name() == "reset" && parked.meter().weak_mode_slot_checks == slots {
            break;
        }
        saw_closed = true;
        let before = parked.meter();
        let view = parked
            .pulse_with_root_batch(1, &[Rooted::Table(graph.other)])
            .expect("exhausted window");
        assert!(!view.finished, "budget 1 finished before roots");
        assert!(!parked.mutation_open(), "phase {}", parked.phase_name());
        assert_eq!(
            view.meter.parked_root_batch_accepts,
            before.parked_root_batch_accepts
        );
        assert_eq!(view.meter.parked_barrier_marks, before.parked_barrier_marks);
        assert_eq!(view.meter.mark_passes, 1);
        assert!(!parked.marked(graph.other), "exhausted window marked A");
        assert!(!parked.marked(graph.target), "exhausted window marked B");
    }
    assert!(saw_closed, "no closed window was observed");
    assert_eq!(parked.phase_name(), "reset");
    assert_eq!(parked.meter().parked_root_batch_accepts, 0);
    parked.park().expect("park the unpublished window");
    let mut parked = ParkedMark::resume(&mut heap).expect("resume");
    assert_eq!(parked.phase_name(), "reset");
    assert_eq!(parked.meter().mark_passes, 1);
    assert_eq!(parked.meter().parked_root_batch_accepts, 0);
    assert!(!parked.marked(graph.other));
    let before = parked.meter();
    let entered = parked
        .pulse_with_root_batch(1, &[Rooted::Table(graph.target)])
        .expect("roots window");
    assert!(
        !entered.finished,
        "the entering call finished before the commit could be sampled"
    );
    assert_eq!(entered.meter.mark_passes, 1);
    assert_eq!(entered.meter.parked_root_batch_accepts, 1);
    assert_eq!(entered.meter.parked_barrier_marks, 1);
    assert_eq!(
        entered.meter.node_slot_visits,
        before.node_slot_visits + 2,
        "precheck or a second enqueue changed the commit visits"
    );
    assert_eq!(entered.meter.root_slot_visits, before.root_slot_visits);
    assert!(parked.mutation_open());
    assert_eq!(parked.phase_name(), "gray");
    let marked = finish_ordinary(&mut parked);
    assert_eq!(marked.mark_passes, 1);
    assert_eq!(marked.parked_root_batch_accepts, 1);
    sweep(&mut parked);
    drop(parked);
    assert!(
        heap.alive(graph.target) && heap.alive(graph.successor),
        "the batch passed at roots was not the one kept; target {} successor {}",
        heap.alive(graph.target),
        heap.alive(graph.successor)
    );
    assert!(
        !heap.alive(graph.other) && !heap.alive(graph.other_child),
        "the exhausted batch was published later; other {} child {}",
        heap.alive(graph.other),
        heap.alive(graph.other_child)
    );
    assert!(!heap.alive(graph.orphan));
    assert_eq!(
        heap.edge(graph.target, KEY_NEXT).expect("B edge"),
        Some(graph.successor)
    );
    assert_no_fixed_roots(&heap);
}

#[test]
fn open_roots_handoff_keeps_the_batch_and_leaves_the_frame() {
    let mut heap = LuaHeap::new(limits(12, 8, 12));
    let graph = framed(&mut heap);
    let mut parked = ParkedMark::start(&mut heap).expect("start");
    pause_after_root_visits(&mut parked, 2);
    assert_eq!(parked.phase_name(), "roots");
    assert_frame_untouched(&parked, &graph);
    let before = parked.meter();
    let view = parked
        .pulse_with_root_batch(u64::from(HARNESS_BOUND), &[Rooted::Table(graph.target)])
        .expect("open roots handoff");
    assert!(view.finished, "phase {}", parked.phase_name());
    assert_eq!(view.meter.mark_passes, 1);
    assert_eq!(view.meter.parked_root_batch_accepts, 1);
    assert_eq!(view.meter.parked_barrier_marks, 1);
    assert_eq!(
        view.meter.root_slot_visits,
        u64::from(parked.stats().root_slots),
        "batch commit changed the root-slot scan"
    );
    assert!(view.meter.root_slot_visits > before.root_slot_visits);
    assert_frame_untouched(&parked, &graph);
    let marked_roots = view.meter.root_slot_visits;
    sweep(&mut parked);
    assert_eq!(
        parked.meter().root_slot_visits,
        marked_roots + u64::from(parked.stats().root_slots),
        "sweep root visits were folded into the batch"
    );
    assert_eq!(parked.meter().parked_root_batch_accepts, 1);
    assert_frame_untouched(&parked, &graph);
    drop(parked);
    assert!(heap.alive(graph.target) && heap.alive(graph.successor));
    assert!(heap.alive(graph.child), "the fixed root was dropped");
    assert!(!heap.alive(graph.orphan));
    assert_eq!(
        heap.edge(graph.target, KEY_NEXT).expect("successor"),
        Some(graph.successor)
    );
    assert_eq!(heap.stats().root_slots, 4);
    assert_eq!(heap.current_frame(), Some(graph.frame));
    assert_eq!(heap.scan.parked_root_writes, 0);
}

#[test]
fn open_gray_handoff_keeps_the_batch_and_leaves_the_frame() {
    let mut heap = LuaHeap::new(limits(12, 8, 12));
    let graph = framed(&mut heap);
    let mut parked = ParkedMark::start(&mut heap).expect("start");
    pause_at_gray(&mut parked, graph.later);
    let pending = parked.pending().expect("gray cursor");
    let before = parked.meter();
    assert_frame_untouched(&parked, &graph);
    let view = parked
        .pulse_with_root_batch(u64::from(HARNESS_BOUND), &[Rooted::Table(graph.target)])
        .expect("open gray handoff");
    assert!(view.finished, "phase {}", parked.phase_name());
    assert_eq!(view.meter.mark_passes, 1);
    assert_eq!(view.meter.parked_root_batch_accepts, 1);
    assert_eq!(view.meter.parked_barrier_marks, 1);
    assert_eq!(
        view.meter.root_slot_visits, before.root_slot_visits,
        "the gray handoff scanned root slots"
    );
    assert_frame_untouched(&parked, &graph);
    let marked_roots = view.meter.root_slot_visits;
    sweep(&mut parked);
    assert_eq!(
        parked.meter().root_slot_visits,
        marked_roots + u64::from(parked.stats().root_slots)
    );
    assert_eq!(parked.meter().parked_root_batch_accepts, 1);
    assert_eq!(
        parked.root_table(graph.frame, 0).expect("later"),
        Some(graph.later)
    );
    drop(parked);
    assert!(
        heap.alive(graph.target) && heap.alive(graph.successor),
        "gray handoff returned success but collected the batch; target {} successor {}",
        heap.alive(graph.target),
        heap.alive(graph.successor)
    );
    assert!(heap.alive(graph.child));
    assert!(!heap.alive(graph.orphan));
    assert_eq!(pending.owner, graph.later.slot());
    assert_eq!(heap.stats().root_slots, 4);
    assert_eq!(heap.current_frame(), Some(graph.frame));
}

#[test]
fn zero_budget_does_not_validate_or_publish() {
    let mut heap = LuaHeap::new(limits(8, 4, 8));
    let graph = chain(&mut heap);
    let stale = heap.alloc().expect("stale");
    heap.rollback(stale).expect("rollback");
    let mut parked = ParkedMark::start(&mut heap).expect("start");
    let before = parked.meter();
    let stats = parked.stats();
    let view = parked
        .pulse_with_root_batch(0, &[Rooted::Table(graph.target), Rooted::Table(stale)])
        .expect("zero budget observes an invalid batch");
    assert!(!view.finished);
    assert_eq!(view.meter, before);
    assert_eq!(parked.meter(), before);
    assert_eq!(parked.phase_name(), "start");
    assert_eq!(parked.stats(), stats);
    assert!(!parked.marked(graph.target));
    drop(parked);

    let mut framed_heap = LuaHeap::new(limits(12, 8, 12));
    let framed = framed(&mut framed_heap);
    let mut open = ParkedMark::start(&mut framed_heap).expect("start");
    pause_after_root_visits(&mut open, 2);
    let open_before = open.meter();
    let open_phase = open.phase_name();
    let observed = open
        .pulse_with_root_batch(0, &[Rooted::Table(framed.target)])
        .expect("zero budget on open roots");
    assert!(!observed.finished);
    assert_eq!(observed.meter, open_before);
    assert_eq!(open.phase_name(), open_phase);
    assert_eq!(open.meter().parked_root_batch_accepts, 0);
    assert_frame_untouched(&open, &framed);
    assert!(!open.marked(framed.target));
}

#[test]
fn finished_positive_budget_is_refused_before_a_scan() {
    let mut heap = LuaHeap::new(limits(8, 4, 8));
    let graph = chain(&mut heap);
    let stale = heap.alloc().expect("stale");
    heap.rollback(stale).expect("rollback");
    let mut parked = ParkedMark::start(&mut heap).expect("start");
    let finished = parked.pulse(u64::from(HARNESS_BOUND));
    assert!(finished.finished);
    let before = parked.meter();
    let err = parked
        .pulse_with_root_batch(1, &[Rooted::Table(graph.target)])
        .err()
        .expect("finished positive budget");
    assert_eq!(err, HeapError::MarkDenied);
    assert_eq!(parked.meter(), before);
    assert_eq!(parked.phase_name(), "done");
    let stale_err = parked
        .pulse_with_root_batch(1, &[Rooted::Table(stale)])
        .err()
        .expect("finished invalid batch");
    assert_eq!(
        stale_err,
        HeapError::MarkDenied,
        "done was validated instead of refused"
    );
    assert_eq!(parked.meter(), before);
    assert_eq!(parked.phase_name(), "done");
}

#[test]
fn invalid_tail_is_refused_before_the_mark_advances() {
    refuse_before_advance(InvalidTail::Stale, HeapError::StaleHandle);
    refuse_before_advance(InvalidTail::Foreign, HeapError::WrongDomain);
    refuse_before_advance(InvalidTail::WrongKind, HeapError::WrongKind);
}

enum InvalidTail {
    Stale,
    Foreign,
    WrongKind,
}

fn refuse_before_advance(kind: InvalidTail, expected: HeapError) {
    let mut heap = LuaHeap::new(limits(8, 4, 8));
    let graph = chain(&mut heap);
    let tail = match kind {
        InvalidTail::Stale => {
            let stale = heap.alloc().expect("stale");
            heap.rollback(stale).expect("rollback");
            Rooted::Table(stale)
        }
        InvalidTail::Foreign => {
            let mut other = LuaHeap::new(limits(2, 1, 1));
            let foreign = other.alloc().expect("foreign");
            Rooted::Table(foreign)
        }
        InvalidTail::WrongKind => {
            let closure = heap.alloc_closure().expect("closure");
            let wrong = TableRef::table(closure.domain(), closure.slot(), closure.generation());
            Rooted::Table(wrong)
        }
    };
    let mut parked = ParkedMark::start(&mut heap).expect("start");
    let before = parked.meter();
    let stats = parked.stats();
    let err = parked
        .pulse_with_root_batch(1, &[Rooted::Table(graph.target), tail])
        .err()
        .expect("invalid tail");
    assert_eq!(err, expected);
    assert_eq!(parked.phase_name(), "start");
    assert_eq!(parked.meter(), before, "refusal advanced or scanned");
    assert_eq!(parked.meter().mark_passes, 0);
    assert_eq!(parked.stats(), stats);
    assert!(!parked.marked(graph.target));
    let marked = finish_ordinary(&mut parked);
    assert_eq!(marked.parked_root_batch_accepts, 0);
    assert_eq!(marked.parked_barrier_marks, 0);
    sweep(&mut parked);
    drop(parked);
    assert!(
        !heap.alive(graph.target) && !heap.alive(graph.successor),
        "the refused leading entry was marked"
    );
    assert_no_fixed_roots(&heap);
}

#[test]
fn over_quota_is_refused_before_the_mark_advances() {
    let mut heap = LuaHeap::new(limits(8, 0, 8));
    let graph = chain(&mut heap);
    let mut parked = ParkedMark::start(&mut heap).expect("start");
    let before = parked.meter();
    let err = parked
        .pulse_with_root_batch(1, &[Rooted::Table(graph.target)])
        .err()
        .expect("zero root quota");
    assert_eq!(err, HeapError::RootCapacity);
    assert_eq!(parked.phase_name(), "start");
    assert_eq!(parked.meter(), before);
    assert_eq!(parked.meter().mark_passes, 0);
    assert!(!parked.marked(graph.target));
    assert_eq!(parked.stats().root_slots, 0);
}

#[test]
fn empty_batch_matches_an_ordinary_pulse() {
    let from_start = |batch: bool| {
        let mut heap = LuaHeap::new(limits(8, 4, 8));
        let _graph = chain(&mut heap);
        let mut parked = ParkedMark::start(&mut heap).expect("start");
        let view = if batch {
            parked.pulse_with_root_batch(1, &[]).expect("empty")
        } else {
            parked.pulse(1)
        };
        (
            parked.phase_name().to_string(),
            view.finished,
            view.meter,
            parked.stats(),
        )
    };
    let ordinary = from_start(false);
    let empty = from_start(true);
    assert_eq!(empty.0, ordinary.0);
    assert_eq!(empty.1, ordinary.1);
    assert_eq!(empty.2, ordinary.2);
    assert_eq!(empty.3, ordinary.3);
    assert_eq!(empty.2.parked_root_batch_accepts, 0);
    assert!(!empty.1);

    let from_tail = |batch: bool| {
        let mut heap = LuaHeap::new(limits(8, 4, 8));
        let graph = chain(&mut heap);
        let mut parked = park_at_reset_tail(&mut heap);
        let view = if batch {
            parked.pulse_with_root_batch(1, &[]).expect("empty tail")
        } else {
            parked.pulse(1)
        };
        assert!(view.finished);
        sweep(&mut parked);
        drop(parked);
        (
            view.meter,
            heap.alive(graph.target),
            heap.stats().root_slots,
        )
    };
    let ordinary_tail = from_tail(false);
    let empty_tail = from_tail(true);
    assert_eq!(empty_tail.0, ordinary_tail.0);
    assert_eq!(empty_tail.1, ordinary_tail.1);
    assert!(!empty_tail.1);
    assert_eq!(empty_tail.2, 0);
}

#[test]
fn mode_phases_hold_the_batch_until_roots() {
    let mut heap = LuaHeap::new(limits(12, 2, 12));
    let graph = chain_with_mode(&mut heap);
    let mut parked = ParkedMark::start(&mut heap).expect("start");
    let slots = u64::from(parked.stats().node_slots);
    let mut saw_mode = false;
    for _ in 0..HARNESS_BOUND {
        let phase = parked.phase_name();
        if phase.starts_with("mode") {
            saw_mode = true;
            assert!(!parked.mutation_open());
            assert_eq!(parked.meter().parked_root_batch_accepts, 0);
        }
        if phase == "reset" && parked.meter().weak_mode_slot_checks == slots {
            break;
        }
        let before = parked.meter();
        let view = parked
            .pulse_with_root_batch(1, &[Rooted::Table(graph.other)])
            .expect("mode window");
        assert!(!view.finished, "phase {}", parked.phase_name());
        assert_eq!(
            view.meter.parked_root_batch_accepts,
            before.parked_root_batch_accepts
        );
        assert!(!parked.mutation_open(), "phase {}", parked.phase_name());
        assert_eq!(view.meter.mark_passes, 1);
    }
    assert!(saw_mode, "mode phases were not observed");
    assert_eq!(parked.phase_name(), "reset");
    assert_eq!(parked.meter().parked_root_batch_accepts, 0);
    assert!(!parked.marked(graph.other));
    let view = parked
        .pulse_with_root_batch(u64::from(HARNESS_BOUND), &[Rooted::Table(graph.target)])
        .expect("roots after mode");
    assert!(view.finished, "phase {}", parked.phase_name());
    assert_eq!(view.meter.mark_passes, 1);
    assert_eq!(view.meter.parked_root_batch_accepts, 1);
    assert_eq!(view.meter.parked_barrier_marks, 1);
    sweep(&mut parked);
    drop(parked);
    assert!(heap.alive(graph.target) && heap.alive(graph.successor));
    assert!(!heap.alive(graph.other) && !heap.alive(graph.orphan));
    assert_eq!(
        heap.edge(graph.target, KEY_NEXT).expect("successor"),
        Some(graph.successor)
    );
    assert_no_fixed_roots(&heap);
}

#[test]
fn closure_batch_keeps_the_cell_identity_without_a_root_frame() {
    let mut heap = LuaHeap::new(limits(8, 1, 8));
    let closure = heap.alloc_closure().expect("closure");
    let cell = heap.alloc_upvalue().expect("cell");
    let held = heap.alloc().expect("held");
    let orphan = heap.alloc().expect("orphan");
    heap.set_upvalue_edge(closure, cell).expect("cell edge");
    heap.write_upvalue(cell, held).expect("current");
    let closure_slot = closure.slot();
    let cell_slot = cell.slot();
    let mut parked = park_at_reset_tail(&mut heap);
    let view = parked
        .pulse_with_root_batch(u64::from(HARNESS_BOUND), &[Rooted::Closure(closure)])
        .expect("closure handoff");
    assert!(view.finished);
    assert_eq!(view.meter.mark_passes, 1);
    assert_eq!(view.meter.parked_root_batch_accepts, 1);
    assert_eq!(view.meter.root_slot_visits, 0);
    sweep(&mut parked);
    drop(parked);
    assert!(
        heap.closure_alive(closure) && heap.upvalue_alive(cell) && heap.alive(held),
        "closure handoff dropped the chain"
    );
    assert!(!heap.alive(orphan));
    assert_eq!(closure.slot(), closure_slot);
    assert_eq!(cell.slot(), cell_slot);
    assert_eq!(heap.read_upvalue(closure).expect("current"), held);
    assert_no_fixed_roots(&heap);
}
