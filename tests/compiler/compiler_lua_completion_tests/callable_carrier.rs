//! Exact Core callable ownership on a Lua closure slot generation.
//!
//! One real `.lua` file is fed through the real lexer, parser, resolver and
//! private completion lowering, compiled through the fixed Core and run in one
//! interpreter VM with JIT disabled. The fixture writes three real callables
//! and hands each to the heap-owned `note` admission import as an ordinary
//! guest argument. That guest call is the issuance evidence: the host side
//! receives the exact `SharedCallable` the same VM produced, and this module
//! never builds a `CallableValue` itself.
//!
//! The import is not a raw `Arc + count` entry. Its dispatch function is
//! private to the heap storage module, derives the capture count from this
//! VM's compiled prototype for the value, and refuses any prototype with a
//! capture before the heap changes. The test compile catalog only adds the
//! heap-owned schema and registers the heap-owned import. The test never sees
//! a function that accepts an `Arc<CallableValue>` and a caller-chosen count,
//! so the RED that admitted a real captured callable with a bare `0` cannot be
//! expressed against the final surface.
//!
//! Core attaches an empty `CallableEnvironment` container even to a
//! chunk-level Lua function with no captures, so the narrow admission is the
//! exact compiled capture count, not the container bit. The run log records
//! `rooted captures=0 admitted=true`, `unrooted captures=0 admitted=true`, and
//! `captured captures=1 admitted=false`; a literal `env.is_none()` admission
//! has no producer in this frontend. After the single `Vm::run` the test uses
//! the real heap collector:
//!
//! - the registered node with an explicit Lua root survives a real collection
//!   and still returns the exact `Arc`;
//! - the independently registered node without a root is really collected even
//!   though the VM stack and the test still hold live Arcs to that callable, so
//!   the carrier is not a root or a mark edge;
//! - the captured callable is refused by the import, receives no closure
//!   generation, and its Arc is never stored;
//! - clearing the only root and collecting drops exactly the one Arc the slot
//!   held, measured on a local Arc without a temporary clone and checked
//!   against the cleared slot;
//! - the next generation of the same slot starts unbound and rejects the old
//!   handle.
//!
//! Pure slot semantics that a guest cannot name directly — same-generation
//! rebind, store-level capture refusal, foreign-domain store, wrong-kind store,
//! and the existing table carrier — are units inside the storage module's own
//! `cfg(test)` scope. Those units use fabricated Arcs only to reach the private
//! store; they are not a business admission path. No queue dequeue, finalizer,
//! or production admission is added by this slice.

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
use crate::heap::{self, HeapError, HeapLimits, LuaHeap};

const FIXTURE: &str = "callable_carrier.lua";

/// Stage labels recorded by the test-owned `mark` import. The admission import
/// is heap-owned; this marker only orders the guest calls that reach it.
static STAGE: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn carrier_catalog() -> &'static Arc<HostApiCatalog> {
    static CATALOG: OnceLock<Arc<HostApiCatalog>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut builder = HostApiBuilder::new();
        builder.function(HostFunctionSchema::with_return(
            "mark",
            vec![HostParamSchema::value("label", HostTypeSchema::String)],
            HostTypeSchema::Int,
        ));
        // The heap owns this schema and dispatch. The test adds the exact
        // declared shape to its compile catalog; it does not define the
        // admission behavior.
        builder.function(LuaHeap::guest_callable_import_schema());
        Arc::new(builder.build().expect("callable carrier catalog"))
    })
}

fn catalog_schema(name: &str) -> HostImportSchema {
    let catalog = carrier_catalog();
    let function = catalog
        .function(name)
        .unwrap_or_else(|| panic!("catalog is missing '{name}'"));
    HostImportSchema::from_function(catalog, function)
}

fn bind_hosts(vm: &mut Vm) -> Result<(), VmError> {
    let mut registry = HostFunctionRegistry::empty();
    registry.register_exact_static("mark", 1, catalog_schema("mark"), host_mark)?;
    LuaHeap::register_guest_callable_import(
        &mut registry,
        catalog_schema(LuaHeap::GUEST_CALLABLE_IMPORT),
    )?;
    registry.bind_vm_cached(vm)
}

fn host_fault(message: impl Into<String>) -> VmError {
    VmError::HostError(message.into())
}

/// Records the stage label. The next heap-owned admission is ordered after it
/// by the guest program, not by a shared host slot.
fn host_mark(_vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
    let [Value::String(label)] = args else {
        return Err(host_fault("mark expects one string label"));
    };
    let label = label.as_str().to_string();
    eprintln!("LUA_CALLABLE_CARRIER stage={label}");
    STAGE.lock().expect("callable carrier stage").push(label);
    Ok(CallOutcome::Return(CallReturn::one(Value::Int(0))))
}

struct CarrierPlugin;

static CARRIER_PLUGIN: CarrierPlugin = CarrierPlugin;

impl SourcePlugin for CarrierPlugin {
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
        let resolver = HostCallResolver::new(carrier_catalog());
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

struct CarrierRun {
    vm: Vm,
    status: Result<VmStatus, VmError>,
    stack: Vec<Value>,
    stages: Vec<String>,
    fuel_consumed: u64,
}

fn run_fixture() -> CarrierRun {
    STAGE.lock().expect("callable carrier stage").clear();

    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&CARRIER_PLUGIN)
        .with_host_api_catalog(Arc::clone(carrier_catalog()));
    let compiled = vm::compile_source_file_with_options(fixture_path(), options)
        .unwrap_or_else(|error| panic!("callable carrier: compile failed: {error:?}"));

    let mut runtime = Vm::new_with_jit_config(
        compiled.program,
        JitConfig {
            enabled: false,
            hot_loop_threshold: 8,
            max_trace_len: 256,
        },
    );
    bind_hosts(&mut runtime)
        .unwrap_or_else(|error| panic!("callable carrier: bind failed: {error}"));
    runtime
        .set_fuel_check_interval(FUEL_CHECK_INTERVAL)
        .unwrap_or_else(|error| panic!("callable carrier: fuel interval: {error}"));
    runtime.set_fuel(FUEL_BUDGET);
    let status = runtime.run();
    let remaining = runtime
        .get_fuel()
        .unwrap_or_else(|| panic!("callable carrier: fuel metering missing"));
    let fuel_consumed = FUEL_BUDGET
        .checked_sub(remaining)
        .unwrap_or_else(|| panic!("callable carrier: fuel increased"));
    let stack = runtime.stack().to_vec();
    let stages = std::mem::take(&mut *STAGE.lock().expect("callable carrier stage"));
    CarrierRun {
        vm: runtime,
        status,
        stack,
        stages,
        fuel_consumed,
    }
}

/// The one completion array of the chunk:
/// `[true, rooted, unrooted, captured, rooted_ok, unrooted_ok, captured_ok]`.
fn exit_items(stack: &[Value]) -> &[Value] {
    let [Value::Array(items)] = stack else {
        panic!("callable carrier: expected the one completion array, got {stack:?}");
    };
    items.as_slice()
}

fn callable_item(value: &Value) -> Arc<CallableValue> {
    match value {
        Value::Callable(callable) => Arc::clone(callable),
        other => panic!("callable carrier: expected a callable, got {other:?}"),
    }
}

fn prototype_captures(vm: &Vm, callable: &Arc<CallableValue>) -> usize {
    vm.program()
        .callable_prototypes
        .get(callable.prototype_id as usize)
        .unwrap_or_else(|| {
            panic!(
                "callable carrier: prototype {} is outside this VM",
                callable.prototype_id
            )
        })
        .capture_slots
        .len()
}

fn heap_with<T>(vm: &mut Vm, body: impl FnOnce(&mut LuaHeap) -> Result<T, HeapError>) -> T {
    let mut host = vm.host_context();
    heap::with_heap(&mut host, body)
        .unwrap_or_else(|error| panic!("callable carrier heap: {error:?}"))
}

fn heap_try<T>(
    vm: &mut Vm,
    body: impl FnOnce(&mut LuaHeap) -> Result<T, HeapError>,
) -> Result<T, HeapError> {
    let mut host = vm.host_context();
    heap::with_heap(&mut host, body)
}

/// One host-boundary collection. Closure slots carry no Core resource token,
/// so this is the same mark/sweep the heap uses; going through the host entry
/// keeps the collection on the real host path.
fn heap_collect(vm: &mut Vm) -> heap::CollectReport {
    let mut host = vm.host_context();
    heap::collect(&mut host).unwrap_or_else(|error| panic!("callable carrier collect: {error:?}"))
}

#[test]
fn core_callable_follows_closure_generation_registration_and_release() {
    let mut run = run_fixture();
    assert!(
        matches!(run.status, Ok(VmStatus::Halted)),
        "callable carrier: the fixture must halt, got {:?}",
        run.status
    );
    assert!(
        run.fuel_consumed > 0 && run.fuel_consumed < FUEL_BUDGET,
        "callable carrier: expected real fuel use, saw {}",
        run.fuel_consumed
    );
    assert_eq!(
        run.stages,
        vec![
            "register-rooted".to_string(),
            "register-unrooted".to_string(),
            "register-environment".to_string(),
            "done".to_string()
        ],
        "callable carrier: each real guest binding must reach the marker once"
    );

    let (rooted, unrooted, captured, rooted_ok, unrooted_ok, captured_ok) = {
        let items = exit_items(&run.stack);
        assert_eq!(
            items.len(),
            7,
            "callable carrier: [true] plus three callables and three admission results"
        );
        assert_eq!(items[0], Value::Bool(true));
        match (&items[4], &items[5], &items[6]) {
            (Value::Bool(rooted_ok), Value::Bool(unrooted_ok), Value::Bool(captured_ok)) => (
                callable_item(&items[1]),
                callable_item(&items[2]),
                callable_item(&items[3]),
                *rooted_ok,
                *unrooted_ok,
                *captured_ok,
            ),
            other => panic!("callable carrier: expected three boolean results, got {other:?}"),
        }
    };
    assert!(
        !Arc::ptr_eq(&rooted, &unrooted),
        "the two chunk-level functions must be distinct Core Arcs"
    );
    assert!(
        !Arc::ptr_eq(&rooted, &captured),
        "the captured function is a third Core Arc"
    );
    assert!(
        rooted_ok && unrooted_ok,
        "the two zero-capture callables must be admitted"
    );
    assert!(
        !captured_ok,
        "the captured callable must be refused by the heap-owned import"
    );

    let rooted_captures = prototype_captures(&run.vm, &rooted);
    let unrooted_captures = prototype_captures(&run.vm, &unrooted);
    let captured_captures = prototype_captures(&run.vm, &captured);
    assert_eq!(
        rooted_captures, 0,
        "the chunk-level function has no captures"
    );
    assert_eq!(
        unrooted_captures, 0,
        "the chunk-level function has no captures"
    );
    assert_eq!(
        captured_captures, 1,
        "the captured nested function has one real capture"
    );
    assert!(
        rooted.env.is_some() && unrooted.env.is_some(),
        "Core attaches an empty environment container even to a zero-capture closure"
    );
    assert!(
        captured.env.is_some(),
        "the captured callable carries the same container bit"
    );
    eprintln!(
        "LUA_CALLABLE_CARRIER real rooted captures={rooted_captures} admitted={rooted_ok} \
         unrooted captures={unrooted_captures} admitted={unrooted_ok} \
         captured captures={captured_captures} admitted={captured_ok}"
    );

    let admissions = heap_with(&mut run.vm, |heap| {
        Ok(heap.guest_callable_admissions().to_vec())
    });
    assert_eq!(
        admissions.len(),
        2,
        "only the two zero-capture guest callables may receive a closure generation"
    );
    let rooted_handle = admissions[0];
    let unrooted_handle = admissions[1];
    assert_eq!(rooted_handle.domain(), unrooted_handle.domain());
    assert_ne!(rooted_handle.slot(), unrooted_handle.slot());
    let unrooted_generation = unrooted_handle.generation();

    // The stored identity is exactly the Arc the guest returned, and the
    // refused captured Arc was never stored on either admitted generation.
    {
        let stored_rooted = heap_with(&mut run.vm, |heap| heap.closure_callable(rooted_handle));
        assert!(
            Arc::ptr_eq(&stored_rooted, &rooted),
            "the rooted generation must return the exact guest Arc"
        );
        let stored_unrooted = heap_with(&mut run.vm, |heap| heap.closure_callable(unrooted_handle));
        assert!(
            Arc::ptr_eq(&stored_unrooted, &unrooted),
            "the unrooted generation must return the exact guest Arc"
        );
        assert!(
            !Arc::ptr_eq(&stored_unrooted, &captured),
            "the captured Arc must not be stored on an admitted generation"
        );
    }

    // No temporary clone is alive between these counts and the collections.
    let unrooted_count_before = Arc::strong_count(&unrooted);

    let frame = heap_with(&mut run.vm, |heap| heap.enter_roots(1));
    heap_with(&mut run.vm, |heap| {
        heap.write_closure_root(frame, 0, rooted_handle)
    });
    let first = heap_collect(&mut run.vm);
    assert_eq!(
        first.live_nodes, 1,
        "only the explicitly rooted closure may survive the first collection"
    );
    assert_eq!(
        first.marked_nodes, 1,
        "mark must follow the one explicit closure root, not the callable carrier"
    );
    assert!(
        heap_with(&mut run.vm, |heap| Ok(heap.closure_alive(rooted_handle))),
        "the explicitly rooted registered closure must stay live"
    );
    {
        let stored = heap_with(&mut run.vm, |heap| heap.closure_callable(rooted_handle));
        assert!(
            Arc::ptr_eq(&stored, &rooted),
            "the rooted generation must still return the exact Arc"
        );
    }
    assert!(
        !heap_with(&mut run.vm, |heap| Ok(heap.closure_alive(unrooted_handle))),
        "the independently registered unrooted closure must be collected"
    );
    assert!(
        matches!(
            heap_try(&mut run.vm, |heap| heap.closure_callable(unrooted_handle)),
            Err(HeapError::StaleHandle)
        ),
        "the collected generation must refuse its old handle"
    );
    let unrooted_count_after = Arc::strong_count(&unrooted);
    eprintln!(
        "LUA_CALLABLE_CARRIER unrooted strong before={unrooted_count_before} after={unrooted_count_after} live={} marked={}",
        first.live_nodes, first.marked_nodes
    );
    assert_eq!(
        unrooted_count_after,
        unrooted_count_before - 1,
        "the collected slot must release exactly its one Arc; the VM stack and the test Arcs stay"
    );
    assert!(
        unrooted_count_after > 0,
        "the callable must stay alive outside the slot; this is not a last-Arc drop"
    );
    assert_eq!(
        heap_with(&mut run.vm, |heap| Ok(heap
            .slot_generation(unrooted_handle.slot())
            .expect("the reused slot stays in the vector"))),
        unrooted_generation + 1,
        "release must advance the slot generation"
    );

    // Remove the only Lua root and collect. The slot's one Arc is released
    // even though the VM stack and the test still own live Arcs.
    let rooted_count_before = Arc::strong_count(&rooted);
    heap_with(&mut run.vm, |heap| heap.clear_root(frame, 0));
    let second = heap_collect(&mut run.vm);
    assert_eq!(
        second.live_nodes, 0,
        "both registered generations are unreachable after the only root is cleared"
    );
    let rooted_count_after = Arc::strong_count(&rooted);
    eprintln!(
        "LUA_CALLABLE_CARRIER rooted strong before={rooted_count_before} after={rooted_count_after} live={}",
        second.live_nodes
    );
    assert_eq!(
        rooted_count_after,
        rooted_count_before - 1,
        "the retired slot must release exactly its one Arc"
    );
    assert!(
        rooted_count_after > 0,
        "the VM stack still owns the callable Arc after the slot release"
    );
    assert!(
        !heap_with(&mut run.vm, |heap| Ok(heap.closure_alive(rooted_handle))),
        "the closure whose only root was removed must be collected"
    );
    assert!(
        matches!(
            heap_try(&mut run.vm, |heap| heap.closure_callable(rooted_handle)),
            Err(HeapError::StaleHandle)
        ),
        "the released rooted generation must refuse its old handle"
    );
    heap_with(&mut run.vm, |heap| heap.leave_roots(frame));

    // Reuse the freed slot at the next generation. It must start unbound, and
    // the old handles stay stale, so the old Arc cannot be recovered from the
    // new generation.
    let reused = heap_with(&mut run.vm, |heap| heap.alloc_closure());
    assert_eq!(
        reused.slot(),
        rooted_handle.slot(),
        "the freed root slot must be the next free slot"
    );
    assert_eq!(
        reused.generation(),
        rooted_handle.generation() + 1,
        "the reused slot must take the next generation"
    );
    assert!(
        matches!(
            heap_try(&mut run.vm, |heap| heap.closure_callable(reused)),
            Err(HeapError::CarrierNotBound)
        ),
        "a reused generation must start with no carrier"
    );
    assert!(
        matches!(
            heap_try(&mut run.vm, |heap| heap.closure_callable(rooted_handle)),
            Err(HeapError::StaleHandle)
        ),
        "the old rooted handle must stay stale after slot reuse"
    );
    assert!(
        matches!(
            heap_try(&mut run.vm, |heap| heap.closure_callable(unrooted_handle)),
            Err(HeapError::StaleHandle)
        ),
        "the old unrooted handle must stay stale after its slot was reused"
    );

    // A same-shape closure from another heap is a different domain, and a
    // closure handle naming a live table is the wrong kind. Both are refused
    // before any carrier is read. The store-side refusal for these handles is
    // covered by the storage module's own unit test.
    let mut foreign_heap = LuaHeap::new(HeapLimits::production());
    let foreign = foreign_heap.alloc_closure().expect("foreign closure");
    heap_with(&mut run.vm, |heap| {
        assert!(
            matches!(heap.closure_callable(foreign), Err(HeapError::WrongDomain)),
            "a foreign-domain handle must be refused"
        );
        Ok(())
    });

    let table = heap_with(&mut run.vm, |heap| heap.alloc());
    let wrong_kind = heap_with(&mut run.vm, |heap| Ok(heap.closure_handle_for_table(table)));
    heap_with(&mut run.vm, |heap| {
        assert!(
            matches!(heap.closure_callable(wrong_kind), Err(HeapError::WrongKind)),
            "a closure handle naming a live table must be the wrong kind"
        );
        Ok(())
    });
    heap_with(&mut run.vm, |heap| heap.rollback(table));
}
