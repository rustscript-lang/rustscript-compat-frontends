//! One top root frame retired during a saved published sweep tail.
//!
//! The host entry is private. It does not install a heap, resume the sweep,
//! look up nodes, or roll back the scan meter. Release keeps the dead cursor.
//! Roots deducts only `Some` tags already scanned in the retiring frame.

use std::sync::Arc;

use vm::Value;

use crate::heap::{
    self, HeapError, HeapLimits, LuaHeap, ScanMeter, StepOutcome, TableRef,
    open_private_step_cycle, retire_published_root_frame, step_private_cycle,
};

const BOUND: u32 = 64;
const LOWER_SOME: u32 = 4;
const TOP_SOME: u32 = 3;
const ROOT_SLOTS: usize = 8;

#[derive(Debug)]
struct TailFacts {
    phase: String,
    index: usize,
    dead_len: usize,
    live_roots: u32,
    marked_nodes: u32,
    late_nodes: u32,
}

struct Graph {
    owner: TableRef,
    subject: TableRef,
    subject_token: i64,
    dead_a: TableRef,
    dead_a_generation: u32,
    dead_b: TableRef,
    dead_b_generation: u32,
    lower: super::RootFrameId,
    top: super::RootFrameId,
    carrier: Arc<Vec<Value>>,
}

fn limits() -> HeapLimits {
    HeapLimits {
        nodes: 16,
        roots: 8,
        edges: 16,
        max_generation: 8,
    }
}

fn publish(host: &mut vm::HostContext<'_>) -> (TableRef, i64) {
    let raw = crate::publish_raw(host);
    (crate::table_ref(host, raw), raw)
}

fn token_open(host: &mut vm::HostContext<'_>, raw: i64) -> bool {
    host.typed_resource_with_key::<heap::LuaTableResource>(
        crate::resource_handle(raw),
        &heap::LuaTableResource::key(),
    )
    .is_ok()
}

fn clear_meter(host: &mut vm::HostContext<'_>) -> ScanMeter {
    heap::take_scan_meter(host).expect("meter")
}

fn parse_tail(text: &str) -> TailFacts {
    assert!(
        text.starts_with("Some("),
        "expected a saved tail, got {text}"
    );
    let phase = field_token(text, "phase");
    let dead = dead_body(text);
    TailFacts {
        phase,
        index: field_usize(text, "index"),
        dead_len: if dead.is_empty() {
            0
        } else {
            dead.split(',').count()
        },
        live_roots: field_usize(text, "live_roots") as u32,
        marked_nodes: field_usize(text, "marked_nodes") as u32,
        late_nodes: field_usize(text, "late_nodes") as u32,
    }
}

fn field_token(text: &str, name: &str) -> String {
    let marker = format!("{name}: ");
    let start = text
        .find(&marker)
        .unwrap_or_else(|| panic!("no {name} in {text}"));
    let rest = &text[start + marker.len()..];
    let end = rest
        .find([',', ' ', '}'])
        .unwrap_or_else(|| panic!("no end for {name} in {text}"));
    rest[..end].to_string()
}

fn field_usize(text: &str, name: &str) -> usize {
    let token = field_token(text, name);
    token
        .parse()
        .unwrap_or_else(|_| panic!("bad {name} token {token} in {text}"))
}

fn dead_body(text: &str) -> String {
    let marker = "dead: [";
    let start = text
        .find(marker)
        .unwrap_or_else(|| panic!("no dead list in {text}"));
    let rest = &text[start + marker.len()..];
    let end = rest
        .find(']')
        .unwrap_or_else(|| panic!("no dead end in {text}"));
    rest[..end].to_string()
}

fn tail_facts(host: &mut vm::HostContext<'_>) -> TailFacts {
    heap::with_heap(host, |heap| {
        Ok(parse_tail(&format!("{:?}", heap.sweep_tail)))
    })
    .expect("tail")
}

fn install_graph(host: &mut vm::HostContext<'_>) -> Graph {
    heap::install(host, limits()).expect("install");
    let meta = heap::with_heap(host, LuaHeap::alloc).expect("meta");
    let anchor = heap::with_heap(host, LuaHeap::alloc).expect("anchor");
    let lower_peer = heap::with_heap(host, LuaHeap::alloc).expect("lower peer");
    let (owner, _) = publish(host);
    let (subject, subject_token) = publish(host);
    let (peer, _) = publish(host);
    let (dead_a, _) = publish(host);
    let (dead_b, _) = publish(host);
    let built = heap::with_heap(host, |heap| {
        heap.set_mode_scalar(meta, Value::string("v"))?;
        heap.set_metatable(owner, Some(meta))?;
        heap.set_named_edge(owner, "a", dead_a)?;
        heap.set_named_edge(owner, "b", dead_b)?;
        heap.set_named_edge(owner, "watch", subject)?;
        let lower = heap.enter_roots(4)?;
        heap.write_root(lower, 0, meta)?;
        heap.write_root(lower, 1, owner)?;
        heap.write_root(lower, 2, anchor)?;
        heap.write_root(lower, 3, lower_peer)?;
        let top = heap.enter_roots(4)?;
        heap.write_root(top, 0, subject)?;
        heap.write_root(top, 2, subject)?;
        heap.write_root(top, 3, peer)?;
        let carrier = heap.issue_carrier(subject)?;
        Ok((
            heap.slots[dead_a.slot() as usize].generation,
            heap.slots[dead_b.slot() as usize].generation,
            carrier,
            lower,
            top,
        ))
    })
    .expect("graph");
    let (dead_a_generation, dead_b_generation, carrier, lower, top) = built;
    let Value::Array(carrier) = carrier else {
        panic!("subject carrier is the issued array");
    };
    Graph {
        owner,
        subject,
        subject_token,
        dead_a,
        dead_a_generation,
        dead_b,
        dead_b_generation,
        lower,
        top,
        carrier,
    }
}

fn open_and_publish(host: &mut vm::HostContext<'_>) {
    clear_meter(host);
    open_private_step_cycle(host).expect("open");
    for _ in 0..BOUND {
        match step_private_cycle(host, 1, &[]).expect("mark") {
            StepOutcome::Paused { tail: false } => {}
            StepOutcome::Published { .. } => {
                let facts = tail_facts(host);
                assert_eq!(facts.phase, "Release");
                assert_eq!(facts.index, 0);
                assert_eq!(facts.dead_len, 2, "two weak orphans stay physical");
                assert_eq!(facts.live_roots, 0);
                return;
            }
            other => panic!("small budgets stay in mark until publish, got {other:?}"),
        }
    }
    panic!("publish did not arrive");
}

fn step_paused(host: &mut vm::HostContext<'_>) -> ScanMeter {
    clear_meter(host);
    let outcome = step_private_cycle(host, 1, &[]).expect("tail step");
    assert!(
        matches!(outcome, StepOutcome::Paused { tail: true }),
        "budget 1 does not finish, got {outcome:?}"
    );
    clear_meter(host)
}

fn drive_roots_index(host: &mut vm::HostContext<'_>, target: usize) {
    for _ in 0..BOUND {
        let facts = tail_facts(host);
        if facts.phase == "Roots" && facts.index == target {
            return;
        }
        if facts.phase == "Roots" && facts.index > target {
            panic!("root cursor overshot {target}, got {facts:?}");
        }
        let meter = step_paused(host);
        if tail_facts(host).phase == "Roots" {
            assert_eq!(meter.root_slot_visits, 1, "one root slot per charged step");
        }
    }
    panic!(
        "did not reach roots index {target}, now {:?}",
        tail_facts(host)
    );
}

fn finish(host: &mut vm::HostContext<'_>) -> heap::CollectReport {
    for _ in 0..BOUND {
        clear_meter(host);
        match step_private_cycle(host, 1, &[]).expect("finish") {
            StepOutcome::Paused { tail: true } => {}
            StepOutcome::Completed(report) => {
                assert_eq!(
                    clear_meter(host),
                    ScanMeter::default(),
                    "the report step is uncharged"
                );
                return report;
            }
            other => panic!("a saved tail does not publish again, got {other:?}"),
        }
    }
    panic!("tail did not finish");
}

/// All eight root slots are already scanned. Truncate/pop without deducting
/// the three top `Some` tags leaves those slots in the completion report.
#[test]
fn published_root_retirement_drops_scanned_top_slots_from_the_report() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = install_graph(&mut host);
        open_and_publish(&mut host);
        drive_roots_index(&mut host, ROOT_SLOTS);
        let scanned = tail_facts(&mut host);
        assert_eq!(scanned.phase, "Roots");
        assert_eq!(scanned.index, ROOT_SLOTS);
        assert_eq!(scanned.live_roots, LOWER_SOME + TOP_SOME);
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 0);
        let meter_before = heap::with_heap(&mut host, |heap| Ok(heap.scan)).expect("meter");

        let retired = retire_published_root_frame(&mut host, graph.top).expect("retire top");
        let meter_after = heap::with_heap(&mut host, |heap| Ok(heap.scan)).expect("meter");
        assert_eq!(
            meter_after, meter_before,
            "retirement does not roll the meter back"
        );
        let report = finish(&mut host);
        assert_eq!(
            (
                report.live_roots,
                retired.dropped_live,
                retired.prefix_slots,
                retired.release_phase,
            ),
            (LOWER_SOME, TOP_SOME, 4, false),
            "truncate/pop without deducting scanned top slots leaves live_roots too large"
        );
        assert_eq!(report.marked_nodes, scanned.marked_nodes);
        assert_eq!(report.live_nodes, report.marked_nodes);
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 1);
        let Value::Array(after) =
            heap::with_heap(&mut host, |heap| heap.issued_value(graph.subject)).expect("carrier")
        else {
            panic!("floating subject carrier is still an array");
        };
        assert!(Arc::ptr_eq(&graph.carrier, &after));
        assert!(token_open(&mut host, graph.subject_token));
        assert_eq!(
            step_private_cycle(&mut host, 1, &[]).expect_err("one report"),
            HeapError::MarkDenied
        );
    });
}

/// The root cursor has scanned one lower slot and has not entered the top frame.
/// Moving that cursor to the frame start skips the remaining lower slots.
#[test]
fn published_root_retirement_still_scans_unvisited_lower_slots() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = install_graph(&mut host);
        open_and_publish(&mut host);
        drive_roots_index(&mut host, 1);
        let before = tail_facts(&mut host);
        assert_eq!(before.phase, "Roots");
        assert_eq!(before.index, 1);
        assert!(before.index < 4, "the cursor has not entered the top frame");
        assert_eq!(before.live_roots, 1);

        let retired = retire_published_root_frame(&mut host, graph.top).expect("retire top");
        let cursor = tail_facts(&mut host);
        let mut root_visits = 0u64;
        let mut seen = Vec::new();
        let report = loop {
            clear_meter(&mut host);
            match step_private_cycle(&mut host, 1, &[]).expect("continue") {
                StepOutcome::Paused { tail: true } => {
                    let facts = tail_facts(&mut host);
                    seen.push(facts.index);
                    root_visits += clear_meter(&mut host).root_slot_visits;
                }
                StepOutcome::Completed(report) => {
                    assert_eq!(clear_meter(&mut host), ScanMeter::default());
                    break report;
                }
                other => panic!("continuation stays on the tail, got {other:?}"),
            }
        };
        assert_eq!(
            (
                report.live_roots,
                root_visits,
                cursor.index,
                cursor.phase.as_str(),
                retired.prefix_slots,
                retired.dropped_live,
                seen.as_slice(),
            ),
            (LOWER_SOME, 3, 1, "Roots", 0, 0, &[2, 3, 4][..]),
            "setting the cursor to the frame start skips unscanned lower slots"
        );
        assert_eq!(report.marked_nodes, before.marked_nodes);
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.current_frame())).expect("frame")
                == Some(graph.lower)
        );
    });
}

fn generation(host: &mut vm::HostContext<'_>, table: TableRef) -> u32 {
    heap::with_heap(
        host,
        |heap| Ok(heap.slots[table.slot() as usize].generation),
    )
    .expect("generation")
}

fn alive(host: &mut vm::HostContext<'_>, table: TableRef) -> bool {
    heap::with_heap(host, |heap| Ok(heap.alive(table))).expect("alive")
}

fn watch(host: &mut vm::HostContext<'_>, owner: TableRef) -> Option<TableRef> {
    heap::with_heap(host, |heap| heap.named_edge(owner, "watch")).expect("watch")
}

fn tail_text(host: &mut vm::HostContext<'_>) -> String {
    heap::with_heap(host, |heap| Ok(format!("{:?}", heap.sweep_tail))).expect("tail text")
}

fn raw_token(handle: vm::ResourceHandle) -> i64 {
    i64::try_from(handle.raw()).expect("resource handle fits a positive integer")
}

fn publish_until_release(host: &mut vm::HostContext<'_>) {
    clear_meter(host);
    open_private_step_cycle(host).expect("open");
    for _ in 0..BOUND {
        match step_private_cycle(host, 1, &[]).expect("mark") {
            StepOutcome::Paused { tail: false } => {}
            StepOutcome::Published { .. } => {
                let facts = tail_facts(host);
                assert_eq!(facts.phase, "Release");
                assert_eq!(facts.index, 0);
                return;
            }
            other => panic!("small budgets stay in mark until publish, got {other:?}"),
        }
    }
    panic!("publish did not arrive");
}

fn save_unrooted_tail(host: &mut vm::HostContext<'_>) {
    clear_meter(host);
    open_private_step_cycle(host).expect("open");
    for _ in 0..BOUND {
        match step_private_cycle(host, 1, &[]).expect("step") {
            StepOutcome::Paused { tail: false } => {}
            StepOutcome::Published { .. } => return,
            other => panic!("an unrooted node publishes a tail, got {other:?}"),
        }
    }
    panic!("unrooted publish did not arrive");
}

/// One dead slot has been released. The dead cursor and its history stay put,
/// and the remaining lower slots are still scanned one at a time.
#[test]
fn published_root_retirement_keeps_the_release_cursor_and_scans_lower_slots() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = install_graph(&mut host);
        open_and_publish(&mut host);
        clear_meter(&mut host);
        let outcome = step_private_cycle(&mut host, 1, &[]).expect("one release");
        assert!(matches!(outcome, StepOutcome::Paused { tail: true }));
        let pending = heap::with_heap(&mut host, |heap| Ok(heap.scan)).expect("meter");
        assert_eq!(pending.node_slot_visits, 1);
        assert_eq!(pending.root_slot_visits, 0);
        assert_eq!(
            generation(&mut host, graph.dead_a),
            graph.dead_a_generation + 1
        );
        assert_eq!(generation(&mut host, graph.dead_b), graph.dead_b_generation);
        let marked = tail_facts(&mut host).marked_nodes;

        let retired = retire_published_root_frame(&mut host, graph.top).expect("retire");
        let kept = heap::with_heap(&mut host, |heap| Ok(heap.scan)).expect("meter");
        let cursor = tail_facts(&mut host);
        assert_eq!(
            kept, pending,
            "retirement does not roll the release meter back"
        );
        assert_eq!(
            (
                retired.release_phase,
                retired.prefix_slots,
                retired.dropped_live,
                cursor.phase.as_str(),
                cursor.index,
                cursor.live_roots,
                cursor.marked_nodes,
            ),
            (true, 0, 0, "Release", 1, 0, marked)
        );
        clear_meter(&mut host);
        step_paused(&mut host);
        assert_eq!(
            generation(&mut host, graph.dead_a),
            graph.dead_a_generation + 1
        );
        assert_eq!(
            generation(&mut host, graph.dead_b),
            graph.dead_b_generation + 1
        );
        assert_eq!(tail_facts(&mut host).phase, "Release");
        assert_eq!(tail_facts(&mut host).index, 2);
        drive_roots_index(&mut host, 4);
        let report = finish(&mut host);
        assert_eq!(report.live_roots, LOWER_SOME);
        assert_eq!(report.marked_nodes, marked);
        assert!(!alive(&mut host, graph.dead_a));
        assert!(!alive(&mut host, graph.dead_b));
        assert_eq!(
            generation(&mut host, graph.dead_a),
            graph.dead_a_generation + 1
        );
        assert_eq!(
            generation(&mut host, graph.dead_b),
            graph.dead_b_generation + 1
        );
    });
}

/// The scanned top prefix contains one empty slot. That slot is in the prefix
/// and is not a live root.
#[test]
fn published_root_retirement_counts_a_scanned_hole_without_calling_it_live() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = install_graph(&mut host);
        open_and_publish(&mut host);
        drive_roots_index(&mut host, 6);
        let scanned = tail_facts(&mut host);
        assert_eq!(scanned.live_roots, LOWER_SOME + 1);
        heap::with_heap(&mut host, |heap| {
            assert_eq!(heap.roots[4], Some(super::Rooted::Table(graph.subject)));
            assert!(heap.roots[5].is_none());
            Ok(())
        })
        .expect("hole");
        let retired = retire_published_root_frame(&mut host, graph.top).expect("retire");
        assert_eq!(
            (
                retired.prefix_slots,
                retired.dropped_live,
                retired.release_phase
            ),
            (2, 1, false)
        );
        assert_eq!(tail_facts(&mut host).index, 4);
        let report = finish(&mut host);
        assert_eq!(report.live_roots, LOWER_SOME);
    });
}

/// The same subject occupies two already scanned slots. Both tags are deducted.
#[test]
fn published_root_retirement_deducts_duplicate_scanned_tags() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = install_graph(&mut host);
        open_and_publish(&mut host);
        drive_roots_index(&mut host, 7);
        assert_eq!(tail_facts(&mut host).live_roots, LOWER_SOME + 2);
        heap::with_heap(&mut host, |heap| {
            assert_eq!(heap.roots[4], Some(super::Rooted::Table(graph.subject)));
            assert_eq!(heap.roots[6], Some(super::Rooted::Table(graph.subject)));
            Ok(())
        })
        .expect("duplicate");
        let retired = retire_published_root_frame(&mut host, graph.top).expect("retire");
        assert_eq!((retired.prefix_slots, retired.dropped_live), (3, 2));
        let report = finish(&mut host);
        assert_eq!(report.live_roots, LOWER_SOME);
    });
}

/// Two successive top frames account disjoint prefixes and then the report is zero.
#[test]
fn published_root_retirement_accounts_two_frames_in_disjoint_prefixes() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = install_graph(&mut host);
        open_and_publish(&mut host);
        drive_roots_index(&mut host, ROOT_SLOTS);
        let first = retire_published_root_frame(&mut host, graph.top).expect("top");
        assert_eq!(heap::stats(&mut host).expect("stats").root_slots, 4);
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.current_frame())).expect("frame"),
            Some(graph.lower)
        );
        let second = retire_published_root_frame(&mut host, graph.lower).expect("lower");
        assert_eq!(heap::stats(&mut host).expect("stats").root_slots, 0);
        assert_eq!(
            (
                first.prefix_slots,
                first.dropped_live,
                second.prefix_slots,
                second.dropped_live,
            ),
            (4, TOP_SOME, 4, LOWER_SOME)
        );
        assert_eq!(
            first.dropped_live + second.dropped_live,
            LOWER_SOME + TOP_SOME
        );
        let report = finish(&mut host);
        assert_eq!(report.live_roots, 0);
    });
}

#[test]
fn published_root_retirement_checks_the_frame_before_the_tail() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let missing = retire_published_root_frame(&mut host, heap::RootFrameId::from_parts(0, 1));
        assert_eq!(missing, Err(HeapError::SweepDenied));
        assert!(host.scope_state::<LuaHeap>().is_none());
    });
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let absent = retire_published_root_frame(&mut host, heap::RootFrameId::from_parts(0, 1));
        assert_eq!(absent, Err(HeapError::NoRootFrame));
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.sweep_tail.is_none())).expect("tail"));
        let frame = heap::with_heap(&mut host, |heap| {
            let popped = heap.enter_roots(1)?;
            heap.leave_roots(popped)?;
            let frame = heap.enter_roots(1)?;
            Ok((popped, frame))
        })
        .expect("frame");
        let (popped, frame) = frame;
        assert_eq!(
            retire_published_root_frame(&mut host, popped),
            Err(HeapError::StaleFrame)
        );
        let stale = retire_published_root_frame(
            &mut host,
            heap::RootFrameId::from_parts(frame.parts().0, frame.parts().1 + 1),
        );
        assert_eq!(stale, Err(HeapError::StaleFrame));
        assert_eq!(
            retire_published_root_frame(&mut host, frame),
            Err(HeapError::SweepDenied)
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.current_frame())).expect("frame"),
            Some(frame)
        );
        assert_eq!(heap::stats(&mut host).expect("stats").root_slots, 1);
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.enter_roots(0)).expect_err("zero"),
            HeapError::StaleFrame
        );
        assert_eq!(heap::stats(&mut host).expect("stats").root_slots, 1);
    });
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let (dead, token) = publish(&mut host);
        save_unrooted_tail(&mut host);
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.current_frame()))
                .expect("frame")
                .is_none()
        );
        let before = tail_text(&mut host);
        let refused = retire_published_root_frame(&mut host, heap::RootFrameId::from_parts(0, 1));
        assert_eq!(refused, Err(HeapError::NoRootFrame));
        assert_eq!(tail_text(&mut host), before);
        assert!(!token_open(&mut host, token));
        assert!(dead.slot() < limits().nodes);
    });
}

#[test]
fn published_root_retirement_refuses_a_non_top_or_old_frame_without_moving_the_tail() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = install_graph(&mut host);
        open_and_publish(&mut host);
        let before = tail_text(&mut host);
        let meter = heap::with_heap(&mut host, |heap| Ok(heap.scan)).expect("meter");
        let non_top = retire_published_root_frame(&mut host, graph.lower);
        let old = retire_published_root_frame(
            &mut host,
            heap::RootFrameId::from_parts(graph.top.parts().0, graph.top.parts().1 + 1),
        );
        assert_eq!(non_top, Err(HeapError::StaleFrame));
        assert_eq!(old, Err(HeapError::StaleFrame));
        assert_eq!(tail_text(&mut host), before);
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.scan)).expect("meter"),
            meter
        );
        assert_eq!(
            heap::stats(&mut host).expect("stats").root_slots,
            ROOT_SLOTS as u32
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.current_frame())).expect("frame"),
            Some(graph.top)
        );
    });
}

#[test]
fn published_root_retirement_keeps_ordinary_root_and_write_gates() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = install_graph(&mut host);
        open_and_publish(&mut host);
        let before = tail_text(&mut host);
        heap::with_heap(&mut host, |heap| {
            assert_eq!(heap.leave_roots(graph.top), Err(HeapError::SweepPending));
            assert_eq!(heap.enter_roots(1), Err(HeapError::RootCapacity));
            assert_eq!(
                heap.write_root(graph.lower, 0, graph.owner),
                Err(HeapError::SweepPending)
            );
            assert_eq!(
                heap.clear_root(graph.lower, 0),
                Err(HeapError::SweepPending)
            );
            assert_eq!(
                heap.set_named_edge(graph.owner, "extra", graph.subject),
                Err(HeapError::SweepPending)
            );
            Ok(())
        })
        .expect("gates");
        assert_eq!(tail_text(&mut host), before);
        assert_eq!(
            heap::stats(&mut host).expect("stats").root_slots,
            ROOT_SLOTS as u32
        );
        let retired = retire_published_root_frame(&mut host, graph.top).expect("retire");
        assert!(retired.release_phase);
        heap::with_heap(&mut host, |heap| {
            assert_eq!(heap.leave_roots(graph.lower), Err(HeapError::SweepPending));
            assert_eq!(heap.leave_roots(graph.top), Err(HeapError::StaleFrame));
            assert_eq!(heap.enter_roots(1), Err(HeapError::SweepPending));
            assert_eq!(
                heap.clear_root(graph.lower, 1),
                Err(HeapError::SweepPending)
            );
            assert_eq!(
                heap.set_named_edge(graph.owner, "extra", graph.subject),
                Err(HeapError::SweepPending)
            );
            Ok(())
        })
        .expect("gates after");
        let cursor = tail_facts(&mut host);
        let (late, handle) = heap::publish_late(&mut host).expect("late still opens");
        let after = tail_facts(&mut host);
        assert_eq!(after.index, cursor.index);
        assert_eq!(after.phase, cursor.phase);
        assert_eq!(after.late_nodes, cursor.late_nodes + 1);
        assert!(token_open(&mut host, raw_token(handle)));
        assert!(alive(&mut host, late));
        assert_eq!(heap::stats(&mut host).expect("stats").root_slots, 4);
    });
}

struct Arm {
    subject_open: bool,
    weak_present: bool,
    late_open: bool,
    collections: u32,
    live_roots: u32,
    live_nodes: u32,
    marked_nodes: u32,
}

fn arm_after_complete(
    host: &mut vm::HostContext<'_>,
    graph: &Graph,
    retire: bool,
) -> (Arm, Option<i64>) {
    open_and_publish(host);
    let late_token = if retire {
        let (late, handle) = heap::publish_late(host).expect("late");
        let facts = tail_facts(host);
        assert_eq!(facts.late_nodes, 1);
        let retired = retire_published_root_frame(host, graph.top).expect("retire");
        assert!(retired.release_phase);
        assert_eq!(retired.prefix_slots, 0);
        assert_eq!(tail_facts(host).index, 0);
        assert!(alive(host, late));
        assert!(matches!(
            heap::with_heap(host, |heap| Ok(heap.slots[late.slot() as usize].state))
                .expect("state"),
            super::SlotState::Live { marked: false }
        ));
        Some(raw_token(handle))
    } else {
        None
    };
    let report = finish(host);
    let arm = Arm {
        subject_open: token_open(host, graph.subject_token),
        weak_present: watch(host, graph.owner) == Some(graph.subject),
        late_open: late_token.is_some_and(|token| token_open(host, token)),
        collections: heap::stats(host).expect("stats").collections,
        live_roots: report.live_roots,
        live_nodes: report.live_nodes,
        marked_nodes: report.marked_nodes,
    };
    (arm, late_token)
}

/// A late node allocated before retirement stays alive for this cycle only.
/// Keeping the frame, retiring before the cycle, and omitting the next cycle
/// do not produce that same pair of observations.
#[test]
fn published_root_retirement_floats_the_current_cycle_and_collects_next() {
    let floated = crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = install_graph(&mut host);
        let (before_next, late_token) = arm_after_complete(&mut host, &graph, true);
        assert!(before_next.subject_open);
        assert!(before_next.weak_present);
        assert!(before_next.late_open);
        assert_eq!(before_next.live_nodes, before_next.marked_nodes + 1);
        assert_eq!(before_next.live_roots, LOWER_SOME);
        let Value::Array(after) =
            heap::with_heap(&mut host, |heap| heap.issued_value(graph.subject)).expect("carrier")
        else {
            panic!("carrier remains an array");
        };
        assert!(Arc::ptr_eq(&graph.carrier, &after));
        heap::collect(&mut host).expect("next");
        assert_eq!(
            heap::stats(&mut host).expect("stats").collections,
            before_next.collections + 1
        );
        (
            !token_open(&mut host, graph.subject_token),
            watch(&mut host, graph.owner).is_none(),
            !token_open(&mut host, late_token.expect("late token")),
            alive(&mut host, graph.owner),
        )
    });
    let omitted = crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = install_graph(&mut host);
        let (arm, _) = arm_after_complete(&mut host, &graph, true);
        (
            arm.subject_open,
            arm.weak_present,
            arm.late_open,
            arm.collections,
        )
    });
    let retained = crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = install_graph(&mut host);
        let (arm, _) = arm_after_complete(&mut host, &graph, false);
        let again = heap::collect(&mut host).expect("next");
        (
            arm.live_roots,
            token_open(&mut host, graph.subject_token),
            watch(&mut host, graph.owner) == Some(graph.subject),
            again.live_roots,
        )
    });
    let before_cycle = crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = install_graph(&mut host);
        heap::with_heap(&mut host, |heap| heap.leave_roots(graph.top)).expect("leave before cycle");
        publish_until_release(&mut host);
        let closed_at_publish = !token_open(&mut host, graph.subject_token);
        let weak_cleared = watch(&mut host, graph.owner).is_none();
        let report = finish(&mut host);
        (
            closed_at_publish,
            weak_cleared,
            report.live_roots,
            !alive(&mut host, graph.subject),
        )
    });
    assert_eq!(floated, (true, true, true, true));
    assert_eq!(omitted, (true, true, true, 1));
    assert_eq!(
        retained,
        (LOWER_SOME + TOP_SOME, true, true, LOWER_SOME + TOP_SOME)
    );
    assert_eq!(before_cycle, (true, true, LOWER_SOME, true));
}
