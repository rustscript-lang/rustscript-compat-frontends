//! Source scalar return across a published private step cycle.
//!
//! The Lua source is lowered, compiled, and run. The step host records the
//! real `StepOutcome`. It does not invent a pause, a publication, or a report.
//! Roots already sit in the callee frame, so the step batch is empty.

use std::sync::{Arc, OnceLock};

use vm::{
    CallOutcome, CallReturn, CompileSourceFileOptions, FrontendImportSyntax, FrontendIr,
    HostFunctionRegistry, HostImportSchema, HostOwnedFunction, HostParamPassing, HostParamSchema,
    HostTypeSchema, OpCode, OwnedHostCall, ParseError, Program, SourceFlavor, SourcePlugin, Value,
    VmError, VmStatus,
};

use super::ObservedTable;
use super::candidate::{self, ReturnedElement};
use super::heap::{self, StepOutcome};

const STEP: &str = "lua_host::step";
const PEEK: &str = "lua_host::peek";
const SAME: &str = "lua_host::same";
const NOTE: &str = "lua_host::note";
const TEXT: &str = "lua_host::text";

const SOURCE: &str = r#"
local function witness()
  local w = {}
  local mt = {}
  local t = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = t
  w[2] = {}
  w[3] = {}
  peek(t)
  note(30, rawlen(w))
  step(1)
  step(10000)
  note(10, rawlen(w))
  note(11, same(t, rawget(w, 1)))
  text(2, type(rawget(w, 2)))
  text(3, type(rawget(w, 3)))
  step(1)
  local n = rawlen(w)
  return n
end
witness()
step(1)
step(1)
collectgarbage("collect")
return 1
"#;

#[derive(Clone, Debug, PartialEq)]
enum Stop {
    Halted,
    HostError(String),
    Other(String),
}

#[derive(Clone, Debug)]
struct StepNote {
    budget: u64,
    opened: bool,
    outcome: StepOutcome,
    node_slot_visits: u64,
    live_nodes_before: u32,
    live_nodes_after: u32,
    root_slots: u32,
    live_roots: u32,
    frame: Option<heap::RootFrameId>,
    tail: bool,
    subject_alive: Option<bool>,
    collections: u32,
}

#[derive(Clone, Debug, Default)]
struct Trace {
    steps: Vec<StepNote>,
    peeks: Vec<ObservedTable>,
    sames: Vec<(heap::TableRef, heap::TableRef)>,
    notes: Vec<(f64, f64)>,
    texts: Vec<(f64, String)>,
}

#[derive(Clone, Debug)]
struct HandoffSnap {
    elements: Vec<ReturnedElement>,
    callee_left: bool,
    lua_frame_alive_at_publish: bool,
    handoff_depth: Option<usize>,
    published_root: bool,
}

#[derive(Clone, Debug)]
struct Ran {
    stop: Stop,
    stack: Vec<Value>,
    trace: Trace,
    frame: Option<heap::RootFrameId>,
    tail: bool,
    collections: u32,
    live_nodes: u32,
    root_slots: u32,
    live_roots: u32,
    subject_alive: Option<bool>,
    token_open: Option<bool>,
    handoff: Option<HandoffSnap>,
    collects: usize,
}

struct Plugin;

impl SourcePlugin for Plugin {
    fn flavor(&self) -> SourceFlavor {
        SourceFlavor::Lua
    }

    fn extensions(&self) -> &'static [&'static str] {
        &[]
    }

    fn import_syntax(&self) -> FrontendImportSyntax {
        FrontendImportSyntax::Lua
    }

    fn parse_source(&self, source: &str) -> Result<FrontendIr, ParseError> {
        candidate::lower(source, catalog())
    }
}

fn catalog() -> &'static Arc<vm::HostApiCatalog> {
    static CATALOG: OnceLock<Arc<vm::HostApiCatalog>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut builder = vm::HostApiBuilder::new();
        candidate::install_schemas(&mut builder);
        let table = HostTypeSchema::Resource(heap::LuaTableResource::key());
        builder.function(vm::HostFunctionSchema::with_return(
            STEP,
            vec![HostParamSchema::value("budget", HostTypeSchema::Float)],
            HostTypeSchema::Float,
        ));
        builder.function(vm::HostFunctionSchema::with_return(
            PEEK,
            vec![HostParamSchema::with_passing(
                "table",
                table.clone(),
                HostParamPassing::Borrow,
            )],
            HostTypeSchema::Bool,
        ));
        builder.function(vm::HostFunctionSchema::with_return(
            SAME,
            vec![
                HostParamSchema::with_passing("left", table.clone(), HostParamPassing::Borrow),
                HostParamSchema::with_passing("right", table, HostParamPassing::Borrow),
            ],
            HostTypeSchema::Float,
        ));
        builder.function(vm::HostFunctionSchema::with_return(
            NOTE,
            vec![
                HostParamSchema::value("tag", HostTypeSchema::Float),
                HostParamSchema::value("value", HostTypeSchema::Float),
            ],
            HostTypeSchema::Float,
        ));
        builder.function(vm::HostFunctionSchema::with_return(
            TEXT,
            vec![
                HostParamSchema::value("tag", HostTypeSchema::Float),
                HostParamSchema::value("text", HostTypeSchema::String),
            ],
            HostTypeSchema::Bool,
        ));
        Arc::new(builder.build().expect("source scalar catalog"))
    })
}

fn register(registry: &mut HostFunctionRegistry, catalog: &vm::HostApiCatalog) {
    for (name, op) in [
        (STEP, HostOp::Step),
        (PEEK, HostOp::Peek),
        (SAME, HostOp::Same),
        (NOTE, HostOp::Note),
        (TEXT, HostOp::Text),
    ] {
        let function = catalog.function(name).expect("scalar step host schema");
        registry
            .register_exact_owned(
                name,
                function.params.len() as u8,
                HostImportSchema::from_function(catalog, function),
                move |_| Box::new(op),
            )
            .expect("register scalar step host");
    }
}

#[derive(Clone, Copy)]
enum HostOp {
    Step,
    Peek,
    Same,
    Note,
    Text,
}

impl HostOwnedFunction for HostOp {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        let args = call.args().to_vec();
        let mut host = call.vm().host_context();
        let value = match self {
            Self::Step => step_host(&mut host, &args)?,
            Self::Peek => {
                let observed = borrowed_table(&mut host, &args[0])?;
                let alive = heap::with_heap(&mut host, |heap| Ok(heap.alive(observed.table)))
                    .map_err(host_heap)?;
                note(&mut host, |state| {
                    state.peeks.push(observed);
                })?;
                Value::Bool(alive)
            }
            Self::Same => {
                let left = borrowed_table(&mut host, &args[0])?;
                let right = borrowed_table(&mut host, &args[1])?;
                let same = left.table == right.table;
                note(&mut host, |state| {
                    state.sames.push((left.table, right.table));
                })?;
                Value::Float(if same { 1.0 } else { 0.0 })
            }
            Self::Note => {
                let [Value::Float(tag), Value::Float(value)] = args.as_slice() else {
                    return Err(host_fault("expected numeric note"));
                };
                let noted = (*tag, *value);
                note(&mut host, |state| state.notes.push(noted))?;
                Value::Float(noted.1)
            }
            Self::Text => {
                let [Value::Float(tag), Value::String(text)] = args.as_slice() else {
                    return Err(host_fault("expected text note"));
                };
                let noted = (*tag, text.to_string());
                note(&mut host, |state| state.texts.push(noted))?;
                Value::Bool(true)
            }
        };
        Ok(CallOutcome::Return(CallReturn::one(value)))
    }
}

fn step_host(host: &mut vm::HostContext<'_>, args: &[Value]) -> Result<Value, VmError> {
    let [Value::Float(budget)] = args else {
        return Err(host_fault("expected step budget"));
    };
    if !budget.is_finite() || *budget <= 0.0 || budget.fract() != 0.0 {
        return Err(host_fault("step budget must be a positive integer"));
    }
    let budget = *budget as u64;
    let idle = heap::fixed_step_idle(host).map_err(host_heap)?;
    if idle {
        heap::open_fixed_step(host).map_err(host_heap)?;
    }
    let subject = note(host, |state| state.peeks.first().map(|item| item.table))?;
    let before = heap::stats(host).map_err(host_heap)?;
    let _cleared = heap::take_scan_meter(host).map_err(host_heap)?;
    let outcome = heap::step_fixed_frames(host, budget).map_err(host_heap)?;
    let meter = heap::take_scan_meter(host).map_err(host_heap)?;
    let after = heap::stats(host).map_err(host_heap)?;
    let frame = heap::with_heap(host, |heap| Ok(heap.current_frame())).map_err(host_heap)?;
    let tail = heap::saved_sweep_tail(host).map_err(host_heap)?;
    let subject_alive = match subject {
        Some(table) => {
            Some(heap::with_heap(host, |heap| Ok(heap.alive(table))).map_err(host_heap)?)
        }
        None => None,
    };
    note(host, |state| {
        state.steps.push(StepNote {
            budget,
            opened: idle,
            outcome,
            node_slot_visits: meter.node_slot_visits,
            live_nodes_before: before.live_nodes,
            live_nodes_after: after.live_nodes,
            root_slots: after.root_slots,
            live_roots: after.live_roots,
            frame,
            tail,
            subject_alive,
            collections: after.collections,
        });
    })?;
    let code = match outcome {
        StepOutcome::Paused { tail: false } => 0.0,
        StepOutcome::Published { .. } => 1.0,
        StepOutcome::Paused { tail: true } => 2.0,
        StepOutcome::Completed(_) => 3.0,
    };
    Ok(Value::Float(code))
}

fn note<T>(
    host: &mut vm::HostContext<'_>,
    body: impl FnOnce(&mut Trace) -> T,
) -> Result<T, VmError> {
    let state = host
        .scope_state_or_insert_with(Trace::default)
        .map_err(|error| VmError::HostError(error.to_string()))?;
    Ok(body(state))
}

fn borrowed_table(host: &mut vm::HostContext<'_>, value: &Value) -> Result<ObservedTable, VmError> {
    let Value::Int(raw) = value else {
        return Err(host_fault("expected table token"));
    };
    let token = vm::ResourceHandle::from_raw(
        u64::try_from(*raw).map_err(|_| host_fault("table token out of range"))?,
    )
    .map_err(|_| host_fault("table token out of range"))?;
    let key = heap::LuaTableResource::key();
    let table = host
        .borrow_resource_with_key::<heap::LuaTableResource>(token, &key)
        .map_err(|error| VmError::HostError(error.to_string()))?
        .table();
    Ok(ObservedTable { table, token })
}

fn host_fault(message: &str) -> VmError {
    VmError::HostError(message.to_string())
}

fn host_heap(error: heap::HeapError) -> VmError {
    VmError::HostError(error.to_string())
}

fn run(source: &str) -> Ran {
    let catalog = Arc::clone(catalog());
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&Plugin)
        .with_host_api_catalog(Arc::clone(&catalog));
    let prepared = vm::compile_source_with_flavor_and_options(source, SourceFlavor::Lua, options)
        .unwrap_or_else(|error| panic!("compile rejected the source: {error}"));
    let mut vm = prepared.into_vm().expect("create VM");
    assert_return_window(vm.program());
    let mut registry = HostFunctionRegistry::empty();
    candidate::register(&mut registry, &catalog).expect("register candidate runtime");
    register(&mut registry, &catalog);
    registry.bind_vm_cached(&mut vm).expect("bind runtime");
    let status = vm.run();
    let trace = vm
        .host_context()
        .scope_state::<Trace>()
        .cloned()
        .unwrap_or_default();
    let frame = vm
        .host_context()
        .scope_state::<heap::LuaHeap>()
        .and_then(|heap| heap.current_frame());
    let tail = heap::saved_sweep_tail(&mut vm.host_context()).unwrap_or(false);
    let stats = heap::stats(&mut vm.host_context())
        .unwrap_or_else(|error| panic!("heap stats unavailable after {status:?}: {error}"));
    let subject = trace.peeks.first().copied();
    let (subject_alive, token_open) = match subject {
        Some(observed) => {
            let alive =
                heap::with_heap(
                    &mut vm.host_context(),
                    |heap| Ok(heap.alive(observed.table)),
                )
                .unwrap_or(false);
            let open = vm
                .host_context()
                .typed_resource_with_key::<heap::LuaTableResource>(
                    observed.token,
                    &heap::LuaTableResource::key(),
                )
                .is_ok();
            (Some(alive), Some(open))
        }
        None => (None, None),
    };
    let handoff = vm
        .host_context()
        .scope_state::<candidate::ReturnWitness>()
        .map(|witness| {
            let last = witness.handoffs.last();
            HandoffSnap {
                elements: last
                    .map(|item| item.elements.clone())
                    .unwrap_or_else(|| witness.elements.clone()),
                callee_left: witness.callee_left,
                lua_frame_alive_at_publish: witness.lua_frame_alive_at_publish,
                handoff_depth: witness.handoff_depth,
                published_root: witness.published_root,
            }
        });
    let collects = vm
        .host_context()
        .scope_state::<candidate::ReturnWitness>()
        .map(|witness| witness.collects.len())
        .unwrap_or(0);
    let stop = match status {
        Ok(VmStatus::Halted) => Stop::Halted,
        Err(VmError::HostError(message)) => Stop::HostError(message),
        Ok(other) => Stop::Other(format!("{other:?}")),
        Err(other) => Stop::Other(other.to_string()),
    };
    Ran {
        stop,
        stack: vm.stack().to_vec(),
        trace,
        frame,
        tail,
        collections: stats.collections,
        live_nodes: stats.live_nodes,
        root_slots: stats.root_slots,
        live_roots: stats.live_roots,
        subject_alive,
        token_open,
        handoff,
        collects,
    }
}

fn assert_return_window(program: &Program) {
    let finish = import_index(program, candidate::FINISH_RETURN);
    let retire = import_index(program, candidate::RETIRE_CALLEE);
    let mut call_ip = None;
    let mut prototype = None;
    let mut ip = 0;
    while ip < program.code.len() {
        let op = OpCode::try_from(program.code[ip]).expect("opcode");
        if op == OpCode::CallScript && call_ip.is_none() {
            call_ip = Some(ip);
            prototype = Some(u32::from_le_bytes(
                program.code[ip + 1..ip + 5].try_into().expect("prototype"),
            ));
            assert_eq!(program.code[ip + 5], 0, "the witness takes no arguments");
        }
        ip += 1 + op.operand_len();
    }
    let call_ip = call_ip.expect("CallScript");
    let prototype = program
        .callable_prototypes
        .get(prototype.expect("prototype") as usize)
        .expect("witness prototype");
    let vm::CallableTarget::ScriptFunction(index) = prototype.target else {
        panic!("CallScript target is not a script function");
    };
    let function = program
        .script_functions
        .get(index as usize)
        .expect("script function");
    assert!(
        call_ip < function.entry_ip as usize || call_ip >= function.end_ip as usize,
        "the call site is outside the callee"
    );
    assert!(
        region_has(program, function.entry_ip, function.end_ip, OpCode::Ret),
        "the callee must return through Core Ret"
    );
    let finish_ip = next_call(program, call_ip, finish);
    assert_no_calls_between(program, call_ip, finish_ip);
    let retire_ip = next_call(program, finish_ip, retire);
    if let Some(collect) = import_index_opt(program, candidate::COLLECT) {
        assert!(
            !region_calls(program, finish_ip as u32, retire_ip as u32, collect),
            "collect does not sit between publish and retire"
        );
    }
    if let Some(gather) = import_index_opt(program, "lua_internal::gather_root") {
        assert!(
            !region_calls(program, call_ip as u32, retire_ip as u32, gather),
            "the step host must not gather a fresh root batch before retire"
        );
    }
    if let Some(pending) = import_index_opt(program, "lua_internal::enter_pending_frame") {
        assert!(
            !region_calls(program, 0, program.code.len() as u32, pending),
            "this witness must not open a pending frame"
        );
    }
}

fn import_index(program: &Program, name: &str) -> u16 {
    import_index_opt(program, name).unwrap_or_else(|| panic!("missing import {name}"))
}

fn import_index_opt(program: &Program, name: &str) -> Option<u16> {
    program
        .imports
        .iter()
        .position(|import| import.name == name)
        .map(|index| index.try_into().expect("import index"))
}

fn region_has(program: &Program, start: u32, end: u32, want: OpCode) -> bool {
    let mut ip = start as usize;
    let end = end as usize;
    while ip < end {
        let op = OpCode::try_from(program.code[ip]).expect("opcode");
        if op == want {
            return true;
        }
        ip += 1 + op.operand_len();
    }
    false
}

fn region_calls(program: &Program, start: u32, end: u32, import: u16) -> bool {
    let mut ip = start as usize;
    let end = end as usize;
    while ip < end {
        let op = OpCode::try_from(program.code[ip]).expect("opcode");
        if op == OpCode::Call && call_index(program, ip) == import {
            return true;
        }
        ip += 1 + op.operand_len();
    }
    false
}

fn next_call(program: &Program, from: usize, import: u16) -> usize {
    let op = OpCode::try_from(program.code[from]).expect("opcode");
    let mut ip = from + 1 + op.operand_len();
    while ip < program.code.len() {
        let op = OpCode::try_from(program.code[ip]).expect("opcode");
        if op == OpCode::Call && call_index(program, ip) == import {
            return ip;
        }
        ip += 1 + op.operand_len();
    }
    panic!("import {import} does not follow ip {from}");
}

fn assert_no_calls_between(program: &Program, from: usize, until: usize) {
    let op = OpCode::try_from(program.code[from]).expect("opcode");
    let mut ip = from + 1 + op.operand_len();
    while ip < until {
        let op = OpCode::try_from(program.code[ip]).expect("opcode");
        assert!(
            !matches!(op, OpCode::Call | OpCode::CallValue | OpCode::CallScript),
            "a call sits between Ret and finish_return at {ip}"
        );
        ip += 1 + op.operand_len();
    }
}

fn call_index(program: &Program, ip: usize) -> u16 {
    u16::from_le_bytes(program.code[ip + 1..ip + 3].try_into().expect("call index"))
}

fn note_value(trace: &Trace, tag: f64) -> f64 {
    trace
        .notes
        .iter()
        .find(|(found, _)| *found == tag)
        .unwrap_or_else(|| panic!("missing note {tag} in {:?}", trace.notes))
        .1
}

fn text_value(trace: &Trace, tag: f64) -> &str {
    trace
        .texts
        .iter()
        .find(|(found, _)| *found == tag)
        .unwrap_or_else(|| panic!("missing text {tag} in {:?}", trace.texts))
        .1
        .as_str()
}

#[test]
fn source_scalar_return_keeps_the_published_round() {
    let ran = run(SOURCE);
    let steps = &ran.trace.steps;
    assert!(
        steps.len() >= 3,
        "the witness must step before it returns: {ran:#?}"
    );
    assert!(
        matches!(steps[0].outcome, StepOutcome::Paused { tail: false }),
        "budget 1 must pause the mark before publication: {ran:#?}"
    );
    assert_eq!(steps[0].budget, 1);
    assert!(steps[0].opened, "the first step opens the idle cycle");
    assert!(
        matches!(steps[1].outcome, StepOutcome::Published { .. }),
        "the following positive budget must publish and not resume that tail: {ran:#?}"
    );
    assert_eq!(note_value(&ran.trace, 30.0), 3.0, "rawlen before publish");
    assert_eq!(note_value(&ran.trace, 10.0), 1.0, "rawlen after publish");
    assert_eq!(
        note_value(&ran.trace, 11.0),
        1.0,
        "published weak read keeps t"
    );
    assert_eq!(ran.trace.sames.len(), 1);
    assert_eq!(ran.trace.sames[0].0, ran.trace.sames[0].1);
    assert_eq!(ran.trace.peeks.len(), 1);
    assert_eq!(ran.trace.sames[0].0, ran.trace.peeks[0].table);
    assert_eq!(text_value(&ran.trace, 2.0), "nil");
    assert_eq!(text_value(&ran.trace, 3.0), "nil");
    assert!(
        matches!(steps[2].outcome, StepOutcome::Paused { tail: true }),
        "budget 1 after publish releases one dead slot and stays in the tail: {ran:#?}"
    );
    assert_eq!(steps[2].budget, 1);
    assert_eq!(steps[2].node_slot_visits, 1, "one real dead slot");
    assert_eq!(steps[2].live_nodes_before, steps[2].live_nodes_after + 1);
    assert_eq!(steps[2].subject_alive, Some(true));
    assert!(steps[2].tail, "the release step keeps the published tail");
    assert_eq!(steps[2].collections, 0);
    let callee_before_return = steps[2]
        .frame
        .expect("callee frame before the scalar return");
    assert_eq!(
        steps[2].live_roots, 3,
        "the callee frame still holds its three roots"
    );
    assert!(
        steps[..3]
            .iter()
            .all(|step| !matches!(step.outcome, StepOutcome::Completed(_))),
        "the cycle must still be open at the return: {ran:#?}"
    );
    let handoff = ran.handoff.as_ref().expect("finish_return ran");
    assert!(handoff.lua_frame_alive_at_publish);
    assert!(
        !handoff.published_root,
        "a scalar return does not write a caller root"
    );
    assert!(
        matches!(handoff.elements.as_slice(), [ReturnedElement::Number(number)] if *number == 1.0)
    );
    assert_eq!(
        ran.stop,
        Stop::Halted,
        "retire after Published must leave the callee and continue the same round: {ran:#?}"
    );
    assert!(handoff.callee_left, "the callee frame left during retire");
    assert_eq!(handoff.handoff_depth, Some(1));
    assert_ne!(
        ran.frame,
        Some(callee_before_return),
        "the caller slot is not the retired callee {callee_before_return:?}"
    );
    assert!(ran.frame.is_none(), "no caller frame remains: {ran:#?}");
    assert_eq!(ran.root_slots, 0, "root count follows retained frames");
    assert!(
        steps.len() >= 5,
        "return is followed by two budget-1 steps: {ran:#?}"
    );
    assert!(
        matches!(steps[3].outcome, StepOutcome::Paused { tail: true }),
        "budget 1 after return continues the same tail: {ran:#?}"
    );
    assert_eq!(steps[3].budget, 1);
    assert_eq!(steps[3].node_slot_visits, 1, "second real dead slot");
    assert_eq!(steps[3].live_nodes_before, steps[3].live_nodes_after + 1);
    assert_eq!(steps[3].subject_alive, Some(true));
    assert!(steps[3].frame.is_none());
    assert!(steps[3].tail);
    assert_eq!(steps[3].live_roots, 0);
    assert!(
        matches!(steps[4].outcome, StepOutcome::Completed(_)),
        "the next budget finishes this round once: {ran:#?}"
    );
    assert_eq!(
        steps
            .iter()
            .filter(|step| matches!(step.outcome, StepOutcome::Completed(_)))
            .count(),
        1
    );
    assert_eq!(
        steps[4].subject_alive,
        Some(true),
        "the marked subject survives this round"
    );
    assert_eq!(steps[4].root_slots, 0);
    assert_eq!(
        steps[4].collections, 1,
        "the completion report is this round's only collection"
    );
    assert_eq!(ran.stack, vec![Value::Float(1.0)]);
    assert_eq!(ran.collects, 1, "the later full collect is a new round");
    assert_eq!(
        ran.collections, 2,
        "the step report and the later collect are separate rounds"
    );
    assert_eq!(
        ran.live_nodes, 0,
        "the next full collect reclaims the unrooted nodes"
    );
    assert_eq!(ran.live_roots, 0);
    assert_eq!(
        ran.subject_alive,
        Some(false),
        "the next full collect reclaims t"
    );
    assert_eq!(ran.token_open, Some(false), "that collect closes the token");
    assert!(
        !ran.tail,
        "the independent collect does not leave the old tail"
    );
}

const LATE_WRITE: &str = r#"
local function witness()
  local w = {}
  local mt = {}
  local t = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = t
  w[2] = {}
  w[3] = {}
  step(1)
  step(10000)
  text(1, "before-write")
  w[3] = "late"
  return 1
end
witness()
return 1
"#;

const EARLY_WRITE: &str = r#"
local function witness()
  local w = {}
  local mt = {}
  local t = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = t
  w[2] = {}
  w[3] = {}
  w[3] = "late"
  text(1, type(w[3]))
  return 1
end
return witness()
"#;

const COLLECT_DURING_TAIL: &str = r#"
local function witness()
  local w = {}
  local mt = {}
  local t = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = t
  w[2] = {}
  w[3] = {}
  step(1)
  step(10000)
  collectgarbage("collect")
  return 1
end
witness()
return 1
"#;

const CHUNK_TAIL: &str = r#"
local function witness()
  local w = {}
  local mt = {}
  local t = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = t
  w[2] = {}
  w[3] = {}
  step(1)
  step(10000)
  return 1
end
local n = witness()
"#;

const PLAIN_RETURN: &str = r#"
local function witness()
  local t = {}
  return 1
end
witness()
return 1
"#;

fn assert_sweep_pending(ran: &Ran, why: &str) {
    assert!(
        matches!(ran.stop, Stop::HostError(ref message) if message == "lua sweep is still pending"),
        "{why}: {ran:#?}"
    );
}

#[test]
fn published_tail_rejects_a_late_string_write() {
    let ran = run(LATE_WRITE);
    assert!(
        matches!(
            ran.trace.steps.last().map(|step| step.outcome),
            Some(StepOutcome::Published { .. })
        ),
        "the write runs only after publication: {ran:#?}"
    );
    assert_eq!(text_value(&ran.trace, 1.0), "before-write");
    assert!(
        ran.trace
            .steps
            .iter()
            .all(|step| !matches!(step.outcome, StepOutcome::Completed(_))),
        "the late write must not finish the round: {ran:#?}"
    );
    assert!(
        ran.tail,
        "the tail is still saved when the write is refused"
    );
    assert!(ran.frame.is_some(), "the callee frame is still current");
    let handoff = ran.handoff.as_ref();
    assert!(
        handoff.is_none_or(|item| !item.callee_left && item.elements.is_empty()),
        "the write fails before return: {ran:#?}"
    );
    assert_eq!(ran.collects, 0);
    assert_sweep_pending(&ran, "post-publish string write");
}

#[test]
fn same_shape_string_write_succeeds_before_publish() {
    let ran = run(EARLY_WRITE);
    assert_eq!(ran.stop, Stop::Halted, "{ran:#?}");
    assert_eq!(ran.stack, vec![Value::Float(1.0)]);
    assert_eq!(text_value(&ran.trace, 1.0), "string");
    assert!(ran.trace.steps.is_empty(), "the early write does not step");
    assert!(!ran.tail);
    let handoff = ran.handoff.expect("ordinary return");
    assert!(handoff.callee_left, "no tail still uses leave_roots");
    assert!(matches!(
        handoff.elements.as_slice(),
        [ReturnedElement::Number(number)] if *number == 1.0
    ));
}

#[test]
fn published_tail_keeps_the_full_collect_gate() {
    let ran = run(COLLECT_DURING_TAIL);
    assert!(matches!(
        ran.trace.steps.last().map(|step| step.outcome),
        Some(StepOutcome::Published { .. })
    ));
    assert!(
        ran.trace
            .steps
            .iter()
            .all(|step| !matches!(step.outcome, StepOutcome::Completed(_)))
    );
    assert!(ran.tail);
    assert!(ran.frame.is_some());
    assert!(
        ran.handoff
            .as_ref()
            .is_none_or(|item| !item.callee_left && item.elements.is_empty()),
        "collect fails before return: {ran:#?}"
    );
    assert_eq!(ran.collects, 0, "the refused collect records no report");
    assert_sweep_pending(&ran, "full collect during a published tail");
}

#[test]
fn published_tail_keeps_the_chunk_retire_gate() {
    let ran = run(CHUNK_TAIL);
    assert!(matches!(
        ran.trace.steps.last().map(|step| step.outcome),
        Some(StepOutcome::Published { .. })
    ));
    assert!(
        ran.trace
            .steps
            .iter()
            .all(|step| !matches!(step.outcome, StepOutcome::Completed(_)))
    );
    let handoff = ran.handoff.as_ref().expect("callee return ran");
    assert!(handoff.callee_left, "the callee used the private retire");
    assert!(
        ran.frame.is_some(),
        "chunk leave_roots did not drop the caller frame"
    );
    assert!(ran.tail);
    assert_eq!(ran.collects, 0);
    assert_sweep_pending(&ran, "chunk retire during a published tail");
}

#[test]
fn ordinary_scalar_return_still_leaves_roots() {
    let ran = run(PLAIN_RETURN);
    assert_eq!(ran.stop, Stop::Halted, "{ran:#?}");
    assert_eq!(ran.stack, vec![Value::Float(1.0)]);
    assert!(!ran.tail);
    assert!(ran.frame.is_none());
    let handoff = ran.handoff.expect("ordinary return");
    assert!(handoff.callee_left);
    assert!(
        matches!(handoff.elements.as_slice(), [ReturnedElement::Number(number)] if *number == 1.0)
    );
    assert!(!handoff.published_root);
}

/// Direct `return rawlen(witness())` consumes the callee table with publish 0.
/// A saved tail must refuse that table before rawlen runs. The same shape with
/// no tail still returns the length.
const TABLE_RETURN: &str = r#"
local function witness()
  local w = {}
  local mt = {}
  local t = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  t[1] = t
  w[1] = t
  w[2] = {}
  w[3] = {}
  peek(t)
  note(30, rawlen(w))
  step(1)
  step(10000)
  note(10, rawlen(w))
  note(11, same(t, rawget(w, 1)))
  text(2, type(rawget(w, 2)))
  text(3, type(rawget(w, 3)))
  step(1)
  return w
end
return rawlen(witness())
"#;

const TABLE_RETURN_OPEN: &str = r#"
local function witness()
  local w = {}
  local mt = {}
  local t = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  t[1] = t
  w[1] = t
  w[2] = {}
  w[3] = {}
  return w
end
return rawlen(witness())
"#;

#[test]
fn published_tail_refuses_direct_table_consumption() {
    let open = run(TABLE_RETURN_OPEN);
    assert_eq!(
        open.stop,
        Stop::Halted,
        "no-tail table return must still reach rawlen: {open:#?}"
    );
    assert_eq!(
        open.stack,
        vec![Value::Float(3.0)],
        "no-tail rawlen sees the self-cycle and both orphans: {open:#?}"
    );
    assert!(!open.tail, "the open control must not publish a tail");
    let open_handoff = open.handoff.expect("no-tail finish_return");
    assert!(
        !open_handoff.published_root,
        "direct consumption does not write a caller root"
    );
    assert!(matches!(
        open_handoff.elements.as_slice(),
        [ReturnedElement::Table { .. }]
    ));
    assert!(
        open_handoff.callee_left,
        "no tail still retires through leave_roots"
    );

    let ran = run(TABLE_RETURN);
    let steps = &ran.trace.steps;
    assert!(
        steps.len() >= 3,
        "the witness must publish and release before it returns w: {ran:#?}"
    );
    assert!(
        matches!(steps[0].outcome, StepOutcome::Paused { tail: false }),
        "budget 1 must pause before publication: {ran:#?}"
    );
    assert!(
        matches!(steps[1].outcome, StepOutcome::Published { .. }),
        "the return must happen after publication: {ran:#?}"
    );
    assert_eq!(note_value(&ran.trace, 30.0), 3.0, "rawlen before publish");
    assert_eq!(note_value(&ran.trace, 10.0), 1.0, "rawlen after publish");
    assert_eq!(
        note_value(&ran.trace, 11.0),
        1.0,
        "published weak read keeps t"
    );
    assert_eq!(text_value(&ran.trace, 2.0), "nil");
    assert_eq!(text_value(&ran.trace, 3.0), "nil");
    assert!(
        matches!(steps[2].outcome, StepOutcome::Paused { tail: true }),
        "budget 1 after publish must pause in Release: {ran:#?}"
    );
    assert_eq!(steps[2].budget, 1);
    assert!(steps[2].tail, "the release pause keeps the published tail");
    assert_eq!(steps[2].collections, 0);
    let callee = steps[2]
        .frame
        .expect("callee frame during the release pause");
    assert!(
        steps
            .iter()
            .all(|step| !matches!(step.outcome, StepOutcome::Completed(_))),
        "the cycle is still open at the table return: {ran:#?}"
    );
    assert!(
        matches!(
            ran.stop,
            Stop::HostError(ref message) if message == "lua sweep is still pending"
        ),
        "direct rawlen consumed a published tail table: stop={:?} stack={:?} published_root={:?} frame={:?} tail={} callee={:?} handoff={:?}",
        ran.stop,
        ran.stack,
        ran.handoff.as_ref().map(|item| item.published_root),
        ran.frame,
        ran.tail,
        callee,
        ran.handoff,
    );
    assert_eq!(ran.frame, Some(callee), "the original callee stays current");
    assert!(ran.tail, "the published tail stays saved");
    assert_eq!(ran.collects, 0, "refusal records no completion report");
    assert!(
        ran.collections == 0,
        "the step host recorded no completion report: {ran:#?}"
    );
    assert!(
        !matches!(ran.stack.as_slice(), [Value::Float(_)]),
        "rawlen must not publish a length: {ran:#?}"
    );
    assert!(
        ran.handoff
            .as_ref()
            .is_none_or(|item| !item.callee_left && item.elements.is_empty()),
        "the table is refused before handoff and retire: {ran:#?}"
    );
}
