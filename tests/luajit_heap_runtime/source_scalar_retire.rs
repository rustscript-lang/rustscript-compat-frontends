//! Source scalar return across a published private step cycle.
//!
//! The Lua source is lowered, compiled, and run. The step host records the
//! real `StepOutcome`. It does not invent a pause, a publication, or a report.
//! The private host uses fixed frames; the opt-in guest spelling gathers the
//! current source roots on mark steps and bypasses gathering on tail steps.

use std::collections::HashSet;
use std::num::NonZeroU64;
use std::sync::{Arc, OnceLock};

use vm::{
    CallOutcome, CallReturn, CompileSourceFileOptions, Expr, FrontendImportSyntax, FrontendIr,
    HostFunctionRegistry, HostImportSchema, HostOwnedFunction, HostParamPassing, HostParamSchema,
    HostTypeSchema, OpCode, OwnedHostCall, ParseError, Program, SourceFlavor, SourcePlugin, Stmt,
    Value, VmError, VmStatus,
};

use super::ObservedTable;
use super::candidate::{self, ReturnedElement};
use super::heap::{self, StepOutcome};

const STEP: &str = "lua_host::step";
const PEEK: &str = "lua_host::peek";
const SAME: &str = "lua_host::same";
const NOTE: &str = "lua_host::note";
const TEXT: &str = "lua_host::text";
const BIT: &str = "lua_host::bit";
const SHAPE: &str = "lua_host::shape";
const METER: &str = "lua_host::meter";
const MARK_PHASE: &str = "lua_host::mark_phase";
const OCCUPY: &str = "lua_host::occupy";

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
    root_slot_visits: u64,
    live_nodes_before: u32,
    live_nodes_after: u32,
    root_slots: u32,
    live_roots: u32,
    frame: Option<heap::RootFrameId>,
    tail: bool,
    subject_alive: Option<bool>,
    collections: u32,
}

#[derive(Clone, Debug)]
struct MarkNote {
    tag: f64,
    phase: Option<&'static str>,
    pending: bool,
    parked_root_writes: u64,
    work_pushed: u64,
    mark_passes: u64,
    live_nodes: u32,
    collects: usize,
}

/// Read-only occupancy taken from the already installed heap. A missing heap
/// is an error from the host; this note does not call `with_heap`.
#[derive(Clone, Copy, Debug)]
struct OccupancyNote {
    tag: f64,
    root_slots: u32,
    live_roots: u32,
    live_nodes: u32,
    node_slots: u32,
    edge_slots: u32,
    phase: Option<&'static str>,
    mark_passes: u64,
    node_slot_visits: u64,
}

#[derive(Clone, Debug, Default)]
struct Trace {
    steps: Vec<StepNote>,
    peeks: Vec<ObservedTable>,
    sames: Vec<(heap::TableRef, heap::TableRef)>,
    notes: Vec<(f64, f64)>,
    texts: Vec<(f64, String)>,
    bits: Vec<(f64, bool)>,
    bit_cycle_state: Vec<(bool, u32)>,
    bit_accounting: Vec<Option<(u32, u32)>>,
    bit_tail: Vec<Option<heap::SavedTailState>>,
    bit_roots: Vec<Vec<Option<heap::TableRef>>>,
    bit_resources: Vec<usize>,
    bit_collects: Vec<usize>,
    bit_root_visits: Vec<u64>,
    bit_root_slots: Vec<u32>,
    marks: Vec<MarkNote>,
    occupancy: Vec<OccupancyNote>,
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
    returned: Option<heap::TableRef>,
    release_root: Option<candidate::ReleaseRootNote>,
    budgets: Vec<u64>,
    running: bool,
    mark_phase: Option<&'static str>,
    mark_pending: bool,
    parked_root_writes: u64,
    tail_state: Option<heap::SavedTailState>,
    peek_end: Vec<(bool, bool)>,
    resources: usize,
    reports: Vec<candidate::CollectNote>,
    issued_alive: Vec<bool>,
    root_visits: Vec<u64>,
    tail_accounting: Option<(u32, u32)>,
    top_roots: Vec<Option<heap::TableRef>>,
    permit: Option<(u32, bool)>,
    txn: Option<heap::ReleaseTxnWitness>,
}

struct Plugin {
    guest_step: bool,
    run_control: bool,
    auto_table: bool,
    restart_entry_only: bool,
    due_only: bool,
}

static PLUGIN: Plugin = Plugin {
    guest_step: false,
    run_control: false,
    auto_table: false,
    restart_entry_only: false,
    due_only: false,
};
static STEP_PLUGIN: Plugin = Plugin {
    guest_step: true,
    run_control: false,
    auto_table: false,
    restart_entry_only: false,
    due_only: false,
};
static CONTROL_PLUGIN: Plugin = Plugin {
    guest_step: true,
    run_control: true,
    auto_table: false,
    restart_entry_only: false,
    due_only: false,
};
static AUTO_TABLE_PLUGIN: Plugin = Plugin {
    guest_step: true,
    run_control: true,
    auto_table: true,
    restart_entry_only: false,
    due_only: false,
};
static RESTART_ENTRY_PLUGIN: Plugin = Plugin {
    guest_step: true,
    run_control: true,
    auto_table: false,
    restart_entry_only: true,
    due_only: false,
};
static DUE_ONLY_PLUGIN: Plugin = Plugin {
    guest_step: true,
    run_control: true,
    auto_table: false,
    restart_entry_only: false,
    due_only: true,
};

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
        candidate::lower(source, plugin_catalog(self))
    }
}

fn catalog(guest_step: bool) -> &'static Arc<vm::HostApiCatalog> {
    catalog_for(guest_step, false)
}

fn install_witness_schemas(builder: &mut vm::HostApiBuilder) {
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
    builder.function(vm::HostFunctionSchema::with_return(
        SHAPE,
        vec![HostParamSchema::value("value", HostTypeSchema::Unknown)],
        HostTypeSchema::Float,
    ));
    builder.function(vm::HostFunctionSchema::with_return(
        METER,
        vec![HostParamSchema::value("tag", HostTypeSchema::Float)],
        HostTypeSchema::Float,
    ));
    builder.function(vm::HostFunctionSchema::with_return(
        BIT,
        vec![
            HostParamSchema::value("tag", HostTypeSchema::Float),
            HostParamSchema::value("bit", HostTypeSchema::Bool),
        ],
        HostTypeSchema::Bool,
    ));
    builder.function(vm::HostFunctionSchema::with_return(
        MARK_PHASE,
        vec![HostParamSchema::value("tag", HostTypeSchema::Float)],
        HostTypeSchema::Float,
    ));
    builder.function(vm::HostFunctionSchema::with_return(
        OCCUPY,
        vec![HostParamSchema::value("tag", HostTypeSchema::Float)],
        HostTypeSchema::Float,
    ));
}

fn catalog_for(guest_step: bool, run_control: bool) -> &'static Arc<vm::HostApiCatalog> {
    static CATALOG: OnceLock<Arc<vm::HostApiCatalog>> = OnceLock::new();
    static STEP_CATALOG: OnceLock<Arc<vm::HostApiCatalog>> = OnceLock::new();
    static CONTROL_CATALOG: OnceLock<Arc<vm::HostApiCatalog>> = OnceLock::new();
    let catalog = if run_control {
        &CONTROL_CATALOG
    } else if guest_step {
        &STEP_CATALOG
    } else {
        &CATALOG
    };
    catalog.get_or_init(|| {
        let mut builder = vm::HostApiBuilder::new();
        candidate::install_schemas(&mut builder);
        if guest_step || run_control {
            candidate::install_step_schemas(&mut builder);
        }
        if run_control {
            candidate::install_run_control_schemas(&mut builder);
        }
        install_witness_schemas(&mut builder);
        Arc::new(builder.build().expect("source scalar catalog"))
    })
}

fn catalog_auto() -> &'static Arc<vm::HostApiCatalog> {
    static AUTO_CATALOG: OnceLock<Arc<vm::HostApiCatalog>> = OnceLock::new();
    AUTO_CATALOG.get_or_init(|| {
        let mut builder = vm::HostApiBuilder::new();
        candidate::install_schemas(&mut builder);
        candidate::install_step_schemas(&mut builder);
        candidate::install_run_control_schemas(&mut builder);
        candidate::install_auto_table_restart_schema(&mut builder);
        install_witness_schemas(&mut builder);
        Arc::new(builder.build().expect("auto table restart catalog"))
    })
}

fn catalog_restart_entry() -> &'static Arc<vm::HostApiCatalog> {
    static ENTRY_CATALOG: OnceLock<Arc<vm::HostApiCatalog>> = OnceLock::new();
    ENTRY_CATALOG.get_or_init(|| {
        let mut builder = vm::HostApiBuilder::new();
        candidate::install_schemas(&mut builder);
        candidate::install_step_schemas(&mut builder);
        candidate::install_run_control_schemas(&mut builder);
        candidate::install_auto_table_restart_entry_schema(&mut builder);
        install_witness_schemas(&mut builder);
        Arc::new(builder.build().expect("restart entry catalog"))
    })
}

fn catalog_due_only() -> &'static Arc<vm::HostApiCatalog> {
    static DUE_CATALOG: OnceLock<Arc<vm::HostApiCatalog>> = OnceLock::new();
    DUE_CATALOG.get_or_init(|| {
        let mut builder = vm::HostApiBuilder::new();
        candidate::install_schemas(&mut builder);
        candidate::install_step_schemas(&mut builder);
        candidate::install_run_control_schemas(&mut builder);
        candidate::install_auto_table_due_schema(&mut builder);
        install_witness_schemas(&mut builder);
        Arc::new(builder.build().expect("due observation catalog"))
    })
}

fn plugin_catalog(plugin: &Plugin) -> &'static Arc<vm::HostApiCatalog> {
    if plugin.auto_table {
        catalog_auto()
    } else if plugin.restart_entry_only {
        catalog_restart_entry()
    } else if plugin.due_only {
        catalog_due_only()
    } else {
        catalog_for(plugin.guest_step, plugin.run_control)
    }
}

fn register(registry: &mut HostFunctionRegistry, catalog: &vm::HostApiCatalog) {
    for (name, op) in [
        (STEP, HostOp::Step),
        (PEEK, HostOp::Peek),
        (SAME, HostOp::Same),
        (NOTE, HostOp::Note),
        (TEXT, HostOp::Text),
        (BIT, HostOp::Bit),
        (SHAPE, HostOp::Shape),
        (METER, HostOp::Meter),
        (MARK_PHASE, HostOp::MarkPhase),
        (OCCUPY, HostOp::Occupy),
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
    Bit,
    Shape,
    Meter,
    MarkPhase,
    Occupy,
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
            Self::Bit => {
                let [Value::Float(tag), Value::Bool(bit)] = args.as_slice() else {
                    return Err(host_fault("expected a guest boolean"));
                };
                let tail = heap::saved_sweep_tail(&mut host).map_err(host_heap)?;
                let stats = heap::stats(&mut host).map_err(host_heap)?;
                let accounting = heap::saved_tail_accounting(&mut host).map_err(host_heap)?;
                let tail_state = heap::saved_tail_state(&mut host).map_err(host_heap)?;
                let roots = heap::top_frame_tables(&mut host).map_err(host_heap)?;
                let resources = host.resource_count();
                let collects = host
                    .scope_state::<candidate::ReturnWitness>()
                    .map(|witness| witness.collects.len())
                    .unwrap_or(0);
                let root_visits = heap::copy_scan_meter(&mut host)
                    .map_err(host_heap)?
                    .root_slot_visits;
                note(&mut host, |state| {
                    state.bits.push((*tag, *bit));
                    state.bit_cycle_state.push((tail, stats.collections));
                    state.bit_accounting.push(accounting);
                    state.bit_tail.push(tail_state);
                    state.bit_roots.push(roots);
                    state.bit_resources.push(resources);
                    state.bit_collects.push(collects);
                    state.bit_root_visits.push(root_visits);
                    state.bit_root_slots.push(stats.root_slots);
                })?;
                Value::Bool(*bit)
            }
            Self::Text => {
                let [Value::Float(tag), Value::String(text)] = args.as_slice() else {
                    return Err(host_fault("expected text note"));
                };
                let noted = (*tag, text.to_string());
                note(&mut host, |state| state.texts.push(noted))?;
                Value::Bool(true)
            }
            Self::Shape => Value::Float(if saved_external_shape(args.first()) {
                1.0
            } else {
                0.0
            }),
            Self::Meter => {
                let [Value::Float(tag)] = args.as_slice() else {
                    return Err(host_fault("expected meter tag"));
                };
                let snap = candidate::auto_table_meter(&mut host)?;
                let tag = *tag;
                note(&mut host, |state| {
                    state.notes.push((tag, snap.logical as f64));
                    state.notes.push((tag + 1.0, snap.threshold as f64));
                    state
                        .notes
                        .push((tag + 2.0, if snap.due { 1.0 } else { 0.0 }));
                    state
                        .notes
                        .push((tag + 3.0, if snap.requested { 1.0 } else { 0.0 }));
                    state
                        .notes
                        .push((tag + 4.0, if snap.running { 1.0 } else { 0.0 }));
                })?;
                Value::Float(snap.logical as f64)
            }
            Self::MarkPhase => {
                let [Value::Float(tag)] = args.as_slice() else {
                    return Err(host_fault("expected mark phase tag"));
                };
                let mark = heap::saved_mark_query(&host);
                let meter = heap::copy_scan_meter(&mut host).map_err(host_heap)?;
                let stats = heap::stats(&mut host).map_err(host_heap)?;
                let collects = host
                    .scope_state::<candidate::ReturnWitness>()
                    .map(|witness| witness.collects.len())
                    .unwrap_or(0);
                note(&mut host, |state| {
                    state.marks.push(MarkNote {
                        tag: *tag,
                        phase: mark.map(|(phase, _)| phase),
                        pending: mark.is_some_and(|(_, pending)| pending),
                        parked_root_writes: meter.parked_root_writes,
                        work_pushed: meter.scratch.work_pushed,
                        mark_passes: meter.mark_passes,
                        live_nodes: stats.live_nodes,
                        collects,
                    });
                })?;
                Value::Float(0.0)
            }
            Self::Occupy => {
                let [Value::Float(tag)] = args.as_slice() else {
                    return Err(host_fault("expected occupy tag"));
                };
                let snap = {
                    let Some(heap) = host.scope_state::<heap::LuaHeap>() else {
                        return Err(host_fault("occupy requires the installed heap"));
                    };
                    let stats = heap.stats();
                    let mark = heap.saved_mark_query();
                    let meter = heap.scan_meter();
                    OccupancyNote {
                        tag: *tag,
                        root_slots: stats.root_slots,
                        live_roots: stats.live_roots,
                        live_nodes: stats.live_nodes,
                        node_slots: stats.node_slots,
                        edge_slots: stats.edge_slots,
                        phase: mark.map(|(phase, _)| phase),
                        mark_passes: meter.mark_passes,
                        node_slot_visits: meter.node_slot_visits,
                    }
                };
                note(&mut host, |state| state.occupancy.push(snap))?;
                Value::Float(0.0)
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
            root_slot_visits: meter.root_slot_visits,
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

/// The array from `external_shape`: saved and readable, but not a Lua table.
fn saved_external_shape(value: Option<&Value>) -> bool {
    let Some(Value::Array(items)) = value else {
        return false;
    };
    items.len() == 2
        && matches!(items.first(), Some(Value::Int(0)))
        && matches!(items.get(1), Some(Value::Int(1)))
}

fn host_heap(error: heap::HeapError) -> VmError {
    VmError::HostError(error.to_string())
}

fn run(source: &str) -> Ran {
    drive(source, None, false, None)
}

fn run_with_step_budgets(source: &str, budgets: Option<&[u64]>) -> Ran {
    drive(source, budgets, false, None)
}

fn run_control(source: &str, budgets: Option<&[u64]>) -> Ran {
    drive(source, budgets, true, None)
}

fn run_auto_table(source: &str, budgets: Option<&[u64]>) -> Ran {
    drive_plugin_opts(
        source,
        budgets,
        &AUTO_TABLE_PLUGIN,
        None,
        8,
        AutoReleaseFault::None,
    )
}

fn run_auto_table_limited(
    source: &str,
    budgets: Option<&[u64]>,
    nodes: u32,
    max_generation: u32,
    fail_after_registration: bool,
) -> Ran {
    drive_plugin_opts(
        source,
        budgets,
        &AUTO_TABLE_PLUGIN,
        Some(nodes),
        max_generation,
        if fail_after_registration {
            AutoReleaseFault::AfterRegistration
        } else {
            AutoReleaseFault::None
        },
    )
}

fn run_auto_table_post_commit(source: &str, budgets: Option<&[u64]>) -> Ran {
    drive_plugin_opts(
        source,
        budgets,
        &AUTO_TABLE_PLUGIN,
        Some(64),
        8,
        AutoReleaseFault::AfterCommit,
    )
}

fn drive(source: &str, budgets: Option<&[u64]>, run_control: bool, nodes: Option<u32>) -> Ran {
    let plugin: &Plugin = if run_control {
        &CONTROL_PLUGIN
    } else if budgets.is_some() {
        &STEP_PLUGIN
    } else {
        &PLUGIN
    };
    drive_plugin(source, budgets, plugin, nodes)
}

fn drive_plugin(
    source: &str,
    budgets: Option<&[u64]>,
    plugin: &'static Plugin,
    nodes: Option<u32>,
) -> Ran {
    drive_plugin_opts(source, budgets, plugin, nodes, 8, AutoReleaseFault::None)
}

#[derive(Clone, Copy)]
enum AutoReleaseFault {
    None,
    AfterRegistration,
    AfterCommit,
}

fn drive_plugin_opts(
    source: &str,
    budgets: Option<&[u64]>,
    plugin: &'static Plugin,
    nodes: Option<u32>,
    max_generation: u32,
    fault: AutoReleaseFault,
) -> Ran {
    let catalog = Arc::clone(plugin_catalog(plugin));
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(plugin)
        .with_host_api_catalog(Arc::clone(&catalog));
    let prepared = vm::compile_source_with_flavor_and_options(source, SourceFlavor::Lua, options)
        .unwrap_or_else(|error| panic!("compile rejected the source: {error}\n{source}"));
    let mut vm = prepared.into_vm().expect("create VM");
    assert_return_window(vm.program());
    let mut registry = HostFunctionRegistry::empty();
    candidate::register(&mut registry, &catalog).expect("register candidate runtime");
    register(&mut registry, &catalog);
    registry.bind_vm_cached(&mut vm).expect("bind runtime");
    if let Some(budgets) = budgets {
        let budgets: Vec<_> = budgets
            .iter()
            .map(|&budget| NonZeroU64::new(budget).expect("positive fixture budget"))
            .collect();
        candidate::set_step_budgets(&mut vm.host_context(), &budgets);
    }
    if let Some(nodes) = nodes {
        heap::install(
            &mut vm.host_context(),
            heap::HeapLimits {
                nodes,
                roots: 64,
                edges: 64,
                max_generation,
            },
        )
        .expect("test host installs the node quota before execution");
    }
    match fault {
        AutoReleaseFault::AfterRegistration => {
            candidate::arm_release_alloc_fault(&mut vm.host_context()).expect("arm release fault");
        }
        AutoReleaseFault::AfterCommit => {
            candidate::arm_release_alloc_post_commit_fault(&mut vm.host_context())
                .expect("arm the post-commit injection");
        }
        AutoReleaseFault::None => {}
    }
    if plugin.auto_table {
        vm.host_context()
            .scope_state_or_insert_with(candidate::VisitSamples::default)
            .expect("record root-slot visits");
    }
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
    let (returned, release_root) = vm
        .host_context()
        .scope_state::<candidate::ReturnWitness>()
        .map(|witness| (witness.returned, witness.release_root.clone()))
        .unwrap_or((None, None));
    let budgets = candidate::remaining_step_budgets(&mut vm.host_context());
    let running = candidate::gc_running(&vm.host_context());
    let status_debug = format!("{status:?}");
    let stop = match status {
        Ok(VmStatus::Halted) => Stop::Halted,
        Err(VmError::HostError(message)) => Stop::HostError(message),
        Ok(other) => Stop::Other(format!("{other:?}")),
        Err(other) => Stop::Other(other.to_string()),
    };
    let mark = heap::saved_mark_query(&vm.host_context());
    let meter = heap::copy_scan_meter(&mut vm.host_context()).unwrap_or_default();
    let tail_state = heap::saved_tail_state(&mut vm.host_context())
        .ok()
        .flatten();
    let peek_end: Vec<(bool, bool)> = trace
        .peeks
        .iter()
        .map(|observed| {
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
            (alive, open)
        })
        .collect();
    let resources = vm.host_context().resource_count();
    let (reports, issued_alive) = vm
        .host_context()
        .scope_state::<candidate::ReturnWitness>()
        .map(|witness| {
            (
                witness.collects.clone(),
                witness.issued_alive_at_collect.clone(),
            )
        })
        .unwrap_or_default();
    let root_visits = vm
        .host_context()
        .scope_state::<candidate::VisitSamples>()
        .map(|samples| samples.visits.clone())
        .unwrap_or_default();
    let tail_accounting = heap::saved_tail_accounting(&mut vm.host_context())
        .unwrap_or_else(|error| panic!("tail accounting failed after {status_debug}: {error}"));
    let top_roots = heap::top_frame_tables(&mut vm.host_context())
        .unwrap_or_else(|error| panic!("top frame read failed after {status_debug}: {error}"));
    let permit = candidate::release_alloc_permit(&vm.host_context());
    let txn = heap::release_txn_witness(&vm.host_context());
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
        returned,
        release_root,
        budgets,
        running,
        mark_phase: mark.map(|(phase, _)| phase),
        mark_pending: mark.is_some_and(|(_, pending)| pending),
        parked_root_writes: meter.parked_root_writes,
        tail_state,
        peek_end,
        resources,
        reports,
        issued_alive,
        root_visits,
        tail_accounting,
        top_roots,
        permit,
        txn,
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

fn bit_value(trace: &Trace, tag: f64) -> bool {
    trace
        .bits
        .iter()
        .find(|(found, _)| *found == tag)
        .unwrap_or_else(|| panic!("missing bit {tag} in {:?}", trace.bits))
        .1
}

fn cycle_at(trace: &Trace, tag: f64) -> (bool, u32) {
    let index = trace
        .bits
        .iter()
        .position(|(found, _)| *found == tag)
        .unwrap_or_else(|| panic!("missing bit {tag} in {:?}", trace.bits));
    trace.bit_cycle_state[index]
}

const STEP_WITNESS: &str = r#"local function witness()
  local w = {}
  local mt = {}
  local t = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = t
  w[2] = {}
  w[3] = {}
  peek(t)
  note(10, rawlen(w))
  note(20, same(t, rawget(w, 1)))
  -- tick 1
  note(11, rawlen(w))
  note(21, same(t, rawget(w, 1)))
  -- tick 2
  note(12, rawlen(w))
  note(22, same(t, rawget(w, 1)))
  -- tick 3
  note(13, rawlen(w))
  note(23, same(t, rawget(w, 1)))
  -- tick 4
  note(14, rawlen(w))
  note(24, same(t, rawget(w, 1)))
  text(2, type(rawget(w, 2)))
  text(3, type(rawget(w, 3)))
  return 1
end
witness()
return 1
"#;

fn run_step_witness(arm: &str, ticks: [&str; 4], budgets: Option<&[u64]>) -> Ran {
    let mut source = STEP_WITNESS.to_owned();
    for (i, tick) in ticks.into_iter().enumerate() {
        if !tick.is_empty() {
            source = source.replace(&format!("-- tick {}", i + 1), tick);
        }
    }
    let ran = run_with_step_budgets(&source, budgets);
    assert_eq!(ran.stop, Stop::Halted, "{arm}: {ran:#?}");
    assert_eq!(ran.stack, [Value::Float(1.0)]);
    let original = ran.trace.peeks[0].table;
    assert_eq!(ran.trace.peeks.len(), 1);
    assert_eq!(ran.trace.sames, [(original, original); 5]);
    for tag in [20.0, 21.0, 22.0, 23.0, 24.0] {
        assert_eq!(note_value(&ran.trace, tag), 1.0);
    }
    assert!(!ran.tail, "all successful arms leave no saved tail");
    assert!(ran.frame.is_none());
    ran
}

fn weak_lengths(ran: &Ran) -> [f64; 5] {
    [10.0, 11.0, 12.0, 13.0, 14.0].map(|tag| note_value(&ran.trace, tag))
}

fn assert_final_weak_types(ran: &Ran, expected: &str) {
    assert_eq!(text_value(&ran.trace, 2.0), expected);
    assert_eq!(text_value(&ran.trace, 3.0), expected);
}

/// The opt-in slice only lets the guest spell the default data. An empty
/// `data` is the already-supported omitted argument.
fn collect_step_call(data: &str) -> String {
    if data.is_empty() {
        r#"collectgarbage("step")"#.to_string()
    } else {
        format!(r#"collectgarbage("step", {data})"#)
    }
}

fn assert_guest_step_cycle(ran: &Ran) {
    assert_eq!(weak_lengths(ran), [3.0, 3.0, 1.0, 1.0, 1.0]);
    assert_eq!(
        ran.trace.bits,
        [(1.0, false), (2.0, false), (3.0, false), (4.0, true)]
    );
    assert_eq!(
        ran.trace.bit_cycle_state,
        [(false, 0), (true, 0), (true, 0), (false, 1)]
    );
    assert_final_weak_types(ran, "nil");
    assert!(ran.trace.steps.is_empty(), "no private test step host ran");
    assert_eq!(ran.collects, 1, "one completion report");
    assert_eq!(ran.collections, 1, "one completed round");
}

fn assert_step_source_rejected(source: &str) {
    let error = candidate::lower(source, catalog(true))
        .expect_err("the opt-in step lowering accepted an unsupported shape");
    assert!(
        error.message.contains("not lowered yet"),
        "expected the existing unsupported path for {source:?}, got {error}"
    );
}

#[test]
fn guest_step_reports_completion_after_visible_weak_clear() {
    let ran = run_step_witness(
        "guest-step",
        [
            r#"bit(1, collectgarbage("step"))"#,
            r#"bit(2, collectgarbage("step"))"#,
            r#"bit(3, collectgarbage("step"))"#,
            r#"bit(4, collectgarbage("step"))"#,
        ],
        Some(&[1, 10_000, 1, 10_000]),
    );
    let lengths = weak_lengths(&ran);
    assert_eq!(
        ran.trace.bits,
        [(1.0, false), (2.0, false), (3.0, false), (4.0, true)]
    );
    assert_eq!(
        ran.trace.bit_cycle_state,
        [(false, 0), (true, 0), (true, 0), (false, 1)]
    );
    assert_eq!(lengths[0], 3.0);
    assert_eq!(lengths[4], 1.0);
    assert!(
        lengths[1..]
            .iter()
            .zip(&ran.trace.bit_cycle_state)
            .any(|(&length, &(tail, collections))| length == 1.0 && tail && collections == 0),
        "weak clear visible while the incomplete tail is saved",
    );
    assert_final_weak_types(&ran, "nil");
    assert!(ran.trace.steps.is_empty(), "no private test step host ran");
    assert_eq!(ran.collects, 1);
    assert_eq!(ran.collections, 1);
}

#[test]
fn guest_step_explicit_default_data_matches_omitted() {
    for (label, data) in [
        ("omitted", ""),
        ("nil", "nil"),
        ("zero", "0"),
        ("string-zero", "\"0\""),
    ] {
        let calls: Vec<_> = (1..=4)
            .map(|tag| format!("bit({tag}, {})", collect_step_call(data)))
            .collect();
        let ran = run_step_witness(
            label,
            [
                calls[0].as_str(),
                calls[1].as_str(),
                calls[2].as_str(),
                calls[3].as_str(),
            ],
            Some(&[1, 10_000, 1, 10_000]),
        );
        assert_guest_step_cycle(&ran);
    }
}

#[test]
fn guest_step_explicit_default_data_rejects_other_shapes() {
    for source in [
        "return collectgarbage(\"step\", nil, nil)\n",
        "local opt = \"step\"\nreturn collectgarbage(opt, nil)\n",
        // Unary minus is not an ordinary expression. The candidate's number
        // token parses as positive zero; source unary + is also unsupported.
        "return collectgarbage(\"step\", -0)\n",
        "local collectgarbage = nil\nreturn collectgarbage(\"step\", nil)\n",
        "local function collectgarbage()\n  return nil\nend\nreturn collectgarbage(\"step\", nil)\n",
    ] {
        assert_step_source_rejected(source);
    }
    let ordinary = candidate::lower("return collectgarbage(\"step\", nil)\n", catalog(false))
        .expect_err("ordinary catalog accepted explicit step data");
    assert!(ordinary.message.contains("not lowered yet"), "{ordinary}");
    let runtime = candidate::lower(
        "local n = nil\nreturn collectgarbage(\"step\", n)\n",
        catalog(false),
    )
    .expect_err("ordinary catalog accepted runtime step data");
    assert!(runtime.message.contains("not lowered yet"), "{runtime}");
}

fn witness_chunk(body: &str) -> String {
    format!("local function witness()\n{body}  return 1\nend\nwitness()\nreturn 1\n")
}

/// Runtime data of the same three categories uses the omitted step's bool,
/// weak-clear timing, and single completion report. Literals stay on the
/// reviewed arm above.
#[test]
fn guest_step_runtime_default_data_matches_omitted() {
    for (label, data, prelude) in [
        ("runtime-nil", "(nil)", ""),
        ("runtime-zero", "0+0", ""),
        ("runtime-string", "s", "  local s = \"0\"\n"),
    ] {
        let mut source = STEP_WITNESS.to_owned();
        if !prelude.is_empty() {
            source = source.replacen("-- tick 1", &format!("{prelude}  -- tick 1"), 1);
        }
        for tag in 1..=4 {
            source = source.replace(
                &format!("-- tick {tag}"),
                &format!("bit({tag}, {})", collect_step_call(data)),
            );
        }
        let ran = run_with_step_budgets(&source, Some(&[1, 10_000, 1, 10_000]));
        assert_eq!(ran.stop, Stop::Halted, "{label}: {ran:#?}");
        assert_eq!(ran.stack, [Value::Float(1.0)], "{label}");
        assert_guest_step_cycle(&ran);
        assert!(ran.budgets.is_empty(), "{label} left {:?}", ran.budgets);
    }
}

/// The data call runs once, including when its value is not a default.
#[test]
fn guest_step_runtime_default_data_evaluates_once() {
    let accepted = run_with_step_budgets(
        &witness_chunk("  bit(1, collectgarbage(\"step\", note(7, 0)))\n"),
        Some(&[1]),
    );
    assert_eq!(accepted.stop, Stop::Halted, "{accepted:#?}");
    assert_eq!(accepted.trace.notes, [(7.0, 0.0)]);
    assert_eq!(accepted.trace.bits.len(), 1, "{accepted:#?}");
    assert_eq!(accepted.trace.bits[0].0, 1.0);
    assert!(accepted.budgets.is_empty(), "{:?}", accepted.budgets);

    let rejected = run_with_step_budgets(
        &witness_chunk("  bit(1, collectgarbage(\"step\", note(8, 1)))\n"),
        Some(&[1, 9]),
    );
    assert!(
        matches!(rejected.stop, Stop::HostError(ref message) if message == "step data is not the default"),
        "{rejected:#?}"
    );
    assert_eq!(
        rejected.trace.notes,
        [(8.0, 1.0)],
        "the rejected call still evaluated once"
    );
    assert!(
        rejected.trace.bits.is_empty(),
        "bit did not run: {rejected:#?}"
    );
    assert_eq!(rejected.budgets, [1, 9]);
    assert!(!rejected.tail);
    assert_eq!(rejected.collects, 0);
    assert_eq!(rejected.collections, 0);
}

/// A bool or non-default number is refused before any step or root gather.
/// The live unissued value would make a mark gather fail with a different fault.
#[test]
fn guest_step_runtime_default_data_rejects_before_advance() {
    let queued = [1_u64, 10_000];
    for (label, data) in [
        ("bool", "true"),
        ("number", "1"),
        ("text", "\"1\""),
        // Parent lowering negatives. After the join they lower, then the
        // category host refuses them before a step. They are not new data.
        ("text-0.0", "\"0.0\""),
        ("text-00", "\"00\""),
    ] {
        let ran = run_with_step_budgets(
            &witness_chunk(&format!(
                "  local unissued = external_shape()\n  bit(1, collectgarbage(\"step\", {data}))\n"
            )),
            Some(&queued),
        );
        assert!(
            matches!(ran.stop, Stop::HostError(ref message) if message == "step data is not the default"),
            "{label}: {ran:#?}"
        );
        assert_eq!(ran.budgets, queued, "{label}");
        assert!(!ran.tail, "{label}");
        assert_eq!(ran.collects, 0, "{label}");
        assert_eq!(ran.collections, 0, "{label}");
        assert!(ran.trace.bits.is_empty(), "{label}");
        assert!(ran.trace.steps.is_empty(), "{label}");
    }
    let gathered = run_with_step_budgets(
        &witness_chunk("  local unissued = external_shape()\n  bit(1, collectgarbage(\"step\"))\n"),
        Some(&queued),
    );
    assert!(
        matches!(
            gathered.stop,
            Stop::HostError(ref message) if message.contains("invalid private Lua root candidate")
        ),
        "mark gather control: {gathered:#?}"
    );
    assert_eq!(gathered.budgets, queued, "gather fails before a budget pop");
    assert!(!gathered.tail);
    assert_eq!(gathered.collects, 0);
}

/// An already-supported step inside the data call crosses publish. The outer
/// step then reads that tail and does not gather the caller's unissued value.
/// Cancelling the inner steps makes the same outer call gather and refuse.
#[test]
fn guest_step_runtime_data_reads_tail_after_nested_step() {
    let stepped = r#"
local function produce()
  local w = {}
  local mt = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = {}
  w[2] = {}
  collectgarbage("step")
  collectgarbage("step")
  return nil
end
local unissued = external_shape()
bit(1, collectgarbage("step", produce()))
return 1
"#;
    let cancelled = r#"
local function produce()
  return nil
end
local unissued = external_shape()
bit(1, collectgarbage("step", produce()))
return 1
"#;
    let ran = run_with_step_budgets(stepped, Some(&[1, 10_000, 1]));
    assert_eq!(ran.trace.bits, [(1.0, false)], "{ran:#?}");
    assert_eq!(ran.trace.bit_cycle_state, [(true, 0)]);
    assert!(ran.tail, "the outer step continued the saved tail");
    assert!(ran.budgets.is_empty(), "{:?}", ran.budgets);
    assert_eq!(ran.collects, 0);
    assert_eq!(ran.collections, 0);
    assert_sweep_pending(&ran, "chunk exit keeps the tail the outer step read");

    let idle = run_with_step_budgets(cancelled, Some(&[1, 10_000, 1]));
    assert!(
        matches!(
            idle.stop,
            Stop::HostError(ref message) if message.contains("invalid private Lua root candidate")
        ),
        "cancelled inner step must gather, not step: {idle:#?}"
    );
    assert_eq!(idle.budgets, [1, 10_000, 1]);
    assert!(!idle.tail);
    assert_eq!(idle.collects, 0);
    assert!(idle.trace.bits.is_empty());
}

/// A same-name function with arguments is the existing arity refusal. A local
/// binding is the dedicated shadow path. Neither is the opt-in step.
#[test]
fn guest_step_same_name_function_is_not_the_shadow_path() {
    let function = run_with_step_budgets(
        "local function collectgarbage()\n  return nil\nend\nreturn collectgarbage()\n",
        Some(&[1]),
    );
    assert_eq!(function.stop, Stop::Halted, "{function:#?}");
    assert_eq!(function.stack, [Value::Null]);
    assert_eq!(function.budgets, [1]);
    assert!(function.trace.bits.is_empty());
    assert!(!function.tail);

    let arity = candidate::lower(
        "local function collectgarbage()\n  return nil\nend\nreturn collectgarbage(\"step\", nil)\n",
        catalog(true),
    )
    .expect_err("a same-name function accepted arguments");
    assert!(arity.message.contains("not lowered yet"), "{arity}");
    assert_eq!(arity.line, 4usize, "arity refusal is the call, got {arity}");

    let shadow = candidate::lower(
        "local collectgarbage = nil\nreturn collectgarbage(\"step\", nil)\n",
        catalog(true),
    )
    .expect_err("a shadowed name became the step builtin");
    assert!(shadow.message.contains("not lowered yet"), "{shadow}");
    assert_eq!(
        shadow.line, 2usize,
        "shadow refusal is the call, got {shadow}"
    );

    let init = candidate::lower(
        "local collectgarbage = type\nreturn collectgarbage(\"step\", nil)\n",
        catalog(true),
    )
    .expect_err("an unbound value initializer was accepted");
    assert!(init.message.contains("not lowered yet"), "{init}");
    assert_eq!(
        init.line, 1usize,
        "this is the initializer, not the call shadow, got {init}"
    );
}

const TEMPORARY_STEP: &str = r#"
local function witness()
  local w = {}
  local mt = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = {}
  w[2] = {}
  peek(rawget(w, 1))
  -- publish
  text(1, type(rawget(w, 1)))
  text(2, type(rawget(w, 2)))
  -- identity
  -- This is not a guest value. Any root gather during the saved tail rejects it.
  local unissued = external_shape()
  bit(2, collectgarbage("step"))
  return 1
end
witness()
return 1
"#;

#[test]
fn guest_step_marks_current_argument_and_skips_tail_root_gather() {
    for hold_argument in [true, false] {
        let (publish, identity) = if hold_argument {
            (
                r#"bit(1, rawequal(rawget(w, 1), (collectgarbage("step"))))"#,
                "peek(rawget(w, 1))",
            )
        } else {
            (r#"bit(1, collectgarbage("step"))"#, "")
        };
        let ran = run_with_step_budgets(
            &TEMPORARY_STEP
                .replace("-- publish", publish)
                .replace("-- identity", identity),
            Some(&[10_000, 10_000]),
        );
        assert_eq!(ran.stop, Stop::Halted, "{ran:#?}");
        assert_eq!(ran.trace.bits, [(1.0, false), (2.0, true)]);
        assert_eq!(ran.trace.bit_cycle_state, [(true, 0), (false, 1)]);
        assert_eq!(
            text_value(&ran.trace, 1.0),
            if hold_argument { "table" } else { "nil" }
        );
        assert_eq!(text_value(&ran.trace, 2.0), "nil");
        assert_eq!(ran.trace.peeks.len(), if hold_argument { 2 } else { 1 });
        if hold_argument {
            assert_eq!(ran.trace.peeks[0].table, ran.trace.peeks[1].table);
        }
        assert_eq!(ran.subject_alive, Some(hold_argument));
        assert!(ran.frame.is_none() && !ran.tail);
        assert_eq!(ran.root_slots, 0);
        assert_eq!(ran.collections, 1);
        assert!(ran.trace.steps.is_empty());
    }
}

#[test]
fn private_step_matches_guest_witness_graph() {
    let ran = run_step_witness(
        "private-step",
        [
            "note(1, step(1))",
            "note(2, step(10000))",
            "note(3, step(1))",
            "note(4, step(10000))",
        ],
        None,
    );
    assert_eq!(weak_lengths(&ran), [3.0, 3.0, 1.0, 1.0, 1.0]);
    assert_final_weak_types(&ran, "nil");
    let [mark, publish, release, done] = ran.trace.steps.as_slice() else {
        panic!("expected four private step observations: {ran:#?}");
    };
    assert!(matches!(mark.outcome, StepOutcome::Paused { tail: false }));
    assert!(matches!(publish.outcome, StepOutcome::Published { .. }));
    assert!(matches!(
        release.outcome,
        StepOutcome::Paused { tail: true }
    ));
    assert!(matches!(done.outcome, StepOutcome::Completed(_)));
    assert_eq!(
        [mark, publish, release, done].map(|step| step.collections),
        [0, 0, 0, 1]
    );
    assert_eq!(
        [1.0, 2.0, 3.0, 4.0].map(|tag| note_value(&ran.trace, tag)),
        [0.0, 1.0, 2.0, 3.0]
    );
    assert_eq!(ran.collections, 1);
    assert!(ran.trace.bits.is_empty());
}

#[test]
fn full_collect_matches_guest_witness_graph() {
    let ran = run_step_witness(
        "full-collect",
        [r#"collectgarbage("collect")"#, "", "", ""],
        None,
    );
    assert_eq!(weak_lengths(&ran), [3.0, 1.0, 1.0, 1.0, 1.0]);
    assert_final_weak_types(&ran, "nil");
    assert!(ran.trace.steps.is_empty());
    assert_eq!(ran.collects, 1);
    assert_eq!(ran.collections, 1);
}

#[test]
fn omitted_collect_matches_guest_witness_graph() {
    let ran = run_step_witness("omitted", [""; 4], None);
    assert_eq!(weak_lengths(&ran), [3.0; 5]);
    assert_final_weak_types(&ran, "table");
    assert!(ran.trace.steps.is_empty());
    assert_eq!(ran.collects, 0);
    assert_eq!(ran.collections, 0);
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

const RETAIN_OPEN: &str = r#"
local function callee()
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
  note(12, same(t, rawget(t, 1)))
  return t
end
local kept = callee()
peek(kept)
note(13, same(kept, rawget(kept, 1)))
collectgarbage("collect")
peek(kept)
note(14, same(kept, rawget(kept, 1)))
kept = nil
collectgarbage("collect")
return 1
"#;

const RETAIN_RELEASE: &str = r#"
local function callee()
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
  note(12, same(t, rawget(t, 1)))
  text(2, type(rawget(w, 2)))
  text(3, type(rawget(w, 3)))
  step(1)
  return t
end
local kept = callee()
peek(kept)
note(13, same(kept, rawget(kept, 1)))
step(10000)
collectgarbage("collect")
peek(kept)
note(14, same(kept, rawget(kept, 1)))
kept = nil
collectgarbage("collect")
return 1
"#;

fn assert_kept_identity(ran: &Ran) {
    assert_eq!(ran.stop, Stop::Halted, "{ran:#?}");
    assert_eq!(ran.stack, vec![Value::Float(1.0)], "{ran:#?}");
    assert_eq!(
        ran.trace.peeks.len(),
        3,
        "callee, caller, and held collect: {ran:#?}"
    );
    let issued = ran.trace.peeks[0].table;
    assert_eq!(
        ran.trace.peeks[1].table, issued,
        "caller keeps the original table"
    );
    assert_eq!(
        ran.trace.peeks[2].table, issued,
        "the next collection keeps that identity"
    );
    assert_eq!(issued.domain(), ran.trace.peeks[1].table.domain());
    assert_eq!(issued.slot(), ran.trace.peeks[1].table.slot());
    assert_eq!(issued.generation(), ran.trace.peeks[1].table.generation());
    assert_eq!(note_value(&ran.trace, 12.0), 1.0, "self-loop before return");
    assert_eq!(note_value(&ran.trace, 13.0), 1.0, "self-loop after publish");
    assert_eq!(
        note_value(&ran.trace, 14.0),
        1.0,
        "self-loop after the held collection"
    );
    assert_eq!(ran.returned, Some(issued));
    let handoff = ran.handoff.as_ref().expect("finish_return witness");
    assert!(handoff.published_root, "the caller slot receives the table");
    assert!(
        handoff.callee_left,
        "the callee frame leaves after the publish"
    );
    assert!(
        handoff.lua_frame_alive_at_publish,
        "the callee frame is still current when the caller slot is written"
    );
    assert!(matches!(
        handoff.elements.as_slice(),
        [ReturnedElement::Table {
            same_ref: true,
            same_carrier: true
        }]
    ));
    assert_eq!(
        ran.subject_alive,
        Some(false),
        "clearing the slot lets the next collection reclaim it"
    );
    assert_eq!(
        ran.token_open,
        Some(false),
        "that collection closes the original token"
    );
    assert_eq!(ran.live_roots, 0);
    assert_eq!(ran.collects, 2, "held collect, then the clear collect");
}

#[test]
fn no_tail_local_keeps_the_returned_table_identity() {
    let ran = run(RETAIN_OPEN);
    assert!(!ran.tail, "no tail was saved: {ran:#?}");
    assert!(
        ran.release_root.is_none(),
        "the open path does not use the Release writer"
    );
    assert_kept_identity(&ran);
    assert_eq!(ran.collections, 2, "only the two guest collections ran");
}

#[test]
fn published_release_retains_one_returned_table() {
    let ran = run(RETAIN_RELEASE);
    assert_kept_identity(&ran);
    let steps = &ran.trace.steps;
    assert!(
        steps.len() >= 4,
        "publish, release pause, then the continuation: {steps:?}"
    );
    assert!(matches!(
        steps[0].outcome,
        StepOutcome::Paused { tail: false }
    ));
    assert!(matches!(steps[1].outcome, StepOutcome::Published { .. }));
    assert!(matches!(
        steps[2].outcome,
        StepOutcome::Paused { tail: true }
    ));
    assert_eq!(
        steps[2].node_slot_visits, 1,
        "one release visit before the return"
    );
    assert_eq!(
        steps[2].root_slot_visits, 0,
        "Release has not scanned roots"
    );
    assert_eq!(note_value(&ran.trace, 30.0), 3.0);
    assert_eq!(
        note_value(&ran.trace, 10.0),
        1.0,
        "weak clear was published"
    );
    assert_eq!(note_value(&ran.trace, 11.0), 1.0);
    assert_eq!(text_value(&ran.trace, 2.0), "nil");
    assert_eq!(text_value(&ran.trace, 3.0), "nil");
    let note = ran.release_root.as_ref().expect("Release handoff note");
    assert_eq!(
        note.meter_before, note.meter_after,
        "the handoff does not scan"
    );
    assert_eq!(
        note.meter_before.root_slot_visits,
        note.meter_after.root_slot_visits
    );
    assert_eq!(note.tail_before.phase, heap::SavedTailPhase::Release);
    assert_eq!(note.tail_after.phase, heap::SavedTailPhase::Release);
    assert_eq!(note.tail_before.cursor, note.tail_after.cursor);
    assert_eq!(note.tail_before.live_roots, note.tail_after.live_roots);
    assert_eq!(note.tail_before.dead_len, note.tail_after.dead_len);
    assert_eq!(
        note.tail_before.live_roots, 0,
        "Release has not accounted a root yet"
    );
    let completed: Vec<_> = steps
        .iter()
        .filter(|step| matches!(step.outcome, StepOutcome::Completed(_)))
        .collect();
    assert_eq!(completed.len(), 1, "this cycle reports once: {steps:?}");
    let report = match completed[0].outcome {
        StepOutcome::Completed(report) => report,
        _ => unreachable!("filtered"),
    };
    assert_eq!(
        report.live_roots, 1,
        "the continuation counts the caller slot once"
    );
    assert_eq!(
        completed[0].root_slot_visits, 1,
        "the later root scan visits that slot once"
    );
    assert!(
        steps
            .iter()
            .take(3)
            .all(|step| !matches!(step.outcome, StepOutcome::Completed(_))),
        "completion is after the return"
    );
    assert_ne!(
        steps[2].frame, completed[0].frame,
        "the callee has left before the report"
    );
    assert_eq!(
        ran.collections, 3,
        "tail report, held collect, then the clear collect"
    );
}

#[derive(Clone, Copy, Debug)]
enum Refusal {
    WrongFrame,
    StaleGeneration,
    Occupied,
    Unmarked,
    ClosedToken,
    Roots,
    NoTail,
}

struct ReleaseGraph {
    caller: heap::RootFrameId,
    callee: heap::RootFrameId,
    subject: Value,
    subject_ref: heap::TableRef,
    unmarked: Value,
    spare: Value,
    occupant: Option<heap::TableRef>,
}

fn table_of(host: &mut vm::HostContext<'_>, value: &Value) -> heap::TableRef {
    match heap::with_heap(host, |heap| Ok(heap.guest_class(value))).expect("class") {
        heap::GuestClass::Table(table) => table,
        other => panic!("expected an issued table, got {other:?}"),
    }
}

fn open_release(vm: &mut vm::Vm, prefill: bool) -> ReleaseGraph {
    let mut host = vm.host_context();
    heap::install(&mut host, heap::HeapLimits::production()).expect("install");
    let caller = heap::with_heap(&mut host, |heap| heap.enter_roots(1)).expect("caller");
    let occupant = if prefill {
        let value = heap::publish_guest(&mut host).expect("occupant");
        let table = table_of(&mut host, &value);
        heap::with_heap(&mut host, |heap| heap.write_root(caller, 0, table)).expect("prefill");
        Some(table)
    } else {
        None
    };
    let callee = heap::with_heap(&mut host, |heap| heap.enter_roots(1)).expect("callee");
    let subject = heap::publish_guest(&mut host).expect("subject");
    let subject_ref = table_of(&mut host, &subject);
    heap::with_heap(&mut host, |heap| heap.write_root(callee, 0, subject_ref))
        .expect("root subject");
    let unmarked = heap::publish_guest(&mut host).expect("unmarked");
    let spare = heap::publish_guest(&mut host).expect("spare unmarked");
    let _dead = heap::publish(&mut host).expect("dead slot");
    heap::open_fixed_step(&mut host).expect("open");
    let _pause = heap::step_fixed_frames(&mut host, 1).expect("mark pause");
    let published = heap::step_fixed_frames(&mut host, 10_000).expect("publish");
    assert!(
        matches!(published, StepOutcome::Published { .. }),
        "the graph must publish a tail: {published:?}"
    );
    let release = heap::step_fixed_frames(&mut host, 1).expect("release");
    assert!(
        matches!(release, StepOutcome::Paused { tail: true }),
        "one release unit must leave the tail saved: {release:?}"
    );
    let state = heap::saved_tail_state(&mut host)
        .expect("tail")
        .expect("saved");
    assert_eq!(state.phase, heap::SavedTailPhase::Release);
    ReleaseGraph {
        caller,
        callee,
        subject,
        subject_ref,
        unmarked,
        spare,
        occupant,
    }
}

fn drive_to_roots(host: &mut vm::HostContext<'_>) {
    for _ in 0..16 {
        let state = heap::saved_tail_state(host).expect("tail read");
        if state.is_some_and(|item| item.phase == heap::SavedTailPhase::Roots) {
            return;
        }
        assert!(state.is_some(), "the tail ended before Roots");
        heap::step_fixed_frames(host, 1).expect("step toward Roots");
    }
    panic!("did not reach Roots");
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AdmissionSnap {
    tail: Option<heap::SavedTailState>,
    meter: heap::ScanMeter,
    stats: heap::HeapStats,
    caller_root: Option<heap::TableRef>,
    frame: Option<heap::RootFrameId>,
}

fn admission_snap(host: &mut vm::HostContext<'_>, caller: heap::RootFrameId) -> AdmissionSnap {
    AdmissionSnap {
        tail: heap::saved_tail_state(host).expect("tail"),
        meter: heap::copy_scan_meter(host).expect("meter"),
        stats: heap::stats(host).expect("stats"),
        caller_root: heap::rooted_table(host, caller, 0).expect("slot"),
        frame: heap::with_heap(host, |heap| Ok(heap.current_frame())).expect("frame"),
    }
}

#[test]
fn release_return_root_refuses_without_writing() {
    for case in [
        Refusal::WrongFrame,
        Refusal::StaleGeneration,
        Refusal::Occupied,
        Refusal::Unmarked,
        Refusal::ClosedToken,
        Refusal::Roots,
        Refusal::NoTail,
    ] {
        crate::support::with_idle_vm(|vm| {
            let graph = open_release(vm, matches!(case, Refusal::Occupied));
            let mut host = vm.host_context();
            if matches!(case, Refusal::Roots) {
                drive_to_roots(&mut host);
            }
            if matches!(case, Refusal::NoTail) {
                let done = heap::step_fixed_frames(&mut host, 10_000).expect("finish tail");
                assert!(
                    matches!(done, StepOutcome::Completed(_)),
                    "the no-tail case starts after the report: {done:?}"
                );
            }
            if matches!(case, Refusal::ClosedToken) {
                let raw =
                    heap::with_heap(&mut host, |heap| heap.published_token(graph.subject_ref))
                        .expect("binding");
                let handle = vm::ResourceHandle::from_raw(raw).expect("handle");
                host.close_resource::<heap::LuaTableResource>(
                    handle,
                    vm::ResourceCloseReason::Requested,
                )
                .expect("close token");
            }
            let before = admission_snap(&mut host, graph.caller);
            let unmarked_live = matches!(
                heap::with_heap(&mut host, |heap| Ok(heap.guest_class(&graph.unmarked)))
                    .expect("class"),
                heap::GuestClass::Table(_)
            );
            let value = if !matches!(case, Refusal::Unmarked) {
                &graph.subject
            } else if unmarked_live {
                &graph.unmarked
            } else {
                &graph.spare
            };
            if matches!(case, Refusal::Unmarked) {
                assert!(
                    matches!(
                        heap::with_heap(&mut host, |heap| Ok(heap.guest_class(value)))
                            .expect("class"),
                        heap::GuestClass::Table(_)
                    ),
                    "the unmarked refusal needs a still-issued table"
                );
            }
            let frame = match case {
                Refusal::WrongFrame => graph.callee,
                Refusal::StaleGeneration => {
                    let (index, generation) = graph.caller.parts();
                    heap::RootFrameId::from_parts(index, generation.wrapping_add(1))
                }
                _ => graph.caller,
            };
            let error = heap::publish_release_return_root(&mut host, frame, 0, value)
                .expect_err("the refused shape must not write");
            match case {
                Refusal::WrongFrame | Refusal::StaleGeneration => {
                    assert!(
                        matches!(error, heap::HeapError::StaleFrame),
                        "{case:?} returned {error:?}"
                    );
                }
                Refusal::NoTail => {
                    assert!(
                        matches!(error, heap::HeapError::SweepDenied),
                        "{case:?} returned {error:?}"
                    );
                }
                Refusal::ClosedToken => {
                    assert!(
                        matches!(error, heap::HeapError::Scope(_)),
                        "a stored raw id must not pass a closed token: {error:?}"
                    );
                    let still =
                        heap::with_heap(&mut host, |heap| heap.published_token(graph.subject_ref));
                    assert!(
                        still.is_ok(),
                        "the heap binding remains; Core usability is a separate check: {still:?}"
                    );
                }
                Refusal::Occupied | Refusal::Roots => {
                    assert!(
                        matches!(error, heap::HeapError::SweepPending),
                        "{case:?} returned {error:?}"
                    );
                }
                Refusal::Unmarked => {
                    assert!(
                        matches!(error, heap::HeapError::StaleHandle),
                        "publish already cleared the doomed binding: {error:?}"
                    );
                }
            }
            let after = admission_snap(&mut host, graph.caller);
            assert_eq!(before, after, "{case:?} changed heap state");
            if let Some(occupant) = graph.occupant {
                assert_eq!(
                    after.caller_root,
                    Some(occupant),
                    "the occupied slot was replaced"
                );
            }
        });
    }
}

fn bind_control(source: &str) -> vm::Vm {
    let catalog = Arc::clone(catalog_for(true, true));
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&CONTROL_PLUGIN)
        .with_host_api_catalog(Arc::clone(&catalog));
    let prepared = vm::compile_source_with_flavor_and_options(source, SourceFlavor::Lua, options)
        .unwrap_or_else(|error| panic!("compile rejected the source: {error}"));
    let mut vm = prepared.into_vm().expect("create VM");
    assert_return_window(vm.program());
    let mut registry = HostFunctionRegistry::empty();
    candidate::register(&mut registry, &catalog).expect("register candidate runtime");
    register(&mut registry, &catalog);
    registry.bind_vm_cached(&mut vm).expect("bind runtime");
    vm
}

fn finish_reset(vm: &mut vm::Vm) {
    vm.reset_for_reuse().expect("start reset");
    let waker = std::task::Waker::noop();
    let mut context = std::task::Context::from_waker(waker);
    match vm.poll_reset_for_reuse(&mut context) {
        std::task::Poll::Ready(Ok(())) => {}
        other => panic!("reset did not finish: {other:?}"),
    }
}

fn control_bits(vm: &mut vm::Vm) -> Vec<(f64, bool)> {
    vm.host_context()
        .scope_state::<Trace>()
        .map(|trace| trace.bits.clone())
        .unwrap_or_default()
}

const CONTROL_SWITCH: &str = r#"
local function witness()
  local t = {}
  peek(t)
  bit(1, collectgarbage("isrunning"))
  note(1, collectgarbage("stop"))
  bit(2, collectgarbage("isrunning"))
  return 1
end
witness()
return 1
"#;

#[test]
fn guest_run_control_is_observable_and_does_not_collect() {
    let ran = run_control(
        r#"
local function witness()
  local w = {}
  local mt = {}
  local t = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = t
  w[2] = {}
  peek(t)
  bit(1, collectgarbage("isrunning"))
  note(1, collectgarbage("stop"))
  note(2, collectgarbage("stop"))
  bit(2, collectgarbage("isrunning"))
  note(3, collectgarbage("count"))
  note(4, collectgarbage("count"))
  note(5, rawlen(w))
  text(1, type(rawget(w, 2)))
  note(6, collectgarbage("restart"))
  note(7, collectgarbage("restart"))
  bit(3, collectgarbage("isrunning"))
  note(8, rawlen(w))
  text(2, type(rawget(w, 2)))
  collectgarbage("collect")
  bit(4, collectgarbage("isrunning"))
  note(9, rawlen(w))
  text(3, type(rawget(w, 2)))
  note(10, same(t, rawget(w, 1)))
  return 1
end
witness()
return 1
"#,
        None,
    );
    assert_eq!(ran.stop, Stop::Halted, "{ran:?}");
    assert_eq!(ran.stack, [Value::Float(1.0)]);
    assert!(
        bit_value(&ran.trace, 1.0),
        "a fresh execution starts running"
    );
    assert_eq!(note_value(&ran.trace, 1.0), 0.0);
    assert_eq!(note_value(&ran.trace, 2.0), 0.0, "stop is idempotent");
    assert!(!bit_value(&ran.trace, 2.0));
    let count = note_value(&ran.trace, 3.0);
    assert!(
        count.is_finite() && count > 0.0,
        "count is the logical meter: {count}"
    );
    assert_eq!(count, note_value(&ran.trace, 4.0), "stop does not allocate");
    assert_eq!(note_value(&ran.trace, 5.0), 2.0);
    assert_eq!(text_value(&ran.trace, 1.0), "table");
    assert_eq!(note_value(&ran.trace, 6.0), 0.0);
    assert_eq!(note_value(&ran.trace, 7.0), 0.0, "restart is idempotent");
    assert!(bit_value(&ran.trace, 3.0));
    assert_eq!(note_value(&ran.trace, 8.0), 2.0);
    assert_eq!(
        text_value(&ran.trace, 2.0),
        "table",
        "restart does not collect"
    );
    assert!(bit_value(&ran.trace, 4.0));
    assert_eq!(note_value(&ran.trace, 9.0), 1.0);
    assert_eq!(text_value(&ran.trace, 3.0), "nil");
    assert_eq!(note_value(&ran.trace, 10.0), 1.0, "the strong value stays");
    assert!(
        ran.trace.steps.is_empty(),
        "control does not run the private step host"
    );
    assert_eq!(ran.collects, 1, "only the final explicit collect completes");
    assert_eq!(ran.collections, 1);
    assert!(!ran.tail);
}

#[test]
fn explicit_collect_from_stop_restores_running_and_clears_weak() {
    let ran = run_control(
        r#"
local function witness()
  local w = {}
  local mt = {}
  local t = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = t
  w[2] = {}
  peek(t)
  bit(1, collectgarbage("isrunning"))
  note(1, collectgarbage("stop"))
  bit(2, collectgarbage("isrunning"))
  text(1, type(rawget(w, 2)))
  collectgarbage("collect")
  bit(3, collectgarbage("isrunning"))
  text(2, type(rawget(w, 2)))
  note(2, same(t, rawget(w, 1)))
  note(3, rawlen(w))
  return 1
end
witness()
return 1
"#,
        None,
    );
    assert_eq!(ran.stop, Stop::Halted, "{ran:?}");
    assert!(bit_value(&ran.trace, 1.0));
    assert_eq!(note_value(&ran.trace, 1.0), 0.0);
    assert!(!bit_value(&ran.trace, 2.0));
    assert_eq!(text_value(&ran.trace, 1.0), "table");
    assert!(
        bit_value(&ran.trace, 3.0),
        "explicit collect restores running"
    );
    assert_eq!(text_value(&ran.trace, 2.0), "nil");
    assert_eq!(note_value(&ran.trace, 2.0), 1.0);
    assert_eq!(note_value(&ran.trace, 3.0), 1.0);
    assert_eq!(ran.collects, 1);
    assert_eq!(ran.collections, 1);
}

#[test]
fn stopped_guest_step_advances_and_restores_even_when_false() {
    let ran = run_control(
        r#"
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
  note(1, collectgarbage("stop"))
  bit(1, collectgarbage("isrunning"))
  bit(2, collectgarbage("step"))
  bit(3, collectgarbage("isrunning"))
  note(11, rawlen(w))
  bit(4, collectgarbage("step"))
  note(12, rawlen(w))
  bit(5, collectgarbage("step"))
  bit(6, collectgarbage("step"))
  note(14, rawlen(w))
  text(2, type(rawget(w, 2)))
  text(3, type(rawget(w, 3)))
  note(20, same(t, rawget(w, 1)))
  return 1
end
witness()
return 1
"#,
        Some(&[1, 10_000, 1, 10_000]),
    );
    assert_eq!(ran.stop, Stop::Halted, "{ran:?}");
    assert_eq!(note_value(&ran.trace, 1.0), 0.0);
    assert!(!bit_value(&ran.trace, 1.0));
    assert!(
        !bit_value(&ran.trace, 2.0),
        "the small budget does not complete"
    );
    assert!(
        bit_value(&ran.trace, 3.0),
        "a false step still restores running"
    );
    assert!(!bit_value(&ran.trace, 4.0));
    assert!(!bit_value(&ran.trace, 5.0));
    assert!(bit_value(&ran.trace, 6.0));
    assert_eq!(
        [
            cycle_at(&ran.trace, 2.0),
            cycle_at(&ran.trace, 4.0),
            cycle_at(&ran.trace, 5.0),
            cycle_at(&ran.trace, 6.0),
        ],
        [(false, 0), (true, 0), (true, 0), (false, 1)]
    );
    assert_eq!(note_value(&ran.trace, 11.0), 3.0);
    assert_eq!(
        note_value(&ran.trace, 12.0),
        1.0,
        "weak clear is visible on the tail"
    );
    assert_eq!(note_value(&ran.trace, 14.0), 1.0);
    assert_eq!(text_value(&ran.trace, 2.0), "nil");
    assert_eq!(text_value(&ran.trace, 3.0), "nil");
    assert_eq!(note_value(&ran.trace, 20.0), 1.0);
    assert!(ran.trace.steps.is_empty());
    assert_eq!(ran.collects, 1);
    assert_eq!(ran.collections, 1);
}

#[test]
fn stop_during_published_tail_keeps_the_cycle_and_budget() {
    let ran = run_control(
        r#"
local function witness()
  local w = {}
  local mt = {}
  local held = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = held
  w[2] = {}
  peek(held)
  bit(1, collectgarbage("step"))
  note(1, collectgarbage("stop"))
  bit(2, collectgarbage("isrunning"))
  note(2, collectgarbage("restart"))
  bit(3, collectgarbage("isrunning"))
  bit(4, collectgarbage("step"))
  bit(5, collectgarbage("step"))
  note(20, same(held, rawget(w, 1)))
  return 1
end
witness()
return 1
"#,
        Some(&[10_000, 1, 10_000]),
    );
    assert_eq!(ran.stop, Stop::Halted, "{ran:?}");
    assert!(!bit_value(&ran.trace, 1.0));
    assert_eq!(cycle_at(&ran.trace, 1.0), (true, 0));
    assert_eq!(note_value(&ran.trace, 1.0), 0.0);
    assert!(!bit_value(&ran.trace, 2.0));
    assert_eq!(
        cycle_at(&ran.trace, 2.0),
        (true, 0),
        "stop does not open a cycle"
    );
    assert_eq!(note_value(&ran.trace, 2.0), 0.0);
    assert!(bit_value(&ran.trace, 3.0));
    assert_eq!(
        cycle_at(&ran.trace, 3.0),
        (true, 0),
        "restart does not open a cycle"
    );
    assert!(
        !bit_value(&ran.trace, 4.0),
        "the saved unit budget still pauses"
    );
    assert_eq!(cycle_at(&ran.trace, 4.0), (true, 0));
    assert!(bit_value(&ran.trace, 5.0));
    assert_eq!(cycle_at(&ran.trace, 5.0), (false, 1));
    assert_eq!(note_value(&ran.trace, 20.0), 1.0);
    assert_eq!(ran.collects, 1);
    assert_eq!(ran.collections, 1);
    assert!(!ran.tail);
}

#[test]
fn run_control_does_not_cross_vms_or_a_reset_execution() {
    let stopped = run_control(CONTROL_SWITCH, None);
    assert_eq!(stopped.stop, Stop::Halted, "{stopped:?}");
    assert!(bit_value(&stopped.trace, 1.0));
    assert!(!bit_value(&stopped.trace, 2.0));
    let fresh = run_control(CONTROL_SWITCH, None);
    assert_eq!(fresh.stop, Stop::Halted, "{fresh:?}");
    assert!(
        bit_value(&fresh.trace, 1.0),
        "the shared catalog must not carry the stopped switch into another VM"
    );
    assert!(!bit_value(&fresh.trace, 2.0));

    let mut vm = bind_control(CONTROL_SWITCH);
    assert!(matches!(vm.run(), Ok(VmStatus::Halted)));
    assert_eq!(control_bits(&mut vm), [(1.0, true), (2.0, false)]);
    finish_reset(&mut vm);
    assert!(
        vm.host_context().scope_state::<Trace>().is_none(),
        "reset publishes a new execution scope"
    );
    assert!(matches!(vm.run(), Ok(VmStatus::Halted)));
    assert_eq!(
        control_bits(&mut vm),
        [(1.0, true), (2.0, false)],
        "a new execution on the reused VM starts running"
    );
}

#[test]
fn ordinary_catalog_and_shadowed_name_reject_run_control() {
    let witness = |body: &str| {
        format!("local function witness()\n  {body}\n  return 1\nend\nwitness()\nreturn 1\n")
    };
    for option in ["stop", "restart", "isrunning"] {
        let source = witness(&format!("collectgarbage(\"{option}\")"));
        candidate::lower(&source, catalog(false)).expect_err("ordinary catalog keeps the refusal");
        candidate::lower(&source, catalog(true)).expect_err("step opt-in is not run control");
    }
    candidate::lower(&witness("collectgarbage(\"collect\")"), catalog(false))
        .expect("ordinary collect still lowers");
    candidate::lower(&witness("collectgarbage(\"count\")"), catalog(false))
        .expect("ordinary count still lowers");
    let dynamic = witness("local opt = \"stop\"\n  collectgarbage(opt)");
    candidate::lower(&dynamic, catalog_for(true, true))
        .expect_err("a dynamic option is not this slice");
    let extra = witness("collectgarbage(\"stop\", 0)");
    candidate::lower(&extra, catalog_for(true, true)).expect_err("extra data is not this slice");
    let shadowed = r#"
local collectgarbage = 1
local function witness()
  return collectgarbage("stop")
end
witness()
return 1
"#;
    candidate::lower(shadowed, catalog_for(true, true))
        .expect_err("an upvalue shadows the builtin");
    for option in ["restart", "isrunning"] {
        let source = format!(
            "local collectgarbage = 1\nlocal function witness()\n  return collectgarbage(\"{option}\")\nend\nwitness()\nreturn 1\n"
        );
        candidate::lower(&source, catalog_for(true, true)).expect_err(option);
    }
}

#[test]
fn stop_does_not_cancel_a_host_supplied_node_quota() {
    let ran = drive(
        r#"
local function witness()
  local a = {}
  peek(a)
  note(1, collectgarbage("stop"))
  bit(1, collectgarbage("isrunning"))
  local b = {}
  peek(b)
  return 1
end
witness()
return 1
"#,
        None,
        true,
        Some(1),
    );
    assert!(
        matches!(ran.stop, Stop::HostError(ref message) if message.contains("lua heap node capacity")),
        "the installed node cap still refuses: {:?}",
        ran.stop
    );
    assert_eq!(note_value(&ran.trace, 1.0), 0.0);
    assert!(!bit_value(&ran.trace, 1.0));
    assert_eq!(
        ran.live_nodes, 1,
        "the refused table did not occupy another node"
    );
    assert_eq!(ran.collects, 0);
    assert_eq!(ran.collections, 0);
}

#[test]
fn refused_progress_does_not_fake_completion_or_skip_the_gate() {
    let sweep = run_control(
        r#"
local function witness()
  local w = {}
  local mt = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = {}
  w[2] = {}
  peek(rawget(w, 1))
  bit(1, collectgarbage("step"))
  note(1, collectgarbage("stop"))
  bit(2, collectgarbage("isrunning"))
  collectgarbage("collect")
  return 1
end
witness()
return 1
"#,
        Some(&[10_000]),
    );
    assert!(
        matches!(sweep.stop, Stop::HostError(ref message) if message.contains("lua sweep is still pending")),
        "collect must keep the published-tail gate: {:?}",
        sweep.stop
    );
    assert!(!bit_value(&sweep.trace, 1.0));
    assert_eq!(cycle_at(&sweep.trace, 1.0), (true, 0));
    assert!(!bit_value(&sweep.trace, 2.0));
    assert_eq!(cycle_at(&sweep.trace, 2.0), (true, 0));
    assert!(sweep.tail, "the refusal must leave the original tail");
    assert_eq!(sweep.collects, 0);
    assert_eq!(sweep.collections, 0);

    let missing = run_control(
        r#"
local function witness()
  local t = {}
  peek(t)
  note(1, collectgarbage("stop"))
  bit(1, collectgarbage("isrunning"))
  collectgarbage("step")
  return 1
end
witness()
return 1
"#,
        Some(&[]),
    );
    assert!(
        matches!(missing.stop, Stop::HostError(ref message) if message.contains("missing source step fixture budget")),
        "a missing budget is a real refusal: {:?}",
        missing.stop
    );
    assert!(!bit_value(&missing.trace, 1.0));
    assert_eq!(missing.collects, 0);
    assert_eq!(missing.collections, 0);
    assert!(!missing.tail);
}

/// Four weak groups publish together through real `collectgarbage("step")`.
///
/// Upstream control: `evidence/lua52-gc-step/mixed-weak-clear-visibility.{lua,json}`.
/// The lowerable subset has no loop and no `~=`, so a weak-key walk is two
/// `next` calls inside a `do`. That block ends before the next step, which
/// drops the cursors from the gather list; the test must not keep a doomed
/// key or value live in order to count it. Weak values are read only as
/// `type(rawget(...))`. `bit`, `text`, and `note` record those Lua results
/// across the existing host boundary. They do not build the graph or choose
/// the step outcome. The budget queue is the fixture `collectgarbage("step")`
/// consumes, not a LuaJIT step index.
#[test]
fn guest_mixed_weak_step_publishes_four_groups_together() {
    for shared in [false, true] {
        let step = run_mixed(shared, MixedArm::Step);
        assert_mixed_step(&step, shared);
        let full = run_mixed(shared, MixedArm::Full);
        assert_mixed_settled(&full, shared, MixedArm::Full, [0, 0, 0, 0], 1);
        let omitted = run_mixed(shared, MixedArm::Omit);
        assert_mixed_settled(&omitted, shared, MixedArm::Omit, [1, 1, 1, 1], 0);
        let partial = run_mixed(shared, MixedArm::Partial);
        assert_mixed_settled(&partial, shared, MixedArm::Partial, [1, 0, 1, 1], 0);
    }
}

#[derive(Clone, Copy, Debug)]
enum MixedArm {
    Step,
    Full,
    Omit,
    Partial,
}

fn run_mixed(shared: bool, arm: MixedArm) -> Ran {
    let budgets: Option<&[u64]> = match arm {
        MixedArm::Step => Some(&[1, 10_000, 1, 10_000]),
        MixedArm::Full | MixedArm::Omit | MixedArm::Partial => None,
    };
    let source = mixed_source(shared, arm);
    let ran = run_with_step_budgets(&source, budgets);
    assert_eq!(
        ran.stop,
        Stop::Halted,
        "shared={shared} {arm:?} did not halt\n{source}"
    );
    assert_eq!(ran.stack, [Value::Float(1.0)], "shared={shared} {arm:?}");
    assert!(
        ran.trace.steps.is_empty(),
        "shared={shared} {arm:?} used the private step host"
    );
    assert!(!ran.tail, "shared={shared} {arm:?} left a sweep tail");
    assert!(ran.frame.is_none(), "shared={shared} {arm:?} left a frame");
    ran
}

fn assert_mixed_settled(
    ran: &Ran,
    shared: bool,
    arm: MixedArm,
    expected: [u32; 4],
    collects: usize,
) {
    let counts = mixed_counts(&ran.trace, 0);
    assert_eq!(
        counts, expected,
        "shared={shared} {arm:?} group counts, not a combined total"
    );
    assert!(
        mixed_steps(&ran.trace).is_empty(),
        "shared={shared} {arm:?} stepped"
    );
    assert_eq!(ran.collects, collects, "shared={shared} {arm:?} reports");
    assert_eq!(
        ran.collections, collects as u32,
        "shared={shared} {arm:?} heap completions"
    );
}

fn assert_mixed_step(ran: &Ran, shared: bool) {
    let points: Vec<[u32; 4]> = (0..=4)
        .map(|point| mixed_counts(&ran.trace, point))
        .collect();
    let steps = mixed_steps(&ran.trace);
    assert_eq!(
        points[0],
        [1, 1, 1, 1],
        "shared={shared} groups were not all present before the first step: {points:?}"
    );
    let mut cleared = false;
    for counts in &points {
        assert!(
            *counts == [1, 1, 1, 1] || *counts == [0, 0, 0, 0],
            "shared={shared} mixed weak snapshot {counts:?} in {points:?}"
        );
        if cleared {
            assert_eq!(
                *counts,
                [0, 0, 0, 0],
                "shared={shared} resurrected {points:?}"
            );
        }
        cleared = *counts == [0, 0, 0, 0];
    }
    assert_eq!(steps.len(), 4, "shared={shared} steps {steps:?}");
    let trues: Vec<_> = steps
        .iter()
        .enumerate()
        .filter(|(_, (bit, _, _))| *bit)
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        trues,
        vec![3],
        "shared={shared} expected one final true, steps={steps:?} points={points:?}"
    );
    let mut saw_incomplete_clear = false;
    for (index, (bit, tail, collections)) in steps.iter().enumerate() {
        let after = points[index + 1];
        if !*bit && after == [0, 0, 0, 0] {
            assert_eq!(
                (*tail, *collections),
                (true, 0),
                "shared={shared} cleared while a step was still incomplete"
            );
            saw_incomplete_clear = true;
        }
        if *bit {
            assert_eq!(after, [0, 0, 0, 0], "shared={shared} true before clear");
            assert_eq!(*collections, 1, "shared={shared} true without a report");
            assert!(!*tail, "shared={shared} true left a tail");
        }
    }
    assert!(
        saw_incomplete_clear,
        "shared={shared} never showed Bool false with all four groups clear: points={points:?} steps={steps:?}"
    );
    assert_eq!(ran.collects, 1, "shared={shared} completion reports");
    assert_eq!(ran.collections, 1, "shared={shared} heap completions");
}

fn mixed_counts(trace: &Trace, point: u32) -> [u32; 4] {
    [
        mixed_key_count(trace, point, 1, "k"),
        mixed_value_count(trace, point, 2, "v"),
        mixed_key_count(trace, point, 3, "kv-key"),
        mixed_value_count(trace, point, 4, "kv-value"),
    ]
}

fn mixed_tag(point: u32, group: u32, signal: u32) -> f64 {
    f64::from(1000 + point * 100 + group * 10 + signal)
}

fn mixed_bit(trace: &Trace, tag: f64) -> bool {
    trace
        .bits
        .iter()
        .find(|(found, _)| *found == tag)
        .unwrap_or_else(|| panic!("missing mixed bit {tag}"))
        .1
}

fn mixed_key_count(trace: &Trace, point: u32, group: u32, label: &str) -> u32 {
    let tag = |signal| mixed_tag(point, group, signal);
    let type_a = text_value(trace, tag(1));
    let type_b = text_value(trace, tag(2));
    let a_keep = mixed_bit(trace, tag(1));
    let b_keep = mixed_bit(trace, tag(2));
    let av_value = mixed_bit(trace, tag(3));
    let bv_value = mixed_bit(trace, tag(4));
    let b_nil = mixed_bit(trace, tag(5));
    let bv_nil = mixed_bit(trace, tag(6));
    assert_eq!(
        note_value(trace, tag(1)),
        1.0,
        "{label} strong identity at point {point}"
    );
    match (
        type_a, type_b, a_keep, b_keep, av_value, bv_value, b_nil, bv_nil,
    ) {
        ("table", "table", true, false, true, true, false, false)
        | ("table", "table", false, true, true, true, false, false) => 1,
        ("table", "nil", true, false, true, false, true, true) => 0,
        other => panic!("{label} unexpected weak-key observation at point {point}: {other:?}"),
    }
}

fn mixed_value_count(trace: &Trace, point: u32, group: u32, label: &str) -> u32 {
    let tag = |signal| mixed_tag(point, group, signal);
    assert_eq!(
        note_value(trace, tag(1)),
        1.0,
        "{label} strong identity at point {point}"
    );
    match text_value(trace, tag(1)) {
        "table" => 1,
        "nil" => 0,
        other => panic!("{label} unexpected weak-value type at point {point}: {other}"),
    }
}

fn mixed_steps(trace: &Trace) -> Vec<(bool, bool, u32)> {
    trace
        .bits
        .iter()
        .zip(&trace.bit_cycle_state)
        .filter(|((tag, _), _)| (1.0..=4.0).contains(tag))
        .map(|((_, bit), (tail, collections))| (*bit, *tail, *collections))
        .collect()
}

fn mixed_source(shared: bool, arm: MixedArm) -> String {
    let mut source = String::from(
        "local function witness()\n  return 1\nend\nwitness()\n\
         local k_owner = {}\nlocal k_key = {}\nlocal k_value = {}\n\
         local v_owner = {}\nlocal v_key = {}\nlocal v_value = {}\n\
         local kvk_owner = {}\nlocal kvk_key = {}\nlocal kvk_value = {}\n\
         local kvv_owner = {}\nlocal kvv_key = {}\nlocal kvv_value = {}\n",
    );
    if shared {
        source.push_str(
            "local mode_k = {}\nmode_k.__mode = \"k\"\n\
             local mode_v = {}\nmode_v.__mode = \"v\"\n\
             local mode_kv = {}\nmode_kv.__mode = \"kv\"\n\
             setmetatable(k_owner, mode_k)\nsetmetatable(v_owner, mode_v)\n\
             setmetatable(kvk_owner, mode_kv)\nsetmetatable(kvv_owner, mode_kv)\n",
        );
    } else {
        for (owner, mode) in [
            ("k_owner", "k"),
            ("v_owner", "v"),
            ("kvk_owner", "kv"),
            ("kvv_owner", "kv"),
        ] {
            let mt = format!("{owner}_mt");
            source.push_str(&format!(
                "local {mt} = {{}}\n{mt}.__mode = \"{mode}\"\nsetmetatable({owner}, {mt})\n"
            ));
        }
    }
    source.push_str(
        "k_owner[k_key] = k_value\nv_owner[v_key] = v_value\n\
         kvk_owner[kvk_key] = kvk_value\nkvv_owner[kvv_key] = kvv_value\n\
         do\n  local k_dead = {}\n  k_dead.self = k_dead\n  k_owner[k_dead] = k_value\nend\n\
         do\n  local v_dead = {}\n  v_dead.self = v_dead\n  v_owner.orphan = v_dead\nend\n\
         do\n  local kvk_dead = {}\n  kvk_dead.self = kvk_dead\n  kvk_owner[kvk_dead] = kvk_value\nend\n\
         do\n  local kvv_dead = {}\n  kvv_dead.self = kvv_dead\n  kvv_owner.orphan = kvv_dead\nend\n",
    );
    if matches!(arm, MixedArm::Partial) {
        source.push_str("rawset(v_owner, \"orphan\", nil)\n");
    }
    if matches!(arm, MixedArm::Full) {
        source.push_str("collectgarbage(\"collect\")\n");
    }
    let points: &[u32] = if matches!(arm, MixedArm::Step) {
        &[0, 1, 2, 3, 4]
    } else {
        &[0]
    };
    for point in points {
        if *point > 0 {
            source.push_str(&format!("bit({point}, collectgarbage(\"step\"))\n"));
        }
        push_key_observe(&mut source, *point, 1, "k_owner", "k_key", "k_value");
        push_value_observe(&mut source, *point, 2, "v_owner", "v_key", "v_value");
        push_key_observe(&mut source, *point, 3, "kvk_owner", "kvk_key", "kvk_value");
        push_value_observe(&mut source, *point, 4, "kvv_owner", "kvv_key", "kvv_value");
    }
    source.push_str("return 1\n");
    source
}

fn push_key_observe(
    source: &mut String,
    point: u32,
    group: u32,
    owner: &str,
    key: &str,
    value: &str,
) {
    let tag = |signal| 1000 + point * 100 + group * 10 + signal;
    let prefix = format!("p{point}g{group}");
    source.push_str(&format!(
        "do\n  local {prefix}a, {prefix}av = next({owner}, nil)\n  \
         local {prefix}b, {prefix}bv = next({owner}, {prefix}a)\n  \
         text({t1}, type({prefix}a))\n  text({t2}, type({prefix}b))\n  \
         bit({b1}, rawequal({prefix}a, {key}))\n  bit({b2}, rawequal({prefix}b, {key}))\n  \
         bit({b3}, rawequal({prefix}av, {value}))\n  bit({b4}, rawequal({prefix}bv, {value}))\n  \
         bit({b5}, rawequal({prefix}b, nil))\n  bit({b6}, rawequal({prefix}bv, nil))\n  \
         note({n1}, same({value}, rawget({owner}, {key})))\nend\n",
        t1 = tag(1),
        t2 = tag(2),
        b1 = tag(1),
        b2 = tag(2),
        b3 = tag(3),
        b4 = tag(4),
        b5 = tag(5),
        b6 = tag(6),
        n1 = tag(1),
    ));
}

fn push_value_observe(
    source: &mut String,
    point: u32,
    group: u32,
    owner: &str,
    key: &str,
    value: &str,
) {
    let tag = 1000 + point * 100 + group * 10 + 1;
    source.push_str(&format!(
        "text({tag}, type(rawget({owner}, \"orphan\")))\n\
         note({tag}, same({value}, rawget({owner}, {key})))\n"
    ));
}

/// Stop, then a variable zero, enters a real partial step on the combined
/// catalog. Bool false restores running and does not complete. A later literal
/// `0` is the static default on that same catalog; the weak clear completes once.
#[test]
fn stopped_runtime_zero_steps_partial_then_completes_once() {
    let variable = "local zero = 0\nreturn collectgarbage(\"step\", zero)\n";
    let stop = "return collectgarbage(\"stop\")\n";
    let ordinary = candidate::lower(variable, catalog(false))
        .expect_err("ordinary catalog accepted runtime step data");
    assert!(ordinary.message.contains("not lowered yet"), "{ordinary}");
    let ordinary_stop =
        candidate::lower(stop, catalog(false)).expect_err("ordinary catalog accepted stop");
    assert!(
        ordinary_stop.message.contains("not lowered yet"),
        "{ordinary_stop}"
    );
    candidate::lower(variable, catalog(true)).expect("step catalog rejected runtime zero");
    let step_stop =
        candidate::lower(stop, catalog(true)).expect_err("step-only catalog accepted stop");
    assert!(step_stop.message.contains("not lowered yet"), "{step_stop}");
    candidate::lower(variable, catalog_for(true, true))
        .expect("combined catalog rejected runtime zero");
    candidate::lower(stop, catalog_for(true, true)).expect("combined catalog rejected stop");

    let ran = run_control(
        r#"
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
  local zero = 0
  note(1, collectgarbage("stop"))
  bit(1, collectgarbage("isrunning"))
  bit(2, collectgarbage("step", zero))
  bit(3, collectgarbage("isrunning"))
  note(11, rawlen(w))
  bit(4, collectgarbage("step", 0))
  note(12, rawlen(w))
  bit(5, collectgarbage("step", zero))
  bit(6, collectgarbage("step", 0))
  note(14, rawlen(w))
  text(2, type(rawget(w, 2)))
  text(3, type(rawget(w, 3)))
  note(20, same(t, rawget(w, 1)))
  return 1
end
witness()
return 1
"#,
        Some(&[1, 10_000, 1, 10_000]),
    );
    assert_eq!(ran.stop, Stop::Halted, "{ran:?}");
    assert_eq!(note_value(&ran.trace, 1.0), 0.0);
    assert!(!bit_value(&ran.trace, 1.0), "stop is observable");
    assert!(
        !bit_value(&ran.trace, 2.0),
        "variable zero returns the real partial Bool false"
    );
    assert_eq!(
        cycle_at(&ran.trace, 2.0),
        (false, 0),
        "the partial step does not publish a tail or a completion"
    );
    assert!(
        bit_value(&ran.trace, 3.0),
        "a false runtime step still restores running"
    );
    assert!(!bit_value(&ran.trace, 4.0));
    assert!(!bit_value(&ran.trace, 5.0));
    assert!(bit_value(&ran.trace, 6.0), "the cycle completes once");
    assert_eq!(
        [
            cycle_at(&ran.trace, 4.0),
            cycle_at(&ran.trace, 5.0),
            cycle_at(&ran.trace, 6.0),
        ],
        [(true, 0), (true, 0), (false, 1)]
    );
    assert_eq!(note_value(&ran.trace, 11.0), 3.0);
    assert_eq!(
        note_value(&ran.trace, 12.0),
        1.0,
        "the independent weak items clear on the published tail"
    );
    assert_eq!(note_value(&ran.trace, 14.0), 1.0);
    assert_eq!(text_value(&ran.trace, 2.0), "nil");
    assert_eq!(text_value(&ran.trace, 3.0), "nil");
    assert_eq!(note_value(&ran.trace, 20.0), 1.0);
    assert!(
        ran.running,
        "the successful completion leaves the switch running"
    );
    assert!(ran.budgets.is_empty(), "{:?}", ran.budgets);
    assert_eq!(ran.collects, 1);
    assert_eq!(ran.collections, 1);
    assert!(!ran.tail);
}

/// A published tail, then stop, then an evaluated non-default. The category
/// host refuses before the next budget. Valid data still reads that tail and
/// does not gather; without a tail the same data gathers and refuses.
#[test]
fn stopped_tail_rejects_evaluated_data_before_next_budget() {
    let queued = [10_000_u64, 1, 10_000];
    let refused = run_control(
        r#"
local function witness()
  local w = {}
  local mt = {}
  local held = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = held
  w[2] = {}
  peek(held)
  bit(1, collectgarbage("step"))
  note(1, collectgarbage("stop"))
  bit(2, collectgarbage("isrunning"))
  local unissued = external_shape()
  collectgarbage("step", note(8, 1))
  return 1
end
witness()
return 1
"#,
        Some(&queued),
    );
    assert!(
        matches!(refused.stop, Stop::HostError(ref message) if message == "step data is not the default"),
        "category refusal, not lowering: {refused:?}"
    );
    assert_eq!(
        refused.trace.notes,
        [(1.0, 0.0), (8.0, 1.0)],
        "stop and the rejected data expression both ran"
    );
    assert!(!bit_value(&refused.trace, 1.0));
    assert_eq!(
        cycle_at(&refused.trace, 1.0),
        (true, 0),
        "the refusal starts from a published tail"
    );
    assert!(
        !bit_value(&refused.trace, 2.0),
        "stop was visible before the refusal"
    );
    assert!(!refused.running, "the refusal does not restore running");
    assert!(refused.tail, "the published tail remains");
    assert_eq!(refused.collections, 0);
    assert_eq!(refused.collects, 0);
    assert_eq!(
        refused.budgets,
        [1, 10_000],
        "the next budget segment was not consumed: {:?}",
        refused.budgets
    );
    assert_eq!(refused.trace.bits.len(), 2, "{refused:?}");

    let continued = run_control(
        r#"
local function witness()
  local w = {}
  local mt = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = {}
  w[2] = {}
  bit(1, collectgarbage("step"))
  local unissued = external_shape()
  local zero = 0
  bit(2, collectgarbage("step", zero))
  return 1
end
witness()
return 1
"#,
        Some(&[10_000, 1]),
    );
    assert_eq!(continued.stop, Stop::Halted, "{continued:?}");
    assert!(!bit_value(&continued.trace, 1.0));
    assert_eq!(cycle_at(&continued.trace, 1.0), (true, 0));
    assert!(
        !bit_value(&continued.trace, 2.0),
        "valid runtime zero continues the saved tail"
    );
    assert_eq!(cycle_at(&continued.trace, 2.0), (true, 0));
    assert!(continued.tail, "the unit budget still leaves the tail");
    assert!(continued.running);
    assert!(continued.budgets.is_empty(), "{:?}", continued.budgets);
    assert_eq!(continued.collects, 0);
    assert_eq!(continued.collections, 0);

    let gathered = run_control(
        r#"
local function witness()
  local unissued = external_shape()
  local zero = 0
  bit(1, collectgarbage("step", zero))
  return 1
end
witness()
return 1
"#,
        Some(&[1, 9]),
    );
    assert!(
        matches!(
            gathered.stop,
            Stop::HostError(ref message) if message.contains("invalid private Lua root candidate")
        ),
        "no tail must gather, not accept the data and step: {gathered:?}"
    );
    assert_eq!(gathered.budgets, [1, 9], "gather fails before a budget pop");
    assert!(!gathered.tail);
    assert_eq!(gathered.collects, 0);
    assert_eq!(gathered.collections, 0);
    assert!(gathered.trace.bits.is_empty(), "{gathered:?}");
}

/// Ordinary catalog has no automatic table safepoint. An empty local after a
/// weak witness must not collect.
#[test]
fn ordinary_catalog_empty_local_does_not_collect_a_weak_witness() {
    let ran = run(r#"
local function witness()
  local w = {}
  local mt = {}
  local held = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = held
  w[2] = {}
  peek(held)
  local fresh = {}
  peek(fresh)
  text(1, type(fresh))
  text(2, type(rawget(w, 2)))
  note(1, same(held, rawget(w, 1)))
  note(2, rawlen(w))
  return 1
end
witness()
return 1
"#);
    assert_eq!(ran.stop, Stop::Halted, "{ran:?}");
    assert_eq!(ran.stack, [Value::Float(1.0)]);
    assert_eq!(text_value(&ran.trace, 1.0), "table");
    assert_eq!(text_value(&ran.trace, 2.0), "table");
    assert_eq!(note_value(&ran.trace, 1.0), 1.0);
    assert_eq!(note_value(&ran.trace, 2.0), 2.0);
    assert_eq!(ran.trace.peeks.len(), 2);
    assert_ne!(ran.trace.peeks[0].table, ran.trace.peeks[1].table);
    assert_eq!(ran.collects, 0);
    assert_eq!(ran.collections, 0);
    assert!(!ran.tail);
    assert!(ran.trace.steps.is_empty());
}

/// Stop still suppresses collection. The following empty local is ordinary
/// allocation, not a safepoint.
#[test]
fn stopped_empty_local_keeps_the_weak_witness() {
    let ran = run_control(
        r#"
local function witness()
  local w = {}
  local mt = {}
  local held = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = held
  w[2] = {}
  peek(held)
  note(1, collectgarbage("stop"))
  bit(1, collectgarbage("isrunning"))
  text(1, type(rawget(w, 2)))
  local fresh = {}
  peek(fresh)
  bit(2, collectgarbage("isrunning"))
  text(2, type(fresh))
  text(3, type(rawget(w, 2)))
  note(2, same(held, rawget(w, 1)))
  note(3, rawlen(w))
  return 1
end
witness()
return 1
"#,
        None,
    );
    assert_eq!(ran.stop, Stop::Halted, "{ran:?}");
    assert_eq!(note_value(&ran.trace, 1.0), 0.0);
    assert!(!bit_value(&ran.trace, 1.0));
    assert_eq!(text_value(&ran.trace, 1.0), "table");
    assert!(!bit_value(&ran.trace, 2.0));
    assert_eq!(text_value(&ran.trace, 2.0), "table");
    assert_eq!(
        text_value(&ran.trace, 3.0),
        "table",
        "stop must keep the weak item across the empty local"
    );
    assert_eq!(note_value(&ran.trace, 2.0), 1.0);
    assert_eq!(note_value(&ran.trace, 3.0), 2.0);
    assert_eq!(ran.trace.peeks.len(), 2);
    assert_eq!(ran.collects, 0);
    assert_eq!(ran.collections, 0);
    assert!(!ran.tail);
    assert!(ran.subject_alive.unwrap_or(false));
}

/// Same refusal as the existing node cap: stop must not collect to make room.
#[test]
fn stopped_empty_local_keeps_the_installed_node_quota_refusal() {
    let ran = drive(
        r#"
local function witness()
  local a = {}
  peek(a)
  note(1, collectgarbage("stop"))
  bit(1, collectgarbage("isrunning"))
  local b = {}
  peek(b)
  return 1
end
witness()
return 1
"#,
        None,
        true,
        Some(1),
    );
    assert!(
        matches!(ran.stop, Stop::HostError(ref message) if message.contains("lua heap node capacity")),
        "stop must not change the installed node refusal: {:?}",
        ran.stop
    );
    assert_eq!(note_value(&ran.trace, 1.0), 0.0);
    assert!(!bit_value(&ran.trace, 1.0));
    assert_eq!(ran.live_nodes, 1, "the refused table did not occupy a node");
    assert_eq!(
        ran.trace.peeks.len(),
        1,
        "the second table was not published"
    );
    assert_eq!(ran.collects, 0);
    assert_eq!(ran.collections, 0);
    assert!(!ran.tail);
    assert!(ran.subject_alive.unwrap_or(false));
    assert!(ran.token_open.unwrap_or(false));
}

/// The run-control catalog does not grow this automatic safepoint.
#[test]
fn run_control_empty_local_keeps_the_weak_witness() {
    let ran = run_control(
        r#"
local function witness()
  local w = {}
  local mt = {}
  local held = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = held
  w[2] = {}
  peek(held)
  note(1, collectgarbage("stop"))
  note(2, collectgarbage("restart"))
  text(1, type(rawget(w, 2)))
  local fresh = {}
  peek(fresh)
  text(2, type(fresh))
  text(3, type(rawget(w, 2)))
  note(3, same(held, rawget(w, 1)))
  return 1
end
witness()
return 1
"#,
        Some(&[10_000, 10_000]),
    );
    assert_eq!(ran.stop, Stop::Halted, "{ran:?}");
    assert_eq!(text_value(&ran.trace, 1.0), "table");
    assert_eq!(text_value(&ran.trace, 2.0), "table");
    assert_eq!(
        text_value(&ran.trace, 3.0),
        "table",
        "run control without the empty-local opt-in must keep the weak item"
    );
    assert_eq!(note_value(&ran.trace, 3.0), 1.0);
    assert_eq!(ran.collects, 0);
    assert_eq!(ran.collections, 0);
    assert!(!ran.tail);
    assert!(ran.trace.steps.is_empty());
}

/// The opt-in schema does not collect or widen the node cap while stopped.
#[test]
fn stopped_auto_table_keeps_the_installed_node_quota_refusal() {
    let ran = drive_plugin(
        r#"
local function witness()
  local a = {}
  peek(a)
  note(1, collectgarbage("stop"))
  bit(1, collectgarbage("isrunning"))
  local b = {}
  peek(b)
  return 1
end
witness()
return 1
"#,
        None,
        &AUTO_TABLE_PLUGIN,
        Some(1),
    );
    assert!(
        matches!(ran.stop, Stop::HostError(ref message) if message.contains("lua heap node capacity")),
        "stopped opt-in must keep the installed node refusal: {:?}",
        ran.stop
    );
    assert_eq!(note_value(&ran.trace, 1.0), 0.0);
    assert!(!bit_value(&ran.trace, 1.0));
    assert_eq!(ran.live_nodes, 1);
    assert_eq!(ran.trace.peeks.len(), 1);
    assert_eq!(ran.collects, 0);
    assert_eq!(ran.collections, 0);
    assert!(!ran.tail);
}

/// Restart and a pure read keep the weak witness. With the empty-local opt-in
/// and two configured host budgets, the next empty local finishes one idle
/// cycle before publish.
#[test]
fn restarted_empty_local_clears_old_weak_before_the_new_table_is_published() {
    let ran = run_auto_table(
        r#"
local function witness()
  local w = {}
  local mt = {}
  local held = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = held
  w[2] = {}
  peek(held)
  bit(1, collectgarbage("isrunning"))
  note(1, collectgarbage("stop"))
  bit(2, collectgarbage("isrunning"))
  text(1, type(rawget(w, 2)))
  note(2, rawlen(w))
  note(3, same(held, rawget(w, 1)))
  note(11, collectgarbage("count"))
  note(4, collectgarbage("restart"))
  bit(3, collectgarbage("isrunning"))
  note(12, collectgarbage("count"))
  text(2, type(rawget(w, 2)))
  note(5, rawlen(w))
  note(6, same(held, rawget(w, 1)))
  bit(4, collectgarbage("isrunning"))
  local fresh = {}
  bit(5, peek(fresh))
  text(3, type(fresh))
  text(4, type(rawget(w, 2)))
  note(7, rawlen(w))
  note(8, same(held, rawget(w, 1)))
  w[3] = {}
  text(5, type(rawget(w, 3)))
  collectgarbage("collect")
  text(6, type(rawget(w, 3)))
  note(9, same(held, rawget(w, 1)))
  return 1
end
witness()
return 1
"#,
        Some(&[10_000, 10_000]),
    );
    assert_eq!(ran.stop, Stop::Halted, "{ran:?}");
    assert_eq!(ran.stack, [Value::Float(1.0)]);
    assert!(
        bit_value(&ran.trace, 1.0),
        "a fresh execution starts running"
    );
    assert_eq!(note_value(&ran.trace, 1.0), 0.0);
    assert!(!bit_value(&ran.trace, 2.0));
    assert_eq!(text_value(&ran.trace, 1.0), "table");
    assert_eq!(note_value(&ran.trace, 2.0), 2.0);
    assert_eq!(note_value(&ran.trace, 3.0), 1.0);
    let before = note_value(&ran.trace, 11.0);
    assert!(
        before.is_finite() && before > 0.0,
        "count is the logical meter: {before}"
    );
    assert_eq!(note_value(&ran.trace, 4.0), 0.0);
    assert!(bit_value(&ran.trace, 3.0));
    assert_eq!(
        note_value(&ran.trace, 12.0),
        before,
        "restart does not allocate"
    );
    assert_eq!(
        text_value(&ran.trace, 2.0),
        "table",
        "restart and the following read must keep the weak item"
    );
    assert_eq!(note_value(&ran.trace, 5.0), 2.0);
    assert_eq!(note_value(&ran.trace, 6.0), 1.0);
    assert!(bit_value(&ran.trace, 4.0));
    assert_eq!(cycle_at(&ran.trace, 4.0), (false, 0));
    assert!(bit_value(&ran.trace, 5.0), "the new table is usable");
    assert_eq!(text_value(&ran.trace, 3.0), "table");
    assert_eq!(ran.trace.peeks.len(), 2);
    assert_ne!(ran.trace.peeks[0].table, ran.trace.peeks[1].table);
    assert_eq!(
        text_value(&ran.trace, 4.0),
        "nil",
        "the empty local must clear the old weak item before publish; got {:?}",
        ran.trace.texts
    );
    assert_eq!(note_value(&ran.trace, 7.0), 1.0);
    assert_eq!(
        note_value(&ran.trace, 8.0),
        1.0,
        "the strong root keeps its identity"
    );
    assert_eq!(
        cycle_at(&ran.trace, 5.0),
        (false, 1),
        "the safepoint must finish before the new table is published: {:?}",
        ran.trace.bit_cycle_state
    );
    assert_eq!(
        text_value(&ran.trace, 5.0),
        "table",
        "w[3] = {{}} is an assignment, not `local name = {{}}`, so it is not an automatic safepoint"
    );
    assert_eq!(text_value(&ran.trace, 6.0), "nil");
    assert_eq!(note_value(&ran.trace, 9.0), 1.0);
    assert!(
        ran.trace.steps.is_empty(),
        "lua_host::step is not this safepoint"
    );
    assert!(!ran.tail);
    assert_eq!(
        ran.collections, 2,
        "automatic cycle plus the explicit collect"
    );
    assert_eq!(ran.collects, 2, "both completions record the real report");
    assert!(ran.subject_alive.unwrap_or(false));
    assert!(ran.token_open.unwrap_or(false));
}

const LIVE_UNISSUED_PREFIX: &str = r#"
local function witness()
  local w = {}
  local mt = {}
  local held = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = held
  w[2] = {}
  peek(held)
  local unissued = external_shape()
  note(1, shape(unissued))
"#;

/// Due empty local still gathers. The saved array is usable, then GATHER_ROOT
/// rejects it. This is the positive control for the skipped-gather witness.
#[test]
fn due_empty_local_rejects_a_saved_unissued_shape_at_gather() {
    let ran = run_auto_table(
        &format!(
            "{LIVE_UNISSUED_PREFIX}
  note(2, collectgarbage(\"restart\"))
  bit(1, collectgarbage(\"isrunning\"))
  text(1, type(rawget(w, 2)))
  local fresh = {{}}
  peek(fresh)
  return 1
end
witness()
return 1
"
        ),
        Some(&[10_000, 10_000]),
    );
    assert_eq!(
        note_value(&ran.trace, 1.0),
        1.0,
        "the array was saved and read"
    );
    assert_eq!(
        note_value(&ran.trace, 2.0),
        0.0,
        "restart itself does not collect"
    );
    assert!(
        bit_value(&ran.trace, 1.0),
        "restart leaves the collector running"
    );
    assert_eq!(
        text_value(&ran.trace, 1.0),
        "table",
        "weak item before the local"
    );
    assert!(
        matches!(ran.stop, Stop::HostError(ref message) if message.contains("invalid private Lua root candidate")),
        "due gather must reject the unissued array at GATHER_ROOT, not a later host: {:?}",
        ran.stop
    );
    assert_eq!(ran.trace.peeks.len(), 1, "the new table was not published");
    assert_eq!(ran.collects, 0);
    assert_eq!(ran.collections, 0);
    assert!(!ran.tail);
}

/// Heap exists, restart has already reached the threshold, then stop. The next
/// empty local is not due. A still-live unissued array must not be gathered.
#[test]
fn stopped_reached_threshold_skips_gather_on_the_next_empty_local() {
    let ran = run_auto_table(
        &format!(
            "{LIVE_UNISSUED_PREFIX}
  note(2, collectgarbage(\"restart\"))
  note(3, collectgarbage(\"stop\"))
  bit(1, collectgarbage(\"isrunning\"))
  text(1, type(rawget(w, 2)))
  local fresh = {{}}
  peek(fresh)
  text(2, type(fresh))
  text(3, type(rawget(w, 2)))
  note(4, same(held, rawget(w, 1)))
  return 1
end
witness()
return 1
"
        ),
        Some(&[10_000, 10_000]),
    );
    assert_eq!(ran.stop, Stop::Halted, "{ran:?}");
    assert_eq!(
        note_value(&ran.trace, 1.0),
        1.0,
        "the array was saved and read"
    );
    assert_eq!(note_value(&ran.trace, 2.0), 0.0);
    assert_eq!(note_value(&ran.trace, 3.0), 0.0);
    assert!(!bit_value(&ran.trace, 1.0));
    assert_eq!(text_value(&ran.trace, 1.0), "table");
    assert_eq!(
        text_value(&ran.trace, 2.0),
        "table",
        "the empty local is ordinary allocation"
    );
    assert_eq!(
        text_value(&ran.trace, 3.0),
        "table",
        "stop after a reached threshold must not gather or collect"
    );
    assert_eq!(note_value(&ran.trace, 4.0), 1.0);
    assert_eq!(ran.trace.peeks.len(), 2);
    assert_eq!(ran.collects, 0);
    assert_eq!(ran.collections, 0);
    assert!(!ran.tail);
    assert!(ran.trace.steps.is_empty());
}

/// Full opt-in, heap present, collector still running, but restart never wrote
/// a threshold. The empty local is not due, so the live unissued array is not
/// gathered.
#[test]
fn running_without_restart_skips_gather_on_an_empty_local() {
    let ran = run_auto_table(
        &format!(
            "{LIVE_UNISSUED_PREFIX}
  text(1, type(rawget(w, 2)))
  local fresh = {{}}
  peek(fresh)
  text(2, type(fresh))
  text(3, type(rawget(w, 2)))
  note(4, same(held, rawget(w, 1)))
  return 1
end
witness()
return 1
"
        ),
        Some(&[10_000, 10_000]),
    );
    assert_eq!(ran.stop, Stop::Halted, "{ran:?}");
    assert_eq!(note_value(&ran.trace, 1.0), 1.0);
    assert_eq!(text_value(&ran.trace, 2.0), "table");
    assert_eq!(text_value(&ran.trace, 3.0), "table");
    assert_eq!(note_value(&ran.trace, 4.0), 1.0);
    assert_eq!(ran.collects, 0);
    assert_eq!(ran.collections, 0);
}

/// A direct restart-entry call while stopped must return before decode, budget,
/// latch, or a cycle. Empty budgets would fault if the dispatch gate were skipped.
#[test]
fn direct_auto_restart_while_stopped_does_not_pass_the_due_gate() {
    let ran = run_auto_table(
        r#"
local function witness()
  local w = {}
  local mt = {}
  local held = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = held
  w[2] = {}
  peek(held)
  note(1, collectgarbage("restart"))
  note(2, collectgarbage("stop"))
  note(3, direct_auto_restart())
  text(1, type(rawget(w, 2)))
  return 1
end
witness()
return 1
"#,
        Some(&[]),
    );
    assert_eq!(ran.stop, Stop::Halted, "{ran:?}");
    assert_eq!(note_value(&ran.trace, 1.0), 0.0);
    assert_eq!(note_value(&ran.trace, 2.0), 0.0);
    assert_eq!(
        note_value(&ran.trace, 3.0),
        0.0,
        "not due returns before the body"
    );
    assert_eq!(text_value(&ran.trace, 1.0), "table");
    assert_eq!(ran.collects, 0);
    assert_eq!(ran.collections, 0);
    assert!(!ran.tail);
}

fn incomplete_route_source() -> String {
    format!(
        "{LIVE_UNISSUED_PREFIX}
  note(2, collectgarbage(\"restart\"))
  bit(1, collectgarbage(\"isrunning\"))
  local fresh = {{}}
  peek(fresh)
  text(2, type(fresh))
  text(3, type(rawget(w, 2)))
  return 1
end
witness()
return 1
"
    )
}

fn assert_unrouted_empty_local(ran: &Ran) {
    assert_eq!(ran.stop, Stop::Halted, "{ran:?}");
    assert_eq!(
        note_value(&ran.trace, 1.0),
        1.0,
        "the array was saved and read"
    );
    assert!(bit_value(&ran.trace, 1.0));
    assert_eq!(text_value(&ran.trace, 2.0), "table");
    assert_eq!(
        text_value(&ran.trace, 3.0),
        "table",
        "an incomplete catalog must not gather or collect"
    );
    assert_eq!(ran.collects, 0);
    assert_eq!(ran.collections, 0);
}

#[test]
fn restart_entry_without_due_schema_does_not_connect_the_route() {
    let ran = drive_plugin(
        &incomplete_route_source(),
        Some(&[10_000, 10_000]),
        &RESTART_ENTRY_PLUGIN,
        None,
    );
    assert_unrouted_empty_local(&ran);
    assert!(
        due_branch(&candidate::lower(&incomplete_route_source(), catalog_restart_entry()).unwrap())
            .is_none()
    );
}

#[test]
fn due_schema_without_restart_entry_does_not_connect_the_route() {
    let ran = drive_plugin(
        &incomplete_route_source(),
        Some(&[10_000, 10_000]),
        &DUE_ONLY_PLUGIN,
        None,
    );
    assert_unrouted_empty_local(&ran);
    assert!(
        due_branch(&candidate::lower(&incomplete_route_source(), catalog_due_only()).unwrap())
            .is_none()
    );
}

#[test]
fn ordinary_run_control_catalog_does_not_connect_the_auto_route() {
    let ran = drive_plugin(
        &incomplete_route_source(),
        Some(&[10_000, 10_000]),
        &CONTROL_PLUGIN,
        None,
    );
    assert_unrouted_empty_local(&ran);
    assert!(
        due_branch(&candidate::lower(&incomplete_route_source(), catalog_for(true, true)).unwrap())
            .is_none()
    );
}

/// First empty local spends the only two budget segments. A later real
/// `local name = {}` stays available, and an assignment-seeded weak item is
/// still present, because the latch makes that local not due.
#[test]
fn second_empty_local_does_not_repeat_the_automatic_cycle() {
    let pad = "x".repeat(4096);
    let ran = run_auto_table(
        &format!(
            r#"
local function witness()
  local w = {{}}
  local mt = {{}}
  local held = {{}}
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = held
  w[2] = {{}}
  peek(held)
  note(11, collectgarbage("count"))
  note(2, collectgarbage("restart"))
  local fresh = {{}}
  bit(5, peek(fresh))
  text(3, type(fresh))
  text(4, type(rawget(w, 2)))
  w[3] = {{}}
  held.pad = "{pad}"
  meter(40)
  note(13, collectgarbage("count"))
  local again = {{}}
  bit(6, peek(again))
  text(7, type(again))
  text(8, type(rawget(w, 3)))
  collectgarbage("collect")
  text(9, type(rawget(w, 3)))
  note(9, same(held, rawget(w, 1)))
  return 1
end
witness()
return 1
"#
        ),
        Some(&[10_000, 10_000]),
    );
    assert_eq!(ran.stop, Stop::Halted, "{ran:?}");
    assert_eq!(text_value(&ran.trace, 3.0), "table");
    assert_eq!(
        text_value(&ran.trace, 4.0),
        "nil",
        "the first local collected"
    );
    assert_eq!(cycle_at(&ran.trace, 5.0), (false, 1));
    let logical = note_value(&ran.trace, 40.0);
    let threshold = note_value(&ran.trace, 41.0);
    assert!(
        threshold > 0.0 && logical >= threshold,
        "logical {logical} must already meet the restart threshold {threshold} before the second local"
    );
    assert!(
        note_value(&ran.trace, 13.0) >= note_value(&ran.trace, 11.0),
        "count before the second local must not sit below the restart reading"
    );
    assert!(
        note_value(&ran.trace, 42.0) == 0.0,
        "the latch makes the second local not due"
    );
    assert!(
        note_value(&ran.trace, 43.0) == 1.0,
        "the first cycle set the one-shot latch"
    );
    assert!(
        note_value(&ran.trace, 44.0) == 1.0,
        "the collector is still running"
    );
    assert!(
        bit_value(&ran.trace, 6.0),
        "the second empty local is usable"
    );
    assert_eq!(text_value(&ran.trace, 7.0), "table");
    assert_eq!(
        text_value(&ran.trace, 8.0),
        "table",
        "the assignment-seeded weak item must survive the second local"
    );
    assert_eq!(
        cycle_at(&ran.trace, 6.0),
        (false, 1),
        "the second local must not record another collection"
    );
    assert_eq!(
        text_value(&ran.trace, 9.0),
        "nil",
        "a later explicit collect can clear it"
    );
    assert_eq!(note_value(&ran.trace, 9.0), 1.0);
    assert_eq!(
        ran.collections, 2,
        "automatic cycle plus the explicit collect"
    );
    assert_eq!(ran.collects, 2);
    assert!(ran.trace.steps.is_empty());
    assert!(!ran.tail);
}

const RESERVED_SOURCE: &str = r#"
local function witness()
  local held = external_shape()
  local fresh = {}
  return 1
end
witness()
return 1
"#;

/// The due call has no guest arguments. Gather statements, including their
/// reserved slots, live only in the taken branch and are absent from the
/// not-due branch.
#[test]
fn due_branch_reserves_gather_slots_without_a_guest_argument() {
    let ir = candidate::lower(RESERVED_SOURCE, catalog_auto()).expect("lower opt-in");
    let branch = due_branch(&ir).expect("opt-in empty local emits the due branch");
    assert!(
        branch.condition_args == 0,
        "the judgment must not evaluate a guest argument"
    );
    assert!(
        branch.gather_slots >= 1,
        "the taken branch reserves gather slots"
    );
    assert!(branch.gather_slots_unique);
    assert!(
        !branch.then_calls.is_empty()
            && branch
                .then_calls
                .iter()
                .all(|name| name == candidate::GATHER_ROOT),
        "the taken branch is only gather statements: {:?}",
        branch.then_calls
    );
    assert_eq!(branch.then_expr, candidate::AUTO_TABLE_RESTART);
    assert!(
        branch.else_calls.is_empty(),
        "the not-due branch does no host work"
    );
    assert!(branch.restart_args_have_no_call);
    let plain = candidate::lower(RESERVED_SOURCE, catalog_for(true, true)).expect("lower plain");
    assert!(due_branch(&plain).is_none());
    assert!(
        branch.gather_slots + count_lets(&plain) <= count_lets(&ir),
        "skipping the branch at run time does not remove the reserved IR slots"
    );
}

struct DueBranch {
    condition_args: usize,
    gather_slots: usize,
    gather_slots_unique: bool,
    then_calls: Vec<String>,
    then_expr: String,
    else_calls: Vec<String>,
    restart_args_have_no_call: bool,
}

fn due_branch(ir: &FrontendIr) -> Option<DueBranch> {
    let mut found = None;
    for function in ir.function_impls.values() {
        for stmt in &function.body_stmts {
            visit_stmt(stmt, &mut found);
        }
        visit_expr(&function.body_expr, &mut found);
    }
    for stmt in &ir.stmts {
        visit_stmt(stmt, &mut found);
    }
    found
}

fn visit_stmt(stmt: &Stmt, found: &mut Option<DueBranch>) {
    match stmt {
        Stmt::Let { expr, .. } | Stmt::Assign { expr, .. } => visit_expr(expr, found),
        _ => {}
    }
}

fn visit_expr(expr: &Expr, found: &mut Option<DueBranch>) {
    if let Expr::IfElse {
        condition,
        then_expr,
        else_expr,
    } = expr
        && condition
            .host_call_resolution()
            .is_some_and(|call| call.name == candidate::AUTO_TABLE_DUE)
    {
        let Expr::Call(_, _, args, _, _) = condition.as_ref() else {
            unreachable!("due resolution is a call");
        };
        let Expr::Block { stmts, expr: taken } = then_expr.as_ref() else {
            panic!("due branch must hold the gather statements");
        };
        let mut slots = Vec::new();
        let mut then_calls = Vec::new();
        for stmt in stmts {
            let Stmt::Let { index, expr, .. } = stmt else {
                panic!("gather branch contains a non-let: {stmt:?}");
            };
            let name = expr
                .host_call_resolution()
                .map(|call| call.name.clone())
                .unwrap_or_default();
            then_calls.push(name);
            slots.push(format!("{index:?}"));
        }
        let unique = slots.iter().collect::<HashSet<_>>().len() == slots.len();
        let restart_args_have_no_call = match taken.as_ref() {
            Expr::Call(_, _, args, _, _) => args.iter().all(|arg| call_names(arg).is_empty()),
            _ => false,
        };
        *found = Some(DueBranch {
            condition_args: args.len(),
            gather_slots: slots.len(),
            gather_slots_unique: unique,
            then_calls,
            then_expr: taken
                .host_call_resolution()
                .map(|call| call.name.clone())
                .unwrap_or_default(),
            else_calls: call_names(else_expr),
            restart_args_have_no_call,
        });
        return;
    }
    match expr {
        Expr::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            visit_expr(condition, found);
            visit_expr(then_expr, found);
            visit_expr(else_expr, found);
        }
        Expr::Block { stmts, expr } => {
            for stmt in stmts {
                visit_stmt(stmt, found);
            }
            visit_expr(expr, found);
        }
        Expr::Call(_, _, args, _, _) => {
            for arg in args {
                visit_expr(arg, found);
            }
        }
        _ => {}
    }
}

fn call_names(expr: &Expr) -> Vec<String> {
    let mut names = Vec::new();
    fn walk(expr: &Expr, names: &mut Vec<String>) {
        if let Some(call) = expr.host_call_resolution() {
            names.push(call.name.clone());
        }
        match expr {
            Expr::Call(_, _, args, _, _) => {
                for arg in args {
                    walk(arg, names);
                }
            }
            Expr::IfElse {
                condition,
                then_expr,
                else_expr,
            } => {
                walk(condition, names);
                walk(then_expr, names);
                walk(else_expr, names);
            }
            Expr::Block { stmts, expr } => {
                for stmt in stmts {
                    if let Stmt::Let { expr, .. } | Stmt::Assign { expr, .. } = stmt {
                        walk(expr, names);
                    }
                }
                walk(expr, names);
            }
            _ => {}
        }
    }
    walk(expr, &mut names);
    names
}

fn count_lets(ir: &FrontendIr) -> usize {
    fn stmts(stmts: &[Stmt]) -> usize {
        stmts
            .iter()
            .map(|stmt| match stmt {
                Stmt::Let { expr, .. } | Stmt::Assign { expr, .. } => 1 + exprs(expr),
                _ => 0,
            })
            .sum()
    }
    fn exprs(expr: &Expr) -> usize {
        match expr {
            Expr::Block { stmts: inner, expr } => stmts(inner) + exprs(expr),
            Expr::IfElse {
                condition,
                then_expr,
                else_expr,
            } => exprs(condition) + exprs(then_expr) + exprs(else_expr),
            Expr::Call(_, _, args, _, _) => args.iter().map(exprs).sum(),
            _ => 0,
        }
    }
    let mut total = stmts(&ir.stmts);
    for function in ir.function_impls.values() {
        total += stmts(&function.body_stmts);
        total += exprs(&function.body_expr);
    }
    total
}

/// This graph's measured pause map, from a read-only phase sweep on the parent:
/// 1 is reset, 33 is roots, 34..=54 is gray with a pending scan, 56 is gray
/// with the scan already drained, and 58 publishes the tail. 40 is inside the
/// pending gray window, not a one-unit guess.
const GRAY_PENDING_BUDGET: u64 = 40;
const GRAY_NODES_BEFORE_FRESH: u32 = 13;

fn gray_function(tail: &str) -> String {
    format!(
        "local function witness()\n  \
local w = {{}}\n  \
local mt = {{}}\n  \
local held = {{}}\n  \
local ballast = {{}}\n  \
mt.__mode = \"v\"\n  \
setmetatable(w, mt)\n  \
w[1] = held\n  \
w[2] = {{}}\n  \
ballast[1] = {{}}\n  \
ballast[2] = {{}}\n  \
ballast[3] = {{}}\n  \
ballast[4] = {{}}\n  \
ballast[5] = {{}}\n  \
ballast[6] = {{}}\n  \
ballast[7] = {{}}\n  \
ballast[8] = {{}}\n  \
{tail}\n\
end\nwitness()\nreturn 1\n",
        tail = tail
    )
}

fn mark_note(trace: &Trace, tag: f64) -> &MarkNote {
    trace
        .marks
        .iter()
        .find(|note| note.tag == tag)
        .unwrap_or_else(|| panic!("missing mark note {tag} in {:?}", trace.marks))
}

fn assert_fresh_not_built(ran: &Ran) {
    assert_eq!(
        ran.trace.peeks.len(),
        1,
        "only the pre-existing root was peeked"
    );
    assert_eq!(ran.parked_root_writes, 0, "ROOT_ISSUED did not run");
    assert_eq!(ran.collects, 0);
    assert_eq!(ran.collections, 0);
    assert!(!ran.tail);
    assert_eq!(
        text_value(&ran.trace, 1.0),
        "table",
        "weak item still present"
    );
}

/// Opt-in restart that pauses in gray with scan work still queued must finish
/// the empty local through the existing NEW_TABLE plus ROOT_ISSUED handoff,
/// leave that cycle saved, and let a later explicit step finish it once.
#[test]
fn auto_gray_pause_allocates_the_empty_local_and_keeps_the_cycle() {
    let source = gray_function(
        "\
bit(1, peek(held))
  note(1, collectgarbage(\"restart\"))
  text(1, type(rawget(w, 2)))
  local fresh = {}
  bit(2, peek(fresh))
  mark_phase(1)
  text(2, type(rawget(w, 2)))
  bit(3, collectgarbage(\"step\"))
  mark_phase(2)
  text(3, type(rawget(w, 2)))
  bit(4, collectgarbage(\"step\"))
  text(4, type(rawget(w, 2)))
  bit(5, collectgarbage(\"step\"))
  bit(6, peek(held))
  bit(7, peek(fresh))
  return 1",
    );
    let ran = run_auto_table(&source, Some(&[GRAY_PENDING_BUDGET, 1, 10_000, 10_000]));
    assert_eq!(ran.stop, Stop::Halted, "{ran:?}");
    assert!(
        bit_value(&ran.trace, 1.0),
        "held was alive before the pause"
    );
    assert!(bit_value(&ran.trace, 2.0), "fresh was allocated");
    assert!(bit_value(&ran.trace, 6.0), "held survived the continuation");
    assert!(
        bit_value(&ran.trace, 7.0),
        "fresh survived the continuation"
    );
    let held = ran.trace.peeks[0].table;
    let fresh = ran.trace.peeks[1].table;
    assert_ne!(held, fresh, "the new table is not the existing strong root");
    assert_eq!(ran.trace.peeks[2].table, held);
    assert_eq!(ran.trace.peeks[3].table, fresh);
    let at_handoff = mark_note(&ran.trace, 1.0);
    assert_eq!(at_handoff.phase, Some("gray"));
    assert!(
        at_handoff.pending,
        "scan work remained after the root handoff"
    );
    assert!(
        at_handoff.parked_root_writes >= 1,
        "ROOT_ISSUED wrote the new root during gray: {at_handoff:?}"
    );
    assert!(
        at_handoff.work_pushed >= 11,
        "the new root was marked onto the gray queue: {at_handoff:?}"
    );
    assert_eq!(at_handoff.mark_passes, 1);
    assert_eq!(
        at_handoff.collects, 0,
        "the automatic pause did not record a completion"
    );
    assert_eq!(at_handoff.live_nodes, GRAY_NODES_BEFORE_FRESH + 1);
    assert_eq!(text_value(&ran.trace, 1.0), "table");
    assert_eq!(
        text_value(&ran.trace, 2.0),
        "table",
        "weak item survived the handoff"
    );
    assert!(
        !bit_value(&ran.trace, 3.0),
        "the one-unit explicit step must be the next budget, not a later 10000"
    );
    let after_unit = mark_note(&ran.trace, 2.0);
    assert_eq!(after_unit.phase, Some("gray"));
    assert!(after_unit.pending);
    assert_eq!(text_value(&ran.trace, 3.0), "table");
    assert!(
        !bit_value(&ran.trace, 4.0),
        "publishing the weak clear is not a completion"
    );
    assert_eq!(
        text_value(&ran.trace, 4.0),
        "nil",
        "the explicit continuation cleared the weak item"
    );
    assert!(
        bit_value(&ran.trace, 5.0),
        "the following explicit step completed the saved tail"
    );
    assert_eq!(
        ran.collects, 1,
        "the continuation recorded the completion once"
    );
    assert_eq!(ran.collections, 1);
    assert!(!ran.tail);
    assert!(
        ran.trace.steps.is_empty(),
        "guest collectgarbage step is not the private step host"
    );
}

/// The same pause must not skip the ordinary node-capacity refusal.
#[test]
fn auto_gray_pause_still_refuses_a_full_node_quota() {
    let source = gray_function(
        "\
bit(1, peek(held))
  note(1, collectgarbage(\"restart\"))
  text(1, type(rawget(w, 2)))
  local fresh = {}
  bit(2, peek(fresh))
  return 1",
    );
    let ran = drive_plugin(
        &source,
        Some(&[GRAY_PENDING_BUDGET]),
        &AUTO_TABLE_PLUGIN,
        Some(GRAY_NODES_BEFORE_FRESH),
    );
    assert!(
        matches!(ran.stop, Stop::HostError(ref message) if message.contains("lua heap node capacity")),
        "gray pause must not bypass the node quota: {:?}",
        ran.stop
    );
    assert_fresh_not_built(&ran);
    assert_eq!(ran.mark_phase, Some("gray"));
    assert!(ran.mark_pending);
    assert_eq!(ran.live_nodes, GRAY_NODES_BEFORE_FRESH);
}

#[test]
fn auto_gray_graph_reset_pause_does_not_allocate() {
    let source = gray_function(
        "\
bit(1, peek(held))
  note(1, collectgarbage(\"restart\"))
  text(1, type(rawget(w, 2)))
  local fresh = {}
  bit(2, peek(fresh))
  return 1",
    );
    let ran = run_auto_table(&source, Some(&[1]));
    assert!(
        matches!(ran.stop, Stop::HostError(ref message) if message == "auto table restart mark paused tail=false"),
        "{:?}",
        ran.stop
    );
    assert_eq!(ran.mark_phase, Some("reset"));
    assert!(!ran.mark_pending);
    assert_fresh_not_built(&ran);
}

#[test]
fn auto_gray_graph_roots_pause_does_not_allocate() {
    let source = gray_function(
        "\
bit(1, peek(held))
  note(1, collectgarbage(\"restart\"))
  text(1, type(rawget(w, 2)))
  local fresh = {}
  bit(2, peek(fresh))
  return 1",
    );
    let ran = run_auto_table(&source, Some(&[33]));
    assert!(
        matches!(ran.stop, Stop::HostError(ref message) if message == "auto table restart mark paused tail=false"),
        "{:?}",
        ran.stop
    );
    assert_eq!(ran.mark_phase, Some("roots"));
    assert!(ran.mark_pending);
    assert_fresh_not_built(&ran);
}

#[test]
fn auto_gray_pause_without_pending_scan_does_not_allocate() {
    let source = gray_function(
        "\
bit(1, peek(held))
  note(1, collectgarbage(\"restart\"))
  text(1, type(rawget(w, 2)))
  local fresh = {}
  bit(2, peek(fresh))
  return 1",
    );
    let ran = run_auto_table(&source, Some(&[56]));
    assert!(
        matches!(ran.stop, Stop::HostError(ref message) if message == "auto table restart mark paused tail=false"),
        "{:?}",
        ran.stop
    );
    assert_eq!(ran.mark_phase, Some("gray"));
    assert!(!ran.mark_pending, "drained gray is not remaining scan work");
    assert_fresh_not_built(&ran);
}

#[test]
fn auto_gray_published_tail_is_not_swallowed() {
    let source = gray_function(
        "\
bit(1, peek(held))
  note(1, collectgarbage(\"restart\"))
  text(1, type(rawget(w, 2)))
  local fresh = {}
  bit(2, peek(fresh))
  return 1",
    );
    let ran = run_auto_table(&source, Some(&[58]));
    assert!(
        matches!(
            ran.stop,
            Stop::HostError(ref message)
                if message.contains("missing source step fixture budget")
                    || message.contains("did not finish the published tail")
        ),
        "a published tail must stay an error: {:?}",
        ran.stop
    );
    assert_eq!(ran.trace.peeks.len(), 1, "fresh was not constructed");
    assert_eq!(ran.parked_root_writes, 0);
    assert_eq!(ran.collects, 0);
    assert_eq!(ran.collections, 0);
    assert_eq!(text_value(&ran.trace, 1.0), "table");
    assert!(ran.tail);
    assert!(ran.mark_phase.is_none());
}

/// Same function, one ordinary empty local, after restart has made the heap due.
/// Four weak-only entries give the published tail a dead suffix. The first
/// budget is fixture evidence that this graph publishes; it is not a phase
/// contract. The second budget does one Release visit and leaves a suffix.
const RELEASE_ALLOC_BODY: &str = r#"
local function witness()
  local w = {}
  local mt = {}
  local held = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = held
  w[2] = {}
  w[3] = {}
  w[4] = {}
  w[5] = {}
  bit(1, peek(held))
  bit(2, peek(w))
  note(1, collectgarbage("restart"))
  text(1, type(rawget(w, 2)))
  local fresh = {}
  text(2, type(rawget(w, 2)))
  bit(3, peek(fresh))
  collectgarbage("step")
  bit(4, peek(fresh))
  collectgarbage("collect")
  bit(5, peek(fresh))
  return 1
end
witness()
collectgarbage("collect")
return 1
"#;

const RELEASE_ALLOC_PAUSE: &str = r#"
local function witness()
  local w = {}
  local mt = {}
  local held = {}
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = held
  w[2] = {}
  w[3] = {}
  w[4] = {}
  w[5] = {}
  bit(1, peek(held))
  bit(2, peek(w))
  note(1, collectgarbage("restart"))
  text(1, type(rawget(w, 2)))
  local fresh = {}
  bit(3, peek(fresh))
  return 1
end
witness()
return 1
"#;

fn release_pause_error(message: &str) -> bool {
    message == "auto table restart did not finish the published tail: Paused { tail: true }"
}

#[test]
fn auto_release_pause_allocates_the_empty_local() {
    let ran = run_auto_table(RELEASE_ALLOC_BODY, Some(&[10_000, 1, 10_000]));
    assert_eq!(ran.stop, Stop::Halted, "{ran:#?}");
    assert_eq!(
        ran.trace.peeks.len(),
        5,
        "fresh is issued and re-read: {ran:#?}"
    );
    let fresh = ran.trace.peeks[2].table;
    assert_eq!(ran.trace.peeks[3].table, fresh);
    assert_eq!(ran.trace.peeks[4].table, fresh);
    assert!(
        fresh.generation() > 1,
        "the new node reuses a released slot: {fresh:?}"
    );
    assert_eq!(
        ran.trace.bit_cycle_state[2],
        (true, 0),
        "handoff keeps the tail"
    );
    assert_eq!(
        ran.trace.bit_cycle_state[3],
        (false, 1),
        "explicit continuation completes once"
    );
    assert_eq!(
        ran.trace.bits[3],
        (4.0, true),
        "the same object stays reachable"
    );
    assert_eq!(
        ran.trace.bits[4],
        (5.0, true),
        "the function root keeps it through another cycle"
    );
    assert_eq!(text_value(&ran.trace, 1.0), "table");
    assert_eq!(
        text_value(&ran.trace, 2.0),
        "nil",
        "the independent weak entry was cleared"
    );
    assert_eq!(
        ran.parked_root_writes, 0,
        "the handoff must not park a mark"
    );
    let accounting = ran.trace.bit_accounting[2].expect("tail accounting at the handoff");
    assert_eq!(accounting.1, 1, "one late node: {accounting:?}");
    assert!(
        accounting.0 > 0,
        "the handoff does not clear the mark count"
    );
    let tail = ran.trace.bit_tail[2].expect("saved tail at the handoff");
    assert_eq!(tail.phase, heap::SavedTailPhase::Release);
    assert_eq!(
        tail.cursor, 1,
        "the handoff does not move the release cursor"
    );
    assert_eq!(
        tail.live_roots, 0,
        "the new root is not counted until continuation"
    );
    assert!(
        tail.cursor < tail.dead_len,
        "an unconsumed dead suffix remains"
    );
    let roots = &ran.trace.bit_roots[2];
    assert_eq!(
        roots.get(3).copied().flatten(),
        Some(fresh),
        "top-frame slot 3 is fresh"
    );
    let held = ran.trace.peeks[0].table;
    let table_w = ran.trace.peeks[1].table;
    assert_eq!(roots.len(), roots.iter().flatten().count());
    assert_eq!(roots[0], Some(table_w));
    assert_eq!(roots[2], Some(held));
    assert!(roots[1].is_some());
    assert_ne!(roots[1], Some(table_w));
    assert_ne!(roots[1], Some(held));
    assert_ne!(roots[1], Some(fresh));
    let occupied = roots.iter().flatten().count();
    let txn = ran.txn.expect("the handoff stored a root");
    assert_eq!(txn.before.tail, txn.after.tail);
    assert_eq!(txn.before.tail.phase, heap::SavedTailPhase::Release);
    assert_eq!(txn.before.marked, txn.after.marked);
    assert!(txn.before.marked > 0, "the pause sampled a real mark count");
    assert_eq!(txn.before.late, 0);
    assert_eq!(txn.after.late, txn.before.late + 1);
    assert_eq!(
        ran.trace.bit_accounting[2],
        Some((txn.after.marked, txn.after.late))
    );
    assert_eq!(ran.trace.bit_tail[2], Some(txn.after.tail));
    assert_eq!(
        ran.trace.bit_collects,
        vec![0, 0, 0, 1, 2],
        "paused handoff, one continuation, then the in-function collect"
    );
    assert_eq!(ran.reports.len(), 3, "frame exit is followed by one report");
    assert_eq!(ran.collects, 3);
    assert_eq!(ran.collections, 3);
    assert_eq!(ran.issued_alive.len(), ran.reports.len());
    assert_eq!(ran.root_visits.len(), ran.reports.len());
    let continuation = &ran.reports[0];
    assert_eq!(continuation.marked_nodes, txn.before.marked);
    assert_eq!(continuation.live_nodes, txn.before.marked + 1);
    assert_eq!(continuation.live_roots as usize, occupied);
    assert_eq!(ran.trace.bit_root_slots[2] as usize, roots.len());
    let continuation_visits = ran.root_visits[0] - ran.trace.bit_root_visits[2];
    assert_eq!(
        continuation_visits as usize,
        roots.len(),
        "continuation scans each fixed root slot once"
    );
    assert!(
        ran.issued_alive[1],
        "the in-function collect still sees this generation"
    );
    assert_eq!(ran.trace.peeks[4].table.generation(), fresh.generation());
    assert!(
        !ran.issued_alive[2],
        "the collect after return no longer sees fresh"
    );
    assert_eq!(
        ran.tail_accounting, None,
        "a finished tail is not late zero"
    );
    let alive = ran.peek_end[2];
    assert!(!alive.0, "scalar return then collect releases the node");
    assert!(!alive.1, "the token is closed with the node");
    assert_eq!(ran.peek_end[3], (false, false));
    assert_eq!(ran.peek_end[4], (false, false));
}

#[test]
fn stopped_release_shape_allocates_without_a_cycle() {
    let source =
        RELEASE_ALLOC_PAUSE.replace("collectgarbage(\"restart\")", "collectgarbage(\"stop\")");
    let ran = run_auto_table(&source, None);
    assert_eq!(ran.stop, Stop::Halted, "{ran:#?}");
    assert!(!ran.tail, "stop must not start a cycle");
    assert_eq!(ran.collections, 0);
    assert_eq!(
        ran.trace.peeks.len(),
        3,
        "the empty local is an ordinary allocation"
    );
    assert_eq!(text_value(&ran.trace, 1.0), "table");
    assert_eq!(
        ran.tail_accounting, None,
        "stop never opens a tail; None is not a zero late count"
    );
}

#[test]
fn auto_release_roots_pause_does_not_allocate() {
    let ran = run_auto_table(RELEASE_ALLOC_PAUSE, Some(&[10_000, 5]));
    assert!(
        matches!(ran.stop, Stop::HostError(ref message) if release_pause_error(message)),
        "{ran:#?}"
    );
    assert_eq!(ran.trace.peeks.len(), 2);
    assert_eq!(ran.collections, 0);
    let state = ran.tail_state.expect("saved tail");
    assert_eq!(state.phase, heap::SavedTailPhase::Roots);
    let (marked, late) = saved_release_accounting(&ran);
    assert_eq!(late, 0, "the Roots refusal adds no late node");
    assert!(marked > 0);
    assert_eq!(ran.permit, None, "Roots never installs a Release permit");
    assert_eq!(ran.top_roots.len(), ran.trace.bit_roots[1].len());
    for (after, before) in ran.top_roots.iter().zip(&ran.trace.bit_roots[1]) {
        if after.is_some() {
            assert_eq!(after, before, "the Roots refusal writes no new root");
        }
    }
}

#[test]
fn auto_release_chunk_local_does_not_allocate() {
    let source = r#"
local function keep_return_window()
  return 1
end
keep_return_window()
local w = {}
local mt = {}
local held = {}
mt.__mode = "v"
setmetatable(w, mt)
w[1] = held
w[2] = {}
w[3] = {}
w[4] = {}
w[5] = {}
bit(1, peek(held))
bit(2, peek(w))
note(1, collectgarbage("restart"))
text(1, type(rawget(w, 2)))
local fresh = {}
bit(3, peek(fresh))
return 1
"#;
    let ran = run_auto_table(source, Some(&[10_000, 1]));
    assert!(
        matches!(ran.stop, Stop::HostError(ref message) if release_pause_error(message)),
        "chunk has no root handoff: {ran:#?}"
    );
    assert_eq!(ran.trace.peeks.len(), 2);
    let state = ran.tail_state.expect("saved tail");
    assert_eq!(state.phase, heap::SavedTailPhase::Release);
    assert!(state.cursor > 0 && state.cursor < state.dead_len);
    let (_, late) = saved_release_accounting(&ran);
    assert_eq!(late, 0);
    assert_eq!(
        ran.permit, None,
        "a chunk local never takes a Release permit"
    );
    assert_eq!(ran.trace.peeks.len(), 2, "fresh is not issued");
}

#[test]
fn auto_release_pause_refuses_a_full_node_quota() {
    let ran = run_auto_table_limited(RELEASE_ALLOC_PAUSE, Some(&[10_000, 1]), 7, 1, false);
    assert!(
        matches!(ran.stop, Stop::HostError(ref message) if message.contains("node capacity")),
        "{ran:#?}"
    );
    assert_eq!(ran.trace.peeks.len(), 2, "the refused local is not issued");
    assert_eq!(ran.collections, 0);
    let state = ran.tail_state.expect("legal scan remains");
    assert_eq!(state.phase, heap::SavedTailPhase::Release);
    assert_eq!(state.cursor, 1);
    assert!(state.cursor < state.dead_len);
    assert_eq!(state.live_roots, 0);
    let (_, late) = saved_release_accounting(&ran);
    assert_eq!(late, 0);
    assert_eq!(
        ran.permit.map(|(_, consumed)| consumed),
        Some(true),
        "capacity is refused after the permit is taken"
    );
    assert_eq!(
        ran.live_nodes, 6,
        "the release visit is scan progress, not a new node"
    );
}

#[test]
fn auto_release_pause_rolls_back_a_registered_token() {
    let refused = run_auto_table_limited(RELEASE_ALLOC_PAUSE, Some(&[10_000, 1]), 64, 8, true);
    let clean = run_auto_table_limited(RELEASE_ALLOC_PAUSE, Some(&[10_000, 1]), 7, 1, false);
    assert!(
        matches!(refused.stop, Stop::HostError(ref message) if message.contains("refused after the token was registered")),
        "{refused:#?}"
    );
    assert!(
        matches!(clean.stop, Stop::HostError(ref message) if message.contains("node capacity")),
        "capacity refusal is the no-token control: {clean:#?}"
    );
    assert_eq!(refused.trace.peeks.len(), 2);
    let (_, late) = saved_release_accounting(&refused);
    assert_eq!(late, 0);
    assert_eq!(
        refused.tail_state, clean.tail_state,
        "rollback does not move the legal scan"
    );
    assert_eq!(refused.live_nodes, clean.live_nodes);
    assert_eq!(
        refused.resources, clean.resources,
        "the registered token is closed"
    );
}

#[test]
fn auto_release_pause_rolls_back_an_injected_committed_late() {
    // Injected after commit_late and before the first root write. This is not
    // a guest store error. The rollback is the same close, undo_committed_late,
    // and slot release a real store error uses.
    let injected = run_auto_table_post_commit(RELEASE_ALLOC_PAUSE, Some(&[10_000, 1]));
    let clean = run_auto_table_limited(RELEASE_ALLOC_PAUSE, Some(&[10_000, 1]), 7, 1, false);
    assert!(
        matches!(
            injected.stop,
            Stop::HostError(ref message)
                if message.contains("injected after commit_late and before the first root write")
        ),
        "{injected:#?}"
    );
    assert!(
        matches!(clean.stop, Stop::HostError(ref message) if message.contains("node capacity")),
        "capacity refusal is the no-token control: {clean:#?}"
    );
    assert!(injected.txn.is_none(), "the injection never stores a root");
    let (_, late) = saved_release_accounting(&injected);
    assert_eq!(late, 0, "undo_committed_late returns the late count");
    assert_eq!(injected.tail_accounting, clean.tail_accounting);
    assert_eq!(
        injected.tail_state, clean.tail_state,
        "the cursor is not advanced"
    );
    assert_eq!(injected.live_nodes, clean.live_nodes);
    assert_eq!(
        injected.resources, clean.resources,
        "the committed token is closed"
    );
    assert_eq!(
        root_identity(&injected.top_roots),
        root_identity(&clean.top_roots),
        "the root was not stored"
    );
    assert!(
        injected.top_roots.last().is_some_and(|slot| slot.is_none()),
        "the handoff slot stays empty"
    );
    assert_eq!(injected.trace.peeks.len(), 2);
    assert_eq!(injected.permit.map(|(_, consumed)| consumed), Some(true));
}

fn saved_release_accounting(ran: &Ran) -> (u32, u32) {
    ran.tail_accounting
        .expect("a saved tail is Some((marked, late)), not a missing sample")
}

fn root_identity(roots: &[Option<heap::TableRef>]) -> Vec<Option<(u32, u32)>> {
    roots
        .iter()
        .map(|slot| slot.map(|table| (table.slot(), table.generation())))
        .collect()
}

#[test]
fn chunk_auto_gray_pause_does_not_allocate() {
    let source = r#"
local function keep_return_window()
  return 1
end
local w = {}
local mt = {}
local held = {}
local ballast = {}
mt.__mode = "v"
setmetatable(w, mt)
w[1] = held
w[2] = {}
ballast[1] = {}
ballast[2] = {}
ballast[3] = {}
ballast[4] = {}
ballast[5] = {}
ballast[6] = {}
ballast[7] = {}
ballast[8] = {}
bit(1, peek(held))
note(1, collectgarbage("restart"))
text(1, type(rawget(w, 2)))
local fresh = {}
bit(2, peek(fresh))
keep_return_window()
return 1
"#;
    let ran = run_auto_table(source, Some(&[GRAY_PENDING_BUDGET]));
    assert!(
        matches!(ran.stop, Stop::HostError(ref message) if message == "auto table restart mark paused tail=false"),
        "chunk has no ROOT_ISSUED handoff: {:?}",
        ran.stop
    );
    assert_eq!(ran.mark_phase, Some("gray"));
    assert!(ran.mark_pending);
    assert_fresh_not_built(&ran);
}

#[test]
fn stopped_gray_graph_does_not_start_a_cycle() {
    let source = gray_function(
        "\
bit(1, peek(held))
  note(1, collectgarbage(\"stop\"))
  bit(2, collectgarbage(\"isrunning\"))
  text(1, type(rawget(w, 2)))
  local fresh = {}
  bit(3, peek(fresh))
  return 1",
    );
    let ran = run_auto_table(&source, None);
    assert_eq!(ran.stop, Stop::Halted, "{ran:?}");
    assert!(bit_value(&ran.trace, 1.0));
    assert!(!bit_value(&ran.trace, 2.0));
    assert!(bit_value(&ran.trace, 3.0));
    assert_eq!(text_value(&ran.trace, 1.0), "table");
    assert_eq!(ran.trace.peeks.len(), 2);
    assert_ne!(ran.trace.peeks[0].table, ran.trace.peeks[1].table);
    assert_eq!(ran.collects, 0);
    assert_eq!(ran.collections, 0);
    assert!(ran.mark_phase.is_none());
    assert!(!ran.tail);
    assert!(ran.trace.steps.is_empty());
}

#[test]
fn direct_auto_restart_gray_pause_does_not_allocate() {
    let source = gray_function(
        "\
bit(1, peek(held))
  note(1, collectgarbage(\"restart\"))
  text(1, type(rawget(w, 2)))
  note(2, direct_auto_restart())
  return 1",
    );
    let ran = run_auto_table(&source, Some(&[GRAY_PENDING_BUDGET]));
    assert!(
        matches!(ran.stop, Stop::HostError(ref message) if message == "auto table restart mark paused tail=false"),
        "a direct call is not the root handoff: {:?}",
        ran.stop
    );
    assert_fresh_not_built(&ran);
}

/// Fixed frame at the first due empty local: `w`, `mt`, `held`, `fresh`, `spare`.
const ROOT_CAPACITY_SLOTS: u32 = 5;
/// Issued tables in the decoded batch. The live scalar is not one of them.
const ROOT_CAPACITY_BATCH: u32 = 3;
const ROOT_CAPACITY_PLANNED: u32 = ROOT_CAPACITY_SLOTS + ROOT_CAPACITY_BATCH;
const ROOT_STEP_BUDGETS: [u64; 2] = [10_000, 10_000];

const AUTOMATIC_ROOT_CAPACITY_SOURCE: &str = r#"
local function witness()
  local w = {}
  local mt = {}
  local held = {}
  local scalar = 7
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = held
  w[2] = {}
  peek(held)
  note(1, collectgarbage("restart"))
  note(20, scalar)
  meter(30)
  occupy(1)
  text(1, type(rawget(w, 2)))
  local fresh = {}
  bit(5, peek(fresh))
  text(3, type(fresh))
  text(4, type(rawget(w, 2)))
  note(8, same(held, rawget(w, 1)))
  note(21, scalar)
  local spare = {}
  bit(9, peek(spare))
  return 1
end
witness()
return 1
"#;

#[derive(Clone, Debug)]
struct RootArm {
    roots_limit: u32,
    stop: Stop,
    trace: Trace,
    tail: bool,
    collections: u32,
    live_nodes: u32,
    root_slots: u32,
    live_roots: u32,
    node_slots: u32,
    edge_slots: u32,
    collects: usize,
    mark_phase: Option<&'static str>,
    mark_passes: u64,
    node_slot_visits: u64,
    parked_root_writes: u64,
    budgets: Vec<u64>,
    meter: candidate::AutoTableMeter,
    plan: Option<candidate::AutoRootPlan>,
    idle: bool,
    held_alive: Option<bool>,
}

fn occupy_note(trace: &Trace) -> &OccupancyNote {
    trace
        .occupancy
        .first()
        .unwrap_or_else(|| panic!("occupy did not run in this scope: {:?}", trace.occupancy))
}

fn run_auto_roots(source: &str, roots: u32) -> RootArm {
    let plugin = &AUTO_TABLE_PLUGIN;
    let catalog = Arc::clone(plugin_catalog(plugin));
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(plugin)
        .with_host_api_catalog(Arc::clone(&catalog));
    let prepared = vm::compile_source_with_flavor_and_options(source, SourceFlavor::Lua, options)
        .unwrap_or_else(|error| panic!("compile rejected the source: {error}"));
    let mut vm = prepared.into_vm().expect("create VM");
    assert_return_window(vm.program());
    let mut registry = HostFunctionRegistry::empty();
    candidate::register(&mut registry, &catalog).expect("register candidate runtime");
    register(&mut registry, &catalog);
    registry.bind_vm_cached(&mut vm).expect("bind runtime");
    let prepared_budgets: Vec<_> = ROOT_STEP_BUDGETS
        .iter()
        .map(|&budget| NonZeroU64::new(budget).expect("positive fixture budget"))
        .collect();
    candidate::set_step_budgets(&mut vm.host_context(), &prepared_budgets);
    let production = heap::HeapLimits::production();
    heap::install(
        &mut vm.host_context(),
        heap::HeapLimits {
            nodes: production.nodes,
            roots,
            edges: production.edges,
            max_generation: production.max_generation,
        },
    )
    .expect("install the roots limit before execution");
    let status = vm.run();
    let stop = match status {
        Ok(VmStatus::Halted) => Stop::Halted,
        Err(VmError::HostError(message)) => Stop::HostError(message),
        Ok(other) => Stop::Other(format!("{other:?}")),
        Err(other) => Stop::Other(other.to_string()),
    };
    let mut host = vm.host_context();
    let trace = host.scope_state::<Trace>().cloned().unwrap_or_default();
    let stats = host
        .scope_state::<heap::LuaHeap>()
        .map(|heap| heap.stats())
        .unwrap_or_else(|| {
            panic!("installed heap missing after {stop:?}; refusing to install again")
        });
    let mark = heap::saved_mark_query(&host);
    let scan = heap::copy_scan_meter(&mut host)
        .unwrap_or_else(|error| panic!("scan copy failed without installing: {error}"));
    let tail = heap::saved_sweep_tail(&mut host)
        .unwrap_or_else(|error| panic!("tail read failed without installing: {error}"));
    let idle = heap::fixed_step_idle(&mut host)
        .unwrap_or_else(|error| panic!("idle read failed without installing: {error}"));
    let meter = candidate::auto_table_meter(&mut host)
        .unwrap_or_else(|error| panic!("meter read failed without installing: {error}"));
    let budgets = candidate::step_budget_snapshot(&host);
    let plan = candidate::auto_root_plan(&host);
    let collects = host
        .scope_state::<candidate::ReturnWitness>()
        .map(|witness| witness.collects.len())
        .unwrap_or(0);
    let held_alive = trace.peeks.first().map(|observed| {
        host.scope_state::<heap::LuaHeap>()
            .is_some_and(|heap| heap.alive(observed.table))
    });
    RootArm {
        roots_limit: roots,
        stop,
        trace,
        tail,
        collections: stats.collections,
        live_nodes: stats.live_nodes,
        root_slots: stats.root_slots,
        live_roots: stats.live_roots,
        node_slots: stats.node_slots,
        edge_slots: stats.edge_slots,
        collects,
        mark_phase: mark.map(|(phase, _)| phase),
        mark_passes: scan.mark_passes,
        node_slot_visits: scan.node_slot_visits,
        parked_root_writes: scan.parked_root_writes,
        budgets,
        meter,
        plan,
        idle,
        held_alive,
    }
}

fn assert_same_scope_plan(ran: &RootArm) {
    let occupy = occupy_note(&ran.trace);
    assert_eq!(occupy.tag, 1.0);
    assert!(
        occupy.phase.is_none(),
        "the pre-safepoint read must still be idle: {occupy:?}"
    );
    assert!(
        occupy.root_slots > occupy.live_roots,
        "an empty fixed slot must count in root capacity: {occupy:?}"
    );
    assert_eq!(
        note_value(&ran.trace, 20.0),
        7.0,
        "the scalar is live and is not a table root"
    );
    assert_eq!(occupy.root_slots, ROOT_CAPACITY_SLOTS, "{occupy:?}");
    assert_eq!(
        occupy.live_roots, ROOT_CAPACITY_BATCH,
        "issued tables only; the scalar is not a root slot: {occupy:?}"
    );
    let plan = ran
        .plan
        .unwrap_or_else(|| panic!("decoded plan was not recorded in this scope: {ran:?}"));
    assert!(
        plan.phase.is_none(),
        "the decoded batch was measured before a mark: {plan:?}"
    );
    assert_eq!(
        plan.root_slots, occupy.root_slots,
        "plan {plan:?} occupy {occupy:?}"
    );
    assert_eq!(plan.batch, ROOT_CAPACITY_BATCH, "{plan:?}");
    assert_eq!(plan.planned, plan.root_slots + plan.batch);
    assert_eq!(plan.planned, ROOT_CAPACITY_PLANNED, "{plan:?}");
    assert_ne!(
        ran.roots_limit, 0,
        "the arm must install a real roots limit"
    );
}

/// Exact `roots.len() + batch` still finishes the first automatic empty local.
#[test]
fn exact_automatic_root_capacity_finishes_the_empty_local() {
    let ran = run_auto_roots(AUTOMATIC_ROOT_CAPACITY_SOURCE, ROOT_CAPACITY_PLANNED);
    assert_eq!(ran.stop, Stop::Halted, "{ran:?}");
    assert_same_scope_plan(&ran);
    assert!(bit_value(&ran.trace, 5.0), "fresh was allocated");
    assert!(
        bit_value(&ran.trace, 9.0),
        "the later empty slot was allocated"
    );
    assert_eq!(ran.trace.peeks.len(), 3);
    assert_ne!(ran.trace.peeks[0].table, ran.trace.peeks[1].table);
    assert_eq!(text_value(&ran.trace, 3.0), "table");
    assert_eq!(
        text_value(&ran.trace, 4.0),
        "nil",
        "the automatic cycle clears the independent weak item"
    );
    assert_eq!(note_value(&ran.trace, 8.0), 1.0, "held keeps its identity");
    assert_eq!(
        note_value(&ran.trace, 21.0),
        7.0,
        "the scalar survives the cycle"
    );
    assert_eq!(cycle_at(&ran.trace, 5.0), (false, 1));
    assert_eq!(ran.collects, 1);
    assert_eq!(ran.collections, 1);
    assert!(!ran.tail);
    assert!(ran.mark_phase.is_none());
    assert!(ran.idle);
    assert!(ran.held_alive.unwrap_or(false));
    assert!(ran.trace.steps.is_empty());
    assert!(
        ran.budgets.len() < ROOT_STEP_BUDGETS.len(),
        "the exact arm must actually spend a step allowance: {:?}",
        ran.budgets
    );
}

/// One root short of the decoded batch must refuse before latch, budget, or mark.
#[test]
fn automatic_root_capacity_one_short_keeps_the_idle_heap() {
    let ran = run_auto_roots(AUTOMATIC_ROOT_CAPACITY_SOURCE, ROOT_CAPACITY_PLANNED - 1);
    assert!(
        matches!(ran.stop, Stop::HostError(ref message) if message == "lua heap root capacity"),
        "first refusal must be root capacity, not another resource error: {:?}",
        ran.stop
    );
    assert_same_scope_plan(&ran);
    assert_eq!(ran.roots_limit + 1, ran.plan.expect("plan").planned);
    assert!(
        ran.mark_phase.is_none(),
        "refusal must not leave a saved mark, got {:?} plan {:?}",
        ran.mark_phase,
        ran.plan
    );
    assert!(ran.idle, "fixed step must stay idle");
    assert!(!ran.tail, "refusal must not publish a tail");
    assert!(!ran.meter.requested, "auto_requested {:?}", ran.meter);
    assert!(ran.meter.due, "due must stay true: {:?}", ran.meter);
    assert!(ran.meter.running, "running changed: {:?}", ran.meter);
    assert_eq!(
        ran.meter.threshold as f64,
        note_value(&ran.trace, 31.0),
        "threshold changed: {:?} notes {:?}",
        ran.meter,
        ran.trace.notes
    );
    assert_eq!(
        note_value(&ran.trace, 32.0),
        1.0,
        "due was true before the safepoint"
    );
    assert_eq!(
        note_value(&ran.trace, 33.0),
        0.0,
        "latch was clear before the safepoint"
    );
    assert_eq!(
        note_value(&ran.trace, 34.0),
        1.0,
        "running before the safepoint"
    );
    assert_eq!(
        ran.budgets,
        ROOT_STEP_BUDGETS.to_vec(),
        "both allowances must remain"
    );
    let occupy = occupy_note(&ran.trace);
    assert_eq!(ran.root_slots, occupy.root_slots);
    assert_eq!(ran.live_roots, occupy.live_roots);
    assert_eq!(ran.live_nodes, occupy.live_nodes);
    assert_eq!(ran.node_slots, occupy.node_slots);
    assert_eq!(ran.edge_slots, occupy.edge_slots);
    assert_eq!(ran.mark_passes, occupy.mark_passes);
    assert_eq!(ran.node_slot_visits, occupy.node_slot_visits);
    assert_eq!(ran.parked_root_writes, 0);
    assert_eq!(ran.collects, 0);
    assert_eq!(ran.collections, 0);
    assert_eq!(ran.trace.peeks.len(), 1, "fresh was not built");
    assert_eq!(
        text_value(&ran.trace, 1.0),
        "table",
        "weak item still present"
    );
    assert!(ran.held_alive.unwrap_or(false));
    assert!(ran.trace.steps.is_empty(), "no scan step ran");
}

/// The short roots limit still admits the frame and an ordinary allocation.
/// Stop keeps the non-due path, so this is not the automatic refusal.
#[test]
fn stopped_tight_root_limit_still_allocates_the_empty_local() {
    let source = r#"
local function witness()
  local w = {}
  local mt = {}
  local held = {}
  local scalar = 7
  mt.__mode = "v"
  setmetatable(w, mt)
  w[1] = held
  w[2] = {}
  peek(held)
  note(1, collectgarbage("stop"))
  note(20, scalar)
  meter(30)
  occupy(1)
  text(1, type(rawget(w, 2)))
  local fresh = {}
  bit(5, peek(fresh))
  note(21, scalar)
  local spare = {}
  bit(9, peek(spare))
  return 1
end
witness()
return 1
"#;
    let ran = run_auto_roots(source, ROOT_CAPACITY_PLANNED - 1);
    assert_eq!(ran.stop, Stop::Halted, "{ran:?}");
    let occupy = occupy_note(&ran.trace);
    assert_eq!(occupy.root_slots, ROOT_CAPACITY_SLOTS, "{occupy:?}");
    assert!(occupy.root_slots > occupy.live_roots, "{occupy:?}");
    assert_eq!(note_value(&ran.trace, 20.0), 7.0);
    assert!(
        ran.plan.is_none(),
        "stop is not due, so no automatic batch is decoded"
    );
    assert!(
        bit_value(&ran.trace, 5.0),
        "fresh allocated without a collection"
    );
    assert!(bit_value(&ran.trace, 9.0));
    assert_eq!(text_value(&ran.trace, 1.0), "table");
    assert_eq!(note_value(&ran.trace, 21.0), 7.0);
    assert_eq!(ran.collects, 0);
    assert_eq!(ran.collections, 0);
    assert!(!ran.tail);
    assert!(ran.mark_phase.is_none());
    assert!(ran.idle);
    assert!(!ran.meter.requested);
    assert_eq!(ran.budgets, ROOT_STEP_BUDGETS.to_vec());
    assert_eq!(ran.trace.peeks.len(), 3);
}
