//! Source-level pending argument: a later zero-arg Lua call may collect.
//!
//! The host stores identities and scalars only. It does not retain a carrier.

use std::sync::{Arc, OnceLock};

use vm::{
    CallOutcome, CallReturn, CompileSourceFileOptions, FrontendImportSyntax, FrontendIr,
    HostFunctionRegistry, HostImportSchema, HostOwnedFunction, HostParamPassing, HostParamSchema,
    HostTypeSchema, OwnedHostCall, ParseError, SourceFlavor, SourcePlugin, Value, VmError,
    VmStatus,
};

use super::ObservedTable;
use super::candidate;
use super::heap;

const MARK: &str = "lua_host::mark";
const PEEK: &str = "lua_host::peek";
const CONSUME: &str = "lua_host::consume";
const PIN: &str = "lua_host::pin";
const AFTER: &str = "lua_host::after";
const SNAP: &str = "lua_host::snap";
const NOTE: &str = "lua_host::note";

#[derive(Clone, Debug, Default)]
struct Obs {
    order: Vec<Event>,
    peeked: Vec<ObservedTable>,
    consumed: Vec<Consumed>,
    snaps: Vec<Snap>,
    notes: Vec<(f64, String)>,
}

#[derive(Clone, Debug, PartialEq)]
enum Event {
    Mark(f64),
    Peek,
    Consume,
    Snap(f64),
    Note(f64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Consumed {
    table: heap::TableRef,
    token: vm::ResourceHandle,
    marker: f64,
    live_roots: u32,
    root_slots: u32,
}

#[derive(Clone, Debug, PartialEq)]
struct Snap {
    tag: f64,
    live_roots: u32,
    root_slots: u32,
    live_nodes: u32,
    collections: u32,
    root_slot_visits: u64,
    peeked_alive: Vec<bool>,
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
            MARK,
            vec![HostParamSchema::value("tag", HostTypeSchema::Float)],
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
            CONSUME,
            vec![
                HostParamSchema::with_passing("table", table.clone(), HostParamPassing::Borrow),
                HostParamSchema::value("marker", HostTypeSchema::Float),
            ],
            HostTypeSchema::Float,
        ));
        builder.function(vm::HostFunctionSchema::with_return(
            PIN,
            vec![
                HostParamSchema::with_passing("left", table.clone(), HostParamPassing::Borrow),
                HostParamSchema::with_passing("right", table.clone(), HostParamPassing::Borrow),
                HostParamSchema::value("marker", HostTypeSchema::Float),
            ],
            HostTypeSchema::Float,
        ));
        builder.function(vm::HostFunctionSchema::with_return(
            AFTER,
            vec![
                HostParamSchema::value("marker", HostTypeSchema::Float),
                HostParamSchema::with_passing("table", table, HostParamPassing::Borrow),
            ],
            HostTypeSchema::Float,
        ));
        builder.function(vm::HostFunctionSchema::with_return(
            SNAP,
            vec![HostParamSchema::value("tag", HostTypeSchema::Float)],
            HostTypeSchema::Bool,
        ));
        builder.function(vm::HostFunctionSchema::with_return(
            NOTE,
            vec![
                HostParamSchema::value("tag", HostTypeSchema::Float),
                HostParamSchema::value("text", HostTypeSchema::String),
            ],
            HostTypeSchema::Bool,
        ));
        Arc::new(builder.build().unwrap())
    })
}

fn register(registry: &mut HostFunctionRegistry, catalog: &vm::HostApiCatalog) {
    for (name, op) in [
        (MARK, HostOp::Mark),
        (PEEK, HostOp::Peek),
        (CONSUME, HostOp::Consume),
        (PIN, HostOp::Pin),
        (AFTER, HostOp::After),
        (SNAP, HostOp::Snap),
        (NOTE, HostOp::Note),
    ] {
        let function = catalog.function(name).expect("pending host schema");
        registry
            .register_exact_owned(
                name,
                function.params.len() as u8,
                HostImportSchema::from_function(catalog, function),
                move |_| Box::new(op),
            )
            .expect("register pending host");
    }
}

#[derive(Clone, Copy)]
enum HostOp {
    Mark,
    Peek,
    Consume,
    Pin,
    After,
    Snap,
    Note,
}

impl HostOwnedFunction for HostOp {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        let args = call.args().to_vec();
        let mut host = call.vm().host_context();
        let value = match self {
            Self::Mark => {
                let [Value::Float(tag)] = args.as_slice() else {
                    return Err(host_fault("expected stage tag"));
                };
                let tag = *tag;
                note(&mut host, |state| state.order.push(Event::Mark(tag)))?;
                Value::Float(tag)
            }
            Self::Peek => {
                let observed = borrowed_table(&mut host, &args[0])?;
                let alive = heap::with_heap(&mut host, |heap| Ok(heap.alive(observed.table)))
                    .map_err(|error| VmError::HostError(error.to_string()))?;
                note(&mut host, |state| {
                    state.order.push(Event::Peek);
                    state.peeked.push(observed);
                })?;
                Value::Bool(alive)
            }
            Self::Consume | Self::Pin => {
                let observed = borrowed_table(&mut host, &args[0])?;
                let marker = match args.last() {
                    Some(Value::Float(marker)) => *marker,
                    _ => return Err(host_fault("expected marker")),
                };
                let (live_roots, root_slots) = heap::with_heap(&mut host, |heap| {
                    let stats = heap.stats();
                    Ok((stats.live_roots, stats.root_slots))
                })
                .map_err(|error| VmError::HostError(error.to_string()))?;
                note(&mut host, |state| {
                    state.order.push(Event::Consume);
                    state.consumed.push(Consumed {
                        table: observed.table,
                        token: observed.token,
                        marker,
                        live_roots,
                        root_slots,
                    });
                })?;
                Value::Float(marker)
            }
            Self::After => {
                let marker = match args.first() {
                    Some(Value::Float(marker)) => *marker,
                    _ => return Err(host_fault("expected marker")),
                };
                let observed = borrowed_table(&mut host, &args[1])?;
                note(&mut host, |state| {
                    state.order.push(Event::Consume);
                    state.consumed.push(Consumed {
                        table: observed.table,
                        token: observed.token,
                        marker,
                        live_roots: 0,
                        root_slots: 0,
                    });
                })?;
                Value::Float(marker)
            }
            Self::Snap => {
                let [Value::Float(tag)] = args.as_slice() else {
                    return Err(host_fault("expected snap tag"));
                };
                let tag = *tag;
                let peeked = note(&mut host, |state| state.peeked.clone())?;
                let (stats, visits) = heap::with_heap(&mut host, |heap| {
                    Ok((heap.stats(), heap.take_scan_meter().root_slot_visits))
                })
                .map_err(|error| VmError::HostError(error.to_string()))?;
                let alive = peeked
                    .iter()
                    .map(|item| {
                        heap::with_heap(&mut host, |heap| Ok(heap.alive(item.table)))
                            .unwrap_or(false)
                    })
                    .collect();
                note(&mut host, |state| {
                    state.order.push(Event::Snap(tag));
                    state.snaps.push(Snap {
                        tag,
                        live_roots: stats.live_roots,
                        root_slots: stats.root_slots,
                        live_nodes: stats.live_nodes,
                        collections: stats.collections,
                        root_slot_visits: visits,
                        peeked_alive: alive,
                    });
                })?;
                Value::Bool(true)
            }
            Self::Note => {
                let [Value::Float(tag), Value::String(text)] = args.as_slice() else {
                    return Err(host_fault("expected note"));
                };
                let noted = (*tag, text.to_string());
                note(&mut host, |state| {
                    state.order.push(Event::Note(noted.0));
                    state.notes.push(noted);
                })?;
                Value::Bool(true)
            }
        };
        Ok(CallOutcome::Return(CallReturn::one(value)))
    }
}

fn note<T>(host: &mut vm::HostContext<'_>, body: impl FnOnce(&mut Obs) -> T) -> Result<T, VmError> {
    let state = host
        .scope_state_or_insert_with(Obs::default)
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

struct Ran {
    status: Result<VmStatus, String>,
    stack: Vec<Value>,
    obs: Obs,
    live_roots: u32,
    root_slots: u32,
    collections: u32,
    visits: Vec<u64>,
    returned: Option<heap::TableRef>,
    discarded: Option<heap::TableRef>,
    issued: usize,
}

fn run_source(source: &str) -> Ran {
    run_with(source, None)
}

fn run_with(source: &str, roots: Option<u32>) -> Ran {
    let catalog = Arc::clone(catalog());
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&Plugin)
        .with_host_api_catalog(Arc::clone(&catalog));
    let prepared = vm::compile_source_with_flavor_and_options(source, SourceFlavor::Lua, options)
        .unwrap_or_else(|error| panic!("compile rejected the source: {error}"));
    let mut vm = prepared.into_vm().expect("create VM");
    if let Some(roots) = roots {
        heap::install(
            &mut vm.host_context(),
            heap::HeapLimits {
                nodes: 4096,
                roots,
                edges: 8192,
                max_generation: u32::MAX,
            },
        )
        .unwrap_or_else(|error| panic!("install: {error}"));
    }
    let mut registry = HostFunctionRegistry::empty();
    candidate::register(&mut registry, &catalog).expect("register candidate runtime");
    register(&mut registry, &catalog);
    registry.bind_vm_cached(&mut vm).expect("bind runtime");
    vm.host_context()
        .scope_state_or_insert_with(candidate::VisitSamples::default)
        .expect("visit samples");
    let status = vm.run().map_err(|error| error.to_string());
    let obs = vm
        .host_context()
        .scope_state::<Obs>()
        .cloned()
        .unwrap_or_default();
    let (live_roots, root_slots, collections) = heap::with_heap(&mut vm.host_context(), |heap| {
        let stats = heap.stats();
        Ok((stats.live_roots, stats.root_slots, stats.collections))
    })
    .unwrap_or((u32::MAX, u32::MAX, u32::MAX));
    let visits = vm
        .host_context()
        .scope_state::<candidate::VisitSamples>()
        .map(|samples| samples.visits.clone())
        .unwrap_or_default();
    let (returned, discarded, issued) = vm
        .host_context()
        .scope_state::<candidate::ReturnWitness>()
        .map(|witness| (witness.returned, witness.discarded, witness.issued.len()))
        .unwrap_or((None, None, 0));
    let stack = vm.stack().to_vec();
    Ran {
        status,
        stack,
        obs,
        live_roots,
        root_slots,
        collections,
        visits,
        returned,
        discarded,
        issued,
    }
}

const SEEDED: &str = r#"
local function seed()
  local weak = {}
  local mode = {}
  mode.__mode = "v"
  setmetatable(weak, mode)
  local target = {}
  target.self = target
  rawset(weak, 1, target)
  local orphan = {}
  orphan.self = orphan
  rawset(weak, 2, orphan)
  return weak
end
local weak = seed()
local function sweep()
  mark(2)
  collectgarbage("collect")
  snap(2)
  mark(41)
  return 41
end
peek(rawget(weak, 1))
peek(rawget(weak, 2))
local marker = consume(rawget(weak, (mark(1))), sweep())
snap(4)
note(6, type(rawget(weak, 1)))
note(7, type(rawget(weak, 2)))
collectgarbage("collect")
snap(5)
note(8, type(rawget(weak, 1)))
return marker
"#;

#[test]
fn pending_argument_value_call_returns_the_marker() {
    let ran = run_source(
        r#"
local function sweep()
  return 41
end
return sweep()
"#,
    );
    assert_eq!(ran.status, Ok(VmStatus::Halted), "{ran:?}");
    assert_eq!(ran.stack, vec![Value::Float(41.0)], "{ran:?}");
}

#[test]
fn pending_argument_keeps_the_earlier_table_across_the_later_lua_collect() {
    let ran = run_source(SEEDED);
    assert_eq!(
        ran.status,
        Ok(VmStatus::Halted),
        "status={:?} stack={:?} order={:?} snaps={:?} notes={:?} collections={} roots={}/{}",
        ran.status,
        ran.stack,
        ran.obs.order,
        ran.obs.snaps,
        ran.obs.notes,
        ran.collections,
        ran.live_roots,
        ran.root_slots,
    );
    assert_eq!(ran.stack, vec![Value::Float(41.0)]);
    assert_eq!(
        ran.obs.order,
        vec![
            Event::Peek,
            Event::Peek,
            Event::Mark(1.0),
            Event::Mark(2.0),
            Event::Snap(2.0),
            Event::Mark(41.0),
            Event::Consume,
            Event::Snap(4.0),
            Event::Note(6.0),
            Event::Note(7.0),
            Event::Snap(5.0),
            Event::Note(8.0),
        ]
    );
    let target = ran.obs.peeked[0];
    let orphan = ran.obs.peeked[1];
    assert_ne!(target.table, orphan.table);
    let consumed = ran
        .obs
        .consumed
        .first()
        .expect("consumer stored the identity");
    assert_eq!(consumed.table, target.table);
    assert_eq!(consumed.token, target.token);
    assert_eq!(consumed.marker, 41.0);
    assert_eq!(
        consumed.root_slots, 2,
        "caller frame plus the pending frame"
    );
    assert_eq!(consumed.live_roots, 2, "weak table and the pending table");
    let nested = snap(&ran, 2.0);
    let released = snap(&ran, 4.0);
    let later = snap(&ran, 5.0);
    assert_eq!(nested.peeked_alive, vec![true, false]);
    assert_eq!(nested.root_slots, 2);
    assert_eq!(nested.live_roots, 2);
    assert_eq!(nested.collections, 1);
    assert_eq!(
        nested.root_slot_visits, 0,
        "return witness already took this meter"
    );
    assert_eq!(
        ran.visits,
        vec![4, 2],
        "nested and later collects stay separate"
    );
    assert_ne!(ran.visits[0], nested.root_slots as u64);
    assert_ne!(ran.visits[0], nested.live_roots as u64);
    assert_eq!(
        released.root_slots, 1,
        "pending frame left before the next snap"
    );
    assert_eq!(released.live_roots, 1);
    assert_eq!(released.peeked_alive, vec![true, false]);
    assert_eq!(later.peeked_alive, vec![false, false]);
    assert_eq!(later.collections, 2);
    assert_eq!(later.root_slots, 1);
    assert_eq!(later.root_slot_visits, 0);
    assert_ne!(ran.visits[0], ran.visits[1]);
    assert_eq!(
        ran.obs.notes,
        vec![
            (6.0, "table".to_string()),
            (7.0, "nil".to_string()),
            (8.0, "nil".to_string()),
        ]
    );
    assert_eq!(ran.live_roots, 0, "chunk retire returned the caller frame");
    assert_eq!(ran.root_slots, 0);
    assert_eq!(
        ran.issued, 4,
        "pending publish did not issue another witness root"
    );
    assert_ne!(ran.returned, Some(target.table));
    assert_ne!(ran.discarded, Some(target.table));
}

#[test]
fn pending_argument_repeat_reuses_the_returned_frame() {
    let ran = run_source(
        r#"
local function seed()
  local weak = {}
  local mode = {}
  mode.__mode = "v"
  setmetatable(weak, mode)
  local target = {}
  target.self = target
  rawset(weak, 1, target)
  return weak
end
local weak = seed()
local function sweep()
  collectgarbage("collect")
  return 41
end
local first = consume(rawget(weak, 1), sweep())
local second = consume(rawget(weak, 1), sweep())
return first + second
"#,
    );
    assert_eq!(ran.status, Ok(VmStatus::Halted), "{ran:?}");
    assert_eq!(ran.stack, vec![Value::Float(82.0)], "{ran:?}");
    assert_eq!(ran.obs.consumed.len(), 2);
    assert_eq!(ran.obs.consumed[0].table, ran.obs.consumed[1].table);
    assert_eq!(ran.obs.consumed[0].root_slots, 2);
    assert_eq!(
        ran.obs.consumed[1].root_slots, 2,
        "the first frame was returned"
    );
    assert_eq!(ran.root_slots, 0);
}

#[test]
fn pending_argument_omits_the_root_without_a_nested_safepoint() {
    let ran = run_source(
        r#"
local function seed()
  local weak = {}
  local mode = {}
  mode.__mode = "v"
  setmetatable(weak, mode)
  local target = {}
  target.self = target
  rawset(weak, 1, target)
  return weak
end
local weak = seed()
local marker = consume(rawget(weak, 1), 41)
note(9, type(rawget(weak, 1)))
return marker
"#,
    );
    assert_eq!(ran.status, Ok(VmStatus::Halted), "{ran:?}");
    assert_eq!(ran.stack, vec![Value::Float(41.0)]);
    assert_eq!(
        ran.obs.consumed[0].root_slots, 1,
        "scalar arg added no frame"
    );
    assert_eq!(ran.obs.notes, vec![(9.0, "table".to_string())]);
    assert!(ran.visits.is_empty());
}

#[test]
fn pending_argument_statement_sweep_collects_without_a_pending_root() {
    let ran = run_source(
        r#"
local function seed()
  local weak = {}
  local mode = {}
  mode.__mode = "v"
  setmetatable(weak, mode)
  local target = {}
  target.self = target
  rawset(weak, 1, target)
  local orphan = {}
  orphan.self = orphan
  rawset(weak, 2, orphan)
  return weak
end
local weak = seed()
peek(rawget(weak, 1))
peek(rawget(weak, 2))
local function sweep()
  collectgarbage("collect")
  snap(2)
  return 41
end
sweep()
note(9, type(rawget(weak, 1)))
return 41
"#,
    );
    assert_eq!(ran.status, Ok(VmStatus::Halted), "{ran:?}");
    assert_eq!(ran.stack, vec![Value::Float(41.0)]);
    assert!(ran.obs.consumed.is_empty());
    assert_eq!(snap(&ran, 2.0).peeked_alive, vec![false, false]);
    assert_eq!(snap(&ran, 2.0).root_slots, 1);
    assert_eq!(ran.visits, vec![2]);
    assert_eq!(ran.obs.notes, vec![(9.0, "nil".to_string())]);
}

#[test]
fn pending_argument_reverse_get_is_already_collected() {
    let ran = run_source(
        r#"
local function seed()
  local weak = {}
  local mode = {}
  mode.__mode = "v"
  setmetatable(weak, mode)
  local target = {}
  target.self = target
  rawset(weak, 1, target)
  return weak
end
local weak = seed()
peek(rawget(weak, 1))
local function sweep()
  collectgarbage("collect")
  snap(2)
  return 41
end
return after(sweep(), rawget(weak, 1))
"#,
    );
    assert!(ran.status.is_err(), "{ran:?}");
    assert!(ran.obs.consumed.is_empty(), "{ran:?}");
    assert_eq!(snap(&ran, 2.0).peeked_alive, vec![false]);
    assert_eq!(snap(&ran, 2.0).root_slots, 1);
    assert_eq!(ran.visits, vec![2]);
    assert_eq!(ran.root_slots, 1, "failed get did not keep a pending frame");
}

#[test]
fn pending_argument_rejects_a_forged_carrier() {
    let ran = run_source(
        r#"
local function sweep()
  mark(2)
  return 41
end
return consume(7, sweep())
"#,
    );
    assert!(ran.status.is_err(), "{ran:?}");
    assert!(ran.obs.order.is_empty(), "the later call must not start");
    assert_eq!(ran.root_slots, 0);
    assert!(ran.visits.is_empty());
}

#[test]
fn pending_argument_capacity_diverges_only_at_the_pending_frame() {
    let source = r#"
local function seed()
  local weak = {}
  rawset(weak, 1, weak)
  return weak
end
local weak = seed()
local function sweep()
  return 41
end
return pin(rawget(weak, 1), rawget(weak, 1), sweep())
"#;
    let tight = run_with(source, Some(2));
    assert!(tight.status.is_err(), "{tight:?}");
    assert!(tight.obs.consumed.is_empty(), "{tight:?}");
    assert_eq!(tight.root_slots, 1, "admission failure must not grow slots");
    assert!(tight.visits.is_empty());
    let wide = run_with(source, Some(3));
    assert_eq!(wide.status, Ok(VmStatus::Halted), "{wide:?}");
    assert_eq!(wide.stack, vec![Value::Float(41.0)]);
    assert_eq!(wide.obs.consumed[0].root_slots, 3);
    assert_eq!(wide.root_slots, 0);
}

fn snap(ran: &Ran, tag: f64) -> &Snap {
    ran.obs
        .snaps
        .iter()
        .find(|snap| snap.tag == tag)
        .unwrap_or_else(|| panic!("missing snap {tag}"))
}

impl std::fmt::Debug for Ran {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Ran")
            .field("status", &self.status)
            .field("stack", &self.stack)
            .field("order", &self.obs.order)
            .field("snaps", &self.obs.snaps)
            .field("notes", &self.obs.notes)
            .field("consumed", &self.obs.consumed)
            .field("collections", &self.collections)
            .field("live_roots", &self.live_roots)
            .field("root_slots", &self.root_slots)
            .field("visits", &self.visits)
            .field("issued", &self.issued)
            .finish()
    }
}
