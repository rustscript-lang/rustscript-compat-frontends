//! Checked node access emitters: string equality, GetValue, dispatch.
//!
//! Every consumer resolves its logical slot/generation pair through the
//! checked base helper and asserts the expected node kind before touching
//! payload cells. Stale, freed and wrong-kind references fail closed; no
//! raw base or hardcoded generation survives in produced handles, stored
//! keys, returned fields or dispatch validation.

use super::super::heap as layout;
use super::super::lowerer::{Lowerer, assert_stmt};
use super::super::representation as value;
use super::heap::{decode_int_call, fresh_index, read_cell};
use vm::compiler::ir::{FunctionDecl, FunctionImpl};
use vm::{Expr, FrontendIr, ParseError, Stmt, TypeSchema, ValueType};

/// Read one stored UTF-16 unit through an already-checked base: physical
/// cell `base + 3 + index`, normalized to Number like every other read.
fn read_string_unit(helper_read: u16, carrier: u16, base: u16, index: u16) -> Expr {
    let physical = Expr::Add(
        Box::new(Expr::Add(
            Box::new(Expr::Var(base)),
            Box::new(Expr::Int(layout::STRING_HEADER_CELLS)),
        )),
        Box::new(Expr::Var(index)),
    );
    Lowerer::as_number(Lowerer::guest_call(
        helper_read,
        vec![Expr::Var(carrier), Lowerer::as_number(physical)],
    ))
}

/// Append the string content comparator with the fixed typed
/// `(Carrier, JsValue, JsValue) -> Bool` signature. Both boxes resolve
/// through the checked base with kind validation before any unit is read;
/// stale or wrong-kind boxes fail closed there.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_string_equality(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &TypeSchema,
    helper_read: u16,
    decode: u16,
    checked_base: u16,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let index = fresh_index(lower, ir, span)?;
    let line = lower.line(span);
    let param_carrier = lower.slot(span)?;
    let param_left = lower.slot(span)?;
    let param_right = lower.slot(span)?;
    let lslot = lower.slot(span)?;
    let lgen = lower.slot(span)?;
    let rslot = lower.slot(span)?;
    let rgen = lower.slot(span)?;
    let base_left = lower.slot(span)?;
    let base_right = lower.slot(span)?;
    let kind_num = lower.slot(span)?;
    let kind = lower.slot(span)?;
    let length_left = lower.slot(span)?;
    let length_right = lower.slot(span)?;
    let cursor = lower.slot(span)?;
    let result = lower.slot(span)?;
    let js_schema = value::schema();
    let mut body = vec![
        Stmt::Let {
            index: lslot,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field(param_left, "ref_slot"),
            line,
        },
        Stmt::Let {
            index: lgen,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field(param_left, "generation"),
            line,
        },
        Stmt::Let {
            index: rslot,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field(param_right, "ref_slot"),
            line,
        },
        Stmt::Let {
            index: rgen,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field(param_right, "generation"),
            line,
        },
        Stmt::Let {
            index: base_left,
            declared_schema: Some(TypeSchema::Int),
            expr: Lowerer::guest_call(
                checked_base,
                vec![Expr::Var(param_carrier), Expr::Var(lslot), Expr::Var(lgen)],
            ),
            line,
        },
        Stmt::Let {
            index: base_right,
            declared_schema: Some(TypeSchema::Int),
            expr: Lowerer::guest_call(
                checked_base,
                vec![Expr::Var(param_carrier), Expr::Var(rslot), Expr::Var(rgen)],
            ),
            line,
        },
    ];
    // Both nodes must be primitive strings; any other kind fails closed.
    for base in [base_left, base_right] {
        body.push(Stmt::Let {
            index: kind_num,
            declared_schema: Some(TypeSchema::Number),
            expr: read_cell(helper_read, param_carrier, Expr::Var(base)),
            line,
        });
        body.push(Stmt::Let {
            index: kind,
            declared_schema: Some(TypeSchema::Int),
            expr: decode_int_call(decode, Expr::Var(kind_num)),
            line,
        });
        body.push(assert_stmt(
            Expr::Eq(
                Box::new(Expr::Var(kind)),
                Box::new(Expr::Int(layout::KIND_STRING)),
            ),
            line,
        ));
    }
    let length_operand = |slot: u16| -> Expr {
        Lowerer::as_number(Lowerer::guest_call(
            helper_read,
            vec![
                Expr::Var(param_carrier),
                Lowerer::as_number(Expr::Add(Box::new(Expr::Var(slot)), Box::new(Expr::Int(2)))),
            ],
        ))
    };
    body.push(Stmt::Let {
        index: length_left,
        declared_schema: Some(TypeSchema::Number),
        expr: length_operand(base_left),
        line,
    });
    body.push(Stmt::Let {
        index: length_right,
        declared_schema: Some(TypeSchema::Number),
        expr: length_operand(base_right),
        line,
    });
    body.push(Stmt::Let {
        index: cursor,
        declared_schema: Some(TypeSchema::Int),
        expr: Expr::Int(0),
        line,
    });
    body.push(Stmt::Let {
        index: result,
        declared_schema: Some(TypeSchema::Bool),
        expr: Expr::Eq(
            Box::new(Expr::Var(length_left)),
            Box::new(Expr::Var(length_right)),
        ),
        line,
    });
    body.push(Stmt::While {
        condition: Expr::And(
            Box::new(Expr::Var(result)),
            Box::new(Expr::Lt(
                Box::new(Lowerer::as_number(Expr::Var(cursor))),
                Box::new(Expr::Var(length_left)),
            )),
        ),
        body: vec![
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: result,
                expr: Expr::And(
                    Box::new(Expr::Var(result)),
                    Box::new(Expr::Eq(
                        Box::new(read_string_unit(
                            helper_read,
                            param_carrier,
                            base_left,
                            cursor,
                        )),
                        Box::new(read_string_unit(
                            helper_read,
                            param_carrier,
                            base_right,
                            cursor,
                        )),
                    )),
                ),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: cursor,
                expr: Expr::Add(Box::new(Expr::Var(cursor)), Box::new(Expr::Int(1))),
                line,
            },
        ],
        line,
    });
    let args = vec!["carrier".to_owned(), "left".to_owned(), "right".to_owned()];
    ir.functions.push(FunctionDecl {
        name: layout::STRING_EQUALITY_HELPER.to_owned(),
        arity: 3,
        index,
        args: args.clone(),
        arg_schemas: vec![
            Some(carrier_schema.clone()),
            Some(js_schema.clone()),
            Some(js_schema),
        ],
        return_schema: Some(TypeSchema::Bool),
        type_params: Vec::new(),
        exported: false,
        return_type: ValueType::Bool,
        symbol: None,
    });
    ir.function_impls.insert(
        index,
        FunctionImpl {
            param_slots: vec![param_carrier, param_left, param_right],
            capture_copies: Vec::new(),
            body_stmts: body,
            body_expr: Expr::Var(result),
            body_expr_line: line,
        },
    );
    statements.push(Stmt::FuncDecl {
        name: layout::STRING_EQUALITY_HELPER.to_owned(),
        index,
        arity: 3,
        args,
        exported: false,
        has_impl: true,
        line,
    });
    Ok(index)
}

/// Append the operational GetValue helper `(Carrier, Reference) -> JsValue`.
/// Validates the receiver pair and the Reference key pair through the
/// checked base (both must resolve to live nodes of object and string kind
/// respectively), walks the seven-cell rows comparing real UTF-16 content,
/// validates each stored key pair and each stored string/object payload
/// before reconstructing the complete tagged value. Missing keys and
/// non-object bases are capability guards, never semantic Undefined.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_ref_get(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &TypeSchema,
    helper_read: u16,
    decode: u16,
    decode_u32: u16,
    checked_base: u16,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let index = fresh_index(lower, ir, span)?;
    let line = lower.line(span);
    let param_carrier = lower.slot(span)?;
    let param_ref = lower.slot(span)?;
    let obj_val = lower.slot(span)?;
    let key_slot = lower.slot(span)?;
    let key_gen = lower.slot(span)?;
    let obj_slot = lower.slot(span)?;
    let obj_gen = lower.slot(span)?;
    let obj_base = lower.slot(span)?;
    let obj_tag = lower.slot(span)?;
    let obj_kind = lower.slot(span)?;
    let kind_num = lower.slot(span)?;
    let count_num = lower.slot(span)?;
    let key_base = lower.slot(span)?;
    let search_len = lower.slot(span)?;
    let found = lower.slot(span)?;
    let prop_idx = lower.slot(span)?;
    let res_tag = lower.slot(span)?;
    let res_number = lower.slot(span)?;
    let res_flag = lower.slot(span)?;
    let res_ref = lower.slot(span)?;
    let res_gen = lower.slot(span)?;
    let cur_key_slot = lower.slot(span)?;
    let cur_key_gen = lower.slot(span)?;
    let cur_key_base = lower.slot(span)?;
    let cur_key_kind = lower.slot(span)?;
    let cur_key_len = lower.slot(span)?;
    let cur_tag_num = lower.slot(span)?;
    let cur_number = lower.slot(span)?;
    let cur_flag_num = lower.slot(span)?;
    let cur_ref_num = lower.slot(span)?;
    let cur_gen_num = lower.slot(span)?;
    let cur_tag = lower.slot(span)?;
    let cur_ref = lower.slot(span)?;
    let cur_gen = lower.slot(span)?;
    let unit_cursor = lower.slot(span)?;
    let unit_match = lower.slot(span)?;
    let key_match = lower.slot(span)?;
    let search_unit = lower.slot(span)?;
    let stored_unit = lower.slot(span)?;
    let validated = lower.slot(span)?;
    let js_schema = value::schema();
    let read_base = |base: u16, offset: i64| -> Expr {
        Lowerer::guest_call(
            helper_read,
            vec![
                Expr::Var(param_carrier),
                Lowerer::as_number(Expr::Add(
                    Box::new(Expr::Var(base)),
                    Box::new(Expr::Int(offset)),
                )),
            ],
        )
    };
    let checked = |slot: u16, slot_gen: u16| -> Expr {
        Lowerer::guest_call(
            checked_base,
            vec![
                Expr::Var(param_carrier),
                Expr::Var(slot),
                Expr::Var(slot_gen),
            ],
        )
    };
    let mut body = vec![
        Stmt::Let {
            index: obj_val,
            declared_schema: Some(js_schema.clone()),
            expr: value::field_of(Expr::Var(param_ref), "base"),
            line,
        },
        Stmt::Let {
            index: obj_slot,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field(obj_val, "ref_slot"),
            line,
        },
        Stmt::Let {
            index: obj_gen,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field(obj_val, "generation"),
            line,
        },
        Stmt::Let {
            index: key_slot,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field_of(Expr::Var(param_ref), "key_slot"),
            line,
        },
        Stmt::Let {
            index: key_gen,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field_of(Expr::Var(param_ref), "key_generation"),
            line,
        },
        Stmt::Let {
            index: obj_tag,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field(obj_val, "tag"),
            line,
        },
        assert_stmt(
            Expr::Eq(
                Box::new(Expr::Var(obj_tag)),
                Box::new(Expr::Int(value::OBJECT)),
            ),
            line,
        ),
        Stmt::Let {
            index: obj_base,
            declared_schema: Some(TypeSchema::Int),
            expr: checked(obj_slot, obj_gen),
            line,
        },
        Stmt::Let {
            index: obj_kind,
            declared_schema: Some(TypeSchema::Number),
            expr: read_base(obj_base, 0),
            line,
        },
        assert_stmt(
            Expr::Or(
                Box::new(Expr::Eq(
                    Box::new(Lowerer::as_number(Expr::Var(obj_kind))),
                    Box::new(Expr::Float(layout::KIND_OBJECT as f64)),
                )),
                Box::new(Expr::Eq(
                    Box::new(Lowerer::as_number(Expr::Var(obj_kind))),
                    Box::new(Expr::Float(layout::KIND_FUNCTION as f64)),
                )),
            ),
            line,
        ),
        Stmt::Let {
            index: count_num,
            declared_schema: Some(TypeSchema::Number),
            expr: read_base(obj_base, 2),
            line,
        },
        Stmt::Let {
            index: key_base,
            declared_schema: Some(TypeSchema::Int),
            expr: checked(key_slot, key_gen),
            line,
        },
        Stmt::Let {
            index: kind_num,
            declared_schema: Some(TypeSchema::Number),
            expr: read_base(key_base, 0),
            line,
        },
        assert_stmt(
            Expr::Eq(
                Box::new(Lowerer::as_number(Expr::Var(kind_num))),
                Box::new(Expr::Float(layout::KIND_STRING as f64)),
            ),
            line,
        ),
        Stmt::Let {
            index: search_len,
            declared_schema: Some(TypeSchema::Number),
            expr: read_base(key_base, 2),
            line,
        },
        Stmt::Let {
            index: found,
            declared_schema: Some(TypeSchema::Bool),
            expr: Expr::Bool(false),
            line,
        },
        Stmt::Let {
            index: prop_idx,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: res_tag,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: res_number,
            declared_schema: Some(TypeSchema::Float),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: res_flag,
            declared_schema: Some(TypeSchema::Bool),
            expr: Expr::Bool(false),
            line,
        },
        Stmt::Let {
            index: res_ref,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: res_gen,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: cur_key_slot,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: cur_key_gen,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: cur_key_base,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: cur_key_kind,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: cur_key_len,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: cur_tag_num,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: cur_number,
            declared_schema: Some(TypeSchema::Float),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: cur_flag_num,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: cur_ref_num,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: cur_gen_num,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: cur_tag,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: cur_ref,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: cur_gen,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: unit_cursor,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: unit_match,
            declared_schema: Some(TypeSchema::Bool),
            expr: Expr::Bool(true),
            line,
        },
        Stmt::Let {
            index: key_match,
            declared_schema: Some(TypeSchema::Bool),
            expr: Expr::Bool(false),
            line,
        },
        Stmt::Let {
            index: search_unit,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: stored_unit,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: validated,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
    ];
    // Row base for property `prop_idx`, cell `c`:
    // `obj_base + 5 + 7 * prop_idx + c` (object header is 5 cells now).
    let row_cell = |cell: i64| -> Expr {
        Expr::Add(
            Box::new(Expr::Add(
                Box::new(Expr::Var(obj_base)),
                Box::new(Expr::Add(
                    Box::new(Expr::Int(layout::OBJECT_HEADER_CELLS)),
                    Box::new(Expr::Mul(
                        Box::new(Expr::Var(prop_idx)),
                        Box::new(Expr::Int(layout::OBJECT_CELLS_PER_PROPERTY)),
                    )),
                )),
            )),
            Box::new(Expr::Int(cell)),
        )
    };
    let search_unit_read = Lowerer::guest_call(
        helper_read,
        vec![
            Expr::Var(param_carrier),
            Lowerer::as_number(Expr::Add(
                Box::new(Expr::Add(
                    Box::new(Expr::Var(key_base)),
                    Box::new(Expr::Int(layout::STRING_HEADER_CELLS)),
                )),
                Box::new(Expr::Var(unit_cursor)),
            )),
        ],
    );
    let stored_unit_read = Lowerer::guest_call(
        helper_read,
        vec![
            Expr::Var(param_carrier),
            Lowerer::as_number(Expr::Add(
                Box::new(Expr::Add(
                    Box::new(Expr::Var(cur_key_base)),
                    Box::new(Expr::Int(layout::STRING_HEADER_CELLS)),
                )),
                Box::new(Expr::Var(unit_cursor)),
            )),
        ],
    );
    let unit_loop = Stmt::While {
        condition: Expr::And(
            Box::new(Expr::Var(unit_match)),
            Box::new(Expr::Lt(
                Box::new(Lowerer::as_number(Expr::Var(unit_cursor))),
                Box::new(Expr::Var(search_len)),
            )),
        ),
        body: vec![
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: search_unit,
                expr: search_unit_read,
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: stored_unit,
                expr: stored_unit_read,
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: unit_match,
                expr: Expr::And(
                    Box::new(Expr::Var(unit_match)),
                    Box::new(Expr::Eq(
                        Box::new(Expr::Var(search_unit)),
                        Box::new(Expr::Var(stored_unit)),
                    )),
                ),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: unit_cursor,
                expr: Expr::Add(Box::new(Expr::Var(unit_cursor)), Box::new(Expr::Int(1))),
                line,
            },
        ],
        line,
    };
    let decode_call = |input: u16| -> Expr { Lowerer::guest_call(decode, vec![Expr::Var(input)]) };
    let decode_gen =
        |input: u16| -> Expr { Lowerer::guest_call(decode_u32, vec![Expr::Var(input)]) };
    let row_number = |cell: i64| -> Expr {
        Lowerer::guest_call(
            helper_read,
            vec![Expr::Var(param_carrier), Lowerer::as_number(row_cell(cell))],
        )
    };
    // Per-property body: read the seven cells, validate the stored key
    // pair through the checked base (never ignored), compare contents, and
    // on a hit validate plus reconstruct the tagged payload.
    let per_property: Vec<Stmt> = vec![
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: cur_tag_num,
            expr: row_number(0),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: cur_key_slot,
            expr: decode_call(cur_tag_num),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: cur_ref_num,
            expr: row_number(1),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: cur_key_gen,
            expr: decode_gen(cur_ref_num),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: cur_key_base,
            expr: checked(cur_key_slot, cur_key_gen),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: cur_tag_num,
            expr: Lowerer::guest_call(
                helper_read,
                vec![
                    Expr::Var(param_carrier),
                    Lowerer::as_number(Expr::Var(cur_key_base)),
                ],
            ),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: cur_key_kind,
            expr: decode_call(cur_tag_num),
            line,
        },
        assert_stmt(
            Expr::Eq(
                Box::new(Expr::Var(cur_key_kind)),
                Box::new(Expr::Int(layout::KIND_STRING)),
            ),
            line,
        ),
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: cur_key_len,
            expr: Lowerer::guest_call(
                helper_read,
                vec![
                    Expr::Var(param_carrier),
                    Lowerer::as_number(Expr::Add(
                        Box::new(Expr::Var(cur_key_base)),
                        Box::new(Expr::Int(2)),
                    )),
                ],
            ),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: unit_cursor,
            expr: Expr::Int(0),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: unit_match,
            expr: Expr::Bool(true),
            line,
        },
        unit_loop,
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: key_match,
            expr: Expr::And(
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(search_len)),
                    Box::new(Expr::Var(cur_key_len)),
                )),
                Box::new(Expr::Var(unit_match)),
            ),
            line,
        },
        Stmt::IfElse {
            condition: Expr::Var(key_match),
            then_branch: vec![
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: cur_tag_num,
                    expr: row_number(2),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: cur_tag,
                    expr: decode_call(cur_tag_num),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: cur_number,
                    expr: Lowerer::as_number(row_number(3)),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: cur_flag_num,
                    expr: row_number(4),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: cur_ref_num,
                    expr: row_number(5),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: cur_ref,
                    expr: decode_call(cur_ref_num),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: cur_gen_num,
                    expr: row_number(6),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: cur_gen,
                    expr: decode_gen(cur_gen_num),
                    line,
                },
                // Stored string/object payloads must resolve before the
                // value is reconstructed; anything else rides as scalars.
                Stmt::IfElse {
                    condition: Expr::Eq(
                        Box::new(Expr::Var(cur_tag)),
                        Box::new(Expr::Int(value::STRING)),
                    ),
                    then_branch: vec![
                        Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: validated,
                            expr: checked(cur_ref, cur_gen),
                            line,
                        },
                        Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: cur_tag_num,
                            expr: Lowerer::guest_call(
                                helper_read,
                                vec![
                                    Expr::Var(param_carrier),
                                    Lowerer::as_number(Expr::Var(validated)),
                                ],
                            ),
                            line,
                        },
                        Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: cur_key_kind,
                            expr: decode_call(cur_tag_num),
                            line,
                        },
                        assert_stmt(
                            Expr::Eq(
                                Box::new(Expr::Var(cur_key_kind)),
                                Box::new(Expr::Int(layout::KIND_STRING)),
                            ),
                            line,
                        ),
                    ],
                    else_branch: Vec::new(),
                    line,
                },
                Stmt::IfElse {
                    condition: Expr::Eq(
                        Box::new(Expr::Var(cur_tag)),
                        Box::new(Expr::Int(value::OBJECT)),
                    ),
                    then_branch: vec![
                        Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: validated,
                            expr: checked(cur_ref, cur_gen),
                            line,
                        },
                        Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: cur_tag_num,
                            expr: Lowerer::guest_call(
                                helper_read,
                                vec![
                                    Expr::Var(param_carrier),
                                    Lowerer::as_number(Expr::Var(validated)),
                                ],
                            ),
                            line,
                        },
                        Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: cur_key_kind,
                            expr: decode_call(cur_tag_num),
                            line,
                        },
                        assert_stmt(
                            Expr::Or(
                                Box::new(Expr::Eq(
                                    Box::new(Expr::Var(cur_key_kind)),
                                    Box::new(Expr::Int(layout::KIND_ARRAY)),
                                )),
                                Box::new(Expr::Or(
                                    Box::new(Expr::Eq(
                                        Box::new(Expr::Var(cur_key_kind)),
                                        Box::new(Expr::Int(layout::KIND_OBJECT)),
                                    )),
                                    Box::new(Expr::Eq(
                                        Box::new(Expr::Var(cur_key_kind)),
                                        Box::new(Expr::Int(layout::KIND_FUNCTION)),
                                    )),
                                )),
                            ),
                            line,
                        ),
                    ],
                    else_branch: Vec::new(),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: res_tag,
                    expr: Expr::Var(cur_tag),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: res_number,
                    expr: Expr::Var(cur_number),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: res_flag,
                    expr: Expr::Eq(
                        Box::new(Lowerer::as_number(Expr::Var(cur_flag_num))),
                        Box::new(Expr::Float(1.0)),
                    ),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: res_ref,
                    expr: Expr::Var(cur_ref),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: res_gen,
                    expr: Expr::Var(cur_gen),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: found,
                    expr: Expr::Bool(true),
                    line,
                },
            ],
            else_branch: vec![Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: prop_idx,
                expr: Expr::Add(Box::new(Expr::Var(prop_idx)), Box::new(Expr::Int(1))),
                line,
            }],
            line,
        },
    ];
    // The already checked UTF-16 key needs no allocation. Only exact
    // "length" is intrinsic on function nodes; other keys still refuse.
    let mut exact_length = Expr::Bool(true);
    for (position, unit) in "length".encode_utf16().enumerate() {
        exact_length = Expr::And(
            Box::new(exact_length),
            Box::new(Expr::Eq(
                Box::new(Lowerer::as_number(read_base(
                    key_base,
                    layout::STRING_HEADER_CELLS + position as i64,
                ))),
                Box::new(Expr::Float(f64::from(unit))),
            )),
        );
    }
    body.push(Stmt::IfElse {
        condition: Expr::Eq(
            Box::new(Lowerer::as_number(Expr::Var(obj_kind))),
            Box::new(Expr::Float(layout::KIND_FUNCTION as f64)),
        ),
        then_branch: vec![
            assert_stmt(
                Expr::Eq(
                    Box::new(Lowerer::as_number(Expr::Var(search_len))),
                    Box::new(Expr::Float(6.0)),
                ),
                line,
            ),
            assert_stmt(exact_length, line),
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: res_tag,
                expr: Expr::Int(value::NUMBER),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: res_number,
                expr: Lowerer::as_number(read_base(obj_base, layout::FUNCTION_LENGTH)),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: found,
                expr: Expr::Bool(true),
                line,
            },
        ],
        else_branch: Vec::new(),
        line,
    });
    body.push(Stmt::While {
        condition: Expr::And(
            Box::new(Expr::Not(Box::new(Expr::Var(found)))),
            Box::new(Expr::Lt(
                Box::new(Lowerer::as_number(Expr::Var(prop_idx))),
                Box::new(Expr::Var(count_num)),
            )),
        ),
        body: per_property,
        line,
    });
    // Deferred capability guard: missing own property, prototype lookup and
    // function-object standard members never produce semantic Undefined.
    body.push(assert_stmt(Expr::Var(found), line));
    let args = vec!["carrier".to_owned(), "reference".to_owned()];
    ir.functions.push(FunctionDecl {
        name: layout::REF_GET_HELPER.to_owned(),
        arity: 2,
        index,
        args: args.clone(),
        arg_schemas: vec![
            Some(carrier_schema.clone()),
            Some(value::reference_schema()),
        ],
        return_schema: Some(js_schema),
        type_params: Vec::new(),
        exported: false,
        return_type: ValueType::Map,
        symbol: None,
    });
    ir.function_impls.insert(
        index,
        FunctionImpl {
            param_slots: vec![param_carrier, param_ref],
            capture_copies: Vec::new(),
            body_stmts: body,
            body_expr: value::dynamic_value(
                Expr::Var(res_tag),
                Expr::Var(res_number),
                Expr::Var(res_flag),
                Expr::Var(res_ref),
                Expr::Var(res_gen),
            ),
            body_expr_line: line,
        },
    );
    statements.push(Stmt::FuncDecl {
        name: layout::REF_GET_HELPER.to_owned(),
        index,
        arity: 2,
        args,
        exported: false,
        has_impl: true,
        line,
    });
    Ok(index)
}

/// Append the finite static dispatch helper
/// `(Carrier, func: JsValue, this: JsValue, args: Array<JsValue>) ->
/// Completion`. Validates the function generation and kind through the
/// checked base before reading the code identity and environment edge,
/// then calls the statically compiled source function; unknown IDs and
/// non-function targets are capability guards, never silent Undefined.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_dispatch(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &TypeSchema,
    helper_read: u16,
    decode: u16,
    decode_u32: u16,
    checked_base: u16,
    func_indices: &[u16],
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let index = fresh_index(lower, ir, span)?;
    let line = lower.line(span);
    let param_carrier = lower.slot(span)?;
    let param_func = lower.slot(span)?;
    let param_this = lower.slot(span)?;
    let param_args = lower.slot(span)?;
    let func_slot = lower.slot(span)?;
    let func_gen = lower.slot(span)?;
    let func_base = lower.slot(span)?;
    let func_tag = lower.slot(span)?;
    let kind_num = lower.slot(span)?;
    let code_num = lower.slot(span)?;
    let code = lower.slot(span)?;
    let env_num = lower.slot(span)?;
    let env_slot = lower.slot(span)?;
    let env_gen_num = lower.slot(span)?;
    let env_gen = lower.slot(span)?;
    let validated = lower.slot(span)?;
    let result = lower.slot(span)?;
    let js_schema = value::schema();
    let args_schema = TypeSchema::Array(Box::new(js_schema.clone()));
    let mut body = vec![
        Stmt::Let {
            index: func_slot,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field(param_func, "ref_slot"),
            line,
        },
        Stmt::Let {
            index: func_gen,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field(param_func, "generation"),
            line,
        },
        Stmt::Let {
            index: func_tag,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field(param_func, "tag"),
            line,
        },
        assert_stmt(
            Expr::Eq(
                Box::new(Expr::Var(func_tag)),
                Box::new(Expr::Int(value::OBJECT)),
            ),
            line,
        ),
        Stmt::Let {
            index: func_base,
            declared_schema: Some(TypeSchema::Int),
            expr: Lowerer::guest_call(
                checked_base,
                vec![
                    Expr::Var(param_carrier),
                    Expr::Var(func_slot),
                    Expr::Var(func_gen),
                ],
            ),
            line,
        },
        Stmt::Let {
            index: kind_num,
            declared_schema: Some(TypeSchema::Number),
            expr: Lowerer::guest_call(
                helper_read,
                vec![
                    Expr::Var(param_carrier),
                    Lowerer::as_number(Expr::Var(func_base)),
                ],
            ),
            line,
        },
        assert_stmt(
            Expr::Eq(
                Box::new(Lowerer::as_number(Expr::Var(kind_num))),
                Box::new(Expr::Float(layout::KIND_FUNCTION as f64)),
            ),
            line,
        ),
        Stmt::Let {
            index: code_num,
            declared_schema: Some(TypeSchema::Number),
            expr: Lowerer::guest_call(
                helper_read,
                vec![
                    Expr::Var(param_carrier),
                    Lowerer::as_number(Expr::Add(
                        Box::new(Expr::Var(func_base)),
                        Box::new(Expr::Int(5)),
                    )),
                ],
            ),
            line,
        },
        Stmt::Let {
            index: code,
            declared_schema: Some(TypeSchema::Int),
            expr: Lowerer::guest_call(decode, vec![Expr::Var(code_num)]),
            line,
        },
        // A materialized environment edge travels with the call; empty
        // functions keep the explicit empty marker, validated when present.
        Stmt::Let {
            index: env_num,
            declared_schema: Some(TypeSchema::Number),
            expr: Lowerer::guest_call(
                helper_read,
                vec![
                    Expr::Var(param_carrier),
                    Lowerer::as_number(Expr::Add(
                        Box::new(Expr::Var(func_base)),
                        Box::new(Expr::Int(6)),
                    )),
                ],
            ),
            line,
        },
        Stmt::Let {
            index: env_slot,
            declared_schema: Some(TypeSchema::Int),
            expr: Lowerer::guest_call(decode, vec![Expr::Var(env_num)]),
            line,
        },
        Stmt::Let {
            index: env_gen_num,
            declared_schema: Some(TypeSchema::Number),
            expr: Lowerer::guest_call(
                helper_read,
                vec![
                    Expr::Var(param_carrier),
                    Lowerer::as_number(Expr::Add(
                        Box::new(Expr::Var(func_base)),
                        Box::new(Expr::Int(7)),
                    )),
                ],
            ),
            line,
        },
        Stmt::Let {
            index: env_gen,
            declared_schema: Some(TypeSchema::Int),
            expr: Lowerer::guest_call(decode_u32, vec![Expr::Var(env_gen_num)]),
            line,
        },
        Stmt::IfElse {
            condition: Expr::Not(Box::new(Expr::Eq(
                Box::new(Expr::Var(env_gen)),
                Box::new(Expr::Int(0)),
            ))),
            then_branch: vec![Stmt::Let {
                index: validated,
                declared_schema: Some(TypeSchema::Int),
                expr: Lowerer::guest_call(
                    checked_base,
                    vec![
                        Expr::Var(param_carrier),
                        Expr::Var(env_slot),
                        Expr::Var(env_gen),
                    ],
                ),
                line,
            }],
            else_branch: Vec::new(),
            line,
        },
        Stmt::Let {
            index: result,
            declared_schema: Some(value::completion_schema()),
            expr: value::completion_normal(),
            line,
        },
    ];
    // Finite branch table over static code IDs, last to first so the
    // outermost IfElse tests code 0.
    let mut tail: Vec<Stmt> = vec![assert_stmt(Expr::Bool(false), line)];
    for (code_id, func_index) in func_indices.iter().enumerate().rev() {
        let call = Lowerer::guest_call(
            *func_index,
            vec![
                Expr::Var(param_carrier),
                Expr::Var(env_slot),
                Expr::Var(param_this),
                Expr::Var(param_args),
                Expr::Int(0),
            ],
        );
        tail = vec![Stmt::IfElse {
            condition: Expr::Eq(
                Box::new(Expr::Var(code)),
                Box::new(Expr::Int(code_id as i64)),
            ),
            then_branch: vec![Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: result,
                expr: call,
                line,
            }],
            else_branch: tail,
            line,
        }];
    }
    body.extend(tail);
    let args = vec![
        "carrier".to_owned(),
        "func".to_owned(),
        "this_value".to_owned(),
        "args".to_owned(),
    ];
    ir.functions.push(FunctionDecl {
        name: layout::DISPATCH_HELPER.to_owned(),
        arity: 4,
        index,
        args: args.clone(),
        arg_schemas: vec![
            Some(carrier_schema.clone()),
            Some(js_schema.clone()),
            Some(js_schema),
            Some(args_schema),
        ],
        return_schema: Some(value::completion_schema()),
        type_params: Vec::new(),
        exported: false,
        return_type: ValueType::Map,
        symbol: None,
    });
    ir.function_impls.insert(
        index,
        FunctionImpl {
            param_slots: vec![param_carrier, param_func, param_this, param_args],
            capture_copies: Vec::new(),
            body_stmts: body,
            body_expr: Expr::Var(result),
            body_expr_line: line,
        },
    );
    statements.push(Stmt::FuncDecl {
        name: layout::DISPATCH_HELPER.to_owned(),
        index,
        arity: 4,
        args,
        exported: false,
        has_impl: true,
        line,
    });
    Ok(index)
}
