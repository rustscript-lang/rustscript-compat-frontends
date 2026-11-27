//! Synchronous `CallValue(2)` returns `Bool`, `parent-after` still borrows A,
//! and an exact `retire` import then calls `leave_roots` on this commit's full
//! `RootFrameId`. The import does not collect and does not advance a phase.
//! `sweep` is the later guest collection. `done` only observes.
//!
//! The omission arm reaches the same `sweep` and cleans the same kind of
//! unrooted weak orphan, but it never calls `retire`, so A and the commit
//! root frame stay. The commit witness keeps a strong callable, and B stays
//! reachable through the raw `__call` edge. This file does not claim that Arc
//! has retired.

use vm::{CallOutcome, CallReturn, ResourceHandle, Value, Vm, VmError, VmStatus};

use crate::heap::{self, GuestClass, LuaUserdataResource, TableRef, UserdataRef};

use super::{
    Arm, ArmRun, host_fault_error, published_commit, published_pending, published_prepared,
    published_slot_generation, published_userdata_alive, raw_handle, run_arm,
};

const RETIRE_MODE: i64 = 12;
const ORPHAN_KEY: f64 = 2.0;
const CLEARED_PREPARE_KEY: f64 = 1.0;

#[derive(Clone, Debug)]
struct ExitRecord {
    frame: heap::RootFrameId,
    prepare_frame: heap::RootFrameId,
    top_before: heap::RootFrameId,
    top_after: heap::RootFrameId,
    roots_before: u32,
    roots_after: u32,
    collections_before: u32,
    collections_after: u32,
    scan_before: heap::ScanMeter,
    scan_after: heap::ScanMeter,
    tail_before: bool,
    tail_after: bool,
    mark_before: bool,
    mark_after: bool,
    token_open_before: bool,
    borrowed_userdata: UserdataRef,
    weak_before: TableRef,
    receiver_before: TableRef,
    sentinel_before: TableRef,
    weak_after: TableRef,
    receiver_after: TableRef,
    sentinel_after: TableRef,
    stale_userdata: bool,
    stale_closure: bool,
    stale_receiver: bool,
}

#[derive(Clone, Debug)]
struct SweepRecord {
    orphan: TableRef,
    orphan_rooted: bool,
    roots_before: u32,
    roots_after_create: u32,
    collections_before: u32,
    collections_after: u32,
    weak_cleared: bool,
    prepare_key_still_null: bool,
    orphan_dead: bool,
}

#[derive(Clone, Debug, Default)]
struct RetirementScope {
    exit: Option<ExitRecord>,
    sweep: Option<SweepRecord>,
}

fn with_retirement<T>(
    vm: &mut Vm,
    body: impl FnOnce(&mut RetirementScope) -> T,
) -> vm::VmResult<T> {
    let mut host = vm.host_context();
    if host.scope_state::<RetirementScope>().is_none() {
        host.scope_state_or_insert_with(RetirementScope::default)
            .map_err(|error| host_fault_error(format!("retirement scope: {error:?}")))?;
    }
    let scope = host
        .scope_state_mut::<RetirementScope>()
        .ok_or_else(|| host_fault_error("retirement scope was not installed"))?;
    Ok(body(scope))
}

fn heap_fault(error: heap::HeapError) -> VmError {
    super::heap_fault(error)
}

/// Exact exit import. Confirms the three commit roots and the live token, then
/// calls `leave_roots` on the successful commit's full frame id. It does not
/// collect, step, or clear a root.
pub(super) fn host_retire(vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
    if !args.is_empty() {
        return Err(host_fault_error("retire takes no arguments"));
    }
    let prepared = super::with_run_scope(vm, |scope| scope.witness.prepared.clone())
        .ok_or_else(|| host_fault_error("retire requires the prepared published head"))?;
    let commit = super::with_run_scope(vm, |scope| {
        scope
            .witness
            .commits
            .iter()
            .rev()
            .find(|commit| commit.outcome.is_ok())
            .cloned()
    })
    .ok_or_else(|| host_fault_error("retire requires this run's successful commit"))?;
    let (frame, roots_written) = commit.outcome.clone().map_err(|error| {
        host_fault_error(format!("retire requires a successful commit, got {error}"))
    })?;
    if roots_written != 3 {
        return Err(host_fault_error(
            "retire requires the commit that wrote exactly three roots",
        ));
    }

    let mut host = vm.host_context();
    let tail_before = heap::with_heap(&mut host, |heap| Ok(heap.saved_tail_state().is_some()))
        .map_err(heap_fault)?;
    if tail_before {
        return Err(host_fault_error(
            "retire refuses a saved tail; it does not finish or advance one",
        ));
    }
    let mark_before = heap::saved_mark_watch(&host).is_some();
    if mark_before {
        return Err(host_fault_error(
            "retire refuses a saved mark; it does not advance a phase",
        ));
    }
    let stats_before = heap::with_heap(&mut host, |heap| Ok(heap.stats())).map_err(heap_fault)?;
    let scan_before =
        heap::with_heap(&mut host, |heap| Ok(heap.scan_meter())).map_err(heap_fault)?;
    let top_before = heap::with_heap(&mut host, |heap| Ok(heap.current_frame()))
        .map_err(heap_fault)?
        .ok_or_else(|| host_fault_error("retire requires a current root frame"))?;
    if top_before != frame {
        return Err(host_fault_error(
            "retire only leaves the current top frame, which must be this commit",
        ));
    }
    let rooted_user = heap::with_heap(&mut host, |heap| Ok(heap.rooted_userdata(frame, 0)))
        .map_err(heap_fault)?
        .map_err(heap_fault)?;
    let rooted_closure = heap::with_heap(&mut host, |heap| Ok(heap.rooted_closure(frame, 1)))
        .map_err(heap_fault)?
        .map_err(heap_fault)?;
    let rooted_receiver = heap::with_heap(&mut host, |heap| Ok(heap.rooted_table(frame, 2)))
        .map_err(heap_fault)?
        .map_err(heap_fault)?;
    if rooted_user != Some(prepared.userdata) {
        return Err(host_fault_error(
            "the commit frame's first root is not the live userdata A",
        ));
    }
    let Some(closure) = rooted_closure else {
        return Err(host_fault_error("the commit frame's closure root is empty"));
    };
    if (closure.domain(), closure.slot(), closure.generation()) != commit.action_closure {
        return Err(host_fault_error(
            "the commit frame's closure root is not the committed raw action",
        ));
    }
    if rooted_receiver != Some(prepared.receiver) {
        return Err(host_fault_error(
            "the commit frame's receiver root is not the prepared receiver",
        ));
    }
    let weak_before = heap::with_heap(&mut host, |heap| {
        Ok(heap.rooted_table(prepared.root_frame, 0))
    })
    .map_err(heap_fault)?
    .map_err(heap_fault)?;
    let receiver_before = heap::with_heap(&mut host, |heap| {
        Ok(heap.rooted_table(prepared.root_frame, 1))
    })
    .map_err(heap_fault)?
    .map_err(heap_fault)?;
    let sentinel_before = heap::with_heap(&mut host, |heap| {
        Ok(heap.rooted_table(prepared.root_frame, 2))
    })
    .map_err(heap_fault)?
    .map_err(heap_fault)?;
    if weak_before != Some(prepared.weak_table)
        || receiver_before != Some(prepared.receiver)
        || sentinel_before != Some(prepared.sentinel)
    {
        return Err(host_fault_error(
            "the prepare frame does not still hold the weak table, receiver, and sentinel",
        ));
    }
    // End this host borrow before the next host_context() call.
    {
        let _host = host;
    }

    let borrowed_userdata = {
        let host = vm.host_context();
        let borrowed = host
            .borrow_resource_with_key::<LuaUserdataResource>(
                raw_handle(prepared.userdata_raw)?,
                &LuaUserdataResource::key(),
            )
            .map_err(|error| {
                host_fault_error(format!("A token must still borrow before retire: {error}"))
            })?;
        borrowed.get().userdata()
    };
    if borrowed_userdata != prepared.userdata {
        return Err(host_fault_error(
            "the live token borrow does not name userdata A",
        ));
    }

    let mut host = vm.host_context();
    heap::with_heap(&mut host, |heap| heap.leave_roots(frame)).map_err(heap_fault)?;
    let tail_after = heap::with_heap(&mut host, |heap| Ok(heap.saved_tail_state().is_some()))
        .map_err(heap_fault)?;
    let mark_after = heap::saved_mark_watch(&host).is_some();
    let stats_after = heap::with_heap(&mut host, |heap| Ok(heap.stats())).map_err(heap_fault)?;
    let scan_after =
        heap::with_heap(&mut host, |heap| Ok(heap.scan_meter())).map_err(heap_fault)?;
    let top_after = heap::with_heap(&mut host, |heap| Ok(heap.current_frame()))
        .map_err(heap_fault)?
        .ok_or_else(|| host_fault_error("leave_roots left no prepare frame"))?;
    if top_after != prepared.root_frame {
        return Err(host_fault_error(
            "leave_roots must reveal the prepare frame as the new top",
        ));
    }
    if stats_after.root_slots != stats_before.root_slots - 3 {
        return Err(host_fault_error(format!(
            "leave_roots must shorten the root vector by exactly three, {} -> {}",
            stats_before.root_slots, stats_after.root_slots
        )));
    }
    if stats_after.collections != stats_before.collections || scan_after != scan_before {
        return Err(host_fault_error(
            "leave_roots collected or advanced a scan; this import must not",
        ));
    }
    if tail_after || mark_after {
        return Err(host_fault_error(
            "leave_roots saved a tail or a mark; this import must not",
        ));
    }
    let stale_userdata = heap::with_heap(&mut host, |heap| Ok(heap.rooted_userdata(frame, 0)))
        .map_err(heap_fault)?;
    let stale_closure =
        heap::with_heap(&mut host, |heap| Ok(heap.rooted_closure(frame, 1))).map_err(heap_fault)?;
    let stale_receiver =
        heap::with_heap(&mut host, |heap| Ok(heap.rooted_table(frame, 2))).map_err(heap_fault)?;
    if !matches!(stale_userdata, Err(heap::HeapError::StaleFrame))
        || !matches!(stale_closure, Err(heap::HeapError::StaleFrame))
        || !matches!(stale_receiver, Err(heap::HeapError::StaleFrame))
    {
        return Err(host_fault_error(
            "the retired commit frame is still readable",
        ));
    }
    let weak_after = heap::with_heap(&mut host, |heap| {
        Ok(heap.rooted_table(prepared.root_frame, 0))
    })
    .map_err(heap_fault)?
    .map_err(heap_fault)?;
    let receiver_after = heap::with_heap(&mut host, |heap| {
        Ok(heap.rooted_table(prepared.root_frame, 1))
    })
    .map_err(heap_fault)?
    .map_err(heap_fault)?;
    let sentinel_after = heap::with_heap(&mut host, |heap| {
        Ok(heap.rooted_table(prepared.root_frame, 2))
    })
    .map_err(heap_fault)?
    .map_err(heap_fault)?;
    if weak_after != weak_before
        || receiver_after != receiver_before
        || sentinel_after != sentinel_before
    {
        return Err(host_fault_error(
            "leave_roots changed the prepare frame's weak table, receiver, or sentinel",
        ));
    }
    let record = ExitRecord {
        frame,
        prepare_frame: prepared.root_frame,
        top_before,
        top_after,
        roots_before: stats_before.root_slots,
        roots_after: stats_after.root_slots,
        collections_before: stats_before.collections,
        collections_after: stats_after.collections,
        scan_before,
        scan_after,
        tail_before,
        tail_after,
        mark_before,
        mark_after,
        token_open_before: true,
        borrowed_userdata,
        weak_before: weak_before.expect("weak before"),
        receiver_before: receiver_before.expect("receiver before"),
        sentinel_before: sentinel_before.expect("sentinel before"),
        weak_after: weak_after.expect("weak after"),
        receiver_after: receiver_after.expect("receiver after"),
        sentinel_after: sentinel_after.expect("sentinel after"),
        stale_userdata: true,
        stale_closure: true,
        stale_receiver: true,
    };
    // End this host borrow before the next host_context() call.
    {
        let _host = host;
    }
    with_retirement(vm, |scope| {
        scope.exit = Some(record);
    })?;
    Ok(CallOutcome::Return(CallReturn::one(Value::Bool(true))))
}

/// Guest collection after the parent returns. Creates one weak orphan, does
/// not root it, and runs the real collector. It does not call `leave_roots`
/// or `clear_root`.
pub(super) fn host_sweep(vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
    if !args.is_empty() {
        return Err(host_fault_error("sweep takes no arguments"));
    }
    let prepared = super::with_run_scope(vm, |scope| scope.witness.prepared.clone())
        .ok_or_else(|| host_fault_error("sweep requires the prepared published head"))?;
    let mut host = vm.host_context();
    if heap::with_heap(&mut host, |heap| Ok(heap.saved_tail_state().is_some()))
        .map_err(heap_fault)?
    {
        return Err(host_fault_error(
            "sweep refuses a saved tail; the guest path collects only when sweep is open",
        ));
    }
    let roots_before =
        heap::with_heap(&mut host, |heap| Ok(heap.stats().root_slots)).map_err(heap_fault)?;
    let collections_before =
        heap::with_heap(&mut host, |heap| Ok(heap.stats().collections)).map_err(heap_fault)?;
    let orphan = {
        let value = heap::publish_guest(&mut host).map_err(heap_fault)?;
        let class =
            heap::with_heap(&mut host, |heap| Ok(heap.guest_class(&value))).map_err(heap_fault)?;
        let GuestClass::Table(orphan) = class else {
            return Err(host_fault_error(
                "the guest orphan must be an issued heap table",
            ));
        };
        orphan
    };
    if orphan == prepared.weak_table || orphan == prepared.receiver || orphan == prepared.sentinel {
        return Err(host_fault_error(
            "the guest orphan must be a new table, not an already rooted one",
        ));
    }
    heap::with_heap(&mut host, |heap| heap.set_edge(orphan, 1, orphan)).map_err(heap_fault)?;
    heap::with_heap(&mut host, |heap| {
        heap.set_number_edge(prepared.weak_table, ORPHAN_KEY, orphan)
    })
    .map_err(heap_fault)?;
    let roots_after_create =
        heap::with_heap(&mut host, |heap| Ok(heap.stats().root_slots)).map_err(heap_fault)?;
    let orphan_rooted = heap::current_frame_roots_table(&host, orphan);
    if orphan_rooted || roots_after_create != roots_before {
        return Err(host_fault_error(
            "the guest orphan must not be entered into a root frame",
        ));
    }
    let prepare_key = heap::with_heap(&mut host, |heap| {
        heap.number_value(prepared.weak_table, CLEARED_PREPARE_KEY)
    })
    .map_err(heap_fault)?;
    // The guest path collects here, after the unrooted weak orphan exists and
    // before done. This import does not leave roots.
    heap::collect(&mut host).map_err(heap_fault)?;
    let collections_after =
        heap::with_heap(&mut host, |heap| Ok(heap.stats().collections)).map_err(heap_fault)?;
    let weak_value = heap::with_heap(&mut host, |heap| {
        heap.number_value(prepared.weak_table, ORPHAN_KEY)
    })
    .map_err(heap_fault)?;
    let prepare_after = heap::with_heap(&mut host, |heap| {
        heap.number_value(prepared.weak_table, CLEARED_PREPARE_KEY)
    })
    .map_err(heap_fault)?;
    let orphan_dead =
        heap::with_heap(&mut host, |heap| Ok(!heap.alive(orphan))).map_err(heap_fault)?;
    let record = SweepRecord {
        orphan,
        orphan_rooted,
        roots_before,
        roots_after_create,
        collections_before,
        collections_after,
        weak_cleared: weak_value == Value::Null,
        prepare_key_still_null: prepare_key == Value::Null && prepare_after == Value::Null,
        orphan_dead,
    };
    // End this host borrow before the next host_context() call.
    {
        let _host = host;
    }
    with_retirement(vm, |scope| {
        scope.sweep = Some(record);
    })?;
    Ok(CallOutcome::Return(CallReturn::one(Value::Bool(true))))
}

fn take_retirement(run: &mut ArmRun) -> RetirementScope {
    let mut host = run.vm.host_context();
    host.take_scope_state::<RetirementScope>()
        .unwrap_or_else(|| {
            panic!(
                "{}: the retirement scope was not installed",
                run.arm.label()
            )
        })
}

fn token_borrows(run: &mut ArmRun, raw: i64) -> bool {
    let Ok(raw) = u64::try_from(raw) else {
        return false;
    };
    let Ok(handle) = ResourceHandle::from_raw(raw) else {
        return false;
    };
    let host = run.vm.host_context();
    host.borrow_resource_with_key::<LuaUserdataResource>(handle, &LuaUserdataResource::key())
        .is_ok()
}

fn root_slots(run: &mut ArmRun) -> u32 {
    let mut host = run.vm.host_context();
    heap::with_heap(&mut host, |heap| Ok(heap.stats().root_slots)).expect("root length")
}

fn collections(run: &mut ArmRun) -> u32 {
    let mut host = run.vm.host_context();
    heap::with_heap(&mut host, |heap| Ok(heap.stats().collections)).expect("collections")
}

fn current_root(run: &mut ArmRun) -> Option<heap::RootFrameId> {
    let mut host = run.vm.host_context();
    heap::with_heap(&mut host, |heap| Ok(heap.current_frame())).expect("current frame")
}

fn assert_halted(run: &ArmRun) {
    assert!(
        matches!(run.status, Ok(VmStatus::Halted)),
        "{}: the script must halt, got {:?}",
        run.arm.label(),
        run.status
    );
}

fn assert_call_value_two(run: &ArmRun) {
    let parent = run
        .mark("parent-enter")
        .last_prototype
        .unwrap_or_else(|| panic!("{}: parent-enter has no prototype", run.arm.label()));
    let dynamic: Vec<_> = run
        .call_sites
        .iter()
        .filter(|site| site.owner == Some(parent) && site.argc == 2)
        .collect();
    assert_eq!(
        dynamic.len(),
        1,
        "{}: the parent has one dynamic call",
        run.arm.label()
    );
    assert_eq!(
        dynamic[0].argc,
        2,
        "{}: the dynamic call is CallValue(2)",
        run.arm.label()
    );
}

fn assert_parent_after_borrows_a(run: &ArmRun) {
    let prepared = published_prepared(run);
    let after = run.mark("parent-after");
    let probe = after.continuation.as_ref().unwrap_or_else(|| {
        panic!(
            "{}: parent-after did not borrow the argument token",
            run.arm.label()
        )
    });
    assert_eq!(
        probe.raw,
        prepared.userdata_raw,
        "{}: parent-after borrowed a different token",
        run.arm.label()
    );
    assert_eq!(
        probe.parts,
        (
            prepared.userdata.domain(),
            prepared.userdata.slot(),
            prepared.userdata.generation(),
        ),
        "{}: parent-after's borrow is not userdata A",
        run.arm.label()
    );
}

fn assert_compiled_bool(run: &ArmRun, expected: bool) {
    let slot = run.witness.call_result_slot.unwrap_or_else(|| {
        panic!(
            "{}: the compiled CallValue result was not stored in a frame local",
            run.arm.label()
        )
    });
    assert_eq!(
        run.witness.call_result,
        Some(Value::Bool(expected)),
        "{}: the compiled call result slot {slot:?} does not hold the callback bool",
        run.arm.label()
    );
    assert!(
        run.witness.host_true_beside_call,
        "{}: a host import true must still sit beside the call result, or the slot check cannot tell them apart",
        run.arm.label()
    );
}

fn assert_sweep_cleaned_orphan(run: &mut ArmRun, sweep: &SweepRecord) {
    assert!(
        !sweep.orphan_rooted && sweep.roots_before == sweep.roots_after_create,
        "{}: the weak orphan {:?} was rooted",
        run.arm.label(),
        sweep.orphan
    );
    assert!(
        sweep.weak_cleared && sweep.orphan_dead && sweep.prepare_key_still_null,
        "{}: the guest collection did not clear the new weak orphan",
        run.arm.label()
    );
    assert_eq!(
        sweep.collections_after,
        sweep.collections_before + 1,
        "{}: sweep must run exactly one real collection",
        run.arm.label()
    );
    assert_eq!(
        collections(run),
        sweep.collections_after,
        "{}: a collection ran after the guest sweep or after Vm::run",
        run.arm.label()
    );
    assert_eq!(
        root_slots(run),
        sweep.roots_after_create,
        "{}: roots changed after the guest sweep",
        run.arm.label()
    );
}

fn assert_b_stays(run: &mut ArmRun) {
    let prepared = published_prepared(run).clone();
    let (callable_kept, action) = {
        let commit = published_commit(run);
        let callable_kept = matches!(
            &commit.callable,
            Value::Callable(arc) if std::sync::Arc::strong_count(arc) >= 1
        );
        (callable_kept, commit.action_closure)
    };
    assert!(
        callable_kept,
        "{}: the commit witness must keep the strong callable; this is not an Arc retirement",
        run.arm.label()
    );
    let edge = super::heap_query(&mut run.vm, |heap| {
        heap.named_closure_edge(prepared.receiver_metatable, "__call")
    });
    assert_eq!(
        (edge.domain(), edge.slot(), edge.generation()),
        action,
        "{}: B must stay reachable through the raw __call edge",
        run.arm.label()
    );
    assert_eq!(
        published_slot_generation(run, prepared.successor),
        prepared.successor.generation(),
        "{}: B's generation changed",
        run.arm.label()
    );
    assert!(
        token_borrows(run, prepared.successor_raw),
        "{}: B's token must still borrow",
        run.arm.label()
    );
    assert_eq!(
        published_pending(run),
        vec![(
            u64::try_from(prepared.successor_raw).expect("B token"),
            prepared.successor.slot(),
            prepared.successor.generation(),
        )],
        "{}: B must remain the pending head",
        run.arm.label()
    );
}

fn assert_exit_retired_only_the_commit_frame(run: &mut ArmRun, exit: &ExitRecord) {
    let userdata = published_prepared(run).userdata;
    assert_eq!(exit.top_before, exit.frame);
    assert_eq!(exit.prepare_frame, published_prepared(run).root_frame);
    assert_eq!(exit.top_after, exit.prepare_frame);
    assert_eq!(exit.roots_after, exit.roots_before - 3);
    assert_eq!(exit.collections_before, exit.collections_after);
    assert_eq!(exit.scan_before, exit.scan_after);
    assert!(!exit.tail_before && !exit.tail_after);
    assert!(!exit.mark_before && !exit.mark_after);
    assert!(exit.token_open_before);
    assert_eq!(exit.borrowed_userdata, userdata);
    assert!(exit.stale_userdata && exit.stale_closure && exit.stale_receiver);
    assert_eq!(exit.weak_before, published_prepared(run).weak_table);
    assert_eq!(exit.receiver_before, published_prepared(run).receiver);
    assert_eq!(exit.sentinel_before, published_prepared(run).sentinel);
    assert_eq!(exit.weak_after, exit.weak_before);
    assert_eq!(exit.receiver_after, exit.receiver_before);
    assert_eq!(exit.sentinel_after, exit.sentinel_before);
    assert_eq!(
        current_root(run),
        Some(exit.prepare_frame),
        "{}: the prepare frame must still be top after Vm::run; done must not clear roots",
        run.arm.label()
    );
    let mut host = run.vm.host_context();
    let stale = heap::with_heap(&mut host, |heap| Ok(heap.rooted_userdata(exit.frame, 0)))
        .expect("retired frame read");
    assert!(
        matches!(stale, Err(heap::HeapError::StaleFrame)),
        "{}: the retired frame became readable after Vm::run",
        run.arm.label()
    );
}

fn assert_common_guest_path(run: &mut ArmRun) -> (Option<ExitRecord>, SweepRecord) {
    assert_halted(run);
    assert_eq!(
        run.labels(),
        ["parent-enter", "target-enter", "parent-after", "done"],
        "{}: done is an observation mark after the call",
        run.arm.label()
    );
    assert_call_value_two(run);
    assert_parent_after_borrows_a(run);
    let scope = take_retirement(run);
    let sweep = scope
        .sweep
        .clone()
        .unwrap_or_else(|| panic!("{}: sweep did not run", run.arm.label()));
    assert_sweep_cleaned_orphan(run, &sweep);
    assert_b_stays(run);
    (scope.exit, sweep)
}

#[test]
fn synchronous_table_call_retires_the_commit_root_before_guest_collection() {
    let mut run = run_arm(Arm::CallbackRootRetirement);
    assert_eq!(i64::try_from(run.arm.select()).unwrap(), RETIRE_MODE);
    let (exit, _sweep) = assert_common_guest_path(&mut run);
    let exit = exit.expect("retirement: retire must run inside the parent frame");
    assert_exit_retired_only_the_commit_frame(&mut run, &exit);
    assert_compiled_bool(&run, true);
    let prepared = published_prepared(&run).clone();
    assert!(
        !published_userdata_alive(&mut run, prepared.userdata),
        "retirement: guest collection must drop A after the exit import"
    );
    assert_eq!(
        published_slot_generation(&mut run, prepared.userdata),
        prepared.userdata.generation() + 1,
        "retirement: A's slot generation must advance by one"
    );
    assert!(
        !token_borrows(&mut run, prepared.userdata_raw),
        "retirement: A's real borrow must fail after the guest collection"
    );
}

#[test]
fn omitted_exit_import_keeps_the_commit_root_through_guest_collection() {
    let mut run = run_arm(Arm::CallbackRootOmission);
    let (exit, sweep) = assert_common_guest_path(&mut run);
    assert!(exit.is_none(), "omission: the exit import must not run");
    assert_compiled_bool(&run, true);
    let prepared = published_prepared(&run).clone();
    let commit = published_commit(&run);
    let frame = commit.outcome.clone().expect("omission commit").0;
    assert!(
        published_userdata_alive(&mut run, prepared.userdata),
        "omission: A stays alive because the exit import was not called"
    );
    assert_eq!(
        published_slot_generation(&mut run, prepared.userdata),
        prepared.userdata.generation(),
        "omission: A's generation stays"
    );
    assert!(
        token_borrows(&mut run, prepared.userdata_raw),
        "omission: A's token still borrows"
    );
    assert_eq!(
        current_root(&mut run),
        Some(frame),
        "omission: the commit root frame is still top"
    );
    let rooted = {
        let mut host = run.vm.host_context();
        heap::with_heap(&mut host, |heap| Ok(heap.rooted_userdata(frame, 0)))
            .expect("omission frame read")
            .expect("omission frame must still be readable")
    };
    assert_eq!(rooted, Some(prepared.userdata));
    assert_eq!(
        root_slots(&mut run),
        sweep.roots_after_create,
        "omission: sweep did not shorten the root vector"
    );
}

#[test]
fn callback_bool_false_is_the_compiled_slot_not_a_host_true() {
    let mut run = run_arm(Arm::CallbackRootFalseReturn);
    assert_eq!(i64::try_from(run.arm.select()).unwrap(), RETIRE_MODE);
    let (exit, _sweep) = assert_common_guest_path(&mut run);
    let exit = exit.expect("false-return: the same exit import must run");
    assert_exit_retired_only_the_commit_frame(&mut run, &exit);
    assert_compiled_bool(&run, false);
    let prepared = published_prepared(&run).clone();
    assert!(
        !published_userdata_alive(&mut run, prepared.userdata),
        "false-return: the same exit import still retires A"
    );
    assert!(
        !token_borrows(&mut run, prepared.userdata_raw),
        "false-return: A's real borrow fails after the same guest collection"
    );
}
