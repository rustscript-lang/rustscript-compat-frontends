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
    ScanMeter, TableRef, UserdataRef,
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
    weak_key_values_self: TableRef,
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
    let (
        metatable,
        graph_first,
        graph_second,
        weak_values,
        weak_keys,
        weak_key_values,
        weak_key_values_self,
        orphan,
    ) = heap::with_heap(host, |heap| {
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

        // The same pending object is both key and value of one `kv`
        // entry. Its key role keeps the entry only if the value role can
        // be published; a bare userdata key cannot carry a cursor gap, so
        // the record must be dropped while scalar `k`/`kv` entries stay.
        let weak_key_values_self = heap.alloc()?;
        let self_mode = heap.alloc()?;
        heap.set_mode_scalar(self_mode, Value::string("kv"))?;
        heap.set_metatable(weak_key_values_self, Some(self_mode))?;
        heap.set_userdata_key_userdata(weak_key_values_self, userdata, userdata)?;

        Ok((
            metatable,
            graph_first,
            graph_second,
            weak_values,
            weak_keys,
            weak_key_values,
            weak_key_values_self,
            orphan,
        ))
    })
    .expect("lattice");
    let frame = heap::with_heap(host, |heap| {
        let frame = heap.enter_roots(4)?;
        heap.write_root(frame, 0, weak_values)?;
        heap.write_root(frame, 1, weak_keys)?;
        heap.write_root(frame, 2, weak_key_values)?;
        heap.write_root(frame, 3, weak_key_values_self)?;
        Ok(frame)
    })
    .expect("roots");
    Lattice {
        userdata,
        userdata_raw,
        weak_values,
        weak_keys,
        weak_key_values,
        weak_key_values_self,
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
        // The same object as key and value loses its value role. A bare
        // userdata key cannot carry a cursor gap, so the whole record is
        // dropped rather than keeping the retained object strongly.
        assert_eq!(
            heap::with_heap(&mut host, |heap| {
                heap.userdata_key_value(lattice.weak_key_values_self, lattice.userdata)
            })
            .expect("same-object value role"),
            Value::Null
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| {
                heap.next_guest(lattice.weak_key_values_self, &Value::Null)
            })
            .expect("same-object kv entry"),
            None
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
        assert_eq!(report.live_nodes, 12);
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
            heap::with_heap(&mut host, |heap| {
                heap.userdata_key_value(lattice.weak_key_values_self, lattice.userdata)
            })
            .expect("same-object value role"),
            Value::Null
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| {
                heap.next_guest(lattice.weak_key_values_self, &Value::Null)
            })
            .expect("same-object kv entry"),
            None
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
        assert_eq!(report.live_nodes, 12);
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
        assert_eq!(report.live_nodes, 8);
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
        assert_eq!(report.live_nodes, 12);
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

struct Eligible {
    userdata: UserdataRef,
    raw: u64,
    weak_values: TableRef,
}

/// One real typed userdata with its own token, a raw `__gc = false` metatable,
/// and one rooted weak-value table. The userdata itself is not rooted.
fn publish_eligible(host: &mut vm::HostContext<'_>) -> Eligible {
    let userdata = heap::publish_userdata(host).expect("eligible userdata");
    let raw = bound_raw(host, userdata);
    let weak_values = heap::with_heap(host, |heap| {
        let metatable = heap.alloc()?;
        heap.set_named_scalar(metatable, "__gc", Value::Bool(false))?;
        heap.set_userdata_metatable(userdata, Some(metatable))?;
        let weak_values = weak_table(heap, "v")?;
        heap.set_number_userdata(weak_values, 1.0, userdata)?;
        Ok(weak_values)
    })
    .expect("eligible lattice");
    Eligible {
        userdata,
        raw,
        weak_values,
    }
}

fn pending_flag(host: &mut vm::HostContext<'_>, userdata: UserdataRef) -> bool {
    heap::with_heap(host, |heap| heap.slot_pending_finalizer(userdata)).expect("pending flag")
}

/// A previous-cycle record, a new candidate verified before the failing item,
/// and a same-type misbinding. The private selection must remove only this
/// batch's new flags and records, leaving the earlier record and every raw
/// generation untouched. While the bad binding remains, an ordinary entry must
/// not retain or weakly publish from a fake record. Restoring the exact raw and
/// retrying in the same context selects the real finalizer and publishes.
#[test]
fn owner_verification_failure_rolls_back_the_new_batch_and_recovers() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");

        // Previous cycle: one genuine selected pending finalizer.
        let prior = publish_eligible(&mut host);
        let prior_frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(1)?;
            heap.write_root(frame, 0, prior.weak_values)?;
            Ok(frame)
        })
        .expect("prior roots");
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        finish_release(&mut host);
        assert_eq!(
            pending_records(&mut host),
            vec![(
                prior.raw,
                prior.userdata.slot(),
                prior.userdata.generation()
            )]
        );

        // This batch: C is selected and verified before A, whose slot is
        // temporarily bound to B's real same-type token.
        let b = heap::publish_userdata(&mut host).expect("rooted B");
        let b_raw = bound_raw(&mut host, b);
        let c = publish_eligible(&mut host);
        let a = publish_eligible(&mut host);
        assert!(
            c.userdata.slot() < a.userdata.slot(),
            "C must be verified before A"
        );
        let orphan = heap::with_heap(&mut host, |heap| heap.alloc()).expect("orphan table");
        let (_orphan_weak, frame) = heap::with_heap(&mut host, |heap| {
            let orphan_weak = weak_table(heap, "v")?;
            heap.set_number_edge(orphan_weak, 1.0, orphan)?;
            let frame = heap.enter_roots(4)?;
            heap.write_root(frame, 0, c.weak_values)?;
            heap.write_root(frame, 1, a.weak_values)?;
            heap.write_root(frame, 2, orphan_weak)?;
            heap.write_userdata_root(frame, 3, b)?;
            Ok((orphan_weak, frame))
        })
        .expect("batch roots");
        let a_generation = slot_generation(&mut host, a.userdata.slot());
        let b_generation = slot_generation(&mut host, b.slot());
        let c_generation = slot_generation(&mut host, c.userdata.slot());
        let prior_generation = slot_generation(&mut host, prior.userdata.slot());
        let stats_before = heap::stats(&mut host).expect("stats before");
        let resources_before = host.resource_count();

        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(a.userdata)?;
            heap.bind_userdata_token(a.userdata, b_raw)
        })
        .expect("same-type misbind");

        let _ = heap::take_scan_meter(&mut host);
        let error = heap::collect_pending_finalizers(&mut host).expect_err("foreign owner");
        assert_eq!(error, HeapError::TokenMismatch);
        let meter = heap::take_scan_meter(&mut host).expect("refusal meter");
        assert_eq!(meter.pending_finalizer_offers, 1, "only the earlier record");
        assert_eq!(
            meter.pending_finalizers_selected, 2,
            "both new candidates were selected before the failing verification"
        );
        assert_eq!(meter.token_closes, 0, "verification fails before close");

        // The whole new batch is gone; the earlier record is untouched.
        assert_eq!(
            pending_records(&mut host),
            vec![(prior.raw, prior.userdata.slot(), prior_generation)],
            "only the earlier-cycle record remains"
        );
        assert!(pending_flag(&mut host, prior.userdata));
        assert!(!pending_flag(&mut host, c.userdata));
        assert!(!pending_flag(&mut host, a.userdata));
        assert_eq!(slot_generation(&mut host, a.userdata.slot()), a_generation);
        assert_eq!(slot_generation(&mut host, c.userdata.slot()), c_generation);
        assert_eq!(slot_generation(&mut host, b.slot()), b_generation);
        assert_eq!(
            slot_generation(&mut host, prior.userdata.slot()),
            prior_generation
        );
        assert!(userdata_token_open(&host, a.raw));
        assert!(userdata_token_open(&host, c.raw));
        assert!(userdata_token_open(&host, b_raw));
        assert!(userdata_token_open(&host, prior.raw));
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.published_userdata_token(a.userdata))
                .expect("failing binding stays"),
            b_raw
        );
        assert_eq!(host.resource_count(), resources_before);
        assert_eq!(
            heap::stats(&mut host).expect("stats after").collections,
            stats_before.collections
        );
        assert!(!heap::saved_sweep_tail(&mut host).expect("no tail"));
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.number_userdata(a.weak_values, 1.0))
                .expect("A weak value"),
            Some(a.userdata),
            "no weak publication from a fake pending record"
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.number_userdata(c.weak_values, 1.0))
                .expect("C weak value"),
            Some(c.userdata)
        );
        assert!(alive(&mut host, orphan));

        // Ordinary entry while the bad binding is still present: the real
        // close verifier refuses the foreign owner, so no fake record can
        // retain A or C and no weak edge is published.
        let control = heap::collect(&mut host).expect_err("ordinary close rejects the misbind");
        assert_eq!(control, HeapError::TokenMismatch);
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.number_userdata(a.weak_values, 1.0))
                .expect("A weak value after control"),
            Some(a.userdata)
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.number_userdata(c.weak_values, 1.0))
                .expect("C weak value after control"),
            Some(c.userdata)
        );
        assert!(alive(&mut host, orphan));
        assert!(userdata_token_open(&host, a.raw));
        assert!(userdata_token_open(&host, b_raw));
        assert!(userdata_token_open(&host, prior.raw));
        assert!(!heap::saved_sweep_tail(&mut host).expect("no tail after control"));
        assert_eq!(
            pending_records(&mut host),
            vec![(prior.raw, prior.userdata.slot(), prior_generation)]
        );

        // Restore A's exact raw and retry in the same HostContext. C's token
        // was already closed by the ordinary control, so C is ordinary dead
        // garbage here; A is the real new finalizer.
        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(a.userdata)?;
            heap.bind_userdata_token(a.userdata, a.raw)
        })
        .expect("restore A");
        let _ = heap::take_scan_meter(&mut host);
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        let meter = heap::take_scan_meter(&mut host).expect("retry meter");
        assert_eq!(meter.pending_finalizer_offers, 1);
        assert_eq!(meter.pending_finalizers_selected, 1);
        assert_eq!(meter.token_closes, 0);
        assert_eq!(
            pending_records(&mut host),
            vec![
                (prior.raw, prior.userdata.slot(), prior_generation),
                (a.raw, a.userdata.slot(), a_generation)
            ]
        );
        assert!(pending_flag(&mut host, a.userdata));
        assert!(userdata_token_open(&host, a.raw));
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.number_userdata(a.weak_values, 1.0))
                .expect("A weak value after retry"),
            None,
            "the real weak publication clears A's weak value"
        );
        let tail = heap::saved_tail_state(&mut host)
            .expect("tail state")
            .expect("release tail");
        assert_eq!(tail.phase, heap::SavedTailPhase::Release);
        assert_eq!(tail.dead_len, 3, "C, C's metatable, and the orphan");

        finish_release(&mut host);
        assert!(!alive(&mut host, orphan));
        assert_eq!(
            slot_generation(&mut host, orphan.slot()),
            orphan.generation() + 1
        );
        assert!(userdata_token_open(&host, a.raw));
        assert_eq!(slot_generation(&mut host, a.userdata.slot()), a_generation);
        assert_eq!(
            pending_records(&mut host),
            vec![
                (prior.raw, prior.userdata.slot(), prior_generation),
                (a.raw, a.userdata.slot(), a_generation)
            ]
        );
        heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave batch roots");
        heap::with_heap(&mut host, |heap| heap.leave_roots(prior_frame))
            .expect("leave prior roots");
        assert!(!alive(&mut host, orphan));
    });
}

/// Release one ordinary dead slot from a real published tail while several
/// genuine pending records exist. The only measured work is the release path
/// itself; the returned count is the real number of pending-queue records the
/// old per-dead-slot loop visited.
fn release_queue_reads_for(pending_count: usize) -> u64 {
    let mut reads = None;
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let mut eligible = Vec::new();
        for _ in 0..pending_count {
            eligible.push(publish_eligible(&mut host));
        }
        let frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots((pending_count + 1) as u32)?;
            for (index, item) in eligible.iter().enumerate() {
                heap.write_root(frame, index as u32, item.weak_values)?;
            }
            Ok(frame)
        })
        .expect("pending roots");
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        finish_release(&mut host);
        assert_eq!(pending_records(&mut host).len(), pending_count);

        let dead = heap::with_heap(&mut host, |heap| heap.alloc()).expect("dead table");
        let _dead_weak = heap::with_heap(&mut host, |heap| {
            let weak = weak_table(heap, "v")?;
            heap.set_number_edge(weak, 1.0, dead)?;
            heap.write_root(frame, pending_count as u32, weak)?;
            Ok(weak)
        })
        .expect("dead weak table");
        let dead_generation = slot_generation(&mut host, dead.slot());
        let pending_generations: Vec<u32> = eligible
            .iter()
            .map(|item| slot_generation(&mut host, item.userdata.slot()))
            .collect();

        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        let before = heap::saved_tail_state(&mut host)
            .expect("tail state")
            .expect("release tail");
        assert_eq!(before.phase, heap::SavedTailPhase::Release);
        assert_eq!(before.cursor, 0);
        assert_eq!(before.dead_len, 1);

        let _ = heap::take_scan_meter(&mut host);
        let resumed = heap::resume_published_sweep(&mut host, 1).expect("one release unit");
        assert!(
            matches!(resumed, heap::PublishedResume::Paused),
            "one unit releases one slot and leaves the tail saved"
        );
        let meter = heap::take_scan_meter(&mut host).expect("release meter");
        let after = heap::saved_tail_state(&mut host)
            .expect("tail state")
            .expect("release tail");
        assert_eq!(after.phase, heap::SavedTailPhase::Release);
        assert_eq!(after.cursor, 1, "exactly one release unit was spent");
        assert_eq!(
            slot_generation(&mut host, dead.slot()),
            dead_generation + 1,
            "the ordinary dead slot advances exactly one generation"
        );
        for (item, generation) in eligible.iter().zip(pending_generations) {
            assert_eq!(
                slot_generation(&mut host, item.userdata.slot()),
                generation,
                "a pending slot keeps its generation"
            );
            assert!(pending_flag(&mut host, item.userdata));
        }
        assert_eq!(pending_records(&mut host).len(), pending_count);
        assert!(!alive(&mut host, dead));
        reads = Some(meter.pending_release_queue_reads);
    });
    reads.expect("release arm")
}

#[test]
fn release_of_an_ordinary_dead_slot_reads_no_pending_queue() {
    let two = release_queue_reads_for(2);
    let five = release_queue_reads_for(5);
    assert_eq!(
        (two, five),
        (0, 0),
        "Release must not traverse the pending queue per dead slot; the count grew with the queue"
    );
}

#[test]
fn pending_slot_forced_into_reclaim_is_refused_before_weak_publication() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let lattice = seed(&mut host, false);
        publish_boundary(&mut host);
        finish_release(&mut host);
        let eligible = publish_eligible(&mut host);
        let frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(1)?;
            heap.write_root(frame, 0, eligible.weak_values)?;
            Ok(frame)
        })
        .expect("eligible root");
        let generation = slot_generation(&mut host, lattice.userdata.slot());
        heap::with_heap(&mut host, |heap| {
            heap.arm_pending_finalizer_reclaim_control(lattice.userdata)
        })
        .expect("arm reclaim control");
        let _ = heap::take_scan_meter(&mut host);

        let refused = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _ = heap::collect_pending_finalizers(&mut host);
        }))
        .is_err();
        assert!(
            refused,
            "shared Select must reject a true pending slot before Snapshot or Owners"
        );
        let meter = heap::take_scan_meter(&mut host).expect("refusal meter");
        assert_eq!(meter.token_closes, 0, "refusal is before any close");
        assert!(!heap::saved_sweep_tail(&mut host).expect("no tail"));
        assert_eq!(
            heap::with_heap(&mut host, |heap| {
                heap.number_userdata(eligible.weak_values, 1.0)
            })
            .expect("candidate weak value"),
            Some(eligible.userdata),
            "no weak edge is published before the bounded check refuses"
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| {
                heap.userdata_key_value(lattice.weak_key_values, lattice.userdata)
            })
            .expect("retained scalar"),
            Value::Float(7.5)
        );
        assert!(pending_flag(&mut host, lattice.userdata));
        assert!(pending_flag(&mut host, eligible.userdata));
        assert_eq!(
            slot_generation(&mut host, lattice.userdata.slot()),
            generation
        );
        assert!(userdata_token_open(&host, lattice.userdata_raw));
        assert!(userdata_token_open(&host, eligible.raw));
        heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave eligible root");
        heap::with_heap(&mut host, |heap| heap.leave_roots(lattice.frame)).expect("leave lattice");
    });
}

/// Builds a metatable with `length` named records. `gc_at` places the raw
/// `__gc = false` record at that position in insertion order.
fn raw_gc_chain(
    heap: &mut heap::LuaHeap,
    length: usize,
    gc_at: Option<usize>,
) -> Result<TableRef, HeapError> {
    let metatable = heap.alloc()?;
    for position in 0..length {
        if gc_at == Some(position) {
            heap.set_named_scalar(metatable, "__gc", Value::Bool(false))?;
        } else {
            let name = format!("f{position}");
            heap.set_named_scalar(metatable, &name, Value::string(format!("v{position}")))?;
        }
    }
    Ok(metatable)
}

/// One eligible userdata on a chain metatable. Returns the real meter, the
/// real per-record trace, whether the original token stayed open, and the
/// metatable slot.
fn metered_chain_arm(
    length: usize,
    gc_at: Option<usize>,
) -> (ScanMeter, Vec<(u32, u32)>, bool, u32) {
    let mut out = None;
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let userdata = heap::publish_userdata(&mut host).expect("userdata");
        let raw = bound_raw(&mut host, userdata);
        let metatable_slot = heap::with_heap(&mut host, |heap| {
            let metatable = raw_gc_chain(heap, length, gc_at)?;
            heap.set_userdata_metatable(userdata, Some(metatable))?;
            let weak = weak_table(heap, "v")?;
            heap.set_number_userdata(weak, 1.0, userdata)?;
            let frame = heap.enter_roots(1)?;
            heap.write_root(frame, 0, weak)?;
            Ok(metatable.slot())
        })
        .expect("chain lattice");
        heap::with_heap(&mut host, |heap| {
            heap.clear_pending_gc_edge_trace();
            Ok(())
        })
        .expect("clear trace");
        let _ = heap::take_scan_meter(&mut host);
        let _ = publish_boundary(&mut host);
        let meter = heap::take_scan_meter(&mut host).expect("chain meter");
        let trace = heap::with_heap(&mut host, |heap| Ok(heap.pending_gc_edge_trace()))
            .expect("chain trace");
        out = Some((
            meter,
            trace,
            userdata_token_open(&host, raw),
            metatable_slot,
        ));
    });
    out.expect("chain arm")
}

#[test]
fn raw_gc_lookup_reads_one_edge_record_per_unit_at_chain_head_and_tail() {
    let (meter, trace, token_open, metatable_slot) = metered_chain_arm(24, Some(0));
    assert_eq!(meter.pending_finalizers_selected, 1);
    assert_eq!(meter.pending_finalizer_gc_lookups, 1);
    assert_eq!(
        meter.pending_finalizer_gc_edge_checks, 1,
        "the head record is the only one read when __gc is first"
    );
    assert_eq!(trace.len(), 1);
    assert_eq!(trace[0].0, metatable_slot);
    assert!(token_open);

    let (meter, trace, token_open, metatable_slot) = metered_chain_arm(24, Some(23));
    assert_eq!(meter.pending_finalizers_selected, 1);
    assert_eq!(meter.pending_finalizer_gc_lookups, 1);
    assert_eq!(
        meter.pending_finalizer_gc_edge_checks, 24,
        "a tail __gc costs one charged record per chain entry"
    );
    assert_eq!(trace.len(), 24);
    assert!(
        trace.iter().all(|(slot, _)| *slot == metatable_slot),
        "every charged read belongs to the candidate's metatable"
    );
    assert!(token_open);
}

#[test]
fn raw_gc_missing_or_nil_on_a_long_chain_is_not_selected() {
    let (meter, trace, token_open, _) = metered_chain_arm(8, None);
    assert_eq!(meter.pending_finalizers_selected, 0);
    assert_eq!(meter.pending_finalizer_gc_lookups, 1);
    assert_eq!(meter.pending_finalizer_gc_edge_checks, 8);
    assert_eq!(trace.len(), 8);
    assert!(!token_open);

    let mut nil_meter = None;
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let userdata = heap::publish_userdata(&mut host).expect("userdata");
        let raw = bound_raw(&mut host, userdata);
        heap::with_heap(&mut host, |heap| {
            let metatable = raw_gc_chain(heap, 8, Some(7))?;
            heap.set_named_scalar(metatable, "__gc", Value::Null)?;
            heap.set_userdata_metatable(userdata, Some(metatable))?;
            let weak = weak_table(heap, "v")?;
            heap.set_number_userdata(weak, 1.0, userdata)?;
            let frame = heap.enter_roots(1)?;
            heap.write_root(frame, 0, weak)?;
            Ok(())
        })
        .expect("nil lattice");
        let _ = heap::take_scan_meter(&mut host);
        let _ = publish_boundary(&mut host);
        let meter = heap::take_scan_meter(&mut host).expect("nil meter");
        assert_eq!(meter.pending_finalizers_selected, 0, "nil is not selected");
        assert_eq!(
            meter.pending_finalizer_gc_edge_checks, 7,
            "deleting the raw record shortens the charged chain"
        );
        assert!(!userdata_token_open(&host, raw));
        nil_meter = Some(());
    });
    nil_meter.expect("nil arm");
}

#[test]
fn raw_gc_stored_cursor_gap_on_a_long_chain_is_not_selected() {
    let mut receipt = None;
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let userdata = heap::publish_userdata(&mut host).expect("userdata");
        let raw = bound_raw(&mut host, userdata);
        let metatable_slot = heap::with_heap(&mut host, |heap| {
            let metatable = raw_gc_chain(heap, 8, Some(5))?;
            heap.set_named_gap_test_control(metatable, "__gc")?;
            heap.set_userdata_metatable(userdata, Some(metatable))?;
            let weak = weak_table(heap, "v")?;
            heap.set_number_userdata(weak, 1.0, userdata)?;
            let frame = heap.enter_roots(1)?;
            heap.write_root(frame, 0, weak)?;
            Ok(metatable.slot())
        })
        .expect("gap lattice");
        heap::with_heap(&mut host, |heap| {
            heap.clear_pending_gc_edge_trace();
            Ok(())
        })
        .expect("clear trace");
        let _ = heap::take_scan_meter(&mut host);
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        let meter = heap::take_scan_meter(&mut host).expect("gap meter");
        let trace =
            heap::with_heap(&mut host, |heap| Ok(heap.pending_gc_edge_trace())).expect("gap trace");
        assert_eq!(
            meter.pending_finalizers_selected, 0,
            "a stored cursor gap is not a raw __gc value"
        );
        assert_eq!(meter.pending_finalizer_gc_lookups, 1);
        assert_eq!(
            meter.pending_finalizer_gc_edge_checks, 6,
            "the lookup stops at the stored gap and reads only records before it"
        );
        assert_eq!(trace.len(), 6);
        assert!(trace.iter().all(|(slot, _)| *slot == metatable_slot));
        assert_eq!(meter.token_closes, 1);
        assert!(!userdata_token_open(&host, raw));
        receipt = Some(());
    });
    receipt.expect("gap arm");
}

#[test]
fn shared_metatable_raw_gc_chain_is_read_once_for_multiple_candidates() {
    let mut receipt = None;
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let first = heap::publish_userdata(&mut host).expect("first");
        let first_raw = bound_raw(&mut host, first);
        let second = heap::publish_userdata(&mut host).expect("second");
        let second_raw = bound_raw(&mut host, second);
        let metatable_slot = heap::with_heap(&mut host, |heap| {
            let metatable = raw_gc_chain(heap, 24, Some(23))?;
            let first_weak = weak_table(heap, "v")?;
            heap.set_number_userdata(first_weak, 1.0, first)?;
            let second_weak = weak_table(heap, "v")?;
            heap.set_number_userdata(second_weak, 1.0, second)?;
            heap.set_userdata_metatable(first, Some(metatable))?;
            heap.set_userdata_metatable(second, Some(metatable))?;
            let frame = heap.enter_roots(2)?;
            heap.write_root(frame, 0, first_weak)?;
            heap.write_root(frame, 1, second_weak)?;
            Ok(metatable.slot())
        })
        .expect("shared metatable lattice");
        heap::with_heap(&mut host, |heap| {
            heap.clear_pending_gc_edge_trace();
            Ok(())
        })
        .expect("clear trace");
        let _ = heap::take_scan_meter(&mut host);
        let _ = publish_boundary(&mut host);
        let meter = heap::take_scan_meter(&mut host).expect("shared meter");
        let trace = heap::with_heap(&mut host, |heap| Ok(heap.pending_gc_edge_trace()))
            .expect("shared trace");
        assert_eq!(meter.pending_finalizers_selected, 2);
        assert_eq!(
            meter.pending_finalizer_gc_lookups, 1,
            "the second candidate reuses the first round's metatable result"
        );
        assert_eq!(
            meter.pending_finalizer_gc_edge_checks, 24,
            "a shared metatable is not read once per candidate"
        );
        assert_eq!(trace.len(), 24);
        assert!(trace.iter().all(|(slot, _)| *slot == metatable_slot));
        assert!(userdata_token_open(&host, first_raw));
        assert!(userdata_token_open(&host, second_raw));
        receipt = Some(());
    });
    receipt.expect("shared arm");
}

struct LinkedReceipt {
    first_selected: u64,
    a_token_open: bool,
    b_token_open: bool,
    a_value_cleared: bool,
    b_value_kept_after_first: bool,
    records_after: usize,
}

/// A's metatable strongly references B. Both userdata have independent raw
/// `__gc = false` metatables. `b_first` fixes the userdata slot order; `root_b`
/// gives B an ordinary strong root for the selecting round.
fn linked_pair(b_first: bool, root_b: bool) -> LinkedReceipt {
    let mut receipt = None;
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let (a, a_raw, b, b_raw) = if b_first {
            let b = heap::publish_userdata(&mut host).expect("B");
            let b_raw = bound_raw(&mut host, b);
            let a = heap::publish_userdata(&mut host).expect("A");
            let a_raw = bound_raw(&mut host, a);
            (a, a_raw, b, b_raw)
        } else {
            let a = heap::publish_userdata(&mut host).expect("A");
            let a_raw = bound_raw(&mut host, a);
            let b = heap::publish_userdata(&mut host).expect("B");
            let b_raw = bound_raw(&mut host, b);
            (a, a_raw, b, b_raw)
        };
        let (a_weak, b_weak) = heap::with_heap(&mut host, |heap| {
            let a_metatable = raw_gc_chain(heap, 1, Some(0))?;
            let b_metatable = raw_gc_chain(heap, 1, Some(0))?;
            heap.set_named_userdata(a_metatable, "graph", b)?;
            heap.set_userdata_metatable(a, Some(a_metatable))?;
            heap.set_userdata_metatable(b, Some(b_metatable))?;
            let a_weak = weak_table(heap, "v")?;
            heap.set_number_userdata(a_weak, 1.0, a)?;
            let b_weak = weak_table(heap, "v")?;
            heap.set_number_userdata(b_weak, 1.0, b)?;
            let weak_frame = heap.enter_roots(2)?;
            heap.write_root(weak_frame, 0, a_weak)?;
            heap.write_root(weak_frame, 1, b_weak)?;
            if root_b {
                let b_frame = heap.enter_roots(1)?;
                heap.write_userdata_root(b_frame, 0, b)?;
            }
            Ok((a_weak, b_weak))
        })
        .expect("linked pair lattice");

        let _ = heap::take_scan_meter(&mut host);
        let _ = publish_boundary(&mut host);
        let first = heap::take_scan_meter(&mut host).expect("first linked meter");
        let a_value_cleared = heap::with_heap(&mut host, |heap| heap.number_userdata(a_weak, 1.0))
            .expect("A weak value")
            .is_none();
        let b_after_first = heap::with_heap(&mut host, |heap| heap.number_userdata(b_weak, 1.0))
            .expect("B weak value");
        let a_open = userdata_token_open(&host, a_raw);
        let b_open = userdata_token_open(&host, b_raw);
        let records_after = pending_records(&mut host).len();
        finish_release(&mut host);

        receipt = Some(LinkedReceipt {
            first_selected: first.pending_finalizers_selected,
            a_token_open: a_open,
            b_token_open: b_open,
            a_value_cleared,
            b_value_kept_after_first: b_after_first == Some(b),
            records_after,
        });
    });
    receipt.expect("linked pair arm")
}

#[test]
fn linked_candidates_select_in_both_slot_orders_and_respect_a_rooted_target() {
    for b_first in [false, true] {
        let both = linked_pair(b_first, false);
        assert_eq!(
            both.first_selected, 2,
            "the whole pending batch is scanned before Gray, so both linked candidates select"
        );
        assert!(both.a_token_open && both.b_token_open);
        assert!(both.a_value_cleared && !both.b_value_kept_after_first);
        assert_eq!(both.records_after, 2);

        let rooted = linked_pair(b_first, true);
        assert_eq!(
            rooted.first_selected, 1,
            "a rooted B is already marked from the ordinary roots"
        );
        assert!(rooted.a_token_open && rooted.b_token_open);
        assert!(rooted.a_value_cleared, "A publishes its real weak clear");
        assert!(
            rooted.b_value_kept_after_first,
            "B's weak value survives while B has an ordinary strong root"
        );
        assert_eq!(rooted.records_after, 1);
    }
}
