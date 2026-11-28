//! Sealed primitive binary-Add operand pairs. Strings never enter numeric parsing.
use super::super::prelude::object::{assign, branch, call, eq, int, or, var};
use super::*;
use std::cell::{Cell, RefCell};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Number,
    Boolean,
    Null,
    Undefined,
    String,
    Dynamic,
}
impl Mode {
    fn from_kind(kind: Kind) -> Option<Self> {
        Some(match kind {
            Kind::Number => Self::Number,
            Kind::Boolean => Self::Boolean,
            Kind::Null => Self::Null,
            Kind::Undefined => Self::Undefined,
            Kind::String => Self::String,
            Kind::Dynamic => Self::Dynamic,
            _ => return None,
        })
    }
    fn kind(self) -> Kind {
        match self {
            Self::Number => Kind::Number,
            Self::Boolean => Kind::Boolean,
            Self::Null => Kind::Null,
            Self::Undefined => Kind::Undefined,
            Self::String => Kind::String,
            Self::Dynamic => Kind::Dynamic,
        }
    }
    fn string(self) -> bool {
        matches!(self, Self::String | Self::Dynamic)
    }
    fn number(self) -> bool {
        matches!(self, Self::Number | Self::Dynamic)
    }
    fn fixed(self) -> bool {
        matches!(
            self,
            Self::Boolean | Self::Null | Self::Undefined | Self::Dynamic
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Left,
    Right,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pair {
    op: ast::BinaryOp,
    left: (Role, Mode),
    right: (Role, Mode),
}
impl Pair {
    fn managed(self) -> bool {
        self.left.1.string() || self.right.1.string()
    }
    fn number(self) -> bool {
        (self.left.1.string() && self.right.1.number())
            || (self.right.1.string() && self.left.1.number())
    }
    fn fixed(self) -> bool {
        (self.left.1.string() && self.right.1.fixed())
            || (self.right.1.string() && self.left.1.fixed())
    }
    fn result(self) -> Kind {
        if self.left.1 == Mode::String || self.right.1 == Mode::String {
            Kind::String
        } else if self.left.1 == Mode::Dynamic || self.right.1 == Mode::Dynamic {
            Kind::Dynamic
        } else {
            Kind::Number
        }
    }
    /// Operand-wise join used only while collecting validation facts: a
    /// widened fact set (for example a loop back edge making an accumulator
    /// Dynamic) must stay expressible when the same source site is walked
    /// again. Dynamic is the top primitive mode.
    fn join(self, other: Self) -> Self {
        let join_mode = |left: Mode, right: Mode| {
            if left == right || right == Mode::Dynamic {
                right
            } else {
                Mode::Dynamic
            }
        };
        Pair {
            op: self.op,
            left: (Role::Left, join_mode(self.left.1, other.left.1)),
            right: (Role::Right, join_mode(self.right.1, other.right.1)),
        }
    }
    /// Emission must be at least as precise as the sealed pair; a Dynamic
    /// sealed operand accepts any concrete primitive mode because its
    /// runtime arm already checks the tag.
    fn refines(self, sealed: Self) -> bool {
        let covers = |actual: Mode, sealed: Mode| actual == sealed || sealed == Mode::Dynamic;
        self.op == sealed.op
            && self.left.0 == sealed.left.0
            && self.right.0 == sealed.right.0
            && covers(self.left.1, sealed.left.1)
            && covers(self.right.1, sealed.right.1)
    }
}
#[derive(Default)]
pub(super) struct State {
    sites: RefCell<HashMap<(u32, u32), Pair>>,
    sealed: Cell<bool>,
}
fn register(state: &State, site: (u32, u32), pair: Pair) -> Result<Pair, &'static str> {
    if pair.op != ast::BinaryOp::Add || pair.left.0 != Role::Left || pair.right.0 != Role::Right {
        return Err("Primitive addition requires exact binary Add operand roles");
    }
    let mut sites = state.sites.borrow_mut();
    if let Some(old) = sites.get(&site).copied()
        && old != pair
    {
        if state.sealed.get() {
            // Emission: the actual operand modes may be narrower than the
            // sealed dynamic pair, but never wider.
            return if pair.refines(old) {
                Ok(old)
            } else {
                Err("Primitive addition operand pair changed")
            };
        }
        // Validation walks the same site under refined/joined facts (loop
        // back edges). Keep the least upper bound instead of rejecting the
        // legitimate widening.
        let joined = old.join(pair);
        sites.insert(site, joined);
        return Ok(joined);
    }
    if state.sealed.get() && sites.get(&site) != Some(&pair) {
        return Err("Primitive addition operand pair is not sealed");
    }
    sites.insert(site, pair);
    Ok(pair)
}

impl Lowerer<'_> {
    pub(in crate::javascript) fn record_primitive_addition_site(
        &self,
        binary: &ast::BinExpr,
        left: Kind,
        right: Kind,
    ) -> Result<Kind, ParseError> {
        let mode = |kind| {
            Mode::from_kind(kind).ok_or_else(|| {
                self.error(binary.span, "P0 arithmetic currently requires numeric operands: binary Add requires primitive operands")
            })
        };
        let pair = Pair {
            op: binary.op,
            left: (Role::Left, mode(left)?),
            right: (Role::Right, mode(right)?),
        };
        let settled = register(
            &self.primitive_addition,
            (binary.span.lo.0, binary.span.hi.0),
            pair,
        )
        .map_err(|message| self.error(binary.span, message))?;
        // Validation may walk one source site again under joined loop facts.
        // Publish the canonical least-upper-bound pair's result so the fact
        // for this expression is stable across walks and never narrower than
        // an earlier walk already admitted.
        Ok(settled.result())
    }
    pub(super) fn seal_primitive_addition_sites(&self) {
        self.primitive_addition.sealed.set(true);
    }
    pub(super) fn has_primitive_addition_site(&self, binary: &ast::BinExpr) -> bool {
        self.primitive_addition.sealed.get()
            && self
                .primitive_addition
                .sites
                .borrow()
                .contains_key(&(binary.span.lo.0, binary.span.hi.0))
    }
    pub(super) fn addition_string_from_kind(
        &mut self,
        input: u16,
        output: u16,
        kind: Kind,
        span: swc_common::Span,
    ) -> Result<Vec<Stmt>, ParseError> {
        let mode = Mode::from_kind(kind).ok_or_else(|| {
            self.error(span, "Add string conversion requires a primitive operand")
        })?;
        self.addition_string_conversion(input, output, mode, span)
    }
    pub(super) fn addition_needs_strings(&self) -> bool {
        self.primitive_addition
            .sites
            .borrow()
            .values()
            .any(|pair| pair.managed())
            || self.mutation_needs_strings()
    }
    pub(super) fn addition_needs_formatter(&self) -> bool {
        self.primitive_addition
            .sites
            .borrow()
            .values()
            .any(|pair| pair.number())
            || self.mutation_needs_formatter()
    }
    pub(super) fn addition_needs_fixed_strings(&self) -> bool {
        self.primitive_addition
            .sites
            .borrow()
            .values()
            .any(|pair| pair.fixed())
            || self.mutation_needs_fixed_strings()
    }
    fn addition_pair(&self, binary: &ast::BinExpr) -> Result<Pair, ParseError> {
        let pair = self
            .primitive_addition
            .sites
            .borrow()
            .get(&(binary.span.lo.0, binary.span.hi.0))
            .copied()
            .ok_or_else(|| {
                self.error(binary.span, "Primitive addition operand pair is not sealed")
            })?;
        if !self.primitive_addition.sealed.get() {
            return Err(self.error(binary.span, "Primitive addition operand pair is not sealed"));
        }
        let actual = Pair {
            op: binary.op,
            ..pair
        };
        register(
            &self.primitive_addition,
            (binary.span.lo.0, binary.span.hi.0),
            actual,
        )
        .map_err(|message| self.error(binary.span, message))?;
        Ok(pair)
    }
    // This authorization is distinct from unary Plus and full numeric Dynamic.
    // Dynamic here is a non-String primitive, only inside the checked Add arm.
    fn addition_non_string_projection(
        &self,
        binary: &ast::BinExpr,
        role: Role,
        slot: u16,
    ) -> Result<Expr, ParseError> {
        let pair = self.addition_pair(binary)?;
        let mode = match role {
            Role::Left => pair.left.1,
            Role::Right => pair.right.1,
        };
        primitive_number::non_string_projection(slot, mode.kind()).ok_or_else(|| {
            self.error(
                binary.span,
                "String cannot enter binary Add numeric projection",
            )
        })
    }
    fn addition_string_conversion(
        &mut self,
        input: u16,
        output: u16,
        mode: Mode,
        span: swc_common::Span,
    ) -> Result<Vec<Stmt>, ParseError> {
        let line = self.line(span);
        let c = self.active_carrier(span)?;
        let frame = self.active_frame(span)?;
        let entry = self.entry_for(output, span)?;
        let convert = |helper, value| {
            assign(
                output,
                Self::guest_call(
                    helper,
                    vec![var(c), value, var(frame), int(i64::from(entry))],
                ),
                line,
            )
        };
        let numeric = if mode.number() {
            let helper = self
                .number_string_entries
                .as_ref()
                .ok_or_else(|| {
                    self.error(
                        span,
                        "Add formatter was not selected before runtime indices",
                    )
                })?
                .managed;
            vec![convert(
                helper,
                Self::as_number(super::super::prelude::math::checked_number(var(input))),
            )]
        } else {
            vec![]
        };
        let fixed = if mode.fixed() {
            vec![convert(
                self.helper_fixed_scalar_string.ok_or_else(|| {
                    self.error(
                        span,
                        "Add fixed producer was not selected before runtime indices",
                    )
                })?,
                var(input),
            )]
        } else {
            vec![]
        };
        let mut stmts = match mode {
            Mode::String => vec![assign(output, var(input), line)],
            Mode::Number => numeric,
            Mode::Boolean | Mode::Null | Mode::Undefined => fixed,
            Mode::Dynamic => vec![branch(
                eq(value::field(input, "tag"), int(value::STRING)),
                vec![assign(output, var(input), line)],
                vec![branch(
                    eq(value::field(input, "tag"), int(value::NUMBER)),
                    numeric,
                    fixed,
                    line,
                )],
                line,
            )],
        };
        self.refresh_root(output, span, &mut stmts)?;
        Ok(stmts)
    }
    pub(super) fn lower_primitive_addition(
        &mut self,
        binary: &ast::BinExpr,
    ) -> Result<Lowered, ParseError> {
        let pair = self.addition_pair(binary)?;
        let span = binary.span;
        let line = self.line(span);
        let managed = pair.managed();
        // Result and conversion publication entries dominate every allocation.
        let (out, mut stmts) = if managed {
            self.bind_rooted(value::undefined(), value::schema(), span)?
        } else {
            let (out, init) = self.bind_temp_bare(value::number(Expr::Float(0.0)), span)?;
            (out, vec![init])
        };
        let converted = if managed {
            let (a, mut init) = self.bind_rooted(value::undefined(), value::schema(), span)?;
            stmts.append(&mut init);
            let (b, mut init) = self.bind_rooted(value::undefined(), value::schema(), span)?;
            stmts.append(&mut init);
            Some((a, b))
        } else {
            None
        };
        let left = self.expr(&binary.left)?;
        let right = self.expr(&binary.right)?;
        let mut consumed = left.owned_roots;
        consumed.extend(right.owned_roots);
        let (l, mut ls) = self.bind_evaluation(left.expr, left.kind, binary.left.span())?;
        stmts.append(&mut ls);
        let (r, mut rhs) = self.bind_evaluation(right.expr, right.kind, binary.right.span())?;
        consumed.extend([l, r].into_iter().filter(|slot| self.has_root(*slot)));
        let mut compute = Vec::new();
        for (slot, mode) in [(l, pair.left.1), (r, pair.right.1)] {
            if mode == Mode::Dynamic {
                let is = |tag| eq(value::field(slot, "tag"), int(tag));
                let primitive = or(
                    or(is(value::NUMBER), is(value::BOOLEAN)),
                    or(or(is(value::NULL), is(value::UNDEFINED)), is(value::STRING)),
                );
                compute.push(Stmt::Expr {
                    expr: call(BuiltinFunction::Assert.call_index(), vec![primitive]),
                    line,
                });
            }
        }
        let mut numeric = Vec::new();
        if pair.left.1 != Mode::String && pair.right.1 != Mode::String {
            let (a, init) = self.bind_typed(
                self.addition_non_string_projection(binary, Role::Left, l)?,
                TypeSchema::Float,
                span,
            )?;
            numeric.push(init);
            let (b, init) = self.bind_typed(
                self.addition_non_string_projection(binary, Role::Right, r)?,
                TypeSchema::Float,
                span,
            )?;
            numeric.push(init);
            numeric.push(assign(
                out,
                value::number(Expr::Add(Box::new(var(a)), Box::new(var(b)))),
                line,
            ));
            if managed {
                self.refresh_root(out, span, &mut numeric)?;
            }
        }
        if let Some((a, b)) = converted {
            // In this arm a lone possible String side is necessarily String.
            // Do not emit unreachable converters that would borrow unselected demand.
            let left_mode = if !pair.right.1.string() {
                Mode::String
            } else {
                pair.left.1
            };
            let right_mode = if !pair.left.1.string() {
                Mode::String
            } else {
                pair.right.1
            };
            let mut concat = self.addition_string_conversion(l, a, left_mode, span)?;
            concat.extend(self.addition_string_conversion(r, b, right_mode, span)?);
            let helper = self.helper_string_concat.ok_or_else(|| {
                self.error(
                    span,
                    "Add concat helper was not selected before runtime indices",
                )
            })?;
            let c = self.active_carrier(span)?;
            let frame = self.active_frame(span)?;
            let entry = self.entry_for(out, span)?;
            concat.push(assign(
                out,
                Self::guest_call(
                    helper,
                    vec![var(c), var(a), var(b), var(frame), int(i64::from(entry))],
                ),
                line,
            ));
            self.refresh_root(out, span, &mut concat)?;
            if pair.left.1 == Mode::String || pair.right.1 == Mode::String {
                compute.extend(concat);
            } else {
                compute.push(branch(
                    or(
                        eq(value::field(l, "tag"), int(value::STRING)),
                        eq(value::field(r, "tag"), int(value::STRING)),
                    ),
                    concat,
                    numeric,
                    line,
                ));
            }
            consumed.extend([a, b]);
        } else {
            compute.extend(numeric);
        }
        // Both GetValues must finish normally before either domain guard runs.
        if let Some(normal) = self.pending_normal {
            rhs.push(branch(var(normal), compute, vec![], line));
            stmts.push(branch(var(normal), rhs, vec![], line));
        } else {
            stmts.append(&mut rhs);
            stmts.extend(compute);
        }
        for slot in Self::compose_owned(consumed.into_iter().filter(|slot| self.has_root(*slot))) {
            self.clear_root(slot, span, &mut stmts)?;
        }
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(var(out)),
            },
            kind: pair.result(),
            array_length: None,
            owned_roots: if managed { vec![out] } else { vec![] },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn addition140_sealed_pair_roles_mode_operator_and_empty() {
        let state = State::default();
        let pair = Pair {
            op: ast::BinaryOp::Add,
            left: (Role::Left, Mode::Dynamic),
            right: (Role::Right, Mode::Number),
        };
        register(&state, (1, 9), pair).unwrap();
        register(&state, (1, 9), pair).unwrap();
        state.sealed.set(true);
        assert_eq!(register(&state, (1, 9), pair).unwrap(), pair);
        assert!(register(&state, (2, 9), pair).is_err());
        // An actual Boolean left refines the sealed Dynamic left: the sealed
        // runtime arm already checks the tag, so this is accepted and the
        // canonical sealed pair must stay untouched.
        let refined = register(
            &state,
            (1, 9),
            Pair {
                left: (Role::Left, Mode::Boolean),
                ..pair
            },
        )
        .unwrap();
        assert_eq!(refined, pair, "refinement must not rewrite the sealed pair");
        assert_eq!(state.sites.borrow().get(&(1, 9)), Some(&pair));
        assert!(
            register(
                &state,
                (1, 9),
                Pair {
                    right: (Role::Left, Mode::Number),
                    ..pair
                }
            )
            .is_err()
        );
        assert!(
            register(
                &state,
                (1, 9),
                Pair {
                    op: ast::BinaryOp::Sub,
                    ..pair
                }
            )
            .is_err()
        );
        // A sealed exact pair must never widen to Dynamic at emission.
        let exact = State::default();
        let numbers = Pair {
            op: ast::BinaryOp::Add,
            left: (Role::Left, Mode::Number),
            right: (Role::Right, Mode::Number),
        };
        register(&exact, (3, 7), numbers).unwrap();
        exact.sealed.set(true);
        assert!(
            register(
                &exact,
                (3, 7),
                Pair {
                    left: (Role::Left, Mode::Dynamic),
                    ..numbers
                }
            )
            .is_err()
        );
        assert_eq!(state.sites.borrow().len(), 1);
        let empty = State::default();
        empty.sealed.set(true);
        assert!(register(&empty, (1, 9), pair).is_err());
        assert!(empty.sites.borrow().is_empty());
    }
    #[test]
    fn addition140_validation_join_keeps_canonical_result() {
        let state = State::default();
        let entry = Pair {
            op: ast::BinaryOp::Add,
            left: (Role::Left, Mode::Number),
            right: (Role::Right, Mode::String),
        };
        assert_eq!(
            register(&state, (4, 11), entry).unwrap().result(),
            Kind::String
        );
        // The loop back edge arrives with a numeric right operand. Its single
        // covering mode is Dynamic, so the canonical pair widens once and the
        // published result must be the canonical Dynamic, never the narrower
        // incoming Number.
        let narrowed = Pair {
            right: (Role::Right, Mode::Number),
            ..entry
        };
        let settled = Pair {
            right: (Role::Right, Mode::Dynamic),
            ..entry
        };
        assert_eq!(
            register(&state, (4, 11), narrowed).unwrap(),
            settled,
            "join must cover both validation walks"
        );
        assert_eq!(
            settled.result(),
            Kind::Dynamic,
            "the narrow walk cannot publish Number"
        );
        // Once settled, another walk in either direction returns the same
        // canonical pair and never shrinks it.
        assert_eq!(register(&state, (4, 11), narrowed).unwrap(), settled);
        assert_eq!(register(&state, (4, 11), entry).unwrap(), settled);
        assert_eq!(state.sites.borrow().len(), 1);
    }
    #[test]
    fn addition140_pair_facts_and_precise_demand() {
        for left in [
            Mode::Number,
            Mode::Boolean,
            Mode::Null,
            Mode::Undefined,
            Mode::String,
            Mode::Dynamic,
        ] {
            for right in [
                Mode::Number,
                Mode::Boolean,
                Mode::Null,
                Mode::Undefined,
                Mode::String,
                Mode::Dynamic,
            ] {
                let p = Pair {
                    op: ast::BinaryOp::Add,
                    left: (Role::Left, left),
                    right: (Role::Right, right),
                };
                assert_eq!(p.managed(), left.string() || right.string());
                assert!(
                    p.result()
                        == if left == Mode::String || right == Mode::String {
                            Kind::String
                        } else if left == Mode::Dynamic || right == Mode::Dynamic {
                            Kind::Dynamic
                        } else {
                            Kind::Number
                        }
                );
            }
        }
        assert!(Mode::from_kind(Kind::Object).is_none());
        assert!(Mode::from_kind(Kind::Array).is_none());
        assert!(Mode::from_kind(Kind::Function).is_none());
    }
    #[test]
    fn addition140_actual_function_scope_and_strict_restore() {
        for (source, fails) in [
            (
                "function f(x){'use strict';return x+true;}var o={'\\141':1};",
                false,
            ),
            (
                "function f(x){'use strict';x+true;return 'x'+({});}var o={'\\141':1};",
                true,
            ),
            (
                "function f(x){function g(y){'use strict';return y+null;}return x+1;}var o={'\\141':1};",
                false,
            ),
        ] {
            let source_id = 0;
            let (script, base) =
                crate::javascript::parser::parse_script(source, source_id).unwrap();
            let mut lower = Lowerer {
                globals: crate::javascript::global_binding::Plan::default(),
                global_helpers: None,
                global_env: None,
                source,
                source_id,
                base,
                next_slot: 0,
                bindings: HashMap::new(),
                array_lengths: HashMap::new(),
                array_length_ids: HashMap::new(),
                array_names: std::collections::HashSet::new(),
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
                primitive_number: primitive_number::State::default(),
                primitive_addition: primitive_addition::State::default(),
                mutation: primitive_assignment::State::default(),
                abstract_equality: abstract_equality::State::default(),
                relational_sites: std::cell::RefCell::new(HashMap::new()),
                relational_sealed: std::cell::Cell::new(false),
                helper_string_compare: None,
                number_key_sites: std::cell::RefCell::new(std::collections::HashSet::new()),
                number_key_sealed: std::cell::Cell::new(false),
                number_string_entries: None,
                primitive_key: super::primitive_key::State::default(),
                init_cells: 0,
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
                arguments_array_from_values: None,
                arguments_string_key: None,
                arguments_sync: None,
                arguments_object: None,
                carrier_override: None,
                frame_override: None,
                root_scope: None,
                helper_decode: None,
                helper_ref_get: None,
                helper_dispatch: None,
                helper_int_identity: None,
                function_code_ids: HashMap::new(),
                collected_functions: Vec::new(),
                graph: scope::build(&script, source, source_id, base).unwrap(),
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
            lower.active_scope = lower.graph.script;
            let scope = lower.active_scope;
            collect_source_functions(&script, &mut lower, false).unwrap();
            let result = lower.collect_primitive_number_function_sites();
            assert_eq!(result.is_err(), fails);
            assert_eq!(lower.active_scope, scope);
            assert!(!lower.enclosing_strict);
            assert!(!lower.primitive_addition.sites.borrow().is_empty());
            let ast::Stmt::Decl(ast::Decl::Var(v)) = &script.body[1] else {
                panic!("var")
            };
            let ast::Expr::Object(o) = v.decls[0].init.as_deref().unwrap() else {
                panic!("object")
            };
            let ast::PropOrSpread::Prop(prop) = &o.props[0] else {
                panic!("prop")
            };
            let ast::Prop::KeyValue(kv) = prop.as_ref() else {
                panic!("key")
            };
            assert_eq!(
                lower.object_key_units(&kv.key, kv.key.span()).unwrap().0,
                vec![97]
            );
        }
    }
}
