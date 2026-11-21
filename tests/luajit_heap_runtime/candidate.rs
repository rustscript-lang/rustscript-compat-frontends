//! Executable slice of the AST lowering. The production file entry does not
//! select it until the supported execution and root-transfer gates pass.

use std::collections::{HashMap, HashSet};
use vm::compiler::ir::{FunctionImpl, ResolvedHostCall, ResolvedHostParam};
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
const GATHER_ROOT: &str = "lua_internal::gather_root";
pub(super) const COLLECT: &str = "lua_internal::collect";
const ENTER_RESULT: &str = "lua_internal::enter_result_frame";
const ENTER_CALLEE: &str = "lua_internal::enter_callee_frame";
const ROOT_ISSUED: &str = "lua_internal::root_issued";
const SET_EDGE: &str = "lua_internal::set_edge";
pub(super) const FINISH_RETURN: &str = "lua_internal::finish_return";
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
        (COLLECT, HostTypeSchema::Float),
    ] {
        builder.function(HostFunctionSchema::with_return(
            name,
            vec![HostParamSchema::value("value", HostTypeSchema::Unknown)],
            result,
        ));
    }
    for (name, params) in [
        (ENTER_RESULT, vec![]),
        (ENTER_CALLEE, vec!["width"]),
        (ROOT_ISSUED, vec!["value", "slot", "role"]),
        (SET_EDGE, vec!["from", "key", "to"]),
        (FINISH_RETURN, vec!["values", "publish"]),
        (RELEASE_RESULT, vec![]),
    ] {
        builder.function(HostFunctionSchema::with_return(
            name,
            params
                .into_iter()
                .map(|param| HostParamSchema::value(param, HostTypeSchema::Unknown))
                .collect(),
            HostTypeSchema::Float,
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
        Op::GatherRoot,
        Op::Collect,
        Op::EnterResult,
        Op::EnterCallee,
        Op::RootIssued,
        Op::SetEdge,
        Op::FinishReturn,
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
            Op::GatherRoot => GATHER_ROOT,
            Op::Collect => COLLECT,
            Op::EnterResult => ENTER_RESULT,
            Op::EnterCallee => ENTER_CALLEE,
            Op::RootIssued => ROOT_ISSUED,
            Op::SetEdge => SET_EDGE,
            Op::FinishReturn => FINISH_RETURN,
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
    GatherRoot,
    Collect,
    EnterResult,
    EnterCallee,
    RootIssued,
    SetEdge,
    FinishReturn,
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
                [value @ (Value::Float(_) | Value::String(_) | Value::Bool(_))] => value.clone(),
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
            Self::FinishReturn => finish_return(call)?,
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
) -> Option<heap::TableRef> {
    match (table, key) {
        (Some(table), Some(key)) => heap.edge(table, key).ok().flatten(),
        _ => None,
    }
}

fn enter_result(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let frame = heap::with_heap(&mut call.vm().host_context(), |heap| heap.enter_roots(1))
        .map_err(heap_error)?;
    let depth = call.vm().call_depth();
    with_witness(call.vm(), |witness| {
        witness.caller_frame = Some(frame);
        witness.caller_depth = Some(depth);
    })?;
    Ok(Value::Float(0.0))
}

fn enter_callee(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let width = match call.args() {
        [value] => float_u32(value)?,
        _ => return Err(host_fault("invalid callee frame width")),
    };
    let frame = if width == 0 {
        None
    } else {
        Some(
            heap::with_heap(&mut call.vm().host_context(), |heap| {
                heap.enter_roots(width)
            })
            .map_err(heap_error)?,
        )
    };
    let depth = call.vm().call_depth();
    with_witness(call.vm(), |witness| {
        witness.callee_depth = Some(depth);
        witness.callee_frame = frame;
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
    let frame = with_witness(call.vm(), |witness| {
        let frame = witness.callee_frame?;
        if role == 0 {
            witness.returned = Some(table);
            witness.returned_carrier = carrier_ptr(&value);
        } else if witness.discarded.is_none() {
            witness.discarded = Some(table);
        }
        Some(frame)
    })?
    .ok_or_else(|| host_fault("no callee root frame"))?;
    heap::with_heap(&mut call.vm().host_context(), |heap| {
        heap.write_root(frame, slot, table)
    })
    .map_err(heap_error)?;
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
    let (values, publish) = match call.args() {
        [Value::Array(values), Value::Float(flag)] => (values.clone(), *flag != 0.0),
        _ => return Err(host_fault("Lua return transport is not one array")),
    };
    let depth = call.vm().call_depth();
    let mut elements = Vec::with_capacity(values.len());
    let mut returned_value = None;
    for value in values.iter() {
        elements.push(return_element(call, value, &mut returned_value)?);
    }
    let (returned, discarded, key, caller_frame, callee_frame) =
        with_witness(call.vm(), |witness| {
            (
                witness.returned,
                witness.discarded,
                witness.field_key,
                witness.caller_frame,
                witness.callee_frame,
            )
        })?;
    let (returned_edge, discarded_edge) = heap::with_heap(&mut call.vm().host_context(), |heap| {
        Ok((
            read_edge(heap, returned, key),
            read_edge(heap, discarded, key),
        ))
    })
    .map_err(heap_error)?;
    let mut published = false;
    if publish && let (Some(frame), Some(table)) = (caller_frame, returned_value) {
        heap::with_heap(&mut call.vm().host_context(), |heap| {
            heap.write_root(frame, 0, table)
        })
        .map_err(heap_error)?;
        published = true;
    }
    let mut callee_left = false;
    if let Some(frame) = callee_frame {
        heap::with_heap(&mut call.vm().host_context(), |heap| {
            heap.leave_roots(frame)
        })
        .map_err(heap_error)?;
        callee_left = true;
    }
    let top_is_caller = heap::with_heap(&mut call.vm().host_context(), |heap| {
        Ok(caller_frame.is_some() && heap.current_frame() == caller_frame)
    })
    .map_err(heap_error)?;
    with_witness(call.vm(), |witness| {
        witness.elements = elements;
        witness.published_root = published;
        witness.callee_left = callee_left;
        witness.top_is_caller_after_handoff = top_is_caller;
        witness.handoff_depth = Some(depth);
        witness.returned_edge_at_handoff = returned_edge;
        witness.discarded_edge_at_handoff = discarded_edge;
    })?;
    Ok(Value::Float(values.len() as f64))
}

fn return_element(
    call: &mut OwnedHostCall<'_>,
    value: &Value,
    returned_value: &mut Option<heap::TableRef>,
) -> Result<ReturnedElement, VmError> {
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
            if same_ref {
                *returned_value = Some(table);
            }
            Ok(ReturnedElement::Table {
                same_ref,
                same_carrier,
            })
        }
    }
}

fn release_result(call: &mut OwnedHostCall<'_>) -> Result<Value, VmError> {
    let (returned, key, frame) = with_witness(call.vm(), |witness| {
        (witness.returned, witness.field_key, witness.caller_frame)
    })?;
    let frame = frame.ok_or_else(|| host_fault("no caller result frame"))?;
    let (alive, edge) = heap::with_heap(&mut call.vm().host_context(), |heap| {
        Ok((
            returned.is_some_and(|table| heap.alive(table)),
            read_edge(heap, returned, key),
        ))
    })
    .map_err(heap_error)?;
    heap::with_heap(&mut call.vm().host_context(), |heap| {
        heap.leave_roots(frame)
    })
    .map_err(heap_error)?;
    let live_roots = heap::stats(&mut call.vm().host_context())
        .map_err(heap_error)?
        .live_roots;
    with_witness(call.vm(), |witness| {
        witness.caller_frame = None;
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
    let Some((returned, discarded, key)) = host
        .scope_state::<ReturnWitness>()
        .map(|witness| (witness.returned, witness.discarded, witness.field_key))
    else {
        return Ok(());
    };
    let (returned_alive, discarded_alive, returned_edge) = heap::with_heap(host, |heap| {
        Ok((
            returned.is_some_and(|table| heap.alive(table)),
            discarded.is_some_and(|table| heap.alive(table)),
            read_edge(heap, returned, key),
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
        result_bindings: HashSet::new(),
        field_keys: HashMap::new(),
        publish_results,
        in_function: false,
        return_names: HashSet::new(),
        function_returns_table: false,
        root_slot: 0,
        ignored_slot: None,
    };
    if let Some(error) = resolve::resolve(&chunk).errors.first() {
        return Err(ParseError::at_line(1, format!("{error:?}")));
    }
    for stmt in &chunk.body.stmts {
        lower.lower_stmt(stmt)?;
    }
    let mut ir = lower.builder.finish(lower.stmts);
    ir.function_impls = lower.function_impls;
    Ok(ir)
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
    result_bindings: HashSet<Vec<u8>>,
    field_keys: HashMap<Vec<u8>, u32>,
    publish_results: bool,
    in_function: bool,
    return_names: HashSet<Vec<u8>>,
    function_returns_table: bool,
    root_slot: u32,
    ignored_slot: Option<LocalSlot>,
}

#[derive(Clone, Copy)]
struct FuncInfo {
    index: u16,
    returns_table: bool,
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
        let returns_call = self.direct_function_call(value).is_some();
        let expr = self.expr(value)?;
        let index = self
            .builder
            .alloc_local_named(&format!("#{}", span.start))?;
        self.locals.insert(name.bytes.clone(), index);
        if returns_call {
            self.result_bindings.insert(name.bytes.clone());
        } else {
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
            let role = if self.return_names.contains(&name.bytes) {
                self.function_returns_table = true;
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
            (ast::AssignTarget::Name(name), ast::Expr::Nil { .. })
                if self.result_bindings.contains(&name.bytes) =>
            {
                let expr = self.call(RELEASE_RESULT, vec![])?;
                self.emit_ignored(expr, self.line(span))
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
        let saved_results = std::mem::take(&mut self.result_bindings);
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
        let saved_returns_table = self.function_returns_table;
        let saved_root_slot = self.root_slot;
        let saved_ignored = self.ignored_slot;
        let saved_in_function = self.in_function;
        self.function_returns_table = false;
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
        let returns_table = self.function_returns_table;
        self.locals = saved_locals;
        self.live_locals = saved_live;
        self.active_temps = saved_temps;
        self.stmts = saved_stmts;
        self.result_bindings = saved_results;
        self.return_names = saved_return_names;
        self.function_returns_table = saved_returns_table;
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
                returns_table,
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

    fn finish_call(&mut self, name: &[u8], span: ast::Span) -> Result<Expr, ParseError> {
        let info = self
            .functions
            .get(name)
            .copied()
            .ok_or_else(|| self.unsupported(span))?;
        if info.returns_table {
            let enter = self.call(ENTER_RESULT, vec![])?;
            self.emit_ignored(enter, self.line(span))?;
        }
        let call = Expr::Call(info.index, vec![], vec![], None, None);
        let publish = if self.publish_results && info.returns_table {
            1.0
        } else {
            0.0
        };
        self.call(FINISH_RETURN, vec![call, Expr::Float(publish)])
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
                    return self.finish_call(&name.bytes, expr.span());
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
    pub returned_edge_at_handoff: Option<heap::TableRef>,
    pub discarded_edge_at_handoff: Option<heap::TableRef>,
    pub elements: Vec<ReturnedElement>,
    pub published_root: bool,
    pub callee_left: bool,
    pub top_is_caller_after_handoff: bool,
    pub collects: Vec<CollectNote>,
    pub release: Option<ReleaseNote>,
    caller_frame: Option<heap::RootFrameId>,
    callee_frame: Option<heap::RootFrameId>,
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
    pub returned_edge: Option<heap::TableRef>,
}

#[derive(Clone, Debug)]
pub(super) struct ReleaseNote {
    pub returned_alive: bool,
    pub returned_edge: Option<heap::TableRef>,
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
