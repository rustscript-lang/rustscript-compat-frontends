//! Private typed userdata resource lifecycle on the unwired Lua heap.
//!
//! A bare userdata has no Core resource token. The private host path adds a
//! separate `lua.userdata` typed token that is bound in both directions:
//! the heap slot stores the raw Core handle and the resource payload stores
//! the Lua domain, kind, slot, and generation.
//!
//! The first test is the baseline witness for this slice. It uses only the
//! pre-existing bare userdata API and shows that the old path neither
//! registers nor closes a Core resource. It is not the business RED: the
//! typed path cannot compile before its issuer exists.

use std::panic::AssertUnwindSafe;

use vm::Value;

use crate::heap::{
    self, HandleKind, HeapError, HeapLimits, LuaHeap, LuaTableResource, LuaUserdataResource,
    PublishedBoundary, PublishedResume, RootFrameId, TableRef, UserdataRef,
};

fn limits() -> HeapLimits {
    HeapLimits {
        nodes: 16,
        roots: 4,
        edges: 8,
        max_generation: 8,
    }
}

fn room() -> HeapLimits {
    HeapLimits {
        nodes: 24,
        roots: 8,
        edges: 16,
        max_generation: 8,
    }
}

fn parts(handle: UserdataRef) -> (u64, u32, u32) {
    (handle.domain(), handle.slot(), handle.generation())
}

fn resource_handle(raw: u64) -> vm::ResourceHandle {
    vm::ResourceHandle::from_raw(raw).expect("valid resource handle")
}

fn userdata_handle(raw: i64) -> vm::ResourceHandle {
    let raw = u64::try_from(raw).expect("positive resource handle");
    vm::ResourceHandle::from_raw(raw).expect("valid resource handle")
}

fn bound_raw(host: &mut vm::HostContext<'_>, userdata: UserdataRef) -> u64 {
    heap::with_heap(host, |heap| heap.published_userdata_token(userdata)).expect("bound userdata")
}

fn table_ref(host: &vm::HostContext<'_>, raw: i64) -> TableRef {
    let handle = userdata_handle(raw);
    let token = host
        .typed_resource_with_key::<LuaTableResource>(handle, &LuaTableResource::key())
        .expect("table token");
    host.resource(&token).expect("live table").get().table()
}

fn publish_table_token(host: &mut vm::HostContext<'_>) -> (TableRef, i64) {
    match heap::publish(host).expect("publish table") {
        Value::Int(raw) => (table_ref(host, raw), raw),
        other => panic!("publish returned {other:?}"),
    }
}

fn userdata_token_open(host: &vm::HostContext<'_>, raw: u64) -> bool {
    host.typed_resource_with_key::<LuaUserdataResource>(
        resource_handle(raw),
        &LuaUserdataResource::key(),
    )
    .is_ok()
}

fn table_token_open(host: &vm::HostContext<'_>, raw: i64) -> bool {
    host.typed_resource_with_key::<LuaTableResource>(userdata_handle(raw), &LuaTableResource::key())
        .is_ok()
}

fn scope_code(error: &vm::HostContextError) -> vm::resource::ResourceErrorCode {
    match error.kind() {
        vm::HostContextErrorKind::Scope(vm::execution_scope::ExecutionScopeError::Resource(
            resource,
        ))
        | vm::HostContextErrorKind::Resource(resource) => resource.code(),
        other => panic!("not a resource error: {other:?}"),
    }
}

fn assert_type_mismatch(error: &HeapError) {
    match error {
        HeapError::Scope(message) => assert!(
            message.contains("resource_type_mismatch")
                || message.contains("resource_type_key_mismatch"),
            "expected the real Core type refusal, got {message}"
        ),
        other => panic!("expected a scope error, got {other:?}"),
    }
}

fn slot_generation(heap: &LuaHeap, slot: u32) -> u32 {
    heap.slot_generation(slot).expect("slot exists")
}

fn live_in(heap: &LuaHeap, slot: u32) -> bool {
    heap.slot_is_live(slot)
}

struct Graph {
    owner: TableRef,
    live_table: TableRef,
    live_table_raw: i64,
    live_userdata: UserdataRef,
    doomed_userdata: UserdataRef,
    doomed_userdata_raw: u64,
    doomed_table: TableRef,
    doomed_table_raw: i64,
    frame: RootFrameId,
}

/// One root/live layout: a weak table owner, one rooted table, one rooted
/// typed userdata, one doomed typed userdata, and one doomed table control.
fn seed(host: &mut vm::HostContext<'_>) -> Graph {
    heap::install(host, room()).expect("install");
    let (live_table, live_table_raw) = publish_table_token(host);
    let (doomed_table, doomed_table_raw) = publish_table_token(host);
    let live_userdata = heap::publish_userdata(host).expect("live userdata");
    let doomed_userdata = heap::publish_userdata(host).expect("doomed userdata");
    let doomed_userdata_raw = bound_raw(host, doomed_userdata);
    let (owner, frame) = heap::with_heap(host, |heap| {
        let meta = heap.alloc()?;
        heap.set_mode_scalar(meta, Value::string("v"))?;
        let owner = heap.alloc()?;
        heap.set_metatable(owner, Some(meta))?;
        heap.set_number_userdata(owner, 1.0, doomed_userdata)?;
        heap.set_number_edge(owner, 2.0, doomed_table)?;
        let frame = heap.enter_roots(4)?;
        heap.write_root(frame, 0, meta)?;
        heap.write_root(frame, 1, owner)?;
        heap.write_root(frame, 2, live_table)?;
        heap.write_userdata_root(frame, 3, live_userdata)?;
        Ok((owner, frame))
    })
    .expect("graph");
    Graph {
        owner,
        live_table,
        live_table_raw,
        live_userdata,
        doomed_userdata,
        doomed_userdata_raw,
        doomed_table,
        doomed_table_raw,
        frame,
    }
}

#[test]
fn baseline_bare_userdata_has_no_core_token_and_collect_releases_it() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        crate::heap::install(&mut host, limits()).expect("install");
        let resources_before = host.resource_count();

        let bare =
            crate::heap::with_heap(&mut host, LuaHeap::alloc_userdata).expect("bare userdata");
        let (domain, slot, generation) = parts(bare);
        assert_eq!(
            host.resource_count(),
            resources_before,
            "the old bare userdata path owns no Core resource"
        );
        assert!(
            crate::heap::with_heap(&mut host, |heap| Ok(heap.userdata_alive(bare))).expect("alive")
        );

        // The pure-heap collector has no HostContext. With no bound token it
        // releases the node and never touches the scope resource table.
        let report = crate::heap::with_heap(&mut host, |heap| Ok(heap.collect())).expect("collect");
        assert_eq!(report.live_nodes, 0);
        assert_eq!(host.resource_count(), resources_before);
        assert!(
            !crate::heap::with_heap(&mut host, |heap| Ok(heap.userdata_alive(bare))).expect("dead")
        );

        let reused =
            crate::heap::with_heap(&mut host, LuaHeap::alloc_userdata).expect("reuse bare slot");
        assert_eq!(parts(reused), (domain, slot, generation + 1));
        assert_eq!(host.resource_count(), resources_before);
    });
}

#[test]
fn issued_typed_userdata_binds_slot_and_core_payload_both_ways() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let resources_before = host.resource_count();
        let (table, table_raw) = publish_table_token(&mut host);
        let userdata = heap::publish_userdata(&mut host).expect("typed userdata");
        assert_eq!(
            host.resource_count(),
            resources_before + 2,
            "only the two typed resources occupy Core slots"
        );

        let raw = bound_raw(&mut host, userdata);
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.published_token(table)).expect("table bound"),
            u64::try_from(table_raw).expect("positive table token")
        );
        assert_ne!(
            raw,
            u64::try_from(table_raw).expect("positive table token"),
            "the userdata token is a different Core resource"
        );

        // The heap slot stores the same handle the resource payload carries.
        {
            let borrowed = host
                .borrow_resource_with_key::<LuaUserdataResource>(
                    resource_handle(raw),
                    &LuaUserdataResource::key(),
                )
                .expect("typed userdata token");
            let payload = borrowed.get().userdata();
            assert_eq!(
                (payload.domain(), payload.slot(), payload.generation()),
                (userdata.domain(), userdata.slot(), userdata.generation())
            );
            assert_eq!(payload.kind(), HandleKind::Userdata);
        }
        assert_eq!(
            heap::borrow_published_userdata(&host, userdata).expect("checked borrow"),
            userdata
        );

        // A bare userdata on the same heap stays outside the resource table.
        let bare = heap::with_heap(&mut host, LuaHeap::alloc_userdata).expect("bare userdata");
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.userdata_alive(bare))).expect("bare live")
        );
        assert_eq!(host.resource_count(), resources_before + 2);
        assert!(table_token_open(&host, table_raw));
        assert!(userdata_token_open(&host, raw));
    });
}

#[test]
fn borrow_rejects_wrong_type_foreign_arena_and_foreign_heap() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let (table, table_raw) = publish_table_token(&mut host);
        let userdata = heap::publish_userdata(&mut host).expect("typed userdata");
        let raw = bound_raw(&mut host, userdata);

        // Concrete TypeId confusion and catalog-key confusion are both
        // rejected before any borrow reaches the payload.
        let by_type = host
            .typed_resource::<LuaUserdataResource>(userdata_handle(table_raw))
            .expect_err("table is not a userdata");
        assert_eq!(
            scope_code(&by_type),
            vm::resource::ResourceErrorCode::ResourceTypeMismatch
        );
        let by_key = host
            .borrow_resource_with_key::<LuaUserdataResource>(
                userdata_handle(table_raw),
                &LuaUserdataResource::key(),
            )
            .expect_err("table key is not lua.userdata");
        assert_eq!(
            scope_code(&by_key),
            vm::resource::ResourceErrorCode::ResourceTypeKeyMismatch
        );
        let table_as_userdata = host
            .borrow_resource_with_key::<LuaTableResource>(
                resource_handle(raw),
                &LuaTableResource::key(),
            )
            .expect_err("userdata is not a table");
        assert_eq!(
            scope_code(&table_as_userdata),
            vm::resource::ResourceErrorCode::ResourceTypeKeyMismatch
        );
        assert_eq!(
            heap::borrow_published_userdata(&host, userdata).expect("own token"),
            userdata
        );

        // A foreign VM owns a different resource arena. Even a genuine
        // LuaUserdataResource handle from it is refused here.
        let foreign_raw = crate::support::with_idle_vm(|other_vm| {
            let mut other_host = other_vm.host_context();
            let mut scratch = LuaHeap::new(limits());
            let foreign_userdata = scratch.alloc_userdata().expect("scratch userdata");
            let resource = other_host
                .push_resource(LuaUserdataResource::new(foreign_userdata))
                .expect("foreign resource");
            resource.handle().raw()
        });
        let foreign = host
            .borrow_resource_with_key::<LuaUserdataResource>(
                resource_handle(foreign_raw),
                &LuaUserdataResource::key(),
            )
            .expect_err("foreign arena");
        assert_eq!(
            scope_code(&foreign),
            vm::resource::ResourceErrorCode::ResourceHandleWrongTable
        );

        // A handle from another LuaHeap domain is refused before the raw
        // token is even read.
        let mut other_heap = LuaHeap::new(limits());
        let other_userdata = other_heap.alloc_userdata().expect("other heap userdata");
        assert_eq!(
            heap::borrow_published_userdata(&host, other_userdata),
            Err(HeapError::WrongDomain)
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| {
                heap.bind_userdata_token(other_userdata, raw)
            }),
            Err(HeapError::WrongDomain)
        );
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(table))).expect("table live"));
        assert!(userdata_token_open(&host, raw));
    });
}

#[test]
fn borrow_rejects_same_type_misbind_and_old_generation() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, room()).expect("install");
        let first = heap::publish_userdata(&mut host).expect("first");
        let second = heap::publish_userdata(&mut host).expect("second");
        let first_raw = bound_raw(&mut host, first);
        let second_raw = bound_raw(&mut host, second);
        let frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(2)?;
            heap.write_userdata_root(frame, 0, first)?;
            heap.write_userdata_root(frame, 1, second)?;
            Ok(frame)
        })
        .expect("roots");

        // Same concrete type and same key, but the payload names another
        // object. Only the payload check can reject this binding.
        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(first)?;
            heap.bind_userdata_token(first, second_raw)
        })
        .expect("misbind fixture");
        assert_eq!(
            heap::borrow_published_userdata(&host, first),
            Err(HeapError::TokenMismatch)
        );
        assert_eq!(
            heap::borrow_published_userdata(&host, second).expect("other object open"),
            second
        );

        // A table token in a userdata slot is refused by catalog key.
        let (table, table_raw) = publish_table_token(&mut host);
        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(first)?;
            let raw = u64::try_from(table_raw).expect("positive table token");
            heap.bind_userdata_token(first, raw)
        })
        .expect("wrong-type fixture");
        assert_type_mismatch(
            &heap::borrow_published_userdata(&host, first).expect_err("wrong type"),
        );
        assert!(table_token_open(&host, table_raw));
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.published_token(table)).expect("table bound"),
            u64::try_from(table_raw).expect("positive table token")
        );

        // Restore the first binding so a real collect can release the slot,
        // then reuse it at the next generation. The old handle is stale even
        // though that slot is live again.
        heap::with_heap(&mut host, |heap| {
            heap.unbind_userdata_token(first)?;
            heap.bind_userdata_token(first, first_raw)
        })
        .expect("restore first");
        heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).expect("leave");
        let doomed_raw = first_raw;
        let doomed_slot = first.slot();
        heap::collect(&mut host).expect("collect");
        assert!(!heap::with_heap(&mut host, |heap| Ok(heap.userdata_alive(first))).expect("dead"));
        assert_eq!(
            heap::borrow_published_userdata(&host, first),
            Err(HeapError::StaleHandle)
        );
        let reused = heap::publish_userdata(&mut host).expect("reuse");
        assert!(reused.slot() == doomed_slot || reused.slot() == table.slot());
        assert_eq!(reused.generation(), 2);
        assert_ne!(bound_raw(&mut host, reused), doomed_raw);
        assert_eq!(
            heap::borrow_published_userdata(&host, first),
            Err(HeapError::StaleHandle)
        );
        assert_eq!(
            heap::borrow_published_userdata(&host, reused).expect("reused open"),
            reused
        );
    });
}

#[test]
fn capacity_refusal_rolls_back_only_the_empty_reservation() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(
            &mut host,
            HeapLimits {
                nodes: 2048,
                roots: 4,
                edges: 4,
                max_generation: 8,
            },
        )
        .expect("install");
        let userdata = heap::publish_userdata(&mut host).expect("live userdata");
        let (table, table_raw) = publish_table_token(&mut host);
        let userdata_raw = bound_raw(&mut host, userdata);
        let frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(2)?;
            heap.write_root(frame, 0, table)?;
            heap.write_userdata_root(frame, 1, userdata)?;
            Ok(frame)
        })
        .expect("roots");

        let freed = host
            .push_resource(crate::UnrelatedHostResource)
            .expect("spare resource");
        while host.resource_count() < vm::resource::handle::DEFAULT_MAX_RESOURCES {
            host.push_resource(crate::UnrelatedHostResource)
                .expect("fill resource table");
        }
        assert_eq!(
            host.resource_count(),
            vm::resource::handle::DEFAULT_MAX_RESOURCES
        );
        let before = heap::stats(&mut host).expect("before");
        let error = heap::publish_userdata(&mut host).expect_err("full Core table");
        match &error {
            HeapError::Scope(message) if message.contains("resource_limit_exceeded") => {}
            other => panic!("expected the real Core capacity refusal, got {other:?}"),
        }
        let after = heap::stats(&mut host).expect("after");
        assert_eq!(
            after.node_slots,
            before.node_slots + 1,
            "the Lua slot was reserved before push_resource refused"
        );
        assert_eq!(after.live_nodes, before.live_nodes);
        assert_eq!(after.live_roots, before.live_roots);
        assert_eq!(
            host.resource_count(),
            vm::resource::handle::DEFAULT_MAX_RESOURCES,
            "a refused push leaves the Core table unchanged"
        );
        assert!(userdata_token_open(&host, userdata_raw));
        assert!(table_token_open(&host, table_raw));
        assert_eq!(
            heap::borrow_published_userdata(&host, userdata).expect("existing open"),
            userdata
        );

        host.close_resource::<crate::UnrelatedHostResource>(
            freed.handle(),
            vm::ResourceCloseReason::Requested,
        )
        .expect("free one Core slot");
        let reused = heap::publish_userdata(&mut host).expect("reuse the refused slot");
        assert_eq!(reused.slot(), before.node_slots);
        assert_eq!(
            reused.generation(),
            2,
            "rollback advanced the empty slot before reuse"
        );
        assert_eq!(
            heap::borrow_published_userdata(&host, reused).expect("new token open"),
            reused
        );
        assert_eq!(
            host.resource_count(),
            vm::resource::handle::DEFAULT_MAX_RESOURCES
        );
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.alive(table))).expect("table"),
            "the control table stays live"
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(slot_generation(heap, userdata.slot())))
                .expect("live generation"),
            userdata.generation()
        );
        let _ = frame;
    });
}

#[test]
fn post_registration_failure_closes_confirmed_then_releases_the_slot() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let userdata = heap::publish_userdata(&mut host).expect("live userdata");
        let (table, table_raw) = publish_table_token(&mut host);
        let userdata_raw = bound_raw(&mut host, userdata);
        let frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(2)?;
            heap.write_root(frame, 0, table)?;
            heap.write_userdata_root(frame, 1, userdata)?;
            Ok(frame)
        })
        .expect("roots");
        let resources_before = host.resource_count();
        let stats_before = heap::stats(&mut host).expect("before");

        let error = heap::publish_userdata_after_register_failure(&mut host)
            .expect_err("injected post-registration failure");
        match &error {
            HeapError::Scope(message) => assert!(
                message.contains("injected userdata failure after registration"),
                "unexpected failure: {message}"
            ),
            other => panic!("expected the injected scope failure, got {other:?}"),
        }
        let witness =
            heap::last_userdata_post_register_failure(&host).expect("post-registration witness");
        assert_eq!(witness.registered_count, resources_before + 1);
        assert_eq!(witness.closed_count, resources_before);
        assert_eq!(witness.close, vm::resource::CloseProgress::Ready);
        assert_eq!(witness.generation_after, 2);
        assert_eq!(witness.slot, stats_before.node_slots);
        assert!(
            !userdata_token_open(&host, witness.raw),
            "the confirmed-closed handle must not be usable"
        );
        assert!(
            host.typed_resource_with_key::<LuaUserdataResource>(
                resource_handle(witness.raw),
                &LuaUserdataResource::key(),
            )
            .is_err()
        );
        assert_eq!(host.resource_count(), resources_before);
        let stats_after = heap::stats(&mut host).expect("after");
        assert_eq!(stats_after.node_slots, stats_before.node_slots + 1);
        assert_eq!(stats_after.live_nodes, stats_before.live_nodes);
        assert_eq!(stats_after.live_roots, stats_before.live_roots);
        assert!(userdata_token_open(&host, userdata_raw));
        assert!(table_token_open(&host, table_raw));
        assert_eq!(
            heap::borrow_published_userdata(&host, userdata).expect("existing open"),
            userdata
        );

        let reused = heap::publish_userdata(&mut host).expect("reuse failed slot");
        assert_eq!(reused.slot(), witness.slot);
        assert_eq!(reused.generation(), witness.generation_after);
        assert_ne!(bound_raw(&mut host, reused), witness.raw);
        assert_eq!(
            heap::borrow_published_userdata(&host, reused).expect("new token open"),
            reused
        );
        let _ = frame;
    });
}

#[test]
fn full_collect_closes_doomed_userdata_and_table_before_release() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = seed(&mut host);
        let resources_before = host.resource_count();
        let live_userdata_raw = bound_raw(&mut host, graph.live_userdata);
        let doomed_userdata_slot = graph.doomed_userdata.slot();
        let doomed_userdata_generation = graph.doomed_userdata.generation();
        let doomed_table_slot = graph.doomed_table.slot();
        let doomed_table_generation = graph.doomed_table.generation();
        let _ = heap::take_scan_meter(&mut host);

        let (report, seen) = heap::collect_observing(&mut host, |read| {
            assert!(!read.finished());
            assert!(!read.release_started());
            assert_eq!(
                read.number(graph.owner, 1.0).expect("weak userdata"),
                Value::Null
            );
            assert_eq!(
                read.number(graph.owner, 2.0).expect("weak table"),
                Value::Null
            );
            assert_eq!(read.occupying_dead(), 2);
            assert_eq!(read.dead_still_physical(), 2);
            assert!(read.still_physical(graph.doomed_table));
            assert!(read.alive(graph.live_table).expect("live table"));
            (read.occupying_dead(), read.dead_still_physical())
        })
        .expect("collect observing");
        assert_eq!(seen, Some((2, 2)));
        assert_eq!(
            report.live_nodes, 4,
            "meta, owner, live table, live userdata"
        );
        assert_eq!(report.live_roots, 4);
        let meter = heap::take_scan_meter(&mut host).expect("meter");
        assert_eq!(meter.token_closes, 2, "one close per doomed typed token");
        assert_eq!(
            host.resource_count(),
            resources_before - 2,
            "both doomed tokens returned their Core slots"
        );
        assert!(!userdata_token_open(&host, graph.doomed_userdata_raw));
        assert!(!table_token_open(&host, graph.doomed_table_raw));
        assert!(userdata_token_open(&host, live_userdata_raw));
        assert!(table_token_open(&host, graph.live_table_raw));
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(
                heap.userdata_alive(graph.doomed_userdata)
            ))
            .expect("doomed userdata")
        );
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.alive(graph.doomed_table)))
                .expect("doomed table")
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(slot_generation(
                heap,
                doomed_userdata_slot
            )))
            .expect("userdata slot"),
            doomed_userdata_generation + 1
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(slot_generation(
                heap,
                doomed_table_slot
            )))
            .expect("table slot"),
            doomed_table_generation + 1
        );
        assert_eq!(
            heap::borrow_published_userdata(&host, graph.doomed_userdata),
            Err(HeapError::StaleHandle)
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.number_userdata(graph.owner, 1.0))
                .expect("weak userdata cleared"),
            None
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.number_value(graph.owner, 2.0))
                .expect("weak table cleared"),
            Value::Null
        );
        assert_eq!(
            heap::borrow_published_userdata(&host, graph.live_userdata).expect("live open"),
            graph.live_userdata
        );
        assert!(
            heap::stats(&mut host).expect("slot storage").node_slots > doomed_userdata_slot,
            "the released slot stays in the vector for reuse"
        );
    });
}

#[test]
fn step_collect_closes_before_release_then_reissues_the_same_slots() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = seed(&mut host);
        let resources_before = host.resource_count();
        let live_userdata_raw = bound_raw(&mut host, graph.live_userdata);
        let live_table_raw = graph.live_table_raw;
        let doomed_userdata_slot = graph.doomed_userdata.slot();
        let doomed_userdata_generation = graph.doomed_userdata.generation();
        let doomed_table_slot = graph.doomed_table.slot();
        let doomed_table_generation = graph.doomed_table.generation();
        let _ = heap::take_scan_meter(&mut host);

        let boundary = heap::collect_until_published(&mut host).expect("publish boundary");
        assert!(
            matches!(boundary, PublishedBoundary::Published { .. }),
            "two dead typed slots must publish and save a release tail"
        );
        let meter = heap::take_scan_meter(&mut host).expect("publish meter");
        assert_eq!(meter.token_closes, 2);
        assert!(meter.sweep_edge_checks > 0, "weak edges were published");
        assert_eq!(host.resource_count(), resources_before - 2);
        assert!(!userdata_token_open(&host, graph.doomed_userdata_raw));
        assert!(!table_token_open(&host, graph.doomed_table_raw));
        assert!(userdata_token_open(&host, live_userdata_raw));
        assert!(table_token_open(&host, live_table_raw));
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 0);

        // The token is gone, but the Lua slot is still physical at the same
        // generation: Release has not run yet.
        assert!(
            heap::with_heap(&mut host, |heap| Ok(
                heap.userdata_alive(graph.doomed_userdata)
            ))
            .expect("physical userdata")
        );
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.alive(graph.doomed_table)))
                .expect("physical table")
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(slot_generation(
                heap,
                doomed_userdata_slot
            )))
            .expect("userdata generation"),
            doomed_userdata_generation,
            "publish must not advance the generation"
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(slot_generation(
                heap,
                doomed_table_slot
            )))
            .expect("table generation"),
            doomed_table_generation
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| {
                heap.published_userdata_token(graph.doomed_userdata)
            }),
            Err(HeapError::StaleHandle),
            "the binding is cleared before release"
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.number_userdata(graph.owner, 1.0))
                .expect("weak userdata cleared"),
            None
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.number_value(graph.owner, 2.0))
                .expect("weak table cleared"),
            Value::Null
        );
        assert!(heap::saved_sweep_tail(&mut host).expect("saved tail"));

        // A second collect while the tail is saved is refused, not re-closed.
        assert_eq!(
            heap::collect(&mut host).expect_err("tail blocks a host collect"),
            HeapError::SweepPending
        );
        assert!(matches!(
            heap::resume_published_sweep(&mut host, 1).expect("one release"),
            PublishedResume::Paused
        ));
        let live_after_one = heap::with_heap(&mut host, |heap| {
            Ok([doomed_userdata_slot, doomed_table_slot]
                .into_iter()
                .filter(|slot| live_in(heap, *slot))
                .count())
        })
        .expect("partial release");
        assert_eq!(
            live_after_one, 1,
            "budget 1 releases one dead slot and leaves the other physical"
        );
        let finished = heap::resume_published_sweep(&mut host, 8).expect("finish release");
        let report = match finished {
            PublishedResume::Completed(report) => report,
            PublishedResume::Paused => panic!("budget must finish the release tail"),
        };
        assert_eq!(report.live_nodes, 4);

        assert!(
            !heap::with_heap(&mut host, |heap| Ok(
                heap.userdata_alive(graph.doomed_userdata)
            ))
            .expect("released userdata")
        );
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.alive(graph.doomed_table)))
                .expect("released table")
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(slot_generation(
                heap,
                doomed_userdata_slot
            )))
            .expect("userdata next generation"),
            doomed_userdata_generation + 1
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(slot_generation(
                heap,
                doomed_table_slot
            )))
            .expect("table next generation"),
            doomed_table_generation + 1
        );
        assert_eq!(
            heap::resume_published_sweep(&mut host, 1).expect_err("no second release"),
            HeapError::SweepDenied
        );

        // Reissue one of the released userdata slots at its next generation.
        let reused = heap::publish_userdata(&mut host).expect("reissue");
        assert!(reused.slot() == doomed_userdata_slot || reused.slot() == doomed_table_slot);
        assert_eq!(reused.generation(), 2);
        assert_eq!(
            heap::borrow_published_userdata(&host, reused).expect("new token open"),
            reused
        );
        let reused_frame = heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(1)?;
            heap.write_userdata_root(frame, 0, reused)?;
            Ok(frame)
        })
        .expect("root reissue");
        let resources_after = host.resource_count();
        let _ = heap::take_scan_meter(&mut host);
        let again = heap::collect(&mut host).expect("repeat collect");
        assert_eq!(again.live_nodes, 5);
        assert_eq!(host.resource_count(), resources_after);
        assert_eq!(
            heap::take_scan_meter(&mut host)
                .expect("repeat meter")
                .token_closes,
            0,
            "a live rooted token is not closed again"
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(slot_generation(heap, reused.slot())))
                .expect("reused generation"),
            2
        );
        let reused_raw = bound_raw(&mut host, reused);
        assert!(userdata_token_open(&host, reused_raw));
        let _ = reused_frame;
        let _ = graph.frame;
    });
}

#[test]
fn pure_heap_collect_refuses_a_bound_token_instead_of_skipping_it() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let userdata = heap::publish_userdata(&mut host).expect("typed userdata");
        let raw = bound_raw(&mut host, userdata);
        let generation = userdata.generation();
        let resources_before = host.resource_count();

        let panicked = std::panic::catch_unwind(AssertUnwindSafe(|| {
            heap::with_heap(&mut host, |heap| Ok(heap.collect()))
        }));
        assert!(
            panicked.is_err(),
            "pure collect must not silently release a bound Core resource"
        );
        assert_eq!(host.resource_count(), resources_before);
        assert!(userdata_token_open(&host, raw));
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.userdata_alive(userdata)))
                .expect("still live")
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(slot_generation(heap, userdata.slot())))
                .expect("generation"),
            generation
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.published_userdata_token(userdata))
                .expect("binding intact"),
            raw
        );

        // The host collector closes the token first and only then sweeps.
        heap::collect(&mut host).expect("host collect");
        assert!(!userdata_token_open(&host, raw));
        assert_eq!(host.resource_count(), resources_before - 1);
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.userdata_alive(userdata)))
                .expect("released")
        );
    });
}

#[test]
fn failed_userdata_close_keeps_failure_and_suffix_bindings() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, room()).expect("install");
        let (table, table_raw) = publish_table_token(&mut host);
        let prefix = heap::publish_userdata(&mut host).expect("prefix userdata");
        let prefix_raw = bound_raw(&mut host, prefix);
        let obstacle = heap::with_heap(&mut host, |heap| {
            let obstacle = heap.alloc_userdata()?;
            let raw = u64::try_from(table_raw).expect("positive table token");
            heap.bind_userdata_token(obstacle, raw)?;
            Ok(obstacle)
        })
        .expect("obstacle");
        let suffix = heap::publish_userdata(&mut host).expect("suffix userdata");
        let suffix_raw = bound_raw(&mut host, suffix);
        let (owner, frame) = heap::with_heap(&mut host, |heap| {
            let meta = heap.alloc()?;
            heap.set_mode_scalar(meta, Value::string("v"))?;
            let owner = heap.alloc()?;
            heap.set_metatable(owner, Some(meta))?;
            heap.set_number_userdata(owner, 1.0, prefix)?;
            heap.set_number_userdata(owner, 2.0, suffix)?;
            let frame = heap.enter_roots(3)?;
            heap.write_root(frame, 0, meta)?;
            heap.write_root(frame, 1, owner)?;
            heap.write_root(frame, 2, table)?;
            Ok((owner, frame))
        })
        .expect("fixture");
        let resources_before = host.resource_count();
        let stats_before = heap::stats(&mut host).expect("before");
        let prefix_generation =
            heap::with_heap(&mut host, |heap| Ok(slot_generation(heap, prefix.slot())))
                .expect("prefix generation");
        let suffix_generation =
            heap::with_heap(&mut host, |heap| Ok(slot_generation(heap, suffix.slot())))
                .expect("suffix generation");

        let error = heap::collect(&mut host).expect_err("table token in a userdata slot");
        assert_type_mismatch(&error);
        assert!(
            !userdata_token_open(&host, prefix_raw),
            "closed prefix is real"
        );
        assert!(!userdata_token_open(&host, prefix_raw));
        assert!(userdata_token_open(&host, suffix_raw), "suffix stays open");
        assert!(
            table_token_open(&host, table_raw),
            "owned table token stays open"
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| {
                Ok((
                    slot_generation(heap, prefix.slot()),
                    slot_generation(heap, suffix.slot()),
                ))
            })
            .expect("generations"),
            (prefix_generation, suffix_generation),
            "a failed close does not advance a generation"
        );
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 0);
        assert!(!heap::saved_sweep_tail(&mut host).expect("no tail"));
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.number_userdata(owner, 1.0))
                .expect("prefix weak value"),
            Some(prefix),
            "no weak edge is published before every close succeeds"
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.number_userdata(owner, 2.0))
                .expect("suffix weak value"),
            Some(suffix)
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.published_userdata_token(suffix))
                .expect("suffix binding"),
            suffix_raw
        );
        assert!(
            heap::with_heap(&mut host, |heap| {
                let raw = u64::try_from(table_raw).expect("positive table token");
                heap.published_userdata_token(obstacle)
                    .map(|stored| stored == raw)
            })
            .expect("obstacle binding"),
            "the failing binding stays on the slot"
        );
        assert_eq!(
            heap::stats(&mut host).expect("live nodes").live_nodes,
            stats_before.live_nodes,
            "failed close keeps every slot physical"
        );

        // Remove the obstacle. The already closed prefix is not recollected;
        // the suffix closes and the weak values are published.
        heap::with_heap(&mut host, |heap| heap.unbind_userdata_token(obstacle))
            .expect("remove obstacle");
        let report = heap::collect(&mut host).expect("retry");
        assert!(!userdata_token_open(&host, suffix_raw));
        assert_eq!(host.resource_count(), resources_before - 2);
        assert_eq!(report.live_nodes, 3, "meta, owner, and the rooted table");
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.number_userdata(owner, 1.0))
                .expect("prefix cleared"),
            None
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.number_userdata(owner, 2.0))
                .expect("suffix cleared"),
            None
        );
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.userdata_alive(prefix)))
                .expect("prefix released")
        );
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.userdata_alive(suffix)))
                .expect("suffix released")
        );
        assert!(table_token_open(&host, table_raw));
        let _ = frame;
    });
}

#[test]
fn externally_closed_userdata_token_is_retired_by_kind() {
    crate::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, limits()).expect("install");
        let userdata = heap::publish_userdata(&mut host).expect("userdata");
        let raw = bound_raw(&mut host, userdata);
        let resources_before = host.resource_count();
        let progress = host
            .close_resource::<LuaUserdataResource>(
                resource_handle(raw),
                vm::ResourceCloseReason::Requested,
            )
            .expect("external close");
        assert_eq!(progress, vm::resource::CloseProgress::Ready);
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.published_userdata_token(userdata))
                .expect("binding still stored"),
            raw
        );

        let report = heap::collect(&mut host).expect("collect retires the closed token");
        assert_eq!(report.live_nodes, 0);
        assert_eq!(host.resource_count(), resources_before - 1);
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.published_userdata_token(userdata)),
            Err(HeapError::StaleHandle)
        );
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.userdata_alive(userdata)))
                .expect("released")
        );
    });
}
