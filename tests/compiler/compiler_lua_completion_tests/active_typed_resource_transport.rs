//! Typed `resource<lua.userdata>` transport through a host-returned Lua callable.
//!
//! One real `.lua` file is fed through the real lexer, parser, resolver and
//! private completion lowering, compiled through the fixed Core and run in one
//! interpreter VM with JIT disabled. Every Lua function and host call is real.
//! The plugin makes four structural changes, all local to this test and none
//! touching a catalog schema:
//!
//! - it declares the exact callable schema on the two chunk-level closure
//!   bindings, so their compiled prototypes carry it;
//! - it declares the exact callable schema on the typed local that holds the
//!   callable the exact static host `note` returns, and retargets the one
//!   dynamic call the real lowering produced onto that local;
//! - it rewrites the typed target's own return expression from the private
//!   completion array to the `Bool` its declared signature names, because
//!   Core validates a script call's real return against the prototype result;
//! - it uses the plain local read (identical codegen) for admitted callables
//!   instead of the private lowerer's explicit `ToOwned` copy, because this
//!   pin classifies a callable whose schema contains a resource as owned and
//!   refuses `.copy()` on it; none of these callables is captured.
//!
//! The positive prototype is a real top-level zero-capture Lua closure whose
//! compiled prototype declares `(Array<Int>, Resource(lua.userdata)) -> Bool`.
//! `Array<Int>` is the actual shape of the issued LuaHeap carrier
//! (`[slot, generation]`, both `Int`), not `Unknown`. The import is registered
//! with `register_exact_static`, so the program carries that exact return
//! schema and Core's `validate_host_call_return`/`validate_callable_value`
//! accepts only a callable whose own prototype schema matches it.
//!
//! Witnessed in the positive run:
//!
//! - the host receives and returns the exact `SharedCallable` Arc the VM
//!   produced for the guest binding (`Arc::ptr_eq` at the host boundary, in the
//!   parent's `cb` slot and in a second parent-frame local);
//! - `CallValue(2)` enters a real script frame whose declared parameter slots
//!   hold the original issued `Array<Int>` carrier and the original
//!   `lua.userdata` token handle, bound to the decoded call site and resume
//!   address;
//! - the child frame uses the formal typed resource API
//!   (`ResourceHandle::from_raw` + `HostContext::borrow_resource_with_key::<
//!   LuaUserdataResource>`) to read the payload, and the heap's published
//!   handle also names the same userdata;
//! - the parent continuation re-borrows the same token in the same VM after
//!   one script call and then returns its own `Bool` completion.
//!
//! Two separate runtime type controls share the same declarations:
//!
//! - `OtherResourceKey`: the host really selects `other_resource`, a real
//!   top-level zero-capture closure with the same arity, receiver schema and
//!   result, whose prototype declares the second parameter as the other legally
//!   registered resource key (`lua.table`). The static host body runs, Core's
//!   real return boundary refuses the callable with `TypeMismatch("callable")`,
//!   and no callback frame starts.
//! - `OtherArgumentToken`: the correctly declared callable is returned and
//!   invoked, but the actual second argument is a real registered `lua.table`
//!   token. Core's real call entry refuses the operand with the resource key
//!   `HostError` and no callback frame starts.
//!
//! Both controls compile successfully; neither is a compile-time schema
//! mismatch. The compile-time declaration check is covered by the retargeted
//! typed local itself, and the runtime layer is what produces these two
//! diagnostics.
//!
//! The callback entry is observed before any fallible work: every marker host
//! call first records the live last frame's actual prototype id, depth and
//! resume address. Marker records are still committed only after the typed
//! borrows, so the raw entry record is what preserves the evidence when a
//! probe fails after the frame really entered. The positive run has exactly
//! one entry into a declared two-parameter typed script callable; both
//! negative controls have none.
//!
//! Control B binds its diagnostic to the native
//! `resource_type_key_mismatch` error itself: the stable code, the
//! `resource::table` operation, the expected `lua.userdata` and actual
//! `lua.table` keys, and the raw handle of this run's issued `lua.table`
//! token. It is an exact native message, so any test probe wrapper (which
//! prefixes its own text) can no longer satisfy the assertion.
//!
//! Two more fixed-pin constraints are recorded here rather than hidden: the
//! completion status read `get(completion, 0)` cannot be typed for a callable
//! whose declared result is the required concrete `Bool`
//! (`builtin 'get' does not accept argument types (bool, int)`), so the parent
//! keeps the real call and continues after it without that status branch; and
//! Core's script-return check compares the target's real return value with the
//! prototype result, so the target returns `Bool(true)` instead of the
//! private completion array. The typed local, the exact import return schema
//! and both closure prototypes keep the exact signature: no `Unknown`, no
//! fabricated handle, no relaxed schema. The child's real `Bool` return is
//! observed in the parent frame.
//!
//! The first four arms do not claim a finalizer dequeue, the pending queue,
//! commit, root handoff, close, collection, scheduling pauses, or a
//! source-level general closure bridge. The heap only issues one real table
//! carrier, one real `lua.userdata` token and one real `lua.table` token;
//! nothing is collected or closed there.
//!
//! ## Typed published head commit
//!
//! Four more arms run one real `.lua` fixture. `fault()` builds, in this same
//! VM, two real typed userdata A and B whose shared metatable stores
//! `__gc = recv`, clears one independent weak-value orphan with the real
//! selecting collection, finishes the published Release tail, and returns A's
//! real token. The plugin inserts `bind(recv, target)`, `wrong_token()`,
//! `admit`, and `commit` as exact statics. `admit` is the read-only
//! `Borrow/Value/Borrow` seam over the same typed local and the same two
//! parent slots. `commit` re-reads the live published head, metatable, raw
//! action, issued receiver carrier, exact stored callable `Arc`, typed payload
//! owner, token, and a fresh three-root plan in one exclusive heap borrow; it
//! opens the root frame and pops, roots, and settles only after all of them
//! match. The following `CallValue(2)` uses the same local and the same two
//! slots into the real two-parameter child frame, and the parent continuation
//! re-borrows the same token. After the run, withdrawing only the three commit
//! roots and collecting retires A's token and generation while B stays the
//! published head and the independently rooted sentinel stays readable. Three
//! controls pass the queued successor's real token, an equal-valued unissued
//! receiver array, or a raw `__call` rewritten between the seam and the
//! commit; each is refused by the same entry before any write, and the same VM
//! then re-takes the real credentials and commits A. A mutation that removes
//! only the callable `Arc` comparison turns the raw-edge control into a real
//! `Ok(Halted)` business failure.
//!
//! ## Pre-commit borrow seam
//!
//! Two extra arms keep the same real fixture and insert one more exact static
//! import, `admit`, between the typed local and the parent's own `CallValue(2)`.
//! The catalog declares it as
//! `(Borrow<Callable<(Array<Int>, Resource(lua.userdata)) -> Bool>>,
//! Value<Array<Int>>, Borrow<Resource(lua.userdata)>) -> Bool`, and the plugin
//! resolves the call with those concrete operand schemas, so the compiled
//! program carries the complete `HostImportSchema` and catalog fingerprint and
//! `register_exact_static` binds the static body. The body records the actual
//! callable Arc, receiver carrier and typed userdata payload, refuses only a
//! receiver the heap did not issue (with its own `HostError`), and otherwise
//! returns one fixed `Bool`. The positive arm then runs the same compiled
//! `CallValue(2)` into the real two-parameter child frame and the parent
//! continuation with those identical operands; the control arm passes an
//! equal-valued but never issued array, built at run time from the issued
//! carrier's two integers, and stops in the static body. Not claimed: a queue
//! write, dequeue, commit, root plan, generation snapshot, or GC.
//!
//! ## Compile and run scope ownership
//!
//! The compile input (`GATE_MODE`) and the structural patch report belong to
//! the plugin instance that performed that compile. The runtime selection
//! (`SELECT`) and the observation witness belong to the execution scope of
//! the VM that ran the fixture. Two arms compiled and interpreted on two
//! threads therefore cannot read, overwrite or take each other's patch or
//! records; the two-thread controls at the bottom of this module drive the
//! real compiler and the real interpreter concurrently.

use std::cell::Cell;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use vm::compiler::{HostCallResolver, ResolvedHostCall};
use vm::vm::VmFrameContinuation;
use vm::{
    BuiltinFunction, CallOutcome, CallReturn, ClosureExpr, CompileSourceFileOptions, Expr,
    FrontendImportSyntax, FrontendIr, FunctionDecl, HostApiBuilder, HostApiCatalog,
    HostFunctionRegistry, HostFunctionSchema, HostImport, HostImportSchema, HostParamPassing,
    HostParamSchema, HostTypeSchema, JitConfig, LocalSlot, OpCode, ParseError, Program,
    ResourceHandle, ResourceTypeSchema, SourceFlavor, SourcePlugin, Stmt, TypeSchema, Value, Vm,
    VmError, VmStatus,
};

use super::{FUEL_BUDGET, FUEL_CHECK_INTERVAL, decoded_call_sites};
use crate::heap::{self, GuestClass, LuaTableResource, LuaUserdataResource, TableRef, UserdataRef};

#[path = "active_typed_callback_root_retirement.rs"]
mod active_typed_callback_root_retirement;

/// The fixture the positive and resource-key control arms execute.
const FIXTURE: &str = "active_typed_resource_transport.lua";
/// The fixture whose only structural difference is the actual second argument.
const OTHER_ARGUMENT_FIXTURE: &str = "active_typed_resource_other_arg.lua";
/// `parent(cb, first, second, single, other, recv, arg2, mode)`.
const PARENT_PARAMS: usize = 8;
/// `cb(recv, arg2)`.
const CALL_ARGC: u8 = 2;
/// The admitted callable and the two actual arguments in `parent`.
const PARENT_CALLEE_SLOT: usize = 0;
const PARENT_RECEIVER_SLOT: usize = 5;
const PARENT_ARGUMENT_SLOT: usize = 6;

/// The second exact static import this module registers and the exact label of
/// the raw entry record its static body writes before any fallible check.
const PRECOMMIT_IMPORT: &str = "admit";
const PRECOMMIT_ENTRY_LABEL: &str = "precommit-admit-enter";
/// The static body's own diagnostic kinds; the test host never wraps these.
const PRECOMMIT_REFUSAL_RECEIVER: &str =
    "typed precommit seam refused: receiver is not the issued LuaHeap carrier";

/// The fixture that carries a real published typed head through the parent.
const PUBLISHED_COMMIT_FIXTURE: &str = "active_typed_published_commit.lua";
/// Its parent has the pre-commit operands plus the successor token and mode.
const PUBLISHED_PARENT_PARAMS: usize = 9;
const PUBLISHED_WRONG_SLOT: usize = 7;
/// The parent's `second` parameter carries the exact alternate closure Arc.
const PUBLISHED_ALTERNATE_SLOT: usize = 2;
/// The exact imports the published-commit plugin resolves and inserts. None of
/// these names is a guest-lowered host name; the plugin adds the IR calls.
const BIND_IMPORT: &str = "bind";
const COMMIT_IMPORT: &str = "commit";
const SWAP_IMPORT: &str = "swap";
const WRONG_TOKEN_IMPORT: &str = "wrong_token";
const COMMIT_ENTRY_LABEL: &str = "published-commit-enter";
/// The typed commit body's own diagnostic prefix; the test host never wraps it.
const COMMIT_REFUSAL_PREFIX: &str = "typed published commit refused: ";

/// The fixture whose parent asks the private exact static `produce` for the
/// callable instead of taking one of its own callable arguments.
const PRODUCER_FIXTURE: &str = "active_typed_published_head_producer.lua";
/// The exact static that reads the published head's raw `__gc` / `__call`
/// chain and returns the stored callable Arc for Core's typed return check.
const PRODUCER_IMPORT: &str = "produce";
const PRODUCER_ENTRY_LABEL: &str = "producer-enter";
/// The distinct source-level marker of the before-read replacement closure.
const ALTERNATE_LABEL: &str = "alternate-enter";
/// The fixture whose parent optionally calls the exact `retire` import after
/// the real `CallValue(2)` returns, then always reaches the guest `sweep`.
const RETIREMENT_FIXTURE: &str = "active_typed_callback_root_retirement.lua";
const RETIRE_IMPORT: &str = "retire";
const SWEEP_IMPORT: &str = "sweep";
/// Producer-shaped gate whose compiled target result is `Bool(false)`.
const FALSE_RETURN_GATE: usize = 11;

/// The first statement of the two declared closures names them.
const TARGET_LABEL: &str = "target-enter";
const OTHER_LABEL: &str = "other-enter";

// ---- compile state per plugin, run state per VM execution scope ----------

/// Compile-time arm input and the structural report belong to one plugin
/// instance. Two arms compiled on two threads cannot observe each other's
/// gate mode or patch report.
struct TransportPlugin {
    gate_mode: usize,
    pause_on_compile: bool,
    patch: Mutex<Option<PatchReport>>,
}

impl TransportPlugin {
    fn new(arm: Arm) -> Self {
        Self {
            gate_mode: arm.gate_mode(),
            pause_on_compile: false,
            patch: Mutex::new(None),
        }
    }

    fn with_compile_pause(arm: Arm) -> Self {
        Self {
            gate_mode: arm.gate_mode(),
            pause_on_compile: true,
            patch: Mutex::new(None),
        }
    }

    fn take_patch(&self, arm: Arm) -> PatchReport {
        self.patch
            .lock()
            .expect("typed resource patch report")
            .take()
            .unwrap_or_else(|| panic!("{}: the plugin produced no patch report", arm.label()))
    }
}

/// Runtime arm input and the observation witness belong to exactly the
/// execution scope of the VM that produced them. Another VM cannot read,
/// overwrite or take them.
struct RunScope {
    select: usize,
    witness: Witness,
    /// Compiled `CallValue` result local. Set only for the retirement arms.
    call_result_slot: Option<LocalSlot>,
}

fn with_run_scope<T>(vm: &mut Vm, body: impl FnOnce(&mut RunScope) -> T) -> T {
    let mut host = vm.host_context();
    let scope = host
        .scope_state_mut::<RunScope>()
        .expect("the typed resource run scope must be installed before the run");
    body(scope)
}

// ---- controlled interleaving for the two-thread controls ------------------

/// The test-only ordering points. They gate timing only, and no lock that
/// protects shared observation is held across either wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InterleavePoint {
    /// Inside `TransportPlugin::parse_source`, after that plugin instance
    /// wrote its own patch report and before it returns.
    Compile,
    /// In `host_mark("chunk-mode")`, before the VM's own selected arm is read.
    Runtime,
    /// In the run driver, after the VM stopped and before its own execution
    /// scope is taken.
    Take,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InterleaveRole {
    First,
    Second,
}

const POINT_RUNTIME: u8 = 1 << 0;
const POINT_TAKE: u8 = 1 << 1;

thread_local! {
    static INTERLEAVE_PLAN: Cell<Option<(InterleaveRole, u8)>> = const { Cell::new(None) };
}

fn interleave_plan(role: InterleaveRole, points: u8) {
    INTERLEAVE_PLAN.with(|plan| plan.set(Some((role, points))));
}

fn clear_interleave_plan() {
    INTERLEAVE_PLAN.with(|plan| plan.set(None));
}

fn interleave_pause(point: InterleavePoint) {
    let Some((role, points)) = INTERLEAVE_PLAN.with(|plan| plan.get()) else {
        return;
    };
    let bit = match point {
        InterleavePoint::Compile => return,
        InterleavePoint::Runtime => POINT_RUNTIME,
        InterleavePoint::Take => POINT_TAKE,
    };
    let expected_role = match point {
        InterleavePoint::Compile | InterleavePoint::Runtime => InterleaveRole::First,
        InterleavePoint::Take => InterleaveRole::Second,
    };
    if role != expected_role || points & bit == 0 {
        return;
    }
    interleave_gate_for(point).park(point);
}

struct InterleaveGate {
    reached: Mutex<Option<InterleavePoint>>,
    reached_signal: Condvar,
    released: Mutex<Vec<InterleavePoint>>,
    released_signal: Condvar,
}

static COMPILE_INTERLEAVE_GATE: OnceLock<InterleaveGate> = OnceLock::new();
static RUNTIME_INTERLEAVE_GATE: OnceLock<InterleaveGate> = OnceLock::new();

fn compile_interleave_gate() -> &'static InterleaveGate {
    COMPILE_INTERLEAVE_GATE.get_or_init(InterleaveGate::new)
}

fn runtime_interleave_gate() -> &'static InterleaveGate {
    RUNTIME_INTERLEAVE_GATE.get_or_init(InterleaveGate::new)
}

fn interleave_gate_for(point: InterleavePoint) -> &'static InterleaveGate {
    match point {
        InterleavePoint::Compile => compile_interleave_gate(),
        InterleavePoint::Runtime | InterleavePoint::Take => runtime_interleave_gate(),
    }
}

impl InterleaveGate {
    fn new() -> Self {
        Self {
            reached: Mutex::new(None),
            reached_signal: Condvar::new(),
            released: Mutex::new(Vec::new()),
            released_signal: Condvar::new(),
        }
    }

    fn park(&self, point: InterleavePoint) {
        *self
            .reached
            .lock()
            .expect("typed transport interleave reached") = Some(point);
        self.reached_signal.notify_all();
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut released = self
            .released
            .lock()
            .expect("typed transport interleave released");
        while !released.contains(&point) {
            let now = Instant::now();
            assert!(
                now < deadline,
                "typed transport interleave: the {point:?} point was not released"
            );
            let wait = deadline
                .saturating_duration_since(now)
                .min(Duration::from_millis(100));
            let (guard, _) = self
                .released_signal
                .wait_timeout(released, wait)
                .expect("typed transport interleave released wait");
            released = guard;
        }
    }

    fn wait_reached<T: Send>(
        &self,
        point: InterleavePoint,
        thread: &std::thread::JoinHandle<T>,
    ) -> Result<(), String> {
        let mut reached = self
            .reached
            .lock()
            .expect("typed transport interleave reached");
        let deadline = Instant::now() + Duration::from_secs(120);
        while reached.as_ref() != Some(&point) {
            if thread.is_finished() {
                return Err(format!(
                    "the first thread ended before the {point:?} interleave point"
                ));
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(format!(
                    "the first thread did not reach the {point:?} interleave point"
                ));
            }
            let wait = deadline
                .saturating_duration_since(now)
                .min(Duration::from_millis(100));
            let (guard, _) = self
                .reached_signal
                .wait_timeout(reached, wait)
                .expect("typed transport interleave reached wait");
            reached = guard;
        }
        Ok(())
    }

    fn release(&self, point: InterleavePoint) {
        self.released
            .lock()
            .expect("typed transport interleave release")
            .push(point);
        self.released_signal.notify_all();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arm {
    Positive,
    OtherResourceKey,
    OtherArgumentToken,
    PrecommitBorrowSeam,
    PrecommitUnissuedReceiver,
    PublishedCommit,
    PublishedCommitOtherOwner,
    PublishedCommitUnissuedReceiver,
    PublishedCommitRawArcChanged,
    PublishedHeadProducer,
    PublishedHeadProducerReplacedBeforeRead,
    PublishedHeadProducerReplacedBeforeCommit,
    PublishedHeadProducerWrongSignature,
    CallbackRootRetirement,
    CallbackRootOmission,
    CallbackRootFalseReturn,
}

impl Arm {
    fn label(self) -> &'static str {
        match self {
            Arm::Positive => "positive",
            Arm::OtherResourceKey => "other-resource-key",
            Arm::OtherArgumentToken => "other-argument-token",
            Arm::PrecommitBorrowSeam => "precommit-borrow-seam",
            Arm::PrecommitUnissuedReceiver => "precommit-unissued-receiver",
            Arm::PublishedCommit => "published-commit",
            Arm::PublishedCommitOtherOwner => "published-commit-other-owner",
            Arm::PublishedCommitUnissuedReceiver => "published-commit-unissued-receiver",
            Arm::PublishedCommitRawArcChanged => "published-commit-raw-arc-changed",
            Arm::PublishedHeadProducer => "published-head-producer",
            Arm::PublishedHeadProducerReplacedBeforeRead => {
                "published-head-producer-replaced-before-read"
            }
            Arm::PublishedHeadProducerReplacedBeforeCommit => {
                "published-head-producer-replaced-before-commit"
            }
            Arm::PublishedHeadProducerWrongSignature => "published-head-producer-wrong-signature",
            Arm::CallbackRootRetirement => "callback-root-retirement",
            Arm::CallbackRootOmission => "callback-root-omission",
            Arm::CallbackRootFalseReturn => "callback-root-false-return",
        }
    }

    fn fixture(self) -> &'static str {
        match self {
            Arm::OtherArgumentToken => OTHER_ARGUMENT_FIXTURE,
            Arm::PublishedCommit
            | Arm::PublishedCommitOtherOwner
            | Arm::PublishedCommitUnissuedReceiver
            | Arm::PublishedCommitRawArcChanged => PUBLISHED_COMMIT_FIXTURE,
            Arm::PublishedHeadProducer
            | Arm::PublishedHeadProducerReplacedBeforeRead
            | Arm::PublishedHeadProducerReplacedBeforeCommit
            | Arm::PublishedHeadProducerWrongSignature => PRODUCER_FIXTURE,
            Arm::CallbackRootRetirement
            | Arm::CallbackRootOmission
            | Arm::CallbackRootFalseReturn => RETIREMENT_FIXTURE,
            _ => FIXTURE,
        }
    }

    fn select(self) -> usize {
        match self {
            Arm::OtherResourceKey => 5,
            Arm::PublishedCommit => 3,
            Arm::PublishedCommitOtherOwner => 4,
            Arm::PublishedCommitRawArcChanged => 6,
            Arm::PublishedCommitUnissuedReceiver => 7,
            Arm::PublishedHeadProducer => 8,
            Arm::PublishedHeadProducerReplacedBeforeRead => 9,
            Arm::PublishedHeadProducerReplacedBeforeCommit => 10,
            Arm::PublishedHeadProducerWrongSignature => 11,
            Arm::CallbackRootRetirement | Arm::CallbackRootFalseReturn => 12,
            Arm::CallbackRootOmission => 13,
            _ => 0,
        }
    }

    fn gate_mode(self) -> usize {
        match self {
            Arm::PrecommitBorrowSeam => 1,
            Arm::PrecommitUnissuedReceiver => 2,
            Arm::PublishedCommit => 3,
            Arm::PublishedCommitOtherOwner => 4,
            Arm::PublishedCommitUnissuedReceiver => 5,
            Arm::PublishedCommitRawArcChanged => 6,
            Arm::PublishedHeadProducer => 7,
            Arm::PublishedHeadProducerReplacedBeforeRead => 8,
            Arm::PublishedHeadProducerReplacedBeforeCommit => 9,
            Arm::PublishedHeadProducerWrongSignature => 10,
            Arm::CallbackRootRetirement => 12,
            Arm::CallbackRootOmission => 13,
            Arm::CallbackRootFalseReturn => FALSE_RETURN_GATE,
            _ => 0,
        }
    }

    fn observes_call_result(self) -> bool {
        matches!(
            self,
            Arm::CallbackRootRetirement | Arm::CallbackRootOmission | Arm::CallbackRootFalseReturn
        )
    }
}

// ---- the private test catalog and exact registration ----------------------

/// `(Array<Int>, Resource(lua.userdata)) -> Bool`, the declared prototype.
fn userdata_callable_schema() -> TypeSchema {
    TypeSchema::Callable {
        params: vec![
            TypeSchema::Array(Box::new(TypeSchema::Int)),
            TypeSchema::Resource(LuaUserdataResource::key()),
        ],
        result: Box::new(TypeSchema::Bool),
    }
}

/// The same arity, receiver and result with the other legal resource key.
fn table_callable_schema() -> TypeSchema {
    TypeSchema::Callable {
        params: vec![
            TypeSchema::Array(Box::new(TypeSchema::Int)),
            TypeSchema::Resource(LuaTableResource::key()),
        ],
        result: Box::new(TypeSchema::Bool),
    }
}

/// The exact static import return schema. The callable's own signature is
/// concrete in both parameters and the result; no `Unknown` is accepted here.
fn typed_host_callable_schema() -> HostTypeSchema {
    HostTypeSchema::Callable {
        params: vec![
            HostTypeSchema::Array(Box::new(HostTypeSchema::Int)),
            HostTypeSchema::Resource(LuaUserdataResource::key()),
        ],
        result: Box::new(HostTypeSchema::Bool),
    }
}

/// The second exact static import under test. The callable parameter contains
/// a resource, so this pin only accepts a reference passing mode for it; the
/// direct `lua.userdata` token is borrowed too. The receiver keeps the real
/// `Array<Int>` carrier shape of the issued LuaHeap table.
fn precommit_import_schema() -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        PRECOMMIT_IMPORT,
        vec![
            HostParamSchema::with_passing(
                "target",
                typed_host_callable_schema(),
                HostParamPassing::Borrow,
            ),
            HostParamSchema::value(
                "receiver",
                HostTypeSchema::Array(Box::new(HostTypeSchema::Int)),
            ),
            HostParamSchema::with_passing(
                "token",
                HostTypeSchema::Resource(LuaUserdataResource::key()),
                HostParamPassing::Borrow,
            ),
        ],
        HostTypeSchema::Bool,
    )
}

/// The typed exclusive commit body. It takes the same actual operands as the
/// read-only seam: the typed callable local, the original receiver carrier and
/// the actual typed userdata token. The static body re-checks them against the
/// live published head before it writes anything.
fn commit_import_schema() -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        COMMIT_IMPORT,
        vec![
            HostParamSchema::with_passing(
                "target",
                typed_host_callable_schema(),
                HostParamPassing::Borrow,
            ),
            HostParamSchema::value(
                "receiver",
                HostTypeSchema::Array(Box::new(HostTypeSchema::Int)),
            ),
            HostParamSchema::with_passing(
                "token",
                HostTypeSchema::Resource(LuaUserdataResource::key()),
                HostParamPassing::Borrow,
            ),
        ],
        HostTypeSchema::Bool,
    )
}

/// The read-only exact static that sources the callable from the published
/// head. It has no callable argument: only the actual issued receiver carrier
/// and the actual typed userdata token. The return is the same exact callable
/// schema Core checks for the committed typed local.
fn produce_import_schema() -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        PRODUCER_IMPORT,
        vec![
            HostParamSchema::value(
                "receiver",
                HostTypeSchema::Array(Box::new(HostTypeSchema::Int)),
            ),
            HostParamSchema::with_passing(
                "token",
                HostTypeSchema::Resource(LuaUserdataResource::key()),
                HostParamPassing::Borrow,
            ),
        ],
        typed_host_callable_schema(),
    )
}

fn transport_catalog() -> &'static Arc<HostApiCatalog> {
    static CATALOG: OnceLock<Arc<HostApiCatalog>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut builder = HostApiBuilder::new();
        // Both keys are legally registered resource types before any function
        // signature references them.
        builder.resource(ResourceTypeSchema::new(
            LuaTableResource::key(),
            "Lua table",
        ));
        builder.resource(ResourceTypeSchema::new(
            LuaUserdataResource::key(),
            "Lua userdata",
        ));
        // Markers, the staged arm integer, and the real `lua.table` token
        // issuer. The token is a real registered handle; its schema is
        // deliberately `Unknown` because this one adapter also returns plain
        // marker integers.
        builder.function(HostFunctionSchema::with_return(
            "mark",
            vec![HostParamSchema::value("label", HostTypeSchema::String)],
            HostTypeSchema::Unknown,
        ));
        // The exact typed callable-returning adapter under test.
        builder.function(HostFunctionSchema::with_return(
            "note",
            vec![
                HostParamSchema::value("mode", HostTypeSchema::Int),
                HostParamSchema::value("target", HostTypeSchema::Unknown),
                HostParamSchema::value("first", HostTypeSchema::Unknown),
                HostParamSchema::value("second", HostTypeSchema::Unknown),
                HostParamSchema::value("single", HostTypeSchema::Unknown),
                HostParamSchema::value("other", HostTypeSchema::Unknown),
            ],
            typed_host_callable_schema(),
        ));
        // The second exact static import: the typed local (borrowed, it
        // contains a resource), the original issued receiver carrier and the
        // actual typed userdata token (borrowed). No `Unknown` parameter.
        builder.function(precommit_import_schema());
        // The published-commit fixture's own exact statics. `prepare` issues
        // A, installs the shared `__gc = receiver` and the raw `__call`
        // closure, clears one weak orphan, and finishes the published tail;
        // `wrong_token` returns the queued successor's real typed token;
        // `swap` rewrites the receiver's raw `__call` between the seam and the
        // commit; `commit` is the typed exclusive commit body.
        builder.function(HostFunctionSchema::with_return(
            BIND_IMPORT,
            vec![
                HostParamSchema::value("owner", HostTypeSchema::Unknown),
                HostParamSchema::value("target", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Bool,
        ));
        builder.function(HostFunctionSchema::with_return(
            WRONG_TOKEN_IMPORT,
            Vec::new(),
            HostTypeSchema::Resource(LuaUserdataResource::key()),
        ));
        builder.function(HostFunctionSchema::with_return(
            SWAP_IMPORT,
            vec![
                HostParamSchema::value("owner", HostTypeSchema::Unknown),
                HostParamSchema::value("target", HostTypeSchema::Unknown),
            ],
            HostTypeSchema::Bool,
        ));
        builder.function(commit_import_schema());
        // The read-only typed head producer. It takes only the issued
        // receiver carrier and the actual typed userdata token; the callable
        // itself comes from the heap's published `__gc`/`__call` chain.
        builder.function(produce_import_schema());
        builder.function(HostFunctionSchema::with_return(
            RETIRE_IMPORT,
            Vec::new(),
            HostTypeSchema::Bool,
        ));
        builder.function(HostFunctionSchema::with_return(
            SWEEP_IMPORT,
            Vec::new(),
            HostTypeSchema::Bool,
        ));
        // One freshly issued LuaHeap table carrier, whose real shape is
        // `Array<Int>`.
        builder.function(HostFunctionSchema::with_return(
            "tick",
            vec![HostParamSchema::value("label", HostTypeSchema::String)],
            HostTypeSchema::Array(Box::new(HostTypeSchema::Int)),
        ));
        // One freshly issued typed userdata token.
        builder.function(HostFunctionSchema::with_return(
            "fault",
            Vec::new(),
            HostTypeSchema::Resource(LuaUserdataResource::key()),
        ));
        Arc::new(builder.build().expect("active typed resource catalog"))
    })
}

fn catalog_schema(name: &str) -> HostImportSchema {
    let catalog = transport_catalog();
    let function = catalog
        .function(name)
        .unwrap_or_else(|| panic!("catalog is missing '{name}'"));
    HostImportSchema::from_function(catalog, function)
}

/// Binds every host through the exact static path, so the compiled program and
/// the VM share one catalog schema per import and Core validates the typed
/// return at run time.
fn bind_hosts_for(vm: &mut Vm, retirement: bool) -> Result<(), VmError> {
    let mut registry = HostFunctionRegistry::empty();
    registry.register_exact_static("mark", 1, catalog_schema("mark"), host_mark)?;
    registry.register_exact_static("note", 6, catalog_schema("note"), host_note)?;
    registry.register_exact_static(
        PRECOMMIT_IMPORT,
        3,
        catalog_schema(PRECOMMIT_IMPORT),
        host_precommit,
    )?;
    registry.register_exact_static("tick", 1, catalog_schema("tick"), host_tick)?;
    registry.register_exact_static("fault", 0, catalog_schema("fault"), host_fault)?;
    registry.register_exact_static(BIND_IMPORT, 2, catalog_schema(BIND_IMPORT), host_bind)?;
    registry.register_exact_static(
        WRONG_TOKEN_IMPORT,
        0,
        catalog_schema(WRONG_TOKEN_IMPORT),
        host_wrong_token,
    )?;
    registry.register_exact_static("swap", 2, catalog_schema("swap"), host_swap)?;
    registry.register_exact_static(COMMIT_IMPORT, 3, catalog_schema(COMMIT_IMPORT), host_commit)?;
    registry.register_exact_static(
        PRODUCER_IMPORT,
        2,
        catalog_schema(PRODUCER_IMPORT),
        host_produce,
    )?;
    if retirement {
        registry.register_exact_static(
            RETIRE_IMPORT,
            0,
            catalog_schema(RETIRE_IMPORT),
            active_typed_callback_root_retirement::host_retire,
        )?;
        registry.register_exact_static(
            SWEEP_IMPORT,
            0,
            catalog_schema(SWEEP_IMPORT),
            active_typed_callback_root_retirement::host_sweep,
        )?;
    }
    registry.bind_vm_cached(vm)
}

// ---- observation records --------------------------------------------------

#[derive(Clone, Debug)]
struct TokenProbe {
    raw: i64,
    parts: (u64, u32, u32),
    rebound_parts: (u64, u32, u32),
}

#[derive(Clone, Debug)]
struct TableTokenRecord {
    raw: i64,
    parts: (u64, u32, u32),
}

/// One observation of the second exact static import, recorded by its static
/// body from the actual operands and the live VM before any refusal.
#[derive(Clone, Debug)]
struct GateRecord {
    callable: Value,
    receiver: Value,
    receiver_class: GuestClass,
    receiver_is_issued: bool,
    token_raw: i64,
    token_parts: Option<(u64, u32, u32)>,
    token_is_issued: bool,
    refused: bool,
}

/// One prepared published head, read after the released Release tail finished.
/// It is a fixed snapshot of the real A/B state, not a queue lease and not a
/// root: the commit body must re-read every identity in its own borrow.
#[derive(Clone, Debug)]
struct PublishedPrepareRecord {
    receiver: TableRef,
    receiver_metatable: TableRef,
    finalizer_metatable: TableRef,
    userdata: UserdataRef,
    userdata_raw: i64,
    successor: UserdataRef,
    successor_raw: i64,
    call_closure: Option<heap::ClosureRef>,
    weak_table: TableRef,
    sentinel: TableRef,
    root_frame: heap::RootFrameId,
    published_count: usize,
    queue_len: usize,
}

/// One actual invocation of the typed commit body, recorded from its real
/// operands and the live heap before the exclusive borrow returns.
#[derive(Clone, Debug)]
struct PublishedCommitRecord {
    callable: Value,
    receiver: Value,
    receiver_class: GuestClass,
    token_raw: i64,
    payload_parts: Option<(u64, u32, u32)>,
    payload_is_head: bool,
    weak_matches_callable: bool,
    head_parts: (u64, u32, u32),
    head_raw: i64,
    metatable_parts: (u64, u32, u32),
    action_receiver: (u64, u32, u32),
    action_closure: (u64, u32, u32),
    published_before: usize,
    queue_before: usize,
    outcome: Result<(heap::RootFrameId, u32), String>,
    taken_userdata: Option<UserdataRef>,
}

/// One read-only invocation of the exact static producer. It records the
/// actual heap head identities, the stored callable as a `Weak` only, the
/// actual prototype id of that Arc, and the parent's own callable slot (the
/// old fixture selection) as an identity comparison, never as an extra strong
/// `Arc`. The queue/flag/top-frame fields are read before and after the pass.
#[derive(Clone, Debug)]
struct ProducerRecord {
    userdata: UserdataRef,
    raw_token: i64,
    metatable: TableRef,
    receiver: TableRef,
    closure: heap::ClosureRef,
    callable: std::sync::Weak<vm::CallableValue>,
    published_count: usize,
    queue_len: usize,
    returned_prototype: u32,
    receiver_class: GuestClass,
    payload_is_head: bool,
    token_still_open: bool,
    parent_candidate_matches: bool,
    parent_candidate_prototype: Option<u32>,
    queue_before: Vec<(u64, u32, u32)>,
    queue_after: Vec<(u64, u32, u32)>,
    top_frame_before: Option<heap::RootFrameId>,
    top_frame_after: Option<heap::RootFrameId>,
    pending_before: [bool; 2],
    pending_after: [bool; 2],
    settled_before: [bool; 2],
    settled_after: [bool; 2],
}

/// The first observation of one host callback. Recorded before any frame
/// verification, resource probe, or error wrapping, from the live execution
/// frames alone, so a later probe failure cannot erase whether the callback
/// frame actually entered.
#[derive(Clone, Debug)]
struct EntryRecord {
    /// The first marker the entered frame executes.
    label: String,
    /// The actual execution-frame depth at entry.
    frame_len: usize,
    /// The actual prototype id of the entered (last) frame.
    prototype_id: Option<u32>,
    /// The actual resume address of the entered frame, when it resumes
    /// bytecode through the executed `CallValue`.
    return_ip: Option<usize>,
    /// The actual prototype id of the caller frame.
    parent_prototype: Option<u32>,
}

#[derive(Clone, Debug)]
struct ParentEntryFacts {
    argument_raw: i64,
}

#[derive(Clone, Debug)]
struct CalleeRecord {
    prototype_id: u32,
    parameter_slots: Vec<u16>,
    parameter_values: Vec<Option<Value>>,
    receiver_class: String,
    ordinary_array_class: String,
    token: TokenProbe,
    table_token_refused: bool,
    caller_callable: Option<Value>,
    caller_declared_slot: Option<u16>,
    parent_same_arc_slots: Vec<u16>,
}

#[derive(Clone, Debug)]
struct MarkRecord {
    label: String,
    frame_len: usize,
    last_prototype: Option<u32>,
    last_return_ip: Option<usize>,
    parent_prototype: Option<u32>,
    parent_entry: Option<ParentEntryFacts>,
    continuation: Option<TokenProbe>,
    child_return: Option<Value>,
    callee: Option<CalleeRecord>,
}

#[derive(Clone, Debug)]
struct PickRecord {
    mode: i64,
    target: Value,
    first: Value,
    second: Value,
    single: Value,
    other: Value,
    chosen: Value,
    chosen_is_target: bool,
    chosen_is_other: bool,
}

#[derive(Clone, Debug, Default)]
struct Witness {
    entries: Vec<EntryRecord>,
    marks: Vec<MarkRecord>,
    picks: Vec<PickRecord>,
    gates: Vec<GateRecord>,
    prepared: Option<PublishedPrepareRecord>,
    commits: Vec<PublishedCommitRecord>,
    producers: Vec<ProducerRecord>,
    issued_table: Option<Value>,
    issued_table_ref: Option<TableRef>,
    issued_userdata: Option<UserdataRef>,
    issued_userdata_raw: Option<i64>,
    table_token: Option<TableTokenRecord>,
    /// The compiled call-result slot and the value read from it at parent-after.
    call_result_slot: Option<LocalSlot>,
    call_result: Option<Value>,
    /// True when some other parent local is still `Bool(true)`.
    host_true_beside_call: bool,
}

/// The independent entry record of the active (last) frame, read from the live
/// frames alone before any host-side probe can fail.
fn frame_entry(vm: &Vm, label: &str) -> EntryRecord {
    let frames = vm.execution_frames();
    let current = frames.last();
    EntryRecord {
        label: label.to_string(),
        frame_len: frames.len(),
        prototype_id: current.and_then(|frame| frame.prototype_id),
        return_ip: current.and_then(|frame| match &frame.continuation {
            VmFrameContinuation::ResumeBytecode { return_ip } => Some(*return_ip),
            _ => None,
        }),
        parent_prototype: frames
            .iter()
            .rev()
            .nth(1)
            .and_then(|frame| frame.prototype_id),
    }
}

fn host_fault_error(message: impl Into<String>) -> VmError {
    VmError::HostError(message.into())
}

fn heap_fault(error: heap::HeapError) -> VmError {
    VmError::HostError(format!("lua heap: {error:?}"))
}

// ---- typed token probes ---------------------------------------------------

/// The formal Core API: a raw handle is validated by the resource table and
/// then borrowed with the exact concrete `lua.userdata` key. The payload is
/// copied out before the borrow ends, so a heap check can follow.
fn probe_userdata(host: &vm::HostContext, raw: i64) -> Result<TokenProbe, VmError> {
    let raw_u64 = u64::try_from(raw)
        .map_err(|_| host_fault_error("the userdata token is not a positive handle"))?;
    let handle = ResourceHandle::from_raw(raw_u64)
        .map_err(|error| host_fault_error(format!("the userdata handle is invalid: {error}")))?;
    let payload = {
        let borrowed = host
            .borrow_resource_with_key::<LuaUserdataResource>(handle, &LuaUserdataResource::key())
            .map_err(|error| {
                host_fault_error(format!("the lua.userdata borrow was refused: {error}"))
            })?;
        borrowed.get().userdata()
    };
    let rebound = heap::borrow_published_userdata(host, payload).map_err(heap_fault)?;
    Ok(TokenProbe {
        raw,
        parts: (payload.domain(), payload.slot(), payload.generation()),
        rebound_parts: (rebound.domain(), rebound.slot(), rebound.generation()),
    })
}

/// The same formal borrow with the other registered key. It proves the token
/// issued by `mark("table-token")` is a real `lua.table` resource.
fn probe_table(host: &vm::HostContext, raw: i64) -> Result<(u64, u32, u32), VmError> {
    let raw_u64 = u64::try_from(raw)
        .map_err(|_| host_fault_error("the table token is not a positive handle"))?;
    let handle = ResourceHandle::from_raw(raw_u64)
        .map_err(|error| host_fault_error(format!("the table handle is invalid: {error}")))?;
    let borrowed = host
        .borrow_resource_with_key::<LuaTableResource>(handle, &LuaTableResource::key())
        .map_err(|error| host_fault_error(format!("the lua.table borrow was refused: {error}")))?;
    let table = borrowed.get().table();
    Ok((table.domain(), table.slot(), table.generation()))
}

// ---- the hosts ------------------------------------------------------------

fn host_mark(vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
    let [Value::String(label)] = args else {
        return Err(host_fault_error("mark expects one string label"));
    };
    let label = label.as_str().to_string();

    // The first observation of this host callback. This runs before every
    // fallible frame check, resource probe and error wrapper below, so a
    // probe that fails after the frame really entered still leaves the
    // entered frame's actual prototype, depth and resume address recorded.
    let entry = frame_entry(vm, &label);
    with_run_scope(vm, |scope| scope.witness.entries.push(entry));

    if label == "chunk-mode" {
        interleave_pause(InterleavePoint::Runtime);
        let select = i64::try_from(with_run_scope(vm, |scope| scope.select))
            .map_err(|_| host_fault_error("the selected arm does not fit a guest integer"))?;
        return Ok(CallOutcome::Return(CallReturn::one(Value::Int(select))));
    }

    if label == "table-token" {
        let mut host = vm.host_context();
        let issued = heap::publish(&mut host).map_err(heap_fault)?;
        let Value::Int(raw) = issued else {
            return Err(host_fault_error("the issued table token is not a handle"));
        };
        let parts = probe_table(&host, raw)?;
        with_run_scope(vm, |scope| {
            scope.witness.table_token = Some(TableTokenRecord { raw, parts });
        });
        return Ok(CallOutcome::Return(CallReturn::one(Value::Int(raw))));
    }

    let parent_entry;
    let continuation;
    let child_return;
    let frame;
    let (frame_len, last_prototype, last_return_ip, parent_prototype) = {
        let frames = vm.execution_frames();
        let program = vm.program();
        let locals = vm.locals();
        let current = frames.last();
        let parent = frames.iter().rev().nth(1);

        parent_entry = (label == "parent-enter")
            .then(|| {
                current.and_then(|frame| {
                    let prototype_id = frame.prototype_id?;
                    let prototype = program.callable_prototypes.get(prototype_id as usize)?;
                    if prototype.parameter_slots.len() != PARENT_PARAMS
                        && prototype.parameter_slots.len() != PUBLISHED_PARENT_PARAMS
                    {
                        return None;
                    }
                    let slot = *prototype.parameter_slots.get(PARENT_ARGUMENT_SLOT)?;
                    let value = locals.get(frame.local_base + usize::from(slot))?;
                    let Value::Int(raw) = value else {
                        return None;
                    };
                    Some(ParentEntryFacts { argument_raw: *raw })
                })
            })
            .flatten();

        continuation = (label == "parent-after")
            .then(|| {
                current.and_then(|frame| {
                    let prototype_id = frame.prototype_id?;
                    let prototype = program.callable_prototypes.get(prototype_id as usize)?;
                    if prototype.parameter_slots.len() != PARENT_PARAMS
                        && prototype.parameter_slots.len() != PUBLISHED_PARENT_PARAMS
                    {
                        return None;
                    }
                    let slot = *prototype.parameter_slots.get(PARENT_ARGUMENT_SLOT)?;
                    let value = locals.get(frame.local_base + usize::from(slot))?;
                    let Value::Int(raw) = value else {
                        return None;
                    };
                    Some(*raw)
                })
            })
            .flatten();

        child_return = (label == "parent-after")
            .then(|| {
                current.and_then(|frame| {
                    let values =
                        locals.get(frame.local_base..frame.local_base + frame.local_count)?;
                    values
                        .iter()
                        .find(|value| matches!(value, Value::Bool(true)))
                        .cloned()
                })
            })
            .flatten();

        frame = current.and_then(|child| {
            let prototype_id = child.prototype_id?;
            let prototype = program.callable_prototypes.get(prototype_id as usize)?;
            if prototype.parameter_slots.len() != usize::from(CALL_ARGC) {
                return None;
            }
            let declared = prototype.parameter_slots.clone();
            let mut values = Vec::with_capacity(declared.len());
            for slot in &declared {
                let absolute = child.local_base.checked_add(usize::from(*slot))?;
                if absolute >= child.local_base + child.local_count {
                    return None;
                }
                values.push(locals.get(absolute).cloned());
            }
            let mut caller_callable = None;
            let mut caller_declared_slot = None;
            let mut parent_same_arc_slots = Vec::new();
            if let Some(caller) = parent
                && let Some(caller_prototype) = caller
                    .prototype_id
                    .and_then(|id| program.callable_prototypes.get(id as usize))
                && let Some(&slot) = caller_prototype.parameter_slots.first()
                && let Some(absolute) = caller.local_base.checked_add(usize::from(slot))
            {
                caller_declared_slot = Some(slot);
                caller_callable = locals.get(absolute).cloned();
                if let Some(Value::Callable(caller_arc)) = &caller_callable {
                    let start = caller.local_base;
                    let end = caller.local_base + caller.local_count;
                    if let Some(values) = locals.get(start..end) {
                        for (offset, value) in values.iter().enumerate() {
                            if let Value::Callable(candidate) = value
                                && Arc::ptr_eq(candidate, caller_arc)
                            {
                                parent_same_arc_slots.push(offset as u16);
                            }
                        }
                    }
                }
            }
            Some((
                prototype_id,
                child.local_base,
                child.local_count,
                declared,
                values,
                caller_callable,
                caller_declared_slot,
                parent_same_arc_slots,
            ))
        });

        (
            frames.len(),
            current.and_then(|frame| frame.prototype_id),
            current.and_then(|frame| match &frame.continuation {
                VmFrameContinuation::ResumeBytecode { return_ip } => Some(*return_ip),
                _ => None,
            }),
            parent.and_then(|frame| frame.prototype_id),
        )
    };

    let mut record = MarkRecord {
        label: label.clone(),
        frame_len,
        last_prototype,
        last_return_ip,
        parent_prototype,
        parent_entry,
        continuation: None,
        child_return,
        callee: None,
    };

    if let Some((
        prototype_id,
        _local_base,
        _local_count,
        slots,
        values,
        caller,
        caller_slot,
        same_arc,
    )) = frame
    {
        let receiver = values
            .first()
            .cloned()
            .flatten()
            .ok_or_else(|| host_fault_error("the callee's first formal slot is empty"))?;
        let token = values
            .get(1)
            .cloned()
            .flatten()
            .ok_or_else(|| host_fault_error("the callee's second formal slot is empty"))?;
        let Value::Int(raw) = token else {
            return Err(host_fault_error(format!(
                "the callee's second formal slot is not a handle token: {token:?}"
            )));
        };
        let mut host = vm.host_context();
        let receiver_class = heap::with_heap(&mut host, |heap| Ok(heap.guest_class(&receiver)))
            .map_err(heap_fault)?;
        let ordinary = Value::array(vec![Value::Int(0), Value::Int(0)]);
        let ordinary_class = heap::with_heap(&mut host, |heap| Ok(heap.guest_class(&ordinary)))
            .map_err(heap_fault)?;
        let token_probe = probe_userdata(&host, raw)?;
        let table_token_refused = host
            .borrow_resource_with_key::<LuaTableResource>(
                raw_handle(raw)?,
                &LuaTableResource::key(),
            )
            .is_err();
        record.callee = Some(CalleeRecord {
            prototype_id,
            parameter_slots: slots,
            parameter_values: values,
            receiver_class: format!("{receiver_class:?}"),
            ordinary_array_class: format!("{ordinary_class:?}"),
            token: token_probe,
            table_token_refused,
            caller_callable: caller,
            caller_declared_slot: caller_slot,
            parent_same_arc_slots: same_arc,
        });
    }

    if let Some(raw) = continuation {
        let host = vm.host_context();
        record.continuation = Some(probe_userdata(&host, raw)?);
    }

    if label == "parent-after" {
        let slot = with_run_scope(vm, |scope| scope.call_result_slot);
        if let Some(slot) = slot {
            let (value, host_true) = {
                let frames = vm.execution_frames();
                let locals = vm.locals();
                let current = frames.last();
                let value = current.and_then(|frame| {
                    let absolute = frame.local_base.checked_add(usize::from(slot))?;
                    locals.get(absolute).cloned()
                });
                let host_true = current.is_some_and(|frame| {
                    locals
                        .get(frame.local_base..frame.local_base + frame.local_count)
                        .is_some_and(|values| {
                            values.iter().enumerate().any(|(index, local)| {
                                index != usize::from(slot) && matches!(local, Value::Bool(true))
                            })
                        })
                });
                (value, host_true)
            };
            with_run_scope(vm, |scope| {
                scope.witness.call_result_slot = Some(slot);
                scope.witness.call_result = value;
                scope.witness.host_true_beside_call = host_true;
            });
        }
    }

    with_run_scope(vm, |scope| scope.witness.marks.push(record));
    Ok(CallOutcome::Return(CallReturn::one(Value::Int(0))))
}

/// The VM local written by the `Stloc` immediately after the real
/// `CallValue(2)`. That is the compiled result slot, not the IR temp.
fn compiled_call_result_slot(program: &Program) -> Option<LocalSlot> {
    let code = &program.code;
    let mut ip = 0;
    while ip < code.len() {
        let op = OpCode::try_from(code[ip]).ok()?;
        let next = ip + 1 + op.operand_len();
        if next > code.len() {
            return None;
        }
        if op == OpCode::CallValue && code[ip + 1] == CALL_ARGC {
            if next < code.len() && OpCode::try_from(code[next]).ok() == Some(OpCode::Stloc) {
                return LocalSlot::try_from(usize::from(code[next + 1])).ok();
            }
            return None;
        }
        ip = next;
    }
    None
}

/// The raw handle of one guest token, through the formal Core constructor.
fn raw_handle(raw: i64) -> vm::VmResult<ResourceHandle> {
    let raw =
        u64::try_from(raw).map_err(|_| host_fault_error("the token is not a positive handle"))?;
    ResourceHandle::from_raw(raw)
        .map_err(|error| host_fault_error(format!("the token handle is invalid: {error}")))
}

/// Returns one of the candidates the fixture already wrote at the call site.
/// The returned value is always the exact `SharedCallable` the VM produced.
fn host_note(vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
    let [Value::Int(mode), target, first, second, single, other] = args else {
        return Err(host_fault_error(
            "note expects a mode and five written candidates",
        ));
    };
    let chosen = match *mode {
        0 | 3 | 4 | 6 | 7 => target,
        5 => other,
        // The producer arms still call the old selection once. Its staged mode
        // is the published-head arm selector, and like every other published
        // arm it names the written target candidate.
        8..=13 => target,
        other_mode => {
            return Err(host_fault_error(format!(
                "note mode {other_mode} is not a fixture arm"
            )));
        }
    };
    if !matches!(chosen, Value::Callable(_)) {
        return Err(host_fault_error(
            "the selected typed candidate is not a callable",
        ));
    }
    let record = PickRecord {
        mode: *mode,
        target: target.clone(),
        first: first.clone(),
        second: second.clone(),
        single: single.clone(),
        other: other.clone(),
        chosen: chosen.clone(),
        chosen_is_target: same_callable(chosen, target),
        chosen_is_other: same_callable(chosen, other),
    };
    with_run_scope(vm, |scope| scope.witness.picks.push(record));
    Ok(CallOutcome::Return(CallReturn::one(chosen.clone())))
}

/// Issues one real LuaHeap table and returns its guest carrier.
fn host_tick(vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
    let [Value::String(_label)] = args else {
        return Err(host_fault_error("tick expects one string label"));
    };
    let mut host = vm.host_context();
    let issued = heap::publish_guest(&mut host).map_err(heap_fault)?;
    let class =
        heap::with_heap(&mut host, |heap| Ok(heap.guest_class(&issued))).map_err(heap_fault)?;
    let GuestClass::Table(table) = class else {
        return Err(host_fault_error(
            "the issued carrier is not a live heap table",
        ));
    };
    with_run_scope(vm, |scope| {
        scope.witness.issued_table_ref = Some(table);
        scope.witness.issued_table = Some(issued.clone());
    });
    Ok(CallOutcome::Return(CallReturn::one(issued)))
}

/// Issues one real typed `lua.userdata` token, or, for the published-commit
/// arms, builds the complete published A/B state first.
///
/// The published arms select at least three. Their receiver must already be the
/// table `tick` issued; this body gives it a metatable with the false raw
/// `__call` sentinel, publishes B and then A as real typed userdata, attaches
/// their shared `__gc = receiver` metatable, roots a separate weak table and
/// one unrelated sentinel, runs the real selecting collection, and finishes
/// the saved Release tail. The exact `bind` import runs after this and installs
/// the raw `__call`. The returned token is A's real typed `lua.userdata`.
fn host_fault(vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
    if !args.is_empty() {
        return Err(host_fault_error("fault takes no arguments"));
    }
    let select = with_run_scope(vm, |scope| scope.select);
    if select >= 3 {
        return host_build_published_head(vm);
    }
    let mut host = vm.host_context();
    let userdata = heap::publish_userdata(&mut host).map_err(heap_fault)?;
    let raw = heap::with_heap(&mut host, |heap| heap.published_userdata_token(userdata))
        .map_err(heap_fault)?;
    let raw = i64::try_from(raw)
        .map_err(|_| host_fault_error("the userdata token does not fit a signed carrier"))?;
    with_run_scope(vm, |scope| {
        scope.witness.issued_userdata = Some(userdata);
        scope.witness.issued_userdata_raw = Some(raw);
    });
    Ok(CallOutcome::Return(CallReturn::one(Value::Int(raw))))
}

/// The second exact static import. It is bound with `register_exact_static`,
/// so Core hands the actual operands to this body before the parent's own
/// `CallValue(2)` without an owned-dispatch argument preflight. The body
/// records the real callable Arc, the heap classification of the receiver and
/// the borrowed userdata payload, then refuses with its own `HostError` when
/// the receiver is not the issued carrier. It returns one fixed `Bool` and
/// writes nothing.
fn host_precommit(vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
    // The independent raw entry record comes first: it is taken from the live
    // frames alone, so a later probe failure cannot erase that this static
    // body ran in the parent frame.
    let entry = frame_entry(vm, PRECOMMIT_ENTRY_LABEL);
    with_run_scope(vm, |scope| scope.witness.entries.push(entry));

    let [callable, receiver, token] = args else {
        return Err(host_fault_error(
            "the precommit static import expects three operands",
        ));
    };
    let Value::Callable(_) = callable else {
        return Err(host_fault_error(
            "the precommit static import received a non-callable first operand",
        ));
    };
    let Value::Int(token_raw) = *token else {
        return Err(host_fault_error(
            "the precommit static import received a non-handle token operand",
        ));
    };

    // The exact static `note` has already selected the callable and the
    // fixture has already issued the one carrier and the one userdata token.
    let (issued_table, issued_userdata) = with_run_scope(vm, |scope| {
        assert!(
            !scope.witness.picks.is_empty(),
            "the exact static note must run before the precommit import"
        );
        (
            scope.witness.issued_table_ref,
            scope.witness.issued_userdata,
        )
    });

    let mut host = vm.host_context();
    let receiver_class =
        heap::with_heap(&mut host, |heap| Ok(heap.guest_class(receiver))).map_err(heap_fault)?;
    let receiver_is_issued = match (&receiver_class, issued_table) {
        (GuestClass::Table(table), Some(issued)) => *table == issued,
        _ => false,
    };
    let payload = {
        let borrowed = host
            .borrow_resource_with_key::<LuaUserdataResource>(
                raw_handle(token_raw)?,
                &LuaUserdataResource::key(),
            )
            .map_err(|error| {
                host_fault_error(format!("the precommit token borrow was refused: {error}"))
            })?;
        borrowed.get().userdata()
    };
    let payload_rebound = heap::borrow_published_userdata(&host, payload).map_err(heap_fault)?;
    let token_is_issued = issued_userdata == Some(payload) && payload == payload_rebound;
    let token_parts = (payload.domain(), payload.slot(), payload.generation());

    let record = GateRecord {
        callable: callable.clone(),
        receiver: receiver.clone(),
        receiver_class,
        receiver_is_issued,
        token_raw,
        token_parts: Some(token_parts),
        token_is_issued,
        refused: !receiver_is_issued,
    };
    with_run_scope(vm, |scope| scope.witness.gates.push(record));

    if !receiver_is_issued {
        return Err(host_fault_error(PRECOMMIT_REFUSAL_RECEIVER));
    }
    Ok(CallOutcome::Return(CallReturn::one(Value::Bool(true))))
}

/// Builds the real published typed-head state for the published-commit arms.
///
/// The receiver must be the table `tick` issued without a metatable. This body
/// installs the receiver's own metatable with the false raw `__call` sentinel,
/// hands the exact target `Arc` into that record through the same private heap
/// admission the controlled field import uses, publishes B and then A as real
/// typed userdata, attaches their shared `__gc = receiver` metatable, roots a
/// separate weak table and one unrelated sentinel, publishes them with the
/// real selecting collection, and finishes the saved Release tail. Only then
/// does it read the fixed A/B identities and return A's real typed token.
fn host_build_published_head(vm: &mut Vm) -> vm::VmResult<CallOutcome> {
    let receiver_ref = with_run_scope(vm, |scope| scope.witness.issued_table_ref)
        .ok_or_else(|| host_fault_error("fault requires the issued receiver table"))?;
    let receiver_metatable = {
        let mut host = vm.host_context();
        let existing =
            heap::with_heap(&mut host, |heap| heap.metatable(receiver_ref)).map_err(heap_fault)?;
        if existing.is_some() {
            return Err(host_fault_error(
                "fault expects a receiver without a metatable",
            ));
        }
        let metatable = heap::with_heap(&mut host, |heap| heap.alloc()).map_err(heap_fault)?;
        heap::with_heap(&mut host, |heap| {
            heap.set_metatable(receiver_ref, Some(metatable))
        })
        .map_err(heap_fault)?;
        heap::with_heap(&mut host, |heap| {
            heap.set_named_scalar(metatable, "__call", Value::Bool(false))
        })
        .map_err(heap_fault)?;
        metatable
    };

    let mut host = vm.host_context();
    // B is published first and A second, so the selecting scan's reverse
    // allocation order makes A the published head and B its successor.
    let successor = heap::publish_userdata(&mut host).map_err(heap_fault)?;
    let successor_raw = heap::with_heap(&mut host, |heap| heap.published_userdata_token(successor))
        .map_err(heap_fault)?;
    let userdata = heap::publish_userdata(&mut host).map_err(heap_fault)?;
    let userdata_raw = heap::with_heap(&mut host, |heap| heap.published_userdata_token(userdata))
        .map_err(heap_fault)?;
    let finalizer_metatable =
        heap::with_heap(&mut host, |heap| heap.alloc()).map_err(heap_fault)?;
    heap::with_heap(&mut host, |heap| {
        heap.set_named_edge(finalizer_metatable, "__gc", receiver_ref)
    })
    .map_err(heap_fault)?;
    for candidate in [userdata, successor] {
        heap::with_heap(&mut host, |heap| {
            heap.set_userdata_metatable(candidate, Some(finalizer_metatable))
        })
        .map_err(heap_fault)?;
    }

    // One real issued-table weak-value orphan: the real selecting publication
    // clears the only weak reference to it.
    let orphan = {
        let value = heap::publish_guest(&mut host).map_err(heap_fault)?;
        let class =
            heap::with_heap(&mut host, |heap| Ok(heap.guest_class(&value))).map_err(heap_fault)?;
        let GuestClass::Table(orphan) = class else {
            return Err(host_fault_error(
                "prepare's weak orphan must be an issued heap table",
            ));
        };
        orphan
    };
    heap::with_heap(&mut host, |heap| heap.set_edge(orphan, 1, orphan)).map_err(heap_fault)?;
    let weak_table = heap::with_heap(&mut host, |heap| heap.alloc()).map_err(heap_fault)?;
    let weak_mode = heap::with_heap(&mut host, |heap| heap.alloc()).map_err(heap_fault)?;
    heap::with_heap(&mut host, |heap| {
        heap.set_mode_scalar(weak_mode, Value::string("v"))
    })
    .map_err(heap_fault)?;
    heap::with_heap(&mut host, |heap| {
        heap.set_metatable(weak_table, Some(weak_mode))
    })
    .map_err(heap_fault)?;
    heap::with_heap(&mut host, |heap| {
        heap.set_number_edge(weak_table, 1.0, orphan)
    })
    .map_err(heap_fault)?;
    let sentinel = heap::with_heap(&mut host, |heap| heap.alloc()).map_err(heap_fault)?;
    let root_frame = heap::with_heap(&mut host, |heap| heap.enter_roots(4)).map_err(heap_fault)?;
    heap::with_heap(&mut host, |heap| heap.write_root(root_frame, 0, weak_table))
        .map_err(heap_fault)?;
    heap::with_heap(&mut host, |heap| {
        heap.write_root(root_frame, 1, receiver_ref)
    })
    .map_err(heap_fault)?;
    heap::with_heap(&mut host, |heap| heap.write_root(root_frame, 2, sentinel))
        .map_err(heap_fault)?;

    let queue_before = heap::pending_finalizer_count(&mut host).map_err(heap_fault)?;
    if queue_before != 0 {
        return Err(host_fault_error(
            "prepare expects an empty pending queue before publication",
        ));
    }
    let boundary = heap::collect_pending_finalizers(&mut host).map_err(heap_fault)?;
    if !matches!(boundary, heap::PublishedBoundary::Published { .. }) {
        return Err(host_fault_error(
            "prepare's real selecting collection must save a Release tail",
        ));
    }
    let finished = heap::resume_published_sweep(&mut host, u64::MAX).map_err(heap_fault)?;
    if !matches!(finished, heap::PublishedResume::Completed(_)) {
        return Err(host_fault_error(
            "prepare must finish the published Release tail",
        ));
    }
    let published_count = heap::pending_finalizer_published_count(&mut host).map_err(heap_fault)?;
    let queue_len = heap::pending_finalizer_count(&mut host).map_err(heap_fault)?;
    if published_count != 2 || queue_len != 2 {
        return Err(host_fault_error(format!(
            "prepare expected A and B published, saw published={published_count} queue={queue_len}"
        )));
    }
    let weak_cleared = heap::with_heap(&mut host, |heap| heap.number_value(weak_table, 1.0))
        .map(|value| value == Value::Null)
        .map_err(heap_fault)?;
    if !weak_cleared {
        return Err(host_fault_error(
            "prepare's independent weak value must be cleared at publication",
        ));
    }
    let userdata_raw_i64 = i64::try_from(userdata_raw)
        .map_err(|_| host_fault_error("the prepared token does not fit a signed carrier"))?;
    let record = PublishedPrepareRecord {
        receiver: receiver_ref,
        receiver_metatable,
        finalizer_metatable,
        userdata,
        userdata_raw: userdata_raw_i64,
        successor,
        successor_raw: i64::try_from(successor_raw)
            .map_err(|_| host_fault_error("the successor token does not fit a signed carrier"))?,
        call_closure: None,
        weak_table,
        sentinel,
        root_frame,
        published_count,
        queue_len,
    };
    let _ = host;
    with_run_scope(vm, |scope| {
        scope.witness.issued_table_ref = Some(receiver_ref);
        scope.witness.issued_userdata = Some(userdata);
        scope.witness.issued_userdata_raw = Some(userdata_raw_i64);
        scope.witness.prepared = Some(record);
    });
    Ok(CallOutcome::Return(CallReturn::one(Value::Int(
        userdata_raw_i64,
    ))))
}

/// The read-only exact static producer.
///
/// The two operands are the actual parent-frame values: the original issued
/// receiver carrier and the actual typed userdata token. This body never
/// receives a candidate callable. It borrows the token payload through the
/// formal Core API, then reads the published head, the current userdata
/// metatable's raw `__gc`, and one layer of raw `__call` through the heap
/// owner's read-only preflight. That preflight returns the stored callable as
/// a `Weak`, and this body upgrades exactly one strong `Arc` to return to
/// Core, which validates it against the exact `(Array<Int>,
/// Resource(lua.userdata)) -> Bool` return schema and stores it in the typed
/// local. The record keeps only heap identities, the `Weak`, the returned
/// prototype id, and a pointer comparison against the parent's own callable
/// slot, so this body adds no second strong reference and opens no root.
fn host_produce(vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
    let entry = frame_entry(vm, PRODUCER_ENTRY_LABEL);
    with_run_scope(vm, |scope| scope.witness.entries.push(entry));

    let [receiver, token] = args else {
        return Err(host_fault_error(
            "the typed producer expects the receiver and token operands",
        ));
    };
    let Value::Int(token_raw) = *token else {
        return Err(host_fault_error(
            "the typed producer received a non-handle token operand",
        ));
    };
    let token_u64 = u64::try_from(token_raw)
        .map_err(|_| host_fault_error("the userdata token is not a positive handle"))?;

    let prepared = with_run_scope(vm, |scope| scope.witness.prepared.clone())
        .ok_or_else(|| host_fault_error("produce requires the prepared published head"))?;

    // The actual typed payload comes from the Core resource table with the
    // exact concrete key. The borrow ends before any heap read.
    let payload = {
        let host = vm.host_context();
        let borrowed = host
            .borrow_resource_with_key::<LuaUserdataResource>(
                raw_handle(token_raw)?,
                &LuaUserdataResource::key(),
            )
            .map_err(|error| {
                host_fault_error(format!("the producer token borrow was refused: {error}"))
            })?;
        borrowed.get().userdata()
    };

    // The parent's own first parameter is the old fixture selection. Read its
    // prototype id and later its exact Arc identity by reference; no clone of
    // that candidate Arc is kept by this body.
    let parent_candidate_prototype = {
        let frames = vm.execution_frames();
        let program = vm.program();
        let locals = vm.locals();
        frames.last().and_then(|frame| {
            let prototype = frame
                .prototype_id
                .and_then(|id| program.callable_prototypes.get(id as usize))?;
            if prototype.parameter_slots.len() != PUBLISHED_PARENT_PARAMS {
                return None;
            }
            let slot = *prototype.parameter_slots.first()?;
            match locals.get(frame.local_base.checked_add(usize::from(slot))?) {
                Some(Value::Callable(candidate)) => Some(candidate.prototype_id),
                _ => None,
            }
        })
    };

    let (read, receiver_class) = {
        let mut host = vm.host_context();
        let receiver_class = heap::with_heap(&mut host, |heap| Ok(heap.guest_class(receiver)))
            .map_err(heap_fault)?;
        let read = read_published_typed_call_head(
            &mut host,
            receiver,
            Some(payload),
            token_u64,
            prepared.successor,
            prepared.root_frame,
        )
        .map_err(heap_fault)?;
        (read, receiver_class)
    };
    let token_still_open = {
        let host = vm.host_context();
        host.typed_resource_with_key::<LuaUserdataResource>(
            raw_handle(token_raw)?,
            &LuaUserdataResource::key(),
        )
        .is_ok()
    };
    let callable = read
        .callable
        .upgrade()
        .ok_or_else(|| host_fault_error("the published head callable is no longer live"))?;
    // Re-run the identity comparison against the upgraded heap Arc. The
    // parent slot is read by reference and is never cloned here; this is the
    // only strong Arc the producer returns.
    let parent_candidate_matches = {
        let frames = vm.execution_frames();
        let program = vm.program();
        let locals = vm.locals();
        frames.last().is_some_and(|frame| {
            frame
                .prototype_id
                .and_then(|id| program.callable_prototypes.get(id as usize))
                .filter(|prototype| prototype.parameter_slots.len() == PUBLISHED_PARENT_PARAMS)
                .and_then(|prototype| prototype.parameter_slots.first())
                .and_then(|slot| locals.get(frame.local_base.checked_add(usize::from(*slot))?))
                .is_some_and(|value| match value {
                    Value::Callable(candidate) => Arc::ptr_eq(candidate, &callable),
                    _ => false,
                })
        })
    };
    let record = ProducerRecord {
        userdata: read.userdata,
        raw_token: i64::try_from(read.raw_token).unwrap_or(i64::MAX),
        metatable: read.metatable,
        receiver: read.receiver,
        closure: read.closure,
        callable: read.callable.clone(),
        published_count: read.published_count,
        queue_len: read.queue_len,
        returned_prototype: callable.prototype_id,
        receiver_class,
        payload_is_head: payload == read.userdata,
        token_still_open,
        parent_candidate_matches,
        parent_candidate_prototype,
        queue_before: read.queue_before,
        queue_after: read.queue_after,
        top_frame_before: read.top_frame_before,
        top_frame_after: read.top_frame_after,
        pending_before: read.pending_before,
        pending_after: read.pending_after,
        settled_before: read.settled_before,
        settled_after: read.settled_after,
    };
    with_run_scope(vm, |scope| scope.witness.producers.push(record));
    Ok(CallOutcome::Return(CallReturn::one(Value::Callable(
        callable,
    ))))
}

/// Installs the exact target `Arc` into the receiver's own metatable raw
/// `__call` record, the same heap admission the controlled field import runs,
/// and pins that installed closure generation in the prepared witness.
fn host_bind(vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
    let [owner, target] = args else {
        return Err(host_fault_error("bind expects the owner and the callable"));
    };
    let Value::Callable(target_arc) = target else {
        return Err(host_fault_error("bind expects a callable target"));
    };
    install_guest_callable_into_raw_call(vm, owner, target).map_err(heap_fault)?;
    let (receiver, metatable, closure) = {
        let mut host = vm.host_context();
        let class =
            heap::with_heap(&mut host, |heap| Ok(heap.guest_class(owner))).map_err(heap_fault)?;
        let GuestClass::Table(receiver) = class else {
            return Err(host_fault_error(
                "bind expects the issued receiver table carrier",
            ));
        };
        let metatable = heap::with_heap(&mut host, |heap| heap.metatable(receiver))
            .map_err(heap_fault)?
            .ok_or_else(|| host_fault_error("bind expects the receiver metatable"))?;
        let closure = heap::with_heap(&mut host, |heap| {
            heap.named_closure_edge(metatable, "__call")
        })
        .map_err(heap_fault)?;
        let stored = heap::with_heap(&mut host, |heap| heap.closure_callable(closure))
            .map_err(heap_fault)?;
        if !Arc::ptr_eq(&stored, target_arc) {
            return Err(host_fault_error(
                "bind's raw __call must hold the exact target Arc",
            ));
        }
        (receiver, metatable, closure)
    };
    let stored = with_run_scope(vm, |scope| {
        let Some(record) = scope.witness.prepared.as_mut() else {
            return false;
        };
        if record.receiver != receiver {
            return false;
        }
        record.receiver_metatable = metatable;
        record.call_closure = Some(closure);
        true
    });
    if !stored {
        return Err(host_fault_error(
            "bind must follow the prepared published head for its own receiver",
        ));
    }
    Ok(CallOutcome::Return(CallReturn::one(Value::Bool(true))))
}

/// Returns the queued successor's real typed token. It is a legal
/// `lua.userdata` handle naming a different object than the published head.
fn host_wrong_token(vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
    if !args.is_empty() {
        return Err(host_fault_error("wrong_token takes no arguments"));
    }
    let raw = with_run_scope(vm, |scope| {
        scope
            .witness
            .prepared
            .as_ref()
            .map(|record| record.successor_raw)
    })
    .ok_or_else(|| host_fault_error("wrong_token requires the prepared published head"))?;
    Ok(CallOutcome::Return(CallReturn::one(Value::Int(raw))))
}

/// Rewrites the receiver's raw `__call` to the given zero-capture closure
/// between the typed seam and the typed commit. Only the arm whose selected
/// mode is the raw-edge-change control performs the real heap admission; the
/// other published arms execute the same compiled call but keep their edge.
fn host_swap(vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
    let [owner, target] = args else {
        return Err(host_fault_error("swap expects the owner and the callable"));
    };
    let select = with_run_scope(vm, |scope| scope.select);
    if select == 6 || select == 10 {
        install_guest_callable_into_raw_call(vm, owner, target).map_err(heap_fault)?;
    }
    Ok(CallOutcome::Return(CallReturn::one(Value::Bool(true))))
}

/// The typed exclusive commit body.
///
/// The operands are the parent frame's actual values: the same typed callable
/// local the following `CallValue(2)` uses, the original receiver carrier and
/// the actual typed userdata token. The body first reads a fixed snapshot and
/// the real `lua.userdata` payload, then hands those exact identities to the
/// heap's exclusive commit. That entry re-reads the live head, metatable,
/// action, issued receiver, closure `Arc`, and root plan in one borrow and
/// refuses before any write; the body never trusts the snapshot, a bare
/// handle, or the later `CallValue` check.
fn host_commit(vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
    let entry = frame_entry(vm, COMMIT_ENTRY_LABEL);
    with_run_scope(vm, |scope| scope.witness.entries.push(entry));

    let [callable, receiver, token] = args else {
        return Err(host_fault_error(
            "the typed commit body expects three operands",
        ));
    };
    let Value::Callable(callable_arc) = callable else {
        return Err(host_fault_error(
            "the typed commit body received a non-callable first operand",
        ));
    };
    let Value::Int(token_raw) = *token else {
        return Err(host_fault_error(
            "the typed commit body received a non-handle token operand",
        ));
    };

    let (snapshot, payload, receiver_class, weak_matches) = {
        let mut host = vm.host_context();
        let snapshot = snapshot_published_typed_table_call(&mut host).map_err(heap_fault)?;
        let receiver_class = heap::with_heap(&mut host, |heap| Ok(heap.guest_class(receiver)))
            .map_err(heap_fault)?;
        let payload = raw_handle(token_raw).ok().and_then(|handle| {
            host.borrow_resource_with_key::<LuaUserdataResource>(
                handle,
                &LuaUserdataResource::key(),
            )
            .ok()
            .map(|borrowed| borrowed.get().userdata())
        });
        let weak_matches = snapshot
            .callable
            .upgrade()
            .is_some_and(|stored| Arc::ptr_eq(&stored, callable_arc));
        (snapshot, payload, receiver_class, weak_matches)
    };
    let payload_is_head = payload == Some(snapshot.userdata);
    let head_parts = (
        snapshot.userdata.domain(),
        snapshot.userdata.slot(),
        snapshot.userdata.generation(),
    );
    let metatable_parts = (
        snapshot.metatable.domain(),
        snapshot.metatable.slot(),
        snapshot.metatable.generation(),
    );
    let action_receiver = (
        snapshot.receiver.domain(),
        snapshot.receiver.slot(),
        snapshot.receiver.generation(),
    );
    let action_closure = (
        snapshot.closure.domain(),
        snapshot.closure.slot(),
        snapshot.closure.generation(),
    );
    let published_before = snapshot.published_count;
    let queue_before = snapshot.queue_len;
    let token_u64 = u64::try_from(token_raw).unwrap_or(u64::MAX);

    let mut host = vm.host_context();
    let outcome = commit_published_typed_table_call(
        &mut host,
        callable_arc,
        payload,
        snapshot.metatable,
        receiver,
        token_u64,
    );
    let _ = host;
    let (recorded_outcome, taken_userdata) = match &outcome {
        Ok((frame, taken)) => (Ok((*frame, taken.roots_written)), Some(taken.userdata)),
        Err(error) => (Err(format!("{error:?}")), None),
    };
    let record = PublishedCommitRecord {
        callable: callable.clone(),
        receiver: receiver.clone(),
        receiver_class,
        token_raw,
        payload_parts: payload
            .map(|userdata| (userdata.domain(), userdata.slot(), userdata.generation())),
        payload_is_head,
        weak_matches_callable: weak_matches,
        head_parts,
        head_raw: i64::try_from(snapshot.raw_token).unwrap_or(i64::MAX),
        metatable_parts,
        action_receiver,
        action_closure,
        published_before,
        queue_before,
        outcome: recorded_outcome,
        taken_userdata,
    };
    with_run_scope(vm, |scope| scope.witness.commits.push(record));
    match outcome {
        Ok(_) => Ok(CallOutcome::Return(CallReturn::one(Value::Bool(true)))),
        Err(error) => Err(host_fault_error(format!(
            "{COMMIT_REFUSAL_PREFIX}{error:?}"
        ))),
    }
}

// ---- the private plugin: real lowering plus declared schemas --------------

#[derive(Clone, Debug)]
struct CallPatch {
    completion_slot: LocalSlot,
    written_callee: LocalSlot,
    typed_callee: LocalSlot,
    argc: u8,
    arg_slots: Vec<LocalSlot>,
}

/// The inserted pre-commit seam call: the host-import slot, the same typed
/// local the dynamic call uses, the fresh result local and the exact
/// resolution the real catalog returned.
#[derive(Clone, Debug)]
struct GatePatch {
    import_index: u16,
    typed_local: LocalSlot,
    result_slot: LocalSlot,
    resolution: ResolvedHostCall,
}

/// The actual operand choice the published-commit arm passes to the typed
/// commit body. `Argument` is the head's real token, `Wrong` is the queued
/// successor's real token, and `ForgedArgument` additionally builds an
/// equal-valued array from the issued receiver's two integers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommitArgument {
    Argument,
    Wrong,
    ForgedArgument,
}

/// The actual fixture callable the chunk-level `bind` installs into the
/// receiver's raw `__call`. The positive and post-read-replacement controls
/// install `Target`; the before-read replacement installs `Alternate`; the
/// wrong-signature control installs `Other`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BindChoice {
    Target,
    Alternate,
    Other,
}

/// The inserted exclusive commit call and its actual operand plan.
#[derive(Clone, Debug)]
struct CommitPatch {
    import_index: u16,
    typed_local: LocalSlot,
    result_slot: LocalSlot,
    resolution: ResolvedHostCall,
    argument: CommitArgument,
}

/// One resolved import the published-commit plugin adds to the IR directly.
#[derive(Clone)]
struct PublishedImport {
    import_index: u16,
    resolution: ResolvedHostCall,
}

/// The resolved exclusive commit import plus the operand choice for this arm.
#[derive(Clone)]
struct PublishedCommitSpec {
    import_index: u16,
    resolution: ResolvedHostCall,
    argument: CommitArgument,
    /// The raw-edge-change arm inserts the real heap `swap` between the seam
    /// and the commit.
    swap: Option<PublishedImport>,
}

#[derive(Clone, Debug, Default)]
struct PatchReport {
    typed_local: Option<LocalSlot>,
    declared_schema: Option<TypeSchema>,
    call: Option<CallPatch>,
    gate: Option<GatePatch>,
    commit: Option<CommitPatch>,
    sentinel_bind: Option<LocalSlot>,
    sentinel_wrong: Option<LocalSlot>,
    parent_parameters: Vec<LocalSlot>,
    target_declared: bool,
    alternate_declared: bool,
    other_declared: bool,
    continuation_rewritten: bool,
    producer: bool,
    producer_import: Option<u16>,
}

impl SourcePlugin for TransportPlugin {
    fn flavor(&self) -> SourceFlavor {
        SourceFlavor::Lua
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["lua"]
    }

    fn import_syntax(&self) -> FrontendImportSyntax {
        FrontendImportSyntax::Lua
    }

    fn parse_source(&self, source: &str) -> Result<FrontendIr, ParseError> {
        let resolver = HostCallResolver::new(transport_catalog());
        let mut ir = super::lower::lower_source_with(source, Some(&resolver))?;
        let report = patch_ir(&mut ir, &resolver, self.gate_mode)?;
        *self.patch.lock().expect("typed resource patch report") = Some(report);
        if self.pause_on_compile {
            compile_interleave_gate().park(InterleavePoint::Compile);
        }
        Ok(ir)
    }
}

fn patch_error(message: impl Into<String>) -> ParseError {
    ParseError::at_line(1, message)
}

/// Resolves and declares one plugin-added exact static import.
fn insert_published_import(
    ir: &mut FrontendIr,
    resolver: &HostCallResolver<'_>,
    name: &str,
    args: &[TypeSchema],
    value_type: vm::ValueType,
) -> Result<PublishedImport, ParseError> {
    let import_index = u16::try_from(ir.functions.len())
        .map_err(|_| patch_error("too many declared host functions"))?;
    let resolution = resolver
        .resolve(name, args)
        .map_err(|error| patch_error(format!("the {name} import did not resolve: {error}")))?;
    let arity = u8::try_from(resolution.params.len())
        .map_err(|_| patch_error("a plugin import arity overflows a byte"))?;
    ir.functions.push(FunctionDecl {
        name: name.to_string(),
        arity,
        index: import_index,
        args: resolution
            .params
            .iter()
            .map(|param| param.name.clone())
            .collect(),
        arg_schemas: resolution
            .params
            .iter()
            .map(|param| Some(param.schema.clone()))
            .collect(),
        return_schema: Some(resolution.return_type.clone()),
        type_params: Vec::new(),
        exported: false,
        return_type: value_type,
        symbol: None,
    });
    Ok(PublishedImport {
        import_index,
        resolution,
    })
}

/// Adds the chunk-level `bind(recv, target)` and `wrong_token()` calls the
/// published-commit fixture cannot name itself, and rewrites the parent call's
/// eighth operand to the successor-token local.
fn patch_published_chunk(
    stmts: &mut Vec<Stmt>,
    bind: &PublishedImport,
    wrong: &PublishedImport,
    bind_choice: BindChoice,
    next_local: &mut usize,
    report: &mut PatchReport,
) -> Result<bool, ParseError> {
    let mut fault_position = None;
    let mut receiver_slot = None;
    let mut target_slot = None;
    let mut alternate_slot = None;
    let mut other_slot = None;
    let mut parent_position = None;
    for (position, stmt) in stmts.iter_mut().enumerate() {
        match stmt {
            Stmt::Let {
                index: _,
                expr: Expr::Call(_, _, _, Some(resolution), _),
                ..
            } if resolution.name == "fault" => {
                fault_position = Some(position);
            }
            Stmt::Let {
                index,
                expr: Expr::Call(_, _, _, Some(resolution), _),
                ..
            } if resolution.name == "tick" => {
                receiver_slot = Some(*index);
            }
            Stmt::Let {
                index,
                expr: Expr::Closure(closure),
                ..
            } => match closure_first_mark_label(closure) {
                Some(TARGET_LABEL) => target_slot = Some(*index),
                Some(ALTERNATE_LABEL) => alternate_slot = Some(*index),
                Some(OTHER_LABEL) => other_slot = Some(*index),
                _ => {}
            },
            Stmt::Let {
                expr: Expr::LocalCall(_, _, args, _),
                ..
            } if args.len() == PUBLISHED_PARENT_PARAMS => {
                parent_position = Some(position);
            }
            _ => {}
        }
    }
    let bind_slot = match bind_choice {
        BindChoice::Target => target_slot,
        BindChoice::Alternate => alternate_slot,
        BindChoice::Other => other_slot,
    };
    if let (Some(fault_position), Some(receiver_slot), Some(bind_slot), Some(parent_position)) =
        (fault_position, receiver_slot, bind_slot, parent_position)
    {
        let bind_result = LocalSlot::try_from(*next_local)
            .map_err(|_| patch_error("the bind result local overflows a slot"))?;
        *next_local = next_local
            .checked_add(1)
            .ok_or_else(|| patch_error("too many locals for the bind result"))?;
        let wrong_slot = LocalSlot::try_from(*next_local)
            .map_err(|_| patch_error("the wrong-token local overflows a slot"))?;
        *next_local = next_local
            .checked_add(1)
            .ok_or_else(|| patch_error("too many locals for the wrong token"))?;
        let Stmt::Let {
            expr: Expr::LocalCall(_, _, args, _),
            ..
        } = &mut stmts[parent_position]
        else {
            return Err(patch_error("the published parent call changed shape"));
        };
        args[PUBLISHED_WRONG_SLOT] = Expr::Var(wrong_slot);
        let bind_statement = Stmt::Let {
            index: bind_result,
            declared_schema: None,
            expr: Expr::Call(
                bind.import_index,
                Vec::new(),
                vec![Expr::Var(receiver_slot), Expr::Var(bind_slot)],
                Some(Box::new(bind.resolution.clone())),
                None,
            ),
            line: 1,
        };
        let wrong_statement = Stmt::Let {
            index: wrong_slot,
            declared_schema: None,
            expr: Expr::Call(
                wrong.import_index,
                Vec::new(),
                Vec::new(),
                Some(Box::new(wrong.resolution.clone())),
                None,
            ),
            line: 1,
        };
        stmts.insert(fault_position + 1, wrong_statement);
        stmts.insert(fault_position + 1, bind_statement);
        report.sentinel_bind = Some(bind_result);
        report.sentinel_wrong = Some(wrong_slot);
        return Ok(true);
    }
    // The completion lowering wraps the chunk statements in one block
    // expression; descend until the level that owns the fault and parent call.
    for stmt in stmts.iter_mut() {
        let inner = match stmt {
            Stmt::Expr {
                expr: Expr::Block { stmts: inner, .. },
                ..
            }
            | Stmt::Let {
                expr: Expr::Block { stmts: inner, .. },
                ..
            } => inner,
            _ => continue,
        };
        if patch_published_chunk(inner, bind, wrong, bind_choice, next_local, report)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The resolved pre-commit seam the plugin installs, when the current arm asks
/// for it.
struct GateSpec {
    import_index: u16,
    resolution: ResolvedHostCall,
    forged_receiver: bool,
}

fn patch_ir(
    ir: &mut FrontendIr,
    resolver: &HostCallResolver<'_>,
    gate_mode: usize,
) -> Result<PatchReport, ParseError> {
    let mut report = PatchReport::default();
    let gate = if gate_mode == 0 {
        None
    } else {
        let import_index = u16::try_from(ir.functions.len())
            .map_err(|_| patch_error("too many declared host functions"))?;
        // Resolve against the real catalog with the concrete operand schemas:
        // no `Unknown`, no downgraded key, no fabricated token.
        let resolution = resolver
            .resolve(
                PRECOMMIT_IMPORT,
                &[
                    userdata_callable_schema(),
                    TypeSchema::Array(Box::new(TypeSchema::Int)),
                    TypeSchema::Resource(LuaUserdataResource::key()),
                ],
            )
            .map_err(|error| {
                patch_error(format!("the precommit import did not resolve: {error}"))
            })?;
        let arity = u8::try_from(resolution.params.len())
            .map_err(|_| patch_error("the precommit import arity overflows a byte"))?;
        // The resolver maps the catalog return to `Bool`; the compiled import
        // is asserted against that exact return below.
        let return_type = vm::ValueType::Bool;
        // The declaration is built from the resolver output itself: names,
        // concrete parameter schemas and the return stay the exact selected
        // catalog identity.
        ir.functions.push(FunctionDecl {
            name: PRECOMMIT_IMPORT.to_string(),
            arity,
            index: import_index,
            args: resolution
                .params
                .iter()
                .map(|param| param.name.clone())
                .collect(),
            arg_schemas: resolution
                .params
                .iter()
                .map(|param| Some(param.schema.clone()))
                .collect(),
            return_schema: Some(resolution.return_type.clone()),
            type_params: Vec::new(),
            exported: false,
            return_type,
            symbol: None,
        });
        Some(GateSpec {
            import_index,
            resolution,
            forged_receiver: gate_mode == 2,
        })
    };
    let producer = gate_mode >= 7;
    let bind_choice = match gate_mode {
        8 => BindChoice::Alternate,
        10 => BindChoice::Other,
        _ => BindChoice::Target,
    };
    // A published-commit arm gets a second exact static import resolved with
    // the same concrete operands. The commit body runs after the read-only
    // seam and before the parent's own `CallValue(2)`.
    let mut commit = if gate_mode >= 3 {
        let import_index = u16::try_from(ir.functions.len())
            .map_err(|_| patch_error("too many declared host functions"))?;
        let resolution = resolver
            .resolve(
                COMMIT_IMPORT,
                &[
                    userdata_callable_schema(),
                    TypeSchema::Array(Box::new(TypeSchema::Int)),
                    TypeSchema::Resource(LuaUserdataResource::key()),
                ],
            )
            .map_err(|error| patch_error(format!("the commit import did not resolve: {error}")))?;
        let arity = u8::try_from(resolution.params.len())
            .map_err(|_| patch_error("the commit import arity overflows a byte"))?;
        ir.functions.push(FunctionDecl {
            name: COMMIT_IMPORT.to_string(),
            arity,
            index: import_index,
            args: resolution
                .params
                .iter()
                .map(|param| param.name.clone())
                .collect(),
            arg_schemas: resolution
                .params
                .iter()
                .map(|param| Some(param.schema.clone()))
                .collect(),
            return_schema: Some(resolution.return_type.clone()),
            type_params: Vec::new(),
            exported: false,
            return_type: vm::ValueType::Bool,
            symbol: None,
        });
        let argument = match gate_mode {
            4 => CommitArgument::Wrong,
            5 => CommitArgument::ForgedArgument,
            _ => CommitArgument::Argument,
        };
        Some(PublishedCommitSpec {
            import_index,
            resolution,
            argument,
            swap: None,
        })
    } else {
        None
    };
    // The published-commit fixture's chunk gets two more exact static calls
    // that are not guest-lowered names: `bind(recv, target)` installs the raw
    // `__call` and `wrong_token()` returns the queued successor's real token.
    // The parent call's eighth operand is rewritten to that token slot.
    let published_chunk = if gate_mode >= 3 {
        let bind = insert_published_import(
            ir,
            resolver,
            BIND_IMPORT,
            &[TypeSchema::Unknown, TypeSchema::Unknown],
            vm::ValueType::Bool,
        )?;
        let wrong =
            insert_published_import(ir, resolver, WRONG_TOKEN_IMPORT, &[], vm::ValueType::Int)?;
        if (gate_mode == 6 || gate_mode == 9)
            && let Some(spec) = commit.as_mut()
        {
            spec.swap = Some(insert_published_import(
                ir,
                resolver,
                SWAP_IMPORT,
                &[TypeSchema::Unknown, TypeSchema::Unknown],
                vm::ValueType::Bool,
            )?);
        }
        Some((bind, wrong))
    } else {
        None
    };
    // The producer fixture cannot name a private host call in source. The
    // plugin inserts the exact static `produce(recv, arg2)`: the callable
    // operand is absent, and only the actual issued receiver carrier and the
    // actual typed userdata token cross the boundary. Core's exact return
    // schema is the same callable schema the typed local declares.
    let produce = if producer {
        Some(insert_published_import(
            ir,
            resolver,
            PRODUCER_IMPORT,
            &[
                TypeSchema::Array(Box::new(TypeSchema::Int)),
                TypeSchema::Resource(LuaUserdataResource::key()),
            ],
            vm::ValueType::Callable,
        )?)
    } else {
        None
    };
    // Only the arms that must retire get this import. The omission arm uses
    // the same fixture and the same producer rewrite, but the plugin does not
    // insert the call, so the parent continuation never names it.
    let retire = if gate_mode == 12 || gate_mode == FALSE_RETURN_GATE {
        Some(insert_published_import(
            ir,
            resolver,
            RETIRE_IMPORT,
            &[],
            vm::ValueType::Bool,
        )?)
    } else {
        None
    };
    // The guest collection import is also unnamed in source. Every retirement
    // arm, including the omission arm, gets it before `done`. It does not
    // leave roots.
    let sweep = if matches!(gate_mode, FALSE_RETURN_GATE | 12 | 13) {
        Some(insert_published_import(
            ir,
            resolver,
            SWEEP_IMPORT,
            &[],
            vm::ValueType::Bool,
        )?)
    } else {
        None
    };
    let mut next_local = ir.locals;
    let rewrite = ProducerRewrite {
        enabled: producer,
        produce: produce.as_ref(),
        retire: retire.as_ref(),
        observe_call: matches!(gate_mode, FALSE_RETURN_GATE | 12 | 13),
        callback_false: gate_mode == FALSE_RETURN_GATE,
    };
    patch_stmts(
        &mut ir.stmts,
        gate.as_ref(),
        commit.as_ref(),
        &rewrite,
        &mut next_local,
        &mut report,
    )?;
    if let Some((bind, wrong)) = &published_chunk
        && !patch_published_chunk(
            &mut ir.stmts,
            bind,
            wrong,
            bind_choice,
            &mut next_local,
            &mut report,
        )?
    {
        return Err(patch_error(
            "the published-commit chunk is missing its fault, tick, target or parent call",
        ));
    }
    if let Some(sweep) = sweep.as_ref()
        && !insert_sweep_before_done(&mut ir.stmts, sweep, &mut next_local)?
    {
        return Err(patch_error(
            "the retirement chunk has no done mark for the guest collection import",
        ));
    }
    ir.locals = next_local;
    report.producer = producer;
    report.producer_import = produce.as_ref().map(|produce| produce.import_index);
    // The private lowerer reads every admitted Lua callable through an
    // explicit `ToOwned` copy. This pin classifies a callable whose schema
    // contains a resource as an owned local and then refuses `.copy()` on it.
    // None of the callables in this fixture is captured by a nested closure,
    // so they are plain shared reads; use the same plain local read codegen
    // produces for `ToOwned` and keep the exact declared schema intact.
    strip_callable_copies(&mut ir.stmts);
    Ok(report)
}

fn strip_callable_copies(stmts: &mut [Stmt]) {
    for stmt in stmts.iter_mut() {
        match stmt {
            Stmt::Let { expr, .. } | Stmt::Expr { expr, .. } => strip_to_owned(expr),
            _ => {}
        }
    }
}

fn strip_to_owned(expr: &mut Expr) {
    if let Expr::ToOwned(inner) = expr {
        strip_to_owned(inner);
        if let Expr::Var(slot) = inner.as_ref().clone() {
            *expr = Expr::Var(slot);
            return;
        }
    }
    match expr {
        Expr::Call(_, _, args, _, _) | Expr::LocalCall(_, _, args, _) => {
            for arg in args {
                strip_to_owned(arg);
            }
        }
        Expr::Closure(closure) => strip_to_owned(&mut closure.body),
        Expr::ClosureCall(closure, args) => {
            strip_to_owned(&mut closure.body);
            for arg in args {
                strip_to_owned(arg);
            }
        }
        Expr::Block { stmts, expr } => {
            strip_callable_copies(stmts);
            strip_to_owned(expr);
        }
        Expr::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            strip_to_owned(condition);
            strip_to_owned(then_expr);
            strip_to_owned(else_expr);
        }
        _ => {}
    }
}

struct ProducerRewrite<'a> {
    enabled: bool,
    produce: Option<&'a PublishedImport>,
    retire: Option<&'a PublishedImport>,
    observe_call: bool,
    callback_false: bool,
}

fn patch_stmts(
    stmts: &mut [Stmt],
    gate: Option<&GateSpec>,
    commit: Option<&PublishedCommitSpec>,
    rewrite: &ProducerRewrite<'_>,
    next_local: &mut usize,
    report: &mut PatchReport,
) -> Result<(), ParseError> {
    for stmt in stmts.iter_mut() {
        match stmt {
            Stmt::Let {
                declared_schema,
                expr: Expr::Closure(closure),
                ..
            } => {
                match closure_first_mark_label(closure) {
                    Some(TARGET_LABEL) => {
                        *declared_schema = Some(userdata_callable_schema());
                        report.target_declared = true;
                        // Core validates a script call's real return against
                        // the prototype's declared result. The private
                        // completion lowering wraps every source return into
                        // an array; the declared typed target returns the
                        // `Bool` this signature names. Only the typed target
                        // is rewritten: its call site is the sequential
                        // continuation below, and no completion consumer
                        // observes it.
                        if let Expr::Block { expr, .. } = closure.body.as_mut() {
                            // The false-return arm keeps the source `return true`.
                            // Only this compiled target result becomes `Bool(false)`,
                            // so the parent can tell that slot from a host `true`.
                            **expr = Expr::Bool(!rewrite.callback_false);
                        }
                    }
                    Some(ALTERNATE_LABEL) if rewrite.enabled => {
                        // The before-read replacement is a second real
                        // zero-capture closure with the same exact schema and
                        // its own execution marker. It is only declared on
                        // the producer fixture, so the original published
                        // arms keep their compiled shape.
                        *declared_schema = Some(userdata_callable_schema());
                        if let Expr::Block { expr, .. } = closure.body.as_mut() {
                            **expr = Expr::Bool(true);
                        }
                        report.alternate_declared = true;
                    }
                    Some(OTHER_LABEL) => {
                        *declared_schema = Some(table_callable_schema());
                        report.other_declared = true;
                    }
                    _ => {}
                }
                if closure.param_slots.len() == PARENT_PARAMS
                    || closure.param_slots.len() == PUBLISHED_PARENT_PARAMS
                {
                    patch_parent(closure, gate, commit, rewrite, next_local, report)?;
                }
            }
            Stmt::Expr {
                expr: Expr::Block { stmts, .. },
                ..
            } => patch_stmts(stmts, gate, commit, rewrite, next_local, report)?,
            _ => {}
        }
    }
    Ok(())
}

/// The first statement of the closure body is `mark("<label>")`, a real host
/// call node from the lowering; the label alone identifies the declaration.
fn closure_first_mark_label(closure: &ClosureExpr) -> Option<&str> {
    let Expr::Block { stmts, .. } = closure.body.as_ref() else {
        return None;
    };
    let Stmt::Let {
        expr: Expr::Call(_, _, args, Some(resolution), _),
        ..
    } = stmts.first()?
    else {
        return None;
    };
    if resolution.name != "mark" {
        return None;
    }
    match args.as_slice() {
        [Expr::String(label)] => Some(label.as_str()),
        _ => None,
    }
}

fn is_slot_read(expr: &Expr, slot: LocalSlot) -> bool {
    matches!(expr, Expr::Var(read) if *read == slot)
}

/// The private completion lowering's status read: `get(completion, 0)`.
fn reads_completion(expr: &Expr, completion: LocalSlot) -> bool {
    let Expr::Call(_, _, args, _, _) = expr else {
        return false;
    };
    matches!(args.as_slice(), [first, second] if is_slot_read(first, completion)
        && matches!(second, Expr::Int(0)))
}

/// Inserts the exact zero-argument exit import after `parent-after` and
/// before the parent return. The call runs in the same parent frame that
/// borrowed A. The omission arm never receives this import.
fn insert_retire_after_parent_mark(
    tail: &mut Expr,
    retire: &PublishedImport,
    next_local: &mut usize,
) -> Result<(), ParseError> {
    let Expr::Block { stmts, .. } = tail else {
        return Err(patch_error("the call continuation is not a block"));
    };
    let mark_at = stmts.iter().position(is_parent_after_mark).ok_or_else(|| {
        patch_error("the continuation has no parent-after mark before the exit import")
    })?;
    let slot = LocalSlot::try_from(*next_local)
        .map_err(|_| patch_error("the exit-import result local overflows a slot"))?;
    *next_local = next_local
        .checked_add(1)
        .ok_or_else(|| patch_error("the exit-import result local overflows"))?;
    stmts.insert(
        mark_at + 1,
        Stmt::Let {
            index: slot,
            declared_schema: None,
            expr: Expr::Call(
                retire.import_index,
                Vec::new(),
                Vec::new(),
                Some(Box::new(retire.resolution.clone())),
                None,
            ),
            line: 0,
        },
    );
    Ok(())
}

fn is_done_mark(stmt: &Stmt) -> bool {
    let Stmt::Let {
        expr: Expr::Call(_, _, args, Some(resolution), _),
        ..
    } = stmt
    else {
        return false;
    };
    resolution.name == "mark" && matches!(args.as_slice(), [Expr::String(label)] if label == "done")
}

fn insert_sweep_before_done(
    stmts: &mut [Stmt],
    sweep: &PublishedImport,
    next_local: &mut usize,
) -> Result<bool, ParseError> {
    for stmt in stmts {
        if insert_sweep_in_stmt(stmt, sweep, next_local)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn insert_sweep_in_stmt(
    stmt: &mut Stmt,
    sweep: &PublishedImport,
    next_local: &mut usize,
) -> Result<bool, ParseError> {
    match stmt {
        Stmt::Expr { expr, .. } | Stmt::Let { expr, .. } => {
            insert_sweep_in_expr(expr, sweep, next_local)
        }
        _ => Ok(false),
    }
}

fn insert_sweep_in_expr(
    expr: &mut Expr,
    sweep: &PublishedImport,
    next_local: &mut usize,
) -> Result<bool, ParseError> {
    match expr {
        Expr::Block { stmts, expr } => {
            if let Some(index) = stmts.iter().position(is_done_mark) {
                let slot = LocalSlot::try_from(*next_local).map_err(|_| {
                    patch_error("the guest-collection result local overflows a slot")
                })?;
                *next_local = next_local
                    .checked_add(1)
                    .ok_or_else(|| patch_error("the guest-collection result local overflows"))?;
                stmts.insert(
                    index,
                    Stmt::Let {
                        index: slot,
                        declared_schema: None,
                        expr: Expr::Call(
                            sweep.import_index,
                            Vec::new(),
                            Vec::new(),
                            Some(Box::new(sweep.resolution.clone())),
                            None,
                        ),
                        line: 0,
                    },
                );
                return Ok(true);
            }
            for stmt in stmts.iter_mut() {
                if insert_sweep_in_stmt(stmt, sweep, next_local)? {
                    return Ok(true);
                }
            }
            insert_sweep_in_expr(expr, sweep, next_local)
        }
        Expr::IfElse {
            then_expr,
            else_expr,
            ..
        } => {
            if insert_sweep_in_expr(then_expr, sweep, next_local)? {
                return Ok(true);
            }
            insert_sweep_in_expr(else_expr, sweep, next_local)
        }
        Expr::Closure(closure) => insert_sweep_in_expr(&mut closure.body, sweep, next_local),
        _ => Ok(false),
    }
}

fn consume_call_result(tail: &mut Expr, slot: LocalSlot) -> bool {
    let expr = match tail {
        Expr::Block { expr, .. } => expr.as_mut(),
        other => other,
    };
    replace_returned_value(expr, slot)
}

fn replace_returned_value(expr: &mut Expr, slot: LocalSlot) -> bool {
    let Expr::Call(_, _, args, _, _) = expr else {
        return false;
    };
    let Some(last) = args.last_mut() else {
        return false;
    };
    if matches!(last, Expr::Bool(_)) {
        *last = Expr::Var(slot);
        return true;
    }
    replace_returned_value(last, slot)
}

fn is_parent_after_mark(stmt: &Stmt) -> bool {
    let Stmt::Let {
        expr: Expr::Call(_, _, args, Some(resolution), _),
        ..
    } = stmt
    else {
        return false;
    };
    resolution.name == "mark"
        && matches!(args.as_slice(), [Expr::String(label)] if label == "parent-after")
}

fn patch_parent(
    closure: &mut ClosureExpr,
    gate: Option<&GateSpec>,
    commit: Option<&PublishedCommitSpec>,
    rewrite: &ProducerRewrite<'_>,
    next_local: &mut usize,
    report: &mut PatchReport,
) -> Result<(), ParseError> {
    let Expr::Block { stmts, expr: tail } = closure.body.as_mut() else {
        return Err(patch_error("the parent body is not a block"));
    };
    let parameters = closure.param_slots.clone();
    if parameters.len() != PARENT_PARAMS && parameters.len() != PUBLISHED_PARENT_PARAMS {
        return Err(patch_error("the parent parameter list changed"));
    }
    if commit.is_some() != (parameters.len() == PUBLISHED_PARENT_PARAMS) {
        return Err(patch_error(
            "only the published-commit parent carries the commit import",
        ));
    }
    report.parent_parameters = parameters.clone();

    // The old fixture selection is `note`. The producer arm keeps it in the
    // frame for the in-run RED comparison, but the typed local that feeds the
    // exclusive commit and the following `CallValue(2)` is created by the
    // plugin's own exact static `produce(recv, arg2)` call.
    let mut note_position = None;
    let mut note_local = None;
    for (position, stmt) in stmts.iter_mut().enumerate() {
        let Stmt::Let {
            index,
            declared_schema,
            expr: Expr::Call(_, _, _, Some(resolution), _),
            ..
        } = stmt
        else {
            continue;
        };
        if resolution.name != "note" {
            continue;
        }
        if note_position.is_some() {
            return Err(patch_error(
                "the parent declares two typed candidate locals",
            ));
        }
        if !rewrite.enabled {
            *declared_schema = Some(userdata_callable_schema());
        }
        note_local = Some(*index);
        note_position = Some(position);
    }
    let Some(note_position) = note_position else {
        return Err(patch_error("the parent has no typed candidate local"));
    };
    let typed_local = if let Some(produce) = rewrite.produce {
        let produced = LocalSlot::try_from(*next_local)
            .map_err(|_| patch_error("the producer result local overflows a slot"))?;
        *next_local = next_local
            .checked_add(1)
            .ok_or_else(|| patch_error("too many locals for the producer result"))?;
        let expression = Expr::Call(
            produce.import_index,
            Vec::new(),
            vec![
                Expr::Var(parameters[PARENT_RECEIVER_SLOT]),
                Expr::Var(parameters[PARENT_ARGUMENT_SLOT]),
            ],
            Some(Box::new(produce.resolution.clone())),
            None,
        );
        stmts.insert(
            note_position + 1,
            Stmt::Let {
                index: produced,
                declared_schema: Some(userdata_callable_schema()),
                expr: expression,
                line: 1,
            },
        );
        produced
    } else {
        note_local.ok_or_else(|| patch_error("the parent has no typed candidate local"))?
    };
    report.typed_local = Some(typed_local);
    report.declared_schema = Some(userdata_callable_schema());

    let mut call_position = None;
    for (position, stmt) in stmts.iter_mut().enumerate() {
        let Stmt::Let {
            index: completion,
            expr: Expr::LocalCall(callee, _, args, _),
            ..
        } = stmt
        else {
            continue;
        };
        if *callee != parameters[PARENT_CALLEE_SLOT] {
            continue;
        }
        if args.len() != usize::from(CALL_ARGC) {
            return Err(patch_error("the admitted dynamic call changed its arity"));
        }
        if !is_slot_read(&args[0], parameters[PARENT_RECEIVER_SLOT])
            || !is_slot_read(&args[1], parameters[PARENT_ARGUMENT_SLOT])
        {
            return Err(patch_error(
                "the admitted dynamic call does not pass the receiver and argument slots",
            ));
        }
        if call_position.is_some() {
            return Err(patch_error("the parent has two admitted dynamic calls"));
        }
        *callee = typed_local;
        report.call = Some(CallPatch {
            completion_slot: *completion,
            written_callee: parameters[PARENT_CALLEE_SLOT],
            typed_callee: typed_local,
            argc: CALL_ARGC,
            arg_slots: vec![
                parameters[PARENT_RECEIVER_SLOT],
                parameters[PARENT_ARGUMENT_SLOT],
            ],
        });
        call_position = Some(position);
    }

    let Some(completion) = report.call.as_ref().map(|call| call.completion_slot) else {
        return Err(patch_error("the parent has no admitted dynamic call"));
    };

    // This pin types the completion status read as `get(bool, int)` once the
    // callable declares the required concrete `Bool` result. The real call
    // node and its continuation are kept; only the status read is replaced by
    // the sequential continuation, and the child's returned completion array
    // is observed from the parent frame instead.
    let replaced = std::mem::replace(tail, Box::new(Expr::Null));
    let Expr::IfElse {
        condition,
        then_expr,
        ..
    } = *replaced
    else {
        return Err(patch_error(
            "the admitted dynamic call has no completion branch",
        ));
    };
    if !reads_completion(&condition, completion) {
        return Err(patch_error(
            "the completion branch does not read the call's own slot",
        ));
    }
    *tail = then_expr;
    if let Some(retire) = rewrite.retire {
        insert_retire_after_parent_mark(tail, retire, next_local)?;
    }
    if rewrite.observe_call {
        let slot = report
            .call
            .as_ref()
            .map(|call| call.completion_slot)
            .ok_or_else(|| patch_error("the observed call has no completion slot"))?;
        if !consume_call_result(tail, slot) {
            return Err(patch_error(
                "the parent return does not consume the call result",
            ));
        }
    }
    report.continuation_rewritten = true;

    if let Some(gate) = gate {
        let Some(position) = call_position else {
            return Err(patch_error("the parent has no admitted dynamic call"));
        };
        let admit_position = if rewrite.produce.is_some() {
            note_position + 2
        } else if commit.is_some() {
            note_position + 1
        } else {
            position
        };
        let result_slot = LocalSlot::try_from(*next_local)
            .map_err(|_| patch_error("the precommit result local overflows a slot"))?;
        *next_local = next_local
            .checked_add(1)
            .ok_or_else(|| patch_error("too many locals for the precommit result"))?;
        let receiver = if gate.forged_receiver {
            forged_receiver_expr(parameters[PARENT_RECEIVER_SLOT])
        } else {
            Expr::Var(parameters[PARENT_RECEIVER_SLOT])
        };
        // The exact operands the following dynamic call will use: the same
        // typed local, the receiver parameter and the argument parameter.
        let expr = Expr::Call(
            gate.import_index,
            Vec::new(),
            vec![
                Expr::Var(typed_local),
                receiver,
                Expr::Var(parameters[PARENT_ARGUMENT_SLOT]),
            ],
            Some(Box::new(gate.resolution.clone())),
            None,
        );
        if let Some(commit) = commit {
            let commit_result = LocalSlot::try_from(*next_local)
                .map_err(|_| patch_error("the commit result local overflows a slot"))?;
            *next_local = next_local
                .checked_add(1)
                .ok_or_else(|| patch_error("too many locals for the commit result"))?;
            let commit_receiver = if commit.argument == CommitArgument::ForgedArgument {
                forged_receiver_expr(parameters[PARENT_RECEIVER_SLOT])
            } else {
                Expr::Var(parameters[PARENT_RECEIVER_SLOT])
            };
            let argument_slot = match commit.argument {
                CommitArgument::Wrong => parameters[PUBLISHED_WRONG_SLOT],
                CommitArgument::Argument | CommitArgument::ForgedArgument => {
                    parameters[PARENT_ARGUMENT_SLOT]
                }
            };
            let commit_expr = Expr::Call(
                commit.import_index,
                Vec::new(),
                vec![
                    Expr::Var(typed_local),
                    commit_receiver,
                    Expr::Var(argument_slot),
                ],
                Some(Box::new(commit.resolution.clone())),
                None,
            );
            stmts.insert(
                position,
                Stmt::Let {
                    index: commit_result,
                    declared_schema: None,
                    expr: commit_expr,
                    line: 1,
                },
            );
            report.commit = Some(CommitPatch {
                import_index: commit.import_index,
                typed_local,
                result_slot: commit_result,
                resolution: commit.resolution.clone(),
                argument: commit.argument,
            });
        }
        // The raw-edge-change arm runs the real heap `swap(recv, alternate)`
        // after the read-only seam and before the exclusive commit. `second`
        // carries the exact `alternate` Arc the fixture passed as candidate.
        if let Some(commit) = commit
            && let Some(swap) = commit.swap.as_ref()
        {
            let swap_result = LocalSlot::try_from(*next_local)
                .map_err(|_| patch_error("the swap result local overflows a slot"))?;
            *next_local = next_local
                .checked_add(1)
                .ok_or_else(|| patch_error("too many locals for the swap result"))?;
            let swap_expr = Expr::Call(
                swap.import_index,
                Vec::new(),
                vec![
                    Expr::Var(parameters[PARENT_RECEIVER_SLOT]),
                    Expr::Var(parameters[PUBLISHED_ALTERNATE_SLOT]),
                ],
                Some(Box::new(swap.resolution.clone())),
                None,
            );
            stmts.insert(
                position,
                Stmt::Let {
                    index: swap_result,
                    declared_schema: None,
                    expr: swap_expr,
                    line: 1,
                },
            );
        }
        stmts.insert(
            admit_position,
            Stmt::Let {
                index: result_slot,
                declared_schema: None,
                expr,
                line: 1,
            },
        );
        report.gate = Some(GatePatch {
            import_index: gate.import_index,
            typed_local,
            result_slot,
            resolution: gate.resolution.clone(),
        });
    }
    Ok(())
}

/// An array with the same two integers as the issued receiver carrier but a
/// fresh VM array, never issued by the heap. The reads run against the real
/// receiver local, so the values are equal at run time; only the Arc identity
/// differs.
fn forged_receiver_expr(receiver: LocalSlot) -> Expr {
    let element = |index: i64| {
        Expr::Call(
            BuiltinFunction::Get.call_index(),
            Vec::new(),
            vec![Expr::Var(receiver), Expr::Int(index)],
            None,
            None,
        )
    };
    let mut array = Expr::Call(
        BuiltinFunction::ArrayNew.call_index(),
        Vec::new(),
        Vec::new(),
        None,
        None,
    );
    for index in 0..2 {
        array = Expr::Call(
            BuiltinFunction::ArrayPush.call_index(),
            Vec::new(),
            vec![array, element(index)],
            None,
            None,
        );
    }
    array
}

// ---- one arm --------------------------------------------------------------

struct ArmRun {
    arm: Arm,
    status: Result<VmStatus, VmError>,
    stack: Vec<Value>,
    witness: Witness,
    patch: PatchReport,
    call_sites: Vec<super::DecodedCallSite>,
    host_call_sites: Vec<(usize, u8, Option<u32>)>,
    commit_call_sites: Vec<(usize, u8, Option<u32>)>,
    producer_call_sites: Vec<(usize, u8, Option<u32>)>,
    prototypes: Vec<vm::CallablePrototype>,
    host_imports: Vec<HostImport>,
    host_schemas: Vec<Option<HostImportSchema>>,
    local_schemas: Vec<Option<TypeSchema>>,
    fuel_consumed: u64,
    bound_functions: usize,
    vm: Vm,
}

/// Every compiled static host `Call` site for `import`: `(ip, argc, owner)`.
/// The shared helper decodes only `CallValue`; this seam also needs the static
/// host-call site to prove where it sits relative to the dynamic call.
fn decoded_host_calls(program: &Program, import: u16) -> Vec<(usize, u8, Option<u32>)> {
    let code = &program.code;
    let mut ip = 0usize;
    let mut sites = Vec::new();
    while ip < code.len() {
        let op = OpCode::try_from(code[ip]).expect("invalid opcode");
        let next = ip + 1 + op.operand_len();
        assert!(next <= code.len(), "truncated instruction at {ip}");
        if op == OpCode::Call && u16::from_le_bytes([code[ip + 1], code[ip + 2]]) == import {
            let owners: Vec<_> = program
                .function_regions
                .iter()
                .filter(|region| region.start_ip as usize <= ip && next <= region.end_ip as usize)
                .collect();
            let [owner] = owners.as_slice() else {
                panic!("expected a unique caller region for Call at {ip}");
            };
            sites.push((ip, code[ip + 3], owner.prototype_id));
        }
        ip = next;
    }
    sites
}

fn fixture_path(fixture: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("luajit")
        .join("finalizer")
        .join(fixture)
}

/// One real compile of one arm. The plugin instance owns both the arm's
/// compile input and the structural report it produced.
struct CompiledArm {
    arm: Arm,
    program: Program,
    patch: PatchReport,
    call_sites: Vec<super::DecodedCallSite>,
    host_call_sites: Vec<(usize, u8, Option<u32>)>,
    commit_call_sites: Vec<(usize, u8, Option<u32>)>,
    producer_call_sites: Vec<(usize, u8, Option<u32>)>,
    prototypes: Vec<vm::CallablePrototype>,
    host_imports: Vec<HostImport>,
    host_schemas: Vec<Option<HostImportSchema>>,
    local_schemas: Vec<Option<TypeSchema>>,
}

fn compile_arm(arm: Arm) -> CompiledArm {
    compile_arm_with(arm, TransportPlugin::new(arm))
}

fn compile_arm_paused(arm: Arm) -> CompiledArm {
    compile_arm_with(arm, TransportPlugin::with_compile_pause(arm))
}

fn compile_arm_with(arm: Arm, plugin: TransportPlugin) -> CompiledArm {
    let plugin: &'static TransportPlugin = Box::leak(Box::new(plugin));
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(plugin)
        .with_host_api_catalog(Arc::clone(transport_catalog()));
    let path = fixture_path(arm.fixture());
    let compiled = vm::compile_source_file_with_options(&path, options)
        .unwrap_or_else(|error| panic!("{}: compile failed: {error:?}", arm.label()));
    let patch = plugin.take_patch(arm);
    let call_sites = decoded_call_sites(&compiled.program);
    let host_call_sites = patch
        .gate
        .as_ref()
        .map(|gate| decoded_host_calls(&compiled.program, gate.import_index))
        .unwrap_or_default();
    let commit_call_sites = patch
        .commit
        .as_ref()
        .map(|commit| decoded_host_calls(&compiled.program, commit.import_index))
        .unwrap_or_default();
    let producer_call_sites = patch
        .producer_import
        .map(|import| decoded_host_calls(&compiled.program, import))
        .unwrap_or_default();
    let prototypes = compiled.program.callable_prototypes.clone();
    let host_imports = compiled.program.imports.clone();
    let host_schemas = compiled.program.host_import_schemas().to_vec();
    let local_schemas = compiled
        .program
        .type_map
        .as_ref()
        .map(|map| map.local_schemas.clone())
        .unwrap_or_default();
    CompiledArm {
        arm,
        program: compiled.program,
        patch,
        call_sites,
        host_call_sites,
        commit_call_sites,
        producer_call_sites,
        prototypes,
        host_imports,
        host_schemas,
        local_schemas,
    }
}

fn run_arm(arm: Arm) -> ArmRun {
    run_compiled(compile_arm(arm))
}

fn run_compiled(compiled: CompiledArm) -> ArmRun {
    let arm = compiled.arm;
    let call_result_slot = if arm.observes_call_result() {
        compiled_call_result_slot(&compiled.program)
    } else {
        None
    };
    let mut runtime = Vm::new_with_jit_config(
        compiled.program,
        JitConfig {
            enabled: false,
            hot_loop_threshold: 8,
            max_trace_len: 256,
        },
    );
    bind_hosts_for(&mut runtime, arm.observes_call_result())
        .unwrap_or_else(|error| panic!("{}: bind failed: {error}", arm.label()));
    {
        let mut host = runtime.host_context();
        host.scope_state_or_insert_with(|| RunScope {
            select: arm.select(),
            witness: Witness::default(),
            call_result_slot,
        })
        .unwrap_or_else(|error| panic!("{}: run scope: {error:?}", arm.label()));
    }
    runtime
        .set_fuel_check_interval(FUEL_CHECK_INTERVAL)
        .unwrap_or_else(|error| panic!("{}: fuel interval: {error}", arm.label()));
    runtime.set_fuel(FUEL_BUDGET);
    let status = runtime.run();
    let remaining = runtime
        .get_fuel()
        .unwrap_or_else(|| panic!("{}: fuel metering missing", arm.label()));
    let fuel_consumed = FUEL_BUDGET
        .checked_sub(remaining)
        .unwrap_or_else(|| panic!("{}: fuel increased", arm.label()));
    let stack = runtime.stack().to_vec();
    let bound_functions = runtime.bound_function_count();
    interleave_pause(InterleavePoint::Take);
    let scope = {
        let mut host = runtime.host_context();
        host.take_scope_state::<RunScope>()
            .unwrap_or_else(|| panic!("{}: the run scope was not installed", arm.label()))
    };

    let run = ArmRun {
        arm,
        status,
        stack,
        witness: scope.witness,
        patch: compiled.patch,
        call_sites: compiled.call_sites,
        host_call_sites: compiled.host_call_sites,
        commit_call_sites: compiled.commit_call_sites,
        producer_call_sites: compiled.producer_call_sites,
        prototypes: compiled.prototypes,
        host_imports: compiled.host_imports,
        host_schemas: compiled.host_schemas,
        local_schemas: compiled.local_schemas,
        fuel_consumed,
        bound_functions,
        vm: runtime,
    };
    run.report();
    run
}

impl ArmRun {
    fn report(&self) {
        eprintln!(
            "LUA_TYPED_RESOURCE arm={} fixture={} fuel={} status={:?} patch={:?}",
            self.arm.label(),
            self.arm.fixture(),
            self.fuel_consumed,
            self.status,
            self.patch
        );
        eprintln!(
            "LUA_TYPED_RESOURCE arm={} entries={:?}",
            self.arm.label(),
            self.witness.entries
        );
        eprintln!(
            "LUA_TYPED_RESOURCE arm={} marks={:?}",
            self.arm.label(),
            self.witness
                .marks
                .iter()
                .map(|mark| (mark.label.as_str(), mark.frame_len, mark.last_prototype))
                .collect::<Vec<_>>()
        );
        for mark in &self.witness.marks {
            if let Some(entry) = &mark.parent_entry {
                eprintln!(
                    "LUA_TYPED_RESOURCE arm={} parent-entry label={} argument_raw={}",
                    self.arm.label(),
                    mark.label,
                    entry.argument_raw
                );
            }
            if let Some(continuation) = &mark.continuation {
                eprintln!(
                    "LUA_TYPED_RESOURCE arm={} continuation label={} raw={} parts={:?} rebound={:?}",
                    self.arm.label(),
                    mark.label,
                    continuation.raw,
                    continuation.parts,
                    continuation.rebound_parts
                );
            }
            if let Some(child_return) = &mark.child_return {
                eprintln!(
                    "LUA_TYPED_RESOURCE arm={} child-return label={} value={:?}",
                    self.arm.label(),
                    mark.label,
                    child_return
                );
            }
            let Some(callee) = &mark.callee else {
                continue;
            };
            eprintln!(
                "LUA_TYPED_RESOURCE arm={} callee label={} prototype={} slots={:?} values={:?} receiver={} ordinary={} token={:?} rebound={:?} table_refused={} caller_slot={:?} same_arc_slots={:?}",
                self.arm.label(),
                mark.label,
                callee.prototype_id,
                callee.parameter_slots,
                callee.parameter_values,
                callee.receiver_class,
                callee.ordinary_array_class,
                callee.token.parts,
                callee.token.rebound_parts,
                callee.table_token_refused,
                callee.caller_declared_slot,
                callee.parent_same_arc_slots
            );
        }
        for pick in &self.witness.picks {
            let target_schema = self.prototype(callable_id(&pick.target)).schema.clone();
            let chosen_schema = self.prototype(callable_id(&pick.chosen)).schema.clone();
            eprintln!(
                "LUA_TYPED_RESOURCE arm={} pick mode={} chosen_is_target={} chosen_is_other={} chosen_prototype={} target_schema={:?} chosen_schema={:?}",
                self.arm.label(),
                pick.mode,
                pick.chosen_is_target,
                pick.chosen_is_other,
                callable_id(&pick.chosen),
                target_schema,
                chosen_schema,
            );
        }
        for gate in &self.witness.gates {
            eprintln!(
                "LUA_TYPED_RESOURCE arm={} gate callable={:?} receiver={:?} class={:?} issued={} token_raw={} token_parts={:?} token_issued={} refused={}",
                self.arm.label(),
                gate.callable,
                gate.receiver,
                gate.receiver_class,
                gate.receiver_is_issued,
                gate.token_raw,
                gate.token_parts,
                gate.token_is_issued,
                gate.refused
            );
        }
        if let Some(token) = &self.witness.table_token {
            eprintln!(
                "LUA_TYPED_RESOURCE arm={} table_token raw={} parts={:?}",
                self.arm.label(),
                token.raw,
                token.parts
            );
        }
    }

    fn exit(&self) -> Option<Vec<Value>> {
        let Ok(VmStatus::Halted) = &self.status else {
            return None;
        };
        let [Value::Array(items)] = self.stack.as_slice() else {
            return None;
        };
        Some(items.as_ref().clone())
    }

    fn labels(&self) -> Vec<&str> {
        self.witness
            .marks
            .iter()
            .map(|mark| mark.label.as_str())
            .collect()
    }

    fn mark(&self, label: &str) -> &MarkRecord {
        self.witness
            .marks
            .iter()
            .find(|mark| mark.label == label)
            .unwrap_or_else(|| {
                panic!(
                    "{}: missing '{label}' marker, saw {:?}",
                    self.arm.label(),
                    self.labels()
                )
            })
    }

    fn pick(&self) -> &PickRecord {
        self.witness
            .picks
            .first()
            .unwrap_or_else(|| panic!("{}: the typed host never ran", self.arm.label()))
    }

    fn assert_fuel(&self) {
        assert!(
            self.fuel_consumed > 0,
            "{}: no fuel spent",
            self.arm.label()
        );
        assert!(
            self.fuel_consumed < FUEL_BUDGET,
            "{}: exhausted fuel",
            self.arm.label()
        );
    }

    fn assert_typed_import(&self) {
        let index = self
            .host_imports
            .iter()
            .position(|import| import.name == "note")
            .unwrap_or_else(|| panic!("{}: note is not an import", self.arm.label()));
        assert_eq!(
            self.host_imports[index].return_type,
            vm::ValueType::Callable,
            "{}: the import must carry a callable return",
            self.arm.label()
        );
        let schema = self.host_schemas[index].as_ref().unwrap_or_else(|| {
            panic!(
                "{}: note has no program schema; the runtime check would be disabled",
                self.arm.label()
            )
        });
        assert_eq!(
            schema.return_type,
            typed_host_callable_schema(),
            "{}: the program must carry the full typed callable return schema",
            self.arm.label()
        );
        assert_eq!(
            schema.params.len(),
            6,
            "{}: the typed adapter arity",
            self.arm.label()
        );

        let userdata = self
            .host_imports
            .iter()
            .position(|import| import.name == "fault")
            .unwrap_or_else(|| panic!("{}: fault is not an import", self.arm.label()));
        let schema = self.host_schemas[userdata]
            .as_ref()
            .unwrap_or_else(|| panic!("{}: fault has no program schema", self.arm.label()));
        assert_eq!(
            schema.return_type,
            HostTypeSchema::Resource(LuaUserdataResource::key()),
            "{}: the userdata return must stay a typed resource",
            self.arm.label()
        );

        let carrier = self
            .host_imports
            .iter()
            .position(|import| import.name == "tick")
            .unwrap_or_else(|| panic!("{}: tick is not an import", self.arm.label()));
        let schema = self.host_schemas[carrier]
            .as_ref()
            .unwrap_or_else(|| panic!("{}: tick has no program schema", self.arm.label()));
        assert_eq!(
            schema.return_type,
            HostTypeSchema::Array(Box::new(HostTypeSchema::Int)),
            "{}: the carrier return must declare the real Array<Int> shape",
            self.arm.label()
        );
    }

    fn assert_typed_local(&self) {
        let slot = self
            .patch
            .typed_local
            .unwrap_or_else(|| panic!("{}: no typed local", self.arm.label()));
        assert_eq!(
            self.patch.declared_schema,
            Some(userdata_callable_schema()),
            "{}: the generated typed local must declare the exact callable schema",
            self.arm.label()
        );
        assert!(
            self.patch.call.is_some(),
            "{}: the typed local must be the one the call node uses",
            self.arm.label()
        );
        assert!(
            self.patch.target_declared,
            "{}: the target closure binding must carry the declared prototype schema",
            self.arm.label()
        );
        assert!(
            self.patch.other_declared,
            "{}: the other-resource closure binding must carry its declared schema",
            self.arm.label()
        );
        assert_eq!(
            self.patch.parent_parameters.len(),
            PARENT_PARAMS,
            "{}: the parent parameter list",
            self.arm.label()
        );
        assert!(
            self.patch.continuation_rewritten,
            "{}: the real lowered completion continuation must be retained",
            self.arm.label()
        );
        let resource_locals: Vec<usize> = self
            .local_schemas
            .iter()
            .enumerate()
            .filter(|(_, schema)| {
                schema.as_ref() == Some(&TypeSchema::Resource(LuaUserdataResource::key()))
            })
            .map(|(slot, _)| slot)
            .collect();
        if self.arm != Arm::OtherArgumentToken {
            assert!(
                !resource_locals.is_empty(),
                "{}: at least one local must carry the typed userdata schema",
                self.arm.label()
            );
        }
        eprintln!(
            "LUA_TYPED_RESOURCE arm={} typed_local={slot} resource_locals={resource_locals:?}",
            self.arm.label()
        );
    }

    fn prototype(&self, id: u32) -> &vm::CallablePrototype {
        self.prototypes
            .get(id as usize)
            .unwrap_or_else(|| panic!("{}: prototype {id} is missing", self.arm.label()))
    }

    fn assert_two_parameter_script_prototype(&self, id: u32, schema: TypeSchema) {
        let prototype = self.prototype(id);
        assert_eq!(
            prototype.arity,
            CALL_ARGC,
            "{}: the callable must be a two-parameter Lua function",
            self.arm.label()
        );
        assert_eq!(
            prototype.schema,
            Some(schema),
            "{}: the compiled callable prototype schema",
            self.arm.label()
        );
        assert_eq!(
            prototype.parameter_slots.len(),
            usize::from(CALL_ARGC),
            "{}: the compiled formal parameter slots",
            self.arm.label()
        );
        assert!(
            matches!(prototype.target, vm::CallableTarget::ScriptFunction(_)),
            "{}: the executed callable must be a script function",
            self.arm.label()
        );
        assert!(
            prototype.capture_slots.is_empty() && prototype.capture_source_slots.is_empty(),
            "{}: the declared closure must be zero-capture",
            self.arm.label()
        );
    }
}

fn same_callable(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Callable(left), Value::Callable(right)) => Arc::ptr_eq(left, right),
        _ => false,
    }
}

fn same_array(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Array(left), Value::Array(right)) => Arc::ptr_eq(left, right),
        _ => false,
    }
}

fn callable_id(value: &Value) -> u32 {
    match value {
        Value::Callable(callable) => callable.prototype_id,
        other => panic!("expected a callable, got {other:?}"),
    }
}

/// Every raw callback entry whose actual frame was one of the declared
/// two-parameter typed script closures this module compiles. This reads the
/// entry record, not the marker record: a probe or frame check that fails
/// after entry cannot remove the evidence that the frame started.
fn typed_callback_entries(run: &ArmRun) -> Vec<&EntryRecord> {
    run.witness
        .entries
        .iter()
        .filter(|entry| {
            entry
                .prototype_id
                .and_then(|id| run.prototypes.get(id as usize))
                .is_some_and(|prototype| {
                    matches!(prototype.target, vm::CallableTarget::ScriptFunction(_))
                        && matches!(
                            prototype.schema.as_ref(),
                            Some(TypeSchema::Callable { params, .. })
                                if params.len() == usize::from(CALL_ARGC)
                                    && matches!(params.get(1), Some(TypeSchema::Resource(_)))
                        )
                })
        })
        .collect()
}

/// Binds the executed callee marker to the one decoded `CallValue` whose
/// successor is that child frame's resume address and whose owner is the
/// active parent frame.
fn assert_resumed_call_site(run: &ArmRun, child_label: &str) -> (usize, u32) {
    let child = run.mark(child_label);
    let caller = child
        .parent_prototype
        .expect("the callee marker must have a caller frame");
    let return_ip = child
        .last_return_ip
        .expect("the callee marker must resume through a CallValue");
    let matching: Vec<_> = run
        .call_sites
        .iter()
        .filter(|site| {
            site.owner == Some(caller) && site.return_ip == return_ip && site.argc == CALL_ARGC
        })
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "{}: expected exactly one executed CallValue(owner={caller:?}, return_ip={return_ip}, argc={CALL_ARGC}); sites={:?}",
        run.arm.label(),
        run.call_sites
    );
    let site = matching[0];
    assert_eq!(
        site.ip + 2,
        site.return_ip,
        "{}: CallValue carries one argc operand byte",
        run.arm.label()
    );
    (return_ip, caller)
}

fn assert_receiver_and_token(run: &ArmRun) {
    let callee = run
        .witness
        .marks
        .iter()
        .find_map(|mark| mark.callee.as_ref())
        .unwrap_or_else(|| panic!("{}: no callee frame was observed", run.arm.label()));
    assert_eq!(
        callee.ordinary_array_class,
        "Unissued",
        "{}: an ordinary array must not be the issued carrier",
        run.arm.label()
    );
    assert!(
        callee.receiver_class.starts_with("Table("),
        "{}: the receiver must be the issued heap table carrier, saw {}",
        run.arm.label(),
        callee.receiver_class
    );
    assert_eq!(
        callee.receiver_class,
        format!(
            "{:?}",
            GuestClass::Table(
                run.witness
                    .issued_table_ref
                    .expect("the issued table handle must stay live")
            )
        ),
        "{}: the callee's receiver must be the same live heap table",
        run.arm.label()
    );
    let issued_table = run
        .witness
        .issued_table
        .as_ref()
        .unwrap_or_else(|| panic!("{}: no issued carrier", run.arm.label()));
    assert!(
        matches!(issued_table, Value::Array(items)
            if items.iter().all(|value| matches!(value, Value::Int(_))),
        ),
        "{}: the issued carrier elements must match the declared Array<Int> schema",
        run.arm.label()
    );
    assert!(
        same_array(
            &callee.parameter_values[0].clone().expect("receiver slot"),
            issued_table
        ),
        "{}: the callee must receive the original issued Array<Int> carrier",
        run.arm.label()
    );
    let issued_userdata = run
        .witness
        .issued_userdata
        .as_ref()
        .unwrap_or_else(|| panic!("{}: no issued userdata", run.arm.label()));
    let issued_parts = (
        issued_userdata.domain(),
        issued_userdata.slot(),
        issued_userdata.generation(),
    );
    assert_eq!(
        callee.token.parts,
        issued_parts,
        "{}: the Core borrow must name the issued userdata",
        run.arm.label()
    );
    assert_eq!(
        callee.token.rebound_parts,
        issued_parts,
        "{}: the heap borrow must name the same userdata",
        run.arm.label()
    );
    assert!(
        callee.table_token_refused,
        "{}: the same handle must be refused as a lua.table token",
        run.arm.label()
    );
    assert_eq!(
        callee.parameter_values[1],
        Some(Value::Int(
            run.witness
                .issued_userdata_raw
                .expect("the fixture issued one userdata")
        )),
        "{}: the callee must receive the issued userdata token carrier",
        run.arm.label()
    );
    assert_eq!(
        callee.parameter_slots,
        run.prototype(callee.prototype_id).parameter_slots,
        "{}: the witness must read the declared callee slots",
        run.arm.label()
    );
}

// ---- the arms -------------------------------------------------------------

#[test]
fn active_parent_invokes_typed_resource_callable() {
    let positive = run_arm(Arm::Positive);
    assert_positive(&positive);

    let other_key = run_arm(Arm::OtherResourceKey);
    assert_other_resource_key_control(&positive, &other_key);

    let other_argument = run_arm(Arm::OtherArgumentToken);
    assert_other_argument_control(&other_argument);
}

fn assert_positive(run: &ArmRun) {
    run.assert_fuel();
    run.assert_typed_import();
    run.assert_typed_local();
    assert!(
        matches!(run.status, Ok(VmStatus::Halted)),
        "positive: the parent must run to a normal halt, got {:?}",
        run.status
    );
    assert_eq!(
        run.labels(),
        ["parent-enter", "target-enter", "parent-after", "done"],
        "positive: the callee must run between the parent's own markers"
    );
    let values = run.exit().unwrap_or_else(|| {
        panic!(
            "positive: expected the chunk completion, got {:?}",
            run.status
        )
    });
    assert_eq!(values.len(), 4, "positive: [true] plus three values");
    let selected = &values[1];
    let receiver = &values[2];
    let table_token = &values[3];

    let pick = run.pick();
    assert_eq!(pick.mode, 0, "positive: the staged arm");
    assert!(
        pick.chosen_is_target,
        "positive: the typed host must return the exact written target Arc"
    );
    assert_eq!(
        selected,
        &Value::Bool(true),
        "positive: the parent returns its own completion status"
    );
    assert!(
        same_callable(&pick.chosen, &pick.target),
        "positive: the selected value must be the written target"
    );
    let candidates = [
        &pick.target,
        &pick.first,
        &pick.second,
        &pick.single,
        &pick.other,
    ];
    for left in 0..candidates.len() {
        for right in (left + 1)..candidates.len() {
            assert!(
                !same_callable(candidates[left], candidates[right]),
                "positive: the written candidates must be distinct Arcs"
            );
        }
    }

    let target_id = callable_id(&pick.chosen);
    run.assert_two_parameter_script_prototype(target_id, userdata_callable_schema());
    let prototype = run.prototype(target_id);
    assert_eq!(
        prototype.schema,
        Some(userdata_callable_schema()),
        "positive: the real target prototype must declare the exact userdata signature"
    );

    // The child frame received the original issued carrier and token, and the
    // parent frame still holds the same returned Arc in two slots (its `cb`
    // parameter and the typed local).
    let callee = run
        .witness
        .marks
        .iter()
        .find_map(|mark| mark.callee.as_ref())
        .expect("positive: the child frame must be observed");
    assert_eq!(
        callee.prototype_id, target_id,
        "positive: the executed frame must be the returned callable"
    );
    assert!(
        callee.parameter_values.len() == usize::from(CALL_ARGC),
        "positive: the callee must declare two formal slots"
    );
    assert!(
        same_callable(
            callee
                .caller_callable
                .as_ref()
                .expect("positive: the parent's cb slot must hold the callable"),
            &pick.chosen
        ),
        "positive: the parent's held argument must be the returned Arc"
    );
    let caller_slot = run
        .mark("parent-enter")
        .last_prototype
        .and_then(|id| run.prototype(id).parameter_slots.first().copied());
    assert_eq!(
        callee.caller_declared_slot, caller_slot,
        "positive: the held argument is the parent's first declared slot"
    );
    assert!(
        callee.parent_same_arc_slots.len() >= 2,
        "positive: the parent frame must hold the returned Arc in more than one slot (cb and the typed local), saw {:?}",
        callee.parent_same_arc_slots
    );
    let parent_first_slot = caller_slot.expect("positive: the parent's first slot");
    assert!(
        callee.parent_same_arc_slots.contains(&parent_first_slot),
        "positive: the cb slot is one of the two matching slots"
    );
    assert_receiver_and_token(run);

    let entry = run
        .mark("parent-enter")
        .parent_entry
        .as_ref()
        .expect("positive: the parent entry must be observed");
    assert_eq!(
        entry.argument_raw,
        run.witness
            .issued_userdata_raw
            .expect("positive: the fixture issued one userdata"),
        "positive: the parent receives the original userdata token as its argument"
    );
    let continuation = run
        .mark("parent-after")
        .continuation
        .as_ref()
        .expect("positive: the parent continuation must re-borrow the token");
    assert_eq!(
        continuation.raw,
        run.witness
            .issued_userdata_raw
            .expect("positive: the fixture issued one userdata"),
        "positive: the continuation must use the same token handle"
    );
    let issued_parts = {
        let issued = run
            .witness
            .issued_userdata
            .as_ref()
            .expect("positive: the fixture issued one userdata");
        (issued.domain(), issued.slot(), issued.generation())
    };
    assert_eq!(
        continuation.parts, issued_parts,
        "positive: the continuation borrow must name the same userdata"
    );
    assert_eq!(
        continuation.rebound_parts, issued_parts,
        "positive: the continuation heap borrow must name the same userdata"
    );
    assert_eq!(
        run.mark("parent-after").child_return,
        Some(Value::Bool(true)),
        "positive: the parent frame must hold the child's real Bool return"
    );

    let (return_ip, caller) = assert_resumed_call_site(run, "target-enter");
    assert!(return_ip > 0, "positive: the resume address is real");
    assert_eq!(
        run.mark("parent-enter").last_prototype,
        Some(caller),
        "positive: the parent's own marker names the caller prototype"
    );
    assert_eq!(
        run.mark("parent-enter").frame_len + 1,
        run.mark("target-enter").frame_len,
        "positive: the callee frame is one below the parent"
    );
    assert_eq!(
        run.mark("parent-after").frame_len,
        run.mark("parent-enter").frame_len,
        "positive: the parent resumes at its own depth"
    );
    assert_eq!(
        run.mark("done").parent_prototype,
        None,
        "positive: the chunk marker has no caller"
    );

    // The raw entry record is committed before any probe or frame check, so
    // exactly one typed callback entry must exist independently of the marker
    // records that are committed after the typed borrows.
    let callback_entries = typed_callback_entries(run);
    assert_eq!(
        callback_entries.len(),
        1,
        "positive: exactly one typed callback frame entry, saw {:?}",
        run.witness.entries
    );
    let callback_entry = callback_entries[0];
    assert_eq!(
        callback_entry.prototype_id,
        Some(target_id),
        "positive: the entry must be the returned callable's prototype"
    );
    assert_eq!(
        callback_entry.label.as_str(),
        TARGET_LABEL,
        "positive: the entry must be the target's first marker"
    );
    assert_eq!(
        callback_entry.frame_len,
        run.mark("target-enter").frame_len,
        "positive: the raw entry depth must match the child marker depth"
    );
    assert_eq!(
        callback_entry.return_ip,
        run.mark("target-enter").last_return_ip,
        "positive: the raw entry must carry the child's resume address"
    );
    assert_eq!(
        callback_entry.return_ip,
        Some(return_ip),
        "positive: the entry resume address must be the decoded CallValue"
    );
    assert_eq!(
        callback_entry.parent_prototype,
        Some(caller),
        "positive: the entry's caller must be the parent prototype"
    );
    assert_eq!(
        callback_entry.frame_len,
        run.mark("parent-enter").frame_len + 1,
        "positive: the typed callback is one frame below its caller"
    );

    let call = run
        .patch
        .call
        .as_ref()
        .expect("positive: the call node was patched");
    assert_eq!(call.argc, CALL_ARGC);
    assert_ne!(
        call.written_callee, call.typed_callee,
        "positive: the callee slot must be retargeted"
    );
    assert_eq!(
        call.arg_slots,
        vec![
            run.patch.parent_parameters[PARENT_RECEIVER_SLOT],
            run.patch.parent_parameters[PARENT_ARGUMENT_SLOT],
        ],
        "positive: the call still passes the declared receiver and argument slots"
    );

    assert!(
        same_array(
            receiver,
            &run.witness.issued_table.clone().expect("issued carrier")
        ),
        "positive: the chunk returns the issued Array<Int> carrier"
    );
    assert_eq!(
        table_token,
        &Value::Int(
            run.witness
                .table_token
                .as_ref()
                .expect("the fixture issued one table token")
                .raw
        ),
        "positive: the chunk returns the issued table token"
    );
}

fn assert_other_resource_key_control(positive: &ArmRun, run: &ArmRun) {
    run.assert_fuel();
    run.assert_typed_import();
    run.assert_typed_local();
    match &run.status {
        Err(VmError::TypeMismatch(kind)) => assert_eq!(
            *kind, "callable",
            "other-resource-key: the Core callable return check must refuse the value"
        ),
        other => panic!(
            "other-resource-key: expected the Core callable type refusal at the return boundary, got {other:?}"
        ),
    }
    assert_eq!(
        run.labels(),
        ["parent-enter"],
        "other-resource-key: the parent must stop at the typed host return"
    );
    assert_eq!(
        typed_callback_entries(run).len(),
        0,
        "other-resource-key: the refused callable must not enter a callback frame"
    );
    let pick = run.pick();
    assert_eq!(
        pick.mode, 5,
        "other-resource-key: the staged arm must select the other candidate"
    );
    assert!(
        pick.chosen_is_other,
        "other-resource-key: the host must return the exact other-resource Arc it was handed"
    );
    assert!(
        !pick.chosen_is_target,
        "other-resource-key: the returned value must not be the target Arc"
    );
    assert!(
        same_callable(&pick.chosen, &pick.other),
        "other-resource-key: the returned value is the written other instance"
    );
    assert!(
        run.witness.marks.iter().all(|mark| mark.callee.is_none()),
        "other-resource-key: no callback frame may start"
    );
    assert!(
        run.exit().is_none(),
        "other-resource-key: the failed run must have no chunk completion"
    );

    // Same arity, receiver schema and result; only the declared resource key
    // of the second parameter differs.
    let chosen_id = callable_id(&pick.chosen);
    run.assert_two_parameter_script_prototype(chosen_id, table_callable_schema());
    let target_id = callable_id(&pick.target);
    positive.assert_two_parameter_script_prototype(target_id, userdata_callable_schema());
    let chosen = run.prototype(chosen_id);
    let target = run.prototype(target_id);
    assert_eq!(
        chosen.arity, target.arity,
        "the two declarations have the same arity"
    );
    assert_eq!(
        chosen.parameter_slots.len(),
        target.parameter_slots.len(),
        "the two declarations have the same formal arity"
    );
    let (
        Some(TypeSchema::Callable {
            params: chosen_params,
            result: chosen_result,
        }),
        Some(TypeSchema::Callable {
            params: target_params,
            result: target_result,
        }),
    ) = (&chosen.schema, &target.schema)
    else {
        panic!("both selected prototypes must carry their declared callable schemas");
    };
    assert_eq!(
        chosen_params[0], target_params[0],
        "the receiver schema must be identical"
    );
    assert_eq!(
        chosen_result, target_result,
        "the declared result must be identical"
    );
    assert_eq!(
        chosen_params[1],
        TypeSchema::Resource(LuaTableResource::key()),
        "the refused declaration must name the other legal resource key"
    );
    assert_eq!(
        target_params[1],
        TypeSchema::Resource(LuaUserdataResource::key()),
        "the target declaration must still name lua.userdata"
    );
    assert!(
        run.witness
            .marks
            .iter()
            .all(|mark| mark.label != "other-enter"),
        "other-resource-key: the callback body itself must not run"
    );

    // The static host body ran, but its return never became guest-visible.
    assert_eq!(
        run.witness.picks.len(),
        1,
        "other-resource-key: the static host body must run exactly once"
    );
    eprintln!(
        "LUA_TYPED_RESOURCE arm=other-resource-key layer=runtime-return compile=ok static_body_ran=1 callback_frames=0 diagnostic=TypeMismatch(callable)"
    );
}

fn assert_other_argument_control(run: &ArmRun) {
    run.assert_fuel();
    run.assert_typed_import();
    run.assert_typed_local();
    let table = run
        .witness
        .table_token
        .as_ref()
        .expect("other-argument-token: the fixture issued one table token");
    let message = match &run.status {
        Err(VmError::HostError(message)) => message.clone(),
        other => panic!(
            "other-argument-token: expected the Core resource key HostError at the call boundary, got {other:?}"
        ),
    };
    // This is the native Core diagnostic itself: stable code, operation scope,
    // both keys and the raw handle of the actual (issued) `lua.table` token.
    // The exact equality also rejects the test's own probe wrappers, which
    // prefix their own text before the same native message.
    let expected = format!(
        "resource error [resource_type_key_mismatch] in resource::table: \
         resource type key does not match expected key 'lua.userdata'; \
         actual key is 'lua.table' (value: {})",
        table.raw
    );
    assert_eq!(
        message, expected,
        "other-argument-token: the diagnostic must be the native resource_type_key_mismatch \
         error bound to the issued lua.table token, not a test probe wrapper"
    );
    eprintln!(
        "LUA_TYPED_RESOURCE arm=other-argument-token layer=call-boundary diagnostic=HostError({message})"
    );
    assert_eq!(
        run.labels(),
        ["parent-enter"],
        "other-argument-token: the parent must stop at the CallValue"
    );
    assert_eq!(
        typed_callback_entries(run).len(),
        0,
        "other-argument-token: the typed callback frame must not enter"
    );
    let pick = run.pick();
    assert_eq!(pick.mode, 0, "other-argument-token: the staged arm");
    assert!(
        pick.chosen_is_target,
        "other-argument-token: the returned callable must be the exact target Arc"
    );
    let target_id = callable_id(&pick.chosen);
    run.assert_two_parameter_script_prototype(target_id, userdata_callable_schema());
    assert_eq!(
        run.prototype(target_id).schema,
        Some(userdata_callable_schema()),
        "other-argument-token: the correct callable declaration must be unchanged"
    );
    assert!(
        run.witness.marks.iter().all(|mark| mark.callee.is_none()),
        "other-argument-token: no callback frame may start"
    );
    assert!(
        run.labels().iter().all(|label| *label != "target-enter"),
        "other-argument-token: the callback body itself must not run"
    );
    assert!(
        run.exit().is_none(),
        "other-argument-token: the failed run must have no chunk completion"
    );

    let issued = run
        .witness
        .issued_userdata_raw
        .expect("other-argument-token: the fixture issued one userdata token");
    assert_ne!(
        table.raw, issued,
        "other-argument-token: the table token is a different handle"
    );
    let entry = run
        .mark("parent-enter")
        .parent_entry
        .as_ref()
        .expect("other-argument-token: the parent entry must be observed");
    assert_eq!(
        entry.argument_raw, table.raw,
        "other-argument-token: the actual parent argument must be the real lua.table token"
    );

    // The token itself really is a registered `lua.table` resource; the typed
    // probe recorded its payload while the VM was live.
    let issued_userdata = run
        .witness
        .issued_userdata
        .as_ref()
        .expect("other-argument-token: the fixture issued one userdata");
    let userdata_parts = (
        issued_userdata.domain(),
        issued_userdata.slot(),
        issued_userdata.generation(),
    );
    assert_ne!(
        table.parts, userdata_parts,
        "other-argument-token: the table token names another resource slot"
    );

    // The call site is still the compiled CallValue(2) owned by the parent;
    // the failure is the runtime argument check, not a compile-time refusal.
    let parent_id = run
        .mark("parent-enter")
        .last_prototype
        .expect("other-argument-token: the parent frame");
    let matching: Vec<_> = run
        .call_sites
        .iter()
        .filter(|site| site.owner == Some(parent_id) && site.argc == CALL_ARGC)
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "other-argument-token: exactly one compiled CallValue(2) belongs to the parent, saw {:?}",
        run.call_sites
    );
    assert!(
        run.witness
            .marks
            .iter()
            .all(|mark| mark.label != "parent-after"),
        "other-argument-token: the parent continuation must not be reached"
    );
    assert_eq!(
        run.patch.declared_schema,
        Some(userdata_callable_schema()),
        "other-argument-token: the typed local declaration must stay exact"
    );
    eprintln!(
        "LUA_TYPED_RESOURCE arm=other-argument-token layer=call-boundary compile=ok same_callable=1 same_declaration=1 actual_token={} diagnostic=HostError",
        table.raw
    );
}

// ---- the pre-commit borrow seam ------------------------------------------

#[test]
fn active_parent_precommits_typed_borrow_operands_before_the_call() {
    let positive = run_arm(Arm::PrecommitBorrowSeam);
    assert_positive(&positive);
    assert_precommit_import(&positive);
    assert_precommit_positive(&positive);

    let control = run_arm(Arm::PrecommitUnissuedReceiver);
    assert_precommit_import(&control);
    assert_precommit_unissued_receiver_control(&control);
}

/// The live catalog install, the program's complete schema and fingerprint,
/// the resolved `Borrow`/`Value`/`Borrow` call, the binding and the IR slots.
fn assert_precommit_import(run: &ArmRun) {
    let label = run.arm.label();
    let catalog = transport_catalog();
    let function = catalog
        .function(PRECOMMIT_IMPORT)
        .unwrap_or_else(|| panic!("{label}: the catalog is missing '{PRECOMMIT_IMPORT}'"));
    assert_eq!(
        function,
        &precommit_import_schema(),
        "{label}: the live catalog install"
    );
    let expected = HostImportSchema::from_function(catalog, function);
    assert_eq!(
        expected.fingerprint,
        catalog.fingerprint(),
        "{label}: the catalog fingerprint"
    );

    let index = run
        .host_imports
        .iter()
        .position(|import| import.name == PRECOMMIT_IMPORT)
        .unwrap_or_else(|| panic!("{label}: '{PRECOMMIT_IMPORT}' is not a compiled import"));
    assert_eq!(
        run.host_imports[index].arity, 3,
        "{label}: the import arity"
    );
    assert_eq!(
        run.host_imports[index].return_type,
        vm::ValueType::Bool,
        "{label}: the import's coarse return"
    );
    let schema = run.host_schemas[index]
        .as_ref()
        .unwrap_or_else(|| panic!("{label}: the seam import must carry its full schema"));
    assert_eq!(
        schema, &expected,
        "{label}: the program must carry the complete catalog schema and fingerprint"
    );

    let gate = run
        .patch
        .gate
        .as_ref()
        .unwrap_or_else(|| panic!("{label}: the precommit seam was not inserted"));
    assert_eq!(gate.import_index, index as u16, "{label}: the import slot");
    assert_eq!(
        gate.resolution.name, PRECOMMIT_IMPORT,
        "{label}: the resolved name"
    );
    assert_eq!(
        gate.resolution.passing,
        [
            HostParamPassing::Borrow,
            HostParamPassing::Value,
            HostParamPassing::Borrow,
        ],
        "{label}: the resolved passing modes"
    );
    assert_eq!(
        gate.resolution.host_params,
        [
            typed_host_callable_schema(),
            HostTypeSchema::Array(Box::new(HostTypeSchema::Int)),
            HostTypeSchema::Resource(LuaUserdataResource::key()),
        ],
        "{label}: the resolved concrete host schemas"
    );
    assert_eq!(
        gate.resolution.host_return_type,
        HostTypeSchema::Bool,
        "{label}: the resolved host return"
    );
    assert_eq!(
        gate.resolution.fingerprint,
        catalog.fingerprint(),
        "{label}: the resolution fingerprint"
    );

    assert_eq!(
        Some(gate.typed_local),
        run.patch.typed_local,
        "{label}: the seam must receive the typed local"
    );
    let call = run
        .patch
        .call
        .as_ref()
        .unwrap_or_else(|| panic!("{label}: the dynamic call was not patched"));
    assert_eq!(
        call.typed_callee, gate.typed_local,
        "{label}: the seam and CallValue must share the typed local"
    );
    assert!(
        gate.result_slot != gate.typed_local
            && !run.patch.parent_parameters.contains(&gate.result_slot),
        "{label}: the seam result must land in a fresh local"
    );
    assert_eq!(
        run.bound_functions, 5,
        "{label}: all five precommit exact static imports must bind"
    );
    eprintln!(
        "{label}: precommit import slot={index} schema={schema:?} resolution_fingerprint={:?} bound={}",
        gate.resolution.fingerprint, run.bound_functions
    );
}

fn gate_record(run: &ArmRun) -> &GateRecord {
    run.witness
        .gates
        .first()
        .unwrap_or_else(|| panic!("{}: the precommit static body never ran", run.arm.label()))
}

fn precommit_entries(run: &ArmRun) -> Vec<&EntryRecord> {
    run.witness
        .entries
        .iter()
        .filter(|entry| entry.label == PRECOMMIT_ENTRY_LABEL)
        .collect()
}

/// The parent's compiled static seam `Call` and its one dynamic
/// `CallValue(2)`, both owned by the active parent prototype.
fn parent_seam_and_call(run: &ArmRun) -> (usize, usize) {
    let parent = run
        .mark("parent-enter")
        .last_prototype
        .unwrap_or_else(|| panic!("{}: the parent frame has no prototype", run.arm.label()));
    let seams: Vec<_> = run
        .host_call_sites
        .iter()
        .filter(|(_, _, owner)| *owner == Some(parent))
        .collect();
    assert_eq!(
        seams.len(),
        1,
        "{}: expected exactly one compiled seam Call owned by the parent, saw {:?}",
        run.arm.label(),
        run.host_call_sites
    );
    let (seam_ip, argc, _) = *seams[0];
    assert_eq!(
        argc,
        3,
        "{}: the seam Call must carry three operands",
        run.arm.label()
    );
    let calls: Vec<_> = run
        .call_sites
        .iter()
        .filter(|site| site.owner == Some(parent) && site.argc == CALL_ARGC)
        .collect();
    assert_eq!(
        calls.len(),
        1,
        "{}: expected exactly one compiled CallValue(2) owned by the parent, saw {:?}",
        run.arm.label(),
        run.call_sites
    );
    (seam_ip, calls[0].ip)
}

fn assert_precommit_positive(run: &ArmRun) {
    let gate = gate_record(run);
    assert!(
        !gate.refused,
        "precommit-borrow-seam: the static body must accept the real operands"
    );
    let issued_ref = run
        .witness
        .issued_table_ref
        .expect("precommit-borrow-seam: the fixture issued one table");
    assert_eq!(
        gate.receiver_class,
        GuestClass::Table(issued_ref),
        "precommit-borrow-seam: the live heap must classify the operand as the issued table"
    );
    assert!(
        gate.receiver_is_issued,
        "precommit-borrow-seam: the receiver must be the issued LuaHeap carrier"
    );

    let pick = run.pick();
    assert!(
        same_callable(&gate.callable, &pick.chosen),
        "precommit-borrow-seam: the callable operand must be the exact Arc note returned"
    );
    let issued = run
        .witness
        .issued_table
        .as_ref()
        .expect("precommit-borrow-seam: the fixture issued one table");
    assert!(
        same_array(&gate.receiver, issued),
        "precommit-borrow-seam: the receiver operand must be the issued array"
    );
    assert!(
        gate.token_is_issued,
        "precommit-borrow-seam: the token payload must be the issued lua.userdata"
    );
    let issued_raw = run
        .witness
        .issued_userdata_raw
        .expect("precommit-borrow-seam: the fixture issued one userdata");
    assert_eq!(
        gate.token_raw, issued_raw,
        "precommit-borrow-seam: the seam must receive the issued token handle"
    );
    let issued_parts = {
        let issued = run
            .witness
            .issued_userdata
            .as_ref()
            .expect("precommit-borrow-seam: the fixture issued one userdata");
        (issued.domain(), issued.slot(), issued.generation())
    };
    assert_eq!(
        gate.token_parts,
        Some(issued_parts),
        "precommit-borrow-seam: the formal borrow must name the issued payload"
    );

    let parent = run.mark("parent-enter");
    let entries = precommit_entries(run);
    assert_eq!(
        entries.len(),
        1,
        "precommit-borrow-seam: exactly one independent seam entry"
    );
    let entry = entries[0];
    assert_eq!(
        (entry.frame_len, entry.prototype_id, entry.parent_prototype),
        (
            parent.frame_len,
            parent.last_prototype,
            parent.parent_prototype
        ),
        "precommit-borrow-seam: the static body must run in the active parent frame"
    );

    let (seam_ip, callvalue_ip) = parent_seam_and_call(run);
    assert!(
        seam_ip < callvalue_ip,
        "precommit-borrow-seam: the static seam call must precede CallValue({CALL_ARGC})"
    );
    let callee = run
        .witness
        .marks
        .iter()
        .find_map(|mark| mark.callee.as_ref())
        .expect("precommit-borrow-seam: the child frame must be observed");
    assert!(
        same_callable(
            &gate.callable,
            callee
                .caller_callable
                .as_ref()
                .expect("precommit-borrow-seam: the parent cb slot must hold the callable")
        ),
        "precommit-borrow-seam: CallValue must reuse the seam's callable Arc"
    );
    let child_receiver = callee
        .parameter_values
        .first()
        .cloned()
        .flatten()
        .expect("precommit-borrow-seam: the child's receiver slot");
    assert!(
        same_array(&gate.receiver, &child_receiver),
        "precommit-borrow-seam: the child frame must reuse the seam's receiver Arc"
    );
    assert_eq!(
        callee.parameter_values.get(1).cloned().flatten(),
        Some(Value::Int(gate.token_raw)),
        "precommit-borrow-seam: the child frame must reuse the seam's token handle"
    );
    eprintln!(
        "LUA_TYPED_RESOURCE arm=precommit-borrow-seam seam_ip={seam_ip} callvalue_ip={callvalue_ip} child_entered=1 parent_continuation=1"
    );
}

fn assert_precommit_unissued_receiver_control(run: &ArmRun) {
    run.assert_fuel();
    let gate = gate_record(run);
    assert!(
        gate.refused,
        "precommit-unissued-receiver: the static body must refuse the unissued receiver"
    );
    assert!(
        !gate.receiver_is_issued,
        "precommit-unissued-receiver: the forged receiver must not be issued"
    );
    assert_eq!(
        gate.receiver_class,
        GuestClass::Unissued,
        "precommit-unissued-receiver: an equal-valued fresh array must be unissued"
    );

    let issued = run
        .witness
        .issued_table
        .as_ref()
        .expect("precommit-unissued-receiver: the fixture issued one table");
    let Value::Array(issued_items) = issued else {
        panic!("precommit-unissued-receiver: the issued carrier must be an array");
    };
    assert!(
        matches!(issued_items.as_slice(), [Value::Int(_), Value::Int(_)]),
        "precommit-unissued-receiver: the issued carrier is a real two-integer array"
    );
    let Value::Array(copied_items) = &gate.receiver else {
        panic!("precommit-unissued-receiver: the receiver operand must be an array");
    };
    assert_eq!(
        copied_items.as_slice(),
        issued_items.as_slice(),
        "precommit-unissued-receiver: the copy must have the issued values"
    );
    assert!(
        !same_array(&gate.receiver, issued),
        "precommit-unissued-receiver: the copy must be a different VM array"
    );

    let pick = run.pick();
    assert!(
        same_callable(&gate.callable, &pick.chosen),
        "precommit-unissued-receiver: the callable operand must still be the exact note return"
    );
    assert!(
        gate.token_is_issued,
        "precommit-unissued-receiver: the userdata operand must still be the issued token"
    );
    let issued_raw = run
        .witness
        .issued_userdata_raw
        .expect("precommit-unissued-receiver: the fixture issued one userdata");
    assert_eq!(
        gate.token_raw, issued_raw,
        "precommit-unissued-receiver: the token operand is the issued handle"
    );
    let issued_parts = {
        let issued = run
            .witness
            .issued_userdata
            .as_ref()
            .expect("precommit-unissued-receiver: the fixture issued one userdata");
        (issued.domain(), issued.slot(), issued.generation())
    };
    assert_eq!(
        gate.token_parts,
        Some(issued_parts),
        "precommit-unissued-receiver: the formal borrow names the issued payload"
    );

    let message = match &run.status {
        Err(VmError::HostError(message)) => message.clone(),
        other => panic!(
            "precommit-unissued-receiver: expected the static body's HostError, got {other:?}"
        ),
    };
    assert_eq!(
        message, PRECOMMIT_REFUSAL_RECEIVER,
        "precommit-unissued-receiver: the exact diagnostic must come from the static body"
    );

    assert_eq!(
        run.labels(),
        ["parent-enter"],
        "precommit-unissued-receiver: the parent must stop at the static seam"
    );
    assert_eq!(
        typed_callback_entries(run).len(),
        0,
        "precommit-unissued-receiver: no typed callback frame may start"
    );
    assert!(
        run.witness
            .marks
            .iter()
            .all(|mark| mark.label != "target-enter"
                && mark.label != "parent-after"
                && mark.label != "done"),
        "precommit-unissued-receiver: neither the child frame nor the parent continuation may run"
    );
    assert!(
        run.exit().is_none(),
        "precommit-unissued-receiver: the refused run must have no chunk completion"
    );
    assert_eq!(
        run.witness.gates.len(),
        1,
        "precommit-unissued-receiver: the static body must run exactly once"
    );

    let parent = run.mark("parent-enter");
    let entries = precommit_entries(run);
    assert_eq!(
        entries.len(),
        1,
        "precommit-unissued-receiver: the raw static-body entry must be recorded once"
    );
    let entry = entries[0];
    assert_eq!(
        (entry.frame_len, entry.prototype_id, entry.parent_prototype),
        (
            parent.frame_len,
            parent.last_prototype,
            parent.parent_prototype
        ),
        "precommit-unissued-receiver: the raw entry must be the parent frame, not a child frame or the continuation"
    );

    let (seam_ip, callvalue_ip) = parent_seam_and_call(run);
    assert!(
        seam_ip < callvalue_ip,
        "precommit-unissued-receiver: the static seam precedes the dynamic call"
    );
    eprintln!(
        "LUA_TYPED_RESOURCE arm=precommit-unissued-receiver seam_ip={seam_ip} callvalue_ip={callvalue_ip} receiver_class=Unissued equal_values=true child_entered=0"
    );
}

// ---- two-thread scope controls -------------------------------------------

/// The parked seam compile must carry its own `admit` input and patch; the
/// concurrent control arm must carry neither. Both compiles are real: lexer,
/// parser, resolver, the private plugin and the fixed Core pipeline.
fn assert_compile_scope_ownership(first: &CompiledArm, second: &CompiledArm) {
    assert_eq!(first.arm, Arm::PrecommitBorrowSeam);
    assert_eq!(second.arm, Arm::OtherResourceKey);
    let gate = first.patch.gate.as_ref().unwrap_or_else(|| {
        panic!(
            "compile isolation: the parked seam compile must own its own admit gate patch, saw {:?}",
            first.patch
        )
    });
    assert_eq!(
        gate.resolution.name, PRECOMMIT_IMPORT,
        "compile isolation: the parked arm's gate resolution"
    );
    assert_eq!(
        gate.resolution.passing,
        [
            HostParamPassing::Borrow,
            HostParamPassing::Value,
            HostParamPassing::Borrow,
        ],
        "compile isolation: the parked arm's own Borrow/Value/Borrow input"
    );
    assert_eq!(
        first.patch.call.as_ref().map(|call| call.typed_callee),
        Some(gate.typed_local),
        "compile isolation: the parked arm's seam and CallValue share its typed local"
    );
    assert!(
        second.patch.gate.is_none(),
        "compile isolation: the concurrent arm must not inherit the parked arm's gate, saw {:?}",
        second.patch
    );
    assert_eq!(
        second.patch.declared_schema,
        Some(userdata_callable_schema()),
        "compile isolation: the concurrent arm keeps its own declared typed local"
    );
    assert!(
        second.patch.call.is_some(),
        "compile isolation: the concurrent arm still patches its own dynamic call"
    );

    let first_import = first
        .host_imports
        .iter()
        .position(|import| import.name == PRECOMMIT_IMPORT)
        .unwrap_or_else(|| panic!("compile isolation: the parked arm has no admit import"));
    assert_eq!(
        first.host_imports[first_import].arity, 3,
        "compile isolation: the parked arm's admit arity"
    );
    assert_eq!(
        first.host_schemas[first_import]
            .as_ref()
            .expect("compile isolation: the parked arm's full admit schema")
            .fingerprint,
        transport_catalog().fingerprint(),
        "compile isolation: the parked arm's catalog fingerprint"
    );
    assert!(
        second
            .host_imports
            .iter()
            .all(|import| import.name != PRECOMMIT_IMPORT),
        "compile isolation: the concurrent arm must compile without admit, saw {:?}",
        second.host_imports
    );
    assert_eq!(
        second.host_call_sites.len(),
        0,
        "compile isolation: the concurrent arm has no static seam call"
    );
    assert_eq!(
        first.host_call_sites.len(),
        1,
        "compile isolation: the parked arm has exactly its own static seam call"
    );
    let (seam_ip, argc, owner) = first.host_call_sites[0];
    assert_eq!(argc, 3, "compile isolation: the seam arity");
    assert!(seam_ip > 0, "compile isolation: the seam address is real");
    let owner = owner.expect("compile isolation: the seam has a caller region");
    assert_eq!(
        first
            .prototypes
            .get(owner as usize)
            .expect("compile isolation: the Seam caller prototype")
            .parameter_slots
            .len(),
        PARENT_PARAMS,
        "compile isolation: the seam belongs to the parked arm's parent prototype"
    );
    eprintln!(
        "LUA_TYPED_RESOURCE compile-isolation seam_gate=1 control_gate=0 seam_ip={seam_ip} seam_owner={owner}"
    );
}

/// The seam VM's own witness, checked before any concurrent-arm panic is
/// surfaced so a mixed or lost observation is reported as the business
/// failure it is.
fn assert_runtime_first_scope_only(run: &ArmRun) {
    let modes: Vec<i64> = run.witness.picks.iter().map(|pick| pick.mode).collect();
    assert_eq!(
        modes,
        [0],
        "runtime isolation: the seam VM must keep exactly its own selected mode, saw {modes:?}"
    );
    let entries: Vec<&str> = run
        .witness
        .entries
        .iter()
        .map(|entry| entry.label.as_str())
        .collect();
    assert_eq!(
        entries,
        [
            "chunk-mode",
            "table-token",
            "parent-enter",
            PRECOMMIT_ENTRY_LABEL,
            "target-enter",
            "parent-after",
            "done",
        ],
        "runtime isolation: the seam VM must keep its own entry records, saw {entries:?}"
    );
    assert_eq!(
        run.witness.gates.len(),
        1,
        "runtime isolation: the seam VM must keep its own admit observation"
    );
}

/// Each concurrent arm keeps its own runtime selection, witness records,
/// issued carrier, issued userdata token and callable closures.
fn assert_runtime_scope_ownership(first: &ArmRun, second: &ArmRun) {
    assert_eq!(
        second.witness.picks.len(),
        1,
        "runtime isolation: the control VM must keep exactly its own pick"
    );
    assert_eq!(
        second.witness.picks[0].mode, 5,
        "runtime isolation: the control VM must read its own selection 5"
    );
    let entries: Vec<&str> = second
        .witness
        .entries
        .iter()
        .map(|entry| entry.label.as_str())
        .collect();
    assert_eq!(
        entries,
        ["chunk-mode", "table-token", "parent-enter"],
        "runtime isolation: the control VM must keep its own entry records, saw {entries:?}"
    );
    assert_eq!(
        precommit_entries(second).len(),
        0,
        "runtime isolation: the control VM must have no seam entry"
    );
    assert!(
        second.witness.gates.is_empty(),
        "runtime isolation: the control VM must have no admit observation"
    );
    assert!(
        second.patch.gate.is_none(),
        "runtime isolation: the control compile must carry no gate"
    );

    let first_table = first
        .witness
        .issued_table
        .as_ref()
        .expect("runtime isolation: the seam VM issued one table");
    let second_table = second
        .witness
        .issued_table
        .as_ref()
        .expect("runtime isolation: the control VM issued one table");
    assert!(
        !same_array(first_table, second_table),
        "runtime isolation: the two VMs must issue different carrier Arcs"
    );
    assert_ne!(
        first.witness.issued_table_ref, second.witness.issued_table_ref,
        "runtime isolation: the two VMs must issue different heap tables"
    );
    assert_ne!(
        first.witness.issued_userdata_raw, second.witness.issued_userdata_raw,
        "runtime isolation: the two VMs must issue different userdata tokens"
    );
    assert!(
        !same_callable(&first.pick().chosen, &second.pick().chosen),
        "runtime isolation: the two VMs must return their own callable Arcs"
    );
    assert!(
        !same_callable(&first.pick().chosen, &second.pick().target),
        "runtime isolation: the seam VM's callable must not be the control VM's target"
    );

    let gate = gate_record(first);
    assert_eq!(
        gate.receiver_class,
        GuestClass::Table(
            first
                .witness
                .issued_table_ref
                .expect("runtime isolation: the seam VM issued one table")
        ),
        "runtime isolation: the seam gate must classify its own issued table"
    );
    assert!(
        same_array(&gate.receiver, first_table),
        "runtime isolation: the seam gate must borrow its own receiver Arc"
    );
    assert_eq!(
        gate.token_raw,
        first
            .witness
            .issued_userdata_raw
            .expect("runtime isolation: the seam VM issued one userdata"),
        "runtime isolation: the seam gate must receive its own token"
    );
    assert!(
        gate.token_is_issued && !gate.refused,
        "runtime isolation: the seam gate must accept its own issued operands"
    );
    eprintln!(
        "LUA_TYPED_RESOURCE runtime-isolation seam_entries={} control_entries={} seam_mode=0 control_mode=5 tables_distinct=1 tokens_distinct=1",
        first.witness.entries.len(),
        second.witness.entries.len()
    );
}

#[test]
fn typed_transport_compile_scope_is_isolated_between_two_concurrent_arms() {
    let gate = compile_interleave_gate();
    let first = std::thread::spawn(|| compile_arm_paused(Arm::PrecommitBorrowSeam));
    let second = match gate.wait_reached(InterleavePoint::Compile, &first) {
        Ok(()) => {
            let compiled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                compile_arm(Arm::OtherResourceKey)
            }));
            gate.release(InterleavePoint::Compile);
            match compiled {
                Ok(compiled) => compiled,
                Err(panic) => {
                    let _ = first.join();
                    std::panic::resume_unwind(panic);
                }
            }
        }
        Err(message) => {
            gate.release(InterleavePoint::Compile);
            let _ = first.join();
            panic!("typed transport compile isolation: {message}");
        }
    };
    let first = match first.join() {
        Ok(compiled) => compiled,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    assert_compile_scope_ownership(&first, &second);
}

#[test]
fn typed_transport_runtime_scope_is_isolated_between_two_concurrent_arms() {
    let gate = runtime_interleave_gate();
    let first = std::thread::spawn(|| {
        interleave_plan(InterleaveRole::First, POINT_RUNTIME);
        let run = run_arm(Arm::PrecommitBorrowSeam);
        clear_interleave_plan();
        run
    });
    if let Err(message) = gate.wait_reached(InterleavePoint::Runtime, &first) {
        gate.release(InterleavePoint::Runtime);
        gate.release(InterleavePoint::Take);
        let _ = first.join();
        panic!("typed transport runtime isolation: {message}");
    }
    let second = std::thread::spawn(|| {
        interleave_plan(InterleaveRole::Second, POINT_TAKE);
        let run = run_arm(Arm::OtherResourceKey);
        clear_interleave_plan();
        run
    });
    if let Err(message) = gate.wait_reached(InterleavePoint::Take, &second) {
        gate.release(InterleavePoint::Runtime);
        gate.release(InterleavePoint::Take);
        let _ = first.join();
        let _ = second.join();
        panic!("typed transport runtime isolation: {message}");
    }
    gate.release(InterleavePoint::Runtime);
    let first = first.join();
    gate.release(InterleavePoint::Take);
    let second = second.join();
    let first = match first {
        Ok(run) => run,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    // The seam witness is checked before the concurrent arm's outcome so a
    // mixed or lost observation names the real business failure.
    assert_runtime_first_scope_only(&first);
    let second = match second {
        Ok(run) => run,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    assert_positive(&first);
    assert_precommit_import(&first);
    assert_precommit_positive(&first);
    assert_other_resource_key_control(&first, &second);
    assert_runtime_scope_ownership(&first, &second);
}

// ---- the typed published head commit --------------------------------------

fn published_prepared(run: &ArmRun) -> &PublishedPrepareRecord {
    run.witness
        .prepared
        .as_ref()
        .unwrap_or_else(|| panic!("{}: no prepared published head", run.arm.label()))
}

fn published_commit(run: &ArmRun) -> &PublishedCommitRecord {
    let commits = &run.witness.commits;
    assert_eq!(
        commits.len(),
        1,
        "{}: the typed commit body must run exactly once, saw {}",
        run.arm.label(),
        commits.len()
    );
    &commits[0]
}

fn published_producer(run: &ArmRun) -> &ProducerRecord {
    let producers = &run.witness.producers;
    assert_eq!(
        producers.len(),
        1,
        "{}: the typed producer must run exactly once, saw {}",
        run.arm.label(),
        producers.len()
    );
    &producers[0]
}

fn producer_entries(run: &ArmRun) -> Vec<&EntryRecord> {
    run.witness
        .entries
        .iter()
        .filter(|entry| entry.label == PRODUCER_ENTRY_LABEL)
        .collect()
}

fn commit_entries(run: &ArmRun) -> Vec<&EntryRecord> {
    run.witness
        .entries
        .iter()
        .filter(|entry| entry.label == COMMIT_ENTRY_LABEL)
        .collect()
}

fn heap_query<T>(
    vm: &mut Vm,
    body: impl FnOnce(&mut heap::LuaHeap) -> Result<T, heap::HeapError>,
) -> T {
    let mut host = vm.host_context();
    heap::with_heap(&mut host, body).unwrap_or_else(|error| panic!("heap query: {error:?}"))
}

/// One read-only view of the current published head for the typed commit
/// body. The heap owner performs the reads; this wrapper only re-shapes its
/// test-only view. It holds a `Weak` callable identity, never a strong `Arc`
/// or root.
#[derive(Clone)]
struct PublishedTypedCommitSnapshot {
    userdata: UserdataRef,
    raw_token: u64,
    metatable: TableRef,
    receiver: TableRef,
    closure: heap::ClosureRef,
    callable: std::sync::Weak<vm::CallableValue>,
    published_count: usize,
    queue_len: usize,
}

fn snapshot_published_typed_table_call(
    host: &mut vm::HostContext,
) -> Result<PublishedTypedCommitSnapshot, heap::HeapError> {
    heap::with_heap(host, |heap| {
        let call = heap.test_snapshot_published_typed_call()?;
        Ok(PublishedTypedCommitSnapshot {
            userdata: call.userdata,
            raw_token: call.raw_token,
            metatable: call.metatable,
            receiver: call.receiver,
            closure: call.closure,
            callable: call.callable,
            published_count: call.published_count,
            queue_len: call.queue_len,
        })
    })
}

/// One read-only source pass over the published head. The heap owner performs
/// every identity read and the before/after comparison; this wrapper only
/// re-shapes its test-only view. The callable stays a `Weak`.
#[derive(Clone)]
struct PublishedTypedHeadRead {
    userdata: UserdataRef,
    raw_token: u64,
    metatable: TableRef,
    receiver: TableRef,
    closure: heap::ClosureRef,
    callable: std::sync::Weak<vm::CallableValue>,
    published_count: usize,
    queue_len: usize,
    queue_before: Vec<(u64, u32, u32)>,
    queue_after: Vec<(u64, u32, u32)>,
    top_frame_before: Option<heap::RootFrameId>,
    top_frame_after: Option<heap::RootFrameId>,
    pending_before: [bool; 2],
    pending_after: [bool; 2],
    settled_before: [bool; 2],
    settled_after: [bool; 2],
}

fn read_published_typed_call_head(
    host: &mut vm::HostContext,
    expected_receiver: &Value,
    expected_payload: Option<UserdataRef>,
    expected_token: u64,
    expected_successor: UserdataRef,
    expected_top: heap::RootFrameId,
) -> Result<PublishedTypedHeadRead, heap::HeapError> {
    heap::with_heap(host, |heap| {
        let read = heap.test_read_published_typed_call_head(
            expected_receiver,
            expected_payload,
            expected_token,
            expected_successor,
            expected_top,
        )?;
        Ok(PublishedTypedHeadRead {
            userdata: read.call.userdata,
            raw_token: read.call.raw_token,
            metatable: read.call.metatable,
            receiver: read.call.receiver,
            closure: read.call.closure,
            callable: read.call.callable,
            published_count: read.call.published_count,
            queue_len: read.call.queue_len,
            queue_before: read.queue_before,
            queue_after: read.queue_after,
            top_frame_before: read.top_frame_before,
            top_frame_after: read.top_frame_after,
            pending_before: read.pending_before,
            pending_after: read.pending_after,
            settled_before: read.settled_before,
            settled_after: read.settled_after,
        })
    })
}

/// The typed commit body's exclusive entry. The heap owner reads the live
/// published head and performs every identity check in the one borrow that
/// pops, roots, and settles, before the first write. No external queue record,
/// generation, or root index crosses this seam.
fn commit_published_typed_table_call(
    host: &mut vm::HostContext,
    expected_callable: &Arc<vm::CallableValue>,
    expected_payload: Option<UserdataRef>,
    expected_metatable: TableRef,
    expected_receiver: &Value,
    expected_token: u64,
) -> Result<(heap::RootFrameId, heap::TakenPendingFinalizer), heap::HeapError> {
    heap::with_heap(host, |heap| {
        heap.test_commit_published_typed_table_call(
            expected_callable,
            expected_payload,
            expected_metatable,
            expected_receiver,
            expected_token,
        )
    })
}

/// Test-only bridge to the heap-owned guest callable field admission. The VM
/// operand values are the actual guest values; the heap owner resolves the
/// prototype shape and performs its private read-before-write transaction.
fn install_guest_callable_into_raw_call(
    vm: &mut Vm,
    owner: &Value,
    target: &Value,
) -> Result<(), heap::HeapError> {
    heap::LuaHeap::test_admit_guest_callable_to_raw_call(vm, owner, target).map(|_| ())
}

fn published_pending(run: &mut ArmRun) -> Vec<(u64, u32, u32)> {
    heap_query(&mut run.vm, |heap| Ok(heap.pending_finalizer_records()))
}

fn published_count(run: &mut ArmRun) -> usize {
    let mut host = run.vm.host_context();
    heap::pending_finalizer_published_count(&mut host)
        .unwrap_or_else(|error| panic!("published count: {error:?}"))
}

fn published_pending_flag(run: &mut ArmRun, userdata: UserdataRef) -> bool {
    heap_query(&mut run.vm, |heap| heap.slot_pending_finalizer(userdata))
}

fn published_settled_flag(run: &mut ArmRun, userdata: UserdataRef) -> bool {
    heap_query(&mut run.vm, |heap| heap.slot_finalizer_settled(userdata))
}

fn published_slot_generation(run: &mut ArmRun, userdata: UserdataRef) -> u32 {
    heap_query(&mut run.vm, |heap| {
        heap.slot_generation(userdata.slot())
            .ok_or(heap::HeapError::StaleHandle)
    })
}

/// The two static Calls owned by the parent and its one dynamic
/// `CallValue(2)`, in compiled program order: read-only seam, exclusive
/// commit, then the parent's own call.
fn published_parent_sites(run: &ArmRun) -> (usize, usize, usize) {
    let parent = run
        .mark("parent-enter")
        .last_prototype
        .unwrap_or_else(|| panic!("{}: the parent frame has no prototype", run.arm.label()));
    let admits: Vec<_> = run
        .host_call_sites
        .iter()
        .filter(|(_, _, owner)| *owner == Some(parent))
        .collect();
    assert_eq!(
        admits.len(),
        1,
        "{}: expected one admit Call owned by the parent, saw {:?}",
        run.arm.label(),
        run.host_call_sites
    );
    assert_eq!(admits[0].1, 3, "{}: the seam Call arity", run.arm.label());
    let commits: Vec<_> = run
        .commit_call_sites
        .iter()
        .filter(|(_, _, owner)| *owner == Some(parent))
        .collect();
    assert_eq!(
        commits.len(),
        1,
        "{}: expected one commit Call owned by the parent, saw {:?}",
        run.arm.label(),
        run.commit_call_sites
    );
    assert_eq!(
        commits[0].1,
        3,
        "{}: the commit Call arity",
        run.arm.label()
    );
    let calls: Vec<_> = run
        .call_sites
        .iter()
        .filter(|site| site.owner == Some(parent) && site.argc == CALL_ARGC)
        .collect();
    assert_eq!(
        calls.len(),
        1,
        "{}: expected one CallValue(2) owned by the parent, saw {:?}",
        run.arm.label(),
        run.call_sites
    );
    assert!(
        admits[0].0 < commits[0].0,
        "{}: the read-only seam must precede the exclusive commit",
        run.arm.label()
    );
    assert!(
        commits[0].0 < calls[0].ip,
        "{}: the exclusive commit must precede CallValue(2)",
        run.arm.label()
    );
    (admits[0].0, commits[0].0, calls[0].ip)
}

fn assert_published_import(run: &ArmRun) {
    let label = run.arm.label();
    let catalog = transport_catalog();
    let function = catalog
        .function(COMMIT_IMPORT)
        .unwrap_or_else(|| panic!("{label}: the catalog is missing '{COMMIT_IMPORT}'"));
    assert_eq!(
        function,
        &commit_import_schema(),
        "{label}: the live catalog install"
    );
    let expected = HostImportSchema::from_function(catalog, function);
    assert_eq!(
        expected.fingerprint,
        catalog.fingerprint(),
        "{label}: the catalog fingerprint"
    );
    let index = run
        .host_imports
        .iter()
        .position(|import| import.name == COMMIT_IMPORT)
        .unwrap_or_else(|| panic!("{label}: '{COMMIT_IMPORT}' is not a compiled import"));
    assert_eq!(
        run.host_imports[index].arity, 3,
        "{label}: the commit import arity"
    );
    assert_eq!(
        run.host_imports[index].return_type,
        vm::ValueType::Bool,
        "{label}: the commit import's coarse return"
    );
    let schema = run.host_schemas[index]
        .as_ref()
        .unwrap_or_else(|| panic!("{label}: the commit import must carry its full schema"));
    assert_eq!(
        schema, &expected,
        "{label}: the program must carry the complete catalog schema and fingerprint"
    );
    let patch = run
        .patch
        .commit
        .as_ref()
        .unwrap_or_else(|| panic!("{label}: the commit seam was not inserted"));
    assert_eq!(
        patch.import_index, index as u16,
        "{label}: the commit import slot"
    );
    assert_eq!(
        patch.resolution.name, COMMIT_IMPORT,
        "{label}: the resolved commit name"
    );
    assert_eq!(
        patch.resolution.passing,
        [
            HostParamPassing::Borrow,
            HostParamPassing::Value,
            HostParamPassing::Borrow,
        ],
        "{label}: the commit resolution's passing modes"
    );
    assert_eq!(
        patch.resolution.host_params,
        [
            typed_host_callable_schema(),
            HostTypeSchema::Array(Box::new(HostTypeSchema::Int)),
            HostTypeSchema::Resource(LuaUserdataResource::key()),
        ],
        "{label}: the commit resolution's concrete operand schemas"
    );
    assert_eq!(
        patch.resolution.host_return_type,
        HostTypeSchema::Bool,
        "{label}: the commit resolution's return"
    );
    assert_eq!(
        patch.resolution.fingerprint,
        catalog.fingerprint(),
        "{label}: the commit resolution fingerprint"
    );
    assert_eq!(
        Some(patch.typed_local),
        run.patch.typed_local,
        "{label}: the commit and the call must share the typed local"
    );
    let call = run
        .patch
        .call
        .as_ref()
        .unwrap_or_else(|| panic!("{label}: the dynamic call was not patched"));
    assert_eq!(
        call.typed_callee, patch.typed_local,
        "{label}: CallValue must use the typed local the commit reviewed"
    );
    assert_eq!(
        run.patch.parent_parameters.len(),
        PUBLISHED_PARENT_PARAMS,
        "{label}: the published parent parameter list"
    );
    let gate = run
        .patch
        .gate
        .as_ref()
        .unwrap_or_else(|| panic!("{label}: the read-only seam was not inserted"));
    assert_eq!(
        gate.resolution.passing,
        [
            HostParamPassing::Borrow,
            HostParamPassing::Value,
            HostParamPassing::Borrow,
        ],
        "{label}: the read-only seam's Borrow/Value/Borrow transport"
    );
    assert!(
        gate.result_slot != patch.result_slot,
        "{label}: the two static results must be distinct locals"
    );
    assert!(
        run.patch.sentinel_bind.is_some() && run.patch.sentinel_wrong.is_some(),
        "{label}: the plugin must add the chunk-level bind and wrong-token calls"
    );
}

fn assert_published_positive(run: &mut ArmRun) {
    run.assert_fuel();
    assert_published_import(run);
    assert!(
        matches!(run.status, Ok(VmStatus::Halted)),
        "published-commit: the parent must run to a normal halt, got {:?}",
        run.status
    );
    assert_eq!(
        run.labels(),
        ["parent-enter", "target-enter", "parent-after", "done"],
        "published-commit: the child must run between the parent's markers"
    );
    let values = run.exit().unwrap_or_else(|| {
        panic!(
            "published-commit: expected a chunk completion, got {:?}",
            run.status
        )
    });
    assert_eq!(
        values.len(),
        3,
        "published-commit: [true] plus the parent result and receiver"
    );
    assert_eq!(values[0], Value::Bool(true));
    assert_eq!(
        values[1],
        Value::Bool(true),
        "published-commit: the parent result"
    );
    let receiver_value = &values[2];

    let pick = run.pick().clone();
    assert!(
        pick.chosen_is_target,
        "published-commit: note must return the exact written target Arc"
    );
    let prepared = published_prepared(run).clone();
    let prepared_closure = prepared
        .call_closure
        .expect("published-commit: bind must install the raw __call");
    assert_eq!(
        prepared.published_count, 2,
        "published-commit: both selected userdata are a published prefix"
    );
    assert_eq!(
        prepared.queue_len, 2,
        "published-commit: both records are queued"
    );
    assert_ne!(
        prepared.userdata, prepared.successor,
        "published-commit: A and B are distinct objects"
    );
    assert!(
        same_array(
            receiver_value,
            &run.witness.issued_table.clone().expect("issued receiver")
        ),
        "published-commit: the chunk returned the exact issued receiver carrier"
    );

    let commit = published_commit(run).clone();
    assert!(
        same_callable(&commit.callable, &pick.chosen),
        "published-commit: the commit operand must be the exact note Arc"
    );
    assert!(
        same_array(&commit.receiver, receiver_value),
        "published-commit: the commit receiver must be the exact issued carrier"
    );
    assert_eq!(
        commit.receiver_class,
        GuestClass::Table(prepared.receiver),
        "published-commit: the live heap must classify the receiver as the action receiver"
    );
    assert_eq!(
        commit.token_raw, prepared.userdata_raw,
        "published-commit: the commit receives A's real token"
    );
    assert_eq!(
        commit.payload_parts,
        Some((
            prepared.userdata.domain(),
            prepared.userdata.slot(),
            prepared.userdata.generation(),
        )),
        "published-commit: the Core payload must name A"
    );
    assert!(
        commit.payload_is_head,
        "published-commit: the typed payload owner must be the published head"
    );
    assert!(
        commit.weak_matches_callable,
        "published-commit: the snapshot Weak must name the operand Arc"
    );
    assert_eq!(
        commit.head_parts,
        (
            prepared.userdata.domain(),
            prepared.userdata.slot(),
            prepared.userdata.generation(),
        ),
        "published-commit: the snapshot head is A"
    );
    assert_eq!(
        commit.head_raw, prepared.userdata_raw,
        "published-commit: the snapshot head token is A's real token"
    );
    assert_eq!(
        commit.metatable_parts,
        (
            prepared.finalizer_metatable.domain(),
            prepared.finalizer_metatable.slot(),
            prepared.finalizer_metatable.generation(),
        ),
        "published-commit: the snapshot metatable is A's finalizer metatable"
    );
    assert_eq!(
        commit.published_before, 2,
        "published-commit: the commit starts from a two-record prefix"
    );
    assert_eq!(
        commit.queue_before, 2,
        "published-commit: the commit starts from a two-record queue"
    );
    assert_eq!(
        commit.action_receiver,
        (
            prepared.receiver.domain(),
            prepared.receiver.slot(),
            prepared.receiver.generation(),
        ),
        "published-commit: the raw __gc action names the issued receiver"
    );
    assert_eq!(
        commit.action_closure,
        (
            prepared_closure.domain(),
            prepared_closure.slot(),
            prepared_closure.generation(),
        ),
        "published-commit: the raw __call action names the installed closure"
    );
    let (frame, roots) = commit
        .outcome
        .as_ref()
        .unwrap_or_else(|error| panic!("published-commit: the commit refused: {error}"));
    assert_eq!(*roots, 3, "published-commit: three roots written");
    assert_eq!(
        commit.taken_userdata,
        Some(prepared.userdata),
        "published-commit: the committed object is A"
    );

    let (admit_ip, commit_ip, call_ip) = published_parent_sites(run);
    let parent = run.mark("parent-enter");
    let entries = commit_entries(run);
    assert_eq!(
        entries.len(),
        1,
        "published-commit: exactly one raw commit entry"
    );
    let entry = entries[0];
    assert_eq!(
        (entry.frame_len, entry.prototype_id, entry.parent_prototype),
        (
            parent.frame_len,
            parent.last_prototype,
            parent.parent_prototype
        ),
        "published-commit: the commit body must run in the active parent frame"
    );

    // The child entry carries the same two actual slots, and the parent
    // continuation re-borrows the same token after one script call.
    let target_id = callable_id(&pick.chosen);
    let callee = run
        .witness
        .marks
        .iter()
        .find_map(|mark| mark.callee.as_ref())
        .expect("published-commit: the child frame must be observed");
    assert_eq!(callee.prototype_id, target_id);
    let child_receiver = callee
        .parameter_values
        .first()
        .cloned()
        .flatten()
        .expect("published-commit: the child receiver formal slot must be populated");
    assert!(
        same_array(&child_receiver, &commit.receiver),
        "published-commit: the child receiver slot must hold the exact commit operand Arc"
    );
    assert_eq!(
        callee.parameter_values.get(1).cloned().flatten(),
        Some(Value::Int(commit.token_raw)),
        "published-commit: the child token slot reuses the commit operand"
    );
    assert_eq!(
        callee.token.parts,
        (
            prepared.userdata.domain(),
            prepared.userdata.slot(),
            prepared.userdata.generation(),
        ),
        "published-commit: the child formal token borrows A"
    );
    assert_eq!(
        callee.token.rebound_parts, callee.token.parts,
        "published-commit: the heap still names the same payload"
    );
    assert!(
        callee.table_token_refused,
        "published-commit: A's token is not a lua.table resource"
    );
    let continuation = run
        .mark("parent-after")
        .continuation
        .as_ref()
        .expect("published-commit: the parent continuation must re-borrow the token");
    assert_eq!(
        continuation.parts, callee.token.parts,
        "published-commit: the continuation reads the same A token"
    );
    let callback_entry = run
        .witness
        .entries
        .iter()
        .find(|entry| entry.label == "target-enter")
        .expect("published-commit: the child entry must be recorded first");
    let child_return_ip = run
        .mark("target-enter")
        .last_return_ip
        .expect("published-commit: the child frame must resume bytecode");
    assert_eq!(
        callback_entry.return_ip,
        Some(child_return_ip),
        "published-commit: the child entry resume address is the real call"
    );
    let call_site = run
        .call_sites
        .iter()
        .find(|site| site.owner == parent.last_prototype && site.argc == CALL_ARGC)
        .expect("published-commit: the parent CallValue(2)");
    assert_eq!(
        child_return_ip, call_site.return_ip,
        "published-commit: the child resumes at the compiled call's return address"
    );

    // After the pop, B is still the true published head with its own exact
    // identity and the same raw action.
    assert_eq!(
        published_pending(run),
        vec![(
            u64::try_from(prepared.successor_raw).expect("successor token"),
            prepared.successor.slot(),
            prepared.successor.generation(),
        )],
        "published-commit: only B remains queued"
    );
    assert_eq!(published_count(run), 1);
    assert!(
        published_pending_flag(run, prepared.successor)
            && !published_settled_flag(run, prepared.successor),
        "published-commit: B is still pending and not settled"
    );
    assert!(
        !published_pending_flag(run, prepared.userdata)
            && published_settled_flag(run, prepared.userdata),
        "published-commit: A is settled exactly once"
    );
    assert_eq!(
        heap_query(&mut run.vm, |heap| heap
            .userdata_metatable(prepared.successor)),
        Some(prepared.finalizer_metatable),
        "published-commit: B keeps the same finalizer metatable"
    );
    let next = {
        let mut host = run.vm.host_context();
        snapshot_published_typed_table_call(&mut host)
            .unwrap_or_else(|error| panic!("published-commit: B snapshot: {error:?}"))
    };
    assert_eq!(
        next.userdata, prepared.successor,
        "published-commit: B is the real published front"
    );
    assert_eq!(
        next.raw_token,
        u64::try_from(prepared.successor_raw).expect("successor token")
    );
    assert_eq!(next.receiver, prepared.receiver);
    assert_eq!(next.closure, prepared_closure);
    eprintln!(
        "LUA_TYPED_RESOURCE arm=published-commit admit_ip={admit_ip} commit_ip={commit_ip} call_ip={call_ip} head=A successor=B roots={roots}"
    );

    // The three written roots of the commit frame, and only those, are
    // present before withdrawal.
    let mut host = run.vm.host_context();
    assert_eq!(
        heap::rooted_userdata(&mut host, *frame, 0),
        Ok(Some(prepared.userdata))
    );
    assert_eq!(
        heap::rooted_closure(&mut host, *frame, 1),
        Ok(Some(prepared_closure))
    );
    assert_eq!(
        heap::rooted_table(&mut host, *frame, 2),
        Ok(Some(prepared.receiver))
    );
    let prepare_frame = prepared.root_frame;
    assert_eq!(
        heap::rooted_table(&mut host, prepare_frame, 1),
        Ok(Some(prepared.receiver))
    );
    assert_eq!(
        heap::rooted_table(&mut host, prepare_frame, 2),
        Ok(Some(prepared.sentinel))
    );
    assert_eq!(
        heap::rooted_table(&mut host, prepare_frame, 0),
        Ok(Some(prepared.weak_table))
    );
    for slot in [0u32, 1, 2] {
        heap::with_heap(&mut host, |heap| heap.clear_root(*frame, slot))
            .unwrap_or_else(|error| panic!("published-commit: clear root: {error:?}"));
    }
    // Only the commit's three roots are withdrawn: the independent prepare
    // frame roots still name their exact tables.
    assert_eq!(
        heap::rooted_table(&mut host, prepare_frame, 1),
        Ok(Some(prepared.receiver))
    );
    assert_eq!(
        heap::rooted_table(&mut host, prepare_frame, 2),
        Ok(Some(prepared.sentinel))
    );
    assert_eq!(heap::rooted_userdata(&mut host, *frame, 0), Ok(None));
    let _ = heap::collect(&mut host).expect("published-commit: real collection");
    let _ = host;

    // A's exact generation and token retire before reset; B and the
    // independent root survive.
    assert!(
        !published_userdata_alive(run, prepared.userdata),
        "published-commit: A is collected after its three roots are withdrawn"
    );
    assert!(
        !published_token_open(run, prepared.userdata_raw),
        "published-commit: A's Core token is retired"
    );
    assert_eq!(
        published_slot_generation(run, prepared.userdata),
        prepared.userdata.generation() + 1,
        "published-commit: A's slot advances exactly one generation"
    );
    assert!(
        published_userdata_alive(run, prepared.successor),
        "published-commit: the queued B stays alive"
    );
    assert!(
        published_token_open(run, prepared.successor_raw),
        "published-commit: B's typed token stays open"
    );
    assert_eq!(
        published_slot_generation(run, prepared.successor),
        prepared.successor.generation(),
        "published-commit: B does not advance its generation"
    );
    assert_eq!(
        published_pending(run),
        vec![(
            u64::try_from(prepared.successor_raw).expect("successor token"),
            prepared.successor.slot(),
            prepared.successor.generation(),
        )],
        "published-commit: the collection does not consume B"
    );
    assert_eq!(published_count(run), 1);
    assert!(
        heap_query(&mut run.vm, |heap| Ok(heap.alive(prepared.sentinel))),
        "published-commit: the independent sentinel root stays readable"
    );
    assert!(
        heap_query(&mut run.vm, |heap| Ok(heap.alive(prepared.receiver))),
        "published-commit: the receiver stays readable through its independent root and B"
    );
    assert!(
        heap_query(&mut run.vm, |heap| Ok(heap.alive(prepared.weak_table))),
        "published-commit: the still-rooted weak witness table stays"
    );
}

// ---- the published head producer ------------------------------------------

/// The compiled producer import must carry the exact catalog schema and
/// fingerprint: only the issued receiver carrier and the actual typed token,
/// with the exact callable return Core validates. No candidate callable
/// parameter is present.
fn assert_producer_import(run: &ArmRun) {
    let catalog = transport_catalog();
    let function = catalog.function(PRODUCER_IMPORT).unwrap_or_else(|| {
        panic!(
            "{}: the catalog is missing '{PRODUCER_IMPORT}'",
            run.arm.label()
        )
    });
    assert_eq!(
        function,
        &produce_import_schema(),
        "{}: the live producer catalog install",
        run.arm.label()
    );
    let expected = HostImportSchema::from_function(catalog, function);
    assert_eq!(
        expected.fingerprint,
        catalog.fingerprint(),
        "{}: the producer catalog fingerprint",
        run.arm.label()
    );
    let index = run
        .host_imports
        .iter()
        .position(|import| import.name == PRODUCER_IMPORT)
        .unwrap_or_else(|| {
            panic!(
                "{}: '{PRODUCER_IMPORT}' is not a compiled import",
                run.arm.label()
            )
        });
    assert_eq!(
        run.patch.producer_import,
        Some(index as u16),
        "{}: the compiled producer import slot",
        run.arm.label()
    );
    assert_eq!(
        run.host_imports[index].arity,
        2,
        "{}: the producer import arity",
        run.arm.label()
    );
    assert_eq!(
        run.host_imports[index].return_type,
        vm::ValueType::Callable,
        "{}: the producer's coarse return",
        run.arm.label()
    );
    let schema = run.host_schemas[index].as_ref().unwrap_or_else(|| {
        panic!(
            "{}: the producer import must carry its full schema",
            run.arm.label()
        )
    });
    assert_eq!(
        schema,
        &expected,
        "{}: the program must carry the complete producer schema and fingerprint",
        run.arm.label()
    );
    assert_eq!(
        schema.return_type,
        typed_host_callable_schema(),
        "{}: the producer return is the exact typed callable schema",
        run.arm.label()
    );
    assert_eq!(
        schema.params[0].schema,
        HostTypeSchema::Array(Box::new(HostTypeSchema::Int)),
        "{}: the producer receiver is the issued Array<Int> carrier",
        run.arm.label()
    );
    assert_eq!(
        schema.params[1].schema,
        HostTypeSchema::Resource(LuaUserdataResource::key()),
        "{}: the producer token is the concrete lua.userdata key",
        run.arm.label()
    );
    assert_eq!(
        schema.params[1].passing,
        HostParamPassing::Borrow,
        "{}: the producer token is a borrowed actual operand",
        run.arm.label()
    );
    assert!(
        schema.params.iter().all(|param| {
            !matches!(
                param.schema,
                HostTypeSchema::Callable { .. } | HostTypeSchema::Unknown
            )
        }),
        "{}: the producer has no candidate callable or Unknown parameter",
        run.arm.label()
    );
}

/// The one compiled `produce` Call, the one `admit` Call, the one `commit`
/// Call and the one `CallValue(2)` owned by the active parent, in program
/// order.
fn producer_parent_sites(run: &ArmRun) -> (usize, usize, usize, usize) {
    let parent = run
        .mark("parent-enter")
        .last_prototype
        .unwrap_or_else(|| panic!("{}: the parent frame has no prototype", run.arm.label()));
    let produces: Vec<_> = run
        .producer_call_sites
        .iter()
        .filter(|(_, _, owner)| *owner == Some(parent))
        .collect();
    assert_eq!(
        produces.len(),
        1,
        "{}: expected one producer Call owned by the parent, saw {:?}",
        run.arm.label(),
        run.producer_call_sites
    );
    assert_eq!(
        produces[0].1,
        2,
        "{}: the producer Call arity",
        run.arm.label()
    );
    let admits: Vec<_> = run
        .host_call_sites
        .iter()
        .filter(|(_, _, owner)| *owner == Some(parent))
        .collect();
    assert_eq!(
        admits.len(),
        1,
        "{}: expected one admit Call owned by the parent, saw {:?}",
        run.arm.label(),
        run.host_call_sites
    );
    assert_eq!(admits[0].1, 3, "{}: the seam Call arity", run.arm.label());
    let commits: Vec<_> = run
        .commit_call_sites
        .iter()
        .filter(|(_, _, owner)| *owner == Some(parent))
        .collect();
    assert_eq!(
        commits.len(),
        1,
        "{}: expected one commit Call owned by the parent, saw {:?}",
        run.arm.label(),
        run.commit_call_sites
    );
    assert_eq!(
        commits[0].1,
        3,
        "{}: the commit Call arity",
        run.arm.label()
    );
    let calls: Vec<_> = run
        .call_sites
        .iter()
        .filter(|site| site.owner == Some(parent) && site.argc == CALL_ARGC)
        .collect();
    assert_eq!(
        calls.len(),
        1,
        "{}: expected one CallValue(2) owned by the parent, saw {:?}",
        run.arm.label(),
        run.call_sites
    );
    assert!(
        produces[0].0 < admits[0].0,
        "{}: the producer must run before the read-only seam",
        run.arm.label()
    );
    assert!(
        admits[0].0 < commits[0].0,
        "{}: the read-only seam must precede the exclusive commit",
        run.arm.label()
    );
    assert!(
        commits[0].0 < calls[0].ip,
        "{}: the exclusive commit must precede CallValue(2)",
        run.arm.label()
    );
    (produces[0].0, admits[0].0, commits[0].0, calls[0].ip)
}

/// The read-phase evidence: the producer's actual heap identities, its
/// `Weak` against the current raw `__call`, the exact recipient/queue top
/// frame, the unchanged pending/settled state, and the returned prototype's
/// exact declared schema. It creates at most one temporary strong `Arc` from
/// the heap slot for the `Weak` comparison and drops it before returning.
fn assert_producer_read_phase(
    run: &mut ArmRun,
    live_edge_matches_producer: bool,
    expect_userdata_schema: bool,
) -> (ProducerRecord, u32) {
    assert_producer_import(run);
    let prepared = published_prepared(run).clone();
    let producer = published_producer(run).clone();
    let target_id = callable_id(&run.pick().chosen);
    assert!(
        run.pick().mode >= 8,
        "{}: the old fixture selection must still run",
        run.arm.label()
    );
    assert_eq!(
        producer.userdata,
        prepared.userdata,
        "{}: the producer read A as the head",
        run.arm.label()
    );
    assert_eq!(
        producer.raw_token,
        prepared.userdata_raw,
        "{}: the producer read A's real token",
        run.arm.label()
    );
    assert_eq!(
        producer.metatable,
        prepared.finalizer_metatable,
        "{}: the producer read A's finalizer metatable",
        run.arm.label()
    );
    assert_eq!(
        producer.receiver,
        prepared.receiver,
        "{}: the producer followed raw __gc to the issued receiver",
        run.arm.label()
    );
    assert_eq!(
        producer.receiver_class,
        GuestClass::Table(prepared.receiver),
        "{}: the receiver operand is the issued carrier",
        run.arm.label()
    );
    assert!(
        producer.payload_is_head,
        "{}: the actual typed payload names the head",
        run.arm.label()
    );
    assert!(
        producer.token_still_open,
        "{}: the read pass did not consume the token",
        run.arm.label()
    );
    assert_eq!(
        producer.published_count,
        2,
        "{}: both selected owners remain published",
        run.arm.label()
    );
    assert_eq!(
        producer.queue_len,
        2,
        "{}: both records remain queued",
        run.arm.label()
    );
    let expected_queue = vec![
        (
            u64::try_from(prepared.userdata_raw).expect("A token"),
            prepared.userdata.slot(),
            prepared.userdata.generation(),
        ),
        (
            u64::try_from(prepared.successor_raw).expect("B token"),
            prepared.successor.slot(),
            prepared.successor.generation(),
        ),
    ];
    assert_eq!(
        producer.queue_before,
        expected_queue,
        "{}: the producer starts from A then B",
        run.arm.label()
    );
    assert_eq!(
        producer.queue_after,
        expected_queue,
        "{}: the read pass leaves A then B queued",
        run.arm.label()
    );
    assert_eq!(
        producer.top_frame_before,
        Some(prepared.root_frame),
        "{}: the producer starts on the prepared root frame",
        run.arm.label()
    );
    assert_eq!(
        producer.top_frame_after,
        Some(prepared.root_frame),
        "{}: the read pass opens no root frame",
        run.arm.label()
    );
    assert_eq!(
        producer.pending_before,
        [true, true],
        "{}: both owners are pending before the read",
        run.arm.label()
    );
    assert_eq!(
        producer.pending_after,
        [true, true],
        "{}: both owners stay pending after the read",
        run.arm.label()
    );
    assert_eq!(
        producer.settled_before,
        [false, false],
        "{}: neither owner is settled before the read",
        run.arm.label()
    );
    assert_eq!(
        producer.settled_after,
        [false, false],
        "{}: neither owner is settled by the read",
        run.arm.label()
    );

    // The stored Arc must be the exact one the producer read. The live raw
    // edge is used when the arm did not replace it after the read; the
    // post-read replacement control compares against the exact closure the
    // read recorded instead. The upgrade below is only for this comparison;
    // it is dropped at once.
    let read_closure = if live_edge_matches_producer {
        let current_closure = heap_query(&mut run.vm, |heap| {
            heap.named_closure_edge(prepared.receiver_metatable, "__call")
        });
        assert_eq!(
            producer.closure,
            current_closure,
            "{}: the producer read the current raw __call generation",
            run.arm.label()
        );
        current_closure
    } else {
        let recorded = prepared
            .call_closure
            .expect("the bind import must install the raw __call");
        assert_eq!(
            producer.closure,
            recorded,
            "{}: the producer read the pre-replacement raw __call",
            run.arm.label()
        );
        recorded
    };
    let read_arc = {
        let mut host = run.vm.host_context();
        heap::with_heap(&mut host, |heap| heap.closure_callable(read_closure))
            .unwrap_or_else(|error| panic!("{}: raw __call Arc read: {error:?}", run.arm.label()))
    };
    assert!(
        std::sync::Weak::ptr_eq(&producer.callable, &Arc::downgrade(&read_arc)),
        "{}: the producer Weak names the raw __call Arc",
        run.arm.label()
    );
    assert_eq!(
        producer.returned_prototype,
        read_arc.prototype_id,
        "{}: the returned prototype is the raw __call instance",
        run.arm.label()
    );
    drop(read_arc);

    assert_eq!(
        producer.parent_candidate_prototype,
        Some(target_id),
        "{}: the parent's own callable slot is the old fixture selection",
        run.arm.label()
    );

    let prototype = run.prototype(producer.returned_prototype);
    assert_eq!(
        prototype.arity,
        CALL_ARGC,
        "{}: the producer result is a two-parameter Lua function",
        run.arm.label()
    );
    if expect_userdata_schema {
        assert_eq!(
            prototype.schema,
            Some(userdata_callable_schema()),
            "{}: Core checked the exact declared callable schema",
            run.arm.label()
        );
    }
    assert!(
        matches!(prototype.target, vm::CallableTarget::ScriptFunction(_)),
        "{}: the producer result is a script function",
        run.arm.label()
    );
    assert!(
        prototype.capture_slots.is_empty() && prototype.capture_source_slots.is_empty(),
        "{}: the producer result is zero-capture",
        run.arm.label()
    );

    let entries = producer_entries(run);
    assert_eq!(
        entries.len(),
        1,
        "{}: exactly one raw producer entry",
        run.arm.label()
    );
    let parent = run.mark("parent-enter");
    let entry = entries[0];
    assert_eq!(
        (entry.frame_len, entry.prototype_id, entry.parent_prototype),
        (
            parent.frame_len,
            parent.last_prototype,
            parent.parent_prototype
        ),
        "{}: the producer runs in the active parent frame",
        run.arm.label()
    );
    if !run.witness.commits.is_empty() {
        let producer_index = run
            .witness
            .entries
            .iter()
            .position(|entry| entry.label == PRODUCER_ENTRY_LABEL)
            .expect("producer entry");
        let admit_index = run
            .witness
            .entries
            .iter()
            .position(|entry| entry.label == PRECOMMIT_ENTRY_LABEL)
            .expect("admit entry");
        let commit_index = run
            .witness
            .entries
            .iter()
            .position(|entry| entry.label == COMMIT_ENTRY_LABEL)
            .expect("commit entry");
        assert!(
            producer_index < admit_index && admit_index < commit_index,
            "{}: the producer, seam and commit entries run in that order",
            run.arm.label()
        );
    }
    (producer, target_id)
}

/// The commit operand must be the exact Arc the producer read. The `Weak`
/// comparison creates a temporary `Weak` from the commit operand; it does not
/// clone that strong `Arc`.
fn assert_producer_commit_identity(run: &ArmRun) {
    let producer = published_producer(run);
    let commit = published_commit(run);
    assert_eq!(
        callable_id(&commit.callable),
        producer.returned_prototype,
        "{}: the commit operand is the producer's prototype",
        run.arm.label()
    );
    let Value::Callable(commit_arc) = &commit.callable else {
        panic!("{}: the commit operand is not callable", run.arm.label());
    };
    assert!(
        std::sync::Weak::ptr_eq(&producer.callable, &Arc::downgrade(commit_arc)),
        "{}: the commit operand is the exact Arc the producer read",
        run.arm.label()
    );
    assert!(
        commit.weak_matches_callable,
        "{}: the commit's own snapshot Weak names the operand Arc",
        run.arm.label()
    );
}

/// The successful replacement transaction: the real child frame, the parent
/// continuation, B as the remaining head, and retirement of exactly the
/// commit's three roots.
fn assert_producer_transaction(run: &mut ArmRun, child_label: &str) {
    run.assert_fuel();
    assert_published_import(run);
    assert!(
        matches!(run.status, Ok(VmStatus::Halted)),
        "{}: the parent must halt normally, got {:?}",
        run.arm.label(),
        run.status
    );
    assert_eq!(
        run.labels(),
        ["parent-enter", child_label, "parent-after", "done"],
        "{}: the actual replacement child must run between the parent markers",
        run.arm.label()
    );
    let values = run.exit().unwrap_or_else(|| {
        panic!(
            "{}: expected a chunk completion, got {:?}",
            run.arm.label(),
            run.status
        )
    });
    assert_eq!(
        values.len(),
        3,
        "{}: [true] plus the parent result and receiver",
        run.arm.label()
    );
    assert_eq!(values[0], Value::Bool(true));
    assert_eq!(values[1], Value::Bool(true));
    assert!(
        same_array(
            &values[2],
            &run.witness
                .issued_table
                .clone()
                .expect("the issued receiver carrier")
        ),
        "{}: the chunk returned the exact issued receiver carrier",
        run.arm.label()
    );

    let prepared = published_prepared(run).clone();
    let producer = published_producer(run).clone();
    let commit = published_commit(run).clone();
    assert_eq!(
        callable_id(&commit.callable),
        producer.returned_prototype,
        "{}: the commit uses the producer return",
        run.arm.label()
    );
    assert_eq!(
        commit.receiver_class,
        GuestClass::Table(prepared.receiver),
        "{}: the commit receiver is the action receiver",
        run.arm.label()
    );
    assert_eq!(commit.token_raw, prepared.userdata_raw);
    assert!(commit.payload_is_head);
    assert!(commit.weak_matches_callable);
    assert_eq!(
        commit.action_closure,
        (
            producer.closure.domain(),
            producer.closure.slot(),
            producer.closure.generation(),
        ),
        "{}: the commit re-read the raw __call the producer used",
        run.arm.label()
    );
    let (frame, roots) = commit
        .outcome
        .as_ref()
        .unwrap_or_else(|error| panic!("{}: the commit refused: {error}", run.arm.label()));
    assert_eq!(*roots, 3);
    assert_eq!(commit.taken_userdata, Some(prepared.userdata));

    let callee = run
        .witness
        .marks
        .iter()
        .find_map(|mark| mark.callee.as_ref())
        .unwrap_or_else(|| panic!("{}: no child frame was observed", run.arm.label()));
    assert_eq!(
        callee.prototype_id,
        producer.returned_prototype,
        "{}: the child executed the producer's actual instance",
        run.arm.label()
    );
    let child_receiver = callee
        .parameter_values
        .first()
        .cloned()
        .flatten()
        .expect("the child receiver formal slot");
    assert!(
        same_array(&child_receiver, &commit.receiver),
        "{}: the child receiver is the exact commit operand Arc",
        run.arm.label()
    );
    assert_eq!(
        callee.parameter_values.get(1).cloned().flatten(),
        Some(Value::Int(commit.token_raw)),
        "{}: the child token slot reuses the commit operand",
        run.arm.label()
    );
    assert_eq!(
        callee.token.parts,
        (
            prepared.userdata.domain(),
            prepared.userdata.slot(),
            prepared.userdata.generation(),
        ),
        "{}: the child formal token borrows A",
        run.arm.label()
    );
    assert_eq!(
        callee.token.rebound_parts,
        callee.token.parts,
        "{}: the heap still names the same payload",
        run.arm.label()
    );
    assert!(
        callee.table_token_refused,
        "{}: A's token is not a lua.table resource",
        run.arm.label()
    );
    let continuation = run
        .mark("parent-after")
        .continuation
        .as_ref()
        .expect("the parent continuation must re-borrow the token");
    assert_eq!(
        continuation.parts,
        callee.token.parts,
        "{}: the continuation reads the same A token",
        run.arm.label()
    );

    // The child entry is bound to the same compiled call site the parent
    // resumes, and the child is the declared replacement prototype.
    let callback_entry = run
        .witness
        .entries
        .iter()
        .find(|entry| entry.label == child_label)
        .expect("the child entry must be recorded first");
    let child_return_ip = run
        .mark(child_label)
        .last_return_ip
        .expect("the child frame must resume bytecode");
    assert_eq!(
        callback_entry.return_ip,
        Some(child_return_ip),
        "{}: the child entry resume address is the real call",
        run.arm.label()
    );
    let call_site = run
        .call_sites
        .iter()
        .find(|site| {
            site.owner == run.mark("parent-enter").last_prototype && site.argc == CALL_ARGC
        })
        .expect("the parent CallValue(2)");
    assert_eq!(
        child_return_ip,
        call_site.return_ip,
        "{}: the child resumes at the compiled call's return address",
        run.arm.label()
    );

    // B is still the real published head.
    assert_eq!(
        published_pending(run),
        vec![(
            u64::try_from(prepared.successor_raw).expect("successor token"),
            prepared.successor.slot(),
            prepared.successor.generation(),
        )],
        "{}: only B remains queued",
        run.arm.label()
    );
    assert_eq!(published_count(run), 1);
    assert!(
        published_pending_flag(run, prepared.successor)
            && !published_settled_flag(run, prepared.successor),
        "{}: B is still pending and not settled",
        run.arm.label()
    );
    assert!(
        !published_pending_flag(run, prepared.userdata)
            && published_settled_flag(run, prepared.userdata),
        "{}: A is settled exactly once",
        run.arm.label()
    );
    assert_eq!(
        heap_query(&mut run.vm, |heap| heap
            .userdata_metatable(prepared.successor)),
        Some(prepared.finalizer_metatable),
        "{}: B keeps the same finalizer metatable",
        run.arm.label()
    );
    let next = {
        let mut host = run.vm.host_context();
        snapshot_published_typed_table_call(&mut host)
            .unwrap_or_else(|error| panic!("{}: B snapshot: {error:?}", run.arm.label()))
    };
    assert_eq!(next.userdata, prepared.successor);
    assert_eq!(
        next.raw_token,
        u64::try_from(prepared.successor_raw).expect("successor token")
    );
    assert_eq!(next.receiver, prepared.receiver);
    assert_eq!(next.closure, producer.closure);

    // Only the three roots this commit wrote are withdrawn. The prepare
    // frame's independent roots stay exact.
    let prepared_closure = prepared
        .call_closure
        .expect("the bind import must install the raw __call");
    let mut host = run.vm.host_context();
    assert_eq!(
        heap::rooted_userdata(&mut host, *frame, 0),
        Ok(Some(prepared.userdata))
    );
    assert_eq!(
        heap::rooted_closure(&mut host, *frame, 1),
        Ok(Some(prepared_closure))
    );
    assert_eq!(
        heap::rooted_table(&mut host, *frame, 2),
        Ok(Some(prepared.receiver))
    );
    let prepare_frame = prepared.root_frame;
    assert_eq!(
        heap::rooted_table(&mut host, prepare_frame, 0),
        Ok(Some(prepared.weak_table))
    );
    assert_eq!(
        heap::rooted_table(&mut host, prepare_frame, 1),
        Ok(Some(prepared.receiver))
    );
    assert_eq!(
        heap::rooted_table(&mut host, prepare_frame, 2),
        Ok(Some(prepared.sentinel))
    );
    for slot in [0u32, 1, 2] {
        heap::with_heap(&mut host, |heap| heap.clear_root(*frame, slot))
            .unwrap_or_else(|error| panic!("{}: clear root: {error:?}", run.arm.label()));
    }
    assert_eq!(
        heap::rooted_table(&mut host, prepare_frame, 1),
        Ok(Some(prepared.receiver))
    );
    assert_eq!(heap::rooted_userdata(&mut host, *frame, 0), Ok(None));
    let _ = heap::collect(&mut host).expect("real collection");
    let _ = host;

    assert!(
        !published_userdata_alive(run, prepared.userdata),
        "{}: A is collected after its three roots are withdrawn",
        run.arm.label()
    );
    assert!(
        !published_token_open(run, prepared.userdata_raw),
        "{}: A's Core token is retired",
        run.arm.label()
    );
    assert_eq!(
        published_slot_generation(run, prepared.userdata),
        prepared.userdata.generation() + 1,
        "{}: A's slot advances exactly one generation",
        run.arm.label()
    );
    assert!(
        published_userdata_alive(run, prepared.successor),
        "{}: the queued B stays alive",
        run.arm.label()
    );
    assert!(
        published_token_open(run, prepared.successor_raw),
        "{}: B's typed token stays open",
        run.arm.label()
    );
    assert_eq!(
        published_slot_generation(run, prepared.successor),
        prepared.successor.generation(),
        "{}: B does not advance its generation",
        run.arm.label()
    );
    assert_eq!(
        published_pending(run),
        vec![(
            u64::try_from(prepared.successor_raw).expect("successor token"),
            prepared.successor.slot(),
            prepared.successor.generation(),
        )],
        "{}: the collection does not consume B",
        run.arm.label()
    );
    assert_eq!(published_count(run), 1);
    assert!(
        heap_query(&mut run.vm, |heap| Ok(heap.alive(prepared.sentinel))),
        "{}: the independent sentinel root stays readable",
        run.arm.label()
    );
    assert!(
        heap_query(&mut run.vm, |heap| Ok(heap.alive(prepared.receiver))),
        "{}: the receiver stays readable through its independent root and B",
        run.arm.label()
    );
    assert!(
        heap_query(&mut run.vm, |heap| Ok(heap.alive(prepared.weak_table))),
        "{}: the still-rooted weak witness table stays",
        run.arm.label()
    );
}

/// The post-read replacement refusal: the old typed local did not commit, no
/// child started, and the exact A/B queue, prepared roots and real tokens are
/// unchanged. The same VM's heap helper then re-prepares with the replacement
/// raw edge and takes A with the new Arc. The replacement's real guest call is
/// witnessed by the before-read arm; this faulted VM is not resumed.
fn assert_producer_post_read_refusal(run: &mut ArmRun) {
    let prepared = published_prepared(run).clone();
    let producer = published_producer(run).clone();
    let old_closure = prepared
        .call_closure
        .expect("the bind import must install the raw __call");
    assert_eq!(
        producer.closure, old_closure,
        "the producer read the original raw __call before the swap"
    );
    assert!(
        producer.parent_candidate_matches,
        "the producer read the old target Arc, not the fixture selection"
    );
    let current = heap_query(&mut run.vm, |heap| {
        heap.named_closure_edge(prepared.receiver_metatable, "__call")
    });
    assert_ne!(
        current, old_closure,
        "the swap really replaced the raw __call generation"
    );
    assert!(
        heap_query(&mut run.vm, |heap| Ok(heap.closure_alive(old_closure))),
        "the producer's original closure generation is still live"
    );
    assert!(
        heap_query(&mut run.vm, |heap| Ok(heap.closure_alive(current))),
        "the replacement closure generation is live"
    );

    assert_commit_refusal(
        run,
        "TokenMismatch",
        GuestClass::Table(prepared.receiver),
        true,
        false,
    );
    let commit = published_commit(run).clone();
    assert_eq!(
        commit.action_closure,
        (current.domain(), current.slot(), current.generation()),
        "the refused commit re-read the replacement generation"
    );
    assert_eq!(
        callable_id(&commit.callable),
        producer.returned_prototype,
        "the refused commit still carried the old typed local"
    );
    assert_published_queue_intact(run);

    // Re-prepare from the same VM's current raw edge and take A with the new
    // instance Arc through the same heap-owner helper. The old typed local
    // Arc is not reused.
    let new_arc = {
        let mut host = run.vm.host_context();
        heap::with_heap(&mut host, |heap| heap.closure_callable(current))
            .unwrap_or_else(|error| panic!("raw __call Arc read: {error:?}"))
    };
    assert!(
        !std::sync::Weak::ptr_eq(&producer.callable, &Arc::downgrade(&new_arc)),
        "the old typed local Weak is not the replacement Arc"
    );
    let payload = {
        let host = run.vm.host_context();
        let handle =
            ResourceHandle::from_raw(u64::try_from(prepared.userdata_raw).expect("A token"))
                .expect("A handle");
        let borrowed = host
            .borrow_resource_with_key::<LuaUserdataResource>(handle, &LuaUserdataResource::key())
            .unwrap_or_else(|error| panic!("A borrow: {error}"));
        borrowed.get().userdata()
    };
    let receiver_value = run
        .witness
        .issued_table
        .clone()
        .expect("the issued receiver carrier");
    let (frame, taken) = {
        let mut host = run.vm.host_context();
        commit_published_typed_table_call(
            &mut host,
            &new_arc,
            Some(payload),
            prepared.finalizer_metatable,
            &receiver_value,
            u64::try_from(prepared.userdata_raw).expect("A token"),
        )
        .unwrap_or_else(|error| panic!("same-VM re-prepared commit: {error:?}"))
    };
    assert_eq!(taken.userdata, prepared.userdata);
    assert_eq!(taken.roots_written, 3);
    let mut host = run.vm.host_context();
    assert_eq!(
        heap::rooted_userdata(&mut host, frame, 0),
        Ok(Some(prepared.userdata))
    );
    assert_eq!(heap::rooted_closure(&mut host, frame, 1), Ok(Some(current)));
    assert_eq!(
        heap::rooted_table(&mut host, frame, 2),
        Ok(Some(prepared.receiver))
    );
    let _ = host;
    assert_eq!(
        published_pending(run),
        vec![(
            u64::try_from(prepared.successor_raw).expect("successor token"),
            prepared.successor.slot(),
            prepared.successor.generation(),
        )],
        "the re-prepared helper consumes exactly A"
    );
    assert_eq!(published_count(run), 1);
    assert!(
        published_pending_flag(run, prepared.successor)
            && !published_settled_flag(run, prepared.successor),
        "B is still the pending head after the re-prepared helper"
    );
    let mut host = run.vm.host_context();
    let _ = heap::collect(&mut host).expect("re-prepared cleanup collection");
}

/// The same-arity wrong-signature return control: Core refuses the real
/// callable at the exact return boundary, before any commit or child. The
/// queue, prepared roots and both real tokens stay exact.
fn assert_producer_wrong_signature(run: &mut ArmRun) {
    run.assert_fuel();
    assert_eq!(
        run.patch.producer_import.map(|index| {
            run.host_imports
                .get(index as usize)
                .map(|import| import.name.as_str())
        }),
        Some(Some(PRODUCER_IMPORT)),
        "the compiled program must carry the producer import"
    );
    match &run.status {
        Err(VmError::TypeMismatch(kind)) => assert_eq!(
            *kind,
            "callable",
            "{}: the Core callable return check must refuse the value",
            run.arm.label()
        ),
        other => panic!(
            "{}: expected the Core callable return refusal, got {other:?}",
            run.arm.label()
        ),
    }
    assert_eq!(
        run.labels(),
        ["parent-enter"],
        "{}: the parent must stop at the producer's Core return check",
        run.arm.label()
    );
    assert_eq!(
        typed_callback_entries(run).len(),
        0,
        "{}: the refused callable must not enter a callback frame",
        run.arm.label()
    );
    assert!(
        run.witness.commits.is_empty(),
        "{}: no exclusive commit body may run",
        run.arm.label()
    );
    assert!(
        run.exit().is_none(),
        "{}: the refused run has no chunk completion",
        run.arm.label()
    );
    let producer = published_producer(run).clone();
    assert_eq!(
        producer.returned_prototype,
        callable_id(&run.pick().other),
        "{}: the producer read the installed wrong-signature other instance",
        run.arm.label()
    );
    assert!(
        !producer.parent_candidate_matches,
        "{}: the wrong-signature instance is not the old fixture selection",
        run.arm.label()
    );
    let prototype = run.prototype(producer.returned_prototype);
    assert_eq!(
        prototype.arity,
        CALL_ARGC,
        "{}: the refused callable has the same arity",
        run.arm.label()
    );
    assert_eq!(
        prototype.schema,
        Some(table_callable_schema()),
        "{}: the refused prototype declares lua.table, not lua.userdata",
        run.arm.label()
    );
    let Some(TypeSchema::Callable { params, result }) = &prototype.schema else {
        panic!(
            "{}: the refused prototype has no callable schema",
            run.arm.label()
        );
    };
    assert_eq!(
        params[0],
        TypeSchema::Array(Box::new(TypeSchema::Int)),
        "{}: the receiver schema is identical",
        run.arm.label()
    );
    assert_eq!(
        params[1],
        TypeSchema::Resource(LuaTableResource::key()),
        "{}: the refused declaration names the other real resource key",
        run.arm.label()
    );
    assert_eq!(
        result.as_ref(),
        &TypeSchema::Bool,
        "{}: the declared result is identical",
        run.arm.label()
    );
    assert!(
        run.witness
            .marks
            .iter()
            .all(|mark| mark.label != OTHER_LABEL),
        "{}: the refused callback body must not run",
        run.arm.label()
    );
    assert_published_queue_intact(run);
}

fn published_userdata_alive(run: &mut ArmRun, userdata: UserdataRef) -> bool {
    heap_query(&mut run.vm, |heap| Ok(heap.userdata_alive(userdata)))
}

fn published_token_open(run: &mut ArmRun, raw: i64) -> bool {
    let Ok(raw) = u64::try_from(raw) else {
        return false;
    };
    let Ok(handle) = ResourceHandle::from_raw(raw) else {
        return false;
    };
    let host = run.vm.host_context();
    host.typed_resource_with_key::<LuaUserdataResource>(handle, &LuaUserdataResource::key())
        .is_ok()
}

fn assert_published_queue_intact(run: &mut ArmRun) {
    let prepared = published_prepared(run).clone();
    assert_eq!(
        published_pending(run),
        vec![
            (
                u64::try_from(prepared.userdata_raw).expect("A token"),
                prepared.userdata.slot(),
                prepared.userdata.generation(),
            ),
            (
                u64::try_from(prepared.successor_raw).expect("B token"),
                prepared.successor.slot(),
                prepared.successor.generation(),
            ),
        ],
        "{}: the refused commit must leave A then B queued",
        run.arm.label()
    );
    assert_eq!(published_count(run), 2);
    assert!(
        published_pending_flag(run, prepared.userdata)
            && !published_settled_flag(run, prepared.userdata),
        "{}: A stays pending and unsettled",
        run.arm.label()
    );
    assert!(
        published_pending_flag(run, prepared.successor)
            && !published_settled_flag(run, prepared.successor),
        "{}: B stays pending and unsettled",
        run.arm.label()
    );
    let mut host = run.vm.host_context();
    let top = heap::with_heap(&mut host, |heap| Ok(heap.current_frame()))
        .unwrap_or_else(|error| panic!("{}: top root frame: {error:?}", run.arm.label()));
    assert_eq!(
        top,
        Some(prepared.root_frame),
        "{}: the refused commit must not open a root frame",
        run.arm.label()
    );
    assert_eq!(
        heap::rooted_table(&mut host, prepared.root_frame, 0),
        Ok(Some(prepared.weak_table)),
        "{}: the independent weak witness root is unchanged",
        run.arm.label()
    );
    assert_eq!(
        heap::rooted_table(&mut host, prepared.root_frame, 1),
        Ok(Some(prepared.receiver)),
        "{}: the independent receiver root is unchanged",
        run.arm.label()
    );
    assert_eq!(
        heap::rooted_table(&mut host, prepared.root_frame, 2),
        Ok(Some(prepared.sentinel)),
        "{}: the independent sentinel root is unchanged",
        run.arm.label()
    );
    assert_eq!(
        heap::rooted_table(&mut host, prepared.root_frame, 3),
        Ok(None),
        "{}: the unused prepare slot must stay empty",
        run.arm.label()
    );
    assert_eq!(
        heap::rooted_userdata(&mut host, prepared.root_frame, 3),
        Ok(None),
        "{}: the unused prepare slot must hold no userdata",
        run.arm.label()
    );
    assert_eq!(
        heap::rooted_closure(&mut host, prepared.root_frame, 3),
        Ok(None),
        "{}: the unused prepare slot must hold no closure",
        run.arm.label()
    );
    for (owner_label, userdata, raw) in [
        ("A", prepared.userdata, prepared.userdata_raw),
        ("B", prepared.successor, prepared.successor_raw),
    ] {
        let handle = ResourceHandle::from_raw(u64::try_from(raw).expect("prepared typed token"))
            .expect("prepared typed handle");
        let borrowed = host
            .borrow_resource_with_key::<LuaUserdataResource>(handle, &LuaUserdataResource::key())
            .unwrap_or_else(|error| {
                panic!("{}: {owner_label} token borrow: {error}", run.arm.label())
            });
        assert_eq!(
            borrowed.get().userdata(),
            userdata,
            "{}: {owner_label}'s borrowable token must name its exact owner",
            run.arm.label()
        );
        drop(borrowed);
        assert_eq!(
            heap::borrow_published_userdata(&host, userdata),
            Ok(userdata),
            "{}: {owner_label}'s published heap token must stay bound to its owner",
            run.arm.label()
        );
    }
}

fn assert_commit_refusal(
    run: &ArmRun,
    needle: &str,
    expected_class: GuestClass,
    expected_payload_is_head: bool,
    expected_weak_match: bool,
) {
    run.assert_fuel();
    assert_published_import(run);
    let message = match &run.status {
        Err(VmError::HostError(message)) => message.clone(),
        other => panic!(
            "{}: expected the typed commit body's HostError, got {other:?}",
            run.arm.label()
        ),
    };
    assert!(
        message.starts_with(COMMIT_REFUSAL_PREFIX),
        "{}: the diagnostic must come from the commit body: {message}",
        run.arm.label()
    );
    assert!(
        message.contains(needle),
        "{}: expected '{needle}' in the commit diagnostic: {message}",
        run.arm.label()
    );
    assert_eq!(
        run.labels(),
        ["parent-enter"],
        "{}: the parent must stop at the exclusive commit",
        run.arm.label()
    );
    assert_eq!(
        typed_callback_entries(run).len(),
        0,
        "{}: no typed child frame may start",
        run.arm.label()
    );
    assert!(
        run.exit().is_none(),
        "{}: the refused run has no chunk completion",
        run.arm.label()
    );
    let prepared = published_prepared(run);
    let commit = published_commit(run);
    assert_eq!(
        commit.receiver_class,
        expected_class,
        "{}: the actual receiver operand classification",
        run.arm.label()
    );
    assert_eq!(
        commit.payload_is_head,
        expected_payload_is_head,
        "{}: the typed payload owner relation",
        run.arm.label()
    );
    assert_eq!(
        commit.weak_matches_callable,
        expected_weak_match,
        "{}: the Weak credential relation to the actual operand Arc",
        run.arm.label()
    );
    assert!(
        commit.outcome.is_err(),
        "{}: the exclusive commit must refuse",
        run.arm.label()
    );
    assert!(
        commit.taken_userdata.is_none(),
        "{}: a refusal consumes nothing",
        run.arm.label()
    );
    let parent = run.mark("parent-enter");
    let entries = commit_entries(run);
    assert_eq!(
        entries.len(),
        1,
        "{}: exactly one raw commit entry",
        run.arm.label()
    );
    let entry = entries[0];
    assert_eq!(
        (entry.frame_len, entry.prototype_id, entry.parent_prototype),
        (
            parent.frame_len,
            parent.last_prototype,
            parent.parent_prototype
        ),
        "{}: the commit body runs in the active parent frame",
        run.arm.label()
    );
    let _ = prepared;
}

/// Re-takes the real credentials in this same VM, proves the commit entry
/// refuses a missing typed payload and a real lua.table key, then commits A
/// with the same callable Arc and receiver.
fn retry_published_commit(run: &mut ArmRun, restore_old_closure: bool) {
    let prepared = published_prepared(run).clone();
    let prepared_closure = prepared
        .call_closure
        .expect("the bind import must install the raw __call");
    let pick = run.pick();
    let Value::Callable(target_arc) = &pick.chosen else {
        panic!("{}: the picked token is not callable", run.arm.label());
    };
    let target_arc = Arc::clone(target_arc);
    let receiver_value = run
        .witness
        .issued_table
        .clone()
        .expect("the issued receiver carrier");
    let mut host = run.vm.host_context();
    if restore_old_closure {
        assert!(
            heap::with_heap(&mut host, |heap| Ok(heap.closure_alive(prepared_closure)))
                .unwrap_or_else(|error| panic!("closure liveness: {error:?}")),
            "{}: the original closure generation is still live before restore",
            run.arm.label()
        );
        heap::with_heap(&mut host, |heap| {
            heap.set_named_closure(prepared.receiver_metatable, "__call", prepared_closure)
        })
        .unwrap_or_else(|error| panic!("{}: restore raw __call: {error:?}", run.arm.label()));
    }
    let snapshot = snapshot_published_typed_table_call(&mut host)
        .unwrap_or_else(|error| panic!("{}: retry snapshot: {error:?}", run.arm.label()));
    assert_eq!(snapshot.userdata, prepared.userdata);
    assert_eq!(snapshot.receiver, prepared.receiver);
    assert_eq!(snapshot.closure, prepared_closure);
    let payload = {
        let raw = u64::try_from(prepared.userdata_raw).expect("A token");
        let handle = ResourceHandle::from_raw(raw).expect("A handle");
        let borrowed = host
            .borrow_resource_with_key::<LuaUserdataResource>(handle, &LuaUserdataResource::key())
            .unwrap_or_else(|error| panic!("{}: A borrow: {error}", run.arm.label()));
        borrowed.get().userdata()
    };
    assert_eq!(payload, prepared.userdata);

    // A real lua.table token does not pass the lua.userdata key, and A's real
    // token does not pass the lua.table key. The commit entry with no typed
    // payload refuses before any write.
    let table_token = heap::publish(&mut host).expect("a real lua.table token");
    let Value::Int(table_raw) = table_token else {
        panic!("{}: the fresh table token is not a handle", run.arm.label());
    };
    let table_handle = ResourceHandle::from_raw(u64::try_from(table_raw).expect("table token"))
        .expect("table handle");
    assert!(
        host.borrow_resource_with_key::<LuaUserdataResource>(
            table_handle,
            &LuaUserdataResource::key()
        )
        .is_err(),
        "{}: a lua.table token must not borrow as lua.userdata",
        run.arm.label()
    );
    let a_handle = ResourceHandle::from_raw(u64::try_from(prepared.userdata_raw).expect("A token"))
        .expect("A handle");
    assert!(
        host.borrow_resource_with_key::<LuaTableResource>(a_handle, &LuaTableResource::key())
            .is_err(),
        "{}: A's lua.userdata token must not borrow as lua.table",
        run.arm.label()
    );
    let refused = commit_published_typed_table_call(
        &mut host,
        &target_arc,
        None,
        prepared.finalizer_metatable,
        &receiver_value,
        u64::try_from(prepared.userdata_raw).expect("A token"),
    );
    assert!(
        matches!(refused, Err(heap::HeapError::TokenMismatch)),
        "{}: the commit entry must refuse a missing typed payload, saw {refused:?}",
        run.arm.label()
    );
    let after_refusal = heap::with_heap(&mut host, |heap| Ok(heap.pending_finalizer_records()))
        .unwrap_or_else(|error| panic!("{}: pending read: {error:?}", run.arm.label()));
    assert_eq!(
        after_refusal,
        vec![
            (
                u64::try_from(prepared.userdata_raw).expect("A token"),
                prepared.userdata.slot(),
                prepared.userdata.generation(),
            ),
            (
                u64::try_from(prepared.successor_raw).expect("B token"),
                prepared.successor.slot(),
                prepared.successor.generation(),
            ),
        ],
        "{}: the refused entry leaves the queue unchanged",
        run.arm.label()
    );
    let (frame, taken) = commit_published_typed_table_call(
        &mut host,
        &target_arc,
        Some(payload),
        prepared.finalizer_metatable,
        &receiver_value,
        u64::try_from(prepared.userdata_raw).expect("A token"),
    )
    .unwrap_or_else(|error| panic!("{}: same-VM retry commit: {error:?}", run.arm.label()));
    assert_eq!(taken.userdata, prepared.userdata);
    assert_eq!(taken.roots_written, 3);
    assert_eq!(
        heap::rooted_userdata(&mut host, frame, 0),
        Ok(Some(prepared.userdata))
    );
    assert_eq!(
        heap::rooted_closure(&mut host, frame, 1),
        Ok(Some(prepared_closure))
    );
    assert_eq!(
        heap::rooted_table(&mut host, frame, 2),
        Ok(Some(prepared.receiver))
    );
    let _ = host;
    assert_eq!(
        published_pending(run),
        vec![(
            u64::try_from(prepared.successor_raw).expect("B token"),
            prepared.successor.slot(),
            prepared.successor.generation(),
        )],
        "{}: the retry consumes exactly A",
        run.arm.label()
    );
    assert_eq!(published_count(run), 1);
    assert!(
        published_pending_flag(run, prepared.successor)
            && !published_settled_flag(run, prepared.successor),
        "{}: B is still the pending head after the retry",
        run.arm.label()
    );
    let mut host = run.vm.host_context();
    let _ = heap::collect(&mut host).expect("retry cleanup collection");
}

#[test]
fn active_parent_reads_the_published_typed_head_and_commits_it() {
    let mut run = run_arm(Arm::PublishedHeadProducer);
    let (producer, target_id) = assert_producer_read_phase(&mut run, true, true);
    assert!(
        producer.parent_candidate_matches,
        "producer: the positive arm installs the same target the old selection names"
    );
    assert_eq!(
        producer.returned_prototype, target_id,
        "producer: the heap-sourced Arc is the target instance"
    );
    assert_producer_commit_identity(&run);
    let (produce_ip, admit_ip, commit_ip, call_ip) = producer_parent_sites(&run);
    assert_published_positive(&mut run);
    eprintln!(
        "LUA_TYPED_RESOURCE arm={} layer=published-head-source producer_ip={produce_ip} admit_ip={admit_ip} commit_ip={commit_ip} call_ip={call_ip} prototype={} roots=3",
        run.arm.label(),
        producer.returned_prototype
    );
}

#[test]
fn active_parent_reads_the_replacement_before_committing_it() {
    let mut run = run_arm(Arm::PublishedHeadProducerReplacedBeforeRead);
    let (producer, target_id) = assert_producer_read_phase(&mut run, true, true);
    assert!(
        !producer.parent_candidate_matches,
        "replacement-before-read: the producer did not take the old fixture selection"
    );
    assert_ne!(
        producer.returned_prototype, target_id,
        "replacement-before-read: the producer read a different instance"
    );
    assert!(
        run.pick().chosen_is_target,
        "replacement-before-read: the old selection still names the target candidate"
    );
    assert_producer_commit_identity(&run);
    let sites = producer_parent_sites(&run);
    assert_producer_transaction(&mut run, ALTERNATE_LABEL);
    eprintln!(
        "LUA_TYPED_RESOURCE arm={} layer=published-head-source replacement=1 old_selection={target_id} read_prototype={} sites={sites:?}",
        run.arm.label(),
        producer.returned_prototype
    );
}

#[test]
fn typed_published_head_producer_refuses_the_post_read_replacement_and_wrong_signature() {
    let mut changed = run_arm(Arm::PublishedHeadProducerReplacedBeforeCommit);
    let (producer, target_id) = assert_producer_read_phase(&mut changed, false, true);
    assert!(
        producer.parent_candidate_matches,
        "post-read-replacement: the read happened before the raw edge changed"
    );
    assert_eq!(
        producer.returned_prototype, target_id,
        "post-read-replacement: the old typed local carried the target Arc"
    );
    assert_producer_post_read_refusal(&mut changed);

    let mut wrong = run_arm(Arm::PublishedHeadProducerWrongSignature);
    let (producer, target_id) = assert_producer_read_phase(&mut wrong, true, false);
    assert!(
        !producer.parent_candidate_matches,
        "wrong-signature: the installed instance is not the old fixture selection"
    );
    assert_ne!(
        producer.returned_prototype, target_id,
        "wrong-signature: the read instance is the other-resource closure"
    );
    assert_producer_wrong_signature(&mut wrong);
}

#[test]
fn active_parent_commits_the_published_typed_head_before_the_same_slot_call() {
    let mut run = run_arm(Arm::PublishedCommit);
    assert_eq!(
        run.patch.commit.as_ref().map(|commit| commit.argument),
        Some(CommitArgument::Argument),
        "published-commit: the positive arm passes the head's real token"
    );
    assert_published_positive(&mut run);
}

#[test]
fn typed_published_commit_refuses_wrong_operands_without_consuming_the_head() {
    let mut other_owner = run_arm(Arm::PublishedCommitOtherOwner);
    assert_eq!(
        other_owner
            .patch
            .commit
            .as_ref()
            .map(|commit| commit.argument),
        Some(CommitArgument::Wrong),
        "other-owner: the commit operands select the successor token"
    );
    let prepared = published_prepared(&other_owner).clone();
    assert_commit_refusal(
        &other_owner,
        "TokenMismatch",
        GuestClass::Table(prepared.receiver),
        false,
        true,
    );
    let commit = published_commit(&other_owner);
    assert_eq!(
        commit.token_raw, prepared.successor_raw,
        "other-owner: the actual token is the queued successor's"
    );
    assert_eq!(
        commit.payload_parts,
        Some((
            prepared.successor.domain(),
            prepared.successor.slot(),
            prepared.successor.generation(),
        )),
        "other-owner: the Core payload is B, a legal lua.userdata for another object"
    );
    assert!(
        commit.token_raw != prepared.userdata_raw,
        "other-owner: the token differs from the head's real token"
    );
    assert_published_queue_intact(&mut other_owner);
    retry_published_commit(&mut other_owner, false);

    let mut forged = run_arm(Arm::PublishedCommitUnissuedReceiver);
    assert_eq!(
        forged.patch.commit.as_ref().map(|commit| commit.argument),
        Some(CommitArgument::ForgedArgument),
        "unissued receiver: the commit operands select the forged receiver"
    );
    assert_commit_refusal(&forged, "WrongKind", GuestClass::Unissued, true, true);
    let commit = published_commit(&forged);
    let Value::Array(forged_items) = &commit.receiver else {
        panic!("unissued receiver: the operand must be an array");
    };
    let Value::Array(issued_items) = &forged
        .witness
        .issued_table
        .clone()
        .expect("issued receiver")
    else {
        panic!("unissued receiver: the issued value must be an array");
    };
    assert_eq!(
        forged_items.as_slice(),
        issued_items.as_slice(),
        "unissued receiver: the copy has the issued values"
    );
    assert!(
        !same_array(&commit.receiver, &Value::Array(Arc::clone(issued_items))),
        "unissued receiver: the copy is a different VM array object"
    );
    assert_published_queue_intact(&mut forged);
    retry_published_commit(&mut forged, false);

    let mut changed = run_arm(Arm::PublishedCommitRawArcChanged);
    let prepared = published_prepared(&changed).clone();
    let prepared_closure = prepared
        .call_closure
        .expect("raw-arc-changed: bind must install the raw __call");
    assert_commit_refusal(
        &changed,
        "TokenMismatch",
        GuestClass::Table(prepared.receiver),
        true,
        false,
    );
    let commit = published_commit(&changed).clone();
    let current = heap_query(&mut changed.vm, |heap| {
        heap.named_closure_edge(prepared.receiver_metatable, "__call")
    });
    assert_ne!(
        current, prepared_closure,
        "raw-arc-changed: the guest swap replaced the raw __call generation"
    );
    assert_eq!(
        commit.action_closure,
        (current.domain(), current.slot(), current.generation()),
        "raw-arc-changed: the commit re-read the replacement generation"
    );
    assert!(
        heap_query(&mut changed.vm, |heap| Ok(
            heap.closure_alive(prepared_closure)
        )),
        "raw-arc-changed: the pre-swap generation is still live before restore"
    );
    assert!(
        heap_query(&mut changed.vm, |heap| Ok(heap.closure_alive(current))),
        "raw-arc-changed: the replacement generation is live"
    );
    assert_published_queue_intact(&mut changed);
    retry_published_commit(&mut changed, true);
}
