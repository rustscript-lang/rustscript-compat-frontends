//! A narrow primitive JS AST to existing typed IR bridge with one realm heap.
//!
//! Heap-free primitive sources keep the original independent path. When the
//! actual parsed AST requires array or string storage, the lowerer
//! bootstraps one real B1 realm (fixed accepted prelude, single owning
//! carrier with headers) and lowers literal/alias/read/mutate through the
//! same carrier. Private generated guest helpers reserve managed nodes,
//! register compiler roots and reclaim reusable extents. Logical slot plus
//! generation remains distinct from the physical backing address. No host
//! evaluator, no source substring matching.
use super::heap as layout;
use super::prelude::heap as heap_emitters;
use super::prelude::undefined_box;
use super::prelude::utf16_init;
use super::prelude::values as value_emitters;
use super::reference;
use super::representation as value;
use super::scope;
#[path = "expression.rs"]
pub(super) mod expression;
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
pub(super) enum Kind {
    /// An ordinary JsValue without a proven source kind.
    Dynamic,
    Undefined,
    Null,
    Boolean,
    Number,
    String,
    Array,
    Object,
    Function,
}

/// Bounded object/function admission: at most this many own properties per
/// literal and this many source functions per compilation unit.
const MAX_OBJECT_PROPERTIES: usize = 256;
const MAX_SOURCE_FUNCTIONS: usize = 256;
/// Private exact-Int decode helper `(Number) -> Int` factored from the
/// proven allocator bit logic. Converts bounded guest metadata cells
/// (tags, bases, code IDs) without invented casts; generations use the
/// separate UInt32-domain decoder.
const DECODE_INT_HELPER: &str = "__compat_es5_decode_int";
/// Bounded initialization: total header+payload cells reserved across the
/// whole source before large IR emission, including the known fixed
/// managed-layout prefix, known directory pages and compiler root frames.
/// Dynamic activation depth and repeated allocations remain runtime quotas.
const MAX_REALM_INITIAL_CELLS: u64 = 65536;

pub(super) struct Heap {
    pub carrier: u16,
    pub helper_read: u16,
    pub helper_write: u16,
    /// Flat indices of the managed heap core shared by every producer and
    /// consumer in this scope.
    pub helpers: heap_emitters::HeapHelpers,
}

/// One compiler-managed root scope per script/source-function activation:
/// the runtime frame slot plus the dense temp-to-entry layout. Metadata
/// scalars, label/code IDs and primitive boxes never take entries.
#[derive(Default)]
pub(super) struct RootScope {
    pub frame_slot: u16,
    pub entries: HashMap<u16, u16>,
    pub next_entry: u16,
}

/// One compiler-side pending allocation: the guest handle slot plus the
/// extracted Int fields the initializer writes through.
pub(super) struct Newborn {
    pub slot: u16,
    pub generation: u16,
    pub base: u16,
}

pub(super) type ObjectEntry = (Vec<u16>, Box<ast::Expr>, swc_common::Span);

pub(super) struct Lowered {
    pub(super) expr: Expr,
    pub(super) kind: Kind,
    /// Exact numeric-array length when this value is a known array.
    /// `None` is unknown, never a fabricated zero.
    pub(super) array_length: Option<usize>,
    /// Compiler temps whose root entries this value owns until a rooted
    /// destination takes the payload. Source bindings and activation
    /// caches are not listed here.
    pub(super) owned_roots: Vec<u16>,
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

pub(super) struct Lowerer<'a> {
    pub(super) source: &'a str,
    pub(super) source_id: u32,
    pub(super) base: u32,
    pub(super) next_slot: u16,
    pub(super) bindings: HashMap<String, (u16, Kind)>,
    pub(super) array_lengths: HashMap<String, usize>,
    pub(super) array_length_ids: HashMap<scope::BindingId, usize>,
    pub(super) source_bindings: Vec<(String, u16)>,
    pub(super) heap: Option<Heap>,
    /// Total header+payload cells reserved by literals so far. This static
    /// running total remains a whole-source budget cap: overflow/excess
    /// rejects before large emission. It never supplies runtime IDs; every
    /// literal base comes from the runtime allocator reading live backing
    /// length instead.
    init_cells: u64,
    /// Static allocation sites charged to the conservative initial budget.
    init_nodes: u64,
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
    pub(super) function_args: Option<u16>,
    /// Active carrier override inside function bodies (the carrier
    /// parameter slot). When `None`, the root realm carrier is used.
    carrier_override: Option<u16>,
    /// Active root-frame override inside function bodies (the frame base
    /// slot reserved for the activation). Mirrors the carrier override.
    frame_override: Option<u16>,
    /// Dense root layout of the activation currently being lowered.
    /// Saved and restored around nested source functions; absent on the
    /// heap-free scalar path, where no frame exists.
    pub(super) root_scope: Option<RootScope>,
    /// Flat indices of the private object/function helpers. Set on the
    /// realm path once appended; used by inline object/call lowering and
    /// by nested function bodies.
    pub(super) helper_decode: Option<u16>,
    helper_ref_get: Option<u16>,
    helper_dispatch: Option<u16>,
    /// Finite static code IDs for every admitted source-function AST node,
    /// keyed by absolute span `(lo, hi)`. Never source-text recognition.
    function_code_ids: HashMap<(u32, u32), i64>,
    /// Collected admitted functions in code-ID order for predeclaration.
    collected_functions: Vec<CollectedFunction>,
    pub(super) graph: scope::ScopeGraph,
    pub(super) active_scope: scope::ScopeId,
    pub(super) binding_slots: HashMap<scope::BindingId, u16>,
    pub(super) binding_kinds: HashMap<scope::BindingId, Kind>,
    pub(super) type_names: Option<reference::TypeNames>,
    /// Compiler-global UTF-16 initializer pool. Not saved around nested
    /// source functions: identical unit sequences share one helper across
    /// every activation in this compilation.
    pub(crate) utf16_inits: utf16_init::Utf16InitPool,
}

impl Lowerer<'_> {
    pub(super) fn line(&self, span: swc_common::Span) -> u32 {
        let offset = span.lo.0.saturating_sub(self.base) as usize;
        u32::try_from(
            self.source[..offset.min(self.source.len())]
                .matches('\n')
                .count()
                + 1,
        )
        .unwrap_or(u32::MAX)
    }

    pub(super) fn error(&self, span: swc_common::Span, message: &str) -> ParseError {
        let lo = span.lo.0.saturating_sub(self.base) as usize;
        let hi = span.hi.0.saturating_sub(self.base) as usize;
        ParseError::at_span(
            vm::compiler::source_map::Span::new(self.source_id, lo, hi),
            message,
        )
    }

    pub(super) fn slot(&mut self, span: swc_common::Span) -> Result<u16, ParseError> {
        let slot = self.next_slot;
        self.next_slot = slot
            .checked_add(1)
            .ok_or_else(|| self.error(span, "JavaScript ES5 local slot limit exceeded"))?;
        Ok(slot)
    }

    /// Dedup root-token identity only. Two entries for one box both stay
    /// when they are distinct slots; a slot listed twice is one token.
    pub(super) fn compose_owned(parts: impl IntoIterator<Item = u16>) -> Vec<u16> {
        let mut owned = Vec::new();
        for slot in parts {
            if !owned.contains(&slot) {
                owned.push(slot);
            }
        }
        owned
    }

    pub(super) fn bind_temp(
        &mut self,
        expr: Expr,
        span: swc_common::Span,
    ) -> Result<(u16, Vec<Stmt>), ParseError> {
        self.bind_rooted(expr, value::schema(), span)
    }

    /// Proven primitive boxes need physical storage for evaluation order,
    /// but have no heap edge to register or clear.
    pub(super) fn bind_evaluation(
        &mut self,
        expr: Expr,
        kind: Kind,
        span: swc_common::Span,
    ) -> Result<(u16, Vec<Stmt>), ParseError> {
        if matches!(
            kind,
            Kind::Undefined | Kind::Null | Kind::Boolean | Kind::Number
        ) {
            let (slot, stmt) = self.bind_temp_bare(expr, span)?;
            Ok((slot, vec![stmt]))
        } else {
            self.bind_temp(expr, span)
        }
    }

    /// Bind a boxed value with NO root entry: dropped statement temporaries
    /// and synthetic primitive boxes whose lifetime the surrounding roots
    /// already cover. Unrooted newborns stay collectable by design.
    pub(super) fn bind_temp_bare(
        &mut self,
        expr: Expr,
        span: swc_common::Span,
    ) -> Result<(u16, Stmt), ParseError> {
        let slot = self.slot(span)?;
        Ok((
            slot,
            Stmt::Let {
                index: slot,
                declared_schema: Some(value::schema()),
                expr,
                line: self.line(span),
            },
        ))
    }

    /// Bind a boxed value followed immediately by its no-allocation root
    /// write into this activation's frame. The heap-free scalar path has no
    /// frame and binds bare.
    pub(super) fn has_root(&self, slot: u16) -> bool {
        self.root_scope
            .as_ref()
            .is_some_and(|scope| scope.entries.contains_key(&slot))
    }

    /// Refresh an existing binding/temp root. Heap-free activations have no
    /// frame, so this is a no-op there. The caller must place this immediately
    /// after the Assign that stored the new box.
    pub(super) fn refresh_root(
        &mut self,
        slot: u16,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        self.write_root_value(slot, Expr::Var(slot), span, stmts)
    }

    /// Drop only the logical edge. The physical box may remain as an
    /// intentionally unrooted value; no later semantic use may depend on it.
    pub(super) fn clear_root(
        &mut self,
        slot: u16,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        if !self.has_root(slot) {
            return Ok(());
        }
        self.write_root_value(slot, value::undefined(), span, stmts)
    }

    fn write_root_value(
        &mut self,
        slot: u16,
        boxed: Expr,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        if self.root_scope.is_none() {
            return Ok(());
        }
        let carrier = self.active_carrier(span)?;
        let root_write = self.heap_ref(span)?.helpers.root_write;
        let frame = self.active_frame(span)?;
        let entry = self.entry_for(slot, span)?;
        let ack = self.slot(span)?;
        let line = self.line(span);
        stmts.push(Stmt::Let {
            index: ack,
            declared_schema: Some(TypeSchema::Int),
            expr: Self::guest_call(
                root_write,
                vec![
                    Expr::Var(carrier),
                    Expr::Var(frame),
                    Expr::Int(i64::from(entry)),
                    boxed,
                ],
            ),
            line,
        });
        Ok(())
    }

    /// Publish a newborn into an already-instantiated binding slot so the
    /// predeclared root entry is reused instead of retaining a second object.
    pub(super) fn publish_into(
        &mut self,
        dest: u16,
        carrier: u16,
        newborn: &Newborn,
        span: swc_common::Span,
        line: u32,
        stmts: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let helpers = self.heap_ref(span)?.helpers;
        let frame = self.active_frame(span)?;
        let entry = self.entry_for(dest, span)?;
        // Materialize the publish call in a Let. Assigning a host call
        // directly is a shape this native backend does not terminate on.
        let published = self.slot(span)?;
        stmts.push(Stmt::Let {
            index: published,
            declared_schema: Some(value::schema()),
            expr: Self::guest_call(
                helpers.node_publish,
                vec![
                    Expr::Var(carrier),
                    Expr::Var(newborn.slot),
                    Expr::Var(newborn.generation),
                    Expr::Var(newborn.base),
                    Expr::Var(frame),
                    Expr::Int(i64::from(entry)),
                ],
            ),
            line,
        });
        stmts.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: dest,
            expr: Expr::Var(published),
            line,
        });
        self.refresh_root(dest, span, stmts)
    }

    fn bind_rooted(
        &mut self,
        expr: Expr,
        schema: TypeSchema,
        span: swc_common::Span,
    ) -> Result<(u16, Vec<Stmt>), ParseError> {
        let slot = self.slot(span)?;
        let line = self.line(span);
        let mut stmts = vec![Stmt::Let {
            index: slot,
            declared_schema: Some(schema),
            expr,
            line,
        }];
        if self.root_scope.is_some() {
            let carrier = self.active_carrier(span)?;
            let root_write = self.heap_ref(span)?.helpers.root_write;
            let frame = self.active_frame(span)?;
            let entry = self.entry_for(slot, span)?;
            let ack = self.slot(span)?;
            stmts.push(Stmt::Let {
                index: ack,
                declared_schema: Some(TypeSchema::Int),
                expr: Self::guest_call(
                    root_write,
                    vec![
                        Expr::Var(carrier),
                        Expr::Var(frame),
                        Expr::Int(i64::from(entry)),
                        Expr::Var(slot),
                    ],
                ),
                line,
            });
        }
        Ok((slot, stmts))
    }

    /// Reserve one root entry for a temp slot in the current activation.
    fn entry_for(&mut self, slot: u16, span: swc_common::Span) -> Result<u16, ParseError> {
        let source_id = self.source_id;
        if self.root_scope.is_none() {
            return Err(self.error(span, "JavaScript ES5 roots require an activation frame"));
        }
        let scope = self.root_scope.as_mut().expect("scope checked above");
        if let Some(&entry) = scope.entries.get(&slot) {
            return Ok(entry);
        }
        let entry = scope.next_entry;
        scope.next_entry = entry.checked_add(1).ok_or_else(|| {
            ParseError::at_span(
                vm::compiler::source_map::Span::new(source_id, 0, 0),
                "JavaScript ES5 root entry space exhausted",
            )
        })?;
        scope.entries.insert(slot, entry);
        self.charge_initial_cells(layout::FRAME_CELLS_PER_ENTRY as u64, span)?;
        Ok(entry)
    }

    fn charge_initial_cells(
        &mut self,
        cells: u64,
        span: swc_common::Span,
    ) -> Result<(), ParseError> {
        self.init_cells = self
            .init_cells
            .checked_add(cells)
            .filter(|end| *end <= MAX_REALM_INITIAL_CELLS)
            .ok_or_else(|| {
                self.error(
                    span,
                    "JavaScript ES5 realm initialization exceeds the 65536 cell budget",
                )
            })?;
        Ok(())
    }

    /// Reserve one node allocation: static budget charge plus the runtime
    /// reservation call. The caller initializes header/payload cells with
    /// numeric writes only, then publishes before any nested allocation.
    pub(super) fn begin_node(
        &mut self,
        carrier: u16,
        count: i64,
        kind: i64,
        span: swc_common::Span,
        line: u32,
        stmts: &mut Vec<Stmt>,
    ) -> Result<Newborn, ParseError> {
        let helpers = self.heap_ref(span)?.helpers;
        let cells = u64::try_from(count)
            .map_err(|_| self.error(span, "JavaScript ES5 realm allocation count overflow"))?;
        let need = cells
            .checked_add(layout::RAW_HEADER_CELLS as u64)
            .ok_or_else(|| self.error(span, "JavaScript ES5 realm allocation count overflow"))?;
        let page_cells = if self.init_nodes.is_multiple_of(layout::PAGE_ROWS as u64) {
            layout::DIR_PAGE_CELLS as u64
        } else {
            0
        };
        self.charge_initial_cells(
            need.checked_add(page_cells).ok_or_else(|| {
                self.error(
                    span,
                    "JavaScript ES5 realm initialization cell count overflow",
                )
            })?,
            span,
        )?;
        self.init_nodes = self.init_nodes.checked_add(1).ok_or_else(|| {
            self.error(span, "JavaScript ES5 realm allocation site count overflow")
        })?;
        let alloc = self.slot(span)?;
        stmts.push(Stmt::Let {
            index: alloc,
            declared_schema: Some(value::allocation_schema()),
            expr: Self::guest_call(
                helpers.node_begin,
                vec![Expr::Var(carrier), Expr::Int(count), Expr::Int(kind)],
            ),
            line,
        });
        let slot = self.slot(span)?;
        stmts.push(Stmt::Let {
            index: slot,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field_of(Expr::Var(alloc), "slot"),
            line,
        });
        let generation = self.slot(span)?;
        stmts.push(Stmt::Let {
            index: generation,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field_of(Expr::Var(alloc), "generation"),
            line,
        });
        let base = self.slot(span)?;
        stmts.push(Stmt::Let {
            index: base,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field_of(Expr::Var(alloc), "base"),
            line,
        });
        Ok(Newborn {
            slot,
            generation,
            base,
        })
    }

    /// Publish one initialized node: validates the pending allocation,
    /// writes the newborn root directly into a fresh frame entry, activates
    /// and returns the real boxed value already rooted.
    pub(super) fn publish_node(
        &mut self,
        carrier: u16,
        newborn: &Newborn,
        span: swc_common::Span,
        line: u32,
        stmts: &mut Vec<Stmt>,
    ) -> Result<u16, ParseError> {
        let helpers = self.heap_ref(span)?.helpers;
        let frame = self.active_frame(span)?;
        let boxed = self.slot(span)?;
        let entry = self.entry_for(boxed, span)?;
        stmts.push(Stmt::Let {
            index: boxed,
            declared_schema: Some(value::schema()),
            expr: Self::guest_call(
                helpers.node_publish,
                vec![
                    Expr::Var(carrier),
                    Expr::Var(newborn.slot),
                    Expr::Var(newborn.generation),
                    Expr::Var(newborn.base),
                    Expr::Var(frame),
                    Expr::Int(i64::from(entry)),
                ],
            ),
            line,
        });
        Ok(boxed)
    }

    /// Emit one consumed numeric cell write through the realm write helper.
    pub(super) fn emit_cell_write(
        &mut self,
        carrier: u16,
        addr: Expr,
        payload: Expr,
        span: swc_common::Span,
        line: u32,
        stmts: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let heap = self.heap_ref(span)?;
        let helper_write = heap.helper_write;
        let ack = self.slot(span)?;
        stmts.push(Stmt::Let {
            index: ack,
            declared_schema: Some(TypeSchema::Number),
            expr: Self::guest_call(
                helper_write,
                vec![Expr::Var(carrier), Self::as_number(addr), payload],
            ),
            line,
        });
        Ok(())
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

    pub(super) fn builtin_call(index: u16, args: Vec<Expr>) -> Expr {
        Expr::Call(index, Vec::new(), args, None, None)
    }

    pub(super) fn guest_call(index: u16, args: Vec<Expr>) -> Expr {
        Expr::Call(index, Vec::new(), args, None, None)
    }

    pub(super) fn as_number(expr: Expr) -> Expr {
        Expr::Div(Box::new(expr), Box::new(Expr::Float(1.0)))
    }

    pub(super) fn heap_ref(&self, span: swc_common::Span) -> Result<&Heap, ParseError> {
        self.heap
            .as_ref()
            .ok_or_else(|| self.error(span, "JavaScript ES5 numeric arrays require the realm heap"))
    }

    /// Active owning carrier slot: the function-body parameter inside
    /// source functions, otherwise the root realm carrier.
    pub(super) fn active_carrier(&self, span: swc_common::Span) -> Result<u16, ParseError> {
        if let Some(carrier) = self.carrier_override {
            return Ok(carrier);
        }
        Ok(self.heap_ref(span)?.carrier)
    }

    /// Active root-frame slot: the function-body frame inside source
    /// functions, otherwise the script frame reserved for the activation.
    fn active_frame(&self, span: swc_common::Span) -> Result<u16, ParseError> {
        if let Some(frame) = self.frame_override {
            return Ok(frame);
        }
        self.root_scope
            .as_ref()
            .map(|scope| scope.frame_slot)
            .ok_or_else(|| self.error(span, "JavaScript ES5 roots require an activation frame"))
    }

    /// Exact raw `"use strict"` directive check over the literal lexeme.
    /// Only the two exact spellings admit; escaped lookalikes
    /// (`"\u0075se strict"`, `"\\u2028"` text, braced escapes) reject
    /// because their raw bytes differ.
    pub(super) fn is_exact_use_strict_lexeme(&self, span: swc_common::Span) -> bool {
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

    pub(super) fn member_index_literal(property: &ast::Expr) -> Option<i64> {
        match property {
            ast::Expr::Lit(ast::Lit::Num(n)) => Self::literal_index(n.value),
            ast::Expr::Paren(p) => Self::member_index_literal(&p.expr),
            _ => None,
        }
    }

    pub(super) fn expr(&mut self, expression: &ast::Expr) -> Result<Lowered, ParseError> {
        match expression {
            ast::Expr::Lit(ast::Lit::Num(n)) => Ok(Lowered {
                expr: value::number(Expr::Float(n.value)),
                kind: Kind::Number,
                array_length: None,
                owned_roots: Vec::new(),
            }),
            ast::Expr::Lit(ast::Lit::Null(_)) => Ok(Lowered {
                expr: value::null(),
                kind: Kind::Null,
                array_length: None,
                owned_roots: Vec::new(),
            }),
            ast::Expr::Lit(ast::Lit::Bool(b)) => Ok(Lowered {
                expr: value::boolean(Expr::Bool(b.value)),
                kind: Kind::Boolean,
                array_length: None,
                owned_roots: Vec::new(),
            }),
            ast::Expr::Lit(ast::Lit::Str(s)) => self.lower_string_literal(s),
            ast::Expr::Paren(p) => self.expr(&p.expr),
            ast::Expr::Ident(i) => reference::lower_get(self, i),
            ast::Expr::Assign(assign) if assign.op == ast::AssignOp::Assign => {
                reference::lower_assign_expr(self, assign)
            }
            ast::Expr::Unary(u) if u.op == ast::UnaryOp::TypeOf => reference::lower_typeof(self, u),
            ast::Expr::Unary(u) if u.op == ast::UnaryOp::Bang => self.lower_not(u),
            ast::Expr::Bin(b)
                if matches!(b.op, ast::BinaryOp::LogicalAnd | ast::BinaryOp::LogicalOr) =>
            {
                self.lower_logical(b)
            }
            ast::Expr::Array(literal) => self.lower_array_expr(literal),
            ast::Expr::Unary(u) if u.op == ast::UnaryOp::Minus => {
                let operand = self.expr(&u.arg)?;
                reference::admit_numeric_operand(self, u.span, operand.kind)?;
                let (slot, statement) = self.bind_evaluation(operand.expr, operand.kind, u.span)?;
                Ok(Lowered {
                    expr: Expr::Block {
                        stmts: statement,
                        expr: Box::new(value::number(Expr::Neg(Box::new(value::field(
                            slot, "number",
                        ))))),
                    },
                    kind: Kind::Number,
                    array_length: None,
                    owned_roots: operand.owned_roots,
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
                if !equality {
                    reference::admit_numeric_operand(self, b.span, left.kind)?;
                    reference::admit_numeric_operand(self, b.span, right.kind)?;
                }
                let mut owned_roots = left.owned_roots;
                owned_roots.extend(right.owned_roots);
                let (l, ls) = self.bind_evaluation(left.expr, left.kind, b.left.span())?;
                let (r, rs) = self.bind_evaluation(right.expr, right.kind, b.right.span())?;
                owned_roots.extend([l, r].into_iter().filter(|slot| self.has_root(*slot)));
                let owned_roots = Self::compose_owned(owned_roots);
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
                                stmts: {
                                    let mut combined = [ls, rs].concat();
                                    combined.push(flag_stmt);
                                    combined
                                },
                                expr: Box::new(value::boolean(Expr::Not(Box::new(Expr::Var(
                                    flag_slot,
                                ))))),
                            },
                            kind: Kind::Boolean,
                            array_length: None,
                            owned_roots,
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
                        stmts: [ls, rs].concat(),
                        expr: Box::new(result),
                    },
                    kind: if equality {
                        Kind::Boolean
                    } else {
                        Kind::Number
                    },
                    array_length: None,
                    owned_roots,
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
        // binding preserves the accepted behavior exactly, now through the
        // checked generation/kind/length gate before the element is read.
        if let ast::MemberProp::Computed(property) = &member.prop
            && let Some(index) = Self::member_index_literal(&property.expr)
            && let ast::Expr::Ident(object) = member.obj.as_ref()
            && let Some(&(slot, kind)) = self.bindings.get(object.sym.as_str())
            && kind == Kind::Array
        {
            let name = object.sym.as_str();
            let length = self.array_lengths.get(name).copied();
            reference::admit_member_read(self, member, kind, length)?;
            let position = usize::try_from(index).unwrap_or(usize::MAX);
            if length.is_some_and(|length| position >= length) {
                return Ok(Lowered {
                    expr: value::undefined(),
                    kind: Kind::Undefined,
                    array_length: None,
                    owned_roots: Vec::new(),
                });
            }
            let carrier = self.active_carrier(member.span)?;
            let heap = self.heap_ref(member.span)?;
            let (helper_read, checked_base) = (heap.helper_read, heap.helpers.checked_base);
            let decode = self.helper_decode.ok_or_else(|| {
                self.error(
                    member.span,
                    "P0 array reads require the realm decode helper",
                )
            })?;
            let line = self.line(member.span);
            // Preserve the source binding for subsequent accesses; each
            // access reads its identity fields once from a rooted copy.
            let (receiver, mut stmts) = self.bind_temp(Expr::Var(slot), member.obj.span())?;
            let base_tmp = self.slot(member.span)?;
            stmts.push(Stmt::Let {
                index: base_tmp,
                declared_schema: Some(TypeSchema::Int),
                expr: Self::guest_call(
                    checked_base,
                    vec![
                        Expr::Var(carrier),
                        value::field(receiver, "ref_slot"),
                        value::field(receiver, "generation"),
                    ],
                ),
                line,
            });
            let kind_tmp = self.slot(member.span)?;
            stmts.push(Stmt::Let {
                index: kind_tmp,
                declared_schema: Some(TypeSchema::Int),
                expr: Self::guest_call(
                    decode,
                    vec![Self::guest_call(
                        helper_read,
                        vec![Expr::Var(carrier), Self::as_number(Expr::Var(base_tmp))],
                    )],
                ),
                line,
            });
            stmts.push(assert_stmt(
                Expr::Eq(
                    Box::new(Expr::Var(kind_tmp)),
                    Box::new(Expr::Int(layout::KIND_ARRAY)),
                ),
                line,
            ));
            let len_tmp = self.slot(member.span)?;
            stmts.push(Stmt::Let {
                index: len_tmp,
                declared_schema: Some(TypeSchema::Int),
                expr: Self::guest_call(
                    decode,
                    vec![Self::guest_call(
                        helper_read,
                        vec![
                            Expr::Var(carrier),
                            Self::as_number(Expr::Add(
                                Box::new(Expr::Var(base_tmp)),
                                Box::new(Expr::Int(2)),
                            )),
                        ],
                    )],
                ),
                line,
            });
            stmts.push(assert_stmt(
                Expr::Lt(Box::new(Expr::Int(index)), Box::new(Expr::Var(len_tmp))),
                line,
            ));
            let physical = Expr::Add(
                Box::new(Expr::Var(base_tmp)),
                Box::new(Expr::Int(layout::ARRAY_HEADER_CELLS + index)),
            );
            let read = Self::guest_call(
                helper_read,
                vec![Expr::Var(carrier), Self::as_number(physical)],
            );
            return Ok(Lowered {
                expr: Expr::Block {
                    stmts,
                    expr: Box::new(value::number(Self::as_number(read))),
                },
                kind: Kind::Number,
                array_length: None,
                owned_roots: vec![receiver],
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

    /// Re-affirm one already-registered root entry with a no-allocation
    /// write: used right after materializing a Reference so both the
    /// receiver and the key are pinned by the same statement sequence that
    /// consumes them.
    fn reaffirm_root(
        &mut self,
        carrier: u16,
        slot: u16,
        span: swc_common::Span,
        line: u32,
        stmts: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let helpers = self.heap_ref(span)?.helpers;
        let frame = self.active_frame(span)?;
        let entry = self.entry_for(slot, span)?;
        let ack = self.slot(span)?;
        stmts.push(Stmt::Let {
            index: ack,
            declared_schema: Some(TypeSchema::Int),
            expr: Self::guest_call(
                helpers.root_write,
                vec![
                    Expr::Var(carrier),
                    Expr::Var(frame),
                    Expr::Int(i64::from(entry)),
                    Expr::Var(slot),
                ],
            ),
            line,
        });
        Ok(())
    }

    /// Operational own-data-property read through a real Reference:
    /// evaluate base once, allocate the UTF-16 key once, materialize
    /// `{kind, base, key_slot, key_generation, strict}` with caller
    /// strictness, then GetValue through the actual heap object.
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
        let result_kind = reference::admit_member_read(self, member, base.kind, base.array_length)?;
        let mut owned_roots = base.owned_roots;
        let (base_slot, base_stmt) = self.bind_temp(base.expr, member.obj.span())?;
        owned_roots.push(base_slot);
        let key_units = self.member_key_units(member)?;
        let mut stmts = base_stmt;
        // The receiver root precedes the member key allocation; the key
        // publishes into its own entry before GetValue can run.
        let key_box = self.emit_utf16_node(&key_units, member.span, &mut stmts)?;
        owned_roots.push(key_box);
        let strict = Expr::Bool(self.enclosing_strict);
        let reference = value::reference(
            Expr::Var(base_slot),
            value::field(key_box, "ref_slot"),
            value::field(key_box, "generation"),
            strict,
        );
        let (ref_slot, ref_stmt) =
            self.bind_typed(reference, value::reference_schema(), member.span)?;
        stmts.push(ref_stmt);
        let line = self.line(member.span);
        self.reaffirm_root(carrier, base_slot, member.span, line, &mut stmts)?;
        self.reaffirm_root(carrier, key_box, member.span, line, &mut stmts)?;
        let got = Self::guest_call(ref_get, vec![Expr::Var(carrier), Expr::Var(ref_slot)]);
        let (value_slot, mut value_stmt) = self.bind_temp(got, member.span)?;
        stmts.append(&mut value_stmt);
        owned_roots.push(value_slot);
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(Expr::Var(value_slot)),
            },
            // Preserve the established broad own-property classification;
            // the checked intrinsic function length has a numeric fact.
            kind: result_kind,
            array_length: None,
            owned_roots: Self::compose_owned(owned_roots),
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

    /// Evaluate and root the callee/receiver before any actual expression.
    fn lower_call_expr(&mut self, call: &ast::CallExpr) -> Result<Lowered, ParseError> {
        reference::admit_call_callee(self, call)?;
        let ast::Callee::Expr(callee) = &call.callee else {
            unreachable!()
        };
        let span = call.span;
        let carrier = self.active_carrier(span)?;
        let mut owned = Vec::new();
        let mut stmts = Vec::new();
        let (func, receiver) = match Self::unwrap_parens(callee) {
            ast::Expr::Ident(ident) => {
                let callee = reference::lower_get(self, ident)?;
                reference::admit_callable_kind(self, ident.span, callee.kind)?;
                let (func, mut saved) = self.bind_temp(callee.expr, span)?;
                stmts.append(&mut saved);
                owned.extend(callee.owned_roots);
                owned.push(func);
                let (receiver, saved) = self.bind_temp_bare(value::undefined(), span)?;
                stmts.push(saved);
                (func, receiver)
            }
            ast::Expr::Member(member) => {
                let base = self.expr(&member.obj)?;
                reference::admit_object_receiver(self, member.span, base.kind)?;
                let callee_kind =
                    reference::admit_member_read(self, member, base.kind, base.array_length)?;
                reference::admit_callable_kind(self, member.span, callee_kind)?;
                owned.extend(base.owned_roots);
                let (receiver, mut saved) = self.bind_temp(base.expr, span)?;
                stmts.append(&mut saved);
                owned.push(receiver);
                let key = self.member_key_units(member)?;
                let key = self.emit_utf16_node(&key, span, &mut stmts)?;
                owned.push(key);
                let reference = value::reference(
                    Expr::Var(receiver),
                    value::field(key, "ref_slot"),
                    value::field(key, "generation"),
                    Expr::Bool(self.enclosing_strict),
                );
                let (reference, saved) =
                    self.bind_typed(reference, value::reference_schema(), span)?;
                stmts.push(saved);
                let get = self.helper_ref_get.ok_or_else(|| {
                    self.error(span, "P0 method calls require the realm reference helper")
                })?;
                let got = Self::guest_call(get, vec![Expr::Var(carrier), Expr::Var(reference)]);
                let (func, mut saved) = self.bind_temp(got, span)?;
                stmts.append(&mut saved);
                owned.push(func);
                (func, receiver)
            }
            _ => unreachable!("shared callee admission"),
        };
        // The Core container does not trace guest payloads. Save/root each
        // actual independently, including extras, across subsequent effects.
        let mut actuals = Vec::new();
        for arg in &call.args {
            let actual = self.expr(&arg.expr)?;
            owned.extend(actual.owned_roots);
            let (slot, mut saved) = self.bind_temp(actual.expr, arg.span())?;
            stmts.append(&mut saved);
            owned.push(slot);
            actuals.push(slot);
        }
        let (vector, saved) = self.bind_typed(
            Self::builtin_call(BuiltinFunction::ArrayNew.call_index(), Vec::new()),
            TypeSchema::Array(Box::new(value::schema())),
            span,
        )?;
        stmts.push(saved);
        for actual in actuals {
            stmts.push(Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: vector,
                expr: Self::builtin_call(
                    BuiltinFunction::ArrayPush.call_index(),
                    vec![Expr::Var(vector), Expr::Var(actual)],
                ),
                line: self.line(span),
            });
        }
        let dispatch = self.helper_dispatch.ok_or_else(|| {
            self.error(span, "P0 function calls require the realm dispatch helper")
        })?;
        let completed = Self::guest_call(
            dispatch,
            vec![
                Expr::Var(carrier),
                Expr::Var(func),
                Expr::Var(receiver),
                Expr::Var(vector),
            ],
        );
        let (completion, saved) = self.bind_typed(completed, value::completion_schema(), span)?;
        stmts.push(saved);
        // root_leave is nonallocating. Root the surviving result FIRST, then
        // release only evaluation-owned metadata, never caller bindings.
        let (result, mut saved) = self.bind_temp(value::field(completion, "value"), span)?;
        stmts.append(&mut saved);
        // A late nonallocating use keeps the exact Core vector (including
        // extras) alive through completion-value handoff, not just its boxes.
        let (_, retained) = self.bind_typed(
            Self::builtin_call(BuiltinFunction::Len.call_index(), vec![Expr::Var(vector)]),
            TypeSchema::Int,
            span,
        )?;
        stmts.push(retained);
        reference::release_consumed_roots(self, &owned, result, span, &mut stmts)?;
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(Expr::Var(result)),
            },
            kind: Kind::Object,
            array_length: None,
            owned_roots: vec![result],
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

    /// Reject a provably excessive direct-binding literal total before any
    /// large literal emits locals. This is a lower-bound preflight, not a
    /// replacement for exact node/root/directory reservation checks.
    fn preflight_binding_literals(&self, script: &ast::Script) -> Result<(), ParseError> {
        let mut cells = (layout::PREFIX_CELLS + layout::FRAME_FIXED_CELLS) as u64;
        let mut nodes = 0u64;
        for statement in &script.body {
            let ast::Stmt::Decl(ast::Decl::Var(declaration)) = statement else {
                continue;
            };
            for binding in &declaration.decls {
                let Some(mut expression) = binding.init.as_deref() else {
                    continue;
                };
                while let ast::Expr::Paren(parenthesized) = expression {
                    expression = &parenthesized.expr;
                }
                let data_cells = match expression {
                    ast::Expr::Array(array) => {
                        array.elems.len() as u64 + layout::ARRAY_HEADER_CELLS as u64
                    }
                    ast::Expr::Lit(ast::Lit::Str(string)) => {
                        self.decode_string_token(string.span)?.len() as u64
                            + layout::STRING_HEADER_CELLS as u64
                    }
                    _ => continue,
                };
                // Minimum known roots: one publication entry per node and
                // an additional boxed binding entry for string expressions.
                let roots = if matches!(expression, ast::Expr::Array(_)) {
                    1
                } else {
                    2
                };
                let metadata = roots * layout::FRAME_CELLS_PER_ENTRY as u64
                    + if nodes.is_multiple_of(layout::PAGE_ROWS as u64) {
                        layout::DIR_PAGE_CELLS as u64
                    } else {
                        0
                    };
                nodes += 1;
                cells = cells
                    .checked_add(metadata)
                    .and_then(|total| total.checked_add(data_cells))
                    .and_then(|total| total.checked_add(layout::RAW_HEADER_CELLS as u64))
                    .filter(|total| *total <= MAX_REALM_INITIAL_CELLS)
                    .ok_or_else(|| {
                        self.error(
                            expression.span(),
                            "JavaScript ES5 realm initialization exceeds the 65536 cell budget",
                        )
                    })?;
            }
        }
        Ok(())
    }

    /// Lower a string literal to an immutable realm value. Units come from
    /// the shared original-source codec over the literal's own span bytes
    /// (never the parser cooked payload), capped by the codec at one unit
    /// past the bound so oversized literals reject before emission.
    /// The logical binding copies this value by alias without payload copy.
    fn lower_string_literal(&mut self, literal: &ast::Str) -> Result<Lowered, ParseError> {
        let span = literal.span;
        let units = self.decode_string_token(span)?;
        let mut stmts = Vec::new();
        let boxed = self.emit_utf16_node(&units, span, &mut stmts)?;
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(Expr::Var(boxed)),
            },
            kind: Kind::String,
            array_length: None,
            owned_roots: vec![boxed],
        })
    }

    /// Emit one UTF-16 string node (header + units) into `statements`,
    /// returning the rooted boxed value slot. Shared by string values and
    /// by object/member key nodes so keys are real guest strings. Kind and
    /// generation are stamped by reservation; only length plus units are
    /// initialized here, then the node publishes into its frame entry.
    pub(super) fn emit_utf16_node(
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
        let count = layout::STRING_HEADER_CELLS
            .checked_add(length)
            .ok_or_else(|| self.error(span, "JavaScript ES5 string length overflow"))?;
        let carrier = self.active_carrier(span)?;
        let line = self.line(span);
        let newborn =
            self.begin_node(carrier, count, layout::KIND_STRING, span, line, statements)?;
        if let Some(helper) = utf16_init::intern(&mut self.utf16_inits, units) {
            // Consume the raw-base result in a typed Let. The call is not a
            // Stmt::Expr, so a surrounding branch does not leave a stack value.
            let returned = self.slot(span)?;
            statements.push(Stmt::Let {
                index: returned,
                declared_schema: Some(TypeSchema::Int),
                expr: Self::guest_call(helper, vec![Expr::Var(carrier), Expr::Var(newborn.base)]),
                line,
            });
        } else {
            utf16_init::emit_initializer_writes(
                self,
                carrier,
                newborn.base,
                units,
                span,
                line,
                statements,
            )?;
        }
        let boxed = self.publish_node(carrier, &newborn, span, line, statements)?;
        Ok(boxed)
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
            array_length: None,
            owned_roots: Vec::new(),
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
    pub(super) fn object_key_units(
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
    pub(super) fn member_key_units(
        &self,
        member: &ast::MemberExpr,
    ) -> Result<Vec<u16>, ParseError> {
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

    pub(super) fn object_literal_entries(
        &self,
        literal: &ast::ObjectLit,
    ) -> Result<Vec<ObjectEntry>, ParseError> {
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
        Ok(entries)
    }

    fn lower_object_literal(&mut self, literal: &ast::ObjectLit) -> Result<Lowered, ParseError> {
        let entries = self.object_literal_entries(literal)?;
        let count = entries.len() as i64;
        let line = self.line(literal.span);
        let mut stmts: Vec<Stmt> = Vec::new();
        // Allocate key string nodes first so every property key is a real
        // UTF-16 guest string compared by content at runtime. Each key
        // publishes into its own entry before the containing allocation.
        let mut owned_roots = Vec::new();
        let mut key_boxes: Vec<u16> = Vec::new();
        for (units, _, key_span) in &entries {
            // Keys borrow the literal span for resource diagnostics.
            let boxed = self.emit_utf16_node(units, *key_span, &mut stmts)?;
            key_boxes.push(boxed);
            // Key publication joins owned roots before the parent is assembled.
            // Parent edges keep the key after binding handoff clears this token.
            owned_roots.push(boxed);
        }
        // Lower values (numbers, strings, objects, functions, member
        // reads, aliases). Closed-function bodies were prevalidated; free
        // names reject here through ordinary unknown-local diagnostics.
        // Every value roots before the containing object begins, so the
        // reservation safepoint keeps them all alive.
        let mut value_slots: Vec<(u16, Kind)> = Vec::new();
        for (_, value_expr, value_span) in &entries {
            let value = self.expr(value_expr)?;
            reference::admit_object_value(self, *value_span, value.kind)?;
            let (slot, mut stmt) = self.bind_evaluation(value.expr, value.kind, *value_span)?;
            owned_roots.extend(value.owned_roots);
            if self.has_root(slot) {
                owned_roots.push(slot);
            }
            stmts.append(&mut stmt);
            value_slots.push((slot, value.kind));
        }
        let carrier = self.active_carrier(literal.span)?;
        let data_count = layout::OBJECT_HEADER_CELLS
            .checked_add(
                layout::OBJECT_CELLS_PER_PROPERTY
                    .checked_mul(count)
                    .ok_or_else(|| self.error(literal.span, "P0 plain-object size overflow"))?,
            )
            .ok_or_else(|| self.error(literal.span, "P0 plain-object size overflow"))?;
        let newborn = self.begin_node(
            carrier,
            data_count,
            layout::KIND_OBJECT,
            literal.span,
            line,
            &mut stmts,
        )?;
        // Header remainder: property count plus the null prototype pair.
        // Kind/generation are stamped by reservation.
        for (offset, marker) in [(2i64, count), (3i64, 0), (4i64, 0)] {
            self.emit_cell_write(
                carrier,
                Expr::Add(
                    Box::new(Expr::Var(newborn.base)),
                    Box::new(Expr::Int(offset)),
                ),
                Self::as_number(Expr::Int(marker)),
                literal.span,
                line,
                &mut stmts,
            )?;
        }
        for (index, (key_box, (value_slot, _kind))) in
            key_boxes.iter().zip(value_slots.iter()).enumerate()
        {
            let row =
                layout::OBJECT_HEADER_CELLS + layout::OBJECT_CELLS_PER_PROPERTY * index as i64;
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
                self.bind_temp_bare(value::boolean(Expr::Bool(true)), literal.span)?;
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
                (0, Self::as_number(value::field(*key_box, "ref_slot"))),
                (1, Self::as_number(value::field(*key_box, "generation"))),
                (2, tag_number),
                (3, value::field(*value_slot, "number")),
                (4, flag_number),
                (5, ref_number),
                (6, gen_number),
            ];
            for (cell, payload) in cells {
                self.emit_cell_write(
                    carrier,
                    Expr::Add(
                        Box::new(Expr::Var(newborn.base)),
                        Box::new(Expr::Int(row + cell)),
                    ),
                    payload,
                    literal.span,
                    line,
                    &mut stmts,
                )?;
            }
        }
        let boxed = self.publish_node(carrier, &newborn, literal.span, line, &mut stmts)?;
        owned_roots.push(boxed);
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(Expr::Var(boxed)),
            },
            kind: Kind::Object,
            array_length: None,
            owned_roots,
        })
    }

    fn lower_array_expr(&mut self, literal: &ast::ArrayLit) -> Result<Lowered, ParseError> {
        let mut stmts = Vec::new();
        let published = lower_array_binding(self, "", literal, literal.span, &mut stmts, true)?;
        let (slot, length) = published.ok_or_else(|| {
            self.error(literal.span, "P3 array expression did not publish a value")
        })?;
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(Expr::Var(slot)),
            },
            kind: Kind::Array,
            array_length: Some(length),
            owned_roots: vec![slot],
        })
    }

    pub(super) fn lower_function_expr(
        &mut self,
        function: &ast::FnExpr,
    ) -> Result<Lowered, ParseError> {
        let span = function.function.span;
        let key = (span.lo.0, span.hi.0);
        let Some(&code_id) = self.function_code_ids.get(&key) else {
            return Err(self.error(span, "P0 source-function admission requires predeclaration"));
        };
        let carrier = self.active_carrier(span)?;
        let line = self.line(span);
        let mut stmts = Vec::new();
        let newborn = self.begin_node(
            carrier,
            layout::FUNCTION_CELLS,
            layout::KIND_FUNCTION,
            span,
            line,
            &mut stmts,
        )?;
        // Header remainder: zero pad, null prototype pair, static code
        // identity, empty environment pair, strict marker. The code ID is
        // a finite compile-time identity into the static source-function
        // table; the closed-env sentinel is explicitly empty, never a
        // handle. Kind/generation are stamped by reservation.
        for (offset, marker) in [
            (2i64, 0),
            (3i64, 0),
            (4i64, 0),
            (5i64, code_id),
            (6i64, 0),
            (7i64, 0),
            (8i64, 1),
            (
                layout::FUNCTION_LENGTH,
                function.function.params.len() as i64,
            ),
        ] {
            self.emit_cell_write(
                carrier,
                Expr::Add(
                    Box::new(Expr::Var(newborn.base)),
                    Box::new(Expr::Int(offset)),
                ),
                Self::as_number(Expr::Int(marker)),
                span,
                line,
                &mut stmts,
            )?;
        }
        let boxed = self.publish_node(carrier, &newborn, span, line, &mut stmts)?;
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(Expr::Var(boxed)),
            },
            kind: Kind::Function,
            array_length: None,
            owned_roots: vec![boxed],
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
    let mut ir = if !heap_needed {
        lower_primitive(script, source, source_id, base)?
    } else {
        lower_with_realm(script, source, source_id, base, context)?
    };
    undefined_box::factor(&mut ir);
    Ok(ir)
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
        ast::Stmt::Decl(ast::Decl::Fn(_)) => true,
        ast::Stmt::Expr(e) => expr_needs_heap(&e.expr),
        ast::Stmt::Block(block) => block.stmts.iter().any(stmt_needs_heap),
        ast::Stmt::If(if_stmt) => {
            expr_needs_heap(&if_stmt.test)
                || stmt_needs_heap(&if_stmt.cons)
                || if_stmt.alt.as_ref().is_some_and(|alt| stmt_needs_heap(alt))
        }
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
        ast::Expr::Unary(u) => u.op == ast::UnaryOp::TypeOf || expr_needs_heap(&u.arg),
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
        array_length_ids: HashMap::new(),
        source_bindings: Vec::new(),
        heap: None,
        init_cells: 0,
        init_nodes: 0,
        string_eq: None,
        enclosing_strict: false,
        function_this: None,
        function_args: None,
        carrier_override: None,
        frame_override: None,
        root_scope: None,
        helper_decode: None,
        helper_ref_get: None,
        helper_dispatch: None,
        function_code_ids: HashMap::new(),
        collected_functions: Vec::new(),
        graph: scope::build(script, source, source_id, base)?,
        active_scope: scope::ScopeId(0),
        binding_slots: HashMap::new(),
        binding_kinds: HashMap::new(),
        type_names: None,
        utf16_inits: utf16_init::Utf16InitPool::default(),
    };
    lower.active_scope = lower.graph.script;
    let mut statements = Vec::new();
    reference::lower_script_statements(&mut lower, &script.body, &mut statements)?;
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
        ast::Stmt::Decl(ast::Decl::Fn(decl)) => {
            collect_function_decl(decl, lower, enclosing_strict)
        }
        ast::Stmt::Block(block) => {
            for statement in &block.stmts {
                collect_from_stmt(statement, lower, enclosing_strict)?;
            }
            Ok(())
        }
        ast::Stmt::If(if_stmt) => {
            collect_from_expr(&if_stmt.test, lower, enclosing_strict)?;
            collect_from_stmt(&if_stmt.cons, lower, enclosing_strict)?;
            if let Some(alt) = &if_stmt.alt {
                collect_from_stmt(alt, lower, enclosing_strict)?;
            }
            Ok(())
        }
        ast::Stmt::Return(ret) => {
            if let Some(arg) = ret.arg.as_deref() {
                collect_from_expr(arg, lower, enclosing_strict)?;
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
            reference::admit_call_callee(lower, c)?;
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

/// Validate one bounded anonymous ordinary strict function and assign its
/// code ID. Closed declarations and the existing single-Return body profile
/// share the declaration graph with ordered plain formals. Free lexical
/// captures, named/self binding, constructors, throw/try and generators are explicit
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
    if func.params.len() > scope::MAX_ARGUMENTS {
        return Err(lower.error(
            span,
            "P4 source functions exceed the 64 formal parameter profile limit",
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
    // Closed declaration subset: directives, var/function declarations,
    // expression statements, declaration-only if, and one return. Unsupported
    // statements still fail at lowering; collection must see nested functions.
    for statement in &body.stmts {
        collect_from_stmt(statement, lower, strict)?;
    }
    Ok(())
}

fn collect_function_decl(
    decl: &ast::FnDecl,
    lower: &mut Lowerer<'_>,
    enclosing_strict: bool,
) -> Result<(), ParseError> {
    collect_function(
        &ast::FnExpr {
            ident: None,
            function: decl.function.clone(),
        },
        lower,
        enclosing_strict,
    )
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
    fn stmt_needs(statement: &ast::Stmt) -> bool {
        match statement {
            ast::Stmt::Decl(ast::Decl::Var(decl)) => decl
                .decls
                .iter()
                .any(|d| d.init.as_deref().is_some_and(expr_needs)),
            ast::Stmt::Decl(ast::Decl::Fn(decl)) => decl
                .function
                .body
                .as_ref()
                .is_some_and(|body| body.stmts.iter().any(stmt_needs)),
            ast::Stmt::Expr(e) => expr_needs(&e.expr),
            ast::Stmt::Block(block) => block.stmts.iter().any(stmt_needs),
            ast::Stmt::If(if_stmt) => {
                expr_needs(&if_stmt.test)
                    || stmt_needs(&if_stmt.cons)
                    || if_stmt.alt.as_ref().is_some_and(|alt| stmt_needs(alt))
            }
            ast::Stmt::Return(ret) => ret.arg.as_deref().is_some_and(expr_needs),
            _ => false,
        }
    }
    script.body.iter().any(stmt_needs)
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
        array_length_ids: HashMap::new(),
        source_bindings: Vec::new(),
        heap: None,
        init_cells: 0,
        init_nodes: 0,
        string_eq: None,
        enclosing_strict: false,
        function_this: None,
        function_args: None,
        carrier_override: None,
        frame_override: None,
        root_scope: None,
        helper_decode: None,
        helper_ref_get: None,
        helper_dispatch: None,
        function_code_ids: HashMap::new(),
        collected_functions: Vec::new(),
        graph: scope::build(script, source, source_id, base)?,
        active_scope: scope::ScopeId(0),
        binding_slots: HashMap::new(),
        binding_kinds: HashMap::new(),
        type_names: None,
        utf16_inits: utf16_init::Utf16InitPool::default(),
    };
    lower.active_scope = lower.graph.script;
    let script_strict = lower.script_strict(script);
    lower.enclosing_strict = script_strict;
    lower.preflight_binding_literals(script)?;
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
    // Fixed realm header plus zeroed page table through the existing
    // factory; the static budget seeds the known fixed layout cost.
    let carrier = heap_emitters::emit_bootstrap(
        &mut lower,
        helper_make,
        helper_write,
        helper_resize,
        probe_span,
        &mut statements,
    )?;
    lower.init_cells = (layout::PREFIX_CELLS + layout::FRAME_FIXED_CELLS) as u64;
    let decode = append_decode_int(&mut lower, &mut base_ir, probe_span, &mut statements)?;
    lower.helper_decode = Some(decode);
    let helpers = heap_emitters::append_heap_core(
        &mut lower,
        &mut base_ir,
        helper_make,
        helper_read,
        helper_write,
        helper_length,
        helper_resize,
        decode,
        probe_span,
        &mut statements,
    )?;
    lower.heap = Some(Heap {
        carrier,
        helper_read,
        helper_write,
        helpers,
    });
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
    let string_eq = value_emitters::append_string_equality(
        &mut lower,
        &mut base_ir,
        &carrier_schema,
        helper_read,
        decode,
        helpers.checked_base,
        probe_span,
        &mut statements,
    )?;
    lower.string_eq = Some(string_eq);
    // Script strictness from the exact raw prologue; inherited by every
    // admitted function without its own directive.
    collect_source_functions(script, &mut lower, script_strict)?;
    // Only append the object/function helpers when some source shape needs
    // them; pure numeric-array/string sources keep the accepted program
    // without dead native code.
    let need_ref = script_needs_ref_helpers(script);
    let function_count = lower.collected_functions.len();
    if need_ref || function_count > 0 {
        // Operational GetValue through checked generation pairs, then the
        // finite static source-function table plus dispatch.
        let ref_get = value_emitters::append_ref_get(
            &mut lower,
            &mut base_ir,
            &carrier_schema,
            helper_read,
            decode,
            helpers.decode_u32,
            helpers.checked_base,
            probe_span,
            &mut statements,
        )?;
        lower.helper_ref_get = Some(ref_get);
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
        utf16_init::arm(
            &mut lower.utf16_inits,
            dispatch_index.checked_add(1),
            helper_write,
            carrier_schema.clone(),
        );
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
            let dispatch = value_emitters::append_dispatch(
                &mut lower,
                &mut base_ir,
                &carrier_schema,
                helper_read,
                decode,
                helpers.decode_u32,
                helpers.checked_base,
                &func_indices,
                probe_span,
                &mut statements,
            )?;
            debug_assert_eq!(dispatch, dispatch_index);
        }
    }
    if !lower.utf16_inits.is_armed() {
        let first = base_ir
            .functions
            .iter()
            .map(|function| function.index)
            .max()
            .unwrap_or(0)
            .checked_add(1);
        utf16_init::arm(
            &mut lower.utf16_inits,
            first,
            helper_write,
            carrier_schema.clone(),
        );
    }
    // Script activation frame: reserve the frame slot, lower the body into
    // a side buffer under a fresh root scope, then prefix frame entry with
    // the full capacity known only after lowering.
    let frame_slot = lower.slot(probe_span)?;
    lower.root_scope = Some(RootScope {
        frame_slot,
        entries: HashMap::new(),
        next_entry: 0,
    });
    let mut body = Vec::new();
    reference::lower_script_statements(&mut lower, &script.body, &mut body)?;
    // Prefix the script frame entry with the full capacity now known, then
    // the lowered body. The frame stays active through the final result and
    // any appended in-realm observations; teardown owns its release.
    let capacity = lower
        .root_scope
        .as_ref()
        .map(|scope| scope.next_entry)
        .unwrap_or(0);
    let line = lower.line(probe_span);
    statements.push(Stmt::Let {
        index: frame_slot,
        declared_schema: Some(TypeSchema::Int),
        expr: Lowerer::guest_call(
            helpers.root_enter,
            vec![Expr::Var(carrier), Expr::Int(i64::from(capacity))],
        ),
        line,
    });
    let body_len = body.len();
    statements.extend(body);
    let frame_at = statements.len() - body_len - 1;
    utf16_init::append_declarations(
        &mut lower,
        &mut base_ir,
        &mut statements,
        frame_at,
        probe_span,
    )?;
    base_ir.stmts.extend(statements);
    base_ir.locals = usize::from(lower.next_slot);
    base_ir.local_bindings.extend(lower.source_bindings);
    let declaration = value::declaration();
    base_ir
        .struct_schemas
        .insert(declaration.name.clone(), declaration);
    let allocation = value::allocation_declaration();
    base_ir
        .struct_schemas
        .insert(allocation.name.clone(), allocation);
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
pub(super) fn assert_stmt(condition: Expr, line: u32) -> Stmt {
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
    // map so free variables cannot fall back to root locals. The fresh
    // root scope plus frame override route every newborn root into this
    // activation's frame; capacity is prefixed after the body is known.
    let frame_slot = lower.slot(collected.span)?;
    let body = collected
        .function
        .body
        .as_ref()
        .ok_or_else(|| lower.error(collected.span, "P0 source functions require a body"))?;
    let mut body_stmts: Vec<Stmt> = Vec::new();
    let fn_key = (collected.span.lo.0, collected.span.hi.0);
    let fn_scope = lower
        .graph
        .function_scopes
        .get(&fn_key)
        .copied()
        .ok_or_else(|| lower.error(collected.span, "P3 function scope was not collected"))?;
    let saved_bindings = std::mem::take(&mut lower.bindings);
    let saved_this = lower.function_this.replace(param_this);
    let saved_args = lower.function_args.replace(param_args);
    let saved_carrier = lower.carrier_override.replace(param_carrier);
    let saved_frame = lower.frame_override.replace(frame_slot);
    let saved_scope = lower.root_scope.replace(RootScope {
        frame_slot,
        entries: HashMap::new(),
        next_entry: 0,
    });
    let saved_strict = std::mem::replace(&mut lower.enclosing_strict, collected.strict);
    let saved_active = lower.active_scope;
    let saved_lengths = std::mem::take(&mut lower.array_lengths);
    let saved_type_names = lower.type_names.take();
    lower.active_scope = fn_scope;
    let saved_slots = std::mem::take(&mut lower.binding_slots);
    let saved_kinds = std::mem::take(&mut lower.binding_kinds);
    let saved_length_ids = std::mem::take(&mut lower.array_length_ids);
    let lowered = reference::lower_function_body(lower, &body.stmts, &mut body_stmts);
    lower.binding_slots = saved_slots;
    lower.binding_kinds = saved_kinds;
    lower.array_length_ids = saved_length_ids;
    lower.function_args = saved_args;
    lower.active_scope = saved_active;
    lower.array_lengths = saved_lengths;
    lower.type_names = saved_type_names;
    let completion = match lowered {
        Ok(value) => value,
        Err(err) => {
            lower.bindings = saved_bindings;
            lower.function_this = saved_this;
            lower.carrier_override = saved_carrier;
            lower.frame_override = saved_frame;
            lower.root_scope = saved_scope;
            lower.enclosing_strict = saved_strict;
            lower.type_names = saved_type_names;
            return Err(err);
        }
    };
    lower.bindings = saved_bindings;
    lower.function_this = saved_this;
    lower.carrier_override = saved_carrier;
    lower.frame_override = saved_frame;
    lower.enclosing_strict = saved_strict;
    // Frame capacity is known only after the body lowered; prefix entry,
    // then save the real Completion, leave the frame without allocating
    // or collecting, and return. The caller pins Completion.value with
    // its first no-allocation root write before another safepoint.
    let capacity = lower
        .root_scope
        .as_ref()
        .map(|scope| scope.next_entry)
        .unwrap_or(0);
    lower.root_scope = saved_scope;
    let helpers = lower
        .heap
        .as_ref()
        .map(|heap| heap.helpers)
        .ok_or_else(|| lower.error(collected.span, "P0 source functions require the realm heap"))?;
    lower.charge_initial_cells(layout::FRAME_FIXED_CELLS as u64, collected.span)?;
    let completion_expr = completion.unwrap_or_else(value::completion_normal);
    let (comp_slot, comp_stmt) =
        lower.bind_typed(completion_expr, value::completion_schema(), collected.span)?;
    let mut prologue = vec![Stmt::Let {
        index: frame_slot,
        declared_schema: Some(TypeSchema::Int),
        expr: Lowerer::guest_call(
            helpers.root_enter,
            vec![Expr::Var(param_carrier), Expr::Int(i64::from(capacity))],
        ),
        line,
    }];
    let leave_ack = lower.slot(collected.span)?;
    let retained_args = lower.slot(collected.span)?;
    let mut epilogue = vec![
        comp_stmt,
        Stmt::Let {
            index: retained_args,
            declared_schema: Some(TypeSchema::Int),
            expr: Lowerer::builtin_call(
                BuiltinFunction::Len.call_index(),
                vec![Expr::Var(param_args)],
            ),
            line,
        },
        Stmt::Let {
            index: leave_ack,
            declared_schema: Some(TypeSchema::Int),
            expr: Lowerer::guest_call(
                helpers.root_leave,
                vec![Expr::Var(param_carrier), Expr::Var(frame_slot)],
            ),
            line,
        },
    ];
    prologue.append(&mut body_stmts);
    prologue.append(&mut epilogue);
    body_stmts = prologue;
    let body_expr = Expr::Var(comp_slot);
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

pub(super) fn lower_array_binding(
    lower: &mut Lowerer<'_>,
    name: &str,
    literal: &ast::ArrayLit,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
    anonymous: bool,
) -> Result<Option<(u16, usize)>, ParseError> {
    let carrier = lower.active_carrier(span)?;
    let line = lower.line(span);
    // Static pre-check from the syntactic element count (holes and spread
    // still reject below) so budget excess fails before large emission;
    // the reservation charges the same extent again below.
    let syntactic = layout::ARRAY_HEADER_CELLS
        .checked_add(literal.elems.len() as i64)
        .ok_or_else(|| lower.error(span, "P0 numeric array length overflow"))?;
    let pre_need = (syntactic as u64)
        .checked_add(layout::RAW_HEADER_CELLS as u64)
        .ok_or_else(|| lower.error(span, "P0 numeric array length overflow"))?;
    if lower
        .init_cells
        .checked_add(pre_need)
        .is_none_or(|end| end > MAX_REALM_INITIAL_CELLS)
    {
        return Err(lower.error(
            span,
            "JavaScript ES5 realm initialization exceeds the 65536 cell budget",
        ));
    }
    // Numeric elements evaluate first; they carry no heap edges, so the
    // later reservation safepoint cannot strand them.
    let mut element_slots = Vec::new();
    for element in &literal.elems {
        let Some(element) = element else {
            return Err(lower.error(span, "P0 numeric arrays reject holes"));
        };
        if element.spread.is_some() {
            return Err(lower.error(element.span(), "P0 numeric arrays reject spread elements"));
        }
        let value = lower.expr(&element.expr)?;
        reference::admit_numeric_element(lower, element.span(), value.kind)?;
        // Kind::Number was checked above: this boxed scalar has no heap
        // edge, so numeric payload length must not consume root entries.
        let (slot, stmt) = lower.bind_temp_bare(value.expr, element.span())?;
        statements.push(stmt);
        reference::release_consumed_roots(
            lower,
            &value.owned_roots,
            slot,
            element.span(),
            statements,
        )?;
        element_slots.push(slot);
    }
    let length = element_slots.len();
    let data_count = layout::ARRAY_HEADER_CELLS
        .checked_add(length as i64)
        .ok_or_else(|| lower.error(span, "P0 numeric array length overflow"))?;
    let newborn = lower.begin_node(
        carrier,
        data_count,
        layout::KIND_ARRAY,
        span,
        line,
        statements,
    )?;
    // Header remainder: length plus the null prototype pair.
    for (offset, marker) in [(2i64, length as i64), (3i64, 0), (4i64, 0)] {
        lower.emit_cell_write(
            carrier,
            Expr::Add(
                Box::new(Expr::Var(newborn.base)),
                Box::new(Expr::Int(offset)),
            ),
            Lowerer::as_number(Expr::Int(marker)),
            span,
            line,
            statements,
        )?;
    }
    for (position, element) in element_slots.iter().enumerate() {
        let offset = layout::ARRAY_HEADER_CELLS + position as i64;
        lower.emit_cell_write(
            carrier,
            Expr::Add(
                Box::new(Expr::Var(newborn.base)),
                Box::new(Expr::Int(offset)),
            ),
            value::field(*element, "number"),
            span,
            line,
            statements,
        )?;
    }
    if anonymous {
        let boxed = lower.publish_node(carrier, &newborn, span, line, statements)?;
        return Ok(Some((boxed, length)));
    }
    if let Some(id) = lower
        .graph
        .scope(lower.active_scope)
        .by_name
        .get(name)
        .copied()
        && let Some(&slot) = lower.binding_slots.get(&id)
    {
        lower.publish_into(slot, carrier, &newborn, span, line, statements)?;
        lower.bindings.insert(name.to_owned(), (slot, Kind::Array));
        lower.binding_kinds.insert(id, Kind::Array);
        lower.array_lengths.insert(name.to_owned(), length);
        lower.array_length_ids.insert(id, length);
        return Ok(None);
    }
    let boxed = lower.publish_node(carrier, &newborn, span, line, statements)?;
    lower.bindings.insert(name.to_owned(), (boxed, Kind::Array));
    lower.array_lengths.insert(name.to_owned(), length);
    lower.source_bindings.push((name.to_owned(), boxed));
    Ok(None)
}

pub(super) fn lower_member_write_stmt(
    lower: &mut Lowerer<'_>,
    member: &ast::MemberExpr,
    right: &ast::Expr,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let kind = match member.obj.as_ref() {
        ast::Expr::Ident(object) => lower
            .bindings
            .get(object.sym.as_str())
            .map(|(_, kind)| *kind)
            .unwrap_or(Kind::Undefined),
        ast::Expr::This(_) => Kind::Object,
        _ => Kind::Undefined,
    };
    let length = if let ast::Expr::Ident(object) = member.obj.as_ref() {
        lower.array_lengths.get(object.sym.as_str()).copied()
    } else {
        None
    };
    reference::admit_member_write(lower, member, kind, length)?;
    let ast::MemberProp::Computed(property) = &member.prop else {
        unreachable!()
    };
    let index = Lowerer::member_index_literal(&property.expr).expect("validated numeric index");
    let ast::Expr::Ident(object) = member.obj.as_ref() else {
        unreachable!()
    };
    let (slot, _) = lower.bindings[object.sym.as_str()];
    // Receiver evaluation/rooting precedes the RHS and does not consume
    // identity fields from the reusable source binding.
    let (receiver, mut receiver_bound) = lower.bind_temp(Expr::Var(slot), member.obj.span())?;
    statements.append(&mut receiver_bound);
    let value = lower.expr(right)?;
    if value.kind != Kind::Number {
        return Err(lower.error(right.span(), "P0 numeric arrays store only numbers"));
    }
    let mut value_roots = value.owned_roots;
    let (value_slot, mut value_bound) =
        lower.bind_evaluation(value.expr, value.kind, right.span())?;
    statements.append(&mut value_bound);
    let carrier = lower.active_carrier(span)?;
    let checked_base = lower.heap_ref(span)?.helpers.checked_base;
    let helper_read = lower.heap_ref(span)?.helper_read;
    let decode = lower
        .helper_decode
        .ok_or_else(|| lower.error(span, "P0 array writes require the realm decode helper"))?;
    let line = lower.line(span);
    // Generation/kind/length gate before the element write lands.
    let base_tmp = lower.slot(span)?;
    statements.push(Stmt::Let {
        index: base_tmp,
        declared_schema: Some(TypeSchema::Int),
        expr: Lowerer::guest_call(
            checked_base,
            vec![
                Expr::Var(carrier),
                value::field(receiver, "ref_slot"),
                value::field(receiver, "generation"),
            ],
        ),
        line,
    });
    let kind_tmp = lower.slot(span)?;
    statements.push(Stmt::Let {
        index: kind_tmp,
        declared_schema: Some(TypeSchema::Int),
        expr: Lowerer::guest_call(
            decode,
            vec![Lowerer::guest_call(
                helper_read,
                vec![Expr::Var(carrier), Lowerer::as_number(Expr::Var(base_tmp))],
            )],
        ),
        line,
    });
    statements.push(assert_stmt(
        Expr::Eq(
            Box::new(Expr::Var(kind_tmp)),
            Box::new(Expr::Int(layout::KIND_ARRAY)),
        ),
        line,
    ));
    let len_tmp = lower.slot(span)?;
    statements.push(Stmt::Let {
        index: len_tmp,
        declared_schema: Some(TypeSchema::Int),
        expr: Lowerer::guest_call(
            decode,
            vec![Lowerer::guest_call(
                helper_read,
                vec![
                    Expr::Var(carrier),
                    Lowerer::as_number(Expr::Add(
                        Box::new(Expr::Var(base_tmp)),
                        Box::new(Expr::Int(2)),
                    )),
                ],
            )],
        ),
        line,
    });
    statements.push(assert_stmt(
        Expr::Lt(Box::new(Expr::Int(index)), Box::new(Expr::Var(len_tmp))),
        line,
    ));
    lower.emit_cell_write(
        carrier,
        Expr::Add(
            Box::new(Expr::Var(base_tmp)),
            Box::new(Expr::Int(layout::ARRAY_HEADER_CELLS + index)),
        ),
        value::field(value_slot, "number"),
        span,
        line,
        statements,
    )?;
    value_roots.push(receiver);
    if lower.has_root(value_slot) {
        value_roots.push(value_slot);
    }
    reference::release_consumed_roots(lower, &value_roots, slot, span, statements)?;
    Ok(value_slot)
}
