//! Lower already-classified Lua numbers onto Core binary64 arithmetic.
//!
//! The caller has finished numeric classification. This module does not coerce
//! strings, invoke metamethods, test truthiness, or accept a Core expression as
//! proof of binary64. Addition, subtraction, multiplication, division, and
//! negation are Core f64 operations. Floor remainder is not Core `Mod`: that
//! opcode is truncating. Power is `math::powf`.
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
//!
//! `ClassifiedNumber` is closed. Arithmetic returns the same type, and
//! `into_expr` is only for the statement that consumes the value. A local or
//! temp is a typed reference returned by the operation that just emitted the
//! binding. There is no constructor from an arbitrary `Expr` or `LocalSlot`.

use vm::{BuiltinFunction, Expr, LocalIrBuilder, LocalSlot, ParseError, Stmt};

/// A Lua number built only from a binary64 literal or from an operation on one.
#[derive(Clone, Debug)]
pub(crate) struct ClassifiedNumber {
    expr: Expr,
}

impl ClassifiedNumber {
    pub(crate) fn literal(value: f64) -> Self {
        Self {
            expr: Expr::Float(value),
        }
    }

    pub(crate) fn neg(self) -> Self {
        Self {
            expr: Expr::Neg(Box::new(self.expr)),
        }
    }

    pub(crate) fn add(self, rhs: Self) -> Self {
        Self {
            expr: Expr::Add(Box::new(self.expr), Box::new(rhs.expr)),
        }
    }

    pub(crate) fn sub(self, rhs: Self) -> Self {
        Self {
            expr: Expr::Sub(Box::new(self.expr), Box::new(rhs.expr)),
        }
    }

    pub(crate) fn mul(self, rhs: Self) -> Self {
        Self {
            expr: Expr::Mul(Box::new(self.expr), Box::new(rhs.expr)),
        }
    }

    pub(crate) fn div(self, rhs: Self) -> Self {
        Self {
            expr: Expr::Div(Box::new(self.expr), Box::new(rhs.expr)),
        }
    }

    pub(crate) fn pow(self, exponent: Self) -> Self {
        Self {
            expr: Expr::Call(
                BuiltinFunction::MathPowF.call_index(),
                Vec::new(),
                vec![self.expr, exponent.expr],
                None,
                None,
            ),
        }
    }

    /// Final consumption. Callers that still need a classified value must use
    /// the reference returned by `bind_named`, `store`, or `materialize`.
    pub(crate) fn into_expr(self) -> Expr {
        self.expr
    }

    /// Slot named by a binding this kernel just emitted.
    ///
    /// Literals and arithmetic results name no slot. This does not classify a
    /// caller-supplied slot.
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn bound_slot(&self) -> Option<LocalSlot> {
        match &self.expr {
            Expr::Var(slot) => Some(*slot),
            _ => None,
        }
    }

    /// Closed test producer. The value expression is always `Expr::Float`.
    ///
    /// `before` may observe evaluation. It is not a number source, and it
    /// cannot turn an external slot into the produced value.
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn from_observed_literal(before: Vec<Stmt>, value: f64) -> Self {
        if before.is_empty() {
            return Self::literal(value);
        }
        Self {
            expr: Expr::Block {
                stmts: before,
                expr: Box::new(Expr::Float(value)),
            },
        }
    }
}

/// An external Core slot was not emitted from a classified value.
#[cfg(test)]
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UnprovenSlot {
    pub(crate) slot: LocalSlot,
}

/// Refuse an arbitrary slot. There is no success path.
///
/// A typed reference exists only as the return value of `bind_named`,
/// `store`, or `materialize`.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn external_slot_evidence(slot: LocalSlot) -> Result<ClassifiedNumber, UnprovenSlot> {
    Err(UnprovenSlot { slot })
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
    ) -> Result<ClassifiedNumber, ParseError> {
        let lhs_slot = self.alloc_temp("mod_lhs")?;
        let rhs_slot = self.alloc_temp("mod_rhs")?;
        let lhs_var = Expr::Var(lhs_slot);
        let rhs_var = Expr::Var(rhs_slot);
        let quotient = Expr::Div(Box::new(lhs_var.clone()), Box::new(rhs_var.clone()));
        let product = Expr::Mul(Box::new(call_math_floor(quotient)), Box::new(rhs_var));
        let remainder = Expr::Sub(Box::new(lhs_var), Box::new(product));
        Ok(ClassifiedNumber {
            expr: Expr::Block {
                stmts: vec![
                    bind_temp(lhs_slot, lhs.into_expr()),
                    bind_temp(rhs_slot, rhs.into_expr()),
                ],
                expr: Box::new(remainder),
            },
        })
    }

    /// Emit a guest binding of an already classified value.
    ///
    /// The returned reference names that binding. It is not rebuilt from a
    /// slot the caller already had.
    pub(crate) fn bind_named(
        &mut self,
        name: &str,
        value: ClassifiedNumber,
        line: u32,
        stmts: &mut Vec<Stmt>,
    ) -> Result<ClassifiedNumber, ParseError> {
        let slot = self.builder.alloc_local_named(name)?;
        stmts.push(Stmt::Let {
            index: slot,
            declared_schema: None,
            expr: value.into_expr(),
            line,
        });
        Ok(ClassifiedNumber {
            expr: Expr::Var(slot),
        })
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
        stmts.push(bind_temp(slot, value.into_expr()));
        Ok(ClassifiedNumber {
            expr: Expr::Var(slot),
        })
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
