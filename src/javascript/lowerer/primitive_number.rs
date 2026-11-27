//! Sealed primitive numeric conversions and shared private parser demand.
use super::super::prelude::object::{assign, branch, call, choose, var};
use super::*;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(in crate::javascript) enum Operator {
    Plus,
    Neg,
    Mul,
    Sub,
    Div,
    Exp,
    Remainder,
    Not,
    And,
    Or,
    Xor,
    LeftShift,
    RightShift,
    UnsignedRightShift,
    RelationalLt,
    RelationalGt,
    RelationalLe,
    RelationalGe,
    AbstractEq,
    AbstractNe,
}

impl Operator {
    pub(in crate::javascript) fn binary(op: ast::BinaryOp) -> Option<Self> {
        Some(match op {
            ast::BinaryOp::Mod => Self::Remainder,
            ast::BinaryOp::BitAnd => Self::And,
            ast::BinaryOp::BitOr => Self::Or,
            ast::BinaryOp::BitXor => Self::Xor,
            ast::BinaryOp::LShift => Self::LeftShift,
            ast::BinaryOp::RShift => Self::RightShift,
            ast::BinaryOp::ZeroFillRShift => Self::UnsignedRightShift,
            _ => return Self::arithmetic(op),
        })
    }
    pub(in crate::javascript) fn arithmetic(op: ast::BinaryOp) -> Option<Self> {
        Some(match op {
            ast::BinaryOp::Sub => Self::Sub,
            ast::BinaryOp::Mul => Self::Mul,
            ast::BinaryOp::Div => Self::Div,
            ast::BinaryOp::Exp => Self::Exp,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(in crate::javascript) enum Role {
    Unary,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::javascript) enum Mode {
    Dynamic,
    Number,
    Boolean,
    Null,
    Undefined,
    String,
}

impl Mode {
    pub(super) fn kind(self) -> Kind {
        match self {
            Self::Dynamic => Kind::Dynamic,
            Self::Number => Kind::Number,
            Self::Boolean => Kind::Boolean,
            Self::Null => Kind::Null,
            Self::Undefined => Kind::Undefined,
            Self::String => Kind::String,
        }
    }
    pub(in crate::javascript) fn from_kind(kind: Kind) -> Option<Self> {
        Some(match kind {
            Kind::Dynamic => Self::Dynamic,
            Kind::Number => Self::Number,
            Kind::Boolean => Self::Boolean,
            Kind::Null => Self::Null,
            Kind::Undefined => Self::Undefined,
            Kind::String => Self::String,
            _ => return None,
        })
    }
}

type Site = (u32, u32, Operator, Role);

// Pure scalar construction shared with the separately sealed binary Add arm.
// Dynamic is accepted only after that caller excluded String/nonprimitives;
// ordinary numeric Dynamic keeps its original guarded parser path below.
pub(super) fn non_string_projection(slot: u16, kind: Kind) -> Option<Expr> {
    use super::super::prelude::object::eq;
    Some(match kind {
        Kind::Number => value::field(slot, "number"),
        Kind::Boolean => choose(
            value::field(slot, "flag"),
            Expr::Float(1.0),
            Expr::Float(0.0),
        ),
        Kind::Null => Expr::Float(0.0),
        Kind::Undefined => Expr::Float(f64::NAN),
        Kind::Dynamic => {
            let is = |tag| eq(value::field(slot, "tag"), Expr::Int(tag));
            choose(
                is(value::NUMBER),
                Expr::Div(
                    Box::new(value::field(slot, "number")),
                    Box::new(Expr::Float(1.0)),
                ),
                choose(
                    is(value::BOOLEAN),
                    non_string_projection(slot, Kind::Boolean)?,
                    choose(is(value::NULL), Expr::Float(0.0), Expr::Float(f64::NAN)),
                ),
            )
        }
        _ => return None,
    })
}

#[derive(Default)]
pub(super) struct State {
    sites: RefCell<HashSet<(u32, u32)>>,
    conversions: RefCell<HashMap<Site, Mode>>,
    sealed: Cell<bool>,
    parser: Option<u16>,
    // The entry walk of a loop discovers effects; only its conservative
    // back-edge walk may publish relational pair/conversion authorization.
    relational_probe: Cell<bool>,
    // Unreachable text is capability-checked without sealing a conversion and
    // without publishing facts. Emission skips those statement spans.
    dead_capability: Cell<bool>,
    dead_spans: RefCell<HashSet<(u32, u32)>>,
}

fn register(
    sites: &mut HashSet<(u32, u32)>,
    sealed: bool,
    site: (u32, u32),
) -> Result<(), &'static str> {
    if sealed && !sites.contains(&site) {
        return Err(
            "Primitive numeric managed site was not proved before the runtime index window sealed",
        );
    }
    sites.insert(site);
    Ok(())
}

struct EmissionCheckpoint {
    facts: super::expression::BranchFacts,
    effects: super::primitive_key::EffectSnapshot,
    type_names: Option<super::super::reference::TypeNames>,
    dead_capability: bool,
}

impl Lowerer<'_> {
    pub(in crate::javascript) fn probe_relational_collection<T>(
        &self,
        validate: impl FnOnce() -> Result<T, ParseError>,
    ) -> Result<T, ParseError> {
        let previous = self.primitive_number.relational_probe.replace(true);
        let result = validate();
        self.primitive_number.relational_probe.set(previous);
        result
    }
    pub(super) fn relational_collection_is_probe(&self) -> bool {
        self.primitive_number.relational_probe.get()
    }
    pub(in crate::javascript) fn probe_dead_capability<T>(
        &self,
        validate: impl FnOnce() -> Result<T, ParseError>,
    ) -> Result<T, ParseError> {
        let previous = self.primitive_number.dead_capability.replace(true);
        let result = validate();
        self.primitive_number.dead_capability.set(previous);
        result
    }

    /// Discarded dead-suffix emission. Captures the live region's liveness
    /// facts, runs the probe with fresh throw effects, and restores on both
    /// success and error. Nested calls restore the previous dead mode rather
    /// than forcing it off. Slot, helper, and site allocation stay monotone.
    pub(in crate::javascript) fn speculate_dead<T>(
        &mut self,
        body: impl FnOnce(&mut Self) -> Result<T, ParseError>,
    ) -> Result<T, ParseError> {
        let saved = EmissionCheckpoint {
            facts: super::expression::BranchFacts::save(self),
            effects: self.capture_emission_effects(),
            type_names: self.type_names,
            dead_capability: self.primitive_number.dead_capability.get(),
        };
        self.primitive_number.dead_capability.set(true);
        self.freshen_emission_effects();
        let result = body(self);
        self.primitive_number
            .dead_capability
            .set(saved.dead_capability);
        self.type_names = saved.type_names;
        self.restore_emission_effects(saved.effects);
        saved.facts.restore(self);
        result
    }

    pub(in crate::javascript) fn in_dead_capability(&self) -> bool {
        self.primitive_number.dead_capability.get()
    }

    /// Capability-only emission of a region that provably cannot execute, such
    /// as the arms behind a definitely throwing conditional test. Lowering
    /// still runs for every static admission/refusal, but primitive-key proof
    /// publication is suppressed. Facts, effects and throw edges stay owned by
    /// the caller's existing isolation and are neither refreshed nor restored
    /// here.
    pub(in crate::javascript) fn with_dead_capability<T>(
        &mut self,
        body: impl FnOnce(&mut Self) -> Result<T, ParseError>,
    ) -> Result<T, ParseError> {
        let previous = self.primitive_number.dead_capability.replace(true);
        let result = body(self);
        self.primitive_number
            .dead_capability
            .set(previous);
        result
    }

    pub(in crate::javascript) fn note_dead_span(&self, span: swc_common::Span) {
        if self.primitive_number.dead_capability.get() {
            self.primitive_number
                .dead_spans
                .borrow_mut()
                .insert((span.lo.0, span.hi.0));
        }
    }

    pub(in crate::javascript) fn statement_is_dead(&self, span: swc_common::Span) -> bool {
        self.primitive_number
            .dead_spans
            .borrow()
            .contains(&(span.lo.0, span.hi.0))
    }

    pub(in crate::javascript) fn record_primitive_number_site(
        &self,
        span: swc_common::Span,
        op: Operator,
        role: Role,
        kind: Kind,
    ) -> Result<(), ParseError> {
        let mode = Mode::from_kind(kind).ok_or_else(|| {
            self.error(
                span,
                "Primitive numeric conversion requires numeric primitive operands",
            )
        })?;
        if self.primitive_number.dead_capability.get() {
            return Ok(());
        }
        let site = (span.lo.0, span.hi.0, op, role);
        let mut conversions = self.primitive_number.conversions.borrow_mut();
        if let Some(previous) = conversions.get(&site) {
            if *previous != mode {
                return Err(self.error(span, "Primitive numeric conversion mode changed"));
            }
        } else if self.primitive_number.sealed.get() {
            return Err(self.error(span, "Primitive numeric conversion site is not sealed"));
        }
        conversions.insert(site, mode);
        if matches!(mode, Mode::String | Mode::Dynamic) {
            register(
                &mut self.primitive_number.sites.borrow_mut(),
                self.primitive_number.sealed.get(),
                (span.lo.0, span.hi.0),
            )
            .map_err(|message| self.error(span, message))?;
        }
        Ok(())
    }

    pub(in crate::javascript) fn record_string_number_site(
        &self,
        span: swc_common::Span,
    ) -> Result<(), ParseError> {
        self.record_primitive_number_site(span, Operator::Plus, Role::Unary, Kind::String)
    }

    // Numeric conversion can occur in a function with no historical member/concat/
    // relational trigger. Validate all collected activations in their own facts.
    pub(super) fn collect_primitive_number_function_sites(&mut self) -> Result<(), ParseError> {
        for collected in self.collected_functions.clone() {
            let Some(body) = &collected.function.body else {
                continue;
            };
            let scope = *self
                .graph
                .function_scopes
                .get(&(collected.function.span.lo.0, collected.function.span.hi.0))
                .ok_or_else(|| {
                    self.error(collected.span, "Primitive Number function scope is missing")
                })?;
            let parent_scope = self.active_scope;
            let parent_strict = self.enclosing_strict;
            self.active_scope = scope;
            self.enclosing_strict = collected.strict;
            // Facts-only pass before activation slots exist: the `arguments`
            // gates key off the invocation vector's presence, so install a
            // placeholder for the walk. Validation never reads the value.
            let saved_args = self.function_args.replace(0);
            let result = reference::validate_function_feature_body(self, &body.stmts);
            self.function_args = saved_args;
            self.active_scope = parent_scope;
            self.enclosing_strict = parent_strict;
            result?;
        }
        Ok(())
    }

    pub(super) fn has_primitive_number_sites(&self) -> bool {
        !self.primitive_number.sites.borrow().is_empty()
    }

    pub(super) fn seal_primitive_number_sites(&self) {
        self.primitive_number.sealed.set(true);
    }

    pub(super) fn append_primitive_number_parser(
        &mut self,
        ir: &mut FrontendIr,
        carrier_schema: &TypeSchema,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        if !self.primitive_number.sealed.get() {
            return Err(self.error(span, "Primitive numeric conversion sites are not sealed"));
        }
        if self.primitive_number.sites.borrow().is_empty()
            || super::super::prelude::math::used(self)
        {
            return Ok(());
        }
        ir.locals = ir.locals.max(usize::from(self.next_slot));
        self.primitive_number.parser =
            Some(super::super::prelude::math::append_string_number_parser(
                self,
                ir,
                carrier_schema,
                span,
                stmts,
                ("__compat_es5_numeric_scan", "__compat_es5_round_decimal"),
            )?);
        ir.locals = ir.locals.max(usize::from(self.next_slot));
        Ok(())
    }

    pub(crate) fn publish_math_number_parser(&mut self, parser: u16) {
        self.primitive_number.parser = Some(parser);
    }

    pub(crate) fn published_number_parser(&self) -> Option<u16> {
        self.primitive_number.parser
    }

    pub(super) fn lower_primitive_unary_plus(
        &mut self,
        unary: &ast::UnaryExpr,
        operand: Lowered,
    ) -> Result<Lowered, ParseError> {
        self.lower_primitive_unary(unary, operand)
    }

    pub(super) fn lower_primitive_unary(
        &mut self,
        unary: &ast::UnaryExpr,
        operand: Lowered,
    ) -> Result<Lowered, ParseError> {
        if operand.kind == Kind::String
            && !self.primitive_number.dead_capability.get()
            && (!self.primitive_number.sealed.get()
                || !self
                    .primitive_number
                    .sites
                    .borrow()
                    .contains(&(unary.span.lo.0, unary.span.hi.0)))
        {
            return Err(self.error(
                unary.span,
                "String unary plus requires its sealed exact site",
            ));
        }
        let (slot, mut stmts) =
            self.bind_evaluation(operand.expr, operand.kind, unary.arg.span())?;
        let owned_roots = Self::compose_owned(
            operand
                .owned_roots
                .into_iter()
                .chain(std::iter::once(slot))
                .filter(|slot| self.has_root(*slot)),
        );
        let op = if unary.op == ast::UnaryOp::Minus {
            Operator::Neg
        } else {
            Operator::Plus
        };
        let number =
            self.primitive_number_projection(slot, operand.kind, unary.span, op, Role::Unary)?;
        let number = if unary.op == ast::UnaryOp::Minus {
            Expr::Neg(Box::new(number))
        } else {
            number
        };
        let projected = value::number(number);
        let result = if let Some(normal) = self.pending_normal {
            let (out, init) = self.bind_temp_bare(value::number(Expr::Float(0.0)), unary.span)?;
            stmts.push(init);
            stmts.push(branch(
                var(normal),
                vec![assign(out, projected, self.line(unary.span))],
                vec![],
                self.line(unary.span),
            ));
            var(out)
        } else {
            projected
        };
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(result),
            },
            kind: Kind::Number,
            array_length: None,
            owned_roots,
        })
    }
    pub(super) fn primitive_number_projection(
        &self,
        slot: u16,
        kind: Kind,
        span: swc_common::Span,
        op: Operator,
        role: Role,
    ) -> Result<Expr, ParseError> {
        let site = (span.lo.0, span.hi.0, op, role);
        if self.primitive_number.dead_capability.get() {
            return Ok(Expr::Float(0.0));
        }
        if !self.primitive_number.sealed.get()
            || self
                .primitive_number
                .conversions
                .borrow()
                .get(&site)
                .copied()
                != Mode::from_kind(kind)
        {
            return Err(self.error(
                span,
                "Primitive numeric conversion requires its sealed mode",
            ));
        }
        Ok(match kind {
            Kind::Number => self.numeric_projection(slot, kind),
            Kind::Dynamic => {
                use super::super::prelude::object::{eq, or};
                let is = |tag| eq(value::field(slot, "tag"), Expr::Int(tag));
                let parser = self.primitive_number.parser.ok_or_else(|| {
                    self.error(
                        span,
                        "Primitive numeric parser must precede the runtime index window",
                    )
                })?;
                let parsed = call(
                    parser,
                    vec![
                        var(self.active_carrier(span)?),
                        var(slot),
                        Expr::Int(0),
                        Expr::Float(0.0),
                    ],
                );
                let primitive = or(
                    or(is(value::NUMBER), is(value::BOOLEAN)),
                    or(or(is(value::NULL), is(value::UNDEFINED)), is(value::STRING)),
                );
                Expr::Block {
                    stmts: vec![Stmt::Expr {
                        expr: call(BuiltinFunction::Assert.call_index(), vec![primitive]),
                        line: self.line(span),
                    }],
                    expr: Box::new(choose(
                        is(value::NUMBER),
                        Expr::Div(
                            Box::new(value::field(slot, "number")),
                            Box::new(Expr::Float(1.0)),
                        ),
                        choose(
                            is(value::BOOLEAN),
                            choose(
                                value::field(slot, "flag"),
                                Expr::Float(1.0),
                                Expr::Float(0.0),
                            ),
                            choose(
                                is(value::NULL),
                                Expr::Float(0.0),
                                choose(is(value::UNDEFINED), Expr::Float(f64::NAN), parsed),
                            ),
                        ),
                    )),
                }
            }
            Kind::Boolean | Kind::Null | Kind::Undefined => {
                non_string_projection(slot, kind).expect("proved scalar numeric projection")
            }
            Kind::String => {
                let parser = self.primitive_number.parser.ok_or_else(|| {
                    self.error(
                        span,
                        "Primitive numeric parser must precede the runtime index window",
                    )
                })?;
                call(
                    parser,
                    vec![
                        var(self.active_carrier(span)?),
                        var(slot),
                        Expr::Int(0),
                        Expr::Float(0.0),
                    ],
                )
            }
            _ => {
                return Err(self.error(
                    span,
                    "Primitive numeric conversion requires numeric primitive operands",
                ));
            }
        })
    }

    /// Bind the two projected Number operands once as Float locals and
    /// compute the shared ES5 power kernel over those slots. The projected
    /// source expressions can carry guarded conversions, so they must never
    /// be cloned into repeated guards; each is bound exactly once here.
    pub(super) fn bind_number_power(
        &mut self,
        left: u16,
        left_kind: Kind,
        right: u16,
        right_kind: Kind,
        span: swc_common::Span,
    ) -> Result<Expr, ParseError> {
        let (base, base_stmt) = self.bind_typed(
            self.numeric_projection(left, left_kind),
            TypeSchema::Float,
            span,
        )?;
        let (exponent, exponent_stmt) = self.bind_typed(
            self.numeric_projection(right, right_kind),
            TypeSchema::Float,
            span,
        )?;
        Ok(Expr::Block {
            stmts: vec![base_stmt, exponent_stmt],
            expr: Box::new(super::super::prelude::math::number_power(base, exponent)),
        })
    }

    pub(super) fn lower_primitive_binary(
        &mut self,
        binary: &ast::BinExpr,
        left: Lowered,
        right: Lowered,
    ) -> Result<Lowered, ParseError> {
        let span = binary.span;
        let line = self.line(span);
        let op = Operator::arithmetic(binary.op).expect("scoped arithmetic conversion");
        let mut owned = left.owned_roots;
        owned.extend(right.owned_roots);
        let (l, mut stmts) = self.bind_evaluation(left.expr, left.kind, binary.left.span())?;
        let out = if self.pending_normal.is_some() {
            let (out, init) = self.bind_temp_bare(value::number(Expr::Float(0.0)), span)?;
            stmts.push(init);
            Some(out)
        } else {
            None
        };
        let (r, mut rhs) = self.bind_evaluation(right.expr, right.kind, binary.right.span())?;
        owned.extend([l, r].into_iter().filter(|s| self.has_root(*s)));
        let (a, pa) = self.bind_typed(
            self.primitive_number_projection(l, left.kind, span, op, Role::Left)?,
            TypeSchema::Float,
            span,
        )?;
        let (b, pb) = self.bind_typed(
            self.primitive_number_projection(r, right.kind, span, op, Role::Right)?,
            TypeSchema::Float,
            span,
        )?;
        let mut compute = vec![pa, pb];
        let boxed = value::number(match op {
            Operator::Sub => Expr::Sub(Box::new(var(a)), Box::new(var(b))),
            Operator::Mul => Expr::Mul(Box::new(var(a)), Box::new(var(b))),
            Operator::Div => Expr::Div(Box::new(var(a)), Box::new(var(b))),
            Operator::Exp => super::super::prelude::math::number_power(a, b),
            _ => unreachable!("scoped arithmetic conversion"),
        });
        let result = if let Some(normal) = self.pending_normal {
            let out = out.expect("pending numeric conversion placeholder");
            compute.push(assign(out, boxed, line));
            rhs.push(branch(var(normal), compute, vec![], line));
            stmts.push(branch(var(normal), rhs, vec![], line));
            var(out)
        } else {
            stmts.append(&mut rhs);
            stmts.append(&mut compute);
            boxed
        };
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(result),
            },
            kind: Kind::Number,
            array_length: None,
            owned_roots: Self::compose_owned(owned.into_iter().filter(|s| self.has_root(*s))),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn primitive135_sealed_exact_sites() {
        let mut sites = HashSet::new();
        register(&mut sites, false, (1, 2)).unwrap();
        register(&mut sites, true, (1, 2)).unwrap();
        assert!(register(&mut sites, true, (1, 3)).is_err());
        assert_eq!(sites, HashSet::from([(1, 2)]));
        let mut empty = HashSet::new();
        assert!(register(&mut empty, true, (1, 2)).is_err());
        assert!(empty.is_empty());
    }
}
