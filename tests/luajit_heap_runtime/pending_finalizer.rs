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

/// The panic text of a caught shared-Select refusal. The private heap uses
/// string payloads for its exact diagnostics.
fn panic_text(result: std::thread::Result<()>) -> String {
    match result {
        Ok(()) => panic!("expected the shared Select to refuse the pending slot"),
        Err(payload) => {
            if let Some(text) = payload.downcast_ref::<String>() {
                text.clone()
            } else if let Some(text) = payload.downcast_ref::<&'static str>() {
                (*text).to_owned()
            } else {
                panic!("panic payload was not a string")
            }
        }
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

        let slot = lattice.userdata.slot();
        let message = panic_text(std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _ = heap::collect_pending_finalizers(&mut host);
        })));
        assert_eq!(
            message,
            format!(
                "lua heap sweep selected pending-finalizer slot {slot} as dead; \
                 the mark must retain every live pending record"
            ),
            "the shared Select must refuse the pending slot with its own diagnostic, \
             not with the adjacent bound-token assertion"
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

/// One `pulse(1)` of the real selecting mark.
///
/// `edges` is the delta between the cumulative pending-`__gc` edge meter
/// before and after this one call. The before/after values are kept so a
/// failure shows the real access boundary, not only a sum after a drain.
struct PulseStep {
    finished: bool,
    before_edges: u64,
    after_edges: u64,
    edges: u64,
    scan_slots: u64,
    lookups: u64,
    selected: u64,
    owner_heads: u64,
    outgoing_edges: u64,
}

/// Drives the private selecting mark one charged unit at a time. Each call
/// records the real edge meter at its own start and end.
fn pulse_steps(heap: &mut heap::LuaHeap, limit: usize) -> (Vec<PulseStep>, ScanMeter) {
    let mut session = heap::SelectingPendingMark::start(heap);
    let mut previous = session.meter();
    let mut steps = Vec::new();
    for _ in 0..limit {
        let before = previous;
        let (finished, after) = session.pulse(1);
        steps.push(PulseStep {
            finished,
            before_edges: before.pending_finalizer_gc_edge_checks,
            after_edges: after.pending_finalizer_gc_edge_checks,
            edges: after.pending_finalizer_gc_edge_checks - before.pending_finalizer_gc_edge_checks,
            scan_slots: after.pending_finalizer_scan_slots - before.pending_finalizer_scan_slots,
            lookups: after.pending_finalizer_gc_lookups - before.pending_finalizer_gc_lookups,
            selected: after.pending_finalizers_selected - before.pending_finalizers_selected,
            owner_heads: after.owner_head_checks - before.owner_head_checks,
            outgoing_edges: after.outgoing_edge_checks - before.outgoing_edge_checks,
        });
        previous = after;
        if finished {
            return (steps, after);
        }
    }
    panic!("selecting mark did not finish within {limit} pulses");
}

/// Checks the one-record-per-call boundary on a real pulse trace. A lookup
/// start and a cursor exhaustion each spend their pulse unit without reading
/// an edge record; the edge meter, not the budget, counts real reads, so
/// neither may hide a fabricated one.
fn assert_single_edge_per_pulse(steps: &[PulseStep], expected_edges: u64) {
    let mut starts = 0;
    let mut total = 0;
    for (index, step) in steps.iter().enumerate() {
        assert_eq!(
            step.after_edges - step.before_edges,
            step.edges,
            "pulse {index} lost its real before/after edge delta"
        );
        assert!(
            step.edges <= 1,
            "pulse {index} read {} pending-__gc edge records; one pulse spends one unit",
            step.edges
        );
        if step.lookups == 1 {
            starts += 1;
            assert_eq!(
                step.edges, 0,
                "pulse {index} opened a lookup and must not also read a record"
            );
        }
        if step.edges == 1 {
            assert_eq!(
                step.lookups, 0,
                "pulse {index} read a record while opening a lookup"
            );
        }
        total += step.edges;
    }
    assert_eq!(
        starts, 1,
        "one uncached metatable starts exactly one lookup"
    );
    assert_eq!(
        total, expected_edges,
        "the real reads cover the chain prefix"
    );
}

struct PulsedChain {
    steps: Vec<PulseStep>,
    meter: ScanMeter,
    metatable_slot: u32,
    trace: Vec<(u32, u32)>,
    token_open: bool,
    pending: bool,
    records: usize,
}

/// A real 24-record metatable chain with `__gc` at `gc_at`, driven by the
/// private selecting mark through `pulse(1)`.
fn pulsed_chain_arm(length: usize, gc_at: Option<usize>) -> PulsedChain {
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
        let (steps, meter, trace, pending, records) = heap::with_heap(&mut host, |heap| {
            let _ = heap.take_scan_meter();
            let (steps, meter) = pulse_steps(heap, 20_000);
            let trace = heap.pending_gc_edge_trace();
            let pending = heap.slot_pending_finalizer(userdata)?;
            let records = heap.pending_finalizer_records().len();
            Ok((steps, meter, trace, pending, records))
        })
        .expect("pulse chain");
        out = Some(PulsedChain {
            steps,
            meter,
            metatable_slot,
            trace,
            token_open: userdata_token_open(&host, raw),
            pending,
            records,
        });
    });
    out.expect("pulsed chain arm")
}

#[test]
fn raw_gc_pulses_read_at_most_one_edge_record_at_chain_head_and_tail() {
    let head = pulsed_chain_arm(24, Some(0));
    assert_single_edge_per_pulse(&head.steps, 1);
    assert_eq!(head.meter.pending_finalizers_selected, 1);
    assert_eq!(head.meter.pending_finalizer_gc_lookups, 1);
    assert_eq!(head.meter.pending_finalizer_gc_edge_checks, 1);
    assert_eq!(head.trace.len(), 1);
    assert!(
        head.trace
            .iter()
            .all(|(slot, _)| *slot == head.metatable_slot)
    );
    assert!(head.pending && head.token_open && head.records == 1);
    let start = head
        .steps
        .iter()
        .position(|step| step.lookups == 1)
        .expect("head lookup start");
    assert_eq!(head.steps[start].edges, 0);
    assert_eq!(head.steps[start].selected, 0);
    assert_eq!(head.steps[start + 1].edges, 1);
    assert_eq!(head.steps[start + 1].selected, 1);

    let tail = pulsed_chain_arm(24, Some(23));
    assert_single_edge_per_pulse(&tail.steps, 24);
    assert_eq!(tail.meter.pending_finalizers_selected, 1);
    assert_eq!(tail.meter.pending_finalizer_gc_lookups, 1);
    assert_eq!(tail.meter.pending_finalizer_gc_edge_checks, 24);
    assert_eq!(tail.trace.len(), 24);
    assert!(
        tail.trace
            .iter()
            .all(|(slot, _)| *slot == tail.metatable_slot)
    );
    assert!(tail.pending && tail.token_open && tail.records == 1);
    assert_eq!(
        tail.steps.iter().filter(|step| step.edges == 1).count(),
        24,
        "each chain record is read by its own pulse"
    );
    let last_edge = tail
        .steps
        .iter()
        .rposition(|step| step.edges == 1)
        .expect("tail edge read");
    assert_eq!(
        tail.steps[last_edge].selected, 1,
        "the matching tail record commits on the same one-record pulse"
    );
}

#[test]
fn raw_gc_pulse_exhaustion_of_a_missing_chain_reads_no_edge() {
    let missing = pulsed_chain_arm(24, None);
    assert_single_edge_per_pulse(&missing.steps, 24);
    assert_eq!(missing.meter.pending_finalizers_selected, 0);
    assert_eq!(missing.meter.pending_finalizer_gc_lookups, 1);
    assert_eq!(missing.meter.pending_finalizer_gc_edge_checks, 24);
    assert_eq!(missing.trace.len(), 24);
    assert!(
        missing
            .trace
            .iter()
            .all(|(slot, _)| *slot == missing.metatable_slot)
    );
    assert!(!missing.pending && missing.token_open && missing.records == 0);
    let last_edge = missing
        .steps
        .iter()
        .rposition(|step| step.edges == 1)
        .expect("missing chain edge read");
    let exhaustion = &missing.steps[last_edge + 1];
    assert_eq!(exhaustion.edges, 0, "running off the chain reads no record");
    assert_eq!(exhaustion.lookups, 0, "exhaustion starts no second lookup");
    assert_eq!(exhaustion.scan_slots, 0, "exhaustion is not a slot unit");
    assert_eq!(exhaustion.selected, 0, "a missing chain selects nothing");
}

struct PulsedPair {
    steps: Vec<PulseStep>,
    meter: ScanMeter,
    metatable_slot: u32,
    first_slot: u32,
    second_slot: u32,
    last_slot: u32,
    trace: Vec<(u32, u32)>,
    first_open: bool,
    second_open: bool,
    first_pending: bool,
    second_pending: bool,
    records: usize,
}

/// Two userdata that share one 24-record metatable and occupy the final two
/// slots. Both survive to the selection; the second is a same-round cache hit.
fn pulsed_shared_pair_arm() -> PulsedPair {
    let mut out = None;
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let (metatable, first_weak, second_weak) = heap::with_heap(&mut host, |heap| {
            let metatable = raw_gc_chain(heap, 24, Some(23))?;
            let first_weak = weak_table(heap, "v")?;
            let second_weak = weak_table(heap, "v")?;
            Ok((metatable, first_weak, second_weak))
        })
        .expect("shared tables");
        let first = heap::publish_userdata(&mut host).expect("first");
        let first_raw = bound_raw(&mut host, first);
        let second = heap::publish_userdata(&mut host).expect("second");
        let second_raw = bound_raw(&mut host, second);
        let (first_slot, second_slot, last_slot) = heap::with_heap(&mut host, |heap| {
            heap.set_userdata_metatable(first, Some(metatable))?;
            heap.set_userdata_metatable(second, Some(metatable))?;
            heap.set_number_userdata(first_weak, 1.0, first)?;
            heap.set_number_userdata(second_weak, 1.0, second)?;
            let frame = heap.enter_roots(2)?;
            heap.write_root(frame, 0, first_weak)?;
            heap.write_root(frame, 1, second_weak)?;
            Ok((first.slot(), second.slot(), heap.stats().node_slots))
        })
        .expect("shared lattice");
        let (steps, meter, trace, first_pending, second_pending, records) =
            heap::with_heap(&mut host, |heap| {
                let _ = heap.take_scan_meter();
                let (steps, meter) = pulse_steps(heap, 40_000);
                let trace = heap.pending_gc_edge_trace();
                let first_pending = heap.slot_pending_finalizer(first)?;
                let second_pending = heap.slot_pending_finalizer(second)?;
                let records = heap.pending_finalizer_records().len();
                Ok((steps, meter, trace, first_pending, second_pending, records))
            })
            .expect("shared pulses");
        out = Some(PulsedPair {
            steps,
            meter,
            metatable_slot: metatable.slot(),
            first_slot,
            second_slot,
            last_slot,
            trace,
            first_open: userdata_token_open(&host, first_raw),
            second_open: userdata_token_open(&host, second_raw),
            first_pending,
            second_pending,
            records,
        });
    });
    out.expect("pulsed shared pair arm")
}

#[test]
fn raw_gc_pulse_shared_metatable_hits_cache_without_an_edge_read() {
    let pair = pulsed_shared_pair_arm();
    assert_single_edge_per_pulse(&pair.steps, 24);
    assert_eq!(pair.meter.pending_finalizers_selected, 2);
    assert_eq!(pair.meter.pending_finalizer_gc_lookups, 1);
    assert_eq!(pair.meter.pending_finalizer_gc_edge_checks, 24);
    assert_eq!(pair.trace.len(), 24);
    assert!(
        pair.trace
            .iter()
            .all(|(slot, _)| *slot == pair.metatable_slot)
    );
    assert!(pair.first_open && pair.second_open);
    assert!(pair.first_pending && pair.second_pending);
    assert_eq!(pair.records, 2);
    assert_eq!(
        pair.last_slot,
        pair.second_slot + 1,
        "the two candidates are the final slots"
    );
    assert_eq!(pair.first_slot + 1, pair.second_slot);

    let pending_start = pair
        .steps
        .iter()
        .position(|step| step.scan_slots > 0)
        .expect("pending scan start");
    let first_selected = pair
        .steps
        .iter()
        .position(|step| step.selected > 0)
        .expect("first candidate selected");
    let second_selected = pair
        .steps
        .iter()
        .enumerate()
        .skip(first_selected + 1)
        .find(|(_, step)| step.selected > 0)
        .map(|(index, _)| index)
        .expect("second candidate selected");
    assert!(pending_start < first_selected && first_selected < second_selected);
    assert!(
        pair.steps[pending_start..=second_selected]
            .iter()
            .all(|step| step.owner_heads == 0),
        "the complete candidate batch is read before Gray opens an owner"
    );
    assert_eq!(pair.steps[second_selected].scan_slots, 1);
    assert_eq!(
        pair.steps[second_selected].lookups, 0,
        "the shared metatable already has a result for this round"
    );
    assert_eq!(
        pair.steps[second_selected].edges, 0,
        "a same-generation cache hit reads no edge record"
    );
    assert_eq!(pair.steps[second_selected].selected, 1);
    assert_eq!(
        pair.steps.iter().filter(|step| step.edges == 1).count(),
        24,
        "the shared chain is paid for once, one record per pulse"
    );
    let after: Vec<&PulseStep> = pair.steps[second_selected + 1..].iter().collect();
    assert!(
        after
            .iter()
            .all(|step| step.edges == 0 && step.lookups == 0),
        "the cache hit and the following Gray pass fabricate no pending edge read"
    );
    assert!(
        after.iter().any(|step| step.owner_heads > 0),
        "Gray runs only after both candidates were selected"
    );
    assert!(
        after.iter().any(|step| step.outgoing_edges > 0),
        "the Gray pass scans the selected metatable's own edges"
    );
    assert!(pair.steps.last().expect("shared last pulse").finished);
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

// -- White userdata with no raw `__gc` -------------------------------------
//
// The upstream `lj_gc_separateudata` pass runs before the second mark. A white
// userdata whose metatable has no raw `__gc` is marked finalized but is not
// put on the mmudata queue. The finalized flag has two consequences:
//   * a later raw `__gc` installed on the same metatable never queues that
//     generation (LuaJIT collects it as ordinary garbage); and
//   * while the object is alive through another edge, weak *values* naming it
//     are cleared even though weak *keys* are kept.
// The Rust slice keeps the existing candidate filter: a live, unmarked,
// token-bound userdata with a live table metatable. A userdata with no
// metatable is not widened into this slice; it has no raw `__gc` site here.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SettlementArm {
    /// B is only reachable through A's metatable and has no raw `__gc`.
    Linked,
    /// B has an ordinary strong root in the first round and no raw `__gc`.
    OrdinaryRoot,
    /// B has a raw non-nil `__gc` field before the first round.
    Preinstalled,
}

struct SettlementSeed {
    a: UserdataRef,
    a_raw: u64,
    a_metatable: TableRef,
    a_graph: TableRef,
    a_graph_tail: TableRef,
    b: UserdataRef,
    b_raw: u64,
    b_metatable: TableRef,
    b_graph: TableRef,
    b_graph_tail: TableRef,
    orphan: TableRef,
    weak_values: TableRef,
    weak_keys: TableRef,
    weak_frame: RootFrameId,
    b_strong_frame: Option<RootFrameId>,
}

fn settlement_limits() -> HeapLimits {
    HeapLimits {
        nodes: 64,
        roots: 8,
        edges: 128,
        max_generation: 8,
    }
}

fn finalizer_settled(host: &mut vm::HostContext<'_>, userdata: UserdataRef) -> bool {
    heap::with_heap(host, |heap| heap.slot_finalizer_settled(userdata)).expect("settled flag")
}

fn weak_value(host: &mut vm::HostContext<'_>, table: TableRef, key: f64) -> Option<UserdataRef> {
    heap::with_heap(host, |heap| heap.number_userdata(table, key)).expect("weak value")
}

fn weak_key(host: &mut vm::HostContext<'_>, table: TableRef, key: UserdataRef) -> Value {
    heap::with_heap(host, |heap| heap.userdata_key_value(table, key)).expect("weak key")
}

/// A's metatable has a raw `__gc = false` and strongly links B. B's metatable
/// is empty for the linked and ordinary-root arms, and holds the same raw
/// `false` for the preinstalled arm. Neither B nor its metatable is rooted in
/// the linked and preinstalled arms. The weak tables are the only roots; the
/// ordinary-root arm adds one separate B root that its test releases later.
fn seed_settlement(
    host: &mut vm::HostContext<'_>,
    arm: SettlementArm,
    b_first: bool,
) -> SettlementSeed {
    heap::install(host, settlement_limits()).expect("install");
    let (a, a_raw, b, b_raw) = if b_first {
        let b = heap::publish_userdata(host).expect("B");
        let b_raw = bound_raw(host, b);
        let a = heap::publish_userdata(host).expect("A");
        let a_raw = bound_raw(host, a);
        (a, a_raw, b, b_raw)
    } else {
        let a = heap::publish_userdata(host).expect("A");
        let a_raw = bound_raw(host, a);
        let b = heap::publish_userdata(host).expect("B");
        let b_raw = bound_raw(host, b);
        (a, a_raw, b, b_raw)
    };
    let (
        a_metatable,
        a_graph,
        a_graph_tail,
        b_metatable,
        b_graph,
        b_graph_tail,
        orphan,
        weak_values,
        weak_keys,
        weak_frame,
    ) = heap::with_heap(host, |heap| {
        let a_metatable = heap.alloc()?;
        heap.set_named_scalar(a_metatable, "__gc", Value::Bool(false))?;
        let a_graph = heap.alloc()?;
        let a_graph_tail = heap.alloc()?;
        heap.set_named_edge(a_metatable, "graph", a_graph)?;
        heap.set_named_edge(a_graph, "next", a_graph_tail)?;
        heap.set_named_edge(a_graph_tail, "next", a_graph)?;
        heap.set_number_userdata(a_metatable, 9.0, b)?;
        heap.set_userdata_metatable(a, Some(a_metatable))?;

        let b_metatable = heap.alloc()?;
        if arm == SettlementArm::Preinstalled {
            heap.set_named_scalar(b_metatable, "__gc", Value::Bool(false))?;
        }
        let b_graph = heap.alloc()?;
        let b_graph_tail = heap.alloc()?;
        heap.set_named_edge(b_metatable, "graph", b_graph)?;
        heap.set_named_edge(b_graph, "next", b_graph_tail)?;
        heap.set_named_edge(b_graph_tail, "next", b_graph)?;
        heap.set_userdata_metatable(b, Some(b_metatable))?;

        let orphan = heap.alloc()?;
        heap.set_edge(orphan, 1, orphan)?;

        let weak_values = weak_table(heap, "v")?;
        heap.set_number_userdata(weak_values, 1.0, a)?;
        heap.set_number_userdata(weak_values, 2.0, b)?;
        heap.set_number_edge(weak_values, 3.0, orphan)?;
        let weak_keys = weak_table(heap, "k")?;
        heap.set_userdata_key_scalar(weak_keys, a, Value::string("A"))?;
        heap.set_userdata_key_scalar(weak_keys, b, Value::string("B"))?;

        let weak_frame = heap.enter_roots(2)?;
        heap.write_root(weak_frame, 0, weak_values)?;
        heap.write_root(weak_frame, 1, weak_keys)?;
        Ok((
            a_metatable,
            a_graph,
            a_graph_tail,
            b_metatable,
            b_graph,
            b_graph_tail,
            orphan,
            weak_values,
            weak_keys,
            weak_frame,
        ))
    })
    .expect("settlement lattice");

    let b_strong_frame = if arm == SettlementArm::OrdinaryRoot {
        let frame = heap::with_heap(host, |heap| {
            let frame = heap.enter_roots(1)?;
            heap.write_userdata_root(frame, 0, b)?;
            Ok(frame)
        })
        .expect("B strong root");
        Some(frame)
    } else {
        None
    };

    SettlementSeed {
        a,
        a_raw,
        a_metatable,
        a_graph,
        a_graph_tail,
        b,
        b_raw,
        b_metatable,
        b_graph,
        b_graph_tail,
        orphan,
        weak_values,
        weak_keys,
        weak_frame,
        b_strong_frame,
    }
}

/// A, B's metatable, and both cyclic strong graph pairs are observable
/// through real stored edges. B's graph is reachable only through the second
/// mark's userdata-to-metatable edge when B has no ordinary root.
fn assert_settlement_graph_alive(host: &mut vm::HostContext<'_>, seed: &SettlementSeed) {
    assert_eq!(
        heap::with_heap(host, |heap| heap.userdata_metatable(seed.a)).expect("A metatable"),
        Some(seed.a_metatable)
    );
    assert_eq!(
        heap::with_heap(host, |heap| heap.userdata_metatable(seed.b)).expect("B metatable"),
        Some(seed.b_metatable)
    );
    assert_eq!(
        heap::with_heap(host, |heap| heap.number_userdata(seed.a_metatable, 9.0))
            .expect("A -> B edge"),
        Some(seed.b)
    );
    assert_eq!(
        heap::with_heap(host, |heap| heap.named_edge(seed.a_metatable, "graph")).expect("A graph"),
        Some(seed.a_graph)
    );
    assert_eq!(
        heap::with_heap(host, |heap| heap.named_edge(seed.a_graph, "next")).expect("A graph tail"),
        Some(seed.a_graph_tail)
    );
    assert_eq!(
        heap::with_heap(host, |heap| heap.named_edge(seed.a_graph_tail, "next"))
            .expect("A graph cycle"),
        Some(seed.a_graph)
    );
    assert_eq!(
        heap::with_heap(host, |heap| heap.named_edge(seed.b_metatable, "graph")).expect("B graph"),
        Some(seed.b_graph)
    );
    assert_eq!(
        heap::with_heap(host, |heap| heap.named_edge(seed.b_graph, "next")).expect("B graph tail"),
        Some(seed.b_graph_tail)
    );
    assert_eq!(
        heap::with_heap(host, |heap| heap.named_edge(seed.b_graph_tail, "next"))
            .expect("B graph cycle"),
        Some(seed.b_graph)
    );
    for table in [
        seed.a_metatable,
        seed.a_graph,
        seed.a_graph_tail,
        seed.b_metatable,
        seed.b_graph,
        seed.b_graph_tail,
    ] {
        assert!(
            alive(host, table),
            "the strong subgraph of A and B stays alive"
        );
    }
}

fn assert_first_settlement_publication(
    host: &mut vm::HostContext<'_>,
    seed: &SettlementSeed,
    arm: SettlementArm,
) {
    let _ = heap::take_scan_meter(host);
    let boundary = publish_boundary(host);
    assert!(matches!(boundary, PublishedBoundary::Published { .. }));
    let records = pending_records(host);
    assert!(
        records
            .iter()
            .any(|record| record.1 == seed.a.slot() && record.2 == seed.a.generation()),
        "A has a raw __gc and enters the pending queue"
    );
    assert!(
        !finalizer_settled(host, seed.a),
        "a queued finalizer is pending, not settled"
    );
    match arm {
        SettlementArm::Linked => {
            assert!(
                finalizer_settled(host, seed.b),
                "a linked white B with no raw __gc must be settled before the second mark"
            );
            assert_eq!(
                records.len(),
                1,
                "linked B must not enter the pending queue; it survives only through A's metatable"
            );
        }
        SettlementArm::OrdinaryRoot => {
            assert!(
                !finalizer_settled(host, seed.b),
                "B was already marked from an ordinary root, so the white-userdata pass must skip it"
            );
            assert_eq!(records.len(), 1);
        }
        SettlementArm::Preinstalled => {
            assert!(
                !finalizer_settled(host, seed.b),
                "a raw non-nil __gc is queued, not settled"
            );
            assert_eq!(records.len(), 2);
            assert!(
                records
                    .iter()
                    .any(|record| record.1 == seed.b.slot() && record.2 == seed.b.generation()),
                "preinstalled B is enqueued in the first round"
            );
        }
    }

    assert_eq!(
        weak_value(host, seed.weak_values, 1.0),
        None,
        "A's weak value is cleared"
    );
    match arm {
        SettlementArm::OrdinaryRoot => assert_eq!(
            weak_value(host, seed.weak_values, 2.0),
            Some(seed.b),
            "B's weak value survives while B has an ordinary strong root"
        ),
        SettlementArm::Linked | SettlementArm::Preinstalled => assert_eq!(
            weak_value(host, seed.weak_values, 2.0),
            None,
            "a live finalized B is value-dead even when its weak value entry is marked"
        ),
    }
    assert_eq!(
        weak_value(host, seed.weak_values, 3.0),
        None,
        "the independent orphan is value-dead"
    );
    assert_eq!(
        weak_key(host, seed.weak_keys, seed.a),
        Value::string("A"),
        "A is marked, so its weak key stays readable"
    );
    assert_eq!(
        weak_key(host, seed.weak_keys, seed.b),
        Value::string("B"),
        "B is marked, so its weak key stays readable even when finalized"
    );

    assert!(userdata_token_open(host, seed.a_raw));
    assert!(
        userdata_token_open(host, seed.b_raw),
        "B's original token must stay open while its generation is marked"
    );
    assert_eq!(
        slot_generation(host, seed.b.slot()),
        seed.b.generation(),
        "settlement never advances the generation"
    );
    assert_settlement_graph_alive(host, seed);
}

fn assert_release_after_settlement(host: &mut vm::HostContext<'_>, seed: &SettlementSeed) {
    let report = finish_release(host);
    assert_eq!(report.live_nodes, 12);
    assert!(!alive(host, seed.orphan));
    assert_eq!(
        slot_generation(host, seed.orphan.slot()),
        seed.orphan.generation() + 1
    );
    assert_eq!(slot_generation(host, seed.a.slot()), seed.a.generation());
    assert_eq!(slot_generation(host, seed.b.slot()), seed.b.generation());
    assert!(userdata_token_open(host, seed.a_raw));
    assert!(userdata_token_open(host, seed.b_raw));
    assert_settlement_graph_alive(host, seed);
}

#[test]
fn linked_white_without_raw_gc_settles_under_second_mark_and_never_requeues() {
    for b_first in [false, true] {
        crate::support::with_idle_vm(|vm| {
            let mut host = vm.host_context();
            let seed = seed_settlement(&mut host, SettlementArm::Linked, b_first);
            assert_first_settlement_publication(&mut host, &seed, SettlementArm::Linked);
            assert_release_after_settlement(&mut host, &seed);

            // After Release, write a real non-nil raw `__gc` through the
            // original metatable field. No Lua callback runs in this Rust
            // heap; the write is the same non-nil field the positive control
            // uses. Consume A's pending record and drop its graph, then run
            // the next ordinary selection cycle.
            heap::with_heap(&mut host, |heap| {
                heap.set_named_scalar(seed.b_metatable, "__gc", Value::Bool(false))
            })
            .expect("install raw __gc after release");
            heap::retire_pending_finalizer_test_control(&mut host, seed.a).expect("consume A");
            assert!(pending_records(&mut host).is_empty());

            let boundary = publish_boundary(&mut host);
            assert!(
                matches!(boundary, PublishedBoundary::Published { .. }),
                "the settled pair is ordinary dead garbage in the next cycle"
            );
            assert!(
                pending_records(&mut host).is_empty(),
                "linked B must not be requeued because a raw __gc appears after its settlement"
            );
            assert!(
                !userdata_token_open(&host, seed.b_raw),
                "settled B is really closed when it next dies"
            );
            assert!(
                !userdata_token_open(&host, seed.a_raw),
                "A was already consumed and now closes as ordinary garbage"
            );
            assert_eq!(weak_value(&mut host, seed.weak_values, 2.0), None);
            assert_eq!(
                weak_key(&mut host, seed.weak_keys, seed.b),
                Value::Null,
                "the dead B key is cleared in the next cycle"
            );
            let report = finish_release(&mut host);
            assert_eq!(
                report.live_nodes, 4,
                "only the two weak tables and their two modes remain"
            );
            assert_eq!(
                slot_generation(&mut host, seed.b.slot()),
                seed.b.generation() + 1,
                "B's slot really advances"
            );
            assert_eq!(
                slot_generation(&mut host, seed.a.slot()),
                seed.a.generation() + 1
            );
            heap::with_heap(&mut host, |heap| heap.leave_roots(seed.weak_frame))
                .expect("leave weak roots");
        });
    }
}

#[test]
fn ordinary_root_white_without_raw_gc_is_not_settled_and_queues_next_round() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let seed = seed_settlement(&mut host, SettlementArm::OrdinaryRoot, false);
        assert_first_settlement_publication(&mut host, &seed, SettlementArm::OrdinaryRoot);
        assert_release_after_settlement(&mut host, &seed);

        heap::retire_pending_finalizer_test_control(&mut host, seed.a).expect("consume A");
        heap::with_heap(&mut host, |heap| {
            heap.set_named_scalar(seed.b_metatable, "__gc", Value::Bool(false))
        })
        .expect("install raw __gc after release");
        let b_frame = seed.b_strong_frame.expect("ordinary-root arm");
        heap::with_heap(&mut host, |heap| heap.leave_roots(b_frame))
            .expect("release B's control root");

        let _ = heap::take_scan_meter(&mut host);
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        assert_eq!(
            pending_records(&mut host),
            vec![(seed.b_raw, seed.b.slot(), seed.b.generation())],
            "without a prior settlement B enters the queue in the next round"
        );
        assert!(pending_flag(&mut host, seed.b));
        assert!(!finalizer_settled(&mut host, seed.b));
        assert!(userdata_token_open(&host, seed.b_raw));
        assert_eq!(
            slot_generation(&mut host, seed.b.slot()),
            seed.b.generation()
        );
        assert_eq!(
            weak_value(&mut host, seed.weak_values, 2.0),
            None,
            "the queued B's weak value is cleared at this publication"
        );
        assert_eq!(
            weak_key(&mut host, seed.weak_keys, seed.b),
            Value::string("B")
        );
        finish_release(&mut host);

        heap::retire_pending_finalizer_test_control(&mut host, seed.b).expect("consume B");
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        assert!(pending_records(&mut host).is_empty());
        assert!(!userdata_token_open(&host, seed.b_raw));
        finish_release(&mut host);
        assert_eq!(
            slot_generation(&mut host, seed.b.slot()),
            seed.b.generation() + 1
        );
        heap::with_heap(&mut host, |heap| heap.leave_roots(seed.weak_frame))
            .expect("leave weak roots");
    });
}

#[test]
fn preinstalled_raw_gc_white_is_queued_in_the_first_round() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let seed = seed_settlement(&mut host, SettlementArm::Preinstalled, false);
        assert_first_settlement_publication(&mut host, &seed, SettlementArm::Preinstalled);
        assert_release_after_settlement(&mut host, &seed);

        heap::retire_pending_finalizer_test_control(&mut host, seed.a).expect("consume A");
        heap::retire_pending_finalizer_test_control(&mut host, seed.b).expect("consume B");
        assert!(pending_records(&mut host).is_empty());

        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        assert!(
            pending_records(&mut host).is_empty(),
            "a consumed preinstalled generation is not queued again"
        );
        assert!(!userdata_token_open(&host, seed.a_raw));
        assert!(!userdata_token_open(&host, seed.b_raw));
        finish_release(&mut host);
        assert_eq!(
            slot_generation(&mut host, seed.b.slot()),
            seed.b.generation() + 1
        );
        heap::with_heap(&mut host, |heap| heap.leave_roots(seed.weak_frame))
            .expect("leave weak roots");
    });
}

#[test]
fn missing_raw_gc_on_a_shared_metatable_settles_every_white_identity() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, settlement_limits()).expect("install");
        let (b_first, b_first_raw) = {
            let b = heap::publish_userdata(&mut host).expect("B1");
            (b, bound_raw(&mut host, b))
        };
        let (b_second, b_second_raw) = {
            let b = heap::publish_userdata(&mut host).expect("B2");
            (b, bound_raw(&mut host, b))
        };
        let a = heap::publish_userdata(&mut host).expect("A");
        let a_raw = bound_raw(&mut host, a);
        let (a_metatable, shared, shared_graph, weak_values, weak_keys, frame) =
            heap::with_heap(&mut host, |heap| {
                let a_metatable = heap.alloc()?;
                heap.set_named_scalar(a_metatable, "__gc", Value::Bool(false))?;
                heap.set_named_userdata(a_metatable, "first", b_first)?;
                heap.set_named_userdata(a_metatable, "second", b_second)?;
                heap.set_userdata_metatable(a, Some(a_metatable))?;

                let shared = heap.alloc()?;
                let shared_graph = heap.alloc()?;
                heap.set_named_edge(shared, "graph", shared_graph)?;
                heap.set_userdata_metatable(b_first, Some(shared))?;
                heap.set_userdata_metatable(b_second, Some(shared))?;

                let weak_values = weak_table(heap, "v")?;
                heap.set_number_userdata(weak_values, 1.0, b_first)?;
                heap.set_number_userdata(weak_values, 2.0, b_second)?;
                heap.set_number_userdata(weak_values, 3.0, a)?;
                let weak_keys = weak_table(heap, "k")?;
                heap.set_userdata_key_scalar(weak_keys, b_first, Value::string("B1"))?;
                heap.set_userdata_key_scalar(weak_keys, b_second, Value::string("B2"))?;
                let frame = heap.enter_roots(2)?;
                heap.write_root(frame, 0, weak_values)?;
                heap.write_root(frame, 1, weak_keys)?;
                Ok((
                    a_metatable,
                    shared,
                    shared_graph,
                    weak_values,
                    weak_keys,
                    frame,
                ))
            })
            .expect("shared metatable lattice");

        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        assert_eq!(
            pending_records(&mut host),
            vec![(a_raw, a.slot(), a.generation())]
        );
        assert!(
            finalizer_settled(&mut host, b_first),
            "the first shared-metatable identity is settled on the lookup miss"
        );
        assert!(
            finalizer_settled(&mut host, b_second),
            "a cached negative raw __gc result must still settle every later white identity"
        );
        assert_eq!(weak_value(&mut host, weak_values, 1.0), None);
        assert_eq!(weak_value(&mut host, weak_values, 2.0), None);
        assert_eq!(weak_key(&mut host, weak_keys, b_first), Value::string("B1"));
        assert_eq!(
            weak_key(&mut host, weak_keys, b_second),
            Value::string("B2")
        );
        assert!(userdata_token_open(&host, b_first_raw));
        assert!(userdata_token_open(&host, b_second_raw));
        assert!(alive(&mut host, a_metatable));
        assert!(alive(&mut host, shared));
        assert!(alive(&mut host, shared_graph));
        finish_release(&mut host);
        heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave roots");
    });
}

#[test]
fn missing_and_nil_raw_gc_settle_without_reading_an_index_chain() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, settlement_limits()).expect("install");
        let missing = heap::publish_userdata(&mut host).expect("missing");
        let missing_raw = bound_raw(&mut host, missing);
        let nil = heap::publish_userdata(&mut host).expect("nil");
        let nil_raw = bound_raw(&mut host, nil);
        let (missing_metatable, index_table, nil_metatable, weak_values, frame) =
            heap::with_heap(&mut host, |heap| {
                // The only `__gc` reachable from the missing arm is through
                // `__index`. The raw lookup must not follow it.
                let missing_metatable = heap.alloc()?;
                let index_meta = heap.alloc()?;
                let index_table = heap.alloc()?;
                heap.set_named_scalar(index_table, "__gc", Value::Bool(false))?;
                heap.set_named_edge(index_meta, "__index", index_table)?;
                heap.set_metatable(missing_metatable, Some(index_meta))?;
                heap.set_userdata_metatable(missing, Some(missing_metatable))?;

                // The nil arm stores the raw field itself as nil.
                let nil_metatable = heap.alloc()?;
                heap.set_named_scalar(nil_metatable, "__gc", Value::Null)?;
                heap.set_userdata_metatable(nil, Some(nil_metatable))?;

                let weak_values = weak_table(heap, "v")?;
                heap.set_number_userdata(weak_values, 1.0, missing)?;
                heap.set_number_userdata(weak_values, 2.0, nil)?;
                let frame = heap.enter_roots(1)?;
                heap.write_root(frame, 0, weak_values)?;
                Ok((
                    missing_metatable,
                    index_table,
                    nil_metatable,
                    weak_values,
                    frame,
                ))
            })
            .expect("missing and nil lattice");

        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        assert!(pending_records(&mut host).is_empty());
        assert!(
            finalizer_settled(&mut host, missing),
            "an absent raw __gc settles the white userdata without following __index"
        );
        assert!(
            finalizer_settled(&mut host, nil),
            "a stored nil raw __gc settles the white userdata"
        );
        assert!(
            !userdata_token_open(&host, missing_raw),
            "settlement is not retention: the unreached missing arm dies in this cycle"
        );
        assert!(!userdata_token_open(&host, nil_raw));
        assert_eq!(weak_value(&mut host, weak_values, 1.0), None);
        assert_eq!(weak_value(&mut host, weak_values, 2.0), None);
        assert!(alive(&mut host, missing_metatable));
        assert!(alive(&mut host, index_table));
        assert!(alive(&mut host, nil_metatable));
        let report = finish_release(&mut host);
        assert_eq!(report.live_nodes, 2, "the weak table and its mode");
        assert!(!alive(&mut host, missing_metatable));
        assert!(!alive(&mut host, index_table));
        assert!(!alive(&mut host, nil_metatable));
        assert_eq!(
            slot_generation(&mut host, missing.slot()),
            missing.generation() + 1
        );
        assert_eq!(slot_generation(&mut host, nil.slot()), nil.generation() + 1);
        heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave roots");
    });
}

/// One typed userdata with a live metatable that has no direct raw `__gc`,
/// its own real token, and one rooted weak-value table.
struct MissingEligible {
    userdata: UserdataRef,
    raw: u64,
    metatable: TableRef,
    weak_values: TableRef,
}

fn publish_missing_eligible(host: &mut vm::HostContext<'_>) -> MissingEligible {
    let userdata = heap::publish_userdata(host).expect("missing-raw userdata");
    let raw = bound_raw(host, userdata);
    let (metatable, weak_values) = heap::with_heap(host, |heap| {
        let metatable = heap.alloc()?;
        heap.set_named_scalar(metatable, "f0", Value::string("not __gc"))?;
        heap.set_userdata_metatable(userdata, Some(metatable))?;
        let weak_values = weak_table(heap, "v")?;
        heap.set_number_userdata(weak_values, 1.0, userdata)?;
        Ok((metatable, weak_values))
    })
    .expect("missing-raw lattice");
    MissingEligible {
        userdata,
        raw,
        metatable,
        weak_values,
    }
}

/// Rollback at the pre-close typed-owner check. A prior pending record and a
/// prior settled-but-rooted identity survive the failed batch; the new
/// settlements are removed. After the same-type misbinding is fixed, a retry
/// in the same `HostContext` selects the previously settled identities as
/// ordinary raw-`__gc` candidates.
#[test]
fn settlement_rolls_back_at_owner_verification_and_retry_selects() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, settlement_limits()).expect("install");

        // A valid previous-cycle pending record.
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

        // A previous-cycle settled identity S, reachable only through A0's
        // metatable during its settling pass and then kept alive by a real
        // root. Its settled flag is valid prior state.
        let a0 = heap::publish_userdata(&mut host).expect("A0");
        let a0_raw = bound_raw(&mut host, a0);
        let s = heap::publish_userdata(&mut host).expect("S");
        let s_raw = bound_raw(&mut host, s);
        let (a0_metatable, s_metatable) = heap::with_heap(&mut host, |heap| {
            let a0_metatable = heap.alloc()?;
            heap.set_named_scalar(a0_metatable, "__gc", Value::Bool(false))?;
            heap.set_named_userdata(a0_metatable, "child", s)?;
            heap.set_userdata_metatable(a0, Some(a0_metatable))?;
            let s_metatable = heap.alloc()?;
            heap.set_userdata_metatable(s, Some(s_metatable))?;
            Ok((a0_metatable, s_metatable))
        })
        .expect("prior settled lattice");
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.userdata_metatable(a0)).expect("A0 metatable"),
            Some(a0_metatable)
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.userdata_metatable(s)).expect("S metatable"),
            Some(s_metatable)
        );
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        finish_release(&mut host);
        assert!(
            finalizer_settled(&mut host, s),
            "S was settled by the previous selecting pass"
        );
        assert_eq!(pending_records(&mut host).len(), 2);
        let s_frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(1)?;
            heap.write_userdata_root(frame, 0, s)?;
            Ok(frame)
        })
        .expect("S root");

        // The failed batch: A is a real raw-`__gc` candidate selected before
        // C1 and C2, which have no raw field. B owns the real token used for
        // the same-type misbinding.
        let b = heap::publish_userdata(&mut host).expect("token owner B");
        let b_raw = bound_raw(&mut host, b);
        let b_frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(1)?;
            heap.write_userdata_root(frame, 0, b)?;
            Ok(frame)
        })
        .expect("B root");
        let a = publish_eligible(&mut host);
        let c_first = publish_missing_eligible(&mut host);
        let c_second = publish_missing_eligible(&mut host);
        assert!(
            a.userdata.slot() < c_first.userdata.slot()
                && c_first.userdata.slot() < c_second.userdata.slot(),
            "A is doomed before the two new settlements, so the ordinary control fails before closing them"
        );
        let c_frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(2)?;
            heap.write_root(frame, 0, c_first.weak_values)?;
            heap.write_root(frame, 1, c_second.weak_values)?;
            Ok(frame)
        })
        .expect("C roots");
        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(a.userdata)?;
            heap.bind_userdata_token(a.userdata, b_raw)
        })
        .expect("same-type misbind");

        let _ = heap::take_scan_meter(&mut host);
        let error = heap::collect_pending_finalizers(&mut host).expect_err("foreign owner");
        assert_eq!(error, HeapError::TokenMismatch);
        let meter = heap::take_scan_meter(&mut host).expect("refusal meter");
        assert_eq!(meter.token_closes, 0, "verification fails before close");
        assert_eq!(
            pending_records(&mut host),
            vec![
                (
                    prior.raw,
                    prior.userdata.slot(),
                    prior.userdata.generation()
                ),
                (a0_raw, a0.slot(), a0.generation())
            ],
            "the prior records survive; the new batch record is removed"
        );
        assert!(pending_flag(&mut host, prior.userdata));
        assert!(pending_flag(&mut host, a0));
        assert!(!pending_flag(&mut host, a.userdata));
        assert!(
            !finalizer_settled(&mut host, c_first.userdata),
            "a pre-publication verification failure must roll back the new settlement"
        );
        assert!(!finalizer_settled(&mut host, c_second.userdata));
        assert!(
            finalizer_settled(&mut host, s),
            "valid prior settled state is not erased by a failed new batch"
        );
        assert!(!heap::saved_sweep_tail(&mut host).expect("no tail"));
        assert_eq!(
            weak_value(&mut host, c_first.weak_values, 1.0),
            Some(c_first.userdata),
            "no weak publication on a verification failure"
        );
        assert_eq!(
            weak_value(&mut host, c_second.weak_values, 1.0),
            Some(c_second.userdata)
        );
        assert!(userdata_token_open(&host, a.raw));
        assert!(userdata_token_open(&host, c_first.raw));
        assert!(userdata_token_open(&host, c_second.raw));
        assert!(userdata_token_open(&host, b_raw));
        assert!(userdata_token_open(&host, s_raw));
        assert_eq!(
            slot_generation(&mut host, c_first.userdata.slot()),
            c_first.userdata.generation()
        );

        // The ordinary entry, still carrying the same-type misbinding, must
        // refuse before any close and publish no weak edge. A is the first
        // doomed slot, so neither new settlement token closes.
        let control = heap::collect(&mut host).expect_err("ordinary close rejects the misbind");
        assert_eq!(control, HeapError::TokenMismatch);
        assert!(userdata_token_open(&host, c_first.raw));
        assert!(userdata_token_open(&host, c_second.raw));
        assert!(!finalizer_settled(&mut host, c_first.userdata));
        assert!(!finalizer_settled(&mut host, c_second.userdata));
        assert_eq!(
            weak_value(&mut host, c_first.weak_values, 1.0),
            Some(c_first.userdata)
        );
        assert!(!heap::saved_sweep_tail(&mut host).expect("no tail after control"));

        // Restore the exact binding, install the positive-control raw field
        // on both settled candidates, and retry in the same context.
        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(a.userdata)?;
            heap.bind_userdata_token(a.userdata, a.raw)?;
            heap.set_named_scalar(c_first.metatable, "__gc", Value::Bool(false))?;
            heap.set_named_scalar(c_second.metatable, "__gc", Value::Bool(false))
        })
        .expect("restore and install");
        let _ = heap::take_scan_meter(&mut host);
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        let records = pending_records(&mut host);
        for userdata in [a.userdata, c_first.userdata, c_second.userdata] {
            assert!(
                records
                    .iter()
                    .any(|record| record.1 == userdata.slot() && record.2 == userdata.generation()),
                "the retry selects the restored candidate"
            );
        }
        assert!(pending_flag(&mut host, c_first.userdata));
        assert!(pending_flag(&mut host, c_second.userdata));
        assert_eq!(weak_value(&mut host, c_first.weak_values, 1.0), None);
        assert_eq!(weak_value(&mut host, c_second.weak_values, 1.0), None);
        assert!(userdata_token_open(&host, c_first.raw));
        assert!(userdata_token_open(&host, c_second.raw));
        finish_release(&mut host);

        for frame in [c_frame, b_frame, s_frame, prior_frame] {
            heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave roots");
        }
    });
}

/// Rollback at the doomed-token close boundary. A is the retained pending
/// record; C is settled by the same selecting pass. D is doomed with C's real
/// same-type token, so the close verifier fails after selection. The failure
/// must remove only C's new settlement, preserve A's record and the prior
/// state, publish nothing, and allow the same context to select C after the
/// binding is fixed and a raw `__gc` is installed.
#[test]
fn settlement_rolls_back_at_doomed_close_and_retry_selects() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, settlement_limits()).expect("install");
        let a = heap::publish_userdata(&mut host).expect("A");
        let a_raw = bound_raw(&mut host, a);
        let (a_metatable, weak_values, weak_keys, frame) = heap::with_heap(&mut host, |heap| {
            let a_metatable = heap.alloc()?;
            heap.set_named_scalar(a_metatable, "__gc", Value::Bool(false))?;
            heap.set_userdata_metatable(a, Some(a_metatable))?;
            let weak_values = weak_table(heap, "v")?;
            heap.set_number_userdata(weak_values, 1.0, a)?;
            let weak_keys = weak_table(heap, "k")?;
            heap.set_userdata_key_scalar(weak_keys, a, Value::string("A"))?;
            let frame = heap.enter_roots(2)?;
            heap.write_root(frame, 0, weak_values)?;
            heap.write_root(frame, 1, weak_keys)?;
            Ok((a_metatable, weak_values, weak_keys, frame))
        })
        .expect("A lattice");
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.userdata_metatable(a)).expect("A metatable"),
            Some(a_metatable)
        );

        // D is published before C and is the first doomed slot. Its slot is
        // temporarily bound to C's real token, so the doomed close sees a
        // same-type payload mismatch for D.
        let d = heap::publish_userdata(&mut host).expect("D");
        let d_raw = bound_raw(&mut host, d);
        let c = publish_missing_eligible(&mut host);
        assert!(d.slot() < c.userdata.slot());
        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(d)?;
            heap.bind_userdata_token(d, c.raw)
        })
        .expect("same-type misbind on the doomed slot");

        let _ = heap::take_scan_meter(&mut host);
        let error = heap::collect_pending_finalizers(&mut host).expect_err("doomed close mismatch");
        assert_eq!(error, HeapError::TokenMismatch);
        let meter = heap::take_scan_meter(&mut host).expect("close refusal meter");
        assert_eq!(meter.token_closes, 0, "D fails before any token closes");
        assert_eq!(
            pending_records(&mut host),
            vec![(a_raw, a.slot(), a.generation())],
            "the selected A record survives a close failure"
        );
        assert!(pending_flag(&mut host, a));
        assert!(
            !finalizer_settled(&mut host, c.userdata),
            "a failed doomed close must roll back C's new settlement"
        );
        assert!(!heap::saved_sweep_tail(&mut host).expect("no tail"));
        assert_eq!(
            weak_value(&mut host, weak_values, 1.0),
            Some(a),
            "A's weak value is still unpublished"
        );
        assert_eq!(
            weak_value(&mut host, c.weak_values, 1.0),
            Some(c.userdata),
            "C's weak value is still unpublished"
        );
        assert_eq!(
            weak_key(&mut host, weak_keys, a),
            Value::string("A"),
            "A's weak key is still unpublished"
        );
        assert!(userdata_token_open(&host, a_raw));
        assert!(userdata_token_open(&host, c.raw));
        assert!(userdata_token_open(&host, d_raw));
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.published_userdata_token(d))
                .expect("D keeps the failing binding"),
            c.raw
        );

        // The ordinary entry with the same bad binding also refuses before
        // making a fake record or publishing. D is the first doomed slot.
        let control = heap::collect(&mut host).expect_err("ordinary close rejects the misbind");
        assert_eq!(control, HeapError::TokenMismatch);
        assert_eq!(
            pending_records(&mut host),
            vec![(a_raw, a.slot(), a.generation())]
        );
        assert!(!finalizer_settled(&mut host, c.userdata));
        assert!(userdata_token_open(&host, c.raw));
        assert_eq!(weak_value(&mut host, c.weak_values, 1.0), Some(c.userdata));
        assert!(!heap::saved_sweep_tail(&mut host).expect("no tail after control"));

        // Fix D, install the positive-control raw field on C, and retry.
        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(d)?;
            heap.bind_userdata_token(d, d_raw)?;
            heap.set_named_scalar(c.metatable, "__gc", Value::Bool(false))
        })
        .expect("restore D and install C's raw __gc");
        let _ = heap::take_scan_meter(&mut host);
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        let records = pending_records(&mut host);
        assert!(
            records
                .iter()
                .any(|record| record.1 == c.userdata.slot() && record.2 == c.userdata.generation()),
            "the retry selects C after its settlement was rolled back"
        );
        assert!(pending_flag(&mut host, c.userdata));
        assert!(!finalizer_settled(&mut host, c.userdata));
        assert!(userdata_token_open(&host, c.raw));
        assert_eq!(
            slot_generation(&mut host, c.userdata.slot()),
            c.userdata.generation()
        );
        assert_eq!(weak_value(&mut host, c.weak_values, 1.0), None);
        assert_eq!(
            weak_key(&mut host, weak_keys, a),
            Value::string("A"),
            "A remains a marked pending key"
        );
        finish_release(&mut host);
        heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave roots");
    });
}

/// Anti-control: settlement is not a root. With A's metatable follow
/// suppressed, the linked B has no other strong edge. B must still be
/// settled, but its token must close and its generation must advance in the
/// same cycle. This proves the surviving B in the main linked arm is retained
/// by the real second mark, not by the settlement flag.
#[test]
fn settlement_alone_does_not_retain_a_linked_white_without_raw_gc() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let seed = seed_settlement(&mut host, SettlementArm::Linked, false);
        heap::with_heap(&mut host, |heap| {
            heap.arm_pending_finalizer_metatable_skip(true);
            Ok(())
        })
        .expect("arm metatable skip");

        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        assert_eq!(
            pending_records(&mut host),
            vec![(seed.a_raw, seed.a.slot(), seed.a.generation())]
        );
        assert!(finalizer_settled(&mut host, seed.b));
        assert!(
            !userdata_token_open(&host, seed.b_raw),
            "settlement does not retain B when the second-mark edge is suppressed"
        );
        assert!(userdata_token_open(&host, seed.a_raw));
        assert_eq!(weak_value(&mut host, seed.weak_values, 2.0), None);
        assert_eq!(
            weak_key(&mut host, seed.weak_keys, seed.b),
            Value::Null,
            "an unmarked settled B does not keep its weak key"
        );
        finish_release(&mut host);
        assert_eq!(
            slot_generation(&mut host, seed.b.slot()),
            seed.b.generation() + 1,
            "B really dies in this cycle"
        );
        assert!(!alive(&mut host, seed.b_metatable));
        assert!(!alive(&mut host, seed.b_graph));
        assert_eq!(
            slot_generation(&mut host, seed.a.slot()),
            seed.a.generation()
        );
        assert!(userdata_token_open(&host, seed.a_raw));
        heap::with_heap(&mut host, |heap| heap.leave_roots(seed.weak_frame))
            .expect("leave weak roots");
    });
}

/// Cross-round compatibility: a settled generation that is later kept alive
/// by a real strong root is still value-dead for a fresh weak value. The
/// second cycle removes A, so the third-cycle publication can only be forced
/// by the settled-and-marked B, not by a pending record or a dead slot.
#[test]
fn settled_generation_reclears_a_reinserted_weak_value_while_rooted() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let seed = seed_settlement(&mut host, SettlementArm::Linked, false);
        assert_first_settlement_publication(&mut host, &seed, SettlementArm::Linked);
        assert_release_after_settlement(&mut host, &seed);

        // Consume A and root B. The next cycle releases A and leaves B live
        // and settled, with no pending record.
        heap::retire_pending_finalizer_test_control(&mut host, seed.a).expect("consume A");
        let b_frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(1)?;
            heap.write_userdata_root(frame, 0, seed.b)?;
            Ok(frame)
        })
        .expect("B control root");
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        assert!(pending_records(&mut host).is_empty());
        assert!(!userdata_token_open(&host, seed.a_raw));
        finish_release(&mut host);
        assert_eq!(
            slot_generation(&mut host, seed.a.slot()),
            seed.a.generation() + 1
        );
        assert!(userdata_token_open(&host, seed.b_raw));
        assert!(finalizer_settled(&mut host, seed.b));

        // No dead slot and no pending record exist now. Reinsert a real weak
        // value through the original table field. Only B's settled generation
        // can force the publication window.
        heap::with_heap(&mut host, |heap| {
            heap.set_number_userdata(seed.weak_values, 6.0, seed.b)
        })
        .expect("reinsert B weak value");
        let boundary = publish_boundary(&mut host);
        assert!(
            matches!(boundary, PublishedBoundary::Published { .. }),
            "a live settled generation forces weak publication with no dead slot or pending record"
        );
        assert_eq!(
            weak_value(&mut host, seed.weak_values, 6.0),
            None,
            "a settled generation is value-dead across rounds"
        );
        assert_eq!(
            weak_key(&mut host, seed.weak_keys, seed.b),
            Value::string("B"),
            "the real root keeps B marked, so its weak key stays"
        );
        assert!(userdata_token_open(&host, seed.b_raw));
        assert_eq!(
            slot_generation(&mut host, seed.b.slot()),
            seed.b.generation()
        );
        finish_release(&mut host);
        heap::with_heap(&mut host, |heap| heap.leave_roots(b_frame)).expect("leave B root");
        heap::with_heap(&mut host, |heap| heap.leave_roots(seed.weak_frame))
            .expect("leave weak roots");
    });
}
