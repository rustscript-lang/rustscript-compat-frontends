//! Private lazy numeric generator lowering for the explicit reference-array
//! TypeScript mode.
//!
//! A `function*` constructor keeps its original parameters and default
//! expressions on an ordinary typed callable: defaults still evaluate once,
//! left-to-right, in callee scope at iterator creation through the existing
//! default-thunk machinery. The constructor body only initializes persistent
//! scalar cells and returns a private iterator record holding a real
//! zero-argument `next` closure. User body and local initializers move into
//! that closure as a finite program-counter state machine over the same
//! persistent capture cells, so each `next()` runs source work to the next
//! yield or return. Aliases copy the record (sharing the closure cells);
//! different constructor evaluations build independent cells.
//!
//! Representation uses only existing Core constructs: `TypeSchema::Named`
//! nominals for the private iterator/result records, `Optional` for
//! finite-completion values, `Expr::Closure`/`LocalCall`, `MapNew`/`Set`/`Get`
//! builtins, `OptionalGet` for optional reads and structured `While`/`IfElse`
//! IR. No new Value, schema, opcode, ABI or runtime descriptor is
//! introduced, and ordinary compilation never consults this module.

use std::collections::{HashMap, HashSet};

use swc_common::Spanned;
use swc_ecma_ast as ast;
use vm::compiler::ir::{
    AssignmentKind, FunctionDecl, FunctionDeclSite, FunctionImpl, GuestTypeProvenance,
    LocalDeclSite, ParsedCallSite, ParsedCallTarget, ParsedLexicalScope, StmtSpanSite, StructDecl,
    StructDeclOrigin, StructDeclSite,
};
use vm::{Expr, ParseError, Stmt, TypeSchema};

use super::parser::{
    Lowerer, byte_span, line_at, parameter_binding, schema_value_type, unsupported,
};

/// Hygienic nominal prefix for a generator's private iterator record.
/// Documented as reserved in reference-array mode; a user declaration
/// colliding with one fails precisely as a duplicate.
pub(crate) const ITERATOR_PREFIX: &str = "__CompatGenIterator_";
/// Hygienic nominal prefix for a generator's private next-result record.
pub(crate) const RESULT_PREFIX: &str = "__CompatGenResult_";

/// Iterator record nominal for the generator constructor `func`.
pub(crate) fn generator_iterator_type_name(func: &str) -> String {
    format!("{ITERATOR_PREFIX}{func}")
}

/// Next-result record nominal for the generator constructor `func`.
pub(crate) fn generator_result_type_name(func: &str) -> String {
    format!("{RESULT_PREFIX}{func}")
}

/// Lowered representation of one generator constructor, keyed by its
/// iterator nominal. Only schemas produced by this module (constructor
/// return schemas and `next()` result schemas) ever qualify; user records,
/// callables and host APIs never match by spelling or shape.
#[derive(Clone, Debug)]
pub(crate) struct GeneratorInfo {
    pub(crate) iterator_nominal: String,
    pub(crate) result_nominal: String,
    /// Joined numeric schema of the body's yield values (`Int` when every
    /// yield is an `Int`, otherwise `Number`). Finite results wrap it in
    /// `Optional`; proven-infinite results carry it directly.
    pub(crate) value_schema: TypeSchema,
    /// True when the body provably never completes normally: no `return`
    /// anywhere, no `break` anywhere, and the final top-level statement is
    /// an unconditional loop. Computed structurally over the source body,
    /// never by function name.
    pub(crate) infinite: bool,
}

impl GeneratorInfo {
    pub(crate) fn iterator_schema(&self) -> TypeSchema {
        TypeSchema::Named(self.iterator_nominal.clone(), Vec::new())
    }

    pub(crate) fn result_schema(&self) -> TypeSchema {
        TypeSchema::Named(self.result_nominal.clone(), Vec::new())
    }

    pub(crate) fn result_object_schema(&self) -> TypeSchema {
        let mut fields = HashMap::new();
        let value = if self.infinite {
            self.value_schema.clone()
        } else {
            TypeSchema::Optional(Box::new(self.value_schema.clone()))
        };
        fields.insert("value".to_owned(), value);
        fields.insert("done".to_owned(), TypeSchema::Bool);
        TypeSchema::Object(fields)
    }
}

/// Program-counter value marking a completed generator. No dispatch state
/// ever uses it; the entry `done` check short-circuits completed iterators
/// before dispatch.
const DONE_PC: i64 = -1;

/// Pure structural scan of one generator body.
struct BodyScan {
    /// True when any `return` appears (nested closures excluded).
    has_return: bool,
    /// True when any `return <value>` appears.
    has_return_value: bool,
    /// True when any `break` appears (nested closures excluded). Breaks keep
    /// the conservative finite classification even when they target an inner
    /// loop; precision loss only widens to the honest `Optional` result.
    has_break: bool,
}

fn scan_generator_body(
    stmts: &[ast::Stmt],
    base: u32,
    source_id: u32,
) -> Result<BodyScan, ParseError> {
    let mut scan = BodyScan {
        has_return: false,
        has_return_value: false,
        has_break: false,
    };
    scan_stmts(stmts, &mut scan, base, source_id)?;
    Ok(scan)
}

fn scan_stmts(
    stmts: &[ast::Stmt],
    scan: &mut BodyScan,
    base: u32,
    source_id: u32,
) -> Result<(), ParseError> {
    for stmt in stmts {
        scan_stmt(stmt, scan, base, source_id)?;
    }
    Ok(())
}

fn scan_stmt(
    stmt: &ast::Stmt,
    scan: &mut BodyScan,
    base: u32,
    source_id: u32,
) -> Result<(), ParseError> {
    match stmt {
        ast::Stmt::Expr(expr) => scan_expr(&expr.expr, true, base, source_id),
        ast::Stmt::Decl(ast::Decl::Var(decl)) => {
            for declarator in &decl.decls {
                if let Some(init) = &declarator.init {
                    scan_expr(init, false, base, source_id)?;
                }
            }
            Ok(())
        }
        ast::Stmt::Decl(_) => Ok(()),
        ast::Stmt::Return(ret) => {
            scan.has_return = true;
            if ret.arg.is_some() {
                scan.has_return_value = true;
            }
            if let Some(arg) = &ret.arg {
                scan_expr(arg, false, base, source_id)?;
            }
            Ok(())
        }
        ast::Stmt::If(if_stmt) => {
            scan_expr(&if_stmt.test, false, base, source_id)?;
            scan_stmt(&if_stmt.cons, scan, base, source_id)?;
            if let Some(alt) = &if_stmt.alt {
                scan_stmt(alt, scan, base, source_id)?;
            }
            Ok(())
        }
        ast::Stmt::While(while_stmt) => {
            scan_expr(&while_stmt.test, false, base, source_id)?;
            scan_stmt(&while_stmt.body, scan, base, source_id)
        }
        ast::Stmt::For(for_stmt) => {
            if let Some(init) = &for_stmt.init {
                match init {
                    ast::VarDeclOrExpr::VarDecl(decl) => {
                        for declarator in &decl.decls {
                            if let Some(init) = &declarator.init {
                                scan_expr(init, false, base, source_id)?;
                            }
                        }
                    }
                    ast::VarDeclOrExpr::Expr(expr) => {
                        scan_expr(expr, false, base, source_id)?;
                    }
                }
            }
            if let Some(test) = &for_stmt.test {
                scan_expr(test, false, base, source_id)?;
            }
            if let Some(update) = &for_stmt.update {
                scan_expr(update, false, base, source_id)?;
            }
            scan_stmt(&for_stmt.body, scan, base, source_id)
        }
        ast::Stmt::Block(block) => scan_stmts(&block.stmts, scan, base, source_id),
        ast::Stmt::Break(_) => {
            scan.has_break = true;
            Ok(())
        }
        ast::Stmt::Continue(_) => Ok(()),
        ast::Stmt::Throw(throw) => scan_expr(&throw.arg, false, base, source_id),
        ast::Stmt::Try(try_stmt) => Err(unsupported(
            try_stmt.span,
            base,
            source_id,
            "generator try/finally across suspension",
        )),
        // Other statements carry no yields relevant to suspension; the
        // sequencer rejects unsupported shapes precisely before execution.
        _ => Ok(()),
    }
}

fn scan_expr(
    expr: &ast::Expr,
    statement_position: bool,
    base: u32,
    source_id: u32,
) -> Result<(), ParseError> {
    match expr {
        ast::Expr::Yield(yield_expr) => {
            if yield_expr.delegate {
                return Err(unsupported(
                    yield_expr.span,
                    base,
                    source_id,
                    "generator yield delegation (yield*)",
                ));
            }
            if !statement_position {
                return Err(unsupported(
                    yield_expr.span,
                    base,
                    source_id,
                    "generator sent-value yield expression",
                ));
            }
            if yield_expr.arg.is_none() {
                return Err(unsupported(
                    yield_expr.span,
                    base,
                    source_id,
                    "generator bare yield without a value",
                ));
            }
            if let Some(arg) = &yield_expr.arg {
                scan_expr(arg, false, base, source_id)?;
            }
            Ok(())
        }
        // Nested closures own their control flow; their bodies never belong
        // to this generator's suspension graph.
        ast::Expr::Arrow(_) | ast::Expr::Fn(_) | ast::Expr::Class(_) => Ok(()),
        ast::Expr::Paren(paren) => scan_expr(&paren.expr, false, base, source_id),
        ast::Expr::Array(array) => {
            for element in array.elems.iter().flatten() {
                if element.spread.is_some() {
                    continue;
                }
                scan_expr(&element.expr, false, base, source_id)?;
            }
            Ok(())
        }
        ast::Expr::Object(object) => {
            for property in &object.props {
                match property {
                    ast::PropOrSpread::Spread(_) => {}
                    ast::PropOrSpread::Prop(property) => match property.as_ref() {
                        ast::Prop::KeyValue(property) => {
                            scan_expr(&property.value, false, base, source_id)?;
                        }
                        ast::Prop::Shorthand(_) => {}
                        _ => {}
                    },
                }
            }
            Ok(())
        }
        ast::Expr::Unary(unary) => scan_expr(&unary.arg, false, base, source_id),
        ast::Expr::Update(update) => scan_expr(&update.arg, false, base, source_id),
        ast::Expr::Bin(binary) => {
            scan_expr(&binary.left, false, base, source_id)?;
            scan_expr(&binary.right, false, base, source_id)
        }
        ast::Expr::Assign(assign) => match &assign.left {
            ast::AssignTarget::Simple(_) => scan_expr(&assign.right, false, base, source_id),
            ast::AssignTarget::Pat(_) => scan_expr(&assign.right, false, base, source_id),
        },
        ast::Expr::Member(member) => {
            scan_expr(&member.obj, false, base, source_id)?;
            if let ast::MemberProp::Computed(property) = &member.prop {
                scan_expr(&property.expr, false, base, source_id)?;
            }
            Ok(())
        }
        ast::Expr::SuperProp(super_prop) => match &super_prop.prop {
            ast::SuperProp::Computed(property) => scan_expr(&property.expr, false, base, source_id),
            _ => Ok(()),
        },
        ast::Expr::Cond(cond) => {
            scan_expr(&cond.test, false, base, source_id)?;
            scan_expr(&cond.cons, false, base, source_id)?;
            scan_expr(&cond.alt, false, base, source_id)
        }
        ast::Expr::Call(call) => {
            if let ast::Callee::Expr(callee) = &call.callee {
                // The callee member receiver is ordinary expression
                // position, never a statement yield.
                scan_expr(callee, false, base, source_id)?;
            }
            for arg in &call.args {
                if arg.spread.is_some() {
                    continue;
                }
                scan_expr(&arg.expr, false, base, source_id)?;
            }
            Ok(())
        }
        ast::Expr::New(new_expr) => {
            scan_expr(&new_expr.callee, false, base, source_id)?;
            if let Some(args) = &new_expr.args {
                for arg in args {
                    if arg.spread.is_some() {
                        continue;
                    }
                    scan_expr(&arg.expr, false, base, source_id)?;
                }
            }
            Ok(())
        }
        ast::Expr::Seq(seq) => {
            for expr in &seq.exprs {
                scan_expr(expr, false, base, source_id)?;
            }
            Ok(())
        }
        ast::Expr::Tpl(tpl) => {
            for expr in &tpl.exprs {
                scan_expr(expr, false, base, source_id)?;
            }
            Ok(())
        }
        ast::Expr::TaggedTpl(tagged) => {
            scan_expr(&tagged.tag, false, base, source_id)?;
            for expr in &tagged.tpl.exprs {
                scan_expr(expr, false, base, source_id)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// True when `stmt` is an unconditional loop: `while (true)` with a literal
/// `true` test, or a `for` with no test. Only these shapes can terminate a
/// proven-infinite body.
fn is_unconditional_loop(stmt: &ast::Stmt) -> bool {
    match stmt {
        ast::Stmt::While(while_stmt) => matches!(
            while_stmt.test.as_ref(),
            ast::Expr::Lit(ast::Lit::Bool(value)) if value.value
        ),
        ast::Stmt::For(for_stmt) => for_stmt.test.is_none(),
        _ => false,
    }
}

/// Per-generator state-machine build context. Statement buffers are indexed
/// by dispatch state; `current` is the state receiving straight-line code,
/// or `None` past a terminal suspension point.
struct GenCtx {
    infinite: bool,
    func_span: swc_common::Span,
    /// Persistent cells, captured by the `next` closure. All reads and
    /// writes inside `next` use these captured slots; the constructor-frame
    /// source slots are only dummy-initialized for availability and never
    /// observed.
    pc: u16,
    done: u16,
    has_value: u16,
    held: u16,
    /// Constructor-frame dummy initializers `(slot, schema)`, emitted into
    /// the constructor body after the state machine is built.
    dummies: Vec<(u16, TypeSchema)>,
    states: Vec<Vec<Stmt>>,
    current: Option<usize>,
    loops: Vec<LoopFrame>,
}

struct LoopFrame {
    head: usize,
    after: usize,
    update: Option<usize>,
}

fn dummy_for_schema(schema: &TypeSchema) -> Expr {
    match schema {
        TypeSchema::Float => Expr::Float(0.0),
        TypeSchema::Int => Expr::Int(0),
        TypeSchema::Bool => Expr::Bool(false),
        TypeSchema::Null => Expr::Null,
        _ => Expr::Int(0),
    }
}

impl<'a> Lowerer<'a> {
    /// Declared return type of a generator constructor: `Generator<number>`
    /// or `Generator<number, number|void>`. Returns the promised return
    /// flavor: `None` when absent, `Some(true)` for a numeric return and
    /// `Some(false)` for `void`.
    fn gen_parse_annotation(&self, function: &ast::FnDecl) -> Result<Option<bool>, ParseError> {
        let Some(annotation) = function.function.return_type.as_ref() else {
            return Ok(None);
        };
        let ast::TsType::TsTypeRef(reference) = &annotation.type_ann.as_ref() else {
            return Err(unsupported(
                annotation.span,
                self.base,
                self.source_id,
                "generator return type (expected Generator<number> or Generator<number, number | void>)",
            ));
        };
        let name = match &reference.type_name {
            ast::TsEntityName::Ident(ident) => ident.sym.to_string(),
            ast::TsEntityName::TsQualifiedName(_) => String::new(),
        };
        if name != "Generator" {
            return Err(unsupported(
                annotation.span,
                self.base,
                self.source_id,
                "generator return type (expected Generator<number> or Generator<number, number | void>)",
            ));
        }
        let params = reference
            .type_params
            .as_ref()
            .map(|params| params.params.as_slice())
            .unwrap_or(&[]);
        if params.is_empty() || params.len() > 3 {
            return Err(unsupported(
                annotation.span,
                self.base,
                self.source_id,
                "generator return type (expected Generator<number> or Generator<number, number | void>)",
            ));
        }
        let yield_schema = self.lower_type(&params[0])?;
        if !Self::is_numeric_schema(&yield_schema) {
            return Err(ParseError::at_span(
                byte_span(params[0].span(), self.base, self.source_id),
                "generator yield type must be a number".to_owned(),
            ));
        }
        if params.len() >= 2 {
            let back = self.lower_type(&params[1])?;
            match back {
                TypeSchema::Number => return Ok(Some(true)),
                TypeSchema::Null => return Ok(Some(false)),
                _ => {
                    return Err(ParseError::at_span(
                        byte_span(params[1].span(), self.base, self.source_id),
                        "generator return type must be number or void".to_owned(),
                    ));
                }
            }
        }
        Ok(None)
    }

    /// True when `expr` denotes a locally lowered generator iterator: its
    /// static schema is exactly a registered iterator nominal. Structural
    /// records, callables, host values and names never qualify.
    pub(crate) fn generator_iterator_of(&self, expr: &ast::Expr) -> Option<GeneratorInfo> {
        let schema = self.expression_schema(expr)?;
        match schema {
            TypeSchema::Named(name, args) if args.is_empty() => {
                self.gen_iterators.get(&name).cloned()
            }
            _ => None,
        }
    }

    /// Result-typed value resolution: maps a result nominal back to its
    /// generator representation through the exact reverse index. User
    /// shapes never qualify.
    pub(crate) fn generator_result_of(&self, expr: &ast::Expr) -> Option<GeneratorInfo> {
        let schema = self.expression_schema(expr)?;
        match schema {
            TypeSchema::Named(name, args) if args.is_empty() => {
                let iterator = self.gen_results.get(&name)?.clone();
                self.gen_iterators.get(&iterator).cloned()
            }
            _ => None,
        }
    }

    /// Lower `receiver.next()` with exactly-once receiver evaluation. The
    /// receiver is hoisted into a fresh hidden slot, so the existing
    /// ownership analysis (which only tracks named locals) never marks the
    /// source iterator partially moved: repeated `next()` calls from one
    /// binding, and whole-value uses afterwards, stay valid. The `next`
    /// closure itself is never consumed by calling it.
    pub(crate) fn lower_generator_next_call(
        &mut self,
        receiver: Box<ast::Expr>,
        member_span: swc_common::Span,
        call_span: swc_common::Span,
        args: Vec<ast::ExprOrSpread>,
    ) -> Result<Expr, ParseError> {
        if let Some(first) = args.first() {
            return Err(unsupported(
                first.span(),
                self.base,
                self.source_id,
                "generator next call with an argument",
            ));
        }
        let info = self
            .generator_iterator_of(&receiver)
            .expect("generator next receiver was resolved before lowering");
        let receiver_span = receiver.span();
        let receiver_schema = self
            .expression_schema(&receiver)
            .unwrap_or(TypeSchema::Unknown);
        let site = byte_span(call_span, self.base, self.source_id);
        let line = line_at(self.source, site.lo);
        let lowered_receiver = self.lower_expr(*receiver)?;
        let receiver_slot = self.alloc_hidden_local(receiver_span)?;
        let mut stmts = vec![Stmt::Let {
            index: receiver_slot,
            declared_schema: Some(receiver_schema),
            expr: lowered_receiver,
            line,
        }];
        let get_id = self.alloc_node_id();
        let member_site = byte_span(member_span, self.base, self.source_id);
        self.call_sites.push(ParsedCallSite {
            id: get_id,
            callee_span: member_site,
            expr_span: member_site,
            target: ParsedCallTarget::Function(vm::BuiltinFunction::Get.call_index()),
            name: "next".to_owned(),
            scope_id: self.scope_id,
            is_namespace_call: false,
        });
        let get = Expr::Call(
            vm::BuiltinFunction::Get.call_index(),
            Vec::new(),
            vec![Expr::Var(receiver_slot), Expr::String("next".to_owned())],
            None,
            Some(get_id),
        );
        let next_callable = TypeSchema::Callable {
            params: Vec::new(),
            result: Box::new(info.result_schema()),
        };
        let thunk_slot = self.alloc_hidden_local(member_span)?;
        stmts.push(Stmt::Let {
            index: thunk_slot,
            declared_schema: Some(next_callable),
            expr: get,
            line,
        });
        let call_id = self.alloc_node_id();
        self.call_sites.push(ParsedCallSite {
            id: call_id,
            callee_span: member_site,
            expr_span: site,
            target: ParsedCallTarget::Local(thunk_slot),
            name: "next".to_owned(),
            scope_id: self.scope_id,
            is_namespace_call: false,
        });
        Ok(Expr::Block {
            stmts,
            expr: Box::new(Expr::LocalCall(
                thunk_slot,
                Vec::new(),
                Vec::new(),
                Some(call_id),
            )),
        })
    }

    /// Entry point from `lower_function`: lower a `function*` declaration in
    /// reference-array mode. Ordinary mode never reaches here.
    pub(crate) fn lower_generator_function(
        &mut self,
        function: &ast::FnDecl,
        exported: bool,
        output: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let name = function.ident.sym.to_string();
        let (index, arity) = *self.function_indices.get(&name).ok_or_else(|| {
            unsupported(
                function.ident.span,
                self.base,
                self.source_id,
                "function declaration",
            )
        })?;
        if self.locals.contains_key(&name) {
            return Err(ParseError::at_span(
                byte_span(function.ident.span, self.base, self.source_id),
                format!("function '{name}' conflicts with a local binding"),
            ));
        }
        let Some(body) = function.function.body.as_ref() else {
            return Err(unsupported(
                function.function.span,
                self.base,
                self.source_id,
                "ambient function declaration",
            ));
        };
        let type_params = function
            .function
            .type_params
            .as_ref()
            .map(|params| {
                params
                    .params
                    .iter()
                    .map(|param| {
                        if param.constraint.is_some()
                            || param.default.is_some()
                            || param.is_in
                            || param.is_out
                            || param.is_const
                        {
                            return Err(unsupported(
                                param.span,
                                self.base,
                                self.source_id,
                                "generic constraints, defaults, variance, or const modifiers",
                            ));
                        }
                        Ok(param.name.sym.to_string())
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        let mut function_type_params = HashSet::new();
        for param in &type_params {
            if !function_type_params.insert(param.clone()) {
                return Err(ParseError::new(format!(
                    "duplicate TypeScript generic parameter '{param}'"
                )));
            }
        }
        if !type_params.is_empty() {
            return Err(unsupported(
                function.function.span,
                self.base,
                self.source_id,
                "generic generator function",
            ));
        }
        if exported {
            // Iterator/result nominals are module-private (their identity
            // cannot cross modules without loader support), so generator
            // constructors cannot be exported. Same-module private
            // generators cover the admitted scope.
            return Err(unsupported(
                function.ident.span,
                self.base,
                self.source_id,
                "exported generator function",
            ));
        }
        if function.function.is_async {
            return Err(unsupported(
                function.function.span,
                self.base,
                self.source_id,
                "async or generator function",
            ));
        }
        let previous_generic_type_params =
            std::mem::replace(&mut self.generic_type_params, function_type_params);
        let result = self
            .lower_generator_function_inner(function, body, exported, output, &name, index, arity);
        self.generic_type_params = previous_generic_type_params;
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_generator_function_inner(
        &mut self,
        function: &ast::FnDecl,
        body: &ast::FunctionBody,
        exported: bool,
        output: &mut Vec<Stmt>,
        name: &str,
        index: u16,
        arity: u8,
    ) -> Result<(), ParseError> {
        // Structural scan first: unsupported suspension shapes fail before
        // any state is built or executed.
        let scan = scan_generator_body(&body.stmts, self.base, self.source_id)?;
        let promised_return = self.gen_parse_annotation(function)?;
        // Infinite only with a structural no-completion proof: no return
        // anywhere, no break anywhere, and an unconditional loop in tail
        // position. Anything else is honestly finite.
        let infinite = !scan.has_return
            && !scan.has_break
            && body.stmts.last().is_some_and(is_unconditional_loop);
        // A promised numeric return constrains delivered values, not
        // delivery itself: bodies that never return a value (including
        // proven-infinite ones) satisfy it vacuously through the honest
        // `Optional` result. Only a void promise with a valued return
        // would be a schema lie.
        // Collect yield value schemas for the honest value representation.
        // (Runs after parameters below, via the scoped pre-scan.)
        let mut value_schema: Option<TypeSchema> = None;
        if scan.has_return_value && promised_return == Some(false) {
            return Err(ParseError::at_span(
                byte_span(function.function.span, self.base, self.source_id),
                format!("generator '{name}' declares a void return but returns a value"),
            ));
        }

        // Reserve the private nominal names before parameters so user
        // collisions fail early with the same duplicate diagnostic.
        let iterator_nominal = generator_iterator_type_name(name);
        let result_nominal = generator_result_type_name(name);
        for reserved in [&iterator_nominal, &result_nominal] {
            if self.struct_schemas.contains_key(reserved)
                || self.named_types.contains_key(reserved)
                || self.named_type_declarations.contains_key(reserved)
                || self.function_indices.contains_key(reserved)
                || self.imported_functions.contains_key(reserved)
                || self.host_function_indices.contains_key(reserved)
                || self.locals.contains_key(reserved)
            {
                return Err(ParseError::at_span(
                    byte_span(function.ident.span, self.base, self.source_id),
                    format!("duplicate TypeScript type '{reserved}'"),
                ));
            }
        }

        // Constructor scope: the enclosing (module) scope stays visible as
        // the base, mirroring arrow capture rather than plain-function
        // isolation. Parameters and body bindings layer on top with ordinary
        // shadowing; the `next` closure captures whatever it observes into
        // persistent cells. Default evaluation timing is untouched (thunks
        // still evaluate in caller scope through the existing machinery).
        let previous_locals = self.locals.clone();
        let previous_local_types = self.local_types.clone();
        let previous_mutable_locals = self.mutable_locals.clone();
        let previous_scope = self.scope_id;
        let scope_id = self.next_scope_id;
        self.next_scope_id = self.next_scope_id.checked_add(1).ok_or_else(|| {
            unsupported(
                function.function.span,
                self.base,
                self.source_id,
                "lexical scope count",
            )
        })?;
        self.scope_id = scope_id;
        let previous_bindings_len = self.local_bindings.len();
        let declaration_start = self.local_decls.len();
        let mut param_slots = Vec::new();
        let mut arg_names = Vec::new();
        let mut arg_schemas = Vec::new();
        let mut default_seen = false;
        let mut seen_params = HashSet::new();
        for (order, param) in function.function.params.iter().enumerate() {
            let Some((binding, default)) = parameter_binding(&param.pat) else {
                self.restore_generator_scope(
                    previous_locals,
                    previous_local_types,
                    previous_mutable_locals,
                    previous_scope,
                    previous_bindings_len,
                );
                return Err(unsupported(
                    param.span(),
                    self.base,
                    self.source_id,
                    "function parameter pattern",
                ));
            };
            if default.is_some() {
                default_seen = true;
            } else if default_seen {
                self.restore_generator_scope(
                    previous_locals,
                    previous_local_types,
                    previous_mutable_locals,
                    previous_scope,
                    previous_bindings_len,
                );
                return Err(unsupported(
                    param.span(),
                    self.base,
                    self.source_id,
                    "required parameter after a default parameter",
                ));
            }
            let param_name = binding.id.sym.to_string();
            if binding.id.sym.as_ref().starts_with("__gen_")
                || binding.id.sym.as_ref().starts_with("__CompatGen")
            {
                self.restore_generator_scope(
                    previous_locals,
                    previous_local_types,
                    previous_mutable_locals,
                    previous_scope,
                    previous_bindings_len,
                );
                return Err(ParseError::at_span(
                    byte_span(binding.id.span, self.base, self.source_id),
                    format!(
                        "generator parameter '{param_name}' uses a reserved generator state prefix"
                    ),
                ));
            }
            if binding.type_ann.as_ref().is_none() {
                self.restore_generator_scope(
                    previous_locals,
                    previous_local_types,
                    previous_mutable_locals,
                    previous_scope,
                    previous_bindings_len,
                );
                return Err(ParseError::at_span(
                    byte_span(binding.id.span, self.base, self.source_id),
                    format!("TypeScript noImplicitAny: parameter '{param_name}' requires a type"),
                ));
            }
            // Parameters shadow enclosing bindings (arrow-like scope); only
            // a repeated parameter name in one list is a duplicate.
            if !seen_params.insert(param_name.clone()) {
                self.restore_generator_scope(
                    previous_locals,
                    previous_local_types,
                    previous_mutable_locals,
                    previous_scope,
                    previous_bindings_len,
                );
                return Err(ParseError::at_span(
                    byte_span(binding.id.span, self.base, self.source_id),
                    format!("duplicate TypeScript parameter '{param_name}'"),
                ));
            }
            let schema =
                match self.lower_type(&binding.type_ann.as_ref().expect("checked").type_ann) {
                    Ok(schema) => self.ref_map_number_array(schema, binding.id.span),
                    Err(error) => {
                        self.restore_generator_scope(
                            previous_locals,
                            previous_local_types,
                            previous_mutable_locals,
                            previous_scope,
                            previous_bindings_len,
                        );
                        return Err(error);
                    }
                };
            let slot = match self.next_slot.checked_add(1) {
                Some(_) => {
                    let slot = self.next_slot;
                    self.next_slot += 1;
                    slot
                }
                None => {
                    self.restore_generator_scope(
                        previous_locals,
                        previous_local_types,
                        previous_mutable_locals,
                        previous_scope,
                        previous_bindings_len,
                    );
                    return Err(unsupported(
                        binding.id.span,
                        self.base,
                        self.source_id,
                        "function local slot limit",
                    ));
                }
            };
            self.locals.insert(param_name.clone(), slot);
            self.local_types.insert(param_name.clone(), schema.clone());
            self.mutable_locals.insert(slot);
            self.local_bindings.push((param_name.clone(), slot));
            let ident_span = byte_span(binding.id.span, self.base, self.source_id);
            let node_id = self.alloc_node_id();
            self.local_decls.push(LocalDeclSite {
                id: node_id,
                ident_span,
                stmt_span: byte_span(param.span(), self.base, self.source_id),
                slot,
                name: param_name.clone(),
                scope_id,
                decl_order: u32::try_from(order).unwrap_or(u32::MAX),
            });
            param_slots.push(slot);
            arg_names.push(param_name);
            arg_schemas.push(Some(schema));
        }

        // Scoped pre-scan after parameters: register each body-local scalar
        // schema in walk order so yield values resolve exactly as they will
        // during lowering. Integer precision applies from here through the
        // state machine build.
        self.in_gen_body = true;
        gen_prescan_yield_schemas(self, &body.stmts, &mut value_schema)?;
        let value_schema = value_schema.unwrap_or(TypeSchema::Number);

        // Synthesize the private nominals with truthful spans on this
        // declaration now that the value representation is known.
        let info = GeneratorInfo {
            iterator_nominal: iterator_nominal.clone(),
            result_nominal: result_nominal.clone(),
            value_schema: value_schema.clone(),
            infinite,
        };
        let result_object = info.result_object_schema();
        let iterator_object = {
            let mut fields = HashMap::new();
            fields.insert(
                "next".to_owned(),
                TypeSchema::Callable {
                    params: Vec::new(),
                    result: Box::new(info.result_schema()),
                },
            );
            TypeSchema::Object(fields)
        };
        let declaration_span = byte_span(function.function.span, self.base, self.source_id);
        for (nominal, object) in [
            (iterator_nominal.clone(), iterator_object),
            (result_nominal.clone(), result_object),
        ] {
            let declaration = StructDecl {
                name: nominal.clone(),
                type_params: Vec::new(),
                body_schema: object,
                origin: StructDeclOrigin::Guest,
            };
            self.named_type_declarations
                .insert(nominal.clone(), declaration.clone());
            self.struct_schemas.insert(nominal.clone(), declaration);
            self.type_provenance.push(GuestTypeProvenance {
                canonical_name: nominal.clone(),
                source_name: name.to_owned(),
                source_id: self.source_id,
                declaration_span: Some(declaration_span),
            });
            let site_id = self.alloc_node_id();
            self.struct_decl_sites.push(StructDeclSite {
                id: site_id,
                ident_span: byte_span(function.ident.span, self.base, self.source_id),
                decl_span: declaration_span,
                name: nominal,
                scope_id: 0,
            });
        }
        self.gen_iterators
            .insert(iterator_nominal.clone(), info.clone());
        self.gen_results
            .insert(result_nominal.clone(), iterator_nominal.clone());

        // Persistent scalar cells, constructor-owned. The constructor writes
        // dummy initializers (never observed: every read inside `next`
        // follows the first-next initialization in flow order); the `next`
        // closure captures them into shared cells.
        let mut ctx = GenCtx {
            infinite,
            func_span: function.function.span,
            pc: 0,
            done: 0,
            has_value: 0,
            held: 0,
            dummies: Vec::new(),
            states: vec![Vec::new()],
            current: Some(0),
            loops: Vec::new(),
        };
        let cell_line = line_at(
            self.source,
            byte_span(function.ident.span, self.base, self.source_id).lo,
        );
        let pc_slot =
            self.gen_persistent_cell(TypeSchema::Int, function.function.span, &mut ctx.dummies)?;
        let done_slot =
            self.gen_persistent_cell(TypeSchema::Bool, function.function.span, &mut ctx.dummies)?;
        let has_slot =
            self.gen_persistent_cell(TypeSchema::Bool, function.function.span, &mut ctx.dummies)?;
        let held_slot = self.gen_persistent_cell(
            value_schema.clone(),
            function.function.span,
            &mut ctx.dummies,
        )?;
        ctx.pc = pc_slot;
        ctx.done = done_slot;
        ctx.has_value = has_slot;
        ctx.held = held_slot;

        // Build the `next` closure inside the constructor scope: pre-capture
        // parameters so no lowering path observes an uncaptured first touch,
        // then capture the persistent cells.
        let closure_scope = self.next_scope_id;
        self.next_scope_id = self.next_scope_id.checked_add(1).ok_or_else(|| {
            unsupported(
                function.function.span,
                self.base,
                self.source_id,
                "lexical scope count",
            )
        })?;
        self.closure_captures
            .push(super::parser::ClosureCaptureContext {
                local_slots: HashSet::new(),
                captures: HashMap::new(),
                capture_copies: Vec::new(),
            });
        // Parameters must be captured before any body lowering observes
        // them, so every path (reads, calls, updates) sees captured slots.
        for param_name in arg_names.clone() {
            if let Some(source) = self.locals.get(&param_name).copied() {
                self.capture_slot(source, &param_name, function.function.span)?;
            }
        }
        let capture_name_pc = "__gen_pc";
        let capture_name_done = "__gen_done";
        let capture_name_has = "__gen_has";
        let capture_name_held = "__gen_held";
        let pc = self.capture_slot(pc_slot, capture_name_pc, function.function.span)?;
        let done = self.capture_slot(done_slot, capture_name_done, function.function.span)?;
        let has_value = self.capture_slot(has_slot, capture_name_has, function.function.span)?;
        let held = self.capture_slot(held_slot, capture_name_held, function.function.span)?;
        ctx.pc = pc;
        ctx.done = done;
        ctx.has_value = has_value;
        ctx.held = held;
        let previous_scope_for_closure = self.scope_id;
        self.scope_id = closure_scope;
        self.stmt_value_depth += 1;
        let next_result = self.gen_next_body(body, &mut ctx, name);
        self.stmt_value_depth -= 1;
        self.scope_id = previous_scope_for_closure;
        let capture_context = self
            .closure_captures
            .pop()
            .expect("generator next closure context was pushed");
        // Lexical scope for the closure itself (no direct declarations;
        // hidden/captured slots need no sites).
        self.function_scopes.push(ParsedLexicalScope {
            id: closure_scope,
            parent: Some(scope_id),
            range: byte_span(function.function.span, self.base, self.source_id),
            declarations: Vec::new(),
            functions: Vec::new(),
        });
        self.next_scope_id = self.next_scope_id.max(closure_scope + 1);
        let next_body = next_result?;
        self.in_gen_body = false;
        let next_closure = Expr::Closure(vm::compiler::ir::ClosureExpr {
            param_slots: Vec::new(),
            capture_copies: capture_context.capture_copies,
            body: Box::new(next_body),
        });

        // Constructor body: dummy cell initializers, then the iterator
        // record. Assembled after the state machine so body-local dummy
        // slots allocated during lowering are included.
        let mut constructor_stmts = Vec::new();
        for (slot, schema) in ctx.dummies {
            constructor_stmts.push(Stmt::Let {
                index: slot,
                declared_schema: Some(schema.clone()),
                expr: dummy_for_schema(&schema),
                line: cell_line,
            });
        }
        let iterator_record = Self::builtin_call(vm::BuiltinFunction::MapNew, Vec::new());
        let iterator_record = Self::builtin_call(
            vm::BuiltinFunction::Set,
            vec![
                iterator_record,
                Expr::String("next".to_owned()),
                next_closure,
            ],
        );
        let return_schema = info.iterator_schema();
        let scope_declarations = self.local_decls[declaration_start..]
            .iter()
            .filter(|site| site.scope_id == scope_id)
            .map(|site| site.slot)
            .collect();
        let function_id = self.alloc_node_id();
        let function_order = u32::try_from(self.function_sites.len()).unwrap_or(u32::MAX);
        self.function_sites.push(FunctionDeclSite {
            id: function_id,
            ident_span: byte_span(function.ident.span, self.base, self.source_id),
            function_index: index,
            name: name.to_owned(),
            scope_id: 0,
            decl_order: function_order,
        });
        self.functions.push(FunctionDecl {
            name: name.to_owned(),
            arity,
            index,
            args: arg_names.clone(),
            arg_schemas,
            return_schema: Some(return_schema.clone()),
            type_params: Vec::new(),
            exported,
            return_type: schema_value_type(&return_schema),
            symbol: None,
        });
        self.function_impls.insert(
            index,
            FunctionImpl {
                param_slots,
                capture_copies: Vec::new(),
                body_stmts: constructor_stmts,
                body_expr: iterator_record,
                body_expr_line: line_at(
                    self.source,
                    byte_span(body.span, self.base, self.source_id).lo,
                ),
            },
        );
        self.local_bindings.truncate(previous_bindings_len);
        self.locals = previous_locals;
        self.local_types = previous_local_types;
        self.mutable_locals = previous_mutable_locals;
        self.scope_id = previous_scope;
        self.functions.sort_by_key(|declaration| declaration.index);
        output.push(Stmt::FuncDecl {
            name: name.to_owned(),
            index,
            arity,
            args: arg_names,
            exported,
            has_impl: true,
            line: line_at(
                self.source,
                byte_span(function.ident.span, self.base, self.source_id).lo,
            ),
        });
        self.stmt_spans.push(StmtSpanSite {
            line: line_at(
                self.source,
                byte_span(function.function.span, self.base, self.source_id).lo,
            ),
            span: byte_span(function.function.span, self.base, self.source_id),
        });
        self.next_scope_id = self.next_scope_id.max(scope_id + 1);
        self.function_scopes.push(ParsedLexicalScope {
            id: scope_id,
            parent: Some(0),
            range: byte_span(function.function.span, self.base, self.source_id),
            declarations: scope_declarations,
            functions: Vec::new(),
        });
        Ok(())
    }

    fn restore_generator_scope(
        &mut self,
        previous_locals: HashMap<String, u16>,
        previous_local_types: HashMap<String, TypeSchema>,
        previous_mutable_locals: HashSet<u16>,
        previous_scope: u32,
        previous_bindings_len: usize,
    ) {
        self.local_bindings.truncate(previous_bindings_len);
        self.locals = previous_locals;
        self.local_types = previous_local_types;
        self.mutable_locals = previous_mutable_locals;
        self.scope_id = previous_scope;
    }

    /// Allocate one constructor-owned persistent scalar cell and record its
    /// dummy initializer. Returns the constructor-frame source slot; the
    /// caller captures it into the `next` closure before use.
    fn gen_persistent_cell(
        &mut self,
        schema: TypeSchema,
        span: swc_common::Span,
        dummies: &mut Vec<(u16, TypeSchema)>,
    ) -> Result<u16, ParseError> {
        let slot = self.next_slot;
        self.next_slot = self.next_slot.checked_add(1).ok_or_else(|| {
            unsupported(span, self.base, self.source_id, "generator cell slot limit")
        })?;
        dummies.push((slot, schema));
        Ok(slot)
    }

    /// Build the `next` closure body: entry `done` check, the pc dispatch
    /// driver, and the result-record tail.
    fn gen_next_body(
        &mut self,
        body: &ast::FunctionBody,
        ctx: &mut GenCtx,
        name: &str,
    ) -> Result<Expr, ParseError> {
        let body_span = body.span;
        for stmt in &body.stmts {
            // A terminal statement ends the reachable prefix; anything after
            // it is genuinely unreachable source.
            if ctx.current.is_none() {
                return Err(ParseError::at_span(
                    byte_span(stmt.span(), self.base, self.source_id),
                    "unreachable statement after suspension, return or jump in generator body"
                        .to_owned(),
                ));
            }
            self.gen_stmt(stmt.clone(), ctx, name)?;
        }
        // Falling off the end completes without a value, mirroring an
        // implicit `return undefined`.
        if ctx.current.is_some() {
            self.gen_complete(ctx, false, None, body_span)?;
        }
        // Assemble the driver: a total pc dispatch chain ending in the miss
        // branch. Each straight-line state runs, sets pc for its successor,
        // and falls to the loop end for re-dispatch; only suspension breaks
        // out.
        let driver_span = ctx.func_span;
        let driver_site = byte_span(driver_span, self.base, self.source_id);
        let driver_line = line_at(self.source, driver_site.lo);
        let driver_body = self.gen_driver_chain(ctx, driver_line, driver_site)?;
        let driver = Stmt::While {
            condition: Expr::Bool(true),
            body: vec![driver_body],
            line: driver_line,
        };
        self.stmt_spans.push(StmtSpanSite {
            line: driver_line,
            span: driver_site,
        });
        // Entry: a completed iterator skips the driver and reports the
        // terminal result again (without a numeric value); otherwise the
        // driver runs to the next suspension and the tail projects it.
        let entry = Stmt::IfElse {
            condition: Expr::Var(ctx.done),
            then_branch: vec![Stmt::Assign {
                kind: AssignmentKind::Set,
                index: ctx.has_value,
                expr: Expr::Bool(false),
                line: driver_line,
            }],
            else_branch: vec![driver],
            line: driver_line,
        };
        self.stmt_spans.push(StmtSpanSite {
            line: driver_line,
            span: driver_site,
        });
        let tail = self.gen_result_tail(ctx);
        Ok(Expr::Block {
            stmts: vec![entry],
            expr: Box::new(tail),
        })
    }

    /// Build the total pc dispatch chain ending in the miss branch.
    fn gen_driver_chain(
        &mut self,
        ctx: &GenCtx,
        driver_line: u32,
        driver_site: vm::compiler::source_map::Span,
    ) -> Result<Stmt, ParseError> {
        // Innermost else first: the miss branch.
        let miss: Stmt = if ctx.infinite {
            Stmt::Assign {
                kind: AssignmentKind::Set,
                index: ctx.pc,
                expr: Expr::Var(ctx.pc),
                line: driver_line,
            }
        } else {
            // Finite miss: complete without a value, then break out.
            Stmt::IfElse {
                condition: Expr::Bool(true),
                then_branch: vec![
                    Stmt::Assign {
                        kind: AssignmentKind::Set,
                        index: ctx.done,
                        expr: Expr::Bool(true),
                        line: driver_line,
                    },
                    Stmt::Assign {
                        kind: AssignmentKind::Set,
                        index: ctx.has_value,
                        expr: Expr::Bool(false),
                        line: driver_line,
                    },
                    Stmt::Break { line: driver_line },
                ],
                else_branch: Vec::new(),
                line: driver_line,
            }
        };
        let mut chain = Some(miss);
        for (state, buffer) in ctx.states.iter().enumerate().rev() {
            let condition = Expr::Eq(
                Box::new(Expr::Div(
                    Box::new(Expr::Var(ctx.pc)),
                    Box::new(Expr::Float(1.0)),
                )),
                Box::new(Expr::Div(
                    Box::new(Expr::Int(state as i64)),
                    Box::new(Expr::Float(1.0)),
                )),
            );
            let branch = Stmt::IfElse {
                condition,
                then_branch: buffer.clone(),
                else_branch: chain.take().map(|stmt| vec![stmt]).unwrap_or_default(),
                line: driver_line,
            };
            self.stmt_spans.push(StmtSpanSite {
                line: driver_line,
                span: driver_site,
            });
            chain = Some(branch);
        }
        Ok(chain.expect("at least the miss branch exists"))
    }

    /// Result-record tail projecting the suspension cells. Finite bodies use
    /// the honest optional value (explicit `Null` when absent, matching the
    /// accepted optional-record representation); proven-infinite bodies
    /// carry the value directly.
    fn gen_result_tail(&self, ctx: &GenCtx) -> Expr {
        let value = if ctx.infinite {
            Expr::Var(ctx.held)
        } else {
            Expr::IfElse {
                condition: Box::new(Expr::Var(ctx.has_value)),
                then_expr: Box::new(Expr::Var(ctx.held)),
                else_expr: Box::new(Expr::Null),
            }
        };
        let record = Self::builtin_call(vm::BuiltinFunction::MapNew, Vec::new());
        let record = Self::builtin_call(
            vm::BuiltinFunction::Set,
            vec![record, Expr::String("value".to_owned()), value],
        );
        Self::builtin_call(
            vm::BuiltinFunction::Set,
            vec![record, Expr::String("done".to_owned()), Expr::Var(ctx.done)],
        )
    }

    fn gen_line(&self, span: swc_common::Span) -> u32 {
        line_at(self.source, byte_span(span, self.base, self.source_id).lo)
    }

    fn gen_new_state(&mut self, ctx: &mut GenCtx) -> usize {
        let id = ctx.states.len();
        ctx.states.push(Vec::new());
        id
    }

    fn gen_emit(&mut self, ctx: &mut GenCtx, stmt: Stmt) {
        let current = ctx.current.expect("emitted into a terminated state");
        ctx.states[current].push(stmt);
    }

    /// Suspend with `value`: record the yield, arm the resume state, and
    /// break the driver. The resumption state continues after the yield.
    fn gen_suspend(
        &mut self,
        ctx: &mut GenCtx,
        value: Expr,
        span: swc_common::Span,
    ) -> Result<(), ParseError> {
        let line = self.gen_line(span);
        let resume = self.gen_new_state(ctx);
        self.gen_emit(
            ctx,
            Stmt::Assign {
                kind: AssignmentKind::Set,
                index: ctx.held,
                expr: value,
                line,
            },
        );
        self.gen_emit(
            ctx,
            Stmt::Assign {
                kind: AssignmentKind::Set,
                index: ctx.has_value,
                expr: Expr::Bool(true),
                line,
            },
        );
        self.gen_emit(
            ctx,
            Stmt::Assign {
                kind: AssignmentKind::Set,
                index: ctx.done,
                expr: Expr::Bool(false),
                line,
            },
        );
        self.gen_emit(
            ctx,
            Stmt::Assign {
                kind: AssignmentKind::Set,
                index: ctx.pc,
                expr: Expr::Int(resume as i64),
                line,
            },
        );
        self.gen_emit(ctx, Stmt::Break { line });
        self.stmt_spans.push(StmtSpanSite {
            line,
            span: byte_span(span, self.base, self.source_id),
        });
        ctx.current = Some(resume);
        Ok(())
    }

    /// Complete with `done`: record the optional return value once, arm the
    /// terminal pc, and break the driver. Later completions carry no value.
    fn gen_complete(
        &mut self,
        ctx: &mut GenCtx,
        has_value: bool,
        value: Option<Expr>,
        span: swc_common::Span,
    ) -> Result<(), ParseError> {
        let line = self.gen_line(span);
        if let Some(value) = value {
            self.gen_emit(
                ctx,
                Stmt::Assign {
                    kind: AssignmentKind::Set,
                    index: ctx.held,
                    expr: value,
                    line,
                },
            );
        }
        self.gen_emit(
            ctx,
            Stmt::Assign {
                kind: AssignmentKind::Set,
                index: ctx.has_value,
                expr: Expr::Bool(has_value),
                line,
            },
        );
        self.gen_emit(
            ctx,
            Stmt::Assign {
                kind: AssignmentKind::Set,
                index: ctx.done,
                expr: Expr::Bool(true),
                line,
            },
        );
        self.gen_emit(
            ctx,
            Stmt::Assign {
                kind: AssignmentKind::Set,
                index: ctx.pc,
                expr: Expr::Int(DONE_PC),
                line,
            },
        );
        self.gen_emit(ctx, Stmt::Break { line });
        self.stmt_spans.push(StmtSpanSite {
            line,
            span: byte_span(span, self.base, self.source_id),
        });
        ctx.current = None;
        Ok(())
    }

    /// Transfer control to `target` without suspending: set pc and fall to
    /// the driver loop end for re-dispatch.
    fn gen_goto(&mut self, ctx: &mut GenCtx, target: usize, span: swc_common::Span) {
        let line = self.gen_line(span);
        self.gen_emit(
            ctx,
            Stmt::Assign {
                kind: AssignmentKind::Set,
                index: ctx.pc,
                expr: Expr::Int(target as i64),
                line,
            },
        );
    }

    /// Sequence source statements into the current dispatch state, splitting
    /// states at suspension and completion points.
    fn gen_seq(
        &mut self,
        stmts: Vec<ast::Stmt>,
        ctx: &mut GenCtx,
        name: &str,
    ) -> Result<(), ParseError> {
        for stmt in stmts {
            if ctx.current.is_none() {
                return Err(ParseError::at_span(
                    byte_span(stmt.span(), self.base, self.source_id),
                    "unreachable statement after suspension, return or jump in generator body"
                        .to_owned(),
                ));
            }
            self.gen_stmt(stmt, ctx, name)?;
        }
        Ok(())
    }

    /// Lower one body statement. Control shapes compile to pc dispatch;
    /// single statements delegate to the existing lowering paths so
    /// evaluation order, call semantics and error behavior match ordinary
    /// functions exactly.
    fn gen_stmt(
        &mut self,
        stmt: ast::Stmt,
        ctx: &mut GenCtx,
        name: &str,
    ) -> Result<(), ParseError> {
        let fallback_span = stmt.span();
        match stmt {
            ast::Stmt::Decl(ast::Decl::Var(decl)) => self.gen_var_decl(*decl, ctx),
            ast::Stmt::Expr(expr) => self.gen_expr_stmt(*expr.expr, ctx),
            ast::Stmt::Block(block) => self.gen_block(block.span, block.stmts, ctx, name),
            ast::Stmt::If(if_stmt) => self.gen_if(if_stmt, ctx, name),
            ast::Stmt::While(while_stmt) => self.gen_while(while_stmt, ctx, name),
            ast::Stmt::For(for_stmt) => self.gen_for(for_stmt, ctx, name),
            ast::Stmt::Break(break_stmt) => {
                if break_stmt.label.is_some() {
                    return Err(unsupported(
                        fallback_span,
                        self.base,
                        self.source_id,
                        "labeled break in generator body",
                    ));
                }
                self.gen_break_continue(break_stmt.span, true, ctx)
            }
            ast::Stmt::Continue(continue_stmt) => {
                if continue_stmt.label.is_some() {
                    return Err(unsupported(
                        fallback_span,
                        self.base,
                        self.source_id,
                        "labeled continue in generator body",
                    ));
                }
                self.gen_break_continue(continue_stmt.span, false, ctx)
            }
            ast::Stmt::Return(ret) => {
                let span = ret.span;
                match ret.arg {
                    Some(value) => {
                        let schema = self.expression_schema(&value);
                        if !schema.as_ref().is_some_and(Self::is_numeric_schema) {
                            return Err(ParseError::at_span(
                                byte_span(value.span(), self.base, self.source_id),
                                "generator return value must be a number".to_owned(),
                            ));
                        }
                        let lowered = self.lower_expr(*value)?;
                        self.gen_complete(ctx, true, Some(lowered), span)?;
                    }
                    None => self.gen_complete(ctx, false, None, span)?,
                }
                Ok(())
            }
            ast::Stmt::Throw(throw) => {
                let current = ctx.current.expect("reachable statement has a state");
                let mut buffer = Vec::new();
                self.with_consumed_stmt_values(|this| this.lower_throw(throw, &mut buffer))?;
                ctx.states[current].extend(buffer);
                // Throwing aborts the whole VM; no suspension follows it.
                ctx.current = None;
                Ok(())
            }
            ast::Stmt::Empty(_) => Ok(()),
            ast::Stmt::Decl(_) => Err(unsupported(
                fallback_span,
                self.base,
                self.source_id,
                "declaration in generator body",
            )),
            other => Err(unsupported(
                other.span(),
                self.base,
                self.source_id,
                "statement in generator body",
            )),
        }
    }

    /// Expression statements: statement-position yields suspend; identifier
    /// updates route through the capture-safe expression path (the statement
    /// path reads the pre-capture slot on first touch); everything else uses
    /// the existing statement lowering unchanged.
    fn gen_expr_stmt(&mut self, expr: ast::Expr, ctx: &mut GenCtx) -> Result<(), ParseError> {
        match expr {
            ast::Expr::Yield(yield_expr) => {
                if yield_expr.delegate {
                    return Err(unsupported(
                        yield_expr.span,
                        self.base,
                        self.source_id,
                        "generator yield delegation (yield*)",
                    ));
                }
                let Some(arg) = yield_expr.arg else {
                    return Err(unsupported(
                        yield_expr.span,
                        self.base,
                        self.source_id,
                        "generator bare yield without a value",
                    ));
                };
                let span = yield_expr.span;
                let schema = self.expression_schema(&arg);
                if !schema.as_ref().is_some_and(Self::is_numeric_schema) {
                    return Err(ParseError::at_span(
                        byte_span(arg.span(), self.base, self.source_id),
                        "generator yield value must be a number".to_owned(),
                    ));
                }
                let lowered = self.lower_expr(*arg)?;
                self.gen_suspend(ctx, lowered, span)
            }
            ast::Expr::Update(update) if matches!(update.arg.as_ref(), ast::Expr::Ident(_)) => {
                let span = update.span;
                let line = self.gen_line(span);
                let schema = self
                    .expression_schema(&update.arg)
                    .filter(Self::is_numeric_schema)
                    .or(Some(TypeSchema::Number));
                let updated = self.lower_update_expr(update)?;
                let slot = self.alloc_hidden_local(span)?;
                self.gen_emit(
                    ctx,
                    Stmt::Let {
                        index: slot,
                        declared_schema: schema,
                        expr: updated,
                        line,
                    },
                );
                self.stmt_spans.push(StmtSpanSite {
                    line,
                    span: byte_span(span, self.base, self.source_id),
                });
                Ok(())
            }
            other => {
                let current = ctx.current.expect("reachable statement has a state");
                let mut buffer = Vec::new();
                self.with_consumed_stmt_values(|this| {
                    this.lower_assignment_statement(other, &mut buffer)
                })?;
                ctx.states[current].extend(buffer);
                Ok(())
            }
        }
    }

    /// Block with lexical scoping: names resolve per block (shadowing just
    /// works through fresh persistent slots); slots outlive the block in the
    /// constructor frame exactly like hoisted generator state.
    fn gen_block(
        &mut self,
        block_span: swc_common::Span,
        stmts: Vec<ast::Stmt>,
        ctx: &mut GenCtx,
        name: &str,
    ) -> Result<(), ParseError> {
        let parent_scope = self.scope_id;
        let previous_locals = self.locals.clone();
        let previous_local_types = self.local_types.clone();
        let previous_mutable = self.mutable_locals.clone();
        let scope_id = self.next_scope_id;
        self.next_scope_id = self.next_scope_id.checked_add(1).ok_or_else(|| {
            ParseError::at_span(
                byte_span(block_span, self.base, self.source_id),
                "TypeScript lexical scope count exceeded".to_owned(),
            )
        })?;
        self.scope_id = scope_id;
        let declaration_start = self.local_decls.len();
        let result = self.gen_seq(stmts, ctx, name);
        let declarations = self.local_decls[declaration_start..]
            .iter()
            .filter(|site| site.scope_id == scope_id)
            .map(|site| site.slot)
            .collect();
        self.function_scopes.push(ParsedLexicalScope {
            id: scope_id,
            parent: Some(parent_scope),
            range: byte_span(block_span, self.base, self.source_id),
            declarations,
            functions: Vec::new(),
        });
        self.scope_id = parent_scope;
        self.locals = previous_locals;
        self.local_types = previous_local_types;
        self.mutable_locals = previous_mutable;
        result
    }

    /// `let`/`const` with scalar initializers. Slots are constructor-owned
    /// (never closure-local, so they persist); the initializer runs in flow
    /// order at first/next arrival through the captured slot.
    fn gen_var_decl(&mut self, decl: ast::VarDecl, ctx: &mut GenCtx) -> Result<(), ParseError> {
        if decl.kind == ast::VarDeclKind::Var {
            return Err(unsupported(
                decl.span,
                self.base,
                self.source_id,
                "var declaration",
            ));
        }
        let mutable = decl.kind == ast::VarDeclKind::Let;
        let decl_span = byte_span(decl.span, self.base, self.source_id);
        let line = line_at(self.source, decl_span.lo);
        for declarator in decl.decls {
            let ast::Pat::Ident(binding) = declarator.name else {
                return Err(unsupported(
                    declarator.name.span(),
                    self.base,
                    self.source_id,
                    "binding pattern",
                ));
            };
            let binding_name = binding.id.sym.to_string();
            if binding_name.starts_with("__gen_") || binding_name.starts_with("__CompatGen") {
                return Err(ParseError::at_span(
                    byte_span(binding.id.span, self.base, self.source_id),
                    format!(
                        "generator body binding '{binding_name}' uses a reserved generator state prefix"
                    ),
                ));
            }
            let duplicate = self
                .local_decls
                .iter()
                .any(|site| site.scope_id == self.scope_id && site.name == binding_name);
            if duplicate {
                return Err(ParseError::at_span(
                    byte_span(binding.id.span, self.base, self.source_id),
                    format!("duplicate TypeScript binding '{binding_name}'"),
                ));
            }
            let Some(init) = declarator.init else {
                return Err(unsupported(
                    declarator.span,
                    self.base,
                    self.source_id,
                    "uninitialized declaration",
                ));
            };
            let declared_schema = binding
                .type_ann
                .as_ref()
                .map(|ann| self.lower_type(&ann.type_ann))
                .transpose()?;
            let inferred_schema = self.expression_schema(&init);
            // Admitted body scalars only: numbers, booleans and explicit
            // null. Arrays, records, strings and callables would need a
            // different carrier/closure discipline; they fail here, before
            // execution, instead of silently degrading.
            let local_type = declared_schema.clone().or(inferred_schema.clone());
            let scalar = local_type.clone().filter(|schema| {
                matches!(
                    schema,
                    TypeSchema::Int
                        | TypeSchema::Float
                        | TypeSchema::Number
                        | TypeSchema::Bool
                        | TypeSchema::Null
                )
            });
            let Some(scalar) = scalar else {
                return Err(ParseError::at_span(
                    byte_span(binding.id.span, self.base, self.source_id),
                    format!(
                        "generator body binding '{binding_name}' must be a number, boolean or null"
                    ),
                ));
            };
            let callable_origin = (!mutable)
                .then(|| self.immutable_guest_callable_origin(&init))
                .flatten();
            // Arrow initializers reserve their slot first for recursion; the
            // reservation is constructor-owned here (never closure-local).
            let source_slot = self.next_slot;
            self.next_slot = self.next_slot.checked_add(1).ok_or_else(|| {
                ParseError::at_span(
                    byte_span(binding.id.span, self.base, self.source_id),
                    "TypeScript local slot limit exceeded".to_owned(),
                )
            })?;
            // Dummy constructor-frame initializer for availability: never
            // observed, since every read follows the flow-ordered captured
            // initialization below.
            ctx.dummies.push((source_slot, scalar.clone()));
            self.locals.insert(binding_name.clone(), source_slot);
            self.local_types
                .insert(binding_name.clone(), scalar.clone());
            if mutable {
                self.mutable_locals.insert(source_slot);
            }
            if let Some(origin) = callable_origin {
                self.const_callable_origins.insert(source_slot, origin);
            }
            let captured = self.capture_slot(source_slot, &binding_name, binding.id.span)?;
            let lowered = self.lower_expr(*init)?;
            self.gen_emit(
                ctx,
                Stmt::Let {
                    index: captured,
                    declared_schema: Some(scalar.clone()),
                    expr: lowered,
                    line,
                },
            );
            self.local_bindings
                .push((binding_name.clone(), source_slot));
            let node_id = self.alloc_node_id();
            let decl_order = u32::try_from(
                self.local_decls
                    .iter()
                    .filter(|site| site.scope_id == self.scope_id)
                    .count(),
            )
            .unwrap_or(u32::MAX);
            self.local_decls.push(LocalDeclSite {
                id: node_id,
                ident_span: byte_span(binding.id.span, self.base, self.source_id),
                stmt_span: decl_span,
                slot: source_slot,
                name: binding_name,
                scope_id: self.scope_id,
                decl_order,
            });
            self.stmt_spans.push(StmtSpanSite {
                line,
                span: decl_span,
            });
        }
        Ok(())
    }

    fn gen_if(
        &mut self,
        if_stmt: ast::IfStmt,
        ctx: &mut GenCtx,
        name: &str,
    ) -> Result<(), ParseError> {
        let span = if_stmt.span;
        let line = self.gen_line(span);
        let condition = self.lower_expr(*if_stmt.test)?;
        let then_state = self.gen_new_state(ctx);
        let join_state = self.gen_new_state(ctx);
        let has_else = if_stmt.alt.is_some();
        let else_state = if has_else {
            self.gen_new_state(ctx)
        } else {
            join_state
        };
        // Dispatch out of the entry state into the branch states.
        self.gen_emit(
            ctx,
            Stmt::IfElse {
                condition,
                then_branch: vec![Stmt::Assign {
                    kind: AssignmentKind::Set,
                    index: ctx.pc,
                    expr: Expr::Int(then_state as i64),
                    line,
                }],
                else_branch: vec![Stmt::Assign {
                    kind: AssignmentKind::Set,
                    index: ctx.pc,
                    expr: Expr::Int(else_state as i64),
                    line,
                }],
                line,
            },
        );
        self.stmt_spans.push(StmtSpanSite {
            line,
            span: byte_span(span, self.base, self.source_id),
        });
        // Then branch falls through to the join on completion.
        ctx.current = Some(then_state);
        self.gen_stmt(*if_stmt.cons, ctx, name)?;
        let then_open = ctx.current.is_some();
        if then_open {
            self.gen_goto(ctx, join_state, span);
        }
        let mut reachable = then_open || !has_else;
        if let Some(alt) = if_stmt.alt {
            ctx.current = Some(else_state);
            self.gen_stmt(*alt, ctx, name)?;
            let else_open = ctx.current.is_some();
            if else_open {
                self.gen_goto(ctx, join_state, span);
            }
            reachable = then_open || else_open;
        }
        ctx.current = reachable.then_some(join_state);
        Ok(())
    }

    fn gen_while(
        &mut self,
        while_stmt: ast::WhileStmt,
        ctx: &mut GenCtx,
        name: &str,
    ) -> Result<(), ParseError> {
        let span = while_stmt.span;
        let line = self.gen_line(span);
        let head = self.gen_new_state(ctx);
        let body_state = self.gen_new_state(ctx);
        let after = self.gen_new_state(ctx);
        self.gen_goto(ctx, head, span);
        // Loop head: unconditional loops jump straight to the body; general
        // conditions dispatch between body and exit.
        ctx.current = Some(head);
        if matches!(
            while_stmt.test.as_ref(),
            ast::Expr::Lit(ast::Lit::Bool(value)) if value.value
        ) {
            self.gen_goto(ctx, body_state, span);
        } else {
            let condition = self.lower_expr(*while_stmt.test)?;
            self.gen_emit(
                ctx,
                Stmt::IfElse {
                    condition,
                    then_branch: vec![Stmt::Assign {
                        kind: AssignmentKind::Set,
                        index: ctx.pc,
                        expr: Expr::Int(body_state as i64),
                        line,
                    }],
                    else_branch: vec![Stmt::Assign {
                        kind: AssignmentKind::Set,
                        index: ctx.pc,
                        expr: Expr::Int(after as i64),
                        line,
                    }],
                    line,
                },
            );
            self.stmt_spans.push(StmtSpanSite {
                line,
                span: byte_span(span, self.base, self.source_id),
            });
        }
        ctx.loops.push(LoopFrame {
            head,
            after,
            update: None,
        });
        ctx.current = Some(body_state);
        // A block body scopes its bindings; a single-statement body lowers
        // in place, mirroring ordinary lowering.
        match *while_stmt.body {
            ast::Stmt::Block(block) => self.gen_block(block.span, block.stmts, ctx, name)?,
            other => self.gen_stmt(other, ctx, name)?,
        }
        ctx.loops.pop();
        if ctx.current.is_some() {
            self.gen_goto(ctx, head, span);
        }
        ctx.current = Some(after);
        Ok(())
    }

    fn gen_for(
        &mut self,
        for_stmt: ast::ForStmt,
        ctx: &mut GenCtx,
        name: &str,
    ) -> Result<(), ParseError> {
        let span = for_stmt.span;
        let line = self.gen_line(span);
        // One loop scope covers init, test, update and body, mirroring
        // ordinary lowering.
        let parent_scope = self.scope_id;
        let previous_locals = self.locals.clone();
        let previous_local_types = self.local_types.clone();
        let previous_mutable = self.mutable_locals.clone();
        let scope_id = self.next_scope_id;
        self.next_scope_id = self.next_scope_id.checked_add(1).ok_or_else(|| {
            ParseError::at_span(
                byte_span(span, self.base, self.source_id),
                "TypeScript lexical scope count exceeded".to_owned(),
            )
        })?;
        self.scope_id = scope_id;
        let declaration_start = self.local_decls.len();
        let lowered = self.gen_for_inner(for_stmt, ctx, name, line);
        let declarations = self.local_decls[declaration_start..]
            .iter()
            .filter(|site| site.scope_id == scope_id)
            .map(|site| site.slot)
            .collect();
        let loop_span = byte_span(span, self.base, self.source_id);
        self.function_scopes.push(ParsedLexicalScope {
            id: scope_id,
            parent: Some(parent_scope),
            range: loop_span,
            declarations,
            functions: Vec::new(),
        });
        self.scope_id = parent_scope;
        self.locals = previous_locals;
        self.local_types = previous_local_types;
        self.mutable_locals = previous_mutable;
        self.stmt_spans.push(StmtSpanSite {
            line,
            span: loop_span,
        });
        lowered
    }

    fn gen_for_inner(
        &mut self,
        for_stmt: ast::ForStmt,
        ctx: &mut GenCtx,
        name: &str,
        line: u32,
    ) -> Result<(), ParseError> {
        let span = for_stmt.span;
        // Initializer runs once in the entry state on flow arrival.
        match for_stmt.init {
            None => {}
            Some(ast::VarDeclOrExpr::VarDecl(decl)) => {
                if decl.decls.len() != 1 {
                    return Err(unsupported(
                        span,
                        self.base,
                        self.source_id,
                        "multiple for-loop initializers in generator body",
                    ));
                }
                self.gen_var_decl(*decl, ctx)?;
            }
            Some(ast::VarDeclOrExpr::Expr(expr)) => {
                self.gen_expr_stmt(*expr, ctx)?;
            }
        }
        let head = self.gen_new_state(ctx);
        let body_state = self.gen_new_state(ctx);
        let after = self.gen_new_state(ctx);
        // The update (when present) runs at the end of each iteration in its
        // own state so `continue` reaches it; without an update, `continue`
        // targets the head directly.
        let has_update = for_stmt.update.is_some();
        let update_state = if has_update {
            Some(self.gen_new_state(ctx))
        } else {
            None
        };
        self.gen_goto(ctx, head, span);
        ctx.current = Some(head);
        match for_stmt.test {
            None => self.gen_goto(ctx, body_state, span),
            Some(test) => {
                let condition = self.lower_expr(*test)?;
                self.gen_emit(
                    ctx,
                    Stmt::IfElse {
                        condition,
                        then_branch: vec![Stmt::Assign {
                            kind: AssignmentKind::Set,
                            index: ctx.pc,
                            expr: Expr::Int(body_state as i64),
                            line,
                        }],
                        else_branch: vec![Stmt::Assign {
                            kind: AssignmentKind::Set,
                            index: ctx.pc,
                            expr: Expr::Int(after as i64),
                            line,
                        }],
                        line,
                    },
                );
            }
        }
        ctx.loops.push(LoopFrame {
            head,
            after,
            update: update_state,
        });
        ctx.current = Some(body_state);
        match *for_stmt.body {
            ast::Stmt::Block(block) => self.gen_block(block.span, block.stmts, ctx, name)?,
            other => self.gen_stmt(other, ctx, name)?,
        }
        ctx.loops.pop();
        if ctx.current.is_some() {
            match update_state {
                Some(update) => self.gen_goto(ctx, update, span),
                None => self.gen_goto(ctx, head, span),
            }
        }
        if let (Some(update_state), Some(update)) = (update_state, for_stmt.update) {
            ctx.current = Some(update_state);
            // Identifier updates run through the capture-safe expression
            // path; every other update keeps existing statement semantics.
            match *update {
                ast::Expr::Update(inner) if matches!(inner.arg.as_ref(), ast::Expr::Ident(_)) => {
                    let update_span = inner.span;
                    let update_line = self.gen_line(update_span);
                    let schema = self
                        .expression_schema(&inner.arg)
                        .filter(Self::is_numeric_schema)
                        .or(Some(TypeSchema::Number));
                    let updated = self.lower_update_expr(inner)?;
                    let slot = self.alloc_hidden_local(update_span)?;
                    self.gen_emit(
                        ctx,
                        Stmt::Let {
                            index: slot,
                            declared_schema: schema,
                            expr: updated,
                            line: update_line,
                        },
                    );
                }
                other => self.gen_expr_stmt(other, ctx)?,
            }
            // The update state always falls through to the head: it is only
            // entered via goto and contains no suspension (updates cannot
            // yield; any suspension inside would have split states above,
            // which expression updates cannot produce).
            if ctx.current.is_some() {
                self.gen_goto(ctx, head, span);
            } else {
                // A `throw` inside the update terminates the iteration.
                ctx.current = Some(head);
            }
        }
        ctx.current = Some(after);
        Ok(())
    }

    fn gen_break_continue(
        &mut self,
        span: swc_common::Span,
        is_break: bool,
        ctx: &mut GenCtx,
    ) -> Result<(), ParseError> {
        // Labels are rejected: only the innermost loop target exists here.
        // (The AST shape carries labels on BreakStmt/ContinueStmt.)
        let frame = ctx.loops.last().ok_or_else(|| {
            ParseError::at_span(
                byte_span(span, self.base, self.source_id),
                if is_break {
                    "generator break outside a loop is not supported".to_owned()
                } else {
                    "generator continue outside a loop is not supported".to_owned()
                },
            )
        })?;
        let target = if is_break {
            frame.after
        } else {
            frame.update.unwrap_or(frame.head)
        };
        self.gen_goto(ctx, target, span);
        self.stmt_spans.push(StmtSpanSite {
            line: self.gen_line(span),
            span: byte_span(span, self.base, self.source_id),
        });
        // Control leaves the state: like return, nothing after a break or
        // continue in the same straight-line prefix is reachable.
        ctx.current = None;
        Ok(())
    }
}

/// Collect every `yield <expr>` value schema for the honest result
/// representation. Runs after parameters are registered: a scoped pre-scan
/// registers each body-local scalar schema in walk order (shadowing through
/// save/restore frames, no slots emitted), so yields over body bindings
/// resolve exactly as they will during lowering. Non-numeric or
/// uninferable yields fail here, before any execution. Nested closures
/// never contribute.
fn gen_prescan_yield_schemas(
    lowerer: &mut Lowerer<'_>,
    stmts: &[ast::Stmt],
    joined: &mut Option<TypeSchema>,
) -> Result<(), ParseError> {
    let saved_locals = lowerer.locals.clone();
    let saved_types = lowerer.local_types.clone();
    let saved_mutable = lowerer.mutable_locals.clone();
    let result = prescan_seq(lowerer, stmts, joined);
    lowerer.locals = saved_locals;
    lowerer.local_types = saved_types;
    lowerer.mutable_locals = saved_mutable;
    result
}

fn prescan_seq(
    lowerer: &mut Lowerer<'_>,
    stmts: &[ast::Stmt],
    joined: &mut Option<TypeSchema>,
) -> Result<(), ParseError> {
    for stmt in stmts {
        prescan_stmt(lowerer, stmt, joined)?;
    }
    Ok(())
}

fn prescan_scoped(
    lowerer: &mut Lowerer<'_>,
    stmts: &[ast::Stmt],
    joined: &mut Option<TypeSchema>,
) -> Result<(), ParseError> {
    let saved_locals = lowerer.locals.clone();
    let saved_types = lowerer.local_types.clone();
    let saved_mutable = lowerer.mutable_locals.clone();
    let result = prescan_seq(lowerer, stmts, joined);
    lowerer.locals = saved_locals;
    lowerer.local_types = saved_types;
    lowerer.mutable_locals = saved_mutable;
    result
}

fn prescan_declarator(
    lowerer: &mut Lowerer<'_>,
    binding: &ast::BindingIdent,
    init: &Option<Box<ast::Expr>>,
) -> Result<(), ParseError> {
    let binding_name = binding.id.sym.to_string();
    let Some(init) = init else {
        return Ok(());
    };
    let declared = binding
        .type_ann
        .as_ref()
        .map(|ann| lowerer.lower_type(&ann.type_ann))
        .transpose()?;
    let inferred = lowerer.expression_schema(init);
    let local_type = declared.or(inferred);
    let scalar = local_type.clone().filter(|schema| {
        matches!(
            schema,
            TypeSchema::Int
                | TypeSchema::Float
                | TypeSchema::Number
                | TypeSchema::Bool
                | TypeSchema::Null
        )
    });
    let Some(scalar) = scalar else {
        return Err(ParseError::at_span(
            byte_span(binding.id.span, lowerer.base, lowerer.source_id),
            format!("generator body binding '{binding_name}' must be a number, boolean or null"),
        ));
    };
    lowerer.local_types.insert(binding_name, scalar);
    Ok(())
}

fn prescan_stmt(
    lowerer: &mut Lowerer<'_>,
    stmt: &ast::Stmt,
    joined: &mut Option<TypeSchema>,
) -> Result<(), ParseError> {
    match stmt {
        ast::Stmt::Expr(expr) => {
            if let ast::Expr::Yield(yield_expr) = expr.expr.as_ref()
                && let Some(arg) = yield_expr.arg.as_ref()
            {
                match lowerer.expression_schema(arg) {
                    Some(schema) if Lowerer::is_numeric_schema(&schema) => {
                        *joined = Some(match joined.take() {
                            None => schema,
                            Some(previous) if previous == schema => schema,
                            _ => TypeSchema::Number,
                        });
                    }
                    _ => {
                        return Err(ParseError::at_span(
                            byte_span(arg.span(), lowerer.base, lowerer.source_id),
                            "generator yield value must be a number".to_owned(),
                        ));
                    }
                }
            }
            Ok(())
        }
        ast::Stmt::Decl(ast::Decl::Var(decl)) => {
            for declarator in &decl.decls {
                if let ast::Pat::Ident(binding) = &declarator.name {
                    prescan_declarator(lowerer, binding, &declarator.init)?;
                }
            }
            Ok(())
        }
        ast::Stmt::Block(block) => prescan_scoped(lowerer, &block.stmts, joined),
        ast::Stmt::If(if_stmt) => {
            prescan_stmt(lowerer, &if_stmt.cons, joined)?;
            if let Some(alt) = &if_stmt.alt {
                prescan_stmt(lowerer, alt, joined)?;
            }
            Ok(())
        }
        ast::Stmt::While(while_stmt) => prescan_stmt(lowerer, &while_stmt.body, joined),
        ast::Stmt::For(for_stmt) => {
            // Loop scope covers init, body and update, mirroring lowering.
            let saved_locals = lowerer.locals.clone();
            let saved_types = lowerer.local_types.clone();
            let saved_mutable = lowerer.mutable_locals.clone();
            let result = (|| -> Result<(), ParseError> {
                if let Some(ast::VarDeclOrExpr::VarDecl(decl)) = &for_stmt.init {
                    for declarator in &decl.decls {
                        if let ast::Pat::Ident(binding) = &declarator.name {
                            prescan_declarator(lowerer, binding, &declarator.init)?;
                        }
                    }
                }
                prescan_stmt(lowerer, &for_stmt.body, joined)
            })();
            lowerer.locals = saved_locals;
            lowerer.local_types = saved_types;
            lowerer.mutable_locals = saved_mutable;
            result
        }
        _ => Ok(()),
    }
}
