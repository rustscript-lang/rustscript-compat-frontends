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

pub(super) fn install_schemas(builder: &mut HostApiBuilder) {
    let key = heap::LuaTableResource::key();
    builder.resource(vm::ResourceTypeSchema::new(key.clone(), "Lua table"));
    builder.function(HostFunctionSchema::with_return(
        NEW_TABLE,
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
    ] {
        let name = match op {
            Op::NewTable => NEW_TABLE,
            Op::Type => TYPE,
            Op::Add => ADD,
            Op::ReturnScalar => RETURN_SCALAR,
            Op::TableArg => TABLE_ARG,
            Op::NumberArg => NUMBER_ARG,
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
}

impl HostOwnedFunction for Op {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        let value = match self {
            Self::NewTable => {
                let (table, _) = heap::publish_table(&mut call.vm().host_context())
                    .map_err(|error| VmError::HostError(error.to_string()))?;
                // This array is a private VM carrier, not a Lua array or a
                // host resource token. Only Lua table construction produces it.
                Value::array(vec![
                    Value::Int(table.domain().cast_signed()),
                    Value::Int(i64::from(table.slot())),
                    Value::Int(i64::from(table.generation())),
                ])
            }
            Self::Type => match call.args() {
                [Value::Float(_)] => Value::string("number"),
                [Value::Array(_)] => Value::string("table"),
                [Value::Bool(_)] => Value::string("boolean"),
                [Value::String(_)] => Value::string("string"),
                _ => return Err(VmError::HostError("invalid private Lua value".to_string())),
            },
            Self::Add => match call.args() {
                [Value::Float(left), Value::Float(right)] => Value::Float(left + right),
                [Value::Array(_), _] | [_, Value::Array(_)] => {
                    return Err(VmError::HostError(
                        "attempt to perform arithmetic on a table value".to_string(),
                    ));
                }
                _ => {
                    return Err(VmError::HostError(
                        "unsupported Lua arithmetic operand".to_string(),
                    ));
                }
            },
            Self::ReturnScalar => match call.args() {
                [value @ (Value::Float(_) | Value::String(_) | Value::Bool(_))] => value.clone(),
                _ => {
                    return Err(VmError::HostError(
                        "Lua result needs a typed adapter".to_string(),
                    ));
                }
            },
            Self::TableArg => {
                let table = match call.args() {
                    [Value::Array(fields)] => decode_table(fields),
                    _ => None,
                }
                .ok_or_else(|| VmError::HostError("expected Lua table".to_string()))?;
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
        };
        Ok(CallOutcome::Return(CallReturn::one(value)))
    }
}

fn decode_table(fields: &[Value]) -> Option<heap::TableRef> {
    let [Value::Int(domain), Value::Int(slot), Value::Int(generation)] = fields else {
        return None;
    };
    Some(heap::TableRef::table(
        domain.cast_unsigned(),
        (*slot).try_into().ok()?,
        (*generation).try_into().ok()?,
    ))
}

pub(super) fn lower(source: &str, catalog: &HostApiCatalog) -> Result<FrontendIr, ParseError> {
    let chunk = parser::parse_chunk(source.as_bytes())
        .map_err(|error| ParseError::at_line(error.line as usize, error.message))?;
    let mut lower = Lower {
        source,
        catalog,
        builder: LocalIrBuilder::new(),
        locals: HashMap::new(),
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
    stmts: Vec<Stmt>,
}

impl Lower<'_> {
    fn expr(&mut self, expr: &ast::Expr) -> Result<Expr, ParseError> {
        match expr {
            ast::Expr::Table { fields, .. } if fields.is_empty() => self.call(NEW_TABLE, vec![]),
            ast::Expr::Number { raw, kind, .. }
                if matches!(kind, ast::NumberKind::Integer | ast::NumberKind::Float) =>
            {
                raw.parse()
                    .map(Expr::Float)
                    .map_err(|_| self.unsupported(expr.span()))
            }
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
                    let right = self.expr(&step.expr)?;
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
            checked.push(self.call(adapter, vec![Expr::Var(slot)])?);
        }
        self.call(name, checked)
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
