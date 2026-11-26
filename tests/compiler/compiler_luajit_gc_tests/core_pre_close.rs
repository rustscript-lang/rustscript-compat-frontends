//! Core pre-close callback witness after a normal root halt.
//!
//! Every input is a real RSS fixture compiled by the locked Core; no Lua
//! frontend is involved and JIT is disabled. The first test exercises the
//! primary positive control on one VM: the root halts with no marker, three
//! really exported script callables A/B/C run on that same VM, each marker
//! reads only `parameter_slots[0]` of the actual `ReturnToHost` frame and
//! compares the exact original argument array, B returns the explicit
//! `[false, payload]` data array, and an explicitly shutdown scope reaches a
//! real `Quiescent` with a synchronously closed resource destructed once.
//!
//! The second test is the Pending control on another VM: shutdown issues one
//! close request for a Gate that stays Pending, the scope stays `Closing` with
//! no terminal, the queued callback is discarded, and only an explicit public
//! `poll_close` after the gate is released reaches `Ready(Ok(Success))`.
//!
//! The third test is the real `HostError` control: A, the fault script, and C
//! are queued; the drain returns the original host error with C still queued
//! and no C marker, and shutdown discards C without any new marker. The test
//! never drains again after the fault and never claims the errored VM is
//! reusable.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use vm::execution_scope::{ScopeCloseOutcome, ScopeState};
use vm::resource::{CloseProgress, HostResource, ResourceResult};
use vm::vm::VmFrameContinuation;
use vm::{
    CallOutcome, CallReturn, HostFunction, JitConfig, ResourceCloseReason, Value, Vm, VmError,
    VmStatus,
};

const EXPORTS: &str = include_str!("../fixtures/core_pre_close/exports.rss");
const QUEUE_FAULT: &str = include_str!("../fixtures/core_pre_close/queue-fault.rss");
const FAULT_MESSAGE: &str = "pre-close host fault";

#[derive(Clone, Debug, PartialEq)]
struct Mark {
    phase: i64,
    frame_len: usize,
    call_depth: usize,
    prototype_id: Option<u32>,
    continuation: Option<VmFrameContinuation>,
    declared_slot: Option<u16>,
    local_base: Option<usize>,
    local_count: Option<usize>,
    slot_inside_frame: bool,
    read_within_bounds: bool,
    argument_is_array: bool,
    argument: Option<Value>,
}

struct MarkHost {
    log: Arc<Mutex<Vec<Mark>>>,
}

impl HostFunction for MarkHost {
    fn call(&mut self, vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
        let [Value::Int(phase)] = args else {
            return Err(VmError::HostError("mark expects one int".to_string()));
        };
        let mark = observe_mark(vm, *phase);
        self.log.lock().expect("mark log").push(mark);
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(*phase))))
    }
}

struct FaultHost {
    message: String,
}

impl HostFunction for FaultHost {
    fn call(&mut self, _vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
        if args.len() != 1 {
            return Err(VmError::HostError("explode expects one int".to_string()));
        }
        Err(VmError::HostError(self.message.clone()))
    }
}

/// Records the current script frame and reads exactly its first declared
/// parameter slot. No other slot is looked at, so an unrelated value cannot
/// satisfy the witness.
fn observe_mark(vm: &Vm, phase: i64) -> Mark {
    let frames = vm.execution_frames();
    let current = frames.last();
    let mut mark = Mark {
        phase,
        frame_len: frames.len(),
        call_depth: vm.call_depth(),
        prototype_id: current.and_then(|frame| frame.prototype_id),
        continuation: current.map(|frame| frame.continuation.clone()),
        declared_slot: None,
        local_base: None,
        local_count: None,
        slot_inside_frame: false,
        read_within_bounds: false,
        argument_is_array: false,
        argument: None,
    };
    let Some(frame) = current else {
        return mark;
    };
    mark.local_base = Some(frame.local_base);
    mark.local_count = Some(frame.local_count);
    let Some(prototype_id) = frame.prototype_id else {
        return mark;
    };
    let Some(prototype) = vm.program().callable_prototypes.get(prototype_id as usize) else {
        return mark;
    };
    let Some(&declared_slot) = prototype.parameter_slots.first() else {
        return mark;
    };
    mark.declared_slot = Some(declared_slot);
    let relative = usize::from(declared_slot);
    if relative >= frame.local_count {
        return mark;
    }
    mark.slot_inside_frame = true;
    let Some(absolute) = frame.local_base.checked_add(relative) else {
        return mark;
    };
    let Some(value) = vm.locals().get(absolute) else {
        return mark;
    };
    mark.read_within_bounds = true;
    mark.argument_is_array = matches!(value, Value::Array(_));
    mark.argument = Some(value.clone());
    mark
}

fn build_vm(source: &str) -> (Vm, Arc<Mutex<Vec<Mark>>>) {
    let compiled = vm::compile_source(source).expect("pre-close RSS must compile");
    let mut vm = Vm::new(compiled.program);
    vm.set_jit_config(JitConfig {
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
    vm.bind_function(
        "explode",
        Box::new(FaultHost {
            message: FAULT_MESSAGE.to_string(),
        }),
    );
    (vm, log)
}

fn exported_names(vm: &Vm) -> Vec<String> {
    vm.program()
        .exported_callables
        .iter()
        .map(|item| item.name.clone())
        .collect()
}

fn export(vm: &Vm, name: &str) -> Value {
    let value = vm
        .resolve_exported_callable(name)
        .unwrap_or_else(|error| panic!("exported callable `{name}` is missing: {error}"));
    assert!(
        matches!(value, Value::Callable(_)),
        "exported `{name}` is not a script callable"
    );
    value
}

fn callable_prototype(value: &Value) -> u32 {
    let Value::Callable(callable) = value else {
        panic!("expected a script callable, got {value:?}");
    };
    callable.prototype_id
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

fn assert_zero_frames_active(vm: &mut Vm) {
    assert!(
        vm.execution_frames().is_empty(),
        "each callback must retire its real frame"
    );
    assert_eq!(vm.call_depth(), 0, "callback depth must return to zero");
    assert_eq!(
        vm.execution_scope().state(),
        ScopeState::Active,
        "the execution scope must stay Active before shutdown"
    );
    assert!(
        vm.execution_scope().terminal().is_none(),
        "no terminal outcome may exist before shutdown"
    );
    assert_eq!(vm.queued_callable_count(), 0);
}

fn assert_marker_record(
    log: &Arc<Mutex<Vec<Mark>>>,
    phase: i64,
    callable: &Value,
    expected_argument: &Value,
) {
    let records = log.lock().expect("mark log");
    let mark = records.last().expect("the marker must have run");
    assert_eq!(mark.phase, phase);
    assert_eq!(
        mark.continuation,
        Some(VmFrameContinuation::ReturnToHost),
        "the marker must inspect the real ReturnToHost frame: {mark:?}"
    );
    assert_eq!(
        mark.frame_len, 1,
        "the invoked export must be the only real frame: {mark:?}"
    );
    assert_eq!(
        mark.prototype_id,
        Some(callable_prototype(callable)),
        "the marker must run in the exact exported prototype: {mark:?}"
    );
    assert!(
        mark.declared_slot.is_some(),
        "the prototype must declare parameter_slots[0]: {mark:?}"
    );
    assert!(
        mark.slot_inside_frame,
        "parameter_slots[0] must be inside the real frame: {mark:?}"
    );
    assert!(
        mark.read_within_bounds,
        "the checked local read must succeed: {mark:?}"
    );
    assert!(
        mark.argument_is_array,
        "the first parameter must hold the original array: {mark:?}"
    );
    let recorded = mark
        .argument
        .as_ref()
        .expect("the marker must record the argument");
    assert!(
        same_array(recorded, expected_argument),
        "the marker must read the exact original array Arc: {mark:?}"
    );
}

fn halt_normal_root(vm: &mut Vm, log: &Arc<Mutex<Vec<Mark>>>) {
    assert_eq!(
        vm.run()
            .expect("the real RSS root chunk must compile and run"),
        VmStatus::Halted
    );
    assert_eq!(vm.stack(), &[Value::Int(0)], "root statement result");
    assert!(
        log.lock().expect("mark log").is_empty(),
        "the root chunk must not call the marker"
    );
    assert!(
        vm.execution_frames().is_empty(),
        "a normal halt must leave no real frame"
    );
    assert_eq!(vm.call_depth(), 0);
    assert_eq!(
        vm.execution_scope().state(),
        ScopeState::Active,
        "a normal halt must leave the scope Active"
    );
    assert!(
        vm.execution_scope().terminal().is_none(),
        "a normal halt must not fix a terminal outcome"
    );
}

struct SyncCloseResource {
    begins: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
    reason: Arc<Mutex<Option<ResourceCloseReason>>>,
}

impl HostResource for SyncCloseResource {
    fn begin_close(&mut self, reason: ResourceCloseReason) -> ResourceResult<CloseProgress> {
        self.begins.fetch_add(1, Ordering::SeqCst);
        *self.reason.lock().expect("close reason") = Some(reason);
        Ok(CloseProgress::Ready)
    }
}

impl Drop for SyncCloseResource {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

struct PendingGate {
    begins: Arc<AtomicUsize>,
    polls: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
    reasons: Arc<Mutex<Vec<ResourceCloseReason>>>,
    ready: Arc<AtomicBool>,
}

impl HostResource for PendingGate {
    fn begin_close(&mut self, reason: ResourceCloseReason) -> ResourceResult<CloseProgress> {
        self.begins.fetch_add(1, Ordering::SeqCst);
        self.reasons
            .lock()
            .expect("gate close reasons")
            .push(reason);
        Ok(CloseProgress::Pending)
    }

    fn poll_close(&mut self, _cx: &mut Context<'_>) -> Poll<ResourceResult<()>> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        if self.ready.load(Ordering::SeqCst) {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }
}

impl Drop for PendingGate {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn core_pre_close_exported_callables_after_normal_halt() {
    let (mut vm, log) = build_vm(EXPORTS);
    assert_eq!(
        exported_names(&vm),
        vec!["a".to_string(), "b".to_string(), "c".to_string()],
        "the real compiler must export exactly the three script callables"
    );
    halt_normal_root(&mut vm, &log);
    let a = export(&vm, "a");
    let b = export(&vm, "b");
    let c = export(&vm, "c");

    let argument_a = Value::array(vec![Value::Int(7)]);
    let value_a = vm
        .invoke_callable(a.clone(), std::slice::from_ref(&argument_a))
        .expect("A must run after the root frame halted normally");
    assert_eq!(value_a, Value::Int(7));
    assert_marker_record(&log, 1, &a, &argument_a);
    assert_zero_frames_active(&mut vm);

    let argument_b = Value::array(vec![Value::Int(8)]);
    let value_b = vm
        .invoke_callable(b.clone(), std::slice::from_ref(&argument_b))
        .expect("B must return the explicit [false, payload] data array, not a VmError");
    match value_b {
        Value::Array(items) => {
            assert_eq!(items.len(), 2, "B data arity must be exactly two");
            assert_eq!(items[0], Value::Bool(false), "B data tag");
            assert!(
                same_array(&items[1], &argument_b),
                "B payload must be the exact original array object"
            );
        }
        other => panic!("B must return the explicit [false, payload] data array, got {other:?}"),
    }
    assert_marker_record(&log, 2, &b, &argument_b);
    assert_zero_frames_active(&mut vm);

    let argument_c = Value::array(vec![Value::Int(9)]);
    let value_c = vm
        .invoke_callable(c.clone(), std::slice::from_ref(&argument_c))
        .expect("C must run after the root frame halted normally");
    assert_eq!(value_c, Value::Int(9));
    assert_marker_record(&log, 3, &c, &argument_c);
    assert_zero_frames_active(&mut vm);
    assert_eq!(
        phases(&log),
        vec![1, 2, 3],
        "one marker per executed script"
    );

    let begins = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let reason = Arc::new(Mutex::new(None));
    let _token = vm
        .execution_scope()
        .push_resource(SyncCloseResource {
            begins: Arc::clone(&begins),
            drops: Arc::clone(&drops),
            reason: Arc::clone(&reason),
        })
        .expect("the Active scope must admit one observable resource");
    assert_eq!(vm.host_context().resource_count(), 1);
    assert_eq!(vm.host_context().operation_count(), 0);

    vm.shutdown();
    assert_eq!(
        vm.host_context().scope_phase(),
        ScopeState::Quiescent,
        "an explicit shutdown must reach a real Quiescent"
    );
    assert_eq!(
        vm.host_context().resource_count(),
        0,
        "resource registry empty"
    );
    assert_eq!(
        vm.host_context().operation_count(),
        0,
        "operation registry empty"
    );
    assert_eq!(
        vm.execution_scope().close_reason(),
        Some(ResourceCloseReason::VmDrop)
    );
    assert_eq!(
        vm.execution_scope().terminal(),
        Some(&ScopeCloseOutcome::Success),
        "the real scope close must succeed"
    );
    assert_eq!(
        begins.load(Ordering::SeqCst),
        1,
        "the synchronous close must be requested exactly once"
    );
    assert_eq!(
        drops.load(Ordering::SeqCst),
        1,
        "the resource must be destructed exactly once"
    );
    assert_eq!(
        *reason.lock().expect("close reason"),
        Some(ResourceCloseReason::VmDrop)
    );
    assert_eq!(phases(&log), vec![1, 2, 3], "shutdown must add no marker");
    assert!(vm.execution_frames().is_empty());
    assert_eq!(vm.call_depth(), 0);
    assert_eq!(vm.queued_callable_count(), 0);
    let error = vm
        .invoke_callable(c, std::slice::from_ref(&argument_c))
        .expect_err("a shut-down VM must reject callbacks");
    assert!(
        matches!(error, VmError::InvalidFrameState("vm is shut down")),
        "unexpected error: {error:?}"
    );
}

#[test]
fn core_pre_close_pending_resource_keeps_scope_closing() {
    let (mut vm, log) = build_vm(EXPORTS);
    halt_normal_root(&mut vm, &log);
    let a = export(&vm, "a");
    let c = export(&vm, "c");

    let argument_a = Value::array(vec![Value::Int(7)]);
    let value_a = vm
        .invoke_callable(a.clone(), std::slice::from_ref(&argument_a))
        .expect("A must run on the second VM");
    assert_eq!(value_a, Value::Int(7));
    assert_marker_record(&log, 1, &a, &argument_a);
    assert_zero_frames_active(&mut vm);

    let argument_c = Value::array(vec![Value::Int(9)]);
    vm.queue_callable(c.clone(), vec![argument_c.clone()])
        .expect("C must queue after the root halted normally");
    assert_eq!(vm.queued_callable_count(), 1, "C is queued but not drained");
    assert_eq!(phases(&log), vec![1], "C must not run before drain");

    let begins = Arc::new(AtomicUsize::new(0));
    let polls = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let reasons = Arc::new(Mutex::new(Vec::new()));
    let ready = Arc::new(AtomicBool::new(false));
    let _token = vm
        .execution_scope()
        .push_resource(PendingGate {
            begins: Arc::clone(&begins),
            polls: Arc::clone(&polls),
            drops: Arc::clone(&drops),
            reasons: Arc::clone(&reasons),
            ready: Arc::clone(&ready),
        })
        .expect("the Active scope must admit the Pending gate");
    assert_eq!(vm.host_context().resource_count(), 1);

    vm.shutdown();
    assert_eq!(
        begins.load(Ordering::SeqCst),
        1,
        "shutdown must issue exactly one close request"
    );
    assert!(
        polls.load(Ordering::SeqCst) >= 1,
        "shutdown must poll the Pending gate at least once"
    );
    assert_eq!(
        vm.execution_scope().state(),
        ScopeState::Closing,
        "a Pending gate must keep the scope Closing"
    );
    assert_eq!(
        vm.execution_scope().close_reason(),
        Some(ResourceCloseReason::VmDrop)
    );
    assert!(
        vm.execution_scope().terminal().is_none(),
        "a Pending gate must not fix a terminal outcome"
    );
    assert_eq!(
        vm.host_context().resource_count(),
        1,
        "the Pending resource must still be registered"
    );
    assert_eq!(
        drops.load(Ordering::SeqCst),
        0,
        "the Pending resource must not be destructed early"
    );
    assert_eq!(
        vm.queued_callable_count(),
        0,
        "shutdown must clear the queue even while the scope stays Closing"
    );
    assert_eq!(phases(&log), vec![1], "C must still have no marker");
    let error = vm
        .invoke_callable(c.clone(), std::slice::from_ref(&argument_c))
        .expect_err("a shut-down VM must reject callbacks at the boundary");
    assert!(
        matches!(error, VmError::InvalidFrameState("vm is shut down")),
        "unexpected error: {error:?}"
    );

    let polls_before_manual = polls.load(Ordering::SeqCst);
    ready.store(true, Ordering::SeqCst);
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut manual_polls = 0usize;
    let outcome = loop {
        match vm.execution_scope().poll_close(&mut cx) {
            Poll::Pending => {
                manual_polls += 1;
                assert!(manual_polls < 8, "manual poll never reached Ready");
            }
            Poll::Ready(Ok(outcome)) => break outcome,
            Poll::Ready(Err(error)) => panic!("manual poll failed: {error:?}"),
        }
    };
    assert_eq!(outcome, ScopeCloseOutcome::Success);
    assert!(
        polls.load(Ordering::SeqCst) > polls_before_manual,
        "the explicit poll must drive the released gate"
    );
    assert_eq!(
        begins.load(Ordering::SeqCst),
        1,
        "the close request must stay exactly once"
    );
    assert_eq!(drops.load(Ordering::SeqCst), 1, "one real destructor");
    assert_eq!(
        vm.execution_scope().state(),
        ScopeState::Quiescent,
        "the explicit poll must reach Quiescent"
    );
    assert_eq!(
        vm.execution_scope().terminal(),
        Some(&ScopeCloseOutcome::Success)
    );
    assert_eq!(
        vm.host_context().resource_count(),
        0,
        "resource registry empty"
    );
    assert_eq!(
        vm.host_context().operation_count(),
        0,
        "operation registry empty"
    );
    assert_eq!(
        *reasons.lock().expect("gate close reasons"),
        vec![ResourceCloseReason::VmDrop]
    );
    assert_eq!(vm.queued_callable_count(), 0);
    assert_eq!(phases(&log), vec![1], "C must never execute");
}

#[test]
fn core_pre_close_host_fault_leaves_queue_and_shutdown_discards() {
    let (mut vm, log) = build_vm(QUEUE_FAULT);
    assert_eq!(
        exported_names(&vm),
        vec!["a".to_string(), "c".to_string(), "fault".to_string()]
    );
    halt_normal_root(&mut vm, &log);
    let a = export(&vm, "a");
    let fault = export(&vm, "fault");
    let c = export(&vm, "c");
    let fault_prototype = callable_prototype(&fault);

    let argument_a = Value::array(vec![Value::Int(7)]);
    let argument_fault = Value::array(vec![Value::Int(8)]);
    let argument_c = Value::array(vec![Value::Int(9)]);
    vm.queue_callable(a.clone(), vec![argument_a])
        .expect("A must queue");
    vm.queue_callable(fault.clone(), vec![argument_fault])
        .expect("the fault script must queue");
    vm.queue_callable(c.clone(), vec![argument_c])
        .expect("C must queue behind the fault");
    assert_eq!(vm.queued_callable_count(), 3);

    let error = vm
        .drain_callable_queue()
        .expect_err("the real host error must abort the drain");
    assert!(
        matches!(error, VmError::HostError(ref message) if message == FAULT_MESSAGE),
        "the original host error must surface unchanged: {error:?}"
    );
    assert_eq!(
        vm.queued_callable_count(),
        1,
        "C must remain queued when the fault returns"
    );
    assert_eq!(
        phases(&log),
        vec![1, 2],
        "A and the real fault marker must run; C must not"
    );
    let records = log.lock().expect("mark log");
    assert_eq!(records[0].prototype_id, Some(callable_prototype(&a)));
    assert_eq!(
        records[1].prototype_id,
        Some(fault_prototype),
        "the fault marker must run in the real fault export"
    );
    assert_eq!(
        records[1].continuation,
        Some(VmFrameContinuation::ReturnToHost)
    );
    drop(records);
    assert!(
        vm.execution_frames().is_empty(),
        "the error cleanup returns to zero real frames"
    );
    assert_eq!(vm.call_depth(), 0);
    assert_eq!(
        vm.execution_scope().state(),
        ScopeState::Active,
        "the scope itself stays Active after the callback error"
    );

    vm.shutdown();
    assert_eq!(
        vm.queued_callable_count(),
        0,
        "shutdown must discard C without running it"
    );
    assert_eq!(phases(&log), vec![1, 2], "shutdown must not add a C marker");
    assert_eq!(vm.execution_scope().state(), ScopeState::Quiescent);
    assert_eq!(
        vm.execution_scope().terminal(),
        Some(&ScopeCloseOutcome::Success)
    );
}
