//! Dynamic Lua completion transport through the real `.lua` file entry.
//!
//! Every input is a real Lua 5.2-compat file under
//! `tests/fixtures/luajit/completion/`. The candidate plugin feeds the real
//! lexer/parser/resolver, then lowers each Lua function to the private
//! completion `[Bool, ...values]`, compiles through the fixed Core and runs one
//! interpreter VM (JIT disabled). No LuaHeap is involved; the payload table is
//! compared by Core array identity only. `mark`, `note`, and `fault` are real
//! host functions injected by the test.
//!
//! The default production `LuaPlugin` still runs the line lowerer and does not
//! support these chunks. That legacy rejection is recorded as the first
//! unsupported layer for the same real input; a compile-time rejection is not
//! counted as a business RED. The candidate path then executes the same file
//! and the behavioral assertions are the real RED/GREEN boundary.

#[allow(dead_code)]
#[path = "../../src/frontends/lua/ast.rs"]
mod ast;
#[path = "../../src/frontends/lua/lexer.rs"]
mod lexer;
#[path = "../../src/frontends/lua/lower.rs"]
mod lower;
#[path = "../../src/frontends/lua/parser.rs"]
mod parser;
#[path = "../../src/frontends/lua/resolve.rs"]
mod resolve;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use vm::compiler::HostCallResolver;
use vm::vm::{VmExecutionFrameSnapshot, VmFrameContinuation};
use vm::{
    CallOutcome, CallReturn, CompileSourceFileOptions, FrontendImportSyntax, FrontendIr,
    HostApiBuilder, HostApiCatalog, HostFunction, HostFunctionSchema, HostParamSchema,
    HostTypeSchema, OpCode, ParseError, Program, SourceError, SourceFlavor, SourcePathError,
    SourcePlugin, Value, Vm, VmError, VmStatus,
};

const FUEL_BUDGET: u64 = 1_000_000;
const FUEL_CHECK_INTERVAL: u32 = 1;

const DEFAULT_ENTRY_FIXTURES: &[&str] = &[
    "argument-order.lua",
    "dynamic-error.lua",
    "dynamic-success.lua",
    "dynamic-false-table.lua",
    "host-return-false.lua",
    "ordinary-zero.lua",
    "ordinary-one-nil.lua",
    "protected-zero.lua",
    "protected-one-nil.lua",
    "scalar-zero.lua",
    "host-fault.lua",
    "uncaught-error.lua",
];

struct CompletionLuaPlugin;

static COMPLETION_LUA_PLUGIN: CompletionLuaPlugin = CompletionLuaPlugin;

impl SourcePlugin for CompletionLuaPlugin {
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
        let resolver = HostCallResolver::new(catalog());
        lower::lower_source_with(source, Some(&resolver))
    }
}

/// Test-only host catalog for the three real host functions the fixtures use.
/// The candidate resolves every host call through this catalog, so the Core
/// module loader never sees them as imported externs.
fn catalog() -> &'static Arc<HostApiCatalog> {
    static CATALOG: OnceLock<Arc<HostApiCatalog>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut builder = HostApiBuilder::new();
        builder.function(HostFunctionSchema::with_return(
            "mark",
            vec![HostParamSchema::value("label", HostTypeSchema::String)],
            HostTypeSchema::Int,
        ));
        builder.function(HostFunctionSchema::with_return(
            "note",
            vec![HostParamSchema::value("value", HostTypeSchema::Unknown)],
            HostTypeSchema::Bool,
        ));
        builder.function(HostFunctionSchema::with_return(
            "fault",
            Vec::new(),
            HostTypeSchema::Unknown,
        ));
        builder.function(HostFunctionSchema::with_return(
            "tick",
            vec![HostParamSchema::value("label", HostTypeSchema::String)],
            HostTypeSchema::Int,
        ));
        Arc::new(builder.build().expect("completion host catalog"))
    })
}

fn candidate_options() -> CompileSourceFileOptions {
    CompileSourceFileOptions::new()
        .with_source_plugin(&COMPLETION_LUA_PLUGIN)
        .with_host_api_catalog(Arc::clone(catalog()))
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("luajit")
        .join("completion")
        .join(name)
}

/// Records the first layer at which the default entry refuses each real input.
///
/// The assertion is only that the default entry refuses the file; the message
/// text is printed, never matched, so this control does not freeze a legacy
/// diagnostic.
#[test]
fn default_entry_records_first_unsupported_layer() {
    for name in DEFAULT_ENTRY_FIXTURES {
        let path = fixture_path(name);
        let result =
            vm::compile_source_file_with_options(&path, pd_vm_compat_frontends::compile_options());
        match result {
            Ok(_) => panic!("default entry must not support the completion fixture {name}"),
            Err(error) => {
                let layer = default_entry_layer(&error);
                eprintln!(
                    "LUA_COMPLETION default-entry fixture={name} first-layer={layer} error={error:?}"
                );
            }
        }
    }
}

/// Coarse first-layer label: parse and compile failures are distinct variants.
/// This is a label, not a text match, and no variant is treated as a
/// behavioral result.
fn default_entry_layer(error: &SourcePathError) -> &'static str {
    match error {
        SourcePathError::Source(vm::SourceError::Parse(_)) => "parse",
        SourcePathError::Source(vm::SourceError::Compile(_)) => "compile",
        SourcePathError::SourceWithMap { error, .. } => match error {
            vm::SourceError::Parse(_) => "parse",
            vm::SourceError::Compile(_) => "compile",
        },
        _ => "loader",
    }
}

#[derive(Clone, Debug)]
struct MarkRecord {
    label: String,
    frame_len: usize,
    last_prototype: Option<u32>,
    last_return_ip: Option<usize>,
    parent_prototype: Option<u32>,
    held: HeldParameterWitness,
}

/// Exact observation of the active parent frame's first declared parameter.
///
/// Only `parameter_slots[0]` of the caller prototype is read, with checked
/// arithmetic inside the caller frame, exactly like the accepted Core
/// active-parent witness. No other slot is searched.
#[derive(Clone, Debug, Default)]
struct HeldParameterWitness {
    caller_prototype: Option<u32>,
    declared_slot: Option<u16>,
    slot_inside_caller_frame: bool,
    read_within_bounds: bool,
    value_is_callable: bool,
    callable_prototype: Option<u32>,
    callable_is_script_function: bool,
    callee_prototype: Option<u32>,
}

#[derive(Clone, Default)]
struct Observation {
    marks: Arc<Mutex<Vec<MarkRecord>>>,
    seen: Arc<Mutex<Option<Value>>>,
    ticks: Arc<Mutex<Vec<String>>>,
}

struct MarkHost {
    marks: Arc<Mutex<Vec<MarkRecord>>>,
    seen: Arc<Mutex<Option<Value>>>,
}

impl HostFunction for MarkHost {
    fn call(&mut self, vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
        let [Value::String(label)] = args else {
            return Err(VmError::HostError(
                "mark expects one string label".to_string(),
            ));
        };
        let frames = vm.execution_frames();
        let parent = frames.iter().rev().nth(1);
        let record = MarkRecord {
            label: label.as_str().to_string(),
            frame_len: frames.len(),
            last_prototype: frames.last().and_then(|frame| frame.prototype_id),
            last_return_ip: frames.last().and_then(|frame| match &frame.continuation {
                VmFrameContinuation::ResumeBytecode { return_ip } => Some(*return_ip),
                _ => None,
            }),
            parent_prototype: parent.and_then(|frame| frame.prototype_id),
            held: observe_held_parameter(vm, &frames),
        };
        let _ = &self.seen;
        self.marks.lock().expect("mark log").push(record);
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(0))))
    }
}

fn observe_held_parameter(vm: &Vm, frames: &[VmExecutionFrameSnapshot]) -> HeldParameterWitness {
    let (Some(callee), Some(caller)) = (frames.last(), frames.iter().rev().nth(1)) else {
        return HeldParameterWitness::default();
    };
    let mut witness = HeldParameterWitness {
        caller_prototype: caller.prototype_id,
        callee_prototype: callee.prototype_id,
        ..HeldParameterWitness::default()
    };
    let program = vm.program();
    let Some(prototype) = caller
        .prototype_id
        .and_then(|id| program.callable_prototypes.get(id as usize))
    else {
        return witness;
    };
    let Some(&declared_slot) = prototype.parameter_slots.first() else {
        return witness;
    };
    witness.declared_slot = Some(declared_slot);
    let relative = usize::from(declared_slot);
    if relative >= caller.local_count {
        return witness;
    }
    witness.slot_inside_caller_frame = true;
    let Some(absolute) = caller.local_base.checked_add(relative) else {
        return witness;
    };
    let Some(value) = vm.locals().get(absolute) else {
        return witness;
    };
    witness.read_within_bounds = true;
    let Value::Callable(callable) = value else {
        return witness;
    };
    witness.value_is_callable = true;
    witness.callable_prototype = Some(callable.prototype_id);
    witness.callable_is_script_function = matches!(
        program
            .callable_prototypes
            .get(callable.prototype_id as usize)
            .map(|prototype| prototype.target),
        Some(vm::CallableTarget::ScriptFunction(_))
    );
    witness
}

struct NoteHost {
    seen: Arc<Mutex<Option<Value>>>,
}

impl HostFunction for NoteHost {
    fn call(&mut self, _vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
        let [value] = args else {
            return Err(VmError::HostError("note expects one value".to_string()));
        };
        *self.seen.lock().expect("seen payload") = Some(value.clone());
        // The false here is an ordinary host return value, not a completion.
        Ok(CallOutcome::Return(CallReturn::one(Value::Bool(false))))
    }
}

struct FaultHost;

impl HostFunction for FaultHost {
    fn call(&mut self, _vm: &mut Vm, _args: &[Value]) -> vm::VmResult<CallOutcome> {
        Err(VmError::HostError("completion host fault".to_string()))
    }
}

struct TickHost {
    ticks: Arc<Mutex<Vec<String>>>,
}

impl HostFunction for TickHost {
    fn call(&mut self, _vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
        let [Value::String(label)] = args else {
            return Err(VmError::HostError(
                "tick expects one string label".to_string(),
            ));
        };
        let mut ticks = self.ticks.lock().expect("tick log");
        ticks.push(label.as_str().to_string());
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(
            ticks.len() as i64,
        ))))
    }
}

/// Typed result of the candidate file-execution exit.
///
/// `drive_fixture` runs this consumer on the real path, after `Vm::run` and
/// before any caller observes the run. A normal completion is an ordered value
/// list; an uncaught explicit Lua error is a typed failure carrying exactly its
/// original payload. A real `VmError` is surfaced unchanged, and any other
/// `VmStatus` is reported as itself, never as a guest error.
#[derive(Debug)]
enum CompletionExit {
    /// Normal completion: values in order, preserving zero returns, one nil,
    /// and trailing nils.
    Values(Vec<Value>),
    /// An uncaught explicit guest error: exactly the original payload.
    Uncaught { payload: Value },
    /// The underlying VM stopped on a real core or host error.
    VmFailure(VmError),
    /// The underlying VM reported a status other than `Halted`.
    VmStatus(VmStatus),
    /// The stack did not match the private transport; fail closed.
    MalformedTransport { detail: String },
}

/// Consume the candidate completion at the file-execution exit.
///
/// This is the one place that reads the private `[Bool, ...values]` transport.
/// It is not a per-test assertion and it never guesses provenance from a guest
/// value's shape: only the single chunk result slot of a candidate-compiled
/// file is inspected. `[false, payload]` is the explicit error completion.
fn consume_completion_exit(status: Result<VmStatus, VmError>, stack: Vec<Value>) -> CompletionExit {
    let status = match status {
        Err(error) => return CompletionExit::VmFailure(error),
        Ok(status) => status,
    };
    if status != VmStatus::Halted {
        return CompletionExit::VmStatus(status);
    }
    let [Value::Array(items)] = stack.as_slice() else {
        return CompletionExit::MalformedTransport {
            detail: format!("expected exactly one completion array, stack={stack:?}"),
        };
    };
    match items.first() {
        Some(Value::Bool(true)) => CompletionExit::Values(items[1..].to_vec()),
        Some(Value::Bool(false)) if items.len() == 2 => CompletionExit::Uncaught {
            payload: items[1].clone(),
        },
        Some(Value::Bool(false)) => CompletionExit::MalformedTransport {
            detail: format!("error completion must carry exactly one payload: {items:?}"),
        },
        Some(other) => CompletionExit::MalformedTransport {
            detail: format!("completion status is not a Bool: {other:?}"),
        },
        None => CompletionExit::MalformedTransport {
            detail: "completion is empty".to_string(),
        },
    }
}

struct Run {
    stack: Vec<Value>,
    fuel_consumed: u64,
    marks: Vec<MarkRecord>,
    seen: Option<Value>,
    ticks: Vec<String>,
    exit: CompletionExit,
    call_sites: Vec<DecodedCallSite>,
    capture_plans: Vec<CapturePlan>,
}

/// The fixed Core's capture decision for one closure prototype, read from the
/// compiled program. `Copy` keeps the source binding usable; `Move` detaches
/// it at `BindCallable` time.
#[derive(Clone, Debug)]
struct CapturePlan {
    source_slots: Vec<u16>,
    modes: Vec<vm::CaptureBindingMode>,
}

fn capture_plans(program: &Program) -> Vec<CapturePlan> {
    program
        .callable_prototypes
        .iter()
        .filter(|prototype| !prototype.capture_modes.is_empty())
        .map(|plan| CapturePlan {
            source_slots: plan.capture_source_slots.clone(),
            modes: plan.capture_modes.clone(),
        })
        .collect()
}

fn drive_fixture(name: &str) -> Run {
    let path = fixture_path(name);
    let compiled = vm::compile_source_file_with_options(&path, candidate_options())
        .unwrap_or_else(|error| panic!("{name}: candidate compile failed: {error:?}"));
    let call_sites = decoded_call_sites(&compiled.program);
    let capture_plans = capture_plans(&compiled.program);
    let mut vm = Vm::new_with_jit_config(
        compiled.program,
        vm::JitConfig {
            enabled: false,
            hot_loop_threshold: 8,
            max_trace_len: 256,
        },
    );
    let observation = Observation::default();
    vm.bind_function(
        "mark",
        Box::new(MarkHost {
            marks: Arc::clone(&observation.marks),
            seen: Arc::clone(&observation.seen),
        }),
    );
    vm.bind_function(
        "note",
        Box::new(NoteHost {
            seen: Arc::clone(&observation.seen),
        }),
    );
    vm.bind_function("fault", Box::new(FaultHost));
    vm.bind_function(
        "tick",
        Box::new(TickHost {
            ticks: Arc::clone(&observation.ticks),
        }),
    );
    vm.set_fuel_check_interval(FUEL_CHECK_INTERVAL)
        .unwrap_or_else(|error| panic!("{name}: fuel interval: {error}"));
    vm.set_fuel(FUEL_BUDGET);
    let status = vm.run();
    let remaining = vm
        .get_fuel()
        .unwrap_or_else(|| panic!("{name}: fuel metering missing"));
    let fuel_consumed = FUEL_BUDGET
        .checked_sub(remaining)
        .unwrap_or_else(|| panic!("{name}: fuel increased"));
    let marks = observation.marks.lock().expect("mark log").clone();
    let seen = observation.seen.lock().expect("seen payload").clone();
    let ticks = observation.ticks.lock().expect("tick log").clone();
    let stack = vm.stack().to_vec();
    let status_log = format!("{status:?}");
    let exit = consume_completion_exit(status, stack.clone());
    if let CompletionExit::MalformedTransport { detail } = &exit {
        eprintln!("LUA_COMPLETION malformed-transport fixture={name} detail={detail}");
    }
    eprintln!(
        "LUA_COMPLETION fixture={name} fuel_budget={FUEL_BUDGET} check_interval={FUEL_CHECK_INTERVAL} consumed={fuel_consumed} remaining={remaining} jit=disabled status={status_log} exit={exit:?} stack={stack:?} marks={:?} captures={capture_plans:?}",
        marks
            .iter()
            .map(|mark| mark.label.as_str())
            .collect::<Vec<_>>()
    );
    Run {
        stack,
        fuel_consumed,
        marks,
        seen,
        ticks,
        exit,
        call_sites,
        capture_plans,
    }
}

fn run_stopped(name: &str) -> Run {
    let run = drive_fixture(name);
    assert!(run.fuel_consumed > 0, "{name}: recorded no fuel");
    assert!(
        run.fuel_consumed < FUEL_BUDGET,
        "{name}: exhausted the fuel budget"
    );
    run
}

fn run_halted(name: &str) -> Run {
    let run = run_stopped(name);
    match &run.exit {
        CompletionExit::Values(_) => {}
        other => panic!("{name}: expected a normal completion exit, got {other:?}"),
    }
    run
}

/// Decodes the whole instruction stream with `OpCode::operand_len()` and
/// returns every `CallValue` site with its exact successor and owning
/// prototype.
#[derive(Clone, Copy, Debug)]
struct DecodedCallSite {
    ip: usize,
    argc: u8,
    return_ip: usize,
    owner: Option<u32>,
}

fn decoded_call_sites(program: &Program) -> Vec<DecodedCallSite> {
    let code = &program.code;
    let mut ip = 0usize;
    let mut sites = Vec::new();
    while ip < code.len() {
        let op = OpCode::try_from(code[ip]).expect("invalid opcode");
        let next = ip + 1 + op.operand_len();
        assert!(next <= code.len(), "truncated instruction at {ip}");
        if op == OpCode::CallValue {
            let owners: Vec<_> = program
                .function_regions
                .iter()
                .filter(|region| region.start_ip as usize <= ip && next <= region.end_ip as usize)
                .collect();
            let [owner] = owners.as_slice() else {
                panic!("expected a unique caller region for CallValue at {ip}");
            };
            sites.push(DecodedCallSite {
                ip,
                argc: code[ip + 1],
                return_ip: next,
                owner: owner.prototype_id,
            });
        }
        ip = next;
    }
    sites
}

/// Binds the executed marker to the one decoded CallValue whose successor is
/// the executed callee's resume address, owned by the active caller frame.
fn assert_dynamic_call_witness(run: &Run, callee_label: &str, expected_argc: u8) {
    let mark = run
        .marks
        .iter()
        .find(|mark| mark.label == callee_label)
        .unwrap_or_else(|| panic!("missing {callee_label} marker: {:?}", labels(run)));
    let caller = mark
        .parent_prototype
        .expect("callee marker must have a caller frame");
    let return_ip = mark
        .last_return_ip
        .expect("callee marker must resume through a CallValue continuation");
    let sites = &run.call_sites;
    let matching: Vec<_> = sites
        .iter()
        .filter(|site| {
            site.owner == Some(caller) && site.return_ip == return_ip && site.argc == expected_argc
        })
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "expected one executed CallValue (owner={caller:?}, return_ip={return_ip}, argc={expected_argc}); sites={sites:?}"
    );
    let site = matching[0];
    assert_eq!(
        site.ip + 2,
        site.return_ip,
        "CallValue carries one argc operand byte"
    );
    let held = &mark.held;
    assert_eq!(
        held.caller_prototype,
        Some(caller),
        "held argument must be read from the caller frame: {held:?}"
    );
    assert_eq!(
        held.callee_prototype, mark.last_prototype,
        "the witness must compare against the executing callee frame: {held:?}"
    );
    assert!(
        held.declared_slot.is_some(),
        "the caller prototype must declare a first parameter: {held:?}"
    );
    assert!(
        held.slot_inside_caller_frame,
        "the declared parameter slot must be inside the parent frame: {held:?}"
    );
    assert!(held.read_within_bounds, "{held:?}");
    assert!(held.value_is_callable, "{held:?}");
    assert_eq!(
        held.callable_prototype, mark.last_prototype,
        "the held callable must be the executing callee: {held:?}"
    );
    assert!(
        held.callable_is_script_function,
        "the held callable prototype target must be a script function: {held:?}"
    );
    assert_ne!(
        mark.last_prototype, mark.parent_prototype,
        "the dynamic callee must not be the caller frame"
    );
}

fn labels(run: &Run) -> Vec<&str> {
    run.marks.iter().map(|mark| mark.label.as_str()).collect()
}

fn completed_values<'a>(name: &str, run: &'a Run) -> &'a [Value] {
    match &run.exit {
        CompletionExit::Values(values) => values,
        other => panic!("{name}: expected a normal completion exit, got {other:?}"),
    }
}

fn uncaught_payload<'a>(name: &str, run: &'a Run) -> &'a Value {
    match &run.exit {
        CompletionExit::Uncaught { payload } => payload,
        other => panic!("{name}: expected a typed uncaught guest error, got {other:?}"),
    }
}

fn same_array(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Array(left), Value::Array(right)) => Arc::ptr_eq(left, right),
        _ => false,
    }
}

fn payload_of(name: &str, run: &Run) -> Value {
    run.seen
        .clone()
        .unwrap_or_else(|| panic!("{name}: note never observed a payload"))
}

fn compile_message(name: &str) -> String {
    let path = fixture_path(name);
    let error = vm::compile_source_file_with_options(&path, candidate_options())
        .err()
        .unwrap_or_else(|| panic!("{name}: must be refused at lowering/compile"));
    match error {
        SourcePathError::SourceWithMap {
            error: SourceError::Parse(parse),
            ..
        }
        | SourcePathError::Source(SourceError::Parse(parse)) => parse.message,
        other => panic!("{name}: expected a parse-layer refusal, got {other:?}"),
    }
}

#[test]
fn dynamic_error_propagates_through_ordinary_call_and_pcall_recovers() {
    let run = run_halted("dynamic-error.lua");
    assert_eq!(
        labels(&run),
        [
            "outer",
            "middle",
            "leaf",
            "inner-caught",
            "outer-tail",
            "done"
        ],
        "the ordinary caller tail must be skipped and the outer tail kept"
    );
    let values = completed_values("dynamic-error.lua", &run);
    assert_eq!(
        values.len(),
        2,
        "pcall returns exactly false and the payload"
    );
    assert_eq!(values[0], Value::Bool(false));
    let observed = payload_of("dynamic-error.lua", &run);
    assert!(
        same_array(&values[1], &observed),
        "the caught error must keep the original table object"
    );
    assert_eq!(
        run.marks.iter().filter(|mark| mark.label == "leaf").count(),
        1,
        "the dynamic callee must run exactly once"
    );
    assert_eq!(
        run.marks
            .iter()
            .filter(|mark| mark.label == "middle")
            .count(),
        1
    );
    let outer_frame = run
        .marks
        .iter()
        .find(|mark| mark.label == "outer")
        .expect("outer marker")
        .frame_len;
    let leaf_frame = run
        .marks
        .iter()
        .find(|mark| mark.label == "leaf")
        .expect("leaf marker")
        .frame_len;
    assert_eq!(
        leaf_frame,
        outer_frame + 2,
        "the dynamic callee runs two frames below the outer frame"
    );
    assert_dynamic_call_witness(&run, "leaf", 1);
}

#[test]
fn call_arguments_evaluate_left_to_right_before_one_callee_run() {
    let run = run_halted("argument-order.lua");
    assert_eq!(
        labels(&run),
        ["middle", "leaf", "middle-tail", "done"],
        "both arguments run before the single callee"
    );
    assert_eq!(
        run.ticks,
        ["first", "second"],
        "dynamic call arguments must evaluate left to right"
    );
    assert_eq!(
        run.marks.iter().filter(|mark| mark.label == "leaf").count(),
        1,
        "the dynamic callee must run exactly once"
    );
    assert_eq!(
        completed_values("argument-order.lua", &run),
        &[Value::Int(2)][..],
        "the second argument is the returned value, unpadded"
    );
    let Value::Int(first) = run.seen.clone().expect("note observed the first argument") else {
        panic!("note must observe the first tick value");
    };
    assert_eq!(first, 1);
    assert_dynamic_call_witness(&run, "leaf", 2);
}

#[test]
fn dynamic_success_keeps_the_ordinary_tail_and_true() {
    let run = run_halted("dynamic-success.lua");
    assert_eq!(
        labels(&run),
        [
            "outer",
            "middle",
            "leaf",
            "middle-tail",
            "inner-caught",
            "outer-tail",
            "done"
        ]
    );
    let values = completed_values("dynamic-success.lua", &run);
    assert_eq!(values.len(), 2);
    assert_eq!(values[0], Value::Bool(true));
    assert!(same_array(
        &values[1],
        &payload_of("dynamic-success.lua", &run)
    ));
    assert_dynamic_call_witness(&run, "leaf", 1);
}

#[test]
fn false_first_table_item_is_data_not_an_error() {
    let run = run_halted("dynamic-false-table.lua");
    assert_eq!(
        labels(&run),
        [
            "outer",
            "middle",
            "leaf",
            "middle-tail",
            "inner-caught",
            "outer-tail",
            "done"
        ],
        "a false table item must not turn the success path into an error"
    );
    let values = completed_values("dynamic-false-table.lua", &run);
    assert_eq!(values[0], Value::Bool(true));
    let Value::Array(payload) = &values[1] else {
        panic!("payload must be the original table");
    };
    assert_eq!(payload.first(), Some(&Value::Bool(false)));
    assert!(same_array(
        &values[1],
        &payload_of("dynamic-false-table.lua", &run)
    ));
}

#[test]
fn host_false_return_is_not_a_completion_status() {
    let run = run_halted("host-return-false.lua");
    assert_eq!(
        labels(&run),
        ["middle", "leaf", "leaf-tail", "middle-tail", "done"],
        "a false host return value must not stop either tail"
    );
    let values = completed_values("host-return-false.lua", &run);
    assert_eq!(values.len(), 1);
    assert!(same_array(
        &values[0],
        &payload_of("host-return-false.lua", &run)
    ));
}

#[test]
fn ordinary_tail_zero_and_one_nil_stay_distinct() {
    let zero = run_halted("ordinary-zero.lua");
    assert_eq!(labels(&zero), ["zero"]);
    let values = completed_values("ordinary-zero.lua", &zero);
    assert!(
        values.is_empty(),
        "ordinary tail zero returns is zero values"
    );

    let one_nil = run_halted("ordinary-one-nil.lua");
    assert_eq!(labels(&one_nil), ["nil"]);
    let values = completed_values("ordinary-one-nil.lua", &one_nil);
    assert_eq!(values.len(), 1, "one explicit nil is one value");
    assert_eq!(values[0], Value::Null);
}

#[test]
fn protected_tail_zero_and_one_nil_stay_distinct() {
    let zero = run_halted("protected-zero.lua");
    assert_eq!(labels(&zero), ["zero"]);
    assert_eq!(
        completed_values("protected-zero.lua", &zero),
        &[Value::Bool(true)][..],
        "pcall adds exactly its success value"
    );
    assert_dynamic_call_witness(&zero, "zero", 0);

    let one_nil = run_halted("protected-one-nil.lua");
    assert_eq!(labels(&one_nil), ["nil"]);
    assert_eq!(
        completed_values("protected-one-nil.lua", &one_nil),
        &[Value::Bool(true), Value::Null][..]
    );
}

#[test]
fn single_slot_zero_return_binds_nil_with_lazy_guard() {
    let run = run_halted("scalar-zero.lua");
    assert_eq!(labels(&run), ["zero", "scalar-tail", "done"]);
    assert_eq!(
        completed_values("scalar-zero.lua", &run),
        &[Value::Null][..]
    );
    assert_dynamic_call_witness(&run, "zero", 0);
}

#[test]
fn uncaught_explicit_error_is_not_a_normal_chunk_result() {
    let run = run_stopped("uncaught-error.lua");
    assert_eq!(labels(&run), ["leaf"]);
    let payload = uncaught_payload("uncaught-error.lua", &run);
    let observed = payload_of("uncaught-error.lua", &run);
    assert!(
        same_array(payload, &observed),
        "the typed uncaught failure must keep the original table object"
    );
}

#[test]
fn host_error_aborts_without_guest_catch() {
    let run = drive_fixture("host-fault.lua");
    let error = match &run.exit {
        CompletionExit::VmFailure(error) => error,
        other => panic!("a real host error must abort the VM, got {other:?}"),
    };
    assert!(
        matches!(error, VmError::HostError(message) if message == "completion host fault"),
        "unexpected host error: {error:?}"
    );
    assert_eq!(
        labels(&run),
        ["outer", "middle", "leaf"],
        "no marker after the fault and no pcall recovery may run"
    );
    assert!(
        !run.stack.iter().any(|value| matches!(
            value,
            Value::Array(items) if matches!(items.first(), Some(Value::Bool(_)))
        )),
        "the host error must not turn into a guest completion tag: {:?}",
        run.stack
    );
}

#[test]
fn unproven_host_callable_is_refused_at_lowering() {
    let message = compile_message("unproven-callable.lua");
    assert!(
        message.contains("not a proven Lua callable"),
        "unexpected refusal: {message}"
    );
}

#[test]
fn unknown_global_call_is_refused_at_lowering() {
    let message = compile_message("unknown-global.lua");
    assert!(
        message.contains("unsupported global call"),
        "unexpected refusal: {message}"
    );
}

/// Admission RED/GREEN boundary: the candidate lowering must refuse the input
/// before any VM runs. If the current lowering still admits it, the same real
/// fixture is executed and the panic reports the actual runtime gate, so a
/// diagnostic is never assumed from a helper-only assertion.
fn assert_lowering_refusal(name: &str, needle: &str) {
    let path = fixture_path(name);
    match vm::compile_source_file_with_options(&path, candidate_options()) {
        Err(SourcePathError::SourceWithMap {
            error: SourceError::Parse(parse),
            ..
        })
        | Err(SourcePathError::Source(SourceError::Parse(parse))) => {
            eprintln!(
                "LUA_COMPLETION refusal fixture={name} message={}",
                parse.message
            );
            assert!(
                parse.message.contains(needle),
                "{name}: unexpected refusal: {}",
                parse.message
            );
        }
        Err(other) => panic!("{name}: expected a lowering/parse refusal, got {other:?}"),
        Ok(_) => {
            let run = drive_fixture(name);
            panic!(
                "{name}: the fixture must be refused at lowering, but the candidate ran: exit={:?} stack={:?} marks={:?}",
                run.exit,
                run.stack,
                labels(&run)
            );
        }
    }
}

#[test]
fn missing_callable_argument_is_refused_at_lowering() {
    assert_lowering_refusal("missing-callee-argument.lua", "not a proven Lua callable");
}

#[test]
fn pcall_missing_callable_argument_is_refused_at_lowering() {
    assert_lowering_refusal(
        "pcall-missing-callee-argument.lua",
        "not a proven Lua callable",
    );
}

#[test]
fn pcall_forwarded_non_lua_argument_is_refused_at_lowering() {
    assert_lowering_refusal("pcall-forwarded-non-lua.lua", "not a proven Lua callable");
}

#[test]
fn all_lua_multilevel_forwarding_runs() {
    let run = run_halted("forward-all-lua.lua");
    assert_eq!(labels(&run), ["forward", "invoke", "leaf", "done"]);
    assert_eq!(
        completed_values("forward-all-lua.lua", &run),
        &[Value::Bool(true), Value::Int(7)][..]
    );
    assert_dynamic_call_witness(&run, "leaf", 1);
}

fn count_label(run: &Run, label: &str) -> usize {
    run.marks.iter().filter(|mark| mark.label == label).count()
}

/// The forwarding closure in the capture-order fixtures captures exactly one
/// local: the `invoke` binding. The original binding must be copied into the
/// closure cell, not moved out of its slot, because Lua keeps it usable.
fn assert_single_copy_capture(run: &Run, name: &str) {
    assert_eq!(
        run.capture_plans.len(),
        1,
        "{name}: only the forwarding closure may capture: {:?}",
        run.capture_plans
    );
    let plan = &run.capture_plans[0];
    assert_eq!(
        plan.source_slots.len(),
        1,
        "{name}: the forwarding closure captures one binding: {plan:?}"
    );
    assert_eq!(
        plan.modes,
        [vm::CaptureBindingMode::Copy],
        "{name}: the captured original binding must be copied, not moved: {plan:?}"
    );
}

/// Every executed marker must resume through exactly one decoded `CallValue`
/// owned by the active caller frame, with the marker's return address and the
/// expected argc. Repeated executions of the same site stay one site.
fn assert_marker_call_site(run: &Run, mark: &MarkRecord, expected_argc: u8) {
    let return_ip = mark
        .last_return_ip
        .expect("marker must resume through a CallValue continuation");
    let matching: Vec<_> = run
        .call_sites
        .iter()
        .filter(|site| {
            site.owner == mark.parent_prototype
                && site.return_ip == return_ip
                && site.argc == expected_argc
        })
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "expected one CallValue (owner={:?}, return_ip={return_ip}, argc={expected_argc}) for {}; sites={:?}",
        mark.parent_prototype,
        mark.label,
        run.call_sites
    );
    assert_eq!(
        matching[0].ip + 2,
        matching[0].return_ip,
        "CallValue carries one argc operand byte"
    );
}

/// `direct-before.lua` is the layout control: the original `invoke` binding is
/// called before `forward` is created, so no capture has happened yet.
#[test]
fn direct_before_calls_original_binding_control() {
    let run = run_halted("direct-before.lua");
    assert_eq!(
        labels(&run),
        ["invoke", "leaf", "forward", "invoke", "leaf", "done"],
        "the original call and the forwarded call both run before done"
    );
    assert_eq!(
        completed_values("direct-before.lua", &run),
        &[Value::Bool(true), Value::Int(7), Value::Int(5)][..],
        "pcall returns true, the forwarded 7, and the original call's 5"
    );
    assert_eq!(count_label(&run, "invoke"), 2, "invoke runs twice");
    assert_eq!(count_label(&run, "leaf"), 2, "leaf runs twice");
    assert_eq!(count_label(&run, "forward"), 1, "forward runs once");
    let invoke_marks: Vec<_> = run
        .marks
        .iter()
        .filter(|mark| mark.label == "invoke")
        .collect();
    for mark in &invoke_marks {
        assert_marker_call_site(&run, mark, 2);
    }
    let mut invoke_callers: Vec<_> = invoke_marks
        .iter()
        .map(|mark| mark.parent_prototype)
        .collect();
    invoke_callers.sort_unstable();
    invoke_callers.dedup();
    assert_eq!(
        invoke_callers.len(),
        2,
        "the original call and the closure-forwarded call have distinct callers"
    );
    for mark in run.marks.iter().filter(|mark| mark.label == "leaf") {
        assert_marker_call_site(&run, mark, 1);
    }
    assert_dynamic_call_witness(&run, "leaf", 1);
    assert_eq!(
        payload_of("direct-before.lua", &run),
        Value::Int(7),
        "the last leaf argument is the forwarded 7"
    );
}

/// `direct-after.lua` calls the original `invoke` binding after `forward` has
/// captured it. Lua keeps that binding usable; the capture must not detach it.
#[test]
fn direct_after_calls_original_binding_after_capture() {
    let run = run_halted("direct-after.lua");
    assert_eq!(
        labels(&run),
        ["invoke", "leaf", "forward", "invoke", "leaf", "done"],
        "the captured call must not break the later original call"
    );
    assert_eq!(
        completed_values("direct-after.lua", &run),
        &[Value::Bool(true), Value::Int(7), Value::Int(5)][..],
        "pcall returns true, the forwarded 7, and the later original call's 5"
    );
    assert_eq!(count_label(&run, "invoke"), 2, "invoke runs twice");
    assert_eq!(count_label(&run, "leaf"), 2, "leaf runs twice");
    for mark in run.marks.iter().filter(|mark| mark.label == "invoke") {
        assert_marker_call_site(&run, mark, 2);
    }
    for mark in run.marks.iter().filter(|mark| mark.label == "leaf") {
        assert_marker_call_site(&run, mark, 1);
    }
    assert_dynamic_call_witness(&run, "leaf", 1);
    assert_single_copy_capture(&run, "direct-after.lua");
    assert_eq!(payload_of("direct-after.lua", &run), Value::Int(7));
}

/// `protected-after.lua` wraps the post-capture original binding call in
/// `pcall`. The direct protected call succeeds and contributes its Boolean.
#[test]
fn protected_after_calls_original_binding_after_capture() {
    let run = run_halted("protected-after.lua");
    assert_eq!(
        labels(&run),
        ["invoke", "leaf", "forward", "invoke", "leaf", "done"],
        "the protected original call and the forwarded call both run"
    );
    assert_eq!(
        completed_values("protected-after.lua", &run),
        &[
            Value::Bool(true),
            Value::Int(7),
            Value::Int(5),
            Value::Bool(true)
        ][..],
        "forward pcall succeeds, then the protected original call returns true and 5"
    );
    assert_eq!(count_label(&run, "invoke"), 2, "invoke runs twice");
    assert_eq!(count_label(&run, "leaf"), 2, "leaf runs twice");
    for mark in run.marks.iter().filter(|mark| mark.label == "invoke") {
        assert_marker_call_site(&run, mark, 2);
    }
    for mark in run.marks.iter().filter(|mark| mark.label == "leaf") {
        assert_marker_call_site(&run, mark, 1);
    }
    assert_dynamic_call_witness(&run, "leaf", 1);
    assert_single_copy_capture(&run, "protected-after.lua");
    assert_eq!(payload_of("protected-after.lua", &run), Value::Int(7));
}

#[test]
fn missing_data_arguments_pad_nil_without_rejecting() {
    let run = run_halted("missing-data-argument.lua");
    assert_eq!(labels(&run), ["take", "take", "forward", "take", "done"]);
    assert_eq!(
        completed_values("missing-data-argument.lua", &run),
        &[Value::Null, Value::Bool(true), Value::Null, Value::Int(42)][..]
    );
}

#[test]
fn ordinary_trailing_nil_is_preserved() {
    let run = run_halted("ordinary-trailing-nil.lua");
    assert_eq!(labels(&run), ["pair"]);
    assert_eq!(
        completed_values("ordinary-trailing-nil.lua", &run),
        &[Value::Int(1), Value::Null][..]
    );
}

#[test]
fn completion_exit_separates_vm_status_and_malformed_transport() {
    match consume_completion_exit(Ok(VmStatus::Yielded), Vec::new()) {
        CompletionExit::VmStatus(VmStatus::Yielded) => {}
        other => panic!("a non-Halted status must be reported as itself: {other:?}"),
    }
    let malformed = [
        Vec::new(),
        vec![Value::Int(1)],
        vec![Value::array(Vec::new())],
        vec![Value::array(vec![Value::Int(0)])],
        vec![Value::array(vec![Value::Bool(false)])],
        vec![Value::array(vec![
            Value::Bool(false),
            Value::Null,
            Value::Null,
        ])],
        vec![Value::array(vec![Value::Bool(true)]), Value::Null],
    ];
    for stack in malformed {
        match consume_completion_exit(Ok(VmStatus::Halted), stack) {
            CompletionExit::MalformedTransport { .. } => {}
            other => panic!("malformed transport must fail closed: {other:?}"),
        }
    }
}
