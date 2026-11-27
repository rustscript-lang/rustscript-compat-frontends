//! Exact Core callable ownership on a Lua closure slot generation.
//!
//! One real `.lua` file is fed through the real lexer, parser, resolver and
//! private completion lowering, compiled through the fixed Core and run in one
//! interpreter VM with JIT disabled. The fixture writes three real callables
//! and hands each to the private `note` import as an ordinary guest argument.
//! That guest binding call is the issuance evidence: the host receives the
//! exact `SharedCallable` the same VM produced, and this module never builds a
//! `CallableValue` itself.
//!
//! The host registers the two no-capture chunk-level callables on two live
//! closure nodes. Core's Lua frontend compiles every function literal as
//! `CallableKind::Closure` and attaches an empty `CallableEnvironment`
//! container even when the compiled prototype declares no captures, so the
//! narrow admission is the exact capture count, not the container bit. The run
//! log records `env=true captures=0` for both admitted functions and
//! `captures=1` for the refused captured function; a literal `env.is_none()`
//! admission has no producer in this frontend. After the single `Vm::run` the
//! test uses the real heap collector:
//!
//! - the registered node with an explicit Lua root survives a real collection
//!   and still returns the exact `Arc`;
//! - the independently registered node without a root is really collected even
//!   though the VM and the witness still hold live Arcs to that callable, so
//!   the carrier is not a root or a mark edge;
//! - clearing the only root and collecting drops exactly the one Arc the slot
//!   held, measured on the witness Arc without a temporary clone and checked
//!   against the cleared slot;
//! - the next generation of the same slot starts unbound and rejects the old
//!   handle, then accepts and returns another real Arc.
//!
//! Direct controls after that run cover a second bind with a different Arc,
//! an unbound generation, a foreign-domain same-shape handle, a wrong-kind
//! handle naming a live table slot, and a callable with a real capture. No
//! later VM run and no fabricated callable is used. The capture graph, real
//! finalizers, queue dequeue, and production admission are outside this slice.

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
use crate::heap::{self, ClosureRef, HeapError, HeapLimits, LuaHeap};

const FIXTURE: &str = "callable_carrier.lua";

/// The stage label the last marker host recorded. `note` attaches it to the
/// registration it is about to perform.
static STAGE: Mutex<Option<String>> = Mutex::new(None);

/// The observation of the one admitted run. This module runs one test body, so
/// a process-wide slot carries the same record a per-run handle would.
static WITNESS: Mutex<Option<Witness>> = Mutex::new(None);

#[derive(Clone, Debug)]
struct Registration {
    stage: String,
    callable: Arc<CallableValue>,
    /// `Some` only for the two no-capture callables the host registered.
    handle: Option<ClosureRef>,
    /// Core's `env` field. The Lua frontend's `CallableKind::Closure` builds an
    /// empty `CallableEnvironment` even for a chunk-level function with no
    /// captures, so this is the container bit, not the capture count.
    environment: bool,
    /// Exact compiled prototype capture count read from the same VM.
    captures: usize,
}

#[derive(Clone, Debug, Default)]
struct Witness {
    marks: Vec<String>,
    registrations: Vec<Registration>,
}

fn carrier_catalog() -> &'static Arc<HostApiCatalog> {
    static CATALOG: OnceLock<Arc<HostApiCatalog>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut builder = HostApiBuilder::new();
        builder.function(HostFunctionSchema::with_return(
            "mark",
            vec![HostParamSchema::value("label", HostTypeSchema::String)],
            HostTypeSchema::Int,
        ));
        // One ordinary guest-written callable argument. The return is an
        // integer the fixture ignores; the registration is the observation.
        builder.function(HostFunctionSchema::with_return(
            "note",
            vec![HostParamSchema::value("target", HostTypeSchema::Unknown)],
            HostTypeSchema::Int,
        ));
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
    registry.register_exact_static("note", 1, catalog_schema("note"), host_note)?;
    registry.bind_vm_cached(vm)
}

fn host_fault(message: impl Into<String>) -> VmError {
    VmError::HostError(message.into())
}

fn heap_fault(error: HeapError) -> VmError {
    VmError::HostError(format!("lua heap: {error:?}"))
}

/// Records the stage label. The next `note` attaches it to its registration.
fn host_mark(_vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
    let [Value::String(label)] = args else {
        return Err(host_fault("mark expects one string label"));
    };
    let label = label.as_str().to_string();
    *STAGE.lock().expect("callable carrier stage") = Some(label.clone());
    with_witness(|witness| witness.marks.push(label));
    Ok(CallOutcome::Return(CallReturn::one(Value::Int(0))))
}

/// Receives one exact guest-written callable. A callable whose compiled
/// prototype has no capture slots is registered on a fresh closure node
/// through the private heap entry; a callable with a real capture is recorded
/// but not allocated, because the narrow refusal is exercised directly after
/// the run.
fn host_note(vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
    let [value] = args else {
        return Err(host_fault("note expects one callable argument"));
    };
    let Value::Callable(callable) = value else {
        return Err(host_fault(format!(
            "note expected a guest callable, got {value:?}"
        )));
    };
    let stage = STAGE
        .lock()
        .expect("callable carrier stage")
        .take()
        .unwrap_or_else(|| "unlabelled".to_string());
    let environment = callable.env.is_some();
    let prototype = vm
        .program()
        .callable_prototypes
        .get(callable.prototype_id as usize)
        .ok_or_else(|| host_fault("the guest callable names no compiled prototype"))?;
    let captures = prototype.capture_slots.len();
    eprintln!(
        "LUA_CALLABLE_CARRIER stage={stage} kind={:?} env={} captures={captures}",
        callable.kind, environment
    );
    let mut handle = None;
    if captures == 0 {
        let mut host = vm.host_context();
        let closure = heap::with_heap(&mut host, LuaHeap::alloc_closure).map_err(heap_fault)?;
        heap::with_heap(&mut host, |heap| {
            heap.bind_closure_callable(closure, Arc::clone(callable), captures)
        })
        .map_err(heap_fault)?;
        handle = Some(closure);
    }
    with_witness(|witness| {
        witness.registrations.push(Registration {
            stage,
            callable: Arc::clone(callable),
            handle,
            environment,
            captures,
        });
    });
    Ok(CallOutcome::Return(CallReturn::one(Value::Int(0))))
}

fn with_witness<T>(body: impl FnOnce(&mut Witness) -> T) -> T {
    let mut guard = WITNESS.lock().expect("callable carrier witness");
    let witness = guard
        .as_mut()
        .expect("the callable carrier witness must be installed before the run");
    body(witness)
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
    witness: Witness,
    fuel_consumed: u64,
}

fn run_fixture() -> CarrierRun {
    *STAGE.lock().expect("callable carrier stage") = None;
    *WITNESS.lock().expect("callable carrier witness") = Some(Witness::default());

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
    let witness = WITNESS
        .lock()
        .expect("callable carrier witness")
        .take()
        .unwrap_or_else(|| panic!("callable carrier: the witness was not installed"));
    CarrierRun {
        vm: runtime,
        status,
        stack,
        witness,
        fuel_consumed,
    }
}

/// The one completion array of the chunk: `[true, rooted, unrooted, captured]`.
fn exit_items(stack: &[Value]) -> &[Value] {
    let [Value::Array(items)] = stack else {
        panic!("callable carrier: expected the one completion array, got {stack:?}");
    };
    items.as_slice()
}

fn same_callable(left: &Value, right: &Arc<CallableValue>) -> bool {
    match left {
        Value::Callable(left) => Arc::ptr_eq(left, right),
        _ => false,
    }
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
        run.witness.marks,
        [
            "register-rooted",
            "register-unrooted",
            "register-environment",
            "done"
        ],
        "callable carrier: each real guest binding must reach the host once"
    );
    assert_eq!(
        run.witness.registrations.len(),
        3,
        "callable carrier: three guest-written callables must reach the host"
    );

    let mut registrations = run.witness.registrations.into_iter();
    let rooted = registrations.next().expect("rooted registration");
    let unrooted = registrations.next().expect("unrooted registration");
    let environment = registrations.next().expect("environment registration");
    assert!(
        registrations.next().is_none(),
        "callable carrier: no extra registration"
    );
    assert_eq!(rooted.stage, "register-rooted");
    assert_eq!(unrooted.stage, "register-unrooted");
    assert_eq!(environment.stage, "register-environment");
    assert_eq!(
        rooted.captures, 0,
        "the chunk-level function has no captures"
    );
    assert_eq!(
        unrooted.captures, 0,
        "the chunk-level function has no captures"
    );
    assert!(
        rooted.environment && unrooted.environment,
        "Core attaches an empty environment container to every Lua closure"
    );
    assert!(
        environment.captures > 0,
        "the captured nested function must have a real capture"
    );
    assert!(environment.environment);
    assert!(rooted.handle.is_some());
    assert!(unrooted.handle.is_some());
    assert!(
        environment.handle.is_none(),
        "the environment-bearing value must not be registered during the run"
    );
    assert!(
        !Arc::ptr_eq(&rooted.callable, &unrooted.callable),
        "the two chunk-level functions must be distinct Core Arcs"
    );

    let items = exit_items(&run.stack);
    assert_eq!(
        items.len(),
        4,
        "callable carrier: [true] plus three callables"
    );
    assert_eq!(items[0], Value::Bool(true));
    assert!(
        same_callable(&items[1], &rooted.callable),
        "the chunk must return the first registered Arc"
    );
    assert!(
        same_callable(&items[2], &unrooted.callable),
        "the chunk must return the second registered Arc"
    );
    assert!(
        same_callable(&items[3], &environment.callable),
        "the chunk must return the captured callable"
    );

    let rooted_handle = rooted.handle.expect("rooted handle");
    let unrooted_handle = unrooted.handle.expect("unrooted handle");
    assert_eq!(rooted_handle.domain(), unrooted_handle.domain());
    assert_ne!(rooted_handle.slot(), unrooted_handle.slot());
    let unrooted_generation = unrooted_handle.generation();

    // A live generation holds one carrier. A different real Arc must not
    // replace it, and the original identity must still come back.
    heap_with(&mut run.vm, |heap| {
        assert_eq!(
            heap.bind_closure_callable(
                unrooted_handle,
                Arc::clone(&rooted.callable),
                rooted.captures,
            ),
            Err(HeapError::CarrierAlreadyBound)
        );
        Ok(())
    });
    {
        let stored = heap_with(&mut run.vm, |heap| heap.closure_callable(unrooted_handle));
        assert!(
            Arc::ptr_eq(&stored, &unrooted.callable),
            "a refused rebind must leave the original Arc"
        );
    }

    // No temporary clone is alive between these counts and the collections.
    let unrooted_count_before = Arc::strong_count(&unrooted.callable);

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
            Arc::ptr_eq(&stored, &rooted.callable),
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
    let unrooted_count_after = Arc::strong_count(&unrooted.callable);
    eprintln!(
        "LUA_CALLABLE_CARRIER unrooted strong before={unrooted_count_before} after={unrooted_count_after} live={} marked={}",
        first.live_nodes, first.marked_nodes
    );
    assert_eq!(
        unrooted_count_after,
        unrooted_count_before - 1,
        "the collected slot must release exactly its one Arc; the VM and witness Arcs stay"
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
    // even though the VM stack and the witness still own live Arcs.
    let rooted_count_before = Arc::strong_count(&rooted.callable);
    heap_with(&mut run.vm, |heap| heap.clear_root(frame, 0));
    let second = heap_collect(&mut run.vm);
    assert_eq!(
        second.live_nodes, 0,
        "both registered generations are unreachable after the only root is cleared"
    );
    let rooted_count_after = Arc::strong_count(&rooted.callable);
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
    // only the newly registered Arc may be returned. The old handles stay
    // stale, so the old Arc cannot be recovered from the new generation.
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
    heap_with(&mut run.vm, |heap| {
        heap.bind_closure_callable(reused, Arc::clone(&unrooted.callable), unrooted.captures)
    });
    {
        let stored = heap_with(&mut run.vm, |heap| heap.closure_callable(reused));
        assert!(
            Arc::ptr_eq(&stored, &unrooted.callable),
            "the reused generation must return the newly registered real Arc"
        );
        assert!(
            !Arc::ptr_eq(&stored, &rooted.callable),
            "the reused generation must not return the old Arc"
        );
    }
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
    // before any carrier is read or written.
    let mut foreign_heap = LuaHeap::new(HeapLimits::production());
    let foreign = foreign_heap.alloc_closure().expect("foreign closure");
    heap_with(&mut run.vm, |heap| {
        assert!(
            matches!(heap.closure_callable(foreign), Err(HeapError::WrongDomain)),
            "a foreign-domain handle must be refused"
        );
        assert_eq!(
            heap.bind_closure_callable(foreign, Arc::clone(&unrooted.callable), unrooted.captures),
            Err(HeapError::WrongDomain)
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
        assert_eq!(
            heap.bind_closure_callable(
                wrong_kind,
                Arc::clone(&unrooted.callable),
                unrooted.captures
            ),
            Err(HeapError::WrongKind)
        );
        Ok(())
    });
    heap_with(&mut run.vm, |heap| heap.rollback(table));

    // The environment-bearing callable is a real value from the same file. Its
    // narrow refusal must leave the fresh generation unbound and hold no Arc.
    let environment_slot = heap_with(&mut run.vm, |heap| heap.alloc_closure());
    let environment_count_before = Arc::strong_count(&environment.callable);
    heap_with(&mut run.vm, |heap| {
        assert_eq!(
            heap.bind_closure_callable(
                environment_slot,
                Arc::clone(&environment.callable),
                environment.captures,
            ),
            Err(HeapError::CallableHasEnvironment)
        );
        Ok(())
    });
    assert_eq!(
        Arc::strong_count(&environment.callable),
        environment_count_before,
        "the refused environment bind must not retain an Arc"
    );
    assert!(
        matches!(
            heap_try(&mut run.vm, |heap| heap.closure_callable(environment_slot)),
            Err(HeapError::CarrierNotBound)
        ),
        "the refused environment bind must leave the generation unbound"
    );
}
