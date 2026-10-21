//! Private guest heap core: bootstrap, exact decoders, root frames, checked
//! access, raw reservation, node begin/publish, iterative collection,
//! metadata observation and prototype mutation.
//!
//! Guest numeric discipline: raw backing cells are Numbers (Float; freshly
//! resized cells read as Int 0 and are normalized through divide-by-one at
//! every read). Decoded slot/offset/count values are Ints in the exact
//! 17-bit domain; generations are Ints in the exact UInt32 domain. All
//! comparisons stay within one numeric type. Temps are pre-declared with
//! `Let` outside loops and updated with `Assign` inside; loop bodies never
//! introduce new slots, keeping native SSA loop edges balanced.

use super::super::heap as layout;
use super::super::lowerer::{Lowerer, assert_stmt};
use super::super::representation as value;
use vm::compiler::ir::{FunctionDecl, FunctionImpl};
use vm::{BuiltinFunction, Expr, FrontendIr, ParseError, Stmt, TypeSchema, ValueType};

/// Flat indices of every heap-core helper, threaded into producers and
/// consumers in one scope so no signature-only artifact is handed over.
/// Guest-internal indices (mark/reserve/collect/observe/set-prototype/edge
/// writers) execute through guest calls and name lookup in drivers; the
/// struct keeps the complete core contract in one place.
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub(crate) struct HeapHelpers {
    pub decode_u32: u16,
    pub root_enter: u16,
    pub root_write: u16,
    pub root_write_node: u16,
    pub root_leave: u16,
    pub checked_base: u16,
    pub mark: u16,
    pub raw_reserve: u16,
    pub node_begin: u16,
    pub node_publish: u16,
    pub node_publish_edge: u16,
    pub heap_collect: u16,
    pub heap_observe: u16,
    pub set_prototype: u16,
}

/// Read one raw backing cell through the B1 read helper. The address is an
/// Int expression; the result stays Number like every other realm read.
pub(crate) fn read_cell(helper_read: u16, carrier: u16, addr: Expr) -> Expr {
    Lowerer::guest_call(
        helper_read,
        vec![Expr::Var(carrier), Lowerer::as_number(addr)],
    )
}

/// Build (not emit) one raw backing write through the B1 write helper. The
/// payload must already be Number; the call result is the written value.
fn write_call(helper_write: u16, carrier: u16, addr: Expr, payload: Expr) -> Expr {
    Lowerer::guest_call(
        helper_write,
        vec![Expr::Var(carrier), Lowerer::as_number(addr), payload],
    )
}

/// Emit one consumed write: the B1 acknowledgment binds to a fresh Int/Number
/// slot instead of accumulating operand debris on the stack.
#[allow(clippy::too_many_arguments)]
fn emit_write(
    lower: &mut Lowerer<'_>,
    helper_write: u16,
    carrier: u16,
    addr: Expr,
    payload: Expr,
    number: bool,
    span: swc_common::Span,
    line: u32,
    body: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let ack = lower.slot(span)?;
    body.push(Stmt::Let {
        index: ack,
        declared_schema: Some(if number {
            TypeSchema::Number
        } else {
            TypeSchema::Int
        }),
        expr: write_call(helper_write, carrier, addr, payload),
        line,
    });
    Ok(())
}

/// Read one header cell as Number.
fn header_cell(helper_read: u16, carrier: u16, offset: i64) -> Expr {
    read_cell(helper_read, carrier, Expr::Int(offset))
}

/// Decode one Number cell to Int through the 17-bit slot decoder.
pub(crate) fn decode_int_call(decode: u16, input: Expr) -> Expr {
    Lowerer::guest_call(decode, vec![input])
}

/// Decode one Number cell to Int through the UInt32 generation decoder.
pub(crate) fn decode_gen_call(decode_u32: u16, input: Expr) -> Expr {
    Lowerer::guest_call(decode_u32, vec![input])
}

/// Carrier return schema shared by every heap helper taking the owner.
fn carrier_schema(
    lower: &Lowerer<'_>,
    ir: &FrontendIr,
    helper_make: u16,
    span: swc_common::Span,
) -> Result<TypeSchema, ParseError> {
    ir.functions
        .iter()
        .find(|function| function.index == helper_make)
        .and_then(|function| function.return_schema.clone())
        .ok_or_else(|| {
            lower.error(
                span,
                "JS heap helpers require the carrier helper return schema",
            )
        })
}

/// Fresh flat function index above every declared helper.
pub(crate) fn fresh_index(
    lower: &Lowerer<'_>,
    ir: &FrontendIr,
    span: swc_common::Span,
) -> Result<u16, ParseError> {
    ir.functions
        .iter()
        .map(|function| function.index)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| lower.error(span, "JS heap helper function index space exhausted"))
}

/// Declare one private guest helper with its implementation.
#[allow(clippy::too_many_arguments)]
pub(crate) fn declare_helper(
    ir: &mut FrontendIr,
    statements: &mut Vec<Stmt>,
    name: &str,
    index: u16,
    arity: u8,
    args: Vec<String>,
    arg_schemas: Vec<Option<TypeSchema>>,
    return_schema: Option<TypeSchema>,
    return_type: ValueType,
    param_slots: Vec<u16>,
    body: Vec<Stmt>,
    body_expr: Expr,
    line: u32,
) {
    ir.functions.push(FunctionDecl {
        name: name.to_owned(),
        arity,
        index,
        args: args.clone(),
        arg_schemas,
        return_schema,
        type_params: Vec::new(),
        exported: false,
        return_type,
        symbol: None,
    });
    ir.function_impls.insert(
        index,
        FunctionImpl {
            param_slots,
            capture_copies: Vec::new(),
            body_stmts: body,
            body_expr,
            body_expr_line: line,
        },
    );
    statements.push(Stmt::FuncDecl {
        name: name.to_owned(),
        index,
        arity,
        args,
        exported: false,
        has_impl: true,
        line,
    });
}

/// Emit the directory row lookup for Int slot temp `slot`:
/// `page_out`/`row_out` get the page/row indices, `addr_out` the row base
/// (block + 3 + 7 * row). All five temps are caller-owned; the first call
/// declares them and looped calls reuse them through updates. With `require`
/// the helper asserts a live page; otherwise `addr_out` is 0 when the page
/// is absent and the caller skips.
#[allow(clippy::too_many_arguments)]
fn emit_row_lookup(
    helper_read: u16,
    decode: u16,
    carrier: u16,
    slot: u16,
    line: u32,
    body: &mut Vec<Stmt>,
    declare: bool,
    page_out: u16,
    row_out: u16,
    addr_out: u16,
    page_ptr_num: u16,
    page_ptr: u16,
    require: bool,
) {
    body.push(bind_slot(
        page_out,
        Some(TypeSchema::Int),
        Expr::Int(0),
        line,
        declare,
    ));
    body.push(bind_slot(
        row_out,
        Some(TypeSchema::Int),
        Expr::Var(slot),
        line,
        declare,
    ));
    body.push(Stmt::While {
        condition: Expr::Or(
            Box::new(Expr::Gt(
                Box::new(Expr::Var(row_out)),
                Box::new(Expr::Int(layout::PAGE_ROWS)),
            )),
            Box::new(Expr::Eq(
                Box::new(Expr::Var(row_out)),
                Box::new(Expr::Int(layout::PAGE_ROWS)),
            )),
        ),
        body: vec![
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: row_out,
                expr: Expr::Sub(
                    Box::new(Expr::Var(row_out)),
                    Box::new(Expr::Int(layout::PAGE_ROWS)),
                ),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: page_out,
                expr: Expr::Add(Box::new(Expr::Var(page_out)), Box::new(Expr::Int(1))),
                line,
            },
        ],
        line,
    });
    body.push(bind_slot(
        page_ptr_num,
        Some(TypeSchema::Number),
        read_cell(
            helper_read,
            carrier,
            Expr::Add(
                Box::new(Expr::Int(layout::HEADER_CELLS)),
                Box::new(Expr::Var(page_out)),
            ),
        ),
        line,
        declare,
    ));
    body.push(bind_slot(
        page_ptr,
        Some(TypeSchema::Int),
        decode_int_call(decode, Expr::Var(page_ptr_num)),
        line,
        declare,
    ));
    if require {
        body.push(assert_stmt(
            Expr::Not(Box::new(Expr::Eq(
                Box::new(Expr::Var(page_ptr)),
                Box::new(Expr::Int(0)),
            ))),
            line,
        ));
    }
    body.push(bind_slot(
        addr_out,
        Some(TypeSchema::Int),
        Expr::IfElse {
            condition: Box::new(Expr::Eq(
                Box::new(Expr::Var(page_ptr)),
                Box::new(Expr::Int(0)),
            )),
            then_expr: Box::new(Expr::Int(0)),
            else_expr: Box::new(Expr::Add(
                Box::new(Expr::Add(
                    Box::new(Expr::Var(page_ptr)),
                    Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
                )),
                Box::new(Expr::Mul(
                    Box::new(Expr::Var(row_out)),
                    Box::new(Expr::Int(layout::ROW_CELLS)),
                )),
            )),
        },
        line,
        declare,
    ));
}

/// Bootstrap one realm: the single owning carrier plus an explicit fixed
/// header (version, heap end, empty free/root/slot accounting) and a zeroed
/// page-pointer table. Every cell is written before any user node exists;
/// `heap_end` agrees with the backing logical length afterwards.
pub(crate) fn emit_bootstrap(
    lower: &mut Lowerer<'_>,
    helper_make: u16,
    helper_write: u16,
    helper_resize: u16,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let line = lower.line(span);
    let carrier = lower.slot(span)?;
    let empty = Lowerer::builtin_call(BuiltinFunction::ArrayNew.call_index(), Vec::new());
    let made = Lowerer::guest_call(helper_make, vec![empty]);
    statements.push(Stmt::Let {
        index: carrier,
        declared_schema: None,
        expr: made,
        line,
    });
    let resized = lower.slot(span)?;
    statements.push(Stmt::Let {
        index: resized,
        declared_schema: Some(TypeSchema::Number),
        expr: Lowerer::guest_call(
            helper_resize,
            vec![
                Expr::Var(carrier),
                Lowerer::as_number(Expr::Int(layout::PREFIX_CELLS)),
            ],
        ),
        line,
    });
    let mut header = [0i64; 16];
    header[layout::HDR_VERSION as usize] = layout::REALM_VERSION;
    header[layout::HDR_HEAP_END as usize] = layout::PREFIX_CELLS;
    for (offset, marker) in header.iter().enumerate() {
        emit_write(
            lower,
            helper_write,
            carrier,
            Expr::Int(offset as i64),
            Lowerer::as_number(Expr::Int(*marker)),
            true,
            span,
            line,
            statements,
        )?;
    }
    for page in 0..layout::PAGE_POINTER_CELLS {
        emit_write(
            lower,
            helper_write,
            carrier,
            Expr::Int(layout::HEADER_CELLS + page),
            Lowerer::as_number(Expr::Int(0)),
            true,
            span,
            line,
            statements,
        )?;
    }
    Ok(carrier)
}

/// Append the exact UInt32 generation decoder `(Number) -> Int`: bounded
/// binary decomposition over 2^31 down to 2^0 with Float remainder
/// subtraction plus Int accumulation, then a zero-remainder guard. Separate
/// from the 17-bit slot decoder; generations never ride the slot domain.
pub(crate) fn append_decode_u32(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let index = fresh_index(lower, ir, span)?;
    let line = lower.line(span);
    let param_x = lower.slot(span)?;
    let remaining = lower.slot(span)?;
    let decoded = lower.slot(span)?;
    let mut body = vec![
        Stmt::Let {
            index: remaining,
            declared_schema: Some(TypeSchema::Number),
            expr: Lowerer::as_number(Expr::Var(param_x)),
            line,
        },
        assert_stmt(
            Expr::Not(Box::new(Expr::Lt(
                Box::new(Expr::Var(remaining)),
                Box::new(Expr::Float(0.0)),
            ))),
            line,
        ),
        assert_stmt(
            Expr::Or(
                Box::new(Expr::Lt(
                    Box::new(Expr::Var(remaining)),
                    Box::new(Expr::Float(layout::MAX_GENERATION_F64)),
                )),
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(remaining)),
                    Box::new(Expr::Float(layout::MAX_GENERATION_F64)),
                )),
            ),
            line,
        ),
        Stmt::Let {
            index: decoded,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
    ];
    for bit in (0..=31u32).rev() {
        let threshold = f64::from(1u32 << bit);
        let bit_value = 1i64 << bit;
        body.push(Stmt::IfElse {
            condition: Expr::Or(
                Box::new(Expr::Gt(
                    Box::new(Expr::Var(remaining)),
                    Box::new(Expr::Float(threshold)),
                )),
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(remaining)),
                    Box::new(Expr::Float(threshold)),
                )),
            ),
            then_branch: vec![
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: remaining,
                    expr: Expr::Sub(
                        Box::new(Expr::Var(remaining)),
                        Box::new(Expr::Float(threshold)),
                    ),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: decoded,
                    expr: Expr::Add(Box::new(Expr::Var(decoded)), Box::new(Expr::Int(bit_value))),
                    line,
                },
            ],
            else_branch: Vec::new(),
            line,
        });
    }
    body.push(assert_stmt(
        Expr::Eq(Box::new(Expr::Var(remaining)), Box::new(Expr::Float(0.0))),
        line,
    ));
    declare_helper(
        ir,
        statements,
        layout::DECODE_U32_HELPER,
        index,
        1,
        vec!["x".to_owned()],
        vec![Some(TypeSchema::Number)],
        Some(TypeSchema::Int),
        ValueType::Int,
        vec![param_x],
        body,
        Expr::Var(decoded),
        line,
    );
    Ok(index)
}

/// Append the root-frame entry helper `(Carrier, capacity: Int) -> Int`.
/// Reserves one raw system block, zeroes every entry, links it on the
/// header root chain and accounts active capacity. Under the caller-root
/// invariant (script start with no user nodes, or callee entry with caller
/// inputs already rooted) an interval-triggered collection may run before
/// the frame is pushed. Returns the frame block address.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_root_enter(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &TypeSchema,
    helper_read: u16,
    helper_write: u16,
    decode: u16,
    collect: u16,
    raw_reserve: u16,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let index = fresh_index(lower, ir, span)?;
    let line = lower.line(span);
    let param_carrier = lower.slot(span)?;
    let param_cap = lower.slot(span)?;
    let since_num = lower.slot(span)?;
    let since = lower.slot(span)?;
    let cap_total_num = lower.slot(span)?;
    let cap_total = lower.slot(span)?;
    let size = lower.slot(span)?;
    let data = lower.slot(span)?;
    let top_num = lower.slot(span)?;
    let top = lower.slot(span)?;
    let cursor = lower.slot(span)?;
    let sink = lower.slot(span)?;
    let mut body = vec![
        Stmt::Let {
            index: sink,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: since_num,
            declared_schema: Some(TypeSchema::Number),
            expr: header_cell(helper_read, param_carrier, layout::HDR_ALLOCATIONS_SINCE_GC),
            line,
        },
        Stmt::Let {
            index: since,
            declared_schema: Some(TypeSchema::Int),
            expr: decode_int_call(decode, Expr::Var(since_num)),
            line,
        },
        assert_stmt(
            Expr::Not(Box::new(Expr::Lt(
                Box::new(Expr::Var(param_cap)),
                Box::new(Expr::Int(0)),
            ))),
            line,
        ),
        assert_stmt(
            Expr::Or(
                Box::new(Expr::Lt(
                    Box::new(Expr::Var(param_cap)),
                    Box::new(Expr::Int(layout::MAX_ACTIVE_ROOT_ENTRIES)),
                )),
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(param_cap)),
                    Box::new(Expr::Int(layout::MAX_ACTIVE_ROOT_ENTRIES)),
                )),
            ),
            line,
        ),
        Stmt::Let {
            index: cap_total_num,
            declared_schema: Some(TypeSchema::Number),
            expr: header_cell(helper_read, param_carrier, layout::HDR_ACTIVE_ROOT_CAPACITY),
            line,
        },
        Stmt::Let {
            index: cap_total,
            declared_schema: Some(TypeSchema::Int),
            expr: decode_int_call(decode, Expr::Var(cap_total_num)),
            line,
        },
        assert_stmt(
            Expr::Or(
                Box::new(Expr::Lt(
                    Box::new(Expr::Add(
                        Box::new(Expr::Var(cap_total)),
                        Box::new(Expr::Var(param_cap)),
                    )),
                    Box::new(Expr::Int(layout::MAX_ACTIVE_ROOT_ENTRIES)),
                )),
                Box::new(Expr::Eq(
                    Box::new(Expr::Add(
                        Box::new(Expr::Var(cap_total)),
                        Box::new(Expr::Var(param_cap)),
                    )),
                    Box::new(Expr::Int(layout::MAX_ACTIVE_ROOT_ENTRIES)),
                )),
            ),
            line,
        ),
        // Pressure collection only under the caller-root invariant: the
        // script's first frame precedes every user node, and callee entry
        // runs while actual caller inputs stay rooted.
        Stmt::IfElse {
            condition: Expr::Or(
                Box::new(Expr::Gt(
                    Box::new(Expr::Var(since)),
                    Box::new(Expr::Int(layout::GC_INTERVAL_ALLOCATIONS)),
                )),
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(since)),
                    Box::new(Expr::Int(layout::GC_INTERVAL_ALLOCATIONS)),
                )),
            ),
            then_branch: vec![Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: sink,
                expr: Lowerer::as_number(Lowerer::guest_call(
                    collect,
                    vec![Expr::Var(param_carrier)],
                )),
                line,
            }],
            else_branch: Vec::new(),
            line,
        },
        Stmt::Let {
            index: size,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Add(
                Box::new(Expr::Int(layout::FRAME_FIXED_CELLS)),
                Box::new(Expr::Mul(
                    Box::new(Expr::Var(param_cap)),
                    Box::new(Expr::Int(layout::FRAME_CELLS_PER_ENTRY)),
                )),
            ),
            line,
        },
        Stmt::Let {
            index: data,
            declared_schema: Some(TypeSchema::Int),
            expr: Lowerer::guest_call(
                raw_reserve,
                vec![
                    Expr::Var(param_carrier),
                    Expr::Var(size),
                    Expr::Int(layout::RAW_CLASS_ROOT_FRAME),
                ],
            ),
            line,
        },
        Stmt::Let {
            index: top_num,
            declared_schema: Some(TypeSchema::Number),
            expr: header_cell(helper_read, param_carrier, layout::HDR_ROOT_TOP),
            line,
        },
        Stmt::Let {
            index: top,
            declared_schema: Some(TypeSchema::Int),
            expr: decode_int_call(decode, Expr::Var(top_num)),
            line,
        },
    ];
    // Capacity cell, then zeroed entries: reused raw storage must never read
    // as a live root. Fire-and-forget writes consume into one reused sink so
    // repeated helper calls never accumulate operand debris on the stack.
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: sink,
        expr: write_call(
            helper_write,
            param_carrier,
            Expr::Var(data),
            Lowerer::as_number(Expr::Var(param_cap)),
        ),
        line,
    });
    body.push(Stmt::Let {
        index: cursor,
        declared_schema: Some(TypeSchema::Int),
        expr: Expr::Int(0),
        line,
    });
    body.push(Stmt::While {
        condition: Expr::Lt(Box::new(Expr::Var(cursor)), Box::new(Expr::Var(param_cap))),
        body: vec![
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: sink,
                expr: write_call(
                    helper_write,
                    param_carrier,
                    Expr::Add(
                        Box::new(Expr::Add(Box::new(Expr::Var(data)), Box::new(Expr::Int(1)))),
                        Box::new(Expr::Mul(
                            Box::new(Expr::Var(cursor)),
                            Box::new(Expr::Int(layout::FRAME_CELLS_PER_ENTRY)),
                        )),
                    ),
                    Lowerer::as_number(Expr::Int(0)),
                ),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: sink,
                expr: write_call(
                    helper_write,
                    param_carrier,
                    Expr::Add(
                        Box::new(Expr::Add(Box::new(Expr::Var(data)), Box::new(Expr::Int(2)))),
                        Box::new(Expr::Mul(
                            Box::new(Expr::Var(cursor)),
                            Box::new(Expr::Int(layout::FRAME_CELLS_PER_ENTRY)),
                        )),
                    ),
                    Lowerer::as_number(Expr::Int(0)),
                ),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: sink,
                expr: write_call(
                    helper_write,
                    param_carrier,
                    Expr::Add(
                        Box::new(Expr::Add(Box::new(Expr::Var(data)), Box::new(Expr::Int(3)))),
                        Box::new(Expr::Mul(
                            Box::new(Expr::Var(cursor)),
                            Box::new(Expr::Int(layout::FRAME_CELLS_PER_ENTRY)),
                        )),
                    ),
                    Lowerer::as_number(Expr::Int(0)),
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
    // Link: the raw header's free-next cell becomes the previous-frame link,
    // then publish the new top and the accounted capacity.
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: sink,
        expr: write_call(
            helper_write,
            param_carrier,
            Expr::Sub(Box::new(Expr::Var(data)), Box::new(Expr::Int(1))),
            Lowerer::as_number(Expr::Var(top)),
        ),
        line,
    });
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: sink,
        expr: write_call(
            helper_write,
            param_carrier,
            Expr::Int(layout::HDR_ROOT_TOP),
            Lowerer::as_number(Expr::Sub(
                Box::new(Expr::Var(data)),
                Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
            )),
        ),
        line,
    });
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: sink,
        expr: write_call(
            helper_write,
            param_carrier,
            Expr::Int(layout::HDR_ACTIVE_ROOT_CAPACITY),
            Lowerer::as_number(Expr::Add(
                Box::new(Expr::Var(cap_total)),
                Box::new(Expr::Var(param_cap)),
            )),
        ),
        line,
    });

    declare_helper(
        ir,
        statements,
        layout::ROOT_ENTER_HELPER,
        index,
        2,
        vec!["carrier".to_owned(), "capacity".to_owned()],
        vec![Some(carrier_schema.clone()), Some(TypeSchema::Int)],
        Some(TypeSchema::Int),
        ValueType::Int,
        vec![param_carrier, param_cap],
        body,
        Expr::Sub(
            Box::new(Expr::Var(data)),
            Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
        ),
        line,
    );
    Ok(index)
}

/// Shared frame/entry validation: the frame must be a live system block
/// below the heap end, and the entry must lie inside its capacity.
#[allow(clippy::too_many_arguments)]
fn emit_frame_check(
    helper_read: u16,
    decode: u16,
    param_carrier: u16,
    frame: u16,
    entry: u16,
    line: u32,
    body: &mut Vec<Stmt>,
    class_num: u16,
    class: u16,
    cap_num: u16,
    cap: u16,
    end_num: u16,
    end: u16,
) {
    body.push(Stmt::Let {
        index: end_num,
        declared_schema: Some(TypeSchema::Number),
        expr: header_cell(helper_read, param_carrier, layout::HDR_HEAP_END),
        line,
    });
    body.push(Stmt::Let {
        index: end,
        declared_schema: Some(TypeSchema::Int),
        expr: decode_int_call(decode, Expr::Var(end_num)),
        line,
    });
    body.push(assert_stmt(
        Expr::And(
            Box::new(Expr::Or(
                Box::new(Expr::Gt(
                    Box::new(Expr::Var(frame)),
                    Box::new(Expr::Int(layout::PREFIX_CELLS)),
                )),
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(frame)),
                    Box::new(Expr::Int(layout::PREFIX_CELLS)),
                )),
            )),
            Box::new(Expr::Lt(
                Box::new(Expr::Var(frame)),
                Box::new(Expr::Var(end)),
            )),
        ),
        line,
    ));
    body.push(Stmt::Let {
        index: class_num,
        declared_schema: Some(TypeSchema::Number),
        expr: read_cell(
            helper_read,
            param_carrier,
            Expr::Add(
                Box::new(Expr::Var(frame)),
                Box::new(Expr::Int(layout::RAW_CLASS)),
            ),
        ),
        line,
    });
    body.push(Stmt::Let {
        index: class,
        declared_schema: Some(TypeSchema::Int),
        expr: decode_int_call(decode, Expr::Var(class_num)),
        line,
    });
    body.push(assert_stmt(
        Expr::Eq(
            Box::new(Expr::Var(class)),
            Box::new(Expr::Int(layout::RAW_CLASS_ROOT_FRAME)),
        ),
        line,
    ));
    body.push(Stmt::Let {
        index: cap_num,
        declared_schema: Some(TypeSchema::Number),
        expr: read_cell(
            helper_read,
            param_carrier,
            Expr::Add(
                Box::new(Expr::Var(frame)),
                Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
            ),
        ),
        line,
    });
    body.push(Stmt::Let {
        index: cap,
        declared_schema: Some(TypeSchema::Int),
        expr: decode_int_call(decode, Expr::Var(cap_num)),
        line,
    });
    body.push(assert_stmt(
        Expr::And(
            Box::new(Expr::Not(Box::new(Expr::Lt(
                Box::new(Expr::Var(entry)),
                Box::new(Expr::Int(0)),
            )))),
            Box::new(Expr::Lt(
                Box::new(Expr::Var(entry)),
                Box::new(Expr::Var(cap)),
            )),
        ),
        line,
    ));
}

/// Append the root write helper
/// `(Carrier, frame: Int, entry: Int, value: JsValue) -> Int ack`.
/// Derives the entry category from the boxed tag (strings and object-like
/// nodes pin their slot/generation; primitives clear the entry), writes the
/// triple directly and returns an acknowledgment. Never allocates or
/// collects.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_root_write(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &TypeSchema,
    helper_read: u16,
    helper_write: u16,
    decode: u16,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let index = fresh_index(lower, ir, span)?;
    let line = lower.line(span);
    let param_carrier = lower.slot(span)?;
    let param_frame = lower.slot(span)?;
    let param_entry = lower.slot(span)?;
    let param_value = lower.slot(span)?;
    let class_num = lower.slot(span)?;
    let class = lower.slot(span)?;
    let cap_num = lower.slot(span)?;
    let cap = lower.slot(span)?;
    let end_num = lower.slot(span)?;
    let end = lower.slot(span)?;
    let tag = lower.slot(span)?;
    let ref_slot = lower.slot(span)?;
    let generation = lower.slot(span)?;
    let category = lower.slot(span)?;
    let out_slot = lower.slot(span)?;
    let out_gen = lower.slot(span)?;
    let sink = lower.slot(span)?;
    let js_schema = value::schema();
    let mut body = vec![Stmt::Let {
        index: sink,
        declared_schema: Some(TypeSchema::Number),
        expr: Expr::Float(0.0),
        line,
    }];
    emit_frame_check(
        helper_read,
        decode,
        param_carrier,
        param_frame,
        param_entry,
        line,
        &mut body,
        class_num,
        class,
        cap_num,
        cap,
        end_num,
        end,
    );
    body.push(Stmt::Let {
        index: tag,
        declared_schema: Some(TypeSchema::Int),
        expr: value::field(param_value, "tag"),
        line,
    });
    body.push(Stmt::Let {
        index: ref_slot,
        declared_schema: Some(TypeSchema::Int),
        expr: value::field(param_value, "ref_slot"),
        line,
    });
    body.push(Stmt::Let {
        index: generation,
        declared_schema: Some(TypeSchema::Int),
        expr: value::field(param_value, "generation"),
        line,
    });
    let is_string = Expr::Eq(Box::new(Expr::Var(tag)), Box::new(Expr::Int(value::STRING)));
    let is_object = Expr::Eq(Box::new(Expr::Var(tag)), Box::new(Expr::Int(value::OBJECT)));
    body.push(Stmt::Let {
        index: category,
        declared_schema: Some(TypeSchema::Int),
        expr: Expr::IfElse {
            condition: Box::new(is_string),
            then_expr: Box::new(Expr::Int(layout::ROOT_STRING)),
            else_expr: Box::new(Expr::IfElse {
                condition: Box::new(is_object),
                then_expr: Box::new(Expr::Int(layout::ROOT_OBJECT)),
                else_expr: Box::new(Expr::Int(layout::ROOT_EMPTY)),
            }),
        },
        line,
    });
    body.push(Stmt::Let {
        index: out_slot,
        declared_schema: Some(TypeSchema::Int),
        expr: Expr::IfElse {
            condition: Box::new(Expr::Eq(
                Box::new(Expr::Var(category)),
                Box::new(Expr::Int(layout::ROOT_EMPTY)),
            )),
            then_expr: Box::new(Expr::Int(0)),
            else_expr: Box::new(Expr::Var(ref_slot)),
        },
        line,
    });
    body.push(Stmt::Let {
        index: out_gen,
        declared_schema: Some(TypeSchema::Int),
        expr: Expr::IfElse {
            condition: Box::new(Expr::Eq(
                Box::new(Expr::Var(category)),
                Box::new(Expr::Int(layout::ROOT_EMPTY)),
            )),
            then_expr: Box::new(Expr::Int(0)),
            else_expr: Box::new(Expr::Var(generation)),
        },
        line,
    });
    let entry_base = Expr::Add(
        Box::new(Expr::Add(
            Box::new(Expr::Var(param_frame)),
            Box::new(Expr::Int(layout::FRAME_FIXED_CELLS)),
        )),
        Box::new(Expr::Mul(
            Box::new(Expr::Var(param_entry)),
            Box::new(Expr::Int(layout::FRAME_CELLS_PER_ENTRY)),
        )),
    );
    for (cell, payload) in [
        (0, Expr::Var(category)),
        (1, Expr::Var(out_slot)),
        (2, Expr::Var(out_gen)),
    ] {
        body.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: sink,
            expr: write_call(
                helper_write,
                param_carrier,
                Expr::Add(Box::new(entry_base.clone()), Box::new(Expr::Int(cell))),
                Lowerer::as_number(payload),
            ),
            line,
        });
    }

    declare_helper(
        ir,
        statements,
        layout::ROOT_WRITE_HELPER,
        index,
        4,
        vec![
            "carrier".to_owned(),
            "frame".to_owned(),
            "entry".to_owned(),
            "value".to_owned(),
        ],
        vec![
            Some(carrier_schema.clone()),
            Some(TypeSchema::Int),
            Some(TypeSchema::Int),
            Some(js_schema),
        ],
        Some(TypeSchema::Int),
        ValueType::Int,
        vec![param_carrier, param_frame, param_entry, param_value],
        body,
        Expr::Int(1),
        line,
    );
    Ok(index)
}

/// Append the explicit edge root helper
/// `(Carrier, frame, entry, category, slot, gen: Int) -> Int ack` for
/// internal graph edges (environment nodes) that never become user JS
/// objects. Validates the edge through the checked base before pinning it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_root_write_node(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &TypeSchema,
    helper_read: u16,
    helper_write: u16,
    decode: u16,
    checked_base: u16,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let index = fresh_index(lower, ir, span)?;
    let line = lower.line(span);
    let param_carrier = lower.slot(span)?;
    let param_frame = lower.slot(span)?;
    let param_entry = lower.slot(span)?;
    let param_cat = lower.slot(span)?;
    let param_slot = lower.slot(span)?;
    let param_gen = lower.slot(span)?;
    let class_num = lower.slot(span)?;
    let class = lower.slot(span)?;
    let cap_num = lower.slot(span)?;
    let cap = lower.slot(span)?;
    let end_num = lower.slot(span)?;
    let end = lower.slot(span)?;
    let sink = lower.slot(span)?;
    let mut body = vec![Stmt::Let {
        index: sink,
        declared_schema: Some(TypeSchema::Number),
        expr: Expr::Float(0.0),
        line,
    }];
    emit_frame_check(
        helper_read,
        decode,
        param_carrier,
        param_frame,
        param_entry,
        line,
        &mut body,
        class_num,
        class,
        cap_num,
        cap,
        end_num,
        end,
    );
    // Explicit edges name object-like (string 4, object 5) or internal (6)
    // graph targets; no other category is a valid edge.
    body.push(assert_stmt(
        Expr::Or(
            Box::new(Expr::Or(
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(param_cat)),
                    Box::new(Expr::Int(layout::ROOT_STRING)),
                )),
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(param_cat)),
                    Box::new(Expr::Int(layout::ROOT_OBJECT)),
                )),
            )),
            Box::new(Expr::Eq(
                Box::new(Expr::Var(param_cat)),
                Box::new(Expr::Int(layout::ROOT_INTERNAL)),
            )),
        ),
        line,
    ));
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: sink,
        expr: Lowerer::as_number(Lowerer::guest_call(
            checked_base,
            vec![
                Expr::Var(param_carrier),
                Expr::Var(param_slot),
                Expr::Var(param_gen),
            ],
        )),
        line,
    });
    let entry_base = Expr::Add(
        Box::new(Expr::Add(
            Box::new(Expr::Var(param_frame)),
            Box::new(Expr::Int(layout::FRAME_FIXED_CELLS)),
        )),
        Box::new(Expr::Mul(
            Box::new(Expr::Var(param_entry)),
            Box::new(Expr::Int(layout::FRAME_CELLS_PER_ENTRY)),
        )),
    );
    for (cell, payload) in [
        (0, Expr::Var(param_cat)),
        (1, Expr::Var(param_slot)),
        (2, Expr::Var(param_gen)),
    ] {
        body.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: sink,
            expr: write_call(
                helper_write,
                param_carrier,
                Expr::Add(Box::new(entry_base.clone()), Box::new(Expr::Int(cell))),
                Lowerer::as_number(payload),
            ),
            line,
        });
    }
    // The validated edge keeps the target live through auditing lints while
    // the entry triple carries the actual root.
    declare_helper(
        ir,
        statements,
        layout::ROOT_WRITE_NODE_HELPER,
        index,
        6,
        vec![
            "carrier".to_owned(),
            "frame".to_owned(),
            "entry".to_owned(),
            "category".to_owned(),
            "slot".to_owned(),
            "generation".to_owned(),
        ],
        vec![
            Some(carrier_schema.clone()),
            Some(TypeSchema::Int),
            Some(TypeSchema::Int),
            Some(TypeSchema::Int),
            Some(TypeSchema::Int),
            Some(TypeSchema::Int),
        ],
        Some(TypeSchema::Int),
        ValueType::Int,
        vec![
            param_carrier,
            param_frame,
            param_entry,
            param_cat,
            param_slot,
            param_gen,
        ],
        body,
        Expr::Int(1),
        line,
    );
    Ok(index)
}

/// Append the frame leave helper `(Carrier, frame: Int) -> Int ack`.
/// Unlinks the top frame in LIFO order, accounts capacity down and returns
/// its raw block to the free list at once; the old contents never remain on
/// the active root chain. Never allocates or collects.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_root_leave(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &TypeSchema,
    helper_read: u16,
    helper_write: u16,
    decode: u16,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let index = fresh_index(lower, ir, span)?;
    let line = lower.line(span);
    let param_carrier = lower.slot(span)?;
    let param_frame = lower.slot(span)?;
    let top_num = lower.slot(span)?;
    let top = lower.slot(span)?;
    let class_num = lower.slot(span)?;
    let class = lower.slot(span)?;
    let cap_num = lower.slot(span)?;
    let cap = lower.slot(span)?;
    let total_num = lower.slot(span)?;
    let total = lower.slot(span)?;
    let prev_num = lower.slot(span)?;
    let prev = lower.slot(span)?;
    let free_num = lower.slot(span)?;
    let free = lower.slot(span)?;
    let sink = lower.slot(span)?;
    let mut body = vec![
        Stmt::Let {
            index: sink,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: top_num,
            declared_schema: Some(TypeSchema::Number),
            expr: header_cell(helper_read, param_carrier, layout::HDR_ROOT_TOP),
            line,
        },
        Stmt::Let {
            index: top,
            declared_schema: Some(TypeSchema::Int),
            expr: decode_int_call(decode, Expr::Var(top_num)),
            line,
        },
        // Frames leave in strict LIFO order.
        assert_stmt(
            Expr::Eq(Box::new(Expr::Var(top)), Box::new(Expr::Var(param_frame))),
            line,
        ),
        Stmt::Let {
            index: class_num,
            declared_schema: Some(TypeSchema::Number),
            expr: read_cell(
                helper_read,
                param_carrier,
                Expr::Add(
                    Box::new(Expr::Var(param_frame)),
                    Box::new(Expr::Int(layout::RAW_CLASS)),
                ),
            ),
            line,
        },
        Stmt::Let {
            index: class,
            declared_schema: Some(TypeSchema::Int),
            expr: decode_int_call(decode, Expr::Var(class_num)),
            line,
        },
        assert_stmt(
            Expr::Eq(
                Box::new(Expr::Var(class)),
                Box::new(Expr::Int(layout::RAW_CLASS_ROOT_FRAME)),
            ),
            line,
        ),
        Stmt::Let {
            index: cap_num,
            declared_schema: Some(TypeSchema::Number),
            expr: read_cell(
                helper_read,
                param_carrier,
                Expr::Add(
                    Box::new(Expr::Var(param_frame)),
                    Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
                ),
            ),
            line,
        },
        Stmt::Let {
            index: cap,
            declared_schema: Some(TypeSchema::Int),
            expr: decode_int_call(decode, Expr::Var(cap_num)),
            line,
        },
        Stmt::Let {
            index: total_num,
            declared_schema: Some(TypeSchema::Number),
            expr: header_cell(helper_read, param_carrier, layout::HDR_ACTIVE_ROOT_CAPACITY),
            line,
        },
        Stmt::Let {
            index: total,
            declared_schema: Some(TypeSchema::Int),
            expr: decode_int_call(decode, Expr::Var(total_num)),
            line,
        },
        Stmt::Let {
            index: prev_num,
            declared_schema: Some(TypeSchema::Number),
            expr: read_cell(
                helper_read,
                param_carrier,
                Expr::Add(
                    Box::new(Expr::Var(param_frame)),
                    Box::new(Expr::Int(layout::RAW_FREE_NEXT)),
                ),
            ),
            line,
        },
        Stmt::Let {
            index: prev,
            declared_schema: Some(TypeSchema::Int),
            expr: decode_int_call(decode, Expr::Var(prev_num)),
            line,
        },
        Stmt::Let {
            index: free_num,
            declared_schema: Some(TypeSchema::Number),
            expr: header_cell(helper_read, param_carrier, layout::HDR_RAW_FREE_HEAD),
            line,
        },
        Stmt::Let {
            index: free,
            declared_schema: Some(TypeSchema::Int),
            expr: decode_int_call(decode, Expr::Var(free_num)),
            line,
        },
    ];
    for (offset, payload) in [
        (layout::HDR_ROOT_TOP, Lowerer::as_number(Expr::Var(prev))),
        (
            layout::HDR_ACTIVE_ROOT_CAPACITY,
            Lowerer::as_number(Expr::Sub(
                Box::new(Expr::Var(total)),
                Box::new(Expr::Var(cap)),
            )),
        ),
    ] {
        body.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: sink,
            expr: write_call(helper_write, param_carrier, Expr::Int(offset), payload),
            line,
        });
    }
    // Return the raw block to the free list; sweep coalescing merges it
    // with adjacent free blocks later.
    for (offset, payload) in [
        (
            layout::RAW_CLASS,
            Lowerer::as_number(Expr::Int(layout::RAW_CLASS_FREE)),
        ),
        (layout::RAW_FREE_NEXT, Lowerer::as_number(Expr::Var(free))),
    ] {
        let (cell, value) = (offset, payload);
        body.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: sink,
            expr: write_call(
                helper_write,
                param_carrier,
                Expr::Add(Box::new(Expr::Var(param_frame)), Box::new(Expr::Int(cell))),
                value,
            ),
            line,
        });
    }

    // Publish the returned block in the free chain immediately. A source
    // function returning a primitive may allocate no nodes at all, so no
    // future node-triggered collection can be assumed to discover it.
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: sink,
        expr: write_call(
            helper_write,
            param_carrier,
            Expr::Int(layout::HDR_RAW_FREE_HEAD),
            Lowerer::as_number(Expr::Var(param_frame)),
        ),
        line,
    });

    declare_helper(
        ir,
        statements,
        layout::ROOT_LEAVE_HELPER,
        index,
        2,
        vec!["carrier".to_owned(), "frame".to_owned()],
        vec![Some(carrier_schema.clone()), Some(TypeSchema::Int)],
        Some(TypeSchema::Int),
        ValueType::Int,
        vec![param_carrier, param_frame],
        body,
        Expr::Int(1),
        line,
    );
    Ok(index)
}

/// Append the checked dereference helper
/// `(Carrier, slot: Int, generation: Int) -> Int`: validates the slot bound,
/// page, active state and current generation, then the raw block class and
/// data bounds plus the stored header generation, and returns the physical
/// node base. Stale, freed, retired and wrong-generation references fail
/// closed before any payload is addressed.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_checked_base(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &TypeSchema,
    helper_read: u16,
    decode: u16,
    decode_u32: u16,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let index = fresh_index(lower, ir, span)?;
    let line = lower.line(span);
    let param_carrier = lower.slot(span)?;
    let param_slot = lower.slot(span)?;
    let param_gen = lower.slot(span)?;
    let next_num = lower.slot(span)?;
    let next = lower.slot(span)?;
    let page = lower.slot(span)?;
    let row = lower.slot(span)?;
    let addr = lower.slot(span)?;
    let page_ptr_num = lower.slot(span)?;
    let page_ptr = lower.slot(span)?;
    let field_num = lower.slot(span)?;
    let state = lower.slot(span)?;
    let stored_gen = lower.slot(span)?;
    let base = lower.slot(span)?;
    let cells = lower.slot(span)?;
    let end_num = lower.slot(span)?;
    let end = lower.slot(span)?;
    let block_num = lower.slot(span)?;
    let block_class = lower.slot(span)?;
    let block_size = lower.slot(span)?;
    let head_gen = lower.slot(span)?;
    let mut body = vec![
        Stmt::Let {
            index: next_num,
            declared_schema: Some(TypeSchema::Number),
            expr: header_cell(helper_read, param_carrier, layout::HDR_NEXT_SLOT),
            line,
        },
        Stmt::Let {
            index: next,
            declared_schema: Some(TypeSchema::Int),
            expr: decode_int_call(decode, Expr::Var(next_num)),
            line,
        },
        assert_stmt(
            Expr::And(
                Box::new(Expr::Not(Box::new(Expr::Lt(
                    Box::new(Expr::Var(param_slot)),
                    Box::new(Expr::Int(0)),
                )))),
                Box::new(Expr::Lt(
                    Box::new(Expr::Var(param_slot)),
                    Box::new(Expr::Var(next)),
                )),
            ),
            line,
        ),
    ];
    emit_row_lookup(
        helper_read,
        decode,
        param_carrier,
        param_slot,
        line,
        &mut body,
        true,
        page,
        row,
        addr,
        page_ptr_num,
        page_ptr,
        true,
    );
    // Row state must be active; the stored generation must equal the
    // presented one exactly (a free slot retains its old generation and a
    // reused slot increments it, so neither authenticates a stale box).
    body.push(Stmt::Let {
        index: field_num,
        declared_schema: Some(TypeSchema::Number),
        expr: read_cell(
            helper_read,
            param_carrier,
            Expr::Add(
                Box::new(Expr::Var(addr)),
                Box::new(Expr::Int(layout::ROW_STATE)),
            ),
        ),
        line,
    });
    body.push(Stmt::Let {
        index: state,
        declared_schema: Some(TypeSchema::Int),
        expr: decode_int_call(decode, Expr::Var(field_num)),
        line,
    });
    body.push(assert_stmt(
        Expr::Eq(
            Box::new(Expr::Var(state)),
            Box::new(Expr::Int(layout::STATE_ACTIVE)),
        ),
        line,
    ));
    body.push(Stmt::Let {
        index: field_num,
        declared_schema: Some(TypeSchema::Number),
        expr: read_cell(
            helper_read,
            param_carrier,
            Expr::Add(
                Box::new(Expr::Var(addr)),
                Box::new(Expr::Int(layout::ROW_GENERATION)),
            ),
        ),
        line,
    });
    body.push(Stmt::Let {
        index: stored_gen,
        declared_schema: Some(TypeSchema::Int),
        expr: decode_gen_call(decode_u32, Expr::Var(field_num)),
        line,
    });
    body.push(assert_stmt(
        Expr::Eq(
            Box::new(Expr::Var(stored_gen)),
            Box::new(Expr::Var(param_gen)),
        ),
        line,
    ));
    body.push(Stmt::Let {
        index: field_num,
        declared_schema: Some(TypeSchema::Number),
        expr: read_cell(
            helper_read,
            param_carrier,
            Expr::Add(
                Box::new(Expr::Var(addr)),
                Box::new(Expr::Int(layout::ROW_PHYSICAL_BASE)),
            ),
        ),
        line,
    });
    body.push(Stmt::Let {
        index: base,
        declared_schema: Some(TypeSchema::Int),
        expr: decode_int_call(decode, Expr::Var(field_num)),
        line,
    });
    body.push(Stmt::Let {
        index: field_num,
        declared_schema: Some(TypeSchema::Number),
        expr: read_cell(
            helper_read,
            param_carrier,
            Expr::Add(
                Box::new(Expr::Var(addr)),
                Box::new(Expr::Int(layout::ROW_DATA_CELLS)),
            ),
        ),
        line,
    });
    body.push(Stmt::Let {
        index: cells,
        declared_schema: Some(TypeSchema::Int),
        expr: decode_int_call(decode, Expr::Var(field_num)),
        line,
    });
    // Raw block validation: the extent must start after the fixed prefix,
    // carry the node class with the exact charged size, stay inside the
    // heap end, and store the same header generation.
    body.push(Stmt::Let {
        index: end_num,
        declared_schema: Some(TypeSchema::Number),
        expr: header_cell(helper_read, param_carrier, layout::HDR_HEAP_END),
        line,
    });
    body.push(Stmt::Let {
        index: end,
        declared_schema: Some(TypeSchema::Int),
        expr: decode_int_call(decode, Expr::Var(end_num)),
        line,
    });
    body.push(assert_stmt(
        Expr::And(
            Box::new(Expr::Or(
                Box::new(Expr::Gt(
                    Box::new(Expr::Var(base)),
                    Box::new(Expr::Int(layout::PREFIX_CELLS)),
                )),
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(base)),
                    Box::new(Expr::Int(layout::PREFIX_CELLS + layout::RAW_HEADER_CELLS)),
                )),
            )),
            Box::new(Expr::Or(
                Box::new(Expr::Lt(
                    Box::new(Expr::Add(
                        Box::new(Expr::Var(base)),
                        Box::new(Expr::Var(cells)),
                    )),
                    Box::new(Expr::Var(end)),
                )),
                Box::new(Expr::Eq(
                    Box::new(Expr::Add(
                        Box::new(Expr::Var(base)),
                        Box::new(Expr::Var(cells)),
                    )),
                    Box::new(Expr::Var(end)),
                )),
            )),
        ),
        line,
    ));
    body.push(Stmt::Let {
        index: block_num,
        declared_schema: Some(TypeSchema::Number),
        expr: read_cell(
            helper_read,
            param_carrier,
            Expr::Add(
                Box::new(Expr::Var(base)),
                Box::new(Expr::Int(-layout::RAW_HEADER_CELLS)),
            ),
        ),
        line,
    });
    body.push(Stmt::Let {
        index: block_size,
        declared_schema: Some(TypeSchema::Int),
        expr: decode_int_call(decode, Expr::Var(block_num)),
        line,
    });
    body.push(assert_stmt(
        Expr::Eq(
            Box::new(Expr::Var(block_size)),
            Box::new(Expr::Add(
                Box::new(Expr::Var(cells)),
                Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
            )),
        ),
        line,
    ));
    body.push(Stmt::Let {
        index: block_num,
        declared_schema: Some(TypeSchema::Number),
        expr: read_cell(
            helper_read,
            param_carrier,
            Expr::Add(
                Box::new(Expr::Var(base)),
                Box::new(Expr::Int(-(layout::RAW_HEADER_CELLS - layout::RAW_CLASS))),
            ),
        ),
        line,
    });
    body.push(Stmt::Let {
        index: block_class,
        declared_schema: Some(TypeSchema::Int),
        expr: decode_int_call(decode, Expr::Var(block_num)),
        line,
    });
    body.push(assert_stmt(
        Expr::Eq(
            Box::new(Expr::Var(block_class)),
            Box::new(Expr::Int(layout::RAW_CLASS_NODE)),
        ),
        line,
    ));
    body.push(Stmt::Let {
        index: block_num,
        declared_schema: Some(TypeSchema::Number),
        expr: read_cell(
            helper_read,
            param_carrier,
            Expr::Add(Box::new(Expr::Var(base)), Box::new(Expr::Int(1))),
        ),
        line,
    });
    body.push(Stmt::Let {
        index: head_gen,
        declared_schema: Some(TypeSchema::Int),
        expr: decode_gen_call(decode_u32, Expr::Var(block_num)),
        line,
    });
    body.push(assert_stmt(
        Expr::Eq(
            Box::new(Expr::Var(head_gen)),
            Box::new(Expr::Var(param_gen)),
        ),
        line,
    ));
    declare_helper(
        ir,
        statements,
        layout::CHECKED_BASE_HELPER,
        index,
        3,
        vec![
            "carrier".to_owned(),
            "slot".to_owned(),
            "generation".to_owned(),
        ],
        vec![
            Some(carrier_schema.clone()),
            Some(TypeSchema::Int),
            Some(TypeSchema::Int),
        ],
        Some(TypeSchema::Int),
        ValueType::Int,
        vec![param_carrier, param_slot, param_gen],
        body,
        Expr::Var(base),
        line,
    );
    Ok(index)
}

/// Append the intrusive gray marker
/// `(Carrier, slot: Int, generation: Int, expect: Int) -> Int ack`.
/// Validates the edge exactly (bound, page, active state, current
/// generation, node class) and asserts the node kind matches `expect`
/// (0 any node, 2 string, 3 object-like array/object/function, 5 internal
/// environment). A stale edge is corruption and fails closed; a live unseen
/// node is marked and pushed on the gray worklist. Returns 1.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_mark(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &TypeSchema,
    helper_read: u16,
    helper_write: u16,
    decode: u16,
    decode_u32: u16,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let index = fresh_index(lower, ir, span)?;
    let line = lower.line(span);
    let param_carrier = lower.slot(span)?;
    let param_slot = lower.slot(span)?;
    let param_gen = lower.slot(span)?;
    let param_expect = lower.slot(span)?;
    let next_num = lower.slot(span)?;
    let next = lower.slot(span)?;
    let page = lower.slot(span)?;
    let row = lower.slot(span)?;
    let addr = lower.slot(span)?;
    let page_ptr_num = lower.slot(span)?;
    let page_ptr = lower.slot(span)?;
    let field_num = lower.slot(span)?;
    let state = lower.slot(span)?;
    let stored_gen = lower.slot(span)?;
    let base = lower.slot(span)?;
    let marked_num = lower.slot(span)?;
    let marked = lower.slot(span)?;
    let gray_num = lower.slot(span)?;
    let gray = lower.slot(span)?;
    let kind_num = lower.slot(span)?;
    let kind = lower.slot(span)?;
    let sink = lower.slot(span)?;
    let mut body = vec![
        Stmt::Let {
            index: sink,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: next_num,
            declared_schema: Some(TypeSchema::Number),
            expr: header_cell(helper_read, param_carrier, layout::HDR_NEXT_SLOT),
            line,
        },
        Stmt::Let {
            index: next,
            declared_schema: Some(TypeSchema::Int),
            expr: decode_int_call(decode, Expr::Var(next_num)),
            line,
        },
        assert_stmt(
            Expr::And(
                Box::new(Expr::Not(Box::new(Expr::Lt(
                    Box::new(Expr::Var(param_slot)),
                    Box::new(Expr::Int(0)),
                )))),
                Box::new(Expr::Lt(
                    Box::new(Expr::Var(param_slot)),
                    Box::new(Expr::Var(next)),
                )),
            ),
            line,
        ),
    ];
    emit_row_lookup(
        helper_read,
        decode,
        param_carrier,
        param_slot,
        line,
        &mut body,
        true,
        page,
        row,
        addr,
        page_ptr_num,
        page_ptr,
        true,
    );
    for (field, target, is_gen) in [
        (layout::ROW_STATE, state, false),
        (layout::ROW_GENERATION, stored_gen, true),
    ] {
        body.push(Stmt::Let {
            index: field_num,
            declared_schema: Some(TypeSchema::Number),
            expr: read_cell(
                helper_read,
                param_carrier,
                Expr::Add(Box::new(Expr::Var(addr)), Box::new(Expr::Int(field))),
            ),
            line,
        });
        body.push(Stmt::Let {
            index: target,
            declared_schema: Some(TypeSchema::Int),
            expr: if is_gen {
                decode_gen_call(decode_u32, Expr::Var(field_num))
            } else {
                decode_int_call(decode, Expr::Var(field_num))
            },
            line,
        });
    }
    body.push(assert_stmt(
        Expr::Eq(
            Box::new(Expr::Var(state)),
            Box::new(Expr::Int(layout::STATE_ACTIVE)),
        ),
        line,
    ));
    body.push(assert_stmt(
        Expr::Eq(
            Box::new(Expr::Var(stored_gen)),
            Box::new(Expr::Var(param_gen)),
        ),
        line,
    ));
    body.push(Stmt::Let {
        index: field_num,
        declared_schema: Some(TypeSchema::Number),
        expr: read_cell(
            helper_read,
            param_carrier,
            Expr::Add(
                Box::new(Expr::Var(addr)),
                Box::new(Expr::Int(layout::ROW_PHYSICAL_BASE)),
            ),
        ),
        line,
    });
    body.push(Stmt::Let {
        index: base,
        declared_schema: Some(TypeSchema::Int),
        expr: decode_int_call(decode, Expr::Var(field_num)),
        line,
    });
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
    // Kind expectation: any node skips; otherwise the stored kind must be
    // the string kind, one of the object-like kinds, or the environment
    // kind respectively.
    let is_string_kind = Expr::Eq(
        Box::new(Expr::Var(kind)),
        Box::new(Expr::Int(layout::KIND_STRING)),
    );
    let is_object_like = Expr::Or(
        Box::new(Expr::Eq(
            Box::new(Expr::Var(kind)),
            Box::new(Expr::Int(layout::KIND_ARRAY)),
        )),
        Box::new(Expr::Or(
            Box::new(Expr::Eq(
                Box::new(Expr::Var(kind)),
                Box::new(Expr::Int(layout::KIND_OBJECT)),
            )),
            Box::new(Expr::Eq(
                Box::new(Expr::Var(kind)),
                Box::new(Expr::Int(layout::KIND_FUNCTION)),
            )),
        )),
    );
    let is_env_kind = Expr::Eq(
        Box::new(Expr::Var(kind)),
        Box::new(Expr::Int(layout::KIND_ENVIRONMENT)),
    );
    let wanted_string = Expr::Eq(Box::new(Expr::Var(param_expect)), Box::new(Expr::Int(2)));
    let wanted_object_like = Expr::Eq(Box::new(Expr::Var(param_expect)), Box::new(Expr::Int(3)));
    let wanted_env = Expr::Eq(Box::new(Expr::Var(param_expect)), Box::new(Expr::Int(5)));
    body.push(assert_stmt(
        Expr::Or(
            Box::new(Expr::Eq(
                Box::new(Expr::Var(param_expect)),
                Box::new(Expr::Int(0)),
            )),
            Box::new(Expr::Or(
                Box::new(Expr::And(Box::new(wanted_string), Box::new(is_string_kind))),
                Box::new(Expr::Or(
                    Box::new(Expr::And(
                        Box::new(wanted_object_like),
                        Box::new(is_object_like),
                    )),
                    Box::new(Expr::And(Box::new(wanted_env), Box::new(is_env_kind))),
                )),
            )),
        ),
        line,
    ));
    body.push(Stmt::Let {
        index: marked_num,
        declared_schema: Some(TypeSchema::Number),
        expr: read_cell(
            helper_read,
            param_carrier,
            Expr::Add(
                Box::new(Expr::Var(addr)),
                Box::new(Expr::Int(layout::ROW_MARK)),
            ),
        ),
        line,
    });
    body.push(Stmt::Let {
        index: marked,
        declared_schema: Some(TypeSchema::Int),
        expr: decode_int_call(decode, Expr::Var(marked_num)),
        line,
    });
    body.push(Stmt::Let {
        index: gray_num,
        declared_schema: Some(TypeSchema::Number),
        expr: header_cell(helper_read, param_carrier, layout::HDR_GRAY_HEAD_PLUS_ONE),
        line,
    });
    body.push(Stmt::Let {
        index: gray,
        declared_schema: Some(TypeSchema::Int),
        expr: decode_int_call(decode, Expr::Var(gray_num)),
        line,
    });
    // Previously unseen live nodes join the intrusive gray worklist with a
    // slot+1 link (0 stays the empty link).
    body.push(Stmt::IfElse {
        condition: Expr::Eq(Box::new(Expr::Var(marked)), Box::new(Expr::Int(0))),
        then_branch: vec![
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: sink,
                expr: write_call(
                    helper_write,
                    param_carrier,
                    Expr::Add(
                        Box::new(Expr::Var(addr)),
                        Box::new(Expr::Int(layout::ROW_MARK)),
                    ),
                    Lowerer::as_number(Expr::Int(1)),
                ),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: sink,
                expr: write_call(
                    helper_write,
                    param_carrier,
                    Expr::Add(
                        Box::new(Expr::Var(addr)),
                        Box::new(Expr::Int(layout::ROW_GRAY_NEXT_PLUS_ONE)),
                    ),
                    Lowerer::as_number(Expr::Var(gray)),
                ),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: sink,
                expr: write_call(
                    helper_write,
                    param_carrier,
                    Expr::Int(layout::HDR_GRAY_HEAD_PLUS_ONE),
                    Lowerer::as_number(Expr::Add(
                        Box::new(Expr::Var(param_slot)),
                        Box::new(Expr::Int(1)),
                    )),
                ),
                line,
            },
        ],
        else_branch: Vec::new(),
        line,
    });

    declare_helper(
        ir,
        statements,
        layout::MARK_HELPER,
        index,
        4,
        vec![
            "carrier".to_owned(),
            "slot".to_owned(),
            "generation".to_owned(),
            "expect".to_owned(),
        ],
        vec![
            Some(carrier_schema.clone()),
            Some(TypeSchema::Int),
            Some(TypeSchema::Int),
            Some(TypeSchema::Int),
        ],
        Some(TypeSchema::Int),
        ValueType::Int,
        vec![param_carrier, param_slot, param_gen, param_expect],
        body,
        Expr::Int(1),
        line,
    );
    Ok(index)
}

/// Declare (`Let`) or update (`Assign`) one local slot. Every slot is
/// declared exactly once; repeated scans reuse the same slots through
/// updates so native loop edges stay balanced.
fn bind_slot(index: u16, schema: Option<TypeSchema>, expr: Expr, line: u32, declare: bool) -> Stmt {
    if declare {
        Stmt::Let {
            index,
            declared_schema: schema,
            expr,
            line,
        }
    } else {
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index,
            expr,
            line,
        }
    }
}

/// First-fit free-list scan for an `need`-cell block: sets `found` to the
/// block address (or -1), with its successor and predecessor links. Corrupt
/// headers (short blocks, overruns, non-free class) fail closed. Every scan
/// temp is declared once by the caller; the loop body only updates, so no
/// slot is ever read on a path where its declaration did not execute.
#[allow(clippy::too_many_arguments)]
fn emit_raw_search(
    helper_read: u16,
    decode: u16,
    param_carrier: u16,
    need: u16,
    line: u32,
    body: &mut Vec<Stmt>,
    head_num: u16,
    addr: u16,
    prev: u16,
    found: u16,
    found_next: u16,
    found_prev: u16,
    size_num: u16,
    size: u16,
    class_num: u16,
    class: u16,
    next_num: u16,
    next: u16,
    end: u16,
) {
    body.push(bind_slot(
        head_num,
        Some(TypeSchema::Number),
        header_cell(helper_read, param_carrier, layout::HDR_RAW_FREE_HEAD),
        line,
        false,
    ));
    body.push(bind_slot(
        addr,
        Some(TypeSchema::Int),
        decode_int_call(decode, Expr::Var(head_num)),
        line,
        false,
    ));
    body.push(bind_slot(
        prev,
        Some(TypeSchema::Int),
        Expr::Int(-1),
        line,
        false,
    ));
    body.push(bind_slot(
        found,
        Some(TypeSchema::Int),
        Expr::Int(-1),
        line,
        false,
    ));
    body.push(Stmt::While {
        condition: Expr::And(
            Box::new(Expr::Not(Box::new(Expr::Eq(
                Box::new(Expr::Var(addr)),
                Box::new(Expr::Int(0)),
            )))),
            Box::new(Expr::Eq(
                Box::new(Expr::Var(found)),
                Box::new(Expr::Int(-1)),
            )),
        ),
        body: vec![
            bind_slot(
                size_num,
                Some(TypeSchema::Number),
                read_cell(helper_read, param_carrier, Expr::Var(addr)),
                line,
                false,
            ),
            bind_slot(
                size,
                Some(TypeSchema::Int),
                decode_int_call(decode, Expr::Var(size_num)),
                line,
                false,
            ),
            assert_stmt(
                Expr::Or(
                    Box::new(Expr::Gt(
                        Box::new(Expr::Var(size)),
                        Box::new(Expr::Int(layout::MIN_RAW_BLOCK_CELLS)),
                    )),
                    Box::new(Expr::Eq(
                        Box::new(Expr::Var(size)),
                        Box::new(Expr::Int(layout::MIN_RAW_BLOCK_CELLS)),
                    )),
                ),
                line,
            ),
            assert_stmt(
                Expr::Or(
                    Box::new(Expr::Lt(
                        Box::new(Expr::Add(
                            Box::new(Expr::Var(addr)),
                            Box::new(Expr::Var(size)),
                        )),
                        Box::new(Expr::Var(end)),
                    )),
                    Box::new(Expr::Eq(
                        Box::new(Expr::Add(
                            Box::new(Expr::Var(addr)),
                            Box::new(Expr::Var(size)),
                        )),
                        Box::new(Expr::Var(end)),
                    )),
                ),
                line,
            ),
            bind_slot(
                class_num,
                Some(TypeSchema::Number),
                read_cell(
                    helper_read,
                    param_carrier,
                    Expr::Add(
                        Box::new(Expr::Var(addr)),
                        Box::new(Expr::Int(layout::RAW_CLASS)),
                    ),
                ),
                line,
                false,
            ),
            bind_slot(
                class,
                Some(TypeSchema::Int),
                decode_int_call(decode, Expr::Var(class_num)),
                line,
                false,
            ),
            assert_stmt(
                Expr::Eq(
                    Box::new(Expr::Var(class)),
                    Box::new(Expr::Int(layout::RAW_CLASS_FREE)),
                ),
                line,
            ),
            bind_slot(
                next_num,
                Some(TypeSchema::Number),
                read_cell(
                    helper_read,
                    param_carrier,
                    Expr::Add(
                        Box::new(Expr::Var(addr)),
                        Box::new(Expr::Int(layout::RAW_FREE_NEXT)),
                    ),
                ),
                line,
                false,
            ),
            bind_slot(
                next,
                Some(TypeSchema::Int),
                decode_int_call(decode, Expr::Var(next_num)),
                line,
                false,
            ),
            Stmt::IfElse {
                condition: Expr::Or(
                    Box::new(Expr::Gt(
                        Box::new(Expr::Var(size)),
                        Box::new(Expr::Var(need)),
                    )),
                    Box::new(Expr::Eq(
                        Box::new(Expr::Var(size)),
                        Box::new(Expr::Var(need)),
                    )),
                ),
                then_branch: vec![
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: found,
                        expr: Expr::Var(addr),
                        line,
                    },
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: found_next,
                        expr: Expr::Var(next),
                        line,
                    },
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: found_prev,
                        expr: Expr::Var(prev),
                        line,
                    },
                ],
                else_branch: vec![
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: prev,
                        expr: Expr::Var(addr),
                        line,
                    },
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: addr,
                        expr: Expr::Var(next),
                        line,
                    },
                ],
                line,
            },
        ],
        line,
    });
}

/// Append the raw reservation helper
/// `(Carrier, need: Int, class: Int) -> Int`: first-fit free extent with
/// splitting, otherwise one transactional backing growth. On pressure (no
/// fit while growth would breach the ceiling) one collection runs before
/// the final reservation attempt; a remaining breach is a profile resource
/// error. Returns the data address (block + 3); the header is written with
/// the requested class before return.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_raw_reserve(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &TypeSchema,
    helper_read: u16,
    helper_write: u16,
    helper_resize: u16,
    decode: u16,
    collect: u16,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let index = fresh_index(lower, ir, span)?;
    let line = lower.line(span);
    let param_carrier = lower.slot(span)?;
    let param_need = lower.slot(span)?;
    let param_class = lower.slot(span)?;
    let end_num = lower.slot(span)?;
    let end = lower.slot(span)?;
    let head_num = lower.slot(span)?;
    let addr = lower.slot(span)?;
    let prev = lower.slot(span)?;
    let found = lower.slot(span)?;
    let found_next = lower.slot(span)?;
    let found_prev = lower.slot(span)?;
    let size_num = lower.slot(span)?;
    let size = lower.slot(span)?;
    let class_num = lower.slot(span)?;
    let class = lower.slot(span)?;
    let next_num = lower.slot(span)?;
    let next = lower.slot(span)?;
    let data = lower.slot(span)?;
    let sink = lower.slot(span)?;
    let mut body = vec![
        Stmt::Let {
            index: sink,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        assert_stmt(
            Expr::Or(
                Box::new(Expr::Gt(
                    Box::new(Expr::Var(param_need)),
                    Box::new(Expr::Int(layout::MIN_RAW_BLOCK_CELLS)),
                )),
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(param_need)),
                    Box::new(Expr::Int(layout::MIN_RAW_BLOCK_CELLS)),
                )),
            ),
            line,
        ),
        assert_stmt(
            Expr::And(
                Box::new(Expr::Or(
                    Box::new(Expr::Gt(
                        Box::new(Expr::Var(param_class)),
                        Box::new(Expr::Int(layout::RAW_CLASS_FREE)),
                    )),
                    Box::new(Expr::Eq(
                        Box::new(Expr::Var(param_class)),
                        Box::new(Expr::Int(layout::RAW_CLASS_NODE)),
                    )),
                )),
                Box::new(Expr::Or(
                    Box::new(Expr::Lt(
                        Box::new(Expr::Var(param_class)),
                        Box::new(Expr::Int(layout::RAW_CLASS_ROOT_FRAME)),
                    )),
                    Box::new(Expr::Eq(
                        Box::new(Expr::Var(param_class)),
                        Box::new(Expr::Int(layout::RAW_CLASS_ROOT_FRAME)),
                    )),
                )),
            ),
            line,
        ),
        Stmt::Let {
            index: end_num,
            declared_schema: Some(TypeSchema::Number),
            expr: header_cell(helper_read, param_carrier, layout::HDR_HEAP_END),
            line,
        },
        Stmt::Let {
            index: end,
            declared_schema: Some(TypeSchema::Int),
            expr: decode_int_call(decode, Expr::Var(end_num)),
            line,
        },
        // Link temps are only assigned on a fit; pre-declare them once here.
        Stmt::Let {
            index: found_next,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: found_prev,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        // Scan temps are declared once up front; both the initial scan and
        // the pressure retry only update them, and the take branch reads
        // them on paths where the scan provably ran.
        Stmt::Let {
            index: head_num,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: addr,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: prev,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: found,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: size_num,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: size,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: class_num,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: class,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: next_num,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: next,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
    ];
    #[allow(clippy::too_many_arguments)]
    let search = |body: &mut Vec<Stmt>| {
        emit_raw_search(
            helper_read,
            decode,
            param_carrier,
            param_need,
            line,
            body,
            head_num,
            addr,
            prev,
            found,
            found_next,
            found_prev,
            size_num,
            size,
            class_num,
            class,
            next_num,
            next,
            end,
        );
    };
    search(&mut body);
    // Pressure path: no fit while growth would breach the ceiling collects
    // once (pending is zero at every reserve call site) before the final
    // reservation attempt.
    body.push(Stmt::IfElse {
        condition: Expr::And(
            Box::new(Expr::Eq(
                Box::new(Expr::Var(found)),
                Box::new(Expr::Int(-1)),
            )),
            Box::new(Expr::Not(Box::new(Expr::Eq(
                Box::new(Expr::Var(param_class)),
                Box::new(Expr::Int(layout::RAW_CLASS_NODE)),
            )))),
        ),
        then_branch: vec![Stmt::IfElse {
            condition: Expr::Gt(
                Box::new(Expr::Add(
                    Box::new(Expr::Var(end)),
                    Box::new(Expr::Var(param_need)),
                )),
                Box::new(Expr::Int(layout::PHYSICAL_CEILING)),
            ),
            then_branch: {
                let mut retry = vec![Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: sink,
                    expr: Lowerer::as_number(Lowerer::guest_call(
                        collect,
                        vec![Expr::Var(param_carrier)],
                    )),
                    line,
                }];
                search(&mut retry);
                retry
            },
            else_branch: Vec::new(),
            line,
        }],
        else_branch: Vec::new(),
        line,
    });
    // Take a fit (splitting a valid nonempty remainder) or grow once.
    body.push(Stmt::IfElse {
        condition: Expr::Not(Box::new(Expr::Eq(
            Box::new(Expr::Var(found)),
            Box::new(Expr::Int(-1)),
        ))),
        then_branch: {
            // The scan leaves the taken block's size in place; splitting
            // and unlinking only rearrange validated links.
            let take = vec![
                Stmt::IfElse {
                    condition: Expr::Or(
                        Box::new(Expr::Gt(
                            Box::new(Expr::Sub(
                                Box::new(Expr::Var(size)),
                                Box::new(Expr::Var(param_need)),
                            )),
                            Box::new(Expr::Int(layout::MIN_RAW_BLOCK_CELLS)),
                        )),
                        Box::new(Expr::Eq(
                            Box::new(Expr::Sub(
                                Box::new(Expr::Var(size)),
                                Box::new(Expr::Var(param_need)),
                            )),
                            Box::new(Expr::Int(layout::MIN_RAW_BLOCK_CELLS)),
                        )),
                    ),
                    then_branch: vec![
                        Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: sink,
                            expr: write_call(
                                helper_write,
                                param_carrier,
                                Expr::Add(
                                    Box::new(Expr::Var(found)),
                                    Box::new(Expr::Var(param_need)),
                                ),
                                Lowerer::as_number(Expr::Sub(
                                    Box::new(Expr::Var(size)),
                                    Box::new(Expr::Var(param_need)),
                                )),
                            ),
                            line,
                        },
                        Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: sink,
                            expr: write_call(
                                helper_write,
                                param_carrier,
                                Expr::Add(
                                    Box::new(Expr::Add(
                                        Box::new(Expr::Var(found)),
                                        Box::new(Expr::Var(param_need)),
                                    )),
                                    Box::new(Expr::Int(layout::RAW_CLASS)),
                                ),
                                Lowerer::as_number(Expr::Int(layout::RAW_CLASS_FREE)),
                            ),
                            line,
                        },
                        Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: sink,
                            expr: write_call(
                                helper_write,
                                param_carrier,
                                Expr::Add(
                                    Box::new(Expr::Add(
                                        Box::new(Expr::Var(found)),
                                        Box::new(Expr::Var(param_need)),
                                    )),
                                    Box::new(Expr::Int(layout::RAW_FREE_NEXT)),
                                ),
                                Lowerer::as_number(Expr::Var(found_next)),
                            ),
                            line,
                        },
                        Stmt::IfElse {
                            condition: Expr::Eq(
                                Box::new(Expr::Var(found_prev)),
                                Box::new(Expr::Int(-1)),
                            ),
                            then_branch: vec![Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: sink,
                                expr: write_call(
                                    helper_write,
                                    param_carrier,
                                    Expr::Int(layout::HDR_RAW_FREE_HEAD),
                                    Lowerer::as_number(Expr::Add(
                                        Box::new(Expr::Var(found)),
                                        Box::new(Expr::Var(param_need)),
                                    )),
                                ),
                                line,
                            }],
                            else_branch: vec![Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: sink,
                                expr: write_call(
                                    helper_write,
                                    param_carrier,
                                    Expr::Add(
                                        Box::new(Expr::Var(found_prev)),
                                        Box::new(Expr::Int(layout::RAW_FREE_NEXT)),
                                    ),
                                    Lowerer::as_number(Expr::Add(
                                        Box::new(Expr::Var(found)),
                                        Box::new(Expr::Var(param_need)),
                                    )),
                                ),
                                line,
                            }],
                            line,
                        },
                    ],
                    else_branch: vec![Stmt::IfElse {
                        condition: Expr::Eq(
                            Box::new(Expr::Var(found_prev)),
                            Box::new(Expr::Int(-1)),
                        ),
                        then_branch: vec![Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: sink,
                            expr: write_call(
                                helper_write,
                                param_carrier,
                                Expr::Int(layout::HDR_RAW_FREE_HEAD),
                                Lowerer::as_number(Expr::Var(found_next)),
                            ),
                            line,
                        }],
                        else_branch: vec![Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: sink,
                            expr: write_call(
                                helper_write,
                                param_carrier,
                                Expr::Add(
                                    Box::new(Expr::Var(found_prev)),
                                    Box::new(Expr::Int(layout::RAW_FREE_NEXT)),
                                ),
                                Lowerer::as_number(Expr::Var(found_next)),
                            ),
                            line,
                        }],
                        line,
                    }],
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: sink,
                    expr: write_call(
                        helper_write,
                        param_carrier,
                        Expr::Var(found),
                        // An unsplittable tail remains part of this extent;
                        // shrinking its header would leave unowned raw cells.
                        Lowerer::as_number(Expr::IfElse {
                            condition: Box::new(Expr::Lt(
                                Box::new(Expr::Sub(
                                    Box::new(Expr::Var(size)),
                                    Box::new(Expr::Var(param_need)),
                                )),
                                Box::new(Expr::Int(layout::MIN_RAW_BLOCK_CELLS)),
                            )),
                            then_expr: Box::new(Expr::Var(size)),
                            else_expr: Box::new(Expr::Var(param_need)),
                        }),
                    ),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: sink,
                    expr: write_call(
                        helper_write,
                        param_carrier,
                        Expr::Add(
                            Box::new(Expr::Var(found)),
                            Box::new(Expr::Int(layout::RAW_CLASS)),
                        ),
                        Lowerer::as_number(Expr::Var(param_class)),
                    ),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: sink,
                    expr: write_call(
                        helper_write,
                        param_carrier,
                        Expr::Add(
                            Box::new(Expr::Var(found)),
                            Box::new(Expr::Int(layout::RAW_FREE_NEXT)),
                        ),
                        Lowerer::as_number(Expr::Int(0)),
                    ),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: data,
                    expr: Expr::Add(
                        Box::new(Expr::Var(found)),
                        Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
                    ),
                    line,
                },
            ];
            take
        },
        else_branch: {
            let grown = vec![
                assert_stmt(
                    Expr::Or(
                        Box::new(Expr::Lt(
                            Box::new(Expr::Add(
                                Box::new(Expr::Var(end)),
                                Box::new(Expr::Var(param_need)),
                            )),
                            Box::new(Expr::Int(layout::PHYSICAL_CEILING)),
                        )),
                        Box::new(Expr::Eq(
                            Box::new(Expr::Add(
                                Box::new(Expr::Var(end)),
                                Box::new(Expr::Var(param_need)),
                            )),
                            Box::new(Expr::Int(layout::PHYSICAL_CEILING)),
                        )),
                    ),
                    line,
                ),
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: sink,
                    expr: Lowerer::guest_call(
                        helper_resize,
                        vec![
                            Expr::Var(param_carrier),
                            Lowerer::as_number(Expr::Add(
                                Box::new(Expr::Var(end)),
                                Box::new(Expr::Var(param_need)),
                            )),
                        ],
                    ),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: sink,
                    expr: write_call(
                        helper_write,
                        param_carrier,
                        Expr::Var(end),
                        Lowerer::as_number(Expr::Var(param_need)),
                    ),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: sink,
                    expr: write_call(
                        helper_write,
                        param_carrier,
                        Expr::Add(
                            Box::new(Expr::Var(end)),
                            Box::new(Expr::Int(layout::RAW_CLASS)),
                        ),
                        Lowerer::as_number(Expr::Var(param_class)),
                    ),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: sink,
                    expr: write_call(
                        helper_write,
                        param_carrier,
                        Expr::Add(
                            Box::new(Expr::Var(end)),
                            Box::new(Expr::Int(layout::RAW_FREE_NEXT)),
                        ),
                        Lowerer::as_number(Expr::Int(0)),
                    ),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: sink,
                    expr: write_call(
                        helper_write,
                        param_carrier,
                        Expr::Int(layout::HDR_HEAP_END),
                        Lowerer::as_number(Expr::Add(
                            Box::new(Expr::Var(end)),
                            Box::new(Expr::Var(param_need)),
                        )),
                    ),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: data,
                    expr: Expr::Add(
                        Box::new(Expr::Var(end)),
                        Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
                    ),
                    line,
                },
            ];
            grown
        },
        line,
    });
    // `data` is bound on every path above; pre-declare it here.
    body.insert(
        1,
        Stmt::Let {
            index: data,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
    );

    declare_helper(
        ir,
        statements,
        layout::RAW_RESERVE_HELPER,
        index,
        3,
        vec!["carrier".to_owned(), "need".to_owned(), "class".to_owned()],
        vec![
            Some(carrier_schema.clone()),
            Some(TypeSchema::Int),
            Some(TypeSchema::Int),
        ],
        Some(TypeSchema::Int),
        ValueType::Int,
        vec![param_carrier, param_need, param_class],
        body,
        Expr::Var(data),
        line,
    );
    Ok(index)
}

/// Append the node reservation helper
/// `(Carrier, count: Int, kind: Int) -> __CompatEs5Allocation`.
/// Fails closed unless no node is pending. Runs the interval collection,
/// reuses a free slot (incrementing its retained generation) or claims the
/// next slot, preflights combined page-plus-data capacity before touching
/// row/free-list/live accounting, reserves one transactional extent, charges
/// the pending guard plus the exact occupied cells, writes kind/generation
/// and returns the internal handle. Never exposes a JsValue.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_node_begin(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &TypeSchema,
    helper_read: u16,
    helper_write: u16,
    helper_resize: u16,
    decode: u16,
    decode_u32: u16,
    collect: u16,
    raw_reserve: u16,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let index = fresh_index(lower, ir, span)?;
    let line = lower.line(span);
    let param_carrier = lower.slot(span)?;
    let param_count = lower.slot(span)?;
    let param_kind = lower.slot(span)?;
    let pending_num = lower.slot(span)?;
    let pending = lower.slot(span)?;
    let since_num = lower.slot(span)?;
    let since = lower.slot(span)?;
    let free_num = lower.slot(span)?;
    let free_head = lower.slot(span)?;
    let slot = lower.slot(span)?;
    let new_gen = lower.slot(span)?;
    let new_gen_num = lower.slot(span)?;
    let page = lower.slot(span)?;
    let row = lower.slot(span)?;
    let addr = lower.slot(span)?;
    let page_missing = lower.slot(span)?;
    let need = lower.slot(span)?;
    let data = lower.slot(span)?;
    let end_num = lower.slot(span)?;
    let end = lower.slot(span)?;
    let next_num = lower.slot(span)?;
    let next = lower.slot(span)?;
    let pages_num = lower.slot(span)?;
    let pages = lower.slot(span)?;
    let cursor = lower.slot(span)?;
    let live_num = lower.slot(span)?;
    let live_cells = lower.slot(span)?;
    let page_r = lower.slot(span)?;
    let row_r = lower.slot(span)?;
    let addr_r = lower.slot(span)?;
    let page_ptr_r_num = lower.slot(span)?;
    let page_ptr_r = lower.slot(span)?;
    let gen_r = lower.slot(span)?;
    let link_r = lower.slot(span)?;
    let page_f = lower.slot(span)?;
    let row_f = lower.slot(span)?;
    let addr_f = lower.slot(span)?;
    let page_ptr_f_num = lower.slot(span)?;
    let page_ptr_f = lower.slot(span)?;
    let sink = lower.slot(span)?;
    let mut body = vec![
        Stmt::Let {
            index: sink,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        assert_stmt(
            Expr::Gt(Box::new(Expr::Var(param_count)), Box::new(Expr::Int(0))),
            line,
        ),
        assert_stmt(
            Expr::And(
                Box::new(Expr::Or(
                    Box::new(Expr::Gt(
                        Box::new(Expr::Var(param_kind)),
                        Box::new(Expr::Int(0)),
                    )),
                    Box::new(Expr::Eq(
                        Box::new(Expr::Var(param_kind)),
                        Box::new(Expr::Int(layout::KIND_ARRAY)),
                    )),
                )),
                Box::new(Expr::Or(
                    Box::new(Expr::Lt(
                        Box::new(Expr::Var(param_kind)),
                        Box::new(Expr::Int(layout::KIND_ENVIRONMENT)),
                    )),
                    Box::new(Expr::Eq(
                        Box::new(Expr::Var(param_kind)),
                        Box::new(Expr::Int(layout::KIND_ENVIRONMENT)),
                    )),
                )),
            ),
            line,
        ),
        Stmt::Let {
            index: pending_num,
            declared_schema: Some(TypeSchema::Number),
            expr: header_cell(helper_read, param_carrier, layout::HDR_PENDING_NODES),
            line,
        },
        Stmt::Let {
            index: pending,
            declared_schema: Some(TypeSchema::Int),
            expr: decode_int_call(decode, Expr::Var(pending_num)),
            line,
        },
        assert_stmt(
            Expr::Eq(Box::new(Expr::Var(pending)), Box::new(Expr::Int(0))),
            line,
        ),
        Stmt::Let {
            index: since_num,
            declared_schema: Some(TypeSchema::Number),
            expr: header_cell(helper_read, param_carrier, layout::HDR_ALLOCATIONS_SINCE_GC),
            line,
        },
        Stmt::Let {
            index: since,
            declared_schema: Some(TypeSchema::Int),
            expr: decode_int_call(decode, Expr::Var(since_num)),
            line,
        },
        Stmt::IfElse {
            condition: Expr::Or(
                Box::new(Expr::Gt(
                    Box::new(Expr::Var(since)),
                    Box::new(Expr::Int(layout::GC_INTERVAL_ALLOCATIONS)),
                )),
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(since)),
                    Box::new(Expr::Int(layout::GC_INTERVAL_ALLOCATIONS)),
                )),
            ),
            then_branch: vec![Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: sink,
                expr: Lowerer::as_number(Lowerer::guest_call(
                    collect,
                    vec![Expr::Var(param_carrier)],
                )),
                line,
            }],
            else_branch: Vec::new(),
            line,
        },
        // Conservative combined page+node pressure check. Run maintenance
        // before choosing a row: collection can rebuild free slot links.
        Stmt::IfElse {
            condition: Expr::Gt(
                Box::new(Expr::Add(
                    Box::new(decode_int_call(
                        decode,
                        header_cell(helper_read, param_carrier, layout::HDR_HEAP_END),
                    )),
                    Box::new(Expr::Add(
                        Box::new(Expr::Var(param_count)),
                        Box::new(Expr::Int(layout::RAW_HEADER_CELLS + layout::DIR_PAGE_CELLS)),
                    )),
                )),
                Box::new(Expr::Int(layout::PHYSICAL_CEILING)),
            ),
            then_branch: vec![Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: sink,
                expr: Lowerer::as_number(Lowerer::guest_call(
                    collect,
                    vec![Expr::Var(param_carrier)],
                )),
                line,
            }],
            else_branch: Vec::new(),
            line,
        },
        Stmt::Let {
            index: need,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Add(
                Box::new(Expr::Var(param_count)),
                Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
            ),
            line,
        },
        Stmt::Let {
            index: slot,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: new_gen,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: new_gen_num,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: page,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: row,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: addr,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: page_missing,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: data,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
        Stmt::Let {
            index: link_r,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: free_num,
            declared_schema: Some(TypeSchema::Number),
            expr: header_cell(
                helper_read,
                param_carrier,
                layout::HDR_FREE_SLOT_HEAD_PLUS_ONE,
            ),
            line,
        },
        Stmt::Let {
            index: free_head,
            declared_schema: Some(TypeSchema::Int),
            expr: decode_int_call(decode, Expr::Var(free_num)),
            line,
        },
    ];
    // Slot decision: pop the free list (retained generation + 1) or claim
    // the next slot with generation 1. Each arm uses disjoint lookup slots;
    // shared outcome slots are declared once above and updated by both arms.
    //
    // Reuse arm: slot = head - 1, strict row lookup, bumped generation
    // (fail closed at the maximum: such rows retire instead of entering
    // the free list, so reaching the bound here is corruption), pop link.
    let mut then_reuse = vec![Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: slot,
        expr: Expr::Sub(Box::new(Expr::Var(free_head)), Box::new(Expr::Int(1))),
        line,
    }];
    emit_row_lookup(
        helper_read,
        decode,
        param_carrier,
        slot,
        line,
        &mut then_reuse,
        true,
        page_r,
        row_r,
        addr_r,
        page_ptr_r_num,
        page_ptr_r,
        true,
    );
    then_reuse.push(assert_stmt(
        Expr::Eq(
            Box::new(decode_int_call(
                decode,
                read_cell(
                    helper_read,
                    param_carrier,
                    Expr::Add(
                        Box::new(Expr::Var(addr_r)),
                        Box::new(Expr::Int(layout::ROW_STATE)),
                    ),
                ),
            )),
            Box::new(Expr::Int(layout::STATE_FREE)),
        ),
        line,
    ));
    then_reuse.push(Stmt::Let {
        index: gen_r,
        declared_schema: Some(TypeSchema::Number),
        expr: read_cell(
            helper_read,
            param_carrier,
            Expr::Add(
                Box::new(Expr::Var(addr_r)),
                Box::new(Expr::Int(layout::ROW_GENERATION)),
            ),
        ),
        line,
    });
    // Validate the retained generation before incrementing it. Decoding
    // only old+1 would normalize a corrupt -1 into an apparent generation0.
    then_reuse.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: new_gen,
        expr: decode_gen_call(decode_u32, Expr::Var(gen_r)),
        line,
    });
    then_reuse.push(assert_stmt(
        Expr::And(
            Box::new(Expr::Gt(
                Box::new(Expr::Var(new_gen)),
                Box::new(Expr::Int(0)),
            )),
            Box::new(Expr::Lt(
                Box::new(Lowerer::as_number(Expr::Var(new_gen))),
                Box::new(Expr::Float(layout::MAX_GENERATION_F64)),
            )),
        ),
        line,
    ));
    then_reuse.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: new_gen_num,
        expr: Expr::Add(Box::new(Expr::Var(gen_r)), Box::new(Expr::Float(1.0))),
        line,
    });
    then_reuse.push(assert_stmt(
        Expr::Or(
            Box::new(Expr::Lt(
                Box::new(Expr::Var(new_gen_num)),
                Box::new(Expr::Float(layout::MAX_GENERATION_F64)),
            )),
            Box::new(Expr::Eq(
                Box::new(Expr::Var(new_gen_num)),
                Box::new(Expr::Float(layout::MAX_GENERATION_F64)),
            )),
        ),
        line,
    ));
    then_reuse.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: new_gen,
        expr: decode_gen_call(decode_u32, Expr::Var(new_gen_num)),
        line,
    });
    then_reuse.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: link_r,
        expr: read_cell(
            helper_read,
            param_carrier,
            Expr::Add(
                Box::new(Expr::Var(addr_r)),
                Box::new(Expr::Int(layout::ROW_FREE_NEXT_PLUS_ONE)),
            ),
        ),
        line,
    });
    for (target, source) in [(page, page_r), (row, row_r), (addr, addr_r)] {
        then_reuse.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: target,
            expr: Expr::Var(source),
            line,
        });
    }
    // Fresh arm: claim next_slot (directory bound enforced), generation 1,
    // soft page lookup to detect a missing page.
    let mut else_fresh = vec![
        Stmt::Let {
            index: next_num,
            declared_schema: Some(TypeSchema::Number),
            expr: header_cell(helper_read, param_carrier, layout::HDR_NEXT_SLOT),
            line,
        },
        Stmt::Let {
            index: next,
            declared_schema: Some(TypeSchema::Int),
            expr: decode_int_call(decode, Expr::Var(next_num)),
            line,
        },
        assert_stmt(
            Expr::Lt(
                Box::new(Expr::Var(next)),
                Box::new(Expr::Int(layout::MAX_SLOTS)),
            ),
            line,
        ),
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: slot,
            expr: Expr::Var(next),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: new_gen,
            expr: Expr::Int(1),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: new_gen_num,
            expr: Expr::Float(1.0),
            line,
        },
    ];
    emit_row_lookup(
        helper_read,
        decode,
        param_carrier,
        slot,
        line,
        &mut else_fresh,
        true,
        page_f,
        row_f,
        addr_f,
        page_ptr_f_num,
        page_ptr_f,
        false,
    );
    for (target, source) in [(page, page_f), (row, row_f), (addr, addr_f)] {
        else_fresh.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: target,
            expr: Expr::Var(source),
            line,
        });
    }
    else_fresh.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: page_missing,
        expr: Expr::IfElse {
            condition: Box::new(Expr::Eq(
                Box::new(Expr::Var(addr_f)),
                Box::new(Expr::Int(0)),
            )),
            then_expr: Box::new(Expr::Int(1)),
            else_expr: Box::new(Expr::Int(0)),
        },
        line,
    });
    body.push(Stmt::IfElse {
        condition: Expr::Not(Box::new(Expr::Eq(
            Box::new(Expr::Var(free_head)),
            Box::new(Expr::Int(0)),
        ))),
        then_branch: then_reuse,
        else_branch: else_fresh,
        line,
    });
    // Placement: a missing page takes the combined single-resize path for
    // page plus node (one transactional growth, preflighted before any
    // commit); otherwise the transactional raw reservation serves the node.
    // All outcome temps are declared once here and updated inside the arms.
    body.push(Stmt::Let {
        index: end_num,
        declared_schema: Some(TypeSchema::Number),
        expr: Expr::Float(0.0),
        line,
    });
    body.push(Stmt::Let {
        index: end,
        declared_schema: Some(TypeSchema::Int),
        expr: Expr::Int(0),
        line,
    });
    body.push(Stmt::Let {
        index: cursor,
        declared_schema: Some(TypeSchema::Int),
        expr: Expr::Int(0),
        line,
    });
    body.push(Stmt::Let {
        index: pages_num,
        declared_schema: Some(TypeSchema::Number),
        expr: Expr::Float(0.0),
        line,
    });
    body.push(Stmt::Let {
        index: pages,
        declared_schema: Some(TypeSchema::Int),
        expr: Expr::Int(0),
        line,
    });
    let mut then_page = vec![
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: end_num,
            expr: header_cell(helper_read, param_carrier, layout::HDR_HEAP_END),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: end,
            expr: decode_int_call(decode, Expr::Var(end_num)),
            line,
        },
        assert_stmt(
            Expr::Or(
                Box::new(Expr::Lt(
                    Box::new(Expr::Add(
                        Box::new(Expr::Add(
                            Box::new(Expr::Var(end)),
                            Box::new(Expr::Int(layout::DIR_PAGE_CELLS)),
                        )),
                        Box::new(Expr::Var(need)),
                    )),
                    Box::new(Expr::Int(layout::PHYSICAL_CEILING)),
                )),
                Box::new(Expr::Eq(
                    Box::new(Expr::Add(
                        Box::new(Expr::Add(
                            Box::new(Expr::Var(end)),
                            Box::new(Expr::Int(layout::DIR_PAGE_CELLS)),
                        )),
                        Box::new(Expr::Var(need)),
                    )),
                    Box::new(Expr::Int(layout::PHYSICAL_CEILING)),
                )),
            ),
            line,
        ),
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: sink,
            expr: Lowerer::guest_call(
                helper_resize,
                vec![
                    Expr::Var(param_carrier),
                    Lowerer::as_number(Expr::Add(
                        Box::new(Expr::Add(
                            Box::new(Expr::Var(end)),
                            Box::new(Expr::Int(layout::DIR_PAGE_CELLS)),
                        )),
                        Box::new(Expr::Var(need)),
                    )),
                ],
            ),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: sink,
            expr: write_call(
                helper_write,
                param_carrier,
                Expr::Int(layout::HDR_HEAP_END),
                Lowerer::as_number(Expr::Add(
                    Box::new(Expr::Add(
                        Box::new(Expr::Var(end)),
                        Box::new(Expr::Int(layout::DIR_PAGE_CELLS)),
                    )),
                    Box::new(Expr::Var(need)),
                )),
            ),
            line,
        },
        // Directory page header, then explicit zeroed rows: reused growth
        // fill is already zero, but the page contract never relies on it.
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: sink,
            expr: write_call(
                helper_write,
                param_carrier,
                Expr::Var(end),
                Lowerer::as_number(Expr::Int(layout::DIR_PAGE_CELLS)),
            ),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: sink,
            expr: write_call(
                helper_write,
                param_carrier,
                Expr::Add(
                    Box::new(Expr::Var(end)),
                    Box::new(Expr::Int(layout::RAW_CLASS)),
                ),
                Lowerer::as_number(Expr::Int(layout::RAW_CLASS_DIR_PAGE)),
            ),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: sink,
            expr: write_call(
                helper_write,
                param_carrier,
                Expr::Add(
                    Box::new(Expr::Var(end)),
                    Box::new(Expr::Int(layout::RAW_FREE_NEXT)),
                ),
                Lowerer::as_number(Expr::Int(0)),
            ),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: cursor,
            expr: Expr::Int(0),
            line,
        },
    ];
    then_page.push(Stmt::While {
        condition: Expr::Lt(
            Box::new(Lowerer::as_number(Expr::Var(cursor))),
            Box::new(Expr::Float((layout::PAGE_ROWS * layout::ROW_CELLS) as f64)),
        ),
        body: vec![
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: sink,
                expr: write_call(
                    helper_write,
                    param_carrier,
                    Expr::Add(
                        Box::new(Expr::Add(
                            Box::new(Expr::Var(end)),
                            Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
                        )),
                        Box::new(Expr::Var(cursor)),
                    ),
                    Lowerer::as_number(Expr::Int(0)),
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
    then_page.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: sink,
        expr: write_call(
            helper_write,
            param_carrier,
            Expr::Add(
                Box::new(Expr::Int(layout::HEADER_CELLS)),
                Box::new(Expr::Var(page)),
            ),
            Lowerer::as_number(Expr::Var(end)),
        ),
        line,
    });
    then_page.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: pages_num,
        expr: header_cell(helper_read, param_carrier, layout::HDR_DIRECTORY_PAGES),
        line,
    });
    then_page.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: pages,
        expr: decode_int_call(decode, Expr::Var(pages_num)),
        line,
    });
    then_page.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: sink,
        expr: write_call(
            helper_write,
            param_carrier,
            Expr::Int(layout::HDR_DIRECTORY_PAGES),
            Lowerer::as_number(Expr::Add(
                Box::new(Expr::Var(pages)),
                Box::new(Expr::Int(1)),
            )),
        ),
        line,
    });
    // Node header right after the page, then the data address.
    for (offset, marker) in [
        (0, Expr::Var(need)),
        (layout::RAW_CLASS, Expr::Int(layout::RAW_CLASS_NODE)),
        (layout::RAW_FREE_NEXT, Expr::Int(0)),
    ] {
        then_page.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: sink,
            expr: write_call(
                helper_write,
                param_carrier,
                Expr::Add(
                    Box::new(Expr::Add(
                        Box::new(Expr::Var(end)),
                        Box::new(Expr::Int(layout::DIR_PAGE_CELLS)),
                    )),
                    Box::new(Expr::Int(offset)),
                ),
                Lowerer::as_number(marker),
            ),
            line,
        });
    }
    then_page.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: data,
        expr: Expr::Add(
            Box::new(Expr::Add(
                Box::new(Expr::Var(end)),
                Box::new(Expr::Int(layout::DIR_PAGE_CELLS)),
            )),
            Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
        ),
        line,
    });
    // Row address of the fresh slot inside the new page.
    then_page.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: addr,
        expr: Expr::Add(
            Box::new(Expr::Add(
                Box::new(Expr::Var(end)),
                Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
            )),
            Box::new(Expr::Mul(
                Box::new(Expr::Var(row)),
                Box::new(Expr::Int(layout::ROW_CELLS)),
            )),
        ),
        line,
    });
    body.push(Stmt::IfElse {
        condition: Expr::Eq(Box::new(Expr::Var(page_missing)), Box::new(Expr::Int(1))),
        then_branch: then_page,
        else_branch: vec![Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: data,
            expr: Lowerer::guest_call(
                raw_reserve,
                vec![
                    Expr::Var(param_carrier),
                    Expr::Var(need),
                    Expr::Int(layout::RAW_CLASS_NODE),
                ],
            ),
            line,
        }],
        line,
    });
    // Charge the real occupied extent, including any unsplittable tail.
    // The row records its full physical data capacity, while the node's
    // initialized logical length continues to bound payload traversal.
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: need,
        expr: decode_int_call(
            decode,
            read_cell(
                helper_read,
                param_carrier,
                Expr::Sub(
                    Box::new(Expr::Var(data)),
                    Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
                ),
            ),
        ),
        line,
    });
    // Every fallible storage reservation has completed. Commit the logical
    // slot choice only now; quota refusal cannot pop a free row or advance
    // the high-water mark. No collector runs between selection and here.
    body.push(Stmt::IfElse {
        condition: Expr::Not(Box::new(Expr::Eq(
            Box::new(Expr::Var(free_head)),
            Box::new(Expr::Int(0)),
        ))),
        then_branch: vec![Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: sink,
            expr: write_call(
                helper_write,
                param_carrier,
                Expr::Int(layout::HDR_FREE_SLOT_HEAD_PLUS_ONE),
                Expr::Var(link_r),
            ),
            line,
        }],
        else_branch: vec![Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: sink,
            expr: write_call(
                helper_write,
                param_carrier,
                Expr::Int(layout::HDR_NEXT_SLOT),
                Lowerer::as_number(Expr::Add(Box::new(Expr::Var(slot)), Box::new(Expr::Int(1)))),
            ),
            line,
        }],
        line,
    });
    // Commit the pending row and charge the exact occupied extent, then
    // stamp kind/generation on the node data.
    body.push(Stmt::Let {
        index: live_num,
        declared_schema: Some(TypeSchema::Number),
        expr: header_cell(helper_read, param_carrier, layout::HDR_NODE_CELLS),
        line,
    });
    body.push(Stmt::Let {
        index: live_cells,
        declared_schema: Some(TypeSchema::Int),
        expr: decode_int_call(decode, Expr::Var(live_num)),
        line,
    });
    for (field, payload) in [
        (
            layout::ROW_STATE,
            Lowerer::as_number(Expr::Int(layout::STATE_PENDING)),
        ),
        (layout::ROW_GENERATION, Expr::Var(new_gen_num)),
        (
            layout::ROW_PHYSICAL_BASE,
            Lowerer::as_number(Expr::Var(data)),
        ),
        (
            layout::ROW_DATA_CELLS,
            Lowerer::as_number(Expr::Sub(
                Box::new(Expr::Var(need)),
                Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
            )),
        ),
        (layout::ROW_MARK, Lowerer::as_number(Expr::Int(0))),
        (
            layout::ROW_GRAY_NEXT_PLUS_ONE,
            Lowerer::as_number(Expr::Int(0)),
        ),
        (
            layout::ROW_FREE_NEXT_PLUS_ONE,
            Lowerer::as_number(Expr::Int(0)),
        ),
    ] {
        body.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: sink,
            expr: write_call(
                helper_write,
                param_carrier,
                Expr::Add(Box::new(Expr::Var(addr)), Box::new(Expr::Int(field))),
                payload,
            ),
            line,
        });
    }
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: sink,
        expr: write_call(
            helper_write,
            param_carrier,
            Expr::Int(layout::HDR_PENDING_NODES),
            Lowerer::as_number(Expr::Int(1)),
        ),
        line,
    });
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: sink,
        expr: write_call(
            helper_write,
            param_carrier,
            Expr::Int(layout::HDR_NODE_CELLS),
            Lowerer::as_number(Expr::Add(
                Box::new(Expr::Var(live_cells)),
                Box::new(Expr::Var(need)),
            )),
        ),
        line,
    });
    for (offset, payload) in [
        (0, Lowerer::as_number(Expr::Var(param_kind))),
        (1, Expr::Var(new_gen_num)),
    ] {
        body.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: sink,
            expr: write_call(
                helper_write,
                param_carrier,
                Expr::Add(Box::new(Expr::Var(data)), Box::new(Expr::Int(offset))),
                payload,
            ),
            line,
        });
    }
    let allocation_schema = value::allocation_schema();

    declare_helper(
        ir,
        statements,
        layout::NODE_BEGIN_HELPER,
        index,
        3,
        vec!["carrier".to_owned(), "count".to_owned(), "kind".to_owned()],
        vec![
            Some(carrier_schema.clone()),
            Some(TypeSchema::Int),
            Some(TypeSchema::Int),
        ],
        Some(allocation_schema),
        ValueType::Map,
        vec![param_carrier, param_count, param_kind],
        body,
        value::allocation(Expr::Var(slot), Expr::Var(new_gen), Expr::Var(data)),
        line,
    );
    Ok(index)
}

/// Shared publish validation: the pending guard is set, the row is pending
/// with the presented slot/generation/base, the node header carries a live
/// kind with the same generation, and the destination frame/entry is live.
/// Leaves the decoded kind in `kind`.
#[allow(clippy::too_many_arguments)]
fn emit_publish_preamble(
    lower: &mut Lowerer<'_>,
    helper_read: u16,
    helper_write: u16,
    decode: u16,
    decode_u32: u16,
    param_carrier: u16,
    param_slot: u16,
    param_gen: u16,
    param_base: u16,
    param_frame: u16,
    param_entry: u16,
    span: swc_common::Span,
    line: u32,
    body: &mut Vec<Stmt>,
    pending_num: u16,
    pending: u16,
    page: u16,
    row: u16,
    addr: u16,
    field_num: u16,
    state: u16,
    rowgen: u16,
    rowbase: u16,
    kind_num: u16,
    kind: u16,
    headgen: u16,
    class_num: u16,
    class: u16,
    cap_num: u16,
    cap: u16,
    end_num: u16,
    end: u16,
) -> Result<(), ParseError> {
    let page_ptr_num = lower.slot(span)?;
    let page_ptr = lower.slot(span)?;
    body.push(Stmt::Let {
        index: pending_num,
        declared_schema: Some(TypeSchema::Number),
        expr: header_cell(helper_read, param_carrier, layout::HDR_PENDING_NODES),
        line,
    });
    body.push(Stmt::Let {
        index: pending,
        declared_schema: Some(TypeSchema::Int),
        expr: decode_int_call(decode, Expr::Var(pending_num)),
        line,
    });
    body.push(assert_stmt(
        Expr::Eq(Box::new(Expr::Var(pending)), Box::new(Expr::Int(1))),
        line,
    ));
    emit_row_lookup(
        helper_read,
        decode,
        param_carrier,
        param_slot,
        line,
        body,
        true,
        page,
        row,
        addr,
        page_ptr_num,
        page_ptr,
        true,
    );
    for (field, target, is_gen) in [
        (layout::ROW_STATE, state, false),
        (layout::ROW_GENERATION, rowgen, true),
        (layout::ROW_PHYSICAL_BASE, rowbase, false),
    ] {
        body.push(Stmt::Let {
            index: field_num,
            declared_schema: Some(TypeSchema::Number),
            expr: read_cell(
                helper_read,
                param_carrier,
                Expr::Add(Box::new(Expr::Var(addr)), Box::new(Expr::Int(field))),
            ),
            line,
        });
        body.push(Stmt::Let {
            index: target,
            declared_schema: Some(TypeSchema::Int),
            expr: if is_gen {
                decode_gen_call(decode_u32, Expr::Var(field_num))
            } else {
                decode_int_call(decode, Expr::Var(field_num))
            },
            line,
        });
    }
    body.push(assert_stmt(
        Expr::Eq(
            Box::new(Expr::Var(state)),
            Box::new(Expr::Int(layout::STATE_PENDING)),
        ),
        line,
    ));
    body.push(assert_stmt(
        Expr::Eq(Box::new(Expr::Var(rowgen)), Box::new(Expr::Var(param_gen))),
        line,
    ));
    body.push(assert_stmt(
        Expr::Eq(
            Box::new(Expr::Var(rowbase)),
            Box::new(Expr::Var(param_base)),
        ),
        line,
    ));
    body.push(Stmt::Let {
        index: kind_num,
        declared_schema: Some(TypeSchema::Number),
        expr: read_cell(helper_read, param_carrier, Expr::Var(param_base)),
        line,
    });
    body.push(Stmt::Let {
        index: kind,
        declared_schema: Some(TypeSchema::Int),
        expr: decode_int_call(decode, Expr::Var(kind_num)),
        line,
    });
    body.push(Stmt::Let {
        index: field_num,
        declared_schema: Some(TypeSchema::Number),
        expr: read_cell(
            helper_read,
            param_carrier,
            Expr::Add(Box::new(Expr::Var(param_base)), Box::new(Expr::Int(1))),
        ),
        line,
    });
    body.push(Stmt::Let {
        index: headgen,
        declared_schema: Some(TypeSchema::Int),
        expr: decode_gen_call(decode_u32, Expr::Var(field_num)),
        line,
    });
    body.push(assert_stmt(
        Expr::Eq(Box::new(Expr::Var(headgen)), Box::new(Expr::Var(param_gen))),
        line,
    ));
    emit_frame_check(
        helper_read,
        decode,
        param_carrier,
        param_frame,
        param_entry,
        line,
        body,
        class_num,
        class,
        cap_num,
        cap,
        end_num,
        end,
    );
    let _ = helper_write;
    Ok(())
}

/// Shared publish commit: direct validated root write of the newborn triple,
/// row activation, live accounting, and the pending guard cleared LAST.
/// Fatal interruption anywhere in this guarded section leaves opaque fully
/// charged storage until reset; no multi-write commit is fuel-atomic.
#[allow(clippy::too_many_arguments)]
fn emit_publish_commit(
    helper_write: u16,
    decode: u16,
    param_carrier: u16,
    param_slot: u16,
    param_gen: u16,
    addr: u16,
    param_frame: u16,
    param_entry: u16,
    category: Expr,
    line: u32,
    body: &mut Vec<Stmt>,
    sink: u16,
    live_num: u16,
    live: u16,
    since_num: u16,
    since: u16,
    helper_read: u16,
    param_ref_slot: Expr,
    param_ref_gen: Expr,
) {
    let entry_base = Expr::Add(
        Box::new(Expr::Add(
            Box::new(Expr::Var(param_frame)),
            Box::new(Expr::Int(layout::FRAME_FIXED_CELLS)),
        )),
        Box::new(Expr::Mul(
            Box::new(Expr::Var(param_entry)),
            Box::new(Expr::Int(layout::FRAME_CELLS_PER_ENTRY)),
        )),
    );
    for (cell, payload) in [(0, category), (1, param_ref_slot), (2, param_ref_gen)] {
        body.push(Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: sink,
            expr: write_call(
                helper_write,
                param_carrier,
                Expr::Add(Box::new(entry_base.clone()), Box::new(Expr::Int(cell))),
                Lowerer::as_number(payload),
            ),
            line,
        });
    }
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: sink,
        expr: write_call(
            helper_write,
            param_carrier,
            Expr::Add(
                Box::new(Expr::Var(addr)),
                Box::new(Expr::Int(layout::ROW_STATE)),
            ),
            Lowerer::as_number(Expr::Int(layout::STATE_ACTIVE)),
        ),
        line,
    });
    body.push(Stmt::Let {
        index: live_num,
        declared_schema: Some(TypeSchema::Number),
        expr: header_cell(helper_read, param_carrier, layout::HDR_LIVE_NODES),
        line,
    });
    body.push(Stmt::Let {
        index: live,
        declared_schema: Some(TypeSchema::Int),
        expr: decode_int_call(decode, Expr::Var(live_num)),
        line,
    });
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: sink,
        expr: write_call(
            helper_write,
            param_carrier,
            Expr::Int(layout::HDR_LIVE_NODES),
            Lowerer::as_number(Expr::Add(Box::new(Expr::Var(live)), Box::new(Expr::Int(1)))),
        ),
        line,
    });
    body.push(Stmt::Let {
        index: since_num,
        declared_schema: Some(TypeSchema::Number),
        expr: header_cell(helper_read, param_carrier, layout::HDR_ALLOCATIONS_SINCE_GC),
        line,
    });
    body.push(Stmt::Let {
        index: since,
        declared_schema: Some(TypeSchema::Int),
        expr: decode_int_call(decode, Expr::Var(since_num)),
        line,
    });
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: sink,
        expr: write_call(
            helper_write,
            param_carrier,
            Expr::Int(layout::HDR_ALLOCATIONS_SINCE_GC),
            Lowerer::as_number(Expr::Add(
                Box::new(Expr::Var(since)),
                Box::new(Expr::Int(1)),
            )),
        ),
        line,
    });
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: sink,
        expr: write_call(
            helper_write,
            param_carrier,
            Expr::Int(layout::HDR_PENDING_NODES),
            Lowerer::as_number(Expr::Int(0)),
        ),
        line,
    });
    let _ = (param_slot, param_gen);
}

/// Append the node publication helper
/// `(Carrier, slot, generation, base, frame, entry: Int) -> JsValue`.
/// Prevalidates the pending allocation/header and destination, writes the
/// newborn root directly from the validated allocation (never through the
/// active-only checked base), activates and accounts, clears the pending
/// guard LAST and returns the real boxed value. The guarded section never
/// allocates or collects.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_node_publish(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &TypeSchema,
    helper_read: u16,
    helper_write: u16,
    decode: u16,
    decode_u32: u16,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let index = fresh_index(lower, ir, span)?;
    let line = lower.line(span);
    let param_carrier = lower.slot(span)?;
    let param_slot = lower.slot(span)?;
    let param_gen = lower.slot(span)?;
    let param_base = lower.slot(span)?;
    let param_frame = lower.slot(span)?;
    let param_entry = lower.slot(span)?;
    let pending_num = lower.slot(span)?;
    let pending = lower.slot(span)?;
    let page = lower.slot(span)?;
    let row = lower.slot(span)?;
    let addr = lower.slot(span)?;
    let field_num = lower.slot(span)?;
    let state = lower.slot(span)?;
    let rowgen = lower.slot(span)?;
    let rowbase = lower.slot(span)?;
    let kind_num = lower.slot(span)?;
    let kind = lower.slot(span)?;
    let headgen = lower.slot(span)?;
    let class_num = lower.slot(span)?;
    let class = lower.slot(span)?;
    let cap_num = lower.slot(span)?;
    let cap = lower.slot(span)?;
    let end_num = lower.slot(span)?;
    let end = lower.slot(span)?;
    let category = lower.slot(span)?;
    let tag = lower.slot(span)?;
    let sink = lower.slot(span)?;
    let live_num = lower.slot(span)?;
    let live = lower.slot(span)?;
    let since_num = lower.slot(span)?;
    let since = lower.slot(span)?;
    let js_schema = value::schema();
    let mut body = vec![Stmt::Let {
        index: sink,
        declared_schema: Some(TypeSchema::Number),
        expr: Expr::Float(0.0),
        line,
    }];
    emit_publish_preamble(
        lower,
        helper_read,
        helper_write,
        decode,
        decode_u32,
        param_carrier,
        param_slot,
        param_gen,
        param_base,
        param_frame,
        param_entry,
        span,
        line,
        &mut body,
        pending_num,
        pending,
        page,
        row,
        addr,
        field_num,
        state,
        rowgen,
        rowbase,
        kind_num,
        kind,
        headgen,
        class_num,
        class,
        cap_num,
        cap,
        end_num,
        end,
    )?;
    // User nodes only: arrays, strings, plain objects and functions get
    // the object-like or string root category (environment nodes publish
    // through the internal edge variant).
    body.push(assert_stmt(
        Expr::And(
            Box::new(Expr::Or(
                Box::new(Expr::Gt(Box::new(Expr::Var(kind)), Box::new(Expr::Int(0)))),
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(kind)),
                    Box::new(Expr::Int(layout::KIND_ARRAY)),
                )),
            )),
            Box::new(Expr::Or(
                Box::new(Expr::Lt(
                    Box::new(Expr::Var(kind)),
                    Box::new(Expr::Int(layout::KIND_ENVIRONMENT)),
                )),
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(kind)),
                    Box::new(Expr::Int(layout::KIND_FUNCTION)),
                )),
            )),
        ),
        line,
    ));
    body.push(Stmt::Let {
        index: category,
        declared_schema: Some(TypeSchema::Int),
        expr: Expr::IfElse {
            condition: Box::new(Expr::Eq(
                Box::new(Expr::Var(kind)),
                Box::new(Expr::Int(layout::KIND_STRING)),
            )),
            then_expr: Box::new(Expr::Int(layout::ROOT_STRING)),
            else_expr: Box::new(Expr::Int(layout::ROOT_OBJECT)),
        },
        line,
    });
    body.push(Stmt::Let {
        index: tag,
        declared_schema: Some(TypeSchema::Int),
        expr: Expr::IfElse {
            condition: Box::new(Expr::Eq(
                Box::new(Expr::Var(kind)),
                Box::new(Expr::Int(layout::KIND_STRING)),
            )),
            then_expr: Box::new(Expr::Int(value::STRING)),
            else_expr: Box::new(Expr::Int(value::OBJECT)),
        },
        line,
    });
    emit_publish_commit(
        helper_write,
        decode,
        param_carrier,
        param_slot,
        param_gen,
        addr,
        param_frame,
        param_entry,
        Expr::Var(category),
        line,
        &mut body,
        sink,
        live_num,
        live,
        since_num,
        since,
        helper_read,
        Expr::Var(param_slot),
        Expr::Var(param_gen),
    );

    declare_helper(
        ir,
        statements,
        layout::NODE_PUBLISH_HELPER,
        index,
        6,
        vec![
            "carrier".to_owned(),
            "slot".to_owned(),
            "generation".to_owned(),
            "base".to_owned(),
            "frame".to_owned(),
            "entry".to_owned(),
        ],
        vec![
            Some(carrier_schema.clone()),
            Some(TypeSchema::Int),
            Some(TypeSchema::Int),
            Some(TypeSchema::Int),
            Some(TypeSchema::Int),
            Some(TypeSchema::Int),
        ],
        Some(js_schema),
        ValueType::Map,
        vec![
            param_carrier,
            param_slot,
            param_gen,
            param_base,
            param_frame,
            param_entry,
        ],
        body,
        value::dynamic_value(
            Expr::Var(tag),
            Expr::Float(0.0),
            Expr::Bool(false),
            Expr::Var(param_slot),
            Expr::Var(param_gen),
        ),
        line,
    );
    Ok(index)
}

/// Append the internal edge publication helper
/// `(Carrier, slot, generation, base, frame, entry, category: Int) -> Int`.
/// Same guarded transaction as plain publication, but registers an
/// environment graph reference under the explicit internal category without
/// making it a user JS object.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_node_publish_edge(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &TypeSchema,
    helper_read: u16,
    helper_write: u16,
    decode: u16,
    decode_u32: u16,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let index = fresh_index(lower, ir, span)?;
    let line = lower.line(span);
    let param_carrier = lower.slot(span)?;
    let param_slot = lower.slot(span)?;
    let param_gen = lower.slot(span)?;
    let param_base = lower.slot(span)?;
    let param_frame = lower.slot(span)?;
    let param_entry = lower.slot(span)?;
    let param_cat = lower.slot(span)?;
    let pending_num = lower.slot(span)?;
    let pending = lower.slot(span)?;
    let page = lower.slot(span)?;
    let row = lower.slot(span)?;
    let addr = lower.slot(span)?;
    let field_num = lower.slot(span)?;
    let state = lower.slot(span)?;
    let rowgen = lower.slot(span)?;
    let rowbase = lower.slot(span)?;
    let kind_num = lower.slot(span)?;
    let kind = lower.slot(span)?;
    let headgen = lower.slot(span)?;
    let class_num = lower.slot(span)?;
    let class = lower.slot(span)?;
    let cap_num = lower.slot(span)?;
    let cap = lower.slot(span)?;
    let end_num = lower.slot(span)?;
    let end = lower.slot(span)?;
    let sink = lower.slot(span)?;
    let live_num = lower.slot(span)?;
    let live = lower.slot(span)?;
    let since_num = lower.slot(span)?;
    let since = lower.slot(span)?;
    let mut body = vec![Stmt::Let {
        index: sink,
        declared_schema: Some(TypeSchema::Number),
        expr: Expr::Float(0.0),
        line,
    }];
    emit_publish_preamble(
        lower,
        helper_read,
        helper_write,
        decode,
        decode_u32,
        param_carrier,
        param_slot,
        param_gen,
        param_base,
        param_frame,
        param_entry,
        span,
        line,
        &mut body,
        pending_num,
        pending,
        page,
        row,
        addr,
        field_num,
        state,
        rowgen,
        rowbase,
        kind_num,
        kind,
        headgen,
        class_num,
        class,
        cap_num,
        cap,
        end_num,
        end,
    )?;
    body.push(assert_stmt(
        Expr::Eq(
            Box::new(Expr::Var(kind)),
            Box::new(Expr::Int(layout::KIND_ENVIRONMENT)),
        ),
        line,
    ));
    body.push(assert_stmt(
        Expr::Eq(
            Box::new(Expr::Var(param_cat)),
            Box::new(Expr::Int(layout::ROOT_INTERNAL)),
        ),
        line,
    ));
    emit_publish_commit(
        helper_write,
        decode,
        param_carrier,
        param_slot,
        param_gen,
        addr,
        param_frame,
        param_entry,
        Expr::Var(param_cat),
        line,
        &mut body,
        sink,
        live_num,
        live,
        since_num,
        since,
        helper_read,
        Expr::Var(param_slot),
        Expr::Var(param_gen),
    );

    declare_helper(
        ir,
        statements,
        layout::NODE_PUBLISH_EDGE_HELPER,
        index,
        7,
        vec![
            "carrier".to_owned(),
            "slot".to_owned(),
            "generation".to_owned(),
            "base".to_owned(),
            "frame".to_owned(),
            "entry".to_owned(),
            "category".to_owned(),
        ],
        vec![
            Some(carrier_schema.clone()),
            Some(TypeSchema::Int),
            Some(TypeSchema::Int),
            Some(TypeSchema::Int),
            Some(TypeSchema::Int),
            Some(TypeSchema::Int),
            Some(TypeSchema::Int),
        ],
        Some(TypeSchema::Int),
        ValueType::Int,
        vec![
            param_carrier,
            param_slot,
            param_gen,
            param_base,
            param_frame,
            param_entry,
            param_cat,
        ],
        body,
        Expr::Int(1),
        line,
    );
    Ok(index)
}

/// Trace one nullable slot/generation edge pair at `base + slot_off`:
/// when the generation is nonzero the target is validated and marked with
/// the expected kind. Shared by every prototype/parent/boxed edge.
#[allow(clippy::too_many_arguments)]
fn emit_trace_pair(
    helper_read: u16,
    decode: u16,
    decode_u32: u16,
    mark: u16,
    param_carrier: u16,
    sink: u16,
    ps: u16,
    pg: u16,
    line: u32,
    body: &mut Vec<Stmt>,
    base: u16,
    slot_off: i64,
    expect: i64,
) {
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: ps,
        expr: decode_int_call(
            decode,
            read_cell(
                helper_read,
                param_carrier,
                Expr::Add(Box::new(Expr::Var(base)), Box::new(Expr::Int(slot_off))),
            ),
        ),
        line,
    });
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: pg,
        expr: decode_gen_call(
            decode_u32,
            read_cell(
                helper_read,
                param_carrier,
                Expr::Add(Box::new(Expr::Var(base)), Box::new(Expr::Int(slot_off + 1))),
            ),
        ),
        line,
    });
    body.push(Stmt::IfElse {
        condition: Expr::Not(Box::new(Expr::Eq(
            Box::new(Expr::Var(pg)),
            Box::new(Expr::Int(0)),
        ))),
        then_branch: vec![Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: sink,
            expr: Lowerer::as_number(Lowerer::guest_call(
                mark,
                vec![
                    Expr::Var(param_carrier),
                    Expr::Var(ps),
                    Expr::Var(pg),
                    Expr::Int(expect),
                ],
            )),
            line,
        }],
        else_branch: Vec::new(),
        line,
    });
}
/// Append the iterative mark/sweep collector `(Carrier) -> Int`.
/// Requires no pending node. Clears active marks and gray links, marks
/// every checked live root, traces the actual node graph through the
/// intrusive gray worklist (no recursion, no repeated full-table scans),
/// sweeps unmarked active rows (freeing or retiring their slots and
/// returning exact charged extents), coalesces adjacent free raw blocks
/// with a rebuilt free list, resets the interval counter and returns the
/// reclaimed node count. Directory pages and active root frames stay
/// system blocks throughout.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_heap_collect(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &TypeSchema,
    helper_read: u16,
    helper_write: u16,
    decode: u16,
    decode_u32: u16,
    mark: u16,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let index = fresh_index(lower, ir, span)?;
    let line = lower.line(span);
    let param_carrier = lower.slot(span)?;
    // Header and scan temps, all declared once up front and updated with
    // Assigns inside every loop below.
    let pending_num = lower.slot(span)?;
    let pending = lower.slot(span)?;
    let next_num = lower.slot(span)?;
    let next = lower.slot(span)?;
    let sink = lower.slot(span)?;
    let rdn = lower.slot(span)?;
    let s = lower.slot(span)?;
    let page = lower.slot(span)?;
    let row = lower.slot(span)?;
    let pp = lower.slot(span)?;
    let addr = lower.slot(span)?;
    let st = lower.slot(span)?;
    let f = lower.slot(span)?;
    let cls = lower.slot(span)?;
    let cap = lower.slot(span)?;
    let e = lower.slot(span)?;
    let cat = lower.slot(span)?;
    let eslot = lower.slot(span)?;
    let egen = lower.slot(span)?;
    let expect = lower.slot(span)?;
    let gray = lower.slot(span)?;
    let base = lower.slot(span)?;
    let cells = lower.slot(span)?;
    let kind = lower.slot(span)?;
    let ps = lower.slot(span)?;
    let pg = lower.slot(span)?;
    let count = lower.slot(span)?;
    let rc = lower.slot(span)?;
    let rb = lower.slot(span)?;
    let ks = lower.slot(span)?;
    let kg = lower.slot(span)?;
    let tag = lower.slot(span)?;
    let vs = lower.slot(span)?;
    let vg = lower.slot(span)?;
    let g_page = lower.slot(span)?;
    let g_row = lower.slot(span)?;
    let g_addr = lower.slot(span)?;
    let g_ppn = lower.slot(span)?;
    let g_pp = lower.slot(span)?;
    let g_next = lower.slot(span)?;
    let reclaimed = lower.slot(span)?;
    let live = lower.slot(span)?;
    let cellsv = lower.slot(span)?;
    let genv = lower.slot(span)?;
    let extent = lower.slot(span)?;
    let retired = lower.slot(span)?;
    let freeh = lower.slot(span)?;
    let bstart = lower.slot(span)?;
    let bsize = lower.slot(span)?;
    let cur = lower.slot(span)?;
    let prev = lower.slot(span)?;
    let prev_size = lower.slot(span)?;
    let end = lower.slot(span)?;
    let sz = lower.slot(span)?;
    let coll = lower.slot(span)?;
    let mut body = vec![
        Stmt::Let {
            index: sink,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: rdn,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: pending_num,
            declared_schema: Some(TypeSchema::Number),
            expr: header_cell(helper_read, param_carrier, layout::HDR_PENDING_NODES),
            line,
        },
        Stmt::Let {
            index: pending,
            declared_schema: Some(TypeSchema::Int),
            expr: decode_int_call(decode, Expr::Var(pending_num)),
            line,
        },
        assert_stmt(
            Expr::Eq(Box::new(Expr::Var(pending)), Box::new(Expr::Int(0))),
            line,
        ),
        Stmt::Let {
            index: next_num,
            declared_schema: Some(TypeSchema::Number),
            expr: header_cell(helper_read, param_carrier, layout::HDR_NEXT_SLOT),
            line,
        },
        Stmt::Let {
            index: next,
            declared_schema: Some(TypeSchema::Int),
            expr: decode_int_call(decode, Expr::Var(next_num)),
            line,
        },
    ];
    // Declare every loop temp once.
    for (temp, schema, init) in [
        (s, TypeSchema::Int, Expr::Int(0)),
        (page, TypeSchema::Int, Expr::Int(0)),
        (row, TypeSchema::Int, Expr::Int(0)),
        (pp, TypeSchema::Int, Expr::Int(0)),
        (addr, TypeSchema::Int, Expr::Int(0)),
        (st, TypeSchema::Int, Expr::Int(0)),
        (f, TypeSchema::Int, Expr::Int(0)),
        (cls, TypeSchema::Int, Expr::Int(0)),
        (cap, TypeSchema::Int, Expr::Int(0)),
        (e, TypeSchema::Int, Expr::Int(0)),
        (cat, TypeSchema::Int, Expr::Int(0)),
        (eslot, TypeSchema::Int, Expr::Int(0)),
        (egen, TypeSchema::Int, Expr::Int(0)),
        (expect, TypeSchema::Int, Expr::Int(0)),
        (gray, TypeSchema::Int, Expr::Int(0)),
        (base, TypeSchema::Int, Expr::Int(0)),
        (cells, TypeSchema::Int, Expr::Int(0)),
        (kind, TypeSchema::Int, Expr::Int(0)),
        (ps, TypeSchema::Int, Expr::Int(0)),
        (pg, TypeSchema::Int, Expr::Int(0)),
        (count, TypeSchema::Int, Expr::Int(0)),
        (rc, TypeSchema::Int, Expr::Int(0)),
        (rb, TypeSchema::Int, Expr::Int(0)),
        (ks, TypeSchema::Int, Expr::Int(0)),
        (kg, TypeSchema::Int, Expr::Int(0)),
        (tag, TypeSchema::Int, Expr::Int(0)),
        (vs, TypeSchema::Int, Expr::Int(0)),
        (vg, TypeSchema::Int, Expr::Int(0)),
        (g_page, TypeSchema::Int, Expr::Int(0)),
        (g_row, TypeSchema::Int, Expr::Int(0)),
        (g_addr, TypeSchema::Int, Expr::Int(0)),
        (g_ppn, TypeSchema::Number, Expr::Float(0.0)),
        (g_pp, TypeSchema::Int, Expr::Int(0)),
        (g_next, TypeSchema::Int, Expr::Int(0)),
        (reclaimed, TypeSchema::Int, Expr::Int(0)),
        (live, TypeSchema::Int, Expr::Int(0)),
        (cellsv, TypeSchema::Int, Expr::Int(0)),
        (genv, TypeSchema::Number, Expr::Float(0.0)),
        (extent, TypeSchema::Int, Expr::Int(0)),
        (retired, TypeSchema::Int, Expr::Int(0)),
        (freeh, TypeSchema::Int, Expr::Int(0)),
        (bstart, TypeSchema::Int, Expr::Int(0)),
        (bsize, TypeSchema::Int, Expr::Int(0)),
        (cur, TypeSchema::Int, Expr::Int(0)),
        (prev, TypeSchema::Int, Expr::Int(-1)),
        (prev_size, TypeSchema::Int, Expr::Int(0)),
        (end, TypeSchema::Int, Expr::Int(0)),
        (sz, TypeSchema::Int, Expr::Int(0)),
        (coll, TypeSchema::Int, Expr::Int(0)),
    ] {
        body.push(Stmt::Let {
            index: temp,
            declared_schema: Some(schema),
            expr: init,
            line,
        });
    }
    let read = |slot: u16| read_cell(helper_read, param_carrier, Expr::Var(slot));
    // Phase 1: empty the gray worklist, then clear active marks/links with
    // a sequential directory scan (row state domain checked fail-closed).
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: sink,
        expr: write_call(
            helper_write,
            param_carrier,
            Expr::Int(layout::HDR_GRAY_HEAD_PLUS_ONE),
            Lowerer::as_number(Expr::Int(0)),
        ),
        line,
    });
    body.push(Stmt::While {
        condition: Expr::Lt(
            Box::new(Lowerer::as_number(Expr::Var(s))),
            Box::new(Lowerer::as_number(Expr::Var(next))),
        ),
        body: vec![
            Stmt::IfElse {
                condition: Expr::Eq(Box::new(Expr::Var(row)), Box::new(Expr::Int(0))),
                then_branch: vec![
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: rdn,
                        expr: read_cell(
                            helper_read,
                            param_carrier,
                            Expr::Add(
                                Box::new(Expr::Int(layout::HEADER_CELLS)),
                                Box::new(Expr::Var(page)),
                            ),
                        ),
                        line,
                    },
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: pp,
                        expr: decode_int_call(decode, Expr::Var(rdn)),
                        line,
                    },
                ],
                else_branch: Vec::new(),
                line,
            },
            Stmt::IfElse {
                condition: Expr::Not(Box::new(Expr::Eq(
                    Box::new(Expr::Var(pp)),
                    Box::new(Expr::Int(0)),
                ))),
                then_branch: vec![
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: addr,
                        expr: Expr::Add(
                            Box::new(Expr::Add(
                                Box::new(Expr::Var(pp)),
                                Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
                            )),
                            Box::new(Expr::Mul(
                                Box::new(Expr::Var(row)),
                                Box::new(Expr::Int(layout::ROW_CELLS)),
                            )),
                        ),
                        line,
                    },
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: rdn,
                        expr: read_cell(
                            helper_read,
                            param_carrier,
                            Expr::Add(
                                Box::new(Expr::Var(addr)),
                                Box::new(Expr::Int(layout::ROW_STATE)),
                            ),
                        ),
                        line,
                    },
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: st,
                        expr: decode_int_call(decode, Expr::Var(rdn)),
                        line,
                    },
                    assert_stmt(
                        Expr::Or(
                            Box::new(Expr::Lt(
                                Box::new(Expr::Var(st)),
                                Box::new(Expr::Int(layout::STATE_RETIRED)),
                            )),
                            Box::new(Expr::Eq(
                                Box::new(Expr::Var(st)),
                                Box::new(Expr::Int(layout::STATE_RETIRED)),
                            )),
                        ),
                        line,
                    ),
                    Stmt::IfElse {
                        condition: Expr::Eq(
                            Box::new(Expr::Var(st)),
                            Box::new(Expr::Int(layout::STATE_ACTIVE)),
                        ),
                        then_branch: vec![
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: sink,
                                expr: write_call(
                                    helper_write,
                                    param_carrier,
                                    Expr::Add(
                                        Box::new(Expr::Var(addr)),
                                        Box::new(Expr::Int(layout::ROW_MARK)),
                                    ),
                                    Lowerer::as_number(Expr::Int(0)),
                                ),
                                line,
                            },
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: sink,
                                expr: write_call(
                                    helper_write,
                                    param_carrier,
                                    Expr::Add(
                                        Box::new(Expr::Var(addr)),
                                        Box::new(Expr::Int(layout::ROW_GRAY_NEXT_PLUS_ONE)),
                                    ),
                                    Lowerer::as_number(Expr::Int(0)),
                                ),
                                line,
                            },
                        ],
                        else_branch: Vec::new(),
                        line,
                    },
                ],
                else_branch: Vec::new(),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: row,
                expr: Expr::Add(Box::new(Expr::Var(row)), Box::new(Expr::Int(1))),
                line,
            },
            Stmt::IfElse {
                condition: Expr::Eq(
                    Box::new(Expr::Var(row)),
                    Box::new(Expr::Int(layout::PAGE_ROWS)),
                ),
                then_branch: vec![
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: row,
                        expr: Expr::Int(0),
                        line,
                    },
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: page,
                        expr: Expr::Add(Box::new(Expr::Var(page)), Box::new(Expr::Int(1))),
                        line,
                    },
                ],
                else_branch: Vec::new(),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: s,
                expr: Expr::Add(Box::new(Expr::Var(s)), Box::new(Expr::Int(1))),
                line,
            },
        ],
        line,
    });
    let _ = read;
    // Phase 2: scan every active frame and mark each checked live root.
    // A stale edge is corruption and fails closed inside the marker.
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: rdn,
        expr: header_cell(helper_read, param_carrier, layout::HDR_ROOT_TOP),
        line,
    });
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: f,
        expr: decode_int_call(decode, Expr::Var(rdn)),
        line,
    });
    body.push(Stmt::While {
        condition: Expr::Not(Box::new(Expr::Eq(
            Box::new(Expr::Var(f)),
            Box::new(Expr::Int(0)),
        ))),
        body: vec![
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: cls,
                expr: decode_int_call(
                    decode,
                    read_cell(
                        helper_read,
                        param_carrier,
                        Expr::Add(
                            Box::new(Expr::Var(f)),
                            Box::new(Expr::Int(layout::RAW_CLASS)),
                        ),
                    ),
                ),
                line,
            },
            assert_stmt(
                Expr::Eq(
                    Box::new(Expr::Var(cls)),
                    Box::new(Expr::Int(layout::RAW_CLASS_ROOT_FRAME)),
                ),
                line,
            ),
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: cap,
                expr: decode_int_call(
                    decode,
                    read_cell(
                        helper_read,
                        param_carrier,
                        Expr::Add(
                            Box::new(Expr::Var(f)),
                            Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
                        ),
                    ),
                ),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: e,
                expr: Expr::Int(0),
                line,
            },
            Stmt::While {
                condition: Expr::Lt(Box::new(Expr::Var(e)), Box::new(Expr::Var(cap))),
                body: vec![
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: cat,
                        expr: decode_int_call(
                            decode,
                            read_cell(
                                helper_read,
                                param_carrier,
                                Expr::Add(
                                    Box::new(Expr::Add(
                                        Box::new(Expr::Var(f)),
                                        Box::new(Expr::Int(layout::FRAME_FIXED_CELLS)),
                                    )),
                                    Box::new(Expr::Mul(
                                        Box::new(Expr::Var(e)),
                                        Box::new(Expr::Int(layout::FRAME_CELLS_PER_ENTRY)),
                                    )),
                                ),
                            ),
                        ),
                        line,
                    },
                    Stmt::IfElse {
                        condition: Expr::Not(Box::new(Expr::Eq(
                            Box::new(Expr::Var(cat)),
                            Box::new(Expr::Int(0)),
                        ))),
                        then_branch: vec![
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: eslot,
                                expr: decode_int_call(
                                    decode,
                                    read_cell(
                                        helper_read,
                                        param_carrier,
                                        Expr::Add(
                                            Box::new(Expr::Add(
                                                Box::new(Expr::Var(f)),
                                                Box::new(Expr::Int(layout::FRAME_FIXED_CELLS)),
                                            )),
                                            Box::new(Expr::Add(
                                                Box::new(Expr::Mul(
                                                    Box::new(Expr::Var(e)),
                                                    Box::new(Expr::Int(
                                                        layout::FRAME_CELLS_PER_ENTRY,
                                                    )),
                                                )),
                                                Box::new(Expr::Int(1)),
                                            )),
                                        ),
                                    ),
                                ),
                                line,
                            },
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: egen,
                                expr: decode_gen_call(
                                    decode_u32,
                                    read_cell(
                                        helper_read,
                                        param_carrier,
                                        Expr::Add(
                                            Box::new(Expr::Add(
                                                Box::new(Expr::Var(f)),
                                                Box::new(Expr::Int(layout::FRAME_FIXED_CELLS)),
                                            )),
                                            Box::new(Expr::Add(
                                                Box::new(Expr::Mul(
                                                    Box::new(Expr::Var(e)),
                                                    Box::new(Expr::Int(
                                                        layout::FRAME_CELLS_PER_ENTRY,
                                                    )),
                                                )),
                                                Box::new(Expr::Int(2)),
                                            )),
                                        ),
                                    ),
                                ),
                                line,
                            },
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: expect,
                                expr: Expr::IfElse {
                                    condition: Box::new(Expr::Eq(
                                        Box::new(Expr::Var(cat)),
                                        Box::new(Expr::Int(layout::ROOT_STRING)),
                                    )),
                                    then_expr: Box::new(Expr::Int(2)),
                                    else_expr: Box::new(Expr::IfElse {
                                        condition: Box::new(Expr::Eq(
                                            Box::new(Expr::Var(cat)),
                                            Box::new(Expr::Int(layout::ROOT_OBJECT)),
                                        )),
                                        then_expr: Box::new(Expr::Int(3)),
                                        else_expr: Box::new(Expr::Int(5)),
                                    }),
                                },
                                line,
                            },
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: sink,
                                expr: Lowerer::as_number(Lowerer::guest_call(
                                    mark,
                                    vec![
                                        Expr::Var(param_carrier),
                                        Expr::Var(eslot),
                                        Expr::Var(egen),
                                        Expr::Var(expect),
                                    ],
                                )),
                                line,
                            },
                        ],
                        else_branch: Vec::new(),
                        line,
                    },
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: e,
                        expr: Expr::Add(Box::new(Expr::Var(e)), Box::new(Expr::Int(1))),
                        line,
                    },
                ],
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: f,
                expr: decode_int_call(
                    decode,
                    read_cell(
                        helper_read,
                        param_carrier,
                        Expr::Add(
                            Box::new(Expr::Var(f)),
                            Box::new(Expr::Int(layout::RAW_FREE_NEXT)),
                        ),
                    ),
                ),
                line,
            },
        ],
        line,
    });
    // Phase 3: drain the intrusive gray worklist, tracing actual children
    // per node kind. Object property keys must resolve to live strings and
    // string/object payloads are traced alongside object-like ones; array
    // elements stay Float numbers with no edges.
    // Object property-row scan shared by the object arm below.
    let object_rows = {
        let mut rows = vec![
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: rdn,
                expr: read_cell(
                    helper_read,
                    param_carrier,
                    Expr::Add(Box::new(Expr::Var(base)), Box::new(Expr::Int(2))),
                ),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: count,
                expr: decode_int_call(decode, Expr::Var(rdn)),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: rc,
                expr: Expr::Int(0),
                line,
            },
        ];
        rows.push(Stmt::While {
            condition: Expr::Lt(
                Box::new(Lowerer::as_number(Expr::Var(rc))),
                Box::new(Lowerer::as_number(Expr::Var(count))),
            ),
            body: vec![
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: rb,
                    expr: Expr::Add(
                        Box::new(Expr::Add(
                            Box::new(Expr::Var(base)),
                            Box::new(Expr::Int(layout::OBJECT_HEADER_CELLS)),
                        )),
                        Box::new(Expr::Mul(
                            Box::new(Expr::Var(rc)),
                            Box::new(Expr::Int(layout::OBJECT_CELLS_PER_PROPERTY)),
                        )),
                    ),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: ks,
                    expr: decode_int_call(
                        decode,
                        read_cell(helper_read, param_carrier, Expr::Var(rb)),
                    ),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: kg,
                    expr: decode_gen_call(
                        decode_u32,
                        read_cell(
                            helper_read,
                            param_carrier,
                            Expr::Add(Box::new(Expr::Var(rb)), Box::new(Expr::Int(1))),
                        ),
                    ),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: sink,
                    expr: Lowerer::as_number(Lowerer::guest_call(
                        mark,
                        vec![
                            Expr::Var(param_carrier),
                            Expr::Var(ks),
                            Expr::Var(kg),
                            Expr::Int(2),
                        ],
                    )),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: tag,
                    expr: decode_int_call(
                        decode,
                        read_cell(
                            helper_read,
                            param_carrier,
                            Expr::Add(Box::new(Expr::Var(rb)), Box::new(Expr::Int(2))),
                        ),
                    ),
                    line,
                },
                assert_stmt(
                    Expr::Or(
                        Box::new(Expr::Lt(
                            Box::new(Expr::Var(tag)),
                            Box::new(Expr::Int(value::OBJECT)),
                        )),
                        Box::new(Expr::Eq(
                            Box::new(Expr::Var(tag)),
                            Box::new(Expr::Int(value::OBJECT)),
                        )),
                    ),
                    line,
                ),
                Stmt::IfElse {
                    condition: Expr::Eq(
                        Box::new(Expr::Var(tag)),
                        Box::new(Expr::Int(value::STRING)),
                    ),
                    then_branch: vec![
                        Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: vs,
                            expr: decode_int_call(
                                decode,
                                read_cell(
                                    helper_read,
                                    param_carrier,
                                    Expr::Add(Box::new(Expr::Var(rb)), Box::new(Expr::Int(5))),
                                ),
                            ),
                            line,
                        },
                        Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: vg,
                            expr: decode_gen_call(
                                decode_u32,
                                read_cell(
                                    helper_read,
                                    param_carrier,
                                    Expr::Add(Box::new(Expr::Var(rb)), Box::new(Expr::Int(6))),
                                ),
                            ),
                            line,
                        },
                        Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: sink,
                            expr: Lowerer::as_number(Lowerer::guest_call(
                                mark,
                                vec![
                                    Expr::Var(param_carrier),
                                    Expr::Var(vs),
                                    Expr::Var(vg),
                                    Expr::Int(2),
                                ],
                            )),
                            line,
                        },
                    ],
                    else_branch: Vec::new(),
                    line,
                },
                Stmt::IfElse {
                    condition: Expr::Eq(
                        Box::new(Expr::Var(tag)),
                        Box::new(Expr::Int(value::OBJECT)),
                    ),
                    then_branch: vec![
                        Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: vs,
                            expr: decode_int_call(
                                decode,
                                read_cell(
                                    helper_read,
                                    param_carrier,
                                    Expr::Add(Box::new(Expr::Var(rb)), Box::new(Expr::Int(5))),
                                ),
                            ),
                            line,
                        },
                        Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: vg,
                            expr: decode_gen_call(
                                decode_u32,
                                read_cell(
                                    helper_read,
                                    param_carrier,
                                    Expr::Add(Box::new(Expr::Var(rb)), Box::new(Expr::Int(6))),
                                ),
                            ),
                            line,
                        },
                        Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: sink,
                            expr: Lowerer::as_number(Lowerer::guest_call(
                                mark,
                                vec![
                                    Expr::Var(param_carrier),
                                    Expr::Var(vs),
                                    Expr::Var(vg),
                                    Expr::Int(3),
                                ],
                            )),
                            line,
                        },
                    ],
                    else_branch: Vec::new(),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: rc,
                    expr: Expr::Add(Box::new(Expr::Var(rc)), Box::new(Expr::Int(1))),
                    line,
                },
            ],
            line,
        });
        rows
    };
    // Environment boxed-entry scan (5 scalar cells per entry, no key pair).
    let env_entries = {
        let mut entries = vec![
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: rdn,
                expr: read_cell(
                    helper_read,
                    param_carrier,
                    Expr::Add(Box::new(Expr::Var(base)), Box::new(Expr::Int(2))),
                ),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: count,
                expr: decode_int_call(decode, Expr::Var(rdn)),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: rc,
                expr: Expr::Int(0),
                line,
            },
        ];
        entries.push(Stmt::While {
            condition: Expr::Lt(
                Box::new(Lowerer::as_number(Expr::Var(rc))),
                Box::new(Lowerer::as_number(Expr::Var(count))),
            ),
            body: vec![
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: rb,
                    expr: Expr::Add(
                        Box::new(Expr::Add(
                            Box::new(Expr::Var(base)),
                            Box::new(Expr::Int(layout::ENV_HEADER_CELLS)),
                        )),
                        Box::new(Expr::Mul(
                            Box::new(Expr::Var(rc)),
                            Box::new(Expr::Int(layout::ENV_CELLS_PER_ENTRY)),
                        )),
                    ),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: tag,
                    expr: decode_int_call(
                        decode,
                        read_cell(helper_read, param_carrier, Expr::Var(rb)),
                    ),
                    line,
                },
                assert_stmt(
                    Expr::Or(
                        Box::new(Expr::Lt(
                            Box::new(Expr::Var(tag)),
                            Box::new(Expr::Int(value::OBJECT)),
                        )),
                        Box::new(Expr::Eq(
                            Box::new(Expr::Var(tag)),
                            Box::new(Expr::Int(value::OBJECT)),
                        )),
                    ),
                    line,
                ),
                Stmt::IfElse {
                    condition: Expr::Or(
                        Box::new(Expr::Eq(
                            Box::new(Expr::Var(tag)),
                            Box::new(Expr::Int(value::STRING)),
                        )),
                        Box::new(Expr::Eq(
                            Box::new(Expr::Var(tag)),
                            Box::new(Expr::Int(value::OBJECT)),
                        )),
                    ),
                    then_branch: vec![
                        Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: vs,
                            expr: decode_int_call(
                                decode,
                                read_cell(
                                    helper_read,
                                    param_carrier,
                                    Expr::Add(Box::new(Expr::Var(rb)), Box::new(Expr::Int(3))),
                                ),
                            ),
                            line,
                        },
                        Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: vg,
                            expr: decode_gen_call(
                                decode_u32,
                                read_cell(
                                    helper_read,
                                    param_carrier,
                                    Expr::Add(Box::new(Expr::Var(rb)), Box::new(Expr::Int(4))),
                                ),
                            ),
                            line,
                        },
                        Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: sink,
                            expr: Lowerer::as_number(Lowerer::guest_call(
                                mark,
                                vec![
                                    Expr::Var(param_carrier),
                                    Expr::Var(vs),
                                    Expr::Var(vg),
                                    Expr::IfElse {
                                        condition: Box::new(Expr::Eq(
                                            Box::new(Expr::Var(tag)),
                                            Box::new(Expr::Int(value::STRING)),
                                        )),
                                        then_expr: Box::new(Expr::Int(2)),
                                        else_expr: Box::new(Expr::Int(3)),
                                    },
                                ],
                            )),
                            line,
                        },
                    ],
                    else_branch: Vec::new(),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: rc,
                    expr: Expr::Add(Box::new(Expr::Var(rc)), Box::new(Expr::Int(1))),
                    line,
                },
            ],
            line,
        });
        entries
    };
    let mut gray_body = vec![Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: s,
        expr: Expr::Sub(Box::new(Expr::Var(gray)), Box::new(Expr::Int(1))),
        line,
    }];
    emit_row_lookup(
        helper_read,
        decode,
        param_carrier,
        s,
        line,
        &mut gray_body,
        false,
        g_page,
        g_row,
        g_addr,
        g_ppn,
        g_pp,
        true,
    );
    gray_body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: rdn,
        expr: read_cell(
            helper_read,
            param_carrier,
            Expr::Add(
                Box::new(Expr::Var(g_addr)),
                Box::new(Expr::Int(layout::ROW_GRAY_NEXT_PLUS_ONE)),
            ),
        ),
        line,
    });
    gray_body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: g_next,
        expr: decode_int_call(decode, Expr::Var(rdn)),
        line,
    });
    gray_body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: sink,
        expr: write_call(
            helper_write,
            param_carrier,
            Expr::Int(layout::HDR_GRAY_HEAD_PLUS_ONE),
            Lowerer::as_number(Expr::Var(g_next)),
        ),
        line,
    });
    gray_body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: rdn,
        expr: read_cell(
            helper_read,
            param_carrier,
            Expr::Add(
                Box::new(Expr::Var(g_addr)),
                Box::new(Expr::Int(layout::ROW_PHYSICAL_BASE)),
            ),
        ),
        line,
    });
    gray_body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: base,
        expr: decode_int_call(decode, Expr::Var(rdn)),
        line,
    });
    gray_body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: kind,
        expr: decode_int_call(
            decode,
            read_cell(helper_read, param_carrier, Expr::Var(base)),
        ),
        line,
    });
    gray_body.push(assert_stmt(
        Expr::And(
            Box::new(Expr::Or(
                Box::new(Expr::Gt(Box::new(Expr::Var(kind)), Box::new(Expr::Int(0)))),
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(kind)),
                    Box::new(Expr::Int(layout::KIND_ARRAY)),
                )),
            )),
            Box::new(Expr::Or(
                Box::new(Expr::Lt(
                    Box::new(Expr::Var(kind)),
                    Box::new(Expr::Int(layout::KIND_ENVIRONMENT)),
                )),
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(kind)),
                    Box::new(Expr::Int(layout::KIND_ENVIRONMENT)),
                )),
            )),
        ),
        line,
    ));
    // Per-kind child tracing. Strings carry no edges.
    let mut array_arm = Vec::new();
    emit_trace_pair(
        helper_read,
        decode,
        decode_u32,
        mark,
        param_carrier,
        sink,
        ps,
        pg,
        line,
        &mut array_arm,
        base,
        layout::ARRAY_HEADER_CELLS - 2,
        3,
    );
    let mut object_arm = Vec::new();
    emit_trace_pair(
        helper_read,
        decode,
        decode_u32,
        mark,
        param_carrier,
        sink,
        ps,
        pg,
        line,
        &mut object_arm,
        base,
        layout::OBJECT_HEADER_CELLS - 2,
        3,
    );
    object_arm.extend(object_rows);
    let mut function_arm = Vec::new();
    emit_trace_pair(
        helper_read,
        decode,
        decode_u32,
        mark,
        param_carrier,
        sink,
        ps,
        pg,
        line,
        &mut function_arm,
        base,
        3,
        3,
    );
    function_arm.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: rdn,
        expr: read_cell(
            helper_read,
            param_carrier,
            Expr::Add(Box::new(Expr::Var(base)), Box::new(Expr::Int(6))),
        ),
        line,
    });
    function_arm.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: ps,
        expr: decode_int_call(decode, Expr::Var(rdn)),
        line,
    });
    function_arm.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: rdn,
        expr: read_cell(
            helper_read,
            param_carrier,
            Expr::Add(Box::new(Expr::Var(base)), Box::new(Expr::Int(7))),
        ),
        line,
    });
    function_arm.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: pg,
        expr: decode_gen_call(decode_u32, Expr::Var(rdn)),
        line,
    });
    function_arm.push(Stmt::IfElse {
        condition: Expr::Not(Box::new(Expr::Eq(
            Box::new(Expr::Var(pg)),
            Box::new(Expr::Int(0)),
        ))),
        then_branch: vec![Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: sink,
            expr: Lowerer::as_number(Lowerer::guest_call(
                mark,
                vec![
                    Expr::Var(param_carrier),
                    Expr::Var(ps),
                    Expr::Var(pg),
                    Expr::Int(5),
                ],
            )),
            line,
        }],
        else_branch: Vec::new(),
        line,
    });
    let mut env_arm = vec![assert_stmt(
        Expr::Eq(
            Box::new(Expr::Var(kind)),
            Box::new(Expr::Int(layout::KIND_ENVIRONMENT)),
        ),
        line,
    )];
    emit_trace_pair(
        helper_read,
        decode,
        decode_u32,
        mark,
        param_carrier,
        sink,
        ps,
        pg,
        line,
        &mut env_arm,
        base,
        layout::ENV_HEADER_CELLS - 2,
        5,
    );
    env_arm.extend(env_entries);
    gray_body.push(Stmt::IfElse {
        condition: Expr::Eq(
            Box::new(Expr::Var(kind)),
            Box::new(Expr::Int(layout::KIND_STRING)),
        ),
        // Strings carry no edges.
        then_branch: Vec::new(),
        else_branch: vec![Stmt::IfElse {
            condition: Expr::Eq(
                Box::new(Expr::Var(kind)),
                Box::new(Expr::Int(layout::KIND_ARRAY)),
            ),
            then_branch: array_arm,
            else_branch: vec![Stmt::IfElse {
                condition: Expr::Eq(
                    Box::new(Expr::Var(kind)),
                    Box::new(Expr::Int(layout::KIND_OBJECT)),
                ),
                then_branch: object_arm,
                else_branch: vec![Stmt::IfElse {
                    condition: Expr::Eq(
                        Box::new(Expr::Var(kind)),
                        Box::new(Expr::Int(layout::KIND_FUNCTION)),
                    ),
                    then_branch: function_arm,
                    else_branch: env_arm,
                    line,
                }],
                line,
            }],
            line,
        }],
        line,
    });
    // Refresh the head: tracing may have pushed newly discovered nodes.
    gray_body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: rdn,
        expr: header_cell(helper_read, param_carrier, layout::HDR_GRAY_HEAD_PLUS_ONE),
        line,
    });
    gray_body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: gray,
        expr: decode_int_call(decode, Expr::Var(rdn)),
        line,
    });
    // Root marking populated the backing's worklist. Seed the drain local
    // from that actual head before its first condition is evaluated.
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: rdn,
        expr: header_cell(helper_read, param_carrier, layout::HDR_GRAY_HEAD_PLUS_ONE),
        line,
    });
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: gray,
        expr: decode_int_call(decode, Expr::Var(rdn)),
        line,
    });
    body.push(Stmt::While {
        condition: Expr::Not(Box::new(Expr::Eq(
            Box::new(Expr::Var(gray)),
            Box::new(Expr::Int(0)),
        ))),
        body: gray_body,
        line,
    });
    // Phase 4: sweep unmarked active rows. Maximum-generation nodes
    // retire instead of wrapping; all others rejoin the free list with
    // their retained generation. Exact charged extents return to the raw
    // free list; live accounting moves down by the same amounts.
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: s,
        expr: Expr::Int(0),
        line,
    });
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: page,
        expr: Expr::Int(0),
        line,
    });
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: row,
        expr: Expr::Int(0),
        line,
    });
    body.push(Stmt::While {
        condition: Expr::Lt(
            Box::new(Lowerer::as_number(Expr::Var(s))),
            Box::new(Lowerer::as_number(Expr::Var(next))),
        ),
        body: vec![
            Stmt::IfElse {
                condition: Expr::Eq(Box::new(Expr::Var(row)), Box::new(Expr::Int(0))),
                then_branch: vec![
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: rdn,
                        expr: read_cell(
                            helper_read,
                            param_carrier,
                            Expr::Add(
                                Box::new(Expr::Int(layout::HEADER_CELLS)),
                                Box::new(Expr::Var(page)),
                            ),
                        ),
                        line,
                    },
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: pp,
                        expr: decode_int_call(decode, Expr::Var(rdn)),
                        line,
                    },
                ],
                else_branch: Vec::new(),
                line,
            },
            Stmt::IfElse {
                condition: Expr::Not(Box::new(Expr::Eq(
                    Box::new(Expr::Var(pp)),
                    Box::new(Expr::Int(0)),
                ))),
                then_branch: vec![
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: addr,
                        expr: Expr::Add(
                            Box::new(Expr::Add(
                                Box::new(Expr::Var(pp)),
                                Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
                            )),
                            Box::new(Expr::Mul(
                                Box::new(Expr::Var(row)),
                                Box::new(Expr::Int(layout::ROW_CELLS)),
                            )),
                        ),
                        line,
                    },
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: rdn,
                        expr: read_cell(
                            helper_read,
                            param_carrier,
                            Expr::Add(
                                Box::new(Expr::Var(addr)),
                                Box::new(Expr::Int(layout::ROW_STATE)),
                            ),
                        ),
                        line,
                    },
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: st,
                        expr: decode_int_call(decode, Expr::Var(rdn)),
                        line,
                    },
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: cls,
                        expr: decode_int_call(
                            decode,
                            read_cell(
                                helper_read,
                                param_carrier,
                                Expr::Add(
                                    Box::new(Expr::Var(addr)),
                                    Box::new(Expr::Int(layout::ROW_MARK)),
                                ),
                            ),
                        ),
                        line,
                    },
                    Stmt::IfElse {
                        condition: Expr::And(
                            Box::new(Expr::Eq(
                                Box::new(Expr::Var(st)),
                                Box::new(Expr::Int(layout::STATE_ACTIVE)),
                            )),
                            Box::new(Expr::Eq(Box::new(Expr::Var(cls)), Box::new(Expr::Int(0)))),
                        ),
                        then_branch: vec![
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: genv,
                                expr: read_cell(
                                    helper_read,
                                    param_carrier,
                                    Expr::Add(
                                        Box::new(Expr::Var(addr)),
                                        Box::new(Expr::Int(layout::ROW_GENERATION)),
                                    ),
                                ),
                                line,
                            },
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: rdn,
                                expr: read_cell(
                                    helper_read,
                                    param_carrier,
                                    Expr::Add(
                                        Box::new(Expr::Var(addr)),
                                        Box::new(Expr::Int(layout::ROW_PHYSICAL_BASE)),
                                    ),
                                ),
                                line,
                            },
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: base,
                                expr: decode_int_call(decode, Expr::Var(rdn)),
                                line,
                            },
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: rdn,
                                expr: read_cell(
                                    helper_read,
                                    param_carrier,
                                    Expr::Add(
                                        Box::new(Expr::Var(addr)),
                                        Box::new(Expr::Int(layout::ROW_DATA_CELLS)),
                                    ),
                                ),
                                line,
                            },
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: cells,
                                expr: decode_int_call(decode, Expr::Var(rdn)),
                                line,
                            },
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: extent,
                                expr: Expr::Add(
                                    Box::new(Expr::Var(cells)),
                                    Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
                                ),
                                line,
                            },
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: rdn,
                                expr: header_cell(
                                    helper_read,
                                    param_carrier,
                                    layout::HDR_LIVE_NODES,
                                ),
                                line,
                            },
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: live,
                                expr: decode_int_call(decode, Expr::Var(rdn)),
                                line,
                            },
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: sink,
                                expr: write_call(
                                    helper_write,
                                    param_carrier,
                                    Expr::Int(layout::HDR_LIVE_NODES),
                                    Lowerer::as_number(Expr::Sub(
                                        Box::new(Expr::Var(live)),
                                        Box::new(Expr::Int(1)),
                                    )),
                                ),
                                line,
                            },
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: rdn,
                                expr: header_cell(
                                    helper_read,
                                    param_carrier,
                                    layout::HDR_NODE_CELLS,
                                ),
                                line,
                            },
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: cellsv,
                                expr: decode_int_call(decode, Expr::Var(rdn)),
                                line,
                            },
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: sink,
                                expr: write_call(
                                    helper_write,
                                    param_carrier,
                                    Expr::Int(layout::HDR_NODE_CELLS),
                                    Lowerer::as_number(Expr::Sub(
                                        Box::new(Expr::Var(cellsv)),
                                        Box::new(Expr::Var(extent)),
                                    )),
                                ),
                                line,
                            },
                            Stmt::IfElse {
                                condition: Expr::Eq(
                                    Box::new(Lowerer::as_number(Expr::Var(genv))),
                                    Box::new(Expr::Float(layout::MAX_GENERATION_F64)),
                                ),
                                then_branch: vec![
                                    Stmt::Assign {
                                        kind: vm::AssignmentKind::Set,
                                        index: sink,
                                        expr: write_call(
                                            helper_write,
                                            param_carrier,
                                            Expr::Var(addr),
                                            Lowerer::as_number(Expr::Int(layout::STATE_RETIRED)),
                                        ),
                                        line,
                                    },
                                    Stmt::Assign {
                                        kind: vm::AssignmentKind::Set,
                                        index: rdn,
                                        expr: header_cell(
                                            helper_read,
                                            param_carrier,
                                            layout::HDR_RETIRED_SLOTS,
                                        ),
                                        line,
                                    },
                                    Stmt::Assign {
                                        kind: vm::AssignmentKind::Set,
                                        index: retired,
                                        expr: decode_int_call(decode, Expr::Var(rdn)),
                                        line,
                                    },
                                    Stmt::Assign {
                                        kind: vm::AssignmentKind::Set,
                                        index: sink,
                                        expr: write_call(
                                            helper_write,
                                            param_carrier,
                                            Expr::Int(layout::HDR_RETIRED_SLOTS),
                                            Lowerer::as_number(Expr::Add(
                                                Box::new(Expr::Var(retired)),
                                                Box::new(Expr::Int(1)),
                                            )),
                                        ),
                                        line,
                                    },
                                ],
                                else_branch: vec![
                                    Stmt::Assign {
                                        kind: vm::AssignmentKind::Set,
                                        index: sink,
                                        expr: write_call(
                                            helper_write,
                                            param_carrier,
                                            Expr::Var(addr),
                                            Lowerer::as_number(Expr::Int(layout::STATE_FREE)),
                                        ),
                                        line,
                                    },
                                    Stmt::Assign {
                                        kind: vm::AssignmentKind::Set,
                                        index: rdn,
                                        expr: header_cell(
                                            helper_read,
                                            param_carrier,
                                            layout::HDR_FREE_SLOT_HEAD_PLUS_ONE,
                                        ),
                                        line,
                                    },
                                    Stmt::Assign {
                                        kind: vm::AssignmentKind::Set,
                                        index: freeh,
                                        expr: decode_int_call(decode, Expr::Var(rdn)),
                                        line,
                                    },
                                    Stmt::Assign {
                                        kind: vm::AssignmentKind::Set,
                                        index: sink,
                                        expr: write_call(
                                            helper_write,
                                            param_carrier,
                                            Expr::Add(
                                                Box::new(Expr::Var(addr)),
                                                Box::new(Expr::Int(layout::ROW_FREE_NEXT_PLUS_ONE)),
                                            ),
                                            Lowerer::as_number(Expr::Var(freeh)),
                                        ),
                                        line,
                                    },
                                    Stmt::Assign {
                                        kind: vm::AssignmentKind::Set,
                                        index: sink,
                                        expr: write_call(
                                            helper_write,
                                            param_carrier,
                                            Expr::Int(layout::HDR_FREE_SLOT_HEAD_PLUS_ONE),
                                            Lowerer::as_number(Expr::Add(
                                                Box::new(Expr::Var(s)),
                                                Box::new(Expr::Int(1)),
                                            )),
                                        ),
                                        line,
                                    },
                                ],
                                line,
                            },
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: bstart,
                                expr: Expr::Sub(
                                    Box::new(Expr::Var(base)),
                                    Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
                                ),
                                line,
                            },
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: rdn,
                                expr: read_cell(helper_read, param_carrier, Expr::Var(bstart)),
                                line,
                            },
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: bsize,
                                expr: decode_int_call(decode, Expr::Var(rdn)),
                                line,
                            },
                            assert_stmt(
                                Expr::Eq(Box::new(Expr::Var(bsize)), Box::new(Expr::Var(extent))),
                                line,
                            ),
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: sink,
                                expr: write_call(
                                    helper_write,
                                    param_carrier,
                                    Expr::Add(
                                        Box::new(Expr::Var(bstart)),
                                        Box::new(Expr::Int(layout::RAW_CLASS)),
                                    ),
                                    Lowerer::as_number(Expr::Int(layout::RAW_CLASS_FREE)),
                                ),
                                line,
                            },
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: rdn,
                                expr: header_cell(
                                    helper_read,
                                    param_carrier,
                                    layout::HDR_RAW_FREE_HEAD,
                                ),
                                line,
                            },
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: freeh,
                                expr: decode_int_call(decode, Expr::Var(rdn)),
                                line,
                            },
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: sink,
                                expr: write_call(
                                    helper_write,
                                    param_carrier,
                                    Expr::Add(
                                        Box::new(Expr::Var(bstart)),
                                        Box::new(Expr::Int(layout::RAW_FREE_NEXT)),
                                    ),
                                    Lowerer::as_number(Expr::Var(freeh)),
                                ),
                                line,
                            },
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: sink,
                                expr: write_call(
                                    helper_write,
                                    param_carrier,
                                    Expr::Int(layout::HDR_RAW_FREE_HEAD),
                                    Lowerer::as_number(Expr::Var(bstart)),
                                ),
                                line,
                            },
                            Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: reclaimed,
                                expr: Expr::Add(
                                    Box::new(Expr::Var(reclaimed)),
                                    Box::new(Expr::Int(1)),
                                ),
                                line,
                            },
                        ],
                        else_branch: Vec::new(),
                        line,
                    },
                ],
                else_branch: Vec::new(),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: row,
                expr: Expr::Add(Box::new(Expr::Var(row)), Box::new(Expr::Int(1))),
                line,
            },
            Stmt::IfElse {
                condition: Expr::Eq(
                    Box::new(Expr::Var(row)),
                    Box::new(Expr::Int(layout::PAGE_ROWS)),
                ),
                then_branch: vec![
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: row,
                        expr: Expr::Int(0),
                        line,
                    },
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: page,
                        expr: Expr::Add(Box::new(Expr::Var(page)), Box::new(Expr::Int(1))),
                        line,
                    },
                ],
                else_branch: Vec::new(),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: s,
                expr: Expr::Add(Box::new(Expr::Var(s)), Box::new(Expr::Int(1))),
                line,
            },
        ],
        line,
    });
    // Phase 5: coalesce adjacent free raw blocks and rebuild the free
    // list in address order; system blocks (directory pages, live frames)
    // break runs and are never mistaken for nodes. Then reset the
    // interval counter and record the collection.
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: sink,
        expr: write_call(
            helper_write,
            param_carrier,
            Expr::Int(layout::HDR_RAW_FREE_HEAD),
            Lowerer::as_number(Expr::Int(0)),
        ),
        line,
    });
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: cur,
        expr: Expr::Int(layout::PREFIX_CELLS),
        line,
    });
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: rdn,
        expr: header_cell(helper_read, param_carrier, layout::HDR_HEAP_END),
        line,
    });
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: end,
        expr: decode_int_call(decode, Expr::Var(rdn)),
        line,
    });
    body.push(Stmt::While {
        condition: Expr::Lt(Box::new(Expr::Var(cur)), Box::new(Expr::Var(end))),
        body: vec![
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: rdn,
                expr: read_cell(helper_read, param_carrier, Expr::Var(cur)),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: sz,
                expr: decode_int_call(decode, Expr::Var(rdn)),
                line,
            },
            assert_stmt(
                Expr::And(
                    Box::new(Expr::Or(
                        Box::new(Expr::Gt(
                            Box::new(Expr::Var(sz)),
                            Box::new(Expr::Int(layout::MIN_RAW_BLOCK_CELLS)),
                        )),
                        Box::new(Expr::Eq(
                            Box::new(Expr::Var(sz)),
                            Box::new(Expr::Int(layout::MIN_RAW_BLOCK_CELLS)),
                        )),
                    )),
                    Box::new(Expr::Or(
                        Box::new(Expr::Lt(
                            Box::new(Expr::Add(Box::new(Expr::Var(cur)), Box::new(Expr::Var(sz)))),
                            Box::new(Expr::Var(end)),
                        )),
                        Box::new(Expr::Eq(
                            Box::new(Expr::Add(Box::new(Expr::Var(cur)), Box::new(Expr::Var(sz)))),
                            Box::new(Expr::Var(end)),
                        )),
                    )),
                ),
                line,
            ),
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: rdn,
                expr: read_cell(
                    helper_read,
                    param_carrier,
                    Expr::Add(
                        Box::new(Expr::Var(cur)),
                        Box::new(Expr::Int(layout::RAW_CLASS)),
                    ),
                ),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: cls,
                expr: decode_int_call(decode, Expr::Var(rdn)),
                line,
            },
            Stmt::IfElse {
                condition: Expr::Eq(
                    Box::new(Expr::Var(cls)),
                    Box::new(Expr::Int(layout::RAW_CLASS_FREE)),
                ),
                then_branch: vec![Stmt::IfElse {
                    condition: Expr::And(
                        Box::new(Expr::Not(Box::new(Expr::Eq(
                            Box::new(Expr::Var(prev)),
                            Box::new(Expr::Int(-1)),
                        )))),
                        Box::new(Expr::Eq(
                            Box::new(Expr::Add(
                                Box::new(Expr::Var(prev)),
                                Box::new(Expr::Var(prev_size)),
                            )),
                            Box::new(Expr::Var(cur)),
                        )),
                    ),
                    then_branch: vec![
                        Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: prev_size,
                            expr: Expr::Add(
                                Box::new(Expr::Var(prev_size)),
                                Box::new(Expr::Var(sz)),
                            ),
                            line,
                        },
                        Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: sink,
                            expr: write_call(
                                helper_write,
                                param_carrier,
                                Expr::Var(prev),
                                Lowerer::as_number(Expr::Var(prev_size)),
                            ),
                            line,
                        },
                    ],
                    else_branch: vec![
                        Stmt::IfElse {
                            condition: Expr::Eq(Box::new(Expr::Var(prev)), Box::new(Expr::Int(-1))),
                            then_branch: vec![Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: sink,
                                expr: write_call(
                                    helper_write,
                                    param_carrier,
                                    Expr::Int(layout::HDR_RAW_FREE_HEAD),
                                    Lowerer::as_number(Expr::Var(cur)),
                                ),
                                line,
                            }],
                            else_branch: vec![Stmt::Assign {
                                kind: vm::AssignmentKind::Set,
                                index: sink,
                                expr: write_call(
                                    helper_write,
                                    param_carrier,
                                    Expr::Add(
                                        Box::new(Expr::Var(prev)),
                                        Box::new(Expr::Int(layout::RAW_FREE_NEXT)),
                                    ),
                                    Lowerer::as_number(Expr::Var(cur)),
                                ),
                                line,
                            }],
                            line,
                        },
                        Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: sink,
                            expr: write_call(
                                helper_write,
                                param_carrier,
                                Expr::Add(
                                    Box::new(Expr::Var(cur)),
                                    Box::new(Expr::Int(layout::RAW_FREE_NEXT)),
                                ),
                                Lowerer::as_number(Expr::Int(0)),
                            ),
                            line,
                        },
                        Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: prev,
                            expr: Expr::Var(cur),
                            line,
                        },
                        Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: prev_size,
                            expr: Expr::Var(sz),
                            line,
                        },
                    ],
                    line,
                }],
                // Keep the last free block across intervening live/system
                // blocks. Adjacency is checked separately above; clearing
                // this chain tail would replace the global head at each gap.
                else_branch: Vec::new(),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: cur,
                expr: Expr::Add(Box::new(Expr::Var(cur)), Box::new(Expr::Var(sz))),
                line,
            },
        ],
        line,
    });
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: sink,
        expr: write_call(
            helper_write,
            param_carrier,
            Expr::Int(layout::HDR_ALLOCATIONS_SINCE_GC),
            Lowerer::as_number(Expr::Int(0)),
        ),
        line,
    });
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: rdn,
        expr: header_cell(helper_read, param_carrier, layout::HDR_COLLECTIONS),
        line,
    });
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: coll,
        expr: decode_int_call(decode, Expr::Var(rdn)),
        line,
    });
    body.push(Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: sink,
        expr: write_call(
            helper_write,
            param_carrier,
            Expr::Int(layout::HDR_COLLECTIONS),
            Lowerer::as_number(Expr::Add(Box::new(Expr::Var(coll)), Box::new(Expr::Int(1)))),
        ),
        line,
    });

    declare_helper(
        ir,
        statements,
        layout::HEAP_COLLECT_HELPER,
        index,
        1,
        vec!["carrier".to_owned()],
        vec![Some(carrier_schema.clone())],
        Some(TypeSchema::Int),
        ValueType::Int,
        vec![param_carrier],
        body,
        Expr::Var(reclaimed),
        line,
    );
    Ok(index)
}

/// Append the metadata observer `(Carrier, which: Int, slot: Int) -> Int`.
/// Reads real numeric header, directory, frame and backing metadata while
/// the owner lives. Global counters ignore `slot`; per-row selectors return
/// -1 for an absent page or out-of-range slot instead of failing.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_heap_observe(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &TypeSchema,
    helper_read: u16,
    helper_length: u16,
    decode: u16,
    decode_u32: u16,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let index = fresh_index(lower, ir, span)?;
    let line = lower.line(span);
    let param_carrier = lower.slot(span)?;
    let param_which = lower.slot(span)?;
    let param_slot = lower.slot(span)?;
    let rdn = lower.slot(span)?;
    let res = lower.slot(span)?;
    let s = lower.slot(span)?;
    let p = lower.slot(span)?;
    let r = lower.slot(span)?;
    let pp = lower.slot(span)?;
    let a = lower.slot(span)?;
    let t = lower.slot(span)?;
    let f = lower.slot(span)?;
    let cp = lower.slot(span)?;
    let e = lower.slot(span)?;
    let ct = lower.slot(span)?;
    let next = lower.slot(span)?;
    let mut body = Vec::new();
    for (temp, schema, init) in [
        (rdn, TypeSchema::Number, Expr::Float(0.0)),
        (res, TypeSchema::Int, Expr::Int(0)),
        (s, TypeSchema::Int, Expr::Int(0)),
        (p, TypeSchema::Int, Expr::Int(0)),
        (r, TypeSchema::Int, Expr::Int(0)),
        (pp, TypeSchema::Int, Expr::Int(0)),
        (a, TypeSchema::Int, Expr::Int(0)),
        (t, TypeSchema::Int, Expr::Int(0)),
        (f, TypeSchema::Int, Expr::Int(0)),
        (cp, TypeSchema::Int, Expr::Int(0)),
        (e, TypeSchema::Int, Expr::Int(0)),
        (ct, TypeSchema::Int, Expr::Int(0)),
        (next, TypeSchema::Int, Expr::Int(0)),
    ] {
        body.push(Stmt::Let {
            index: temp,
            declared_schema: Some(schema),
            expr: init,
            line,
        });
    }
    // One header counter into `res`.
    let counter_arm = |offset: i64| -> Vec<Stmt> {
        vec![
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: rdn,
                expr: header_cell(helper_read, param_carrier, offset),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: res,
                expr: decode_int_call(decode, Expr::Var(rdn)),
                line,
            },
        ]
    };
    // Backing length through the real B1 helper.
    let length_arm = vec![Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: res,
        expr: decode_int_call(
            decode,
            Lowerer::guest_call(helper_length, vec![Expr::Var(param_carrier)]),
        ),
        line,
    }];
    // Free-slot scan.
    let mut free_arm = vec![
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: rdn,
            expr: header_cell(helper_read, param_carrier, layout::HDR_NEXT_SLOT),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: next,
            expr: decode_int_call(decode, Expr::Var(rdn)),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: s,
            expr: Expr::Int(0),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: p,
            expr: Expr::Int(0),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: r,
            expr: Expr::Int(0),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: res,
            expr: Expr::Int(0),
            line,
        },
    ];
    free_arm.push(Stmt::While {
        condition: Expr::Lt(
            Box::new(Lowerer::as_number(Expr::Var(s))),
            Box::new(Lowerer::as_number(Expr::Var(next))),
        ),
        body: vec![
            Stmt::IfElse {
                condition: Expr::Eq(Box::new(Expr::Var(r)), Box::new(Expr::Int(0))),
                then_branch: vec![
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: rdn,
                        expr: read_cell(
                            helper_read,
                            param_carrier,
                            Expr::Add(
                                Box::new(Expr::Int(layout::HEADER_CELLS)),
                                Box::new(Expr::Var(p)),
                            ),
                        ),
                        line,
                    },
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: pp,
                        expr: decode_int_call(decode, Expr::Var(rdn)),
                        line,
                    },
                ],
                else_branch: Vec::new(),
                line,
            },
            Stmt::IfElse {
                condition: Expr::Not(Box::new(Expr::Eq(
                    Box::new(Expr::Var(pp)),
                    Box::new(Expr::Int(0)),
                ))),
                then_branch: vec![
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: a,
                        expr: Expr::Add(
                            Box::new(Expr::Add(
                                Box::new(Expr::Var(pp)),
                                Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
                            )),
                            Box::new(Expr::Mul(
                                Box::new(Expr::Var(r)),
                                Box::new(Expr::Int(layout::ROW_CELLS)),
                            )),
                        ),
                        line,
                    },
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: t,
                        expr: decode_int_call(
                            decode,
                            read_cell(helper_read, param_carrier, Expr::Var(a)),
                        ),
                        line,
                    },
                    Stmt::IfElse {
                        condition: Expr::Eq(
                            Box::new(Expr::Var(t)),
                            Box::new(Expr::Int(layout::STATE_FREE)),
                        ),
                        then_branch: vec![Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: res,
                            expr: Expr::Add(Box::new(Expr::Var(res)), Box::new(Expr::Int(1))),
                            line,
                        }],
                        else_branch: Vec::new(),
                        line,
                    },
                ],
                else_branch: Vec::new(),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: r,
                expr: Expr::Add(Box::new(Expr::Var(r)), Box::new(Expr::Int(1))),
                line,
            },
            Stmt::IfElse {
                condition: Expr::Eq(
                    Box::new(Expr::Var(r)),
                    Box::new(Expr::Int(layout::PAGE_ROWS)),
                ),
                then_branch: vec![
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: r,
                        expr: Expr::Int(0),
                        line,
                    },
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: p,
                        expr: Expr::Add(Box::new(Expr::Var(p)), Box::new(Expr::Int(1))),
                        line,
                    },
                ],
                else_branch: Vec::new(),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: s,
                expr: Expr::Add(Box::new(Expr::Var(s)), Box::new(Expr::Int(1))),
                line,
            },
        ],
        line,
    });
    // Active-root scan across frames.
    let mut roots_arm = vec![
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: rdn,
            expr: header_cell(helper_read, param_carrier, layout::HDR_ROOT_TOP),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: f,
            expr: decode_int_call(decode, Expr::Var(rdn)),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: res,
            expr: Expr::Int(0),
            line,
        },
    ];
    roots_arm.push(Stmt::While {
        condition: Expr::Not(Box::new(Expr::Eq(
            Box::new(Expr::Var(f)),
            Box::new(Expr::Int(0)),
        ))),
        body: vec![
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: cp,
                expr: decode_int_call(
                    decode,
                    read_cell(
                        helper_read,
                        param_carrier,
                        Expr::Add(
                            Box::new(Expr::Var(f)),
                            Box::new(Expr::Int(layout::RAW_HEADER_CELLS)),
                        ),
                    ),
                ),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: e,
                expr: Expr::Int(0),
                line,
            },
            Stmt::While {
                condition: Expr::Lt(Box::new(Expr::Var(e)), Box::new(Expr::Var(cp))),
                body: vec![
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: ct,
                        expr: decode_int_call(
                            decode,
                            read_cell(
                                helper_read,
                                param_carrier,
                                Expr::Add(
                                    Box::new(Expr::Add(
                                        Box::new(Expr::Var(f)),
                                        Box::new(Expr::Int(layout::FRAME_FIXED_CELLS)),
                                    )),
                                    Box::new(Expr::Mul(
                                        Box::new(Expr::Var(e)),
                                        Box::new(Expr::Int(layout::FRAME_CELLS_PER_ENTRY)),
                                    )),
                                ),
                            ),
                        ),
                        line,
                    },
                    Stmt::IfElse {
                        condition: Expr::Not(Box::new(Expr::Eq(
                            Box::new(Expr::Var(ct)),
                            Box::new(Expr::Int(0)),
                        ))),
                        then_branch: vec![Stmt::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: res,
                            expr: Expr::Add(Box::new(Expr::Var(res)), Box::new(Expr::Int(1))),
                            line,
                        }],
                        else_branch: Vec::new(),
                        line,
                    },
                    Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: e,
                        expr: Expr::Add(Box::new(Expr::Var(e)), Box::new(Expr::Int(1))),
                        line,
                    },
                ],
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: f,
                expr: decode_int_call(
                    decode,
                    read_cell(
                        helper_read,
                        param_carrier,
                        Expr::Add(
                            Box::new(Expr::Var(f)),
                            Box::new(Expr::Int(layout::RAW_FREE_NEXT)),
                        ),
                    ),
                ),
                line,
            },
        ],
        line,
    });
    // Per-row selectors with a soft lookup; absent pages read as -1.
    let row_arm = |field: i64, is_gen: bool| -> Vec<Stmt> {
        let mut arm = vec![
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: rdn,
                expr: header_cell(helper_read, param_carrier, layout::HDR_NEXT_SLOT),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: next,
                expr: decode_int_call(decode, Expr::Var(rdn)),
                line,
            },
        ];
        arm.push(Stmt::IfElse {
            condition: Expr::Or(
                Box::new(Expr::Lt(
                    Box::new(Expr::Var(param_slot)),
                    Box::new(Expr::Int(0)),
                )),
                Box::new(Expr::Or(
                    Box::new(Expr::Gt(
                        Box::new(Expr::Var(param_slot)),
                        Box::new(Expr::Var(next)),
                    )),
                    Box::new(Expr::Eq(
                        Box::new(Expr::Var(param_slot)),
                        Box::new(Expr::Var(next)),
                    )),
                )),
            ),
            then_branch: vec![Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: res,
                expr: Expr::Int(-1),
                line,
            }],
            else_branch: Vec::new(),
            line,
        });
        emit_row_lookup(
            helper_read,
            decode,
            param_carrier,
            param_slot,
            line,
            &mut arm,
            false,
            p,
            r,
            a,
            rdn,
            pp,
            false,
        );
        arm.push(Stmt::IfElse {
            condition: Expr::Eq(Box::new(Expr::Var(a)), Box::new(Expr::Int(0))),
            then_branch: vec![Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: res,
                expr: Expr::Int(-1),
                line,
            }],
            else_branch: vec![Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: res,
                expr: if is_gen {
                    decode_gen_call(
                        decode_u32,
                        read_cell(
                            helper_read,
                            param_carrier,
                            Expr::Add(Box::new(Expr::Var(a)), Box::new(Expr::Int(field))),
                        ),
                    )
                } else {
                    decode_int_call(
                        decode,
                        read_cell(
                            helper_read,
                            param_carrier,
                            Expr::Add(Box::new(Expr::Var(a)), Box::new(Expr::Int(field))),
                        ),
                    )
                },
                line,
            }],
            line,
        });
        arm
    };
    // Fold the selector chain from the default (fail closed) backwards.
    let mut tail: Vec<Stmt> = vec![assert_stmt(Expr::Bool(false), line)];
    let arms: Vec<(i64, Vec<Stmt>)> = vec![
        (
            layout::OBSERVE_LIVE_NODES,
            counter_arm(layout::HDR_LIVE_NODES),
        ),
        (
            layout::OBSERVE_RETIRED_SLOTS,
            counter_arm(layout::HDR_RETIRED_SLOTS),
        ),
        (
            layout::OBSERVE_NEXT_SLOT,
            counter_arm(layout::HDR_NEXT_SLOT),
        ),
        (layout::OBSERVE_HEAP_END, counter_arm(layout::HDR_HEAP_END)),
        (
            layout::OBSERVE_ACTIVE_ROOT_CAPACITY,
            counter_arm(layout::HDR_ACTIVE_ROOT_CAPACITY),
        ),
        (
            layout::OBSERVE_COLLECTIONS,
            counter_arm(layout::HDR_COLLECTIONS),
        ),
        (
            layout::OBSERVE_ALLOCATIONS_SINCE_GC,
            counter_arm(layout::HDR_ALLOCATIONS_SINCE_GC),
        ),
        (
            layout::OBSERVE_NODE_CELLS,
            counter_arm(layout::HDR_NODE_CELLS),
        ),
        (
            layout::OBSERVE_PENDING_NODES,
            counter_arm(layout::HDR_PENDING_NODES),
        ),
        (
            layout::OBSERVE_DIRECTORY_PAGES,
            counter_arm(layout::HDR_DIRECTORY_PAGES),
        ),
        (layout::OBSERVE_BACKING_LENGTH, length_arm),
        (layout::OBSERVE_FREE_SLOTS, free_arm),
        (layout::OBSERVE_ACTIVE_ROOTS, roots_arm),
        (layout::OBSERVE_ROW_STATE, row_arm(layout::ROW_STATE, false)),
        (
            layout::OBSERVE_ROW_GENERATION,
            row_arm(layout::ROW_GENERATION, true),
        ),
    ];
    for (code, arm) in arms.into_iter().rev() {
        tail = vec![Stmt::IfElse {
            condition: Expr::Eq(Box::new(Expr::Var(param_which)), Box::new(Expr::Int(code))),
            then_branch: arm,
            else_branch: tail,
            line,
        }];
    }
    body.extend(tail);
    declare_helper(
        ir,
        statements,
        layout::HEAP_OBSERVE_HELPER,
        index,
        3,
        vec!["carrier".to_owned(), "which".to_owned(), "slot".to_owned()],
        vec![
            Some(carrier_schema.clone()),
            Some(TypeSchema::Int),
            Some(TypeSchema::Int),
        ],
        Some(TypeSchema::Int),
        ValueType::Int,
        vec![param_carrier, param_which, param_slot],
        body,
        Expr::Var(res),
        line,
    );
    Ok(index)
}

/// Append the prototype setter `(Carrier, obj: JsValue, proto: JsValue)`.
/// Resolves and kind-checks both object-like nodes, walks the prototype
/// chain from the candidate (bounded by the directory high-water, failing
/// closed on corruption or foreign cycles) and refuses self/indirect cycles
/// with a 0 acknowledgment leaving the old pair unchanged; a valid link
/// writes the pair and returns 1. Type violations fail closed.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_set_prototype(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &TypeSchema,
    helper_read: u16,
    helper_write: u16,
    decode: u16,
    decode_u32: u16,
    checked_base: u16,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let index = fresh_index(lower, ir, span)?;
    let line = lower.line(span);
    let param_carrier = lower.slot(span)?;
    let param_obj = lower.slot(span)?;
    let param_proto = lower.slot(span)?;
    let oslot = lower.slot(span)?;
    let ogen = lower.slot(span)?;
    let pslot = lower.slot(span)?;
    let pgen = lower.slot(span)?;
    let obase = lower.slot(span)?;
    let pbase = lower.slot(span)?;
    let kind_num = lower.slot(span)?;
    let kind = lower.slot(span)?;
    let cs = lower.slot(span)?;
    let cg = lower.slot(span)?;
    let steps = lower.slot(span)?;
    let next_num = lower.slot(span)?;
    let next = lower.slot(span)?;
    let cycled = lower.slot(span)?;
    let cb = lower.slot(span)?;
    let rdn = lower.slot(span)?;
    let sink = lower.slot(span)?;
    let js_schema = value::schema();
    let mut body = vec![
        Stmt::Let {
            index: sink,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: rdn,
            declared_schema: Some(TypeSchema::Number),
            expr: Expr::Float(0.0),
            line,
        },
        Stmt::Let {
            index: oslot,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field(param_obj, "ref_slot"),
            line,
        },
        Stmt::Let {
            index: ogen,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field(param_obj, "generation"),
            line,
        },
        Stmt::Let {
            index: pslot,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field(param_proto, "ref_slot"),
            line,
        },
        Stmt::Let {
            index: pgen,
            declared_schema: Some(TypeSchema::Int),
            expr: value::field(param_proto, "generation"),
            line,
        },
        Stmt::Let {
            index: obase,
            declared_schema: Some(TypeSchema::Int),
            expr: Lowerer::guest_call(
                checked_base,
                vec![Expr::Var(param_carrier), Expr::Var(oslot), Expr::Var(ogen)],
            ),
            line,
        },
        Stmt::Let {
            index: pbase,
            declared_schema: Some(TypeSchema::Int),
            expr: Lowerer::guest_call(
                checked_base,
                vec![Expr::Var(param_carrier), Expr::Var(pslot), Expr::Var(pgen)],
            ),
            line,
        },
    ];
    // Both ends must be object-like (array, plain object, function):
    // primitive strings and internal environments never link here.
    for (base, name) in [(obase, "object"), (pbase, "prototype")] {
        let _ = name;
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
            Expr::Or(
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(kind)),
                    Box::new(Expr::Int(layout::KIND_ARRAY)),
                )),
                Box::new(Expr::Or(
                    Box::new(Expr::Eq(
                        Box::new(Expr::Var(kind)),
                        Box::new(Expr::Int(layout::KIND_OBJECT)),
                    )),
                    Box::new(Expr::Eq(
                        Box::new(Expr::Var(kind)),
                        Box::new(Expr::Int(layout::KIND_FUNCTION)),
                    )),
                )),
            ),
            line,
        ));
    }
    body.push(Stmt::Let {
        index: next_num,
        declared_schema: Some(TypeSchema::Number),
        expr: header_cell(helper_read, param_carrier, layout::HDR_NEXT_SLOT),
        line,
    });
    body.push(Stmt::Let {
        index: next,
        declared_schema: Some(TypeSchema::Int),
        expr: decode_int_call(decode, Expr::Var(next_num)),
        line,
    });
    body.push(Stmt::Let {
        index: cs,
        declared_schema: Some(TypeSchema::Int),
        expr: Expr::Var(pslot),
        line,
    });
    body.push(Stmt::Let {
        index: cg,
        declared_schema: Some(TypeSchema::Int),
        expr: Expr::Var(pgen),
        line,
    });
    body.push(Stmt::Let {
        index: steps,
        declared_schema: Some(TypeSchema::Int),
        expr: Expr::Int(0),
        line,
    });
    body.push(Stmt::Let {
        index: cycled,
        declared_schema: Some(TypeSchema::Int),
        expr: Expr::Int(0),
        line,
    });
    body.push(Stmt::While {
        condition: Expr::And(
            Box::new(Expr::And(
                Box::new(Expr::Not(Box::new(Expr::Eq(
                    Box::new(Expr::Var(cg)),
                    Box::new(Expr::Int(0)),
                )))),
                Box::new(Expr::Or(
                    Box::new(Expr::Lt(
                        Box::new(Expr::Var(steps)),
                        Box::new(Expr::Var(next)),
                    )),
                    Box::new(Expr::Eq(
                        Box::new(Expr::Var(steps)),
                        Box::new(Expr::Var(next)),
                    )),
                )),
            )),
            Box::new(Expr::Eq(
                Box::new(Expr::Var(cycled)),
                Box::new(Expr::Int(0)),
            )),
        ),
        body: vec![Stmt::IfElse {
            condition: Expr::And(
                Box::new(Expr::Eq(
                    Box::new(Expr::Var(cs)),
                    Box::new(Expr::Var(oslot)),
                )),
                Box::new(Expr::Eq(Box::new(Expr::Var(cg)), Box::new(Expr::Var(ogen)))),
            ),
            then_branch: vec![Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: cycled,
                expr: Expr::Int(1),
                line,
            }],
            else_branch: vec![
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: cb,
                    expr: Lowerer::guest_call(
                        checked_base,
                        vec![Expr::Var(param_carrier), Expr::Var(cs), Expr::Var(cg)],
                    ),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: rdn,
                    expr: read_cell(helper_read, param_carrier, Expr::Var(cb)),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: kind,
                    expr: decode_int_call(decode, Expr::Var(rdn)),
                    line,
                },
                assert_stmt(
                    Expr::Or(
                        Box::new(Expr::Eq(
                            Box::new(Expr::Var(kind)),
                            Box::new(Expr::Int(layout::KIND_ARRAY)),
                        )),
                        Box::new(Expr::Or(
                            Box::new(Expr::Eq(
                                Box::new(Expr::Var(kind)),
                                Box::new(Expr::Int(layout::KIND_OBJECT)),
                            )),
                            Box::new(Expr::Eq(
                                Box::new(Expr::Var(kind)),
                                Box::new(Expr::Int(layout::KIND_FUNCTION)),
                            )),
                        )),
                    ),
                    line,
                ),
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: rdn,
                    expr: read_cell(
                        helper_read,
                        param_carrier,
                        Expr::Add(Box::new(Expr::Var(cb)), Box::new(Expr::Int(3))),
                    ),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: cs,
                    expr: decode_int_call(decode, Expr::Var(rdn)),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: rdn,
                    expr: read_cell(
                        helper_read,
                        param_carrier,
                        Expr::Add(Box::new(Expr::Var(cb)), Box::new(Expr::Int(4))),
                    ),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: cg,
                    expr: decode_gen_call(decode_u32, Expr::Var(rdn)),
                    line,
                },
                Stmt::Assign {
                    kind: vm::AssignmentKind::Set,
                    index: steps,
                    expr: Expr::Add(Box::new(Expr::Var(steps)), Box::new(Expr::Int(1))),
                    line,
                },
            ],
            line,
        }],
        line,
    });
    // A refused cycle leaves the old pair unchanged; a terminated chain
    // (null generation) links the new pair, while an overrun chain fails
    // closed as corruption.
    body.push(Stmt::IfElse {
        condition: Expr::Eq(Box::new(Expr::Var(cycled)), Box::new(Expr::Int(1))),
        then_branch: Vec::new(),
        else_branch: vec![
            assert_stmt(
                Expr::Eq(Box::new(Expr::Var(cg)), Box::new(Expr::Int(0))),
                line,
            ),
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: sink,
                expr: write_call(
                    helper_write,
                    param_carrier,
                    Expr::Add(Box::new(Expr::Var(obase)), Box::new(Expr::Int(3))),
                    Lowerer::as_number(Expr::Var(pslot)),
                ),
                line,
            },
            Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: sink,
                expr: write_call(
                    helper_write,
                    param_carrier,
                    Expr::Add(Box::new(Expr::Var(obase)), Box::new(Expr::Int(4))),
                    Lowerer::as_number(Expr::Var(pgen)),
                ),
                line,
            },
        ],
        line,
    });
    // `cb` is assigned on every chain step above; pre-declare it here.
    body.insert(
        2,
        Stmt::Let {
            index: cb,
            declared_schema: Some(TypeSchema::Int),
            expr: Expr::Int(0),
            line,
        },
    );
    declare_helper(
        ir,
        statements,
        layout::SET_PROTOTYPE_HELPER,
        index,
        3,
        vec![
            "carrier".to_owned(),
            "object".to_owned(),
            "prototype".to_owned(),
        ],
        vec![
            Some(carrier_schema.clone()),
            Some(js_schema.clone()),
            Some(js_schema),
        ],
        Some(TypeSchema::Int),
        ValueType::Int,
        vec![param_carrier, param_obj, param_proto],
        body,
        Expr::IfElse {
            condition: Box::new(Expr::Eq(
                Box::new(Expr::Var(cycled)),
                Box::new(Expr::Int(1)),
            )),
            then_expr: Box::new(Expr::Int(0)),
            else_expr: Box::new(Expr::Int(1)),
        },
        line,
    );
    Ok(index)
}

/// Append every heap-core helper in dependency order and return the flat
/// indices producers and consumers share in this scope.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_heap_core(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    helper_make: u16,
    helper_read: u16,
    helper_write: u16,
    helper_length: u16,
    helper_resize: u16,
    decode_int: u16,
    span: swc_common::Span,
    statements: &mut Vec<Stmt>,
) -> Result<HeapHelpers, ParseError> {
    let carrier_schema = carrier_schema(lower, ir, helper_make, span)?;
    let decode_u32 = append_decode_u32(lower, ir, span, statements)?;
    let checked_base = append_checked_base(
        lower,
        ir,
        &carrier_schema,
        helper_read,
        decode_int,
        decode_u32,
        span,
        statements,
    )?;
    let mark = append_mark(
        lower,
        ir,
        &carrier_schema,
        helper_read,
        helper_write,
        decode_int,
        decode_u32,
        span,
        statements,
    )?;
    let collect = append_heap_collect(
        lower,
        ir,
        &carrier_schema,
        helper_read,
        helper_write,
        decode_int,
        decode_u32,
        mark,
        span,
        statements,
    )?;
    let raw_reserve = append_raw_reserve(
        lower,
        ir,
        &carrier_schema,
        helper_read,
        helper_write,
        helper_resize,
        decode_int,
        collect,
        span,
        statements,
    )?;
    let root_enter = append_root_enter(
        lower,
        ir,
        &carrier_schema,
        helper_read,
        helper_write,
        decode_int,
        collect,
        raw_reserve,
        span,
        statements,
    )?;
    let root_write = append_root_write(
        lower,
        ir,
        &carrier_schema,
        helper_read,
        helper_write,
        decode_int,
        span,
        statements,
    )?;
    let root_write_node = append_root_write_node(
        lower,
        ir,
        &carrier_schema,
        helper_read,
        helper_write,
        decode_int,
        checked_base,
        span,
        statements,
    )?;
    let root_leave = append_root_leave(
        lower,
        ir,
        &carrier_schema,
        helper_read,
        helper_write,
        decode_int,
        span,
        statements,
    )?;
    let node_begin = append_node_begin(
        lower,
        ir,
        &carrier_schema,
        helper_read,
        helper_write,
        helper_resize,
        decode_int,
        decode_u32,
        collect,
        raw_reserve,
        span,
        statements,
    )?;
    let node_publish = append_node_publish(
        lower,
        ir,
        &carrier_schema,
        helper_read,
        helper_write,
        decode_int,
        decode_u32,
        span,
        statements,
    )?;
    let node_publish_edge = append_node_publish_edge(
        lower,
        ir,
        &carrier_schema,
        helper_read,
        helper_write,
        decode_int,
        decode_u32,
        span,
        statements,
    )?;
    let heap_observe = append_heap_observe(
        lower,
        ir,
        &carrier_schema,
        helper_read,
        helper_length,
        decode_int,
        decode_u32,
        span,
        statements,
    )?;
    let set_prototype = append_set_prototype(
        lower,
        ir,
        &carrier_schema,
        helper_read,
        helper_write,
        decode_int,
        decode_u32,
        checked_base,
        span,
        statements,
    )?;
    Ok(HeapHelpers {
        decode_u32,
        root_enter,
        root_write,
        root_write_node,
        root_leave,
        checked_base,
        mark,
        raw_reserve,
        node_begin,
        node_publish,
        node_publish_edge,
        heap_collect: collect,
        heap_observe,
        set_prototype,
    })
}
