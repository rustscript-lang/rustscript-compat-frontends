//! Fixed primitive lexemes; the existing bridge publishes into the caller root.
use super::super::prelude::object::{assign, call, declare, int, or, tag_is, var};
use super::*;
use vm::BuiltinFunction as B;

const HELPER: &str = "__compat_es5_fixed_scalar_string_managed";

fn append_ascii(units: u16, token: &[u8], line: u32) -> Vec<Stmt> {
    token
        .iter()
        .map(|byte| {
            assign(
                units,
                call(
                    B::ArrayPush.call_index(),
                    vec![Expr::MoveVar(units), int(i64::from(*byte))],
                ),
                line,
            )
        })
        .collect()
}

impl Lowerer<'_> {
    pub(super) fn append_fixed_scalar_string_helper(
        &mut self,
        ir: &mut FrontendIr,
        carrier_schema: &TypeSchema,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        if self.helper_fixed_scalar_string.is_some() {
            return Ok(());
        }
        // A reused bridge may return without synchronizing intervening emitter
        // reservations. Never downshift the live local-slot high-water mark.
        ir.locals = ir.locals.max(usize::from(self.next_slot));
        self.next_slot = u16::try_from(ir.locals)
            .map_err(|_| self.error(span, "Fixed scalar local slot space exhausted"))?;
        self.ensure_utf16_bridge(ir, span)?;
        let from = self
            .utf16_bridge
            .as_ref()
            .expect("ensured bridge")
            .units_to_managed;
        let index = heap_emitters::fresh_index(self, ir, span)?;
        let c = self.slot(span)?;
        let scalar = self.slot(span)?;
        let frame = self.slot(span)?;
        let entry = self.slot(span)?;
        let units = self.slot(span)?;
        let line = self.line(span);
        let body = vec![
            // Out-of-profile representations are fatal transport refusals,
            // not guest exceptions and not generic ToPrimitive dispatch.
            assert_stmt(
                or(
                    tag_is(var(scalar), value::BOOLEAN),
                    or(
                        tag_is(var(scalar), value::NULL),
                        tag_is(var(scalar), value::UNDEFINED),
                    ),
                ),
                line,
            ),
            Stmt::Let {
                index: units,
                declared_schema: Some(TypeSchema::Array(Box::new(TypeSchema::Int))),
                expr: call(B::ArrayNew.call_index(), vec![]),
                line,
            },
            Stmt::IfElse {
                condition: tag_is(var(scalar), value::BOOLEAN),
                then_branch: vec![Stmt::IfElse {
                    condition: value::field(scalar, "flag"),
                    then_branch: append_ascii(units, b"true", line),
                    else_branch: append_ascii(units, b"false", line),
                    line,
                }],
                else_branch: vec![Stmt::IfElse {
                    condition: tag_is(var(scalar), value::NULL),
                    then_branch: append_ascii(units, b"null", line),
                    else_branch: append_ascii(units, b"undefined", line),
                    line,
                }],
                line,
            },
        ];
        // Scalar input has no managed edge. No private frame/pool root is
        // needed; units_to_managed roots the result before returning it.
        declare(
            ir,
            stmts,
            HELPER,
            index,
            vec![c, scalar, frame, entry],
            vec![
                carrier_schema.clone(),
                value::schema(),
                TypeSchema::Int,
                TypeSchema::Int,
            ],
            value::schema(),
            body,
            call(
                from,
                vec![var(c), Expr::MoveVar(units), var(frame), var(entry)],
            ),
            line,
        );
        self.helper_fixed_scalar_string = Some(index);
        ir.locals = ir.locals.max(usize::from(self.next_slot));
        Ok(())
    }
}
