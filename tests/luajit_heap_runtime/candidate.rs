//! Executable slice of the AST lowering. The production file entry does not
//! select it until the supported execution and root-transfer gates pass.

use std::collections::{HashMap, HashSet, VecDeque};
use std::num::NonZeroU64;
use vm::compiler::ir::{FunctionDecl, FunctionImpl, ResolvedHostCall, ResolvedHostParam};
use vm::{
    AssignmentKind, CallOutcome, CallReturn, Expr, FrontendIr, HostApiBuilder, HostApiCatalog,
    HostFunctionRegistry, HostFunctionSchema, HostImportSchema, HostOwnedFunction, HostParamSchema,
    HostTypeSchema, LocalIrBuilder, LocalSlot, OwnedHostCall, ParseError, Stmt, TypeSchema, Value,
    VmError,
};

use super::{ast, heap, parser, resolve};

const NEW_TABLE: &str = "lua_internal::new_table";
const TYPE: &str = "lua_internal::type_of";
const ADD: &str = "lua_internal::add";
pub(super) const STRING_BYTE_LEN: &str = "lua_internal::string_byte_len";
const RAW_LEN: &str = "lua_internal::raw_len";
const RETURN_SCALAR: &str = "lua_internal::return_scalar";

const TABLE_ARG: &str = "lua_internal::table_arg";
const NUMBER_ARG: &str = "lua_internal::number_arg";
const BOOL_ARG: &str = "lua_internal::bool_arg";
const STRING_ARG: &str = "lua_internal::string_arg";
pub(super) const GATHER_ROOT: &str = "lua_internal::gather_root";
pub(super) const COLLECT: &str = "lua_internal::collect";
const STEP: &str = "lua_internal::step";
const STEP_HAS_TAIL: &str = "lua_internal::step_has_tail";
const GC_STOP: &str = "lua_internal::gc_stop";
const GC_RESTART: &str = "lua_internal::gc_restart";
const GC_IS_RUNNING: &str = "lua_internal::gc_is_running";
const STEP_DATA: &str = "lua_internal::step_data";
pub(super) const AUTO_TABLE_DUE: &str = "lua_internal::auto_table_due";
pub(super) const AUTO_TABLE_RESTART: &str = "lua_internal::auto_table_restart";
const COUNT: &str = "lua_internal::count";
const OPEN_CELL: &str = "lua_internal::open_cell";
const READ_CELL: &str = "lua_internal::read_cell";
const WRITE_CELL: &str = "lua_internal::write_cell";
const TRACK_CLOSURE: &str = "lua_internal::track_closure";
const ENTER_RESULT: &str = "lua_internal::enter_result_frame";
const ENTER_CALLEE: &str = "lua_internal::enter_callee_frame";
const ENTER_PENDING: &str = "lua_internal::enter_pending_frame";
const WRITE_PENDING: &str = "lua_internal::write_pending_root";
const WRITE_OPTIONAL_PENDING: &str = "lua_internal::write_optional_pending_root";
const LEAVE_PENDING: &str = "lua_internal::leave_pending_frame";
const ROOT_ISSUED: &str = "lua_internal::root_issued";
const SET_EDGE: &str = "lua_internal::set_edge";
const GET_EDGE: &str = "lua_internal::get_edge";
const NEXT: &str = "lua_internal::next";
const NOTE_NEXT_COUNT: &str = "lua_internal::note_next_count";
const SET_METATABLE: &str = "lua_internal::set_metatable";
const GET_METATABLE: &str = "lua_internal::get_metatable";
pub(super) const FINISH_RETURN: &str = "lua_internal::finish_return";
pub(super) const RETIRE_CALLEE: &str = "lua_internal::retire_callee";
pub(super) const RETIRE_CHUNK: &str = "lua_internal::retire_chunk";
/// Name of the test-only script callable that wraps one already-lowered chunk.
///
/// The inner Lua function stays unexported. Exporting that callee directly would
/// skip the caller's `FINISH_RETURN` / `RETIRE_CALLEE` window.
pub(super) const EXPORTED_CHUNK: &str = "lua_chunk_entry";
const RELEASE_RESULT: &str = "lua_internal::release_result";
// Test-only witnesses. They place an ordinary VM array on the source path.
// They are not Lua syntax and are not registered on the production entry.
const COPY_FIELDS: &str = "lua_internal::copy_fields";
const EXTERNAL_SHAPE: &str = "lua_internal::external_shape";
const BYTE_TEXT: &str = "lua_internal::byte_text";
const HOST_INT: &str = "lua_internal::host_int";
const RAW_EQUAL: &str = "lua_internal::raw_equal";

/// Lengths the IR consumer read from `next`'s return transport with Core `len`.
///
/// Source has no `select`. These lengths are not a source multi-return count
/// and are not the host array length recorded by `next` itself.
#[derive(Clone, Debug, Default)]
pub(super) struct NextCounts {
    pub consumed: Vec<u32>,
}

pub(super) fn next_counts(vm: &mut vm::Vm) -> Vec<u32> {
    vm.host_context()
        .scope_state::<NextCounts>()
        .map(|counts| counts.consumed.clone())
        .unwrap_or_default()
}

#[derive(Default)]
struct StepBudgets(VecDeque<NonZeroU64>);

pub(super) fn set_step_budgets(host: &mut vm::HostContext<'_>, budgets: &[NonZeroU64]) {
    host.scope_state_or_insert_with(StepBudgets::default)
        .expect("test step budget state")
        .0 = budgets.iter().copied().collect();
}

/// Remaining fixture budgets. This read does not pop, step, or gather.
pub(super) fn remaining_step_budgets(host: &mut vm::HostContext<'_>) -> Vec<u64> {
    host.scope_state::<StepBudgets>()
        .map(|budgets| budgets.0.iter().map(|budget| budget.get()).collect())
        .unwrap_or_default()
}

/// Remaining fixture allowances, oldest first. Does not pop, install, or step.
pub(super) fn step_budget_snapshot(host: &vm::HostContext<'_>) -> Vec<u64> {
    host.scope_state::<StepBudgets>()
        .map(|budgets| budgets.0.iter().map(|budget| budget.get()).collect())
        .unwrap_or_default()
}

/// Opt in to the fixture-budgeted guest step in this catalog only.
pub(super) fn install_step_schemas(builder: &mut HostApiBuilder) {
    builder.function(HostFunctionSchema::with_return(
        STEP_HAS_TAIL,
        vec![],
        HostTypeSchema::Bool,
    ));
    builder.function(HostFunctionSchema::with_return(
        STEP,
        vec![HostParamSchema::value("roots", HostTypeSchema::Unknown)],
        HostTypeSchema::Bool,
    ));
    builder.function(HostFunctionSchema::with_return(
        STEP_DATA,
        vec![HostParamSchema::value("data", HostTypeSchema::Unknown)],
        HostTypeSchema::Bool,
    ));
}

/// Opt in to guest `stop` / `restart` / `isrunning` in this catalog only.
/// The switch lives in the VM execution scope, not in this catalog.
pub(super) fn install_run_control_schemas(builder: &mut HostApiBuilder) {
    builder.function(HostFunctionSchema::with_return(
        GC_STOP,
        vec![],
        HostTypeSchema::Float,
    ));
    builder.function(HostFunctionSchema::with_return(
        GC_RESTART,
        vec![],
        HostTypeSchema::Float,
    ));
    builder.function(HostFunctionSchema::with_return(
        GC_IS_RUNNING,
        vec![],
        HostTypeSchema::Bool,
    ));
}

/// Opt in to one automatic empty-local safepoint. Not part of the ordinary
/// catalog or the run-control catalog. Due is a read-only observation; the
/// restart entry is the only writer. Either schema alone does not connect the
/// source route.
pub(super) fn install_auto_table_due_schema(builder: &mut HostApiBuilder) {
    builder.function(HostFunctionSchema::with_return(
        AUTO_TABLE_DUE,
        vec![],
        HostTypeSchema::Bool,
    ));
}

pub(super) fn install_auto_table_restart_entry_schema(builder: &mut HostApiBuilder) {
    builder.function(HostFunctionSchema::with_return(
        AUTO_TABLE_RESTART,
        vec![
            HostParamSchema::value("roots", HostTypeSchema::Unknown),
            HostParamSchema::value("handoff", HostTypeSchema::Float),
        ],
        HostTypeSchema::Float,
    ));
}

pub(super) fn install_auto_table_restart_schema(builder: &mut HostApiBuilder) {
    install_auto_table_due_schema(builder);
    install_auto_table_restart_entry_schema(builder);
}

pub(super) fn install_schemas(builder: &mut HostApiBuilder) {
    builder.function(HostFunctionSchema::with_return(
        GATHER_ROOT,
        vec![
            HostParamSchema::value("roots", HostTypeSchema::Unknown),
            HostParamSchema::value("value", HostTypeSchema::Unknown),
        ],
        HostTypeSchema::Unknown,
    ));
    let key = heap::LuaTableResource::key();
    builder.resource(vm::ResourceTypeSchema::new(key.clone(), "Lua table"));
    builder.function(HostFunctionSchema::with_return(
        NEW_TABLE,
        vec![],
        HostTypeSchema::Unknown,
    ));
    builder.function(HostFunctionSchema::with_return(
        COUNT,
        vec![],
        HostTypeSchema::Float,
    ));
    builder.function(HostFunctionSchema::with_return(
        COPY_FIELDS,
        vec![HostParamSchema::value("value", HostTypeSchema::Unknown)],
        HostTypeSchema::Unknown,
    ));
    builder.function(HostFunctionSchema::with_return(
        EXTERNAL_SHAPE,
        vec![],
        HostTypeSchema::Unknown,
    ));
    builder.function(HostFunctionSchema::with_return(
        BYTE_TEXT,
        vec![],
        HostTypeSchema::Unknown,
    ));
    builder.function(HostFunctionSchema::with_return(
        HOST_INT,
        vec![],
        HostTypeSchema::Unknown,
    ));
    builder.function(HostFunctionSchema::with_return(
        RAW_EQUAL,
        vec![
            HostParamSchema::value("left", HostTypeSchema::Unknown),
            HostParamSchema::value("right", HostTypeSchema::Unknown),
        ],
        HostTypeSchema::Bool,
    ));
    builder.function(HostFunctionSchema::with_return(
        TYPE,
        vec![HostParamSchema::value("value", HostTypeSchema::Unknown)],
        HostTypeSchema::String,
    ));
    builder.function(HostFunctionSchema::with_return(
        ADD,
        vec![
            HostParamSchema::value("left", HostTypeSchema::Unknown),
            HostParamSchema::value("right", HostTypeSchema::Unknown),
        ],
        HostTypeSchema::Float,
    ));
    builder.function(HostFunctionSchema::with_return(
        STRING_BYTE_LEN,
        vec![HostParamSchema::value("value", HostTypeSchema::Unknown)],
        HostTypeSchema::Float,
    ));
    builder.function(HostFunctionSchema::with_return(
        RAW_LEN,
        vec![HostParamSchema::value("value", HostTypeSchema::Unknown)],
        HostTypeSchema::Float,
    ));
    builder.function(HostFunctionSchema::with_return(
        RETURN_SCALAR,
        vec![HostParamSchema::value("value", HostTypeSchema::Unknown)],
        HostTypeSchema::Unknown,
    ));
    for (name, result) in [
        (TABLE_ARG, HostTypeSchema::Resource(key)),
        (NUMBER_ARG, HostTypeSchema::Float),
        (BOOL_ARG, HostTypeSchema::Bool),
        (STRING_ARG, HostTypeSchema::String),
        (COLLECT, HostTypeSchema::Float),
    ] {
        builder.function(HostFunctionSchema::with_return(
            name,
            vec![HostParamSchema::value("value", HostTypeSchema::Unknown)],
            result,
        ));
    }
    for (name, params, result) in [
        (ENTER_RESULT, vec!["width"], HostTypeSchema::Unknown),
        (ENTER_CALLEE, vec!["width"], HostTypeSchema::Float),
        (ENTER_PENDING, vec!["width"], HostTypeSchema::Unknown),
        (
            WRITE_PENDING,
            vec!["frame", "slot", "value"],
            HostTypeSchema::Float,
        ),
        (
            WRITE_OPTIONAL_PENDING,
            vec!["frame", "slot", "value"],
            HostTypeSchema::Float,
        ),
        (LEAVE_PENDING, vec!["frame"], HostTypeSchema::Float),
        (
            ROOT_ISSUED,
            vec!["value", "slot", "role"],
            HostTypeSchema::Float,
        ),
        // Success returns the issued receiver. Assignment discards it.
        (SET_EDGE, vec!["from", "key", "to"], HostTypeSchema::Unknown),
        (GET_EDGE, vec!["table", "key"], HostTypeSchema::Unknown),
        (NEXT, vec!["table", "cursor"], HostTypeSchema::Unknown),
        (NOTE_NEXT_COUNT, vec!["count"], HostTypeSchema::Float),
        (
            SET_METATABLE,
            vec!["owner", "metatable"],
            HostTypeSchema::Unknown,
        ),
        (GET_METATABLE, vec!["owner"], HostTypeSchema::Unknown),
        (
            FINISH_RETURN,
            vec!["values", "frame", "publish", "slots", "callee"],
            HostTypeSchema::Unknown,
        ),
        (RETIRE_CALLEE, vec!["flag", "frame"], HostTypeSchema::Float),
        (RETIRE_CHUNK, vec!["frame"], HostTypeSchema::Float),
        (RELEASE_RESULT, vec!["frame", "slot"], HostTypeSchema::Float),
        (OPEN_CELL, vec!["table"], HostTypeSchema::Unknown),
        (READ_CELL, vec!["envelope"], HostTypeSchema::Unknown),
        (WRITE_CELL, vec!["envelope", "table"], HostTypeSchema::Float),
        (
            TRACK_CLOSURE,
            vec!["closure", "anchor", "envelope"],
            HostTypeSchema::Float,
        ),
    ] {
        builder.function(HostFunctionSchema::with_return(
            name,
            params
                .into_iter()
                .map(|param| HostParamSchema::value(param, HostTypeSchema::Unknown))
                .collect(),
            result,
        ));
    }
}

pub(super) fn register(
    registry: &mut HostFunctionRegistry,
    catalog: &HostApiCatalog,
) -> Result<(), VmError> {
    register_except(registry, catalog, None)
}

/// Binds the same exact catalog bodies as [`register`], omitting one name.
///
/// The omitted name stays in the catalog the lowerer saw. A later bind fails
/// closed on that import. This does not install a loose fallback.
pub(super) fn register_except(
    registry: &mut HostFunctionRegistry,
    catalog: &HostApiCatalog,
    skip: Option<&str>,
) -> Result<(), VmError> {
    for op in [
        Op::NewTable,
        Op::Type,
        Op::Add,
        Op::StringByteLen,
        Op::RawLen,
        Op::ReturnScalar,
        Op::TableArg,
        Op::NumberArg,
        Op::BoolArg,
        Op::StringArg,
        Op::GatherRoot,
        Op::Collect,
        Op::Step,
        Op::StepHasTail,
        Op::GcStop,
        Op::GcRestart,
        Op::GcIsRunning,
        Op::StepData,
        Op::AutoTableDue,
        Op::AutoTableRestart,
        Op::Count,
        Op::EnterResult,
        Op::EnterCallee,
        Op::EnterPending,
        Op::WritePending,
        Op::WriteOptionalPending,
        Op::LeavePending,
        Op::RootIssued,
        Op::SetEdge,
        Op::GetEdge,
        Op::Next,
        Op::NoteNextCount,
        Op::SetMetatable,
        Op::GetMetatable,
        Op::FinishReturn,
        Op::RetireCallee,
        Op::RetireChunk,
        Op::ReleaseResult,
        Op::CopyFields,
        Op::ExternalShape,
        Op::ByteText,
        Op::HostInt,
        Op::RawEqual,
        Op::OpenCell,
        Op::ReadCell,
        Op::WriteCell,
        Op::TrackClosure,
    ] {
        let name = match op {
            Op::NewTable => NEW_TABLE,
            Op::Type => TYPE,
            Op::Add => ADD,
            Op::StringByteLen => STRING_BYTE_LEN,
            Op::RawLen => RAW_LEN,
            Op::ReturnScalar => RETURN_SCALAR,
            Op::TableArg => TABLE_ARG,
            Op::NumberArg => NUMBER_ARG,
            Op::BoolArg => BOOL_ARG,
            Op::StringArg => STRING_ARG,
            Op::GatherRoot => GATHER_ROOT,
            Op::Collect => COLLECT,
            Op::Step => STEP,
            Op::StepHasTail => STEP_HAS_TAIL,
            Op::GcStop => GC_STOP,
            Op::GcRestart => GC_RESTART,
            Op::GcIsRunning => GC_IS_RUNNING,
            Op::StepData => STEP_DATA,
            Op::AutoTableDue => AUTO_TABLE_DUE,
            Op::AutoTableRestart => AUTO_TABLE_RESTART,
            Op::Count => COUNT,
            Op::EnterResult => ENTER_RESULT,
            Op::EnterCallee => ENTER_CALLEE,
            Op::EnterPending => ENTER_PENDING,
            Op::WritePending => WRITE_PENDING,
            Op::WriteOptionalPending => WRITE_OPTIONAL_PENDING,
            Op::LeavePending => LEAVE_PENDING,
            Op::RootIssued => ROOT_ISSUED,
            Op::SetEdge => SET_EDGE,
            Op::GetEdge => GET_EDGE,
            Op::Next => NEXT,
            Op::NoteNextCount => NOTE_NEXT_COUNT,
            Op::SetMetatable => SET_METATABLE,
            Op::GetMetatable => GET_METATABLE,
            Op::FinishReturn => FINISH_RETURN,
            Op::RetireCallee => RETIRE_CALLEE,
            Op::RetireChunk => RETIRE_CHUNK,
            Op::ReleaseResult => RELEASE_RESULT,
            Op::CopyFields => COPY_FIELDS,
            Op::ExternalShape => EXTERNAL_SHAPE,
            Op::ByteText => BYTE_TEXT,
            Op::HostInt => HOST_INT,
            Op::RawEqual => RAW_EQUAL,
            Op::OpenCell => OPEN_CELL,
            Op::ReadCell => READ_CELL,
            Op::WriteCell => WRITE_CELL,
            Op::TrackClosure => TRACK_CLOSURE,
        };
        if skip == Some(name) {
            continue;
        }
        if matches!(
            op,
            Op::Step
                | Op::StepHasTail
                | Op::GcStop
                | Op::GcRestart
                | Op::GcIsRunning
                | Op::StepData
                | Op::AutoTableDue
                | Op::AutoTableRestart
        ) && catalog.function(name).is_none()
        {
            continue;
        }
        let function = catalog.function(name).expect("private Lua operation");
        let schema = HostImportSchema::from_function(catalog, function);
        registry.register_exact_owned(name, function.params.len() as u8, schema, move |_| {
            Box::new(op)
        })?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Op {
    NewTable,
    Type,
    Add,
    StringByteLen,
    RawLen,
    ReturnScalar,
    TableArg,
    NumberArg,
    BoolArg,
    StringArg,
    GatherRoot,
    Collect,
    Step,
    StepHasTail,
    GcStop,
    GcRestart,
    GcIsRunning,
    StepData,
    AutoTableDue,
    AutoTableRestart,
    Count,
    EnterResult,
    EnterCallee,
    EnterPending,
    WritePending,
    WriteOptionalPending,
    LeavePending,
    RootIssued,
    SetEdge,
    GetEdge,
    Next,
    NoteNextCount,
    SetMetatable,
    GetMetatable,
    FinishReturn,
    RetireCallee,
    RetireChunk,
    ReleaseResult,
    CopyFields,
    ExternalShape,
    ByteText,
    HostInt,
    RawEqual,
    OpenCell,
    ReadCell,
    WriteCell,
    TrackClosure,
}

impl HostOwnedFunction for Op {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        let value = match self {
            Self::NewTable => match take_automatic_release_table(&mut call.vm().host_context())? {
                Some(value) => value,
                None => heap::publish_guest(&mut call.vm().host_context())
                    .map_err(|error| VmError::HostError(error.to_string()))?,
            },
            Self::Type => {
                let value = match call.args() {
                    [value] => value.clone(),
                    _ => {
                        return Err(VmError::HostError("invalid private Lua value".to_string()));
                    }
                };
                if matches!(value, Value::Null) {
                    return Ok(CallOutcome::Return(CallReturn::one(Value::string("nil"))));
                }
                let name = match classify(&mut call.vm().host_context(), &value)? {
                    heap::GuestClass::Table(_) => "table",
                    heap::GuestClass::Number => "number",
                    heap::GuestClass::Boolean => "boolean",
                    heap::GuestClass::String => "string",
                    heap::GuestClass::Unissued => {
                        return Err(VmError::HostError("invalid private Lua value".to_string()));
                    }
                };
                Value::string(name)
            }
            Self::Add => {
                let (left, right) = match call.args() {
                    [left, right] => (left.clone(), right.clone()),
                    _ => {
                        return Err(VmError::HostError(
                            "unsupported Lua arithmetic operand".to_string(),
                        ));
                    }
                };
                let left_class = classify(&mut call.vm().host_context(), &left)?;
                let right_class = classify(&mut call.vm().host_context(), &right)?;
                match (left_class, right_class, left, right) {
                    (
                        heap::GuestClass::Number,
                        heap::GuestClass::Number,
                        Value::Float(left),
                        Value::Float(right),
                    ) => Value::Float(left + right),
                    (heap::GuestClass::Table(_), _, _, _)
                    | (_, heap::GuestClass::Table(_), _, _) => {
                        return Err(VmError::HostError(
                            "attempt to perform arithmetic on a table value".to_string(),
                        ));
                    }
                    _ => {
                        return Err(VmError::HostError(
                            "unsupported Lua arithmetic operand".to_string(),
                        ));
                    }
                }
            }
            Self::StringByteLen => string_byte_len(call)?,
            Self::RawLen => raw_len(call)?,
            Self::ReturnScalar => match call.args() {
                [value @ (Value::Float(_) | Value::String(_) | Value::Bool(_) | Value::Null)] => {
                    value.clone()
                }
                _ => {
                    return Err(VmError::HostError(
                        "Lua result needs a typed adapter".to_string(),
                    ));
                }
            },
            Self::TableArg => {
                let value = match call.args() {
                    [value] => value.clone(),
                    _ => {
                        return Err(VmError::HostError("expected Lua table".to_string()));
                    }
                };
                let table = match classify(&mut call.vm().host_context(), &value)? {
                    heap::GuestClass::Table(table) => table,
                    _ => return Err(VmError::HostError("expected Lua table".to_string())),
                };
                let raw = heap::with_heap(&mut call.vm().host_context(), |heap| {
                    heap.published_token(table)
                })
                .map_err(|error| VmError::HostError(error.to_string()))?;
                Value::Int(
                    i64::try_from(raw)
                        .map_err(|_| VmError::HostError("table token out of range".to_string()))?,
                )
            }
            Self::NumberArg => match call.args() {
                [Value::Float(number)] => Value::Float(*number),
                _ => return Err(VmError::HostError("expected Lua number".to_string())),
            },
            Self::BoolArg => match call.args() {
                [Value::Bool(value)] => Value::Bool(*value),
                _ => return Err(VmError::HostError("expected Lua boolean".to_string())),
            },
            Self::StringArg => match call.args() {
                [Value::String(value)] => Value::string(value.as_str()),
                _ => return Err(VmError::HostError("expected Lua string".to_string())),
            },
            Self::GatherRoot => {
                let value = match call.args() {
                    [_, value] => value.clone(),
                    _ => {
                        return Err(VmError::HostError(
                            "invalid private Lua root candidate".to_string(),
                        ));
                    }
                };
                let included = if matches!(value, Value::Null | Value::Bytes(_)) {
                    // These scalars are legal Lua values and have no heap node.
                    // Host integers, forged arrays, and expired carriers still
                    // fall through to classification and stay rejected.
                    None
                } else {
                    match classify(&mut call.vm().host_context(), &value)? {
                        heap::GuestClass::Table(_) => Some(value),
                        heap::GuestClass::Number
                        | heap::GuestClass::Boolean
                        | heap::GuestClass::String => None,
                        heap::GuestClass::Unissued => {
                            // A callable is not a table root. Its shadow envelope,
                            // when still assigned, is the issued table gather sees.
                            if matches!(value, Value::Callable(_)) {
                                None
                            } else {
                                return Err(VmError::HostError(
                                    "invalid private Lua root candidate".to_string(),
                                ));
                            }
                        }
                    }
                };
                let root_list = call.take_arg(0)?;
                let mut roots = match root_list {
                    Value::Null => {
                        return Ok(CallOutcome::Return(CallReturn::one(Value::array(
                            included.into_iter().collect(),
                        ))));
                    }
                    Value::Array(roots) => roots,
                    _ => {
                        return Err(VmError::HostError(
                            "invalid private Lua root list".to_string(),
                        ));
                    }
                };
                if let Some(table) = included {
                    std::sync::Arc::make_mut(&mut roots).push(table);
                }
                Value::Array(roots)
            }
            Self::Collect => {
                let tables = decode_safepoint_roots(call)?;
                collect_at_safepoint(&mut call.vm().host_context(), &tables)?;
                restore_running_after_progress(&mut call.vm().host_context())?;
                Value::Float(0.0)
            }
            Self::Step => {
                let value = step_at_safepoint(call)?;
                restore_running_after_progress(&mut call.vm().host_context())?;
                value
            }
            Self::StepHasTail => Value::Bool(heap::saved_source_tail(&call.vm().host_context())),
            Self::GcStop => set_gc_running(&mut call.vm().host_context(), false)?,
            Self::GcRestart => gc_restart(&mut call.vm().host_context())?,
            Self::AutoTableDue => Value::Bool(auto_table_due(&mut call.vm().host_context())?),
            Self::AutoTableRestart => auto_table_restart(call)?,
            Self::GcIsRunning => gc_is_running(&call.vm().host_context()),
            Self::StepData => step_data_default(call)?,
            Self::Count => {
                let kib =
                    heap::with_heap(&mut call.vm().host_context(), |heap| Ok(heap.logical_kib()))
                        .map_err(|error| VmError::HostError(error.to_string()))?;
                Value::Float(kib)
            }
            Self::EnterResult => enter_result(call)?,
            Self::EnterCallee => enter_callee(call)?,
            Self::EnterPending => enter_pending(call)?,
            Self::WritePending => write_pending(call)?,
            Self::WriteOptionalPending => write_optional_pending(call)?,
            Self::LeavePending => leave_pending(call)?,
            Self::RootIssued => root_issued(call)?,
            Self::SetEdge => set_edge(call)?,
            Self::GetEdge => get_edge(call)?,
            Self::Next => next_entry(call)?,
            Self::NoteNextCount => note_next_count(call)?,
            Self::SetMetatable => set_metatable(call)?,
            Self::GetMetatable => get_metatable(call)?,
            Self::FinishReturn => finish_return(call)?,
            Self::RetireCallee => retire_callee(call)?,
            Self::RetireChunk => retire_chunk(call)?,
            Self::ReleaseResult => release_result(call)?,
            Self::CopyFields => match call.args() {
                [Value::Array(fields)] => Value::array(fields.iter().cloned().collect()),
                _ => {
                    return Err(VmError::HostError(
                        "copy_fields witness expects an array carrier".to_string(),
                    ));
                }
            },
            Self::ExternalShape => Value::array(vec![Value::Int(0), Value::Int(1)]),
            Self::ByteText => Value::bytes(b"ab".to_vec()),
            Self::HostInt => Value::Int(7),
            Self::RawEqual => raw_equal(call)?,
            Self::OpenCell => open_cell(call)?,
            Self::ReadCell => read_cell(call)?,
            Self::WriteCell => write_cell(call)?,
            Self::TrackClosure => track_closure(call)?,
        };
        Ok(CallOutcome::Return(CallReturn::one(value)))
    }
}

/// Byte length of a Lua string or binary string.
///
/// Core `len` is not this operation: it counts Rust `char`s for
/// `Value::String`, returns `Value::Int`, and also accepts arrays and maps.
fn byte_len_of(value: &Value) -> Option<usize> {
    match value {
        Value::String(text) => Some(text.len()),
        Value::Bytes(bytes) => Some(bytes.len()),
        _ => None,
    }
}

fn string_byte_len(call: &OwnedHostCall<'_>) -> Result<Value, VmError> {
    let len = match call.args() {
        [value] => byte_len_of(value)
            .ok_or_else(|| VmError::HostError("Lua length operand is not a string".to_string()))?,
        _ => {
            return Err(VmError::HostError(
                "Lua length operand is not a string".to_string(),
            ));
        }
    };
    Ok(Value::Float(len as f64))
}

/// Unbound `rawlen`. Strings share [`byte_len_of`]. An issued table is a raw
/// numeric border. A number, a Core array, and a foreign carrier are not tables.
fn raw_len(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let value = match call.args() {
        [value] => value.clone(),
        _ => {
            return Err(host_fault("Lua rawlen operand is not a string or table"));
        }
    };
    if let Some(len) = byte_len_of(&value) {
        return Ok(Value::Float(len as f64));
    }
    let table = match classify(&mut call.vm().host_context(), &value)? {
        heap::GuestClass::Table(table) => table,
        _ => {
            return Err(host_fault("Lua rawlen operand is not a string or table"));
        }
    };
    let len = heap::with_heap(&mut call.vm().host_context(), |heap| {
        heap.raw_sequence_len(table)
    })
    .map_err(heap_error)?;
    Ok(Value::Float(len))
}

fn skip_parens(expr: &ast::Expr) -> &ast::Expr {
    let mut expr = expr;
    while let ast::Expr::Paren { inner, .. } = expr {
        expr = inner;
    }
    expr
}

fn raw_equal(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let (left, right) = match call.args() {
        [left, right] => (left.clone(), right.clone()),
        _ => return Err(host_fault("Lua rawequal needs two values")),
    };
    let same = values_raw_equal(&mut call.vm().host_context(), &left, &right)?;
    Ok(Value::Bool(same))
}

enum EqualKind<'a> {
    Nil,
    Bool(bool),
    Number(f64),
    Bytes(&'a [u8]),
    Table(heap::TableRef),
}

fn equal_kind<'a>(
    host: &mut vm::HostContext<'_>,
    value: &'a Value,
) -> Result<EqualKind<'a>, VmError> {
    match value {
        Value::Null => Ok(EqualKind::Nil),
        Value::Bool(bit) => Ok(EqualKind::Bool(*bit)),
        Value::Float(number) => Ok(EqualKind::Number(*number)),
        Value::Int(_) => Err(host_fault("host integer is not a Lua number")),
        Value::String(text) => Ok(EqualKind::Bytes(text.as_bytes())),
        Value::Bytes(bytes) => Ok(EqualKind::Bytes(bytes.as_ref())),
        Value::Array(_) => match classify(host, value)? {
            heap::GuestClass::Table(table) => Ok(EqualKind::Table(table)),
            _ => Err(host_fault("Lua rawequal operand is not an issued table")),
        },
        _ => Err(host_fault("Lua rawequal operand is not a supported value")),
    }
}

fn values_raw_equal(
    host: &mut vm::HostContext<'_>,
    left: &Value,
    right: &Value,
) -> Result<bool, VmError> {
    let left = equal_kind(host, left)?;
    let right = equal_kind(host, right)?;
    Ok(match (left, right) {
        (EqualKind::Nil, EqualKind::Nil) => true,
        (EqualKind::Bool(left), EqualKind::Bool(right)) => left == right,
        (EqualKind::Number(left), EqualKind::Number(right)) => left == right,
        (EqualKind::Bytes(left), EqualKind::Bytes(right)) => left == right,
        (EqualKind::Table(left), EqualKind::Table(right)) => left == right,
        _ => false,
    })
}

fn host_fault(message: &str) -> VmError {
    VmError::HostError(message.to_string())
}

fn with_witness<T>(
    vm: &mut vm::Vm,
    body: impl FnOnce(&mut ReturnWitness) -> T,
) -> Result<T, VmError> {
    let mut host = vm.host_context();
    let witness = host
        .scope_state_or_insert_with(ReturnWitness::default)
        .map_err(|error| VmError::HostError(error.to_string()))?;
    Ok(body(witness))
}

fn float_u32(value: &Value) -> Result<u32, VmError> {
    let Value::Float(number) = value else {
        return Err(host_fault("expected a compile-time Lua integer"));
    };
    if !number.is_finite() || *number < 0.0 || number.fract() != 0.0 {
        return Err(host_fault("expected a compile-time Lua integer"));
    }
    u32::try_from(*number as u64).map_err(|_| host_fault("integer out of range"))
}

fn carrier_ptr(value: &Value) -> Option<usize> {
    match value {
        Value::Array(issued) => Some(std::sync::Arc::as_ptr(issued) as usize),
        _ => None,
    }
}

fn read_edge(
    heap: &heap::LuaHeap,
    table: Option<heap::TableRef>,
    key: Option<&str>,
) -> Result<EdgeView, heap::HeapError> {
    let (Some(table), Some(key)) = (table, key) else {
        return Ok(EdgeView::Absent);
    };
    match heap.named_edge(table, key) {
        Ok(Some(target)) => Ok(EdgeView::Live(target)),
        Ok(None) => Ok(EdgeView::Absent),
        Err(heap::HeapError::StaleHandle) => Ok(EdgeView::Stale),
        Err(error) => Err(error),
    }
}

fn decode_frame(call: &mut OwnedHostCall<'_>, token: &Value) -> Result<heap::RootFrameId, VmError> {
    if matches!(
        classify(&mut call.vm().host_context(), token)?,
        heap::GuestClass::Table(_)
    ) {
        return Err(host_fault("caller frame token is not a private array"));
    }
    let Value::Array(parts) = token else {
        return Err(host_fault("caller frame token is not a private array"));
    };
    let [Value::Float(index), Value::Float(generation)] = parts.as_slice() else {
        return Err(host_fault("caller frame token is not a private array"));
    };
    Ok(heap::RootFrameId::from_parts(
        float_u32(&Value::Float(*index))?,
        float_u32(&Value::Float(*generation))?,
    ))
}

fn frame_token(frame: heap::RootFrameId) -> Value {
    let (index, generation) = frame.parts();
    Value::array(vec![
        Value::Float(index as f64),
        Value::Float(generation as f64),
    ])
}

fn enter_result(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let width = match call.args() {
        [value] => float_u32(value)?,
        _ => return Err(host_fault("invalid caller frame width")),
    };
    let frame = heap::with_heap(&mut call.vm().host_context(), |heap| {
        heap.enter_roots(width)
    })
    .map_err(heap_error)?;
    let depth = call.vm().call_depth();
    with_witness(call.vm(), |witness| {
        witness.caller_depth = Some(depth);
    })?;
    Ok(frame_token(frame))
}

fn enter_callee(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let width = match call.args() {
        [value] => float_u32(value)?,
        _ => return Err(host_fault("invalid callee frame width")),
    };
    if width > 0 {
        heap::with_heap(&mut call.vm().host_context(), |heap| {
            heap.enter_roots(width)
        })
        .map_err(heap_error)?;
    }
    let depth = call.vm().call_depth();
    with_witness(call.vm(), |witness| {
        witness.callee_depth = Some(depth);
    })?;
    Ok(Value::Float(0.0))
}

fn enter_pending(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let width = match call.args() {
        [value] => float_u32(value)?,
        _ => return Err(host_fault("invalid pending frame")),
    };
    let frame = heap::with_heap(&mut call.vm().host_context(), |heap| {
        heap.enter_roots(width)
    })
    .map_err(heap_error)?;
    Ok(frame_token(frame))
}

fn write_pending(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let (frame_token, slot, value) = match call.args() {
        [frame, slot, value] => (frame.clone(), float_u32(slot)?, value.clone()),
        _ => return Err(host_fault("invalid pending root")),
    };
    let table = match classify(&mut call.vm().host_context(), &value)? {
        heap::GuestClass::Table(table) => table,
        _ => {
            if let Ok(frame) = decode_frame(call, &frame_token) {
                let _ = heap::with_heap(&mut call.vm().host_context(), |heap| {
                    heap.leave_roots(frame)
                });
            }
            return Err(host_fault("pending root is not a table"));
        }
    };
    let frame = decode_frame(call, &frame_token)?;
    if let Err(error) = heap::with_heap(&mut call.vm().host_context(), |heap| {
        heap.write_root(frame, slot, table)
    }) {
        let _ = heap::with_heap(&mut call.vm().host_context(), |heap| {
            heap.leave_roots(frame)
        });
        return Err(heap_error(error));
    }
    Ok(Value::Float(0.0))
}

/// Candidate-private root write. Tables use the same frame check and
/// `write_root` as `WRITE_PENDING`. Legal scalars, nil, bytes, and callables
/// leave the slot empty and do not abort the later call. Forged, stale, and
/// other unissued values still reject, and that rejection drops the frame.
fn write_optional_pending(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let (frame_token, slot, value) = match call.args() {
        [frame, slot, value] => (frame.clone(), float_u32(slot)?, value.clone()),
        _ => return Err(host_fault("invalid pending root")),
    };
    let table = match optional_pending_table(call, &value) {
        Ok(Some(table)) => table,
        Ok(None) => return Ok(Value::Float(0.0)),
        Err(error) => {
            abandon_pending(call, &frame_token);
            return Err(error);
        }
    };
    let frame = decode_frame(call, &frame_token)?;
    if let Err(error) = heap::with_heap(&mut call.vm().host_context(), |heap| {
        heap.write_root(frame, slot, table)
    }) {
        let _ = heap::with_heap(&mut call.vm().host_context(), |heap| {
            heap.leave_roots(frame)
        });
        return Err(heap_error(error));
    }
    Ok(Value::Float(0.0))
}

fn optional_pending_table(
    call: &mut OwnedHostCall<'_>,
    value: &Value,
) -> Result<Option<heap::TableRef>, VmError> {
    if matches!(value, Value::Null | Value::Bytes(_)) {
        return Ok(None);
    }
    match classify(&mut call.vm().host_context(), value)? {
        heap::GuestClass::Table(table) => Ok(Some(table)),
        heap::GuestClass::Number | heap::GuestClass::Boolean | heap::GuestClass::String => Ok(None),
        heap::GuestClass::Unissued => {
            if matches!(value, Value::Callable(_)) {
                Ok(None)
            } else {
                Err(host_fault("invalid private Lua root candidate"))
            }
        }
    }
}

fn abandon_pending(call: &mut OwnedHostCall<'_>, token: &Value) {
    if let Ok(frame) = decode_frame(call, token) {
        let _ = heap::with_heap(&mut call.vm().host_context(), |heap| {
            heap.leave_roots(frame)
        });
    }
}

fn leave_pending(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let frame_token = match call.args() {
        [frame] => frame.clone(),
        _ => return Err(host_fault("invalid pending release")),
    };
    let frame = decode_frame(call, &frame_token)?;
    let current = heap::with_heap(&mut call.vm().host_context(), |heap| {
        Ok(heap.current_frame())
    })
    .map_err(heap_error)?;
    if current != Some(frame) {
        return Err(host_fault("pending release is not the current frame"));
    }
    heap::with_heap(&mut call.vm().host_context(), |heap| {
        heap.leave_roots(frame)
    })
    .map_err(heap_error)?;
    Ok(Value::Float(0.0))
}

fn root_issued(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let (value, slot, role) = match call.args() {
        [value, slot, role] => (value.clone(), float_u32(slot)?, float_u32(role)?),
        _ => return Err(host_fault("invalid issued-table root")),
    };
    let table = match classify(&mut call.vm().host_context(), &value)? {
        heap::GuestClass::Table(table) => table,
        _ => return Err(host_fault("issued root is not a table")),
    };
    let frame = heap::with_heap(&mut call.vm().host_context(), |heap| {
        heap.current_frame().ok_or(heap::HeapError::StaleFrame)
    })
    .map_err(heap_error)?;
    if !acknowledge_release_root(&mut call.vm().host_context(), table, slot)? {
        heap::with_heap(&mut call.vm().host_context(), |heap| {
            heap.write_root(frame, slot, table)
        })
        .map_err(heap_error)?;
    }
    with_witness(call.vm(), |witness| {
        witness.issued.push(table);
        if role == 0 {
            witness.returned = Some(table);
            witness.returned_carrier = carrier_ptr(&value);
        } else if witness.discarded.is_none() {
            witness.discarded = Some(table);
        }
    })?;
    Ok(Value::Float(0.0))
}

fn write_number_edge(
    call: &mut OwnedHostCall<'_>,
    from: heap::TableRef,
    key: f64,
    to_value: &Value,
) -> Result<(), VmError> {
    if matches!(to_value, Value::Null) {
        return heap::with_heap(&mut call.vm().host_context(), |heap| {
            heap.clear_number_edge(from, key)
        })
        .map_err(heap_error);
    }
    let mut host = call.vm().host_context();
    match classify(&mut host, to_value)? {
        heap::GuestClass::Table(to) => {
            heap::with_heap(&mut host, |heap| heap.set_number_edge(from, key, to))
        }
        heap::GuestClass::String => heap::with_heap(&mut host, |heap| {
            heap.set_number_scalar(from, key, to_value.clone())
        }),
        _ => return Err(host_fault("edge target is not a table")),
    }
    .map_err(heap_error)
}

fn set_edge(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let (issued, key_value, to_value) = match call.args() {
        [from, key, to] => (from.clone(), key.clone(), to.clone()),
        _ => return Err(host_fault("invalid table edge")),
    };
    let from = match classify(&mut call.vm().host_context(), &issued)? {
        heap::GuestClass::Table(table) => table,
        _ => return Err(host_fault("edge source is not a table")),
    };
    match key_value {
        Value::String(name) => {
            let mut host = call.vm().host_context();
            match classify(&mut host, &to_value)? {
                heap::GuestClass::Table(to) => {
                    heap::with_heap(&mut host, |heap| heap.set_named_edge(from, &name, to))
                }
                class
                    if matches!(class, heap::GuestClass::String | heap::GuestClass::Boolean)
                        || matches!(to_value, Value::Null)
                        || (heap::LuaHeap::names_mode_field(&name)
                            && matches!(class, heap::GuestClass::Number)) =>
                {
                    heap::with_heap(&mut host, |heap| {
                        heap.set_named_scalar(from, &name, to_value)
                    })
                }
                _ => return Err(host_fault("edge target is not a table")),
            }
            .map_err(heap_error)?;
            with_witness(call.vm(), |witness| {
                if witness.field_key.is_none() {
                    witness.field_key = Some(name.to_string());
                }
            })?;
        }
        // Binary64 only. An integer resource carrier is not a number key.
        Value::Float(number) => write_number_edge(call, from, number, &to_value)?,
        other => {
            let heap::GuestClass::Table(key) = classify(&mut call.vm().host_context(), &other)?
            else {
                return Err(host_fault("edge key is not a table"));
            };
            if matches!(to_value, Value::Null) {
                heap::with_heap(&mut call.vm().host_context(), |heap| {
                    heap.clear_object_edge(from, key)
                })
                .map_err(heap_error)?;
            } else {
                let mut host = call.vm().host_context();
                let class = classify(&mut host, &to_value)?;
                match class {
                    heap::GuestClass::Table(to) => {
                        heap::with_heap(&mut host, |heap| heap.set_object_edge(from, key, to))
                    }
                    heap::GuestClass::String => heap::with_heap(&mut host, |heap| {
                        heap.set_object_scalar(from, key, to_value)
                    }),
                    _ => return Err(host_fault("edge target is not a table")),
                }
                .map_err(heap_error)?;
            }
        }
    }
    Ok(issued)
}

fn finish_return(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let (values, frame_token, publish, slots, callee_flag) = match call.args() {
        [
            Value::Array(values),
            frame,
            Value::Float(publish),
            slots,
            Value::Float(flag),
        ] => (
            values.clone(),
            frame.clone(),
            *publish != 0.0,
            slots.clone(),
            *flag != 0.0,
        ),
        _ => return Err(host_fault("Lua return transport is not one array")),
    };
    let root_slots = match &slots {
        Value::Array(items) => items.as_slice().to_vec(),
        Value::Null => Vec::new(),
        _ => return Err(host_fault("caller root slots are not a private array")),
    };
    let transport = Value::Array(values.clone());
    if matches!(
        classify(&mut call.vm().host_context(), &transport)?,
        heap::GuestClass::Table(_)
    ) {
        return Err(host_fault(
            "Lua return transport must stay an unissued array",
        ));
    }
    // Core has already executed Ret. The callee Lua root frame, if any, is still current.
    let depth = call.vm().call_depth();
    let mut elements = Vec::with_capacity(values.len());
    for value in values.iter() {
        elements.push(return_element(call, value)?);
    }
    // A saved Release tail may publish one issued table into the direct
    // caller's empty slot, then the existing retire drops the callee frame.
    // Every other table shape, including publish=0, still refuses before any
    // root write. Number and nil do not enter this path.
    let release_root = if callee_flag
        && elements
            .iter()
            .any(|element| matches!(element, ReturnedElement::Table { .. }))
    {
        match heap::saved_tail_state(&mut call.vm().host_context()).map_err(heap_error)? {
            Some(_) => {
                if !publish || values.len() != 1 || root_slots.len() != 1 {
                    return Err(heap_error(heap::HeapError::SweepPending));
                }
                Some(publish_release_table(
                    call,
                    &values[0],
                    &frame_token,
                    &root_slots,
                )?)
            }
            None => None,
        }
    } else {
        None
    };
    // Classify each consumed position. Nil, scalars, and the transport array
    // do not become roots. The callee source name is not consulted. The
    // Release handoff above already wrote its one root and must not call this
    // writer again.
    let wrote = if release_root.is_some() {
        true
    } else {
        publish_consumed_tables(call, &values, &frame_token, publish, &root_slots)?
    };
    let lua_frame_alive = if callee_flag {
        heap::with_heap(&mut call.vm().host_context(), |heap| {
            Ok(heap.current_frame().is_some())
        })
        .map_err(heap_error)?
    } else {
        false
    };
    let (returned, discarded, key, callee_depth) = with_witness(call.vm(), |witness| {
        (
            witness.returned,
            witness.discarded,
            witness.field_key.clone(),
            witness.callee_depth,
        )
    })?;
    let (returned_edge, discarded_edge) = heap::with_heap(&mut call.vm().host_context(), |heap| {
        Ok((
            read_edge(heap, returned, key.as_deref())?,
            read_edge(heap, discarded, key.as_deref())?,
        ))
    })
    .map_err(heap_error)?;
    with_witness(call.vm(), |witness| {
        witness.elements = elements.clone();
        witness.published_root = wrote;
        witness.handoff_depth = Some(depth);
        witness.lua_frame_alive_at_publish = lua_frame_alive;
        witness.returned_edge_at_handoff = returned_edge;
        witness.discarded_edge_at_handoff = discarded_edge;
        witness.release_root = release_root;
        witness.handoffs.push(Handoff {
            elements,
            published_root: wrote,
            callee_left: false,
            top_is_caller_after_handoff: false,
            lua_frame_alive_at_publish: lua_frame_alive,
            handoff_depth: Some(depth),
            callee_depth,
            returned_edge,
            discarded_edge,
        });
    })?;
    Ok(transport)
}

fn publish_release_table(
    call: &mut OwnedHostCall<'_>,
    value: &Value,
    frame_token: &Value,
    root_slots: &[Value],
) -> Result<ReleaseRootNote, VmError> {
    if !matches!(
        classify(&mut call.vm().host_context(), value)?,
        heap::GuestClass::Table(_)
    ) {
        return Err(heap_error(heap::HeapError::SweepPending));
    }
    if matches!(frame_token, Value::Null) {
        return Err(heap_error(heap::HeapError::SweepPending));
    }
    let frame = decode_frame(call, frame_token)?;
    let slot = float_u32(&root_slots[0])?;
    let meter_before = heap::copy_scan_meter(&mut call.vm().host_context()).map_err(heap_error)?;
    let tail_before = heap::saved_tail_state(&mut call.vm().host_context())
        .map_err(heap_error)?
        .ok_or_else(|| heap_error(heap::HeapError::SweepDenied))?;
    heap::publish_release_return_root(&mut call.vm().host_context(), frame, slot, value)
        .map_err(heap_error)?;
    let meter_after = heap::copy_scan_meter(&mut call.vm().host_context()).map_err(heap_error)?;
    let tail_after = heap::saved_tail_state(&mut call.vm().host_context())
        .map_err(heap_error)?
        .ok_or_else(|| heap_error(heap::HeapError::SweepDenied))?;
    Ok(ReleaseRootNote {
        meter_before,
        meter_after,
        tail_before,
        tail_after,
    })
}

fn publish_consumed_tables(
    call: &mut OwnedHostCall<'_>,
    values: &[Value],
    frame_token: &Value,
    publish: bool,
    root_slots: &[Value],
) -> Result<bool, VmError> {
    if !publish {
        return Ok(false);
    }
    if matches!(frame_token, Value::Null) {
        if root_slots.is_empty() {
            return Ok(false);
        }
        return Err(host_fault("missing caller root frame"));
    }
    let frame = decode_frame(call, frame_token)?;
    let mut wrote = false;
    for (index, slot_value) in root_slots.iter().enumerate() {
        let Some(element) = values.get(index) else {
            continue;
        };
        let heap::GuestClass::Table(table) = classify(&mut call.vm().host_context(), element)?
        else {
            continue;
        };
        let slot = float_u32(slot_value)?;
        // Publish before any later retire removes the callee frame.
        heap::with_heap(&mut call.vm().host_context(), |heap| {
            heap.write_root(frame, slot, table)
        })
        .map_err(heap_error)?;
        wrote = true;
    }
    Ok(wrote)
}

fn return_element(call: &mut OwnedHostCall<'_>, value: &Value) -> Result<ReturnedElement, VmError> {
    match value {
        Value::Null => Ok(ReturnedElement::Nil),
        Value::Float(number) => Ok(ReturnedElement::Number(*number)),
        other => {
            let table = match classify(&mut call.vm().host_context(), other)? {
                heap::GuestClass::Table(table) => table,
                _ => return Err(host_fault("return element is not a pass-through value")),
            };
            let (same_ref, same_carrier) = with_witness(call.vm(), |witness| {
                let same_ref = witness.returned == Some(table);
                let same_carrier = matches!(
                    (witness.returned_carrier, carrier_ptr(other)),
                    (Some(issued), Some(found)) if issued == found
                );
                (same_ref, same_carrier)
            })?;
            Ok(ReturnedElement::Table {
                same_ref,
                same_carrier,
            })
        }
    }
}

fn retire_callee(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let (flag, frame_token) = match call.args() {
        [Value::Float(flag), frame] => (*flag != 0.0, frame.clone()),
        _ => return Err(host_fault("invalid callee retire")),
    };
    let caller = if matches!(frame_token, Value::Null) {
        None
    } else {
        Some(decode_frame(call, &frame_token)?)
    };
    let mut left = false;
    if flag {
        let callee = heap::with_heap(&mut call.vm().host_context(), |heap| {
            heap.current_frame().ok_or(heap::HeapError::StaleFrame)
        })
        .map_err(heap_error)?;
        if caller == Some(callee) {
            return Err(host_fault("would retire the caller result frame"));
        }
        let tail = heap::saved_sweep_tail(&mut call.vm().host_context()).map_err(heap_error)?;
        if tail {
            heap::retire_saved_tail_frame(&mut call.vm().host_context(), callee)
                .map_err(heap_error)?;
        } else {
            heap::with_heap(&mut call.vm().host_context(), |heap| {
                heap.leave_roots(callee)
            })
            .map_err(heap_error)?;
        }
        left = true;
    }
    let top_is_caller = heap::with_heap(&mut call.vm().host_context(), |heap| {
        Ok(caller.is_some() && heap.current_frame() == caller)
    })
    .map_err(heap_error)?;
    with_witness(call.vm(), |witness| {
        witness.callee_left = left;
        witness.top_is_caller_after_handoff = top_is_caller;
        if let Some(last) = witness.handoffs.last_mut() {
            last.callee_left = left;
            last.top_is_caller_after_handoff = top_is_caller;
        }
    })?;
    Ok(Value::Float(0.0))
}

fn retire_chunk(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let frame_token = match call.args() {
        [frame] => frame.clone(),
        _ => return Err(host_fault("invalid chunk retire")),
    };
    let frame = decode_frame(call, &frame_token)?;
    heap::with_heap(&mut call.vm().host_context(), |heap| {
        heap.leave_roots(frame)
    })
    .map_err(heap_error)?;
    Ok(Value::Float(0.0))
}

fn get_edge(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let (from, key_value) = match call.args() {
        [from, key] => (from.clone(), key.clone()),
        _ => return Err(host_fault("invalid field read")),
    };
    let table = match classify(&mut call.vm().host_context(), &from)? {
        heap::GuestClass::Table(table) => table,
        _ => return Err(host_fault("field source is not a table")),
    };
    // Nil and boolean keys are misses. Boolean keys are not written. Unissued
    // integers, foreign handles, and stale carriers stay on the error path.
    if matches!(key_value, Value::Null | Value::Bool(_)) {
        return Ok(Value::Null);
    }
    let mut host = call.vm().host_context();
    if let Value::String(name) = key_value {
        return heap::with_heap(&mut host, |heap| heap.named_value(table, &name))
            .map_err(heap_error);
    }
    if let Value::Float(number) = key_value {
        return heap::with_heap(&mut host, |heap| heap.number_value(table, number))
            .map_err(heap_error);
    }
    let heap::GuestClass::Table(key) = classify(&mut host, &key_value)? else {
        return Err(host_fault("edge key is not a table"));
    };
    heap::with_heap(&mut host, |heap| heap.object_value(table, key)).map_err(heap_error)
}

fn next_entry(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let (table_value, cursor) = match call.args() {
        [table, cursor] => (table.clone(), cursor.clone()),
        _ => return Err(host_fault("invalid next")),
    };
    let table = match classify(&mut call.vm().host_context(), &table_value)? {
        heap::GuestClass::Table(table) => table,
        _ => return Err(host_fault("next target is not a table")),
    };
    let step = heap::with_heap(&mut call.vm().host_context(), |heap| {
        heap.next_guest(table, &cursor)
    })
    .map_err(heap_error)?;
    // The array is the same unissued return transport a multi-local already
    // consumes with Get. It is not a second protocol and not a Lua table.
    // End is one nil. An entry is the key and the value.
    let values = match step {
        None => vec![Value::Null],
        Some((key, value)) => vec![key, value],
    };
    Ok(Value::array(values))
}

fn note_next_count(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let count = match call.args() {
        [Value::Int(count)] => {
            u32::try_from(*count).map_err(|_| host_fault("next result count"))?
        }
        _ => return Err(host_fault("next result count")),
    };
    call.vm()
        .host_context()
        .scope_state_or_insert_with(NextCounts::default)
        .map_err(|error| VmError::HostError(error.to_string()))?
        .consumed
        .push(count);
    Ok(Value::Float(0.0))
}

fn set_metatable(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let (owner, mt) = match call.args() {
        [owner, mt] => (owner.clone(), mt.clone()),
        _ => return Err(host_fault("invalid metatable assignment")),
    };
    let owner = match classify(&mut call.vm().host_context(), &owner)? {
        heap::GuestClass::Table(table) => table,
        _ => return Err(host_fault("metatable owner is not a table")),
    };
    let mt = match &mt {
        Value::Null => None,
        other => match classify(&mut call.vm().host_context(), other)? {
            heap::GuestClass::Table(table) => Some(table),
            _ => return Err(host_fault("metatable target is not a table")),
        },
    };
    let protected = heap::with_heap(&mut call.vm().host_context(), |heap| {
        raw_metatable_guard(heap, owner)
    })
    .map_err(heap_error)?;
    if protected.is_some() {
        return Err(host_fault("cannot change a protected metatable"));
    }
    heap::with_heap(&mut call.vm().host_context(), |heap| {
        heap.set_metatable(owner, mt)?;
        heap.issued_value(owner)
    })
    .map_err(heap_error)
}

fn get_metatable(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let owner = match call.args() {
        [owner] => owner.clone(),
        _ => return Err(host_fault("invalid metatable read")),
    };
    let owner = match classify(&mut call.vm().host_context(), &owner)? {
        heap::GuestClass::Table(table) => table,
        _ => return Err(host_fault("metatable owner is not a table")),
    };
    heap::with_heap(&mut call.vm().host_context(), |heap| {
        if let Some(field) = raw_metatable_guard(heap, owner)? {
            return Ok(field);
        }
        match heap.metatable(owner)? {
            Some(target) => heap.issued_value(target),
            None => Ok(Value::Null),
        }
    })
    .map_err(heap_error)
}

fn raw_metatable_guard(
    heap: &heap::LuaHeap,
    owner: heap::TableRef,
) -> Result<Option<Value>, heap::HeapError> {
    let Some(mt) = heap.metatable(owner)? else {
        return Ok(None);
    };
    let field = heap.named_value(mt, "__metatable")?;
    if matches!(field, Value::Null) {
        Ok(None)
    } else {
        Ok(Some(field))
    }
}

fn release_result(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let (frame_token, slot) = match call.args() {
        [frame, slot] => (frame.clone(), float_u32(slot)?),
        _ => return Err(host_fault("invalid result release")),
    };
    let frame = decode_frame(call, &frame_token)?;
    let (returned, key) = with_witness(call.vm(), |witness| {
        (witness.returned, witness.field_key.clone())
    })?;
    let (alive, edge) = heap::with_heap(&mut call.vm().host_context(), |heap| {
        Ok((
            returned.is_some_and(|table| heap.alive(table)),
            read_edge(heap, returned, key.as_deref())?,
        ))
    })
    .map_err(heap_error)?;
    heap::with_heap(&mut call.vm().host_context(), |heap| {
        heap.clear_root(frame, slot)
    })
    .map_err(heap_error)?;
    let live_roots = heap::stats(&mut call.vm().host_context())
        .map_err(heap_error)?
        .live_roots;
    with_witness(call.vm(), |witness| {
        witness.release = Some(ReleaseNote {
            returned_alive: alive,
            returned_edge: edge,
            live_roots,
        });
    })?;
    Ok(Value::Float(0.0))
}

fn note_collect(
    host: &mut vm::HostContext<'_>,
    report: heap::CollectReport,
) -> Result<(), VmError> {
    let Some((returned, discarded, key, latest)) =
        host.scope_state::<ReturnWitness>().map(|witness| {
            (
                witness.returned,
                witness.discarded,
                witness.field_key.clone(),
                witness.issued.last().copied(),
            )
        })
    else {
        return Ok(());
    };
    let (returned_alive, discarded_alive, returned_edge, latest_alive) =
        heap::with_heap(host, |heap| {
            Ok((
                returned.is_some_and(|table| heap.alive(table)),
                discarded.is_some_and(|table| heap.alive(table)),
                read_edge(heap, returned, key.as_deref())?,
                latest.is_some_and(|table| heap.alive(table)),
            ))
        })
        .map_err(heap_error)?;
    let meter = heap::take_scan_meter(host).map_err(heap_error)?;
    if let Some(samples) = host.scope_state_mut::<VisitSamples>() {
        samples.visits.push(meter.root_slot_visits);
    }
    let witness = host
        .scope_state_mut::<ReturnWitness>()
        .ok_or_else(|| host_fault("return witness disappeared"))?;
    witness.collects.push(CollectNote {
        live_nodes: report.live_nodes,
        marked_nodes: report.marked_nodes,
        live_roots: report.live_roots,
        swept: meter.scratch.dead_slots_pushed,
        returned_alive,
        discarded_alive,
        returned_edge,
        metatable_edge_checks: meter.metatable_edge_checks,
        outgoing_gap_checks: meter.outgoing_gap_checks,
        outgoing_edge_checks: meter.outgoing_edge_checks,
        sweep_edge_checks: meter.sweep_edge_checks,
    });
    witness.issued_alive_at_collect.push(latest_alive);
    Ok(())
}

#[derive(Clone, Copy, Debug)]
pub(super) struct CaptureNote {
    pub envelope: heap::TableRef,
    pub closure: heap::ClosureRef,
    pub cell: heap::UpvalueRef,
    pub callable: usize,
}

struct CaptureGraph {
    notes: Vec<CaptureNote>,
    reads: u32,
    writes: u32,
}

fn with_capture<T>(
    host: &mut vm::HostContext<'_>,
    body: impl FnOnce(&mut CaptureGraph) -> T,
) -> Result<T, VmError> {
    let graph = host
        .scope_state_or_insert_with(|| CaptureGraph {
            notes: Vec::new(),
            reads: 0,
            writes: 0,
        })
        .map_err(|error| host_fault(&error.to_string()))?;
    Ok(body(graph))
}

fn root_count(host: &mut vm::HostContext<'_>) -> Result<u32, VmError> {
    heap::stats(host)
        .map(|stats| stats.live_roots)
        .map_err(heap_error)
}

fn refuse_root_change(
    before: u32,
    host: &mut vm::HostContext<'_>,
    what: &str,
) -> Result<(), VmError> {
    let after = root_count(host)?;
    if after != before {
        return Err(host_fault(what));
    }
    Ok(())
}

fn issued_table(host: &mut vm::HostContext<'_>, value: &Value) -> Result<heap::TableRef, VmError> {
    match classify(host, value)? {
        heap::GuestClass::Table(table) => Ok(table),
        _ => Err(host_fault("Lua cell operand is not an issued table")),
    }
}

fn open_cell(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let table = match call.args() {
        [table] => table.clone(),
        _ => return Err(host_fault("open_cell needs the initial table")),
    };
    let initial = issued_table(&mut call.vm().host_context(), &table)?;
    let before = root_count(&mut call.vm().host_context())?;
    let carrier = heap::with_heap(&mut call.vm().host_context(), |heap| {
        let anchor = heap.alloc()?;
        let carrier = heap.issue_carrier(anchor)?;
        let cell = heap.alloc_upvalue()?;
        heap.write_upvalue(cell, initial)?;
        heap.set_cell_edge(anchor, cell)?;
        Ok(carrier)
    })
    .map_err(heap_error)?;
    refuse_root_change(
        before,
        &mut call.vm().host_context(),
        "open_cell published a heap root",
    )?;
    Ok(carrier)
}

fn read_cell(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let table = match call.args() {
        [table] => table.clone(),
        _ => return Err(host_fault("read_cell needs the cell table")),
    };
    let table = issued_table(&mut call.vm().host_context(), &table)?;
    with_capture(&mut call.vm().host_context(), |graph| graph.reads += 1)?;
    let current = heap::with_heap(&mut call.vm().host_context(), |heap| {
        let cell = heap.cell_edge(table)?;
        heap.current_edge(cell)
    })
    .map_err(heap_error)?;
    match current {
        Some(heap::CellValue::Table(table)) => {
            heap::with_heap(&mut call.vm().host_context(), |heap| {
                heap.issued_value(table)
            })
            .map_err(heap_error)
        }
        Some(heap::CellValue::Scalar(value)) => Ok(value),
        Some(heap::CellValue::Closure(_)) => Err(host_fault("Lua cell holds a closure")),
        None => Ok(Value::Null),
    }
}

fn write_cell(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let (table, next) = match call.args() {
        [table, next] => (table.clone(), next.clone()),
        _ => {
            return Err(host_fault(
                "write_cell needs the cell table and the next value",
            ));
        }
    };
    let table = issued_table(&mut call.vm().host_context(), &table)?;
    enum NextWrite {
        Clear,
        Scalar(Value),
        Table(heap::TableRef),
    }
    let next_write = match next {
        Value::Null => NextWrite::Clear,
        value @ (Value::Bool(_) | Value::Float(_) | Value::String(_) | Value::Bytes(_)) => {
            NextWrite::Scalar(value)
        }
        other => NextWrite::Table(issued_table(&mut call.vm().host_context(), &other)?),
    };
    let before = root_count(&mut call.vm().host_context())?;
    heap::with_heap(&mut call.vm().host_context(), |heap| {
        let cell = heap.cell_edge(table)?;
        match next_write {
            NextWrite::Clear => heap.clear_upvalue(cell),
            NextWrite::Scalar(value) => heap.write_scalar_upvalue(cell, value),
            NextWrite::Table(next) => heap.write_upvalue(cell, next),
        }
    })
    .map_err(heap_error)?;
    with_capture(&mut call.vm().host_context(), |graph| graph.writes += 1)?;
    refuse_root_change(
        before,
        &mut call.vm().host_context(),
        "write_cell published a heap root",
    )?;
    Ok(Value::Float(0.0))
}

fn track_closure(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let (closure, anchor, envelope) = match call.args() {
        [closure, anchor, envelope] => (closure.clone(), anchor.clone(), envelope.clone()),
        _ => {
            return Err(host_fault(
                "track_closure needs the callable, its anchor, and its envelope",
            ));
        }
    };
    let Value::Callable(callable) = &closure else {
        return Err(host_fault("tracked closure is not a callable"));
    };
    let ptr = std::sync::Arc::as_ptr(callable) as usize;
    let anchor = issued_table(&mut call.vm().host_context(), &anchor)?;
    let envelope = issued_table(&mut call.vm().host_context(), &envelope)?;
    let before = root_count(&mut call.vm().host_context())?;
    let note = heap::with_heap(&mut call.vm().host_context(), |heap| {
        let cell = heap.cell_edge(anchor)?;
        let node = heap.alloc_closure()?;
        heap.set_upvalue_edge(node, cell)?;
        heap.set_cell_edge(envelope, cell)?;
        heap.set_closure_edge(envelope, 1, node)?;
        Ok(CaptureNote {
            envelope,
            closure: node,
            cell,
            callable: ptr,
        })
    })
    .map_err(heap_error)?;
    with_capture(&mut call.vm().host_context(), |graph| {
        graph.notes.push(note);
    })?;
    refuse_root_change(
        before,
        &mut call.vm().host_context(),
        "track_closure published a heap root",
    )?;
    Ok(Value::Float(0.0))
}

pub(super) fn capture_notes(vm: &mut vm::Vm) -> Vec<CaptureNote> {
    vm.host_context()
        .scope_state::<CaptureGraph>()
        .map(|graph| graph.notes.clone())
        .unwrap_or_default()
}

pub(super) fn capture_counts(vm: &mut vm::Vm) -> (u32, u32) {
    vm.host_context()
        .scope_state::<CaptureGraph>()
        .map(|graph| (graph.reads, graph.writes))
        .unwrap_or((0, 0))
}

/// Tables already decoded for a real safepoint. Recording them does not gather,
/// step, or write a root. The batch that was passed to the stepper is copied
/// after decode and before the step consumes it.
#[derive(Clone, Debug, Default)]
pub(super) struct SafepointBatches {
    pub batches: Vec<Vec<heap::TableRef>>,
}

pub(super) fn safepoint_batches(host: &vm::HostContext<'_>) -> Vec<Vec<heap::TableRef>> {
    host.scope_state::<SafepointBatches>()
        .map(|state| state.batches.clone())
        .unwrap_or_default()
}

fn note_safepoint_batch(
    host: &mut vm::HostContext<'_>,
    batch: &[heap::TableRef],
) -> Result<(), VmError> {
    let state = host
        .scope_state_or_insert_with(SafepointBatches::default)
        .map_err(|error| VmError::HostError(error.to_string()))?;
    state.batches.push(batch.to_vec());
    Ok(())
}

fn decode_safepoint_roots(call: &mut OwnedHostCall<'_>) -> Result<Vec<heap::TableRef>, VmError> {
    let roots = match call.args() {
        [Value::Null] | [Value::Null, _] => return Ok(Vec::new()),
        [Value::Array(roots)] | [Value::Array(roots), _] => roots.clone(),
        _ => {
            return Err(VmError::HostError(
                "invalid private Lua root list".to_string(),
            ));
        }
    };
    let mut host = call.vm().host_context();
    roots
        .iter()
        .map(|root| match classify(&mut host, root)? {
            heap::GuestClass::Table(table) => Ok(table),
            _ => Err(VmError::HostError(
                "invalid private Lua table root".to_string(),
            )),
        })
        .collect()
}

/// Absent means running. A new VM scope, including one published by reset,
/// therefore starts running even when another execution of this catalog stopped.
///
/// `threshold_bytes` is written only by single-arg restart and is the current
/// logical byte count. It is not a pause-adjusted debt.
struct GcRunControl {
    running: bool,
    threshold_bytes: Option<u64>,
    auto_requested: bool,
}

fn fresh_run_control() -> GcRunControl {
    GcRunControl {
        running: true,
        threshold_bytes: None,
        auto_requested: false,
    }
}

fn set_gc_running(host: &mut vm::HostContext<'_>, running: bool) -> Result<Value, VmError> {
    // Control only. Do not publish roots, pop a step budget, or touch a saved tail.
    let control = host
        .scope_state_or_insert_with(fresh_run_control)
        .map_err(|error| VmError::HostError(error.to_string()))?;
    control.running = running;
    Ok(Value::Float(0.0))
}

fn gc_restart(host: &mut vm::HostContext<'_>) -> Result<Value, VmError> {
    // Current logical amount only. Do not install a heap, collect, or pop a budget.
    let bytes = host
        .scope_state::<heap::LuaHeap>()
        .map(|heap| heap.logical_bytes())
        .unwrap_or(0);
    let control = host
        .scope_state_or_insert_with(fresh_run_control)
        .map_err(|error| VmError::HostError(error.to_string()))?;
    control.running = true;
    control.threshold_bytes = Some(bytes);
    control.auto_requested = false;
    Ok(Value::Float(0.0))
}

fn gc_is_running(host: &vm::HostContext<'_>) -> Value {
    Value::Bool(gc_running(host))
}

/// Read the guest switch after the VM has already stopped. A HostError cannot
/// call `isrunning`. This read does not restart, step, or gather.
pub(super) fn gc_running(host: &vm::HostContext<'_>) -> bool {
    host.scope_state::<GcRunControl>()
        .map(|control| control.running)
        .unwrap_or(true)
}

fn restore_running_after_progress(host: &mut vm::HostContext<'_>) -> Result<(), VmError> {
    // A successful explicit collect, or a successful step, reaches here.
    // Success includes a partial step: Bool false does not record completion,
    // but still restores running. Omitted data, a static default literal, and
    // a runtime expression that already passed the data check share that step.
    // A refusal returns before this call, so it cannot record a completion or
    // skip the heap gate by flipping the switch.
    if let Some(control) = host.scope_state_mut::<GcRunControl>() {
        control.running = true;
    }
    Ok(())
}

/// Counts real `step_data` calls. A literal fallback never reaches this host.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct StepDataNotes {
    pub calls: u32,
    pub number_zero: bool,
    pub text_zero: bool,
}

pub(super) fn step_data_notes(host: &vm::HostContext<'_>) -> StepDataNotes {
    host.scope_state::<StepDataNotes>()
        .copied()
        .unwrap_or_default()
}

fn note_step_data(call: &mut OwnedHostCall<'_>, value: &Value) -> Result<(), VmError> {
    let mut host = call.vm().host_context();
    let notes = host
        .scope_state_or_insert_with(StepDataNotes::default)
        .map_err(|error| VmError::HostError(error.to_string()))?;
    notes.calls = notes.calls.saturating_add(1);
    notes.number_zero = matches!(value, Value::Float(number) if *number == 0.0);
    notes.text_zero = matches!(value, Value::String(text) if text.as_bytes() == b"0")
        || matches!(value, Value::Bytes(bytes) if bytes.as_ref() == b"0");
    Ok(())
}

fn step_data_default(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    // Category only. Do not pop a budget, read a tail, or gather.
    // Static nil, the byte string "0", and the positive-zero number token
    // never call this host; they take the explicit-default arm. An expression
    // that expr() lowers arrives here as f64, and both zero sign bits compare
    // equal. Source unary `-0` is not an expr(): lowering rejects it, so it
    // never arrives. Source unary + is unsupported. Host Int is not a Lua
    // number and is not normalized to zero.
    let value = match call.args() {
        [value] => value.clone(),
        _ => {
            return Err(VmError::HostError(
                "step data is not the default".to_string(),
            ));
        }
    };
    note_step_data(call, &value)?;
    let accepted = match &value {
        Value::Null => true,
        Value::Float(number) => *number == 0.0,
        Value::String(text) => text.as_bytes() == b"0",
        Value::Bytes(bytes) => bytes.as_ref() == b"0",
        _ => false,
    };
    if accepted {
        Ok(Value::Bool(true))
    } else {
        Err(VmError::HostError(
            "step data is not the default".to_string(),
        ))
    }
}

fn pop_step_budget(host: &mut vm::HostContext<'_>) -> Result<NonZeroU64, VmError> {
    host.scope_state_mut::<StepBudgets>()
        .and_then(|budgets| budgets.0.pop_front())
        .ok_or_else(|| VmError::HostError("missing source step fixture budget".to_string()))
}

fn auto_table_due(host: &mut vm::HostContext<'_>) -> Result<bool, VmError> {
    let (running, threshold, requested) = match host.scope_state::<GcRunControl>() {
        Some(control) => (
            control.running,
            control.threshold_bytes,
            control.auto_requested,
        ),
        None => return Ok(false),
    };
    if !running || requested {
        return Ok(false);
    }
    let Some(threshold) = threshold else {
        return Ok(false);
    };
    // An absent heap is not an idle cycle. Do not install one here.
    if host.scope_state::<heap::LuaHeap>().is_none() {
        return Ok(false);
    }
    // A saved tail or mark is an existing cycle. Leave the ordinary allocation gate.
    if heap::saved_source_tail(host) || !heap::fixed_step_idle(host).map_err(heap_error)? {
        return Ok(false);
    }
    let logical = host
        .scope_state::<heap::LuaHeap>()
        .map(|heap| heap.logical_bytes())
        .unwrap_or(0);
    Ok(logical >= threshold)
}

/// Read-only snapshot for the once-only witness. Does not decode roots, pop a
/// budget, set the latch, or start a cycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AutoTableMeter {
    pub logical: u64,
    pub threshold: u64,
    pub running: bool,
    pub requested: bool,
    pub due: bool,
}

pub(super) fn auto_table_meter(host: &mut vm::HostContext<'_>) -> Result<AutoTableMeter, VmError> {
    let due = auto_table_due(host)?;
    let logical = host
        .scope_state::<heap::LuaHeap>()
        .map(|heap| heap.logical_bytes())
        .unwrap_or(0);
    let (running, threshold, requested) = match host.scope_state::<GcRunControl>() {
        Some(control) => (
            control.running,
            control.threshold_bytes.unwrap_or(0),
            control.auto_requested,
        ),
        None => (false, 0, false),
    };
    Ok(AutoTableMeter {
        logical,
        threshold,
        running,
        requested,
        due,
    })
}

fn confirm_round_finished(host: &mut vm::HostContext<'_>) -> Result<(), VmError> {
    if heap::saved_source_tail(host) || !heap::fixed_step_idle(host).map_err(heap_error)? {
        return Err(VmError::HostError(
            "auto table restart left a cycle open before publish".to_string(),
        ));
    }
    Ok(())
}

/// Decoded automatic batch measured in the same scope, before a budget pop.
///
/// `planned` is `roots.len() + batch`. Empty fixed slots are in `root_slots`;
/// a live scalar is in neither count. Recording this does not install a heap,
/// pop a budget, set the latch, or open a mark.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AutoRootPlan {
    pub phase: Option<&'static str>,
    pub root_slots: u32,
    pub batch: u32,
    pub planned: u32,
}

pub(super) fn auto_root_plan(host: &vm::HostContext<'_>) -> Option<AutoRootPlan> {
    host.scope_state::<AutoRootPlan>().copied()
}

/// One function-local empty table that the next `NEW_TABLE` may publish
/// into the saved Release tail. Not a permission for ordinary `alloc`.
struct ReleaseAllocPermit {
    slot: u32,
    consumed: bool,
}

/// The top-frame root already written for that permit. `ROOT_ISSUED` records
/// the witness and does not call the ordinary writer.
struct ReleaseRootAck {
    table: heap::TableRef,
    slot: u32,
    pending: bool,
}

/// Test-only refusal after the Core token is registered. Not a source example.
#[derive(Clone, Copy, Debug)]
pub(super) struct ReleaseAllocFault;

pub(super) struct ReleaseAllocPostCommitFault;

pub(super) fn arm_release_alloc_fault(host: &mut vm::HostContext<'_>) -> Result<(), VmError> {
    host.scope_state_or_insert_with(|| ReleaseAllocFault)
        .map(|_| ())
        .map_err(|error| VmError::HostError(error.to_string()))
}

pub(super) fn arm_release_alloc_post_commit_fault(
    host: &mut vm::HostContext<'_>,
) -> Result<(), VmError> {
    host.scope_state_or_insert_with(|| ReleaseAllocPostCommitFault)
        .map(|_| ())
        .map_err(|error| VmError::HostError(error.to_string()))
}

pub(super) fn release_alloc_permit(host: &vm::HostContext<'_>) -> Option<(u32, bool)> {
    host.scope_state::<ReleaseAllocPermit>()
        .map(|permit| (permit.slot, permit.consumed))
}

fn release_handoff_slot(flag: Option<&Value>) -> Option<u32> {
    let Value::Float(flag) = flag? else {
        return None;
    };
    if !flag.is_finite() || *flag < 1.0 || flag.fract() != 0.0 {
        return None;
    }
    let slot = (*flag as u64).checked_sub(1)?;
    u32::try_from(slot).ok()
}

fn note_auto_root_plan(host: &mut vm::HostContext<'_>, batch: usize) -> Result<(), VmError> {
    let (phase, root_slots) = {
        let Some(heap) = host.scope_state::<heap::LuaHeap>() else {
            return Err(VmError::HostError(
                "auto root plan has no installed heap".to_string(),
            ));
        };
        (
            heap.saved_mark_query().map(|(phase, _)| phase),
            heap.stats().root_slots,
        )
    };
    let batch = u32::try_from(batch)
        .map_err(|_| VmError::HostError("auto root plan batch does not fit u32".to_string()))?;
    let planned = root_slots
        .checked_add(batch)
        .ok_or_else(|| VmError::HostError("auto root plan overflow".to_string()))?;
    host.scope_state_or_insert_with(|| AutoRootPlan {
        phase,
        root_slots,
        batch,
        planned,
    })
    .map_err(|error| VmError::HostError(error.to_string()))?;
    Ok(())
}

/// One idle empty-local safepoint after single-arg restart.
///
/// The mark allowance and, only when that call publishes a tail, the
/// continuation allowance are separate host phase budgets. Pending close,
/// token close, and the Owners walk stay on the existing driver's own meters
/// and are not limited by either allowance. This does not call `heap::collect`
/// and does not treat a saved tail as idle.
fn prepare_automatic_release(host: &mut vm::HostContext<'_>, slot: u32) -> Result<bool, VmError> {
    let admitted = heap::automatic_release_slot_admitted(host, slot).map_err(heap_error)?;
    if !admitted {
        return Ok(false);
    }
    if host
        .scope_state::<ReleaseAllocPermit>()
        .is_some_and(|permit| !permit.consumed)
    {
        return Err(VmError::HostError(
            "automatic release allocation is already pending".to_string(),
        ));
    }
    let permit = ReleaseAllocPermit {
        slot,
        consumed: false,
    };
    if let Some(existing) = host.scope_state_mut::<ReleaseAllocPermit>() {
        *existing = permit;
    } else {
        host.scope_state_or_insert_with(|| permit)
            .map_err(|error| VmError::HostError(error.to_string()))?;
    }
    Ok(true)
}

fn take_automatic_release_table(host: &mut vm::HostContext<'_>) -> Result<Option<Value>, VmError> {
    let Some(slot) = host.scope_state::<ReleaseAllocPermit>().and_then(|permit| {
        if permit.consumed {
            None
        } else {
            Some(permit.slot)
        }
    }) else {
        return Ok(None);
    };
    if let Some(permit) = host.scope_state_mut::<ReleaseAllocPermit>() {
        permit.consumed = true;
    }
    let fail = host.scope_state::<ReleaseAllocFault>().is_some();
    let fail_after_commit = host.scope_state::<ReleaseAllocPostCommitFault>().is_some();
    let value = heap::publish_automatic_release_table(host, slot, fail, fail_after_commit)
        .map_err(heap_error)?;
    let Some(value) = value else {
        return Err(VmError::HostError(
            "automatic release allocation was not admitted at NEW_TABLE".to_string(),
        ));
    };
    let table = match classify(host, &value)? {
        heap::GuestClass::Table(table) => table,
        _ => {
            return Err(VmError::HostError(
                "automatic release carrier is not a table".to_string(),
            ));
        }
    };
    let ack = ReleaseRootAck {
        table,
        slot,
        pending: true,
    };
    if let Some(existing) = host.scope_state_mut::<ReleaseRootAck>() {
        *existing = ack;
    } else {
        host.scope_state_or_insert_with(|| ack)
            .map_err(|error| VmError::HostError(error.to_string()))?;
    }
    Ok(Some(value))
}

fn acknowledge_release_root(
    host: &mut vm::HostContext<'_>,
    table: heap::TableRef,
    slot: u32,
) -> Result<bool, VmError> {
    let matches = host
        .scope_state::<ReleaseRootAck>()
        .is_some_and(|ack| ack.pending && ack.table == table && ack.slot == slot);
    if matches && let Some(ack) = host.scope_state_mut::<ReleaseRootAck>() {
        ack.pending = false;
    }
    Ok(matches)
}

/// One idle empty-local safepoint after single-arg restart.
///
/// The mark allowance and, only when that call publishes a tail, the
/// continuation allowance are separate host phase budgets. Pending close,
/// token close, and the Owners walk stay on the existing driver's own meters
/// and are not limited by either allowance. This does not call `heap::collect`
/// and does not treat a saved tail as idle.
fn auto_table_restart(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    if !auto_table_due(&mut call.vm().host_context())? {
        return Ok(Value::Float(0.0));
    }
    let tables = decode_safepoint_roots(call)?;
    // A positive integer is the function-local root slot plus one. The next
    // statements are NEW_TABLE and ROOT_ISSUED. Chunk locals and direct calls
    // pass 0. Gray still only continues when that slot is present; the slot
    // number is used by the Release handoff, not by the gray pause.
    let handoff_slot = release_handoff_slot(call.args().get(1));
    let root_handoff = handoff_slot.is_some();
    {
        let mut host = call.vm().host_context();
        note_auto_root_plan(&mut host, tables.len())?;
        heap::preflight_automatic_root_capacity(&host, tables.len()).map_err(heap_error)?;
    }
    let mut host = call.vm().host_context();
    let mark_budget = pop_step_budget(&mut host)?;
    if let Some(control) = host.scope_state_mut::<GcRunControl>() {
        control.auto_requested = true;
    }
    note_safepoint_batch(&mut host, &tables)?;
    let first = heap::step_source_safepoint(&mut host, mark_budget, tables).map_err(heap_error)?;
    match first {
        heap::StepOutcome::Completed(report) => {
            note_collect(&mut host, report)?;
            confirm_round_finished(&mut host)?;
        }
        heap::StepOutcome::Published { .. } => {
            let sweep_budget = pop_step_budget(&mut host)?;
            let second = heap::step_source_safepoint(&mut host, sweep_budget, Vec::new())
                .map_err(heap_error)?;
            match second {
                heap::StepOutcome::Completed(report) => {
                    note_collect(&mut host, report)?;
                    confirm_round_finished(&mut host)?;
                }
                heap::StepOutcome::Paused { tail: true } => {
                    let unfinished = "auto table restart did not finish the published tail: Paused { tail: true }".to_string();
                    let Some(slot) = handoff_slot else {
                        return Err(VmError::HostError(unfinished));
                    };
                    if !prepare_automatic_release(&mut host, slot)? {
                        return Err(VmError::HostError(unfinished));
                    }
                }
                other => {
                    return Err(VmError::HostError(format!(
                        "auto table restart did not finish the published tail: {other:?}"
                    )));
                }
            }
        }
        heap::StepOutcome::Paused { tail: false }
            if root_handoff && matches!(heap::saved_mark_query(&host), Some(("gray", true))) =>
        {
            return Ok(Value::Float(0.0));
        }
        heap::StepOutcome::Paused { tail } => {
            return Err(VmError::HostError(format!(
                "auto table restart mark paused tail={tail}"
            )));
        }
    }
    Ok(Value::Float(0.0))
}

fn step_at_safepoint(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let mut host = call.vm().host_context();
    let budget = host
        .scope_state_mut::<StepBudgets>()
        .and_then(|budgets| budgets.0.pop_front())
        .ok_or_else(|| VmError::HostError("missing source step fixture budget".to_string()))?;
    let tables = if heap::saved_source_tail(&host) {
        Vec::new()
    } else {
        decode_safepoint_roots(call)?
    };
    let mut host = call.vm().host_context();
    note_safepoint_batch(&mut host, &tables)?;
    let outcome = heap::step_source_safepoint(&mut host, budget, tables).map_err(heap_error)?;
    if let heap::StepOutcome::Completed(report) = outcome {
        note_collect(&mut host, report)?;
        Ok(Value::Bool(true))
    } else {
        Ok(Value::Bool(false))
    }
}

fn collect_at_safepoint(
    host: &mut vm::HostContext<'_>,
    roots: &[heap::TableRef],
) -> Result<(), VmError> {
    let frame = heap::with_heap(host, |heap| {
        if roots.is_empty() {
            return Ok(None);
        }
        let frame = heap
            .enter_roots(u32::try_from(roots.len()).map_err(|_| heap::HeapError::RootCapacity)?)?;
        for (index, root) in roots.iter().enumerate() {
            if let Err(error) = heap.write_root(frame, index as u32, *root) {
                heap.leave_roots(frame)?;
                return Err(error);
            }
        }
        Ok(Some(frame))
    })
    .map_err(heap_error)?;
    let report = heap::collect(host);
    if let Some(frame) = frame {
        heap::with_heap(host, |heap| heap.leave_roots(frame)).map_err(heap_error)?;
    }
    let report = report.map_err(heap_error)?;
    note_collect(host, report)
}

fn heap_error(error: heap::HeapError) -> VmError {
    VmError::HostError(error.to_string())
}

fn classify(host: &mut vm::HostContext<'_>, value: &Value) -> Result<heap::GuestClass, VmError> {
    heap::with_heap(host, |heap| Ok(heap.guest_class(value))).map_err(heap_error)
}

pub(super) fn lower(source: &str, catalog: &HostApiCatalog) -> Result<FrontendIr, ParseError> {
    lower_returning(source, catalog, true)
}

pub(super) fn lower_returning(
    source: &str,
    catalog: &HostApiCatalog,
    publish_results: bool,
) -> Result<FrontendIr, ParseError> {
    let chunk = parser::parse_chunk(source.as_bytes())
        .map_err(|error| ParseError::at_line(error.line as usize, error.message))?;
    let resolution = resolve::resolve(&chunk);
    if let Some(error) = resolution.errors.first() {
        return Err(ParseError::at_line(1, format!("{error:?}")));
    }
    let mut lower = Lower {
        source,
        catalog,
        builder: LocalIrBuilder::new(),
        locals: HashMap::new(),
        live_locals: Vec::new(),
        active_temps: Vec::new(),
        stmts: Vec::new(),
        functions: HashMap::new(),
        function_impls: HashMap::new(),
        caller_frame: None,
        receiver_roots: HashMap::new(),
        next_receiver_slot: 0,
        receiver_width: chunk_receiver_width(&chunk.body.stmts),
        table_locals: HashSet::new(),
        publish_results,
        in_function: false,
        return_names: HashSet::new(),
        root_slot: 0,
        ignored_slot: None,
        resolution: &resolution,
        binding_slots: vec![None; resolution.bindings.len()],
        capture_envelopes: HashMap::new(),
        shadows: HashMap::new(),
    };
    if lower.receiver_width > 0 {
        let slot = lower.builder.alloc_local_named("#caller-frame")?;
        let enter = lower.call(ENTER_RESULT, vec![Expr::Float(lower.receiver_width as f64)])?;
        lower.stmts.push(Stmt::Let {
            index: slot,
            declared_schema: None,
            expr: enter,
            line: 1,
        });
        lower.caller_frame = Some(slot);
    }
    for stmt in &chunk.body.stmts {
        lower.lower_stmt(stmt)?;
    }
    // Statements, including an explicit scalar return, have already run.
    // A chunk with no receiver slots never opened this frame, so it does not
    // leave one. Fallthrough has no result expression here.
    if let Some(frame) = lower.caller_frame {
        let retire = lower.call(RETIRE_CHUNK, vec![Expr::Var(frame)])?;
        lower.emit_ignored(retire, 1)?;
    }
    let mut ir = lower.builder.finish(lower.stmts);
    ir.function_impls = lower.function_impls;
    Ok(ir)
}

/// Wrap one real, already-lowered chunk as an exported script callable.
///
/// The body is the chunk's own statements, including the Lua callee call and
/// the normal `RETIRE_CHUNK` completion. The wrapper only connects the
/// compiler's export slot. It does not emit a second root lifecycle.
pub(super) fn lower_exported_chunk(
    source: &str,
    catalog: &HostApiCatalog,
    publish_results: bool,
) -> Result<FrontendIr, ParseError> {
    let mut ir = lower_returning(source, catalog, publish_results)?;
    let body_expr = connect_scalar_return(&mut ir)?;
    let index = u16::try_from(ir.functions.len())
        .map_err(|_| ParseError::at_line(1, "exported chunk exceeds the function index space"))?;
    if ir.functions.iter().any(|func| func.index == index)
        || ir.function_impls.contains_key(&index)
        || ir.functions.iter().any(|func| func.name == EXPORTED_CHUNK)
    {
        return Err(ParseError::at_line(
            1,
            "exported chunk collides with an existing function",
        ));
    }
    ir.functions.push(FunctionDecl {
        name: EXPORTED_CHUNK.to_string(),
        arity: 0,
        index,
        args: Vec::new(),
        arg_schemas: Vec::new(),
        return_schema: None,
        type_params: Vec::new(),
        exported: true,
        return_type: vm::ValueType::Unknown,
        symbol: None,
    });
    ir.function_sources.insert(index, source.to_string());
    ir.function_impls.insert(
        index,
        FunctionImpl {
            param_slots: Vec::new(),
            capture_copies: Vec::new(),
            body_stmts: std::mem::take(&mut ir.stmts),
            body_expr,
            body_expr_line: 1,
        },
    );
    Ok(ir)
}

/// Keep the chunk's scalar return in a local so the script `Ret` can load it
/// after `RETIRE_CHUNK`. A chunk with no return stays a null fallthrough.
fn connect_scalar_return(ir: &mut FrontendIr) -> Result<Expr, ParseError> {
    let Some(return_index) = ir
        .functions
        .iter()
        .find(|func| func.name == RETURN_SCALAR)
        .map(|func| func.index)
    else {
        return Ok(Expr::Null);
    };
    let mut found = None;
    for (index, stmt) in ir.stmts.iter().enumerate() {
        let Stmt::Expr { expr, .. } = stmt else {
            continue;
        };
        let Expr::Call(call_index, ..) = expr else {
            continue;
        };
        if *call_index != return_index {
            continue;
        }
        if found.is_some() {
            return Err(ParseError::at_line(
                1,
                "exported chunk has more than one scalar return",
            ));
        }
        found = Some(index);
    }
    let Some(stmt_index) = found else {
        return Err(ParseError::at_line(
            1,
            "scalar return host is declared but the chunk has no return statement",
        ));
    };
    let slot = u16::try_from(ir.locals).map_err(|_| {
        ParseError::at_line(1, "exported scalar slot exceeds the local index space")
    })?;
    ir.locals += 1;
    ir.local_bindings.push(("#export-scalar".to_string(), slot));
    let Stmt::Expr { expr, line } = ir.stmts.remove(stmt_index) else {
        return Err(ParseError::at_line(
            1,
            "scalar return statement disappeared",
        ));
    };
    ir.stmts.insert(
        stmt_index,
        Stmt::Let {
            index: slot,
            declared_schema: None,
            expr,
            line,
        },
    );
    Ok(Expr::Var(slot))
}

struct Lower<'a> {
    source: &'a str,
    catalog: &'a HostApiCatalog,
    builder: LocalIrBuilder,
    locals: HashMap<Vec<u8>, LocalSlot>,
    live_locals: Vec<LocalSlot>,
    active_temps: Vec<LocalSlot>,
    stmts: Vec<Stmt>,
    functions: HashMap<Vec<u8>, FuncInfo>,
    function_impls: HashMap<u16, FunctionImpl>,
    caller_frame: Option<LocalSlot>,
    receiver_roots: HashMap<LocalSlot, u32>,
    next_receiver_slot: u32,
    receiver_width: u32,
    table_locals: HashSet<Vec<u8>>,
    publish_results: bool,
    in_function: bool,
    return_names: HashSet<Vec<u8>>,
    root_slot: u32,
    ignored_slot: Option<LocalSlot>,
    resolution: &'a resolve::Resolution,
    binding_slots: Vec<Option<LocalSlot>>,
    capture_envelopes: HashMap<usize, LocalSlot>,
    /// Callable slot to the issued envelope that roots that callable's cell.
    shadows: HashMap<LocalSlot, LocalSlot>,
}

#[derive(Clone, Copy)]
struct FuncInfo {
    index: u16,
    callee_width: u32,
    return_count: u32,
}

impl Lower<'_> {
    fn lower_stmt(&mut self, stmt: &ast::Stmt) -> Result<(), ParseError> {
        match stmt {
            ast::Stmt::LocalFunction {
                span, name, func, ..
            } => self.lower_function(name, func, *span),
            ast::Stmt::Local {
                span,
                names,
                values,
            } if values.exprs.len() == 1
                && self.direct_function_call(&values.exprs[0]).is_some() =>
            {
                self.lower_call_local(*span, names, &values.exprs[0])
            }
            ast::Stmt::Local {
                span,
                names,
                values,
            } if values.exprs.len() == 1 && self.is_next_call(&values.exprs[0]) => {
                self.lower_next_local(*span, names, &values.exprs[0])
            }
            ast::Stmt::Local {
                span,
                names,
                values,
            } if let ([name], [value]) = (names.as_slice(), values.exprs.as_slice()) => {
                self.lower_local(*span, &name.name, value)
            }
            ast::Stmt::Assign {
                span,
                targets,
                values,
            } if targets.len() >= 2
                && targets.len() == values.exprs.len()
                && targets.iter().all(plain_named_field) =>
            {
                self.lower_parallel_named_fields(*span, targets, &values.exprs)
            }
            ast::Stmt::Assign {
                span,
                targets,
                values,
            } if let ([target], [value]) = (targets.as_slice(), values.exprs.as_slice()) => {
                self.lower_assign(*span, target, value)
            }
            ast::Stmt::Call { span, call } => {
                let expr = self.expr(call)?;
                self.emit_ignored(expr, self.line(*span))
            }
            ast::Stmt::Return { span, values } if !self.in_function && values.exprs.len() == 1 => {
                // One public chunk return. `CallReturn::Many` is not a
                // non-yielding host result, so an unparenthesized tail `next`
                // is the existing unsupported path rather than a second
                // protocol. `return (next(t))` still exports the first value.
                if self.is_next_call(&values.exprs[0]) {
                    return Err(self.unsupported(*span));
                }
                let value = self.expr(&values.exprs[0])?;
                let expr = self.call(RETURN_SCALAR, vec![value])?;
                self.stmts.push(Stmt::Expr {
                    expr,
                    line: self.line(*span),
                });
                Ok(())
            }
            ast::Stmt::Empty { .. } => Ok(()),
            ast::Stmt::Do { span, body } if !self.in_function => self.lower_do(*span, body, None),
            other => Err(ParseError::at_line(
                self.line(stmt_span(other)) as usize,
                "Lua statement not lowered yet",
            )),
        }
    }

    /// Normal exit of one chunk `do`. Slots declared in this block leave
    /// `live_locals`. Same-name outer slots stay where they are, including a
    /// slot already removed by `= nil`. A function body is not this path.
    ///
    /// A same-block forward `goto` still uses `retained_statements`. One direct
    /// child `do` may instead carry `pending`: the resolver target span of its
    /// single unconditional `goto`, already known to be a later direct label
    /// of this block. That child lowers its prefix and `retire_block` drops
    /// only its slots. This walk then continues after the label. `retire_block`
    /// still runs once, at this statement's end, not at the label.
    fn lower_do(
        &mut self,
        span: ast::Span,
        body: &ast::Block,
        pending: Option<ast::Span>,
    ) -> Result<(), ParseError> {
        if let Some(label_span) = pending {
            let goto_index = self
                .goto_targeting(body, label_span)
                .ok_or_else(|| self.statement_unsupported(span))?;
            for stmt in &body.stmts[..goto_index] {
                if !matches!(stmt, ast::Stmt::Label { .. }) {
                    self.lower_stmt(stmt)?;
                }
            }
            return self.retire_block(span, body);
        }
        // A same-block skip owns this block. Do not also structure a nested
        // jump; `forward_goto_skip` already drops or keeps that child.
        if self.forward_goto_skip(body)?.is_none()
            && let Some((child_index, label_index, label_span)) = self.outward_forward_goto(body)?
        {
            for (index, stmt) in body.stmts.iter().enumerate() {
                if matches!(stmt, ast::Stmt::Label { .. }) {
                    continue;
                }
                if index == child_index {
                    let ast::Stmt::Do {
                        span: child_span,
                        body: child_body,
                    } = stmt
                    else {
                        return Err(self.statement_unsupported(stmt_span(stmt)));
                    };
                    self.lower_do(*child_span, child_body, Some(label_span))?;
                    continue;
                }
                if index > child_index && index < label_index {
                    continue;
                }
                self.lower_stmt(stmt)?;
            }
            return self.retire_block(span, body);
        }
        for stmt in self.retained_statements(body)? {
            self.lower_stmt(stmt)?;
        }
        self.retire_block(span, body)
    }

    /// Direct child `do` whose one direct `goto` resolves to a later direct
    /// label of `body`. The label is `GotoUse::target`'s span, not a name
    /// match. A backward target or a second such child is unsupported. A
    /// target outside this block is left for that child to reject.
    fn outward_forward_goto(
        &self,
        body: &ast::Block,
    ) -> Result<Option<(usize, usize, ast::Span)>, ParseError> {
        let mut found = None;
        for (index, stmt) in body.stmts.iter().enumerate() {
            let ast::Stmt::Do { body: child, .. } = stmt else {
                continue;
            };
            let mut goto_span = None;
            for child_stmt in &child.stmts {
                let ast::Stmt::Goto { span, .. } = child_stmt else {
                    continue;
                };
                if goto_span.is_some() {
                    goto_span = None;
                    break;
                }
                goto_span = Some(*span);
            }
            let Some(goto_span) = goto_span else {
                continue;
            };
            let Some(label) = self.chunk_target_label(goto_span) else {
                continue;
            };
            let Some(label_index) = body.stmts.iter().position(
                |stmt| matches!(stmt, ast::Stmt::Label { name, .. } if name.span == label.span),
            ) else {
                continue;
            };
            if label_index <= index || found.is_some() {
                return Err(self.statement_unsupported(goto_span));
            }
            found = Some((index, label_index, label.span));
        }
        Ok(found)
    }

    /// Index of the one direct `goto` whose resolver target span is `label_span`.
    fn goto_targeting(&self, body: &ast::Block, label_span: ast::Span) -> Option<usize> {
        let mut found = None;
        for (index, stmt) in body.stmts.iter().enumerate() {
            let ast::Stmt::Goto { span, .. } = stmt else {
                continue;
            };
            let label = self.chunk_target_label(*span)?;
            if label.span != label_span || found.is_some() {
                return None;
            }
            found = Some(index);
        }
        found
    }

    fn chunk_target_label(&self, goto_span: ast::Span) -> Option<&resolve::LabelBinding> {
        let jump = self
            .resolution
            .gotos
            .iter()
            .find(|jump| jump.span == goto_span && jump.function_id == 0)?;
        let label = self.resolution.labels.get(jump.target?)?;
        (label.function_id == 0).then_some(label)
    }

    /// Statements this chunk `do` lowers, in source order. Labels are not
    /// runtime statements. Empty statements stay, including a trailing `;`.
    ///
    /// With no direct `goto`, that is the whole block. One direct unconditional
    /// `goto` whose resolver target is a later direct child keeps the prefix
    /// and the statements after that label, and drops the jump plus the
    /// statements between them. A trailing label is the same shape: only
    /// labels and empty statements follow it, so the kept tail is those
    /// semicolons. The target id is matched by label span, not by name.
    fn retained_statements<'b>(
        &self,
        body: &'b ast::Block,
    ) -> Result<Vec<&'b ast::Stmt>, ParseError> {
        let skip = self.forward_goto_skip(body)?;
        let mut kept = Vec::new();
        for (index, stmt) in body.stmts.iter().enumerate() {
            if matches!(stmt, ast::Stmt::Label { .. }) {
                continue;
            }
            if let Some((goto_index, label_index)) = skip
                && index >= goto_index
                && index < label_index
            {
                continue;
            }
            kept.push(stmt);
        }
        Ok(kept)
    }

    /// Bounds of the one direct forward `goto` this block can structure.
    /// `None` is the straight path. The resolver target must be a later
    /// direct child of this block. Another goto whose target lies in the
    /// skipped span is excluded: that shape needs a graph, and this path
    /// does not invent one. A second direct goto, a backward jump, or a
    /// target in another block stays unsupported instead of falling through
    /// the skipped statements.
    fn forward_goto_skip(&self, body: &ast::Block) -> Result<Option<(usize, usize)>, ParseError> {
        let mut found = None;
        for (index, stmt) in body.stmts.iter().enumerate() {
            let ast::Stmt::Goto {
                span: goto_span, ..
            } = stmt
            else {
                continue;
            };
            if found.is_some() {
                return Err(self.statement_unsupported(*goto_span));
            }
            found = Some((index, *goto_span));
        }
        let Some((goto_index, goto_span)) = found else {
            return Ok(None);
        };
        let jump = self
            .resolution
            .gotos
            .iter()
            .find(|jump| jump.span == goto_span && jump.function_id == 0);
        let Some(jump) = jump else {
            return Err(self.statement_unsupported(goto_span));
        };
        let Some(target) = jump.target else {
            return Err(self.statement_unsupported(goto_span));
        };
        let Some(label) = self.resolution.labels.get(target) else {
            return Err(self.statement_unsupported(goto_span));
        };
        if label.function_id != 0 {
            return Err(self.statement_unsupported(goto_span));
        }
        let Some(label_index) = body.stmts.iter().position(
            |stmt| matches!(stmt, ast::Stmt::Label { name, .. } if name.span == label.span),
        ) else {
            return Err(self.statement_unsupported(goto_span));
        };
        if label_index <= goto_index
            || self.outside_goto_enters_skip(body, goto_index, label_index, goto_span)
        {
            return Err(self.statement_unsupported(goto_span));
        }
        Ok(Some((goto_index, label_index)))
    }

    /// True when some other goto targets a label inside the skipped statements.
    /// The structuring goto itself lands on the label after that span.
    fn outside_goto_enters_skip(
        &self,
        body: &ast::Block,
        goto_index: usize,
        label_index: usize,
        goto_span: ast::Span,
    ) -> bool {
        let skipped = &body.stmts[goto_index + 1..label_index];
        let Some(first) = skipped.first() else {
            return false;
        };
        let start = stmt_span(first).start;
        let end = stmt_span(skipped.last().expect("skipped statement")).end;
        self.resolution.gotos.iter().any(|jump| {
            if jump.span == goto_span {
                return false;
            }
            let Some(target) = jump.target else {
                return true;
            };
            let Some(label) = self.resolution.labels.get(target) else {
                return true;
            };
            let outside = jump.span.end <= start || jump.span.start >= end;
            let inside = label.span.start >= start && label.span.end <= end;
            outside && inside
        })
    }

    fn statement_unsupported(&self, span: ast::Span) -> ParseError {
        ParseError::at_line(self.line(span) as usize, "Lua statement not lowered yet")
    }

    fn retire_block(&mut self, span: ast::Span, body: &ast::Block) -> Result<(), ParseError> {
        let Some(scope_id) = self.block_scope(span, body) else {
            return Ok(());
        };
        let ended: Vec<usize> = self
            .resolution
            .bindings
            .iter()
            .enumerate()
            .filter(|(_, binding)| binding.function_id == 0 && binding.scope_id == scope_id)
            .map(|(index, _)| index)
            .collect();
        for index in ended {
            let Some(slot) = self.binding_slots[index] else {
                continue;
            };
            self.live_locals.retain(|local| *local != slot);
            if let Some(anchor) = self.capture_envelopes.remove(&index) {
                self.live_locals.retain(|local| *local != anchor);
            }
            if let Some(shadow) = self.shadows.remove(&slot) {
                self.live_locals.retain(|local| *local != shadow);
            }
            self.release_receiver_root(span, slot)?;
            let name = self.resolution.bindings[index].name.clone();
            if self.locals.get(&name).copied() == Some(slot) {
                if let Some(outer) = self.visible_slot(&name, span.end) {
                    self.locals.insert(name, outer);
                } else {
                    self.locals.remove(&name);
                }
            }
        }
        Ok(())
    }

    /// Drops one published caller-frame root when its local slot leaves scope
    /// or is assigned nil. A slot already cleared is absent from the map and
    /// is left alone. This is the caller frame recorded for the chunk, not
    /// the frame that is current while a callee runs.
    fn release_receiver_root(
        &mut self,
        span: ast::Span,
        slot: LocalSlot,
    ) -> Result<(), ParseError> {
        let Some(root_slot) = self.receiver_roots.remove(&slot) else {
            return Ok(());
        };
        let frame = self.caller_frame.ok_or_else(|| self.unsupported(span))?;
        let release = self.call(
            RELEASE_RESULT,
            vec![Expr::Var(frame), Expr::Float(root_slot as f64)],
        )?;
        self.emit_ignored(release, self.line(span))
    }

    /// Scope of the locals this `do` itself declares. Nested blocks have a
    /// different `visible_to`, and the chunk scope can share this statement's
    /// end byte when the block is last.
    fn block_scope(&self, span: ast::Span, body: &ast::Block) -> Option<u32> {
        self.resolution.bindings.iter().find_map(|binding| {
            let inside = binding.span.start >= body.span.start && binding.span.end <= body.span.end;
            if binding.function_id == 0 && inside && binding.visible_to == span.end {
                Some(binding.scope_id)
            } else {
                None
            }
        })
    }

    fn visible_slot(&self, name: &[u8], at: u32) -> Option<LocalSlot> {
        let mut best: Option<(usize, u32)> = None;
        for (index, binding) in self.resolution.bindings.iter().enumerate() {
            if binding.function_id != 0 || binding.name != name {
                continue;
            }
            if at < binding.visible_from || at >= binding.visible_to {
                continue;
            }
            if best.is_none_or(|(_, from)| binding.visible_from >= from) {
                best = Some((index, binding.visible_from));
            }
        }
        best.and_then(|(index, _)| self.binding_slots[index])
    }

    fn remember_binding(
        &mut self,
        span: ast::Span,
        name: &ast::Name,
        slot: LocalSlot,
    ) -> Result<(), ParseError> {
        if !self.in_function {
            let index = self.chunk_binding(name, span)?;
            self.binding_slots[index] = Some(slot);
        }
        self.locals.insert(name.bytes.clone(), slot);
        Ok(())
    }

    fn chunk_binding(&self, name: &ast::Name, span: ast::Span) -> Result<usize, ParseError> {
        // Repeated names in one statement share visible_from. The resolver
        // stores each name token as Binding.span, which is the declaration.
        self.resolution
            .bindings
            .iter()
            .position(|binding| {
                binding.function_id == 0
                    && binding.name == name.bytes
                    && binding.span == name.span
                    && binding.visible_from == span.end
            })
            .ok_or_else(|| self.unsupported(span))
    }

    fn name_is_bound(&self, name: &ast::Name) -> bool {
        self.resolution
            .uses
            .iter()
            .any(|use_site| use_site.span == name.span && use_site.binding.is_some())
    }

    fn length_expr(&mut self, operand: &ast::Expr, span: ast::Span) -> Result<Expr, ParseError> {
        let operand = skip_parens(operand);
        self.reject_tail_next(operand, span)?;
        let value = self.length_operand(operand)?;
        let slot = self
            .builder
            .alloc_local_named(&format!("len:{}", self.stmts.len()))?;
        self.stmts.push(Stmt::Let {
            index: slot,
            declared_schema: None,
            expr: value,
            line: self.line(span),
        });
        self.call(STRING_BYTE_LEN, vec![Expr::Var(slot)])
    }

    fn rawlen_expr(&mut self, operand: &ast::Expr, span: ast::Span) -> Result<Expr, ParseError> {
        let operand = skip_parens(operand);
        self.reject_tail_next(operand, span)?;
        let value = self.length_operand(operand)?;
        let slot = self
            .builder
            .alloc_local_named(&format!("rawlen:{}", self.stmts.len()))?;
        self.stmts.push(Stmt::Let {
            index: slot,
            declared_schema: None,
            expr: value,
            line: self.line(span),
        });
        self.call(RAW_LEN, vec![Expr::Var(slot)])
    }

    fn length_operand(&mut self, expr: &ast::Expr) -> Result<Expr, ParseError> {
        match skip_parens(expr) {
            ast::Expr::String { bytes, .. } => Ok(self.string_literal(bytes)),
            other => self.expr(other),
        }
    }

    fn builtin_next(&self, name: &ast::Name) -> bool {
        name.is("next")
            && !self.functions.contains_key(&name.bytes)
            && !self.locals.contains_key(&name.bytes)
    }

    fn is_next_call(&self, expr: &ast::Expr) -> bool {
        let ast::Expr::SuffixChain { base, steps, .. } = expr else {
            return false;
        };
        let ast::Expr::Name { name, .. } = base.as_ref() else {
            return false;
        };
        if !self.builtin_next(name) {
            return false;
        }
        matches!(
            steps.as_slice(),
            [ast::Suffix::Call {
                args: ast::Args::List(_),
                nav: false,
                ..
            }]
        )
    }

    /// Last-argument adjustment is not implemented. Parentheses and a
    /// non-last argument still take one value through `expr`.
    fn reject_tail_next(&self, arg: &ast::Expr, span: ast::Span) -> Result<(), ParseError> {
        if self.is_next_call(arg) {
            Err(self.unsupported(span))
        } else {
            Ok(())
        }
    }

    /// `local k, v = next(...)` binds through the existing array transport.
    /// The RHS is lowered before the new names are bound, so
    /// `local next, v = next(t)` still sees the builtin.
    ///
    /// Chunk `return next(...)` is not this path. A missing cursor is
    /// `invalid key to next`, not a restart. A weak-cleared cursor is still
    /// stored, so `next` continues from it. `next` walks the owner's edges;
    /// that walk is the scan cost.
    fn lower_next_local(
        &mut self,
        span: ast::Span,
        names: &[ast::LocalName],
        call: &ast::Expr,
    ) -> Result<(), ParseError> {
        let line = self.line(span);
        let transport = self.next_transport(call, span)?;
        let mut slots = Vec::with_capacity(names.len());
        for (offset, name) in names.iter().enumerate() {
            let index = self
                .builder
                .alloc_local_named(&format!("#{offset}:{}", span.start))?;
            self.remember_binding(span, &name.name, index)?;
            self.live_locals.push(index);
            slots.push(index);
        }
        self.bind_transport(transport, &slots, line)
    }

    fn next_expr(&mut self, args: &ast::ExprList, span: ast::Span) -> Result<Expr, ParseError> {
        let transport = self.next_transport_exprs(&args.exprs, span)?;
        let get = vm::BuiltinFunction::Get.call_index();
        Ok(Expr::Call(
            get,
            vec![],
            vec![Expr::Var(transport), Expr::Int(0)],
            None,
            None,
        ))
    }

    fn next_transport(
        &mut self,
        call: &ast::Expr,
        span: ast::Span,
    ) -> Result<LocalSlot, ParseError> {
        let ast::Expr::SuffixChain { steps, .. } = call else {
            return Err(self.unsupported(span));
        };
        let [
            ast::Suffix::Call {
                args: ast::Args::List(list),
                ..
            },
        ] = steps.as_slice()
        else {
            return Err(self.unsupported(span));
        };
        self.next_transport_exprs(&list.exprs, span)
    }

    fn next_transport_exprs(
        &mut self,
        args: &[ast::Expr],
        span: ast::Span,
    ) -> Result<LocalSlot, ParseError> {
        // A fresh receiver is a temp root only while the cursor expression
        // runs. It is released with the call, so a later collect can reclaim
        // it. A missing or cleared cursor still errors; this walk is the scan.
        if args.last().is_some_and(|arg| self.is_next_call(arg)) {
            return Err(self.unsupported(span));
        }
        let (table, cursor) = match args {
            [table] => (table, None),
            [table, cursor] => (table, Some(cursor)),
            _ => return Err(self.unsupported(span)),
        };
        let temp_base = self.active_temps.len();
        let table = self.expr(table)?;
        let table = self.rooted_operand(table, span)?;
        let cursor = match cursor {
            Some(ast::Expr::Nil { .. }) => Expr::Null,
            Some(cursor) => self.expr(cursor)?,
            None => Expr::Null,
        };
        let cursor = self.operand_slot(cursor, span)?;
        let line = self.line(span);
        let transport = self
            .builder
            .alloc_local_named(&format!("#next:{}", span.start))?;
        let call = self.call(NEXT, vec![Expr::Var(table), Expr::Var(cursor)])?;
        self.stmts.push(Stmt::Let {
            index: transport,
            declared_schema: None,
            expr: call,
            line,
        });
        self.note_next_len(transport, line)?;
        self.active_temps.truncate(temp_base);
        Ok(transport)
    }

    /// Records Core `len` of the return transport. `next` itself does not
    /// write this count.
    fn note_next_len(&mut self, transport: LocalSlot, line: u32) -> Result<(), ParseError> {
        let len = vm::BuiltinFunction::Len.call_index();
        let count = self
            .builder
            .alloc_local_named(&format!("#next-len:{line}"))?;
        self.stmts.push(Stmt::Let {
            index: count,
            declared_schema: None,
            expr: Expr::Call(len, vec![], vec![Expr::Var(transport)], None, None),
            line,
        });
        let note = self.call(NOTE_NEXT_COUNT, vec![Expr::Var(count)])?;
        self.emit_ignored(note, line)
    }

    fn bind_transport(
        &mut self,
        transport: LocalSlot,
        slots: &[LocalSlot],
        line: u32,
    ) -> Result<(), ParseError> {
        let has = vm::BuiltinFunction::Has.call_index();
        let get = vm::BuiltinFunction::Get.call_index();
        for (index, slot) in slots.iter().enumerate() {
            self.stmts.push(Stmt::Let {
                index: *slot,
                declared_schema: None,
                expr: Expr::Null,
                line,
            });
            let present = self
                .builder
                .alloc_local_named(&format!("#next-has:{line}:{index}"))?;
            self.stmts.push(Stmt::Let {
                index: present,
                declared_schema: None,
                expr: Expr::Call(
                    has,
                    vec![],
                    vec![Expr::Var(transport), Expr::Int(index as i64)],
                    None,
                    None,
                ),
                line,
            });
            self.stmts.push(Stmt::IfElse {
                condition: Expr::Var(present),
                then_branch: vec![Stmt::Assign {
                    kind: AssignmentKind::Set,
                    index: *slot,
                    expr: Expr::Call(
                        get,
                        vec![],
                        vec![Expr::Var(transport), Expr::Int(index as i64)],
                        None,
                        None,
                    ),
                    line,
                }],
                else_branch: vec![],
                line,
            });
        }
        Ok(())
    }

    fn rooted_operand(&mut self, expr: Expr, span: ast::Span) -> Result<LocalSlot, ParseError> {
        if let Expr::Var(slot) = expr {
            return Ok(slot);
        }
        let slot = self.operand_slot(expr, span)?;
        self.active_temps.push(slot);
        Ok(slot)
    }

    fn operand_slot(&mut self, expr: Expr, span: ast::Span) -> Result<LocalSlot, ParseError> {
        if let Expr::Var(slot) = expr {
            return Ok(slot);
        }
        let slot = self
            .builder
            .alloc_local_named(&format!("#next-arg:{}", self.stmts.len()))?;
        self.stmts.push(Stmt::Let {
            index: slot,
            declared_schema: None,
            expr,
            line: self.line(span),
        });
        Ok(slot)
    }

    fn lower_local(
        &mut self,
        span: ast::Span,
        name: &ast::Name,
        value: &ast::Expr,
    ) -> Result<(), ParseError> {
        if self.auto_table_routed()
            && matches!(value, ast::Expr::Table { fields, .. } if fields.is_empty())
        {
            self.emit_auto_table_safepoint(span)?;
        }
        let expr = self.expr(value)?;
        let index = self
            .builder
            .alloc_local_named(&format!("#{}", span.start))?;
        self.remember_binding(span, name, index)?;
        let nil_init = matches!(value, ast::Expr::Nil { .. });
        let shadowed = self.copy_shadow(span, index, value)?;
        if !nil_init && !shadowed {
            self.live_locals.push(index);
        }
        self.stmts.push(Stmt::Let {
            index,
            declared_schema: None,
            expr,
            line: self.line(span),
        });
        if self.in_function && matches!(value, ast::Expr::Table { fields, .. } if fields.is_empty())
        {
            self.table_locals.insert(name.bytes.clone());
            let role = if self.return_names.contains(&name.bytes) {
                0.0
            } else {
                1.0
            };
            let slot = self.root_slot;
            self.root_slot += 1;
            let expr = self.call(
                ROOT_ISSUED,
                vec![
                    Expr::Var(index),
                    Expr::Float(slot as f64),
                    Expr::Float(role),
                ],
            )?;
            self.emit_ignored(expr, self.line(span))?;
        }
        if !nil_init {
            self.open_captured_cell(span, name, index)?;
        }
        Ok(())
    }

    fn auto_table_routed(&self) -> bool {
        self.catalog.function(AUTO_TABLE_DUE).is_some()
            && self.catalog.function(AUTO_TABLE_RESTART).is_some()
    }

    fn emit_auto_table_safepoint(&mut self, span: ast::Span) -> Result<(), ParseError> {
        // Only this source local. Shadow and cell constructors call NEW_TABLE
        // themselves and do not pass through here.
        //
        // The due call has no guest arguments, so the judgment does not read or
        // move a live value. Gather statements are reserved in the taken branch
        // and are not executed when the judgment is false. The restart entry
        // still refuses a direct call that is not due.
        let due = self.call(AUTO_TABLE_DUE, vec![])?;
        let start = self.stmts.len();
        let roots = self.gather_safepoint_roots(span)?;
        let gather = self.stmts.split_off(start);
        // Only a function-local empty table continues into NEW_TABLE and then
        // ROOT_ISSUED, with no guest safepoint between those statements.
        // The handoff float is that local's root slot plus one. Chunk locals,
        // and the direct probe, pass 0 and keep the pause error.
        let handoff = Expr::Float(if self.in_function {
            f64::from(self.root_slot) + 1.0
        } else {
            0.0
        });
        let taken = self.call(AUTO_TABLE_RESTART, vec![roots, handoff])?;
        let expr = Expr::IfElse {
            condition: Box::new(due),
            then_expr: Box::new(Expr::Block {
                stmts: gather,
                expr: Box::new(taken),
            }),
            else_expr: Box::new(Expr::Float(0.0)),
        };
        self.emit_ignored(expr, self.line(span))
    }

    fn open_captured_cell(
        &mut self,
        span: ast::Span,
        name: &ast::Name,
        slot: LocalSlot,
    ) -> Result<(), ParseError> {
        let Ok(binding) = self.chunk_binding(name, span) else {
            return Ok(());
        };
        if !self.binding_is_captured(binding) {
            return Ok(());
        }
        let anchor = self
            .builder
            .alloc_local_named(&format!("#cell:{binding}"))?;
        let expr = self.call(OPEN_CELL, vec![Expr::Var(slot)])?;
        self.stmts.push(Stmt::Let {
            index: anchor,
            declared_schema: None,
            expr,
            line: self.line(span),
        });
        self.capture_envelopes.insert(binding, anchor);
        self.live_locals.push(anchor);
        self.live_locals.retain(|local| *local != slot);
        self.stmts.push(Stmt::Assign {
            kind: AssignmentKind::Set,
            index: slot,
            expr: Expr::Null,
            line: self.line(span),
        });
        Ok(())
    }

    fn copy_shadow(
        &mut self,
        span: ast::Span,
        dest: LocalSlot,
        value: &ast::Expr,
    ) -> Result<bool, ParseError> {
        let ast::Expr::Name { name, .. } = value else {
            return Ok(false);
        };
        let Some(source) = self.locals.get(&name.bytes).copied() else {
            return Ok(false);
        };
        let Some(shadow) = self.shadows.get(&source).copied() else {
            return Ok(false);
        };
        let copied = self
            .builder
            .alloc_local_named(&format!("#shadow:{dest:?}"))?;
        self.stmts.push(Stmt::Let {
            index: copied,
            declared_schema: None,
            expr: Expr::Var(shadow),
            line: self.line(span),
        });
        self.shadows.insert(dest, copied);
        self.live_locals.push(copied);
        Ok(true)
    }

    fn binding_is_captured(&self, binding: usize) -> bool {
        let owner = self.resolution.bindings[binding].function_id;
        self.resolution
            .uses
            .iter()
            .any(|use_site| use_site.binding == Some(binding) && use_site.function_id != owner)
    }

    fn captures_in(&self, span: ast::Span) -> Vec<usize> {
        let mut found = Vec::new();
        for use_site in &self.resolution.uses {
            if use_site.span.start < span.start || use_site.span.end > span.end {
                continue;
            }
            let Some(binding) = use_site.binding else {
                continue;
            };
            if self.resolution.bindings[binding].function_id == use_site.function_id {
                continue;
            }
            if !found.contains(&binding) {
                found.push(binding);
            }
        }
        found
    }

    fn lower_closure_assign(
        &mut self,
        span: ast::Span,
        name: &ast::Name,
        func: &ast::FuncBody,
    ) -> Result<(), ParseError> {
        // Reads of this binding use the shared cell, which cannot hold a callable.
        if self.anchor_of_name(name).is_some() {
            return Err(self.unsupported(span));
        }
        if !func.params.is_empty() || func.vararg || func.self_param {
            return Err(self.unsupported(func.span));
        }
        let captures = self.captures_in(func.span);
        let [binding] = captures.as_slice() else {
            return Err(self.unsupported(func.span));
        };
        let anchor = self
            .capture_envelopes
            .get(binding)
            .copied()
            .ok_or_else(|| self.unsupported(func.span))?;
        let slot = self
            .locals
            .get(&name.bytes)
            .copied()
            .ok_or_else(|| self.unsupported(span))?;
        let shadow = self.fresh_shadow(span, slot)?;
        let captured = self
            .builder
            .alloc_local_named(&format!("#capture:{}", func.span.start))?;
        let body = self.closure_body(func, *binding, captured)?;
        self.stmts.push(Stmt::Assign {
            kind: AssignmentKind::Set,
            index: slot,
            expr: Expr::Closure(vm::ClosureExpr {
                param_slots: Vec::new(),
                capture_copies: vec![(shadow, captured)],
                body: Box::new(body),
            }),
            line: self.line(span),
        });
        let track = self.call(
            TRACK_CLOSURE,
            vec![Expr::Var(slot), Expr::Var(anchor), Expr::Var(shadow)],
        )?;
        self.emit_ignored(track, self.line(span))?;
        self.live_locals.retain(|local| *local != slot);
        Ok(())
    }

    fn fresh_shadow(&mut self, span: ast::Span, slot: LocalSlot) -> Result<LocalSlot, ParseError> {
        let fresh = self.call(NEW_TABLE, vec![])?;
        if let Some(shadow) = self.shadows.get(&slot).copied() {
            self.stmts.push(Stmt::Assign {
                kind: AssignmentKind::Set,
                index: shadow,
                expr: fresh,
                line: self.line(span),
            });
            return Ok(shadow);
        }
        let shadow = self
            .builder
            .alloc_local_named(&format!("#shadow:{slot:?}"))?;
        self.stmts.push(Stmt::Let {
            index: shadow,
            declared_schema: None,
            expr: fresh,
            line: self.line(span),
        });
        self.shadows.insert(slot, shadow);
        self.live_locals.push(shadow);
        Ok(shadow)
    }

    fn closure_body(
        &mut self,
        func: &ast::FuncBody,
        binding: usize,
        captured: LocalSlot,
    ) -> Result<Expr, ParseError> {
        match func.body.stmts.as_slice() {
            [ast::Stmt::Return { values, .. }]
                if values.exprs.len() == 1 && self.use_is_binding(&values.exprs[0], binding) =>
            {
                self.call(READ_CELL, vec![Expr::Var(captured)])
            }
            [
                ast::Stmt::Assign {
                    targets,
                    values,
                    span,
                },
            ] if targets.len() == 1
                && values.exprs.len() == 1
                && self.target_is_binding(&targets[0], binding) =>
            {
                let span = *span;
                if let Some(literal) = self.proven_scalar_literal(&values.exprs[0])? {
                    return self
                        .call(WRITE_CELL, vec![Expr::Var(captured), literal])
                        .map_err(|_| self.unsupported(span));
                }
                if matches!(values.exprs[0], ast::Expr::Nil { .. }) {
                    return self
                        .call(WRITE_CELL, vec![Expr::Var(captured), Expr::Null])
                        .map_err(|_| self.unsupported(span));
                }
                if matches!(
                    values.exprs[0],
                    ast::Expr::Table { ref fields, .. } if fields.is_empty()
                ) {
                    let next = self.call(NEW_TABLE, vec![])?;
                    return self
                        .call(WRITE_CELL, vec![Expr::Var(captured), next])
                        .map_err(|_| self.unsupported(span));
                }
                Err(self.unsupported(span))
            }
            [other, ..] => Err(self.unsupported(stmt_span(other))),
            [] => Err(self.unsupported(func.span)),
        }
    }

    fn use_is_binding(&self, expr: &ast::Expr, binding: usize) -> bool {
        let ast::Expr::Name { name, .. } = expr else {
            return false;
        };
        self.resolution
            .uses
            .iter()
            .any(|use_site| use_site.span == name.span && use_site.binding == Some(binding))
    }

    fn target_is_binding(&self, target: &ast::AssignTarget, binding: usize) -> bool {
        let ast::AssignTarget::Name(name) = target else {
            return false;
        };
        self.resolution
            .uses
            .iter()
            .any(|use_site| use_site.span == name.span && use_site.binding == Some(binding))
    }

    fn anchor_of_name(&self, name: &ast::Name) -> Option<LocalSlot> {
        let binding = self
            .resolution
            .uses
            .iter()
            .find_map(|use_site| (use_site.span == name.span).then_some(use_site.binding)?)?;
        self.capture_envelopes.get(&binding).copied()
    }

    fn proven_scalar_literal(&self, expr: &ast::Expr) -> Result<Option<Expr>, ParseError> {
        match expr {
            ast::Expr::Bool { value, .. } => Ok(Some(Expr::Bool(*value))),
            ast::Expr::Number { raw, kind, span } => {
                Ok(Some(self.proven_number(raw, *kind, *span)?))
            }
            ast::Expr::Unary {
                op: ast::UnOp::Neg,
                expr,
                span,
            } => Ok(Some(self.neg_zero(expr, *span)?)),
            ast::Expr::String { bytes, .. } => Ok(Some(self.string_literal(bytes))),
            _ => Ok(None),
        }
    }

    /// Decimal integer or float only. Hex, binary, and 64-bit suffixes stay
    /// outside this slice, so a raw token is not reinterpreted.
    fn proven_number(
        &self,
        raw: &str,
        kind: ast::NumberKind,
        span: ast::Span,
    ) -> Result<Expr, ParseError> {
        match kind {
            ast::NumberKind::Integer | ast::NumberKind::Float => raw
                .parse::<f64>()
                .map(Expr::Float)
                .map_err(|_| self.unsupported(span)),
            ast::NumberKind::HexInteger
            | ast::NumberKind::HexFloat
            | ast::NumberKind::Binary
            | ast::NumberKind::Signed64
            | ast::NumberKind::Unsigned64
            | ast::NumberKind::Imaginary => Err(self.unsupported(span)),
        }
    }

    /// Only unary minus of a positive zero. The result keeps the negative-zero
    /// bits. Any other negation is not this slice.
    fn neg_zero(&self, expr: &ast::Expr, span: ast::Span) -> Result<Expr, ParseError> {
        let ast::Expr::Number { raw, kind, .. } = expr else {
            return Err(self.unsupported(span));
        };
        let value = match self.proven_number(raw, *kind, span)? {
            Expr::Float(value) => value,
            _ => return Err(self.unsupported(span)),
        };
        if value != 0.0 || value.is_sign_negative() {
            return Err(self.unsupported(span));
        }
        Ok(Expr::Float(-0.0))
    }

    /// UTF-8, including an interior NUL, stays a string. Any other byte stays
    /// bytes. The two carriers are not converted.
    fn string_literal(&self, bytes: &[u8]) -> Expr {
        match std::str::from_utf8(bytes) {
            Ok(text) => Expr::String(text.to_string()),
            Err(_) => Expr::Bytes(bytes.to_vec()),
        }
    }

    /// Save each local-table receiver, evaluate every right-hand value into a
    /// private slot, then write those slots from right to left through the
    /// same named-field edge. One target stays on `lower_assign`. The slots
    /// are not guest bindings.
    ///
    /// A parenthesized no-argument static Lua function is accepted in any
    /// value, including more than once. Receivers are copied first, including
    /// a captured table. One pending frame opens before the first such call.
    /// Its width is the receivers plus every value index before the last
    /// call. Already-evaluated values, and each later value that still has a
    /// call after it, are written into that frame before the next call. The
    /// last call's result is not rooted. Scalars and nil are not roots.
    /// Right-to-left writes then leave the frame once.
    fn lower_parallel_named_fields(
        &mut self,
        span: ast::Span,
        targets: &[ast::AssignTarget],
        values: &[ast::Expr],
    ) -> Result<(), ParseError> {
        if values
            .iter()
            .any(|value| self.parenthesized_static_call(value))
        {
            return self.lower_parallel_named_call(span, targets, values);
        }
        let temp_base = self.active_temps.len();
        let mut receivers = Vec::with_capacity(targets.len());
        for target in targets {
            let ast::AssignTarget::Field {
                table,
                name,
                nav: false,
                ..
            } = target
            else {
                return Err(self.statement_unsupported(span));
            };
            let key = utf8_field(&name.bytes).map_err(|_| self.unsupported(target.span()))?;
            let from = self.expr(table)?;
            let from = self.index_operand_slot(from, target.span())?;
            receivers.push((from, key));
        }
        let mut saved = Vec::with_capacity(values.len());
        for (index, value) in values.iter().enumerate() {
            if !parallel_rhs_allowed(value) {
                return Err(self.unsupported(value.span()));
            }
            let expr = match value {
                ast::Expr::Nil { .. } => Expr::Null,
                other => self.expr(other)?,
            };
            let slot = self.builder.alloc_local_named(&format!(
                "#parallel-rhs:{}:{}",
                self.stmts.len(),
                index
            ))?;
            self.stmts.push(Stmt::Let {
                index: slot,
                declared_schema: None,
                expr,
                line: self.line(value.span()),
            });
            saved.push(slot);
        }
        self.active_temps.truncate(temp_base);
        for (slot, (from, key)) in saved.iter().rev().zip(receivers.iter().rev()) {
            let written = self.call(
                SET_EDGE,
                vec![
                    Expr::Var(*from),
                    Expr::String(key.clone()),
                    Expr::Var(*slot),
                ],
            )?;
            self.emit_ignored(written, self.line(span))?;
        }
        Ok(())
    }

    fn parenthesized_static_call(&self, expr: &ast::Expr) -> bool {
        matches!(expr, ast::Expr::Paren { .. })
            && self.direct_function_call(skip_parens(expr)).is_some()
    }

    /// Receivers are copied before any right-hand value. One pending frame
    /// stays open from the first parenthesized static call through the
    /// reverse writes, then leaves once.
    fn lower_parallel_named_call(
        &mut self,
        span: ast::Span,
        targets: &[ast::AssignTarget],
        values: &[ast::Expr],
    ) -> Result<(), ParseError> {
        let mut call_indexes = Vec::new();
        for (index, value) in values.iter().enumerate() {
            if self.parenthesized_static_call(value) {
                call_indexes.push(index);
            }
        }
        let Some(&last_call_index) = call_indexes.last() else {
            return Err(self.statement_unsupported(span));
        };
        let first_call_index = call_indexes[0];
        if self.in_function {
            return Err(self.unsupported(values[first_call_index].span()));
        }
        let temp_base = self.active_temps.len();
        let mut receivers = Vec::with_capacity(targets.len());
        for target in targets {
            let ast::AssignTarget::Field {
                table,
                name,
                nav: false,
                ..
            } = target
            else {
                return Err(self.statement_unsupported(span));
            };
            let key = utf8_field(&name.bytes).map_err(|_| self.unsupported(target.span()))?;
            let from = self.expr(table)?;
            let from = self.snapshot_private(from, target.span(), "parallel-recv")?;
            receivers.push((from, key));
        }
        let mut saved = Vec::with_capacity(values.len());
        for value in values.iter().take(first_call_index) {
            if !parallel_rhs_allowed(value) {
                return Err(self.unsupported(value.span()));
            }
            saved.push(self.save_parallel_value(value)?);
        }
        self.active_temps.truncate(temp_base);
        let mut rooted = Vec::with_capacity(receivers.len() + saved.len());
        for (slot, _) in &receivers {
            rooted.push(*slot);
        }
        rooted.extend(saved.iter().copied());
        let width = receivers.len() + last_call_index;
        let frame = self.open_optional_pending(&rooted, width, span)?;
        for (index, value) in values.iter().enumerate().skip(first_call_index) {
            let slot = if self.parenthesized_static_call(value) {
                let call = self.lower_direct_value(skip_parens(value))?;
                match call {
                    Expr::Var(slot) => slot,
                    other => self.snapshot_private(other, value.span(), "parallel-rhs")?,
                }
            } else if parallel_rhs_allowed(value) {
                self.save_parallel_value(value)?
            } else {
                return Err(self.unsupported(value.span()));
            };
            if index < last_call_index {
                self.write_optional_pending(frame, receivers.len() + index, slot, value.span())?;
            }
            saved.push(slot);
        }
        if saved.len() != receivers.len() {
            return Err(self.statement_unsupported(span));
        }
        for (slot, (from, key)) in saved.iter().rev().zip(receivers.iter().rev()) {
            let written = self.call(
                SET_EDGE,
                vec![
                    Expr::Var(*from),
                    Expr::String(key.clone()),
                    Expr::Var(*slot),
                ],
            )?;
            self.emit_ignored(written, self.line(span))?;
        }
        let leave = self.call(LEAVE_PENDING, vec![Expr::Var(frame)])?;
        self.emit_ignored(leave, self.line(span))?;
        Ok(())
    }

    fn save_parallel_value(&mut self, value: &ast::Expr) -> Result<LocalSlot, ParseError> {
        let expr = if matches!(value, ast::Expr::Nil { .. }) {
            Expr::Null
        } else {
            self.expr(value)?
        };
        self.snapshot_private(expr, value.span(), "parallel-rhs")
    }

    fn write_optional_pending(
        &mut self,
        frame: LocalSlot,
        index: usize,
        source: LocalSlot,
        span: ast::Span,
    ) -> Result<(), ParseError> {
        let written = self.call(
            WRITE_OPTIONAL_PENDING,
            vec![
                Expr::Var(frame),
                Expr::Float(index as f64),
                Expr::Var(source),
            ],
        )?;
        self.emit_ignored(written, self.line(span))
    }

    fn snapshot_private(
        &mut self,
        expr: Expr,
        span: ast::Span,
        label: &str,
    ) -> Result<LocalSlot, ParseError> {
        let slot = self
            .builder
            .alloc_local_named(&format!("#{label}:{}", self.stmts.len()))?;
        self.stmts.push(Stmt::Let {
            index: slot,
            declared_schema: None,
            expr,
            line: self.line(span),
        });
        Ok(slot)
    }

    fn open_optional_pending(
        &mut self,
        slots: &[LocalSlot],
        width: usize,
        span: ast::Span,
    ) -> Result<LocalSlot, ParseError> {
        if slots.is_empty() || width < slots.len() {
            return Err(self.statement_unsupported(span));
        }
        let frame = self
            .builder
            .alloc_local_named(&format!("#optional-pending:{}", self.stmts.len()))?;
        let entered = self.call(ENTER_PENDING, vec![Expr::Float(width as f64)])?;
        self.stmts.push(Stmt::Let {
            index: frame,
            declared_schema: None,
            expr: entered,
            line: self.line(span),
        });
        for (slot, source) in slots.iter().enumerate() {
            self.write_optional_pending(frame, slot, *source, span)?;
        }
        Ok(frame)
    }

    fn lower_assign(
        &mut self,
        span: ast::Span,
        target: &ast::AssignTarget,
        value: &ast::Expr,
    ) -> Result<(), ParseError> {
        if let ast::AssignTarget::Name(name) = target
            && let Some(literal) = self.proven_scalar_literal(value)?
        {
            let anchor = self
                .anchor_of_name(name)
                .ok_or_else(|| self.unsupported(span))?;
            let written = self.call(WRITE_CELL, vec![Expr::Var(anchor), literal])?;
            self.emit_ignored(written, self.line(span))?;
            if let Some(slot) = self.locals.get(&name.bytes).copied() {
                self.live_locals.retain(|local| *local != slot);
                self.stmts.push(Stmt::Assign {
                    kind: AssignmentKind::Set,
                    index: slot,
                    expr: Expr::Null,
                    line: self.line(span),
                });
            }
            return Ok(());
        }
        match (target, value) {
            (ast::AssignTarget::Name(name), ast::Expr::Nil { .. }) => {
                let slot = self
                    .locals
                    .get(&name.bytes)
                    .copied()
                    .ok_or_else(|| self.unsupported(span))?;
                if let Some(anchor) = self.anchor_of_name(name) {
                    let cleared = self.call(WRITE_CELL, vec![Expr::Var(anchor), Expr::Null])?;
                    self.emit_ignored(cleared, self.line(span))?;
                }
                if let Some(shadow) = self.shadows.remove(&slot) {
                    self.live_locals.retain(|local| *local != shadow);
                    self.stmts.push(Stmt::Assign {
                        kind: AssignmentKind::Set,
                        index: shadow,
                        expr: Expr::Null,
                        line: self.line(span),
                    });
                }
                self.stmts.push(Stmt::Assign {
                    kind: AssignmentKind::Set,
                    index: slot,
                    expr: Expr::Null,
                    line: self.line(span),
                });
                self.live_locals.retain(|local| *local != slot);
                // The name may now denote a shadow. Release the root of this slot only.
                self.release_receiver_root(span, slot)?;
                Ok(())
            }
            (ast::AssignTarget::Name(name), ast::Expr::Table { fields, .. })
                if fields.is_empty() =>
            {
                let anchor = self
                    .anchor_of_name(name)
                    .ok_or_else(|| self.unsupported(span))?;
                let next = self.call(NEW_TABLE, vec![])?;
                let written = self.call(WRITE_CELL, vec![Expr::Var(anchor), next])?;
                self.emit_ignored(written, self.line(span))?;
                if let Some(slot) = self.locals.get(&name.bytes).copied() {
                    self.live_locals.retain(|local| *local != slot);
                    self.stmts.push(Stmt::Assign {
                        kind: AssignmentKind::Set,
                        index: slot,
                        expr: Expr::Null,
                        line: self.line(span),
                    });
                }
                Ok(())
            }
            (
                ast::AssignTarget::Field {
                    table,
                    name,
                    nav: false,
                    ..
                },
                _,
            ) => {
                let key = utf8_field(&name.bytes).map_err(|_| self.unsupported(span))?;
                let temp_base = self.active_temps.len();
                let from = self.expr(table)?;
                let from = self.index_operand_slot(from, span)?;
                let to = match value {
                    ast::Expr::Nil { .. } => Expr::Null,
                    other => self.expr(other)?,
                };
                self.active_temps.truncate(temp_base);
                let expr = self.call(SET_EDGE, vec![Expr::Var(from), Expr::String(key), to])?;
                self.emit_ignored(expr, self.line(span))
            }
            (
                ast::AssignTarget::Index {
                    table,
                    key,
                    nav: false,
                    ..
                },
                _,
            ) => self.lower_index_assign(span, table, key, value),
            (ast::AssignTarget::Name(name), ast::Expr::Function { body, .. }) => {
                self.lower_closure_assign(span, name, body)
            }
            _ => Err(self.unsupported(span)),
        }
    }

    /// Evaluate the receiver and key before the RHS and keep their temporary
    /// slots visible to explicit collection during later operand evaluation.
    fn lower_index_assign(
        &mut self,
        span: ast::Span,
        table: &ast::Expr,
        key: &ast::Expr,
        value: &ast::Expr,
    ) -> Result<(), ParseError> {
        let temp_base = self.active_temps.len();
        let (from, key_slot) = self.index_operands(table, key, span)?;
        let to = match value {
            ast::Expr::Nil { .. } => Expr::Null,
            other => self.expr(other)?,
        };
        self.active_temps.truncate(temp_base);
        let expr = self.call(SET_EDGE, vec![Expr::Var(from), Expr::Var(key_slot), to])?;
        self.emit_ignored(expr, self.line(span))
    }

    /// Same rooted operands and write as index assignment. The call result is
    /// the issued receiver; a statement call discards it through `emit_ignored`.
    fn rawset_expr(
        &mut self,
        table: &ast::Expr,
        key: &ast::Expr,
        value: &ast::Expr,
        span: ast::Span,
    ) -> Result<Expr, ParseError> {
        let temp_base = self.active_temps.len();
        let (from, key_slot) = self.index_operands(table, key, span)?;
        let to = match value {
            ast::Expr::Nil { .. } => Expr::Null,
            other => self.expr(other)?,
        };
        self.active_temps.truncate(temp_base);
        self.call(SET_EDGE, vec![Expr::Var(from), Expr::Var(key_slot), to])
    }

    fn index_read(
        &mut self,
        base: &ast::Expr,
        key: &ast::Expr,
        span: ast::Span,
    ) -> Result<Expr, ParseError> {
        let temp_base = self.active_temps.len();
        let (table, key_slot) = self.index_operands(base, key, span)?;
        self.active_temps.truncate(temp_base);
        self.call(GET_EDGE, vec![Expr::Var(table), Expr::Var(key_slot)])
    }

    fn index_operands(
        &mut self,
        base: &ast::Expr,
        key: &ast::Expr,
        span: ast::Span,
    ) -> Result<(LocalSlot, LocalSlot), ParseError> {
        if !matches!(
            key,
            ast::Expr::Name { .. }
                | ast::Expr::Paren { .. }
                | ast::Expr::String { .. }
                | ast::Expr::Number { .. }
        ) {
            return Err(self.unsupported(key.span()));
        }
        let table = self.expr(base)?;
        let table = self.index_operand_slot(table, span)?;
        let key = self.expr(key)?;
        let key = self.index_operand_slot(key, span)?;
        Ok((table, key))
    }

    fn index_operand_slot(&mut self, expr: Expr, span: ast::Span) -> Result<LocalSlot, ParseError> {
        if let Expr::Var(slot) = expr {
            return Ok(slot);
        }
        let slot = self
            .builder
            .alloc_local_named(&format!("#index:{}", self.stmts.len()))?;
        self.stmts.push(Stmt::Let {
            index: slot,
            declared_schema: None,
            expr,
            line: self.line(span),
        });
        self.active_temps.push(slot);
        Ok(slot)
    }

    fn lower_function(
        &mut self,
        name: &ast::Name,
        func: &ast::FuncBody,
        span: ast::Span,
    ) -> Result<(), ParseError> {
        if !func.params.is_empty() || func.vararg || func.self_param {
            return Err(self.unsupported(span));
        }
        let function_name = std::str::from_utf8(&name.bytes).map_err(|_| self.unsupported(span))?;
        self.builder.declare_function(function_name, Some(0))?;
        let index = self
            .builder
            .function_index(function_name)
            .expect("declared Lua function");
        let (prefix, returns) = split_return(&func.body.stmts).map_err(|span| {
            ParseError::at_line(self.line(span) as usize, "Lua return must end the block")
        })?;
        let width = rooted_table_count(prefix);
        let saved_locals = std::mem::take(&mut self.locals);
        let saved_live = std::mem::take(&mut self.live_locals);
        let saved_temps = std::mem::take(&mut self.active_temps);
        let saved_stmts = std::mem::take(&mut self.stmts);
        let saved_roots = std::mem::take(&mut self.receiver_roots);
        let saved_frame = self.caller_frame.take();
        let saved_next = self.next_receiver_slot;
        self.next_receiver_slot = 0;
        let saved_tables = std::mem::take(&mut self.table_locals);
        let saved_return_names = std::mem::replace(
            &mut self.return_names,
            returns
                .iter()
                .filter_map(|expr| match expr {
                    ast::Expr::Name { name, .. } => Some(name.bytes.clone()),
                    _ => None,
                })
                .collect(),
        );
        let saved_root_slot = self.root_slot;
        let saved_ignored = self.ignored_slot;
        let saved_in_function = self.in_function;
        self.root_slot = 0;
        self.ignored_slot = None;
        self.in_function = true;
        let enter = self.call(ENTER_CALLEE, vec![Expr::Float(width as f64)])?;
        self.emit_ignored(enter, self.line(span))?;
        for stmt in prefix {
            self.lower_stmt(stmt)?;
        }
        if self.root_slot != width {
            return Err(self.unsupported(span));
        }
        let body_expr = self.pack_return(returns)?;
        let body_stmts = std::mem::take(&mut self.stmts);
        let table_positions: Vec<u32> = returns
            .iter()
            .enumerate()
            .filter_map(|(index, expr)| match expr {
                ast::Expr::Name { name, .. } if self.table_locals.contains(&name.bytes) => {
                    Some(index as u32)
                }
                _ => None,
            })
            .collect();
        if table_positions.len() > 1 {
            return Err(self.unsupported(span));
        }
        self.locals = saved_locals;
        self.live_locals = saved_live;
        self.active_temps = saved_temps;
        self.stmts = saved_stmts;
        self.receiver_roots = saved_roots;
        self.caller_frame = saved_frame;
        self.next_receiver_slot = saved_next;
        self.table_locals = saved_tables;
        self.return_names = saved_return_names;
        self.root_slot = saved_root_slot;
        self.ignored_slot = saved_ignored;
        self.in_function = saved_in_function;
        self.function_impls.insert(
            index,
            FunctionImpl {
                param_slots: Vec::new(),
                capture_copies: Vec::new(),
                body_stmts,
                body_expr,
                body_expr_line: self.line(span),
            },
        );
        self.functions.insert(
            name.bytes.clone(),
            FuncInfo {
                index,
                callee_width: width,
                return_count: u32::try_from(returns.len()).expect("return count"),
            },
        );
        Ok(())
    }

    fn pack_return(&mut self, values: &[ast::Expr]) -> Result<Expr, ParseError> {
        let mut packed = Vec::with_capacity(values.len());
        for value in values {
            packed.push(self.return_value(value)?);
        }
        Ok(pack_array(packed))
    }

    fn return_value(&mut self, expr: &ast::Expr) -> Result<Expr, ParseError> {
        match expr {
            ast::Expr::Nil { .. } => Ok(Expr::Null),
            ast::Expr::Number {
                raw,
                kind: ast::NumberKind::Integer | ast::NumberKind::Float,
                ..
            } => raw
                .parse()
                .map(Expr::Float)
                .map_err(|_| self.unsupported(expr.span())),
            ast::Expr::Name { name, .. } => self
                .locals
                .get(&name.bytes)
                .copied()
                .map(Expr::Var)
                .ok_or_else(|| self.unsupported(expr.span())),
            other => Err(self.unsupported(other.span())),
        }
    }

    fn direct_function_call<'a>(&self, expr: &'a ast::Expr) -> Option<&'a ast::Name> {
        match expr {
            ast::Expr::SuffixChain { base, steps, .. }
                if let ast::Expr::Name { name, .. } = base.as_ref()
                    && let [
                        ast::Suffix::Call {
                            args: ast::Args::List(args),
                            nav: false,
                            ..
                        },
                    ] = steps.as_slice()
                    && args.exprs.is_empty()
                    && self.functions.contains_key(&name.bytes) =>
            {
                Some(name)
            }
            _ => None,
        }
    }

    fn lower_call_local(
        &mut self,
        span: ast::Span,
        names: &[ast::LocalName],
        call: &ast::Expr,
    ) -> Result<(), ParseError> {
        if self.in_function {
            return Err(self.unsupported(span));
        }
        let callee = self
            .direct_function_call(call)
            .ok_or_else(|| self.unsupported(span))?;
        let info = self
            .functions
            .get(&callee.bytes)
            .copied()
            .ok_or_else(|| self.unsupported(span))?;
        let line = self.line(span);
        let mut slots = Vec::with_capacity(names.len());
        let mut root_slots = Vec::with_capacity(names.len());
        for (offset, name) in names.iter().enumerate() {
            let index = self
                .builder
                .alloc_local_named(&format!("#{}:{offset}", span.start))?;
            self.remember_binding(span, &name.name, index)?;
            if self.next_receiver_slot >= self.receiver_width {
                return Err(self.unsupported(span));
            }
            let root_slot = self.next_receiver_slot;
            self.next_receiver_slot += 1;
            // Keep the published slot with this binding, not with the spelling.
            self.receiver_roots.insert(index, root_slot);
            slots.push(index);
            root_slots.push(Expr::Float(root_slot as f64));
        }
        // The slot list is a previous statement, so its array calls cannot
        // sit between Core Ret and the publish that follows it.
        let slots_local = self
            .builder
            .alloc_local_named(&format!("#slots:{}", span.start))?;
        self.stmts.push(Stmt::Let {
            index: slots_local,
            declared_schema: None,
            expr: pack_array(root_slots),
            line,
        });
        let transport = self
            .builder
            .alloc_local_named(&format!("#ret:{}", span.start))?;
        let call_expr = Expr::Call(info.index, vec![], vec![], None, None);
        let publish = if self.publish_results { 1.0 } else { 0.0 };
        let frame_expr = self.caller_frame.map(Expr::Var).unwrap_or(Expr::Null);
        // The script call is the transport argument. Core Ret runs inside that
        // call. finish_return then publishes into this chunk's one caller
        // frame, while the callee Lua frame is still current.
        let finish = self.call(
            FINISH_RETURN,
            vec![
                call_expr,
                frame_expr.clone(),
                Expr::Float(publish),
                Expr::Var(slots_local),
                Expr::Float(if info.callee_width > 0 { 1.0 } else { 0.0 }),
            ],
        )?;
        self.stmts.push(Stmt::Let {
            index: transport,
            declared_schema: None,
            expr: finish,
            line,
        });
        let get = vm::BuiltinFunction::Get.call_index();
        for (index, slot) in slots.iter().enumerate() {
            let expr = if (index as u32) < info.return_count {
                Expr::Call(
                    get,
                    vec![],
                    vec![Expr::Var(transport), Expr::Int(index as i64)],
                    None,
                    None,
                )
            } else {
                Expr::Null
            };
            self.stmts.push(Stmt::Let {
                index: *slot,
                declared_schema: None,
                expr,
                line,
            });
        }
        let retire = self.call(
            RETIRE_CALLEE,
            vec![
                Expr::Float(if info.callee_width > 0 { 1.0 } else { 0.0 }),
                frame_expr,
            ],
        )?;
        self.emit_ignored(retire, line)?;
        Ok(())
    }

    /// Value-position direct call. The result is a real Get, not a swallowed
    /// null. No root is published here; a later argument still has to root
    /// an earlier table before this call can collect.
    fn lower_direct_value(&mut self, call: &ast::Expr) -> Result<Expr, ParseError> {
        let span = call.span();
        if self.in_function {
            return Err(self.unsupported(span));
        }
        let callee = self
            .direct_function_call(call)
            .ok_or_else(|| self.unsupported(span))?;
        let info = self
            .functions
            .get(&callee.bytes)
            .copied()
            .ok_or_else(|| self.unsupported(span))?;
        let line = self.line(span);
        let transport = self
            .builder
            .alloc_local_named(&format!("#ret:{}", span.start))?;
        let call_expr = Expr::Call(info.index, vec![], vec![], None, None);
        let frame_expr = self.caller_frame.map(Expr::Var).unwrap_or(Expr::Null);
        let finish = self.call(
            FINISH_RETURN,
            vec![
                call_expr,
                frame_expr.clone(),
                Expr::Float(0.0),
                Expr::Null,
                Expr::Float(if info.callee_width > 0 { 1.0 } else { 0.0 }),
            ],
        )?;
        self.stmts.push(Stmt::Let {
            index: transport,
            declared_schema: None,
            expr: finish,
            line,
        });
        let result = self
            .builder
            .alloc_local_named(&format!("#value:{}", span.start))?;
        let value = if info.return_count > 0 {
            let get = vm::BuiltinFunction::Get.call_index();
            Expr::Call(
                get,
                vec![],
                vec![Expr::Var(transport), Expr::Int(0)],
                None,
                None,
            )
        } else {
            Expr::Null
        };
        self.stmts.push(Stmt::Let {
            index: result,
            declared_schema: None,
            expr: value,
            line,
        });
        let retire = self.call(
            RETIRE_CALLEE,
            vec![
                Expr::Float(if info.callee_width > 0 { 1.0 } else { 0.0 }),
                frame_expr,
            ],
        )?;
        self.emit_ignored(retire, line)?;
        Ok(Expr::Var(result))
    }

    fn field_read(
        &mut self,
        base: &ast::Expr,
        key_name: &[u8],
        span: ast::Span,
    ) -> Result<Expr, ParseError> {
        let table = self.expr(base)?;
        let slot = self
            .builder
            .alloc_local_named(&format!("#field:{}", self.stmts.len()))?;
        self.stmts.push(Stmt::Let {
            index: slot,
            declared_schema: None,
            expr: table,
            line: self.line(span),
        });
        let key = utf8_field(key_name).map_err(|_| self.unsupported(span))?;
        self.call(GET_EDGE, vec![Expr::Var(slot), Expr::String(key)])
    }

    fn emit_ignored(&mut self, expr: Expr, line: u32) -> Result<(), ParseError> {
        if let Some(index) = self.ignored_slot {
            self.stmts.push(Stmt::Assign {
                kind: AssignmentKind::Set,
                index,
                expr,
                line,
            });
            return Ok(());
        }
        let index = self
            .builder
            .alloc_local_named(&format!("#ignored:{}", self.stmts.len()))?;
        self.ignored_slot = Some(index);
        self.stmts.push(Stmt::Let {
            index,
            declared_schema: None,
            expr,
            line,
        });
        Ok(())
    }

    fn expr(&mut self, expr: &ast::Expr) -> Result<Expr, ParseError> {
        match expr {
            ast::Expr::Table { fields, .. } if fields.is_empty() => self.call(NEW_TABLE, vec![]),
            ast::Expr::String { bytes, .. } => String::from_utf8(bytes.clone())
                .map(Expr::String)
                .map_err(|_| self.unsupported(expr.span())),
            ast::Expr::Bool { value, .. } => Ok(Expr::Bool(*value)),
            ast::Expr::Number {
                raw,
                kind: ast::NumberKind::Integer | ast::NumberKind::Float,
                ..
            } => raw
                .parse()
                .map(Expr::Float)
                .map_err(|_| self.unsupported(expr.span())),
            ast::Expr::Name { name, .. } => {
                if let Some(anchor) = self.anchor_of_name(name) {
                    return self.call(READ_CELL, vec![Expr::Var(anchor)]);
                }
                self.locals
                    .get(&name.bytes)
                    .copied()
                    .map(Expr::Var)
                    .ok_or_else(|| self.unsupported(expr.span()))
            }
            ast::Expr::Nil { .. } => Ok(Expr::Null),
            ast::Expr::Paren { inner, .. } => self.expr(inner),
            ast::Expr::BinaryChain { span, base, steps } => {
                let mut left = self.expr(base)?;
                for step in steps {
                    if step.op != ast::BinOp::Add {
                        return Err(self.unsupported(*span));
                    }
                    let temp = self
                        .builder
                        .alloc_local_named(&format!("#step:{}", self.stmts.len()))?;
                    self.stmts.push(Stmt::Let {
                        index: temp,
                        declared_schema: None,
                        expr: left,
                        line: self.line(*span),
                    });
                    self.active_temps.push(temp);
                    let right = self.expr(&step.expr)?;
                    self.active_temps.pop();
                    left = self.call(ADD, vec![Expr::Var(temp), right])?;
                }
                Ok(left)
            }
            ast::Expr::SuffixChain { base, steps, span }
                if let [
                    ast::Suffix::Index {
                        key, nav: false, ..
                    },
                ] = steps.as_slice() =>
            {
                self.index_read(base, key, *span)
            }
            ast::Expr::SuffixChain { base, steps, span }
                if let [
                    ast::Suffix::Field {
                        name, nav: false, ..
                    },
                ] = steps.as_slice() =>
            {
                self.field_read(base, &name.bytes, *span)
            }
            ast::Expr::SuffixChain { base, steps, .. }
                if let ast::Expr::Name { name, .. } = base.as_ref()
                    && let Some(slot) = self.locals.get(&name.bytes).copied()
                    && let [
                        ast::Suffix::Call {
                            args: ast::Args::List(args),
                            nav: false,
                            ..
                        },
                    ] = steps.as_slice()
                    && args.exprs.is_empty() =>
            {
                Ok(Expr::LocalCall(slot, Vec::new(), Vec::new(), None))
            }
            ast::Expr::SuffixChain { base, steps, .. }
                if let ast::Expr::Name { name, .. } = base.as_ref()
                    && !self.locals.contains_key(&name.bytes)
                    && let [
                        ast::Suffix::Call {
                            args: ast::Args::List(args),
                            nav: false,
                            ..
                        },
                    ] = steps.as_slice() =>
            {
                if self.functions.contains_key(&name.bytes) {
                    if !args.exprs.is_empty() {
                        return Err(self.unsupported(expr.span()));
                    }
                    return self.lower_direct_value(expr);
                }
                if name.is("rawlen")
                    && let [arg] = args.exprs.as_slice()
                {
                    if self.name_is_bound(name) {
                        return Err(self.unsupported(expr.span()));
                    }
                    return self.rawlen_expr(arg, expr.span());
                }
                if name.is("type")
                    && let [arg] = args.exprs.as_slice()
                {
                    self.reject_tail_next(arg, expr.span())?;
                    let arg = self.expr(arg)?;
                    return self.call(TYPE, vec![arg]);
                }
                if name.is("collectgarbage")
                    && let [ast::Expr::String { bytes, .. }] = args.exprs.as_slice()
                {
                    if bytes == b"collect" {
                        return self.collect_expr(expr.span());
                    }
                    if bytes == b"count" {
                        return self.count_expr(expr.span());
                    }
                    if bytes == b"step"
                        && !self.name_is_bound(name)
                        && self.catalog.function(STEP).is_some()
                        && self.catalog.function(STEP_HAS_TAIL).is_some()
                    {
                        return self.step_expr(expr.span());
                    }
                    if self.run_control_spelling(bytes) && self.run_control_ready(name) {
                        return self.run_control_expr(bytes, expr.span());
                    }
                }
                // Explicit nil, the parsed positive zero, and the byte string "0"
                // share the omitted step. The number token is +0; source unary +
                // is not supported.
                if name.is("collectgarbage")
                    && !self.name_is_bound(name)
                    && self.catalog.function(STEP).is_some()
                    && self.catalog.function(STEP_HAS_TAIL).is_some()
                    && self.explicit_default_step_data(&args.exprs)
                {
                    return self.step_expr(expr.span());
                }
                if name.is("collectgarbage")
                    && !self.name_is_bound(name)
                    && self.catalog.function(STEP).is_some()
                    && self.catalog.function(STEP_HAS_TAIL).is_some()
                    && self.catalog.function(STEP_DATA).is_some()
                    && let [ast::Expr::String { bytes, .. }, data] = args.exprs.as_slice()
                    && bytes == b"step"
                {
                    return self.step_with_runtime_data(expr.span(), data);
                }
                if name.is("copy_fields")
                    && let [arg] = args.exprs.as_slice()
                {
                    self.reject_tail_next(arg, expr.span())?;
                    let value = self.expr(arg)?;
                    let slot = self
                        .builder
                        .alloc_local_named(&format!("#copy:{}", self.stmts.len()))?;
                    self.stmts.push(Stmt::Let {
                        index: slot,
                        declared_schema: None,
                        expr: value,
                        line: self.line(expr.span()),
                    });
                    return self.call(COPY_FIELDS, vec![Expr::Var(slot)]);
                }
                if name.is("setmetatable")
                    && let [owner, mt] = args.exprs.as_slice()
                {
                    self.reject_tail_next(mt, expr.span())?;
                    let owner = self.expr(owner)?;
                    let mt = match mt {
                        ast::Expr::Nil { .. } => Expr::Null,
                        other => self.expr(other)?,
                    };
                    return self.call(SET_METATABLE, vec![owner, mt]);
                }
                if name.is("getmetatable")
                    && let [owner] = args.exprs.as_slice()
                {
                    self.reject_tail_next(owner, expr.span())?;
                    let owner = self.expr(owner)?;
                    return self.call(GET_METATABLE, vec![owner]);
                }
                if self.builtin_next(name) {
                    return self.next_expr(args, expr.span());
                }
                if name.is("rawget") {
                    // Exact two arguments. Same GET_EDGE path as t[k]. No metamethod.
                    let [table, key] = args.exprs.as_slice() else {
                        return Err(self.unsupported(expr.span()));
                    };
                    self.reject_tail_next(key, expr.span())?;
                    return self.index_read(table, key, expr.span());
                }
                if name.is("rawset") {
                    // A resolver binding is not yet in `locals` for an upvalue
                    // use. Do not treat that name as the builtin.
                    if self.name_is_bound(name) {
                        return Err(self.unsupported(expr.span()));
                    }
                    let [table, key, value] = args.exprs.as_slice() else {
                        return Err(self.unsupported(expr.span()));
                    };
                    self.reject_tail_next(value, expr.span())?;
                    return self.rawset_expr(table, key, value, expr.span());
                }
                if name.is("rawequal") {
                    // Same binding rule as rawset: a shadowed or upvalue name
                    // is not this builtin, and must not fall through to it.
                    if self.name_is_bound(name) {
                        return Err(self.unsupported(expr.span()));
                    }
                    return self.rawequal_expr(&args.exprs, expr.span());
                }
                if name.is("byte_text") && args.exprs.is_empty() {
                    if self.name_is_bound(name) {
                        return Err(self.unsupported(expr.span()));
                    }
                    return self.call(BYTE_TEXT, vec![]);
                }
                if name.is("host_int") && args.exprs.is_empty() {
                    if self.name_is_bound(name) {
                        return Err(self.unsupported(expr.span()));
                    }
                    return self.call(HOST_INT, vec![]);
                }
                if name.is("external_shape") && args.exprs.is_empty() {
                    return self.call(EXTERNAL_SHAPE, vec![]);
                }
                if name.is("direct_auto_restart") && args.exprs.is_empty() {
                    // Test probe for the dispatch gate. Not the automatic route.
                    if self.catalog.function(AUTO_TABLE_RESTART).is_none() {
                        return Err(self.unsupported(expr.span()));
                    }
                    return self.call(AUTO_TABLE_RESTART, vec![Expr::Null, Expr::Float(0.0)]);
                }
                let name =
                    std::str::from_utf8(&name.bytes).map_err(|_| self.unsupported(expr.span()))?;
                self.host_call(&format!("lua_host::{name}"), &args.exprs, expr.span())
            }
            ast::Expr::Unary {
                op: ast::UnOp::Len,
                expr: operand,
                ..
            } => self.length_expr(operand, expr.span()),
            _ => Err(self.unsupported(expr.span())),
        }
    }

    fn param_is_table(param: &HostParamSchema) -> bool {
        matches!(
            (&param.ty, param.passing),
            (HostTypeSchema::Resource(key), vm::HostParamPassing::Borrow)
                if *key == heap::LuaTableResource::key()
        )
    }

    fn host_call(
        &mut self,
        name: &str,
        args: &[ast::Expr],
        span: ast::Span,
    ) -> Result<Expr, ParseError> {
        let Some(function) = self.catalog.function(name) else {
            return Err(self.unsupported(span));
        };
        if function.params.len() != args.len()
            || !matches!(
                function.return_type,
                HostTypeSchema::Bool | HostTypeSchema::Float | HostTypeSchema::String
            )
        {
            return Err(self.unsupported(span));
        }
        if args.last().is_some_and(|arg| self.is_next_call(arg)) {
            return Err(self.unsupported(span));
        }
        let mut checked = Vec::new();
        let temp_base = self.active_temps.len();
        let mut pending_slots = Vec::new();
        let mut pending_frames = Vec::new();
        for (arg, param) in args.iter().zip(&function.params) {
            // Value parentheses still execute this direct call. Recognize
            // them only here; other direct_function_call sites keep spelling.
            if self.direct_function_call(skip_parens(arg)).is_some() && !pending_slots.is_empty() {
                pending_frames.push(self.open_pending_frame(&pending_slots, span)?);
                pending_slots.clear();
            }
            let value = self.expr(arg)?;
            let slot = self
                .builder
                .alloc_local_named(&format!("#arg:{}", self.stmts.len()))?;
            self.stmts.push(Stmt::Let {
                index: slot,
                declared_schema: None,
                expr: value,
                line: self.line(span),
            });
            self.active_temps.push(slot);
            if Self::param_is_table(param) {
                pending_slots.push(slot);
            }
            let forwarded = match (&param.ty, param.passing) {
                (HostTypeSchema::Float, vm::HostParamPassing::Value) => NUMBER_ARG,
                (HostTypeSchema::Bool, vm::HostParamPassing::Value) => BOOL_ARG,
                (HostTypeSchema::String, vm::HostParamPassing::Value) => STRING_ARG,
                (HostTypeSchema::Resource(key), vm::HostParamPassing::Borrow)
                    if *key == heap::LuaTableResource::key() =>
                {
                    TABLE_ARG
                }
                (HostTypeSchema::Unknown, vm::HostParamPassing::Value) => {
                    checked.push(Expr::Var(slot));
                    continue;
                }
                _ => return Err(self.unsupported(span)),
            };
            checked.push(self.call(forwarded, vec![Expr::Var(slot)])?);
        }
        self.active_temps.truncate(temp_base);
        let call_expr = self.call(name, checked)?;
        if pending_frames.is_empty() {
            return Ok(call_expr);
        }
        let result = self
            .builder
            .alloc_local_named(&format!("#host:{}", span.start))?;
        self.stmts.push(Stmt::Let {
            index: result,
            declared_schema: None,
            expr: call_expr,
            line: self.line(span),
        });
        for frame in pending_frames.into_iter().rev() {
            let leave = self.call(LEAVE_PENDING, vec![Expr::Var(frame)])?;
            self.emit_ignored(leave, self.line(span))?;
        }
        Ok(Expr::Var(result))
    }

    fn open_pending_frame(
        &mut self,
        slots: &[vm::LocalSlot],
        span: ast::Span,
    ) -> Result<vm::LocalSlot, ParseError> {
        let line = self.line(span);
        let width = u32::try_from(slots.len()).map_err(|_| self.unsupported(span))?;
        let entered = self.call(ENTER_PENDING, vec![Expr::Float(width as f64)])?;
        let frame = self
            .builder
            .alloc_local_named(&format!("#pending:{}", span.start))?;
        self.stmts.push(Stmt::Let {
            index: frame,
            declared_schema: None,
            expr: entered,
            line,
        });
        for (index, slot) in slots.iter().enumerate() {
            let wrote = self.call(
                WRITE_PENDING,
                vec![
                    Expr::Var(frame),
                    Expr::Float(index as f64),
                    Expr::Var(*slot),
                ],
            )?;
            self.emit_ignored(wrote, line)?;
        }
        Ok(frame)
    }

    /// Evaluate every static argument on the same temporary-root path as an
    /// index operand, then compare only the first two. Extra arguments are
    /// evaluated and ignored. Fewer than two is a refusal, not an implicit nil.
    fn rawequal_expr(&mut self, args: &[ast::Expr], span: ast::Span) -> Result<Expr, ParseError> {
        if args.len() < 2 || args.last().is_some_and(|arg| self.is_next_call(arg)) {
            return Err(self.unsupported(span));
        }
        let temp_base = self.active_temps.len();
        let mut slots = Vec::with_capacity(args.len());
        for arg in args {
            let value = self.equal_operand(arg)?;
            slots.push(self.index_operand_slot(value, span)?);
        }
        self.active_temps.truncate(temp_base);
        self.call(RAW_EQUAL, vec![Expr::Var(slots[0]), Expr::Var(slots[1])])
    }

    /// String literals keep their raw bytes. Other operands use the existing
    /// expression lowerer, including calls that already return one value.
    fn equal_operand(&mut self, expr: &ast::Expr) -> Result<Expr, ParseError> {
        match skip_parens(expr) {
            ast::Expr::String { bytes, .. } => Ok(self.string_literal(bytes)),
            other => self.expr(other),
        }
    }

    fn run_control_spelling(&self, bytes: &[u8]) -> bool {
        matches!(bytes, b"stop" | b"restart" | b"isrunning")
    }

    fn run_control_ready(&self, name: &ast::Name) -> bool {
        !self.name_is_bound(name)
            && self.catalog.function(GC_STOP).is_some()
            && self.catalog.function(GC_RESTART).is_some()
            && self.catalog.function(GC_IS_RUNNING).is_some()
    }

    fn run_control_expr(&mut self, bytes: &[u8], span: ast::Span) -> Result<Expr, ParseError> {
        let name = match bytes {
            b"stop" => GC_STOP,
            b"restart" => GC_RESTART,
            b"isrunning" => GC_IS_RUNNING,
            _ => return Err(self.unsupported(span)),
        };
        self.call(name, vec![])
    }

    fn collect_expr(&mut self, span: ast::Span) -> Result<Expr, ParseError> {
        let roots = self.gather_safepoint_roots(span)?;
        self.call(COLLECT, vec![roots])
    }

    fn explicit_default_step_data(&self, args: &[ast::Expr]) -> bool {
        let [ast::Expr::String { bytes, .. }, data] = args else {
            return false;
        };
        if bytes != b"step" {
            return false;
        }
        match data {
            ast::Expr::Nil { .. } => true,
            ast::Expr::String { bytes, .. } => bytes == b"0",
            ast::Expr::Number { raw, kind, span } => matches!(
                self.proven_number(raw, *kind, *span),
                Ok(Expr::Float(value)) if value.to_bits() == 0
            ),
            _ => false,
        }
    }

    fn step_with_runtime_data(
        &mut self,
        span: ast::Span,
        data: &ast::Expr,
    ) -> Result<Expr, ParseError> {
        // One ordinary evaluation, then the category host, then the reviewed
        // step. A data fault stays; this path does not roll it back.
        let value = self.expr(data)?;
        let slot = self
            .builder
            .alloc_local_named(&format!("#step-data:{}", self.stmts.len()))?;
        self.stmts.push(Stmt::Let {
            index: slot,
            declared_schema: None,
            expr: value,
            line: self.line(span),
        });
        let checked = self.call(STEP_DATA, vec![Expr::Var(slot)])?;
        self.emit_ignored(checked, self.line(span))?;
        self.step_expr(span)
    }

    fn step_expr(&mut self, span: ast::Span) -> Result<Expr, ParseError> {
        // Defer root gathering to the mark branch, including the reads of
        // IR locals. A saved tail passes only the null placeholder to Core.
        let start = self.stmts.len();
        let roots = self.gather_safepoint_roots(span)?;
        let gather = self.stmts.split_off(start);
        Ok(Expr::IfElse {
            condition: Box::new(self.call(STEP_HAS_TAIL, vec![])?),
            then_expr: Box::new(self.call(STEP, vec![Expr::Null])?),
            else_expr: Box::new(Expr::Block {
                stmts: gather,
                expr: Box::new(self.call(STEP, vec![roots])?),
            }),
        })
    }

    fn gather_safepoint_roots(&mut self, span: ast::Span) -> Result<Expr, ParseError> {
        // All supported collects are explicit safepoints. Completed arguments
        // live in compat-emitted temporaries, not in an implicit Core stack scan.
        let live: Vec<_> = self
            .live_locals
            .iter()
            .chain(&self.active_temps)
            .copied()
            .collect();
        let mut roots = Expr::Null;
        for slot in live {
            let packed = self.call(GATHER_ROOT, vec![roots, Expr::Var(slot)])?;
            let next = self
                .builder
                .alloc_local_named(&format!("#roots:{}", self.stmts.len()))?;
            self.stmts.push(Stmt::Let {
                index: next,
                declared_schema: None,
                expr: packed,
                line: self.line(span),
            });
            roots = Expr::MoveVar(next);
        }
        Ok(roots)
    }

    fn count_expr(&mut self, _span: ast::Span) -> Result<Expr, ParseError> {
        self.call(COUNT, vec![])
    }

    fn call(&mut self, name: &str, args: Vec<Expr>) -> Result<Expr, ParseError> {
        let function = self.catalog.function(name).expect("private Lua operation");
        self.builder
            .declare_function(name, Some(function.params.len() as u8))?;
        let index = self
            .builder
            .function_index(name)
            .expect("declared operation");
        let resolution = ResolvedHostCall {
            name: name.to_string(),
            params: function
                .params
                .iter()
                .map(|param| ResolvedHostParam {
                    name: param.name.clone(),
                    schema: schema_type(&param.ty),
                })
                .collect(),
            return_type: schema_type(&function.return_type),
            passing: function.params.iter().map(|param| param.passing).collect(),
            fingerprint: self.catalog.fingerprint(),
            host_params: function
                .params
                .iter()
                .map(|param| param.ty.clone())
                .collect(),
            host_return_type: function.return_type.clone(),
        };
        Ok(Expr::Call(
            index,
            vec![],
            args,
            Some(Box::new(resolution)),
            None,
        ))
    }

    fn line(&self, span: ast::Span) -> u32 {
        1 + self.source.as_bytes()[..span.start as usize]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count() as u32
    }

    fn unsupported(&self, span: ast::Span) -> ParseError {
        ParseError::at_line(self.line(span) as usize, "Lua expression not lowered yet")
    }
}

fn stmt_span(stmt: &ast::Stmt) -> ast::Span {
    match stmt {
        ast::Stmt::Empty { span }
        | ast::Stmt::Do { span, .. }
        | ast::Stmt::While { span, .. }
        | ast::Stmt::Repeat { span, .. }
        | ast::Stmt::If { span, .. }
        | ast::Stmt::NumericFor { span, .. }
        | ast::Stmt::GenericFor { span, .. }
        | ast::Stmt::Local { span, .. }
        | ast::Stmt::LocalFunction { span, .. }
        | ast::Stmt::Function { span, .. }
        | ast::Stmt::Assign { span, .. }
        | ast::Stmt::CompoundAssign { span, .. }
        | ast::Stmt::Call { span, .. }
        | ast::Stmt::Label { span, .. }
        | ast::Stmt::Goto { span, .. }
        | ast::Stmt::Break { span }
        | ast::Stmt::Continue { span }
        | ast::Stmt::Return { span, .. } => *span,
    }
}

fn split_return(stmts: &[ast::Stmt]) -> Result<(&[ast::Stmt], &[ast::Expr]), ast::Span> {
    let Some((last, prefix)) = stmts.split_last() else {
        return Ok((stmts, &[]));
    };
    match last {
        ast::Stmt::Return { values, .. } => Ok((prefix, &values.exprs)),
        other
            if stmts
                .iter()
                .any(|stmt| matches!(stmt, ast::Stmt::Return { .. })) =>
        {
            Err(stmt_span(other))
        }
        _ => Ok((stmts, &[])),
    }
}

fn rooted_table_count(stmts: &[ast::Stmt]) -> u32 {
    stmts
        .iter()
        .filter(|stmt| {
            matches!(
                stmt,
                ast::Stmt::Local { values, .. }
                    if matches!(
                        values.exprs.as_slice(),
                        [ast::Expr::Table { fields, .. }] if fields.is_empty()
                    )
            )
        })
        .count() as u32
}

fn chunk_receiver_width(stmts: &[ast::Stmt]) -> u32 {
    stmts.iter().fold(0u32, |count, stmt| {
        count.saturating_add(statement_receiver_width(stmt))
    })
}

/// Direct multi-return receivers, including those declared in a chunk `do`.
/// A function body is a different frame and is not counted here.
fn statement_receiver_width(stmt: &ast::Stmt) -> u32 {
    match stmt {
        ast::Stmt::Local { names, values, .. } => {
            let [call] = values.exprs.as_slice() else {
                return 0;
            };
            if !syntactic_direct_call(call) {
                return 0;
            }
            u32::try_from(names.len()).unwrap_or(u32::MAX)
        }
        ast::Stmt::Do { body, .. } => chunk_receiver_width(&body.stmts),
        _ => 0,
    }
}

fn utf8_field(name: &[u8]) -> Result<String, ()> {
    String::from_utf8(name.to_vec()).map_err(|_| ())
}

/// A non-optional `name.field` whose receiver is a local name.
fn plain_named_field(target: &ast::AssignTarget) -> bool {
    matches!(
        target,
        ast::AssignTarget::Field {
            table,
            nav: false,
            ..
        } if matches!(table.as_ref(), ast::Expr::Name { .. })
    )
}

/// No call. Bool, string, nil, a local, or one ordinary field read.
fn parallel_rhs_allowed(expr: &ast::Expr) -> bool {
    match expr {
        ast::Expr::Nil { .. } | ast::Expr::Bool { .. } | ast::Expr::String { .. } => true,
        ast::Expr::Name { .. } => true,
        ast::Expr::SuffixChain { base, steps, .. } => {
            matches!(base.as_ref(), ast::Expr::Name { .. })
                && matches!(steps.as_slice(), [ast::Suffix::Field { nav: false, .. }])
        }
        _ => false,
    }
}

fn syntactic_direct_call(expr: &ast::Expr) -> bool {
    matches!(
        expr,
        ast::Expr::SuffixChain { base, steps, .. }
            if matches!(base.as_ref(), ast::Expr::Name { .. })
                && matches!(
                    steps.as_slice(),
                    [ast::Suffix::Call {
                        args: ast::Args::List(args),
                        nav: false,
                        ..
                    }] if args.exprs.is_empty()
                )
    )
}

fn pack_array(values: Vec<Expr>) -> Expr {
    let new_array = vm::BuiltinFunction::ArrayNew.call_index();
    let push = vm::BuiltinFunction::ArrayPush.call_index();
    values.into_iter().fold(
        Expr::Call(new_array, vec![], vec![], None, None),
        |array, value| Expr::Call(push, vec![], vec![array, value], None, None),
    )
}

#[derive(Clone, Debug, Default)]
pub(super) struct VisitSamples {
    pub visits: Vec<u64>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct ReturnWitness {
    pub callee_depth: Option<usize>,
    pub caller_depth: Option<usize>,
    pub handoff_depth: Option<usize>,
    pub returned: Option<heap::TableRef>,
    pub discarded: Option<heap::TableRef>,
    pub returned_carrier: Option<usize>,
    pub field_key: Option<String>,
    pub returned_edge_at_handoff: EdgeView,
    pub discarded_edge_at_handoff: EdgeView,
    pub elements: Vec<ReturnedElement>,
    pub published_root: bool,
    pub callee_left: bool,
    pub top_is_caller_after_handoff: bool,
    /// True when the callee Lua frame was still current after the caller root was written.
    /// Core's `Ret` has already run; this is not the same fact.
    pub lua_frame_alive_at_publish: bool,
    pub collects: Vec<CollectNote>,
    pub release: Option<ReleaseNote>,
    pub handoffs: Vec<Handoff>,
    /// Every table rooted by `root_issued`, in activation order.
    pub issued: Vec<heap::TableRef>,
    /// Whether the latest issued table was still alive at each guest collect.
    pub issued_alive_at_collect: Vec<bool>,
    /// Meter and tail snapshot around a Release caller-root write. Absent when
    /// that path did not run.
    pub release_root: Option<ReleaseRootNote>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ReleaseRootNote {
    pub meter_before: heap::ScanMeter,
    pub meter_after: heap::ScanMeter,
    pub tail_before: heap::SavedTailState,
    pub tail_after: heap::SavedTailState,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum EdgeView {
    #[default]
    Absent,
    Live(heap::TableRef),
    Stale,
}

#[derive(Clone, Debug)]
pub(super) struct Handoff {
    pub elements: Vec<ReturnedElement>,
    pub published_root: bool,
    pub callee_left: bool,
    pub top_is_caller_after_handoff: bool,
    pub lua_frame_alive_at_publish: bool,
    pub handoff_depth: Option<usize>,
    pub callee_depth: Option<usize>,
    pub returned_edge: EdgeView,
    pub discarded_edge: EdgeView,
}

#[derive(Clone, Debug)]
pub(super) enum ReturnedElement {
    Nil,
    Number(f64),
    Table { same_ref: bool, same_carrier: bool },
}

#[derive(Clone, Debug)]
pub(super) struct CollectNote {
    pub live_nodes: u32,
    pub marked_nodes: u32,
    pub live_roots: u32,
    pub swept: u64,
    pub returned_alive: bool,
    pub discarded_alive: bool,
    pub returned_edge: EdgeView,
    /// Metatable references followed from marked nodes. Not a field-edge count.
    pub metatable_edge_checks: u64,
    pub outgoing_gap_checks: u64,
    pub outgoing_edge_checks: u64,
    pub sweep_edge_checks: u64,
}

#[derive(Clone, Debug)]
pub(super) struct ReleaseNote {
    pub returned_alive: bool,
    pub returned_edge: EdgeView,
    pub live_roots: u32,
}

fn schema_type(schema: &HostTypeSchema) -> TypeSchema {
    match schema {
        HostTypeSchema::String => TypeSchema::String,
        HostTypeSchema::Float => TypeSchema::Float,
        HostTypeSchema::Bool => TypeSchema::Bool,
        HostTypeSchema::Resource(key) => TypeSchema::Resource(key.clone()),
        _ => TypeSchema::Unknown,
    }
}
