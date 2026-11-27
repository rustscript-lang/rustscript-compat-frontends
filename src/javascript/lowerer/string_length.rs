//! Sealed literal-length reads; non-String References retain ordinary Get.
use super::super::prelude::object::{
    Emit, add, assign, branch, call, declare, eq, field, int, lt, not, num, result, sub, tag_is,
    var,
};
use super::*;

pub(super) const HELPER: &str = "__compat_es5_string_length_get";

fn register_site(
    sites: &mut std::collections::HashSet<(u32, u32)>,
    sealed: bool,
    site: (u32, u32),
) -> Result<(), &'static str> {
    if sealed && !sites.contains(&site) {
        return Err("String length site was not proved before the runtime index window sealed");
    }
    sites.insert(site);
    Ok(())
}

impl Lowerer<'_> {
    pub(in crate::javascript) fn record_string_length_site(
        &self,
        member: &ast::MemberExpr,
    ) -> Result<(), ParseError> {
        register_site(
            &mut self.string_length_sites.borrow_mut(),
            self.string_length_sealed.get(),
            (member.span.lo.0, member.span.hi.0),
        )
        .map_err(|message| self.error(member.span, message))
    }

    pub(super) fn has_string_length_site(&self, member: &ast::MemberExpr) -> bool {
        self.string_length_sealed.get()
            && self
                .string_length_sites
                .borrow()
                .contains(&(member.span.lo.0, member.span.hi.0))
    }

    pub(super) fn append_string_length_adapter(
        &mut self,
        ir: &mut FrontendIr,
        carrier_schema: &TypeSchema,
        span: swc_common::Span,
        stmts: &mut Vec<Stmt>,
    ) -> Result<(), ParseError> {
        if self.string_length_sites.borrow().is_empty() {
            return Ok(());
        }
        let get = self
            .helper_ref_get
            .ok_or_else(|| self.error(span, "String length requires ordinary Get"))?;
        let index = heap_emitters::fresh_index(self, ir, span)?;
        let carrier = self.slot(span)?;
        let reference = self.slot(span)?;
        let schema = value::value_result_schema();
        let mut e = Emit::new(self, span, carrier)?;
        let line = e.line;
        let source_span = e.lower.ir_span(span);
        let out = e.temp(
            schema.clone(),
            result(schema.clone(), true, value::undefined(), source_span),
        )?;
        let receiver = e.boxed()?;
        let key = e.boxed()?;
        let base = e.integer()?;
        let extent = e.integer()?;
        let length = e.integer()?;
        let key_base = e.integer()?;
        let key_extent = e.integer()?;
        // All address arithmetic follows checked_base's live slot/generation and
        // occupied-extent check. Unsplittable tails may exceed the minimal size.
        let header = layout::RAW_HEADER_CELLS + layout::STRING_HEADER_CELLS;
        let mut string = vec![
            assign(base, e.base(var(receiver)), line),
            assert_stmt(eq(e.decoded(var(base)), int(layout::KIND_STRING)), line),
            assign(
                extent,
                e.decoded(sub(var(base), int(layout::RAW_HEADER_CELLS))),
                line,
            ),
            assert_stmt(not(lt(var(extent), int(header))), line),
            assign(length, e.decoded(add(var(base), int(2))), line),
            assert_stmt(not(lt(var(length), int(0))), line),
            assert_stmt(not(lt(sub(var(extent), int(header)), var(length))), line),
            assign(
                key,
                value::dynamic_value(
                    int(value::STRING),
                    Expr::Float(0.0),
                    Expr::Bool(false),
                    field(var(reference), "key_slot"),
                    field(var(reference), "key_generation"),
                ),
                line,
            ),
            assign(key_base, e.base(var(key)), line),
            assert_stmt(eq(e.decoded(var(key_base)), int(layout::KIND_STRING)), line),
            assign(
                key_extent,
                e.decoded(sub(var(key_base), int(layout::RAW_HEADER_CELLS))),
                line,
            ),
            assert_stmt(not(lt(var(key_extent), int(header + 6))), line),
            assert_stmt(eq(e.decoded(add(var(key_base), int(2))), int(6)), line),
        ];
        for (offset, unit) in "length".encode_utf16().enumerate() {
            string.push(assert_stmt(
                eq(
                    e.decoded(add(
                        var(key_base),
                        int(layout::STRING_HEADER_CELLS + offset as i64),
                    )),
                    int(i64::from(unit)),
                ),
                line,
            ));
        }
        string.push(assign(
            out,
            result(
                schema.clone(),
                true,
                value::number(num(var(length))),
                source_span,
            ),
            line,
        ));
        // No guest allocation/callback in the String branch. The fallback gets
        // the original rooted Reference, and returns its actual typed Result.
        let body = vec![
            assign(receiver, field(var(reference), "base"), line),
            branch(
                tag_is(var(receiver), value::STRING),
                string,
                vec![assign(
                    out,
                    call(get, vec![var(carrier), var(reference)]),
                    line,
                )],
                line,
            ),
        ];
        let body = e.finish(body);
        declare(
            ir,
            stmts,
            HELPER,
            index,
            vec![carrier, reference],
            vec![carrier_schema.clone(), value::reference_schema()],
            schema,
            body,
            var(out),
            line,
        );
        self.helper_string_length = Some(index);
        Ok(())
    }
}

#[cfg(test)]
mod phase_tests {
    use super::register_site;
    use std::collections::HashSet;

    #[test]
    fn string_length115_sealed_exact_sites() {
        let mut sites = HashSet::new();
        register_site(&mut sites, false, (1, 2)).unwrap();
        register_site(&mut sites, true, (1, 2)).unwrap();
        let before = sites.clone();
        assert!(register_site(&mut sites, true, (3, 4)).is_err());
        assert_eq!(sites, before);
        let mut empty = HashSet::new();
        assert!(register_site(&mut empty, true, (1, 2)).is_err());
        assert!(empty.is_empty());
    }
}
