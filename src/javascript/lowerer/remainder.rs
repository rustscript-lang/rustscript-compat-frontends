//! Bounded numeric remainder on checked, bound Float operands.
use super::super::prelude::object::{assign, branch, var};
use super::primitive_number::{Operator, Role};
use super::*;

impl Lowerer<'_> {
    pub(super) fn lower_remainder(&mut self, binary: &ast::BinExpr) -> Result<Lowered, ParseError> {
        let span = binary.span;
        let line = self.line(span);
        let left = self.expr(&binary.left)?;
        let right = self.expr(&binary.right)?;
        let mut owned = left.owned_roots;
        owned.extend(right.owned_roots);
        // Snapshot/root the original left box before evaluating the right.
        let (l, mut stmts) = self.bind_evaluation(left.expr, left.kind, binary.left.span())?;
        let out = if self.pending_normal.is_some() {
            let (out, init) = self.bind_temp_bare(value::number(Expr::Float(0.0)), span)?;
            stmts.push(init);
            Some(out)
        } else {
            None
        };
        let (r, mut rhs) = self.bind_evaluation(right.expr, right.kind, binary.right.span())?;
        owned.extend([l, r].into_iter().filter(|slot| self.has_root(*slot)));
        let (a, project_left) = self.bind_typed(
            self.primitive_number_projection(l, left.kind, span, Operator::Remainder, Role::Left)?,
            TypeSchema::Float,
            span,
        )?;
        let (b, project_right) = self.bind_typed(
            self.primitive_number_projection(
                r,
                right.kind,
                span,
                Operator::Remainder,
                Role::Right,
            )?,
            TypeSchema::Float,
            span,
        )?;
        let mut compute = vec![project_left, project_right];
        let boxed = value::number(Expr::Mod(Box::new(var(a)), Box::new(var(b))));
        let result = if let Some(normal) = self.pending_normal {
            let out = out.expect("pending remainder has a dominating placeholder");
            compute.push(assign(out, boxed, line));
            // A right throw suppresses BOTH checked projections. Recheck after
            // the right operand rather than projecting in the left-normal arm.
            rhs.push(branch(var(normal), compute, vec![], line));
            stmts.push(branch(var(normal), rhs, vec![], line));
            var(out)
        } else {
            stmts.append(&mut rhs);
            stmts.append(&mut compute);
            boxed
        };
        let owned_roots =
            Self::compose_owned(owned.into_iter().filter(|slot| self.has_root(*slot)));
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
