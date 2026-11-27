//! Protected table metatables for the Lua 5.2 candidate.
//!
//! Real `.lua` files go through the candidate source plugin, Core, and the
//! interpreter. `getmetatable` / `setmetatable` stay on the existing host
//! calls. This slice does not add `pcall`, non-table metatables, or `__index`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use vm::{
    CompileSourceFileOptions, FrontendImportSyntax, FrontendIr, HostFunctionRegistry, ParseError,
    SourceFlavor, SourcePlugin, Value, VmStatus,
};

use super::candidate;

struct MetatablePlugin;

impl SourcePlugin for MetatablePlugin {
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
        candidate::lower(source, super::catalog())
    }
}

fn claim_file(source: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let parent = std::env::temp_dir().join("compat-frontends-lua-source-fixtures");
    fs::create_dir_all(&parent).expect("fixture parent");
    let pid = std::process::id();
    let root = loop {
        let ticket = NEXT.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!("table-metatable-{pid}-{ticket}"));
        match fs::create_dir(&candidate) {
            Ok(()) => break candidate,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("claim {}: {error}", candidate.display()),
        }
    };
    let path = root.join("chunk.lua");
    fs::write(&path, source).unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
    path
}

struct Run {
    status: Result<VmStatus, String>,
    stack: Vec<Value>,
    path: PathBuf,
    vm: Option<vm::Vm>,
}

#[derive(Clone, Copy)]
enum Note {
    Text,
    Bool,
}

#[derive(Default)]
struct Notes {
    text: Vec<(f64, String)>,
    flags: Vec<(f64, bool)>,
}

impl vm::HostOwnedFunction for Note {
    fn call(&mut self, call: &mut vm::OwnedHostCall<'_>) -> Result<vm::CallOutcome, vm::VmError> {
        let args = call.args().to_vec();
        let mut host = call.vm().host_context();
        let notes = host
            .scope_state_or_insert_with(Notes::default)
            .map_err(|error| vm::VmError::HostError(error.to_string()))?;
        match self {
            Self::Text => {
                let (tag, text) = match args.as_slice() {
                    [Value::Float(tag), Value::String(text)] => (*tag, text.as_str().to_string()),
                    _ => {
                        return Err(vm::VmError::HostError("expected Lua string".to_string()));
                    }
                };
                notes.text.push((tag, text));
            }
            Self::Bool => {
                let (tag, flag) = match args.as_slice() {
                    [Value::Float(tag), Value::Bool(flag)] => (*tag, *flag),
                    _ => {
                        return Err(vm::VmError::HostError("expected Lua boolean".to_string()));
                    }
                };
                notes.flags.push((tag, flag));
            }
        }
        Ok(vm::CallOutcome::Return(vm::CallReturn::one(Value::Bool(
            true,
        ))))
    }
}

fn register_note(
    registry: &mut HostFunctionRegistry,
    catalog: &vm::HostApiCatalog,
    name: &str,
    note: Note,
) -> Result<(), String> {
    let function = catalog
        .function(name)
        .ok_or_else(|| format!("missing {name}"))?;
    registry
        .register_exact_owned(
            name,
            function.params.len() as u8,
            vm::HostImportSchema::from_function(catalog, function),
            move |_| Box::new(note),
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn run_file(source: &str) -> Run {
    let path = claim_file(source);
    let catalog = Arc::clone(super::catalog());
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&MetatablePlugin)
        .with_host_api_catalog(Arc::clone(&catalog));
    let prepared = match vm::compile_source_file_with_options(&path, options) {
        Ok(prepared) => prepared,
        Err(error) => {
            return Run {
                status: Err(error.to_string()),
                stack: Vec::new(),
                path,
                vm: None,
            };
        }
    };
    let mut vm = match prepared.into_vm() {
        Ok(vm) => vm,
        Err(error) => {
            return Run {
                status: Err(error.to_string()),
                stack: Vec::new(),
                path,
                vm: None,
            };
        }
    };
    let mut registry = HostFunctionRegistry::empty();
    if let Err(error) = candidate::register(&mut registry, &catalog) {
        return Run {
            status: Err(error.to_string()),
            stack: Vec::new(),
            path,
            vm: None,
        };
    }
    super::register_witnesses(&mut registry, &catalog);
    for (name, note) in [
        ("lua_host::note_text", Note::Text),
        ("lua_host::note_bool", Note::Bool),
    ] {
        if let Err(error) = register_note(&mut registry, &catalog, name, note) {
            return Run {
                status: Err(error),
                stack: Vec::new(),
                path,
                vm: None,
            };
        }
    }
    if let Err(error) = registry.bind_vm_cached(&mut vm) {
        return Run {
            status: Err(error.to_string()),
            stack: Vec::new(),
            path,
            vm: None,
        };
    }
    let status = vm.run().map_err(|error| error.to_string());
    let stack = vm.stack().to_vec();
    eprintln!(
        "TABLE_METATABLE path={} status={status:?} stack={stack:?}",
        path.display()
    );
    Run {
        status,
        stack,
        path,
        vm: Some(vm),
    }
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

#[test]
fn unprotected_setmetatable_returns_the_same_table() {
    let typed = run_file("return type(setmetatable({}, {}))\n");
    assert!(
        matches!(typed.status, Ok(VmStatus::Halted)),
        "type path {} status={:?}",
        display(&typed.path),
        typed.status
    );
    assert_eq!(
        typed.stack,
        vec![Value::string("table")],
        "setmetatable must return a table, not a non-table sentinel: {}",
        display(&typed.path)
    );

    let same = run_file(
        "local t = {}\n\
         local mt = {}\n\
         return same_table(setmetatable(t, mt), t)\n",
    );
    assert!(
        matches!(same.status, Ok(VmStatus::Halted)),
        "identity path {} status={:?}",
        display(&same.path),
        same.status
    );
    assert_eq!(
        same.stack,
        vec![Value::Bool(true)],
        "the call result must be the same table {}",
        display(&same.path)
    );
}

#[test]
fn string_guard_is_returned_and_blocks_replacement() {
    let exposed = run_file(
        "local t = {}\n\
         local mt = {}\n\
         mt.__metatable = \"sealed\"\n\
         setmetatable(t, mt)\n\
         return type(getmetatable(t))\n",
    );
    assert!(
        matches!(exposed.status, Ok(VmStatus::Halted)),
        "exposure path {} status={:?}",
        display(&exposed.path),
        exposed.status
    );
    assert_eq!(
        exposed.stack,
        vec![Value::string("string")],
        "raw __metatable must be exposed instead of the real metatable: {}",
        display(&exposed.path)
    );

    let noted = run_file(
        "local t = {}\n\
         local mt = {}\n\
         mt.__metatable = \"sealed\"\n\
         setmetatable(t, mt)\n\
         note_text(1, getmetatable(t))\n\
         return 1\n",
    );
    assert!(
        matches!(noted.status, Ok(VmStatus::Halted)),
        "note path {} status={:?}",
        display(&noted.path),
        noted.status
    );
    let text = noted
        .vm
        .expect("vm")
        .host_context()
        .scope_state::<Notes>()
        .map(|notes| notes.text.clone())
        .unwrap_or_default();
    assert_eq!(text, vec![(1.0, "sealed".to_string())]);

    let mut blocked = run_file(
        "local owner = {}\n\
         local mt = {}\n\
         local other = {}\n\
         observe_table(owner, 0)\n\
         observe_table(mt, 0)\n\
         mt.__metatable = \"sealed\"\n\
         setmetatable(owner, mt)\n\
         setmetatable(owner, other)\n\
         return 1\n",
    );
    let error = blocked
        .status
        .expect_err("protected setmetatable must fail");
    assert!(
        error.contains("cannot change a protected metatable"),
        "candidate host error, not a pcall string: {error}"
    );
    let vm = blocked.vm.as_mut().expect("vm after host error");
    let observed = vm
        .host_context()
        .scope_state::<Vec<super::ObservedTable>>()
        .cloned()
        .unwrap_or_default();
    let owner = observed[0].table;
    let original = observed[1].table;
    let current = super::heap::with_heap(&mut vm.host_context(), |heap| heap.metatable(owner))
        .expect("read metatable");
    assert_eq!(
        current,
        Some(original),
        "failed set must leave the real metatable"
    );
}

#[test]
fn absent_metatable_is_nil_and_table_guard_keeps_identity() {
    let absent = run_file("return type(getmetatable({}))\n");
    assert!(
        matches!(absent.status, Ok(VmStatus::Halted)),
        "{:?}",
        absent.status
    );
    assert_eq!(absent.stack, vec![Value::string("nil")]);

    let seen = run_file(
        "local shield = {}\n\
         local mt = {}\n\
         mt.__metatable = shield\n\
         local owner = setmetatable({}, mt)\n\
         return same_table(getmetatable(owner), shield)\n",
    );
    assert!(
        matches!(seen.status, Ok(VmStatus::Halted)),
        "{:?}",
        seen.status
    );
    assert_eq!(
        seen.stack,
        vec![Value::Bool(true)],
        "table guard must be the stored table, not the real metatable"
    );

    let not_mt = run_file(
        "local shield = {}\n\
         local mt = {}\n\
         mt.__metatable = shield\n\
         local owner = setmetatable({}, mt)\n\
         return same_table(getmetatable(owner), mt)\n",
    );
    assert!(
        matches!(not_mt.status, Ok(VmStatus::Halted)),
        "{:?}",
        not_mt.status
    );
    assert_eq!(not_mt.stack, vec![Value::Bool(false)]);

    let mut blocked = run_file(
        "local shield = {}\n\
         local mt = {}\n\
         local owner = {}\n\
         observe_table(owner, 0)\n\
         observe_table(mt, 0)\n\
         mt.__metatable = shield\n\
         setmetatable(owner, mt)\n\
         setmetatable(owner, nil)\n\
         return 1\n",
    );
    let error = blocked.status.expect_err("protected clear must fail");
    assert!(
        error.contains("cannot change a protected metatable"),
        "{error}"
    );
    let vm = blocked.vm.as_mut().expect("vm");
    let observed = vm
        .host_context()
        .scope_state::<Vec<super::ObservedTable>>()
        .cloned()
        .unwrap_or_default();
    let current = super::heap::with_heap(&mut vm.host_context(), |heap| {
        heap.metatable(observed[0].table)
    })
    .expect("read metatable");
    assert_eq!(current, Some(observed[1].table));
}

#[test]
fn raw_nil_guard_allows_a_later_clear() {
    let cleared = run_file(
        "local owner = {}\n\
         local mt = {}\n\
         mt.__metatable = \"sealed\"\n\
         setmetatable(owner, mt)\n\
         mt.__metatable = nil\n\
         return same_table(setmetatable(owner, nil), owner)\n",
    );
    assert!(
        matches!(cleared.status, Ok(VmStatus::Halted)),
        "{:?}",
        cleared.status
    );
    assert_eq!(cleared.stack, vec![Value::Bool(true)]);

    let gone = run_file(
        "local owner = {}\n\
         local mt = {}\n\
         mt.__metatable = \"sealed\"\n\
         setmetatable(owner, mt)\n\
         mt.__metatable = nil\n\
         setmetatable(owner, nil)\n\
         return type(getmetatable(owner))\n",
    );
    assert!(
        matches!(gone.status, Ok(VmStatus::Halted)),
        "{:?}",
        gone.status
    );
    assert_eq!(gone.stack, vec![Value::string("nil")]);
}

#[test]
fn inherited_guard_does_not_protect() {
    let source = "\
local hidden = {}\n\
hidden.__metatable = \"inherited\"\n\
local parent = {}\n\
parent.__index = hidden\n\
local mt = {}\n\
local owner = {}\n\
observe_table(mt, 0)\n\
observe_table(parent, 0)\n\
setmetatable(mt, parent)\n\
setmetatable(owner, mt)\n\
local replaced = {}\n\
return same_table(getmetatable(owner), mt)\n";
    let seen = run_file(source);
    assert!(
        matches!(seen.status, Ok(VmStatus::Halted)),
        "{:?}",
        seen.status
    );
    assert_eq!(seen.stack, vec![Value::Bool(true)]);

    let mut replaced = run_file(
        "local hidden = {}\n\
         hidden.__metatable = \"inherited\"\n\
         local parent = {}\n\
         parent.__index = hidden\n\
         local mt = {}\n\
         local owner = {}\n\
         local other = {}\n\
         observe_table(owner, 0)\n\
         observe_table(mt, 0)\n\
         observe_table(parent, 0)\n\
         setmetatable(mt, parent)\n\
         setmetatable(owner, mt)\n\
         return same_table(setmetatable(owner, other), owner)\n",
    );
    assert!(
        matches!(replaced.status, Ok(VmStatus::Halted)),
        "inherited __metatable must not protect: {:?}",
        replaced.status
    );
    assert_eq!(replaced.stack, vec![Value::Bool(true)]);
    let vm = replaced.vm.as_mut().expect("vm");
    let observed = vm
        .host_context()
        .scope_state::<Vec<super::ObservedTable>>()
        .cloned()
        .unwrap_or_default();
    let (owner_mt, mt_parent) = super::heap::with_heap(&mut vm.host_context(), |heap| {
        Ok((
            heap.metatable(observed[0].table)?,
            heap.metatable(observed[1].table)?,
        ))
    })
    .expect("read metatables");
    assert_ne!(owner_mt, Some(observed[1].table), "replacement must stick");
    assert_eq!(
        mt_parent,
        Some(observed[2].table),
        "the real metatable keeps its own parent metatable"
    );
}

#[test]
fn shadowed_guard_does_not_stop_weak_value_collection() {
    let source = "\
local mt = {}\n\
mt.__mode = \"v\"\n\
mt.__metatable = \"shadow\"\n\
local weak = {}\n\
setmetatable(weak, mt)\n\
local key = {}\n\
local cycle = {}\n\
cycle.self = cycle\n\
weak[key] = cycle\n\
observe_table(cycle, 0)\n\
local peer = {}\n\
peer.self = peer\n\
observe_table(peer, 0)\n\
cycle = nil\n\
note_text(1, getmetatable(weak))\n\
collectgarbage(\"collect\")\n\
return type(weak[key])\n";
    let mut run = run_file(source);
    assert!(
        matches!(run.status, Ok(VmStatus::Halted)),
        "{:?}",
        run.status
    );
    assert_eq!(run.stack, vec![Value::string("nil")]);
    let vm = run.vm.as_mut().expect("vm");
    let text = vm
        .host_context()
        .scope_state::<Notes>()
        .map(|notes| notes.text.clone())
        .unwrap_or_default();
    assert_eq!(text, vec![(1.0, "shadow".to_string())]);
    let observed = vm
        .host_context()
        .scope_state::<Vec<super::ObservedTable>>()
        .cloned()
        .unwrap_or_default();
    let stats = super::heap::stats(&mut vm.host_context()).expect("stats");
    assert!(stats.collections >= 1, "collections={}", stats.collections);
    let alive = super::heap::with_heap(&mut vm.host_context(), |heap| {
        Ok((heap.alive(observed[0].table), heap.alive(observed[1].table)))
    })
    .expect("liveness");
    assert_eq!(alive, (false, true), "weak cycle cleared, rooted peer kept");
}

/// `false` is a raw guard. A real field write stores it; `getmetatable`
/// returns that boolean, and `setmetatable` cannot replace or clear the
/// owner until the raw field is nil.
#[test]
fn false_guard_is_returned_and_blocks_replacement() {
    let exposed = run_file(
        "local owner = {}\n\
         local mt = {}\n\
         mt.__metatable = false\n\
         setmetatable(owner, mt)\n\
         note_bool(1, getmetatable(owner))\n\
         return getmetatable(owner)\n",
    );
    assert!(
        matches!(exposed.status, Ok(VmStatus::Halted)),
        "false must be stored by field assignment before the guard can be read: {:?}",
        exposed.status
    );
    assert_eq!(
        exposed.stack,
        vec![Value::Bool(false)],
        "getmetatable must return the stored false, not a string or the real metatable"
    );
    let flags = exposed
        .vm
        .expect("vm")
        .host_context()
        .scope_state::<Notes>()
        .map(|notes| notes.flags.clone())
        .unwrap_or_default();
    assert_eq!(flags, vec![(1.0, false)]);

    let mut blocked = run_file(
        "local owner = {}\n\
         local mt = {}\n\
         observe_table(owner, 0)\n\
         observe_table(mt, 0)\n\
         mt.__metatable = false\n\
         setmetatable(owner, mt)\n\
         setmetatable(owner, {})\n\
         return 1\n",
    );
    let error = blocked.status.expect_err("false must protect replacement");
    assert!(
        error.contains("cannot change a protected metatable"),
        "candidate host error, not a pcall string: {error}"
    );
    let vm = blocked.vm.as_mut().expect("vm");
    let observed = vm
        .host_context()
        .scope_state::<Vec<super::ObservedTable>>()
        .cloned()
        .unwrap_or_default();
    let current = super::heap::with_heap(&mut vm.host_context(), |heap| {
        heap.metatable(observed[0].table)
    })
    .expect("read metatable");
    assert_eq!(current, Some(observed[1].table));

    let mut cleared = run_file(
        "local owner = {}\n\
         local mt = {}\n\
         observe_table(owner, 0)\n\
         observe_table(mt, 0)\n\
         mt.__metatable = false\n\
         setmetatable(owner, mt)\n\
         setmetatable(owner, nil)\n\
         return 1\n",
    );
    let error = cleared.status.expect_err("false must protect clear");
    assert!(
        error.contains("cannot change a protected metatable"),
        "{error}"
    );
    let vm = cleared.vm.as_mut().expect("vm");
    let observed = vm
        .host_context()
        .scope_state::<Vec<super::ObservedTable>>()
        .cloned()
        .unwrap_or_default();
    let current = super::heap::with_heap(&mut vm.host_context(), |heap| {
        heap.metatable(observed[0].table)
    })
    .expect("read metatable");
    assert_eq!(current, Some(observed[1].table));

    let released = run_file(
        "local owner = {}\n\
         local mt = {}\n\
         mt.__metatable = false\n\
         setmetatable(owner, mt)\n\
         mt.__metatable = nil\n\
         return same_table(setmetatable(owner, nil), owner)\n",
    );
    assert!(
        matches!(released.status, Ok(VmStatus::Halted)),
        "nil raw field must allow a later clear: {:?}",
        released.status
    );
    assert_eq!(released.stack, vec![Value::Bool(true)]);
}
