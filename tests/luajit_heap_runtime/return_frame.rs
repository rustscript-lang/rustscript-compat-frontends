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
