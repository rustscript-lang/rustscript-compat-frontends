//! Pausable mark. Progress lives in [`MarkState`]. One stepper drains it.
//!
//! A long-borrow session holds the heap until it is dropped or swept. The
//! parked driver stores that same state on the heap between pulses. `park`
//! consumes the driver and leaves an incomplete mark there; `resume` borrows
//! it and does not create another. Drop and cancel still clear the saved
//! state and do not sweep. Done is not left for the caller: the driver sweeps
//! in that same window and returns only a report. A full collect drops any
//! saved mark and starts again from reset.
//!
//! A pulse spends a caller budget on these units, each checked before it runs:
//! one slot reset, one mode-edge open, one mode-string byte (including a NUL),
//! one mode-cache initialization, one root slot, one reached owner's outgoing
//! head, or one outgoing edge or gap. Target checks and the metatable check
//! ride with the edge or the owner head. A zero budget does not read the graph.
//!
//! A safepoint root batch is not one of those units. `accept_root_batch` is
//! open only in Roots or Gray: phase, then root quota, then every typed
//! handle, before any mark. Those prechecks do not increment the scan meter.
//! Success hands each entry to the existing gray queue. It does not write a
//! root slot, open a frame, copy the batch, or spend pulse budget.
//!
//! A positive `pulse_with_root_batch` refuses Done or a missing mark before
//! it reads the batch. It then uses that same quota and handle check and
//! advances with the ordinary stepper. Roots or Gray, including a same-pulse
//! entry from Start, Reset, or Mode, commit once before any root or gray
//! step. A budget that runs out before Roots does not commit. The next call,
//! including one after park and resume, reads only the batch it was given.
//! Zero budget only observes.
//!
//! `ParkedMark::advance_with_root_batch` and the host continuation share
//! [`LuaHeap::advance_saved_mark_with_root_batch`]. That primitive checks the
//! saved mark, Done, the root quota, and each typed handle before it takes
//! the state, then uses this same `pulse_budget`. The host entry does not
//! build a `ParkedMark`: dropping one would discard an unparked mark, and
//! `complete` would sweep without closing tokens.
//!
//! Reset and mode pulses still spend that budget. They do not open mutation.
//! Roots and Gray open an ordinary field delete, an ordinary field write on
//! an owner that is not yet marked, an in-place replacement of an existing
//! non-gap ordinary field on an already marked owner, and a fixed root-slot
//! update through the typed root writers. The shared field replacement
//! applies that barrier, so an ordinary `set_edge` of an existing field uses
//! the same gray queue. New keys, gap resurrection,
//! allocation, rollback, metatable and `__mode` writes, root-frame changes,
//! internal closure edges, the private cell edge, and token edits stay closed.
//! A refused edit returns before it changes the heap.

use super::{
    ClosureRef, CollectReport, EdgeKey, EdgeValue, HandleKind, HeapError, HeapStats, LuaHeap,
    MarkSnapshot, RootFrameId, Rooted, Slot, SlotState, TableRef, UpvalueRef, WeakMode,
};
use vm::Value;

pub(super) struct MarkState {
    phase: MarkPhase,
    gray: Vec<u32>,
    modes: Option<Vec<WeakMode>>,
    index: usize,
    mode_edge: Option<u32>,
    mode_owner: usize,
    byte_at: usize,
    pending_mode: WeakMode,
    init_at: usize,
    pub(super) owner: Option<u32>,
    pub(super) cursor: Option<u32>,
    owner_kind: HandleKind,
    owner_mode: WeakMode,
    /// First node of this mark's rescan queue. The links live on the slots, so
    /// a push or a release-time unlink is constant work and allocates nothing.
    rescan_head: Option<u32>,
    /// Identifies this mark's weak-start and queue tags on slot records. A new
    /// mark gets a new value; a slot tag from an older mark is inert without a
    /// whole-slot clear, including after an abandoned mark.
    rescan_epoch: u64,
    rescan_owner: Option<u32>,
    rescan_cursor: Option<u32>,
    rescan_mode: WeakMode,
    /// Indexed by metatable slot; initialized alongside the weak-mode cache.
    /// Stamps from completed passes are left in place and no longer match.
    discover_marks: Vec<ModeDiscoveryMark>,
    /// Zero is reserved for unarmed slots. Each new batch requires another
    /// distinct cache entry to become strong, so at most `limits.nodes`
    /// batches can start in one mark (no slot reuse in the mutation window).
    discover_batch: u32,
    /// A delete after the cursor moves stamps the next batch instead, so
    /// owners behind the cursor are not missed.
    discover_next: bool,
    discover_at: usize,
    discover_on: bool,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct ModeDiscoveryMark {
    generation: u32,
    batch: u32,
}

enum MarkPhase {
    Start,
    Reset,
    ModeOpen,
    ModeBytes,
    ModeInit,
    Roots,
    Gray,
    Done,
}

pub(super) struct MarkView {
    pub(super) finished: bool,
    pub(super) meter: super::ScanMeter,
}

/// Owner and record the gray scan will charge next. The cursor is the record
/// index, not a field id.
#[derive(Clone, Copy, Debug)]
pub(super) struct PendingScan {
    pub(super) owner: u32,
    pub(super) cursor: u32,
}

/// Operations the pause window does not open. The driver refuses each one
/// before calling the heap method that would apply it.
pub(super) enum PausedOp {
    Alloc,
    Rollback,
    Metatable,
    Mode,
    Root,
    Token,
}

pub(super) struct MarkSession<'a> {
    heap: &'a mut LuaHeap,
}

/// Exclusive pure-heap mark. `park` consumes the driver and can leave an
/// incomplete mark on the heap for `resume`. Drop and cancel still clear it.
/// Done is swept in this window and is not parked.
pub(super) struct ParkedMark<'a> {
    heap: &'a mut LuaHeap,
    done: Option<MarkSnapshot>,
    swept: bool,
    /// Set only by a successful `park`, so Drop does not clear that mark.
    keep: bool,
}

impl MarkState {
    fn new(rescan_epoch: u64) -> Self {
        Self {
            phase: MarkPhase::Start,
            gray: Vec::new(),
            modes: None,
            index: 0,
            mode_edge: None,
            mode_owner: 0,
            byte_at: 0,
            pending_mode: WeakMode::default(),
            init_at: 0,
            owner: None,
            cursor: None,
            owner_kind: HandleKind::Table,
            owner_mode: WeakMode::default(),
            rescan_head: None,
            rescan_epoch,
            rescan_owner: None,
            rescan_cursor: None,
            rescan_mode: WeakMode::default(),
            discover_marks: Vec::new(),
            discover_batch: 0,
            discover_next: false,
            discover_at: 0,
            discover_on: false,
        }
    }

    fn mutation_open(&self) -> bool {
        matches!(self.phase, MarkPhase::Roots | MarkPhase::Gray)
    }

    fn phase_name(&self) -> &'static str {
        match self.phase {
            MarkPhase::Start => "start",
            MarkPhase::Reset => "reset",
            MarkPhase::ModeOpen => "mode-open",
            MarkPhase::ModeBytes => "mode-bytes",
            MarkPhase::ModeInit => "mode-init",
            MarkPhase::Roots => "roots",
            MarkPhase::Gray => "gray",
            MarkPhase::Done => "done",
        }
    }
}

impl<'a> MarkSession<'a> {
    pub(super) fn start(heap: &'a mut LuaHeap) -> Self {
        debug_assert!(heap.mark.is_none(), "a mark is already parked");
        debug_assert!(
            !heap.has_sweep_tail(),
            "a published sweep tail and a mark session cannot coexist"
        );
        let rescan_epoch = heap.advance_rescan_epoch();
        heap.mark = Some(MarkState::new(rescan_epoch));
        Self { heap }
    }

    pub(super) fn meter(&self) -> super::ScanMeter {
        self.heap.scan
    }

    pub(super) fn pulse(&mut self, budget: u64) -> MarkView {
        let mut state = self.heap.mark.take().expect("mark state");
        let view = pulse_state(self.heap, &mut state, budget);
        self.heap.mark = Some(state);
        view
    }

    pub(super) fn drain(&mut self) {
        while self
            .heap
            .mark
            .as_ref()
            .is_some_and(|state| !matches!(state.phase, MarkPhase::Done))
        {
            self.pulse(64);
        }
    }

    pub(super) fn sweep(self) -> CollectReport {
        let state = self.heap.mark.take().expect("mark state");
        assert!(
            matches!(state.phase, MarkPhase::Done),
            "sweep needs a finished mark"
        );
        self.heap.sweep_marked(MarkSnapshot { modes: state.modes })
    }

    pub(super) fn into_snapshot(self) -> MarkSnapshot {
        let state = self.heap.mark.take().expect("drained mark");
        assert!(
            matches!(state.phase, MarkPhase::Done),
            "drain needs a finished mark"
        );
        MarkSnapshot { modes: state.modes }
    }
}

impl Drop for MarkSession<'_> {
    fn drop(&mut self) {
        self.heap.mark = None;
    }
}

impl<'a> ParkedMark<'a> {
    pub(super) fn start(heap: &'a mut LuaHeap) -> Result<Self, HeapError> {
        if heap.mark.is_some() {
            return Err(HeapError::MarkActive);
        }
        if heap.has_sweep_tail() {
            return Err(HeapError::SweepPending);
        }
        let rescan_epoch = heap.advance_rescan_epoch();
        heap.mark = Some(MarkState::new(rescan_epoch));
        Ok(Self {
            heap,
            done: None,
            swept: false,
            keep: false,
        })
    }

    /// Borrows a mark this heap already holds. Absent is `None`.
    /// This does not create, reset, write, or replace that mark.
    pub(super) fn resume(heap: &'a mut LuaHeap) -> Option<Self> {
        heap.mark.as_ref()?;
        Some(Self {
            heap,
            done: None,
            swept: false,
            keep: false,
        })
    }

    /// Leaves an incomplete mark on the heap and ends this borrow.
    /// Done, or a heap with no mark, is refused. Refusal does not sweep and
    /// does not return a collection report.
    pub(super) fn park(mut self) -> Result<(), HeapError> {
        if self.heap.mark.is_none() || self.done.is_some() || self.swept {
            return Err(HeapError::MarkDenied);
        }
        self.keep = true;
        Ok(())
    }

    /// Opens one saved mark and parks it. Does not scan.
    /// Refusal leaves the heap unchanged.
    pub(super) fn open_saved(heap: &'a mut LuaHeap) -> Result<(), HeapError> {
        Self::start(heap)?.park()
    }

    pub(super) fn meter(&self) -> super::ScanMeter {
        self.heap.scan
    }

    pub(super) fn stats(&self) -> HeapStats {
        self.heap.stats()
    }

    pub(super) fn logical_bytes(&self) -> u64 {
        self.heap.logical_bytes()
    }

    pub(super) fn pulse(&mut self, budget: u64) -> MarkView {
        if self.done.is_some() {
            return self.finished_view();
        }
        let mut state = self.heap.mark.take().expect("parked mark");
        let view = pulse_state(self.heap, &mut state, budget);
        if matches!(state.phase, MarkPhase::Done) {
            self.done = Some(MarkSnapshot {
                modes: state.modes.take(),
            });
        } else {
            self.heap.mark = Some(state);
        }
        view
    }

    /// Hands the current safepoint batch across the first Roots boundary.
    ///
    /// Zero budget only observes: it does not check the batch or read the
    /// graph. A positive budget refuses Done or a missing mark before any
    /// batch read, then checks quota and handles before the stepper moves.
    /// Refusal does not scan or write. An open Roots or Gray phase commits
    /// first. A closed phase commits once in this pulse, after the stepper
    /// enters Roots and before any root or gray step. The slice is not stored.
    pub(super) fn pulse_with_root_batch(
        &mut self,
        budget: u64,
        batch: &[Rooted],
    ) -> Result<MarkView, HeapError> {
        if budget == 0 {
            return Ok(self.pulse(budget));
        }
        self.advance_with_root_batch(budget, batch)
    }

    fn advance_with_root_batch(
        &mut self,
        budget: u64,
        batch: &[Rooted],
    ) -> Result<MarkView, HeapError> {
        if self.done.is_some() || self.swept {
            return Err(HeapError::MarkDenied);
        }
        let (view, done) = self
            .heap
            .advance_saved_mark_with_root_batch(budget, batch)?;
        if let Some(snapshot) = done {
            self.done = Some(snapshot);
        }
        Ok(view)
    }

    fn saved_mark(&self) -> Result<&MarkState, HeapError> {
        if self.done.is_some() || self.swept {
            return Err(HeapError::MarkDenied);
        }
        self.heap.mark.as_ref().ok_or(HeapError::MarkDenied)
    }

    pub(super) fn pending(&self) -> Option<PendingScan> {
        let state = self.heap.mark.as_ref()?;
        Some(PendingScan {
            owner: state.owner?,
            cursor: state.cursor?,
        })
    }

    /// Owner and record the weak-to-strong rescan will charge next.
    /// Independent of [`Self::pending`]. Absent unless that scan is paused
    /// on a record. This reads two fields and does not move either cursor.
    pub(super) fn pending_rescan(&self) -> Option<PendingScan> {
        let state = self.heap.mark.as_ref()?;
        Some(PendingScan {
            owner: state.rescan_owner?,
            cursor: state.rescan_cursor?,
        })
    }

    /// Next slot a deleted-mode probe will read. `None` when that walk is
    /// not armed. This does not move the cursor.
    #[cfg(test)]
    pub(super) fn discovery_slot(&self) -> Option<u32> {
        let state = self.heap.mark.as_ref()?;
        if !state.discover_on {
            return None;
        }
        u32::try_from(state.discover_at).ok()
    }

    pub(super) fn marked(&self, table: TableRef) -> bool {
        self.heap
            .slots
            .get(table.slot() as usize)
            .is_some_and(|slot| {
                slot.generation == table.generation()
                    && slot.kind == HandleKind::Table
                    && matches!(slot.state, SlotState::Live { marked: true })
            })
    }

    pub(super) fn phase_name(&self) -> &'static str {
        if self.done.is_some() {
            return "done";
        }
        self.heap
            .mark
            .as_ref()
            .map(MarkState::phase_name)
            .unwrap_or("absent")
    }

    pub(super) fn mutation_open(&self) -> bool {
        self.heap
            .mark
            .as_ref()
            .is_some_and(MarkState::mutation_open)
    }

    pub(super) fn write_allowed(&self, owner: TableRef) -> bool {
        self.mutation_open() && self.heap.alive(owner) && !self.marked(owner)
    }

    pub(super) fn field_index(&self, owner: TableRef, key: u32) -> Option<u32> {
        self.heap.find_key(owner.slot(), &EdgeKey::Field(key))
    }

    pub(super) fn edge(&self, from: TableRef, key: u32) -> Result<Option<TableRef>, HeapError> {
        self.heap.edge(from, key)
    }

    pub(super) fn metatable(&self, owner: TableRef) -> Result<Option<TableRef>, HeapError> {
        self.heap.metatable(owner)
    }

    pub(super) fn field_scalar(&self, from: TableRef, key: u32) -> Result<Value, HeapError> {
        self.heap
            .stored_value(from, |stored| stored == &EdgeKey::Field(key))
    }

    pub(super) fn delete_field(&mut self, from: TableRef, key: u32) -> Result<(), HeapError> {
        self.gate_field(from, key)?;
        let key = EdgeKey::Field(key);
        self.heap.clear_edge(from, &key)?;
        Ok(())
    }

    pub(super) fn write_field(
        &mut self,
        from: TableRef,
        key: u32,
        to: TableRef,
    ) -> Result<(), HeapError> {
        self.gate_field(from, key)?;
        self.heap.require_live(to)?;
        if self.marked(from) {
            return self.replace_discovered_field(from, key, to);
        }
        self.heap.set_edge(from, key, to)?;
        self.heap.scan.parked_edge_writes += 1;
        Ok(())
    }

    /// In-place replacement of one existing non-gap field. One key lookup.
    /// A missing key or a cursor gap is refused before any record changes.
    /// This does not insert, append, or reclaim another owner's gap.
    /// The shared writer barriers the table value; this path does not enqueue again.
    fn replace_discovered_field(
        &mut self,
        from: TableRef,
        key: u32,
        to: TableRef,
    ) -> Result<(), HeapError> {
        let edge_key = EdgeKey::Field(key);
        let Some(index) = self.heap.find_key(from.slot(), &edge_key) else {
            return Err(HeapError::MarkDenied);
        };
        let gap = self.heap.edge_slots[index as usize]
            .edge
            .as_ref()
            .is_some_and(|edge| edge.value.is_cursor_gap());
        if gap {
            return Err(HeapError::MarkDenied);
        }
        self.heap.replace_edge_value(
            from.slot(),
            index,
            EdgeValue::Table {
                slot: to.slot(),
                generation: to.generation(),
            },
        );
        self.heap.scan.parked_edge_writes += 1;
        Ok(())
    }

    /// Accepts a borrowed root batch in this exclusive window.
    ///
    /// Open only in Roots or Gray. Checks run in order: phase, then
    /// `roots.len() + batch.len()` (no overflow, and not above `limits.roots`;
    /// reserved empty slots and duplicate entries count), then each handle
    /// through `require_live`, `require_closure`, or `require_upvalue`. Those
    /// calls keep `WrongDomain`, `WrongKind`, and `StaleHandle`. The precheck
    /// reads are not scan visits. A refusal does not mark, enqueue, move a
    /// cursor, write a root slot, or change a success meter.
    ///
    /// Success hands each accepted entry, including a duplicate or an
    /// already-marked handle, to the existing gray queue through
    /// `enqueue_barrier_target` / `follow_one`. `follow_one` charges two
    /// `node_slot_visits` per entry. Accepted entries are
    /// `parked_root_batch_accepts`. A first enqueue reuses
    /// `parked_barrier_marks`. `work_pushed` still counts every newly marked
    /// node, whatever enqueued it. The borrowed slice is not stored. No frame
    /// is opened, and the call does not spend pulse budget.
    pub(super) fn accept_root_batch(&mut self, batch: &[Rooted]) -> Result<(), HeapError> {
        self.gate_root_batch(batch)?;
        if batch.is_empty() {
            return Ok(());
        }
        let mut state = self.heap.mark.take().expect("open mark");
        commit_root_batch(self.heap, &mut state, batch);
        self.heap.mark = Some(state);
        Ok(())
    }

    fn gate_root_batch(&self, batch: &[Rooted]) -> Result<(), HeapError> {
        if !self.saved_mark()?.mutation_open() {
            return Err(HeapError::MarkDenied);
        }
        validate_root_batch(self.heap, batch)
    }

    pub(super) fn write_table_root(
        &mut self,
        frame: RootFrameId,
        slot: u32,
        handle: TableRef,
    ) -> Result<(), HeapError> {
        self.gate_mutation()?;
        self.heap.write_root(frame, slot, handle)
    }

    pub(super) fn write_closure_root(
        &mut self,
        frame: RootFrameId,
        slot: u32,
        handle: ClosureRef,
    ) -> Result<(), HeapError> {
        self.gate_mutation()?;
        self.heap.write_closure_root(frame, slot, handle)
    }

    pub(super) fn write_upvalue_root(
        &mut self,
        frame: RootFrameId,
        slot: u32,
        handle: UpvalueRef,
    ) -> Result<(), HeapError> {
        self.gate_mutation()?;
        self.heap.write_upvalue_root(frame, slot, handle)
    }

    /// Test borrow of the heap while saved mark state stays on it.
    ///
    /// `LuaHeap::set_edge` can replace an existing field through this borrow.
    /// It is not a production entry and is absent outside tests.
    #[cfg(test)]
    pub(super) fn with_saved_heap<R>(&mut self, body: impl FnOnce(&mut LuaHeap) -> R) -> R {
        body(self.heap)
    }

    pub(super) fn scanning_owner(&self) -> Option<u32> {
        self.heap.mark.as_ref().and_then(|state| state.owner)
    }

    pub(super) fn object_index(&self, owner: TableRef, key: TableRef) -> Option<u32> {
        self.heap.find_key(owner.slot(), &EdgeKey::object(key))
    }

    pub(super) fn record_is_gap(&self, index: u32) -> bool {
        self.heap
            .edge_slots
            .get(index as usize)
            .and_then(|record| record.edge.as_ref())
            .is_some_and(|edge| edge.value.is_cursor_gap())
    }

    pub(super) fn root_table(
        &self,
        frame: RootFrameId,
        slot: u32,
    ) -> Result<Option<TableRef>, HeapError> {
        let found = self.heap.frame(frame)?;
        if slot >= found.width {
            return Err(HeapError::StaleFrame);
        }
        match self.heap.roots[found.start as usize + slot as usize] {
            Some(Rooted::Table(handle)) => Ok(Some(handle)),
            Some(_) => Err(HeapError::WrongKind),
            None => Ok(None),
        }
    }

    pub(super) fn root_closure(
        &self,
        frame: RootFrameId,
        slot: u32,
    ) -> Result<Option<ClosureRef>, HeapError> {
        let found = self.heap.frame(frame)?;
        if slot >= found.width {
            return Err(HeapError::StaleFrame);
        }
        match self.heap.roots[found.start as usize + slot as usize] {
            Some(Rooted::Closure(handle)) => Ok(Some(handle)),
            Some(_) => Err(HeapError::WrongKind),
            None => Ok(None),
        }
    }

    pub(super) fn root_upvalue(
        &self,
        frame: RootFrameId,
        slot: u32,
    ) -> Result<Option<UpvalueRef>, HeapError> {
        let found = self.heap.frame(frame)?;
        if slot >= found.width {
            return Err(HeapError::StaleFrame);
        }
        match self.heap.roots[found.start as usize + slot as usize] {
            Some(Rooted::Upvalue(handle)) => Ok(Some(handle)),
            Some(_) => Err(HeapError::WrongKind),
            None => Ok(None),
        }
    }

    /// Private collect while this driver holds the heap. It does not sweep.
    pub(super) fn ordinary_collect(&mut self) -> Result<CollectReport, HeapError> {
        Err(HeapError::MarkActive)
    }

    /// A second start while saved progress is still on this heap.
    pub(super) fn restart(&mut self) -> Result<(), HeapError> {
        if self.heap.mark.is_some() || self.done.is_some() {
            return Err(HeapError::MarkActive);
        }
        Err(HeapError::MarkActive)
    }

    pub(super) fn deny(&mut self, op: PausedOp) -> Result<(), HeapError> {
        match op {
            PausedOp::Alloc
            | PausedOp::Rollback
            | PausedOp::Metatable
            | PausedOp::Mode
            | PausedOp::Root
            | PausedOp::Token => Err(HeapError::MarkDenied),
        }
    }

    pub(super) fn cancel(mut self) {
        self.heap.mark = None;
        self.done = None;
    }

    /// Sweep a finished mark in this same window. An unfinished mark is not a
    /// snapshot and is not swept.
    pub(super) fn complete(&mut self) -> Result<CollectReport, HeapError> {
        if self.swept {
            return Err(HeapError::MarkDenied);
        }
        let Some(modes) = self.done.take() else {
            return Err(HeapError::MarkDenied);
        };
        self.heap.mark = None;
        self.swept = true;
        Ok(self.heap.sweep_marked(modes))
    }

    fn finished_view(&self) -> MarkView {
        MarkView {
            finished: true,
            meter: self.heap.scan,
        }
    }

    fn gate_field(&self, from: TableRef, key: u32) -> Result<(), HeapError> {
        self.gate_mutation()?;
        // Field 0 is `__mode`. The private cell id is not a Lua key either.
        if key == LuaHeap::MODE_FIELD || key == super::CELL_EDGE {
            return Err(HeapError::MarkDenied);
        }
        self.heap.require_live(from)?;
        Ok(())
    }

    fn gate_mutation(&self) -> Result<(), HeapError> {
        let state = self.heap.mark.as_ref().ok_or(HeapError::MarkDenied)?;
        if !state.mutation_open() {
            return Err(HeapError::MarkDenied);
        }
        Ok(())
    }
}

impl Drop for ParkedMark<'_> {
    fn drop(&mut self) {
        if !self.keep {
            self.heap.mark = None;
        }
    }
}

impl LuaHeap {
    /// Checked `roots.len() + extra` against `limits.roots`.
    /// Does not read handles, open a mark, or scan.
    pub(crate) fn root_batch_fits(&self, extra: usize) -> Result<(), HeapError> {
        checked_root_occupancy(self, extra)
    }

    /// Positive-budget step of a mark already stored on this heap.
    ///
    /// Order is the parked driver's order: saved state, Done, quota, then
    /// each typed handle. Only then is `heap.mark` taken. [`pulse_budget`]
    /// commits `batch` once, after this pulse enters Roots and before any
    /// root or gray step. An unfinished mark is written back. A finished
    /// mark is not stored; its modes are returned once. This does not
    /// enumerate tokens, close them, or sweep.
    pub(super) fn advance_saved_mark_with_root_batch(
        &mut self,
        budget: u64,
        batch: &[Rooted],
    ) -> Result<(MarkView, Option<MarkSnapshot>), HeapError> {
        debug_assert!(budget > 0, "zero budget does not enter the shared advance");
        let state = self.mark.as_ref().ok_or(HeapError::MarkDenied)?;
        if matches!(state.phase, MarkPhase::Done) {
            return Err(HeapError::MarkDenied);
        }
        validate_root_batch(self, batch)?;
        let mut state = self.mark.take().expect("saved mark");
        let view = pulse_budget(self, &mut state, budget, Some(batch));
        if matches!(state.phase, MarkPhase::Done) {
            Ok((
                view,
                Some(MarkSnapshot {
                    modes: state.modes.take(),
                }),
            ))
        } else {
            self.mark = Some(state);
            Ok((view, None))
        }
    }
}

fn checked_root_occupancy(heap: &LuaHeap, extra: usize) -> Result<(), HeapError> {
    let occupied = u32::try_from(heap.roots.len()).map_err(|_| HeapError::RootCapacity)?;
    let extra = u32::try_from(extra).map_err(|_| HeapError::RootCapacity)?;
    let end = occupied.checked_add(extra).ok_or(HeapError::RootCapacity)?;
    if end > heap.limits.roots {
        return Err(HeapError::RootCapacity);
    }
    Ok(())
}

fn validate_root_batch(heap: &LuaHeap, batch: &[Rooted]) -> Result<(), HeapError> {
    checked_root_occupancy(heap, batch.len())?;
    for rooted in batch {
        match *rooted {
            Rooted::Table(handle) => heap.require_live(handle)?,
            Rooted::Closure(handle) => heap.require_closure(handle)?,
            Rooted::Upvalue(handle) => heap.require_upvalue(handle)?,
            Rooted::Userdata(handle) => heap.require_userdata(handle)?,
        }
    }
    Ok(())
}

fn commit_root_batch(heap: &mut LuaHeap, state: &mut MarkState, batch: &[Rooted]) {
    if batch.is_empty() {
        return;
    }
    for rooted in batch {
        let (slot, generation, kind) = rooted_identity(*rooted);
        enqueue_barrier_target(heap, state, slot, generation, kind);
    }
    heap.scan.parked_root_batch_accepts +=
        u64::try_from(batch.len()).expect("quota already checked the length");
}

fn pulse_state(heap: &mut LuaHeap, state: &mut MarkState, budget: u64) -> MarkView {
    pulse_budget(heap, state, budget, None)
}

/// Same units, gray queue, and meters as [`pulse_state`]. A batch, when
/// present, is committed once before any Roots or Gray step. It is not stored.
fn pulse_budget(
    heap: &mut LuaHeap,
    state: &mut MarkState,
    budget: u64,
    batch: Option<&[Rooted]>,
) -> MarkView {
    if budget == 0 || matches!(state.phase, MarkPhase::Done) {
        return view(heap, state);
    }
    if matches!(state.phase, MarkPhase::Start) {
        heap.scan.mark_passes += 1;
        state.phase = MarkPhase::Reset;
    }
    let mut handed = false;
    let mut left = budget;
    let mut spins = 0u32;
    while left > 0 && !matches!(state.phase, MarkPhase::Done) {
        if !handed && matches!(state.phase, MarkPhase::Roots | MarkPhase::Gray) {
            if let Some(batch) = batch {
                commit_root_batch(heap, state, batch);
            }
            handed = true;
        }
        if step(heap, state) {
            left -= 1;
            spins = 0;
        } else {
            spins += 1;
            debug_assert!(spins <= 4, "mark phase did not advance");
        }
    }
    view(heap, state)
}

fn view(heap: &LuaHeap, state: &MarkState) -> MarkView {
    MarkView {
        finished: matches!(state.phase, MarkPhase::Done),
        meter: heap.scan,
    }
}

fn step(heap: &mut LuaHeap, state: &mut MarkState) -> bool {
    match state.phase {
        MarkPhase::Start => false,
        MarkPhase::Reset => step_reset(heap, state),
        MarkPhase::ModeOpen => step_mode_open(heap, state),
        MarkPhase::ModeBytes => step_mode_byte(heap, state),
        MarkPhase::ModeInit => step_mode_init(heap, state),
        MarkPhase::Roots => step_root(heap, state),
        MarkPhase::Gray => step_gray(heap, state),
        MarkPhase::Done => false,
    }
}

fn step_reset(heap: &mut LuaHeap, state: &mut MarkState) -> bool {
    if state.index >= heap.slots.len() {
        state.phase = MarkPhase::Roots;
        state.index = 0;
        return false;
    }
    let index = state.index;
    if let SlotState::Live { marked } = &mut heap.slots[index].state {
        *marked = false;
    }
    heap.scan.node_slot_visits += 1;
    heap.scan.weak_mode_slot_checks += 1;
    let mode_edge = heap.slots[index].mode_edge;
    let is_table = heap.slots[index].kind == HandleKind::Table;
    if heap.metatable_edges > 0 && is_table && mode_edge.is_some() {
        state.mode_edge = mode_edge;
        state.mode_owner = index;
        state.byte_at = 0;
        state.pending_mode = WeakMode::default();
        state.phase = MarkPhase::ModeOpen;
    } else {
        state.index += 1;
    }
    true
}

fn step_mode_open(heap: &mut LuaHeap, state: &mut MarkState) -> bool {
    let edge_index = state.mode_edge.expect("mode open has an edge");
    heap.scan.weak_mode_edge_checks += 1;
    if mode_string_len(heap, edge_index).is_some() {
        state.phase = MarkPhase::ModeBytes;
    } else {
        advance_after_mode(state);
    }
    true
}

fn step_mode_byte(heap: &mut LuaHeap, state: &mut MarkState) -> bool {
    let edge_index = state.mode_edge.expect("mode byte has an edge");
    let at = state.byte_at;
    let Some(byte) = mode_string_byte(heap, edge_index, at) else {
        finish_mode_parse(state);
        return false;
    };
    heap.scan.weak_mode_bytes += 1;
    state.byte_at = at + 1;
    match byte {
        b'k' => state.pending_mode.keys = true,
        b'v' => state.pending_mode.values = true,
        0 => {
            finish_mode_parse(state);
            return true;
        }
        _ => {}
    }
    let len = mode_string_len(heap, edge_index).unwrap_or(0);
    if state.byte_at >= len {
        finish_mode_parse(state);
    }
    true
}

fn finish_mode_parse(state: &mut MarkState) {
    let parsed = state.pending_mode;
    if parsed.keys || parsed.values {
        if state.modes.is_none() {
            state.init_at = 0;
            state.phase = MarkPhase::ModeInit;
            return;
        }
        store_mode(state, parsed);
    }
    advance_after_mode(state);
}

fn step_mode_init(heap: &mut LuaHeap, state: &mut MarkState) -> bool {
    let slot_count = heap.slots.len();
    if state.modes.is_none() {
        state.modes = Some(Vec::with_capacity(slot_count));
        state.discover_marks = Vec::with_capacity(slot_count);
    }
    let modes = state.modes.as_mut().expect("mode init has a cache");
    modes.push(WeakMode::default());
    state.discover_marks.push(ModeDiscoveryMark::default());
    heap.scan.scratch.weak_modes_inited += 1;
    heap.scan.scratch.mode_discover_inited += 1;
    state.init_at += 1;
    if state.init_at == slot_count {
        let parsed = state.pending_mode;
        store_mode(state, parsed);
        advance_after_mode(state);
    }
    true
}

fn store_mode(state: &mut MarkState, parsed: WeakMode) {
    let modes = state.modes.as_mut().expect("mode cache");
    modes[state.mode_owner] = parsed;
}

fn advance_after_mode(state: &mut MarkState) {
    state.mode_edge = None;
    state.index += 1;
    state.phase = MarkPhase::Reset;
}

fn step_root(heap: &mut LuaHeap, state: &mut MarkState) -> bool {
    if state.index >= heap.roots.len() {
        state.phase = MarkPhase::Gray;
        return false;
    }
    let index = state.index;
    state.index += 1;
    heap.scan.root_slot_visits += 1;
    let Some(rooted) = heap.roots[index] else {
        return true;
    };
    heap.scan.node_slot_visits += 1;
    if heap.rooted_alive(rooted) {
        heap.scan.node_slot_visits += 1;
        let slot = rooted.slot();
        if heap.mark_new(slot) {
            state.gray.push(slot);
        }
    }
    true
}

fn step_gray(heap: &mut LuaHeap, state: &mut MarkState) -> bool {
    if state.owner.is_some() {
        if state.cursor.is_some() {
            return charge_edge(heap, state);
        }
        state.owner = None;
        return false;
    }
    if !state.gray.is_empty() {
        return begin_owner(heap, state);
    }
    if state.rescan_owner.is_some() {
        if state.rescan_cursor.is_some() {
            return charge_rescan(heap, state);
        }
        state.rescan_owner = None;
        return false;
    }
    if let Some(slot) = pop_rescan(heap, state) {
        return open_rescan(heap, state, slot);
    }
    if state.discover_on {
        return probe_deleted_mode_owner(heap, state);
    }
    state.phase = MarkPhase::Done;
    false
}

fn begin_owner(heap: &mut LuaHeap, state: &mut MarkState) -> bool {
    let Some(slot) = state.gray.pop() else {
        state.phase = MarkPhase::Done;
        return false;
    };
    let Some(stored) = heap.slots.get(slot as usize) else {
        return false;
    };
    if !matches!(stored.state, SlotState::Live { .. }) {
        return false;
    }
    let kind = stored.kind;
    let mode = if kind == HandleKind::Table && heap.metatable_edges > 0 {
        owner_mode(heap, state, slot)
    } else {
        WeakMode::default()
    };
    heap.scan.owner_head_checks += 1;
    state.cursor = heap.slots[slot as usize].edge_head;
    state.owner = Some(slot);
    state.owner_kind = kind;
    state.owner_mode = mode;
    if mode.keys || mode.values {
        note_weak_started(heap, state, slot);
    }
    true
}

fn charge_edge(heap: &mut LuaHeap, state: &mut MarkState) -> bool {
    let cursor = state.cursor.expect("edge unit has a cursor");
    let next = heap.edge_slots[cursor as usize].next;
    state.cursor = next;
    let gap = heap.edge_slots[cursor as usize]
        .edge
        .as_ref()
        .is_some_and(|edge| edge.value.is_cursor_gap());
    if gap {
        heap.scan.outgoing_gap_checks += 1;
        return true;
    }
    heap.scan.outgoing_edge_checks += 1;
    follow_edge_ends(heap, state, cursor);
    true
}

fn charge_rescan(heap: &mut LuaHeap, state: &mut MarkState) -> bool {
    let cursor = state.rescan_cursor.expect("rescan unit has a cursor");
    let next = heap.edge_slots[cursor as usize].next;
    state.rescan_cursor = next;
    let gap = heap.edge_slots[cursor as usize]
        .edge
        .as_ref()
        .is_some_and(|edge| edge.value.is_cursor_gap());
    if gap {
        heap.scan.outgoing_gap_checks += 1;
        return true;
    }
    heap.scan.outgoing_edge_checks += 1;
    follow_rescan_ends(heap, state, cursor);
    true
}

fn pop_rescan(heap: &mut LuaHeap, state: &mut MarkState) -> Option<u32> {
    let slot = state.rescan_head?;
    let next = {
        let stored = heap.slots.get_mut(slot as usize)?;
        debug_assert_eq!(
            stored.rescan_epoch, state.rescan_epoch,
            "rescan head belongs to an older cycle"
        );
        debug_assert!(stored.rescan_queued, "rescan head is not queued");
        stored.rescan_queued = false;
        stored.rescan_prev = None;
        stored.rescan_next.take()
    };
    state.rescan_head = next;
    if let Some(next) = next
        && let Some(stored) = heap.slots.get_mut(next as usize)
    {
        stored.rescan_prev = None;
    }
    Some(slot)
}

fn push_rescan(heap: &mut LuaHeap, state: &mut MarkState, owner: u32) {
    {
        let stored = &mut heap.slots[owner as usize];
        stored.rescan_queued = true;
        stored.rescan_prev = None;
        stored.rescan_next = state.rescan_head;
    }
    if let Some(head) = state.rescan_head {
        heap.slots[head as usize].rescan_prev = Some(owner);
    }
    state.rescan_head = Some(owner);
}

/// Removes a slot being released from this mark's rescan list before its state
/// becomes Free. The links are on the slot, so this is constant work and does
/// not walk the queue. A tag from an older epoch is not in the current list;
/// it is cleared without touching the current head. A released slot must never
/// stay queued: reuse would otherwise inherit queue membership.
pub(super) fn release_rescan_slot(
    mut state: Option<&mut MarkState>,
    slots: &mut [Slot],
    index: u32,
) {
    let Some(stored) = slots.get(index as usize) else {
        return;
    };
    let epoch = stored.rescan_epoch;
    let queued = stored.rescan_queued;
    let prev = stored.rescan_prev;
    let next = stored.rescan_next;
    let same_cycle = state
        .as_ref()
        .is_some_and(|state| state.rescan_epoch == epoch);
    if same_cycle && queued {
        if let Some(prev) = prev {
            if let Some(prev_slot) = slots.get_mut(prev as usize) {
                prev_slot.rescan_next = next;
            }
        } else if let Some(state) = state.as_mut() {
            state.rescan_head = next;
        }
        if let Some(next) = next
            && let Some(next_slot) = slots.get_mut(next as usize)
        {
            next_slot.rescan_prev = prev;
        }
    }
    if let Some(stored) = slots.get_mut(index as usize) {
        stored.rescan_epoch = 0;
        stored.rescan_queued = false;
        stored.rescan_prev = None;
        stored.rescan_next = None;
    }
}

fn open_rescan(heap: &mut LuaHeap, state: &mut MarkState, slot: u32) -> bool {
    let Some(stored) = heap.slots.get(slot as usize) else {
        return false;
    };
    if stored.kind != HandleKind::Table || !matches!(stored.state, SlotState::Live { .. }) {
        return false;
    }
    let mode = cached_owner_mode(heap, state, slot);
    heap.scan.owner_head_checks += 1;
    heap.scan.owner_rescan_heads += 1;
    state.rescan_cursor = heap.slots[slot as usize].edge_head;
    state.rescan_owner = Some(slot);
    state.rescan_mode = mode;
    true
}

/// Reads one capacity slot and at most one indexed discovery stamp. A
/// matching owner that already started a weak scan is queued on the existing
/// intrusive rescan list. Only the discovery cursor advances here.
fn probe_deleted_mode_owner(heap: &mut LuaHeap, state: &mut MarkState) -> bool {
    let at = state.discover_at;
    if at >= heap.slots.len() {
        state.discover_at = 0;
        state.discover_on = std::mem::take(&mut state.discover_next);
        if state.discover_on {
            state.discover_batch += 1;
        }
        return false;
    }
    state.discover_at = at + 1;
    heap.scan.mode_owner_probes += 1;
    let Some(stored) = heap.slots.get(at) else {
        return true;
    };
    if stored.kind != HandleKind::Table || !matches!(stored.state, SlotState::Live { .. }) {
        return true;
    }
    let Some((meta_slot, meta_gen)) = stored.metatable else {
        return true;
    };
    let Some(&stamp) = state.discover_marks.get(meta_slot as usize) else {
        return true;
    };
    let active = ModeDiscoveryMark {
        generation: meta_gen,
        batch: state.discover_batch,
    };
    heap.scan.mode_discover_compares += 1;
    if stamp == active {
        queue_started_owner(heap, state, at as u32);
    }
    true
}

fn queue_started_owner(heap: &mut LuaHeap, state: &mut MarkState, owner: u32) {
    let started = heap
        .slots
        .get(owner as usize)
        .is_some_and(|stored| stored.rescan_epoch == state.rescan_epoch);
    if !started {
        return;
    }
    if heap
        .slots
        .get(owner as usize)
        .is_some_and(|stored| stored.rescan_queued)
    {
        return;
    }
    push_rescan(heap, state, owner);
    heap.scan.scratch.owner_rescan_pushed += 1;
    heap.scan.owner_rescan_schedules += 1;
}

/// Called only on the cache's weak-to-strong transition, once per metatable
/// slot per mark. No identity lookup or allocation is needed to arm it.
fn arm_deleted_mode(heap: &mut LuaHeap, state: &mut MarkState, slot: u32, generation: u32) {
    if !state.discover_on {
        state.discover_batch += 1;
        state.discover_on = true;
    }
    let batch = if state.discover_at == 0 {
        state.discover_batch
    } else {
        state.discover_next = true;
        state.discover_batch + 1
    };
    state.discover_marks[slot as usize] = ModeDiscoveryMark { generation, batch };
    heap.scan.mode_discover_writes += 1;
}

fn follow_rescan_ends(heap: &mut LuaHeap, state: &mut MarkState, cursor: u32) {
    let (object_key, value_target) = {
        let edge = heap.edge_slots[cursor as usize]
            .edge
            .as_ref()
            .expect("rescan chain holds an occupied record");
        let object_key = match &edge.key {
            EdgeKey::Object {
                slot,
                generation,
                kind,
            } => Some((*slot, *generation, *kind)),
            EdgeKey::Field(_) | EdgeKey::Utf8(_) | EdgeKey::Number(_) => None,
        };
        let value_target = match &edge.value {
            EdgeValue::Table { slot, generation } => Some((*slot, *generation, HandleKind::Table)),
            EdgeValue::Closure { slot, generation } => {
                Some((*slot, *generation, HandleKind::Closure))
            }
            EdgeValue::Upvalue { slot, generation } => {
                Some((*slot, *generation, HandleKind::Upvalue))
            }
            EdgeValue::Userdata { slot, generation } => {
                Some((*slot, *generation, HandleKind::Userdata))
            }
            EdgeValue::Scalar(_) => None,
        };
        (object_key, value_target)
    };
    if !state.rescan_mode.keys
        && let Some((slot, generation, kind)) = object_key
    {
        let _matched = follow_one(heap, state, slot, generation, kind);
    }
    if !state.rescan_mode.values
        && let Some((slot, generation, kind)) = value_target
    {
        let _matched = follow_one(heap, state, slot, generation, kind);
    }
}

fn follow_edge_ends(heap: &mut LuaHeap, state: &mut MarkState, cursor: u32) {
    let (object_key, value_target) = {
        let edge = heap.edge_slots[cursor as usize]
            .edge
            .as_ref()
            .expect("owner chain holds an occupied record");
        let object_key = match &edge.key {
            EdgeKey::Object {
                slot,
                generation,
                kind,
            } => Some((*slot, *generation, *kind)),
            EdgeKey::Field(_) | EdgeKey::Utf8(_) | EdgeKey::Number(_) => None,
        };
        let value_target = match &edge.value {
            EdgeValue::Table { slot, generation } => Some((*slot, *generation, HandleKind::Table)),
            EdgeValue::Closure { slot, generation } => {
                Some((*slot, *generation, HandleKind::Closure))
            }
            EdgeValue::Upvalue { slot, generation } => {
                Some((*slot, *generation, HandleKind::Upvalue))
            }
            EdgeValue::Userdata { slot, generation } => {
                Some((*slot, *generation, HandleKind::Userdata))
            }
            EdgeValue::Scalar(_) => None,
        };
        (object_key, value_target)
    };
    let from_table = state.owner_kind == HandleKind::Table;
    if from_table
        && !state.owner_mode.keys
        && let Some((slot, generation, kind)) = object_key
    {
        let _matched = follow_one(heap, state, slot, generation, kind);
    }
    // A table entry follows that table's weak mode. A closure, cell, or
    // userdata value on a non-table owner is not a table entry, so it stays
    // strong. A userdata value on a table is weak exactly when values are.
    let strong_value = !from_table || !state.owner_mode.values;
    if strong_value && let Some((slot, generation, kind)) = value_target {
        let _matched = follow_one(heap, state, slot, generation, kind);
    }
}

/// Tags the target slot when its ordinary scan opens a weak side. The tag is
/// the slot's own epoch, so a grown slot needs no vector entry and no suffix
/// initialization. The first tag for a slot in a mark is one initialized slot
/// state; a repeat is a no-op. An older epoch is inert and is replaced here.
fn note_weak_started(heap: &mut LuaHeap, state: &mut MarkState, slot: u32) {
    let epoch = state.rescan_epoch;
    let first = {
        let Some(stored) = heap.slots.get_mut(slot as usize) else {
            return;
        };
        if stored.rescan_epoch == epoch {
            false
        } else {
            stored.rescan_epoch = epoch;
            stored.rescan_queued = false;
            stored.rescan_prev = None;
            stored.rescan_next = None;
            true
        }
    };
    if first {
        heap.scan.scratch.owner_rescan_inited =
            heap.scan.scratch.owner_rescan_inited.saturating_add(1);
    }
}

fn mode_of_tuple(state: &MarkState, tuple: Option<(u32, u32)>) -> WeakMode {
    let Some((slot, _)) = tuple else {
        return WeakMode::default();
    };
    state
        .modes
        .as_ref()
        .and_then(|modes| modes.get(slot as usize))
        .copied()
        .unwrap_or_default()
}

fn owner_mode(heap: &mut LuaHeap, state: &mut MarkState, slot: u32) -> WeakMode {
    let Some(stored) = heap.slots.get(slot as usize) else {
        return WeakMode::default();
    };
    let Some((to_slot, to_generation)) = stored.metatable else {
        return WeakMode::default();
    };
    heap.scan.metatable_edge_checks += 1;
    if follow_one(heap, state, to_slot, to_generation, HandleKind::Table)
        && let Some(modes) = state.modes.as_ref()
    {
        heap.scan.weak_mode_lookups += 1;
        return modes.get(to_slot as usize).copied().unwrap_or_default();
    }
    WeakMode::default()
}

/// Slot identity already stored on a typed root. This does not allocate a handle.
fn rooted_identity(rooted: Rooted) -> (u32, u32, HandleKind) {
    match rooted {
        Rooted::Table(handle) => (handle.slot(), handle.generation(), HandleKind::Table),
        Rooted::Closure(handle) => (handle.slot(), handle.generation(), HandleKind::Closure),
        Rooted::Upvalue(handle) => (handle.slot(), handle.generation(), HandleKind::Upvalue),
        Rooted::Userdata(handle) => (handle.slot(), handle.generation(), HandleKind::Userdata),
    }
}

/// Counts a newly gray target. An already-marked slot does not increment.
fn enqueue_barrier_target(
    heap: &mut LuaHeap,
    state: &mut MarkState,
    slot: u32,
    generation: u32,
    kind: HandleKind,
) {
    let before = heap.scan.scratch.work_pushed;
    let matched = follow_one(heap, state, slot, generation, kind);
    if matched && heap.scan.scratch.work_pushed > before {
        heap.scan.parked_barrier_marks += 1;
    }
}

/// Reads the mark bit of a table owner. No chain walk and no scan counter.
fn discovered_table(heap: &LuaHeap, owner: u32) -> bool {
    heap.slots.get(owner as usize).is_some_and(|slot| {
        slot.kind == HandleKind::Table && matches!(slot.state, SlotState::Live { marked: true })
    })
}

/// Reads the mark bit of one upvalue generation. A reused slot of another
/// generation is not this cell. No chain walk and no scan counter.
fn discovered_upvalue(heap: &LuaHeap, owner: u32, generation: u32) -> bool {
    heap.slots.get(owner as usize).is_some_and(|slot| {
        slot.kind == HandleKind::Upvalue
            && slot.generation == generation
            && matches!(slot.state, SlotState::Live { marked: true })
    })
}

/// Reads the reset-time mode cache. No metatable follow and no scan counter.
fn cached_values_weak(heap: &LuaHeap, state: &MarkState, owner: u32) -> bool {
    if heap.metatable_edges == 0 {
        return false;
    }
    let Some(stored) = heap.slots.get(owner as usize) else {
        return false;
    };
    if stored.kind != HandleKind::Table {
        return false;
    }
    let Some((meta_slot, _)) = stored.metatable else {
        return false;
    };
    state
        .modes
        .as_ref()
        .and_then(|modes| modes.get(meta_slot as usize))
        .is_some_and(|mode| mode.values)
}

/// Both weak bits from the same reset-time cache. No metatable follow and
/// no scan counter. The values-only fast path above is unchanged.
fn cached_owner_mode(heap: &LuaHeap, state: &MarkState, owner: u32) -> WeakMode {
    if heap.metatable_edges == 0 {
        return WeakMode::default();
    }
    let Some(stored) = heap.slots.get(owner as usize) else {
        return WeakMode::default();
    };
    if stored.kind != HandleKind::Table {
        return WeakMode::default();
    }
    let Some((meta_slot, _)) = stored.metatable else {
        return WeakMode::default();
    };
    state
        .modes
        .as_ref()
        .and_then(|modes| modes.get(meta_slot as usize))
        .copied()
        .unwrap_or_default()
}

/// Same visits as the old target follow. True when the slot matches, even
/// if it was already marked. Only a newly marked slot joins the gray stack.
fn follow_one(
    heap: &mut LuaHeap,
    state: &mut MarkState,
    slot: u32,
    generation: u32,
    kind: HandleKind,
) -> bool {
    heap.scan.node_slot_visits += 1;
    if !heap.slot_matches(slot, generation, kind) {
        return false;
    }
    heap.scan.node_slot_visits += 1;
    if heap.mark_new(slot) {
        state.gray.push(slot);
    }
    true
}

fn mode_string_len(heap: &LuaHeap, edge_index: u32) -> Option<usize> {
    let edge = heap.edge_slots[edge_index as usize].edge.as_ref()?;
    match &edge.value {
        EdgeValue::Scalar(Value::String(mode)) => Some(mode.len()),
        _ => None,
    }
}

fn mode_string_byte(heap: &LuaHeap, edge_index: u32, at: usize) -> Option<u8> {
    let edge = heap.edge_slots[edge_index as usize].edge.as_ref()?;
    match &edge.value {
        EdgeValue::Scalar(Value::String(mode)) => mode.as_bytes().get(at).copied(),
        _ => None,
    }
}

/// The direct object a closed-cell write just stored.
///
/// Only these two carriers use the current edge. The write names which one it
/// stored; the barrier does not inspect the record again.
pub(super) enum ClosedCellTarget {
    Table,
    Closure,
}

impl LuaHeap {
    /// Phase and whether a gray edge or queued owner is still unscanned.
    /// Does not take the mark, scan, or write.
    #[cfg(test)]
    pub fn saved_mark_query(&self) -> Option<(&'static str, bool)> {
        let state = self.mark.as_ref()?;
        Some((
            state.phase_name(),
            !state.gray.is_empty()
                || state.cursor.is_some()
                || state.rescan_owner.is_some()
                || state.rescan_cursor.is_some()
                || state.rescan_head.is_some()
                || state.discover_on,
        ))
    }

    /// Phase, pending bit, cursor owner slot, cursor record, and gray length.
    /// Reads the saved mark only. Does not take it, walk an edge, enqueue, or write.
    #[cfg(test)]
    pub fn saved_mark_watch(&self) -> Option<super::super::SavedMarkWatch> {
        let state = self.mark.as_ref()?;
        let gray_len = u32::try_from(state.gray.len()).unwrap_or(u32::MAX);
        Some(super::super::SavedMarkWatch {
            phase: state.phase_name(),
            pending: !state.gray.is_empty() || state.cursor.is_some(),
            cursor_owner: state.owner,
            cursor_index: state.cursor,
            gray_len,
        })
    }

    /// Live marked bit of one table. Does not walk an edge or change the bit.
    #[cfg(test)]
    pub fn table_marked(&self, table: TableRef) -> bool {
        self.slots.get(table.slot() as usize).is_some_and(|slot| {
            slot.generation == table.generation()
                && slot.kind == HandleKind::Table
                && matches!(slot.state, SlotState::Live { marked: true })
        })
    }

    /// Whether this table's slot is already in the saved gray vector.
    /// Reads that vector only. Does not walk edges or push.
    #[cfg(test)]
    pub fn table_in_gray(&self, table: TableRef) -> bool {
        self.mark
            .as_ref()
            .is_some_and(|state| state.gray.contains(&table.slot()))
    }

    /// Whether one open frame already roots this table. Empty and non-table
    /// slots are skipped. Does not scan edges.
    #[cfg(test)]
    pub fn frame_roots_table(&self, frame: RootFrameId, table: TableRef) -> bool {
        let Ok(found) = self.frame(frame) else {
            return false;
        };
        (0..found.width).any(|slot| {
            self.rooted_table(frame, slot)
                .ok()
                .flatten()
                .is_some_and(|rooted| rooted == table)
        })
    }

    /// Record index of one number key, plus the table slot of its value when
    /// that value is a table, and whether the value is a string. One owner-chain
    /// read. Does not write, enqueue, or move a cursor.
    #[cfg(test)]
    pub fn number_edge_watch(&self, table: TableRef, key: f64) -> Option<(u32, Option<u32>, bool)> {
        if key.is_nan() || !self.alive(table) {
            return None;
        }
        self.edge_watch(table, &EdgeKey::number(key))
    }

    /// Record index of one UTF-8 key. Same read-only shape as [`Self::number_edge_watch`].
    #[cfg(test)]
    pub fn utf8_edge_watch(&self, table: TableRef, key: &str) -> Option<(u32, Option<u32>, bool)> {
        if !self.alive(table) {
            return None;
        }
        self.edge_watch(table, &EdgeKey::named(key))
    }

    #[cfg(test)]
    fn edge_watch(&self, table: TableRef, key: &EdgeKey) -> Option<(u32, Option<u32>, bool)> {
        let index = self.find_key(table.slot(), key)?;
        let edge = self.edge_slots[index as usize].edge.as_ref()?;
        let (value_slot, value_string) = match &edge.value {
            EdgeValue::Table { slot, .. } => (Some(*slot), false),
            EdgeValue::Scalar(Value::String(_)) => (None, true),
            _ => (None, false),
        };
        Some((index, value_slot, value_string))
    }

    /// Counts a fixed root store and marks its target while mutation is open.
    /// The enqueue uses the same `follow_one` path as a root visit. It does
    /// not move the root cursor or the current owner cursor.
    pub(super) fn note_parked_root_store(&mut self, rooted: Rooted) {
        let Some(mut state) = self.mark.take() else {
            return;
        };
        if !state.mutation_open() {
            self.mark = Some(state);
            return;
        }
        self.scan.parked_root_writes += 1;
        let (slot, generation, kind) = match rooted {
            Rooted::Table(handle) => (handle.slot(), handle.generation(), HandleKind::Table),
            Rooted::Closure(handle) => (handle.slot(), handle.generation(), HandleKind::Closure),
            Rooted::Upvalue(handle) => (handle.slot(), handle.generation(), HandleKind::Upvalue),
            Rooted::Userdata(handle) => (handle.slot(), handle.generation(), HandleKind::Userdata),
        };
        enqueue_barrier_target(self, &mut state, slot, generation, kind);
        self.mark = Some(state);
    }

    /// Marks a strong table value written into an already discovered field.
    /// Weak values stay on the cached mode and are not followed. An owner
    /// that is not yet marked does not enter the gray queue. This does not
    /// call `owner_mode` and does not move either cursor.
    pub(super) fn barrier_strong_field(&mut self, owner: u32, slot: u32, generation: u32) {
        let Some(mut state) = self.mark.take() else {
            return;
        };
        if state.mutation_open()
            && discovered_table(self, owner)
            && !cached_values_weak(self, &state, owner)
        {
            enqueue_barrier_target(self, &mut state, slot, generation, HandleKind::Table);
        }
        self.mark = Some(state);
    }

    /// Checks whether either weak side becomes strong using the tuples captured
    /// before the slot write. The old mode comes from this cycle's cache and
    /// that tuple, not from the edge count, so clearing the last metatable
    /// edge cannot make the old mode look strong. A not-yet-started owner is
    /// left for the ordinary scan. This does not enqueue, scan, or move a cursor.
    pub(super) fn needs_owner_rescan(
        &self,
        owner: u32,
        old_tuple: Option<(u32, u32)>,
        new_tuple: Option<(u32, u32)>,
    ) -> bool {
        let Some(state) = self.mark.as_ref() else {
            return false;
        };
        if !state.mutation_open() {
            return false;
        }
        let old_mode = mode_of_tuple(state, old_tuple);
        let new_mode = mode_of_tuple(state, new_tuple);
        if !((old_mode.keys && !new_mode.keys) || (old_mode.values && !new_mode.values)) {
            return false;
        }
        self.slots
            .get(owner as usize)
            .is_some_and(|slot| slot.rescan_epoch == state.rescan_epoch)
    }

    /// After a real direct `__mode` unlink, replace this cycle's value-weak
    /// cache for that same generation. Owners that have not started read the
    /// missing-field mode. Owners that already started stay for the budgeted
    /// probe. A key-weak cache, a missing record, and a closed mutation are
    /// left alone. This does not walk slots or move either cursor.
    pub(super) fn note_direct_mode_deleted(&mut self, owner: u32) {
        let Some(mut state) = self.mark.take() else {
            return;
        };
        if !state.mutation_open() {
            self.mark = Some(state);
            return;
        }
        let generation = self.slots[owner as usize].generation;
        let cached = state
            .modes
            .as_ref()
            .and_then(|modes| modes.get(owner as usize))
            .copied()
            .unwrap_or_default();
        if !cached.values || cached.keys {
            self.mark = Some(state);
            return;
        }
        let Some(slot_mode) = state
            .modes
            .as_mut()
            .and_then(|modes| modes.get_mut(owner as usize))
        else {
            self.mark = Some(state);
            return;
        };
        *slot_mode = WeakMode::default();
        self.scan.mode_cache_updates += 1;
        arm_deleted_mode(self, &mut state, owner, generation);
        self.mark = Some(state);
    }

    /// Queues one chain-head rescan for an owner that already started a weak
    /// scan. A repeat reads the slot tag and does not walk the queue. This
    /// does not scan the owner, reset a mark, initialize an unrelated slot,
    /// allocate a queue buffer, or move the ordinary cursor.
    pub(super) fn schedule_owner_rescan(&mut self, owner: u32) {
        let Some(mut state) = self.mark.take() else {
            return;
        };
        if state.mutation_open()
            && self
                .slots
                .get(owner as usize)
                .is_some_and(|slot| slot.rescan_epoch == state.rescan_epoch)
            && !self.slots[owner as usize].rescan_queued
        {
            push_rescan(self, &mut state, owner);
            self.scan.scratch.owner_rescan_pushed += 1;
            self.scan.owner_rescan_schedules += 1;
        }
        self.mark = Some(state);
    }

    /// Enqueues a metatable stored on an already discovered owner.
    ///
    /// The edge is strong even when that owner caches weak values. Only the
    /// direct metatable is queued; its `__index` child waits for the continued
    /// scan. An absent metatable is not a target. This does not read `__mode`,
    /// scan the owner, restart the cycle, or push an object that is already marked.
    pub(super) fn barrier_strong_metatable(&mut self, owner: u32, slot: u32, generation: u32) {
        let Some(mut state) = self.mark.take() else {
            return;
        };
        if state.mutation_open() && discovered_table(self, owner) {
            enqueue_barrier_target(self, &mut state, slot, generation, HandleKind::Table);
        }
        self.mark = Some(state);
    }

    /// Enqueues the direct object stored through an already discovered closed cell.
    ///
    /// The current edge is strong: a cell is not a table entry, so weak values
    /// do not apply. The caller names the stored table or closure. Only that
    /// object is queued; a closure's capture waits for the continued scan. The
    /// cell itself is not rescanned. This does not add a root, read `__mode`,
    /// or move either cursor. An owner that is not this upvalue generation, or
    /// is not yet marked, is left for the continued scan. A published tail
    /// never reaches here.
    pub(super) fn barrier_strong_closed_cell(
        &mut self,
        owner: u32,
        owner_generation: u32,
        slot: u32,
        generation: u32,
        target: ClosedCellTarget,
    ) {
        let Some(mut state) = self.mark.take() else {
            return;
        };
        if state.mutation_open() && discovered_upvalue(self, owner, owner_generation) {
            let kind = match target {
                ClosedCellTarget::Table => HandleKind::Table,
                ClosedCellTarget::Closure => HandleKind::Closure,
            };
            enqueue_barrier_target(self, &mut state, slot, generation, kind);
        }
        self.mark = Some(state);
    }

    /// Enqueues a newly inserted table-object key, and a strong table value.
    ///
    /// A string value is not a table node. An owner that is not yet marked is
    /// left for the continued scan. Successors stay on the gray queue. This
    /// does not read a metatable or move the pending cursor.
    pub(super) fn barrier_new_table_key(
        &mut self,
        owner: u32,
        key_slot: u32,
        key_generation: u32,
        value: Option<(u32, u32)>,
    ) {
        let Some(mut state) = self.mark.take() else {
            return;
        };
        if state.mutation_open() && discovered_table(self, owner) {
            let mode = cached_owner_mode(self, &state, owner);
            if !mode.keys {
                enqueue_barrier_target(
                    self,
                    &mut state,
                    key_slot,
                    key_generation,
                    HandleKind::Table,
                );
            }
            if !mode.values
                && let Some((slot, generation)) = value
            {
                enqueue_barrier_target(self, &mut state, slot, generation, HandleKind::Table);
            }
        }
        self.mark = Some(state);
    }

    /// Advances a parked gray cursor, a parked rescan cursor, or both, when
    /// `unlink_at` removes that record. Two independent field compares. No
    /// chain, root, or slot walk.
    pub(super) fn repair_parked_cursor(&mut self, owner: u32, index: u32, next: Option<u32>) {
        let Some(state) = self.mark.as_mut() else {
            return;
        };
        if state.owner == Some(owner) && state.cursor == Some(index) {
            if next.is_some() {
                state.cursor = next;
            } else {
                state.owner = None;
                state.cursor = None;
            }
            self.scan.parked_cursor_repairs += 1;
        }
        if state.rescan_owner == Some(owner) && state.rescan_cursor == Some(index) {
            if next.is_some() {
                state.rescan_cursor = next;
            } else {
                state.rescan_owner = None;
                state.rescan_cursor = None;
            }
            self.scan.parked_rescan_cursor_repairs += 1;
        }
    }

    pub(super) fn drain_mark(&mut self) -> MarkSnapshot {
        let mut session = MarkSession::start(self);
        session.drain();
        session.into_snapshot()
    }

    fn mark_new(&mut self, index: u32) -> bool {
        if let SlotState::Live { marked } = &mut self.slots[index as usize].state
            && !*marked
        {
            *marked = true;
            self.scan.scratch.work_pushed += 1;
            return true;
        }
        false
    }
}
