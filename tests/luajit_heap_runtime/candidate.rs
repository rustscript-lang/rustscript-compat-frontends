//! Executable slice of the AST lowering. The production file entry does not
//! select it until the supported execution and root-transfer gates pass.

use std::collections::HashMap;
use vm::compiler::ir::{ResolvedHostCall, ResolvedHostParam};
use vm::{
    CallOutcome, CallReturn, Expr, FrontendIr, HostApiBuilder, HostApiCatalog,
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
const COLLECT: &str = "lua_internal::collect";
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
    let collect = heap::collect(host);
    if let Some(frame) = frame {
        heap::with_heap(host, |heap| heap.leave_roots(frame)).map_err(heap_error)?;
    }
    collect.map(|_| ()).map_err(heap_error)
}

fn heap_error(error: heap::HeapError) -> VmError {
    VmError::HostError(error.to_string())
}

fn classify(host: &mut vm::HostContext<'_>, value: &Value) -> Result<heap::GuestClass, VmError> {
    heap::with_heap(host, |heap| Ok(heap.guest_class(value))).map_err(heap_error)
}

pub(super) fn lower(source: &str, catalog: &HostApiCatalog) -> Result<FrontendIr, ParseError> {
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
    };
    if let Some(error) = resolve::resolve(&chunk).errors.first() {
        return Err(ParseError::at_line(1, format!("{error:?}")));
    }
    for stmt in &chunk.body.stmts {
        match stmt {
            ast::Stmt::Local {
                span,
                names,
                values,
            } if let ([name], [value]) = (names.as_slice(), values.exprs.as_slice()) => {
                let expr = lower.expr(value)?;
                let index = lower
                    .builder
                    .alloc_local_named(&format!("#{}", span.start))?;
                lower.locals.insert(name.name.bytes.clone(), index);
                lower.live_locals.push(index);
                lower.stmts.push(Stmt::Let {
                    index,
                    declared_schema: None,
                    expr,
                    line: lower.line(*span),
                });
            }
            ast::Stmt::Return { span, values } if let [value] = values.exprs.as_slice() => {
                let value = lower.expr(value)?;
                let expr = lower.call(RETURN_SCALAR, vec![value])?;
                lower.stmts.push(Stmt::Expr {
                    expr,
                    line: lower.line(*span),
                });
            }
            ast::Stmt::Empty { .. } => {}
            _ => return Err(ParseError::at_line(1, "Lua statement not lowered yet")),
        }
    }
    Ok(lower.builder.finish(lower.stmts))
}

struct Lower<'a> {
    source: &'a str,
    catalog: &'a HostApiCatalog,
    builder: LocalIrBuilder,
    locals: HashMap<Vec<u8>, LocalSlot>,
    live_locals: Vec<LocalSlot>,
    active_temps: Vec<LocalSlot>,
    stmts: Vec<Stmt>,
}

impl Lower<'_> {
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

fn schema_type(schema: &HostTypeSchema) -> TypeSchema {
    match schema {
        HostTypeSchema::String => TypeSchema::String,
        HostTypeSchema::Float => TypeSchema::Float,
        HostTypeSchema::Bool => TypeSchema::Bool,
        HostTypeSchema::Resource(key) => TypeSchema::Resource(key.clone()),
        _ => TypeSchema::Unknown,
    }
}
