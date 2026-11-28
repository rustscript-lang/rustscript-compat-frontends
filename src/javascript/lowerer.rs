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
use super::iteration;
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
use std::collections::{HashMap, HashSet};
use swc_common::Spanned;
use swc_ecma_ast as ast;
use vm::compiler::ir::{FunctionDecl, FunctionImpl};
use vm::{
    BuiltinFunction, Expr, FrontendIr, LocalIrBuilder, ParseError, SourceFrontendContext, Stmt,
    TypeSchema, ValueType,
};

pub(super) mod abstract_equality;
pub(super) mod bitwise;
mod fixed_scalar_string;
mod member_sites;
mod number_key;
pub(super) mod primitive_addition;
pub(super) mod primitive_assignment;
pub(super) mod primitive_key;
pub(super) mod primitive_number;
pub(super) mod relational;
mod remainder;
pub(super) mod string_concat;
mod string_length;
mod unary;
pub(super) mod uri_calls;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// Physical specialization of one logical dense JS Array. `Packed` is the
/// private one-cell-per-element numeric kind; `Tagged` stores full JsValue
/// entries at five cells each. Neither is a public type or realm version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ArrayRepr {
    Packed,
    Tagged,
}

/// Exact known length plus physical representation of a dense array value.
/// Only produced when both branches/facts agree; never a fabricated guess.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ArrayShape {
    pub length: usize,
    pub repr: ArrayRepr,
    /// Packed indexes below this are physically present and number-only.
    /// Indexes at or above it are holes and may store any admitted kind.
    pub number_only_below: usize,
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LiteralRole {
    Data,
    Getter,
    Setter,
}
pub(super) type ObjectEntry = (Vec<u16>, Box<ast::Expr>, swc_common::Span, LiteralRole);

pub(super) struct Lowered {
    pub(super) expr: Expr,
    pub(super) kind: Kind,
    /// Exact numeric-array shape when this value is a known array.
    /// `None` is unknown, never a fabricated zero.
    pub(super) array_length: Option<ArrayShape>,
    /// Compiler temps whose root entries this value owns until a rooted
    /// destination takes the payload. Source bindings and activation
    /// caches are not listed here.
    pub(super) owned_roots: Vec<u16>,
}

/// Rooting destination chosen by the enclosing statement for one expression.
/// Only the outermost `Call` arm of a local `var <ident> = f(...)`
/// initializer selects the immediate handoff; every nested operand, callee,
/// receiver and actual is lowered with `Temporary`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CallResultRooting {
    Temporary,
    ImmediateLocalInitializer,
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
    pub(super) globals: super::global_binding::Plan,
    pub(super) global_helpers: Option<super::prelude::global_binding::Helpers>,
    pub(super) global_env: Option<u16>,
    pub(super) source: &'a str,
    pub(super) source_id: u32,
    pub(super) base: u32,
    pub(super) next_slot: u16,
    pub(super) bindings: HashMap<String, (u16, Kind)>,
    pub(super) array_lengths: HashMap<String, ArrayShape>,
    pub(super) array_length_ids: HashMap<scope::BindingId, ArrayShape>,
    /// Names published as arrays. An index write may drop the static length,
    /// but the length intercept still has to see the binding.
    pub(super) array_names: HashSet<String>,
    pub(super) source_bindings: Vec<(String, u16)>,
    pub(super) heap: Option<Heap>,
    pub(super) intrinsics: Option<super::prelude::intrinsics::Intrinsics>,
    primitive_number: primitive_number::State,
    primitive_addition: primitive_addition::State,
    mutation: primitive_assignment::State,
    abstract_equality: abstract_equality::State,
    pub(super) math_used: std::cell::Cell<bool>,
    pub(super) numeric_globals: std::cell::Cell<u8>,
    pub(super) uri_used: std::cell::Cell<u8>,
    /// The optional `JSON` global object and its one own `stringify` row.
    /// Only a source that names `JSON` pays for the object, exactly like the
    /// numeric and URI manifests.
    pub(super) json_used: std::cell::Cell<bool>,
    pub(super) number_namespace_used: std::cell::Cell<bool>,
    /// `Number.prototype.toString` row on the private Number prototype. Only a
    /// source that names `toString` pays the row and its function node; the
    /// key String is shared with `Object.prototype.toString`.
    pub(super) number_to_string_used: std::cell::Cell<bool>,
    /// `Function.prototype.name` static member name in the source. Only a
    /// source that can spell the property pays the name accessor row, its
    /// getter and the per-source-function `name` row.
    pub(super) function_name_used: std::cell::Cell<bool>,
    /// `Function.prototype.toString` static member name in the source. The
    /// prototype method needs the same per-function name storage as `.name`
    /// so it can print the real function name.
    pub(super) function_to_string_used: std::cell::Cell<bool>,
    /// `Function.prototype.prototype` accessor. A source that can spell
    /// `.prototype` (or that contains a supported `new` site) pays one
    /// get/set accessor row on Function.prototype and its two private nodes;
    /// no per-function row is created until the getter materializes one. A
    /// `.prototype` read on `Number`/`String`/`Boolean`/`Array` additionally
    /// pays that builtin prototype's `constructor` row.
    pub(super) function_prototype_used: std::cell::Cell<bool>,
    /// A `.constructor` *read* is the only site that needs the three primitive
    /// carriers: the receiver may be a Number, a String or a Boolean and the
    /// static walk cannot tell which, so the back-link row on every private
    /// primitive prototype must name a real function object. `.prototype`
    /// reads, `new` and the `in`/`hasOwnProperty` observations keep the plain
    /// accessor charge.
    pub(super) primitive_constructor_read: std::cell::Cell<bool>,
    /// A supported `new F(...)` site over a source function. Only such a
    /// source pays the private construct helper.
    pub(super) construct_used: std::cell::Cell<bool>,
    /// Flat index of the private `__compat_es5_construct` helper.
    pub(super) helper_construct: Option<u16>,
    /// The pre-`Get` callee gate of the same slice: one guest Result that
    /// rejects a non-source-function callee before `Get(F, "prototype")`.
    pub(super) helper_construct_check: Option<u16>,
    /// `String.prototype.replaceAll` row on the private String prototype. Only
    /// a source that can name `replaceAll` pays the row, its function node and
    /// its ten-unit key String.
    pub(super) string_replace_all_used: std::cell::Cell<bool>,
    /// `String.prototype.search` row on the private String prototype in the
    /// one admitted `/a/` RegExp domain. Only a source that can name `search`
    /// pays the row, its function node, its six-unit key String and the
    /// private `/a/` matcher kernel pair the adapter calls.
    pub(super) string_search_used: std::cell::Cell<bool>,
    /// `String.prototype.match` row on the private String prototype in the
    /// same one admitted `/a/` RegExp domain. Only a source that can name
    /// `match` pays the row, its function node and its five-unit key String;
    /// the adapter reuses the realm's existing `/a/` matcher kernel pair.
    pub(super) string_match_used: std::cell::Cell<bool>,
    /// Optional function-shaped `String` carrier. A shadowed spelling does not select it.
    pub(super) string_namespace_used: std::cell::Cell<bool>,
    pub(super) array_namespace_used: std::cell::Cell<bool>,
    pub(super) array_constructor_used: std::cell::Cell<bool>,
    /// Private `Core Array<JsValue> -> guest Tagged Array` boxing helper,
    /// present only when the source passes bare `arguments` to
    /// `Array.isArray`. The invocation vector is a Core array, so a real
    /// Get+Call needs a guest Array value for the callee to observe.
    pub(super) arguments_array_from_values: Option<u16>,
    /// Private `arguments[<runtime String>]` key classifier. Present only when
    /// the source has a computed `arguments` subscript that is not a literal;
    /// a canonical decimal String names an element and `"length"` the length
    /// row, so the invocation vector's Array semantics stay observable.
    pub(super) arguments_string_key: Option<u16>,
    /// Private two-way mirror between the Core invocation vector and the
    /// activation's single guest `arguments` object. Present only when the
    /// source passes bare `arguments` to `Array.isArray`.
    pub(super) arguments_sync: Option<u16>,
    /// Slot holding the activation's guest `arguments` object, allocated only
    /// in an activation that passes it to `Array.isArray`. Saved/restored
    /// around nested activations, like `function_args`.
    pub(super) arguments_object: Option<u16>,
    pub(super) array_buffer_used: std::cell::Cell<bool>,
    /// Some reachable `new Uint8Array(...)` first actual cannot be proven
    /// inside the per-buffer byte budget, so the owned-length route's fatal
    /// host quota branch is live and needs the scalar adapter.
    pub(super) typed_array_owned_quota_reachable: std::cell::Cell<bool>,
    pub(super) typed_array_elements: std::cell::Cell<u16>,
    /// Ordinary `Boolean(...)` only. A shadowed spelling does not select it.
    pub(super) boolean_used: std::cell::Cell<bool>,
    pub(super) range_error_used: std::cell::Cell<bool>,
    pub(super) optional_errors_used: std::cell::Cell<u8>,
    utf16_bridge: Option<super::prelude::utf16_bridge::Entries>,
    uri_call: Option<u16>,
    pub(super) objects: Option<super::prelude::object::ObjectHelpers>,
    /// Exact flow-approved reads; never inferred from broad emitter Kinds.
    string_length_sites: std::cell::RefCell<std::collections::HashSet<(u32, u32)>>,
    string_length_sealed: std::cell::Cell<bool>,
    helper_string_length: Option<u16>,
    string_concat_sites:
        std::cell::RefCell<std::collections::HashMap<(u32, u32), string_concat::Mode>>,
    string_concat_sealed: std::cell::Cell<bool>,
    helper_string_concat: Option<u16>,
    helper_fixed_scalar_string: Option<u16>,
    relational_sites: std::cell::RefCell<HashMap<(u32, u32), relational::Site>>,
    relational_sealed: std::cell::Cell<bool>,
    helper_string_compare: Option<u16>,
    number_key_sites: std::cell::RefCell<std::collections::HashSet<(u32, u32)>>,
    number_key_sealed: std::cell::Cell<bool>,
    number_string_entries: Option<super::prelude::number_string::Entries>,
    primitive_key: primitive_key::State,
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
    pub(super) helper_dispatch: Option<u16>,
    /// Existing pure (Int)->Int declaration, registered before any source
    /// activation is lowered. This does not require Object intrinsic setup.
    pub(super) helper_int_identity: Option<u16>,
    /// Finite static code IDs for every admitted source-function AST node,
    /// keyed by absolute span `(lo, hi)`. Never source-text recognition.
    function_code_ids: HashMap<(u32, u32), i64>,
    /// Collected admitted functions in code-ID order for predeclaration.
    collected_functions: Vec<CollectedFunction>,
    pub(super) graph: scope::ScopeGraph,
    pub(super) active_scope: scope::ScopeId,
    /// Activation-local structured-iteration lowering state. A fresh state is
    /// installed for every script/function activation; plain accepted sources
    /// never enter enhanced mode, so their P6A/P6B emission is unchanged.
    pub(super) iteration: iteration::IterationState,
    /// Monotone static jump-target identity shared by every activation.
    /// Targets are positive; zero stays the Completion nonjump sentinel.
    pub(super) next_target: u32,
    /// Validation-only catch scope. Emission uses `active_scope` instead.
    pub(super) catch_overlay: std::cell::Cell<Option<scope::ScopeId>>,
    /// Set by the complete binding-aware admission walk, not identifier spelling.
    pub(super) needs_ordinary_realm: std::cell::Cell<bool>,
    /// Set only while lowering a function that can publish a pending throw.
    /// Call sites consume callee `Err` into these slots; the single epilogue
    /// turns that completion into `Result::Err`.
    pub(super) pending_normal: Option<u16>,
    pub(super) pending_completion: Option<u16>,
    pub(super) pending_payload: Option<u16>,
    /// Exact `typescript::throw_error` index when this script can leave a throw
    /// uncaught. Absent on programs that cannot publish a script throw.
    pub(super) uncaught_host: Option<u16>,
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

    pub(super) fn ir_span(&self, span: swc_common::Span) -> vm::Span {
        let lo = span.lo.0.saturating_sub(self.base) as usize;
        let hi = span.hi.0.saturating_sub(self.base) as usize;
        vm::compiler::source_map::Span::new(self.source_id, lo, hi)
    }

    /// Publish a callee `Err` payload as this activation's pending throw.
    /// Without a pending tail the throw is unhandled: abort as a fatal host
    /// error, never as a guest catch value.
    pub(super) fn publish_callee_throw(
        &mut self,
        payload: u16,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let (Some(normal), Some(completion), Some(pending)) = (
            self.pending_normal,
            self.pending_completion,
            self.pending_payload,
        ) else {
            stmts.push(assert_stmt(Expr::Bool(false), self.line(span)));
            return Ok(());
        };
        let line = self.line(span);
        stmts.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: pending,
            expr: Expr::Var(payload),
            line,
        });
        self.refresh_root(pending, span, stmts)?;
        let record = self.slot(span)?;
        stmts.push(Stmt::Let {
            index: record,
            declared_schema: Some(value::completion_schema()),
            expr: value::completion_throw(Expr::Var(pending)),
            line,
        });
        stmts.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: completion,
            expr: Expr::Var(record),
            line,
        });
        stmts.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: normal,
            expr: Expr::Bool(false),
            line,
        });
        Ok(())
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

    /// The exact managed String -> UTF-16 Int array bridge appended for this
    /// realm. The admitted `/a/` test reuses it instead of reading units a
    /// second way; it exists whenever the object runtime is installed.
    pub(super) fn utf16_units_helper(&self) -> Option<u16> {
        self.utf16_bridge
            .as_ref()
            .map(|entries| entries.managed_to_units)
    }

    /// The exact UTF-16 Int array -> managed String publisher already
    /// installed with the bridge. Callers reuse this index; this accessor
    /// does not append a helper, reserve a function index, or add a field.
    pub(super) fn utf16_publish_helper(&self) -> Option<u16> {
        self.utf16_bridge
            .as_ref()
            .map(|entries| entries.units_to_managed)
    }

    /// Publish the current local-slot high-water mark before a private
    /// completed-IR helper appends its own locals, then adopt the new total.
    /// Completed-IR appenders reserve from `ir.locals`, so both bounds must
    /// agree before and after without touching dormant programs.
    pub(super) fn reserve_private_locals(
        &mut self,
        ir: &mut FrontendIr,
        span: swc_common::Span,
    ) -> Result<(), ParseError> {
        ir.locals = ir.locals.max(usize::from(self.next_slot));
        self.next_slot = u16::try_from(ir.locals)
            .map_err(|_| self.error(span, "private helper local slot space exhausted"))?;
        Ok(())
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

    /// Reserve the private pending identity for an initial Undefined value
    /// without publishing a root edge. The frame entry is zeroed on
    /// reservation, so the pending slot and its entry already agree; only
    /// the entry index has to exist. Heap-free activations have no frame,
    /// so this is a no-op there. The caller must bind the slot to
    /// `Undefined` before calling this, and this never acknowledges the
    /// entry, writes a raw cell, or emits an IR statement.
    pub(super) fn reserve_root(
        &mut self,
        slot: u16,
        span: swc_common::Span,
    ) -> Result<(), ParseError> {
        if self.root_scope.is_none() {
            return Ok(());
        }
        self.entry_for(slot, span)?;
        Ok(())
    }

    /// Drop only the logical edge. The physical box may remain as an
    /// intentionally unrooted value; no later semantic use may depend on it.
    /// The slot is a compiler-owned binding/temp whose entry is cleared by the
    /// canonical three-slot helper, never by publishing a fresh Undefined
    /// record through the generic four-argument write.
    pub(super) fn clear_root(
        &mut self,
        slot: u16,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        if !self.has_root(slot) {
            return Ok(());
        }
        self.write_root_clear(slot, span, stmts)
    }

    /// Managed TypeError for a normal guest value that is not a numeric array.
    /// Visible to the iteration owner; the primitive-key helper stays module-private.
    pub(super) fn raise_managed_type_error(
        &mut self,
        span: swc_common::Span,
        statements: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        self.emit_proved_nullish_put_error(span, statements)
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

    /// Clear one known root entry through the private three-slot helper. The
    /// exact frame/entry bookkeeping of the generic write is reused; the
    /// payload is the canonical empty entry itself, so no boxed record is
    /// built, loaded or published. Reserved for compiler-owned canonical
    /// Undefined producers (a rooted temp or a freshly reserved entry); the
    /// caller decides whether the slot must already be rooted.
    fn write_root_clear(
        &mut self,
        slot: u16,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        if self.root_scope.is_none() {
            return Ok(());
        }
        let carrier = self.active_carrier(span)?;
        let root_clear = self.heap_ref(span)?.helpers.root_clear;
        let frame = self.active_frame(span)?;
        let entry = self.entry_for(slot, span)?;
        let ack = self.slot(span)?;
        let line = self.line(span);
        stmts.push(Stmt::Let {
            index: ack,
            declared_schema: Some(TypeSchema::Int),
            expr: Self::guest_call(
                root_clear,
                vec![
                    Expr::Var(carrier),
                    Expr::Var(frame),
                    Expr::Int(i64::from(entry)),
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
        match self.init_cells.checked_add(cells) {
            Some(end) if end <= MAX_REALM_INITIAL_CELLS => {
                self.init_cells = end;
                Ok(())
            }
            end => {
                #[cfg(test)]
                note_realm_budget(end.unwrap_or(u64::MAX), true);
                #[cfg(not(test))]
                let _ = end;
                Err(self.error(
                    span,
                    "JavaScript ES5 realm initialization exceeds the 65536 cell budget",
                ))
            }
        }
    }

    /// Charge one static node request against the realm initialization
    /// budget: the payload cells, the raw block header, and a directory page
    /// whenever this request starts a new page. `begin_node` uses this for
    /// every reservation it emits, and a site whose private guest helper
    /// allocates a fixed node set on its behalf charges those exact payloads
    /// through the same arithmetic, so the static budget and the runtime
    /// constructor limits stay in agreement. Only accounting is shared: every
    /// runtime heap/root/slot limit keeps its own check.
    fn charge_node_request(
        &mut self,
        count: i64,
        span: swc_common::Span,
    ) -> Result<(), ParseError> {
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
        self.charge_node_request(count, span)?;
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

    /// Guest string-equality helper for strict (`===`) matching outside the
    /// binary-expression owner: the owning carrier plus the flat comparator
    /// index, or `None` on the heap-free scalar path where string values
    /// cannot occur.
    pub(super) fn string_equality(&self, span: swc_common::Span) -> Option<value::StringEquality> {
        match (self.active_carrier(span).ok(), self.string_eq) {
            (Some(carrier), Some(index)) => Some(value::StringEquality {
                carrier: Expr::Var(carrier),
                index,
            }),
            _ => None,
        }
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
    /// (`"\u0075se strict"`, `"\\u2028"` text, any braced spelling) are
    /// different raw bytes, so they stay ordinary string expressions and
    /// never activate strict mode — the shared codec accepts their Micro
    /// braced escapes as string data, but a Use Strict Directive is a
    /// source-spelling property, not a decoded-value property.
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
        self.expr_with(expression, CallResultRooting::Temporary)
    }

    /// One expression dispatcher with the caller's rooting destination. Only
    /// the statement owner that proves an immediate local handoff passes
    /// `ImmediateLocalInitializer`; every recursive operand stays `Temporary`.
    pub(super) fn expr_with(
        &mut self,
        expression: &ast::Expr,
        rooting: CallResultRooting,
    ) -> Result<Lowered, ParseError> {
        if !self.in_dead_capability() && self.statement_is_dead(expression.span()) {
            // Validation reached this operand only after an abrupt point and
            // marked it capability-only. Keep every static admission, but do
            // not materialize any prepared Reference proof for discarded IR.
            // The dead re-walk stays Temporary: a discarded value has no
            // binding destination to receive an immediate handoff.
            return self.with_dead_capability(|lower| lower.expr(expression));
        }
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
            ast::Expr::Lit(ast::Lit::Regex(literal)) => self.lower_regexp_literal(literal),
            ast::Expr::Paren(p) => self.expr_with(&p.expr, rooting),
            ast::Expr::Ident(i) => reference::lower_get(self, i),
            ast::Expr::Assign(assign) => reference::lower_assign_expr(self, assign),
            ast::Expr::Update(update) => self.lower_update(update),
            ast::Expr::Unary(u) if u.op == ast::UnaryOp::TypeOf => reference::lower_typeof(self, u),
            ast::Expr::Unary(u) if u.op == ast::UnaryOp::Bang => self.lower_not(u),
            ast::Expr::Unary(u) if u.op == ast::UnaryOp::Delete => self.lower_delete(u),
            ast::Expr::Unary(u) if u.op == ast::UnaryOp::Void => self.lower_void(u),
            ast::Expr::Unary(u) if u.op == ast::UnaryOp::Plus => self.lower_unary_plus(u),
            ast::Expr::Bin(b) if b.op == ast::BinaryOp::In => self.lower_in(b),
            ast::Expr::Bin(b)
                if matches!(b.op, ast::BinaryOp::LogicalAnd | ast::BinaryOp::LogicalOr) =>
            {
                self.lower_logical(b)
            }
            ast::Expr::Array(literal) => self.lower_array_expr(literal),
            ast::Expr::Unary(u) if u.op == ast::UnaryOp::Minus => {
                let operand = self.expr(&u.arg)?;
                if operand.kind != Kind::Number
                    || (self.pending_normal.is_some()
                        && reference::numeric_operand_may_throw(&u.arg))
                {
                    return self.lower_primitive_unary(u, operand);
                }
                reference::admit_numeric_operand(self, u.span, operand.kind)?;
                let (slot, statement) = self.bind_evaluation(operand.expr, operand.kind, u.span)?;
                Ok(Lowered {
                    expr: Expr::Block {
                        stmts: statement,
                        expr: Box::new(value::number(Expr::Neg(Box::new(
                            self.numeric_projection(slot, operand.kind),
                        )))),
                    },
                    kind: Kind::Number,
                    array_length: None,
                    owned_roots: operand.owned_roots,
                })
            }
            ast::Expr::Bin(b) if abstract_equality::Op::from_op(b.op).is_some() => {
                self.lower_abstract_equality(b)
            }
            ast::Expr::Bin(b) if relational::Relation::from_op(b.op).is_some() => {
                self.lower_relational(b)
            }
            ast::Expr::Unary(u) if u.op == ast::UnaryOp::Tilde => self.lower_bitwise_not(u),
            ast::Expr::Bin(b) if bitwise::Operation::from_op(b.op).is_some() => {
                self.lower_bitwise(b)
            }
            ast::Expr::Bin(b) if b.op == ast::BinaryOp::Mod => self.lower_remainder(b),
            ast::Expr::Bin(b) => {
                if !matches!(
                    b.op,
                    ast::BinaryOp::Add
                        | ast::BinaryOp::Sub
                        | ast::BinaryOp::Mul
                        | ast::BinaryOp::Div
                        | ast::BinaryOp::Exp
                        | ast::BinaryOp::EqEqEq
                        | ast::BinaryOp::NotEqEq
                ) {
                    return Err(self.error(b.span, "unsupported JavaScript ES5 P0 binary operator"));
                }
                // A sealed joined primitive-addition site is authoritative.
                // A loop back edge can leave a stale exact String-concat
                // registration behind (for example String+Number from the
                // entry walk), but its converters only fit that narrow pair.
                // Dispatching on the stale concat would silently re-apply
                // the old exact pair to the widened operands.
                if b.op == ast::BinaryOp::Add && self.has_primitive_addition_site(b) {
                    return self.lower_primitive_addition(b);
                }
                if b.op == ast::BinaryOp::Add && self.has_string_concat_site(b) {
                    return self.lower_string_concat(b);
                }
                let left = self.expr(&b.left)?;
                let right = self.expr(&b.right)?;
                if primitive_number::Operator::arithmetic(b.op).is_some()
                    && (left.kind != Kind::Number || right.kind != Kind::Number)
                {
                    return self.lower_primitive_binary(b, left, right);
                }
                let equality = matches!(b.op, ast::BinaryOp::EqEqEq | ast::BinaryOp::NotEqEq);
                if !equality {
                    reference::admit_numeric_operand(self, b.span, left.kind)?;
                    reference::admit_numeric_operand(self, b.span, right.kind)?;
                }
                let mut owned_roots = left.owned_roots;
                owned_roots.extend(right.owned_roots);
                let (l, ls) = self.bind_evaluation(left.expr, left.kind, b.left.span())?;
                owned_roots.push(l);
                // P6B-ABRUPT-SIBLING-PREFIX: stage right evaluation behind a
                // Normal recheck. A throwing left prefix must stop later
                // operand evaluation, key construction and allocation.
                // Dominating typed placeholders keep the join well-formed
                // without committing a source value while abrupt.
                if let Some(normal) = self.pending_normal {
                    let line = self.line(b.span);
                    let result_kind = if equality {
                        Kind::Boolean
                    } else {
                        Kind::Number
                    };
                    let (result_slot, result_init) = if equality {
                        self.bind_temp_bare(value::boolean(Expr::Bool(false)), b.span)?
                    } else {
                        self.bind_temp_bare(value::number(Expr::Float(0.0)), b.span)?
                    };
                    let mut stmts = ls;
                    stmts.push(result_init);
                    let mut then_branch = Vec::new();
                    let (r, mut rs) =
                        self.bind_evaluation(right.expr, right.kind, b.right.span())?;
                    then_branch.append(&mut rs);
                    owned_roots.push(r);
                    let owned_roots = Self::compose_owned(owned_roots.into_iter().filter(|_| true));
                    // Filter to rooted slots only for the caller's release.
                    let mut filtered: Vec<u16> = Vec::new();
                    for slot in owned_roots {
                        if self.has_root(slot) {
                            filtered.push(slot);
                        }
                    }
                    if equality {
                        let carrier = self.active_carrier(b.span).ok();
                        let strings = match (carrier, self.string_eq) {
                            (Some(carrier), Some(index)) => Some(value::StringEquality {
                                carrier: Expr::Var(carrier),
                                index,
                            }),
                            _ => None,
                        };
                        if b.op == ast::BinaryOp::NotEqEq {
                            let flag_strings =
                                match (self.active_carrier(b.span).ok(), self.string_eq) {
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
                            then_branch.push(flag_stmt);
                            then_branch.push(Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: result_slot,
                                expr: value::boolean(Expr::Not(Box::new(Expr::Var(flag_slot)))),
                                line,
                            });
                        } else {
                            let equal = value::strict_equal(l, r, strings);
                            then_branch.push(Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: result_slot,
                                expr: equal,
                                line,
                            });
                        }
                    } else {
                        let a = Box::new(self.numeric_projection(l, left.kind));
                        let c = Box::new(self.numeric_projection(r, right.kind));
                        let number = match b.op {
                            ast::BinaryOp::Add => Expr::Add(a, c),
                            ast::BinaryOp::Sub => Expr::Sub(a, c),
                            ast::BinaryOp::Mul => Expr::Mul(a, c),
                            ast::BinaryOp::Div => Expr::Div(a, c),
                            ast::BinaryOp::Exp => {
                                self.bind_number_power(l, left.kind, r, right.kind, b.span)?
                            }
                            _ => unreachable!("operator checked above"),
                        };
                        then_branch.push(Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: result_slot,
                            expr: value::number(number),
                            line,
                        });
                    }
                    stmts.push(Stmt::IfElse {
                        condition: Expr::Var(normal),
                        then_branch,
                        else_branch: Vec::new(),
                        line,
                    });
                    return Ok(Lowered {
                        expr: Expr::Block {
                            stmts,
                            expr: Box::new(Expr::Var(result_slot)),
                        },
                        kind: result_kind,
                        array_length: None,
                        owned_roots: filtered,
                    });
                }
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
                    let a = Box::new(self.numeric_projection(l, left.kind));
                    let c = Box::new(self.numeric_projection(r, right.kind));
                    let number = match b.op {
                        ast::BinaryOp::Add => Expr::Add(a, c),
                        ast::BinaryOp::Sub => Expr::Sub(a, c),
                        ast::BinaryOp::Mul => Expr::Mul(a, c),
                        ast::BinaryOp::Div => Expr::Div(a, c),
                        ast::BinaryOp::Exp => {
                            self.bind_number_power(l, left.kind, r, right.kind, b.span)?
                        }
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
            ast::Expr::Fn(f) => self.lower_function_expr(f, None),
            ast::Expr::Call(c) => self.lower_call_expr(c, rooting),
            ast::Expr::New(c) => self.lower_new_expr(c),
            ast::Expr::Cond(branch) => self.lower_conditional(branch),
            ast::Expr::Seq(sequence) => self.lower_sequence(sequence),
            _ => Err(self.error(
                expression.span(),
                "unsupported JavaScript ES5 P0 primitive expression",
            )),
        }
    }

    /// Accepted managed Number-to-String producer, emitted before the runtime
    /// index window for every realm that installs the ordinary prototype.
    pub(super) fn number_string_managed(&self) -> u16 {
        self.number_string_entries
            .as_ref()
            .expect("ordinary prototype requires Number-to-String")
            .managed
    }

    /// Accepted fixed scalar (Boolean/Null/Undefined) String producer.
    pub(super) fn fixed_scalar_string(&self) -> u16 {
        self.helper_fixed_scalar_string
            .expect("ordinary prototype requires fixed scalar Strings")
    }

    fn numeric_projection(&self, slot: u16, kind: Kind) -> Expr {
        if kind != Kind::Dynamic {
            return value::field(slot, "number");
        }
        let checked = super::prelude::math::checked_number(Expr::Var(slot));
        if let Some(normal) = self.pending_normal {
            Expr::IfElse {
                condition: Box::new(Expr::Var(normal)),
                then_expr: Box::new(checked),
                else_expr: Box::new(Expr::Float(0.0)),
            }
        } else {
            checked
        }
    }

    fn lower_member_read(&mut self, member: &ast::MemberExpr) -> Result<Lowered, ParseError> {
        // Direct `arguments` observations inside a source activation: the
        // invocation vector supplies `.length` and numeric element reads.
        // No heap, no roots, no ordinary realm, and no aliasing of formals.
        if super::reference::is_arguments_base(member)
            && self.function_args.is_some()
            && super::reference::implicit_arguments(self)
        {
            return self.lower_arguments_read(member);
        }
        if self.has_number_key_site(member) {
            return self.lower_number_key_read(member);
        }
        if self.primitive_key_get_mode(member).is_some() {
            // A Dynamic receiver with a proved Number key may hold either an
            // ordinary object or one of the two dense-array specializations
            // at runtime (call results and conditional joins). Emit the
            // kind-dispatched consumer instead of the plain string Reference.
            if self.primitive_key_dynamic_number(member, primitive_key::Role::Get) {
                return self.lower_dynamic_numeric_get(member);
            }
            return self.lower_primitive_get(member);
        }
        if let ast::Expr::Ident(object) = member.obj.as_ref()
            && let Some(&(_, kind)) = self.bindings.get(object.sym.as_str())
            && kind == Kind::Array
        {
            return self.lower_array_member_read(member, object.sym.as_str());
        }
        // Discarded text: the live read selects its primitive-key Reference
        // consumer from the sealed proof, which a capability-only walk never
        // publishes. Keep the same receiver/key admission and emit no
        // executable Reference, helper or realm demand.
        if (self.in_dead_capability() || self.emission_known_throw())
            && let Some(key_expr) = Self::open_computed_key(member)
        {
            let base = self.expr(&member.obj)?;
            let key = self.expr(key_expr)?;
            if self.discard_member_capability(
                member,
                base.kind,
                key.kind,
                primitive_key::Role::Get,
            )? {
                return Ok(Self::capability_only_read());
            }
        }
        // If this is a computed integer subscript but the base is not an
        // admitted array, keep the original precise array diagnostics when
        // the base is array-like; otherwise fall through to the object path
        // which gives the object-specific key diagnostic.
        if let ast::MemberProp::Computed(property) = &member.prop
            && Self::member_index_literal(&property.expr).is_some()
        {
            if let ast::Expr::Ident(object) = member.obj.as_ref()
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

    /// Direct `arguments` read: `.length` or one numeric element index of the
    /// retained invocation vector. No allocation for the length/literal
    /// forms, no rooting, no ordinary realm. Out-of-range and non-integral
    /// keys are genuine Undefined; a dynamic key is checked for finiteness,
    /// integrality and range before `Get` (which traps out of range).
    fn lower_arguments_read(&mut self, member: &ast::MemberExpr) -> Result<Lowered, ParseError> {
        use super::reference::ArgumentsKey;
        let args = self.function_args.ok_or_else(|| {
            self.error(
                member.span,
                "P4 arguments observations require the invocation argument vector",
            )
        })?;
        let Some(shape) = super::reference::arguments_member_key(self, member)? else {
            return Err(self.error(
                member.span,
                "P4 arguments observations admit only .length, .callee and numeric indexes",
            ));
        };
        match shape {
            ArgumentsKey::Length => Ok(Lowered {
                expr: value::number(Self::as_number(Self::builtin_call(
                    BuiltinFunction::Len.call_index(),
                    vec![Expr::Var(args)],
                ))),
                kind: Kind::Number,
                array_length: None,
                owned_roots: Vec::new(),
            }),
            // Pinned Micro: `arguments.callee` reads as Undefined.
            ArgumentsKey::Callee => Ok(Lowered {
                expr: value::undefined(),
                kind: Kind::Undefined,
                array_length: None,
                owned_roots: Vec::new(),
            }),
            ArgumentsKey::Absent => Ok(Lowered {
                expr: value::undefined(),
                kind: Kind::Undefined,
                array_length: None,
                owned_roots: Vec::new(),
            }),
            ArgumentsKey::Static(index) => Ok(Lowered {
                expr: Expr::IfElse {
                    condition: Box::new(Expr::Lt(
                        Box::new(Expr::Int(index)),
                        Box::new(Self::builtin_call(
                            BuiltinFunction::Len.call_index(),
                            vec![Expr::Var(args)],
                        )),
                    )),
                    then_expr: Box::new(Self::builtin_call(
                        BuiltinFunction::Get.call_index(),
                        vec![
                            Expr::Borrow(Box::new(Expr::Var(args))),
                            Expr::Int(index),
                        ],
                    )),
                    else_expr: Box::new(value::undefined()),
                },
                kind: Kind::Dynamic,
                array_length: None,
                owned_roots: Vec::new(),
            }),
            ArgumentsKey::Dynamic => {
                let ast::MemberProp::Computed(property) = &member.prop else {
                    unreachable!("dynamic arguments key is computed");
                };
                let key = self.expr(&property.expr)?;
                if !matches!(key.kind, Kind::Number | Kind::String | Kind::Dynamic) {
                    return Err(self.error(
                        property.span(),
                        "P4 arguments subscripts must be numeric or String in this scope",
                    ));
                }
                let mut owned = key.owned_roots;
                let key_kind = key.kind;
                let (key_slot, stmts) =
                    self.bind_evaluation(key.expr, key.kind, property.span())?;
                owned.push(key_slot);
                // Pinned Micro materializes the binding as a real Array, so a
                // String subscript is an ordinary property name and a Number
                // subscript is an element index. A Dynamic key can be either at
                // runtime, so the tag chooses the same two consumers instead of
                // trapping on a String key.
                let result = match key_kind {
                    Kind::String => {
                        self.lower_arguments_string_key_read(args, key_slot, member.span)?
                    }
                    Kind::Number => {
                        self.lower_arguments_numeric_key_read(args, key_slot, key_kind, property.span(), member.span)?
                    }
                    _ => {
                        let string_key =
                            self.lower_arguments_string_key_read(args, key_slot, member.span)?;
                        let numeric_key = self.lower_arguments_numeric_key_read(
                            args,
                            key_slot,
                            key_kind,
                            property.span(),
                            member.span,
                        )?;
                        Expr::IfElse {
                            condition: Box::new(Expr::Eq(
                                Box::new(Self::as_number(value::field(key_slot, "tag"))),
                                Box::new(Self::as_number(Expr::Int(value::STRING))),
                            )),
                            then_expr: Box::new(string_key),
                            else_expr: Box::new(numeric_key),
                        }
                    }
                };
                Ok(Lowered {
                    expr: Expr::Block {
                        stmts,
                        expr: Box::new(result),
                    },
                    kind: Kind::Dynamic,
                    array_length: None,
                    owned_roots: owned,
                })
            }
        }
    }

    /// Bind the activation's single guest `arguments` object and return it as
    /// the call actual. The object is created from the invocation vector on the
    /// first pass and mirrored from the vector on every later pass, so the
    /// callee always observes one writable Array (pinned Micro identity)
    /// rather than a fresh copy per call.
    fn prepare_arguments_object(
        &mut self,
        args: u16,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
    ) -> Result<Expr, ParseError> {
        let object = self.arguments_object.ok_or_else(|| {
            self.error(
                span,
                "Array.isArray(arguments) requires the activation arguments object",
            )
        })?;
        let sync = self.arguments_sync.ok_or_else(|| {
            self.error(
                span,
                "Array.isArray(arguments) requires the private arguments mirror",
            )
        })?;
        let create = self.arguments_array_from_values.ok_or_else(|| {
            self.error(
                span,
                "Array.isArray(arguments) requires the private array boxing helper",
            )
        })?;
        let carrier = self.active_carrier(span)?;
        let line = self.line(span);
        let mut created = vec![Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: object,
            expr: Self::guest_call(create, vec![Expr::Var(carrier), Expr::Var(args)]),
            line,
        }];
        self.refresh_root(object, span, &mut created)?;
        stmts.push(Stmt::IfElse {
            condition: Expr::Eq(
                Box::new(Self::as_number(value::field(object, "tag"))),
                Box::new(Self::as_number(Expr::Int(value::OBJECT))),
            ),
            then_branch: vec![Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: args,
                expr: Self::guest_call(
                    sync,
                    vec![
                        Expr::Var(carrier),
                        Expr::Var(args),
                        Expr::Var(object),
                        Expr::Int(1),
                    ],
                ),
                line,
            }],
            else_branch: created,
            line,
        });
        Ok(Expr::Var(object))
    }

    /// Mirror the guest `arguments` object back into the invocation vector after
    /// a pass, so a rewritten `Array.isArray` that writes `a[i]` (or resizes the
    /// object) is observed by the activation's own `arguments[i]`/`.length`.
    fn finish_arguments_object(
        &mut self,
        args: u16,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let object = self.arguments_object.ok_or_else(|| {
            self.error(
                span,
                "Array.isArray(arguments) requires the activation arguments object",
            )
        })?;
        let sync = self.arguments_sync.ok_or_else(|| {
            self.error(
                span,
                "Array.isArray(arguments) requires the private arguments mirror",
            )
        })?;
        let carrier = self.active_carrier(span)?;
        stmts.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: args,
            expr: Self::guest_call(
                sync,
                vec![
                    Expr::Var(carrier),
                    Expr::Var(args),
                    Expr::Var(object),
                    Expr::Int(0),
                ],
            ),
            line: self.line(span),
        });
        Ok(())
    }

    /// `arguments[<runtime String>]`: pinned Micro materializes the binding as
    /// a real Array, so the String is an ordinary property name. The private
    /// classifier maps it to a canonical index (`>= 0`), the `length` row
    /// (`-1`) or an absent own property (`-2`); the resulting element/length
    /// read reuses the invocation vector's existing consumers. Nothing here
    /// allocates and no guest call is made.
    fn lower_arguments_string_key_read(
        &mut self,
        args: u16,
        key_slot: u16,
        span: swc_common::Span,
    ) -> Result<Expr, ParseError> {
        let helper = self.arguments_string_key.ok_or_else(|| {
            self.error(
                span,
                "arguments String subscripts require the private key classifier",
            )
        })?;
        let carrier = self.active_carrier(span)?;
        let line = self.line(span);
        let code = self.slot(span)?;
        let length = value::number(Self::as_number(Self::builtin_call(
            BuiltinFunction::Len.call_index(),
            vec![Expr::Var(args)],
        )));
        let element = Self::builtin_call(
            BuiltinFunction::Get.call_index(),
            vec![
                Expr::Borrow(Box::new(Expr::Var(args))),
                Expr::Var(code),
            ],
        );
        let present = Expr::And(
            Box::new(Expr::Not(Box::new(Expr::Lt(
                Box::new(Expr::Var(code)),
                Box::new(Expr::Int(0)),
            )))),
            Box::new(Expr::Lt(
                Box::new(Expr::Var(code)),
                Box::new(Self::builtin_call(
                    BuiltinFunction::Len.call_index(),
                    vec![Expr::Var(args)],
                )),
            )),
        );
        Ok(Expr::Block {
            stmts: vec![Stmt::Let {
                index: code,
                declared_schema: Some(TypeSchema::Int),
                expr: Self::guest_call(helper, vec![Expr::Var(carrier), Expr::Var(key_slot)]),
                line,
            }],
            expr: Box::new(Expr::IfElse {
                condition: Box::new(Expr::Eq(
                    Box::new(Expr::Var(code)),
                    Box::new(Expr::Int(-1)),
                )),
                then_expr: Box::new(length),
                else_expr: Box::new(Expr::IfElse {
                    condition: Box::new(present),
                    then_expr: Box::new(element),
                    else_expr: Box::new(value::undefined()),
                }),
            }),
        })
    }

    /// `arguments[<runtime Number>]`: finite, integral, non-negative indexes
    /// inside the invocation vector read one element; every other Number is a
    /// genuine Undefined. `index == len` is out of range for a read, so no
    /// `Get` is performed outside the bound.
    fn lower_arguments_numeric_key_read(
        &mut self,
        args: u16,
        key_slot: u16,
        key_kind: Kind,
        key_span: swc_common::Span,
        span: swc_common::Span,
    ) -> Result<Expr, ParseError> {
        let (number_slot, number_init) = self.bind_typed(
            self.array_index_number(key_slot, key_kind),
            TypeSchema::Float,
            key_span,
        )?;
        let index_slot = self.slot(span)?;
        let index_init = Stmt::Let {
            index: index_slot,
            declared_schema: Some(TypeSchema::Int),
            expr: Self::builtin_call(
                BuiltinFunction::CheckedIntFromNumber.call_index(),
                vec![Expr::Var(number_slot)],
            ),
            line: self.line(span),
        };
        let in_range = Expr::Lt(
            Box::new(Expr::Var(index_slot)),
            Box::new(Self::builtin_call(
                BuiltinFunction::Len.call_index(),
                vec![Expr::Var(args)],
            )),
        );
        let guard = Expr::And(
            Box::new(Self::builtin_call(
                BuiltinFunction::MathIsFinite.call_index(),
                vec![Expr::Var(number_slot)],
            )),
            Box::new(Expr::And(
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(number_slot)),
                    Box::new(Self::builtin_call(
                        BuiltinFunction::MathTrunc.call_index(),
                        vec![Expr::Var(number_slot)],
                    )),
                )),
                Box::new(Expr::And(
                    Box::new(Expr::Not(Box::new(Expr::Lt(
                        Box::new(Expr::Var(number_slot)),
                        Box::new(Expr::Float(0.0)),
                    )))),
                    Box::new(Expr::Lt(
                        Box::new(Expr::Var(number_slot)),
                        Box::new(Expr::Float(9_223_372_036_854_775_808.0)),
                    )),
                )),
            )),
        );
        let read = Expr::IfElse {
            condition: Box::new(guard),
            then_expr: Box::new(Expr::Block {
                stmts: vec![index_init],
                expr: Box::new(Expr::IfElse {
                    condition: Box::new(in_range),
                    then_expr: Box::new(Self::builtin_call(
                        BuiltinFunction::Get.call_index(),
                        vec![
                            Expr::Borrow(Box::new(Expr::Var(args))),
                            Expr::Var(index_slot),
                        ],
                    )),
                    else_expr: Box::new(value::undefined()),
                }),
            }),
            else_expr: Box::new(value::undefined()),
        };
        // The Float projection is a `Let` before the guard so a String/Dynamic
        // key never reaches the Number assert of a non-Number payload.
        Ok(Expr::Block {
            stmts: vec![number_init],
            expr: Box::new(read),
        })
    }

    /// Store one already-evaluated value at a canonical numeric `arguments`
    /// index. The element lives in the activation's invocation vector, so a
    /// later `arguments[i]` read observes it while a formal stays untouched:
    /// the pinned engine has no parameter mapping in either mode. A write past
    /// the current length is the pinned guest
    /// `TypeError("invalid array subscript")`, never a VM-level host error;
    /// `index == len` appends and `index < len` overwrites.
    fn lower_arguments_store(
        &mut self,
        member: &ast::MemberExpr,
        right: &ast::Expr,
        span: swc_common::Span,
    ) -> Result<Lowered, ParseError> {
        use super::reference::ArgumentsKey;
        let args = self.function_args.ok_or_else(|| {
            self.error(
                member.span,
                "P4 arguments writes require the invocation argument vector",
            )
        })?;
        let shape = super::reference::arguments_member_key(self, member)?;
        let index = match shape {
            Some(ArgumentsKey::Static(index)) => index,
            Some(ArgumentsKey::Length) => {
                return self.lower_arguments_length_store(args, right, span);
            }
            _ => {
                return Err(self.error(
                    member.span,
                    "P4 arguments writes admit only a canonical numeric index",
                ));
            }
        };
        let rhs = self.expr(right)?;
        let mut owned = rhs.owned_roots;
        let (value_slot, mut stmts) = self.bind_evaluation(rhs.expr, rhs.kind, right.span())?;
        owned.push(value_slot);
        let line = self.line(span);
        let length_slot = self.slot(span)?;
        stmts.push(Stmt::Let {
            index: length_slot,
            declared_schema: Some(TypeSchema::Int),
            expr: Self::builtin_call(BuiltinFunction::Len.call_index(), vec![Expr::Var(args)]),
            line,
        });
        let store = vec![Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: args,
            expr: Self::builtin_call(
                BuiltinFunction::Set.call_index(),
                vec![Expr::Var(args), Expr::Int(index), Expr::Var(value_slot)],
            ),
            line,
        }];
        // The Core invocation vector is opaque to the collector, so a fresh
        // object written here would be unrooted until the next mirror sync.
        // When the activation owns a guest `arguments` object, write the same
        // element through to it: a real heap edge keeps the value alive.
        let mut store_through = store;
        self.mirror_arguments_store(args, span, &mut store_through)?;
        let mut reject = Vec::new();
        let error = self.emit_arguments_type_error("invalid array subscript", span, &mut reject)?;
        let thrown = super::prelude::object::result(
            value::value_result_schema(),
            false,
            Expr::Var(error),
            self.ir_span(span),
        );
        let _ = self.consume_value_result(thrown, span, &mut reject)?;
        stmts.push(Stmt::IfElse {
            // `index == len` appends; only `index > len` is the jumping write.
            condition: Expr::Not(Box::new(Expr::Gt(
                Box::new(Expr::Int(index)),
                Box::new(Expr::Var(length_slot)),
            ))),
            then_branch: store_through,
            else_branch: reject,
            line,
        });
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(Expr::Var(value_slot)),
            },
            kind: Kind::Dynamic,
            array_length: None,
            owned_roots: owned,
        })
    }

    /// Root a source-written `arguments[<index>]` value until the next
    /// mirror pass by refreshing the activation's guest `arguments` object
    /// from the invocation vector. The Core vector is opaque to the
    /// collector, so without a heap edge the fresh object written here is
    /// collectable immediately after the statement's temporary root retires.
    ///
    /// This reuses the same creation/sync pair every `Array.isArray`
    /// observation runs: `prepare_arguments_object` creates the object from
    /// the vector when it does not exist yet and otherwise resizes it in
    /// place and copies every element, so a write at `index == length`
    /// (append) and a write into a hole both land inside the resized node.
    /// Callers have already refused `index > vector.length` with the guest
    /// `TypeError`, so this never feeds `array_put` an out-of-extent index.
    fn mirror_arguments_store(
        &mut self,
        args: u16,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        if self.arguments_object.is_none() || self.arguments_array_from_values.is_none() {
            return Ok(());
        }
        let _ = self.prepare_arguments_object(args, span, stmts)?;
        Ok(())
    }

    /// `arguments.length = value`: a real resize of the invocation vector.
    /// The pinned engine truncates on a smaller length (cleared elements are
    /// gone for ever), extends with holes that read as Undefined, keeps the
    /// RHS side effects and yields the RHS value. A value outside
    /// `0 .. 2^30-1` after the ToUint32/ToInt32 conversion raises the guest
    /// `TypeError("invalid array length")`.
    fn lower_arguments_length_store(
        &mut self,
        args: u16,
        right: &ast::Expr,
        span: swc_common::Span,
    ) -> Result<Lowered, ParseError> {
        let rhs = self.expr(right)?;
        let mut owned = rhs.owned_roots;
        let (rhs_slot, mut stmts) = self.bind_evaluation(rhs.expr, rhs.kind, right.span())?;
        owned.push(rhs_slot);
        let line = self.line(span);
        let carrier = self.active_carrier(span)?;
        let number = self.slot(span)?;
        let trunc = self.slot(span)?;
        let len_float = self.slot(span)?;
        let len_int = self.slot(span)?;
        let mut after_array = Vec::new();
        let (subject, proceed) = self.prepare_length_subject(rhs_slot, span, &mut after_array)?;
        let mut body = Vec::new();
        let tag_of = Self::as_number(value::field(subject, "tag"));
        let is_tag = |code| {
            Expr::Eq(
                Box::new(tag_of.clone()),
                Box::new(Self::as_number(Expr::Int(code))),
            )
        };
        let parser = self.published_number_parser();
        let string_number = match parser {
            Some(parser) => Self::guest_call(
                parser,
                vec![
                    Expr::Var(carrier),
                    Expr::Var(subject),
                    Expr::Int(0),
                    Expr::Float(0.0),
                ],
            ),
            None => Expr::Float(0.0),
        };
        let boolean_number = Expr::IfElse {
            condition: Box::new(value::field(subject, "flag")),
            then_expr: Box::new(Expr::Float(1.0)),
            else_expr: Box::new(Expr::Float(0.0)),
        };
        body.push(Stmt::Let {
            index: number,
            declared_schema: Some(TypeSchema::Float),
            expr: Expr::IfElse {
                condition: Box::new(is_tag(value::NUMBER)),
                then_expr: Box::new(Self::as_number(value::field(subject, "number"))),
                else_expr: Box::new(Expr::IfElse {
                    condition: Box::new(is_tag(value::NULL)),
                    then_expr: Box::new(Expr::Float(0.0)),
                    else_expr: Box::new(Expr::IfElse {
                        condition: Box::new(is_tag(value::BOOLEAN)),
                        then_expr: Box::new(boolean_number),
                        else_expr: Box::new(Expr::IfElse {
                            condition: Box::new(is_tag(value::STRING)),
                            then_expr: Box::new(string_number),
                            else_expr: Box::new(Expr::IfElse {
                                condition: Box::new(is_tag(value::UNDEFINED)),
                                then_expr: Box::new(Expr::Float(f64::NAN)),
                                else_expr: Box::new(Expr::Float(0.0)),
                            }),
                        }),
                    }),
                }),
            },
            line,
        });
        body.push(Stmt::Let {
            index: trunc,
            declared_schema: Some(TypeSchema::Float),
            expr: Self::builtin_call(
                BuiltinFunction::MathTrunc.call_index(),
                vec![Expr::Var(number)],
            ),
            line,
        });
        // ES5 ToUint32, then signed ToInt32. 2^32 wraps to 0 before the
        // Micro length bound; values still outside 0..2^30-1 stay TypeError.
        let modulus = Expr::Float(4_294_967_296.0);
        let wrapped = Expr::Sub(
            Box::new(Expr::Var(trunc)),
            Box::new(Expr::Mul(
                Box::new(Self::builtin_call(
                    BuiltinFunction::MathFloor.call_index(),
                    vec![Expr::Div(
                        Box::new(Expr::Var(trunc)),
                        Box::new(modulus.clone()),
                    )],
                )),
                Box::new(modulus),
            )),
        );
        let uint32 = Expr::IfElse {
            condition: Box::new(Self::builtin_call(
                BuiltinFunction::MathIsFinite.call_index(),
                vec![Expr::Var(trunc)],
            )),
            then_expr: Box::new(wrapped),
            else_expr: Box::new(Expr::Float(0.0)),
        };
        body.push(Stmt::Let {
            index: len_float,
            declared_schema: Some(TypeSchema::Float),
            expr: Expr::IfElse {
                condition: Box::new(Expr::Not(Box::new(Expr::Lt(
                    Box::new(uint32.clone()),
                    Box::new(Expr::Float(2_147_483_648.0)),
                )))),
                then_expr: Box::new(Expr::Sub(
                    Box::new(uint32.clone()),
                    Box::new(Expr::Float(4_294_967_296.0)),
                )),
                else_expr: Box::new(uint32),
            },
            line,
        });
        let in_range = Expr::And(
            Box::new(Expr::Not(Box::new(Expr::Lt(
                Box::new(Expr::Var(len_float)),
                Box::new(Expr::Float(0.0)),
            )))),
            Box::new(Expr::Not(Box::new(Expr::Gt(
                Box::new(Expr::Var(len_float)),
                Box::new(Expr::Float(1_073_741_823.0)),
            )))),
        );
        let tag_ok = Expr::Or(
            Box::new(Expr::Or(
                Box::new(Expr::Or(
                    Box::new(is_tag(value::NUMBER)),
                    Box::new(is_tag(value::NULL)),
                )),
                Box::new(Expr::Or(
                    Box::new(is_tag(value::BOOLEAN)),
                    Box::new(is_tag(value::UNDEFINED)),
                )),
            )),
            Box::new(if parser.is_some() {
                is_tag(value::STRING)
            } else {
                Expr::Bool(false)
            }),
        );
        let accept = vec![
            Stmt::Let {
                index: len_int,
                declared_schema: Some(TypeSchema::Int),
                expr: Self::builtin_call(
                    BuiltinFunction::CheckedIntFromNumber.call_index(),
                    vec![Expr::Var(len_float)],
                ),
                line,
            },
            // Truncate: `Slice(vector, 0, n)` is the only Core primitive that
            // returns a shorter array. The invocation vector is a Core
            // `Array<JsValue>` (so `Array.isArray(arguments)` stays true), not
            // a heap node, hence it cannot take the heap `set_length` helper.
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: args,
                expr: Self::builtin_call(
                    BuiltinFunction::Slice.call_index(),
                    vec![Expr::Var(args), Expr::Int(0), Expr::Var(len_int)],
                ),
                line,
            },
            // Extend: holes read as Undefined in pinned Micro, and a Core
            // array has no hole representation, so append Undefined records
            // until the physical length agrees with the requested length.
            Stmt::While {
                condition: Expr::Lt(
                    Box::new(Self::builtin_call(
                        BuiltinFunction::Len.call_index(),
                        vec![Expr::Var(args)],
                    )),
                    Box::new(Expr::Var(len_int)),
                ),
                body: vec![Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: args,
                    expr: Self::builtin_call(
                        BuiltinFunction::ArrayPush.call_index(),
                        vec![Expr::Var(args), value::undefined()],
                    ),
                    line,
                }],
                line,
            },
        ];
        let mut reject = Vec::new();
        let error = self.emit_arguments_type_error("invalid array length", span, &mut reject)?;
        let thrown = super::prelude::object::result(
            value::value_result_schema(),
            false,
            Expr::Var(error),
            self.ir_span(span),
        );
        let _ = self.consume_value_result(thrown, span, &mut reject)?;
        body.push(Stmt::IfElse {
            condition: Expr::And(Box::new(tag_ok), Box::new(in_range)),
            then_branch: accept,
            else_branch: reject,
            line,
        });
        if let Some(proceed) = proceed {
            after_array.push(Stmt::IfElse {
                condition: Expr::Var(proceed),
                then_branch: body,
                else_branch: Vec::new(),
                line,
            });
        } else {
            after_array.extend(body);
        }
        stmts.append(&mut after_array);
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(Expr::Var(rhs_slot)),
            },
            kind: Kind::Dynamic,
            array_length: None,
            owned_roots: owned,
        })
    }

    /// Build one branded realm `TypeError` with an own `message` row. The
    /// allocation uses a reserved frame entry and the private allocator, so
    /// the error is a normal guest value that `try/catch` can observe; the
    /// two fixed message Strings are interned runtime nodes.
    fn emit_arguments_type_error(
        &mut self,
        message: &str,
        span: swc_common::Span,
        statements: &mut Vec<Stmt>,
    ) -> Result<u16, ParseError> {
        let carrier = self.active_carrier(span)?;
        let heap = self.heap_ref(span)?;
        let checked_base = heap.helpers.checked_base;
        let helper_write = heap.helper_write;
        let intrinsic = self
            .intrinsics
            .ok_or_else(|| self.error(span, "ordinary runtime missing"))?;
        let frame = self.active_frame(span)?;
        let line = self.line(span);
        let (error, init) = self.bind_temp_bare(value::undefined(), span)?;
        statements.push(init);
        let entry = self.entry_for(error, span)?;
        let key_units: Vec<u16> = "message".encode_utf16().collect();
        let key = self.emit_utf16_node(&key_units, span, statements)?;
        let text_units: Vec<u16> = message.encode_utf16().collect();
        let text = self.emit_utf16_node(&text_units, span, statements)?;
        statements.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: error,
            expr: Self::guest_call(
                intrinsic.allocate,
                vec![
                    Expr::Var(carrier),
                    Expr::Int(layout::KIND_OBJECT),
                    Self::guest_call(
                        intrinsic.root,
                        vec![Expr::Var(carrier), Expr::Int(3)],
                    ),
                    Expr::Int(1),
                    Expr::Int(0),
                    Expr::Int(0),
                    Expr::Int(layout::OBJECT_LEGACY_ERROR_FLAG | layout::OBJECT_ERROR_INSTANCE_FLAG),
                    Expr::Var(frame),
                    Expr::Int(i64::from(entry)),
                ],
            ),
            line,
        });
        let base = self.slot(span)?;
        statements.push(Stmt::Let {
            index: base,
            declared_schema: Some(TypeSchema::Int),
            expr: Self::guest_call(
                checked_base,
                vec![
                    Expr::Var(carrier),
                    value::field(error, "ref_slot"),
                    value::field(error, "generation"),
                ],
            ),
            line,
        });
        // The instance brand guarantees the seven-cell Error header, so the
        // first own row sits directly at `base + ERROR_HEADER_CELLS`.
        let row = |offset: i64| {
            Expr::Add(
                Box::new(Expr::Var(base)),
                Box::new(Expr::Int(layout::ERROR_HEADER_CELLS + offset)),
            )
        };
        let flag = Expr::IfElse {
            condition: Box::new(value::field(text, "flag")),
            then_expr: Box::new(Expr::Int(1)),
            else_expr: Box::new(Expr::Int(0)),
        };
        for (offset, payload) in [
            (0, value::field(key, "ref_slot")),
            (1, value::field(key, "generation")),
            (
                2,
                super::prelude::object::pack_data(
                    value::field(text, "tag"),
                    Expr::Bool(true),
                    Expr::Bool(false),
                    Expr::Bool(true),
                ),
            ),
            (3, value::field(text, "number")),
            (4, flag),
            (5, value::field(text, "ref_slot")),
            (6, value::field(text, "generation")),
        ] {
            let ack = self.slot(span)?;
            statements.push(Stmt::Let {
                index: ack,
                declared_schema: Some(TypeSchema::Number),
                expr: Self::guest_call(
                    helper_write,
                    vec![
                        Expr::Var(carrier),
                        Self::as_number(row(offset)),
                        payload,
                    ],
                ),
                line,
            });
        }
        // Publish the row: the allocator created this instance with count 0,
        // headers flagged extensible|legacy|instance, so the number of own
        // rows has to move to one *after* the row cells are in place. Without
        // this header write the `message` row is invisible and the read falls
        // through to `Error.prototype.message` (the empty String).
        let ack = self.slot(span)?;
        statements.push(Stmt::Let {
            index: ack,
            declared_schema: Some(TypeSchema::Number),
            expr: Self::guest_call(
                helper_write,
                vec![
                    Expr::Var(carrier),
                    Self::as_number(Expr::Add(
                        Box::new(Expr::Var(base)),
                        Box::new(Expr::Int(2)),
                    )),
                    Expr::Int(
                        layout::OBJECT_COUNT_FACTOR
                            + layout::OBJECT_EXTENSIBLE_FLAG
                            + layout::OBJECT_LEGACY_ERROR_FLAG
                            + layout::OBJECT_ERROR_INSTANCE_FLAG,
                    ),
                ],
            ),
            line,
        });
        Ok(error)
    }

    /// A capability-only member read in discarded dead text. The caller has
    /// already run the same admission the live route uses; the value is
    /// never observed, so no Reference, helper, proof or realm demand is
    /// materialized for it.
    fn capability_only_read() -> Lowered {
        Self::capability_only_value(Kind::Dynamic)
    }

    /// A typed placeholder for discarded IR. Nothing here is executed and no
    /// helper/proof is materialized, so the value only has to agree with the
    /// role's established result kind; the live emitter still owns the real
    /// consumer.
    pub(super) fn capability_only_value(kind: Kind) -> Lowered {
        let expr = match kind {
            Kind::Number => value::number(Expr::Float(0.0)),
            Kind::Boolean => value::boolean(Expr::Bool(false)),
            _ => value::undefined(),
        };
        Lowered {
            expr,
            kind,
            array_length: None,
            owned_roots: Vec::new(),
        }
    }

    /// Dense-array member read on a static binding: `.length` or one computed
    /// index. A statically packed in-bounds literal keeps the exact old
    /// inline path; every other case dispatches on the real node kind.
    fn lower_array_member_read(
        &mut self,
        member: &ast::MemberExpr,
        name: &str,
    ) -> Result<Lowered, ParseError> {
        let shape = self.array_lengths.get(name).copied();
        let length_units: Vec<u16> = "length".encode_utf16().collect();
        let is_length = match &member.prop {
            ast::MemberProp::Ident(id) => id.sym == *"length",
            ast::MemberProp::Computed(property) => {
                matches!(
                    Self::unwrap_parens(&property.expr),
                    ast::Expr::Lit(ast::Lit::Str(_))
                ) && self.member_key_units(member)? == length_units
            }
            _ => false,
        };
        if is_length {
            return self.lower_array_length_read(member, name, shape);
        }
        // Any other property name is an ordinary Reference through the real
        // prototype chain: the array has no named own rows, so the inherited
        // prototype methods (and an explicitly replaced prototype) decide.
        let ast::MemberProp::Computed(property) = &member.prop else {
            return self.lower_object_member_read(member);
        };
        if let Some(index) = Self::member_index_literal(&property.expr) {
            if let Some(shape) = shape {
                let position = usize::try_from(index).unwrap_or(usize::MAX);
                if position >= shape.length {
                    return Ok(Lowered {
                        expr: value::undefined(),
                        kind: Kind::Undefined,
                        array_length: None,
                        owned_roots: Vec::new(),
                    });
                }
                if shape.repr == ArrayRepr::Packed {
                    return self.lower_packed_array_literal_read(member, name, index);
                }
            }
            return self.lower_array_runtime_read(member, name, Some(index));
        }
        if matches!(
            Self::unwrap_parens(&property.expr),
            ast::Expr::Lit(ast::Lit::Str(_))
        ) {
            if self.in_dead_capability() || self.emission_known_throw() {
                // A known-Array String subscript in discarded text is the same
                // primitive-key capability the live read prepares, but there
                // is no emitted Reference to consume a proof. The admission
                // below still rejects the same key domains as the live path.
                let admitted = self.probe_dead_capability(|| {
                    self.admit_primitive_member(
                        member,
                        Kind::Array,
                        Kind::String,
                        primitive_key::Role::Get,
                    )
                })?;
                if admitted {
                    return Ok(Self::capability_only_read());
                }
            }
            return self.lower_object_member_read(member);
        }
        self.lower_array_runtime_read(member, name, None)
    }

    /// Exact historical packed literal read: checked generation/kind/length
    /// gate then one Number cell. Kept byte-identical for pure numeric arrays.
    fn lower_packed_array_literal_read(
        &mut self,
        member: &ast::MemberExpr,
        name: &str,
        index: i64,
    ) -> Result<Lowered, ParseError> {
        let slot = self
            .bindings
            .get(name)
            .map(|(slot, _)| *slot)
            .ok_or_else(|| {
                self.error(
                    member.span,
                    "P0 indexed reads require a numeric-array binding",
                )
            })?;
        let carrier = self.active_carrier(member.span)?;
        let heap = self.heap_ref(member.span)?;
        let (helper_read, checked_base) = (heap.helper_read, heap.helpers.checked_base);
        let decode_length = heap.helpers.decode_u32;
        let decode = self.helper_decode.ok_or_else(|| {
            self.error(
                member.span,
                "P0 array reads require the realm decode helper",
            )
        })?;
        let line = self.line(member.span);
        // Preserve the source binding for subsequent accesses; each access
        // reads its identity fields once from a rooted copy.
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
                decode_length,
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
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(value::number(Self::as_number(read))),
            },
            kind: Kind::Number,
            array_length: None,
            owned_roots: vec![receiver],
        })
    }

    /// Runtime dense-array element read for tagged arrays, non-literal
    /// integer indexes and length-unknown joins. The helper checks the real
    /// node kind, generation and extent; out-of-range integer indexes yield
    /// Undefined rather than a fabricated cell.
    fn lower_array_runtime_read(
        &mut self,
        member: &ast::MemberExpr,
        name: &str,
        literal: Option<i64>,
    ) -> Result<Lowered, ParseError> {
        let slot = self
            .bindings
            .get(name)
            .map(|(slot, _)| *slot)
            .ok_or_else(|| {
                self.error(
                    member.span,
                    "P0 indexed reads require a numeric-array binding",
                )
            })?;
        // Dead text still runs the original static admission, but a proved
        // String subscript on a known Array is capability-only there: the
        // discarded IR emits no Reference and no proof may be published. The
        // same isolation applies to an operand after an unconditionally
        // throwing prefix, which is equally discarded.
        let mut deferred_key = None;
        if literal.is_none() && (self.in_dead_capability() || self.emission_known_throw()) {
            let ast::MemberProp::Computed(property) = &member.prop else {
                unreachable!("runtime array read is computed");
            };
            let key = self.expr(&property.expr)?;
            let admitted = self.probe_dead_capability(|| {
                self.admit_primitive_member(
                    member,
                    Kind::Array,
                    key.kind,
                    primitive_key::Role::Get,
                )
            })?;
            if key.kind == Kind::String && admitted {
                return Ok(Self::capability_only_read());
            }
            deferred_key = Some(key);
        }
        let carrier = self.active_carrier(member.span)?;
        let heap = self.heap_ref(member.span)?;
        let (helper_read, checked_base, array_get) = (
            heap.helper_read,
            heap.helpers.checked_base,
            heap.helpers.array_get,
        );
        let decode = self.helper_decode.ok_or_else(|| {
            self.error(
                member.span,
                "P0 array reads require the realm decode helper",
            )
        })?;
        let line = self.line(member.span);
        let (receiver, mut stmts) = self.bind_temp(Expr::Var(slot), member.obj.span())?;
        let mut owned = vec![receiver];
        let index = if let Some(index) = literal {
            Expr::Int(index)
        } else {
            let ast::MemberProp::Computed(property) = &member.prop else {
                unreachable!("runtime array read is computed");
            };
            let key = match deferred_key {
                Some(key) => key,
                None => self.expr(&property.expr)?,
            };
            if !matches!(key.kind, Kind::Number | Kind::Dynamic) {
                return Err(self.error(
                    property.span(),
                    "P0 dense array indexes must be numeric in this scope",
                ));
            }
            owned.extend(key.owned_roots);
            let (key_slot, mut access) =
                self.bind_evaluation(key.expr, key.kind, property.span())?;
            owned.push(key_slot);
            let idx_tmp = self.slot(member.span)?;
            access.push(Stmt::Let {
                index: idx_tmp,
                declared_schema: Some(TypeSchema::Int),
                expr: Self::guest_call(
                    decode,
                    vec![Self::as_number(self.array_index_number(key_slot, key.kind))],
                ),
                line,
            });
            stmts.extend(access);
            Expr::Var(idx_tmp)
        };
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
        let got = Self::guest_call(
            array_get,
            vec![Expr::Var(carrier), Expr::Var(base_tmp), index],
        );
        let (result, bound) = self.bind_temp(got, member.span)?;
        stmts.extend(bound);
        owned.push(result);
        // The decode helper only narrows proven numeric metadata; keep the
        // read helper's own tag/extent asserts as the fail-closed gate.
        let _ = helper_read;
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(Expr::Var(result)),
            },
            kind: Kind::Dynamic,
            array_length: None,
            owned_roots: owned,
        })
    }

    /// Dense-array `.length`. A proven shape yields the exact constant;
    /// an unknown/joined shape reads the live header length after the same
    /// checked generation/kind gate, so rebinding cannot fabricate a value.
    fn lower_array_length_read(
        &mut self,
        member: &ast::MemberExpr,
        name: &str,
        shape: Option<ArrayShape>,
    ) -> Result<Lowered, ParseError> {
        if let Some(shape) = shape {
            return Ok(Lowered {
                expr: value::number(Self::as_number(Expr::Int(shape.length as i64))),
                kind: Kind::Number,
                array_length: None,
                owned_roots: Vec::new(),
            });
        }
        let slot = self
            .bindings
            .get(name)
            .map(|(slot, _)| *slot)
            .ok_or_else(|| {
                self.error(
                    member.span,
                    "P0 indexed reads require a numeric-array binding",
                )
            })?;
        let carrier = self.active_carrier(member.span)?;
        let heap = self.heap_ref(member.span)?;
        let (checked_base, array_length) = (heap.helpers.checked_base, heap.helpers.array_length);
        let line = self.line(member.span);
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
        let read = Self::guest_call(array_length, vec![Expr::Var(carrier), Expr::Var(base_tmp)]);
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(value::number(Self::as_number(read))),
            },
            kind: Kind::Number,
            array_length: None,
            owned_roots: vec![receiver],
        })
    }

    /// Project one already-evaluated numeric index box to its Float number.
    /// A Dynamic-fact key is checked for the Number tag at runtime, so the
    /// bounded dense-index domain is enforced instead of misreading object
    /// payload bits.
    fn array_index_number(&self, slot: u16, kind: Kind) -> Expr {
        if kind == Kind::Dynamic {
            super::prelude::math::checked_number(Expr::Var(slot))
        } else {
            value::field(slot, "number")
        }
    }

    /// Dynamic receiver + proved Number key on a Get. At runtime an ordinary
    /// object keeps the established string-key Reference path; a packed or
    /// tagged array takes the element helper. Evaluation order (base, key,
    /// nullish check) matches the accepted primitive-key continuation.
    fn lower_dynamic_numeric_get(
        &mut self,
        member: &ast::MemberExpr,
    ) -> Result<Lowered, ParseError> {
        let span = member.span;
        self.require_primitive_site(span, primitive_key::Role::Get)?;
        let ast::MemberProp::Computed(property) = &member.prop else {
            unreachable!("sealed computed primitive key");
        };
        let carrier = self.active_carrier(span)?;
        let heap = self.heap_ref(span)?;
        let (helper_read, checked_base, array_get) = (
            heap.helper_read,
            heap.helpers.checked_base,
            heap.helpers.array_get,
        );
        let decode = self
            .helper_decode
            .ok_or_else(|| self.error(span, "array reads require the realm decode helper"))?;
        let ref_get = self
            .helper_ref_get
            .ok_or_else(|| self.error(span, "ordinary runtime missing"))?;
        let line = self.line(span);
        let base = self.expr(&member.obj)?;
        if base.kind != Kind::Dynamic {
            return Err(self.error(span, "sealed Dynamic receiver fact does not match emission"));
        }
        let mut owned = base.owned_roots;
        let (base_slot, mut stmts) = self.bind_temp(base.expr, member.obj.span())?;
        owned.push(base_slot);
        let key = self.expr(&property.expr)?;
        if key.kind != Kind::Number {
            return Err(self.error(
                property.span(),
                "sealed Number key fact does not match emission",
            ));
        }
        owned.extend(key.owned_roots);
        let (key_slot, mut access) = self.bind_evaluation(key.expr, key.kind, property.span())?;
        owned.push(key_slot);
        let (result, init) = self.bind_temp(value::undefined(), span)?;
        stmts.extend(init);
        owned.push(result);

        let kind_tmp = self.slot(span)?;
        let base_tmp = self.slot(span)?;
        let mut array_arm = Vec::new();
        array_arm.push(Stmt::Let {
            index: base_tmp,
            declared_schema: Some(TypeSchema::Int),
            expr: Self::guest_call(
                checked_base,
                vec![
                    Expr::Var(carrier),
                    value::field(base_slot, "ref_slot"),
                    value::field(base_slot, "generation"),
                ],
            ),
            line,
        });
        array_arm.push(Stmt::Let {
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
        let mut object_arm = Vec::new();
        // Ordinary object fallback: managed Number-to-string key plus the
        // established Reference Get. A fresh rooted temp holds the string.
        let (key_str, mut key_bound) = self.bind_temp(value::undefined(), span)?;
        object_arm.append(&mut key_bound);
        let frame = self.active_frame(span)?;
        let entry = self.entry_for(key_str, span)?;
        self.assign_number_key(
            carrier,
            frame,
            entry,
            key_slot,
            key_str,
            span,
            &mut object_arm,
        )?;
        self.refresh_root(key_str, span, &mut object_arm)?;
        let (reference, ref_bound) = self.bind_typed(
            value::reference(
                Expr::Var(base_slot),
                value::field(key_str, "ref_slot"),
                value::field(key_str, "generation"),
                Expr::Bool(self.enclosing_strict),
            ),
            value::reference_schema(),
            span,
        )?;
        object_arm.push(ref_bound);
        let got = self.consume_value_result(
            Self::guest_call(ref_get, vec![Expr::Var(carrier), Expr::Var(reference)]),
            span,
            &mut object_arm,
        )?;
        object_arm.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: result,
            expr: Expr::Var(got),
            line,
        });
        self.refresh_root(result, span, &mut object_arm)?;
        owned.push(key_str);
        owned.push(reference);

        // Packed/tagged arrays: index from the raw Number key before the
        // string-key conversion above ran (that conversion stays in the
        // ordinary-object arm only).
        let idx_tmp = self.slot(span)?;
        let mut array_get_stmts = Vec::new();
        array_get_stmts.push(Stmt::Let {
            index: idx_tmp,
            declared_schema: Some(TypeSchema::Int),
            expr: Self::guest_call(
                decode,
                vec![Self::as_number(value::field(key_slot, "number"))],
            ),
            line,
        });
        let got = Self::guest_call(
            array_get,
            vec![Expr::Var(carrier), Expr::Var(base_tmp), Expr::Var(idx_tmp)],
        );
        let (array_value, mut array_bound) = self.bind_temp(got, span)?;
        array_get_stmts.append(&mut array_bound);
        array_get_stmts.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: result,
            expr: Expr::Var(array_value),
            line,
        });
        self.refresh_root(result, span, &mut array_get_stmts)?;
        owned.push(array_value);
        let array_predicate = Expr::Or(
            Box::new(Expr::Eq(
                Box::new(Expr::Var(kind_tmp)),
                Box::new(Expr::Int(layout::KIND_ARRAY)),
            )),
            Box::new(Expr::Eq(
                Box::new(Expr::Var(kind_tmp)),
                Box::new(Expr::Int(layout::KIND_TAGGED_ARRAY)),
            )),
        );
        array_arm.push(Stmt::IfElse {
            condition: array_predicate,
            then_branch: array_get_stmts,
            else_branch: object_arm,
            line,
        });
        let mut throw = Vec::new();
        self.emit_proved_nullish_put_error(span, &mut throw)?;
        access.push(Stmt::IfElse {
            condition: Self::nullish_tag(base_slot),
            then_branch: throw,
            else_branch: vec![
                assert_stmt(
                    Expr::Eq(
                        Box::new(Self::as_number(value::field(base_slot, "tag"))),
                        Box::new(Self::as_number(Expr::Int(value::OBJECT))),
                    ),
                    line,
                ),
                Stmt::IfElse {
                    condition: Expr::Bool(true),
                    then_branch: array_arm,
                    else_branch: Vec::new(),
                    line,
                },
            ],
            line,
        });
        stmts.extend(access);
        reference::release_consumed_roots(self, &owned, result, span, &mut stmts)?;
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(Expr::Var(result)),
            },
            kind: Kind::Dynamic,
            array_length: None,
            owned_roots: vec![result],
        })
    }

    /// Statement/expression form of `base[key] = rhs` for a Dynamic receiver
    /// with a proved Number key. The ordinary-object arm keeps the managed
    /// string-key Reference Put; the array arm writes the real element.
    fn lower_dynamic_numeric_put(
        &mut self,
        member: &ast::MemberExpr,
        right: &ast::Expr,
        span: swc_common::Span,
    ) -> Result<Lowered, ParseError> {
        self.require_primitive_site(member.span, primitive_key::Role::Put)?;
        let ast::MemberProp::Computed(property) = &member.prop else {
            unreachable!("sealed computed primitive key");
        };
        let carrier = self.active_carrier(span)?;
        let (out, init) = self.bind_temp_bare(value::undefined(), span)?;
        let base = self.expr(&member.obj)?;
        if base.kind != Kind::Dynamic {
            return Err(self.error(span, "sealed Dynamic receiver fact does not match emission"));
        }
        let mut owned = base.owned_roots;
        let (base_slot, mut body) = self.bind_temp(base.expr, member.obj.span())?;
        owned.push(base_slot);
        body.push(init);
        let key = self.expr(&property.expr)?;
        if key.kind != Kind::Number {
            return Err(self.error(
                property.span(),
                "sealed Number key fact does not match emission",
            ));
        }
        owned.extend(key.owned_roots);
        let (key_slot, mut access) = self.bind_evaluation(key.expr, key.kind, property.span())?;
        owned.push(key_slot);
        let mut put = Vec::new();
        let rhs = self.expr(right)?;
        if self.emission_known_throw() {
            reference::release_consumed_roots(self, &owned, out, span, &mut body)?;
            return Ok(Lowered {
                expr: Expr::Block {
                    stmts: body,
                    expr: Box::new(Expr::Var(out)),
                },
                kind: Kind::Dynamic,
                array_length: None,
                owned_roots: Vec::new(),
            });
        }
        owned.extend(rhs.owned_roots);
        let (rhs_slot, mut rhs_bound) = self.bind_evaluation(rhs.expr, rhs.kind, right.span())?;
        put.append(&mut rhs_bound);
        self.emit_dynamic_numeric_store(carrier, base_slot, key_slot, rhs_slot, span, &mut put)?;
        put.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: out,
            expr: Expr::Var(rhs_slot),
            line: self.line(span),
        });
        if self.has_root(out) {
            self.refresh_root(out, span, &mut put)?;
        }
        if self.has_root(rhs_slot) {
            owned.push(rhs_slot);
        }
        let mut throw = Vec::new();
        self.emit_proved_nullish_put_error(span, &mut throw)?;
        access.push(Stmt::IfElse {
            condition: Self::nullish_tag(base_slot),
            then_branch: throw,
            else_branch: vec![Stmt::IfElse {
                condition: Expr::Bool(true),
                then_branch: put,
                else_branch: Vec::new(),
                line: self.line(span),
            }],
            line: self.line(span),
        });
        body.extend(access);
        reference::release_consumed_roots(self, &owned, out, span, &mut body)?;
        Ok(Lowered {
            expr: Expr::Block {
                stmts: body,
                expr: Box::new(Expr::Var(out)),
            },
            kind: Kind::Dynamic,
            array_length: None,
            owned_roots: Vec::new(),
        })
    }

    /// Shared runtime store for a Dynamic receiver + Number key. Evaluates
    /// the base tag/kind once and dispatches between the dense array writer
    /// and the established ordinary-object Reference Put.
    fn emit_dynamic_numeric_store(
        &mut self,
        carrier: u16,
        base_slot: u16,
        key_slot: u16,
        value_slot: u16,
        span: swc_common::Span,
        out: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let heap = self.heap_ref(span)?;
        let checked_base = heap.helpers.checked_base;
        let helper_read = heap.helper_read;
        let array_put = heap.helpers.array_put;
        let decode = self
            .helper_decode
            .ok_or_else(|| self.error(span, "array writes require the realm decode helper"))?;
        let ref_put = self
            .objects
            .ok_or_else(|| self.error(span, "ordinary runtime missing"))?
            .put;
        let line = self.line(span);
        let base_tmp = self.slot(span)?;
        out.push(assert_stmt(
            Expr::Eq(
                Box::new(Self::as_number(value::field(base_slot, "tag"))),
                Box::new(Self::as_number(Expr::Int(value::OBJECT))),
            ),
            line,
        ));
        out.push(Stmt::Let {
            index: base_tmp,
            declared_schema: Some(TypeSchema::Int),
            expr: Self::guest_call(
                checked_base,
                vec![
                    Expr::Var(carrier),
                    value::field(base_slot, "ref_slot"),
                    value::field(base_slot, "generation"),
                ],
            ),
            line,
        });
        let kind_tmp = self.slot(span)?;
        out.push(Stmt::Let {
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
        // Array arm: numeric element write, no string key materialization.
        let mut array_arm = Vec::new();
        let idx_tmp = self.slot(span)?;
        array_arm.push(Stmt::Let {
            index: idx_tmp,
            declared_schema: Some(TypeSchema::Int),
            expr: Self::guest_call(
                decode,
                vec![Self::as_number(value::field(key_slot, "number"))],
            ),
            line,
        });
        let ack = self.slot(span)?;
        array_arm.push(Stmt::Let {
            index: ack,
            declared_schema: None,
            expr: Self::guest_call(
                array_put,
                vec![
                    Expr::Var(carrier),
                    Expr::Var(base_tmp),
                    Expr::Var(idx_tmp),
                    Expr::Var(value_slot),
                    value::field(base_slot, "ref_slot"),
                    value::field(base_slot, "generation"),
                ],
            ),
            line,
        });
        // Ordinary object arm: managed Number-to-string key, Reference Put.
        let mut object_arm = Vec::new();
        let (key_str, mut key_bound) = self.bind_temp(value::undefined(), span)?;
        object_arm.append(&mut key_bound);
        let frame = self.active_frame(span)?;
        let entry = self.entry_for(key_str, span)?;
        self.assign_number_key(
            carrier,
            frame,
            entry,
            key_slot,
            key_str,
            span,
            &mut object_arm,
        )?;
        self.refresh_root(key_str, span, &mut object_arm)?;
        let (reference, ref_bound) = self.bind_typed(
            value::reference(
                Expr::Var(base_slot),
                value::field(key_str, "ref_slot"),
                value::field(key_str, "generation"),
                Expr::Bool(self.enclosing_strict),
            ),
            value::reference_schema(),
            span,
        )?;
        object_arm.push(ref_bound);
        self.consume_result(
            Self::guest_call(
                ref_put,
                vec![
                    Expr::Var(carrier),
                    Expr::Var(reference),
                    Expr::Var(value_slot),
                ],
            ),
            TypeSchema::Null,
            Expr::Null,
            |_| value::undefined(),
            span,
            &mut object_arm,
        )?;
        out.push(Stmt::IfElse {
            condition: Expr::Or(
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(kind_tmp)),
                    Box::new(Expr::Int(layout::KIND_ARRAY)),
                )),
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(kind_tmp)),
                    Box::new(Expr::Int(layout::KIND_TAGGED_ARRAY)),
                )),
            ),
            then_branch: array_arm,
            else_branch: object_arm,
            line,
        });
        Ok(())
    }

    /// Store one already-evaluated value through a Dynamic receiver and a
    /// proved Number key. Used by for-of member targets, whose current item
    /// is rooted by the iteration owner across this store.
    fn lower_dynamic_numeric_put_slot(
        &mut self,
        member: &ast::MemberExpr,
        value_slot: u16,
        span: swc_common::Span,
        statements: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        self.require_primitive_site(member.span, primitive_key::Role::Put)?;
        let ast::MemberProp::Computed(property) = &member.prop else {
            unreachable!("sealed computed primitive key");
        };
        let carrier = self.active_carrier(span)?;
        let base = self.expr(&member.obj)?;
        if base.kind != Kind::Dynamic {
            return Err(self.error(span, "sealed Dynamic receiver fact does not match emission"));
        }
        let mut owned = base.owned_roots;
        let (base_slot, mut body) = self.bind_temp(base.expr, member.obj.span())?;
        owned.push(base_slot);
        let key = self.expr(&property.expr)?;
        if key.kind != Kind::Number {
            return Err(self.error(
                property.span(),
                "sealed Number key fact does not match emission",
            ));
        }
        owned.extend(key.owned_roots);
        let (key_slot, mut access) = self.bind_evaluation(key.expr, key.kind, property.span())?;
        owned.push(key_slot);
        let mut store = Vec::new();
        self.emit_dynamic_numeric_store(
            carrier, base_slot, key_slot, value_slot, span, &mut store,
        )?;
        let mut throw = Vec::new();
        self.emit_proved_nullish_put_error(span, &mut throw)?;
        let line = self.line(span);
        access.push(Stmt::IfElse {
            condition: Self::nullish_tag(base_slot),
            then_branch: throw,
            else_branch: vec![Stmt::IfElse {
                condition: Expr::Bool(true),
                then_branch: store,
                else_branch: Vec::new(),
                line,
            }],
            line,
        });
        body.extend(access);
        reference::release_consumed_roots(self, &owned, value_slot, span, &mut body)?;
        statements.extend(body);
        Ok(())
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

    pub(super) fn consume_value_result(
        &mut self,
        expression: Expr,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
    ) -> Result<u16, ParseError> {
        self.consume_result(
            expression,
            value::schema(),
            value::undefined(),
            Expr::Var,
            span,
            stmts,
        )
    }
    pub(super) fn consume_result(
        &mut self,
        expression: Expr,
        ok_schema: TypeSchema,
        ok_initial: Expr,
        boxed: impl FnOnce(u16) -> Expr,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
    ) -> Result<u16, ParseError> {
        let line = self.line(span);
        let schema = if ok_schema == value::schema() {
            value::value_result_schema()
        } else {
            super::prelude::object::result_schema(ok_schema.clone())
        };
        let (raw, saved) = self.bind_typed(expression, schema.clone(), span)?;
        stmts.push(saved);
        let (ok, saved) = self.bind_typed(ok_initial, ok_schema, span)?;
        stmts.push(saved);
        let (error, saved) = self.bind_temp_bare(value::undefined(), span)?;
        stmts.push(saved);
        let (out, saved) = self.bind_temp_bare(value::undefined(), span)?;
        stmts.push(saved);
        let mut success = vec![Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: out,
            expr: boxed(ok),
            line,
        }];
        self.refresh_root(out, span, &mut success)?;
        let mut failure = Vec::new();
        self.refresh_root(error, span, &mut failure)?;
        self.publish_callee_throw(error, span, &mut failure)?;
        self.clear_root(error, span, &mut failure)?;
        stmts.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: out,
            expr: Expr::ResultMatch {
                value: Box::new(Expr::Var(raw)),
                value_slot: raw,
                ok_binding: Some(ok),
                err_binding: Some(error),
                ok_expr: Box::new(Expr::Block {
                    stmts: success,
                    expr: Box::new(Expr::Var(out)),
                }),
                err_expr: Box::new(Expr::Block {
                    stmts: failure,
                    expr: Box::new(value::undefined()),
                }),
                scrutinee_schema: schema,
                span: self.ir_span(span),
                semantic_id: None,
            },
            line,
        });
        Ok(out)
    }

    pub(super) fn lower_in(&mut self, binary: &ast::BinExpr) -> Result<Lowered, ParseError> {
        if self.primitive_key_role(binary.span, primitive_key::Role::In) {
            return self.lower_primitive_in(binary);
        }
        // Discarded `in`: the prepared HasProperty continuation is suppressed
        // with the dead walk, but key/right admission still runs before the
        // site becomes capability-only. Proved String/Undefined keys keep
        // their legacy route untouched.
        if self.in_dead_capability() || self.emission_known_throw() {
            let left = self.expr(&binary.left)?;
            let right = self.expr(&binary.right)?;
            if self.discard_in_capability(binary, left.kind, right.kind)? {
                return Ok(Self::capability_only_value(Kind::Boolean));
            }
        }
        let left = self.expr(&binary.left)?;
        let right = self.expr(&binary.right)?;
        if !matches!(left.kind, Kind::String | Kind::Undefined) {
            return Err(self.error(
                binary.left.span(),
                "P7 in keys require a proved String or undefined",
            ));
        }
        let mut owned = left.owned_roots;
        owned.extend(right.owned_roots);
        let (key, mut body) = self.bind_temp(left.expr, binary.left.span())?;
        owned.push(key);
        let (out, saved) = self.bind_temp_bare(value::undefined(), binary.span)?;
        body.push(saved);
        let mut step = Vec::new();
        let (object, saved) = self.bind_temp(right.expr, binary.right.span())?;
        step.extend(saved);
        owned.push(object);
        let carrier = self.active_carrier(binary.span)?;
        let helpers = self
            .objects
            .ok_or_else(|| self.error(binary.span, "ordinary runtime missing"))?;
        let intrinsic = self.intrinsics.expect("intrinsics");
        let span = self.ir_span(binary.span);
        let mut success = Vec::new();
        let key = if left.kind == Kind::Undefined {
            self.emit_utf16_node(
                &"undefined".encode_utf16().collect::<Vec<_>>(),
                binary.span,
                &mut success,
            )?
        } else {
            key
        };
        let got = Self::guest_call(
            helpers.has,
            vec![Expr::Var(carrier), Expr::Var(object), Expr::Var(key)],
        );
        let schema = value::value_result_schema();
        let result = Expr::IfElse {
            condition: Box::new(Expr::Eq(
                Box::new(Self::as_number(value::field(object, "tag"))),
                Box::new(Self::as_number(Expr::Int(value::OBJECT))),
            )),
            then_expr: Box::new(Expr::Block {
                stmts: success,
                expr: Box::new(super::prelude::object::result(
                    schema.clone(),
                    true,
                    value::boolean(got),
                    span,
                )),
            }),
            else_expr: Box::new(super::prelude::object::result(
                schema,
                false,
                Self::guest_call(intrinsic.type_error, vec![Expr::Var(carrier)]),
                span,
            )),
        };
        let value = self.consume_value_result(result, binary.span, &mut step)?;
        step.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: out,
            expr: Expr::Var(value),
            line: self.line(binary.span),
        });
        self.refresh_root(out, binary.span, &mut step)?;
        if let Some(normal) = self.pending_normal {
            body.push(Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: step,
                else_branch: Vec::new(),
                line: self.line(binary.span),
            });
        } else {
            body.extend(step);
        }
        reference::release_consumed_roots(self, &owned, out, binary.span, &mut body)?;
        Ok(Lowered {
            expr: Expr::Block {
                stmts: body,
                expr: Box::new(Expr::Var(out)),
            },
            kind: Kind::Boolean,
            array_length: None,
            owned_roots: vec![out],
        })
    }

    pub(super) fn lower_property_assignment(
        &mut self,
        member: &ast::MemberExpr,
        right: &ast::Expr,
        span: swc_common::Span,
    ) -> Result<Lowered, ParseError> {
        if super::reference::is_arguments_base(member)
            && self.function_args.is_some()
            && super::reference::implicit_arguments(self)
        {
            return self.lower_arguments_store(member, right, span);
        }
        if self.is_array_length_assign(member) {
            let mut stmts = Vec::new();
            let slot = self.lower_array_length_assign(member, right, span, &mut stmts)?;
            return Ok(Lowered {
                expr: Expr::Block {
                    stmts,
                    expr: Box::new(Expr::Var(slot)),
                },
                kind: Kind::Dynamic,
                array_length: None,
                owned_roots: Vec::new(),
            });
        }
        if self.primitive_key_put_mode(member).is_some() {
            if self.primitive_key_dynamic_number(member, primitive_key::Role::Put) {
                return self.lower_dynamic_numeric_put(member, right, span);
            }
            return self.lower_primitive_number_put(member, right, span);
        }
        let left = self.expr(&member.obj)?;
        reference::admit_object_receiver(self, member.span, left.kind)?;
        // Discarded write: the prepared Reference is suppressed with the dead
        // walk, but receiver/key/RHS admission still runs in source order
        // before the site becomes capability-only.
        if (self.in_dead_capability() || self.emission_known_throw())
            && let Some(key_expr) = Self::open_computed_key(member)
        {
            let key = self.expr(key_expr)?;
            if self.discard_member_capability(
                member,
                left.kind,
                key.kind,
                primitive_key::Role::Put,
            )? {
                let _rhs = self.expr(right)?;
                return Ok(Self::capability_only_value(Kind::Object));
            }
        }
        let mut owned = left.owned_roots;
        let (base, mut body) = self.bind_temp(left.expr, member.obj.span())?;
        owned.push(base);
        let (out, saved) = self.bind_temp_bare(value::undefined(), span)?;
        body.push(saved);
        let mut step = Vec::new();
        let units = self.member_key_units(member)?;
        if matches!(left.kind, Kind::Null | Kind::Undefined) {
            self.emit_proved_nullish_put_error(span, &mut body)?;
            reference::release_consumed_roots(self, &owned, out, span, &mut body)?;
            return Ok(Lowered {
                expr: Expr::Block {
                    stmts: body,
                    expr: Box::new(Expr::Var(out)),
                },
                kind: Kind::Dynamic,
                array_length: None,
                owned_roots: vec![out],
            });
        }
        let key = self.emit_utf16_node(&units, member.span, &mut step)?;
        owned.push(key);
        let rhs = self.expr(right)?;
        owned.extend(rhs.owned_roots);
        let (rhs, saved) = self.bind_temp(rhs.expr, right.span())?;
        step.extend(saved);
        owned.push(rhs);
        let carrier = self.active_carrier(span)?;
        let helper = self
            .objects
            .ok_or_else(|| self.error(span, "ordinary runtime missing"))?
            .put;
        let reference = value::reference(
            Expr::Var(base),
            value::field(key, "ref_slot"),
            value::field(key, "generation"),
            Expr::Bool(self.enclosing_strict),
        );
        let mut put = Vec::new();
        self.consume_result(
            Self::guest_call(helper, vec![Expr::Var(carrier), reference, Expr::Var(rhs)]),
            TypeSchema::Null,
            Expr::Null,
            |_| value::undefined(),
            span,
            &mut put,
        )?;
        let mut publish = vec![Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: out,
            expr: Expr::Var(rhs),
            line: self.line(span),
        }];
        self.refresh_root(out, span, &mut publish)?;
        if left.kind == Kind::Dynamic {
            let mut throw = Vec::new();
            self.emit_proved_nullish_put_error(span, &mut throw)?;
            body.push(Stmt::IfElse {
                condition: Expr::Or(
                    Box::new(Expr::Eq(
                        Box::new(Self::as_number(value::field(base, "tag"))),
                        Box::new(Self::as_number(Expr::Int(value::NULL))),
                    )),
                    Box::new(Expr::Eq(
                        Box::new(Self::as_number(value::field(base, "tag"))),
                        Box::new(Self::as_number(Expr::Int(value::UNDEFINED))),
                    )),
                ),
                then_branch: throw,
                else_branch: vec![assert_stmt(
                    Expr::Eq(
                        Box::new(Self::as_number(value::field(base, "tag"))),
                        Box::new(Self::as_number(Expr::Int(value::OBJECT))),
                    ),
                    self.line(span),
                )],
                line: self.line(span),
            });
        }
        if let Some(normal) = self.pending_normal {
            put.push(Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: publish,
                else_branch: Vec::new(),
                line: self.line(span),
            });
            step.push(Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: put,
                else_branch: Vec::new(),
                line: self.line(span),
            });
            body.push(Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: step,
                else_branch: Vec::new(),
                line: self.line(span),
            });
        } else {
            step.extend(put);
            step.extend(publish);
            body.extend(step);
        }
        reference::release_consumed_roots(self, &owned, out, span, &mut body)?;
        Ok(Lowered {
            expr: Expr::Block {
                stmts: body,
                expr: Box::new(Expr::Var(out)),
            },
            kind: Kind::Object,
            array_length: None,
            owned_roots: vec![out],
        })
    }

    /// Put a precomputed JsValue through the existing member assignment path.
    /// Base and key are evaluated here; the value was obtained by the caller.
    pub(super) fn put_member_slot(
        &mut self,
        member: &ast::MemberExpr,
        value_slot: u16,
        span: swc_common::Span,
        statements: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        if self.primitive_key_put_mode(member).is_some() {
            if self.primitive_key_dynamic_number(member, primitive_key::Role::Put) {
                return self.lower_dynamic_numeric_put_slot(member, value_slot, span, statements);
            }
            return self.put_prepared_slot(member, value_slot, span, statements);
        }
        let kind = match member.obj.as_ref() {
            ast::Expr::Ident(object) => self
                .bindings
                .get(object.sym.as_str())
                .map(|(_, kind)| *kind)
                .unwrap_or(Kind::Undefined),
            ast::Expr::This(_) => Kind::Object,
            _ => Kind::Dynamic,
        };
        if kind == Kind::Array {
            return self.put_array_index_slot(member, value_slot, span, statements);
        }
        let left = self.expr(&member.obj)?;
        reference::admit_object_receiver(self, member.span, left.kind)?;
        // The same discarded-write boundary for a precomputed JsValue: the
        // site keeps its admitted receiver/key capability and materializes no
        // Reference while the whole statement is unreachable.
        if (self.in_dead_capability() || self.emission_known_throw())
            && let Some(key_expr) = Self::open_computed_key(member)
        {
            let key = self.expr(key_expr)?;
            if self.discard_member_capability(
                member,
                left.kind,
                key.kind,
                primitive_key::Role::Put,
            )? {
                return Ok(());
            }
        }
        let mut owned = left.owned_roots;
        let (base, mut body) = self.bind_temp(left.expr, member.obj.span())?;
        owned.push(base);
        if matches!(left.kind, Kind::Null | Kind::Undefined) || self.emission_known_throw() {
            if matches!(left.kind, Kind::Null | Kind::Undefined) {
                self.emit_proved_nullish_put_error(span, &mut body)?;
            }
            reference::release_consumed_roots(self, &owned, value_slot, span, &mut body)?;
            statements.extend(body);
            return Ok(());
        }
        let mut step = Vec::new();
        let units = self.member_key_units(member)?;
        let key = self.emit_utf16_node(&units, member.span, &mut step)?;
        owned.push(key);
        let carrier = self.active_carrier(span)?;
        let helper = self
            .objects
            .ok_or_else(|| self.error(span, "ordinary runtime missing"))?
            .put;
        let reference = value::reference(
            Expr::Var(base),
            value::field(key, "ref_slot"),
            value::field(key, "generation"),
            Expr::Bool(self.enclosing_strict),
        );
        let mut put = Vec::new();
        self.consume_result(
            Self::guest_call(
                helper,
                vec![Expr::Var(carrier), reference, Expr::Var(value_slot)],
            ),
            TypeSchema::Null,
            Expr::Null,
            |_| value::undefined(),
            span,
            &mut put,
        )?;
        if left.kind == Kind::Dynamic {
            let mut throw = Vec::new();
            self.emit_proved_nullish_put_error(span, &mut throw)?;
            body.push(Stmt::IfElse {
                condition: Expr::Or(
                    Box::new(Expr::Eq(
                        Box::new(Self::as_number(value::field(base, "tag"))),
                        Box::new(Self::as_number(Expr::Int(value::NULL))),
                    )),
                    Box::new(Expr::Eq(
                        Box::new(Self::as_number(value::field(base, "tag"))),
                        Box::new(Self::as_number(Expr::Int(value::UNDEFINED))),
                    )),
                ),
                then_branch: throw,
                else_branch: vec![assert_stmt(
                    Expr::Eq(
                        Box::new(Self::as_number(value::field(base, "tag"))),
                        Box::new(Self::as_number(Expr::Int(value::OBJECT))),
                    ),
                    self.line(span),
                )],
                line: self.line(span),
            });
        }
        if let Some(normal) = self.pending_normal {
            step.push(Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: put,
                else_branch: Vec::new(),
                line: self.line(span),
            });
            body.push(Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: step,
                else_branch: Vec::new(),
                line: self.line(span),
            });
        } else {
            step.extend(put);
            body.extend(step);
        }
        reference::release_consumed_roots(self, &owned, value_slot, span, &mut body)?;
        statements.extend(body);
        Ok(())
    }

    fn put_array_index_slot(
        &mut self,
        member: &ast::MemberExpr,
        value_slot: u16,
        span: swc_common::Span,
        statements: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        self.lower_array_store_slot(member, value_slot, span, statements)
    }

    /// Store one already-evaluated JsValue into a dense-array binding. Used
    /// by for-of member targets, where the item must survive LHS base/key
    /// evaluation and the eventual store. A non-literal key is evaluated
    /// here, once per execution of this store.
    fn lower_array_store_slot(
        &mut self,
        member: &ast::MemberExpr,
        value_slot: u16,
        span: swc_common::Span,
        statements: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let ast::MemberProp::Computed(property) = &member.prop else {
            return Err(self.error(member.span, "P0 dense arrays admit only integer subscripts"));
        };
        let ast::Expr::Ident(object) = member.obj.as_ref() else {
            return Err(self.error(
                member.span,
                "P0 indexed writes require a numeric-array binding",
            ));
        };
        let name = object.sym.as_str();
        let receiver_slot = self
            .bindings
            .get(name)
            .map(|(slot, _)| *slot)
            .ok_or_else(|| {
                self.error(
                    object.span,
                    "P0 indexed writes require a numeric-array binding",
                )
            })?;
        let shape = self.array_lengths.get(name).copied();
        let literal = Self::member_index_literal(&property.expr);
        let (receiver, mut bound) = self.bind_temp(Expr::Var(receiver_slot), member.obj.span())?;
        statements.append(&mut bound);
        let mut key_slot = None;
        if literal.is_none() {
            let key = self.expr(&property.expr)?;
            if !matches!(key.kind, Kind::Number | Kind::Dynamic) {
                return Err(self.error(
                    property.span(),
                    "P0 dense array indexes must be numeric in this scope",
                ));
            }
            let (slot, mut key_bound) =
                self.bind_evaluation(key.expr, key.kind, property.span())?;
            statements.append(&mut key_bound);
            key_slot = Some((slot, key.kind));
        }
        let line = self.line(span);
        let mut commit = Vec::new();
        self.emit_array_store(
            member,
            receiver,
            shape.map(|shape| shape.repr),
            literal,
            key_slot,
            value_slot,
            span,
            &mut commit,
        )?;
        if let Some(normal) = self.pending_normal {
            statements.push(Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: commit,
                else_branch: Vec::new(),
                line,
            });
        } else {
            statements.extend(commit);
        }
        reference::release_consumed_roots(self, &[receiver], value_slot, span, statements)?;
        Ok(())
    }

    /// Statement form `a[i] = rhs`: the receiver, then the key (before the
    /// RHS, as JavaScript requires), then the RHS, then the store. Packed
    /// arrays keep the old static Number-only refusal.
    fn is_array_length_assign(&self, member: &ast::MemberExpr) -> bool {
        let ast::MemberProp::Ident(prop) = &member.prop else {
            return false;
        };
        if prop.sym.as_str() != "length" {
            return false;
        }
        match member.obj.as_ref() {
            ast::Expr::Ident(object) => {
                let name = object.sym.as_str();
                self.bindings
                    .get(name)
                    .is_some_and(|(_, kind)| *kind == Kind::Array)
                    || self.array_names.contains(name)
            }
            ast::Expr::Call(_) => true,
            _ => false,
        }
    }

    /// Instance `length` write. Micro converts with ToInt32 and throws
    /// TypeError outside `0 .. 2^30`. A positive fraction truncates; it does
    /// not throw. Growth does not allocate. Indexes in `[present, length)`
    /// read as undefined and are own; they are not stored cells.
    fn lower_array_length_assign(
        &mut self,
        member: &ast::MemberExpr,
        right: &ast::Expr,
        span: swc_common::Span,
        statements: &mut Vec<Stmt>,
    ) -> Result<u16, ParseError> {
        let receiver_expr = match member.obj.as_ref() {
            ast::Expr::Ident(object) => {
                // A length write can be observed through any alias. Drop every
                // cached array shape so later reads use the cells.
                self.array_lengths.clear();
                self.array_length_ids.clear();
                let name = object.sym.as_str();
                let slot = self
                    .bindings
                    .get(name)
                    .map(|(slot, _)| *slot)
                    .ok_or_else(|| {
                        self.error(object.span, "P0 array length writes require an array binding")
                    })?;
                Expr::Var(slot)
            }
            ast::Expr::Call(_) => {
                self.array_lengths.clear();
                self.array_length_ids.clear();
                self.expr(member.obj.as_ref())?.expr
            }
            _ => {
                return Err(self.error(
                    member.span,
                    "P0 array length writes require an array binding or call",
                ))
            }
        };
        let (receiver, mut receiver_bound) = self.bind_temp(receiver_expr, member.obj.span())?;
        statements.append(&mut receiver_bound);
        let rhs = self.expr(right)?;
        let mut roots = rhs.owned_roots;
        let (rhs_slot, mut rhs_bound) = self.bind_evaluation(rhs.expr, rhs.kind, right.span())?;
        statements.append(&mut rhs_bound);
        let ordinary_object = matches!(Self::unwrap_parens(member.obj.as_ref()), ast::Expr::Call(_));
        self.emit_array_length_store(
            receiver,
            rhs_slot,
            ordinary_object,
            span,
            statements,
        )?;
        roots.push(receiver);
        reference::release_consumed_roots(self, &roots, rhs_slot, span, statements)?;
        Ok(rhs_slot)
    }

    /// Number-hint DefaultValue for an object length RHS. The existing
    /// ToNumber/ToInt32 path then reads the primitive. A missing converter,
    /// or no pending owner, keeps the historical slot so a converter Err
    /// cannot become a host assert. `proceed` is false only when this call
    /// published the throw.
    fn prepare_length_subject(
        &mut self,
        rhs_slot: u16,
        span: swc_common::Span,
        statements: &mut Vec<Stmt>,
    ) -> Result<(u16, Option<u16>), ParseError> {
        let Some(objects) = self.objects else {
            return Ok((rhs_slot, None));
        };
        let Some(normal) = self.pending_normal else {
            return Ok((rhs_slot, None));
        };
        let line = self.line(span);
        let carrier = self.active_carrier(span)?;
        let proceed = self.slot(span)?;
        statements.push(Stmt::Let {
            index: proceed,
            declared_schema: Some(TypeSchema::Bool),
            expr: Expr::Bool(true),
            line,
        });
        let (subject, mut subject_init) = self.bind_temp(Expr::Var(rhs_slot), span)?;
        statements.append(&mut subject_init);
        let mut convert = Vec::new();
        let primitive = self.consume_value_result(
            Self::guest_call(
                objects.numeric_default_value,
                vec![
                    Expr::Var(carrier),
                    Expr::Var(rhs_slot),
                    Expr::Bool(false),
                ],
            ),
            span,
            &mut convert,
        )?;
        convert.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: subject,
            expr: Expr::Var(primitive),
            line,
        });
        self.refresh_root(subject, span, &mut convert)?;
        statements.push(Stmt::IfElse {
            condition: Expr::Eq(
                Box::new(Self::as_number(value::field(rhs_slot, "tag"))),
                Box::new(Self::as_number(Expr::Int(value::OBJECT))),
            ),
            then_branch: convert,
            else_branch: Vec::new(),
            line,
        });
        // Store only while this completion is still normal. A throw that
        // arrived before conversion, or one this conversion publishes, skips
        // the store. Do not treat a prior throw as permission to write.
        statements.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: proceed,
            expr: Expr::Var(normal),
            line,
        });
        Ok((subject, Some(proceed)))
    }

    /// Ordinary `length` write for a call receiver that is not an array.
    /// Reuses `objects.put` and the original RHS slot. No ToNumber.
    fn emit_ordinary_length_put(
        &mut self,
        receiver: u16,
        rhs_slot: u16,
        span: swc_common::Span,
        statements: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let put = self
            .objects
            .ok_or_else(|| self.error(span, "ordinary length write requires the object runtime"))?
            .put;
        let carrier = self.active_carrier(span)?;
        let units: Vec<u16> = "length".encode_utf16().collect();
        let key = self.emit_utf16_node(&units, span, statements)?;
        let reference = value::reference(
            Expr::Var(receiver),
            value::field(key, "ref_slot"),
            value::field(key, "generation"),
            Expr::Bool(self.enclosing_strict),
        );
        self.consume_result(
            Self::guest_call(
                put,
                vec![Expr::Var(carrier), reference, Expr::Var(rhs_slot)],
            ),
            TypeSchema::Null,
            Expr::Null,
            |_| value::undefined(),
            span,
            statements,
        )?;
        reference::release_consumed_roots(self, &[key], rhs_slot, span, statements)?;
        Ok(())
    }

    fn emit_array_length_store(
        &mut self,
        receiver: u16,
        rhs_slot: u16,
        ordinary_object: bool,
        span: swc_common::Span,
        statements: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let carrier = self.active_carrier(span)?;
        let heap = self.heap_ref(span)?;
        let checked_base = heap.helpers.checked_base;
        let setter = heap.helpers.array_set_length;
        let helper_read = heap.helper_read;
        let decode = self.helper_decode.ok_or_else(|| {
            self.error(span, "array length writes require the realm decode helper")
        })?;
        let line = self.line(span);
        let mut after_array = Vec::new();
        let (subject, proceed) = self.prepare_length_subject(rhs_slot, span, &mut after_array)?;
        let mut body = Vec::new();
        let number = self.slot(span)?;
        let trunc = self.slot(span)?;
        let len_float = self.slot(span)?;
        let len_int = self.slot(span)?;
        let base = self.slot(span)?;
        let ack = self.slot(span)?;
        let kind = self.slot(span)?;
        let tag_of = Self::as_number(value::field(subject, "tag"));
        let is_tag = |code| {
            Expr::Eq(
                Box::new(tag_of.clone()),
                Box::new(Self::as_number(Expr::Int(code))),
            )
        };
        let parser = self.published_number_parser();
        let string_number = match parser {
            Some(parser) => Self::guest_call(
                parser,
                vec![
                    Expr::Var(carrier),
                    Expr::Var(subject),
                    Expr::Int(0),
                    Expr::Float(0.0),
                ],
            ),
            None => Expr::Float(0.0),
        };
        let boolean_number = Expr::IfElse {
            condition: Box::new(value::field(subject, "flag")),
            then_expr: Box::new(Expr::Float(1.0)),
            else_expr: Box::new(Expr::Float(0.0)),
        };
        body.push(Stmt::Let {
            index: number,
            declared_schema: Some(TypeSchema::Float),
            expr: Expr::IfElse {
                condition: Box::new(is_tag(value::NUMBER)),
                then_expr: Box::new(Self::as_number(value::field(subject, "number"))),
                else_expr: Box::new(Expr::IfElse {
                    condition: Box::new(is_tag(value::NULL)),
                    then_expr: Box::new(Expr::Float(0.0)),
                    else_expr: Box::new(Expr::IfElse {
                        condition: Box::new(is_tag(value::BOOLEAN)),
                        then_expr: Box::new(boolean_number),
                        else_expr: Box::new(Expr::IfElse {
                            condition: Box::new(is_tag(value::STRING)),
                            then_expr: Box::new(string_number),
                            else_expr: Box::new(Expr::IfElse {
                                condition: Box::new(is_tag(value::UNDEFINED)),
                                then_expr: Box::new(Expr::Float(f64::NAN)),
                                else_expr: Box::new(Expr::Float(0.0)),
                            }),
                        }),
                    }),
                }),
            },
            line,
        });
        body.push(Stmt::Let {
            index: trunc,
            declared_schema: Some(TypeSchema::Float),
            expr: Self::builtin_call(
                BuiltinFunction::MathTrunc.call_index(),
                vec![Expr::Var(number)],
            ),
            line,
        });
        // ES5 ToUint32, then signed ToInt32. 2^32 wraps to 0 before the
        // Micro length bound; values still outside 0..2^30-1 stay TypeError.
        let modulus = Expr::Float(4_294_967_296.0);
        let wrapped = Expr::Sub(
            Box::new(Expr::Var(trunc)),
            Box::new(Expr::Mul(
                Box::new(Self::builtin_call(
                    BuiltinFunction::MathFloor.call_index(),
                    vec![Expr::Div(
                        Box::new(Expr::Var(trunc)),
                        Box::new(modulus.clone()),
                    )],
                )),
                Box::new(modulus),
            )),
        );
        let uint32 = Expr::IfElse {
            condition: Box::new(Self::builtin_call(
                BuiltinFunction::MathIsFinite.call_index(),
                vec![Expr::Var(trunc)],
            )),
            then_expr: Box::new(wrapped),
            else_expr: Box::new(Expr::Float(0.0)),
        };
        body.push(Stmt::Let {
            index: len_float,
            declared_schema: Some(TypeSchema::Float),
            expr: Expr::IfElse {
                condition: Box::new(Expr::Not(Box::new(Expr::Lt(
                    Box::new(uint32.clone()),
                    Box::new(Expr::Float(2_147_483_648.0)),
                )))),
                then_expr: Box::new(Expr::Sub(
                    Box::new(uint32.clone()),
                    Box::new(Expr::Float(4_294_967_296.0)),
                )),
                else_expr: Box::new(uint32),
            },
            line,
        });
        let in_range = Expr::And(
            Box::new(Expr::Not(Box::new(Expr::Lt(
                Box::new(Expr::Var(len_float)),
                Box::new(Expr::Float(0.0)),
            )))),
            Box::new(Expr::Not(Box::new(Expr::Gt(
                Box::new(Expr::Var(len_float)),
                Box::new(Expr::Float(1_073_741_823.0)),
            )))),
        );
        let tag_ok = Expr::Or(
            Box::new(Expr::Or(
                Box::new(Expr::Or(
                    Box::new(is_tag(value::NUMBER)),
                    Box::new(is_tag(value::NULL)),
                )),
                Box::new(Expr::Or(
                    Box::new(is_tag(value::BOOLEAN)),
                    Box::new(is_tag(value::UNDEFINED)),
                )),
            )),
            Box::new(if parser.is_some() {
                is_tag(value::STRING)
            } else {
                Expr::Bool(false)
            }),
        );
        let mut reject = Vec::new();
        self.raise_managed_type_error(span, &mut reject)?;
        let mut reject_range = Vec::new();
        self.raise_managed_type_error(span, &mut reject_range)?;
        let mut not_array = Vec::new();
        self.raise_managed_type_error(span, &mut not_array)?;
        let receiver_object = Expr::Eq(
            Box::new(Self::as_number(value::field(receiver, "tag"))),
            Box::new(Self::as_number(Expr::Int(value::OBJECT))),
        );
        let is_array = Expr::Or(
            Box::new(Expr::Eq(
                Box::new(Expr::Var(kind)),
                Box::new(Expr::Int(layout::KIND_ARRAY)),
            )),
            Box::new(Expr::Eq(
                Box::new(Expr::Var(kind)),
                Box::new(Expr::Int(layout::KIND_TAGGED_ARRAY)),
            )),
        );
        let store = vec![
            Stmt::Let {
                index: base,
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
            },
            Stmt::Let {
                index: kind,
                declared_schema: Some(TypeSchema::Int),
                expr: Self::guest_call(
                    decode,
                    vec![Self::guest_call(
                        helper_read,
                        vec![Expr::Var(carrier), Self::as_number(Expr::Var(base))],
                    )],
                ),
                line,
            },
            Stmt::IfElse {
                condition: is_array,
                then_branch: vec![Stmt::Let {
                    index: ack,
                    declared_schema: Some(TypeSchema::Null),
                    expr: Self::guest_call(
                        setter,
                        vec![Expr::Var(carrier), Expr::Var(base), Expr::Var(len_int)],
                    ),
                    line,
                }],
                else_branch: not_array,
                line,
            },
        ];
        let accept = vec![
            Stmt::Let {
                index: len_int,
                declared_schema: Some(TypeSchema::Int),
                expr: Self::builtin_call(
                    BuiltinFunction::CheckedIntFromNumber.call_index(),
                    vec![Expr::Var(len_float)],
                ),
                line,
            },
            Stmt::IfElse {
                condition: receiver_object,
                then_branch: store,
                else_branch: {
                    let mut again = Vec::new();
                    self.raise_managed_type_error(span, &mut again)?;
                    again
                },
                line,
            },
        ];
        body.push(Stmt::IfElse {
            condition: tag_ok,
            then_branch: vec![Stmt::IfElse {
                condition: in_range,
                then_branch: accept,
                else_branch: reject_range,
                line,
            }],
            else_branch: reject,
            line,
        });
        if let Some(proceed) = proceed {
            after_array.push(Stmt::IfElse {
                condition: Expr::Var(proceed),
                then_branch: body,
                else_branch: Vec::new(),
                line,
            });
        } else {
            after_array.extend(body);
        }
        // Micro JS_SetProperty calls the length setter with an already
        // evaluated RHS, and js_array_set_length checks the receiver with
        // js_get_array before JS_ToInt32. valueOf/ToNumber must not run
        // unless this receiver is already an array. A throw published by
        // the RHS expression itself stays the completion; do not enter the
        // setter, and do not replace that throw with TypeError. A call that
        // returned an ordinary object stores the original RHS through the
        // existing property put. Proven array bindings keep the TypeError
        // for a non-array object, which that path cannot produce.
        let mut not_array = Vec::new();
        if ordinary_object {
            self.emit_ordinary_length_put(receiver, rhs_slot, span, &mut not_array)?;
        } else {
            self.raise_managed_type_error(span, &mut not_array)?;
        }
        let mut not_object = Vec::new();
        self.raise_managed_type_error(span, &mut not_object)?;
        let probe_base = self.slot(span)?;
        let probe_kind = self.slot(span)?;
        let receiver_object = Expr::Eq(
            Box::new(Self::as_number(value::field(receiver, "tag"))),
            Box::new(Self::as_number(Expr::Int(value::OBJECT))),
        );
        let is_array = Expr::Or(
            Box::new(Expr::Eq(
                Box::new(Expr::Var(probe_kind)),
                Box::new(Expr::Int(layout::KIND_ARRAY)),
            )),
            Box::new(Expr::Eq(
                Box::new(Expr::Var(probe_kind)),
                Box::new(Expr::Int(layout::KIND_TAGGED_ARRAY)),
            )),
        );
        let validate = vec![Stmt::IfElse {
            condition: receiver_object,
            then_branch: vec![
                Stmt::Let {
                    index: probe_base,
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
                },
                Stmt::Let {
                    index: probe_kind,
                    declared_schema: Some(TypeSchema::Int),
                    expr: Self::guest_call(
                        decode,
                        vec![Self::guest_call(
                            helper_read,
                            vec![Expr::Var(carrier), Self::as_number(Expr::Var(probe_base))],
                        )],
                    ),
                    line,
                },
                Stmt::IfElse {
                    condition: is_array,
                    then_branch: after_array,
                    else_branch: not_array,
                    line,
                },
            ],
            else_branch: not_object,
            line,
        }];
        if let Some(normal) = self.pending_normal {
            statements.push(Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: validate,
                else_branch: Vec::new(),
                line,
            });
        } else {
            statements.extend(validate);
        }
        Ok(())
    }

    fn lower_array_member_write_stmt(
        &mut self,
        member: &ast::MemberExpr,
        right: &ast::Expr,
        span: swc_common::Span,
        statements: &mut Vec<Stmt>,
    ) -> Result<u16, ParseError> {
        let ast::MemberProp::Computed(property) = &member.prop else {
            return Err(self.error(member.span, "P0 dense arrays admit only integer subscripts"));
        };
        let ast::Expr::Ident(object) = member.obj.as_ref() else {
            return Err(self.error(
                member.span,
                "P0 indexed writes require a numeric-array binding",
            ));
        };
        let name = object.sym.as_str();
        let receiver_slot = self
            .bindings
            .get(name)
            .map(|(slot, _)| *slot)
            .ok_or_else(|| {
                self.error(
                    object.span,
                    "P0 indexed writes require a numeric-array binding",
                )
            })?;
        let shape = self.array_lengths.get(name).copied();
        let literal = Self::member_index_literal(&property.expr);
        // Receiver evaluation/rooting precedes key and RHS and does not
        // consume identity fields from the reusable source binding.
        let (receiver, mut receiver_bound) =
            self.bind_temp(Expr::Var(receiver_slot), member.obj.span())?;
        statements.append(&mut receiver_bound);
        let mut key_slot = None;
        if literal.is_none() {
            let key = self.expr(&property.expr)?;
            if !matches!(key.kind, Kind::Number | Kind::Dynamic) {
                return Err(self.error(
                    property.span(),
                    "P0 dense array indexes must be numeric in this scope",
                ));
            }
            let (slot, mut key_bound) =
                self.bind_evaluation(key.expr, key.kind, property.span())?;
            statements.append(&mut key_bound);
            key_slot = Some((slot, key.kind));
        }
        let value = self.expr(right)?;
        if shape.is_some_and(|shape| shape.repr == ArrayRepr::Packed) && value.kind != Kind::Number
        {
            return Err(self.error(right.span(), "P0 numeric arrays store only numbers"));
        }
        let mut value_roots = value.owned_roots;
        let (value_slot, value_kind, mut value_bound) = (value.expr, value.kind, Vec::new());
        let (value_slot, mut bound) = self.bind_evaluation(value_slot, value_kind, right.span())?;
        value_bound.append(&mut bound);
        statements.append(&mut value_bound);
        let line = self.line(span);
        let mut commit = Vec::new();
        self.emit_array_store(
            member,
            receiver,
            shape.map(|shape| shape.repr),
            literal,
            key_slot,
            value_slot,
            span,
            &mut commit,
        )?;
        if let Some(normal) = self.pending_normal {
            statements.push(Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: commit,
                else_branch: Vec::new(),
                line,
            });
        } else {
            statements.extend(commit);
        }
        value_roots.push(receiver);
        if self.has_root(value_slot) {
            value_roots.push(value_slot);
        }
        reference::release_consumed_roots(self, &value_roots, receiver_slot, span, statements)?;
        Ok(value_slot)
    }

    /// Shared dense-array store emission. Packed in-bounds literal writes
    /// keep the exact historical inline cell write; every other case goes
    /// through the runtime kind check and five-cell writer.
    #[allow(clippy::too_many_arguments)]
    fn emit_array_store(
        &mut self,
        member: &ast::MemberExpr,
        receiver: u16,
        repr: Option<ArrayRepr>,
        literal: Option<i64>,
        key_slot: Option<(u16, Kind)>,
        value_slot: u16,
        span: swc_common::Span,
        out: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let carrier = self.active_carrier(span)?;
        let heap = self.heap_ref(span)?;
        let checked_base = heap.helpers.checked_base;
        let helper_read = heap.helper_read;
        let array_put = heap.helpers.array_put;
        let decode_length = heap.helpers.decode_u32;
        let decode = self
            .helper_decode
            .ok_or_else(|| self.error(span, "P0 array writes require the realm decode helper"))?;
        let line = self.line(span);
        let base_tmp = self.slot(span)?;
        out.push(Stmt::Let {
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
        let index = if let Some(literal) = literal {
            Expr::Int(literal)
        } else {
            let (key, key_kind) = key_slot.ok_or_else(|| {
                self.error(member.span, "P0 dense array store lost its evaluated key")
            })?;
            let idx = self.slot(span)?;
            out.push(Stmt::Let {
                index: idx,
                declared_schema: Some(TypeSchema::Int),
                expr: Self::guest_call(
                    decode,
                    vec![Self::as_number(self.array_index_number(key, key_kind))],
                ),
                line,
            });
            Expr::Var(idx)
        };
        if repr == Some(ArrayRepr::Packed)
            && let Some(literal_index) = literal
        {
            let kind_tmp = self.slot(span)?;
            out.push(Stmt::Let {
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
            out.push(assert_stmt(
                Expr::Or(
                    Box::new(Expr::Eq(
                        Box::new(Expr::Var(kind_tmp)),
                        Box::new(Expr::Int(layout::KIND_ARRAY)),
                    )),
                    Box::new(Expr::Eq(
                        Box::new(Expr::Var(kind_tmp)),
                        Box::new(Expr::Int(layout::KIND_TAGGED_ARRAY)),
                    )),
                ),
                line,
            ));
            let len_tmp = self.slot(span)?;
            out.push(Stmt::Let {
                index: len_tmp,
                declared_schema: Some(TypeSchema::Int),
                expr: Self::guest_call(
                    decode_length,
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
            out.push(assert_stmt(
                Expr::Lt(Box::new(index.clone()), Box::new(Expr::Var(len_tmp))),
                line,
            ));
            let present_raw = self.slot(span)?;
            out.push(Stmt::Let {
                index: present_raw,
                declared_schema: Some(TypeSchema::Int),
                expr: Self::guest_call(
                    decode_length,
                    vec![Self::guest_call(
                        helper_read,
                        vec![
                            Expr::Var(carrier),
                            Self::as_number(Expr::Add(
                                Box::new(Expr::Var(base_tmp)),
                                Box::new(Expr::Int(layout::ARRAY_PRESENT_OFFSET)),
                            )),
                        ],
                    )],
                ),
                line,
            });
            let present_tmp = self.slot(span)?;
            out.push(Stmt::Let {
                index: present_tmp,
                declared_schema: Some(TypeSchema::Int),
                expr: Expr::Sub(
                    Box::new(Expr::Var(present_raw)),
                    Box::new(Expr::Int(1)),
                ),
                line,
            });
            let mut inline = Vec::new();
            self.emit_cell_write(
                carrier,
                Expr::Add(
                    Box::new(Expr::Var(base_tmp)),
                    Box::new(Expr::Int(layout::ARRAY_HEADER_CELLS + literal_index)),
                ),
                value::field(value_slot, "number"),
                span,
                line,
                &mut inline,
            )?;
            let via_put = vec![Stmt::Let {
                index: self.slot(span)?,
                declared_schema: None,
                expr: Self::guest_call(
                    array_put,
                    vec![
                        Expr::Var(carrier),
                        Expr::Var(base_tmp),
                        index.clone(),
                        Expr::Var(value_slot),
                        value::field(receiver, "ref_slot"),
                        value::field(receiver, "generation"),
                    ],
                ),
                line,
            }];
            out.push(Stmt::IfElse {
                condition: Expr::And(
                    Box::new(Expr::Eq(
                        Box::new(Expr::Var(kind_tmp)),
                        Box::new(Expr::Int(layout::KIND_ARRAY)),
                    )),
                    Box::new(Expr::Lt(
                        Box::new(index),
                        Box::new(Expr::Var(present_tmp)),
                    )),
                ),
                then_branch: inline,
                else_branch: via_put,
                line,
            });
        } else {
            let ack = self.slot(span)?;
            out.push(Stmt::Let {
                index: ack,
                declared_schema: None,
                expr: Self::guest_call(
                    array_put,
                    vec![
                        Expr::Var(carrier),
                        Expr::Var(base_tmp),
                        index,
                        Expr::Var(value_slot),
                        value::field(receiver, "ref_slot"),
                        value::field(receiver, "generation"),
                    ],
                ),
                line,
            });
        }
        Ok(())
    }

    pub(super) fn lower_delete(&mut self, unary: &ast::UnaryExpr) -> Result<Lowered, ParseError> {
        let ast::Expr::Member(member) = Self::unwrap_parens(&unary.arg) else {
            return Err(self.error(
                unary.span,
                "P7 delete supports ordinary property references",
            ));
        };
        if self.primitive_key_role(member.span, primitive_key::Role::Delete) {
            return self.lower_primitive_delete(unary, member);
        }
        let left = self.expr(&member.obj)?;
        reference::admit_object_receiver(self, member.span, left.kind)?;
        // Discarded delete: admitted receiver/key capability, no Reference.
        if (self.in_dead_capability() || self.emission_known_throw())
            && let Some(key_expr) = Self::open_computed_key(member)
        {
            let key = self.expr(key_expr)?;
            if self.discard_member_capability(
                member,
                left.kind,
                key.kind,
                primitive_key::Role::Delete,
            )? {
                return Ok(Self::capability_only_value(Kind::Boolean));
            }
        }
        let mut owned = left.owned_roots;
        let (base, mut body) = self.bind_temp(left.expr, member.obj.span())?;
        owned.push(base);
        let units = self.member_key_units(member)?;
        let key = self.emit_utf16_node(&units, member.span, &mut body)?;
        owned.push(key);
        let carrier = self.active_carrier(unary.span)?;
        let helper = self
            .objects
            .ok_or_else(|| self.error(unary.span, "ordinary runtime missing"))?
            .delete;
        let reference = value::reference(
            Expr::Var(base),
            value::field(key, "ref_slot"),
            value::field(key, "generation"),
            Expr::Bool(self.enclosing_strict),
        );
        let out = self.consume_result(
            Self::guest_call(helper, vec![Expr::Var(carrier), reference]),
            TypeSchema::Bool,
            Expr::Bool(false),
            |slot| value::boolean(Expr::Var(slot)),
            unary.span,
            &mut body,
        )?;
        reference::release_consumed_roots(self, &owned, out, unary.span, &mut body)?;
        Ok(Lowered {
            expr: Expr::Block {
                stmts: body,
                expr: Box::new(Expr::Var(out)),
            },
            kind: Kind::Boolean,
            array_length: None,
            owned_roots: vec![out],
        })
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
        // A Dynamic receiver routed to the proved String-length adapter may
        // actually hold a dense Array at runtime (call result, method result,
        // parameter alias). Dispatch on the boxed tag and the live node kind so
        // `length` reads the real Array header; every other runtime value
        // keeps the established adapter/ordinary-Get path unchanged.
        let dynamic_length = base.kind == Kind::Dynamic && self.has_string_length_site(member);
        let ref_get = if self.has_string_length_site(member) {
            self.helper_string_length.ok_or_else(|| {
                self.error(
                    member.span,
                    "String length adapter missing before read emission",
                )
            })?
        } else {
            ref_get
        };
        let mut owned_roots = base.owned_roots;
        let (base_slot, base_stmt) = self.bind_temp(base.expr, member.obj.span())?;
        owned_roots.push(base_slot);
        let key_units = self.member_key_units(member)?;
        let mut stmts = base_stmt;
        let (value_slot, saved) = self.bind_temp_bare(value::undefined(), member.span)?;
        stmts.push(saved);
        let mut access = Vec::new();
        let key_box = self.emit_utf16_node(&key_units, member.span, &mut access)?;
        owned_roots.push(key_box);
        let reference = value::reference(
            Expr::Var(base_slot),
            value::field(key_box, "ref_slot"),
            value::field(key_box, "generation"),
            Expr::Bool(self.enclosing_strict),
        );
        let (reference, saved) =
            self.bind_typed(reference, value::reference_schema(), member.span)?;
        access.push(saved);
        let line = self.line(member.span);
        self.reaffirm_root(carrier, base_slot, member.span, line, &mut access)?;
        self.reaffirm_root(carrier, key_box, member.span, line, &mut access)?;
        if !dynamic_length {
            let ret = self.consume_value_result(
                Self::guest_call(ref_get, vec![Expr::Var(carrier), Expr::Var(reference)]),
                member.span,
                &mut access,
            )?;
            access.push(Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: value_slot,
                expr: Expr::Var(ret),
                line,
            });
            self.refresh_root(value_slot, member.span, &mut access)?;
        } else {
            // The ordinary read is retained verbatim as the runtime fallback;
            // only a live kind-1/kind-6 node takes the Array header path.
            let mut fallback = Vec::new();
            let ret = self.consume_value_result(
                Self::guest_call(ref_get, vec![Expr::Var(carrier), Expr::Var(reference)]),
                member.span,
                &mut fallback,
            )?;
            fallback.push(Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: value_slot,
                expr: Expr::Var(ret),
                line,
            });
            self.refresh_root(value_slot, member.span, &mut fallback)?;
            let heap = self.heap_ref(member.span)?;
            let (checked_base, array_length) =
                (heap.helpers.checked_base, heap.helpers.array_length);
            let helper_read = heap.helper_read;
            let decode = self.helper_decode.ok_or_else(|| {
                self.error(
                    member.span,
                    "P0 array length requires the realm decode helper",
                )
            })?;
            let base_tmp = self.slot(member.span)?;
            let kind_tmp = self.slot(member.span)?;
            // Both temporaries dominate the gate that reads them.
            access.push(Stmt::Let {
                index: base_tmp,
                declared_schema: Some(TypeSchema::Int),
                expr: Expr::Int(-1),
                line,
            });
            access.push(Stmt::Let {
                index: kind_tmp,
                declared_schema: Some(TypeSchema::Int),
                expr: Expr::Int(-1),
                line,
            });
            access.push(Stmt::IfElse {
                condition: Expr::Eq(
                    Box::new(Self::as_number(value::field(base_slot, "tag"))),
                    Box::new(Self::as_number(Expr::Int(value::OBJECT))),
                ),
                then_branch: vec![
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: base_tmp,
                        expr: Self::guest_call(
                            checked_base,
                            vec![
                                Expr::Var(carrier),
                                value::field(base_slot, "ref_slot"),
                                value::field(base_slot, "generation"),
                            ],
                        ),
                        line,
                    },
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: kind_tmp,
                        expr: Self::guest_call(
                            decode,
                            vec![Self::guest_call(
                                helper_read,
                                vec![Expr::Var(carrier), Self::as_number(Expr::Var(base_tmp))],
                            )],
                        ),
                        line,
                    },
                ],
                else_branch: Vec::new(),
                line,
            });
            let mut array_arm = vec![Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: value_slot,
                expr: value::number(Self::as_number(Self::guest_call(
                    array_length,
                    vec![Expr::Var(carrier), Expr::Var(base_tmp)],
                ))),
                line,
            }];
            self.refresh_root(value_slot, member.span, &mut array_arm)?;
            access.push(Stmt::IfElse {
                condition: Expr::Or(
                    Box::new(Expr::Eq(
                        Box::new(Expr::Var(kind_tmp)),
                        Box::new(Expr::Int(layout::KIND_ARRAY)),
                    )),
                    Box::new(Expr::Eq(
                        Box::new(Expr::Var(kind_tmp)),
                        Box::new(Expr::Int(layout::KIND_TAGGED_ARRAY)),
                    )),
                ),
                then_branch: array_arm,
                else_branch: fallback,
                line,
            });
        }
        owned_roots.push(value_slot);

        if let Some(normal) = self.pending_normal {
            stmts.push(Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: access,
                else_branch: Vec::new(),
                line,
            });
        } else {
            stmts.extend(access);
        }
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
    pub(super) fn unwrap_parens(expression: &ast::Expr) -> &ast::Expr {
        let mut current = expression;
        while let ast::Expr::Paren(p) = current {
            current = &p.expr;
        }
        current
    }

    /// Evaluate the test once. Only the selected arm runs, and neither arm
    /// runs when the test completed abruptly. The result kind is the existing
    /// flow join; Ident/Member arms are values, not References.
    fn lower_conditional(&mut self, branch: &ast::CondExpr) -> Result<Lowered, ParseError> {
        let (flag, mut stmts) = self.condition_flag(&branch.test)?;
        // A definitely throwing test means neither arm can run. Their IR is
        // discarded, so lower them for capability only: the same static
        // admissions run, but a prepared Reference proof may not be published.
        let test_threw = self.emission_known_throw();
        let incoming = super::lowerer::expression::BranchFacts::save(self);
        let names = self.type_names;
        let (cons, cons_threw) = if test_threw {
            self.with_dead_capability(|lower| {
                lower.isolate_conditional_emission(|lower| lower.expr(&branch.cons))
            })?
        } else {
            self.isolate_conditional_emission(|lower| lower.expr(&branch.cons))?
        };
        let cons_facts = super::lowerer::expression::BranchFacts::save(self);
        incoming.clone().restore(self);
        self.type_names = names;
        let (alt, alt_threw) = if test_threw {
            self.with_dead_capability(|lower| {
                lower.isolate_conditional_emission(|lower| lower.expr(&branch.alt))
            })?
        } else {
            self.isolate_conditional_emission(|lower| lower.expr(&branch.alt))?
        };
        if cons_threw && alt_threw {
            incoming.restore(self);
            self.note_emission_throw();
        } else if cons_threw {
            // The alternate is the only normal completion.
        } else if alt_threw {
            cons_facts.restore(self);
        } else {
            cons_facts.merge(self);
        }
        self.type_names = names;
        let kind = super::lowerer::expression::join_kind(cons.kind, alt.kind);
        let array_length =
            super::lowerer::expression::join_length(kind, cons.array_length, alt.array_length);
        let selected = Expr::IfElse {
            condition: Box::new(Expr::Var(flag)),
            then_expr: Box::new(cons.expr),
            else_expr: Box::new(alt.expr),
        };
        let chosen = if let Some(normal) = self.pending_normal {
            Expr::IfElse {
                condition: Box::new(Expr::Var(normal)),
                then_expr: Box::new(selected),
                else_expr: Box::new(value::undefined()),
            }
        } else {
            selected
        };
        let (result, mut bound) = self.bind_evaluation(chosen, kind, branch.span)?;
        stmts.append(&mut bound);
        let owned = Self::compose_owned(cons.owned_roots.into_iter().chain(alt.owned_roots));
        reference::release_consumed_roots(self, &owned, result, branch.span, &mut stmts)?;
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(Expr::Var(result)),
            },
            kind,
            array_length,
            owned_roots: if self.has_root(result) {
                vec![result]
            } else {
                Vec::new()
            },
        })
    }

    /// Left to right. A thrown operand skips the rest. The value is the last
    /// operand, so a member in the tail is not a call Reference.
    fn lower_sequence(&mut self, sequence: &ast::SeqExpr) -> Result<Lowered, ParseError> {
        let Some((first, rest)) = sequence.exprs.split_first() else {
            return Err(self.error(
                sequence.span,
                "unsupported JavaScript ES5 P0 primitive expression",
            ));
        };
        let current = self.expr(first)?;
        let mut kind = current.kind;
        let mut array_length = current.array_length;
        let mut owned = current.owned_roots;
        let (mut saved, bound) = self.bind_evaluation(current.expr, kind, first.span())?;
        let mut stmts = bound;
        if self.has_root(saved) {
            owned.push(saved);
        }
        for expr in rest {
            if self.emission_known_throw() {
                break;
            }
            let next = self.expr(expr)?;
            kind = next.kind;
            array_length = next.array_length;
            owned.extend(next.owned_roots);
            let selected = if let Some(normal) = self.pending_normal {
                Expr::IfElse {
                    condition: Box::new(Expr::Var(normal)),
                    then_expr: Box::new(next.expr),
                    else_expr: Box::new(Expr::Var(saved)),
                }
            } else {
                next.expr
            };
            let (slot, mut bound) = self.bind_evaluation(selected, kind, expr.span())?;
            stmts.append(&mut bound);
            if self.has_root(slot) {
                owned.push(slot);
            }
            saved = slot;
        }
        reference::release_consumed_roots(self, &owned, saved, sequence.span, &mut stmts)?;
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(Expr::Var(saved)),
            },
            kind,
            array_length,
            owned_roots: if self.has_root(saved) {
                vec![saved]
            } else {
                Vec::new()
            },
        })
    }

    /// Evaluate and root the callee/receiver before any actual expression.
    /// `rooting` is the enclosing statement's destination. Only an immediate
    /// local `var` initializer suppresses the temporary result publication:
    /// the common callee/input retirement and binding store are non-collecting,
    /// so the private result box reaches the rooted binding directly.
    fn lower_call_expr(
        &mut self,
        call: &ast::CallExpr,
        rooting: CallResultRooting,
    ) -> Result<Lowered, ParseError> {
        self.lower_call_or_intrinsic_construct(call, rooting, None)
    }

    fn lower_call_or_intrinsic_construct(
        &mut self,
        call: &ast::CallExpr,
        rooting: CallResultRooting,
        construct: Option<(u16, i64)>,
    ) -> Result<Lowered, ParseError> {
        reference::admit_call_callee(self, call)?;
        let ast::Callee::Expr(callee) = &call.callee else {
            unreachable!()
        };
        let span = call.span;
        // `Array.isArray(arguments)` passes the activation's implicit binding
        // as a real actual. The call is a real Get and Call: `Array.isArray`
        // is guest-writable, so a constant fold would observe `true` after a
        // program replaced the method with a function returning false. The
        // bare `arguments` actual is the invocation vector itself.
        let arguments_actual = self.function_args.is_some()
            && call.args.len() == 1
            && super::reference::is_arguments_ident(&call.args[0].expr)
            && super::reference::implicit_arguments(self)
            && matches!(
                Self::unwrap_parens(callee),
                ast::Expr::Member(member)
                    if super::reference::is_array_is_array_member(self, member)
            );
        let arguments_slot = self.function_args;
        if self.in_dead_capability()
            && let ast::Expr::Ident(ident) = Self::unwrap_parens(callee)
            && matches!(reference::resolve(self, ident.sym.as_str()), reference::NameHit::BuiltinOptionalError(_))
        {
            // Callee and supported literal actuals were capability-checked above.
            return Ok(Self::capability_only_read());
        }
        // A discarded `obj[key](...)` callee has its prepared Reference
        // suppressed with the rest of the dead walk. Run the exact receiver,
        // key, callable and actual admission the live route uses, then emit a
        // capability-only placeholder instead of a Reference/helper chain.
        if let ast::Expr::Member(member) = Self::unwrap_parens(callee)
            && (self.in_dead_capability() || self.emission_known_throw())
            && let Some(key_expr) = Self::open_computed_key(member)
        {
            let base = self.expr(&member.obj)?;
            let key = self.expr(key_expr)?;
            if self.discard_member_capability(
                member,
                base.kind,
                key.kind,
                primitive_key::Role::Call,
            )? {
                reference::admit_callable_kind(self, member.span, Kind::Dynamic)?;
                for arg in &call.args {
                    let _ = self.expr(&arg.expr)?;
                }
                return Ok(Self::capability_only_read());
            }
        }
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
                if self.primitive_key_role(member.span, primitive_key::Role::Call) {
                    let (func, receiver, saved, extra) =
                        self.prepare_primitive_call(member, span)?;
                    stmts.extend(saved);
                    owned.extend(extra);
                    owned.push(func);
                    owned.push(receiver);
                    (func, receiver)
                } else {
                    let base = self.expr(&member.obj)?;
                    if !reference::named_primitive_read(self, member, base.kind)? {
                        reference::admit_object_receiver(self, member.span, base.kind)?;
                    }
                    let callee_kind = reference::admit_object_member_read(self, member, base.kind)?;
                    reference::admit_callable_kind(self, member.span, callee_kind)?;
                    owned.extend(base.owned_roots);
                    let (receiver, mut saved) = self.bind_temp(base.expr, span)?;
                    stmts.append(&mut saved);
                    owned.push(receiver);
                    let (func, saved) = self.bind_temp_bare(value::undefined(), span)?;
                    stmts.push(saved);
                    let mut access = Vec::new();
                    let units = self.member_key_units(member)?;
                    let key = self.emit_utf16_node(&units, span, &mut access)?;
                    owned.push(key);
                    let reference = value::reference(
                        Expr::Var(receiver),
                        value::field(key, "ref_slot"),
                        value::field(key, "generation"),
                        Expr::Bool(self.enclosing_strict),
                    );
                    let (reference, saved) =
                        self.bind_typed(reference, value::reference_schema(), span)?;
                    access.push(saved);
                    let get = self
                        .helper_ref_get
                        .ok_or_else(|| self.error(span, "ordinary Get helper missing"))?;
                    let got = self.consume_value_result(
                        Self::guest_call(get, vec![Expr::Var(carrier), Expr::Var(reference)]),
                        span,
                        &mut access,
                    )?;
                    access.push(Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: func,
                        expr: Expr::Var(got),
                        line: self.line(span),
                    });
                    self.refresh_root(func, span, &mut access)?;
                    if let Some(normal) = self.pending_normal {
                        stmts.push(Stmt::IfElse {
                            condition: Expr::Var(normal),
                            then_branch: access,
                            else_branch: Vec::new(),
                            line: self.line(span),
                        });
                    } else {
                        stmts.extend(access);
                    }
                    owned.push(func);
                    (func, receiver)
                }
            }
            other => {
                // GetValue once. The saved box is rooted before any actual.
                // Strict source functions receive undefined; this is not a
                // Reference, so a member inside the expression does not
                // contribute its base.
                let lowered = self.expr(other)?;
                reference::admit_callable_kind(self, other.span(), lowered.kind)?;
                owned.extend(lowered.owned_roots);
                let (func, mut saved) = self.bind_temp(lowered.expr, span)?;
                stmts.append(&mut saved);
                owned.push(func);
                let (receiver, saved) = self.bind_temp_bare(value::undefined(), span)?;
                stmts.push(saved);
                (func, receiver)
            }
        };
        // The Core container does not trace guest payloads. Save/root each
        // actual independently, including extras, across subsequent effects.
        let mut actuals = Vec::new();
        let mut actual_groups = Vec::new();
        if !self.emission_known_throw() {
            for arg in &call.args {
                let actual = if arguments_actual {
                    // Pinned Micro hands over the activation's own Array, not a
                    // fresh copy: bind it once, mirror the vector into it on
                    // later passes, and let the callee keep the same object.
                    let object = self.prepare_arguments_object(
                        arguments_slot.expect("arguments actual slot"),
                        arg.span(),
                        &mut stmts,
                    )?;
                    Lowered {
                        expr: object,
                        kind: Kind::Array,
                        array_length: None,
                        owned_roots: Vec::new(),
                    }
                } else {
                    self.expr(&arg.expr)?
                };
                owned.extend(actual.owned_roots);
                let (slot, saved) = self.bind_temp(actual.expr, arg.span())?;
                owned.push(slot);
                actuals.push(slot);
                actual_groups.push((arg.span(), saved));
                if self.emission_known_throw() {
                    break;
                }
            }
        }
        // A method call can mutate its Array receiver through the real heap.
        // Static literal lengths and packed-cell folds must be retired for all
        // bindings: another binding may alias this same Array.
        if let ast::Expr::Member(member) = Self::unwrap_parens(callee) {
            let mutates_array = match &member.prop {
                ast::MemberProp::Ident(id) => matches!(id.sym.as_ref(), "push" | "pop" | "shift" | "unshift" | "splice" | "sort"),
                ast::MemberProp::Computed(computed) => matches!(Self::unwrap_parens(&computed.expr), ast::Expr::Lit(ast::Lit::Str(text)) if text.value == *"push" || text.value == *"pop" || text.value == *"shift" || text.value == *"unshift" || text.value == *"splice" || text.value == *"sort"),
                _ => false,
            };
            if mutates_array {
                self.array_lengths.clear();
                self.array_length_ids.clear();
            }
        }
        let mut tail = Vec::new();
        let (vector, saved) = self.bind_typed(
            Self::builtin_call(BuiltinFunction::ArrayNew.call_index(), Vec::new()),
            TypeSchema::Array(Box::new(value::schema())),
            span,
        )?;
        tail.push(saved);
        for actual in actuals {
            tail.push(Stmt::Assign {
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
        let completed = if let Some((helper, mode)) = construct {
            Self::guest_call(helper, vec![
                Expr::Var(carrier), Expr::Int(mode), Expr::Var(receiver), Expr::Var(vector),
            ])
        } else {
            Self::guest_call(dispatch, vec![Expr::Var(carrier), Expr::Var(func), Expr::Var(receiver), Expr::Var(vector)])
        };
        let (raw, saved) = self.bind_typed(completed, value::logical_result_schema(), span)?;
        tail.push(saved);
        if arguments_actual {
            self.finish_arguments_object(
                arguments_slot.expect("arguments actual slot"),
                span,
                &mut tail,
            )?;
        }
        let ok_slot = self.slot(span)?;
        let err_slot = self.slot(span)?;
        let (value_slot, value_stmt) = self.bind_temp_bare(value::undefined(), span)?;
        stmts.push(value_stmt);
        tail.push(Stmt::Let {
            index: ok_slot,
            declared_schema: None,
            expr: value::completion_normal(),
            line: self.line(span),
        });
        tail.push(Stmt::Let {
            index: err_slot,
            declared_schema: Some(value::schema()),
            expr: value::undefined(),
            line: self.line(span),
        });
        let mut ok_arm = Vec::new();
        ok_arm.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: value_slot,
            expr: Expr::IfElse {
                condition: Box::new(Expr::Eq(
                    // This binding is the Ok arm of dispatch's exact private
                    // Result<Completion, JsValue>, not a user property. The
                    // completion constructors write Int enum kinds 0..=4;
                    // object-intrinsic success writes completion_return (1).
                    // Keep one Get in its original evaluation position. The
                    // outer Result.tag and Completion.has_value remain Bool.
                    Box::new(Self::guest_call(
                        self.helper_int_identity.ok_or_else(|| {
                            self.error(span, "private Int identity helper missing")
                        })?,
                        vec![value::field(ok_slot, "kind")],
                    )),
                    Box::new(Expr::Int(value::COMPLETION_RETURN)),
                )),
                then_expr: Box::new(value::field(ok_slot, "value")),
                else_expr: Box::new(value::undefined()),
            },
            line: self.line(span),
        });
        // The Ok arm extracts the private completion value into its result
        // slot. A temporary destination publishes a frame root for it; the
        // immediate local initializer hands the still-unpublished box straight
        // to the binding store, which roots it under the binding identity.
        if rooting == CallResultRooting::Temporary {
            self.refresh_root(value_slot, span, &mut ok_arm)?;
        }
        let (_, retained) = self.bind_typed(
            Self::builtin_call(BuiltinFunction::Len.call_index(), vec![Expr::Var(vector)]),
            TypeSchema::Int,
            span,
        )?;
        ok_arm.push(retained);
        let mut err_arm = Vec::new();
        self.refresh_root(err_slot, span, &mut err_arm)?;
        self.publish_callee_throw(err_slot, span, &mut err_arm)?;
        // P6B-PENDING-RELEASE: the caller err slot handed its box to the
        // surviving pending owner. Release the caller slot with the existing
        // nonallocating clear so only pending roots the payload.
        self.clear_root(err_slot, span, &mut err_arm)?;
        let matched = Expr::ResultMatch {
            value: Box::new(Expr::Var(raw)),
            value_slot: raw,
            ok_binding: Some(ok_slot),
            ok_expr: Box::new(Expr::Block {
                stmts: ok_arm,
                expr: Box::new(Expr::Var(value_slot)),
            }),
            err_binding: Some(err_slot),
            err_expr: Box::new(Expr::Block {
                stmts: err_arm,
                expr: Box::new(value::undefined()),
            }),
            scrutinee_schema: value::logical_result_schema(),
            span: self.ir_span(span),
            semantic_id: None,
        };
        let expr = if let Some(normal) = self.pending_normal {
            tail.push(Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: value_slot,
                expr: matched,
                line: self.line(span),
            });
            let mut nested = tail;
            for (arg_span, saved) in actual_groups.into_iter().rev() {
                let mut step = saved;
                step.push(Stmt::IfElse {
                    condition: Expr::Var(normal),
                    then_branch: nested,
                    else_branch: Vec::new(),
                    line: self.line(arg_span),
                });
                nested = step;
            }
            stmts.push(Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: nested,
                else_branch: Vec::new(),
                line: self.line(span),
            });
            Expr::Var(value_slot)
        } else {
            for (_, saved) in actual_groups {
                stmts.extend(saved);
            }
            stmts.extend(tail);
            stmts.push(Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: value_slot,
                expr: matched,
                line: self.line(span),
            });
            Expr::Var(value_slot)
        };
        reference::release_consumed_roots(self, &owned, value_slot, span, &mut stmts)?;
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(expr),
            },
            kind: Kind::Dynamic,
            array_length: None,
            // An immediate local initializer never published its result
            // temp, so there is no result root for the store to clear.
            owned_roots: match rooting {
                CallResultRooting::Temporary => vec![value_slot],
                CallResultRooting::ImmediateLocalInitializer => Vec::new(),
            },
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
    /// Identifier, string and numeric literal keys share exact runtime and
    /// duplicate-check units. Computed property names remain rejected.
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
            ast::PropName::Num(value) => {
                let units: Vec<u16> = super::admission::number_to_string(value.value)
                    .encode_utf16()
                    .collect();
                Ok((units.clone(), units))
            }
            _ => Err(self.error(
                span,
                "P0 plain objects admit only identifier, string-literal and numeric-literal keys",
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
        let mut entries = Vec::new();
        let mut names = std::collections::HashSet::new();
        for prop in &literal.props {
            let ast::PropOrSpread::Prop(prop) = prop else {
                return Err(self.error(prop.span(), "P7 ordinary literals reject spread"));
            };
            let (key, expr, role) = match prop.as_ref() {
                ast::Prop::KeyValue(kv) => (kv.key.clone(), kv.value.clone(), LiteralRole::Data),
                ast::Prop::Getter(getter) => (
                    getter.key.clone(),
                    Box::new(ast::Expr::Fn(ast::FnExpr {
                        ident: None,
                        function: getter.function.clone(),
                    })),
                    LiteralRole::Getter,
                ),
                ast::Prop::Setter(setter) => (
                    setter.key.clone(),
                    Box::new(ast::Expr::Fn(ast::FnExpr {
                        ident: None,
                        function: setter.function.clone(),
                    })),
                    LiteralRole::Setter,
                ),
                _ => {
                    return Err(self.error(
                        prop.span(),
                        "P7 ordinary literals admit data, getter and setter properties",
                    ));
                }
            };
            let (units, _) = self.object_key_units(&key, prop.span())?;
            names.insert(units.clone());
            if names.len() > MAX_OBJECT_PROPERTIES {
                return Err(self.error(
                    literal.span,
                    "P7 ordinary objects admit at most 256 own properties",
                ));
            }
            entries.push((units, expr, prop.span(), role));
        }
        Ok(entries)
    }

    /// The admitted regexp literals, `/a/g` and `/a/`.
    ///
    /// The profile layer proved the exact pattern and flags before this site
    /// is reached, so any other pattern, any other flag set, or any other
    /// regexp form keeps the existing refusal. `/a/g` keeps its earlier
    /// zero-own-row ordinary object: no `.source`/`.flags`/`.test`/`.exec`/
    /// `lastIndex` surface is admitted for it. `/a/` reuses the existing
    /// object runtime to publish a fresh object node whose private kind is
    /// `KIND_REGEXP` and whose one own data row is the `test` method the
    /// `append_matcher` guest kernel backs; no permanent intrinsic object and
    /// no second matcher is installed, and nothing is rooted through the
    /// carrier's empty Core array.
    fn lower_regexp_literal(&mut self, literal: &ast::Regex) -> Result<Lowered, ParseError> {
        if literal.exp.as_str() == "a" && literal.flags.as_str().is_empty() {
            // The value, its key, its method and the whole matcher already
            // exist inside the object runtime, so this site evaluates one
            // guest call and binds its real JsValue result. Nothing here is a
            // folded Boolean and no allocation is duplicated outside the
            // helper's own root frame.
            let helper = self
                .objects
                .ok_or_else(|| {
                    self.error(
                        literal.span,
                        "the admitted regexp test literal requires the object runtime",
                    )
                })?
                .regexp_value;
            let carrier = self.active_carrier(literal.span)?;
            let line = self.line(literal.span);
            // The helper really requests four guest nodes on every
            // evaluation: the four-unit `test` key String, the method
            // function, the owner object and its private environment record.
            // Charge exactly those payloads once for this source site through
            // the same page/header arithmetic `begin_node` uses. The runtime
            // constructor keeps its own heap/root limit checks; this restores
            // only the static budget those helper-internal reservations used
            // to bypass.
            self.charge_node_request(layout::STRING_HEADER_CELLS + 4, literal.span)?;
            self.charge_node_request(
                layout::FUNCTION_CELLS + layout::OBJECT_CELLS_PER_PROPERTY,
                literal.span,
            )?;
            self.charge_node_request(
                layout::OBJECT_HEADER_CELLS + layout::OBJECT_CELLS_PER_PROPERTY,
                literal.span,
            )?;
            self.charge_node_request(
                layout::ENV_HEADER_CELLS + layout::ENV_CELLS_PER_ENTRY,
                literal.span,
            )?;
            // The literal is a latent allocation: after an abrupt completion
            // the whole helper call, its bind and both root writes are
            // suppressed, so the escaping destination keeps a real typed
            // Undefined placeholder instead of committing an object. The
            // destination root entry is written once outside the conditional
            // region, so the skipped arm only ever publishes that same static
            // Undefined and never reads an uninitialized local.
            let dest = self.slot(literal.span)?;
            let mut outer: Vec<Stmt> = vec![Stmt::Let {
                index: dest,
                declared_schema: Some(value::schema()),
                expr: value::undefined(),
                line,
            }];
            // The destination is a fresh slot with no root entry yet, so the
            // initial publication must not pass through the `has_root` guard:
            // it reserves the entry and publishes the canonical empty cells
            // through the same three-slot clear the retirement uses.
            self.write_root_clear(dest, literal.span, &mut outer)?;
            let mut taken: Vec<Stmt> = Vec::new();
            let (slot, mut bind) = self.bind_temp(
                Self::guest_call(helper, vec![Expr::Var(carrier)]),
                literal.span,
            )?;
            taken.append(&mut bind);
            taken.push(Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: dest,
                expr: Expr::Var(slot),
                line,
            });
            self.refresh_root(dest, literal.span, &mut taken)?;
            if let Some(normal) = self.pending_normal {
                outer.push(Stmt::IfElse {
                    condition: Expr::Var(normal),
                    then_branch: taken,
                    else_branch: Vec::new(),
                    line,
                });
            } else {
                outer.extend(taken);
            }
            return Ok(Lowered {
                expr: Expr::Block {
                    stmts: outer,
                    expr: Box::new(Expr::Var(dest)),
                },
                kind: Kind::Object,
                array_length: None,
                owned_roots: vec![dest],
            });
        }
        if literal.exp.as_str() != "a" || literal.flags.as_str() != "g" {
            return Err(self.error(
                literal.span,
                "unsupported JavaScript ES5 P0 primitive expression",
            ));
        }
        let carrier = self.active_carrier(literal.span)?;
        let line = self.line(literal.span);
        let guarded = self.pending_normal;
        let mut alloc_stmts: Vec<Stmt> = Vec::new();
        let newborn = self.begin_node(
            carrier,
            layout::OBJECT_HEADER_CELLS,
            layout::KIND_OBJECT,
            literal.span,
            line,
            &mut alloc_stmts,
        )?;
        // Header remainder: zero own properties plus the null prototype pair.
        // Kind/generation are stamped by reservation.
        for (offset, marker) in [(2i64, layout::OBJECT_EXTENSIBLE_FLAG), (3i64, 0), (4i64, 0)] {
            self.emit_cell_write(
                carrier,
                Expr::Add(
                    Box::new(Expr::Var(newborn.base)),
                    Box::new(Expr::Int(offset)),
                ),
                Self::as_number(Expr::Int(marker)),
                literal.span,
                line,
                &mut alloc_stmts,
            )?;
        }
        if let Some(intrinsics) = self.intrinsics {
            let prototype =
                Self::guest_call(intrinsics.root, vec![Expr::Var(carrier), Expr::Int(0)]);
            for (offset, name) in [(3, "ref_slot"), (4, "generation")] {
                self.emit_cell_write(
                    carrier,
                    Expr::Add(
                        Box::new(Expr::Var(newborn.base)),
                        Box::new(Expr::Int(offset)),
                    ),
                    Self::as_number(value::field_of(prototype.clone(), name)),
                    literal.span,
                    line,
                    &mut alloc_stmts,
                )?;
            }
        }
        // Declare the escaping destination outside the conditional region so
        // an abrupt completion leaves a real typed Undefined placeholder
        // instead of committing this object.
        let dest = self.slot(literal.span)?;
        let mut outer: Vec<Stmt> = Vec::new();
        outer.push(Stmt::Let {
            index: dest,
            declared_schema: Some(value::schema()),
            expr: value::undefined(),
            line,
        });
        self.publish_into(dest, carrier, &newborn, literal.span, line, &mut alloc_stmts)?;
        if let Some(normal) = guarded {
            outer.push(Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: alloc_stmts,
                else_branch: Vec::new(),
                line,
            });
        } else {
            outer.extend(alloc_stmts);
        }
        Ok(Lowered {
            expr: Expr::Block {
                stmts: outer,
                expr: Box::new(Expr::Var(dest)),
            },
            kind: Kind::Object,
            array_length: None,
            owned_roots: vec![dest],
        })
    }

    pub(super) fn typed_array_used(&self) -> bool {
        self.typed_array_elements.get() != 0
    }

    fn lower_new_expr(&mut self, construct: &ast::NewExpr) -> Result<Lowered, ParseError> {
        // One route resolution, shared with the scan and the validation walk.
        // Every arm lowers what `construct_route` decided; no arm re-derives
        // the binding questions from the spelling.
        match reference::construct_route(Some(self), construct) {
            reference::ConstructRoute::Array(call) => {
                // The alias read may not have selected the namespace arm; the
                // runtime value still dispatches through the Array constructor.
                self.array_constructor_used.set(true);
                self.lower_call_expr(&call, CallResultRooting::Temporary)
            }
            reference::ConstructRoute::ArrayBuffer(call) => {
                let helper = self
                    .intrinsics
                    .and_then(|i| i.array_buffer)
                    .expect("ArrayBuffer helper");
                self.lower_call_or_intrinsic_construct(
                    &call,
                    CallResultRooting::Temporary,
                    Some((helper, 0)),
                )
            }
            reference::ConstructRoute::TypedArray { call, element } => {
                let helper = self
                    .intrinsics
                    .and_then(|i| i.typed_array)
                    .expect("typed array helper");
                self.typed_array_elements
                    .set(self.typed_array_elements.get() | element.bit());
                self.lower_call_or_intrinsic_construct(
                    &call,
                    CallResultRooting::Temporary,
                    Some((helper, element.construct_mode())),
                )
            }
            reference::ConstructRoute::Object { alias: Some(_) } => {
                self.lower_new_object_alias(construct)
            }
            reference::ConstructRoute::Object { alias: None } => {
                self.lower_new_object(construct)
            }
            reference::ConstructRoute::Error => self.lower_new_error(construct),
            reference::ConstructRoute::NativeError { alias, .. } => {
                self.lower_new_native_error(construct, alias)
            }
            reference::ConstructRoute::OptionalError(_) => {
                self.lower_new_native_error(construct, None)
            }
            reference::ConstructRoute::Date => self.lower_new_date(construct),
            reference::ConstructRoute::Unsupported => self.lower_new_unsupported(construct),
            reference::ConstructRoute::Source => self.lower_source_construct(construct),
        }
    }
    /// `new` over a builtin-constructor alias: the callee identifier is only a
    /// value read, so the empty-object allocation runs exactly like the direct
    /// `new Object()` spelling.
    fn lower_new_object_alias(&mut self, construct: &ast::NewExpr) -> Result<Lowered, ParseError> {
        let empty = ast::ObjectLit {
            span: construct.span,
            props: Vec::new(),
        };
        self.lower_object_literal(&empty)
    }

    /// Upstream keeps only `new Number`/`new String`/`new Boolean` as runtime
    /// TemplateErrors, but the actuals are ordinary expressions: they run in
    /// source order before the constructor TypeError is raised, and the first
    /// one that throws wins. The message text is not part of the contract; the
    /// catchable `name` is.
    fn lower_new_unsupported(&mut self, construct: &ast::NewExpr) -> Result<Lowered, ParseError> {
        let span = construct.span;
        let carrier = self.active_carrier(span)?;
        let type_error = self.intrinsics.map(|i| i.type_error).ok_or_else(|| {
            self.error(
                span,
                "the admitted Number/String/Boolean construction requires the object runtime",
            )
        })?;
        // Evaluate every admitted actual in order, dropping the value but
        // keeping its side effects. Each later actual and the final TypeError
        // sit behind the previous step's Normal guard, exactly like a Call's
        // actual list: an abrupt actual suppresses all later evaluation.
        let normal = self.pending_normal;
        let mut actual_groups = Vec::new();
        for arg in construct.args.iter().flatten() {
            let actual = self.expr(&arg.expr)?;
            let (_slot, saved) = self.bind_temp_bare(actual.expr, arg.span())?;
            actual_groups.push((arg.span(), vec![saved]));
        }
        let (slot, mut throw) = self.bind_temp(value::undefined(), span)?;
        throw.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: slot,
            expr: Self::guest_call(type_error, vec![Expr::Var(carrier)]),
            line: self.line(span),
        });
        self.refresh_root(slot, span, &mut throw)?;
        self.publish_callee_throw(slot, span, &mut throw)?;
        self.clear_root(slot, span, &mut throw)?;
        let mut stmts = Vec::new();
        match normal {
            Some(normal) => {
                let mut nested = vec![Stmt::IfElse {
                    condition: Expr::Var(normal),
                    then_branch: throw,
                    else_branch: Vec::new(),
                    line: self.line(span),
                }];
                for (arg_span, mut step) in actual_groups.into_iter().rev() {
                    step.push(Stmt::IfElse {
                        condition: Expr::Var(normal),
                        then_branch: nested,
                        else_branch: Vec::new(),
                        line: self.line(arg_span),
                    });
                    nested = step;
                }
                stmts.push(Stmt::IfElse {
                    condition: Expr::Var(normal),
                    then_branch: nested,
                    else_branch: Vec::new(),
                    line: self.line(span),
                });
            }
            None => {
                for (_, step) in actual_groups {
                    stmts.extend(step);
                }
                stmts.extend(throw);
            }
        }
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(value::undefined()),
            },
            kind: Kind::Object,
            array_length: None,
            owned_roots: Vec::new(),
        })
    }

    fn lower_new_native_error(
        &mut self,
        construct: &ast::NewExpr,
        alias: Option<scope::ConstructorAlias>,
    ) -> Result<Lowered, ParseError> {
        let span = construct.span;
        let refused = || self.error(span, "unsupported JavaScript ES5 P0 primitive expression");
        let ast::Expr::Ident(callee) = Self::unwrap_parens(&construct.callee) else {
            return Err(refused());
        };
        let forced = matches!(
            alias,
            Some(scope::ConstructorAlias::TypeError | scope::ConstructorAlias::RangeError)
        );
        let hit = reference::resolve(self, callee.sym.as_str());
        if forced {
            if !construct.args.as_ref().is_some_and(Vec::is_empty) {
                return Err(refused());
            }
        } else if let reference::NameHit::BuiltinOptionalError(_) = hit {
            reference::admit_optional_error_args(self, construct.args.as_deref().unwrap_or_default(), span)?;
            if self.in_dead_capability() {
                return Ok(Lowered { kind: Kind::Object, ..Self::capability_only_read() });
            }
        } else if !construct.args.as_ref().is_some_and(Vec::is_empty) {
            return Err(refused());
        }
        let (private, takes_rows) = match (forced, alias) {
            (true, Some(scope::ConstructorAlias::TypeError)) => (self.intrinsics.map(|i| i.type_error), false),
            (true, Some(scope::ConstructorAlias::RangeError)) => (self.intrinsics.and_then(|i| i.range_error), true),
            _ => match hit {
                reference::NameHit::BuiltinTypeError => (self.intrinsics.map(|i| i.type_error), false),
                reference::NameHit::BuiltinRangeError => (self.intrinsics.and_then(|i| i.range_error), true),
                reference::NameHit::BuiltinOptionalError(error) => (self.intrinsics.and_then(|i| error.allocator(i)), true),
                _ => return Err(refused()),
            },
        };
        let private = private.ok_or_else(|| self.error(span, &format!("{} requires an ordinary realm", callee.sym)))?;
        let carrier = self.active_carrier(span)?;
        let (slot, mut stmts) = self.bind_temp(value::undefined(), span)?;
        let mut allocate = vec![Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: slot,
            expr: Self::guest_call(
                private,
                if takes_rows {
                    vec![Expr::Var(carrier), Expr::Int(0)]
                } else {
                    vec![Expr::Var(carrier)]
                },
            ),
            line: self.line(span),
        }];
        self.refresh_root(slot, span, &mut allocate)?;
        if let Some(normal) = self.pending_normal {
            stmts.push(Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: allocate,
                else_branch: Vec::new(),
                line: self.line(span),
            });
        } else {
            stmts.extend(allocate);
        }
        Ok(Lowered {
            expr: Expr::Block { stmts, expr: Box::new(Expr::Var(slot)) },
            kind: Kind::Object,
            array_length: None,
            owned_roots: vec![slot],
        })
    }

    /// The one admitted empty-object construction, `new Object()`.
    fn lower_new_object(&mut self, construct: &ast::NewExpr) -> Result<Lowered, ParseError> {
        let refused = || {
            self.error(
                construct.span,
                "unsupported JavaScript ES5 P0 primitive expression",
            )
        };
        let ast::Expr::Ident(callee) = Self::unwrap_parens(&construct.callee) else {
            return Err(refused());
        };
        if callee.sym != *"Object" {
            return Err(refused());
        }
        let Some(args) = construct.args.as_ref() else {
            return Err(refused());
        };
        if !args.is_empty() {
            return Err(refused());
        }
        // Reuse the existing empty plain-object allocator: an empty literal
        // lowers through the ordinary object reservation/publication path.
        let empty = ast::ObjectLit {
            span: construct.span,
            props: Vec::new(),
        };
        self.lower_object_literal(&empty)
    }

    /// The one admitted construction, `new Error("boom")`.
    ///
    /// `Error` is not a global binding: the profile layer proved the exact
    /// callee, arity and message before this site is reached, so no source
    /// constructor runs and no argument is evaluated here. The value is a
    /// rooted managed Error object: kind Object with the error flag set, the
    /// existing realm Error prototype as its proto pair, an internal message
    /// pair, and one ordinary
    /// checked `message` data row holding the String `boom`. Nothing is
    /// rooted through the carrier's empty Core array.
    fn lower_new_error(&mut self, construct: &ast::NewExpr) -> Result<Lowered, ParseError> {
        let span = construct.span;
        let refused = || {
            self.error(
                span,
                "unsupported JavaScript ES5 P0 primitive expression",
            )
        };
        let ast::Expr::Ident(callee) = Self::unwrap_parens(&construct.callee) else {
            return Err(refused());
        };
        if callee.sym != *"Error"
            || !matches!(
                reference::resolve(self, callee.sym.as_str()),
                reference::NameHit::Missing
            )
        {
            return Err(refused());
        }
        let Some(args) = construct.args.as_ref() else {
            return Err(refused());
        };
        let [argument] = args.as_slice() else {
            return Err(refused());
        };
        if argument.spread.is_some() {
            return Err(refused());
        }
        let ast::Expr::Lit(ast::Lit::Str(literal)) = Self::unwrap_parens(&argument.expr) else {
            return Err(refused());
        };
        let units = self.decode_string_token(literal.span)?;
        if units != [98, 111, 111, 109] {
            return Err(refused());
        }
        let intrinsics = self.intrinsics.ok_or_else(|| {
            self.error(
                span,
                "the admitted Error construction requires the object runtime",
            )
        })?;
        let helper_int = self.helper_int_identity.ok_or_else(|| {
            self.error(span, "private Int identity helper missing")
        })?;
        let carrier = self.active_carrier(span)?;
        let line = self.line(span);
        let guarded = self.pending_normal;
        // All three reservations below (the `message` key string node, the
        // `boom` value string node and the Error object itself) are latent
        // allocations. After an abrupt completion the guard must suppress
        // every one of them, so they accumulate into this single region,
        // which is emitted either conditionally on `pending_normal` or not
        // at all when this lowering has no pending-guard slot.
        let mut alloc_stmts: Vec<Stmt> = Vec::new();
        let mut owned_roots = Vec::new();
        // The key is a real UTF-16 guest string compared by content at
        // runtime, exactly like a literal key. It publishes into its own
        // entry before the containing allocation.
        let key_units: Vec<u16> = "message".encode_utf16().collect();
        let key_box = self.emit_utf16_node(&key_units, span, &mut alloc_stmts)?;
        owned_roots.push(key_box);
        // The message keeps its own node: the row below stores this checked
        // String by payload identity, never a folded host primitive.
        let value_box = self.emit_utf16_node(&units, literal.span, &mut alloc_stmts)?;
        if self.has_root(value_box) {
            owned_roots.push(value_box);
        }
        let newborn = self.begin_node(
            carrier,
            layout::ERROR_HEADER_CELLS + layout::OBJECT_CELLS_PER_PROPERTY,
            layout::KIND_OBJECT,
            span,
            line,
            &mut alloc_stmts,
        )?;
        // Header: one own property, extensible, error flag set. The proto
        // pair reuses the existing realm Error prototype root (entry 2).
        self.emit_cell_write(
            carrier,
            Expr::Add(
                Box::new(Expr::Var(newborn.base)),
                Box::new(Expr::Int(2)),
            ),
            Self::as_number(Expr::Int(
                layout::OBJECT_COUNT_FACTOR
                    + layout::OBJECT_EXTENSIBLE_FLAG
                    + layout::OBJECT_LEGACY_ERROR_FLAG
                    + layout::OBJECT_ERROR_INSTANCE_FLAG,
            )),
            span,
            line,
            &mut alloc_stmts,
        )?;
        let prototype =
            Self::guest_call(intrinsics.root, vec![Expr::Var(carrier), Expr::Int(2)]);
        for (offset, name) in [(3, "ref_slot"), (4, "generation")] {
            self.emit_cell_write(
                carrier,
                Expr::Add(
                    Box::new(Expr::Var(newborn.base)),
                    Box::new(Expr::Int(offset)),
                ),
                Self::as_number(value::field_of(prototype.clone(), name)),
                span,
                line,
                &mut alloc_stmts,
            )?;
        }
        // One ordinary checked data row: the `message` key with the `boom`
        // String payload, writable and configurable like the realm Error
        // prototype's own `message` row.
        for (offset, name) in [(0, "ref_slot"), (1, "generation")] {
            self.emit_cell_write(
                carrier,
                Expr::Add(
                    Box::new(Expr::Var(newborn.base)),
                    Box::new(Expr::Int(layout::ERROR_MESSAGE_OFFSET + offset)),
                ),
                Self::as_number(value::field(value_box, name)),
                span,
                line,
                &mut alloc_stmts,
            )?;
        }
        let row = layout::ERROR_HEADER_CELLS;
        let mut cells = vec![
            (0, Self::as_number(value::field(key_box, "ref_slot"))),
            (1, Self::as_number(value::field(key_box, "generation"))),
        ];
        let (flag, stmt) = self.bind_typed(value::field(value_box, "flag"), TypeSchema::Bool, span)?;
        alloc_stmts.push(stmt);
        let (yes, stmt) = self.bind_temp_bare(value::boolean(Expr::Bool(true)), span)?;
        alloc_stmts.push(stmt);
        let flag = Expr::IfElse {
            condition: Box::new(Expr::Eq(
                Box::new(Expr::Var(flag)),
                Box::new(value::field(yes, "flag")),
            )),
            then_expr: Box::new(Expr::Float(1.0)),
            else_expr: Box::new(Expr::Float(0.0)),
        };
        cells.extend([
            (
                2,
                Self::as_number(super::prelude::object::pack_data(
                    Self::guest_call(helper_int, vec![value::field(value_box, "tag")]),
                    Expr::Bool(true),
                    Expr::Bool(false),
                    Expr::Bool(true),
                )),
            ),
            (3, value::field(value_box, "number")),
            (4, flag),
            (5, Self::as_number(value::field(value_box, "ref_slot"))),
            (6, Self::as_number(value::field(value_box, "generation"))),
        ]);
        for (offset, payload) in cells {
            self.emit_cell_write(
                carrier,
                Expr::Add(
                    Box::new(Expr::Var(newborn.base)),
                    Box::new(Expr::Int(row + offset)),
                ),
                payload,
                span,
                line,
                &mut alloc_stmts,
            )?;
        }
        // Declare the escaping destination outside the conditional region so
        // an abrupt completion leaves a real typed Undefined placeholder
        // instead of committing this object.
        let dest = self.slot(span)?;
        self.publish_into(dest, carrier, &newborn, span, line, &mut alloc_stmts)?;
        let mut outer: Vec<Stmt> = vec![Stmt::Let {
            index: dest,
            declared_schema: Some(value::schema()),
            expr: value::undefined(),
            line,
        }];
        if let Some(normal) = guarded {
            outer.push(Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: alloc_stmts,
                else_branch: Vec::new(),
                line,
            });
        } else {
            outer.extend(alloc_stmts);
        }
        owned_roots.push(dest);
        Ok(Lowered {
            expr: Expr::Block {
                stmts: outer,
                expr: Box::new(Expr::Var(dest)),
            },
            kind: Kind::Object,
            array_length: None,
            owned_roots,
        })
    }

    /// The one admitted construction, `new Date(0)`.
    ///
    /// `Date` is not a global binding: the profile layer proved the exact
    /// callee, arity and argument before this site is reached, so no name is
    /// looked up, no source constructor runs and no argument is evaluated
    /// here. The value is a real ordinary object built by the object runtime
    /// whose own `getTime` row the unchanged ordinary Get later finds. Its
    /// epoch millisecond value is the Number 0; no host clock is read.
    fn lower_new_date(&mut self, construct: &ast::NewExpr) -> Result<Lowered, ParseError> {
        let span = construct.span;
        let helper = self
            .objects
            .ok_or_else(|| {
                self.error(
                    span,
                    "the admitted Date construction requires the object runtime",
                )
            })?
            .date_epoch;
        let carrier = self.active_carrier(span)?;
        let (slot, stmts) = self.bind_temp(
            Self::guest_call(helper, vec![Expr::Var(carrier)]),
            span,
        )?;
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(Expr::Var(slot)),
            },
            kind: Kind::Object,
            array_length: None,
            owned_roots: vec![slot],
        })
    }

    /// The admitted `new F(...)` over a source function.
    ///
    /// Evaluation order is fixed by the spec: callee first, every actual left
    /// to right second, and only then `Get(F, "prototype")`. The instance
    /// allocation, the call with the instance as receiver and the
    /// returned-object-or-instance choice live in one private helper, so the
    /// guest throw path stays the unchanged dispatch Result.
    fn lower_source_construct(&mut self, construct: &ast::NewExpr) -> Result<Lowered, ParseError> {
        let span = construct.span;
        let helper = self.helper_construct.ok_or_else(|| {
            self.error(span, "new requires the private construct helper")
        })?;
        let carrier = self.active_carrier(span)?;
        let mut stmts = Vec::new();
        let mut owned = Vec::new();
        let callee = self.expr(&construct.callee)?;
        reference::admit_callable_kind(self, construct.callee.span(), callee.kind)?;
        let (func, mut saved) = self.bind_temp(callee.expr, span)?;
        stmts.append(&mut saved);
        owned.extend(callee.owned_roots);
        owned.push(func);
        // Every later stage (each actual, the vector, `Get(F, "prototype")`,
        // the helper call and the result consumption) runs only while this
        // activation's completion is still Normal: the first actual that
        // throws suppresses all of them, so no later actual is evaluated and
        // no instance is allocated for a construction that never started.
        let normal = self.pending_normal;
        let mut actual_groups = Vec::new();
        let mut actuals = Vec::new();
        for arg in construct.args.iter().flatten() {
            let actual = self.expr(&arg.expr)?;
            owned.extend(actual.owned_roots);
            let (slot, saved) = self.bind_temp(actual.expr, arg.span())?;
            owned.push(slot);
            actuals.push(slot);
            actual_groups.push((arg.span(), saved));
        }
        let mut tail = Vec::new();
        // The construction result escapes the guarded region, so its
        // destination is a typed Undefined placeholder declared before the
        // first `if normal`: an aborted construction still leaves a real local
        // every later read can consume.
        let (value, value_stmt) = self.bind_temp_bare(value::undefined(), span)?;
        stmts.push(value_stmt);
        // The pre-`Get` callee gate: callee, every actual, then reject a
        // non-source function, and only then `Get(F, "prototype")`. The gate's
        // guest TypeError is the same answer the construct helper gives, but
        // it runs first so a non-function callee's own `prototype` getter is
        // never consulted and nothing is allocated for a construction that
        // cannot start. An abrupt actual still wins: the gate sits behind the
        // same `if normal` as the rest of the tail.
        let check = self.helper_construct_check.ok_or_else(|| {
            self.error(span, "new requires the private construct check helper")
        })?;
        let _ = self.consume_value_result(
            Self::guest_call(check, vec![Expr::Var(carrier), Expr::Var(func)]),
            span,
            &mut tail,
        )?;
        let mut work = Vec::new();
        let (vector, saved) = self.bind_typed(
            Self::builtin_call(BuiltinFunction::ArrayNew.call_index(), Vec::new()),
            TypeSchema::Array(Box::new(value::schema())),
            span,
        )?;
        work.push(saved);
        for actual in actuals {
            work.push(Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: vector,
                expr: Self::builtin_call(
                    BuiltinFunction::ArrayPush.call_index(),
                    vec![Expr::Var(vector), Expr::Var(actual)],
                ),
                line: self.line(span),
            });
        }
        let proto = self.construct_prototype_get(func, span, &mut work, &mut owned)?;
        // `Get(F, "prototype")` is itself a guest-result step: if it raises,
        // the helper must not run and no instance may be allocated for the
        // aborted construction. The post-Get half is one more `if normal`
        // region, exactly like the actuals before it.
        let mut post = Vec::new();
        let got = self.consume_value_result(
            Self::guest_call(
                helper,
                vec![
                    Expr::Var(carrier),
                    Expr::Var(func),
                    Expr::Var(vector),
                    Expr::Var(proto),
                ],
            ),
            span,
            &mut post,
        )?;
        post.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: value,
            expr: Expr::Var(got),
            line: self.line(span),
        });
        self.refresh_root(value, span, &mut post)?;
        match normal {
            Some(normal) => {
                // Nested exactly like the ordinary Call actuals: the first
                // actual is unconditional, each later one and the whole tail
                // sit behind the previous step's `if normal`. The tail itself
                // gets one outer entry guard, so a zero-actual construction
                // that already threw in its callee never reaches the gate, the
                // vector, the prototype Get or the helper. The gate's own
                // error withholds the vector/Get half through a second guard.
                work.push(Stmt::IfElse {
                    condition: Expr::Var(normal),
                    then_branch: post,
                    else_branch: Vec::new(),
                    line: self.line(span),
                });
                tail.push(Stmt::IfElse {
                    condition: Expr::Var(normal),
                    then_branch: work,
                    else_branch: Vec::new(),
                    line: self.line(span),
                });
                let mut nested = tail;
                for (arg_span, mut step) in actual_groups.into_iter().rev() {
                    step.push(Stmt::IfElse {
                        condition: Expr::Var(normal),
                        then_branch: nested,
                        else_branch: Vec::new(),
                        line: self.line(arg_span),
                    });
                    nested = step;
                }
                stmts.push(Stmt::IfElse {
                    condition: Expr::Var(normal),
                    then_branch: nested,
                    else_branch: Vec::new(),
                    line: self.line(span),
                });
            }
            None => {
                for (_, saved) in actual_groups {
                    stmts.extend(saved);
                }
                stmts.extend(tail);
                stmts.extend(work);
                stmts.extend(post);
            }
        }
        // Input roots are released at the common exit, exactly like an
        // ordinary Call: an aborted actual or prototype Get must not skip
        // the cleanup for the already evaluated callee/actuals.
        reference::release_consumed_roots(self, &owned, value, span, &mut stmts)?;
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(Expr::Var(value)),
            },
            kind: Kind::Object,
            array_length: None,
            owned_roots: vec![value],
        })
    }

    /// `Get(F, "prototype")` through a real Reference built from the already
    /// evaluated callee. No AST member is synthesized: the callee is
    /// evaluated exactly once and only its saved value is read.
    fn construct_prototype_get(
        &mut self,
        func: u16,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
        owned: &mut Vec<u16>,
    ) -> Result<u16, ParseError> {
        let ref_get = self.helper_ref_get.ok_or_else(|| {
            self.error(span, "new requires the realm reference helper")
        })?;
        let carrier = self.active_carrier(span)?;
        let key_box =
            self.emit_utf16_node(&Self::ident_key_units("prototype"), span, stmts)?;
        owned.push(key_box);
        let reference = value::reference(
            Expr::Var(func),
            value::field(key_box, "ref_slot"),
            value::field(key_box, "generation"),
            Expr::Bool(self.enclosing_strict),
        );
        let (reference, saved) =
            self.bind_typed(reference, value::reference_schema(), span)?;
        stmts.push(saved);
        let line = self.line(span);
        self.reaffirm_root(carrier, key_box, span, line, stmts)?;
        let (value, saved) = self.bind_temp_bare(value::undefined(), span)?;
        stmts.push(saved);
        let got = self.consume_value_result(
            Self::guest_call(ref_get, vec![Expr::Var(carrier), Expr::Var(reference)]),
            span,
            stmts,
        )?;
        stmts.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: value,
            expr: Expr::Var(got),
            line,
        });
        self.refresh_root(value, span, stmts)?;
        owned.push(value);
        Ok(value)
    }

    fn lower_object_literal(&mut self, literal: &ast::ObjectLit) -> Result<Lowered, ParseError> {
        let entries = self.object_literal_entries(literal)?;
        let count = entries
            .iter()
            .map(|(key, _, _, _)| key)
            .collect::<std::collections::HashSet<_>>()
            .len() as i64;
        let line = self.line(literal.span);
        let mut stmts: Vec<Stmt> = Vec::new();
        // Allocate key string nodes first so every property key is a real
        // UTF-16 guest string compared by content at runtime. Each key
        // publishes into its own entry before the containing allocation.
        let mut owned_roots = Vec::new();
        let mut key_boxes: Vec<u16> = Vec::new();
        let mut shared_keys = HashMap::new();
        for (units, _, key_span, _) in &entries {
            if let Some(key) = shared_keys.get(units) {
                key_boxes.push(*key);
                continue;
            }
            // Keys borrow the literal span for resource diagnostics.
            let boxed = self.emit_utf16_node(units, *key_span, &mut stmts)?;
            key_boxes.push(boxed);
            shared_keys.insert(units.clone(), boxed);
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
        let mut value_groups: Vec<Vec<Stmt>> = Vec::new();
        for (_, value_expr, value_span, _) in &entries {
            let value = self.expr(value_expr)?;
            reference::admit_object_value(self, *value_span, value.kind)?;
            let (slot, stmt) = self.bind_evaluation(value.expr, value.kind, *value_span)?;
            owned_roots.extend(value.owned_roots);
            if self.has_root(slot) {
                owned_roots.push(slot);
            }
            value_groups.push(stmt);
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
        // Within an activation that tracks a pending Completion, the
        // containing reservation and every later property value run only
        // while the completion is still Normal. The destination is a real
        // typed Undefined placeholder declared before the region, so an
        // abrupt completion cannot allocate this object or commit its cells.
        let guarded = self.pending_normal;
        let mut alloc_stmts: Vec<Stmt> = Vec::new();
        let newborn = self.begin_node(
            carrier,
            data_count,
            layout::KIND_OBJECT,
            literal.span,
            line,
            &mut alloc_stmts,
        )?;
        // Header remainder: property count plus the null prototype pair.
        // Kind/generation are stamped by reservation.
        for (offset, marker) in [
            (2i64, layout::OBJECT_COUNT_FACTOR * count + layout::OBJECT_EXTENSIBLE_FLAG),
            (3i64, 0),
            (4i64, 0),
        ] {
            self.emit_cell_write(
                carrier,
                Expr::Add(
                    Box::new(Expr::Var(newborn.base)),
                    Box::new(Expr::Int(offset)),
                ),
                Self::as_number(Expr::Int(marker)),
                literal.span,
                line,
                &mut alloc_stmts,
            )?;
        }
        if let Some(intrinsics) = self.intrinsics {
            let prototype =
                Self::guest_call(intrinsics.root, vec![Expr::Var(carrier), Expr::Int(0)]);
            for (offset, name) in [(3, "ref_slot"), (4, "generation")] {
                self.emit_cell_write(
                    carrier,
                    Expr::Add(
                        Box::new(Expr::Var(newborn.base)),
                        Box::new(Expr::Int(offset)),
                    ),
                    Self::as_number(value::field_of(prototype.clone(), name)),
                    literal.span,
                    line,
                    &mut alloc_stmts,
                )?;
            }
        }
        struct LiteralRow {
            key: u16,
            data: Option<u16>,
            get: Option<u16>,
            set: Option<u16>,
        }
        let mut rows: Vec<LiteralRow> = Vec::new();
        let mut positions = HashMap::new();
        for ((key, _, _, role), (key_box, (payload, _))) in
            entries.iter().zip(key_boxes.iter().zip(value_slots.iter()))
        {
            let position = *positions.entry(key.clone()).or_insert_with(|| {
                let i = rows.len();
                rows.push(LiteralRow {
                    key: *key_box,
                    data: None,
                    get: None,
                    set: None,
                });
                i
            });
            let row = &mut rows[position];
            match role {
                LiteralRole::Data => row.data = Some(*payload),
                LiteralRole::Getter => row.get = Some(*payload),
                LiteralRole::Setter => row.set = Some(*payload),
            }
        }
        for (
            index,
            LiteralRow {
                key,
                data,
                get,
                set,
            },
        ) in rows.into_iter().enumerate()
        {
            let row =
                layout::OBJECT_HEADER_CELLS + layout::OBJECT_CELLS_PER_PROPERTY * index as i64;
            let mut cells = vec![
                (0, Self::as_number(value::field(key, "ref_slot"))),
                (1, Self::as_number(value::field(key, "generation"))),
            ];
            if let Some(payload) = data {
                let (flag, stmt) = self.bind_typed(
                    value::field(payload, "flag"),
                    TypeSchema::Bool,
                    literal.span,
                )?;
                alloc_stmts.push(stmt);
                let (yes, stmt) =
                    self.bind_temp_bare(value::boolean(Expr::Bool(true)), literal.span)?;
                alloc_stmts.push(stmt);
                let flag = Expr::IfElse {
                    condition: Box::new(Expr::Eq(
                        Box::new(Expr::Var(flag)),
                        Box::new(value::field(yes, "flag")),
                    )),
                    then_expr: Box::new(Expr::Float(1.0)),
                    else_expr: Box::new(Expr::Float(0.0)),
                };
                cells.extend([
                    (
                        2,
                        Self::as_number(super::prelude::object::pack_data(
                            // `payload` is the evaluated/rooted private
                            // JsValue box, whose constructors store Int tags
                            // 0..=5. Restore that Int before pack_data's
                            // power-of-two multiply, not after the wire Number
                            // projection. Flags and raw writes stay unchanged.
                            Self::guest_call(
                                self.helper_int_identity.ok_or_else(|| {
                                    self.error(literal.span, "private Int identity helper missing")
                                })?,
                                vec![value::field(payload, "tag")],
                            ),
                            Expr::Bool(true),
                            Expr::Bool(true),
                            Expr::Bool(true),
                        )),
                    ),
                    (3, value::field(payload, "number")),
                    (4, flag),
                    (5, Self::as_number(value::field(payload, "ref_slot"))),
                    (6, Self::as_number(value::field(payload, "generation"))),
                ]);
            } else {
                let identity = |payload: Option<u16>, name: &str| {
                    payload.map_or(Expr::Float(0.0), |slot| {
                        Self::as_number(value::field(slot, name))
                    })
                };
                cells.extend([
                    (2, Expr::Float(14.0)),
                    (3, identity(get, "ref_slot")),
                    (4, identity(get, "generation")),
                    (5, identity(set, "ref_slot")),
                    (6, identity(set, "generation")),
                ]);
            }
            for (offset, payload) in cells {
                self.emit_cell_write(
                    carrier,
                    Expr::Add(
                        Box::new(Expr::Var(newborn.base)),
                        Box::new(Expr::Int(row + offset)),
                    ),
                    payload,
                    literal.span,
                    line,
                    &mut alloc_stmts,
                )?;
            }
        }
        // Declare the escaping destination outside the conditional region so
        // an abrupt completion leaves a real typed Undefined placeholder
        // instead of committing this object. Publication targets the existing
        // destination entry, so the activation keeps exactly one root entry
        // for this literal, as before.
        let dest = self.slot(literal.span)?;
        let mut outer = stmts;
        outer.push(Stmt::Let {
            index: dest,
            declared_schema: Some(value::schema()),
            expr: value::undefined(),
            line,
        });
        self.publish_into(
            dest,
            carrier,
            &newborn,
            literal.span,
            line,
            &mut alloc_stmts,
        )?;
        // Nest each property-value group and then the allocation/publish block
        // behind a Normal recheck, so a value that threw stops every later
        // sibling evaluation and the containing reservation itself.
        let region = if let Some(normal) = guarded {
            let mut nested = alloc_stmts;
            for group in value_groups.into_iter().rev() {
                let mut step = group;
                step.push(Stmt::IfElse {
                    condition: Expr::Var(normal),
                    then_branch: nested,
                    else_branch: Vec::new(),
                    line,
                });
                nested = step;
            }
            nested
        } else {
            let mut flat: Vec<Stmt> = value_groups.into_iter().flatten().collect();
            flat.extend(alloc_stmts);
            flat
        };
        if let Some(normal) = guarded {
            outer.push(Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: region,
                else_branch: Vec::new(),
                line,
            });
        } else {
            outer.extend(region);
        }
        owned_roots.push(dest);
        Ok(Lowered {
            expr: Expr::Block {
                stmts: outer,
                expr: Box::new(Expr::Var(dest)),
            },
            kind: Kind::Object,
            array_length: None,
            owned_roots,
        })
    }

    fn initialize_global_environment(
        &mut self,
        span: swc_common::Span,
        out: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        let carrier = self.active_carrier(span)?;
        let frame = self.active_frame(span)?;
        let helpers = self.heap_ref(span)?.helpers;
        let global = self.global_helpers.expect("shared helpers");
        let env = self.slot(span)?;
        let entry = self.entry_for(env, span)?;
        let line = self.line(span);
        let n = self.globals.count();
        let born = self.begin_node(
            carrier,
            layout::ENV_HEADER_CELLS + layout::ENV_CELLS_PER_ENTRY * n,
            layout::KIND_ENVIRONMENT,
            span,
            line,
            out,
        )?;
        for (offset, payload) in [(2, Expr::Int(n)), (3, Expr::Int(0)), (4, Expr::Int(0))] {
            self.emit_cell_write(
                carrier,
                Expr::Add(Box::new(Expr::Var(born.base)), Box::new(Expr::Int(offset))),
                Self::as_number(payload),
                span,
                line,
                out,
            )?;
        }
        // Raw extents may be reused: every counted entry is closed before publication.
        for position in 0..n {
            for offset in 0..5 {
                self.emit_cell_write(
                    carrier,
                    Expr::Add(
                        Box::new(Expr::Var(born.base)),
                        Box::new(Expr::Int(layout::ENV_HEADER_CELLS + position * 5 + offset)),
                    ),
                    Expr::Float(0.0),
                    span,
                    line,
                    out,
                )?;
            }
        }
        for i in 0..super::global_binding::NAMES.len() as u8 {
            if self.globals.contains(i) && self.globals.initial & (1 << i) != 0 {
                let (original, stmt) = self.bind_typed(
                    reference::original_value(self, i, span)?,
                    value::schema(),
                    span,
                )?;
                out.push(stmt);
                for (offset, payload) in [
                    Self::as_number(value::field(original, "tag")),
                    value::field(original, "number"),
                    Expr::IfElse {
                        condition: Box::new(value::field(original, "flag")),
                        then_expr: Box::new(Expr::Float(1.0)),
                        else_expr: Box::new(Expr::Float(0.0)),
                    },
                    Self::as_number(value::field(original, "ref_slot")),
                    Self::as_number(value::field(original, "generation")),
                ]
                .into_iter()
                .enumerate()
                {
                    self.emit_cell_write(
                        carrier,
                        Expr::Add(
                            Box::new(Expr::Var(born.base)),
                            Box::new(Expr::Int(
                                layout::ENV_HEADER_CELLS
                                    + self.globals.entry(i) * 5
                                    + offset as i64,
                            )),
                        ),
                        payload,
                        span,
                        line,
                        out,
                    )?;
                }
            }
        }
        let ack = self.slot(span)?;
        out.push(Stmt::Let {
            index: ack,
            declared_schema: Some(TypeSchema::Int),
            expr: Self::guest_call(
                helpers.node_publish_edge,
                vec![
                    Expr::Var(carrier),
                    Expr::Var(born.slot),
                    Expr::Var(born.generation),
                    Expr::Var(born.base),
                    Expr::Var(frame),
                    Expr::Int(i64::from(entry)),
                    Expr::Int(layout::ROOT_INTERNAL),
                ],
            ),
            line,
        });
        out.push(Stmt::Let {
            index: env,
            declared_schema: Some(TypeSchema::Int),
            expr: Self::guest_call(
                global.pack,
                vec![
                    Expr::Var(carrier),
                    Expr::Var(born.slot),
                    Expr::Var(born.generation),
                ],
            ),
            line,
        });
        self.global_env = Some(env);
        Ok(())
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
        declaration_name: Option<&str>,
    ) -> Result<Lowered, ParseError> {
        let span = function.function.span;
        let key = (span.lo.0, span.hi.0);
        let Some(&code_id) = self.function_code_ids.get(&key) else {
            return Err(self.error(span, "P0 source-function admission requires predeclaration"));
        };
        let strict = self
            .graph
            .function_scopes
            .get(&key)
            .map(|scope| self.graph.scope(*scope).strict)
            .ok_or_else(|| self.error(span, "P0 source-function scope requires predeclaration"))?;
        let carrier = self.active_carrier(span)?;
        let line = self.line(span);
        let mut stmts = Vec::new();
        // `Function.prototype.name`/`toString` make every function node carry
        // a second own row. The name String is published before the function
        // node reserves, so a collection during either reservation can never
        // observe an unpublished node. Anonymous functions read the shared
        // empty String from its private permanent root instead of allocating
        // a payload of their own.
        let named = self.function_name_used.get() || self.function_to_string_used.get();
        let name_value = if named {
            match declaration_name {
                Some(name) => {
                    let units: Vec<u16> = name.encode_utf16().collect();
                    Some(self.emit_utf16_node(&units, span, &mut stmts)?)
                }
                None => {
                    let intrinsics = self
                        .intrinsics
                        .ok_or_else(|| self.error(span, "function intrinsic realm missing"))?;
                    let (empty, saved) = self.bind_typed(
                        Self::guest_call(
                            intrinsics
                                .function_name_empty
                                .ok_or_else(|| self.error(span, "function name empty helper missing"))?,
                            vec![Expr::Var(carrier)],
                        ),
                        value::schema(),
                        span,
                    )?;
                    stmts.push(saved);
                    Some(empty)
                }
            }
        } else {
            None
        };
        let own_rows = if named { 2 } else { 1 };
        let newborn = self.begin_node(
            carrier,
            layout::FUNCTION_CELLS + own_rows * layout::OBJECT_CELLS_PER_PROPERTY,
            layout::KIND_FUNCTION,
            span,
            line,
            &mut stmts,
        )?;
        // Header remainder: own properties (`length`, plus the gated `name`),
        // extensible, null prototype pair, static code identity, empty
        // environment pair, strict marker. Kind/generation are stamped by
        // reservation.
        let packed_header =
            own_rows * layout::OBJECT_COUNT_FACTOR + layout::OBJECT_EXTENSIBLE_FLAG;
        for (offset, marker) in [
            (2i64, packed_header),
            (3i64, 0),
            (4i64, 0),
            (5i64, code_id),
            (6i64, 0),
            (7i64, 0),
            (8i64, i64::from(strict)),
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
                Self::as_number(if self.globals.shared && (offset == 6 || offset == 7) {
                    let helpers = self.global_helpers.expect("shared helpers");
                    Self::guest_call(
                        if offset == 6 {
                            helpers.slot
                        } else {
                            helpers.generation
                        },
                        vec![Expr::Var(self.global_env.ok_or_else(|| {
                            self.error(span, "function global environment missing")
                        })?)],
                    )
                } else {
                    Expr::Int(marker)
                }),
                span,
                line,
                &mut stmts,
            )?;
        }
        let intrinsics = self
            .intrinsics
            .ok_or_else(|| self.error(span, "function intrinsic realm missing"))?;
        let (prototype, saved) = self.bind_typed(
            Self::guest_call(intrinsics.root, vec![Expr::Var(carrier), Expr::Int(1)]),
            value::schema(),
            span,
        )?;
        stmts.push(saved);
        let (key, saved) = self.bind_typed(
            Self::guest_call(intrinsics.length_key, vec![Expr::Var(carrier)]),
            value::schema(),
            span,
        )?;
        stmts.push(saved);
        let mut row_cells = vec![
            (3, Self::as_number(value::field(prototype, "ref_slot"))),
            (4, Self::as_number(value::field(prototype, "generation"))),
            (10, Self::as_number(value::field(key, "ref_slot"))),
            (11, Self::as_number(value::field(key, "generation"))),
            (12, Expr::Float(48.0)),
            (13, Expr::Float(function.function.params.len() as f64)),
            (14, Expr::Float(0.0)),
            (15, Expr::Float(0.0)),
            (16, Expr::Float(0.0)),
        ];
        if let Some(name_value) = name_value {
            let name_key = intrinsics
                .function_name_key
                .ok_or_else(|| self.error(span, "function name key helper missing"))?;
            let (name_key, saved) = self.bind_typed(
                Self::guest_call(name_key, vec![Expr::Var(carrier)]),
                value::schema(),
                span,
            )?;
            stmts.push(saved);
            let row = layout::FUNCTION_CELLS + layout::OBJECT_CELLS_PER_PROPERTY;
            row_cells.extend([
                (row, Self::as_number(value::field(name_key, "ref_slot"))),
                (
                    row + 1,
                    Self::as_number(value::field(name_key, "generation")),
                ),
                (row + 2, Expr::Float(64.0)),
                (row + 3, Expr::Float(0.0)),
                (row + 4, Expr::Float(0.0)),
                (row + 5, Self::as_number(value::field(name_value, "ref_slot"))),
                (row + 6, Self::as_number(value::field(name_value, "generation"))),
            ]);
        }
        for (offset, payload) in row_cells {
            self.emit_cell_write(
                carrier,
                Expr::Add(
                    Box::new(Expr::Var(newborn.base)),
                    Box::new(Expr::Int(offset)),
                ),
                payload,
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
    let (mut ir, owned) = if !heap_needed {
        lower_primitive(script, source, source_id, base, context)?
    } else {
        lower_with_realm(script, source, source_id, base, context)?
    };
    undefined_box::factor(&mut ir);
    super::private_reachability::prune(&mut ir, &owned);
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
        ast::Stmt::Throw(_) | ast::Stmt::Try(_) => true,
        ast::Stmt::While(loop_) => expr_needs_heap(&loop_.test) || stmt_needs_heap(&loop_.body),
        ast::Stmt::DoWhile(loop_) => expr_needs_heap(&loop_.test) || stmt_needs_heap(&loop_.body),
        ast::Stmt::For(loop_) => {
            loop_.init.as_ref().is_some_and(|init| match init {
                ast::VarDeclOrExpr::VarDecl(declaration) => declaration
                    .decls
                    .iter()
                    .any(|d| d.init.as_deref().is_some_and(expr_needs_heap)),
                ast::VarDeclOrExpr::Expr(expression) => expr_needs_heap(expression),
            }) || loop_
                .test
                .as_ref()
                .is_some_and(|test| expr_needs_heap(test))
                || loop_
                    .update
                    .as_ref()
                    .is_some_and(|update| expr_needs_heap(update))
                || stmt_needs_heap(&loop_.body)
        }
        ast::Stmt::ForOf(_) => true,
        ast::Stmt::Labeled(labeled) => stmt_needs_heap(&labeled.body),
        ast::Stmt::Return(ret) => ret.arg.as_deref().is_some_and(expr_needs_heap),
        ast::Stmt::Switch(select) => {
            expr_needs_heap(&select.discriminant)
                || select.cases.iter().any(|case| {
                    case.test.as_ref().is_some_and(|test| expr_needs_heap(test))
                        || case.cons.iter().any(stmt_needs_heap)
                })
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
        ast::Expr::Update(update) => expr_needs_heap(&update.arg),
        ast::Expr::Bin(b) if bitwise::Operation::from_op(b.op).is_some() => {
            unary::operand_needs_heap(&b.left) || unary::operand_needs_heap(&b.right)
        }
        ast::Expr::Unary(u) if u.op == ast::UnaryOp::Tilde => unary::operand_needs_heap(&u.arg),
        ast::Expr::Bin(b) if b.op == ast::BinaryOp::Mod => {
            unary::operand_needs_heap(&b.left) || unary::operand_needs_heap(&b.right)
        }
        ast::Expr::Bin(b) => expr_needs_heap(&b.left) || expr_needs_heap(&b.right),
        ast::Expr::Unary(u) if matches!(u.op, ast::UnaryOp::Void | ast::UnaryOp::Plus) => {
            unary::operand_needs_heap(&u.arg)
        }
        ast::Expr::Unary(u) => u.op == ast::UnaryOp::TypeOf || expr_needs_heap(&u.arg),
        _ => false,
    }
}

fn lower_primitive(
    script: &ast::Script,
    source: &str,
    source_id: u32,
    base: u32,
    context: &SourceFrontendContext<'_>,
) -> Result<(FrontendIr, Vec<super::private_reachability::OwnedHelper>), ParseError> {
    let mut lower = Lowerer {
        globals: super::global_binding::Plan::default(),
        global_helpers: None,
        global_env: None,
        source,
        source_id,
        base,
        next_slot: 0,
        bindings: HashMap::new(),
        array_lengths: HashMap::new(),
        array_length_ids: HashMap::new(),
        array_names: HashSet::new(),
        source_bindings: Vec::new(),
        heap: None,
        intrinsics: None,
        objects: None,
        string_length_sites: std::cell::RefCell::new(std::collections::HashSet::new()),
        string_length_sealed: std::cell::Cell::new(false),
        helper_string_length: None,
        string_concat_sites: std::cell::RefCell::new(std::collections::HashMap::new()),
        string_concat_sealed: std::cell::Cell::new(false),
        helper_string_concat: None,
        helper_fixed_scalar_string: None,
        relational_sites: std::cell::RefCell::new(HashMap::new()),
        relational_sealed: std::cell::Cell::new(false),
        helper_string_compare: None,
        number_key_sites: std::cell::RefCell::new(std::collections::HashSet::new()),
        number_key_sealed: std::cell::Cell::new(false),
        number_string_entries: None,
        primitive_key: primitive_key::State::default(),
        init_cells: 0,
        primitive_number: primitive_number::State::default(),
        primitive_addition: primitive_addition::State::default(),
        mutation: primitive_assignment::State::default(),
        abstract_equality: abstract_equality::State::default(),
        math_used: std::cell::Cell::new(false),
        numeric_globals: std::cell::Cell::new(0),
        uri_used: std::cell::Cell::new(0),
        json_used: std::cell::Cell::new(false),
        number_namespace_used: std::cell::Cell::new(false),
        number_to_string_used: std::cell::Cell::new(false),
        function_name_used: std::cell::Cell::new(false),
        function_to_string_used: std::cell::Cell::new(false),
function_prototype_used: std::cell::Cell::new(false),
            primitive_constructor_read: std::cell::Cell::new(false),
construct_used: std::cell::Cell::new(false),
helper_construct: None,
            helper_construct_check: None,
        string_replace_all_used: std::cell::Cell::new(false),
        string_search_used: std::cell::Cell::new(false),
        string_match_used: std::cell::Cell::new(false),
        string_namespace_used: std::cell::Cell::new(false),
        array_namespace_used: std::cell::Cell::new(false),
        array_constructor_used: std::cell::Cell::new(false),
        arguments_array_from_values: None,
        arguments_string_key: None,
        arguments_sync: None,
        arguments_object: None,
        array_buffer_used: std::cell::Cell::new(false),
        typed_array_owned_quota_reachable: std::cell::Cell::new(false),
        typed_array_elements: std::cell::Cell::new(0),
        boolean_used: std::cell::Cell::new(false),
        range_error_used: std::cell::Cell::new(false),
        optional_errors_used: std::cell::Cell::new(0),
        utf16_bridge: None,
        uri_call: None,
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
        helper_int_identity: None,
        function_code_ids: HashMap::new(),
        collected_functions: Vec::new(),
        graph: scope::build(script, source, source_id, base)?,
        active_scope: scope::ScopeId(0),
        iteration: iteration::IterationState::default(),
        next_target: 1,
        catch_overlay: std::cell::Cell::new(None),
        needs_ordinary_realm: std::cell::Cell::new(false),
        pending_normal: None,
        pending_completion: None,
        pending_payload: None,
        uncaught_host: None,
        binding_slots: HashMap::new(),
        binding_kinds: HashMap::new(),
        type_names: None,
        utf16_inits: utf16_init::Utf16InitPool::default(),
    };
    lower.globals = super::global_binding::prepare(&mut lower.graph, script);
    lower.active_scope = lower.graph.script;
    // Resolve names through the ordinary binding graph before choosing the
    // heap-free route. Bare intrinsic values are real objects; a shadowed
    // spelling must retain the original scalar bootstrap/resource behavior.
    // The returned aggregate carries the independent producers *and* any
    // surviving primitive-key prepared proof, so the route choice has exactly
    // one complete consumer and never reads the cell behind its back.
    let need_ref = reference::script_needs_ordinary_realm(&lower, &script.body)?;
    if need_ref
        || lower.math_used.get()
        || lower.numeric_globals.get() != 0
        || !lower.string_concat_sites.borrow().is_empty()
        || lower.has_string_relational_sites()
        || lower.has_primitive_number_sites()
        || lower.addition_needs_strings()
    {
        return lower_with_realm(script, source, source_id, base, context);
    }
    lower.relational_sealed.set(true);
    lower.seal_primitive_number_sites();
    lower.seal_primitive_addition_sites();
    lower.seal_mutation_sites();
    lower.seal_abstract_equality_sites();
    let mut statements = Vec::new();
    reference::lower_script_statements(&mut lower, &script.body, &mut statements)?;
    let mut ir = LocalIrBuilder::new().finish(statements);
    ir.locals = usize::from(lower.next_slot);
    ir.local_bindings = lower.source_bindings;
    let declaration = value::declaration();
    ir.struct_schemas
        .insert(declaration.name.clone(), declaration);
    // Enhanced statement/value flow declares Completion-typed locals even on
    // the heap-free scalar path; the nominal schema must be present exactly
    // as on the realm path. Plain accepted primitive sources keep their
    // one-declaration IR unchanged.
    if lower.iteration.mode {
        let completion = value::completion_declaration();
        ir.struct_schemas
            .insert(completion.name.clone(), completion);
    }
    Ok((ir, Vec::new()))
}

fn install_scalar_error_host(
    context: &SourceFrontendContext<'_>,
    ir: &mut FrontendIr,
    lower: &Lowerer<'_>,
    span: swc_common::Span,
    use_site: &str,
) -> Result<u16, ParseError> {
    let missing = || {
        lower.error(
            span,
            &format!(
                "{use_site} requires the exact typescript::throw_error(message: string): null host catalog entry"
            ),
        )
    };
    let Some(catalog) = context.host_api_catalog else {
        return Err(missing());
    };
    let candidates = catalog.functions_named(crate::runtime::scalar_error::THROW_ERROR_FUNCTION);
    let exact = matches!(candidates.as_slice(), [candidate]
        if candidate.name == crate::runtime::scalar_error::THROW_ERROR_FUNCTION
            && candidate.params.len() == 1
            && candidate.params[0].name == "message"
            && candidate.params[0].passing == vm::HostParamPassing::Value
            && matches!(&candidate.params[0].ty, vm::HostTypeSchema::String)
            && matches!(&candidate.return_type, vm::HostTypeSchema::Null));
    if !exact {
        return Err(missing());
    }
    context
        .install_host_function(ir, crate::runtime::scalar_error::THROW_ERROR_FUNCTION, 1)
        .map_err(|_| missing())
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
/// finite static code IDs and validate the bounded closed subset.
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
        ast::Stmt::While(loop_) => {
            collect_from_expr(&loop_.test, lower, enclosing_strict)?;
            collect_from_stmt(&loop_.body, lower, enclosing_strict)
        }
        ast::Stmt::DoWhile(loop_) => {
            collect_from_stmt(&loop_.body, lower, enclosing_strict)?;
            collect_from_expr(&loop_.test, lower, enclosing_strict)
        }
        ast::Stmt::For(loop_) => {
            if let Some(init) = &loop_.init {
                match init {
                    ast::VarDeclOrExpr::VarDecl(declaration) => {
                        for declarator in &declaration.decls {
                            if let Some(init) = declarator.init.as_deref() {
                                collect_from_expr(init, lower, enclosing_strict)?;
                            }
                        }
                    }
                    ast::VarDeclOrExpr::Expr(expression) => {
                        collect_from_expr(expression, lower, enclosing_strict)?;
                    }
                }
            }
            if let Some(test) = &loop_.test {
                collect_from_expr(test, lower, enclosing_strict)?;
            }
            if let Some(update) = &loop_.update {
                collect_from_expr(update, lower, enclosing_strict)?;
            }
            collect_from_stmt(&loop_.body, lower, enclosing_strict)
        }
        ast::Stmt::ForOf(loop_) => {
            if let ast::ForHead::Pat(pat) = &loop_.left
                && let ast::Pat::Expr(expr) = pat.as_ref()
            {
                collect_from_expr(expr, lower, enclosing_strict)?;
            }
            collect_from_expr(&loop_.right, lower, enclosing_strict)?;
            collect_from_stmt(&loop_.body, lower, enclosing_strict)
        }
        ast::Stmt::Labeled(labeled) => collect_from_stmt(&labeled.body, lower, enclosing_strict),
        ast::Stmt::Throw(throw) => collect_from_expr(&throw.arg, lower, enclosing_strict),
        // A switch never defers its own clauses: the discriminant, every
        // selector and every clause body belong to this activation, so
        // functions there are collected even when never selected.
        ast::Stmt::Switch(select) => {
            collect_from_expr(&select.discriminant, lower, enclosing_strict)?;
            for case in &select.cases {
                if let Some(test) = &case.test {
                    collect_from_expr(test, lower, enclosing_strict)?;
                }
                for statement in &case.cons {
                    collect_from_stmt(statement, lower, enclosing_strict)?;
                }
            }
            Ok(())
        }
        ast::Stmt::Try(try_stmt) => {
            for statement in &try_stmt.block.stmts {
                collect_from_stmt(statement, lower, enclosing_strict)?;
            }
            if let Some(handler) = &try_stmt.handler {
                for statement in &handler.body.stmts {
                    collect_from_stmt(statement, lower, enclosing_strict)?;
                }
            }
            if let Some(finalizer) = &try_stmt.finalizer {
                for statement in &finalizer.stmts {
                    collect_from_stmt(statement, lower, enclosing_strict)?;
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
                if let ast::PropOrSpread::Prop(boxed) = prop {
                    match boxed.as_ref() {
                        ast::Prop::KeyValue(kv) => {
                            collect_from_expr(&kv.value, lower, enclosing_strict)?
                        }
                        ast::Prop::Getter(getter) => collect_function(
                            &ast::FnExpr {
                                ident: None,
                                function: getter.function.clone(),
                            },
                            lower,
                            enclosing_strict,
                        )?,
                        ast::Prop::Setter(setter) => collect_function(
                            &ast::FnExpr {
                                ident: None,
                                function: setter.function.clone(),
                            },
                            lower,
                            enclosing_strict,
                        )?,
                        _ => {}
                    }
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
        ast::Expr::Update(u) => collect_from_expr(&u.arg, lower, enclosing_strict),
        ast::Expr::Cond(b) => {
            collect_from_expr(&b.test, lower, enclosing_strict)?;
            collect_from_expr(&b.cons, lower, enclosing_strict)?;
            collect_from_expr(&b.alt, lower, enclosing_strict)
        }
        ast::Expr::Seq(s) => {
            for expression in &s.exprs {
                collect_from_expr(expression, lower, enclosing_strict)?;
            }
            Ok(())
        }
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

/// Validate one bounded ordinary function and assign its code ID. Non-strict
/// activations must first prove independence from unsupported call context.
/// Closed declarations share the declaration graph with ordered plain formals;
/// captures, named/self binding, constructors and generators remain refusals.
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
        reference::admit_closed_sloppy_function(lower, func)?;
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

fn lower_with_realm(
    script: &ast::Script,
    source: &str,
    source_id: u32,
    base: u32,
    context: &SourceFrontendContext<'_>,
) -> Result<(FrontendIr, Vec<super::private_reachability::OwnedHelper>), ParseError> {
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
        globals: super::global_binding::Plan::default(),
        global_helpers: None,
        global_env: None,
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
        array_names: HashSet::new(),
        source_bindings: Vec::new(),
        heap: None,
        intrinsics: None,
        objects: None,
        string_length_sites: std::cell::RefCell::new(std::collections::HashSet::new()),
        string_length_sealed: std::cell::Cell::new(false),
        helper_string_length: None,
        string_concat_sites: std::cell::RefCell::new(std::collections::HashMap::new()),
        string_concat_sealed: std::cell::Cell::new(false),
        helper_string_concat: None,
        helper_fixed_scalar_string: None,
        relational_sites: std::cell::RefCell::new(HashMap::new()),
        relational_sealed: std::cell::Cell::new(false),
        helper_string_compare: None,
        number_key_sites: std::cell::RefCell::new(std::collections::HashSet::new()),
        number_key_sealed: std::cell::Cell::new(false),
        number_string_entries: None,
        primitive_key: primitive_key::State::default(),
        init_cells: 0,
        primitive_number: primitive_number::State::default(),
        primitive_addition: primitive_addition::State::default(),
        mutation: primitive_assignment::State::default(),
        abstract_equality: abstract_equality::State::default(),
        math_used: std::cell::Cell::new(false),
        numeric_globals: std::cell::Cell::new(0),
        uri_used: std::cell::Cell::new(0),
        json_used: std::cell::Cell::new(false),
        number_namespace_used: std::cell::Cell::new(false),
        number_to_string_used: std::cell::Cell::new(false),
        function_name_used: std::cell::Cell::new(false),
        function_to_string_used: std::cell::Cell::new(false),
function_prototype_used: std::cell::Cell::new(false),
            primitive_constructor_read: std::cell::Cell::new(false),
construct_used: std::cell::Cell::new(false),
helper_construct: None,
            helper_construct_check: None,
        string_replace_all_used: std::cell::Cell::new(false),
        string_search_used: std::cell::Cell::new(false),
        string_match_used: std::cell::Cell::new(false),
        string_namespace_used: std::cell::Cell::new(false),
        array_namespace_used: std::cell::Cell::new(false),
        array_constructor_used: std::cell::Cell::new(false),
        arguments_array_from_values: None,
        arguments_string_key: None,
        arguments_sync: None,
        arguments_object: None,
        array_buffer_used: std::cell::Cell::new(false),
        typed_array_owned_quota_reachable: std::cell::Cell::new(false),
        typed_array_elements: std::cell::Cell::new(0),
        boolean_used: std::cell::Cell::new(false),
        range_error_used: std::cell::Cell::new(false),
        optional_errors_used: std::cell::Cell::new(0),
        utf16_bridge: None,
        uri_call: None,
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
        helper_int_identity: None,
        function_code_ids: HashMap::new(),
        collected_functions: Vec::new(),
        graph: scope::build(script, source, source_id, base)?,
        active_scope: scope::ScopeId(0),
        iteration: iteration::IterationState::default(),
        next_target: 1,
        catch_overlay: std::cell::Cell::new(None),
        needs_ordinary_realm: std::cell::Cell::new(false),
        pending_normal: None,
        pending_completion: None,
        pending_payload: None,
        uncaught_host: None,
        binding_slots: HashMap::new(),
        binding_kinds: HashMap::new(),
        type_names: None,
        utf16_inits: utf16_init::Utf16InitPool::default(),
    };
    lower.globals = super::global_binding::prepare(&mut lower.graph, script);
    lower.active_scope = lower.graph.script;
    let script_strict = lower.script_strict(script);
    lower.enclosing_strict = script_strict;
    lower.preflight_binding_literals(script)?;
    collect_source_functions(script, &mut lower, script_strict)?;
    // Script-level demand: independent producers plus surviving primitive-key
    // proofs. Function bodies are feature-validated just below; a proof there
    // can only live in a realm the existing `function_count > 0` contract
    // already installs, so this value stays the script-level route choice.
    let need_ref = reference::script_needs_ordinary_realm(&lower, &script.body)?;
    lower.collect_function_feature_sites()?;
    lower.collect_primitive_number_function_sites()?;
    lower.seal_primitive_number_sites();
    lower.seal_primitive_addition_sites();
    lower.seal_mutation_sites();
    lower.seal_abstract_equality_sites();
    lower.string_length_sealed.set(true);
    lower.string_concat_sealed.set(true);
    lower.relational_sealed.set(true);
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
    lower.helper_int_identity = Some(super::prelude::object::append_int_identity(
        &mut lower,
        &mut base_ir,
        probe_span,
        &mut statements,
    )?);
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
    // Demand-gated abstract wrapper. Strict ===/!== keep string_eq. This
    // append is before the runtime index window and synchronizes locals.
    lower.append_checked_abstract_string_equality(
        &mut base_ir,
        &carrier_schema,
        probe_span,
        &mut statements,
    )?;
    // Install the shared scalar error adapter before CORE function indices are
    // preassigned: `install_host_function` takes the next free index, and the
    // UTF-16 initializer pool reserves its own contiguous range after the
    // dispatch. Installing the adapter afterwards would race that reserved
    // range and duplicate a host-metadata index when the same source also
    // carries string literals.
    let throw_facts = reference::script_throw_facts(Some(&lower), &script.body);
    let catches_callee_throw = throw_facts.may_guest_throw;
    let array_buffer_used = lower.array_buffer_used.get();
    // Three independent facts, kept apart on purpose:
    //   A. `array_buffer_used` installs the ArrayBuffer realm (a public
    //      spelling or an owned Uint8 backing prototype pair needs it);
    //   B. `array_buffer_quota_reachable` is the public `new ArrayBuffer(...)`
    //      helper's fatal quota branch;
    //   C. `owned_uint8_quota_reachable` is the owned `new Uint8Array(...)`
    //      backing's fatal quota branch.
    // The realm fact never implies B or C: a safe owned literal installs the
    // realm without reaching either branch. B and C are the only fatal
    // adapter demands, and each is decided by the construction route's first
    // actual alone.
    let array_buffer_quota = array_buffer_used && throw_facts.array_buffer_quota_reachable;
    let typed_array_quota = lower.typed_array_used()
        && (lower.typed_array_owned_quota_reachable.get()
            || throw_facts.owned_uint8_quota_reachable);
    let scalar_error_host = if catches_callee_throw || array_buffer_quota || typed_array_quota {
        Some(install_scalar_error_host(
            context,
            &mut base_ir,
            &lower,
            probe_span,
            if catches_callee_throw {
                "JavaScript uncaught throw"
            } else if array_buffer_quota {
                "JavaScript ArrayBuffer quota handling"
            } else {
                "JavaScript Uint8Array quota handling"
            },
        )?)
    } else {
        None
    };
    if catches_callee_throw {
        lower.uncaught_host = scalar_error_host;
    }
    lower.append_string_concat_helper(
        &mut base_ir,
        &carrier_schema,
        probe_span,
        &mut statements,
    )?;
    lower.append_string_compare_helper(
        &mut base_ir,
        &carrier_schema,
        probe_span,
        &mut statements,
    )?;
    lower.append_primitive_number_parser(
        &mut base_ir,
        &carrier_schema,
        probe_span,
        &mut statements,
    )?;
    let random = if lower.math_used.get() {
        let catalog = context
            .host_api_catalog
            .ok_or_else(|| lower.error(probe_span, "Math.random requires the approved catalog"))?;
        let candidates = catalog.functions_named("compat_math::random");
        if !matches!(candidates.as_slice(), [c] if c.params.is_empty() && c.return_type == vm::HostTypeSchema::Number)
        {
            return Err(lower.error(
                probe_span,
                "Math.random requires exact compat_math::random() -> Number",
            ));
        }
        Some(context.install_host_function(&mut base_ir, "compat_math::random", 0)?)
    } else {
        None
    };
    // Script strictness from the exact raw prologue; inherited by every
    // admitted function without its own directive.
    // Only append the object/function helpers when some source shape needs
    // them; pure numeric-array/string sources keep the accepted program
    // without dead native code.
    let function_count = lower.collected_functions.len();
    if need_ref || function_count > 0 {
        // Number.parseInt/parseFloat select this arm. The else arm's
        // DefaultValue keys are not also charged.
        if super::prelude::math::used(&lower) {
            let (cells, nodes) = super::prelude::math::bootstrap_request(&lower);
            lower.charge_initial_cells(cells, probe_span)?;
            lower.init_nodes = lower
                .init_nodes
                .checked_add(nodes)
                .ok_or_else(|| lower.error(probe_span, "numeric intrinsic node budget overflow"))?;
        } else {
            // Object-bundle DefaultValue still materializes keys when Math is
            // absent. Charge the producer-measured pair, not the Math arm pair.
            let (cells, nodes) = super::prelude::math::default_value_key_request();
            lower.charge_initial_cells(cells, probe_span)?;
            lower.init_nodes = lower.init_nodes.checked_add(nodes).ok_or_else(|| {
                lower.error(probe_span, "DefaultValue key node budget overflow")
            })?;
        }
        if lower.uri_used.get() != 0 {
            // Adapter input/result frame and private URIError instance frame.
            lower.charge_initial_cells(
                (2 * layout::FRAME_FIXED_CELLS + 3 * layout::FRAME_CELLS_PER_ENTRY) as u64,
                probe_span,
            )?;
        }
        if lower.json_used.get() {
            // The optional `JSON` global object, its one own `stringify`
            // function and that property's key String. Sources that never name
            // `JSON` keep the exact previous manifest.
            lower.charge_initial_cells(
                super::prelude::intrinsics::json_request_cells(),
                probe_span,
            )?;
            lower.init_nodes = lower
                .init_nodes
                .checked_add(super::prelude::intrinsics::JSON_REQUEST_NODES)
                .ok_or_else(|| lower.error(probe_span, "JSON intrinsic node budget overflow"))?;
        }
        lower.charge_initial_cells(
            super::prelude::intrinsics::bootstrap_request_cells(&lower)
                + super::prelude::object::ObjectHelpers::helper_frame_request_cells()
                + (layout::FRAME_FIXED_CELLS + layout::FRAME_CELLS_PER_ENTRY) as u64,
            probe_span,
        )?;
        if lower.boolean_used.get() || lower.primitive_constructor_read.get() {
            // The Boolean carrier is always the same two-row function node; a
            // `.constructor` read selects it without a permanent root entry.
            lower.charge_initial_cells(super::prelude::intrinsics::BOOLEAN_FUNCTION_CELLS, probe_span)?;
        }
        if super::prelude::intrinsics::range_error_realm_used(&lower) {
            lower.charge_initial_cells(super::prelude::intrinsics::RANGE_ERROR_EXTRA_CELLS, probe_span)?;
        }
        for error in super::prelude::intrinsics::OptionalError::selected(&lower) {
            let (cells, nodes) = error.bootstrap_request(&lower);
            lower.charge_initial_cells(cells, probe_span)?;
            lower.init_nodes += nodes;
        }
        if lower.array_buffer_used.get() {
            lower.charge_initial_cells(super::prelude::array_buffer::BOOTSTRAP_CELLS, probe_span)?;
            lower.init_nodes += super::prelude::array_buffer::BOOTSTRAP_NODES;
        }
        if lower.typed_array_used() {
            let (cells, nodes) = super::prelude::typed_array::bootstrap_request(&lower);
            lower.charge_initial_cells(cells, probe_span)?;
            lower.init_nodes += nodes;
        }
        lower.init_nodes = lower
            .init_nodes
            .checked_add(
                super::prelude::intrinsics::bootstrap_node_count_gated(
                    lower.number_namespace_used.get(),
                    lower.string_namespace_used.get(),
                    lower.array_namespace_used.get(),
                    lower.number_to_string_used.get(),
                    lower.string_replace_all_used.get(),
                    lower.string_search_used.get(),
                    lower.string_match_used.get(),
                    lower.function_name_used.get() || lower.function_to_string_used.get(),
                    lower.function_to_string_used.get(),
                    lower.primitive_constructor_read.get(),
                )
                    + super::prelude::intrinsics::bootstrap_function_prototype_nodes(
                        lower.function_prototype_used.get(),
                    )
                    + uri_calls::manifest(lower.uri_used.get()).1
                    + u64::from(
                        lower.boolean_used.get() || lower.primitive_constructor_read.get(),
                    )
                    + if super::prelude::intrinsics::range_error_realm_used(&lower) {
                        super::prelude::intrinsics::RANGE_ERROR_EXTRA_NODES
                    } else {
                        0
                    },
            )
            .ok_or_else(|| lower.error(probe_span, "intrinsic node budget overflow"))?;
        // The intrinsic request already owns the first directory page; the
        // complete selected manifest is final at this point, so charge every
        // extra directory page exactly once.
        lower.charge_initial_cells(
            ((lower.init_nodes - 1) / layout::PAGE_ROWS as u64) * layout::DIR_PAGE_CELLS as u64,
            probe_span,
        )?;
        #[cfg(test)]
        super::prelude::intrinsics::record_intrinsic_budget(lower.init_cells, lower.init_nodes);
        lower.intrinsics = Some(super::prelude::intrinsics::append(
            &mut lower,
            &mut base_ir,
            &carrier_schema,
            probe_span,
            &mut statements,
        )?);
        if lower.globals.shared {
            lower.global_helpers = Some(super::prelude::global_binding::append(
                &mut lower,
                &mut base_ir,
                &carrier_schema,
                probe_span,
                &mut statements,
            )?);
        }
        if lower.helper_string_concat.is_some() || lower.helper_string_compare.is_some() {
            // Reused bridge entries skip their own append-time slot sync. The
            // intervening concat/intrinsic declarations must dominate both
            // completed-IR private kernels, without touching dormant programs.
            base_ir.locals = base_ir.locals.max(usize::from(lower.next_slot));
        }
        lower.append_number_key_prerequisites(&mut base_ir, probe_span)?;
        lower.append_primitive_key_prerequisites(
            &mut base_ir,
            &carrier_schema,
            probe_span,
            &mut statements,
        )?;
        // Object.prototype.hasOwnProperty is part of the ordinary prototype,
        // so its String-hint key conversion prerequisites are unconditional
        // for every realm that installs the object runtime. The producers are
        // the accepted managed Number/fixed-scalar String emitters, shared
        // with the numeric and URI paths that may also ask for them.
        lower.ensure_number_string_prerequisites(&mut base_ir, probe_span)?;
        lower.append_fixed_scalar_string_helper(
            &mut base_ir,
            &carrier_schema,
            probe_span,
            &mut statements,
        )?;
        lower.append_uri_prerequisites(
            &mut base_ir,
            &carrier_schema,
            probe_span,
            &mut statements,
        )?;
        // The bare `arguments` actual of `Array.isArray` is the Core
        // invocation vector, which has no guest identity. Box it once per
        // call through a private helper so the real Get+Call observes a
        // guest Array value (and a rewritten method observes the call).
        if super::reference::source_uses_array_is_array_arguments(&script.body) {
            let helper = super::prelude::array_constructor::append_from_values(
                &mut lower, &mut base_ir, &carrier_schema, probe_span, &mut statements,
            )?;
            lower.arguments_array_from_values = Some(helper);
            // One guest object per activation, mirrored with the invocation
            // vector around every pass: pinned Micro hands the same writable
            // Array to a rewritten `Array.isArray`.
            let sync = super::prelude::arguments_object::append_sync(
                &mut lower,
                &mut base_ir,
                &carrier_schema,
                probe_span,
                &mut statements,
            )?;
            lower.arguments_sync = Some(sync);
        }
        // A computed `arguments[key]` whose key is not a literal can hold a
        // guest String at runtime; the pinned binding is a real Array, so the
        // key is an ordinary property name. One private classifier decodes the
        // canonical-index / `length` / absent domains without allocating.
        if super::reference::source_uses_arguments_dynamic_key(&script.body) {
            let helper = super::prelude::arguments_key::append(
                &mut lower, &mut base_ir, &carrier_schema, probe_span, &mut statements,
            )?;
            lower.arguments_string_key = Some(helper);
        }
        let runtime_start = heap_emitters::fresh_index(&lower, &base_ir, probe_span)?;
        let reserved_dispatch = runtime_start
            .checked_add(super::prelude::object::ObjectHelpers::COUNT)
            .and_then(|i| i.checked_add(u16::from(!lower.string_length_sites.borrow().is_empty())))
            .and_then(|i| i.checked_add(u16::from(lower.function_prototype_used.get())))
            .and_then(|i| {
                i.checked_add(2 * u16::from(lower.construct_used.get()))
            })
            .and_then(|i| {
                i.checked_add(if super::prelude::math::used(&lower) {
                    super::prelude::math::COUNT
                } else {
                    0
                })
            })
            .and_then(|i| i.checked_add(u16::from(lower.array_buffer_used.get()) + u16::from(lower.typed_array_used())))
            .and_then(|i| i.checked_add(function_count as u16))
            .ok_or_else(|| lower.error(probe_span, "object runtime index space exhausted"))?;
        lower.helper_dispatch = Some(reserved_dispatch);
        let objects = super::prelude::object::append(
            &mut lower,
            &mut base_ir,
            &carrier_schema,
            probe_span,
            &mut statements,
            reserved_dispatch,
        )?;
        lower.helper_ref_get = Some(objects.get);
        lower.objects = Some(objects);
        // The gated `prototype` get/set adapter sits outside the fixed object
        // helper window: it is one extra private function whose flat index the
        // dispatch reservation above already charged.
        let function_prototype_api = if lower.function_prototype_used.get() {
            Some(super::prelude::function_prototype::append(
                &mut lower,
                &mut base_ir,
                &carrier_schema,
                probe_span,
                &mut statements,
            )?)
        } else {
            None
        };
        // The `new F(...)` construct helper is the second extra private
        // function outside the fixed object window; it calls the reserved
        // dispatch index, so it must be declared after that index is fixed.
        if lower.construct_used.get() {
            lower.helper_construct = Some(super::prelude::construct::append(
                &mut lower,
                &mut base_ir,
                &carrier_schema,
                probe_span,
                &mut statements,
            )?);
            // The pre-`Get` gate: the callee kind must be decided before the
            // prototype read, because a non-function callee may answer the
            // read with its own throwing `prototype` getter.
            lower.helper_construct_check = Some(super::prelude::construct::append_check(
                &mut lower,
                &mut base_ir,
                &carrier_schema,
                probe_span,
                &mut statements,
            )?);
        }
        lower.append_string_length_adapter(
            &mut base_ir,
            &carrier_schema,
            probe_span,
            &mut statements,
        )?;
        let numeric = if super::prelude::math::used(&lower) {
            Some(super::prelude::math::append(
                &mut lower,
                &mut base_ir,
                &carrier_schema,
                probe_span,
                &mut statements,
                random,
            )?)
        } else {
            None
        };
        if let Some(ref numeric) = numeric {
            lower.publish_math_number_parser(numeric.parser);
        }
        if array_buffer_used {
            let helper = super::prelude::array_buffer::append(
                &mut lower,
                &mut base_ir,
                &carrier_schema,
                probe_span,
                &mut statements,
                if array_buffer_quota {
                    scalar_error_host
                } else {
                    None
                },
            )?;
            lower.intrinsics.as_mut().expect("ArrayBuffer realm").array_buffer = Some(helper);
        }
        if lower.typed_array_used() {
            let helper = super::prelude::typed_array::append(
                &mut lower,
                &mut base_ir,
                &carrier_schema,
                probe_span,
                &mut statements,
                if typed_array_quota {
                    scalar_error_host
                } else {
                    None
                },
            )?;
            lower.intrinsics.as_mut().expect("typed array realm").typed_array = Some(helper);
        }
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
        {
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
                function_prototype_api,
            )?;
            debug_assert_eq!(dispatch, dispatch_index);
            if let Some(numeric) = numeric {
                super::prelude::math::extend_dispatch(&mut base_ir, dispatch, numeric.call);
            }
            if let Some(adapter) = lower.uri_call {
                uri_calls::extend_dispatch(&mut base_ir, dispatch, adapter, lower.uri_used.get());
            }
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
    if let Some(intrinsics) = lower.intrinsics {
        let sink = lower.slot(probe_span)?;
        statements.push(Stmt::Let {
            index: sink,
            declared_schema: Some(TypeSchema::Null),
            expr: Lowerer::guest_call(intrinsics.bootstrap, vec![Expr::Var(carrier)]),
            line: lower.line(probe_span),
        });
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
    if lower.globals.shared {
        lower.initialize_global_environment(probe_span, &mut body)?;
    }
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
    let descriptor = super::prelude::object::descriptor_declaration();
    base_ir
        .struct_schemas
        .insert(descriptor.name.clone(), descriptor);
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
    if lower.objects.is_some() {
        super::prelude::object::apply_scalar_boundaries(&mut base_ir);
    }
    // Ownership comes from the actual helper constructors, never a name scan.
    // Carry these IDs through final factoring before recomputing reachability.
    use super::private_reachability::{OwnedHelper, Role};
    let mut owned = Vec::new();
    if let Some(i) = lower.intrinsics {
        owned.push(OwnedHelper {
            index: i.length_key,
            role: Role::LengthKey,
        });
    }
    if let Some(o) = lower.objects {
        owned.push(OwnedHelper {
            index: o.put,
            role: Role::Put,
        });
        owned.push(OwnedHelper {
            index: o.delete,
            role: Role::Delete,
        });
    }
    #[cfg(test)]
    note_realm_budget(lower.init_cells, false);
    Ok((base_ir, owned))
}

#[cfg(test)]
thread_local! {
    static REALM_BUDGET_WITNESS: std::cell::Cell<(u64, bool)> = const { std::cell::Cell::new((0, false)) };
}

#[cfg(test)]
fn note_realm_budget(cells: u64, refused: bool) {
    REALM_BUDGET_WITNESS.with(|slot| slot.set((cells, refused)));
}

#[cfg(test)]
pub(crate) fn realm_budget_witness() -> (u64, bool) {
    REALM_BUDGET_WITNESS.with(|slot| slot.get())
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

/// Append the private exact-Int decode helper `(Number) -> Int`. The accepted
/// Core internal `__checked_int_from_number` builtin performs the exact
/// integral check itself, so this helper keeps only its original Compat
/// domain guards: nonnegative and inclusive `MAX_REALM_INITIAL_CELLS`. The one
/// conversion call receives the raw caller parameter, never the
/// as_number-normalized guard value, and yields a bare inline `Int` or the
/// existing `VmError`; no guest Result wrapper is introduced.
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
    let budget = MAX_REALM_INITIAL_CELLS as f64;
    let body = vec![
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
    ];
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
            body_expr: Expr::Call(
                vm::BuiltinFunction::CheckedIntFromNumber.call_index(),
                Vec::new(),
                vec![Expr::Var(param_x)],
                None,
                None,
            ),
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
    // An activation that hands bare `arguments` to `Array.isArray` owns one
    // guest Array value for its whole lifetime. Reserve its slot and root
    // entry before the body is emitted; nested activations restore the outer
    // slot, exactly like `function_args`.
    let saved_object = lower.arguments_object.take();
    if super::reference::source_uses_array_is_array_arguments(&body.stmts) {
        let (object, mut reserved) = lower.bind_rooted(
            value::undefined(),
            value::schema(),
            collected.span,
        )?;
        body_stmts.append(&mut reserved);
        lower.arguments_object = Some(object);
    }
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
    // Every activation owns its iteration state: enhanced-mode selection,
    // control-target stack, list/loop slot pools and the empty Completion
    // record. Nested function bodies are separate activations, so their
    // labels/jumps can never resolve to an outer target. Static target IDs
    // stay monotone across activations on the Lowerer itself.
    let saved_iteration = std::mem::take(&mut lower.iteration);
    let saved_pending = (
        lower.pending_normal,
        lower.pending_completion,
        lower.pending_payload,
    );
    lower.pending_normal = None;
    lower.pending_completion = None;
    lower.pending_payload = None;
    let saved_global_env = lower.global_env;
    if lower.globals.shared {
        lower.global_env = Some(param_env);
        let checked = lower.slot(collected.span)?;
        body_stmts.push(Stmt::Let {
            index: checked,
            declared_schema: Some(TypeSchema::Int),
            expr: Lowerer::guest_call(
                lower.global_helpers.expect("shared helpers").checked,
                vec![Expr::Var(param_carrier), Expr::Var(param_env)],
            ),
            line,
        });
    }
    let lowered = reference::lower_function_body(lower, &body.stmts, &mut body_stmts);
    lower.global_env = saved_global_env;
    lower.pending_normal = saved_pending.0;
    lower.pending_completion = saved_pending.1;
    lower.pending_payload = saved_pending.2;
    lower.binding_slots = saved_slots;
    lower.binding_kinds = saved_kinds;
    lower.array_length_ids = saved_length_ids;
    lower.iteration = saved_iteration;
    lower.arguments_object = saved_object;
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
    // Frame capacity is known only after the body lowered. An activation
    // whose body and completion never name the frame keeps no entry and no
    // exit at all; a retained frame is prefixed, saves the real Completion,
    // leaves without allocating or collecting, and returns. The caller pins
    // Completion.value with its first no-allocation root write before
    // another safepoint.
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
    let completion_expr = completion.unwrap_or_else(value::completion_normal);
    // One static decision for the whole activation frame. Both the lowered
    // body and the completion expression are complete here, and neither the
    // frame prologue/epilogue nor the frame accounting has been emitted.
    // A closure, or the producer's actual capture vector, always retains.
    let capture_copies = Vec::new();
    let root_frame_used = !capture_copies.is_empty()
        || super::private_reachability::slot_may_be_used(&body_stmts, &completion_expr, frame_slot);
    if root_frame_used {
        lower.charge_initial_cells(layout::FRAME_FIXED_CELLS as u64, collected.span)?;
    } else {
        // Every body entry already charged FRAME_CELLS_PER_ENTRY at creation;
        // an omitted frame refunds exactly those cells and never charges the
        // fixed frame cells. Underflow cannot happen and stays a real error.
        let refund = u64::from(capacity) * layout::FRAME_CELLS_PER_ENTRY as u64;
        let remaining = lower.init_cells.checked_sub(refund).ok_or_else(|| {
            lower.error(
                collected.span,
                "JavaScript ES5 idle activation frame accounting underflow",
            )
        })?;
        lower.init_cells = remaining;
    }
    let (comp_slot, comp_stmt) =
        lower.bind_typed(completion_expr, value::completion_schema(), collected.span)?;
    let mut prologue = Vec::new();
    if root_frame_used {
        prologue.push(Stmt::Let {
            index: frame_slot,
            declared_schema: Some(TypeSchema::Int),
            expr: Lowerer::guest_call(
                helpers.root_enter,
                vec![Expr::Var(param_carrier), Expr::Int(i64::from(capacity))],
            ),
            line,
        });
    }
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
    ];
    if root_frame_used {
        epilogue.push(Stmt::Let {
            index: leave_ack,
            declared_schema: Some(TypeSchema::Int),
            expr: Lowerer::guest_call(
                helpers.root_leave,
                vec![Expr::Var(param_carrier), Expr::Var(frame_slot)],
            ),
            line,
        });
    }
    // Build the boundary Result while frame roots still cover the payload.
    // root_leave stays nonallocating and remains the single frame exit.
    let boundary = lower.slot(collected.span)?;
    let thrown = lower.slot(collected.span)?;
    let ir_span = lower.ir_span(collected.span);
    epilogue.insert(
        1,
        Stmt::Let {
            index: boundary,
            declared_schema: Some(value::logical_result_schema()),
            expr: value::result_ok(Expr::Var(comp_slot), ir_span),
            line,
        },
    );
    epilogue.insert(
        2,
        Stmt::IfElse {
            condition: Expr::Eq(
                Box::new(value::field(comp_slot, "kind")),
                Box::new(Expr::Int(value::COMPLETION_THROW)),
            ),
            then_branch: vec![
                Stmt::Let {
                    index: thrown,
                    declared_schema: Some(value::schema()),
                    expr: value::field(comp_slot, "value"),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: boundary,
                    expr: value::result_err(Expr::Var(thrown), ir_span),
                    line,
                },
            ],
            else_branch: Vec::new(),
            line,
        },
    );
    prologue.append(&mut body_stmts);
    prologue.append(&mut epilogue);
    body_stmts = prologue;
    let body_expr = Expr::Var(boundary);
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
        return_schema: Some(value::logical_result_schema()),
        type_params: Vec::new(),
        exported: false,
        return_type: ValueType::Map,
        symbol: None,
    });
    ir.function_impls.insert(
        flat_index,
        FunctionImpl {
            param_slots: vec![param_carrier, param_env, param_this, param_args, param_mode],
            capture_copies,
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
) -> Result<Option<(u16, ArrayShape)>, ParseError> {
    let carrier = lower.active_carrier(span)?;
    let line = lower.line(span);
    // Static pre-check from the syntactic element count (holes and spread
    // still reject below) so budget excess fails before large emission; the
    // exact reservation below charges the actual packed/tagged extent again.
    let syntactic = layout::ARRAY_HEADER_CELLS
        .checked_add(literal.elems.len() as i64)
        .ok_or_else(|| lower.error(span, "P0 dense array length overflow"))?;
    let pre_need = (syntactic as u64)
        .checked_add(layout::RAW_HEADER_CELLS as u64)
        .ok_or_else(|| lower.error(span, "P0 dense array length overflow"))?;
    if let Some(end) = lower.init_cells.checked_add(pre_need) {
        if end > MAX_REALM_INITIAL_CELLS {
            #[cfg(test)]
            note_realm_budget(end, true);
            return Err(lower.error(
                span,
                "JavaScript ES5 realm initialization exceeds the 65536 cell budget",
            ));
        }
    } else {
        #[cfg(test)]
        note_realm_budget(u64::MAX, true);
        return Err(lower.error(span, "P0 dense array length overflow"));
    }
    // Elements evaluate left to right before the allocation safepoint.
    // Number elements carry no heap edge and keep the old root-free
    // fastpath; every other element is a rooted JsValue so the later
    // reservation safepoint cannot strand it.
    let mut element_slots: Vec<(u16, Kind)> = Vec::new();
    let mut owned_elements: Vec<u16> = Vec::new();
    for element in &literal.elems {
        let Some(element) = element else {
            return Err(lower.error(span, "P0 dense arrays reject holes"));
        };
        if element.spread.is_some() {
            return Err(lower.error(element.span(), "P0 dense arrays reject spread elements"));
        }
        let value = lower.expr(&element.expr)?;
        reference::admit_array_element(lower, element.span(), value.kind, None)?;
        if value.kind == Kind::Number {
            let (slot, stmt) = lower.bind_temp_bare(value.expr, element.span())?;
            statements.push(stmt);
            reference::release_consumed_roots(
                lower,
                &value.owned_roots,
                slot,
                element.span(),
                statements,
            )?;
            element_slots.push((slot, value.kind));
        } else {
            owned_elements.extend(value.owned_roots);
            let (slot, bound) = lower.bind_temp(value.expr, element.span())?;
            statements.extend(bound);
            element_slots.push((slot, value.kind));
            owned_elements.push(slot);
        }
    }
    let length = element_slots.len();
    let repr = if element_slots.iter().all(|(_, kind)| *kind == Kind::Number) {
        ArrayRepr::Packed
    } else {
        ArrayRepr::Tagged
    };
    let stride = match repr {
        ArrayRepr::Packed => 1,
        ArrayRepr::Tagged => layout::ARRAY_TAGGED_CELLS_PER_ENTRY,
    };
    let payload_count = stride
        .checked_mul(length as i64)
        .ok_or_else(|| lower.error(span, "P0 dense array length overflow"))?;
    let data_count = layout::ARRAY_HEADER_CELLS
        .checked_add(payload_count)
        .ok_or_else(|| lower.error(span, "P0 dense array length overflow"))?;
    let newborn = lower.begin_node(
        carrier,
        data_count,
        match repr {
            ArrayRepr::Packed => layout::KIND_ARRAY,
            ArrayRepr::Tagged => layout::KIND_TAGGED_ARRAY,
        },
        span,
        line,
        statements,
    )?;
    // Cell 2 is the plain public length. Cell 5 is physical present + 1.
    // Cell 1 is the node generation and must stay untouched.
    let mut header_cells = vec![
        (2i64, Expr::Int(length as i64)),
        (
            layout::ARRAY_PRESENT_OFFSET,
            Expr::Int(length as i64 + 1),
        ),
    ];
    if let Some(intrinsics) = lower.intrinsics {
        let proto = Lowerer::guest_call(intrinsics.array_proto, vec![Expr::Var(carrier)]);
        for (offset, name) in [(3i64, "ref_slot"), (4i64, "generation")] {
            header_cells.push((
                offset,
                Lowerer::as_number(value::field_of(proto.clone(), name)),
            ));
        }
    } else {
        header_cells.extend([(3i64, Expr::Int(0)), (4i64, Expr::Int(0))]);
    }
    for (offset, payload) in header_cells {
        lower.emit_cell_write(
            carrier,
            Expr::Add(
                Box::new(Expr::Var(newborn.base)),
                Box::new(Expr::Int(offset)),
            ),
            payload,
            span,
            line,
            statements,
        )?;
    }
    for (position, (element, kind)) in element_slots.iter().enumerate() {
        let base = layout::ARRAY_HEADER_CELLS + stride * position as i64;
        match (repr, kind) {
            (ArrayRepr::Packed, _) => {
                lower.emit_cell_write(
                    carrier,
                    Expr::Add(Box::new(Expr::Var(newborn.base)), Box::new(Expr::Int(base))),
                    value::field(*element, "number"),
                    span,
                    line,
                    statements,
                )?;
            }
            (ArrayRepr::Tagged, _) => {
                // Same tag/number/flag/slot/gen scalar encoding as the
                // environment entry reader; never a divergent box format.
                let flag = Expr::IfElse {
                    condition: Box::new(value::field(*element, "flag")),
                    then_expr: Box::new(Expr::Int(1)),
                    else_expr: Box::new(Expr::Int(0)),
                };
                for (offset, payload) in [
                    (0i64, Lowerer::as_number(value::field(*element, "tag"))),
                    (1i64, value::field(*element, "number")),
                    (2i64, Lowerer::as_number(flag)),
                    (3i64, Lowerer::as_number(value::field(*element, "ref_slot"))),
                    (
                        4i64,
                        Lowerer::as_number(value::field(*element, "generation")),
                    ),
                ] {
                    lower.emit_cell_write(
                        carrier,
                        Expr::Add(
                            Box::new(Expr::Var(newborn.base)),
                            Box::new(Expr::Int(base + offset)),
                        ),
                        payload,
                        span,
                        line,
                        statements,
                    )?;
                }
            }
        }
    }
    let shape = ArrayShape {
        length,
        repr,
        number_only_below: length,
    };
    if anonymous {
        let boxed = lower.publish_node(carrier, &newborn, span, line, statements)?;
        reference::release_consumed_roots(lower, &owned_elements, boxed, span, statements)?;
        return Ok(Some((boxed, shape)));
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
        reference::release_consumed_roots(lower, &owned_elements, slot, span, statements)?;
        lower.bindings.insert(name.to_owned(), (slot, Kind::Array));
        lower.binding_kinds.insert(id, Kind::Array);
        lower.array_names.insert(name.to_owned());
        lower.array_lengths.insert(name.to_owned(), shape);
        lower.array_length_ids.insert(id, shape);
        return Ok(None);
    }
    let boxed = lower.publish_node(carrier, &newborn, span, line, statements)?;
    reference::release_consumed_roots(lower, &owned_elements, boxed, span, statements)?;
    lower.bindings.insert(name.to_owned(), (boxed, Kind::Array));
    lower.array_names.insert(name.to_owned());
    lower.array_lengths.insert(name.to_owned(), shape);
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
    if super::reference::is_arguments_base(member)
        && lower.function_args.is_some()
        && super::reference::implicit_arguments(lower)
    {
        let value = lower.lower_arguments_store(member, right, span)?;
        let (slot, saved) = lower.bind_temp(value.expr, span)?;
        statements.extend(saved);
        return Ok(slot);
    }
    if lower.is_array_length_assign(member) {
        return lower.lower_array_length_assign(member, right, span, statements);
    }
    if lower.primitive_key_put_mode(member).is_some() {
        let value = lower.lower_property_assignment(member, right, span)?;
        let (slot, saved) = lower.bind_temp(value.expr, span)?;
        statements.extend(saved);
        return Ok(slot);
    }
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
    // Discarded computed writes on the ordinary/named receiver routes already
    // own their capability boundary in `lower_property_assignment`: receiver,
    // key and RHS admission run in source order and the site becomes
    // capability-only instead of materializing a Reference. Take that existing
    // boundary before the legacy identifier/string-literal key gate, which
    // would otherwise refuse the same already-admitted site. The dense-Array
    // numeric route below (including its String/Object key refusals) is
    // untouched; `admit=false` never becomes a permission.
    if matches!(
        kind,
        Kind::Object | Kind::Function | Kind::Null | Kind::Undefined | Kind::Dynamic
    ) && (lower.in_dead_capability() || lower.emission_known_throw())
        && Lowerer::open_computed_key(member).is_some()
    {
        let value = lower.lower_property_assignment(member, right, span)?;
        let (slot, saved) = lower.bind_temp(value.expr, span)?;
        statements.extend(saved);
        return Ok(slot);
    }
    reference::admit_member_write(lower, member, kind, length)?;
    if kind != Kind::Array {
        let value = lower.lower_property_assignment(member, right, span)?;
        let (slot, saved) = lower.bind_temp(value.expr, span)?;
        statements.extend(saved);
        return Ok(slot);
    }
    lower.lower_array_member_write_stmt(member, right, span, statements)
}

#[cfg(test)]
mod private_prune_tests {
    use super::*;
    #[test]
    fn p7_private_prune_actual_final_ir_preserves_all_retained_bytes() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let observers = [
            "__compat_es5_root_write_node",
            "__compat_es5_node_publish_edge",
            "__compat_es5_heap_observe",
            "__compat_es5_set_prototype",
        ];
        for (source, dead) in [
            (
                "var p={x:1};var o=Object.create(p);var a=o.x===1;var b=\"x\" in o;var c=Object.getOwnPropertyDescriptor(o,\"x\")===undefined;a&&b&&c;",
                vec![
                    "__compat_es5_function_length_key",
                    "__compat_es5_ref_put",
                    "__compat_es5_delete_property",
                ],
            ),
            (
                "var o={x:1};if(false){o.x=2;delete o.x;}o.x;",
                vec!["__compat_es5_function_length_key"],
            ),
            (
                "function f(){'use strict';var o={x:1};if(false){o.x=2;}return o.x;}f();",
                vec!["__compat_es5_delete_property"],
            ),
            (
                "var o={set x(v){'use strict';},get x(){'use strict';return 1;}};o.x=2;",
                vec!["__compat_es5_delete_property"],
            ),
        ] {
            let (script, base) = super::super::parser::parse_script(source, 0).unwrap();
            // Private emitter unit control, with no new parse mode or API.
            // This is exactly the stage immediately before the ordinary final
            // pass; scalar/caller boundaries and all source bodies are present.
            let (mut ir, owned) = lower_with_realm(&script, source, 0, base, &context).unwrap();
            undefined_box::factor(&mut ir);
            let mut expected = ir.clone();
            let dropped = dead
                .iter()
                .map(|name| ir.functions.iter().find(|d| &d.name == name).unwrap().index)
                .collect::<Vec<_>>();
            expected.functions.retain(|d| !dropped.contains(&d.index));
            expected.function_impls.retain(|i, _| !dropped.contains(i));
            expected
                .stmts
                .retain(|s| !matches!(s, Stmt::FuncDecl { index, .. } if dropped.contains(index)));
            super::super::private_reachability::prune(&mut ir, &owned);
            // Exact full IR equality: all source/SDK/root/capture/declaration
            // bodies, semantic IDs, source-code constants and metadata survive.
            assert_eq!(
                format!("{ir:#?}"),
                format!("{expected:#?}"),
                "source={source}"
            );
            for name in observers {
                let decl = ir.functions.iter().find(|d| d.name == name).unwrap();
                assert!(ir.function_impls.contains_key(&decl.index));
            }
            eprintln!(
                "P7_PRIVATE_PRUNE_FINAL_IR source={source:?} dropped={dead:?} retained={} observer_roots=4 exact_retained_ir=true identity_indices=true native=false",
                ir.functions.len()
            );
        }
    }
}

#[cfg(test)]
mod bootstrap_page_budget_tests {
    use super::*;

    fn lower_witness(
        script: &ast::Script,
        source: &str,
        source_id: u32,
        base: u32,
        context: &SourceFrontendContext<'_>,
    ) -> Result<(FrontendIr, Vec<super::super::private_reachability::OwnedHelper>), ParseError> {
        let result = lower_with_realm(script, source, source_id, base, context);
        let (cells, refused) = super::realm_budget_witness();
        let line = if refused {
            format!(
                "BUDGET_EVIDENCE bytes={} refuse attempted={cells} overage={}",
                source.len(),
                cells.saturating_sub(65_536)
            )
        } else {
            format!(
                "BUDGET_EVIDENCE bytes={} admit cells={cells} remainder={}",
                source.len(),
                65_536u64.saturating_sub(cells)
            )
        };
        eprintln!("{line}");
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tmp/es51-abuf176");
        let _ = std::fs::create_dir_all(dir);
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(format!("{dir}/budget-evidence.log"))
        {
            use std::io::Write;
            let _ = writeln!(file, "{line}");
        }
        result
    }

    // Four-URI source family boundary. The N below is the measured
    // admit/refuse edge, not a static derivation: each of the four trailing
    // String literals costs one cell per UTF-16 unit, so 15252 units (61078
    // source bytes) admit at 65534 cells (two left) and 15253 units (61082
    // bytes) refuse at 65538. Slope 4 cells per family unit (one per literal
    // per unit), fixed term 4526 cells. The zero-margin tripwire now sits
    // on the default family below, not here. The prefix is the four full-URI
    // builtin statements; the witness source is generated here rather than
    // embedded.
    #[test]
    fn full_uri_second_directory_page_refuses_static_budget() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let padding = "a".repeat(15253);
        let source = format!(
            "encodeURI;decodeURI;encodeURIComponent;decodeURIComponent;\"{padding}\";\"{padding}\";\"{padding}\";\"{padding}\";"
        );
        assert_eq!(source.len(), 61082, "witness JS_source_bytes must match");
        let (script, base) = super::super::parser::parse_script(&source, 0)
            .expect("four 15253-unit full-URI String statements must parse");
        // One parse and one lower per test, producing IR only: no Core
        // compilation, no VM execution, and no Node execution.
        match lower_witness(&script, &source, 0, base, &context) {
            Ok(_) => panic!(
                "four 15253-unit full-URI String literals were unexpectedly admitted; the measured edge is 15252 units at 65534 cells"
            ),
            Err(error) => assert!(
                format!("{error:?}").contains("65536 cell budget"),
                "four 15253-unit full-URI String literals must refuse at the 65536 cell budget, got {error:?}"
            ),
        }
    }

    // Optional-manifest source boundaries. Every case below uses the same
    // generator: a prefix of bare builtin reads, then one tagged Array whose
    // elements are references to an installed managed function or the JSON
    // object. Each test parses once and lowers once to IR only: no Core
    // compilation, no VM execution, and no Node execution.
    //
    // The N in each case is the measured admit/refuse edge, not a static
    // prediction, and the recorded cell totals quoted beside it come from
    // `BUDGET_EVIDENCE` on the same run. Cells grow linearly at 8 per Array
    // element for every manifest (measured at N=0..3 and again at the edge),
    // so every case also states the measured fixed term at N=0.
    //
    // Node totals: the default manifest records 126 init nodes
    // (`bootstrap_node_count` 124 plus the two object-bundle DefaultValue key
    // nodes), JSON-only 131 (124 + JSON 5 + 2), one URI 129 (124 + 3 + 2),
    // one URI plus JSON 134 (124 + 3 + 5 + 2), the four-URI family 132
    // (124 + 6 + 2) and Math plus four URI plus JSON 198. The default
    // manifest therefore already occupies two directory pages, so the
    // 451-cell page fee sits on the manifest and is not charged again on the
    // first source node.

    // JSON-only manifest, 131 nodes, so the second directory page is already
    // charged. Measured edge: N=7638 (38197 bytes) admits at 65531 cells,
    // N=7639 (38202 bytes) refuses at 65539. Slope 8 cells per element, fixed
    // term 4427 cells. Prefix "JSON;".
    #[test]
    fn json_manifest_selected_second_page_refuses() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let prefix = "JSON;";
        let entries = std::iter::repeat_n("JSON", 7639)
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("{prefix}[{entries}];");
        assert_eq!(source.len(), 38202, "witness JS_source_bytes must match");
        let (script, base) = super::super::parser::parse_script(&source, 0)
            .expect("JSON manifest first-source tagged Array must parse");
        // One parse and one lower, IR only.
        match lower_witness(&script, &source, 0, base, &context) {
            Ok(_) => panic!(
                "JSON manifest tagged Array at N=7639 was unexpectedly admitted; the measured edge is N=7638 at 65531 cells"
            ),
            Err(error) => assert!(
                format!("{error:?}").contains("65536 cell budget"),
                "JSON manifest first-source tagged Array must refuse at the 65536 cell budget, got {error:?}"
            ),
        }
    }

    // The same measured JSON edge under the near-boundary name: N=7639
    // (38202 bytes) refuses at 65539; the admit side is N=7638 at 65531.
    // Slope 8 cells per element, fixed term 4427 cells. This family has one
    // edge, so the near-boundary case repeats the witness rather than moving
    // it.
    #[test]
    fn json_manifest_selected_second_page_near_boundary_refuses() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let prefix = "JSON;";
        let entries = std::iter::repeat_n("JSON", 7639)
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("{prefix}[{entries}];");
        assert_eq!(source.len(), 38202, "witness JS_source_bytes must match");
        let (script, base) = super::super::parser::parse_script(&source, 0)
            .expect("JSON manifest near-boundary tagged Array must parse");
        // One parse and one lower, IR only.
        match lower_witness(&script, &source, 0, base, &context) {
            Ok(_) => panic!(
                "JSON manifest near-boundary tagged Array was unexpectedly admitted; the 65536 cell budget must refuse N=7639"
            ),
            Err(error) => assert!(
                format!("{error:?}").contains("65536 cell budget"),
                "JSON manifest near-boundary tagged Array must refuse at the 65536 cell budget, got {error:?}"
            ),
        }
    }

    // The same measured JSON edge under the adjacent-lower-source name:
    // N=7639 (38202 bytes) refuses at 65539; N=7638 admits at 65531. Slope 8
    // cells per element, fixed term 4427 cells.
    #[test]
    fn json_manifest_selected_second_page_adjacent_lower_source_refuses() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let prefix = "JSON;";
        let entries = std::iter::repeat_n("JSON", 7639)
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("{prefix}[{entries}];");
        assert_eq!(source.len(), 38202, "witness JS_source_bytes must match");
        let (script, base) = super::super::parser::parse_script(&source, 0)
            .expect("JSON manifest adjacent lower tagged Array must parse");
        // One parse and one lower, IR only.
        match lower_witness(&script, &source, 0, base, &context) {
            Ok(_) => panic!(
                "JSON manifest adjacent lower tagged Array was unexpectedly admitted; the 65536 cell budget must refuse N=7639"
            ),
            Err(error) => assert!(
                format!("{error:?}").contains("65536 cell budget"),
                "JSON manifest adjacent lower tagged Array must refuse at the 65536 cell budget, got {error:?}"
            ),
        }
    }

    // One URI plus JSON manifest, 134 nodes, so both extra directory pages
    // (902 cells) are already charged. Measured edge: N=7628 (38157 bytes)
    // admits at 65533 cells, N=7629 (38162 bytes) refuses at 65538. Slope 8
    // cells per element, fixed term 4509 cells. Prefix "encodeURI;JSON;".
    #[test]
    fn uri_json_manifest_second_page_refuses() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let prefix = "encodeURI;JSON;";
        let entries = std::iter::repeat_n("JSON", 7629)
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("{prefix}[{entries}];");
        assert_eq!(source.len(), 38162, "witness JS_source_bytes must match");
        let (script, base) = super::super::parser::parse_script(&source, 0)
            .expect("URI plus JSON manifest tagged Array must parse");
        // One parse and one lower, IR only.
        match lower_witness(&script, &source, 0, base, &context) {
            Ok(_) => panic!(
                "URI plus JSON manifest tagged Array at N=7629 was unexpectedly admitted; the measured edge is N=7628 at 65533 cells"
            ),
            Err(error) => assert!(
                format!("{error:?}").contains("65536 cell budget"),
                "URI plus JSON manifest tagged Array must refuse at the 65536 cell budget, got {error:?}"
            ),
        }
    }

    // The same measured one-URI-plus-JSON edge under the near-boundary name:
    // N=7629 (38162 bytes) refuses at 65538; N=7628 admits at 65533. Slope 8
    // cells per element, fixed term 4509 cells.
    #[test]
    fn uri_json_manifest_second_page_near_boundary_refuses() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let prefix = "encodeURI;JSON;";
        let entries = std::iter::repeat_n("JSON", 7629)
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("{prefix}[{entries}];");
        assert_eq!(source.len(), 38162, "witness JS_source_bytes must match");
        let (script, base) = super::super::parser::parse_script(&source, 0)
            .expect("URI plus JSON manifest near-boundary tagged Array must parse");
        // One parse and one lower, IR only.
        match lower_witness(&script, &source, 0, base, &context) {
            Ok(_) => panic!(
                "URI plus JSON manifest near-boundary tagged Array was unexpectedly admitted; the 65536 cell budget must refuse N=7629"
            ),
            Err(error) => assert!(
                format!("{error:?}").contains("65536 cell budget"),
                "URI plus JSON manifest near-boundary tagged Array must refuse at the 65536 cell budget, got {error:?}"
            ),
        }
    }

    // The same measured one-URI-plus-JSON edge under the adjacent-lower-source
    // name: N=7629 (38162 bytes) refuses at 65538; N=7628 admits at 65533.
    // Slope 8 cells per element, fixed term 4509 cells.
    #[test]
    fn uri_json_manifest_second_page_adjacent_lower_source_refuses() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let prefix = "encodeURI;JSON;";
        let entries = std::iter::repeat_n("JSON", 7629)
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("{prefix}[{entries}];");
        assert_eq!(source.len(), 38162, "witness JS_source_bytes must match");
        let (script, base) = super::super::parser::parse_script(&source, 0)
            .expect("URI plus JSON manifest adjacent lower tagged Array must parse");
        // One parse and one lower, IR only.
        match lower_witness(&script, &source, 0, base, &context) {
            Ok(_) => panic!(
                "URI plus JSON manifest adjacent lower tagged Array was unexpectedly admitted; the 65536 cell budget must refuse N=7629"
            ),
            Err(error) => assert!(
                format!("{error:?}").contains("65536 cell budget"),
                "URI plus JSON manifest adjacent lower tagged Array must refuse at the 65536 cell budget, got {error:?}"
            ),
        }
    }

    // Math plus the four URI builtins plus JSON manifest, 198 nodes, so three
    // extra directory pages (1353 cells) are already charged. Measured edge:
    // N=7419 (37165 bytes) admits at 65532 cells, N=7420 (37170 bytes)
    // refuses at 65537. Slope 8 cells per element, fixed term 6180 cells.
    // Prefix "Math;encodeURI;decodeURI;encodeURIComponent;decodeURIComponent;JSON;".
    #[test]
    fn math_full_uri_json_manifest_third_page_refuses() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let prefix = "Math;encodeURI;decodeURI;encodeURIComponent;decodeURIComponent;JSON;";
        let entries = std::iter::repeat_n("JSON", 7420)
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("{prefix}[{entries}];");
        assert_eq!(source.len(), 37170, "witness JS_source_bytes must match");
        let (script, base) = super::super::parser::parse_script(&source, 0)
            .expect("Math plus full URI plus JSON manifest tagged Array must parse");
        // One parse and one lower, IR only.
        match lower_witness(&script, &source, 0, base, &context) {
            Ok(_) => panic!(
                "Math plus full URI plus JSON manifest tagged Array at N=7420 was unexpectedly admitted; the measured edge is N=7419 at 65532 cells"
            ),
            Err(error) => assert!(
                format!("{error:?}").contains("65536 cell budget"),
                "Math plus full URI plus JSON manifest tagged Array must refuse at the 65536 cell budget, got {error:?}"
            ),
        }
    }

    // The same measured Math-plus-four-URI-plus-JSON edge under the
    // near-boundary name: N=7420 (37170 bytes) refuses at 65537; N=7419
    // admits at 65532. Slope 8 cells per element, fixed term 6180 cells.
    #[test]
    fn math_full_uri_json_manifest_third_page_near_boundary_refuses() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let prefix = "Math;encodeURI;decodeURI;encodeURIComponent;decodeURIComponent;JSON;";
        let entries = std::iter::repeat_n("JSON", 7420)
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("{prefix}[{entries}];");
        assert_eq!(source.len(), 37170, "witness JS_source_bytes must match");
        let (script, base) = super::super::parser::parse_script(&source, 0)
            .expect("Math plus full URI plus JSON manifest near-boundary tagged Array must parse");
        // One parse and one lower, IR only.
        match lower_witness(&script, &source, 0, base, &context) {
            Ok(_) => panic!(
                "Math plus full URI plus JSON manifest near-boundary tagged Array was unexpectedly admitted; the 65536 cell budget must refuse N=7420"
            ),
            Err(error) => assert!(
                format!("{error:?}").contains("65536 cell budget"),
                "Math plus full URI plus JSON manifest near-boundary tagged Array must refuse at the 65536 cell budget, got {error:?}"
            ),
        }
    }

    // The same measured Math-plus-four-URI-plus-JSON edge under the
    // adjacent-lower-source name: N=7420 (37170 bytes) refuses at 65537;
    // N=7419 admits at 65532. Slope 8 cells per element, fixed term 6180
    // cells.
    #[test]
    fn math_full_uri_json_manifest_third_page_adjacent_lower_source_refuses() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let prefix = "Math;encodeURI;decodeURI;encodeURIComponent;decodeURIComponent;JSON;";
        let entries = std::iter::repeat_n("JSON", 7420)
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("{prefix}[{entries}];");
        assert_eq!(source.len(), 37170, "witness JS_source_bytes must match");
        let (script, base) = super::super::parser::parse_script(&source, 0)
            .expect("Math plus full URI plus JSON manifest adjacent lower tagged Array must parse");
        // One parse and one lower, IR only.
        match lower_witness(&script, &source, 0, base, &context) {
            Ok(_) => panic!(
                "Math plus full URI plus JSON manifest adjacent lower tagged Array was unexpectedly admitted; the 65536 cell budget must refuse N=7420"
            ),
            Err(error) => assert!(
                format!("{error:?}").contains("65536 cell budget"),
                "Math plus full URI plus JSON manifest adjacent lower tagged Array must refuse at the 65536 cell budget, got {error:?}"
            ),
        }
    }

    // One URI only manifest, 129 nodes, so the second directory page is
    // already charged. Measured edge: N=7640 (76412 bytes) admits at 65535
    // cells, N=7641 (76422 bytes) refuses at 65537. Slope 8 cells per element (each
    // element is the managed `encodeURI` reference), fixed term 4415 cells.
    // Prefix "encodeURI;".
    #[test]
    fn uri_only_manifest_selected_manifest_second_page_refuses() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let prefix = "encodeURI;";
        let entries = std::iter::repeat_n("encodeURI", 7641)
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("{prefix}[{entries}];");
        assert_eq!(source.len(), 76422, "witness JS_source_bytes must match");
        let (script, base) = super::super::parser::parse_script(&source, 0)
            .expect("one-URI selected-manifest tagged Array must parse");
        // One parse and one lower, IR only.
        match lower_witness(&script, &source, 0, base, &context) {
            Ok(_) => panic!(
                "one-URI tagged Array at N=7641 was unexpectedly admitted; the measured edge is N=7640 at 65535 cells"
            ),
            Err(error) => assert!(
                format!("{error:?}").contains("65536 cell budget"),
                "one-URI selected-manifest tagged Array must refuse at the 65536 cell budget, got {error:?}"
            ),
        }
    }

    // The same measured one-URI edge under the nearby name: N=7641
    // (76422 bytes) refuses; N=7640 admits at 65535. Slope 8 cells per
    // element, fixed term 4415 cells; the manifest already holds the
    // 451-cell second page, so this family has one edge.
    #[test]
    fn uri_only_manifest_selected_manifest_second_page_nearby_refuses() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let prefix = "encodeURI;";
        let entries = std::iter::repeat_n("encodeURI", 7641)
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("{prefix}[{entries}];");
        assert_eq!(source.len(), 76422, "witness JS_source_bytes must match");
        let (script, base) = super::super::parser::parse_script(&source, 0)
            .expect("one-URI selected-manifest nearby tagged Array must parse");
        // One parse and one lower, IR only.
        match lower_witness(&script, &source, 0, base, &context) {
            Ok(_) => panic!(
                "one-URI selected-manifest nearby tagged Array was unexpectedly admitted; the 65536 cell budget must refuse N=7641"
            ),
            Err(error) => assert!(
                format!("{error:?}").contains("65536 cell budget"),
                "one-URI selected-manifest nearby tagged Array must refuse at the 65536 cell budget, got {error:?}"
            ),
        }
    }

    // Default manifest, 126 nodes, so bootstrap already owns two directory
    // pages and the tagged Array adds no page fee. Measured edge: N=7705
    // (15421 bytes) admits at 65536 cells, N=7706 (15423 bytes) refuses at
    // 65538. Slope 8 cells per element (`o` root reference), fixed term 3896 cells
    // including the "var o={};" prefix. One parse and one lower, IR only.
    #[test]
    fn default_manifest_bootstrap_owned_page_empty_object_refuses() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let prefix = "var o={};";
        let entries = std::iter::repeat_n("o", 7706).collect::<Vec<_>>().join(",");
        let source = format!("{prefix}[{entries}];");
        assert_eq!(source.len(), 15423, "witness JS_source_bytes must match");
        let (script, base) = super::super::parser::parse_script(&source, 0)
            .expect("default manifest bootstrap-owned-page tagged Array must parse");
        // One parse and one lower, IR only.
        match lower_witness(&script, &source, 0, base, &context) {
            Ok(_) => panic!(
                "default manifest bootstrap-owned-page tagged Array was unexpectedly admitted; the 65536 cell budget must refuse N=7706"
            ),
            Err(error) => assert!(
                format!("{error:?}").contains("65536 cell budget"),
                "default manifest bootstrap-owned-page tagged Array must refuse at the 65536 cell budget, got {error:?}"
            ),
        }
    }

    // The same measured default-manifest edge under the nearby name: N=7706
    // (15423 bytes) refuses; N=7705 admits at 65536. Slope 8 cells per
    // element, fixed term 3896 cells. Bootstrap already owns the second
    // directory page.
    #[test]
    fn default_manifest_bootstrap_owned_page_empty_object_nearby_refuses() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let prefix = "var o={};";
        let entries = std::iter::repeat_n("o", 7706).collect::<Vec<_>>().join(",");
        let source = format!("{prefix}[{entries}];");
        assert_eq!(source.len(), 15423, "witness JS_source_bytes must match");
        let (script, base) = super::super::parser::parse_script(&source, 0)
            .expect("default manifest bootstrap-owned-page nearby tagged Array must parse");
        // One parse and one lower, IR only.
        match lower_witness(&script, &source, 0, base, &context) {
            Ok(_) => panic!(
                "default manifest bootstrap-owned-page nearby tagged Array was unexpectedly admitted; the 65536 cell budget must refuse N=7706"
            ),
            Err(error) => assert!(
                format!("{error:?}").contains("65536 cell budget"),
                "default manifest bootstrap-owned-page nearby tagged Array must refuse at the 65536 cell budget, got {error:?}"
            ),
        }
    }

    // The same measured JSON edge under the new-lower-source name: N=7639
    // (38202 bytes) refuses; N=7638 admits at 65531. Slope 8 cells per
    // element, fixed term 4427 cells.
    #[test]
    fn json_manifest_bootstrap_owned_page_new_lower_source_refuses() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let prefix = "JSON;";
        let entries = std::iter::repeat_n("JSON", 7639)
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("{prefix}[{entries}];");
        assert_eq!(source.len(), 38202, "witness JS_source_bytes must match");
        let (script, base) = super::super::parser::parse_script(&source, 0)
            .expect("JSON manifest new lower tagged Array must parse");
        // One parse and one lower, IR only.
        match lower_witness(&script, &source, 0, base, &context) {
            Err(error) => assert!(format!("{error:?}").contains("65536"), "{error:?}"),
            Ok(_) => panic!("JSON manifest former admit must now exceed 65536"),
        }
    }

    // The same measured one-URI-plus-JSON edge under the new-lower-source
    // name: N=7629 (38162 bytes) refuses; N=7628 admits at 65533. Slope 8
    // cells per element, fixed term 4509 cells.
    #[test]
    fn uri_json_manifest_bootstrap_owned_page_new_lower_source_refuses() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let prefix = "encodeURI;JSON;";
        let entries = std::iter::repeat_n("JSON", 7629)
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("{prefix}[{entries}];");
        assert_eq!(source.len(), 38162, "witness JS_source_bytes must match");
        let (script, base) = super::super::parser::parse_script(&source, 0)
            .expect("URI plus JSON manifest new lower tagged Array must parse");
        // One parse and one lower, IR only.
        match lower_witness(&script, &source, 0, base, &context) {
            Err(error) => assert!(format!("{error:?}").contains("65536"), "{error:?}"),
            Ok(_) => panic!("URI plus JSON former admit must now exceed 65536"),
        }
    }

    // The same measured Math-plus-four-URI-plus-JSON edge under the
    // new-lower-source name: N=7420 (37170 bytes) refuses; N=7419 admits at
    // 65532. Slope 8 cells per element, fixed term 6180 cells.
    #[test]
    fn math_full_uri_json_manifest_bootstrap_owned_page_new_lower_source_refuses() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let prefix = "Math;encodeURI;decodeURI;encodeURIComponent;decodeURIComponent;JSON;";
        let entries = std::iter::repeat_n("JSON", 7420)
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("{prefix}[{entries}];");
        assert_eq!(source.len(), 37170, "witness JS_source_bytes must match");
        let (script, base) = super::super::parser::parse_script(&source, 0)
            .expect("Math plus full URI plus JSON manifest new lower tagged Array must parse");
        // One parse and one lower, IR only.
        match lower_witness(&script, &source, 0, base, &context) {
            Err(error) => assert!(format!("{error:?}").contains("65536"), "{error:?}"),
            Ok(_) => panic!("Math plus full URI plus JSON former admit must now exceed 65536"),
        }
    }

    // The same measured one-URI edge under the new-lower-source name: N=7641
    // (76422 bytes) refuses; N=7640 admits at 65535. Slope 8 cells per
    // element, fixed term 4415 cells.
    #[test]
    fn uri_only_manifest_bootstrap_owned_page_new_lower_source_refuses() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let prefix = "encodeURI;";
        let entries = std::iter::repeat_n("encodeURI", 7641)
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("{prefix}[{entries}];");
        assert_eq!(source.len(), 76422, "witness JS_source_bytes must match");
        let (script, base) = super::super::parser::parse_script(&source, 0)
            .expect("one-URI selected-manifest new lower tagged Array must parse");
        // One parse and one lower, IR only.
        match lower_witness(&script, &source, 0, base, &context) {
            Err(error) => assert!(format!("{error:?}").contains("65536"), "{error:?}"),
            Ok(_) => panic!("one-URI former admit must now exceed 65536"),
        }
    }

    // The same measured default-manifest edge under the new-lower-source
    // name, not a profile that recreates an older realm: N=7706 (15423 bytes)
    // refuses; N=7705 admits at 65536. Slope 8 cells per element, fixed term
    // 3896 cells.
    #[test]
    fn default_manifest_bootstrap_owned_page_new_lower_source_refuses() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let prefix = "var o={};";
        let entries = std::iter::repeat_n("o", 7706).collect::<Vec<_>>().join(",");
        let source = format!("{prefix}[{entries}];");
        assert_eq!(source.len(), 15423, "witness JS_source_bytes must match");
        let (script, base) = super::super::parser::parse_script(&source, 0)
            .expect("default manifest bootstrap-owned-page new lower tagged Array must parse");
        // One parse and one lower, IR only.
        match lower_witness(&script, &source, 0, base, &context) {
            Err(error) => assert!(format!("{error:?}").contains("65536"), "{error:?}"),
            Ok(_) => panic!("default manifest former admit must now exceed 65536"),
        }
    }

    // Admit side of the four-URI family edge: 15252 units per literal,
    // 61078 source bytes, 65534 cells (two remaining), so the family has
    // exactly one edge and the former "nearby source" refusal at 15253 was
    // the same witness as the refusal test above (removed, not duplicated).
    //
    // The zero-margin expect-to-flip tripwire moved to the default family
    // (`default_case_shift_array_admits`), which now lands on exactly 0
    // remaining cells. Any constant that gains even one cell flips it.
    #[test]
    fn full_uri_case_shift_nearby_admits() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let padding = "a".repeat(15252);
        let source = format!(
            "encodeURI;decodeURI;encodeURIComponent;decodeURIComponent;\"{padding}\";\"{padding}\";\"{padding}\";\"{padding}\";"
        );
        assert_eq!(source.len(), 61078, "witness JS_source_bytes must match");
        let (script, base) = super::super::parser::parse_script(&source, 0)
            .expect("four 15252-unit full-URI String statements must parse");
        lower_witness(&script, &source, 0, base, &context)
            .expect("four 15252-unit full-URI String literals must stay within 65536");
    }

    fn case_shift_array_admits(prefix: &str, item: &str, count: usize, bytes: usize) {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let entries = std::iter::repeat_n(item, count).collect::<Vec<_>>().join(",");
        let source = format!("{prefix}[{entries}];");
        assert_eq!(source.len(), bytes, "witness JS_source_bytes must match");
        let (script, base) = super::super::parser::parse_script(&source, 0)
            .expect("case-shift nearby Array must parse");
        lower_witness(&script, &source, 0, base, &context)
            .expect("case-shift nearby Array must stay within 65536");
    }

    #[test]
    fn json_case_shift_array_admits() {
        case_shift_array_admits("JSON;", "JSON", 7638, 38197);
    }

    #[test]
    fn uri_json_case_shift_array_admits() {
        case_shift_array_admits("encodeURI;JSON;", "JSON", 7628, 38157);
    }

    #[test]
    fn math_case_shift_array_admits() {
        case_shift_array_admits(
            "Math;encodeURI;decodeURI;encodeURIComponent;decodeURIComponent;JSON;",
            "JSON",
            7419,
            37165,
        );
    }

    #[test]
    fn uri_only_case_shift_array_admits() {
        case_shift_array_admits("encodeURI;", "encodeURI", 7640, 76412);
    }

    #[test]
    fn default_case_shift_array_admits() {
        case_shift_array_admits("var o={};", "o", 7705, 15421);
    }

    // The cell totals quoted in the comments above are measurements, not
    // derivations, so pin each admit-side edge: every family's last admitted N
    // must land on exactly the documented total, and the default family on
    // exactly zero remaining cells (the new expect-to-flip tripwire).
    #[test]
    fn measured_edge_totals_match_documented_cell_counts() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let array_total = |prefix: &str, item: &str, n: usize| {
            let entries = std::iter::repeat_n(item, n).collect::<Vec<_>>().join(",");
            let source = format!("{prefix}[{entries}];");
            let (script, base) = super::super::parser::parse_script(&source, 0).unwrap();
            let _ = lower_witness(&script, &source, 0, base, &context).unwrap();
            super::realm_budget_witness().0
        };
        assert_eq!(array_total("var o={};", "o", 7705), 65536, "default manifest edge");
        assert_eq!(array_total("JSON;", "JSON", 7638), 65531, "JSON manifest edge");
        assert_eq!(array_total("encodeURI;JSON;", "JSON", 7628), 65533, "one-URI-plus-JSON edge");
        assert_eq!(
            array_total(
                "Math;encodeURI;decodeURI;encodeURIComponent;decodeURIComponent;JSON;",
                "JSON",
                7419
            ),
            65532,
            "Math-plus-four-URI-plus-JSON edge"
        );
        assert_eq!(array_total("encodeURI;", "encodeURI", 7640), 65535, "one-URI edge");
        let padding = "a".repeat(15252);
        let source = format!(
            "encodeURI;decodeURI;encodeURIComponent;decodeURIComponent;\"{padding}\";\"{padding}\";\"{padding}\";\"{padding}\";"
        );
        let (script, base) = super::super::parser::parse_script(&source, 0).unwrap();
        let _ = lower_witness(&script, &source, 0, base, &context).unwrap();
        assert_eq!(
            super::realm_budget_witness().0,
            65534,
            "four-URI family edge lands on two remaining cells"
        );
    }
}
