//! Real uncaptured Lua calls and the explicit return-root handoff.
//!
//! The sources are legal Lua. Observation stays in the candidate witness:
//! Core's nested `Ret` still carries one value, and that value is the private
//! ordered result array. `self` is an ordinary field name.

use super::candidate::{self, ReturnWitness, ReturnedElement};
use super::catalog;
use std::sync::Arc;
use vm::{
    CompileSourceFileOptions, FrontendImportSyntax, FrontendIr, HostFunctionRegistry, OpCode,
    ParseError, Program, SourceFlavor, SourcePlugin, Value, VmStatus,
};

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
    registry
        .bind_vm_cached(&mut vm)
        .expect("bind candidate runtime");
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

#[test]
fn zero_return_uses_a_real_frame_and_exact_arity() {
    let mut vm = run(ZERO, true);
    assert_eq!(vm.run().unwrap(), VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::Float(0.0)]);
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
    assert_eq!(vm.stack(), &[Value::Float(1.0)]);
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
    assert_eq!(witness.returned_edge_at_handoff, Some(returned));
    assert_eq!(witness.discarded_edge_at_handoff, Some(discarded));
    assert!(witness.published_root);
    assert!(witness.callee_left);
    assert!(witness.top_is_caller_after_handoff);
    assert_retired_call(&witness);
    let first = &witness.collects[0];
    assert!(first.returned_alive);
    assert_eq!(first.returned_edge, Some(returned));
    assert!(!first.discarded_alive);
    assert_eq!(first.marked_nodes, 1);
    assert_eq!(first.live_roots, 1);
    assert_eq!(first.live_nodes, 1);
    assert_eq!(first.swept, 1);
    let release = witness.release.expect("caller dropped the result root");
    assert!(release.returned_alive);
    assert_eq!(release.returned_edge, Some(returned));
    assert_eq!(release.live_roots, 0);
    let second = &witness.collects[1];
    assert!(!second.returned_alive);
    assert!(second.returned_edge.is_none());
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
    assert!(first.returned_edge.is_none());
    assert!(!first.discarded_alive);
    assert_eq!(first.marked_nodes, 0);
    assert_eq!(first.live_roots, 0);
    assert_eq!(first.swept, 2);
    assert_real_return_window(vm.program());
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
    let function = &program.script_functions[prototype.expect("prototype") as usize];
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
