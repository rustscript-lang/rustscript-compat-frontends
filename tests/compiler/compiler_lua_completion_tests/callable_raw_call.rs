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
//!
//! A second witness in this file joins that raw field to the private published
//! pending-finalizer queue in one VM. Two real typed userdata share one
//! metatable whose raw `__gc` is the same issued receiver table; one private
//! collection selects both in allocation-reversed cohort order and publishes
//! them, with an independent weak-value orphan as the real weak-clearance
//! witness. The saved Release tail is observed and finished before the first
//! dequeue. The actual published head is taken through the action handoff and
//! roots the exact userdata, the existing closure generation, and the original
//! receiver. The second head stays published; a real guest helper invoked in
//! the same halted VM calls the same `note(owner, target)` import against the
//! same metatable and replaces `__call` with the second original zero-capture
//! closure. The second dequeue then re-reads that live edge. Each take writes
//! exactly three real roots. After both takes the source edges and the
//! dequeue roots are withdrawn, a real collection retires both typed userdata
//! tokens and advances their old generations, while an independent external
//! receiver root and an unrelated sentinel stay readable. That outer root is
//! then withdrawn and another real collection reclaims the receiver. The
//! pre-replacement edge is recorded as the omission control, and the second
//! take is required not to return the old original Arc.

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
use crate::heap::{
    self, ClosureRef, GuestClass, HandleKind, HeapError, HeapStats, LuaHeap, LuaUserdataResource,
    PendingFinalizerAction, RootFrameId, TableRef, UserdataRef,
};

const FIXTURE: &str = "callable_raw_call.lua";
const PUBLISHED_DEQUEUE_FIXTURE: &str = "callable_raw_call_published_dequeue.lua";
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

fn named_fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("luajit")
        .join("finalizer")
        .join(name)
}

struct RawCallRun {
    vm: Vm,
    status: Result<VmStatus, VmError>,
    stack: Vec<Value>,
    witness: Witness,
    fuel_consumed: u64,
}

fn run_fixture() -> RawCallRun {
    run_fixture_named(FIXTURE)
}

fn run_fixture_named(fixture: &str) -> RawCallRun {
    *WITNESS.lock().expect("raw call field witness") = Witness::default();

    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&RAW_CALL_PLUGIN)
        .with_host_api_catalog(Arc::clone(raw_call_catalog()));
    let compiled = vm::compile_source_file_with_options(named_fixture_path(fixture), options)
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

// ---------------------------------------------------------------------------
// Published dequeue: the live raw `__call` edge after a real guest replacement
// ---------------------------------------------------------------------------

fn publish_userdata(vm: &mut Vm) -> (UserdataRef, u64) {
    let mut host = vm.host_context();
    let userdata = heap::publish_userdata(&mut host)
        .unwrap_or_else(|error| panic!("raw call dequeue: publish userdata: {error:?}"));
    let raw = heap::with_heap(&mut host, |heap| heap.published_userdata_token(userdata))
        .unwrap_or_else(|error| panic!("raw call dequeue: published userdata token: {error:?}"));
    (userdata, raw)
}

fn publish_orphan_table(vm: &mut Vm) -> (TableRef, Value) {
    let mut host = vm.host_context();
    let value = heap::publish_guest(&mut host)
        .unwrap_or_else(|error| panic!("raw call dequeue: publish weak orphan: {error:?}"));
    let class = heap::with_heap(&mut host, |heap| Ok(heap.guest_class(&value)))
        .unwrap_or_else(|error| panic!("raw call dequeue: weak orphan class: {error:?}"));
    let GuestClass::Table(table) = class else {
        panic!("raw call dequeue: the weak orphan must be an issued table");
    };
    (table, value)
}

fn userdata_from_token(vm: &mut Vm, raw: u64) -> UserdataRef {
    let host = vm.host_context();
    let handle = vm::ResourceHandle::from_raw(raw)
        .unwrap_or_else(|error| panic!("raw call dequeue: invalid userdata handle {raw}: {error}"));
    let borrowed = host
        .borrow_resource_with_key::<LuaUserdataResource>(handle, &LuaUserdataResource::key())
        .unwrap_or_else(|error| panic!("raw call dequeue: typed lua.userdata borrow: {error:?}"));
    let payload = borrowed.get().userdata();
    assert_eq!(
        payload.kind(),
        HandleKind::Userdata,
        "raw call dequeue: the typed payload names a userdata"
    );
    payload
}

fn userdata_token_open(vm: &mut Vm, raw: u64) -> bool {
    let Ok(handle) = vm::ResourceHandle::from_raw(raw) else {
        return false;
    };
    let host = vm.host_context();
    host.typed_resource_with_key::<LuaUserdataResource>(handle, &LuaUserdataResource::key())
        .is_ok()
}

fn borrow_published_userdata(vm: &mut Vm, userdata: UserdataRef) -> Result<UserdataRef, HeapError> {
    let host = vm.host_context();
    heap::borrow_published_userdata(&host, userdata)
}

fn pending_records(vm: &mut Vm) -> Vec<(u64, u32, u32)> {
    heap_with(vm, |heap| Ok(heap.pending_finalizer_records()))
}

fn published_count(vm: &mut Vm) -> usize {
    let mut host = vm.host_context();
    heap::pending_finalizer_published_count(&mut host)
        .unwrap_or_else(|error| panic!("raw call dequeue: published count: {error:?}"))
}

fn slot_generation(vm: &mut Vm, slot: u32) -> u32 {
    heap_with(vm, |heap| Ok(heap.slot_generation(slot)))
        .unwrap_or_else(|| panic!("raw call dequeue: slot {slot} stays in the vector"))
}

fn pending_flag(vm: &mut Vm, userdata: UserdataRef) -> bool {
    heap_with(vm, |heap| heap.slot_pending_finalizer(userdata))
}

fn settled_flag(vm: &mut Vm, userdata: UserdataRef) -> bool {
    heap_with(vm, |heap| heap.slot_finalizer_settled(userdata))
}

fn closure_alive(vm: &mut Vm, closure: ClosureRef) -> bool {
    heap_with(vm, |heap| Ok(heap.closure_alive(closure)))
}

fn saved_tail(vm: &mut Vm) -> Option<heap::SavedTailState> {
    let mut host = vm.host_context();
    heap::saved_tail_state(&mut host)
        .unwrap_or_else(|error| panic!("raw call dequeue: saved tail state: {error:?}"))
}

fn finish_published_tail(vm: &mut Vm) -> heap::CollectReport {
    let mut host = vm.host_context();
    match heap::resume_published_sweep(&mut host, u64::MAX)
        .unwrap_or_else(|error| panic!("raw call dequeue: resume tail: {error:?}"))
    {
        heap::PublishedResume::Completed(report) => report,
        heap::PublishedResume::Paused => {
            panic!("raw call dequeue: an unbounded budget finishes the Release tail")
        }
    }
}

fn take_published_action(
    vm: &mut Vm,
    frame: RootFrameId,
    slots: &[u32],
) -> Result<crate::heap::TakenPendingFinalizer, HeapError> {
    let mut host = vm.host_context();
    heap::take_published_pending_finalizer_action(&mut host, frame, slots)
}

fn assert_empty_roots(vm: &mut Vm, frame: RootFrameId, slots: &[u32]) {
    let mut host = vm.host_context();
    for slot in slots {
        assert_eq!(
            heap::rooted_userdata(&mut host, frame, *slot),
            Ok(None),
            "raw call dequeue: root slot {slot} has no written userdata"
        );
        assert_eq!(
            heap::rooted_closure(&mut host, frame, *slot),
            Ok(None),
            "raw call dequeue: root slot {slot} has no written closure"
        );
        assert_eq!(
            heap::rooted_table(&mut host, frame, *slot),
            Ok(None),
            "raw call dequeue: root slot {slot} has no written table"
        );
    }
}

/// Checks the exact three-slot action plan: userdata, call closure, receiver.
/// Each slot must hold that one kind and no other kind.
fn assert_table_call_roots(
    vm: &mut Vm,
    frame: RootFrameId,
    slots: &[u32],
    userdata: UserdataRef,
    closure: ClosureRef,
    receiver: TableRef,
) {
    assert_eq!(slots.len(), 3);
    let mut host = vm.host_context();
    assert_eq!(
        heap::rooted_userdata(&mut host, frame, slots[0]),
        Ok(Some(userdata))
    );
    assert_eq!(
        heap::rooted_closure(&mut host, frame, slots[0]),
        Err(HeapError::StaleHandle)
    );
    assert_eq!(
        heap::rooted_table(&mut host, frame, slots[0]),
        Err(HeapError::StaleHandle)
    );
    assert_eq!(
        heap::rooted_userdata(&mut host, frame, slots[1]),
        Err(HeapError::StaleHandle)
    );
    assert_eq!(
        heap::rooted_closure(&mut host, frame, slots[1]),
        Ok(Some(closure))
    );
    assert_eq!(
        heap::rooted_table(&mut host, frame, slots[1]),
        Err(HeapError::StaleHandle)
    );
    assert_eq!(
        heap::rooted_userdata(&mut host, frame, slots[2]),
        Err(HeapError::StaleHandle)
    );
    assert_eq!(
        heap::rooted_closure(&mut host, frame, slots[2]),
        Err(HeapError::StaleHandle)
    );
    assert_eq!(
        heap::rooted_table(&mut host, frame, slots[2]),
        Ok(Some(receiver))
    );
}

/// Joins one real guest-written raw `__call` to the private published
/// pending-finalizer prefix. Two real typed userdata in this same VM carry one
/// shared metatable whose raw `__gc` is the issued receiver table. The real
/// private collector selects both in allocation-reversed cohort order and
/// publishes them after one real weak-value orphan is cleared. The saved
/// Release tail refuses a dequeue until it is finished; a saved mark also
/// refuses. The first actual head is taken and roots the exact userdata, the
/// existing guest closure generation, and the original receiver. The second
/// head stays published while a real guest helper, invoked in the same halted
/// VM, runs the same `note(owner, target)` import and rewrites the same raw
/// `__call` edge. The second take re-reads that live edge.
///
/// The pre-replacement edge is recorded as the omission control: if the guest
/// replacement were omitted, the second take would have to name the first
/// original Arc. The second take is also required not to name that old Arc.
/// The helper invocation is ordinary guest execution used only to reach the
/// existing `note` import; it is not a `__gc` or `__call` callback.
/// Withdrawing the two dequeue root triples and both source edges lets a real
/// collection retire both typed tokens and old generations, while an
/// independent external receiver root and an unrelated sentinel remain
/// readable. Withdrawing that outer root and collecting again reclaims the
/// receiver and its metatable.
#[test]
fn raw_call_published_dequeue_keeps_fifo_and_relooks_the_live_guest_call() {
    let mut run = run_fixture_named(PUBLISHED_DEQUEUE_FIXTURE);
    assert!(
        matches!(run.status, Ok(VmStatus::Halted)),
        "raw call dequeue: the fixture must halt, got {:?}",
        run.status
    );
    assert!(
        run.fuel_consumed > 0 && run.fuel_consumed < FUEL_BUDGET,
        "raw call dequeue: expected real fuel use, saw {}",
        run.fuel_consumed
    );

    let items = exit_items(&run.stack);
    assert_eq!(
        items.len(),
        6,
        "raw call dequeue: [true] plus five returned values"
    );
    assert_eq!(items[0], Value::Bool(true));
    let receiver_value = &items[1];
    let first = callable_item(&items[2]);
    let second = callable_item(&items[3]);
    let replace_call = items[4].clone();
    let first_ok = match &items[5] {
        Value::Bool(ok) => *ok,
        other => panic!("raw call dequeue: expected the first note result, got {other:?}"),
    };
    assert!(
        first_ok,
        "raw call dequeue: the first note must install the first original closure"
    );
    assert!(
        matches!(replace_call, Value::Callable(_)),
        "raw call dequeue: the guest helper must remain a real callable"
    );
    assert!(
        !same_callable(&items[2], &items[3]),
        "raw call dequeue: the two original guest closures are distinct Core Arcs"
    );
    assert_eq!(
        prototype_captures(&run.vm, &first),
        0,
        "raw call dequeue: the first target has no captures"
    );
    assert_eq!(
        prototype_captures(&run.vm, &second),
        0,
        "raw call dequeue: the second target has no captures"
    );

    let (receiver, metatable) = {
        let record = run.witness.record("owner");
        (record.owner, record.metatable)
    };
    let issued_receiver = heap_with(&mut run.vm, |heap| heap.issued_value(receiver));
    assert!(
        same_array(&issued_receiver, receiver_value),
        "raw call dequeue: the receiver is the exact issued table carrier"
    );
    assert_eq!(
        heap_with(&mut run.vm, |heap| heap.metatable(receiver)),
        Some(metatable),
        "raw call dequeue: the receiver keeps its one issued metatable"
    );

    let first_closure = heap_try(&mut run.vm, |heap| {
        heap.named_closure_edge(metatable, RAW_CALL_FIELD)
    })
    .expect("raw call dequeue: the first note must create a raw closure edge");
    assert_eq!(
        first_closure.domain(),
        receiver.domain(),
        "raw call dequeue: the closure is in the receiver's heap domain"
    );
    assert_eq!(first_closure.kind(), HandleKind::Closure);
    let first_stored = heap_with(&mut run.vm, |heap| heap.closure_callable(first_closure));
    assert!(
        Arc::ptr_eq(&first_stored, &first),
        "raw call dequeue: the first raw edge names the exact first guest Arc"
    );
    assert_eq!(
        heap_with(&mut run.vm, |heap| Ok(heap
            .guest_callable_admissions()
            .to_vec())),
        vec![first_closure],
        "raw call dequeue: only the first note has allocated a generation"
    );

    let (ready_tag, ready_admissions) = {
        let ready = run.witness.picture("ready");
        let ready_row = ready
            .rows
            .iter()
            .find(|row| row.label == "owner")
            .expect("raw call dequeue: the ready picture has the receiver row");
        (ready_row.raw_call.clone(), ready.admissions)
    };
    assert_eq!(
        ready_tag,
        closure_tag(first_closure),
        "raw call dequeue: the guest run reaches the ready point with the first generation"
    );
    assert_eq!(
        ready_admissions, 1,
        "raw call dequeue: the guest run admits exactly one closure generation"
    );

    // B is allocated before A, so the live-userdata chain head and the
    // published FIFO head are A, followed by B.
    let (b, b_raw) = publish_userdata(&mut run.vm);
    let (a, a_raw) = publish_userdata(&mut run.vm);
    assert!(
        a.slot() > b.slot(),
        "raw call dequeue: the later allocation is the live-chain head"
    );
    assert_eq!(a.domain(), b.domain());
    assert_eq!(a.kind(), HandleKind::Userdata);
    assert_eq!(b.kind(), HandleKind::Userdata);
    assert_ne!(
        a.slot(),
        b.slot(),
        "raw call dequeue: A and B occupy distinct slots"
    );
    assert_eq!(
        userdata_from_token(&mut run.vm, a_raw),
        a,
        "raw call dequeue: A's real typed token names A"
    );
    assert_eq!(
        userdata_from_token(&mut run.vm, b_raw),
        b,
        "raw call dequeue: B's real typed token names B"
    );
    assert!(
        userdata_token_open(&mut run.vm, a_raw) && userdata_token_open(&mut run.vm, b_raw),
        "raw call dequeue: both typed tokens start open"
    );
    assert_eq!(
        slot_generation(&mut run.vm, a.slot()),
        a.generation(),
        "raw call dequeue: A starts at its exact generation"
    );
    assert_eq!(
        slot_generation(&mut run.vm, b.slot()),
        b.generation(),
        "raw call dequeue: B starts at its exact generation"
    );

    // One real metatable with a raw `__gc` table value for both candidates.
    let finalizer_metatable = heap_with(&mut run.vm, |heap| heap.alloc());
    heap_with(&mut run.vm, |heap| {
        heap.set_named_edge(finalizer_metatable, "__gc", receiver)
    });
    heap_with(&mut run.vm, |heap| {
        heap.set_userdata_metatable(a, Some(finalizer_metatable))
    });
    heap_with(&mut run.vm, |heap| {
        heap.set_userdata_metatable(b, Some(finalizer_metatable))
    });
    assert_eq!(
        heap_with(&mut run.vm, |heap| heap.userdata_metatable(a)),
        Some(finalizer_metatable),
        "raw call dequeue: A's own metatable stores the shared finalizer record"
    );
    assert_eq!(
        heap_with(&mut run.vm, |heap| heap.userdata_metatable(b)),
        Some(finalizer_metatable),
        "raw call dequeue: B's own metatable stores the shared finalizer record"
    );
    assert!(
        same_array(
            &heap_with(&mut run.vm, |heap| {
                heap.named_value(finalizer_metatable, "__gc")
            }),
            receiver_value,
        ),
        "raw call dequeue: both userdata share a raw __gc that names the receiver table"
    );

    // The independent weak-value orphan is the real weak-clearance witness.
    // It is an issued table so the pre-collection weak read can name the same
    // guest identity that the real weak publication later clears.
    let (orphan, orphan_value) = publish_orphan_table(&mut run.vm);
    heap_with(&mut run.vm, |heap| heap.set_edge(orphan, 1, orphan));
    let weak = heap_with(&mut run.vm, |heap| {
        let weak = heap.alloc()?;
        let mode = heap.alloc()?;
        heap.set_mode_scalar(mode, Value::string("v"))?;
        heap.set_metatable(weak, Some(mode))?;
        heap.set_number_edge(weak, 1.0, orphan)?;
        Ok(weak)
    });
    let sentinel = heap_with(&mut run.vm, |heap| heap.alloc());
    let frame = heap_with(&mut run.vm, |heap| heap.enter_roots(9));
    heap_with(&mut run.vm, |heap| heap.write_root(frame, 0, weak));
    heap_with(&mut run.vm, |heap| heap.write_root(frame, 1, receiver));
    heap_with(&mut run.vm, |heap| heap.write_root(frame, 2, sentinel));
    let weak_before = heap_with(&mut run.vm, |heap| heap.number_value(weak, 1.0));
    assert!(
        same_array(&weak_before, &orphan_value),
        "raw call dequeue: the weak orphan is readable before the real collection"
    );

    let boundary = {
        let mut host = run.vm.host_context();
        heap::collect_pending_finalizers(&mut host)
            .unwrap_or_else(|error| panic!("raw call dequeue: publish boundary: {error:?}"))
    };
    assert!(
        matches!(boundary, heap::PublishedBoundary::Published { .. }),
        "raw call dequeue: the real collection must save a Release tail"
    );
    assert_eq!(
        pending_records(&mut run.vm),
        vec![
            (a_raw, a.slot(), a.generation()),
            (b_raw, b.slot(), b.generation()),
        ],
        "raw call dequeue: publication preserves the selection-time FIFO A then B"
    );
    assert_eq!(
        published_count(&mut run.vm),
        2,
        "raw call dequeue: both selected candidates are a consumable prefix"
    );
    let tail = saved_tail(&mut run.vm).expect("raw call dequeue: the Release tail is saved");
    assert_eq!(
        tail.phase,
        heap::SavedTailPhase::Release,
        "raw call dequeue: publication stops at the Release phase"
    );
    assert_eq!(
        heap_with(&mut run.vm, |heap| heap.number_value(weak, 1.0)),
        Value::Null,
        "raw call dequeue: the independent weak orphan is cleared at real publication"
    );
    assert!(
        heap_with(&mut run.vm, |heap| Ok(heap.userdata_alive(a)))
            && heap_with(&mut run.vm, |heap| Ok(heap.userdata_alive(b))),
        "raw call dequeue: selection retains both typed userdata"
    );
    assert!(
        userdata_token_open(&mut run.vm, a_raw) && userdata_token_open(&mut run.vm, b_raw),
        "raw call dequeue: publication closes neither selected token"
    );
    assert!(
        closure_alive(&mut run.vm, first_closure),
        "raw call dequeue: the selected metatable keeps the first generation alive"
    );

    // The saved Release tail owns the graph and refuses before any write.
    let refused = take_published_action(&mut run.vm, frame, &[3, 4, 5])
        .expect_err("raw call dequeue: a saved Release tail must refuse");
    assert_eq!(refused, HeapError::SweepPending);
    assert_empty_roots(&mut run.vm, frame, &[3, 4, 5]);
    assert_eq!(pending_records(&mut run.vm).len(), 2);

    let report = finish_published_tail(&mut run.vm);
    assert!(
        report.live_nodes > 0,
        "raw call dequeue: finishing the tail records a real report"
    );
    assert_eq!(
        saved_tail(&mut run.vm),
        None,
        "raw call dequeue: the saved tail is gone after the real resume"
    );
    assert!(
        !heap_with(&mut run.vm, |heap| Ok(heap.alive(orphan))),
        "raw call dequeue: the independent weak orphan is released by the real tail"
    );
    assert_eq!(
        slot_generation(&mut run.vm, orphan.slot()),
        orphan.generation() + 1,
        "raw call dequeue: the released orphan slot advances its generation"
    );

    // A parked mark also refuses before any root or queue write. The ordinary
    // collection drops it and keeps the published prefix intact.
    {
        let mut host = run.vm.host_context();
        heap::open_saved_mark_for_test(&mut host).expect("raw call dequeue: open one saved mark");
    }
    let refused = take_published_action(&mut run.vm, frame, &[3, 4, 5])
        .expect_err("raw call dequeue: a saved mark must refuse");
    assert_eq!(refused, HeapError::MarkActive);
    assert_empty_roots(&mut run.vm, frame, &[3, 4, 5]);
    assert_eq!(
        pending_records(&mut run.vm),
        vec![
            (a_raw, a.slot(), a.generation()),
            (b_raw, b.slot(), b.generation()),
        ],
        "raw call dequeue: neither refusal consumes a record"
    );
    {
        let mut host = run.vm.host_context();
        let _ =
            heap::collect(&mut host).expect("raw call dequeue: ordinary collect drops the mark");
    }
    assert_eq!(
        pending_records(&mut run.vm).len(),
        2,
        "raw call dequeue: the ordinary collection keeps the published prefix"
    );
    assert_eq!(published_count(&mut run.vm), 2);

    // Take the actual published head A. It roots exactly A, the current first
    // closure generation, and the original table receiver.
    let a_taken = take_published_action(&mut run.vm, frame, &[3, 4, 5])
        .expect("raw call dequeue: the first published head");
    assert_eq!(a_taken.userdata, a);
    assert_eq!(
        a_taken.action,
        PendingFinalizerAction::TableCall {
            receiver,
            call: first_closure,
        },
        "raw call dequeue: A names the same receiver and the existing first closure"
    );
    assert_eq!(a_taken.roots_written, 3);
    assert_table_call_roots(&mut run.vm, frame, &[3, 4, 5], a, first_closure, receiver);
    assert_eq!(
        pending_records(&mut run.vm),
        vec![(b_raw, b.slot(), b.generation())],
        "raw call dequeue: B remains the published head after A leaves"
    );
    assert_eq!(published_count(&mut run.vm), 1);
    assert!(
        settled_flag(&mut run.vm, a) && !pending_flag(&mut run.vm, a),
        "raw call dequeue: A is settled exactly once"
    );
    assert!(
        pending_flag(&mut run.vm, b),
        "raw call dequeue: B is still pending and published"
    );
    assert!(
        userdata_token_open(&mut run.vm, a_raw),
        "raw call dequeue: A's token stays open while its root exists"
    );
    assert_eq!(
        slot_generation(&mut run.vm, a.slot()),
        a.generation(),
        "raw call dequeue: taking A does not advance its generation"
    );
    let a_stored = heap_with(&mut run.vm, |heap| heap.closure_callable(first_closure));
    assert!(
        Arc::ptr_eq(&a_stored, &first_stored),
        "raw call dequeue: A's action still resolves the exact first Arc"
    );

    // Omission control: before the guest replacement, the same raw edge still
    // names the first original Arc. If `note` were omitted below, the second
    // take could only produce this old identity.
    let before_replacement = heap_try(&mut run.vm, |heap| {
        heap.named_closure_edge(metatable, RAW_CALL_FIELD)
    })
    .expect("raw call dequeue: the pre-replacement edge exists");
    assert_eq!(before_replacement, first_closure);
    assert!(
        Arc::ptr_eq(
            &heap_with(&mut run.vm, |heap| heap
                .closure_callable(before_replacement)),
            &first
        ),
        "raw call dequeue: the omission control is the old original Arc"
    );

    // The real guest helper runs in this same halted VM and calls the same
    // note(owner, target) import on the same receiver and metatable.
    let replaced = run
        .vm
        .invoke_callable(replace_call, &[])
        .unwrap_or_else(|error| panic!("raw call dequeue: guest replacement: {error}"));
    let Value::Array(replacement_completion) = &replaced else {
        panic!("raw call dequeue: the guest helper must return its completion, got {replaced:?}");
    };
    assert_eq!(
        replacement_completion.as_slice().first(),
        Some(&Value::Bool(true)),
        "raw call dequeue: the guest helper returns normally"
    );
    assert_eq!(
        replacement_completion.as_slice().get(1),
        Some(&Value::Bool(true)),
        "raw call dequeue: the same note import accepts the second original closure"
    );

    let second_closure = heap_try(&mut run.vm, |heap| {
        heap.named_closure_edge(metatable, RAW_CALL_FIELD)
    })
    .expect("raw call dequeue: the replacement must create the new raw edge");
    assert_ne!(
        second_closure, first_closure,
        "raw call dequeue: the guest replacement allocates a new closure generation"
    );
    assert_eq!(
        heap_with(&mut run.vm, |heap| Ok(heap
            .guest_callable_admissions()
            .to_vec())),
        vec![first_closure, second_closure],
        "raw call dequeue: the two guest imports admit exactly two generations"
    );
    let second_stored = heap_with(&mut run.vm, |heap| heap.closure_callable(second_closure));
    assert!(
        Arc::ptr_eq(&second_stored, &second),
        "raw call dequeue: the replaced raw edge names the exact second guest Arc"
    );
    assert!(
        !Arc::ptr_eq(&second_stored, &first),
        "raw call dequeue: the replacement does not reuse the first guest Arc"
    );
    assert_eq!(
        heap_with(&mut run.vm, |heap| Ok(raw_call_tag(heap, metatable))),
        closure_tag(second_closure),
        "raw call dequeue: the same metatable now stores the second generation"
    );
    assert!(
        closure_alive(&mut run.vm, first_closure),
        "raw call dequeue: the old generation is still a live, distinct Arc before GC"
    );

    // Take B through the same action handoff. The re-read current raw edge is
    // the replacement, not the selection-time or pre-replacement identity.
    let b_taken = take_published_action(&mut run.vm, frame, &[6, 7, 8])
        .expect("raw call dequeue: the second published head");
    assert_eq!(b_taken.userdata, b);
    assert_eq!(
        b_taken.action,
        PendingFinalizerAction::TableCall {
            receiver,
            call: second_closure,
        },
        "raw call dequeue: B re-reads the guest-replaced __call on the same receiver"
    );
    assert_eq!(b_taken.roots_written, 3);
    assert_table_call_roots(&mut run.vm, frame, &[6, 7, 8], b, second_closure, receiver);
    assert!(pending_records(&mut run.vm).is_empty());
    assert_eq!(published_count(&mut run.vm), 0);
    assert!(
        settled_flag(&mut run.vm, b) && !pending_flag(&mut run.vm, b),
        "raw call dequeue: B is settled exactly once"
    );
    assert!(
        Arc::ptr_eq(&second_stored, &second) && !Arc::ptr_eq(&second_stored, &first),
        "raw call dequeue: the single old-Arc mis-selection control is refuted"
    );
    assert_eq!(
        slot_generation(&mut run.vm, b.slot()),
        b.generation(),
        "raw call dequeue: taking B does not advance its generation"
    );

    // Withdraw both dequeue triples and the two source edges. The outer frame
    // still roots the receiver and the unrelated sentinel.
    for slot in [3, 4, 5, 6, 7, 8] {
        heap_with(&mut run.vm, |heap| heap.clear_root(frame, slot));
    }
    heap_with(&mut run.vm, |heap| {
        heap.set_named_scalar(finalizer_metatable, "__gc", Value::Null)
    });
    heap_with(&mut run.vm, |heap| {
        heap.set_named_scalar(metatable, RAW_CALL_FIELD, Value::Null)
    });
    {
        let mut host = run.vm.host_context();
        let _ = heap::collect(&mut host)
            .expect("raw call dequeue: real collection after withdrawing the roots");
    }

    for (label, userdata, raw) in [("A", a, a_raw), ("B", b, b_raw)] {
        assert!(
            !heap_with(&mut run.vm, |heap| Ok(heap.userdata_alive(userdata))),
            "raw call dequeue: {label}'s old generation is collected"
        );
        assert!(
            !userdata_token_open(&mut run.vm, raw),
            "raw call dequeue: {label}'s typed token is retired"
        );
        assert_eq!(
            slot_generation(&mut run.vm, userdata.slot()),
            userdata.generation() + 1,
            "raw call dequeue: {label}'s slot advances exactly one generation"
        );
        assert!(
            matches!(
                borrow_published_userdata(&mut run.vm, userdata),
                Err(HeapError::StaleHandle)
            ),
            "raw call dequeue: {label}'s old handle is refused after retirement"
        );
    }
    assert!(
        !closure_alive(&mut run.vm, first_closure),
        "raw call dequeue: the old closure generation dies without its dequeue root"
    );
    assert!(
        !closure_alive(&mut run.vm, second_closure),
        "raw call dequeue: the replacement closure dies after the source edge is removed"
    );
    for closure in [first_closure, second_closure] {
        assert!(
            matches!(
                heap_try(&mut run.vm, |heap| heap.closure_callable(closure)),
                Err(HeapError::StaleHandle)
            ),
            "raw call dequeue: the collected closure handle is refused"
        );
    }
    assert!(
        heap_with(&mut run.vm, |heap| Ok(heap.alive(receiver))),
        "raw call dequeue: the external receiver root keeps the receiver readable"
    );
    assert_eq!(
        heap_with(&mut run.vm, |heap| heap.metatable(receiver)),
        Some(metatable),
        "raw call dequeue: the externally rooted receiver keeps its metatable"
    );
    assert!(
        !heap_with(&mut run.vm, |heap| Ok(heap.alive(finalizer_metatable))),
        "raw call dequeue: the shared finalizer metatable dies with both userdata"
    );
    assert!(
        heap_with(&mut run.vm, |heap| Ok(heap.alive(sentinel))),
        "raw call dequeue: the unrelated external sentinel stays readable"
    );

    // Withdraw the outer roots and collect once more: the receiver and its
    // metatable are reclaimed only now.
    for slot in [0, 1, 2] {
        heap_with(&mut run.vm, |heap| heap.clear_root(frame, slot));
    }
    {
        let mut host = run.vm.host_context();
        let _ = heap::collect(&mut host)
            .expect("raw call dequeue: real collection after withdrawing the outer roots");
    }
    heap_with(&mut run.vm, |heap| heap.leave_roots(frame));
    assert!(
        !heap_with(&mut run.vm, |heap| Ok(heap.alive(receiver))),
        "raw call dequeue: the receiver is reclaimed after its external root is withdrawn"
    );
    assert!(
        !heap_with(&mut run.vm, |heap| Ok(heap.alive(metatable))),
        "raw call dequeue: the receiver's metatable dies with it"
    );
    assert!(
        !heap_with(&mut run.vm, |heap| Ok(heap.alive(sentinel))),
        "raw call dequeue: the sentinel is reclaimed after its external root is withdrawn"
    );
    assert!(
        !heap_with(&mut run.vm, |heap| Ok(heap.alive(weak))),
        "raw call dequeue: the weak witness table is reclaimed with its final root"
    );
}
