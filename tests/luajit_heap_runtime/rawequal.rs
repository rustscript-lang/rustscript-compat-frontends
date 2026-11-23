//! Unbound `rawequal` for the Lua 5.2 candidate.
//!
//! Two static arguments are evaluated left to right and compared without
//! `__eq`, structural equality, or a host integer treated as a Lua number.
//! A fresh table argument is rooted only until the call returns. Extra static
//! arguments are evaluated and ignored. Fewer than two arguments is a lower
//! refusal, not an implicit nil: this candidate has no argument frame that
//! can raise LuaJIT's runtime "value expected".
//!
//! String-call syntax, a trailing `next` adjustment, method calls, and a
//! nested function that reads a chunk local are not lowered. Closures cannot
//! return `type` of a weak field, so the collection witness uses
//! `observe_table`, which records a handle without keeping the carrier.

use std::sync::Arc;

use vm::{
    CallOutcome, CallReturn, CompileSourceFileOptions, FrontendImportSyntax, FrontendIr,
    HostFunctionRegistry, HostImportSchema, HostOwnedFunction, OwnedHostCall, ParseError,
    SourceFlavor, SourcePlugin, Value, VmError, VmStatus,
};

use super::candidate;
use super::heap;
use super::{catalog, register_witnesses};

struct NoteHost;

#[derive(Clone, Debug, Default)]
struct NoteTags {
    pairs: Vec<(f64, bool)>,
}

impl HostOwnedFunction for NoteHost {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        let (tag, value) = match call.args() {
            [Value::Float(tag), Value::Bool(value)] => (*tag, *value),
            _ => {
                return Err(VmError::HostError(
                    "note_bool expected a tag and a bool".into(),
                ));
            }
        };
        call.vm()
            .host_context()
            .scope_state_or_insert_with(NoteTags::default)
            .map_err(|error| VmError::HostError(error.to_string()))?
            .pairs
            .push((tag, value));
        Ok(CallOutcome::Return(CallReturn::one(Value::Bool(value))))
    }
}

struct RawequalPlugin;

impl SourcePlugin for RawequalPlugin {
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

struct Attempt {
    vm: vm::Vm,
    stack: Option<Vec<Value>>,
}

fn attempt(source: &str) -> Result<Attempt, String> {
    let catalog = Arc::clone(catalog());
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&RawequalPlugin)
        .with_host_api_catalog(Arc::clone(&catalog));
    let mut vm = vm::compile_source_with_flavor_and_options(source, SourceFlavor::Lua, options)
        .map_err(|error| error.to_string())?
        .into_vm()
        .map_err(|error| error.to_string())?;
    let mut registry = HostFunctionRegistry::empty();
    candidate::register(&mut registry, &catalog).map_err(|error| error.to_string())?;
    register_witnesses(&mut registry, &catalog);
    let note = catalog
        .function("lua_host::note_bool")
        .expect("note_bool schema");
    registry
        .register_exact_owned(
            "lua_host::note_bool",
            note.params.len() as u8,
            HostImportSchema::from_function(&catalog, note),
            |_| Box::new(NoteHost),
        )
        .map_err(|error| error.to_string())?;
    registry
        .bind_vm_cached(&mut vm)
        .map_err(|error| error.to_string())?;
    let status = vm.run().map_err(|error| error.to_string())?;
    let stack = (status == VmStatus::Halted).then(|| vm.stack().to_vec());
    Ok(Attempt { vm, stack })
}

fn halted(source: &str) -> Attempt {
    let ran = attempt(source).unwrap_or_else(|error| panic!("rawequal source failed: {error}"));
    assert!(
        ran.stack.is_some(),
        "rawequal source must halt, stack {:?}",
        ran.vm.stack()
    );
    ran
}

fn bool_result(source: &str) -> bool {
    let ran = halted(source);
    match ran.stack.as_deref() {
        Some([Value::Bool(value)]) => *value,
        other => panic!("expected one bool, got {other:?}"),
    }
}

fn notes_of(ran: &mut Attempt) -> Vec<(f64, bool)> {
    ran.vm
        .host_context()
        .scope_state::<NoteTags>()
        .map(|note| note.pairs.clone())
        .unwrap_or_default()
}

fn alive(ran: &mut Attempt, table: heap::TableRef) -> bool {
    heap::with_heap(&mut ran.vm.host_context(), |heap| Ok(heap.alive(table))).expect("alive")
}

const ALIAS: &str = "\
local t = {}
local alias = t
local other = {}
return rawequal(t, alias)
";

const DISTINCT: &str = "\
local t = {}
local other = {}
return rawequal(t, other)
";

const SAME_TABLE: &str = "\
local t = {}
return rawequal(t, t)
";

const EQ_FIELD: &str = "\
local a = {}
local b = {}
local mt = {}
mt.__eq = \"yes\"
setmetatable(a, mt)
setmetatable(b, mt)
note_bool(1, rawequal(a, b))
note_bool(2, rawequal(a, a))
return mt.__eq
";

const CYCLE: &str = "\
local t = {}
t.self = t
return rawequal(t, t.self)
";

const SCALARS: &str = "\
note_bool(1, rawequal(nil, nil))
note_bool(2, rawequal(false, nil))
note_bool(3, rawequal(true, true))
note_bool(4, rawequal(true, false))
note_bool(5, rawequal(1, \"1\"))
note_bool(6, rawequal(0, float_bits(1)))
note_bool(7, rawequal(float_bits(2), float_bits(2)))
note_bool(8, rawequal(\"a\\0b\", \"a\\0c\"))
note_bool(9, rawequal(\"a\\0b\", \"a\\0b\"))
note_bool(10, rawequal(\"\\255\\0a\", \"\\255\\0a\"))
note_bool(11, rawequal(\"\\255\\0a\", \"\\255\\0b\"))
note_bool(12, rawequal(\"ab\", byte_text()))
note_bool(13, rawequal(\"abc\", byte_text()))
note_bool(14, rawequal(byte_text(), byte_text()))
return true
";

const FORGED_SHAPE: &str = "\
return rawequal(external_shape(), {})
";

const FORGED_COPY: &str = "\
local t = {}
return rawequal(t, copy_fields(t))
";

const HOST_INT: &str = "\
return rawequal(host_int(), 1)
";

const HOST_INT_PAIR: &str = "\
return rawequal(host_int(), host_int())
";

const ORDER: &str = "\
return rawequal(note_bool(1, true), note_bool(2, false))
";

const REVERSE_ORDER: &str = "\
return rawequal(note_bool(2, true), note_bool(1, true))
";

const EXTRA: &str = "\
return rawequal(nil, nil, note_bool(9, true))
";

const MISSING_SECOND: &str = "\
return rawequal(nil)
";

const MISSING_BOTH: &str = "\
return rawequal()
";

const STRING_CALL: &str = "\
return rawequal\"ab\"
";

const TAIL_NEXT: &str = "\
local t = {}
return rawequal(t, next(t))
";

const BOUND_LOCAL: &str = "\
local rawequal = 1
return rawequal(nil, nil)
";

const BOUND_UPVALUE: &str = "\
local rawequal = nil
local function probe()
  local ok = rawequal(nil, nil)
  return ok
end
return 1
";

const UNBOUND_IN_FUNCTION: &str = "\
local function probe()
  note_bool(1, rawequal(nil, nil))
end
probe()
return 1
";

const WEAK_PREFIX: &str = "\
local mt = {}
mt.__mode = \"v\"
local weak = {}
setmetatable(weak, mt)
local orphan = {}
orphan.self = orphan
weak[1] = orphan
observe_table(orphan, 0)
orphan = nil
";

const COLLECT_SECOND: &str = "\
local eq = rawequal({}, (collectgarbage(\"collect\")))
return eq
";

const COLLECT_FIRST: &str = "\
local eq = rawequal((collectgarbage(\"collect\")), {})
return eq
";

const OMIT_COLLECT: &str = "\
local eq = rawequal({}, 0)
return eq
";

const COLLECT_THEN_RELEASE: &str = "\
local eq = rawequal({}, (collectgarbage(\"collect\")))
collectgarbage(\"collect\")
return eq
";

fn weak_source(arm: &str) -> String {
    format!("{WEAK_PREFIX}{arm}")
}

fn refuse(source: &str) {
    let error = match attempt(source) {
        Err(error) => error,
        Ok(_) => panic!("bound or incomplete rawequal must not run"),
    };
    assert!(
        error.contains("Lua expression not lowered yet"),
        "refusal must stay a lower error, not a nil result: {error}"
    );
    assert!(
        !error.contains("true"),
        "omission must not be reported as a true comparison: {error}"
    );
}

#[test]
fn lua_file_rawequal_compares_aliases_and_not_distinct_tables() {
    assert!(bool_result(ALIAS), "an alias is the same issued table");
    assert!(bool_result(SAME_TABLE), "a table equals itself");
    assert!(
        !bool_result(DISTINCT),
        "a different empty table is not structurally equal"
    );
}

#[test]
fn lua_file_rawequal_does_not_dispatch_eq_or_walk_a_cycle() {
    let mut ran = halted(EQ_FIELD);
    assert_eq!(ran.stack.as_deref(), Some(&[Value::string("yes")][..]));
    assert_eq!(
        notes_of(&mut ran),
        vec![(1.0, false), (2.0, true)],
        "__eq must not make distinct tables equal, and the same table stays true"
    );
    let mut cycle = halted(CYCLE);
    assert_eq!(cycle.stack.as_deref(), Some(&[Value::Bool(true)][..]));
    let stats =
        heap::with_heap(&mut cycle.vm.host_context(), |heap| Ok(heap.stats())).expect("stats");
    assert_eq!(
        stats.live_edges, 1,
        "identity comparison must not walk the cycle"
    );
    assert_eq!(
        stats.collections, 0,
        "the cycle comparison does not collect"
    );
}

#[test]
fn lua_file_rawequal_compares_scalars_and_string_bytes() {
    let mut ran = halted(SCALARS);
    assert_eq!(
        notes_of(&mut ran),
        vec![
            (1.0, true),
            (2.0, false),
            (3.0, true),
            (4.0, false),
            (5.0, false),
            (6.0, true),
            (7.0, false),
            (8.0, false),
            (9.0, true),
            (10.0, true),
            (11.0, false),
            (12.0, true),
            (13.0, false),
            (14.0, true),
        ]
    );
}

#[test]
fn lua_file_rawequal_rejects_forged_carriers_and_host_integers() {
    let shape = match attempt(FORGED_SHAPE) {
        Err(error) => error,
        Ok(_) => panic!("an unsigned shape is not a table"),
    };
    assert!(
        shape.contains("not an issued table"),
        "forged shape must be rejected, not compared: {shape}"
    );
    let copied = match attempt(FORGED_COPY) {
        Err(error) => error,
        Ok(_) => panic!("a field copy is not the issued table"),
    };
    assert!(
        copied.contains("not an issued table"),
        "structural copy must be rejected, not equal: {copied}"
    );
    for source in [HOST_INT, HOST_INT_PAIR] {
        let error = match attempt(source) {
            Err(error) => error,
            Ok(_) => panic!("a host integer is not a Lua number"),
        };
        assert!(
            error.contains("host integer"),
            "host integer must not compare as a number or as false: {error}"
        );
    }
}

#[test]
fn lua_file_rawequal_evaluates_static_arguments_left_to_right() {
    let mut forward = halted(ORDER);
    assert_eq!(forward.stack.as_deref(), Some(&[Value::Bool(false)][..]));
    assert_eq!(notes_of(&mut forward), vec![(1.0, true), (2.0, false)]);
    let mut reverse = halted(REVERSE_ORDER);
    assert_eq!(reverse.stack.as_deref(), Some(&[Value::Bool(true)][..]));
    assert_eq!(
        notes_of(&mut reverse),
        vec![(2.0, true), (1.0, true)],
        "argument order is source order, not tag order"
    );
    let mut extra = halted(EXTRA);
    assert_eq!(extra.stack.as_deref(), Some(&[Value::Bool(true)][..]));
    assert_eq!(
        notes_of(&mut extra),
        vec![(9.0, true)],
        "an extra static argument is evaluated, then ignored"
    );
}

#[test]
fn lua_file_rawequal_refuses_missing_arguments_and_bound_names() {
    refuse(MISSING_SECOND);
    refuse(MISSING_BOTH);
    refuse(STRING_CALL);
    refuse(TAIL_NEXT);
    refuse(BOUND_LOCAL);
    refuse(BOUND_UPVALUE);
    let mut unbound = halted(UNBOUND_IN_FUNCTION);
    assert_eq!(unbound.stack.as_deref(), Some(&[Value::Float(1.0)][..]));
    assert_eq!(
        notes_of(&mut unbound),
        vec![(1.0, true)],
        "an unbound name inside a function is the builtin"
    );
}

#[test]
fn lua_file_rawequal_roots_the_first_table_only_until_the_call_returns() {
    let mut kept = halted(&weak_source(COLLECT_SECOND));
    assert_eq!(kept.stack.as_deref(), Some(&[Value::Bool(false)][..]));
    let kept_stats =
        heap::with_heap(&mut kept.vm.host_context(), |heap| Ok(heap.stats())).expect("kept stats");
    assert_eq!(
        kept_stats.collections, 1,
        "the second argument collects once"
    );
    assert_eq!(
        kept_stats.last_marked, 3,
        "metatable, weak table, and the unrooted first argument are marked"
    );
    assert_eq!(
        kept_stats.live_nodes, 3,
        "the first argument stays live through that collection"
    );
    let kept_seen = super::observed_tables(&mut kept.vm);
    assert_eq!(
        kept_seen.len(),
        1,
        "the weak orphan was observed before collection"
    );
    assert!(
        !alive(&mut kept, kept_seen[0].table),
        "the independent weak orphan must actually be collected"
    );

    let mut released = halted(&weak_source(COLLECT_THEN_RELEASE));
    let released_stats =
        heap::with_heap(&mut released.vm.host_context(), |heap| Ok(heap.stats())).expect("release");
    assert_eq!(
        released_stats.collections, 2,
        "the call returns, then a second collect runs"
    );
    assert_eq!(
        released_stats.last_marked, 2,
        "after the call, the first argument has no root"
    );
    assert_eq!(
        released_stats.live_nodes, 2,
        "the returned first argument can be collected"
    );

    let mut omitted = halted(&weak_source(OMIT_COLLECT));
    let omitted_stats =
        heap::with_heap(&mut omitted.vm.host_context(), |heap| Ok(heap.stats())).expect("omit");
    assert_eq!(omitted_stats.collections, 0, "the control does not collect");
    assert_eq!(
        omitted_stats.live_nodes, 4,
        "the orphan stays when collection is omitted"
    );
    let omitted_seen = super::observed_tables(&mut omitted.vm);
    assert!(
        alive(&mut omitted, omitted_seen[0].table),
        "without a collection the weak orphan stays live"
    );

    let mut reversed = halted(&weak_source(COLLECT_FIRST));
    let reversed_stats =
        heap::with_heap(&mut reversed.vm.host_context(), |heap| Ok(heap.stats())).expect("reverse");
    assert_eq!(reversed_stats.collections, 1);
    assert_eq!(
        reversed_stats.last_marked, 2,
        "collection in the first argument happens before the table exists"
    );
    let reversed_seen = super::observed_tables(&mut reversed.vm);
    assert!(
        !alive(&mut reversed, reversed_seen[0].table),
        "the weak orphan is collected when the collect runs first"
    );
}

#[test]
fn rawequal_is_not_a_host_catalog_entry() {
    assert!(catalog().function("lua_internal::new_table").is_some());
    assert!(catalog().function("lua_host::rawequal").is_none());
}
