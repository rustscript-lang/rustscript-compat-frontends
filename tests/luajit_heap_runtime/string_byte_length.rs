//! String byte length for the Lua 5.2 candidate.
//!
//! `#` and unbound `rawlen` share one private length operation. These tests
//! feed real `.lua` files through the candidate source plugin, Core, and the
//! interpreter. Table length, `__len`, and `pcall` stay out of this slice.
//! A sibling module keeps those fixtures out of the closure-graph tests.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use vm::{
    CallOutcome, CallReturn, CompileSourceFileOptions, FrontendImportSyntax, FrontendIr,
    HostFunctionRegistry, HostFunctionSchema, HostImportSchema, HostOwnedFunction, HostParamSchema,
    HostTypeSchema, OwnedHostCall, ParseError, SourceFlavor, SourcePlugin, Value, VmError,
    VmStatus,
};

use super::candidate;

const NOTE: &str = "lua_host::note";
const NEXT_TEXT: &str = "lua_host::next_text";

const LENGTHS: &str = r#"note(#"")
note(rawlen(""))
note(#"abc")
note(rawlen("abc"))
note(#"\195\169\0z")
note(rawlen("\195\169\0z"))
note(#"\255\0a")
note(rawlen("\255\0a"))
local ascii = "abc"
note(#ascii)
note(rawlen(ascii))
local get = nil
local set = nil
do
  local held = {}
  get = function()
    return held
  end
  set = function()
    held = "\255\0a"
  end
end
set()
local dynamic = get()
note(#dynamic)
note(rawlen(dynamic))
return rawlen("abc")
"#;

const TYPE_HASH: &str = "return type(#\"\\195\\169\\0z\")\n";
const TYPE_RAWLEN: &str = "return type(rawlen(\"\\255\\0a\"))\n";

const ONCE: &str = "note(#(next_text()))\nreturn rawlen(next_text())\n";

const GETTER_RAWLEN: &str = r#"local get = nil
local set = nil
do
  local held = {}
  get = function()
    return held
  end
  set = function()
    held = "ab"
  end
end
set()
return rawlen(get())
"#;

const GETTER_HASH: &str = r#"local get = nil
local set = nil
do
  local held = {}
  get = function()
    return held
  end
  set = function()
    held = "abcd"
  end
end
set()
return #get()
"#;

const SHADOW_LOCAL: &str = r#"local rawlen = nil
local set = nil
do
  local held = {}
  rawlen = function()
    return held
  end
  set = function()
    held = "shadow"
  end
end
set()
local seen = rawlen()
return #seen
"#;

const SHADOW_FUNCTION: &str = r#"local function rawlen()
  local n = 7
  return n
end
local seen = rawlen()
return seen
"#;

const SHADOW_FUNCTION_STRING: &str = r#"local function rawlen()
  local held = "script"
  return held
end
local seen = rawlen()
return #seen
"#;

const BOUND_LOCAL_CALL: &str = "local rawlen = \"bound\"\nreturn rawlen(\"abc\")\n";
const BOUND_FUNCTION_CALL: &str = r#"local function rawlen()
  local held = "nope"
  return held
end
return rawlen("abc")
"#;
const RAWLEN_STRING_CALL: &str = "return rawlen\"abc\"\n";
const RAWLEN_NO_ARG: &str = "return rawlen()\n";

const NUMBER_OPERAND: &str = "local n = 42\nreturn rawlen(n)\n";
const NIL_OPERAND: &str = "local n = nil\nreturn #n\n";
const TABLE_OPERAND: &str = "return rawlen({})\n";

#[derive(Clone, Debug, Default)]
struct NotedLengths {
    values: Vec<f64>,
}

#[derive(Clone, Debug, Default)]
struct TextFeed {
    texts: Vec<String>,
    calls: u32,
}

struct NoteHost;
struct NextTextHost;
struct LengthPlugin;

#[derive(Debug)]
struct Witness {
    stack: Vec<Value>,
    notes: Vec<f64>,
    calls: u32,
    reads: u32,
    imports: Vec<String>,
    path: PathBuf,
}

fn catalog() -> &'static Arc<vm::HostApiCatalog> {
    static CATALOG: OnceLock<Arc<vm::HostApiCatalog>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut builder = vm::HostApiBuilder::new();
        candidate::install_schemas(&mut builder);
        builder.function(HostFunctionSchema::with_return(
            NOTE,
            vec![HostParamSchema::value("value", HostTypeSchema::Float)],
            HostTypeSchema::Bool,
        ));
        builder.function(HostFunctionSchema::with_return(
            NEXT_TEXT,
            vec![],
            HostTypeSchema::String,
        ));
        Arc::new(builder.build().expect("string byte length catalog"))
    })
}

impl HostOwnedFunction for NoteHost {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        let value = match call.args() {
            [Value::Float(number)] => *number,
            _ => {
                return Err(VmError::HostError(
                    "length note expected a Lua number".to_string(),
                ));
            }
        };
        let mut host = call.vm().host_context();
        host.scope_state_or_insert_with(NotedLengths::default)
            .map_err(|error| VmError::HostError(error.to_string()))?
            .values
            .push(value);
        Ok(CallOutcome::Return(CallReturn::one(Value::Bool(true))))
    }
}

impl HostOwnedFunction for NextTextHost {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        let mut host = call.vm().host_context();
        let feed = host
            .scope_state_or_insert_with(TextFeed::default)
            .map_err(|error| VmError::HostError(error.to_string()))?;
        let index = feed.calls as usize;
        feed.calls = feed.calls.saturating_add(1);
        let Some(text) = feed.texts.get(index).cloned() else {
            return Err(VmError::HostError("text feed exhausted".to_string()));
        };
        Ok(CallOutcome::Return(CallReturn::one(Value::string(text))))
    }
}

impl SourcePlugin for LengthPlugin {
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

fn register_hosts(registry: &mut HostFunctionRegistry, catalog: &vm::HostApiCatalog) {
    for name in [NOTE, NEXT_TEXT] {
        let function = catalog.function(name).expect("length witness schema");
        registry
            .register_exact_owned(
                name,
                function.params.len() as u8,
                HostImportSchema::from_function(catalog, function),
                move |_| match name {
                    NOTE => Box::new(NoteHost),
                    NEXT_TEXT => Box::new(NextTextHost),
                    _ => unreachable!("length witness name"),
                },
            )
            .expect("register length witness");
    }
}

fn claim_file(source: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let parent = std::env::temp_dir().join("compat-frontends-lua-source-fixtures");
    fs::create_dir_all(&parent).expect("fixture parent");
    let pid = std::process::id();
    let root = loop {
        let ticket = NEXT.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!("string-byte-length-{pid}-{ticket}"));
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

fn run_file(source: &str, texts: &[&str]) -> Result<Witness, String> {
    let path = claim_file(source);
    let catalog = Arc::clone(catalog());
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&LengthPlugin)
        .with_host_api_catalog(Arc::clone(&catalog));
    let prepared =
        vm::compile_source_file_with_options(&path, options).map_err(|error| error.to_string())?;
    let mut vm = prepared.into_vm().map_err(|error| error.to_string())?;
    if !texts.is_empty() {
        vm.host_context()
            .scope_state_or_insert_with(|| TextFeed {
                texts: texts.iter().map(|text| (*text).to_string()).collect(),
                calls: 0,
            })
            .map_err(|error| error.to_string())?;
    }
    let mut registry = HostFunctionRegistry::empty();
    candidate::register(&mut registry, &catalog).map_err(|error| error.to_string())?;
    register_hosts(&mut registry, &catalog);
    registry
        .bind_vm_cached(&mut vm)
        .map_err(|error| error.to_string())?;
    let status = vm.run().map_err(|error| error.to_string())?;
    if status != VmStatus::Halted {
        return Err(format!("status {status:?}"));
    }
    let (reads, _) = candidate::capture_counts(&mut vm);
    let notes = vm
        .host_context()
        .scope_state::<NotedLengths>()
        .map(|noted| noted.values.clone())
        .unwrap_or_default();
    let calls = vm
        .host_context()
        .scope_state::<TextFeed>()
        .map(|feed| feed.calls)
        .unwrap_or(0);
    let stack = vm.stack().to_vec();
    let imports = vm
        .program()
        .imports
        .iter()
        .map(|import| import.name.clone())
        .collect();
    eprintln!(
        "STRING_BYTE_LENGTH path={} notes={notes:?} stack={stack:?} calls={calls} reads={reads}",
        path.display()
    );
    Ok(Witness {
        stack,
        notes,
        calls,
        reads,
        imports,
        path,
    })
}

fn assert_floats(actual: &[f64], expected: &[f64], label: &str) {
    assert_eq!(
        actual.len(),
        expected.len(),
        "{label}: count actual={actual:?} expected={expected:?}"
    );
    for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        assert_eq!(
            actual.to_bits(),
            expected.to_bits(),
            "{label}: [{index}] actual={actual:?} expected={expected:?}"
        );
    }
}

fn assert_rejected(source: &str, needle: &str) {
    let error = run_file(source, &[]).expect_err(needle);
    assert!(error.contains(needle), "missing {needle:?} in {error}");
}

#[test]
fn string_byte_lengths_match_runtime_witness() {
    let run = run_file(LENGTHS, &[]).expect("length fixture");
    assert!(
        run.imports
            .iter()
            .any(|name| name == candidate::STRING_BYTE_LEN),
        "length must be a runtime call, not a folded constant: {:?} {}",
        run.imports,
        run.path.display()
    );
    assert_floats(
        &run.notes,
        &[0.0, 0.0, 3.0, 3.0, 4.0, 4.0, 3.0, 3.0, 3.0, 3.0, 3.0, 3.0],
        "ordered byte lengths",
    );
    assert_eq!(run.stack, vec![Value::Float(3.0)], "chunk return");
    assert_eq!(run.reads, 1, "dynamic getter read once");

    let hash_type = run_file(TYPE_HASH, &[]).expect("type of hash");
    assert_eq!(hash_type.stack, vec![Value::string("number")]);
    let rawlen_type = run_file(TYPE_RAWLEN, &[]).expect("type of rawlen");
    assert_eq!(rawlen_type.stack, vec![Value::string("number")]);
}

#[test]
fn string_byte_length_evaluates_operand_once() {
    let first = ["ab", "wxyz", "x", "abcdef"];
    let once = run_file(ONCE, &first).expect("once, first input");
    assert_floats(&once.notes, &[2.0], "hash uses the first text");
    assert_eq!(
        once.stack,
        vec![Value::Float(4.0)],
        "rawlen uses the second text"
    );
    assert_eq!(once.calls, 2, "one evaluation per operator");

    let swapped = ["wxyz", "ab", "abcdef", "x"];
    let other = run_file(ONCE, &swapped).expect("once, swapped input");
    assert_floats(&other.notes, &[4.0], "swapped hash");
    assert_eq!(other.stack, vec![Value::Float(2.0)], "swapped rawlen");
    assert_eq!(other.calls, 2, "swapped input is still one evaluation each");

    let getter = run_file(GETTER_RAWLEN, &[]).expect("getter rawlen");
    assert_eq!(getter.stack, vec![Value::Float(2.0)]);
    assert_eq!(getter.reads, 1, "rawlen(get()) reads the cell once");
    let hashed = run_file(GETTER_HASH, &[]).expect("getter hash");
    assert_eq!(hashed.stack, vec![Value::Float(4.0)]);
    assert_eq!(hashed.reads, 1, "#get() reads the cell once");
}

#[test]
fn shadowed_rawlen_is_a_normal_call() {
    let local_fn = run_file(SHADOW_LOCAL, &[]).expect("local rawlen");
    assert_eq!(local_fn.stack, vec![Value::Float(6.0)]);
    assert_eq!(local_fn.reads, 1, "local rawlen() reads its cell");
    assert!(
        local_fn
            .imports
            .iter()
            .any(|name| name == candidate::STRING_BYTE_LEN),
        "#seen must call the length operation"
    );

    let script = run_file(SHADOW_FUNCTION, &[]).expect("script rawlen");
    assert_eq!(script.stack, vec![Value::Float(7.0)]);
    assert!(
        !script
            .imports
            .iter()
            .any(|name| name == candidate::STRING_BYTE_LEN),
        "a script function named rawlen is not the length operation"
    );
    let error = run_file(SHADOW_FUNCTION_STRING, &[]).expect_err("string return");
    assert!(
        error.contains("return element is not a pass-through value"),
        "unsupported string return keeps its diagnostic, not a length: {error}"
    );
}

#[test]
fn bound_rawlen_keeps_unsupported_diagnostic() {
    assert_rejected(BOUND_LOCAL_CALL, "not lowered yet");
    assert_rejected(BOUND_FUNCTION_CALL, "not lowered yet");
    assert_rejected(RAWLEN_STRING_CALL, "not lowered yet");
    assert_rejected(RAWLEN_NO_ARG, "not lowered yet");
}

#[test]
fn non_string_length_is_rejected() {
    assert_rejected(NUMBER_OPERAND, "Lua length operand is not a string");
    assert_rejected(NIL_OPERAND, "Lua length operand is not a string");
    assert_rejected(TABLE_OPERAND, "Lua length operand is not a string");
}
