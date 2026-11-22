//! Same-graph witness for a table, a closure, and a shared cell.
//!
//! This is not Lua source and does not execute a closure. Locked LuaJIT stores
//! the closure at numeric key `1` (`evidence/lua52-mixed-closure-weak-graph`).
//! The reviewed table-to-closure edge is a private field id. Field ids and
//! binary64 keys do not alias. The second table's back-edge uses the existing
//! string key `"1"`, matching that oracle. The observer stores the closure in
//! another private field, not under the oracle's string key `call`.

use std::sync::Arc;

use vm::{HostContext, Value};

use super::super::heap::{
    self, CellValue, ClosureRef, HeapError, HeapLimits, TableRef, UpvalueRef,
};
use super::super::{publish_raw, resource_handle, table_ref};

/// Private field on the anchor table. Not Lua numeric key 1.
const CLOSURE_FIELD: u32 = 1;
/// Private field on the weak observer. Not the string key `call`.
const OBSERVER_CLOSURE_FIELD: u32 = 1;

struct Mixed {
    frame: heap::RootFrameId,
    first: TableRef,
    first_raw: i64,
    first_carrier: Value,
    second: TableRef,
    second_raw: i64,
    second_carrier: Value,
    closure: ClosureRef,
    cell: UpvalueRef,
    observer: TableRef,
    observer_raw: i64,
    peer: TableRef,
    peer_raw: i64,
    orphan: TableRef,
    orphan_raw: i64,
    mode: TableRef,
    mode_raw: i64,
    tables_published: usize,
}

fn mixed_limits() -> HeapLimits {
    HeapLimits {
        nodes: 8,
        roots: 3,
        edges: 11,
        max_generation: 8,
    }
}

fn carrier_ptr(value: &Value) -> *const Vec<Value> {
    match value {
        Value::Array(array) => Arc::as_ptr(array),
        other => panic!("issued table is not an array: {other:?}"),
    }
}

fn token_open(host: &HostContext<'_>, raw: i64) -> bool {
    host.typed_resource_with_key::<heap::LuaTableResource>(
        resource_handle(raw),
        &heap::LuaTableResource::key(),
    )
    .is_ok()
}

fn issue(host: &mut HostContext<'_>, table: TableRef) -> Value {
    heap::with_heap(host, |heap| heap.issue_carrier(table)).expect("issue table carrier")
}

fn read_capture(host: &mut HostContext<'_>, closure: ClosureRef) -> Value {
    let table = heap::with_heap(host, |heap| match heap.read_cell(closure)? {
        Some(CellValue::Table(table)) => Ok(table),
        Some(CellValue::Closure(_)) => panic!("capture is not a closure"),
        Some(CellValue::Scalar(_)) => panic!("capture is not a table"),
        None => panic!("capture is nil"),
    })
    .expect("read the cell through the closure");
    heap::with_heap(host, |heap| heap.issued_value(table)).expect("issued captured table")
}

/// First table is the cycle's only explicit root. Observer and peer have their
/// own roots. The cell is not published and has no token.
fn seed(host: &mut HostContext<'_>) -> Mixed {
    heap::install(host, mixed_limits()).expect("install");
    let before_tokens = host.resource_count();
    let first_raw = publish_raw(host);
    let first = table_ref(host, first_raw);
    let second_raw = publish_raw(host);
    let second = table_ref(host, second_raw);
    let observer_raw = publish_raw(host);
    let observer = table_ref(host, observer_raw);
    let peer_raw = publish_raw(host);
    let peer = table_ref(host, peer_raw);
    let orphan_raw = publish_raw(host);
    let orphan = table_ref(host, orphan_raw);
    let mode_raw = publish_raw(host);
    let mode = table_ref(host, mode_raw);
    let tables_published = 6;
    assert_eq!(
        host.resource_count() - before_tokens,
        tables_published,
        "six table tokens"
    );

    let (closure, cell) = heap::with_heap(host, |heap| {
        let closure = heap.alloc_closure()?;
        let cell = heap.alloc_upvalue()?;
        Ok((closure, cell))
    })
    .expect("closure and cell");
    assert_eq!(
        host.resource_count() - before_tokens,
        tables_published,
        "a cell has no token; do not publish one"
    );
    assert_eq!(closure.kind(), heap::HandleKind::Closure);
    assert_eq!(cell.kind(), heap::HandleKind::Upvalue);

    let frame = heap::with_heap(host, |heap| heap.enter_roots(3)).expect("three roots");
    heap::with_heap(host, |heap| heap.write_root(frame, 0, first)).expect("anchor");
    heap::with_heap(host, |heap| heap.write_root(frame, 1, observer)).expect("observer root");
    heap::with_heap(host, |heap| heap.write_root(frame, 2, peer)).expect("peer root");

    heap::with_heap(host, |heap| {
        heap.set_closure_edge(first, CLOSURE_FIELD, closure)?;
        heap.set_upvalue_edge(closure, cell)?;
        heap.write_upvalue(cell, second)?;
        heap.set_named_edge(second, "1", first)?;
        heap.set_mode_scalar(mode, Value::string("v"))?;
        heap.set_metatable(observer, Some(mode))?;
        heap.set_number_edge(observer, 1.0, first)?;
        heap.set_named_edge(observer, "1", second)?;
        heap.set_closure_edge(observer, OBSERVER_CLOSURE_FIELD, closure)?;
        heap.set_named_edge(observer, "orphan", orphan)?;
        heap.set_named_edge(observer, "peer", peer)?;
        heap.set_named_edge(orphan, "self", orphan)?;
        Ok(())
    })
    .expect("link the mixed cycle and the weak observer");

    let first_carrier = issue(host, first);
    let second_carrier = issue(host, second);
    assert_eq!(
        carrier_ptr(&read_capture(host, closure)),
        carrier_ptr(&second_carrier),
        "capture readback uses read_cell, not a Lua call"
    );
    assert!(
        matches!(
            heap::with_heap(host, |heap| heap.number_value(first, 1.0)).expect("number key"),
            Value::Null
        ),
        "private field {CLOSURE_FIELD} is not Lua numeric key 1"
    );
    assert_eq!(
        heap::with_heap(host, |heap| heap.closure_edge(first, CLOSURE_FIELD)).expect("field"),
        closure
    );
    assert!(
        heap::with_heap(host, |heap| heap.named_edge(observer, "call"))
            .expect("call key")
            .is_none(),
        "observer closure is a private field, not string key call"
    );

    Mixed {
        frame,
        first,
        first_raw,
        first_carrier,
        second,
        second_raw,
        second_carrier,
        closure,
        cell,
        observer,
        observer_raw,
        peer,
        peer_raw,
        orphan,
        orphan_raw,
        mode,
        mode_raw,
        tables_published,
    }
}

fn edge_present(host: &mut HostContext<'_>, from: TableRef, name: &str, to: TableRef) -> bool {
    heap::with_heap(host, |heap| heap.named_edge(from, name))
        .expect("named edge")
        .is_some_and(|found| found == to)
}

fn number_present(host: &mut HostContext<'_>, from: TableRef, key: f64, to: TableRef) -> bool {
    let observed =
        heap::with_heap(host, |heap| heap.number_value(from, key)).expect("number value");
    let Value::Array(observed) = observed else {
        return false;
    };
    let Ok(Value::Array(expected)) = heap::with_heap(host, |heap| heap.issued_value(to)) else {
        return false;
    };
    Arc::ptr_eq(&observed, &expected)
}

fn closure_entry_present(host: &mut HostContext<'_>, graph: &Mixed) -> bool {
    heap::with_heap(host, |heap| {
        heap.closure_edge(graph.observer, OBSERVER_CLOSURE_FIELD)
    })
    .ok()
    .is_some_and(|found| found == graph.closure)
}

fn observer_count(host: &mut HostContext<'_>, graph: &Mixed) -> u32 {
    let mut count = 0;
    if number_present(host, graph.observer, 1.0, graph.first) {
        count += 1;
    }
    if edge_present(host, graph.observer, "1", graph.second) {
        count += 1;
    }
    if closure_entry_present(host, graph) {
        count += 1;
    }
    if edge_present(host, graph.observer, "orphan", graph.orphan) {
        count += 1;
    }
    if edge_present(host, graph.observer, "peer", graph.peer) {
        count += 1;
    }
    count
}

fn cycle_alive(host: &mut HostContext<'_>, graph: &Mixed) -> bool {
    heap::with_heap(host, |heap| {
        Ok(heap.alive(graph.first)
            && heap.closure_alive(graph.closure)
            && heap.upvalue_alive(graph.cell)
            && heap.alive(graph.second))
    })
    .expect("cycle handles")
}

fn assert_capture_identity(host: &mut HostContext<'_>, graph: &Mixed) {
    assert_eq!(
        carrier_ptr(&read_capture(host, graph.closure)),
        carrier_ptr(&graph.second_carrier)
    );
    assert_eq!(
        heap::with_heap(host, |heap| heap.read_cell(graph.closure)).expect("cell"),
        Some(CellValue::Table(graph.second))
    );
}

fn assert_retired_cycle(host: &mut HostContext<'_>, graph: &Mixed) {
    assert!(!cycle_alive(host, graph));
    assert!(!token_open(host, graph.first_raw));
    assert!(!token_open(host, graph.second_raw));
    assert!(!token_open(host, graph.orphan_raw));
    assert!(token_open(host, graph.peer_raw));
    assert!(token_open(host, graph.observer_raw));
    assert!(token_open(host, graph.mode_raw));
    assert!(heap::with_heap(host, |heap| Ok(heap.alive(graph.mode))).expect("mode table"));
    assert_eq!(
        observer_count(host, graph),
        1,
        "only the peer weak entry remains"
    );
    assert_eq!(
        heap::with_heap(host, |heap| heap.read_cell(graph.closure)).expect_err("expired"),
        heap::HeapError::StaleHandle
    );
    assert_eq!(
        heap::with_heap(host, |heap| Ok(heap.guest_class(&graph.first_carrier))).expect("class"),
        heap::GuestClass::Unissued
    );
}

fn collect(host: &mut HostContext<'_>) {
    heap::collect(host).expect("collect");
}

#[test]
fn mixed_graph_full_keeps_identities_then_retires_the_cycle() {
    super::super::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = seed(&mut host);
        assert!(cycle_alive(&mut host, &graph));
        assert!(token_open(&host, graph.orphan_raw));
        assert_eq!(observer_count(&mut host, &graph), 5);
        collect(&mut host);
        assert!(cycle_alive(&mut host, &graph));
        assert_capture_identity(&mut host, &graph);
        assert!(!heap::with_heap(&mut host, |heap| Ok(heap.alive(graph.orphan))).expect("orphan"));
        assert!(!token_open(&host, graph.orphan_raw), "orphan token retired");
        assert!(
            !edge_present(&mut host, graph.observer, "orphan", graph.orphan),
            "weak orphan entry cleared"
        );
        assert_eq!(observer_count(&mut host, &graph), 4);
        assert!(token_open(&host, graph.first_raw));
        assert!(token_open(&host, graph.second_raw));
        heap::with_heap(&mut host, |heap| heap.clear_root(graph.frame, 0)).expect("drop anchor");
        collect(&mut host);
        assert_retired_cycle(&mut host, &graph);
        assert_eq!(
            host.resource_count(),
            3,
            "observer, peer, and mode tokens; the cell was never issued"
        );
        let _ = graph.tables_published;
    });
}

#[test]
fn mixed_graph_omit_first_keeps_the_orphan_until_the_final_collect() {
    super::super::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = seed(&mut host);
        assert!(cycle_alive(&mut host, &graph));
        assert!(token_open(&host, graph.orphan_raw));
        assert_eq!(observer_count(&mut host, &graph), 5);
        heap::with_heap(&mut host, |heap| heap.clear_root(graph.frame, 0)).expect("drop anchor");
        collect(&mut host);
        assert_retired_cycle(&mut host, &graph);
    });
}

#[test]
fn mixed_graph_omit_final_leaves_the_cycle_after_the_anchor_is_cleared() {
    super::super::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = seed(&mut host);
        collect(&mut host);
        assert!(!token_open(&host, graph.orphan_raw));
        assert_eq!(observer_count(&mut host, &graph), 4);
        heap::with_heap(&mut host, |heap| heap.clear_root(graph.frame, 0)).expect("drop anchor");
        assert!(cycle_alive(&mut host, &graph));
        assert_capture_identity(&mut host, &graph);
        assert!(token_open(&host, graph.first_raw));
        assert!(token_open(&host, graph.second_raw));
        assert_eq!(observer_count(&mut host, &graph), 4);
        assert_eq!(heap::stats(&mut host).expect("stats").collections, 1);
    });
}

#[test]
fn mixed_graph_cut_drops_closure_cell_and_captured_table() {
    super::super::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = seed(&mut host);
        heap::with_heap(&mut host, |heap| {
            heap.set_field_scalar(graph.first, CLOSURE_FIELD, Value::Null)
        })
        .expect("cut the private field, not numeric key 1");
        assert!(matches!(
            heap::with_heap(&mut host, |heap| heap.number_value(graph.first, 1.0))
                .expect("number key"),
            Value::Null
        ));
        collect(&mut host);
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(graph.first))).expect("first"));
        assert!(token_open(&host, graph.first_raw));
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.closure_alive(graph.closure)))
                .expect("closure")
        );
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.upvalue_alive(graph.cell))).expect("cell")
        );
        assert!(!heap::with_heap(&mut host, |heap| Ok(heap.alive(graph.second))).expect("second"));
        assert!(!token_open(&host, graph.second_raw));
        assert!(!token_open(&host, graph.orphan_raw));
        assert!(token_open(&host, graph.peer_raw));
        assert_eq!(
            observer_count(&mut host, &graph),
            2,
            "anchored table and peer"
        );
        assert!(!closure_entry_present(&mut host, &graph));
        heap::with_heap(&mut host, |heap| heap.clear_root(graph.frame, 0)).expect("drop anchor");
        collect(&mut host);
        assert_retired_cycle(&mut host, &graph);
    });
}

const LEFT_KEY: u32 = 1;
const RIGHT_KEY: u32 = 2;
const BACK_KEY: u32 = 1;

struct Graph {
    frame: heap::RootFrameId,
    root: TableRef,
    root_raw: i64,
    root_carrier: Value,
    left: ClosureRef,
    right: ClosureRef,
    cell: UpvalueRef,
    original: TableRef,
    original_raw: i64,
    original_carrier: Value,
    replacement: TableRef,
    replacement_raw: i64,
    replacement_carrier: Value,
    independent: TableRef,
    independent_raw: i64,
    independent_carrier: Value,
}

fn graph_limits() -> HeapLimits {
    HeapLimits {
        nodes: 8,
        roots: 2,
        edges: 8,
        max_generation: 8,
    }
}
fn read_carrier(host: &mut HostContext<'_>, closure: ClosureRef) -> Value {
    let table = heap::with_heap(host, |heap| heap.read_upvalue(closure)).expect("read shared cell");
    heap::with_heap(host, |heap| heap.issued_value(table)).expect("issued current table")
}

/// One root table holds two closures over one cell. A write through that cell
/// is visible from the other closure. No collection runs here.
fn link_shared_cell(host: &mut HostContext<'_>) -> Graph {
    heap::install(host, graph_limits()).expect("install");
    let root_raw = publish_raw(host);
    let root = table_ref(host, root_raw);
    let independent_raw = publish_raw(host);
    let independent = table_ref(host, independent_raw);
    let frame = heap::with_heap(host, |heap| heap.enter_roots(2)).expect("two root slots");
    heap::with_heap(host, |heap| heap.write_root(frame, 0, root)).expect("root the cycle owner");
    heap::with_heap(host, |heap| heap.write_root(frame, 1, independent)).expect("independent root");

    let (left, right, cell) = heap::with_heap(host, |heap| {
        let left = heap.alloc_closure()?;
        let right = heap.alloc_closure()?;
        let cell = heap.alloc_upvalue()?;
        heap.set_closure_edge(root, LEFT_KEY, left)?;
        heap.set_closure_edge(root, RIGHT_KEY, right)?;
        heap.set_upvalue_edge(left, cell)?;
        heap.set_upvalue_edge(right, cell)?;
        Ok((left, right, cell))
    })
    .expect("two closures, one upvalue");

    let original_raw = publish_raw(host);
    let original = table_ref(host, original_raw);
    heap::with_heap(host, |heap| heap.write_upvalue(cell, original))
        .expect("capture the original table");
    let root_carrier = issue(host, root);
    let original_carrier = issue(host, original);
    let independent_carrier = issue(host, independent);

    let through_left = read_carrier(host, left);
    let through_right = read_carrier(host, right);
    assert_eq!(
        carrier_ptr(&through_left),
        carrier_ptr(&original_carrier),
        "the left closure reads the original table carrier"
    );
    assert_eq!(
        carrier_ptr(&through_right),
        carrier_ptr(&original_carrier),
        "the right closure reads the same carrier"
    );
    assert_eq!(
        heap::with_heap(host, |heap| heap.upvalue_of(left)).expect("left cell"),
        cell,
        "both closures name one upvalue"
    );
    assert_eq!(
        heap::with_heap(host, |heap| heap.upvalue_of(right)).expect("right cell"),
        cell
    );

    let replacement_raw = publish_raw(host);
    let replacement = table_ref(host, replacement_raw);
    let replacement_carrier = issue(host, replacement);
    heap::with_heap(host, |heap| heap.write_upvalue(cell, replacement))
        .expect("replace the current table");
    let seen = read_carrier(host, right);
    assert_eq!(
        carrier_ptr(&seen),
        carrier_ptr(&replacement_carrier),
        "a write is visible through the other closure"
    );
    assert_ne!(
        carrier_ptr(&seen),
        carrier_ptr(&original_carrier),
        "the other closure no longer reads the original carrier"
    );
    let stats = heap::stats(host).expect("stats after the write");
    assert_eq!(
        stats.live_nodes, 7,
        "four tables, two closures, one upvalue"
    );
    assert_eq!(
        stats.live_edges, 5,
        "two fields, two closure links, one current value"
    );
    assert_eq!(stats.node_slots, 7);
    assert_eq!(stats.edge_slots, 5);

    Graph {
        frame,
        root,
        root_raw,
        root_carrier,
        left,
        right,
        cell,
        original,
        original_raw,
        original_carrier,
        replacement,
        replacement_raw,
        replacement_carrier,
        independent,
        independent_raw,
        independent_carrier,
    }
}

fn release_replaced_table(host: &mut HostContext<'_>, graph: &Graph) {
    let before = heap::stats(host).expect("before replaced-table collect");
    let before_tokens = host.resource_count();
    let report = heap::collect(host).expect("collect the replaced table");
    let meter = heap::take_scan_meter(host).expect("meter");
    assert_eq!(meter.mark_passes, 1, "the existing collector marks once");
    assert_eq!(
        meter.outgoing_edge_checks, 5,
        "mark follows the closure and upvalue edges; got {}",
        meter.outgoing_edge_checks
    );
    assert_eq!(report.live_nodes, 6, "the replaced table is the only sweep");
    assert_eq!(before.live_nodes - report.live_nodes, 1);
    assert!(!heap::with_heap(host, |heap| Ok(heap.alive(graph.original))).expect("original"));
    assert!(
        !token_open(host, graph.original_raw),
        "the replaced table token is closed"
    );
    assert_eq!(
        heap::with_heap(host, |heap| Ok(heap.guest_class(&graph.original_carrier))).expect("class"),
        heap::GuestClass::Unissued,
        "holding the carrier must not keep the swept table"
    );
    let current = read_carrier(host, graph.left);
    assert_eq!(
        carrier_ptr(&current),
        carrier_ptr(&graph.replacement_carrier),
        "mark missed the capture: the rooted cell's current table was swept"
    );
    assert!(token_open(host, graph.replacement_raw));
    assert!(token_open(host, graph.root_raw));
    assert!(token_open(host, graph.independent_raw));
    assert_eq!(host.resource_count(), before_tokens - 1);
}

fn close_cycle(host: &mut HostContext<'_>, graph: &Graph) {
    heap::with_heap(host, |heap| {
        heap.set_edge(graph.replacement, BACK_KEY, graph.root)
    })
    .expect("current table points back at the root");
    assert_eq!(
        heap::with_heap(host, |heap| heap.edge(graph.replacement, BACK_KEY)).expect("back edge"),
        Some(graph.root)
    );
    assert_eq!(
        heap::with_heap(host, |heap| heap.edge(graph.root, LEFT_KEY)).expect_err("closure field"),
        HeapError::WrongKind,
        "a table-edge read must not turn a closure into a table"
    );
    heap::with_heap(host, |heap| heap.clear_root(graph.frame, 0))
        .expect("drop the only external root");
}

#[test]
fn closure_upvalue_quota_rejects_another_node_or_edge_and_reuses_kind_checks() {
    let mut heap = heap::LuaHeap::new(HeapLimits {
        nodes: 3,
        roots: 1,
        edges: 2,
        max_generation: 4,
    });
    let table = heap.alloc().expect("table");
    let closure = heap.alloc_closure().expect("closure");
    let cell = heap.alloc_upvalue().expect("upvalue");
    let before = heap.stats();
    assert!(
        heap.alloc_closure().is_err(),
        "node cap counts closure nodes"
    );
    assert_eq!(
        heap.stats(),
        before,
        "a refused closure alloc does not grow storage"
    );

    heap.set_closure_edge(table, LEFT_KEY, closure)
        .expect("field");
    heap.set_upvalue_edge(closure, cell).expect("cell");
    let edges = heap.stats();
    let captured = heap.alloc().expect_err("node cap still includes the table");
    assert!(matches!(captured, HeapError::NodeCapacity));
    assert_eq!(
        heap.write_upvalue(cell, table),
        Err(HeapError::EdgeCapacity),
        "edge cap counts the closure and upvalue links"
    );
    assert_eq!(heap.stats().live_edges, edges.live_edges);
    assert_eq!(heap.stats().edge_slots, edges.edge_slots);

    let mut other = heap::LuaHeap::new(HeapLimits {
        nodes: 4,
        roots: 1,
        edges: 4,
        max_generation: 4,
    });
    let foreign = other.alloc_closure().expect("foreign closure");
    assert_eq!(
        heap.set_upvalue_edge(foreign, cell),
        Err(HeapError::WrongDomain)
    );
    assert_eq!(
        heap.edge(table, LEFT_KEY),
        Err(HeapError::WrongKind),
        "the stored closure edge is not a table edge"
    );
    let foreign_table = other.alloc().expect("foreign table");
    assert_eq!(
        other.closure_edge(foreign_table, LEFT_KEY),
        Err(HeapError::StaleHandle)
    );

    let stale_closure = closure;
    let stale_cell = cell;
    assert_eq!(heap.collect().live_nodes, 0);
    assert_eq!(
        heap.read_upvalue(stale_closure),
        Err(HeapError::StaleHandle)
    );
    assert_eq!(
        heap.write_upvalue(stale_cell, table),
        Err(HeapError::StaleHandle)
    );
    assert!(!heap.alive(table));
}

#[test]
fn closure_upvalue_two_closures_share_a_cell_and_a_write_replaces_it() {
    super::super::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = link_shared_cell(&mut host);
        assert_eq!(graph.left.kind(), heap::HandleKind::Closure);
        assert_eq!(graph.right.kind(), heap::HandleKind::Closure);
        assert_eq!(graph.cell.kind(), heap::HandleKind::Upvalue);
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.upvalue_alive(graph.cell))).expect("cell")
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.read_upvalue(graph.left)).expect("left"),
            graph.replacement
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.read_upvalue(graph.right)).expect("right"),
            graph.replacement
        );
    });
}

#[test]
fn closure_upvalue_explicit_collect_releases_only_the_replaced_table() {
    super::super::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = link_shared_cell(&mut host);
        release_replaced_table(&mut host, &graph);
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(graph.root))).expect("root"));
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.closure_alive(graph.left))).expect("left")
        );
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.closure_alive(graph.right))).expect("right")
        );
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.upvalue_alive(graph.cell))).expect("cell")
        );
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.alive(graph.independent)))
                .expect("independent")
        );
    });
}

#[test]
fn closure_upvalue_collect_reclaims_the_mixed_cycle_and_closes_tokens() {
    super::super::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = link_shared_cell(&mut host);
        release_replaced_table(&mut host, &graph);
        close_cycle(&mut host, &graph);
        let before_tokens = host.resource_count();
        assert_eq!(
            before_tokens, 3,
            "root, replacement, and independent tokens"
        );
        assert_eq!(heap::stats(&mut host).expect("linked").live_edges, 6);
        assert!(token_open(&host, graph.root_raw));
        assert!(token_open(&host, graph.replacement_raw));

        let report = heap::collect(&mut host).expect("collect the unreachable cycle");
        assert_eq!(report.live_nodes, 1, "only the independent root remains");
        assert_eq!(report.live_roots, 1);
        assert!(!heap::with_heap(&mut host, |heap| Ok(heap.alive(graph.root))).expect("root"));
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.closure_alive(graph.left))).expect("left")
        );
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.closure_alive(graph.right))).expect("right")
        );
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.upvalue_alive(graph.cell))).expect("cell")
        );
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.alive(graph.replacement))).expect("current")
        );
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.alive(graph.independent)))
                .expect("independent")
        );
        assert!(
            !token_open(&host, graph.root_raw),
            "cycle table token closed"
        );
        assert!(
            !token_open(&host, graph.replacement_raw),
            "current table token closed"
        );
        assert!(token_open(&host, graph.independent_raw));
        assert_eq!(host.resource_count(), 1);
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.guest_class(&graph.root_carrier)))
                .expect("class"),
            heap::GuestClass::Unissued
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(
                heap.guest_class(&graph.independent_carrier)
            ))
            .expect("independent class"),
            heap::GuestClass::Table(graph.independent)
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.read_upvalue(graph.left))
                .expect_err("expired closure"),
            HeapError::StaleHandle
        );
    });
}

#[test]
fn closure_upvalue_omitting_the_final_collect_leaves_the_mixed_cycle() {
    super::super::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let graph = link_shared_cell(&mut host);
        release_replaced_table(&mut host, &graph);
        close_cycle(&mut host, &graph);
        let stats = heap::stats(&mut host).expect("before the omitted collect");
        assert_eq!(stats.live_nodes, 6);
        assert_eq!(stats.collections, 1, "only the replaced-table collect ran");
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(graph.root))).expect("root"));
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.closure_alive(graph.left))).expect("left")
        );
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.closure_alive(graph.right))).expect("right")
        );
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.upvalue_alive(graph.cell))).expect("cell")
        );
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.alive(graph.replacement))).expect("current")
        );
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.alive(graph.independent)))
                .expect("independent")
        );
        assert!(token_open(&host, graph.root_raw));
        assert!(token_open(&host, graph.replacement_raw));
        assert!(token_open(&host, graph.independent_raw));
        assert_eq!(host.resource_count(), 3);
        assert_eq!(
            carrier_ptr(&read_carrier(&mut host, graph.right)),
            carrier_ptr(&graph.replacement_carrier)
        );
    });
}

/// Direct closure root and a rebindable cell. This is object topology only:
/// not a recursive call, and not a guest closure carrier.
struct RecursiveCell {
    frame: heap::RootFrameId,
    original: ClosureRef,
    observer: ClosureRef,
    cell: UpvalueRef,
    replacement: ClosureRef,
    independent: TableRef,
    independent_raw: i64,
    independent_carrier: Value,
    doomed_raw: i64,
    doomed_carrier: Value,
    independent_closure: ClosureRef,
    proof_roots: u64,
}

fn recursive_limits() -> HeapLimits {
    HeapLimits {
        nodes: 7,
        roots: 4,
        edges: 3,
        max_generation: 8,
    }
}

fn read_cell(
    host: &mut HostContext<'_>,
    closure: ClosureRef,
) -> Result<Option<CellValue>, HeapError> {
    heap::with_heap(host, |heap| heap.read_cell(closure))
}

fn bind_recursive_cell(host: &mut HostContext<'_>) -> RecursiveCell {
    heap::install(host, recursive_limits()).expect("install");
    let independent_raw = publish_raw(host);
    let independent = table_ref(host, independent_raw);
    let doomed_raw = publish_raw(host);
    let doomed = table_ref(host, doomed_raw);
    assert_eq!(
        host.resource_count(),
        2,
        "two table tokens, no closure token"
    );

    let (original, observer, cell, independent_closure) = heap::with_heap(host, |heap| {
        let original = heap.alloc_closure()?;
        let observer = heap.alloc_closure()?;
        let independent_closure = heap.alloc_closure()?;
        let cell = heap.alloc_upvalue()?;
        Ok((original, observer, cell, independent_closure))
    })
    .expect("closures and one shared cell");
    let frame = heap::with_heap(host, |heap| heap.enter_roots(4)).expect("shared root quota");
    heap::with_heap(host, |heap| heap.write_closure_root(frame, 0, original))
        .expect("caller holds the original closure directly");
    heap::with_heap(host, |heap| heap.write_closure_root(frame, 1, observer))
        .expect("observer root");
    heap::with_heap(host, |heap| heap.write_root(frame, 2, independent))
        .expect("independent table");
    heap::with_heap(host, |heap| {
        heap.write_closure_root(frame, 3, independent_closure)
    })
    .expect("independent closure");
    assert_eq!(
        heap::with_heap(host, |heap| heap.enter_roots(1)).expect_err("root cap"),
        HeapError::RootCapacity,
        "a closure root spends the same root quota as a table root"
    );
    assert_eq!(heap::stats(host).expect("roots").root_slots, 4);
    assert_eq!(heap::stats(host).expect("roots").live_roots, 4);

    heap::with_heap(host, |heap| {
        heap.set_upvalue_edge(original, cell)?;
        heap.set_upvalue_edge(observer, cell)?;
        heap.write_upvalue(cell, doomed)?;
        Ok(())
    })
    .expect("two closures, one cell, current table");
    let edges = heap::stats(host).expect("at edge cap");
    assert_eq!(edges.edge_slots, 3);
    assert_eq!(
        heap::with_heap(host, |heap| heap.set_closure_edge(independent, 1, original))
            .expect_err("no spare edge"),
        HeapError::EdgeCapacity
    );
    assert_eq!(heap::stats(host).expect("refused edge").edge_slots, 3);

    heap::with_heap(host, |heap| heap.write_closure_upvalue(cell, original))
        .expect("replace the table with the original closure");
    assert_eq!(
        heap::stats(host).expect("overwrite").edge_slots,
        3,
        "rebinding the current edge must not add a record"
    );
    let alias = original;
    assert_eq!(
        read_cell(host, alias).expect("alias"),
        Some(CellValue::Closure(original))
    );
    assert_eq!(
        read_cell(host, observer).expect("observer"),
        Some(CellValue::Closure(original))
    );
    assert_eq!(
        heap::with_heap(host, |heap| heap.read_upvalue(original)).expect_err("table adapter"),
        HeapError::WrongKind,
        "a closure in the cell is not a table"
    );
    assert_eq!(
        read_cell(host, original).expect("kind error must not clear the cell"),
        Some(CellValue::Closure(original))
    );

    let doomed_carrier = issue(host, doomed);
    let independent_carrier = issue(host, independent);
    let retired = heap::collect(host).expect("retire the replaced table");
    assert_eq!(
        retired.live_nodes, 5,
        "the unrooted table is the only sweep"
    );
    assert!(
        !token_open(host, doomed_raw),
        "the replaced table token is closed"
    );
    assert!(token_open(host, independent_raw));
    assert_eq!(host.resource_count(), 1);
    assert_eq!(
        heap::with_heap(host, |heap| Ok(heap.guest_class(&doomed_carrier))).expect("class"),
        heap::GuestClass::Unissued,
        "holding the carrier must not keep the swept table"
    );
    assert!(heap::with_heap(host, |heap| Ok(heap.closure_alive(original))).expect("original"));

    let replacement = heap::with_heap(host, |heap| heap.alloc_closure()).expect("replacement");
    heap::with_heap(host, |heap| heap.write_closure_upvalue(cell, replacement))
        .expect("rebind the shared cell");
    assert_eq!(heap::stats(host).expect("second overwrite").edge_slots, 3);
    assert_eq!(
        read_cell(host, alias).expect("alias sees the rebind"),
        Some(CellValue::Closure(replacement))
    );
    assert_eq!(
        read_cell(host, observer).expect("observer sees the rebind"),
        Some(CellValue::Closure(replacement))
    );

    let _ = heap::take_scan_meter(host).expect("clear the previous meter");
    let proof = heap::collect(host).expect("closure root keeps the old closure");
    let meter = heap::take_scan_meter(host).expect("root scan meter");
    assert_eq!(proof.live_nodes, 6);
    assert_eq!(
        proof.live_roots, 4,
        "a live closure root is not cleared as a dead table"
    );
    assert!(
        heap::with_heap(host, |heap| Ok(heap.closure_alive(original))).expect("old closure"),
        "the old closure stays while its direct root is held"
    );
    assert!(
        heap::with_heap(host, |heap| Ok(heap.closure_alive(replacement))).expect("replacement")
    );
    assert_eq!(
        meter.root_slot_visits, 8,
        "mark and the dead-root sweep each visit the four shared root slots"
    );
    assert_eq!(meter.outgoing_edge_checks, 3);

    heap::with_heap(host, |heap| heap.clear_upvalue(cell)).expect("nil the cell");
    assert_eq!(
        heap::stats(host).expect("nil drops the record").edge_slots,
        2,
        "nil removes the current edge and does not store a nil record"
    );
    assert_eq!(read_cell(host, alias).expect("nil"), None);
    assert_eq!(
        read_cell(host, observer).expect("nil through the other observer"),
        None
    );
    let released = heap::collect(host).expect("nil releases the unrooted replacement");
    assert_eq!(released.live_nodes, 5);
    assert!(!heap::with_heap(host, |heap| Ok(heap.closure_alive(replacement))).expect("released"));
    assert!(heap::with_heap(host, |heap| Ok(heap.closure_alive(original))).expect("still rooted"));
    assert_eq!(
        read_cell(host, original).expect("nil is not an invalid handle"),
        None
    );

    heap::with_heap(host, |heap| heap.write_closure_upvalue(cell, original))
        .expect("restore the self-cycle");
    assert_eq!(heap::stats(host).expect("restored").edge_slots, 3);
    assert_eq!(
        read_cell(host, alias).expect("restored alias"),
        Some(CellValue::Closure(original))
    );
    assert_eq!(
        read_cell(host, observer).expect("restored observer"),
        Some(CellValue::Closure(original))
    );

    RecursiveCell {
        frame,
        original,
        observer,
        cell,
        replacement,
        independent,
        independent_raw,
        independent_carrier,
        doomed_raw,
        doomed_carrier,
        independent_closure,
        proof_roots: meter.root_slot_visits,
    }
}

fn drop_cycle_roots(host: &mut HostContext<'_>, cycle: &RecursiveCell) {
    heap::with_heap(host, |heap| heap.clear_root(cycle.frame, 0)).expect("drop original root");
    heap::with_heap(host, |heap| heap.clear_root(cycle.frame, 1)).expect("drop observer root");
}

#[test]
fn closure_root_shares_root_quota_and_cleanup_with_table_roots() {
    let mut heap = heap::LuaHeap::new(HeapLimits {
        nodes: 4,
        roots: 2,
        edges: 1,
        max_generation: 4,
    });
    let table = heap.alloc().expect("table");
    let closure = heap.alloc_closure().expect("closure");
    let frame = heap.enter_roots(2).expect("two shared slots");
    heap.write_root(frame, 0, table).expect("table root");
    heap.write_closure_root(frame, 1, closure)
        .expect("closure root");
    assert_eq!(heap.enter_roots(1), Err(HeapError::RootCapacity));
    assert_eq!(heap.stats().root_slots, 2);
    assert_eq!(heap.stats().live_roots, 2);

    heap.clear_root(frame, 1).expect("clear the closure root");
    assert_eq!(heap.stats().live_roots, 1);
    let swept = heap.collect();
    assert_eq!(swept.live_nodes, 1, "the unrooted closure is swept");
    assert!(heap.alive(table));
    assert!(!heap.closure_alive(closure));
    assert_eq!(swept.live_roots, 1);

    heap.leave_roots(frame).expect("drop the table root too");
    assert_eq!(heap.stats().root_slots, 0);
    assert_eq!(heap.stats().live_roots, 0);
    let again = heap
        .enter_roots(2)
        .expect("quota returns to the same vector");
    let fresh = heap.alloc_closure().expect("reuse the swept slot");
    heap.write_closure_root(again, 0, fresh)
        .expect("closure in a freed slot");
    heap.write_root(again, 1, table)
        .expect("table in the other slot");
    assert_eq!(heap.stats().live_roots, 2);
    assert_eq!(heap.collect().live_nodes, 2);
}

#[test]
fn closure_root_and_cell_reject_foreign_and_retired_without_clobber() {
    let mut heap = heap::LuaHeap::new(HeapLimits {
        nodes: 4,
        roots: 2,
        edges: 2,
        max_generation: 4,
    });
    let local = heap.alloc_closure().expect("local");
    let cell = heap.alloc_upvalue().expect("cell");
    heap.set_upvalue_edge(local, cell).expect("link");
    heap.write_closure_upvalue(cell, local).expect("self-cycle");
    let frame = heap.enter_roots(1).expect("frame");
    heap.write_closure_root(frame, 0, local).expect("root");
    assert_eq!(
        heap.read_upvalue(local),
        Err(HeapError::WrongKind),
        "the table read must not turn the closure into a table"
    );
    assert_eq!(
        heap.read_cell(local).expect("typed read"),
        Some(CellValue::Closure(local))
    );

    let mut other = heap::LuaHeap::new(HeapLimits {
        nodes: 2,
        roots: 1,
        edges: 1,
        max_generation: 4,
    });
    let foreign = other.alloc_closure().expect("foreign");
    let foreign_cell = other.alloc_upvalue().expect("foreign cell");
    assert_eq!(
        heap.write_closure_root(frame, 0, foreign),
        Err(HeapError::WrongDomain)
    );
    assert_eq!(
        heap.write_closure_upvalue(cell, foreign),
        Err(HeapError::WrongDomain)
    );
    assert_eq!(
        heap.clear_upvalue(foreign_cell),
        Err(HeapError::WrongDomain)
    );
    assert_eq!(
        heap.read_cell(local).expect("unchanged cell"),
        Some(CellValue::Closure(local))
    );
    assert_eq!(heap.stats().live_roots, 1);
    assert_eq!(heap.stats().live_edges, 2);
    assert_eq!(heap.collect().live_nodes, 2);
    assert!(heap.closure_alive(local));

    let retired = heap.alloc_closure().expect("soon retired");
    assert_eq!(heap.collect().live_nodes, 2);
    assert!(!heap.closure_alive(retired));
    assert_eq!(
        heap.write_closure_root(frame, 0, retired),
        Err(HeapError::StaleHandle)
    );
    assert_eq!(
        heap.write_closure_upvalue(cell, retired),
        Err(HeapError::StaleHandle)
    );
    assert_eq!(
        heap.read_cell(local)
            .expect("retired write must not replace the cell"),
        Some(CellValue::Closure(local))
    );
    assert_eq!(heap.stats().live_roots, 1);
    assert!(heap.closure_alive(local));

    heap.clear_root(frame, 0).expect("drop");
    assert_eq!(heap.collect().live_nodes, 0);
    let empty = heap.enter_roots(1).expect("empty frame");
    assert_eq!(
        heap.write_closure_root(empty, 0, local),
        Err(HeapError::StaleHandle)
    );
    assert_eq!(heap.clear_upvalue(cell), Err(HeapError::StaleHandle));
    assert_eq!(heap.read_cell(local), Err(HeapError::StaleHandle));
    assert_ne!(
        heap.read_cell(local),
        Ok(None),
        "a retired closure is an invalid handle, not nil"
    );
    assert_eq!(heap.stats().live_roots, 0);
    assert_eq!(heap.stats().live_edges, 0);
}

#[test]
fn closure_root_recursive_cell_rebind_is_visible_and_collect_reclaims_the_cycle() {
    super::super::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let cycle = bind_recursive_cell(&mut host);
        assert_eq!(
            cycle.proof_roots, 8,
            "the closure-root collect metered the root scan"
        );
        drop_cycle_roots(&mut host, &cycle);
        let before = heap::stats(&mut host).expect("before the cycle collect");
        assert_eq!(before.live_nodes, 5);
        assert!(token_open(&host, cycle.independent_raw));
        assert!(!token_open(&host, cycle.doomed_raw));

        let report = heap::collect(&mut host).expect("collect the unrooted cycle");
        assert_eq!(report.live_nodes, 2, "independent table and closure remain");
        assert_eq!(report.live_roots, 2);
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.closure_alive(cycle.original)))
                .expect("original")
        );
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.closure_alive(cycle.observer)))
                .expect("observer")
        );
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.upvalue_alive(cycle.cell))).expect("cell")
        );
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.closure_alive(cycle.replacement)))
                .expect("replacement")
        );
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.alive(cycle.independent))).expect("table")
        );
        assert!(
            heap::with_heap(&mut host, |heap| Ok(
                heap.closure_alive(cycle.independent_closure)
            ))
            .expect("independent closure")
        );
        assert!(token_open(&host, cycle.independent_raw));
        assert!(!token_open(&host, cycle.doomed_raw));
        assert_eq!(host.resource_count(), 1);
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(
                heap.guest_class(&cycle.independent_carrier)
            ))
            .expect("independent class"),
            heap::GuestClass::Table(cycle.independent)
        );
        assert_eq!(
            heap::with_heap(
                &mut host,
                |heap| Ok(heap.guest_class(&cycle.doomed_carrier))
            )
            .expect("doomed class"),
            heap::GuestClass::Unissued
        );
        assert_eq!(
            read_cell(&mut host, cycle.original).expect_err("retired closure"),
            HeapError::StaleHandle
        );
    });
}

#[test]
fn closure_root_recursive_cell_omitting_the_final_collect_leaves_the_cycle() {
    super::super::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let cycle = bind_recursive_cell(&mut host);
        drop_cycle_roots(&mut host, &cycle);
        let stats = heap::stats(&mut host).expect("omitted collect");
        assert_eq!(stats.live_nodes, 5);
        assert_eq!(
            stats.collections, 3,
            "only the earlier release collects ran"
        );
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.closure_alive(cycle.original)))
                .expect("original")
        );
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.closure_alive(cycle.observer)))
                .expect("observer")
        );
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.upvalue_alive(cycle.cell))).expect("cell")
        );
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.alive(cycle.independent))).expect("table")
        );
        assert!(
            heap::with_heap(&mut host, |heap| Ok(
                heap.closure_alive(cycle.independent_closure)
            ))
            .expect("independent closure")
        );
        assert_eq!(
            read_cell(&mut host, cycle.observer).expect("still linked"),
            Some(CellValue::Closure(cycle.original))
        );
        assert!(token_open(&host, cycle.independent_raw));
        assert!(!token_open(&host, cycle.doomed_raw));
        assert_eq!(host.resource_count(), 1);
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(
                heap.guest_class(&cycle.independent_carrier)
            ))
            .expect("independent class"),
            heap::GuestClass::Table(cycle.independent)
        );
    });
}

/// One frame slot roots the open cell. The captured table has no root of its own.
struct OpenCell {
    frame: heap::RootFrameId,
    cell: UpvalueRef,
    original: TableRef,
    original_raw: i64,
    original_carrier: Value,
    sibling: TableRef,
    sibling_raw: i64,
}

fn open_cell_limits() -> HeapLimits {
    HeapLimits {
        nodes: 4,
        roots: 1,
        edges: 2,
        max_generation: 4,
    }
}

fn root_open_cell(host: &mut HostContext<'_>) -> OpenCell {
    heap::install(host, open_cell_limits()).expect("install");
    let original_raw = publish_raw(host);
    let original = table_ref(host, original_raw);
    let sibling_raw = publish_raw(host);
    let sibling = table_ref(host, sibling_raw);
    let cell = heap::with_heap(host, |heap| heap.alloc_upvalue()).expect("cell");
    heap::with_heap(host, |heap| heap.write_upvalue(cell, original)).expect("capture");
    let frame = heap::with_heap(host, |heap| heap.enter_roots(1)).expect("one slot");
    heap::with_heap(host, |heap| heap.write_upvalue_root(frame, 0, cell))
        .expect("the frame slot roots the cell");
    assert_eq!(
        heap::with_heap(host, |heap| heap.enter_roots(1)).expect_err("shared quota"),
        HeapError::RootCapacity,
        "an upvalue root spends the same root quota as a table root"
    );
    let stats = heap::stats(host).expect("rooted cell");
    assert_eq!(stats.live_roots, 1, "the cell is the only root");
    assert_eq!(
        stats.root_slots, 1,
        "the table has no independent root slot"
    );
    assert_eq!(
        stats.live_edges, 1,
        "the current edge is the only stored value"
    );
    assert_eq!(stats.live_nodes, 3, "two tables and one cell");
    let original_carrier = issue(host, original);
    OpenCell {
        frame,
        cell,
        original,
        original_raw,
        original_carrier,
        sibling,
        sibling_raw,
    }
}

fn collect_open_sibling(host: &mut HostContext<'_>, open: &OpenCell) {
    let _ = heap::take_scan_meter(host).expect("clear setup meter");
    let report = heap::collect(host).expect("collect the unrooted sibling");
    let meter = heap::take_scan_meter(host).expect("same collector");
    assert_eq!(meter.mark_passes, 1);
    assert_eq!(
        meter.outgoing_edge_checks, 1,
        "mark follows the current edge from the rooted cell"
    );
    assert_eq!(
        meter.root_slot_visits, 2,
        "mark and the dead-root sweep each visit the one frame slot"
    );
    assert_eq!(
        meter.token_closes, 1,
        "the sibling token is closed by this collect"
    );
    assert_eq!(report.live_nodes, 2, "the cell and its current table stay");
    assert_eq!(report.live_roots, 1);
    assert!(heap::with_heap(host, |heap| Ok(heap.upvalue_alive(open.cell))).expect("cell"));
    assert!(heap::with_heap(host, |heap| Ok(heap.alive(open.original))).expect("table"));
    assert!(!heap::with_heap(host, |heap| Ok(heap.alive(open.sibling))).expect("sibling"));
    assert!(token_open(host, open.original_raw));
    assert!(!token_open(host, open.sibling_raw));
    assert_eq!(host.resource_count(), 1);
    assert_eq!(
        heap::with_heap(host, |heap| Ok(heap.guest_class(&open.original_carrier))).expect("class"),
        heap::GuestClass::Table(open.original)
    );
}

/// The active frame's one slot keeps the captured table. No table root is stored.
/// After that collect, clearing the slot lets the unescaped cell and table die.
#[test]
fn frame_owned_cell_keeps_the_table_then_clear_and_collect_reclaims_it() {
    super::super::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let open = root_open_cell(&mut host);
        collect_open_sibling(&mut host, &open);

        let reader = heap::with_heap(&mut host, |heap| heap.alloc_closure()).expect("reader");
        assert_eq!(
            reader.slot(),
            open.sibling.slot(),
            "the swept sibling slot is reused"
        );
        assert_ne!(reader.generation(), open.sibling.generation());
        heap::with_heap(&mut host, |heap| heap.set_upvalue_edge(reader, open.cell))
            .expect("read through the surviving cell");
        let seen = read_carrier(&mut host, reader);
        assert_eq!(
            carrier_ptr(&seen),
            carrier_ptr(&open.original_carrier),
            "clearing is not required for the current edge to stay the original table"
        );

        heap::with_heap(&mut host, |heap| heap.clear_root(open.frame, 0)).expect("block exit");
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.current_frame())).expect("frame"),
            Some(open.frame),
            "clearing the slot does not retire the frame"
        );
        assert_eq!(heap::stats(&mut host).expect("cleared").live_roots, 0);
        assert_eq!(heap::stats(&mut host).expect("cleared").root_slots, 1);
        let still = read_carrier(&mut host, reader);
        assert_eq!(
            carrier_ptr(&still),
            carrier_ptr(&open.original_carrier),
            "clearing the root must not copy or clear the current value"
        );

        let report = heap::collect(&mut host).expect("collect the unescaped cell");
        assert_eq!(report.live_nodes, 0);
        assert_eq!(report.live_roots, 0);
        assert!(
            !heap::with_heap(&mut host, |heap| Ok(heap.upvalue_alive(open.cell))).expect("cell")
        );
        assert!(!heap::with_heap(&mut host, |heap| Ok(heap.alive(open.original))).expect("table"));
        assert!(!token_open(&host, open.original_raw));
        assert_eq!(host.resource_count(), 0);
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(
                heap.guest_class(&open.original_carrier)
            ))
            .expect("class"),
            heap::GuestClass::Unissued
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.read_cell(reader)).expect_err("swept reader"),
            HeapError::StaleHandle
        );
    });
}

/// Clearing the frame slot does not destroy the cell. The paired collect does.
#[test]
fn frame_owned_cell_clear_without_collect_keeps_the_table() {
    super::super::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        let open = root_open_cell(&mut host);
        collect_open_sibling(&mut host, &open);
        heap::with_heap(&mut host, |heap| heap.clear_root(open.frame, 0)).expect("block exit");
        let stats = heap::stats(&mut host).expect("omitted collect");
        assert_eq!(stats.collections, 1, "only the sibling collect ran");
        assert_eq!(stats.live_nodes, 2);
        assert_eq!(stats.live_roots, 0);
        assert_eq!(stats.root_slots, 1, "the frame is still active");
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.upvalue_alive(open.cell))).expect("cell")
        );
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(open.original))).expect("table"));
        assert!(token_open(&host, open.original_raw));
        assert!(!token_open(&host, open.sibling_raw));
        assert_eq!(host.resource_count(), 1);
        let reader = heap::with_heap(&mut host, |heap| heap.alloc_closure()).expect("reader");
        heap::with_heap(&mut host, |heap| heap.set_upvalue_edge(reader, open.cell)).expect("link");
        let seen = read_carrier(&mut host, reader);
        assert_eq!(
            carrier_ptr(&seen),
            carrier_ptr(&open.original_carrier),
            "root cleanup must not copy or clear the current value"
        );
    });
}

fn shared_slot_limits() -> HeapLimits {
    HeapLimits {
        nodes: 8,
        roots: 3,
        edges: 4,
        max_generation: 8,
    }
}

/// Two closures share the old cell. Block exit clears only that root slot.
/// The same slot then roots a new cell. The old closures still read the old value.
#[test]
fn escaped_closures_keep_the_old_cell_after_the_frame_slot_is_reused() {
    super::super::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(&mut host, shared_slot_limits()).expect("install");
        let original_raw = publish_raw(&mut host);
        let original = table_ref(&host, original_raw);
        let orphan_raw = publish_raw(&mut host);
        let orphan = table_ref(&host, orphan_raw);
        let (left, right, old_cell) = heap::with_heap(&mut host, |heap| {
            let left = heap.alloc_closure()?;
            let right = heap.alloc_closure()?;
            let old_cell = heap.alloc_upvalue()?;
            heap.set_upvalue_edge(left, old_cell)?;
            heap.set_upvalue_edge(right, old_cell)?;
            heap.write_upvalue(old_cell, original)?;
            Ok((left, right, old_cell))
        })
        .expect("two closures, one cell");
        let frame = heap::with_heap(&mut host, |heap| heap.enter_roots(3)).expect("outer frame");
        heap::with_heap(&mut host, |heap| {
            heap.write_upvalue_root(frame, 0, old_cell)
        })
        .expect("open cell root");
        heap::with_heap(&mut host, |heap| heap.write_closure_root(frame, 1, left))
            .expect("left escape root");
        heap::with_heap(&mut host, |heap| heap.write_closure_root(frame, 2, right))
            .expect("right escape root");
        let original_carrier = issue(&mut host, original);

        let _ = heap::take_scan_meter(&mut host).expect("clear setup meter");
        let first = heap::collect(&mut host).expect("collect the orphan");
        let first_meter = heap::take_scan_meter(&mut host).expect("first meter");
        assert_eq!(first_meter.mark_passes, 1);
        assert_eq!(first_meter.outgoing_edge_checks, 3);
        assert_eq!(first_meter.root_slot_visits, 6);
        assert_eq!(first_meter.token_closes, 1);
        assert_eq!(first.live_nodes, 4);
        assert!(!token_open(&host, orphan_raw));
        assert!(token_open(&host, original_raw));
        assert_eq!(
            carrier_ptr(&read_carrier(&mut host, left)),
            carrier_ptr(&original_carrier)
        );
        assert_eq!(
            carrier_ptr(&read_carrier(&mut host, right)),
            carrier_ptr(&original_carrier)
        );

        heap::with_heap(&mut host, |heap| heap.clear_root(frame, 0)).expect("block exit");
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.current_frame())).expect("outer frame"),
            Some(frame),
            "block exit clears one slot and leaves the outer frame active"
        );
        assert_eq!(heap::stats(&mut host).expect("after exit").live_roots, 2);
        assert_eq!(heap::stats(&mut host).expect("after exit").root_slots, 3);
        assert_eq!(
            carrier_ptr(&read_carrier(&mut host, left)),
            carrier_ptr(&original_carrier),
            "clearing the open root must not copy or clear the old value"
        );

        let new_cell = heap::with_heap(&mut host, |heap| heap.alloc_upvalue()).expect("new cell");
        assert_eq!(
            new_cell.slot(),
            orphan.slot(),
            "the swept orphan slot is reused"
        );
        assert_ne!(new_cell.generation(), orphan.generation());
        heap::with_heap(&mut host, |heap| {
            heap.write_upvalue_root(frame, 0, new_cell)
        })
        .expect("reuse the same frame slot");
        assert_eq!(heap::stats(&mut host).expect("reused").live_roots, 3);
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.upvalue_of(left)).expect("left still old"),
            old_cell
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.upvalue_of(right)).expect("right still old"),
            old_cell
        );

        let fresh_raw = publish_raw(&mut host);
        let fresh = table_ref(&host, fresh_raw);
        heap::with_heap(&mut host, |heap| heap.write_upvalue(new_cell, fresh))
            .expect("first new value");
        let fresh_carrier = issue(&mut host, fresh);
        let replacement_raw = publish_raw(&mut host);
        let replacement = table_ref(&host, replacement_raw);
        heap::with_heap(&mut host, |heap| heap.write_upvalue(new_cell, replacement))
            .expect("change the new cell");
        let replacement_carrier = issue(&mut host, replacement);
        assert_eq!(heap::stats(&mut host).expect("overwrite").edge_slots, 4);
        assert_eq!(
            carrier_ptr(&read_carrier(&mut host, right)),
            carrier_ptr(&original_carrier),
            "the old closure still reads the original table"
        );
        assert_ne!(
            carrier_ptr(&read_carrier(&mut host, left)),
            carrier_ptr(&replacement_carrier)
        );
        assert_ne!(
            carrier_ptr(&read_carrier(&mut host, left)),
            carrier_ptr(&fresh_carrier)
        );

        let middle = heap::collect(&mut host).expect("survival follows existing roots");
        let middle_meter = heap::take_scan_meter(&mut host).expect("middle meter");
        assert_eq!(middle_meter.mark_passes, 1);
        assert_eq!(middle_meter.outgoing_edge_checks, 4);
        assert_eq!(middle_meter.root_slot_visits, 6);
        assert_eq!(middle_meter.token_closes, 1);
        assert_eq!(middle.live_nodes, 6);
        assert_eq!(middle.live_roots, 3);
        assert!(
            !token_open(&host, fresh_raw),
            "the replaced new value is collected"
        );
        assert!(token_open(&host, original_raw));
        assert!(token_open(&host, replacement_raw));
        assert_eq!(host.resource_count(), 2);
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.upvalue_alive(old_cell))).expect("old cell")
        );
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.upvalue_alive(new_cell))).expect("new cell")
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.guest_class(&fresh_carrier))).expect("fresh"),
            heap::GuestClass::Unissued
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.guest_class(&replacement_carrier)))
                .expect("replacement"),
            heap::GuestClass::Table(replacement)
        );

        heap::with_heap(&mut host, |heap| heap.clear_root(frame, 1)).expect("drop left root");
        heap::with_heap(&mut host, |heap| heap.clear_root(frame, 2)).expect("drop right root");
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.current_frame())).expect("still outer"),
            Some(frame)
        );
        assert_eq!(heap::stats(&mut host).expect("roots dropped").live_roots, 1);
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.upvalue_alive(old_cell)))
                .expect("clear does not destroy")
        );
        assert_eq!(
            carrier_ptr(&read_carrier(&mut host, left)),
            carrier_ptr(&original_carrier),
            "dropping the closure roots must not rewrite the old cell"
        );

        let final_report = heap::collect(&mut host).expect("collect the old path");
        let final_meter = heap::take_scan_meter(&mut host).expect("final meter");
        assert_eq!(final_meter.mark_passes, 1);
        assert_eq!(final_meter.outgoing_edge_checks, 1);
        assert_eq!(final_meter.token_closes, 1);
        assert_eq!(
            final_report.live_nodes, 2,
            "the new cell and its table stay"
        );
        assert_eq!(final_report.live_roots, 1);
        assert!(!heap::with_heap(&mut host, |heap| Ok(heap.upvalue_alive(old_cell))).expect("old"));
        assert!(!heap::with_heap(&mut host, |heap| Ok(heap.closure_alive(left))).expect("left"));
        assert!(!heap::with_heap(&mut host, |heap| Ok(heap.closure_alive(right))).expect("right"));
        assert!(!heap::with_heap(&mut host, |heap| Ok(heap.alive(original))).expect("old table"));
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.upvalue_alive(new_cell))).expect("new"));
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(replacement))).expect("new table"));
        assert!(
            !token_open(&host, original_raw),
            "the old table token is retired"
        );
        assert!(token_open(&host, replacement_raw));
        assert_eq!(host.resource_count(), 1);
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.guest_class(&original_carrier)))
                .expect("old class"),
            heap::GuestClass::Unissued
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.guest_class(&replacement_carrier)))
                .expect("new class"),
            heap::GuestClass::Table(replacement)
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.upvalue_of(left)).expect_err("old path"),
            HeapError::StaleHandle
        );
    });
}

/// A foreign or retired cell is refused by the existing handle check.
/// The occupied frame slot stays on the live cell.
#[test]
fn upvalue_root_rejects_foreign_and_stale_without_clobber() {
    let mut heap = heap::LuaHeap::new(HeapLimits {
        nodes: 4,
        roots: 1,
        edges: 1,
        max_generation: 4,
    });
    let table = heap.alloc().expect("table");
    let cell = heap.alloc_upvalue().expect("cell");
    heap.write_upvalue(cell, table).expect("capture");
    let frame = heap.enter_roots(1).expect("frame");
    heap.write_upvalue_root(frame, 0, cell).expect("root");

    let mut other = heap::LuaHeap::new(HeapLimits {
        nodes: 1,
        roots: 1,
        edges: 1,
        max_generation: 4,
    });
    let foreign = other.alloc_upvalue().expect("foreign");
    assert_eq!(
        heap.write_upvalue_root(frame, 0, foreign),
        Err(HeapError::WrongDomain)
    );
    assert_eq!(heap.stats().live_roots, 1);
    assert_eq!(
        heap.collect().live_nodes,
        2,
        "the foreign write did not replace the slot"
    );
    assert!(heap.upvalue_alive(cell));
    assert!(heap.alive(table));

    let stale = heap.alloc_upvalue().expect("soon stale");
    assert_eq!(heap.collect().live_nodes, 2, "the unrooted cell is swept");
    assert!(!heap.upvalue_alive(stale));
    assert_eq!(
        heap.write_upvalue_root(frame, 0, stale),
        Err(HeapError::StaleHandle)
    );
    assert_eq!(heap.stats().live_roots, 1);
    assert_eq!(heap.stats().live_edges, 1);
    assert_eq!(
        heap.collect().live_nodes,
        2,
        "the stale write did not clear the slot"
    );
    assert!(heap.upvalue_alive(cell));
    assert!(heap.alive(table));
}

/// Private field on a value table that points back at its closure key.
///
/// Locked LuaJIT `96f7b297` with `-joff` and Lua 5.2 compat stores that
/// back-edge as `{ key = key }` (`evidence/lua52-gc-weak-closure-modes`).
/// This heap has no Lua source closure key. The value uses the existing
/// private field, which is not a closure identity and not numeric key 1.
const CLOSURE_KEY_BACKREF_FIELD: u32 = 1;

fn closure_key_limits() -> HeapLimits {
    HeapLimits {
        nodes: 12,
        roots: 4,
        edges: 12,
        max_generation: 8,
    }
}

struct ClosureKeyGraph {
    owner: TableRef,
    survivor: TableRef,
    survivor_raw: i64,
    watch: TableRef,
    orphan: TableRef,
    orphan_raw: i64,
    value: TableRef,
    value_raw: i64,
    isolated: ClosureRef,
    linked: ClosureRef,
    baseline_tokens: usize,
}

fn seed_closure_keys(host: &mut HostContext<'_>, mode: &str) -> ClosureKeyGraph {
    heap::install(host, closure_key_limits()).expect("install");
    let baseline_tokens = host.resource_count();
    let owner_raw = publish_raw(host);
    let owner = table_ref(host, owner_raw);
    let mode_raw = publish_raw(host);
    let mode_table = table_ref(host, mode_raw);
    let survivor_raw = publish_raw(host);
    let survivor = table_ref(host, survivor_raw);
    let watch_raw = publish_raw(host);
    let watch = table_ref(host, watch_raw);
    let watch_mode_raw = publish_raw(host);
    let watch_mode = table_ref(host, watch_mode_raw);
    let orphan_raw = publish_raw(host);
    let orphan = table_ref(host, orphan_raw);
    let value_raw = publish_raw(host);
    let value = table_ref(host, value_raw);
    assert_eq!(host.resource_count() - baseline_tokens, 7);

    let (isolated, linked) = heap::with_heap(host, |heap| {
        let isolated = heap.alloc_closure()?;
        let linked = heap.alloc_closure()?;
        Ok((isolated, linked))
    })
    .expect("two closure keys");
    assert_eq!(
        host.resource_count() - baseline_tokens,
        7,
        "a closure key has no resource token"
    );
    assert_eq!(isolated.kind(), heap::HandleKind::Closure);
    assert_eq!(linked.kind(), heap::HandleKind::Closure);
    assert_eq!(isolated.domain(), owner.domain());
    assert_ne!(isolated, linked);

    let frame = heap::with_heap(host, |heap| heap.enter_roots(3)).expect("roots");
    heap::with_heap(host, |heap| {
        heap.write_root(frame, 0, owner)?;
        heap.write_root(frame, 1, survivor)?;
        heap.write_root(frame, 2, watch)?;
        heap.set_mode_scalar(mode_table, Value::string(mode))?;
        heap.set_metatable(owner, Some(mode_table))?;
        heap.set_mode_scalar(watch_mode, Value::string("v"))?;
        heap.set_metatable(watch, Some(watch_mode))?;
        heap.set_closure_key_scalar(owner, isolated, Value::string("key-only"))?;
        heap.set_closure_key_edge(owner, linked, value)?;
        heap.set_closure_edge(value, CLOSURE_KEY_BACKREF_FIELD, linked)?;
        heap.set_named_edge(watch, "orphan", orphan)?;
        heap.set_named_edge(orphan, "self", orphan)?;
        heap.set_named_scalar(survivor, "mark", Value::string("alive"))?;
        Ok(())
    })
    .expect("publish closure keys");

    assert_eq!(
        heap::with_heap(host, |heap| heap
            .closure_edge(value, CLOSURE_KEY_BACKREF_FIELD))
        .expect("private back-edge"),
        linked
    );
    assert_eq!(
        heap::with_heap(host, |heap| heap.closure_key_edge(value, linked)).expect("not a key"),
        None,
        "the value holds the closure in a private field, not as a closure key"
    );
    assert!(
        heap::with_heap(host, |heap| heap.edge(owner, isolated.slot()))
            .expect("field read")
            .is_none(),
        "a closure key is not the field id of its slot"
    );

    ClosureKeyGraph {
        owner,
        survivor,
        survivor_raw,
        watch,
        orphan,
        orphan_raw,
        value,
        value_raw,
        isolated,
        linked,
        baseline_tokens,
    }
}

fn closure_key_string(
    host: &mut HostContext<'_>,
    graph: &ClosureKeyGraph,
) -> Result<Value, HeapError> {
    heap::with_heap(host, |heap| {
        heap.closure_key_value(graph.owner, graph.isolated)
    })
}

fn linked_key_table(
    host: &mut HostContext<'_>,
    graph: &ClosureKeyGraph,
) -> Result<Option<TableRef>, HeapError> {
    heap::with_heap(host, |heap| {
        heap.closure_key_edge(graph.owner, graph.linked)
    })
}

#[derive(Clone, Copy)]
struct ClosureKeyExpect {
    isolated_string: bool,
    isolated_alive: bool,
    linked_table: bool,
    linked_alive: bool,
    value_alive: bool,
    orphan_alive: bool,
    live_nodes: u32,
    live_edges: u32,
    tokens: usize,
}

fn assert_closure_key_graph(
    host: &mut HostContext<'_>,
    graph: &ClosureKeyGraph,
    expect: ClosureKeyExpect,
    label: &str,
) {
    let string = closure_key_string(host, graph);
    if expect.isolated_alive {
        assert_eq!(
            string.expect(label),
            Value::string("key-only"),
            "{label}: isolated closure key string"
        );
        assert!(
            heap::with_heap(host, |heap| Ok(heap.closure_alive(graph.isolated))).expect(label),
            "{label}: isolated closure stays"
        );
    } else {
        assert_eq!(string, Err(HeapError::StaleHandle), "{label}: isolated key");
        assert!(
            !heap::with_heap(host, |heap| Ok(heap.closure_alive(graph.isolated))).expect(label),
            "{label}: isolated closure reclaimed"
        );
    }
    assert_eq!(
        expect.isolated_string, expect.isolated_alive,
        "{label}: a dead closure key cannot keep its string entry"
    );

    let linked = linked_key_table(host, graph);
    if expect.linked_alive {
        assert_eq!(
            linked.expect(label),
            expect.linked_table.then_some(graph.value),
            "{label}: linked entry"
        );
        assert!(
            heap::with_heap(host, |heap| Ok(heap.closure_alive(graph.linked))).expect(label),
            "{label}: linked closure"
        );
    } else {
        assert_eq!(linked, Err(HeapError::StaleHandle), "{label}: linked key");
        assert!(!heap::with_heap(host, |heap| Ok(heap.closure_alive(graph.linked))).expect(label));
        assert!(!expect.linked_table, "{label}: a dead key has no entry");
    }

    assert_eq!(
        heap::with_heap(host, |heap| Ok(heap.alive(graph.value))).expect(label),
        expect.value_alive,
        "{label}: value table"
    );
    assert_eq!(
        token_open(host, graph.value_raw),
        expect.value_alive,
        "{label}: value token"
    );
    assert_eq!(
        heap::with_heap(host, |heap| heap.named_edge(graph.watch, "orphan")).expect(label),
        expect.orphan_alive.then_some(graph.orphan),
        "{label}: weak orphan entry"
    );
    assert_eq!(
        heap::with_heap(host, |heap| Ok(heap.alive(graph.orphan))).expect(label),
        expect.orphan_alive,
        "{label}: orphan node"
    );
    assert_eq!(
        token_open(host, graph.orphan_raw),
        expect.orphan_alive,
        "{label}: orphan token"
    );
    assert!(
        heap::with_heap(host, |heap| Ok(heap.alive(graph.survivor))).expect(label),
        "{label}: survival control"
    );
    assert!(
        token_open(host, graph.survivor_raw),
        "{label}: survivor token"
    );
    assert_eq!(
        heap::with_heap(host, |heap| heap.named_value(graph.survivor, "mark")).expect(label),
        Value::string("alive"),
        "{label}: survivor mark"
    );

    let stats = heap::stats(host).expect(label);
    assert_eq!(stats.live_nodes, expect.live_nodes, "{label}: nodes");
    assert_eq!(stats.live_edges, expect.live_edges, "{label}: edges");
    assert_eq!(stats.live_roots, 3, "{label}: roots");
    assert_eq!(
        host.resource_count() - graph.baseline_tokens,
        expect.tokens,
        "{label}: open table tokens"
    );
}

/// Closure identity keys on one heap, for `k` / `v` / `kv`, with and without
/// collection.
///
/// The oracle removes an isolated closure key in `k` and `kv`, keeps it in
/// `v`, and keeps a strong value's back-edge only in `k`. `v` marks that key
/// while dropping the weak value, so the first collect must not be read as
/// reclaiming the key. The next collect reclaims it. The rooted survivor and
/// the weak orphan separate that from a collect that did not run.
#[test]
fn closure_key_weak_modes_keep_or_drop_entries_and_reclaim_on_the_next_pass() {
    let seeded = ClosureKeyExpect {
        isolated_string: true,
        isolated_alive: true,
        linked_table: true,
        linked_alive: true,
        value_alive: true,
        orphan_alive: true,
        live_nodes: 9,
        live_edges: 8,
        tokens: 7,
    };
    for mode in ["k", "v", "kv"] {
        for collect in [false, true] {
            super::super::support::with_idle_vm(|vm| {
                let mut host = vm.host_context();
                let graph = seed_closure_keys(&mut host, mode);
                assert_closure_key_graph(&mut host, &graph, seeded, "before");
                if !collect {
                    assert_closure_key_graph(&mut host, &graph, seeded, "omit");
                    assert_eq!(heap::stats(&mut host).expect("omit").collections, 0);
                    return;
                }

                let report = heap::collect(&mut host).expect("collect");
                let meter = heap::take_scan_meter(&mut host).expect("one pass");
                assert_eq!(meter.mark_passes, 1, "{mode}: one mark pass");
                assert_eq!(report.live_roots, 3, "{mode}: roots stay");
                let after = match mode {
                    "k" => ClosureKeyExpect {
                        isolated_string: false,
                        isolated_alive: false,
                        linked_table: true,
                        linked_alive: true,
                        value_alive: true,
                        orphan_alive: false,
                        live_nodes: 7,
                        live_edges: 5,
                        tokens: 6,
                    },
                    "v" => ClosureKeyExpect {
                        isolated_string: true,
                        isolated_alive: true,
                        linked_table: false,
                        linked_alive: true,
                        value_alive: false,
                        orphan_alive: false,
                        live_nodes: 7,
                        live_edges: 4,
                        tokens: 5,
                    },
                    "kv" => ClosureKeyExpect {
                        isolated_string: false,
                        isolated_alive: false,
                        linked_table: false,
                        linked_alive: false,
                        value_alive: false,
                        orphan_alive: false,
                        live_nodes: 5,
                        live_edges: 3,
                        tokens: 5,
                    },
                    _ => unreachable!("mode is k, v, or kv"),
                };
                assert_closure_key_graph(&mut host, &graph, after, mode);
                if mode != "v" {
                    return;
                }

                let again = heap::collect(&mut host).expect("second collect");
                let second = heap::take_scan_meter(&mut host).expect("second pass");
                assert_eq!(second.mark_passes, 1, "the next collect is still one pass");
                assert_eq!(
                    again.live_nodes, 6,
                    "the unreferenced closure key is reclaimed"
                );
                assert_closure_key_graph(
                    &mut host,
                    &graph,
                    ClosureKeyExpect {
                        isolated_string: true,
                        isolated_alive: true,
                        linked_table: false,
                        linked_alive: false,
                        value_alive: false,
                        orphan_alive: false,
                        live_nodes: 6,
                        live_edges: 4,
                        tokens: 5,
                    },
                    "v next",
                );
            });
        }
    }
}

/// Two closures can share one cell and still be different table keys.
/// Deleting one entry leaves the other. A stale, foreign, or full-edge write
/// is refused before it replaces a stored entry.
#[test]
fn closure_keys_sharing_a_cell_stay_distinct_and_refused_writes_keep_entries() {
    let mut heap = heap::LuaHeap::new(HeapLimits {
        nodes: 10,
        roots: 1,
        edges: 4,
        max_generation: 4,
    });
    let owner = heap.alloc().expect("owner");
    let table_value = heap.alloc().expect("table value");
    let cell = heap.alloc_upvalue().expect("cell");
    let left = heap.alloc_closure().expect("left");
    let right = heap.alloc_closure().expect("right");
    heap.set_upvalue_edge(left, cell).expect("left cell");
    heap.set_upvalue_edge(right, cell).expect("right cell");
    assert_eq!(heap.upvalue_of(left).expect("left"), cell);
    assert_eq!(heap.upvalue_of(right).expect("right"), cell);
    assert_ne!(left, right);
    assert_ne!(left.slot(), right.slot());
    assert_eq!(left.domain(), right.domain());
    assert_eq!(left.kind(), heap::HandleKind::Closure);

    let frame = heap.enter_roots(1).expect("frame");
    heap.write_root(frame, 0, owner).expect("root");
    heap.set_closure_key_scalar(owner, left, Value::string("left"))
        .expect("left string");
    heap.set_closure_key_edge(owner, right, table_value)
        .expect("right table");
    heap.issue_carrier(table_value)
        .expect("issue the table value");
    assert_eq!(
        heap.closure_key_value(owner, left).expect("read left"),
        Value::string("left")
    );
    assert_eq!(
        heap.closure_key_edge(owner, right).expect("read right"),
        Some(table_value)
    );
    assert_eq!(
        heap.closure_key_value(owner, right).expect("issued read"),
        heap.issued_value(table_value).expect("carrier")
    );
    assert_eq!(
        heap.closure_key_edge(owner, left)
            .expect("scalar is not a table"),
        None
    );
    assert_eq!(heap.stats().live_edges, 4);

    let table_key = heap.alloc().expect("table key");
    assert_eq!(
        heap.object_value(owner, table_key).expect("table key"),
        Value::Null,
        "a table key does not select a closure key"
    );
    assert_eq!(
        heap.edge(owner, left.slot()).expect("field id"),
        None,
        "the closure slot is not a field key"
    );

    let extra = heap.alloc_closure().expect("extra");
    let before = heap.stats();
    assert_eq!(
        heap.set_closure_key_scalar(owner, extra, Value::string("extra")),
        Err(HeapError::EdgeCapacity)
    );
    assert_eq!(heap.stats().live_edges, before.live_edges);
    assert_eq!(heap.stats().edge_slots, before.edge_slots);
    assert_eq!(
        heap.closure_key_value(owner, left)
            .expect("capacity left the string"),
        Value::string("left")
    );
    assert_eq!(
        heap.closure_key_edge(owner, right)
            .expect("capacity left the table"),
        Some(table_value)
    );

    heap.clear_closure_key(owner, left).expect("delete left");
    assert_eq!(
        heap.closure_key_value(owner, left).expect("deleted"),
        Value::Null
    );
    assert_eq!(
        heap.closure_key_edge(owner, right).expect("right stays"),
        Some(table_value)
    );
    assert!(heap.closure_alive(left));
    assert!(heap.closure_alive(right));
    assert_eq!(heap.upvalue_of(left).expect("cell stays"), cell);
    assert_eq!(heap.upvalue_of(right).expect("shared cell"), cell);
    heap.set_closure_key_scalar(owner, left, Value::string("left"))
        .expect("restore left");

    let doomed = heap.alloc().expect("doomed table");
    let soon = heap.alloc_closure().expect("soon stale");
    let soon_slot = soon.slot();
    let soon_generation = soon.generation();
    assert_eq!(heap.collect().live_nodes, 5, "unkeyed nodes are swept");
    assert!(!heap.alive(doomed));
    assert!(!heap.closure_alive(soon));
    assert!(!heap.closure_alive(extra));
    assert!(!heap.alive(table_key));
    let reused = heap.alloc_closure().expect("reuse");
    assert_eq!(reused.slot(), soon_slot);
    assert_ne!(reused.generation(), soon_generation);
    assert_eq!(reused.kind(), heap::HandleKind::Closure);
    assert_eq!(
        heap.set_closure_key_edge(owner, right, doomed),
        Err(HeapError::StaleHandle)
    );
    assert_eq!(
        heap.set_closure_key_scalar(owner, soon, Value::string("stale")),
        Err(HeapError::StaleHandle)
    );
    assert_eq!(
        heap.closure_key_value(owner, left)
            .expect("stale write kept left"),
        Value::string("left")
    );
    assert_eq!(
        heap.closure_key_edge(owner, right)
            .expect("stale write kept right"),
        Some(table_value)
    );

    let mut other = heap::LuaHeap::new(HeapLimits {
        nodes: 2,
        roots: 1,
        edges: 1,
        max_generation: 4,
    });
    let foreign = other.alloc_closure().expect("foreign");
    assert_ne!(foreign.domain(), owner.domain());
    assert_eq!(foreign.kind(), heap::HandleKind::Closure);
    let foreign_table = other.alloc().expect("foreign table");
    assert_eq!(
        heap.set_closure_key_scalar(owner, foreign, Value::string("foreign")),
        Err(HeapError::WrongDomain)
    );
    assert_eq!(
        heap.set_closure_key_edge(owner, right, foreign_table),
        Err(HeapError::WrongDomain)
    );
    assert_eq!(
        heap.clear_closure_key(owner, foreign),
        Err(HeapError::WrongDomain)
    );
    assert_eq!(
        heap.closure_key_value(owner, foreign),
        Err(HeapError::WrongDomain)
    );
    assert_eq!(heap.stats().live_edges, 4);
    assert_eq!(
        heap.closure_key_value(owner, left)
            .expect("foreign write kept left"),
        Value::string("left")
    );
    assert_eq!(
        heap.closure_key_edge(owner, right)
            .expect("foreign write kept right"),
        Some(table_value)
    );
    assert_eq!(heap.collect().live_nodes, 5);
}

fn shared_current(
    host: &mut HostContext<'_>,
    left: ClosureRef,
    right: ClosureRef,
) -> Option<CellValue> {
    let through_left = heap::with_heap(host, |heap| heap.read_cell(left)).expect("left cell");
    let through_right = heap::with_heap(host, |heap| heap.read_cell(right)).expect("right cell");
    assert!(
        same_recorded_cell(&through_left, &through_right),
        "both closures read the same current edge: {through_left:?} vs {through_right:?}"
    );
    through_left
}

fn same_recorded_cell(left: &Option<CellValue>, right: &Option<CellValue>) -> bool {
    match (left, right) {
        (
            Some(CellValue::Scalar(Value::Float(left))),
            Some(CellValue::Scalar(Value::Float(right))),
        ) => left.to_bits() == right.to_bits(),
        _ => left == right,
    }
}

/// Two closures share one cell. `false` replaces that cell's only object edge
/// in the same record. The old self-cycle stays live until the next collect.
#[test]
fn shared_upvalue_scalar_replaces_object_edge_and_collect_reclaims_only_that_cycle() {
    super::super::support::with_idle_vm(|vm| {
        let mut host = vm.host_context();
        heap::install(
            &mut host,
            HeapLimits {
                nodes: 6,
                roots: 2,
                edges: 6,
                max_generation: 8,
            },
        )
        .expect("install");
        let root_raw = publish_raw(&mut host);
        let root = table_ref(&host, root_raw);
        let independent_raw = publish_raw(&mut host);
        let independent = table_ref(&host, independent_raw);
        let frame = heap::with_heap(&mut host, |heap| heap.enter_roots(2)).expect("two roots");
        heap::with_heap(&mut host, |heap| heap.write_root(frame, 0, root)).expect("root the owner");
        heap::with_heap(&mut host, |heap| heap.write_root(frame, 1, independent))
            .expect("root the survivor");

        let (left, right, cell) = heap::with_heap(&mut host, |heap| {
            let left = heap.alloc_closure()?;
            let right = heap.alloc_closure()?;
            let cell = heap.alloc_upvalue()?;
            heap.set_closure_edge(root, LEFT_KEY, left)?;
            heap.set_closure_edge(root, RIGHT_KEY, right)?;
            heap.set_upvalue_edge(left, cell)?;
            heap.set_upvalue_edge(right, cell)?;
            Ok((left, right, cell))
        })
        .expect("two closures, one cell");
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.upvalue_of(left)).expect("left cell"),
            cell
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.upvalue_of(right)).expect("right cell"),
            cell,
            "both closures name one upvalue"
        );

        let original_raw = publish_raw(&mut host);
        let original = table_ref(&host, original_raw);
        heap::with_heap(&mut host, |heap| heap.write_upvalue(cell, original))
            .expect("capture the table");
        heap::with_heap(&mut host, |heap| {
            heap.set_edge(original, BACK_KEY, original)
        })
        .expect("self-cycle");
        let original_carrier = issue(&mut host, original);
        let independent_carrier = issue(&mut host, independent);
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.read_upvalue(left)).expect("left object"),
            original
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.read_upvalue(right)).expect("right object"),
            original,
            "the live root keeps the shared object readable"
        );
        let linked = heap::stats(&mut host).expect("linked");
        assert_eq!(linked.live_nodes, 6, "three tables, two closures, one cell");
        assert_eq!(
            linked.live_edges, 6,
            "two fields, two closure links, one current edge, one self-edge"
        );
        assert_eq!(
            linked.edge_slots, 6,
            "the edge cap is the current record count"
        );

        heap::with_heap(&mut host, |heap| {
            heap.write_scalar_upvalue(cell, Value::Bool(false))
        })
        .expect("replace the object edge with false");
        assert_eq!(
            heap::stats(&mut host).expect("overwrite").live_edges,
            linked.live_edges,
            "a scalar overwrite does not add an edge"
        );
        assert_eq!(
            heap::stats(&mut host).expect("slots").edge_slots,
            linked.edge_slots
        );
        assert_eq!(
            shared_current(&mut host, left, right),
            Some(CellValue::Scalar(Value::Bool(false)))
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.read_upvalue(left)).expect_err("not a table"),
            HeapError::WrongKind
        );

        // Uncollected control. The old cycle is still live before a real collect.
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(original))).expect("still live"));
        assert!(
            token_open(&host, original_raw),
            "token stays open without collect"
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.guest_class(&original_carrier)))
                .expect("class"),
            heap::GuestClass::Table(original)
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.edge(original, BACK_KEY)).expect("self-edge"),
            Some(original)
        );

        let before_tokens = host.resource_count();
        assert_eq!(before_tokens, 3, "root, survivor, and the captured table");
        let report = heap::collect(&mut host).expect("collect the replaced cycle");
        let meter = heap::take_scan_meter(&mut host).expect("meter");
        assert_eq!(meter.mark_passes, 1, "the existing collector marks once");
        assert_eq!(
            meter.outgoing_edge_checks, 5,
            "mark walks the rooted edges and does not follow the scalar"
        );
        assert_eq!(
            meter.index_edge_checks,
            u64::from(linked.live_edges) * 2,
            "index walks the same edge vector twice"
        );
        assert_eq!(report.live_nodes, 5, "only the self-cycle table is swept");
        assert!(!heap::with_heap(&mut host, |heap| Ok(heap.alive(original))).expect("swept"));
        assert!(
            !token_open(&host, original_raw),
            "the captured table token is closed"
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| Ok(heap.guest_class(&original_carrier)))
                .expect("class"),
            heap::GuestClass::Unissued
        );
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(root))).expect("root"));
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.closure_alive(left))).expect("left"));
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.closure_alive(right))).expect("right"));
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.upvalue_alive(cell))).expect("cell"));
        assert!(heap::with_heap(&mut host, |heap| Ok(heap.alive(independent))).expect("survivor"));
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.upvalue_of(left)).expect("left still shared"),
            cell
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.upvalue_of(right)).expect("right still shared"),
            cell
        );
        assert!(token_open(&host, root_raw));
        assert!(token_open(&host, independent_raw));
        assert_eq!(host.resource_count(), before_tokens - 1);
        assert_eq!(
            heap::stats(&mut host).expect("after sweep").live_edges,
            linked.live_edges - 1,
            "sweep drops the self-edge and keeps the current scalar record"
        );
        assert_eq!(
            shared_current(&mut host, left, right),
            Some(CellValue::Scalar(Value::Bool(false)))
        );

        let neg_zero = -0.0_f64;
        heap::with_heap(&mut host, |heap| {
            heap.write_scalar_upvalue(cell, Value::Float(neg_zero))
        })
        .expect("store -0");
        match shared_current(&mut host, left, right) {
            Some(CellValue::Scalar(Value::Float(stored))) => {
                assert_eq!(
                    stored.to_bits(),
                    neg_zero.to_bits(),
                    "signed zero is stored as given"
                );
                assert_ne!(stored.to_bits(), 0.0_f64.to_bits());
                assert_eq!(1.0 / stored, f64::NEG_INFINITY);
            }
            other => panic!("expected -0, got {other:?}"),
        }
        assert_eq!(
            heap::stats(&mut host).expect("-0 edges").live_edges,
            linked.live_edges - 1
        );

        let nan_bits = 0x7ff8_0000_0000_0001_u64;
        let nan = f64::from_bits(nan_bits);
        heap::with_heap(&mut host, |heap| {
            heap.write_scalar_upvalue(cell, Value::Float(nan))
        })
        .expect("store NaN");
        match shared_current(&mut host, left, right) {
            Some(CellValue::Scalar(Value::Float(stored))) => {
                assert_eq!(stored.to_bits(), nan_bits, "NaN bits are not rewritten");
                assert!(stored.is_nan());
            }
            other => panic!("expected NaN, got {other:?}"),
        }
        assert_eq!(
            heap::stats(&mut host).expect("NaN edges").live_edges,
            linked.live_edges - 1
        );

        heap::with_heap(&mut host, |heap| heap.clear_upvalue(cell)).expect("nil");
        assert_eq!(shared_current(&mut host, left, right), None);
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.read_upvalue(left)).expect_err("nil"),
            HeapError::StaleHandle
        );
        assert_eq!(
            heap::stats(&mut host).expect("cleared").live_edges,
            linked.live_edges - 2,
            "nil deletes the current record"
        );

        heap::with_heap(&mut host, |heap| heap.write_upvalue(cell, independent))
            .expect("write the object back onto the same edge");
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.read_upvalue(left)).expect("left object"),
            independent
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.read_upvalue(right)).expect("right object"),
            independent
        );
        let kept = shared_current(&mut host, left, right);
        let edges_kept = heap::stats(&mut host).expect("restored").live_edges;
        assert_eq!(edges_kept, linked.live_edges - 1);
        for value in [
            Value::Null,
            Value::Int(1),
            Value::string("no"),
            independent_carrier,
        ] {
            assert_eq!(
                heap::with_heap(&mut host, |heap| heap.write_scalar_upvalue(cell, value))
                    .expect_err("unsupported value"),
                HeapError::WrongKind
            );
        }
        assert_eq!(shared_current(&mut host, left, right), kept);
        assert_eq!(
            heap::stats(&mut host).expect("refusal").live_edges,
            edges_kept,
            "a refused write does not add or delete an edge"
        );
        assert_eq!(
            heap::with_heap(&mut host, |heap| heap.read_upvalue(right)).expect("not clobbered"),
            independent
        );
    });
}
