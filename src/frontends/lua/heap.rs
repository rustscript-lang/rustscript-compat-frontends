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
//! frames, and non-moving mark/sweep. Not borrowed: numeric guest backing,
//! JavaScript classifications, swc, or guest IR.
//!
//! Later integration, not this kernel: LuaJIT `ordered.lua` plus the reverse
//! and skip-collection controls in
//! `/mnt/workspace/rustscript-lua/evidence/lua52-gc-arg-order/qualification.md`.
//! When `.lua` is connected, that stage consumes the sample. A collection
//! witness must be a helper that only returns a boolean; do not inline-read a
//! weak referent from the caller. This milestone has no weak references.

#[path = "heap/handle.rs"]
mod handle;
#[path = "heap/resource.rs"]
mod resource;
#[path = "heap/table.rs"]
mod table;

pub use handle::{HandleKind, TableRef};
pub use resource::LuaTableResource;
pub use table::{CollectReport, HeapError, HeapLimits, HeapStats, LuaHeap, RootFrameId};

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
    Ok(Value::Int(raw))
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

/// Host-side collection. Unreachable tables lose their typed tokens through
/// `HostContext::close_resource` before the heap sweep, so the scope quota
/// comes back in this same VM. Stored handles are not roots.
pub fn collect(host: &mut HostContext) -> Result<table::CollectReport, HeapError> {
    let doomed = with_heap(host, |heap| Ok(heap.unmarked_tokens()))?;
    for raw in doomed {
        let handle =
            vm::ResourceHandle::from_raw(raw).map_err(|_| HeapError::CarrierOutOfRange)?;
        let progress = host
            .close_resource::<LuaTableResource>(handle, vm::ResourceCloseReason::Requested)
            .map_err(HeapError::from_scope)?;
        if progress != vm::resource::CloseProgress::Ready {
            return Err(HeapError::Scope(
                "lua table close did not finish synchronously".to_string(),
            ));
        }
        with_heap(host, |heap| {
            heap.clear_token(raw);
            Ok(())
        })?;
    }
    with_heap(host, |heap| Ok(heap.collect()))
}

/// Reads occupancy counters from the scope heap.
pub fn stats(host: &mut HostContext) -> Result<HeapStats, HeapError> {
    with_heap(host, |heap| Ok(heap.stats()))
}
