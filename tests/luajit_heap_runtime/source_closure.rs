//! Lua source creates one shared cell, calls both closures, and collects it.
//!
//! `collect_at_safepoint` still receives only [`heap::TableRef`] roots. A live
//! callable is not itself a root: gather publishes the issued envelope that
//! the closure copy-captured. The heap upvalue, not a Core borrow cell, is the
//! mutable capture. Host observers record handles and booleans; they do not
//! root, collect, call the closures, or write the cell.

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
}

struct ObserveTable;
struct NoteFn;
struct NotePhase;
struct NoteNil;
struct NoteGraph;

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
    for name in [OBSERVE, NOTE_FN, NOTE_PHASE, NOTE_NIL, NOTE_GRAPH] {
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
        let (old, orphan, next, held_alive, orphan_alive) = {
            let mut host = call.vm().host_context();
            heap::with_heap(&mut host, |heap| {
                Ok((
                    heap.named_edge(weak, "old")?.is_some(),
                    heap.named_edge(weak, "orphan")?.is_some(),
                    heap.named_edge(weak, "next")?.is_some(),
                    held.is_some_and(|table| heap.alive(table)),
                    orphan_ref.is_some_and(|table| heap.alive(table)),
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
