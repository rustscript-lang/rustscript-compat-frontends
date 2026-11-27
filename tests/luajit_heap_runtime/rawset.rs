//! Unbound `rawset` for the Lua 5.2 candidate.
//!
//! Real `.lua` files go through `compile_source_file_with_options`, the
//! candidate plugin, and Core. The write is the existing edge operation.
//! This slice does not add key categories, `__newindex`, `pcall`, or a
//! public host ABI. LuaJIT's `__newindex` / `pcall` receipt stays outside
//! these assertions: this candidate does not dispatch metatable writes.
//!
//! A source key `-0.0` is not an index operand here. Unary minus is only
//! lowered for a proven scalar store, so the numeric contrast uses
//! `float_bits(1)`, the candidate's existing negative zero.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use vm::{
    CompileSourceFileOptions, FrontendImportSyntax, FrontendIr, HostFunctionRegistry, ParseError,
    SourceFlavor, SourcePlugin, Value, VmStatus,
};

use super::candidate;
use super::heap;

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
        candidate::lower(source, super::catalog())
    }
}

struct Attempt {
    vm: vm::Vm,
    stack: Result<Vec<Value>, String>,
}

fn claim_file(label: &str, source: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let parent = std::env::temp_dir().join("compat-frontends-lua-source-fixtures");
    fs::create_dir_all(&parent)
        .unwrap_or_else(|error| panic!("create {}: {error}", parent.display()));
    let pid = std::process::id();
    let root = loop {
        let ticket = NEXT.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!("rawset-{label}-{pid}-{ticket}"));
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

fn attempt(label: &str, source: &str) -> Result<Attempt, String> {
    let path = claim_file(label, source);
    let catalog = std::sync::Arc::clone(super::catalog());
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&Plugin)
        .with_host_api_catalog(std::sync::Arc::clone(&catalog));
    let prepared = vm::compile_source_file_with_options(&path, options).map_err(|error| {
        format!(
            "{}: compile_source_file_with_options: {error}",
            path.display()
        )
    })?;
    let mut vm = prepared
        .into_vm()
        .map_err(|error| format!("{}: vm: {error}", path.display()))?;
    let mut registry = HostFunctionRegistry::empty();
    candidate::register(&mut registry, &catalog)
        .map_err(|error| format!("{}: register: {error}", path.display()))?;
    super::register_witnesses(&mut registry, &catalog);
    registry
        .bind_vm_cached(&mut vm)
        .map_err(|error| format!("{}: bind: {error}", path.display()))?;
    let stack = match vm.run() {
        Ok(status) => {
            assert_eq!(status, VmStatus::Halted, "{}", path.display());
            Ok(vm.stack().to_vec())
        }
        Err(error) => Err(format!("{}: {error}", path.display())),
    };
    Ok(Attempt { vm, stack })
}

fn run(label: &str, source: &str) -> Attempt {
    let ran = attempt(label, source).unwrap_or_else(|error| panic!("{label}: {error}"));
    if let Err(message) = &ran.stack {
        panic!("{label}: runtime error: {message}");
    }
    ran
}

fn refuse(label: &str, source: &str) -> String {
    match attempt(label, source) {
        Err(message) => message,
        Ok(ran) => panic!(
            "{label}: lowered a refused shape, stack {:?}",
            ran.vm.stack()
        ),
    }
}

fn host_fault(label: &str, source: &str) -> (vm::Vm, String) {
    let ran = attempt(label, source).unwrap_or_else(|error| panic!("{label}: {error}"));
    let Err(message) = ran.stack else {
        panic!("{label}: expected a host fault, stack {:?}", ran.vm.stack());
    };
    (ran.vm, message)
}

fn stats(vm: &mut vm::Vm) -> heap::HeapStats {
    heap::with_heap(&mut vm.host_context(), |heap| Ok(heap.stats())).expect("heap stats")
}

fn stack_of(ran: &Attempt) -> &[Value] {
    ran.stack.as_ref().expect("halted stack")
}

const WEAK_ORPHAN: &str = r#"local mt = {}
mt.__mode = "v"
local weak = {}
setmetatable(weak, mt)
local orphan = {}
orphan.self = orphan
weak[1] = orphan
orphan = nil
"#;

#[test]
fn lua_file_rawset_returns_the_issued_receiver() {
    let owner = run(
        "receiver",
        r#"local owner = {}
local value = {}
return same_table(rawset(owner, 1, value), owner)
"#,
    );
    assert_eq!(stack_of(&owner), &[Value::Bool(true)]);

    let not_value = run(
        "not-value",
        r#"local owner = {}
local value = {}
return same_table(rawset(owner, 1, value), value)
"#,
    );
    assert_eq!(
        stack_of(&not_value),
        &[Value::Bool(false)],
        "rawset must return the receiver, not the stored value"
    );
}

#[test]
fn lua_file_rawset_shares_storage_with_rawget_and_assignment() {
    let from_rawset = run(
        "string-rawget",
        r#"local owner = {}
rawset(owner, "name", "yes")
return rawget(owner, "name")
"#,
    );
    assert_eq!(stack_of(&from_rawset), &[Value::string("yes")]);

    let from_index = run(
        "string-index",
        r#"local owner = {}
rawset(owner, "name", "yes")
return owner["name"]
"#,
    );
    assert_eq!(stack_of(&from_index), &[Value::string("yes")]);

    let from_field = run(
        "string-field",
        r#"local owner = {}
rawset(owner, "name", "yes")
return owner.name
"#,
    );
    assert_eq!(stack_of(&from_field), &[Value::string("yes")]);

    let from_assign = run(
        "assign-rawget",
        r#"local owner = {}
owner["slot"] = "from-assign"
return rawget(owner, "slot")
"#,
    );
    assert_eq!(stack_of(&from_assign), &[Value::string("from-assign")]);

    let mut split = run(
        "split-keys",
        r#"local owner = {}
rawset(owner, 1, "number")
rawset(owner, "1", "string")
return rawget(owner, 1)
"#,
    );
    assert_eq!(stack_of(&split), &[Value::string("number")]);
    assert_eq!(
        stats(&mut split.vm).live_edges,
        2,
        "string and number keys stay distinct"
    );
    let string_key = run(
        "split-string",
        r#"local owner = {}
rawset(owner, 1, "number")
rawset(owner, "1", "string")
return rawget(owner, "1")
"#,
    );
    assert_eq!(stack_of(&string_key), &[Value::string("string")]);
}

#[test]
fn lua_file_rawset_keeps_object_key_identity() {
    let ran = run(
        "object-key",
        r#"local owner = {}
local key = {}
local value = {}
local written = rawset(owner, key, value)
return same_table(rawget(written, key), value)
"#,
    );
    assert_eq!(stack_of(&ran), &[Value::Bool(true)]);
    let same_owner = run(
        "object-owner",
        r#"local owner = {}
local key = {}
local value = {}
return same_table(rawset(owner, key, value), owner)
"#,
    );
    assert_eq!(stack_of(&same_owner), &[Value::Bool(true)]);
}

#[test]
fn lua_file_rawset_updates_and_deletes_with_explicit_nil() {
    let mut updated = run(
        "update",
        r#"local owner = {}
rawset(owner, 1, "first")
rawset(owner, 1, "second")
return rawget(owner, 1)
"#,
    );
    assert_eq!(stack_of(&updated), &[Value::string("second")]);
    assert_eq!(
        stats(&mut updated.vm).live_edges,
        1,
        "update replaces one edge"
    );

    let mut deleted = run(
        "delete",
        r#"local owner = {}
rawset(owner, 1, "first")
rawset(owner, 1, nil)
return rawget(owner, 1)
"#,
    );
    assert_eq!(stack_of(&deleted), &[Value::Null]);
    assert_eq!(
        stats(&mut deleted.vm).live_edges,
        0,
        "explicit nil removes the edge"
    );
}

#[test]
fn lua_file_rawset_writes_negative_zero_into_the_zero_slot() {
    let wrote = run(
        "neg-zero-write",
        r#"local owner = {}
rawset(owner, (float_bits(1)), "negzero")
return rawget(owner, 0)
"#,
    );
    assert_eq!(stack_of(&wrote), &[Value::string("negzero")]);

    let read = run(
        "neg-zero-read",
        r#"local owner = {}
rawset(owner, 0, "zero")
return rawget(owner, (float_bits(1)))
"#,
    );
    assert_eq!(stack_of(&read), &[Value::string("zero")]);
}

#[test]
fn lua_file_rawset_calls_a_local_function_of_the_same_name() {
    let local_fn = run(
        "local-fn",
        r#"local function rawset()
  return 7
end
local n = rawset()
return n
"#,
    );
    assert_eq!(stack_of(&local_fn), &[Value::Float(7.0)]);

    let after_block = run(
        "after-block",
        r#"do
  local rawset = 1
end
local owner = {}
rawset(owner, 1, "kept")
return rawget(owner, 1)
"#,
    );
    assert_eq!(stack_of(&after_block), &[Value::string("kept")]);
}

#[test]
fn lua_file_rawset_does_not_capture_a_bound_name() {
    let chunk = refuse(
        "chunk-shadow",
        r#"local rawset = 1
local owner = {}
owner[1] = "kept"
rawset(owner, 1, "nope")
return owner[1]
"#,
    );
    assert!(
        chunk.contains("Lua expression not lowered yet"),
        "a chunk local must not become builtin rawset, got {chunk}"
    );

    let inside = refuse(
        "function-shadow",
        r#"local rawset = 1
local function probe()
  local owner = {}
  rawset(owner, 1, "nope")
end
return 1
"#,
    );
    assert!(
        inside.contains("Lua expression not lowered yet"),
        "an upvalue binding must not become builtin rawset, got {inside}"
    );

    let unbound = run(
        "function-unbound",
        r#"local function probe()
  local owner = {}
  rawset(owner, 1, "kept")
end
return 1
"#,
    );
    assert_eq!(
        stack_of(&unbound),
        &[Value::Float(1.0)],
        "an unbound call inside a function is still the builtin"
    );
}

#[test]
fn lua_file_rawset_rejects_nil_and_nan_keys_without_deleting() {
    let nil_assign = host_fault(
        "nil-assign",
        r#"local owner = {}
owner[1] = "stay"
local k = nil
owner[k] = "nope"
"#,
    );
    let (mut nil_write_vm, nil_write) = host_fault(
        "nil-write",
        r#"local owner = {}
owner[1] = "stay"
local k = nil
rawset(owner, k, "nope")
"#,
    );
    assert!(
        nil_assign.1.contains("edge key is not a table"),
        "assignment nil key: {}",
        nil_assign.1
    );
    assert!(
        nil_write.contains("edge key is not a table"),
        "rawset nil key must use the assignment error, got {nil_write}"
    );
    assert_eq!(stats(&mut nil_write_vm).live_edges, 1);

    let (mut nil_delete_vm, nil_delete) = host_fault(
        "nil-delete",
        r#"local owner = {}
owner[1] = "stay"
local k = nil
rawset(owner, k, nil)
"#,
    );
    assert!(
        nil_delete.contains("edge key is not a table"),
        "nil key must not take the delete path, got {nil_delete}"
    );
    assert_eq!(
        stats(&mut nil_delete_vm).live_edges,
        1,
        "a nil key must not delete the existing edge"
    );

    let nan_assign = host_fault(
        "nan-assign",
        r#"local owner = {}
owner[1] = "stay"
local k = float_bits(2)
owner[k] = "nope"
"#,
    );
    let (mut nan_write_vm, nan_write) = host_fault(
        "nan-write",
        r#"local owner = {}
owner[1] = "stay"
local k = float_bits(2)
rawset(owner, k, "nope")
"#,
    );
    assert!(
        nan_assign.1.contains("table index is NaN"),
        "assignment NaN key: {}",
        nan_assign.1
    );
    assert!(
        nan_write.contains("table index is NaN"),
        "rawset NaN key: {nan_write}"
    );
    assert_eq!(stats(&mut nan_write_vm).live_edges, 1);

    let (mut nan_delete_vm, nan_delete) = host_fault(
        "nan-delete",
        r#"local owner = {}
owner[1] = "stay"
local k = float_bits(2)
rawset(owner, k, nil)
"#,
    );
    assert!(
        nan_delete.contains("table index is NaN"),
        "NaN key must not take the delete path, got {nan_delete}"
    );
    assert_eq!(stats(&mut nan_delete_vm).live_edges, 1);
}

#[test]
fn lua_file_rawset_missing_value_is_not_a_nil_delete() {
    for (label, source) in [
        ("no-args", "return rawset()\n"),
        ("one-arg", "local owner = {}\nreturn rawset(owner)\n"),
        (
            "two-args",
            "local owner = {}\nowner[1] = \"stay\"\nreturn rawset(owner, 1)\n",
        ),
    ] {
        let message = refuse(label, source);
        assert!(
            message.contains("Lua expression not lowered yet"),
            "{label} must not default a missing argument to nil, got {message}"
        );
    }

    let mut deleted = run(
        "explicit-nil",
        r#"local owner = {}
owner[1] = "stay"
rawset(owner, 1, nil)
return rawget(owner, 1)
"#,
    );
    assert_eq!(stack_of(&deleted), &[Value::Null]);
    assert_eq!(stats(&mut deleted.vm).live_edges, 0);
}

#[test]
fn lua_file_rawset_collects_during_the_value_without_extra_roots() {
    let mut quiet = run(
        "quiet-collect",
        &format!("{WEAK_ORPHAN}collectgarbage(\"collect\")\nreturn type(weak[1])\n"),
    );
    assert_eq!(stack_of(&quiet), &[Value::string("nil")]);
    let quiet_marked = stats(&mut quiet.vm).last_marked;

    let (mut during, message) = host_fault(
        "value-collect",
        &format!("{WEAK_ORPHAN}rawset({{}}, ({{}}), collectgarbage(\"collect\"))\n"),
    );
    assert!(
        message.contains("edge target is not a table"),
        "value collect must still reach the write, got {message}"
    );
    let during_stats = stats(&mut during);
    assert_eq!(
        during_stats.collections, 1,
        "the value expression must collect"
    );
    assert_eq!(
        during_stats.last_marked,
        quiet_marked + 2,
        "only the fresh receiver and key are extra roots during the value collect"
    );
}

#[test]
fn lua_file_rawset_does_not_check_the_table_before_later_arguments() {
    let (mut skipped, skipped_message) = host_fault(
        "check-without-collect",
        &format!("{WEAK_ORPHAN}rawset(0, 1, \"later\")\n"),
    );
    assert!(
        skipped_message.contains("edge source is not a table"),
        "number receiver: {skipped_message}"
    );
    let skipped_nodes = stats(&mut skipped).live_nodes;

    let (mut checked, message) = host_fault(
        "check-after-key",
        &format!("{WEAK_ORPHAN}rawset(0, (collectgarbage(\"collect\")), \"later\")\n"),
    );
    assert!(
        message.contains("edge source is not a table"),
        "later key side effect must still reach the table check, got {message}"
    );
    let checked_stats = stats(&mut checked);
    assert_eq!(
        checked_stats.collections, 1,
        "the key collect must run before the table check"
    );
    assert_eq!(
        checked_stats.live_nodes + 1,
        skipped_nodes,
        "the weak orphan must be swept by that key collect"
    );
}

#[test]
fn lua_file_rawset_evaluates_key_and_value_once() {
    // A table-yielding expression with its own side-effect counter is not
    // expressible: a local function call used as a value lowers to nil.
    // The key and value can each collect once, and the stored string shows
    // the value expression's single type() result.
    let mut ran = run(
        "once",
        r#"local owner = {}
local written = rawset(owner, (collectgarbage("collect")), type(collectgarbage("collect")))
return rawget(written, 0)
"#,
    );
    assert_eq!(stack_of(&ran), &[Value::string("number")]);
    assert_eq!(
        stats(&mut ran.vm).collections,
        2,
        "each collect expression ran once"
    );
    let identity = run(
        "once-identity",
        r#"local owner = {}
return same_table(rawset(owner, (collectgarbage("collect")), type(collectgarbage("collect"))), owner)
"#,
    );
    assert_eq!(stack_of(&identity), &[Value::Bool(true)]);
}

#[test]
fn lua_file_rawset_discarded_return_is_not_an_extra_root() {
    let mut held = run(
        "held-receiver",
        &format!(
            "{WEAK_ORPHAN}local key = {{}}\nlocal holder = rawset({{}}, key, \"kept\")\ncollectgarbage(\"collect\")\nreturn rawget(holder, key)\n"
        ),
    );
    assert_eq!(stack_of(&held), &[Value::string("kept")]);
    let held_nodes = stats(&mut held.vm).live_nodes;

    let mut dropped = run(
        "dropped-receiver",
        &format!(
            "{WEAK_ORPHAN}local key = {{}}\nrawset({{}}, key, \"kept\")\ncollectgarbage(\"collect\")\nreturn type(weak[1])\n"
        ),
    );
    assert_eq!(
        stack_of(&dropped),
        &[Value::string("nil")],
        "the later collect must clear the weak orphan"
    );
    assert_eq!(
        stats(&mut dropped.vm).live_nodes + 1,
        held_nodes,
        "a statement rawset must not root the discarded receiver"
    );
}

#[test]
fn lua_file_assignment_discarded_write_is_not_an_extra_root() {
    let mut ran = run(
        "assign-drop",
        r#"local key = {}
({})[key] = "kept"
collectgarbage("collect")
return type(key)
"#,
    );
    assert_eq!(stack_of(&ran), &[Value::string("table")]);
    assert_eq!(
        stats(&mut ran.vm).live_nodes,
        1,
        "assignment must still discard the write result"
    );
}
