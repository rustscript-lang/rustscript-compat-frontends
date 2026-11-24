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

pub use handle::{ClosureRef, HandleKind, TableRef, UpvalueRef};
pub use resource::LuaTableResource;
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
/// `LuaTableResource` uses the default synchronous close, which returns Ready.
/// A Pending result is an error. This type has no path that starts an
/// asynchronous close, and this collector does not add one.
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

/// Publish stopped before release, or the heap had no dead slot to publish.
#[derive(Debug)]
enum PublishedBoundary {
    /// No dead slot. Sweep finished and recorded the one report.
    Completed(table::CollectReport),
    /// Weak edges are published. Dead slots are still physical. No report.
    Published {
        window_heads: u64,
        window_edges: u64,
    },
}

/// A saved published sweep that this call did not finish, or the one report
/// from the tail that did finish.
#[derive(Debug)]
enum PublishedResume {
    /// The tail is still saved. This call did not finish the sweep.
    Paused,
    /// The tail finished in this call. The report was recorded once.
    Completed(table::CollectReport),
}

/// Marks, closes doomed tokens, publishes weak edges, and saves the release tail.
///
/// Does not take an observer. A parked mark is refused and left in place.
/// Close failure publishes nothing and saves no tail.
fn collect_until_published(host: &mut HostContext) -> Result<PublishedBoundary, HeapError> {
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

/// Continues a published sweep this scope already saved. Does not install a
/// heap and does not start a sweep. No saved tail is [`HeapError::SweepDenied`].
///
/// Zero budget with a saved tail returns [`PublishedResume::Paused`] without
/// reading the graph. A positive budget spends that many release or root units
/// on the same machine as a full sweep. The one report is returned only when
/// the tail finishes, and that tail is then gone.
fn resume_published_sweep(
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
    let handle =
        vm::ResourceHandle::from_raw(token.raw).map_err(|_| HeapError::CarrierOutOfRange)?;
    with_heap(host, |heap| {
        heap.note_token_close();
        Ok(())
    })?;
    match host.close_resource::<LuaTableResource>(handle, vm::ResourceCloseReason::Requested) {
        Ok(progress) => {
            if progress != vm::resource::CloseProgress::Ready {
                return Err(HeapError::Scope(
                    "lua table close did not finish synchronously".to_string(),
                ));
            }
            with_heap(host, |heap| {
                let _cleared = heap.clear_exact_token(token);
                Ok(())
            })?;
            Ok(())
        }
        Err(error) => {
            if !retired_lua_token(&error) {
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

fn retired_lua_token(error: &vm::HostContextError) -> bool {
    match error.kind() {
        vm::HostContextErrorKind::Scope(vm::execution_scope::ExecutionScopeError::Resource(
            resource,
        )) => matches!(
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
