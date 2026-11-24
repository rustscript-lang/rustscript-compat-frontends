//! Same-key refill of an ordinary UTF-8 gap while a published sweep tail is saved.
//!
//! Not a guest `collectgarbage` step and not a `.lua` rawset. `heap::collect`
//! and `set_named_edge` are the host paths a production guest step or rawset
//! would share; both still refuse while the tail is saved. `named_value` and
//! `next_guest` are the private host's observation of the pre-issued carrier.
//! The Roots witness reads the saved tail receipt and the scan meter. It does
//! not build a tail.

use vm::Value;

use super::{EdgeKey, EdgeValue, SlotState};
use crate::heap::{
    self, CollectReport, GuestClass, HeapError, HeapLimits, LuaHeap, PublishedBoundary,
    PublishedResume, ScanMeter, TableRef,
};

fn limits() -> HeapLimits {
    HeapLimits {
        nodes: 16,
        roots: 8,
        edges: 24,
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

fn slot_generation(heap: &LuaHeap, table: TableRef) -> u32 {
    heap.slots[table.slot() as usize].generation
}

fn marked(heap: &LuaHeap, table: TableRef) -> bool {
    matches!(
        heap.slots[table.slot() as usize].state,
        SlotState::Live { marked: true }
    )
}

fn utf8_gap(heap: &LuaHeap, owner: TableRef, name: &str) -> bool {
    let Some((_, index)) = heap.locate_named(owner.slot(), name) else {
        return false;
    };
    heap.edge_slots[index as usize]
        .edge
        .as_ref()
        .is_some_and(|edge| matches!(edge.key, EdgeKey::Utf8(_)) && edge.value.is_cursor_gap())
}

fn tail_text(host: &mut vm::HostContext<'_>) -> String {
    heap::with_heap(host, |heap| Ok(format!("{:?}", heap.sweep_tail))).expect("tail")
}

fn finish_tail(host: &mut vm::HostContext<'_>) -> CollectReport {
    let resumed = heap::resume_published_sweep(host, 64).expect("finish");
    let PublishedResume::Completed(report) = resumed else {
        panic!("one budget finishes the saved tail, got {resumed:?}");
    };
    assert_eq!(
        heap::resume_published_sweep(host, 1).expect_err("tail is gone"),
        HeapError::SweepDenied
    );
    report
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Place {
    head: Option<u32>,
    tail: Option<u32>,
    index: u32,
    next: Option<u32>,
    free: Option<u32>,
    occupied: u32,
    payload: u64,
    logical: u64,
}

fn place(heap: &LuaHeap, owner: TableRef, name: &str) -> Place {
    let (_, index) = heap.locate_named(owner.slot(), name).expect(name);
    let slot = &heap.slots[owner.slot() as usize];
    Place {
        head: slot.edge_head,
        tail: slot.edge_tail,
        index,
        next: heap.edge_slots[index as usize].next,
        free: heap.edge_free_head,
        occupied: heap.occupied_edges,
        payload: heap.payload_bytes,
        logical: heap.logical_bytes(),
    }
}

fn assert_original_carrier(
    heap: &LuaHeap,
    owner: TableRef,
    name: &str,
    table: TableRef,
    carrier: &Value,
) -> Result<(), HeapError> {
    let read = heap.named_value(owner, name)?;
    assert_eq!(read, *carrier);
    assert_eq!(
        heap.guest_class(&read),
        GuestClass::Table(table),
        "private host read is the pre-issued carrier, not a new array"
    );
    Ok(())
}

struct Graph {
    weak: TableRef,
    other: TableRef,
    spare: TableRef,
    keeper: TableRef,
    target: TableRef,
    target_token: i64,
    target_carrier: Value,
    successor: TableRef,
    successor_carrier: Value,
    doomed_a: TableRef,
    doomed_a_token: i64,
    doomed_b: TableRef,
    doomed_b_token: i64,
    doomed_b_carrier: Value,
    bare: TableRef,
    meta: TableRef,
}

fn build_graph(host: &mut vm::HostContext<'_>) -> Graph {
    heap::install(host, limits()).expect("install");
    let meta = heap::with_heap(host, LuaHeap::alloc).expect("meta");
    let (weak, _) = publish(host);
    let (other, _) = publish(host);
    let (spare, _) = publish(host);
    let (keeper, _) = publish(host);
    let (target, target_token) = publish(host);
    let (successor, _) = publish(host);
    let (doomed_a, doomed_a_token) = publish(host);
    let (doomed_b, doomed_b_token) = publish(host);
    let bare = heap::with_heap(host, LuaHeap::alloc).expect("bare");
    let (target_carrier, successor_carrier, doomed_b_carrier) = heap::with_heap(host, |heap| {
        heap.set_mode_scalar(meta, Value::string("v"))?;
        heap.set_metatable(weak, Some(meta))?;
        heap.set_metatable(other, Some(meta))?;
        heap.set_named_edge(weak, "a", doomed_a)?;
        heap.set_named_edge(weak, "after", successor)?;
        heap.set_named_edge(weak, "solid", target)?;
        heap.set_named_edge(other, "gap", doomed_b)?;
        heap.set_named_edge(other, "next", successor)?;
        heap.set_named_edge(keeper, "hold", target)?;
        heap.set_named_edge(keeper, "succ", successor)?;
        heap.set_named_edge(keeper, "bare", bare)?;
        let target_carrier = heap.issue_carrier(target)?;
        let successor_carrier = heap.issue_carrier(successor)?;
        let doomed_b_carrier = heap.issue_carrier(doomed_b)?;
        let frame = heap.enter_roots(4)?;
        heap.write_root(frame, 0, weak)?;
        heap.write_root(frame, 1, other)?;
        heap.write_root(frame, 2, spare)?;
        heap.write_root(frame, 3, keeper)?;
        Ok((target_carrier, successor_carrier, doomed_b_carrier))
    })
    .expect("graph");
    Graph {
        weak,
        other,
        spare,
        keeper,
        target,
        target_token,
        target_carrier,
        successor,
        successor_carrier,
        doomed_a,
        doomed_a_token,
        doomed_b,
        doomed_b_token,
        doomed_b_carrier,
        bare,
        meta,
    }
}

fn publish_two_gaps(host: &mut vm::HostContext<'_>, graph: &Graph) {
    let published = heap::collect_until_published(host).expect("publish");
    let PublishedBoundary::Published {
        window_heads,
        window_edges,
    } = published
    else {
        panic!("a real dead slot must save the tail, got {published:?}");
    };
    assert_eq!(window_heads, 10, "one owner head per slot");
    assert_eq!(window_edges, 9, "mode plus eight table edges");
    assert!(!token_open(host, graph.doomed_a_token));
    assert!(!token_open(host, graph.doomed_b_token));
    assert!(token_open(host, graph.target_token));
    heap::with_heap(host, |heap| {
        assert!(utf8_gap(heap, graph.weak, "a"), "first real gap");
        assert!(utf8_gap(heap, graph.other, "gap"), "second real gap");
        assert!(heap.alive(graph.doomed_a), "dead slot still physical");
        assert_eq!(
            slot_generation(heap, graph.doomed_a),
            graph.doomed_a.generation()
        );
        assert!(!marked(heap, graph.doomed_a), "dead slot was not poked");
        assert!(heap.alive(graph.doomed_b));
        assert!(!marked(heap, graph.doomed_b));
        assert_eq!(
            heap.issued_value(graph.doomed_b)?,
            graph.doomed_b_carrier,
            "the unmarked dead slot still has its carrier"
        );
        assert!(marked(heap, graph.weak));
        assert!(marked(heap, graph.other));
        assert!(marked(heap, graph.spare));
        assert!(marked(heap, graph.target));
        assert!(marked(heap, graph.bare));
        assert!(heap.slots[graph.bare.slot() as usize].carrier.is_none());
        Ok(())
    })
    .expect("published facts");
}

/// Full edge quota. A missing name does not reclaim the other owner's gap.
/// The private wrapper reuses the cleared record and the pre-issued carrier.
/// The ordinary writer and host collect still refuse. The next cycle keeps
/// the target while its strong root remains.
#[test]
fn published_refill_reuses_the_issued_carrier_under_a_full_quota() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = build_graph(&mut host);
        publish_two_gaps(&mut host, &graph);
        let _ = heap::take_scan_meter(&mut host).expect("clear meter");
        let before = heap::stats(&mut host).expect("before");
        assert_eq!(before.collections, 0);
        let tail = tail_text(&mut host);
        let resources = host.resource_count();
        heap::with_heap(&mut host, |heap| {
            heap.limits.edges = heap.occupied_edges;
            assert_eq!(heap.occupied_edges, heap.limits.edges);
            assert_eq!(
                heap.next_guest(graph.weak, &Value::Null)?
                    .expect("gap is skipped")
                    .1,
                graph.successor_carrier
            );
            Ok(())
        })
        .expect("quota");
        let weak_place = heap::with_heap(&mut host, |heap| Ok(place(heap, graph.weak, "a")))
            .expect("weak place");
        let other_place = heap::with_heap(&mut host, |heap| Ok(place(heap, graph.other, "gap")))
            .expect("other place");
        let mode_edge = heap::with_heap(&mut host, |heap| {
            Ok(heap.slots[graph.meta.slot() as usize].mode_edge)
        })
        .expect("mode");

        assert_eq!(
            heap::collect(&mut host).expect_err("production collect"),
            HeapError::SweepPending
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| {
                heap.set_named_edge(graph.weak, "a", graph.target)
            }),
            Err(HeapError::SweepPending),
            "ordinary writer still stops at the sweep gate"
        );

        let mut foreign_heap = LuaHeap::new(limits());
        let foreign = foreign_heap.alloc().expect("foreign");
        assert_eq!(
            heap::refill_published_gap(&mut host, graph.spare, "missing", foreign),
            Ok(false),
            "a missing name does not inspect the target"
        );
        assert!(foreign_heap.alive(foreign));
        heap::with_heap(&mut host, |heap| {
            assert_eq!(place(heap, graph.other, "gap"), other_place);
            assert!(utf8_gap(heap, graph.other, "gap"));
            assert_eq!(
                heap.next_guest(graph.other, &Value::string("gap"))?
                    .expect("other gap cursor continues"),
                (Value::string("next"), graph.successor_carrier.clone())
            );
            Ok(())
        })
        .expect("missing key did not reclaim");

        assert_eq!(
            heap::refill_published_gap(&mut host, graph.weak, "a", graph.target),
            Ok(true)
        );
        heap::with_heap(&mut host, |heap| {
            assert_eq!(place(heap, graph.weak, "a"), weak_place);
            assert_eq!(place(heap, graph.other, "gap"), other_place);
            assert_eq!(heap.slots[graph.meta.slot() as usize].mode_edge, mode_edge);
            assert_original_carrier(heap, graph.weak, "a", graph.target, &graph.target_carrier)?;
            assert_eq!(
                heap.next_guest(graph.weak, &Value::Null)?
                    .expect("refilled key is now visible"),
                (Value::string("a"), graph.target_carrier.clone())
            );
            assert_eq!(
                heap.next_guest(graph.weak, &Value::string("a"))?
                    .expect("successor stays"),
                (Value::string("after"), graph.successor_carrier.clone())
            );
            assert_eq!(heap.named_edge(graph.weak, "after")?, Some(graph.successor));
            assert_eq!(heap.live_edge_count, before.live_edges + 1);
            Ok(())
        })
        .expect("refilled");
        let mut expected = before;
        expected.live_edges += 1;
        assert_eq!(heap::stats(&mut host).expect("after"), expected);
        assert_eq!(host.resource_count(), resources);
        assert_eq!(tail_text(&mut host), tail);
        assert_eq!(
            heap::take_scan_meter(&mut host).expect("meter"),
            ScanMeter::default()
        );
        assert!(matches!(
            heap::resume_published_sweep(&mut host, 0).expect("zero"),
            PublishedResume::Paused
        ));

        let report = finish_tail(&mut host);
        assert_eq!(report.live_nodes, 8, "two dead slots released; one report");
        assert_eq!(heap::stats(&mut host).expect("finished").collections, 1);
        heap::with_heap(&mut host, |heap| {
            assert!(!heap.alive(graph.doomed_a));
            assert_ne!(
                slot_generation(heap, graph.doomed_a),
                graph.doomed_a.generation()
            );
            assert_original_carrier(heap, graph.weak, "a", graph.target, &graph.target_carrier)?;
            assert_eq!(
                heap.next_guest(graph.weak, &Value::string("a"))?
                    .expect("successor after the tail"),
                (Value::string("after"), graph.successor_carrier.clone())
            );
            Ok(())
        })
        .expect("reread");
        assert_eq!(
            heap::refill_published_gap(&mut host, graph.weak, "a", graph.target),
            Err(HeapError::SweepDenied),
            "the finished round has no tail"
        );

        let kept = heap::collect(&mut host).expect("strong root retains the target");
        assert_eq!(kept.live_nodes, 8);
        assert!(token_open(&mut host, graph.target_token));
        heap::with_heap(&mut host, |heap| {
            assert!(heap.alive(graph.target));
            assert_original_carrier(heap, graph.weak, "a", graph.target, &graph.target_carrier)?;
            Ok(())
        })
        .expect("retained");
    });
}

/// Refusals leave the tail, resources, and edge statistics alone. A missing
/// name still does not inspect `to`. The ordinary write gate stays closed.
#[test]
fn published_refill_rejects_without_moving_the_tail() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let ghost = TableRef::table(1, 0, 1);
        assert_eq!(
            heap::refill_published_gap(&mut host, ghost, "a", ghost),
            Err(HeapError::SweepDenied)
        );
        assert_eq!(
            heap::refill_published_gap(&mut host, ghost, "a", ghost),
            Err(HeapError::SweepDenied),
            "the refusal does not install a heap"
        );
        assert!(host.scope_state::<LuaHeap>().is_none());

        let graph = build_graph(&mut host);
        let installed = heap::stats(&mut host).expect("installed");
        assert_eq!(
            heap::refill_published_gap(&mut host, graph.weak, "a", graph.target),
            Err(HeapError::SweepDenied)
        );
        heap::with_heap(&mut host, |heap| {
            assert_eq!(heap.named_edge(graph.weak, "a")?, Some(graph.doomed_a));
            Ok(())
        })
        .expect("field remains");
        assert_eq!(heap::stats(&mut host).expect("unchanged"), installed);

        let stale = heap::with_heap(&mut host, |heap| {
            let rolled = heap.alloc()?;
            heap.rollback(rolled)?;
            Ok(rolled)
        })
        .expect("stale");
        assert_eq!(
            heap::refill_published_gap(&mut host, stale, "a", graph.target),
            Err(HeapError::StaleHandle)
        );
        let forged = heap::with_heap(&mut host, |heap| {
            let closure = heap.alloc_closure()?;
            let frame = heap.enter_roots(1)?;
            heap.write_closure_root(frame, 0, closure)?;
            heap.write_from(
                graph.weak.slot(),
                EdgeKey::named("hidden"),
                EdgeValue::Closure {
                    slot: closure.slot(),
                    generation: closure.generation(),
                },
            )?;
            Ok(TableRef::table(
                closure.domain(),
                closure.slot(),
                closure.generation(),
            ))
        })
        .expect("closure");
        assert_eq!(
            heap::refill_published_gap(&mut host, forged, "a", graph.target),
            Err(HeapError::WrongKind)
        );
        let mut foreign_heap = LuaHeap::new(limits());
        let foreign = foreign_heap.alloc().expect("foreign");
        assert_eq!(
            heap::refill_published_gap(&mut host, foreign, "a", graph.target),
            Err(HeapError::WrongDomain)
        );
        assert!(foreign_heap.alive(foreign));

        let published = heap::collect_until_published(&mut host).expect("publish");
        assert!(matches!(published, PublishedBoundary::Published { .. }));
        let _ = heap::take_scan_meter(&mut host);
        let before = heap::stats(&mut host).expect("before");
        let tail = tail_text(&mut host);
        let resources = host.resource_count();
        let logical = heap::with_heap(&mut host, |heap| Ok(heap.logical_bytes())).expect("bytes");
        let frozen = |host: &mut vm::HostContext<'_>| {
            assert_eq!(host.resource_count(), resources);
            assert_eq!(heap::stats(host).expect("stats"), before);
            assert_eq!(tail_text(host), tail);
            assert_eq!(
                heap::with_heap(host, |heap| Ok(heap.logical_bytes())).expect("bytes"),
                logical
            );
        };

        assert_eq!(
            heap::refill_published_gap(&mut host, graph.doomed_a, "a", graph.target),
            Err(HeapError::SweepPending),
            "unmarked live owner"
        );
        frozen(&mut host);
        assert_eq!(
            heap::refill_published_gap(&mut host, graph.meta, "__mode", graph.target),
            Err(HeapError::SweepPending)
        );
        frozen(&mut host);
        assert_eq!(
            heap::refill_published_gap(&mut host, graph.weak, "__mode", graph.target),
            Err(HeapError::SweepPending),
            "reserved name is not a missing ordinary field"
        );
        frozen(&mut host);
        assert_eq!(
            heap::refill_published_gap(&mut host, graph.weak, "solid", graph.target),
            Err(HeapError::SweepPending),
            "non-gap"
        );
        frozen(&mut host);
        assert_eq!(
            heap::refill_published_gap(&mut host, graph.weak, "hidden", graph.target),
            Err(HeapError::SweepPending),
            "internal value is not a gap"
        );
        frozen(&mut host);
        assert_eq!(
            heap::refill_published_gap(&mut host, graph.weak, "a", graph.doomed_b),
            Err(HeapError::SweepPending),
            "unmarked physical dead slot, same generation, carrier present"
        );
        frozen(&mut host);
        assert_eq!(
            heap::refill_published_gap(&mut host, graph.weak, "a", graph.bare),
            Err(HeapError::StaleHandle),
            "marked target with no carrier"
        );
        frozen(&mut host);
        let stale_target = TableRef::table(graph.target.domain(), graph.target.slot(), 0);
        assert_eq!(
            heap::refill_published_gap(&mut host, graph.weak, "a", stale_target),
            Err(HeapError::StaleHandle)
        );
        frozen(&mut host);
        assert_eq!(
            heap::refill_published_gap(&mut host, graph.weak, "a", forged),
            Err(HeapError::WrongKind)
        );
        frozen(&mut host);
        assert_eq!(
            heap::refill_published_gap(&mut host, graph.weak, "a", foreign),
            Err(HeapError::WrongDomain)
        );
        frozen(&mut host);
        assert_eq!(
            heap::refill_published_gap(&mut host, graph.spare, "missing", foreign),
            Ok(false)
        );
        frozen(&mut host);
        assert_eq!(
            heap::with_heap(&mut host, |heap| {
                heap.set_named_edge(graph.weak, "a", graph.target)
            }),
            Err(HeapError::SweepPending)
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| {
                heap.clear_edge(graph.weak, &EdgeKey::named("a"))
            }),
            Err(HeapError::SweepPending)
        );
        frozen(&mut host);
        heap::with_heap(&mut host, |heap| {
            assert!(utf8_gap(heap, graph.weak, "a"));
            assert!(utf8_gap(heap, graph.other, "gap"));
            assert!(heap.locate_named(graph.weak.slot(), "hidden").is_some());
            assert!(heap.slots[graph.meta.slot() as usize].mode_edge.is_some());
            assert_eq!(
                heap.next_guest(graph.other, &Value::string("gap"))?
                    .expect("gap cursor still continues")
                    .1,
                graph.successor_carrier
            );
            Ok(())
        })
        .expect("zero effect");
        assert!(foreign_heap.alive(foreign));
        assert_eq!(
            heap::take_scan_meter(&mut host).expect("meter"),
            ScanMeter::default()
        );
    });
}

/// A gap left by the previous cycle stays refillable after `__mode` changes.
/// This round's tail comes from a new dead slot. The entry does not reread mode.
#[test]
fn published_refill_accepts_a_prior_gap_after_mode_changes() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = build_graph(&mut host);
        publish_two_gaps(&mut host, &graph);
        let report = finish_tail(&mut host);
        assert_eq!(report.live_nodes, 8);
        heap::with_heap(&mut host, |heap| {
            assert!(utf8_gap(heap, graph.weak, "a"), "the gap outlives the tail");
            assert!(!heap.alive(graph.doomed_a));
            Ok(())
        })
        .expect("prior gap");
        assert_eq!(
            heap::refill_published_gap(&mut host, graph.weak, "a", graph.target),
            Err(HeapError::SweepDenied)
        );

        heap::with_heap(&mut host, |heap| {
            heap.set_mode_scalar(graph.meta, Value::string("k"))?;
            Ok(())
        })
        .expect("mode");
        let mode_edge = heap::with_heap(&mut host, |heap| {
            Ok(heap.slots[graph.meta.slot() as usize].mode_edge)
        })
        .expect("mode edge");
        assert!(mode_edge.is_some());

        let (fresh, fresh_token) = publish(&mut host);
        let published = heap::collect_until_published(&mut host).expect("next publish");
        assert!(
            matches!(published, PublishedBoundary::Published { .. }),
            "the new dead slot opens a real tail, got {published:?}"
        );
        assert!(!token_open(&mut host, fresh_token));
        heap::with_heap(&mut host, |heap| {
            assert!(heap.alive(fresh));
            assert_eq!(slot_generation(heap, fresh), fresh.generation());
            assert!(!marked(heap, fresh), "this round's dead slot was not poked");
            assert!(utf8_gap(heap, graph.weak, "a"));
            assert_eq!(heap.named_value(graph.meta, "__mode")?, Value::string("k"));
            assert_eq!(
                heap.set_named_edge(graph.weak, "a", graph.target),
                Err(HeapError::SweepPending),
                "mode change is not a write-gate bypass"
            );
            Ok(())
        })
        .expect("still a gap");

        assert_eq!(
            heap::refill_published_gap(&mut host, graph.weak, "a", graph.target),
            Ok(true),
            "the prior gap does not depend on the current mode"
        );
        heap::with_heap(&mut host, |heap| {
            assert_eq!(heap.slots[graph.meta.slot() as usize].mode_edge, mode_edge);
            assert_eq!(heap.named_value(graph.meta, "__mode")?, Value::string("k"));
            assert_original_carrier(heap, graph.weak, "a", graph.target, &graph.target_carrier)?;
            assert_eq!(
                heap.refill_published_gap(graph.weak, "solid", graph.target),
                Err(HeapError::SweepPending),
                "a non-gap is still refused"
            );
            Ok(())
        })
        .expect("refilled without touching mode");
    });
}

/// After the tail, clearing the target's only strong edge lets the next real
/// collection release it. The weak edge becomes a gap again.
#[test]
fn published_refill_releases_the_target_after_its_strong_root_is_cleared() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = build_graph(&mut host);
        let resources = host.resource_count();
        publish_two_gaps(&mut host, &graph);
        assert_eq!(
            heap::refill_published_gap(&mut host, graph.weak, "a", graph.target),
            Ok(true)
        );
        let report = finish_tail(&mut host);
        assert_eq!(report.live_nodes, 8);
        heap::with_heap(&mut host, |heap| {
            heap.set_named_scalar(graph.keeper, "hold", Value::Null)?;
            Ok(())
        })
        .expect("drop the strong edge");
        let cleaned = heap::collect(&mut host).expect("next cycle");
        assert_eq!(
            cleaned.live_nodes, 7,
            "the refilled target is actually released"
        );
        assert!(!token_open(&mut host, graph.target_token));
        assert_eq!(host.resource_count(), resources - 3);
        heap::with_heap(&mut host, |heap| {
            assert!(!heap.alive(graph.target));
            assert_ne!(
                slot_generation(heap, graph.target),
                graph.target.generation()
            );
            assert!(utf8_gap(heap, graph.weak, "a"));
            assert!(utf8_gap(heap, graph.weak, "solid"));
            assert_eq!(heap.named_value(graph.weak, "a")?, Value::Null);
            assert!(heap.alive(graph.successor));
            assert!(heap.alive(graph.keeper));
            Ok(())
        })
        .expect("cleaned");
    });
}

/// Omitting the refill leaves the gap. The strong root keeps the target, and
/// the next collection does not invent a value.
#[test]
fn omitted_refill_keeps_the_gap_and_the_strong_target() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = build_graph(&mut host);
        publish_two_gaps(&mut host, &graph);
        let report = finish_tail(&mut host);
        assert_eq!(report.live_nodes, 8);
        let collected = heap::collect(&mut host).expect("omitted followup");
        assert_eq!(collected.live_nodes, 8);
        assert!(token_open(&mut host, graph.target_token));
        heap::with_heap(&mut host, |heap| {
            assert!(utf8_gap(heap, graph.weak, "a"));
            assert_eq!(heap.named_value(graph.weak, "a")?, Value::Null);
            assert!(heap.alive(graph.target));
            assert_eq!(heap.named_edge(graph.keeper, "hold")?, Some(graph.target));
            Ok(())
        })
        .expect("omitted");
    });
}

/// Root slots charged before the refill. The rest of the frame stays pending.
const ROOT_PREFIX: usize = 2;

fn unmarked_slots(heap: &LuaHeap) -> Vec<u32> {
    heap.slots
        .iter()
        .enumerate()
        .filter_map(|(index, slot)| {
            matches!(slot.state, SlotState::Live { marked: false }).then_some(index as u32)
        })
        .collect()
}

fn marked_slot_count(heap: &LuaHeap) -> u32 {
    heap.slots
        .iter()
        .filter(|slot| matches!(slot.state, SlotState::Live { marked: true }))
        .count() as u32
}

fn tail_receipt(phase: &str, index: usize, dead: &[u32], marked: u32, live_roots: u32) -> String {
    format!(
        "Some(SweepTail {{ phase: {phase}, index: {index}, dead: {dead:?}, \
         marked_nodes: {marked}, live_roots: {live_roots}, late_nodes: 0 }})"
    )
}

fn visits(nodes: u64, roots: u64) -> ScanMeter {
    ScanMeter {
        node_slot_visits: nodes,
        root_slot_visits: roots,
        ..ScanMeter::default()
    }
}

struct RootsWitness {
    prefix_receipt: String,
    report: CollectReport,
}

fn resume_paused(host: &mut vm::HostContext<'_>, budget: u64) {
    let resumed = heap::resume_published_sweep(host, budget).expect("saved tail");
    assert!(
        matches!(resumed, PublishedResume::Paused),
        "the saved tail is not finished, got {resumed:?}"
    );
}

/// Refill during a real Roots prefix, or omit it and keep the gap. Both arms
/// release the published dead slots, scan the same prefix, then finish the
/// remaining root slots once. `named_value` is the direct read a `rawget`
/// candidate would use. The tail comes from `collect_until_published`.
fn drive_roots_refill(refill: bool) -> RootsWitness {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = build_graph(&mut host);
        let resources_before = host.resource_count();
        publish_two_gaps(&mut host, &graph);
        let _ = heap::take_scan_meter(&mut host).expect("clear publish meter");

        let (dead, marked, root_slots) = heap::with_heap(&mut host, |heap| {
            let dead = unmarked_slots(heap);
            let mut expected = [graph.doomed_a.slot(), graph.doomed_b.slot()];
            expected.sort_unstable();
            assert_eq!(dead, expected, "the dead set is the two published orphans");
            assert!(
                heap.roots.iter().all(|root| root.is_some()),
                "the frame has no hole to hide a skipped visit"
            );
            assert!(
                heap.roots.len() > ROOT_PREFIX,
                "a partial prefix needs a pending suffix"
            );
            Ok((dead, marked_slot_count(heap), heap.roots.len()))
        })
        .expect("dead set");
        assert_eq!(
            tail_text(&mut host),
            tail_receipt("Release", 0, &dead, marked, 0),
            "publish saved a real Release tail"
        );

        resume_paused(&mut host, dead.len() as u64);
        assert_eq!(
            heap::take_scan_meter(&mut host).expect("release meter"),
            visits(dead.len() as u64, 0),
            "release visits the dead slots and no root slot"
        );
        assert_eq!(
            tail_text(&mut host),
            tail_receipt("Release", dead.len(), &dead, marked, 0),
            "the budget ended on the last release; Roots has not started"
        );
        heap::with_heap(&mut host, |heap| {
            assert!(!heap.alive(graph.doomed_a));
            assert!(!heap.alive(graph.doomed_b));
            assert_ne!(
                slot_generation(heap, graph.doomed_a),
                graph.doomed_a.generation()
            );
            assert_ne!(
                slot_generation(heap, graph.doomed_b),
                graph.doomed_b.generation()
            );
            Ok(())
        })
        .expect("released");
        assert!(!token_open(&mut host, graph.doomed_a_token));
        assert!(!token_open(&mut host, graph.doomed_b_token));
        assert_eq!(heap::stats(&mut host).expect("released").collections, 0);

        resume_paused(&mut host, ROOT_PREFIX as u64);
        assert_eq!(
            heap::take_scan_meter(&mut host).expect("prefix meter"),
            visits(ROOT_PREFIX as u64, ROOT_PREFIX as u64),
            "the prefix is occupied root slots, not a call-count guess"
        );
        let prefix_receipt = tail_receipt("Roots", ROOT_PREFIX, &dead, marked, ROOT_PREFIX as u32);
        assert_eq!(
            tail_text(&mut host),
            prefix_receipt,
            "receipt shows Roots, a scanned prefix, and an unfinished tail"
        );
        assert_eq!(heap::stats(&mut host).expect("paused").collections, 0);
        assert!(matches!(
            heap::resume_published_sweep(&mut host, 0).expect("zero"),
            PublishedResume::Paused
        ));
        assert_eq!(
            tail_text(&mut host),
            prefix_receipt,
            "zero budget does not move the cursor"
        );
        assert_eq!(
            heap::collect(&mut host).expect_err("production collect"),
            HeapError::SweepPending
        );
        assert_eq!(
            heap::take_scan_meter(&mut host).expect("clear incremental meter"),
            ScanMeter::default()
        );

        let before = heap::stats(&mut host).expect("before");
        let resources = host.resource_count();
        let (weak_place, other_place, mode_edge, saved_cursor) =
            heap::with_heap(&mut host, |heap| {
                let saved_cursor = heap
                    .next_guest(graph.weak, &Value::string("a"))?
                    .expect("the cleaned key is a saved cursor");
                Ok((
                    place(heap, graph.weak, "a"),
                    place(heap, graph.other, "gap"),
                    heap.slots[graph.meta.slot() as usize].mode_edge,
                    saved_cursor,
                ))
            })
            .expect("places");
        assert_eq!(saved_cursor.0, Value::string("after"));
        assert_eq!(saved_cursor.1, graph.successor_carrier);

        if refill {
            assert_eq!(
                heap::with_heap(&mut host, |heap| {
                    heap.set_named_edge(graph.weak, "a", graph.target)
                }),
                Err(HeapError::SweepPending),
                "ordinary writer still stops at the sweep gate"
            );
            assert_eq!(
                heap::refill_published_gap(&mut host, graph.weak, "a", graph.target),
                Ok(true)
            );
            heap::with_heap(&mut host, |heap| {
                assert_eq!(place(heap, graph.weak, "a"), weak_place);
                assert_eq!(place(heap, graph.other, "gap"), other_place);
                assert_eq!(heap.slots[graph.meta.slot() as usize].mode_edge, mode_edge);
                assert_original_carrier(
                    heap,
                    graph.weak,
                    "a",
                    graph.target,
                    &graph.target_carrier,
                )?;
                assert_eq!(
                    heap.next_guest(graph.weak, &Value::string("a"))?
                        .expect("saved cursor unchanged"),
                    saved_cursor.clone()
                );
                assert_eq!(
                    heap.next_guest(graph.weak, &Value::Null)?
                        .expect("refilled key is now visible"),
                    (Value::string("a"), graph.target_carrier.clone())
                );
                assert_eq!(
                    heap.next_guest(graph.other, &Value::string("gap"))?
                        .expect("other gap cursor"),
                    (Value::string("next"), graph.successor_carrier.clone())
                );
                assert!(utf8_gap(heap, graph.other, "gap"));
                assert_eq!(heap.named_edge(graph.keeper, "hold")?, Some(graph.target));
                assert_eq!(heap.live_edge_count, before.live_edges + 1);
                Ok(())
            })
            .expect("refilled");
            let mut expected = before;
            expected.live_edges += 1;
            assert_eq!(heap::stats(&mut host).expect("after"), expected);
            assert_eq!(host.resource_count(), resources);
            assert_eq!(
                tail_text(&mut host),
                prefix_receipt,
                "refill does not move the cursor or collect another root"
            );
            assert_eq!(
                heap::take_scan_meter(&mut host).expect("refill meter"),
                ScanMeter::default(),
                "refill does not continue the scan"
            );
            assert_eq!(
                heap::with_heap(&mut host, |heap| {
                    heap.set_named_edge(graph.weak, "solid", graph.successor)
                }),
                Err(HeapError::SweepPending),
                "refill does not open the ordinary writer"
            );
            assert_eq!(tail_text(&mut host), prefix_receipt);
            assert_eq!(
                heap::take_scan_meter(&mut host).expect("writer refusal"),
                ScanMeter::default()
            );
        } else {
            heap::with_heap(&mut host, |heap| {
                assert!(utf8_gap(heap, graph.weak, "a"));
                assert_eq!(heap.named_value(graph.weak, "a")?, Value::Null);
                assert_eq!(place(heap, graph.other, "gap"), other_place);
                assert_eq!(
                    heap.next_guest(graph.weak, &Value::string("a"))?
                        .expect("omitted cursor"),
                    saved_cursor.clone()
                );
                Ok(())
            })
            .expect("gap remains before the suffix");
            assert_eq!(heap::stats(&mut host).expect("omitted"), before);
            assert_eq!(host.resource_count(), resources);
            assert_eq!(tail_text(&mut host), prefix_receipt);
            assert_eq!(
                heap::take_scan_meter(&mut host).expect("omission meter"),
                ScanMeter::default()
            );
        }

        let remaining = root_slots - ROOT_PREFIX;
        resume_paused(&mut host, remaining as u64);
        assert_eq!(
            heap::take_scan_meter(&mut host).expect("suffix meter"),
            visits(remaining as u64, remaining as u64),
            "the same Roots phase scans the remaining slots"
        );
        assert_eq!(
            tail_text(&mut host),
            tail_receipt("Roots", root_slots, &dead, marked, root_slots as u32),
            "the cursor is at the end of Roots and the cycle is not reported"
        );
        assert_eq!(heap::stats(&mut host).expect("suffix").collections, 0);

        let resumed = heap::resume_published_sweep(&mut host, 1).expect("complete");
        let PublishedResume::Completed(report) = resumed else {
            panic!("the uncharged completion finishes once, got {resumed:?}");
        };
        assert_eq!(
            heap::take_scan_meter(&mut host).expect("completion meter"),
            ScanMeter::default(),
            "completion does not scan another root"
        );
        assert_eq!(report.live_nodes, marked);
        assert_eq!(report.marked_nodes, marked);
        assert_eq!(report.live_roots, root_slots as u32);
        assert_eq!(heap::stats(&mut host).expect("finished").collections, 1);
        assert_eq!(
            heap::resume_published_sweep(&mut host, 1).expect_err("tail is gone"),
            HeapError::SweepDenied
        );

        if refill {
            heap::with_heap(&mut host, |heap| {
                assert_original_carrier(
                    heap,
                    graph.weak,
                    "a",
                    graph.target,
                    &graph.target_carrier,
                )?;
                Ok(())
            })
            .expect("identity survives the rest of the root scan");
            let kept = heap::collect(&mut host).expect("strong root retains the target");
            assert_eq!(kept.live_nodes, marked);
            assert!(token_open(&mut host, graph.target_token));
            heap::with_heap(&mut host, |heap| {
                assert_original_carrier(
                    heap,
                    graph.weak,
                    "a",
                    graph.target,
                    &graph.target_carrier,
                )?;
                heap.set_named_scalar(graph.keeper, "hold", Value::Null)?;
                Ok(())
            })
            .expect("drop the independent strong root");
            let cleaned = heap::collect(&mut host).expect("next cycle");
            assert_eq!(cleaned.live_nodes, marked - 1);
            assert!(!token_open(&mut host, graph.target_token));
            assert_eq!(host.resource_count(), resources_before - 3);
            heap::with_heap(&mut host, |heap| {
                assert!(!heap.alive(graph.target));
                assert_ne!(
                    slot_generation(heap, graph.target),
                    graph.target.generation()
                );
                assert!(utf8_gap(heap, graph.weak, "a"));
                assert!(utf8_gap(heap, graph.weak, "solid"));
                assert_eq!(heap.named_value(graph.weak, "a")?, Value::Null);
                assert!(heap.alive(graph.successor));
                assert!(heap.alive(graph.keeper));
                Ok(())
            })
            .expect("released after the strong root was cleared");
        } else {
            assert!(token_open(&mut host, graph.target_token));
            heap::with_heap(&mut host, |heap| {
                assert!(utf8_gap(heap, graph.weak, "a"), "omission keeps the gap");
                assert_eq!(heap.named_value(graph.weak, "a")?, Value::Null);
                assert!(heap.alive(graph.target));
                assert_eq!(heap.named_edge(graph.keeper, "hold")?, Some(graph.target));
                assert_eq!(
                    heap.next_guest(graph.weak, &Value::string("a"))?
                        .expect("cursor still continues"),
                    saved_cursor
                );
                Ok(())
            })
            .expect("omission");
        }

        RootsWitness {
            prefix_receipt,
            report,
        }
    })
}

/// Roots partial prefix: the pre-issued carrier stays readable, the saved
/// cursor and the other gap stay put, and the same phase finishes once.
/// The omission run is the matching control.
#[test]
fn published_refill_during_partial_root_scan_keeps_the_carrier_and_resumes() {
    let refilled = drive_roots_refill(true);
    let omitted = drive_roots_refill(false);
    assert_eq!(
        refilled.prefix_receipt, omitted.prefix_receipt,
        "both runs paused on the same Roots prefix"
    );
    assert_eq!(
        refilled.report.live_roots, omitted.report.live_roots,
        "finished root count matches the omission control"
    );
    assert_eq!(refilled.report, omitted.report);
}
