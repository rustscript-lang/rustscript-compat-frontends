//! Private pending-finalizer qualification, metatable propagation, and weak
//! publication witness on the unwired Lua heap.
//!
//! These tests use the real `publish_userdata` typed resource, a real
//! userdata-to-metatable strong edge, and a raw `__gc = false` table field.
//! They never root the subject userdata, its metatable, or the two-node table
//! graph, never install a table token, and never hand-place a pending record.
//! The private retirement entry is an explicit test control for the eventual
//! finalizer consumer; it is not a callback and is not presented as one.

use std::panic::AssertUnwindSafe;

use vm::Value;

use crate::heap::{
    self, HandleKind, HeapError, HeapLimits, LuaUserdataResource, PublishedBoundary, RootFrameId,
    TableRef, UserdataRef,
};

fn limits() -> HeapLimits {
    HeapLimits {
        nodes: 32,
        roots: 8,
        edges: 64,
        max_generation: 8,
    }
}

fn resource_handle(raw: u64) -> vm::ResourceHandle {
    vm::ResourceHandle::from_raw(raw).expect("valid resource handle")
}

fn bound_raw(host: &mut vm::HostContext<'_>, userdata: UserdataRef) -> u64 {
    heap::with_heap(host, |heap| heap.published_userdata_token(userdata))
        .expect("bound userdata token")
}

fn userdata_token_open(host: &vm::HostContext<'_>, raw: u64) -> bool {
    host.typed_resource_with_key::<LuaUserdataResource>(
        resource_handle(raw),
        &LuaUserdataResource::key(),
    )
    .is_ok()
}

fn pending_records(host: &mut vm::HostContext<'_>) -> Vec<(u64, u32, u32)> {
    heap::with_heap(host, |heap| Ok(heap.pending_finalizer_records())).expect("pending records")
}

fn slot_generation(host: &mut vm::HostContext<'_>, slot: u32) -> u32 {
    heap::with_heap(host, |heap| Ok(heap.slot_generation(slot)))
        .expect("slot generation")
        .expect("slot exists")
}

fn alive(host: &mut vm::HostContext<'_>, table: TableRef) -> bool {
    heap::with_heap(host, |heap| Ok(heap.alive(table))).expect("table liveness")
}

fn weak_table(heap: &mut heap::LuaHeap, mode: &str) -> Result<TableRef, HeapError> {
    let table = heap.alloc()?;
    let meta = heap.alloc()?;
    heap.set_mode_scalar(meta, Value::string(mode))?;
    heap.set_metatable(table, Some(meta))?;
    Ok(table)
}

/// The dedicated private pending-finalizer publication entry.
fn publish_boundary(host: &mut vm::HostContext<'_>) -> PublishedBoundary {
    heap::collect_pending_finalizers(host).expect("pending-finalizer publish boundary")
}

fn finish_release(host: &mut vm::HostContext<'_>) -> heap::CollectReport {
    match heap::resume_published_sweep(host, u64::MAX).expect("release") {
        heap::PublishedResume::Completed(report) => report,
        heap::PublishedResume::Paused => panic!("a full budget finishes the release tail"),
    }
}

struct Lattice {
    userdata: UserdataRef,
    userdata_raw: u64,
    weak_values: TableRef,
    weak_keys: TableRef,
    weak_key_values: TableRef,
    metatable: TableRef,
    graph_first: TableRef,
    graph_second: TableRef,
    orphan: Option<TableRef>,
    frame: RootFrameId,
}

/// Builds the weak tables and the finalizer graph. The weak tables are the
/// only roots; the userdata, its metatable, and the two-node graph have no
/// strong observation.
fn seed(host: &mut vm::HostContext<'_>, with_orphan: bool) -> Lattice {
    heap::install(host, limits()).expect("install");
    let userdata = heap::publish_userdata(host).expect("typed userdata");
    let userdata_raw = bound_raw(host, userdata);
    let (metatable, graph_first, graph_second, weak_values, weak_keys, weak_key_values, orphan) =
        heap::with_heap(host, |heap| {
            let metatable = heap.alloc()?;
            heap.set_named_scalar(metatable, "__gc", Value::Bool(false))?;
            let graph_first = heap.alloc()?;
            let graph_second = heap.alloc()?;
            heap.set_named_edge(metatable, "graph", graph_first)?;
            heap.set_named_edge(graph_first, "next", graph_second)?;
            heap.set_named_edge(graph_second, "next", graph_first)?;
            heap.set_userdata_metatable(userdata, Some(metatable))?;

            let orphan = if with_orphan {
                let orphan = heap.alloc()?;
                heap.set_edge(orphan, 1, orphan)?;
                Some(orphan)
            } else {
                None
            };

            let weak_values = heap.alloc()?;
            let values_mode = heap.alloc()?;
            heap.set_mode_scalar(values_mode, Value::string("v"))?;
            heap.set_metatable(weak_values, Some(values_mode))?;
            heap.set_number_userdata(weak_values, 1.0, userdata)?;
            if let Some(orphan) = orphan {
                heap.set_number_edge(weak_values, 2.0, orphan)?;
            }

            let weak_keys = heap.alloc()?;
            let keys_mode = heap.alloc()?;
            heap.set_mode_scalar(keys_mode, Value::string("k"))?;
            heap.set_metatable(weak_keys, Some(keys_mode))?;
            heap.set_userdata_key_scalar(weak_keys, userdata, Value::string("keys"))?;

            let weak_key_values = heap.alloc()?;
            let key_values_mode = heap.alloc()?;
            heap.set_mode_scalar(key_values_mode, Value::string("kv"))?;
            heap.set_metatable(weak_key_values, Some(key_values_mode))?;
            heap.set_userdata_key_scalar(weak_key_values, userdata, Value::Float(7.5))?;

            Ok((
                metatable,
                graph_first,
                graph_second,
                weak_values,
                weak_keys,
                weak_key_values,
                orphan,
            ))
        })
        .expect("lattice");
    let frame = heap::with_heap(host, |heap| {
        let frame = heap.enter_roots(3)?;
        heap.write_root(frame, 0, weak_values)?;
        heap.write_root(frame, 1, weak_keys)?;
        heap.write_root(frame, 2, weak_key_values)?;
        Ok(frame)
    })
    .expect("roots");
    Lattice {
        userdata,
        userdata_raw,
        weak_values,
        weak_keys,
        weak_key_values,
        metatable,
        graph_first,
        graph_second,
        orphan,
        frame,
    }
}

fn assert_graph_alive(host: &mut vm::HostContext<'_>, lattice: &Lattice) {
    assert_eq!(
        heap::with_heap(host, |heap| heap.userdata_metatable(lattice.userdata)).expect("metatable"),
        Some(lattice.metatable)
    );
    assert!(
        alive(host, lattice.metatable),
        "the selected userdata's metatable stays alive"
    );
    assert_eq!(
        heap::with_heap(host, |heap| heap.named_edge(lattice.metatable, "graph"))
            .expect("metatable graph edge"),
        Some(lattice.graph_first)
    );
    assert_eq!(
        heap::with_heap(host, |heap| heap.named_edge(lattice.graph_first, "next"))
            .expect("first graph edge"),
        Some(lattice.graph_second)
    );
    assert_eq!(
        heap::with_heap(host, |heap| heap.named_edge(lattice.graph_second, "next"))
            .expect("closing graph edge"),
        Some(lattice.graph_first)
    );
    assert!(
        alive(host, lattice.graph_first),
        "the first graph node stays alive"
    );
    assert!(
        alive(host, lattice.graph_second),
        "the second graph node stays alive"
    );
}

#[test]
fn selected_userdata_keeps_token_and_metatable_graph_at_weak_publish() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let lattice = seed(&mut host, true);
        let resources_before = host.resource_count();
        let _ = heap::take_scan_meter(&mut host);

        let boundary = publish_boundary(&mut host);
        assert!(
            matches!(boundary, PublishedBoundary::Published { .. }),
            "a real weak publication must stop at the published boundary"
        );
        let tail = heap::saved_tail_state(&mut host)
            .expect("tail state")
            .expect("saved release tail");
        assert_eq!(tail.phase, heap::SavedTailPhase::Release);
        assert_eq!(tail.dead_len, 1, "only the independent orphan is dead");

        // The selected userdata keeps its exact generation, typed payload, and
        // Core token across the weak publication.
        assert_eq!(
            heap::borrow_published_userdata(&host, lattice.userdata)
                .expect("selected finalizer keeps its exact Core token"),
            lattice.userdata
        );
        {
            let borrowed = host
                .borrow_resource_with_key::<LuaUserdataResource>(
                    resource_handle(lattice.userdata_raw),
                    &LuaUserdataResource::key(),
                )
                .expect("typed payload is still open");
            let payload = borrowed.get().userdata();
            assert_eq!(
                (payload.domain(), payload.slot(), payload.generation()),
                (
                    lattice.userdata.domain(),
                    lattice.userdata.slot(),
                    lattice.userdata.generation()
                )
            );
            assert_eq!(payload.kind(), HandleKind::Userdata);
        }
        assert_eq!(
            slot_generation(&mut host, lattice.userdata.slot()),
            lattice.userdata.generation(),
            "selection must not advance the original generation"
        );
        assert_eq!(
            host.resource_count(),
            resources_before,
            "no doomed token was closed"
        );

        // Weak values are cleared, including the independent orphan; the
        // original weak value key stays a readable cursor.
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap
                .number_value(lattice.weak_values, 1.0))
            .expect("weak value"),
            Value::Null
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap
                .number_value(lattice.weak_values, 2.0))
            .expect("orphan value"),
            Value::Null
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| {
                heap.next_guest(lattice.weak_values, &Value::Float(1.0))
            })
            .expect("the weak value key remains a cursor"),
            None
        );

        // Weak keys in `k` and `kv` keep the userdata key and its scalar.
        assert_eq!(
            heap::with_heap(&mut host, |heap| {
                heap.userdata_key_value(lattice.weak_keys, lattice.userdata)
            })
            .expect("weak-key scalar"),
            Value::string("keys")
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| {
                heap.userdata_key_value(lattice.weak_key_values, lattice.userdata)
            })
            .expect("weak-key-value scalar"),
            Value::Float(7.5)
        );

        assert_graph_alive(&mut host, &lattice);

        let meter = heap::take_scan_meter(&mut host).expect("publish meter");
        assert_eq!(meter.pending_finalizers_selected, 1);
        assert_eq!(
            meter.pending_finalizer_scan_slots,
            u64::from(heap::stats(&mut host).expect("slot storage").node_slots),
            "the selection scan reads every slot record once"
        );
        assert_eq!(
            meter.pending_finalizer_offers, 0,
            "no record existed before"
        );
        assert_eq!(meter.token_closes, 0);

        let report = finish_release(&mut host);
        assert_eq!(report.live_nodes, 10);
        let orphan = lattice.orphan.expect("this arm seeds an orphan");
        assert!(
            !alive(&mut host, orphan),
            "the independent weak-value orphan is released"
        );
        assert_eq!(
            slot_generation(&mut host, orphan.slot()),
            orphan.generation() + 1
        );
        assert_eq!(
            heap::borrow_published_userdata(&host, lattice.userdata).expect("still open"),
            lattice.userdata
        );
        assert!(userdata_token_open(&host, lattice.userdata_raw));
        assert_graph_alive(&mut host, &lattice);
        heap::with_heap(&mut host, |heap| heap.leave_roots(lattice.frame)).expect("leave roots");
    });
}

#[test]
fn pending_weak_clear_publishes_with_no_other_dead_slot() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let lattice = seed(&mut host, false);
        let _ = heap::take_scan_meter(&mut host);

        let boundary = publish_boundary(&mut host);
        assert!(
            matches!(boundary, PublishedBoundary::Published { .. }),
            "a pending weak clear must publish even with no dead slot"
        );
        let tail = heap::saved_tail_state(&mut host)
            .expect("tail state")
            .expect("saved release tail");
        assert_eq!(tail.phase, heap::SavedTailPhase::Release);
        assert_eq!(tail.dead_len, 0, "the selected userdata is not dead");
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap
                .number_value(lattice.weak_values, 1.0))
            .expect("weak value"),
            Value::Null
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| {
                heap.userdata_key_value(lattice.weak_keys, lattice.userdata)
            })
            .expect("weak-key scalar"),
            Value::string("keys")
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| {
                heap.userdata_key_value(lattice.weak_key_values, lattice.userdata)
            })
            .expect("weak-key-value scalar"),
            Value::Float(7.5)
        );
        assert_eq!(
            heap::borrow_published_userdata(&host, lattice.userdata).expect("token open"),
            lattice.userdata
        );
        assert_graph_alive(&mut host, &lattice);
        let meter = heap::take_scan_meter(&mut host).expect("publish meter");
        assert_eq!(meter.pending_finalizers_selected, 1);
        assert_eq!(meter.token_closes, 0);

        let report = finish_release(&mut host);
        assert_eq!(report.live_nodes, 10);
        assert_eq!(
            slot_generation(&mut host, lattice.userdata.slot()),
            lattice.userdata.generation()
        );
        assert!(userdata_token_open(&host, lattice.userdata_raw));
        heap::with_heap(&mut host, |heap| heap.leave_roots(lattice.frame)).expect("leave roots");
    });
}

#[test]
fn raw_missing_gc_through_index_chain_is_not_selected_and_clears() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let userdata = heap::publish_userdata(&mut host).expect("typed userdata");
        let raw = bound_raw(&mut host, userdata);
        let resources_before = host.resource_count();
        let (metatable, index_table, weak_values, weak_keys, weak_key_values, frame) =
            heap::with_heap(&mut host, |heap| {
                // The raw `__gc` is only reachable through `__index`; the
                // direct field on the userdata's metatable stays absent.
                let metatable = heap.alloc()?;
                let index_meta = heap.alloc()?;
                let index_table = heap.alloc()?;
                heap.set_named_scalar(index_table, "__gc", Value::Bool(false))?;
                heap.set_named_edge(index_meta, "__index", index_table)?;
                heap.set_metatable(metatable, Some(index_meta))?;
                heap.set_userdata_metatable(userdata, Some(metatable))?;

                let weak_values = weak_table(heap, "v")?;
                heap.set_number_userdata(weak_values, 1.0, userdata)?;
                let weak_keys = weak_table(heap, "k")?;
                heap.set_userdata_key_scalar(weak_keys, userdata, Value::string("keys"))?;
                let weak_key_values = weak_table(heap, "kv")?;
                heap.set_userdata_key_scalar(weak_key_values, userdata, Value::Float(7.5))?;
                let frame = heap.enter_roots(3)?;
                heap.write_root(frame, 0, weak_values)?;
                heap.write_root(frame, 1, weak_keys)?;
                heap.write_root(frame, 2, weak_key_values)?;
                Ok((
                    metatable,
                    index_table,
                    weak_values,
                    weak_keys,
                    weak_key_values,
                    frame,
                ))
            })
            .expect("missing raw gc lattice");
        let _ = heap::take_scan_meter(&mut host);

        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        let meter = heap::take_scan_meter(&mut host).expect("publish meter");
        assert_eq!(meter.pending_finalizers_selected, 0, "raw __gc is absent");
        assert_eq!(
            meter.token_closes, 1,
            "the unselected userdata token closes"
        );
        assert_eq!(host.resource_count(), resources_before - 1);
        assert!(!userdata_token_open(&host, raw));
        assert_eq!(
            heap::borrow_published_userdata(&host, userdata),
            Err(HeapError::StaleHandle)
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.number_value(weak_values, 1.0))
                .expect("weak value"),
            Value::Null
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| {
                heap.userdata_key_value(weak_keys, userdata)
            })
            .expect("weak key"),
            Value::Null
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| {
                heap.userdata_key_value(weak_key_values, userdata)
            })
            .expect("weak key value"),
            Value::Null
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.userdata_metatable(userdata))
                .expect("metatable still stored at publish"),
            Some(metatable)
        );

        let report = finish_release(&mut host);
        assert_eq!(report.live_nodes, 6, "three weak tables and three modes");
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.userdata_alive(userdata)))
                .expect("unselected userdata")
        );
        assert_eq!(
            slot_generation(&mut host, userdata.slot()),
            userdata.generation() + 1
        );
        assert!(!alive(&mut host, metatable));
        assert!(!alive(&mut host, index_table));
        heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave roots");
    });
}

#[test]
fn tokenless_userdata_and_table_with_gc_are_not_selected() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let resources_before = host.resource_count();
        let (bare, bare_metatable, table, table_metatable, weak_values, frame) =
            heap::with_heap(&mut host, |heap| {
                let bare = heap.alloc_userdata()?;
                let bare_metatable = heap.alloc()?;
                heap.set_named_scalar(bare_metatable, "__gc", Value::Bool(false))?;
                heap.set_userdata_metatable(bare, Some(bare_metatable))?;

                let table = heap.alloc()?;
                let table_metatable = heap.alloc()?;
                heap.set_named_scalar(table_metatable, "__gc", Value::Bool(false))?;
                heap.set_metatable(table, Some(table_metatable))?;

                let weak_values = weak_table(heap, "v")?;
                heap.set_number_userdata(weak_values, 1.0, bare)?;
                heap.set_number_edge(weak_values, 2.0, table)?;
                let frame = heap.enter_roots(1)?;
                heap.write_root(frame, 0, weak_values)?;
                Ok((
                    bare,
                    bare_metatable,
                    table,
                    table_metatable,
                    weak_values,
                    frame,
                ))
            })
            .expect("tokenless lattice");
        let _ = heap::take_scan_meter(&mut host);

        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        let meter = heap::take_scan_meter(&mut host).expect("publish meter");
        assert_eq!(meter.pending_finalizers_selected, 0);
        assert_eq!(meter.token_closes, 0, "neither control owns a Core token");
        assert_eq!(host.resource_count(), resources_before);
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.number_value(weak_values, 1.0))
                .expect("tokenless weak value"),
            Value::Null
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.number_value(weak_values, 2.0))
                .expect("table weak value"),
            Value::Null
        );

        let report = finish_release(&mut host);
        assert_eq!(report.live_nodes, 2, "the weak table and its mode table");
        assert!(!alive(&mut host, table));
        assert!(!alive(&mut host, table_metatable));
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.userdata_alive(bare)))
                .expect("bare userdata")
        );
        assert!(!alive(&mut host, bare_metatable));
        assert_eq!(
            slot_generation(&mut host, bare.slot()),
            bare.generation() + 1
        );
        assert_eq!(
            slot_generation(&mut host, table.slot()),
            table.generation() + 1
        );
        heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave roots");
    });
}

#[test]
fn ordinary_collect_does_not_select_a_fresh_eligible_userdata() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let lattice = seed(&mut host, false);
        let resources = host.resource_count();
        let _ = heap::take_scan_meter(&mut host);

        // Selection belongs to the private entry only. An ordinary host
        // collection keeps its frozen close/sweep path for a fresh eligible
        // object: the token closes and the metatable graph dies with it.
        let report = heap::collect(&mut host).expect("ordinary collect");
        let meter = heap::take_scan_meter(&mut host).expect("ordinary meter");
        assert_eq!(meter.pending_finalizer_scan_slots, 0);
        assert_eq!(meter.pending_finalizers_selected, 0);
        assert_eq!(meter.pending_finalizer_offers, 0);
        assert_eq!(meter.token_closes, 1);
        assert_eq!(report.live_nodes, 6);
        assert_eq!(host.resource_count(), resources - 1);
        assert!(!userdata_token_open(&host, lattice.userdata_raw));
        assert_eq!(
            heap::borrow_published_userdata(&host, lattice.userdata),
            Err(HeapError::StaleHandle)
        );
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.userdata_alive(lattice.userdata)))
                .expect("ordinary release")
        );
        assert_eq!(
            slot_generation(&mut host, lattice.userdata.slot()),
            lattice.userdata.generation() + 1
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap
                .number_value(lattice.weak_values, 1.0))
            .expect("weak value"),
            Value::Null
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| {
                heap.next_guest(lattice.weak_keys, &Value::Null)
            })
            .expect("weak-key table"),
            None
        );
        assert!(!alive(&mut host, lattice.metatable));
        assert!(!alive(&mut host, lattice.graph_first));
        assert!(!alive(&mut host, lattice.graph_second));
        heap::with_heap(&mut host, |heap| heap.leave_roots(lattice.frame)).expect("leave roots");
    });
}

#[test]
fn ordinary_collect_retains_pending_until_explicit_retirement_then_advances() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let lattice = seed(&mut host, false);
        let resources = host.resource_count();
        publish_boundary(&mut host);
        finish_release(&mut host);
        assert_eq!(pending_records(&mut host).len(), 1);
        assert!(
            heap::with_heap(&mut host, |heap| {
                heap.slot_pending_finalizer(lattice.userdata)
            })
            .expect("pending flag")
        );

        // A second selecting entry is a no-op for an already pending object.
        let _ = heap::take_scan_meter(&mut host);
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        let meter = heap::take_scan_meter(&mut host).expect("second selection meter");
        assert_eq!(meter.pending_finalizers_selected, 0, "selected only once");
        assert_eq!(
            meter.pending_finalizer_offers, 1,
            "one host-retained record"
        );
        assert_eq!(pending_records(&mut host).len(), 1);
        assert_eq!(
            heap::saved_tail_state(&mut host)
                .expect("tail")
                .expect("release tail")
                .dead_len,
            0
        );
        finish_release(&mut host);

        // The ordinary collector has no selection path and no finalizer
        // consumer, so it must keep the retained object and its exact token.
        let _ = heap::take_scan_meter(&mut host);
        let report = heap::collect(&mut host).expect("ordinary collect");
        assert_eq!(report.live_nodes, 10);
        let meter = heap::take_scan_meter(&mut host).expect("ordinary meter");
        assert_eq!(meter.pending_finalizer_offers, 1);
        assert_eq!(meter.pending_finalizers_selected, 0);
        assert_eq!(meter.token_closes, 0);
        assert_eq!(host.resource_count(), resources);
        assert!(userdata_token_open(&host, lattice.userdata_raw));
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap
                .number_value(lattice.weak_values, 1.0))
            .expect("weak value"),
            Value::Null
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| {
                heap.userdata_key_value(lattice.weak_keys, lattice.userdata)
            })
            .expect("weak key"),
            Value::string("keys")
        );
        assert_graph_alive(&mut host, &lattice);

        // Explicit retirement is the current consumer stand-in. It clears
        // only the record and the pending flag; the token stays open and the
        // generation is settled so selection cannot pick this object again.
        heap::retire_pending_finalizer_test_control(&mut host, lattice.userdata)
            .expect("retire pending record");
        assert!(pending_records(&mut host).is_empty());
        assert!(
            !heap::with_heap(&mut host, |heap| {
                heap.slot_pending_finalizer(lattice.userdata)
            })
            .expect("pending flag")
        );
        assert!(
            heap::with_heap(&mut host, |heap| {
                heap.slot_finalizer_settled(lattice.userdata)
            })
            .expect("settled flag")
        );
        assert!(userdata_token_open(&host, lattice.userdata_raw));

        // A second selecting entry does not re-qualify the settled object:
        // it is ordinary garbage now, so the token closes and Release
        // advances the generation.
        let _ = heap::take_scan_meter(&mut host);
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        let meter = heap::take_scan_meter(&mut host).expect("retired meter");
        assert_eq!(meter.pending_finalizer_offers, 0);
        assert_eq!(meter.pending_finalizers_selected, 0);
        assert_eq!(meter.token_closes, 1);
        let _report = finish_release(&mut host);
        assert_eq!(host.resource_count(), resources - 1);
        assert!(!userdata_token_open(&host, lattice.userdata_raw));
        assert_eq!(
            heap::borrow_published_userdata(&host, lattice.userdata),
            Err(HeapError::StaleHandle)
        );
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.userdata_alive(lattice.userdata)))
                .expect("retired userdata")
        );
        assert_eq!(
            slot_generation(&mut host, lattice.userdata.slot()),
            lattice.userdata.generation() + 1,
            "the final release advances the slot generation"
        );
        assert!(!alive(&mut host, lattice.metatable));
        assert!(!alive(&mut host, lattice.graph_first));
        assert!(!alive(&mut host, lattice.graph_second));
        assert_eq!(
            heap::with_heap(&mut host, |heap| {
                heap.next_guest(lattice.weak_keys, &Value::Null)
            })
            .expect("weak-key table"),
            None
        );
        heap::with_heap(&mut host, |heap| heap.leave_roots(lattice.frame)).expect("leave roots");
    });
}

#[test]
fn secondary_propagation_omission_makes_the_graph_assertion_fail() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let lattice = seed(&mut host, false);
        heap::with_heap(&mut host, |heap| {
            heap.arm_pending_finalizer_metatable_skip(true);
            Ok(())
        })
        .expect("arm omission control");

        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        let meter = heap::take_scan_meter(&mut host).expect("publish meter");
        assert_eq!(meter.pending_finalizers_selected, 1);
        assert_eq!(
            heap::with_heap(&mut host, |heap| {
                heap.userdata_key_value(lattice.weak_keys, lattice.userdata)
            })
            .expect("weak key"),
            Value::string("keys"),
            "the selected userdata itself stays protected"
        );
        assert_eq!(
            heap::borrow_published_userdata(&host, lattice.userdata).expect("token open"),
            lattice.userdata
        );

        finish_release(&mut host);
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.userdata_alive(lattice.userdata)))
                .expect("protected userdata"),
            "the omission control keeps the selected object"
        );
        assert!(userdata_token_open(&host, lattice.userdata_raw));
        assert!(!alive(&mut host, lattice.metatable));
        assert!(!alive(&mut host, lattice.graph_first));
        assert!(!alive(&mut host, lattice.graph_second));

        let graph_alive_assertion_failed = std::panic::catch_unwind(AssertUnwindSafe(|| {
            assert!(
                alive(&mut host, lattice.metatable),
                "the userdata metatable must stay alive"
            );
        }))
        .is_err();
        assert!(
            graph_alive_assertion_failed,
            "omitting the second propagation must fail the graph-alive assertion"
        );
        heap::with_heap(&mut host, |heap| heap.leave_roots(lattice.frame)).expect("leave roots");
    });
}
