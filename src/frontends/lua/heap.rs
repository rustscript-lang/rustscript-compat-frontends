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
//! payload, and Core token. At weak publication a weak value that names it is
//! cleared, while a weak key keeps the entry and its scalar; a sweep with no
//! dead slot still runs the Owners walk when a pending finalizer needs that
//! publication. Ordinary collections never select; they offer every existing
//! record as a host-retained edge, so no live token is closed or orphaned
//! while it waits for a consumer. No callback, resurrection, guest step, or
//! production entry is added here. The test-only retirement control consumes
//! one record so a later collection can close the token and advance the slot
//! generation; it is not a finalizer.
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
    /// Neither a dead slot nor a retained pending finalizer needs a weak
    /// publication. Sweep finished and recorded the one report.
    Completed(table::CollectReport),
    /// Weak edges are published. Dead slots are still physical. No report.
    /// `window_heads`/`window_edges` are the actual Owners walk, which can
    /// be non-empty with no dead slot when a pending finalizer's weak value
    /// is cleared.
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
    close_then_halt_at_publish(host, marked)
}

/// Closes doomed tokens, then publishes or finishes. Same order as the
/// original collector: a close error returns before publish, so there is no
/// tail and no report. Pending-close enumeration already ran; per-token close
/// and the Owners walk are not limited by a step budget.
fn close_then_halt_at_publish(
    host: &mut HostContext,
    marked: table::DoomedMark,
) -> Result<PublishedBoundary, HeapError> {
    for token in marked.tokens {
        retire_doomed_token(host, token)?;
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
///    and the metatable's own strong entries. A slot already pending or
///    settled by the explicit retirement control is not selected again.
/// 4. Enumerate pending-close candidates as usual. A selected userdata is
///    marked, so it is not in that list and its token stays bound.
/// 5. Borrow every pending record with its exact `lua.userdata` token and
///    require the payload to name the same slot and generation. A mismatch
///    returns before any close or publish.
/// 6. Close each doomed token synchronously and confirm `Ready`.
/// 7. Run the Owners walk once. Weak values that name a retained pending
///    userdata are published as gaps; weak keys keep the entry and its
///    scalar because the selected object is marked. A dead slot is not
///    required for this step. Store the Release/Roots tail and return
///    [`PublishedBoundary::Published`]; a heap with neither a dead slot nor a
///    pending finalizer finishes and returns
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
        // only the records and flags appended by this selecting pass. Earlier
        // valid records are untouched, no doomed token has been closed yet,
        // and no weak edge has been published, so the two later failure
        // prefixes are not mixed into this rollback.
        with_heap(host, |heap| {
            heap.discard_pending_finalizers_from(base);
            Ok(())
        })?;
        return Err(error);
    }
    close_then_halt_at_publish(host, marked)
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
/// fabricate a close status.
#[cfg(test)]
pub fn retire_pending_finalizer_test_control(
    host: &mut HostContext,
    userdata: UserdataRef,
) -> Result<(), HeapError> {
    with_heap(host, |heap| heap.retire_pending_finalizer(userdata))
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
    match close_then_halt_at_publish(host, marked)? {
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
