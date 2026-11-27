//! Equal named-field assignment with parenthesized static calls.
//!
//! Lua text goes through the candidate SourcePlugin, Core, and the VM.
//! Parentheses keep one result. A function still cannot return a boolean or
//! string through the existing pass-through; those literals are written as
//! earlier values. The collect witness stores identities, not carriers.

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

const PEEK: &str = "lua_host::peek";
const SNAP: &str = "lua_host::snap";
const NOTE: &str = "lua_host::note";
const EDGE: &str = "lua_host::edge";
const ARM: &str = "lua_host::arm";
const TOUCH: &str = "lua_host::touch";

#[derive(Clone, Debug, Default)]
struct Obs {
    peeked: Vec<ObservedTable>,
    snaps: Vec<Snap>,
    notes: Vec<(f64, String)>,
    edges: Vec<Option<heap::TableRef>>,
    /// Table a no-arg function can stamp. Not a heap root.
    armed: Option<heap::TableRef>,
}

#[derive(Clone, Debug, PartialEq)]
struct Snap {
    tag: f64,
    live_roots: u32,
    root_slots: u32,
    collections: u32,
    resources: usize,
    root_slot_visits: u64,
    peeked_alive: Vec<bool>,
    token_open: Vec<bool>,
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
            PEEK,
            vec![HostParamSchema::with_passing(
                "table",
                table.clone(),
                HostParamPassing::Borrow,
            )],
            HostTypeSchema::Bool,
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
        builder.function(vm::HostFunctionSchema::with_return(
            EDGE,
            vec![
                HostParamSchema::with_passing("table", table.clone(), HostParamPassing::Borrow),
                HostParamSchema::value("key", HostTypeSchema::String),
            ],
            HostTypeSchema::Bool,
        ));
        builder.function(vm::HostFunctionSchema::with_return(
            ARM,
            vec![HostParamSchema::with_passing(
                "table",
                table,
                HostParamPassing::Borrow,
            )],
            HostTypeSchema::Bool,
        ));
        builder.function(vm::HostFunctionSchema::with_return(
            TOUCH,
            vec![],
            HostTypeSchema::Bool,
        ));
        Arc::new(builder.build().unwrap())
    })
}

#[derive(Clone, Copy)]
enum HostOp {
    Peek,
    Snap,
    Note,
    Edge,
    Arm,
    Touch,
}

impl HostOwnedFunction for HostOp {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        let args = call.args().to_vec();
        let mut host = call.vm().host_context();
        let value = match self {
            Self::Peek => {
                let observed = borrowed_table(&mut host, &args[0])?;
                let alive = heap::with_heap(&mut host, |heap| Ok(heap.alive(observed.table)))
                    .map_err(|error| VmError::HostError(error.to_string()))?;
                note(&mut host, |state| state.peeked.push(observed))?;
                Value::Bool(alive)
            }
            Self::Snap => {
                let [Value::Float(tag)] = args.as_slice() else {
                    return Err(host_fault("expected snap tag"));
                };
                let tag = *tag;
                let peeked = host
                    .scope_state::<Obs>()
                    .map(|state| state.peeked.clone())
                    .unwrap_or_default();
                let (stats, visits) = heap::with_heap(&mut host, |heap| {
                    Ok((heap.stats(), heap.take_scan_meter().root_slot_visits))
                })
                .map_err(|error| VmError::HostError(error.to_string()))?;
                let key = heap::LuaTableResource::key();
                let mut peeked_alive = Vec::with_capacity(peeked.len());
                let mut token_open = Vec::with_capacity(peeked.len());
                for item in &peeked {
                    let alive = heap::with_heap(&mut host, |heap| Ok(heap.alive(item.table)))
                        .unwrap_or(false);
                    let open = host
                        .borrow_resource_with_key::<heap::LuaTableResource>(item.token, &key)
                        .is_ok();
                    peeked_alive.push(alive);
                    token_open.push(open);
                }
                let resources = host.resource_count();
                note(&mut host, |state| {
                    state.snaps.push(Snap {
                        tag,
                        live_roots: stats.live_roots,
                        root_slots: stats.root_slots,
                        collections: stats.collections,
                        resources,
                        root_slot_visits: visits,
                        peeked_alive,
                        token_open,
                    });
                })?;
                Value::Bool(true)
            }
            Self::Note => {
                let [Value::Float(tag), Value::String(text)] = args.as_slice() else {
                    return Err(host_fault("expected note"));
                };
                let noted = (*tag, text.to_string());
                note(&mut host, |state| state.notes.push(noted))?;
                Value::Bool(true)
            }
            Self::Edge => {
                let observed = borrowed_table(&mut host, &args[0])?;
                let Some(Value::String(key)) = args.get(1) else {
                    return Err(host_fault("expected edge key"));
                };
                let found = heap::with_heap(&mut host, |heap| heap.named_edge(observed.table, key))
                    .map_err(|error| VmError::HostError(error.to_string()))?;
                note(&mut host, |state| state.edges.push(found))?;
                Value::Bool(found.is_some())
            }
            Self::Arm => {
                let observed = borrowed_table(&mut host, &args[0])?;
                note(&mut host, |state| state.armed = Some(observed.table))?;
                Value::Bool(true)
            }
            Self::Touch => {
                let armed = host
                    .scope_state::<Obs>()
                    .and_then(|state| state.armed)
                    .ok_or_else(|| host_fault("touch without arm"))?;
                heap::with_heap(&mut host, |heap| {
                    heap.set_named_scalar(armed, "step", Value::string("called"))
                })
                .map_err(|error| VmError::HostError(error.to_string()))?;
                Value::Bool(true)
            }
        };
        Ok(CallOutcome::Return(CallReturn::one(value)))
    }
}

fn register(registry: &mut HostFunctionRegistry, catalog: &vm::HostApiCatalog) {
    for (name, op) in [
        (PEEK, HostOp::Peek),
        (SNAP, HostOp::Snap),
        (NOTE, HostOp::Note),
        (EDGE, HostOp::Edge),
        (ARM, HostOp::Arm),
        (TOUCH, HostOp::Touch),
    ] {
        let function = catalog.function(name).expect("assignment root host schema");
        registry
            .register_exact_owned(
                name,
                function.params.len() as u8,
                HostImportSchema::from_function(catalog, function),
                move |_| Box::new(op),
            )
            .expect("register assignment root host");
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
    resources: usize,
    visits: Vec<u64>,
}

fn run_source(source: &str) -> Ran {
    let catalog = Arc::clone(catalog());
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&Plugin)
        .with_host_api_catalog(Arc::clone(&catalog));
    let prepared = vm::compile_source_with_flavor_and_options(source, SourceFlavor::Lua, options)
        .unwrap_or_else(|error| panic!("compile rejected the source: {error}"));
    let mut vm = prepared.into_vm().expect("create VM");
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
    let resources = vm.host_context().resource_count();
    let visits = vm
        .host_context()
        .scope_state::<candidate::VisitSamples>()
        .map(|samples| samples.visits.clone())
        .unwrap_or_default();
    let stack = vm.stack().to_vec();
    Ran {
        status,
        stack,
        obs,
        resources,
        visits,
    }
}

fn snap(ran: &Ran, tag: f64) -> &Snap {
    ran.obs
        .snaps
        .iter()
        .find(|snap| snap.tag == tag)
        .unwrap_or_else(|| panic!("missing snap {tag}: {:?}", ran.obs.snaps))
}

fn bool_chunk(source: &str) -> Vec<Value> {
    let mut vm =
        super::try_candidate(source).unwrap_or_else(|error| panic!("lower Lua text: {error}"));
    assert_eq!(vm.run().unwrap(), VmStatus::Halted);
    vm.stack().to_vec()
}

#[test]
fn trailing_parenthesized_call_keeps_the_saved_left_value() {
    let stack = bool_chunk(
        r#"local function sweep()
  local made = {}
  return made
end
local t = {}
local marker = {}
t.a, t.a = marker, (sweep())
return rawequal(t.a, marker)
"#,
    );
    assert_eq!(stack, vec![Value::Bool(true)]);
}

#[test]
fn double_parentheses_still_keep_one_call_result() {
    let stack = bool_chunk(
        r#"local function sweep()
  local made = {}
  return made
end
local t = {}
local u = {}
t.a, u.b = "left", ((sweep()))
return type(u.b)
"#,
    );
    assert_eq!(stack, vec![Value::string("table")]);
}

#[test]
fn call_shape_stores_string_and_boolean_literals() {
    let stack = bool_chunk(
        r#"local function sweep()
  return nil
end
local t = {}
local u = {}
t.a, u.b = "kept", (sweep())
t.c, u.d = true, (sweep())
return rawequal(t.a, "kept")
"#,
    );
    assert_eq!(stack, vec![Value::Bool(true)]);
    let flag = bool_chunk(
        r#"local function sweep()
  return nil
end
local t = {}
t.c, t.d = true, (sweep())
return rawequal(t.c, true)
"#,
    );
    assert_eq!(flag, vec![Value::Bool(true)]);
}

#[test]
fn nil_before_the_call_does_not_block_the_result() {
    let stack = bool_chunk(
        r#"local function sweep()
  local made = {}
  return made
end
local t = {}
local u = {}
t.a, u.b = nil, (sweep())
return type(u.b)
"#,
    );
    assert_eq!(stack, vec![Value::string("table")]);
}

#[test]
fn function_boolean_return_is_not_a_pass_through_value() {
    let mut vm = super::try_candidate(
        r#"local function sweep()
  local flag = false
  return flag
end
local t = {}
t.a, t.b = true, (sweep())
return rawequal(t.b, false)
"#,
    )
    .unwrap_or_else(|error| panic!("lower Lua text: {error}"));
    let error = vm.run().expect_err("bool return is not pass-through");
    assert!(
        error
            .to_string()
            .contains("return element is not a pass-through value"),
        "{error}"
    );
}

#[test]
fn function_string_return_is_not_a_pass_through_value() {
    let mut vm = super::try_candidate(
        r#"local function sweep()
  local text = "swept"
  return text
end
local t = {}
t.a, t.b = "left", (sweep())
return rawequal(t.b, "swept")
"#,
    )
    .unwrap_or_else(|error| panic!("lower Lua text: {error}"));
    let error = vm.run().expect_err("string return is not pass-through");
    assert!(
        error
            .to_string()
            .contains("return element is not a pass-through value"),
        "{error}"
    );
}

#[test]
fn unparenthesized_call_stays_unlowered() {
    let error = match super::try_candidate(
        r#"local function sweep()
  return nil
end
local t = {}
local u = {}
t.a, u.b = true, sweep()
return true
"#,
    ) {
        Err(error) => error,
        Ok(_) => panic!("bare call is outside this slice"),
    };
    assert!(error.contains("Lua expression not lowered yet"), "{error}");
}

#[test]
fn early_call_then_plain_rhs_observes_evaluation_and_write_order() {
    // A local function cannot capture the log table, so the host stamps it.
    // The stamp is not a heap root. The later field read must see it, and
    // the overlapping left write must beat the right-hand marker.
    let ran = run_source(
        r#"local log = {}
log.step = "start"
arm(log)
local function first()
  touch()
  local made = {}
  return made
end
local dest = {}
local marker = {}
dest.slot, dest.seen, dest.slot = (first()), log.step, marker
note(1, dest.seen)
peek(marker)
edge(dest, "slot")
return true
"#,
    );
    assert_eq!(ran.status.as_ref().map(|_| ()), Ok(()), "{:?}", ran.status);
    assert_eq!(ran.obs.notes, vec![(1.0, "called".to_string())]);
    let marker = ran.obs.peeked[0];
    let stored = ran.obs.edges[0].expect("left write kept a table");
    assert_ne!(
        stored, marker.table,
        "right-to-left write leaves the call result"
    );
}

#[test]
fn first_call_table_survives_second_call_collect() {
    let ran = run_source(
        r#"local function seed()
  local weak = {}
  local mode = {}
  mode.__mode = "v"
  setmetatable(weak, mode)
  local orphan = {}
  orphan.self = orphan
  rawset(weak, 2, orphan)
  return weak
end
local function fresh()
  local made = {}
  return made
end
local weak = seed()
local a = fresh()
local b = fresh()
local lost = rawget(weak, 2)
peek(lost)
lost = nil
local function first()
  local made = {}
  made.self = made
  peek(made)
  return made
end
local function sweep()
  collectgarbage("collect")
  snap(2)
  return nil
end
snap(1)
a.field, b.field = (first()), (sweep())
snap(3)
edge(a, "field")
note(1, type(a.field))
note(2, type(b.field))
rawset(weak, 1, a.field)
collectgarbage("collect")
snap(4)
note(3, type(rawget(weak, 1)))
a.field = nil
collectgarbage("collect")
snap(5)
note(4, type(rawget(weak, 1)))
return true
"#,
    );
    assert_eq!(ran.status.as_ref().map(|_| ()), Ok(()), "{:?}", ran.status);
    let before = snap(&ran, 1.0);
    let during = snap(&ran, 2.0);
    let after = snap(&ran, 3.0);
    let held = snap(&ran, 4.0);
    let dropped = snap(&ran, 5.0);
    let target = ran.obs.peeked[1];
    assert_eq!(during.collections, 1);
    assert_eq!(during.peeked_alive, vec![false, true]);
    assert_eq!(during.token_open, vec![false, true]);
    assert_eq!(during.root_slots, before.root_slots + 3);
    assert_eq!(during.live_roots, before.live_roots + 3);
    assert_eq!(after.root_slots, before.root_slots);
    assert_eq!(after.live_roots, before.live_roots);
    assert_eq!(ran.obs.edges, vec![Some(target.table)]);
    assert_eq!(
        ran.obs.notes,
        vec![
            (1.0, "table".to_string()),
            (2.0, "nil".to_string()),
            (3.0, "table".to_string()),
            (4.0, "nil".to_string()),
        ]
    );
    assert!(
        held.token_open[1],
        "written edge keeps the target: {held:?}"
    );
    assert!(
        !dropped.token_open[1],
        "next collect reclaims the target: {dropped:?}"
    );
    assert!(
        !dropped.token_open[0],
        "orphan stays collected: {dropped:?}"
    );
    assert_eq!(dropped.collections, 3);
    assert_eq!(ran.visits.len(), 3, "three real collects: {:?}", ran.visits);
}

#[test]
fn evaluated_table_between_calls_survives_the_later_collect() {
    let ran = run_source(
        r#"local function seed()
  local weak = {}
  local mode = {}
  mode.__mode = "v"
  setmetatable(weak, mode)
  local orphan = {}
  orphan.self = orphan
  rawset(weak, 2, orphan)
  local mid = {}
  mid.self = mid
  rawset(weak, 3, mid)
  return weak
end
local function fresh()
  local made = {}
  return made
end
local weak = seed()
local a = fresh()
local b = fresh()
local c = fresh()
local mid = rawget(weak, 3)
local lost = rawget(weak, 2)
peek(mid)
peek(lost)
lost = nil
local function first()
  local made = {}
  made.self = made
  peek(made)
  return made
end
local function sweep()
  collectgarbage("collect")
  snap(2)
  return nil
end
snap(1)
a.field, b.field, c.field = (first()), mid, (sweep())
snap(3)
edge(a, "field")
edge(b, "field")
note(1, type(a.field))
note(2, type(b.field))
note(3, type(c.field))
return true
"#,
    );
    assert_eq!(ran.status.as_ref().map(|_| ()), Ok(()), "{:?}", ran.status);
    let before = snap(&ran, 1.0);
    let during = snap(&ran, 2.0);
    let mid = ran.obs.peeked[0];
    let target = ran.obs.peeked[2];
    assert_eq!(during.collections, 1);
    assert_eq!(during.peeked_alive, vec![true, false, true]);
    assert_eq!(during.token_open, vec![true, false, true]);
    assert_eq!(during.root_slots, before.root_slots + 5);
    assert_eq!(during.live_roots, before.live_roots + 5);
    assert_eq!(ran.obs.edges, vec![Some(target.table), Some(mid.table)]);
    assert_eq!(
        ran.obs.notes,
        vec![
            (1.0, "table".to_string()),
            (2.0, "table".to_string()),
            (3.0, "nil".to_string()),
        ]
    );
    assert_eq!(snap(&ran, 3.0).root_slots, before.root_slots);
}

#[test]
fn scalar_or_nil_between_calls_is_not_a_root() {
    let nil_between = r#"local function fresh()
  local made = {}
  return made
end
local function first()
  return nil
end
local function sweep()
  collectgarbage("collect")
  snap(2)
  return nil
end
local a = fresh()
local b = fresh()
local c = fresh()
snap(1)
a.field, b.field, c.field = (first()), nil, (sweep())
snap(3)
note(1, type(a.field))
note(2, type(b.field))
note(3, type(c.field))
return true
"#;
    let ran = run_source(nil_between);
    assert_eq!(ran.status.as_ref().map(|_| ()), Ok(()), "{:?}", ran.status);
    let before = snap(&ran, 1.0);
    let during = snap(&ran, 2.0);
    assert_eq!(during.root_slots, before.root_slots + 5);
    assert_eq!(during.live_roots, before.live_roots + 3);
    assert_eq!(
        ran.obs.notes,
        vec![
            (1.0, "nil".to_string()),
            (2.0, "nil".to_string()),
            (3.0, "nil".to_string()),
        ]
    );

    let kept = nil_between.replace(
        "(first()), nil, (sweep())",
        "(first()), \"kept\", (sweep())",
    );
    let ran = run_source(&kept);
    assert_eq!(ran.status.as_ref().map(|_| ()), Ok(()), "{:?}", ran.status);
    assert_eq!(snap(&ran, 2.0).live_roots, snap(&ran, 1.0).live_roots + 3);
    assert_eq!(ran.obs.notes[1], (2.0, "string".to_string()));

    let flag = nil_between.replace("(first()), nil, (sweep())", "(first()), true, (sweep())");
    let ran = run_source(&flag);
    assert_eq!(ran.status.as_ref().map(|_| ()), Ok(()), "{:?}", ran.status);
    assert_eq!(snap(&ran, 2.0).live_roots, snap(&ran, 1.0).live_roots + 3);
    assert_eq!(ran.obs.notes[1], (2.0, "boolean".to_string()));

    let early = r#"local function fresh()
  local made = {}
  return made
end
local function first()
  return nil
end
local function sweep()
  collectgarbage("collect")
  snap(2)
  return nil
end
local a = fresh()
local b = fresh()
local c = fresh()
local d = fresh()
snap(1)
a.field, b.field, c.field, d.field = "early", (first()), nil, (sweep())
snap(3)
note(1, a.field)
note(2, type(c.field))
return true
"#;
    let ran = run_source(early);
    assert_eq!(ran.status.as_ref().map(|_| ()), Ok(()), "{:?}", ran.status);
    let before = snap(&ran, 1.0);
    let during = snap(&ran, 2.0);
    assert_eq!(during.root_slots, before.root_slots + 7);
    assert_eq!(during.live_roots, before.live_roots + 4);
    assert_eq!(ran.obs.notes[0], (1.0, "early".to_string()));
    assert_eq!(ran.obs.notes[1], (2.0, "nil".to_string()));
}

#[test]
fn no_call_assignment_does_not_open_a_root_frame() {
    let ran = run_source(
        r#"local t = {}
snap(1)
t.a, t.b = true, "x"
snap(2)
return rawequal(t.a, true)
"#,
    );
    assert_eq!(ran.status.as_ref().map(|_| ()), Ok(()), "{:?}", ran.status);
    assert_eq!(ran.stack, vec![Value::Bool(true)]);
    let before = snap(&ran, 1.0);
    let after = snap(&ran, 2.0);
    assert_eq!(before.root_slots, after.root_slots);
    assert_eq!(before.collections, 0);
    assert_eq!(after.collections, 0);
    assert!(ran.visits.is_empty());
}

fn seeded(call: &str, early: &str) -> String {
    format!(
        r#"local function seed()
  local weak = {{}}
  local mode = {{}}
  mode.__mode = "v"
  setmetatable(weak, mode)
  local target = {{}}
  target.self = target
  rawset(weak, 1, target)
  local orphan = {{}}
  orphan.self = orphan
  rawset(weak, 2, orphan)
  return weak
end
local weak = seed()
local dest = {{}}
local other = {{}}
local early = {early}
local lost = rawget(weak, 2)
local function sweep()
  collectgarbage("collect")
  snap(2)
  return nil
end
peek(early)
peek(lost)
snap(1)
dest.kept, dest.kept, other.mid, other.flag = early, nil, true, {call}
snap(3)
note(1, type(dest.kept))
note(2, type(other.mid))
note(3, type(other.flag))
note(4, type(rawget(weak, 2)))
edge(dest, "kept")
early = nil
lost = nil
dest.kept = nil
collectgarbage("collect")
snap(4)
note(5, type(rawget(weak, 1)))
return rawequal(type(other.mid), "boolean")
"#
    )
}

fn assert_collect_witness(ran: &Ran, early_is_target: bool) {
    assert_eq!(
        ran.status.as_ref().map(|_| ()),
        Ok(()),
        "status={:?} stack={:?} notes={:?} snaps={:?}",
        ran.status,
        ran.stack,
        ran.obs.notes,
        ran.obs.snaps
    );
    assert_eq!(ran.stack, vec![Value::Bool(true)]);
    let target = ran.obs.peeked[0];
    let orphan = ran.obs.peeked[1];
    assert_ne!(target.table, orphan.table);
    let before = snap(ran, 1.0);
    let during = snap(ran, 2.0);
    let written = snap(ran, 3.0);
    let cleared = snap(ran, 4.0);
    assert_eq!(before.collections, 0);
    assert_eq!(during.collections, 1, "the call collected");
    assert_eq!(
        written.root_slots, before.root_slots,
        "leave restores the frame width after the reverse writes"
    );
    assert_eq!(written.live_roots, before.live_roots);
    assert!(
        during.root_slots > before.root_slots,
        "pending frame is open at the safepoint"
    );
    assert_eq!(
        during.root_slots - before.root_slots,
        7,
        "four receivers plus three already-evaluated values"
    );
    assert!(!during.peeked_alive[1], "orphan has no root");
    assert!(!during.token_open[1], "orphan token closed");
    assert!(
        during.resources < before.resources,
        "orphan resource collected"
    );
    assert_eq!(ran.obs.notes[1], (2.0, "boolean".to_string()));
    assert_eq!(ran.obs.notes[2], (3.0, "nil".to_string()));
    assert_eq!(ran.obs.notes[3], (4.0, "nil".to_string()));
    if early_is_target {
        assert!(during.peeked_alive[0], "early RHS root kept the target");
        assert!(during.token_open[0]);
        assert_eq!(
            during.live_roots - before.live_roots,
            5,
            "four receiver slots and the target; nil and true are not roots"
        );
        assert_eq!(ran.obs.notes[0], (1.0, "table".to_string()));
        assert_eq!(ran.obs.edges, vec![Some(target.table)]);
        assert!(written.peeked_alive[0]);
        assert!(!cleared.peeked_alive[0], "cleared edge lets the target die");
        assert!(!cleared.token_open[0]);
        assert!(cleared.resources < written.resources);
        assert_eq!(ran.obs.notes[4], (5.0, "nil".to_string()));
        assert_eq!(cleared.collections, 2);
        assert_eq!(ran.resources, cleared.resources);
    }
    assert_eq!(ran.visits.len(), 2, "two real collects");
    assert_ne!(ran.visits[0], during.root_slots as u64);
    assert_ne!(ran.visits[0], during.live_roots as u64);
    assert!(ran.visits[0] > 0);
}

#[test]
fn parenthesized_sweep_keeps_only_the_extracted_rhs() {
    let ran = run_source(&seeded("(sweep())", "rawget(weak, 1)"));
    assert_collect_witness(&ran, true);
}

#[test]
fn double_parenthesized_sweep_keeps_the_same_identity() {
    let ran = run_source(&seeded("((sweep()))", "rawget(weak, 1)"));
    assert_collect_witness(&ran, true);
}

#[test]
fn string_before_a_double_call_does_not_block_collection() {
    let source = seeded("((sweep()))", "rawget(weak, 1)")
        .replace("nil, true, ((sweep()))", "nil, \"keep\", ((sweep()))");
    let ran = run_source(&source);
    assert_eq!(ran.status.as_ref().map(|_| ()), Ok(()), "{:?}", ran.status);
    let during = snap(&ran, 2.0);
    assert_eq!(during.collections, 1);
    assert_eq!(during.peeked_alive, vec![true, false]);
    assert_eq!(ran.obs.notes[1], (2.0, "string".to_string()));
}

#[test]
fn sweep_without_collection_leaves_the_orphan() {
    let source = r#"local function seed()
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
local dest = {}
local other = {}
local early = rawget(weak, 1)
local lost = rawget(weak, 2)
local function sweep()
  snap(2)
  return nil
end
peek(early)
peek(lost)
snap(1)
dest.kept, other.flag = early, (sweep())
snap(3)
return rawequal(dest.kept, rawget(weak, 1))
"#;
    let ran = run_source(source);
    assert_eq!(ran.status.as_ref().map(|_| ()), Ok(()), "{:?}", ran.status);
    assert_eq!(ran.stack, vec![Value::Bool(true)]);
    let during = snap(&ran, 2.0);
    assert_eq!(during.collections, 0);
    assert_eq!(during.peeked_alive, vec![true, true]);
    assert_eq!(snap(&ran, 3.0).root_slots, snap(&ran, 1.0).root_slots);
    assert!(ran.visits.is_empty());
}

#[test]
fn dropping_the_early_rhs_lets_the_target_die() {
    let source = r#"local function seed()
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
local dest = {}
local other = {}
local early = rawget(weak, 1)
local lost = rawget(weak, 2)
local function sweep()
  collectgarbage("collect")
  snap(2)
  return nil
end
peek(early)
peek(lost)
snap(1)
dest.kept, other.flag = nil, (sweep())
snap(3)
note(1, type(rawget(weak, 1)))
return true
"#;
    let ran = run_source(source);
    assert_eq!(ran.status.as_ref().map(|_| ()), Ok(()), "{:?}", ran.status);
    let during = snap(&ran, 2.0);
    assert_eq!(during.collections, 1);
    assert_eq!(during.peeked_alive, vec![false, false]);
    assert_eq!(during.token_open, vec![false, false]);
    assert!(during.resources < snap(&ran, 1.0).resources);
    assert_eq!(ran.obs.notes, vec![(1.0, "nil".to_string())]);
    assert_eq!(snap(&ran, 3.0).root_slots, snap(&ran, 1.0).root_slots);
}
