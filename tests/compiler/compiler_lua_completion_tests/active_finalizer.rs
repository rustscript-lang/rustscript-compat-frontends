//! Active Lua parent frame invoking the callable an exact typed host returns.
//!
//! One real `.lua` file (`tests/fixtures/luajit/finalizer/`) is fed through the
//! real lexer, parser, resolver and private completion lowering, compiled
//! through the fixed Core and run in one interpreter VM with JIT disabled.
//! Every Lua function and every host call in the fixture is real. The only
//! generated node is the typed local and the `Expr::LocalCall` target that the
//! production admission gate (`proof_for`) refuses to emit for a host-returned
//! callable: the plugin retargets the one dynamic call node the real lowering
//! produced for the admitted parameter onto the typed local that holds the
//! host-returned callable, and marks that local with an explicit
//! `TypeSchema::Callable`. Core typing, codegen and the VM then do the rest:
//! `CallValue(2)`, a real script frame, a real `Ret`, and the parent's resumed
//! return address.
//!
//! The host adapter is registered with `register_exact_static`, so the program
//! carries the catalog schema for every import and the returned callable is
//! checked by `validate_host_call_return`/`validate_callable_value` against the
//! compiled `CallablePrototype.schema` at run time. `register_exact_owned` is
//! not used: this pin refuses a return schema with a resource nested inside an
//! aggregate on that path, and no schema validation is disabled or downgraded
//! to `Unknown`.
//!
//! The private completion lowerer admits exactly four guest host-call names
//! (`mark`, `note`, `fault`, `tick`) and one arity per name, so the private test
//! catalog binds those names to the four adapters this witness needs. The
//! fixture comments name each adapter's real job; `mark` additionally returns
//! the integer the test staged for the arm and observes the active frame, and
//! `note` is the typed callable-returning adapter.
//!
//! Witnessed here:
//!
//! - the returned value is the exact `SharedCallable` the VM produced for the
//!   guest binding (`Arc::ptr_eq`, not schema equality);
//! - the receiver is the original issued Lua table, and a same-shaped ordinary
//!   array is not that table;
//! - the second argument is a real `resource<lua.userdata>` token: the Core
//!   resource table borrow revalidates arena, generation, concrete type and the
//!   exact `lua.userdata` key, the heap payload names the same userdata, and the
//!   same handle is refused as a `lua.table` token;
//! - the callee frame's declared parameter slots hold exactly those values;
//! - the call resumes the parent at the decoded `CallValue` successor and the
//!   parent continues with its own markers and its own returned value.
//!
//! Refuting controls, each a separate single `Vm::run` of the same file:
//!
//! - `OmitCall`: the typed host still runs and the typed local is still bound,
//!   but the call statement is absent from the executed IR, so no child frame
//!   and no callee marker exists;
//! - `OtherInstance`: the host returns another real Lua callable of the same
//!   declared two-parameter shape (a different chunk-level function with a
//!   different capture), so the call still runs and the original-instance
//!   assertion and the capture observation are both refuted;
//! - `OtherSignature`: the host returns a real one-parameter callable, so the
//!   typed return check refuses the value with the Core type error before any
//!   call runs;
//! - `OtherCapture` (attempted, refused before the call): the host really
//!   receives and selects `second`, the second instance of the one factory
//!   prototype that also produced `first`, so the two values are one nested
//!   prototype with two different captures. This pin compiles no callable
//!   schema for a nested closure prototype, so the exact typed return check
//!   refuses the selected instance with the Core callable type error and the
//!   guest call never happens. The host-selected same-prototype call is
//!   therefore *not* achieved: this arm records the pinned diagnostic and the
//!   witness does not claim that control.
//!
//! Not claimed here: a real finalizer dequeue, the pending-finalizer queue, a
//! source-level general closure bridge, close, collection, or a host-selected
//! call of a nested same-prototype instance (this pin refuses that typed
//! return). The heap is only used to issue one real table carrier and one real
//! userdata token; nothing is collected or closed.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use vm::compiler::HostCallResolver;
use vm::vm::VmFrameContinuation;
use vm::{
    CallOutcome, CallReturn, ClosureExpr, CompileSourceFileOptions, Expr, FrontendImportSyntax,
    FrontendIr, HostApiBuilder, HostApiCatalog, HostFunctionRegistry, HostFunctionSchema,
    HostImport, HostImportSchema, HostParamSchema, HostTypeSchema, JitConfig, LocalSlot,
    ParseError, ResourceTypeSchema, SourceError, SourceFlavor, SourcePathError, SourcePlugin, Stmt,
    TypeSchema, Value, Vm, VmError, VmStatus,
};

use super::{FUEL_BUDGET, FUEL_CHECK_INTERVAL, decoded_call_sites};
use crate::heap::{self, GuestClass, LuaTableResource, LuaUserdataResource, TableRef, UserdataRef};

/// The one fixture every arm executes.
const FIXTURE: &str = "active_typed_parent_call.lua";
/// The admission control that must stay refused at lowering.
const UNADMITTED_FIXTURE: &str = "active_typed_unadmitted.lua";
/// `parent(cb, first, second, single, other, recv, ud)`.
const PARENT_PARAMS: usize = 7;
/// `cb(recv, ud)`.
const CALL_ARGC: u8 = 2;
/// The admitted callee is the parent's first declared parameter.
const PARENT_CALLEE_SLOT: usize = 0;
/// The receiver and the userdata argument positions in `parent`.
const PARENT_RECEIVER_SLOT: usize = 5;
const PARENT_USERDATA_SLOT: usize = 6;

// ---- one run at a time ----------------------------------------------------

/// The exact observation of the last admitted run. This test module drives
/// every arm sequentially in one test body, so a process-wide slot holds the
/// same value a per-run handle would carry; no other test touches it.
static WITNESS: Mutex<Option<Witness>> = Mutex::new(None);

/// The integer the marker host returns and the fixture passes back as its
/// selection mode.
static SELECT: AtomicUsize = AtomicUsize::new(0);

/// The structural record the plugin produced for the last compile.
static PATCH: Mutex<Option<PatchReport>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arm {
    Positive,
    OmitCall,
    OtherInstance,
    OtherSignature,
    OtherCapture,
}

impl Arm {
    fn label(self) -> &'static str {
        match self {
            Arm::Positive => "positive",
            Arm::OmitCall => "omit-call",
            Arm::OtherInstance => "other-instance",
            Arm::OtherSignature => "other-signature",
            Arm::OtherCapture => "other-capture",
        }
    }

    fn select(self) -> usize {
        match self {
            Arm::Positive | Arm::OmitCall => 0,
            Arm::OtherInstance => 1,
            Arm::OtherSignature => 2,
            Arm::OtherCapture => 3,
        }
    }

    fn plugin(self) -> &'static dyn SourcePlugin {
        match self {
            Arm::OmitCall => &OMIT_PLUGIN,
            _ => &CALL_PLUGIN,
        }
    }
}

// ---- the private test catalog and exact registration ----------------------

fn typed_callable_host_schema() -> HostTypeSchema {
    HostTypeSchema::Callable {
        params: vec![HostTypeSchema::Unknown, HostTypeSchema::Unknown],
        result: Box::new(HostTypeSchema::Unknown),
    }
}

fn typed_callable_schema() -> TypeSchema {
    typed_callable_schema_with(usize::from(CALL_ARGC))
}

fn typed_callable_schema_with(parameters: usize) -> TypeSchema {
    TypeSchema::Callable {
        params: vec![TypeSchema::Unknown; parameters],
        result: Box::new(TypeSchema::Unknown),
    }
}

fn active_catalog() -> &'static Arc<HostApiCatalog> {
    static CATALOG: OnceLock<Arc<HostApiCatalog>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut builder = HostApiBuilder::new();
        // The marker channel. The label schema stays `Unknown` because a Lua
        // parameter has no static schema in this frontend and the callee
        // reports its capture through the label; the host body still requires
        // a real string and the return is the staged arm integer.
        builder.function(HostFunctionSchema::with_return(
            "mark",
            vec![HostParamSchema::value("label", HostTypeSchema::Unknown)],
            HostTypeSchema::Int,
        ));
        // The typed callable-returning adapter. This is the schema under test.
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
            typed_callable_host_schema(),
        ));
        // One freshly issued LuaHeap table carrier.
        builder.function(HostFunctionSchema::with_return(
            "tick",
            vec![HostParamSchema::value("label", HostTypeSchema::String)],
            HostTypeSchema::Unknown,
        ));
        // One freshly issued typed userdata token.
        builder.resource(ResourceTypeSchema::new(
            LuaUserdataResource::key(),
            "Lua userdata",
        ));
        builder.function(HostFunctionSchema::with_return(
            "fault",
            Vec::new(),
            HostTypeSchema::Resource(LuaUserdataResource::key()),
        ));
        Arc::new(builder.build().expect("active typed call catalog"))
    })
}

fn catalog_schema(name: &str) -> HostImportSchema {
    let catalog = active_catalog();
    let function = catalog
        .function(name)
        .unwrap_or_else(|| panic!("catalog is missing '{name}'"));
    HostImportSchema::from_function(catalog, function)
}

/// Binds every host the fixture calls through the exact static registration
/// path, so the compiled program and the VM share one catalog schema per
/// import and the typed return is validated at run time.
fn bind_hosts(vm: &mut Vm) -> Result<(), VmError> {
    let mut registry = HostFunctionRegistry::empty();
    registry.register_exact_static("mark", 1, catalog_schema("mark"), host_mark)?;
    registry.register_exact_static("note", 6, catalog_schema("note"), host_note)?;
    registry.register_exact_static("tick", 1, catalog_schema("tick"), host_tick)?;
    registry.register_exact_static("fault", 0, catalog_schema("fault"), host_fault)?;
    registry.bind_vm_cached(vm)
}

// ---- observation records --------------------------------------------------

#[derive(Clone, Debug)]
struct MarkRecord {
    label: String,
    frame_len: usize,
    last_prototype: Option<u32>,
    last_return_ip: Option<usize>,
    parent_prototype: Option<u32>,
    callee: Option<CalleeRecord>,
}

/// What one active two-parameter callee frame actually received.
#[derive(Clone, Debug)]
struct CalleeRecord {
    prototype_id: u32,
    local_base: usize,
    local_count: usize,
    parameter_slots: Vec<u16>,
    parameter_values: Vec<Option<Value>>,
    receiver_class: String,
    ordinary_array_class: String,
    token_parts: (u64, u32, u32),
    rebound_parts: (u64, u32, u32),
    table_token_refused: bool,
    caller_callable: Option<Value>,
    caller_declared_slot: Option<u16>,
}

#[derive(Clone, Debug)]
struct PickRecord {
    mode: i64,
    prototype_id: u32,
    first: Value,
    second: Value,
    other: Value,
    chosen: Value,
}

#[derive(Clone, Debug, Default)]
struct Witness {
    marks: Vec<MarkRecord>,
    picks: Vec<PickRecord>,
    issued_table: Option<Value>,
    issued_table_ref: Option<TableRef>,
    issued_userdata: Option<UserdataRef>,
    issued_userdata_raw: Option<i64>,
}

fn with_witness<T>(body: impl FnOnce(&mut Witness) -> T) -> T {
    let mut guard = WITNESS.lock().expect("active typed witness");
    let witness = guard
        .as_mut()
        .expect("the active typed witness must be installed before the run");
    body(witness)
}

fn host_fault_error(message: impl Into<String>) -> VmError {
    VmError::HostError(message.into())
}

fn heap_fault(error: heap::HeapError) -> VmError {
    VmError::HostError(format!("lua heap: {error:?}"))
}

// ---- the hosts ------------------------------------------------------------

/// Records one active-frame marker and returns the staged arm integer. In a
/// two-parameter callee frame it also records the declared formal slots, the
/// receiver class and revalidates the `lua.userdata` token those slots hold.
fn host_mark(vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
    let [Value::String(label)] = args else {
        return Err(host_fault_error("mark expects one string label"));
    };
    let label = label.as_str().to_string();
    let (frame_len, last_prototype, last_return_ip, parent_prototype, frame) = {
        let frames = vm.execution_frames();
        let program = vm.program();
        let locals = vm.locals();
        let parent = frames.iter().rev().nth(1);
        let frame = frames.last().and_then(|child| {
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
            let caller = frames.iter().rev().nth(1);
            let mut caller_callable = None;
            let mut caller_declared_slot = None;
            if let Some(caller) = caller
                && let Some(caller_prototype) = caller
                    .prototype_id
                    .and_then(|id| program.callable_prototypes.get(id as usize))
                && let Some(&slot) = caller_prototype.parameter_slots.first()
                && let Some(absolute) = caller.local_base.checked_add(usize::from(slot))
            {
                caller_declared_slot = Some(slot);
                caller_callable = locals.get(absolute).cloned();
            }
            Some((
                prototype_id,
                child.local_base,
                child.local_count,
                declared,
                values,
                caller_callable,
                caller_declared_slot,
            ))
        });
        (
            frames.len(),
            frames.last().and_then(|frame| frame.prototype_id),
            frames.last().and_then(|frame| match &frame.continuation {
                VmFrameContinuation::ResumeBytecode { return_ip } => Some(*return_ip),
                _ => None,
            }),
            parent.and_then(|frame| frame.prototype_id),
            frame,
        )
    };

    let mut record = MarkRecord {
        label,
        frame_len,
        last_prototype,
        last_return_ip,
        parent_prototype,
        callee: None,
    };
    if let Some((prototype_id, local_base, local_count, slots, values, caller, caller_slot)) = frame
    {
        let receiver = values
            .first()
            .cloned()
            .flatten()
            .ok_or_else(|| host_fault_error("the callee's first formal slot is empty"))?;
        let userdata = values
            .get(1)
            .cloned()
            .flatten()
            .ok_or_else(|| host_fault_error("the callee's second formal slot is empty"))?;
        let mut host = vm.host_context();
        let receiver_class = heap::with_heap(&mut host, |heap| Ok(heap.guest_class(&receiver)))
            .map_err(heap_fault)?;
        // A same-shaped ordinary array is data, not the issued table.
        let ordinary = Value::array(vec![Value::Int(0), Value::Int(0)]);
        let ordinary_class = heap::with_heap(&mut host, |heap| Ok(heap.guest_class(&ordinary)))
            .map_err(heap_fault)?;
        let Value::Int(raw) = userdata else {
            return Err(host_fault_error(format!(
                "the callee's userdata argument is not a handle token: {userdata:?}"
            )));
        };
        let raw = u64::try_from(raw)
            .map_err(|_| host_fault_error("the userdata handle token is not a positive integer"))?;
        let handle = vm::ResourceHandle::from_raw(raw).map_err(|error| {
            host_fault_error(format!("the userdata handle is invalid: {error}"))
        })?;
        let token_parts = {
            let borrowed = host
                .borrow_resource_with_key::<LuaUserdataResource>(
                    handle,
                    &LuaUserdataResource::key(),
                )
                .map_err(|error| {
                    host_fault_error(format!("the lua.userdata borrow was refused: {error}"))
                })?;
            let payload = borrowed.get().userdata();
            (payload.domain(), payload.slot(), payload.generation())
        };
        let rebound_parts = {
            let borrowed = host
                .borrow_resource_with_key::<LuaUserdataResource>(
                    handle,
                    &LuaUserdataResource::key(),
                )
                .map_err(|error| {
                    host_fault_error(format!(
                        "the second lua.userdata borrow was refused: {error}"
                    ))
                })?;
            let payload = borrowed.get().userdata();
            let rebound = heap::borrow_published_userdata(&host, payload).map_err(heap_fault)?;
            (rebound.domain(), rebound.slot(), rebound.generation())
        };
        let table_token_refused = host
            .borrow_resource_with_key::<LuaTableResource>(handle, &LuaTableResource::key())
            .is_err();
        record.callee = Some(CalleeRecord {
            prototype_id,
            local_base,
            local_count,
            parameter_slots: slots,
            parameter_values: values,
            receiver_class: format!("{receiver_class:?}"),
            ordinary_array_class: format!("{ordinary_class:?}"),
            token_parts,
            rebound_parts,
            table_token_refused,
            caller_callable: caller,
            caller_declared_slot: caller_slot,
        });
    }
    with_witness(|witness| witness.marks.push(record));
    let select = i64::try_from(SELECT.load(Ordering::SeqCst))
        .map_err(|_| host_fault_error("the selected arm does not fit a guest integer"))?;
    Ok(CallOutcome::Return(CallReturn::one(Value::Int(select))))
}

/// Returns one of the candidates the fixture already wrote at the call site.
/// The value is always the exact `SharedCallable` the VM produced for that
/// guest binding; the host never fabricates a callable.
fn host_note(_vm: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
    let [Value::Int(mode), target, first, second, single, other] = args else {
        return Err(host_fault_error(
            "note expects a mode and five written candidates",
        ));
    };
    let chosen = match *mode {
        0 => target,
        1 => other,
        2 => single,
        // The second instance of the same factory prototype that produced
        // `first`. The host really selects this captured sibling; whether the
        // exact typed return accepts it is the question the arm records.
        3 => second,
        other => {
            return Err(host_fault_error(format!(
                "note mode {other} is not a fixture arm"
            )));
        }
    };
    let Value::Callable(callable) = chosen else {
        return Err(host_fault_error(
            "the selected typed candidate is not a callable",
        ));
    };
    let record = PickRecord {
        mode: *mode,
        prototype_id: callable.prototype_id,
        first: first.clone(),
        second: second.clone(),
        other: other.clone(),
        chosen: chosen.clone(),
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
            "the issued table is not a live heap table",
        ));
    };
    // Keep the heap's own handle alive in the record: a still-live table is
    // what makes the carrier an issued Lua table rather than an array.
    with_witness(|witness| {
        witness.issued_table_ref = Some(table);
        witness.issued_table = Some(issued.clone());
    });
    Ok(CallOutcome::Return(CallReturn::one(issued)))
}

/// Issues one real typed `lua.userdata` token. The Core resource table
/// validates the returned carrier against the catalog key before the guest
/// sees it.
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

// ---- the private plugin: real lowering plus the one generated node --------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CallMode {
    /// Keep the call node and retarget it onto the typed local.
    Call,
    /// Drop the call node and its completion branch.
    Omit,
}

struct ActivePlugin {
    mode: CallMode,
    declared_parameters: usize,
}

static CALL_PLUGIN: ActivePlugin = ActivePlugin {
    mode: CallMode::Call,
    declared_parameters: 2,
};
static OMIT_PLUGIN: ActivePlugin = ActivePlugin {
    mode: CallMode::Omit,
    declared_parameters: 2,
};
static SCHEMA_PLUGIN: ActivePlugin = ActivePlugin {
    mode: CallMode::Call,
    declared_parameters: 3,
};

#[derive(Clone, Debug)]
struct CallPatch {
    completion_slot: LocalSlot,
    written_callee: LocalSlot,
    typed_callee: LocalSlot,
    argc: u8,
    arg_slots: Vec<LocalSlot>,
}

#[derive(Clone, Debug, Default)]
struct PatchReport {
    typed_local: Option<LocalSlot>,
    declared_schema: Option<TypeSchema>,
    call: Option<CallPatch>,
    omitted: bool,
    parent_parameters: Vec<LocalSlot>,
}

impl SourcePlugin for ActivePlugin {
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
        let resolver = HostCallResolver::new(active_catalog());
        let mut ir = super::lower::lower_source_with(source, Some(&resolver))?;
        let report = patch_ir(&mut ir, self.mode, self.declared_parameters)?;
        *PATCH.lock().expect("active typed patch report") = Some(report);
        Ok(ir)
    }
}

fn patch_error(message: impl Into<String>) -> ParseError {
    ParseError::at_line(1, message)
}

fn patch_ir(
    ir: &mut FrontendIr,
    mode: CallMode,
    declared_parameters: usize,
) -> Result<PatchReport, ParseError> {
    let mut report = PatchReport::default();
    patch_stmts(&mut ir.stmts, mode, declared_parameters, &mut report)?;
    Ok(report)
}

fn patch_stmts(
    stmts: &mut [Stmt],
    mode: CallMode,
    declared_parameters: usize,
    report: &mut PatchReport,
) -> Result<(), ParseError> {
    for stmt in stmts.iter_mut() {
        match stmt {
            Stmt::Let { expr, .. } | Stmt::Expr { expr, .. } => {
                patch_expr_closures(expr, mode, declared_parameters, report)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Descends through block expressions looking for Lua closure bindings. The
/// chunk's own statements live in one `Stmt::Expr` block, so this walk has to
/// start there rather than at the top-level statement list alone.
fn patch_expr_closures(
    expr: &mut Expr,
    mode: CallMode,
    declared_parameters: usize,
    report: &mut PatchReport,
) -> Result<(), ParseError> {
    match expr {
        Expr::Closure(closure) => patch_closure_body(closure, mode, declared_parameters, report),
        Expr::Block { stmts, expr } => {
            patch_stmts(stmts, mode, declared_parameters, report)?;
            patch_expr_closures(expr, mode, declared_parameters, report)
        }
        _ => Ok(()),
    }
}

fn patch_closure_body(
    closure: &mut ClosureExpr,
    mode: CallMode,
    declared_parameters: usize,
    report: &mut PatchReport,
) -> Result<(), ParseError> {
    let Expr::Block { stmts, expr: tail } = closure.body.as_mut() else {
        return Ok(());
    };
    if closure.param_slots.len() == PARENT_PARAMS {
        patch_parent(
            stmts,
            tail,
            closure.param_slots.clone(),
            mode,
            declared_parameters,
            report,
        )?;
    }
    for stmt in stmts.iter_mut() {
        let Stmt::Let { expr, .. } = stmt else {
            continue;
        };
        patch_expr_closures(expr, mode, declared_parameters, report)?;
    }
    Ok(())
}

fn is_slot_read(expr: &Expr, slot: LocalSlot) -> bool {
    matches!(expr, Expr::Var(read) if *read == slot)
}

fn reads_completion(expr: &Expr, completion: LocalSlot) -> bool {
    let Expr::Call(_, _, args, _, _) = expr else {
        return false;
    };
    matches!(args.as_slice(), [first, second] if is_slot_read(first, completion)
        && matches!(second, Expr::Int(0)))
}

fn patch_parent(
    stmts: &mut Vec<Stmt>,
    tail: &mut Expr,
    parameters: Vec<LocalSlot>,
    mode: CallMode,
    declared_parameters: usize,
    report: &mut PatchReport,
) -> Result<(), ParseError> {
    if parameters.len() != PARENT_PARAMS {
        return Err(patch_error("the parent parameter list changed"));
    }
    report.parent_parameters = parameters.clone();
    let typed_schema = typed_callable_schema_with(declared_parameters);

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
        *declared_schema = Some(typed_schema.clone());
        typed_local = Some(*index);
    }
    let Some(typed_local) = typed_local else {
        return Err(patch_error("the parent has no typed candidate local"));
    };
    report.typed_local = Some(typed_local);
    report.declared_schema = Some(typed_schema);

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
            || !is_slot_read(&args[1], parameters[PARENT_USERDATA_SLOT])
        {
            return Err(patch_error(
                "the admitted dynamic call does not pass the receiver and userdata slots",
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
                parameters[PARENT_USERDATA_SLOT],
            ],
        });
        call_position = Some((position, *completion));
    }

    match (mode, call_position) {
        (CallMode::Call, Some(_)) => Ok(()),
        (CallMode::Call, None) => Err(patch_error("the parent has no admitted dynamic call")),
        (CallMode::Omit, Some((position, completion))) => {
            stmts.remove(position);
            let replaced = std::mem::replace(tail, Expr::Null);
            let Expr::IfElse {
                condition,
                then_expr,
                ..
            } = replaced
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
            *tail = *then_expr;
            report.omitted = true;
            Ok(())
        }
        (CallMode::Omit, None) => Err(patch_error("the parent has no admitted dynamic call")),
    }
}

// ---- one arm --------------------------------------------------------------

struct ArmRun {
    arm: Arm,
    status: Result<VmStatus, VmError>,
    stack: Vec<Value>,
    witness: Witness,
    patch: PatchReport,
    call_sites: Vec<super::DecodedCallSite>,
    prototypes: Vec<vm::CallablePrototype>,
    host_imports: Vec<HostImport>,
    host_schemas: Vec<Option<HostImportSchema>>,
    local_schemas: Vec<Option<TypeSchema>>,
    fuel_consumed: u64,
}

fn fixture_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("luajit")
        .join("finalizer")
        .join(FIXTURE)
}

fn run_arm(arm: Arm) -> ArmRun {
    *PATCH.lock().expect("active typed patch report") = None;
    SELECT.store(arm.select(), Ordering::SeqCst);
    *WITNESS.lock().expect("active typed witness") = Some(Witness::default());

    let options = CompileSourceFileOptions::new()
        .with_source_plugin(arm.plugin())
        .with_host_api_catalog(Arc::clone(active_catalog()));
    let path = fixture_path();
    let compiled = vm::compile_source_file_with_options(&path, options)
        .unwrap_or_else(|error| panic!("{}: compile failed: {error:?}", arm.label()));
    let patch = PATCH
        .lock()
        .expect("active typed patch report")
        .clone()
        .unwrap_or_else(|| panic!("{}: the plugin produced no patch report", arm.label()));
    let call_sites = decoded_call_sites(&compiled.program);
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
    let witness = WITNESS
        .lock()
        .expect("active typed witness")
        .take()
        .unwrap_or_else(|| panic!("{}: the witness was not installed", arm.label()));

    let run = ArmRun {
        arm,
        status,
        stack,
        witness,
        patch,
        call_sites,
        prototypes,
        host_imports,
        host_schemas,
        local_schemas,
        fuel_consumed,
    };
    run.report();
    run
}

impl ArmRun {
    fn report(&self) {
        eprintln!(
            "LUA_ACTIVE_TYPED arm={} fuel={} status={:?} exit={:?} patch={:?}",
            self.arm.label(),
            self.fuel_consumed,
            self.status,
            self.exit(),
            self.patch
        );
        eprintln!(
            "LUA_ACTIVE_TYPED arm={} marks={:?}",
            self.arm.label(),
            self.witness
                .marks
                .iter()
                .map(|mark| (mark.label.as_str(), mark.frame_len, mark.last_prototype))
                .collect::<Vec<_>>()
        );
        for mark in &self.witness.marks {
            let Some(callee) = &mark.callee else {
                continue;
            };
            eprintln!(
                "LUA_ACTIVE_TYPED arm={} callee label={} prototype={} slots={:?} values={:?} receiver={} ordinary={} token={:?} rebound={:?} table_refused={} caller_slot={:?} caller_callable={:?}",
                self.arm.label(),
                mark.label,
                callee.prototype_id,
                callee.parameter_slots,
                callee.parameter_values,
                callee.receiver_class,
                callee.ordinary_array_class,
                callee.token_parts,
                callee.rebound_parts,
                callee.table_token_refused,
                callee.caller_declared_slot,
                callee.caller_callable,
            );
        }
        for pick in &self.witness.picks {
            eprintln!(
                "LUA_ACTIVE_TYPED arm={} pick mode={} prototype={}",
                self.arm.label(),
                pick.mode,
                pick.prototype_id
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

    /// The one marker recorded inside a two-parameter callee frame.
    fn callee_mark(&self) -> &MarkRecord {
        let mut marked = self
            .witness
            .marks
            .iter()
            .filter(|mark| mark.callee.is_some());
        let first = marked
            .next()
            .unwrap_or_else(|| panic!("{}: no callee frame was observed", self.arm.label()));
        assert!(
            marked.next().is_none(),
            "{}: more than one callee frame was observed",
            self.arm.label()
        );
        first
    }

    fn callee(&self) -> &CalleeRecord {
        self.callee_mark()
            .callee
            .as_ref()
            .expect("the callee mark carries its record")
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
            typed_callable_host_schema(),
            "{}: the program must carry the full callable return schema",
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
    }

    fn assert_typed_local(&self) {
        let slot = self
            .patch
            .typed_local
            .unwrap_or_else(|| panic!("{}: no typed local", self.arm.label()));
        assert_eq!(
            self.patch.declared_schema,
            Some(typed_callable_schema()),
            "{}: the generated typed local must declare the callable schema",
            self.arm.label()
        );
        assert!(
            self.patch.call.is_some() || self.patch.omitted,
            "{}: the typed local must be the one the call node uses",
            self.arm.label()
        );
        // This pin records a compiled local schema only for the slots it walks
        // as statement bindings, so the annotation generated inside `parent` is
        // proven by the retargeted `CallValue` instead. The typed resource local
        // is recorded, and it is the independent compile-time check that the
        // userdata carrier is a real `lua.userdata` value.
        let resource_locals: Vec<usize> = self
            .local_schemas
            .iter()
            .enumerate()
            .filter(|(_, schema)| {
                schema.as_ref() == Some(&TypeSchema::Resource(LuaUserdataResource::key()))
            })
            .map(|(slot, _)| slot)
            .collect();
        assert_eq!(
            resource_locals.len(),
            1,
            "{}: exactly one local must carry the typed userdata schema, saw {resource_locals:?}",
            self.arm.label()
        );
        eprintln!(
            "LUA_ACTIVE_TYPED arm={} typed_local={slot} resource_locals={resource_locals:?}",
            self.arm.label()
        );
    }

    fn prototype(&self, id: u32) -> &vm::CallablePrototype {
        self.prototypes
            .get(id as usize)
            .unwrap_or_else(|| panic!("{}: prototype {id} is missing", self.arm.label()))
    }

    fn assert_two_parameter_script_prototype(&self, id: u32) {
        let prototype = self.prototype(id);
        assert_eq!(
            prototype.arity,
            CALL_ARGC,
            "{}: the callable must be a two-parameter Lua function",
            self.arm.label()
        );
        assert_eq!(
            prototype.schema,
            Some(typed_callable_schema()),
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
    let callee = run.callee();
    assert_eq!(
        callee.ordinary_array_class,
        "Unissued",
        "{}: an ordinary array must not be the issued table",
        run.arm.label()
    );
    assert!(
        callee.receiver_class.starts_with("Table("),
        "{}: the receiver must be the issued heap table, saw {}",
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
        .unwrap_or_else(|| panic!("{}: no issued table", run.arm.label()));
    assert!(
        same_array(
            &callee.parameter_values[0].clone().expect("receiver slot"),
            issued_table
        ),
        "{}: the callee must receive the original issued table carrier",
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
        callee.token_parts,
        issued_parts,
        "{}: the Core borrow must name the issued userdata",
        run.arm.label()
    );
    assert_eq!(
        callee.rebound_parts,
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
        "{}: the callee must receive the issued token carrier",
        run.arm.label()
    );
}

fn assert_formal_slots(run: &ArmRun) {
    let callee = run.callee();
    let child_prototype = run.prototype(callee.prototype_id);
    assert_eq!(
        callee.parameter_slots,
        child_prototype.parameter_slots,
        "{}: the witness must read the declared callee slots",
        run.arm.label()
    );
    assert_eq!(
        callee.parameter_values.len(),
        usize::from(CALL_ARGC),
        "{}: the callee must declare two formal slots",
        run.arm.label()
    );
    assert!(
        callee.parameter_values[0]
            .as_ref()
            .is_some_and(|value| same_array(
                value,
                run.witness.issued_table.as_ref().expect("issued table")
            )),
        "{}: the first formal slot must hold the receiver carrier",
        run.arm.label()
    );
    assert!(
        callee.local_count > 0,
        "{}: the callee frame must be non-empty",
        run.arm.label()
    );
    assert!(
        callee.local_base > 0,
        "{}: the callee frame must sit above the chunk frame",
        run.arm.label()
    );
}

// ---- the arms -------------------------------------------------------------

#[test]
fn active_parent_invokes_typed_host_returned_callable() {
    let positive = run_arm(Arm::Positive);
    assert_positive(&positive);

    let omitted = run_arm(Arm::OmitCall);
    assert_omit_control(&positive, &omitted);

    let other = run_arm(Arm::OtherInstance);
    assert_other_instance_control(&positive, &other);

    let signature = run_arm(Arm::OtherSignature);
    assert_signature_control(&signature);

    let capture = run_arm(Arm::OtherCapture);
    assert_same_prototype_capture_control(&capture);

    assert_declared_schema_is_checked();
    assert_unadmitted_call_is_refused();
}

/// The real lowerer still refuses a dynamic call whose callee is a local bound
/// from a host return. This is why the witness generates one typed local and
/// one call node instead of relaxing the admission rule, and it is a real
/// compile-layer refusal of the same catalog and the same plugin.
fn assert_unadmitted_call_is_refused() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("luajit")
        .join("finalizer")
        .join(UNADMITTED_FIXTURE);
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&CALL_PLUGIN)
        .with_host_api_catalog(Arc::clone(active_catalog()));
    let error = vm::compile_source_file_with_options(&path, options)
        .err()
        .unwrap_or_else(|| panic!("the unadmitted dynamic call must be refused at lowering"));
    let detail = match &error {
        SourcePathError::SourceWithMap {
            error: SourceError::Parse(parse),
            ..
        }
        | SourcePathError::Source(SourceError::Parse(parse)) => parse.message.clone(),
        other => panic!("unadmitted: expected a lowering refusal, got {other:?}"),
    };
    eprintln!("LUA_ACTIVE_TYPED arm=unadmitted refusal={detail}");
    assert!(
        detail.contains("not a Lua function binding or admitted parameter"),
        "unadmitted: expected the Lua-provenance refusal, got {detail}"
    );
}

/// The generated typed local is not decoration: Core typing checks the
/// declared callable schema against the value the host returns and validates
/// the retargeted call's argument list against it. Declaring three parameters
/// for the same node is refused at compile time, before any VM exists.
fn assert_declared_schema_is_checked() {
    *PATCH.lock().expect("active typed patch report") = None;
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&SCHEMA_PLUGIN)
        .with_host_api_catalog(Arc::clone(active_catalog()));
    let path = fixture_path();
    let error = vm::compile_source_file_with_options(&path, options)
        .err()
        .unwrap_or_else(|| {
            panic!("a three-parameter typed local must refuse the two-parameter callable")
        });
    let detail = match &error {
        SourcePathError::SourceWithMap {
            error: SourceError::Compile(compile),
            ..
        }
        | SourcePathError::Source(SourceError::Compile(compile)) => format!("{compile:?}"),
        other => panic!("schema-mismatch: expected a compile-layer refusal, got {other:?}"),
    };
    eprintln!("LUA_ACTIVE_TYPED arm=schema-mismatch compile-error={detail}");
    assert!(
        detail.contains("declared as schema type"),
        "schema-mismatch: expected the declared-schema refusal, got {detail}"
    );
    let patch = PATCH
        .lock()
        .expect("active typed patch report")
        .clone()
        .unwrap_or_else(|| panic!("schema-mismatch: the plugin produced no patch report"));
    assert_eq!(
        patch.declared_schema,
        Some(typed_callable_schema_with(3)),
        "schema-mismatch: the plugin must have declared the three-parameter schema"
    );
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
        ["parent-enter", "mode", "parent-tag", "parent-after", "done"],
        "positive: the callee must run between the parent's own markers"
    );
    let values = run.exit().unwrap_or_else(|| {
        panic!(
            "positive: expected the chunk completion, got {:?}",
            run.status
        )
    });
    assert_eq!(values.len(), 8, "positive: [true] plus seven values");
    let selected = &values[1];
    let decoy = &values[2];
    let other = &values[3];
    let first = &values[4];
    let second = &values[5];
    let receiver = &values[6];
    let userdata = &values[7];
    assert!(
        same_callable(other, &run.pick().other),
        "positive: 'other' is the guest control instance"
    );
    assert!(
        !same_callable(other, decoy),
        "positive: the two chunk-level functions are distinct instances"
    );
    assert!(
        same_callable(selected, decoy),
        "positive: the typed host must return the original guest callable"
    );
    assert!(
        same_callable(first, &run.pick().first),
        "positive: 'first' is the guest instance"
    );
    assert!(
        same_callable(second, &run.pick().second),
        "positive: 'second' is the guest instance"
    );
    assert!(
        !same_callable(first, second),
        "positive: the two written instances must stay distinct"
    );
    assert_eq!(
        callable_id(first),
        callable_id(second),
        "positive: both nested instances come from one prototype"
    );
    // This pin records a compiled callable schema for the chunk-level function
    // bindings only. The nested `callback` prototype therefore carries none and
    // can never be handed through a typed callable return: that is a compiled
    // fact of this pin, recorded here instead of assumed.
    assert_eq!(
        run.prototype(callable_id(first)).schema,
        None,
        "positive: a nested closure prototype carries no compiled callable schema"
    );

    // The executed callee is the value the host returned, and it is the same
    // Arc the parent's own first parameter slot still holds.
    let executed = run.callee().prototype_id;
    assert_eq!(
        callable_id(selected),
        executed,
        "positive: the executed frame must be the returned callable"
    );
    let held = run
        .callee()
        .caller_callable
        .as_ref()
        .unwrap_or_else(|| panic!("positive: the parent's first parameter is empty"));
    assert!(
        same_callable(held, selected),
        "positive: the parent's held argument must be the returned callable"
    );
    let held_slot = run
        .mark("parent-enter")
        .last_prototype
        .and_then(|id| run.prototype(id).parameter_slots.first().copied());
    assert_eq!(
        run.callee().caller_declared_slot,
        held_slot,
        "positive: the held argument is the parent's first declared slot"
    );

    assert_receiver_and_token(run);
    assert_formal_slots(run);
    run.assert_two_parameter_script_prototype(executed);
    let (return_ip, caller) = assert_resumed_call_site(run, "parent-tag");
    assert!(return_ip > 0, "positive: the resume address is real");
    assert_eq!(
        run.mark("parent-enter").last_prototype,
        Some(caller),
        "positive: the parent's own marker names the caller prototype"
    );
    assert_eq!(
        run.mark("parent-enter").frame_len + 1,
        run.mark("parent-tag").frame_len,
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
    let call = run
        .patch
        .call
        .as_ref()
        .unwrap_or_else(|| panic!("positive: the call node was not patched"));
    assert_eq!(call.argc, CALL_ARGC);
    assert_ne!(
        call.written_callee, call.typed_callee,
        "positive: the callee slot must be retargeted"
    );
    assert_eq!(
        call.arg_slots,
        vec![
            run.patch.parent_parameters[PARENT_RECEIVER_SLOT],
            run.patch.parent_parameters[PARENT_USERDATA_SLOT],
        ],
        "positive: the call still passes the declared receiver and userdata slots"
    );
    assert_ne!(
        call.completion_slot, call.typed_callee,
        "positive: the completion slot is the call result, not the callee"
    );
    assert!(
        !run.patch.omitted,
        "positive: the call statement must be present"
    );
    assert!(
        same_array(
            receiver,
            &run.witness.issued_table.clone().expect("issued table")
        ),
        "positive: the chunk returns the issued table carrier"
    );
    assert_eq!(
        userdata,
        &Value::Int(
            run.witness
                .issued_userdata_raw
                .expect("the fixture issued one userdata")
        ),
        "positive: the chunk returns the issued token"
    );
}

fn assert_omit_control(positive: &ArmRun, omitted: &ArmRun) {
    omitted.assert_fuel();
    omitted.assert_typed_import();
    omitted.assert_typed_local();
    assert!(
        matches!(omitted.status, Ok(VmStatus::Halted)),
        "omit: the parent still runs to a normal halt, got {:?}",
        omitted.status
    );
    assert!(
        omitted.patch.omitted,
        "omit: the call statement must be omitted"
    );
    assert_eq!(
        omitted.labels(),
        ["parent-enter", "mode", "parent-after", "done"],
        "omit: no callee marker may appear while the typed getter still ran"
    );
    assert_eq!(
        omitted.witness.picks.len(),
        1,
        "omit: the typed host still ran exactly once"
    );
    assert!(
        omitted
            .witness
            .marks
            .iter()
            .all(|mark| mark.callee.is_none()),
        "omit: no callee frame may observe the arguments"
    );
    let values = omitted
        .exit()
        .unwrap_or_else(|| panic!("omit: expected the chunk completion"));
    assert_eq!(values.len(), 8, "omit: the same chunk shape");
    assert!(
        same_callable(&values[1], &values[2]),
        "omit: the typed local still holds the original guest callable"
    );
    assert_eq!(
        positive.patch.call.as_ref().map(|call| call.typed_callee),
        omitted.patch.typed_local,
        "omit and positive must bind the same typed local"
    );
    assert_eq!(
        omitted.patch.call.as_ref().map(|call| call.argc),
        Some(CALL_ARGC),
        "omit: the omitted node was the same two-argument call"
    );
}

fn assert_other_instance_control(positive: &ArmRun, other: &ArmRun) {
    other.assert_fuel();
    other.assert_typed_import();
    other.assert_typed_local();
    assert!(
        matches!(other.status, Ok(VmStatus::Halted)),
        "other-instance: the call still runs to a normal halt, got {:?}",
        other.status
    );
    assert_eq!(
        other.labels(),
        ["parent-enter", "mode", "other-tag", "parent-after", "done"],
        "other-instance: the callee frame still runs and reports its own capture"
    );
    let values = other
        .exit()
        .unwrap_or_else(|| panic!("other-instance: expected the chunk completion"));
    let selected = &values[1];
    let decoy = &values[2];
    let written_other = &values[3];
    assert!(
        !same_callable(selected, decoy),
        "other-instance: another real callable must not pass the original-instance assertion"
    );
    assert!(
        same_callable(selected, written_other),
        "other-instance: the host returned the written control instance"
    );
    assert_eq!(
        other.callee().prototype_id,
        callable_id(selected),
        "other-instance: the executed callee is the returned instance"
    );
    // Both chunk-level functions carry the same compiled callable schema and
    // arity, so the return check accepts both and only pointer identity
    // separates them.
    other.assert_two_parameter_script_prototype(callable_id(selected));
    other.assert_two_parameter_script_prototype(callable_id(decoy));
    assert_ne!(
        callable_id(selected),
        callable_id(decoy),
        "other-instance: the two accepted callables are different prototypes"
    );
    assert_eq!(
        other.callee_mark().label,
        "other-tag",
        "other-instance: the capture of the selected instance must be observed"
    );
    assert_eq!(
        positive.callee_mark().label,
        "parent-tag",
        "positive: the capture of the original instance must be observed"
    );
    assert_ne!(
        positive.callee_mark().label,
        other.callee_mark().label,
        "the two accepted instances must differ by captured value"
    );
    assert!(
        !same_callable(selected, &positive.pick().chosen),
        "other-instance: the selection must differ from the positive selection"
    );
    assert_eq!(
        other.callee().parameter_values.len(),
        usize::from(CALL_ARGC),
        "other-instance: the same two formal slots receive the arguments"
    );
    assert_receiver_and_token(other);
}

fn assert_signature_control(run: &ArmRun) {
    run.assert_fuel();
    run.assert_typed_import();
    run.assert_typed_local();
    match &run.status {
        Err(VmError::TypeMismatch(kind)) => assert_eq!(
            *kind, "callable",
            "other-signature: the callable return check must refuse the value"
        ),
        other => {
            panic!("other-signature: expected the Core callable type refusal, got {other:?}")
        }
    }
    assert!(
        run.witness.marks.iter().all(|mark| mark.callee.is_none()),
        "other-signature: no callee frame may run"
    );
    assert_eq!(
        run.labels(),
        ["parent-enter", "mode"],
        "other-signature: the parent stops at the typed host return"
    );
    let pick = run.pick();
    assert_eq!(
        pick.prototype_id,
        callable_id(&pick.chosen),
        "other-signature: the refused value is the written single-parameter instance"
    );
    assert_eq!(
        run.prototype(pick.prototype_id).arity,
        1,
        "other-signature: the refused callable has one parameter"
    );
}

/// The host actually selects the other capture of the factory pair. `first`
/// and `second` are two instances of `make`'s one nested `callback` prototype,
/// so the selected value is a real sibling instance, not the chunk-level
/// `decoy` or `other` control. This pin attaches no compiled callable schema to
/// a nested closure prototype, so the exact typed return check refuses the
/// selected instance with the Core callable type error and the guest call never
/// starts. The host-selected same-prototype call is therefore not achieved
/// here; the arm records the pinned diagnostic instead of presenting the pair
/// as an executed control.
fn assert_same_prototype_capture_control(run: &ArmRun) {
    run.assert_fuel();
    run.assert_typed_import();
    run.assert_typed_local();

    // The host was handed both factory instances and really selected `second`.
    let pick = run.pick();
    assert_eq!(
        pick.mode, 3,
        "other-capture: the staged arm must select the factory sibling"
    );
    assert!(
        same_callable(&pick.chosen, &pick.second),
        "other-capture: the host must select the second factory instance itself"
    );
    assert!(
        !same_callable(&pick.chosen, &pick.first),
        "other-capture: the selected value must be the other capture of the pair"
    );
    assert!(
        !same_callable(&pick.first, &pick.second),
        "other-capture: the two factory instances must stay distinct Arcs"
    );
    assert_eq!(
        callable_id(&pick.first),
        callable_id(&pick.second),
        "other-capture: both factory instances must share one nested prototype"
    );

    // The pinned compile-time fact that refuses the typed return: the nested
    // prototype is a real two-parameter script function with no compiled
    // callable schema.
    let prototype = run.prototype(pick.prototype_id);
    assert_eq!(
        prototype.arity, CALL_ARGC,
        "other-capture: the refusal must not be an arity mismatch"
    );
    assert_eq!(
        prototype.schema, None,
        "other-capture: this pin attaches no callable schema to a nested closure prototype"
    );
    assert!(
        matches!(prototype.target, vm::CallableTarget::ScriptFunction(_)),
        "other-capture: the selected instance must still be a real Lua function"
    );

    // The selected instance never becomes guest-visible: the exact typed return
    // check refuses it at the host boundary, so no call node of this run is
    // executed with it.
    match &run.status {
        Err(VmError::TypeMismatch(kind)) => assert_eq!(
            *kind, "callable",
            "other-capture: the typed return check must refuse the nested callable"
        ),
        other => {
            panic!("other-capture: expected the Core callable type refusal, got {other:?}")
        }
    }
    assert_eq!(
        run.labels(),
        ["parent-enter", "mode"],
        "other-capture: the parent must stop at the typed host return"
    );
    assert!(
        run.witness.marks.iter().all(|mark| mark.callee.is_none()),
        "other-capture: no callee frame may run for the refused instance"
    );
    assert!(
        run.exit().is_none(),
        "other-capture: the failed run must have no chunk completion"
    );
    eprintln!(
        "LUA_ACTIVE_TYPED arm=other-capture control=not-achieved same_prototype={} chosen_is_second=1 refusal=TypeMismatch(callable)",
        callable_id(&pick.chosen)
    );
}
