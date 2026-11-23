//! Pausable sweep after a finished mark. The session holds the heap borrow.
//!
//! A pulse spends one unit before it runs: one selection slot, one snapshot
//! slot when any live node is unmarked, one owner head, one occupied edge
//! (including a gap), or one node release or root slot. A zero budget does
//! not read the graph and does not mutate it. Drop does not sweep.
//!
//! The snapshot is one appended slot record per unit. It is not allocated or
//! zero-filled as a whole vector. `Vec` capacity above that count is not a
//! scan unit and is not a per-pulse wall-clock bound. Weak mode comes from
//! the owner's metatable and the mark snapshot when that owner is opened.
//! That read is part of the head unit. It does not rescan a mode string and
//! does not add `weak_mode_lookups`.
//!
//! Production callers drain this session before they return a report. `pulse`
//! still rewrites one owner unit at a time. The private observation entry does
//! not return that partial graph: before Owners the original edges are
//! readable, and the same Owners walker publishes once when it finishes,
//! still short of Release. That window's heads and edges are the walk it
//! actually did. They are not a `pulse(1)` bound. Guest lookup on the entry
//! rejects a handle already judged dead. A sweep with no dead slot never
//! enters Owners and does not invent a publish window.

use super::{
    CollectReport, EdgeKey, EdgeValue, GuestClass, HandleKind, HeapError, LuaHeap, MarkSnapshot,
    ScanMeter, SlotState, TableRef,
};
use vm::Value;

pub(super) struct SweepSession<'a> {
    heap: &'a mut LuaHeap,
    modes: MarkSnapshot,
    phase: SweepPhase,
    index: usize,
    dead: Vec<u32>,
    snapshot: Vec<SweepSlot>,
    owner: usize,
    owner_open: bool,
    owner_weak: bool,
    prev: Option<u32>,
    cursor: Option<u32>,
    marked_nodes: u32,
    live_roots: u32,
    report: Option<CollectReport>,
    pulse_spent: SweepUnits,
}

struct SweepSlot {
    marked: bool,
    kind: HandleKind,
    generation: u32,
}

enum SweepPhase {
    Select,
    Snapshot,
    Owners,
    Release,
    Roots,
    Done,
}

enum SweepAction {
    Keep,
    Gap,
    Drop,
}

pub(super) struct SweepView {
    pub(super) finished: bool,
    pub(super) meter: ScanMeter,
    pub(super) spent: SweepUnits,
}

/// Units charged by one pulse. Each field is one kind of unit. Node-slot
/// visits overlap selection, snapshot, release, and root liveness, so they
/// are not added into [`SweepUnits::total`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct SweepUnits {
    pub(super) selection: u64,
    pub(super) snapshot: u64,
    pub(super) heads: u64,
    pub(super) edges: u64,
    pub(super) releases: u64,
    pub(super) roots: u64,
}

impl SweepUnits {
    pub(super) fn total(self) -> u64 {
        self.selection + self.snapshot + self.heads + self.edges + self.releases + self.roots
    }
}

impl<'a> SweepSession<'a> {
    pub(super) fn start(heap: &'a mut LuaHeap, modes: MarkSnapshot) -> Self {
        Self {
            heap,
            modes,
            phase: SweepPhase::Select,
            index: 0,
            dead: Vec::new(),
            snapshot: Vec::new(),
            owner: 0,
            owner_open: false,
            owner_weak: false,
            prev: None,
            cursor: None,
            marked_nodes: 0,
            live_roots: 0,
            report: None,
            pulse_spent: SweepUnits::default(),
        }
    }

    pub(super) fn meter(&self) -> ScanMeter {
        self.heap.scan
    }

    pub(super) fn stats(&self) -> super::HeapStats {
        self.heap.stats()
    }

    pub(super) fn pulse(&mut self, budget: u64) -> SweepView {
        if budget == 0 || matches!(self.phase, SweepPhase::Done) {
            return self.view(SweepUnits::default());
        }
        self.pulse_spent = SweepUnits::default();
        let mut left = budget;
        let mut spins = 0u32;
        while left > 0 && !matches!(self.phase, SweepPhase::Done) {
            if self.step() {
                left -= 1;
                spins = 0;
            } else {
                spins += 1;
                debug_assert!(spins <= 4, "sweep phase did not advance");
            }
        }
        self.view(self.pulse_spent)
    }

    pub(super) fn drain(&mut self) {
        while !matches!(self.phase, SweepPhase::Done) {
            self.pulse(64);
        }
    }

    /// The report built when the sweep finished. A second call has nothing
    /// left: completion is recorded once.
    pub(super) fn report(&mut self) -> CollectReport {
        self.report.take().expect("sweep report is issued once")
    }

    fn view(&self, spent: SweepUnits) -> SweepView {
        SweepView {
            finished: matches!(self.phase, SweepPhase::Done),
            meter: self.heap.scan,
            spent,
        }
    }

    fn step(&mut self) -> bool {
        match self.phase {
            SweepPhase::Select => self.step_select(),
            SweepPhase::Snapshot => self.step_snapshot(),
            SweepPhase::Owners => self.step_owners(),
            SweepPhase::Release => self.step_release(),
            SweepPhase::Roots => self.step_root(),
            SweepPhase::Done => false,
        }
    }

    fn step_select(&mut self) -> bool {
        if self.index >= self.heap.slots.len() {
            self.index = 0;
            self.phase = if self.dead.is_empty() {
                SweepPhase::Roots
            } else {
                SweepPhase::Snapshot
            };
            return false;
        }
        let index = self.index;
        self.index += 1;
        self.heap.scan.node_slot_visits += 1;
        self.pulse_spent.selection += 1;
        match self.heap.slots[index].state {
            SlotState::Live { marked: false } => {
                self.dead.push(index as u32);
                self.heap.scan.scratch.dead_slots_pushed += 1;
            }
            SlotState::Live { marked: true } => self.marked_nodes += 1,
            SlotState::Free | SlotState::Retired => {}
        }
        true
    }

    fn step_snapshot(&mut self) -> bool {
        if self.index >= self.heap.slots.len() {
            self.index = 0;
            self.owner = 0;
            self.phase = SweepPhase::Owners;
            return false;
        }
        let index = self.index;
        self.index += 1;
        let slot = &self.heap.slots[index];
        self.snapshot.push(SweepSlot {
            marked: matches!(slot.state, SlotState::Live { marked: true }),
            kind: slot.kind,
            generation: slot.generation,
        });
        self.heap.scan.node_slot_visits += 1;
        self.heap.scan.scratch.sweep_bitmap_inited += 1;
        self.pulse_spent.snapshot += 1;
        true
    }

    fn step_owners(&mut self) -> bool {
        if self.cursor.is_some() {
            return self.step_edge();
        }
        if self.owner_open {
            self.owner_open = false;
            self.owner += 1;
            return false;
        }
        if self.owner >= self.heap.slots.len() {
            self.index = 0;
            self.phase = SweepPhase::Release;
            return false;
        }
        self.open_owner()
    }

    fn open_owner(&mut self) -> bool {
        let owner = self.owner;
        self.heap.scan.owner_head_checks += 1;
        self.owner_weak = self.weak_values(owner);
        self.prev = None;
        self.cursor = self.heap.slots[owner].edge_head;
        self.owner_open = true;
        self.pulse_spent.heads += 1;
        true
    }

    fn weak_values(&self, owner: usize) -> bool {
        let Some(modes) = self.modes.modes.as_ref() else {
            return false;
        };
        let found = &self.snapshot[owner];
        if !found.marked || found.kind != HandleKind::Table {
            return false;
        }
        let Some((mt_slot, mt_generation)) = self.heap.slots[owner].metatable else {
            return false;
        };
        let Some(mt) = self.snapshot.get(mt_slot as usize) else {
            return false;
        };
        if !mt.marked || mt.kind != HandleKind::Table || mt.generation != mt_generation {
            return false;
        }
        modes.get(mt_slot as usize).is_some_and(|mode| mode.values)
    }

    fn step_edge(&mut self) -> bool {
        let index = self.cursor.expect("edge step has a cursor");
        let owner = self.owner as u32;
        let next = self.heap.edge_slots[index as usize].next;
        let action = self.decide(owner, index);
        match action {
            SweepAction::Keep => self.prev = Some(index),
            SweepAction::Gap => {
                self.heap.rewrite_gap(index);
                self.prev = Some(index);
            }
            SweepAction::Drop => self.heap.unlink_at(owner, self.prev, index),
        }
        self.cursor = next;
        self.heap.scan.sweep_edge_checks += 1;
        self.pulse_spent.edges += 1;
        true
    }

    fn decide(&self, owner: u32, index: u32) -> SweepAction {
        let edge = self.heap.edge_slots[index as usize]
            .edge
            .as_ref()
            .expect("owner chain holds an occupied record");
        let owner_marked = self.is_marked(owner);
        let weak = self.owner_weak;
        let value_dead = match &edge.value {
            EdgeValue::Scalar(_) => false,
            EdgeValue::Table { slot, .. } => !self.is_marked(*slot),
            EdgeValue::Closure { slot, generation } => {
                !self.typed(*slot, *generation, HandleKind::Closure)
            }
            EdgeValue::Upvalue { slot, generation } => {
                !self.typed(*slot, *generation, HandleKind::Upvalue)
            }
        };
        let make_gap = owner_marked && weak && value_dead && self.guest_cursor(&edge.key);
        let value_ok = make_gap
            || match &edge.value {
                EdgeValue::Table { slot, .. } => self.is_marked(*slot),
                EdgeValue::Scalar(_) => true,
                EdgeValue::Closure { slot, generation } => {
                    self.typed(*slot, *generation, HandleKind::Closure)
                }
                EdgeValue::Upvalue { slot, generation } => {
                    self.typed(*slot, *generation, HandleKind::Upvalue)
                }
            };
        let key_ok = match &edge.key {
            EdgeKey::Field(_) | EdgeKey::Utf8(_) | EdgeKey::Number(_) => true,
            EdgeKey::Object {
                slot,
                generation,
                kind,
            } if make_gap || edge.value.is_cursor_gap() => self.typed(*slot, *generation, *kind),
            EdgeKey::Object { slot, .. } => self.is_marked(*slot),
        };
        let keep = owner_marked && value_ok && key_ok;
        if !keep {
            SweepAction::Drop
        } else if make_gap && !edge.value.is_cursor_gap() {
            SweepAction::Gap
        } else {
            SweepAction::Keep
        }
    }

    fn step_release(&mut self) -> bool {
        if self.index >= self.dead.len() {
            self.index = 0;
            self.phase = SweepPhase::Roots;
            return false;
        }
        let slot = self.dead[self.index];
        self.index += 1;
        self.heap.scan.node_slot_visits += 1;
        self.heap.release_slot(slot);
        self.pulse_spent.releases += 1;
        true
    }

    fn step_root(&mut self) -> bool {
        if self.index >= self.heap.roots.len() {
            self.finish();
            return false;
        }
        let index = self.index;
        self.index += 1;
        self.heap.scan.root_slot_visits += 1;
        self.pulse_spent.roots += 1;
        let Some(rooted) = self.heap.roots[index] else {
            return true;
        };
        self.heap.scan.node_slot_visits += 1;
        if self.heap.rooted_alive(rooted) {
            self.live_roots += 1;
        } else {
            self.heap.roots[index] = None;
        }
        true
    }

    fn finish(&mut self) {
        if self.report.is_some() {
            self.phase = SweepPhase::Done;
            return;
        }
        self.heap.collections = self.heap.collections.saturating_add(1);
        self.heap.last_marked = self.marked_nodes;
        self.report = Some(CollectReport {
            live_nodes: self.marked_nodes,
            marked_nodes: self.marked_nodes,
            live_roots: self.live_roots,
        });
        self.phase = SweepPhase::Done;
    }

    fn is_marked(&self, slot: u32) -> bool {
        self.snapshot
            .get(slot as usize)
            .is_some_and(|found| found.marked)
    }

    fn typed(&self, slot: u32, generation: u32, kind: HandleKind) -> bool {
        self.snapshot.get(slot as usize).is_some_and(|found| {
            found.marked && found.kind == kind && found.generation == generation
        })
    }

    /// Reads the graph only outside the Owners walk.
    ///
    /// Select and Snapshot still hold the original edges. The first Release
    /// index, after Owners, is the published graph. Any other phase, including
    /// a unit inside Owners, returns nothing.
    fn weak_view(&self) -> Option<WeakView<'_>> {
        let publish = match self.phase {
            SweepPhase::Select | SweepPhase::Snapshot => WeakPublish::Original,
            SweepPhase::Release if self.index == 0 && !self.dead.is_empty() => {
                WeakPublish::Published
            }
            _ => return None,
        };
        Some(self.boundary(publish))
    }

    /// Runs the current Owners walker to its end and returns that one boundary.
    ///
    /// The caller must still be before any owner unit. A walk that has already
    /// rewritten an edge is not restarted and is not published. The transition
    /// into Owners is not charged. Release does not start.
    fn publish_owners(&mut self) -> Option<PublishWindow<'_>> {
        if matches!(self.phase, SweepPhase::Snapshot) && self.index >= self.heap.slots.len() {
            let charged = self.step();
            debug_assert!(!charged);
        }
        if !matches!(self.phase, SweepPhase::Owners) || !self.at_owner_start() {
            return None;
        }
        let mut spent = SweepUnits::default();
        let mut spins = 0u32;
        while matches!(self.phase, SweepPhase::Owners) {
            self.pulse_spent = SweepUnits::default();
            if self.step() {
                spent.heads += self.pulse_spent.heads;
                spent.edges += self.pulse_spent.edges;
                spent.selection += self.pulse_spent.selection;
                spent.snapshot += self.pulse_spent.snapshot;
                spent.releases += self.pulse_spent.releases;
                spent.roots += self.pulse_spent.roots;
                spins = 0;
            } else {
                spins += 1;
                debug_assert!(spins <= 4, "owner transition did not reach Release");
            }
        }
        debug_assert!(matches!(self.phase, SweepPhase::Release));
        debug_assert_eq!(self.index, 0);
        debug_assert!(!self.owner_open);
        debug_assert!(self.cursor.is_none());
        Some(PublishWindow {
            spent,
            view: self.boundary(WeakPublish::Published),
        })
    }

    fn at_owner_start(&self) -> bool {
        self.owner == 0 && !self.owner_open && self.cursor.is_none()
    }

    fn boundary(&self, publish: WeakPublish) -> WeakView<'_> {
        WeakView {
            publish,
            finished: false,
            release_started: false,
            heap: self.heap,
            dead: &self.dead,
            snapshot: &self.snapshot,
        }
    }

    /// Occupancy of one slot. This is not a guest lookup and does not accept
    /// a table handle.
    fn slot_physical(&self, slot: u32) -> Option<(bool, u32)> {
        let stored = self.heap.slots.get(slot as usize)?;
        let live = matches!(stored.state, SlotState::Live { .. });
        Some((live, stored.generation))
    }

    fn guest_cursor(&self, key: &EdgeKey) -> bool {
        match key {
            EdgeKey::Utf8(_) | EdgeKey::Number(_) => true,
            EdgeKey::Field(_) => false,
            EdgeKey::Object {
                slot,
                generation,
                kind: HandleKind::Table,
            } => self.typed(*slot, *generation, HandleKind::Table),
            EdgeKey::Object { .. } => false,
        }
    }
}

/// Which exclusive boundary the observation entry is returning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WeakPublish {
    /// Select or Snapshot. No owner edge has been rewritten.
    Original,
    /// Owners has finished. Release has not taken a slot.
    Published,
}

/// Actual heads and edges of one exclusive Owners walk, plus the graph it left.
struct PublishWindow<'a> {
    spent: SweepUnits,
    view: WeakView<'a>,
}

/// Read-only graph at one boundary. A judged-dead handle is not a target.
struct WeakView<'a> {
    publish: WeakPublish,
    finished: bool,
    release_started: bool,
    heap: &'a LuaHeap,
    dead: &'a [u32],
    snapshot: &'a [SweepSlot],
}

#[derive(Debug, PartialEq, Eq)]
enum ObserveError {
    JudgedDead,
    Heap(HeapError),
}

impl WeakView<'_> {
    fn publish(&self) -> WeakPublish {
        self.publish
    }

    fn finished(&self) -> bool {
        self.finished
    }

    fn release_started(&self) -> bool {
        self.release_started
    }

    fn occupying_dead(&self) -> u32 {
        self.dead.len() as u32
    }

    fn dead_still_physical(&self) -> u32 {
        self.dead
            .iter()
            .filter(|&&slot| self.still_physical(slot))
            .count() as u32
    }

    fn named(&self, table: TableRef, name: &str) -> Result<Option<TableRef>, ObserveError> {
        self.reject(table)?;
        match self.heap.named_edge(table, name) {
            Ok(found) => Ok(found),
            Err(error) => Err(ObserveError::Heap(error)),
        }
    }

    fn number(&self, table: TableRef, key: f64) -> Result<Value, ObserveError> {
        self.reject(table)?;
        match self.heap.number_value(table, key) {
            Ok(found) => Ok(found),
            Err(error) => Err(ObserveError::Heap(error)),
        }
    }

    fn object(&self, table: TableRef, key: TableRef) -> Result<Option<TableRef>, ObserveError> {
        self.reject(table)?;
        self.reject(key)?;
        match self.heap.object_edge(table, key) {
            Ok(found) => Ok(found),
            Err(error) => Err(ObserveError::Heap(error)),
        }
    }

    fn raw_sequence_len(&self, table: TableRef) -> Result<f64, ObserveError> {
        self.reject(table)?;
        match self.heap.raw_sequence_len(table) {
            Ok(len) => Ok(len),
            Err(error) => Err(ObserveError::Heap(error)),
        }
    }

    fn next(
        &self,
        table: TableRef,
        cursor: &Value,
    ) -> Result<Option<(Value, Value)>, ObserveError> {
        self.reject(table)?;
        if let GuestClass::Table(key) = self.heap.guest_class(cursor) {
            self.reject(key)?;
        }
        match self.heap.next_guest(table, cursor) {
            Ok(found) => Ok(found),
            Err(error) => Err(ObserveError::Heap(error)),
        }
    }

    fn alive(&self, table: TableRef) -> Result<bool, ObserveError> {
        self.reject(table)?;
        Ok(self.heap.alive(table))
    }

    fn reject(&self, table: TableRef) -> Result<(), ObserveError> {
        if self.dead.iter().any(|&slot| slot == table.slot()) {
            return Err(ObserveError::JudgedDead);
        }
        Ok(())
    }

    fn still_physical(&self, slot: u32) -> bool {
        let Some(stored) = self.heap.slots.get(slot as usize) else {
            return false;
        };
        let Some(snap) = self.snapshot.get(slot as usize) else {
            return false;
        };
        matches!(stored.state, SlotState::Live { .. }) && stored.generation == snap.generation
    }
}

#[cfg(test)]
#[path = "../../../../../tests/luajit_heap_runtime/weak_publish.rs"]
mod weak_publish;
