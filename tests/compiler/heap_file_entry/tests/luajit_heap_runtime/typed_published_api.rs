//! Test-only typed published-head transaction surface.
//!
//! This file is the real module for the compiler harness. The compiler
//! harness resolves the `#[path]` in `heap/table.rs` here; the heap harness
//! resolves the same relative path to an empty stand-in under
//! `tests/luajit_heap_runtime/`, so only the compiler harness compiles and
//! consumes these entries. Every entry is an inherent `LuaHeap` method
//! defined in a child of the table module, so it can read the private
//! published head and use the private root plan, pop, and settle primitives
//! without widening a single production visibility.

use super::*;

/// Test-only read view of the current published typed table-call head.
///
/// It is not a production handle and it cannot pop, root, or settle anything:
/// it carries the same identities the exclusive commit must re-read, with the
/// stored callable kept as a `Weak`.
#[derive(Clone)]
pub struct TestPublishedTypedCall {
    pub userdata: UserdataRef,
    pub raw_token: u64,
    pub metatable: TableRef,
    pub receiver: TableRef,
    pub closure: ClosureRef,
    pub callable: std::sync::Weak<CallableValue>,
    pub published_count: usize,
    pub queue_len: usize,
}

impl LuaHeap {
    /// Test-only read of the current published typed table-call head.
    ///
    /// A saved mark or Release tail is refused before the queue is read. The
    /// record's live slot must still bind its exact generation, kind, pending
    /// flag, and raw handle; the typed token stored for that userdata must name
    /// the same raw handle; and the current raw `__gc` must still resolve to a
    /// live table receiver with its own raw `__call` closure. The stored
    /// callable is returned as a `Weak`, so this read creates no strong Arc and
    /// no root. Nothing is written.
    pub fn test_snapshot_published_typed_call(&self) -> Result<TestPublishedTypedCall, HeapError> {
        if self.has_sweep_tail() {
            return Err(HeapError::SweepPending);
        }
        if self.has_saved_mark() {
            return Err(HeapError::MarkActive);
        }
        let head = self.pending_finalizer_published_front()?;
        let userdata = self.pending_finalizer_head_identity(head)?;
        if self.published_userdata_token(userdata)? != head.raw {
            return Err(HeapError::TokenMismatch);
        }
        let metatable = self
            .userdata_metatable(userdata)?
            .ok_or(HeapError::StaleHandle)?;
        let PendingFinalizerAction::TableCall { receiver, call } =
            self.pending_finalizer_action(userdata)
        else {
            return Err(HeapError::Scope(
                "lua typed published head is not a table call".to_string(),
            ));
        };
        let callable = Arc::downgrade(&self.closure_callable(call)?);
        Ok(TestPublishedTypedCall {
            userdata,
            raw_token: head.raw,
            metatable,
            receiver,
            closure: call,
            callable,
            published_count: self.pending_finalizer_published_count(),
            queue_len: self.pending_finalizer_count(),
        })
    }

    /// Test-only exclusive typed commit for the published table-call head.
    ///
    /// The caller supplies only the actual parent-frame operands: the exact
    /// typed callable Arc, the Core payload the actual `lua.userdata` token
    /// named, the snapshot metatable, the actual receiver carrier value, and
    /// the actual token. The heap owner reads the published head itself; no
    /// external queue record, generation, or root index is accepted.
    ///
    /// Every check runs in this one exclusive borrow before any write: no
    /// saved mark or Release tail, a published head, a live head identity, the
    /// head's real token, the exact typed payload owner, the expected
    /// metatable, the current one-layer raw table-call action, the receiver
    /// operand classified as that action's exact issued array, and the exact
    /// stored callable `Arc`. Only then does it open one fresh three-position
    /// root frame, re-check its fixed empty positions, pop the head, write the
    /// userdata/closure/receiver roots, and settle the generation. A refusal
    /// leaves the queue, roots, and settled flag unchanged.
    pub fn test_commit_published_typed_table_call(
        &mut self,
        expected_callable: &Arc<CallableValue>,
        expected_payload: Option<UserdataRef>,
        expected_metatable: TableRef,
        expected_receiver: &Value,
        expected_token: u64,
    ) -> Result<(RootFrameId, TakenPendingFinalizer), HeapError> {
        self.deny_unbarriered()?;
        let head = self.pending_finalizer_published_front()?;
        let Some(userdata) = expected_payload else {
            return Err(HeapError::TokenMismatch);
        };
        if self.pending_finalizer_head_identity(head)? != userdata {
            return Err(HeapError::TokenMismatch);
        }
        if self.published_userdata_token(userdata)? != expected_token {
            return Err(HeapError::TokenMismatch);
        }
        if self.userdata_metatable(userdata)? != Some(expected_metatable) {
            return Err(HeapError::StaleHandle);
        }
        let action = self.pending_finalizer_action(userdata);
        let PendingFinalizerAction::TableCall { receiver, call } = action else {
            return Err(HeapError::Scope(
                "lua typed published commit requires a table call".to_string(),
            ));
        };
        if self.guest_class(expected_receiver) != GuestClass::Table(receiver) {
            return Err(HeapError::WrongKind);
        }
        let Value::Array(operand) = expected_receiver else {
            return Err(HeapError::WrongKind);
        };
        let Value::Array(issued) = self.issued_value(receiver)? else {
            return Err(HeapError::StaleHandle);
        };
        if !Arc::ptr_eq(operand, &issued) {
            return Err(HeapError::WrongKind);
        }
        let stored = self.closure_callable(call)?;
        if !Arc::ptr_eq(&stored, expected_callable) {
            return Err(HeapError::TokenMismatch);
        }
        let frame = self.enter_roots(3)?;
        let indices = self.pending_finalizer_root_indices(frame, &[0, 1, 2], 3)?;
        let userdata = self.commit_published_finalizer(
            head,
            userdata,
            &[
                (indices[0], Rooted::Userdata(userdata)),
                (indices[1], Rooted::Closure(call)),
                (indices[2], Rooted::Table(receiver)),
            ],
        )?;
        Ok((
            frame,
            TakenPendingFinalizer {
                userdata,
                action,
                roots_written: 3,
            },
        ))
    }

    /// Test-only admission of one actual guest callable into the receiver's raw
    /// `__call` record.
    ///
    /// The harness supplies the same two actual guest values the production
    /// two-argument import would receive. The prototype lookup, closure-kind
    /// check, and zero-capture requirement run against this VM's compiled
    /// program; the private heap write then re-reads the issued owner, its
    /// metatable, and the existing raw record before the same three infallible
    /// steps. Production reaches this write only through its registered
    /// import.
    pub fn test_admit_guest_callable_to_raw_call(
        vm: &mut Vm,
        owner: &Value,
        target: &Value,
    ) -> Result<ClosureRef, HeapError> {
        let Value::Callable(target) = target else {
            return Err(HeapError::WrongKind);
        };
        if target.kind != CallableKind::Closure {
            return Err(HeapError::CallableHasEnvironment);
        }
        let capture_slots = script_callable_capture_slots(vm, target)
            .map_err(|error| HeapError::Scope(format!("{error:?}")))?;
        if capture_slots != 0 {
            return Err(HeapError::CallableHasEnvironment);
        }
        let callable = Arc::clone(target);
        let mut host = vm.host_context();
        super::super::with_heap(&mut host, |heap| {
            heap.handoff_guest_callable_to_raw_call(owner, callable, capture_slots)
        })
    }
}
