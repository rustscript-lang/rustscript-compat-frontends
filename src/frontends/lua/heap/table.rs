//! Non-moving mark/sweep table storage, explicit root frames, and hard caps.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use vm::HostContextError;

use super::handle::{HandleKind, TableRef};

static NEXT_DOMAIN: AtomicU64 = AtomicU64::new(1);

/// Hard caps. Edges and root slots are counted separately so fields cannot
/// grow past the edge cap by avoiding the node cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapLimits {
    /// Slot-vector length, including free and retired slots.
    pub nodes: u32,
    /// Simultaneous explicit root slots.
    pub roots: u32,
    /// Live reference-edge records.
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
    /// The scope resource table cannot take another token.
    PublishRejected,
    /// The resource handle does not fit in a positive VM integer.
    CarrierOutOfRange,
    /// This scope already has a heap.
    AlreadyInstalled,
    /// Execution-scope or resource-table failure. The node was rolled back
    /// when the failure happened after a reserve.
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
            Self::PublishRejected => write!(formatter, "lua table publish rejected"),
            Self::CarrierOutOfRange => write!(formatter, "lua resource carrier out of range"),
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
    /// Reference edges currently stored.
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

#[derive(Clone, Copy, Debug)]
enum SlotState {
    Free,
    Live { marked: bool },
    Retired,
}

struct Slot {
    generation: u32,
    state: SlotState,
}

struct Edge {
    from: u32,
    key: u32,
    to_slot: u32,
    to_generation: u32,
}

struct Frame {
    index: u32,
    generation: u32,
    start: u32,
    width: u32,
}

/// One scope's table nodes, edges, roots, and scan state.
pub struct LuaHeap {
    domain: u64,
    limits: HeapLimits,
    slots: Vec<Slot>,
    free_list: Vec<u32>,
    edges: Vec<Edge>,
    roots: Vec<Option<TableRef>>,
    frames: Vec<Frame>,
    next_frame_generation: u32,
    collections: u32,
    last_marked: u32,
    peak_nodes: u32,
    peak_roots: u32,
    peak_edges: u32,
}

impl LuaHeap {
    /// Creates an empty heap. Slot zero is ordinary storage; handles start at
    /// generation 1 so generation 0 never denotes a live node.
    pub fn new(limits: HeapLimits) -> Self {
        Self {
            domain: NEXT_DOMAIN.fetch_add(1, Ordering::Relaxed),
            limits,
            slots: Vec::new(),
            free_list: Vec::new(),
            edges: Vec::new(),
            roots: Vec::new(),
            frames: Vec::new(),
            next_frame_generation: 1,
            collections: 0,
            last_marked: 0,
            peak_nodes: 0,
            peak_roots: 0,
            peak_edges: 0,
        }
    }

    /// Reserves a live, unrooted table. Failure leaves occupancy unchanged.
    pub fn alloc(&mut self) -> Result<TableRef, HeapError> {
        if let Some(index) = self.free_list.pop() {
            let slot = &mut self.slots[index as usize];
            if !matches!(slot.state, SlotState::Free) {
                return Err(HeapError::StaleHandle);
            }
            slot.state = SlotState::Live { marked: false };
            return Ok(TableRef::table(self.domain, index, slot.generation));
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
            state: SlotState::Live { marked: false },
        });
        self.note_node_peak();
        Ok(TableRef::table(self.domain, index, 1))
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

    /// Writes a reference edge. An existing key is overwritten and does not
    /// consume another edge slot. A new key at the cap fails without insert.
    pub fn set_edge(&mut self, from: TableRef, key: u32, to: TableRef) -> Result<(), HeapError> {
        self.require_live(from)?;
        self.require_live(to)?;
        if let Some(edge) = self
            .edges
            .iter_mut()
            .find(|edge| edge.from == from.slot() && edge.key == key)
        {
            edge.to_slot = to.slot();
            edge.to_generation = to.generation();
            return Ok(());
        }
        if self.edges.len() >= self.limits.edges as usize {
            return Err(HeapError::EdgeCapacity);
        }
        self.edges.push(Edge {
            from: from.slot(),
            key,
            to_slot: to.slot(),
            to_generation: to.generation(),
        });
        self.note_edge_peak();
        Ok(())
    }

    /// Reads a reference edge through either alias of `from`.
    pub fn edge(&self, from: TableRef, key: u32) -> Result<Option<TableRef>, HeapError> {
        self.require_live(from)?;
        Ok(self.edges.iter().find_map(|edge| {
            if edge.from == from.slot() && edge.key == key {
                Some(TableRef::table(self.domain, edge.to_slot, edge.to_generation))
            } else {
                None
            }
        }))
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

    /// Stores `handle` in an open frame. The frame does not have to be the
    /// top frame; a stale id is refused.
    pub fn write_root(
        &mut self,
        frame: RootFrameId,
        slot: u32,
        handle: TableRef,
    ) -> Result<(), HeapError> {
        self.require_live(handle)?;
        let found = self.frame(frame)?;
        if slot >= found.width {
            return Err(HeapError::StaleFrame);
        }
        let index = found.start as usize + slot as usize;
        self.roots[index] = Some(handle);
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

    /// Non-moving mark from explicit roots, then sweep unmarked nodes.
    /// Freed slots are reused with a bumped generation. A slot whose next
    /// generation would pass `max_generation` is retired instead.
    pub fn collect(&mut self) -> CollectReport {
        for slot in &mut self.slots {
            if let SlotState::Live { marked } = &mut slot.state {
                *marked = false;
            }
        }
        let mut work = Vec::new();
        let rooted: Vec<TableRef> = self.roots.iter().copied().flatten().collect();
        for handle in rooted {
            if self.alive(handle) {
                self.mark_slot(handle.slot(), &mut work);
            }
        }
        while let Some(slot) = work.pop() {
            let targets: Vec<(u32, u32)> = self
                .edges
                .iter()
                .filter(|edge| edge.from == slot)
                .map(|edge| (edge.to_slot, edge.to_generation))
                .collect();
            for (to_slot, to_generation) in targets {
                if self.slot_live(to_slot, to_generation) {
                    self.mark_slot(to_slot, &mut work);
                }
            }
        }
        let swept: Vec<u32> = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| match slot.state {
                SlotState::Live { marked: false } => Some(index as u32),
                _ => None,
            })
            .collect();
        let marked_nodes = self
            .slots
            .iter()
            .filter(|slot| matches!(slot.state, SlotState::Live { marked: true }))
            .count() as u32;
        for index in swept {
            self.remove_edges_touching(index);
            self.release_slot(index);
        }
        self.clear_dead_roots();
        self.collections = self.collections.saturating_add(1);
        self.last_marked = marked_nodes;
        CollectReport {
            live_nodes: self.live_nodes(),
            marked_nodes,
            live_roots: self.live_roots(),
        }
    }

    /// Reports whether `handle` names a live table in this heap.
    pub fn alive(&self, handle: TableRef) -> bool {
        self.require_live(handle).is_ok()
    }

    /// Current occupancy. Peaks count storage, not only live objects.
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
        if handle.domain() != self.domain {
            return Err(HeapError::WrongDomain);
        }
        if handle.kind() != HandleKind::Table {
            return Err(HeapError::WrongKind);
        }
        if self.slot_live(handle.slot(), handle.generation()) {
            Ok(())
        } else {
            Err(HeapError::StaleHandle)
        }
    }

    fn slot_live(&self, index: u32, generation: u32) -> bool {
        self.slots.get(index as usize).is_some_and(|slot| {
            slot.generation == generation && matches!(slot.state, SlotState::Live { .. })
        })
    }

    fn mark_slot(&mut self, index: u32, work: &mut Vec<u32>) {
        if let SlotState::Live { marked } = &mut self.slots[index as usize].state {
            if !*marked {
                *marked = true;
                work.push(index);
            }
        }
    }

    fn release_slot(&mut self, index: u32) {
        let slot = &mut self.slots[index as usize];
        if slot.generation >= self.limits.max_generation {
            slot.state = SlotState::Retired;
            return;
        }
        slot.generation = slot.generation.saturating_add(1);
        slot.state = SlotState::Free;
        self.free_list.push(index);
    }

    fn remove_edges_touching(&mut self, slot: u32) {
        self.edges
            .retain(|edge| edge.from != slot && edge.to_slot != slot);
    }

    fn clear_roots_to(&mut self, handle: TableRef) {
        for root in &mut self.roots {
            if *root == Some(handle) {
                *root = None;
            }
        }
    }

    fn clear_dead_roots(&mut self) {
        let len = self.roots.len();
        for index in 0..len {
            let Some(handle) = self.roots[index] else {
                continue;
            };
            if !self.alive(handle) {
                self.roots[index] = None;
            }
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
        self.peak_edges = self.peak_edges.max(self.edges.len() as u32);
    }
}
