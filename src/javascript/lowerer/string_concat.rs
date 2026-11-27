//! Exact proved String additions, using the single managed UTF-16 bridge.
use super::super::prelude::object::{Emit, add, assign, call, declare, int, lt, not, tag_is, var};
use super::*;
use vm::BuiltinFunction as B;

pub(super) const HELPER: &str = "__compat_es5_string_concat";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::javascript) enum Mode {
    StringString,
    StringNumber,
    NumberString,
    StringBoolean,
    BooleanString,
    StringNull,
    NullString,
    StringUndefined,
    UndefinedString,
}
impl Mode {
    fn number(self) -> bool {
        matches!(self, Self::StringNumber | Self::NumberString)
    }
    fn fixed(self) -> bool {
        !matches!(
            self,
            Self::StringString | Self::StringNumber | Self::NumberString
        )
    }
    fn string_left(self) -> bool {
        matches!(
            self,
            Self::StringNumber | Self::StringBoolean | Self::StringNull | Self::StringUndefined
        )
    }
}

fn register_site(
    sites: &mut std::collections::HashMap<(u32, u32), Mode>,
    sealed: bool,
    site: (u32, u32),
    mode: Mode,
) -> Result<(), &'static str> {
    if sealed && sites.get(&site) != Some(&mode) {
        return Err("String concat site was not proved before the runtime index window sealed");
    }
    sites.insert(site, mode);
    Ok(())
}

impl Lowerer<'_> {
    pub(in crate::javascript) fn record_string_concat_site(
        &self,
        binary: &ast::BinExpr,
        mode: Mode,
    ) -> Result<(), ParseError> {
        register_site(
            &mut self.string_concat_sites.borrow_mut(),
            self.string_concat_sealed.get(),
            (binary.span.lo.0, binary.span.hi.0),
            mode,
        )
        .map_err(|message| self.error(binary.span, message))
    }

    pub(super) fn has_string_concat_site(&self, binary: &ast::BinExpr) -> bool {
        self.string_concat_sealed.get()
            && self
                .string_concat_sites
                .borrow()
                .contains_key(&(binary.span.lo.0, binary.span.hi.0))
    }

    pub(super) fn append_string_concat_helper(
        &mut self,
        ir: &mut FrontendIr,
        carrier_schema: &TypeSchema,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        if self.string_concat_sites.borrow().is_empty() && !self.addition_needs_strings() {
            return Ok(());
        }
        let numeric = self
            .string_concat_sites
            .borrow()
            .values()
            .any(|mode| mode.number())
            || self.addition_needs_formatter();
        if numeric {
            self.ensure_number_string_prerequisites(ir, span)?;
        }
        self.ensure_utf16_bridge(ir, span)?;
        let fixed = self
            .string_concat_sites
            .borrow()
            .values()
            .any(|mode| mode.fixed())
            || self.addition_needs_fixed_strings();
        if fixed {
            self.append_fixed_scalar_string_helper(ir, carrier_schema, span, stmts)?;
        }
        let bridge = self.utf16_bridge.as_ref().expect("ensured bridge");
        let (to, from) = (bridge.managed_to_units, bridge.units_to_managed);
        let index = heap_emitters::fresh_index(self, ir, span)?;
        let h = self.heap_ref(span)?.helpers;
        let c = self.slot(span)?;
        let left = self.slot(span)?;
        let right = self.slot(span)?;
        let caller_frame = self.slot(span)?;
        let caller_entry = self.slot(span)?;
        let mut e = Emit::new(self, span, c)?;
        let line = e.line;
        let frame = e.integer()?;
        let out = e.boxed()?;
        let cursor = e.integer()?;
        let unit = e.integer()?;
        let left_units = e.lower.slot(span)?;
        let right_units = e.lower.slot(span)?;
        let right_len = e.lower.slot(span)?;
        let array = TypeSchema::Array(Box::new(TypeSchema::Int));
        let len = |slot| call(B::Len.call_index(), vec![Expr::Borrow(Box::new(var(slot)))]);
        let body = vec![
            assert_stmt(tag_is(var(left), value::STRING), line),
            assert_stmt(tag_is(var(right), value::STRING), line),
            assign(frame, call(h.root_enter, vec![var(c), int(2)]), line),
            e.root(var(frame), int(0), var(left))?,
            e.root(var(frame), int(1), var(right))?,
            Stmt::Let {
                index: left_units,
                declared_schema: Some(array.clone()),
                expr: call(to, vec![var(c), var(left)]),
                line,
            },
            Stmt::Let {
                index: right_units,
                declared_schema: Some(array),
                expr: call(to, vec![var(c), var(right)]),
                line,
            },
            Stmt::Let {
                index: right_len,
                declared_schema: Some(TypeSchema::Int),
                expr: len(right_units),
                line,
            },
            // Keep the bridge's original representation ceiling. Refusal is
            // fatal transport, never a fabricated guest RangeError/Result.
            assert_stmt(
                not(lt(int(65_536), add(len(left_units), var(right_len)))),
                line,
            ),
            Stmt::While {
                condition: lt(var(cursor), var(right_len)),
                body: vec![
                    assign(
                        unit,
                        call(
                            B::Get.call_index(),
                            vec![Expr::Borrow(Box::new(var(right_units))), var(cursor)],
                        ),
                        line,
                    ),
                    // Consume the unique left array, avoiding an Arc alias and
                    // copy-on-write on every scalar append. No guest allocation.
                    assign(
                        left_units,
                        call(
                            B::ArrayPush.call_index(),
                            vec![Expr::MoveVar(left_units), var(unit)],
                        ),
                        line,
                    ),
                    assign(cursor, add(var(cursor), int(1)), line),
                ],
                line,
            },
            // The existing bridge publishes the completed node directly into
            // the active caller's reserved entry, while our private roots live.
            assign(
                out,
                call(
                    from,
                    vec![
                        var(c),
                        Expr::MoveVar(left_units),
                        var(caller_frame),
                        var(caller_entry),
                    ],
                ),
                line,
            ),
            // Nonallocating leave; caller publication stays rooted throughout.
            e.effect(h.root_leave, vec![var(c), var(frame)])?,
        ];
        let body = e.finish(body);
        declare(
            ir,
            stmts,
            HELPER,
            index,
            vec![c, left, right, caller_frame, caller_entry],
            vec![
                carrier_schema.clone(),
                value::schema(),
                value::schema(),
                TypeSchema::Int,
                TypeSchema::Int,
            ],
            value::schema(),
            body,
            var(out),
            line,
        );
        self.helper_string_concat = Some(index);
        Ok(())
    }

    pub(super) fn lower_string_concat(
        &mut self,
        binary: &ast::BinExpr,
    ) -> Result<Lowered, ParseError> {
        let mode = self
            .string_concat_sites
            .borrow()
            .get(&(binary.span.lo.0, binary.span.hi.0))
            .copied()
            .ok_or_else(|| self.error(binary.span, "String concat site was not proved"))?;
        self.record_string_concat_site(binary, mode)?;
        let span = binary.span;
        let line = self.line(span);
        let helper = self
            .helper_string_concat
            .ok_or_else(|| self.error(span, "String concat helper was not selected"))?;
        let c = self.active_carrier(span)?;
        let frame = self.active_frame(span)?;
        // Reserve and initialize the result root before either operand runs.
        let (out, mut stmts) = self.bind_rooted(value::undefined(), value::schema(), span)?;
        let entry = self.entry_for(out, span)?;
        let converted = if mode != Mode::StringString {
            let (slot, mut init) = self.bind_rooted(value::undefined(), value::schema(), span)?;
            stmts.append(&mut init);
            Some(slot)
        } else {
            None
        };
        let left = self.expr(&binary.left)?;
        let right = self.expr(&binary.right)?;
        let mut consumed = left.owned_roots;
        consumed.extend(right.owned_roots);
        let (l, mut ls) = self.bind_evaluation(left.expr, left.kind, binary.left.span())?;
        stmts.append(&mut ls);
        if self.has_root(l) {
            consumed.push(l);
        }
        let (r, mut right_branch) =
            self.bind_evaluation(right.expr, right.kind, binary.right.span())?;
        if self.has_root(r) {
            consumed.push(r);
        }
        let mut publish = Vec::new();
        let (string_left, string_right) = if let Some(converted) = converted {
            let scalar = if mode.string_left() { r } else { l };
            let (managed, input) = if mode.number() {
                let managed = self
                    .number_string_entries
                    .as_ref()
                    .ok_or_else(|| self.error(span, "String concat formatter was not selected"))?
                    .managed;
                (
                    managed,
                    Self::as_number(super::super::prelude::math::checked_number(var(scalar))),
                )
            } else {
                (
                    self.helper_fixed_scalar_string.ok_or_else(|| {
                        self.error(span, "Fixed scalar producer was not selected")
                    })?,
                    var(scalar),
                )
            };
            let converted_entry = self.entry_for(converted, span)?;
            publish.push(assign(
                converted,
                Self::guest_call(
                    managed,
                    vec![var(c), input, var(frame), int(i64::from(converted_entry))],
                ),
                line,
            ));
            self.refresh_root(converted, span, &mut publish)?;
            consumed.push(converted);
            if mode.string_left() {
                (l, converted)
            } else {
                (converted, r)
            }
        } else {
            (l, r)
        };
        let (returned, call_stmt) = self.bind_temp_bare(
            Self::guest_call(
                helper,
                vec![
                    var(c),
                    var(string_left),
                    var(string_right),
                    var(frame),
                    int(i64::from(entry)),
                ],
            ),
            span,
        )?;
        publish.push(call_stmt);
        publish.push(assign(out, var(returned), line));
        // Already caller-rooted by units_to_managed; reaffirm before clearing
        // consumed operands. Nothing allocates in this protected handoff.
        self.refresh_root(out, span, &mut publish)?;
        for slot in Self::compose_owned(consumed) {
            self.clear_root(slot, span, &mut publish)?;
        }
        if let Some(normal) = self.pending_normal {
            // A String fact does not imply normal completion: in particular a
            // sequence prefix can throw before its final String is evaluated.
            right_branch.push(Stmt::IfElse {
                condition: var(normal),
                then_branch: publish,
                else_branch: Vec::new(),
                line,
            });
            stmts.push(Stmt::IfElse {
                condition: var(normal),
                then_branch: right_branch,
                else_branch: Vec::new(),
                line,
            });
        } else {
            stmts.append(&mut right_branch);
            stmts.append(&mut publish);
        }
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(var(out)),
            },
            kind: Kind::String,
            array_length: None,
            owned_roots: vec![out],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Mode, register_site};
    use std::collections::HashMap;
    #[test]
    fn concat117_sealed_exact_sites() {
        let mut sites = HashMap::new();
        register_site(&mut sites, false, (1, 2), Mode::StringString).unwrap();
        register_site(&mut sites, true, (1, 2), Mode::StringString).unwrap();
        let before = sites.clone();
        assert!(register_site(&mut sites, true, (3, 4), Mode::StringString).is_err());
        assert!(register_site(&mut sites, true, (1, 2), Mode::StringNumber).is_err());
        assert!(register_site(&mut sites, true, (1, 2), Mode::NumberString).is_err());
        for mode in [
            Mode::StringBoolean,
            Mode::BooleanString,
            Mode::StringNull,
            Mode::NullString,
            Mode::StringUndefined,
            Mode::UndefinedString,
        ] {
            assert!(register_site(&mut sites, true, (1, 2), mode).is_err());
            let mut fixed = HashMap::new();
            register_site(&mut fixed, false, (5, 9), mode).unwrap();
            register_site(&mut fixed, true, (5, 9), mode).unwrap();
            assert!(register_site(&mut fixed, true, (5, 10), mode).is_err());
            assert!(register_site(&mut fixed, true, (5, 9), Mode::StringString).is_err());
        }
        assert_eq!(sites, before);
        let mut empty = HashMap::new();
        assert!(register_site(&mut empty, true, (1, 2), Mode::StringString).is_err());
        assert!(empty.is_empty());
    }
}
