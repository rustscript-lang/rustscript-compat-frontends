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

use vm::{HostContextError, Value};

use super::handle::{ClosureRef, HandleKind, TableRef, UpvalueRef, UserdataRef};

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
}

impl HeapError {
    pub(super) fn from_scope(error: HostContextError) -> Self {
        Self::Scope(error.to_string())
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

struct Slot {
    generation: u32,
    kind: HandleKind,
    state: SlotState,
    /// Raw resource handle for this node. Ownership metadata only: the mark
    /// phase does not read it, so the token is not a GC root.
    token: Option<u64>,
    /// Canonical VM array for this generation, if issued. The slot owns the
    /// array so a field can read it back while the node is live. It contains
    /// identity only, not object edges, and does not participate in marking.
    carrier: Option<Arc<Vec<Value>>>,
    /// Optional metatable. Not a field key and not an edge record.
    metatable: Option<(u32, u32)>,
    /// Head of this owner's edge chain. `None` means no edges. A reused owner
    /// starts with no chain.
    edge_head: Option<u32>,
    /// Tail of this owner's edge chain. New keys append here.
    edge_tail: Option<u32>,
    /// Direct `__mode` edge of a table, if that field is stored. Not a parsed
    /// mode, and not set for field 0 on a closure or cell.
    mode_edge: Option<u32>,
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
/// stored metatable refs followed from marked nodes. A collect with none
/// stored does not read those slots, so the field-edge counters stay the
/// same as a heap that never had a metatable.
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
    pub owner_rescan_inited: u64,
    /// Owners linked into the rescan queue. One per owner per mark. A repeat
    /// setter does not link again and does not increment this.
    pub owner_rescan_pushed: u64,
}

/// A stored token identified while marking. Close checks this slot and
/// generation directly; it does not search the slot vector.
#[derive(Clone, Copy, Debug)]
pub(super) struct DoomedToken {
    pub(super) raw: u64,
    pub(super) slot: u32,
    pub(super) generation: u32,
}

/// Parsed weak modes for one collect. Built during the clear-mark slot walk
/// from each table's direct `__mode` pointer, then borrowed by sweep. A close
/// error before sweep drops it with the mark result, so the next collect
/// parses again. It is not stored on the heap.
pub(super) struct MarkSnapshot {
    modes: Option<Vec<WeakMode>>,
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

/// Where a publish-only sweep stopped. A completion means no dead slot was
/// published. `Published` has rewritten weak edges and has not released.
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
        }
    }

    /// Returns the counts since the previous take and clears them.
    /// Does not collect, and does not record this read as collection work.
    pub fn take_scan_meter(&mut self) -> ScanMeter {
        let meter = self.scan;
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
        slot.edge_head = None;
        slot.edge_tail = None;
        slot.mode_edge = None;
        slot.rescan_epoch = 0;
        slot.rescan_queued = false;
        slot.rescan_prev = None;
        slot.rescan_next = None;
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
            edge_head: None,
            edge_tail: None,
            mode_edge: None,
            rescan_epoch: 0,
            rescan_queued: false,
            rescan_prev: None,
            rescan_next: None,
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
        self.clear_roots_to(handle);
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
        self.slots[index].carrier = Some(carrier);
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
        self.slots[index].carrier = Some(carrier);
        Ok(value)
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
        let current = stored.carrier.as_ref()?;
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
            self.barrier_strong_field(from_slot, slot, generation);
        }
        if let Some(insert) = table_key {
            self.barrier_new_table_key(
                from_slot,
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

    /// Clones the slot's issued carrier for a field read. This does not add a root.
    pub fn issued_value(&self, table: TableRef) -> Result<Value, HeapError> {
        self.require_live(table)?;
        let carrier = self.slots[table.slot() as usize]
            .carrier
            .as_ref()
            .ok_or(HeapError::StaleHandle)?;
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

    /// Clears `token` only when that exact slot and generation still store it.
    /// A reused slot, including a successor, is left alone.
    pub(super) fn clear_exact_token(&mut self, token: DoomedToken) -> bool {
        self.scan.token_slot_checks += 1;
        let matches = self.slots.get(token.slot as usize).is_some_and(|slot| {
            slot.generation == token.generation && slot.token == Some(token.raw)
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
    /// which closes tokens before [`Self::sweep_marked`]. Calling this on a
    /// host heap would skip that close.
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
    /// This function does not return a partial sweep.
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
    /// once, after Owners and before Release, and only when a dead slot exists.
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

    /// Publishes weak edges and stores the release tail. No dead slot finishes
    /// the sweep and records the one report.
    pub(super) fn halt_at_publish(&mut self, modes: MarkSnapshot) -> PublishHalt {
        debug_assert!(self.sweep_tail.is_none(), "publish must not stack a tail");
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

    fn release_slot(&mut self, index: u32) {
        debug_assert!(
            self.slots[index as usize].edge_head.is_none()
                && self.slots[index as usize].edge_tail.is_none(),
            "owner chain must already be unlinked; release does not scan edges"
        );
        let mark = self.mark.as_mut();
        mark::release_rescan_slot(mark, &mut self.slots, index);
        if self.slots[index as usize].metatable.take().is_some() {
            self.metatable_edges = self.metatable_edges.saturating_sub(1);
        }
        let slot = &mut self.slots[index as usize];
        slot.token = None;
        slot.carrier = None;
        slot.edge_head = None;
        slot.edge_tail = None;
        slot.mode_edge = None;
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

    fn clear_roots_to(&mut self, handle: TableRef) {
        for root in &mut self.roots {
            if *root == Some(Rooted::Table(handle)) {
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
        self.repair_parked_cursor(owner, index, next);
        match prev {
            Some(prev) => self.edge_slots[prev as usize].next = next,
            None => self.slots[owner as usize].edge_head = next,
        }
        if self.slots[owner as usize].edge_tail == Some(index) {
            self.slots[owner as usize].edge_tail = prev;
        }
        if self.slots[owner as usize].mode_edge == Some(index) {
            self.slots[owner as usize].mode_edge = None;
        }
        self.free_edge_record(index);
    }

    fn free_edge_record(&mut self, index: u32) {
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
        if let Some((slot, generation)) = barrier {
            self.barrier_strong_field(owner, slot, generation);
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
