//! Sealed primitive compound assignment and prefix/postfix update.
//! One prepared reference, one GetValue, one RHS, then conversion and one PutValue.
use super::super::prelude::object::{assign, branch, call, eq, int, or, var};
use super::bitwise::Operation;
use super::primitive_number::{self, Mode, Operator, Role};
use super::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(in crate::javascript) enum MutationOp {
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
    RemAssign,
    ExpAssign,
    ShlAssign,
    ShrAssign,
    UshrAssign,
    BitAndAssign,
    BitOrAssign,
    BitXorAssign,
    Inc,
    Dec,
}

impl MutationOp {
    pub(in crate::javascript) fn from_assign(op: ast::AssignOp) -> Option<Self> {
        Some(match op {
            ast::AssignOp::AddAssign => Self::AddAssign,
            ast::AssignOp::SubAssign => Self::SubAssign,
            ast::AssignOp::MulAssign => Self::MulAssign,
            ast::AssignOp::DivAssign => Self::DivAssign,
            ast::AssignOp::ModAssign => Self::RemAssign,
            ast::AssignOp::ExpAssign => Self::ExpAssign,
            ast::AssignOp::LShiftAssign => Self::ShlAssign,
            ast::AssignOp::RShiftAssign => Self::ShrAssign,
            ast::AssignOp::ZeroFillRShiftAssign => Self::UshrAssign,
            ast::AssignOp::BitAndAssign => Self::BitAndAssign,
            ast::AssignOp::BitOrAssign => Self::BitOrAssign,
            ast::AssignOp::BitXorAssign => Self::BitXorAssign,
            _ => return None,
        })
    }
    fn update(self) -> bool {
        matches!(self, Self::Inc | Self::Dec)
    }
    fn numeric_operator(self) -> Option<Operator> {
        Some(match self {
            Self::SubAssign => Operator::Sub,
            Self::MulAssign => Operator::Mul,
            Self::DivAssign => Operator::Div,
            Self::RemAssign => Operator::Remainder,
            Self::ExpAssign => Operator::Exp,
            Self::ShlAssign => Operator::LeftShift,
            Self::ShrAssign => Operator::RightShift,
            Self::UshrAssign => Operator::UnsignedRightShift,
            Self::BitAndAssign => Operator::And,
            Self::BitOrAssign => Operator::Or,
            Self::BitXorAssign => Operator::Xor,
            Self::Inc | Self::Dec => Operator::Plus,
            Self::AddAssign => return None,
        })
    }
    fn bitwise(self) -> Option<Operation> {
        Some(match self {
            Self::ShlAssign => Operation::Left,
            Self::ShrAssign => Operation::Right,
            Self::UshrAssign => Operation::UnsignedRight,
            Self::BitAndAssign => Operation::And,
            Self::BitOrAssign => Operation::Or,
            Self::BitXorAssign => Operation::Xor,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(in crate::javascript) enum Owner {
    Local,
    Catch,
    Parameter,
    Global,
    Property,
    ArrayItem,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::javascript) struct Proof {
    pub op: MutationOp,
    pub prefix: bool,
    pub role: Role,
    pub owner: Owner,
    pub left: Mode,
    pub right: Option<Mode>,
    pub result: Kind,
}

#[derive(Default)]
pub(super) struct State {
    sites: RefCell<HashMap<(u32, u32), Proof>>,
    sealed: Cell<bool>,
}

fn mode_of(kind: Kind) -> Option<Mode> {
    Mode::from_kind(kind)
}

fn stringish(mode: Mode) -> bool {
    matches!(mode, Mode::String | Mode::Dynamic)
}

fn result_kind(op: MutationOp, left: Mode, right: Option<Mode>) -> Kind {
    if op.update() {
        return Kind::Number;
    }
    if op != MutationOp::AddAssign {
        return Kind::Number;
    }
    let right = right.unwrap_or(Mode::Number);
    if left == Mode::String || right == Mode::String {
        Kind::String
    } else if left == Mode::Dynamic || right == Mode::Dynamic {
        Kind::Dynamic
    } else {
        Kind::Number
    }
}

fn mode_covers(sealed: Mode, probe: Mode) -> bool {
    sealed == probe || sealed == Mode::Dynamic
}

fn kind_covers(sealed: Kind, probe: Kind) -> bool {
    sealed == probe
        || (sealed == Kind::Dynamic && matches!(probe, Kind::Number | Kind::String | Kind::Dynamic))
}

fn proof_covers(sealed: &Proof, probe: &Proof) -> bool {
    sealed.op == probe.op
        && sealed.prefix == probe.prefix
        && sealed.role == probe.role
        && sealed.owner == probe.owner
        && mode_covers(sealed.left, probe.left)
        && match (sealed.right, probe.right) {
            (None, None) => true,
            (Some(sealed_right), Some(probe_right)) => mode_covers(sealed_right, probe_right),
            _ => false,
        }
        && kind_covers(sealed.result, probe.result)
}

fn register(
    state: &State,
    site: (u32, u32),
    proof: Proof,
    probe: bool,
) -> Result<(), &'static str> {
    if probe {
        if state.sealed.get()
            && !state
                .sites
                .borrow()
                .get(&site)
                .is_some_and(|old| proof_covers(old, &proof))
        {
            return Err("Primitive mutation probe is not covered by its sealed proof");
        }
        return Ok(());
    }
    if proof.op.update() {
        if proof.right.is_some() || proof.role != Role::Unary {
            return Err("Primitive update proof requires a unary place role");
        }
    } else if proof.role != Role::Left || proof.right.is_none() {
        return Err("Primitive compound proof requires a left place and a right operand");
    }
    let mut sites = state.sites.borrow_mut();
    if sites.get(&site).is_some_and(|old| *old != proof) {
        return Err("Primitive mutation proof changed");
    }
    if state.sealed.get() && sites.get(&site) != Some(&proof) {
        return Err("Primitive mutation proof is not sealed");
    }
    sites.insert(site, proof);
    Ok(())
}

impl Lowerer<'_> {
    pub(super) fn seal_mutation_sites(&self) {
        self.mutation.sealed.set(true);
    }
    pub(in crate::javascript) fn binding_owner(&self, id: scope::BindingId) -> Owner {
        let binding = self.graph.binding(id);
        let scope = self.graph.scope(binding.scope);
        if scope.kind == scope::ScopeKind::Catch {
            Owner::Catch
        } else if scope.formals.iter().any(|(formal, _, _)| *formal == id) {
            Owner::Parameter
        } else {
            Owner::Local
        }
    }
    pub(in crate::javascript) fn note_mutation(
        &self,
        span: swc_common::Span,
        op: MutationOp,
        prefix: bool,
        owner: Owner,
        left: Kind,
        right: Option<Kind>,
    ) -> Result<Kind, ParseError> {
        let left_mode = mode_of(left)
            .ok_or_else(|| self.error(span, "P0 arithmetic currently requires numeric operands"))?;
        let right_mode = match right {
            Some(kind) => Some(mode_of(kind).ok_or_else(|| {
                self.error(span, "P0 arithmetic currently requires numeric operands")
            })?),
            None => None,
        };
        if op == MutationOp::AddAssign
            && (matches!(left, Kind::Object | Kind::Array | Kind::Function)
                || right.is_some_and(|kind| {
                    matches!(kind, Kind::Object | Kind::Array | Kind::Function)
                }))
        {
            return Err(self.error(
                span,
                "P0 arithmetic currently requires numeric operands: binary Add requires primitive operands",
            ));
        }
        let proof = Proof {
            op,
            prefix,
            role: if op.update() { Role::Unary } else { Role::Left },
            owner,
            left: left_mode,
            right: right_mode,
            result: result_kind(op, left_mode, right_mode),
        };
        register(
            &self.mutation,
            (span.lo.0, span.hi.0),
            proof,
            self.relational_collection_is_probe(),
        )
        .map_err(|message| self.error(span, message))?;
        if !self.relational_collection_is_probe() && op != MutationOp::AddAssign {
            let numeric = op
                .numeric_operator()
                .expect("non-add mutation has a numeric operator");
            if op.update() {
                self.record_primitive_number_site(span, numeric, Role::Unary, left)?;
            } else {
                self.record_primitive_number_site(span, numeric, Role::Left, left)?;
                self.record_primitive_number_site(
                    span,
                    numeric,
                    Role::Right,
                    right.expect("compound right"),
                )?;
            }
        }
        Ok(proof.result)
    }
    fn mutation_proof(&self, span: swc_common::Span) -> Result<Proof, ParseError> {
        let site = (span.lo.0, span.hi.0);
        self.mutation
            .sites
            .borrow()
            .get(&site)
            .copied()
            .filter(|_| self.mutation.sealed.get())
            .ok_or_else(|| self.error(span, "Primitive mutation proof is not sealed"))
    }
    pub(super) fn mutation_needs_strings(&self) -> bool {
        self.mutation.sites.borrow().values().any(|proof| {
            proof.op == MutationOp::AddAssign
                && (stringish(proof.left) || proof.right.is_some_and(stringish))
        })
    }
    pub(super) fn mutation_needs_formatter(&self) -> bool {
        self.mutation.sites.borrow().values().any(|proof| {
            proof.op == MutationOp::AddAssign
                && (stringish(proof.left) || proof.right.is_some_and(stringish))
                && (matches!(proof.left, Mode::Number | Mode::Dynamic)
                    || proof
                        .right
                        .is_some_and(|mode| matches!(mode, Mode::Number | Mode::Dynamic)))
        })
    }
    pub(super) fn mutation_needs_fixed_strings(&self) -> bool {
        self.mutation.sites.borrow().values().any(|proof| {
            let fixed = |mode| {
                matches!(
                    mode,
                    Mode::Boolean | Mode::Null | Mode::Undefined | Mode::Dynamic
                )
            };
            proof.op == MutationOp::AddAssign
                && (stringish(proof.left) || proof.right.is_some_and(stringish))
                && (fixed(proof.left) || proof.right.is_some_and(fixed))
        })
    }
}

fn gate(lower: &Lowerer<'_>, stmts: Vec<Stmt>, span: swc_common::Span) -> Vec<Stmt> {
    if let Some(normal) = lower.pending_normal {
        vec![Stmt::IfElse {
            condition: Expr::Var(normal),
            then_branch: stmts,
            else_branch: Vec::new(),
            line: lower.line(span),
        }]
    } else {
        stmts
    }
}

enum Place {
    Binding {
        name: String,
        span: swc_common::Span,
    },
    Property {
        member: ast::MemberExpr,
    },
    Array {
        object: ast::Ident,
        index: i64,
        span: swc_common::Span,
    },
    /// `array[key]` on a receiver binding that is still a dense Array, with a
    /// computed key that is neither a numeric literal index nor an explicit
    /// String literal. The key is an arbitrary expression and is evaluated
    /// exactly once. The literal-index route stays `Place::Array`; the
    /// explicit String literal route stays the ordinary `Place::Property`
    /// Reference.
    ComputedArray {
        member: ast::MemberExpr,
    },
}

fn unwrap_expr(expression: &ast::Expr) -> &ast::Expr {
    match expression {
        ast::Expr::Paren(paren) => unwrap_expr(&paren.expr),
        other => other,
    }
}

/// The shape half of the computed dense-array place: a computed key that is
/// neither a numeric literal index nor an explicit String literal. Callers
/// add the `Array` receiver proof. The validation walk
/// (`reference::mutation_place`) and emission (`place_of_expr`) share this
/// classification so the sealed old mode and the emitted place cannot drift.
pub(in crate::javascript) fn computed_array_candidate(member: &ast::MemberExpr) -> bool {
    let ast::MemberProp::Computed(property) = &member.prop else {
        return false;
    };
    Lowerer::member_index_literal(&property.expr).is_none()
        && !matches!(
            Lowerer::unwrap_parens(&property.expr),
            ast::Expr::Lit(ast::Lit::Str(_))
        )
}

fn place_of_expr(lower: &Lowerer<'_>, expression: &ast::Expr) -> Result<Place, ParseError> {
    match unwrap_expr(expression) {
        ast::Expr::Ident(ident) => Ok(Place::Binding {
            name: ident.sym.to_string(),
            span: ident.span,
        }),
        ast::Expr::Member(member) => {
            if let ast::MemberProp::Computed(property) = &member.prop
                && let Some(index) = Lowerer::member_index_literal(&property.expr)
                && let ast::Expr::Ident(object) = unwrap_expr(&member.obj)
                && lower
                    .bindings
                    .get(object.sym.as_str())
                    .is_some_and(|(_, kind)| *kind == Kind::Array)
            {
                return Ok(Place::Array {
                    object: object.clone(),
                    index,
                    span: member.span,
                });
            }
            if computed_array_candidate(member)
                && let ast::Expr::Ident(object) = member.obj.as_ref()
                && lower
                    .bindings
                    .get(object.sym.as_str())
                    .is_some_and(|(_, kind)| *kind == Kind::Array)
            {
                return Ok(Place::ComputedArray {
                    member: member.clone(),
                });
            }
            Ok(Place::Property {
                member: member.clone(),
            })
        }
        other => Err(lower.error(
            other.span(),
            "unsupported JavaScript ES5 P0 primitive expression",
        )),
    }
}

fn place_of_target(lower: &Lowerer<'_>, target: &ast::AssignTarget) -> Result<Place, ParseError> {
    match target {
        ast::AssignTarget::Simple(ast::SimpleAssignTarget::Ident(binding)) => Ok(Place::Binding {
            name: binding.id.sym.to_string(),
            span: binding.id.span,
        }),
        ast::AssignTarget::Simple(ast::SimpleAssignTarget::Member(member)) => {
            place_of_expr(lower, &ast::Expr::Member(member.clone()))
        }
        ast::AssignTarget::Simple(ast::SimpleAssignTarget::Paren(paren)) => {
            place_of_expr(lower, &paren.expr)
        }
        other => Err(lower.error(
            other.span(),
            "unsupported JavaScript ES5 P0 primitive expression",
        )),
    }
}

impl Lowerer<'_> {
    /// Capability-only mutation target in discarded text. The live compound/
    /// update consumer needs the sealed primitive-key Reference, which the
    /// dead walk never publishes; the receiver/key (and compound RHS) still
    /// run through the canonical admission in source order, then the mutation
    /// emits no executable Reference, helper or proof.
    fn discard_property_mutation(
        &mut self,
        place: &Place,
        role: super::primitive_key::Role,
        right: Option<&ast::Expr>,
        span: swc_common::Span,
    ) -> Result<Option<Lowered>, ParseError> {
        if !self.in_dead_capability() && !self.emission_known_throw() {
            return Ok(None);
        }
        let member = match place {
            Place::Property { member } => member,
            Place::ComputedArray { member } => {
                // A computed dense-array place publishes no primitive-key
                // proof at all (an Array Compound observation is always
                // ineligible), so this route cannot use
                // `discard_member_capability`. Keep the canonical source order
                // (receiver, key, optional RHS) and stop without heap access,
                // a helper call, a new proof or a realm fee.
                let _receiver = self.expr(&member.obj)?;
                let ast::MemberProp::Computed(property) = &member.prop else {
                    return Ok(None);
                };
                let _key = self.expr(&property.expr)?;
                if let Some(right) = right {
                    let _rhs = self.expr(right)?;
                }
                let kind = self
                    .mutation_proof(span)
                    .map(|proof| proof.result)
                    .unwrap_or(Kind::Dynamic);
                return Ok(Some(Self::capability_only_value(kind)));
            }
            _ => return Ok(None),
        };
        if self.primitive_key_role(member.span, role) {
            return Ok(None);
        }
        let Some(key_expr) = Self::open_computed_key(member) else {
            return Ok(None);
        };
        let base = self.expr(&member.obj)?;
        let key = self.expr(key_expr)?;
        if !self.discard_member_capability(member, base.kind, key.kind, role)? {
            return Ok(None);
        }
        if let Some(right) = right {
            let _rhs = self.expr(right)?;
        }
        let kind = self
            .mutation_proof(span)
            .map(|proof| proof.result)
            .unwrap_or(Kind::Dynamic);
        Ok(Some(Self::capability_only_value(kind)))
    }

    pub(in crate::javascript) fn lower_update(
        &mut self,
        update: &ast::UpdateExpr,
    ) -> Result<Lowered, ParseError> {
        let op = match update.op {
            ast::UpdateOp::PlusPlus => MutationOp::Inc,
            ast::UpdateOp::MinusMinus => MutationOp::Dec,
        };
        let place = place_of_expr(self, &update.arg)?;
        if let Some(placeholder) = self.discard_property_mutation(
            &place,
            super::primitive_key::Role::Update,
            None,
            update.span,
        )? {
            return Ok(placeholder);
        }
        let proof = self.mutation_proof(update.span)?;
        if proof.op != op || proof.prefix != update.prefix {
            return Err(self.error(update.span, "Primitive mutation proof changed"));
        }
        self.emit_mutation(proof, place, None, update.span)
    }
    pub(in crate::javascript) fn lower_compound(
        &mut self,
        assign: &ast::AssignExpr,
    ) -> Result<Lowered, ParseError> {
        let op = MutationOp::from_assign(assign.op).ok_or_else(|| {
            self.error(
                assign.span,
                "unsupported JavaScript ES5 P0 primitive expression",
            )
        })?;
        let place = place_of_target(self, &assign.left)?;
        if let Some(placeholder) = self.discard_property_mutation(
            &place,
            super::primitive_key::Role::Compound,
            Some(&assign.right),
            assign.span,
        )? {
            return Ok(placeholder);
        }
        let proof = self.mutation_proof(assign.span)?;
        if proof.op != op || proof.prefix {
            return Err(self.error(assign.span, "Primitive mutation proof changed"));
        }
        self.emit_mutation(proof, place, Some(&assign.right), assign.span)
    }
    fn emit_mutation(
        &mut self,
        proof: Proof,
        place: Place,
        right: Option<&ast::Expr>,
        span: swc_common::Span,
    ) -> Result<Lowered, ParseError> {
        let mut owned = Vec::new();
        let mut body = Vec::new();
        let (old, saved, mut get_stmts) = self.read_place(&place, proof, &mut owned, &mut body)?;
        let (out, init) = if proof.result == Kind::Number {
            let (out, init) = self.bind_temp_bare(value::number(Expr::Float(0.0)), span)?;
            (out, vec![init])
        } else {
            self.bind_temp(value::undefined(), span)?
        };
        body.extend(init);
        if self.emission_known_throw() {
            body.extend(gate(self, get_stmts, span));
            owned.push(out);
            return Ok(Lowered {
                expr: Expr::Block {
                    stmts: body,
                    expr: Box::new(Expr::Var(out)),
                },
                kind: proof.result,
                array_length: None,
                owned_roots: owned,
            });
        }
        let mut after_get = Vec::new();
        let rhs = if let Some(right) = right {
            let lowered = self.expr(right)?;
            owned.extend(lowered.owned_roots);
            let kind = proof.right.expect("compound right").kind();
            let (slot, bound) = self.bind_evaluation(lowered.expr, kind, right.span())?;
            after_get.extend(bound);
            if self.has_root(slot) {
                owned.push(slot);
            }
            Some(slot)
        } else {
            None
        };
        let mut compute = Vec::new();
        let (new_slot, result_slot) = self.emit_compute(proof, old, rhs, &mut compute, span)?;
        if self.has_root(new_slot) {
            owned.push(new_slot);
        }
        if result_slot != new_slot && self.has_root(result_slot) {
            owned.push(result_slot);
        }
        let mut put = Vec::new();
        self.write_place(&place, &saved, new_slot, proof, &mut put, span)?;
        let mut publish = vec![Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: out,
            expr: Expr::Var(result_slot),
            line: self.line(span),
        }];
        if proof.result != Kind::Number {
            self.refresh_root(out, span, &mut publish)?;
        }
        put.extend(gate(self, publish, span));
        compute.extend(put);
        after_get.extend(gate(self, compute, span));
        // Nest conversion inside the successful GetValue branch. A sibling
        // gate would merge the place slot back to unknown before the numeric
        // if/else, which strict mode rejects as unknown vs float.
        get_stmts.extend(gate(self, after_get, span));
        body.extend(gate(self, get_stmts, span));
        super::reference::release_consumed_roots(self, &owned, out, span, &mut body)?;
        Ok(Lowered {
            expr: Expr::Block {
                stmts: body,
                expr: Box::new(Expr::Var(out)),
            },
            kind: proof.result,
            array_length: None,
            owned_roots: if proof.result == Kind::Number {
                Vec::new()
            } else {
                vec![out]
            },
        })
    }
}

enum Saved {
    Binding {
        name: String,
    },
    Property {
        reference: u16,
    },
    Array {
        receiver: u16,
        index: i64,
    },
    /// The saved computed dense-array place: the rooted receiver box and the
    /// once-evaluated key box, so neither identity is re-read from a rebindable
    /// source binding when the store runs after the RHS.
    ComputedArray {
        receiver: u16,
        key: u16,
        key_kind: Kind,
    },
}

impl Lowerer<'_> {
    fn read_place(
        &mut self,
        place: &Place,
        proof: Proof,
        owned: &mut Vec<u16>,
        body: &mut Vec<Stmt>,
    ) -> Result<(u16, Saved, Vec<Stmt>), ParseError> {
        match place {
            Place::Binding { name, span } => {
                let old = self.read_binding(name, proof.left.kind(), *span, body)?;
                if self.has_root(old) {
                    owned.push(old);
                }
                Ok((old, Saved::Binding { name: name.clone() }, Vec::new()))
            }
            Place::Property { member } => {
                let (old, reference, access) =
                    self.read_property(member, proof.left.kind(), owned, body)?;
                Ok((old, Saved::Property { reference }, access))
            }
            Place::Array {
                object,
                index,
                span,
            } => {
                let (old, receiver, access) =
                    self.read_array(object, *index, proof.left == Mode::Dynamic, *span, owned, body)?;
                Ok((
                    old,
                    Saved::Array {
                        receiver,
                        index: *index,
                    },
                    access,
                ))
            }
            Place::ComputedArray { member } => {
                let ast::MemberProp::Computed(property) = &member.prop else {
                    return Err(self.error(member.span, "Primitive mutation place changed"));
                };
                let ast::Expr::Ident(object) = member.obj.as_ref() else {
                    return Err(self.error(
                        member.span,
                        "P0 indexed writes require a numeric-array binding",
                    ));
                };
                let (binding, _) = *self.bindings.get(object.sym.as_str()).ok_or_else(|| {
                    self.error(member.span, "P3 binding was collected but not instantiated")
                })?;
                // Receiver identity is saved once before the key and stays
                // rooted through the key, the Get, the RHS and the store, so a
                // key or RHS rebind of the source name cannot retarget it.
                let (receiver, bound) = self.bind_temp(Expr::Var(binding), object.span)?;
                body.extend(bound);
                owned.push(receiver);
                let key = self.expr(&property.expr)?;
                if !matches!(key.kind, Kind::Number | Kind::Dynamic) {
                    return Err(self.error(
                        property.span(),
                        "P0 dense array indexes must be numeric in this scope",
                    ));
                }
                let key_kind = key.kind;
                owned.extend(key.owned_roots);
                let (key_slot, access) =
                    self.bind_evaluation(key.expr, key_kind, property.span())?;
                body.extend(access);
                owned.push(key_slot);
                let mut get_stmts = Vec::new();
                if self.emission_known_throw() {
                    // A statically throwing key keeps its own effects, but the
                    // Get can never run: no helper, decode or array_get is
                    // materialized, and the enclosing known-throw path owns the
                    // root transfer.
                    let (old, init) = self.bind_temp_bare(value::undefined(), member.span)?;
                    body.push(init);
                    owned.push(old);
                    return Ok((
                        old,
                        Saved::ComputedArray {
                            receiver,
                            key: key_slot,
                            key_kind,
                        },
                        get_stmts,
                    ));
                }
                let carrier = self.active_carrier(member.span)?;
                let heap = self.heap_ref(member.span)?;
                let (checked_base, array_get) = (heap.helpers.checked_base, heap.helpers.array_get);
                let decode = self.helper_decode.ok_or_else(|| {
                    self.error(
                        member.span,
                        "P0 array reads require the realm decode helper",
                    )
                })?;
                let line = self.line(member.span);
                let idx_tmp = self.slot(member.span)?;
                get_stmts.push(Stmt::Let {
                    index: idx_tmp,
                    declared_schema: Some(TypeSchema::Int),
                    expr: Self::guest_call(
                        decode,
                        vec![Self::as_number(self.array_index_number(key_slot, key_kind))],
                    ),
                    line,
                });
                let (base_tmp, saved) = self.bind_typed(
                    Self::guest_call(
                        checked_base,
                        vec![
                            Expr::Var(carrier),
                            value::field(receiver, "ref_slot"),
                            value::field(receiver, "generation"),
                        ],
                    ),
                    TypeSchema::Int,
                    member.span,
                )?;
                get_stmts.push(saved);
                let got = Self::guest_call(
                    array_get,
                    vec![Expr::Var(carrier), Expr::Var(base_tmp), Expr::Var(idx_tmp)],
                );
                let (old, saved) = self.bind_temp(got, member.span)?;
                get_stmts.extend(saved);
                owned.push(old);
                Ok((
                    old,
                    Saved::ComputedArray {
                        receiver,
                        key: key_slot,
                        key_kind,
                    },
                    get_stmts,
                ))
            }
        }
    }

    fn read_binding(
        &mut self,
        name: &str,
        kind: Kind,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
    ) -> Result<u16, ParseError> {
        match super::reference::resolve(self, name) {
            super::reference::NameHit::Global(index) => {
                let helpers = self
                    .global_helpers
                    .ok_or_else(|| self.error(span, "shared environment unavailable"))?;
                let env = self
                    .global_env
                    .ok_or_else(|| self.error(span, "shared environment unavailable"))?;
                let expr = Self::guest_call(
                    helpers.get,
                    vec![
                        Expr::Var(self.active_carrier(span)?),
                        Expr::Var(env),
                        Expr::Int(self.globals.entry(index)),
                    ],
                );
                let (slot, saved) = self.bind_evaluation(expr, kind, span)?;
                stmts.extend(saved);
                Ok(slot)
            }
            super::reference::NameHit::Local(id) => {
                let binding = *self.binding_slots.get(&id).ok_or_else(|| {
                    self.error(span, "P3 binding was collected but not instantiated")
                })?;
                let (slot, saved) = self.bind_evaluation(Expr::Var(binding), kind, span)?;
                stmts.extend(saved);
                Ok(slot)
            }
            super::reference::NameHit::Outer(_) => Err(self.error(
                span,
                &format!("P4 free-variable capture of '{name}' is outside this closed subset"),
            )),
            _ => Err(self.error(span, &format!("unknown local '{name}'"))),
        }
    }

    fn read_property(
        &mut self,
        member: &ast::MemberExpr,
        kind: Kind,
        owned: &mut Vec<u16>,
        body: &mut Vec<Stmt>,
    ) -> Result<(u16, u16, Vec<Stmt>), ParseError> {
        if self.primitive_key_role(member.span, super::primitive_key::Role::Compound)
            || self.primitive_key_role(member.span, super::primitive_key::Role::Update)
        {
            return self.read_primitive_property(member, kind, owned, body);
        }
        let base = self.expr(&member.obj)?;
        super::reference::admit_object_receiver(self, member.span, base.kind)?;
        owned.extend(base.owned_roots);
        let (base_slot, saved) = self.bind_temp(base.expr, member.obj.span())?;
        body.extend(saved);
        owned.push(base_slot);
        let mut access = Vec::new();
        let units = self.member_key_units(member)?;
        let key = self.emit_utf16_node(&units, member.span, &mut access)?;
        owned.push(key);
        let reference = value::reference(
            Expr::Var(base_slot),
            value::field(key, "ref_slot"),
            value::field(key, "generation"),
            Expr::Bool(self.enclosing_strict),
        );
        let (reference, saved) =
            self.bind_typed(reference, value::reference_schema(), member.span)?;
        access.push(saved);
        let carrier = self.active_carrier(member.span)?;
        let line = self.line(member.span);
        self.reaffirm_root(carrier, base_slot, member.span, line, &mut access)?;
        self.reaffirm_root(carrier, key, member.span, line, &mut access)?;
        let helper = self.helper_ref_get.ok_or_else(|| {
            self.error(
                member.span,
                "P0 plain-object reads require the realm reference helper",
            )
        })?;
        let got = Self::guest_call(helper, vec![Expr::Var(carrier), Expr::Var(reference)]);
        let value = self.consume_value_result(got, member.span, &mut access)?;
        let (old, saved) = self.bind_evaluation(Expr::Var(value), kind, member.span)?;
        access.extend(saved);
        if self.has_root(old) {
            owned.push(old);
        }
        Ok((old, reference, access))
    }

    fn read_array(
        &mut self,
        object: &ast::Ident,
        index: i64,
        dynamic: bool,
        span: swc_common::Span,
        owned: &mut Vec<u16>,
        body: &mut Vec<Stmt>,
    ) -> Result<(u16, u16, Vec<Stmt>), ParseError> {
        let (binding, _) = *self
            .bindings
            .get(object.sym.as_str())
            .ok_or_else(|| self.error(span, "P3 binding was collected but not instantiated"))?;
        let (receiver, saved) = self.bind_temp(Expr::Var(binding), object.span)?;
        body.extend(saved);
        owned.push(receiver);
        let mut access = Vec::new();
        let carrier = self.active_carrier(span)?;
        let heap = self.heap_ref(span)?;
        let checked_base = heap.helpers.checked_base;
        let array_get = heap.helpers.array_get;
        let (base_tmp, saved) = self.bind_typed(
            Self::guest_call(
                checked_base,
                vec![
                    Expr::Var(carrier),
                    value::field(receiver, "ref_slot"),
                    value::field(receiver, "generation"),
                ],
            ),
            TypeSchema::Int,
            span,
        )?;
        access.push(saved);
        // Same reader as a dynamic index: present/extent stay inside array_get.
        // A hole is undefined, and ToNumber(undefined) is NaN, not the 0 stored
        // in an undefined box and not a cell past the physical block.
        let got = Self::guest_call(
            array_get,
            vec![Expr::Var(carrier), Expr::Var(base_tmp), Expr::Int(index)],
        );
        let (raw, saved) = self.bind_temp(got, span)?;
        access.extend(saved);
        owned.push(raw);
        if dynamic {
            // The sealed unary/update ToNumber consumer projects this JsValue.
            // Do not invent a string-constant arm here.
            return Ok((raw, receiver, access));
        }
        let tag = Self::as_number(value::field(raw, "tag"));
        let is_tag = |code: i64| {
            Expr::Eq(
                Box::new(tag.clone()),
                Box::new(Self::as_number(Expr::Int(code))),
            )
        };
        let string_number = match self.published_number_parser() {
            Some(parser) => Self::guest_call(
                parser,
                vec![
                    Expr::Var(carrier),
                    Expr::Var(raw),
                    Expr::Int(0),
                    Expr::Float(0.0),
                ],
            ),
            None => Expr::Float(f64::NAN),
        };
        let projected = Expr::IfElse {
            condition: Box::new(is_tag(value::NUMBER)),
            then_expr: Box::new(Self::as_number(value::field(raw, "number"))),
            else_expr: Box::new(Expr::IfElse {
                condition: Box::new(is_tag(value::UNDEFINED)),
                then_expr: Box::new(Expr::Float(f64::NAN)),
                else_expr: Box::new(Expr::IfElse {
                    condition: Box::new(is_tag(value::NULL)),
                    then_expr: Box::new(Expr::Float(0.0)),
                    else_expr: Box::new(Expr::IfElse {
                        condition: Box::new(is_tag(value::BOOLEAN)),
                        then_expr: Box::new(Expr::IfElse {
                            condition: Box::new(value::field(raw, "flag")),
                            then_expr: Box::new(Expr::Float(1.0)),
                            else_expr: Box::new(Expr::Float(0.0)),
                        }),
                        else_expr: Box::new(Expr::IfElse {
                            condition: Box::new(is_tag(value::STRING)),
                            then_expr: Box::new(string_number),
                            else_expr: Box::new(Expr::Float(f64::NAN)),
                        }),
                    }),
                }),
            }),
        };
        let (old, saved) = self.bind_temp_bare(value::number(projected), span)?;
        access.push(saved);
        Ok((old, receiver, access))
    }

    fn write_place(
        &mut self,
        place: &Place,
        saved: &Saved,
        value: u16,
        proof: Proof,
        stmts: &mut Vec<Stmt>,
        span: swc_common::Span,
    ) -> Result<(), ParseError> {
        match (place, saved) {
            (
                Place::Binding {
                    name,
                    span: name_span,
                },
                Saved::Binding { name: saved },
            ) => {
                if name != saved {
                    return Err(self.error(*name_span, "Primitive mutation place changed"));
                }
                let lowered = Lowered {
                    expr: Expr::Var(value),
                    kind: proof.result,
                    array_length: None,
                    owned_roots: Vec::new(),
                };
                super::reference::store_local(self, name, *name_span, lowered, stmts)?;
                Ok(())
            }
            (_, Saved::Property { reference }) => {
                let carrier = self.active_carrier(span)?;
                let helper = self
                    .objects
                    .ok_or_else(|| self.error(span, "ordinary runtime missing"))?
                    .put;
                self.consume_result(
                    Self::guest_call(
                        helper,
                        vec![Expr::Var(carrier), Expr::Var(*reference), Expr::Var(value)],
                    ),
                    TypeSchema::Null,
                    Expr::Null,
                    |_| value::undefined(),
                    span,
                    stmts,
                )?;
                Ok(())
            }
            (_, Saved::Array { receiver, index }) => {
                if proof.result != Kind::Number {
                    return Err(self.error(span, "P0 numeric arrays store only numbers"));
                }
                let carrier = self.active_carrier(span)?;
                let heap = self.heap_ref(span)?;
                let checked_base = heap.helpers.checked_base;
                let array_put = heap.helpers.array_put;
                let helper_read = heap.helper_read;
                let decode_length = heap.helpers.decode_u32;
                let decode = self.helper_decode.ok_or_else(|| {
                    self.error(span, "P0 array writes require the realm decode helper")
                })?;
                let line = self.line(span);
                let (base_tmp, saved) = self.bind_typed(
                    Self::guest_call(
                        checked_base,
                        vec![
                            Expr::Var(carrier),
                            value::field(*receiver, "ref_slot"),
                            value::field(*receiver, "generation"),
                        ],
                    ),
                    TypeSchema::Int,
                    span,
                )?;
                stmts.push(saved);
                let (kind_tmp, saved) = self.bind_typed(
                    Self::guest_call(
                        decode,
                        vec![Self::guest_call(
                            helper_read,
                            vec![Expr::Var(carrier), Self::as_number(Expr::Var(base_tmp))],
                        )],
                    ),
                    TypeSchema::Int,
                    span,
                )?;
                stmts.push(saved);
                stmts.push(assert_stmt(
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
                let (len_tmp, saved) = self.bind_typed(
                    Self::guest_call(
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
                    TypeSchema::Int,
                    span,
                )?;
                stmts.push(saved);
                stmts.push(assert_stmt(
                    Expr::Lt(Box::new(Expr::Int(*index)), Box::new(Expr::Var(len_tmp))),
                    line,
                ));
                let (present_raw, saved) = self.bind_typed(
                    Self::guest_call(
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
                    TypeSchema::Int,
                    span,
                )?;
                stmts.push(saved);
                let (present_tmp, saved) = self.bind_typed(
                    Expr::Sub(
                        Box::new(Expr::Var(present_raw)),
                        Box::new(Expr::Int(1)),
                    ),
                    TypeSchema::Int,
                    span,
                )?;
                stmts.push(saved);
                let mut inline = Vec::new();
                self.emit_cell_write(
                    carrier,
                    Expr::Add(
                        Box::new(Expr::Var(base_tmp)),
                        Box::new(Expr::Int(layout::ARRAY_HEADER_CELLS + *index)),
                    ),
                    value::field(value, "number"),
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
                            Expr::Int(*index),
                            Expr::Var(value),
                            value::field(*receiver, "ref_slot"),
                            value::field(*receiver, "generation"),
                        ],
                    ),
                    line,
                }];
                stmts.push(Stmt::IfElse {
                    condition: Expr::And(
                        Box::new(Expr::Eq(
                            Box::new(Expr::Var(kind_tmp)),
                            Box::new(Expr::Int(layout::KIND_ARRAY)),
                        )),
                        Box::new(Expr::Lt(
                            Box::new(Expr::Int(*index)),
                            Box::new(Expr::Var(present_tmp)),
                        )),
                    ),
                    then_branch: inline,
                    else_branch: via_put,
                    line,
                });
                Ok(())
            }
            (
                Place::ComputedArray { member },
                Saved::ComputedArray {
                    receiver,
                    key,
                    key_kind,
                },
            ) => {
                if proof.result != Kind::Number {
                    return Err(self.error(span, "P0 numeric arrays store only numbers"));
                }
                // One store, after the Get and the RHS already ran, with the
                // saved receiver and the saved key box. The store re-reads the
                // live base and re-checks kind/length/extent; no base pointer
                // survives the RHS and the key expression is never re-lowered.
                self.emit_array_store(
                    member,
                    *receiver,
                    None,
                    None,
                    Some((*key, *key_kind)),
                    value,
                    span,
                    stmts,
                )
            }
            _ => Err(self.error(span, "Primitive mutation place changed")),
        }
    }

    fn emit_compute(
        &mut self,
        proof: Proof,
        old: u16,
        rhs: Option<u16>,
        stmts: &mut Vec<Stmt>,
        span: swc_common::Span,
    ) -> Result<(u16, u16), ParseError> {
        if proof.op == MutationOp::AddAssign {
            let new_slot = self.emit_add(proof, old, rhs.expect("compound right"), stmts, span)?;
            return Ok((new_slot, new_slot));
        }
        let numeric = proof
            .op
            .numeric_operator()
            .expect("non-add mutation has a numeric operator");
        let left_role = if proof.op.update() {
            Role::Unary
        } else {
            Role::Left
        };
        let left = self.project_number(old, proof.left, span, numeric, left_role, stmts)?;
        let right = if let Some(rhs) = rhs {
            self.project_number(
                rhs,
                proof.right.expect("compound right"),
                span,
                numeric,
                Role::Right,
                stmts,
            )?
        } else {
            let (one, init) = self.bind_typed(Expr::Float(1.0), TypeSchema::Float, span)?;
            stmts.push(init);
            one
        };
        let computed = if let Some(bit) = proof.op.bitwise() {
            let (expr, mut extra) = self.bitwise_compute(bit, left, right, span)?;
            stmts.append(&mut extra);
            expr
        } else {
            // Exponentiation reuses the existing private power kernel over the
            // two already-projected Float slots, so the slots are passed by id
            // and never boxed or rebuilt.
            if proof.op == MutationOp::ExpAssign {
                super::super::prelude::math::number_power(left, right)
            } else {
                let left_expr = Box::new(var(left));
                let right_expr = Box::new(var(right));
                match proof.op {
                    MutationOp::SubAssign | MutationOp::Dec => Expr::Sub(left_expr, right_expr),
                    MutationOp::MulAssign => Expr::Mul(left_expr, right_expr),
                    MutationOp::DivAssign => Expr::Div(left_expr, right_expr),
                    MutationOp::RemAssign => Expr::Mod(left_expr, right_expr),
                    MutationOp::Inc => Expr::Add(left_expr, right_expr),
                    _ => unreachable!("numeric mutation"),
                }
            }
        };
        let (new_slot, init) = self.bind_temp_bare(value::number(computed), span)?;
        stmts.push(init);
        let result = if proof.op.update() && !proof.prefix {
            let (old_number, init) = self.bind_temp_bare(value::number(var(left)), span)?;
            stmts.push(init);
            old_number
        } else {
            new_slot
        };
        Ok((new_slot, result))
    }

    fn project_number(
        &mut self,
        slot: u16,
        mode: Mode,
        span: swc_common::Span,
        op: Operator,
        role: Role,
        stmts: &mut Vec<Stmt>,
    ) -> Result<u16, ParseError> {
        let projected = self.primitive_number_projection(slot, mode.kind(), span, op, role)?;
        let (float, saved) = self.bind_typed(projected, TypeSchema::Float, span)?;
        stmts.push(saved);
        Ok(float)
    }

    fn emit_add(
        &mut self,
        proof: Proof,
        left: u16,
        right: u16,
        stmts: &mut Vec<Stmt>,
        span: swc_common::Span,
    ) -> Result<u16, ParseError> {
        let line = self.line(span);
        let right_mode = proof.right.expect("compound right");
        for (slot, mode) in [(left, proof.left), (right, right_mode)] {
            if mode == Mode::Dynamic {
                let is = |tag| eq(value::field(slot, "tag"), int(tag));
                let primitive = or(
                    or(is(value::NUMBER), is(value::BOOLEAN)),
                    or(or(is(value::NULL), is(value::UNDEFINED)), is(value::STRING)),
                );
                stmts.push(Stmt::Expr {
                    expr: call(BuiltinFunction::Assert.call_index(), vec![primitive]),
                    line,
                });
            }
        }
        let managed = stringish(proof.left) || stringish(right_mode);
        let (out, init) = if managed {
            self.bind_temp(value::undefined(), span)?
        } else {
            let (out, init) = self.bind_temp_bare(value::number(Expr::Float(0.0)), span)?;
            (out, vec![init])
        };
        stmts.extend(init);
        let mut numeric = Vec::new();
        if proof.left != Mode::String && right_mode != Mode::String {
            let left_expr = primitive_number::non_string_projection(left, proof.left.kind())
                .ok_or_else(|| self.error(span, "String cannot enter numeric projection"))?;
            let (a, init) = self.bind_typed(left_expr, TypeSchema::Float, span)?;
            numeric.push(init);
            let right_expr = primitive_number::non_string_projection(right, right_mode.kind())
                .ok_or_else(|| self.error(span, "String cannot enter numeric projection"))?;
            let (b, init) = self.bind_typed(right_expr, TypeSchema::Float, span)?;
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
        if managed {
            let (a, init) = self.bind_temp(value::undefined(), span)?;
            stmts.extend(init);
            let (b, init) = self.bind_temp(value::undefined(), span)?;
            stmts.extend(init);
            let left_mode = if !stringish(right_mode) {
                Kind::String
            } else {
                proof.left.kind()
            };
            let right_kind = if !stringish(proof.left) {
                Kind::String
            } else {
                right_mode.kind()
            };
            let mut concat = self.addition_string_from_kind(left, a, left_mode, span)?;
            concat.extend(self.addition_string_from_kind(right, b, right_kind, span)?);
            let helper = self.helper_string_concat.ok_or_else(|| {
                self.error(
                    span,
                    "Add concat helper was not selected before runtime indices",
                )
            })?;
            let carrier = self.active_carrier(span)?;
            let frame = self.active_frame(span)?;
            let entry = self.entry_for(out, span)?;
            concat.push(assign(
                out,
                Self::guest_call(
                    helper,
                    vec![
                        var(carrier),
                        var(a),
                        var(b),
                        var(frame),
                        int(i64::from(entry)),
                    ],
                ),
                line,
            ));
            self.refresh_root(out, span, &mut concat)?;
            if proof.left == Mode::String || right_mode == Mode::String {
                stmts.extend(concat);
            } else {
                stmts.push(branch(
                    or(
                        eq(value::field(left, "tag"), int(value::STRING)),
                        eq(value::field(right, "tag"), int(value::STRING)),
                    ),
                    concat,
                    numeric,
                    line,
                ));
            }
        } else {
            stmts.extend(numeric);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::super::Kind;
    use super::super::primitive_number::{Mode, Role};
    use super::{MutationOp, Owner, Proof, State};

    fn sample(op: MutationOp, prefix: bool, role: Role, owner: Owner, left: Mode) -> Proof {
        Proof {
            op,
            prefix,
            role,
            owner,
            left,
            right: if op.update() {
                None
            } else {
                Some(Mode::Number)
            },
            result: if op == MutationOp::AddAssign && left == Mode::String {
                Kind::String
            } else {
                Kind::Number
            },
        }
    }

    #[test]
    fn assignment149_probe_publishes_nothing() {
        let state = State::default();
        let proof = sample(
            MutationOp::AddAssign,
            false,
            Role::Left,
            Owner::Local,
            Mode::Number,
        );
        super::register(&state, (1, 8), proof, true).unwrap();
        assert!(
            state.sites.borrow().is_empty(),
            "probe must not publish a mutation proof"
        );
        super::register(&state, (1, 8), proof, false).unwrap();
        assert_eq!(state.sites.borrow().len(), 1);
        super::register(&state, (1, 8), proof, true).unwrap();
        assert_eq!(state.sites.borrow().len(), 1);
    }

    #[test]
    fn assignment149_op_prefix_role_owner_mismatch_and_empty_seal() {
        let state = State::default();
        let proof = sample(
            MutationOp::AddAssign,
            false,
            Role::Left,
            Owner::Local,
            Mode::Number,
        );
        super::register(&state, (1, 8), proof, false).unwrap();
        let mut changed = proof;
        changed.op = MutationOp::SubAssign;
        assert!(super::register(&state, (1, 8), changed, false).is_err());
        changed = proof;
        changed.prefix = true;
        assert!(super::register(&state, (1, 8), changed, false).is_err());
        changed = proof;
        changed.role = Role::Unary;
        changed.right = None;
        assert!(super::register(&state, (1, 8), changed, false).is_err());
        changed = proof;
        changed.owner = Owner::Property;
        assert!(super::register(&state, (1, 8), changed, false).is_err());
        assert_eq!(state.sites.borrow().get(&(1, 8)), Some(&proof));
        let empty = State::default();
        empty.sealed.set(true);
        assert!(
            super::register(&empty, (1, 8), proof, false)
                .unwrap_err()
                .contains("not sealed")
        );
        assert!(empty.sites.borrow().is_empty());
    }

    #[test]
    fn assignment149_probe_requires_sealed_coverage_not_first_entry() {
        let state = State::default();
        let narrow = sample(
            MutationOp::AddAssign,
            false,
            Role::Left,
            Owner::Local,
            Mode::Number,
        );
        let mut wide = narrow;
        wide.left = Mode::Dynamic;
        wide.result = Kind::Dynamic;
        super::register(&state, (4, 12), wide, false).unwrap();
        state.sealed.set(true);
        super::register(&state, (4, 12), narrow, true).unwrap();
        let err = super::register(&state, (4, 12), wide, true);
        assert!(err.is_ok());
        let narrowed = State::default();
        super::register(&narrowed, (4, 12), narrow, false).unwrap();
        narrowed.sealed.set(true);
        let uncovered = super::register(&narrowed, (4, 12), wide, true).unwrap_err();
        assert!(uncovered.contains("not covered"));
        assert!(
            super::register(&narrowed, (9, 12), narrow, true)
                .unwrap_err()
                .contains("not covered")
        );
    }

    #[test]
    fn assignment149_update_role_rejected_and_probe_flag_not_required_to_panic() {
        let state = State::default();
        let mut update = sample(
            MutationOp::Inc,
            true,
            Role::Unary,
            Owner::Parameter,
            Mode::Dynamic,
        );
        update.right = Some(Mode::Number);
        update.result = Kind::Number;
        assert!(
            super::register(&state, (2, 6), update, false)
                .unwrap_err()
                .contains("unary")
        );
        assert!(state.sites.borrow().is_empty());
        update.right = None;
        update.role = Role::Left;
        assert!(super::register(&state, (2, 6), update, false).is_err());
        update.role = Role::Unary;
        super::register(&state, (2, 6), update, false).unwrap();
        assert_eq!(state.sites.borrow().len(), 1);
    }
}
