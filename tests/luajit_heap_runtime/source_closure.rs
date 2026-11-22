//! Lua source creates one shared cell, calls both closures, and collects it.
//!
//! `collect_at_safepoint` still receives only [`heap::TableRef`] roots. A live
//! callable is not itself a root: gather publishes the issued envelope that
//! the closure copy-captured. The heap upvalue, not a Core borrow cell, is the
//! mutable capture. Host observers record handles and booleans; they do not
//! root, collect, call the closures, or write the cell.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use vm::{
    CallOutcome, CallReturn, CompileSourceFileOptions, FrontendImportSyntax, FrontendIr,
    HostFunctionRegistry, HostFunctionSchema, HostImportSchema, HostOwnedFunction,
    HostParamPassing, HostParamSchema, HostTypeSchema, OpCode, OwnedHostCall, ParseError,
    SourceFlavor, SourcePlugin, Value, VmError, VmStatus,
};

use super::candidate::{self, CaptureNote};
use super::heap::{self, TableRef};

const OBSERVE: &str = "lua_host::observe_table";
const NOTE_FN: &str = "lua_host::note_fn";
const NOTE_PHASE: &str = "lua_host::note_phase";
const NOTE_NIL: &str = "lua_host::note_nil";
const NOTE_GRAPH: &str = "lua_host::note_graph";
const NOTE_SCALAR: &str = "lua_host::note_scalar";

const SOURCE: &str = r#"
local mt = {}
mt.__mode = "v"
local weak = {}
setmetatable(weak, mt)
observe_table(weak, 0)
do
  local get = nil
  local set = nil
  do
    local held = {}
    weak.old = held
    observe_table(held, 1)
    get = function()
      return held
    end
    set = function()
      held = {}
    end
  end
  note_fn(get)
  note_fn(set)
  local orphan = {}
  weak.orphan = orphan
  observe_table(orphan, 2)
  orphan = nil
  collectgarbage("collect")
  note_phase(weak, 1)
  local first = get()
  observe_table(first, 3)
  first = nil
  set()
  @replace@
  note_phase(weak, 2)
  local second = get()
  weak.next = second
  observe_table(second, 4)
  second = nil
  get = nil
  set = nil
end
@drop@
note_phase(weak, 3)
"#;

#[derive(Clone, Copy, Debug)]
struct Phase {
    id: u32,
    old: bool,
    orphan: bool,
    next: bool,
    held_alive: bool,
    orphan_alive: bool,
    /// Tag 1 is a table whose `self` edge points at itself.
    self_cycle: bool,
    /// Collections already performed when this phase was observed.
    collections: u32,
}

/// Read-only closure and cell snapshot. Not a root and not a publication.
#[derive(Clone, Debug)]
struct GraphNote {
    id: u32,
    closure_alive: Vec<bool>,
    cell_alive: Vec<bool>,
    /// `true` when that note's closure can still read a table from its cell.
    cell_has_table: Vec<bool>,
}

#[derive(Clone, Debug, Default)]
struct Obs {
    tables: Vec<(u32, TableRef)>,
    callables: Vec<usize>,
    phases: Vec<Phase>,
    nils: Vec<bool>,
    graphs: Vec<GraphNote>,
    scalars: Vec<Value>,
}

struct ObserveTable;
struct NoteFn;
struct NotePhase;
struct NoteNil;
struct NoteGraph;
struct NoteScalar;

fn catalog() -> &'static Arc<vm::HostApiCatalog> {
    static CATALOG: OnceLock<Arc<vm::HostApiCatalog>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut builder = vm::HostApiBuilder::new();
        candidate::install_schemas(&mut builder);
        let table = HostTypeSchema::Resource(heap::LuaTableResource::key());
        builder.function(HostFunctionSchema::with_return(
            OBSERVE,
            vec![
                HostParamSchema::with_passing("table", table.clone(), HostParamPassing::Borrow),
                HostParamSchema::value("tag", HostTypeSchema::Float),
            ],
            HostTypeSchema::Float,
        ));
        builder.function(HostFunctionSchema::with_return(
            NOTE_FN,
            vec![HostParamSchema::value("closure", HostTypeSchema::Unknown)],
            HostTypeSchema::Float,
        ));
        builder.function(HostFunctionSchema::with_return(
            NOTE_PHASE,
            vec![
                HostParamSchema::with_passing("weak", table, HostParamPassing::Borrow),
                HostParamSchema::value("id", HostTypeSchema::Float),
            ],
            HostTypeSchema::Float,
        ));
        builder.function(HostFunctionSchema::with_return(
            NOTE_NIL,
            vec![HostParamSchema::value("value", HostTypeSchema::Unknown)],
            HostTypeSchema::Float,
        ));
        builder.function(HostFunctionSchema::with_return(
            NOTE_GRAPH,
            vec![HostParamSchema::value("id", HostTypeSchema::Float)],
            HostTypeSchema::Float,
        ));
        builder.function(HostFunctionSchema::with_return(
            NOTE_SCALAR,
            vec![HostParamSchema::value("value", HostTypeSchema::Unknown)],
            HostTypeSchema::Float,
        ));
        Arc::new(builder.build().expect("shared-closure catalog"))
    })
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

fn register(registry: &mut HostFunctionRegistry, catalog: &vm::HostApiCatalog) {
    for name in [
        OBSERVE,
        NOTE_FN,
        NOTE_PHASE,
        NOTE_NIL,
        NOTE_GRAPH,
        NOTE_SCALAR,
    ] {
        let function = catalog.function(name).expect("observer schema");
        registry
            .register_exact_owned(
                name,
                function.params.len() as u8,
                HostImportSchema::from_function(catalog, function),
                move |_| ctor_box(name),
            )
            .expect("register observer");
    }
}

fn ctor_box(name: &str) -> Box<dyn HostOwnedFunction> {
    match name {
        OBSERVE => Box::new(ObserveTable),
        NOTE_FN => Box::new(NoteFn),
        NOTE_PHASE => Box::new(NotePhase),
        NOTE_NIL => Box::new(NoteNil),
        NOTE_GRAPH => Box::new(NoteGraph),
        NOTE_SCALAR => Box::new(NoteScalar),
        _ => unreachable!("observer name"),
    }
}

fn host_fault(message: &str) -> VmError {
    VmError::HostError(message.to_string())
}

fn borrowed_table(call: &mut OwnedHostCall<'_>, index: usize) -> Result<TableRef, VmError> {
    let Value::Int(raw) = call.args().get(index).cloned().unwrap_or(Value::Null) else {
        return Err(host_fault("observer table is not a resource handle"));
    };
    let raw = u64::try_from(raw).map_err(|_| host_fault("observer token out of range"))?;
    let token = vm::ResourceHandle::from_raw(raw).map_err(|_| host_fault("observer token"))?;
    let host = call.vm().host_context();
    let key = heap::LuaTableResource::key();
    host.borrow_resource_with_key::<heap::LuaTableResource>(token, &key)
        .map(|resource| resource.table())
        .map_err(|error| host_fault(&error.to_string()))
}

fn tag_of(call: &OwnedHostCall<'_>, index: usize) -> Result<u32, VmError> {
    match call.args().get(index) {
        Some(Value::Float(number)) if number.fract() == 0.0 && *number >= 0.0 => Ok(*number as u32),
        _ => Err(host_fault("observer tag is not a number")),
    }
}

fn obs<'a>(host: &'a mut vm::HostContext<'a>) -> Result<&'a mut Obs, VmError> {
    host.scope_state_or_insert_with(Obs::default)
        .map_err(|error| host_fault(&error.to_string()))
}

impl HostOwnedFunction for ObserveTable {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        let table = borrowed_table(call, 0)?;
        let tag = tag_of(call, 1)?;
        let before = heap::stats(&mut call.vm().host_context())
            .map_err(|error| host_fault(&error.to_string()))?;
        obs(&mut call.vm().host_context())?
            .tables
            .push((tag, table));
        let after = heap::stats(&mut call.vm().host_context())
            .map_err(|error| host_fault(&error.to_string()))?;
        if after.live_roots != before.live_roots || after.collections != before.collections {
            return Err(host_fault("observe_table rooted or collected"));
        }
        Ok(CallOutcome::Return(CallReturn::one(Value::Float(0.0))))
    }
}

impl HostOwnedFunction for NoteFn {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        let ptr = match call.args() {
            [Value::Callable(callable)] => Arc::as_ptr(callable) as usize,
            _ => return Err(host_fault("note_fn expected the source closure")),
        };
        let before = heap::stats(&mut call.vm().host_context())
            .map_err(|error| host_fault(&error.to_string()))?;
        obs(&mut call.vm().host_context())?.callables.push(ptr);
        let after = heap::stats(&mut call.vm().host_context())
            .map_err(|error| host_fault(&error.to_string()))?;
        if after.live_roots != before.live_roots || after.collections != before.collections {
            return Err(host_fault("note_fn rooted or collected"));
        }
        Ok(CallOutcome::Return(CallReturn::one(Value::Float(0.0))))
    }
}

impl HostOwnedFunction for NotePhase {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        let weak = borrowed_table(call, 0)?;
        let id = tag_of(call, 1)?;
        let before = heap::stats(&mut call.vm().host_context())
            .map_err(|error| host_fault(&error.to_string()))?;
        let (held, orphan_ref) = {
            let mut host = call.vm().host_context();
            let tables = obs(&mut host)?.tables.clone();
            (
                tables
                    .iter()
                    .find(|(tag, _)| *tag == 1)
                    .map(|(_, table)| *table),
                tables
                    .iter()
                    .find(|(tag, _)| *tag == 2)
                    .map(|(_, table)| *table),
            )
        };
        let (old, orphan, next, held_alive, orphan_alive, self_cycle) = {
            let mut host = call.vm().host_context();
            heap::with_heap(&mut host, |heap| {
                Ok((
                    heap.named_edge(weak, "old")?.is_some(),
                    heap.named_edge(weak, "orphan")?.is_some(),
                    heap.named_edge(weak, "next")?.is_some(),
                    held.is_some_and(|table| heap.alive(table)),
                    orphan_ref.is_some_and(|table| heap.alive(table)),
                    held.is_some_and(|table| {
                        heap.alive(table)
                            && matches!(
                                heap.named_edge(table, "self"),
                                Ok(Some(target)) if target == table
                            )
                    }),
                ))
            })
            .map_err(|error| host_fault(&error.to_string()))?
        };
        obs(&mut call.vm().host_context())?.phases.push(Phase {
            id,
            old,
            orphan,
            next,
            held_alive,
            orphan_alive,
            self_cycle,
            collections: before.collections,
        });
        let after = heap::stats(&mut call.vm().host_context())
            .map_err(|error| host_fault(&error.to_string()))?;
        if after.live_roots != before.live_roots || after.collections != before.collections {
            return Err(host_fault("note_phase rooted or collected"));
        }
        Ok(CallOutcome::Return(CallReturn::one(Value::Float(0.0))))
    }
}

impl HostOwnedFunction for NoteNil {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        let is_nil = matches!(call.args(), [Value::Null]);
        let before = heap::stats(&mut call.vm().host_context())
            .map_err(|error| host_fault(&error.to_string()))?;
        obs(&mut call.vm().host_context())?.nils.push(is_nil);
        let after = heap::stats(&mut call.vm().host_context())
            .map_err(|error| host_fault(&error.to_string()))?;
        if after.live_roots != before.live_roots || after.collections != before.collections {
            return Err(host_fault("note_nil rooted or collected"));
        }
        Ok(CallOutcome::Return(CallReturn::one(Value::Float(0.0))))
    }
}

impl HostOwnedFunction for NoteGraph {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        let id = tag_of(call, 0)?;
        let before = heap::stats(&mut call.vm().host_context())
            .map_err(|error| host_fault(&error.to_string()))?;
        let notes = candidate::capture_notes(call.vm());
        let (closure_alive, cell_alive, cell_has_table) = {
            let mut host = call.vm().host_context();
            heap::with_heap(&mut host, |heap| {
                let mut closures = Vec::with_capacity(notes.len());
                let mut cells = Vec::with_capacity(notes.len());
                let mut values = Vec::with_capacity(notes.len());
                for note in &notes {
                    let closure_live = heap.closure_alive(note.closure);
                    let cell_live = heap.upvalue_alive(note.cell);
                    let has_table = if closure_live {
                        matches!(
                            heap.read_cell(note.closure)?,
                            Some(heap::CellValue::Table(_))
                        )
                    } else {
                        false
                    };
                    closures.push(closure_live);
                    cells.push(cell_live);
                    values.push(has_table);
                }
                Ok((closures, cells, values))
            })
            .map_err(|error| host_fault(&error.to_string()))?
        };
        obs(&mut call.vm().host_context())?.graphs.push(GraphNote {
            id,
            closure_alive,
            cell_alive,
            cell_has_table,
        });
        let after = heap::stats(&mut call.vm().host_context())
            .map_err(|error| host_fault(&error.to_string()))?;
        if after.live_roots != before.live_roots || after.collections != before.collections {
            return Err(host_fault("note_graph rooted or collected"));
        }
        Ok(CallOutcome::Return(CallReturn::one(Value::Float(0.0))))
    }
}

impl HostOwnedFunction for NoteScalar {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        let before = heap::stats(&mut call.vm().host_context())
            .map_err(|error| host_fault(&error.to_string()))?;
        let value = match call.args() {
            [value] => value.clone(),
            _ => return Err(host_fault("note_scalar arity")),
        };
        obs(&mut call.vm().host_context())?.scalars.push(value);
        let after = heap::stats(&mut call.vm().host_context())
            .map_err(|error| host_fault(&error.to_string()))?;
        if after.live_roots != before.live_roots || after.collections != before.collections {
            return Err(host_fault("note_scalar rooted or collected"));
        }
        Ok(CallOutcome::Return(CallReturn::one(Value::Float(0.0))))
    }
}

struct Arm {
    vm: vm::Vm,
    notes: Vec<CaptureNote>,
    reads: u32,
    writes: u32,
    obs: Obs,
    call_values: usize,
    call_scripts: usize,
}

fn source(replace: bool, drop: bool) -> String {
    SOURCE
        .replace(
            "@replace@",
            if replace {
                "collectgarbage(\"collect\")"
            } else {
                ""
            },
        )
        .replace(
            "@drop@",
            if drop {
                "collectgarbage(\"collect\")"
            } else {
                ""
            },
        )
}

fn run_arm(replace: bool, drop: bool) -> Arm {
    let source = source(replace, drop);
    let catalog = Arc::clone(catalog());
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&Plugin)
        .with_host_api_catalog(Arc::clone(&catalog));
    let prepared = vm::compile_source_with_flavor_and_options(&source, SourceFlavor::Lua, options)
        .unwrap_or_else(|error| panic!("lower/compile rejected the source: {error}"));
    let mut vm = prepared.into_vm().expect("create VM");
    let mut registry = HostFunctionRegistry::empty();
    candidate::register(&mut registry, &catalog).expect("register candidate runtime");
    register(&mut registry, &catalog);
    registry.bind_vm_cached(&mut vm).expect("bind runtime");
    let (call_values, call_scripts) = opcode_counts(vm.program());
    assert_eq!(
        vm.run().expect("run source closures"),
        VmStatus::Halted,
        "replace={replace} drop={drop}"
    );
    let notes = candidate::capture_notes(&mut vm);
    let (reads, writes) = candidate::capture_counts(&mut vm);
    let obs = vm
        .host_context()
        .scope_state::<Obs>()
        .cloned()
        .unwrap_or_default();
    Arm {
        vm,
        notes,
        reads,
        writes,
        obs,
        call_values,
        call_scripts,
    }
}

fn opcode_counts(program: &vm::Program) -> (usize, usize) {
    let mut ip = 0;
    let mut call_values = 0;
    let mut call_scripts = 0;
    while ip < program.code.len() {
        let op = OpCode::try_from(program.code[ip]).expect("opcode");
        match op {
            OpCode::CallValue => call_values += 1,
            OpCode::CallScript => call_scripts += 1,
            _ => {}
        }
        ip += 1 + op.operand_len();
    }
    (call_values, call_scripts)
}

fn tagged(obs: &Obs, tag: u32) -> TableRef {
    obs.tables
        .iter()
        .find(|(found, _)| *found == tag)
        .map(|(_, table)| *table)
        .unwrap_or_else(|| panic!("missing observation {tag}"))
}

fn phase(obs: &Obs, id: u32) -> Phase {
    obs.phases
        .iter()
        .copied()
        .find(|phase| phase.id == id)
        .unwrap_or_else(|| panic!("missing phase {id}"))
}

fn alive(vm: &mut vm::Vm, table: TableRef) -> bool {
    heap::with_heap(&mut vm.host_context(), |heap| Ok(heap.alive(table))).expect("alive")
}

fn closure_alive(vm: &mut vm::Vm, note: &CaptureNote) -> bool {
    heap::with_heap(&mut vm.host_context(), |heap| {
        Ok(heap.closure_alive(note.closure) && heap.upvalue_alive(note.cell))
    })
    .expect("closure")
}

fn assert_shared_calls(arm: &Arm) {
    assert_eq!(
        arm.call_scripts, 0,
        "static CallScript must not implement the calls"
    );
    assert_eq!(
        arm.call_values, 3,
        "getter twice and setter once must be CallValue"
    );
    assert_eq!(arm.reads, 4, "two outer reads and two getter calls");
    assert_eq!(arm.writes, 1, "the source setter wrote the cell");
    assert_eq!(arm.notes.len(), 2, "two source closures");
    assert_eq!(arm.notes[0].cell, arm.notes[1].cell, "one shared cell");
    assert_ne!(
        arm.notes[0].envelope, arm.notes[1].envelope,
        "each closure has its own envelope"
    );
    assert_eq!(
        arm.obs.callables,
        vec![arm.notes[0].callable, arm.notes[1].callable]
    );
    assert_ne!(arm.notes[0].closure, arm.notes[1].closure);
    assert_eq!(
        tagged(&arm.obs, 1),
        tagged(&arm.obs, 3),
        "getter returned the original table"
    );
    assert_ne!(
        tagged(&arm.obs, 3),
        tagged(&arm.obs, 4),
        "setter replaced the cell table"
    );
    let first = phase(&arm.obs, 1);
    assert!(
        !first.orphan && !first.orphan_alive,
        "first collect swept the orphan"
    );
    assert!(
        first.old && first.held_alive,
        "closure kept the original table"
    );
}

#[test]
fn source_closures_share_one_cell_across_block_exit_and_collect() {
    let mut both = run_arm(true, true);
    assert_shared_calls(&both);
    let replaced = phase(&both.obs, 2);
    assert!(
        !replaced.old && !replaced.held_alive,
        "replace collect released the old table"
    );
    let done = phase(&both.obs, 3);
    assert!(!done.next, "drop collect cleared the new weak entry");
    assert!(!alive(&mut both.vm, tagged(&both.obs, 4)));
    assert!(
        both.notes
            .iter()
            .all(|note| !closure_alive(&mut both.vm, note))
    );

    let mut keep_old = run_arm(false, true);
    assert_shared_calls(&keep_old);
    let still = phase(&keep_old.obs, 2);
    assert!(
        still.old && still.held_alive,
        "omitted replace collect left the old table"
    );
    let dropped = phase(&keep_old.obs, 3);
    assert!(!dropped.next);
    assert!(!alive(&mut keep_old.vm, tagged(&keep_old.obs, 4)));
    assert!(
        keep_old
            .notes
            .iter()
            .all(|note| !closure_alive(&mut keep_old.vm, note))
    );

    let mut keep_graph = run_arm(true, false);
    assert_shared_calls(&keep_graph);
    let replaced = phase(&keep_graph.obs, 2);
    assert!(!replaced.old && !replaced.held_alive);
    let kept = phase(&keep_graph.obs, 3);
    assert!(kept.next, "omitted drop collect left the new table");
    assert!(alive(&mut keep_graph.vm, tagged(&keep_graph.obs, 4)));
    assert!(
        keep_graph
            .notes
            .iter()
            .all(|note| closure_alive(&mut keep_graph.vm, note))
    );
    assert!(!alive(&mut keep_graph.vm, tagged(&keep_graph.obs, 1)));
}

fn run_chunk(source: &str) -> Arm {
    let catalog = Arc::clone(catalog());
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&Plugin)
        .with_host_api_catalog(Arc::clone(&catalog));
    let prepared = vm::compile_source_with_flavor_and_options(source, SourceFlavor::Lua, options)
        .unwrap_or_else(|error| panic!("lower/compile rejected the source: {error}"));
    let mut vm = prepared.into_vm().expect("create VM");
    let mut registry = HostFunctionRegistry::empty();
    candidate::register(&mut registry, &catalog).expect("register candidate runtime");
    register(&mut registry, &catalog);
    registry.bind_vm_cached(&mut vm).expect("bind runtime");
    let (call_values, call_scripts) = opcode_counts(vm.program());
    assert_eq!(
        vm.run().expect("vm rejected the chunk"),
        VmStatus::Halted,
        "chunk did not halt"
    );
    let notes = candidate::capture_notes(&mut vm);
    let (reads, writes) = candidate::capture_counts(&mut vm);
    let obs = vm
        .host_context()
        .scope_state::<Obs>()
        .cloned()
        .unwrap_or_default();
    Arm {
        vm,
        notes,
        reads,
        writes,
        obs,
        call_values,
        call_scripts,
    }
}

fn graph(obs: &Obs, id: u32) -> GraphNote {
    obs.graphs
        .iter()
        .find(|note| note.id == id)
        .cloned()
        .unwrap_or_else(|| panic!("missing graph {id}"))
}

fn assert_call_value(arm: &Arm) {
    assert_eq!(
        arm.call_scripts, 0,
        "CallScript must not implement the call"
    );
    assert!(arm.call_values > 0, "missing CallValue");
}

fn assert_shared_upvalue_closure_assignment_unsupported(source: &str, line: usize) {
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&Plugin)
        .with_host_api_catalog(Arc::clone(catalog()));
    let error = match vm::compile_source_with_flavor_and_options(source, SourceFlavor::Lua, options)
    {
        Ok(_) => {
            panic!("candidate accepted a closure assignment to a shared upvalue at line {line}")
        }
        Err(vm::SourcePathError::SourceWithMap {
            error: vm::SourceError::Parse(error),
            ..
        }) => error,
        Err(other) => panic!("expected a candidate lowering diagnostic, got {other}"),
    };
    assert_eq!(error.line, line, "{error}");
    assert!(error.message.contains("not lowered yet"), "{error}");
}

#[test]
fn shared_upvalue_closure_assignment_rejects_self_capture() {
    let source = r#"do
  local a = {}
  a = function() return a end
  local x = a
  note_fn(x)
end
"#;
    assert_shared_upvalue_closure_assignment_unsupported(source, 3);
}

#[test]
fn shared_upvalue_closure_assignment_rejects_target_captured_by_another_closure() {
    let source = r#"do
  local a = {}
  local held = {}
  local get = nil
  get = function() return a end
  a = function() return held end
  local x = a
  note_fn(x)
end
"#;
    assert_shared_upvalue_closure_assignment_unsupported(source, 6);
}

const WEAK_HEAD: &str = "\
local mt = {}\n\
mt.__mode = \"v\"\n\
local weak = {}\n\
setmetatable(weak, mt)\n\
observe_table(weak, 0)\n";

#[test]
fn outer_read_sees_the_table_written_by_the_closure() {
    let source = format!(
        "{WEAK_HEAD}\
do\n\
  local held = {{}}\n\
  observe_table(held, 1)\n\
  local get = nil\n\
  local set = nil\n\
  get = function()\n\
    return held\n\
  end\n\
  set = function()\n\
    held = {{}}\n\
  end\n\
  set()\n\
  local seen = held\n\
  observe_table(seen, 2)\n\
  local again = get()\n\
  observe_table(again, 3)\n\
end\n"
    );
    let arm = run_chunk(&source);
    assert_call_value(&arm);
    assert_eq!(arm.notes.len(), 2, "getter and setter");
    assert_eq!(arm.notes[0].cell, arm.notes[1].cell, "one shared cell");
    assert_ne!(
        tagged(&arm.obs, 1),
        tagged(&arm.obs, 2),
        "outer read after set() still saw the original slot"
    );
    assert_eq!(
        tagged(&arm.obs, 2),
        tagged(&arm.obs, 3),
        "getter and outer read diverged"
    );
}

#[test]
fn replaced_table_leaves_the_shared_cell_and_can_be_collected() {
    let source = format!(
        "{WEAK_HEAD}\
do\n\
  local held = {{}}\n\
  weak.old = held\n\
  observe_table(held, 1)\n\
  local set = nil\n\
  set = function()\n\
    held = {{}}\n\
  end\n\
  set()\n\
  local orphan = {{}}\n\
  weak.orphan = orphan\n\
  orphan = nil\n\
  collectgarbage(\"collect\")\n\
  note_phase(weak, 1)\n\
end\n"
    );
    let arm = run_chunk(&source);
    assert_call_value(&arm);
    let swept = phase(&arm.obs, 1);
    assert!(!swept.orphan && !swept.orphan_alive, "collect did not run");
    assert!(
        !swept.old && !swept.held_alive,
        "old table stayed reachable after set() replaced the cell"
    );
}

#[test]
fn outer_nil_clears_the_shared_cell_seen_by_the_getter() {
    let source = "\
do\n\
  local held = {}\n\
  observe_table(held, 1)\n\
  local get = nil\n\
  get = function()\n\
    return held\n\
  end\n\
  held = nil\n\
  local gone = get()\n\
  note_nil(gone)\n\
  note_graph(1)\n\
end\n";
    let arm = run_chunk(source);
    assert_call_value(&arm);
    assert_eq!(arm.obs.nils.first().copied(), Some(true));
    let snap = graph(&arm.obs, 1);
    assert!(
        snap.closure_alive.first().copied().unwrap_or(false),
        "getter closure was collected while it was still callable"
    );
    assert!(
        !snap.cell_has_table.first().copied().unwrap_or(true),
        "getter still read a table after the outer nil write"
    );
}

#[test]
fn outer_table_write_uses_the_same_binding_cell() {
    let source = "\
do\n\
  local held = {}\n\
  observe_table(held, 1)\n\
  local get = nil\n\
  get = function()\n\
    return held\n\
  end\n\
  held = {}\n\
  local seen = held\n\
  observe_table(seen, 2)\n\
  local got = get()\n\
  observe_table(got, 3)\n\
end\n";
    let arm = run_chunk(source);
    assert_call_value(&arm);
    assert_eq!(arm.notes.len(), 1);
    assert_ne!(tagged(&arm.obs, 1), tagged(&arm.obs, 2));
    assert_eq!(tagged(&arm.obs, 2), tagged(&arm.obs, 3));
}

#[test]
fn alias_calls_the_original_closure_after_nil_slot_and_collect() {
    let source = format!(
        "{WEAK_HEAD}\
do\n\
  local held = {{}}\n\
  observe_table(held, 1)\n\
  local get = nil\n\
  get = function()\n\
    return held\n\
  end\n\
  note_fn(get)\n\
  local alias = get\n\
  note_fn(alias)\n\
  get = nil\n\
  local orphan = {{}}\n\
  weak.orphan = orphan\n\
  orphan = nil\n\
  collectgarbage(\"collect\")\n\
  note_phase(weak, 1)\n\
  local seen = alias()\n\
  observe_table(seen, 2)\n\
  note_graph(1)\n\
end\n"
    );
    let mut arm = run_chunk(&source);
    assert_call_value(&arm);
    assert_eq!(arm.obs.callables.len(), 2);
    assert_eq!(
        arm.obs.callables[0], arm.obs.callables[1],
        "alias was not the original callable"
    );
    let swept = phase(&arm.obs, 1);
    assert!(!swept.orphan && !swept.orphan_alive, "collect did not run");
    assert_eq!(
        tagged(&arm.obs, 1),
        tagged(&arm.obs, 2),
        "alias() did not return the original table"
    );
    let snap = graph(&arm.obs, 1);
    assert!(
        snap.closure_alive.first().copied().unwrap_or(false),
        "nil on the original slot collected the aliased closure"
    );
    assert!(alive(&mut arm.vm, tagged(&arm.obs, 1)));
}

#[test]
fn overwrite_sweeps_the_old_callable_while_its_sibling_keeps_the_cell() {
    let source = format!(
        "{WEAK_HEAD}\
do\n\
  local held = {{}}\n\
  local get = nil\n\
  local set = nil\n\
  get = function()\n\
    return held\n\
  end\n\
  set = function()\n\
    held = {{}}\n\
  end\n\
  note_fn(get)\n\
  note_fn(set)\n\
  get = function()\n\
    return held\n\
  end\n\
  note_fn(get)\n\
  local orphan = {{}}\n\
  weak.orphan = orphan\n\
  orphan = nil\n\
  collectgarbage(\"collect\")\n\
  note_graph(1)\n\
end\n"
    );
    let arm = run_chunk(&source);
    assert_eq!(arm.notes.len(), 3, "old getter, setter, replacement getter");
    assert_eq!(arm.notes[0].cell, arm.notes[1].cell);
    assert_eq!(arm.notes[1].cell, arm.notes[2].cell);
    let snap = graph(&arm.obs, 1);
    assert_eq!(snap.closure_alive.len(), 3);
    assert!(
        !snap.closure_alive[0],
        "overwrite left the old getter reachable through the sibling envelope"
    );
    assert!(snap.closure_alive[1], "sibling setter was collected");
    assert!(snap.closure_alive[2], "replacement getter was collected");
    assert!(snap.cell_alive[1], "sibling lost the shared cell");
}

#[test]
fn block_exit_sweeps_a_closure_that_has_no_remaining_root() {
    let source = format!(
        "{WEAK_HEAD}\
do\n\
  local held = {{}}\n\
  local get = nil\n\
  get = function()\n\
    return held\n\
  end\n\
  note_fn(get)\n\
end\n\
local orphan = {{}}\n\
weak.orphan = orphan\n\
orphan = nil\n\
collectgarbage(\"collect\")\n\
note_phase(weak, 1)\n\
note_graph(1)\n"
    );
    let arm = run_chunk(&source);
    let swept = phase(&arm.obs, 1);
    assert!(!swept.orphan && !swept.orphan_alive, "collect did not run");
    let snap = graph(&arm.obs, 1);
    assert!(
        snap.closure_alive.first().copied() == Some(false),
        "block exit left the closure rooted"
    );
    assert!(
        snap.cell_alive.first().copied() == Some(false),
        "block exit left the cell rooted"
    );
}

#[test]
fn open_binding_keeps_the_cell_after_every_callable_is_cleared() {
    let source = format!(
        "{WEAK_HEAD}\
do\n\
  local held = {{}}\n\
  weak.old = held\n\
  observe_table(held, 1)\n\
  local get = nil\n\
  local set = nil\n\
  get = function()\n\
    return held\n\
  end\n\
  set = function()\n\
    held = {{}}\n\
  end\n\
  set()\n\
  get = nil\n\
  set = nil\n\
  local seen = held\n\
  observe_table(seen, 3)\n\
  weak.next = seen\n\
  local orphan = {{}}\n\
  observe_table(orphan, 2)\n\
  weak.orphan = orphan\n\
  orphan = nil\n\
  collectgarbage(\"collect\")\n\
  note_phase(weak, 1)\n\
  note_graph(1)\n\
end\n\
collectgarbage(\"collect\")\n\
note_phase(weak, 2)\n\
note_graph(2)\n"
    );
    let mut arm = run_chunk(&source);
    assert_call_value(&arm);
    assert_ne!(
        tagged(&arm.obs, 1),
        tagged(&arm.obs, 3),
        "outer read after set() did not see the cell"
    );
    let inside = phase(&arm.obs, 1);
    assert!(
        !inside.orphan && !inside.orphan_alive,
        "collect did not run"
    );
    assert!(
        !inside.old && !inside.held_alive,
        "original table stayed rooted after the cell moved on"
    );
    assert!(inside.next, "binding dropped the replacement table");
    let snap = graph(&arm.obs, 1);
    assert!(
        snap.closure_alive.iter().all(|alive| !alive),
        "withdrawn callables stayed rooted"
    );
    assert!(
        snap.cell_alive.iter().any(|alive| *alive),
        "open binding did not root the cell after every callable left"
    );
    let after = phase(&arm.obs, 2);
    assert!(!after.next, "block exit left the replacement table rooted");
    assert!(!alive(&mut arm.vm, tagged(&arm.obs, 3)));
}

fn namespace_fixture(label: &str, source: &str) -> PathBuf {
    // Same directory as `namespace_case_root` in tests/common. This test crate
    // does not include that module. A missing target dir must not fall back to /tmp.
    let target = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| {
        panic!("CARGO_TARGET_DIR must name the slot target; refusing to write a fixture elsewhere")
    });
    let root = PathBuf::from(target)
        .join("compat-frontends-namespace-cases")
        .join(label);
    fs::create_dir_all(&root).unwrap_or_else(|error| panic!("create {}: {error}", root.display()));
    let path = root.join("chunk.lua");
    fs::write(&path, source).unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
    path
}

fn run_lua_file(path: &Path) -> Arm {
    let catalog = Arc::clone(catalog());
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&Plugin)
        .with_host_api_catalog(Arc::clone(&catalog));
    let prepared = vm::compile_source_file_with_options(path, options)
        .unwrap_or_else(|error| panic!("lower/compile rejected {}: {error}", path.display()));
    let mut vm = prepared.into_vm().expect("create VM");
    let mut registry = HostFunctionRegistry::empty();
    candidate::register(&mut registry, &catalog).expect("register candidate runtime");
    register(&mut registry, &catalog);
    registry.bind_vm_cached(&mut vm).expect("bind runtime");
    let (call_values, call_scripts) = opcode_counts(vm.program());
    assert_eq!(
        vm.run().expect("run lua file"),
        VmStatus::Halted,
        "{}",
        path.display()
    );
    let notes = candidate::capture_notes(&mut vm);
    let (reads, writes) = candidate::capture_counts(&mut vm);
    let obs = vm
        .host_context()
        .scope_state::<Obs>()
        .cloned()
        .unwrap_or_default();
    Arm {
        vm,
        notes,
        reads,
        writes,
        obs,
        call_values,
        call_scripts,
    }
}

fn assert_scalar(actual: &Value, expected: &Value) {
    match (actual, expected) {
        (Value::Float(got), Value::Float(want)) => assert_eq!(
            got.to_bits(),
            want.to_bits(),
            "float bits differ: got {got:?}, want {want:?}"
        ),
        (Value::String(got), Value::String(want)) => {
            assert_eq!(got.as_str(), want.as_str(), "string bytes differ")
        }
        (Value::Bytes(got), Value::Bytes(want)) => {
            assert_eq!(got.as_slice(), want.as_slice(), "byte string differs")
        }
        (got, want) => assert_eq!(got, want, "scalar carrier mismatch"),
    }
}

#[test]
fn source_file_shared_scalar_upvalue_collects_replaced_cycle() {
    let path = namespace_fixture(
        "lua52_source_shared_scalars",
        r#"
local mt = {}
mt.__mode = "v"
local weak = {}
setmetatable(weak, mt)
observe_table(weak, 0)
do
  local get = nil
  local set_false = nil
  local set_neg = nil
  local set_text = nil
  local set_bytes = nil
  local set_nil = nil
  do
    local held = {}
    held.self = held
    weak.old = held
    observe_table(held, 1)
    get = function()
      return held
    end
    set_false = function()
      held = false
    end
    set_neg = function()
      held = -0.0
    end
    set_text = function()
      held = "text\0tail"
    end
    set_bytes = function()
      held = "\255\0tail"
    end
    set_nil = function()
      held = nil
    end
  end
  note_fn(get)
  note_fn(set_false)
  note_fn(set_neg)
  note_fn(set_text)
  note_fn(set_bytes)
  note_fn(set_nil)
  local orphan = {}
  weak.orphan = orphan
  observe_table(orphan, 2)
  orphan = nil
  collectgarbage("collect")
  note_phase(weak, 1)
  local seen = get()
  observe_table(seen, 3)
  seen = nil
  set_false()
  collectgarbage("collect")
  note_phase(weak, 2)
  note_graph(1)
  local got_false = get()
  note_scalar(got_false)
  got_false = nil
  set_neg()
  local got_neg = get()
  note_scalar(got_neg)
  got_neg = nil
  set_text()
  local got_text = get()
  note_scalar(got_text)
  got_text = nil
  set_bytes()
  local got_bytes = get()
  note_scalar(got_bytes)
  got_bytes = nil
  set_nil()
  local got_nil = get()
  note_scalar(got_nil)
  got_nil = nil
  get = nil
  set_false = nil
  set_neg = nil
  set_text = nil
  set_bytes = nil
  set_nil = nil
end
"#,
    );
    let mut arm = run_lua_file(&path);
    assert_eq!(
        arm.call_scripts, 0,
        "static CallScript must not implement the calls"
    );
    assert!(
        arm.call_values > 0,
        "the file did not execute through CallValue"
    );
    assert_eq!(arm.notes.len(), 6, "six closures share the cell");
    let cell = arm.notes[0].cell;
    assert!(
        arm.notes.iter().all(|note| note.cell == cell),
        "closures did not share one cell"
    );
    assert_eq!(arm.reads, 10, "shared reads did not go through the cell");
    assert_eq!(arm.writes, 5, "scalar writes did not go through the cell");
    assert_eq!(
        tagged(&arm.obs, 1),
        tagged(&arm.obs, 3),
        "getter did not keep the original self-cycle after the local block"
    );
    let kept = phase(&arm.obs, 1);
    assert!(kept.self_cycle, "held table was not a self-cycle");
    assert!(
        kept.old && kept.held_alive,
        "getter did not keep the cycle alive"
    );
    assert!(
        !kept.orphan && !kept.orphan_alive,
        "first collect did not run"
    );
    assert!(kept.collections >= 1, "first collect was not recorded");
    let cleared = phase(&arm.obs, 2);
    assert!(
        cleared.collections > kept.collections,
        "second collect did not run"
    );
    assert!(!cleared.old, "weak value still named the replaced cycle");
    assert!(
        !cleared.held_alive,
        "self-cycle stayed alive after the cell moved to false"
    );
    let snap = graph(&arm.obs, 1);
    assert!(
        snap.closure_alive.iter().all(|alive| *alive),
        "getter died before it could observe false"
    );
    assert!(
        snap.cell_has_table.iter().all(|has| !has),
        "cell still held a table after the false write"
    );
    assert_eq!(
        arm.obs.scalars.len(),
        5,
        "getter did not return every replacement"
    );
    assert_scalar(&arm.obs.scalars[0], &Value::Bool(false));
    assert_scalar(&arm.obs.scalars[1], &Value::Float(-0.0));
    assert_scalar(&arm.obs.scalars[2], &Value::string("text\0tail"));
    assert_scalar(
        &arm.obs.scalars[3],
        &Value::bytes([0xff, 0, b't', b'a', b'i', b'l']),
    );
    assert_scalar(&arm.obs.scalars[4], &Value::Null);
    assert!(
        !alive(&mut arm.vm, tagged(&arm.obs, 1)),
        "replaced self-cycle was still reachable after the run"
    );
}

#[test]
fn source_file_scalar_write_follows_binding_not_name() {
    let path = namespace_fixture(
        "lua52_source_shared_scalar_shadow",
        r#"
do
  local held = {}
  observe_table(held, 1)
  local get = nil
  get = function()
    return held
  end
  do
    local held = {}
    observe_table(held, 2)
    local inner_get = nil
    local inner_set = nil
    inner_get = function()
      return held
    end
    inner_set = function()
      held = false
    end
    inner_set()
    local inner_seen = inner_get()
    note_scalar(inner_seen)
    local outer_seen = get()
    observe_table(outer_seen, 3)
    note_fn(inner_get)
    note_fn(inner_set)
    inner_get = nil
    inner_set = nil
  end
  note_fn(get)
  get = nil
end
"#,
    );
    let arm = run_lua_file(&path);
    assert_eq!(
        arm.notes.len(),
        3,
        "outer getter, inner getter, inner setter"
    );
    assert_eq!(
        arm.notes[1].cell, arm.notes[2].cell,
        "inner closures did not share the inner binding"
    );
    assert_ne!(
        arm.notes[0].cell, arm.notes[1].cell,
        "same name collapsed two bindings onto one cell"
    );
    assert_eq!(arm.obs.scalars.len(), 1);
    assert_scalar(&arm.obs.scalars[0], &Value::Bool(false));
    assert_eq!(
        tagged(&arm.obs, 1),
        tagged(&arm.obs, 3),
        "outer binding changed when the inner name was written"
    );
    assert_ne!(tagged(&arm.obs, 1), tagged(&arm.obs, 2));
}
