//! Acceptance for the unwired Lua private table heap.
//!
//! The production files are compiled through `#[path]` and are not registered
//! from `lua/mod.rs`, `lib.rs`, or the `.lua` entry. Weak tables and the
//! LuaJIT `ordered.lua` sample stay in the later integration stage.

#[path = "../src/frontends/lua/heap.rs"]
mod heap;

#[path = "luajit_heap_runtime/mod.rs"]
mod support;
#[path = "luajit_heap_runtime/value_boundary_tests.rs"]
mod value_boundary_tests;

use heap::{
    CollectReport, HandleKind, HeapLimits, LuaHeap, LuaTableResource, RootFrameId, TableRef,
};
use support::{Probe, run_probe, with_idle_vm};
use vm::{HostContext, ResourceHandle, Value};

fn tight_limits() -> HeapLimits {
    HeapLimits {
        nodes: 4,
        roots: 2,
        edges: 2,
        max_generation: 4,
    }
}

#[test]
fn alias_write_collect_reuse_and_hard_caps_stay_inside_one_heap() {
    let mut heap = LuaHeap::new(tight_limits());
    let left = heap.alloc().expect("first table");
    assert_eq!(left.kind(), HandleKind::Table);
    let right = heap.alloc().expect("second table");
    let alias = left;
    heap.set_edge(left, 1, right).expect("edge");
    assert_eq!(heap.edge(alias, 1).expect("read alias"), Some(right));
    assert!(heap.alive(left) && heap.alive(right));

    let unrooted = heap.collect();
    assert_eq!(unrooted.live_nodes, 0, "an edge is not a root");
    assert!(!heap.alive(left));
    assert!(!heap.alive(right));
    assert!(
        heap.edge(left, 1).is_err(),
        "a stale source is not a readable edge"
    );

    let reused = heap.alloc().expect("reuse freed slot");
    assert_eq!(reused.slot(), right.slot());
    assert_ne!(reused.generation(), right.generation());
    assert!(!heap.alive(right), "stale generation stays dead");
    assert_eq!(
        heap.stats().node_slots,
        2,
        "reuse must not grow slot storage"
    );

    let dropped = heap.alloc().expect("second reuse");
    let frame: RootFrameId = heap.enter_roots(1).expect("frame");
    heap.write_root(frame, 0, reused).expect("root reused");
    heap.set_edge(reused, 1, dropped).expect("reachable edge");
    let kept: CollectReport = heap.collect();
    assert_eq!(kept.live_nodes, 2, "a rooted source keeps its target");
    assert!(heap.alive(reused));
    assert!(heap.alive(dropped));

    heap.leave_roots(frame).expect("leave");
    let after_leave = heap.collect();
    assert_eq!(after_leave.live_nodes, 0);
    assert_eq!(heap.stats().live_roots, 0);
}

#[test]
fn failed_alloc_and_rollback_do_not_grow_live_storage() {
    let mut heap = LuaHeap::new(HeapLimits {
        nodes: 1,
        roots: 1,
        edges: 1,
        max_generation: 2,
    });
    let first = heap.alloc().expect("only slot");
    let before = heap.stats();
    assert!(heap.alloc().is_err(), "node cap");
    assert_eq!(heap.stats(), before);

    heap.rollback(first).expect("rollback unpublished");
    assert!(!heap.alive(first));
    assert_eq!(heap.stats().live_nodes, 0);
    assert_eq!(
        heap.stats().node_slots,
        1,
        "the slot stays for reuse, but it is not a live node"
    );
    let again = heap.alloc().expect("reuse after rollback");
    assert_ne!(again.generation(), first.generation());
    assert!(!heap.alive(first));
    assert!(heap.set_edge(again, 1, again).is_ok());
    let edges = heap.stats().edge_slots;
    assert!(heap.set_edge(again, 2, again).is_err(), "edge cap");
    assert_eq!(heap.stats().live_edges, 1);
    assert_eq!(heap.stats().edge_slots, edges);

    let root = heap.enter_roots(1).expect("one root");
    assert!(heap.enter_roots(1).is_err(), "root cap");
    assert_eq!(heap.stats().root_slots, 1);
    heap.leave_roots(root).expect("leave");
}

#[test]
fn generation_exhaustion_retires_the_slot_instead_of_wrapping() {
    let mut heap = LuaHeap::new(HeapLimits {
        nodes: 1,
        roots: 1,
        edges: 1,
        max_generation: 1,
    });
    let once = heap.alloc().expect("generation 1");
    heap.rollback(once).expect("free");
    assert!(heap.alloc().is_err(), "retired slot is not reused");
    assert!(!heap.alive(once));
    assert_eq!(heap.stats().live_nodes, 0);
    assert_eq!(
        heap.stats().node_slots,
        1,
        "retired storage remains, but is not live"
    );
}

#[test]
fn cyclic_create_and_collect_tracks_live_nodes_and_roots() {
    let mut heap = LuaHeap::new(HeapLimits {
        nodes: 8,
        roots: 4,
        edges: 8,
        max_generation: 8,
    });
    for round in 0..3 {
        let frame = heap.enter_roots(1).expect("frame");
        let kept = heap.alloc().expect("kept");
        let linked = heap.alloc().expect("linked");
        heap.set_edge(kept, 1, linked).expect("forward");
        heap.set_edge(linked, 1, kept).expect("back edge");
        heap.write_root(frame, 0, kept).expect("root");
        let orphan = heap.alloc().expect("orphan");
        let orphan_link = heap.alloc().expect("orphan link");
        heap.set_edge(orphan, 1, orphan_link)
            .expect("orphan forward");
        heap.set_edge(orphan_link, 1, orphan).expect("orphan back");
        let marked = heap.collect();
        assert_eq!(
            marked.live_nodes, 2,
            "round {round}: rooted cycle stays, orphan cycle is collected"
        );
        assert_eq!(heap.stats().live_roots, 1);
        assert!(heap.alive(kept) && heap.alive(linked));
        assert!(!heap.alive(orphan) && !heap.alive(orphan_link));
        heap.leave_roots(frame).expect("leave");
        assert_eq!(heap.collect().live_nodes, 0);
        assert_eq!(heap.stats().live_roots, 0);
    }
    assert_eq!(
        heap.stats().node_slots,
        4,
        "later rounds reuse the first four slots"
    );
    assert!(heap.stats().peak_roots <= 4);
}

#[test]
fn a_handle_from_another_heap_is_rejected() {
    let mut left = LuaHeap::new(tight_limits());
    let mut right = LuaHeap::new(tight_limits());
    let handle = left.alloc().expect("left table");
    assert!(!right.alive(handle));
    assert!(right.set_edge(handle, 1, handle).is_err());
}

#[test]
fn typed_resource_boundary_rejects_int_foreign_and_expired_values() {
    let transfer = run_probe(Probe::Transfer).expect("transfer");
    assert_eq!(transfer.stack, vec![Value::Int(1)]);
    assert!(
        transfer.body_entries > 0,
        "typed host body ran for a published table"
    );
    assert!(transfer.alive_entries > 0, "alive ran inside the same VM");

    let alias = run_probe(Probe::Alias).expect("alias");
    assert_eq!(
        alias.stack,
        vec![Value::Int(1)],
        "same table write is visible through the other handle"
    );

    let before_int = run_probe(Probe::RejectInt).expect_err("ordinary int is not a table");
    assert_eq!(
        before_int.body_entries, 0,
        "int rejection happens before the host body"
    );
    assert_eq!(before_int.alive_entries, 0);

    let published = run_probe(Probe::Publish).expect("publish handle");
    assert_eq!(
        published.published_raw,
        published.stack.first().and_then(|value| match value {
            Value::Int(raw) => Some(*raw),
            _ => None,
        })
    );
    let raw = match published.stack.as_slice() {
        [Value::Int(raw)] => *raw,
        other => panic!("publish did not return one handle: {other:?}"),
    };
    let foreign = run_probe(Probe::RejectForeign { raw }).expect_err("cross-vm handle is rejected");
    assert_eq!(
        foreign.body_entries, 0,
        "foreign rejection is before the host body"
    );
    assert_eq!(foreign.alive_entries, 0);

    let expired = run_probe(Probe::Expired).expect_err("expired handle is rejected");
    assert_eq!(
        expired.alive_entries, 0,
        "close runs; the later alive call must not"
    );

    let named = run_probe(Probe::NamedCarrier).expect("named carrier");
    assert_eq!(named.stack, vec![Value::Int(1)]);
    assert_eq!(named.forged_slot, Some(7));
    assert_eq!(named.forged_generation, Some(9));
    // A named record of copyable ints is not the table carrier. Rejecting one
    // ordinary int does not show that every Lua number, or an unrelated auth
    // token, is isolated from table tokens.
}

#[test]
fn nested_argument_root_keeps_the_first_argument_across_collect() {
    let rooted = run_probe(Probe::NestedRooted).expect("rooted nest");
    assert_eq!(rooted.stack, vec![Value::Int(1)]);
    let unrooted = run_probe(Probe::NestedUnrooted).expect_err("collected token is retired");
    assert!(
        unrooted.message.contains("resource_already_closed"),
        "an unrooted table's token must be invalid after collect: {unrooted}"
    );
    assert_eq!(
        unrooted.body_entries, 3,
        "enter, publish, and collect ran; the later observe must not"
    );
    assert_eq!(unrooted.alive_entries, 0);
}

#[test]
fn full_resource_table_publish_occupies_a_slot_then_rolls_it_back() {
    with_idle_vm(|vm| {
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
        let rooted_raw = publish_raw(&mut host);
        let rooted = table_ref(&host, rooted_raw);
        let frame = heap::with_heap(&mut host, |heap| heap.enter_roots(1)).expect("frame");
        heap::with_heap(&mut host, |heap| heap.write_root(frame, 0, rooted)).expect("root");
        let spare_raw = publish_raw(&mut host);
        let spare = resource_handle(spare_raw);
        while host.resource_count() < vm::resource::handle::DEFAULT_MAX_RESOURCES {
            let _filled = publish_raw(&mut host);
        }
        assert_eq!(
            host.resource_count(),
            vm::resource::handle::DEFAULT_MAX_RESOURCES
        );

        let before = heap::stats(&mut host).expect("stats");
        let failed = heap::publish(&mut host);
        let after = heap::stats(&mut host).expect("stats after");
        let error = failed.expect_err("a full resource table must refuse the next publish");
        assert_eq!(
            after.node_slots,
            before.node_slots + 1,
            "refused publish must occupy a lua slot before push_resource returns; error={error}"
        );
        assert_eq!(
            after.live_nodes, before.live_nodes,
            "rollback must release the occupied slot; error={error}"
        );
        assert_eq!(
            after.live_roots, before.live_roots,
            "existing roots must stay"
        );
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.alive(rooted))).expect("rooted check"),
            "the existing root stays live"
        );
        assert_eq!(
            host.resource_count(),
            vm::resource::handle::DEFAULT_MAX_RESOURCES
        );
        match &error {
            heap::HeapError::Scope(message) if message.contains("resource_limit_exceeded") => {}
            other => panic!("refusal must be the real push_resource limit, got {other:?}"),
        }

        let progress = host
            .close_resource::<LuaTableResource>(spare, vm::ResourceCloseReason::Requested)
            .expect("close one token to free a resource slot");
        assert_eq!(progress, vm::resource::CloseProgress::Ready);
        let reused_raw = publish_raw(&mut host);
        let reused = table_ref(&host, reused_raw);
        assert_eq!(
            reused.slot(),
            before.node_slots,
            "the slot occupied by the refused publish is reused"
        );
        assert_eq!(
            reused.generation(),
            2,
            "rollback must bump the generation before reuse"
        );
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(reused))).expect("reused check"));
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(rooted))).expect("root remains"));
    });
}

fn scope_resource_code(error: &vm::HostContextError) -> vm::resource::ResourceErrorCode {
    match error.kind() {
        vm::HostContextErrorKind::Scope(vm::execution_scope::ExecutionScopeError::Resource(
            resource,
        )) => resource.code(),
        other => panic!("close did not return a scope resource error: {other:?}"),
    }
}

fn root_table(host: &mut HostContext<'_>, table: TableRef) {
    let frame = heap::with_heap(host, |heap| heap.enter_roots(1)).expect("root frame");
    heap::with_heap(host, |heap| heap.write_root(frame, 0, table)).expect("root");
}

fn sweep_limits() -> HeapLimits {
    HeapLimits {
        nodes: 16,
        roots: 4,
        edges: 4,
        max_generation: 16,
    }
}

/// External `close_resource` retires the core token while the lua node still
/// stores it. The slot is not reused, so a later close reports
/// `ResourceAlreadyClosed`. Collection must still sweep that node and another
/// ordinary unreachable table, then accept a second collection in the same VM.
#[test]
fn collect_finishes_when_an_unreused_token_is_already_closed() {
    with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, sweep_limits()).expect("install");
        let unrelated = host
            .push_resource(UnrelatedHostResource)
            .expect("unrelated host resource");
        let rooted_raw = publish_raw(&mut host);
        let rooted = table_ref(&host, rooted_raw);
        root_table(&mut host, rooted);
        let retired_raw = publish_raw(&mut host);
        let retired = table_ref(&host, retired_raw);
        let retired_handle = resource_handle(retired_raw);
        let other_raw = publish_raw(&mut host);
        let other_handle = resource_handle(other_raw);

        let progress = host
            .close_resource::<LuaTableResource>(retired_handle, vm::ResourceCloseReason::Requested)
            .expect("external close");
        assert_eq!(progress, vm::resource::CloseProgress::Ready);
        let observed = host
            .close_resource::<LuaTableResource>(retired_handle, vm::ResourceCloseReason::Requested)
            .expect_err("slot was not reused");
        assert_eq!(
            scope_resource_code(&observed),
            vm::resource::ResourceErrorCode::ResourceAlreadyClosed
        );
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.alive(retired))).expect("retired node"),
            "the lua node still stores the closed token"
        );
        assert_eq!(
            host.resource_count(),
            3,
            "close already returned one quota unit"
        );
        assert_eq!(heap::stats(&mut host).expect("before").live_nodes, 3);

        let report = heap::collect(&mut host)
            .expect("an already-closed stored token must not stop collection");
        assert_eq!(report.live_nodes, 1);
        assert_eq!(
            host.resource_count(),
            2,
            "only the other doomed token is closed"
        );
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(rooted))).expect("rooted"));
        assert!(
            host.typed_resource_with_key::<LuaTableResource>(
                resource_handle(rooted_raw),
                &LuaTableResource::key(),
            )
            .is_ok(),
            "the rooted token stays valid"
        );
        assert!(
            host.typed_resource_with_key::<LuaTableResource>(
                other_handle,
                &LuaTableResource::key()
            )
            .is_err(),
            "the other unreachable table is collected"
        );
        assert!(
            host.resource(&unrelated).is_ok(),
            "unrelated resource stays"
        );
        assert!(
            host.scope_state::<heap::LuaHeap>().is_some(),
            "collection must not replace the scope with reset"
        );

        let again = heap::collect(&mut host).expect("a later collection must also finish");
        assert_eq!(again.live_nodes, 1);
        assert_eq!(
            host.resource_count(),
            2,
            "the second collection must not release quota again"
        );
        assert_eq!(heap::stats(&mut host).expect("after").collections, 2);
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(rooted))).expect("root remains"));
        assert!(
            host.resource(&unrelated).is_ok(),
            "unrelated resource remains"
        );
    });
}

/// `take_resource` vacates the core slot. The next publish reuses that slot, so
/// the stored token is `ResourceStale`. The successor in that slot must stay
/// usable, and another unreachable table must still be collected.
#[test]
fn collect_finishes_when_a_reused_token_is_stale() {
    with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, sweep_limits()).expect("install");
        let unrelated = host
            .push_resource(UnrelatedHostResource)
            .expect("unrelated host resource");
        let rooted_raw = publish_raw(&mut host);
        let rooted = table_ref(&host, rooted_raw);
        root_table(&mut host, rooted);
        let other_raw = publish_raw(&mut host);
        let other_handle = resource_handle(other_raw);
        let retired_raw = publish_raw(&mut host);
        let retired = table_ref(&host, retired_raw);
        let retired_handle = resource_handle(retired_raw);

        let _taken = host
            .take_resource::<LuaTableResource>(retired_handle)
            .expect("take the stored token");
        let successor_raw = publish_raw(&mut host);
        let successor_handle = resource_handle(successor_raw);
        let successor = table_ref(&host, successor_raw);
        root_table(&mut host, successor);
        assert_eq!(
            retired_handle.slot_index().expect("retired slot"),
            successor_handle.slot_index().expect("successor slot"),
            "the successor must occupy the same core slot"
        );
        assert_ne!(retired_handle.generation(), successor_handle.generation());
        let observed = host
            .close_resource::<LuaTableResource>(retired_handle, vm::ResourceCloseReason::Requested)
            .expect_err("reused slot");
        assert_eq!(
            scope_resource_code(&observed),
            vm::resource::ResourceErrorCode::ResourceStale
        );
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.alive(retired))).expect("retired node"),
            "the lua node still stores the stale token"
        );
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.alive(successor))).expect("successor node")
        );
        assert_eq!(host.resource_count(), 4);
        assert_eq!(heap::stats(&mut host).expect("before").live_nodes, 4);

        let report =
            heap::collect(&mut host).expect("a stale stored token must not stop collection");
        assert_eq!(report.live_nodes, 2, "rooted table and successor stay");
        assert_eq!(
            host.resource_count(),
            3,
            "only the other doomed token is closed; the successor keeps its quota"
        );
        assert_eq!(
            table_ref(&host, successor_raw).slot(),
            successor.slot(),
            "the successor token is still the live occupant of the reused slot"
        );
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(successor))).expect("successor"));
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(rooted))).expect("rooted"));
        assert!(
            host.typed_resource_with_key::<LuaTableResource>(
                other_handle,
                &LuaTableResource::key()
            )
            .is_err(),
            "the other unreachable table is collected"
        );
        assert!(
            host.resource(&unrelated).is_ok(),
            "unrelated resource stays"
        );
        assert!(
            host.scope_state::<heap::LuaHeap>().is_some(),
            "collection must not replace the scope with reset"
        );

        let again = heap::collect(&mut host).expect("a later collection must also finish");
        assert_eq!(again.live_nodes, 2);
        assert_eq!(
            host.resource_count(),
            3,
            "the second collection must not release quota again"
        );
        assert_eq!(heap::stats(&mut host).expect("after").collections, 2);
        assert_eq!(
            table_ref(&host, successor_raw).generation(),
            successor.generation()
        );
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(rooted))).expect("root remains"));
        assert!(
            host.resource(&unrelated).is_ok(),
            "unrelated resource remains"
        );
    });
}

fn publish_raw(host: &mut HostContext<'_>) -> i64 {
    match heap::publish(host).expect("publish") {
        Value::Int(raw) => raw,
        other => panic!("publish did not return a handle: {other:?}"),
    }
}

fn resource_handle(raw: i64) -> ResourceHandle {
    ResourceHandle::from_raw(u64::try_from(raw).expect("positive handle")).expect("handle")
}

fn table_ref(host: &HostContext<'_>, raw: i64) -> TableRef {
    let handle = resource_handle(raw);
    let token = host
        .typed_resource_with_key::<LuaTableResource>(handle, &LuaTableResource::key())
        .expect("typed token");
    host.resource(&token).expect("live token").get().table()
}

/// A host resource that is not a Lua table. Collection must not close it.
struct UnrelatedHostResource;

impl vm::resource::HostResource for UnrelatedHostResource {}

#[test]
fn collect_returns_token_quota_and_republish_exceeds_the_core_limit() {
    with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(
            &mut host,
            HeapLimits {
                nodes: 64,
                roots: 4,
                edges: 4,
                max_generation: 64,
            },
        )
        .expect("install");
        let unrelated = host
            .push_resource(UnrelatedHostResource)
            .expect("unrelated host resource");
        let rooted_raw = publish_raw(&mut host);
        let rooted = table_ref(&host, rooted_raw);
        let frame = heap::with_heap(&mut host, |heap| heap.enter_roots(1)).expect("frame");
        heap::with_heap(&mut host, |heap| heap.write_root(frame, 0, rooted)).expect("root");

        let mut published = 1usize;
        let mut last_doomed = 0i64;
        let batch = 32usize;
        while published <= vm::resource::handle::DEFAULT_MAX_RESOURCES {
            for _ in 0..batch {
                last_doomed = publish_raw(&mut host);
                published = published.saturating_add(1);
            }
            let before_nodes = heap::stats(&mut host).expect("before nodes").live_nodes;
            let before_tokens = host.resource_count();
            let report = heap::collect(&mut host).expect("collect");
            let after = heap::stats(&mut host).expect("after nodes");
            assert!(
                before_nodes > after.live_nodes,
                "collect must sweep unrooted tables in this same VM"
            );
            assert_eq!(after.live_nodes, 1, "the rooted table stays");
            assert_eq!(report.live_nodes, 1);
            assert_eq!(
                host.resource_count(),
                2,
                "swept tokens must retire and return quota; before_tokens={before_tokens}"
            );
            assert!(
                before_tokens > host.resource_count(),
                "live tokens must drop with the swept tables"
            );
            assert!(
                heap::with_heap(&mut host, |heap| Ok(heap.alive(rooted))).expect("rooted node"),
                "the rooted node stays live"
            );
            let rooted_token = resource_handle(rooted_raw);
            assert!(
                host.typed_resource_with_key::<LuaTableResource>(
                    rooted_token,
                    &LuaTableResource::key(),
                )
                .is_ok(),
                "the rooted token stays valid"
            );
            let doomed = resource_handle(last_doomed);
            assert!(
                host.typed_resource_with_key::<LuaTableResource>(doomed, &LuaTableResource::key())
                    .is_err(),
                "a collected token must be invalid without leaving the VM"
            );
            assert!(
                host.resource(&unrelated).is_ok(),
                "an unrelated host resource is not a collected table"
            );
        }
        assert!(
            published > vm::resource::handle::DEFAULT_MAX_RESOURCES,
            "cumulative publish, collect, and republish must pass the core limit"
        );
        let _again = publish_raw(&mut host);
    });
}

#[test]
fn reset_for_reuse_rejects_old_handles_and_starts_an_empty_heap() {
    with_idle_vm(|vm| {
        let raw = {
            let mut host = vm.host_context();
            heap::install(
                &mut host,
                HeapLimits {
                    nodes: 8,
                    roots: 2,
                    edges: 2,
                    max_generation: 4,
                },
            )
            .expect("install");
            let raw = publish_raw(&mut host);
            let table = table_ref(&host, raw);
            let frame = heap::with_heap(&mut host, |heap| heap.enter_roots(1)).expect("frame");
            heap::with_heap(&mut host, |heap| heap.write_root(frame, 0, table)).expect("root");
            let before = heap::stats(&mut host).expect("before reset");
            assert!(before.live_nodes > 0);
            assert!(before.live_roots > 0);
            raw
        };
        vm.reset_for_reuse().expect("start reset");
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        match vm.poll_reset_for_reuse(&mut context) {
            std::task::Poll::Ready(Ok(())) => {}
            other => panic!("reset did not finish: {other:?}"),
        }
        let mut host = vm.host_context();
        let rejected = host.typed_resource_with_key::<LuaTableResource>(
            resource_handle(raw),
            &LuaTableResource::key(),
        );
        assert!(
            rejected.is_err(),
            "old handle must be rejected: {rejected:?}"
        );
        assert!(
            host.scope_state::<heap::LuaHeap>().is_none(),
            "the replacement scope starts without a heap"
        );
        assert_eq!(
            host.resource_count(),
            0,
            "the new scope has no leftover tokens"
        );
        heap::install(
            &mut host,
            HeapLimits {
                nodes: 8,
                roots: 2,
                edges: 2,
                max_generation: 4,
            },
        )
        .expect("reinstall");
        let empty = heap::stats(&mut host).expect("empty heap");
        assert_eq!(empty.live_nodes, 0);
        assert_eq!(empty.live_roots, 0);
        let _again = publish_raw(&mut host);
    });
}

struct ScanShape {
    name: &'static str,
    nodes: u32,
    roots: u32,
    edges: u32,
    reachable: u32,
    edges_in_live: bool,
}

fn scan_shape(
    name: &'static str,
    nodes: u32,
    roots: u32,
    edges: u32,
    reachable: u32,
    edges_in_live: bool,
) -> ScanShape {
    ScanShape {
        name,
        nodes,
        roots,
        edges,
        reachable,
        edges_in_live,
    }
}

fn plan_scan_edges(shape: &ScanShape) -> Vec<(u32, u32, u32)> {
    let mut next_key = vec![1u32; shape.nodes as usize];
    let mut edges = Vec::with_capacity(shape.edges as usize);
    if shape.edges_in_live {
        let spanning = shape.reachable - shape.roots;
        assert!(
            shape.edges >= spanning,
            "{} needs {spanning} spanning edges",
            shape.name
        );
        for index in shape.roots..shape.reachable {
            let from = index - 1;
            let key = next_key[from as usize];
            next_key[from as usize] = key + 1;
            edges.push((from, key, index));
        }
        let mut round = 0u32;
        while edges.len() < shape.edges as usize {
            let from = round % shape.reachable;
            let to = (round + 1) % shape.reachable;
            let key = next_key[from as usize];
            next_key[from as usize] = key + 1;
            edges.push((from, key, to));
            round += 1;
            assert!(
                round <= shape.edges + shape.reachable + 4,
                "{} live plan stalled",
                shape.name
            );
        }
    } else {
        let dead = shape.nodes - shape.reachable;
        assert!(dead > 0, "{} dead-home needs a dead node", shape.name);
        let mut round = 0u32;
        while edges.len() < shape.edges as usize {
            let from = shape.reachable + (round % dead);
            let to = shape.reachable + ((round + 1) % dead);
            let key = next_key[from as usize];
            next_key[from as usize] = key + 1;
            edges.push((from, key, to));
            round += 1;
            assert!(
                round <= shape.edges + dead + 4,
                "{} dead plan stalled",
                shape.name
            );
        }
    }
    assert_eq!(
        edges.len(),
        shape.edges as usize,
        "{} edge plan",
        shape.name
    );
    edges
}

fn reachable_outgoing(shape: &ScanShape, plan: &[(u32, u32, u32)]) -> u64 {
    plan.iter()
        .filter(|(from, _, _)| *from < shape.reachable)
        .count() as u64
}

struct LinearMeterExpect {
    slots: u64,
    roots: u64,
    occupied_roots: u64,
    edges: u64,
    outgoing: u64,
    dead: u64,
    marked: u64,
    doomed_tokens: u64,
    enumerates_doomed: bool,
}

/// The meter is the observation. Graph size is only the budget, not a stand-in
/// for the counts. Edge fields stay separate from node, root, and scratch
/// requests. Scratch lengths are measured requests, not allocator capacity,
/// RSS, Core close work, or wall time.
fn assert_linear_meter(label: &str, meter: &heap::ScanMeter, expect: LinearMeterExpect) {
    let LinearMeterExpect {
        slots,
        roots,
        occupied_roots,
        edges,
        outgoing,
        dead,
        marked,
        doomed_tokens,
        enumerates_doomed,
    } = expect;
    assert_eq!(
        meter.mark_passes,
        1,
        "{label} marks once (outgoing={} budget={outgoing} sweep={} token_slots={} closes={} index_edges={} node_slots={} root_slots={})",
        meter.outgoing_edge_checks,
        meter.sweep_edge_checks,
        meter.token_slot_checks,
        meter.token_closes,
        meter.index_edge_checks,
        meter.node_slot_visits,
        meter.root_slot_visits
    );
    assert_eq!(
        meter.outgoing_edge_checks, outgoing,
        "{label} outgoing checks must be the visited edges; a full walk of reachable*edges exceeds this budget of {outgoing} (mark_passes={} sweep={} token_slots={})",
        meter.mark_passes, meter.sweep_edge_checks, meter.token_slot_checks
    );
    assert_eq!(
        meter.index_edge_checks,
        edges.saturating_mul(2),
        "{label} index build examines each stored edge twice"
    );
    assert_eq!(
        meter.index_slot_checks, slots,
        "{label} index build examines the counts scratch once"
    );
    let sweep_budget = if dead == 0 { 0 } else { edges };
    assert_eq!(
        meter.sweep_edge_checks, sweep_budget,
        "{label} sweep filters the edge vector once"
    );
    assert_eq!(meter.token_closes, doomed_tokens, "{label} closes");
    assert_eq!(
        meter.token_slot_checks,
        doomed_tokens.saturating_mul(2),
        "{label} token slot checks are two per close"
    );
    // These graphs store only live targets, so each outgoing edge reads the
    // target slot and then updates it. Occupied roots are live at mark time.
    let mut node_visits = slots + occupied_roots.saturating_mul(2) + outgoing.saturating_mul(2);
    if enumerates_doomed {
        node_visits += slots;
    }
    node_visits += slots;
    if dead > 0 {
        node_visits += slots + dead;
    }
    node_visits += occupied_roots;
    assert_eq!(
        meter.node_slot_visits, node_visits,
        "{label} node slot visits"
    );
    assert_eq!(
        meter.root_slot_visits,
        roots.saturating_mul(2),
        "{label} root slot visits"
    );
    let scratch = &meter.scratch;
    assert_eq!(
        scratch.index_counts_inited, slots,
        "{label} counts init is the measured request, not allocator capacity"
    );
    assert_eq!(
        scratch.index_offsets_reserved,
        slots + 1,
        "{label} offsets reserve"
    );
    assert_eq!(
        scratch.index_offsets_pushed,
        slots + 1,
        "{label} offsets pushed"
    );
    assert_eq!(
        scratch.index_cursor_copied,
        slots + 1,
        "{label} cursor copy"
    );
    assert_eq!(
        scratch.index_targets_inited, edges,
        "{label} targets init is the measured in-range edge total"
    );
    assert_eq!(scratch.work_pushed, marked, "{label} work pushes");
    let pending = if enumerates_doomed { doomed_tokens } else { 0 };
    assert_eq!(
        scratch.pending_close_pushed, pending,
        "{label} pending close"
    );
    assert_eq!(scratch.dead_slots_pushed, dead, "{label} dead slots");
    let bitmap = if dead == 0 { 0 } else { slots };
    assert_eq!(
        scratch.sweep_bitmap_inited, bitmap,
        "{label} bitmap init is skipped when nothing is dead"
    );
}

fn eprintln_meter(label: &str, meter: &heap::ScanMeter) {
    let scratch = &meter.scratch;
    eprintln!(
        "meter {label} mark={} index_edges={} index_slots={} outgoing={} sweep={} token_slots={} closes={} node_slots={} root_slots={} counts={} offsets_res={} offsets_pushed={} cursor={} targets={} work={} pending={} dead={} bitmap={}",
        meter.mark_passes,
        meter.index_edge_checks,
        meter.index_slot_checks,
        meter.outgoing_edge_checks,
        meter.sweep_edge_checks,
        meter.token_slot_checks,
        meter.token_closes,
        meter.node_slot_visits,
        meter.root_slot_visits,
        scratch.index_counts_inited,
        scratch.index_offsets_reserved,
        scratch.index_offsets_pushed,
        scratch.index_cursor_copied,
        scratch.index_targets_inited,
        scratch.work_pushed,
        scratch.pending_close_pushed,
        scratch.dead_slots_pushed,
        scratch.sweep_bitmap_inited
    );
}

fn assert_pure_kept(
    heap: &LuaHeap,
    nodes: &[TableRef],
    shape: &ScanShape,
    plan: &[(u32, u32, u32)],
) {
    assert_eq!(
        heap.stats().live_nodes,
        shape.reachable,
        "{} live nodes",
        shape.name
    );
    assert_eq!(
        heap.stats().live_roots,
        shape.roots,
        "{} live roots",
        shape.name
    );
    for index in 0..shape.reachable {
        assert!(
            heap.alive(nodes[index as usize]),
            "{} root region stays",
            shape.name
        );
    }
    for index in shape.reachable..shape.nodes {
        assert!(
            !heap.alive(nodes[index as usize]),
            "{} dead region is collected",
            shape.name
        );
    }
    if let Some((from, key, to)) = plan.iter().find(|(from, _, _)| *from < shape.reachable) {
        assert_eq!(
            heap.edge(nodes[*from as usize], *key).expect("kept edge"),
            Some(nodes[*to as usize]),
            "{} kept edge",
            shape.name
        );
    }
}

fn collect_pure_shape(shape: &ScanShape) {
    let mut heap = LuaHeap::new(HeapLimits::production());
    let mut nodes = Vec::with_capacity(shape.nodes as usize);
    for _ in 0..shape.nodes {
        nodes.push(
            heap.alloc()
                .unwrap_or_else(|error| panic!("{} alloc: {error}", shape.name)),
        );
    }
    let frame = heap
        .enter_roots(shape.roots)
        .unwrap_or_else(|error| panic!("{} roots: {error}", shape.name));
    for index in 0..shape.roots {
        heap.write_root(frame, index, nodes[index as usize])
            .unwrap_or_else(|error| panic!("{} root: {error}", shape.name));
    }
    let plan = plan_scan_edges(shape);
    for (from, key, to) in &plan {
        heap.set_edge(nodes[*from as usize], *key, nodes[*to as usize])
            .unwrap_or_else(|error| panic!("{} edge: {error}", shape.name));
    }
    let before = heap.stats();
    let started = std::time::Instant::now();
    let report = heap.collect();
    if std::env::var_os("LUA_HEAP_TIME_COLLECT").is_some() {
        eprintln!("time {} nanos={}", shape.name, started.elapsed().as_nanos());
    }
    let meter = heap.take_scan_meter();
    eprintln_meter(shape.name, &meter);
    assert_eq!(report.live_nodes, shape.reachable, "{}", shape.name);
    assert_eq!(report.marked_nodes, shape.reachable, "{}", shape.name);
    assert_pure_kept(&heap, &nodes, shape, &plan);
    assert_linear_meter(
        shape.name,
        &meter,
        LinearMeterExpect {
            slots: u64::from(before.node_slots),
            roots: u64::from(before.root_slots),
            occupied_roots: u64::from(before.live_roots),
            edges: u64::from(before.live_edges),
            outgoing: reachable_outgoing(shape, &plan),
            dead: u64::from(shape.nodes - shape.reachable),
            marked: u64::from(shape.reachable),
            doomed_tokens: 0,
            enumerates_doomed: false,
        },
    );
}

fn collect_host_shape(shape: &ScanShape, share_resource: bool) {
    let token_cap = vm::resource::handle::DEFAULT_MAX_RESOURCES;
    let published = if share_resource {
        shape.nodes as usize + 1
    } else {
        shape.nodes as usize
    };
    assert!(
        published <= token_cap,
        "{} uses {published} resources, cap is {token_cap}",
        shape.name
    );
    with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, HeapLimits::production()).expect("install");
        let unrelated = if share_resource {
            Some(
                host.push_resource(UnrelatedHostResource)
                    .expect("shared resource"),
            )
        } else {
            None
        };
        let mut nodes = Vec::with_capacity(shape.nodes as usize);
        let mut raws = Vec::with_capacity(shape.nodes as usize);
        for _ in 0..shape.nodes {
            let raw = publish_raw(&mut host);
            nodes.push(table_ref(&host, raw));
            raws.push(raw);
        }
        let frame = heap::with_heap(&mut host, |heap| heap.enter_roots(shape.roots))
            .unwrap_or_else(|error| panic!("{} roots: {error}", shape.name));
        for index in 0..shape.roots {
            let handle = nodes[index as usize];
            heap::with_heap(&mut host, |heap| heap.write_root(frame, index, handle))
                .unwrap_or_else(|error| panic!("{} root: {error}", shape.name));
        }
        let plan = plan_scan_edges(shape);
        for (from, key, to) in &plan {
            let source = nodes[*from as usize];
            let target = nodes[*to as usize];
            heap::with_heap(&mut host, |heap| heap.set_edge(source, *key, target))
                .unwrap_or_else(|error| panic!("{} edge: {error}", shape.name));
        }
        let before = heap::stats(&mut host).expect("before");
        let resources_before = host.resource_count();
        let started = std::time::Instant::now();
        let report = heap::collect(&mut host)
            .unwrap_or_else(|error| panic!("{} collect: {error}", shape.name));
        if std::env::var_os("LUA_HEAP_TIME_COLLECT").is_some() {
            eprintln!("time {} nanos={}", shape.name, started.elapsed().as_nanos());
        }
        let resources_after = host.resource_count();
        let meter = heap::take_scan_meter(&mut host).expect("meter");
        eprintln_meter(shape.name, &meter);
        let shared = usize::from(share_resource);
        assert_eq!(report.live_nodes, shape.reachable, "{}", shape.name);
        assert_eq!(report.marked_nodes, shape.reachable, "{}", shape.name);
        assert_eq!(
            resources_after,
            shape.reachable as usize + shared,
            "{} quota",
            shape.name
        );
        if shape.reachable < shape.nodes {
            assert!(
                resources_before > resources_after,
                "{} tokens drop",
                shape.name
            );
        } else {
            assert_eq!(
                resources_before, resources_after,
                "{} keeps every token",
                shape.name
            );
        }
        for index in 0..shape.reachable {
            assert!(
                host.typed_resource_with_key::<LuaTableResource>(
                    resource_handle(raws[index as usize]),
                    &LuaTableResource::key(),
                )
                .is_ok(),
                "{} rooted token stays",
                shape.name
            );
        }
        if shape.nodes > shape.reachable {
            let doomed = resource_handle(raws[shape.reachable as usize]);
            assert!(
                host.typed_resource_with_key::<LuaTableResource>(doomed, &LuaTableResource::key())
                    .is_err(),
                "{} collected token is invalid",
                shape.name
            );
        }
        if let Some(unrelated) = unrelated {
            assert!(
                host.resource(&unrelated).is_ok(),
                "{} shared resource stays",
                shape.name
            );
        }
        heap::with_heap(&mut host, |heap| {
            assert_pure_kept(heap, &nodes, shape, &plan);
            Ok(())
        })
        .expect("kept");
        assert_linear_meter(
            shape.name,
            &meter,
            LinearMeterExpect {
                slots: u64::from(before.node_slots),
                roots: u64::from(before.root_slots),
                occupied_roots: u64::from(before.live_roots),
                edges: u64::from(before.live_edges),
                outgoing: reachable_outgoing(shape, &plan),
                dead: u64::from(shape.nodes - shape.reachable),
                marked: u64::from(shape.reachable),
                doomed_tokens: u64::from(shape.nodes - shape.reachable),
                enumerates_doomed: true,
            },
        );
    });
}

/// One small pure collect accounts node slots, root slots, and scratch
/// requests separately from the index's one scratch pass.
#[test]
fn collect_meter_accounts_node_root_and_scratch_work() {
    collect_pure_shape(&scan_shape("account-small", 4, 1, 1, 2, true));
}

#[test]
fn collect_scan_small_graph_stays_inside_one_linear_pass() {
    let pure = scan_shape("small-survival", 8, 2, 12, 4, true);
    collect_pure_shape(&pure);
    let host = scan_shape("small-host-death", 8, 1, 12, 2, true);
    collect_host_shape(&host, true);
}

#[test]
fn collect_scan_pure_limit_graphs_stay_linear() {
    let limits = HeapLimits::production();
    collect_pure_shape(&scan_shape(
        "limit-survival-all",
        limits.nodes,
        limits.roots,
        limits.edges,
        limits.nodes,
        true,
    ));
    collect_pure_shape(&scan_shape(
        "limit-death-live-edges",
        limits.nodes,
        1,
        limits.edges,
        16,
        true,
    ));
    collect_pure_shape(&scan_shape(
        "limit-death-dead-edges",
        limits.nodes,
        1,
        limits.edges,
        1,
        false,
    ));
}

#[test]
fn collect_scan_host_limit_graphs_stay_linear() {
    let token_cap = u32::try_from(vm::resource::handle::DEFAULT_MAX_RESOURCES).expect("cap");
    let edges = HeapLimits::production().edges;
    collect_host_shape(
        &scan_shape(
            "token-cap-survival-all",
            token_cap,
            256,
            edges,
            token_cap,
            true,
        ),
        false,
    );
    collect_host_shape(
        &scan_shape("token-cap-death-live-edges", token_cap, 1, edges, 4, true),
        false,
    );
    collect_host_shape(
        &scan_shape(
            "token-cap-shared-death-dead-edges",
            token_cap - 1,
            1,
            edges,
            1,
            false,
        ),
        true,
    );
}

/// AlreadyClosed and Stale still retire only this heap's exact token. The
/// lookup is the identified slot, not a scan of every slot.
#[test]
fn collect_scan_retired_tokens_check_the_identified_slot() {
    with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, sweep_limits()).expect("install");
        let unrelated = host
            .push_resource(UnrelatedHostResource)
            .expect("unrelated");
        let rooted_raw = publish_raw(&mut host);
        let rooted = table_ref(&host, rooted_raw);
        root_table(&mut host, rooted);
        let mut filler = Vec::new();
        for _ in 0..6 {
            filler.push(publish_raw(&mut host));
        }
        let closed_raw = filler[0];
        let closed = table_ref(&host, closed_raw);
        let closed_handle = resource_handle(closed_raw);
        host.close_resource::<LuaTableResource>(closed_handle, vm::ResourceCloseReason::Requested)
            .expect("external close");
        let stale_raw = filler[1];
        let stale = table_ref(&host, stale_raw);
        let stale_handle = resource_handle(stale_raw);
        let _taken = host
            .take_resource::<LuaTableResource>(stale_handle)
            .expect("take");
        let successor_raw = publish_raw(&mut host);
        let successor = table_ref(&host, successor_raw);
        root_table(&mut host, successor);
        assert_eq!(
            stale_handle.slot_index().expect("stale slot"),
            resource_handle(successor_raw)
                .slot_index()
                .expect("successor slot")
        );

        let before = heap::stats(&mut host).expect("slots");
        let before_slots = before.node_slots;
        let report = heap::collect(&mut host).expect("retired tokens do not stop collection");
        let meter = heap::take_scan_meter(&mut host).expect("meter");
        eprintln_meter("retired-slot", &meter);
        eprintln!("meter retired-slot slots={before_slots}");
        assert!(
            before_slots > 3,
            "the slot vector must be wider than a direct lookup"
        );
        assert_eq!(report.live_nodes, 2, "rooted table and successor stay");
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(rooted))).expect("rooted"));
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(successor))).expect("successor"));
        assert!(!heap::with_heap(&mut host, |heap| Ok(heap.alive(closed))).expect("closed node"));
        assert!(!heap::with_heap(&mut host, |heap| Ok(heap.alive(stale))).expect("stale node"));
        assert!(
            host.resource(&unrelated).is_ok(),
            "unrelated resource stays"
        );
        assert!(
            host.typed_resource_with_key::<LuaTableResource>(
                resource_handle(successor_raw),
                &LuaTableResource::key(),
            )
            .is_ok(),
            "successor token stays usable"
        );
        assert_eq!(
            meter.token_slot_checks,
            meter.token_closes.saturating_mul(2),
            "retired close checks the identified slot twice per close"
        );
        assert_eq!(meter.mark_passes, 1, "retired close still marks once");
        assert_linear_meter(
            "retired-slot",
            &meter,
            LinearMeterExpect {
                slots: u64::from(before.node_slots),
                roots: u64::from(before.root_slots),
                occupied_roots: u64::from(before.live_roots),
                edges: 0,
                outgoing: 0,
                dead: u64::from(before.live_nodes - report.live_nodes),
                marked: u64::from(report.marked_nodes),
                doomed_tokens: meter.token_closes,
                enumerates_doomed: true,
            },
        );
    });
}

/// A non-retired close error keeps the closed prefix, leaves the later token
/// open, and does not sweep. Another collect marks from the current roots and
/// fails again while the foreign token remains. Unbinding that token is a
/// test fixture, not a recovery path in the collector.
#[test]
fn collect_scan_failed_close_retries_after_remark() {
    with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, sweep_limits()).expect("install");
        let unrelated = host
            .push_resource(UnrelatedHostResource)
            .expect("unrelated");
        let unrelated_raw = unrelated.handle().raw();
        let keeper_raw = publish_raw(&mut host);
        let keeper = table_ref(&host, keeper_raw);
        root_table(&mut host, keeper);
        let prefix_raw = publish_raw(&mut host);
        let prefix = table_ref(&host, prefix_raw);
        let other_raw = publish_raw(&mut host);
        let other = table_ref(&host, other_raw);
        let doomed = heap::with_heap(&mut host, |heap| {
            let node = heap.alloc()?;
            heap.bind_token(node, unrelated_raw)?;
            Ok(node)
        })
        .expect("bind a foreign token");
        let suffix_raw = publish_raw(&mut host);
        let suffix = table_ref(&host, suffix_raw);
        let before_nodes = heap::stats(&mut host).expect("before").live_nodes;
        let before_resources = host.resource_count();

        let error = heap::collect(&mut host).expect_err("type mismatch must propagate");
        assert_type_mismatch(&error);
        let failed = heap::take_scan_meter(&mut host).expect("failed meter");
        eprintln_meter("failed-close", &failed);
        assert_eq!(
            heap::stats(&mut host).expect("after error").live_nodes,
            before_nodes
        );
        assert_eq!(heap::stats(&mut host).expect("collections").collections, 0);
        assert_eq!(
            failed.sweep_edge_checks, 0,
            "an incomplete collect does not sweep"
        );
        assert_eq!(failed.mark_passes, 1);
        assert_eq!(
            failed.token_closes, 3,
            "prefix, other, and the foreign attempt"
        );
        assert_eq!(
            failed.token_slot_checks, 4,
            "the two successful closes check twice; the failing close does not"
        );
        assert_eq!(
            failed.node_slot_visits, 12,
            "reset, root checks, and doomed enum"
        );
        assert_eq!(
            failed.root_slot_visits, 1,
            "mark enumerates roots; sweep does not"
        );
        assert_eq!(failed.scratch.pending_close_pushed, 4);
        assert_eq!(failed.scratch.dead_slots_pushed, 0);
        assert_eq!(failed.scratch.sweep_bitmap_inited, 0);
        assert!(
            host.resource(&unrelated).is_ok(),
            "the foreign resource is not closed"
        );
        assert!(
            host.typed_resource_with_key::<LuaTableResource>(
                resource_handle(prefix_raw),
                &LuaTableResource::key(),
            )
            .is_err(),
            "the successful prefix stays closed"
        );
        assert!(
            host.typed_resource_with_key::<LuaTableResource>(
                resource_handle(other_raw),
                &LuaTableResource::key(),
            )
            .is_err(),
            "the second prefix token stays closed"
        );
        assert!(
            host.typed_resource_with_key::<LuaTableResource>(
                resource_handle(suffix_raw),
                &LuaTableResource::key(),
            )
            .is_ok(),
            "the suffix after the failing token stays open"
        );
        let resources_after_failure = host.resource_count();
        assert_eq!(resources_after_failure, before_resources - 2);
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(prefix))).expect("prefix node"));
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(doomed))).expect("doomed node"));
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(suffix))).expect("suffix node"));

        let again = heap::collect(&mut host).expect_err("the same obstacle fails again");
        assert_type_mismatch(&again);
        let again_meter = heap::take_scan_meter(&mut host).expect("second failure meter");
        eprintln_meter("failed-close-again", &again_meter);
        assert_eq!(
            again_meter.mark_passes, 1,
            "the later collect marks from the current roots"
        );
        assert_eq!(
            again_meter.sweep_edge_checks, 0,
            "the later collect still does not sweep"
        );
        assert_eq!(
            again_meter.token_closes, 1,
            "only the foreign token is attempted"
        );
        assert_eq!(
            again_meter.token_slot_checks, 0,
            "the failing close does not check a slot or advance the suffix"
        );
        assert_eq!(
            again_meter.node_slot_visits, 12,
            "the later mark walks the same node slots and does not sweep"
        );
        assert_eq!(
            again_meter.root_slot_visits, 1,
            "the later mark reads the current roots and does not sweep them"
        );
        assert_eq!(
            again_meter.scratch.pending_close_pushed, 2,
            "foreign token and suffix"
        );
        assert_eq!(again_meter.scratch.dead_slots_pushed, 0);
        assert_eq!(
            host.resource_count(),
            resources_after_failure,
            "the closed prefix quota is not returned again"
        );
        assert_eq!(
            heap::stats(&mut host).expect("still unswept").collections,
            0
        );
        assert!(
            host.typed_resource_with_key::<LuaTableResource>(
                resource_handle(suffix_raw),
                &LuaTableResource::key(),
            )
            .is_ok(),
            "the unprocessed suffix stays open"
        );
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(suffix))).expect("suffix stays"));
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(other))).expect("other stays"));
        assert!(
            host.resource(&unrelated).is_ok(),
            "the foreign resource stays open"
        );

        // Test fixture only. The collector does not unbind an obstacle or
        // resume a remainder. This removes the foreign binding so the next
        // collect can finish; it is not a production recovery path.
        heap::with_heap(&mut host, |heap| heap.unbind_token(doomed))
            .expect("fixture drops the foreign token binding");
        root_table(&mut host, prefix);
        let report = heap::collect(&mut host)
            .expect("collect finishes after the fixture removes the foreign binding");
        let retry = heap::take_scan_meter(&mut host).expect("fixture meter");
        eprintln_meter("fixture-after-unbind", &retry);
        assert_eq!(retry.mark_passes, 1, "the fixture collect marks again");
        assert_eq!(
            retry.token_closes, 1,
            "only the still-open suffix is closed"
        );
        assert_eq!(
            retry.token_slot_checks,
            retry.token_closes.saturating_mul(2),
            "the suffix close checks its slot twice"
        );
        assert_eq!(
            report.live_nodes, 2,
            "keeper and the newly rooted prefix stay"
        );
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(keeper))).expect("keeper"));
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.alive(prefix))).expect("remarked prefix")
        );
        assert!(!heap::with_heap(&mut host, |heap| Ok(heap.alive(other))).expect("other swept"));
        assert!(!heap::with_heap(&mut host, |heap| Ok(heap.alive(doomed))).expect("doomed swept"));
        assert!(!heap::with_heap(&mut host, |heap| Ok(heap.alive(suffix))).expect("suffix swept"));
        assert!(
            host.typed_resource_with_key::<LuaTableResource>(
                resource_handle(suffix_raw),
                &LuaTableResource::key(),
            )
            .is_err(),
            "the suffix closes only after the fixture removes the obstacle"
        );
        assert_eq!(host.resource_count(), resources_after_failure - 1);
        assert!(host.resource(&unrelated).is_ok(), "shared resource remains");
        assert!(
            host.typed_resource_with_key::<LuaTableResource>(
                resource_handle(keeper_raw),
                &LuaTableResource::key(),
            )
            .is_ok()
        );
    });
}

fn assert_type_mismatch(error: &heap::HeapError) {
    match error {
        heap::HeapError::Scope(message) => {
            assert!(
                message.contains("resource_type_mismatch"),
                "other close errors propagate, got {message}"
            );
        }
        other => panic!("expected a scope close error, got {other:?}"),
    }
}

#[test]
fn shared_metatable_sweep_reuses_marked_modes() {
    let mode = "xxv";
    let narrow = collect_shared_mode(4, 16, mode);
    let more_owners = collect_shared_mode(16, 16, mode);
    let more_edges = collect_shared_mode(4, 64, mode);
    let strong = collect_shared_mode(16, 16, "s");

    assert_eq!(narrow.bytes, mode.len() as u64, "mark parses the mode once");
    assert_eq!(more_owners.bytes, narrow.bytes);
    assert_eq!(more_edges.bytes, narrow.bytes);
    assert_eq!(strong.bytes, 1, "a non-weak mode is still parsed once");

    assert_eq!(narrow.lookups, u64::from(narrow.owners));
    assert_eq!(
        more_owners.lookups / narrow.lookups,
        u64::from(more_owners.owners / narrow.owners),
        "lookups grow with owners, not with owners times edges"
    );
    assert_eq!(more_edges.lookups, narrow.lookups);
    assert_eq!(strong.lookups, 0);
    assert_eq!(strong.modes_inited, 0);

    assert_eq!(narrow.sweep, u64::from(narrow.edges));
    assert_eq!(
        more_owners.sweep, narrow.sweep,
        "extra owners do not add an edge sweep"
    );
    assert_eq!(
        more_edges.sweep / narrow.sweep,
        u64::from(more_edges.edges / narrow.edges)
    );
    assert_eq!(strong.sweep, u64::from(strong.edges));
    assert_eq!(narrow.index, narrow.sweep * 2);
    assert_eq!(more_owners.index, narrow.index);
    assert_eq!(
        more_edges.index / narrow.index,
        more_edges.sweep / narrow.sweep
    );
    assert_eq!(narrow.outgoing, narrow.sweep);
    assert_eq!(more_owners.outgoing, narrow.outgoing);
    assert_eq!(more_edges.outgoing, more_edges.sweep);

    assert_eq!(narrow.modes_inited, u64::from(narrow.nodes));
    assert_eq!(more_edges.modes_inited, narrow.modes_inited);
    assert!(more_owners.modes_inited > narrow.modes_inited);

    assert_cleared_cursor_reuses_the_dead_slot();
    assert_host_collect_keeps_the_marked_mode();
}

struct ModeSweep {
    owners: u32,
    edges: u32,
    bytes: u64,
    lookups: u64,
    sweep: u64,
    index: u64,
    outgoing: u64,
    modes_inited: u64,
    nodes: u32,
}

/// One shared metatable, `owners` marked tables, `edges` stored records, and
/// one dead node so sweep runs. Padding fields are written first, so the mode
/// string is the last edge.
fn collect_shared_mode(owners: u32, edges: u32, mode: &str) -> ModeSweep {
    let mut heap = LuaHeap::new(HeapLimits {
        nodes: owners + 2,
        roots: owners + 1,
        edges,
        max_generation: 8,
    });
    let mt = heap.alloc().unwrap();
    let mut owner_refs = Vec::new();
    for _ in 0..owners {
        owner_refs.push(heap.alloc().unwrap());
    }
    let _dead = heap.alloc().unwrap();
    let frame = heap.enter_roots(owners + 1).unwrap();
    heap.write_root(frame, 0, mt).unwrap();
    for (index, owner) in owner_refs.iter().enumerate() {
        heap.write_root(frame, index as u32 + 1, *owner).unwrap();
        heap.set_metatable(*owner, Some(mt)).unwrap();
    }
    for field in 1..edges {
        heap.set_field_scalar(mt, field, Value::string("pad"))
            .unwrap();
    }
    heap.set_mode_scalar(mt, Value::string(mode)).unwrap();
    assert_eq!(
        heap.stats().edge_slots,
        edges,
        "mode is the last stored edge"
    );
    heap.collect();
    let meter = heap.take_scan_meter();
    ModeSweep {
        owners,
        edges,
        bytes: meter.weak_mode_bytes,
        lookups: meter.weak_mode_lookups,
        sweep: meter.sweep_edge_checks,
        index: meter.index_edge_checks,
        outgoing: meter.outgoing_edge_checks,
        modes_inited: meter.scratch.weak_modes_inited,
        nodes: heap.stats().node_slots,
    }
}

fn assert_cleared_cursor_reuses_the_dead_slot() {
    let mut heap = LuaHeap::new(HeapLimits {
        nodes: 8,
        roots: 4,
        edges: 4,
        max_generation: 8,
    });
    let mt = heap.alloc().unwrap();
    let owner = heap.alloc().unwrap();
    let key = heap.alloc().unwrap();
    let kept = heap.alloc().unwrap();
    let dead = heap.alloc().unwrap();
    let frame = heap.enter_roots(4).unwrap();
    heap.write_root(frame, 0, mt).unwrap();
    heap.write_root(frame, 1, owner).unwrap();
    heap.write_root(frame, 2, key).unwrap();
    heap.write_root(frame, 3, kept).unwrap();
    heap.set_metatable(owner, Some(mt)).unwrap();
    heap.set_object_edge(owner, key, dead).unwrap();
    heap.set_named_edge(owner, "keep", kept).unwrap();
    heap.set_mode_scalar(mt, Value::string("v")).unwrap();
    heap.issue_carrier(kept).unwrap();
    assert_eq!(heap.stats().edge_slots, 3);
    assert_eq!(heap.stats().live_edges, 3);

    heap.collect();
    let meter = heap.take_scan_meter();
    assert_eq!(meter.weak_mode_bytes, 1);
    assert_eq!(meter.weak_mode_lookups, 1);
    assert_eq!(
        meter.sweep_edge_checks, 3,
        "one pass over the stored records"
    );
    assert_eq!(meter.index_edge_checks, 6);
    assert_eq!(heap.object_value(owner, key).unwrap(), Value::Null);
    assert_eq!(
        heap.stats().edge_slots,
        3,
        "the cleared key still occupies storage"
    );
    assert_eq!(heap.stats().live_edges, 2, "mode and the live entry");
    assert!(!heap.alive(dead));

    let reused = heap.alloc().unwrap();
    assert_eq!(reused.slot(), dead.slot());
    assert_ne!(reused.generation(), dead.generation());
    assert!(heap.alive(reused));
    assert!(!heap.alive(dead));

    let key_value = heap.issue_carrier(key).unwrap();
    let continued = heap.next_guest(owner, &key_value).unwrap().unwrap();
    assert_eq!(continued.0, Value::string("keep"));
    assert_eq!(continued.1, heap.issued_value(kept).unwrap());
}

fn assert_host_collect_keeps_the_marked_mode() {
    with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(
            &mut host,
            HeapLimits {
                nodes: 8,
                roots: 4,
                edges: 4,
                max_generation: 8,
            },
        )
        .expect("install");
        let mt_raw = publish_raw(&mut host);
        let owner_raw = publish_raw(&mut host);
        let key_raw = publish_raw(&mut host);
        let kept_raw = publish_raw(&mut host);
        let dead_raw = publish_raw(&mut host);
        let mt = table_ref(&host, mt_raw);
        let owner = table_ref(&host, owner_raw);
        let key = table_ref(&host, key_raw);
        let kept = table_ref(&host, kept_raw);
        let dead = table_ref(&host, dead_raw);
        heap::with_heap(&mut host, |heap| {
            let frame = heap.enter_roots(4)?;
            heap.write_root(frame, 0, mt)?;
            heap.write_root(frame, 1, owner)?;
            heap.write_root(frame, 2, key)?;
            heap.write_root(frame, 3, kept)?;
            heap.set_metatable(owner, Some(mt))?;
            heap.set_object_edge(owner, key, dead)?;
            heap.set_named_edge(owner, "keep", kept)?;
            heap.set_mode_scalar(mt, Value::string("v"))?;
            Ok(())
        })
        .expect("shape");

        heap::collect(&mut host).expect("host collect");
        let reused_raw = publish_raw(&mut host);
        let reused = table_ref(&host, reused_raw);
        heap::with_heap(&mut host, |heap| {
            assert_eq!(heap.object_value(owner, key)?, Value::Null);
            assert_eq!(heap.named_edge(owner, "keep")?, Some(kept));
            assert!(!heap.alive(dead));
            assert_eq!(reused.slot(), dead.slot());
            assert_ne!(reused.generation(), dead.generation());
            let meter = heap.take_scan_meter();
            assert_eq!(meter.weak_mode_bytes, 1);
            assert_eq!(meter.weak_mode_lookups, 1);
            assert_eq!(meter.sweep_edge_checks, 3);
            assert_eq!(heap.stats().edge_slots, 3);
            assert_eq!(heap.stats().live_edges, 2);
            Ok(())
        })
        .expect("host sweep used the marked mode");
    });
}
