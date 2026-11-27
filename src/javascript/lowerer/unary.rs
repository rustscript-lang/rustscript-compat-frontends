//! GetValue once, then discard or convert a sealed primitive numeric operand.
use super::super::prelude::object::{assign, branch, var};
use super::*;

/// New unary results are scalar even when their operands hide managed work in
/// a sequence or conditional. Inspect the full operand without changing the
/// historical scanner for programs containing neither new operator.
/// This only selects the existing realm path; validation still checks all arms.
pub(super) fn operand_needs_heap(expr: &ast::Expr) -> bool {
    match expr {
        ast::Expr::Cond(c) => {
            operand_needs_heap(&c.test) || operand_needs_heap(&c.cons) || operand_needs_heap(&c.alt)
        }
        ast::Expr::Seq(s) => s.exprs.iter().any(|e| operand_needs_heap(e)),
        ast::Expr::Paren(p) => operand_needs_heap(&p.expr),
        ast::Expr::Bin(b) => operand_needs_heap(&b.left) || operand_needs_heap(&b.right),
        ast::Expr::Unary(u) => u.op == ast::UnaryOp::TypeOf || operand_needs_heap(&u.arg),
        ast::Expr::Assign(a) => expr_needs_heap(expr) || operand_needs_heap(&a.right),
        _ => expr_needs_heap(expr),
    }
}

impl Lowerer<'_> {
    pub(super) fn lower_void(&mut self, unary: &ast::UnaryExpr) -> Result<Lowered, ParseError> {
        let operand = self.expr(&unary.arg)?;
        let (slot, stmts) = self.bind_evaluation(operand.expr, operand.kind, unary.arg.span())?;
        let owned_roots = Self::compose_owned(
            operand
                .owned_roots
                .into_iter()
                .chain(std::iter::once(slot))
                .filter(|slot| self.has_root(*slot)),
        );
        // This local Undefined does not change pending completion or its rooted
        // payload. The enclosing completion path still observes an operand throw.
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(value::undefined()),
            },
            kind: Kind::Undefined,
            array_length: None,
            owned_roots,
        })
    }

    pub(super) fn lower_unary_plus(
        &mut self,
        unary: &ast::UnaryExpr,
    ) -> Result<Lowered, ParseError> {
        let operand = self.expr(&unary.arg)?;
        if matches!(
            operand.kind,
            Kind::Boolean | Kind::Null | Kind::Undefined | Kind::String | Kind::Dynamic
        ) {
            return self.lower_primitive_unary_plus(unary, operand);
        }
        reference::admit_numeric_operand(self, unary.span, operand.kind)?;
        let (slot, mut stmts) =
            self.bind_evaluation(operand.expr, operand.kind, unary.arg.span())?;
        let owned_roots = Self::compose_owned(
            operand
                .owned_roots
                .into_iter()
                .chain(std::iter::once(slot))
                .filter(|slot| self.has_root(*slot)),
        );
        let projected = value::number(self.numeric_projection(slot, operand.kind));
        let result = if let Some(normal) = self.pending_normal {
            // A typed dominating box is needed only for the abrupt join. Even
            // proved Number field access must occur after the normal recheck.
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
}
