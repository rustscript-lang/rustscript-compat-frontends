//! A narrow primitive JS AST to existing typed IR bridge with one realm heap.
//!
//! Heap-free primitive sources keep the original independent path. When the
//! actual parsed AST requires array or string storage, the lowerer
//! bootstraps one real B1 realm (fixed accepted prelude, single owning
//! carrier with headers) and lowers literal/alias/read/mutate through the
//! same carrier. Allocation bases come from a private static guest function
//! that reads the actual backing length at runtime, so repeated executions
//! of the same emission site observe fresh extents on every engine. No host
//! evaluator, no source substring matching.
use super::representation as value;
use super::string_literal::{
    MAX_LITERAL_UTF16_UNITS, StringStrictness, decode_string_literal, narrowed_span,
};
use std::collections::HashMap;
use swc_common::Spanned;
use swc_ecma_ast as ast;
use vm::compiler::ir::{FunctionDecl, FunctionImpl};
use vm::{
    BuiltinFunction, Expr, FrontendIr, LocalIrBuilder, ParseError, SourceFrontendContext, Stmt,
    TypeSchema, ValueType,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Undefined,
    Null,
    Boolean,
    Number,
    String,
    Array,
    Object,
    Function,
}

/// Private realm header layout: [object-kind, generation, logical-length]
/// followed by numeric payload. Constants are generated metadata, never
/// user-accessible names.
const REALM_HEADER_CELLS: i64 = 3;
const REALM_OBJECT_KIND: i64 = 1;
const REALM_GENERATION: i64 = 1;
/// String heap layout mirrors the numeric layout: [kind 2, generation 1,
/// UTF-16 length, code units...]. Units are exact u16 values in numeric
/// cells; logical IDs stay Int, never resource handles.
const REALM_STRING_KIND: i64 = 2;
const REALM_STRING_HEADER_CELLS: i64 = 3;
/// Plain-object heap layout (kind 3): `[3, gen 1, property_count]` then
/// seven cells per own data property:
/// `[key_ref, key_gen, value_tag, value_number, value_flag01, value_ref,
/// value_gen]`. Keys are real UTF-16 string nodes compared by content;
/// values are complete tagged payloads.
const REALM_PLAIN_OBJECT_KIND: i64 = 3;
const REALM_OBJECT_HEADER_CELLS: i64 = 3;
const REALM_OBJECT_CELLS_PER_PROPERTY: i64 = 7;
/// Function heap layout (kind 4):
/// `[4, gen 1, 0, code_id, closed_env 0, strict 1]`. The code ID is a
/// finite compile-time identity into the static source-function table;
/// every INSTANCE base comes from the runtime allocator.
const REALM_FUNCTION_KIND: i64 = 4;
const REALM_FUNCTION_CELLS: i64 = 6;
/// Bounded object/function admission: at most this many own properties per
/// literal and this many source functions per compilation unit.
const MAX_OBJECT_PROPERTIES: usize = 256;
const MAX_SOURCE_FUNCTIONS: usize = 256;
/// Private runtime allocation helper appended to realm IR.
const REALM_ALLOC_HELPER: &str = "__compat_es5_alloc";
/// Private static guest string-equality helper appended to realm IR.
const STRING_EQUALITY_HELPER: &str = "__compat_es5_string_equal";
/// Private exact-Int decode helper `(Number) -> Int` factored from the
/// proven allocator bit logic. Converts bounded guest metadata cells
/// (tags, bases, generations, code IDs) without invented casts.
const DECODE_INT_HELPER: &str = "__compat_es5_decode_int";
/// Private operational GetValue helper `(Carrier, Reference) -> JsValue`.
/// Reads the actual own data property through the real heap.
const REF_GET_HELPER: &str = "__compat_es5_ref_get";
/// Private finite dispatch helper
/// `(Carrier, func: JsValue, this: JsValue, args: Array<JsValue>) ->
/// Completion`. Decodes the function heap code ID and calls the
/// statically compiled source function; never interprets AST.
const DISPATCH_HELPER: &str = "__compat_es5_dispatch";
/// Bounded initialization: total header+payload cells reserved across the
/// whole source before large IR emission, for strings and numeric arrays.
const MAX_REALM_INITIAL_CELLS: u64 = 65536;

struct Heap {
    carrier: u16,
    helper_read: u16,
    helper_write: u16,
    /// Private runtime allocation function index. Every literal calls it
    /// with the owning carrier and its cell count; bases are fresh extents.
    alloc: u16,
}

struct Lowered {
    expr: Expr,
    kind: Kind,
}

/// One admitted closed strict source function in code-ID order. The AST
/// is cloned so predeclaration can lower bodies after helper indices are
/// reserved; the code ID is the finite static dispatch identity.
#[derive(Clone)]
struct CollectedFunction {
    code_id: i64,
    function: Box<ast::Function>,
    span: swc_common::Span,
    strict: bool,
}

struct Lowerer<'a> {
    source: &'a str,
    source_id: u32,
    base: u32,
    next_slot: u16,
    bindings: HashMap<String, (u16, Kind)>,
    array_lengths: HashMap<String, usize>,
    source_bindings: Vec<(String, u16)>,
    heap: Option<Heap>,
    /// Total header+payload cells reserved by literals so far. This static
    /// running total remains a whole-source budget cap: overflow/excess
    /// rejects before large emission. It never supplies runtime IDs; every
    /// literal base comes from the runtime allocator reading live backing
    /// length instead.
    init_cells: u64,
    /// Flat index of the appended guest string-equality helper. Always set
    /// on the realm path; absent on the heap-free scalar path, where string
    /// values cannot occur.
    string_eq: Option<u16>,
    /// Caller-code strictness for the current lowering scope (script
    /// prologue or the enclosing admitted strict function). Recorded on
    /// every materialized Reference; never the callee flag.
    enclosing_strict: bool,
    /// `this` parameter slot when lowering inside a source function body.
    function_this: Option<u16>,
    /// Active carrier override inside function bodies (the carrier
    /// parameter slot). When `None`, the root realm carrier is used.
    carrier_override: Option<u16>,
    /// Flat indices of the private object/function helpers. Set on the
    /// realm path once appended; used by inline object/call lowering and
    /// by nested function bodies.
    helper_decode: Option<u16>,
    helper_ref_get: Option<u16>,
    helper_dispatch: Option<u16>,
    /// Finite static code IDs for every admitted source-function AST node,
    /// keyed by absolute span `(lo, hi)`. Never source-text recognition.
    function_code_ids: HashMap<(u32, u32), i64>,
    /// Collected admitted functions in code-ID order for predeclaration.
    collected_functions: Vec<CollectedFunction>,
}

impl Lowerer<'_> {
    fn line(&self, span: swc_common::Span) -> u32 {
        let offset = span.lo.0.saturating_sub(self.base) as usize;
        u32::try_from(
            self.source[..offset.min(self.source.len())]
                .matches('\n')
                .count()
                + 1,
        )
        .unwrap_or(u32::MAX)
    }

    fn error(&self, span: swc_common::Span, message: &str) -> ParseError {
        let lo = span.lo.0.saturating_sub(self.base) as usize;
        let hi = span.hi.0.saturating_sub(self.base) as usize;
        ParseError::at_span(
            vm::compiler::source_map::Span::new(self.source_id, lo, hi),
            message,
        )
    }

    fn slot(&mut self, span: swc_common::Span) -> Result<u16, ParseError> {
        let slot = self.next_slot;
        self.next_slot = slot
            .checked_add(1)
            .ok_or_else(|| self.error(span, "JavaScript ES5 local slot limit exceeded"))?;
        Ok(slot)
    }

    fn bind_temp(&mut self, expr: Expr, span: swc_common::Span) -> Result<(u16, Stmt), ParseError> {
        self.bind_typed(expr, value::schema(), span)
    }

    fn bind_typed(
        &mut self,
        expr: Expr,
        schema: TypeSchema,
        span: swc_common::Span,
    ) -> Result<(u16, Stmt), ParseError> {
        let slot = self.slot(span)?;
        Ok((
            slot,
            Stmt::Let {
                index: slot,
                declared_schema: Some(schema),
                expr,
                line: self.line(span),
            },
        ))
    }

    fn builtin_call(index: u16, args: Vec<Expr>) -> Expr {
        Expr::Call(index, Vec::new(), args, None, None)
    }

    fn guest_call(index: u16, args: Vec<Expr>) -> Expr {
        Expr::Call(index, Vec::new(), args, None, None)
    }

    fn as_number(expr: Expr) -> Expr {
        Expr::Div(Box::new(expr), Box::new(Expr::Float(1.0)))
    }

    fn heap_ref(&self, span: swc_common::Span) -> Result<&Heap, ParseError> {
        self.heap
            .as_ref()
            .ok_or_else(|| self.error(span, "JavaScript ES5 numeric arrays require the realm heap"))
    }

    /// Active owning carrier slot: the function-body parameter inside
    /// source functions, otherwise the root realm carrier.
    fn active_carrier(&self, span: swc_common::Span) -> Result<u16, ParseError> {
        if let Some(carrier) = self.carrier_override {
            return Ok(carrier);
        }
        Ok(self.heap_ref(span)?.carrier)
    }

    /// Exact raw `"use strict"` directive check over the literal lexeme.
    /// Only the two exact spellings admit; escaped lookalikes
    /// (`"\u0075se strict"`, `"\\u2028"` text, braced escapes) reject
    /// because their raw bytes differ.
    fn is_exact_use_strict_lexeme(&self, span: swc_common::Span) -> bool {
        let lo = span.lo.0.saturating_sub(self.base) as usize;
        let hi = span.hi.0.saturating_sub(self.base) as usize;
        let Some(lexeme) = self.source.get(lo..hi) else {
            return false;
        };
        lexeme == "\"use strict\"" || lexeme == "'use strict'"
    }

    /// Whether a statement is an exact expression-statement string
    /// directive with the given literal span.
    fn directive_literal_span(statement: &ast::Stmt) -> Option<swc_common::Span> {
        let ast::Stmt::Expr(e) = statement else {
            return None;
        };
        // Unwrap redundant parentheses: `("use strict");` is still a
        // directive in sloppy prose, but P0 only admits the exact bare
        // form to keep the raw-prologue check tight; parenthesized
        // directives fall through to the unsupported-expression path.
        match e.expr.as_ref() {
            ast::Expr::Lit(ast::Lit::Str(s)) => Some(s.span),
            _ => None,
        }
    }

    /// Script-level strictness from the exact raw directive prologue.
    fn script_strict(&self, script: &ast::Script) -> bool {
        for statement in &script.body {
            let Some(lit_span) = Self::directive_literal_span(statement) else {
                return false;
            };
            if self.is_exact_use_strict_lexeme(lit_span) {
                return true;
            }
        }
        false
    }

    /// Function-body strictness: own exact prologue or inherited script
    /// strictness. Raw spelling is checked, never decoded content.
    fn function_own_strict(&self, body: &ast::FunctionBody) -> bool {
        for statement in &body.stmts {
            let Some(lit_span) = Self::directive_literal_span(statement) else {
                return false;
            };
            if self.is_exact_use_strict_lexeme(lit_span) {
                return true;
            }
        }
        false
    }

    fn literal_index(value: f64) -> Option<i64> {
        if value.is_finite() && value.fract() == 0.0 && value >= 0.0 && value <= i64::MAX as f64 {
            Some(value as i64)
        } else {
            None
        }
    }

    fn member_index_literal(property: &ast::Expr) -> Option<i64> {
        match property {
            ast::Expr::Lit(ast::Lit::Num(n)) => Self::literal_index(n.value),
            ast::Expr::Paren(p) => Self::member_index_literal(&p.expr),
            _ => None,
        }
    }

    fn expr(&mut self, expression: &ast::Expr) -> Result<Lowered, ParseError> {
        match expression {
            ast::Expr::Lit(ast::Lit::Num(n)) => Ok(Lowered {
                expr: value::number(Expr::Float(n.value)),
                kind: Kind::Number,
            }),
            ast::Expr::Lit(ast::Lit::Null(_)) => Ok(Lowered {
                expr: value::null(),
                kind: Kind::Null,
            }),
            ast::Expr::Lit(ast::Lit::Bool(b)) => Ok(Lowered {
                expr: value::boolean(Expr::Bool(b.value)),
                kind: Kind::Boolean,
            }),
            ast::Expr::Lit(ast::Lit::Str(s)) => self.lower_string_literal(s),
            ast::Expr::Paren(p) => self.expr(&p.expr),
            ast::Expr::Ident(i) => {
                if let Some(&(slot, kind)) = self.bindings.get(i.sym.as_str()) {
                    Ok(Lowered {
                        expr: Expr::Var(slot),
                        kind,
                    })
                } else if i.sym.as_str() == "undefined" {
                    Ok(Lowered {
                        expr: value::undefined(),
                        kind: Kind::Undefined,
                    })
                } else {
                    Err(self.error(i.span, &format!("unknown local '{}'", i.sym)))
                }
            }
            ast::Expr::Unary(u) if u.op == ast::UnaryOp::Minus => {
                let operand = self.expr(&u.arg)?;
                if operand.kind != Kind::Number {
                    return Err(
                        self.error(u.span, "P0 arithmetic currently requires numeric operands")
                    );
                }
                let (slot, statement) = self.bind_temp(operand.expr, u.span)?;
                Ok(Lowered {
                    expr: Expr::Block {
                        stmts: vec![statement],
                        expr: Box::new(value::number(Expr::Neg(Box::new(value::field(
                            slot, "number",
                        ))))),
                    },
                    kind: Kind::Number,
                })
            }
            ast::Expr::Bin(b) => {
                if !matches!(
                    b.op,
                    ast::BinaryOp::Add
                        | ast::BinaryOp::Sub
                        | ast::BinaryOp::Mul
                        | ast::BinaryOp::Div
                        | ast::BinaryOp::EqEqEq
                        | ast::BinaryOp::NotEqEq
                ) {
                    return Err(self.error(b.span, "unsupported JavaScript ES5 P0 binary operator"));
                }
                let left = self.expr(&b.left)?;
                let right = self.expr(&b.right)?;
                let equality = matches!(b.op, ast::BinaryOp::EqEqEq | ast::BinaryOp::NotEqEq);
                if !equality && (left.kind != Kind::Number || right.kind != Kind::Number) {
                    return Err(
                        self.error(b.span, "P0 arithmetic currently requires numeric operands")
                    );
                }
                let (l, ls) = self.bind_temp(left.expr, b.left.span())?;
                let (r, rs) = self.bind_temp(right.expr, b.right.span())?;
                let result = if equality {
                    let carrier = self.active_carrier(b.span).ok();
                    let strings = match (carrier, self.string_eq) {
                        (Some(carrier), Some(index)) => Some(value::StringEquality {
                            carrier: Expr::Var(carrier),
                            index,
                        }),
                        // The heap-free scalar path admits no string values,
                        // so the comparator is unreachable there.
                        _ => None,
                    };
                    let equal = value::strict_equal(l, r, strings);
                    if b.op == ast::BinaryOp::NotEqEq {
                        // Recompute the unboxed flag for inversion so native
                        // branches on a Bool temporary, never on a Tagged Get.
                        let flag_strings = match (self.active_carrier(b.span).ok(), self.string_eq)
                        {
                            (Some(carrier), Some(index)) => Some(value::StringEquality {
                                carrier: Expr::Var(carrier),
                                index,
                            }),
                            _ => None,
                        };
                        let (flag_slot, flag_stmt) = self.bind_typed(
                            value::strict_equal_flag(l, r, flag_strings),
                            TypeSchema::Bool,
                            b.span,
                        )?;
                        return Ok(Lowered {
                            expr: Expr::Block {
                                stmts: vec![ls, rs, flag_stmt],
                                expr: Box::new(value::boolean(Expr::Not(Box::new(Expr::Var(
                                    flag_slot,
                                ))))),
                            },
                            kind: Kind::Boolean,
                        });
                    }
                    equal
                } else {
                    let a = Box::new(value::field(l, "number"));
                    let c = Box::new(value::field(r, "number"));
                    let number = match b.op {
                        ast::BinaryOp::Add => Expr::Add(a, c),
                        ast::BinaryOp::Sub => Expr::Sub(a, c),
                        ast::BinaryOp::Mul => Expr::Mul(a, c),
                        ast::BinaryOp::Div => Expr::Div(a, c),
                        _ => unreachable!("operator checked above"),
                    };
                    value::number(number)
                };
                Ok(Lowered {
                    expr: Expr::Block {
                        stmts: vec![ls, rs],
                        expr: Box::new(result),
                    },
                    kind: if equality {
                        Kind::Boolean
                    } else {
                        Kind::Number
                    },
                })
            }
            ast::Expr::Member(m) => self.lower_member_read(m),
            ast::Expr::This(t) => self.lower_this(t.span),
            ast::Expr::Object(o) => self.lower_object_literal(o),
            ast::Expr::Fn(f) => self.lower_function_expr(f),
            ast::Expr::Call(c) => self.lower_call_expr(c),
            _ => Err(self.error(
                expression.span(),
                "unsupported JavaScript ES5 P0 primitive expression",
            )),
        }
    }

    fn lower_member_read(&mut self, member: &ast::MemberExpr) -> Result<Lowered, ParseError> {
        // Numeric-array fast path: computed integer literal on an array
        // binding preserves the accepted behavior exactly.
        if let ast::MemberProp::Computed(property) = &member.prop
            && let Some(index) = Self::member_index_literal(&property.expr)
            && let ast::Expr::Ident(object) = member.obj.as_ref()
            && let Some(&(slot, kind)) = self.bindings.get(object.sym.as_str())
            && kind == Kind::Array
        {
            let name = object.sym.as_str();
            let length = self.array_lengths.get(name).copied().unwrap_or(0);
            let position = usize::try_from(index).unwrap_or(usize::MAX);
            if position >= length {
                return Ok(Lowered {
                    expr: value::undefined(),
                    kind: Kind::Undefined,
                });
            }
            let carrier = self.active_carrier(member.span)?;
            let heap = self.heap_ref(member.span)?;
            let helper_read = heap.helper_read;
            let base = value::field(slot, "ref_slot");
            let physical = Expr::Add(
                Box::new(base),
                Box::new(Expr::Int(REALM_HEADER_CELLS + index)),
            );
            let read = Self::guest_call(
                helper_read,
                vec![Expr::Var(carrier), Self::as_number(physical)],
            );
            return Ok(Lowered {
                expr: value::number(Self::as_number(read)),
                kind: Kind::Number,
            });
        }
        // If this is a computed integer subscript but the base is not an
        // admitted array, keep the original precise array diagnostics when
        // the base is array-like; otherwise fall through to the object path
        // which gives the object-specific key diagnostic.
        if let ast::MemberProp::Computed(property) = &member.prop
            && Self::member_index_literal(&property.expr).is_some()
        {
            if let ast::Expr::Ident(object) = member.obj.as_ref()
                && let Some(&(_, kind)) = self.bindings.get(object.sym.as_str())
                && kind == Kind::Array
            {
                // Handled above; unreachable, kept for clarity.
            } else if let ast::Expr::Ident(object) = member.obj.as_ref()
                && self.bindings.contains_key(object.sym.as_str())
            {
                // Known non-array binding with an integer subscript: this is
                // an array-shape misuse, not an object key.
                return Err(self.error(
                    member.span,
                    "P0 indexed reads require a numeric-array binding",
                ));
            }
            // Otherwise (unknown base, `this`, nested member) let the object
            // path produce its key diagnostic below.
            if member_key_is_integer_literal(&property.expr) {
                // Integer keys are not plain-object keys in this P0 slice.
                return Err(self.error(
                    property.span(),
                    "P0 plain-object member reads admit only identifier and string-literal keys",
                ));
            }
        }
        self.lower_object_member_read(member)
    }

    /// Operational own-data-property read through a real Reference:
    /// evaluate base once, allocate the UTF-16 key once, materialize
    /// `{kind, base, key_slot, strict}` with caller strictness, then
    /// GetValue through the actual heap object.
    fn lower_object_member_read(
        &mut self,
        member: &ast::MemberExpr,
    ) -> Result<Lowered, ParseError> {
        let ref_get = self.helper_ref_get.ok_or_else(|| {
            self.error(
                member.span,
                "P0 plain-object reads require the realm reference helper",
            )
        })?;
        let carrier = self.active_carrier(member.span)?;
        // Evaluate the receiver once (Ident, `this`, nested member, call
        // result or parenthesized form). Static admission requires an
        // object-like base; runtime guards handle the rest.
        let base = self.expr(&member.obj)?;
        match base.kind {
            Kind::Object | Kind::Function => {}
            _ => {
                return Err(self.error(
                    member.span,
                    "P0 plain-object member reads require an object receiver",
                ));
            }
        }
        let (base_slot, base_stmt) = self.bind_temp(base.expr, member.obj.span())?;
        let key_units = self.member_key_units(member)?;
        let mut stmts = vec![base_stmt];
        let key_base = self.emit_utf16_node(&key_units, member.span, &mut stmts)?;
        let strict = Expr::Bool(self.enclosing_strict);
        let reference = value::reference(Expr::Var(base_slot), Expr::Var(key_base), strict);
        let (ref_slot, ref_stmt) =
            self.bind_typed(reference, value::reference_schema(), member.span)?;
        stmts.push(ref_stmt);
        let got = Self::guest_call(ref_get, vec![Expr::Var(carrier), Expr::Var(ref_slot)]);
        let (value_slot, value_stmt) = self.bind_temp(got, member.span)?;
        stmts.push(value_stmt);
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(Expr::Var(value_slot)),
            },
            // Dynamic own-property value: statically object-like (tag 5
            // covers objects/functions; numbers/strings arrive with their
            // real runtime tags). Callers use runtime tags, never this Kind.
            kind: Kind::Object,
        })
    }

    /// Strip redundant parentheses without losing receiver information.
    /// Call lowering uses this to preserve `(obj.m)()` as a method call;
    /// value lowering of `Paren(Member)` still decays through GetValue.
    fn unwrap_parens(expression: &ast::Expr) -> &ast::Expr {
        let mut current = expression;
        while let ast::Expr::Paren(p) = current {
            current = &p.expr;
        }
        current
    }

    /// Lower a call expression: receiver-preserving member calls
    /// (`obj.m()`, `(obj.m)()`) and bare strict calls (`f()`, `(f)()`).
    /// Zero actual arguments only; the fixed physical ABI still passes a
    /// real empty `Array<JsValue>` plus mode 0. Results unwrap the actual
    /// Completion value.
    fn lower_call_expr(&mut self, call: &ast::CallExpr) -> Result<Lowered, ParseError> {
        if !call.args.is_empty() {
            return Err(self.error(
                call.span,
                "P0 source functions admit only zero-argument calls",
            ));
        }
        if call.type_args.is_some() {
            return Err(self.error(call.span, "P0 source functions reject type arguments"));
        }
        for arg in &call.args {
            if arg.spread.is_some() {
                return Err(self.error(call.span, "P0 source functions reject spread arguments"));
            }
        }
        let ast::Callee::Expr(callee) = &call.callee else {
            return Err(self.error(
                call.span,
                "P0 calls admit only identifier and member callees",
            ));
        };
        let unwrapped = Self::unwrap_parens(callee);
        if let ast::Expr::Member(member) = unwrapped {
            return self.lower_method_call(member, call.span);
        }
        if let ast::Expr::Ident(ident) = unwrapped {
            return self.lower_bare_call(ident, call.span);
        }
        Err(self.error(
            call.span,
            "P0 calls admit only identifier and member callees",
        ))
    }

    /// Receiver-preserving method call: the SAME saved Reference base is
    /// passed as `this`; base/key evaluate exactly once left-to-right.
    fn lower_method_call(
        &mut self,
        member: &ast::MemberExpr,
        span: swc_common::Span,
    ) -> Result<Lowered, ParseError> {
        let ref_get = self.helper_ref_get.ok_or_else(|| {
            self.error(span, "P0 method calls require the realm reference helper")
        })?;
        let dispatch = self
            .helper_dispatch
            .ok_or_else(|| self.error(span, "P0 method calls require the realm dispatch helper"))?;
        let carrier = self.active_carrier(span)?;
        let base = self.expr(&member.obj)?;
        match base.kind {
            Kind::Object | Kind::Function => {}
            _ => {
                return Err(self.error(member.span, "P0 method calls require an object receiver"));
            }
        }
        let (base_slot, base_stmt) = self.bind_temp(base.expr, member.obj.span())?;
        let key_units = self.member_key_units(member)?;
        let mut stmts = vec![base_stmt];
        let key_base = self.emit_utf16_node(&key_units, member.span, &mut stmts)?;
        let strict = Expr::Bool(self.enclosing_strict);
        let reference = value::reference(Expr::Var(base_slot), Expr::Var(key_base), strict);
        let (ref_slot, ref_stmt) = self.bind_typed(reference, value::reference_schema(), span)?;
        stmts.push(ref_stmt);
        // GetValue through the SAME reference, then call with the SAME
        // saved base as receiver.
        let got = Self::guest_call(ref_get, vec![Expr::Var(carrier), Expr::Var(ref_slot)]);
        let (func_slot, func_stmt) = self.bind_temp(got, span)?;
        stmts.push(func_stmt);
        let receiver = value::field_of(Expr::Var(ref_slot), "base");
        let (this_slot, this_stmt) = self.bind_temp(receiver, span)?;
        stmts.push(this_stmt);
        let empty_args = Self::builtin_call(BuiltinFunction::ArrayNew.call_index(), Vec::new());
        let completed = Self::guest_call(
            dispatch,
            vec![
                Expr::Var(carrier),
                Expr::Var(func_slot),
                Expr::Var(this_slot),
                empty_args,
            ],
        );
        let (comp_slot, comp_stmt) =
            self.bind_typed(completed, value::completion_schema(), span)?;
        stmts.push(comp_stmt);
        let (value_slot, value_stmt) = self.bind_temp(value::field(comp_slot, "value"), span)?;
        stmts.push(value_stmt);
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(Expr::Var(value_slot)),
            },
            kind: Kind::Object,
        })
    }

    /// Bare strict call: the callee value was already detached from any
    /// property Reference (stored/aliased), so `this` is Undefined.
    fn lower_bare_call(
        &mut self,
        ident: &ast::Ident,
        span: swc_common::Span,
    ) -> Result<Lowered, ParseError> {
        let dispatch = self.helper_dispatch.ok_or_else(|| {
            self.error(span, "P0 function calls require the realm dispatch helper")
        })?;
        let carrier = self.active_carrier(span)?;
        let name = ident.sym.as_str();
        let Some(&(slot, kind)) = self.bindings.get(name) else {
            if name == "undefined" {
                return Err(self.error(
                    ident.span,
                    "P0 calls admit only identifier and member callees",
                ));
            }
            return Err(self.error(ident.span, &format!("unknown local '{name}'")));
        };
        match kind {
            Kind::Function | Kind::Object => {}
            _ => {
                return Err(self.error(ident.span, "P0 bare calls require a function binding"));
            }
        }
        let mut stmts = Vec::new();
        let (func_slot, func_stmt) = self.bind_temp(Expr::Var(slot), ident.span)?;
        stmts.push(func_stmt);
        let (this_slot, this_stmt) = self.bind_temp(value::undefined(), span)?;
        stmts.push(this_stmt);
        let empty_args = Self::builtin_call(BuiltinFunction::ArrayNew.call_index(), Vec::new());
        let completed = Self::guest_call(
            dispatch,
            vec![
                Expr::Var(carrier),
                Expr::Var(func_slot),
                Expr::Var(this_slot),
                empty_args,
            ],
        );
        let (comp_slot, comp_stmt) =
            self.bind_typed(completed, value::completion_schema(), span)?;
        stmts.push(comp_stmt);
        let (value_slot, value_stmt) = self.bind_temp(value::field(comp_slot, "value"), span)?;
        stmts.push(value_stmt);
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(Expr::Var(value_slot)),
            },
            kind: Kind::Object,
        })
    }

    /// Raw bytes of the user literal lexeme, sliced by its own span. Never
    /// generated prelude text or whole-source substring search.
    fn lexeme(&self, span: swc_common::Span) -> Result<&str, ParseError> {
        let lo = span.lo.0.saturating_sub(self.base) as usize;
        let hi = span.hi.0.saturating_sub(self.base) as usize;
        self.source
            .get(lo..hi)
            .ok_or_else(|| self.error(span, "JavaScript ES5 string literal span is out of range"))
    }

    /// Decode one string token through the shared original-source codec.
    /// Runs over the literal's own span bytes with the actual resolved
    /// strictness of this lowering scope (script prologue, or the enclosing
    /// admitted source function via save/restore) — never the physical
    /// typing policy. Lexical failures keep the codec message with the span
    /// narrowed to the offending escape; the unit bound stays enforced here
    /// before any node/IR emission.
    fn decode_string_token(&self, span: swc_common::Span) -> Result<Vec<u16>, ParseError> {
        let lexeme = self.lexeme(span)?;
        let policy = if self.enclosing_strict {
            StringStrictness::Strict
        } else {
            StringStrictness::Sloppy
        };
        decode_string_literal(lexeme, policy)
            .map_err(|codec| self.error(narrowed_span(span, codec.relative_offset), &codec.message))
    }

    /// Reserve one runtime allocation of `need` header+payload cells.
    /// Checked Rust arithmetic keeps the whole-source budget cap: the static
    /// running total rejects overflow/excess with a spanned resource
    /// diagnostic before large emission, but never supplies an ID. Emits a
    /// call to the private runtime allocator, which reads the live backing
    /// length, guards the extent and resizes before returning the fresh Int
    /// base. Repeated executions of this same call site observe distinct
    /// extents on every engine.
    fn reserve_allocation(
        &mut self,
        carrier: u16,
        alloc: u16,
        span: swc_common::Span,
        line: u32,
        statements: &mut Vec<Stmt>,
        need: u64,
    ) -> Result<u16, ParseError> {
        let end = self.init_cells.checked_add(need).ok_or_else(|| {
            self.error(
                span,
                "JavaScript ES5 realm initialization cell count overflow",
            )
        })?;
        if end > MAX_REALM_INITIAL_CELLS {
            return Err(self.error(
                span,
                "JavaScript ES5 realm initialization exceeds the 65536 cell budget",
            ));
        }
        self.init_cells = end;
        let count = i64::try_from(need)
            .map_err(|_| self.error(span, "JavaScript ES5 realm allocation count overflow"))?;
        let base_slot = self.slot(span)?;
        statements.push(Stmt::Let {
            index: base_slot,
            declared_schema: Some(TypeSchema::Int),
            expr: Self::guest_call(alloc, vec![Expr::Var(carrier), Expr::Int(count)]),
            line,
        });
        Ok(base_slot)
    }

    /// Lower a string literal to an immutable realm value. Units come from
    /// the shared original-source codec over the literal's own span bytes
    /// (never the parser cooked payload), capped by the codec at one unit
    /// past the bound so oversized literals reject before emission.
    /// The logical binding copies this value by alias without payload copy.
    fn lower_string_literal(&mut self, literal: &ast::Str) -> Result<Lowered, ParseError> {
        let span = literal.span;
        let units = self.decode_string_token(span)?;
        if units.len() > MAX_LITERAL_UTF16_UNITS {
            return Err(self.error(
                span,
                "JavaScript ES5 string literal exceeds the 16384 UTF-16 unit bound",
            ));
        }
        let length = units.len() as i64;
        let total = REALM_STRING_HEADER_CELLS
            .checked_add(length)
            .ok_or_else(|| self.error(span, "JavaScript ES5 string length overflow"))?;
        let need = u64::try_from(total)
            .map_err(|_| self.error(span, "JavaScript ES5 string length overflow"))?;
        let carrier = self.active_carrier(span)?;
        let heap = self.heap_ref(span)?;
        let (helper_write, alloc) = (heap.helper_write, heap.alloc);
        let line = self.line(span);
        let mut stmts = Vec::new();
        let base_slot = self.reserve_allocation(carrier, alloc, span, line, &mut stmts, need)?;
        for (offset, marker) in [
            (0i64, REALM_STRING_KIND),
            (1i64, REALM_GENERATION),
            (2i64, length),
        ] {
            let physical = Expr::Add(Box::new(Expr::Var(base_slot)), Box::new(Expr::Int(offset)));
            let written = Self::guest_call(
                helper_write,
                vec![
                    Expr::Var(carrier),
                    Self::as_number(physical),
                    Self::as_number(Expr::Int(marker)),
                ],
            );
            stmts.push(Stmt::Expr {
                expr: written,
                line,
            });
        }
        for (position, unit) in units.iter().enumerate() {
            let offset = REALM_STRING_HEADER_CELLS + position as i64;
            let physical = Expr::Add(Box::new(Expr::Var(base_slot)), Box::new(Expr::Int(offset)));
            let written = Self::guest_call(
                helper_write,
                vec![
                    Expr::Var(carrier),
                    Self::as_number(physical),
                    Self::as_number(Expr::Int(i64::from(*unit))),
                ],
            );
            stmts.push(Stmt::Expr {
                expr: written,
                line,
            });
        }
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(value::string(
                    Expr::Var(base_slot),
                    Expr::Int(REALM_GENERATION),
                )),
            },
            kind: Kind::String,
        })
    }

    /// Emit one UTF-16 string node (header + units) into `statements`,
    /// returning the fresh Int base slot. Shared by string values and by
    /// object/member key nodes so keys are real guest strings.
    fn emit_utf16_node(
        &mut self,
        units: &[u16],
        span: swc_common::Span,
        statements: &mut Vec<Stmt>,
    ) -> Result<u16, ParseError> {
        if units.len() > MAX_LITERAL_UTF16_UNITS {
            return Err(self.error(
                span,
                "JavaScript ES5 string literal exceeds the 16384 UTF-16 unit bound",
            ));
        }
        let length = units.len() as i64;
        let total = REALM_STRING_HEADER_CELLS
            .checked_add(length)
            .ok_or_else(|| self.error(span, "JavaScript ES5 string length overflow"))?;
        let need = u64::try_from(total)
            .map_err(|_| self.error(span, "JavaScript ES5 string length overflow"))?;
        let carrier = self.active_carrier(span)?;
        let heap = self.heap_ref(span)?;
        let (helper_write, alloc) = (heap.helper_write, heap.alloc);
        let line = self.line(span);
        let base_slot = self.reserve_allocation(carrier, alloc, span, line, statements, need)?;
        for (offset, marker) in [
            (0i64, REALM_STRING_KIND),
            (1i64, REALM_GENERATION),
            (2i64, length),
        ] {
            let physical = Expr::Add(Box::new(Expr::Var(base_slot)), Box::new(Expr::Int(offset)));
            let written = Self::guest_call(
                helper_write,
                vec![
                    Expr::Var(carrier),
                    Self::as_number(physical),
                    Self::as_number(Expr::Int(marker)),
                ],
            );
            statements.push(Stmt::Expr {
                expr: written,
                line,
            });
        }
        for (position, unit) in units.iter().enumerate() {
            let offset = REALM_STRING_HEADER_CELLS + position as i64;
            let physical = Expr::Add(Box::new(Expr::Var(base_slot)), Box::new(Expr::Int(offset)));
            let written = Self::guest_call(
                helper_write,
                vec![
                    Expr::Var(carrier),
                    Self::as_number(physical),
                    Self::as_number(Expr::Int(i64::from(*unit))),
                ],
            );
            statements.push(Stmt::Expr {
                expr: written,
                line,
            });
        }
        Ok(base_slot)
    }

    fn lower_this(&mut self, span: swc_common::Span) -> Result<Lowered, ParseError> {
        let Some(slot) = self.function_this else {
            return Err(self.error(
                span,
                "P0 `this` is admitted only inside closed strict source functions",
            ));
        };
        Ok(Lowered {
            expr: Expr::Var(slot),
            kind: Kind::Object,
        })
    }

    /// Decode an identifier key to UTF-16 units (ASCII/Latin1 identifiers
    /// encode directly; general unicode identifiers encode via UTF-16).
    fn ident_key_units(name: &str) -> Vec<u16> {
        name.encode_utf16().collect()
    }

    /// Extract an object key as UTF-16 units plus an exact duplicate-check key.
    /// Admits identifier and string-literal keys only; numeric/computed
    /// keys are explicit capability rejections.
    fn object_key_units(
        &self,
        key: &ast::PropName,
        span: swc_common::Span,
    ) -> Result<(Vec<u16>, Vec<u16>), ParseError> {
        match key {
            ast::PropName::Ident(id) => {
                let units = Self::ident_key_units(id.sym.as_str());
                Ok((units.clone(), units))
            }
            ast::PropName::Str(s) => {
                let units = self.decode_string_token(s.span)?;
                if units.len() > MAX_LITERAL_UTF16_UNITS {
                    return Err(self.error(
                        s.span,
                        "JavaScript ES5 string literal exceeds the 16384 UTF-16 unit bound",
                    ));
                }
                // Duplicate detection compares the exact UTF-16 units, so
                // lone surrogates stay distinct from each other and from
                // the replacement character.
                Ok((units.clone(), units))
            }
            _ => Err(self.error(
                span,
                "P0 plain objects admit only identifier and string-literal keys",
            )),
        }
    }

    /// Extract a member-expression key as UTF-16 units. Admits `obj.key`
    /// identifiers and `obj["key"]` string literals; integer subscripts
    /// belong to the numeric-array path and anything else rejects.
    fn member_key_units(&self, member: &ast::MemberExpr) -> Result<Vec<u16>, ParseError> {
        match &member.prop {
            ast::MemberProp::Ident(id) => Ok(Self::ident_key_units(id.sym.as_str())),
            ast::MemberProp::Computed(c) => match c.expr.as_ref() {
                ast::Expr::Lit(ast::Lit::Str(s)) => {
                    let units = self.decode_string_token(s.span)?;
                    if units.len() > MAX_LITERAL_UTF16_UNITS {
                        return Err(self.error(
                            s.span,
                            "JavaScript ES5 string literal exceeds the 16384 UTF-16 unit bound",
                        ));
                    }
                    Ok(units)
                }
                _ => Err(self.error(
                    c.span,
                    "P0 plain-object member reads admit only identifier and string-literal keys",
                )),
            },
            _ => Err(self.error(
                member.span,
                "P0 plain-object member reads admit only identifier and string-literal keys",
            )),
        }
    }

    fn lower_object_literal(&mut self, literal: &ast::ObjectLit) -> Result<Lowered, ParseError> {
        use std::collections::HashSet;
        if literal.props.len() > MAX_OBJECT_PROPERTIES {
            return Err(self.error(
                literal.span,
                "P0 plain objects admit at most 256 own properties",
            ));
        }
        // Validate keys first (precise admission, duplicate rejection)
        // before any allocation so failures leave no partial heap writes.
        let mut seen: HashSet<Vec<u16>> = HashSet::new();
        let mut entries: Vec<(Vec<u16>, Box<ast::Expr>, swc_common::Span)> = Vec::new();
        for prop in &literal.props {
            let ast::PropOrSpread::Prop(boxed) = prop else {
                return Err(self.error(prop.span(), "P0 plain objects reject spread properties"));
            };
            match boxed.as_ref() {
                ast::Prop::KeyValue(kv) => {
                    let (units, dup) = self.object_key_units(&kv.key, kv.span())?;
                    if !seen.insert(dup) {
                        return Err(self.error(
                            kv.span(),
                            "P0 plain objects reject duplicate keys until P7 descriptors",
                        ));
                    }
                    entries.push((units, kv.value.clone(), kv.span()));
                }
                _ => {
                    return Err(self.error(
                        boxed.span(),
                        "P0 plain objects admit only data key-value properties",
                    ));
                }
            }
        }
        let count = entries.len() as i64;
        let line = self.line(literal.span);
        let mut stmts: Vec<Stmt> = Vec::new();
        // Allocate key string nodes first so every property key is a real
        // UTF-16 guest string compared by content at runtime.
        let mut key_bases: Vec<u16> = Vec::new();
        for (units, _, key_span) in &entries {
            // Keys borrow the literal span for resource diagnostics.
            let base = self.emit_utf16_node(units, *key_span, &mut stmts)?;
            key_bases.push(base);
        }
        // Lower values (numbers, strings, objects, functions, member
        // reads, aliases). Closed-function bodies were prevalidated; free
        // names reject here through ordinary unknown-local diagnostics.
        let mut value_slots: Vec<(u16, Kind)> = Vec::new();
        for (_, value_expr, value_span) in &entries {
            let value = self.expr(value_expr)?;
            match value.kind {
                Kind::Undefined
                | Kind::Null
                | Kind::Boolean
                | Kind::Number
                | Kind::String
                | Kind::Object
                | Kind::Function => {}
                Kind::Array => {
                    return Err(self.error(
                        *value_span,
                        "P0 plain-object values admit primitives, strings, objects and functions; numeric arrays stay separate bindings",
                    ));
                }
            }
            let (slot, stmt) = self.bind_temp(value.expr, *value_span)?;
            stmts.push(stmt);
            value_slots.push((slot, value.kind));
        }
        let carrier = self.active_carrier(literal.span)?;
        let heap = self.heap_ref(literal.span)?;
        let (helper_write, alloc) = (heap.helper_write, heap.alloc);
        let total = REALM_OBJECT_HEADER_CELLS
            .checked_add(
                REALM_OBJECT_CELLS_PER_PROPERTY
                    .checked_mul(count)
                    .ok_or_else(|| self.error(literal.span, "P0 plain-object size overflow"))?,
            )
            .ok_or_else(|| self.error(literal.span, "P0 plain-object size overflow"))?;
        let need = u64::try_from(total)
            .map_err(|_| self.error(literal.span, "P0 plain-object size overflow"))?;
        let base_slot =
            self.reserve_allocation(carrier, alloc, literal.span, line, &mut stmts, need)?;
        for (offset, marker) in [
            (0i64, REALM_PLAIN_OBJECT_KIND),
            (1i64, REALM_GENERATION),
            (2i64, count),
        ] {
            let physical = Expr::Add(Box::new(Expr::Var(base_slot)), Box::new(Expr::Int(offset)));
            let written = Self::guest_call(
                helper_write,
                vec![
                    Expr::Var(carrier),
                    Self::as_number(physical),
                    Self::as_number(Expr::Int(marker)),
                ],
            );
            stmts.push(Stmt::Expr {
                expr: written,
                line,
            });
        }
        for (index, (key_base, (value_slot, _kind))) in
            key_bases.iter().zip(value_slots.iter()).enumerate()
        {
            let row = REALM_OBJECT_HEADER_CELLS + REALM_OBJECT_CELLS_PER_PROPERTY * index as i64;
            // Stored cells: key_ref, key_gen, value_tag, value_number,
            // value_flag01, value_ref, value_gen. Float payload is
            // preserved directly; Ints ride as Numbers via as_number.
            // The flag cell is preserved dynamically for EVERY static
            // kind: member-read and call results arrive as Kind::Object
            // while carrying a runtime Boolean tag, so a static
            // Kind::Boolean shortcut would silently store true as false.
            // Non-Boolean records always carry flag=false by
            // representation invariant, keeping this uniform path
            // truthful without any cast or projection.
            let (flag_slot, flag_stmt) = self.bind_typed(
                value::field(*value_slot, "flag"),
                TypeSchema::Bool,
                literal.span,
            )?;
            stmts.push(flag_stmt);
            // A constant-true record read through the same untyped Get
            // shape, so the comparison stays on the generic Eq path that
            // native SSA already compiles (cf. strict_equal_flag's
            // flag comparison). Eq against a Bool literal emits Ceq,
            // which native SSA rejects; a bare Tagged Get is likewise
            // rejected as a branch condition.
            let (true_slot, true_stmt) =
                self.bind_temp(value::boolean(Expr::Bool(true)), literal.span)?;
            stmts.push(true_stmt);
            let flag_number = Expr::IfElse {
                condition: Box::new(Expr::Eq(
                    Box::new(Expr::Var(flag_slot)),
                    Box::new(value::field(true_slot, "flag")),
                )),
                then_expr: Box::new(Expr::Float(1.0)),
                else_expr: Box::new(Expr::Float(0.0)),
            };
            let tag_number = Self::as_number(value::field(*value_slot, "tag"));
            let ref_number = Self::as_number(value::field(*value_slot, "ref_slot"));
            let gen_number = Self::as_number(value::field(*value_slot, "generation"));
            let cells: [(i64, Expr); 7] = [
                (0, Self::as_number(Expr::Var(*key_base))),
                (1, Self::as_number(Expr::Int(REALM_GENERATION))),
                (2, tag_number),
                (3, value::field(*value_slot, "number")),
                (4, flag_number),
                (5, ref_number),
                (6, gen_number),
            ];
            for (cell, payload) in cells {
                let physical = Expr::Add(
                    Box::new(Expr::Var(base_slot)),
                    Box::new(Expr::Int(row + cell)),
                );
                let written = Self::guest_call(
                    helper_write,
                    vec![Expr::Var(carrier), Self::as_number(physical), payload],
                );
                stmts.push(Stmt::Expr {
                    expr: written,
                    line,
                });
            }
        }
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(value::object_value(
                    Expr::Var(base_slot),
                    Expr::Int(REALM_GENERATION),
                )),
            },
            kind: Kind::Object,
        })
    }

    fn lower_function_expr(&mut self, function: &ast::FnExpr) -> Result<Lowered, ParseError> {
        let span = function.function.span;
        let key = (span.lo.0, span.hi.0);
        let Some(&code_id) = self.function_code_ids.get(&key) else {
            return Err(self.error(span, "P0 source-function admission requires predeclaration"));
        };
        let carrier = self.active_carrier(span)?;
        let heap = self.heap_ref(span)?;
        let (helper_write, alloc) = (heap.helper_write, heap.alloc);
        let line = self.line(span);
        let mut stmts = Vec::new();
        let need = u64::try_from(REALM_FUNCTION_CELLS)
            .map_err(|_| self.error(span, "P0 function allocation overflow"))?;
        let base_slot = self.reserve_allocation(carrier, alloc, span, line, &mut stmts, need)?;
        // Header: [kind 4, gen 1, 0, code_id, closed_env 0, strict 1].
        // The closed-env sentinel is explicitly empty, never a handle.
        for (offset, marker) in [
            (0i64, REALM_FUNCTION_KIND),
            (1i64, REALM_GENERATION),
            (2i64, 0),
            (3i64, code_id),
            (4i64, 0),
            (5i64, 1),
        ] {
            let physical = Expr::Add(Box::new(Expr::Var(base_slot)), Box::new(Expr::Int(offset)));
            let written = Self::guest_call(
                helper_write,
                vec![
                    Expr::Var(carrier),
                    Self::as_number(physical),
                    Self::as_number(Expr::Int(marker)),
                ],
            );
            stmts.push(Stmt::Expr {
                expr: written,
                line,
            });
        }
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(value::function_value(
                    Expr::Var(base_slot),
                    Expr::Int(REALM_GENERATION),
                )),
            },
            kind: Kind::Function,
        })
    }
}

pub(super) fn lower(
    script: &ast::Script,
    source: &str,
    source_id: u32,
    base: u32,
    context: &SourceFrontendContext<'_>,
) -> Result<FrontendIr, ParseError> {
    let heap_needed = script.body.iter().any(stmt_needs_heap);
    if !heap_needed {
        return lower_primitive(script, source, source_id, base);
    }
    lower_with_realm(script, source, source_id, base, context)
}

fn member_key_is_integer_literal(expression: &ast::Expr) -> bool {
    match expression {
        ast::Expr::Lit(ast::Lit::Num(n)) => {
            n.value.is_finite() && n.value.fract() == 0.0 && n.value >= 0.0
        }
        ast::Expr::Paren(p) => member_key_is_integer_literal(&p.expr),
        _ => false,
    }
}

fn stmt_needs_heap(statement: &ast::Stmt) -> bool {
    match statement {
        ast::Stmt::Decl(ast::Decl::Var(declaration)) => declaration
            .decls
            .iter()
            .any(|d| d.init.as_deref().is_some_and(expr_needs_heap)),
        ast::Stmt::Expr(e) => expr_needs_heap(&e.expr),
        _ => false,
    }
}

fn expr_needs_heap(expression: &ast::Expr) -> bool {
    match expression {
        ast::Expr::Array(_) => true,
        ast::Expr::Object(_) => true,
        ast::Expr::Fn(_) => true,
        ast::Expr::Call(_) => true,
        ast::Expr::This(_) => true,
        ast::Expr::Lit(ast::Lit::Str(_)) => true,
        ast::Expr::Member(_) => true,
        ast::Expr::Assign(a) => {
            expr_needs_heap(&a.right)
                || match &a.left {
                    ast::AssignTarget::Simple(ast::SimpleAssignTarget::Member(_)) => true,
                    ast::AssignTarget::Simple(ast::SimpleAssignTarget::Ident(_)) => false,
                    _ => false,
                }
        }
        ast::Expr::Paren(p) => expr_needs_heap(&p.expr),
        ast::Expr::Bin(b) => expr_needs_heap(&b.left) || expr_needs_heap(&b.right),
        ast::Expr::Unary(u) => expr_needs_heap(&u.arg),
        _ => false,
    }
}

fn lower_primitive(
    script: &ast::Script,
    source: &str,
    source_id: u32,
    base: u32,
) -> Result<FrontendIr, ParseError> {
    let mut lower = Lowerer {
        source,
        source_id,
        base,
        next_slot: 0,
        bindings: HashMap::new(),
        array_lengths: HashMap::new(),
        source_bindings: Vec::new(),
        heap: None,
        init_cells: 0,
        string_eq: None,
        enclosing_strict: false,
        function_this: None,
        carrier_override: None,
        helper_decode: None,
        helper_ref_get: None,
        helper_dispatch: None,
        function_code_ids: HashMap::new(),
        collected_functions: Vec::new(),
    };
    let mut statements = Vec::new();
    for statement in &script.body {
        match statement {
            ast::Stmt::Decl(ast::Decl::Var(declaration)) => {
                if declaration.kind != ast::VarDeclKind::Var {
                    return Err(lower.error(
                        declaration.span,
                        "JavaScript ES5 supports var declarations; let and const are rejected",
                    ));
                }
                for declarator in &declaration.decls {
                    let ast::Pat::Ident(binding) = &declarator.name else {
                        return Err(
                            lower.error(declarator.span, "P0 requires an identifier binding")
                        );
                    };
                    let name = binding.id.sym.to_string();
                    if lower.bindings.contains_key(&name) || name == "undefined" {
                        return Err(lower.error(binding.id.span, "P0 does not yet support redeclaration or binding the global undefined name"));
                    }
                    let Some(initializer) = declarator.init.as_deref() else {
                        return Err(lower.error(declarator.span, "P0 requires a var initializer"));
                    };
                    let initial = lower.expr(initializer)?;
                    let (slot, stmt) = lower.bind_temp(initial.expr, declarator.span)?;
                    lower.bindings.insert(name.clone(), (slot, initial.kind));
                    lower.source_bindings.push((name, slot));
                    statements.push(stmt);
                }
            }
            ast::Stmt::Expr(e) => {
                let lowered = lower.expr(&e.expr)?;
                let (slot, stmt) = lower.bind_temp(lowered.expr, e.span)?;
                statements.push(stmt);
                statements.push(Stmt::Expr {
                    expr: Expr::Var(slot),
                    line: lower.line(e.span),
                });
            }
            ast::Stmt::Empty(_) => {}
            _ => {
                return Err(
                    lower.error(statement.span(), "unsupported JavaScript ES5 P0 statement")
                );
            }
        }
    }
    let mut ir = LocalIrBuilder::new().finish(statements);
    ir.locals = usize::from(lower.next_slot);
    ir.local_bindings = lower.source_bindings;
    let declaration = value::declaration();
    ir.struct_schemas
        .insert(declaration.name.clone(), declaration);
    Ok(ir)
}

fn helper_index(
    ir: &FrontendIr,
    name: &str,
    span: swc_common::Span,
    lower: &Lowerer<'_>,
) -> Result<u16, ParseError> {
    ir.functions
        .iter()
        .find(|f| f.name == name)
        .map(|f| f.index)
        .ok_or_else(|| {
            lower.error(
                span,
                &format!("reference-array helper '{name}' is not available"),
            )
        })
}

/// Collect every admitted source-function AST node in pre-order, assign
/// finite static code IDs and validate the P0 closed-strict subset.
/// Never confuses code IDs (compile-time) with instance IDs (runtime).
fn collect_source_functions(
    script: &ast::Script,
    lower: &mut Lowerer<'_>,
    script_strict: bool,
) -> Result<(), ParseError> {
    for statement in &script.body {
        collect_from_stmt(statement, lower, script_strict)?;
    }
    Ok(())
}

fn collect_from_stmt(
    statement: &ast::Stmt,
    lower: &mut Lowerer<'_>,
    enclosing_strict: bool,
) -> Result<(), ParseError> {
    match statement {
        ast::Stmt::Decl(ast::Decl::Var(decl)) => {
            for declarator in &decl.decls {
                if let Some(init) = declarator.init.as_deref() {
                    collect_from_expr(init, lower, enclosing_strict)?;
                }
            }
            Ok(())
        }
        ast::Stmt::Expr(e) => collect_from_expr(&e.expr, lower, enclosing_strict),
        ast::Stmt::Empty(_) => Ok(()),
        // Unsupported statements reject later during lowering; collection
        // only needs the function subset.
        _ => Ok(()),
    }
}

fn collect_from_expr(
    expression: &ast::Expr,
    lower: &mut Lowerer<'_>,
    enclosing_strict: bool,
) -> Result<(), ParseError> {
    match expression {
        ast::Expr::Fn(f) => collect_function(f, lower, enclosing_strict),
        ast::Expr::Object(o) => {
            for prop in &o.props {
                if let ast::PropOrSpread::Prop(boxed) = prop
                    && let ast::Prop::KeyValue(kv) = boxed.as_ref()
                {
                    collect_from_expr(&kv.value, lower, enclosing_strict)?;
                }
            }
            Ok(())
        }
        ast::Expr::Call(c) => {
            if let ast::Callee::Expr(callee) = &c.callee {
                collect_from_expr(callee, lower, enclosing_strict)?;
            }
            for arg in &c.args {
                collect_from_expr(&arg.expr, lower, enclosing_strict)?;
            }
            Ok(())
        }
        ast::Expr::Member(m) => {
            collect_from_expr(&m.obj, lower, enclosing_strict)?;
            if let ast::MemberProp::Computed(c) = &m.prop {
                collect_from_expr(&c.expr, lower, enclosing_strict)?;
            }
            Ok(())
        }
        ast::Expr::Paren(p) => collect_from_expr(&p.expr, lower, enclosing_strict),
        ast::Expr::Bin(b) => {
            collect_from_expr(&b.left, lower, enclosing_strict)?;
            collect_from_expr(&b.right, lower, enclosing_strict)
        }
        ast::Expr::Unary(u) => collect_from_expr(&u.arg, lower, enclosing_strict),
        ast::Expr::Assign(a) => {
            collect_from_expr(&a.right, lower, enclosing_strict)?;
            match &a.left {
                ast::AssignTarget::Simple(ast::SimpleAssignTarget::Member(m)) => {
                    collect_from_expr(&ast::Expr::Member(m.clone()), lower, enclosing_strict)
                }
                _ => Ok(()),
            }
        }
        ast::Expr::Array(lit) => {
            for element in lit.elems.iter().flatten() {
                collect_from_expr(&element.expr, lower, enclosing_strict)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Validate one anonymous ordinary zero-parameter function and assign its
/// code ID. Bodies admit directives plus a simple Return, bare `return;`
/// or empty normal completion. Free lexical captures, named/self binding,
/// parameters, constructors, throw/try and generators are explicit
/// capability rejections.
fn collect_function(
    function: &ast::FnExpr,
    lower: &mut Lowerer<'_>,
    enclosing_strict: bool,
) -> Result<(), ParseError> {
    let span = function.function.span;
    // Deduplicate by span: the same node is reachable once, but defensive
    // re-entry (object values walked twice) must not mint two code IDs.
    let key = (span.lo.0, span.hi.0);
    if lower.function_code_ids.contains_key(&key) {
        return Ok(());
    }
    if function.ident.is_some() {
        return Err(lower.error(
            span,
            "P0 source functions admit only anonymous function expressions",
        ));
    }
    let func = function.function.as_ref();
    if func.is_async || func.is_generator {
        return Err(lower.error(
            span,
            "P0 source functions reject async and generator functions",
        ));
    }
    if !func.params.is_empty() {
        return Err(lower.error(
            span,
            "P0 source functions admit only zero formal parameters",
        ));
    }
    if func.this_param.is_some() {
        return Err(lower.error(span, "P0 source functions reject `this` parameters"));
    }
    if !func.decorators.is_empty() {
        return Err(lower.error(span, "P0 source functions reject decorators"));
    }
    if func.type_params.is_some() || func.return_type.is_some() {
        return Err(lower.error(span, "P0 source functions reject type annotations"));
    }
    let Some(body) = func.body.as_ref() else {
        return Err(lower.error(span, "P0 source functions require a body"));
    };
    let own_strict = lower.function_own_strict(body);
    let strict = own_strict || enclosing_strict;
    if !strict {
        return Err(lower.error(
            span,
            "P0 source functions require an exact \"use strict\" directive or inherited strictness",
        ));
    }
    // Body shape: leading string directives, then Return/Empty only.
    let mut in_prologue = true;
    for statement in &body.stmts {
        match statement {
            ast::Stmt::Expr(e) => {
                if in_prologue && matches!(e.expr.as_ref(), ast::Expr::Lit(ast::Lit::Str(_))) {
                    continue;
                }
                return Err(lower.error(
                    statement.span(),
                    "P0 source-function bodies admit only directives plus Return or empty completion",
                ));
            }
            ast::Stmt::Return(_) => {
                in_prologue = false;
            }
            ast::Stmt::Empty(_) => {}
            _ => {
                return Err(lower.error(
                    statement.span(),
                    "P0 source-function bodies admit only directives plus Return or empty completion",
                ));
            }
        }
    }
    if lower.collected_functions.len() >= MAX_SOURCE_FUNCTIONS {
        return Err(lower.error(span, "P0 source functions exceed the 256 function bound"));
    }
    let code_id = lower.collected_functions.len() as i64;
    lower.function_code_ids.insert(key, code_id);
    lower.collected_functions.push(CollectedFunction {
        code_id,
        function: function.function.clone(),
        span,
        strict,
    });
    // Recurse for nested closed functions with inherited strictness.
    for statement in &body.stmts {
        if let ast::Stmt::Return(ret) = statement
            && let Some(arg) = ret.arg.as_deref()
        {
            collect_from_expr(arg, lower, strict)?;
        }
    }
    Ok(())
}

/// Whether the script needs the operational Reference helpers: any plain
/// object literal, any identifier/string member (object key path) or any
/// `this` parameter. Pure numeric-array computed-index members do not
/// trigger; they keep the accepted fast path without dead helpers.
fn script_needs_ref_helpers(script: &ast::Script) -> bool {
    fn expr_needs(expression: &ast::Expr) -> bool {
        match expression {
            ast::Expr::Object(_) => true,
            ast::Expr::This(_) => true,
            ast::Expr::Fn(_) => true,
            ast::Expr::Call(_) => true,
            ast::Expr::Member(m) => match &m.prop {
                ast::MemberProp::Ident(_) => true,
                ast::MemberProp::Computed(c) => {
                    matches!(c.expr.as_ref(), ast::Expr::Lit(ast::Lit::Str(_)))
                        || expr_needs(&m.obj)
                }
                _ => expr_needs(&m.obj),
            },
            ast::Expr::Paren(p) => expr_needs(&p.expr),
            ast::Expr::Bin(b) => expr_needs(&b.left) || expr_needs(&b.right),
            ast::Expr::Unary(u) => expr_needs(&u.arg),
            ast::Expr::Assign(a) => {
                expr_needs(&a.right)
                    || match &a.left {
                        ast::AssignTarget::Simple(ast::SimpleAssignTarget::Member(m)) => {
                            expr_needs(&ast::Expr::Member(m.clone()))
                        }
                        _ => false,
                    }
            }
            ast::Expr::Array(lit) => lit
                .elems
                .iter()
                .flatten()
                .any(|element| expr_needs(&element.expr)),
            _ => false,
        }
    }
    script.body.iter().any(|statement| match statement {
        ast::Stmt::Decl(ast::Decl::Var(decl)) => decl
            .decls
            .iter()
            .any(|d| d.init.as_deref().is_some_and(expr_needs)),
        ast::Stmt::Expr(e) => expr_needs(&e.expr),
        _ => false,
    })
}

fn lower_with_realm(
    script: &ast::Script,
    source: &str,
    source_id: u32,
    base: u32,
    context: &SourceFrontendContext<'_>,
) -> Result<FrontendIr, ParseError> {
    let mut base_ir = crate::typescript::js_realm_prelude_ir(context).map_err(|error| {
        ParseError::at_span(
            vm::compiler::source_map::Span::new(source_id, 0, 0),
            format!("JS numeric-array realm requires the effective host catalog: {error:?}"),
        )
    })?;
    let probe_span = script
        .body
        .first()
        .map(|s| s.span())
        .unwrap_or(dummy_span(base));
    let mut lower = Lowerer {
        source,
        source_id,
        base,
        next_slot: u16::try_from(base_ir.locals).map_err(|_| {
            ParseError::at_span(
                vm::compiler::source_map::Span::new(source_id, 0, 0),
                "JS realm local slot space exhausted",
            )
        })?,
        bindings: HashMap::new(),
        array_lengths: HashMap::new(),
        source_bindings: Vec::new(),
        heap: None,
        init_cells: 0,
        string_eq: None,
        enclosing_strict: false,
        function_this: None,
        carrier_override: None,
        helper_decode: None,
        helper_ref_get: None,
        helper_dispatch: None,
        function_code_ids: HashMap::new(),
        collected_functions: Vec::new(),
    };
    let helper_make = helper_index(&base_ir, crate::typescript::HELPER_MAKE, probe_span, &lower)?;
    let helper_read = helper_index(&base_ir, crate::typescript::HELPER_READ, probe_span, &lower)?;
    let helper_write = helper_index(
        &base_ir,
        crate::typescript::HELPER_WRITE,
        probe_span,
        &lower,
    )?;
    let helper_resize = helper_index(
        &base_ir,
        crate::typescript::HELPER_RESIZE,
        probe_span,
        &lower,
    )?;
    let helper_length = helper_index(
        &base_ir,
        crate::typescript::HELPER_LENGTH,
        probe_span,
        &lower,
    )?;
    let mut statements = Vec::new();
    let alloc = append_realm_allocator(
        &mut lower,
        &mut base_ir,
        helper_make,
        helper_length,
        helper_resize,
        probe_span,
        &mut statements,
    )?;
    bootstrap_realm(
        &mut lower,
        helper_make,
        helper_read,
        helper_write,
        alloc,
        probe_span,
        &mut statements,
    )?;
    let string_eq = append_string_equality(
        &mut lower,
        &mut base_ir,
        helper_make,
        helper_read,
        probe_span,
        &mut statements,
    )?;
    lower.string_eq = Some(string_eq);
    // Script strictness from the exact raw prologue; inherited by every
    // admitted function without its own directive.
    let script_strict = lower.script_strict(script);
    lower.enclosing_strict = script_strict;
    collect_source_functions(script, &mut lower, script_strict)?;
    // Only append the object/function helpers when some source shape needs
    // them; pure numeric-array/string sources keep the accepted program
    // without dead native code.
    let need_ref = script_needs_ref_helpers(script);
    let function_count = lower.collected_functions.len();
    if need_ref || function_count > 0 {
        // Private object/function helpers. Decode first (pure), then
        // GetValue, then the finite static source-function table plus
        // dispatch.
        let decode = append_decode_int(&mut lower, &mut base_ir, probe_span, &mut statements)?;
        lower.helper_decode = Some(decode);
        let ref_get = append_ref_get(
            &mut lower,
            &mut base_ir,
            helper_make,
            helper_read,
            decode,
            probe_span,
            &mut statements,
        )?;
        lower.helper_ref_get = Some(ref_get);
        let carrier_schema = base_ir
            .functions
            .iter()
            .find(|function| function.index == helper_make)
            .and_then(|function| function.return_schema.clone())
            .ok_or_else(|| {
                lower.error(
                    probe_span,
                    "JS source-function table requires the carrier helper return schema",
                )
            })?;
        // Preassign finite flat indices so bodies can call dispatch before
        // it is appended; the dispatch branch table then targets these.
        let next_index = base_ir
            .functions
            .iter()
            .map(|function| function.index)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| lower.error(probe_span, "JS source-function index space exhausted"))?;
        let mut func_indices: Vec<u16> = Vec::new();
        for offset in 0..function_count {
            let flat = next_index.checked_add(offset as u16).ok_or_else(|| {
                lower.error(probe_span, "JS source-function index space exhausted")
            })?;
            func_indices.push(flat);
        }
        let dispatch_index = next_index
            .checked_add(function_count as u16)
            .ok_or_else(|| lower.error(probe_span, "JS dispatch index space exhausted"))?;
        lower.helper_dispatch = Some(dispatch_index);
        let collected = std::mem::take(&mut lower.collected_functions);
        for (position, item) in collected.iter().enumerate() {
            append_source_function(
                &mut lower,
                &mut base_ir,
                item,
                func_indices[position],
                carrier_schema.clone(),
                probe_span,
                &mut statements,
            )?;
        }
        if function_count > 0 {
            let dispatch = append_dispatch(
                &mut lower,
                &mut base_ir,
                helper_make,
                helper_read,
                decode,
                &func_indices,
                probe_span,
                &mut statements,
            )?;
            debug_assert_eq!(dispatch, dispatch_index);
        }
    }
    for statement in &script.body {
        // Skip exact `"use strict"` directives: they set caller strictness
        // above and produce no value.
        if let ast::Stmt::Expr(e) = statement
            && let ast::Expr::Lit(ast::Lit::Str(s)) = e.expr.as_ref()
            && lower.is_exact_use_strict_lexeme(s.span)
        {
            continue;
        }
        match statement {
            ast::Stmt::Decl(ast::Decl::Var(declaration)) => {
                if declaration.kind != ast::VarDeclKind::Var {
                    return Err(lower.error(
                        declaration.span,
                        "JavaScript ES5 supports var declarations; let and const are rejected",
                    ));
                }
                for declarator in &declaration.decls {
                    let ast::Pat::Ident(binding) = &declarator.name else {
                        return Err(
                            lower.error(declarator.span, "P0 requires an identifier binding")
                        );
                    };
                    let name = binding.id.sym.to_string();
                    if lower.bindings.contains_key(&name) || name == "undefined" {
                        return Err(lower.error(binding.id.span, "P0 does not yet support redeclaration or binding the global undefined name"));
                    }
                    let Some(initializer) = declarator.init.as_deref() else {
                        return Err(lower.error(declarator.span, "P0 requires a var initializer"));
                    };
                    if let ast::Expr::Array(literal) = initializer {
                        lower_array_binding(
                            &mut lower,
                            &name,
                            literal,
                            declarator.span,
                            &mut statements,
                        )?;
                        continue;
                    }
                    if let ast::Expr::Ident(alias) = initializer
                        && let Some(&(src_slot, Kind::Array)) =
                            lower.bindings.get(alias.sym.as_str())
                    {
                        let length = lower
                            .array_lengths
                            .get(alias.sym.as_str())
                            .copied()
                            .unwrap_or(0);
                        let (slot, stmt) = lower.bind_temp(Expr::Var(src_slot), declarator.span)?;
                        lower.bindings.insert(name.clone(), (slot, Kind::Array));
                        lower.array_lengths.insert(name.clone(), length);
                        lower.source_bindings.push((name, slot));
                        statements.push(stmt);
                        continue;
                    }
                    let initial = lower.expr(initializer)?;
                    if initial.kind == Kind::Array {
                        return Err(lower.error(
                            declarator.span,
                            "P0 array bindings require an array literal or array alias",
                        ));
                    }
                    let (slot, stmt) = lower.bind_temp(initial.expr, declarator.span)?;
                    lower.bindings.insert(name.clone(), (slot, initial.kind));
                    lower.source_bindings.push((name, slot));
                    statements.push(stmt);
                }
            }
            ast::Stmt::Expr(e) => {
                if let ast::Expr::Assign(assign) = e.expr.as_ref()
                    && assign.op == ast::AssignOp::Assign
                    && let ast::AssignTarget::Simple(ast::SimpleAssignTarget::Member(member)) =
                        &assign.left
                {
                    lower_member_write_stmt(
                        &mut lower,
                        member,
                        &assign.right,
                        e.span,
                        &mut statements,
                    )?;
                    continue;
                }
                let lowered = lower.expr(&e.expr)?;
                let (slot, stmt) = lower.bind_temp(lowered.expr, e.span)?;
                statements.push(stmt);
                statements.push(Stmt::Expr {
                    expr: Expr::Var(slot),
                    line: lower.line(e.span),
                });
            }
            ast::Stmt::Empty(_) => {}
            _ => {
                return Err(
                    lower.error(statement.span(), "unsupported JavaScript ES5 P0 statement")
                );
            }
        }
    }
    base_ir.stmts.extend(statements);
    base_ir.locals = usize::from(lower.next_slot);
    base_ir.local_bindings.extend(lower.source_bindings);
    let declaration = value::declaration();
    base_ir
        .struct_schemas
        .insert(declaration.name.clone(), declaration);
    let reference = value::reference_declaration();
    base_ir
        .struct_schemas
        .insert(reference.name.clone(), reference);
    let completion = value::completion_declaration();
    base_ir
        .struct_schemas
        .insert(completion.name.clone(), completion);
    Ok(base_ir)
}

fn dummy_span(base: u32) -> swc_common::Span {
    swc_common::Span::new(swc_common::BytePos(base), swc_common::BytePos(base))
}

/// Guard statement over a Bool condition through the existing special
/// `assert` builtin: success pushes nothing (a control check, not a value),
/// failure aborts with `HostError("assertion failed")`. This is private
/// metadata/resource-budget enforcement, never a catchable JS throw.
fn assert_stmt(condition: Expr, line: u32) -> Stmt {
    Stmt::Expr {
        expr: Expr::Call(
            BuiltinFunction::Assert.call_index(),
            Vec::new(),
            vec![condition],
            None,
            None,
        ),
        line,
    }
}

/// Append the private static guest allocation function with the fixed typed
/// `(Carrier, count: Int) -> Int` signature, a fresh function index, fresh
/// slots, explicit schemas and no mutable captures. Every literal calls it
/// at runtime with the owning carrier and its header+payload cell count. It
/// reads the real backing length through the existing length helper,
/// normalizes to Float on the proven as_number path and decodes the exact
/// Int base with bounded binary decomposition: If steps for 2^16 down to
/// 2^0 with Float remainder subtraction plus Int accumulation, then a zero
/// remainder guard. Bounds on length, count and end abort before resize;
/// only after the existing resize succeeds is the decoded Int base
/// returned. The backing is an append-only arena: no captured cursor, no
/// static bases, no extra resource, and logical ref_slot/generation stay
/// Int. Prelude function indices, host metadata, schemas, origins and the
/// source producer policy are untouched; the carrier schema comes from the
/// existing make-helper return schema.
fn append_realm_allocator(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    helper_make: u16,
    helper_length: u16,
    helper_resize: u16,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let carrier_schema = ir
        .functions
        .iter()
        .find(|function| function.index == helper_make)
        .and_then(|function| function.return_schema.clone())
        .ok_or_else(|| {
            lower.error(
                span,
                "JS realm allocator requires the carrier helper return schema",
            )
        })?;
    let index = ir
        .functions
        .iter()
        .map(|function| function.index)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| lower.error(span, "JS realm allocator function index space exhausted"))?;
    let line = lower.line(span);
    let param_carrier = lower.slot(span)?;
    let param_count = lower.slot(span)?;
    let length_number = lower.slot(span)?;
    let remaining = lower.slot(span)?;
    let decoded = lower.slot(span)?;
    let extent_end = lower.slot(span)?;
    let budget = MAX_REALM_INITIAL_CELLS as f64;
    let budget_int = MAX_REALM_INITIAL_CELLS as i64;
    let mut body = vec![
        Stmt::Let {
            index: length_number,
            declared_schema: Some(TypeSchema::Number),
            expr: Lowerer::guest_call(helper_length, vec![Expr::Var(param_carrier)]),
            line,
        },
        Stmt::Let {
            index: remaining,
            declared_schema: Some(TypeSchema::Number),
            expr: Lowerer::as_number(Expr::Var(length_number)),
            line,
        },
        assert_stmt(
            Expr::Not(Box::new(Expr::Lt(
                Box::new(Expr::Var(remaining)),
                Box::new(Expr::Float(0.0)),
            ))),
            line,
        ),
        assert_stmt(
            Expr::Or(
                Box::new(Expr::Lt(
                    Box::new(Expr::Var(remaining)),
                    Box::new(Expr::Float(budget)),
                )),
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(remaining)),
                    Box::new(Expr::Float(budget)),
                )),
            ),
            line,
        ),
        assert_stmt(
            Expr::Gt(Box::new(Expr::Var(param_count)), Box::new(Expr::Int(0))),
            line,
        ),
        assert_stmt(
            Expr::Or(
                Box::new(Expr::Lt(
                    Box::new(Expr::Var(param_count)),
                    Box::new(Expr::Int(budget_int)),
                )),
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(param_count)),
                    Box::new(Expr::Int(budget_int)),
                )),
            ),
            line,
        ),
        Stmt::Let {
            index: decoded,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
    ];
    for bit in (0..=16u32).rev() {
        let threshold = f64::from(1u32 << bit);
        let bit_value = 1i64 << bit;
        body.push(Stmt::IfElse {
            condition: Expr::Or(
                Box::new(Expr::Gt(
                    Box::new(Expr::Var(remaining)),
                    Box::new(Expr::Float(threshold)),
                )),
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(remaining)),
                    Box::new(Expr::Float(threshold)),
                )),
            ),
            then_branch: vec![
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: remaining,
                    expr: Expr::Sub(
                        Box::new(Expr::Var(remaining)),
                        Box::new(Expr::Float(threshold)),
                    ),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: decoded,
                    expr: Expr::Add(Box::new(Expr::Var(decoded)), Box::new(Expr::Int(bit_value))),
                    line,
                },
            ],
            else_branch: Vec::new(),
            line,
        });
    }
    body.push(assert_stmt(
        Expr::Eq(Box::new(Expr::Var(remaining)), Box::new(Expr::Float(0.0))),
        line,
    ));
    body.push(Stmt::Let {
        index: extent_end,
        declared_schema: Some(TypeSchema::Int),
        expr: Expr::Add(
            Box::new(Expr::Var(decoded)),
            Box::new(Expr::Var(param_count)),
        ),
        line,
    });
    body.push(assert_stmt(
        Expr::Or(
            Box::new(Expr::Lt(
                Box::new(Expr::Var(extent_end)),
                Box::new(Expr::Int(budget_int)),
            )),
            Box::new(Expr::Eq(
                Box::new(Expr::Var(extent_end)),
                Box::new(Expr::Int(budget_int)),
            )),
        ),
        line,
    ));
    body.push(Stmt::Expr {
        expr: Lowerer::guest_call(
            helper_resize,
            vec![
                Expr::Var(param_carrier),
                Lowerer::as_number(Expr::Var(extent_end)),
            ],
        ),
        line,
    });
    let args = vec!["carrier".to_owned(), "count".to_owned()];
    ir.functions.push(FunctionDecl {
        name: REALM_ALLOC_HELPER.to_owned(),
        arity: 2,
        index,
        args: args.clone(),
        arg_schemas: vec![Some(carrier_schema), Some(TypeSchema::Int)],
        return_schema: Some(TypeSchema::Int),
        type_params: Vec::new(),
        exported: false,
        return_type: ValueType::Int,
        symbol: None,
    });
    ir.function_impls.insert(
        index,
        FunctionImpl {
            param_slots: vec![param_carrier, param_count],
            capture_copies: Vec::new(),
            body_stmts: body,
            body_expr: Expr::Var(decoded),
            body_expr_line: line,
        },
    );
    statements.push(Stmt::FuncDecl {
        name: REALM_ALLOC_HELPER.to_owned(),
        index,
        arity: 2,
        args,
        exported: false,
        has_impl: true,
        line,
    });
    Ok(index)
}

fn bootstrap_realm(
    lower: &mut Lowerer<'_>,
    helper_make: u16,
    helper_read: u16,
    helper_write: u16,
    alloc: u16,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    // Single owning carrier for the whole realm. Literal bases are fresh
    // runtime extents from the allocator function below, so no compile-time
    // cursor or allocator closure is needed here.
    let carrier = lower.slot(span)?;
    let empty = Lowerer::builtin_call(BuiltinFunction::ArrayNew.call_index(), Vec::new());
    let made = Lowerer::guest_call(helper_make, vec![empty]);
    statements.push(Stmt::Let {
        index: carrier,
        declared_schema: None,
        expr: made,
        line: lower.line(span),
    });
    lower.heap = Some(Heap {
        carrier,
        helper_read,
        helper_write,
        alloc,
    });
    Ok(())
}

/// Read one stored UTF-16 unit through the realm read helper: physical cell
/// `base + 3 + index`, normalized to Number like every other realm read.
fn read_string_unit(helper_read: u16, carrier: u16, base: u16, index: u16) -> Expr {
    let physical = Expr::Add(
        Box::new(Expr::Add(
            Box::new(Expr::Var(base)),
            Box::new(Expr::Int(REALM_STRING_HEADER_CELLS)),
        )),
        Box::new(Expr::Var(index)),
    );
    Lowerer::as_number(Lowerer::guest_call(
        helper_read,
        vec![Expr::Var(carrier), Lowerer::as_number(physical)],
    ))
}

/// Append the private static guest string-equality function with the fixed
/// typed `(Carrier, JsValue, JsValue) -> Bool` signature, a fresh function
/// index and fresh local slots. It reads actual stored lengths and code
/// units through the realm read helper: declared Float lengths compare
/// first, then a declared Int index walks the units while a declared Bool
/// result stays true; a mismatch terminates via the Bool condition. It
/// allocates no new logical nodes. Mixed tags never reach it because the
/// strict-equal STRING branch only invokes it for matching STRING tags.
/// Prelude function indices, host metadata, schemas, origins and the source
/// producer policy are untouched; the carrier schema comes from the
/// existing make-helper return schema.
fn append_string_equality(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    helper_make: u16,
    helper_read: u16,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let carrier_schema = ir
        .functions
        .iter()
        .find(|function| function.index == helper_make)
        .and_then(|function| function.return_schema.clone())
        .ok_or_else(|| {
            lower.error(
                span,
                "JS string realm requires the carrier helper return schema",
            )
        })?;
    let js_schema = value::schema();
    let index = ir
        .functions
        .iter()
        .map(|function| function.index)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| lower.error(span, "JS string helper function index space exhausted"))?;
    let line = lower.line(span);
    let param_carrier = lower.slot(span)?;
    let param_left = lower.slot(span)?;
    let param_right = lower.slot(span)?;
    let base_left = lower.slot(span)?;
    let base_right = lower.slot(span)?;
    let length_left = lower.slot(span)?;
    let length_right = lower.slot(span)?;
    let cursor = lower.slot(span)?;
    let result = lower.slot(span)?;
    let length_operand = |slot: u16| -> Expr {
        Lowerer::as_number(Lowerer::guest_call(
            helper_read,
            vec![
                Expr::Var(param_carrier),
                Lowerer::as_number(Expr::Add(Box::new(Expr::Var(slot)), Box::new(Expr::Int(2)))),
            ],
        ))
    };
    let body = vec![
        Stmt::Let {
            index: base_left,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field(param_left, "ref_slot"),
            line,
        },
        Stmt::Let {
            index: base_right,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field(param_right, "ref_slot"),
            line,
        },
        Stmt::Let {
            index: length_left,
            declared_schema: Some(TypeSchema::Number),
            expr: length_operand(base_left),
            line,
        },
        Stmt::Let {
            index: length_right,
            declared_schema: Some(TypeSchema::Number),
            expr: length_operand(base_right),
            line,
        },
        Stmt::Let {
            index: cursor,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: result,
            declared_schema: Some(TypeSchema::Bool),
            expr: Expr::Eq(
                Box::new(Expr::Var(length_left)),
                Box::new(Expr::Var(length_right)),
            ),
            line,
        },
        Stmt::While {
            condition: Expr::And(
                Box::new(Expr::Var(result)),
                Box::new(Expr::Lt(
                    Box::new(Lowerer::as_number(Expr::Var(cursor))),
                    Box::new(Expr::Var(length_left)),
                )),
            ),
            body: vec![
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: result,
                    expr: Expr::And(
                        Box::new(Expr::Var(result)),
                        Box::new(Expr::Eq(
                            Box::new(read_string_unit(
                                helper_read,
                                param_carrier,
                                base_left,
                                cursor,
                            )),
                            Box::new(read_string_unit(
                                helper_read,
                                param_carrier,
                                base_right,
                                cursor,
                            )),
                        )),
                    ),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: cursor,
                    expr: Expr::Add(Box::new(Expr::Var(cursor)), Box::new(Expr::Int(1))),
                    line,
                },
            ],
            line,
        },
    ];
    let args = vec!["carrier".to_owned(), "left".to_owned(), "right".to_owned()];
    ir.functions.push(FunctionDecl {
        name: STRING_EQUALITY_HELPER.to_owned(),
        arity: 3,
        index,
        args: args.clone(),
        arg_schemas: vec![
            Some(carrier_schema),
            Some(js_schema.clone()),
            Some(js_schema),
        ],
        return_schema: Some(TypeSchema::Bool),
        type_params: Vec::new(),
        exported: false,
        return_type: ValueType::Bool,
        symbol: None,
    });
    ir.function_impls.insert(
        index,
        FunctionImpl {
            param_slots: vec![param_carrier, param_left, param_right],
            capture_copies: Vec::new(),
            body_stmts: body,
            body_expr: Expr::Var(result),
            body_expr_line: line,
        },
    );
    statements.push(Stmt::FuncDecl {
        name: STRING_EQUALITY_HELPER.to_owned(),
        index,
        arity: 3,
        args,
        exported: false,
        has_impl: true,
        line,
    });
    Ok(index)
}

/// Append the private exact-Int decode helper `(Number) -> Int`, factored
/// from the proven allocator bit logic. Bounded binary decomposition over
/// 2^16 down to 2^0 with Float remainder subtraction plus Int
/// accumulation, then a zero-remainder guard. No invented casts.
fn append_decode_int(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let index = ir
        .functions
        .iter()
        .map(|function| function.index)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| lower.error(span, "JS decode helper function index space exhausted"))?;
    let line = lower.line(span);
    let param_x = lower.slot(span)?;
    let remaining = lower.slot(span)?;
    let decoded = lower.slot(span)?;
    let budget = MAX_REALM_INITIAL_CELLS as f64;
    let budget_int = MAX_REALM_INITIAL_CELLS as i64;
    let mut body = vec![
        Stmt::Let {
            index: remaining,
            declared_schema: Some(TypeSchema::Number),
            expr: Lowerer::as_number(Expr::Var(param_x)),
            line,
        },
        assert_stmt(
            Expr::Not(Box::new(Expr::Lt(
                Box::new(Expr::Var(remaining)),
                Box::new(Expr::Float(0.0)),
            ))),
            line,
        ),
        assert_stmt(
            Expr::Or(
                Box::new(Expr::Lt(
                    Box::new(Expr::Var(remaining)),
                    Box::new(Expr::Float(budget)),
                )),
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(remaining)),
                    Box::new(Expr::Float(budget)),
                )),
            ),
            line,
        ),
        Stmt::Let {
            index: decoded,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
    ];
    for bit in (0..=16u32).rev() {
        let threshold = f64::from(1u32 << bit);
        let bit_value = 1i64 << bit;
        body.push(Stmt::IfElse {
            condition: Expr::Or(
                Box::new(Expr::Gt(
                    Box::new(Expr::Var(remaining)),
                    Box::new(Expr::Float(threshold)),
                )),
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(remaining)),
                    Box::new(Expr::Float(threshold)),
                )),
            ),
            then_branch: vec![
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: remaining,
                    expr: Expr::Sub(
                        Box::new(Expr::Var(remaining)),
                        Box::new(Expr::Float(threshold)),
                    ),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: decoded,
                    expr: Expr::Add(Box::new(Expr::Var(decoded)), Box::new(Expr::Int(bit_value))),
                    line,
                },
            ],
            else_branch: Vec::new(),
            line,
        });
    }
    body.push(assert_stmt(
        Expr::Eq(Box::new(Expr::Var(remaining)), Box::new(Expr::Float(0.0))),
        line,
    ));
    // Silence unused-budget lint while keeping the bound explicit.
    let _ = budget_int;
    let args = vec!["x".to_owned()];
    ir.functions.push(FunctionDecl {
        name: DECODE_INT_HELPER.to_owned(),
        arity: 1,
        index,
        args: args.clone(),
        arg_schemas: vec![Some(TypeSchema::Number)],
        return_schema: Some(TypeSchema::Int),
        type_params: Vec::new(),
        exported: false,
        return_type: ValueType::Int,
        symbol: None,
    });
    ir.function_impls.insert(
        index,
        FunctionImpl {
            param_slots: vec![param_x],
            capture_copies: Vec::new(),
            body_stmts: body,
            body_expr: Expr::Var(decoded),
            body_expr_line: line,
        },
    );
    statements.push(Stmt::FuncDecl {
        name: DECODE_INT_HELPER.to_owned(),
        index,
        arity: 1,
        args,
        exported: false,
        has_impl: true,
        line,
    });
    Ok(index)
}

/// Append the operational GetValue helper `(Carrier, Reference) -> JsValue`.
/// Reads the actual own data property: validates the plain-object header,
/// walks the seven-cell rows, compares UTF-16 keys by content (lengths
/// then units through the realm read helper) and reconstructs the complete
/// tagged payload with exact-Int decode. Missing keys and non-object bases
/// are capability guards (assert), never semantic Undefined.
fn append_ref_get(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    helper_make: u16,
    helper_read: u16,
    decode: u16,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let carrier_schema = ir
        .functions
        .iter()
        .find(|function| function.index == helper_make)
        .and_then(|function| function.return_schema.clone())
        .ok_or_else(|| {
            lower.error(
                span,
                "JS reference helper requires the carrier helper return schema",
            )
        })?;
    let index = ir
        .functions
        .iter()
        .map(|function| function.index)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| lower.error(span, "JS reference helper index space exhausted"))?;
    let line = lower.line(span);
    let param_carrier = lower.slot(span)?;
    let param_ref = lower.slot(span)?;
    let obj_val = lower.slot(span)?;
    let key_base = lower.slot(span)?;
    let obj_base = lower.slot(span)?;
    let obj_tag = lower.slot(span)?;
    let kind_num = lower.slot(span)?;
    let gen_num = lower.slot(span)?;
    let count_num = lower.slot(span)?;
    let search_kind = lower.slot(span)?;
    let search_len = lower.slot(span)?;
    let found = lower.slot(span)?;
    let prop_idx = lower.slot(span)?;
    let res_tag = lower.slot(span)?;
    let res_number = lower.slot(span)?;
    let res_flag = lower.slot(span)?;
    let res_ref = lower.slot(span)?;
    let res_gen = lower.slot(span)?;
    let cur_key_ref_num = lower.slot(span)?;
    let cur_key_ref = lower.slot(span)?;
    let cur_key_len = lower.slot(span)?;
    let cur_tag_num = lower.slot(span)?;
    let cur_number = lower.slot(span)?;
    let cur_flag_num = lower.slot(span)?;
    let cur_ref_num = lower.slot(span)?;
    let cur_gen_num = lower.slot(span)?;
    let unit_cursor = lower.slot(span)?;
    let unit_match = lower.slot(span)?;
    let key_match = lower.slot(span)?;
    let search_unit = lower.slot(span)?;
    let stored_unit = lower.slot(span)?;
    let js_schema = value::schema();
    let read_cell = |carrier: u16, base: Expr| -> Expr {
        Lowerer::guest_call(
            helper_read,
            vec![Expr::Var(carrier), Lowerer::as_number(base)],
        )
    };
    let mut body = vec![
        Stmt::Let {
            index: obj_val,
            declared_schema: Some(js_schema.clone()),
            expr: value::field_of(Expr::Var(param_ref), "base"),
            line,
        },
        Stmt::Let {
            index: key_base,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field_of(Expr::Var(param_ref), "key_slot"),
            line,
        },
        Stmt::Let {
            index: obj_base,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field(obj_val, "ref_slot"),
            line,
        },
        Stmt::Let {
            index: obj_tag,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field(obj_val, "tag"),
            line,
        },
        assert_stmt(
            Expr::Eq(
                Box::new(Expr::Var(obj_tag)),
                Box::new(Expr::Int(value::OBJECT)),
            ),
            line,
        ),
        Stmt::Let {
            index: kind_num,
            declared_schema: Some(TypeSchema::Number),
            expr: read_cell(
                param_carrier,
                Expr::Add(Box::new(Expr::Var(obj_base)), Box::new(Expr::Int(0))),
            ),
            line,
        },
        assert_stmt(
            Expr::Eq(
                Box::new(Lowerer::as_number(Expr::Var(kind_num))),
                Box::new(Expr::Float(REALM_PLAIN_OBJECT_KIND as f64)),
            ),
            line,
        ),
        Stmt::Let {
            index: gen_num,
            declared_schema: Some(TypeSchema::Number),
            expr: read_cell(
                param_carrier,
                Expr::Add(Box::new(Expr::Var(obj_base)), Box::new(Expr::Int(1))),
            ),
            line,
        },
        assert_stmt(
            Expr::Eq(
                Box::new(Lowerer::as_number(Expr::Var(gen_num))),
                Box::new(Expr::Float(1.0)),
            ),
            line,
        ),
        Stmt::Let {
            index: count_num,
            declared_schema: Some(TypeSchema::Number),
            expr: read_cell(
                param_carrier,
                Expr::Add(Box::new(Expr::Var(obj_base)), Box::new(Expr::Int(2))),
            ),
            line,
        },
        Stmt::Let {
            index: search_kind,
            declared_schema: Some(TypeSchema::Number),
            expr: read_cell(
                param_carrier,
                Expr::Add(Box::new(Expr::Var(key_base)), Box::new(Expr::Int(0))),
            ),
            line,
        },
        assert_stmt(
            Expr::Eq(
                Box::new(Lowerer::as_number(Expr::Var(search_kind))),
                Box::new(Expr::Float(REALM_STRING_KIND as f64)),
            ),
            line,
        ),
        Stmt::Let {
            index: search_len,
            declared_schema: Some(TypeSchema::Number),
            expr: read_cell(
                param_carrier,
                Expr::Add(Box::new(Expr::Var(key_base)), Box::new(Expr::Int(2))),
            ),
            line,
        },
        Stmt::Let {
            index: found,
            declared_schema: Some(TypeSchema::Bool),
            expr: Expr::Bool(false),
            line,
        },
        Stmt::Let {
            index: prop_idx,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: res_tag,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: res_number,
            declared_schema: Some(TypeSchema::Float),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: res_flag,
            declared_schema: Some(TypeSchema::Bool),
            expr: Expr::Bool(false),
            line,
        },
        Stmt::Let {
            index: res_ref,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: res_gen,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: cur_key_ref_num,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: cur_key_ref,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: cur_key_len,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: cur_tag_num,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: cur_number,
            declared_schema: Some(TypeSchema::Float),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: cur_flag_num,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: cur_ref_num,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: cur_gen_num,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: unit_cursor,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: unit_match,
            declared_schema: Some(TypeSchema::Bool),
            expr: Expr::Bool(true),
            line,
        },
        Stmt::Let {
            index: key_match,
            declared_schema: Some(TypeSchema::Bool),
            expr: Expr::Bool(false),
            line,
        },
        Stmt::Let {
            index: search_unit,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: stored_unit,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
    ];
    // Row base for property `prop_idx`, cell `c`:
    // `obj_base + 3 + 7 * prop_idx + c`.
    let row_cell = |cell: i64| -> Expr {
        Expr::Add(
            Box::new(Expr::Add(
                Box::new(Expr::Var(obj_base)),
                Box::new(Expr::Add(
                    Box::new(Expr::Int(REALM_OBJECT_HEADER_CELLS)),
                    Box::new(Expr::Mul(
                        Box::new(Expr::Var(prop_idx)),
                        Box::new(Expr::Int(REALM_OBJECT_CELLS_PER_PROPERTY)),
                    )),
                )),
            )),
            Box::new(Expr::Int(cell)),
        )
    };
    // Inner unit-compare loop over the candidate key.
    let search_unit_read = read_cell(
        param_carrier,
        Expr::Add(
            Box::new(Expr::Add(
                Box::new(Expr::Var(key_base)),
                Box::new(Expr::Int(REALM_STRING_HEADER_CELLS)),
            )),
            Box::new(Expr::Var(unit_cursor)),
        ),
    );
    let stored_unit_read = read_cell(
        param_carrier,
        Expr::Add(
            Box::new(Expr::Add(
                Box::new(Expr::Var(cur_key_ref)),
                Box::new(Expr::Int(REALM_STRING_HEADER_CELLS)),
            )),
            Box::new(Expr::Var(unit_cursor)),
        ),
    );
    let unit_loop = Stmt::While {
        condition: Expr::And(
            Box::new(Expr::Var(unit_match)),
            Box::new(Expr::Lt(
                Box::new(Lowerer::as_number(Expr::Var(unit_cursor))),
                Box::new(Expr::Var(search_len)),
            )),
        ),
        body: vec![
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: search_unit,
                expr: search_unit_read,
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: stored_unit,
                expr: stored_unit_read,
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: unit_match,
                expr: Expr::And(
                    Box::new(Expr::Var(unit_match)),
                    Box::new(Expr::Eq(
                        Box::new(Expr::Var(search_unit)),
                        Box::new(Expr::Var(stored_unit)),
                    )),
                ),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: unit_cursor,
                expr: Expr::Add(Box::new(Expr::Var(unit_cursor)), Box::new(Expr::Int(1))),
                line,
            },
        ],
        line,
    };
    let decode_call = |input: u16| -> Expr { Lowerer::guest_call(decode, vec![Expr::Var(input)]) };
    // Per-property body: read the seven cells, decode the stored key base,
    // compare contents, and on a hit reconstruct the tagged payload.
    let per_property: Vec<Stmt> = vec![
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: cur_key_ref_num,
            expr: read_cell(param_carrier, row_cell(0)),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: cur_tag_num,
            expr: read_cell(param_carrier, row_cell(2)),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: cur_number,
            expr: Lowerer::as_number(read_cell(param_carrier, row_cell(3))),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: cur_flag_num,
            expr: read_cell(param_carrier, row_cell(4)),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: cur_ref_num,
            expr: read_cell(param_carrier, row_cell(5)),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: cur_gen_num,
            expr: read_cell(param_carrier, row_cell(6)),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: cur_key_ref,
            expr: decode_call(cur_key_ref_num),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: cur_key_len,
            expr: read_cell(
                param_carrier,
                Expr::Add(Box::new(Expr::Var(cur_key_ref)), Box::new(Expr::Int(2))),
            ),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: unit_cursor,
            expr: Expr::Int(0),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: unit_match,
            expr: Expr::Bool(true),
            line,
        },
        unit_loop,
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: key_match,
            expr: Expr::And(
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(search_len)),
                    Box::new(Expr::Var(cur_key_len)),
                )),
                Box::new(Expr::Var(unit_match)),
            ),
            line,
        },
        Stmt::IfElse {
            condition: Expr::Var(key_match),
            then_branch: vec![
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: res_tag,
                    expr: decode_call(cur_tag_num),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: res_number,
                    expr: Expr::Var(cur_number),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: res_flag,
                    expr: Expr::Eq(
                        Box::new(Lowerer::as_number(Expr::Var(cur_flag_num))),
                        Box::new(Expr::Float(1.0)),
                    ),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: res_ref,
                    expr: decode_call(cur_ref_num),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: res_gen,
                    expr: decode_call(cur_gen_num),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: found,
                    expr: Expr::Bool(true),
                    line,
                },
            ],
            else_branch: vec![Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: prop_idx,
                expr: Expr::Add(Box::new(Expr::Var(prop_idx)), Box::new(Expr::Int(1))),
                line,
            }],
            line,
        },
    ];
    body.push(Stmt::While {
        condition: Expr::And(
            Box::new(Expr::Not(Box::new(Expr::Var(found)))),
            Box::new(Expr::Lt(
                Box::new(Lowerer::as_number(Expr::Var(prop_idx))),
                Box::new(Expr::Var(count_num)),
            )),
        ),
        body: per_property,
        line,
    });
    // Deferred capability guard: missing own property, prototype lookup and
    // function-object standard members never produce semantic Undefined.
    body.push(assert_stmt(Expr::Var(found), line));
    let args = vec!["carrier".to_owned(), "reference".to_owned()];
    ir.functions.push(FunctionDecl {
        name: REF_GET_HELPER.to_owned(),
        arity: 2,
        index,
        args: args.clone(),
        arg_schemas: vec![Some(carrier_schema), Some(value::reference_schema())],
        return_schema: Some(js_schema),
        type_params: Vec::new(),
        exported: false,
        return_type: ValueType::Map,
        symbol: None,
    });
    ir.function_impls.insert(
        index,
        FunctionImpl {
            param_slots: vec![param_carrier, param_ref],
            capture_copies: Vec::new(),
            body_stmts: body,
            body_expr: value::dynamic_value(
                Expr::Var(res_tag),
                Expr::Var(res_number),
                Expr::Var(res_flag),
                Expr::Var(res_ref),
                Expr::Var(res_gen),
            ),
            body_expr_line: line,
        },
    );
    statements.push(Stmt::FuncDecl {
        name: REF_GET_HELPER.to_owned(),
        index,
        arity: 2,
        args,
        exported: false,
        has_impl: true,
        line,
    });
    Ok(index)
}

/// Lower one admitted source-function body into a real FunctionDecl/Impl
/// with the fixed physical contract
/// `(Carrier, env: Int, this: JsValue, args: Array<JsValue>, mode: Int) ->
/// Completion`. Closed env 0 is an explicit empty marker; zero actual
/// arguments still pass a real empty typed vector. Every allocation uses
/// the runtime allocator; free lexical names reject via unknown-local.
fn append_source_function(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    collected: &CollectedFunction,
    flat_index: u16,
    carrier_schema: TypeSchema,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let line = lower.line(collected.span);
    let param_carrier = lower.slot(collected.span)?;
    let param_env = lower.slot(collected.span)?;
    let param_this = lower.slot(collected.span)?;
    let param_args = lower.slot(collected.span)?;
    let param_mode = lower.slot(collected.span)?;
    // Closed scope: stash the caller's bindings and lower with an empty
    // map so free variables cannot fall back to root locals.
    let saved_bindings = std::mem::take(&mut lower.bindings);
    let saved_this = lower.function_this.replace(param_this);
    let saved_carrier = lower.carrier_override.replace(param_carrier);
    let saved_strict = std::mem::replace(&mut lower.enclosing_strict, collected.strict);
    let body = collected
        .function
        .body
        .as_ref()
        .ok_or_else(|| lower.error(collected.span, "P0 source functions require a body"))?;
    let mut body_stmts: Vec<Stmt> = Vec::new();
    let mut completion: Option<Expr> = None;
    let mut in_prologue = true;
    for statement in &body.stmts {
        match statement {
            ast::Stmt::Expr(e) => {
                if in_prologue && matches!(e.expr.as_ref(), ast::Expr::Lit(ast::Lit::Str(_))) {
                    continue;
                }
                let err = lower.error(
                    statement.span(),
                    "P0 source-function bodies admit only directives plus Return or empty completion",
                );
                lower.bindings = saved_bindings;
                lower.function_this = saved_this;
                lower.carrier_override = saved_carrier;
                lower.enclosing_strict = saved_strict;
                return Err(err);
            }
            ast::Stmt::Empty(_) => {}
            ast::Stmt::Return(ret) => {
                in_prologue = false;
                if completion.is_some() {
                    let err = lower.error(
                        ret.span,
                        "P0 source-function bodies admit only a single Return",
                    );
                    lower.bindings = saved_bindings;
                    lower.function_this = saved_this;
                    lower.carrier_override = saved_carrier;
                    lower.enclosing_strict = saved_strict;
                    return Err(err);
                }
                match ret.arg.as_deref() {
                    Some(arg) => {
                        let val = match lower.expr(arg) {
                            Ok(v) => v,
                            Err(e) => {
                                lower.bindings = saved_bindings;
                                lower.function_this = saved_this;
                                lower.carrier_override = saved_carrier;
                                lower.enclosing_strict = saved_strict;
                                return Err(e);
                            }
                        };
                        let (val_slot, val_stmt) = match lower.bind_temp(val.expr, ret.span) {
                            Ok(v) => v,
                            Err(e) => {
                                lower.bindings = saved_bindings;
                                lower.function_this = saved_this;
                                lower.carrier_override = saved_carrier;
                                lower.enclosing_strict = saved_strict;
                                return Err(e);
                            }
                        };
                        body_stmts.push(val_stmt);
                        completion = Some(value::completion_return(Expr::Var(val_slot)));
                    }
                    None => {
                        completion = Some(value::completion_return(value::undefined()));
                    }
                }
            }
            _ => {
                let err = lower.error(
                    statement.span(),
                    "P0 source-function bodies admit only directives plus Return or empty completion",
                );
                lower.bindings = saved_bindings;
                lower.function_this = saved_this;
                lower.carrier_override = saved_carrier;
                lower.enclosing_strict = saved_strict;
                return Err(err);
            }
        }
    }
    lower.bindings = saved_bindings;
    lower.function_this = saved_this;
    lower.carrier_override = saved_carrier;
    lower.enclosing_strict = saved_strict;
    let body_expr = completion.unwrap_or_else(value::completion_normal);
    let js_schema = value::schema();
    let args_schema = TypeSchema::Array(Box::new(js_schema.clone()));
    let args = vec![
        "carrier".to_owned(),
        "env".to_owned(),
        "this_value".to_owned(),
        "args".to_owned(),
        "mode".to_owned(),
    ];
    ir.functions.push(FunctionDecl {
        name: format!("__compat_es5_fn_{}", collected.code_id),
        arity: 5,
        index: flat_index,
        args: args.clone(),
        arg_schemas: vec![
            Some(carrier_schema),
            Some(TypeSchema::Int),
            Some(js_schema),
            Some(args_schema),
            Some(TypeSchema::Int),
        ],
        return_schema: Some(value::completion_schema()),
        type_params: Vec::new(),
        exported: false,
        return_type: ValueType::Map,
        symbol: None,
    });
    ir.function_impls.insert(
        flat_index,
        FunctionImpl {
            param_slots: vec![param_carrier, param_env, param_this, param_args, param_mode],
            capture_copies: Vec::new(),
            body_stmts,
            body_expr,
            body_expr_line: line,
        },
    );
    statements.push(Stmt::FuncDecl {
        name: format!("__compat_es5_fn_{}", collected.code_id),
        index: flat_index,
        arity: 5,
        args,
        exported: false,
        has_impl: true,
        line,
    });
    let _ = span;
    Ok(())
}

/// Append the finite static dispatch helper
/// `(Carrier, func: JsValue, this: JsValue, args: Array<JsValue>) ->
/// Completion`. Decodes the function-instance code ID and calls the
/// statically compiled source function; unknown IDs and non-function
/// targets are capability guards, never silent Undefined.
#[allow(clippy::too_many_arguments)]
fn append_dispatch(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    helper_make: u16,
    helper_read: u16,
    decode: u16,
    func_indices: &[u16],
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let carrier_schema = ir
        .functions
        .iter()
        .find(|function| function.index == helper_make)
        .and_then(|function| function.return_schema.clone())
        .ok_or_else(|| {
            lower.error(
                span,
                "JS dispatch helper requires the carrier helper return schema",
            )
        })?;
    let index = ir
        .functions
        .iter()
        .map(|function| function.index)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| lower.error(span, "JS dispatch helper index space exhausted"))?;
    let line = lower.line(span);
    let param_carrier = lower.slot(span)?;
    let param_func = lower.slot(span)?;
    let param_this = lower.slot(span)?;
    let param_args = lower.slot(span)?;
    let func_base = lower.slot(span)?;
    let func_tag = lower.slot(span)?;
    let kind_num = lower.slot(span)?;
    let code_num = lower.slot(span)?;
    let code = lower.slot(span)?;
    let result = lower.slot(span)?;
    let js_schema = value::schema();
    let args_schema = TypeSchema::Array(Box::new(js_schema.clone()));
    let read_at = |base: u16, offset: i64| -> Expr {
        Lowerer::guest_call(
            helper_read,
            vec![
                Expr::Var(param_carrier),
                Lowerer::as_number(Expr::Add(
                    Box::new(Expr::Var(base)),
                    Box::new(Expr::Int(offset)),
                )),
            ],
        )
    };
    let mut body = vec![
        Stmt::Let {
            index: func_base,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field(param_func, "ref_slot"),
            line,
        },
        Stmt::Let {
            index: func_tag,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field(param_func, "tag"),
            line,
        },
        assert_stmt(
            Expr::Eq(
                Box::new(Expr::Var(func_tag)),
                Box::new(Expr::Int(value::OBJECT)),
            ),
            line,
        ),
        Stmt::Let {
            index: kind_num,
            declared_schema: Some(TypeSchema::Number),
            expr: read_at(func_base, 0),
            line,
        },
        assert_stmt(
            Expr::Eq(
                Box::new(Lowerer::as_number(Expr::Var(kind_num))),
                Box::new(Expr::Float(REALM_FUNCTION_KIND as f64)),
            ),
            line,
        ),
        Stmt::Let {
            index: code_num,
            declared_schema: Some(TypeSchema::Number),
            expr: read_at(func_base, 3),
            line,
        },
        Stmt::Let {
            index: code,
            declared_schema: Some(TypeSchema::Int),
            expr: Lowerer::guest_call(decode, vec![Expr::Var(code_num)]),
            line,
        },
        Stmt::Let {
            index: result,
            declared_schema: Some(value::completion_schema()),
            expr: value::completion_normal(),
            line,
        },
    ];
    // Finite branch table over static code IDs, last to first so the
    // outermost IfElse tests code 0.
    let mut tail: Vec<Stmt> = vec![assert_stmt(Expr::Bool(false), line)];
    for (code_id, func_index) in func_indices.iter().enumerate().rev() {
        let call = Lowerer::guest_call(
            *func_index,
            vec![
                Expr::Var(param_carrier),
                Expr::Int(0),
                Expr::Var(param_this),
                Expr::Var(param_args),
                Expr::Int(0),
            ],
        );
        tail = vec![Stmt::IfElse {
            condition: Expr::Eq(
                Box::new(Expr::Var(code)),
                Box::new(Expr::Int(code_id as i64)),
            ),
            then_branch: vec![Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: result,
                expr: call,
                line,
            }],
            else_branch: tail,
            line,
        }];
    }
    body.extend(tail);
    let args = vec![
        "carrier".to_owned(),
        "func".to_owned(),
        "this_value".to_owned(),
        "args".to_owned(),
    ];
    ir.functions.push(FunctionDecl {
        name: DISPATCH_HELPER.to_owned(),
        arity: 4,
        index,
        args: args.clone(),
        arg_schemas: vec![
            Some(carrier_schema),
            Some(js_schema.clone()),
            Some(js_schema),
            Some(args_schema),
        ],
        return_schema: Some(value::completion_schema()),
        type_params: Vec::new(),
        exported: false,
        return_type: ValueType::Map,
        symbol: None,
    });
    ir.function_impls.insert(
        index,
        FunctionImpl {
            param_slots: vec![param_carrier, param_func, param_this, param_args],
            capture_copies: Vec::new(),
            body_stmts: body,
            body_expr: Expr::Var(result),
            body_expr_line: line,
        },
    );
    statements.push(Stmt::FuncDecl {
        name: DISPATCH_HELPER.to_owned(),
        index,
        arity: 4,
        args,
        exported: false,
        has_impl: true,
        line,
    });
    Ok(index)
}

fn lower_array_binding(
    lower: &mut Lowerer<'_>,
    name: &str,
    literal: &ast::ArrayLit,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let carrier = lower.active_carrier(span)?;
    let heap = lower.heap_ref(span)?;
    let (helper_write, alloc) = (heap.helper_write, heap.alloc);
    // Reserve up front from the syntactic element count (holes and spread
    // still reject below) so budget excess fails before large emission.
    let total = REALM_HEADER_CELLS
        .checked_add(literal.elems.len() as i64)
        .ok_or_else(|| lower.error(span, "P0 numeric array length overflow"))?;
    let need =
        u64::try_from(total).map_err(|_| lower.error(span, "P0 numeric array length overflow"))?;
    let line = lower.line(span);
    let base_slot = lower.reserve_allocation(carrier, alloc, span, line, statements, need)?;
    let mut element_slots = Vec::new();
    for element in &literal.elems {
        let Some(element) = element else {
            return Err(lower.error(span, "P0 numeric arrays reject holes"));
        };
        if element.spread.is_some() {
            return Err(lower.error(element.span(), "P0 numeric arrays reject spread elements"));
        }
        let value = lower.expr(&element.expr)?;
        if value.kind != Kind::Number {
            return Err(lower.error(
                element.span(),
                "P0 numeric arrays admit only numeric elements",
            ));
        }
        let (slot, stmt) = lower.bind_temp(value.expr, element.span())?;
        statements.push(stmt);
        element_slots.push(slot);
    }
    let length = element_slots.len();
    for (offset, marker) in [
        (0i64, REALM_OBJECT_KIND),
        (1i64, REALM_GENERATION),
        (2i64, length as i64),
    ] {
        let physical = Expr::Add(Box::new(Expr::Var(base_slot)), Box::new(Expr::Int(offset)));
        let written = Lowerer::guest_call(
            helper_write,
            vec![
                Expr::Var(carrier),
                Lowerer::as_number(physical),
                Lowerer::as_number(Expr::Int(marker)),
            ],
        );
        statements.push(Stmt::Expr {
            expr: written,
            line: lower.line(span),
        });
    }
    for (position, element) in element_slots.iter().enumerate() {
        let offset = REALM_HEADER_CELLS + position as i64;
        let physical = Expr::Add(Box::new(Expr::Var(base_slot)), Box::new(Expr::Int(offset)));
        let written = Lowerer::guest_call(
            helper_write,
            vec![
                Expr::Var(carrier),
                Lowerer::as_number(physical),
                value::field(*element, "number"),
            ],
        );
        statements.push(Stmt::Expr {
            expr: written,
            line: lower.line(span),
        });
    }
    let boxed = value::array(Expr::Var(base_slot), Expr::Int(REALM_GENERATION));
    let (slot, stmt) = lower.bind_temp(boxed, span)?;
    lower.bindings.insert(name.to_owned(), (slot, Kind::Array));
    lower.array_lengths.insert(name.to_owned(), length);
    lower.source_bindings.push((name.to_owned(), slot));
    statements.push(stmt);
    Ok(())
}

fn lower_member_write_stmt(
    lower: &mut Lowerer<'_>,
    member: &ast::MemberExpr,
    right: &ast::Expr,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    // Plain-object property assignment is an explicit P7 capability
    // rejection: do not invent put semantics. Detect object-like receivers
    // first so `obj.x = v` reports the object diagnostic, not the array one.
    if !matches!(member.prop, ast::MemberProp::Computed(_)) {
        return Err(lower.error(
            member.span,
            "P0 plain objects reject property assignment until P7 descriptors",
        ));
    }
    if let ast::Expr::Ident(object) = member.obj.as_ref()
        && let Some(&(_, kind)) = lower.bindings.get(object.sym.as_str())
        && matches!(kind, Kind::Object | Kind::Function)
    {
        return Err(lower.error(
            member.span,
            "P0 plain objects reject property assignment until P7 descriptors",
        ));
    }
    // `this.x = v` inside a source function is the same deferred path.
    if matches!(member.obj.as_ref(), ast::Expr::This(_)) && lower.function_this.is_some() {
        return Err(lower.error(
            member.span,
            "P0 plain objects reject property assignment until P7 descriptors",
        ));
    }
    let ast::MemberProp::Computed(property) = &member.prop else {
        return Err(lower.error(
            member.span,
            "P0 numeric arrays admit only computed integer literal subscripts",
        ));
    };
    let Some(index) = Lowerer::member_index_literal(&property.expr) else {
        return Err(lower.error(
            property.span(),
            "P0 numeric arrays admit only integer literal subscripts",
        ));
    };
    let ast::Expr::Ident(object) = member.obj.as_ref() else {
        return Err(lower.error(
            member.span,
            "P0 numeric array writes require a local array binding",
        ));
    };
    let name = object.sym.as_str();
    let Some(&(slot, kind)) = lower.bindings.get(name) else {
        return Err(lower.error(object.span, &format!("unknown local '{name}'")));
    };
    if kind != Kind::Array {
        return Err(lower.error(
            member.span,
            "P0 indexed writes require a numeric-array binding",
        ));
    }
    let length = lower.array_lengths.get(name).copied().unwrap_or(0);
    let position = usize::try_from(index).unwrap_or(usize::MAX);
    if position >= length {
        return Err(lower.error(
            property.span(),
            "P0 numeric arrays reject length-changing writes",
        ));
    }
    let value = lower.expr(right)?;
    if value.kind != Kind::Number {
        return Err(lower.error(right.span(), "P0 numeric arrays store only numbers"));
    }
    let (value_slot, value_stmt) = lower.bind_temp(value.expr, right.span())?;
    statements.push(value_stmt);
    let carrier = lower.active_carrier(span)?;
    let heap = lower.heap_ref(span)?;
    let helper_write = heap.helper_write;
    let physical = Expr::Add(
        Box::new(value::field(slot, "ref_slot")),
        Box::new(Expr::Int(REALM_HEADER_CELLS + index)),
    );
    let written = Lowerer::guest_call(
        helper_write,
        vec![
            Expr::Var(carrier),
            Lowerer::as_number(physical),
            value::field(value_slot, "number"),
        ],
    );
    statements.push(Stmt::Expr {
        expr: written,
        line: lower.line(span),
    });
    Ok(())
}
