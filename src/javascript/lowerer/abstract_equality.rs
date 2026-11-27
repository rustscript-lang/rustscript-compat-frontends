//! Primitive-only abstract equality, exact sealed pairs and ordered evaluation.
use super::super::prelude::object::{
    Emit, add, and, assign, branch, call, choose, declare, eq, int, lt, not, or, sub, tag_is, var,
};
use super::*;
use std::cell::{Cell, RefCell};

/// Abstract-only checked wrapper. Strict `===`/`!==` keep the original helper.
pub(super) const CHECKED_HELPER: &str = "__compat_es5_abstract_string_equal";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::javascript) enum Op {
    Eq,
    Ne,
}
impl Op {
    pub(in crate::javascript) fn from_op(op: ast::BinaryOp) -> Option<Self> {
        match op {
            ast::BinaryOp::EqEq => Some(Self::Eq),
            ast::BinaryOp::NotEq => Some(Self::Ne),
            _ => None,
        }
    }
    fn conversion_operator(self) -> primitive_number::Operator {
        match self {
            Self::Eq => primitive_number::Operator::AbstractEq,
            Self::Ne => primitive_number::Operator::AbstractNe,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::javascript) struct Mode(primitive_number::Mode, primitive_number::Mode);
impl Mode {
    fn covers(self, entry: Self) -> bool {
        use primitive_number::Mode::Dynamic;
        (self.0 == Dynamic || self.0 == entry.0) && (self.1 == Dynamic || self.1 == entry.1)
    }
    fn numeric_arm(self) -> bool {
        use primitive_number::Mode::{Boolean, Dynamic, Number, String};
        matches!(self.0, Number | Boolean | String | Dynamic)
            && matches!(self.1, Number | Boolean | String | Dynamic)
            && !(self.0 == self.1 && self.0 != Dynamic)
    }
    /// Each side is String or Dynamic, so a runtime pair can both be strings.
    fn both_string_capable(self) -> bool {
        use primitive_number::Mode::{Dynamic, String};
        matches!(self.0, String | Dynamic) && matches!(self.1, String | Dynamic)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Site {
    op: Op,
    mode: Mode,
}
#[derive(Default)]
pub(super) struct State {
    sites: RefCell<HashMap<(u32, u32), Site>>,
    sealed: Cell<bool>,
    /// Installed only when a sealed abstract pair can both be strings.
    checked_string_eq: Cell<Option<u16>>,
}
impl State {
    fn record(&self, span: (u32, u32), site: Site, probe: bool) -> Result<(), &'static str> {
        let mut sites = self.sites.borrow_mut();
        if probe {
            if self.sealed.get()
                && !sites
                    .get(&span)
                    .is_some_and(|old| old.op == site.op && old.mode.covers(site.mode))
            {
                return Err("Abstract equality entry is not covered by its sealed primitive pair");
            }
            return Ok(());
        }
        if sites.get(&span).is_some_and(|old| *old != site)
            || (self.sealed.get() && sites.get(&span) != Some(&site))
        {
            return Err("Abstract equality operator/primitive pair is not sealed");
        }
        if !self.sealed.get() {
            sites.insert(span, site);
        }
        Ok(())
    }
}

impl Lowerer<'_> {
    pub(in crate::javascript) fn record_abstract_equality_pair(
        &self,
        binary: &ast::BinExpr,
        left: Kind,
        right: Kind,
    ) -> Result<(), ParseError> {
        use primitive_number::{Mode as Operand, Role};
        // Validate the entire typed pair before publishing either numeric role.
        let (Some(l), Some(r)) = (Operand::from_kind(left), Operand::from_kind(right)) else {
            return Err(self.error(binary.span, "Primitive abstract equality excludes nonprimitive operands; Object ToPrimitive is deferred"));
        };
        let op = Op::from_op(binary.op)
            .ok_or_else(|| self.error(binary.span, "Unknown abstract equality operator"))?;
        let mode = Mode(l, r);
        let probe = self.relational_collection_is_probe();
        self.abstract_equality
            .record(
                (binary.span.lo.0, binary.span.hi.0),
                Site { op, mode },
                probe,
            )
            .map_err(|message| self.error(binary.span, message))?;
        // Same-tag and nullish arms cannot parse. A conservative Dynamic pair
        // records only conversions reachable on the remaining numeric arm.
        if !probe && mode.numeric_arm() {
            self.record_primitive_number_site(
                binary.span,
                op.conversion_operator(),
                Role::Left,
                left,
            )?;
            self.record_primitive_number_site(
                binary.span,
                op.conversion_operator(),
                Role::Right,
                right,
            )?;
        }
        Ok(())
    }
    pub(super) fn seal_abstract_equality_sites(&self) {
        self.abstract_equality.sealed.set(true);
    }
    pub(super) fn abstract_equality_needs_checked_string(&self) -> bool {
        self.abstract_equality
            .sites
            .borrow()
            .values()
            .any(|site| site.mode.both_string_capable())
    }
    /// Caller-side guard for abstract `==`/`!=` only. Validates both string
    /// tags, checked base, live generation, kind, occupied extent, a
    /// nonnegative integral length inside that payload, and the UTF-16 unit
    /// domain, then calls the unchanged equality helper for the Bool.
    ///
    /// The extra unit walk is at most linear in the sum of the two lengths and
    /// is bounded by the occupied extent (an excessive length fatals before
    /// the walk). It performs no managed allocation, copy, or collection.
    /// Fuel stays the original 10_000_000; this pass does not raise it.
    pub(super) fn append_checked_abstract_string_equality(
        &mut self,
        ir: &mut FrontendIr,
        carrier_schema: &TypeSchema,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        if !self.abstract_equality_needs_checked_string() {
            return Ok(());
        }
        if !self.abstract_equality.sealed.get() {
            return Err(self.error(span, "Abstract equality sites are not sealed"));
        }
        let inner = self.string_eq.ok_or_else(|| {
            self.error(
                span,
                "Abstract string equality requires the existing helper",
            )
        })?;
        self.next_slot = self.next_slot.max(
            u16::try_from(ir.locals)
                .map_err(|_| self.error(span, "Abstract string equality local space exhausted"))?,
        );
        let index = heap_emitters::fresh_index(self, ir, span)?;
        let c = self.slot(span)?;
        let left = self.slot(span)?;
        let right = self.slot(span)?;
        let mut e = Emit::new(self, span, c)?;
        let line = e.line;
        let lb = e.integer()?;
        let rb = e.integer()?;
        let ll = e.integer()?;
        let rl = e.integer()?;
        let extent = e.integer()?;
        let cursor = e.integer()?;
        let unit = e.integer()?;
        let flag = e.boolean()?;
        let header = layout::RAW_HEADER_CELLS + layout::STRING_HEADER_CELLS;
        let mut body = Vec::new();
        for (boxed, base, length) in [(left, lb, ll), (right, rb, rl)] {
            body.extend([
                assert_stmt(tag_is(var(boxed), value::STRING), line),
                assign(base, e.base(var(boxed)), line),
                assert_stmt(eq(e.decoded(var(base)), int(layout::KIND_STRING)), line),
                assign(
                    extent,
                    e.decoded(sub(var(base), int(layout::RAW_HEADER_CELLS))),
                    line,
                ),
                assert_stmt(not(lt(var(extent), int(header))), line),
                assign(length, e.decoded(add(var(base), int(2))), line),
                assert_stmt(not(lt(var(length), int(0))), line),
                assert_stmt(not(lt(sub(var(extent), int(header)), var(length))), line),
            ]);
        }
        for (base, length) in [(lb, ll), (rb, rl)] {
            let visit = vec![
                assign(
                    unit,
                    e.decoded(add(
                        add(var(base), int(layout::STRING_HEADER_CELLS)),
                        var(cursor),
                    )),
                    line,
                ),
                assert_stmt(not(lt(var(unit), int(0))), line),
                assert_stmt(not(lt(int(65535), var(unit))), line),
                assign(cursor, add(var(cursor), int(1)), line),
            ];
            body.push(assign(cursor, int(0), line));
            body.push(Stmt::While {
                condition: lt(var(cursor), var(length)),
                body: visit,
                line,
            });
        }
        body.push(assign(
            flag,
            call(inner, vec![var(c), var(left), var(right)]),
            line,
        ));
        let body = e.finish(body);
        declare(
            ir,
            stmts,
            CHECKED_HELPER,
            index,
            vec![c, left, right],
            vec![carrier_schema.clone(), value::schema(), value::schema()],
            TypeSchema::Bool,
            body,
            var(flag),
            line,
        );
        self.abstract_equality.checked_string_eq.set(Some(index));
        ir.locals = ir.locals.max(usize::from(self.next_slot));
        Ok(())
    }
    pub(super) fn lower_abstract_equality(
        &mut self,
        binary: &ast::BinExpr,
    ) -> Result<Lowered, ParseError> {
        let span = binary.span;
        if !self.abstract_equality.sealed.get() {
            return Err(self.error(span, "Abstract equality sites are not sealed"));
        }
        let site = self
            .abstract_equality
            .sites
            .borrow()
            .get(&(span.lo.0, span.hi.0))
            .copied()
            .ok_or_else(|| self.error(span, "Abstract equality site was not proved"))?;
        let op = Op::from_op(binary.op)
            .ok_or_else(|| self.error(span, "Unknown abstract equality operator"))?;
        self.abstract_equality
            .record(
                (span.lo.0, span.hi.0),
                Site {
                    op,
                    mode: site.mode,
                },
                false,
            )
            .map_err(|message| self.error(span, message))?;
        let left = self.expr(&binary.left)?;
        let right = self.expr(&binary.right)?;
        // The sealed back-edge pair, not a narrow first-entry Kind, owns roots.
        let (left_kind, right_kind) = (site.mode.0.kind(), site.mode.1.kind());
        let line = self.line(span);
        let mut owned = left.owned_roots;
        owned.extend(right.owned_roots);
        let (l, mut stmts) = self.bind_evaluation(left.expr, left_kind, binary.left.span())?;
        let out = if self.pending_normal.is_some() {
            let (out, init) = self.bind_temp_bare(value::boolean(Expr::Bool(false)), span)?;
            stmts.push(init);
            Some(out)
        } else {
            None
        };
        let (r, mut rhs) = self.bind_evaluation(right.expr, right_kind, binary.right.span())?;
        owned.extend([l, r].into_iter().filter(|slot| self.has_root(*slot)));
        let mut compare = Vec::new();
        // These are profile-fatal actual-tag guards, including object/null.
        // Both GetValues must finish normally before even the first guard.
        for (slot, kind) in [(l, left_kind), (r, right_kind)] {
            if kind == Kind::Dynamic {
                let is = |tag| tag_is(var(slot), tag);
                let primitive = or(
                    or(is(value::NUMBER), is(value::BOOLEAN)),
                    or(or(is(value::NULL), is(value::UNDEFINED)), is(value::STRING)),
                );
                compare.push(Stmt::Expr {
                    expr: Expr::Call(
                        BuiltinFunction::Assert.call_index(),
                        Vec::new(),
                        vec![primitive],
                        None,
                        None,
                    ),
                    line,
                });
            }
        }
        let strings = if site.mode.both_string_capable() {
            let index = self
                .abstract_equality
                .checked_string_eq
                .get()
                .ok_or_else(|| {
                    self.error(span, "Abstract string equality wrapper was not installed")
                })?;
            let carrier = self.active_carrier(span)?;
            Some(value::StringEquality {
                carrier: var(carrier),
                index,
            })
        } else {
            match (self.active_carrier(span).ok(), self.string_eq) {
                (Some(carrier), Some(index)) => Some(value::StringEquality {
                    carrier: var(carrier),
                    index,
                }),
                _ => None,
            }
        };
        let same = eq(value::field(l, "tag"), value::field(r, "tag"));
        let nullish = |slot| {
            or(
                tag_is(var(slot), value::NULL),
                tag_is(var(slot), value::UNDEFINED),
            )
        };
        let numeric = if site.mode.numeric_arm() {
            use primitive_number::Role;
            let (a, pa) = self.bind_typed(
                self.primitive_number_projection(
                    l,
                    left_kind,
                    span,
                    op.conversion_operator(),
                    Role::Left,
                )?,
                TypeSchema::Float,
                span,
            )?;
            let (b, pb) = self.bind_typed(
                self.primitive_number_projection(
                    r,
                    right_kind,
                    span,
                    op.conversion_operator(),
                    Role::Right,
                )?,
                TypeSchema::Float,
                span,
            )?;
            Expr::Block {
                stmts: vec![pa, pb],
                expr: Box::new(eq(var(a), var(b))),
            }
        } else {
            Expr::Bool(false)
        };
        let flag = choose(
            same,
            value::strict_equal_flag(l, r, strings),
            choose(
                or(nullish(l), nullish(r)),
                and(nullish(l), nullish(r)),
                numeric,
            ),
        );
        let (flag_slot, flag_stmt) = self.bind_typed(flag, TypeSchema::Bool, span)?;
        compare.push(flag_stmt);
        let flag = if op == Op::Ne {
            not(var(flag_slot))
        } else {
            var(flag_slot)
        };
        let result = if let Some(normal) = self.pending_normal {
            compare.push(assign(
                out.expect("pending equality placeholder"),
                value::boolean(flag),
                line,
            ));
            rhs.push(branch(var(normal), compare, vec![], line));
            stmts.push(branch(var(normal), rhs, vec![], line));
            var(out.expect("pending equality placeholder"))
        } else {
            stmts.append(&mut rhs);
            stmts.append(&mut compare);
            value::boolean(flag)
        };
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(result),
            },
            kind: Kind::Boolean,
            array_length: None,
            owned_roots: Self::compose_owned(owned.into_iter().filter(|slot| self.has_root(*slot))),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_pair_operator_span_and_empty_seal() {
        use primitive_number::Mode::*;
        let state = State::default();
        let site = Site {
            op: Op::Eq,
            mode: Mode(String, Boolean),
        };
        state.record((1, 6), site, false).unwrap();
        state.sealed.set(true);
        state.record((1, 6), site, false).unwrap();
        assert!(state.record((2, 6), site, false).is_err());
        assert!(
            state
                .record((1, 6), Site { op: Op::Ne, ..site }, false)
                .is_err()
        );
        assert!(
            state
                .record(
                    (1, 6),
                    Site {
                        mode: Mode(Boolean, String),
                        ..site
                    },
                    false
                )
                .is_err()
        );
        let empty = State::default();
        empty.sealed.set(true);
        assert!(empty.record((1, 6), site, false).is_err());
    }
    #[test]
    fn validation_probe_publishes_nothing_and_requires_conservative_coverage() {
        use primitive_number::Mode::*;
        let state = State::default();
        let entry = Site {
            op: Op::Eq,
            mode: Mode(Number, Boolean),
        };
        state.record((1, 6), entry, true).unwrap();
        assert!(state.sites.borrow().is_empty());
        state
            .record(
                (1, 6),
                Site {
                    mode: Mode(Dynamic, Boolean),
                    ..entry
                },
                false,
            )
            .unwrap();
        state.sealed.set(true);
        state.record((1, 6), entry, true).unwrap();
        assert!(
            state
                .record(
                    (1, 6),
                    Site {
                        mode: Mode(Number, String),
                        ..entry
                    },
                    true
                )
                .is_err()
        );
        assert!(
            state
                .record(
                    (1, 6),
                    Site {
                        op: Op::Ne,
                        ..entry
                    },
                    true
                )
                .is_err()
        );
        assert!(state.record((2, 6), entry, true).is_err());
    }
    #[test]
    fn numeric_demand_is_pair_specific() {
        use primitive_number::Mode::*;
        for mode in [
            Mode(String, String),
            Mode(Null, String),
            Mode(Undefined, String),
            Mode(Null, Dynamic),
            Mode(Dynamic, Undefined),
            Mode(Number, Number),
        ] {
            assert!(!mode.numeric_arm());
        }
        for mode in [
            Mode(Number, String),
            Mode(String, Boolean),
            Mode(Dynamic, String),
            Mode(Dynamic, Dynamic),
            Mode(Number, Boolean),
        ] {
            assert!(mode.numeric_arm());
        }
    }
    #[test]
    fn checked_string_demand_is_both_string_capable() {
        use primitive_number::Mode::*;
        for mode in [
            Mode(String, String),
            Mode(String, Dynamic),
            Mode(Dynamic, String),
            Mode(Dynamic, Dynamic),
        ] {
            assert!(mode.both_string_capable());
        }
        for mode in [
            Mode(Number, Number),
            Mode(Number, Boolean),
            Mode(String, Boolean),
            Mode(Boolean, String),
            Mode(Null, String),
            Mode(String, Null),
            Mode(Undefined, String),
            Mode(String, Undefined),
            Mode(Null, Dynamic),
            Mode(Dynamic, Undefined),
            Mode(Number, String),
            Mode(Null, Null),
        ] {
            assert!(!mode.both_string_capable());
        }
    }
}
