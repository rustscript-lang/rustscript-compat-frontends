//! Pausable pending-close enumeration after a finished mark.
//!
//! The session holds the heap borrow. A pulse spends one scan unit before it
//! checks one slot. Budget 0 checks nothing and appends nothing. Only a live,
//! unmarked slot with a token is appended, in ascending slot index order.
//! `Vec` growth above that append is not a scan unit and is not a wall-clock
//! bound.
//!
//! `finish` is the only handoff. It returns the tokens and the original mark
//! snapshot, and it refuses an unfinished scan. Drop does not sweep and does
//! not close. This session does not mark again and is not a guest pause.

use super::{DoomedMark, DoomedToken, LuaHeap, MarkSnapshot, ScanMeter, SlotState};

pub(super) struct PendingCloseSession<'a> {
    heap: &'a mut LuaHeap,
    modes: MarkSnapshot,
    tokens: Vec<DoomedToken>,
    index: usize,
    done: bool,
    pulse_pushed: u64,
}

pub(super) struct PendingCloseView {
    pub(super) finished: bool,
    pub(super) meter: ScanMeter,
    pub(super) spent: u64,
    pub(super) pushed: u64,
    pub(super) cursor: u32,
}

impl<'a> PendingCloseSession<'a> {
    /// Starts after mark. `modes` is the snapshot mark already built. This
    /// does not walk slots and does not mark again.
    pub(super) fn start(heap: &'a mut LuaHeap, modes: MarkSnapshot) -> Self {
        Self {
            heap,
            modes,
            tokens: Vec::new(),
            index: 0,
            done: false,
            pulse_pushed: 0,
        }
    }

    pub(super) fn meter(&self) -> ScanMeter {
        self.heap.scan
    }

    pub(super) fn pulse(&mut self, budget: u64) -> PendingCloseView {
        if budget == 0 || self.done {
            return self.view(0, 0);
        }
        self.pulse_pushed = 0;
        let mut left = budget;
        let mut spent = 0u64;
        while left > 0 && !self.done {
            if self.step() {
                left -= 1;
                spent += 1;
            }
        }
        self.view(spent, self.pulse_pushed)
    }

    pub(super) fn drain(&mut self) {
        while !self.done {
            self.pulse(64);
        }
    }

    /// Tokens and the original snapshot. An unfinished scan does not return.
    pub(super) fn finish(self) -> DoomedMark {
        assert!(self.done, "pending close is not finished");
        DoomedMark {
            tokens: self.tokens,
            modes: self.modes,
        }
    }

    fn step(&mut self) -> bool {
        if self.index >= self.heap.slots.len() {
            self.done = true;
            return false;
        }
        let index = self.index;
        self.index += 1;
        self.heap.scan.node_slot_visits += 1;
        let slot = &self.heap.slots[index];
        if let SlotState::Live { marked: false } = slot.state
            && let Some(raw) = slot.token
        {
            self.tokens.push(DoomedToken {
                raw,
                slot: index as u32,
                generation: slot.generation,
            });
            self.heap.scan.scratch.pending_close_pushed += 1;
            self.pulse_pushed += 1;
        }
        if self.index >= self.heap.slots.len() {
            self.done = true;
        }
        true
    }

    fn view(&self, spent: u64, pushed: u64) -> PendingCloseView {
        PendingCloseView {
            finished: self.done,
            meter: self.heap.scan,
            spent,
            pushed,
            cursor: self.index as u32,
        }
    }
}
