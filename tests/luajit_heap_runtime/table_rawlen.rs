//! Unbound single-argument `rawlen` on an issued table.
//!
//! Real `.lua` files go through `compile_source_file_with_options`, the
//! candidate plugin, and Core. String and byte operands keep the existing
//! byte-length result. This slice does not lower table `#`, dispatch `__len`,
//! or adjust argument lists. A sparse table's result is the first border of
//! the contiguous prefix from 1, not a layout-dependent border.
//!
//! A negative integer literal is not a lowerable index in this slice: unary
//! minus only stores `-0`. The negative case therefore uses the existing
//! `float_bits(1)` witness, which is that `-0`.

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
        let candidate = parent.join(format!("table-rawlen-{label}-{pid}-{ticket}"));
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

fn stack_of(ran: &Attempt) -> &[Value] {
    ran.stack.as_ref().expect("halted stack")
}

fn assert_len(label: &str, source: &str, expected: f64) -> Attempt {
    let ran = run(label, source);
    let actual = match stack_of(&ran) {
        [Value::Float(number)] => *number,
        other => panic!("{label}: expected one number, stack {other:?}"),
    };
    assert!(
        actual.to_bits() == expected.to_bits(),
        "{label}: rawlen {actual} != {expected}"
    );
    ran
}

fn assert_fault(label: &str, source: &str, needle: &str) {
    let ran = attempt(label, source).unwrap_or_else(|error| panic!("{label}: {error}"));
    let Err(message) = &ran.stack else {
        panic!("{label}: expected a host fault, stack {:?}", ran.stack);
    };
    assert!(
        message.contains(needle),
        "{label}: {message} does not contain {needle}"
    );
}

fn collections(vm: &mut vm::Vm) -> u32 {
    heap::with_heap(&mut vm.host_context(), |heap| Ok(heap.stats()))
        .expect("heap stats")
        .collections
}

#[test]
fn lua_file_rawlen_of_empty_table_is_zero() {
    let mut ran = assert_len("empty", "return rawlen({})\n", 0.0);
    assert_eq!(collections(&mut ran.vm), 0, "rawlen must not collect");
}

#[test]
fn lua_file_rawlen_of_dense_numeric_keys_is_the_prefix() {
    let mut ran = assert_len(
        "dense",
        r#"local owner = {}
owner[1] = "a"
owner[2] = "b"
owner[3] = "c"
return rawlen(owner)
"#,
        3.0,
    );
    assert_eq!(collections(&mut ran.vm), 0, "rawlen must not collect");
}

#[test]
fn lua_file_rawlen_after_delete_stops_at_the_gap() {
    assert_len(
        "deleted-tail",
        r#"local owner = {}
owner[1] = "a"
owner[2] = "b"
owner[2] = nil
return rawlen(owner)
"#,
        1.0,
    );
}

#[test]
fn lua_file_rawlen_ignores_text_key_that_looks_like_one() {
    assert_len(
        "text-one",
        r#"local owner = {}
owner["1"] = "text"
return rawlen(owner)
"#,
        0.0,
    );
    assert_len(
        "text-and-number",
        r#"local owner = {}
owner[1] = "num"
owner["1"] = "text"
return rawlen(owner)
"#,
        1.0,
    );
}

#[test]
fn lua_file_rawlen_ignores_non_integer_and_huge_keys() {
    // `-1` is not a lowerable index here. `float_bits(1)` is the existing
    // negative zero, and it shares the key with `0`. Neither extends a prefix.
    assert_len(
        "non-integer",
        r#"local owner = {}
local neg = float_bits(1)
local inf = float_bits(3)
local ninf = float_bits(4)
owner[1] = "a"
owner[2] = "b"
owner[0] = "zero"
owner[neg] = "negzero"
owner[1.5] = "half"
owner[1e16] = "huge"
rawset(owner, inf, "inf")
rawset(owner, ninf, "ninf")
return rawlen(owner)
"#,
        2.0,
    );
}

#[test]
fn lua_file_rawlen_of_sparse_table_is_the_first_border() {
    // Intentional candidate choice: the first border of the prefix from 1.
    // This does not freeze a hash or array layout, and it is not a promise
    // to match a capacity-dependent LuaJIT border.
    assert_len(
        "sparse-first-border",
        r#"local owner = {}
owner[1] = "a"
owner[4] = "d"
return rawlen(owner)
"#,
        1.0,
    );
    // No key 1: the first border is 0. The locked LuaJIT binary returned 3 for
    // this assignment order, which is another legal border and depends on
    // insert history. That result is not the candidate rule.
    assert_len(
        "no-key-one",
        r#"local owner = {}
owner[2] = "b"
owner[3] = "c"
return rawlen(owner)
"#,
        0.0,
    );
}

#[test]
fn lua_file_rawlen_skips_a_weak_value_gap() {
    let held = r#"local mt = {}
mt.__mode = "v"
local owner = {}
setmetatable(owner, mt)
local orphan = {}
orphan.self = orphan
owner[1] = orphan
owner[2] = "stay"
orphan = nil
return rawlen(owner)
"#;
    assert_len("weak-held", held, 2.0);

    let cleared = r#"local mt = {}
mt.__mode = "v"
local owner = {}
setmetatable(owner, mt)
local orphan = {}
orphan.self = orphan
owner[1] = orphan
owner[2] = "stay"
orphan = nil
collectgarbage("collect")
return rawlen(owner)
"#;
    assert_len("weak-gap", cleared, 0.0);

    let witness = cleared.replace("return rawlen(owner)", "return type(owner[1])");
    let ran = run("weak-gap-type", &witness);
    assert_eq!(stack_of(&ran), &[Value::string("nil")]);
}

#[test]
fn lua_file_rawlen_ignores_a_protected_metatable_and_len_field() {
    let source = r#"local mt = {}
mt.__metatable = "sealed"
mt.__len = "47"
local owner = {}
setmetatable(owner, mt)
owner[1] = "a"
owner[2] = "b"
return rawlen(owner)
"#;
    assert_len("protected-rawlen", source, 2.0);
    let seal = source.replace("return rawlen(owner)", "return getmetatable(owner)");
    let ran = run("protected-seal", &seal);
    assert_eq!(stack_of(&ran), &[Value::string("sealed")]);
}

#[test]
fn lua_file_rawlen_evaluates_its_operand_once() {
    let mut ran = run(
        "once",
        r#"local ready = {}
ready[1] = "a"
ready[2] = "b"
local get = nil
do
  local held = ready
  get = function()
    return held
  end
end
return rawlen(get())
"#,
    );
    assert_eq!(stack_of(&ran), &[Value::Float(2.0)]);
    assert_eq!(candidate::capture_counts(&mut ran.vm), (1, 0));
    assert_eq!(collections(&mut ran.vm), 0, "rawlen must not collect");
}

#[test]
fn lua_file_rawlen_rejects_a_non_table_and_a_forged_carrier() {
    assert_fault(
        "bool",
        "return rawlen(true)\n",
        "Lua rawlen operand is not a string or table",
    );
    assert_fault(
        "forged",
        "return rawlen(external_shape())\n",
        "Lua rawlen operand is not a string or table",
    );
}

#[test]
fn lua_file_hash_of_a_table_stays_outside_this_slice() {
    assert_fault(
        "hash-table",
        "return #{}\n",
        "Lua length operand is not a string",
    );
}

#[test]
fn lua_file_rawlen_ignores_object_keys_and_private_fields() {
    assert_len(
        "object-key",
        r#"local owner = {}
local key = {}
rawset(owner, key, "obj")
owner[1] = "a"
return rawlen(owner)
"#,
        1.0,
    );
    assert_len(
        "private-field",
        r#"local owner = {}
local planted = {}
plant_private_field(owner, planted)
return rawlen(owner)
"#,
        0.0,
    );
}
