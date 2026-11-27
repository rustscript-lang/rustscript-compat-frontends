//! Private Lua table heap and explicit root frames.
//!
//! Not wired into `lua/mod.rs`, `lib.rs`, or the `.lua` entry. Tests compile
//! this file through `#[path]`. A future production wiring is a separate scope.
//!
//! Table nodes, reference edges, explicit root frames, and the mark/sweep scan
//! live in one `LuaHeap` stored in the current `ExecutionScope`. A published
//! guest value is a typed `resource<lua.table>` token in that same scope's
//! resource table. The token is not a second heap and is not a GC root: only
//! an explicit root frame keeps a node alive across `collect`. Collection
//! closes each swept node's token through the scope close contract and returns
//! that quota before the node is released.
//!
//! A bare userdata keeps its existing semantics: no guest carrier, finalizer,
//! environment, or Core token. The private pending-finalizer slice adds one
//! optional metatable reference and a retained state for a live unmarked
//! typed userdata; neither is a guest metamethod or a callback path. The
//! private host path in this module also issues
//! a separate `resource<lua.userdata>` capability for one userdata node. Its
//! heap slot stores the raw Core handle and the payload stores the exact Lua
//! domain, kind, slot, and generation, so the checked borrow rejects a wrong
//! heap, kind, key, generation, or same-type token for another object. The
//! close is synchronous (`Ready`); a `Pending` result is an error. The host
//! collector closes that token through the same `retire_doomed_token` path as
//! a table token and clears the exact binding before weak publication and
//! Release. A pure `LuaHeap` sweep refuses a dead slot that still carries a
//! bound token instead of skipping it.
//!
//! The private pending-finalizer slice has one selecting entry,
//! [`collect_pending_finalizers`]. After the ordinary mark converges it scans
//! every slot once, retains live unmarked typed userdata whose metatable has a
//! raw non-nil `__gc` field, and marks the userdata so the ordinary gray pass
//! follows its userdata-to-metatable strong edge and the metatable's own
//! strong entries. A retained userdata keeps its original generation, typed
//! payload, and Core token. A white candidate whose raw `__gc` is missing, nil,
//! or a weak-cleared gap is settled instead: that exact generation is marked
//! finalized without a queue record and without a mark, so it is never queued
//! again even if a raw `__gc` is installed later, and an unreachable one dies
//! in the same cycle. Settlement is not retention.
//!
//! At weak publication a weak value that names a retained pending userdata or
//! a live settled generation is cleared, while a weak key keeps the entry and
//! its scalar only when the object is actually marked. A `k`-only table also
//! clears a pending or settled userdata value from its string-keyed and
//! object-keyed hash entries; numeric keys keep the previous strong-value rule
//! because this representation has no array/hash split. A sweep with no dead
//! slot still runs the Owners walk when a pending finalizer or a marked
//! settled generation needs that publication. Ordinary collections never
//! select; they offer every existing record as a host-retained edge, so no
//! live token is closed or orphaned while it waits for a consumer.
//!
//! A successful private collection records the queue length it published as a
//! consumable prefix only after the real weak publication finished. The
//! private real handoff [`take_published_pending_finalizer`] then pops exactly
//! that front record with one `VecDeque` front removal, verifies the exact
//! live userdata and typed Core token, writes the userdata into one specified
//! empty root slot, and settles that generation in the same commit. It never
//! calls guest code. A saved mark or Release tail refuses before any write, and
//! records retained by a failed or ordinary collection stay outside the ready
//! prefix. The first-round order is only the selecting scan's offered order
//! (ascending slots in this representation), not LuaJIT's reverse allocation
//! order and not a guest callback order. The test-only retirement control
//! remains a historical fixture with the same prefix invariant. No callback,
//! resurrection, guest step, or production entry is added here.
//!
//! The private action handoff [`take_published_pending_finalizer_action`]
//! resolves the current raw `__gc` of that same published head in the exclusive
//! borrow that pops it, so a change made between two dequeues is observed. It
//! never reuses the selection cache and never pre-parses a batch. A missing
//! metatable, a missing field, or a true stored-nil gap is `Skip`; a present
//! non-callable value keeps only its original Lua type tag, never the scalar
//! bytes; a live closure is the direct action; a live table or a live
//! userdata adds only its own current instance metatable's raw `__call`
//! closure, with the original object kept as the receiver. Neither receiver
//! form inherits `__call` and neither dispatches a table-valued `__call`
//! again. The commit roots the userdata, the actual closure, and every
//! independent receiver in caller-supplied empty, distinct slots, then settles
//! once; a userdata receiver that is the pending object itself is already
//! rooted as that object and needs no second slot. `Skip` and a typed reject
//! retire exactly once with only the userdata root. An independent userdata
//! receiver is checked through the same typed `lua.userdata` owner contract as
//! the head before the pop, and the commit re-resolves the action in its own
//! exclusive borrow and requires that exact identity. No guest callback,
//! diagnostic text, or Core handle copy is produced here.
//!
//! Borrowed from the ES5 heap at `3e348a010bc8d84baa5b7d1169a5f50aeb4e46ec`
//! `src/javascript/heap.rs`: slot plus generation handles, explicit root
//! frames, and non-moving mark/sweep. The guest table check uses the issued
//! VM array identity instead of numeric guest backing. Not borrowed:
//! JavaScript classifications, swc, or guest IR.
//!
//! A guest table is the VM array issued for one live slot. The slot stores a
//! weak pointer to that array and checks the pointer, the slot, and the
//! generation together. The weak pointer is not a GC root and does not keep
//! the array alive. An ordinary array with the same fields is not a table.
//! This check is the experimental guest boundary for this slice. It is not a
//! measured production collection scheme.
//!
//! Closure and upvalue nodes use the same slots, edges, roots, meter, and
//! sweep. A root slot holds a table, a closure, or an upvalue. A table field can point at
//! a closure, a closure at one shared upvalue, and that upvalue at its current
//! table, closure, boolean, binary64, string, or bytes. A missing current edge is nil. Two closures can point at
//! the same upvalue. A table entry key can be a table or a closure. Both use
//! the same object-key record: slot, generation, and kind. Mark and sweep walk
//! that record in the existing pass. A closure key is not a field id and is
//! not a guest value. Closure and upvalue nodes have no guest carrier and no
//! Core resource token. They are not callable values.
//!
//! Later integration, not this kernel: LuaJIT `ordered.lua` plus the reverse
//! and skip-collection controls in
//! `/mnt/workspace/rustscript-lua/evidence/lua52-gc-arg-order/qualification.md`.
//! When `.lua` is connected, that stage consumes the sample. A collection
//! witness must be a helper that only returns a boolean; do not inline-read a
//! weak referent from the caller. Only a table entry follows `__mode`. A
//! frame root is not a weak edge.

#[path = "heap/handle.rs"]
mod handle;
#[path = "heap/resource.rs"]
mod resource;
#[path = "heap/table.rs"]
mod table;

pub use handle::{ClosureRef, HandleKind, TableRef, UpvalueRef, UserdataRef};
pub use resource::{LuaTableResource, LuaUserdataResource};
#[cfg(test)]
pub(crate) use table::SelectingPendingMark;
pub use table::{
    CellValue, CollectReport, GuestClass, HeapError, HeapLimits, HeapStats, LuaHeap, ObserveFault,
    PublishedRead, RootFrameId, ScanMeter,
};
#[cfg(test)]
pub use table::{SavedTailPhase, SavedTailState};

use vm::{HostContext, Value};

/// Installs the scope heap before the first use. A second install is refused
/// so a test cannot silently replace a live heap.
pub fn install(host: &mut HostContext, limits: HeapLimits) -> Result<(), HeapError> {
    if host.scope_state::<LuaHeap>().is_some() {
        return Err(HeapError::AlreadyInstalled);
    }
    host.scope_state_or_insert_with(|| LuaHeap::new(limits))
        .map_err(HeapError::from_scope)?;
    Ok(())
}

/// Allocates a table and publishes its typed resource token.
///
/// The node is reserved before `HostContext::push_resource`. A full resource
/// table refuses that insert after the reservation. The reserved node is then
/// rolled back, so live occupancy and existing roots return to their previous
/// state and the slot can be reused at the next generation.
pub fn publish(host: &mut HostContext) -> Result<Value, HeapError> {
    publish_table(host).map(|(_, raw)| Value::Int(raw))
}

/// Allocates a table, publishes its token, and issues the guest array.
///
/// The returned array is the only guest table value for that slot. The token
/// stays on the existing host boundary and is not the guest identity.
pub fn publish_guest(host: &mut HostContext) -> Result<Value, HeapError> {
    let (table, raw) = publish_table(host)?;
    match with_heap(host, |heap| heap.issue_carrier(table)) {
        Ok(value) => Ok(value),
        Err(error) => {
            let raw = u64::try_from(raw).map_err(|_| HeapError::CarrierOutOfRange)?;
            let handle =
                vm::ResourceHandle::from_raw(raw).map_err(|_| HeapError::CarrierOutOfRange)?;
            host.close_resource::<LuaTableResource>(handle, vm::ResourceCloseReason::Requested)
                .map_err(HeapError::from_scope)?;
            with_heap(host, |heap| heap.rollback(table))?;
            Err(error)
        }
    }
}

pub(super) fn publish_table(host: &mut HostContext) -> Result<(TableRef, i64), HeapError> {
    let table = with_heap(host, LuaHeap::alloc)?;
    let resource = match host.push_resource(LuaTableResource::new(table)) {
        Ok(resource) => resource,
        Err(error) => {
            with_heap(host, |heap| heap.rollback(table))?;
            return Err(HeapError::from_scope(error));
        }
    };
    let raw_u64 = resource.handle().raw();
    if let Err(error) = with_heap(host, |heap| heap.bind_token(table, raw_u64)) {
        host.close_resource::<LuaTableResource>(
            resource.handle(),
            vm::ResourceCloseReason::Requested,
        )
        .map_err(HeapError::from_scope)?;
        with_heap(host, |heap| heap.rollback(table))?;
        return Err(error);
    }
    let raw = match i64::try_from(raw_u64) {
        Ok(raw) => raw,
        Err(_) => {
            host.close_resource::<LuaTableResource>(
                resource.handle(),
                vm::ResourceCloseReason::Requested,
            )
            .map_err(HeapError::from_scope)?;
            with_heap(host, |heap| heap.rollback(table))?;
            return Err(HeapError::CarrierOutOfRange);
        }
    };
    Ok((table, raw))
}

/// Allocates one bare userdata and publishes its private typed resource token.
///
/// The Lua slot is reserved with an exact reservation record before
/// `HostContext::push_resource`. A refused insert rolls back only that empty
/// reservation in O(1), so a Lua quota or phase refusal never registers a
/// Core resource and never scans the graph. After registration this stores
/// the raw handle on the reservation (not in the published slot binding),
/// borrows the token with the exact `lua.userdata` key, and confirms that its
/// payload names this exact userdata (domain, kind, slot, and generation)
/// before it publishes the slot binding. The returned handle is published
/// only after both directions of that binding exist.
///
/// A later failure closes the registered token through `close_resource`,
/// requires [`vm::resource::CloseProgress::Ready`], and confirms the handle is
/// retired before it rolls the Lua slot back. If that close is not confirmed,
/// the reservation stays with its staged raw handle and its generation is not
/// advanced. A normal collection retains that exact reservation: the shared
/// mark offers it as a host-retained edge and the pending-close enumeration
/// ignores the unpublished token. The next call to this function recovers it
/// before reserving again: the typed payload must name the exact userdata,
/// the same token is closed synchronously and confirmed retired, and only
/// then is the record cleared and the slot generation returned. A failed
/// recovery keeps the record for a later attempt. An ordinary borrow never
/// accepts the staged record.
/// `LuaUserdataResource` has no asynchronous close path: the default
/// synchronous close returns `Ready`, and `Pending` is an error here.
pub fn publish_userdata(host: &mut HostContext) -> Result<UserdataRef, HeapError> {
    recover_pending_userdata(host)?;
    let userdata = with_heap(host, LuaHeap::reserve_userdata)?;
    let resource = match host.push_resource(LuaUserdataResource::new(userdata)) {
        Ok(resource) => resource,
        Err(error) => {
            with_heap(host, |heap| heap.rollback_empty_userdata(userdata))?;
            return Err(HeapError::from_scope(error));
        }
    };
    let handle = resource.handle();
    let raw = handle.raw();
    if let Err(error) = with_heap(host, |heap| heap.stage_userdata_token(userdata, raw)) {
        return rollback_registered_userdata(host, userdata, handle, error);
    }
    let payload = match checked_userdata_payload(host, handle) {
        Ok(payload) => payload,
        Err(error) => {
            return rollback_registered_userdata(host, userdata, handle, error);
        }
    };
    if payload != userdata {
        return rollback_registered_userdata(host, userdata, handle, HeapError::TokenMismatch);
    }
    if let Err(error) = with_heap(host, |heap| heap.bind_userdata_token(userdata, raw)) {
        return rollback_registered_userdata(host, userdata, handle, error);
    }
    Ok(userdata)
}

/// Recovers a reservation left by a refused registered rollback before the
/// next issuance reserves again.
///
/// The record and its live slot are validated first. For a registered
/// reservation, the typed payload must name the exact userdata, then that same
/// token is closed and confirmed retired before the record is cleared and the
/// slot generation returned. A close failure or owner mismatch keeps the
/// record, so a later call can retry it. An unregistered empty reservation has
/// no Core token: it is rolled back in O(1) without a close. No record is a
/// no-op. The whole recovery is one record check, one exact typed borrow, one
/// synchronous close, and one O(1) rollback: it adds no scan unit, no graph
/// search, and no owner walk.
fn recover_pending_userdata(host: &mut HostContext) -> Result<(), HeapError> {
    let Some((userdata, raw)) = with_heap(host, |heap| heap.pending_userdata_reservation())? else {
        return Ok(());
    };
    if let Some(raw) = raw {
        let handle = vm::ResourceHandle::from_raw(raw).map_err(|_| HeapError::CarrierOutOfRange)?;
        let owner = checked_userdata_payload(host, handle)?;
        if owner != userdata {
            return Err(HeapError::TokenMismatch);
        }
        close_confirmed_token::<LuaUserdataResource>(host, handle)?;
    }
    with_heap(host, |heap| heap.rollback_empty_userdata(userdata))?;
    Ok(())
}

/// Borrows the typed resource token recorded for `userdata`.
///
/// The Lua handle is checked against the installed heap domain, live slot,
/// generation, and kind. The stored raw handle is then validated by the Core
/// resource table (arena, slot generation, concrete `TypeId`, and the exact
/// `lua.userdata` key). Finally the payload must name this same userdata, so a
/// same-type token for another object is refused even though its key matches.
/// A missing heap is [`HeapError::SweepDenied`]; a foreign Lua heap is
/// [`HeapError::WrongDomain`]; a stale or unbound Lua slot is
/// [`HeapError::StaleHandle`]; a payload for another object is
/// [`HeapError::TokenMismatch`].
pub fn borrow_published_userdata(
    host: &HostContext,
    userdata: UserdataRef,
) -> Result<UserdataRef, HeapError> {
    let Some(heap) = host.scope_state::<LuaHeap>() else {
        return Err(HeapError::SweepDenied);
    };
    if userdata.domain() != heap.domain() {
        return Err(HeapError::WrongDomain);
    }
    let raw = heap.published_userdata_token(userdata)?;
    let handle = vm::ResourceHandle::from_raw(raw).map_err(|_| HeapError::CarrierOutOfRange)?;
    let borrowed = host
        .borrow_resource_with_key::<LuaUserdataResource>(handle, &LuaUserdataResource::key())
        .map_err(HeapError::from_scope)?;
    if borrowed.get().userdata() != userdata {
        return Err(HeapError::TokenMismatch);
    }
    Ok(userdata)
}

/// Borrows one registered userdata token with the exact key and copies its
/// payload out. The borrow ends here, so a caller can close or roll back the
/// same `HostContext` on the error path.
fn checked_userdata_payload(
    host: &HostContext,
    handle: vm::ResourceHandle,
) -> Result<UserdataRef, HeapError> {
    let borrowed = host
        .borrow_resource_with_key::<LuaUserdataResource>(handle, &LuaUserdataResource::key())
        .map_err(HeapError::from_scope)?;
    Ok(borrowed.get().userdata())
}

/// Closes one registered userdata token and confirms that a second close sees
/// it retired. If the close is not confirmed, the reservation keeps its staged
/// raw handle and its generation, so the private retry can close the same
/// token. A controlled test refusal returns before any close is attempted.
fn rollback_registered_userdata(
    host: &mut HostContext,
    userdata: UserdataRef,
    handle: vm::ResourceHandle,
    error: HeapError,
) -> Result<UserdataRef, HeapError> {
    #[cfg(test)]
    if take_userdata_rollback_refusal(host) {
        return Err(HeapError::Scope(
            "injected userdata rollback refusal before close".to_string(),
        ));
    }
    close_confirmed_token::<LuaUserdataResource>(host, handle)?;
    with_heap(host, |heap| heap.rollback_empty_userdata(userdata))?;
    Err(error)
}

/// One-shot test hook consumed by the real [`rollback_registered_userdata`]
/// control flow. It refuses that rollback before the first close. It is not a
/// `Pending` close and never fabricates a Core progress value.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
struct UserdataRollbackRefusal {
    armed: bool,
}

/// Arms the one-shot refusal for the next registered userdata rollback.
#[cfg(test)]
pub fn arm_userdata_rollback_refusal(host: &mut HostContext) -> Result<(), HeapError> {
    if let Some(stored) = host.scope_state_mut::<UserdataRollbackRefusal>() {
        stored.armed = true;
        return Ok(());
    }
    host.scope_state_or_insert_with(|| UserdataRollbackRefusal { armed: true })
        .map(|_| ())
        .map_err(HeapError::from_scope)
}

#[cfg(test)]
fn take_userdata_rollback_refusal(host: &mut HostContext) -> bool {
    let Some(stored) = host.scope_state_mut::<UserdataRollbackRefusal>() else {
        return false;
    };
    let armed = stored.armed;
    stored.armed = false;
    armed
}

/// Reads the staged (unpublished) raw handle for one exact reservation.
/// Test-only: an ordinary borrow never accepts this record.
#[cfg(test)]
pub fn pending_userdata_raw(
    host: &mut HostContext,
    userdata: UserdataRef,
) -> Result<Option<u64>, HeapError> {
    with_heap(host, |heap| Ok(heap.pending_userdata_token(userdata).ok()))
}

/// Retries one registered userdata rollback after a refused attempt. It reads
/// the staged raw handle and closes that same Core token, then releases the
/// empty reservation. Test-only immediate entry: the production recovery is
/// the next [`publish_userdata`] call, which validates the exact owner and
/// performs the same close and generation return. Normal collections retain
/// the record this entry consumes.
#[cfg(test)]
pub fn retry_pending_userdata_rollback(
    host: &mut HostContext,
    userdata: UserdataRef,
) -> Result<u32, HeapError> {
    let raw = with_heap(host, |heap| heap.pending_userdata_token(userdata))?;
    let handle = vm::ResourceHandle::from_raw(raw).map_err(|_| HeapError::CarrierOutOfRange)?;
    close_confirmed_token::<LuaUserdataResource>(host, handle)?;
    with_heap(host, |heap| heap.rollback_empty_userdata(userdata))
}

/// Closes one typed token, requires the synchronous result, and confirms that
/// the handle is retired. A Pending close is an error: these types have no
/// asynchronous close path.
fn close_confirmed_token<T: vm::resource::HostResource>(
    host: &mut HostContext,
    handle: vm::ResourceHandle,
) -> Result<(), HeapError> {
    match host.close_resource::<T>(handle, vm::ResourceCloseReason::Requested) {
        Ok(vm::resource::CloseProgress::Ready) => {}
        Ok(_) => {
            return Err(HeapError::Scope(
                "lua typed resource close did not finish synchronously".to_string(),
            ));
        }
        Err(error) => return Err(HeapError::from_scope(error)),
    }
    match host.close_resource::<T>(handle, vm::ResourceCloseReason::Requested) {
        Err(error) if retired_token(&error) => Ok(()),
        Ok(_) => Err(HeapError::Scope(
            "lua typed resource close did not stay closed".to_string(),
        )),
        Err(error) => Err(HeapError::from_scope(error)),
    }
}

/// One controlled userdata failure after registration, for the private test
/// injection. It records the real Core handle, the resource count on both
/// sides, and whether the real rollback completed or retained the staged raw.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UserdataPostRegisterFailure {
    /// The reserved Lua handle, still live and unpublished on the refusal path.
    pub userdata: UserdataRef,
    pub raw: u64,
    pub registered_count: usize,
    pub ended_count: usize,
    /// `Some(Ready)` only when this call actually completed the synchronous
    /// close. `None` means the one-shot refusal returned before any close:
    /// this witness never reports a fabricated `Pending`.
    pub close: Option<vm::resource::CloseProgress>,
    /// The staged raw retained for a retry when the rollback was refused.
    pub pending_raw: Option<u64>,
    pub slot: u32,
    /// Generation after the call: advanced only when the slot was released.
    pub generation: u32,
}

/// The witness from the last [`publish_userdata_after_register_failure`] call.
#[cfg(test)]
pub fn last_userdata_post_register_failure(
    host: &HostContext,
) -> Option<UserdataPostRegisterFailure> {
    host.scope_state::<UserdataPostRegisterFailure>().copied()
}

/// Test-only injection: registers a real userdata token, stages its raw, then
/// fails before the slot binding is published and runs the real
/// [`rollback_registered_userdata`] control flow. With no armed refusal it
/// closes and confirms that same token and rolls the slot back. With the
/// one-shot refusal armed it returns before the close and keeps the staged
/// raw. A normal collection then retains that exact registration, and the
/// next [`publish_userdata`] call recovers it through the production path.
/// It never fabricates a handle and never skips the real rollback.
#[cfg(test)]
pub fn publish_userdata_after_register_failure(
    host: &mut HostContext,
) -> Result<UserdataRef, HeapError> {
    let userdata = with_heap(host, LuaHeap::reserve_userdata)?;
    let resource = match host.push_resource(LuaUserdataResource::new(userdata)) {
        Ok(resource) => resource,
        Err(error) => {
            with_heap(host, |heap| heap.rollback_empty_userdata(userdata))?;
            return Err(HeapError::from_scope(error));
        }
    };
    let handle = resource.handle();
    let raw = handle.raw();
    with_heap(host, |heap| heap.stage_userdata_token(userdata, raw))?;
    let registered_count = host.resource_count();
    let outcome = rollback_registered_userdata(
        host,
        userdata,
        handle,
        HeapError::Scope("injected userdata failure after registration".to_string()),
    );
    let ended_count = host.resource_count();
    let pending_raw = pending_userdata_raw(host, userdata)?;
    let generation = with_heap(host, |heap| {
        Ok(heap.slot_generation(userdata.slot()).unwrap_or(0))
    })?;
    let close = (pending_raw.is_none() && ended_count + 1 == registered_count)
        .then_some(vm::resource::CloseProgress::Ready);
    let witness = UserdataPostRegisterFailure {
        userdata,
        raw,
        registered_count,
        ended_count,
        close,
        pending_raw,
        slot: userdata.slot(),
        generation,
    };
    if let Some(stored) = host.scope_state_mut::<UserdataPostRegisterFailure>() {
        *stored = witness;
    } else {
        host.scope_state_or_insert_with(|| witness)
            .map_err(HeapError::from_scope)?;
    }
    outcome?;
    unreachable!("rollback_registered_userdata always returns its failure")
}

/// Runs `body` against the scope heap, creating it with production limits if
/// this scope has not been installed yet.
pub fn with_heap<T>(
    host: &mut HostContext,
    body: impl FnOnce(&mut LuaHeap) -> Result<T, HeapError>,
) -> Result<T, HeapError> {
    let heap = host
        .scope_state_or_insert_with(|| LuaHeap::new(HeapLimits::production()))
        .map_err(HeapError::from_scope)?;
    body(heap)
}

/// Host-side collection. One mark lists unreachable tokens and keeps the
/// parsed weak modes for sweep. Each token is closed through
/// `HostContext::close_resource` before the heap sweep, so the scope quota
/// comes back in this same VM. Stored handles are not roots.
///
/// A close error returns before sweep. That snapshot is dropped with the
/// mark result, and the next collect marks again. Close checks the slot and
/// generation captured at mark time. It does not
/// search the slot vector. For that exact stored token only,
/// `ResourceAlreadyClosed` (slot not reused) and `ResourceStale` (slot reused)
/// are retired: the identified slot is cleared and the sweep continues. A
/// successor in a reused core slot is not closed, and a different lua slot is
/// not cleared. Any other structured error, including a wrong table or a type
/// mismatch, returns immediately. Tokens already cleared stay cleared. Tokens
/// not yet closed stay open, including every token after the failing one.
/// Sweep does not run, so those nodes stay allocated and the scope quota for
/// the unclosed suffix is not returned. A later collection marks again from
/// the current roots. It does not resume after the failing token. If that
/// token is still bound, the later collection can fail again at the same
/// obstacle, and it does not close the suffix or return the prefix quota a
/// second time. Removing the obstacle is outside this collector.
///
/// `LuaTableResource` and `LuaUserdataResource` use the default synchronous
/// close, which returns Ready. A Pending result is an error. These types have
/// no path that starts an asynchronous close, and this collector does not add
/// one. On error the already closed prefix is real: its bindings stay cleared,
/// but the failing token and the whole unclosed suffix keep their bindings,
/// no weak edge is published, no release tail is saved, and no generation is
/// advanced.
pub fn collect(host: &mut HostContext) -> Result<table::CollectReport, HeapError> {
    if host
        .scope_state::<LuaHeap>()
        .is_some_and(LuaHeap::has_sweep_tail)
    {
        return Err(HeapError::SweepPending);
    }
    let marked = with_heap(host, |heap| Ok(heap.mark_and_doomed_tokens()))?;
    for token in marked.tokens {
        retire_doomed_token(host, token)?;
    }
    with_heap(host, |heap| Ok(heap.sweep_marked(marked.modes)))
}

/// Publish stopped before release, or the heap had no weak edge to publish.
#[derive(Debug)]
pub(crate) enum PublishedBoundary {
    /// Neither a dead slot, nor a retained pending finalizer, nor a live
    /// settled userdata needs a weak publication. Sweep finished and
    /// recorded the one report.
    Completed(table::CollectReport),
    /// Weak edges are published. Dead slots are still physical. No report.
    /// `window_heads`/`window_edges` are the actual Owners walk, which can
    /// be non-empty with no dead slot when a pending finalizer's or a settled
    /// userdata's weak value is cleared.
    Published {
        window_heads: u64,
        window_edges: u64,
    },
}

/// A saved published sweep that this call did not finish, or the one report
/// from the tail that did finish.
#[derive(Debug)]
pub(crate) enum PublishedResume {
    /// The tail is still saved. This call did not finish the sweep.
    Paused,
    /// The tail finished in this call. The report was recorded once.
    Completed(table::CollectReport),
}

/// Marks, closes doomed tokens, publishes weak edges, and saves the release tail.
///
/// Does not take an observer. A parked mark is refused and left in place.
/// Close failure publishes nothing and saves no tail.
pub(crate) fn collect_until_published(
    host: &mut HostContext,
) -> Result<PublishedBoundary, HeapError> {
    let Some(heap) = host.scope_state::<LuaHeap>() else {
        return Err(HeapError::SweepDenied);
    };
    if heap.has_sweep_tail() {
        return Err(HeapError::SweepPending);
    }
    if heap.has_saved_mark() {
        return Err(HeapError::MarkActive);
    }
    let marked = with_heap(host, |heap| Ok(heap.mark_and_doomed_tokens()))?;
    close_then_halt_at_publish(host, marked, false)
}

/// Closes doomed tokens, then publishes or finishes. Same order as the
/// original collector: a close error returns before publish, so there is no
/// tail and no report.
///
/// `settle_rollback` is true only for the selecting entry. On a close error
/// that pass's new settlement flags are returned to their previous state, just
/// like its new pending records are handled by the caller; an already closed
/// prefix stays closed and a pending record from an earlier pass is untouched.
fn close_then_halt_at_publish(
    host: &mut HostContext,
    marked: table::DoomedMark,
    settle_rollback: bool,
) -> Result<PublishedBoundary, HeapError> {
    for token in marked.tokens {
        if let Err(error) = retire_doomed_token(host, token) {
            if settle_rollback {
                with_heap(host, |heap| {
                    heap.discard_settlements();
                    Ok(())
                })?;
            }
            return Err(error);
        }
    }
    let halt = with_heap(host, |heap| Ok(heap.halt_at_publish(marked.modes)))?;
    Ok(match halt {
        table::PublishHalt::Completed(report) => PublishedBoundary::Completed(report),
        table::PublishHalt::Published { heads, edges } => PublishedBoundary::Published {
            window_heads: heads,
            window_edges: edges,
        },
    })
}

/// Private single-shot collector for the pending-finalizer slice.
///
/// Phases and boundaries, in order:
/// 1. Refuse a saved mark or saved release tail. No heap installs one.
/// 2. Run one ordinary non-moving mark with the same roots, registered
///    reservation offer, and pending-record offers. A parked mark is dropped,
///    as in [`collect`].
/// 3. After the ordinary gray, rescan, and late work empties, scan every slot
///    once. Select each live, unmarked userdata that binds a typed Core token
///    and whose metatable has a raw non-nil `__gc` field. Mark it through the
///    ordinary target follow; the next gray pass follows its metatable edge
///    and the metatable's own strong entries. A raw field that is missing,
///    nil, or already a weak-cleared cursor gap settles that exact white
///    identity instead: the generation is finalized without a queue record
///    and without a mark. A slot already pending or settled is not selected
///    again. Candidates that share a metatable pay one lookup for that
///    metatable per round, but each one still receives its own queue or
///    settlement result.
/// 4. Enumerate pending-close candidates as usual. A selected userdata is
///    marked, so it is not in that list and its token stays bound. A settled
///    userdata is not marked by the settlement, so an unreachable one is
///    ordinary dead garbage in this same cycle.
/// 5. Borrow every pending record with its exact `lua.userdata` token and
///    require the payload to name the same slot and generation. A mismatch
///    returns before any close or publish and rolls back this pass's new
///    records and settlement flags.
/// 6. Close each doomed token synchronously and confirm `Ready`. A close
///    error also rolls back this pass's new settlement flags; an already
///    closed prefix and earlier pending records stay as they were.
/// 7. Run the Owners walk once. Weak values that name a retained pending
///    userdata or a live settled generation are published as gaps; weak keys
///    keep the entry and its scalar only when the object is marked. A dead
///    slot is not required for this step. Store the Release/Roots tail and
///    return [`PublishedBoundary::Published`]; a heap with no dead slot, no
///    pending finalizer, and no live settled userdata finishes and returns
///    [`PublishedBoundary::Completed`].
///
/// Release and Roots do not run here. The caller finishes them with
/// [`resume_published_sweep`]. No callback, resurrection, guest step, or
/// production entry is added. The records are retained by later ordinary
/// collections until the test-only explicit retirement control consumes one.
pub(crate) fn collect_pending_finalizers(
    host: &mut HostContext,
) -> Result<PublishedBoundary, HeapError> {
    let Some(heap) = host.scope_state::<LuaHeap>() else {
        return Err(HeapError::SweepDenied);
    };
    if heap.has_sweep_tail() {
        return Err(HeapError::SweepPending);
    }
    if heap.has_saved_mark() {
        return Err(HeapError::MarkActive);
    }
    let base = with_heap(host, |heap| Ok(heap.pending_finalizer_count()))?;
    let marked = with_heap(host, |heap| Ok(heap.mark_and_pending_tokens()))?;
    if let Err(error) = verify_pending_finalizers(host) {
        // This is the owner-verification preflight's own transaction: remove
        // only the records and settlement flags appended by this selecting
        // pass. Earlier valid records and settled generations are untouched,
        // no doomed token has been closed yet, and no weak edge has been
        // published, so the two later failure prefixes are not mixed into
        // this rollback.
        with_heap(host, |heap| {
            heap.discard_pending_finalizers_from(base);
            heap.discard_settlements();
            Ok(())
        })?;
        return Err(error);
    }
    let boundary = close_then_halt_at_publish(host, marked, true)?;
    with_heap(host, |heap| {
        // The real weak publication finished in this call. Only now do the
        // retained records become a consumable prefix. A saved Release tail
        // still refuses a dequeue until the caller resumes it to completion.
        heap.publish_pending_finalizer_prefix();
        heap.clear_selecting_settlements();
        Ok(())
    })?;
    Ok(boundary)
}

/// Borrows each pending record with its concrete `lua.userdata` key and
/// checks that the payload names the same slot and generation.
///
/// The borrow ends before the next record. This runs after the selecting mark
/// and before any doomed token closes or any weak edge is published. A
/// missing or wrong token returns [`HeapError::TokenMismatch`] or the real
/// scope error; no record is mutated. The caller then discards only the
/// records appended by this pass.
fn verify_pending_finalizers(host: &HostContext) -> Result<(), HeapError> {
    let Some(heap) = host.scope_state::<LuaHeap>() else {
        return Err(HeapError::SweepDenied);
    };
    let records = heap.pending_finalizer_records();
    for (raw, slot, generation) in records {
        let handle = vm::ResourceHandle::from_raw(raw).map_err(|_| HeapError::CarrierOutOfRange)?;
        let borrowed = host
            .borrow_resource_with_key::<LuaUserdataResource>(handle, &LuaUserdataResource::key())
            .map_err(HeapError::from_scope)?;
        let payload = borrowed.get().userdata();
        if payload.domain() != heap.domain()
            || payload.slot() != slot
            || payload.generation() != generation
            || payload.kind() != HandleKind::Userdata
        {
            return Err(HeapError::TokenMismatch);
        }
    }
    Ok(())
}

/// Test-only explicit retirement control for one pending finalizer.
///
/// This is the current consumer stand-in for a later real finalizer call: it
/// removes the record, clears the pending flag, and marks that generation as
/// settled without closing the token or running guest code. Selection will
/// not pick the settled object again; a later collection may then close that
/// token and advance the slot generation. It is not a callback and does not
/// fabricate a close status. It also keeps the queue's published-prefix
/// invariant: removing a published position drops the ready prefix by one,
/// while removing an unpublished position leaves it unchanged.
#[cfg(test)]
pub fn retire_pending_finalizer_test_control(
    host: &mut HostContext,
    userdata: UserdataRef,
) -> Result<(), HeapError> {
    with_heap(host, |heap| heap.retire_pending_finalizer(userdata))
}

/// Private real handoff for one published pending-finalizer head.
///
/// This is not a guest `__gc` callback and does not run guest code. It refuses
/// a saved mark ([`HeapError::MarkActive`]) or release tail
/// ([`HeapError::SweepPending`]) before reading the queue. The head must be in
/// the prefix that one successful private publication made consumable; an
/// unpublished or absent head returns a scope error. The specified frame and
/// slot must name an open, previously empty root slot
/// ([`HeapError::StaleFrame`] or a scope error for an occupied slot). The
/// record's live slot must still bind its exact generation, kind, pending
/// flag, and raw handle, and the typed `lua.userdata` payload must name that
/// same userdata ([`HeapError::TokenMismatch`] otherwise). All of those checks
/// happen before the first queue, root, or slot write.
///
/// On success the head is popped with one `VecDeque` front removal, the exact
/// `UserdataRef` is written into the specified root slot, and that generation
/// is marked settled and no longer pending. The returned handle is the same
/// typed object the queue verified; the raw Core token is not returned or
/// copied into a guest value. The order is only the selecting scan's offered
/// order, not LuaJIT's reverse allocation or callback order.
pub(crate) fn take_published_pending_finalizer(
    host: &mut HostContext,
    frame: RootFrameId,
    slot: u32,
) -> Result<UserdataRef, HeapError> {
    let Some(heap) = host.scope_state::<LuaHeap>() else {
        return Err(HeapError::SweepDenied);
    };
    if heap.has_sweep_tail() {
        return Err(HeapError::SweepPending);
    }
    if heap.has_saved_mark() {
        return Err(HeapError::MarkActive);
    }
    let head = heap.pending_finalizer_published_front()?;
    heap.pending_finalizer_root_slot(frame, slot)?;
    let userdata = UserdataRef::userdata(heap.domain(), head.slot, head.generation);
    if heap.published_userdata_token(userdata)? != head.raw {
        return Err(HeapError::TokenMismatch);
    }
    let handle =
        vm::ResourceHandle::from_raw(head.raw).map_err(|_| HeapError::CarrierOutOfRange)?;
    let borrowed = host
        .borrow_resource_with_key::<LuaUserdataResource>(handle, &LuaUserdataResource::key())
        .map_err(HeapError::from_scope)?;
    if borrowed.get().userdata() != userdata {
        return Err(HeapError::TokenMismatch);
    }
    drop(borrowed);
    with_heap(host, |heap| {
        heap.take_published_finalizer_into_root(frame, slot, head)
    })
}

/// The current raw finalizer decision for one published pending head.
///
/// This is a private dispatch plan, not a guest callable. It is recomputed at
/// dequeue time from the userdata's current metatable, so a modifier that ran
/// earlier in the same batch is observed. A `Closure`, the `call` inside
/// `TableCall`, or the `call` inside `UserdataCall` is an actual closure
/// identity; a receiver form also keeps the original object instead of
/// replacing it with the callable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PendingFinalizerAction {
    /// No live metatable, no raw `__gc` record, or a true stored-nil gap.
    Skip,
    /// A present raw `__gc` value that is not callable in this slice's
    /// one-layer raw form.
    NonCallable(PendingFinalizerNotCallable),
    /// The raw `__gc` is a live closure.
    Closure(ClosureRef),
    /// The raw `__gc` is a live table whose own current metatable stores a raw
    /// `__call` closure. The receiver is that original table.
    TableCall {
        receiver: TableRef,
        call: ClosureRef,
    },
    /// The raw `__gc` is a live userdata whose own current metatable stores a
    /// raw `__call` closure. The receiver is that original userdata.
    UserdataCall {
        receiver: UserdataRef,
        call: ClosureRef,
    },
}

/// The original Lua type tag of a present raw `__gc` value that is not
/// callable in this slice.
///
/// These tags carry no scalar payload: a boolean, number, or string keeps only
/// its type, never its bytes. A table or userdata reason keeps the exact
/// original receiver identity, so a later diagnostic can name the original
/// called type instead of the invalid `__call` value. This slice emits no
/// diagnostic text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PendingFinalizerNotCallable {
    /// The original value was a boolean.
    Boolean,
    /// The original value was a number (`Int` or `Float`).
    Number,
    /// The original value was a Lua string (`String` or byte string).
    String,
    /// An internal upvalue carrier from the existing `EdgeValue` contract. No
    /// named-field writer stores it; it is not a guest Lua value.
    Upvalue,
    /// A live userdata receiver with no live own metatable or no raw `__call`
    /// field.
    UserdataWithoutCall(UserdataRef),
    /// A live userdata receiver whose raw `__call` exists but is not a live
    /// closure. Another table is refused here: this slice does not dispatch
    /// twice.
    UserdataCallNotClosure(UserdataRef),
    /// A table receiver with no live metatable or no raw `__call` field.
    TableWithoutCall(TableRef),
    /// A table receiver whose raw `__call` exists but is not a live closure.
    /// Another table is refused here: this slice does not dispatch twice.
    TableCallNotClosure(TableRef),
    /// A stored object target no longer names a live node of its kind, or a
    /// scalar carrier outside the named-field writer contract.
    Detached,
}

/// The no-allocation reject tag for one stored scalar, or `None` for a stored
/// nil.
///
/// The private named-field writers can store a boolean or a Lua string
/// directly under `__gc`; a binary64 number remains limited to the reserved
/// `__mode` field and a byte string has no named-field writer. This helper is
/// therefore the narrowest complete classification: it maps `Int`/`Float` to
/// [`PendingFinalizerNotCallable::Number`] and `String`/`Bytes` to
/// [`PendingFinalizerNotCallable::String`] without copying either payload.
/// `Array`, `Map`, and `Callable` are outside this heap's named-field scalar
/// contract and are reported as `Detached`, not as a new guest tag.
pub(crate) fn pending_finalizer_scalar_reject(
    value: &Value,
) -> Option<PendingFinalizerNotCallable> {
    match value {
        Value::Null => None,
        Value::Bool(_) => Some(PendingFinalizerNotCallable::Boolean),
        Value::Int(_) | Value::Float(_) => Some(PendingFinalizerNotCallable::Number),
        Value::String(_) | Value::Bytes(_) => Some(PendingFinalizerNotCallable::String),
        Value::Array(_) | Value::Map(_) | Value::Callable(_) => {
            Some(PendingFinalizerNotCallable::Detached)
        }
    }
}

/// One committed private action handoff.
///
/// The identities are already rooted in the caller's frame. No raw Core token
/// or guest callable is copied out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TakenPendingFinalizer {
    /// The exact typed userdata that left the published prefix.
    pub userdata: UserdataRef,
    /// The current raw decision that this same commit rooted.
    pub action: PendingFinalizerAction,
    /// Root slots written from the caller's plan: one for a skip or typed
    /// reject, two for a direct closure or a self-receiver userdata call, and
    /// three for a table call or an independent userdata receiver.
    pub roots_written: u32,
}

/// Private real dequeue that re-reads the current raw finalizer of the head.
///
/// This is not a guest `__gc` callback and does not run guest code. It refuses
/// a saved mark ([`HeapError::MarkActive`]) or release tail
/// ([`HeapError::SweepPending`]) before reading the queue. The head must be in
/// the prefix that one successful private publication made consumable; an
/// unpublished or absent head returns a scope error. The record's live slot
/// must still bind its exact generation, kind, pending flag, and raw handle,
/// and the typed `lua.userdata` payload must name that same userdata
/// ([`HeapError::TokenMismatch`] otherwise). Every one of those checks happens
/// before the first queue, root, or slot write.
///
/// The current raw `__gc` is resolved after the head checks. When it is a live
/// userdata receiver different from the head, that receiver must still own a
/// real typed `lua.userdata` token naming it (the same checked owner contract
/// as the head); a payload for another object is [`HeapError::TokenMismatch`].
/// The commit then re-resolves the action in the same exclusive borrow that
/// pops the head and requires the exact prepared identity, so no fallible
/// action or owner check remains after the pop. `slots[0]` must be an open,
/// empty root slot for the userdata. A direct closure adds an empty distinct
/// `slots[1]`; the table form and an independent userdata receiver add the
/// closure at `slots[1]` and the original receiver at an empty distinct
/// `slots[2]`. A userdata receiver that is the pending object itself is
/// already rooted at `slots[0]`, so its call needs only two positions. A short
/// plan, an overlapping plan, a stale frame, an occupied slot, or an
/// out-of-range position is refused before any write, so the head and every
/// existing root stay. A `Skip` or typed reject needs only `slots[0]`, is
/// returned as its typed result, and is settled exactly once; it is never
/// re-queued. The commit pops one `VecDeque` front record, writes the required
/// roots together, and settles that generation. The returned action and rooted
/// identities are private; the raw Core token is not copied into a guest
/// value.
pub(crate) fn take_published_pending_finalizer_action(
    host: &mut HostContext,
    frame: RootFrameId,
    slots: &[u32],
) -> Result<TakenPendingFinalizer, HeapError> {
    let Some(heap) = host.scope_state::<LuaHeap>() else {
        return Err(HeapError::SweepDenied);
    };
    if heap.has_sweep_tail() {
        return Err(HeapError::SweepPending);
    }
    if heap.has_saved_mark() {
        return Err(HeapError::MarkActive);
    }
    let head = heap.pending_finalizer_published_front()?;
    let userdata = UserdataRef::userdata(heap.domain(), head.slot, head.generation);
    if heap.published_userdata_token(userdata)? != head.raw {
        return Err(HeapError::TokenMismatch);
    }
    let handle =
        vm::ResourceHandle::from_raw(head.raw).map_err(|_| HeapError::CarrierOutOfRange)?;
    let borrowed = host
        .borrow_resource_with_key::<LuaUserdataResource>(handle, &LuaUserdataResource::key())
        .map_err(HeapError::from_scope)?;
    if borrowed.get().userdata() != userdata {
        return Err(HeapError::TokenMismatch);
    }
    drop(borrowed);
    // One fixed-size prepared action: the independent receiver's typed owner is
    // checked before the commit borrow. The commit re-resolves the same action
    // in its own exclusive borrow and refuses any identity change before the
    // pop, so the root set always matches the checked receiver.
    let action = with_heap(host, |heap| Ok(heap.pending_finalizer_action(userdata)))?;
    verify_pending_finalizer_receiver(host, userdata, action)?;
    with_heap(host, |heap| {
        heap.take_published_finalizer_action_into_roots(frame, slots, head, action)
    })
}

/// Checks that one independent userdata receiver still owns a real typed
/// `lua.userdata` token naming that same object.
///
/// A receiver that is the pending object itself is already covered by the head
/// payload check and is not borrowed twice; the commit roots it at `slots[0]`.
/// This is one slot token read and one checked Core borrow with a payload
/// identity comparison. It reads no queue record and writes nothing.
fn verify_pending_finalizer_receiver(
    host: &HostContext,
    userdata: UserdataRef,
    action: PendingFinalizerAction,
) -> Result<(), HeapError> {
    let PendingFinalizerAction::UserdataCall { receiver, .. } = action else {
        return Ok(());
    };
    if receiver == userdata {
        return Ok(());
    }
    let Some(heap) = host.scope_state::<LuaHeap>() else {
        return Err(HeapError::SweepDenied);
    };
    let raw = heap.published_userdata_token(receiver)?;
    let handle = vm::ResourceHandle::from_raw(raw).map_err(|_| HeapError::CarrierOutOfRange)?;
    let borrowed = host
        .borrow_resource_with_key::<LuaUserdataResource>(handle, &LuaUserdataResource::key())
        .map_err(HeapError::from_scope)?;
    if borrowed.get().userdata() != receiver {
        return Err(HeapError::TokenMismatch);
    }
    Ok(())
}

/// Number of pending records in the published prefix. Test-only header read;
/// it does not read a stored record.
#[cfg(test)]
pub fn pending_finalizer_published_count(host: &mut HostContext) -> Result<usize, HeapError> {
    with_heap(host, |heap| Ok(heap.pending_finalizer_published_count()))
}

/// One userdata root slot. Test-only; does not scan or read the queue.
#[cfg(test)]
pub fn rooted_userdata(
    host: &mut HostContext,
    frame: RootFrameId,
    slot: u32,
) -> Result<Option<UserdataRef>, HeapError> {
    with_heap(host, |heap| heap.rooted_userdata(frame, slot))
}

/// One closure root slot. Test-only; does not scan or read the queue.
#[cfg(test)]
pub fn rooted_closure(
    host: &mut HostContext,
    frame: RootFrameId,
    slot: u32,
) -> Result<Option<ClosureRef>, HeapError> {
    with_heap(host, |heap| heap.rooted_closure(frame, slot))
}

/// Saves one empty private mark for a handoff refusal test. Test-only: the
/// real handoff must refuse a saved mark before it reads the queue. It does
/// not scan, close, publish, or run guest code.
#[cfg(test)]
pub fn open_saved_mark_for_test(host: &mut HostContext) -> Result<(), HeapError> {
    with_heap(host, |heap| heap.open_private_step())
}

/// Continues a published sweep this scope already saved. Does not install a
/// heap and does not start a sweep. No saved tail is [`HeapError::SweepDenied`].
///
/// Zero budget with a saved tail returns [`PublishedResume::Paused`] without
/// reading the graph. A positive budget spends that many release or root units
/// on the same machine as a full sweep. The one report is returned only when
/// the tail finishes, and that tail is then gone.
pub(crate) fn resume_published_sweep(
    host: &mut HostContext,
    budget: u64,
) -> Result<PublishedResume, HeapError> {
    if host.scope_state::<LuaHeap>().is_none() {
        return Err(HeapError::SweepDenied);
    }
    if budget == 0 {
        let saved = host
            .scope_state::<LuaHeap>()
            .is_some_and(LuaHeap::has_sweep_tail);
        if !saved {
            return Err(HeapError::SweepDenied);
        }
        return Ok(PublishedResume::Paused);
    }
    match with_heap(host, |heap| Ok(heap.resume_sweep_tail(budget)))? {
        table::TailResume::Denied => Err(HeapError::SweepDenied),
        table::TailResume::Paused => Ok(PublishedResume::Paused),
        table::TailResume::Completed(report) => Ok(PublishedResume::Completed(report)),
    }
}

/// One call of a private host step cycle.
///
/// `Paused { tail: false }` is a saved mark. `Paused { tail: true }` is a
/// published release tail. Published counts are not a pause signal: a zero
/// edge window is still [`StepOutcome::Published`] when a dead slot remains.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepOutcome {
    /// This call did not finish the saved mark or the saved tail.
    Paused { tail: bool },
    /// The mark finished and weak edges were published. Release did not start.
    Published {
        window_heads: u64,
        window_edges: u64,
    },
    /// The cycle finished in this call. The report was recorded once.
    Completed(table::CollectReport),
}

/// Saves one mark. Does not scan, install a heap, or clear an existing mark or tail.
fn open_private_step_cycle(host: &mut HostContext) -> Result<(), HeapError> {
    let Some(heap) = host.scope_state_mut::<LuaHeap>() else {
        return Err(HeapError::SweepDenied);
    };
    heap.open_private_step()
}

/// Advances one private cycle by `budget`.
///
/// No heap is [`HeapError::SweepDenied`] and installs nothing. A heap with
/// neither a saved mark nor a sweep tail, including after completion, is
/// [`HeapError::MarkDenied`]. A saved tail is resumed first; that path does
/// not read `batch`. Zero budget does not read `batch` or the graph. A
/// positive mark budget uses the existing batch check before any scan.
/// Finishing the mark closes tokens and publishes in this call. Pending-close
/// enumeration, per-token close, and the Owners walk are metered on their own
/// fields and are not limited by `budget`. Close failure returns the original
/// error and does not restore the taken mark. This call does not resume a
/// tail it just published.
fn step_private_cycle(
    host: &mut HostContext,
    budget: u64,
    batch: &[table::Rooted],
) -> Result<StepOutcome, HeapError> {
    let (tail, saved) = {
        let Some(heap) = host.scope_state::<LuaHeap>() else {
            return Err(HeapError::SweepDenied);
        };
        (heap.has_sweep_tail(), heap.has_saved_mark())
    };
    if tail {
        return match resume_published_sweep(host, budget)? {
            PublishedResume::Paused => Ok(StepOutcome::Paused { tail: true }),
            PublishedResume::Completed(report) => Ok(StepOutcome::Completed(report)),
        };
    }
    if !saved {
        return Err(HeapError::MarkDenied);
    }
    if budget == 0 {
        return Ok(StepOutcome::Paused { tail: false });
    }
    let marked = with_heap(host, |heap| heap.take_finished_saved_mark(budget, batch))?;
    let Some(marked) = marked else {
        return Ok(StepOutcome::Paused { tail: false });
    };
    match close_then_halt_at_publish(host, marked, false)? {
        PublishedBoundary::Completed(report) => Ok(StepOutcome::Completed(report)),
        PublishedBoundary::Published {
            window_heads,
            window_edges,
        } => Ok(StepOutcome::Published {
            window_heads,
            window_edges,
        }),
    }
}

/// Drops the top root frame while this scope's published sweep tail is saved.
///
/// No heap is [`HeapError::SweepDenied`] and installs nothing. An installed
/// heap checks the frame before the tail: no frame is [`HeapError::NoRootFrame`],
/// a non-top or old generation is [`HeapError::StaleFrame`], and a valid top
/// frame with no tail is [`HeapError::SweepDenied`]. Refusal does not move the
/// cursor, counts, root vector, or frame stack.
///
/// Release keeps the dead cursor and the accumulated counts. Roots deducts
/// `Some` tags already scanned in this frame, including duplicates, then clamps
/// the root cursor downward. This does not look up nodes, rescan roots, advance
/// the sweep, or roll back the scan meter. Not a guest `collectgarbage` step.
fn retire_published_root_frame(
    host: &mut HostContext,
    frame: table::RootFrameId,
) -> Result<table::FrameRetirement, HeapError> {
    if host.scope_state::<LuaHeap>().is_none() {
        return Err(HeapError::SweepDenied);
    }
    with_heap(host, |heap| heap.retire_published_root_frame(frame))
}

/// Deletes one ordinary UTF-8 named field while this scope's published sweep
/// tail is saved. Does not install a heap and does not resume the tail.
///
/// No installed heap is [`HeapError::SweepDenied`]. The installed domain is
/// checked before any other handle or tail result, so a foreign receiver is
/// [`HeapError::WrongDomain`] even when no tail is saved. A same-domain stale
/// or wrong-kind receiver still uses that handle error. No saved tail, after
/// those checks, is [`HeapError::SweepDenied`]. This is not a guest
/// `collectgarbage` step.
fn delete_published_named(
    host: &mut HostContext,
    table: TableRef,
    name: &str,
) -> Result<bool, HeapError> {
    let Some(heap) = host.scope_state::<LuaHeap>() else {
        return Err(HeapError::SweepDenied);
    };
    if table.domain() != heap.domain() {
        return Err(HeapError::WrongDomain);
    }
    with_heap(host, |heap| heap.delete_published_named(table, name))
}

/// Allocates one unmarked table while this scope's published sweep tail is
/// saved. Does not install a heap, resume the tail, or open a write.
///
/// No installed heap, or no saved tail, is [`HeapError::SweepDenied`] and
/// does not change the scope. A full node vector is [`HeapError::NodeCapacity`]
/// before a resource is pushed. A refused `push_resource` releases only that
/// empty reservation. This is not a guest `collectgarbage` step.
fn publish_late(host: &mut HostContext) -> Result<(TableRef, vm::ResourceHandle), HeapError> {
    let Some(heap) = host.scope_state_mut::<LuaHeap>() else {
        return Err(HeapError::SweepDenied);
    };
    let reservation = heap.reserve_late_table()?;
    let table = reservation.table();
    match host.push_resource(LuaTableResource::new(table)) {
        Ok(resource) => {
            let handle = resource.handle();
            host.scope_state_mut::<LuaHeap>()
                .expect("late reservation keeps the installed heap")
                .commit_late(reservation, handle.raw());
            Ok((table, handle))
        }
        Err(error) => {
            host.scope_state_mut::<LuaHeap>()
                .expect("late reservation keeps the installed heap")
                .abandon_late(reservation);
            Err(HeapError::from_scope(error))
        }
    }
}

/// Publishes one empty table into the current top frame while Release is paused.
///
/// Admission, including the empty root slot, is checked before a node or a
/// Core token is registered. `Ok(None)` means this pause is not an admitted
/// Release handoff; nothing was reserved. A later refusal closes the token,
/// confirms it is closed, and only then returns the slot. The Release cursor,
/// mark count, and accounted roots are not moved.
/// `fail_after_commit` is a test injection. It runs only after `commit_late`
/// has succeeded and before the first root write, then uses the same close,
/// `undo_committed_late`, and slot-release branch as a real store error.
///
#[cfg(test)]
pub fn publish_automatic_release_table(
    host: &mut HostContext,
    slot: u32,
    fail_after_registration: bool,
    fail_after_commit: bool,
) -> Result<Option<Value>, HeapError> {
    let Some(heap) = host.scope_state::<LuaHeap>() else {
        return Ok(None);
    };
    if !heap.automatic_release_slot_admitted(slot)? {
        return Ok(None);
    }
    let reservation = {
        let heap = host
            .scope_state_mut::<LuaHeap>()
            .expect("admission found the installed heap");
        heap.reserve_late_table()?
    };
    if !host
        .scope_state::<LuaHeap>()
        .expect("reservation keeps the installed heap")
        .automatic_release_slot_admitted(slot)?
    {
        host.scope_state_mut::<LuaHeap>()
            .expect("reservation keeps the installed heap")
            .abandon_late(reservation);
        return Err(HeapError::Scope(
            "release handoff lost admission after reserve".to_string(),
        ));
    }
    let table = reservation.table();
    let resource = match host.push_resource(LuaTableResource::new(table)) {
        Ok(resource) => resource,
        Err(error) => {
            host.scope_state_mut::<LuaHeap>()
                .expect("reservation keeps the installed heap")
                .abandon_late(reservation);
            return Err(HeapError::from_scope(error));
        }
    };
    let handle = resource.handle();
    if fail_after_registration {
        return rollback_unbound_release(
            host,
            reservation,
            handle,
            HeapError::Scope(
                "automatic release allocation refused after the token was registered".to_string(),
            ),
        );
    }
    let value = match host.scope_state_mut::<LuaHeap>() {
        Some(heap) => heap.issue_reserved_carrier(table),
        None => Err(HeapError::SweepDenied),
    };
    let value = match value {
        Ok(value) => value,
        Err(error) => return rollback_unbound_release(host, reservation, handle, error),
    };
    if !host
        .scope_state::<LuaHeap>()
        .is_some_and(|heap| heap.automatic_release_slot_admitted(slot).unwrap_or(false))
    {
        return rollback_unbound_release(
            host,
            reservation,
            handle,
            HeapError::Scope("release handoff lost admission after issue".to_string()),
        );
    }
    let raw = handle.raw();
    let before = release_txn_snap(host)?;
    host.scope_state_mut::<LuaHeap>()
        .expect("reservation keeps the installed heap")
        .commit_late(reservation, raw);
    let stored = if fail_after_commit {
        Err(HeapError::Scope(
            "injected after commit_late and before the first root write".to_string(),
        ))
    } else {
        host.scope_state_mut::<LuaHeap>()
            .expect("committed late node keeps the installed heap")
            .store_release_top_root(slot, table)
    };
    if let Err(error) = stored {
        close_confirmed_token::<LuaTableResource>(host, handle)?;
        host.scope_state_mut::<LuaHeap>()
            .expect("committed late node keeps the installed heap")
            .undo_committed_late(table)?;
        return Err(error);
    }
    let after = release_txn_snap(host)?;
    remember_release_txn(host, ReleaseTxnWitness { before, after })?;
    Ok(Some(value))
}

#[cfg(test)]
fn rollback_unbound_release(
    host: &mut HostContext,
    reservation: table::LateReservation,
    handle: vm::ResourceHandle,
    error: HeapError,
) -> Result<Option<Value>, HeapError> {
    close_confirmed_token::<LuaTableResource>(host, handle)?;
    host.scope_state_mut::<LuaHeap>()
        .expect("unbound reservation keeps the installed heap")
        .abandon_late(reservation);
    Err(error)
}

/// True when `slot` can receive one automatic empty table on the saved Release tail.
#[cfg(test)]
pub fn automatic_release_slot_admitted(
    host: &mut HostContext,
    slot: u32,
) -> Result<bool, HeapError> {
    let Some(heap) = host.scope_state::<LuaHeap>() else {
        return Ok(false);
    };
    heap.automatic_release_slot_admitted(slot)
}

/// Mark count and late-node count of the saved tail. Does not resume it.
#[cfg(test)]
pub fn saved_tail_accounting(host: &mut HostContext) -> Result<Option<(u32, u32)>, HeapError> {
    let Some(heap) = host.scope_state::<LuaHeap>() else {
        return Ok(None);
    };
    Ok(heap.saved_tail_accounting())
}

/// One saved-tail reading around an automatic Release handoff.
///
/// `marked` and `late` come from the tail itself. This does not resume it.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReleaseTxnSnap {
    pub tail: SavedTailState,
    pub marked: u32,
    pub late: u32,
}

/// Readings taken immediately before `commit_late` and after the root store.
/// Absent when that handoff did not store a root.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReleaseTxnWitness {
    pub before: ReleaseTxnSnap,
    pub after: ReleaseTxnSnap,
}

/// Returns the committed handoff readings, if this execution stored one.
#[cfg(test)]
pub fn release_txn_witness(host: &HostContext) -> Option<ReleaseTxnWitness> {
    host.scope_state::<ReleaseTxnWitness>().copied()
}

#[cfg(test)]
fn release_txn_snap(host: &mut HostContext) -> Result<ReleaseTxnSnap, HeapError> {
    let tail = saved_tail_state(host)?.ok_or(HeapError::SweepDenied)?;
    let (marked, late) = saved_tail_accounting(host)?.ok_or(HeapError::SweepDenied)?;
    Ok(ReleaseTxnSnap { tail, marked, late })
}

#[cfg(test)]
fn remember_release_txn(
    host: &mut HostContext,
    witness: ReleaseTxnWitness,
) -> Result<(), HeapError> {
    if let Some(existing) = host.scope_state_mut::<ReleaseTxnWitness>() {
        *existing = witness;
        return Ok(());
    }
    host.scope_state_or_insert_with(|| witness)
        .map(|_| ())
        .map_err(HeapError::from_scope)
}

/// Table roots in the current top frame, in slot order. Does not scan.
#[cfg(test)]
pub fn top_frame_tables(host: &mut HostContext) -> Result<Vec<Option<TableRef>>, HeapError> {
    let Some(heap) = host.scope_state::<LuaHeap>() else {
        return Ok(Vec::new());
    };
    Ok(heap.top_frame_tables())
}

/// Replaces one ordinary UTF-8 gap with an already issued table while this
/// scope's published sweep tail is saved. Does not install a heap and does
/// not resume the tail.
///
/// No installed heap is [`HeapError::SweepDenied`]. The installed domain is
/// checked on `from` before any other handle or tail result, so a foreign
/// owner is [`HeapError::WrongDomain`] even when no tail is saved. `to` is
/// not examined until a real gap is found. This is not a guest
/// `collectgarbage` step or a guest `rawset`.
fn refill_published_gap(
    host: &mut HostContext,
    from: TableRef,
    name: &str,
    to: TableRef,
) -> Result<bool, HeapError> {
    let Some(heap) = host.scope_state::<LuaHeap>() else {
        return Err(HeapError::SweepDenied);
    };
    if from.domain() != heap.domain() {
        return Err(HeapError::WrongDomain);
    }
    with_heap(host, |heap| heap.refill_published_gap(from, name, to))
}

/// One host call: a fresh mark, the existing synchronous token close, then
/// one sweep. The observer is lent only at the published boundary and does
/// not receive the host. A parked mark is refused and left in place. This is
/// not a guest `collectgarbage` step.
pub fn collect_observing<R>(
    host: &mut HostContext,
    observe: impl for<'a> FnOnce(&'a PublishedRead<'a>) -> R,
) -> Result<(table::CollectReport, Option<R>), HeapError> {
    if host
        .scope_state::<LuaHeap>()
        .is_some_and(LuaHeap::has_sweep_tail)
    {
        return Err(HeapError::SweepPending);
    }
    let marked = with_heap(host, |heap| heap.begin_private_cycle())?;
    close_then_sweep_observing(host, marked, observe)
}

/// A saved mark that this call did not finish, or the one report from the
/// sweep that followed a finished mark.
#[derive(Debug)]
enum SavedMarkResume<R> {
    /// The saved mark is still on the heap. No token was closed and nothing
    /// was swept.
    Paused,
    /// Close and sweep finished in this call. The observer ran only when a
    /// dead slot was published.
    Completed(table::CollectReport, Option<R>),
}

/// Continues a mark this scope already saved. Does not start a mark and does
/// not install a heap.
///
/// No saved mark is [`HeapError::MarkDenied`], including a missing heap.
/// Zero budget with a saved mark returns [`SavedMarkResume::Paused`] without
/// reading the batch or the graph. A positive budget that stops before Done
/// may move the cursor and returns `Paused` with no enumeration, close, or
/// sweep; a budget that runs out before Roots does not commit `batch`.
/// Done removes the saved mark, enumerates once, and closes through the same
/// token retirement as [`collect`]. The observer and sweep then share the
/// private-cycle tail. Close failure drops that snapshot, keeps the closed
/// prefix, publishes nothing, and leaves the continuation stateless.
///
/// `batch` is an additional safepoint slice. It is not written into a fixed
/// root slot and is not deduped. A root already sitting in a fixed slot or a
/// pending frame must not be passed again. This is not a guest `step`.
///
/// Private to this module. `Rooted` is not a public heap schema, so this
/// signature stays inside the heap module and its tests.
fn resume_private_cycle<R>(
    host: &mut HostContext,
    budget: u64,
    batch: &[table::Rooted],
    observe: impl for<'a> FnOnce(&'a PublishedRead<'a>) -> R,
) -> Result<SavedMarkResume<R>, HeapError> {
    if host.scope_state::<LuaHeap>().is_none() {
        return Err(HeapError::MarkDenied);
    }
    if host
        .scope_state::<LuaHeap>()
        .is_some_and(LuaHeap::has_sweep_tail)
    {
        return Err(HeapError::SweepPending);
    }
    if budget == 0 {
        let saved = host
            .scope_state::<LuaHeap>()
            .is_some_and(LuaHeap::has_saved_mark);
        if !saved {
            return Err(HeapError::MarkDenied);
        }
        return Ok(SavedMarkResume::Paused);
    }
    let marked = with_heap(host, |heap| heap.take_finished_saved_mark(budget, batch))?;
    let Some(marked) = marked else {
        return Ok(SavedMarkResume::Paused);
    };
    let (report, seen) = close_then_sweep_observing(host, marked, observe)?;
    Ok(SavedMarkResume::Completed(report, seen))
}

fn close_then_sweep_observing<R>(
    host: &mut HostContext,
    marked: table::DoomedMark,
    observe: impl for<'a> FnOnce(&'a PublishedRead<'a>) -> R,
) -> Result<(table::CollectReport, Option<R>), HeapError> {
    for token in marked.tokens {
        retire_doomed_token(host, token)?;
    }
    with_heap(host, |heap| Ok(heap.sweep_observing(marked.modes, observe)))
}

fn retire_doomed_token(host: &mut HostContext, token: table::DoomedToken) -> Result<(), HeapError> {
    match token.kind {
        HandleKind::Table => retire_typed_token::<LuaTableResource>(host, token),
        HandleKind::Userdata => retire_userdata_token(host, token),
        HandleKind::Closure | HandleKind::Upvalue => Err(HeapError::WrongKind),
    }
}

/// Closes one doomed userdata token after confirming that the open resource
/// payload names the exact Lua slot the mark captured.
///
/// The slot must still store the captured raw handle at the same generation.
/// The typed borrow validates the Core arena, generation, concrete `TypeId`,
/// and the `lua.userdata` key, and its payload must equal the handle derived
/// from that slot record. A same-type token for another (even rooted) userdata
/// is [`HeapError::TokenMismatch`] and is not closed. The read-only borrow is
/// released before the close runs. An already-retired handle keeps the
/// existing clear-or-error branch. A `Pending` result is an error.
fn retire_userdata_token(
    host: &mut HostContext,
    token: table::DoomedToken,
) -> Result<(), HeapError> {
    let handle =
        vm::ResourceHandle::from_raw(token.raw).map_err(|_| HeapError::CarrierOutOfRange)?;
    let expected = with_heap(host, |heap| heap.bound_userdata_for_token(&token))?;
    let retired = match host
        .borrow_resource_with_key::<LuaUserdataResource>(handle, &LuaUserdataResource::key())
    {
        Ok(borrowed) => {
            if borrowed.get().userdata() != expected {
                return Err(HeapError::TokenMismatch);
            }
            None
        }
        Err(error) => {
            if !retired_token(&error) {
                return Err(HeapError::from_scope(error));
            }
            Some(error)
        }
    };
    if let Some(error) = retired {
        let cleared = with_heap(host, |heap| Ok(heap.clear_exact_token(token)))?;
        return if cleared {
            Ok(())
        } else {
            Err(HeapError::from_scope(error))
        };
    }
    with_heap(host, |heap| {
        heap.note_token_close();
        Ok(())
    })?;
    match host.close_resource::<LuaUserdataResource>(handle, vm::ResourceCloseReason::Requested) {
        Ok(vm::resource::CloseProgress::Ready) => {}
        Ok(_) => {
            return Err(HeapError::Scope(
                "lua typed resource close did not finish synchronously".to_string(),
            ));
        }
        Err(error) => {
            if !retired_token(&error) {
                return Err(HeapError::from_scope(error));
            }
            let cleared = with_heap(host, |heap| Ok(heap.clear_exact_token(token)))?;
            return if cleared {
                Ok(())
            } else {
                Err(HeapError::from_scope(error))
            };
        }
    }
    let cleared = with_heap(host, |heap| Ok(heap.clear_exact_token(token)))?;
    if !cleared {
        return Err(HeapError::StaleHandle);
    }
    Ok(())
}

/// Closes one doomed token through the concrete resource type that the slot
/// kind owns. The slot, generation, kind, and raw handle were captured at mark
/// time; the clear primitive rechecks that exact identity and leaves a reused
/// successor alone. A Pending close is an error and does not clear the binding
/// or release the slot. `ResourceAlreadyClosed` and `ResourceStale` retire the
/// token only when [`table::LuaHeap::clear_exact_token`] confirms the binding.
fn retire_typed_token<T: vm::resource::HostResource>(
    host: &mut HostContext,
    token: table::DoomedToken,
) -> Result<(), HeapError> {
    let handle =
        vm::ResourceHandle::from_raw(token.raw).map_err(|_| HeapError::CarrierOutOfRange)?;
    with_heap(host, |heap| {
        heap.note_token_close();
        Ok(())
    })?;
    match host.close_resource::<T>(handle, vm::ResourceCloseReason::Requested) {
        Ok(progress) => {
            if progress != vm::resource::CloseProgress::Ready {
                return Err(HeapError::Scope(
                    "lua typed resource close did not finish synchronously".to_string(),
                ));
            }
            let cleared = with_heap(host, |heap| Ok(heap.clear_exact_token(token)))?;
            if !cleared {
                return Err(HeapError::StaleHandle);
            }
            Ok(())
        }
        Err(error) => {
            if !retired_token(&error) {
                return Err(HeapError::from_scope(error));
            }
            let cleared = with_heap(host, |heap| Ok(heap.clear_exact_token(token)))?;
            if cleared {
                Ok(())
            } else {
                Err(HeapError::from_scope(error))
            }
        }
    }
}

fn retired_token(error: &vm::HostContextError) -> bool {
    match error.kind() {
        vm::HostContextErrorKind::Scope(vm::execution_scope::ExecutionScopeError::Resource(
            resource,
        ))
        | vm::HostContextErrorKind::Resource(resource) => matches!(
            resource.code(),
            vm::resource::ResourceErrorCode::ResourceAlreadyClosed
                | vm::resource::ResourceErrorCode::ResourceStale
        ),
        _ => false,
    }
}

/// Reads occupancy counters from the scope heap. This does not collect and
/// does not add scan counts.
pub fn stats(host: &mut HostContext) -> Result<HeapStats, HeapError> {
    with_heap(host, |heap| Ok(heap.stats()))
}

/// Reads and clears the scan counts from the scope heap. This does not collect
/// and does not record the read as collection work.
pub fn take_scan_meter(host: &mut HostContext) -> Result<ScanMeter, HeapError> {
    with_heap(host, |heap| Ok(heap.take_scan_meter()))
}

/// Advances the explicitly enabled source candidate at a positive-budget safepoint.
/// Root candidates have been classified only on the mark path; the private
/// stepper validates the complete batch before scanning and ignores it on a tail.
#[cfg(test)]
pub fn step_source_safepoint(
    host: &mut HostContext,
    budget: std::num::NonZeroU64,
    tables: Vec<TableRef>,
) -> Result<StepOutcome, HeapError> {
    if saved_source_tail(host) {
        return step_private_cycle(host, budget.get(), &[]);
    }
    with_heap(host, |_| Ok(()))?;
    if fixed_step_idle(host)? {
        open_private_step_cycle(host)?;
    }
    let batch: Vec<_> = tables.into_iter().map(table::Rooted::Table).collect();
    step_private_cycle(host, budget.get(), &batch)
}

#[cfg(test)]
pub fn saved_source_tail(host: &HostContext) -> bool {
    host.scope_state::<LuaHeap>()
        .is_some_and(LuaHeap::has_sweep_tail)
}

/// Saves one private mark for a cycle whose roots already sit in fixed frames.
///
/// Does not scan. A missing heap, an existing mark, or a saved tail is the
/// private open's existing refusal. Not a guest `collectgarbage` step.
#[cfg(test)]
pub fn open_fixed_step(host: &mut HostContext) -> Result<(), HeapError> {
    open_private_step_cycle(host)
}

/// True when this scope has a heap and neither a saved mark nor a sweep tail.
#[cfg(test)]
pub fn fixed_step_idle(host: &mut HostContext) -> Result<bool, HeapError> {
    let Some(heap) = host.scope_state::<LuaHeap>() else {
        return Err(HeapError::SweepDenied);
    };
    Ok(!heap.has_sweep_tail() && !heap.has_saved_mark())
}

/// Steps one private cycle whose roots already sit in fixed frames.
///
/// `budget` must be positive. Zero is refused before the private stepper, so
/// this adapter does not use the zero-budget observe path. The batch is empty:
/// a root already registered in a frame is not submitted again. The result is
/// the real [`StepOutcome`]. Not a guest `collectgarbage` step.
#[cfg(test)]
pub fn step_fixed_frames(host: &mut HostContext, budget: u64) -> Result<StepOutcome, HeapError> {
    if budget == 0 {
        return Err(HeapError::MarkDenied);
    }
    step_private_cycle(host, budget, &[])
}

/// Whether this scope still has the published sweep tail.
#[cfg(test)]
pub fn saved_sweep_tail(host: &mut HostContext) -> Result<bool, HeapError> {
    let Some(heap) = host.scope_state::<LuaHeap>() else {
        return Err(HeapError::SweepDenied);
    };
    Ok(heap.has_sweep_tail())
}

/// Drops one top frame while this scope's published sweep tail is saved.
///
/// The private retirement remains the authority. Refusal does not move the
/// cursor, counts, root vector, or frame stack. Not a guest `collectgarbage` step.
#[cfg(test)]
pub fn retire_saved_tail_frame(
    host: &mut HostContext,
    frame: RootFrameId,
) -> Result<(), HeapError> {
    retire_published_root_frame(host, frame).map(|_| ())
}

/// Phase, cursor, and accounted roots of the saved tail. Does not resume it.
#[cfg(test)]
pub fn saved_tail_state(host: &mut HostContext) -> Result<Option<SavedTailState>, HeapError> {
    let Some(heap) = host.scope_state::<LuaHeap>() else {
        return Err(HeapError::SweepDenied);
    };
    Ok(heap.saved_tail_state())
}

/// Saved mark phase and whether gray work remains. Absent means no saved mark.
/// Does not take the mark, scan, enqueue, or write.
#[cfg(test)]
pub fn saved_mark_query(host: &HostContext) -> Option<(&'static str, bool)> {
    host.scope_state::<LuaHeap>()
        .and_then(LuaHeap::saved_mark_query)
}

/// Saved mark cursor. Absent means no saved mark. Does not take, scan, or write.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SavedMarkWatch {
    pub phase: &'static str,
    pub pending: bool,
    pub cursor_owner: Option<u32>,
    pub cursor_index: Option<u32>,
    pub gray_len: u32,
}

/// One existing edge record. `value_slot` is set only for a table value.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SavedEdgeWatch {
    pub index: u32,
    pub value_slot: Option<u32>,
    pub value_string: bool,
}

/// Reads the saved mark cursor. Does not take the mark, scan, enqueue, or write.
#[cfg(test)]
pub fn saved_mark_watch(host: &HostContext) -> Option<SavedMarkWatch> {
    host.scope_state::<LuaHeap>()?.saved_mark_watch()
}

/// Live marked bit. A missing heap or a stale handle is false. Does not scan.
#[cfg(test)]
pub fn table_marked(host: &HostContext, table: TableRef) -> bool {
    host.scope_state::<LuaHeap>()
        .is_some_and(|heap| heap.table_marked(table))
}

/// Saved gray-vector membership. Does not walk edges or push.
#[cfg(test)]
pub fn table_in_gray(host: &HostContext, table: TableRef) -> bool {
    host.scope_state::<LuaHeap>()
        .is_some_and(|heap| heap.table_in_gray(table))
}

/// Whether the current top frame already roots this table. Does not scan edges.
#[cfg(test)]
pub fn current_frame_roots_table(host: &HostContext, table: TableRef) -> bool {
    let Some(heap) = host.scope_state::<LuaHeap>() else {
        return false;
    };
    let Some(frame) = heap.current_frame() else {
        return false;
    };
    heap.frame_roots_table(frame, table)
}

/// Existing number-key record. `None` means the key is absent or the table is dead.
#[cfg(test)]
pub fn number_edge_watch(host: &HostContext, table: TableRef, key: f64) -> Option<SavedEdgeWatch> {
    let (index, value_slot, value_string) = host
        .scope_state::<LuaHeap>()?
        .number_edge_watch(table, key)?;
    Some(SavedEdgeWatch {
        index,
        value_slot,
        value_string,
    })
}

/// Existing UTF-8 record. Same read-only contract as [`number_edge_watch`].
#[cfg(test)]
pub fn utf8_edge_watch(host: &HostContext, table: TableRef, key: &str) -> Option<SavedEdgeWatch> {
    let (index, value_slot, value_string) =
        host.scope_state::<LuaHeap>()?.utf8_edge_watch(table, key)?;
    Some(SavedEdgeWatch {
        index,
        value_slot,
        value_string,
    })
}

/// Capacity of an already decoded automatic root batch.
///
/// A missing heap is `SweepDenied` and does not install one. This does not
/// pop a budget, set a latch, open a mark, or scan. The private stepper still
/// rechecks the same sum before it takes a saved mark.
#[cfg(test)]
pub fn preflight_automatic_root_capacity(
    host: &HostContext,
    batch_len: usize,
) -> Result<(), HeapError> {
    let Some(heap) = host.scope_state::<LuaHeap>() else {
        return Err(HeapError::SweepDenied);
    };
    heap.root_batch_fits(batch_len)
}

/// Copies the scan meter without clearing it.
#[cfg(test)]
pub fn copy_scan_meter(host: &mut HostContext) -> Result<ScanMeter, HeapError> {
    let Some(heap) = host.scope_state::<LuaHeap>() else {
        return Err(HeapError::SweepDenied);
    };
    Ok(heap.scan_meter())
}

/// The table currently stored in one frame slot. Does not scan.
#[cfg(test)]
pub fn rooted_table(
    host: &mut HostContext,
    frame: RootFrameId,
    slot: u32,
) -> Result<Option<TableRef>, HeapError> {
    with_heap(host, |heap| heap.rooted_table(frame, slot))
}

/// Publishes one returned table into the direct caller slot during Release.
///
/// The guest carrier and the Core token are checked separately. A stored raw
/// id is not treated as proof that the token is still open. Phase, frame,
/// slot, and mark are checked again before the slot is written. This does not
/// call the ordinary root writer.
#[cfg(test)]
pub fn publish_release_return_root(
    host: &mut HostContext,
    caller: RootFrameId,
    slot: u32,
    value: &Value,
) -> Result<(), HeapError> {
    if host.scope_state::<LuaHeap>().is_none() {
        return Err(HeapError::SweepDenied);
    }
    let table = with_heap(host, |heap| match heap.guest_class(value) {
        GuestClass::Table(table) => Ok(table),
        _ => Err(HeapError::StaleHandle),
    })?;
    let raw = with_heap(host, |heap| heap.published_token(table))?;
    admit_open_table_token(host, table, raw)?;
    with_heap(host, |heap| {
        heap.store_release_caller_root(caller, slot, table)
    })
}

/// Borrows the typed token. Does not take ownership and does not write a root.
#[cfg(test)]
fn admit_open_table_token(host: &HostContext, table: TableRef, raw: u64) -> Result<(), HeapError> {
    let handle = vm::ResourceHandle::from_raw(raw).map_err(|_| HeapError::CarrierOutOfRange)?;
    let borrowed = host
        .borrow_resource_with_key::<LuaTableResource>(handle, &LuaTableResource::key())
        .map_err(HeapError::from_scope)?;
    if borrowed.get().table() != table {
        return Err(HeapError::StaleHandle);
    }
    Ok(())
}
