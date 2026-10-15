//! Optional production Compat host runtime for owned numeric arrays.
//!
//! The accepted A4 lifecycle behavior is promoted here into an explicitly
//! opt-in production surface. Nothing in this module changes the manual
//! `arrays::*` handlers, their defaults, or any Core contract. A guest
//! program opts in per VM through [`initialize_typescript_owned_arrays`];
//! without that call every owned entry point fails closed.
//!
//! Guest protocol (`__compat_array_owner` namespace):
//!
//! - `allocate(initial: Array(Number)) -> Resource(compat.number_array)`
//!   reserves payload capacity under the pending quota and returns a fresh
//!   typed owner. It additionally requires valid source capture
//!   certification, so uncertified or artifact-loaded programs fail closed.
//! - `register(candidate: Borrow Resource(compat.number_array)) -> Number`
//!   moves exactly one pending allocation into one scope-local weak-owner
//!   record. The candidate must be the active dispatcher frame's certified
//!   direct Move owner with a Live root; anything else (wrong handle, wrong
//!   type or key, foreign arena, stale generation, duplicate, missing
//!   pending entry) fails without closing unrelated resources. Registration
//!   runs inside the guest factory before it returns, never on first user
//!   access.
//! - `collect() -> Number` is the explicit safety point. It inspects at most
//!   four registered records per call (cursor plus swap-removal progress, so
//!   retained, error and removed entries all count as work) and returns how
//!   many payloads were synchronously closed on this call.
//! - `resize(candidate: BorrowMut Resource(compat.number_array),
//!   length: Number) -> Number` grows or shrinks one registered payload and
//!   returns the resulting length. Growth inside existing capacity keeps the
//!   same buffer and the same reserved accounting; capacity growth is staged
//!   and transactional, so a refusal preserves pointer, capacity, length,
//!   values and all accounting.
//!
//! Root discipline, mirroring the accepted behavior:
//!
//! - `Live` retains; `Unavailable` (epoch revocation) retains the open
//!   payload and its accounting.
//! - `Released` may close only after exact independent typed
//!   handle/key/concrete-type/arena/generation validation, all borrows
//!   dropped, and an actual `Ready` synchronous close of the unchanged
//!   concrete `NumberArray`.
//! - `AlreadyClosed` also covers `Closing` for resources in general, so it is
//!   never treated as destruction proof on its own; only the narrow,
//!   synchronous, concrete `NumberArray` retirement below applies.
//! - Wrong table, wrong type, wrong key and invalid handles never retire
//!   metadata; `AccessConflict` retains the entry and moves on.
//!
//! State placement: pending counts, the registered weak-root directory, the
//! reserved payload bytes and the collection cursor live in execution scope
//! state and die with terminal reset. Persistent module state holds only the
//! copied [`OwnedArrayLimits`] configuration. No callable values,
//! environments, cell arcs or directory records are retained in module
//! state, and no strong owner reference is ever kept: liveness is observed
//! exclusively through weak capture roots.
//!
//! Quotas are three independent hard bounds with checked arithmetic:
//! pending allocation count, registered record count, and combined
//! pending-plus-registered reserved payload bytes (`capacity *
//! size_of::<Value>()` with checked sums). Failed insertions roll their
//! reservation back; a failed registration leaves its open payload in
//! bounded pending state, never in an unbounded failure ledger.
//!
//! All four entry points are real owned-dispatch descriptors
//! ([`vm::host_extension::HostBindingKind::Owned`] with
//! [`vm::host_extension::HostAdapterDescriptor::Owned`] factories) composed
//! through the ordinary [`vm::host_extension::HostModuleDescriptor`]
//! catalog collection and `install_from_catalog` path, so the exact
//! Borrow/BorrowMut/create-effect contract guards every call. Host
//! callbacks never reenter VM execution and report failures as structured
//! [`VmError`], never panics.

use std::collections::HashMap;
use std::mem::size_of;

use vm::host_extension::{
    HostAdapterDescriptor, HostBindingDescriptor, HostBindingKind, HostFunctionDescriptor,
    HostModuleDescriptor, HostResourceTypeMeta, guest_resource_effects,
};
use vm::resource::{CloseProgress, ResourceErrorCode};
use vm::{
    CallOutcome, CallReturn, CaptureRootState, HostFunctionSchema, HostOwnedAdapterFactory,
    HostOwnedFunction, HostParamPassing, HostParamSchema, HostTypeSchema, OwnedHostCall,
    OwnedHostContext, ResourceCaptureRoot, ResourceCloseReason, Value, Vm, VmError, VmResult,
};

use super::{NumberArray, array_key};
use vm::resource::ResourceHandle;

/// Independent hard quotas for one VM's owned-array runtime.
///
/// All three bounds must be positive; [`OwnedArrayLimits::new`] rejects a
/// zero in any position. Defaults are 8 pending allocations, 64 registered
/// records and 1 MiB of combined reserved payload bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnedArrayLimits {
    pending: usize,
    registered: usize,
    reserved_bytes: usize,
}

impl OwnedArrayLimits {
    /// Builds explicit quotas, returning `None` when any bound is zero.
    pub const fn new(pending: usize, registered: usize, reserved_bytes: usize) -> Option<Self> {
        if pending == 0 || registered == 0 || reserved_bytes == 0 {
            None
        } else {
            Some(Self {
                pending,
                registered,
                reserved_bytes,
            })
        }
    }

    /// Maximum simultaneously unregistered allocations.
    pub const fn pending_quota(self) -> usize {
        self.pending
    }

    /// Maximum simultaneously registered weak-owner records.
    pub const fn registered_quota(self) -> usize {
        self.registered
    }

    /// Maximum combined pending-plus-registered reserved payload bytes.
    pub const fn reserved_bytes_quota(self) -> usize {
        self.reserved_bytes
    }
}

impl Default for OwnedArrayLimits {
    fn default() -> Self {
        Self {
            pending: 8,
            registered: 64,
            reserved_bytes: 1 << 20,
        }
    }
}

/// Read-only snapshot of one VM's owned-array scope accounting.
///
/// Returned by [`owned_array_runtime_stats`]; `None` means the scope
/// directory is absent (never initialized on this scope, or destroyed by
/// terminal reset), never a stale record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnedArrayRuntimeStats {
    /// Currently unregistered allocations.
    pub pending: usize,
    /// Currently registered weak-owner records.
    pub registered: usize,
    /// Currently reserved payload bytes (pending plus registered).
    pub reserved_bytes: usize,
    /// Records inspected by the most recent safety point.
    pub last_scanned: usize,
    /// Cumulative records inspected across safety points.
    pub total_scanned: u64,
    /// Payloads synchronously closed by the most recent safety point.
    pub last_closed: usize,
    /// Cumulative payloads synchronously closed across safety points.
    pub total_closed: u64,
}

/// Persistent per-VM configuration. Only copied limits live here: no
/// callable values, environments, cells or directory records.
struct OwnedArrayConfig {
    limits: OwnedArrayLimits,
}

struct OwnedArrayEntry {
    handle: ResourceHandle,
    root: ResourceCaptureRoot,
    reserved_bytes: usize,
}

/// Scope-local directory. Destroyed by terminal reset; every accounting
/// field below is recreated from zero on the next scope.
#[derive(Default)]
struct OwnedArrayDirectory {
    entries: Vec<OwnedArrayEntry>,
    index: HashMap<ResourceHandle, usize>,
    pending: HashMap<ResourceHandle, usize>,
    cursor: usize,
    payload_bytes: usize,
    last_scanned: usize,
    total_scanned: u64,
    last_closed: usize,
    total_closed: u64,
}

/// Explicitly opts one VM into the owned-array runtime.
///
/// An identical setup is idempotent. Changing the limits while the scope
/// tracks allocations (pending entries, registered records or reserved
/// bytes) is rejected so live accounting can never be reinterpreted under a
/// new budget; with a clean scope the change applies. When the Core callable
/// identity maintenance policy is disabled, the existing budget-4 policy is
/// enabled; an already enabled caller policy is preserved untouched.
pub fn initialize_typescript_owned_arrays(
    machine: &mut Vm,
    limits: OwnedArrayLimits,
) -> VmResult<()> {
    let tracked_allocations = {
        let context = machine.host_context();
        context
            .scope_state::<OwnedArrayDirectory>()
            .is_some_and(|directory| {
                !directory.pending.is_empty()
                    || !directory.entries.is_empty()
                    || directory.payload_bytes != 0
            })
    };
    let current = {
        let context = machine.host_context();
        context
            .module_state::<OwnedArrayConfig>()
            .map(|config| config.limits)
    };
    match current {
        Some(current) if current == limits => {}
        Some(_) if tracked_allocations => {
            return Err(VmError::HostError(
                "owned array limits cannot change while the scope tracks allocations".to_owned(),
            ));
        }
        _ => {
            machine
                .host_context()
                .set_module_state(OwnedArrayConfig { limits });
        }
    }
    if machine.callable_identity_maintenance_stats().is_none() {
        machine.set_callable_identity_maintenance(vm::CallableIdentityMaintenanceConfig::new(4));
    }
    Ok(())
}

/// Reads the current scope accounting without exposing roots or authority.
///
/// Returns `None` after terminal reset destroyed the directory (or when no
/// scope state was ever created), never a stale snapshot.
pub fn owned_array_runtime_stats(machine: &mut Vm) -> Option<OwnedArrayRuntimeStats> {
    let context = machine.host_context();
    context
        .scope_state::<OwnedArrayDirectory>()
        .map(|directory| OwnedArrayRuntimeStats {
            pending: directory.pending.len(),
            registered: directory.entries.len(),
            reserved_bytes: directory.payload_bytes,
            last_scanned: directory.last_scanned,
            total_scanned: directory.total_scanned,
            last_closed: directory.last_closed,
            total_closed: directory.total_closed,
        })
}

fn require_limits(call: &mut OwnedHostCall<'_>) -> VmResult<OwnedArrayLimits> {
    let context = call.vm().host_context();
    context
        .module_state::<OwnedArrayConfig>()
        .map(|config| config.limits)
        .ok_or_else(|| {
            VmError::HostError("typescript owned arrays are not initialized for this VM".to_owned())
        })
}

fn require_source_certification(call: &OwnedHostCall<'_>) -> VmResult<()> {
    let certified = call
        .vm_ref()
        .capture_certification_stats()
        .is_some_and(|stats| stats.valid);
    if certified {
        Ok(())
    } else {
        Err(VmError::HostError(
            "owned array allocation requires a source-certified program".to_owned(),
        ))
    }
}

fn decode_candidate(call: &OwnedHostCall<'_>, index: usize) -> VmResult<ResourceHandle> {
    let raw = match call.arg(index) {
        Some(Value::Int(raw)) => *raw,
        _ => {
            return Err(VmError::TypeMismatch(
                "owned array candidate must be a resource handle",
            ));
        }
    };
    let raw = u64::try_from(raw)
        .map_err(|_| VmError::TypeMismatch("owned array candidate must be a resource handle"))?;
    ResourceHandle::from_raw(raw).map_err(|error| VmError::HostError(error.to_string()))
}

fn decode_resize_length(value: &Value) -> VmResult<usize> {
    match value {
        Value::Int(raw) => usize::try_from(*raw).map_err(|_| {
            VmError::HostError(
                "owned array length must be a non-negative integer within range".to_owned(),
            )
        }),
        Value::Float(raw)
            if raw.is_finite()
                && raw.fract() == 0.0
                && *raw >= 0.0
                && *raw <= usize::MAX as f64 =>
        {
            Ok(*raw as usize)
        }
        _ => Err(VmError::HostError(
            "owned array length must be a finite non-negative integer".to_owned(),
        )),
    }
}

fn is_number(value: &Value) -> bool {
    matches!(value, Value::Int(_) | Value::Float(_))
}

struct AllocateOwnedArrays;

impl HostOwnedFunction for AllocateOwnedArrays {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> VmResult<CallOutcome> {
        let limits = require_limits(call)?;
        require_source_certification(call)?;
        let values: Vec<Value> = match call.arg(0) {
            Some(Value::Array(values)) => values.iter().cloned().collect(),
            _ => {
                return Err(VmError::TypeMismatch(
                    "owned array allocation requires a numeric array",
                ));
            }
        };
        if values.iter().any(|value| !is_number(value)) {
            return Err(VmError::HostError("expected numeric array".to_owned()));
        }
        let bytes = values
            .capacity()
            .checked_mul(size_of::<Value>())
            .ok_or(VmError::TypeMismatch("owned array payload capacity"))?;
        {
            let mut context = call.vm().host_context();
            let directory = context
                .scope_state_or_insert_with(OwnedArrayDirectory::default)
                .map_err(|error| VmError::HostError(error.to_string()))?;
            if directory.pending.len() >= limits.pending {
                return Err(VmError::HostError(
                    "owned array pending quota exceeded".to_owned(),
                ));
            }
            if directory
                .payload_bytes
                .checked_add(bytes)
                .is_none_or(|total| total > limits.reserved_bytes)
            {
                return Err(VmError::HostError(
                    "owned array payload quota exceeded".to_owned(),
                ));
            }
        }
        let token = call
            .vm()
            .host_context()
            .push_resource(NumberArray(values))
            .map_err(|error| VmError::HostError(error.to_string()))?;
        {
            let mut context = call.vm().host_context();
            let directory = context
                .scope_state_mut::<OwnedArrayDirectory>()
                .ok_or(VmError::TypeMismatch("owned array scope state"))?;
            directory.pending.insert(token.handle(), bytes);
            directory.payload_bytes += bytes;
        }
        let raw = i64::try_from(token.handle().raw())
            .map_err(|_| VmError::TypeMismatch("owned array resource handle"))?;
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(raw))))
    }
}

struct RegisterOwnedArray;

impl HostOwnedFunction for RegisterOwnedArray {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> VmResult<CallOutcome> {
        let limits = require_limits(call)?;
        let candidate = decode_candidate(call, 0)?;
        let (handle, root) = {
            let context = call.vm().host_context();
            context
                .typed_resource_with_key::<NumberArray>(candidate, &array_key())
                .map_err(|error| VmError::HostError(error.to_string()))?;
            let capture = context
                .checked_move_resource_capture::<NumberArray>(&array_key())
                .map_err(|error| VmError::HostError(error.to_string()))?;
            let root = capture.root().clone();
            if capture.token().handle() != candidate
                || root.observe(&context) != CaptureRootState::Live
            {
                return Err(VmError::HostError(
                    "owned array registration owner mismatch".to_owned(),
                ));
            }
            (capture.token().handle(), root)
        };
        {
            let mut context = call.vm().host_context();
            let directory = context
                .scope_state_or_insert_with(OwnedArrayDirectory::default)
                .map_err(|error| VmError::HostError(error.to_string()))?;
            if directory.entries.len() >= limits.registered {
                return Err(VmError::HostError(
                    "owned array metadata quota exceeded".to_owned(),
                ));
            }
            if !directory.pending.contains_key(&handle) {
                return Err(VmError::HostError(
                    "owned array owner is not a pending allocation".to_owned(),
                ));
            }
            let bytes = directory.pending.remove(&handle).ok_or_else(|| {
                VmError::HostError("owned array pending allocation missing".to_owned())
            })?;
            directory.index.insert(handle, directory.entries.len());
            directory.entries.push(OwnedArrayEntry {
                handle,
                root,
                reserved_bytes: bytes,
            });
        }
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(1))))
    }
}

fn remove_directory_entry(directory: &mut OwnedArrayDirectory, position: usize) {
    if position >= directory.entries.len() {
        return;
    }
    let entry = directory.entries.swap_remove(position);
    directory.index.remove(&entry.handle);
    if let Some(swapped) = directory.entries.get(position) {
        directory.index.insert(swapped.handle, position);
    }
    directory.payload_bytes = directory.payload_bytes.saturating_sub(entry.reserved_bytes);
}

fn collect_owned_arrays(machine: &mut Vm) -> VmResult<usize> {
    let mut context = machine.host_context();
    let mut directory = context
        .take_scope_state::<OwnedArrayDirectory>()
        .unwrap_or_default();
    let result = (|| {
        let budget = directory.entries.len().min(4);
        let mut closed = 0usize;
        directory.last_scanned = 0;
        directory.last_closed = 0;
        for _ in 0..budget {
            if directory.entries.is_empty() {
                break;
            }
            directory.cursor %= directory.entries.len();
            directory.last_scanned += 1;
            directory.total_scanned = directory.total_scanned.saturating_add(1);
            let (handle, root, _) = {
                let entry = &directory.entries[directory.cursor];
                (entry.handle, entry.root.clone(), entry.reserved_bytes)
            };
            match context.typed_resource_with_key::<NumberArray>(handle, &array_key()) {
                Ok(_) => {}
                Err(error) => {
                    if let vm::HostContextErrorKind::Resource(resource_error) = error.kind() {
                        match resource_error.code() {
                            // Narrowly justified for this unchanged synchronous
                            // concrete NumberArray: an actual completed typed
                            // take (or a justified same-scope generation
                            // reuse) retires metadata here. AlreadyClosed is
                            // NOT a destruction proof for general resources.
                            ResourceErrorCode::ResourceAlreadyClosed
                            | ResourceErrorCode::ResourceStale => {
                                let position = directory.cursor;
                                remove_directory_entry(&mut directory, position);
                                continue;
                            }
                            ResourceErrorCode::ResourceAccessConflict => {
                                directory.cursor += 1;
                                continue;
                            }
                            _ => {}
                        }
                    }
                    return Err(VmError::HostError(error.to_string()));
                }
            }
            if root.observe(&context) == CaptureRootState::Released {
                context
                    .typed_resource_with_key::<NumberArray>(handle, &array_key())
                    .map_err(|error| VmError::HostError(error.to_string()))?;
                let progress = context
                    .close_resource::<NumberArray>(handle, ResourceCloseReason::Requested)
                    .map_err(|error| VmError::HostError(error.to_string()))?;
                if progress == CloseProgress::Ready {
                    let position = directory.cursor;
                    remove_directory_entry(&mut directory, position);
                    closed += 1;
                    directory.last_closed += 1;
                    continue;
                }
            }
            directory.cursor += 1;
        }
        directory.total_closed = directory.total_closed.saturating_add(closed as u64);
        Ok(closed)
    })();
    context
        .scope_state_or_insert_with(|| directory)
        .map_err(|error| VmError::HostError(error.to_string()))?;
    result
}

struct CollectOwnedArrays;

impl HostOwnedFunction for CollectOwnedArrays {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> VmResult<CallOutcome> {
        require_limits(call)?;
        let closed = collect_owned_arrays(call.vm())?;
        let closed = i64::try_from(closed)
            .map_err(|_| VmError::TypeMismatch("owned array collection count"))?;
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(closed))))
    }
}

struct ResizeOwnedArray;

impl HostOwnedFunction for ResizeOwnedArray {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> VmResult<CallOutcome> {
        let limits = require_limits(call)?;
        let candidate = decode_candidate(call, 0)?;
        let new_len = match call.arg(1) {
            Some(value) => decode_resize_length(value)?,
            None => {
                return Err(VmError::TypeMismatch(
                    "owned array resize requires a length",
                ));
            }
        };
        let (position, old_reserved) = {
            let context = call.vm().host_context();
            let directory = context
                .scope_state::<OwnedArrayDirectory>()
                .ok_or_else(|| {
                    VmError::HostError("owned array is not a registered allocation".to_owned())
                })?;
            let position = *directory.index.get(&candidate).ok_or_else(|| {
                VmError::HostError("owned array is not a registered allocation".to_owned())
            })?;
            let entry = directory.entries.get(position).ok_or_else(|| {
                VmError::HostError("owned array is not a registered allocation".to_owned())
            })?;
            if entry.handle != candidate {
                return Err(VmError::HostError(
                    "owned array is not a registered allocation".to_owned(),
                ));
            }
            (position, entry.reserved_bytes)
        };
        let token = call
            .vm()
            .host_context()
            .typed_resource_with_key::<NumberArray>(candidate, &array_key())
            .map_err(|error| VmError::HostError(error.to_string()))?;
        let capacity = {
            let context = call.vm().host_context();
            let payload = context
                .resource(&token)
                .map_err(|error| VmError::HostError(error.to_string()))?;
            payload.get().0.capacity()
        };
        if new_len <= capacity {
            let mut context = call.vm().host_context();
            let mut payload = context
                .resource_mut(&token)
                .map_err(|error| VmError::HostError(error.to_string()))?;
            payload.get().0.resize(new_len, Value::Int(0));
        } else {
            let requested = new_len
                .checked_mul(size_of::<Value>())
                .ok_or(VmError::TypeMismatch("owned array growth size"))?;
            let other_bytes = {
                let context = call.vm().host_context();
                let directory = context
                    .scope_state::<OwnedArrayDirectory>()
                    .ok_or(VmError::TypeMismatch("owned array scope state"))?;
                directory
                    .payload_bytes
                    .checked_sub(old_reserved)
                    .ok_or(VmError::TypeMismatch("owned array byte accounting"))?
            };
            if other_bytes
                .checked_add(requested)
                .is_none_or(|total| total > limits.reserved_bytes)
            {
                return Err(VmError::HostError(
                    "owned array payload quota exceeded".to_owned(),
                ));
            }
            let mut staged = Vec::new();
            staged
                .try_reserve_exact(new_len)
                .map_err(|_| VmError::HostError("owned array growth refused".to_owned()))?;
            let reserved = staged
                .capacity()
                .checked_mul(size_of::<Value>())
                .ok_or(VmError::TypeMismatch("owned array growth capacity"))?;
            let new_total = other_bytes
                .checked_add(reserved)
                .filter(|total| *total <= limits.reserved_bytes)
                .ok_or_else(|| {
                    VmError::HostError("owned array payload quota exceeded".to_owned())
                })?;
            {
                let context = call.vm().host_context();
                let payload = context
                    .resource(&token)
                    .map_err(|error| VmError::HostError(error.to_string()))?;
                staged.extend(payload.get().0.iter().take(new_len).cloned());
            }
            staged.resize(new_len, Value::Int(0));
            {
                let mut context = call.vm().host_context();
                context
                    .resource_mut(&token)
                    .map_err(|error| VmError::HostError(error.to_string()))?
                    .get()
                    .0 = staged;
            }
            {
                let mut context = call.vm().host_context();
                let directory = context
                    .scope_state_mut::<OwnedArrayDirectory>()
                    .ok_or(VmError::TypeMismatch("owned array scope state"))?;
                let entry = directory.entries.get_mut(position).ok_or_else(|| {
                    VmError::HostError("owned array is not a registered allocation".to_owned())
                })?;
                if entry.handle != candidate {
                    return Err(VmError::HostError(
                        "owned array is not a registered allocation".to_owned(),
                    ));
                }
                entry.reserved_bytes = reserved;
                directory.payload_bytes = new_total;
            }
        }
        let length = i64::try_from(new_len).map_err(|_| {
            VmError::HostError("owned array length exceeds numeric range".to_owned())
        })?;
        Ok(CallOutcome::Return(CallReturn::one(Value::Int(length))))
    }
}

struct AllocateOwnedArraysFactory;

impl HostOwnedAdapterFactory for AllocateOwnedArraysFactory {
    fn create(&self, _context: OwnedHostContext<'_>) -> Box<dyn HostOwnedFunction> {
        Box::new(AllocateOwnedArrays)
    }
}

struct RegisterOwnedArrayFactory;

impl HostOwnedAdapterFactory for RegisterOwnedArrayFactory {
    fn create(&self, _context: OwnedHostContext<'_>) -> Box<dyn HostOwnedFunction> {
        Box::new(RegisterOwnedArray)
    }
}

struct CollectOwnedArraysFactory;

impl HostOwnedAdapterFactory for CollectOwnedArraysFactory {
    fn create(&self, _context: OwnedHostContext<'_>) -> Box<dyn HostOwnedFunction> {
        Box::new(CollectOwnedArrays)
    }
}

struct ResizeOwnedArrayFactory;

impl HostOwnedAdapterFactory for ResizeOwnedArrayFactory {
    fn create(&self, _context: OwnedHostContext<'_>) -> Box<dyn HostOwnedFunction> {
        Box::new(ResizeOwnedArray)
    }
}

static ALLOCATE_FACTORY: AllocateOwnedArraysFactory = AllocateOwnedArraysFactory;
static REGISTER_FACTORY: RegisterOwnedArrayFactory = RegisterOwnedArrayFactory;
static COLLECT_FACTORY: CollectOwnedArraysFactory = CollectOwnedArraysFactory;
static RESIZE_FACTORY: ResizeOwnedArrayFactory = ResizeOwnedArrayFactory;

fn allocate_schema() -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        "__compat_array_owner::allocate",
        vec![HostParamSchema::value(
            "initial",
            HostTypeSchema::Array(Box::new(HostTypeSchema::Number)),
        )],
        HostTypeSchema::Resource(array_key()),
    )
}

fn register_schema() -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        "__compat_array_owner::register",
        vec![HostParamSchema::with_passing(
            "candidate",
            HostTypeSchema::Resource(array_key()),
            HostParamPassing::Borrow,
        )],
        HostTypeSchema::Number,
    )
}

fn collect_schema() -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        "__compat_array_owner::collect",
        vec![],
        HostTypeSchema::Number,
    )
}

fn resize_schema() -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        "__compat_array_owner::resize",
        vec![
            HostParamSchema::with_passing(
                "candidate",
                HostTypeSchema::Resource(array_key()),
                HostParamPassing::BorrowMut,
            ),
            HostParamSchema::value("length", HostTypeSchema::Number),
        ],
        HostTypeSchema::Number,
    )
}

fn allocate_descriptor() -> HostFunctionDescriptor {
    let schema = allocate_schema();
    let effects = guest_resource_effects(&schema);
    HostFunctionDescriptor {
        schema,
        binding: HostBindingDescriptor {
            kind: HostBindingKind::Owned,
        },
        adapter: HostAdapterDescriptor::Owned(&ALLOCATE_FACTORY),
        effects,
        resource_types: Vec::new(),
    }
}

fn register_descriptor() -> HostFunctionDescriptor {
    let schema = register_schema();
    let effects = guest_resource_effects(&schema);
    HostFunctionDescriptor {
        schema,
        binding: HostBindingDescriptor {
            kind: HostBindingKind::Owned,
        },
        adapter: HostAdapterDescriptor::Owned(&REGISTER_FACTORY),
        effects,
        resource_types: Vec::new(),
    }
}

fn collect_descriptor() -> HostFunctionDescriptor {
    let schema = collect_schema();
    let effects = guest_resource_effects(&schema);
    HostFunctionDescriptor {
        schema,
        binding: HostBindingDescriptor {
            kind: HostBindingKind::Owned,
        },
        adapter: HostAdapterDescriptor::Owned(&COLLECT_FACTORY),
        effects,
        resource_types: Vec::new(),
    }
}

fn resize_descriptor() -> HostFunctionDescriptor {
    let schema = resize_schema();
    let effects = guest_resource_effects(&schema);
    HostFunctionDescriptor {
        schema,
        binding: HostBindingDescriptor {
            kind: HostBindingKind::Owned,
        },
        adapter: HostAdapterDescriptor::Owned(&RESIZE_FACTORY),
        effects,
        resource_types: Vec::new(),
    }
}

fn owned_number_array_resource() -> HostResourceTypeMeta {
    HostResourceTypeMeta::of::<NumberArray>()
}

const OWNED_ARRAY_FUNCTIONS: &[fn() -> HostFunctionDescriptor] = &[
    allocate_descriptor,
    register_descriptor,
    collect_descriptor,
    resize_descriptor,
];

const OWNED_ARRAY_RESOURCES: &[fn() -> HostResourceTypeMeta] = &[owned_number_array_resource];

/// Optional production Compat module for owned numeric arrays.
///
/// The module carries the four `__compat_array_owner` owned-dispatch
/// functions plus the shared `compat.number_array` resource key. It is
/// composed with the unchanged manual `arrays::*` module by embedding
/// callers: build one effective catalog from both modules, compile the
/// guest against it, initialize the VM, then install both modules from
/// that catalog into a restricted registry.
pub fn typescript_owned_array_host_module() -> HostModuleDescriptor {
    HostModuleDescriptor {
        name: "compat-owned-arrays",
        functions: OWNED_ARRAY_FUNCTIONS,
        resources: OWNED_ARRAY_RESOURCES,
    }
}

#[cfg(test)]
#[path = "array_owned_tests.rs"]
mod owned_tests;
