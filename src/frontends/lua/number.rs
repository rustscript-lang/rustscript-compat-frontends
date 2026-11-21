//! Lower already-classified Lua numbers onto Core binary64 arithmetic.
//!
//! The caller has finished numeric classification. This module does not coerce
//! strings, invoke metamethods, test truthiness, or treat a Core `Int` as a
//! Lua object. Addition, subtraction, multiplication, division, and negation
//! are Core f64 operations. Floor remainder is not Core `Mod`: that opcode is
//! truncating. Power is `math::powf`.
//!
//! Remainder temps are names outside the Lua identifier grammar. They are
//! probed on the same builder, because `alloc_local_named` reuses a slot for
//! an existing name and unnamed allocation is private. A process-wide counter
//! or a hard-to-guess legal identifier is not a private namespace.
//!
//! Each remainder takes two fresh frame slots. Core locals are frame-scoped,
//! and this kernel does not own statement boundaries, so it cannot reuse a
//! slot that a still-live nested expression might need. A future lowerer must
//! bound that cost before production wiring. This module does not.

use vm::{BuiltinFunction, Expr, LocalIrBuilder, LocalSlot, ParseError, Stmt};

/// A Lua number whose classification is already done.
///
/// This kernel checks only a directly visible literal. A visible `Int`, or
/// another non-float literal, is rejected even in release builds. It is not
/// coerced. `Var` and `Call` are not inspected: matching those variants does
/// not prove binary64. Full-pipeline integration must pass
/// [`FloatTypeEvidence`] only after the Lua numeric classifier has established
/// Float.
#[derive(Clone, Debug)]
pub(crate) struct ClassifiedNumber {
    expr: Expr,
}

/// Caller attestation that an expression is already binary64.
///
/// Constructing this does not inspect the expression. The future numeric
/// classifier is the production source of this token. This kernel does not
/// re-check `Var` or `Call`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FloatTypeEvidence {
    _private: (),
}

impl FloatTypeEvidence {
    pub(crate) const fn attested() -> Self {
        Self { _private: () }
    }
}

impl ClassifiedNumber {
    pub(crate) fn literal(value: f64) -> Self {
        Self {
            expr: Expr::Float(value),
        }
    }

    /// `evidence` is the caller's proof. A directly visible non-float literal
    /// still fails, because that proof is contradicted by the IR the kernel
    /// can see.
    pub(crate) fn from_attested_float(
        expr: Expr,
        _evidence: FloatTypeEvidence,
    ) -> Result<Self, ParseError> {
        if let Some(kind) = directly_known_non_float_literal(&expr) {
            return Err(ParseError::new(format!(
                "classified Lua number requires binary64, found Core {kind} literal"
            )));
        }
        Ok(Self { expr })
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

    pub(crate) fn into_expr(self) -> Expr {
        self.expr
    }
}

/// Temps for operations that must evaluate each operand once and then reuse it.
///
/// Slots stay live for the returned expression. Calling `floor_mod` again on
/// this lowering allocates new slots; it does not reclaim the previous pair.
pub(crate) struct NumberLowering<'a> {
    builder: &'a mut LocalIrBuilder,
}

impl<'a> NumberLowering<'a> {
    pub(crate) fn new(builder: &'a mut LocalIrBuilder) -> Self {
        Self { builder }
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

    /// Bind a guest local on the same builder that owns remainder temps.
    pub(crate) fn alloc_local_named(&mut self, name: &str) -> Result<LocalSlot, ParseError> {
        self.builder.alloc_local_named(name)
    }

    /// Bind an already classified value to a fresh temp.
    ///
    /// This does not search for a dead slot. Core compacts the frame later.
    pub(crate) fn store(
        &mut self,
        value: ClassifiedNumber,
        stmts: &mut Vec<Stmt>,
    ) -> Result<ClassifiedNumber, ParseError> {
        let slot = self.alloc_temp("value")?;
        stmts.push(bind_temp(slot, value.expr));
        ClassifiedNumber::from_attested_float(Expr::Var(slot), FloatTypeEvidence::attested())
    }

    /// Keep a chain step shallow when the value is not already a literal or slot.
    pub(crate) fn materialize(
        &mut self,
        value: ClassifiedNumber,
        stmts: &mut Vec<Stmt>,
    ) -> Result<ClassifiedNumber, ParseError> {
        if matches!(value.expr, Expr::Float(_) | Expr::Var(_)) {
            return Ok(value);
        }
        self.store(value, stmts)
    }

    fn alloc_temp(&mut self, role: &str) -> Result<LocalSlot, ParseError> {
        // `$` and `.` are outside `[A-Za-z_][A-Za-z0-9_]*`, so a Lua local
        // cannot use this name. The probe is what makes a second lowering on
        // the same builder fresh; the ordinal is not a process-wide counter.
        let mut ordinal = 0u32;
        loop {
            let name = format!("$lua.number.{role}.{ordinal}");
            if self.builder.resolve_local_expr(&name).is_none() {
                return self.builder.alloc_local_named(&name);
            }
            ordinal = ordinal
                .checked_add(1)
                .ok_or_else(|| ParseError::new("lua number temp ordinal overflow"))?;
        }
    }
}

fn directly_known_non_float_literal(expr: &Expr) -> Option<&'static str> {
    match expr {
        Expr::Int(_) => Some("Int"),
        Expr::Null => Some("Null"),
        Expr::Bool(_) => Some("Bool"),
        Expr::String(_) => Some("String"),
        Expr::Bytes(_) => Some("Bytes"),
        _ => None,
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
