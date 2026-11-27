//! Runtime ToBoolean and operand-valued short circuiting. Child of lowerer:
//! private scalar projections and root protocols stay private to that owner.
use super::*;

pub(in crate::javascript) fn join_kind(left: Kind, right: Kind) -> Kind {
    if left == right { left } else { Kind::Dynamic }
}

pub(in crate::javascript) fn join_length(
    kind: Kind,
    left: Option<super::ArrayShape>,
    right: Option<super::ArrayShape>,
) -> Option<super::ArrayShape> {
    if kind == Kind::Array && left == right {
        left
    } else {
        None
    }
}

/// Only flow facts are merged. Physical slots, root entries, static allocation
/// charges and function identities remain monotonic across conditional emission.
#[derive(Clone)]
struct Flow {
    kinds: HashMap<scope::BindingId, Kind>,
    bindings: HashMap<String, (u16, Kind)>,
    lengths: HashMap<scope::BindingId, super::ArrayShape>,
    named_lengths: HashMap<String, super::ArrayShape>,
    array_names: std::collections::HashSet<String>,
}
impl Flow {
    fn save(lower: &Lowerer<'_>) -> Self {
        Self {
            kinds: lower.binding_kinds.clone(),
            bindings: lower.bindings.clone(),
            lengths: lower.array_length_ids.clone(),
            named_lengths: lower.array_lengths.clone(),
            array_names: lower.array_names.clone(),
        }
    }
    fn restore(self, lower: &mut Lowerer<'_>) {
        lower.binding_kinds = self.kinds;
        lower.bindings = self.bindings;
        lower.array_length_ids = self.lengths;
        lower.array_lengths = self.named_lengths;
        lower.array_names = self.array_names;
    }
    fn merge(self, lower: &mut Lowerer<'_>) {
        for (id, right) in &mut lower.binding_kinds {
            *right = join_kind(self.kinds.get(id).copied().unwrap_or(Kind::Dynamic), *right);
        }
        for (name, (_, right)) in &mut lower.bindings {
            *right = join_kind(
                self.bindings.get(name).map_or(Kind::Dynamic, |(_, k)| *k),
                *right,
            );
        }
        lower.array_length_ids.retain(|id, length| {
            lower.binding_kinds.get(id) == Some(&Kind::Array)
                && self.lengths.get(id) == Some(length)
        });
        lower.array_lengths.retain(|name, length| {
            lower
                .bindings
                .get(name)
                .is_some_and(|(_, k)| *k == Kind::Array)
                && self.named_lengths.get(name) == Some(length)
        });
        lower.array_names.retain(|name| self.array_names.contains(name));
    }
}

/// Emission facts around one runtime conditional. Slots and roots stay
/// monotonic; only kinds and exact lengths are restored or joined.
#[derive(Clone)]
pub(in crate::javascript) struct BranchFacts(Flow);

impl BranchFacts {
    pub(in crate::javascript) fn save(lower: &Lowerer<'_>) -> Self {
        Self(Flow::save(lower))
    }
    pub(in crate::javascript) fn restore(self, lower: &mut Lowerer<'_>) {
        self.0.restore(lower);
    }
    pub(in crate::javascript) fn joined(self, other: &Self) -> Self {
        let mut kinds = self.0.kinds.clone();
        for (id, right) in &other.0.kinds {
            let left = kinds.get(id).copied().unwrap_or(Kind::Dynamic);
            kinds.insert(*id, join_kind(left, *right));
        }
        for id in self.0.kinds.keys() {
            if !other.0.kinds.contains_key(id) {
                let left = kinds[id];
                kinds.insert(*id, join_kind(left, Kind::Dynamic));
            }
        }
        let mut bindings = self.0.bindings.clone();
        for (name, (_, right)) in &other.0.bindings {
            let left = bindings.get(name).map_or(Kind::Dynamic, |(_, kind)| *kind);
            if let Some(slot) = bindings.get_mut(name) {
                slot.1 = join_kind(left, *right);
            }
        }
        for (name, slot) in bindings.iter_mut() {
            if !other.0.bindings.contains_key(name) {
                slot.1 = join_kind(slot.1, Kind::Dynamic);
            }
        }
        let lengths = self
            .0
            .lengths
            .iter()
            .filter(|(id, length)| {
                kinds.get(id) == Some(&Kind::Array) && other.0.lengths.get(id) == Some(*length)
            })
            .map(|(id, length)| (*id, *length))
            .collect();
        let named_lengths = self
            .0
            .named_lengths
            .iter()
            .filter(|(name, length)| {
                bindings
                    .get(*name)
                    .is_some_and(|(_, kind)| *kind == Kind::Array)
                    && other.0.named_lengths.get(*name) == Some(*length)
            })
            .map(|(name, length)| (name.clone(), *length))
            .collect();
        let array_names = self
            .0
            .array_names
            .intersection(&other.0.array_names)
            .cloned()
            .collect();
        Self(Flow {
            kinds,
            bindings,
            lengths,
            named_lengths,
            array_names,
        })
    }
    pub(in crate::javascript) fn merge(self, lower: &mut Lowerer<'_>) {
        self.0.merge(lower);
    }
}

impl Lowerer<'_> {
    /// Evaluate a condition once and bind its ES5 ToBoolean result.
    pub(in crate::javascript) fn condition_flag(
        &mut self,
        test: &ast::Expr,
    ) -> Result<(u16, Vec<Stmt>), ParseError> {
        let lowered = self.expr(test)?;
        let (slot, mut stmts) = self.bind_evaluation(lowered.expr, lowered.kind, test.span())?;
        // P6B-ABRUPT-SIBLING-PREFIX: recheck Normal after the child prefix
        // and before ToBoolean. A throwing test must not run truthiness or
        // select a branch from an Undefined placeholder.
        if let Some(normal) = self.pending_normal {
            let line = self.line(test.span());
            let (flag, init) = self.bind_typed(Expr::Bool(false), TypeSchema::Bool, test.span())?;
            stmts.push(init);
            let predicate = self.truthiness(slot, test.span())?;
            let (tmp, tmp_stmt) = self.bind_typed(predicate, TypeSchema::Bool, test.span())?;
            let mut then_branch = vec![tmp_stmt];
            then_branch.push(Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: flag,
                expr: Expr::Var(tmp),
                line,
            });
            stmts.push(Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch,
                else_branch: Vec::new(),
                line,
            });
            let mut owned = lowered.owned_roots;
            owned.push(slot);
            reference::release_consumed_roots(self, &owned, flag, test.span(), &mut stmts)?;
            return Ok((flag, stmts));
        }
        let predicate = self.truthiness(slot, test.span())?;
        let (flag, flag_stmt) = self.bind_typed(predicate, TypeSchema::Bool, test.span())?;
        stmts.push(flag_stmt);
        let mut owned = lowered.owned_roots;
        owned.push(slot);
        reference::release_consumed_roots(self, &owned, flag, test.span(), &mut stmts)?;
        Ok((flag, stmts))
    }

    /// The saved box is inspected by its real tag, including call/member
    /// results whose compiler Kind is deliberately broad.
    fn truthiness(&mut self, slot: u16, span: swc_common::Span) -> Result<Expr, ParseError> {
        // Get on a bare local is an owning field projection. Inspect this
        // retained record through the existing borrowed Get shape so returning
        // the selected box cannot expose a partially moved map.
        let field = |name: &str| value::field_of(Expr::Borrow(Box::new(Expr::Var(slot))), name);
        let (tag, tag_stmt) = self.bind_typed(field("tag"), TypeSchema::Int, span)?;
        let (flag, flag_stmt) = self.bind_typed(field("flag"), TypeSchema::Bool, span)?;
        let (yes, yes_stmt) = self.bind_temp_bare(value::boolean(Expr::Bool(true)), span)?;
        let boolean = Expr::Eq(
            Box::new(Expr::Var(flag)),
            Box::new(value::field(yes, "flag")),
        );
        let (number, number_stmt) =
            self.bind_typed(Self::as_number(field("number")), TypeSchema::Number, span)?;
        let numeric = Expr::And(
            Box::new(Expr::Eq(
                Box::new(Expr::Var(number)),
                Box::new(Expr::Var(number)),
            )),
            Box::new(Expr::Not(Box::new(Expr::Eq(
                Box::new(Expr::Var(number)),
                Box::new(Expr::Float(0.0)),
            )))),
        );
        let mut fallback = Expr::Bool(false);
        if self.heap.is_some() {
            let carrier = self.active_carrier(span)?;
            let heap = self.heap_ref(span)?;
            let (checked, read) = (heap.helpers.checked_base, heap.helper_read);
            let decode = self
                .helper_decode
                .ok_or_else(|| self.error(span, "ToBoolean requires realm decode"))?;
            let (base, base_stmt) = self.bind_typed(
                Self::guest_call(
                    checked,
                    vec![Expr::Var(carrier), field("ref_slot"), field("generation")],
                ),
                TypeSchema::Int,
                span,
            )?;
            let (kind_number, kind_number_stmt) = self.bind_typed(
                Self::as_number(Self::guest_call(
                    read,
                    vec![Expr::Var(carrier), Self::as_number(Expr::Var(base))],
                )),
                TypeSchema::Number,
                span,
            )?;
            let (kind, kind_stmt) = self.bind_typed(
                Self::guest_call(decode, vec![Expr::Var(kind_number)]),
                TypeSchema::Int,
                span,
            )?;
            let (length, length_stmt) = self.bind_typed(
                Self::as_number(Self::guest_call(
                    read,
                    vec![
                        Expr::Var(carrier),
                        Self::as_number(Expr::Add(
                            Box::new(Expr::Var(base)),
                            Box::new(Expr::Int(2)),
                        )),
                    ],
                )),
                TypeSchema::Number,
                span,
            )?;
            let string = Expr::Block {
                stmts: vec![
                    base_stmt,
                    kind_number_stmt,
                    kind_stmt,
                    assert_stmt(
                        Expr::Eq(
                            Box::new(Expr::Var(kind)),
                            Box::new(Expr::Int(layout::KIND_STRING)),
                        ),
                        self.line(span),
                    ),
                    length_stmt,
                ],
                expr: Box::new(Expr::Not(Box::new(Expr::Eq(
                    Box::new(Expr::Var(length)),
                    Box::new(Expr::Float(0.0)),
                )))),
            };
            fallback = Expr::IfElse {
                condition: Box::new(Expr::Eq(
                    Box::new(Expr::Var(tag)),
                    Box::new(Expr::Int(value::STRING)),
                )),
                then_expr: Box::new(string),
                else_expr: Box::new(Expr::Bool(false)),
            };
        }
        for (kind, predicate) in [
            (value::OBJECT, Expr::Bool(true)),
            (value::NUMBER, numeric),
            (value::BOOLEAN, boolean),
        ] {
            fallback = Expr::IfElse {
                condition: Box::new(Expr::Eq(
                    Box::new(Expr::Var(tag)),
                    Box::new(Expr::Int(kind)),
                )),
                then_expr: Box::new(predicate),
                else_expr: Box::new(fallback),
            };
        }
        Ok(Expr::Block {
            stmts: vec![tag_stmt, flag_stmt, yes_stmt, number_stmt],
            expr: Box::new(fallback),
        })
    }

    pub(super) fn lower_not(&mut self, unary: &ast::UnaryExpr) -> Result<Lowered, ParseError> {
        let operand = self.expr(&unary.arg)?;
        let (slot, mut stmts) = self.bind_evaluation(operand.expr, operand.kind, unary.span)?;
        let predicate = self.truthiness(slot, unary.span)?;
        let (flag, flag_stmt) = self.bind_typed(predicate, TypeSchema::Bool, unary.span)?;
        stmts.push(flag_stmt);
        let owned = Self::compose_owned(operand.owned_roots.into_iter().chain([slot]));
        reference::release_consumed_roots(self, &owned, flag, unary.span, &mut stmts)?;
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(value::boolean(Expr::Not(Box::new(Expr::Var(flag))))),
            },
            kind: Kind::Boolean,
            array_length: None,
            owned_roots: Vec::new(),
        })
    }

    pub(super) fn lower_logical(&mut self, binary: &ast::BinExpr) -> Result<Lowered, ParseError> {
        let left = self.expr(&binary.left)?;
        let (saved, mut stmts) = self.bind_evaluation(left.expr, left.kind, binary.left.span())?;
        let predicate = self.truthiness(saved, binary.span)?;
        // Keep the saved box live across a statement boundary while inspecting
        // it; the branch subsequently returns the intact original record.
        let (predicate_slot, predicate_stmt) =
            self.bind_typed(predicate, TypeSchema::Bool, binary.span)?;
        stmts.push(predicate_stmt);
        let predicate = Expr::Var(predicate_slot);
        let flow = Flow::save(self);
        let cache = self.type_names;
        let previous_roots = self
            .root_scope
            .as_ref()
            .map(|s| s.entries.clone())
            .unwrap_or_default();
        let (right, rhs_definite) =
            self.isolate_conditional_emission(|lower| lower.expr(&binary.right))?;
        self.type_names = cache;
        if rhs_definite {
            flow.restore(self);
        } else {
            flow.merge(self);
        }
        let kind = join_kind(left.kind, right.kind);
        let array_length = join_length(kind, left.array_length, right.array_length);
        let inner = if binary.op == ast::BinaryOp::LogicalAnd {
            Expr::IfElse {
                condition: Box::new(predicate),
                then_expr: Box::new(right.expr),
                else_expr: Box::new(Expr::Var(saved)),
            }
        } else {
            Expr::IfElse {
                condition: Box::new(predicate),
                then_expr: Box::new(Expr::Var(saved)),
                else_expr: Box::new(right.expr),
            }
        };
        // P6B-ABRUPT-SIBLING-PREFIX: a throwing left prefix must not run the
        // right operand's unguarded prefix (key construction/allocation).
        // When Normal is already false, return the saved placeholder without
        // evaluating the right expression.
        let branch = if let Some(normal) = self.pending_normal {
            Expr::IfElse {
                condition: Box::new(Expr::Var(normal)),
                then_expr: Box::new(inner),
                else_expr: Box::new(Expr::Var(saved)),
            }
        } else {
            inner
        };
        let rhs_roots: Vec<_> = self
            .root_scope
            .as_ref()
            .map(|s| {
                s.entries
                    .keys()
                    .filter(|s| !previous_roots.contains_key(s))
                    .copied()
                    .collect()
            })
            .unwrap_or_default();
        let (result, mut result_stmts) = self.bind_evaluation(branch, kind, binary.span)?;
        stmts.append(&mut result_stmts);
        // Root the selected original box before clearing conditional entries.
        // clear_root writes Undefined, never reads a possibly skipped RHS local.
        let owned = Self::compose_owned(
            left.owned_roots
                .into_iter()
                .chain(right.owned_roots)
                .chain(rhs_roots)
                .chain([saved]),
        );
        reference::release_consumed_roots(self, &owned, result, binary.span, &mut stmts)?;
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
}
