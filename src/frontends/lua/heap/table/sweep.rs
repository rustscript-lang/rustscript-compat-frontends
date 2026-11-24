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

/// Release or root scan saved after publish. No snapshot, modes, or owner cursor.
#[derive(Debug)]
pub(super) enum TailPhase {
    Release,
    Roots,
    Done,
}

/// Owned continuation of one published sweep. Release, Roots, and the
/// late-node count recorded after publish.
#[derive(Debug)]
pub(super) struct SweepTail {
    phase: TailPhase,
    index: usize,
    dead: Vec<u32>,
    marked_nodes: u32,
    live_roots: u32,
    late_nodes: u32,
}

pub(super) enum TailStep {
    Paused(SweepTail),
    Completed(CollectReport),
}

impl SweepTail {
    /// Spends `budget` on the shared release/root machine. Zero does not read the heap.
    pub(super) fn drive(self, heap: &mut LuaHeap, budget: u64) -> TailStep {
        let mut phase = self.phase;
        let mut index = self.index;
        let dead = self.dead;
        let marked_nodes = self.marked_nodes;
        let late_nodes = self.late_nodes;
        let mut live_roots = self.live_roots;
        let mut report = None;
        let mut releases = 0u64;
        let mut roots = 0u64;
        let mut left = budget;
        let mut spins = 0u32;
        while left > 0 && !matches!(phase, TailPhase::Done) {
            let mut cursor = TailCursor {
                phase: &mut phase,
                index: &mut index,
                dead: &dead,
                marked_nodes,
                late_nodes,
                live_roots: &mut live_roots,
                report: &mut report,
                releases: &mut releases,
                roots: &mut roots,
            };
            if step_tail(heap, &mut cursor) {
                left -= 1;
                spins = 0;
            } else {
                spins += 1;
                debug_assert!(spins <= 4, "uncharged tail transition did not settle");
            }
        }
        if matches!(phase, TailPhase::Done) {
            return TailStep::Completed(report.take().expect("a finished tail records one report"));
        }
        TailStep::Paused(SweepTail {
            phase,
            index,
            dead,
            marked_nodes,
            live_roots,
            late_nodes,
        })
    }

    pub(super) fn note_late(&mut self) {
        self.late_nodes = self.late_nodes.saturating_add(1);
    }

    /// Adjusts the saved cursor for one retiring top frame.
    ///
    /// Release keeps the dead cursor and the accumulated counts. Roots counts
    /// `Some` tags in the scanned overlap of this frame, including duplicates,
    /// then clamps the cursor downward. A cursor that has not entered the frame
    /// is not sliced and is not moved forward. This does not read nodes or the
    /// scan meter.
    pub(super) fn retire_frame_prefix(
        &mut self,
        roots: &[Option<super::Rooted>],
        start: usize,
        width: usize,
    ) -> super::FrameRetirement {
        debug_assert!(
            matches!(self.phase, TailPhase::Release | TailPhase::Roots),
            "a saved tail is release or roots"
        );
        if matches!(self.phase, TailPhase::Release) {
            return super::FrameRetirement {
                release_phase: true,
                prefix_slots: 0,
                dropped_live: 0,
            };
        }
        let index = self.index;
        if index <= start {
            self.index = index.min(start);
            return super::FrameRetirement {
                release_phase: false,
                prefix_slots: 0,
                dropped_live: 0,
            };
        }
        let frame_end = start.saturating_add(width);
        let slice_end = index.min(frame_end).min(roots.len());
        let overlap = roots.get(start..slice_end).unwrap_or(&[]);
        let dropped_live = overlap.iter().filter(|slot| slot.is_some()).count() as u32;
        self.live_roots = self.live_roots.saturating_sub(dropped_live);
        self.index = index.min(start);
        super::FrameRetirement {
            release_phase: false,
            prefix_slots: overlap.len() as u32,
            dropped_live,
        }
    }
}

/// One release, root, or uncharged transition. Callers share this with [`SweepSession`].
struct TailCursor<'a> {
    phase: &'a mut TailPhase,
    index: &'a mut usize,
    dead: &'a [u32],
    marked_nodes: u32,
    late_nodes: u32,
    live_roots: &'a mut u32,
    report: &'a mut Option<CollectReport>,
    releases: &'a mut u64,
    roots: &'a mut u64,
}

fn step_tail(heap: &mut LuaHeap, cursor: &mut TailCursor<'_>) -> bool {
    match *cursor.phase {
        TailPhase::Release => {
            if *cursor.index >= cursor.dead.len() {
                *cursor.index = 0;
                *cursor.phase = TailPhase::Roots;
                return false;
            }
            let slot = cursor.dead[*cursor.index];
            *cursor.index += 1;
            heap.scan.node_slot_visits += 1;
            heap.release_slot(slot);
            *cursor.releases += 1;
            true
        }
        TailPhase::Roots => {
            if *cursor.index >= heap.roots.len() {
                if cursor.report.is_none() {
                    heap.collections = heap.collections.saturating_add(1);
                    heap.last_marked = cursor.marked_nodes;
                    *cursor.report = Some(CollectReport {
                        live_nodes: cursor.marked_nodes.saturating_add(cursor.late_nodes),
                        marked_nodes: cursor.marked_nodes,
                        live_roots: *cursor.live_roots,
                    });
                }
                *cursor.phase = TailPhase::Done;
                return false;
            }
            let root_index = *cursor.index;
            *cursor.index += 1;
            heap.scan.root_slot_visits += 1;
            *cursor.roots += 1;
            let Some(rooted) = heap.roots[root_index] else {
                return true;
            };
            heap.scan.node_slot_visits += 1;
            if heap.rooted_alive(rooted) {
                *cursor.live_roots += 1;
            } else {
                heap.roots[root_index] = None;
            }
            true
        }
        TailPhase::Done => false,
    }
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
        debug_assert!(
            heap.sweep_tail.is_none(),
            "a saved sweep tail and a new sweep cannot coexist"
        );
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

    /// Walk Select and Snapshot by their real units, then stop before the
    /// first owner unit. An empty dead set is already Roots. This does not
    /// call `pulse`.
    pub(super) fn reach_before_owners(&mut self) -> bool {
        loop {
            match self.phase {
                SweepPhase::Select | SweepPhase::Snapshot => {
                    let _ = self.step();
                }
                SweepPhase::Owners => {
                    debug_assert!(self.at_owner_start());
                    return true;
                }
                SweepPhase::Roots => return false,
                SweepPhase::Release | SweepPhase::Done => {
                    debug_assert!(false, "release started before the owner boundary");
                    return false;
                }
            }
        }
    }

    /// Publish every owner in this same session, lend one read-only view, and
    /// return before Release. No dead slot means no view and no callback.
    pub(super) fn observe_before_release<R>(
        &mut self,
        observe: impl for<'b> FnOnce(&'b PublishedRead<'b>) -> R,
    ) -> Option<R> {
        if !self.reach_before_owners() {
            return None;
        }
        let seen = {
            let window = self
                .publish_owners()
                .expect("the owner boundary is the first owner unit");
            let read = window.published();
            observe(&read)
        };
        Some(seen)
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
            SweepPhase::Release | SweepPhase::Roots | SweepPhase::Done => self.step_saved_tail(),
        }
    }

    fn step_saved_tail(&mut self) -> bool {
        let mut phase = match self.phase {
            SweepPhase::Release => TailPhase::Release,
            SweepPhase::Roots => TailPhase::Roots,
            SweepPhase::Done => TailPhase::Done,
            SweepPhase::Select | SweepPhase::Snapshot | SweepPhase::Owners => {
                unreachable!("saved tail phases only")
            }
        };
        let mut cursor = TailCursor {
            phase: &mut phase,
            index: &mut self.index,
            dead: &self.dead,
            marked_nodes: self.marked_nodes,
            late_nodes: 0,
            live_roots: &mut self.live_roots,
            report: &mut self.report,
            releases: &mut self.pulse_spent.releases,
            roots: &mut self.pulse_spent.roots,
        };
        let charged = step_tail(self.heap, &mut cursor);
        self.phase = match phase {
            TailPhase::Release => SweepPhase::Release,
            TailPhase::Roots => SweepPhase::Roots,
            TailPhase::Done => SweepPhase::Done,
        };
        charged
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

    /// Same owner walk as [`Self::publish_owners`], returning only the charged
    /// head and edge counts. The window borrow ends before this returns.
    pub(super) fn publish_counts(&mut self) -> Option<(u64, u64)> {
        let window = self.publish_owners()?;
        Some((window.spent.heads, window.spent.edges))
    }

    /// Drops the snapshot, modes, and owner cursor. The caller stores this tail.
    pub(super) fn into_release_tail(self) -> SweepTail {
        debug_assert!(matches!(self.phase, SweepPhase::Release));
        debug_assert_eq!(self.index, 0);
        debug_assert!(self.report.is_none());
        SweepTail {
            phase: TailPhase::Release,
            index: 0,
            dead: self.dead,
            marked_nodes: self.marked_nodes,
            live_roots: self.live_roots,
            late_nodes: 0,
        }
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

impl PublishWindow<'_> {
    fn published(&self) -> PublishedRead<'_> {
        PublishedRead {
            view: WeakView {
                publish: self.view.publish,
                finished: self.view.finished,
                release_started: self.view.release_started,
                heap: self.view.heap,
                dead: self.view.dead,
                snapshot: self.view.snapshot,
            },
            heads: self.spent.heads,
            edges: self.spent.edges,
        }
    }
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

/// Why a published-boundary read was refused. A judged-dead handle is not a
/// lookup target. This error does not carry a slot index.
#[derive(Debug)]
pub enum ObserveFault {
    JudgedDead,
    Heap(HeapError),
}

/// Values at the published boundary. The dead-slot list, snapshot, and heap
/// stay behind this view.
pub struct PublishedRead<'a> {
    view: WeakView<'a>,
    heads: u64,
    edges: u64,
}

impl PublishedRead<'_> {
    pub fn named(&self, table: TableRef, name: &str) -> Result<Option<TableRef>, ObserveFault> {
        self.view.named(table, name).map_err(Self::fault)
    }

    pub fn number(&self, table: TableRef, key: f64) -> Result<Value, ObserveFault> {
        self.view.number(table, key).map_err(Self::fault)
    }

    pub fn object(&self, table: TableRef, key: TableRef) -> Result<Option<TableRef>, ObserveFault> {
        self.view.object(table, key).map_err(Self::fault)
    }

    pub fn raw_sequence_len(&self, table: TableRef) -> Result<f64, ObserveFault> {
        self.view.raw_sequence_len(table).map_err(Self::fault)
    }

    pub fn next(
        &self,
        table: TableRef,
        cursor: &Value,
    ) -> Result<Option<(Value, Value)>, ObserveFault> {
        self.view.next(table, cursor).map_err(Self::fault)
    }

    pub fn alive(&self, table: TableRef) -> Result<bool, ObserveFault> {
        self.view.alive(table).map_err(Self::fault)
    }

    pub fn still_physical(&self, table: TableRef) -> bool {
        let slot = table.slot();
        self.view.still_physical(slot)
            && self
                .view
                .snapshot
                .get(slot as usize)
                .is_some_and(|snap| snap.generation == table.generation())
    }

    pub fn dead_still_physical(&self) -> u32 {
        self.view.dead_still_physical()
    }

    pub fn occupying_dead(&self) -> u32 {
        self.view.occupying_dead()
    }

    pub fn finished(&self) -> bool {
        self.view.finished()
    }

    pub fn release_started(&self) -> bool {
        self.view.release_started()
    }

    pub fn window_heads(&self) -> u64 {
        self.heads
    }

    pub fn window_edges(&self) -> u64 {
        self.edges
    }

    fn fault(error: ObserveError) -> ObserveFault {
        match error {
            ObserveError::JudgedDead => ObserveFault::JudgedDead,
            ObserveError::Heap(error) => ObserveFault::Heap(error),
        }
    }
}

#[cfg(test)]
#[path = "../../../../../tests/luajit_heap_runtime/weak_publish.rs"]
mod weak_publish;
