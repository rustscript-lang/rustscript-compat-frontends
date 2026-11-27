//! Pausable mark on a fixed graph. Pulses share one pass and one heap borrow.
//!
//! Each pulse is checked against the scan meter, not against a returned
//! budget number. Sweep stays behind a finished mark.

use super::{HeapLimits, LuaHeap, MarkSession, ScanMeter};
use vm::Value;

fn limits(nodes: u32, roots: u32, edges: u32) -> HeapLimits {
    HeapLimits {
        nodes,
        roots,
        edges,
        max_generation: 8,
    }
}

fn edge_work(meter: &ScanMeter) -> u64 {
    meter.outgoing_edge_checks + meter.outgoing_gap_checks
}

/// Drive `session` with `budget` until the mark finishes. Every pulse must
/// keep edge, mode-byte, and mode-cache work inside that budget.
fn pulse_until_done(session: &mut MarkSession<'_>, budget: u64, limit: u32) -> ScanMeter {
    assert!(budget > 0, "a finishing drive needs a positive budget");
    let mut pulses = 0u32;
    loop {
        let before = session.meter();
        let view = session.pulse(budget);
        let edge_delta = edge_work(&view.meter) - edge_work(&before);
        let byte_delta = view.meter.weak_mode_bytes - before.weak_mode_bytes;
        let init_delta = view.meter.scratch.weak_modes_inited - before.scratch.weak_modes_inited;
        assert!(
            edge_delta <= budget && byte_delta <= budget && init_delta <= budget,
            "pulse {pulses} budget {budget} exceeded: edges {edge_delta}, bytes {byte_delta}, init {init_delta} (outgoing {} gaps {} bytes {} init {})",
            view.meter.outgoing_edge_checks,
            view.meter.outgoing_gap_checks,
            view.meter.weak_mode_bytes,
            view.meter.scratch.weak_modes_inited
        );
        assert_eq!(
            view.meter.sweep_edge_checks, 0,
            "a mark pulse must not sweep"
        );
        assert_eq!(view.meter.scratch.dead_slots_pushed, 0);
        assert_eq!(view.meter.scratch.sweep_bitmap_inited, 0);
        assert_eq!(
            view.meter.mark_passes, 1,
            "pause stays in the original pass"
        );
        if view.finished {
            return view.meter;
        }
        pulses += 1;
        assert!(
            pulses < limit,
            "mark did not finish within {limit} pulses of budget {budget}"
        );
    }
}

#[test]
fn fan_out_pulse_keeps_edge_work_inside_budget() {
    let edges = 256u32;
    let mut heap = LuaHeap::new(limits(1, 1, edges));
    let owner = heap.alloc().expect("owner");
    let frame = heap.enter_roots(1).expect("frame");
    heap.write_root(frame, 0, owner).expect("root");
    for key in 1..=edges {
        heap.set_edge(owner, key, owner).expect("edge");
    }

    let mut session = MarkSession::start(&mut heap);
    let idle = session.pulse(0);
    assert!(!idle.finished, "zero budget does not finish the mark");
    assert_eq!(
        idle.meter,
        ScanMeter::default(),
        "zero budget does not read the graph or change mark state"
    );

    let meter = pulse_until_done(&mut session, 1, edges + 16);
    assert_eq!(meter.outgoing_edge_checks, u64::from(edges));
    assert_eq!(meter.outgoing_gap_checks, 0);
    assert_eq!(meter.owner_head_checks, 1);
    assert_eq!(meter.mark_passes, 1);
    assert_eq!(meter.scratch.work_pushed, 1, "the owner is marked once");
    drop(session);
    assert!(heap.alive(owner));
    assert_eq!(heap.stats().collections, 0, "mark does not collect");
}

#[test]
fn fan_out_budget_three_bounds_each_pulse() {
    let edges = 64u32;
    let budget = 3u64;
    let mut heap = LuaHeap::new(limits(1, 1, edges));
    let owner = heap.alloc().expect("owner");
    let frame = heap.enter_roots(1).expect("frame");
    heap.write_root(frame, 0, owner).expect("root");
    for key in 1..=edges {
        heap.set_edge(owner, key, owner).expect("edge");
    }
    let mut session = MarkSession::start(&mut heap);
    let first = session.pulse(budget);
    assert!(
        !first.finished,
        "budget {budget} must not walk the whole owner"
    );
    let meter = pulse_until_done(&mut session, budget, edges + 16);
    assert_eq!(meter.outgoing_edge_checks, u64::from(edges));
    assert_eq!(meter.mark_passes, 1);
}

const VALUE_PAD: usize = 24;

fn weak_values_mode() -> String {
    let mut mode = String::from("v");
    mode.extend(std::iter::repeat_n('x', VALUE_PAD));
    mode.push('\0');
    mode.push('k');
    mode
}

fn weak_keys_mode() -> String {
    let mut mode = String::from("k");
    mode.push('\0');
    mode.push('v');
    mode
}

fn parsed_values_bytes() -> u64 {
    // 'v', the pad, and the NUL. The trailing 'k' is past the terminator.
    1 + VALUE_PAD as u64 + 1
}

fn parsed_keys_bytes() -> u64 {
    // 'k' and the NUL. The trailing 'v' is past the terminator.
    2
}

#[test]
fn shared_modes_amortize_bytes_and_cache_init() {
    let mut heap = LuaHeap::new(limits(16, 8, 16));
    let mt_values = heap.alloc().expect("values metatable");
    let mt_keys = heap.alloc().expect("keys metatable");
    let owner_values = heap.alloc().expect("values owner");
    let owner_keys = heap.alloc().expect("keys owner");
    let peer = heap.alloc().expect("peer");
    let weak_orphan = heap.alloc().expect("weak orphan");
    let strong_key = heap.alloc().expect("strong key");
    let strong_value = heap.alloc().expect("strong value");
    let weak_key = heap.alloc().expect("weak key");
    let nodes = 9u64;
    let frame = heap.enter_roots(5).expect("frame");
    heap.write_root(frame, 0, mt_values)
        .expect("root mt values");
    heap.write_root(frame, 1, mt_keys).expect("root mt keys");
    heap.write_root(frame, 2, owner_values)
        .expect("root values owner");
    heap.write_root(frame, 3, owner_keys)
        .expect("root keys owner");
    heap.write_root(frame, 4, peer).expect("root peer");
    heap.set_metatable(owner_values, Some(mt_values))
        .expect("values metatable");
    heap.set_metatable(owner_keys, Some(mt_keys))
        .expect("keys metatable");
    heap.set_mode_scalar(mt_values, Value::string(weak_values_mode()))
        .expect("values mode");
    heap.set_mode_scalar(mt_keys, Value::string(weak_keys_mode()))
        .expect("keys mode");
    heap.set_object_edge(owner_values, strong_key, peer)
        .expect("strong key edge");
    heap.set_named_edge(owner_values, "orphan", weak_orphan)
        .expect("weak value edge");
    heap.set_object_edge(owner_keys, weak_key, peer)
        .expect("weak key edge");
    heap.set_named_edge(owner_keys, "kept", strong_value)
        .expect("strong value edge");
    heap.set_edge(weak_orphan, 1, weak_orphan)
        .expect("orphan cycle");

    let mut session = MarkSession::start(&mut heap);
    let meter = pulse_until_done(&mut session, 1, 256);
    assert_eq!(
        meter.weak_mode_bytes,
        parsed_values_bytes() + parsed_keys_bytes(),
        "each mode string is parsed once, and the suffix after NUL is not scanned"
    );
    assert_eq!(
        meter.scratch.weak_modes_inited, nodes,
        "the mode cache is initialized once, not once per owner"
    );
    assert_eq!(meter.weak_mode_edge_checks, 2, "one open per mode edge");
    assert_eq!(
        meter.weak_mode_lookups, 2,
        "shared metatables are looked up, not parsed again"
    );
    assert_eq!(meter.mark_passes, 1);
    let report = session.sweep();
    assert!(heap.alive(peer), "rooted peer keeps its identity");
    assert!(heap.alive(strong_key), "the suffix k was not scanned");
    assert!(heap.alive(strong_value), "the suffix v was not scanned");
    assert!(!heap.alive(weak_orphan), "a weak value orphan is reclaimed");
    assert!(!heap.alive(weak_key), "a weak key orphan is reclaimed");
    assert_eq!(
        heap.object_edge(owner_values, strong_key)
            .expect("kept key"),
        Some(peer)
    );
    assert!(
        heap.named_edge(owner_values, "orphan")
            .expect("cleared value")
            .is_none()
    );
    assert_eq!(
        heap.named_edge(owner_keys, "kept").expect("kept value"),
        Some(strong_value)
    );
    assert_eq!(
        report.live_nodes, 7,
        "two weak orphans are the only dead nodes"
    );
}

#[test]
fn plain_fan_out_sweep_keeps_the_root_and_drops_the_orphan() {
    let edges = 64u32;
    let mut heap = LuaHeap::new(limits(2, 1, edges + 1));
    let owner = heap.alloc().expect("owner");
    let orphan = heap.alloc().expect("orphan");
    let frame = heap.enter_roots(1).expect("frame");
    heap.write_root(frame, 0, owner).expect("root");
    for key in 1..=edges {
        heap.set_edge(owner, key, owner).expect("edge");
    }
    heap.set_edge(orphan, 1, orphan).expect("orphan cycle");
    let mut session = MarkSession::start(&mut heap);
    let meter = pulse_until_done(&mut session, 1, edges + 16);
    assert_eq!(meter.weak_mode_bytes, 0);
    assert_eq!(meter.scratch.weak_modes_inited, 0);
    assert_eq!(meter.outgoing_edge_checks, u64::from(edges));
    let report = session.sweep();
    assert!(heap.alive(owner));
    assert!(!heap.alive(orphan));
    assert_eq!(heap.edge(owner, 1).expect("kept edge"), Some(owner));
    assert_eq!(heap.edge(owner, edges).expect("kept tail"), Some(owner));
    assert_eq!(report.live_nodes, 1);
}

#[test]
fn gap_pulses_visit_one_record_at_a_time() {
    let mut heap = LuaHeap::new(limits(8, 4, 8));
    let mt = heap.alloc().expect("mt");
    let owner = heap.alloc().expect("owner");
    let kept = heap.alloc().expect("kept");
    let dead = [
        heap.alloc().expect("dead"),
        heap.alloc().expect("dead"),
        heap.alloc().expect("dead"),
        heap.alloc().expect("dead"),
    ];
    let frame = heap.enter_roots(3).expect("frame");
    heap.write_root(frame, 0, mt).expect("root mt");
    heap.write_root(frame, 1, owner).expect("root owner");
    heap.write_root(frame, 2, kept).expect("root kept");
    heap.set_metatable(owner, Some(mt)).expect("metatable");
    heap.set_mode_scalar(mt, Value::string("v")).expect("mode");
    for (index, target) in dead.iter().enumerate() {
        heap.set_named_edge(owner, &format!("d{index}"), *target)
            .expect("dead edge");
    }
    heap.set_named_edge(owner, "keep", kept).expect("kept edge");
    heap.collect();
    assert!(!heap.alive(dead[0]));
    let _ = heap.take_scan_meter();

    let mut session = MarkSession::start(&mut heap);
    let meter = pulse_until_done(&mut session, 1, 64);
    assert_eq!(
        meter.outgoing_gap_checks, 4,
        "each cleared cursor is visited once"
    );
    assert_eq!(meter.mark_passes, 1);
    assert!(
        meter.outgoing_edge_checks >= 1,
        "the kept edge is still an edge"
    );
    drop(session);
    assert!(heap.alive(owner));
    assert!(heap.alive(kept));
}

#[test]
fn dropped_partial_mark_collects_the_current_roots() {
    let mut heap = LuaHeap::new(limits(4, 2, 4));
    let old = heap.alloc().expect("old root");
    let child = heap.alloc().expect("child");
    let other = heap.alloc().expect("replacement root");
    let frame = heap.enter_roots(1).expect("frame");
    heap.write_root(frame, 0, old).expect("root");
    heap.set_edge(old, 1, child).expect("edge");

    let mut session = MarkSession::start(&mut heap);
    let view = session.pulse(1);
    assert!(!view.finished, "one unit does not finish this graph");
    assert_eq!(view.meter.mark_passes, 1);
    assert_eq!(view.meter.sweep_edge_checks, 0);
    assert_eq!(view.meter.scratch.dead_slots_pushed, 0);
    assert_eq!(view.meter.scratch.sweep_bitmap_inited, 0);
    drop(session);

    assert!(heap.alive(old) && heap.alive(child) && heap.alive(other));
    assert_eq!(heap.stats().collections, 0);
    let partial = heap.take_scan_meter();
    assert_eq!(
        partial.sweep_edge_checks, 0,
        "dropping a mark does not sweep"
    );
    assert_eq!(partial.mark_passes, 1);

    heap.write_root(frame, 0, other).expect("current root");
    let report = heap.collect();
    assert_eq!(report.live_nodes, 1);
    assert!(heap.alive(other));
    assert!(!heap.alive(old));
    assert!(!heap.alive(child));
    let again = heap.take_scan_meter();
    assert_eq!(again.mark_passes, 1, "the next collect is a fresh pass");
}

#[test]
fn empty_graph_finishes_and_a_discarded_pass_can_remark() {
    let mut heap = LuaHeap::new(limits(4, 2, 4));
    let mut session = MarkSession::start(&mut heap);
    let idle = session.pulse(0);
    assert!(!idle.finished);
    assert_eq!(idle.meter, ScanMeter::default());
    let done = session.pulse(1);
    assert!(done.finished, "an empty graph can finish");
    assert_eq!(done.meter.mark_passes, 1);
    assert_eq!(done.meter.node_slot_visits, 0);
    assert_eq!(done.meter.root_slot_visits, 0);
    assert_eq!(done.meter.outgoing_edge_checks, 0);
    drop(session);
    let first = heap.take_scan_meter();
    assert_eq!(first.mark_passes, 1);
    assert_eq!(first.sweep_edge_checks, 0);
    let report = heap.collect();
    assert_eq!(report.live_nodes, 0);
    let second = heap.take_scan_meter();
    assert_eq!(second.mark_passes, 1, "discard then collect marks again");
}
