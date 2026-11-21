//! Real uncaptured Lua calls and the explicit return-root handoff.
//!
//! The sources are legal Lua. The callee's `Ret` still carries one value, the
//! private ordered result array. The caller binds that array into locals and
//! reads those locals. `self` is an ordinary field name. Numeric keys are refused.

use super::candidate::{self, EdgeView, ReturnWitness, ReturnedElement};
use super::{catalog, heap, register_witnesses};
use std::sync::Arc;
use vm::{
    CompileSourceFileOptions, FrontendImportSyntax, FrontendIr, HostFunctionRegistry, OpCode,
    ParseError, Program, SourceFlavor, SourcePlugin, Value, VmStatus,
};

#[derive(Clone, Debug, PartialEq)]
struct CallerReads {
    field_same: bool,
    gap: String,
    missing: String,
    single: String,
    total: f64,
    cleared: String,
}

fn register_caller_reads(registry: &mut HostFunctionRegistry, catalog: &vm::HostApiCatalog) {
    let function = catalog
        .function("lua_host::caller_reads")
        .expect("caller reads schema");
    registry
        .register_exact_owned(
            "lua_host::caller_reads",
            function.params.len() as u8,
            vm::HostImportSchema::from_function(catalog, function),
            |_| Box::new(CallerReadsHost),
        )
        .expect("register caller reads");
}

struct CallerReadsHost;

impl vm::HostOwnedFunction for CallerReadsHost {
    fn call(&mut self, call: &mut vm::OwnedHostCall<'_>) -> Result<vm::CallOutcome, vm::VmError> {
        let [
            Value::Bool(field_same),
            Value::String(gap),
            Value::String(missing),
            Value::String(single),
            Value::Float(total),
            Value::String(cleared),
        ] = call.args()
        else {
            return Err(vm::VmError::HostError(
                "caller reads need the Lua local observations".to_string(),
            ));
        };
        let cleared = cleared.to_string();
        let reads = CallerReads {
            field_same: *field_same,
            gap: gap.to_string(),
            missing: missing.to_string(),
            single: single.to_string(),
            total: *total,
            cleared: cleared.clone(),
        };
        call.vm()
            .host_context()
            .scope_state_or_insert_with(|| reads)
            .map_err(|error| vm::VmError::HostError(error.to_string()))?;
        Ok(vm::CallOutcome::Return(vm::CallReturn::one(Value::string(
            &cleared,
        ))))
    }
}

#[derive(Clone, Copy)]
enum BindingObs {
    Record,
    Bool,
    Text,
    Stage,
    Snap,
}

fn register_binding_obs(registry: &mut HostFunctionRegistry, catalog: &vm::HostApiCatalog) {
    for (name, op) in [
        ("lua_host::record_binding", BindingObs::Record),
        ("lua_host::note_bool", BindingObs::Bool),
        ("lua_host::note_text", BindingObs::Text),
        ("lua_host::note_stage", BindingObs::Stage),
        ("lua_host::snapshot_bindings", BindingObs::Snap),
    ] {
        let function = catalog.function(name).expect(name);
        registry
            .register_exact_owned(
                name,
                function.params.len() as u8,
                vm::HostImportSchema::from_function(catalog, function),
                move |_| Box::new(op),
            )
            .expect(name);
    }
}

#[derive(Clone, Debug, Default)]
struct ObsLog {
    stage: f64,
    bools: Vec<(f64, bool)>,
    texts: Vec<(f64, String)>,
    bindings: Vec<(f64, heap::TableRef)>,
    snaps: Vec<BindingSnap>,
}

#[derive(Clone, Debug)]
struct BindingSnap {
    tag: f64,
    live_nodes: u32,
    live_roots: u32,
    collections: u32,
    alive: Vec<(f64, bool)>,
}

impl vm::HostOwnedFunction for BindingObs {
    fn call(&mut self, call: &mut vm::OwnedHostCall<'_>) -> Result<vm::CallOutcome, vm::VmError> {
        let args = call.args().to_vec();
        let mut host = call.vm().host_context();
        let result = match self {
            Self::Record => {
                let [Value::Float(tag), token] = args.as_slice() else {
                    return Err(obs_fault("record_binding needs a tag and a table"));
                };
                let table = borrowed_table(&mut host, token)?;
                let tag = *tag;
                host.scope_state_or_insert_with(ObsLog::default)
                    .map_err(|error| vm::VmError::HostError(error.to_string()))?
                    .bindings
                    .push((tag, table));
                Value::Bool(true)
            }
            Self::Bool => {
                let [Value::Float(tag), Value::Bool(value)] = args.as_slice() else {
                    return Err(obs_fault("note_bool needs a tag and a boolean"));
                };
                let noted = (*tag, *value);
                host.scope_state_or_insert_with(ObsLog::default)
                    .map_err(|error| vm::VmError::HostError(error.to_string()))?
                    .bools
                    .push(noted);
                Value::Bool(true)
            }
            Self::Text => {
                let [Value::Float(tag), Value::String(value)] = args.as_slice() else {
                    return Err(obs_fault("note_text needs a tag and a string"));
                };
                let noted = (*tag, value.to_string());
                host.scope_state_or_insert_with(ObsLog::default)
                    .map_err(|error| vm::VmError::HostError(error.to_string()))?
                    .texts
                    .push(noted);
                Value::Bool(true)
            }
            Self::Stage => {
                let [Value::Float(tag)] = args.as_slice() else {
                    return Err(obs_fault("note_stage needs a tag"));
                };
                let tag = *tag;
                host.scope_state_or_insert_with(ObsLog::default)
                    .map_err(|error| vm::VmError::HostError(error.to_string()))?
                    .stage = tag;
                Value::Bool(true)
            }
            Self::Snap => {
                let [Value::Float(tag)] = args.as_slice() else {
                    return Err(obs_fault("snapshot_bindings needs a tag"));
                };
                let tag = *tag;
                let recorded = host
                    .scope_state::<ObsLog>()
                    .map(|log| log.bindings.clone())
                    .unwrap_or_default();
                let (alive, live_nodes, live_roots, collections) =
                    heap::with_heap(&mut host, |heap| {
                        Ok((
                            recorded
                                .iter()
                                .map(|(binding, table)| (*binding, heap.alive(*table)))
                                .collect::<Vec<_>>(),
                            heap.stats().live_nodes,
                            heap.stats().live_roots,
                            heap.stats().collections,
                        ))
                    })
                    .map_err(|error| vm::VmError::HostError(error.to_string()))?;
                host.scope_state_or_insert_with(ObsLog::default)
                    .map_err(|error| vm::VmError::HostError(error.to_string()))?
                    .snaps
                    .push(BindingSnap {
                        tag,
                        live_nodes,
                        live_roots,
                        collections,
                        alive,
                    });
                Value::Bool(true)
            }
        };
        Ok(vm::CallOutcome::Return(vm::CallReturn::one(result)))
    }
}

fn borrowed_table(
    host: &mut vm::HostContext<'_>,
    value: &Value,
) -> Result<heap::TableRef, vm::VmError> {
    let Value::Int(raw) = value else {
        return Err(obs_fault("expected Lua table"));
    };
    let raw = u64::try_from(*raw).map_err(|_| obs_fault("expected Lua table"))?;
    let handle = vm::ResourceHandle::from_raw(raw).map_err(|_| obs_fault("expected Lua table"))?;
    let key = heap::LuaTableResource::key();
    host.borrow_resource_with_key::<heap::LuaTableResource>(handle, &key)
        .map(|resource| resource.table())
        .map_err(|error| vm::VmError::HostError(error.to_string()))
}

fn obs_fault(message: &str) -> vm::VmError {
    vm::VmError::HostError(message.to_string())
}

fn obs_log(vm: &mut vm::Vm) -> ObsLog {
    vm.host_context()
        .scope_state::<ObsLog>()
        .cloned()
        .unwrap_or_default()
}

fn require_halted(vm: &mut vm::Vm) {
    match vm.run() {
        Ok(VmStatus::Halted) => {}
        Ok(other) => panic!(
            "status {other:?} after stage {}; log={:?}",
            obs_log(vm).stage,
            obs_log(vm)
        ),
        Err(error) => panic!(
            "run failed after stage {}: {error}; log={:?}",
            obs_log(vm).stage,
            obs_log(vm)
        ),
    }
}

struct PublishingReturn;
struct UnpublishedReturn;

impl SourcePlugin for PublishingReturn {
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
        candidate::lower_returning(source, catalog(), true)
    }
}

impl SourcePlugin for UnpublishedReturn {
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
        candidate::lower_returning(source, catalog(), false)
    }
}

fn run(source: &str, publish_results: bool) -> vm::Vm {
    let catalog = Arc::clone(catalog());
    let options = if publish_results {
        CompileSourceFileOptions::new().with_source_plugin(&PublishingReturn)
    } else {
        CompileSourceFileOptions::new().with_source_plugin(&UnpublishedReturn)
    }
    .with_host_api_catalog(Arc::clone(&catalog));
    let mut vm = vm::compile_source_with_flavor_and_options(source, SourceFlavor::Lua, options)
        .expect("compile Lua text")
        .into_vm()
        .expect("create VM");
    let mut registry = HostFunctionRegistry::empty();
    candidate::register(&mut registry, &catalog).expect("register candidate runtime");
    register_witnesses(&mut registry, &catalog);
    register_caller_reads(&mut registry, &catalog);
    register_binding_obs(&mut registry, &catalog);
    registry
        .bind_vm_cached(&mut vm)
        .expect("bind candidate runtime");
    vm.set_jit_config(vm::JitConfig {
        enabled: false,
        hot_loop_threshold: 0,
        max_trace_len: 0,
    });
    vm
}

fn witness(vm: &mut vm::Vm) -> ReturnWitness {
    vm.host_context()
        .scope_state::<ReturnWitness>()
        .cloned()
        .expect("return witness")
}

const ZERO: &str = "\
local function none()
  return
end
local n = none()
return n
";

const ONE_NIL: &str = "\
local function one()
  return nil
end
local n = one()
return n
";

const TABLE_RETURN: &str = "\
local function make()
  local returned = {}
  local discarded = {}
  local alias = returned
  alias.self = returned
  local other = discarded
  other.self = discarded
  return returned, nil, 9
end
local kept = make()
collectgarbage(\"collect\")
kept = nil
collectgarbage(\"collect\")
";

const SEQUENTIAL: &str = "\
local function make()
  local returned = {}
  return returned
end
local function zero()
  return
end
local kept = make()
local missing = zero()
return type(missing)
";

const NONE_THEN_NIL: &str = "\
local function none()
  return
end
local n = none()
n = nil
return type(n)
";

const ORDINARY_NIL: &str = "\
local n = 1
n = nil
return type(n)
";

const CONSUME: &str = "\
local function make()
  local returned = {}
  local discarded = {}
  local alias = returned
  alias.self = returned
  local other = discarded
  other.self = discarded
  return returned, nil, 9
end
local function zero()
  return
end
local function one()
  return nil
end
local kept, gap, value = make()
local missing = zero()
local single = one()
local field_ok = same_table(kept, kept.self)
local gap_kind = type(gap)
local missing_kind = type(missing)
local single_kind = type(single)
local total = value + 1
collectgarbage(\"collect\")
kept = nil
local cleared = type(kept)
collectgarbage(\"collect\")
return caller_reads(field_ok, gap_kind, missing_kind, single_kind, total, cleared)
";

const VALUE_ORACLE: &str = "\
local function make()
  local returned = {}
  returned.self = returned
  return returned, nil, 9
end
local function zero()
  return
end
local function one()
  return nil
end
local kept, gap, value = make()
local missing = zero()
local single = one()
local total = value + 1
local field_same = kept.self == kept
kept = nil
io.write(tostring(field_same), \" \", tostring(gap == nil), \" \", tostring(missing == nil), \" \", tostring(single == nil), \" \", tostring(total), \" \", tostring(kept == nil), \"\\n\")
local n = 1
n = nil
io.write(tostring(n == nil), \"\\n\")
local function none()
  return
end
local m = none()
m = nil
io.write(tostring(m == nil), \"\\n\")
";

#[test]
fn zero_return_uses_a_real_frame_and_exact_arity() {
    let mut vm = run(ZERO, true);
    assert_eq!(vm.run().unwrap(), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::Null]);
    let witness = witness(&mut vm);
    assert!(witness.elements.is_empty());
    assert_retired_call(&witness);
    assert!(!witness.published_root);
    assert!(witness.collects.is_empty());
    assert_real_return_window(vm.program());
}

#[test]
fn one_nil_keeps_exact_arity_and_nil_position() {
    let mut vm = run(ONE_NIL, true);
    assert_eq!(vm.run().unwrap(), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::Null]);
    let witness = witness(&mut vm);
    assert!(matches!(
        witness.elements.as_slice(),
        [ReturnedElement::Nil]
    ));
    assert_retired_call(&witness);
    assert_real_return_window(vm.program());
}

#[test]
fn table_nil_number_handoff_keeps_the_returned_object_only() {
    let mut vm = run(TABLE_RETURN, true);
    assert_eq!(vm.run().unwrap(), VmStatus::Halted);
    let witness = witness(&mut vm);
    assert_eq!(witness.elements.len(), 3);
    match witness.elements.as_slice() {
        [
            ReturnedElement::Table {
                same_ref: true,
                same_carrier: true,
            },
            ReturnedElement::Nil,
            ReturnedElement::Number(number),
        ] => assert_eq!(*number, 9.0),
        other => panic!("unexpected return elements: {other:?}"),
    }
    let returned = witness.returned.expect("issued returned table");
    let discarded = witness.discarded.expect("issued discarded table");
    assert_ne!(returned, discarded);
    assert_eq!(witness.returned_edge_at_handoff, EdgeView::Live(returned));
    assert_eq!(witness.discarded_edge_at_handoff, EdgeView::Live(discarded));
    assert!(witness.published_root);
    assert!(witness.callee_left);
    assert!(witness.top_is_caller_after_handoff);
    assert!(
        witness.lua_frame_alive_at_publish,
        "Core has already Ret; the callee Lua frame is a separate fact"
    );
    assert_retired_call(&witness);
    let first = &witness.collects[0];
    assert!(first.returned_alive);
    assert_eq!(first.returned_edge, EdgeView::Live(returned));
    assert!(!first.discarded_alive);
    assert_eq!(first.marked_nodes, 1);
    assert_eq!(first.live_roots, 1);
    assert_eq!(first.live_nodes, 1);
    assert_eq!(first.swept, 1);
    let release = witness.release.expect("caller dropped the result root");
    assert!(release.returned_alive);
    assert_eq!(release.returned_edge, EdgeView::Live(returned));
    assert_eq!(release.live_roots, 0);
    let second = &witness.collects[1];
    assert!(!second.returned_alive);
    assert_eq!(second.returned_edge, EdgeView::Stale);
    assert_stale_edge(&mut vm, returned, witness.field_key);
    assert!(!second.discarded_alive);
    assert_eq!(second.marked_nodes, 0);
    assert_eq!(second.live_roots, 0);
    assert_eq!(second.live_nodes, 0);
    assert_eq!(second.swept, 1);
    assert_real_return_window(vm.program());
}

#[test]
fn omitting_only_the_result_root_drops_the_returned_table_on_first_collect() {
    let mut vm = run(TABLE_RETURN, false);
    assert_eq!(vm.run().unwrap(), VmStatus::Halted);
    let witness = witness(&mut vm);
    assert!(!witness.published_root);
    assert!(witness.callee_left);
    assert_retired_call(&witness);
    let first = &witness.collects[0];
    assert!(!first.returned_alive, "the returned table is unusable");
    assert_eq!(first.returned_edge, EdgeView::Stale);
    assert_stale_edge(
        &mut vm,
        witness.returned.expect("issued returned table"),
        witness.field_key,
    );
    assert!(!first.discarded_alive);
    assert_eq!(first.marked_nodes, 0);
    assert_eq!(first.live_roots, 0);
    assert_eq!(first.swept, 2);
    assert_real_return_window(vm.program());
}

#[test]
fn sequential_calls_do_not_reuse_the_previous_guest_result() {
    let mut vm = run(SEQUENTIAL, true);
    assert_eq!(vm.run().unwrap(), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("nil")]);
    let witness = witness(&mut vm);
    assert_eq!(witness.handoffs.len(), 2);
    assert!(matches!(
        witness.handoffs[0].elements.as_slice(),
        [ReturnedElement::Table {
            same_ref: true,
            same_carrier: true,
        }]
    ));
    assert!(witness.handoffs[1].elements.is_empty());
    assert_eq!(
        witness.handoffs[0].callee_depth, witness.handoffs[1].callee_depth,
        "sibling calls share depth; they must not share the result array"
    );
    assert!(witness.handoffs[0].handoff_depth.is_some());
    assert_eq!(
        witness.handoffs[0].handoff_depth,
        witness.handoffs[1].handoff_depth
    );
    assert_eq!(witness.handoffs[0].returned_edge, EdgeView::Absent);
    assert_eq!(witness.handoffs[1].discarded_edge, EdgeView::Absent);
}

#[test]
fn nil_assignment_after_empty_return_writes_nil() {
    let mut vm = run(NONE_THEN_NIL, true);
    assert_eq!(vm.run().unwrap(), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("nil")]);
}

#[test]
fn ordinary_nil_assignment_writes_nil() {
    let mut vm = run(ORDINARY_NIL, true);
    assert_eq!(vm.run().unwrap(), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("nil")]);
}

#[test]
fn caller_reads_table_nil_and_number_then_clears_the_local() {
    assert_value_oracle();
    let mut vm = run(CONSUME, true);
    assert_eq!(vm.run().unwrap(), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::string("nil")]);
    let reads = vm
        .host_context()
        .scope_state::<CallerReads>()
        .cloned()
        .expect("Lua caller reads");
    assert_eq!(
        reads,
        CallerReads {
            field_same: true,
            gap: "nil".to_string(),
            missing: "nil".to_string(),
            single: "nil".to_string(),
            total: 10.0,
            cleared: "nil".to_string(),
        }
    );
    let witness = witness(&mut vm);
    assert_eq!(witness.handoffs.len(), 3);
    assert!(matches!(
        witness.handoffs[0].elements.as_slice(),
        [
            ReturnedElement::Table {
                same_ref: true,
                same_carrier: true,
            },
            ReturnedElement::Nil,
            ReturnedElement::Number(9.0),
        ]
    ));
    assert!(witness.handoffs[0].published_root);
    assert!(witness.handoffs[0].lua_frame_alive_at_publish);
    assert!(witness.handoffs[0].callee_left);
    assert!(witness.handoffs[1].elements.is_empty());
    assert!(!witness.handoffs[1].published_root);
    assert!(matches!(
        witness.handoffs[2].elements.as_slice(),
        [ReturnedElement::Nil]
    ));
    let returned = witness.returned.expect("issued returned table");
    let first = &witness.collects[0];
    assert!(first.returned_alive);
    assert!(!first.discarded_alive);
    assert_eq!(first.swept, 1);
    let second = &witness.collects[1];
    assert!(!second.returned_alive);
    assert_eq!(second.returned_edge, EdgeView::Stale);
    assert_stale_edge(&mut vm, returned, witness.field_key);
}

#[test]
fn unpublished_consume_source_still_reclaims_on_first_collect() {
    let mut vm = run(CONSUME, false);
    assert_eq!(vm.run().unwrap(), VmStatus::Halted);
    let witness = witness(&mut vm);
    assert!(!witness.published_root);
    let first = &witness.collects[0];
    assert!(!first.returned_alive);
    assert!(!first.discarded_alive);
    assert_eq!(first.swept, 2);
    assert_eq!(vm.stack(), &[Value::string("nil")]);
    assert!(!witness.handoffs[0].published_root);
    assert!(witness.handoffs[0].lua_frame_alive_at_publish);
    assert_eq!(first.returned_edge, EdgeView::Stale);
}

const ALIAS_RETURN: &str = r#"
local function make()
  local returned = {}
  local discarded = {}
  local alias = returned
  alias.self = returned
  discarded.self = discarded
  return alias
end
local kept = make()
record_binding(1, kept)
note_bool(2, same_table(kept, kept.self))
note_stage(10)
collectgarbage("collect")
snapshot_bindings(30)
note_stage(30)
note_bool(3, same_table(kept, kept.self))
record_binding(4, kept)
kept = nil
note_text(5, type(kept))
collectgarbage("collect")
snapshot_bindings(50)
"#;

const ALIAS_PAIR: &str = r#"
local function make()
  local returned = {}
  local discarded = {}
  local alias = returned
  alias.self = returned
  discarded.self = discarded
  return alias, nil, alias
end
local first, gap, last = make()
record_binding(1, first)
record_binding(2, last)
note_bool(3, same_table(first, last))
note_text(4, type(gap))
note_bool(5, same_table(first, first.self))
note_stage(10)
collectgarbage("collect")
snapshot_bindings(30)
note_stage(30)
note_bool(6, same_table(first, first.self))
note_bool(7, same_table(last, last.self))
first = nil
collectgarbage("collect")
snapshot_bindings(31)
note_stage(31)
note_bool(8, same_table(last, last.self))
record_binding(9, last)
last = nil
note_text(11, type(last))
collectgarbage("collect")
snapshot_bindings(50)
"#;

const MIXED_CLEAR_FIRST: &str = r#"
local function make()
  local returned = {}
  local discarded = {}
  local alias = returned
  alias.self = returned
  discarded.self = discarded
  return returned, nil, alias
end
local first, gap, last = make()
record_binding(1, first)
record_binding(2, last)
note_bool(3, same_table(first, last))
note_text(4, type(gap))
note_bool(5, same_table(first, first.self))
note_stage(10)
first = nil
note_stage(20)
collectgarbage("collect")
snapshot_bindings(30)
note_stage(30)
note_bool(8, same_table(last, last.self))
record_binding(9, last)
last = nil
note_text(11, type(last))
collectgarbage("collect")
snapshot_bindings(50)
"#;

const MIXED_CLEAR_LAST: &str = r#"
local function make()
  local returned = {}
  local discarded = {}
  local alias = returned
  alias.self = returned
  discarded.self = discarded
  return returned, nil, alias
end
local first, gap, last = make()
record_binding(1, first)
record_binding(2, last)
note_bool(3, same_table(first, last))
note_text(4, type(gap))
note_bool(5, same_table(last, last.self))
snapshot_bindings(11)
note_stage(10)
last = nil
note_stage(20)
collectgarbage("collect")
snapshot_bindings(30)
note_stage(30)
note_bool(8, same_table(first, first.self))
record_binding(9, first)
first = nil
note_text(11, type(first))
collectgarbage("collect")
snapshot_bindings(50)
"#;

const SIBLING_CALLS: &str = r#"
local function make()
  local returned = {}
  local discarded = {}
  returned.self = returned
  discarded.self = discarded
  return returned
end
local a = make()
local b = make()
record_binding(1, a)
record_binding(2, b)
note_bool(3, same_table(a, b))
note_bool(4, same_table(a, a.self))
note_bool(5, same_table(b, b.self))
note_stage(10)
a = nil
note_stage(20)
collectgarbage("collect")
snapshot_bindings(30)
note_stage(30)
note_bool(6, same_table(b, b.self))
record_binding(7, b)
b = nil
note_text(8, type(b))
collectgarbage("collect")
snapshot_bindings(50)
"#;

#[test]
fn alias_return_keeps_the_caller_binding_until_that_local_is_cleared() {
    let (mut vm, log) = run_obs(ALIAS_RETURN, true);
    let witness = witness(&mut vm);
    assert!(
        witness.returned.is_none(),
        "the alias is not selected from the constructor name"
    );
    assert!(witness.handoffs[0].published_root);
    assert!(witness.handoffs[0].lua_frame_alive_at_publish);
    assert!(witness.callee_left);
    assert!(bool_at(&log, 2.0), "setup field read");
    let kept = binding_at(&log, 1.0);
    let after = binding_at(&log, 4.0);
    assert_eq!(
        kept, after,
        "the guest binding is the same table after collect"
    );
    let first = snap_at(&log, 30.0);
    assert!(alive_at(first, 1.0), "caller binding keeps the alias");
    assert_eq!(first.live_nodes, 1, "the unreturned cycle is collected");
    assert_eq!(first.live_roots, 1);
    assert_eq!(first.collections, 1);
    assert!(bool_at(&log, 3.0), "field read after collect");
    assert_eq!(text_at(&log, 5.0), "nil");
    let cleared = snap_at(&log, 50.0);
    assert!(!alive_at(cleared, 1.0));
    assert_eq!(cleared.live_nodes, 0);
    assert_eq!(cleared.live_roots, 0);
    assert_eq!(cleared.collections, 2);
    assert_real_return_window(vm.program());
}

#[test]
fn alias_pair_keeps_either_binding_until_the_last_one_is_cleared() {
    let (vm, log) = run_obs(ALIAS_PAIR, true);
    assert!(bool_at(&log, 3.0), "both positions are the same table");
    assert_eq!(text_at(&log, 4.0), "nil");
    assert!(bool_at(&log, 5.0));
    let first_id = binding_at(&log, 1.0);
    let last_id = binding_at(&log, 2.0);
    assert_eq!(first_id, last_id);
    let both = snap_at(&log, 30.0);
    assert!(alive_at(both, 1.0) && alive_at(both, 2.0));
    assert_eq!(both.live_nodes, 1);
    assert_eq!(
        both.live_roots, 2,
        "each consumed position has its own slot"
    );
    assert!(bool_at(&log, 6.0) && bool_at(&log, 7.0));
    let one_left = snap_at(&log, 31.0);
    assert!(alive_at(one_left, 1.0));
    assert_eq!(one_left.live_roots, 1);
    assert_eq!(one_left.live_nodes, 1);
    assert!(
        bool_at(&log, 8.0),
        "the remaining local still reads the field"
    );
    assert_eq!(binding_at(&log, 9.0), last_id);
    assert_eq!(text_at(&log, 11.0), "nil");
    let gone = snap_at(&log, 50.0);
    assert!(!alive_at(gone, 1.0));
    assert_eq!(gone.live_nodes, 0);
    assert_eq!(gone.live_roots, 0);
    assert_real_return_window(vm.program());
}

#[test]
fn mixed_return_keeps_the_alias_when_the_constructor_binding_is_cleared() {
    let (vm, log) = run_obs(MIXED_CLEAR_FIRST, true);
    assert!(bool_at(&log, 3.0));
    assert_eq!(text_at(&log, 4.0), "nil");
    assert!(bool_at(&log, 5.0));
    let shared = binding_at(&log, 1.0);
    assert_eq!(shared, binding_at(&log, 2.0));
    let after_first = snap_at(&log, 30.0);
    assert!(
        alive_at(after_first, 2.0),
        "clearing the first local must not drop the alias"
    );
    assert_eq!(after_first.live_roots, 1);
    assert_eq!(after_first.live_nodes, 1);
    assert!(bool_at(&log, 8.0));
    assert_eq!(binding_at(&log, 9.0), shared);
    assert_eq!(text_at(&log, 11.0), "nil");
    let gone = snap_at(&log, 50.0);
    assert!(!alive_at(gone, 1.0));
    assert_eq!(gone.live_nodes, 0);
    assert_eq!(gone.live_roots, 0);
    assert_real_return_window(vm.program());
}

#[test]
fn mixed_return_keeps_the_constructor_binding_when_the_alias_is_cleared() {
    let (vm, log) = run_obs(MIXED_CLEAR_LAST, true);
    assert!(bool_at(&log, 3.0));
    assert_eq!(text_at(&log, 4.0), "nil");
    let shared = binding_at(&log, 1.0);
    assert_eq!(shared, binding_at(&log, 2.0));
    let before = snap_at(&log, 11.0);
    assert_eq!(
        before.live_roots, 2,
        "the alias position is rooted before either clear"
    );
    assert!(alive_at(before, 1.0) && alive_at(before, 2.0));
    let after_last = snap_at(&log, 30.0);
    assert!(alive_at(after_last, 1.0));
    assert_eq!(after_last.live_roots, 1);
    assert_eq!(after_last.live_nodes, 1);
    assert!(bool_at(&log, 8.0));
    assert_eq!(binding_at(&log, 9.0), shared);
    assert_eq!(text_at(&log, 11.0), "nil");
    let gone = snap_at(&log, 50.0);
    assert!(!alive_at(gone, 1.0));
    assert_eq!(gone.live_nodes, 0);
    assert_eq!(gone.live_roots, 0);
    assert_real_return_window(vm.program());
}

#[test]
fn earlier_sibling_call_can_be_cleared_without_collecting_the_later_table() {
    let (mut vm, log) = run_obs(SIBLING_CALLS, true);
    let a = binding_at(&log, 1.0);
    let b = binding_at(&log, 2.0);
    assert_ne!(
        a, b,
        "two calls of the same function allocate different tables"
    );
    assert!(!bool_at(&log, 3.0));
    assert!(bool_at(&log, 4.0) && bool_at(&log, 5.0));
    let witness = witness(&mut vm);
    assert_eq!(witness.handoffs.len(), 2);
    assert!(witness.handoffs[0].published_root && witness.handoffs[1].published_root);
    assert_eq!(
        witness.returned,
        Some(b),
        "the witness field is only the last call; the guest binding selects a"
    );
    let after_a = snap_at(&log, 30.0);
    assert!(!alive_at(after_a, 1.0), "clearing a collects only a");
    assert!(alive_at(after_a, 2.0));
    assert_eq!(after_a.live_nodes, 1);
    assert_eq!(after_a.live_roots, 1);
    assert!(bool_at(&log, 6.0), "b is still readable");
    assert_eq!(binding_at(&log, 7.0), b, "b did not become a");
    assert_ne!(binding_at(&log, 7.0), a);
    assert_eq!(text_at(&log, 8.0), "nil");
    let gone = snap_at(&log, 50.0);
    assert!(!alive_at(gone, 1.0) && !alive_at(gone, 2.0));
    assert_eq!(gone.live_nodes, 0);
    assert_eq!(gone.live_roots, 0);
    assert_eq!(gone.collections, 2);
    assert_real_return_window(vm.program());
}

const UNPUBLISHED_ALIAS: &str = r#"
local function make()
  local returned = {}
  local discarded = {}
  local alias = returned
  alias.self = returned
  discarded.self = discarded
  return alias
end
local kept = make()
record_binding(1, kept)
note_bool(2, same_table(kept, kept.self))
note_stage(10)
collectgarbage("collect")
snapshot_bindings(30)
note_stage(30)
"#;

const SCALAR_SHADOW: &str = r#"
local function make()
  local returned = {}
  returned.self = returned
  return returned
end
local a = make()
record_binding(1, a)
note_bool(2, same_table(a, a.self))
local a = 0
note_text(3, type(a))
a = nil
note_text(4, type(a))
note_stage(20)
snapshot_bindings(20)
collectgarbage("collect")
snapshot_bindings(30)
note_stage(30)
"#;

const CLEAR_BEFORE_SHADOW: &str = r#"
local function make()
  local returned = {}
  returned.self = returned
  return returned
end
local a = make()
record_binding(1, a)
note_bool(2, same_table(a, a.self))
snapshot_bindings(11)
a = nil
note_text(3, type(a))
local a = 0
note_text(4, type(a))
note_stage(20)
collectgarbage("collect")
snapshot_bindings(30)
note_stage(30)
"#;

const TABLE_SHADOW: &str = r#"
local function make()
  local returned = {}
  returned.self = returned
  return returned
end
local a = make()
record_binding(1, a)
note_bool(2, same_table(a, a.self))
local a = {}
record_binding(3, a)
note_text(4, type(a))
a = nil
note_text(5, type(a))
note_stage(20)
snapshot_bindings(20)
collectgarbage("collect")
snapshot_bindings(30)
note_stage(30)
"#;

const TABLE_RESULT_SHADOW: &str = r#"
local function make()
  local returned = {}
  returned.self = returned
  return returned
end
local function other()
  local returned = {}
  returned.self = returned
  return returned
end
local a = make()
record_binding(1, a)
note_bool(2, same_table(a, a.self))
local a = other()
record_binding(3, a)
note_bool(6, same_table(a, a.self))
note_text(4, type(a))
snapshot_bindings(15)
a = nil
note_text(5, type(a))
note_stage(20)
snapshot_bindings(20)
collectgarbage("collect")
snapshot_bindings(30)
note_stage(30)
"#;

#[test]
fn scalar_shadow_nil_keeps_the_earlier_table() {
    let (mut vm, log) = run_obs(SCALAR_SHADOW, true);
    let witness = witness(&mut vm);
    let old = binding_at(&log, 1.0);
    assert_eq!(
        witness.returned,
        Some(old),
        "the recorded binding is the issued table"
    );
    assert!(bool_at(&log, 2.0), "the earlier table was readable");
    assert_eq!(text_at(&log, 3.0), "number", "the new binding is a scalar");
    assert_eq!(
        text_at(&log, 4.0),
        "nil",
        "nil assignment wrote nil into the new scalar"
    );
    let before = snap_at(&log, 20.0);
    assert!(
        alive_at(before, 1.0),
        "the earlier table must still be allocated before collect: {before:?}"
    );
    let after = snap_at(&log, 30.0);
    let collect = witness.collects.first();
    assert!(
        alive_at(after, 1.0)
            && after.live_roots == 1
            && collect.is_some_and(|note| {
                note.returned_alive
                    && note.live_roots == 1
                    && note.swept == 0
                    && note.live_nodes == 1
            }),
        "shadow nil reclaimed the earlier table; id={old:?} before_roots={} after={after:?} collect={:?} release={:?}",
        before.live_roots,
        witness.collects,
        witness.release
    );
    assert_eq!(
        before.live_roots, 1,
        "the published slot stays until this frame retires"
    );
    assert!(
        witness.release.is_none(),
        "clearing the new scalar must not release the earlier root"
    );
    assert_eq!(after.collections, 1);
    assert_real_return_window(vm.program());
}

#[test]
fn explicit_clear_before_shadow_reclaims_the_earlier_table() {
    let (mut vm, log) = run_obs(CLEAR_BEFORE_SHADOW, true);
    let witness = witness(&mut vm);
    let old = binding_at(&log, 1.0);
    assert_eq!(witness.returned, Some(old));
    assert!(bool_at(&log, 2.0));
    let rooted = snap_at(&log, 11.0);
    assert!(alive_at(rooted, 1.0));
    assert_eq!(rooted.live_roots, 1);
    assert_eq!(
        text_at(&log, 3.0),
        "nil",
        "the original binding was cleared"
    );
    assert_eq!(text_at(&log, 4.0), "number");
    let after = snap_at(&log, 30.0);
    assert!(
        !alive_at(after, 1.0),
        "explicit clear must reclaim on collect"
    );
    assert_eq!(after.live_roots, 0);
    assert_eq!(after.collections, 1);
    let collect = &witness.collects[0];
    assert!(!collect.returned_alive);
    assert_eq!(collect.live_roots, 0);
    assert_eq!(collect.swept, 1);
    assert_eq!(collect.live_nodes, 0);
    assert!(
        witness.release.is_some(),
        "the original slot still releases"
    );
    assert_real_return_window(vm.program());
}

#[test]
fn table_shadow_nil_keeps_the_earlier_table() {
    let (mut vm, log) = run_obs(TABLE_SHADOW, true);
    let witness = witness(&mut vm);
    let old = binding_at(&log, 1.0);
    let fresh = binding_at(&log, 3.0);
    assert_ne!(old, fresh, "the shadow table is a different object");
    assert_eq!(witness.returned, Some(old));
    assert!(bool_at(&log, 2.0));
    assert_eq!(text_at(&log, 4.0), "table", "the new binding is a table");
    assert_eq!(
        text_at(&log, 5.0),
        "nil",
        "nil assignment wrote nil into the new table"
    );
    let before = snap_at(&log, 20.0);
    assert!(
        alive_at(before, 1.0),
        "the earlier table must still be allocated before collect: {before:?}"
    );
    let after = snap_at(&log, 30.0);
    let collect = witness.collects.first();
    assert!(
        alive_at(after, 1.0)
            && after.live_roots == 1
            && collect.is_some_and(|note| {
                note.returned_alive && note.live_roots == 1 && note.live_nodes == 1
            }),
        "table shadow nil reclaimed the earlier table; old={old:?} fresh={fresh:?} before_roots={} after={after:?} collect={:?} release={:?}",
        before.live_roots,
        witness.collects,
        witness.release
    );
    assert!(
        !alive_at(after, 3.0),
        "the shadowed constructor must not gain an extra root"
    );
    assert_eq!(
        before.live_roots, 1,
        "shadow does not add a root for the earlier binding"
    );
    assert!(witness.release.is_none());
    assert_eq!(after.collections, 1);
    assert_eq!(collect.expect("collect").swept, 1);
    assert_real_return_window(vm.program());
}

#[test]
fn table_result_shadow_keeps_the_earlier_table_and_drops_the_new_one() {
    let (mut vm, log) = run_obs(TABLE_RESULT_SHADOW, true);
    let witness = witness(&mut vm);
    let old = binding_at(&log, 1.0);
    let fresh = binding_at(&log, 3.0);
    assert_ne!(old, fresh);
    assert!(bool_at(&log, 2.0) && bool_at(&log, 6.0));
    assert_eq!(text_at(&log, 4.0), "table");
    assert_eq!(text_at(&log, 5.0), "nil");
    assert_eq!(witness.handoffs.len(), 2);
    assert!(witness.handoffs[0].published_root && witness.handoffs[1].published_root);
    let both = snap_at(&log, 15.0);
    assert!(alive_at(both, 1.0) && alive_at(both, 3.0));
    assert_eq!(both.live_roots, 2, "each call result keeps its own slot");
    let before = snap_at(&log, 20.0);
    assert!(
        alive_at(before, 1.0),
        "nil must not drop the earlier table before collect: {before:?}"
    );
    let after = snap_at(&log, 30.0);
    assert!(
        alive_at(after, 1.0),
        "nil on the new result must not reclaim the earlier table; after={after:?} collect={:?}",
        witness.collects
    );
    assert!(
        !alive_at(after, 3.0),
        "the new result root is the one released"
    );
    assert_eq!(after.live_roots, 1);
    assert_eq!(after.collections, 1);
    assert_eq!(binding_at(&log, 1.0), old);
    assert_eq!(binding_at(&log, 3.0), fresh);
    let collect = &witness.collects[0];
    assert!(
        !collect.returned_alive,
        "the witness field is the later call"
    );
    assert_eq!(collect.swept, 1);
    assert_eq!(collect.live_roots, 1);
    assert_eq!(collect.live_nodes, 1);
    assert_real_return_window(vm.program());
}

#[test]
fn unpublished_alias_return_is_collected_while_the_caller_binding_remains() {
    let (mut vm, log) = run_obs(UNPUBLISHED_ALIAS, false);
    let witness = witness(&mut vm);
    assert!(!witness.published_root);
    assert!(!witness.handoffs[0].published_root);
    assert!(witness.handoffs[0].lua_frame_alive_at_publish);
    assert!(witness.callee_left);
    let kept = binding_at(&log, 1.0);
    let after = snap_at(&log, 30.0);
    assert!(
        !alive_at(after, 1.0),
        "skipping publish must not leave another root"
    );
    assert!(!heap_alive(&mut vm, kept));
    assert_eq!(after.live_nodes, 0);
    assert_eq!(after.live_roots, 0);
    assert_eq!(after.collections, 1);
    assert_real_return_window(vm.program());
}

fn run_obs(source: &str, publish: bool) -> (vm::Vm, ObsLog) {
    let mut vm = run(source, publish);
    require_halted(&mut vm);
    let log = obs_log(&mut vm);
    (vm, log)
}

fn bool_at(log: &ObsLog, tag: f64) -> bool {
    log.bools
        .iter()
        .find(|(found, _)| *found == tag)
        .unwrap_or_else(|| panic!("missing bool {tag} in {log:?}"))
        .1
}

fn text_at(log: &ObsLog, tag: f64) -> String {
    log.texts
        .iter()
        .find(|(found, _)| *found == tag)
        .unwrap_or_else(|| panic!("missing text {tag} in {log:?}"))
        .1
        .clone()
}

fn binding_at(log: &ObsLog, tag: f64) -> heap::TableRef {
    log.bindings
        .iter()
        .find(|(found, _)| *found == tag)
        .unwrap_or_else(|| panic!("missing binding {tag} in {log:?}"))
        .1
}

fn snap_at(log: &ObsLog, tag: f64) -> &BindingSnap {
    log.snaps
        .iter()
        .find(|snap| snap.tag == tag)
        .unwrap_or_else(|| panic!("missing snap {tag} in {log:?}"))
}

fn alive_at(snap: &BindingSnap, tag: f64) -> bool {
    snap.alive
        .iter()
        .find(|(found, _)| *found == tag)
        .unwrap_or_else(|| panic!("missing alive {tag} in {snap:?}"))
        .1
}

fn heap_alive(vm: &mut vm::Vm, table: heap::TableRef) -> bool {
    heap::with_heap(&mut vm.host_context(), |heap| Ok(heap.alive(table))).expect("heap")
}

fn assert_value_oracle() {
    let Ok(oracle) = std::env::var("LUAJIT_ORACLE") else {
        return;
    };
    let dir = std::env::var("TMPDIR").expect("TMPDIR");
    let path = format!("{dir}/caller-value-oracle.lua");
    std::fs::write(&path, VALUE_ORACLE).expect("write oracle source");
    let output = std::process::Command::new(oracle)
        .arg(&path)
        .output()
        .expect("run LuaJIT");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("oracle stdout"),
        "true true true true 10 true\ntrue\ntrue\n"
    );
}

#[test]
fn numeric_table_keys_are_refused() {
    let error = candidate::lower_returning("local t = {}; local n = t[1]\n", catalog(), true)
        .expect_err("numeric keys stay outside this field-key subset");
    assert!(error.to_string().contains("numeric table keys"), "{error}");
}

fn assert_stale_edge(vm: &mut vm::Vm, table: heap::TableRef, key: Option<u32>) {
    let key = key.expect("field key");
    let edge = heap::with_heap(&mut vm.host_context(), |heap| heap.edge(table, key));
    assert!(
        matches!(edge, Err(heap::HeapError::StaleHandle)),
        "stale edge must stay structured, got {edge:?}"
    );
}

fn assert_retired_call(witness: &ReturnWitness) {
    let callee = witness.callee_depth.expect("callee depth");
    let handoff = witness.handoff_depth.expect("depth after return");
    assert_eq!(
        callee,
        handoff + 1,
        "the callee frame retired before publish"
    );
}

fn assert_real_return_window(program: &Program) {
    let finish = import_index(program, candidate::FINISH_RETURN);
    let mut call_ip = None;
    let mut prototype = None;
    let mut ip = 0;
    while ip < program.code.len() {
        let op = OpCode::try_from(program.code[ip]).expect("opcode");
        if op == OpCode::CallScript && call_ip.is_none() {
            call_ip = Some(ip);
            prototype = Some(u32::from_le_bytes(
                program.code[ip + 1..ip + 5].try_into().expect("prototype"),
            ));
            assert_eq!(program.code[ip + 5], 0, "no captured arguments");
        }
        ip += 1 + op.operand_len();
    }
    let call_ip = call_ip.expect("CallScript");
    let prototype_id = prototype.expect("prototype");
    let prototype = program
        .callable_prototypes
        .get(prototype_id as usize)
        .expect("callable prototype");
    let vm::CallableTarget::ScriptFunction(index) = prototype.target else {
        panic!("CallScript target {prototype_id} is not a script function");
    };
    let function = program
        .script_functions
        .get(index as usize)
        .expect("script function");
    assert!(function.end_ip > function.entry_ip);
    assert!(
        call_ip < function.entry_ip as usize || call_ip >= function.end_ip as usize,
        "the call site is outside the callee, so the frame was not inlined away"
    );
    assert!(
        region_has(program, function.entry_ip, function.end_ip, OpCode::Ret),
        "the callee still retires with Ret"
    );
    if let Some(collect) = program
        .imports
        .iter()
        .position(|import| import.name == candidate::COLLECT)
    {
        let collect = u16::try_from(collect).expect("import index");
        assert!(
            !region_calls(program, function.entry_ip, function.end_ip, collect),
            "the callee does not collect before returning"
        );
    }
    let finish_ip = next_call(program, call_ip, finish);
    assert_no_calls_between(program, call_ip, finish_ip);
    let retire = import_index(program, candidate::RETIRE_CALLEE);
    let retire_ip = next_call(program, finish_ip, retire);
    assert!(retire_ip > finish_ip, "retire follows publish");
    if let Some(collect) = program
        .imports
        .iter()
        .position(|import| import.name == candidate::COLLECT)
    {
        let collect = u16::try_from(collect).expect("import index");
        assert!(
            !region_calls(program, finish_ip as u32, retire_ip as u32, collect),
            "collect does not sit between publish and retire"
        );
    }
}

fn import_index(program: &Program, name: &str) -> u16 {
    program
        .imports
        .iter()
        .position(|import| import.name == name)
        .expect(name)
        .try_into()
        .expect("import index")
}

fn region_has(program: &Program, start: u32, end: u32, want: OpCode) -> bool {
    let mut ip = start as usize;
    let end = end as usize;
    while ip < end {
        let op = OpCode::try_from(program.code[ip]).expect("opcode");
        if op == want {
            return true;
        }
        ip += 1 + op.operand_len();
    }
    false
}

fn region_calls(program: &Program, start: u32, end: u32, import: u16) -> bool {
    let mut ip = start as usize;
    let end = end as usize;
    while ip < end {
        let op = OpCode::try_from(program.code[ip]).expect("opcode");
        if op == OpCode::Call && call_index(program, ip) == import {
            return true;
        }
        ip += 1 + op.operand_len();
    }
    false
}

fn next_call(program: &Program, call_script: usize, import: u16) -> usize {
    let op = OpCode::try_from(program.code[call_script]).expect("opcode");
    let mut ip = call_script + 1 + op.operand_len();
    while ip < program.code.len() {
        let op = OpCode::try_from(program.code[ip]).expect("opcode");
        if op == OpCode::Call && call_index(program, ip) == import {
            return ip;
        }
        ip += 1 + op.operand_len();
    }
    panic!("finish_return does not follow the generated call");
}

fn assert_no_calls_between(program: &Program, call_script: usize, finish: usize) {
    let op = OpCode::try_from(program.code[call_script]).expect("opcode");
    let mut ip = call_script + 1 + op.operand_len();
    while ip < finish {
        let op = OpCode::try_from(program.code[ip]).expect("opcode");
        assert!(
            !matches!(op, OpCode::Call | OpCode::CallValue | OpCode::CallScript),
            "collect or a guest callback sits between the call and publish at {ip}"
        );
        ip += 1 + op.operand_len();
    }
}

fn call_index(program: &Program, ip: usize) -> u16 {
    u16::from_le_bytes(program.code[ip + 1..ip + 3].try_into().expect("call index"))
}

/// Chunk completion must drop only this chunk's caller frame.
///
/// The outer table is rooted on the native heap before the chunk starts. It
/// is not a Lua local of this chunk. LuaJIT's 260-activation run is a separate
/// upstream observation and is not this compat result.
struct ChunkExit {
    vm: vm::Vm,
    outer_frame: heap::RootFrameId,
    outer_table: heap::TableRef,
    chunk_table: heap::TableRef,
    baseline: heap::HeapStats,
    baseline_resources: usize,
}

fn run_chunk_over_outer_root(source: &str) -> ChunkExit {
    let mut vm = run(source, true);
    let outer_frame = heap::with_heap(&mut vm.host_context(), |heap| heap.enter_roots(1))
        .expect("native heap accepts an outer frame before the chunk");
    let carrier = heap::publish_guest(&mut vm.host_context()).expect("outer table");
    let outer_table = heap::with_heap(&mut vm.host_context(), |heap| {
        let heap::GuestClass::Table(table) = heap.guest_class(&carrier) else {
            return Err(heap::HeapError::WrongKind);
        };
        heap.write_root(outer_frame, 0, table)?;
        Ok(table)
    })
    .expect("write the outer root");
    let baseline = heap::stats(&mut vm.host_context()).expect("entry baseline");
    let baseline_resources = vm.host_context().resource_count();
    assert_eq!(
        baseline.root_slots, 1,
        "setup: outer frame occupies one root slot"
    );
    assert_eq!(
        baseline.live_roots, 1,
        "setup: outer table is the only live root"
    );
    assert_eq!(
        baseline.live_nodes, 1,
        "setup: outer table occupies one node"
    );
    assert_eq!(
        baseline_resources, 1,
        "setup: outer table holds one resource"
    );
    assert_eq!(vm.run().expect("run chunk"), VmStatus::Halted);
    let chunk_table = witness(&mut vm)
        .returned
        .expect("the chunk received a returned table");
    assert_ne!(
        chunk_table, outer_table,
        "setup: the chunk table is not the outer table"
    );
    ChunkExit {
        vm,
        outer_frame,
        outer_table,
        chunk_table,
        baseline,
        baseline_resources,
    }
}

fn assert_only_chunk_frame_retired(exit: &mut ChunkExit) {
    let before = heap::stats(&mut exit.vm.host_context()).expect("stats before extra collect");
    assert_eq!(
        before.root_slots, exit.baseline.root_slots,
        "chunk frame must return root storage to the entry baseline"
    );
    assert_eq!(
        before.live_roots, exit.baseline.live_roots,
        "the chunk's live root must be gone before the extra collect"
    );
    assert!(
        heap_alive(&mut exit.vm, exit.chunk_table),
        "the unrooted self-cycle still occupies a node before the extra collect"
    );
    assert_eq!(
        before.live_nodes,
        exit.baseline.live_nodes + 1,
        "the self-cycle node stays until an explicit collect"
    );
    assert!(
        heap_alive(&mut exit.vm, exit.outer_table),
        "the outer root must survive chunk completion"
    );
    let current = heap::with_heap(&mut exit.vm.host_context(), |heap| Ok(heap.current_frame()))
        .expect("current frame");
    assert_eq!(
        current,
        Some(exit.outer_frame),
        "completion leaves only this chunk frame"
    );
    assert_eq!(
        exit.vm.host_context().resource_count(),
        exit.baseline_resources + 1,
        "the self-cycle token stays until collect"
    );
    // Same VM, no reset and no host clear of the remaining root.
    let report = heap::collect(&mut exit.vm.host_context()).expect("same-scope collect");
    assert!(
        !heap_alive(&mut exit.vm, exit.chunk_table),
        "collect reclaims the self-cycle after the frame is gone"
    );
    assert!(
        heap_alive(&mut exit.vm, exit.outer_table),
        "collect must not reclaim the outer root"
    );
    assert_eq!(report.live_nodes, exit.baseline.live_nodes);
    assert_eq!(report.live_roots, exit.baseline.live_roots);
    assert_eq!(
        exit.vm.host_context().resource_count(),
        exit.baseline_resources,
        "collect returns the self-cycle resource"
    );
    let after = heap::stats(&mut exit.vm.host_context()).expect("stats after collect");
    assert_eq!(after.root_slots, exit.baseline.root_slots);
    let current = heap::with_heap(&mut exit.vm.host_context(), |heap| Ok(heap.current_frame()))
        .expect("frame after collect");
    assert_eq!(current, Some(exit.outer_frame));
}

#[test]
fn fallthrough_returns_root_slots_and_leaves_the_self_cycle_until_collect() {
    let source = r#"
local function make()
  local obj = {}
  obj.self = obj
  return obj
end
local kept = make()
"#;
    let mut exit = run_chunk_over_outer_root(source);
    assert_eq!(
        exit.vm.stack(),
        &[] as &[Value],
        "fallthrough adds no result"
    );
    assert_only_chunk_frame_retired(&mut exit);
}

#[test]
fn nil_return_keeps_the_scalar_stack_and_returns_root_slots() {
    let source = r#"
local function make()
  local obj = {}
  obj.self = obj
  return obj
end
local function one()
  return nil
end
local kept = make()
local n = one()
return n
"#;
    let mut exit = run_chunk_over_outer_root(source);
    assert_eq!(exit.vm.stack(), &[Value::Null]);
    assert_only_chunk_frame_retired(&mut exit);
}

#[test]
fn type_return_reads_the_rooted_table_before_the_frame_leaves() {
    let source = r#"
local function make()
  local obj = {}
  obj.self = obj
  return obj
end
local kept = make()
return type(kept)
"#;
    let mut exit = run_chunk_over_outer_root(source);
    assert_eq!(exit.vm.stack(), &[Value::string("table")]);
    assert_only_chunk_frame_retired(&mut exit);
}

#[test]
fn collect_return_sees_the_frame_root_then_the_frame_leaves() {
    let source = r#"
local function make()
  local obj = {}
  obj.self = obj
  return obj
end
local kept = make()
return collectgarbage("collect")
"#;
    let mut exit = run_chunk_over_outer_root(source);
    assert_eq!(exit.vm.stack(), &[Value::Float(0.0)]);
    let witness = witness(&mut exit.vm);
    let during = witness
        .collects
        .first()
        .expect("the return expression collected while the chunk frame was still current");
    assert!(
        during.returned_alive,
        "collect inside the return must still see the chunk root: {during:?}"
    );
    assert_eq!(
        during.live_roots,
        exit.baseline.live_roots + 1,
        "the last collection runs before the chunk frame leaves"
    );
    assert_eq!(during.live_nodes, exit.baseline.live_nodes + 1);
    assert_only_chunk_frame_retired(&mut exit);
}
