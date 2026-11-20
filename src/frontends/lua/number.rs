//! Lower already-classified Lua numbers onto Core binary64 arithmetic.
//!
//! The caller has finished numeric classification. This module does not coerce
//! strings, invoke metamethods, test truthiness, or treat a Core `Int` as a
//! Lua object. Addition, subtraction, multiplication, division, and negation
//! are Core f64 operations. Floor remainder is not Core `Mod`: that opcode is
//! truncating. Power is `math::powf`.

use vm::{BuiltinFunction, Expr, LocalIrBuilder, LocalSlot, ParseError, Stmt};

/// A Lua number whose classification is already done.
///
/// `expr` must evaluate to a binary64 value. Passing a Core `Int` is outside
/// this ABI.
#[derive(Clone, Debug)]
pub(crate) struct ClassifiedNumber {
    expr: Expr,
}

impl ClassifiedNumber {
    pub(crate) fn from_f64_expr(expr: Expr) -> Self {
        Self { expr }
    }

    pub(crate) fn literal(value: f64) -> Self {
        Self {
            expr: Expr::Float(value),
        }
    }

    pub(crate) fn neg(self) -> Expr {
        Expr::Neg(Box::new(self.expr))
    }

    pub(crate) fn add(self, rhs: Self) -> Expr {
        Expr::Add(Box::new(self.expr), Box::new(rhs.expr))
    }

    pub(crate) fn sub(self, rhs: Self) -> Expr {
        Expr::Sub(Box::new(self.expr), Box::new(rhs.expr))
    }

    pub(crate) fn mul(self, rhs: Self) -> Expr {
        Expr::Mul(Box::new(self.expr), Box::new(rhs.expr))
    }

    pub(crate) fn div(self, rhs: Self) -> Expr {
        Expr::Div(Box::new(self.expr), Box::new(rhs.expr))
    }

    pub(crate) fn pow(self, exponent: Self) -> Expr {
        Expr::Call(
            BuiltinFunction::MathPowF.call_index(),
            Vec::new(),
            vec![self.expr, exponent.expr],
            None,
            None,
        )
    }
}

/// Temps for operations that must evaluate each operand once and then reuse it.
pub(crate) struct NumberLowering<'a> {
    builder: &'a mut LocalIrBuilder,
    temp_id: u32,
}

impl<'a> NumberLowering<'a> {
    pub(crate) fn new(builder: &'a mut LocalIrBuilder) -> Self {
        Self {
            builder,
            temp_id: 0,
        }
    }

    /// Lua `%` on binary64: `lhs - floor(lhs / rhs) * rhs`.
    ///
    /// Each operand is bound once before the formula, so a side effect in an
    /// operand runs once and left-to-right.
    pub(crate) fn floor_mod(
        &mut self,
        lhs: ClassifiedNumber,
        rhs: ClassifiedNumber,
    ) -> Result<Expr, ParseError> {
        let lhs_slot = self.alloc_temp("mod_lhs")?;
        let rhs_slot = self.alloc_temp("mod_rhs")?;
        let lhs_var = Expr::Var(lhs_slot);
        let rhs_var = Expr::Var(rhs_slot);
        let quotient = Expr::Div(Box::new(lhs_var.clone()), Box::new(rhs_var.clone()));
        let product = Expr::Mul(Box::new(call_math_floor(quotient)), Box::new(rhs_var));
        let remainder = Expr::Sub(Box::new(lhs_var), Box::new(product));
        Ok(Expr::Block {
            stmts: vec![bind_temp(lhs_slot, lhs.expr), bind_temp(rhs_slot, rhs.expr)],
            expr: Box::new(remainder),
        })
    }

    fn alloc_temp(&mut self, role: &str) -> Result<LocalSlot, ParseError> {
        let name = format!("__lua_number_{role}_{}", self.temp_id);
        self.temp_id += 1;
        self.builder.alloc_local_named(&name)
    }
}

fn bind_temp(slot: LocalSlot, expr: Expr) -> Stmt {
    Stmt::Let {
        index: slot,
        declared_schema: None,
        expr,
        line: 1,
    }
}

fn call_math_floor(argument: Expr) -> Expr {
    Expr::Call(
        BuiltinFunction::MathFloor.call_index(),
        Vec::new(),
        vec![argument],
        None,
        None,
    )
}
