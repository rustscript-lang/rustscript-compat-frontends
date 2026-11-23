//! Pausable mark. A session holds the heap borrow until it is dropped or swept.
//!
//! A pulse spends a caller budget on these units, each checked before it runs:
//! one slot reset, one mode-edge open, one mode-string byte (including a NUL),
//! one mode-cache initialization, one root slot, one reached owner's outgoing
//! head, or one outgoing edge or gap. Target checks and the metatable check
//! ride with the edge or the owner head. A zero budget does not read the graph.

use super::{
    CollectReport, EdgeKey, EdgeValue, HandleKind, LuaHeap, MarkSnapshot, SlotState, WeakMode,
};
use vm::Value;

pub(super) struct MarkSession<'a> {
    heap: &'a mut LuaHeap,
    phase: MarkPhase,
    gray: Vec<u32>,
    modes: Option<Vec<WeakMode>>,
    index: usize,
    mode_edge: Option<u32>,
    mode_owner: usize,
    byte_at: usize,
    pending_mode: WeakMode,
    init_at: usize,
    owner: Option<u32>,
    cursor: Option<u32>,
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

impl<'a> MarkSession<'a> {
    pub(super) fn start(heap: &'a mut LuaHeap) -> Self {
        Self {
            heap,
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

    pub(super) fn meter(&self) -> super::ScanMeter {
        self.heap.scan
    }

    pub(super) fn pulse(&mut self, budget: u64) -> MarkView {
        if budget == 0 || matches!(self.phase, MarkPhase::Done) {
            return self.view();
        }
        if matches!(self.phase, MarkPhase::Start) {
            self.heap.scan.mark_passes += 1;
            self.phase = MarkPhase::Reset;
        }
        let mut left = budget;
        let mut spins = 0u32;
        while left > 0 && !matches!(self.phase, MarkPhase::Done) {
            if self.step() {
                left -= 1;
                spins = 0;
            } else {
                spins += 1;
                debug_assert!(spins <= 4, "mark phase did not advance");
            }
        }
        self.view()
    }

    pub(super) fn drain(&mut self) {
        while !matches!(self.phase, MarkPhase::Done) {
            self.pulse(64);
        }
    }

    pub(super) fn sweep(self) -> CollectReport {
        let MarkSession {
            heap, modes, phase, ..
        } = self;
        assert!(
            matches!(phase, MarkPhase::Done),
            "sweep needs a finished mark"
        );
        heap.sweep_marked(MarkSnapshot { modes })
    }

    fn view(&self) -> MarkView {
        MarkView {
            finished: matches!(self.phase, MarkPhase::Done),
            meter: self.heap.scan,
        }
    }

    fn step(&mut self) -> bool {
        match self.phase {
            MarkPhase::Start => false,
            MarkPhase::Reset => self.step_reset(),
            MarkPhase::ModeOpen => self.step_mode_open(),
            MarkPhase::ModeBytes => self.step_mode_byte(),
            MarkPhase::ModeInit => self.step_mode_init(),
            MarkPhase::Roots => self.step_root(),
            MarkPhase::Gray => self.step_gray(),
            MarkPhase::Done => false,
        }
    }

    fn step_reset(&mut self) -> bool {
        if self.index >= self.heap.slots.len() {
            self.phase = MarkPhase::Roots;
            self.index = 0;
            return false;
        }
        let index = self.index;
        if let SlotState::Live { marked } = &mut self.heap.slots[index].state {
            *marked = false;
        }
        self.heap.scan.node_slot_visits += 1;
        self.heap.scan.weak_mode_slot_checks += 1;
        let mode_edge = self.heap.slots[index].mode_edge;
        let is_table = self.heap.slots[index].kind == HandleKind::Table;
        if self.heap.metatable_edges > 0 && is_table && mode_edge.is_some() {
            self.mode_edge = mode_edge;
            self.mode_owner = index;
            self.byte_at = 0;
            self.pending_mode = WeakMode::default();
            self.phase = MarkPhase::ModeOpen;
        } else {
            self.index += 1;
        }
        true
    }

    fn step_mode_open(&mut self) -> bool {
        let edge_index = self.mode_edge.expect("mode open has an edge");
        self.heap.scan.weak_mode_edge_checks += 1;
        if self.mode_string_len(edge_index).is_some() {
            self.phase = MarkPhase::ModeBytes;
        } else {
            self.advance_after_mode();
        }
        true
    }

    fn step_mode_byte(&mut self) -> bool {
        let edge_index = self.mode_edge.expect("mode byte has an edge");
        let at = self.byte_at;
        let Some(byte) = self.mode_string_byte(edge_index, at) else {
            self.finish_mode_parse();
            return false;
        };
        self.heap.scan.weak_mode_bytes += 1;
        self.byte_at = at + 1;
        match byte {
            b'k' => self.pending_mode.keys = true,
            b'v' => self.pending_mode.values = true,
            0 => {
                self.finish_mode_parse();
                return true;
            }
            _ => {}
        }
        let len = self.mode_string_len(edge_index).unwrap_or(0);
        if self.byte_at >= len {
            self.finish_mode_parse();
        }
        true
    }

    fn finish_mode_parse(&mut self) {
        let parsed = self.pending_mode;
        if parsed.keys || parsed.values {
            if self.modes.is_none() {
                self.init_at = 0;
                self.phase = MarkPhase::ModeInit;
                return;
            }
            self.store_mode(parsed);
        }
        self.advance_after_mode();
    }

    fn step_mode_init(&mut self) -> bool {
        let slot_count = self.heap.slots.len();
        if self.modes.is_none() {
            self.modes = Some(Vec::with_capacity(slot_count));
        }
        let modes = self.modes.as_mut().expect("mode init has a cache");
        modes.push(WeakMode::default());
        self.heap.scan.scratch.weak_modes_inited += 1;
        self.init_at += 1;
        if self.init_at == slot_count {
            let parsed = self.pending_mode;
            self.store_mode(parsed);
            self.advance_after_mode();
        }
        true
    }

    fn store_mode(&mut self, parsed: WeakMode) {
        let modes = self.modes.as_mut().expect("mode cache");
        modes[self.mode_owner] = parsed;
    }

    fn advance_after_mode(&mut self) {
        self.mode_edge = None;
        self.index += 1;
        self.phase = MarkPhase::Reset;
    }

    fn step_root(&mut self) -> bool {
        if self.index >= self.heap.roots.len() {
            self.phase = MarkPhase::Gray;
            return false;
        }
        let index = self.index;
        self.index += 1;
        self.heap.scan.root_slot_visits += 1;
        let Some(rooted) = self.heap.roots[index] else {
            return true;
        };
        self.heap.scan.node_slot_visits += 1;
        if self.heap.rooted_alive(rooted) {
            self.heap.scan.node_slot_visits += 1;
            let slot = rooted.slot();
            if self.heap.mark_new(slot) {
                self.gray.push(slot);
            }
        }
        true
    }

    fn step_gray(&mut self) -> bool {
        if self.owner.is_some() {
            if self.cursor.is_some() {
                return self.charge_edge();
            }
            self.owner = None;
            return false;
        }
        self.begin_owner()
    }

    fn begin_owner(&mut self) -> bool {
        let Some(slot) = self.gray.pop() else {
            self.phase = MarkPhase::Done;
            return false;
        };
        let kind = self.heap.slots[slot as usize].kind;
        let mode = if kind == HandleKind::Table && self.heap.metatable_edges > 0 {
            self.owner_mode(slot)
        } else {
            WeakMode::default()
        };
        self.heap.scan.owner_head_checks += 1;
        self.cursor = self.heap.slots[slot as usize].edge_head;
        self.owner = Some(slot);
        self.owner_kind = kind;
        self.owner_mode = mode;
        true
    }

    fn charge_edge(&mut self) -> bool {
        let cursor = self.cursor.expect("edge unit has a cursor");
        let next = self.heap.edge_slots[cursor as usize].next;
        self.cursor = next;
        let gap = self.heap.edge_slots[cursor as usize]
            .edge
            .as_ref()
            .is_some_and(|edge| edge.value.is_cursor_gap());
        if gap {
            self.heap.scan.outgoing_gap_checks += 1;
            return true;
        }
        self.heap.scan.outgoing_edge_checks += 1;
        self.follow_edge_ends(cursor);
        true
    }

    fn follow_edge_ends(&mut self, cursor: u32) {
        let (object_key, value_target) = {
            let edge = self.heap.edge_slots[cursor as usize]
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
        let from_table = self.owner_kind == HandleKind::Table;
        if from_table
            && !self.owner_mode.keys
            && let Some((slot, generation, kind)) = object_key
        {
            let _matched = self.follow_one(slot, generation, kind);
        }
        // A table entry follows that table's weak mode. A closure or cell
        // link is not a table entry, so it stays strong.
        let strong_value = !from_table || !self.owner_mode.values;
        if strong_value && let Some((slot, generation, kind)) = value_target {
            let _matched = self.follow_one(slot, generation, kind);
        }
    }

    fn owner_mode(&mut self, slot: u32) -> WeakMode {
        let Some((to_slot, to_generation)) = self.heap.slots[slot as usize].metatable else {
            return WeakMode::default();
        };
        self.heap.scan.metatable_edge_checks += 1;
        if self.follow_one(to_slot, to_generation, HandleKind::Table)
            && let Some(modes) = self.modes.as_ref()
        {
            self.heap.scan.weak_mode_lookups += 1;
            return modes[to_slot as usize];
        }
        WeakMode::default()
    }

    /// Same visits as the old target follow. True when the slot matches, even
    /// if it was already marked. Only a newly marked slot joins the gray stack.
    fn follow_one(&mut self, slot: u32, generation: u32, kind: HandleKind) -> bool {
        self.heap.scan.node_slot_visits += 1;
        if !self.heap.slot_matches(slot, generation, kind) {
            return false;
        }
        self.heap.scan.node_slot_visits += 1;
        if self.heap.mark_new(slot) {
            self.gray.push(slot);
        }
        true
    }

    fn mode_string_len(&self, edge_index: u32) -> Option<usize> {
        let edge = self.heap.edge_slots[edge_index as usize].edge.as_ref()?;
        match &edge.value {
            EdgeValue::Scalar(Value::String(mode)) => Some(mode.len()),
            _ => None,
        }
    }

    fn mode_string_byte(&self, edge_index: u32, at: usize) -> Option<u8> {
        let edge = self.heap.edge_slots[edge_index as usize].edge.as_ref()?;
        match &edge.value {
            EdgeValue::Scalar(Value::String(mode)) => mode.as_bytes().get(at).copied(),
            _ => None,
        }
    }
}

impl LuaHeap {
    pub(super) fn drain_mark(&mut self) -> MarkSnapshot {
        let mut session = MarkSession::start(self);
        session.drain();
        MarkSnapshot {
            modes: session.modes.take(),
        }
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
