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

#[derive(Clone, Debug, Default)]
struct Obs {
    tables: Vec<(u32, TableRef)>,
    callables: Vec<usize>,
    phases: Vec<Phase>,
}

struct ObserveTable;
struct NoteFn;
struct NotePhase;

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
    for name in [OBSERVE, NOTE_FN, NOTE_PHASE] {
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
    assert_eq!(arm.reads, 2, "the source getter ran twice");
    assert_eq!(arm.writes, 1, "the source setter wrote the cell");
    assert_eq!(arm.notes.len(), 2, "two source closures");
    assert_eq!(arm.notes[0].cell, arm.notes[1].cell, "one shared cell");
    assert_eq!(
        arm.notes[0].envelope, arm.notes[1].envelope,
        "one captured envelope"
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
