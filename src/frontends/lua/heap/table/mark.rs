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
//! ride with the edge or the owner head. A zero budget does not read the
//! graph. The private pending-finalizer selection adds one phase whose pulse
//! unit is one slot read; only the private selecting entry enables it.
//!
//! Late work is bounded by live sites, not by mutation count: one parse per
//! metatable slot (intrusive queue), one deferred transition check per owner,
//! and one deferred target per edge record (coalesced in place). A known mode
//! is decided by the setter; only a mode still being read queues deferred
//! work, so repeated overwrites of one marked edge do not starve ordinary
//! Gray steps. The two implementation costs are explicit rather than hidden:
//! a late parse stores into the record-keyed map, one O(log L) insertion and
//! one such query per owner head, with L bounded by live metatable slots; a
//! deferred replay pushes one target into the Gray `Vec`, whose geometric
//! growth may copy the current length once at a capacity boundary. Those
//! copies are amortized and total O(live nodes); every ordinary pulse still
//! charges at most one of the listed units.
//!
//! A safepoint root batch is not one of those units. `accept_root_batch` is
//! open in Roots, Gray, or the late sub-phase: phase, then root quota, then
//! every typed handle, before any mark. Those prechecks do not increment the
//! scan meter. Success hands each entry to the existing gray queue. It does
//! not write a root slot, open a frame, copy the batch, or spend pulse budget.
//!
//! A positive `pulse_with_root_batch` refuses Done or a missing mark before
//! it reads the batch. It then uses that same quota and handle check and
//! advances with the ordinary stepper. An accepting phase, including the
//! late sub-phase and a same-pulse entry from Start, Reset, or Mode, commits
//! once before any root, gray, or late step. A budget that runs out before an
//! accepting phase does not commit. The next call, including one after park
//! and resume, reads only the batch it was given. Zero budget only observes.
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
    MarkSnapshot, PendingCandidate, RootFrameId, Rooted, Slot, SlotState, TableRef, UpvalueRef,
    WeakMode,
};
use vm::Value;

/// One open raw-`__gc` lookup in the private pending-finalizer selection.
/// `cursor` is the next metatable edge record to read; one pulse step reads at
/// most that one record.
#[derive(Clone, Copy)]
struct PendingGcLookup {
    candidate: PendingCandidate,
    cursor: Option<u32>,
}

pub(super) struct MarkState {
    phase: MarkPhase,
    gray: Vec<u32>,
    modes: Option<Vec<WeakMode>>,
    /// Marked targets from a deferred replay, linked on their slot records.
    /// They are scanned after the ordinary Gray `Vec` empties and drained
    /// last-in-first-out, so a replay never pushes the shared vector and
    /// never relocates it. At most one node per slot: a target joins only on
    /// the mark-bit transition.
    late_target_head: Option<u32>,
    late_target_tail: Option<u32>,
    index: usize,
    /// Set when the root enumeration has offered the exact registered
    /// userdata reservation to the mark. That reservation is not a guest
    /// value and does not enter `roots`; it is one host-retained edge checked
    /// once per mark after the explicit roots.
    pending_reservation_checked: bool,
    /// Private single-shot entry only: after the ordinary gray, rescan, and
    /// late work empties, scan every slot once for finalizer-eligible
    /// userdata. Each selected userdata is marked through the ordinary target
    /// follow, so the following gray pass traces its metatable graph. An
    /// ordinary mark leaves this false and never charges the selection units.
    select_pending: bool,
    /// The one selection scan finished. A second gray pass may now finish the
    /// mark.
    pending_scan_done: bool,
    /// Next slot the selection scan reads.
    pending_index: usize,
    /// One open raw-`__gc` lookup for the candidate at `pending_index - 1`.
    /// Each step reads at most one metatable edge record and caches the result
    /// for this mark round and exact metatable generation.
    pending_lookup: Option<PendingGcLookup>,
    /// Next pending record offered as a host-retained edge in Roots.
    pending_offer: usize,
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
    /// Slots with a direct `__mode` write that still needs a budgeted read,
    /// in first-write order. The links live on the slots; no index vector is
    /// grown in the late window.
    late_mode_head: Option<u32>,
    late_mode_tail: Option<u32>,
    /// One open parse. It is identified by the slot, generation, edge record
    /// and write version; a different value at any of them restarts at byte
    /// zero rather than mixing two writes.
    late_slot: Option<u32>,
    late_generation: u32,
    late_epoch: u32,
    late_edge: Option<u32>,
    late_byte_at: usize,
    late_pending_mode: WeakMode,
    /// Owners whose metatable transition needs the finished late cache. At
    /// most one record per owner, first-old/final-new.
    late_owner_head: Option<u32>,
    late_owner_tail: Option<u32>,
    /// Edge records with a write that needs the finished late cache. At most
    /// one record per edge, latest live target.
    late_edge_head: Option<u32>,
    late_edge_tail: Option<u32>,
    /// Phase the late window returns to. Set only on entering `ModeLate`.
    late_return: Option<MarkPhase>,
}

/// A write that already happened in the mutation window but whose mode read
/// depends on a late metatable. There is no separate event vector: the state
/// lives on the owner slot or the edge record, so a repeat write coalesces in
/// place and an unlink or release removes it in constant work.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct ModeDiscoveryMark {
    generation: u32,
    batch: u32,
    /// Mark round that armed this stamp. Batch numbers restart at one each
    /// mark, so a same-generation metatable that survives into a later round
    /// must not match an old slot-fallback stamp.
    round: u64,
}

/// Mark phases. `ModeLate` is entered only while the mark is already inside
/// the roots or gray window: a `__mode` field written during that window
/// registers a bounded late parse. The setter only links the slot; one pulse
/// unit opens or restarts the parse, one reads one byte, and one consumes a
/// deferred owner or edge check. No indexed cache is grown.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MarkPhase {
    Start,
    Reset,
    ModeOpen,
    ModeBytes,
    ModeInit,
    Roots,
    Gray,
    Pending,
    ModeLate,
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
            late_target_head: None,
            late_target_tail: None,
            index: 0,
            pending_reservation_checked: false,
            select_pending: false,
            pending_scan_done: false,
            pending_index: 0,
            pending_lookup: None,
            pending_offer: 0,
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
            late_mode_head: None,
            late_mode_tail: None,
            late_slot: None,
            late_generation: 0,
            late_epoch: 0,
            late_edge: None,
            late_byte_at: 0,
            late_pending_mode: WeakMode::default(),
            late_owner_head: None,
            late_owner_tail: None,
            late_edge_head: None,
            late_edge_tail: None,
            late_return: None,
        }
    }

    fn mutation_open(&self) -> bool {
        matches!(self.phase, MarkPhase::Roots | MarkPhase::Gray)
    }

    /// The write hooks stay active through the late-mode sub-phase: a guest
    /// can write between any two pulses, and a late parse is still inside the
    /// roots/gray window rather than a new mark.
    fn write_barriers_open(&self) -> bool {
        matches!(
            self.phase,
            MarkPhase::Roots | MarkPhase::Gray | MarkPhase::ModeLate
        )
    }

    fn has_late_work(&self) -> bool {
        self.late_mode_head.is_some()
            || self.late_owner_head.is_some()
            || self.late_edge_head.is_some()
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
            MarkPhase::Pending => "pending-finalizer",
            MarkPhase::ModeLate => "mode-late",
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

    /// One ordinary mark, plus one selection scan after the ordinary work
    /// empties. Only the private pending-finalizer entry uses this starter.
    /// The scan starts from an empty settlement journal; a previous pulse-only
    /// selecting mark leaves its chain head here, and its flags stay.
    pub(super) fn start_selecting_pending(heap: &'a mut LuaHeap) -> Self {
        heap.clear_selecting_settlements();
        let session = Self::start(heap);
        session
            .heap
            .mark
            .as_mut()
            .expect("start installed the mark state")
            .select_pending = true;
        session
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
        self.heap.sweep_marked(MarkSnapshot {
            modes: state.modes,
            round: state.rescan_epoch,
        })
    }

    pub(super) fn into_snapshot(self) -> MarkSnapshot {
        let state = self.heap.mark.take().expect("drained mark");
        assert!(
            matches!(state.phase, MarkPhase::Done),
            "drain needs a finished mark"
        );
        MarkSnapshot {
            modes: state.modes,
            round: state.rescan_epoch,
        }
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
                round: state.rescan_epoch,
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

    /// Mutation through the whole roots/gray window, including the budgeted
    /// late-mode sub-phase. Used by the write hooks and gated root writers.
    pub(super) fn write_barriers_open(&self) -> bool {
        self.heap
            .mark
            .as_ref()
            .is_some_and(MarkState::write_barriers_open)
    }

    pub(super) fn write_allowed(&self, owner: TableRef) -> bool {
        self.write_barriers_open() && self.heap.alive(owner) && !self.marked(owner)
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
    /// Open in Roots, Gray, or the late sub-phase. Checks run in order: phase,
    /// then `roots.len() + batch.len()` (no overflow, and not above
    /// `limits.roots`; reserved empty slots and duplicate entries count), then
    /// each handle through `require_live`, `require_closure`, or
    /// `require_upvalue`. Those calls keep `WrongDomain`, `WrongKind`, and
    /// `StaleHandle`. The precheck reads are not scan visits. A refusal does
    /// not mark, enqueue, move a cursor, write a root slot, or change a
    /// success meter.
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
        if !self.saved_mark()?.write_barriers_open() {
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
        if !state.write_barriers_open() {
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
                    round: state.rescan_epoch,
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
        if !handed
            && matches!(
                state.phase,
                MarkPhase::Roots | MarkPhase::Gray | MarkPhase::ModeLate
            )
        {
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
    if matches!(state.phase, MarkPhase::Roots | MarkPhase::Gray) && state.has_late_work() {
        state.late_return = Some(state.phase);
        state.phase = MarkPhase::ModeLate;
    }
    match state.phase {
        MarkPhase::Start => false,
        MarkPhase::Reset => step_reset(heap, state),
        MarkPhase::ModeOpen => step_mode_open(heap, state),
        MarkPhase::ModeBytes => step_mode_byte(heap, state),
        MarkPhase::ModeInit => step_mode_init(heap, state),
        MarkPhase::Roots => step_root(heap, state),
        MarkPhase::Gray => step_gray(heap, state),
        MarkPhase::Pending => step_pending(heap, state),
        MarkPhase::ModeLate => step_mode_late(heap, state),
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
        // The record version was inspected here even though its value is not
        // a mode string. First attachment must not re-read this version; a
        // later write still bumps `mode_epoch` and registers a late parse.
        mark_mode_checked(heap, state, state.mode_owner as u32);
        advance_after_mode(state);
    }
    true
}

fn step_mode_byte(heap: &mut LuaHeap, state: &mut MarkState) -> bool {
    let edge_index = state.mode_edge.expect("mode byte has an edge");
    let at = state.byte_at;
    let Some(byte) = mode_string_byte(heap, edge_index, at) else {
        finish_mode_parse(heap, state);
        return false;
    };
    heap.scan.weak_mode_bytes += 1;
    state.byte_at = at + 1;
    match byte {
        b'k' => state.pending_mode.keys = true,
        b'v' => state.pending_mode.values = true,
        0 => {
            finish_mode_parse(heap, state);
            return true;
        }
        _ => {}
    }
    let len = mode_string_len(heap, edge_index).unwrap_or(0);
    if state.byte_at >= len {
        finish_mode_parse(heap, state);
    }
    true
}

fn finish_mode_parse(heap: &mut LuaHeap, state: &mut MarkState) {
    let parsed = state.pending_mode;
    mark_mode_checked(heap, state, state.mode_owner as u32);
    if parsed.keys || parsed.values {
        if state.modes.is_none() {
            state.init_at = 0;
            state.phase = MarkPhase::ModeInit;
            return;
        }
        store_mode(heap, state, parsed);
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
        store_mode(heap, state, parsed);
        advance_after_mode(state);
    }
    true
}

/// Store one parsed mode for the generation that owned the read. The reset
/// walk writes into the indexed vector it sized. A parse outside that vector
/// writes the slot's own weak bits; no record-keyed tree is touched, and the
/// slot generation and this mark's round travel with the bits as the parse
/// identity.
fn store_cache_mode(
    heap: &mut LuaHeap,
    state: &mut MarkState,
    slot: u32,
    generation: u32,
    parsed: WeakMode,
) {
    let in_indexed = state
        .modes
        .as_ref()
        .is_some_and(|modes| (slot as usize) < modes.len());
    match heap.slots.get_mut(slot as usize) {
        Some(stored) => {
            stored.mode_generation = generation;
            stored.mode_round = state.rescan_epoch;
            if !in_indexed {
                stored.mode_weak = parsed;
            }
        }
        None => return,
    }
    if in_indexed && let Some(modes) = state.modes.as_mut() {
        modes[slot as usize] = parsed;
    }
}

fn store_mode(heap: &mut LuaHeap, state: &mut MarkState, parsed: WeakMode) {
    let slot = state.mode_owner as u32;
    let generation = heap.slots[state.mode_owner].generation;
    store_cache_mode(heap, state, slot, generation, parsed);
}

/// Records the exact direct `__mode` record version inspected by this round,
/// either by the reset walk or by a finished late read. The read consumes the
/// string even when it names no weak side, so this stamp is what tells a later
/// first attachment that this field version is already inspected. It
/// deliberately does not touch `mode_generation`, `mode_round`, or the weak
/// bits: those remain the parsed weak-mode cache, and a write after this read
/// must still admit a late parse that sees, for example, an `x -> v` rewrite
/// before the owner opens.
fn mark_mode_checked(heap: &mut LuaHeap, state: &MarkState, slot: u32) {
    let Some(stored) = heap.slots.get_mut(slot as usize) else {
        return;
    };
    stored.mode_checked_generation = stored.generation;
    stored.mode_checked_round = state.rescan_epoch;
    stored.mode_checked_epoch = stored.mode_epoch;
}

fn advance_after_mode(state: &mut MarkState) {
    state.mode_edge = None;
    state.index += 1;
    state.phase = MarkPhase::Reset;
}

/// One budget unit of the late window. Every queued mode is parsed before any
/// deferred owner or edge check. Each call charges at most one open/restart,
/// one byte, or one deferred event. The queues are intrusive on the records,
/// so a new registration is a constant append and a release unlink is
/// constant work.
fn step_mode_late(heap: &mut LuaHeap, state: &mut MarkState) -> bool {
    if state.late_mode_head.is_some() {
        return step_late_mode(heap, state);
    }
    if let Some((owner, old_tuple, new_tuple, old_mode)) = pop_late_owner(heap, state) {
        apply_owner_check(heap, state, owner, old_tuple, new_tuple, old_mode);
        return true;
    }
    if let Some((index, owner, key, value)) = pop_late_edge(heap, state) {
        apply_edge_check(heap, state, index, owner, key, value);
        return true;
    }
    state.phase = state.late_return.take().unwrap_or(MarkPhase::Roots);
    false
}

/// Open, restart, or advance the head parse by one unit. The parse identity
/// includes the slot generation, the current `__mode` edge record, and that
/// record's write version. A rewrite or unlink changes at least one of them,
/// so the reader resets its cursor and pending bits instead of combining the
/// new bytes with the old prefix or length.
fn step_late_mode(heap: &mut LuaHeap, state: &mut MarkState) -> bool {
    let Some(slot) = state.late_mode_head else {
        return false;
    };
    let Some(stored) = heap.slots.get(slot as usize) else {
        pop_late_mode(heap, state);
        return true;
    };
    if stored.kind != HandleKind::Table || !matches!(stored.state, SlotState::Live { .. }) {
        pop_late_mode(heap, state);
        return true;
    }
    let generation = stored.generation;
    let edge = stored.mode_edge;
    let epoch = stored.mode_epoch;
    if state.late_slot != Some(slot)
        || state.late_generation != generation
        || state.late_edge != edge
        || state.late_epoch != epoch
    {
        state.late_slot = Some(slot);
        state.late_generation = generation;
        state.late_edge = edge;
        state.late_epoch = epoch;
        state.late_byte_at = 0;
        state.late_pending_mode = WeakMode::default();
        heap.scan.weak_mode_edge_checks += 1;
        if edge.is_none() {
            finish_late_mode(heap, state, WeakMode::default());
        }
        return true;
    }
    let Some(edge) = edge else {
        finish_late_mode(heap, state, WeakMode::default());
        return true;
    };
    let at = state.late_byte_at;
    let Some(byte) = mode_string_byte(heap, edge, at) else {
        finish_late_mode(heap, state, state.late_pending_mode);
        return true;
    };
    heap.scan.weak_mode_bytes += 1;
    state.late_byte_at = at + 1;
    match byte {
        b'k' => state.late_pending_mode.keys = true,
        b'v' => state.late_pending_mode.values = true,
        0 => {
            finish_late_mode(heap, state, state.late_pending_mode);
            return true;
        }
        _ => {}
    }
    let len = mode_string_len(heap, edge).unwrap_or(0);
    if state.late_byte_at >= len {
        finish_late_mode(heap, state, state.late_pending_mode);
    }
    true
}

fn push_late_mode(heap: &mut LuaHeap, state: &mut MarkState, slot: u32) {
    {
        let stored = &mut heap.slots[slot as usize];
        stored.late_mode_epoch = state.rescan_epoch;
        stored.late_mode_prev = state.late_mode_tail;
        stored.late_mode_next = None;
    }
    if let Some(tail) = state.late_mode_tail {
        heap.slots[tail as usize].late_mode_next = Some(slot);
    } else {
        state.late_mode_head = Some(slot);
    }
    state.late_mode_tail = Some(slot);
}

fn pop_late_mode(heap: &mut LuaHeap, state: &mut MarkState) -> Option<u32> {
    let slot = state.late_mode_head?;
    let next = heap
        .slots
        .get(slot as usize)
        .and_then(|stored| stored.late_mode_next);
    if let Some(stored) = heap.slots.get_mut(slot as usize) {
        stored.late_mode_epoch = 0;
        stored.late_mode_prev = None;
        stored.late_mode_next = None;
    }
    state.late_mode_head = next;
    match next {
        Some(next) => {
            if let Some(stored) = heap.slots.get_mut(next as usize) {
                stored.late_mode_prev = None;
            }
        }
        None => state.late_mode_tail = None,
    }
    Some(slot)
}

fn push_late_owner(heap: &mut LuaHeap, state: &mut MarkState, owner: u32) {
    {
        let stored = &mut heap.slots[owner as usize];
        stored.late_owner_epoch = state.rescan_epoch;
        stored.late_owner_prev = state.late_owner_tail;
        stored.late_owner_next = None;
    }
    if let Some(tail) = state.late_owner_tail {
        heap.slots[tail as usize].late_owner_next = Some(owner);
    } else {
        state.late_owner_head = Some(owner);
    }
    state.late_owner_tail = Some(owner);
}

#[allow(clippy::type_complexity)]
fn pop_late_owner(
    heap: &mut LuaHeap,
    state: &mut MarkState,
) -> Option<(
    u32,
    Option<(u32, u32)>,
    Option<(u32, u32)>,
    Option<WeakMode>,
)> {
    let owner = state.late_owner_head?;
    let stored = heap.slots.get(owner as usize)?;
    let old_tuple = stored.late_owner_old;
    let old_mode = stored.late_owner_old_mode;
    let new_tuple = stored.late_owner_new;
    let next = stored.late_owner_next;
    {
        let stored = &mut heap.slots[owner as usize];
        stored.late_owner_epoch = 0;
        stored.late_owner_old = None;
        stored.late_owner_old_mode = None;
        stored.late_owner_new = None;
        stored.late_owner_prev = None;
        stored.late_owner_next = None;
    }
    state.late_owner_head = next;
    match next {
        Some(next) => {
            if let Some(stored) = heap.slots.get_mut(next as usize) {
                stored.late_owner_prev = None;
            }
        }
        None => state.late_owner_tail = None,
    }
    Some((owner, old_tuple, new_tuple, old_mode))
}

fn push_late_edge(heap: &mut LuaHeap, state: &mut MarkState, index: u32) {
    {
        let record = &mut heap.edge_slots[index as usize];
        record.late_epoch = state.rescan_epoch;
        record.late_prev = state.late_edge_tail;
        record.late_next = None;
    }
    if let Some(tail) = state.late_edge_tail {
        heap.edge_slots[tail as usize].late_next = Some(index);
    } else {
        state.late_edge_head = Some(index);
    }
    state.late_edge_tail = Some(index);
}

#[allow(clippy::type_complexity)]
fn pop_late_edge(
    heap: &mut LuaHeap,
    state: &mut MarkState,
) -> Option<(u32, u32, Option<(u32, u32)>, Option<(u32, u32)>)> {
    let index = state.late_edge_head?;
    let record = heap.edge_slots.get(index as usize)?;
    let owner = record.late_owner;
    let key = record.late_key;
    let value = record.late_value;
    let next = record.late_next;
    {
        let record = &mut heap.edge_slots[index as usize];
        record.late_epoch = 0;
        record.late_owner = 0;
        record.late_value = None;
        record.late_key = None;
        record.late_prev = None;
        record.late_next = None;
    }
    state.late_edge_head = next;
    match next {
        Some(next) => {
            if let Some(record) = heap.edge_slots.get_mut(next as usize) {
                record.late_prev = None;
            }
        }
        None => state.late_edge_tail = None,
    }
    Some((index, owner, key, value))
}

fn finish_late_mode(heap: &mut LuaHeap, state: &mut MarkState, parsed: WeakMode) {
    let Some(slot) = state.late_slot else {
        return;
    };
    let generation = state.late_generation;
    pop_late_mode(heap, state);
    state.late_slot = None;
    state.late_byte_at = 0;
    state.late_pending_mode = WeakMode::default();
    if parsed.keys || parsed.values {
        store_parsed_mode(heap, state, slot, generation, parsed, true);
    } else {
        // A strong late result is a field observation, not an authoritative
        // weak cache. It ends the wait for this version and lets a same-version
        // reattachment skip a second read, but `mode_epoch` moves on the next
        // write, so that write still registers a new late parse.
        mark_mode_checked(heap, state, slot);
        heap.scan.mode_late_parsed += 1;
    }
}

/// Store one parsed mode with the generation that owned the read. A late
/// value-weak entry is replaced at its real slot, and the generation stamp
/// makes a later reuse read as strong rather than inheriting the old mode.
fn store_parsed_mode(
    heap: &mut LuaHeap,
    state: &mut MarkState,
    slot: u32,
    generation: u32,
    parsed: WeakMode,
    late: bool,
) {
    store_cache_mode(heap, state, slot, generation, parsed);
    if late {
        heap.scan.mode_late_parsed += 1;
    }
}

fn apply_owner_check(
    heap: &mut LuaHeap,
    state: &mut MarkState,
    owner: u32,
    old_tuple: Option<(u32, u32)>,
    new_tuple: Option<(u32, u32)>,
    observed_old_mode: Option<WeakMode>,
) {
    let live = heap.slots.get(owner as usize).is_some_and(|stored| {
        stored.kind == HandleKind::Table && matches!(stored.state, SlotState::Live { .. })
    });
    // The coalesced record keeps the first old tuple and the latest new one.
    // Only apply it while the owner really points at that final attachment;
    // a later setter updates the record, so a mismatch is not this check's
    // observation.
    let final_attachment = live
        && heap
            .slots
            .get(owner as usize)
            .is_some_and(|stored| stored.metatable == new_tuple);
    if final_attachment && old_tuple != new_tuple {
        let old_mode = observed_old_mode.unwrap_or_else(|| {
            mode_before_direct_delete(heap, state, old_tuple)
                .unwrap_or_else(|| mode_of_tuple(heap, state, old_tuple))
        });
        let new_mode = mode_of_tuple(heap, state, new_tuple);
        if (old_mode.keys && !new_mode.keys) || (old_mode.values && !new_mode.values) {
            schedule_started_owner(heap, state, owner);
        }
    }
    heap.scan.mode_late_deferred += 1;
}

fn apply_edge_check(
    heap: &mut LuaHeap,
    state: &mut MarkState,
    _index: u32,
    owner: u32,
    key: Option<(u32, u32)>,
    value: Option<(u32, u32)>,
) {
    if discovered_table(heap, owner) {
        let mode = cached_owner_mode(heap, state, owner);
        if !mode.keys
            && let Some((slot, generation)) = key
        {
            late_enqueue_target(heap, state, slot, generation, HandleKind::Table);
        }
        if !mode.values
            && let Some((slot, generation)) = value
        {
            late_enqueue_target(heap, state, slot, generation, HandleKind::Table);
        }
    }
    heap.scan.mode_late_deferred += 1;
}

fn clear_late_slot(stored: &mut Slot) {
    stored.late_mode_epoch = 0;
    stored.late_mode_prev = None;
    stored.late_mode_next = None;
    stored.late_owner_epoch = 0;
    stored.late_owner_old = None;
    stored.late_owner_old_mode = None;
    stored.late_owner_new = None;
    stored.late_owner_prev = None;
    stored.late_owner_next = None;
    stored.late_target_epoch = 0;
    stored.late_target_prev = None;
    stored.late_target_next = None;
}

fn clear_late_edge(record: &mut super::EdgeSlot) {
    record.late_epoch = 0;
    record.late_owner = 0;
    record.late_value = None;
    record.late_key = None;
    record.late_prev = None;
    record.late_next = None;
}

/// Remove a released slot from both intrusive late queues. A tag from an
/// older epoch is not in the current list. A released slot must never stay
/// queued: reuse would otherwise inherit queue membership or parse state.
pub(super) fn release_late_slot(state: Option<&mut MarkState>, slots: &mut [Slot], index: u32) {
    let Some(state) = state else {
        if let Some(stored) = slots.get_mut(index as usize) {
            clear_late_slot(stored);
        }
        return;
    };
    let epoch = state.rescan_epoch;
    let Some(stored) = slots.get(index as usize) else {
        return;
    };
    let mode_epoch = stored.late_mode_epoch;
    let mode_prev = stored.late_mode_prev;
    let mode_next = stored.late_mode_next;
    let owner_epoch = stored.late_owner_epoch;
    let owner_prev = stored.late_owner_prev;
    let owner_next = stored.late_owner_next;
    let target_epoch = stored.late_target_epoch;
    let target_prev = stored.late_target_prev;
    let target_next = stored.late_target_next;
    if mode_epoch == epoch {
        let prev = mode_prev;
        let next = mode_next;
        match prev {
            Some(prev) => {
                if let Some(prev) = slots.get_mut(prev as usize) {
                    prev.late_mode_next = next;
                }
            }
            None => state.late_mode_head = next,
        }
        match next {
            Some(next) => {
                if let Some(next) = slots.get_mut(next as usize) {
                    next.late_mode_prev = prev;
                }
            }
            None => state.late_mode_tail = prev,
        }
        if state.late_slot == Some(index) {
            state.late_slot = None;
            state.late_edge = None;
            state.late_byte_at = 0;
            state.late_pending_mode = WeakMode::default();
        }
    }
    if owner_epoch == epoch {
        let prev = owner_prev;
        let next = owner_next;
        match prev {
            Some(prev) => {
                if let Some(prev) = slots.get_mut(prev as usize) {
                    prev.late_owner_next = next;
                }
            }
            None => state.late_owner_head = next,
        }
        match next {
            Some(next) => {
                if let Some(next) = slots.get_mut(next as usize) {
                    next.late_owner_prev = prev;
                }
            }
            None => state.late_owner_tail = prev,
        }
    }
    if target_epoch == epoch {
        let prev = target_prev;
        let next = target_next;
        match prev {
            Some(prev) => {
                if let Some(prev) = slots.get_mut(prev as usize) {
                    prev.late_target_next = next;
                }
            }
            None => state.late_target_head = next,
        }
        match next {
            Some(next) => {
                if let Some(next) = slots.get_mut(next as usize) {
                    next.late_target_prev = prev;
                }
            }
            None => state.late_target_tail = prev,
        }
    }
    if let Some(stored) = slots.get_mut(index as usize) {
        clear_late_slot(stored);
    }
}

/// Remove one released edge record from the deferred-edge queue.
pub(super) fn release_late_edge(
    mark: Option<&mut MarkState>,
    edge_slots: &mut [super::EdgeSlot],
    index: u32,
) {
    let Some(state) = mark else {
        if let Some(record) = edge_slots.get_mut(index as usize) {
            clear_late_edge(record);
        }
        return;
    };
    let epoch = state.rescan_epoch;
    let Some(record) = edge_slots.get(index as usize) else {
        return;
    };
    if record.late_epoch == epoch {
        let prev = record.late_prev;
        let next = record.late_next;
        match prev {
            Some(prev) => {
                if let Some(prev) = edge_slots.get_mut(prev as usize) {
                    prev.late_next = next;
                }
            }
            None => state.late_edge_head = next,
        }
        match next {
            Some(next) => {
                if let Some(next) = edge_slots.get_mut(next as usize) {
                    next.late_prev = prev;
                }
            }
            None => state.late_edge_tail = prev,
        }
    }
    if let Some(record) = edge_slots.get_mut(index as usize) {
        clear_late_edge(record);
    }
}

/// Record (or update in place) the one deferred transition check for an
/// owner. First old wins, latest new wins. No pending list is walked.
fn queue_owner_check(
    heap: &mut LuaHeap,
    state: &mut MarkState,
    owner: u32,
    old_tuple: Option<(u32, u32)>,
    new_tuple: Option<(u32, u32)>,
) {
    heap.scan.mode_late_lookups += 1;
    if heap.slots[owner as usize].late_owner_epoch == state.rescan_epoch {
        heap.slots[owner as usize].late_owner_new = new_tuple;
        heap.scan.mode_late_event_reuses += 1;
        return;
    }
    // Capture the old observation while the old metatable can still be
    // resolved. A release before the deferred check must not turn an
    // observed weak side into an apparent strong one. An old mode that is
    // itself still being read stays unresolved; the parse queue drains before
    // this check runs.
    let old_mode = if late_mode_unknown(heap, state, old_tuple) {
        None
    } else {
        Some(
            mode_before_direct_delete(heap, state, old_tuple)
                .unwrap_or_else(|| mode_of_tuple(heap, state, old_tuple)),
        )
    };
    let stored = &mut heap.slots[owner as usize];
    stored.late_owner_old = old_tuple;
    stored.late_owner_old_mode = old_mode;
    stored.late_owner_new = new_tuple;
    push_late_owner(heap, state, owner);
    heap.scan.mode_late_events += 1;
}

fn remove_late_edge(heap: &mut LuaHeap, state: &mut MarkState, index: u32) {
    let Some(record) = heap.edge_slots.get(index as usize) else {
        return;
    };
    if record.late_epoch != state.rescan_epoch {
        return;
    }
    let prev = record.late_prev;
    let next = record.late_next;
    match prev {
        Some(prev) => {
            if let Some(prev) = heap.edge_slots.get_mut(prev as usize) {
                prev.late_next = next;
            }
        }
        None => state.late_edge_head = next,
    }
    match next {
        Some(next) => {
            if let Some(next) = heap.edge_slots.get_mut(next as usize) {
                next.late_prev = prev;
            }
        }
        None => state.late_edge_tail = prev,
    }
    if let Some(record) = heap.edge_slots.get_mut(index as usize) {
        record.late_epoch = 0;
        record.late_owner = 0;
        record.late_value = None;
        record.late_key = None;
        record.late_prev = None;
        record.late_next = None;
    }
}

/// Update one edge record's latest table-value target. An existing queued
/// event is changed in place; when both key and value targets are gone, the
/// record leaves the queue.
fn queue_edge_value(
    heap: &mut LuaHeap,
    state: &mut MarkState,
    index: u32,
    owner: u32,
    target: Option<(u32, u32)>,
) {
    heap.scan.mode_late_lookups += 1;
    let queued = heap.edge_slots[index as usize].late_epoch == state.rescan_epoch;
    if queued {
        heap.edge_slots[index as usize].late_value = target;
        heap.scan.mode_late_event_reuses += 1;
        if target.is_none() && heap.edge_slots[index as usize].late_key.is_none() {
            remove_late_edge(heap, state, index);
        }
        return;
    }
    let Some(target) = target else {
        return;
    };
    {
        let record = &mut heap.edge_slots[index as usize];
        record.late_owner = owner;
        record.late_value = Some(target);
        record.late_key = None;
    }
    push_late_edge(heap, state, index);
    heap.scan.mode_late_events += 1;
}

/// Updates the payload of an edge record that is already in this mark's
/// deferred-edge queue. A known-mode overwrite must not replay a target that
/// the parser had queued before the overwrite: the record is changed in place,
/// or leaves the queue when both its value and key are gone. This never
/// creates a second event and does not route the setter through a tree.
fn update_queued_edge_value(
    heap: &mut LuaHeap,
    state: &mut MarkState,
    index: u32,
    target: Option<(u32, u32)>,
) {
    if heap.edge_slots[index as usize].late_epoch != state.rescan_epoch {
        return;
    }
    heap.edge_slots[index as usize].late_value = target;
    heap.scan.mode_late_event_reuses += 1;
    if target.is_none() && heap.edge_slots[index as usize].late_key.is_none() {
        remove_late_edge(heap, state, index);
    }
}

/// Record or merge a new table-object key and its table value on one edge.
fn queue_edge_key(
    heap: &mut LuaHeap,
    state: &mut MarkState,
    index: u32,
    owner: u32,
    key: (u32, u32),
    value: Option<(u32, u32)>,
) {
    heap.scan.mode_late_lookups += 1;
    if heap.edge_slots[index as usize].late_epoch == state.rescan_epoch {
        let record = &mut heap.edge_slots[index as usize];
        record.late_owner = owner;
        record.late_key = Some(key);
        record.late_value = value;
        heap.scan.mode_late_event_reuses += 1;
        return;
    }
    {
        let record = &mut heap.edge_slots[index as usize];
        record.late_owner = owner;
        record.late_key = Some(key);
        record.late_value = value;
    }
    push_late_edge(heap, state, index);
    heap.scan.mode_late_events += 1;
}

/// Whether `tuple` names a metatable with a queued late parse. A parsed cache
/// entry is already readable; a queued marker is not.
fn late_mode_unknown(heap: &LuaHeap, state: &MarkState, tuple: Option<(u32, u32)>) -> bool {
    let Some((slot, generation)) = tuple else {
        return false;
    };
    heap.slots.get(slot as usize).is_some_and(|stored| {
        stored.generation == generation
            && stored.kind == HandleKind::Table
            && stored.late_mode_epoch == state.rescan_epoch
    })
}

/// Reads the owner's current metatable tuple through the slot vector. No
/// follower, no mark step, and no scan counter.
fn owner_mode_unknown(heap: &LuaHeap, state: &MarkState, owner: u32) -> bool {
    let tuple = heap
        .slots
        .get(owner as usize)
        .and_then(|slot| slot.metatable);
    late_mode_unknown(heap, state, tuple)
}

/// Queues one owner that already started a weak scan. Same checks and meters
/// as the synchronous setter path, without taking the saved mark.
fn schedule_started_owner(heap: &mut LuaHeap, state: &mut MarkState, owner: u32) {
    if heap
        .slots
        .get(owner as usize)
        .is_some_and(|slot| slot.rescan_epoch == state.rescan_epoch)
        && !heap.slots[owner as usize].rescan_queued
    {
        push_rescan(heap, state, owner);
        heap.scan.scratch.owner_rescan_pushed += 1;
        heap.scan.owner_rescan_schedules += 1;
    }
}

fn step_root(heap: &mut LuaHeap, state: &mut MarkState) -> bool {
    if state.index >= heap.roots.len() {
        // The exact registered userdata reservation is a host-retained edge,
        // not a guest root: offer it once after the explicit roots. An empty
        // (unregistered) reservation is not retained. The record and slot
        // identity reads are one node-slot visit; the ordinary mark update
        // and any owner scan follow the same units as a rooted node.
        if !state.pending_reservation_checked {
            state.pending_reservation_checked = true;
            if let Some(slot) = heap.registered_pending_userdata_slot() {
                heap.scan.node_slot_visits += 1;
                if heap.mark_new(slot) {
                    state.gray.push(slot);
                }
                return true;
            }
        }
        // Pending finalizers are host-retained edges, not guest roots. Each
        // record is offered once, after the explicit roots and the registered
        // reservation. The record check is one same-slot identity read; a
        // valid record is marked like a root. A record that no longer names
        // its exact live generation is skipped without closing anything.
        if state.pending_offer < heap.pending_finalizer_count() {
            let index = state.pending_offer;
            state.pending_offer += 1;
            heap.scan.pending_finalizer_offers += 1;
            if let Some((slot, _generation)) = heap.pending_finalizer_retention(index) {
                heap.scan.node_slot_visits += 1;
                if heap.mark_new(slot) {
                    state.gray.push(slot);
                }
            }
            return true;
        }
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
    // Deferred replay targets come after the ordinary Gray stack. They are
    // linked on their slots and drained last-in-first-out, matching the LIFO
    // order the shared `Vec` had for these pushes; a replay never touches
    // that `Vec`. Ordinary work is always preferred, so a bounded replay
    // backlog cannot starve it; one pulse opens at most one owner head here.
    if let Some(slot) = pop_late_target(heap, state) {
        return open_owner(heap, state, slot);
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
    if state.select_pending && !state.pending_scan_done {
        state.phase = MarkPhase::Pending;
        return false;
    }
    state.phase = MarkPhase::Done;
    false
}

/// One slot of the private pending-finalizer selection scan, or one edge
/// record of the open raw-`__gc` lookup.
///
/// The scan runs only after the ordinary gray, rescan, and late work has
/// emptied and only when the private entry asked for it. Each slot step reads
/// one slot record, charged as one node-slot visit and one pending selection
/// slot. A candidate then opens a metered lookup whose each step reads at most
/// one metatable edge record; the result is cached on the exact metatable
/// generation for this mark round. A positive result records the candidate and
/// marks it through the ordinary `follow_one` path; the following Gray pass
/// opens it as an owner and follows its metatable. A negative result settles
/// that exact white identity without marking it or queueing it; a cache hit
/// applies the same result to each later candidate that shares the metatable,
/// so every eligible white identity is settled. The scan itself never opens an
/// owner, walks a whole chain in one step, or clears a weak entry. The whole
/// batch is read before Gray starts, so a metatable edge cannot mark a later
/// candidate early. When the slot vector and every lookup finish, the phase
/// returns to Gray; that transition is uncharged.
fn step_pending(heap: &mut LuaHeap, state: &mut MarkState) -> bool {
    if state.pending_lookup.is_some() {
        return step_pending_gc(heap, state);
    }
    if state.pending_index >= heap.slots.len() {
        state.pending_scan_done = true;
        state.phase = MarkPhase::Gray;
        return false;
    }
    let index = state.pending_index;
    state.pending_index += 1;
    heap.scan.pending_finalizer_scan_slots += 1;
    heap.scan.node_slot_visits += 1;
    let Some(candidate) = heap.pending_finalizer_candidate(index) else {
        return true;
    };
    if let Some(non_nil) = heap.cached_pending_gc(&candidate, state.rescan_epoch) {
        apply_pending_gc(heap, state, &candidate, non_nil);
        return true;
    }
    let cursor = heap.slots[candidate.metatable_slot as usize].edge_head;
    heap.scan.pending_finalizer_gc_lookups += 1;
    state.pending_lookup = Some(PendingGcLookup { candidate, cursor });
    true
}

/// Applies one completed raw-`__gc` result to the exact candidate.
///
/// A non-nil field is the existing queue path: the candidate gets one pending
/// record and a mark through the ordinary target follow. A missing or nil
/// field settles the identity instead: no record and no mark. Both outcomes
/// re-check the slot identity in their heap method; a candidate that changed
/// during the walk is not selected or settled.
fn apply_pending_gc(
    heap: &mut LuaHeap,
    state: &mut MarkState,
    candidate: &super::PendingCandidate,
    non_nil: bool,
) {
    if non_nil {
        if heap.commit_pending_finalizer(candidate) {
            heap.scan.pending_finalizers_selected += 1;
            let _matched = follow_one(
                heap,
                state,
                candidate.slot,
                candidate.generation,
                HandleKind::Userdata,
            );
        }
    } else {
        let _settled = heap.settle_pending_finalizer(candidate);
    }
}

/// One edge record of the open raw-`__gc` lookup.
///
/// The first matching record decides: a missing field has already ended the
/// walk as negative, a stored nil or cursor gap is negative, and every other
/// value (including `false`) is positive. The result is stored on the
/// metatable slot for this round before the candidate is committed, so a
/// second candidate sharing that exact generation pays no second walk.
fn step_pending_gc(heap: &mut LuaHeap, state: &mut MarkState) -> bool {
    let lookup = state.pending_lookup.expect("an open lookup was observed");
    let Some(index) = lookup.cursor else {
        heap.cache_pending_gc(
            lookup.candidate.metatable_slot,
            lookup.candidate.metatable_generation,
            state.rescan_epoch,
            false,
        );
        state.pending_lookup = None;
        apply_pending_gc(heap, state, &lookup.candidate, false);
        return true;
    };
    let (next, matched, non_nil) = heap.pending_gc_record(index);
    heap.scan.pending_finalizer_gc_edge_checks += 1;
    #[cfg(test)]
    heap.pending_gc_edge_trace
        .borrow_mut()
        .push((lookup.candidate.metatable_slot, index));
    if matched {
        heap.cache_pending_gc(
            lookup.candidate.metatable_slot,
            lookup.candidate.metatable_generation,
            state.rescan_epoch,
            non_nil,
        );
        state.pending_lookup = None;
        apply_pending_gc(heap, state, &lookup.candidate, non_nil);
        return true;
    }
    state.pending_lookup = Some(PendingGcLookup {
        cursor: next,
        ..lookup
    });
    true
}

fn begin_owner(heap: &mut LuaHeap, state: &mut MarkState) -> bool {
    let Some(slot) = state.gray.pop() else {
        state.phase = MarkPhase::Done;
        return false;
    };
    open_owner(heap, state, slot)
}

/// Opens one marked slot as the current owner. One caller unit. Shared by the
/// ordinary Gray stack and the deferred-replay list.
fn open_owner(heap: &mut LuaHeap, state: &mut MarkState, slot: u32) -> bool {
    let Some(stored) = heap.slots.get(slot as usize) else {
        return false;
    };
    if !matches!(stored.state, SlotState::Live { .. }) {
        return false;
    }
    let kind = stored.kind;
    let userdata_metatable = if kind == HandleKind::Userdata {
        stored.metatable
    } else {
        None
    };
    let mode = if kind == HandleKind::Table && heap.metatable_edges > 0 {
        owner_mode(heap, state, slot)
    } else {
        WeakMode::default()
    };
    // A userdata metatable is a strong edge of the marked userdata. Following
    // it rides on this same owner-head unit, exactly as a table's metatable
    // check rides on the table head. The test-only anti-control can suppress
    // this follow for a selected pending userdata; the userdata itself stays
    // marked and retained in that arm.
    #[cfg(test)]
    let skip_metatable =
        heap.skip_pending_finalizer_metatable && heap.slots[slot as usize].pending_finalizer;
    #[cfg(not(test))]
    let skip_metatable = false;
    if let Some((metatable_slot, metatable_generation)) = userdata_metatable
        && heap.metatable_edges > 0
        && !skip_metatable
    {
        heap.scan.metatable_edge_checks += 1;
        let _matched = follow_one(
            heap,
            state,
            metatable_slot,
            metatable_generation,
            HandleKind::Table,
        );
    }
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
    // An open rescan has a cached mode. A same-tuple rewrite or a late parse
    // on that metatable does not change the owner's metatable tuple, so the
    // tuple comparison used to miss it. Re-read the resolved mode on every
    // charged record; this is two slot reads and does not touch the meter.
    // The late parse queue drains before Gray resumes, so the slot bits read
    // here are final for this round.
    if let Some(owner) = state.rescan_owner {
        state.rescan_mode = cached_owner_mode(heap, state, owner);
    }
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

fn push_late_target(heap: &mut LuaHeap, state: &mut MarkState, slot: u32) {
    {
        let stored = &mut heap.slots[slot as usize];
        stored.late_target_epoch = state.rescan_epoch;
        stored.late_target_prev = None;
        stored.late_target_next = state.late_target_head;
    }
    match state.late_target_head {
        Some(head) => heap.slots[head as usize].late_target_prev = Some(slot),
        None => state.late_target_tail = Some(slot),
    }
    state.late_target_head = Some(slot);
}

fn pop_late_target(heap: &mut LuaHeap, state: &mut MarkState) -> Option<u32> {
    let slot = state.late_target_head?;
    let next = heap
        .slots
        .get(slot as usize)
        .and_then(|stored| stored.late_target_next);
    if let Some(stored) = heap.slots.get_mut(slot as usize) {
        stored.late_target_epoch = 0;
        stored.late_target_prev = None;
        stored.late_target_next = None;
    }
    state.late_target_head = next;
    match next {
        Some(next) => {
            if let Some(stored) = heap.slots.get_mut(next as usize) {
                stored.late_target_prev = None;
            }
        }
        None => state.late_target_tail = None,
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
    let Some(stamp) = discovery_stamp(heap, state, meta_slot) else {
        return true;
    };
    let active = ModeDiscoveryMark {
        generation: meta_gen,
        batch: state.discover_batch,
        round: state.rescan_epoch,
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
/// slot per mark. The stamp lives in the reset-sized sidecar below its length
/// and on the metatable slot beyond it, so no late vector growth is needed.
/// A stale generation or an unexpected path does not panic the collector.
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
    let stamp = ModeDiscoveryMark {
        generation,
        batch,
        round: state.rescan_epoch,
    };
    if let Some(found) = state.discover_marks.get_mut(slot as usize) {
        *found = stamp;
    } else if let Some(stored) = heap.slots.get_mut(slot as usize) {
        stored.discover_generation = generation;
        stored.discover_batch = batch;
        stored.discover_round = state.rescan_epoch;
    } else {
        return;
    }
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

/// Parsed mode for one metatable slot generation. The indexed reset vector is
/// read when it covers the slot; otherwise the slot's own weak bits are read.
/// Both carry the generation and the round stamp written at the parse's
/// linearization point, so a released/reused slot or a cache left by an
/// earlier round cannot be a current observation of the direct field. The
/// late parse therefore costs one slot write and every query is a slot read,
/// not a tree insert or lookup.
fn mode_for_slot(
    heap: &LuaHeap,
    state: &MarkState,
    slot: u32,
    generation: u32,
) -> Option<WeakMode> {
    let stored = heap.slots.get(slot as usize)?;
    if stored.generation != generation
        || stored.mode_generation != generation
        || stored.mode_round != state.rescan_epoch
    {
        return None;
    }
    if let Some(modes) = state.modes.as_ref()
        && let Some(mode) = modes.get(slot as usize)
    {
        return Some(*mode);
    }
    Some(stored.mode_weak)
}

fn mode_of_tuple(heap: &LuaHeap, state: &MarkState, tuple: Option<(u32, u32)>) -> WeakMode {
    let Some((slot, generation)) = tuple else {
        return WeakMode::default();
    };
    mode_for_slot(heap, state, slot, generation).unwrap_or_default()
}

fn discovery_stamp(heap: &LuaHeap, state: &MarkState, slot: u32) -> Option<ModeDiscoveryMark> {
    if let Some(stamp) = state.discover_marks.get(slot as usize) {
        return Some(*stamp);
    }
    heap.slots
        .get(slot as usize)
        .map(|stored| ModeDiscoveryMark {
            generation: stored.discover_generation,
            batch: stored.discover_batch,
            round: stored.discover_round,
        })
}

/// The value-weak mode a tuple had before a direct `__mode` unlink replaced
/// the cache entry with the missing-field mode. The discovery stamp recorded
/// the same metatable generation, so a setter moving an already-started owner
/// away still sees the old weak side. Only the value bit returns: the delete
/// path arms on value-weak, key-strong entries.
fn mode_before_direct_delete(
    heap: &LuaHeap,
    state: &MarkState,
    tuple: Option<(u32, u32)>,
) -> Option<WeakMode> {
    let (slot, generation) = tuple?;
    let stamp = discovery_stamp(heap, state, slot)?;
    if stamp.batch == 0 || stamp.generation != generation || stamp.round != state.rescan_epoch {
        return None;
    }
    Some(WeakMode {
        keys: false,
        values: true,
    })
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
        && let Some(mode) = mode_for_slot(heap, state, to_slot, to_generation)
    {
        heap.scan.weak_mode_lookups += 1;
        return mode;
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

/// Marks one deferred-replay target and links it on its slot record. This is
/// the replay-only queue: it never pushes the shared Gray `Vec`, so a replay
/// at a capacity boundary does not relocate the stored segment. The same
/// target checks and meter as [`enqueue_barrier_target`] ride on the one
/// deferred unit that called this.
fn late_enqueue_target(
    heap: &mut LuaHeap,
    state: &mut MarkState,
    slot: u32,
    generation: u32,
    kind: HandleKind,
) {
    let before = heap.scan.scratch.work_pushed;
    heap.scan.node_slot_visits += 1;
    let mut matched = false;
    if heap.slot_matches(slot, generation, kind) {
        heap.scan.node_slot_visits += 1;
        if heap.mark_new(slot) {
            push_late_target(heap, state, slot);
        }
        matched = true;
    }
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
    let Some((meta_slot, meta_generation)) = stored.metatable else {
        return false;
    };
    mode_for_slot(heap, state, meta_slot, meta_generation).is_some_and(|mode| mode.values)
}

/// Both weak bits from the same cycle cache. No metatable follow and no
/// scan counter. The values-only fast path above is unchanged.
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
    let Some((meta_slot, meta_generation)) = stored.metatable else {
        return WeakMode::default();
    };
    mode_for_slot(heap, state, meta_slot, meta_generation).unwrap_or_default()
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
    /// Phase and whether a gray edge, queued owner, or late-mode unit is still
    /// unscanned. Does not take the mark, scan, or write.
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
                || state.late_target_head.is_some()
                || state.discover_on
                || state.has_late_work(),
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
            pending: !state.gray.is_empty()
                || state.cursor.is_some()
                || state.late_target_head.is_some()
                || state.has_late_work(),
            cursor_owner: state.owner,
            cursor_index: state.cursor,
            gray_len,
        })
    }

    /// Actual Gray stack length and capacity. Test-only: the capacity is not
    /// part of the production meter, but the tests must show a real boundary
    /// crossing rather than infer growth from a push count.
    #[cfg(test)]
    pub fn saved_mark_gray_shape(&self) -> Option<(usize, usize)> {
        self.mark
            .as_ref()
            .map(|state| (state.gray.len(), state.gray.capacity()))
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
    /// not move the root cursor or the current owner cursor. The late-mode
    /// sub-phase still belongs to that window.
    pub(super) fn note_parked_root_store(&mut self, rooted: Rooted) {
        let Some(mut state) = self.mark.take() else {
            return;
        };
        if !state.write_barriers_open() {
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
    /// that is not yet marked does not enter the gray queue. A field whose
    /// metatable is still being read records the target on the edge record and
    /// lets the budgeted late parse decide; a weak arm must not mark it early.
    /// Once the mode is known, the same call decides immediately instead of
    /// queueing a later event. A later write updates the pending record in
    /// place, so repeated overwrites of one live edge keep one deferred site.
    /// This does not call `owner_mode` and does not move either cursor.
    pub(super) fn barrier_strong_field(
        &mut self,
        owner: u32,
        index: u32,
        target: Option<(u32, u32)>,
    ) {
        let Some(mut state) = self.mark.take() else {
            return;
        };
        if state.write_barriers_open() && discovered_table(self, owner) {
            let unknown = owner_mode_unknown(self, &state, owner);
            if unknown {
                // The mode is being read; record the latest target on this
                // edge record and let the budgeted parse decide. A known mode
                // must decide now: queueing it would inject one ModeLate unit
                // per overwrite and starve ordinary Gray work.
                queue_edge_value(self, &mut state, index, owner, target);
            } else {
                // A known mode decides now. If this record still carries a
                // payload from an earlier unknown-mode write, update or cancel
                // it in place: the parse did not make that old target live.
                update_queued_edge_value(self, &mut state, index, target);
                if let Some((slot, generation)) = target
                    && !cached_values_weak(self, &state, owner)
                {
                    enqueue_barrier_target(self, &mut state, slot, generation, HandleKind::Table);
                }
            }
        }
        self.mark = Some(state);
    }

    /// Checks whether either weak side becomes strong using the tuples captured
    /// before the slot write. The old mode comes from this cycle's cache and
    /// that tuple, not from the edge count, so clearing the last metatable
    /// edge cannot make the old mode look strong. A direct value-weak delete
    /// already replaced that cache entry with the missing-field mode; its
    /// discovery stamp restores the pre-delete value bit for an owner that
    /// started under the same metatable. A not-yet-started owner is left for
    /// the ordinary scan. A new metatable whose `__mode` is still being read is
    /// recorded on the owner slot instead: the deferred evaluation uses the
    /// same transition rule after the budgeted parse, and no long string is
    /// walked here. A second setter for the same owner updates the recorded
    /// final tuple in place and keeps the original old tuple. This does not
    /// enqueue, scan, walk a pending list, or move a cursor.
    pub(super) fn needs_owner_rescan(
        &mut self,
        owner: u32,
        old_tuple: Option<(u32, u32)>,
        new_tuple: Option<(u32, u32)>,
    ) -> bool {
        let Some(mut state) = self.mark.take() else {
            return false;
        };
        let mut rescan = false;
        if state.write_barriers_open() && old_tuple != new_tuple {
            let queued = self
                .slots
                .get(owner as usize)
                .is_some_and(|slot| slot.late_owner_epoch == state.rescan_epoch);
            if queued {
                self.slots[owner as usize].late_owner_new = new_tuple;
                self.scan.mode_late_lookups += 1;
                self.scan.mode_late_event_reuses += 1;
            } else if late_mode_unknown(self, &state, new_tuple) {
                queue_owner_check(self, &mut state, owner, old_tuple, new_tuple);
            } else {
                self.scan.mode_late_lookups += 1;
                let old_mode = mode_before_direct_delete(self, &state, old_tuple)
                    .unwrap_or_else(|| mode_of_tuple(self, &state, old_tuple));
                let new_mode = mode_of_tuple(self, &state, new_tuple);
                rescan = ((old_mode.keys && !new_mode.keys)
                    || (old_mode.values && !new_mode.values))
                    && self
                        .slots
                        .get(owner as usize)
                        .is_some_and(|slot| slot.rescan_epoch == state.rescan_epoch);
            }
        }
        self.mark = Some(state);
        rescan
    }

    /// After a real direct `__mode` unlink, replace this cycle's value-weak
    /// cache for that same generation. Owners that have not started read the
    /// missing-field mode. Owners that already started stay for the budgeted
    /// probe. A key-weak cache, a missing record, and a closed mutation are
    /// left alone. The stamp lives on the metatable slot, so a grown or reused
    /// slot needs no sidecar growth. This does not walk slots or move either
    /// cursor.
    pub(super) fn note_direct_mode_deleted(&mut self, owner: u32) {
        let Some(mut state) = self.mark.take() else {
            return;
        };
        if !state.write_barriers_open() {
            self.mark = Some(state);
            return;
        }
        let generation = self.slots[owner as usize].generation;
        let cached = mode_for_slot(self, &state, owner, generation);
        if !cached.is_some_and(|mode| mode.values && !mode.keys) {
            self.mark = Some(state);
            return;
        }
        store_cache_mode(self, &mut state, owner, generation, WeakMode::default());
        self.scan.mode_cache_updates += 1;
        if self.slots[owner as usize].late_mode_epoch == state.rescan_epoch {
            self.scan.mode_late_cancels += 1;
        }
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
        if state.write_barriers_open()
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
        if state.write_barriers_open() && discovered_table(self, owner) {
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
        if state.write_barriers_open() && discovered_upvalue(self, owner, owner_generation) {
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
    /// left for the continued scan. Successors stay on the gray queue. A new
    /// metatable still being read records the key and value on the new edge
    /// record, so a weak side is not marked early. A known mode decides in
    /// this call and queues no later event. A later write merges into that
    /// same record. This does not move the pending cursor.
    pub(super) fn barrier_new_table_key(
        &mut self,
        owner: u32,
        index: u32,
        key_slot: u32,
        key_generation: u32,
        value: Option<(u32, u32)>,
    ) {
        let Some(mut state) = self.mark.take() else {
            return;
        };
        if state.write_barriers_open() && discovered_table(self, owner) {
            let unknown = owner_mode_unknown(self, &state, owner);
            if unknown {
                queue_edge_key(
                    self,
                    &mut state,
                    index,
                    owner,
                    (key_slot, key_generation),
                    value,
                );
            } else {
                // A known mode decides now. The deferred record is only for a
                // mode that is still being read; queueing every insertion
                // would make each new key cost a later ModeLate unit.
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
        }
        self.mark = Some(state);
    }

    /// Registers the direct `__mode` of a metatable first attached while
    /// mutation is open. The reset walk skips every mode read when no
    /// metatable is attached, so a prewritten metatable that is attached for
    /// the first time during Roots or Gray has never been parsed in this
    /// round. The setter only links the slot into the intrusive late parse
    /// queue; it reads no byte and performs no tree operation. A metatable
    /// already parsed in this round, or with no direct mode record, needs no
    /// registration, and an already queued slot keeps its one entry. A field
    /// version the reset walk already inspected at its current record version
    /// also needs no second read, even when the bytes named a strong mode and
    /// left no weak cache: the checked field version and an effective weak
    /// mode are different observations. A rewrite bumps `mode_epoch`, so that
    /// later version still registers here (or on the write path).
    pub(super) fn note_attached_mode(&mut self, slot: u32, generation: u32) {
        let Some(mut state) = self.mark.take() else {
            return;
        };
        if state.write_barriers_open() {
            let needs_read = self.slots.get(slot as usize).is_some_and(|stored| {
                let checked = stored.mode_checked_generation == generation
                    && stored.mode_checked_round == state.rescan_epoch
                    && stored.mode_checked_epoch == stored.mode_epoch;
                stored.kind == HandleKind::Table
                    && stored.generation == generation
                    && stored.mode_edge.is_some()
                    && !checked
                    && (stored.mode_generation != generation
                        || stored.mode_round != state.rescan_epoch)
            });
            if needs_read && self.slots[slot as usize].late_mode_epoch != state.rescan_epoch {
                push_late_mode(self, &mut state, slot);
                self.scan.mode_late_registrations += 1;
            }
        }
        self.mark = Some(state);
    }

    /// Registers a direct `__mode` write made while mutation is open. The
    /// setter only bumps the record version and links the metatable slot into
    /// the late parse queue. A current-round parsed cache for the same slot
    /// generation is already readable and needs no second read; a cache left
    /// by an earlier round is stale and must not block this write. A write on
    /// a slot that is already queued keeps the one queue entry and
    /// invalidates the open parse, which restarts at byte zero. No string is
    /// read and no indexed cache is grown here.
    pub(super) fn note_late_mode_write(&mut self, owner: u32) {
        let Some(mut state) = self.mark.take() else {
            return;
        };
        if state.write_barriers_open() {
            self.scan.mode_late_lookups += 1;
            let current_mode = self.slots[owner as usize].kind == HandleKind::Table
                && self.slots[owner as usize].mode_generation
                    == self.slots[owner as usize].generation
                && self.slots[owner as usize].mode_round == state.rescan_epoch;
            if self.slots[owner as usize].late_mode_epoch == state.rescan_epoch {
                self.scan.mode_late_cancels += 1;
            } else if !current_mode {
                push_late_mode(self, &mut state, owner);
                self.scan.mode_late_registrations += 1;
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
