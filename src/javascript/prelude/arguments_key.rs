//! Private `arguments[<runtime String>]` key classifier.
//!
//! Pinned Micro materializes the activation's implicit binding as a real
//! Array, so a String subscript is an ordinary property get: a canonical
//! decimal String names one element, `"length"` names the length row, and
//! every other String is an absent own property. The invocation vector is a
//! Core `Array<JsValue>` with no ordinary object identity, so this helper
//! reads the guest String through the existing read/decode/checked-base
//! helper set and returns one Int code:
//!
//! * `>= 0` — canonical array index (no sign, no leading zero, <= 16 units)
//! * `-1`   — the `length` row
//! * `-2`   — absent (everything else, including non-ASCII and empty keys)
//!
//! It performs no allocation and no guest call of its own.

use super::super::{heap as layout, lowerer::Lowerer, representation as value};
use super::heap::{decode_int_call, fresh_index, read_cell};
use super::object::{declare, var};
use super::values::read_string_unit;
use vm::{Expr as E, FrontendIr, ParseError, Stmt as S, TypeSchema as T};

/// Longest admitted canonical index text. The invocation vector is bounded by
/// the same 16-unit window the static classifier uses.
const MAX_INDEX_UNITS: i64 = 16;

pub(crate) fn append(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
) -> Result<u16, ParseError> {
    let index = fresh_index(lower, ir, span)?;
    let heap = lower.heap_ref(span)?;
    let h = heap.helpers;
    let helper_read = heap.helper_read;
    let helper_decode = lower
        .helper_decode
        .ok_or_else(|| lower.error(span, "arguments String keys require the realm decode helper"))?;
    let c = lower.slot(span)?;
    let key = lower.slot(span)?;
    let slot = lower.slot(span)?;
    let generation = lower.slot(span)?;
    let base = lower.slot(span)?;
    let kind_raw = lower.slot(span)?;
    let kind = lower.slot(span)?;
    let length = lower.slot(span)?;
    let cursor = lower.slot(span)?;
    let unit = lower.slot(span)?;
    let answer = lower.slot(span)?;
    let digits = lower.slot(span)?;
    let line = lower.line(span);
    // `read_string_unit` addresses the unit through a slot, so every position
    // (constant or loop cursor) is bound to `cursor` first.
    let unit_at_cursor = || read_string_unit(helper_read, c, base, cursor);
    let number_cell = |offset: i64| -> E {
        Lowerer::as_number(read_cell(
            helper_read,
            c,
            E::Add(Box::new(E::Var(base)), Box::new(E::Int(offset))),
        ))
    };
    let mut body = vec![
        S::Let {
            index: slot,
            declared_schema: Some(T::Int),
            expr: value::field(key, "ref_slot"),
            line,
        },
        S::Let {
            index: generation,
            declared_schema: Some(T::Int),
            expr: value::field(key, "generation"),
            line,
        },
        S::Let {
            index: base,
            declared_schema: Some(T::Int),
            expr: Lowerer::guest_call(
                h.checked_base,
                vec![E::Var(c), E::Var(slot), E::Var(generation)],
            ),
            line,
        },
        S::Let {
            index: kind_raw,
            declared_schema: Some(T::Number),
            expr: number_cell(0),
            line,
        },
        S::Let {
            index: kind,
            declared_schema: Some(T::Int),
            expr: decode_int_call(helper_decode, E::Var(kind_raw)),
            line,
        },
        // The caller proves the tag, so a stale or wrong-kind payload fails
        // closed here instead of reading another kind's cells as units.
        super::super::lowerer::assert_stmt(
            E::Eq(Box::new(E::Var(kind)), Box::new(E::Int(layout::KIND_STRING))),
            line,
        ),
        S::Let {
            index: length,
            declared_schema: Some(T::Int),
            expr: Lowerer::guest_call(h.decode_u32, vec![number_cell(2)]),
            line,
        },
        // Default: absent own property.
        S::Let {
            index: answer,
            declared_schema: Some(T::Int),
            expr: E::Int(-2),
            line,
        },
    ];
    // `"length"`: six ASCII units, compared in place with no allocation.
    let length_word: Vec<i64> = "length".encode_utf16().map(i64::from).collect();
    let mut is_length = vec![S::Let {
        index: digits,
        declared_schema: Some(T::Bool),
        expr: E::Eq(Box::new(E::Var(length)), Box::new(E::Int(6))),
        line,
    }];
    for (position, expected) in length_word.iter().enumerate() {
        is_length.push(S::Let {
            index: cursor,
            declared_schema: Some(T::Int),
            expr: E::Int(position as i64),
            line,
        });
        is_length.push(S::Assign {
            kind: vm::AssignmentKind::Set,
            index: digits,
            expr: E::And(
                Box::new(E::Var(digits)),
                Box::new(E::Eq(
                    Box::new(decode_int_call(
                        helper_decode,
                        unit_at_cursor(),
                    )),
                    Box::new(E::Int(*expected)),
                )),
            ),
            line,
        });
    }
    is_length.push(S::IfElse {
        condition: E::Var(digits),
        then_branch: vec![S::Assign {
            kind: vm::AssignmentKind::Set,
            index: answer,
            expr: E::Int(-1),
            line,
        }],
        else_branch: Vec::new(),
        line,
    });
    // Canonical decimal index: only when the key is not the length row.
    let mut canonical = vec![
        S::Let {
            index: digits,
            declared_schema: Some(T::Bool),
            expr: E::And(
                Box::new(E::Gt(Box::new(E::Var(length)), Box::new(E::Int(0)))),
                Box::new(E::Lt(
                    Box::new(E::Var(length)),
                    Box::new(E::Int(MAX_INDEX_UNITS + 1)),
                )),
            ),
            line,
        },
        S::Let {
            index: cursor,
            declared_schema: Some(T::Int),
            expr: E::Int(0),
            line,
        },
    ];
    // A multi-unit key may not start with `0`: `"01"` is not an index.
    canonical.push(S::IfElse {
        condition: E::And(
            Box::new(E::Var(digits)),
            Box::new(E::Gt(Box::new(E::Var(length)), Box::new(E::Int(1)))),
        ),
        then_branch: vec![
            S::Let {
                index: unit,
                declared_schema: Some(T::Int),
                expr: decode_int_call(helper_decode, unit_at_cursor()),
                line,
            },
            S::Assign {
                kind: vm::AssignmentKind::Set,
                index: digits,
                expr: E::Not(Box::new(E::Eq(
                    Box::new(E::Var(unit)),
                    Box::new(E::Int(i64::from(b'0'))),
                ))),
                line,
            },
        ],
        else_branch: Vec::new(),
        line,
    });
    canonical.push(S::While {
        condition: E::And(
            Box::new(E::Var(digits)),
            Box::new(E::Lt(Box::new(E::Var(cursor)), Box::new(E::Var(length)))),
        ),
        body: vec![
            S::Let {
                index: unit,
                declared_schema: Some(T::Int),
                expr: decode_int_call(helper_decode, unit_at_cursor()),
                line,
            },
            // Every unit must be an ASCII digit; overflow of the 16-unit
            // window is impossible because the length gate above bounds it.
            S::IfElse {
                condition: E::And(
                    Box::new(E::Gt(
                        Box::new(E::Var(unit)),
                        Box::new(E::Int(i64::from(b'0') - 1)),
                    )),
                    Box::new(E::Lt(
                        Box::new(E::Var(unit)),
                        Box::new(E::Int(i64::from(b'9') + 1)),
                    )),
                ),
                then_branch: vec![
                    S::IfElse {
                        condition: E::Eq(Box::new(E::Var(cursor)), Box::new(E::Int(0))),
                        then_branch: vec![S::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: answer,
                            expr: E::Sub(
                                Box::new(E::Var(unit)),
                                Box::new(E::Int(i64::from(b'0'))),
                            ),
                            line,
                        }],
                        else_branch: vec![S::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: answer,
                            expr: E::Add(
                                Box::new(E::Mul(
                                    Box::new(E::Var(answer)),
                                    Box::new(E::Int(10)),
                                )),
                                Box::new(E::Sub(
                                    Box::new(E::Var(unit)),
                                    Box::new(E::Int(i64::from(b'0'))),
                                )),
                            ),
                            line,
                        }],
                        line,
                    },
                    S::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: cursor,
                        expr: E::Add(Box::new(E::Var(cursor)), Box::new(E::Int(1))),
                        line,
                    },
                ],
                else_branch: vec![
                    // A non-digit unit invalidates the whole key. The partial
                    // prefix accumulated so far must not survive: pinned
                    // treats `"1 "`, `"1e1"` and `"0x1"` as absent own
                    // properties, never as index 1 or index 0.
                    S::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: digits,
                        expr: E::Bool(false),
                        line,
                    },
                    S::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: answer,
                        expr: E::Int(-2),
                        line,
                    },
                ],
                line,
            },
        ],
        line,
    });
    // The digit scan only refines a key that is not `"length"`; on success it
    // overwrites the absent default with the accumulated index.
    body.push(S::IfElse {
        condition: E::Eq(Box::new(E::Var(answer)), Box::new(E::Int(-2))),
        then_branch: canonical,
        else_branch: Vec::new(),
        line,
    });
    body.extend(is_length);
    declare(
        ir,
        stmts,
        "__compat_es5_arguments_string_key",
        index,
        vec![c, key],
        vec![carrier_schema.clone(), value::schema()],
        T::Int,
        body,
        var(answer),
        line,
    );
    Ok(index)
}
