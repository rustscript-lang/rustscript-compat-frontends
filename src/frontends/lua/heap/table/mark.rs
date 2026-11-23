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
    MarkSnapshot, RootFrameId, Rooted, SlotState, TableRef, UpvalueRef, WeakMode,
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
    fn new() -> Self {
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
        heap.mark = Some(MarkState::new());
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
        heap.mark = Some(MarkState::new());
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

    pub(super) fn pending(&self) -> Option<PendingScan> {
        let state = self.heap.mark.as_ref()?;
        Some(PendingScan {
            owner: state.owner?,
            cursor: state.cursor?,
        })
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
        self.heap.clear_edge(from, &key);
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

fn pulse_state(heap: &mut LuaHeap, state: &mut MarkState, budget: u64) -> MarkView {
    if budget == 0 || matches!(state.phase, MarkPhase::Done) {
        return view(heap, state);
    }
    if matches!(state.phase, MarkPhase::Start) {
        heap.scan.mark_passes += 1;
        state.phase = MarkPhase::Reset;
    }
    let mut left = budget;
    let mut spins = 0u32;
    while left > 0 && !matches!(state.phase, MarkPhase::Done) {
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
    }
    let modes = state.modes.as_mut().expect("mode init has a cache");
    modes.push(WeakMode::default());
    heap.scan.scratch.weak_modes_inited += 1;
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
    begin_owner(heap, state)
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
    // A table entry follows that table's weak mode. A closure or cell
    // link is not a table entry, so it stays strong.
    let strong_value = !from_table || !state.owner_mode.values;
    if strong_value && let Some((slot, generation, kind)) = value_target {
        let _matched = follow_one(heap, state, slot, generation, kind);
    }
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

impl LuaHeap {
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

    /// Advances a parked gray cursor when `unlink_at` removes its record.
    /// One field compare. No chain, root, or slot walk.
    pub(super) fn repair_parked_cursor(&mut self, owner: u32, index: u32, next: Option<u32>) {
        let Some(state) = self.mark.as_mut() else {
            return;
        };
        if state.owner != Some(owner) || state.cursor != Some(index) {
            return;
        }
        if next.is_some() {
            state.cursor = next;
        } else {
            state.owner = None;
            state.cursor = None;
        }
        self.scan.parked_cursor_repairs += 1;
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
