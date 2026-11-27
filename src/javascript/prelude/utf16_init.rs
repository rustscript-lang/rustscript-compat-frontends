//! Compile-time outlining of repeated UTF-16 initializer writes.
//!
//! Each eligible exact `Vec<u16>` sequence gets one private guest helper
//! for this compilation. The helper only stores length and units through
//! the existing `__compat_ref_write` numeric cell write. It does not begin,
//! publish, root, collect, or reenter the host. `__compat_ref_write` is the
//! B1 numeric store; collection is a different dispatcher operation and is
//! not reachable from that write, so a pending newborn cannot be reclaimed
//! by the initializer itself.
//!
//! Sequences shorter than four units, a full 64-helper pool, or a flat
//! index that cannot be reserved keep the original inline writes. This is
//! an emission policy, not a source admission limit.

use super::super::heap as layout;
use super::super::lowerer::Lowerer;
use super::heap::declare_helper;
use std::collections::HashMap;
use vm::{Expr, FrontendIr, ParseError, Stmt, TypeSchema, ValueType};

pub(super) const MIN_UNITS: usize = 4;
pub(super) const POOL_LIMIT: u16 = 64;
const NAME_PREFIX: &str = "__compat_es5_utf16_init_";

#[derive(Clone)]
struct Outlined {
    index: u16,
    units: Vec<u16>,
}

#[derive(Default)]
pub(crate) struct Utf16InitPool {
    armed: bool,
    next_index: Option<u16>,
    remaining: u16,
    by_units: HashMap<Vec<u16>, u16>,
    helpers: Vec<Outlined>,
    helper_write: Option<u16>,
    carrier_schema: Option<TypeSchema>,
}

impl Utf16InitPool {
    pub(crate) fn is_armed(&self) -> bool {
        self.armed
    }
}

/// How many collision-free helper indices fit in `u16`, capped at the trial
/// pool. `None` means the first index itself overflowed.
pub(super) fn reservable_utf16_indices(first: Option<u16>) -> u16 {
    let Some(mut cursor) = first else {
        return 0;
    };
    let mut count = 0u16;
    loop {
        count += 1;
        if count == POOL_LIMIT {
            break;
        }
        match cursor.checked_add(1) {
            Some(next) => cursor = next,
            None => break,
        }
    }
    count
}

pub(crate) fn arm(
    pool: &mut Utf16InitPool,
    first: Option<u16>,
    helper_write: u16,
    carrier_schema: TypeSchema,
) {
    debug_assert!(!pool.armed, "utf16 initializer pool armed twice");
    let remaining = reservable_utf16_indices(first);
    pool.armed = true;
    pool.remaining = remaining;
    pool.next_index = if remaining == 0 { None } else { first };
    pool.helper_write = Some(helper_write);
    pool.carrier_schema = Some(carrier_schema);
}

/// Return the shared helper index, or `None` to keep the original inline writes.
pub(crate) fn intern(pool: &mut Utf16InitPool, units: &[u16]) -> Option<u16> {
    if !pool.armed || units.len() < MIN_UNITS || pool.helper_write.is_none() {
        return None;
    }
    if let Some(index) = pool.by_units.get(units) {
        return Some(*index);
    }
    if pool.remaining == 0 {
        return None;
    }
    let index = pool.next_index?;
    pool.remaining -= 1;
    pool.next_index = if pool.remaining == 0 {
        None
    } else {
        // `reservable_utf16_indices` already proved this successor exists.
        Some(index.checked_add(1).expect("utf16 index reservation"))
    };
    pool.by_units.insert(units.to_vec(), index);
    pool.helpers.push(Outlined {
        index,
        units: units.to_vec(),
    });
    Some(index)
}

pub(crate) fn append_declarations(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    statements: &mut Vec<Stmt>,
    insert_at: usize,
    span: swc_common::Span,
) -> Result<(), ParseError> {
    if lower.utf16_inits.helpers.is_empty() {
        return Ok(());
    }
    let carrier_schema = lower
        .utf16_inits
        .carrier_schema
        .clone()
        .ok_or_else(|| lower.error(span, "utf16 initializer missing carrier schema"))?;
    let line = lower.line(span);
    let outlined = std::mem::take(&mut lower.utf16_inits.helpers);
    let mut decls = Vec::new();
    for (ordinal, helper) in outlined.iter().enumerate() {
        let param_carrier = lower.slot(span)?;
        let param_base = lower.slot(span)?;
        let mut body = Vec::new();
        emit_initializer_writes(
            lower,
            param_carrier,
            param_base,
            &helper.units,
            span,
            line,
            &mut body,
        )?;
        declare_helper(
            ir,
            &mut decls,
            &format!("{NAME_PREFIX}{ordinal}"),
            helper.index,
            2,
            vec!["carrier".to_owned(), "base".to_owned()],
            vec![Some(carrier_schema.clone()), Some(TypeSchema::Int)],
            Some(TypeSchema::Int),
            ValueType::Int,
            vec![param_carrier, param_base],
            body,
            Expr::Var(param_base),
            line,
        );
    }
    let at = insert_at.min(statements.len());
    for (offset, decl) in decls.into_iter().enumerate() {
        statements.insert(at + offset, decl);
    }
    Ok(())
}

/// Length at `base+2` and each unit at `STRING_HEADER_CELLS+i`, including
/// lone surrogates. Same numeric writes as the historical inline initializer.
pub(crate) fn emit_initializer_writes(
    lower: &mut Lowerer<'_>,
    carrier: u16,
    base: u16,
    units: &[u16],
    span: swc_common::Span,
    line: u32,
    statements: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let length = units.len() as i64;
    lower.emit_cell_write(
        carrier,
        Expr::Add(Box::new(Expr::Var(base)), Box::new(Expr::Int(2))),
        Lowerer::as_number(Expr::Int(length)),
        span,
        line,
        statements,
    )?;
    for (position, unit) in units.iter().enumerate() {
        let offset = layout::STRING_HEADER_CELLS + position as i64;
        lower.emit_cell_write(
            carrier,
            Expr::Add(Box::new(Expr::Var(base)), Box::new(Expr::Int(offset))),
            Lowerer::as_number(Expr::Int(i64::from(*unit))),
            span,
            line,
            statements,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::reservable_utf16_indices;

    #[test]
    fn utf16_outline_reservation_stops_at_flat_index_end() {
        assert_eq!(reservable_utf16_indices(None), 0);
        assert_eq!(reservable_utf16_indices(Some(0)), 64);
        assert_eq!(reservable_utf16_indices(Some(u16::MAX)), 1);
        assert_eq!(reservable_utf16_indices(Some(u16::MAX - 2)), 3);
        assert_eq!(reservable_utf16_indices(Some(u16::MAX - 63)), 64);
        assert_eq!(reservable_utf16_indices(Some(u16::MAX - 62)), 63);
    }
}
