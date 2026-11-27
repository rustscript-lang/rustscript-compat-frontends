//! Non-moving mark/sweep storage for tables, closures, shared cells, and bare
//! userdata.
//!
//! Closure, cell, and userdata nodes use the same slots, edges, roots, and
//! sweep as tables. A private field id is not a Lua number. Internal links
//! are strong. Only a table entry follows that table's weak mode. An object
//! key stores the slot, generation, and kind. A closure or userdata key is
//! that record with its own kind. It is not a private field id and not a
//! second edge table.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use vm::{
    CallOutcome, CallReturn, CallableKind, CallableTarget, CallableValue, HostContextError,
    HostFunctionRegistry, HostFunctionSchema, HostImportSchema, HostParamSchema, HostTypeSchema,
    Value, Vm, VmError, VmResult,
};

use super::handle::{ClosureRef, HandleKind, TableRef, UpvalueRef, UserdataRef};
use super::{
    PendingFinalizerAction, PendingFinalizerNotCallable, TakenPendingFinalizer,
    pending_finalizer_scalar_reject,
};

#[path = "table/mark.rs"]
mod mark;
#[path = "table/pending_close.rs"]
mod pending_close;
#[path = "table/sweep.rs"]
mod sweep;
use mark::MarkSession;
#[cfg(test)]
use mark::{ParkedMark, PausedOp, PendingScan};
use pending_close::{PendingCloseSession, PendingCloseView};
use sweep::SweepSession;
pub use sweep::{ObserveFault, PublishedRead};

static NEXT_DOMAIN: AtomicU64 = AtomicU64::new(1);

/// Closure-to-cell field. It is keyed only on a closure node. Field id 0 on a
/// table remains `__mode`. This id is not a Lua number.
const UPVALUE_EDGE: u32 = 0;
/// Cell current-value field. It is keyed only on an upvalue node. A missing
/// edge is nil. This id is not a Lua number.
const CURRENT_EDGE: u32 = 0;
/// Table-to-cell field. Not `__mode`, and not the per-closure field that
/// points at that closure's own node. This id is not a Lua number.
const CELL_EDGE: u32 = 2;

/// Hard caps. Edges and root slots are counted separately so fields cannot
/// grow past the edge cap by avoiding the node cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapLimits {
    /// Slot-vector length, including free and retired slots.
    pub nodes: u32,
    /// Simultaneous explicit root slots.
    pub roots: u32,
    /// Live table-entry records, including one scalar field value.
    pub edges: u32,
    /// Highest generation a slot may hold. The next release retires it.
    pub max_generation: u32,
}

impl HeapLimits {
    /// Defaults for an unwired production heap. Tests pass tighter limits.
    pub const fn production() -> Self {
        Self {
            nodes: 4096,
            roots: 256,
            edges: 8192,
            max_generation: u32::MAX,
        }
    }
}

/// Why a heap operation was refused. Refusal does not mutate live occupancy
/// except `rollback`, which releases a node the caller already reserved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeapError {
    /// Node slot storage is at its cap.
    NodeCapacity,
    /// Root slot storage is at its cap.
    RootCapacity,
    /// Edge storage is at its cap.
    EdgeCapacity,
    /// The slot cannot be reused without wrapping generation through zero.
    GenerationExhausted,
    /// The handle's slot or generation is not a live node in this heap.
    StaleHandle,
    /// The handle was allocated by a different heap domain.
    WrongDomain,
    /// The handle kind is not a table.
    WrongKind,
    /// The bound token has the right type and key, but its payload names a
    /// different object of that type.
    TokenMismatch,
    /// `write_root` ran with no open frame.
    NoRootFrame,
    /// The frame id is not the current top frame, or its index is out of range.
    StaleFrame,
    /// The resource handle does not fit in a positive VM integer.
    CarrierOutOfRange,
    /// A NaN is not a table key. LuaJIT reports this on write; a read is nil.
    NanIndex,
    /// `next` was given a key that is not stored. This is not the end.
    NextCursorMissing,
    /// A cursor or stored entry has no guest key. This is not the end.
    NextUnsupported,
    /// This scope already has a heap.
    AlreadyInstalled,
    /// Execution-scope or resource-table failure. `publish` rolls the reserved
    /// node back before returning this error.
    Scope(String),
    /// A parked mark does not allow this edit. The heap is unchanged.
    MarkDenied,
    /// A mark is already active, so a second start or private collect cannot run.
    MarkActive,
    /// A published sweep tail is still stored, so this mutation or collect cannot run.
    SweepPending,
    /// No heap is installed, or no published sweep tail can accept this resume.
    SweepDenied,
    /// The closure-slot callable carrier does not admit an environment with
    /// captures. Core's empty Lua environment container is not a capture.
    CallableHasEnvironment,
    /// This live closure generation already holds one carrier. Rebinding,
    /// including with a different Arc, is refused and the stored Arc stays.
    CarrierAlreadyBound,
    /// This live closure generation holds no callable carrier.
    CarrierNotBound,
}

impl HeapError {
    pub(super) fn from_scope(error: HostContextError) -> Self {
        Self::Scope(error.to_string())
    }
}

fn guest_import_fault(error: HeapError) -> VmError {
    VmError::HostError(format!("lua heap: {error:?}"))
}

/// The raw Lua name of the one-layer call field on a metatable.
///
/// It is stored as ordinary UTF-8, not a numeric field id. The one-argument
/// admission callback and the two-argument raw-field handoff share this name.
const RAW_CALL_FIELD: &str = "__call";

/// Checks the exact compiled shape a controlled guest callable must have and
/// returns its real capture count from this VM's program.
///
/// Both private guest imports use this. The count is read from the prototype
/// the invoking VM compiled, never from a caller-supplied number. A prototype
/// that is not one compiled Lua closure script function is a host error, so a
/// non-closure value cannot reach either heap admission.
fn script_callable_capture_slots(vm: &Vm, callable: &Arc<CallableValue>) -> VmResult<usize> {
    let prototype = vm
        .program()
        .callable_prototypes
        .get(callable.prototype_id as usize)
        .ok_or_else(|| {
            VmError::HostError(format!(
                "the guest callable names prototype {} outside this VM program",
                callable.prototype_id
            ))
        })?;
    if prototype.kind != CallableKind::Closure
        || !matches!(prototype.target, CallableTarget::ScriptFunction(_))
    {
        return Err(VmError::HostError(
            "the guest callable prototype is not a compiled Lua script function".to_string(),
        ));
    }
    Ok(prototype.capture_slots.len())
}

/// The only function that admits a guest callable.
///
/// It is private and reachable only through the import registered by
/// [`LuaHeap::register_guest_callable_import`]. The VM dispatch supplies the
/// guest argument, so the callable is one value this VM's own execution
/// produced or received; there is no separate `(Arc, VM)` or `(Value, VM)`
/// entry to call.
///
/// The capture count comes from this VM's compiled prototype for
/// `callable.prototype_id`, not from a caller-supplied number. The container
/// bit is not used: Core attaches an empty `CallableEnvironment` to every Lua
/// closure, including a chunk-level function with no captures. A prototype
/// with a real capture returns `false` and leaves the heap unchanged. A
/// zero-capture callable is allocated a fresh closure generation and stored
/// there; `true` reports that admission.
fn admit_guest_callable(vm: &mut Vm, args: &[Value]) -> VmResult<CallOutcome> {
    let [value] = args else {
        return Err(VmError::HostError(
            "the guest callable import expects exactly one argument".to_string(),
        ));
    };
    let Value::Callable(callable) = value else {
        return Err(VmError::HostError(
            "the guest callable import expects a callable value".to_string(),
        ));
    };
    if callable.kind != CallableKind::Closure {
        return Err(VmError::HostError(
            "the guest callable import only admits a Lua closure".to_string(),
        ));
    }
    let capture_slots = script_callable_capture_slots(vm, callable)?;
    let callable = Arc::clone(callable);
    if capture_slots != 0 {
        return Ok(CallOutcome::Return(CallReturn::one(Value::Bool(false))));
    }
    let mut host = vm.host_context();
    let closure =
        super::with_heap(&mut host, LuaHeap::alloc_closure).map_err(guest_import_fault)?;
    super::with_heap(&mut host, |heap| {
        heap.store_guest_callable(closure, callable, capture_slots)
    })
    .map_err(guest_import_fault)?;
    Ok(CallOutcome::Return(CallReturn::one(Value::Bool(true))))
}

/// The only function that hands one guest callable into an existing raw
/// `__call` record.
///
/// It is private and reachable only through the import registered by
/// [`LuaHeap::register_guest_callable_field_import`]. The two arguments are the
/// receiver's issued table carrier and the exact guest callable. The owner is
/// checked by [`LuaHeap::guest_class`], so a same-shaped ordinary array is not
/// the table. The callable is checked against this VM's compiled prototype and
/// must have zero captures.
///
/// The result is `true` only when the existing record was replaced by a fresh
/// closure generation that owns the exact callable Arc and the record names
/// that generation. A missing record, an unsupported raw value, a captured
/// callable, a non-issued owner, a saved mark, a published Release tail, or a
/// full node vector returns `false`; a false result does not change the record,
/// the node vector, or any generation. A host scope failure is a host error.
///
/// This is a direct Lua producer, not an arbitrary host injection check. The
/// guest argument is the issuance evidence; Core's `Vm::owns_callable` is not
/// visible here, so this entry does not claim that a callable returned by some
/// host belongs to this VM.
fn admit_guest_callable_to_raw_call(vm: &mut Vm, args: &[Value]) -> VmResult<CallOutcome> {
    let [owner, target] = args else {
        return Err(VmError::HostError(
            "the raw call field import expects exactly two arguments".to_string(),
        ));
    };
    let Value::Callable(target) = target else {
        return Ok(CallOutcome::Return(CallReturn::one(Value::Bool(false))));
    };
    if target.kind != CallableKind::Closure {
        return Ok(CallOutcome::Return(CallReturn::one(Value::Bool(false))));
    }
    let capture_slots = match script_callable_capture_slots(vm, target) {
        Ok(capture_slots) => capture_slots,
        Err(_) => return Ok(CallOutcome::Return(CallReturn::one(Value::Bool(false)))),
    };
    if capture_slots != 0 {
        return Ok(CallOutcome::Return(CallReturn::one(Value::Bool(false))));
    }
    let callable = Arc::clone(target);
    let mut host = vm.host_context();
    match super::with_heap(&mut host, |heap| {
        heap.handoff_guest_callable_to_raw_call(owner, callable, capture_slots)
    }) {
        Ok(_) => Ok(CallOutcome::Return(CallReturn::one(Value::Bool(true)))),
        Err(HeapError::Scope(message)) => Err(guest_import_fault(HeapError::Scope(message))),
        Err(_) => Ok(CallOutcome::Return(CallReturn::one(Value::Bool(false)))),
    }
}

impl fmt::Display for HeapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NodeCapacity => write!(formatter, "lua heap node capacity"),
            Self::RootCapacity => write!(formatter, "lua heap root capacity"),
            Self::EdgeCapacity => write!(formatter, "lua heap edge capacity"),
            Self::GenerationExhausted => write!(formatter, "lua heap generation exhausted"),
            Self::StaleHandle => write!(formatter, "stale lua table handle"),
            Self::WrongDomain => write!(formatter, "lua table handle is from another heap"),
            Self::WrongKind => write!(formatter, "lua handle is not a table"),
            Self::TokenMismatch => write!(formatter, "typed token names another object"),
            Self::NoRootFrame => write!(formatter, "no lua root frame"),
            Self::StaleFrame => write!(formatter, "stale lua root frame"),
            Self::CarrierOutOfRange => write!(formatter, "lua resource carrier out of range"),
            Self::NanIndex => write!(formatter, "table index is NaN"),
            Self::NextCursorMissing => write!(formatter, "invalid key to next"),
            Self::NextUnsupported => write!(formatter, "next key is not a guest key"),
            Self::AlreadyInstalled => write!(formatter, "lua heap already installed"),
            Self::Scope(message) => write!(formatter, "lua heap scope error: {message}"),
            Self::MarkDenied => write!(formatter, "lua mark denied this edit"),
            Self::MarkActive => write!(formatter, "lua mark is already active"),
            Self::SweepPending => write!(formatter, "lua sweep is still pending"),
            Self::SweepDenied => write!(formatter, "lua sweep denied this resume"),
            Self::CallableHasEnvironment => {
                write!(formatter, "lua closure carrier callable has an environment")
            }
            Self::CarrierAlreadyBound => {
                write!(formatter, "lua closure carrier already bound")
            }
            Self::CarrierNotBound => write!(formatter, "lua closure carrier is not bound"),
        }
    }
}

/// Occupancy and high-water storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapStats {
    /// Nodes reachable as live slots.
    pub live_nodes: u32,
    /// Root slots currently holding a handle.
    pub live_roots: u32,
    /// Table-entry records that still hold a value. A weak-cleared cursor is
    /// stored, but it is not a live entry.
    pub live_edges: u32,
    /// Slot vector length. Free and retired slots still occupy it.
    pub node_slots: u32,
    /// Root vector length.
    pub root_slots: u32,
    /// Edge vector length, including weak-cleared cursors that no longer hold a
    /// value. Quota uses this storage, not `live_edges`.
    pub edge_slots: u32,
    /// Completed collections.
    pub collections: u32,
    /// Nodes marked by the latest collection.
    pub last_marked: u32,
    /// High-water slot storage.
    pub peak_nodes: u32,
    /// High-water root storage.
    pub peak_roots: u32,
    /// High-water edge storage.
    pub peak_edges: u32,
}

/// Phase of a saved published tail. Callers use this instead of `Debug` text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SavedTailPhase {
    /// Dead slots are still being released.
    Release,
    /// Occupied root slots are being counted.
    Roots,
    /// The tail recorded its report and is finished.
    Done,
}

/// Cursor and accounted roots of one saved tail. Reading this does not scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SavedTailState {
    pub phase: SavedTailPhase,
    pub cursor: usize,
    pub live_roots: u32,
    pub dead_len: usize,
}

/// Result of one collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CollectReport {
    /// Live nodes after the sweep.
    pub live_nodes: u32,
    /// Nodes marked from explicit roots, including nodes reached by edges.
    pub marked_nodes: u32,
    /// Root slots still holding a live handle.
    pub live_roots: u32,
}

/// Accounting for one top frame removed during a saved published tail.
///
/// Each field is this retirement only. None of them is a scan unit, and none
/// of them replaces the cycle report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FrameRetirement {
    /// The tail was still releasing dead slots, so the dead cursor stayed put.
    pub(super) release_phase: bool,
    /// Scanned slots of this frame. Zero in Release, and zero when the root
    /// cursor has not entered the frame.
    pub(super) prefix_slots: u32,
    /// `Some` tags in that prefix. A repeated handle counts once per slot.
    pub(super) dropped_live: u32,
}

/// Identity of one explicit root frame. Frames are a stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RootFrameId {
    index: u32,
    generation: u32,
}

impl RootFrameId {
    /// Parts a private IR argument can carry without the witness owning the frame.
    pub fn parts(self) -> (u32, u32) {
        (self.index, self.generation)
    }

    /// Rebuilds a frame id from those IR parts. A stale pair fails at use.
    pub fn from_parts(index: u32, generation: u32) -> Self {
        Self { index, generation }
    }
}

#[derive(Clone, Copy, Debug)]
enum SlotState {
    Free,
    Live { marked: bool },
    Retired,
}

/// One issued value bound to a slot for its current generation.
///
/// A table slot holds the VM array issued by [`LuaHeap::issue_carrier`]; a
/// closure slot holds the exact no-capture Core `Value::Callable` Arc that the
/// controlled `.lua` guest registration import admitted. The import derives
/// the capture count from the same VM's compiled prototype, so no caller can
/// pass a bare count. Neither variant is a mark edge, a root, or a Core
/// resource. [`LuaHeap::release_slot`] clears the field for the retired
/// generation, and a reused slot starts empty.
///
/// Reusing the existing carrier field keeps the one-carrier-per-generation
/// invariant and prevents a table and a callable from coexisting. The enum is
/// one tag plus the same one-word pointer, so the per-slot record grows by one
/// word, the same as an extra `Option<Arc<...>>` field would, without a second
/// field to reset on release and reuse.
enum SlotCarrier {
    Table(Arc<Vec<Value>>),
    Callable(Arc<CallableValue>),
}

struct Slot {
    generation: u32,
    kind: HandleKind,
    state: SlotState,
    /// Raw resource handle for this node. Ownership metadata only: the mark
    /// phase does not read it, so the token is not a GC root.
    token: Option<u64>,
    /// One carrier for this live generation. A table slot owns the VM array
    /// so a field can read it back while the node is live. A closure slot owns
    /// the exact Core script callable the private test entry registered. The
    /// field is identity only, not an object edge, and does not participate in
    /// marking. Release clears it before reuse.
    carrier: Option<SlotCarrier>,
    /// Optional metatable. Not a field key and not an edge record.
    metatable: Option<(u32, u32)>,
    /// This live userdata was selected by the private pending-finalizer scan.
    /// The Core token stays bound and the slot stays live until an explicit
    /// retirement control removes the record. A later ordinary mark retains
    /// the object and its metatable graph instead of releasing them. Weak
    /// values that name this userdata are still cleared because the object is
    /// unreachable from guest roots; weak keys keep the entry because the
    /// selected object is retained for the eventual finalizer consumer.
    pending_finalizer: bool,
    /// This generation was finalized without a queue record: either the
    /// explicit test retirement control consumed its pending record, or the
    /// selecting white-userdata pass found no raw `__gc` on its metatable.
    /// Selection must not pick the object again, and a later raw `__gc` on
    /// the same generation does not queue it. The flag never marks the object
    /// and never keeps it alive; it only makes a live marked generation
    /// value-dead for weak-value publication and for a hash value of a
    /// `k`-only weak table. Release and slot reuse clear the flag.
    finalizer_settled: bool,
    /// Previous node in this heap's live-userdata chain, with its exact
    /// generation. `None` at the head and for a slot that is not linked.
    live_userdata_prev: Option<(u32, u32)>,
    /// Next node in this heap's live-userdata chain, with its exact
    /// generation. `None` at the tail and for a slot that is not linked.
    live_userdata_next: Option<(u32, u32)>,
    /// This slot is linked in the live-userdata chain at its current
    /// generation. A new userdata links at the head; a release unlinks in
    /// constant work before the generation advances. Every other kind is
    /// false.
    live_userdata_linked: bool,
    /// Next slot in the current selecting pass's settlement journal, if this
    /// slot is a journal node. The links live on the slots, so one settlement
    /// is a constant number of slot writes and never grows or copies a vector.
    /// A link left by an earlier pass is unreachable: the head is reset to
    /// `None` when that pass starts or publishes, and a new append overwrites
    /// this field before the slot can join the current chain. Release does not
    /// read the journal; slot reuse clears both fields with the slot record.
    settlement_next: Option<u32>,
    /// Exact generation journalled by this node. Together with the slot index
    /// this is the identity the rollback re-checks before clearing
    /// `finalizer_settled`; zero means no current journal identity.
    settlement_generation: u32,
    /// Cached raw `__gc` lookup result when this table slot is a candidate
    /// metatable. Valid only with `pending_gc_generation` and
    /// `pending_gc_round`; a cache hit reads no edge record. A reused slot or
    /// a later mark round does not inherit it.
    pending_gc_generation: u32,
    pending_gc_round: u64,
    pending_gc_non_nil: bool,
    /// Head of this owner's edge chain. `None` means no edges. A reused owner
    /// starts with no chain.
    edge_head: Option<u32>,
    /// Tail of this owner's edge chain. New keys append here.
    edge_tail: Option<u32>,
    /// Direct `__mode` edge of a table, if that field is stored. Not a parsed
    /// mode, and not set for field 0 on a closure or cell.
    mode_edge: Option<u32>,
    /// Write version of the direct `__mode` record. Every store, in-place
    /// replacement, and unlink bumps it, so a late byte read cannot combine
    /// bytes from two writes.
    mode_epoch: u32,
    /// Generation whose parsed mode is stored for this slot. Zero means no
    /// parse for the current generation. A reused slot cannot inherit the
    /// mode of an older generation.
    mode_generation: u32,
    /// Mark round that wrote the parsed mode. The reset-sized mode vector is
    /// rebuilt each round, but the slot generation survives; a stamp from an
    /// earlier round is not a current observation of the direct field.
    mode_round: u64,
    /// Parsed weak bits for a slot outside the reset-sized `modes` vector.
    /// Written together with `mode_generation` and `mode_round`; that triple
    /// is the parse identity. The reset parse of an indexed slot stays in the
    /// vector; this slot field replaces the old record-keyed map, so a late
    /// parse neither inserts into a tree nor queries one.
    mode_weak: WeakMode,
    /// Current direct `__mode` record version inspected by this round, either
    /// by the reset walk or by a finished late read, including a read whose
    /// bytes name no weak side. This is not a weak-mode cache: `mode_generation`
    /// and `mode_round` above stay unset for a strong read. The triple only
    /// lets a first attachment skip a second read of the same field version. A
    /// later write bumps `mode_epoch` and is still registered and read late.
    mode_checked_generation: u32,
    mode_checked_round: u64,
    mode_checked_epoch: u32,
    /// This table is linked in the saved mark's late-mode parse queue. The
    /// links live on the slot, so a setter append and a release unlink are
    /// constant work and allocate nothing.
    late_mode_epoch: u64,
    late_mode_prev: Option<u32>,
    late_mode_next: Option<u32>,
    /// One coalesced deferred metatable-transition check for this owner.
    /// `old` is the first tuple and `new` is the latest tuple.
    late_owner_epoch: u64,
    late_owner_old: Option<(u32, u32)>,
    /// Old weak observation captured when the check was queued. The old
    /// metatable can be released before the queued check runs; this keeps the
    /// value/key bits that were actually observed then.
    late_owner_old_mode: Option<WeakMode>,
    late_owner_new: Option<(u32, u32)>,
    late_owner_prev: Option<u32>,
    late_owner_next: Option<u32>,
    /// Deleted-mode discovery stamp for a slot past the reset-sized sidecar.
    /// `discover_batch == 0` is unarmed. `discover_round` rejects a stamp from
    /// an earlier mark: batch numbers restart at one, and a same-generation
    /// metatable can outlive the mark that armed the stamp.
    discover_generation: u32,
    discover_batch: u32,
    discover_round: u64,
    /// Mark epoch in which this owner's ordinary scan opened a weak side.
    /// Zero means no current-cycle weak start; an older epoch is inert. Set on
    /// the target slot only, so a grown slot needs no vector entry.
    rescan_epoch: u64,
    /// This owner is linked in the current mark's rescan queue.
    rescan_queued: bool,
    /// Previous rescan-queue node. `None` at the head or when not queued.
    rescan_prev: Option<u32>,
    /// Next rescan-queue node. `None` at the tail or when not queued.
    rescan_next: Option<u32>,
    /// This marked target is linked in the current mark's deferred-replay
    /// queue. The links live on the slot, so a replay append is constant work
    /// and never pushes the shared Gray `Vec`. A tag from an older mark is
    /// inert; release unlinks the slot before reuse.
    late_target_epoch: u64,
    late_target_prev: Option<u32>,
    late_target_next: Option<u32>,
}

/// Empty slot reserved for one late table. It is not a handle and cannot be
/// cloned. The caller must commit the real token or release the slot.
pub(super) struct LateReservation {
    domain: u64,
    index: u32,
    generation: u32,
    spent: bool,
}

impl LateReservation {
    pub(super) fn table(&self) -> TableRef {
        TableRef::table(self.domain, self.index, self.generation)
    }
}

impl Drop for LateReservation {
    fn drop(&mut self) {
        debug_assert!(
            self.spent,
            "late reservation dropped without commit or release"
        );
    }
}

/// Bytes of a string or byte payload stored in a heap record. Capacity is not
/// part of the logical extent: only the stored length is. `usize` fits in
/// `u64` on every Rust target, so the conversion is exact.
fn stored_len(len: usize) -> u64 {
    len as u64
}

/// A new table-object key, with a table value when the value is itself a table.
/// A string value keeps `value` empty so the barrier follows only the key.
struct NewTableKeyInsert {
    key_slot: u32,
    key_generation: u32,
    value: Option<(u32, u32)>,
}

fn new_table_key_insert(key: &EdgeKey, value: &EdgeValue) -> Option<NewTableKeyInsert> {
    let EdgeKey::Object {
        slot,
        generation,
        kind: HandleKind::Table,
    } = key
    else {
        return None;
    };
    let value = match value {
        EdgeValue::Table { slot, generation } => Some((*slot, *generation)),
        EdgeValue::Scalar(Value::String(_)) => None,
        _ => return None,
    };
    Some(NewTableKeyInsert {
        key_slot: *slot,
        key_generation: *generation,
        value,
    })
}

fn key_payload(key: &EdgeKey) -> u64 {
    match key {
        EdgeKey::Utf8(text) => stored_len(text.len()),
        EdgeKey::Field(_) | EdgeKey::Object { .. } | EdgeKey::Number(_) => 0,
    }
}

fn value_payload(value: &EdgeValue) -> u64 {
    match value {
        EdgeValue::Scalar(Value::String(text)) => stored_len(text.len()),
        EdgeValue::Scalar(Value::Bytes(bytes)) => stored_len(bytes.len()),
        EdgeValue::Scalar(_)
        | EdgeValue::Table { .. }
        | EdgeValue::Closure { .. }
        | EdgeValue::Upvalue { .. }
        | EdgeValue::Userdata { .. } => 0,
    }
}

fn edge_payload(edge: &Edge) -> u64 {
    key_payload(&edge.key) + value_payload(&edge.value)
}

struct Edge {
    /// Field ids, object identities, string text, and binary64 numbers share
    /// this record. An object key is the live slot, generation, and kind. A
    /// string key is the complete UTF-8 text. A number key is canonical binary64
    /// bits, not a field id and not a collectable node. The owner is the chain
    /// that holds this record, not a field of the record.
    key: EdgeKey,
    value: EdgeValue,
}

/// One address-stable edge record. `next` is the owner-chain link while
/// occupied and the free-list link while free. Deleting a record does not
/// move any other record.
struct EdgeSlot {
    next: Option<u32>,
    edge: Option<Edge>,
    /// The saved mark has one deferred late-mode check for this record.
    /// A new write updates this record in place; an unlink removes it.
    late_epoch: u64,
    late_owner: u32,
    /// Latest table value written through this edge while the mode is
    /// pending. Only this target is offered after the parse.
    late_value: Option<(u32, u32)>,
    /// Table-object key of a new key insertion while the mode is pending.
    late_key: Option<(u32, u32)>,
    late_prev: Option<u32>,
    late_next: Option<u32>,
}

enum EdgeValue {
    Table {
        slot: u32,
        generation: u32,
    },
    /// A closure target in the same edge record. Not a guest table value.
    Closure {
        slot: u32,
        generation: u32,
    },
    /// A cell target in the same edge record. Not a guest table value.
    Upvalue {
        slot: u32,
        generation: u32,
    },
    /// A bare userdata target. Not a guest table and not a Core number.
    Userdata {
        slot: u32,
        generation: u32,
    },
    /// One guest scalar in the same record as a table target. Field 0 on a
    /// table is the only key whose string is read as `__mode`. A cell current
    /// edge may hold a boolean, binary64, string, or bytes; that value is not
    /// a key.
    Scalar(Value),
}

impl EdgeValue {
    fn object_slot(&self) -> Option<u32> {
        match self {
            Self::Table { slot, .. }
            | Self::Closure { slot, .. }
            | Self::Upvalue { slot, .. }
            | Self::Userdata { slot, .. } => Some(*slot),
            Self::Scalar(_) => None,
        }
    }

    /// A weak-cleared entry left in place so `next` can continue from that key.
    /// Nil is not otherwise stored: an explicit removal deletes the record.
    fn is_cursor_gap(&self) -> bool {
        matches!(self, Self::Scalar(Value::Null))
    }
}

/// Value stored in a shared cell's current edge.
///
/// A missing edge is nil and is not a variant. `None` from [`LuaHeap::read_cell`]
/// is that nil. An error is an invalid handle, not nil. A cell is not issued
/// as a table value. A scalar carries the stored [`Value`]. [`Value`] is not
/// `Copy` or `Eq`, so this enum is not either.
#[derive(Clone, Debug, PartialEq)]
pub enum CellValue {
    /// The cell currently holds a table.
    Table(TableRef),
    /// The cell currently holds a closure.
    Closure(ClosureRef),
    /// The cell currently holds a boolean, binary64, string, or bytes. Not a
    /// table key.
    Scalar(Value),
}

/// One explicit root. Table, closure, upvalue, and bare userdata roots share
/// the root vector.
///
/// Visible to the private host continuation. Not a public heap schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Rooted {
    Table(TableRef),
    Closure(ClosureRef),
    Upvalue(UpvalueRef),
    Userdata(UserdataRef),
}

impl Rooted {
    fn slot(self) -> u32 {
        match self {
            Self::Table(handle) => handle.slot(),
            Self::Closure(handle) => handle.slot(),
            Self::Upvalue(handle) => handle.slot(),
            Self::Userdata(handle) => handle.slot(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum EdgeKey {
    Field(u32),
    Object {
        slot: u32,
        generation: u32,
        kind: HandleKind,
    },
    /// Complete UTF-8 text, including interior NUL. Not a collectable table.
    Utf8(String),
    /// Canonical binary64 bits. Positive and negative zero share the zero
    /// bits. A NaN is never stored. This is not a field id and not a node.
    Number(u64),
}

impl EdgeKey {
    fn object(key: TableRef) -> Self {
        Self::identity(key.slot(), key.generation(), key.kind())
    }

    fn closure_key(key: ClosureRef) -> Self {
        Self::identity(key.slot(), key.generation(), key.kind())
    }

    fn userdata_key(key: UserdataRef) -> Self {
        Self::identity(key.slot(), key.generation(), key.kind())
    }

    /// One object-key record. Table, closure, and userdata keys differ by `kind`.
    fn identity(slot: u32, generation: u32, kind: HandleKind) -> Self {
        Self::Object {
            slot,
            generation,
            kind,
        }
    }

    /// One constructor for a stored Lua name. `__mode` is the reserved field
    /// id. Every other name keeps its full text. A lookup does not call this.
    fn named(name: &str) -> Self {
        if Self::is_mode_name(name) {
            Self::Field(LuaHeap::MODE_FIELD)
        } else {
            Self::Utf8(name.to_owned())
        }
    }

    /// The reserved mode name. Field id 0 is that key; a longer text is not.
    fn is_mode_name(name: &str) -> bool {
        name == "__mode"
    }

    /// One constructor for a numeric key. `+0` and `-0` share the zero bits.
    /// The caller refuses NaN before this runs.
    fn number(number: f64) -> Self {
        debug_assert!(!number.is_nan());
        let bits = if number == 0.0 { 0 } else { number.to_bits() };
        Self::Number(bits)
    }

    /// Compares a borrowed lookup name with this stored key.
    ///
    /// `__mode` matches the reserved field. Every other name matches stored
    /// UTF-8 of the same length, including an interior NUL. An object key
    /// does not match a name. This does not construct a key.
    fn matches_name(&self, name: &str) -> bool {
        if Self::is_mode_name(name) {
            matches!(self, Self::Field(field) if *field == LuaHeap::MODE_FIELD)
        } else {
            matches!(self, Self::Utf8(stored) if stored == name)
        }
    }
}

/// Cursor for [`LuaHeap::next_guest`]. `None` at the call site means start.
enum NextCursor<'a> {
    Name(&'a str),
    Number(u64),
    Object {
        slot: u32,
        generation: u32,
        kind: HandleKind,
    },
}

impl NextCursor<'_> {
    fn matches(&self, key: &EdgeKey) -> bool {
        match self {
            Self::Name(name) => key.matches_name(name),
            Self::Number(bits) => matches!(key, EdgeKey::Number(stored) if stored == bits),
            Self::Object {
                slot,
                generation,
                kind,
            } => matches!(
                key,
                EdgeKey::Object {
                    slot: stored_slot,
                    generation: stored_generation,
                    kind: stored_kind,
                } if stored_slot == slot
                    && stored_generation == generation
                    && stored_kind == kind
            ),
        }
    }
}

struct Frame {
    index: u32,
    generation: u32,
    start: u32,
    width: u32,
}

/// Counts the loops one collect actually ran.
///
/// Production keeps these counters. Each add sits on an edge, node slot, or
/// root slot the collector already visits. They are not a second graph.
/// `stats` and `take_scan_meter` do not collect, and they do not add to these
/// counts. A test query after the take is not part of the collection that was
/// taken.
///
/// Edge work stays in its own fields. Mark walks each marked owner's chain:
/// `outgoing_edge_checks` counts non-gap records on that chain, and
/// `outgoing_gap_checks` counts gaps actually visited there. Neither walk
/// builds an index. `owner_head_checks` counts owner-chain head reads during
/// mark and, when sweep runs, during the owner walk. `sweep_edge_checks` is
/// one visit of each occupied record when any live node is unmarked. It is
/// zero when every live node is marked. Sweep reads modes parsed during the
/// clear-mark slot walk and does not scan edges again to find `__mode`.
/// Adding those fields is not a total GC cost.
///
/// A metatable is not in an owner chain. `metatable_edge_checks` counts
/// stored metatable refs followed from marked table or userdata nodes. A
/// collect with none stored does not read those slots, so the field-edge
/// counters stay the same as a heap that never had a metatable.
///
/// `weak_mode_slot_checks` counts `mode_edge` pointer reads during the
/// clear-mark slot walk. `weak_mode_edge_checks` counts the edge records
/// those pointers actually open. A non-table field 0 has no mode pointer.
/// Heap node walks are `node_slot_visits`. Root-vector walks are
/// `root_slot_visits`. Token identity checks stay in `token_slot_checks`.
///
/// `scratch` records element counts the collector asked Rust to initialize,
/// copy, reserve, or push. Those are measured requests. They are not allocator
/// capacity, process RSS, or a byte size. A `Vec` may reserve more than the
/// request; this meter does not observe that. The ordinary meter also does
/// not include Core's close implementation, the process allocator, or wall
/// clock time.
///
/// Host collection still closes tokens before sweep. There is no public sweep
/// that skips that close.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanMeter {
    pub mark_passes: u64,
    pub outgoing_edge_checks: u64,
    /// Gaps visited while walking a marked owner's chain. Not included in
    /// `outgoing_edge_checks`.
    pub outgoing_gap_checks: u64,
    /// Owner-chain head reads. Mark reads one per marked node. Sweep, when it
    /// runs, reads one per node slot.
    pub owner_head_checks: u64,
    pub sweep_edge_checks: u64,
    /// Stored metatable references followed from a marked node.
    ///
    /// A metatable is an optional per-slot reference, not a field-edge record
    /// and not an integer key. It is not included in `outgoing_edge_checks`
    /// or `sweep_edge_checks`. A heap with no stored metatable reference does
    /// not read this path, so those collects add zero and leave the field-edge
    /// counts unchanged.
    pub metatable_edge_checks: u64,
    /// Bytes examined while parsing direct string modes (including a NUL terminator if present).
    pub weak_mode_bytes: u64,
    /// `mode_edge` pointer reads during the clear-mark slot walk.
    pub weak_mode_slot_checks: u64,
    /// Mode-edge records opened through those pointers. A missing pointer is
    /// not an edge read. A non-table field 0 is not a mode pointer.
    pub weak_mode_edge_checks: u64,
    /// Mode lookups by live metatable slot when a weak mode was indexed.
    pub weak_mode_lookups: u64,
    pub token_slot_checks: u64,
    pub token_closes: u64,
    /// Node-slot reads and writes during mark reset, root liveness, outgoing
    /// target checks, mark updates, pending-close enumeration, sweep
    /// selection, the marked bitmap, slot release, and dead-root liveness.
    pub node_slot_visits: u64,
    /// Root-slot reads during root enumeration and the dead-root sweep.
    pub root_slot_visits: u64,
    /// Ordinary field writes accepted while a mark is parked. Not a pulse unit,
    /// and not folded into the scan budget. A full collect leaves this at zero.
    pub parked_edge_writes: u64,
    /// Parked gray cursors advanced inside `unlink_at` after it has read the
    /// old successor. Not a pulse unit. A full collect leaves this at zero.
    pub parked_cursor_repairs: u64,
    /// Parked rescan cursors advanced inside `unlink_at` after it has read the
    /// old successor. Independent of `parked_cursor_repairs`. Not a pulse unit.
    /// A full collect leaves this at zero.
    pub parked_rescan_cursor_repairs: u64,
    /// Fixed root slots updated through `store_root` while a mark is parked.
    /// Not a pulse unit. A full collect leaves this at zero.
    pub parked_root_writes: u64,
    /// Newly marked targets pushed to the parked gray queue by a write barrier
    /// or a safepoint root batch. An already-marked target does not increment
    /// this. Not a pulse unit. A full collect leaves this at zero.
    pub parked_barrier_marks: u64,
    /// Entries accepted by a safepoint root batch, including duplicates.
    /// A refused batch does not increment this. Precheck slot reads are not
    /// counted here or in `node_slot_visits`. Not a pulse unit. A full collect
    /// leaves this at zero.
    pub parked_root_batch_accepts: u64,
    /// Chain-head reads of an owner whose weak-value scan already started and
    /// whose metatable later became strong. Not an ordinary first coloring,
    /// and not the setter that only recorded the request.
    pub owner_rescan_heads: u64,
    /// Setter requests that recorded one such owner. A repeat does not
    /// increment this. Not an outgoing edge visit and not a pulse unit.
    pub owner_rescan_schedules: u64,
    /// One capacity-slot read while looking for an owner of a metatable whose
    /// direct value-weak `__mode` was deleted. Free and retired slots count.
    /// This is not a live-node walk. The setter does not increment this.
    /// One probe is one pulse unit.
    pub mode_owner_probes: u64,
    /// Comparisons with an indexed deleted-mode stamp: at most one per
    /// capacity-slot probe. The setter does not compare stamps. Not a pulse
    /// unit in addition to the probe.
    pub mode_discover_compares: u64,
    /// Deleted-mode identity records written outside mode-cache init.
    /// Scalar cursor and batch changes do not write a record. Not a pulse unit.
    pub mode_discover_writes: u64,
    /// Matching direct `__mode` deletes that replaced this cycle's value-weak,
    /// key-strong cache with the missing-field mode. One per real unlink, not
    /// a slot walk and not a pulse unit.
    pub mode_cache_updates: u64,
    /// Direct `__mode` strings written on tables allocated after the indexed
    /// cache was sized. One per metatable slot linked into the late parse
    /// queue; a repeat write on the same slot reuses the queued marker and is
    /// `mode_late_cancels`. This is the setter-side append only: bytes and
    /// parsed stores are `weak_mode_bytes` and `mode_late_parsed`.
    pub mode_late_registrations: u64,
    /// O(1) slot/edge identity reads that route a setter or barrier through
    /// the late window. Not a pulse unit.
    pub mode_late_lookups: u64,
    /// A write replaced or unlinked a direct `__mode` record whose slot was
    /// already queued; the queued parse restarts from the new bytes. Not a
    /// pulse unit and not a second registration.
    pub mode_late_cancels: u64,
    /// Parsed late modes stored for one metatable slot generation. One per
    /// completed read, not one per indexed vector slot. No suffix is grown and
    /// no indexed prefix is copied; the record-keyed store moves no vector.
    pub mode_late_parsed: u64,
    /// New deferred owner checks or edge writes. Bounded by owner slots and
    /// edge records, never by mutation count.
    pub mode_late_events: u64,
    /// A repeat write updated an existing deferred owner check or edge write
    /// in place. This is the coalescing path that keeps the backlog bounded.
    pub mode_late_event_reuses: u64,
    /// Deferred owner checks and edge writes consumed after every parse.
    pub mode_late_deferred: u64,
    /// Live slot records read by the private pending-finalizer selection scan.
    /// One per live-userdata chain node in the separate selection phase; a
    /// node that cannot be selected still counts, while a table, closure,
    /// cell, free slot, and retired slot is never read by this walk. Not
    /// charged by an ordinary collection.
    pub pending_finalizer_scan_slots: u64,
    /// Userdata slots newly retained by the private pending-finalizer
    /// selection. One per record appended; an already pending userdata or a
    /// non-qualifying slot does not increment this. Not a callback count.
    pub pending_finalizers_selected: u64,
    /// Pending-finalizer records offered as host-retained edges by a mark.
    /// One per record read in the root phase, including a record whose exact
    /// slot is no longer valid. Not a pulse unit and not a selection.
    pub pending_finalizer_offers: u64,
    /// Edge records read while resolving the raw `__gc` field of a candidate
    /// metatable for the private pending-finalizer selection. One per record
    /// in the owner chain; a cache hit from the same mark round and exact
    /// metatable generation reads none. This is not a whole-owner scan.
    pub pending_finalizer_gc_edge_checks: u64,
    /// Raw `__gc` lookups started for the private pending-finalizer selection.
    /// One per uncached candidate metatable. A cache hit does not increment
    /// this and does not re-walk the chain.
    pub pending_finalizer_gc_lookups: u64,
    /// Test-only: settlement journal node appends performed by the selecting
    /// scan. One per settled identity and at most one per `pulse(1)` unit.
    /// No vector growth or prefix copy participates.
    #[cfg(test)]
    pub settlement_journal_appends: u64,
    /// Test-only: settlement journal nodes actually visited by a failed-pass
    /// rollback. One per node this pass appended; a successful publication
    /// and Release visit no node and leave this at zero.
    #[cfg(test)]
    pub settlement_journal_visits: u64,
    /// Test-only: pending-queue records visited by Release after a dead-slot
    /// release. The bounded per-slot invariant must keep this at zero; the
    /// ordinary release path reads only the released slot's own fields.
    #[cfg(test)]
    pub pending_release_queue_reads: u64,
    /// Test-only: stored pending-queue records moved by a queue extraction,
    /// including the extracted record itself. One front pop charges one moved
    /// record and displaces no successor; a positional removal charges its
    /// removed record. This is separate from the record-read count.
    #[cfg(test)]
    pub pending_queue_storage_moves: u64,
    pub scratch: ScanScratch,
}

/// Measured scratch requests for the collects since the previous take.
///
/// Initialization is a zeroed buffer. A copy duplicates an existing buffer.
/// A reserve is the capacity passed to `Vec::with_capacity`, not the
/// allocator's allocation. A push count is elements appended, not a capacity
/// upper bound above that count. Length fields are the sizes observed after
/// the request. This is not an observation of RSS or global allocation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanScratch {
    /// Slot-indexed modes zero-initialized on the first weak mode during mark.
    pub weak_modes_inited: u64,
    /// Work-stack pushes. One per newly marked node. Not a reserve.
    pub work_pushed: u64,
    /// Pending-close elements appended. Host mark only; pure collect is 0.
    pub pending_close_pushed: u64,
    /// Dead-slot indices appended during sweep selection, one per dead slot.
    /// 0 if sweep did not run. The push rides with that slot's selection unit.
    pub dead_slots_pushed: u64,
    /// Snapshot slot records appended when the edge filter runs, one per slot.
    /// Not a collective zero-fill. 0 when every live node is marked and the
    /// filter is skipped.
    pub sweep_bitmap_inited: u64,
    /// Slot state entries initialized for this mark's rescan bookkeeping: one
    /// per weak owner's first start, tagged on that target slot. A repeat or a
    /// setter reuses the tag and does not count another. Bounded by the node
    /// limit. 0 when this cycle never started a weak owner scan.
    /// Not the deleted-mode sidecar: that one is `mode_discover_inited`.
    pub owner_rescan_inited: u64,
    /// Deleted-mode sidecar slots pushed during mode-cache init. One per
    /// `step_mode_init` unit, and the same capacity as that wave's
    /// `weak_modes_inited`. A setter and a gray probe do not push. This is
    /// not `owner_rescan_inited`, which is a separate all-at-once zero of
    /// rescan bits.
    pub mode_discover_inited: u64,
    /// Owners linked into the rescan queue. One per owner per mark. A repeat
    /// setter does not link again and does not increment this.
    pub owner_rescan_pushed: u64,
}

/// A stored token identified while marking. Close checks this slot,
/// generation, and kind directly; it does not search the slot vector. The
/// kind selects the concrete typed resource that owns the raw handle.
#[derive(Clone, Copy, Debug)]
pub(super) struct DoomedToken {
    pub(super) raw: u64,
    pub(super) slot: u32,
    pub(super) generation: u32,
    pub(super) kind: HandleKind,
}

/// Parsed weak modes for one collect. Built during the clear-mark slot walk
/// from each table's direct `__mode` pointer, and resolved into one value per
/// snapshot record for sweep. A close error before sweep drops it with the
/// mark result, so the next collect parses again. It is not stored on the
/// heap.
pub(super) struct MarkSnapshot {
    modes: Option<Vec<WeakMode>>,
    /// Mark round that owns the parsed modes. A slot record carries the round
    /// that wrote its parse, so a reused generation or an earlier round
    /// cannot contribute a stale weak bit to this snapshot.
    round: u64,
}

/// Doomed host tokens, plus the modes sweep needs after those tokens close.
pub(super) struct DoomedMark {
    pub(super) tokens: Vec<DoomedToken>,
    pub(super) modes: MarkSnapshot,
}

#[derive(Clone, Copy, Default)]
struct WeakMode {
    keys: bool,
    values: bool,
}

/// Category of one guest value on this heap.
///
/// `Table` is only the array issued for a live slot. A structurally equal
/// array, a raw resource token, and a value issued by another heap are
/// `Unissued`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuestClass {
    /// Lua number.
    Number,
    /// Lua boolean.
    Boolean,
    /// Lua string.
    String,
    /// Issued table on this heap.
    Table(TableRef),
    /// Not an issued Lua value on this heap.
    Unissued,
}

/// One scope's table nodes, edges, roots, and scan state.
pub struct LuaHeap {
    domain: u64,
    limits: HeapLimits,
    slots: Vec<Slot>,
    free_list: Vec<u32>,
    edge_slots: Vec<EdgeSlot>,
    /// Free-list head of unoccupied edge records. Reused before the backing grows.
    edge_free_head: Option<u32>,
    /// Occupied edge records, including cursor gaps and excluding free slots.
    occupied_edges: u32,
    /// Occupied records whose value is not a cursor gap.
    live_edge_count: u32,
    /// Stored metatable refs. Zero means mark does not read metatable slots.
    metatable_edges: u32,
    roots: Vec<Option<Rooted>>,
    frames: Vec<Frame>,
    next_frame_generation: u32,
    collections: u32,
    last_marked: u32,
    peak_nodes: u32,
    peak_roots: u32,
    peak_edges: u32,
    scan: ScanMeter,
    /// Mark progress saved between pulses. Absent unless a session or the
    /// parked driver is active. Done is not stored for the parked caller.
    mark: Option<mark::MarkState>,
    /// Source of the per-mark rescan epoch stored on slot records. It never
    /// returns to zero, so an old tag cannot match a new mark after reuse.
    rescan_epoch: u64,
    /// Release/root continuation after a published sweep. Absent unless publish
    /// stopped with a dead slot. Done is not stored.
    sweep_tail: Option<sweep::SweepTail>,
    /// String and byte lengths stored in edge records. Not Vec capacity, not
    /// Core locals, and not a second scan of `edges`.
    payload_bytes: u64,
    /// The single userdata slot the typed issuer has reserved, before its
    /// Core token is published. The raw field appears only after the real
    /// Core registration succeeded. Ordinary borrows and the pending-close
    /// enumeration do not read this record, so a staged token is never a
    /// guest value. A completed mark retains a registered (`raw` is `Some`)
    /// reservation as a host edge, and the next normal issuance closes and
    /// releases it before reserving again.
    pending_userdata: Option<PendingUserdata>,
    /// Userdata slots retained by the private pending-finalizer selection.
    ///
    /// Each record names one exact live userdata generation and the Core raw
    /// handle that its slot still binds. Selection appends at most one record
    /// per userdata; an ordinary mark offers every record as a host-retained
    /// edge so no live token is closed or orphaned while it waits for a
    /// consumer. The explicit retirement test control removes the record and
    /// clears the slot flag; a later collection may then close the token and
    /// release the slot. A valid record's slot is retained and cannot reach
    /// Release, so there is no release-time queue repair.
    ///
    /// The storage is private: every record read goes through
    /// [`PendingQueue`]'s counted boundary, including the test-only release
    /// meter. No call site counts by hand.
    pending_finalizers: PendingQueue,
    /// Head and tail of the intrusive live-userdata chain, with the exact
    /// generation of each endpoint. A new userdata links at the head and a
    /// release unlinks its own node in constant work. The chain is the
    /// private pending selection order: allocation-reverse inside one cohort
    /// and no batch sort or vector rebuild. Release reads only the released
    /// slot's own links; it does not walk this chain.
    live_userdata_head: Option<(u32, u32)>,
    live_userdata_tail: Option<(u32, u32)>,
    /// Head and length of the current selecting pass's settlement journal.
    ///
    /// Each node is a live userdata slot whose `settlement_next` links the
    /// previous head and whose `settlement_generation` names the exact
    /// identity. An append is one flag write, one link write, one generation
    /// write, and one head write: a constant upper bound per `pulse(1)`
    /// settlement unit, with no vector growth or prefix copy. A successful
    /// publication drops the journal by resetting these fields; it does not
    /// visit the links, and Release does not read them. A failed pass walks
    /// exactly its own chain, clears those flags, and stops. That rollback
    /// work is charged to the failure path, never to the next mark's first
    /// pulse and never to Release.
    settlement_head: Option<u32>,
    #[cfg(test)]
    settlement_journal_len: u32,
    /// Owner chains opened by the legacy table rollback edge walk.
    /// Test-only observation: an empty userdata reservation rollback must
    /// leave this unchanged, so its work stays independent of the graph size.
    #[cfg(test)]
    rollback_owner_visits: u64,
    /// Slot indices actually indexed by sweep selection and by the
    /// registered-reservation identity guard. Test-only: the pushes record
    /// the real indexing points and never read another slot. Tests clear the
    /// trace after Mark, so a shared Select pulse trace starts at the cursor.
    /// Interior mutability keeps the identity guard's `&self` read-only.
    #[cfg(test)]
    selection_slot_trace: std::cell::RefCell<Vec<u32>>,
    /// Test-only: edge-record indices read while resolving a candidate
    /// metatable's raw `__gc`. Each push is one real record read; a cache hit
    /// adds none. The pairs are `(metatable_slot, edge_record_index)`.
    #[cfg(test)]
    pending_gc_edge_trace: std::cell::RefCell<Vec<(u32, u32)>>,
    /// Test-only: live-userdata chain nodes read by the private selection, in
    /// visit order. Each entry is one real slot-record read and the exact
    /// generation the chain cursor named, so a test can distinguish a real
    /// chain walk from a counter that merely reports the number of candidates.
    #[cfg(test)]
    pending_live_trace: std::cell::RefCell<Vec<(u32, u32)>>,
    /// Test-only: identities actually visited by a failed settlement
    /// journal rollback, in visit order. A successful pass and Release leave
    /// it empty, so the trace distinguishes precise cleanup from a drain
    /// deferred to the next pulse or to Release.
    #[cfg(test)]
    settlement_journal_trace: std::cell::RefCell<Vec<(u32, u32)>>,
    /// Test-only anti-control for the secondary propagation. When armed, a
    /// selected pending userdata is still marked and protected, but its
    /// metatable edge is not followed while its owner head is scanned. This
    /// proves the graph-retention assertion depends on that real propagation.
    /// It is not a callback path and never fabricates a close or a status.
    #[cfg(test)]
    skip_pending_finalizer_metatable: bool,
    /// Test-only anti-control for the bounded Release invariant. When armed,
    /// the next publish clears this exact pending slot's mark bit. Shared
    /// Select must then refuse the slot before Snapshot or Owners, so no weak
    /// edge is published and no release starts.
    #[cfg(test)]
    reclaim_pending_slot: Option<u32>,
    /// Test-only observation: closure handles admitted by the controlled guest
    /// import, in admission order. Written only after a successful store. The
    /// vector does not admit, retain, or expose a callable; slot refusal
    /// tests read it and the file-entry witness uses it to find the exact
    /// generations. No non-test code can reach the field.
    #[cfg(test)]
    admitted_guest_callables: Vec<ClosureRef>,
}

/// One typed userdata retained after the ordinary mark converged.
///
/// `raw` is the exact Core handle still bound in `slot`; the selection and the
/// host pre-publication check both read that same binding, so a record cannot
/// name a different object. The record is not a root vector entry, is not a
/// guest value, and is visible only to this table module and its heap parent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PendingFinalizer {
    pub(super) slot: u32,
    pub(super) generation: u32,
    pub(super) raw: u64,
}

/// The one-layer raw `__call` result on one receiver's own current instance
/// metatable. A table-valued `__call` is `NotClosure`; this slice never
/// dispatches it again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CurrentRawCall {
    WithoutCall,
    NotClosure,
    Closure(ClosureRef),
}

mod pending_queue {
    use std::collections::VecDeque;

    use super::PendingFinalizer;

    /// Private storage for retained pending-finalizer records.
    ///
    /// The queue is a `VecDeque`. Appending publishes nothing; a successful
    /// private collection stores `published = len` only after its real weak
    /// publication completed. The leading `published` records are then
    /// consumable in the order the selecting scan offered them: the live
    /// chain walks newest allocation first inside one cohort, and a later
    /// cohort appends after every retained earlier record. A front pop is O(1)
    /// in the stored-record count: it reads the head record and moves one
    /// record, and it neither reads nor shifts a successor.
    ///
    /// Every read of a stored record goes through one of the accessors below,
    /// which is also the single place where the test-only read meter observes
    /// a real queue read. The move meter is separate: a front pop charges one
    /// moved record, while a positional removal charges its removed record.
    /// No caller outside this module can index the vector: the field is
    /// private here and no iterator over the raw storage is exported.
    #[derive(Default)]
    pub(super) struct PendingQueue {
        records: VecDeque<PendingFinalizer>,
        /// Number of leading records a successful private publication made
        /// consumable. Always at most `records.len()`.
        published: usize,
        /// Test-only count of records read through this boundary.
        #[cfg(test)]
        reads: std::cell::Cell<u64>,
        /// Test-only count of stored records moved by an extraction. A front
        /// pop moves its one returned record and displaces no successor.
        #[cfg(test)]
        moves: std::cell::Cell<u64>,
    }

    impl PendingQueue {
        pub(super) fn len(&self) -> usize {
            self.records.len()
        }

        /// The consumable prefix length. This is a header read only.
        pub(super) fn published_len(&self) -> usize {
            self.published
        }

        pub(super) fn push(&mut self, record: PendingFinalizer) {
            self.records.push_back(record);
        }

        /// One stored record by position. The hit is one queue read.
        pub(super) fn get(&self, index: usize) -> Option<&PendingFinalizer> {
            let record = self.records.get(index)?;
            self.record_read();
            Some(record)
        }

        /// Iterates stored records in order. Each yielded record is one read.
        pub(super) fn iter(&self) -> PendingQueueIter<'_> {
            PendingQueueIter {
                queue: self,
                next: 0,
            }
        }

        /// One consumable head record. One record read; does not pop or write.
        pub(super) fn published_front(&self) -> Option<PendingFinalizer> {
            if self.published == 0 {
                return None;
            }
            let record = self.records.front().copied()?;
            self.record_read();
            Some(record)
        }

        /// Makes every currently stored record consumable. This is one header
        /// write. Records appended afterwards stay unpublished until the next
        /// successful private publication.
        pub(super) fn publish_all(&mut self) {
            self.published = self.records.len();
        }

        /// Pops the expected published front. A hit reads one record, moves one
        /// stored record, and drops the ready prefix by one. No successor is
        /// read or displaced. The expected record is the value the caller just
        /// read from `published_front`; the identity assertion is debug-only
        /// because those two calls are adjacent and no mutation can intervene.
        pub(super) fn pop_published_front(
            &mut self,
            expected: PendingFinalizer,
        ) -> Option<PendingFinalizer> {
            if self.published == 0 {
                return None;
            }
            let record = self.records.pop_front()?;
            debug_assert_eq!(record, expected);
            self.published -= 1;
            self.record_read();
            self.record_move();
            Some(record)
        }

        /// Removes one record by position. The removed record is one read.
        /// Removing a published position keeps the remaining prefix ready;
        /// removing an unpublished position leaves the prefix unchanged.
        pub(super) fn remove(&mut self, index: usize) -> Option<PendingFinalizer> {
            if index >= self.records.len() {
                return None;
            }
            let record = self.records.remove(index)?;
            if index < self.published {
                self.published -= 1;
            }
            self.record_read();
            self.record_move();
            Some(record)
        }

        /// Removes and returns every record from `base` onward.
        ///
        /// The batch is one boundary: a non-empty suffix is accounted once by
        /// its length in the test meters. The production build has no
        /// per-record loop here; the returned records are the caller's own
        /// rollback input. An out-of-range base returns nothing and reads no
        /// record. The ready prefix is clamped to `base`, so a rollback can
        /// never make an unpublished record consumable.
        pub(super) fn split_off_from(&mut self, base: usize) -> Vec<PendingFinalizer> {
            if base >= self.records.len() {
                return Vec::new();
            }
            let suffix = self.records.split_off(base);
            self.published = self.published.min(base);
            let removed: Vec<_> = suffix.into_iter().collect();
            #[cfg(test)]
            self.record_reads(removed.len() as u64);
            #[cfg(test)]
            self.record_moves(removed.len() as u64);
            removed
        }

        /// Returns the reads since the previous take and clears the count.
        #[cfg(test)]
        pub(super) fn take_reads(&mut self) -> u64 {
            let reads = self.reads.get();
            self.reads.set(0);
            reads
        }

        /// Returns the stored-record moves since the previous take.
        #[cfg(test)]
        pub(super) fn take_moves(&mut self) -> u64 {
            let moves = self.moves.get();
            self.moves.set(0);
            moves
        }

        #[cfg(test)]
        fn record_read(&self) {
            self.reads.set(self.reads.get() + 1);
        }

        /// Accounts a whole batch handed out by one boundary. Test-only.
        #[cfg(test)]
        fn record_reads(&self, count: u64) {
            self.reads.set(self.reads.get() + count);
        }

        #[cfg(test)]
        fn record_move(&self) {
            self.moves.set(self.moves.get() + 1);
        }

        #[cfg(test)]
        fn record_moves(&self, count: u64) {
            self.moves.set(self.moves.get() + count);
        }

        #[cfg(not(test))]
        fn record_read(&self) {}

        #[cfg(not(test))]
        fn record_move(&self) {}
    }

    /// Counted iterator over the private queue. It yields borrowed records
    /// only, so the caller never reaches the vector itself.
    pub(super) struct PendingQueueIter<'a> {
        queue: &'a PendingQueue,
        next: usize,
    }

    impl<'a> Iterator for PendingQueueIter<'a> {
        type Item = &'a PendingFinalizer;

        fn next(&mut self) -> Option<Self::Item> {
            let record = self.queue.records.get(self.next)?;
            self.next += 1;
            self.queue.record_read();
            Some(record)
        }
    }
}
use pending_queue::PendingQueue;

/// The batch boundary's own metering witness.
///
/// This is an equivalence witness for the boundary result: the split has
/// always handed back the suffix exactly once. What the fix changes is where
/// the test meter pays for it: a single test-only `removed.len()` charge
/// replaces a per-record loop that the production build also compiled. These
/// assertions pin the boundary contract on both the old and the new source;
/// they are not a fabricated behavior RED.
#[cfg(test)]
mod pending_queue_tests {
    use super::PendingFinalizer;
    use super::pending_queue::PendingQueue;

    fn record(slot: u32) -> PendingFinalizer {
        PendingFinalizer {
            slot,
            generation: slot + 1,
            raw: u64::from(slot) + 10,
        }
    }

    #[test]
    fn split_off_from_accounts_the_suffix_once_and_keeps_the_prefix() {
        let mut queue = PendingQueue::default();
        assert!(
            queue.split_off_from(0).is_empty(),
            "an empty queue has no suffix"
        );
        assert_eq!(queue.take_reads(), 0, "an empty queue reads no record");
        for slot in 0..3 {
            queue.push(record(slot));
        }
        assert_eq!(queue.take_reads(), 0, "push and len are not record reads");

        let suffix = queue.split_off_from(1);
        assert_eq!(suffix, vec![record(1), record(2)]);
        assert_eq!(queue.len(), 1, "only the old prefix stays queued");
        assert_eq!(
            queue.take_reads(),
            2,
            "a two-record suffix is accounted once by its length"
        );
        assert_eq!(
            queue.get(0),
            Some(&record(0)),
            "the untouched prefix keeps its record"
        );
        assert_eq!(queue.take_reads(), 1, "the split did not read the prefix");

        assert!(
            queue.split_off_from(1).is_empty(),
            "base at the length is an empty suffix"
        );
        assert_eq!(queue.take_reads(), 0, "an empty suffix reads no record");
        assert!(
            queue.split_off_from(9).is_empty(),
            "an out-of-range base is an empty suffix"
        );
        assert_eq!(
            queue.take_reads(),
            0,
            "an out-of-range base reads no record"
        );

        assert_eq!(queue.split_off_from(0), vec![record(0)]);
        assert_eq!(queue.len(), 0);
        assert_eq!(queue.take_reads(), 1, "the last record is one batch read");
        assert!(queue.split_off_from(0).is_empty());
        assert_eq!(queue.take_reads(), 0, "a drained queue reads no record");
    }
}

/// One userdata that passed the slot-local selection checks and whose
/// metatable's raw `__gc` field still has to be resolved by a metered walk.
/// It carries no value and grants no write by itself.
#[derive(Clone, Copy, Debug)]
pub(super) struct PendingCandidate {
    pub(super) slot: u32,
    pub(super) generation: u32,
    pub(super) metatable_slot: u32,
    pub(super) metatable_generation: u32,
}

/// One typed-issuer userdata reservation, keyed by its exact slot and
/// generation. `raw` is the registered Core handle that a published binding
/// does not yet own.
#[derive(Clone, Copy, Debug)]
struct PendingUserdata {
    slot: u32,
    generation: u32,
    raw: Option<u64>,
}

/// Positive finite integer key that can sit in a 1-based prefix.
///
/// `cap` is the edge quota. A contiguous prefix cannot outgrow the owner's
/// degree, and that degree cannot exceed the quota, so a larger key cannot
/// change the border. The caller therefore does not size marks by the key.
fn sequence_index(key: &EdgeKey, cap: u32) -> Option<u32> {
    let EdgeKey::Number(bits) = key else {
        return None;
    };
    let number = f64::from_bits(*bits);
    if !number.is_finite() || number < 1.0 || number.trunc() != number {
        return None;
    }
    if number > f64::from(cap) {
        return None;
    }
    Some(number as u32)
}

/// Where a publish-only sweep stopped. A completion means neither a dead slot
/// nor a retained pending finalizer was published. `Published` has rewritten
/// weak edges and has not released.
pub(super) enum PublishHalt {
    Completed(CollectReport),
    Published { heads: u64, edges: u64 },
}

/// Host-facing result of spending budget on a saved sweep tail.
pub(super) enum TailResume {
    Denied,
    Paused,
    Completed(CollectReport),
}

/// Classification of one located named record. Not a heap error.
enum PublishedName {
    Gap,
    Internal,
    Scalar,
    Table(u32, u32),
}

impl LuaHeap {
    /// Reserved direct field id for `__mode`; ordinary field ids start at 1.
    pub const MODE_FIELD: u32 = 0;

    /// Creates an empty heap. Slot zero is ordinary storage; handles start at
    /// generation 1 so generation 0 never denotes a live node.
    pub fn new(limits: HeapLimits) -> Self {
        Self {
            domain: NEXT_DOMAIN.fetch_add(1, Ordering::Relaxed),
            limits,
            slots: Vec::new(),
            free_list: Vec::new(),
            edge_slots: Vec::new(),
            edge_free_head: None,
            occupied_edges: 0,
            live_edge_count: 0,
            metatable_edges: 0,
            roots: Vec::new(),
            frames: Vec::new(),
            next_frame_generation: 1,
            collections: 0,
            last_marked: 0,
            peak_nodes: 0,
            peak_roots: 0,
            peak_edges: 0,
            scan: ScanMeter::default(),
            mark: None,
            rescan_epoch: 0,
            sweep_tail: None,
            payload_bytes: 0,
            pending_userdata: None,
            pending_finalizers: PendingQueue::default(),
            live_userdata_head: None,
            live_userdata_tail: None,
            settlement_head: None,
            #[cfg(test)]
            settlement_journal_len: 0,
            #[cfg(test)]
            rollback_owner_visits: 0,
            #[cfg(test)]
            selection_slot_trace: std::cell::RefCell::new(Vec::new()),
            #[cfg(test)]
            pending_gc_edge_trace: std::cell::RefCell::new(Vec::new()),
            #[cfg(test)]
            pending_live_trace: std::cell::RefCell::new(Vec::new()),
            #[cfg(test)]
            settlement_journal_trace: std::cell::RefCell::new(Vec::new()),
            #[cfg(test)]
            skip_pending_finalizer_metatable: false,
            #[cfg(test)]
            reclaim_pending_slot: None,
            #[cfg(test)]
            admitted_guest_callables: Vec::new(),
        }
    }

    /// Returns the counts since the previous take and clears them.
    /// Does not collect, and does not record this read as collection work.
    ///
    /// The pending-queue read count is folded in from the queue's own access
    /// boundary. There is no separate call-site accounting: a scan that reads
    /// stored records is visible here even when it was added later.
    pub fn take_scan_meter(&mut self) -> ScanMeter {
        let meter = self.scan;
        #[cfg(test)]
        let meter = {
            let mut meter = meter;
            meter.pending_release_queue_reads += self.pending_finalizers.take_reads();
            meter.pending_queue_storage_moves += self.pending_finalizers.take_moves();
            meter
        };
        self.scan = ScanMeter::default();
        meter
    }

    /// Reserves a live, unrooted table. Failure leaves occupancy unchanged.
    pub fn alloc(&mut self) -> Result<TableRef, HeapError> {
        let (index, generation) = self.alloc_slot(HandleKind::Table)?;
        Ok(TableRef::table(self.domain, index, generation))
    }

    /// Reserves a live, unrooted closure. It uses the same node cap as a table.
    /// It has no guest carrier and no resource token.
    pub fn alloc_closure(&mut self) -> Result<ClosureRef, HeapError> {
        let (index, generation) = self.alloc_slot(HandleKind::Closure)?;
        Ok(ClosureRef::closure(self.domain, index, generation))
    }

    /// Reserves a live, unrooted cell. It uses the same node cap as a table.
    /// It has no guest carrier and no resource token.
    pub fn alloc_upvalue(&mut self) -> Result<UpvalueRef, HeapError> {
        let (index, generation) = self.alloc_slot(HandleKind::Upvalue)?;
        Ok(UpvalueRef::upvalue(self.domain, index, generation))
    }

    /// Reserves a live, unrooted bare userdata. It uses the same node cap as a
    /// table. It has no guest carrier, finalizer, or resource token.
    ///
    /// A paused mark and a published tail are refused before a free slot is
    /// taken or a new slot is grown. `alloc_slot` alone does not see a paused
    /// gray mark. A full node vector still reports capacity and does not write.
    pub fn alloc_userdata(&mut self) -> Result<UserdataRef, HeapError> {
        if !self.free_list.is_empty() {
            self.deny_unbarriered()?;
        } else if self.slots.len() >= self.limits.nodes as usize {
            return Err(HeapError::NodeCapacity);
        } else if self.limits.max_generation == 0 {
            return Err(HeapError::GenerationExhausted);
        } else {
            self.deny_unbarriered()?;
        }
        let (index, generation) = self.alloc_slot(HandleKind::Userdata)?;
        self.link_live_userdata_head(index, generation);
        Ok(UserdataRef::userdata(self.domain, index, generation))
    }

    fn alloc_slot(&mut self, kind: HandleKind) -> Result<(u32, u32), HeapError> {
        if !self.free_list.is_empty() {
            self.ensure_sweep_open()?;
            return self.take_free_slot(kind);
        }
        if self.slots.len() >= self.limits.nodes as usize {
            return Err(HeapError::NodeCapacity);
        }
        if self.limits.max_generation == 0 {
            return Err(HeapError::GenerationExhausted);
        }
        self.ensure_sweep_open()?;
        self.grow_slot(kind)
    }

    fn take_free_slot(&mut self, kind: HandleKind) -> Result<(u32, u32), HeapError> {
        let index = self.free_list.pop().expect("free list was not empty");
        let slot = &mut self.slots[index as usize];
        if !matches!(slot.state, SlotState::Free) {
            return Err(HeapError::StaleHandle);
        }
        slot.kind = kind;
        slot.state = SlotState::Live { marked: false };
        slot.token = None;
        slot.carrier = None;
        slot.metatable = None;
        slot.pending_finalizer = false;
        slot.finalizer_settled = false;
        slot.live_userdata_prev = None;
        slot.live_userdata_next = None;
        slot.live_userdata_linked = false;
        slot.settlement_next = None;
        slot.settlement_generation = 0;
        slot.pending_gc_generation = 0;
        slot.pending_gc_round = 0;
        slot.pending_gc_non_nil = false;
        slot.edge_head = None;
        slot.edge_tail = None;
        slot.mode_edge = None;
        slot.mode_epoch = 0;
        slot.mode_generation = 0;
        slot.mode_round = 0;
        slot.mode_weak = WeakMode::default();
        slot.mode_checked_generation = 0;
        slot.mode_checked_round = 0;
        slot.mode_checked_epoch = 0;
        slot.late_mode_epoch = 0;
        slot.late_mode_prev = None;
        slot.late_mode_next = None;
        slot.late_owner_epoch = 0;
        slot.late_owner_old = None;
        slot.late_owner_old_mode = None;
        slot.late_owner_new = None;
        slot.late_owner_prev = None;
        slot.late_owner_next = None;
        slot.discover_generation = 0;
        slot.discover_batch = 0;
        slot.discover_round = 0;
        slot.rescan_epoch = 0;
        slot.rescan_queued = false;
        slot.rescan_prev = None;
        slot.rescan_next = None;
        slot.late_target_epoch = 0;
        slot.late_target_prev = None;
        slot.late_target_next = None;
        Ok((index, slot.generation))
    }

    fn grow_slot(&mut self, kind: HandleKind) -> Result<(u32, u32), HeapError> {
        let index = u32::try_from(self.slots.len()).map_err(|_| HeapError::NodeCapacity)?;
        self.slots.push(Slot {
            generation: 1,
            kind,
            state: SlotState::Live { marked: false },
            token: None,
            carrier: None,
            metatable: None,
            pending_finalizer: false,
            finalizer_settled: false,
            live_userdata_prev: None,
            live_userdata_next: None,
            live_userdata_linked: false,
            settlement_next: None,
            settlement_generation: 0,
            pending_gc_generation: 0,
            pending_gc_round: 0,
            pending_gc_non_nil: false,
            edge_head: None,
            edge_tail: None,
            mode_edge: None,
            mode_epoch: 0,
            mode_generation: 0,
            mode_round: 0,
            mode_weak: WeakMode::default(),
            mode_checked_generation: 0,
            mode_checked_round: 0,
            mode_checked_epoch: 0,
            late_mode_epoch: 0,
            late_mode_prev: None,
            late_mode_next: None,
            late_owner_epoch: 0,
            late_owner_old: None,
            late_owner_old_mode: None,
            late_owner_new: None,
            late_owner_prev: None,
            late_owner_next: None,
            discover_generation: 0,
            discover_batch: 0,
            discover_round: 0,
            rescan_epoch: 0,
            rescan_queued: false,
            rescan_prev: None,
            rescan_next: None,
            late_target_epoch: 0,
            late_target_prev: None,
            late_target_next: None,
        });
        self.note_node_peak();
        Ok((index, 1))
    }

    /// Reserves one unmarked table after checking for a saved published tail.
    /// The tail gate runs before slot reuse or the growth capacity check.
    /// The reservation is not a handle until committed.
    pub(super) fn reserve_late_table(&mut self) -> Result<LateReservation, HeapError> {
        self.ensure_published_tail()?;
        let (index, generation) = self.place_published_table()?;
        Ok(LateReservation {
            domain: self.domain,
            index,
            generation,
            spent: false,
        })
    }

    fn place_published_table(&mut self) -> Result<(u32, u32), HeapError> {
        if !self.free_list.is_empty() {
            self.ensure_published_tail()?;
            return self.take_free_slot(HandleKind::Table);
        }
        if self.slots.len() >= self.limits.nodes as usize {
            return Err(HeapError::NodeCapacity);
        }
        if self.limits.max_generation == 0 {
            return Err(HeapError::GenerationExhausted);
        }
        self.ensure_published_tail()?;
        self.grow_slot(HandleKind::Table)
    }

    pub(super) fn commit_late(&mut self, mut reservation: LateReservation, raw: u64) {
        debug_assert!(self.sweep_tail.is_some());
        let index = reservation.index as usize;
        debug_assert!(self.slots[index].token.is_none());
        debug_assert!(matches!(
            self.slots[index].state,
            SlotState::Live { marked: false }
        ));
        self.slots[index].token = Some(raw);
        self.sweep_tail
            .as_mut()
            .expect("published tail accepted the reservation")
            .note_late();
        reservation.spent = true;
    }

    pub(super) fn abandon_late(&mut self, mut reservation: LateReservation) {
        let index = reservation.index;
        debug_assert!(self.slots[index as usize].token.is_none());
        debug_assert!(self.slots[index as usize].edge_head.is_none());
        self.release_slot(index);
        reservation.spent = true;
    }

    /// Releases a reserved node. A stale or foreign handle is refused and the
    /// heap is left unchanged. The slot stays in the vector so its generation
    /// cannot be reissued as 1.
    pub fn rollback(&mut self, handle: TableRef) -> Result<(), HeapError> {
        self.require_live(handle)?;
        self.ensure_sweep_open()?;
        self.clear_rooted_to(Rooted::Table(handle));
        self.remove_edges_touching(handle.slot());
        self.release_slot(handle.slot());
        Ok(())
    }

    /// Issues the guest array for a live table.
    ///
    /// The slot keeps one array until release. A second issue for that live
    /// generation is refused, so assignment and field reads keep one identity.
    pub fn issue_carrier(&mut self, table: TableRef) -> Result<Value, HeapError> {
        self.require_live(table)?;
        let index = table.slot() as usize;
        if self.slots[index].carrier.is_some() {
            return Err(HeapError::StaleHandle);
        }
        self.ensure_sweep_open()?;
        let value = Value::array(vec![
            Value::Int(i64::from(table.slot())),
            Value::Int(i64::from(table.generation())),
        ]);
        let carrier = match &value {
            Value::Array(issued) => Arc::clone(issued),
            _ => unreachable!("Value::array returns an array"),
        };
        self.slots[index].carrier = Some(SlotCarrier::Table(carrier));
        Ok(value)
    }

    /// Issues the carrier for one unmarked reservation while a sweep tail is saved.
    ///
    /// Ordinary [`Self::issue_carrier`] stays closed: it still calls
    /// `ensure_sweep_open` and refuses a second issue. This path requires the
    /// exact live unmarked reservation and does not resume the tail.
    #[cfg(test)]
    pub(super) fn issue_reserved_carrier(&mut self, table: TableRef) -> Result<Value, HeapError> {
        self.ensure_published_tail()?;
        self.require_live(table)?;
        let index = table.slot() as usize;
        if Self::slot_marked(&self.slots[index].state) {
            return Err(HeapError::Scope(
                "release handoff carrier slot was marked".to_string(),
            ));
        }
        if self.slots[index].carrier.is_some() || self.slots[index].token.is_some() {
            return Err(HeapError::StaleHandle);
        }
        let value = Value::array(vec![
            Value::Int(i64::from(table.slot())),
            Value::Int(i64::from(table.generation())),
        ]);
        let carrier = match &value {
            Value::Array(issued) => Arc::clone(issued),
            _ => unreachable!("Value::array returns an array"),
        };
        self.slots[index].carrier = Some(SlotCarrier::Table(carrier));
        Ok(value)
    }

    /// Fixed name of the controlled `.lua` guest callable import.
    ///
    /// The completion slice already admits the `note` host name, so the
    /// heap-owned admission reuses that slot instead of changing the lowerer.
    /// The test compile catalog declares exactly this import, and its
    /// dispatch function is private to this module. There is no crate-visible
    /// entry that takes an `Arc<CallableValue>`, a `Value`, a VM, or a capture
    /// count.
    pub(crate) const GUEST_CALLABLE_IMPORT: &'static str = "note";

    /// Declares the schema of the controlled guest callable import.
    ///
    /// One untyped guest argument is the callable. The boolean result is
    /// `true` when a live closure generation now holds the exact Arc and
    /// `false` when the same VM's compiled prototype declares captures. The
    /// caller cannot choose the count: [`admit_guest_callable`] reads the
    /// prototype layout from the VM that invoked the import.
    pub(crate) fn guest_callable_import_schema() -> HostFunctionSchema {
        HostFunctionSchema::with_return(
            Self::GUEST_CALLABLE_IMPORT,
            vec![HostParamSchema::value("target", HostTypeSchema::Unknown)],
            HostTypeSchema::Bool,
        )
    }

    /// Registers the heap-owned callable import on one host registry.
    ///
    /// The caller supplies the schema compiled into its catalog. Only the
    /// exact controlled shape is accepted. The dispatch function stays
    /// private, so registering this import is the whole crate-visible
    /// surface: no API accepts an `Arc<CallableValue>` and a bare count, and
    /// no API accepts a caller-supplied `Value` together with a VM to mint an
    /// admission. The import derives the capture count from the VM that
    /// invokes it and from the value's prototype in that VM.
    ///
    /// This is a direct Lua producer, not an arbitrary host injection check.
    /// Core's `Vm::owns_callable` is not visible here and `CallableValue`
    /// carries no VM identity, so this entry does not claim that a callable
    /// returned by some host belongs to this VM. The issuance evidence is the
    /// guest argument itself.
    pub(crate) fn register_guest_callable_import(
        registry: &mut HostFunctionRegistry,
        schema: HostImportSchema,
    ) -> VmResult<u16> {
        if schema.name != Self::GUEST_CALLABLE_IMPORT
            || schema.params.len() != 1
            || schema.params[0].name != "target"
            || schema.params[0].schema != HostTypeSchema::Unknown
            || schema.params[0].passing != vm::HostParamPassing::Value
            || schema.return_type != HostTypeSchema::Bool
        {
            return Err(VmError::HostError(
                "the guest callable import schema is not the heap-controlled shape".to_string(),
            ));
        }
        registry.register_exact_static(Self::GUEST_CALLABLE_IMPORT, 1, schema, admit_guest_callable)
    }

    /// Declares the fixed two-argument raw-call-field shape.
    ///
    /// The owner is the receiver table's issued guest array; the target is one
    /// callable this VM's guest produced. Both are `Unknown` values, and the
    /// boolean result is `true` only when the existing raw `__call` record was
    /// replaced by a fresh closure generation holding the exact target Arc.
    /// The caller cannot choose a slot, generation, capture count, or field.
    pub(crate) fn guest_callable_field_import_schema() -> HostFunctionSchema {
        HostFunctionSchema::with_return(
            Self::GUEST_CALLABLE_IMPORT,
            vec![
                HostParamSchema::value("owner", HostTypeSchema::Unknown),
                HostParamSchema::value("target", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Bool,
        )
    }

    /// Registers the heap-owned two-argument raw-call-field import.
    ///
    /// The caller supplies the schema compiled into its catalog. Only the
    /// exact controlled shape is accepted; the dispatch function remains
    /// private. There is no crate-visible entry that accepts a raw slot,
    /// generation, field id, `Arc<CallableValue>`, or caller-supplied capture
    /// count, and this method does not widen the one-argument admission above.
    pub(crate) fn register_guest_callable_field_import(
        registry: &mut HostFunctionRegistry,
        schema: HostImportSchema,
    ) -> VmResult<u16> {
        if schema.name != Self::GUEST_CALLABLE_IMPORT
            || schema.params.len() != 2
            || schema.params[0].name != "owner"
            || schema.params[0].schema != HostTypeSchema::Unknown
            || schema.params[0].passing != vm::HostParamPassing::Value
            || schema.params[1].name != "target"
            || schema.params[1].schema != HostTypeSchema::Unknown
            || schema.params[1].passing != vm::HostParamPassing::Value
            || schema.return_type != HostTypeSchema::Bool
        {
            return Err(VmError::HostError(
                "the raw call field import schema is not the heap-controlled shape".to_string(),
            ));
        }
        registry.register_exact_static(
            Self::GUEST_CALLABLE_IMPORT,
            2,
            schema,
            admit_guest_callable_to_raw_call,
        )
    }

    /// Replaces one existing raw `__call` record with a fresh closure
    /// generation that owns the exact guest callable Arc.
    ///
    /// The accepted raw states are exactly two: the fixture's `false` sentinel
    /// and one live closure generation whose slot still holds a callable
    /// carrier from this admission. Every validation happens before the first
    /// write: the issued owner and its exact array Arc, the owner's current
    /// metatable, the exact `__call` record, the live installed closure when
    /// one is present, a saved mark, a published Release tail, the zero-capture
    /// target shape passed by the caller, and node capacity. The remaining
    /// transaction is one slot allocation, one carrier store on that fresh
    /// generation, and one in-place replacement of the existing edge value.
    /// None of those three steps can fail, and none inserts a new edge, root,
    /// scan rule, or metatable.
    fn handoff_guest_callable_to_raw_call(
        &mut self,
        owner_value: &Value,
        callable: Arc<CallableValue>,
        capture_slots: usize,
    ) -> Result<ClosureRef, HeapError> {
        if capture_slots != 0 || callable.kind != CallableKind::Closure {
            return Err(HeapError::CallableHasEnvironment);
        }
        let GuestClass::Table(owner) = self.guest_class(owner_value) else {
            return Err(HeapError::WrongKind);
        };
        let Some((metatable_slot, metatable_generation)) =
            self.slots[owner.slot() as usize].metatable
        else {
            return Err(HeapError::StaleHandle);
        };
        if !self.slot_matches(metatable_slot, metatable_generation, HandleKind::Table) {
            return Err(HeapError::StaleHandle);
        }
        let key = EdgeKey::Utf8(RAW_CALL_FIELD.to_owned());
        let Some(index) = self.find_key(metatable_slot, &key) else {
            return Err(HeapError::StaleHandle);
        };
        let accepted = {
            let edge = self.edge_slots[index as usize]
                .edge
                .as_ref()
                .expect("owner chain holds an occupied record");
            match &edge.value {
                EdgeValue::Scalar(Value::Bool(false)) => true,
                EdgeValue::Closure { slot, generation } => {
                    self.slot_matches(*slot, *generation, HandleKind::Closure)
                        && matches!(
                            self.slots[*slot as usize].carrier,
                            Some(SlotCarrier::Callable(_))
                        )
                }
                _ => false,
            }
        };
        if !accepted {
            return Err(HeapError::WrongKind);
        }
        if self.mark.is_some() {
            return Err(HeapError::MarkDenied);
        }
        if self.sweep_tail.is_some() {
            return Err(HeapError::SweepPending);
        }
        if !self.closure_slot_capacity_open() {
            return Err(HeapError::NodeCapacity);
        }
        let closure = self.alloc_closure()?;
        let closure_slot = closure.slot() as usize;
        debug_assert!(
            self.slots[closure_slot].carrier.is_none(),
            "a fresh closure generation starts unbound"
        );
        // The remaining transaction cannot fail: allocation produced an
        // unbound live closure, and replacing the located edge value does not
        // allocate, insert, or run a barrier (no mark or tail is open here).
        self.slots[closure_slot].carrier = Some(SlotCarrier::Callable(callable));
        #[cfg(test)]
        self.admitted_guest_callables.push(closure);
        self.replace_edge_value(
            metatable_slot,
            index,
            EdgeValue::Closure {
                slot: closure.slot(),
                generation: closure.generation(),
            },
        );
        Ok(closure)
    }

    /// Whether a fresh closure generation can be placed without a write.
    ///
    /// This mirrors the two `alloc_slot` paths: a free-list slot is reusable
    /// without a capacity or generation check, and a new slot needs room under
    /// both the node cap and the generation cap. Callers that must reject at
    /// full capacity before the first write use this instead of guessing after
    /// `alloc_closure`.
    fn closure_slot_capacity_open(&self) -> bool {
        !self.free_list.is_empty()
            || (self.slots.len() < self.limits.nodes as usize && self.limits.max_generation != 0)
    }

    /// Stores one admitted callable on one live closure generation.
    ///
    /// Private to this module. The only non-test caller is
    /// [`admit_guest_callable`], which derives `capture_slots` from the
    /// invoking VM's compiled prototype. A live generation holds at most one
    /// carrier, so a second store, including one with a different Arc, is
    /// refused and the stored Arc stays. A failed store stores no Arc.
    ///
    /// The callable is one identity carrier like the issued table array: mark
    /// does not read it, it is not a root or an edge, and it does not add scan
    /// work. Release clears it with the slot, and a reused generation starts
    /// empty.
    fn store_guest_callable(
        &mut self,
        closure: ClosureRef,
        callable: Arc<CallableValue>,
        capture_slots: usize,
    ) -> Result<(), HeapError> {
        self.require_closure(closure)?;
        if capture_slots != 0 {
            return Err(HeapError::CallableHasEnvironment);
        }
        let index = closure.slot() as usize;
        if self.slots[index].carrier.is_some() {
            return Err(HeapError::CarrierAlreadyBound);
        }
        self.ensure_sweep_open()?;
        self.slots[index].carrier = Some(SlotCarrier::Callable(callable));
        #[cfg(test)]
        self.admitted_guest_callables.push(closure);
        Ok(())
    }

    /// Test-only: closure generations the controlled guest import admitted, in
    /// admission order. Observation only; the stored Arcs stay private to the
    /// slot carrier and no test can put a value back into this vector.
    #[cfg(test)]
    pub(crate) fn guest_callable_admissions(&self) -> &[ClosureRef] {
        &self.admitted_guest_callables
    }

    /// Returns the exact Arc bound to one live closure generation.
    ///
    /// The handle's domain, kind, slot, and generation are checked first. An
    /// unbound generation is [`HeapError::CarrierNotBound`]. There is no
    /// prototype or schema lookup: the stored Arc is the only identity.
    pub(crate) fn closure_callable(
        &self,
        closure: ClosureRef,
    ) -> Result<Arc<CallableValue>, HeapError> {
        self.require_closure(closure)?;
        match self.slots[closure.slot() as usize].carrier.as_ref() {
            Some(SlotCarrier::Callable(callable)) => Ok(Arc::clone(callable)),
            Some(SlotCarrier::Table(_)) | None => Err(HeapError::CarrierNotBound),
        }
    }

    /// Test-only: a closure handle naming this live table's slot and
    /// generation. The kind check must refuse it before any carrier access.
    /// This is not a handle constructor for guests or production code.
    #[cfg(test)]
    pub(crate) fn closure_handle_for_table(&self, table: TableRef) -> ClosureRef {
        ClosureRef::closure(self.domain, table.slot(), table.generation())
    }

    /// Classifies a guest value against this heap.
    ///
    /// A table result requires the live slot, the issued generation, and the
    /// same VM array. Field equality is not enough. An integer resource token
    /// is not consulted. The slot lookup, generation check, and pointer
    /// comparison do not add a root or increment scan counts.
    pub fn guest_class(&self, value: &Value) -> GuestClass {
        match value {
            Value::Float(_) => GuestClass::Number,
            Value::Bool(_) => GuestClass::Boolean,
            Value::String(_) => GuestClass::String,
            Value::Array(issued) => self
                .issued_table(issued)
                .map(GuestClass::Table)
                .unwrap_or(GuestClass::Unissued),
            _ => GuestClass::Unissued,
        }
    }

    fn issued_table(&self, issued: &Arc<Vec<Value>>) -> Option<TableRef> {
        let [Value::Int(slot), Value::Int(generation)] = issued.as_slice() else {
            return None;
        };
        let slot = u32::try_from(*slot).ok()?;
        let generation = u32::try_from(*generation).ok()?;
        let stored = self.slots.get(slot as usize)?;
        if stored.generation != generation {
            return None;
        }
        if !matches!(stored.state, SlotState::Live { .. }) {
            return None;
        }
        let current = match stored.carrier.as_ref()? {
            SlotCarrier::Table(current) => current,
            SlotCarrier::Callable(_) => return None,
        };
        if !Arc::ptr_eq(current, issued) {
            return None;
        }
        Some(TableRef::table(self.domain, slot, generation))
    }

    /// Writes a reference edge. An existing key is overwritten and does not
    /// consume another edge slot. A new key at the cap fails without insert.
    pub fn set_edge(&mut self, from: TableRef, key: u32, to: TableRef) -> Result<(), HeapError> {
        self.write_table_edge(from, EdgeKey::Field(key), to)
    }

    /// Points a table field at a closure. The field id is not a Lua number.
    pub fn set_closure_edge(
        &mut self,
        from: TableRef,
        key: u32,
        closure: ClosureRef,
    ) -> Result<(), HeapError> {
        self.require_live(from)?;
        self.require_closure(closure)?;
        self.write_from(
            from.slot(),
            EdgeKey::Field(key),
            EdgeValue::Closure {
                slot: closure.slot(),
                generation: closure.generation(),
            },
        )
    }

    /// Points a closure at one shared cell. This is not an open-capture list.
    pub fn set_upvalue_edge(
        &mut self,
        closure: ClosureRef,
        cell: UpvalueRef,
    ) -> Result<(), HeapError> {
        self.require_closure(closure)?;
        self.require_upvalue(cell)?;
        self.write_from(
            closure.slot(),
            EdgeKey::Field(UPVALUE_EDGE),
            EdgeValue::Upvalue {
                slot: cell.slot(),
                generation: cell.generation(),
            },
        )
    }

    /// Replaces the table currently stored in a shared cell.
    ///
    /// An existing current edge is overwritten. This does not insert a second
    /// record. A closure target uses [`Self::write_closure_upvalue`]. A
    /// boolean, binary64, string, or bytes value uses
    /// [`Self::write_scalar_upvalue`]. After the write commits, an already
    /// discovered cell shades only that direct table. The published tail is
    /// still refused inside the write, before any barrier.
    pub fn write_upvalue(&mut self, cell: UpvalueRef, table: TableRef) -> Result<(), HeapError> {
        self.require_upvalue(cell)?;
        self.require_live(table)?;
        self.write_from(
            cell.slot(),
            EdgeKey::Field(CURRENT_EDGE),
            EdgeValue::Table {
                slot: table.slot(),
                generation: table.generation(),
            },
        )?;
        self.barrier_strong_closed_cell(
            cell.slot(),
            cell.generation(),
            table.slot(),
            table.generation(),
            mark::ClosedCellTarget::Table,
        );
        Ok(())
    }

    /// Replaces the current edge with a closure. Same record as a table write.
    ///
    /// After the write commits, an already discovered cell shades only that
    /// direct closure. The capture and its table wait for the continued scan.
    /// The published tail is still refused inside the write, before any barrier.
    pub fn write_closure_upvalue(
        &mut self,
        cell: UpvalueRef,
        closure: ClosureRef,
    ) -> Result<(), HeapError> {
        self.require_upvalue(cell)?;
        self.require_closure(closure)?;
        self.write_from(
            cell.slot(),
            EdgeKey::Field(CURRENT_EDGE),
            EdgeValue::Closure {
                slot: closure.slot(),
                generation: closure.generation(),
            },
        )?;
        self.barrier_strong_closed_cell(
            cell.slot(),
            cell.generation(),
            closure.slot(),
            closure.generation(),
            mark::ClosedCellTarget::Closure,
        );
        Ok(())
    }

    /// Replaces the current edge with a boolean, binary64, string, or bytes scalar.
    ///
    /// Same record as [`Self::write_upvalue`]. An existing edge is overwritten.
    /// Nil is not stored here; [`Self::clear_upvalue`] deletes that record.
    /// Any other guest value is refused at entry and leaves the stored edge
    /// unchanged. The number is stored as given: signed zero is not
    /// canonicalized, and NaN is not treated as a table key. A string or bytes
    /// value is stored as given. The two carriers are not converted or compared.
    pub fn write_scalar_upvalue(
        &mut self,
        cell: UpvalueRef,
        value: Value,
    ) -> Result<(), HeapError> {
        if !matches!(
            value,
            Value::Bool(_) | Value::Float(_) | Value::String(_) | Value::Bytes(_)
        ) {
            return Err(HeapError::WrongKind);
        }
        self.require_upvalue(cell)?;
        self.write_from(
            cell.slot(),
            EdgeKey::Field(CURRENT_EDGE),
            EdgeValue::Scalar(value),
        )
    }

    /// Removes the current edge. Nil is the missing edge, not a stored record.
    pub fn clear_upvalue(&mut self, cell: UpvalueRef) -> Result<(), HeapError> {
        self.require_upvalue(cell)?;
        let key = EdgeKey::Field(CURRENT_EDGE);
        self.unlink_key(cell.slot(), &key)
    }

    /// Reads the current edge through a closure.
    ///
    /// `Ok(None)` is nil. `Err` means the closure, its cell, or the stored
    /// target is an invalid handle. A missing edge is not an invalid handle.
    /// This does not call the closure. The edge itself is decoded by
    /// [`Self::current_edge`].
    pub fn read_cell(&self, closure: ClosureRef) -> Result<Option<CellValue>, HeapError> {
        let cell = self.upvalue_of(closure)?;
        self.current_edge(cell)
    }

    /// Reads the table currently stored through a closure's cell.
    ///
    /// A closure or scalar target is `WrongKind`. A missing edge has no table,
    /// so this table adapter returns `StaleHandle`. [`Self::read_cell`] returns
    /// `None` for that nil.
    pub fn read_upvalue(&self, closure: ClosureRef) -> Result<TableRef, HeapError> {
        let cell = self.upvalue_of(closure)?;
        match self.current_table(cell)? {
            Some(table) => Ok(table),
            None => Err(HeapError::StaleHandle),
        }
    }

    /// The cell a closure points at. Two closures can return the same cell.
    pub fn upvalue_of(&self, closure: ClosureRef) -> Result<UpvalueRef, HeapError> {
        self.require_closure(closure)?;
        let Some(value) = self.stored_from(closure.slot(), &EdgeKey::Field(UPVALUE_EDGE)) else {
            return Err(HeapError::StaleHandle);
        };
        let EdgeValue::Upvalue { slot, generation } = value else {
            return Err(HeapError::WrongKind);
        };
        let cell = UpvalueRef::upvalue(self.domain, *slot, *generation);
        self.require_upvalue(cell)?;
        Ok(cell)
    }

    /// Reads a table field that points at a closure.
    ///
    /// A missing field is `StaleHandle`. A table or scalar at that field is
    /// `WrongKind`. The field id is not read as a Lua number.
    pub fn closure_edge(&self, from: TableRef, key: u32) -> Result<ClosureRef, HeapError> {
        self.require_live(from)?;
        let Some(value) = self.stored_from(from.slot(), &EdgeKey::Field(key)) else {
            return Err(HeapError::StaleHandle);
        };
        let EdgeValue::Closure { slot, generation } = value else {
            return Err(HeapError::WrongKind);
        };
        let closure = ClosureRef::closure(self.domain, *slot, *generation);
        self.require_closure(closure)?;
        Ok(closure)
    }

    /// Test-only: reads one raw named closure record exactly.
    ///
    /// This is the readback the file-entry witness uses to prove the raw
    /// `__call` field now names the generation the handoff wrote. It is not an
    /// admission, write, or lookup-by-prototype path, and a missing record or
    /// a non-closure value is refused before any generation is constructed.
    #[cfg(test)]
    pub(crate) fn named_closure_edge(
        &self,
        from: TableRef,
        name: &str,
    ) -> Result<ClosureRef, HeapError> {
        self.require_live(from)?;
        let Some(value) = self.stored_from(from.slot(), &EdgeKey::named(name)) else {
            return Err(HeapError::StaleHandle);
        };
        let EdgeValue::Closure { slot, generation } = value else {
            return Err(HeapError::WrongKind);
        };
        let closure = ClosureRef::closure(self.domain, *slot, *generation);
        self.require_closure(closure)?;
        Ok(closure)
    }

    /// Test-only: reads one raw named scalar record.
    ///
    /// `None` is an absent record. A stored nil gap, boolean, string, or bytes
    /// value is returned as its exact `Value`. A table, closure, upvalue, or
    /// userdata target is `WrongKind`, not a scalar. This observes the existing
    /// record and does not create one, follow `__index`, or cache a result.
    #[cfg(test)]
    pub(crate) fn named_raw_scalar(
        &self,
        from: TableRef,
        name: &str,
    ) -> Result<Option<Value>, HeapError> {
        self.require_live(from)?;
        let Some(value) = self.stored_from(from.slot(), &EdgeKey::named(name)) else {
            return Ok(None);
        };
        match value {
            EdgeValue::Scalar(value) => Ok(Some(value.clone())),
            _ => Err(HeapError::WrongKind),
        }
    }

    /// Points a table at the one cell that binding uses.
    ///
    /// The field is private. It is not a Lua key, so next/raw enumeration does
    /// not see it. A non-weak table follows the edge strongly through the
    /// existing mark; this does not add a scan rule.
    pub fn set_cell_edge(&mut self, from: TableRef, cell: UpvalueRef) -> Result<(), HeapError> {
        self.require_live(from)?;
        self.require_upvalue(cell)?;
        self.write_from(
            from.slot(),
            EdgeKey::Field(CELL_EDGE),
            EdgeValue::Upvalue {
                slot: cell.slot(),
                generation: cell.generation(),
            },
        )
    }

    /// The cell a table publishes. A missing edge is `StaleHandle`.
    pub fn cell_edge(&self, from: TableRef) -> Result<UpvalueRef, HeapError> {
        self.require_live(from)?;
        let Some(value) = self.stored_from(from.slot(), &EdgeKey::Field(CELL_EDGE)) else {
            return Err(HeapError::StaleHandle);
        };
        let EdgeValue::Upvalue { slot, generation } = value else {
            return Err(HeapError::WrongKind);
        };
        let cell = UpvalueRef::upvalue(self.domain, *slot, *generation);
        self.require_upvalue(cell)?;
        Ok(cell)
    }

    /// The table currently stored in a cell.
    ///
    /// `Ok(None)` is a missing current edge, which is nil. This does not go
    /// through a closure. A scalar or closure edge is `WrongKind`, not a table
    /// and not nil. The edge bytes are decoded only by [`Self::current_edge`].
    pub fn current_table(&self, cell: UpvalueRef) -> Result<Option<TableRef>, HeapError> {
        match self.current_edge(cell)? {
            Some(CellValue::Table(table)) => Ok(Some(table)),
            None => Ok(None),
            Some(_) => Err(HeapError::WrongKind),
        }
    }

    /// Decodes the one current edge of an upvalue.
    ///
    /// The caller passes an upvalue, and this checks that handle before
    /// reading. A missing edge is nil. Table, boolean, binary64, string,
    /// bytes, and closure stay distinct carriers. An integer or any other
    /// scalar is `WrongKind` and is not turned into a number or a table.
    /// This reads the existing edge record. It does not allocate a value slot
    /// or a root.
    pub fn current_edge(&self, cell: UpvalueRef) -> Result<Option<CellValue>, HeapError> {
        self.require_upvalue(cell)?;
        let Some(value) = self.stored_from(cell.slot(), &EdgeKey::Field(CURRENT_EDGE)) else {
            return Ok(None);
        };
        match value {
            EdgeValue::Table { slot, generation } => {
                let table = TableRef::table(self.domain, *slot, *generation);
                self.require_live(table)?;
                Ok(Some(CellValue::Table(table)))
            }
            EdgeValue::Closure { slot, generation } => {
                let found = ClosureRef::closure(self.domain, *slot, *generation);
                self.require_closure(found)?;
                Ok(Some(CellValue::Closure(found)))
            }
            EdgeValue::Scalar(
                value @ (Value::Bool(_) | Value::Float(_) | Value::String(_) | Value::Bytes(_)),
            ) => Ok(Some(CellValue::Scalar(value.clone()))),
            EdgeValue::Upvalue { .. } | EdgeValue::Userdata { .. } | EdgeValue::Scalar(_) => {
                Err(HeapError::WrongKind)
            }
        }
    }

    /// Writes a scalar into the same record and quota as a table edge.
    ///
    /// Nil deletes that record. A string may occupy any direct field. A number
    /// or boolean remains a field 0 value; only that key's string is `__mode`.
    pub fn set_field_scalar(
        &mut self,
        from: TableRef,
        key: u32,
        value: Value,
    ) -> Result<(), HeapError> {
        debug_assert!(
            matches!(value, Value::Null | Value::String(_))
                || (key == Self::MODE_FIELD && matches!(value, Value::Float(_) | Value::Bool(_)))
        );
        self.require_live(from)?;
        let key = EdgeKey::Field(key);
        if matches!(value, Value::Null) {
            self.clear_edge(from, &key)?;
            Ok(())
        } else {
            self.write_edge(from, key, EdgeValue::Scalar(value))
        }
    }

    /// Writes a table under a Lua string key.
    ///
    /// `__mode` is the reserved field. Any other text is compared in full,
    /// including interior NUL. The same text overwrites one record.
    pub fn set_named_edge(
        &mut self,
        from: TableRef,
        name: &str,
        to: TableRef,
    ) -> Result<(), HeapError> {
        self.write_table_edge(from, EdgeKey::named(name), to)
    }

    /// Writes one bare userdata under a Lua string key.
    ///
    /// This is the typed strong-edge form used by the private linked-finalizer
    /// fixture: a metatable field can name the exact live userdata generation.
    /// The write uses the same record, quota, and mutation gates as a table
    /// field. It does not issue a guest value.
    pub fn set_named_userdata(
        &mut self,
        from: TableRef,
        name: &str,
        value: UserdataRef,
    ) -> Result<(), HeapError> {
        self.require_live(from)?;
        self.require_userdata(value)?;
        self.deny_unbarriered()?;
        self.write_edge(
            from,
            EdgeKey::named(name),
            EdgeValue::Userdata {
                slot: value.slot(),
                generation: value.generation(),
            },
        )
    }

    /// Writes one closure under a Lua string key.
    ///
    /// This is the private strong-edge form used by the pending-finalizer raw
    /// `__gc` and `__call` fixtures: a metatable field can name the exact live
    /// closure generation. The write uses the same record, quota, and mutation
    /// gates as a table field. It does not issue a guest value and does not
    /// make that closure a callable.
    pub fn set_named_closure(
        &mut self,
        from: TableRef,
        name: &str,
        value: ClosureRef,
    ) -> Result<(), HeapError> {
        self.require_live(from)?;
        self.require_closure(value)?;
        self.deny_unbarriered()?;
        self.write_edge(
            from,
            EdgeKey::named(name),
            EdgeValue::Closure {
                slot: value.slot(),
                generation: value.generation(),
            },
        )
    }

    /// Writes a scalar under a Lua string key, or deletes that record on nil.
    ///
    /// A boolean may occupy any raw named field. A number remains only the
    /// reserved mode field. The direct numeric field API stays available for
    /// heap tests.
    pub fn set_named_scalar(
        &mut self,
        from: TableRef,
        name: &str,
        value: Value,
    ) -> Result<(), HeapError> {
        let key = EdgeKey::named(name);
        let mode = matches!(key, EdgeKey::Field(Self::MODE_FIELD));
        debug_assert!(
            matches!(value, Value::Null | Value::String(_) | Value::Bool(_))
                || (mode && matches!(value, Value::Float(_)))
        );
        self.require_live(from)?;
        if matches!(value, Value::Null) {
            self.clear_edge(from, &key)?;
            Ok(())
        } else {
            self.write_edge(from, key, EdgeValue::Scalar(value))
        }
    }

    /// Test-only: writes a stored nil under a named field without deleting the
    /// record. This is the same scalar-null representation weak publication
    /// leaves as a cursor gap; the private raw `__gc` lookup must treat it as
    /// absent rather than as a value.
    #[cfg(test)]
    pub(crate) fn set_named_gap_test_control(
        &mut self,
        from: TableRef,
        name: &str,
    ) -> Result<(), HeapError> {
        self.require_live(from)?;
        self.write_edge(from, EdgeKey::named(name), EdgeValue::Scalar(Value::Null))
    }

    /// Removes one ordinary UTF-8 field while a sweep tail is saved.
    ///
    /// [`Self::require_live`] runs before the tail check, so a stale or
    /// wrong-kind receiver is not reported as a missing tail. An unmarked live
    /// owner, a `__mode` name, a cursor gap, an internal closure or upvalue
    /// value, and an unmarked live table target are [`HeapError::SweepPending`].
    /// A table target is checked with the same generation, kind, and mark rule
    /// as the owner. Nothing is unlinked on those errors. One owner-chain walk
    /// borrows `name`; it does not construct a key. [`Self::unlink_key`] is not
    /// used, so its write gate stays on the ordinary delete path. A missing
    /// ordinary field is `Ok(false)`. One removed record is `Ok(true)`. This
    /// does not mark, resume the tail, or add a sweep count.
    pub(super) fn delete_published_named(
        &mut self,
        from: TableRef,
        name: &str,
    ) -> Result<bool, HeapError> {
        self.require_live(from)?;
        if self.sweep_tail.is_none() {
            return Err(HeapError::SweepDenied);
        }
        if !Self::slot_marked(&self.slots[from.slot() as usize].state) {
            return Err(HeapError::SweepPending);
        }
        if EdgeKey::is_mode_name(name) {
            return Err(HeapError::SweepPending);
        }
        let Some((prev, index)) = self.locate_named(from.slot(), name) else {
            return Ok(false);
        };
        if !self.published_name_unlinks(index)? {
            return Err(HeapError::SweepPending);
        }
        self.unlink_at(from.slot(), prev, index);
        Ok(true)
    }

    /// Replaces one ordinary UTF-8 gap while a sweep tail is saved.
    ///
    /// The owner checks match [`Self::delete_published_named`]: live, then a
    /// saved tail, then this round's mark, then the reserved mode name, then
    /// one borrowed [`Self::locate_named`]. A missing name is `Ok(false)` and
    /// does not reclaim storage or inspect `to`. A located record that is not
    /// ordinary UTF-8, or not a gap, is [`HeapError::SweepPending`]. Only a
    /// real hit checks `to`: domain, kind, generation, and physical live,
    /// then this round's mark, then the carrier [`Self::issued_value`] would
    /// refuse as [`HeapError::StaleHandle`]. One [`Self::replace_edge_value`]
    /// commits that table and returns `Ok(true)`. This does not mark, resume
    /// the tail, add a key, or call the ordinary writer.
    pub(super) fn refill_published_gap(
        &mut self,
        from: TableRef,
        name: &str,
        to: TableRef,
    ) -> Result<bool, HeapError> {
        self.require_live(from)?;
        if self.sweep_tail.is_none() {
            return Err(HeapError::SweepDenied);
        }
        if !Self::slot_marked(&self.slots[from.slot() as usize].state) {
            return Err(HeapError::SweepPending);
        }
        if EdgeKey::is_mode_name(name) {
            return Err(HeapError::SweepPending);
        }
        let Some((_, index)) = self.locate_named(from.slot(), name) else {
            return Ok(false);
        };
        if !self.published_utf8_gap(index) {
            return Err(HeapError::SweepPending);
        }
        self.require_live(to)?;
        if !Self::slot_marked(&self.slots[to.slot() as usize].state) {
            return Err(HeapError::SweepPending);
        }
        self.issued_value(to)?;
        self.replace_edge_value(
            from.slot(),
            index,
            EdgeValue::Table {
                slot: to.slot(),
                generation: to.generation(),
            },
        );
        Ok(true)
    }

    /// The located record is an ordinary UTF-8 cursor gap.
    fn published_utf8_gap(&self, index: u32) -> bool {
        let edge = self.edge_slots[index as usize]
            .edge
            .as_ref()
            .expect("owner chain holds an occupied record");
        matches!(edge.key, EdgeKey::Utf8(_)) && edge.value.is_cursor_gap()
    }

    fn slot_marked(state: &SlotState) -> bool {
        matches!(state, SlotState::Live { marked: true })
    }

    /// One borrowed-name walk. The predecessor is the argument `unlink_at` needs.
    fn locate_named(&self, owner: u32, name: &str) -> Option<(Option<u32>, u32)> {
        let mut prev = None;
        let mut cursor = self.slots.get(owner as usize)?.edge_head;
        while let Some(index) = cursor {
            let record = &self.edge_slots[index as usize];
            if record
                .edge
                .as_ref()
                .is_some_and(|edge| edge.key.matches_name(name))
            {
                return Some((prev, index));
            }
            prev = Some(index);
            cursor = record.next;
        }
        None
    }

    /// Whether the located record may be unlinked. A table target uses the
    /// existing handle check before the mark bit. Other refusals are `Ok(false)`.
    fn published_name_unlinks(&self, index: u32) -> Result<bool, HeapError> {
        let (utf8, value) = {
            let edge = self.edge_slots[index as usize]
                .edge
                .as_ref()
                .expect("owner chain holds an occupied record");
            let utf8 = matches!(edge.key, EdgeKey::Utf8(_));
            let value = match &edge.value {
                EdgeValue::Scalar(Value::Null) => PublishedName::Gap,
                EdgeValue::Closure { .. }
                | EdgeValue::Upvalue { .. }
                | EdgeValue::Userdata { .. } => PublishedName::Internal,
                EdgeValue::Table { slot, generation } => PublishedName::Table(*slot, *generation),
                EdgeValue::Scalar(_) => PublishedName::Scalar,
            };
            (utf8, value)
        };
        if !utf8 {
            return Ok(false);
        }
        match value {
            PublishedName::Gap | PublishedName::Internal => Ok(false),
            PublishedName::Scalar => Ok(true),
            PublishedName::Table(slot, generation) => {
                self.require_kind(self.domain, slot, generation, HandleKind::Table)?;
                Ok(Self::slot_marked(&self.slots[slot as usize].state))
            }
        }
    }

    /// True when the name constructor selects the reserved mode field.
    pub fn names_mode_field(name: &str) -> bool {
        matches!(EdgeKey::named(name), EdgeKey::Field(Self::MODE_FIELD))
    }

    /// Writes the non-reference forms of the direct `__mode` field.
    /// Nil deletes the record; other scalar values occupy the same edge quota.
    pub fn set_mode_scalar(&mut self, from: TableRef, value: Value) -> Result<(), HeapError> {
        debug_assert!(matches!(
            value,
            Value::Null | Value::String(_) | Value::Float(_) | Value::Bool(_)
        ));
        self.set_field_scalar(from, Self::MODE_FIELD, value)
    }

    /// Writes one object-key edge in the same record and quota as a field edge.
    ///
    /// The key is the live slot and generation. A structurally equal carrier is
    /// not a key: the caller classifies the issued array before this call.
    pub fn set_object_edge(
        &mut self,
        from: TableRef,
        key: TableRef,
        to: TableRef,
    ) -> Result<(), HeapError> {
        self.require_live(key)?;
        self.write_table_edge(from, EdgeKey::object(key), to)
    }

    /// Writes one string under an object key, in the same record and quota.
    ///
    /// Nil removal stays on [`Self::clear_object_edge`]. The string is not
    /// issued as a table token and is not a second edge table.
    pub fn set_object_scalar(
        &mut self,
        from: TableRef,
        key: TableRef,
        value: Value,
    ) -> Result<(), HeapError> {
        debug_assert!(matches!(value, Value::String(_)));
        self.require_live(from)?;
        self.require_live(key)?;
        self.write_edge(from, EdgeKey::object(key), EdgeValue::Scalar(value))
    }

    /// Removes one object-key record. Another key, including a later generation
    /// of the same slot, is a different identity and stays.
    pub fn clear_object_edge(&mut self, from: TableRef, key: TableRef) -> Result<(), HeapError> {
        self.require_live(from)?;
        self.require_live(key)?;
        let key = EdgeKey::object(key);
        self.clear_edge(from, &key)
    }

    /// Writes one table under a closure identity key.
    ///
    /// Same record and quota as a table object key. The key is the closure
    /// slot, generation, and kind. A private field id is not this key. Handle
    /// checks run before any stored entry is replaced.
    pub fn set_closure_key_edge(
        &mut self,
        from: TableRef,
        key: ClosureRef,
        to: TableRef,
    ) -> Result<(), HeapError> {
        self.require_closure(key)?;
        self.write_table_edge(from, EdgeKey::closure_key(key), to)
    }

    /// Writes one string under a closure identity key, in the same record.
    ///
    /// Nil removal stays on [`Self::clear_closure_key`]. A refused handle does
    /// not replace an existing entry.
    pub fn set_closure_key_scalar(
        &mut self,
        from: TableRef,
        key: ClosureRef,
        value: Value,
    ) -> Result<(), HeapError> {
        debug_assert!(matches!(value, Value::String(_)));
        self.require_live(from)?;
        self.require_closure(key)?;
        self.write_edge(from, EdgeKey::closure_key(key), EdgeValue::Scalar(value))
    }

    /// Writes one boolean, binary64, string, or bytes value under a bare userdata key.
    ///
    /// Nil removes that record through [`Self::clear_edge`]. It does not store
    /// a cursor gap, and a missing key allocates nothing. An integer, array,
    /// map, callable, or any other carrier is [`HeapError::WrongKind`] and
    /// leaves the table unchanged.
    ///
    /// The key is an object record with userdata kind. It is not a table key
    /// and not a numeric key. Handle checks run before the gray and published
    /// tail gates, and those gates run before value classification. A bad
    /// handle does not write and does not depend on phase. Nil uses the same
    /// gates. This write has no edge barrier, so a paused mark or saved tail
    /// is refused before the record changes.
    pub fn set_userdata_key_scalar(
        &mut self,
        from: TableRef,
        key: UserdataRef,
        value: Value,
    ) -> Result<(), HeapError> {
        self.require_live(from)?;
        self.require_userdata(key)?;
        self.deny_unbarriered()?;
        let stored = EdgeKey::userdata_key(key);
        match value {
            Value::Null => self.clear_edge(from, &stored),
            Value::Bool(_) | Value::Float(_) | Value::String(_) | Value::Bytes(_) => {
                self.write_edge(from, stored, EdgeValue::Scalar(value))
            }
            _ => Err(HeapError::WrongKind),
        }
    }

    /// Reads the scalar stored under a bare userdata key.
    ///
    /// A dead or reused key is `StaleHandle`. A stored userdata or other
    /// internal target is `WrongKind`, not a Core number or table.
    pub fn userdata_key_value(&self, from: TableRef, key: UserdataRef) -> Result<Value, HeapError> {
        self.require_userdata(key)?;
        let key = EdgeKey::userdata_key(key);
        self.stored_value(from, |stored| stored == &key)
    }

    /// Writes one bare userdata under a bare userdata key.
    ///
    /// This is the same-object `t[u] = u` form for the weak `kv` evidence.
    /// Key and value keep their own roles: the key is not a guest cursor, and
    /// a value that is value-dead cannot be published as a gap under it.
    pub fn set_userdata_key_userdata(
        &mut self,
        from: TableRef,
        key: UserdataRef,
        value: UserdataRef,
    ) -> Result<(), HeapError> {
        self.require_live(from)?;
        self.require_userdata(key)?;
        self.require_userdata(value)?;
        self.deny_unbarriered()?;
        self.write_edge(
            from,
            EdgeKey::userdata_key(key),
            EdgeValue::Userdata {
                slot: value.slot(),
                generation: value.generation(),
            },
        )
    }

    /// Removes one closure-key record. Another closure, including one that
    /// shares this key's cell, is a different identity and stays.
    pub fn clear_closure_key(&mut self, from: TableRef, key: ClosureRef) -> Result<(), HeapError> {
        self.require_live(from)?;
        self.require_closure(key)?;
        let key = EdgeKey::closure_key(key);
        self.clear_edge(from, &key)
    }

    /// Reads a closure-key edge. The key domain, kind, and generation must
    /// still name that live closure.
    pub fn closure_key_edge(
        &self,
        from: TableRef,
        key: ClosureRef,
    ) -> Result<Option<TableRef>, HeapError> {
        self.require_closure(key)?;
        let key = EdgeKey::closure_key(key);
        self.read_table_edge(from, |stored| stored == &key)
    }

    /// Reads a closure-key edge as the guest value.
    ///
    /// A stored string is that string. A stored table is its issued carrier.
    pub fn closure_key_value(&self, from: TableRef, key: ClosureRef) -> Result<Value, HeapError> {
        self.require_closure(key)?;
        let key = EdgeKey::closure_key(key);
        self.stored_value(from, |stored| stored == &key)
    }

    /// Writes one table under a binary64 key, in the same record as a field.
    ///
    /// NaN is refused before any lookup or insert. An integer resource carrier
    /// is not a number key; callers pass `f64` only for `Value::Float`.
    pub fn set_number_edge(
        &mut self,
        from: TableRef,
        key: f64,
        to: TableRef,
    ) -> Result<(), HeapError> {
        let key = Self::number_key(key)?;
        self.write_table_edge(from, key, to)
    }

    /// Writes one bare userdata under a binary64 key.
    ///
    /// NaN is refused before any lookup. `+0` and `-0` select the same record.
    /// The value stays a userdata identity; readers must not return it as a
    /// table or a Core number. Handle checks run before the gray and published
    /// tail gates. This write has no edge barrier, so those gates refuse it
    /// before the record changes.
    pub fn set_number_userdata(
        &mut self,
        from: TableRef,
        key: f64,
        value: UserdataRef,
    ) -> Result<(), HeapError> {
        let key = Self::number_key(key)?;
        self.require_live(from)?;
        self.require_userdata(value)?;
        self.deny_unbarriered()?;
        self.write_edge(
            from,
            key,
            EdgeValue::Userdata {
                slot: value.slot(),
                generation: value.generation(),
            },
        )
    }

    /// Reads the userdata stored under a binary64 key.
    ///
    /// NaN reads as missing after the table is confirmed live. A stored table,
    /// closure, or cell is `WrongKind`. A scalar, including a weak-cleared
    /// cursor, is missing. This does not claim a guest `rawget`.
    pub fn number_userdata(
        &self,
        from: TableRef,
        key: f64,
    ) -> Result<Option<UserdataRef>, HeapError> {
        self.require_live(from)?;
        if key.is_nan() {
            return Ok(None);
        }
        let key = EdgeKey::number(key);
        match self.read_edge(from, |stored| stored == &key)? {
            Some(EdgeValue::Userdata { slot, generation }) => {
                Ok(Some(UserdataRef::userdata(self.domain, *slot, *generation)))
            }
            Some(EdgeValue::Scalar(_)) | None => Ok(None),
            Some(
                EdgeValue::Table { .. } | EdgeValue::Closure { .. } | EdgeValue::Upvalue { .. },
            ) => Err(HeapError::WrongKind),
        }
    }

    /// Writes one string under a binary64 key. Nil removal stays on
    /// [`Self::clear_number_edge`].
    pub fn set_number_scalar(
        &mut self,
        from: TableRef,
        key: f64,
        value: Value,
    ) -> Result<(), HeapError> {
        debug_assert!(matches!(value, Value::String(_)));
        let key = Self::number_key(key)?;
        self.require_live(from)?;
        self.write_edge(from, key, EdgeValue::Scalar(value))
    }

    /// Removes one binary64 record. A NaN write is an error and does not delete.
    pub fn clear_number_edge(&mut self, from: TableRef, key: f64) -> Result<(), HeapError> {
        let key = Self::number_key(key)?;
        self.require_live(from)?;
        self.clear_edge(from, &key)
    }

    /// Reads a binary64 key. NaN reads nil after the table is confirmed live.
    /// `+0` and `-0` select the same record.
    pub fn number_value(&self, from: TableRef, key: f64) -> Result<Value, HeapError> {
        self.require_live(from)?;
        if key.is_nan() {
            return Ok(Value::Null);
        }
        let key = EdgeKey::number(key);
        self.stored_value(from, |stored| stored == &key)
    }

    /// First raw sequence border of this issued table.
    ///
    /// One walk of the owner's chain copies positive finite integer keys into
    /// a buffer. That buffer is at most the owner's degree, and the degree
    /// cannot exceed the edge quota, so a huge key does not size it. Strings,
    /// object keys, private fields, and cursor gaps are ignored. The walk does
    /// not read a metatable, use ordinary lookup, collect, or modify the chain.
    /// The length is not cached.
    ///
    /// A contiguous prefix returns that length. Several legal borders resolve
    /// to the first border of the prefix that starts at 1. No key 1 means 0.
    /// This is not a border that depends on table capacity or insert history.
    pub fn raw_sequence_len(&self, table: TableRef) -> Result<f64, HeapError> {
        self.require_live(table)?;
        Ok(f64::from(self.raw_sequence_border(table.slot())))
    }

    fn raw_sequence_border(&self, owner: u32) -> u32 {
        let cap = self.limits.edges;
        let mut keys = Vec::new();
        let mut cursor = self
            .slots
            .get(owner as usize)
            .and_then(|slot| slot.edge_head);
        while let Some(index) = cursor {
            let record = &self.edge_slots[index as usize];
            let edge = record
                .edge
                .as_ref()
                .expect("owner chain holds an occupied record");
            cursor = record.next;
            if edge.value.is_cursor_gap() {
                continue;
            }
            let Some(key) = sequence_index(&edge.key, cap) else {
                continue;
            };
            keys.push(key);
        }
        keys.sort_unstable();
        let mut expect = 1u32;
        for key in keys {
            if key == expect {
                if expect == cap {
                    return expect;
                }
                expect += 1;
            } else if key > expect {
                break;
            }
        }
        expect - 1
    }

    fn number_key(key: f64) -> Result<EdgeKey, HeapError> {
        if key.is_nan() {
            return Err(HeapError::NanIndex);
        }
        Ok(EdgeKey::number(key))
    }

    fn clear_edge(&mut self, from: TableRef, key: &EdgeKey) -> Result<(), HeapError> {
        self.unlink_key(from.slot(), key)
    }

    fn write_table_edge(
        &mut self,
        from: TableRef,
        key: EdgeKey,
        to: TableRef,
    ) -> Result<(), HeapError> {
        self.require_live(from)?;
        self.require_live(to)?;
        self.write_edge(
            from,
            key,
            EdgeValue::Table {
                slot: to.slot(),
                generation: to.generation(),
            },
        )
    }

    fn write_edge(
        &mut self,
        from: TableRef,
        key: EdgeKey,
        value: EdgeValue,
    ) -> Result<(), HeapError> {
        self.write_from(from.slot(), key, value)
    }

    fn write_from(
        &mut self,
        from_slot: u32,
        key: EdgeKey,
        value: EdgeValue,
    ) -> Result<(), HeapError> {
        self.ensure_sweep_open()?;
        if let Some(index) = self.find_key(from_slot, &key) {
            self.replace_edge_value(from_slot, index, value);
            return Ok(());
        }
        if self.occupied_edges >= self.limits.edges && !self.reclaim_cursor_gap() {
            return Err(HeapError::EdgeCapacity);
        }
        // A parked mark may have already scanned this owner. A new ordinary
        // string or number field with a table value needs this insertion barrier.
        let barrier = if self.mark.is_some()
            && matches!(key, EdgeKey::Utf8(_) | EdgeKey::Number(_))
            && let EdgeValue::Table { slot, generation } = &value
        {
            Some((*slot, *generation))
        } else {
            None
        };
        let table_key = if self.mark.is_some() {
            new_table_key_insert(&key, &value)
        } else {
            None
        };
        let added = key_payload(&key) + value_payload(&value);
        let index = self.take_edge_slot();
        self.edge_slots[index as usize].edge = Some(Edge { key, value });
        self.append_tail(from_slot, index);
        self.payload_bytes += added;
        self.occupied_edges += 1;
        self.live_edge_count += 1;
        self.note_mode_pointer(from_slot, index);
        self.note_edge_peak();
        if let Some((slot, generation)) = barrier {
            self.barrier_strong_field(from_slot, index, Some((slot, generation)));
        }
        if let Some(insert) = table_key {
            self.barrier_new_table_key(
                from_slot,
                index,
                insert.key_slot,
                insert.key_generation,
                insert.value,
            );
        }
        Ok(())
    }

    /// Drops one valueless cursor so a real edge can use that storage.
    /// Lua does not keep an old cursor valid across this structural change.
    /// The search is one owner-chain walk and stops at the first gap. The
    /// removal uses the same unlink as an explicit delete.
    fn reclaim_cursor_gap(&mut self) -> bool {
        for owner in 0..self.slots.len() {
            let mut prev = None;
            let mut cursor = self.slots[owner].edge_head;
            while let Some(index) = cursor {
                let next = self.edge_slots[index as usize].next;
                let gap = self.edge_slots[index as usize]
                    .edge
                    .as_ref()
                    .is_some_and(|edge| edge.value.is_cursor_gap());
                if gap {
                    self.unlink_at(owner as u32, prev, index);
                    return true;
                }
                prev = Some(index);
                cursor = next;
            }
        }
        false
    }

    /// Reads the table target of a direct field through either alias of `from`.
    pub fn edge(&self, from: TableRef, key: u32) -> Result<Option<TableRef>, HeapError> {
        let key = EdgeKey::Field(key);
        self.read_table_edge(from, |stored| stored == &key)
    }

    /// Reads a Lua string key as a table target. `__mode` is the reserved field.
    ///
    /// The name is borrowed for the comparison. This does not construct a key.
    pub fn named_edge(&self, from: TableRef, name: &str) -> Result<Option<TableRef>, HeapError> {
        self.read_table_edge(from, |stored| stored.matches_name(name))
    }

    /// Reads a Lua string key as the stored guest value, or nil when absent.
    ///
    /// The name is borrowed for the comparison. This does not construct a key.
    /// A cloned string in the returned value is the stored scalar, not the lookup.
    pub fn named_value(&self, from: TableRef, name: &str) -> Result<Value, HeapError> {
        self.stored_value(from, |stored| stored.matches_name(name))
    }

    /// Next raw entry of `table` after `cursor`, in owner-chain order.
    ///
    /// `Value::Null` starts at the first valued edge. `None` is the end: one
    /// nil, not an empty pair. A weak-cleared entry keeps its key in that order
    /// with no value, so `rawget` is nil, a fresh `next` skips it, and `next`
    /// with that key continues at the next valued entry or one nil at the end.
    /// A missing key and a non-guest key are errors, not the end and not a
    /// restart. An explicit removal drops the key. A later write of the same
    /// key reuses that record in place. A new key, including a key that was
    /// deleted, appends at the tail even when the record reuses a low slot.
    /// Inserting a different key may drop an old cursor when edge storage is
    /// full; Lua does not keep cursors across that change. This walks the
    /// owner's chain. It does not allocate another table and does not read a
    /// metamethod.
    pub fn next_guest(
        &self,
        table: TableRef,
        cursor: &Value,
    ) -> Result<Option<(Value, Value)>, HeapError> {
        self.require_live(table)?;
        let slot = table.slot();
        match cursor {
            Value::Null => self.next_after(slot, None),
            Value::Bool(_) => Err(HeapError::NextUnsupported),
            Value::String(name) => self.next_after(slot, Some(NextCursor::Name(name.as_str()))),
            Value::Float(number) => {
                if number.is_nan() {
                    return Err(HeapError::NanIndex);
                }
                let bits = if *number == 0.0 { 0 } else { number.to_bits() };
                self.next_after(slot, Some(NextCursor::Number(bits)))
            }
            other => match self.guest_class(other) {
                GuestClass::Table(key) => {
                    self.require_live(key)?;
                    self.next_after(
                        slot,
                        Some(NextCursor::Object {
                            slot: key.slot(),
                            generation: key.generation(),
                            kind: key.kind(),
                        }),
                    )
                }
                _ => Err(HeapError::NextUnsupported),
            },
        }
    }

    fn next_after(
        &self,
        slot: u32,
        cursor: Option<NextCursor<'_>>,
    ) -> Result<Option<(Value, Value)>, HeapError> {
        let mut passed = cursor.is_none();
        let mut index = self
            .slots
            .get(slot as usize)
            .and_then(|owner| owner.edge_head);
        while let Some(current) = index {
            let record = &self.edge_slots[current as usize];
            let edge = record
                .edge
                .as_ref()
                .expect("owner chain holds an occupied record");
            index = record.next;
            if !passed {
                if cursor
                    .as_ref()
                    .is_some_and(|cursor| cursor.matches(&edge.key))
                {
                    passed = true;
                }
                continue;
            }
            if edge.value.is_cursor_gap() {
                continue;
            }
            return Ok(Some((
                self.guest_key(&edge.key)?,
                self.guest_edge_value(&edge.value)?,
            )));
        }
        if !passed {
            return Err(HeapError::NextCursorMissing);
        }
        Ok(None)
    }

    fn guest_key(&self, key: &EdgeKey) -> Result<Value, HeapError> {
        match key {
            EdgeKey::Field(field) if *field == Self::MODE_FIELD => Ok(Value::string("__mode")),
            EdgeKey::Field(_) => Err(HeapError::NextUnsupported),
            EdgeKey::Utf8(text) => Ok(Value::string(text)),
            EdgeKey::Number(bits) => Ok(Value::Float(f64::from_bits(*bits))),
            EdgeKey::Object {
                slot,
                generation,
                kind: HandleKind::Table,
            } => self.issued_value(TableRef::table(self.domain, *slot, *generation)),
            EdgeKey::Object { .. } => Err(HeapError::NextUnsupported),
        }
    }

    fn guest_edge_value(&self, value: &EdgeValue) -> Result<Value, HeapError> {
        match value {
            EdgeValue::Scalar(value) => Ok(value.clone()),
            EdgeValue::Table { slot, generation } => {
                self.issued_value(TableRef::table(self.domain, *slot, *generation))
            }
            EdgeValue::Closure { .. } | EdgeValue::Upvalue { .. } | EdgeValue::Userdata { .. } => {
                Err(HeapError::NextUnsupported)
            }
        }
    }

    /// Reads an object-key edge. The key generation must still be that live slot.
    pub fn object_edge(
        &self,
        from: TableRef,
        key: TableRef,
    ) -> Result<Option<TableRef>, HeapError> {
        self.require_live(key)?;
        let key = EdgeKey::object(key);
        self.read_table_edge(from, |stored| stored == &key)
    }

    /// Reads an object-key edge as the guest value.
    ///
    /// A stored string is that string. It is not decoded as a table token.
    pub fn object_value(&self, from: TableRef, key: TableRef) -> Result<Value, HeapError> {
        self.require_live(key)?;
        let key = EdgeKey::object(key);
        self.stored_value(from, |stored| stored == &key)
    }

    fn stored_value(
        &self,
        from: TableRef,
        matches_key: impl Fn(&EdgeKey) -> bool,
    ) -> Result<Value, HeapError> {
        match self.read_edge(from, matches_key)? {
            Some(EdgeValue::Table { slot, generation }) => {
                self.issued_value(TableRef::table(self.domain, *slot, *generation))
            }
            Some(EdgeValue::Scalar(value)) => Ok(value.clone()),
            Some(
                EdgeValue::Closure { .. } | EdgeValue::Upvalue { .. } | EdgeValue::Userdata { .. },
            ) => Err(HeapError::WrongKind),
            None => Ok(Value::Null),
        }
    }

    fn read_table_edge(
        &self,
        from: TableRef,
        matches_key: impl Fn(&EdgeKey) -> bool,
    ) -> Result<Option<TableRef>, HeapError> {
        match self.read_edge(from, matches_key)? {
            Some(EdgeValue::Table { slot, generation }) => {
                Ok(Some(TableRef::table(self.domain, *slot, *generation)))
            }
            Some(EdgeValue::Scalar(_)) | None => Ok(None),
            Some(
                EdgeValue::Closure { .. } | EdgeValue::Upvalue { .. } | EdgeValue::Userdata { .. },
            ) => Err(HeapError::WrongKind),
        }
    }

    fn stored_from(&self, from_slot: u32, key: &EdgeKey) -> Option<&EdgeValue> {
        let index = self.find_key(from_slot, key)?;
        self.edge_slots[index as usize]
            .edge
            .as_ref()
            .map(|edge| &edge.value)
    }

    /// One raw stored value under a Lua string key through the owner's chain.
    ///
    /// A missing key returns `None`; a stored `Value::Null` gap still returns a
    /// value record, so the caller can tell an absent field from a true
    /// stored-nil gap. This does not consult `__index`, a metatable, or the
    /// selection cache, and it does not construct a key. The owner-chain walk
    /// is linear in that owner's stored edge count.
    fn raw_named_value(&self, owner: u32, name: &str) -> Option<&EdgeValue> {
        let index = self.find_matching(owner, |key| key.matches_name(name))?;
        self.edge_slots
            .get(index as usize)?
            .edge
            .as_ref()
            .map(|edge| &edge.value)
    }

    /// One walk of the owner's chain. The predicate selects the stored key.
    /// This does not build another table.
    fn read_edge(
        &self,
        from: TableRef,
        matches_key: impl Fn(&EdgeKey) -> bool,
    ) -> Result<Option<&EdgeValue>, HeapError> {
        self.require_live(from)?;
        let Some(index) = self.find_matching(from.slot(), matches_key) else {
            return Ok(None);
        };
        Ok(self.edge_slots[index as usize]
            .edge
            .as_ref()
            .map(|edge| &edge.value))
    }

    /// Sets or clears the metatable ref. A table target is not encoded as a
    /// field key. Replacing one ref does not allocate another record. This
    /// does not reject a cycle.
    pub fn set_metatable(
        &mut self,
        owner: TableRef,
        mt: Option<TableRef>,
    ) -> Result<(), HeapError> {
        self.require_live(owner)?;
        if let Some(target) = mt {
            self.require_live(target)?;
        }
        self.ensure_sweep_open()?;
        let old_tuple = self.slots[owner.slot() as usize].metatable;
        let new_tuple = mt.map(|target| (target.slot(), target.generation()));
        // A metatable first attached while mutation is open can have a direct
        // `__mode` that the reset walk never read: with no metatable attached
        // at Reset, the walk parses nothing. Register that read before the
        // owner can be opened, and let the transition check below see the
        // queued parse as unknown rather than as a strong default.
        if let Some(target) = mt {
            self.note_attached_mode(target.slot(), target.generation());
        }
        let rescan = self.needs_owner_rescan(owner.slot(), old_tuple, new_tuple);
        let had = old_tuple.is_some();
        if had != new_tuple.is_some() {
            if new_tuple.is_some() {
                self.metatable_edges = self.metatable_edges.saturating_add(1);
            } else {
                self.metatable_edges = self.metatable_edges.saturating_sub(1);
            }
        }
        self.slots[owner.slot() as usize].metatable = new_tuple;
        if rescan {
            self.schedule_owner_rescan(owner.slot());
        }
        if let Some(target) = mt {
            self.barrier_strong_metatable(owner.slot(), target.slot(), target.generation());
        }
        Ok(())
    }

    /// Reads the stored metatable ref. The caller loads the canonical carrier.
    pub fn metatable(&self, owner: TableRef) -> Result<Option<TableRef>, HeapError> {
        self.require_live(owner)?;
        Ok(self.slots[owner.slot() as usize]
            .metatable
            .map(|(slot, generation)| TableRef::table(self.domain, slot, generation)))
    }

    /// Attaches or clears one metatable reference on a typed userdata.
    ///
    /// This is the minimal fixture wiring for the private pending-finalizer
    /// slice, not a guest metamethod path. A userdata has no owner chain, so
    /// the reference is stored only in the one `metatable` tuple. The target
    /// must be a live table in this heap. A new attachment counts in
    /// `metatable_edges`, the same flag that lets reset parse direct
    /// `__mode` fields; a replacement that keeps the reference present does
    /// not change the count. A parked mark or a saved published tail is
    /// refused before the slot changes, matching the unbarriered
    /// `alloc_userdata` contract. The edge is strong: `open_owner` follows it
    /// when the userdata is marked, and the pending selection marks the
    /// selected userdata after the ordinary mark converges.
    pub(crate) fn set_userdata_metatable(
        &mut self,
        userdata: UserdataRef,
        mt: Option<TableRef>,
    ) -> Result<(), HeapError> {
        self.require_userdata(userdata)?;
        if let Some(target) = mt {
            self.require_live(target)?;
        }
        self.deny_unbarriered()?;
        let index = userdata.slot() as usize;
        let old = self.slots[index].metatable;
        let new = mt.map(|target| (target.slot(), target.generation()));
        if old.is_some() != new.is_some() {
            if new.is_some() {
                self.metatable_edges = self.metatable_edges.saturating_add(1);
            } else {
                self.metatable_edges = self.metatable_edges.saturating_sub(1);
            }
        }
        self.slots[index].metatable = new;
        Ok(())
    }

    /// The metatable reference stored on one typed userdata.
    ///
    /// This is a raw slot read, not a guest `getmetatable`. A missing
    /// reference is `Ok(None)`.
    pub(crate) fn userdata_metatable(
        &self,
        userdata: UserdataRef,
    ) -> Result<Option<TableRef>, HeapError> {
        self.require_userdata(userdata)?;
        Ok(self.slots[userdata.slot() as usize]
            .metatable
            .map(|(slot, generation)| TableRef::table(self.domain, slot, generation)))
    }

    /// Clones the slot's issued carrier for a field read. This does not add a root.
    pub fn issued_value(&self, table: TableRef) -> Result<Value, HeapError> {
        self.require_live(table)?;
        let carrier = match self.slots[table.slot() as usize]
            .carrier
            .as_ref()
            .ok_or(HeapError::StaleHandle)?
        {
            SlotCarrier::Table(carrier) => carrier,
            SlotCarrier::Callable(_) => return Err(HeapError::StaleHandle),
        };
        Ok(Value::Array(Arc::clone(carrier)))
    }

    /// Opens a root frame of `width` slots. Failure does not grow root storage.
    pub fn enter_roots(&mut self, width: u32) -> Result<RootFrameId, HeapError> {
        if width == 0 || self.next_frame_generation == 0 {
            return Err(HeapError::StaleFrame);
        }
        let start = u32::try_from(self.roots.len()).map_err(|_| HeapError::RootCapacity)?;
        let end = start.checked_add(width).ok_or(HeapError::RootCapacity)?;
        if end > self.limits.roots {
            return Err(HeapError::RootCapacity);
        }
        self.ensure_sweep_open()?;
        self.roots.resize(end as usize, None);
        let generation = self.next_frame_generation;
        self.next_frame_generation = self.next_frame_generation.saturating_add(1);
        let index = u32::try_from(self.frames.len()).map_err(|_| HeapError::RootCapacity)?;
        self.frames.push(Frame {
            index,
            generation,
            start,
            width,
        });
        self.note_root_peak();
        Ok(RootFrameId { index, generation })
    }

    /// The open top frame, if any.
    pub fn current_frame(&self) -> Option<RootFrameId> {
        self.frames.last().map(|frame| RootFrameId {
            index: frame.index,
            generation: frame.generation,
        })
    }

    /// Stores `handle` in an open frame. A closure uses [`Self::write_closure_root`].
    /// An upvalue uses [`Self::write_upvalue_root`]. The frame does not have to
    /// be the top frame; a stale id is refused.
    pub fn write_root(
        &mut self,
        frame: RootFrameId,
        slot: u32,
        handle: TableRef,
    ) -> Result<(), HeapError> {
        self.require_live(handle)?;
        self.store_root(frame, slot, Rooted::Table(handle))
    }

    /// Stores a closure in an open frame. Same vector and quota as a table root.
    ///
    /// A stale, foreign, or wrong-kind handle is refused before the slot changes.
    pub fn write_closure_root(
        &mut self,
        frame: RootFrameId,
        slot: u32,
        handle: ClosureRef,
    ) -> Result<(), HeapError> {
        self.require_closure(handle)?;
        self.store_root(frame, slot, Rooted::Closure(handle))
    }

    /// Stores an upvalue in an open frame. Same vector, quota, and collector
    /// as a table or closure root.
    ///
    /// The slot roots the cell. It does not copy or clear the current edge.
    /// A stale, foreign, or wrong-kind handle is refused before the slot changes.
    pub fn write_upvalue_root(
        &mut self,
        frame: RootFrameId,
        slot: u32,
        handle: UpvalueRef,
    ) -> Result<(), HeapError> {
        self.require_upvalue(handle)?;
        self.store_root(frame, slot, Rooted::Upvalue(handle))
    }

    /// Stores a bare userdata in an open frame. Same vector and quota as a
    /// table root.
    ///
    /// A stale, foreign, or wrong-kind handle is refused before the slot
    /// changes. The existing root barrier marks the node while mutation is
    /// open. This does not write an edge.
    pub fn write_userdata_root(
        &mut self,
        frame: RootFrameId,
        slot: u32,
        handle: UserdataRef,
    ) -> Result<(), HeapError> {
        self.require_userdata(handle)?;
        self.store_root(frame, slot, Rooted::Userdata(handle))
    }

    fn store_root(
        &mut self,
        frame: RootFrameId,
        slot: u32,
        rooted: Rooted,
    ) -> Result<(), HeapError> {
        let found = self.frame(frame)?;
        if slot >= found.width {
            return Err(HeapError::StaleFrame);
        }
        self.ensure_sweep_open()?;
        let index = found.start as usize + slot as usize;
        self.roots[index] = Some(rooted);
        self.note_parked_root_store(rooted);
        Ok(())
    }

    /// Stores one marked, published table in the direct caller's empty slot.
    ///
    /// Only a saved Release tail admits this. Phase, frame, slot, emptiness,
    /// liveness, and the mark bit are all checked before the assignment. The
    /// ordinary writer is not used: this does not call `ensure_sweep_open`,
    /// does not park a mark, and does not move the tail cursor, its accounted
    /// root count, the scan meter, or the frame stack.
    #[cfg(test)]
    pub(super) fn store_release_caller_root(
        &mut self,
        caller: RootFrameId,
        slot: u32,
        table: TableRef,
    ) -> Result<(), HeapError> {
        let release = self
            .sweep_tail
            .as_ref()
            .is_some_and(|tail| tail.phase() == sweep::TailPhase::Release);
        if self.sweep_tail.is_none() {
            return Err(HeapError::SweepDenied);
        }
        if !release {
            return Err(HeapError::SweepPending);
        }
        let Some(direct) = self.frames.len().checked_sub(2) else {
            return Err(HeapError::StaleFrame);
        };
        let found = &self.frames[direct];
        if found.index != caller.index || found.generation != caller.generation {
            return Err(HeapError::StaleFrame);
        }
        if slot >= found.width {
            return Err(HeapError::StaleFrame);
        }
        let index = found.start as usize + slot as usize;
        if self.roots.get(index).is_none() {
            return Err(HeapError::StaleFrame);
        }
        if self.roots[index].is_some() {
            return Err(HeapError::SweepPending);
        }
        self.require_live(table)?;
        if !Self::slot_marked(&self.slots[table.slot() as usize].state) {
            return Err(HeapError::SweepPending);
        }
        // The binding must still be on this slot. Core usability is a separate
        // check and is not implied by this raw id.
        self.published_token(table)?;
        self.roots[index] = Some(Rooted::Table(table));
        Ok(())
    }

    /// True when this top-frame slot can receive one automatic empty table.
    ///
    /// Release must already have visited a dead slot and left a suffix. The
    /// slot must be the current top frame's empty fixed root. A Roots pause,
    /// a cursor that has not released, or a finished suffix is `Ok(false)`
    /// and does not allocate. An occupied or missing slot is an error and
    /// does not allocate either.
    #[cfg(test)]
    pub(super) fn automatic_release_slot_admitted(&self, slot: u32) -> Result<bool, HeapError> {
        let Some(tail) = self.sweep_tail.as_ref() else {
            return Ok(false);
        };
        if tail.phase() != sweep::TailPhase::Release {
            return Ok(false);
        }
        let cursor = tail.cursor();
        if cursor == 0 || cursor >= tail.dead_len() {
            return Ok(false);
        }
        self.release_top_slot_empty(slot)?;
        Ok(true)
    }

    /// Stores one unmarked reservation in the current top frame's empty slot.
    ///
    /// This is not [`Self::store_release_caller_root`]: that path still requires
    /// a marked table in the direct caller. This write does not call
    /// `ensure_sweep_open`, does not park a mark, and does not move the Release
    /// cursor, the mark count, or the accounted root count.
    #[cfg(test)]
    pub(super) fn store_release_top_root(
        &mut self,
        slot: u32,
        table: TableRef,
    ) -> Result<(), HeapError> {
        if !self.automatic_release_slot_admitted(slot)? {
            return Err(HeapError::Scope(
                "release handoff root was not empty at store".to_string(),
            ));
        }
        self.require_live(table)?;
        let index = table.slot() as usize;
        if Self::slot_marked(&self.slots[index].state) {
            return Err(HeapError::Scope(
                "release handoff root table was marked".to_string(),
            ));
        }
        let frame = self.frames.last().expect("admission found the top frame");
        let root_index = frame.start as usize + slot as usize;
        self.roots[root_index] = Some(Rooted::Table(table));
        Ok(())
    }

    #[cfg(test)]
    fn release_top_slot_empty(&self, slot: u32) -> Result<(), HeapError> {
        let Some(frame) = self.frames.last() else {
            return Err(HeapError::NoRootFrame);
        };
        if slot >= frame.width {
            return Err(HeapError::StaleFrame);
        }
        let index = frame.start as usize + slot as usize;
        if self.roots.get(index).is_none() {
            return Err(HeapError::StaleFrame);
        }
        if self.roots[index].is_some() {
            return Err(HeapError::Scope(format!(
                "release handoff slot {slot} is occupied"
            )));
        }
        Ok(())
    }

    /// Drops one late node that was committed and then refused. The Core token
    /// must already be closed. Does not move the Release cursor or mark count.
    #[cfg(test)]
    pub(super) fn undo_committed_late(&mut self, table: TableRef) -> Result<(), HeapError> {
        self.ensure_published_tail()?;
        self.require_live(table)?;
        let index = table.slot() as usize;
        if Self::slot_marked(&self.slots[index].state) {
            return Err(HeapError::SweepPending);
        }
        self.sweep_tail
            .as_mut()
            .expect("published tail accepted the undo")
            .undo_late();
        self.slots[index].carrier = None;
        self.slots[index].token = None;
        self.release_slot(table.slot());
        Ok(())
    }

    #[cfg(test)]
    pub fn saved_tail_accounting(&self) -> Option<(u32, u32)> {
        let tail = self.sweep_tail.as_ref()?;
        Some((tail.marked_nodes(), tail.late_nodes()))
    }

    #[cfg(test)]
    pub fn top_frame_tables(&self) -> Vec<Option<TableRef>> {
        let Some(frame) = self.frames.last() else {
            return Vec::new();
        };
        let start = frame.start as usize;
        (0..frame.width)
            .map(|slot| {
                let index = start + slot as usize;
                match self.roots.get(index) {
                    Some(Some(Rooted::Table(table))) => Some(*table),
                    _ => None,
                }
            })
            .collect()
    }

    /// Clears one slot of an open frame. The frame does not have to be the
    /// top frame. Other slots, including another slot holding the same table,
    /// stay. A stale id or an out-of-range slot is refused. An empty slot
    /// stays empty.
    pub fn clear_root(&mut self, frame: RootFrameId, slot: u32) -> Result<(), HeapError> {
        let found = self.frame(frame)?;
        if slot >= found.width {
            return Err(HeapError::StaleFrame);
        }
        self.ensure_sweep_open()?;
        let index = found.start as usize + slot as usize;
        self.roots[index] = None;
        Ok(())
    }

    /// Drops the top frame. A frame that is not top is refused.
    pub fn leave_roots(&mut self, frame: RootFrameId) -> Result<(), HeapError> {
        let top = self.frames.last().ok_or(HeapError::NoRootFrame)?;
        if top.index != frame.index || top.generation != frame.generation {
            return Err(HeapError::StaleFrame);
        }
        self.ensure_sweep_open()?;
        let start = top.start as usize;
        self.roots.truncate(start);
        self.frames.pop();
        Ok(())
    }

    /// Drops the top frame while a published tail is saved.
    ///
    /// The frame is checked before the tail. No frame is [`HeapError::NoRootFrame`].
    /// A frame that is not top, or an old generation, is [`HeapError::StaleFrame`].
    /// A valid top frame with no tail is [`HeapError::SweepDenied`]. Refusal
    /// does not change the cursor, counts, root vector, or frame stack.
    ///
    /// On success the tail adjusts its own prefix, then this truncates the root
    /// vector to the frame start and pops the frame. It does not resume the sweep.
    pub(super) fn retire_published_root_frame(
        &mut self,
        frame: RootFrameId,
    ) -> Result<FrameRetirement, HeapError> {
        let (start, width) = {
            let top = self.frames.last().ok_or(HeapError::NoRootFrame)?;
            if top.index != frame.index || top.generation != frame.generation {
                return Err(HeapError::StaleFrame);
            }
            if self.sweep_tail.is_none() {
                return Err(HeapError::SweepDenied);
            }
            (top.start as usize, top.width as usize)
        };
        let mut tail = self.sweep_tail.take().expect("published tail");
        let retirement = tail.retire_frame_prefix(&self.roots, start, width);
        self.sweep_tail = Some(tail);
        self.roots.truncate(start);
        self.frames.pop();
        Ok(retirement)
    }

    /// Records the typed token that owns `table`. The handle is not installed
    /// as a root.
    pub fn bind_token(&mut self, table: TableRef, raw: u64) -> Result<(), HeapError> {
        self.require_live(table)?;
        if self.slots[table.slot() as usize].token.is_some() {
            return Err(HeapError::StaleHandle);
        }
        self.ensure_sweep_open()?;
        self.slots[table.slot() as usize].token = Some(raw);
        Ok(())
    }

    pub(crate) fn published_token(&self, table: TableRef) -> Result<u64, HeapError> {
        self.require_live(table)?;
        self.slots[table.slot() as usize]
            .token
            .ok_or(HeapError::StaleHandle)
    }

    /// Records the typed token that owns `userdata`. The handle is not
    /// installed as a root.
    ///
    /// The caller must already have confirmed that the raw handle names this
    /// exact userdata through its typed resource payload. The issuer does that
    /// check before binding; this raw write cannot repeat it without a
    /// `HostContext`.
    pub(crate) fn bind_userdata_token(
        &mut self,
        userdata: UserdataRef,
        raw: u64,
    ) -> Result<(), HeapError> {
        self.require_userdata(userdata)?;
        let index = userdata.slot() as usize;
        if self.slots[index].token.is_some() {
            return Err(HeapError::StaleHandle);
        }
        self.ensure_sweep_open()?;
        if let Some(pending) = self.pending_userdata
            && pending.slot == userdata.slot()
        {
            if pending.generation != userdata.generation() {
                return Err(HeapError::StaleHandle);
            }
            if pending.raw.is_some_and(|staged| staged != raw) {
                return Err(HeapError::TokenMismatch);
            }
            self.pending_userdata = None;
        }
        self.slots[index].token = Some(raw);
        Ok(())
    }

    /// The raw token stored for one live typed userdata.
    ///
    /// A missing binding is [`HeapError::StaleHandle`]. A handle from another
    /// domain, of another kind, or at an old generation is rejected first.
    pub(crate) fn published_userdata_token(&self, userdata: UserdataRef) -> Result<u64, HeapError> {
        self.require_userdata(userdata)?;
        self.slots[userdata.slot() as usize]
            .token
            .ok_or(HeapError::StaleHandle)
    }

    /// The exact live userdata that one mark-time token record still names.
    ///
    /// The collector reads this before it closes a userdata resource: the slot
    /// must be live at the captured generation and kind and must still store
    /// the captured raw handle. This is one slot identity read, counted in
    /// `token_slot_checks`; it does not search the slot vector.
    pub(super) fn bound_userdata_for_token(
        &mut self,
        token: &DoomedToken,
    ) -> Result<UserdataRef, HeapError> {
        self.scan.token_slot_checks += 1;
        let matches = self.slots.get(token.slot as usize).is_some_and(|slot| {
            slot.generation == token.generation
                && slot.kind == HandleKind::Userdata
                && slot.token == Some(token.raw)
                && matches!(slot.state, SlotState::Live { .. })
        });
        if !matches {
            return Err(HeapError::StaleHandle);
        }
        Ok(UserdataRef::userdata(
            self.domain,
            token.slot,
            token.generation,
        ))
    }

    /// Clears the token stored on `userdata` without searching other slots.
    /// Does not close a host resource and does not sweep. The host collector
    /// uses [`Self::clear_exact_token`] instead.
    pub(crate) fn unbind_userdata_token(&mut self, userdata: UserdataRef) -> Result<(), HeapError> {
        self.require_userdata(userdata)?;
        self.ensure_sweep_open()?;
        self.slots[userdata.slot() as usize].token = None;
        Ok(())
    }

    /// Reserves one userdata slot for the typed issuer and records the exact
    /// reservation. The record is not a published binding: an ordinary borrow,
    /// the pending-close enumeration, and the sweep do not read it.
    ///
    /// A later failure can roll this slot back with
    /// [`Self::rollback_empty_userdata`], which validates the record and the
    /// empty slot in O(1) and does not scan the roots or the owner chains.
    pub(super) fn reserve_userdata(&mut self) -> Result<UserdataRef, HeapError> {
        if self.pending_userdata.is_some() {
            return Err(HeapError::Scope(
                "a userdata reservation is already in flight".to_string(),
            ));
        }
        let userdata = self.alloc_userdata()?;
        self.pending_userdata = Some(PendingUserdata {
            slot: userdata.slot(),
            generation: userdata.generation(),
            raw: None,
        });
        Ok(userdata)
    }

    /// Attaches the registered Core raw handle to the exact reservation,
    /// before the slot binding is published.
    pub(super) fn stage_userdata_token(
        &mut self,
        userdata: UserdataRef,
        raw: u64,
    ) -> Result<(), HeapError> {
        self.require_userdata(userdata)?;
        let Some(pending) = self.pending_userdata.as_mut() else {
            return Err(HeapError::StaleHandle);
        };
        if pending.slot != userdata.slot() || pending.generation != userdata.generation() {
            return Err(HeapError::StaleHandle);
        }
        pending.raw = Some(raw);
        Ok(())
    }

    /// The exact reservation this heap is holding, if any.
    ///
    /// Returns the private handle and the staged Core raw (`None` before a
    /// successful registration). The record is only returned when it names a
    /// live, empty userdata at its recorded generation, of kind userdata,
    /// with no published token, edge, metatable, mode pointer, or carrier.
    /// Recovery validates the exact owner before it closes the staged token,
    /// and must not close a token for a record whose slot it cannot release.
    /// A saved mark or a saved release tail owns the graph, so both refuse
    /// recovery before any close: the caller keeps the record unchanged.
    /// This is one record check, not a heap scan.
    pub(super) fn pending_userdata_reservation(
        &self,
    ) -> Result<Option<(UserdataRef, Option<u64>)>, HeapError> {
        let Some(pending) = self.pending_userdata else {
            return Ok(None);
        };
        self.ensure_sweep_open()?;
        if self.mark.is_some() {
            return Err(HeapError::MarkDenied);
        }
        let Some(slot) = self.slots.get(pending.slot as usize) else {
            return Err(HeapError::StaleHandle);
        };
        if slot.generation != pending.generation
            || slot.kind != HandleKind::Userdata
            || slot.token.is_some()
            || slot.edge_head.is_some()
            || slot.edge_tail.is_some()
            || slot.metatable.is_some()
            || slot.mode_edge.is_some()
            || slot.carrier.is_some()
            || !matches!(slot.state, SlotState::Live { .. })
        {
            return Err(HeapError::StaleHandle);
        }
        Ok(Some((
            UserdataRef::userdata(self.domain, pending.slot, pending.generation),
            pending.raw,
        )))
    }

    /// The slot of a registered reservation that the current mark must
    /// retain, if the record still names the exact live userdata.
    ///
    /// A registered reservation (`raw` is `Some`) is a host-retained node: it
    /// is not published and not guest-visible, but a normal collection must
    /// not release its slot or strand its Core token without the record that
    /// can close it. An unregistered empty reservation (`raw` is `None`) is
    /// not retained: it owns no Core resource, and a collection may reclaim
    /// it. Domain is the heap's own, so checking the record plus this slot is
    /// the whole identity check. This does not search other slots.
    fn registered_pending_userdata_slot(&self) -> Option<u32> {
        let pending = self.pending_userdata?;
        pending.raw?;
        let slot = self.slots.get(pending.slot as usize)?;
        #[cfg(test)]
        self.selection_slot_trace.borrow_mut().push(pending.slot);
        if slot.generation == pending.generation
            && slot.kind == HandleKind::Userdata
            && slot.token.is_none()
            && matches!(slot.state, SlotState::Live { .. })
        {
            Some(pending.slot)
        } else {
            None
        }
    }

    /// Examines one live-userdata chain node for the private pending-finalizer
    /// selection, without reading the metatable edge chain.
    ///
    /// A candidate is a live, unmarked userdata that already published a
    /// typed Core token, is not pending or settled yet, and stores a live
    /// table metatable at its exact generation. The raw `__gc` lookup is a
    /// separate metered walk; this function reads one slot record only. A
    /// missing token, a table owner, an absent metatable, or a dead metatable
    /// is not a candidate. A missing or nil raw `__gc` is still a candidate:
    /// it is the settlement arm, not a non-candidate.
    pub(super) fn pending_finalizer_candidate(&self, index: usize) -> Option<PendingCandidate> {
        let slot = self.slots.get(index)?;
        if !matches!(slot.state, SlotState::Live { marked: false }) {
            return None;
        }
        if slot.kind != HandleKind::Userdata || slot.pending_finalizer || slot.finalizer_settled {
            return None;
        }
        slot.token?;
        let (metatable_slot, metatable_generation) = slot.metatable?;
        if !self.slot_matches(metatable_slot, metatable_generation, HandleKind::Table) {
            return None;
        }
        Some(PendingCandidate {
            slot: index as u32,
            generation: slot.generation,
            metatable_slot,
            metatable_generation,
        })
    }

    /// The metatable's cached raw `__gc` result for this mark round and exact
    /// generation, if a previous candidate already paid for the walk.
    /// Reads one slot record and no edge record.
    pub(super) fn cached_pending_gc(
        &self,
        candidate: &PendingCandidate,
        round: u64,
    ) -> Option<bool> {
        let metatable = self.slots.get(candidate.metatable_slot as usize)?;
        if metatable.generation == candidate.metatable_generation
            && metatable.pending_gc_generation == candidate.metatable_generation
            && metatable.pending_gc_round == round
        {
            Some(metatable.pending_gc_non_nil)
        } else {
            None
        }
    }

    /// Stores one completed raw `__gc` result on the exact metatable
    /// generation for this mark round. It does not grow a side table.
    pub(super) fn cache_pending_gc(
        &mut self,
        metatable_slot: u32,
        metatable_generation: u32,
        round: u64,
        non_nil: bool,
    ) {
        let Some(metatable) = self.slots.get_mut(metatable_slot as usize) else {
            return;
        };
        if metatable.generation != metatable_generation {
            return;
        }
        metatable.pending_gc_generation = metatable_generation;
        metatable.pending_gc_round = round;
        metatable.pending_gc_non_nil = non_nil;
    }

    /// The next cursor and raw `__gc` result of one edge record read.
    ///
    /// Missing records, a stored nil (`EdgeValue::Scalar(Value::Null)`), and a
    /// weak-cleared cursor gap are not values. `false`, strings, numbers, and
    /// every object carrier are values. This reads the raw record only; it
    /// does not consult `__index`.
    pub(super) fn pending_gc_record(&self, index: u32) -> (Option<u32>, bool, bool) {
        let record = &self.edge_slots[index as usize];
        let edge = record
            .edge
            .as_ref()
            .expect("owner chain holds an occupied record");
        let matched = edge.key.matches_name("__gc");
        let non_nil = matched && !matches!(edge.value, EdgeValue::Scalar(Value::Null));
        (record.next, matched, non_nil)
    }

    /// Commits one metered raw-`__gc`-positive candidate into the pending
    /// record list. Re-checks the exact slot identity before any write, so a
    /// candidate that changed during the walk is not recorded.
    pub(super) fn commit_pending_finalizer(&mut self, candidate: &PendingCandidate) -> bool {
        let index = candidate.slot as usize;
        let Some(slot) = self.slots.get(index) else {
            return false;
        };
        if slot.generation != candidate.generation
            || !matches!(slot.state, SlotState::Live { marked: false })
            || slot.kind != HandleKind::Userdata
            || slot.pending_finalizer
            || slot.finalizer_settled
        {
            return false;
        }
        let Some(raw) = slot.token else {
            return false;
        };
        let generation = slot.generation;
        self.slots[index].pending_finalizer = true;
        self.pending_finalizers.push(PendingFinalizer {
            slot: candidate.slot,
            generation,
            raw,
        });
        true
    }

    /// Settles one metered raw-`__gc`-negative candidate.
    ///
    /// Upstream `lj_gc_separateudata` marks such a white userdata finalized
    /// without queueing it. This writes the same per-identity state on the
    /// exact slot generation: no queue record, no mark, and no retention.
    /// The same call links the identity into this pass's intrusive settlement
    /// journal, one slot node and one head update, so the rollback record is
    /// constant work per pulse unit and never grows or copies a vector. The
    /// live-token, unmarked, userdata, and generation checks mirror
    /// [`Self::commit_pending_finalizer`]; a candidate that changed during
    /// the walk is not settled.
    pub(super) fn settle_pending_finalizer(&mut self, candidate: &PendingCandidate) -> bool {
        let index = candidate.slot as usize;
        let Some(slot) = self.slots.get(index) else {
            return false;
        };
        if slot.generation != candidate.generation
            || !matches!(slot.state, SlotState::Live { marked: false })
            || slot.kind != HandleKind::Userdata
            || slot.pending_finalizer
            || slot.finalizer_settled
            || slot.token.is_none()
        {
            return false;
        }
        let generation = candidate.generation;
        let head = self.settlement_head;
        let slot = &mut self.slots[index];
        slot.finalizer_settled = true;
        slot.settlement_generation = generation;
        slot.settlement_next = head;
        self.settlement_head = Some(candidate.slot);
        #[cfg(test)]
        {
            self.settlement_journal_len += 1;
            self.scan.settlement_journal_appends += 1;
        }
        true
    }

    /// Drops the current journal before a new selecting scan starts, or after
    /// a successful publication.
    ///
    /// This drops the head only. The links and generation stamps stay on the
    /// slots, but they are unreachable: a later append overwrites the next
    /// link before the slot can join the new chain, and slot reuse clears both
    /// fields. A successful pass therefore performs no per-node work here and
    /// no node visit is charged to Release or to the next mark's first pulse.
    pub(super) fn clear_selecting_settlements(&mut self) {
        self.settlement_head = None;
        #[cfg(test)]
        {
            self.settlement_journal_len = 0;
        }
    }

    /// Clears exactly the settlement flags named by the current failed pass
    /// and drops the journal.
    ///
    /// The walk follows this pass's intrusive chain, one slot node at a time,
    /// and touches no other node. Each entry is re-checked against the live
    /// slot generation and the userdata kind. A slot that changed, was already
    /// pending, or was settled by an earlier pass is not cleared, so valid
    /// previous-cycle state and a pending record's flag survive the rollback.
    /// The journal head and length are dropped before the walk, so a refused
    /// or interrupted pass cannot leave a partially reachable chain.
    pub(super) fn discard_settlements(&mut self) {
        let mut cursor = self.settlement_head.take();
        #[cfg(test)]
        {
            self.settlement_journal_len = 0;
        }
        while let Some(index) = cursor {
            let Some(slot) = self.slots.get_mut(index as usize) else {
                break;
            };
            let next = slot.settlement_next.take();
            let generation = std::mem::take(&mut slot.settlement_generation);
            #[cfg(test)]
            {
                self.scan.settlement_journal_visits += 1;
                self.settlement_journal_trace
                    .borrow_mut()
                    .push((index, generation));
            }
            if slot.generation == generation
                && slot.kind == HandleKind::Userdata
                && slot.finalizer_settled
                && !slot.pending_finalizer
            {
                slot.finalizer_settled = false;
            }
            cursor = next;
        }
    }

    /// Number of pending-finalizer records. Does not read the graph or a
    /// stored record; it reads only the queue length.
    pub(super) fn pending_finalizer_count(&self) -> usize {
        self.pending_finalizers.len()
    }

    /// Number of leading records that a successful private publication made
    /// consumable. A header read only; no stored record is read.
    pub(super) fn pending_finalizer_published_count(&self) -> usize {
        self.pending_finalizers.published_len()
    }

    /// The published head record, if the ready prefix is non-empty.
    ///
    /// This is one queue record read and no write. A queue that has records
    /// but no published prefix is not consumable: selection may have retained
    /// them for a later mark, but no successful weak publication has released
    /// them to a consumer yet.
    pub(super) fn pending_finalizer_published_front(&self) -> Result<PendingFinalizer, HeapError> {
        self.pending_finalizers.published_front().ok_or_else(|| {
            HeapError::Scope("lua pending finalizer head is not published".to_string())
        })
    }

    /// Checks one specified root slot can receive the published head.
    ///
    /// Any open frame generation is accepted; an unknown or old frame, an
    /// out-of-range slot, a missing root entry, and an occupied slot are all
    /// refused before a queue or root write. A valid empty slot is `Ok(())`.
    pub(super) fn pending_finalizer_root_slot(
        &self,
        frame: RootFrameId,
        slot: u32,
    ) -> Result<(), HeapError> {
        self.pending_finalizer_root_index(frame, slot).map(|_| ())
    }

    fn pending_finalizer_root_index(
        &self,
        frame: RootFrameId,
        slot: u32,
    ) -> Result<usize, HeapError> {
        // `frame` walks the open-frame stack, so this lookup is linear in the
        // number of open frames and reads no queue record.
        let found = self.frame(frame)?;
        if slot >= found.width {
            return Err(HeapError::StaleFrame);
        }
        let index = found.start as usize + slot as usize;
        if self.roots.get(index).is_none() {
            return Err(HeapError::StaleFrame);
        }
        if self.roots[index].is_some() {
            return Err(HeapError::Scope(
                "lua pending finalizer root slot is occupied".to_string(),
            ));
        }
        Ok(index)
    }

    /// Resolves the concrete root positions for one dequeue plan.
    ///
    /// `slots[0]` receives the userdata. A direct closure adds `slots[1]`; a
    /// table call or an independent userdata receiver adds the closure at
    /// `slots[1]` and the original receiver at `slots[2]`. A self-receiver
    /// userdata call needs only the first two positions. Only the first
    /// `needed` positions are read. They must be distinct, inside one open
    /// frame, present, and empty. A short plan or a duplicate is refused before
    /// any queue, root, or slot write. Each position locates its frame by
    /// walking the open-frame stack, so this plan is linear in the number of
    /// open frames and the number of required positions; it does not read the
    /// queue, any edge chain, or the root vector beyond the checked entries.
    fn pending_finalizer_root_indices(
        &self,
        frame: RootFrameId,
        slots: &[u32],
        needed: usize,
    ) -> Result<[usize; 3], HeapError> {
        if needed > 3 || slots.len() < needed {
            return Err(HeapError::Scope(
                "lua pending finalizer needs more root slots".to_string(),
            ));
        }
        let mut indices = [0usize; 3];
        for (offset, slot) in slots[..needed].iter().copied().enumerate() {
            if slots[..offset].contains(&slot) {
                return Err(HeapError::Scope(
                    "lua pending finalizer root slots overlap".to_string(),
                ));
            }
            indices[offset] = self.pending_finalizer_root_index(frame, slot)?;
        }
        Ok(indices)
    }

    /// The exact live userdata a published head record still names.
    ///
    /// The record's slot must still bind the same generation, kind, pending
    /// flag, raw handle, and live state. A successor generation in the same
    /// slot is refused. This is the shared identity preflight of both dequeue
    /// entries; it reads one record and writes nothing.
    fn pending_finalizer_head_identity(
        &self,
        head: PendingFinalizer,
    ) -> Result<UserdataRef, HeapError> {
        let Some(slot_record) = self.slots.get(head.slot as usize) else {
            return Err(HeapError::StaleHandle);
        };
        if slot_record.generation != head.generation
            || slot_record.kind != HandleKind::Userdata
            || !slot_record.pending_finalizer
            || slot_record.finalizer_settled
            || slot_record.token != Some(head.raw)
            || !matches!(slot_record.state, SlotState::Live { .. })
        {
            return Err(HeapError::StaleHandle);
        }
        Ok(UserdataRef::userdata(
            self.domain,
            head.slot,
            head.generation,
        ))
    }

    /// The current raw finalizer action for one live pending userdata.
    ///
    /// This is the dequeue-time re-read: the userdata's current metatable is
    /// read again, then its raw `__gc` field. A boolean, number, or string
    /// keeps only its Lua type tag; a stored nil stays `Skip`. A live table or
    /// a live userdata adds exactly one raw `__call` lookup on that receiver's
    /// own current instance metatable through [`Self::current_metatable_raw_call`].
    /// It does not reuse the selection cache, does not follow `__index`, does
    /// not walk an inherited metatable, and does not dispatch a table-valued
    /// `__call` a second time. Each raw field lookup walks one owner chain, so
    /// the cost is linear in the receiver metatable's stored edge count. Nothing
    /// is written.
    pub(super) fn pending_finalizer_action(&self, userdata: UserdataRef) -> PendingFinalizerAction {
        let Some(slot) = self.slots.get(userdata.slot() as usize) else {
            return PendingFinalizerAction::Skip;
        };
        let Some((metatable_slot, metatable_generation)) = slot.metatable else {
            return PendingFinalizerAction::Skip;
        };
        if !self.slot_matches(metatable_slot, metatable_generation, HandleKind::Table) {
            return PendingFinalizerAction::Skip;
        }
        let Some(value) = self.raw_named_value(metatable_slot, "__gc") else {
            return PendingFinalizerAction::Skip;
        };
        match value {
            EdgeValue::Scalar(value) => match pending_finalizer_scalar_reject(value) {
                Some(reject) => PendingFinalizerAction::NonCallable(reject),
                None => PendingFinalizerAction::Skip,
            },
            EdgeValue::Userdata { slot, generation } => {
                if !self.slot_matches(*slot, *generation, HandleKind::Userdata) {
                    return PendingFinalizerAction::NonCallable(
                        PendingFinalizerNotCallable::Detached,
                    );
                }
                let receiver = UserdataRef::userdata(self.domain, *slot, *generation);
                match self.current_metatable_raw_call(*slot) {
                    CurrentRawCall::WithoutCall => PendingFinalizerAction::NonCallable(
                        PendingFinalizerNotCallable::UserdataWithoutCall(receiver),
                    ),
                    CurrentRawCall::NotClosure => PendingFinalizerAction::NonCallable(
                        PendingFinalizerNotCallable::UserdataCallNotClosure(receiver),
                    ),
                    CurrentRawCall::Closure(call) => {
                        PendingFinalizerAction::UserdataCall { receiver, call }
                    }
                }
            }
            EdgeValue::Upvalue { .. } => {
                PendingFinalizerAction::NonCallable(PendingFinalizerNotCallable::Upvalue)
            }
            EdgeValue::Closure { slot, generation } => {
                if self.slot_matches(*slot, *generation, HandleKind::Closure) {
                    PendingFinalizerAction::Closure(ClosureRef::closure(
                        self.domain,
                        *slot,
                        *generation,
                    ))
                } else {
                    PendingFinalizerAction::NonCallable(PendingFinalizerNotCallable::Detached)
                }
            }
            EdgeValue::Table { slot, generation } => {
                if !self.slot_matches(*slot, *generation, HandleKind::Table) {
                    return PendingFinalizerAction::NonCallable(
                        PendingFinalizerNotCallable::Detached,
                    );
                }
                let receiver = TableRef::table(self.domain, *slot, *generation);
                match self.current_metatable_raw_call(*slot) {
                    CurrentRawCall::WithoutCall => PendingFinalizerAction::NonCallable(
                        PendingFinalizerNotCallable::TableWithoutCall(receiver),
                    ),
                    CurrentRawCall::NotClosure => PendingFinalizerAction::NonCallable(
                        PendingFinalizerNotCallable::TableCallNotClosure(receiver),
                    ),
                    CurrentRawCall::Closure(call) => {
                        PendingFinalizerAction::TableCall { receiver, call }
                    }
                }
            }
        }
    }

    /// One raw `__call` lookup on a receiver's own current instance metatable.
    ///
    /// This is exactly one layer: the receiver slot's current metatable is
    /// read, then its raw `__call` record. `__index` and the metatable's own
    /// metatable are never followed, and a table-valued `__call` is not
    /// dispatched again. The owner-chain lookup is linear in that metatable's
    /// stored edge count and reads no selection cache. Nothing is written.
    fn current_metatable_raw_call(&self, receiver_slot: u32) -> CurrentRawCall {
        let Some((metatable_slot, metatable_generation)) =
            self.slots[receiver_slot as usize].metatable
        else {
            return CurrentRawCall::WithoutCall;
        };
        if !self.slot_matches(metatable_slot, metatable_generation, HandleKind::Table) {
            return CurrentRawCall::WithoutCall;
        }
        let Some(call) = self.raw_named_value(metatable_slot, "__call") else {
            return CurrentRawCall::WithoutCall;
        };
        match call {
            EdgeValue::Scalar(Value::Null) => CurrentRawCall::WithoutCall,
            EdgeValue::Closure { slot, generation }
                if self.slot_matches(*slot, *generation, HandleKind::Closure) =>
            {
                CurrentRawCall::Closure(ClosureRef::closure(self.domain, *slot, *generation))
            }
            _ => CurrentRawCall::NotClosure,
        }
    }

    /// Pops exactly `head`, writes the already validated roots, and settles
    /// that generation once.
    ///
    /// This is the shared commit behind both dequeue entries. Every fallible
    /// check, including the current raw-field resolution and the whole root
    /// plan, has already finished in this same exclusive borrow. The remaining
    /// published-front pop reads one record and moves one stored record; it
    /// happens before the first root or slot write, and a refusal leaves the
    /// queue and every root unchanged. No guest code is called.
    fn commit_published_finalizer(
        &mut self,
        head: PendingFinalizer,
        userdata: UserdataRef,
        roots: &[(usize, Rooted)],
    ) -> Result<UserdataRef, HeapError> {
        let Some(popped) = self.pending_finalizers.pop_published_front(head) else {
            return Err(HeapError::Scope(
                "lua pending finalizer head is not published".to_string(),
            ));
        };
        debug_assert_eq!(popped, head);
        for (index, rooted) in roots {
            self.roots[*index] = Some(*rooted);
        }
        let slot_record = &mut self.slots[head.slot as usize];
        slot_record.pending_finalizer = false;
        slot_record.finalizer_settled = true;
        Ok(userdata)
    }

    /// Pops the exact published head, roots its typed userdata in the
    /// specified empty slot, and settles that generation in one commit.
    ///
    /// Every refusal is checked before the first queue, root, or slot write:
    /// a saved mark or tail, an unpublished head, a bad frame or slot, an
    /// occupied root, and a record whose live slot no longer binds the exact
    /// generation, kind, pending flag, and raw handle. The front record is
    /// then popped with one `VecDeque` front removal, the exact userdata is
    /// written into the root slot, and the slot's pending flag is replaced by
    /// its settled flag. It does not call a guest callback and does not read a
    /// raw `__gc` field: this is the retained single-root test path.
    pub(super) fn take_published_finalizer_into_root(
        &mut self,
        frame: RootFrameId,
        slot: u32,
        head: PendingFinalizer,
    ) -> Result<UserdataRef, HeapError> {
        self.deny_unbarriered()?;
        let indices = self.pending_finalizer_root_indices(frame, &[slot], 1)?;
        let userdata = self.pending_finalizer_head_identity(head)?;
        self.commit_published_finalizer(head, userdata, &[(indices[0], Rooted::Userdata(userdata))])
    }

    /// Pops one published head with a fresh raw-finalizer decision and roots
    /// every identity that decision needs.
    ///
    /// The queue record is not trusted for callability: after the shared
    /// identity preflight, the userdata's current metatable and its raw
    /// `__gc` are read again in this same exclusive borrow, before any write.
    /// `prepared` is the fixed-size action whose independent userdata receiver
    /// already passed the typed owner check in the caller; this method
    /// re-resolves the action and refuses any identity change
    /// ([`HeapError::StaleHandle`]) before the pop. A table or userdata value
    /// adds one raw `__call` read on that receiver's own current instance
    /// metatable. An inherited or table-valued `__call` is a typed reject, not
    /// a second dispatch, and the reject keeps the exact original receiver.
    /// `slots[0]` always receives the userdata; a direct closure adds
    /// `slots[1]`; a table call or an independent userdata receiver adds the
    /// closure at `slots[1]` and the original receiver at `slots[2]`. A
    /// userdata call whose receiver is the pending object itself adds only
    /// `slots[1]`, because `slots[0]` already roots that receiver. Only that
    /// required prefix is validated and written, and its positions must be
    /// distinct. A skip or a reject retires exactly once with only the
    /// userdata root. No guest code is called and no Core handle is copied.
    pub(super) fn take_published_finalizer_action_into_roots(
        &mut self,
        frame: RootFrameId,
        slots: &[u32],
        head: PendingFinalizer,
        prepared: PendingFinalizerAction,
    ) -> Result<TakenPendingFinalizer, HeapError> {
        self.deny_unbarriered()?;
        let userdata = self.pending_finalizer_head_identity(head)?;
        let action = self.pending_finalizer_action(userdata);
        if action != prepared {
            return Err(HeapError::StaleHandle);
        }
        let needed = match &action {
            PendingFinalizerAction::Skip | PendingFinalizerAction::NonCallable(_) => 1,
            PendingFinalizerAction::Closure(_) => 2,
            PendingFinalizerAction::TableCall { .. } => 3,
            PendingFinalizerAction::UserdataCall { receiver, .. } if *receiver == userdata => 2,
            PendingFinalizerAction::UserdataCall { .. } => 3,
        };
        let indices = self.pending_finalizer_root_indices(frame, slots, needed)?;
        match action {
            PendingFinalizerAction::Skip | PendingFinalizerAction::NonCallable(_) => {
                self.commit_published_finalizer(
                    head,
                    userdata,
                    &[(indices[0], Rooted::Userdata(userdata))],
                )?;
            }
            PendingFinalizerAction::Closure(closure) => {
                self.commit_published_finalizer(
                    head,
                    userdata,
                    &[
                        (indices[0], Rooted::Userdata(userdata)),
                        (indices[1], Rooted::Closure(closure)),
                    ],
                )?;
            }
            PendingFinalizerAction::TableCall { receiver, call } => {
                self.commit_published_finalizer(
                    head,
                    userdata,
                    &[
                        (indices[0], Rooted::Userdata(userdata)),
                        (indices[1], Rooted::Closure(call)),
                        (indices[2], Rooted::Table(receiver)),
                    ],
                )?;
            }
            PendingFinalizerAction::UserdataCall { receiver, call } => {
                if receiver == userdata {
                    self.commit_published_finalizer(
                        head,
                        userdata,
                        &[
                            (indices[0], Rooted::Userdata(userdata)),
                            (indices[1], Rooted::Closure(call)),
                        ],
                    )?;
                } else {
                    self.commit_published_finalizer(
                        head,
                        userdata,
                        &[
                            (indices[0], Rooted::Userdata(userdata)),
                            (indices[1], Rooted::Closure(call)),
                            (indices[2], Rooted::Userdata(receiver)),
                        ],
                    )?;
                }
            }
        }
        Ok(TakenPendingFinalizer {
            userdata,
            action,
            roots_written: needed as u32,
        })
    }

    /// Makes every currently stored pending record consumable after a
    /// successful private publication. One header write; no record is read,
    /// moved, or sorted.
    pub(super) fn publish_pending_finalizer_prefix(&mut self) {
        self.pending_finalizers.publish_all();
    }

    /// Removes only the records appended after `base` and clears their exact
    /// slot flags. This is the pre-verification rollback of one selecting
    /// batch: earlier valid records, their slots, tokens, and generations are
    /// not read for mutation. The mark bits left by the failed pass are not
    /// pending state; the next mark resets them.
    pub(crate) fn discard_pending_finalizers_from(&mut self, base: usize) {
        if base >= self.pending_finalizers.len() {
            return;
        }
        let discarded: Vec<PendingFinalizer> = self.pending_finalizers.split_off_from(base);
        for pending in discarded {
            let Some(slot) = self.slots.get_mut(pending.slot as usize) else {
                continue;
            };
            if slot.generation == pending.generation
                && slot.pending_finalizer
                && slot.token == Some(pending.raw)
            {
                slot.pending_finalizer = false;
            }
        }
    }

    /// Exact live userdata named by one pending record, if that record still
    /// matches the slot, generation, kind, pending flag, live state, and bound
    /// raw handle. One record read plus one same-slot identity read. This is
    /// the ordinary mark's host-retained edge offer; it does not close,
    /// select, or advance the record.
    pub(super) fn pending_finalizer_retention(&self, index: usize) -> Option<(u32, u32)> {
        let pending = *self.pending_finalizers.get(index)?;
        let slot = self.slots.get(pending.slot as usize)?;
        let exact = slot.generation == pending.generation
            && slot.kind == HandleKind::Userdata
            && slot.pending_finalizer
            && !slot.finalizer_settled
            && slot.token == Some(pending.raw)
            && matches!(slot.state, SlotState::Live { .. });
        exact.then_some((pending.slot, pending.generation))
    }

    /// Copies the exact pending records as `(raw, slot, generation)`.
    ///
    /// The host entry borrows each token with that raw and its concrete
    /// `lua.userdata` key before it closes any doomed token or publishes weak
    /// edges. Order is the live-userdata chain order: newest allocation first
    /// inside one cohort, with each later cohort stored after the earlier
    /// records that are still queued.
    pub(crate) fn pending_finalizer_records(&self) -> Vec<(u64, u32, u32)> {
        self.pending_finalizers
            .iter()
            .map(|pending| (pending.raw, pending.slot, pending.generation))
            .collect()
    }

    /// Test-only explicit retirement control for the eventual finalizer
    /// consumer.
    ///
    /// It removes one exact record, clears the slot flag, and leaves the Core
    /// token bound and the object live. It does not call a callback, close a
    /// token, run guest code, or mark anything. A later collection may then
    /// judge the object dead and close its token. Missing record, mismatch, a
    /// parked mark, or a saved tail is refused before any change.
    #[cfg(test)]
    pub(crate) fn retire_pending_finalizer(
        &mut self,
        userdata: UserdataRef,
    ) -> Result<(), HeapError> {
        self.require_userdata(userdata)?;
        self.deny_unbarriered()?;
        let index = userdata.slot();
        let raw = self.slots[index as usize]
            .token
            .ok_or(HeapError::StaleHandle)?;
        let Some(position) = self.pending_finalizers.iter().position(|pending| {
            pending.slot == index
                && pending.generation == userdata.generation()
                && pending.raw == raw
        }) else {
            return Err(HeapError::StaleHandle);
        };
        self.pending_finalizers.remove(position);
        self.slots[index as usize].pending_finalizer = false;
        self.slots[index as usize].finalizer_settled = true;
        Ok(())
    }

    /// Test-only read of the per-slot pending flag.
    #[cfg(test)]
    pub(crate) fn slot_pending_finalizer(&self, userdata: UserdataRef) -> Result<bool, HeapError> {
        self.require_userdata(userdata)?;
        Ok(self.slots[userdata.slot() as usize].pending_finalizer)
    }

    /// Test-only read of the per-slot settled flag.
    #[cfg(test)]
    pub(crate) fn slot_finalizer_settled(&self, userdata: UserdataRef) -> Result<bool, HeapError> {
        self.require_userdata(userdata)?;
        Ok(self.slots[userdata.slot() as usize].finalizer_settled)
    }

    /// Test-only anti-control arm for the secondary metatable propagation.
    ///
    /// With the flag armed, `open_owner` does not follow the metatable edge of
    /// a selected pending userdata. The userdata itself is still marked and
    /// retained; its graph then dies. This proves the graph-retention
    /// assertion depends on the real propagation. It changes no record and
    /// runs no callback.
    #[cfg(test)]
    pub(crate) fn arm_pending_finalizer_metatable_skip(&mut self, armed: bool) {
        self.skip_pending_finalizer_metatable = armed;
    }

    /// Test-only anti-control for the Release invariant: the next publish
    /// clears this exact pending slot's mark bit, so the bounded Select check
    /// must reject it before any weak publication or release. It does not
    /// change a record, token, or generation.
    #[cfg(test)]
    pub(crate) fn arm_pending_finalizer_reclaim_control(
        &mut self,
        userdata: UserdataRef,
    ) -> Result<(), HeapError> {
        self.require_userdata(userdata)?;
        if !self.slots[userdata.slot() as usize].pending_finalizer {
            return Err(HeapError::StaleHandle);
        }
        self.reclaim_pending_slot = Some(userdata.slot());
        Ok(())
    }

    /// The staged Core raw handle for one exact reservation, before publish.
    ///
    /// Test-only: the private retry entry reads it to close the same token
    /// after a refused rollback. An ordinary borrow never accepts it.
    #[cfg(test)]
    pub(super) fn pending_userdata_token(&self, userdata: UserdataRef) -> Result<u64, HeapError> {
        self.require_userdata(userdata)?;
        let Some(pending) = self.pending_userdata else {
            return Err(HeapError::StaleHandle);
        };
        if pending.slot != userdata.slot() || pending.generation != userdata.generation() {
            return Err(HeapError::StaleHandle);
        }
        pending.raw.ok_or(HeapError::StaleHandle)
    }

    /// Releases one empty userdata reservation made by
    /// [`Self::reserve_userdata`] and returns the slot generation after
    /// release.
    ///
    /// The exact pending record must still name this slot and generation, and
    /// the slot itself must carry no binding, edge, metatable, mode pointer,
    /// or carrier. Those checks are O(1): this call does not walk the roots or
    /// the owner chains, and it is not a rollback for an arbitrary published
    /// userdata. The next issuance takes the next generation (or the slot
    /// retires at the generation cap).
    pub(super) fn rollback_empty_userdata(
        &mut self,
        handle: UserdataRef,
    ) -> Result<u32, HeapError> {
        self.require_userdata(handle)?;
        self.ensure_sweep_open()?;
        let index = handle.slot() as usize;
        let Some(pending) = self.pending_userdata else {
            return Err(HeapError::StaleHandle);
        };
        if pending.slot != handle.slot() || pending.generation != handle.generation() {
            return Err(HeapError::StaleHandle);
        }
        let slot = &self.slots[index];
        if slot.token.is_some()
            || slot.edge_head.is_some()
            || slot.edge_tail.is_some()
            || slot.metatable.is_some()
            || slot.mode_edge.is_some()
            || slot.carrier.is_some()
        {
            return Err(HeapError::Scope(
                "userdata rollback is only valid for an empty reservation".to_string(),
            ));
        }
        self.pending_userdata = None;
        self.release_slot(handle.slot());
        Ok(self.slots[index].generation)
    }

    /// Clears the token stored on `table` without searching other slots.
    /// Does not close a host resource and does not sweep. Collection uses
    /// [`Self::clear_exact_token`] instead.
    pub fn unbind_token(&mut self, table: TableRef) -> Result<(), HeapError> {
        self.require_live(table)?;
        self.ensure_sweep_open()?;
        self.slots[table.slot() as usize].token = None;
        Ok(())
    }

    /// Records one close attempt. The attempt is counted even when close
    /// returns an error, so a later retry can be metered on its own.
    pub(super) fn note_token_close(&mut self) {
        self.scan.token_closes += 1;
    }

    /// Clears `token` only when that exact slot, generation, kind, and raw
    /// handle still store it. A reused slot, including a successor, is left
    /// alone.
    pub(super) fn clear_exact_token(&mut self, token: DoomedToken) -> bool {
        self.scan.token_slot_checks += 1;
        let matches = self.slots.get(token.slot as usize).is_some_and(|slot| {
            slot.generation == token.generation
                && slot.kind == token.kind
                && slot.token == Some(token.raw)
        });
        if !matches {
            return false;
        }
        self.scan.token_slot_checks += 1;
        self.slots[token.slot as usize].token = None;
        true
    }

    /// True when a mark is already stored. Does not read slots, edges, or a batch.
    pub(super) fn has_saved_mark(&self) -> bool {
        self.mark.is_some()
    }

    /// True when a published sweep tail is stored. Does not read slots or edges.
    pub(super) fn has_sweep_tail(&self) -> bool {
        self.sweep_tail.is_some()
    }

    /// Phase, cursor, and accounted roots. Does not scan or resume the tail.
    #[cfg(test)]
    pub fn saved_tail_state(&self) -> Option<SavedTailState> {
        let tail = self.sweep_tail.as_ref()?;
        let phase = match tail.phase() {
            sweep::TailPhase::Release => SavedTailPhase::Release,
            sweep::TailPhase::Roots => SavedTailPhase::Roots,
            sweep::TailPhase::Done => SavedTailPhase::Done,
        };
        Some(SavedTailState {
            phase,
            cursor: tail.cursor(),
            live_roots: tail.accounted_roots(),
            dead_len: tail.dead_len(),
        })
    }

    /// Copies the scan meter. Does not clear it and does not visit a slot.
    #[cfg(test)]
    pub fn scan_meter(&self) -> ScanMeter {
        self.scan
    }
    /// The table in one frame slot, if that slot holds a table. Does not scan.
    #[cfg(test)]
    pub fn rooted_table(
        &self,
        frame: RootFrameId,
        slot: u32,
    ) -> Result<Option<TableRef>, HeapError> {
        let (start, width) = {
            let found = self.frame(frame)?;
            (found.start, found.width)
        };
        if slot >= width {
            return Err(HeapError::StaleFrame);
        }
        let index = start as usize + slot as usize;
        match self.roots.get(index).copied().flatten() {
            None => Ok(None),
            Some(Rooted::Table(table)) => Ok(Some(table)),
            Some(_) => Err(HeapError::StaleHandle),
        }
    }

    /// The userdata in one frame slot, if that slot holds a userdata. This is
    /// a test observation for the private handoff and does not scan.
    #[cfg(test)]
    pub(crate) fn rooted_userdata(
        &self,
        frame: RootFrameId,
        slot: u32,
    ) -> Result<Option<UserdataRef>, HeapError> {
        let (start, width) = {
            let found = self.frame(frame)?;
            (found.start, found.width)
        };
        if slot >= width {
            return Err(HeapError::StaleFrame);
        }
        let index = start as usize + slot as usize;
        match self.roots.get(index).copied().flatten() {
            None => Ok(None),
            Some(Rooted::Userdata(userdata)) => Ok(Some(userdata)),
            Some(_) => Err(HeapError::StaleHandle),
        }
    }

    /// The closure in one frame slot, if that slot holds a closure. This is a
    /// test observation for the private action handoff and does not scan.
    #[cfg(test)]
    pub(crate) fn rooted_closure(
        &self,
        frame: RootFrameId,
        slot: u32,
    ) -> Result<Option<ClosureRef>, HeapError> {
        let (start, width) = {
            let found = self.frame(frame)?;
            (found.start, found.width)
        };
        if slot >= width {
            return Err(HeapError::StaleFrame);
        }
        let index = start as usize + slot as usize;
        match self.roots.get(index).copied().flatten() {
            None => Ok(None),
            Some(Rooted::Closure(closure)) => Ok(Some(closure)),
            Some(_) => Err(HeapError::StaleHandle),
        }
    }

    /// Domain that issued handles for this heap. Does not read slots.
    pub(super) fn domain(&self) -> u64 {
        self.domain
    }

    /// Next nonzero epoch for slot weak-start and rescan-queue tags. Wrapping
    /// skips zero, so a new mark never accepts a stale tag.
    fn advance_rescan_epoch(&mut self) -> u64 {
        loop {
            self.rescan_epoch = self.rescan_epoch.wrapping_add(1);
            if self.rescan_epoch != 0 {
                return self.rescan_epoch;
            }
        }
    }

    /// A saved tail owns the graph. Checked after handle and capacity validation,
    /// and before the first write or chain walk of a commit.
    fn ensure_sweep_open(&self) -> Result<(), HeapError> {
        if self.sweep_tail.is_some() {
            Err(HeapError::SweepPending)
        } else {
            Ok(())
        }
    }

    /// Refuses a userdata allocation or edge write that has no full barrier.
    ///
    /// A saved tail is reported first. A parked mark, including paused gray, is
    /// `MarkDenied`. `alloc_slot` only checks the tail, so callers that cannot
    /// barrier the new node must use this before taking or growing a slot.
    fn deny_unbarriered(&self) -> Result<(), HeapError> {
        self.ensure_sweep_open()?;
        if self.mark.is_some() {
            return Err(HeapError::MarkDenied);
        }
        Ok(())
    }

    fn ensure_published_tail(&self) -> Result<(), HeapError> {
        if self.sweep_tail.is_some() {
            Ok(())
        } else {
            Err(HeapError::SweepDenied)
        }
    }

    /// Spends `budget` on the saved tail. Zero restores it without a scan.
    pub(super) fn resume_sweep_tail(&mut self, budget: u64) -> TailResume {
        let Some(tail) = self.sweep_tail.take() else {
            return TailResume::Denied;
        };
        match tail.drive(self, budget) {
            sweep::TailStep::Paused(tail) => {
                self.sweep_tail = Some(tail);
                TailResume::Paused
            }
            sweep::TailStep::Completed(report) => TailResume::Completed(report),
        }
    }

    /// Finishes a saved tail before a fresh pure-heap cycle. No tail is a no-op.
    fn drain_sweep_tail(&mut self) {
        let Some(tail) = self.sweep_tail.take() else {
            return;
        };
        match tail.drive(self, u64::MAX) {
            sweep::TailStep::Completed(_) => {}
            sweep::TailStep::Paused(tail) => {
                self.sweep_tail = Some(tail);
                debug_assert!(false, "a full budget must finish a saved sweep tail");
            }
        }
    }

    /// Saves one mark for a private step cycle. Does not scan.
    pub(super) fn open_private_step(&mut self) -> Result<(), HeapError> {
        mark::ParkedMark::open_saved(self)
    }

    /// Marks once, then drains the pending-close enumerator. Each slot check
    /// is one scan unit. The drain finishes before this returns, so host close
    /// still sees every doomed token and the original mark snapshot. A partial
    /// pulse is not returned from here and is not given to sweep.
    pub(super) fn mark_and_doomed_tokens(&mut self) -> DoomedMark {
        let modes = self.mark_reachable();
        self.drain_pending_close(modes)
    }

    /// The private single-shot entry's mark: the same ordinary mark, then one
    /// selection scan for finalizer-eligible userdata after the gray, rescan,
    /// and late work has emptied, then the usual pending-close enumeration.
    ///
    /// Selection marks each raw-`__gc`-positive userdata through the ordinary
    /// target follow, so `open_owner` follows its metatable edge and the
    /// metatable's own strong entries are scanned by the ordinary gray loop.
    /// A raw-`__gc`-negative candidate is settled instead: it is not marked,
    /// not queued, and not retained, but a live marked generation is
    /// value-dead for weak publication. The phase does not run in
    /// [`Self::mark_and_doomed_tokens`], so an ordinary host collection
    /// neither selects new objects nor charges the selection units. A second
    /// selecting pass skips a slot that is already pending or settled.
    pub(super) fn mark_and_pending_tokens(&mut self) -> DoomedMark {
        let modes = self.mark_reachable_selecting_pending();
        self.drain_pending_close(modes)
    }

    /// Advances a saved mark with the borrowed batch. `None` means the same
    /// state was written back and tokens were not enumerated. `Some` is the
    /// owned close list of a finished mark; the saved state is already gone.
    pub(super) fn take_finished_saved_mark(
        &mut self,
        budget: u64,
        batch: &[Rooted],
    ) -> Result<Option<DoomedMark>, HeapError> {
        let (_view, snapshot) = self.advance_saved_mark_with_root_batch(budget, batch)?;
        Ok(snapshot.map(|modes| self.drain_pending_close(modes)))
    }

    fn drain_pending_close(&mut self, modes: MarkSnapshot) -> DoomedMark {
        let mut session = PendingCloseSession::start(self, modes);
        session.drain();
        session.finish()
    }

    /// Non-moving mark from explicit roots, then one sweep of unmarked nodes.
    /// Freed slots are reused with a bumped generation. A slot whose next
    /// generation would pass `max_generation` is retired instead.
    ///
    /// A saved published tail is drained first, then this starts a fresh cycle.
    /// This is the pure-heap collector. A host heap must use `heap::collect`,
    /// which closes tokens before [`Self::sweep_marked`]. A dead slot that
    /// still stores a bound host token is refused by the shared sweep
    /// selection, before weak publication or release, so this path cannot
    /// skip a typed resource.
    ///
    /// # Panics
    ///
    /// Panics when the shared selection step reaches an unmarked live slot
    /// that still carries a bound host token. This method has no
    /// `HostContext`, so it cannot close that resource. The host collector
    /// closes every doomed token first and clears the binding.
    pub fn collect(&mut self) -> CollectReport {
        self.drain_sweep_tail();
        let modes = self.mark_reachable();
        self.sweep_marked(modes)
    }

    /// Releases nodes the current mark did not reach. Does not mark again.
    ///
    /// [`SweepSession`] drains the same sweep used by [`Self::collect`] and
    /// by [`MarkSession::sweep`]. `heap::collect` reaches it through this
    /// function after tokens close. The report is built once, after the drain.
    /// This function does not return a partial sweep. The shared selection
    /// step refuses (panics on) a dead slot that still stores a host token:
    /// a host caller must close and clear every doomed binding first.
    pub(super) fn sweep_marked(&mut self, modes: MarkSnapshot) -> CollectReport {
        let mut session = SweepSession::start(self, modes);
        session.drain();
        session.report()
    }

    /// Refuses a parked mark without clearing it. Otherwise the same fresh
    /// mark and pending-close enumeration as [`Self::mark_and_doomed_tokens`].
    pub(super) fn begin_private_cycle(&mut self) -> Result<DoomedMark, HeapError> {
        if self.mark.is_some() {
            return Err(HeapError::MarkActive);
        }
        if self.sweep_tail.is_some() {
            return Err(HeapError::SweepPending);
        }
        Ok(self.mark_and_doomed_tokens())
    }

    /// Same sweep session from selection through release. The callback runs
    /// once, after Owners and before Release, and only when that sweep
    /// publishes: a dead slot exists, or a retained pending finalizer needs
    /// its weak values cleared.
    pub(super) fn sweep_observing<R>(
        &mut self,
        modes: MarkSnapshot,
        observe: impl for<'a> FnOnce(&'a PublishedRead<'a>) -> R,
    ) -> (CollectReport, Option<R>) {
        let mut session = SweepSession::start(self, modes);
        let seen = session.observe_before_release(observe);
        session.drain();
        (session.report(), seen)
    }

    /// Publishes weak edges and stores the release tail. No dead slot and no
    /// retained pending finalizer finishes the sweep and records the one
    /// report.
    pub(super) fn halt_at_publish(&mut self, modes: MarkSnapshot) -> PublishHalt {
        debug_assert!(self.sweep_tail.is_none(), "publish must not stack a tail");
        #[cfg(test)]
        if let Some(slot) = self.reclaim_pending_slot.take()
            && let Some(stored) = self.slots.get_mut(slot as usize)
            && stored.pending_finalizer
            && matches!(stored.state, SlotState::Live { marked: true })
        {
            stored.state = SlotState::Live { marked: false };
        }
        let mut session = SweepSession::start(self, modes);
        if !session.reach_before_owners() {
            session.drain();
            return PublishHalt::Completed(session.report());
        }
        let (heads, edges) = session
            .publish_counts()
            .expect("the owner boundary publishes once");
        let tail = session.into_release_tail();
        self.sweep_tail = Some(tail);
        PublishHalt::Published { heads, edges }
    }

    /// Reports whether `handle` names a live table in this heap.
    pub fn alive(&self, handle: TableRef) -> bool {
        self.require_live(handle).is_ok()
    }

    /// Reports whether `handle` names a live closure in this heap.
    pub fn closure_alive(&self, handle: ClosureRef) -> bool {
        self.require_closure(handle).is_ok()
    }

    /// Reports whether `handle` names a live upvalue in this heap.
    pub fn upvalue_alive(&self, handle: UpvalueRef) -> bool {
        self.require_upvalue(handle).is_ok()
    }

    /// Reports whether `handle` names a live bare userdata in this heap.
    pub fn userdata_alive(&self, handle: UserdataRef) -> bool {
        self.require_userdata(handle).is_ok()
    }

    /// Generation stored in one slot, if that slot exists. Test-only: the
    /// generation check for a live handle is its own typed accessor.
    #[cfg(test)]
    pub(crate) fn slot_generation(&self, slot: u32) -> Option<u32> {
        self.slots
            .get(slot as usize)
            .map(|stored| stored.generation)
    }

    /// Whether one slot is physically live. Test-only.
    #[cfg(test)]
    pub(crate) fn slot_is_live(&self, slot: u32) -> bool {
        self.slots
            .get(slot as usize)
            .is_some_and(|stored| matches!(stored.state, SlotState::Live { .. }))
    }

    /// Owner chains the legacy rollback edge walk opened since the heap was
    /// created. Test-only observation for the empty-reservation rollback.
    #[cfg(test)]
    pub(crate) fn rollback_owner_visits(&self) -> u64 {
        self.rollback_owner_visits
    }

    /// Clears the recorded selection slot indices. Test-only.
    #[cfg(test)]
    pub(super) fn clear_selection_slot_trace(&mut self) {
        self.selection_slot_trace.borrow_mut().clear();
    }

    /// Slot indices actually indexed since the last clear. Test-only.
    #[cfg(test)]
    pub(super) fn selection_slot_trace(&self) -> Vec<u32> {
        self.selection_slot_trace.borrow().clone()
    }

    /// Edge records read by the raw `__gc` selection lookup since the last
    /// clear, as `(metatable_slot, edge_record_index)`. Test-only.
    #[cfg(test)]
    pub(crate) fn pending_gc_edge_trace(&self) -> Vec<(u32, u32)> {
        self.pending_gc_edge_trace.borrow().clone()
    }

    /// Clears the raw `__gc` edge-read trace. Test-only.
    #[cfg(test)]
    pub(crate) fn clear_pending_gc_edge_trace(&mut self) {
        self.pending_gc_edge_trace.borrow_mut().clear();
    }

    /// Live-userdata chain nodes read by the private selection since the last
    /// clear, as `(slot, generation)` in visit order. Test-only: each entry is
    /// one real slot-record read, not a selected-record count.
    #[cfg(test)]
    pub(crate) fn pending_live_trace(&self) -> Vec<(u32, u32)> {
        self.pending_live_trace.borrow().clone()
    }

    /// Clears the live-userdata chain-read trace. Test-only.
    #[cfg(test)]
    pub(crate) fn clear_pending_live_trace(&mut self) {
        self.pending_live_trace.borrow_mut().clear();
    }

    /// Walks the live-userdata chain head to tail and asserts each link is
    /// internally consistent at its exact generation. Test-only observation
    /// for release, rollback, neighbour, and same-slot successor checks.
    #[cfg(test)]
    pub(crate) fn live_userdata_chain(&self) -> Vec<(u32, u32)> {
        let mut chain = Vec::new();
        let mut previous = None;
        let mut cursor = self.live_userdata_head;
        while let Some((index, generation)) = cursor {
            let slot = self
                .slots
                .get(index as usize)
                .expect("a live-userdata chain node names a slot");
            assert_eq!(slot.generation, generation, "chain generation");
            assert_eq!(slot.kind, HandleKind::Userdata, "chain node kind");
            assert!(slot.live_userdata_linked, "chain node is linked");
            assert_eq!(slot.live_userdata_prev, previous, "chain back link");
            chain.push((index, generation));
            previous = Some((index, generation));
            cursor = slot.live_userdata_next;
            assert!(chain.len() <= self.slots.len(), "chain must not cycle");
        }
        assert_eq!(previous, self.live_userdata_tail, "chain tail");
        chain
    }

    /// Identities currently linked in the settlement journal, from the head.
    /// Test-only: the walk observes membership and does not count as a
    /// rollback visit or as a successful-pass drain.
    #[cfg(test)]
    pub(crate) fn settlement_journal_members(&self) -> Vec<(u32, u32)> {
        let mut members = Vec::new();
        let mut cursor = self.settlement_head;
        while let Some(index) = cursor {
            let Some(slot) = self.slots.get(index as usize) else {
                break;
            };
            members.push((index, slot.settlement_generation));
            cursor = slot.settlement_next;
        }
        members
    }

    /// Number of identities currently linked in the settlement journal.
    /// Test-only.
    #[cfg(test)]
    pub(crate) fn settlement_journal_len(&self) -> u32 {
        self.settlement_journal_len
    }

    /// Takes the identities actually visited by a failed-pass journal
    /// rollback, in visit order. Test-only; a successful pass and Release
    /// leave the trace empty.
    #[cfg(test)]
    pub(crate) fn take_settlement_journal_trace(&mut self) -> Vec<(u32, u32)> {
        std::mem::take(&mut *self.settlement_journal_trace.borrow_mut())
    }

    /// Current occupancy. Peaks count storage, not only live objects.
    /// Does not collect and does not add scan counts.
    pub fn stats(&self) -> HeapStats {
        HeapStats {
            live_nodes: self.live_nodes(),
            live_roots: self.live_roots(),
            live_edges: self.live_edge_count,
            node_slots: self.slots.len() as u32,
            root_slots: self.roots.len() as u32,
            edge_slots: self.occupied_edges,
            collections: self.collections,
            last_marked: self.last_marked,
            peak_nodes: self.peak_nodes,
            peak_roots: self.peak_roots,
            peak_edges: self.peak_edges,
        }
    }

    /// Logical bytes of this private heap, as KiB. Same meter as
    /// [`Self::logical_bytes`]. Does not collect and does not touch the scan meter.
    pub fn logical_kib(&self) -> f64 {
        self.logical_bytes() as f64 / 1024.0
    }

    /// Logical allocated bytes of this private heap.
    ///
    /// Charged:
    /// - each slot record (`slots.len()`, including a free or retired record
    ///   that still occupies the vector)
    /// - each edge record (`occupied_edges`, including a weak-cleared cursor
    ///   gap and excluding a free slot and backing capacity)
    /// - the stored length of a string or byte payload in those edge records
    ///
    /// Not charged:
    /// - `Vec` capacity above `len`, and string or byte capacity above `len`
    /// - root frames and the free-list (a frame leave truncates roots before
    ///   collection, so charging them would drop the reading first)
    /// - the issued carrier array (Core identity, not a string payload)
    /// - Core locals, temporaries, and strings that are not stored in an edge
    /// - the borrowed `next` cursor (not a heap record)
    ///
    /// A weak clear keeps the cursor key charged and releases only the value
    /// payload. The read is the two lengths plus one counter: no scan, no
    /// mark, and no change to `collections` or the scan meter. This is not a
    /// LuaJIT allocator, not whole-VM bytes, and not string interning.
    ///
    /// The record product fits in `u64`: each vector length is capped by a
    /// `u32` limit, and a record stride is far below `2^32`. Each edge counts
    /// its own reference payload. The same Core `Arc` referenced by more than
    /// one edge is charged once per edge.
    pub fn logical_bytes(&self) -> u64 {
        let nodes = (self.slots.len() as u64) * (std::mem::size_of::<Slot>() as u64);
        let edges = (u64::from(self.occupied_edges)) * (std::mem::size_of::<Edge>() as u64);
        nodes + edges + self.payload_bytes
    }

    fn require_live(&self, handle: TableRef) -> Result<(), HeapError> {
        self.require_kind(
            handle.domain(),
            handle.slot(),
            handle.generation(),
            HandleKind::Table,
        )
    }

    fn require_closure(&self, handle: ClosureRef) -> Result<(), HeapError> {
        self.require_kind(
            handle.domain(),
            handle.slot(),
            handle.generation(),
            HandleKind::Closure,
        )
    }

    fn require_upvalue(&self, handle: UpvalueRef) -> Result<(), HeapError> {
        self.require_kind(
            handle.domain(),
            handle.slot(),
            handle.generation(),
            HandleKind::Upvalue,
        )
    }

    fn require_userdata(&self, handle: UserdataRef) -> Result<(), HeapError> {
        self.require_kind(
            handle.domain(),
            handle.slot(),
            handle.generation(),
            HandleKind::Userdata,
        )
    }

    fn require_kind(
        &self,
        domain: u64,
        slot: u32,
        generation: u32,
        kind: HandleKind,
    ) -> Result<(), HeapError> {
        if domain != self.domain {
            return Err(HeapError::WrongDomain);
        }
        if self.slot_matches(slot, generation, kind) {
            return Ok(());
        }
        let live_other_kind = self.slots.get(slot as usize).is_some_and(|stored| {
            stored.generation == generation
                && stored.kind != kind
                && matches!(stored.state, SlotState::Live { .. })
        });
        if live_other_kind {
            Err(HeapError::WrongKind)
        } else {
            Err(HeapError::StaleHandle)
        }
    }

    fn slot_matches(&self, index: u32, generation: u32, kind: HandleKind) -> bool {
        self.slots.get(index as usize).is_some_and(|slot| {
            slot.generation == generation
                && slot.kind == kind
                && matches!(slot.state, SlotState::Live { .. })
        })
    }

    /// Drops any parked mark, then marks from the current roots.
    /// A later close failure does not put that mark back.
    fn mark_reachable(&mut self) -> MarkSnapshot {
        let abandoned = self.mark.take();
        drop(abandoned);
        self.drain_mark()
    }

    /// Drops any parked mark, then runs one mark whose selection phase scans
    /// for finalizer-eligible userdata after the ordinary work empties.
    /// Selection records and marks each chosen userdata; its metatable graph
    /// is then scanned by the same gray loop. A close failure after this mark
    /// does not put the mark back, but the records and marked bits remain
    /// until the next reset, so a retry marks the same records again.
    fn mark_reachable_selecting_pending(&mut self) -> MarkSnapshot {
        let abandoned = self.mark.take();
        drop(abandoned);
        let mut session = MarkSession::start_selecting_pending(self);
        session.drain();
        session.into_snapshot()
    }

    /// Links one freshly allocated userdata at the live-chain head.
    ///
    /// The caller has already committed the live slot at `generation`. The
    /// append is four link writes and the two head/tail fields; it grows
    /// nothing and moves no other node. A slot that is already linked is a
    /// caller bug.
    fn link_live_userdata_head(&mut self, index: u32, generation: u32) {
        debug_assert_eq!(self.slots[index as usize].generation, generation);
        debug_assert_eq!(self.slots[index as usize].kind, HandleKind::Userdata);
        debug_assert!(matches!(
            self.slots[index as usize].state,
            SlotState::Live { .. }
        ));
        debug_assert!(!self.slots[index as usize].live_userdata_linked);
        let head = self.live_userdata_head;
        {
            let slot = &mut self.slots[index as usize];
            slot.live_userdata_prev = None;
            slot.live_userdata_next = head;
            slot.live_userdata_linked = true;
        }
        if let Some((head_index, head_generation)) = head {
            let head_slot = &mut self.slots[head_index as usize];
            debug_assert_eq!(head_slot.generation, head_generation);
            debug_assert!(head_slot.live_userdata_linked);
            debug_assert!(head_slot.live_userdata_prev.is_none());
            head_slot.live_userdata_prev = Some((index, generation));
        }
        self.live_userdata_head = Some((index, generation));
        if self.live_userdata_tail.is_none() {
            self.live_userdata_tail = Some((index, generation));
        }
    }

    /// Unlinks one live userdata from the chain in constant work.
    ///
    /// The links carry the exact generation of each neighbour, so a successor
    /// generation cannot be reached through an old link. A slot that is not
    /// linked is left alone: release of a table, closure, or cell does not
    /// touch this chain. The head and tail updates are the same bounded
    /// neighbour writes; no node is scanned, moved, or sorted.
    fn unlink_live_userdata(&mut self, index: u32) {
        if !self.slots[index as usize].live_userdata_linked {
            return;
        }
        debug_assert_eq!(self.slots[index as usize].kind, HandleKind::Userdata);
        let identity = (index, self.slots[index as usize].generation);
        let prev = self.slots[index as usize].live_userdata_prev.take();
        let next = self.slots[index as usize].live_userdata_next.take();
        self.slots[index as usize].live_userdata_linked = false;
        match prev {
            Some((prev_index, prev_generation)) => {
                let prev_slot = &mut self.slots[prev_index as usize];
                debug_assert_eq!(prev_slot.generation, prev_generation);
                debug_assert!(prev_slot.live_userdata_linked);
                debug_assert_eq!(prev_slot.live_userdata_next, Some(identity));
                prev_slot.live_userdata_next = next;
            }
            None => {
                debug_assert_eq!(self.live_userdata_head, Some(identity));
                self.live_userdata_head = next;
            }
        }
        match next {
            Some((next_index, next_generation)) => {
                let next_slot = &mut self.slots[next_index as usize];
                debug_assert_eq!(next_slot.generation, next_generation);
                debug_assert!(next_slot.live_userdata_linked);
                debug_assert_eq!(next_slot.live_userdata_prev, Some(identity));
                next_slot.live_userdata_prev = prev;
            }
            None => {
                debug_assert_eq!(self.live_userdata_tail, Some(identity));
                self.live_userdata_tail = prev;
            }
        }
    }

    fn release_slot(&mut self, index: u32) {
        // A live userdata leaves the selection chain before the generation
        // advances, the state becomes Free/Retired, or any field is cleared.
        // The unlink is the released slot's own two neighbours and two links,
        // never a walk of the chain and never a pending-queue read.
        if self.slots[index as usize].kind == HandleKind::Userdata {
            self.unlink_live_userdata(index);
        } else {
            debug_assert!(
                !self.slots[index as usize].live_userdata_linked,
                "a non-userdata slot cannot join the live-userdata chain"
            );
        }
        debug_assert!(
            self.slots[index as usize].edge_head.is_none()
                && self.slots[index as usize].edge_tail.is_none(),
            "owner chain must already be unlinked; release does not scan edges"
        );
        if self.pending_userdata.is_some_and(|pending| {
            pending.slot == index && pending.generation == self.slots[index as usize].generation
        }) {
            debug_assert!(
                self.pending_userdata
                    .is_some_and(|pending| pending.raw.is_none()),
                "a registered userdata reservation reached release without its mark retention"
            );
            self.pending_userdata = None;
        }
        // A pending record is a host-retained edge; a selected slot must not
        // reach release. The per-slot flag is the bounded invariant: release
        // reads only this slot, never the pending queue.
        debug_assert!(
            !self.slots[index as usize].pending_finalizer,
            "a pending finalizer reached release without its retirement"
        );
        self.slots[index as usize].pending_finalizer = false;
        self.slots[index as usize].finalizer_settled = false;
        self.slots[index as usize].pending_gc_generation = 0;
        self.slots[index as usize].pending_gc_round = 0;
        self.slots[index as usize].pending_gc_non_nil = false;
        let mark = self.mark.as_mut();
        mark::release_rescan_slot(mark, &mut self.slots, index);
        let mark = self.mark.as_mut();
        mark::release_late_slot(mark, &mut self.slots, index);
        if self.slots[index as usize].metatable.take().is_some() {
            self.metatable_edges = self.metatable_edges.saturating_sub(1);
        }
        let slot = &mut self.slots[index as usize];
        slot.token = None;
        slot.carrier = None;
        slot.edge_head = None;
        slot.edge_tail = None;
        slot.mode_edge = None;
        slot.mode_epoch = 0;
        slot.mode_generation = 0;
        slot.mode_round = 0;
        slot.mode_weak = WeakMode::default();
        slot.mode_checked_generation = 0;
        slot.mode_checked_round = 0;
        slot.mode_checked_epoch = 0;
        slot.discover_generation = 0;
        slot.discover_batch = 0;
        slot.discover_round = 0;
        if slot.generation >= self.limits.max_generation {
            slot.state = SlotState::Retired;
            return;
        }
        slot.generation = slot.generation.saturating_add(1);
        slot.state = SlotState::Free;
        self.free_list.push(index);
    }

    fn remove_edges_touching(&mut self, slot: u32) {
        for owner in 0..self.slots.len() {
            #[cfg(test)]
            {
                self.rollback_owner_visits += 1;
            }
            let mut prev = None;
            let mut cursor = self.slots[owner].edge_head;
            while let Some(index) = cursor {
                let next = self.edge_slots[index as usize].next;
                let incident = {
                    let edge = self.edge_slots[index as usize]
                        .edge
                        .as_ref()
                        .expect("owner chain holds an occupied record");
                    owner as u32 == slot
                        || edge.value.object_slot() == Some(slot)
                        || edge_key_is_slot(&edge.key, slot)
                };
                if incident {
                    self.unlink_at(owner as u32, prev, index);
                } else {
                    prev = Some(index);
                }
                cursor = next;
            }
        }
        debug_assert!(self.slots[slot as usize].edge_head.is_none());
        debug_assert!(self.slots[slot as usize].edge_tail.is_none());
    }

    fn clear_rooted_to(&mut self, rooted: Rooted) {
        for root in &mut self.roots {
            if *root == Some(rooted) {
                *root = None;
            }
        }
    }

    fn rooted_alive(&self, rooted: Rooted) -> bool {
        match rooted {
            Rooted::Table(handle) => self.alive(handle),
            Rooted::Closure(handle) => self.closure_alive(handle),
            Rooted::Upvalue(handle) => self.upvalue_alive(handle),
            Rooted::Userdata(handle) => self.userdata_alive(handle),
        }
    }

    fn frame(&self, id: RootFrameId) -> Result<&Frame, HeapError> {
        self.frames
            .iter()
            .find(|frame| frame.index == id.index && frame.generation == id.generation)
            .ok_or(HeapError::StaleFrame)
    }

    fn live_nodes(&self) -> u32 {
        self.slots
            .iter()
            .filter(|slot| matches!(slot.state, SlotState::Live { .. }))
            .count() as u32
    }

    fn live_roots(&self) -> u32 {
        self.roots.iter().filter(|root| root.is_some()).count() as u32
    }

    fn note_node_peak(&mut self) {
        self.peak_nodes = self.peak_nodes.max(self.slots.len() as u32);
    }

    fn note_root_peak(&mut self) {
        self.peak_roots = self.peak_roots.max(self.roots.len() as u32);
    }

    fn note_edge_peak(&mut self) {
        self.peak_edges = self.peak_edges.max(self.occupied_edges);
    }

    fn find_key(&self, owner: u32, key: &EdgeKey) -> Option<u32> {
        self.find_matching(owner, |stored| stored == key)
    }

    fn find_matching(&self, owner: u32, matches_key: impl Fn(&EdgeKey) -> bool) -> Option<u32> {
        let mut cursor = self.slots.get(owner as usize)?.edge_head;
        while let Some(index) = cursor {
            let record = &self.edge_slots[index as usize];
            if record
                .edge
                .as_ref()
                .is_some_and(|edge| matches_key(&edge.key))
            {
                return Some(index);
            }
            cursor = record.next;
        }
        None
    }

    fn take_edge_slot(&mut self) -> u32 {
        if let Some(index) = self.edge_free_head {
            let next = self.edge_slots[index as usize].next;
            self.edge_free_head = next;
            self.edge_slots[index as usize].next = None;
            debug_assert!(self.edge_slots[index as usize].edge.is_none());
            return index;
        }
        let index = u32::try_from(self.edge_slots.len()).expect("edge index fits in u32");
        self.edge_slots.push(EdgeSlot {
            next: None,
            edge: None,
            late_epoch: 0,
            late_owner: 0,
            late_value: None,
            late_key: None,
            late_prev: None,
            late_next: None,
        });
        index
    }

    fn append_tail(&mut self, owner: u32, index: u32) {
        self.edge_slots[index as usize].next = None;
        let tail = self.slots[owner as usize].edge_tail;
        match tail {
            Some(tail) => self.edge_slots[tail as usize].next = Some(index),
            None => self.slots[owner as usize].edge_head = Some(index),
        }
        self.slots[owner as usize].edge_tail = Some(index);
    }

    fn unlink_key(&mut self, owner: u32, key: &EdgeKey) -> Result<(), HeapError> {
        self.ensure_sweep_open()?;
        let mut prev = None;
        let mut cursor = self
            .slots
            .get(owner as usize)
            .and_then(|slot| slot.edge_head);
        while let Some(index) = cursor {
            let next = self.edge_slots[index as usize].next;
            let matched = self.edge_slots[index as usize]
                .edge
                .as_ref()
                .is_some_and(|edge| edge.key == *key);
            if matched {
                self.unlink_at(owner, prev, index);
                return Ok(());
            }
            prev = Some(index);
            cursor = next;
        }
        Ok(())
    }

    fn unlink_at(&mut self, owner: u32, prev: Option<u32>, index: u32) {
        let next = self.edge_slots[index as usize].next;
        let cleared_mode = self.slots[owner as usize].mode_edge == Some(index);
        self.repair_parked_cursor(owner, index, next);
        match prev {
            Some(prev) => self.edge_slots[prev as usize].next = next,
            None => self.slots[owner as usize].edge_head = next,
        }
        if self.slots[owner as usize].edge_tail == Some(index) {
            self.slots[owner as usize].edge_tail = prev;
        }
        if cleared_mode {
            self.slots[owner as usize].mode_edge = None;
            self.slots[owner as usize].mode_epoch =
                self.slots[owner as usize].mode_epoch.wrapping_add(1);
        }
        self.free_edge_record(index);
        if cleared_mode {
            self.note_direct_mode_deleted(owner);
        }
    }

    fn free_edge_record(&mut self, index: u32) {
        mark::release_late_edge(self.mark.as_mut(), &mut self.edge_slots, index);
        let edge = self.edge_slots[index as usize]
            .edge
            .take()
            .expect("occupied edge record");
        let gap = edge.value.is_cursor_gap();
        self.payload_bytes -= edge_payload(&edge);
        self.occupied_edges -= 1;
        if !gap {
            self.live_edge_count -= 1;
        }
        self.edge_slots[index as usize].next = self.edge_free_head;
        self.edge_free_head = Some(index);
    }

    fn replace_edge_value(&mut self, owner: u32, index: u32, value: EdgeValue) {
        let (old_gap, barrier) = {
            let edge = self.edge_slots[index as usize]
                .edge
                .as_ref()
                .expect("occupied edge record");
            let old_gap = edge.value.is_cursor_gap();
            let barrier = if self.mark.is_some() {
                replaced_table_target(&edge.key, old_gap, &value)
            } else {
                None
            };
            (old_gap, barrier)
        };
        let edge = self.edge_slots[index as usize]
            .edge
            .as_mut()
            .expect("occupied edge record");
        self.payload_bytes -= value_payload(&edge.value);
        edge.value = value;
        let new_gap = edge.value.is_cursor_gap();
        self.payload_bytes += value_payload(&edge.value);
        match (old_gap, new_gap) {
            (true, false) => self.live_edge_count += 1,
            (false, true) => self.live_edge_count -= 1,
            _ => {}
        }
        self.note_mode_pointer(owner, index);
        if self.mark.is_some() {
            self.barrier_strong_field(owner, index, barrier);
        }
    }

    fn rewrite_gap(&mut self, index: u32) {
        let edge = self.edge_slots[index as usize]
            .edge
            .as_mut()
            .expect("occupied edge record");
        if edge.value.is_cursor_gap() {
            return;
        }
        self.payload_bytes -= value_payload(&edge.value);
        edge.value = EdgeValue::Scalar(Value::Null);
        self.live_edge_count -= 1;
    }

    fn note_mode_pointer(&mut self, owner: u32, index: u32) {
        let is_table = self.slots[owner as usize].kind == HandleKind::Table;
        let is_mode = matches!(
            self.edge_slots[index as usize]
                .edge
                .as_ref()
                .map(|edge| &edge.key),
            Some(EdgeKey::Field(field)) if *field == Self::MODE_FIELD
        );
        if is_table && is_mode {
            self.slots[owner as usize].mode_edge = Some(index);
            self.slots[owner as usize].mode_epoch =
                self.slots[owner as usize].mode_epoch.wrapping_add(1);
            self.note_late_mode_write(owner);
        }
    }
}

fn edge_key_is_slot(key: &EdgeKey, slot: u32) -> bool {
    matches!(key, EdgeKey::Object { slot: key_slot, .. } if *key_slot == slot)
}

/// Ordinary field, UTF-8, number, or table-object key whose new value is a
/// table. Reserved fields, closure and upvalue object keys, and non-table
/// values stay out of the gray queue. A prior gap is also refused, except when
/// an ordinary UTF-8 or number key is refilled with a table: that value uses
/// the same strong-field enqueue as a live edge. The table-object key itself
/// is not recolored; only the new table value is offered to the barrier.
fn replaced_table_target(key: &EdgeKey, old_gap: bool, value: &EdgeValue) -> Option<(u32, u32)> {
    let ordinary_named_or_number = matches!(key, EdgeKey::Utf8(_) | EdgeKey::Number(_));
    if old_gap && !ordinary_named_or_number {
        return None;
    }
    let ordinary = match key {
        EdgeKey::Utf8(_) | EdgeKey::Number(_) => true,
        EdgeKey::Field(field) => *field != LuaHeap::MODE_FIELD && *field != CELL_EDGE,
        EdgeKey::Object {
            kind: HandleKind::Table,
            ..
        } => true,
        EdgeKey::Object { .. } => false,
    };
    if !ordinary {
        return None;
    }
    match value {
        EdgeValue::Table { slot, generation } => Some((*slot, *generation)),
        _ => None,
    }
}

/// Test-only facade over the private selecting mark.
///
/// The pulse witness drives the real mark one charged unit at a time, so it
/// can read the real pending-`__gc` edge meter before and after every call.
/// It does not publish weak edges, close a token, or run a callback.
#[cfg(test)]
pub(crate) struct SelectingPendingMark<'a> {
    session: MarkSession<'a>,
}

#[cfg(test)]
impl<'a> SelectingPendingMark<'a> {
    pub(crate) fn start(heap: &'a mut LuaHeap) -> Self {
        Self {
            session: MarkSession::start_selecting_pending(heap),
        }
    }

    /// Copies the cumulative meter without changing mark state.
    pub(crate) fn meter(&self) -> ScanMeter {
        self.session.meter()
    }

    /// Runs one unit and returns `(finished, cumulative meter)`.
    pub(crate) fn pulse(&mut self, budget: u64) -> (bool, ScanMeter) {
        let view = self.session.pulse(budget);
        (view.finished, view.meter)
    }
}

/// Slot-level callable-carrier units. The business path (a real `.lua` guest
/// import) is exercised by the file-entry witness; these units use fabricated
/// `CallableValue` Arcs only to reach the private store and the generation
/// mechanics that a guest cannot name directly. That is the narrow test-only
/// scope for pure slot refusal; no production or dequeue path can call the
/// store with a bare count.
#[cfg(test)]
mod callable_carrier_units {
    use super::*;

    fn callable(prototype_id: u32) -> Arc<CallableValue> {
        Arc::new(CallableValue {
            prototype_id,
            kind: CallableKind::Closure,
            env: None,
        })
    }

    #[test]
    fn slot_units_cover_rebind_capture_foreign_wrong_kind_and_table_carrier() {
        let mut heap = LuaHeap::new(HeapLimits::production());
        let closure = heap.alloc_closure().expect("closure");
        let first = callable(1);
        let second = callable(2);
        assert!(
            !Arc::ptr_eq(&first, &second),
            "the two Arcs are distinct instances"
        );

        heap.store_guest_callable(closure, Arc::clone(&first), 0)
            .expect("store first");
        assert_eq!(
            heap.guest_callable_admissions(),
            &[closure],
            "a successful store is the only thing that records an admission"
        );
        assert_eq!(
            heap.store_guest_callable(closure, Arc::clone(&second), 0),
            Err(HeapError::CarrierAlreadyBound),
            "a same-generation rebind is refused"
        );
        {
            let stored = heap.closure_callable(closure).expect("stored first");
            assert!(
                Arc::ptr_eq(&stored, &first),
                "the refused rebind left the original Arc"
            );
            assert!(!Arc::ptr_eq(&stored, &second));
        }
        assert_eq!(Arc::strong_count(&first), 2, "local and slot");
        assert_eq!(
            Arc::strong_count(&second),
            1,
            "the refused Arc is not retained"
        );

        let fresh = heap.alloc_closure().expect("fresh closure");
        let captured = callable(3);
        let captured_before = Arc::strong_count(&captured);
        assert_eq!(
            heap.store_guest_callable(fresh, Arc::clone(&captured), 1),
            Err(HeapError::CallableHasEnvironment),
            "a capture count is refused before the slot changes"
        );
        assert_eq!(
            Arc::strong_count(&captured),
            captured_before,
            "the refused store retains no Arc"
        );
        assert!(
            matches!(
                heap.closure_callable(fresh),
                Err(HeapError::CarrierNotBound)
            ),
            "the refused generation stays unbound"
        );

        let unbound = heap.alloc_closure().expect("unbound closure");
        assert!(matches!(
            heap.closure_callable(unbound),
            Err(HeapError::CarrierNotBound)
        ));

        let table = heap.alloc().expect("table");
        let wrong_kind = heap.closure_handle_for_table(table);
        assert!(matches!(
            heap.closure_callable(wrong_kind),
            Err(HeapError::WrongKind)
        ));
        assert_eq!(
            heap.store_guest_callable(wrong_kind, Arc::clone(&second), 0),
            Err(HeapError::WrongKind)
        );

        let mut foreign_heap = LuaHeap::new(HeapLimits::production());
        let foreign = foreign_heap.alloc_closure().expect("foreign closure");
        assert!(matches!(
            heap.closure_callable(foreign),
            Err(HeapError::WrongDomain)
        ));
        assert_eq!(
            heap.store_guest_callable(foreign, Arc::clone(&second), 0),
            Err(HeapError::WrongDomain)
        );

        // The existing table carrier path stays one carrier per generation
        // and is not a callable carrier.
        let issued = heap.issue_carrier(table).expect("table carrier");
        assert!(
            matches!(issued, Value::Array(_)),
            "the table carrier stays the issued VM array"
        );
        assert!(
            matches!(heap.issue_carrier(table), Err(HeapError::StaleHandle)),
            "a second issue for the live generation is refused"
        );
        assert!(matches!(
            heap.closure_callable(wrong_kind),
            Err(HeapError::WrongKind)
        ));
    }

    #[test]
    fn guest_import_schema_is_registered_only_in_the_controlled_shape() {
        let schema = LuaHeap::guest_callable_import_schema();
        assert_eq!(schema.name, LuaHeap::GUEST_CALLABLE_IMPORT);
        assert_eq!(schema.params.len(), 1);
        assert_eq!(schema.params[0].name, "target");
        assert_eq!(schema.params[0].ty, HostTypeSchema::Unknown);
        assert_eq!(schema.params[0].passing, vm::HostParamPassing::Value);
        assert_eq!(schema.return_type, HostTypeSchema::Bool);

        let mut builder = vm::HostApiBuilder::new();
        builder.function(schema);
        let catalog = builder.build().expect("controlled catalog");
        let function = catalog
            .function(LuaHeap::GUEST_CALLABLE_IMPORT)
            .expect("declared function");
        let import = HostImportSchema::from_function(&catalog, function);
        let mut registry = HostFunctionRegistry::empty();
        LuaHeap::register_guest_callable_import(&mut registry, import.clone())
            .expect("the controlled import registers");

        let mut tampered = import;
        tampered.return_type = HostTypeSchema::Int;
        let mut registry = HostFunctionRegistry::empty();
        assert!(
            LuaHeap::register_guest_callable_import(&mut registry, tampered).is_err(),
            "a caller-supplied shape is not accepted as the controlled import"
        );
    }

    #[test]
    fn raw_call_field_import_schema_is_registered_only_in_the_fixed_two_argument_shape() {
        let schema = LuaHeap::guest_callable_field_import_schema();
        assert_eq!(schema.name, LuaHeap::GUEST_CALLABLE_IMPORT);
        assert_eq!(schema.params.len(), 2);
        assert_eq!(schema.params[0].name, "owner");
        assert_eq!(schema.params[0].ty, HostTypeSchema::Unknown);
        assert_eq!(schema.params[0].passing, vm::HostParamPassing::Value);
        assert_eq!(schema.params[1].name, "target");
        assert_eq!(schema.params[1].ty, HostTypeSchema::Unknown);
        assert_eq!(schema.params[1].passing, vm::HostParamPassing::Value);
        assert_eq!(schema.return_type, HostTypeSchema::Bool);

        let mut builder = vm::HostApiBuilder::new();
        builder.function(schema);
        let catalog = builder.build().expect("raw call field catalog");
        let function = catalog
            .function(LuaHeap::GUEST_CALLABLE_IMPORT)
            .expect("declared field function");
        let import = HostImportSchema::from_function(&catalog, function);
        let mut registry = HostFunctionRegistry::empty();
        LuaHeap::register_guest_callable_field_import(&mut registry, import.clone())
            .expect("the fixed two-argument import registers");

        let mut tampered = import;
        tampered.params[0].name = "slot".to_string();
        let mut registry = HostFunctionRegistry::empty();
        assert!(
            LuaHeap::register_guest_callable_field_import(&mut registry, tampered).is_err(),
            "a caller-supplied owner name is not accepted as the controlled shape"
        );

        let mut tampered = HostImportSchema::from_function(&catalog, function);
        tampered.params[1].schema = HostTypeSchema::Int;
        let mut registry = HostFunctionRegistry::empty();
        assert!(
            LuaHeap::register_guest_callable_field_import(&mut registry, tampered).is_err(),
            "a caller-supplied target type is not accepted as the controlled shape"
        );
    }

    #[test]
    fn release_drops_one_arc_and_a_reused_generation_starts_unbound() {
        let mut heap = LuaHeap::new(HeapLimits::production());
        let first_slot = heap.alloc_closure().expect("first closure");
        let first = callable(10);
        heap.store_guest_callable(first_slot, Arc::clone(&first), 0)
            .expect("store first");
        assert_eq!(Arc::strong_count(&first), 2, "local and slot");

        let report = heap.collect();
        assert_eq!(
            report.live_nodes, 0,
            "the unrooted closure must be collected"
        );
        assert!(!heap.closure_alive(first_slot));
        assert!(matches!(
            heap.closure_callable(first_slot),
            Err(HeapError::StaleHandle)
        ));
        assert_eq!(
            Arc::strong_count(&first),
            1,
            "release dropped exactly the slot's one Arc"
        );
        assert_eq!(
            heap.slot_generation(first_slot.slot()),
            Some(first_slot.generation() + 1),
            "release advanced the slot generation"
        );

        let reused = heap.alloc_closure().expect("reuse the released slot");
        assert_eq!(reused.slot(), first_slot.slot());
        assert_eq!(reused.generation(), first_slot.generation() + 1);
        assert!(
            matches!(
                heap.closure_callable(reused),
                Err(HeapError::CarrierNotBound)
            ),
            "a reused generation starts unbound"
        );

        let second = callable(11);
        heap.store_guest_callable(reused, Arc::clone(&second), 0)
            .expect("store on the reused generation");
        assert_eq!(
            heap.guest_callable_admissions(),
            &[first_slot, reused],
            "each admitted generation is recorded once, in order"
        );
        {
            let stored = heap.closure_callable(reused).expect("stored second");
            assert!(
                Arc::ptr_eq(&stored, &second),
                "the reused generation returns the new Arc"
            );
            assert!(
                !Arc::ptr_eq(&stored, &first),
                "the reused generation cannot return the old Arc"
            );
        }
        assert!(
            matches!(
                heap.closure_callable(first_slot),
                Err(HeapError::StaleHandle)
            ),
            "the old generation stays stale after reuse"
        );
    }
}

/// Storage-level units for the raw `__call` field handoff. The business path
/// is one real `.lua` guest import; these units use fabricated `CallableValue`
/// Arcs only to reach the state machine a guest cannot name: saved mark,
/// published Release tail, full node/edge caps, and the unsupported raw values.
#[cfg(test)]
mod raw_call_field_units {
    use super::*;

    fn callable(prototype_id: u32) -> Arc<CallableValue> {
        Arc::new(CallableValue {
            prototype_id,
            kind: CallableKind::Closure,
            env: None,
        })
    }

    fn picture(
        heap: &LuaHeap,
        owner: TableRef,
        metatable: TableRef,
    ) -> (HeapStats, Option<u32>, Option<u32>) {
        (
            heap.stats(),
            heap.slot_generation(owner.slot()),
            heap.slot_generation(metatable.slot()),
        )
    }

    fn owner_with_metatable(
        heap: &mut LuaHeap,
        edge: Option<Value>,
    ) -> (TableRef, TableRef, Value) {
        let owner = heap.alloc().expect("owner");
        let metatable = heap.alloc().expect("metatable");
        heap.set_metatable(owner, Some(metatable))
            .expect("attach metatable");
        if let Some(value) = edge {
            heap.set_named_scalar(metatable, RAW_CALL_FIELD, value)
                .expect("sentinel");
        }
        let owner_value = heap.issue_carrier(owner).expect("owner carrier");
        (owner, metatable, owner_value)
    }

    #[test]
    fn full_edge_cap_replaces_the_sentinel_then_the_installed_closure() {
        let mut heap = LuaHeap::new(HeapLimits {
            nodes: 8,
            roots: 2,
            edges: 1,
            max_generation: 8,
        });
        let (owner, metatable, owner_value) =
            owner_with_metatable(&mut heap, Some(Value::Bool(false)));
        let before = picture(&heap, owner, metatable);
        assert_eq!(
            heap.stats().edge_slots,
            1,
            "the false sentinel occupies the one allowed edge record"
        );

        let first = callable(1);
        let first_closure = heap
            .handoff_guest_callable_to_raw_call(&owner_value, Arc::clone(&first), 0)
            .expect("first replacement");
        assert_eq!(
            heap.named_closure_edge(metatable, RAW_CALL_FIELD)
                .expect("raw edge is a closure"),
            first_closure,
            "the raw edge must name the fresh first generation"
        );
        assert!(
            Arc::ptr_eq(
                &heap.closure_callable(first_closure).expect("first carrier"),
                &first
            ),
            "the first generation must own the exact guest Arc"
        );
        let after_first = picture(&heap, owner, metatable);
        assert_eq!(after_first.0.live_nodes, before.0.live_nodes + 1);
        assert_eq!(
            after_first.0.edge_slots, before.0.edge_slots,
            "replacing an existing record must not allocate a second edge"
        );
        assert_eq!(after_first.0.live_edges, before.0.live_edges);
        assert_eq!(after_first.1, before.1, "the owner generation is unchanged");
        assert_eq!(
            after_first.2, before.2,
            "the metatable generation is unchanged"
        );
        assert_eq!(Arc::strong_count(&first), 2, "local plus the new slot");

        let second = callable(2);
        let second_closure = heap
            .handoff_guest_callable_to_raw_call(&owner_value, Arc::clone(&second), 0)
            .expect("second replacement over the existing closure");
        assert_ne!(first_closure.slot(), second_closure.slot());
        assert_eq!(
            heap.named_closure_edge(metatable, RAW_CALL_FIELD)
                .expect("raw edge is a closure"),
            second_closure,
            "the raw edge must name the fresh second generation"
        );
        assert!(
            Arc::ptr_eq(
                &heap
                    .closure_callable(second_closure)
                    .expect("second carrier"),
                &second
            ),
            "the second generation must own the exact guest Arc"
        );
        assert!(
            heap.closure_alive(first_closure),
            "the replaced generation stays live until a real collection"
        );
        assert!(
            Arc::ptr_eq(
                &heap
                    .closure_callable(first_closure)
                    .expect("first carrier survives replacement"),
                &first
            ),
            "the old generation still owns its own Arc"
        );
        let after_second = picture(&heap, owner, metatable);
        assert_eq!(after_second.0.live_nodes, after_first.0.live_nodes + 1);
        assert_eq!(after_second.0.edge_slots, after_first.0.edge_slots);
        assert_eq!(after_second.1, before.1);
        assert_eq!(after_second.2, before.2);
        assert_eq!(Arc::strong_count(&second), 2, "local plus the new slot");
    }

    #[test]
    fn missing_and_unsupported_raw_states_refuse_without_a_change() {
        let mut heap = LuaHeap::new(HeapLimits {
            nodes: 10,
            roots: 2,
            edges: 4,
            max_generation: 8,
        });
        let (owner, metatable, owner_value) = owner_with_metatable(&mut heap, None);
        let callable = callable(7);

        let missing_before = picture(&heap, owner, metatable);
        assert_eq!(
            heap.handoff_guest_callable_to_raw_call(&owner_value, Arc::clone(&callable), 0),
            Err(HeapError::StaleHandle)
        );
        assert_eq!(picture(&heap, owner, metatable), missing_before);
        assert_eq!(heap.named_raw_scalar(metatable, RAW_CALL_FIELD), Ok(None));
        assert_eq!(Arc::strong_count(&callable), 1);

        heap.set_named_scalar(metatable, RAW_CALL_FIELD, Value::Bool(true))
            .expect("true sentinel");
        let true_before = picture(&heap, owner, metatable);
        assert_eq!(
            heap.handoff_guest_callable_to_raw_call(&owner_value, Arc::clone(&callable), 0),
            Err(HeapError::WrongKind)
        );
        assert_eq!(picture(&heap, owner, metatable), true_before);
        assert_eq!(
            heap.named_raw_scalar(metatable, RAW_CALL_FIELD),
            Ok(Some(Value::Bool(true)))
        );
        assert_eq!(Arc::strong_count(&callable), 1);

        heap.set_named_gap_test_control(metatable, RAW_CALL_FIELD)
            .expect("stored nil gap");
        let gap_before = picture(&heap, owner, metatable);
        assert_eq!(
            heap.handoff_guest_callable_to_raw_call(&owner_value, Arc::clone(&callable), 0),
            Err(HeapError::WrongKind)
        );
        assert_eq!(picture(&heap, owner, metatable), gap_before);
        assert_eq!(
            heap.named_raw_scalar(metatable, RAW_CALL_FIELD),
            Ok(Some(Value::Null))
        );
        assert_eq!(Arc::strong_count(&callable), 1);

        let other = heap.alloc().expect("table target");
        heap.set_named_edge(metatable, RAW_CALL_FIELD, other)
            .expect("table-valued raw call");
        let table_before = picture(&heap, owner, metatable);
        assert_eq!(
            heap.handoff_guest_callable_to_raw_call(&owner_value, Arc::clone(&callable), 0),
            Err(HeapError::WrongKind)
        );
        assert_eq!(picture(&heap, owner, metatable), table_before);
        assert_eq!(heap.named_edge(metatable, RAW_CALL_FIELD), Ok(Some(other)));
        assert_eq!(Arc::strong_count(&callable), 1);

        let userdata = heap.alloc_userdata().expect("userdata target");
        heap.set_named_userdata(metatable, RAW_CALL_FIELD, userdata)
            .expect("userdata-valued raw call");
        let userdata_before = picture(&heap, owner, metatable);
        assert_eq!(
            heap.handoff_guest_callable_to_raw_call(&owner_value, Arc::clone(&callable), 0),
            Err(HeapError::WrongKind)
        );
        assert_eq!(picture(&heap, owner, metatable), userdata_before);
        assert_eq!(Arc::strong_count(&callable), 1);

        let bare_closure = heap.alloc_closure().expect("closure without a carrier");
        heap.set_named_closure(metatable, RAW_CALL_FIELD, bare_closure)
            .expect("closure-valued raw call");
        let bare_before = picture(&heap, owner, metatable);
        assert_eq!(
            heap.handoff_guest_callable_to_raw_call(&owner_value, Arc::clone(&callable), 0),
            Err(HeapError::WrongKind)
        );
        assert_eq!(picture(&heap, owner, metatable), bare_before);
        assert_eq!(Arc::strong_count(&callable), 1);

        let captured_before = picture(&heap, owner, metatable);
        assert_eq!(
            heap.handoff_guest_callable_to_raw_call(&owner_value, Arc::clone(&callable), 1),
            Err(HeapError::CallableHasEnvironment)
        );
        assert_eq!(picture(&heap, owner, metatable), captured_before);
        assert_eq!(Arc::strong_count(&callable), 1);

        let lookalike = Value::array(vec![
            Value::Int(i64::from(owner.slot())),
            Value::Int(i64::from(owner.generation())),
        ]);
        let unissued_before = picture(&heap, owner, metatable);
        assert_eq!(
            heap.handoff_guest_callable_to_raw_call(&lookalike, Arc::clone(&callable), 0),
            Err(HeapError::WrongKind)
        );
        assert_eq!(picture(&heap, owner, metatable), unissued_before);
        assert_eq!(Arc::strong_count(&callable), 1);
    }

    #[test]
    fn saved_mark_and_published_release_tail_refuse_before_any_write() {
        let mut heap = LuaHeap::new(HeapLimits {
            nodes: 10,
            roots: 2,
            edges: 2,
            max_generation: 8,
        });
        let (owner, metatable, owner_value) =
            owner_with_metatable(&mut heap, Some(Value::Bool(false)));
        let callable = callable(9);

        ParkedMark::open_saved(&mut heap).expect("saved mark");
        assert!(heap.has_saved_mark());
        let mark_before = picture(&heap, owner, metatable);
        assert_eq!(
            heap.handoff_guest_callable_to_raw_call(&owner_value, Arc::clone(&callable), 0),
            Err(HeapError::MarkDenied)
        );
        assert_eq!(picture(&heap, owner, metatable), mark_before);
        assert_eq!(
            heap.named_raw_scalar(metatable, RAW_CALL_FIELD),
            Ok(Some(Value::Bool(false)))
        );
        assert_eq!(Arc::strong_count(&callable), 1);
        drop(ParkedMark::resume(&mut heap).expect("saved mark is still open"));
        assert!(!heap.has_saved_mark());

        let dead = heap.alloc().expect("dead table");
        let frame = heap.enter_roots(1).expect("root frame");
        heap.write_root(frame, 0, owner).expect("root owner");
        let modes = heap.mark_reachable();
        match heap.halt_at_publish(modes) {
            PublishHalt::Published { .. } => {}
            _ => panic!("expected a published Release tail"),
        }
        assert!(heap.has_sweep_tail());
        assert_eq!(
            heap.saved_tail_state().expect("saved tail state").phase,
            SavedTailPhase::Release
        );
        let tail_before = picture(&heap, owner, metatable);
        assert_eq!(
            heap.handoff_guest_callable_to_raw_call(&owner_value, Arc::clone(&callable), 0),
            Err(HeapError::SweepPending)
        );
        assert_eq!(picture(&heap, owner, metatable), tail_before);
        assert_eq!(
            heap.named_raw_scalar(metatable, RAW_CALL_FIELD),
            Ok(Some(Value::Bool(false)))
        );
        assert_eq!(Arc::strong_count(&callable), 1);
        assert!(matches!(
            heap.resume_sweep_tail(u64::MAX),
            TailResume::Completed(_)
        ));
        assert!(!heap.alive(dead));
        heap.clear_root(frame, 0).expect("clear the owner root");
        heap.leave_roots(frame).expect("leave the owner root frame");
    }

    #[test]
    fn full_node_capacity_refuses_without_changing_the_field() {
        let mut heap = LuaHeap::new(HeapLimits {
            nodes: 2,
            roots: 2,
            edges: 1,
            max_generation: 8,
        });
        let (owner, metatable, owner_value) =
            owner_with_metatable(&mut heap, Some(Value::Bool(false)));
        assert_eq!(heap.stats().node_slots, 2);
        let callable = callable(11);
        let before = picture(&heap, owner, metatable);
        assert_eq!(
            heap.handoff_guest_callable_to_raw_call(&owner_value, Arc::clone(&callable), 0),
            Err(HeapError::NodeCapacity)
        );
        assert_eq!(picture(&heap, owner, metatable), before);
        assert_eq!(
            heap.named_raw_scalar(metatable, RAW_CALL_FIELD),
            Ok(Some(Value::Bool(false)))
        );
        assert_eq!(Arc::strong_count(&callable), 1);
    }
}

#[cfg(test)]
#[path = "../../../../tests/luajit_heap_runtime/budgeted_mark.rs"]
mod budgeted_mark;
#[cfg(test)]
#[path = "../../../../tests/luajit_heap_runtime/budgeted_sweep.rs"]
mod budgeted_sweep;
#[cfg(test)]
#[path = "../../../../tests/luajit_heap_runtime/budgeted_token_scan.rs"]
mod budgeted_token_scan;

#[cfg(test)]
#[path = "../../../../tests/luajit_heap_runtime/first_roots_handoff.rs"]
mod first_roots_handoff;

#[cfg(test)]
#[path = "../../../../tests/luajit_heap_runtime/parked_lifecycle.rs"]
mod parked_lifecycle;
#[cfg(test)]
#[path = "../../../../tests/luajit_heap_runtime/parked_mark.rs"]
mod parked_mark;
#[cfg(test)]
#[path = "../../../../tests/luajit_heap_runtime/private_cycle.rs"]
mod private_cycle;
#[cfg(test)]
#[path = "../../../../tests/luajit_heap_runtime/published_delete.rs"]
mod published_delete;
#[cfg(test)]
#[path = "../../../../tests/luajit_heap_runtime/published_late_allocation.rs"]
mod published_late_allocation;
#[cfg(test)]
#[path = "../../../../tests/luajit_heap_runtime/published_refill.rs"]
mod published_refill;
#[cfg(test)]
#[path = "../../../../tests/luajit_heap_runtime/published_root_frame_retirement.rs"]
mod published_root_frame_retirement;
#[cfg(test)]
#[path = "../../../../tests/luajit_heap_runtime/published_sweep.rs"]
mod published_sweep;
#[cfg(test)]
#[path = "../../../../tests/luajit_heap_runtime/resume_cycle.rs"]
mod resume_cycle;
#[cfg(test)]
#[path = "../../../../tests/luajit_heap_runtime/safepoint_roots.rs"]
mod safepoint_roots;
#[cfg(test)]
#[path = "../../../../tests/luajit_heap_runtime/step_cycle.rs"]
mod step_cycle;
#[cfg(test)]
#[path = "../../../../tests/luajit_heap_runtime/userdata_identity.rs"]
mod userdata_identity;
