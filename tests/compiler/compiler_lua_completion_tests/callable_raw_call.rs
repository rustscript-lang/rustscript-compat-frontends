//! The original Lua closure registration handed into one existing raw
//! `__call` record, through the real file entry.
//!
//! One real `.lua` file is fed through the real lexer, parser, resolver and
//! private completion lowering, compiled through the fixed Core and run in one
//! interpreter VM with JIT disabled. The fixture issues one live heap table
//! with a metatable whose raw `__call` starts as `false`, then hands two real
//! zero-capture guest closures to the heap-owned fixed two-argument `note`
//! import. The first call replaces the sentinel, the second replaces the
//! record's existing closure generation.
//!
//! Every host import in the fixture returns only an issued table or a scalar;
//! no host returns a callable. The two callables come from the real guest, and
//! the heap owner checks the exact issued array Arc with `guest_class`.
//!
//! The run itself records a before/after picture for each arm: raw `__call`
//! tag, owner and metatable slot generations, `HeapStats`, and the number of
//! admitted closures. The captured, same-shaped unissued, missing-record,
//! true-scalar, and stored-gap arms must leave every picture byte-for-byte
//! equal after the previous successful replacement. After the run the test
//! reads the actual raw edge, its full domain/kind/slot/generation and
//! `Arc::ptr_eq` against the second guest Arc, roots only the owner, runs the
//! real collector, and checks the old generation becomes stale while the new
//! generation stays alive. It then removes the last edge and checks that the
//! slot releases exactly its one Arc.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use vm::compiler::HostCallResolver;
use vm::{
    CallOutcome, CallReturn, CallableValue, CompileSourceFileOptions, FrontendImportSyntax,
    FrontendIr, HostApiBuilder, HostApiCatalog, HostFunctionRegistry, HostFunctionSchema,
    HostImportSchema, HostParamSchema, HostTypeSchema, JitConfig, ParseError, SourceFlavor,
    SourcePlugin, Value, Vm, VmError, VmStatus,
};

use super::{FUEL_BUDGET, FUEL_CHECK_INTERVAL, lower};
use crate::heap::{self, GuestClass, HandleKind, HeapError, HeapStats, LuaHeap, TableRef};

const FIXTURE: &str = "callable_raw_call.lua";
const RAW_CALL_FIELD: &str = "__call";

#[derive(Clone, Debug, PartialEq, Eq)]
struct OwnerRow {
    label: String,
    owner_generation: Option<u32>,
    metatable_generation: Option<u32>,
    raw_call: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Picture {
    stats: HeapStats,
    admissions: usize,
    rows: Vec<OwnerRow>,
}

struct OwnerRecord {
    label: String,
    owner: TableRef,
    metatable: TableRef,
    metatable_value: Value,
}

#[derive(Default)]
struct Witness {
    owners: Vec<OwnerRecord>,
    positive_owner: Option<TableRef>,
    pictures: Vec<(String, Picture)>,
}

static WITNESS: Mutex<Witness> = Mutex::new(Witness {
    owners: Vec::new(),
    positive_owner: None,
    pictures: Vec::new(),
});

fn with_witness<T>(body: impl FnOnce(&mut Witness) -> T) -> T {
    let mut guard = WITNESS.lock().expect("raw call field witness");
    body(&mut guard)
}

impl Witness {
    fn record(&self, label: &str) -> &OwnerRecord {
        self.owners
            .iter()
            .find(|record| record.label == label)
            .unwrap_or_else(|| panic!("raw call field: missing owner arm '{label}'"))
    }

    fn picture(&self, label: &str) -> &Picture {
        self.pictures
            .iter()
            .find(|(stored, _)| stored == label)
            .map(|(_, picture)| picture)
            .unwrap_or_else(|| panic!("raw call field: missing picture '{label}'"))
    }
}

fn raw_call_catalog() -> &'static Arc<HostApiCatalog> {
    static CATALOG: OnceLock<Arc<HostApiCatalog>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut builder = HostApiBuilder::new();
        builder.function(HostFunctionSchema::with_return(
            "mark",
            vec![HostParamSchema::value("label", HostTypeSchema::String)],
            HostTypeSchema::Int,
        ));
        builder.function(HostFunctionSchema::with_return(
            "tick",
            vec![HostParamSchema::value("label", HostTypeSchema::String)],
            HostTypeSchema::Unknown,
        ));
        // The heap owns this schema and dispatch. The test adds the fixed
        // two-argument shape to its compile catalog; it does not define the
        // handoff behavior.
        builder.function(LuaHeap::guest_callable_field_import_schema());
        Arc::new(builder.build().expect("raw call field catalog"))
    })
}

fn catalog_schema(name: &str) -> HostImportSchema {
    let catalog = raw_call_catalog();
    let function = catalog
        .function(name)
        .unwrap_or_else(|| panic!("raw call field catalog is missing '{name}'"));
    HostImportSchema::from_function(catalog, function)
}

fn bind_hosts(vm: &mut Vm) -> Result<(), VmError> {
    let mut registry = HostFunctionRegistry::empty();
    registry.register_exact_static("mark", 1, catalog_schema("mark"), host_mark)?;
    registry.register_exact_static("tick", 1, catalog_schema("tick"), host_tick)?;
    LuaHeap::register_guest_callable_field_import(
        &mut registry,
        catalog_schema(LuaHeap::GUEST_CALLABLE_IMPORT),
    )?;
    registry.bind_vm_cached(vm)
}

fn host_fault(message: impl Into<String>) -> VmError {
    VmError::HostError(message.into())
}

fn heap_fault(error: HeapError) -> VmError {
    VmError::HostError(format!("lua heap: {error:?}"))
}

fn raw_call_tag(heap: &LuaHeap, metatable: TableRef) -> String {
    match heap.named_raw_scalar(metatable, RAW_CALL_FIELD) {
        Ok(Some(Value::Bool(false))) => "false".to_string(),
        Ok(Some(Value::Bool(true))) => "true".to_string(),
        Ok(Some(Value::Null)) => "gap".to_string(),
        Ok(None) => "missing".to_string(),
        Ok(Some(other)) => format!("scalar({other:?})"),
        Err(_) => match heap.named_closure_edge(metatable, RAW_CALL_FIELD) {
            Ok(closure) => format!(
                "closure({},{},{})",
                closure.domain(),
                closure.slot(),
                closure.generation()
            ),
            Err(_) => "other".to_string(),
        },
    }
}

/// Records one arm picture. The rows are the real raw `__call` tags and
/// generations of every owner this fixture issued; the counts come from the
/// live heap. This is observation only and does not participate in a handoff.
fn host_mark(vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
    let [Value::String(label)] = args else {
        return Err(host_fault("mark expects one string label"));
    };
    let label = label.as_str().to_string();
    let owners: Vec<(String, TableRef, TableRef)> = with_witness(|witness| {
        witness
            .owners
            .iter()
            .map(|record| (record.label.clone(), record.owner, record.metatable))
            .collect()
    });
    let picture = {
        let mut host = vm.host_context();
        heap::with_heap(&mut host, |heap| {
            let mut rows = Vec::with_capacity(owners.len());
            for (owner_label, owner, metatable) in &owners {
                rows.push(OwnerRow {
                    label: owner_label.clone(),
                    owner_generation: heap.slot_generation(owner.slot()),
                    metatable_generation: heap.slot_generation(metatable.slot()),
                    raw_call: raw_call_tag(heap, *metatable),
                });
            }
            Ok(Picture {
                stats: heap.stats(),
                admissions: heap.guest_callable_admissions().len(),
                rows,
            })
        })
        .map_err(heap_fault)?
    };
    with_witness(|witness| witness.pictures.push((label, picture)));
    Ok(CallOutcome::Return(CallReturn::one(Value::Int(0))))
}

/// Issues one real heap table and attaches a metatable with the requested raw
/// `__call` arm. Only `owner` starts with the false sentinel; `missing` has no
/// record, `true` has a true scalar, and `gap` has a stored nil gap.
fn host_tick(vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
    let [Value::String(label)] = args else {
        return Err(host_fault("tick expects one string label"));
    };
    let label = label.as_str().to_string();
    if label == "owner-slot" || label == "owner-generation" {
        let owner = with_witness(|witness| {
            witness
                .positive_owner
                .expect("the owner tick must run before a slot field")
        });
        let value = if label == "owner-slot" {
            Value::Int(i64::from(owner.slot()))
        } else {
            Value::Int(i64::from(owner.generation()))
        };
        return Ok(CallOutcome::Return(CallReturn::one(value)));
    }
    let (issued, owner, metatable, metatable_value) = {
        let mut host = vm.host_context();
        let issued = heap::publish_guest(&mut host).map_err(heap_fault)?;
        let class =
            heap::with_heap(&mut host, |heap| Ok(heap.guest_class(&issued))).map_err(heap_fault)?;
        let GuestClass::Table(owner) = class else {
            return Err(host_fault("the issued owner is not a live heap table"));
        };
        let metatable = heap::with_heap(&mut host, LuaHeap::alloc).map_err(heap_fault)?;
        heap::with_heap(&mut host, |heap| heap.set_metatable(owner, Some(metatable)))
            .map_err(heap_fault)?;
        let metatable_value =
            heap::with_heap(&mut host, |heap| heap.issue_carrier(metatable)).map_err(heap_fault)?;
        match label.as_str() {
            "owner" => heap::with_heap(&mut host, |heap| {
                heap.set_named_scalar(metatable, RAW_CALL_FIELD, Value::Bool(false))
            })
            .map_err(heap_fault)?,
            "missing" => {}
            "true" => heap::with_heap(&mut host, |heap| {
                heap.set_named_scalar(metatable, RAW_CALL_FIELD, Value::Bool(true))
            })
            .map_err(heap_fault)?,
            "gap" => heap::with_heap(&mut host, |heap| {
                heap.set_named_gap_test_control(metatable, RAW_CALL_FIELD)
            })
            .map_err(heap_fault)?,
            other => {
                return Err(host_fault(format!(
                    "tick label '{other}' is not a fixture arm"
                )));
            }
        }
        (issued, owner, metatable, metatable_value)
    };
    with_witness(|witness| {
        if label == "owner" {
            witness.positive_owner = Some(owner);
        }
        witness.owners.push(OwnerRecord {
            label,
            owner,
            metatable,
            metatable_value,
        });
    });
    Ok(CallOutcome::Return(CallReturn::one(issued)))
}

struct RawCallPlugin;

static RAW_CALL_PLUGIN: RawCallPlugin = RawCallPlugin;

impl SourcePlugin for RawCallPlugin {
    fn flavor(&self) -> SourceFlavor {
        SourceFlavor::Lua
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["lua"]
    }

    fn import_syntax(&self) -> FrontendImportSyntax {
        FrontendImportSyntax::Lua
    }

    fn parse_source(&self, source: &str) -> Result<FrontendIr, ParseError> {
        let resolver = HostCallResolver::new(raw_call_catalog());
        lower::lower_source_with(source, Some(&resolver))
    }
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("luajit")
        .join("finalizer")
        .join(FIXTURE)
}

struct RawCallRun {
    vm: Vm,
    status: Result<VmStatus, VmError>,
    stack: Vec<Value>,
    witness: Witness,
    fuel_consumed: u64,
}

fn run_fixture() -> RawCallRun {
    *WITNESS.lock().expect("raw call field witness") = Witness::default();

    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&RAW_CALL_PLUGIN)
        .with_host_api_catalog(Arc::clone(raw_call_catalog()));
    let compiled = vm::compile_source_file_with_options(fixture_path(), options)
        .unwrap_or_else(|error| panic!("raw call field: compile failed: {error:?}"));

    let mut runtime = Vm::new_with_jit_config(
        compiled.program,
        JitConfig {
            enabled: false,
            hot_loop_threshold: 8,
            max_trace_len: 256,
        },
    );
    bind_hosts(&mut runtime).unwrap_or_else(|error| panic!("raw call field: bind failed: {error}"));
    runtime
        .set_fuel_check_interval(FUEL_CHECK_INTERVAL)
        .unwrap_or_else(|error| panic!("raw call field: fuel interval: {error}"));
    runtime.set_fuel(FUEL_BUDGET);
    let status = runtime.run();
    let remaining = runtime
        .get_fuel()
        .unwrap_or_else(|| panic!("raw call field: fuel metering missing"));
    let fuel_consumed = FUEL_BUDGET
        .checked_sub(remaining)
        .unwrap_or_else(|| panic!("raw call field: fuel increased"));
    let stack = runtime.stack().to_vec();
    let witness = std::mem::take(&mut *WITNESS.lock().expect("raw call field witness"));
    RawCallRun {
        vm: runtime,
        status,
        stack,
        witness,
        fuel_consumed,
    }
}

/// `[true, owner, first, second, captured, lookalike, missing_owner,
/// true_owner, gap_owner, first_ok, second_ok, captured_ok, unissued_ok,
/// missing_ok, true_ok, gap_ok]`.
fn exit_items(stack: &[Value]) -> &[Value] {
    let [Value::Array(items)] = stack else {
        panic!("raw call field: expected the one completion array, got {stack:?}");
    };
    items.as_slice()
}

fn callable_item(value: &Value) -> Arc<CallableValue> {
    match value {
        Value::Callable(callable) => Arc::clone(callable),
        other => panic!("raw call field: expected a callable, got {other:?}"),
    }
}

fn same_callable(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Callable(left), Value::Callable(right)) => Arc::ptr_eq(left, right),
        _ => false,
    }
}

fn same_array(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Array(left), Value::Array(right)) => Arc::ptr_eq(left, right),
        _ => false,
    }
}

fn heap_with<T>(vm: &mut Vm, body: impl FnOnce(&mut LuaHeap) -> Result<T, HeapError>) -> T {
    let mut host = vm.host_context();
    heap::with_heap(&mut host, body)
        .unwrap_or_else(|error| panic!("raw call field heap: {error:?}"))
}

fn heap_try<T>(
    vm: &mut Vm,
    body: impl FnOnce(&mut LuaHeap) -> Result<T, HeapError>,
) -> Result<T, HeapError> {
    let mut host = vm.host_context();
    heap::with_heap(&mut host, body)
}

fn heap_collect(vm: &mut Vm) -> heap::CollectReport {
    let mut host = vm.host_context();
    heap::collect(&mut host).unwrap_or_else(|error| panic!("raw call field collect: {error:?}"))
}

fn closure_tag(closure: crate::heap::ClosureRef) -> String {
    format!(
        "closure({},{},{})",
        closure.domain(),
        closure.slot(),
        closure.generation()
    )
}

fn prototype_captures(vm: &Vm, callable: &Arc<CallableValue>) -> usize {
    vm.program()
        .callable_prototypes
        .get(callable.prototype_id as usize)
        .unwrap_or_else(|| {
            panic!(
                "raw call field: prototype {} is outside this VM",
                callable.prototype_id
            )
        })
        .capture_slots
        .len()
}

#[test]
fn raw_call_edge_receives_two_guest_closures_and_releases_the_old_generation() {
    let mut run = run_fixture();
    assert!(
        matches!(run.status, Ok(VmStatus::Halted)),
        "raw call field: the fixture must halt, got {:?}",
        run.status
    );
    assert!(
        run.fuel_consumed > 0 && run.fuel_consumed < FUEL_BUDGET,
        "raw call field: expected real fuel use, saw {}",
        run.fuel_consumed
    );

    let expected_labels: Vec<String> = vec![
        "first-before",
        "first-after",
        "second-after",
        "capture-before",
        "capture-after",
        "unissued-before",
        "unissued-after",
        "missing-before",
        "missing-after",
        "true-before",
        "true-after",
        "gap-before",
        "gap-after",
        "done",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    let seen_labels: Vec<String> = run
        .witness
        .pictures
        .iter()
        .map(|(label, _)| label.clone())
        .collect();
    assert_eq!(
        seen_labels, expected_labels,
        "raw call field: every fixture arm must be observed once in order"
    );

    let items = exit_items(&run.stack);
    assert_eq!(
        items.len(),
        16,
        "raw call field: [true] plus seven values and seven results"
    );
    assert_eq!(items[0], Value::Bool(true));
    let owner_value = &items[1];
    let first = callable_item(&items[2]);
    let second = callable_item(&items[3]);
    let captured = callable_item(&items[4]);
    let lookalike = &items[5];
    let missing_owner = &items[6];
    let true_owner = &items[7];
    let gap_owner = &items[8];
    let (first_ok, second_ok, captured_ok, unissued_ok, missing_ok, true_ok, gap_ok) = match (
        &items[9], &items[10], &items[11], &items[12], &items[13], &items[14], &items[15],
    ) {
        (
            Value::Bool(first_ok),
            Value::Bool(second_ok),
            Value::Bool(captured_ok),
            Value::Bool(unissued_ok),
            Value::Bool(missing_ok),
            Value::Bool(true_ok),
            Value::Bool(gap_ok),
        ) => (
            *first_ok,
            *second_ok,
            *captured_ok,
            *unissued_ok,
            *missing_ok,
            *true_ok,
            *gap_ok,
        ),
        other => panic!("raw call field: expected seven boolean results, got {other:?}"),
    };
    assert!(
        first_ok && second_ok,
        "raw call field: the two zero-capture replacements must be accepted"
    );
    assert!(
        !captured_ok,
        "raw call field: a captured callable must be refused"
    );
    assert!(
        !unissued_ok,
        "raw call field: a same-shaped unissued owner must be refused"
    );
    assert!(
        !missing_ok,
        "raw call field: a metatable without a __call record must be refused"
    );
    assert!(
        !true_ok,
        "raw call field: a true-valued __call record must be refused"
    );
    assert!(!gap_ok, "raw call field: a stored nil gap must be refused");
    assert!(
        !same_callable(&items[2], &items[3]),
        "raw call field: the two guest closures must be distinct Core Arcs"
    );
    assert!(
        !same_callable(&items[2], &items[4]),
        "raw call field: the captured callable is a third Core Arc"
    );
    let first_captures = prototype_captures(&run.vm, &first);
    let second_captures = prototype_captures(&run.vm, &second);
    let captured_captures = prototype_captures(&run.vm, &captured);
    eprintln!(
        "LUA_RAW_CALL_FIELD captures first={first_captures} second={second_captures} captured={captured_captures} env_first={} env_second={} env_captured={}",
        first.env.is_some(),
        second.env.is_some(),
        captured.env.is_some()
    );
    assert_eq!(
        first_captures, 0,
        "raw call field: the first guest closure must have no captures"
    );
    assert_eq!(
        second_captures, 0,
        "raw call field: the second guest closure must have no captures"
    );
    assert_eq!(
        captured_captures, 1,
        "raw call field: the refusal control must have one real capture"
    );
    assert!(
        first.env.is_some() && second.env.is_some() && captured.env.is_some(),
        "raw call field: Core attaches an empty environment container to every closure"
    );

    let positive = run.witness.record("owner");
    let class = heap_with(&mut run.vm, |heap| Ok(heap.guest_class(owner_value)));
    let GuestClass::Table(owner) = class else {
        panic!("raw call field: the returned owner must be the issued heap table, saw {class:?}");
    };
    assert_eq!(owner, positive.owner);
    let issued = heap_with(&mut run.vm, |heap| heap.issued_value(owner));
    assert!(
        same_array(&issued, owner_value),
        "raw call field: the owner value must be the slot's exact issued array Arc"
    );
    let metatable = heap_with(&mut run.vm, |heap| heap.metatable(owner))
        .expect("raw call field: the fixture owner must have a metatable");
    let metatable_issued = heap_with(&mut run.vm, |heap| heap.issued_value(metatable));
    assert!(
        same_array(&metatable_issued, &positive.metatable_value),
        "raw call field: the metatable must stay the issued metatable carrier"
    );
    let metatable_class = heap_with(&mut run.vm, |heap| {
        Ok(heap.guest_class(&positive.metatable_value))
    });
    assert_eq!(
        metatable_class,
        GuestClass::Table(metatable),
        "raw call field: the recorded metatable carrier must be that live table"
    );

    let lookalike_class = heap_with(&mut run.vm, |heap| Ok(heap.guest_class(lookalike)));
    assert_eq!(
        lookalike_class,
        GuestClass::Unissued,
        "raw call field: the same-shaped lookalike must not be an issued table"
    );
    assert!(
        !same_array(lookalike, owner_value),
        "raw call field: the lookalike array is a distinct Arc"
    );
    match (lookalike, owner_value) {
        (Value::Array(left), Value::Array(right)) => {
            assert_eq!(
                left.as_slice(),
                right.as_slice(),
                "raw call field: the lookalike has the owner's exact slot and generation fields"
            );
        }
        other => panic!("raw call field: expected two arrays, got {other:?}"),
    }

    for owner_value in [missing_owner, true_owner, gap_owner] {
        let class = heap_with(&mut run.vm, |heap| Ok(heap.guest_class(owner_value)));
        assert!(
            matches!(class, GuestClass::Table(_)),
            "raw call field: every fixture owner must be issued before its refusal arm"
        );
    }

    let final_handle = heap_try(&mut run.vm, |heap| {
        heap.named_closure_edge(metatable, RAW_CALL_FIELD)
    })
    .expect("raw call field: the raw __call record must be a closure after the run");
    assert_eq!(
        final_handle.domain(),
        owner.domain(),
        "raw call field: the raw edge names this heap domain"
    );
    assert_eq!(
        final_handle.kind(),
        HandleKind::Closure,
        "raw call field: the raw edge kind is a closure"
    );
    assert!(final_handle.slot() < heap_with(&mut run.vm, |heap| Ok(heap.stats().node_slots)));
    assert!(final_handle.generation() > 0);
    let admissions: Vec<crate::heap::ClosureRef> = heap_with(&mut run.vm, |heap| {
        Ok(heap.guest_callable_admissions().to_vec())
    });
    assert_eq!(
        admissions.len(),
        2,
        "raw call field: only the two successful replacements may allocate a generation"
    );
    assert_eq!(
        admissions[1], final_handle,
        "raw call field: the final raw edge must be the second admitted generation"
    );
    assert_ne!(
        admissions[0], final_handle,
        "raw call field: the second replacement must allocate a new generation"
    );
    let stored = heap_with(&mut run.vm, |heap| heap.closure_callable(final_handle));
    assert!(
        Arc::ptr_eq(&stored, &second),
        "raw call field: the raw edge generation must return the exact second guest Arc"
    );
    assert!(
        !Arc::ptr_eq(&stored, &first),
        "raw call field: the raw edge must not return the replaced first Arc"
    );
    assert!(
        !Arc::ptr_eq(&stored, &captured),
        "raw call field: the refused captured Arc must never be stored"
    );

    let first_before = run.witness.picture("first-before");
    let first_after = run.witness.picture("first-after");
    let second_after = run.witness.picture("second-after");
    let first_row = first_before
        .rows
        .iter()
        .find(|row| row.label == "owner")
        .expect("first-before owner row");
    assert_eq!(
        first_row.raw_call, "false",
        "raw call field: the fixture must start from the false sentinel"
    );
    assert_eq!(
        first_before.admissions, 0,
        "raw call field: no generation before the first replacement"
    );
    let first_after_row = first_after
        .rows
        .iter()
        .find(|row| row.label == "owner")
        .expect("first-after owner row");
    assert_eq!(
        first_after_row.raw_call,
        closure_tag(admissions[0]),
        "raw call field: the first replacement must name the first admitted generation"
    );
    assert_eq!(
        first_after.admissions, 1,
        "raw call field: exactly one generation after the first replacement"
    );
    assert_eq!(
        first_after.stats.live_nodes,
        first_before.stats.live_nodes + 1,
        "raw call field: the first replacement allocates one closure node"
    );
    let second_after_row = second_after
        .rows
        .iter()
        .find(|row| row.label == "owner")
        .expect("second-after owner row");
    assert_eq!(
        second_after_row.raw_call,
        closure_tag(admissions[1]),
        "raw call field: the second replacement must name the second admitted generation"
    );
    assert_eq!(
        second_after.admissions, 2,
        "raw call field: exactly two generations after the second replacement"
    );
    assert_eq!(
        second_after.stats.live_nodes,
        first_after.stats.live_nodes + 1,
        "raw call field: the second replacement allocates one more closure node"
    );
    assert_eq!(
        first_after.stats.edge_slots, first_before.stats.edge_slots,
        "raw call field: replacing the record must not grow edge storage"
    );

    for (before, after) in [
        ("second-after", "capture-before"),
        ("capture-before", "capture-after"),
        ("capture-after", "unissued-before"),
        ("unissued-before", "unissued-after"),
        ("unissued-after", "missing-before"),
        ("missing-before", "missing-after"),
        ("missing-after", "true-before"),
        ("true-before", "true-after"),
        ("true-after", "gap-before"),
        ("gap-before", "gap-after"),
        ("gap-after", "done"),
    ] {
        assert_eq!(
            run.witness.picture(before),
            run.witness.picture(after),
            "raw call field: {before} and {after} must have byte-identical heap pictures"
        );
    }
    for label in ["missing", "true", "gap"] {
        let row = run
            .witness
            .picture("done")
            .rows
            .iter()
            .find(|row| row.label == label)
            .unwrap_or_else(|| panic!("raw call field: missing refusal row '{label}'"));
        let expected = match label {
            "missing" => "missing",
            "true" => "true",
            _ => "gap",
        };
        assert_eq!(
            row.raw_call, expected,
            "raw call field: the {label} refusal must leave its raw record untouched"
        );
    }
    assert_eq!(
        run.witness.picture("done").admissions,
        2,
        "raw call field: no refused arm may allocate or bind a generation"
    );

    let first_count = Arc::strong_count(&first);
    let second_count = Arc::strong_count(&second);
    let captured_count = Arc::strong_count(&captured);
    eprintln!(
        "LUA_RAW_CALL_FIELD strong first={first_count} second={second_count} captured={captured_count}"
    );
    assert!(
        captured_count < first_count && captured_count < second_count,
        "raw call field: the refused captured closure must hold fewer Arcs than the admitted ones"
    );

    let frame = heap_with(&mut run.vm, |heap| heap.enter_roots(1));
    heap_with(&mut run.vm, |heap| heap.write_root(frame, 0, owner));
    let first_before_collection = Arc::strong_count(&first);
    let first_report = heap_collect(&mut run.vm);
    assert_eq!(
        first_report.live_nodes, 3,
        "raw call field: only the rooted owner, its metatable, and the current closure survive"
    );
    assert_eq!(
        first_report.marked_nodes, 3,
        "raw call field: mark follows owner -> metatable -> raw __call closure"
    );
    assert!(
        !heap_with(&mut run.vm, |heap| Ok(heap.closure_alive(admissions[0]))),
        "raw call field: the replaced generation must be collected as the independent orphan"
    );
    assert!(
        heap_with(&mut run.vm, |heap| Ok(heap.closure_alive(final_handle))),
        "raw call field: the current generation is kept only by the raw edge"
    );
    assert!(
        matches!(
            heap_try(&mut run.vm, |heap| heap.closure_callable(admissions[0])),
            Err(HeapError::StaleHandle)
        ),
        "raw call field: the collected generation must refuse its old handle"
    );
    assert_eq!(
        heap_with(&mut run.vm, |heap| Ok(heap
            .slot_generation(admissions[0].slot())
            .expect("released slot stays in the vector"))),
        admissions[0].generation() + 1,
        "raw call field: the collected generation advances its slot"
    );
    assert_eq!(
        Arc::strong_count(&first),
        first_before_collection - 1,
        "raw call field: the collected slot drops exactly its one Arc"
    );
    assert!(
        Arc::strong_count(&first) > 0,
        "raw call field: the VM stack and the test still own the old Arc"
    );

    let second_before_collection = Arc::strong_count(&second);
    heap_with(&mut run.vm, |heap| {
        heap.set_named_scalar(metatable, RAW_CALL_FIELD, Value::Null)
    });
    assert_eq!(
        heap_with(&mut run.vm, |heap| heap
            .named_raw_scalar(metatable, RAW_CALL_FIELD)),
        None,
        "raw call field: removing the last edge must delete the record"
    );
    let second_report = heap_collect(&mut run.vm);
    assert_eq!(
        second_report.live_nodes, 2,
        "raw call field: only the rooted owner and metatable survive the last-edge removal"
    );
    assert_eq!(second_report.marked_nodes, 2);
    assert!(
        !heap_with(&mut run.vm, |heap| Ok(heap.closure_alive(final_handle))),
        "raw call field: the current generation dies when its only edge is removed"
    );
    assert!(
        matches!(
            heap_try(&mut run.vm, |heap| heap.closure_callable(final_handle)),
            Err(HeapError::StaleHandle)
        ),
        "raw call field: the released current generation must refuse its handle"
    );
    assert_eq!(
        heap_with(&mut run.vm, |heap| Ok(heap
            .slot_generation(final_handle.slot())
            .expect("released slot stays in the vector"))),
        final_handle.generation() + 1
    );
    assert_eq!(
        Arc::strong_count(&second),
        second_before_collection - 1,
        "raw call field: the released slot drops exactly its one Arc"
    );
    assert!(
        Arc::strong_count(&second) > 0,
        "raw call field: the VM stack and the test still own the current Arc"
    );

    heap_with(&mut run.vm, |heap| heap.clear_root(frame, 0));
    heap_with(&mut run.vm, |heap| heap.leave_roots(frame));
    assert_eq!(
        Arc::strong_count(&captured),
        captured_count,
        "raw call field: no collection may change the refused captured Arc ownership"
    );
}
