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
//! Not claimed here: a finalizer dequeue, the pending queue, commit, root
//! handoff, close, collection, scheduling pauses, or a source-level general
//! closure bridge. The heap only issues one real table carrier, one real
//! `lua.userdata` token and one real `lua.table` token; nothing is collected or
//! closed.
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

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

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

/// The first statement of the two declared closures names them.
const TARGET_LABEL: &str = "target-enter";
const OTHER_LABEL: &str = "other-enter";

// ---- one run at a time ----------------------------------------------------

/// The exact observation of the last run, like the sibling active-frame
/// witness: this module drives its arms sequentially in one test body.
static WITNESS: Mutex<Option<Witness>> = Mutex::new(None);

/// The integer the marker host returns and `note` uses as its selection mode.
static SELECT: AtomicUsize = AtomicUsize::new(0);

/// Which operand the private plugin hands to the second exact static import:
/// `0` keeps the original arms untouched, `1` passes the issued receiver, `2`
/// passes an equal-valued array that the heap never issued.
static GATE_MODE: AtomicUsize = AtomicUsize::new(0);

/// The structural record the plugin produced for the last compile.
static PATCH: Mutex<Option<PatchReport>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arm {
    Positive,
    OtherResourceKey,
    OtherArgumentToken,
    PrecommitBorrowSeam,
    PrecommitUnissuedReceiver,
}

impl Arm {
    fn label(self) -> &'static str {
        match self {
            Arm::Positive => "positive",
            Arm::OtherResourceKey => "other-resource-key",
            Arm::OtherArgumentToken => "other-argument-token",
            Arm::PrecommitBorrowSeam => "precommit-borrow-seam",
            Arm::PrecommitUnissuedReceiver => "precommit-unissued-receiver",
        }
    }

    fn fixture(self) -> &'static str {
        match self {
            Arm::OtherArgumentToken => OTHER_ARGUMENT_FIXTURE,
            _ => FIXTURE,
        }
    }

    fn select(self) -> usize {
        match self {
            Arm::OtherResourceKey => 5,
            _ => 0,
        }
    }

    fn gate_mode(self) -> usize {
        match self {
            Arm::PrecommitBorrowSeam => 1,
            Arm::PrecommitUnissuedReceiver => 2,
            _ => 0,
        }
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
fn bind_hosts(vm: &mut Vm) -> Result<(), VmError> {
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
    issued_table: Option<Value>,
    issued_table_ref: Option<TableRef>,
    issued_userdata: Option<UserdataRef>,
    issued_userdata_raw: Option<i64>,
    table_token: Option<TableTokenRecord>,
}

fn with_witness<T>(body: impl FnOnce(&mut Witness) -> T) -> T {
    let mut guard = WITNESS.lock().expect("typed resource witness");
    let witness = guard
        .as_mut()
        .expect("the typed resource witness must be installed before the run");
    body(witness)
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
    with_witness(|witness| witness.entries.push(entry));

    if label == "chunk-mode" {
        let select = i64::try_from(SELECT.load(Ordering::SeqCst))
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
        with_witness(|witness| witness.table_token = Some(TableTokenRecord { raw, parts }));
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
                    if prototype.parameter_slots.len() != PARENT_PARAMS {
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
                    if prototype.parameter_slots.len() != PARENT_PARAMS {
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
        label,
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

    with_witness(|witness| witness.marks.push(record));
    Ok(CallOutcome::Return(CallReturn::one(Value::Int(0))))
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
fn host_note(_vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
    let [Value::Int(mode), target, first, second, single, other] = args else {
        return Err(host_fault_error(
            "note expects a mode and five written candidates",
        ));
    };
    let chosen = match *mode {
        0 | 4 => target,
        5 => other,
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
    with_witness(|witness| witness.picks.push(record));
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
    with_witness(|witness| {
        witness.issued_table_ref = Some(table);
        witness.issued_table = Some(issued.clone());
    });
    Ok(CallOutcome::Return(CallReturn::one(issued)))
}

/// Issues one real typed `lua.userdata` token.
fn host_fault(vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
    if !args.is_empty() {
        return Err(host_fault_error("fault takes no arguments"));
    }
    let mut host = vm.host_context();
    let userdata = heap::publish_userdata(&mut host).map_err(heap_fault)?;
    let raw = heap::with_heap(&mut host, |heap| heap.published_userdata_token(userdata))
        .map_err(heap_fault)?;
    let raw = i64::try_from(raw)
        .map_err(|_| host_fault_error("the userdata token does not fit a signed carrier"))?;
    with_witness(|witness| {
        witness.issued_userdata = Some(userdata);
        witness.issued_userdata_raw = Some(raw);
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
    with_witness(|witness| witness.entries.push(entry));

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
    let (issued_table, issued_userdata) = with_witness(|witness| {
        assert!(
            !witness.picks.is_empty(),
            "the exact static note must run before the precommit import"
        );
        (witness.issued_table_ref, witness.issued_userdata)
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
    with_witness(|witness| witness.gates.push(record));

    if !receiver_is_issued {
        return Err(host_fault_error(PRECOMMIT_REFUSAL_RECEIVER));
    }
    Ok(CallOutcome::Return(CallReturn::one(Value::Bool(true))))
}

// ---- the private plugin: real lowering plus declared schemas --------------

struct TransportPlugin;

static TRANSPORT_PLUGIN: TransportPlugin = TransportPlugin;

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

#[derive(Clone, Debug, Default)]
struct PatchReport {
    typed_local: Option<LocalSlot>,
    declared_schema: Option<TypeSchema>,
    call: Option<CallPatch>,
    gate: Option<GatePatch>,
    parent_parameters: Vec<LocalSlot>,
    target_declared: bool,
    other_declared: bool,
    continuation_rewritten: bool,
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
        let report = patch_ir(&mut ir, &resolver)?;
        *PATCH.lock().expect("typed resource patch report") = Some(report);
        Ok(ir)
    }
}

fn patch_error(message: impl Into<String>) -> ParseError {
    ParseError::at_line(1, message)
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
) -> Result<PatchReport, ParseError> {
    let mut report = PatchReport::default();
    let gate_mode = GATE_MODE.load(Ordering::SeqCst);
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
    let mut next_local = ir.locals;
    patch_stmts(&mut ir.stmts, gate.as_ref(), &mut next_local, &mut report)?;
    ir.locals = next_local;
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

fn patch_stmts(
    stmts: &mut [Stmt],
    gate: Option<&GateSpec>,
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
                            **expr = Expr::Bool(true);
                        }
                    }
                    Some(OTHER_LABEL) => {
                        *declared_schema = Some(table_callable_schema());
                        report.other_declared = true;
                    }
                    _ => {}
                }
                if closure.param_slots.len() == PARENT_PARAMS {
                    patch_parent(closure, gate, next_local, report)?;
                }
            }
            Stmt::Expr {
                expr: Expr::Block { stmts, .. },
                ..
            } => patch_stmts(stmts, gate, next_local, report)?,
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

fn patch_parent(
    closure: &mut ClosureExpr,
    gate: Option<&GateSpec>,
    next_local: &mut usize,
    report: &mut PatchReport,
) -> Result<(), ParseError> {
    let Expr::Block { stmts, expr: tail } = closure.body.as_mut() else {
        return Err(patch_error("the parent body is not a block"));
    };
    let parameters = closure.param_slots.clone();
    if parameters.len() != PARENT_PARAMS {
        return Err(patch_error("the parent parameter list changed"));
    }
    report.parent_parameters = parameters.clone();

    let mut typed_local = None;
    for stmt in stmts.iter_mut() {
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
        if typed_local.is_some() {
            return Err(patch_error(
                "the parent declares two typed candidate locals",
            ));
        }
        *declared_schema = Some(userdata_callable_schema());
        typed_local = Some(*index);
    }
    let Some(typed_local) = typed_local else {
        return Err(patch_error("the parent has no typed candidate local"));
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
    report.continuation_rewritten = true;

    if let Some(gate) = gate {
        let Some(position) = call_position else {
            return Err(patch_error("the parent has no admitted dynamic call"));
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
        stmts.insert(
            position,
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
    prototypes: Vec<vm::CallablePrototype>,
    host_imports: Vec<HostImport>,
    host_schemas: Vec<Option<HostImportSchema>>,
    local_schemas: Vec<Option<TypeSchema>>,
    fuel_consumed: u64,
    bound_functions: usize,
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

fn run_arm(arm: Arm) -> ArmRun {
    *PATCH.lock().expect("typed resource patch report") = None;
    SELECT.store(arm.select(), Ordering::SeqCst);
    GATE_MODE.store(arm.gate_mode(), Ordering::SeqCst);
    *WITNESS.lock().expect("typed resource witness") = Some(Witness::default());

    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&TRANSPORT_PLUGIN)
        .with_host_api_catalog(Arc::clone(transport_catalog()));
    let path = fixture_path(arm.fixture());
    let compiled = vm::compile_source_file_with_options(&path, options)
        .unwrap_or_else(|error| panic!("{}: compile failed: {error:?}", arm.label()));
    let patch = PATCH
        .lock()
        .expect("typed resource patch report")
        .clone()
        .unwrap_or_else(|| panic!("{}: the plugin produced no patch report", arm.label()));
    let call_sites = decoded_call_sites(&compiled.program);
    let host_call_sites = patch
        .gate
        .as_ref()
        .map(|gate| decoded_host_calls(&compiled.program, gate.import_index))
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

    let mut runtime = Vm::new_with_jit_config(
        compiled.program,
        JitConfig {
            enabled: false,
            hot_loop_threshold: 8,
            max_trace_len: 256,
        },
    );
    bind_hosts(&mut runtime)
        .unwrap_or_else(|error| panic!("{}: bind failed: {error}", arm.label()));
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
    let witness = WITNESS
        .lock()
        .expect("typed resource witness")
        .take()
        .unwrap_or_else(|| panic!("{}: the witness was not installed", arm.label()));

    let run = ArmRun {
        arm,
        status,
        stack,
        witness,
        patch,
        call_sites,
        host_call_sites,
        prototypes,
        host_imports,
        host_schemas,
        local_schemas,
        fuel_consumed,
        bound_functions,
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
        "{label}: all five exact static imports must bind"
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
