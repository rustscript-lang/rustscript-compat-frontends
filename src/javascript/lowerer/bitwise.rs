//! Exact, bounded 32-bit Number operations using typed Float primitives.
//! Shared primitive conversion precedes unchanged bounded Float algorithms.
use super::super::prelude::object::{
    add, and, assign, branch, call, choose, eq, lt, mul, not, or, sub, var,
};
use super::primitive_number::{Operator, Role};
use super::*;

#[derive(Clone, Copy)]
pub(in super::super) enum Operation {
    And,
    Or,
    Xor,
    Left,
    Right,
    UnsignedRight,
}
impl Operation {
    pub(in super::super) fn from_op(op: ast::BinaryOp) -> Option<Self> {
        Some(match op {
            ast::BinaryOp::BitAnd => Self::And,
            ast::BinaryOp::BitOr => Self::Or,
            ast::BinaryOp::BitXor => Self::Xor,
            ast::BinaryOp::LShift => Self::Left,
            ast::BinaryOp::RShift => Self::Right,
            ast::BinaryOp::ZeroFillRShift => Self::UnsignedRight,
            _ => return None,
        })
    }
}
const MODULUS: f64 = 4294967296.0;
fn f(x: f64) -> Expr {
    Expr::Float(x)
}
fn modulo(a: Expr, b: Expr) -> Expr {
    Expr::Mod(Box::new(a), Box::new(b))
}
fn div(a: Expr, b: Expr) -> Expr {
    Expr::Div(Box::new(a), Box::new(b))
}
fn math(b: BuiltinFunction, x: Expr) -> Expr {
    call(b.call_index(), vec![x])
}
fn positive_zero(x: Expr) -> Expr {
    choose(eq(x.clone(), f(0.0)), f(0.0), x)
}
fn signed(x: Expr) -> Expr {
    choose(
        lt(x.clone(), f(2147483648.0)),
        x.clone(),
        sub(x, f(MODULUS)),
    )
}

impl Lowerer<'_> {
    /// Lazily bypass trunc/remainder for zero and nonfinite payloads. The
    /// remainder, unlike quotient reconstruction, retains low bits of huge x.
    fn bitwise_uint32(
        &mut self,
        x: u16,
        span: swc_common::Span,
    ) -> Result<(u16, Vec<Stmt>), ParseError> {
        let line = self.line(span);
        let (u, init) = self.bind_typed(f(0.0), TypeSchema::Float, span)?;
        let (t, trunc) = self.bind_typed(
            math(BuiltinFunction::MathTrunc, var(x)),
            TypeSchema::Float,
            span,
        )?;
        let (r, rem) = self.bind_typed(modulo(var(t), f(MODULUS)), TypeSchema::Float, span)?;
        Ok((
            u,
            vec![
                init,
                branch(
                    and(
                        math(BuiltinFunction::MathIsFinite, var(x)),
                        not(eq(var(x), f(0.0))),
                    ),
                    vec![
                        trunc,
                        rem,
                        assign(
                            u,
                            positive_zero(choose(
                                lt(var(r), f(0.0)),
                                add(var(r), f(MODULUS)),
                                var(r),
                            )),
                            line,
                        ),
                    ],
                    vec![],
                    line,
                ),
            ],
        ))
    }

    pub(super) fn bitwise_compute(
        &mut self,
        op: Operation,
        a: u16,
        b: u16,
        span: swc_common::Span,
    ) -> Result<(Expr, Vec<Stmt>), ParseError> {
        let line = self.line(span);
        let (u, mut stmts) = self.bitwise_uint32(a, span)?;
        let (v, mut rhs) = self.bitwise_uint32(b, span)?;
        stmts.append(&mut rhs);
        let result = match op {
            Operation::And | Operation::Or | Operation::Xor => {
                let (out, init) = self.bind_typed(f(0.0), TypeSchema::Float, span)?;
                let (weight, wi) = self.bind_typed(f(1.0), TypeSchema::Float, span)?;
                let (count, ci) = self.bind_typed(Expr::Int(0), TypeSchema::Int, span)?;
                stmts.extend([init, wi, ci]);
                let ap = eq(modulo(var(u), f(2.0)), f(1.0));
                let bp = eq(modulo(var(v), f(2.0)), f(1.0));
                let take = match op {
                    Operation::And => and(ap, bp),
                    Operation::Or => or(ap, bp),
                    Operation::Xor => not(eq(ap, bp)),
                    _ => unreachable!(),
                };
                // Exactly 32 iterations for all inputs, constant scalar scratch.
                stmts.push(Stmt::While {
                    condition: lt(var(count), Expr::Int(32)),
                    body: vec![
                        branch(
                            take,
                            vec![assign(out, add(var(out), var(weight)), line)],
                            vec![],
                            line,
                        ),
                        assign(
                            u,
                            math(BuiltinFunction::MathFloor, div(var(u), f(2.0))),
                            line,
                        ),
                        assign(
                            v,
                            math(BuiltinFunction::MathFloor, div(var(v), f(2.0))),
                            line,
                        ),
                        assign(weight, mul(var(weight), f(2.0)), line),
                        assign(count, add(var(count), Expr::Int(1)), line),
                    ],
                    line,
                });
                signed(var(out))
            }
            Operation::Left | Operation::Right | Operation::UnsignedRight => {
                let (count, ci) =
                    self.bind_typed(modulo(var(v), f(32.0)), TypeSchema::Float, span)?;
                let (scale, si) = self.bind_typed(f(1.0), TypeSchema::Float, span)?;
                let (cursor, ii) = self.bind_typed(f(0.0), TypeSchema::Float, span)?;
                stmts.extend([ci, si, ii]);
                // Count is in [0,31]. Exact power-of-two scaling, no powf.
                stmts.push(Stmt::While {
                    condition: lt(var(cursor), var(count)),
                    body: vec![
                        assign(scale, mul(var(scale), f(2.0)), line),
                        assign(cursor, add(var(cursor), f(1.0)), line),
                    ],
                    line,
                });
                match op {
                    Operation::Left => signed(modulo(mul(var(u), var(scale)), f(MODULUS))),
                    Operation::Right => {
                        math(BuiltinFunction::MathFloor, div(signed(var(u)), var(scale)))
                    }
                    Operation::UnsignedRight => {
                        math(BuiltinFunction::MathFloor, div(var(u), var(scale)))
                    }
                    _ => unreachable!(),
                }
            }
        };
        Ok((positive_zero(result), stmts))
    }

    pub(in super::super) fn lower_bitwise(
        &mut self,
        binary: &ast::BinExpr,
    ) -> Result<Lowered, ParseError> {
        let span = binary.span;
        let line = self.line(span);
        let op = Operation::from_op(binary.op).expect("exact bitwise route");
        let conversion_op = Operator::binary(binary.op).expect("exact bitwise conversion");
        let left = self.expr(&binary.left)?;
        let right = self.expr(&binary.right)?;
        let mut owned = left.owned_roots;
        owned.extend(right.owned_roots);
        let (l, mut stmts) = self.bind_evaluation(left.expr, left.kind, binary.left.span())?;
        let out = if self.pending_normal.is_some() {
            let (out, init) = self.bind_temp_bare(value::number(f(0.0)), span)?;
            stmts.push(init);
            Some(out)
        } else {
            None
        };
        let (r, mut rhs) = self.bind_evaluation(right.expr, right.kind, binary.right.span())?;
        owned.extend([l, r].into_iter().filter(|slot| self.has_root(*slot)));
        let (a, pa) = self.bind_typed(
            self.primitive_number_projection(l, left.kind, span, conversion_op, Role::Left)?,
            TypeSchema::Float,
            span,
        )?;
        let (b, pb) = self.bind_typed(
            self.primitive_number_projection(r, right.kind, span, conversion_op, Role::Right)?,
            TypeSchema::Float,
            span,
        )?;
        let (result, mut computation) = self.bitwise_compute(op, a, b, span)?;
        let mut compute = vec![pa, pb];
        compute.append(&mut computation);
        let boxed = value::number(result);
        let result = if let Some(normal) = self.pending_normal {
            let out = out.expect("pending bitwise placeholder");
            compute.push(assign(out, boxed, line));
            // Both original evaluations finish before either projection. A
            // throwing RHS therefore wins over an invalid actual LHS type.
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
            owned_roots: Self::compose_owned(owned.into_iter().filter(|slot| self.has_root(*slot))),
        })
    }

    pub(in super::super) fn lower_bitwise_not(
        &mut self,
        unary: &ast::UnaryExpr,
    ) -> Result<Lowered, ParseError> {
        let span = unary.span;
        let line = self.line(span);
        let operand = self.expr(&unary.arg)?;
        let (slot, mut stmts) =
            self.bind_evaluation(operand.expr, operand.kind, unary.arg.span())?;
        let owned_roots = Self::compose_owned(
            operand
                .owned_roots
                .into_iter()
                .chain(std::iter::once(slot))
                .filter(|slot| self.has_root(*slot)),
        );
        let (x, projection) = self.bind_typed(
            self.primitive_number_projection(slot, operand.kind, span, Operator::Not, Role::Unary)?,
            TypeSchema::Float,
            span,
        )?;
        let (u, mut conversion) = self.bitwise_uint32(x, span)?;
        let mut compute = vec![projection];
        compute.append(&mut conversion);
        let boxed = value::number(positive_zero(signed(sub(f(4294967295.0), var(u)))));
        let result = if let Some(normal) = self.pending_normal {
            let (out, init) = self.bind_temp_bare(value::number(f(0.0)), span)?;
            stmts.push(init);
            compute.push(assign(out, boxed, line));
            stmts.push(branch(var(normal), compute, vec![], line));
            var(out)
        } else {
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
            owned_roots,
        })
    }
}
