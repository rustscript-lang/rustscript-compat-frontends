//! Safepoint root batch on a resumed private mark.
//!
//! The batch is borrowed for one exclusive window. It does not write a root
//! slot or open a frame. A weak observer root is the phase gate; it must not
//! be what keeps a batch target alive. Precheck slot reads are not scan visits.
//! Visit and accept meters are sampled around the commit, before sweep.

use super::{
    ClosureRef, HeapError, HeapLimits, LuaHeap, ParkedMark, RootFrameId, Rooted, ScanMeter,
    TableRef, UpvalueRef,
};
use vm::Value;

const KEY_CHILD: u32 = 1;
const KEY_TARGET: u32 = 4;
const KEY_ORPHAN: u32 = 5;
const KEY_NEXT: u32 = 6;
const KEY_CLOSURE: u32 = 7;
const HARNESS_BOUND: u32 = 64;

fn limits(nodes: u32, roots: u32, edges: u32) -> HeapLimits {
    HeapLimits {
        nodes,
        roots,
        edges,
        max_generation: 8,
    }
}

struct Observed {
    frame: RootFrameId,
    later: TableRef,
    child: TableRef,
    target: TableRef,
    successor: TableRef,
    orphan: TableRef,
}

fn observed(heap: &mut LuaHeap) -> Observed {
    let later = heap.alloc().expect("later");
    let child = heap.alloc().expect("later child");
    heap.set_edge(later, KEY_CHILD, child).expect("later edge");
    let tail = heap.alloc().expect("tail");
    let mt = heap.alloc().expect("metatable");
    let watch = heap.alloc().expect("watch");
    let target = heap.alloc().expect("target");
    let successor = heap.alloc().expect("successor");
    let orphan = heap.alloc().expect("orphan");
    heap.set_edge(target, KEY_NEXT, successor)
        .expect("target successor");
    heap.set_mode_scalar(mt, Value::string("v\0"))
        .expect("weak values");
    heap.set_metatable(watch, Some(mt))
        .expect("watch metatable");
    heap.set_edge(watch, KEY_TARGET, target)
        .expect("weak target");
    heap.set_edge(watch, KEY_ORPHAN, orphan)
        .expect("weak orphan");
    let frame = heap.enter_roots(4).expect("observer frame");
    heap.write_root(frame, 0, later).expect("root later");
    heap.write_root(frame, 2, tail).expect("root tail");
    heap.write_root(frame, 3, watch).expect("root watch");
    Observed {
        frame,
        later,
        child,
        target,
        successor,
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

fn pause_at(parked: &mut ParkedMark<'_>, owner: u32, cursor: u32) {
    for _ in 0..HARNESS_BOUND {
        let view = parked.pulse(1);
        assert!(!view.finished, "mark finished before the pending edge");
        assert_eq!(view.meter.mark_passes, 1);
        if let Some(pending) = parked.pending()
            && pending.owner == owner
            && pending.cursor == cursor
        {
            assert_eq!(parked.phase_name(), "gray");
            return;
        }
    }
    panic!("pending record {cursor} on owner {owner} was not observed");
}

fn finish(parked: &mut ParkedMark<'_>) -> ScanMeter {
    for _ in 0..HARNESS_BOUND {
        let view = parked.pulse(1);
        assert_eq!(view.meter.mark_passes, 1);
        if view.finished {
            return view.meter;
        }
    }
    panic!("mark did not finish within the harness bound");
}

fn resume_at_roots(heap: &mut LuaHeap) -> ParkedMark<'_> {
    let mut parked = ParkedMark::start(heap).expect("start");
    pause_after_root_visits(&mut parked, 2);
    assert_eq!(parked.phase_name(), "roots");
    assert!(parked.mutation_open());
    parked.park().expect("release the exclusive window");
    let parked = ParkedMark::resume(heap).expect("resume the parked mark");
    assert_eq!(parked.phase_name(), "roots");
    parked
}

fn resume_at_gray(heap: &mut LuaHeap, later: TableRef) -> ParkedMark<'_> {
    let mut parked = ParkedMark::start(heap).expect("start");
    let cursor = parked.field_index(later, KEY_CHILD).expect("child edge");
    pause_at(&mut parked, later.slot(), cursor);
    let pending = parked.pending().expect("gray cursor");
    parked.park().expect("release the exclusive window");
    let parked = ParkedMark::resume(heap).expect("resume the parked mark");
    assert_eq!(parked.phase_name(), "gray");
    assert_eq!(
        parked.pending().map(|item| (item.owner, item.cursor)),
        Some((pending.owner, pending.cursor))
    );
    parked
}

fn assert_batch_did_not_touch_roots(
    parked: &ParkedMark<'_>,
    frame: RootFrameId,
    before: &ScanMeter,
) {
    let after = parked.meter();
    assert_eq!(after.root_slot_visits, before.root_slot_visits);
    assert_eq!(after.parked_root_writes, before.parked_root_writes);
    assert_eq!(after.mark_passes, 1);
    assert_eq!(parked.stats().root_slots, 4);
    assert_eq!(parked.stats().live_roots, 3);
    assert_eq!(
        parked.root_table(frame, 1).expect("empty observer slot"),
        None
    );
}

fn sweep_finished(parked: &mut ParkedMark<'_>) {
    parked.complete().expect("sweep");
}

fn keep_table(gray: bool) {
    let mut heap = LuaHeap::new(limits(16, 8, 16));
    let graph = observed(&mut heap);
    let mut parked = if gray {
        resume_at_gray(&mut heap, graph.later)
    } else {
        resume_at_roots(&mut heap)
    };
    assert!(
        !parked.marked(graph.target),
        "the weak observer already marked the target"
    );
    let before = parked.meter();
    let pending = parked.pending();
    let root_slots = parked.stats().root_slots;
    let live_roots = parked.stats().live_roots;
    let peak_roots = parked.stats().peak_roots;
    parked
        .accept_root_batch(&[Rooted::Table(graph.target)])
        .expect("open batch");
    let after = parked.meter();
    assert_eq!(
        after.node_slot_visits,
        before.node_slot_visits + 2,
        "follow_one charges two visits per accepted entry; sample before sweep"
    );
    assert_eq!(after.root_slot_visits, before.root_slot_visits);
    assert_eq!(
        after.parked_root_batch_accepts,
        before.parked_root_batch_accepts + 1
    );
    assert_eq!(after.parked_barrier_marks, before.parked_barrier_marks + 1);
    assert_eq!(after.scratch.work_pushed, before.scratch.work_pushed + 1);
    assert_eq!(after.parked_root_writes, before.parked_root_writes);
    assert_eq!(after.mark_passes, 1);
    assert_eq!(
        parked.pending().map(|item| (item.owner, item.cursor)),
        pending.map(|item| (item.owner, item.cursor))
    );
    assert_eq!(parked.stats().root_slots, root_slots);
    assert_eq!(parked.stats().live_roots, live_roots);
    assert_eq!(parked.stats().peak_roots, peak_roots);
    assert_eq!(
        parked.root_table(graph.frame, 1).expect("slot"),
        None,
        "the batch wrote a root slot"
    );
    if gray {
        let edges = after.outgoing_edge_checks;
        let view = parked.pulse(1);
        assert!(!view.finished);
        assert_eq!(view.meter.outgoing_edge_checks, edges + 1);
        assert_eq!(view.meter.root_slot_visits, after.root_slot_visits);
    } else {
        let visits = after.root_slot_visits;
        let view = parked.pulse(1);
        assert!(!view.finished);
        assert_eq!(view.meter.root_slot_visits, visits + 1);
        assert_eq!(parked.phase_name(), "roots");
    }
    let marked = finish(&mut parked);
    assert_eq!(marked.mark_passes, 1);
    assert_eq!(marked.parked_root_writes, 0);
    sweep_finished(&mut parked);
    drop(parked);

    assert!(
        heap.alive(graph.child) && !heap.alive(graph.orphan),
        "fixed root and weak observer were not independent; child {} orphan {}",
        heap.alive(graph.child),
        heap.alive(graph.orphan)
    );
    assert!(
        heap.alive(graph.target) && heap.alive(graph.successor),
        "resumed batch returned success but did not keep the table; target {} successor {} child {}",
        heap.alive(graph.target),
        heap.alive(graph.successor),
        heap.alive(graph.child)
    );
    assert_eq!(
        heap.edge(graph.target, KEY_NEXT).expect("successor edge"),
        Some(graph.successor)
    );
    assert_eq!(
        heap.edge(graph.later, KEY_CHILD).expect("later child"),
        Some(graph.child)
    );
    assert_eq!(heap.stats().root_slots, 4);
    assert_eq!(heap.stats().live_roots, 3);
    assert_eq!(heap.current_frame(), Some(graph.frame));
    assert_eq!(heap.scan.parked_root_batch_accepts, 1);
    assert_eq!(heap.scan.parked_barrier_marks, 1);
    assert_eq!(heap.scan.parked_root_writes, 0);

    heap.collect();
    assert!(
        !heap.alive(graph.target) && !heap.alive(graph.successor),
        "the next cycle retained a batch or frame residue"
    );
    assert!(heap.alive(graph.child), "the fixed root was dropped");
    assert_eq!(heap.stats().root_slots, 4);
    assert_eq!(heap.current_frame(), Some(graph.frame));
}

#[test]
fn roots_batch_after_resume_keeps_the_table_and_its_successor() {
    keep_table(false);
}

#[test]
fn gray_batch_after_resume_keeps_the_table_and_its_successor() {
    keep_table(true);
}

struct Chains {
    frame: RootFrameId,
    later: TableRef,
    child: TableRef,
    closure: ClosureRef,
    cell: UpvalueRef,
    held: TableRef,
    free_cell: UpvalueRef,
    free_held: TableRef,
    omitted: TableRef,
    orphan: TableRef,
}

fn chains(heap: &mut LuaHeap) -> Chains {
    let later = heap.alloc().expect("later");
    let child = heap.alloc().expect("child");
    heap.set_edge(later, KEY_CHILD, child).expect("later edge");
    let tail = heap.alloc().expect("tail");
    let mt = heap.alloc().expect("metatable");
    let watch = heap.alloc().expect("watch");
    let closure = heap.alloc_closure().expect("closure");
    let cell = heap.alloc_upvalue().expect("closure cell");
    let held = heap.alloc().expect("closure table");
    let free_cell = heap.alloc_upvalue().expect("independent cell");
    let free_held = heap.alloc().expect("independent table");
    let omitted = heap.alloc().expect("omitted table");
    let orphan = heap.alloc().expect("orphan");
    heap.set_upvalue_edge(closure, cell).expect("closure cell");
    heap.write_upvalue(cell, held).expect("cell current");
    heap.write_upvalue(free_cell, free_held)
        .expect("independent current");
    heap.set_mode_scalar(mt, Value::string("v\0"))
        .expect("weak values");
    heap.set_metatable(watch, Some(mt))
        .expect("watch metatable");
    heap.set_closure_edge(watch, KEY_CLOSURE, closure)
        .expect("weak closure");
    heap.set_cell_edge(watch, free_cell)
        .expect("weak independent cell");
    heap.set_edge(watch, KEY_TARGET, omitted)
        .expect("weak omitted");
    heap.set_edge(watch, KEY_ORPHAN, orphan)
        .expect("weak orphan");
    let frame = heap.enter_roots(4).expect("observer frame");
    heap.write_root(frame, 0, later).expect("root later");
    heap.write_root(frame, 2, tail).expect("root tail");
    heap.write_root(frame, 3, watch).expect("root watch");
    Chains {
        frame,
        later,
        child,
        closure,
        cell,
        held,
        free_cell,
        free_held,
        omitted,
        orphan,
    }
}

fn assert_chain_identity(heap: &LuaHeap, graph: &Chains, free_kept: bool) {
    assert!(
        heap.alive(graph.child) && !heap.alive(graph.orphan) && !heap.alive(graph.omitted),
        "roots shadowed the batch; child {} orphan {} omitted {}",
        heap.alive(graph.child),
        heap.alive(graph.orphan),
        heap.alive(graph.omitted)
    );
    assert!(
        heap.closure_alive(graph.closure)
            && heap.upvalue_alive(graph.cell)
            && heap.alive(graph.held),
        "closure chain was not kept; closure {} cell {} table {}",
        heap.closure_alive(graph.closure),
        heap.upvalue_alive(graph.cell),
        heap.alive(graph.held)
    );
    assert_eq!(
        heap.read_upvalue(graph.closure).expect("closure current"),
        graph.held
    );
    assert_eq!(heap.upvalue_alive(graph.free_cell), free_kept);
    assert_eq!(heap.alive(graph.free_held), free_kept);
    if free_kept {
        assert_eq!(
            heap.current_table(graph.free_cell)
                .expect("independent current"),
            Some(graph.free_held)
        );
    }
    assert_eq!(heap.stats().root_slots, 4);
    assert_eq!(heap.current_frame(), Some(graph.frame));
    assert_eq!(heap.scan.parked_root_writes, 0);
}

fn keep_chains(gray: bool, include_free: bool) {
    let mut heap = LuaHeap::new(limits(24, 8, 24));
    let graph = chains(&mut heap);
    let mut parked = if gray {
        resume_at_gray(&mut heap, graph.later)
    } else {
        resume_at_roots(&mut heap)
    };
    assert!(
        !parked.marked(graph.omitted),
        "observer marked the omitted table"
    );
    let before = parked.meter();
    let mut batch = vec![Rooted::Closure(graph.closure)];
    if include_free {
        batch.push(Rooted::Upvalue(graph.free_cell));
    }
    parked.accept_root_batch(&batch).expect("chain batch");
    let after = parked.meter();
    let entries = batch.len() as u64;
    assert_eq!(
        after.node_slot_visits,
        before.node_slot_visits + 2 * entries
    );
    assert_eq!(after.root_slot_visits, before.root_slot_visits);
    assert_eq!(after.parked_root_batch_accepts, entries);
    assert_eq!(after.parked_barrier_marks, entries);
    assert_eq!(
        after.scratch.work_pushed,
        before.scratch.work_pushed + entries
    );
    assert_eq!(after.parked_root_writes, 0);
    assert_batch_did_not_touch_roots(&parked, graph.frame, &before);
    finish(&mut parked);
    sweep_finished(&mut parked);
    drop(parked);
    assert_chain_identity(&heap, &graph, include_free);
}

#[test]
fn roots_batch_keeps_closure_and_upvalue_identity() {
    keep_chains(false, true);
}

#[test]
fn gray_batch_keeps_closure_and_upvalue_identity() {
    keep_chains(true, true);
}

#[test]
fn omitted_target_is_collected_while_the_named_closure_lives() {
    keep_chains(false, false);
}

#[test]
fn duplicate_entry_enqueues_once() {
    let mut once_heap = LuaHeap::new(limits(16, 8, 16));
    let once_graph = observed(&mut once_heap);
    let mut once = resume_at_roots(&mut once_heap);
    let once_before = once.meter();
    once.accept_root_batch(&[Rooted::Table(once_graph.target)])
        .expect("single");
    assert_eq!(
        once.meter().node_slot_visits,
        once_before.node_slot_visits + 2
    );
    assert_eq!(once.meter().parked_barrier_marks, 1);
    assert_eq!(once.meter().parked_root_batch_accepts, 1);
    let once_mark = finish(&mut once);

    let mut twice_heap = LuaHeap::new(limits(16, 8, 16));
    let twice_graph = observed(&mut twice_heap);
    let mut twice = resume_at_roots(&mut twice_heap);
    let twice_before = twice.meter();
    twice
        .accept_root_batch(&[
            Rooted::Table(twice_graph.target),
            Rooted::Table(twice_graph.target),
        ])
        .expect("duplicate");
    assert_eq!(
        twice.meter().node_slot_visits,
        twice_before.node_slot_visits + 4,
        "each duplicate still takes two follow_one visits"
    );
    assert_eq!(twice.meter().parked_barrier_marks, 1);
    assert_eq!(twice.meter().parked_root_batch_accepts, 2);
    assert_eq!(
        twice.meter().scratch.work_pushed,
        twice_before.scratch.work_pushed + 1
    );
    assert_eq!(
        twice.meter().root_slot_visits,
        twice_before.root_slot_visits
    );
    let twice_mark = finish(&mut twice);
    assert_eq!(
        twice_mark.owner_head_checks, once_mark.owner_head_checks,
        "the duplicate was pushed onto the gray queue again"
    );
    assert_eq!(
        twice_mark.outgoing_edge_checks, once_mark.outgoing_edge_checks,
        "the duplicate was scanned again"
    );
    sweep_finished(&mut twice);
    drop(twice);
    assert!(twice_heap.alive(twice_graph.target));
    assert!(twice_heap.alive(twice_graph.successor));
    assert!(!twice_heap.alive(twice_graph.orphan));
}

fn refuse_and_reclaim(
    heap: &mut LuaHeap,
    graph: &Observed,
    batch_tail: Rooted,
    expected: HeapError,
) {
    let mut parked = resume_at_gray(heap, graph.later);
    assert!(!parked.marked(graph.target), "target was already marked");
    let before = parked.meter();
    let pending = parked.pending().expect("gray cursor");
    let stats = parked.stats();
    let err = parked
        .accept_root_batch(&[Rooted::Table(graph.target), batch_tail])
        .expect_err("invalid tail");
    assert_eq!(err, expected);
    assert_eq!(
        parked.meter(),
        before,
        "precheck reads and the refused batch changed the scan meter"
    );
    assert_eq!(
        parked.pending().map(|item| (item.owner, item.cursor)),
        Some((pending.owner, pending.cursor))
    );
    assert_eq!(parked.stats(), stats);
    assert_eq!(parked.root_table(graph.frame, 1).expect("slot"), None);
    finish(&mut parked);
    sweep_finished(&mut parked);
    drop(parked);
    assert!(
        !heap.alive(graph.target) && !heap.alive(graph.successor),
        "the refused batch marked the leading entry; target {} successor {}",
        heap.alive(graph.target),
        heap.alive(graph.successor)
    );
    assert!(heap.alive(graph.child));
    assert!(!heap.alive(graph.orphan));
    assert_eq!(heap.scan.parked_root_batch_accepts, 0);
    assert_eq!(heap.scan.parked_barrier_marks, 0);
    assert_eq!(heap.scan.parked_root_writes, 0);
}

#[test]
fn refused_stale_tail_does_not_mark_the_leading_entry() {
    let mut heap = LuaHeap::new(limits(16, 8, 16));
    let graph = observed(&mut heap);
    let stale = heap.alloc().expect("stale");
    heap.rollback(stale).expect("rollback");
    refuse_and_reclaim(
        &mut heap,
        &graph,
        Rooted::Table(stale),
        HeapError::StaleHandle,
    );
}

#[test]
fn refused_foreign_tail_does_not_mark_the_leading_entry() {
    let mut other = LuaHeap::new(limits(2, 1, 1));
    let foreign = other.alloc().expect("foreign");
    let mut heap = LuaHeap::new(limits(16, 8, 16));
    let graph = observed(&mut heap);
    refuse_and_reclaim(
        &mut heap,
        &graph,
        Rooted::Table(foreign),
        HeapError::WrongDomain,
    );
}

#[test]
fn refused_wrong_kind_tail_does_not_mark_the_leading_entry() {
    let mut heap = LuaHeap::new(limits(16, 8, 16));
    let graph = observed(&mut heap);
    let closure = heap.alloc_closure().expect("live closure");
    // Existing constructor. Production does not build an unverified handle.
    let wrong_kind = TableRef::table(closure.domain(), closure.slot(), closure.generation());
    refuse_and_reclaim(
        &mut heap,
        &graph,
        Rooted::Table(wrong_kind),
        HeapError::WrongKind,
    );
}

struct Quota {
    frame: RootFrameId,
    target: TableRef,
    successor: TableRef,
    child: TableRef,
}

fn quota_graph(heap: &mut LuaHeap) -> Quota {
    let later = heap.alloc().expect("later");
    let child = heap.alloc().expect("child");
    heap.set_edge(later, KEY_CHILD, child).expect("later edge");
    let mt = heap.alloc().expect("metatable");
    let watch = heap.alloc().expect("watch");
    let target = heap.alloc().expect("target");
    let successor = heap.alloc().expect("successor");
    let orphan = heap.alloc().expect("orphan");
    heap.set_edge(target, KEY_NEXT, successor)
        .expect("successor");
    heap.set_mode_scalar(mt, Value::string("v\0"))
        .expect("mode");
    heap.set_metatable(watch, Some(mt)).expect("metatable");
    heap.set_edge(watch, KEY_TARGET, target)
        .expect("weak target");
    heap.set_edge(watch, KEY_ORPHAN, orphan)
        .expect("weak orphan");
    let frame = heap.enter_roots(3).expect("frame");
    heap.write_root(frame, 0, later).expect("root later");
    heap.write_root(frame, 2, watch).expect("root watch");
    Quota {
        frame,
        target,
        successor,
        child,
    }
}

fn resume_quota(heap: &mut LuaHeap) -> ParkedMark<'_> {
    let mut parked = ParkedMark::start(heap).expect("start");
    pause_after_root_visits(&mut parked, 2);
    assert_eq!(parked.phase_name(), "roots");
    assert_eq!(parked.stats().root_slots, 3);
    assert_eq!(
        parked.stats().live_roots,
        2,
        "the empty reserved slot counts"
    );
    parked.park().expect("park");
    ParkedMark::resume(heap).expect("resume")
}

#[test]
fn full_root_quota_accepts_and_keeps_the_target() {
    let mut heap = LuaHeap::new(limits(12, 4, 12));
    let graph = quota_graph(&mut heap);
    let mut parked = resume_quota(&mut heap);
    parked
        .accept_root_batch(&[Rooted::Table(graph.target)])
        .expect("3 reserved slots + 1 entry = 4");
    assert_eq!(parked.meter().parked_root_batch_accepts, 1);
    assert_eq!(parked.stats().root_slots, 3);
    finish(&mut parked);
    sweep_finished(&mut parked);
    drop(parked);
    assert!(heap.alive(graph.target) && heap.alive(graph.successor));
    assert!(heap.alive(graph.child));
    assert_eq!(heap.stats().root_slots, 3);
    assert_eq!(heap.current_frame(), Some(graph.frame));
}

#[test]
fn quota_counts_empty_slots_and_duplicate_entries() {
    let mut heap = LuaHeap::new(limits(12, 3, 12));
    let graph = quota_graph(&mut heap);
    let mut parked = resume_quota(&mut heap);
    let before = parked.meter();
    let err = parked
        .accept_root_batch(&[Rooted::Table(graph.target)])
        .expect_err("3 reserved slots + 1 entry exceeds 3");
    assert_eq!(err, HeapError::RootCapacity);
    assert_eq!(parked.meter(), before);
    finish(&mut parked);
    sweep_finished(&mut parked);
    drop(parked);
    assert!(
        !heap.alive(graph.target),
        "counting only occupied slots accepted a batch the quota rejects"
    );
    assert!(heap.alive(graph.child));

    let mut heap = LuaHeap::new(limits(12, 4, 12));
    let graph = quota_graph(&mut heap);
    let mut parked = resume_quota(&mut heap);
    let before = parked.meter();
    let err = parked
        .accept_root_batch(&[Rooted::Table(graph.target), Rooted::Table(graph.target)])
        .expect_err("duplicate entries count toward the quota");
    assert_eq!(err, HeapError::RootCapacity);
    assert_eq!(parked.meter(), before);
    finish(&mut parked);
    sweep_finished(&mut parked);
    drop(parked);
    assert!(
        !heap.alive(graph.target) && !heap.alive(graph.successor),
        "deduplicating the batch slipped past the quota and kept the target"
    );
    assert!(heap.alive(graph.child));

    let mut heap = LuaHeap::new(limits(12, 5, 12));
    let graph = quota_graph(&mut heap);
    let mut parked = resume_quota(&mut heap);
    let before = parked.meter();
    parked
        .accept_root_batch(&[Rooted::Table(graph.target), Rooted::Table(graph.target)])
        .expect("3 reserved slots + 2 duplicate entries = 5");
    assert_eq!(parked.meter().node_slot_visits, before.node_slot_visits + 4);
    assert_eq!(parked.meter().parked_root_batch_accepts, 2);
    assert_eq!(parked.meter().parked_barrier_marks, 1);
    assert_eq!(parked.stats().root_slots, 3);
    finish(&mut parked);
    sweep_finished(&mut parked);
    drop(parked);
    assert!(heap.alive(graph.target) && heap.alive(graph.successor));
}

#[test]
fn over_limit_is_reported_before_a_stale_tail() {
    let mut heap = LuaHeap::new(limits(16, 4, 16));
    let stale = heap.alloc().expect("stale");
    heap.rollback(stale).expect("rollback");
    let graph = observed(&mut heap);
    let mut parked = resume_at_roots(&mut heap);
    let before = parked.meter();
    let err = parked
        .accept_root_batch(&[Rooted::Table(graph.target), Rooted::Table(stale)])
        .expect_err("length before handles");
    assert_eq!(err, HeapError::RootCapacity);
    assert_eq!(parked.meter(), before);
    finish(&mut parked);
    sweep_finished(&mut parked);
    drop(parked);
    assert!(!heap.alive(graph.target));
    assert!(heap.alive(graph.child));
}

#[test]
fn closed_phase_and_done_reject_the_batch() {
    let mut heap = LuaHeap::new(limits(16, 8, 16));
    let graph = observed(&mut heap);
    let mut parked = ParkedMark::start(&mut heap).expect("start");
    let before = parked.meter();
    assert_eq!(
        parked
            .accept_root_batch(&[Rooted::Table(graph.target)])
            .expect_err("start"),
        HeapError::MarkDenied
    );
    assert_eq!(parked.meter(), before);
    assert_eq!(parked.phase_name(), "start");
    let view = parked.pulse(1);
    assert_eq!(view.meter.mark_passes, 1);
    assert_ne!(parked.phase_name(), "roots");
    assert_ne!(parked.phase_name(), "gray");
    assert!(!parked.mutation_open());
    let reset = parked.meter();
    assert_eq!(
        parked
            .accept_root_batch(&[])
            .expect_err("empty batch is not a no-op while closed"),
        HeapError::MarkDenied
    );
    assert_eq!(
        parked
            .accept_root_batch(&[Rooted::Table(graph.target)])
            .expect_err("closed phase"),
        HeapError::MarkDenied
    );
    assert_eq!(parked.meter(), reset);
    finish(&mut parked);
    assert_eq!(parked.phase_name(), "done");
    let done = parked.meter();
    assert_eq!(
        parked
            .accept_root_batch(&[Rooted::Table(graph.target)])
            .expect_err("done"),
        HeapError::MarkDenied
    );
    assert_eq!(
        parked.meter(),
        done,
        "the Done refusal must not be charged for the earlier root walk"
    );
    sweep_finished(&mut parked);
    drop(parked);
    assert!(!heap.alive(graph.target) && !heap.alive(graph.successor));
    assert!(heap.alive(graph.child));
    assert_eq!(heap.scan.parked_root_batch_accepts, 0);
    assert_eq!(heap.scan.parked_barrier_marks, 0);
}

#[test]
fn empty_open_batch_is_a_noop() {
    let mut heap = LuaHeap::new(limits(16, 8, 16));
    let graph = observed(&mut heap);
    let mut parked = resume_at_roots(&mut heap);
    let before = parked.meter();
    let stats = parked.stats();
    parked.accept_root_batch(&[]).expect("empty open batch");
    assert_eq!(parked.meter(), before);
    assert_eq!(parked.stats(), stats);
    assert_eq!(parked.phase_name(), "roots");
    assert_eq!(parked.meter().node_slot_visits, before.node_slot_visits);
    assert_eq!(parked.meter().root_slot_visits, before.root_slot_visits);
    finish(&mut parked);
    sweep_finished(&mut parked);
    drop(parked);
    assert!(!heap.alive(graph.target));
    assert!(heap.alive(graph.child));
    assert_eq!(heap.stats().root_slots, 4);
    assert_eq!(heap.current_frame(), Some(graph.frame));
}
