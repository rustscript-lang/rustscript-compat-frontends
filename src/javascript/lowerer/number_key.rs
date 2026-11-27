//! Sealed exact-site Number keys. Static member lowering stays separate.
use super::*;

fn seal_sites(sealed: &std::cell::Cell<bool>) {
    sealed.set(true);
}

fn register_site(
    sites: &mut std::collections::HashSet<(u32, u32)>,
    sealed: bool,
    site: (u32, u32),
) -> Result<(), &'static str> {
    if sealed && !sites.contains(&site) {
        return Err("Number key site was not proved before the runtime index window sealed");
    }
    sites.insert(site);
    Ok(())
}

impl Lowerer<'_> {
    pub(in crate::javascript) fn number_key_candidate(member: &ast::MemberExpr) -> bool {
        matches!(&member.prop, ast::MemberProp::Computed(property)
                if !matches!(Self::unwrap_parens(&property.expr), ast::Expr::Lit(_))
                    && Self::member_index_literal(&property.expr).is_none())
    }

    pub(in crate::javascript) fn record_number_key_site(
        &self,
        member: &ast::MemberExpr,
    ) -> Result<(), ParseError> {
        register_site(
            &mut self.number_key_sites.borrow_mut(),
            self.number_key_sealed.get(),
            (member.span.lo.0, member.span.hi.0),
        )
        .map_err(|message| self.error(member.span, message))
    }

    pub(super) fn has_number_key_site(&self, member: &ast::MemberExpr) -> bool {
        self.number_key_sealed.get()
            && self
                .number_key_sites
                .borrow()
                .contains(&(member.span.lo.0, member.span.hi.0))
    }

    pub(super) fn append_number_key_prerequisites(
        &mut self,
        ir: &mut FrontendIr,
        span: swc_common::Span,
    ) -> Result<(), ParseError> {
        seal_sites(&self.number_key_sealed);
        if self.number_key_sites.borrow().is_empty() {
            return Ok(());
        }
        self.ensure_number_string_prerequisites(ir, span)
    }

    pub(super) fn ensure_number_string_prerequisites(
        &mut self,
        ir: &mut FrontendIr,
        span: swc_common::Span,
    ) -> Result<(), ParseError> {
        if self.number_string_entries.is_some() {
            return Ok(());
        }
        self.ensure_utf16_bridge(ir, span)?;
        ir.locals = ir.locals.max(usize::from(self.next_slot));
        let bridge = self.utf16_bridge.as_ref().expect("ensured bridge");
        self.number_string_entries =
            Some(super::super::prelude::number_string::append(ir, bridge)?);
        self.next_slot = u16::try_from(ir.locals)
            .map_err(|_| self.error(span, "Number key local slot space exhausted"))?;
        Ok(())
    }

    pub(super) fn lower_number_key_read(
        &mut self,
        member: &ast::MemberExpr,
    ) -> Result<Lowered, ParseError> {
        let span = member.span;
        let line = self.line(span);
        let managed = self
            .number_string_entries
            .as_ref()
            .ok_or_else(|| {
                self.error(
                    span,
                    "Number key prerequisites must precede the runtime index window",
                )
            })?
            .managed;
        let ref_get = self
            .helper_ref_get
            .ok_or_else(|| self.error(span, "Number key requires the realm reference helper"))?;
        let ast::MemberProp::Computed(property) = &member.prop else {
            unreachable!("exact approved computed read")
        };
        let carrier = self.active_carrier(span)?;
        let frame = self.active_frame(span)?;
        let base = self.expr(&member.obj)?;
        let mut owned = base.owned_roots;
        let (base_slot, mut stmts) = self.bind_temp(base.expr, member.obj.span())?;
        owned.push(base_slot);
        // Both roots have dominating actual writes even if evaluation is skipped.
        let (key_slot, init) = self.bind_temp(value::undefined(), span)?;
        stmts.extend(init);
        let (result, init) = self.bind_temp(value::undefined(), span)?;
        stmts.extend(init);
        owned.push(key_slot);
        let entry = self.entry_for(key_slot, span)?;
        let key = self.expr(&property.expr)?;
        owned.extend(key.owned_roots);
        let (input, mut access) = self.bind_evaluation(key.expr, key.kind, property.span)?;
        if self.has_root(input) {
            owned.push(input);
        }
        // A key operand may itself complete abruptly. Never project or allocate then.
        let mut get = vec![Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: key_slot,
            expr: Self::guest_call(
                managed,
                vec![
                    Expr::Var(carrier),
                    Self::as_number(super::super::prelude::math::checked_number(Expr::Var(
                        input,
                    ))),
                    Expr::Var(frame),
                    Expr::Int(i64::from(entry)),
                ],
            ),
            line,
        }];
        let reference = value::reference(
            Expr::Var(base_slot),
            value::field(key_slot, "ref_slot"),
            value::field(key_slot, "generation"),
            Expr::Bool(self.enclosing_strict),
        );
        let (reference, saved) = self.bind_typed(reference, value::reference_schema(), span)?;
        get.push(saved);
        self.reaffirm_root(carrier, base_slot, span, line, &mut get)?;
        self.reaffirm_root(carrier, key_slot, span, line, &mut get)?;
        let got = Self::guest_call(ref_get, vec![Expr::Var(carrier), Expr::Var(reference)]);
        let value = self.consume_value_result(got, span, &mut get)?;
        get.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: result,
            expr: Expr::Var(value),
            line,
        });
        self.refresh_root(result, span, &mut get)?;
        if let Some(normal) = self.pending_normal {
            access.push(Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: get,
                else_branch: vec![],
                line,
            });
            stmts.push(Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: access,
                else_branch: vec![],
                line,
            });
        } else {
            access.extend(get);
            stmts.extend(access);
        }
        // A Get Err has already published its rooted pending payload. Operand
        // consumers may skip their own cleanup after that transition.
        reference::release_consumed_roots(
            self,
            &Self::compose_owned(owned),
            result,
            span,
            &mut stmts,
        )?;
        Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(Expr::Var(result)),
            },
            kind: Kind::Dynamic,
            array_length: None,
            owned_roots: vec![result],
        })
    }
}

#[cfg(test)]
mod phase_tests {
    use super::{register_site, seal_sites};
    use std::{cell::Cell, collections::HashSet};

    #[test]
    fn number_key_collecting_inserts() {
        let mut sites = HashSet::new();
        register_site(&mut sites, false, (1, 2)).unwrap();
        assert_eq!(sites, HashSet::from([(1, 2)]));
    }

    #[test]
    fn number_key_sealed_existing_is_unchanged() {
        let mut sites = HashSet::from([(1, 2)]);
        let before = sites.clone();
        let sealed = Cell::new(false);
        seal_sites(&sealed);
        register_site(&mut sites, sealed.get(), (1, 2)).unwrap();
        assert_eq!(sites, before);
    }

    #[test]
    fn number_key_sealed_unseen_refuses_without_mutation() {
        let mut sites = HashSet::from([(1, 2)]);
        let before = sites.clone();
        let sealed = Cell::new(false);
        seal_sites(&sealed);
        assert_eq!(
            register_site(&mut sites, sealed.get(), (3, 4)),
            Err("Number key site was not proved before the runtime index window sealed")
        );
        assert_eq!(sites, before);
    }

    #[test]
    fn number_key_sealed_empty_cannot_mint_demand() {
        let mut sites = HashSet::new();
        let sealed = Cell::new(false);
        seal_sites(&sealed);
        assert!(register_site(&mut sites, sealed.get(), (1, 2)).is_err());
        assert!(sealed.get());
        assert!(sites.is_empty());
    }
}
