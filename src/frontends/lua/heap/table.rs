//! Non-moving mark/sweep storage for tables, closures, and shared cells.
//!
//! Closure and cell nodes use the same slots, edges, roots, and sweep as
//! tables. A private field id is not a Lua number. Internal links are strong.
//! Only a table entry follows that table's weak mode. An object key stores
//! the slot, generation, and kind. A closure key is that record with closure
//! kind. It is not a private field id and not a second edge table.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use vm::{HostContextError, Value};

use super::handle::{ClosureRef, HandleKind, TableRef, UpvalueRef};

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
    /// Table-entry records currently stored.
    pub live_edges: u32,
    /// Slot vector length. Free and retired slots still occupy it.
    pub node_slots: u32,
    /// Root vector length.
    pub root_slots: u32,
    /// Edge vector length.
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
    /// Optional metatable. Not a field key and not an `edges` record.
    metatable: Option<(u32, u32)>,
}

struct Edge {
    from: u32,
    /// Field ids, object identities, string text, and binary64 numbers share
    /// this record. An object key is the live slot, generation, and kind. A
    /// string key is the complete UTF-8 text. A number key is canonical binary64
    /// bits, not a field id and not a collectable node.
    key: EdgeKey,
    value: EdgeValue,
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
    /// One guest scalar in the same record as a table target. Field 0 is the
    /// only key whose string is read as `__mode`.
    Scalar(Value),
}

impl EdgeValue {
    fn object_slot(&self) -> Option<u32> {
        match self {
            Self::Table { slot, .. } | Self::Closure { slot, .. } | Self::Upvalue { slot, .. } => {
                Some(*slot)
            }
            Self::Scalar(_) => None,
        }
    }
}

/// Value stored in a shared cell's current edge.
///
/// A missing edge is nil and is not a variant. `None` from [`LuaHeap::read_cell`]
/// is that nil. An error is an invalid handle, not nil. A cell is not issued
/// as a table value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellValue {
    /// The cell currently holds a table.
    Table(TableRef),
    /// The cell currently holds a closure.
    Closure(ClosureRef),
}

/// One explicit root. Table, closure, and upvalue roots share the root vector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rooted {
    Table(TableRef),
    Closure(ClosureRef),
    Upvalue(UpvalueRef),
}

impl Rooted {
    fn slot(self) -> u32 {
        match self {
            Self::Table(handle) => handle.slot(),
            Self::Closure(handle) => handle.slot(),
            Self::Upvalue(handle) => handle.slot(),
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

    /// One object-key record. Table and closure keys differ by `kind`.
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
/// Edge work stays in its own fields. `index_edge_checks` is the two passes
/// that build the outgoing index (2E). `outgoing_edge_checks` counts records
/// inspected from marked nodes. Each followed object target contributes
/// separately to `node_slot_visits`; skipped weak targets do not.
/// `sweep_edge_checks` is the one edge filter,
/// and it is zero when every live node is marked. Adding those fields is not
/// a total GC cost.
///
/// A metatable is not in that edge vector. `metatable_edge_checks` counts
/// stored metatable refs followed from marked nodes. A collect with none
/// stored does not read those slots, so the three field-edge counters stay
/// the same as a heap that never had a metatable.
///
/// `index_slot_checks` is the one counts-to-offsets walk over index scratch.
/// Its length is the node-slot vector. It is not a heap-node walk. Heap node
/// walks are `node_slot_visits`. Root-vector walks are `root_slot_visits`.
/// Token identity checks stay in `token_slot_checks`.
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
    pub index_edge_checks: u64,
    pub index_slot_checks: u64,
    pub outgoing_edge_checks: u64,
    pub sweep_edge_checks: u64,
    /// Stored metatable references followed from a marked node.
    ///
    /// A metatable is an optional per-slot reference, not a field-edge record
    /// and not an integer key. It is not included in `index_edge_checks`,
    /// `outgoing_edge_checks`, or `sweep_edge_checks`. A heap with no stored
    /// metatable reference does not read this path, so those collects add zero
    /// and leave the field-edge counts unchanged.
    pub metatable_edge_checks: u64,
    /// Bytes examined while parsing direct string modes (including a NUL terminator if present).
    pub weak_mode_bytes: u64,
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
    /// `counts` zero-initialized to the node-slot length.
    pub index_counts_inited: u64,
    /// Capacity requested for `offsets` (`slots + 1`).
    pub index_offsets_reserved: u64,
    /// Elements pushed into `offsets`.
    pub index_offsets_pushed: u64,
    /// Elements copied into `cursor` from `offsets`.
    pub index_cursor_copied: u64,
    /// Outgoing index entries zero-initialized to the in-range edge total.
    pub index_targets_inited: u64,
    /// Slot-indexed modes zero-initialized on the first weak mode in the index.
    pub weak_modes_inited: u64,
    /// Work-stack pushes. One per newly marked node. Not a reserve.
    pub work_pushed: u64,
    /// Pending-close elements appended. Host mark only; pure collect is 0.
    pub pending_close_pushed: u64,
    /// Dead-slot indices appended during sweep selection. 0 if sweep did not run.
    pub dead_slots_pushed: u64,
    /// Mark bitmap zero-initialized when the edge filter runs. 0 when skipped.
    pub sweep_bitmap_inited: u64,
}

/// A stored token identified while marking. Close checks this slot and
/// generation directly; it does not search the slot vector.
#[derive(Clone, Copy, Debug)]
pub(super) struct DoomedToken {
    pub(super) raw: u64,
    pub(super) slot: u32,
    pub(super) generation: u32,
}

/// Temporary outgoing-edge index for one mark. Dropped when mark returns.
/// Not stored on the heap, so it is not a second persistent graph.
struct OutgoingIndex {
    offsets: Vec<u32>,
    entries: Vec<u32>,
    weak_modes: Option<Vec<WeakMode>>,
}

#[derive(Clone, Copy, Default)]
struct WeakMode {
    keys: bool,
    values: bool,
}

impl WeakMode {
    fn parse(mode: &str, bytes: &mut u64) -> Self {
        let mut weak = Self::default();
        for byte in mode.bytes() {
            *bytes += 1;
            match byte {
                b'k' => weak.keys = true,
                b'v' => weak.values = true,
                0 => break, // LuaJIT scans the NUL-terminated mode string.
                _ => {}
            }
        }
        weak
    }
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
    edges: Vec<Edge>,
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
            edges: Vec::new(),
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

    fn alloc_slot(&mut self, kind: HandleKind) -> Result<(u32, u32), HeapError> {
        if let Some(index) = self.free_list.pop() {
            let slot = &mut self.slots[index as usize];
            if !matches!(slot.state, SlotState::Free) {
                return Err(HeapError::StaleHandle);
            }
            slot.kind = kind;
            slot.state = SlotState::Live { marked: false };
            slot.token = None;
            slot.carrier = None;
            slot.metatable = None;
            return Ok((index, slot.generation));
        }
        if self.slots.len() >= self.limits.nodes as usize {
            return Err(HeapError::NodeCapacity);
        }
        if self.limits.max_generation == 0 {
            return Err(HeapError::GenerationExhausted);
        }
        let index = u32::try_from(self.slots.len()).map_err(|_| HeapError::NodeCapacity)?;
        self.slots.push(Slot {
            generation: 1,
            kind,
            state: SlotState::Live { marked: false },
            token: None,
            carrier: None,
            metatable: None,
        });
        self.note_node_peak();
        Ok((index, 1))
    }

    /// Releases a reserved node. A stale or foreign handle is refused and the
    /// heap is left unchanged. The slot stays in the vector so its generation
    /// cannot be reissued as 1.
    pub fn rollback(&mut self, handle: TableRef) -> Result<(), HeapError> {
        self.require_live(handle)?;
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
    /// record. A closure target uses [`Self::write_closure_upvalue`].
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
        )
    }

    /// Replaces the current edge with a closure. Same record as a table write.
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
        )
    }

    /// Removes the current edge. Nil is the missing edge, not a stored record.
    pub fn clear_upvalue(&mut self, cell: UpvalueRef) -> Result<(), HeapError> {
        self.require_upvalue(cell)?;
        let key = EdgeKey::Field(CURRENT_EDGE);
        self.edges
            .retain(|edge| edge.from != cell.slot() || edge.key != key);
        Ok(())
    }

    /// Reads the current edge through a closure.
    ///
    /// `Ok(None)` is nil. `Err` means the closure, its cell, or the stored
    /// target is an invalid handle. A missing edge is not an invalid handle.
    /// This does not call the closure.
    pub fn read_cell(&self, closure: ClosureRef) -> Result<Option<CellValue>, HeapError> {
        let cell = self.upvalue_of(closure)?;
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
            EdgeValue::Upvalue { .. } | EdgeValue::Scalar(_) => Err(HeapError::WrongKind),
        }
    }

    /// Reads the table currently stored through a closure's cell.
    ///
    /// A closure target is `WrongKind`. A missing edge has no table, so this
    /// table adapter returns `StaleHandle`. [`Self::read_cell`] returns `None`
    /// for that nil.
    pub fn read_upvalue(&self, closure: ClosureRef) -> Result<TableRef, HeapError> {
        match self.read_cell(closure)? {
            Some(CellValue::Table(table)) => Ok(table),
            Some(CellValue::Closure(_)) => Err(HeapError::WrongKind),
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
    /// through a closure.
    pub fn current_table(&self, cell: UpvalueRef) -> Result<Option<TableRef>, HeapError> {
        self.require_upvalue(cell)?;
        let Some(value) = self.stored_from(cell.slot(), &EdgeKey::Field(CURRENT_EDGE)) else {
            return Ok(None);
        };
        match value {
            EdgeValue::Table { slot, generation } => {
                let table = TableRef::table(self.domain, *slot, *generation);
                self.require_live(table)?;
                Ok(Some(table))
            }
            _ => Err(HeapError::WrongKind),
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
            self.clear_edge(from, &key);
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
    /// A number or boolean is accepted only for the reserved mode field. The
    /// direct numeric field API stays available for heap tests.
    pub fn set_named_scalar(
        &mut self,
        from: TableRef,
        name: &str,
        value: Value,
    ) -> Result<(), HeapError> {
        let key = EdgeKey::named(name);
        let mode = matches!(key, EdgeKey::Field(Self::MODE_FIELD));
        debug_assert!(
            matches!(value, Value::Null | Value::String(_))
                || (mode && matches!(value, Value::Float(_) | Value::Bool(_)))
        );
        self.require_live(from)?;
        if matches!(value, Value::Null) {
            self.clear_edge(from, &key);
            Ok(())
        } else {
            self.write_edge(from, key, EdgeValue::Scalar(value))
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
        self.clear_edge(from, &key);
        Ok(())
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

    /// Removes one closure-key record. Another closure, including one that
    /// shares this key's cell, is a different identity and stays.
    pub fn clear_closure_key(&mut self, from: TableRef, key: ClosureRef) -> Result<(), HeapError> {
        self.require_live(from)?;
        self.require_closure(key)?;
        let key = EdgeKey::closure_key(key);
        self.clear_edge(from, &key);
        Ok(())
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
        self.clear_edge(from, &key);
        Ok(())
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

    fn number_key(key: f64) -> Result<EdgeKey, HeapError> {
        if key.is_nan() {
            return Err(HeapError::NanIndex);
        }
        Ok(EdgeKey::number(key))
    }

    fn clear_edge(&mut self, from: TableRef, key: &EdgeKey) {
        self.edges
            .retain(|edge| edge.from != from.slot() || &edge.key != key);
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
        if let Some(edge) = self
            .edges
            .iter_mut()
            .find(|edge| edge.from == from_slot && edge.key == key)
        {
            edge.value = value;
            return Ok(());
        }
        if self.edges.len() >= self.limits.edges as usize {
            return Err(HeapError::EdgeCapacity);
        }
        self.edges.push(Edge {
            from: from_slot,
            key,
            value,
        });
        self.note_edge_peak();
        Ok(())
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

    /// Next raw entry of `table` after `cursor`, in stored edge order.
    ///
    /// `Value::Null` starts at the first edge. `None` is the end: one nil, not
    /// an empty pair. A missing cursor and a non-guest key are errors, not the
    /// end and not a restart. This walks `edges`, the same vector weak sweep
    /// retains. It does not allocate another table and does not read a
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
        for edge in &self.edges {
            if edge.from != slot {
                continue;
            }
            if !passed {
                if cursor
                    .as_ref()
                    .is_some_and(|cursor| cursor.matches(&edge.key))
                {
                    passed = true;
                }
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
            EdgeValue::Closure { .. } | EdgeValue::Upvalue { .. } => {
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
            Some(EdgeValue::Closure { .. } | EdgeValue::Upvalue { .. }) => {
                Err(HeapError::WrongKind)
            }
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
            Some(EdgeValue::Closure { .. } | EdgeValue::Upvalue { .. }) => {
                Err(HeapError::WrongKind)
            }
        }
    }

    fn stored_from(&self, from_slot: u32, key: &EdgeKey) -> Option<&EdgeValue> {
        self.edges
            .iter()
            .find(|edge| edge.from == from_slot && &edge.key == key)
            .map(|edge| &edge.value)
    }

    /// One scan of the heap edge vector. `from` is checked before any key test.
    /// The predicate selects the stored key. This does not build another table.
    fn read_edge(
        &self,
        from: TableRef,
        matches_key: impl Fn(&EdgeKey) -> bool,
    ) -> Result<Option<&EdgeValue>, HeapError> {
        self.require_live(from)?;
        Ok(self
            .edges
            .iter()
            .find(|edge| edge.from == from.slot() && matches_key(&edge.key))
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
        let had = self.slots[owner.slot() as usize].metatable.is_some();
        let next = mt.map(|target| (target.slot(), target.generation()));
        if had != next.is_some() {
            if next.is_some() {
                self.metatable_edges = self.metatable_edges.saturating_add(1);
            } else {
                self.metatable_edges = self.metatable_edges.saturating_sub(1);
            }
        }
        self.slots[owner.slot() as usize].metatable = next;
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
        let index = found.start as usize + slot as usize;
        self.roots[index] = Some(rooted);
        Ok(())
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
        let start = top.start as usize;
        self.roots.truncate(start);
        self.frames.pop();
        Ok(())
    }

    /// Records the typed token that owns `table`. The handle is not installed
    /// as a root.
    pub fn bind_token(&mut self, table: TableRef, raw: u64) -> Result<(), HeapError> {
        self.require_live(table)?;
        let slot = &mut self.slots[table.slot() as usize];
        if slot.token.is_some() {
            return Err(HeapError::StaleHandle);
        }
        slot.token = Some(raw);
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

    /// Marks once and returns unreachable stored tokens with their slots.
    /// Does not release nodes. The caller closes those tokens, then
    /// [`Self::sweep_marked`] uses this same mark.
    pub(super) fn mark_and_doomed_tokens(&mut self) -> Vec<DoomedToken> {
        self.mark_reachable();
        let doomed: Vec<DoomedToken> = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| match slot.state {
                SlotState::Live { marked: false } => slot.token.map(|raw| DoomedToken {
                    raw,
                    slot: index as u32,
                    generation: slot.generation,
                }),
                _ => None,
            })
            .collect();
        self.scan.node_slot_visits += self.slots.len() as u64;
        self.scan.scratch.pending_close_pushed += doomed.len() as u64;
        doomed
    }

    /// Non-moving mark from explicit roots, then one sweep of unmarked nodes.
    /// Freed slots are reused with a bumped generation. A slot whose next
    /// generation would pass `max_generation` is retired instead.
    ///
    /// This is the pure-heap collector. A host heap must use `heap::collect`,
    /// which closes tokens before [`Self::sweep_marked`]. Calling this on a
    /// host heap would skip that close.
    pub fn collect(&mut self) -> CollectReport {
        self.mark_reachable();
        self.sweep_marked()
    }

    /// Releases nodes the current mark did not reach. Does not mark again.
    ///
    /// Selection and the marked count share one slot pass. After that pass,
    /// every unmarked live node is released, so remaining live nodes are the
    /// marked count. The report uses that count instead of walking the slot
    /// vector again.
    pub(super) fn sweep_marked(&mut self) -> CollectReport {
        let mut swept = Vec::new();
        let mut marked_nodes = 0u32;
        self.scan.node_slot_visits += self.slots.len() as u64;
        for (index, slot) in self.slots.iter().enumerate() {
            match slot.state {
                SlotState::Live { marked: false } => swept.push(index as u32),
                SlotState::Live { marked: true } => marked_nodes += 1,
                SlotState::Free | SlotState::Retired => {}
            }
        }
        self.scan.scratch.dead_slots_pushed += swept.len() as u64;
        if !swept.is_empty() {
            self.sweep_edges_once();
            for index in swept {
                self.scan.node_slot_visits += 1;
                self.release_slot(index);
            }
        }
        let live_roots = self.clear_dead_roots();
        self.collections = self.collections.saturating_add(1);
        self.last_marked = marked_nodes;
        CollectReport {
            live_nodes: marked_nodes,
            marked_nodes,
            live_roots,
        }
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

    /// Current occupancy. Peaks count storage, not only live objects.
    /// Does not collect and does not add scan counts.
    pub fn stats(&self) -> HeapStats {
        HeapStats {
            live_nodes: self.live_nodes(),
            live_roots: self.live_roots(),
            live_edges: self.edges.len() as u32,
            node_slots: self.slots.len() as u32,
            root_slots: self.roots.len() as u32,
            edge_slots: self.edges.len() as u32,
            collections: self.collections,
            last_marked: self.last_marked,
            peak_nodes: self.peak_nodes,
            peak_roots: self.peak_roots,
            peak_edges: self.peak_edges,
        }
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

    fn mark_reachable(&mut self) {
        self.scan.mark_passes += 1;
        self.scan.node_slot_visits += self.slots.len() as u64;
        for slot in &mut self.slots {
            if let SlotState::Live { marked } = &mut slot.state {
                *marked = false;
            }
        }
        let index = self.build_outgoing_index();
        let mut work = Vec::new();
        self.scan.root_slot_visits += self.roots.len() as u64;
        let rooted: Vec<Rooted> = self.roots.iter().copied().flatten().collect();
        for rooted in rooted {
            self.scan.node_slot_visits += 1;
            if self.rooted_alive(rooted) {
                self.scan.node_slot_visits += 1;
                self.mark_slot(rooted.slot(), &mut work);
            }
        }
        let follow_metatables = self.metatable_edges > 0;
        while let Some(slot) = work.pop() {
            let from_table = self.slots[slot as usize].kind == HandleKind::Table;
            let mode = if from_table && follow_metatables {
                self.follow_metatable(slot, &index, &mut work)
            } else {
                WeakMode::default()
            };
            let start = index.offsets[slot as usize] as usize;
            let end = index.offsets[slot as usize + 1] as usize;
            self.scan.outgoing_edge_checks += (end - start) as u64;
            for &entry in &index.entries[start..end] {
                let (object_key, value_target) = {
                    let edge = &self.edges[entry as usize];
                    let object_key = match &edge.key {
                        EdgeKey::Object {
                            slot,
                            generation,
                            kind,
                        } => Some((*slot, *generation, *kind)),
                        EdgeKey::Field(_) | EdgeKey::Utf8(_) | EdgeKey::Number(_) => None,
                    };
                    let value_target = match &edge.value {
                        EdgeValue::Table { slot, generation } => {
                            Some((*slot, *generation, HandleKind::Table))
                        }
                        EdgeValue::Closure { slot, generation } => {
                            Some((*slot, *generation, HandleKind::Closure))
                        }
                        EdgeValue::Upvalue { slot, generation } => {
                            Some((*slot, *generation, HandleKind::Upvalue))
                        }
                        EdgeValue::Scalar(_) => None,
                    };
                    (object_key, value_target)
                };
                if from_table
                    && !mode.keys
                    && let Some((slot, generation, kind)) = object_key
                {
                    self.follow_target(slot, generation, kind, &mut work);
                }
                // A table entry follows that table's weak mode. A closure or
                // cell link is not a table entry, so it stays strong.
                let strong_value = !from_table || !mode.values;
                if strong_value && let Some((slot, generation, kind)) = value_target {
                    self.follow_target(slot, generation, kind, &mut work);
                }
            }
        }
    }

    /// Follows one stored metatable ref. Absent refs are not edges and are
    /// not counted. The caller skips this entirely when no ref is stored.
    fn follow_metatable(
        &mut self,
        slot: u32,
        index: &OutgoingIndex,
        work: &mut Vec<u32>,
    ) -> WeakMode {
        let Some((to_slot, to_generation)) = self.slots[slot as usize].metatable else {
            return WeakMode::default();
        };
        self.scan.metatable_edge_checks += 1;
        if self.follow_target(to_slot, to_generation, HandleKind::Table, work)
            && let Some(modes) = &index.weak_modes
        {
            self.scan.weak_mode_lookups += 1;
            return modes[to_slot as usize];
        }
        WeakMode::default()
    }

    fn follow_target(
        &mut self,
        to_slot: u32,
        to_generation: u32,
        kind: HandleKind,
        work: &mut Vec<u32>,
    ) -> bool {
        self.scan.node_slot_visits += 1;
        if !self.slot_matches(to_slot, to_generation, kind) {
            return false;
        }
        self.scan.node_slot_visits += 1;
        self.mark_slot(to_slot, work);
        true
    }

    /// Two passes over the stored edge vector, plus one pass over the counts
    /// scratch to turn those counts into offsets. `index_slot_checks` is that
    /// scratch pass. Heap node walks are counted separately. The index is
    /// dropped at the end of mark.
    fn build_outgoing_index(&mut self) -> OutgoingIndex {
        let slot_count = self.slots.len();
        let edge_count = self.edges.len();
        self.scan.index_edge_checks += edge_count as u64;
        let mut counts = vec![0u32; slot_count];
        self.scan.scratch.index_counts_inited += counts.len() as u64;
        for edge in &self.edges {
            if let Some(count) = counts.get_mut(edge.from as usize) {
                *count = count.saturating_add(1);
            }
        }
        self.scan.index_slot_checks += slot_count as u64;
        let offset_request = slot_count + 1;
        self.scan.scratch.index_offsets_reserved += offset_request as u64;
        let mut offsets = Vec::with_capacity(offset_request);
        offsets.push(0);
        let mut running = 0u32;
        for count in &counts {
            running = running.saturating_add(*count);
            offsets.push(running);
        }
        self.scan.scratch.index_offsets_pushed += offsets.len() as u64;
        self.scan.index_edge_checks += edge_count as u64;
        let mut cursor = offsets.clone();
        self.scan.scratch.index_cursor_copied += cursor.len() as u64;
        let mut entries = vec![0u32; running as usize];
        self.scan.scratch.index_targets_inited += entries.len() as u64;
        let mut weak_modes = None;
        for (entry, edge) in self.edges.iter().enumerate() {
            let from = edge.from as usize;
            if from >= slot_count {
                continue;
            }
            let at = cursor[from] as usize;
            if at < entries.len() {
                entries[at] = entry as u32;
                cursor[from] = cursor[from].saturating_add(1);
            }
            if self.metatable_edges > 0
                && edge.key == EdgeKey::Field(Self::MODE_FIELD)
                && let EdgeValue::Scalar(Value::String(mode)) = &edge.value
            {
                let mode = WeakMode::parse(mode, &mut self.scan.weak_mode_bytes);
                if mode.keys || mode.values {
                    let modes = weak_modes.get_or_insert_with(|| {
                        self.scan.scratch.weak_modes_inited += slot_count as u64;
                        vec![WeakMode::default(); slot_count]
                    });
                    modes[from] = mode;
                }
            }
        }
        OutgoingIndex {
            offsets,
            entries,
            weak_modes,
        }
    }

    /// One filter of the edge vector. Skipped when every live node is marked,
    /// so an all-live collect does not walk edges again and does not build
    /// the mark bitmap.
    fn sweep_edges_once(&mut self) {
        struct SweepSlot {
            marked: bool,
            kind: HandleKind,
            generation: u32,
        }
        let sweep: Vec<SweepSlot> = self
            .slots
            .iter()
            .map(|slot| SweepSlot {
                marked: matches!(slot.state, SlotState::Live { marked: true }),
                kind: slot.kind,
                generation: slot.generation,
            })
            .collect();
        self.scan.node_slot_visits += sweep.len() as u64;
        self.scan.scratch.sweep_bitmap_inited += sweep.len() as u64;
        self.scan.sweep_edge_checks += self.edges.len() as u64;
        let is_marked = |slot: u32| sweep.get(slot as usize).is_some_and(|found| found.marked);
        let typed = |slot: u32, generation: u32, kind: HandleKind| {
            sweep.get(slot as usize).is_some_and(|found| {
                found.marked && found.kind == kind && found.generation == generation
            })
        };
        // Strong endpoints of a marked owner were already marked. For weak
        // edges, either dead object endpoint invalidates the whole entry.
        // Closure and cell targets, and object keys of either stored kind,
        // are checked in this same retain.
        self.edges.retain(|edge| {
            is_marked(edge.from)
                && match &edge.value {
                    EdgeValue::Table { slot, .. } => is_marked(*slot),
                    EdgeValue::Scalar(_) => true,
                    EdgeValue::Closure { slot, generation } => {
                        typed(*slot, *generation, HandleKind::Closure)
                    }
                    EdgeValue::Upvalue { slot, generation } => {
                        typed(*slot, *generation, HandleKind::Upvalue)
                    }
                }
                && match &edge.key {
                    EdgeKey::Field(_) | EdgeKey::Utf8(_) | EdgeKey::Number(_) => true,
                    EdgeKey::Object { slot, .. } => is_marked(*slot),
                }
        });
    }

    fn mark_slot(&mut self, index: u32, work: &mut Vec<u32>) {
        if let SlotState::Live { marked } = &mut self.slots[index as usize].state
            && !*marked
        {
            *marked = true;
            work.push(index);
            self.scan.scratch.work_pushed += 1;
        }
    }

    fn release_slot(&mut self, index: u32) {
        if self.slots[index as usize].metatable.take().is_some() {
            self.metatable_edges = self.metatable_edges.saturating_sub(1);
        }
        let slot = &mut self.slots[index as usize];
        slot.token = None;
        slot.carrier = None;
        if slot.generation >= self.limits.max_generation {
            slot.state = SlotState::Retired;
            return;
        }
        slot.generation = slot.generation.saturating_add(1);
        slot.state = SlotState::Free;
        self.free_list.push(index);
    }

    fn remove_edges_touching(&mut self, slot: u32) {
        self.edges.retain(|edge| {
            edge.from != slot
                && edge.value.object_slot().is_none_or(|target| target != slot)
                && !edge_key_is_slot(&edge.key, slot)
        });
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
        }
    }

    fn clear_dead_roots(&mut self) -> u32 {
        let mut live = 0u32;
        let len = self.roots.len();
        self.scan.root_slot_visits += len as u64;
        for index in 0..len {
            let Some(rooted) = self.roots[index] else {
                continue;
            };
            self.scan.node_slot_visits += 1;
            if self.rooted_alive(rooted) {
                live += 1;
            } else {
                self.roots[index] = None;
            }
        }
        live
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
        self.peak_edges = self.peak_edges.max(self.edges.len() as u32);
    }
}

fn edge_key_is_slot(key: &EdgeKey, slot: u32) -> bool {
    matches!(key, EdgeKey::Object { slot: key_slot, .. } if *key_slot == slot)
}
