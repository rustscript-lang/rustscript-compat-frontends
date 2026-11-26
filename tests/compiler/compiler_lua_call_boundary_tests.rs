//! Core-level call-boundary witness for the Lua protected-call preflight.
//!
//! These tests use only the locked Core public API. Each arm runs one VM whose
//! root frame calls a real guest `parent`; `parent` holds a real guest script
//! callable in its own frame and reaches the child frame through the one
//! `CallValue` instruction decoded from the program's real instruction
//! boundaries. No second VM, no host-side re-entry, and no Lua frontend is
//! involved.
//!
//! The guest inputs are the exact RSS fixtures copied from the preflight
//! `next-run/` directory. They are linked with `include_str!` so the witness
//! stays anchored to the exact original bytes.
//!
//! The first real compiler runs on those untouched bytes failed in this order:
//!
//! 1. Untouched fixture: `Parse(ParseError { message: "expected parameter
//!    name", ... })` at the closing `)` of `parent` (the reported line and
//!    span differ per arm; the omitted arm reports line 18, span 457..458).
//!    Locked Core accepts no trailing comma after the final parameter.
//! 2. Only that comma removed: `Parse(ParseError { message: "function header
//!    changed while parsing 'parent'" })`. The predeclaration arity scan
//!    counts the comma inside the callable result type `[bool, [int]]` as a
//!    parameter separator.
//! 3. Callable result widened to `array` and the trailing comma removed:
//!    `Parse(ParseError { message: "unknown function 'len'" })`. `len` is an
//!    ordinary builtin without a source-level name in this compile mode; the
//!    language postfix form is `.length`.
//! 4. `len(r)` changed to `r.length`: `Parse(ParseError { message: "local 'r'
//!    is partially moved; access remaining fields/elements directly or
//!    reinitialize moved children before using 'r' as a whole" })`. Reading
//!    `r[0]` and then returning the whole `r` is rejected by strict ownership.
//!
//! The rewrite below is therefore syntax-only and keeps every runtime value
//! and control-flow decision identical:
//!
//! - delete the one trailing parameter comma;
//! - annotate `parent`'s callable parameter and the root callable binding as
//!   `fn([int]) -> array` instead of `fn([int]) -> [bool, [int]]` (the callee
//!   prototype still carries the exact `[bool, [int]]` result; the runtime
//!   callable schema check only requires a callable value);
//! - use the language postfix `r.length` for the length builtin;
//! - read `tag = r[0]` and `payload = r[1]` and return those elements instead
//!   of the partially moved transport array. The guest therefore still reads
//!   the completion tag and the exact transport length itself, and the payload
//!   keeps its original Core array identity.
//!
//! Each replacement below asserts that its exact before-pattern occurs once,
//! so the witness cannot silently drift from the original fixtures.
//!
//! At `mark(1)` the host also binds the observed call to the actual `held`
//! argument, not merely to some callable in the parent frame:
//!
//! - the caller is the frame immediately below the active callee;
//! - the caller prototype's `parameter_slots[0]` selects one exact local slot;
//! - that slot is read with checked arithmetic inside the caller's own
//!   `local_base..local_base + local_count` frame range;
//! - the value must be a `Value::Callable` whose prototype is the prototype of
//!   the frame that is actually executing, and whose prototype target is a
//!   script function.
//!
//! No other slot is searched, so an unrelated callable cannot satisfy the
//! witness.

use std::sync::{Arc, Mutex};

use vm::vm::{VmExecutionFrameSnapshot, VmFrameContinuation};
use vm::{CallOutcome, CallReturn, CallableTarget, HostFunction, Value, Vm, VmError, VmStatus};

const GUEST_SUCCESS: &str = include_str!("fixtures/core_active_protected_call/guest-success.rss");
const GUEST_ERROR: &str = include_str!("fixtures/core_active_protected_call/guest-error.rss");
const GUEST_OMITTED: &str = include_str!("fixtures/core_active_protected_call/guest-omitted.rss");
const GUEST_HOST_FAULT: &str =
    include_str!("fixtures/core_active_protected_call/guest-host-fault.rss");

#[derive(Clone, Debug, PartialEq, Eq)]
struct Mark {
    phase: i64,
    frame_len: usize,
    call_depth: usize,
    last_prototype: Option<u32>,
    last_return_ip: Option<usize>,
    parent_prototype: Option<u32>,
    held_parameter: HeldParameterWitness,
}

/// Exact observation of the active parent frame's first declared parameter.
///
/// Only `parameter_slots[0]` of the caller prototype is read. The absolute
/// index is formed with checked arithmetic and then read with `get`, so both
/// the relative slot check and the concrete local-range bounds check are
/// recorded instead of assumed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct HeldParameterWitness {
    caller_prototype: Option<u32>,
    declared_slot: Option<u16>,
    caller_local_base: Option<usize>,
    caller_local_count: Option<usize>,
    slot_inside_caller_frame: bool,
    read_within_bounds: bool,
    value_is_callable: bool,
    callable_prototype: Option<u32>,
    callable_is_script_function: bool,
    callee_prototype: Option<u32>,
}

struct MarkHost {
    log: Arc<Mutex<Vec<Mark>>>,
}

impl HostFunction for MarkHost {
    fn call(&mut self, vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
        let [Value::Int(phase)] = args else {
            return Err(VmError::HostError("mark expects one int".to_string()));
        };
        let frames = vm.execution_frames();
        let parent = frames.iter().rev().nth(1);
        // The strict parent-parameter binding belongs to the callee marker: at
        // `mark(1)` the parent is one frame below the active callee.
        let held_parameter = if *phase == 1 {
            observe_held_parameter(vm, &frames)
        } else {
            HeldParameterWitness::default()
        };
        let mark = Mark {
            phase: *phase,
            frame_len: frames.len(),
            call_depth: vm.call_depth(),
            last_prototype: frames.last().and_then(|frame| frame.prototype_id),
            last_return_ip: frames.last().and_then(|frame| match &frame.continuation {
                VmFrameContinuation::ResumeBytecode { return_ip } => Some(*return_ip),
                _ => None,
            }),
            parent_prototype: parent.and_then(|frame| frame.prototype_id),
            held_parameter,
        };
        self.log.lock().expect("mark log").push(mark);
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(*phase))))
    }
}

/// Reads the caller's declared first parameter and records the exact callee
/// binding for the caller's strict assertions.
///
/// The caller is the frame immediately below the active frame. The slot comes
/// from the caller prototype itself (`parameter_slots[0]`), is required to be
/// inside the caller's declared local count, and is then read with
/// `checked_add` plus a bounds-checked slice lookup. The value must be a
/// `Value::Callable` whose prototype is the executing callee frame's
/// prototype, and whose prototype target must be a script function. No other
/// slot is scanned.
fn observe_held_parameter(vm: &Vm, frames: &[VmExecutionFrameSnapshot]) -> HeldParameterWitness {
    let (Some(callee), Some(caller)) = (frames.last(), frames.iter().rev().nth(1)) else {
        return HeldParameterWitness::default();
    };
    let mut witness = HeldParameterWitness {
        caller_prototype: caller.prototype_id,
        caller_local_base: Some(caller.local_base),
        caller_local_count: Some(caller.local_count),
        callee_prototype: callee.prototype_id,
        ..HeldParameterWitness::default()
    };
    let program = vm.program();
    let Some(prototype) = caller
        .prototype_id
        .and_then(|id| program.callable_prototypes.get(id as usize))
    else {
        return witness;
    };
    let Some(&declared_slot) = prototype.parameter_slots.first() else {
        return witness;
    };
    witness.declared_slot = Some(declared_slot);
    let relative = usize::from(declared_slot);
    if relative >= caller.local_count {
        return witness;
    }
    witness.slot_inside_caller_frame = true;
    let Some(absolute) = caller.local_base.checked_add(relative) else {
        return witness;
    };
    let Some(value) = vm.locals().get(absolute) else {
        return witness;
    };
    witness.read_within_bounds = true;
    let Value::Callable(callable) = value else {
        return witness;
    };
    witness.value_is_callable = true;
    witness.callable_prototype = Some(callable.prototype_id);
    witness.callable_is_script_function = matches!(
        program
            .callable_prototypes
            .get(callable.prototype_id as usize)
            .map(|prototype| prototype.target),
        Some(CallableTarget::ScriptFunction(_))
    );
    witness
}

/// Rewrites the exact fixture into the minimal syntax accepted by this Core pin.
///
/// The original bytes stay in the fixture; every edit is asserted to target
/// exactly one site, and no template is kept alongside the fixture.
fn rewrite_fixture(raw: &str) -> String {
    let mut rewritten = raw.to_string();
    for (before, after) in [
        // Locked Core rejects a trailing comma after the final parameter.
        ("    do_call: bool,\n", "    do_call: bool\n"),
        // The predeclaration arity scan mistakes the comma in the callable
        // result tuple for a parameter separator.
        (
            "    held: fn([int]) -> [bool, [int]],\n",
            "    held: fn([int]) -> array,\n",
        ),
        (
            "let held: fn([int]) -> [bool, [int]] = ",
            "let held: fn([int]) -> array = ",
        ),
        // `len` is an ordinary builtin without a source-level name here.
        ("len(r)", "r.length"),
        // Reading r[0] and returning the whole r violates strict ownership.
        (
            ") -> [[bool, [int]], int, int, bool, [int]] {",
            ") -> [bool, [int], int, int, [int]] {",
        ),
        (
            "    let tag = r[0];\n",
            "    let tag = r[0];\n    let payload = r[1];\n",
        ),
        (
            "    [r, arity, marker, tag, t]\n",
            "    [tag, payload, arity, marker, t]\n",
        ),
    ] {
        assert_eq!(
            rewritten.matches(before).count(),
            1,
            "rewrite target must occur exactly once: {before:?}"
        );
        rewritten = rewritten.replace(before, after);
    }
    rewritten
}

/// Decodes the whole instruction stream with `OpCode::operand_len()` and
/// requires exactly one `CallValue` site, owned by a script prototype.
fn call_site(program: &vm::Program) -> (u32, usize) {
    let code = &program.code;
    let mut ip = 0usize;
    let mut sites = Vec::new();
    while ip < code.len() {
        let op = vm::OpCode::try_from(code[ip]).expect("invalid opcode");
        let next = ip + 1 + op.operand_len();
        assert!(next <= code.len(), "truncated instruction at {ip}");
        if op == vm::OpCode::CallValue {
            assert_eq!(code[ip + 1], 1, "held(t) must pass exactly one argument");
            let owners: Vec<_> = program
                .function_regions
                .iter()
                .filter(|region| region.start_ip as usize <= ip && next <= region.end_ip as usize)
                .collect();
            let [owner] = owners.as_slice() else {
                panic!("expected a unique caller region for CallValue at {ip}");
            };
            sites.push((
                owner
                    .prototype_id
                    .expect("held(t) must be inside the parent prototype"),
                next,
            ));
        }
        ip = next;
    }
    let [site] = sites.as_slice() else {
        panic!("expected one decoded CallValue site; got {sites:?}");
    };
    *site
}

fn assert_executed_site(vm: &Vm, log: &Mutex<Vec<Mark>>, invoked: bool) {
    let (caller, return_ip) = call_site(vm.program());
    let records = log.lock().expect("mark log");
    assert!(
        records
            .iter()
            .filter(|mark| mark.phase != 1)
            .all(|mark| mark.last_prototype == Some(caller)),
        "outer markers must run in the parent frame: {records:?}"
    );
    let callees: Vec<_> = records.iter().filter(|mark| mark.phase == 1).collect();
    if invoked {
        let [callee] = callees.as_slice() else {
            panic!("expected one callee marker; got {callees:?}");
        };
        assert_eq!(
            callee.last_return_ip,
            Some(return_ip),
            "the executed callee must resume at the decoded CallValue successor"
        );
        assert_ne!(
            callee.last_prototype,
            Some(caller),
            "the callee frame must not be the parent frame"
        );
        assert_eq!(
            callee.parent_prototype,
            Some(caller),
            "the active parent frame must be one below the callee"
        );
        let held = &callee.held_parameter;
        assert_eq!(
            held.caller_prototype,
            Some(caller),
            "the held argument must be read from the parent frame: {held:?}"
        );
        assert_eq!(
            held.callee_prototype, callee.last_prototype,
            "the witness must compare against the executing callee frame: {held:?}"
        );
        assert_eq!(
            held.declared_slot,
            Some(0),
            "the Core pin must declare the first parameter in slot 0: {held:?}"
        );
        assert!(
            held.slot_inside_caller_frame,
            "the declared parameter slot must be inside the parent frame: {held:?}"
        );
        assert!(
            held.read_within_bounds,
            "the bounds-checked parent local read must succeed: {held:?}"
        );
        assert!(
            held.value_is_callable,
            "the first declared parameter must hold a real callable: {held:?}"
        );
        assert_eq!(
            held.callable_prototype, callee.last_prototype,
            "the held callable must be the executing callee, not any other callable: {held:?}"
        );
        assert!(
            held.callable_is_script_function,
            "the held callable prototype target must be a script function: {held:?}"
        );
        assert_eq!(callee.frame_len, 3, "root + parent + callee");
    } else {
        assert!(callees.is_empty(), "the omitted arm must not call");
    }
}

fn build_guest(raw: &str) -> (Vm, Arc<Mutex<Vec<Mark>>>) {
    let source = rewrite_fixture(raw);
    let compiled = vm::compile_source(&source).expect("preflight source must compile");
    let imports: Vec<_> = compiled
        .program
        .imports
        .iter()
        .filter(|import| import.name == "mark")
        .collect();
    let [mark] = imports.as_slice() else {
        panic!("expected one mark import; got {imports:?}");
    };
    assert_eq!(mark.arity, 1);
    let _ = call_site(&compiled.program);
    let mut vm = Vm::new(compiled.program);
    vm.set_jit_config(vm::JitConfig {
        enabled: false,
        hot_loop_threshold: 0,
        max_trace_len: 0,
    });
    let log = Arc::new(Mutex::new(Vec::new()));
    vm.bind_function(
        "mark",
        Box::new(MarkHost {
            log: Arc::clone(&log),
        }),
    );
    (vm, log)
}

fn unpack(vm: &Vm) -> (bool, i64, i64, Value, Value) {
    let [top] = vm.stack() else {
        panic!("expected exactly one root result");
    };
    let Value::Array(result) = top else {
        panic!("root result must be the outer array");
    };
    assert_eq!(result.len(), 5, "outer result arity");
    let Value::Bool(tag) = &result[0] else {
        panic!("element 0 must be the guest-read completion tag");
    };
    let Value::Array(_) = &result[1] else {
        panic!("element 1 must be the guest-read payload");
    };
    let Value::Int(arity) = &result[2] else {
        panic!("element 2 must be the guest-read transport arity");
    };
    let Value::Int(marker) = &result[3] else {
        panic!("element 3 must be int");
    };
    // `same_array` answers false for a non-array, so the comparator `t` must
    // first be proven to be an array; otherwise the omitted arm's payload
    // inequality would pass without comparing two arrays.
    let Value::Array(_) = &result[4] else {
        panic!("element 4 must be the comparator array t");
    };
    (*tag, *arity, *marker, result[1].clone(), result[4].clone())
}

fn same_array(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Array(left), Value::Array(right)) => Arc::ptr_eq(left, right),
        _ => false,
    }
}

fn phases(log: &Arc<Mutex<Vec<Mark>>>) -> Vec<i64> {
    log.lock()
        .expect("mark log")
        .iter()
        .map(|mark| mark.phase)
        .collect()
}

fn frame_lens(log: &Arc<Mutex<Vec<Mark>>>) -> Vec<usize> {
    log.lock()
        .expect("mark log")
        .iter()
        .map(|mark| mark.frame_len)
        .collect()
}

#[test]
fn core_active_protected_call_preflight_success_data_transport() {
    let (mut vm, log) = build_guest(GUEST_SUCCESS);
    assert_eq!(vm.run().expect("success arm must halt"), VmStatus::Halted);
    let (tag, arity, marker, payload, original) = unpack(&vm);
    assert!(tag, "success tag");
    assert_eq!(arity, 2, "exactly two transport values, read by the guest");
    assert_eq!(marker, 1, "parent continued after the call");
    assert!(
        same_array(&payload, &original),
        "payload must be the same Core array object as the parent's t"
    );
    assert_eq!(phases(&log), vec![0, 1, 2]);
    assert_eq!(frame_lens(&log), vec![2, 3, 2]);
    assert_executed_site(&vm, &log, true);
    let records = log.lock().expect("mark log");
    assert_eq!(records[0].last_prototype, records[2].last_prototype);
    assert_ne!(records[1].last_prototype, records[0].last_prototype);
    assert_eq!(records[1].frame_len, records[0].frame_len + 1);
}

#[test]
fn core_active_protected_call_preflight_explicit_error_is_data() {
    let (mut vm, log) = build_guest(GUEST_ERROR);
    assert_eq!(
        vm.run()
            .expect("explicit error payload must not be a VmError"),
        VmStatus::Halted
    );
    let (tag, arity, marker, payload, original) = unpack(&vm);
    assert!(!tag, "explicit error tag");
    assert_eq!(arity, 2, "error transport keeps exact arity");
    assert_eq!(marker, 1, "parent continued after the error payload");
    assert!(
        same_array(&payload, &original),
        "error payload keeps the original object"
    );
    assert_eq!(phases(&log), vec![0, 1, 2]);
    assert_eq!(frame_lens(&log), vec![2, 3, 2]);
    assert_executed_site(&vm, &log, true);
}

#[test]
fn core_active_protected_call_preflight_omitted_call_has_no_callee_marker() {
    let (mut vm, log) = build_guest(GUEST_OMITTED);
    assert_eq!(vm.run().expect("omitted arm must halt"), VmStatus::Halted);
    let (tag, arity, marker, payload, original) = unpack(&vm);
    assert!(!tag);
    assert_eq!(arity, 2);
    assert_eq!(marker, 0, "the callable must not run");
    assert!(
        !same_array(&payload, &original),
        "the omitted call cannot deliver the original object"
    );
    assert_eq!(phases(&log), vec![0, 2]);
    assert_eq!(frame_lens(&log), vec![2, 2]);
    assert_executed_site(&vm, &log, false);
}

#[test]
fn core_active_protected_call_preflight_host_error_is_not_a_payload() {
    let (mut vm, log) = build_guest(GUEST_HOST_FAULT);
    let error = vm.run().expect_err("a real host error must abort the run");
    assert!(
        matches!(error, VmError::HostError(ref message) if message == "assertion failed"),
        "unexpected error: {error:?}"
    );
    assert_eq!(phases(&log), vec![0, 1], "mark(2) must not run");
    assert_eq!(
        frame_lens(&log),
        vec![2, 3],
        "the callee frame stays active"
    );
    assert!(
        vm.execution_frames().len() >= 3,
        "the callee frame is not unwound by an error: {:?}",
        vm.execution_frames()
    );
    assert_executed_site(&vm, &log, true);
    assert!(
        !vm.is_reusable(),
        "an errored active-frame VM is not reusable"
    );
}
