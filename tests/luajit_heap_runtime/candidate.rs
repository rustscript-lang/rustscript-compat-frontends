//! Executable slice of the AST lowering. The production file entry does not
//! select it until the supported execution and root-transfer gates pass.

use std::collections::{HashMap, HashSet};
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
const RETURN_SCALAR: &str = "lua_internal::return_scalar";

const TABLE_ARG: &str = "lua_internal::table_arg";
const NUMBER_ARG: &str = "lua_internal::number_arg";
const BOOL_ARG: &str = "lua_internal::bool_arg";
const STRING_ARG: &str = "lua_internal::string_arg";
const GATHER_ROOT: &str = "lua_internal::gather_root";
pub(super) const COLLECT: &str = "lua_internal::collect";
const ENTER_RESULT: &str = "lua_internal::enter_result_frame";
const ENTER_CALLEE: &str = "lua_internal::enter_callee_frame";
const ROOT_ISSUED: &str = "lua_internal::root_issued";
const SET_EDGE: &str = "lua_internal::set_edge";
const GET_EDGE: &str = "lua_internal::get_edge";
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
        (
            ROOT_ISSUED,
            vec!["value", "slot", "role"],
            HostTypeSchema::Float,
        ),
        (SET_EDGE, vec!["from", "key", "to"], HostTypeSchema::Float),
        (GET_EDGE, vec!["table", "key"], HostTypeSchema::Unknown),
        (
            FINISH_RETURN,
            vec!["values", "frame", "publish", "slots", "callee"],
            HostTypeSchema::Unknown,
        ),
        (RETIRE_CALLEE, vec!["flag", "frame"], HostTypeSchema::Float),
        (RETIRE_CHUNK, vec!["frame"], HostTypeSchema::Float),
        (RELEASE_RESULT, vec!["frame", "slot"], HostTypeSchema::Float),
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
    for op in [
        Op::NewTable,
        Op::Type,
        Op::Add,
        Op::ReturnScalar,
        Op::TableArg,
        Op::NumberArg,
        Op::BoolArg,
        Op::StringArg,
        Op::GatherRoot,
        Op::Collect,
        Op::EnterResult,
        Op::EnterCallee,
        Op::RootIssued,
        Op::SetEdge,
        Op::GetEdge,
        Op::FinishReturn,
        Op::RetireCallee,
        Op::RetireChunk,
        Op::ReleaseResult,
        Op::CopyFields,
        Op::ExternalShape,
    ] {
        let name = match op {
            Op::NewTable => NEW_TABLE,
            Op::Type => TYPE,
            Op::Add => ADD,
            Op::ReturnScalar => RETURN_SCALAR,
            Op::TableArg => TABLE_ARG,
            Op::NumberArg => NUMBER_ARG,
            Op::BoolArg => BOOL_ARG,
            Op::StringArg => STRING_ARG,
            Op::GatherRoot => GATHER_ROOT,
            Op::Collect => COLLECT,
            Op::EnterResult => ENTER_RESULT,
            Op::EnterCallee => ENTER_CALLEE,
            Op::RootIssued => ROOT_ISSUED,
            Op::SetEdge => SET_EDGE,
            Op::GetEdge => GET_EDGE,
            Op::FinishReturn => FINISH_RETURN,
            Op::RetireCallee => RETIRE_CALLEE,
            Op::RetireChunk => RETIRE_CHUNK,
            Op::ReleaseResult => RELEASE_RESULT,
            Op::CopyFields => COPY_FIELDS,
            Op::ExternalShape => EXTERNAL_SHAPE,
        };
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
    ReturnScalar,
    TableArg,
    NumberArg,
    BoolArg,
    StringArg,
    GatherRoot,
    Collect,
    EnterResult,
    EnterCallee,
    RootIssued,
    SetEdge,
    GetEdge,
    FinishReturn,
    RetireCallee,
    RetireChunk,
    ReleaseResult,
    CopyFields,
    ExternalShape,
}

impl HostOwnedFunction for Op {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        let value = match self {
            Self::NewTable => heap::publish_guest(&mut call.vm().host_context())
                .map_err(|error| VmError::HostError(error.to_string()))?,
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
                let included = match classify(&mut call.vm().host_context(), &value)? {
                    heap::GuestClass::Table(_) => Some(value),
                    heap::GuestClass::Number
                    | heap::GuestClass::Boolean
                    | heap::GuestClass::String => None,
                    heap::GuestClass::Unissued => {
                        return Err(VmError::HostError(
                            "invalid private Lua root candidate".to_string(),
                        ));
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
                let roots = match call.args() {
                    [Value::Null] => Vec::new(),
                    [Value::Array(roots)] => roots.iter().cloned().collect(),
                    _ => {
                        return Err(VmError::HostError(
                            "invalid private Lua root list".to_string(),
                        ));
                    }
                };
                let mut tables = Vec::with_capacity(roots.len());
                for root in &roots {
                    match classify(&mut call.vm().host_context(), root)? {
                        heap::GuestClass::Table(table) => tables.push(table),
                        _ => {
                            return Err(VmError::HostError(
                                "invalid private Lua table root".to_string(),
                            ));
                        }
                    }
                }
                collect_at_safepoint(&mut call.vm().host_context(), &tables)?;
                Value::Float(0.0)
            }
            Self::EnterResult => enter_result(call)?,
            Self::EnterCallee => enter_callee(call)?,
            Self::RootIssued => root_issued(call)?,
            Self::SetEdge => set_edge(call)?,
            Self::GetEdge => get_edge(call)?,
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
        };
        Ok(CallOutcome::Return(CallReturn::one(value)))
    }
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
    key: Option<u32>,
) -> Result<EdgeView, heap::HeapError> {
    let (Some(table), Some(key)) = (table, key) else {
        return Ok(EdgeView::Absent);
    };
    match heap.edge(table, key) {
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
    heap::with_heap(&mut call.vm().host_context(), |heap| {
        heap.write_root(frame, slot, table)
    })
    .map_err(heap_error)?;
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

fn set_edge(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let (from, key, to) = match call.args() {
        [from, key, to] => (from.clone(), float_u32(key)?, to.clone()),
        _ => return Err(host_fault("invalid table edge")),
    };
    let from = match classify(&mut call.vm().host_context(), &from)? {
        heap::GuestClass::Table(table) => table,
        _ => return Err(host_fault("edge source is not a table")),
    };
    let to = match classify(&mut call.vm().host_context(), &to)? {
        heap::GuestClass::Table(table) => table,
        _ => return Err(host_fault("edge target is not a table")),
    };
    heap::with_heap(&mut call.vm().host_context(), |heap| {
        heap.set_edge(from, key, to)
    })
    .map_err(heap_error)?;
    with_witness(call.vm(), |witness| {
        if witness.field_key.is_none() {
            witness.field_key = Some(key);
        }
    })?;
    Ok(Value::Float(0.0))
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
    // Classify each consumed position. Nil, scalars, and the transport array
    // do not become roots. The callee source name is not consulted.
    let wrote = publish_consumed_tables(call, &values, &frame_token, publish, &root_slots)?;
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
            witness.field_key,
            witness.callee_depth,
        )
    })?;
    let (returned_edge, discarded_edge) = heap::with_heap(&mut call.vm().host_context(), |heap| {
        Ok((
            read_edge(heap, returned, key)?,
            read_edge(heap, discarded, key)?,
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
        heap::with_heap(&mut call.vm().host_context(), |heap| {
            heap.leave_roots(callee)
        })
        .map_err(heap_error)?;
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
    let (from, key) = match call.args() {
        [from, key] => (from.clone(), float_u32(key)?),
        _ => return Err(host_fault("invalid field read")),
    };
    let table = match classify(&mut call.vm().host_context(), &from)? {
        heap::GuestClass::Table(table) => table,
        _ => return Err(host_fault("field source is not a table")),
    };
    let target = heap::with_heap(&mut call.vm().host_context(), |heap| heap.edge(table, key))
        .map_err(heap_error)?;
    let Some(target) = target else {
        return Ok(Value::Null);
    };
    heap::with_heap(&mut call.vm().host_context(), |heap| {
        heap.issued_value(target)
    })
    .map_err(heap_error)
}

fn release_result(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let (frame_token, slot) = match call.args() {
        [frame, slot] => (frame.clone(), float_u32(slot)?),
        _ => return Err(host_fault("invalid result release")),
    };
    let frame = decode_frame(call, &frame_token)?;
    let (returned, key) = with_witness(call.vm(), |witness| (witness.returned, witness.field_key))?;
    let (alive, edge) = heap::with_heap(&mut call.vm().host_context(), |heap| {
        Ok((
            returned.is_some_and(|table| heap.alive(table)),
            read_edge(heap, returned, key)?,
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
                witness.field_key,
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
                read_edge(heap, returned, key)?,
                latest.is_some_and(|table| heap.alive(table)),
            ))
        })
        .map_err(heap_error)?;
    let swept = heap::take_scan_meter(host)
        .map_err(heap_error)?
        .scratch
        .dead_slots_pushed;
    let witness = host
        .scope_state_mut::<ReturnWitness>()
        .ok_or_else(|| host_fault("return witness disappeared"))?;
    witness.collects.push(CollectNote {
        live_nodes: report.live_nodes,
        marked_nodes: report.marked_nodes,
        live_roots: report.live_roots,
        swept,
        returned_alive,
        discarded_alive,
        returned_edge,
    });
    witness.issued_alive_at_collect.push(latest_alive);
    Ok(())
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
        field_keys: HashMap::new(),
        publish_results,
        in_function: false,
        return_names: HashSet::new(),
        root_slot: 0,
        ignored_slot: None,
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
    if let Some(error) = resolve::resolve(&chunk).errors.first() {
        return Err(ParseError::at_line(1, format!("{error:?}")));
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
    field_keys: HashMap<Vec<u8>, u32>,
    publish_results: bool,
    in_function: bool,
    return_names: HashSet<Vec<u8>>,
    root_slot: u32,
    ignored_slot: Option<LocalSlot>,
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
            } if let ([name], [value]) = (names.as_slice(), values.exprs.as_slice()) => {
                self.lower_local(*span, &name.name, value)
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
                let value = self.expr(&values.exprs[0])?;
                let expr = self.call(RETURN_SCALAR, vec![value])?;
                self.stmts.push(Stmt::Expr {
                    expr,
                    line: self.line(*span),
                });
                Ok(())
            }
            ast::Stmt::Empty { .. } => Ok(()),
            other => Err(ParseError::at_line(
                self.line(stmt_span(other)) as usize,
                "Lua statement not lowered yet",
            )),
        }
    }

    fn lower_local(
        &mut self,
        span: ast::Span,
        name: &ast::Name,
        value: &ast::Expr,
    ) -> Result<(), ParseError> {
        let expr = self.expr(value)?;
        let index = self
            .builder
            .alloc_local_named(&format!("#{}", span.start))?;
        self.locals.insert(name.bytes.clone(), index);
        self.live_locals.push(index);
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
        Ok(())
    }

    fn lower_assign(
        &mut self,
        span: ast::Span,
        target: &ast::AssignTarget,
        value: &ast::Expr,
    ) -> Result<(), ParseError> {
        match (target, value) {
            (ast::AssignTarget::Name(name), ast::Expr::Nil { .. }) => {
                let slot = self
                    .locals
                    .get(&name.bytes)
                    .copied()
                    .ok_or_else(|| self.unsupported(span))?;
                self.stmts.push(Stmt::Assign {
                    kind: AssignmentKind::Set,
                    index: slot,
                    expr: Expr::Null,
                    line: self.line(span),
                });
                self.live_locals.retain(|local| *local != slot);
                // The name may now denote a shadow. Release the root of this slot only.
                if let Some(root_slot) = self.receiver_roots.remove(&slot) {
                    let frame = self.caller_frame.ok_or_else(|| self.unsupported(span))?;
                    let release = self.call(
                        RELEASE_RESULT,
                        vec![Expr::Var(frame), Expr::Float(root_slot as f64)],
                    )?;
                    self.emit_ignored(release, self.line(span))?;
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
            ) if self.in_function => {
                let key = self.field_key(&name.bytes);
                let from = self.expr(table)?;
                let to = self.expr(value)?;
                let expr = self.call(SET_EDGE, vec![from, Expr::Float(key as f64), to])?;
                self.emit_ignored(expr, self.line(span))
            }
            _ => Err(self.unsupported(span)),
        }
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
            self.locals.insert(name.name.bytes.clone(), index);
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
        let key = self.field_key(key_name);
        self.call(GET_EDGE, vec![Expr::Var(slot), Expr::Float(key as f64)])
    }

    fn field_key(&mut self, name: &[u8]) -> u32 {
        if let Some(key) = self.field_keys.get(name).copied() {
            return key;
        }
        let key = u32::try_from(self.field_keys.len() + 1).expect("field key");
        self.field_keys.insert(name.to_vec(), key);
        key
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
            ast::Expr::SuffixChain { steps, span, .. }
                if steps
                    .iter()
                    .any(|step| matches!(step, ast::Suffix::Index { .. })) =>
            {
                Err(ParseError::at_line(
                    self.line(*span) as usize,
                    "numeric table keys are not in this field-key subset",
                ))
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
                    self.lower_call_local(expr.span(), &[], expr)?;
                    return Ok(Expr::Null);
                }
                if name.is("type")
                    && let [arg] = args.exprs.as_slice()
                {
                    let arg = self.expr(arg)?;
                    return self.call(TYPE, vec![arg]);
                }
                if name.is("collectgarbage")
                    && let [ast::Expr::String { bytes, .. }] = args.exprs.as_slice()
                    && bytes == b"collect"
                {
                    return self.collect_expr(expr.span());
                }
                if name.is("copy_fields")
                    && let [arg] = args.exprs.as_slice()
                {
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
                if name.is("external_shape") && args.exprs.is_empty() {
                    return self.call(EXTERNAL_SHAPE, vec![]);
                }
                let name =
                    std::str::from_utf8(&name.bytes).map_err(|_| self.unsupported(expr.span()))?;
                self.host_call(&format!("lua_host::{name}"), &args.exprs, expr.span())
            }
            _ => Err(self.unsupported(expr.span())),
        }
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
        let mut checked = Vec::new();
        let temp_base = self.active_temps.len();
        for (arg, param) in args.iter().zip(&function.params) {
            let adapter = match (&param.ty, param.passing) {
                (HostTypeSchema::Float, vm::HostParamPassing::Value) => NUMBER_ARG,
                (HostTypeSchema::Bool, vm::HostParamPassing::Value) => BOOL_ARG,
                (HostTypeSchema::String, vm::HostParamPassing::Value) => STRING_ARG,
                (HostTypeSchema::Resource(key), vm::HostParamPassing::Borrow)
                    if *key == heap::LuaTableResource::key() =>
                {
                    TABLE_ARG
                }
                _ => return Err(self.unsupported(span)),
            };
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
            checked.push(self.call(adapter, vec![Expr::Var(slot)])?);
        }
        self.active_temps.truncate(temp_base);
        self.call(name, checked)
    }

    fn collect_expr(&mut self, span: ast::Span) -> Result<Expr, ParseError> {
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
        self.call(COLLECT, vec![roots])
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
        let ast::Stmt::Local { names, values, .. } = stmt else {
            return count;
        };
        let [call] = values.exprs.as_slice() else {
            return count;
        };
        if !syntactic_direct_call(call) {
            return count;
        }
        count.saturating_add(u32::try_from(names.len()).unwrap_or(u32::MAX))
    })
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
pub(super) struct ReturnWitness {
    pub callee_depth: Option<usize>,
    pub caller_depth: Option<usize>,
    pub handoff_depth: Option<usize>,
    pub returned: Option<heap::TableRef>,
    pub discarded: Option<heap::TableRef>,
    pub returned_carrier: Option<usize>,
    pub field_key: Option<u32>,
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
