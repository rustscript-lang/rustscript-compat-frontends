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
    self, ClosureRef, HandleKind, HeapError, HeapLimits, LuaUserdataResource,
    PendingFinalizerAction, PendingFinalizerNotCallable, PublishedBoundary, RootFrameId, ScanMeter,
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

/// Test-only: replaces the typed payload of an existing `lua.userdata` token
/// while keeping the raw handle and every heap binding unchanged. The caller
/// restores the original payload through the same path. This creates the
/// "queue raw == slot raw but the Core payload names another object" shape
/// without a fake token, a second binding, or a production shortcut.
fn overwrite_userdata_payload(host: &mut vm::HostContext<'_>, raw: u64, payload: UserdataRef) {
    let handle = resource_handle(raw);
    let mut borrowed = host
        .borrow_resource_mut_with_key::<LuaUserdataResource>(handle, &LuaUserdataResource::key())
        .expect("mutable typed userdata token");
    *borrowed.get() = LuaUserdataResource::new(payload);
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
            meter.pending_finalizer_scan_slots, 1,
            "the selection scan reads only the one live userdata chain node"
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
    metatable: TableRef,
    weak_values: TableRef,
}

/// One real typed userdata with its own token, a raw `__gc = false` metatable,
/// and one rooted weak-value table. The userdata itself is not rooted.
fn publish_eligible(host: &mut vm::HostContext<'_>) -> Eligible {
    let userdata = heap::publish_userdata(host).expect("eligible userdata");
    let raw = bound_raw(host, userdata);
    let (metatable, weak_values) = heap::with_heap(host, |heap| {
        let metatable = heap.alloc()?;
        heap.set_named_scalar(metatable, "__gc", Value::Bool(false))?;
        heap.set_userdata_metatable(userdata, Some(metatable))?;
        let weak_values = weak_table(heap, "v")?;
        heap.set_number_userdata(weak_values, 1.0, userdata)?;
        Ok((metatable, weak_values))
    })
    .expect("eligible lattice");
    Eligible {
        userdata,
        raw,
        metatable,
        weak_values,
    }
}

fn pending_flag(host: &mut vm::HostContext<'_>, userdata: UserdataRef) -> bool {
    heap::with_heap(host, |heap| heap.slot_pending_finalizer(userdata)).expect("pending flag")
}

fn published_count(host: &mut vm::HostContext<'_>) -> usize {
    heap::pending_finalizer_published_count(host).expect("published pending prefix")
}

fn rooted_userdata(
    host: &mut vm::HostContext<'_>,
    frame: RootFrameId,
    slot: u32,
) -> Option<UserdataRef> {
    heap::rooted_userdata(host, frame, slot).expect("userdata root slot")
}

/// The private handoff's refusal for a queue head that no successful private
/// publication has made consumable.
fn assert_not_published(error: HeapError) {
    match error {
        HeapError::Scope(message) => assert!(
            message.contains("pending finalizer head is not published"),
            "unexpected scope message: {message}"
        ),
        other => panic!("expected an unpublished-head scope error, got {other:?}"),
    }
}

/// The private handoff's refusal for a specified root slot that is already
/// occupied. The slot and the queue must both stay unchanged.
fn assert_root_slot_occupied(error: HeapError) {
    match error {
        HeapError::Scope(message) => assert!(
            message.contains("pending finalizer root slot is occupied"),
            "unexpected scope message: {message}"
        ),
        other => panic!("expected an occupied-root scope error, got {other:?}"),
    }
}

/// A previous-cycle record, a legitimate new candidate C verified before the
/// same-round same-type misbound A, and a real retry. C is allocated after A
/// but reuses a lower freed slot, so the live chain selects C before A while
/// the independent ascending pending-close enumeration still closes C's token
/// before it reaches A's failing binding. The private selection must remove
/// only this batch's new flags and records, leaving the earlier record and
/// every raw generation untouched. While the bad binding remains, an ordinary
/// entry must not retain or weakly publish from a fake record. Restoring the
/// exact raw and retrying in the same context selects the real finalizer and
/// publishes.
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

        // This batch: A is a real raw-`__gc` candidate allocated first. Its
        // slot is temporarily bound to B's real same-type token. C is
        // allocated later, so it is the live chain head and is verified
        // before A fails. C reuses the lower slot freed below A, so the
        // ascending ordinary close reaches C's real token before A's
        // misbinding refuses.
        let sacrificial =
            heap::with_heap(&mut host, |heap| heap.alloc_userdata()).expect("sacrificial");
        let sacrificial_generation = slot_generation(&mut host, sacrificial.slot());
        let a = publish_eligible(&mut host);
        let a_transient_frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(2)?;
            heap.write_userdata_root(frame, 0, a.userdata)?;
            heap.write_root(frame, 1, a.weak_values)?;
            Ok(frame)
        })
        .expect("A transient roots");
        let _ = heap::collect(&mut host).expect("free the lower sacrificial slot");
        assert_eq!(
            slot_generation(&mut host, sacrificial.slot()),
            sacrificial_generation + 1,
            "the sacrificial userdata was really released"
        );
        heap::with_heap(&mut host, |heap| heap.leave_roots(a_transient_frame))
            .expect("leave A transient roots");
        let c = publish_eligible(&mut host);
        assert_eq!(
            c.userdata.slot(),
            sacrificial.slot(),
            "C reuses the real freed slot"
        );
        assert_eq!(c.userdata.generation(), sacrificial_generation + 1);
        assert!(
            c.userdata.slot() < a.userdata.slot(),
            "C is the newer chain head but sits in the lower doomed slot"
        );
        let b = heap::publish_userdata(&mut host).expect("rooted B");
        let b_raw = bound_raw(&mut host, b);
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
        heap::with_heap(&mut host, |heap| {
            heap.clear_owner_verification_trace();
            Ok(())
        })
        .expect("clear owner-verification trace");

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
        let (successes, verified) = heap::with_heap(&mut host, |heap| {
            Ok((
                heap.owner_verification_successes(),
                heap.owner_verification_trace(),
            ))
        })
        .expect("owner-verification successes");
        assert_eq!(
            successes, 2,
            "the old published record and legitimate C passed the real owner check"
        );
        assert_eq!(
            verified,
            vec![
                (prior.userdata.slot(), prior.userdata.generation()),
                (c.userdata.slot(), c.userdata.generation()),
            ],
            "C's real identity is verified before A's misbinding is refused"
        );

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
        // retain A or C and no weak edge is published. C's lower slot is
        // closed first, proving the close really happened before A failed.
        let control = heap::collect(&mut host).expect_err("ordinary close rejects the misbind");
        assert_eq!(control, HeapError::TokenMismatch);
        assert!(
            !userdata_token_open(&host, c.raw),
            "C's real token closed before A's misbinding was refused"
        );
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

/// Which ordinary dead slot a Release probe creates. Both variants share the
/// same pending cohort, weak root, publish boundary, and one-unit resume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReleaseProbe {
    Table,
    Userdata,
}

#[derive(Clone, Copy)]
enum ReleaseDead {
    Table(TableRef),
    Userdata(UserdataRef),
}

impl ReleaseDead {
    fn slot(self) -> u32 {
        match self {
            Self::Table(dead) => dead.slot(),
            Self::Userdata(dead) => dead.slot(),
        }
    }

    fn generation(self) -> u32 {
        match self {
            Self::Table(dead) => dead.generation(),
            Self::Userdata(dead) => dead.generation(),
        }
    }
}

struct ReleaseQueueReceipt {
    reads: u64,
    pending_before: usize,
    pending_after: usize,
    dead_slot: u32,
    dead_generation: u32,
    released_generation: u32,
    chain_before: Vec<(u32, u32)>,
    chain_after: Vec<(u32, u32)>,
    phase_before: heap::SavedTailPhase,
    cursor_before: usize,
    dead_len_before: usize,
    phase_after: heap::SavedTailPhase,
    cursor_after: usize,
    token_closes: u64,
    dead_alive_after: bool,
}

/// Release one ordinary dead slot from a real published tail while several
/// genuine pending records exist. The only measured work is the release path
/// itself; `reads` is the real number of pending-queue records the old
/// per-dead-slot loop visited. The table and userdata variants use this one
/// construction, so their zero-read results, non-empty queue lengths, and
/// Release windows are directly comparable.
fn release_queue_reads_for(pending_count: usize, probe: ReleaseProbe) -> ReleaseQueueReceipt {
    let mut out = None;
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

        let dead = match probe {
            ReleaseProbe::Table => ReleaseDead::Table(
                heap::with_heap(&mut host, |heap| heap.alloc()).expect("dead table"),
            ),
            ReleaseProbe::Userdata => ReleaseDead::Userdata(
                heap::with_heap(&mut host, |heap| heap.alloc_userdata()).expect("dead userdata"),
            ),
        };
        let dead_slot = dead.slot();
        let dead_generation = dead.generation();
        heap::with_heap(&mut host, |heap| {
            let weak = weak_table(heap, "v")?;
            match dead {
                ReleaseDead::Table(dead) => heap.set_number_edge(weak, 1.0, dead)?,
                ReleaseDead::Userdata(dead) => heap.set_number_userdata(weak, 1.0, dead)?,
            }
            heap.write_root(frame, pending_count as u32, weak)?;
            Ok(())
        })
        .expect("dead weak table");
        let pending_generations: Vec<u32> = eligible
            .iter()
            .map(|item| slot_generation(&mut host, item.userdata.slot()))
            .collect();
        let pending_before = pending_records(&mut host).len();
        assert_eq!(
            pending_before, pending_count,
            "both probes start with a non-empty pending queue"
        );
        let chain_before = heap::with_heap(&mut host, |heap| Ok(heap.live_userdata_chain()))
            .expect("live chain before release");

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
        let released_generation = slot_generation(&mut host, dead_slot);
        assert_eq!(
            released_generation,
            dead_generation + 1,
            "the ordinary dead slot advances exactly one generation"
        );
        let dead_alive_after = match dead {
            ReleaseDead::Table(dead) => alive(&mut host, dead),
            ReleaseDead::Userdata(dead) => {
                heap::with_heap(&mut host, |heap| Ok(heap.userdata_alive(dead)))
                    .expect("dead userdata liveness")
            }
        };
        assert!(!dead_alive_after, "the ordinary dead slot is released");
        for (item, generation) in eligible.iter().zip(pending_generations) {
            assert_eq!(
                slot_generation(&mut host, item.userdata.slot()),
                generation,
                "a pending slot keeps its generation"
            );
            assert!(pending_flag(&mut host, item.userdata));
        }
        let pending_after = pending_records(&mut host).len();
        assert_eq!(pending_after, pending_count);
        let chain_after = heap::with_heap(&mut host, |heap| Ok(heap.live_userdata_chain()))
            .expect("live chain after release");
        out = Some(ReleaseQueueReceipt {
            reads: meter.pending_release_queue_reads,
            pending_before,
            pending_after,
            dead_slot,
            dead_generation,
            released_generation,
            chain_before,
            chain_after,
            phase_before: before.phase,
            cursor_before: before.cursor,
            dead_len_before: before.dead_len,
            phase_after: after.phase,
            cursor_after: after.cursor,
            token_closes: meter.token_closes,
            dead_alive_after,
        });
    });
    out.expect("release arm")
}

#[test]
fn release_of_an_ordinary_dead_slot_reads_no_pending_queue() {
    let two = release_queue_reads_for(2, ReleaseProbe::Table);
    let five = release_queue_reads_for(5, ReleaseProbe::Table);
    assert_eq!(
        (two.reads, five.reads),
        (0, 0),
        "Release must not traverse the pending queue per dead slot; the count grew with the queue"
    );
    assert!(
        two.pending_before > 0 && five.pending_before > 0,
        "the table control keeps a non-empty pending queue in both arms"
    );
    assert_eq!(two.pending_after, two.pending_before);
    assert_eq!(five.pending_after, five.pending_before);
}

/// The direct table control and a real ordinary dead userdata share the same
/// pending cohort construction. The userdata arm must read zero pending-queue
/// records, advance its exact old generation, unlink its identity from the
/// live chain, and spend the Release window on a non-empty queue without a
/// token close.
#[test]
fn release_of_an_ordinary_dead_userdata_reads_no_pending_queue() {
    let table = release_queue_reads_for(2, ReleaseProbe::Table);
    let userdata = release_queue_reads_for(2, ReleaseProbe::Userdata);
    assert_eq!(
        (table.reads, userdata.reads),
        (0, 0),
        "the table and userdata Release arms must both avoid the pending queue"
    );
    assert!(
        table.pending_before > 0 && userdata.pending_before > 0,
        "both direct-comparison arms keep a non-empty pending queue"
    );
    assert_eq!(table.pending_after, table.pending_before);
    assert_eq!(userdata.pending_after, userdata.pending_before);

    assert!(
        !userdata.dead_alive_after,
        "the exact userdata generation is gone after Release"
    );
    assert_eq!(
        userdata.released_generation,
        userdata.dead_generation + 1,
        "Release advances the specific userdata's old generation exactly once"
    );
    assert_eq!(
        userdata.chain_before.first(),
        Some(&(userdata.dead_slot, userdata.dead_generation)),
        "the probe userdata is the real live-chain head before Release"
    );
    assert!(
        !userdata
            .chain_after
            .contains(&(userdata.dead_slot, userdata.dead_generation)),
        "the old userdata identity is unlinked from the live chain"
    );
    let mut expected_chain = userdata.chain_before.clone();
    expected_chain.retain(|identity| *identity != (userdata.dead_slot, userdata.dead_generation));
    assert_eq!(
        userdata.chain_after, expected_chain,
        "Release removes only the dead userdata's own chain node"
    );
    assert_eq!(
        table.chain_after, table.chain_before,
        "the table control leaves every live-userdata chain node in place"
    );

    assert_eq!(
        userdata.phase_before,
        heap::SavedTailPhase::Release,
        "the userdata probe starts in the Release window, not a token close or an empty step"
    );
    assert_eq!(userdata.cursor_before, 0);
    assert_eq!(userdata.dead_len_before, 1);
    assert_eq!(userdata.phase_after, heap::SavedTailPhase::Release);
    assert_eq!(userdata.cursor_after, 1);
    assert_eq!(
        userdata.token_closes, 0,
        "the ordinary dead userdata carries no bound token to close"
    );
    assert_eq!(table.phase_before, heap::SavedTailPhase::Release);
    assert_eq!(table.cursor_before, 0);
    assert_eq!(table.dead_len_before, 1);
    assert_eq!(table.cursor_after, 1);
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
/// `live_reads` is the delta of the real live-userdata chain-read trace for
/// this exact pulse, so it is the chain cursor's identity sequence, not a
/// scan-slot or selected-record count.
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
    live_reads: Vec<(u32, u32)>,
}

/// Drives the private selecting mark one charged unit at a time. Each call
/// records the real edge meter and the real live-chain trace delta at its own
/// start and end.
fn pulse_steps(heap: &mut heap::LuaHeap, limit: usize) -> (Vec<PulseStep>, ScanMeter) {
    let mut session = heap::SelectingPendingMark::start(heap);
    let mut previous = session.meter();
    let mut steps = Vec::new();
    for _ in 0..limit {
        let before = previous;
        let (finished, after, live_reads) = session.pulse_with_live_trace(1);
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
            live_reads,
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
                // its own `__index` field. The raw lookup must read that
                // record, not follow its table value.
                let missing_metatable = heap.alloc()?;
                let index_table = heap.alloc()?;
                heap.set_named_scalar(index_table, "__gc", Value::Bool(false))?;
                heap.set_named_edge(missing_metatable, "__index", index_table)?;
                heap.set_userdata_metatable(missing, Some(missing_metatable))?;

                // The nil arm stores the raw field itself as a real value
                // record holding null. This is the physical cursor gap the
                // weak publication leaves, not a deleted field.
                let nil_metatable = heap.alloc()?;
                heap.set_named_gap_test_control(nil_metatable, "__gc")?;
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

        // Before the collection, distinguish a deleted field from a stored
        // null gap through the real cursor walk. The gap key resolves and the
        // walk continues; the missing key is not in the chain at all.
        heap::with_heap(&mut host, |heap| {
            assert_eq!(
                heap.next_guest(nil_metatable, &Value::string("__gc")),
                Ok(None),
                "the stored null __gc record is a real cursor entry"
            );
            assert_eq!(
                heap.next_guest(missing_metatable, &Value::string("__gc")),
                Err(HeapError::NextCursorMissing),
                "the missing arm has no __gc record"
            );
            heap.clear_pending_gc_edge_trace();
            Ok(())
        })
        .expect("gap presence");

        let _ = heap::take_scan_meter(&mut host);
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        let meter = heap::take_scan_meter(&mut host).expect("gap meter");
        let trace =
            heap::with_heap(&mut host, |heap| Ok(heap.pending_gc_edge_trace())).expect("gap trace");
        assert!(pending_records(&mut host).is_empty());
        assert!(
            finalizer_settled(&mut host, missing),
            "an absent raw __gc settles the white userdata without following __index"
        );
        assert!(
            finalizer_settled(&mut host, nil),
            "a stored null raw __gc gap settles the white userdata"
        );
        assert!(
            !pending_flag(&mut host, missing) && !pending_flag(&mut host, nil),
            "neither arm was queued as a raw-__gc candidate"
        );
        assert_eq!(
            meter.pending_finalizer_gc_lookups, 2,
            "the two metatables are looked up independently, with no shared cache"
        );
        assert_eq!(
            meter.pending_finalizer_gc_edge_checks, 2,
            "one real record read per metatable: the __index record and the stored __gc gap"
        );
        let missing_trace: Vec<_> = trace
            .iter()
            .filter(|(slot, _)| *slot == missing_metatable.slot())
            .collect();
        let gap_trace: Vec<_> = trace
            .iter()
            .filter(|(slot, _)| *slot == nil_metatable.slot())
            .collect();
        assert_eq!(missing_trace.len(), 1, "the missing arm reads __index");
        assert_eq!(
            gap_trace.len(),
            1,
            "the gap arm reads the stored __gc record"
        );
        assert!(
            trace.iter().all(|(slot, _)| *slot != index_table.slot()),
            "the __index value's owner chain is never opened"
        );
        assert_eq!(
            meter.settlement_journal_appends, 2,
            "each settled identity appends exactly one journal node"
        );
        assert_eq!(
            meter.settlement_journal_visits, 0,
            "a successful publication drops the journal head without visiting nodes"
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.settlement_journal_len()))
                .expect("journal length"),
            0
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
        let _ = heap::take_scan_meter(&mut host);
        let report = finish_release(&mut host);
        let release_meter = heap::take_scan_meter(&mut host).expect("release meter");
        assert_eq!(
            release_meter.settlement_journal_appends, 0,
            "Release does not append a settlement node"
        );
        assert_eq!(
            release_meter.settlement_journal_visits, 0,
            "Release does not read the settlement journal"
        );
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

        // The failed batch: C2 and C1 are selected before A, which is a real
        // raw-`__gc` candidate; C1 and C2 have no raw field. B owns the real
        // token used for the same-type misbinding.
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
            meter.settlement_journal_appends, 2,
            "the two new settlements were journalled before the refusal"
        );
        assert_eq!(
            meter.settlement_journal_visits, 2,
            "the rollback visits exactly the two nodes this pass appended"
        );
        let trace = heap::with_heap(&mut host, |heap| Ok(heap.take_settlement_journal_trace()))
            .expect("settlement journal trace");
        assert_eq!(
            trace,
            vec![
                (c_first.userdata.slot(), c_first.userdata.generation()),
                (c_second.userdata.slot(), c_second.userdata.generation()),
            ],
            "the rollback visits this pass's nodes in journal order, newest first; old settled S is untouched"
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.settlement_journal_len()))
                .expect("journal length"),
            0,
            "a failed verification drops the journal head"
        );
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
        let retry_meter = heap::take_scan_meter(&mut host).expect("retry meter");
        assert_eq!(
            retry_meter.settlement_journal_appends, 0,
            "the retry selects both repaired candidates as pending, not settled"
        );
        assert_eq!(
            retry_meter.settlement_journal_visits, 0,
            "a successful retry drops its journal head without visiting nodes"
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.settlement_journal_len()))
                .expect("retry journal length"),
            0
        );
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

/// Doomed-close rollback after a real successful prefix. A is a valid prior
/// pending record and S is a valid prior settled generation rooted by the same
/// context. E is a doomed token published before D, so E really closes before
/// D's same-type misbinding is rejected. C is settled by this same selecting
/// pass. The failure must keep E's close, remove only C's new settlement,
/// preserve A, S, and every earlier state, publish nothing, and save no
/// release tail. The same context then retries: E is not closed again, D
/// closes, and C is reselected as pending after the binding and raw field are
/// repaired.
#[test]
fn settlement_rolls_back_at_doomed_close_and_retry_selects() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, settlement_limits()).expect("install");

        // Prior pending record A.
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
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        finish_release(&mut host);
        assert_eq!(
            pending_records(&mut host),
            vec![(a_raw, a.slot(), a.generation())]
        );

        // Prior settled S: A0's raw-`__gc` metatable links S during the
        // second mark, so S is white at selection time and settled, then
        // survives because the real second mark reaches it. Root it after.
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
        assert!(finalizer_settled(&mut host, s));
        assert_eq!(pending_records(&mut host).len(), 2);
        let s_frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(1)?;
            heap.write_userdata_root(frame, 0, s)?;
            Ok(frame)
        })
        .expect("S root");

        // Failing batch: E is doomed and published before D; D carries C's
        // real same-type token. C is white with no raw `__gc` and settles.
        let e = heap::publish_userdata(&mut host).expect("prefix E");
        let e_raw = bound_raw(&mut host, e);
        let d = heap::publish_userdata(&mut host).expect("failing D");
        let d_raw = bound_raw(&mut host, d);
        let c = publish_missing_eligible(&mut host);
        assert!(
            e.slot() < d.slot() && d.slot() < c.userdata.slot(),
            "E must close before D fails, and C is selected after D"
        );
        let c_frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(1)?;
            heap.write_root(frame, 0, c.weak_values)?;
            Ok(frame)
        })
        .expect("C root");
        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(d)?;
            heap.bind_userdata_token(d, c.raw)
        })
        .expect("same-type misbind on the doomed slot");

        let _ = heap::take_scan_meter(&mut host);
        let error = heap::collect_pending_finalizers(&mut host).expect_err("doomed close mismatch");
        assert_eq!(error, HeapError::TokenMismatch);
        let meter = heap::take_scan_meter(&mut host).expect("close refusal meter");
        assert_eq!(
            meter.token_closes, 1,
            "the real E prefix closes exactly once before D is rejected"
        );
        assert_eq!(
            meter.settlement_journal_appends, 1,
            "C's single new settlement was journalled"
        );
        assert_eq!(
            meter.settlement_journal_visits, 1,
            "the rollback visits exactly C's journal node"
        );
        let trace = heap::with_heap(&mut host, |heap| Ok(heap.take_settlement_journal_trace()))
            .expect("settlement journal trace");
        assert_eq!(
            trace,
            vec![(c.userdata.slot(), c.userdata.generation())],
            "only the new C settlement is visited; prior S is untouched"
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.settlement_journal_len()))
                .expect("journal length"),
            0
        );
        assert_eq!(
            pending_records(&mut host),
            vec![
                (a_raw, a.slot(), a.generation()),
                (a0_raw, a0.slot(), a0.generation()),
            ],
            "both prior pending records survive the close failure"
        );
        assert!(pending_flag(&mut host, a));
        assert!(pending_flag(&mut host, a0));
        assert!(
            finalizer_settled(&mut host, s),
            "valid prior settled state is not erased by the failed batch"
        );
        assert!(
            !finalizer_settled(&mut host, c.userdata),
            "a failed doomed close must roll back C's new settlement"
        );
        assert!(!heap::saved_sweep_tail(&mut host).expect("no tail"));
        assert_eq!(
            weak_value(&mut host, weak_values, 1.0),
            None,
            "A's weak value was already cleared by its prior successful publication"
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
        assert!(
            !userdata_token_open(&host, e_raw),
            "the prefix close is real"
        );
        assert!(
            heap::with_heap(&mut host, |heap| heap.published_userdata_token(e)).is_err(),
            "the closed prefix binding is cleared"
        );
        assert_eq!(
            slot_generation(&mut host, e.slot()),
            e.generation(),
            "no release ran, so the closed prefix slot keeps its generation"
        );
        assert!(userdata_token_open(&host, a_raw));
        assert!(userdata_token_open(&host, a0_raw));
        assert!(userdata_token_open(&host, s_raw));
        assert!(userdata_token_open(&host, c.raw));
        assert!(userdata_token_open(&host, d_raw));
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.published_userdata_token(d))
                .expect("D keeps the failing binding"),
            c.raw
        );

        // An ordinary entry with the same bad binding must also refuse before
        // creating a fake record or publishing. E has no binding now, so the
        // ordinary close reaches D and rejects it without closing C.
        let control = heap::collect(&mut host).expect_err("ordinary close rejects the misbind");
        assert_eq!(control, HeapError::TokenMismatch);
        assert_eq!(
            pending_records(&mut host),
            vec![
                (a_raw, a.slot(), a.generation()),
                (a0_raw, a0.slot(), a0.generation()),
            ]
        );
        assert!(!finalizer_settled(&mut host, c.userdata));
        assert!(userdata_token_open(&host, c.raw));
        assert_eq!(weak_value(&mut host, c.weak_values, 1.0), Some(c.userdata));
        assert!(!heap::saved_sweep_tail(&mut host).expect("no tail after control"));

        // Fix D, install the positive-control raw field on C, and retry in
        // the same context. E is already closed and must not close again.
        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(d)?;
            heap.bind_userdata_token(d, d_raw)?;
            heap.set_named_scalar(c.metatable, "__gc", Value::Bool(false))
        })
        .expect("restore D and install C's raw __gc");
        let _ = heap::take_scan_meter(&mut host);
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        let retry_meter = heap::take_scan_meter(&mut host).expect("retry meter");
        assert_eq!(
            retry_meter.token_closes, 1,
            "only D closes on retry; the already closed E prefix is not retried"
        );
        assert_eq!(
            retry_meter.settlement_journal_appends, 0,
            "the repaired C is selected as pending, not settled"
        );
        assert_eq!(
            retry_meter.settlement_journal_visits, 0,
            "the successful retry drops its journal head without visiting nodes"
        );
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
            weak_value(&mut host, weak_values, 1.0),
            None,
            "A's weak value is published now"
        );
        assert_eq!(
            weak_key(&mut host, weak_keys, a),
            Value::string("A"),
            "A remains a marked pending key"
        );
        finish_release(&mut host);
        assert!(!userdata_token_open(&host, d_raw));
        for frame in [c_frame, s_frame, frame] {
            heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave roots");
        }
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

/// A settled generation that is kept alive by a real root is cleared from the
/// string-keyed and object-keyed hash values of a `k`-only table, while its
/// marked weak key, a non-finalized value, and an ordinary strong table keep
/// the same object. Selection must publish the real weak edge in a cycle with
/// no dead slot and no new settlement.
///
/// Upstream `gc_clearweak` checks every hash node of a marked weak table, not
/// only a value-weak one (`lj_gc.c:456-470,488-497`). This heap has one owner
/// chain, so string keys (including the reserved guest `__mode` field),
/// object keys, and the deterministic numeric hash keys (negative and
/// non-integer values) are the branches it can prove. Zero, negative zero,
/// and positive integers may name a dense array slot in LuaJIT; that split is
/// outside this slice, so those numeric values keep the previous rule and are
/// not claimed here.
#[test]
fn settled_generation_clears_k_hash_values_while_strong_controls_keep() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let seed = seed_settlement(&mut host, SettlementArm::Linked, false);
        assert_first_settlement_publication(&mut host, &seed, SettlementArm::Linked);
        assert_release_after_settlement(&mut host, &seed);

        // Pending control from the same real selection path: A is still a
        // queued pending finalizer (no callback runs), so its k-only string
        // hash value must be cleared as finalized by the next publication.
        let (pending_k, pending_k_frame) = heap::with_heap(&mut host, |heap| {
            let table = weak_table(heap, "k")?;
            heap.set_named_userdata(table, "a", seed.a)?;
            let frame = heap.enter_roots(1)?;
            heap.write_root(frame, 0, table)?;
            Ok((table, frame))
        })
        .expect("pending k control");
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.named_value(pending_k, "a")))
                .expect("pending k value"),
            Ok(Value::Null),
            "a separated pending finalizer is finalized for the k-hash value check"
        );
        assert!(pending_flag(&mut host, seed.a));
        assert!(userdata_token_open(&host, seed.a_raw));
        finish_release(&mut host);

        // Consume A and root B: B remains a live settled generation with no
        // pending record. The next cycle releases A's dead graph.
        heap::retire_pending_finalizer_test_control(&mut host, seed.a).expect("consume A");
        let b_frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(1)?;
            heap.write_userdata_root(frame, 0, seed.b)?;
            Ok(frame)
        })
        .expect("B root");
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        finish_release(&mut host);
        assert!(finalizer_settled(&mut host, seed.b));
        assert!(userdata_token_open(&host, seed.b_raw));
        assert_eq!(
            slot_generation(&mut host, seed.b.slot()),
            seed.b.generation()
        );

        // A second typed userdata with no metatable is the non-finalized
        // control value. The strong table is the ordinary-table control.
        let normal = heap::publish_userdata(&mut host).expect("normal userdata");
        let normal_raw = bound_raw(&mut host, normal);
        let (k_table, strong, k_frame) = heap::with_heap(&mut host, |heap| {
            let k_table = weak_table(heap, "k")?;
            let strong = heap.alloc()?;
            heap.set_named_userdata(k_table, "str", seed.b)?;
            heap.set_userdata_key_userdata(k_table, normal, seed.b)?;
            heap.set_named_userdata(k_table, "normal", normal)?;
            heap.set_userdata_key_scalar(k_table, seed.b, Value::string("B-key"))?;
            heap.set_named_userdata(strong, "str", seed.b)?;
            let frame = heap.enter_roots(3)?;
            heap.write_root(frame, 0, k_table)?;
            heap.write_root(frame, 1, strong)?;
            heap.write_userdata_root(frame, 2, normal)?;
            Ok((k_table, strong, frame))
        })
        .expect("k-hash lattice");

        // All three hash-value records hold a userdata before publication.
        heap::with_heap(&mut host, |heap| {
            assert_eq!(
                heap.named_value(k_table, "str"),
                Err(HeapError::WrongKind),
                "the string-key value is a userdata record"
            );
            assert_eq!(
                heap.userdata_key_value(k_table, normal),
                Err(HeapError::WrongKind),
                "the object-key value is a userdata record"
            );
            assert_eq!(
                heap.named_value(k_table, "normal"),
                Err(HeapError::WrongKind),
                "the non-finalized control value is present"
            );
            assert_eq!(
                heap.named_value(strong, "str"),
                Err(HeapError::WrongKind),
                "the ordinary strong table keeps B"
            );
            assert_eq!(
                heap.userdata_key_value(k_table, seed.b),
                Ok(Value::string("B-key")),
                "B is a marked weak key in the same k table"
            );
            Ok(())
        })
        .expect("before publication");

        let _ = heap::take_scan_meter(&mut host);
        let boundary = publish_boundary(&mut host);
        let (window_heads, window_edges) = match boundary {
            PublishedBoundary::Published {
                window_heads,
                window_edges,
            } => (window_heads, window_edges),
            PublishedBoundary::Completed(_) => {
                panic!("the settled generation must force a real weak publication")
            }
        };
        assert!(
            window_heads > 0 && window_edges > 0,
            "the Owners walk actually published weak edges"
        );
        let meter = heap::take_scan_meter(&mut host).expect("k-hash meter");
        assert_eq!(
            meter.settlement_journal_appends, 0,
            "no identity was newly settled in this cycle"
        );
        heap::with_heap(&mut host, |heap| {
            assert_eq!(
                heap.named_value(k_table, "str"),
                Ok(Value::Null),
                "the k-only string hash value of the settled B is cleared"
            );
            assert_eq!(
                heap.userdata_key_value(k_table, normal),
                Ok(Value::Null),
                "the k-only object-key value of the settled B is cleared"
            );
            assert_eq!(
                heap.named_value(k_table, "normal"),
                Err(HeapError::WrongKind),
                "a non-finalized userdata value stays"
            );
            assert_eq!(
                heap.named_value(strong, "str"),
                Err(HeapError::WrongKind),
                "an ordinary strong table keeps its B value"
            );
            assert_eq!(
                heap.userdata_key_value(k_table, seed.b),
                Ok(Value::string("B-key")),
                "a marked weak key keeps its scalar value"
            );
            Ok(())
        })
        .expect("after publication");
        assert_eq!(
            weak_key(&mut host, seed.weak_keys, seed.b),
            Value::string("B"),
            "the separate weak-key table keeps B because the real root marks it"
        );
        assert!(finalizer_settled(&mut host, seed.b));
        assert!(userdata_token_open(&host, seed.b_raw));
        assert!(userdata_token_open(&host, normal_raw));
        assert_eq!(
            slot_generation(&mut host, seed.b.slot()),
            seed.b.generation()
        );

        let _ = heap::take_scan_meter(&mut host);
        finish_release(&mut host);
        let release_meter = heap::take_scan_meter(&mut host).expect("release meter");
        assert_eq!(
            release_meter.settlement_journal_appends, 0,
            "Release appends no settlement node"
        );
        assert_eq!(
            release_meter.settlement_journal_visits, 0,
            "Release does not read the settlement journal"
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.settlement_journal_len()))
                .expect("journal length"),
            0
        );
        assert!(alive(&mut host, k_table));
        assert!(alive(&mut host, strong));
        assert!(alive(&mut host, seed.b_metatable));
        assert!(alive(&mut host, seed.b_graph));
        heap::with_heap(&mut host, |heap| heap.leave_roots(k_frame)).expect("leave k roots");
        heap::with_heap(&mut host, |heap| heap.leave_roots(b_frame)).expect("leave B root");
        heap::with_heap(&mut host, |heap| heap.leave_roots(pending_k_frame))
            .expect("leave pending k roots");
        heap::with_heap(&mut host, |heap| heap.leave_roots(seed.weak_frame))
            .expect("leave weak roots");
    });
}

/// One target key and the real `next` pair that follows it. The target value
/// is a scalar when the pair is captured; the caller then replaces it with a
/// userdata in the same record.
struct CursorPair {
    table: TableRef,
    cursor: Value,
    successor: Value,
    successor_value: Value,
}

#[derive(Clone, Copy)]
enum CursorKey {
    Named(&'static str),
    Number(f64),
}

/// Creates one `k`-only table with a scalar target and a scalar successor,
/// walks both with the real guest `next`, and returns the original cursor and
/// the successor pair. The caller changes the target record to a userdata
/// afterwards, so the published continuation starts from a genuinely obtained
/// key rather than a constructed or rawget-derived value.
fn build_cursor_pair(
    heap: &mut heap::LuaHeap,
    key: CursorKey,
    target_seed: &str,
    successor_name: &str,
    successor_value: &str,
) -> Result<CursorPair, HeapError> {
    let table = weak_table(heap, "k")?;
    match key {
        CursorKey::Named(name) => {
            heap.set_named_scalar(table, name, Value::string(target_seed))?;
        }
        CursorKey::Number(number) => {
            heap.set_number_scalar(table, number, Value::string(target_seed))?;
        }
    }
    heap.set_named_scalar(table, successor_name, Value::string(successor_value))?;
    let (cursor, seed_value) = heap
        .next_guest(table, &Value::Null)?
        .expect("the pre-write scalar target is the first entry");
    assert!(
        matches!(seed_value, Value::String(_)),
        "the captured target is the pre-write scalar"
    );
    let (successor, successor_value) = heap
        .next_guest(table, &cursor)?
        .expect("the real next reaches the scalar successor");
    assert!(
        matches!(successor_value, Value::String(_)),
        "the successor is the pre-write scalar"
    );
    Ok(CursorPair {
        table,
        cursor,
        successor,
        successor_value,
    })
}

/// A pending and a settled userdata are value-dead in a `k`-only table under
/// the reserved guest `__mode` field, a negative integer, and a non-integer
/// hash key. The three classes are separate records, so each clearing is
/// reached on its own. Zero, negative zero, and positive integers are not
/// classified by number type: their array/hash split depends on the actual
/// table layout this slice does not model, so they keep the previous
/// strong-value rule. A non-finalized userdata and an ordinary strong table
/// keep the same object. Boolean keys have no representation here.
///
/// A settled and a queued identity must first be produced by a real cycle:
/// a rooted `k` table's strong value would keep a fresh userdata marked, so
/// it could never become white. The publication checked here is the first
/// one that carries these hash edges after that settlement cycle.
#[test]
fn k_hash_clears_mode_field_and_deterministic_numbers_only() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let seed = seed_settlement(&mut host, SettlementArm::Linked, false);
        let boundary = publish_boundary(&mut host);
        assert!(
            matches!(boundary, PublishedBoundary::Published { .. }),
            "the linked settlement cycle really queues A and settles B"
        );
        assert!(pending_flag(&mut host, seed.a));
        assert!(finalizer_settled(&mut host, seed.b));
        let _ = finish_release(&mut host);
        assert!(userdata_token_open(&host, seed.a_raw));
        assert!(userdata_token_open(&host, seed.b_raw));

        let normal = heap::publish_userdata(&mut host).expect("non-finalized userdata");
        let normal_raw = bound_raw(&mut host, normal);
        let (k_table, strong, mode_meta) = heap::with_heap(&mut host, |heap| {
            let k_table = weak_table(heap, "k")?;
            heap.set_named_userdata(k_table, "__mode", seed.a)?;
            heap.set_number_userdata(k_table, -17.0, seed.b)?;
            heap.set_number_userdata(k_table, 17.5, seed.a)?;
            heap.set_named_userdata(k_table, "normal", normal)?;
            heap.set_userdata_key_scalar(k_table, seed.b, Value::string("B-key"))?;
            heap.set_number_userdata(k_table, 1.0, seed.b)?;
            heap.set_number_userdata(k_table, 0.0, seed.b)?;
            let strong = heap.alloc()?;
            heap.set_named_userdata(strong, "plain", seed.b)?;
            let mode_meta = heap.metatable(k_table)?.expect("k table metatable");
            Ok((k_table, strong, mode_meta))
        })
        .expect("k hash lattice");
        let frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(2)?;
            heap.write_root(frame, 0, k_table)?;
            heap.write_root(frame, 1, strong)?;
            Ok(frame)
        })
        .expect("k hash roots");

        // Before the publication that first carries these records, every
        // claimed entry holds the finalized identity and the anti-controls
        // hold theirs.
        heap::with_heap(&mut host, |heap| {
            assert_eq!(
                heap.named_value(k_table, "__mode"),
                Err(HeapError::WrongKind),
                "the reserved __mode record holds the pending A"
            );
            assert_eq!(heap.number_userdata(k_table, -17.0), Ok(Some(seed.b)));
            assert_eq!(heap.number_userdata(k_table, 17.5), Ok(Some(seed.a)));
            assert_eq!(
                heap.named_value(k_table, "normal"),
                Err(HeapError::WrongKind),
                "the non-finalized value is present"
            );
            assert_eq!(heap.number_userdata(k_table, 1.0), Ok(Some(seed.b)));
            assert_eq!(heap.number_userdata(k_table, 0.0), Ok(Some(seed.b)));
            assert_eq!(
                heap.userdata_key_value(k_table, seed.b),
                Ok(Value::string("B-key")),
                "B is a marked weak key of the same table"
            );
            assert_eq!(
                heap.named_value(strong, "plain"),
                Err(HeapError::WrongKind),
                "the ordinary strong table keeps B"
            );
            Ok(())
        })
        .expect("before first publication");

        let boundary = publish_boundary(&mut host);
        assert!(
            matches!(boundary, PublishedBoundary::Published { .. }),
            "the pending and settled identities force a real weak publication"
        );
        assert!(pending_flag(&mut host, seed.a));
        assert!(finalizer_settled(&mut host, seed.b));

        heap::with_heap(&mut host, |heap| {
            assert_eq!(
                heap.named_value(k_table, "__mode"),
                Ok(Value::Null),
                "the pending A under the guest __mode string key is cleared"
            );
            assert_eq!(
                heap.number_value(k_table, -17.0),
                Ok(Value::Null),
                "the settled B under a negative integer hash key is cleared"
            );
            assert_eq!(
                heap.number_value(k_table, 17.5),
                Ok(Value::Null),
                "the pending A under a non-integer hash key is cleared"
            );
            assert_eq!(
                heap.named_value(k_table, "normal"),
                Err(HeapError::WrongKind),
                "a non-finalized userdata value stays"
            );
            assert_eq!(
                heap.number_userdata(k_table, 1.0),
                Ok(Some(seed.b)),
                "a positive integer stays on the unclaimed array/hash branch"
            );
            assert_eq!(
                heap.number_userdata(k_table, 0.0),
                Ok(Some(seed.b)),
                "zero stays on the unclaimed array/hash branch"
            );
            assert_eq!(
                heap.number_userdata(k_table, -0.0),
                Ok(Some(seed.b)),
                "-0 selects the same zero record and also stays"
            );
            assert_eq!(
                heap.userdata_key_value(k_table, seed.b),
                Ok(Value::string("B-key")),
                "clearing the __mode value does not change the weak-key side"
            );
            assert_eq!(
                heap.named_value(mode_meta, "__mode"),
                Ok(Value::string("k")),
                "the metatable still declares the k-only mode"
            );
            assert_eq!(
                heap.named_value(strong, "plain"),
                Err(HeapError::WrongKind),
                "an ordinary strong table keeps the settled object"
            );
            Ok(())
        })
        .expect("after first publication");

        assert!(userdata_token_open(&host, seed.a_raw));
        assert!(userdata_token_open(&host, seed.b_raw));
        assert!(userdata_token_open(&host, normal_raw));
        let _ = finish_release(&mut host);
        assert!(alive(&mut host, k_table));
        assert!(alive(&mut host, strong));
        assert!(alive(&mut host, mode_meta));
        assert_eq!(
            slot_generation(&mut host, seed.a.slot()),
            seed.a.generation()
        );
        assert_eq!(
            slot_generation(&mut host, seed.b.slot()),
            seed.b.generation()
        );
        heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave k hash roots");
        heap::with_heap(&mut host, |heap| heap.leave_roots(seed.weak_frame))
            .expect("leave weak roots");
    });
}

/// In the first real published window that carries these hash edges, a
/// cursor taken from the real guest `next` continues past a cleared hash
/// value to the same scalar successor. The ordinary string cursor is the
/// regression that the minimal `clear_value` mutation breaks: the value read
/// is still nil because the record is dropped, but `next` at the original key
/// is no longer found. The reserved guest `__mode` field keeps its guest
/// cursor, while an ordinary internal field id stays out of guest iteration.
///
/// The settlement cycle runs first because a rooted `k` table's strong value
/// would keep a fresh userdata marked. The checked publication is the first
/// one that carries these hash edges after that cycle; the cursors are taken
/// by the real guest `next` before each target record is changed to the
/// finalized identity.
#[test]
fn k_hash_published_gaps_keep_real_cursors_to_scalar_successors() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let seed = seed_settlement(&mut host, SettlementArm::Linked, false);
        let boundary = publish_boundary(&mut host);
        assert!(
            matches!(boundary, PublishedBoundary::Published { .. }),
            "the linked settlement cycle really queues A and settles B"
        );
        assert!(pending_flag(&mut host, seed.a));
        assert!(finalizer_settled(&mut host, seed.b));
        let _ = finish_release(&mut host);
        let (string_pair, mode_pair, negative_pair, fraction_pair) =
            heap::with_heap(&mut host, |heap| {
                let string_pair = build_cursor_pair(
                    heap,
                    CursorKey::Named("field"),
                    "seed-one",
                    "after-string",
                    "string-tail",
                )?;
                let mode_pair = build_cursor_pair(
                    heap,
                    CursorKey::Named("__mode"),
                    "seed-two",
                    "after-mode",
                    "mode-tail",
                )?;
                let negative_pair = build_cursor_pair(
                    heap,
                    CursorKey::Number(-17.0),
                    "seed-three",
                    "after-negative",
                    "negative-tail",
                )?;
                let fraction_pair = build_cursor_pair(
                    heap,
                    CursorKey::Number(17.5),
                    "seed-four",
                    "after-fraction",
                    "fraction-tail",
                )?;
                Ok((string_pair, mode_pair, negative_pair, fraction_pair))
            })
            .expect("cursor lattice");

        // The cursors are the real guest keys the pre-write next returned.
        assert_eq!(string_pair.cursor, Value::string("field"));
        assert_eq!(mode_pair.cursor, Value::string("__mode"));
        assert_eq!(negative_pair.cursor, Value::Float(-17.0));
        assert_eq!(fraction_pair.cursor, Value::Float(17.5));

        let (mode_meta, weak_key_value) = heap::with_heap(&mut host, |heap| {
            heap.set_named_userdata(string_pair.table, "field", seed.a)?;
            heap.set_named_userdata(mode_pair.table, "__mode", seed.b)?;
            heap.set_number_userdata(negative_pair.table, -17.0, seed.a)?;
            heap.set_number_userdata(fraction_pair.table, 17.5, seed.b)?;
            heap.set_userdata_key_scalar(mode_pair.table, seed.b, Value::string("mode-key"))?;
            let mode_meta = heap
                .metatable(mode_pair.table)?
                .expect("mode table metatable");
            let weak_key_value = heap.userdata_key_value(mode_pair.table, seed.b)?;
            Ok((mode_meta, weak_key_value))
        })
        .expect("replace targets");
        assert_eq!(weak_key_value, Value::string("mode-key"));

        let frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(4)?;
            heap.write_root(frame, 0, string_pair.table)?;
            heap.write_root(frame, 1, mode_pair.table)?;
            heap.write_root(frame, 2, negative_pair.table)?;
            heap.write_root(frame, 3, fraction_pair.table)?;
            Ok(frame)
        })
        .expect("cursor roots");

        heap::with_heap(&mut host, |heap| {
            assert_eq!(
                heap.named_value(string_pair.table, "field"),
                Err(HeapError::WrongKind)
            );
            assert_eq!(
                heap.named_value(mode_pair.table, "__mode"),
                Err(HeapError::WrongKind)
            );
            assert_eq!(
                heap.number_userdata(negative_pair.table, -17.0),
                Ok(Some(seed.a))
            );
            assert_eq!(
                heap.number_userdata(fraction_pair.table, 17.5),
                Ok(Some(seed.b))
            );
            Ok(())
        })
        .expect("userdata targets before publication");

        let boundary = publish_boundary(&mut host);
        assert!(
            matches!(boundary, PublishedBoundary::Published { .. }),
            "the first real publication that carries the cursor tables"
        );
        assert!(pending_flag(&mut host, seed.a));
        assert!(finalizer_settled(&mut host, seed.b));

        heap::with_heap(&mut host, |heap| {
            assert_eq!(
                heap.named_value(string_pair.table, "field"),
                Ok(Value::Null),
                "rawget of the cleared string hash value is nil"
            );
            assert_eq!(
                heap.named_value(mode_pair.table, "__mode"),
                Ok(Value::Null),
                "rawget of the cleared reserved __mode value is nil"
            );
            assert_eq!(
                heap.number_value(negative_pair.table, -17.0),
                Ok(Value::Null),
                "rawget of the cleared negative hash value is nil"
            );
            assert_eq!(
                heap.number_value(fraction_pair.table, 17.5),
                Ok(Value::Null),
                "rawget of the cleared non-integer hash value is nil"
            );
            for pair in [&string_pair, &mode_pair, &negative_pair, &fraction_pair] {
                assert_eq!(
                    heap.next_guest(pair.table, &pair.cursor),
                    Ok(Some((pair.successor.clone(), pair.successor_value.clone()))),
                    "the original cursor continues at the same scalar successor"
                );
            }
            assert_eq!(
                heap.named_value(mode_meta, "__mode"),
                Ok(Value::string("k")),
                "clearing the table's own __mode entry leaves the metatable mode"
            );
            assert_eq!(
                heap.userdata_key_value(mode_pair.table, seed.b),
                Ok(Value::string("mode-key")),
                "the marked weak key still keeps its scalar value"
            );
            Ok(())
        })
        .expect("published cursor continuation");

        assert!(userdata_token_open(&host, seed.a_raw));
        assert!(userdata_token_open(&host, seed.b_raw));
        let _ = finish_release(&mut host);
        for pair in [&string_pair, &mode_pair, &negative_pair, &fraction_pair] {
            assert!(alive(&mut host, pair.table));
        }
        assert!(alive(&mut host, mode_meta));
        heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave cursor roots");
        heap::with_heap(&mut host, |heap| heap.leave_roots(seed.weak_frame))
            .expect("leave weak roots");

        // The reserved `__mode` field is the only internal field id that
        // guest iteration renders. Field id 1 stays internal: a table whose
        // first entry is that field is not guest-iterable, so no internal
        // field is exposed as a key.
        let internal = heap::with_heap(&mut host, |heap| {
            let table = heap.alloc()?;
            heap.set_field_scalar(table, 1, Value::string("internal"))?;
            Ok(table)
        })
        .expect("internal field table");
        assert_eq!(
            heap::with_heap(
                &mut host,
                |heap| Ok(heap.next_guest(internal, &Value::Null))
            )
            .expect("internal field iteration"),
            Err(HeapError::NextUnsupported),
            "an ordinary internal field id is not a guest key"
        );
    });
}

/// The first integer above LuaJIT's last possible array index is a
/// deterministic hash key: `LJ_MAX_ASIZE = (1 << 27) + 1` includes the zero
/// slot (`lj_def.h:60-61`) and `inarray` tests `index < asize`
/// (`lj_tab.h:79-84`), so `2^27 + 1` cannot be an array index. Under `k`, a
/// finalized value there is cleared, and the real numeric cursor obtained
/// before the write still continues to the same scalar successor. The last
/// possible index `2^27` stays on the existing unclaimed array/hash branch:
/// this slice does not model the actual layout, so the test does not claim
/// that a sparse LuaJIT table must retain it.
///
/// The settlement cycle runs first because a rooted `k` table's strong value
/// would keep a fresh userdata marked. The checked publication is the first
/// one that carries these edges after that cycle.
#[test]
fn k_hash_clears_the_first_integer_above_the_last_array_index() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let seed = seed_settlement(&mut host, SettlementArm::Linked, false);
        let boundary = publish_boundary(&mut host);
        assert!(
            matches!(boundary, PublishedBoundary::Published { .. }),
            "the linked settlement cycle really queues A and settles B"
        );
        assert!(pending_flag(&mut host, seed.a));
        assert!(finalizer_settled(&mut host, seed.b));
        let _ = finish_release(&mut host);

        const LAST_ARRAY_INDEX: f64 = 134_217_728.0; // (1 << 27)
        const FIRST_HASH_INTEGER: f64 = 134_217_729.0; // (1 << 27) + 1
        let (boundary_pair, layout_table) = heap::with_heap(&mut host, |heap| {
            let boundary_pair = build_cursor_pair(
                heap,
                CursorKey::Number(FIRST_HASH_INTEGER),
                "seed-boundary",
                "after-boundary",
                "boundary-tail",
            )?;
            let layout_table = weak_table(heap, "k")?;
            heap.set_number_userdata(layout_table, LAST_ARRAY_INDEX, seed.b)?;
            Ok((boundary_pair, layout_table))
        })
        .expect("boundary lattice");
        assert_eq!(boundary_pair.cursor, Value::Float(FIRST_HASH_INTEGER));

        let frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(2)?;
            heap.write_root(frame, 0, boundary_pair.table)?;
            heap.write_root(frame, 1, layout_table)?;
            Ok(frame)
        })
        .expect("boundary roots");

        heap::with_heap(&mut host, |heap| {
            heap.set_number_userdata(boundary_pair.table, FIRST_HASH_INTEGER, seed.a)?;
            assert_eq!(
                heap.number_userdata(boundary_pair.table, FIRST_HASH_INTEGER),
                Ok(Some(seed.a))
            );
            assert_eq!(
                heap.number_userdata(layout_table, LAST_ARRAY_INDEX),
                Ok(Some(seed.b))
            );
            Ok(())
        })
        .expect("boundary userdata targets");

        let boundary = publish_boundary(&mut host);
        assert!(
            matches!(boundary, PublishedBoundary::Published { .. }),
            "the first real publication that carries the boundary tables"
        );
        assert!(pending_flag(&mut host, seed.a));
        assert!(finalizer_settled(&mut host, seed.b));

        heap::with_heap(&mut host, |heap| {
            assert_eq!(
                heap.number_value(boundary_pair.table, FIRST_HASH_INTEGER),
                Ok(Value::Null),
                "the pending A above the last possible array index is cleared"
            );
            assert_eq!(
                heap.number_userdata(layout_table, LAST_ARRAY_INDEX),
                Ok(Some(seed.b)),
                "the last possible array index stays on the unclaimed layout branch"
            );
            assert_eq!(
                heap.next_guest(boundary_pair.table, &boundary_pair.cursor),
                Ok(Some((
                    boundary_pair.successor.clone(),
                    boundary_pair.successor_value.clone()
                ))),
                "the original numeric cursor continues at the same scalar successor"
            );
            Ok(())
        })
        .expect("boundary publication");

        assert!(userdata_token_open(&host, seed.a_raw));
        assert!(userdata_token_open(&host, seed.b_raw));
        let _ = finish_release(&mut host);
        assert!(alive(&mut host, boundary_pair.table));
        assert!(alive(&mut host, layout_table));
        heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave boundary roots");
        heap::with_heap(&mut host, |heap| heap.leave_roots(seed.weak_frame))
            .expect("leave weak roots");
    });
}

/// Per-unit journal witness across the growth points of the removed vector.
/// Nine white userdata with live empty metatables are settled by the real
/// selecting mark. The old design's `Vec::push` grew at four and eight
/// elements and could copy the batch prefix inside a `pulse(1)` unit. The
/// intrusive journal must append at most one slot node in any unit, visit no
/// node during the mark, and hold exactly the nine identities afterwards.
#[test]
fn settlement_journal_appends_one_node_per_pulse_across_old_capacity() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, settlement_limits()).expect("install");

        let mut candidates = Vec::new();
        for _ in 0..9 {
            let userdata = heap::publish_userdata(&mut host).expect("candidate");
            heap::with_heap(&mut host, |heap| {
                let metatable = heap.alloc()?;
                heap.set_userdata_metatable(userdata, Some(metatable))?;
                Ok(())
            })
            .expect("empty metatable");
            candidates.push(userdata);
        }
        let mut expected: Vec<(u32, u32)> = candidates
            .iter()
            .map(|userdata| (userdata.slot(), userdata.generation()))
            .collect();
        expected.sort_unstable();

        let receipt = heap::with_heap(&mut host, |heap| {
            let mut session = heap::SelectingPendingMark::start(heap);
            let mut previous = session.meter();
            let mut appended = 0u64;
            let mut max_unit_appends = 0u64;
            loop {
                let (finished, meter) = session.pulse(1);
                let unit_appends =
                    meter.settlement_journal_appends - previous.settlement_journal_appends;
                let unit_visits =
                    meter.settlement_journal_visits - previous.settlement_journal_visits;
                assert!(
                    unit_appends <= 1,
                    "one pulse unit appends at most one journal slot node"
                );
                assert_eq!(
                    unit_visits, 0,
                    "a mark pulse never rolls back the settlement journal"
                );
                max_unit_appends = max_unit_appends.max(unit_appends);
                appended += unit_appends;
                previous = meter;
                if finished {
                    break;
                }
            }
            let meter = session.meter();
            drop(session);
            let mut members = heap.settlement_journal_members();
            members.sort_unstable();
            let len = heap.settlement_journal_len();
            let settled = candidates.iter().all(|userdata| {
                heap.slot_finalizer_settled(*userdata)
                    .expect("settled flag")
            });
            Ok((meter, members, len, settled, appended, max_unit_appends))
        })
        .expect("pulse journal witness");

        let (meter, members, len, settled, appended, max_unit_appends) = receipt;
        assert!(
            settled,
            "every white candidate was settled by the real selecting scan"
        );
        assert_eq!(meter.settlement_journal_appends, 9);
        assert_eq!(appended, 9, "the per-unit sum is exactly nine appends");
        assert_eq!(
            max_unit_appends, 1,
            "no pulse unit appends more than one journal node"
        );
        assert_eq!(meter.settlement_journal_visits, 0);
        assert_eq!(len, 9);
        assert_eq!(
            members, expected,
            "the journal holds exactly the nine new identities"
        );
    });
}

fn userdata_alive(host: &mut vm::HostContext<'_>, userdata: UserdataRef) -> bool {
    heap::with_heap(host, |heap| Ok(heap.userdata_alive(userdata))).expect("userdata liveness")
}

fn finish_if_published(host: &mut vm::HostContext<'_>, boundary: PublishedBoundary) {
    if matches!(boundary, PublishedBoundary::Published { .. }) {
        finish_release(host);
    }
}

/// A successful private publication makes its queue prefix consumable. The
/// real handoff removes exactly that published head, roots its still-bound
/// typed token in a specified empty root slot, and settles that generation
/// once. A later ordinary collection must keep the token alive through that
/// root alone, keep the sibling pending record, and still collect an
/// independent weak orphan. Removing the root is the only step that lets a
/// later collection close the token and advance the slot generation; a
/// successor generation is never mistaken for the old one.
#[test]
fn published_head_hands_one_typed_token_to_a_root_and_settles_its_generation() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let second = publish_eligible(&mut host);
        let first = publish_eligible(&mut host);
        assert!(
            first.userdata.slot() > second.userdata.slot(),
            "the head is the later allocation and has the higher slot"
        );
        let frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(4)?;
            heap.write_root(frame, 0, first.weak_values)?;
            heap.write_root(frame, 1, second.weak_values)?;
            heap.write_root(frame, 2, first.metatable)?;
            Ok(frame)
        })
        .expect("initial roots");

        let resources_before = host.resource_count();
        let boundary = publish_boundary(&mut host);
        assert!(
            matches!(boundary, PublishedBoundary::Published { .. }),
            "the real weak publication must stop at the published boundary"
        );
        assert_eq!(
            published_count(&mut host),
            2,
            "both selected records are published by the successful collection"
        );
        assert_eq!(
            pending_records(&mut host),
            vec![
                (
                    first.raw,
                    first.userdata.slot(),
                    first.userdata.generation()
                ),
                (
                    second.raw,
                    second.userdata.slot(),
                    second.userdata.generation()
                )
            ],
            "the queue keeps the real selection order"
        );
        assert_eq!(
            weak_value(&mut host, first.weak_values, 1.0),
            None,
            "the first pending weak value is published as a gap"
        );
        assert_eq!(
            weak_value(&mut host, second.weak_values, 1.0),
            None,
            "the second pending weak value is published as a gap"
        );
        finish_release(&mut host);

        let taken = heap::take_published_pending_finalizer(&mut host, frame, 3)
            .expect("the published head is consumable");
        assert_eq!(taken, first.userdata);
        assert_eq!(
            pending_records(&mut host),
            vec![(
                second.raw,
                second.userdata.slot(),
                second.userdata.generation()
            )],
            "only the consumed head record disappeared"
        );
        assert!(
            !pending_flag(&mut host, first.userdata),
            "the consumed generation is no longer pending"
        );
        assert!(
            finalizer_settled(&mut host, first.userdata),
            "the consumed generation is settled exactly once"
        );
        assert!(
            pending_flag(&mut host, second.userdata),
            "the sibling record stays pending"
        );
        assert_eq!(
            published_count(&mut host),
            1,
            "the sibling head remains in the published prefix"
        );
        assert_eq!(
            rooted_userdata(&mut host, frame, 3),
            Some(first.userdata),
            "the handoff wrote the exact typed userdata into the empty root slot"
        );
        assert_eq!(
            host.resource_count(),
            resources_before,
            "the handoff moves no Core resource"
        );
        assert_eq!(
            heap::borrow_published_userdata(&host, first.userdata),
            Ok(first.userdata),
            "the root carries the exact typed token"
        );
        assert!(userdata_token_open(&host, first.raw));

        // This independent orphan is allocated after the handoff. Only the
        // later ordinary collection may reclaim it.
        let (orphan, orphan_weak, orphan_frame) = heap::with_heap(&mut host, |heap| {
            let orphan = heap.alloc()?;
            heap.set_edge(orphan, 1, orphan)?;
            let orphan_weak = weak_table(heap, "v")?;
            heap.set_number_edge(orphan_weak, 1.0, orphan)?;
            let frame = heap.enter_roots(1)?;
            heap.write_root(frame, 0, orphan_weak)?;
            Ok((orphan, orphan_weak, frame))
        })
        .expect("independent orphan");
        let _ = orphan_weak;

        let _ = heap::collect(&mut host).expect("ordinary collection after the handoff");
        assert!(
            userdata_alive(&mut host, first.userdata),
            "the newly published root keeps the consumed userdata alive"
        );
        assert_eq!(
            slot_generation(&mut host, first.userdata.slot()),
            first.userdata.generation(),
            "the rooted generation did not change"
        );
        assert!(userdata_token_open(&host, first.raw));
        assert_eq!(
            pending_records(&mut host),
            vec![(
                second.raw,
                second.userdata.slot(),
                second.userdata.generation()
            )],
            "the sibling pending record survives the ordinary collection"
        );
        assert!(pending_flag(&mut host, second.userdata));
        assert!(userdata_token_open(&host, second.raw));
        assert!(
            !alive(&mut host, orphan),
            "the independent weak orphan is actually collected"
        );
        assert_eq!(
            slot_generation(&mut host, orphan.slot()),
            orphan.generation() + 1,
            "the independent orphan's slot generation advanced"
        );

        // Only clearing that root lets the next real collection close the
        // consumed token and advance its slot generation.
        heap::with_heap(&mut host, |heap| heap.clear_root(frame, 3))
            .expect("clear handed-off root");
        let _ = heap::collect(&mut host).expect("ordinary collection after clearing the root");
        assert!(
            !userdata_alive(&mut host, first.userdata),
            "without the published root the consumed userdata is dead"
        );
        assert_eq!(
            heap::borrow_published_userdata(&host, first.userdata),
            Err(HeapError::StaleHandle),
            "the consumed typed binding is cleared only by that later collection"
        );
        assert!(
            !userdata_token_open(&host, first.raw),
            "the consumed token closes only after the root is gone"
        );
        assert_eq!(
            slot_generation(&mut host, first.userdata.slot()),
            first.userdata.generation() + 1,
            "the real release advances the consumed slot generation"
        );
        assert!(
            userdata_alive(&mut host, second.userdata),
            "the sibling record still retains its own userdata"
        );
        assert!(userdata_token_open(&host, second.raw));

        // A successor may reuse the freed slot. It must not inherit the old
        // generation's pending or settled state, and a later selection may
        // queue only the new generation.
        let fresh = heap::publish_userdata(&mut host).expect("successor userdata");
        assert_eq!(
            fresh.slot(),
            first.userdata.slot(),
            "the freed slot is reusable"
        );
        assert_eq!(
            fresh.generation(),
            first.userdata.generation() + 1,
            "the successor takes the next generation"
        );
        assert!(!pending_flag(&mut host, fresh));
        assert!(!finalizer_settled(&mut host, fresh));
        let fresh_metatable = heap::with_heap(&mut host, |heap| {
            let metatable = heap.alloc()?;
            heap.set_named_scalar(metatable, "__gc", Value::Bool(false))?;
            heap.set_userdata_metatable(fresh, Some(metatable))?;
            Ok(metatable)
        })
        .expect("successor metatable");
        let _ = fresh_metatable;

        let boundary = publish_boundary(&mut host);
        let records = pending_records(&mut host);
        assert_eq!(published_count(&mut host), records.len());
        assert!(
            records
                .iter()
                .any(|record| record.1 == fresh.slot() && record.2 == fresh.generation()),
            "the new generation is selected as its own record"
        );
        assert!(
            !records
                .iter()
                .any(|record| record.1 == first.userdata.slot()
                    && record.2 == first.userdata.generation()),
            "the old generation is never selected again"
        );
        finish_if_published(&mut host, boundary);
        heap::with_heap(&mut host, |heap| heap.leave_roots(orphan_frame))
            .expect("leave orphan roots");
        heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave initial roots");
    });
}

/// A failed doomed close keeps an already verified but unpublished candidate.
/// That candidate is not consumable until a later successful publication, and
/// the earlier published prefix stays usable. Resource, root-slot, stale-frame,
/// out-of-range, and saved-tail refusals must all happen before any dequeue
/// write. Repairing the real binding and retrying in the same context publishes
/// the candidate, after which the ordinary handoff consumes it.
#[test]
fn unpublished_candidate_survives_close_failure_and_retries_in_the_same_context() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let prior = publish_eligible(&mut host);
        let frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(4)?;
            heap.write_root(frame, 0, prior.weak_values)?;
            Ok(frame)
        })
        .expect("prior roots");
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        finish_release(&mut host);
        assert_eq!(published_count(&mut host), 1);

        let fresh = publish_eligible(&mut host);
        heap::with_heap(&mut host, |heap| {
            heap.write_root(frame, 1, fresh.weak_values)
        })
        .expect("fresh weak root");

        // A real typed userdata is doomed in this batch. Its slot is bound to
        // the fresh token, so the real close verifier refuses after selection
        // but before weak publication.
        let bad = heap::publish_userdata(&mut host).expect("doomed userdata");
        let bad_raw = bound_raw(&mut host, bad);
        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(bad)?;
            heap.bind_userdata_token(bad, fresh.raw)
        })
        .expect("same-type misbind");

        let error = heap::collect_pending_finalizers(&mut host).expect_err("close mismatch");
        assert_eq!(error, HeapError::TokenMismatch);
        assert_eq!(
            pending_records(&mut host),
            vec![
                (
                    prior.raw,
                    prior.userdata.slot(),
                    prior.userdata.generation()
                ),
                (
                    fresh.raw,
                    fresh.userdata.slot(),
                    fresh.userdata.generation()
                )
            ],
            "the verified new record is kept after the real close failure"
        );
        assert_eq!(
            published_count(&mut host),
            1,
            "only the earlier record is in the published prefix"
        );
        assert!(userdata_token_open(&host, bad_raw));
        assert!(userdata_token_open(&host, fresh.raw));
        assert!(
            !heap::saved_sweep_tail(&mut host).expect("no tail"),
            "a close failure publishes nothing"
        );

        // The prior record remains the only consumable head. The new candidate
        // stays behind it and is not touched by this handoff.
        let taken = heap::take_published_pending_finalizer(&mut host, frame, 2)
            .expect("the earlier published head stays consumable");
        assert_eq!(taken, prior.userdata);
        assert_eq!(
            pending_records(&mut host),
            vec![(
                fresh.raw,
                fresh.userdata.slot(),
                fresh.userdata.generation()
            )],
            "only the earlier head disappeared"
        );
        assert_eq!(published_count(&mut host), 0);
        assert_eq!(rooted_userdata(&mut host, frame, 2), Some(prior.userdata));

        let refused =
            heap::take_published_pending_finalizer(&mut host, frame, 3).expect_err("unpublished");
        assert_not_published(refused);
        assert_eq!(
            pending_records(&mut host),
            vec![(
                fresh.raw,
                fresh.userdata.slot(),
                fresh.userdata.generation()
            )],
            "an unpublished head refusal changes no record"
        );
        assert!(pending_flag(&mut host, fresh.userdata));
        assert!(!finalizer_settled(&mut host, fresh.userdata));
        assert!(userdata_token_open(&host, fresh.raw));
        assert_eq!(
            published_count(&mut host),
            0,
            "the unpublished candidate does not advance the ready prefix"
        );
        assert_eq!(rooted_userdata(&mut host, frame, 3), None);

        // Repair the real doomed binding and retry in this same context. The
        // earlier prefix was consumed; the fresh record is selected by the
        // earlier failed pass and becomes published now.
        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(bad)?;
            heap.bind_userdata_token(bad, bad_raw)
        })
        .expect("restore the doomed binding");
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        assert_eq!(published_count(&mut host), 1);
        assert_eq!(
            pending_records(&mut host),
            vec![(
                fresh.raw,
                fresh.userdata.slot(),
                fresh.userdata.generation()
            )]
        );
        assert_eq!(
            weak_value(&mut host, fresh.weak_values, 1.0),
            None,
            "the retry clears the newly published pending weak value"
        );

        // A saved Release tail owns the graph and refuses before any write.
        let refused =
            heap::take_published_pending_finalizer(&mut host, frame, 3).expect_err("saved tail");
        assert_eq!(refused, HeapError::SweepPending);
        assert_eq!(
            pending_records(&mut host),
            vec![(
                fresh.raw,
                fresh.userdata.slot(),
                fresh.userdata.generation()
            )]
        );
        assert!(userdata_token_open(&host, fresh.raw));
        finish_release(&mut host);

        // Occupied root slot: the anchor table and the queue are unchanged.
        assert_eq!(
            rooted_userdata(&mut host, frame, 3),
            None,
            "the target slot starts empty"
        );
        let occupied =
            heap::take_published_pending_finalizer(&mut host, frame, 0).expect_err("occupied root");
        assert_root_slot_occupied(occupied);
        assert!(alive(&mut host, prior.weak_values));
        assert_eq!(
            pending_records(&mut host),
            vec![(
                fresh.raw,
                fresh.userdata.slot(),
                fresh.userdata.generation()
            )]
        );
        assert_eq!(rooted_userdata(&mut host, frame, 3), None);

        // Out-of-range slot and stale frame are refused before any write.
        let refused =
            heap::take_published_pending_finalizer(&mut host, frame, 4).expect_err("slot range");
        assert_eq!(refused, HeapError::StaleFrame);
        let stale =
            heap::with_heap(&mut host, |heap| heap.enter_roots(1)).expect("temporary frame");
        heap::with_heap(&mut host, |heap| heap.leave_roots(stale)).expect("leave temporary frame");
        let refused =
            heap::take_published_pending_finalizer(&mut host, stale, 0).expect_err("stale frame");
        assert_eq!(refused, HeapError::StaleFrame);
        assert_eq!(
            pending_records(&mut host),
            vec![(
                fresh.raw,
                fresh.userdata.slot(),
                fresh.userdata.generation()
            )]
        );
        assert_eq!(rooted_userdata(&mut host, frame, 3), None);

        // A same-type token for another live userdata is refused by the exact
        // payload check. The queue, the published binding, and the root stay
        // unchanged until the real binding is restored.
        let decoy = heap::publish_userdata(&mut host).expect("decoy userdata");
        let decoy_raw = bound_raw(&mut host, decoy);
        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(fresh.userdata)?;
            heap.bind_userdata_token(fresh.userdata, decoy_raw)
        })
        .expect("same-type misbind on the head");
        let refused = heap::take_published_pending_finalizer(&mut host, frame, 3)
            .expect_err("typed token mismatch");
        assert_eq!(refused, HeapError::TokenMismatch);
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap
                .published_userdata_token(fresh.userdata))
            .expect("the failing binding is unchanged"),
            decoy_raw
        );
        assert_eq!(
            pending_records(&mut host),
            vec![(
                fresh.raw,
                fresh.userdata.slot(),
                fresh.userdata.generation()
            )]
        );
        assert_eq!(rooted_userdata(&mut host, frame, 3), None);

        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(fresh.userdata)?;
            heap.bind_userdata_token(fresh.userdata, fresh.raw)
        })
        .expect("restore the head binding");
        let _ = heap::take_scan_meter(&mut host);
        let taken = heap::take_published_pending_finalizer(&mut host, frame, 3)
            .expect("repaired head is consumable");
        assert_eq!(taken, fresh.userdata);
        let meter = heap::take_scan_meter(&mut host).expect("handoff meter");
        assert_eq!(
            meter.pending_queue_storage_moves, 1,
            "one front pop moves one stored record"
        );
        assert_eq!(
            meter.pending_release_queue_reads, 2,
            "the handoff reads the published head twice, independent of queue length"
        );
        assert!(pending_records(&mut host).is_empty());
        assert_eq!(published_count(&mut host), 0);
        assert!(
            finalizer_settled(&mut host, fresh.userdata),
            "the repaired candidate is settled by the handoff"
        );
        assert_eq!(rooted_userdata(&mut host, frame, 3), Some(fresh.userdata));
        assert!(userdata_token_open(&host, fresh.raw));
        heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave roots");
    });
}

/// The saved Release tail, an unpublished head, an occupied slot, a stale
/// frame, an out-of-range slot, and a wrong typed token are all refused before
/// the queue or the root slot is written. The successful repair then performs
/// exactly one front pop.
#[test]
fn handoff_refuses_before_any_write_and_then_dequeues_the_published_head() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let eligible = publish_eligible(&mut host);
        let (frame, anchor) = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(2)?;
            let anchor = heap.alloc()?;
            heap.write_root(frame, 0, anchor)?;
            Ok((frame, anchor))
        })
        .expect("frame with an occupied slot");

        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        let records_before = pending_records(&mut host);
        assert_eq!(published_count(&mut host), 1);

        let refused = heap::take_published_pending_finalizer(&mut host, frame, 1)
            .expect_err("saved release tail");
        assert_eq!(refused, HeapError::SweepPending);
        assert_eq!(pending_records(&mut host), records_before);
        assert_eq!(rooted_userdata(&mut host, frame, 1), None);
        assert!(userdata_token_open(&host, eligible.raw));
        finish_release(&mut host);

        let occupied =
            heap::take_published_pending_finalizer(&mut host, frame, 0).expect_err("occupied slot");
        assert_root_slot_occupied(occupied);
        assert_eq!(pending_records(&mut host), records_before);
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.rooted_table(frame, 0)).expect("anchor root"),
            Some(anchor),
            "the occupied root is unchanged"
        );
        assert_eq!(rooted_userdata(&mut host, frame, 1), None);
        assert!(userdata_token_open(&host, eligible.raw));

        let stale =
            heap::with_heap(&mut host, |heap| heap.enter_roots(1)).expect("temporary frame");
        heap::with_heap(&mut host, |heap| heap.leave_roots(stale)).expect("leave temporary frame");
        let refused =
            heap::take_published_pending_finalizer(&mut host, stale, 0).expect_err("stale frame");
        assert_eq!(refused, HeapError::StaleFrame);
        let refused =
            heap::take_published_pending_finalizer(&mut host, frame, 2).expect_err("slot range");
        assert_eq!(refused, HeapError::StaleFrame);
        assert_eq!(pending_records(&mut host), records_before);
        assert_eq!(rooted_userdata(&mut host, frame, 1), None);

        let decoy = heap::publish_userdata(&mut host).expect("decoy");
        let decoy_raw = bound_raw(&mut host, decoy);
        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(eligible.userdata)?;
            heap.bind_userdata_token(eligible.userdata, decoy_raw)
        })
        .expect("same-type misbind");
        let refused = heap::take_published_pending_finalizer(&mut host, frame, 1)
            .expect_err("typed mismatch");
        assert_eq!(refused, HeapError::TokenMismatch);
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap
                .published_userdata_token(eligible.userdata))
            .expect("failing binding unchanged"),
            decoy_raw
        );
        assert_eq!(pending_records(&mut host), records_before);
        assert_eq!(rooted_userdata(&mut host, frame, 1), None);

        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(eligible.userdata)?;
            heap.bind_userdata_token(eligible.userdata, eligible.raw)
        })
        .expect("restore binding");
        let _ = heap::take_scan_meter(&mut host);
        let taken = heap::take_published_pending_finalizer(&mut host, frame, 1)
            .expect("published head after repair");
        assert_eq!(taken, eligible.userdata);
        let meter = heap::take_scan_meter(&mut host).expect("handoff meter");
        assert_eq!(meter.pending_queue_storage_moves, 1);
        assert_eq!(meter.pending_release_queue_reads, 2);
        assert!(pending_records(&mut host).is_empty());
        assert_eq!(published_count(&mut host), 0);
        assert_eq!(
            rooted_userdata(&mut host, frame, 1),
            Some(eligible.userdata)
        );
        assert_eq!(
            heap::borrow_published_userdata(&host, eligible.userdata),
            Ok(eligible.userdata)
        );

        // No record remains, so the same entry now refuses without touching
        // the root or the queue.
        let refused = heap::take_published_pending_finalizer(&mut host, frame, 1)
            .expect_err("drained published prefix");
        assert_not_published(refused);
        assert_eq!(
            rooted_userdata(&mut host, frame, 1),
            Some(eligible.userdata)
        );
        assert!(userdata_token_open(&host, eligible.raw));
        heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave roots");
    });
}

/// The storage meter distinguishes a record read from a stored-record move.
/// Every front pop, at the first and the last queue length, reads exactly two
/// records and moves exactly one stored record. The queue length and the
/// number of remaining successors do not add a read or a move.
#[test]
fn published_front_pop_reads_and_moves_a_constant_amount_per_item() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(
            &mut host,
            HeapLimits {
                nodes: 256,
                roots: 2,
                edges: 512,
                max_generation: 8,
            },
        )
        .expect("install");
        let count = 24usize;
        let mut expected = Vec::with_capacity(count);
        for _ in 0..count {
            let eligible = publish_eligible(&mut host);
            expected.push((
                eligible.userdata,
                eligible.raw,
                eligible.userdata.slot(),
                eligible.userdata.generation(),
            ));
        }
        // The live chain publishes the newest allocation first, so the
        // provided queue order is the reverse of the allocation loop.
        expected.reverse();
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        assert_eq!(pending_records(&mut host).len(), count);
        assert_eq!(published_count(&mut host), count);
        finish_release(&mut host);

        let frame = heap::with_heap(&mut host, |heap| heap.enter_roots(1)).expect("one root slot");
        let resources = host.resource_count();
        for (index, (userdata, raw, slot, generation)) in expected.iter().copied().enumerate() {
            let _ = heap::take_scan_meter(&mut host);
            let taken = heap::take_published_pending_finalizer(&mut host, frame, 0)
                .expect("published front");
            let meter = heap::take_scan_meter(&mut host).expect("front-pop meter");
            assert_eq!(
                (taken.slot(), taken.generation()),
                (slot, generation),
                "front pop {index} returns the provided queue order"
            );
            assert_eq!(
                meter.pending_release_queue_reads, 2,
                "front pop {index} reads exactly the head, not a successor"
            );
            assert_eq!(
                meter.pending_queue_storage_moves, 1,
                "front pop {index} moves exactly one stored record"
            );
            assert_eq!(
                pending_records(&mut host).len(),
                count - index - 1,
                "front pop {index} shortens the queue by one"
            );
            assert_eq!(rooted_userdata(&mut host, frame, 0), Some(userdata));
            assert!(userdata_token_open(&host, raw));
            heap::with_heap(&mut host, |heap| heap.clear_root(frame, 0)).expect("clear root");
        }
        assert!(pending_records(&mut host).is_empty());
        assert_eq!(published_count(&mut host), 0);
        assert_eq!(host.resource_count(), resources);
        for (_, raw, _, _) in expected {
            assert!(
                userdata_token_open(&host, raw),
                "the handoff itself closes no typed token"
            );
        }
        heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave root");
    });
}

/// A saved mark owns the graph even when no Release tail exists. The handoff
/// must refuse it before reading the published queue or writing the root.
#[test]
fn handoff_refuses_a_saved_mark_before_reading_the_queue() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let eligible = publish_eligible(&mut host);
        let frame = heap::with_heap(&mut host, |heap| heap.enter_roots(1)).expect("root frame");
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        finish_release(&mut host);
        let records = pending_records(&mut host);
        assert_eq!(published_count(&mut host), 1);

        heap::open_saved_mark_for_test(&mut host).expect("saved mark");
        let refused =
            heap::take_published_pending_finalizer(&mut host, frame, 0).expect_err("saved mark");
        assert_eq!(refused, HeapError::MarkActive);
        assert_eq!(pending_records(&mut host), records);
        assert_eq!(published_count(&mut host), 1);
        assert_eq!(rooted_userdata(&mut host, frame, 0), None);
        assert!(userdata_token_open(&host, eligible.raw));
    });
}

// ---------------------------------------------------------------------------
// Private action dequeue: current raw `__gc` and one-layer raw `__call`
// ---------------------------------------------------------------------------

/// The receiver form of one real raw `__gc` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiverForm {
    Table,
    Userdata,
}

/// The exact original receiver identity stored under raw `__gc`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Receiver {
    Table(TableRef),
    Userdata(UserdataRef),
}

impl Receiver {
    fn without_call(self) -> PendingFinalizerNotCallable {
        match self {
            Self::Table(receiver) => PendingFinalizerNotCallable::TableWithoutCall(receiver),
            Self::Userdata(receiver) => PendingFinalizerNotCallable::UserdataWithoutCall(receiver),
        }
    }

    fn call_not_closure(self) -> PendingFinalizerNotCallable {
        match self {
            Self::Table(receiver) => PendingFinalizerNotCallable::TableCallNotClosure(receiver),
            Self::Userdata(receiver) => {
                PendingFinalizerNotCallable::UserdataCallNotClosure(receiver)
            }
        }
    }

    fn table(self) -> TableRef {
        match self {
            Self::Table(receiver) => receiver,
            Self::Userdata(_) => panic!("the receiver is a table"),
        }
    }

    fn userdata(self) -> UserdataRef {
        match self {
            Self::Userdata(receiver) => receiver,
            Self::Table(_) => panic!("the receiver is a userdata"),
        }
    }
}

struct ReceiverEligible {
    userdata: UserdataRef,
    raw: u64,
    metatable: TableRef,
    receiver: Receiver,
    receiver_metatable: TableRef,
    receiver_raw: Option<u64>,
    call: ClosureRef,
    capture: TableRef,
}

/// One real typed userdata whose metatable stores a raw `__gc` table or
/// userdata receiver. That receiver's own current metatable stores a raw
/// `__call` closure which holds one captured table through an upvalue.
/// Nothing here is rooted; the private pending selection marks the userdata
/// and follows the same strong graph.
fn publish_receiver_eligible(
    host: &mut vm::HostContext<'_>,
    form: ReceiverForm,
) -> ReceiverEligible {
    let userdata = heap::publish_userdata(host).expect("receiver-form userdata");
    let raw = bound_raw(host, userdata);
    let receiver_userdata = match form {
        ReceiverForm::Table => None,
        ReceiverForm::Userdata => Some(heap::publish_userdata(host).expect("receiver userdata")),
    };
    let receiver_raw = receiver_userdata.map(|receiver| bound_raw(host, receiver));
    let (metatable, receiver, receiver_metatable, call, capture) = heap::with_heap(host, |heap| {
        let metatable = heap.alloc()?;
        let receiver_metatable = heap.alloc()?;
        let call = heap.alloc_closure()?;
        let capture_cell = heap.alloc_upvalue()?;
        let capture = heap.alloc()?;
        heap.set_upvalue_edge(call, capture_cell)?;
        heap.write_upvalue(capture_cell, capture)?;
        heap.set_named_closure(receiver_metatable, "__call", call)?;
        let receiver = match form {
            ReceiverForm::Table => {
                let receiver = heap.alloc()?;
                heap.set_metatable(receiver, Some(receiver_metatable))?;
                heap.set_named_edge(metatable, "__gc", receiver)?;
                Receiver::Table(receiver)
            }
            ReceiverForm::Userdata => {
                let receiver = receiver_userdata.expect("published receiver");
                heap.set_userdata_metatable(receiver, Some(receiver_metatable))?;
                heap.set_named_userdata(metatable, "__gc", receiver)?;
                Receiver::Userdata(receiver)
            }
        };
        heap.set_userdata_metatable(userdata, Some(metatable))?;
        Ok((metatable, receiver, receiver_metatable, call, capture))
    })
    .expect("receiver-form lattice");
    ReceiverEligible {
        userdata,
        raw,
        metatable,
        receiver,
        receiver_metatable,
        receiver_raw,
        call,
        capture,
    }
}

/// Detaches the original `__gc` and `__call` source edges so only the dequeue
/// roots can keep the receiver, the call closure, and its capture alive.
fn detach_receiver_source_edges(
    heap: &mut heap::LuaHeap,
    head: &ReceiverEligible,
) -> Result<(), HeapError> {
    heap.set_named_scalar(head.metatable, "__gc", Value::Null)?;
    heap.set_named_scalar(head.receiver_metatable, "__call", Value::Null)
}

/// Publishes two real eligible userdata in one selecting pass and finishes the
/// Release tail. The live chain head is the later allocation, so `first` is the
/// published head.
fn publish_two_eligible(host: &mut vm::HostContext<'_>) -> (Eligible, Eligible) {
    let second = publish_eligible(host);
    let first = publish_eligible(host);
    assert!(
        first.userdata.slot() > second.userdata.slot(),
        "the live chain head has the higher allocation slot here"
    );
    let boundary = publish_boundary(host);
    assert!(matches!(boundary, PublishedBoundary::Published { .. }));
    assert_eq!(pending_records(host).len(), 2);
    assert_eq!(published_count(host), 2);
    finish_release(host);
    (first, second)
}

fn rooted_closure(
    host: &mut vm::HostContext<'_>,
    frame: RootFrameId,
    slot: u32,
) -> Option<ClosureRef> {
    heap::rooted_closure(host, frame, slot).expect("closure root slot")
}

fn rooted_table(host: &mut vm::HostContext<'_>, frame: RootFrameId, slot: u32) -> Option<TableRef> {
    heap::with_heap(host, |heap| heap.rooted_table(frame, slot)).expect("table root slot")
}

fn closure_alive(host: &mut vm::HostContext<'_>, closure: ClosureRef) -> bool {
    heap::with_heap(host, |heap| Ok(heap.closure_alive(closure))).expect("closure liveness")
}

/// Allocates one unreachable table with one real weak-value observer.
fn weak_orphan(host: &mut vm::HostContext<'_>) -> (TableRef, TableRef) {
    heap::with_heap(host, |heap| {
        let orphan = heap.alloc()?;
        heap.set_edge(orphan, 1, orphan)?;
        let weak = weak_table(heap, "v")?;
        heap.set_number_edge(weak, 1.0, orphan)?;
        Ok((orphan, weak))
    })
    .expect("weak orphan")
}

fn assert_needs_more_roots(error: HeapError) {
    match error {
        HeapError::Scope(message) => assert!(
            message.contains("needs more root slots"),
            "unexpected scope message: {message}"
        ),
        other => panic!("expected a root-plan scope error, got {other:?}"),
    }
}

fn assert_overlapping_roots(error: HeapError) {
    match error {
        HeapError::Scope(message) => assert!(
            message.contains("root slots overlap"),
            "unexpected scope message: {message}"
        ),
        other => panic!("expected an overlap scope error, got {other:?}"),
    }
}

/// A sibling's rewrite between the two dequeues is observed: the second head's
/// current raw `__gc` names a replacement closure, not the selection-time
/// `false`. The action roots the exact userdata and closure; with the source
/// field edge removed only the new roots keep them alive, and clearing those
/// roots releases both. One independent weak orphan dies in the same real
/// collection.
#[test]
fn published_action_re_reads_replaced_sibling_gc_and_roots_the_new_closure() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let (first, second) = publish_two_eligible(&mut host);
        let frame = heap::with_heap(&mut host, |heap| heap.enter_roots(4)).expect("roots");

        let first_taken = heap::take_published_pending_finalizer_action(&mut host, frame, &[0])
            .expect("first published head");
        assert_eq!(first_taken.userdata, first.userdata);
        assert_eq!(
            first_taken.action,
            PendingFinalizerAction::NonCallable(PendingFinalizerNotCallable::Boolean),
            "the selection-time false is still the current raw value"
        );
        assert_eq!(first_taken.roots_written, 1);
        assert_eq!(rooted_userdata(&mut host, frame, 0), Some(first.userdata));
        assert_eq!(pending_records(&mut host).len(), 1);
        assert_eq!(published_count(&mut host), 1);

        let replacement =
            heap::with_heap(&mut host, |heap| heap.alloc_closure()).expect("replacement closure");
        heap::with_heap(&mut host, |heap| {
            heap.set_named_closure(second.metatable, "__gc", replacement)
        })
        .expect("sibling raw __gc replacement");

        let second_taken = heap::take_published_pending_finalizer_action(&mut host, frame, &[1, 2])
            .expect("second published head");
        assert_eq!(second_taken.userdata, second.userdata);
        assert_eq!(
            second_taken.action,
            PendingFinalizerAction::Closure(replacement),
            "the second dequeue reads the replacement closure, not the old false"
        );
        assert_eq!(second_taken.roots_written, 2);
        assert_eq!(rooted_userdata(&mut host, frame, 1), Some(second.userdata));
        assert_eq!(rooted_closure(&mut host, frame, 2), Some(replacement));
        assert!(pending_records(&mut host).is_empty());
        assert_eq!(published_count(&mut host), 0);
        assert!(!pending_flag(&mut host, second.userdata));
        assert!(finalizer_settled(&mut host, second.userdata));

        // Drop the original metatable field edge: the replacement closure is
        // now reachable only through the new root.
        heap::with_heap(&mut host, |heap| {
            heap.set_named_scalar(second.metatable, "__gc", Value::Null)
        })
        .expect("remove source __gc edge");

        let (orphan, orphan_weak) = weak_orphan(&mut host);
        heap::with_heap(&mut host, |heap| heap.write_root(frame, 3, orphan_weak))
            .expect("weak orphan root");
        let _ = heap::collect(&mut host).expect("ordinary collect");

        assert!(userdata_alive(&mut host, first.userdata));
        assert!(userdata_alive(&mut host, second.userdata));
        assert!(
            closure_alive(&mut host, replacement),
            "the new root alone keeps the replacement closure"
        );
        assert!(
            !alive(&mut host, orphan),
            "the independent orphan is reclaimed"
        );
        assert_eq!(
            slot_generation(&mut host, orphan.slot()),
            orphan.generation() + 1,
            "the collected orphan slot advanced"
        );

        heap::with_heap(&mut host, |heap| heap.clear_root(frame, 2)).expect("clear closure root");
        let _ = heap::collect(&mut host).expect("collect after clearing the closure root");
        assert!(
            !closure_alive(&mut host, replacement),
            "without the root the replacement closure is collected"
        );
        assert!(userdata_alive(&mut host, second.userdata));

        heap::with_heap(&mut host, |heap| heap.clear_root(frame, 1)).expect("clear userdata root");
        let _ = heap::collect(&mut host).expect("collect after clearing the userdata root");
        assert!(!userdata_alive(&mut host, second.userdata));
        assert!(!userdata_token_open(&host, second.raw));
        assert_eq!(
            slot_generation(&mut host, second.userdata.slot()),
            second.userdata.generation() + 1,
            "the real release advances the consumed slot generation"
        );
        heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave roots");
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GcSkipArm {
    MissingField,
    StoredNilGap,
    NoMetatable,
}

/// Selection-time `__gc` is non-nil; the mutation between the two dequeues
/// removes it. All three shapes are `Skip` and retire exactly once. Restoring a
/// real `__gc` on the settled generation and dropping the temporary userdata
/// root does not enqueue it again; the later selecting collection reclaims it.
#[test]
fn published_action_skip_arms_delete_gap_and_no_metatable_retire_once() {
    for arm in [
        GcSkipArm::MissingField,
        GcSkipArm::StoredNilGap,
        GcSkipArm::NoMetatable,
    ] {
        crate::support::with_idle_vm(|vm| {
            let mut host = vm.host_context();
            heap::install(&mut host, limits()).expect("install");
            let (first, second) = publish_two_eligible(&mut host);
            let frame = heap::with_heap(&mut host, |heap| heap.enter_roots(2)).expect("roots");

            let taken = heap::take_published_pending_finalizer_action(&mut host, frame, &[0])
                .expect("first published head");
            assert_eq!(taken.userdata, first.userdata);
            assert_eq!(
                taken.action,
                PendingFinalizerAction::NonCallable(PendingFinalizerNotCallable::Boolean)
            );

            heap::with_heap(&mut host, |heap| match arm {
                GcSkipArm::MissingField => {
                    heap.set_named_scalar(second.metatable, "__gc", Value::Null)
                }
                GcSkipArm::StoredNilGap => {
                    heap.set_named_gap_test_control(second.metatable, "__gc")
                }
                GcSkipArm::NoMetatable => heap.set_userdata_metatable(second.userdata, None),
            })
            .expect("mutation between the two dequeues");

            let taken = heap::take_published_pending_finalizer_action(&mut host, frame, &[1])
                .expect("second published head");
            assert_eq!(taken.userdata, second.userdata);
            assert_eq!(taken.action, PendingFinalizerAction::Skip, "arm {arm:?}");
            assert_eq!(taken.roots_written, 1, "arm {arm:?}");
            assert_eq!(rooted_userdata(&mut host, frame, 1), Some(second.userdata));
            assert!(pending_records(&mut host).is_empty(), "arm {arm:?}");
            assert_eq!(published_count(&mut host), 0, "arm {arm:?}");
            assert!(!pending_flag(&mut host, second.userdata));
            assert!(finalizer_settled(&mut host, second.userdata));

            // Restore a real finalizer field on the settled generation, then
            // drop the temporary userdata root. A later private selection must
            // not enqueue the same generation again.
            heap::with_heap(&mut host, |heap| {
                match arm {
                    GcSkipArm::NoMetatable => {
                        let metatable = heap.alloc()?;
                        heap.set_named_scalar(metatable, "__gc", Value::Bool(false))?;
                        heap.set_userdata_metatable(second.userdata, Some(metatable))?;
                    }
                    _ => {
                        heap.set_named_scalar(second.metatable, "__gc", Value::Bool(false))?;
                    }
                }
                Ok(())
            })
            .expect("restore raw __gc");
            heap::with_heap(&mut host, |heap| heap.clear_root(frame, 1))
                .expect("clear the second userdata root");

            let boundary = publish_boundary(&mut host);
            let records = pending_records(&mut host);
            assert!(
                !records
                    .iter()
                    .any(|record| record.1 == second.userdata.slot()),
                "arm {arm:?}: the settled generation is not queued again"
            );
            finish_if_published(&mut host, boundary);
            assert!(
                !userdata_alive(&mut host, second.userdata),
                "arm {arm:?}: the unreachable settled userdata is swept"
            );
            assert!(!userdata_token_open(&host, second.raw), "arm {arm:?}");
            assert_eq!(
                slot_generation(&mut host, second.userdata.slot()),
                second.userdata.generation() + 1,
                "arm {arm:?}"
            );
            assert!(userdata_alive(&mut host, first.userdata));
            heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave roots");
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GcRejectArm {
    False,
    True,
    String,
    Userdata,
}

/// A present but non-callable raw `__gc` is a typed reject, not `Skip`. Every
/// arm retires exactly once with only the userdata root; restoring a real
/// closure afterwards does not enqueue the settled generation again.
#[test]
fn published_action_non_callable_values_are_typed_rejects_and_retire_once() {
    // The real named writer stores a boolean or Lua string under `__gc`; a
    // number cannot occupy an arbitrary named field (Float is reserved for
    // field 0 `__mode`) and no named writer stores Bytes. The number and byte
    // string tags are therefore pinned at the narrow classification boundary
    // instead of widening the guest writer.
    assert_eq!(
        heap::pending_finalizer_scalar_reject(&Value::Int(3)),
        Some(PendingFinalizerNotCallable::Number)
    );
    assert_eq!(
        heap::pending_finalizer_scalar_reject(&Value::Float(3.25)),
        Some(PendingFinalizerNotCallable::Number)
    );
    assert_eq!(
        heap::pending_finalizer_scalar_reject(&Value::bytes(vec![0xff_u8])),
        Some(PendingFinalizerNotCallable::String)
    );
    assert_eq!(heap::pending_finalizer_scalar_reject(&Value::Null), None);
    for arm in [
        GcRejectArm::False,
        GcRejectArm::True,
        GcRejectArm::String,
        GcRejectArm::Userdata,
    ] {
        crate::support::with_idle_vm(|vm| {
            let mut host = vm.host_context();
            heap::install(&mut host, limits()).expect("install");
            let (first, second) = publish_two_eligible(&mut host);
            let frame = heap::with_heap(&mut host, |heap| heap.enter_roots(3)).expect("roots");

            let taken = heap::take_published_pending_finalizer_action(&mut host, frame, &[0])
                .expect("first published head");
            assert_eq!(taken.userdata, first.userdata);

            let other_userdata = match arm {
                GcRejectArm::Userdata => {
                    Some(heap::publish_userdata(&mut host).expect("non-callable userdata value"))
                }
                _ => None,
            };
            heap::with_heap(&mut host, |heap| match arm {
                GcRejectArm::False => {
                    heap.set_named_scalar(second.metatable, "__gc", Value::Bool(false))
                }
                GcRejectArm::True => {
                    heap.set_named_scalar(second.metatable, "__gc", Value::Bool(true))
                }
                GcRejectArm::String => {
                    heap.set_named_scalar(second.metatable, "__gc", Value::string("not callable"))
                }
                GcRejectArm::Userdata => heap.set_named_userdata(
                    second.metatable,
                    "__gc",
                    other_userdata.expect("userdata arm value"),
                ),
            })
            .expect("mutation between the two dequeues");

            let taken = heap::take_published_pending_finalizer_action(&mut host, frame, &[1])
                .expect("second published head");
            assert_eq!(taken.userdata, second.userdata);
            let expected = match arm {
                GcRejectArm::Userdata => PendingFinalizerAction::NonCallable(
                    PendingFinalizerNotCallable::UserdataWithoutCall(
                        other_userdata.expect("userdata arm value"),
                    ),
                ),
                GcRejectArm::False | GcRejectArm::True => {
                    PendingFinalizerAction::NonCallable(PendingFinalizerNotCallable::Boolean)
                }
                GcRejectArm::String => {
                    PendingFinalizerAction::NonCallable(PendingFinalizerNotCallable::String)
                }
            };
            assert_eq!(taken.action, expected, "arm {arm:?}");
            assert_eq!(taken.roots_written, 1, "arm {arm:?}");
            assert_eq!(rooted_userdata(&mut host, frame, 1), Some(second.userdata));
            assert_eq!(
                rooted_closure(&mut host, frame, 2),
                None,
                "arm {arm:?}: a typed reject roots no closure"
            );
            assert!(pending_records(&mut host).is_empty());
            assert_eq!(published_count(&mut host), 0);
            assert!(!pending_flag(&mut host, second.userdata));
            assert!(finalizer_settled(&mut host, second.userdata));

            // Restore a real finalizer field, drop the temporary root, and
            // confirm the settled generation is not selected again.
            let restored =
                heap::with_heap(&mut host, |heap| heap.alloc_closure()).expect("restored closure");
            heap::with_heap(&mut host, |heap| {
                heap.set_named_closure(second.metatable, "__gc", restored)
            })
            .expect("restore raw __gc");
            heap::with_heap(&mut host, |heap| heap.clear_root(frame, 1))
                .expect("clear the second userdata root");
            let boundary = publish_boundary(&mut host);
            assert!(
                !pending_records(&mut host)
                    .iter()
                    .any(|record| record.1 == second.userdata.slot()),
                "arm {arm:?}: the settled generation is not queued again"
            );
            finish_if_published(&mut host, boundary);
            assert!(!userdata_alive(&mut host, second.userdata), "arm {arm:?}");
            assert!(!userdata_token_open(&host, second.raw), "arm {arm:?}");
            assert!(!closure_alive(&mut host, restored), "arm {arm:?}");
            heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave roots");
        });
    }
}

/// The raw `__gc` table is called through its own current metatable's raw
/// `__call` closure. The original receiver is kept (not replaced by the
/// callable) and all three identities are rooted. Both original source edges
/// (`__gc` and `__call`) are removed before a real collection, so only the
/// roots keep the receiver, the real closure, and its captured table alive.
/// Clearing the closure root first kills the closure and its capture but not
/// the receiver; clearing the receiver root first kills the receiver but not
/// the closure. The two roots cannot substitute for each other, and only then
/// is the pending userdata root removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RootDropOrder {
    ClosureFirst,
    ReceiverFirst,
}

#[test]
fn published_action_table_form_roots_the_exact_receiver_and_call_closure() {
    for order in [RootDropOrder::ClosureFirst, RootDropOrder::ReceiverFirst] {
        crate::support::with_idle_vm(|vm| {
            let mut host = vm.host_context();
            heap::install(&mut host, limits()).expect("install");
            let head = publish_receiver_eligible(&mut host, ReceiverForm::Table);
            let receiver = head.receiver.table();
            let frame = heap::with_heap(&mut host, |heap| heap.enter_roots(4)).expect("roots");
            let boundary = publish_boundary(&mut host);
            assert!(matches!(boundary, PublishedBoundary::Published { .. }));
            assert_eq!(published_count(&mut host), 1);
            finish_release(&mut host);

            let taken = heap::take_published_pending_finalizer_action(&mut host, frame, &[0, 1, 2])
                .expect("table-form published head");
            assert_eq!(taken.userdata, head.userdata);
            assert_eq!(
                taken.action,
                PendingFinalizerAction::TableCall {
                    receiver,
                    call: head.call,
                }
            );
            assert_eq!(taken.roots_written, 3);
            assert_eq!(rooted_userdata(&mut host, frame, 0), Some(head.userdata));
            assert_eq!(rooted_closure(&mut host, frame, 1), Some(head.call));
            assert_eq!(rooted_table(&mut host, frame, 2), Some(receiver));
            assert!(pending_records(&mut host).is_empty());
            assert_eq!(published_count(&mut host), 0);
            assert!(finalizer_settled(&mut host, head.userdata));

            // Remove both original strong edges. From here the receiver is
            // reachable only through root 2, and the real call closure (with
            // its capture) only through root 1.
            heap::with_heap(&mut host, |heap| detach_receiver_source_edges(heap, &head))
                .expect("remove source __gc and __call edges");
            let (orphan, orphan_weak) = weak_orphan(&mut host);
            heap::with_heap(&mut host, |heap| heap.write_root(frame, 3, orphan_weak))
                .expect("weak orphan root");
            let _ = heap::collect(&mut host).expect("ordinary collect");

            assert!(userdata_alive(&mut host, head.userdata));
            assert!(
                closure_alive(&mut host, head.call),
                "the closure root keeps the real call closure"
            );
            assert!(
                alive(&mut host, head.capture),
                "the closure root keeps its necessary capture"
            );
            assert!(
                alive(&mut host, receiver),
                "the receiver root keeps the table"
            );
            assert!(
                !alive(&mut host, orphan),
                "the independent orphan is reclaimed"
            );
            assert_eq!(
                slot_generation(&mut host, orphan.slot()),
                orphan.generation() + 1
            );

            match order {
                RootDropOrder::ClosureFirst => {
                    heap::with_heap(&mut host, |heap| heap.clear_root(frame, 1))
                        .expect("clear closure root");
                    let _ = heap::collect(&mut host).expect("collect without the closure root");
                    assert!(
                        !closure_alive(&mut host, head.call),
                        "the receiver root cannot keep the call closure"
                    );
                    assert!(!alive(&mut host, head.capture));
                    assert!(alive(&mut host, receiver));
                    assert!(userdata_alive(&mut host, head.userdata));

                    heap::with_heap(&mut host, |heap| heap.clear_root(frame, 2))
                        .expect("clear receiver root");
                    let _ = heap::collect(&mut host).expect("collect without the receiver root");
                    assert!(!alive(&mut host, receiver));
                    assert!(
                        !alive(&mut host, head.receiver_metatable),
                        "the receiver's own metatable dies with it"
                    );
                    assert!(userdata_alive(&mut host, head.userdata));
                }
                RootDropOrder::ReceiverFirst => {
                    heap::with_heap(&mut host, |heap| heap.clear_root(frame, 2))
                        .expect("clear receiver root");
                    let _ = heap::collect(&mut host).expect("collect without the receiver root");
                    assert!(!alive(&mut host, receiver));
                    assert!(!alive(&mut host, head.receiver_metatable));
                    assert!(
                        closure_alive(&mut host, head.call),
                        "the closure root cannot be replaced by the receiver root"
                    );
                    assert!(alive(&mut host, head.capture));
                    assert!(userdata_alive(&mut host, head.userdata));

                    heap::with_heap(&mut host, |heap| heap.clear_root(frame, 1))
                        .expect("clear closure root");
                    let _ = heap::collect(&mut host).expect("collect without the closure root");
                    assert!(!closure_alive(&mut host, head.call));
                    assert!(!alive(&mut host, head.capture));
                    assert!(userdata_alive(&mut host, head.userdata));
                }
            }

            heap::with_heap(&mut host, |heap| heap.clear_root(frame, 0))
                .expect("clear userdata root");
            let _ = heap::collect(&mut host).expect("collect after clearing the userdata root");
            assert!(!userdata_alive(&mut host, head.userdata));
            assert!(!userdata_token_open(&host, head.raw));
            assert_eq!(
                slot_generation(&mut host, head.userdata.slot()),
                head.userdata.generation() + 1
            );
            heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave roots");
        });
    }
}

/// The raw `__gc` userdata is called through its own current metatable's raw
/// `__call` closure. The independent receiver is a second real typed userdata,
/// not the pending object. All three identities are rooted; both original
/// source edges are removed before a real collection. Clearing either the
/// closure root or the receiver root only releases its own identity, so the
/// two roots cannot substitute for each other. The receiver's real typed token
/// closes and its generation advances only after its root is removed; the
/// pending userdata token closes only after its own root is removed.
#[test]
fn published_action_userdata_form_roots_the_independent_receiver_and_call_closure() {
    for order in [RootDropOrder::ClosureFirst, RootDropOrder::ReceiverFirst] {
        crate::support::with_idle_vm(|vm| {
            let mut host = vm.host_context();
            heap::install(&mut host, limits()).expect("install");
            let head = publish_receiver_eligible(&mut host, ReceiverForm::Userdata);
            let receiver = head.receiver.userdata();
            let receiver_raw = head.receiver_raw.expect("published receiver token");
            let frame = heap::with_heap(&mut host, |heap| heap.enter_roots(4)).expect("roots");
            let boundary = publish_boundary(&mut host);
            assert!(matches!(boundary, PublishedBoundary::Published { .. }));
            assert_eq!(published_count(&mut host), 1);
            finish_release(&mut host);

            let _ = heap::take_scan_meter(&mut host);
            let taken = heap::take_published_pending_finalizer_action(&mut host, frame, &[0, 1, 2])
                .expect("userdata-form published head");
            let meter = heap::take_scan_meter(&mut host).expect("userdata action meter");
            assert_eq!(
                meter.pending_release_queue_reads, 2,
                "the userdata form still reads only the published head, twice"
            );
            assert_eq!(
                meter.pending_queue_storage_moves, 1,
                "the userdata form still performs one VecDeque front pop"
            );
            assert_eq!(taken.userdata, head.userdata);
            assert_eq!(
                taken.action,
                PendingFinalizerAction::UserdataCall {
                    receiver,
                    call: head.call,
                }
            );
            assert_eq!(taken.roots_written, 3);
            assert_eq!(rooted_userdata(&mut host, frame, 0), Some(head.userdata));
            assert_eq!(rooted_closure(&mut host, frame, 1), Some(head.call));
            assert_eq!(rooted_userdata(&mut host, frame, 2), Some(receiver));
            assert!(pending_records(&mut host).is_empty());
            assert_eq!(published_count(&mut host), 0);
            assert!(finalizer_settled(&mut host, head.userdata));

            heap::with_heap(&mut host, |heap| detach_receiver_source_edges(heap, &head))
                .expect("remove source __gc and __call edges");
            let (orphan, orphan_weak) = weak_orphan(&mut host);
            heap::with_heap(&mut host, |heap| heap.write_root(frame, 3, orphan_weak))
                .expect("weak orphan root");
            let _ = heap::collect(&mut host).expect("ordinary collect");

            assert!(userdata_alive(&mut host, head.userdata));
            assert!(userdata_alive(&mut host, receiver));
            assert!(closure_alive(&mut host, head.call));
            assert!(alive(&mut host, head.capture));
            assert!(!alive(&mut host, orphan));
            assert_eq!(
                slot_generation(&mut host, orphan.slot()),
                orphan.generation() + 1
            );

            match order {
                RootDropOrder::ClosureFirst => {
                    heap::with_heap(&mut host, |heap| heap.clear_root(frame, 1))
                        .expect("clear closure root");
                    let _ = heap::collect(&mut host).expect("collect without the closure root");
                    assert!(!closure_alive(&mut host, head.call));
                    assert!(!alive(&mut host, head.capture));
                    assert!(userdata_alive(&mut host, receiver));
                    assert!(userdata_alive(&mut host, head.userdata));

                    heap::with_heap(&mut host, |heap| heap.clear_root(frame, 2))
                        .expect("clear receiver root");
                    let _ = heap::collect(&mut host).expect("collect without the receiver root");
                    assert!(!userdata_alive(&mut host, receiver));
                    assert!(!userdata_token_open(&host, receiver_raw));
                    assert_eq!(
                        slot_generation(&mut host, receiver.slot()),
                        receiver.generation() + 1,
                        "the independent receiver token closes and its generation advances"
                    );
                    assert!(!alive(&mut host, head.receiver_metatable));
                    assert!(userdata_alive(&mut host, head.userdata));
                }
                RootDropOrder::ReceiverFirst => {
                    heap::with_heap(&mut host, |heap| heap.clear_root(frame, 2))
                        .expect("clear receiver root");
                    let _ = heap::collect(&mut host).expect("collect without the receiver root");
                    assert!(!userdata_alive(&mut host, receiver));
                    assert!(!userdata_token_open(&host, receiver_raw));
                    assert_eq!(
                        slot_generation(&mut host, receiver.slot()),
                        receiver.generation() + 1
                    );
                    assert!(!alive(&mut host, head.receiver_metatable));
                    assert!(closure_alive(&mut host, head.call));
                    assert!(alive(&mut host, head.capture));
                    assert!(userdata_alive(&mut host, head.userdata));

                    heap::with_heap(&mut host, |heap| heap.clear_root(frame, 1))
                        .expect("clear closure root");
                    let _ = heap::collect(&mut host).expect("collect without the closure root");
                    assert!(!closure_alive(&mut host, head.call));
                    assert!(!alive(&mut host, head.capture));
                    assert!(userdata_alive(&mut host, head.userdata));
                }
            }

            heap::with_heap(&mut host, |heap| heap.clear_root(frame, 0))
                .expect("clear userdata root");
            let _ = heap::collect(&mut host).expect("collect after clearing the userdata root");
            assert!(!userdata_alive(&mut host, head.userdata));
            assert!(!userdata_token_open(&host, head.raw));
            assert_eq!(
                slot_generation(&mut host, head.userdata.slot()),
                head.userdata.generation() + 1
            );
            heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave roots");
        });
    }
}

/// The independent userdata receiver uses the same typed `lua.userdata` owner
/// check as the pending head. The queue raw and the head slot raw stay equal;
/// only the receiver token's Core payload is temporarily rewritten to another
/// real userdata. The dequeue is refused before the pop, without a root, queue,
/// token close, settlement, or generation change. Restoring that payload lets
/// the same context dequeue successfully.
#[test]
fn published_action_userdata_receiver_payload_mismatch_refuses_before_any_write() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let head = publish_receiver_eligible(&mut host, ReceiverForm::Userdata);
        let receiver = head.receiver.userdata();
        let receiver_raw = head.receiver_raw.expect("published receiver token");
        let frame = heap::with_heap(&mut host, |heap| heap.enter_roots(4)).expect("roots");
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        assert_eq!(published_count(&mut host), 1);
        finish_release(&mut host);

        let records_before = pending_records(&mut host);
        assert_eq!(records_before[0].0, head.raw);
        assert_eq!(
            records_before[0].0,
            bound_raw(&mut host, head.userdata),
            "the queue raw and the head slot raw are equal"
        );
        assert_eq!(
            receiver_raw,
            bound_raw(&mut host, receiver),
            "the receiver slot raw is the real typed token"
        );
        let receiver_generation = slot_generation(&mut host, receiver.slot());

        let decoy = heap::publish_userdata(&mut host).expect("decoy userdata");
        overwrite_userdata_payload(&mut host, receiver_raw, decoy);
        let refused = heap::take_published_pending_finalizer_action(&mut host, frame, &[0, 1, 2])
            .expect_err("receiver payload belongs to another userdata");
        assert_eq!(refused, HeapError::TokenMismatch);
        assert_eq!(pending_records(&mut host), records_before);
        assert_eq!(published_count(&mut host), 1);
        assert_eq!(rooted_userdata(&mut host, frame, 0), None);
        assert_eq!(rooted_closure(&mut host, frame, 1), None);
        assert_eq!(rooted_userdata(&mut host, frame, 2), None);
        assert!(pending_flag(&mut host, head.userdata));
        assert!(!finalizer_settled(&mut host, head.userdata));
        assert!(userdata_token_open(&host, head.raw));
        assert!(
            userdata_token_open(&host, receiver_raw),
            "the receiver payload refusal closes no token"
        );
        assert_eq!(
            slot_generation(&mut host, receiver.slot()),
            receiver_generation
        );

        overwrite_userdata_payload(&mut host, receiver_raw, receiver);
        let taken = heap::take_published_pending_finalizer_action(&mut host, frame, &[0, 1, 2])
            .expect("repaired receiver payload");
        assert_eq!(
            taken.action,
            PendingFinalizerAction::UserdataCall {
                receiver,
                call: head.call,
            }
        );
        assert_eq!(taken.roots_written, 3);
        assert_eq!(rooted_userdata(&mut host, frame, 0), Some(head.userdata));
        assert_eq!(rooted_closure(&mut host, frame, 1), Some(head.call));
        assert_eq!(rooted_userdata(&mut host, frame, 2), Some(receiver));
        assert!(pending_records(&mut host).is_empty());
        assert_eq!(published_count(&mut host), 0);
        assert!(finalizer_settled(&mut host, head.userdata));
        heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave roots");
    });
}

struct SelfReceiverEligible {
    userdata: UserdataRef,
    raw: u64,
    metatable: TableRef,
    call: ClosureRef,
    capture: TableRef,
}

/// One real typed userdata whose metatable stores `__gc` naming that same
/// userdata and a raw `__call` closure. The self receiver is already rooted as
/// `slots[0]` when the action commits, so it must not require a third slot.
fn publish_self_receiver_eligible(host: &mut vm::HostContext<'_>) -> SelfReceiverEligible {
    let userdata = heap::publish_userdata(host).expect("self-receiver userdata");
    let raw = bound_raw(host, userdata);
    let (metatable, call, capture) = heap::with_heap(host, |heap| {
        let metatable = heap.alloc()?;
        let call = heap.alloc_closure()?;
        let capture_cell = heap.alloc_upvalue()?;
        let capture = heap.alloc()?;
        heap.set_upvalue_edge(call, capture_cell)?;
        heap.write_upvalue(capture_cell, capture)?;
        heap.set_named_closure(metatable, "__call", call)?;
        heap.set_named_userdata(metatable, "__gc", userdata)?;
        heap.set_userdata_metatable(userdata, Some(metatable))?;
        Ok((metatable, call, capture))
    })
    .expect("self-receiver lattice");
    SelfReceiverEligible {
        userdata,
        raw,
        metatable,
        call,
        capture,
    }
}

/// A callable userdata receiver that is the pending object itself adds only
/// the object and closure roots: the object root already mounts the receiver.
/// The archived LuaJIT differential fixes the guest arguments and identity for
/// this shape (both positions are the same object); it does not prescribe an
/// internal root-slot count. The two-root plan below is this private heap's own
/// mount contract. The third plan position stays empty, and the closure root
/// remains the only thing that keeps the real closure and its capture alive
/// after both source edges are removed.
#[test]
fn published_action_userdata_self_receiver_roots_the_object_and_closure_once() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let head = publish_self_receiver_eligible(&mut host);
        let frame = heap::with_heap(&mut host, |heap| heap.enter_roots(3)).expect("roots");
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        assert_eq!(published_count(&mut host), 1);
        finish_release(&mut host);

        let taken = heap::take_published_pending_finalizer_action(&mut host, frame, &[0, 1, 2])
            .expect("self-receiver published head");
        assert_eq!(taken.userdata, head.userdata);
        assert_eq!(
            taken.action,
            PendingFinalizerAction::UserdataCall {
                receiver: head.userdata,
                call: head.call,
            }
        );
        assert_eq!(
            taken.roots_written, 2,
            "the self receiver needs no third root"
        );
        assert_eq!(rooted_userdata(&mut host, frame, 0), Some(head.userdata));
        assert_eq!(rooted_closure(&mut host, frame, 1), Some(head.call));
        assert_eq!(
            rooted_userdata(&mut host, frame, 2),
            None,
            "the self receiver mounts on the existing object root"
        );
        assert!(pending_records(&mut host).is_empty());
        assert_eq!(published_count(&mut host), 0);
        assert!(finalizer_settled(&mut host, head.userdata));

        heap::with_heap(&mut host, |heap| {
            heap.set_named_scalar(head.metatable, "__gc", Value::Null)?;
            heap.set_named_scalar(head.metatable, "__call", Value::Null)
        })
        .expect("remove source __gc and __call edges");
        let (orphan, orphan_weak) = weak_orphan(&mut host);
        heap::with_heap(&mut host, |heap| heap.write_root(frame, 2, orphan_weak))
            .expect("weak orphan root");
        let _ = heap::collect(&mut host).expect("ordinary collect");

        assert!(userdata_alive(&mut host, head.userdata));
        assert!(closure_alive(&mut host, head.call));
        assert!(alive(&mut host, head.capture));
        assert!(!alive(&mut host, orphan));
        assert_eq!(
            slot_generation(&mut host, orphan.slot()),
            orphan.generation() + 1
        );

        heap::with_heap(&mut host, |heap| heap.clear_root(frame, 1)).expect("clear closure root");
        let _ = heap::collect(&mut host).expect("collect without the closure root");
        assert!(!closure_alive(&mut host, head.call));
        assert!(!alive(&mut host, head.capture));
        assert!(userdata_alive(&mut host, head.userdata));

        heap::with_heap(&mut host, |heap| heap.clear_root(frame, 0)).expect("clear userdata root");
        let _ = heap::collect(&mut host).expect("collect after clearing the userdata root");
        assert!(!userdata_alive(&mut host, head.userdata));
        assert!(!userdata_token_open(&host, head.raw));
        assert_eq!(
            slot_generation(&mut host, head.userdata.slot()),
            head.userdata.generation() + 1
        );
        heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave roots");
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CallRelookupArm {
    Keep,
    ReplaceField,
    ReplaceMetatable,
    DeleteField,
    FalseField,
}

/// The table-form target is re-read through its current metatable at dequeue
/// time. Keeping the raw `__gc` receiver but replacing its raw `__call`, or
/// replacing the receiver's whole metatable, executes the new closure identity.
/// A missing or `false` `__call` is a typed reject that keeps the original
/// receiver as a table; reinstalling `__call` after the reject does not retry.
#[test]
fn published_action_table_call_relookup_observes_sibling_call_changes() {
    for arm in [
        CallRelookupArm::Keep,
        CallRelookupArm::ReplaceField,
        CallRelookupArm::ReplaceMetatable,
        CallRelookupArm::DeleteField,
        CallRelookupArm::FalseField,
    ] {
        crate::support::with_idle_vm(|vm| {
            let mut host = vm.host_context();
            heap::install(&mut host, limits()).expect("install");
            let head = publish_receiver_eligible(&mut host, ReceiverForm::Table);
            let first = publish_eligible(&mut host);
            let receiver = head.receiver.table();
            assert!(first.userdata.slot() > head.userdata.slot());
            let frame = heap::with_heap(&mut host, |heap| heap.enter_roots(4)).expect("roots");
            let boundary = publish_boundary(&mut host);
            assert!(matches!(boundary, PublishedBoundary::Published { .. }));
            assert_eq!(published_count(&mut host), 2);
            finish_release(&mut host);

            let taken = heap::take_published_pending_finalizer_action(&mut host, frame, &[0])
                .expect("first published head");
            assert_eq!(taken.userdata, first.userdata);

            let new_call = match arm {
                CallRelookupArm::Keep
                | CallRelookupArm::DeleteField
                | CallRelookupArm::FalseField => None,
                CallRelookupArm::ReplaceField | CallRelookupArm::ReplaceMetatable => Some(
                    heap::with_heap(&mut host, |heap| heap.alloc_closure())
                        .expect("replacement call closure"),
                ),
            };
            heap::with_heap(&mut host, |heap| match arm {
                CallRelookupArm::Keep => Ok(()),
                CallRelookupArm::ReplaceField => heap.set_named_closure(
                    head.receiver_metatable,
                    "__call",
                    new_call.expect("replacement call closure"),
                ),
                CallRelookupArm::ReplaceMetatable => {
                    let metatable = heap.alloc()?;
                    heap.set_named_closure(
                        metatable,
                        "__call",
                        new_call.expect("replacement call closure"),
                    )?;
                    heap.set_metatable(receiver, Some(metatable))
                }
                CallRelookupArm::DeleteField => {
                    heap.set_named_scalar(head.receiver_metatable, "__call", Value::Null)
                }
                CallRelookupArm::FalseField => {
                    heap.set_named_scalar(head.receiver_metatable, "__call", Value::Bool(false))
                }
            })
            .expect("mutation between the two dequeues");

            let taken = heap::take_published_pending_finalizer_action(&mut host, frame, &[1, 2, 3])
                .expect("table-form published head");
            assert_eq!(taken.userdata, head.userdata);
            match arm {
                CallRelookupArm::Keep => {
                    assert_eq!(
                        taken.action,
                        PendingFinalizerAction::TableCall {
                            receiver,
                            call: head.call,
                        },
                        "keep keeps the original closure"
                    );
                }
                CallRelookupArm::ReplaceField | CallRelookupArm::ReplaceMetatable => {
                    assert_eq!(
                        taken.action,
                        PendingFinalizerAction::TableCall {
                            receiver,
                            call: new_call.expect("new closure identity"),
                        },
                        "arm {arm:?}: only the current raw __call is used"
                    );
                }
                CallRelookupArm::DeleteField => {
                    assert_eq!(
                        taken.action,
                        PendingFinalizerAction::NonCallable(
                            PendingFinalizerNotCallable::TableWithoutCall(receiver)
                        ),
                        "a missing __call keeps the original table receiver"
                    );
                }
                CallRelookupArm::FalseField => {
                    assert_eq!(
                        taken.action,
                        PendingFinalizerAction::NonCallable(
                            PendingFinalizerNotCallable::TableCallNotClosure(receiver)
                        ),
                        "false __call keeps the original table receiver"
                    );
                }
            }
            assert!(pending_records(&mut host).is_empty());
            assert_eq!(published_count(&mut host), 0);
            assert!(finalizer_settled(&mut host, head.userdata));
            match arm {
                CallRelookupArm::Keep
                | CallRelookupArm::ReplaceField
                | CallRelookupArm::ReplaceMetatable => {
                    assert_eq!(taken.roots_written, 3);
                    assert_eq!(rooted_userdata(&mut host, frame, 1), Some(head.userdata));
                    assert_eq!(
                        rooted_closure(&mut host, frame, 2),
                        Some(new_call.unwrap_or(head.call))
                    );
                    assert_eq!(rooted_table(&mut host, frame, 3), Some(receiver));
                }
                CallRelookupArm::DeleteField | CallRelookupArm::FalseField => {
                    assert_eq!(taken.roots_written, 1);
                    assert_eq!(rooted_userdata(&mut host, frame, 1), Some(head.userdata));
                    assert_eq!(rooted_closure(&mut host, frame, 2), None);
                    assert_eq!(rooted_table(&mut host, frame, 3), None);

                    // Reinstall a callable on the same original receiver. The
                    // already settled generation is not queued or executed
                    // again; the final collection reclaims it and the restored
                    // closure.
                    let restored = heap::with_heap(&mut host, |heap| heap.alloc_closure())
                        .expect("restored call closure");
                    heap::with_heap(&mut host, |heap| {
                        heap.set_named_closure(head.receiver_metatable, "__call", restored)
                    })
                    .expect("restore raw __call");
                    heap::with_heap(&mut host, |heap| heap.clear_root(frame, 1))
                        .expect("clear the userdata root");
                    let boundary = publish_boundary(&mut host);
                    assert!(
                        !pending_records(&mut host)
                            .iter()
                            .any(|record| record.1 == head.userdata.slot()),
                        "arm {arm:?}: the settled generation is not queued again"
                    );
                    finish_if_published(&mut host, boundary);
                    assert!(!userdata_alive(&mut host, head.userdata), "arm {arm:?}");
                    assert!(!userdata_token_open(&host, head.raw), "arm {arm:?}");
                    assert!(
                        !closure_alive(&mut host, restored),
                        "arm {arm:?}: the restored call closure is not retained"
                    );
                }
            }
            heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave roots");
        });
    }
}

/// A frame plan is validated completely before the queue, root, or slot write.
/// An occupied slot, a duplicate position, a short plan, an out-of-range
/// position, a stale frame, a wrong typed head token, a saved mark, and a saved
/// Release tail all leave the head and every existing root unchanged. The same
/// `HostContext` then succeeds after the obstacle is repaired.
#[test]
fn published_action_refuses_roots_and_saved_state_before_any_write() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let head = publish_receiver_eligible(&mut host, ReceiverForm::Table);
        let receiver = head.receiver.table();
        let (frame, anchor) = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(5)?;
            let anchor = heap.alloc()?;
            heap.write_root(frame, 0, anchor)?;
            Ok((frame, anchor))
        })
        .expect("roots with one occupied slot");

        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        let records_before = pending_records(&mut host);
        assert_eq!(
            records_before,
            vec![(head.raw, head.userdata.slot(), head.userdata.generation())]
        );
        assert_eq!(published_count(&mut host), 1);

        // A saved Release tail owns the graph and refuses before the queue.
        let refused = heap::take_published_pending_finalizer_action(&mut host, frame, &[1, 2, 3])
            .expect_err("saved release tail");
        assert_eq!(refused, HeapError::SweepPending);
        assert_eq!(pending_records(&mut host), records_before);
        assert_eq!(published_count(&mut host), 1);
        assert_eq!(rooted_userdata(&mut host, frame, 1), None);
        assert_eq!(rooted_closure(&mut host, frame, 2), None);
        assert_eq!(rooted_table(&mut host, frame, 3), None);
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.rooted_table(frame, 0)).expect("anchor root"),
            Some(anchor)
        );
        assert!(alive(&mut host, anchor));
        finish_release(&mut host);

        // An occupied first required slot is refused before any write.
        let refused = heap::take_published_pending_finalizer_action(&mut host, frame, &[0, 1, 2])
            .expect_err("occupied root");
        assert_root_slot_occupied(refused);
        assert_eq!(pending_records(&mut host), records_before);
        assert_eq!(published_count(&mut host), 1);
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.rooted_table(frame, 0)).expect("anchor root"),
            Some(anchor)
        );
        assert_eq!(rooted_userdata(&mut host, frame, 1), None);
        assert_eq!(rooted_closure(&mut host, frame, 2), None);

        // A duplicate position and a short plan are both refused up front.
        let refused = heap::take_published_pending_finalizer_action(&mut host, frame, &[1, 1, 2])
            .expect_err("overlapping roots");
        assert_overlapping_roots(refused);
        let refused = heap::take_published_pending_finalizer_action(&mut host, frame, &[1, 2])
            .expect_err("short root plan");
        assert_needs_more_roots(refused);
        assert_eq!(pending_records(&mut host), records_before);
        assert_eq!(published_count(&mut host), 1);
        assert_eq!(rooted_userdata(&mut host, frame, 1), None);

        // An out-of-range slot and a stale frame are refused before any write.
        let refused = heap::take_published_pending_finalizer_action(&mut host, frame, &[1, 2, 5])
            .expect_err("slot range");
        assert_eq!(refused, HeapError::StaleFrame);
        let stale =
            heap::with_heap(&mut host, |heap| heap.enter_roots(1)).expect("temporary frame");
        heap::with_heap(&mut host, |heap| heap.leave_roots(stale)).expect("leave temporary frame");
        let refused = heap::take_published_pending_finalizer_action(&mut host, stale, &[1, 2, 3])
            .expect_err("stale frame");
        assert_eq!(refused, HeapError::StaleFrame);
        assert_eq!(pending_records(&mut host), records_before);
        assert_eq!(published_count(&mut host), 1);
        assert_eq!(rooted_userdata(&mut host, frame, 1), None);

        // A same-type token for another live userdata is refused by the exact
        // head binding before the queue or a root changes.
        let decoy = heap::publish_userdata(&mut host).expect("decoy userdata");
        let decoy_raw = bound_raw(&mut host, decoy);
        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(head.userdata)?;
            heap.bind_userdata_token(head.userdata, decoy_raw)
        })
        .expect("same-type misbind on the head");
        let refused = heap::take_published_pending_finalizer_action(&mut host, frame, &[1, 2, 3])
            .expect_err("typed token mismatch");
        assert_eq!(refused, HeapError::TokenMismatch);
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap
                .published_userdata_token(head.userdata))
            .expect("the failing binding is unchanged"),
            decoy_raw
        );
        assert_eq!(pending_records(&mut host), records_before);
        assert_eq!(rooted_userdata(&mut host, frame, 1), None);
        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(head.userdata)?;
            heap.bind_userdata_token(head.userdata, head.raw)
        })
        .expect("restore the head binding");

        // Queue raw and slot raw are equal here; only the Core payload names
        // another real userdata. This is refused by the typed payload check,
        // not by the raw-binding check above, and the same context recovers
        // after the payload is restored.
        assert_eq!(
            pending_records(&mut host)[0].0,
            heap::with_heap(&mut host, |heap| heap
                .published_userdata_token(head.userdata))
            .expect("slot raw is still bound"),
            "the queue raw and the slot raw are equal in this arm"
        );
        let generation_before = slot_generation(&mut host, head.userdata.slot());
        overwrite_userdata_payload(&mut host, head.raw, decoy);
        let refused = heap::take_published_pending_finalizer_action(&mut host, frame, &[1, 2, 3])
            .expect_err("typed payload belongs to another userdata");
        assert_eq!(refused, HeapError::TokenMismatch);
        assert_eq!(pending_records(&mut host), records_before);
        assert_eq!(published_count(&mut host), 1);
        assert_eq!(rooted_userdata(&mut host, frame, 1), None);
        assert_eq!(rooted_closure(&mut host, frame, 2), None);
        assert_eq!(rooted_table(&mut host, frame, 3), None);
        assert!(pending_flag(&mut host, head.userdata));
        assert!(!finalizer_settled(&mut host, head.userdata));
        assert!(
            userdata_token_open(&host, head.raw),
            "the payload refusal closes no token"
        );
        assert_eq!(
            slot_generation(&mut host, head.userdata.slot()),
            generation_before,
            "the payload refusal advances no generation"
        );
        overwrite_userdata_payload(&mut host, head.raw, head.userdata);
        assert!(pending_flag(&mut host, head.userdata));

        // A saved mark owns the graph even without a Release tail. Dropping
        // that parked mark with an ordinary collection keeps the pending head
        // (host-retained edge) and lets the same context retry.
        heap::open_saved_mark_for_test(&mut host).expect("saved mark");
        let refused = heap::take_published_pending_finalizer_action(&mut host, frame, &[1, 2, 3])
            .expect_err("saved mark");
        assert_eq!(refused, HeapError::MarkActive);
        assert_eq!(pending_records(&mut host), records_before);
        assert_eq!(published_count(&mut host), 1);
        assert_eq!(rooted_userdata(&mut host, frame, 1), None);
        let _ = heap::collect(&mut host).expect("ordinary collect drops the parked mark");
        assert_eq!(pending_records(&mut host), records_before);
        assert_eq!(published_count(&mut host), 1);

        let _ = heap::take_scan_meter(&mut host);
        let taken = heap::take_published_pending_finalizer_action(&mut host, frame, &[1, 2, 3])
            .expect("repaired published head");
        let meter = heap::take_scan_meter(&mut host).expect("action handoff meter");
        assert_eq!(
            meter.pending_release_queue_reads, 2,
            "the action handoff reads the head twice, independent of queue length"
        );
        assert_eq!(
            meter.pending_queue_storage_moves, 1,
            "the action handoff still performs one VecDeque front pop"
        );
        assert_eq!(taken.userdata, head.userdata);
        assert_eq!(
            taken.action,
            PendingFinalizerAction::TableCall {
                receiver,
                call: head.call,
            }
        );
        assert_eq!(taken.roots_written, 3);
        assert_eq!(rooted_userdata(&mut host, frame, 1), Some(head.userdata));
        assert_eq!(rooted_closure(&mut host, frame, 2), Some(head.call));
        assert_eq!(rooted_table(&mut host, frame, 3), Some(receiver));
        assert!(pending_records(&mut host).is_empty());
        assert_eq!(published_count(&mut host), 0);
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.rooted_table(frame, 0)).expect("anchor root"),
            Some(anchor)
        );

        // No record remains; the same entry refuses without touching the
        // roots that the successful handoff wrote.
        let refused = heap::take_published_pending_finalizer_action(&mut host, frame, &[1, 2, 3])
            .expect_err("drained published prefix");
        assert_not_published(refused);
        assert_eq!(rooted_userdata(&mut host, frame, 1), Some(head.userdata));
        assert_eq!(rooted_closure(&mut host, frame, 2), Some(head.call));
        assert_eq!(rooted_table(&mut host, frame, 3), Some(receiver));
        heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave roots");
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiverRejectArm {
    NoMetatable,
    MissingCall,
    TableCall,
    InheritedCall,
}

/// A table or userdata receiver without a callable direct raw `__call` keeps
/// the exact original receiver type in its typed reject. A table-valued
/// `__call` is not dispatched a second time, and a `__call` stored only on the
/// receiver metatable's metatable is not inherited. No closure or receiver
/// root is written for any reject.
#[test]
fn published_action_receiver_rejects_missing_nested_and_inherited_call() {
    for form in [ReceiverForm::Table, ReceiverForm::Userdata] {
        for arm in [
            ReceiverRejectArm::NoMetatable,
            ReceiverRejectArm::MissingCall,
            ReceiverRejectArm::TableCall,
            ReceiverRejectArm::InheritedCall,
        ] {
            crate::support::with_idle_vm(|vm| {
                let mut host = vm.host_context();
                heap::install(&mut host, limits()).expect("install");
                let head = publish_receiver_eligible(&mut host, form);
                let mut nested_call = None;
                heap::with_heap(&mut host, |heap| match arm {
                    ReceiverRejectArm::NoMetatable => match head.receiver {
                        Receiver::Table(receiver) => heap.set_metatable(receiver, None),
                        Receiver::Userdata(receiver) => heap.set_userdata_metatable(receiver, None),
                    },
                    ReceiverRejectArm::MissingCall => {
                        heap.set_named_scalar(head.receiver_metatable, "__call", Value::Null)
                    }
                    ReceiverRejectArm::TableCall => {
                        let nested_metatable = heap.alloc()?;
                        let call = heap.alloc_closure()?;
                        heap.set_named_closure(nested_metatable, "__call", call)?;
                        let nested = heap.alloc()?;
                        heap.set_metatable(nested, Some(nested_metatable))?;
                        heap.set_named_edge(head.receiver_metatable, "__call", nested)?;
                        nested_call = Some(call);
                        Ok(())
                    }
                    ReceiverRejectArm::InheritedCall => {
                        let outer = heap.alloc()?;
                        let call = heap.alloc_closure()?;
                        heap.set_named_closure(outer, "__call", call)?;
                        heap.set_metatable(head.receiver_metatable, Some(outer))?;
                        heap.set_named_scalar(head.receiver_metatable, "__call", Value::Null)?;
                        nested_call = Some(call);
                        Ok(())
                    }
                })
                .expect("receiver rejection lattice");
                let frame = heap::with_heap(&mut host, |heap| heap.enter_roots(3)).expect("roots");
                let boundary = publish_boundary(&mut host);
                assert!(matches!(boundary, PublishedBoundary::Published { .. }));
                assert_eq!(published_count(&mut host), 1);
                finish_release(&mut host);

                let taken = heap::take_published_pending_finalizer_action(&mut host, frame, &[0])
                    .expect("receiver reject published head");
                assert_eq!(taken.userdata, head.userdata);
                let expected = match arm {
                    ReceiverRejectArm::NoMetatable
                    | ReceiverRejectArm::MissingCall
                    | ReceiverRejectArm::InheritedCall => head.receiver.without_call(),
                    ReceiverRejectArm::TableCall => head.receiver.call_not_closure(),
                };
                assert_eq!(
                    taken.action,
                    PendingFinalizerAction::NonCallable(expected),
                    "form {form:?} arm {arm:?}"
                );
                assert_eq!(taken.roots_written, 1, "form {form:?} arm {arm:?}");
                assert_eq!(rooted_userdata(&mut host, frame, 0), Some(head.userdata));
                assert_eq!(
                    rooted_closure(&mut host, frame, 1),
                    None,
                    "form {form:?} arm {arm:?}"
                );
                assert_eq!(
                    rooted_table(&mut host, frame, 2),
                    None,
                    "form {form:?} arm {arm:?}"
                );
                assert_eq!(
                    rooted_userdata(&mut host, frame, 2),
                    None,
                    "form {form:?} arm {arm:?}"
                );
                assert!(pending_records(&mut host).is_empty());
                assert_eq!(published_count(&mut host), 0);
                assert!(finalizer_settled(&mut host, head.userdata));
                if let Some(call) = nested_call {
                    assert!(
                        closure_alive(&mut host, call),
                        "form {form:?} arm {arm:?}: the unused nested closure is still live here"
                    );
                    assert_eq!(
                        rooted_closure(&mut host, frame, 1),
                        None,
                        "form {form:?} arm {arm:?}: an unused closure is never rooted"
                    );
                }
                heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave roots");
            });
        }
    }
}

// -- Observable live-userdata cohort order --------------------------------
//
// LuaJIT inserts a new userdata at the head of the main thread object chain
// and appends each newly separated cohort to the pending queue tail. The
// private heap mirrors only that ordering: allocation order inside one cohort
// reverses at selection, while an earlier published batch stays in front of a
// later one. The fixture below keeps two low sacrificial slots for the same
// generation-reuse check and changes the `__gc` installation order so the
// expected order cannot be explained by slot numbers or installation time.

fn cohort_limits() -> HeapLimits {
    HeapLimits {
        nodes: 64,
        roots: 32,
        edges: 128,
        max_generation: 8,
    }
}

#[derive(Clone, Copy)]
struct CohortMember {
    userdata: UserdataRef,
    raw: u64,
    slot: u32,
    generation: u32,
}

impl CohortMember {
    fn record(self) -> (u64, u32, u32) {
        (self.raw, self.slot, self.generation)
    }
}

struct CohortReceipt {
    a: CohortMember,
    b: CohortMember,
    c: CohortMember,
    d: CohortMember,
    e: CohortMember,
    sacrifice_first: (u32, u32),
    sacrifice_second: (u32, u32),
    first_records: Vec<(u64, u32, u32)>,
    first_taken: UserdataRef,
    first_left: UserdataRef,
    before_second_records: Vec<(u64, u32, u32)>,
    before_second_values: [Option<UserdataRef>; 3],
    second_records: Vec<(u64, u32, u32)>,
    second_values: [Option<UserdataRef>; 3],
    second_keys: [Value; 3],
    second_published: usize,
    taken_order: Vec<UserdataRef>,
}

/// Builds the shared cohort fixture. The two arms differ only at the second
/// weak-publication boundary: one publishes the new batch while the old B is
/// still queued, the other consumes B before any new batch exists.
fn cohort_receipt(publish_second: bool) -> CohortReceipt {
    let mut receipt = None;
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, cohort_limits()).expect("install");

        // The two lowest slots are unrooted tables. Only the first real
        // Release reclaims them, so C and D can reuse them with new
        // generations while E has to grow the slot vector.
        let (first_sacrifice, second_sacrifice) =
            heap::with_heap(&mut host, |heap| Ok((heap.alloc()?, heap.alloc()?)))
                .expect("sacrifice tables");
        let sacrifice_first = (first_sacrifice.slot(), first_sacrifice.generation());
        let sacrifice_second = (second_sacrifice.slot(), second_sacrifice.generation());

        // Pre-build and root every auxiliary weak table. Five value-weak and
        // five key-weak tables cover B, A, C, D, and E.
        let (value_tables, key_tables) = heap::with_heap(&mut host, |heap| {
            let mut values = Vec::with_capacity(5);
            let mut keys = Vec::with_capacity(5);
            for _ in 0..5 {
                values.push(weak_table(heap, "v")?);
                keys.push(weak_table(heap, "k")?);
            }
            Ok((values, keys))
        })
        .expect("weak auxiliary tables");
        let frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(15)?;
            for (offset, table) in value_tables.iter().enumerate() {
                heap.write_root(frame, offset as u32, *table)?;
            }
            for (offset, table) in key_tables.iter().enumerate() {
                heap.write_root(frame, (5 + offset) as u32, *table)?;
            }
            Ok(frame)
        })
        .expect("rooted weak tables");

        // Old cohort: B is allocated before A, so A is the live head.
        let b = heap::publish_userdata(&mut host).expect("B");
        let b_raw = bound_raw(&mut host, b);
        let a = heap::publish_userdata(&mut host).expect("A");
        let a_raw = bound_raw(&mut host, a);
        heap::with_heap(&mut host, |heap| {
            for (index, userdata, label) in [(0usize, b, "B"), (1usize, a, "A")] {
                let metatable = heap.alloc()?;
                heap.set_named_scalar(metatable, "__gc", Value::Bool(false))?;
                heap.set_userdata_metatable(userdata, Some(metatable))?;
                heap.set_number_userdata(value_tables[index], 1.0, userdata)?;
                heap.set_userdata_key_scalar(key_tables[index], userdata, Value::string(label))?;
            }
            Ok(())
        })
        .expect("old cohort lattice");

        let first_boundary = publish_boundary(&mut host);
        assert!(
            matches!(first_boundary, PublishedBoundary::Published { .. }),
            "the old cohort must reach a real weak publication"
        );
        let first_records = pending_records(&mut host);
        finish_release(&mut host);
        let first_taken = heap::take_published_pending_finalizer(&mut host, frame, 10)
            .expect("the first published head");
        let first_left = if first_taken == a { b } else { a };

        // New cohort: C and D take the two reclaimed low slots, E grows.
        let c = heap::publish_userdata(&mut host).expect("C");
        let c_raw = bound_raw(&mut host, c);
        let d = heap::publish_userdata(&mut host).expect("D");
        let d_raw = bound_raw(&mut host, d);
        let e = heap::publish_userdata(&mut host).expect("E");
        let e_raw = bound_raw(&mut host, e);
        assert!(
            d.slot() < c.slot() && c.slot() < e.slot(),
            "the fixture must reuse two low slots before growing"
        );
        assert_eq!(c.slot(), sacrifice_second.0);
        assert_eq!(d.slot(), sacrifice_first.0);
        assert_eq!(c.generation(), sacrifice_second.1 + 1);
        assert_eq!(d.generation(), sacrifice_first.1 + 1);

        // Install `__gc` in E, C, D order, which differs from allocation.
        heap::with_heap(&mut host, |heap| {
            for (index, userdata, label) in [(4usize, e, "E"), (2usize, c, "C"), (3usize, d, "D")] {
                let metatable = heap.alloc()?;
                heap.set_named_scalar(metatable, "__gc", Value::Bool(false))?;
                heap.set_userdata_metatable(userdata, Some(metatable))?;
                heap.set_number_userdata(value_tables[index], 1.0, userdata)?;
                heap.set_userdata_key_scalar(key_tables[index], userdata, Value::string(label))?;
            }
            Ok(())
        })
        .expect("new cohort lattice");

        let before_second_records = pending_records(&mut host);
        let before_second_values = [
            weak_value(&mut host, value_tables[2], 1.0),
            weak_value(&mut host, value_tables[3], 1.0),
            weak_value(&mut host, value_tables[4], 1.0),
        ];

        let (second_records, second_values, second_keys, second_published, taken_order) =
            if publish_second {
                let second_boundary = publish_boundary(&mut host);
                assert!(
                    matches!(second_boundary, PublishedBoundary::Published { .. }),
                    "the new cohort must reach a real weak publication"
                );
                let second_records = pending_records(&mut host);
                let second_published = published_count(&mut host);
                let second_values = [
                    weak_value(&mut host, value_tables[2], 1.0),
                    weak_value(&mut host, value_tables[3], 1.0),
                    weak_value(&mut host, value_tables[4], 1.0),
                ];
                let second_keys = [
                    weak_key(&mut host, key_tables[2], c),
                    weak_key(&mut host, key_tables[3], d),
                    weak_key(&mut host, key_tables[4], e),
                ];
                finish_release(&mut host);
                let mut taken = Vec::new();
                for slot in 11..15 {
                    taken.push(
                        heap::take_published_pending_finalizer(&mut host, frame, slot)
                            .expect("a published cohort head"),
                    );
                }
                (
                    second_records,
                    second_values,
                    second_keys,
                    second_published,
                    taken,
                )
            } else {
                // No second weak publication yet. The old head is consumed
                // first; the new weak values and their order stay separate.
                let mut taken = Vec::new();
                taken.push(
                    heap::take_published_pending_finalizer(&mut host, frame, 11)
                        .expect("the old published head"),
                );
                let second_boundary = publish_boundary(&mut host);
                assert!(
                    matches!(second_boundary, PublishedBoundary::Published { .. }),
                    "the delayed new cohort must reach a real weak publication"
                );
                let second_records = pending_records(&mut host);
                let second_published = published_count(&mut host);
                let second_values = [
                    weak_value(&mut host, value_tables[2], 1.0),
                    weak_value(&mut host, value_tables[3], 1.0),
                    weak_value(&mut host, value_tables[4], 1.0),
                ];
                let second_keys = [
                    weak_key(&mut host, key_tables[2], c),
                    weak_key(&mut host, key_tables[3], d),
                    weak_key(&mut host, key_tables[4], e),
                ];
                finish_release(&mut host);
                for slot in 12..15 {
                    taken.push(
                        heap::take_published_pending_finalizer(&mut host, frame, slot)
                            .expect("a delayed cohort head"),
                    );
                }
                (
                    second_records,
                    second_values,
                    second_keys,
                    second_published,
                    taken,
                )
            };

        receipt = Some(CohortReceipt {
            a: CohortMember {
                userdata: a,
                raw: a_raw,
                slot: a.slot(),
                generation: a.generation(),
            },
            b: CohortMember {
                userdata: b,
                raw: b_raw,
                slot: b.slot(),
                generation: b.generation(),
            },
            c: CohortMember {
                userdata: c,
                raw: c_raw,
                slot: c.slot(),
                generation: c.generation(),
            },
            d: CohortMember {
                userdata: d,
                raw: d_raw,
                slot: d.slot(),
                generation: d.generation(),
            },
            e: CohortMember {
                userdata: e,
                raw: e_raw,
                slot: e.slot(),
                generation: e.generation(),
            },
            sacrifice_first,
            sacrifice_second,
            first_records,
            first_taken,
            first_left,
            before_second_records,
            before_second_values,
            second_records,
            second_values,
            second_keys,
            second_published,
            taken_order,
        });
    });
    receipt.expect("cohort arm")
}

fn cohort_records(members: &[CohortMember]) -> Vec<(u64, u32, u32)> {
    members.iter().map(|member| member.record()).collect()
}

#[test]
fn live_userdata_cohort_order_reverses_allocation_and_keeps_batch_fifo() {
    let receipt = cohort_receipt(true);

    assert_eq!(
        receipt.second_records,
        cohort_records(&[receipt.b, receipt.e, receipt.d, receipt.c]),
        "the old B stays in front; the new cohort is E, D, C in allocation-reverse order"
    );
    assert_eq!(receipt.c.slot, receipt.sacrifice_second.0);
    assert_eq!(receipt.d.slot, receipt.sacrifice_first.0);
    assert_eq!(
        receipt.c.generation,
        receipt.sacrifice_second.1 + 1,
        "C reuses the higher sacrifice slot at the next generation"
    );
    assert_eq!(
        receipt.d.generation,
        receipt.sacrifice_first.1 + 1,
        "D reuses the lower sacrifice slot at the next generation"
    );
    assert!(
        receipt.d.slot < receipt.c.slot && receipt.c.slot < receipt.e.slot,
        "slot(D) < slot(C) < slot(E)"
    );
    assert_eq!(
        receipt.before_second_records,
        cohort_records(&[receipt.b]),
        "C, D, and E are not queued before the second weak publication"
    );
    assert!(
        receipt.before_second_values.iter().all(Option::is_some),
        "the new cohort's weak values are still readable before publication"
    );
    assert_eq!(receipt.second_published, 4);
    assert_eq!(receipt.second_values, [None, None, None]);
    assert_eq!(
        receipt.second_keys,
        [Value::string("C"), Value::string("D"), Value::string("E")],
        "the second weak publication keeps the original weak keys"
    );
    assert_eq!(
        receipt.taken_order,
        vec![
            receipt.b.userdata,
            receipt.e.userdata,
            receipt.d.userdata,
            receipt.c.userdata
        ],
        "the observable dequeue order is B, E, D, C"
    );
    assert_eq!(
        receipt.first_records,
        cohort_records(&[receipt.a, receipt.b]),
        "the old cohort selects A before B"
    );
    assert_eq!(receipt.first_taken, receipt.a.userdata);
    assert_eq!(receipt.first_left, receipt.b.userdata);

    // The same expected batch is not ascending slots, descending slots, or a
    // whole-queue birth-age sort.
    let ascending = cohort_records(&[receipt.b, receipt.d, receipt.c, receipt.e]);
    let descending = cohort_records(&[receipt.b, receipt.e, receipt.c, receipt.d]);
    let birth_age = cohort_records(&[receipt.e, receipt.d, receipt.c, receipt.b]);
    assert_ne!(receipt.second_records, ascending);
    assert_ne!(receipt.second_records, descending);
    assert_ne!(receipt.second_records, birth_age);
}

#[test]
fn live_userdata_cohort_order_without_second_publication_keeps_values_and_order_separate() {
    let receipt = cohort_receipt(false);

    assert_eq!(
        receipt.before_second_records,
        cohort_records(&[receipt.b]),
        "the old B is the only queued record before the second publication"
    );
    assert!(
        receipt.before_second_values.iter().all(Option::is_some),
        "omitting the second weak publication leaves the new weak values readable"
    );
    assert_eq!(
        receipt.second_records,
        cohort_records(&[receipt.e, receipt.d, receipt.c]),
        "the delayed new cohort publishes after B was already consumed"
    );
    assert_eq!(receipt.second_published, 3);
    assert_eq!(receipt.second_values, [None, None, None]);
    assert_eq!(
        receipt.second_keys,
        [Value::string("C"), Value::string("D"), Value::string("E")]
    );
    assert_eq!(
        receipt.taken_order,
        vec![
            receipt.b.userdata,
            receipt.e.userdata,
            receipt.d.userdata,
            receipt.c.userdata
        ],
        "the separated order still consumes B before the delayed cohort"
    );
}

struct PulseCohortReceipt {
    steps: Vec<PulseStep>,
    meter: ScanMeter,
    trace: Vec<(u32, u32)>,
    records: Vec<(u64, u32, u32)>,
    expected_trace: Vec<(u32, u32)>,
    expected_records: Vec<(u64, u32, u32)>,
    node_slots: u32,
}

fn chain_stamp(userdata: UserdataRef) -> (u32, u32) {
    (userdata.slot(), userdata.generation())
}

/// Three eligible userdata separated by an unrelated number of table slots.
/// The weak tables are the only roots. `pulse_steps` drives the real selecting
/// mark one charged unit at a time.
fn pulse_cohort_arm(padding: usize) -> PulseCohortReceipt {
    let mut out = None;
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, cohort_limits()).expect("install");

        let mut padding_tables = Vec::new();
        heap::with_heap(&mut host, |heap| {
            for _ in 0..padding {
                padding_tables.push(heap.alloc()?);
            }
            Ok(())
        })
        .expect("leading padding");

        let first = heap::publish_userdata(&mut host).expect("first");
        let first_raw = bound_raw(&mut host, first);
        let first_weak = heap::with_heap(&mut host, |heap| {
            let metatable = heap.alloc()?;
            heap.set_named_scalar(metatable, "__gc", Value::Bool(false))?;
            heap.set_userdata_metatable(first, Some(metatable))?;
            let weak = weak_table(heap, "v")?;
            heap.set_number_userdata(weak, 1.0, first)?;
            Ok(weak)
        })
        .expect("first lattice");
        heap::with_heap(&mut host, |heap| {
            for _ in 0..padding {
                padding_tables.push(heap.alloc()?);
            }
            Ok(())
        })
        .expect("middle padding");

        let second = heap::publish_userdata(&mut host).expect("second");
        let second_raw = bound_raw(&mut host, second);
        let second_weak = heap::with_heap(&mut host, |heap| {
            let metatable = heap.alloc()?;
            heap.set_named_scalar(metatable, "__gc", Value::Bool(false))?;
            heap.set_userdata_metatable(second, Some(metatable))?;
            let weak = weak_table(heap, "v")?;
            heap.set_number_userdata(weak, 1.0, second)?;
            Ok(weak)
        })
        .expect("second lattice");
        heap::with_heap(&mut host, |heap| {
            for _ in 0..padding {
                padding_tables.push(heap.alloc()?);
            }
            Ok(())
        })
        .expect("trailing padding");

        let third = heap::publish_userdata(&mut host).expect("third");
        let third_raw = bound_raw(&mut host, third);
        let third_weak = heap::with_heap(&mut host, |heap| {
            let metatable = heap.alloc()?;
            heap.set_named_scalar(metatable, "__gc", Value::Bool(false))?;
            heap.set_userdata_metatable(third, Some(metatable))?;
            let weak = weak_table(heap, "v")?;
            heap.set_number_userdata(weak, 1.0, third)?;
            Ok(weak)
        })
        .expect("third lattice");
        heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(3)?;
            heap.write_root(frame, 0, first_weak)?;
            heap.write_root(frame, 1, second_weak)?;
            heap.write_root(frame, 2, third_weak)?;
            Ok(())
        })
        .expect("weak roots");

        let expected_trace = vec![chain_stamp(third), chain_stamp(second), chain_stamp(first)];
        let expected_records = vec![
            (third_raw, third.slot(), third.generation()),
            (second_raw, second.slot(), second.generation()),
            (first_raw, first.slot(), first.generation()),
        ];
        let (steps, meter, trace, records, node_slots) = heap::with_heap(&mut host, |heap| {
            heap.clear_pending_live_trace();
            let _ = heap.take_scan_meter();
            let (steps, meter) = pulse_steps(heap, 50_000);
            Ok((
                steps,
                meter,
                heap.pending_live_trace(),
                heap.pending_finalizer_records(),
                heap.stats().node_slots,
            ))
        })
        .expect("pulse cohort");
        let _ = padding_tables;
        out = Some(PulseCohortReceipt {
            steps,
            meter,
            trace,
            records,
            expected_trace,
            expected_records,
            node_slots,
        });
    });
    out.expect("pulse cohort arm")
}

#[test]
fn live_userdata_selection_reads_one_chain_node_per_pulse_ignoring_non_userdata() {
    for padding in [0usize, 11usize] {
        let receipt = pulse_cohort_arm(padding);
        let mut per_pulse = Vec::new();
        for (index, step) in receipt.steps.iter().enumerate() {
            assert!(
                step.live_reads.len() <= 1,
                "padding {padding}: pulse {index} advanced the live chain cursor {} nodes; one pulse spends one unit",
                step.live_reads.len()
            );
            per_pulse.extend_from_slice(&step.live_reads);
        }
        assert_eq!(
            per_pulse, receipt.expected_trace,
            "padding {padding}: the per-pulse trace deltas name the exact live userdata identity order"
        );
        assert_eq!(
            receipt.trace, receipt.expected_trace,
            "padding {padding}: the cumulative real trace matches the per-pulse deltas"
        );
        assert_eq!(
            receipt.records, receipt.expected_records,
            "padding {padding}: selection order is reverse allocation"
        );
        assert_eq!(
            receipt.meter.pending_finalizer_scan_slots, 3,
            "padding {padding}: only the three live chain nodes were read; this is not a bound on total slot visits"
        );
        assert!(
            receipt.steps.iter().all(|step| step.scan_slots <= 1),
            "padding {padding}: one pulse never advances more than one chain node"
        );
        assert_eq!(
            receipt
                .steps
                .iter()
                .filter(|step| step.live_reads.len() == 1)
                .count(),
            3,
            "padding {padding}: three pulses carry the three real chain reads"
        );
        assert!(
            receipt.node_slots > 3,
            "padding {padding}: the heap has unrelated non-userdata slots"
        );
    }
}

#[test]
fn live_userdata_chain_unlinks_neighbours_and_reheads_a_successor_generation() {
    let mut heap = heap::LuaHeap::new(HeapLimits {
        nodes: 8,
        roots: 4,
        edges: 8,
        max_generation: 8,
    });
    let a = heap.alloc_userdata().expect("A");
    let b = heap.alloc_userdata().expect("B");
    let c = heap.alloc_userdata().expect("C");
    assert_eq!(
        heap.live_userdata_chain(),
        vec![chain_stamp(c), chain_stamp(b), chain_stamp(a)]
    );

    let frame = heap.enter_roots(2).expect("frame");
    heap.write_userdata_root(frame, 0, a).expect("root A");
    heap.write_userdata_root(frame, 1, c).expect("root C");
    let _ = heap.collect();
    assert_eq!(
        heap.live_userdata_chain(),
        vec![chain_stamp(c), chain_stamp(a)],
        "releasing the middle node relinks its two neighbours"
    );

    heap.clear_root(frame, 1).expect("clear C");
    let _ = heap.collect();
    assert_eq!(
        heap.live_userdata_chain(),
        vec![chain_stamp(a)],
        "releasing the head moves the head to its successor"
    );

    heap.clear_root(frame, 0).expect("clear A");
    let _ = heap.collect();
    assert!(
        heap.live_userdata_chain().is_empty(),
        "releasing the tail empties the chain"
    );

    let d = heap.alloc_userdata().expect("D");
    assert_eq!(d.slot(), a.slot(), "the last released slot is reused first");
    assert_eq!(d.generation(), a.generation() + 1);
    assert_eq!(
        heap.live_userdata_chain(),
        vec![chain_stamp(d)],
        "a same-slot successor re-enters at the head with its new generation"
    );

    let e = heap.alloc_userdata().expect("E");
    assert_eq!(e.slot(), c.slot());
    assert_eq!(e.generation(), c.generation() + 1);
    assert_eq!(
        heap.live_userdata_chain(),
        vec![chain_stamp(e), chain_stamp(d)],
        "every new generation is the new head"
    );
}

#[test]
fn live_userdata_registration_rollback_unlinks_and_reheads_the_same_slot() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(
            &mut host,
            HeapLimits {
                nodes: 8,
                roots: 4,
                edges: 8,
                max_generation: 8,
            },
        )
        .expect("install");
        let first = heap::publish_userdata(&mut host).expect("first");
        let second = heap::publish_userdata(&mut host).expect("second");
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.live_userdata_chain())).expect("chain"),
            vec![chain_stamp(second), chain_stamp(first)]
        );

        heap::arm_userdata_rollback_refusal(&mut host).expect("arm refusal");
        heap::publish_userdata_after_register_failure(&mut host).expect_err("refused rollback");
        let witness =
            heap::last_userdata_post_register_failure(&host).expect("post-register witness");
        let reserved = witness.userdata;
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.live_userdata_chain())).expect("chain"),
            vec![
                chain_stamp(reserved),
                chain_stamp(second),
                chain_stamp(first)
            ],
            "a valid reservation links at the head before its token is published"
        );

        heap::retry_pending_userdata_rollback(&mut host, reserved).expect("retry rollback");
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.live_userdata_chain())).expect("chain"),
            vec![chain_stamp(second), chain_stamp(first)],
            "the rollback unlinks before the generation advances"
        );

        let successor = heap::publish_userdata(&mut host).expect("successor");
        assert_eq!(successor.slot(), reserved.slot());
        assert_eq!(successor.generation(), reserved.generation() + 1);
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.live_userdata_chain())).expect("chain"),
            vec![
                chain_stamp(successor),
                chain_stamp(second),
                chain_stamp(first)
            ]
        );
    });
}

// ---------------------------------------------------------------------------
// Slot-linked pending queue: storage boundary, checkpoint rollback, and
// generation/raw snapshots
// ---------------------------------------------------------------------------

/// The queue's real storage boundary for one ordinary sequence.
///
/// A real private publication appends one record per selected candidate; the
/// append writes the new slot and, when non-empty, the old tail, plus one
/// header boundary. The owner preflight reads every stored record once. A
/// front pop reads the old head record (its successor identity comes from that
/// same record), writes the old head slot and one header, and transfers one
/// record. No `VecDeque` move is reported because the slot-linked storage never
/// relocates a stored record.
#[test]
fn pending_queue_publication_and_pop_charge_their_own_storage_boundary() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(
            &mut host,
            HeapLimits {
                nodes: 32,
                roots: 8,
                edges: 64,
                max_generation: 8,
            },
        )
        .expect("install");
        let first = publish_eligible(&mut host);
        let second = publish_eligible(&mut host);
        let frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(3)?;
            heap.write_root(frame, 0, first.weak_values)?;
            heap.write_root(frame, 1, second.weak_values)?;
            Ok(frame)
        })
        .expect("roots");

        let _ = heap::take_scan_meter(&mut host);
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        let meter = heap::take_scan_meter(&mut host).expect("publish boundary meter");
        assert_eq!(
            meter.pending_queue_slot_writes, 3,
            "two appends write the new slot and the old tail once each"
        );
        assert_eq!(
            meter.pending_queue_header_writes, 3,
            "two appends and one publication are three header boundaries"
        );
        assert_eq!(
            meter.pending_release_queue_reads, 2,
            "the whole-queue owner preflight reads both records once"
        );
        assert_eq!(
            meter.pending_queue_storage_moves, 0,
            "an append and a publication transfer no stored record"
        );
        assert_eq!(pending_records(&mut host).len(), 2);
        assert_eq!(published_count(&mut host), 2);
        finish_release(&mut host);
        let _ = heap::with_heap(&mut host, |heap| Ok(heap.take_pending_queue_write_trace()))
            .expect("drain the publish write trace");

        let head = pending_records(&mut host)[0];
        let _ = heap::take_scan_meter(&mut host);
        let taken =
            heap::take_published_pending_finalizer(&mut host, frame, 2).expect("published front");
        assert_eq!(
            (taken.slot(), taken.generation()),
            (head.1, head.2),
            "the pop consumes the published FIFO head"
        );
        let meter = heap::take_scan_meter(&mut host).expect("front-pop boundary meter");
        assert_eq!(
            meter.pending_release_queue_reads, 2,
            "the handoff reads the head record once and the pop once"
        );
        assert_eq!(
            meter.pending_queue_slot_writes, 1,
            "the pop writes only the old head slot"
        );
        assert_eq!(
            meter.pending_queue_header_writes, 1,
            "the pop has one header boundary"
        );
        assert_eq!(
            meter.pending_queue_storage_moves, 1,
            "the pop transfers its one record"
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.take_pending_queue_write_trace()))
                .expect("pop write trace"),
            vec![head.1],
            "the pop writes only the old head slot"
        );
        assert_eq!(pending_records(&mut host).len(), 1);
        assert_eq!(published_count(&mut host), 1);
        heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave roots");
    });
}

/// One real Roots offer per pulse, cursor carried by identity.
///
/// Every Root offer pulse must read exactly one queue record and write none;
/// the offer sequence must be the exact FIFO identity order, so the cursor
/// follows the successor stored on the record it just read and never searches
/// from the head. The selection append reads no queue record and writes the
/// new slot plus the old tail, with one header boundary. No pulse reads more
/// than one queue record.
#[test]
fn pending_queue_offer_cursor_reads_one_record_per_pulse_and_appends_locally() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(
            &mut host,
            HeapLimits {
                nodes: 128,
                roots: 64,
                edges: 512,
                max_generation: 8,
            },
        )
        .expect("install");
        let count = 11usize;
        let mut eligible = Vec::new();
        for _ in 0..count {
            eligible.push(publish_eligible(&mut host));
        }
        let frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots((count + 2) as u32)?;
            for (index, item) in eligible.iter().enumerate() {
                heap.write_root(frame, index as u32, item.weak_values)?;
            }
            Ok(frame)
        })
        .expect("roots");
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        finish_release(&mut host);
        let _ = heap::with_heap(&mut host, |heap| Ok(heap.take_pending_queue_write_trace()))
            .expect("drain the earlier append trace");

        let records = pending_records(&mut host);
        let mut expected_order = Vec::new();
        for item in eligible.iter().rev() {
            expected_order.push((item.raw, item.userdata.slot(), item.userdata.generation()));
        }
        assert_eq!(
            records, expected_order,
            "one cohort is stored newest allocation first"
        );
        let expected_offers: Vec<(u32, u32)> = records
            .iter()
            .map(|(_, slot, generation)| (*slot, *generation))
            .collect();
        let old_tail_slot = records.last().expect("the queue has a tail").1;

        let fresh = publish_eligible(&mut host);
        heap::with_heap(&mut host, |heap| {
            heap.write_root(frame, count as u32, fresh.weak_values)
        })
        .expect("fresh weak root");

        let (initial_counts, final_counts, offers_seen, selected_total, append_trace) =
            heap::with_heap(&mut host, |heap| {
                let mut session = heap::SelectingPendingMark::start(heap);
                let initial = session.pending_queue_counts();
                let mut previous = initial;
                let mut previous_meter = session.meter();
                let mut offers_seen = Vec::new();
                let mut selected_total = 0u64;
                let mut append_trace = Vec::new();
                let mut finished = false;
                for _ in 0..20_000 {
                    let before_selected = previous_meter.pending_finalizers_selected;
                    let (done, after_meter, counts, offers) = session.pulse_with_queue_meter(1);
                    let writes = session.take_pending_queue_write_trace();
                    let reads = counts.0 - previous.0;
                    let slot_writes = counts.1 - previous.1;
                    let header_writes = counts.2 - previous.2;
                    let transfers = counts.3 - previous.3;
                    assert!(reads <= 1, "one pulse read {reads} pending-queue records");
                    assert_eq!(
                        slot_writes,
                        writes.len() as u64,
                        "the write counter and the real slot-write trace disagree"
                    );
                    if !offers.is_empty() {
                        assert_eq!(offers.len(), 1, "one Roots pulse offers one record");
                        assert_eq!(
                            (reads, slot_writes, header_writes, transfers),
                            (1, 0, 0, 0),
                            "a Roots offer reads one record and writes nothing"
                        );
                        offers_seen.push(offers[0]);
                    } else if slot_writes > 0 {
                        assert_eq!(reads, 0, "the append reads no queue record");
                        assert_eq!(
                            (slot_writes, header_writes, transfers),
                            (2, 1, 0),
                            "the append writes the new slot and old tail, one header, no transfer"
                        );
                        assert!(append_trace.is_empty(), "one append pulse only");
                        append_trace = writes;
                    } else {
                        assert_eq!((reads, header_writes, transfers), (0, 0, 0));
                        assert!(writes.is_empty(), "only the append writes a slot");
                    }
                    let selected = after_meter.pending_finalizers_selected - before_selected;
                    selected_total += selected;
                    previous = counts;
                    previous_meter = after_meter;
                    if done {
                        finished = true;
                        break;
                    }
                }
                assert!(finished, "the selecting mark finished");
                Ok((
                    initial,
                    session.pending_queue_counts(),
                    offers_seen,
                    selected_total,
                    append_trace,
                ))
            })
            .expect("queue boundary mark");

        assert_eq!(
            final_counts.0 - initial_counts.0,
            count as u64,
            "the whole mark reads exactly one record per Roots offer"
        );
        assert_eq!(
            final_counts.1 - initial_counts.1,
            2,
            "the single append writes two slot records"
        );
        assert_eq!(final_counts.2 - initial_counts.2, 1);
        assert_eq!(final_counts.3 - initial_counts.3, 0);
        assert_eq!(
            offers_seen, expected_offers,
            "the cursor follows the exact stored successor identities"
        );
        assert_eq!(selected_total, 1, "exactly the fresh candidate is appended");
        assert_eq!(
            append_trace,
            vec![fresh.userdata.slot(), old_tail_slot],
            "the append touches the new slot and the old tail only"
        );
        let records = pending_records(&mut host);
        assert_eq!(records.len(), count + 1);
        assert_eq!(
            records.last().copied(),
            Some((
                fresh.raw,
                fresh.userdata.slot(),
                fresh.userdata.generation()
            ))
        );
        assert_eq!(
            heap::pending_finalizer_count(&mut host).expect("queue count"),
            count + 1
        );
        heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave roots");
    });
}

/// A real same-context three-segment queue: one record from the published
/// prefix, one valid but unpublished record left by an earlier real doomed-close
/// failure, and the current pass's new tail. The current pass adds a valid
/// candidate and then fails the real typed-owner check on a same-type misbound
/// candidate. Rollback must restore the round-start checkpoint: the published
/// prefix and the earlier unpublished segment stay queued and keep their
/// FIFO identities; only the current segment is removed. Repairing both real
/// bindings and retrying in the same context publishes every remaining record,
/// and the first handoff still consumes the oldest published record.
#[test]
fn owner_rejection_restores_the_round_start_queue_not_the_published_prefix() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(
            &mut host,
            HeapLimits {
                nodes: 128,
                roots: 64,
                edges: 512,
                max_generation: 8,
            },
        )
        .expect("install");

        // Segment 1: a genuinely published record.
        let prior = publish_eligible(&mut host);
        let frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(8)?;
            heap.write_root(frame, 0, prior.weak_values)?;
            Ok(frame)
        })
        .expect("prior roots");
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        finish_release(&mut host);
        let prior_record = (
            prior.raw,
            prior.userdata.slot(),
            prior.userdata.generation(),
        );
        assert_eq!(pending_records(&mut host), vec![prior_record]);
        assert_eq!(published_count(&mut host), 1);

        // Segment 2: a valid candidate retained by a real doomed-close failure.
        // The doomed slot is bound to the fresh candidate's token, so the close
        // verifier refuses after selection and before weak publication.
        let fresh = publish_eligible(&mut host);
        heap::with_heap(&mut host, |heap| {
            heap.write_root(frame, 1, fresh.weak_values)
        })
        .expect("fresh weak root");
        let doomed = heap::publish_userdata(&mut host).expect("doomed userdata");
        let doomed_raw = bound_raw(&mut host, doomed);
        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(doomed)?;
            heap.bind_userdata_token(doomed, fresh.raw)
        })
        .expect("doomed same-type misbind");
        let error = heap::collect_pending_finalizers(&mut host).expect_err("doomed close");
        assert_eq!(error, HeapError::TokenMismatch);
        let fresh_record = (
            fresh.raw,
            fresh.userdata.slot(),
            fresh.userdata.generation(),
        );
        assert_eq!(
            pending_records(&mut host),
            vec![prior_record, fresh_record],
            "the valid unpublished record survives the close failure"
        );
        assert_eq!(
            published_count(&mut host),
            1,
            "only the prior record is in the published prefix"
        );
        assert!(pending_flag(&mut host, fresh.userdata));
        assert!(userdata_token_open(&host, fresh.raw));
        assert!(userdata_token_open(&host, doomed_raw));
        assert!(!heap::saved_sweep_tail(&mut host).expect("no tail"));

        // Segment 3: the current pass. A is allocated first, so the live chain
        // selects V before A. Both have a raw `__gc`; A's real binding is
        // replaced by B's same-type token, so the owner preflight accepts the
        // prior, the unpublished fresh record, and V, then refuses A.
        let a = publish_eligible(&mut host);
        let v = publish_eligible(&mut host);
        assert!(
            a.userdata.slot() < v.userdata.slot(),
            "V is the newer live-chain head"
        );
        let b = heap::publish_userdata(&mut host).expect("token owner B");
        let b_raw = bound_raw(&mut host, b);
        heap::with_heap(&mut host, |heap| {
            heap.write_userdata_root(frame, 2, b)?;
            heap.write_root(frame, 3, a.weak_values)?;
            heap.write_root(frame, 4, v.weak_values)
        })
        .expect("segment roots");
        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(a.userdata)?;
            heap.bind_userdata_token(a.userdata, b_raw)
        })
        .expect("same-type misbind on the new candidate");
        heap::with_heap(&mut host, |heap| {
            heap.clear_owner_verification_trace();
            Ok(())
        })
        .expect("clear owner-verification trace");

        let _ = heap::take_scan_meter(&mut host);
        let error = heap::collect_pending_finalizers(&mut host).expect_err("foreign owner");
        assert_eq!(error, HeapError::TokenMismatch);
        let meter = heap::take_scan_meter(&mut host).expect("refusal meter");
        assert_eq!(
            meter.pending_finalizer_offers, 2,
            "Roots offers only the two pre-existing records"
        );
        assert_eq!(
            meter.pending_finalizers_selected, 2,
            "V and A were both selected before the failing verification"
        );
        assert_eq!(meter.token_closes, 0, "verification fails before close");
        let (successes, verified) = heap::with_heap(&mut host, |heap| {
            Ok((
                heap.owner_verification_successes(),
                heap.owner_verification_trace(),
            ))
        })
        .expect("owner-verification successes");
        assert_eq!(successes, 3, "the prior, fresh, and V pass before A");
        assert_eq!(
            verified,
            vec![
                (prior.userdata.slot(), prior.userdata.generation()),
                (fresh.userdata.slot(), fresh.userdata.generation()),
                (v.userdata.slot(), v.userdata.generation()),
            ],
            "the earlier unpublished segment is verified before the current failure"
        );

        // The current segment is gone; the published prefix and the earlier
        // unpublished segment remain in FIFO order. This assertion is the
        // direct anti-control against a rollback that uses the published
        // length (1) instead of the round-start checkpoint (2).
        assert_eq!(
            pending_records(&mut host),
            vec![prior_record, fresh_record],
            "only the current segment is removed"
        );
        assert_eq!(published_count(&mut host), 1);
        assert_eq!(
            heap::pending_finalizer_count(&mut host).expect("queue count"),
            2
        );
        assert!(pending_flag(&mut host, prior.userdata));
        assert!(pending_flag(&mut host, fresh.userdata));
        assert!(!pending_flag(&mut host, v.userdata));
        assert!(!pending_flag(&mut host, a.userdata));
        assert!(alive(&mut host, a.metatable));
        assert!(alive(&mut host, v.metatable));
        assert!(userdata_token_open(&host, fresh.raw));
        assert!(userdata_token_open(&host, v.raw));
        assert!(userdata_token_open(&host, a.raw));
        assert!(userdata_token_open(&host, b_raw));
        assert!(!heap::saved_sweep_tail(&mut host).expect("no tail after refusal"));

        // Repair both real bindings and retry in this same context.
        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(a.userdata)?;
            heap.bind_userdata_token(a.userdata, a.raw)?;
            heap.unbind_userdata_token(doomed)?;
            heap.bind_userdata_token(doomed, doomed_raw)
        })
        .expect("restore real bindings");
        let _ = heap::take_scan_meter(&mut host);
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        let sensor = pending_records(&mut host);
        assert_eq!(
            sensor,
            vec![
                prior_record,
                fresh_record,
                (v.raw, v.userdata.slot(), v.userdata.generation()),
                (a.raw, a.userdata.slot(), a.userdata.generation()),
            ],
            "the retry appends V then A after the preserved segments"
        );
        assert_eq!(published_count(&mut host), 4);
        assert_eq!(
            weak_value(&mut host, fresh.weak_values, 1.0),
            None,
            "the successful retry clear the earlier unpublished weak value"
        );
        assert_eq!(weak_value(&mut host, v.weak_values, 1.0), None);
        assert_eq!(weak_value(&mut host, a.weak_values, 1.0), None);
        assert!(pending_flag(&mut host, a.userdata));
        assert!(pending_flag(&mut host, v.userdata));
        finish_release(&mut host);

        // Published FIFO: the oldest published record is the first handoff.
        let handoff =
            heap::with_heap(&mut host, |heap| heap.enter_roots(1)).expect("handoff frame");
        let taken = heap::take_published_pending_finalizer(&mut host, handoff, 0)
            .expect("oldest published head");
        assert_eq!(taken, prior.userdata);
        assert_eq!(
            pending_records(&mut host),
            vec![
                fresh_record,
                (v.raw, v.userdata.slot(), v.userdata.generation()),
                (a.raw, a.userdata.slot(), a.userdata.generation()),
            ]
        );
        assert_eq!(published_count(&mut host), 3);
        for open in [handoff, frame] {
            heap::with_heap(&mut host, |heap| heap.leave_roots(open)).expect("leave roots");
        }
    });
}

/// Exact snapshots, test-only middle removal, and refusals.
///
/// The queue stores the generation and raw handle captured at append time; a
/// later same-generation rebind does not rewrite the record. Ordinary Roots
/// offers still skip that rebound record and continue to its successor. The
/// test retirement control may search linearly for a middle predecessor, but
/// a saved mark or a Release tail refuses it before any queue write. After a
/// real ordinary collection advances a settled generation, the old identity is
/// not in the queue.
#[test]
fn pending_queue_keeps_its_snapshot_and_refuses_retirement_under_saved_state() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(
            &mut host,
            HeapLimits {
                nodes: 64,
                roots: 16,
                edges: 128,
                max_generation: 8,
            },
        )
        .expect("install");
        let a = publish_eligible(&mut host);
        let b = publish_eligible(&mut host);
        let c = publish_eligible(&mut host);
        let frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(4)?;
            heap.write_root(frame, 0, a.weak_values)?;
            heap.write_root(frame, 1, b.weak_values)?;
            heap.write_root(frame, 2, c.weak_values)?;
            Ok(frame)
        })
        .expect("roots");
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        finish_release(&mut host);
        assert_eq!(
            pending_records(&mut host),
            vec![
                (c.raw, c.userdata.slot(), c.userdata.generation()),
                (b.raw, b.userdata.slot(), b.userdata.generation()),
                (a.raw, a.userdata.slot(), a.userdata.generation()),
            ],
            "the cohort is newest allocation first"
        );

        // The middle record (B) is found by a real predecessor search.
        let _ = heap::take_scan_meter(&mut host);
        let _ = heap::with_heap(&mut host, |heap| Ok(heap.take_pending_queue_write_trace()))
            .expect("drain the earlier append trace");
        heap::retire_pending_finalizer_test_control(&mut host, b.userdata)
            .expect("retire middle record");
        let meter = heap::take_scan_meter(&mut host).expect("middle retirement meter");
        assert_eq!(
            meter.pending_release_queue_reads, 2,
            "the predecessor search reads C, then B"
        );
        assert_eq!(
            meter.pending_queue_slot_writes, 2,
            "the predecessor link and the removed slot are two slot writes"
        );
        assert_eq!(meter.pending_queue_header_writes, 1);
        assert_eq!(meter.pending_queue_storage_moves, 1);
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.take_pending_queue_write_trace()))
                .expect("middle retirement write trace"),
            vec![c.userdata.slot(), b.userdata.slot()],
            "only the predecessor and the removed record are written"
        );
        assert_eq!(
            pending_records(&mut host),
            vec![
                (c.raw, c.userdata.slot(), c.userdata.generation()),
                (a.raw, a.userdata.slot(), a.userdata.generation()),
            ]
        );
        assert_eq!(published_count(&mut host), 2);
        assert!(!pending_flag(&mut host, b.userdata));
        assert!(finalizer_settled(&mut host, b.userdata));

        // A same-generation raw rebind does not rewrite the stored snapshot.
        let decoy = heap::publish_userdata(&mut host).expect("decoy userdata");
        let decoy_raw = bound_raw(&mut host, decoy);
        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(c.userdata)?;
            heap.bind_userdata_token(c.userdata, decoy_raw)
        })
        .expect("same-generation rebind");
        assert_eq!(
            pending_records(&mut host),
            vec![
                (c.raw, c.userdata.slot(), c.userdata.generation()),
                (a.raw, a.userdata.slot(), a.userdata.generation()),
            ],
            "the stored raw snapshot is not rebuilt from the current token"
        );
        let (offers, offer_trace) = heap::with_heap(&mut host, |heap| {
            let mut session = heap::SelectingPendingMark::start(heap);
            let mut trace = Vec::new();
            for _ in 0..10_000 {
                let (finished, _meter, _counts, offers) = session.pulse_with_queue_meter(1);
                trace.extend_from_slice(&offers);
                if finished {
                    break;
                }
            }
            Ok((trace.len(), trace))
        })
        .expect("offer mark with a rebound head");
        assert_eq!(
            offers, 2,
            "the rebound head and its successor are both offered"
        );
        assert_eq!(
            offer_trace,
            vec![
                (c.userdata.slot(), c.userdata.generation()),
                (a.userdata.slot(), a.userdata.generation()),
            ],
            "the raw mismatch skips retention but does not stop the identity cursor"
        );
        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(c.userdata)?;
            heap.bind_userdata_token(c.userdata, c.raw)
        })
        .expect("restore C binding");

        // Retire A, then a real ordinary collection closes its token and
        // advances the generation. The old identity is no longer queued.
        let a_generation = slot_generation(&mut host, a.userdata.slot());
        heap::retire_pending_finalizer_test_control(&mut host, a.userdata).expect("retire A");
        let _ = heap::collect(&mut host).expect("ordinary collection closes A");
        assert_eq!(
            slot_generation(&mut host, a.userdata.slot()),
            a_generation + 1,
            "the ordinary collection really advanced the released slot"
        );
        assert_eq!(
            pending_records(&mut host),
            vec![(c.raw, c.userdata.slot(), c.userdata.generation())],
            "the old generation is not in the queue"
        );
        assert!(pending_flag(&mut host, c.userdata));

        // A Release tail refuses the retirement control before any queue write.
        let orphan = heap::with_heap(&mut host, |heap| heap.alloc()).expect("orphan table");
        heap::with_heap(&mut host, |heap| {
            let weak = weak_table(heap, "v")?;
            heap.set_number_edge(weak, 1.0, orphan)?;
            heap.write_root(frame, 3, weak)
        })
        .expect("orphan weak root");
        let boundary = publish_boundary(&mut host);
        assert!(matches!(boundary, PublishedBoundary::Published { .. }));
        let before = heap::with_heap(&mut host, |heap| Ok(heap.pending_queue_counts()))
            .expect("queue counts");
        let refused = heap::retire_pending_finalizer_test_control(&mut host, c.userdata)
            .expect_err("saved release tail");
        assert_eq!(refused, HeapError::SweepPending);
        let after = heap::with_heap(&mut host, |heap| Ok(heap.pending_queue_counts()))
            .expect("queue counts");
        assert_eq!(
            before, after,
            "a refused retirement writes no queue storage"
        );
        finish_release(&mut host);

        // A saved mark refuses the same entry with no queue write.
        heap::open_saved_mark_for_test(&mut host).expect("saved mark");
        let before = heap::with_heap(&mut host, |heap| Ok(heap.pending_queue_counts()))
            .expect("queue counts");
        let refused = heap::retire_pending_finalizer_test_control(&mut host, c.userdata)
            .expect_err("saved mark");
        assert_eq!(refused, HeapError::MarkDenied);
        let after = heap::with_heap(&mut host, |heap| Ok(heap.pending_queue_counts()))
            .expect("queue counts");
        assert_eq!(
            before, after,
            "a saved mark refusal writes no queue storage"
        );
    });
}
