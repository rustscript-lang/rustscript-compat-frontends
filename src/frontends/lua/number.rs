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
//! or a hard-to-guess legal identifier is not a private namespace. Each
//! lowering keeps a monotonic ordinal and resumes the search there. A later
//! lowering on the same builder starts again at zero, so it still observes
//! names the previous one holds.
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

use vm::compiler::HostCallResolver;
use vm::{BuiltinFunction, Expr, LocalIrBuilder, LocalSlot, ParseError, Stmt, TypeSchema};

pub(crate) const BIT_NOT: &str = "lua_number::bit_not";

#[derive(Clone, Copy)]
pub(crate) enum BitwiseBinary {
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Sar,
}

impl BitwiseBinary {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::And => "lua_number::bit_and",
            Self::Or => "lua_number::bit_or",
            Self::Xor => "lua_number::bit_xor",
            Self::Shl => "lua_number::bit_shl",
            Self::Shr => "lua_number::bit_shr",
            Self::Sar => "lua_number::bit_sar",
        }
    }

    pub(crate) fn evaluate(self, left: f64, right: f64) -> f64 {
        let left = tobit(left);
        let right = tobit(right);
        // Rust's wrapping shifts mask the count to the low five bits for i32/u32.
        let result = match self {
            Self::And => left & right,
            Self::Or => left | right,
            Self::Xor => left ^ right,
            Self::Shl => left.wrapping_shl(right as u32),
            Self::Shr => (left as u32).wrapping_shr(right as u32) as i32,
            Self::Sar => left.wrapping_shr(right as u32),
        };
        f64::from(result)
    }
}

pub(crate) fn bit_not(value: f64) -> f64 {
    f64::from(!tobit(value))
}

/// The pinned LuaJIT x64 `vm_tobit` adds 2^52 + 2^51 in binary64, then
/// reads the low 32 bits of the sum. Large values observe the rounding of
/// the addition itself, so rounding the input and reducing modulo 2^32 differs.
fn tobit(value: f64) -> i32 {
    (value + 6_755_399_441_055_744.0).to_bits() as i32
}

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

/// Temps for operations that must evaluate each operand once and then reuse it.
///
/// Slots stay live for the returned expression. Calling `floor_mod` again on
/// this lowering allocates new slots; it does not reclaim the previous pair.
pub(crate) struct NumberLowering<'a> {
    builder: &'a mut LocalIrBuilder,
    /// Next ordinal this lowering will probe. It only moves forward. It is
    /// not shared with another lowering, even one that borrows this builder
    /// after `self` is dropped.
    next_ordinal: u32,
    /// `resolve_local_expr` calls made while naming temps. One lowering of a
    /// chunk can be counted from the source seam; this is not a timer.
    name_probes: u64,
}

impl<'a> NumberLowering<'a> {
    pub(crate) fn new(builder: &'a mut LocalIrBuilder) -> Self {
        Self {
            builder,
            next_ordinal: 0,
            name_probes: 0,
        }
    }

    /// Name lookups performed by `alloc_temp` on this lowering.
    pub(crate) fn name_probes(&self) -> u64 {
        self.name_probes
    }

    pub(crate) fn bitwise_not(
        &mut self,
        resolver: &HostCallResolver<'_>,
        operand: ClassifiedNumber,
    ) -> Result<ClassifiedNumber, ParseError> {
        self.bit_call(resolver, BIT_NOT, [operand])
    }

    pub(crate) fn bitwise_binary(
        &mut self,
        resolver: &HostCallResolver<'_>,
        op: BitwiseBinary,
        lhs: ClassifiedNumber,
        rhs: ClassifiedNumber,
    ) -> Result<ClassifiedNumber, ParseError> {
        self.bit_call(resolver, op.name(), [lhs, rhs])
    }

    fn bit_call<const N: usize>(
        &mut self,
        resolver: &HostCallResolver<'_>,
        name: &str,
        operands: [ClassifiedNumber; N],
    ) -> Result<ClassifiedNumber, ParseError> {
        let signature = [const { TypeSchema::Float }; N];
        let resolved = resolver
            .resolve(name, &signature)
            .map_err(|error| ParseError::new(error.to_string()))?;
        if resolved.return_type != TypeSchema::Float
            || !resolved
                .params
                .iter()
                .map(|param| &param.schema)
                .eq(&signature)
        {
            return Err(ParseError::new(format!(
                "invalid Lua bitwise signature for {name}"
            )));
        }
        self.builder.declare_function(name, Some(N as u8))?;
        let index = self
            .builder
            .function_index(name)
            .expect("declared bitwise operation");
        Ok(ClassifiedNumber {
            expr: Expr::Call(
                index,
                Vec::new(),
                operands
                    .into_iter()
                    .map(ClassifiedNumber::into_expr)
                    .collect(),
                Some(Box::new(resolved)),
                None,
            ),
        })
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
        // cannot use this name. The cursor skips ordinals this lowering has
        // already probed. It does not make a name free: `resolve_local_expr`
        // still rejects a slot another lowering allocated on this builder.
        let mut ordinal = self.next_ordinal;
        loop {
            let name = format!("$lua.number.{role}.{ordinal}");
            self.name_probes += 1;
            if self.builder.resolve_local_expr(&name).is_none() {
                let slot = self.builder.alloc_local_named(&name)?;
                self.next_ordinal = ordinal
                    .checked_add(1)
                    .ok_or_else(|| ParseError::new("lua number temp ordinal overflow"))?;
                return Ok(slot);
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
