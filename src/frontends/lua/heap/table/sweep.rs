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
//! Production callers drain this session before they return a report. A
//! partial pulse is only visible to the private test that still holds the
//! borrow. Guest `next` and raw reads belong after `drain`.

use super::{
    CollectReport, EdgeKey, EdgeValue, HandleKind, LuaHeap, MarkSnapshot, ScanMeter, SlotState,
};

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
