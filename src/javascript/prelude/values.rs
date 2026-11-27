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
use vm::{BuiltinFunction as B, Expr, FrontendIr, ParseError, Stmt, TypeSchema, ValueType};

/// Read one stored UTF-16 unit through an already-checked base: physical
/// cell `base + 3 + index`, normalized to Number like every other read.
pub(crate) fn read_string_unit(helper_read: u16, carrier: u16, base: u16, index: u16) -> Expr {
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

/// Finite source/intrinsic dispatch with the physical four-argument ABI.
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
    function_prototype_api: Option<u16>,
) -> Result<u16, ParseError> {
    use super::object::*;
    let index = fresh_index(lower, ir, span)?;
    // The UTF-16 initializer pool is already armed at dispatch+1 and may claim
    // the next POOL_LIMIT indices. The repeat helper sits just past that range.
    let repeat = index
        .checked_add(1)
        .and_then(|next| next.checked_add(super::utf16_init::POOL_LIMIT))
        .ok_or_else(|| lower.error(span, "string repeat helper index"))?;
    super::string_repeat::append(lower, ir, carrier_schema, span, statements, repeat)?;
    let from_code = if lower.string_namespace_used.get() {
        let from_code = repeat
            .checked_add(1)
            .ok_or_else(|| lower.error(span, "string fromCharCode helper index"))?;
        super::string_from_code::append(lower, ir, carrier_schema, span, statements, from_code)?;
        Some(from_code)
    } else {
        None
    };
    // `String(value)` and `Number.prototype.toString(radix)` own the two
    // helper indices after the FromCode helper. Each is appended only for a
    // source that pays for it, so an unreferenced realm keeps the exact
    // previous index layout.
    let after_from_code = from_code.map_or(repeat + 1, |code| code + 1);
    let string_call = if lower.string_namespace_used.get() {
        super::string_call::append(lower, ir, carrier_schema, span, statements, after_from_code)?;
        Some(after_from_code)
    } else {
        None
    };
    let number_to_string = if lower.number_to_string_used.get() {
        let index = string_call.map_or(after_from_code, |code| code + 1);
        super::number_proto_string::append(lower, ir, carrier_schema, span, statements, index)?;
        Some(index)
    } else {
        None
    };
    let array_constructor = if lower.array_constructor_used.get() {
        Some(super::array_constructor::append(lower, ir, carrier_schema, span, statements)?)
    } else {
        None
    };
    let line = lower.line(span);
    let carrier = lower.slot(span)?;
    let function = lower.slot(span)?;
    let receiver = lower.slot(span)?;
    let args = lower.slot(span)?;
    let objects = lower.objects.expect("object runtime");
    let intrinsic = lower.intrinsics.expect("intrinsics");
    // The RangeError rejection path writes the message row through the heap
    // write helper; every other dispatch arm still only reads or calls.
    let helper_write = lower.heap_ref(span)?.helper_write;
    let mut e = Emit {
        lower,
        span,
        line,
        declarations: Vec::new(),
        carrier,
        read: helper_read,
        write: helper_write,
        decode,
        decode_u32,
        checked_base,
        number_write_sink: None,
        bootstrap_row_suffix: None,
        bootstrap_ascii_init: None,
    };
    let base = e.integer()?;
    let code = e.integer()?;
    let env = e.integer()?;
    let generation = e.integer()?;
    let valid = e.boolean()?;
    let source_span = e.lower.ir_span(span);
    let out = e.temp(
        value::logical_result_schema(),
        value::result_ok(value::completion_normal(), source_span),
    )?;
    let mut ordinary = vec![
        assign(base, e.base(var(function)), line),
        assign(
            valid,
            eq(e.decoded(var(base)), int(layout::KIND_FUNCTION)),
            line,
        ),
    ];
    let mut callable = vec![
        assign(code, e.decoded(add(var(base), int(5))), line),
        assign(env, e.decoded(add(var(base), int(6))), line),
        assign(generation, e.generation(add(var(base), int(7))), line),
        assert_stmt(
            or(not(eq(var(generation), int(0))), eq(var(env), int(0))),
            line,
        ),
    ];
    let checked = e.integer()?;
    if e.lower.globals.shared {
        callable.push(assert_stmt(
            and(not(lt(var(env), int(0))), lt(var(env), int(4096))),
            line,
        ));
    }
    callable.push(branch(
        not(eq(var(generation), int(0))),
        vec![
            assign(
                checked,
                call(checked_base, vec![var(carrier), var(env), var(generation)]),
                line,
            ),
            assert_stmt(
                eq(e.decoded(var(checked)), int(layout::KIND_ENVIRONMENT)),
                line,
            ),
        ],
        Vec::new(),
        line,
    ));
    let mut tail = vec![assert_stmt(Expr::Bool(false), line)];
    for (id, function) in func_indices.iter().enumerate().rev() {
        tail = vec![branch(
            eq(var(code), int(id as i64)),
            vec![assign(
                out,
                call(
                    *function,
                    vec![
                        var(carrier),
                        if let Some(global) = e.lower.global_helpers {
                            call(global.pack, vec![var(carrier), var(env), var(generation)])
                        } else {
                            var(env)
                        },
                        var(receiver),
                        var(args),
                        int(0),
                    ],
                ),
                line,
            )],
            tail,
            line,
        )];
    }
    // Public TypeError's code uses the private zero-row allocator only after
    // verifying that the actual argument vector is empty.
    tail = vec![branch(
        eq(var(code), int(super::intrinsics::TYPE_ERROR_CODE)),
        vec![branch(
            eq(
                call(
                    vm::BuiltinFunction::Len.call_index(),
                    vec![Expr::Borrow(Box::new(var(args)))],
                ),
                int(0),
            ),
            vec![assign(
                out,
                value::result_ok(
                    value::completion_return(call(intrinsic.type_error, vec![var(carrier)])),
                    source_span,
                ),
                line,
            )],
            vec![assign(
                out,
                value::result_err(call(intrinsic.type_error, vec![var(carrier)]), source_span),
                line,
            )],
            line,
        )],
        tail,
        line,
    )];
    if e.lower.range_error_used.get() {
        let range_error = intrinsic.range_error.expect("RangeError allocator");
        tail = vec![branch(
            eq(var(code), int(super::intrinsics::RANGE_ERROR_CODE)),
            vec![branch(
                eq(
                    call(
                        vm::BuiltinFunction::Len.call_index(),
                        vec![Expr::Borrow(Box::new(var(args)))],
                    ),
                    int(0),
                ),
                vec![assign(
                    out,
                    value::result_ok(
                        value::completion_return(call(range_error, vec![var(carrier), int(0)])),
                        source_span,
                    ),
                    line,
                )],
                vec![assign(
                    out,
                    value::result_err(call(range_error, vec![var(carrier), int(0)]), source_span),
                    line,
                )],
                line,
            )],
            tail,
            line,
        )];
    }
    for error in super::intrinsics::OptionalError::selected(e.lower) {
        let allocator = error.allocator(intrinsic).expect("selected Error allocator");
        tail = vec![branch(
            eq(var(code), int(error.code())),
            vec![branch(
                lt(
                    call(vm::BuiltinFunction::Len.call_index(), vec![Expr::Borrow(Box::new(var(args)))]),
                    int(2),
                ),
                vec![assign(
                    out,
                    value::result_ok(
                        value::completion_return(call(allocator, vec![var(carrier), int(0)])),
                        source_span,
                    ),
                    line,
                )],
                vec![assign(
                    out,
                    value::result_err(call(intrinsic.type_error, vec![var(carrier)]), source_span),
                    line,
                )],
                line,
            )],
            tail,
            line,
        )];
    }
    // Receiver-aware Object.prototype methods carry both the real receiver and
    // the real argument container; they never enter the args-only static
    // adapter or any receiver-only numeric adapter. `Error.prototype.toString`
    // is receiver-aware in the same way and joins this list; the adapter
    // performs the Error brand check itself.
    let proto_code = or(
        eq(var(code), int(super::math::OBJECT_VALUE_OF)),
        or(
            eq(var(code), int(super::math::OBJECT_TO_STRING)),
            or(
                eq(var(code), int(super::intrinsics::HAS_OWN_PROPERTY_CODE)),
                eq(var(code), int(super::intrinsics::ERROR_TO_STRING_CODE)),
            ),
        ),
    );
    tail = vec![branch(
        proto_code,
        vec![assign(
            out,
            call(
                objects.proto_call,
                vec![var(carrier), var(code), var(receiver), var(args)],
            ),
            line,
        )],
        tail,
        line,
    )];
    // Admitted `Function.prototype.call`: the function found by ordinary Get
    // on the Function.prototype chain re-enters dispatch with its own saved
    // receiver as the callable and the admitted thisArg as the receiver.
    tail = vec![branch(
        eq(var(code), int(super::intrinsics::FUNCTION_CALL_CODE)),
        vec![assign(
            out,
            call(
                objects.function_call,
                vec![var(carrier), var(code), var(receiver), var(args)],
            ),
            line,
        )],
        tail,
        line,
    )];
    // Admitted `Function.prototype.apply`: the function found by ordinary Get
    // on the Function.prototype chain re-enters dispatch with its own saved
    // receiver as the callable, the admitted thisArg as the receiver, and an
    // empty argument container. Only the admitted null-thisArg, empty-array
    // shape succeeds; anything else keeps the existing throw.
    tail = vec![branch(
        eq(var(code), int(super::intrinsics::FUNCTION_APPLY_CODE)),
        vec![assign(
            out,
            call(
                objects.function_apply,
                vec![var(carrier), var(code), var(receiver), var(args)],
            ),
            line,
        )],
        tail,
        line,
    )];
    // Admitted `Function.prototype.bind`: the function found by ordinary Get
    // on the Function.prototype chain carries one of two private codes. The
    // method node (FUNCTION_BIND_METHOD_CODE) either creates a bound function
    // (with its own saved callee as the method and the admitted thisArg as the
    // target) or fails; a bound product (FUNCTION_BIND_CODE) re-enters
    // dispatch with the bound target, a null thisArg and an empty argument
    // container. The private code cell, never a row count or the prototype
    // pair, selects the shape. Only those two shapes succeed; anything else
    // keeps the existing throw.
    tail = vec![branch(
        or(
            eq(var(code), int(super::intrinsics::FUNCTION_BIND_CODE)),
            eq(var(code), int(super::intrinsics::FUNCTION_BIND_METHOD_CODE)),
        ),
        vec![assign(
            out,
            call(
                objects.function_bind,
                vec![
                    var(carrier),
                    var(code),
                    var(receiver),
                    var(args),
                    var(function),
                ],
            ),
            line,
        )],
        tail,
        line,
    )];
    // Admitted `Function.prototype.name`/`toString`: the value found by
    // ordinary Get on the gated Function.prototype row. The accessor getter
    // (352) and the method (353) share one private adapter that validates the
    // receiver as a callable function and resolves the stored name row.
    if let Some(api) = intrinsic.function_name_api {
        tail = vec![branch(
            or(
                eq(var(code), int(super::intrinsics::FUNCTION_NAME_GET_CODE)),
                eq(var(code), int(super::intrinsics::FUNCTION_TO_STRING_CODE)),
            ),
            vec![assign(
                out,
                call(
                    api,
                    vec![var(carrier), var(code), var(receiver), var(args)],
                ),
                line,
            )],
            tail,
            line,
        )];
    }
    // Admitted `Function.prototype.prototype`: the gated get/set accessor row
    // found by ordinary Get/Set on the Function.prototype chain. Both halves
    // enter one adapter: a source function lazily materializes (get) or stores
    // (set) its own row, a builtin constructor answers its existing prototype
    // object, and every other function node reads `undefined` on get.
    if let Some(api) = function_prototype_api {
        tail = vec![branch(
            or(
                eq(var(code), int(super::intrinsics::FUNCTION_PROTOTYPE_GET_CODE)),
                eq(var(code), int(super::intrinsics::FUNCTION_PROTOTYPE_SET_CODE)),
            ),
            vec![assign(
                out,
                call(
                    api,
                    vec![var(carrier), var(code), var(receiver), var(args)],
                ),
                line,
            )],
            tail,
            line,
        )];
    }
    // Admitted `JSON.stringify`: the function found by ordinary Get on the
    // `JSON` global object's one own data row. The adapter receives the real
    // `JSON` object as receiver and the real argument container, and admits
    // only the one-argument `{a:1}` shape.
    tail = vec![branch(
        eq(var(code), int(super::intrinsics::JSON_STRINGIFY_CODE)),
        vec![assign(
            out,
            call(
                objects.json_stringify,
                vec![var(carrier), var(code), var(receiver), var(args)],
            ),
            line,
        )],
        tail,
        line,
    )];
    // Admitted `Date.prototype.getTime`: the function found by ordinary Get on
    // the object the one admitted `new Date(0)` construction returned. The
    // adapter receives that object as receiver and the real argument
    // container, and admits only the zero-argument shape.
    tail = vec![branch(
        eq(var(code), int(super::intrinsics::DATE_GETTIME_CODE)),
        vec![assign(
            out,
            call(
                objects.date_gettime,
                vec![var(carrier), var(code), var(receiver), var(args)],
            ),
            line,
        )],
        tail,
        line,
    )];
    // Admitted `/a/.test`: the function found by ordinary Get on the object
    // the one admitted `/a/` literal published. The adapter receives that
    // object as receiver, the real argument container, and the already
    // captured boxed function. Argument evaluation may overwrite or extend
    // the receiver's own rows, so the adapter authenticates the callee it was
    // actually given instead of re-reading those mutable rows. Only the
    // one-String-argument shape with that method's own private brand is
    // admitted. This adds an argument only to the private adapter; the
    // public four-argument dispatch shape is unchanged.
    tail = vec![branch(
        eq(var(code), int(super::intrinsics::REGEXP_TEST_CODE)),
        vec![assign(
            out,
            call(
                objects.regexp_test,
                vec![
                    var(carrier),
                    var(code),
                    var(receiver),
                    var(args),
                    var(function),
                ],
            ),
            line,
        )],
        tail,
        line,
    )];
    // Admitted `String.prototype.charCodeAt`: the function found by ordinary
    // Get on the private String prototype's admitted data row. It receives
    // the real receiver and the real argument container; the adapter performs
    // ToString(this), ToNumber(position), ToIntegerOrInfinity and the checked
    // UTF-16 unit read.
    tail = vec![branch(
        eq(var(code), int(super::intrinsics::STRING_CHAR_CODE_AT_CODE)),
        vec![assign(
            out,
            call(
                objects.char_code_at,
                vec![var(carrier), var(code), var(receiver), var(args)],
            ),
            line,
        )],
        tail,
        line,
    )];
    // Code 276 is the same private adapter. It is not a public call back into
    // charCodeAt, and it does not add a dispatch argument.
    tail = vec![branch(
        eq(var(code), int(super::intrinsics::STRING_CHAR_AT_CODE)),
        vec![assign(
            out,
            call(
                objects.char_code_at,
                vec![var(carrier), var(code), var(receiver), var(args)],
            ),
            line,
        )],
        tail,
        line,
    )];
    // Code 277 is the same private adapter and the same four arguments. It is
    // not a public call back into charCodeAt or charAt.
    tail = vec![branch(
        eq(var(code), int(super::intrinsics::STRING_CODE_POINT_AT_CODE)),
        vec![assign(
            out,
            call(
                objects.char_code_at,
                vec![var(carrier), var(code), var(receiver), var(args)],
            ),
            line,
        )],
        tail,
        line,
    )];
    // Code 278 is the same private adapter and the same four arguments. It is
    // not a public call back into charCodeAt, charAt or codePointAt.
    tail = vec![branch(
        eq(var(code), int(super::intrinsics::STRING_CONCAT_CODE)),
        vec![assign(
            out,
            call(
                objects.char_code_at,
                vec![var(carrier), var(code), var(receiver), var(args)],
            ),
            line,
        )],
        tail,
        line,
    )];
    // Code 279 is the same private adapter and the same four arguments. It is
    // not a public call back into charCodeAt, charAt, codePointAt or concat.
    tail = vec![branch(
        eq(var(code), int(super::intrinsics::STRING_SUBSTRING_CODE)),
        vec![assign(
            out,
            call(
                objects.char_code_at,
                vec![var(carrier), var(code), var(receiver), var(args)],
            ),
            line,
        )],
        tail,
        line,
    )];
    // Code 280 is the same private adapter and the same four arguments. It is
    // not a public call back into charCodeAt, charAt, codePointAt, concat or
    // substring.
    tail = vec![branch(
        eq(var(code), int(super::intrinsics::STRING_SLICE_CODE)),
        vec![assign(
            out,
            call(
                objects.char_code_at,
                vec![var(carrier), var(code), var(receiver), var(args)],
            ),
            line,
        )],
        tail,
        line,
    )];
    // Code 281 is the same private adapter and the same four arguments. It is
    // not a public call back into charCodeAt, charAt, codePointAt, concat,
    // substring or slice. Public length stays 1; the adapter reads arity 2.
    tail = vec![branch(
        eq(var(code), int(super::intrinsics::STRING_INDEXOF_CODE)),
        vec![assign(
            out,
            call(
                objects.char_code_at,
                vec![var(carrier), var(code), var(receiver), var(args)],
            ),
            line,
        )],
        tail,
        line,
    )];
    // Code 282 is the same private adapter and the same four arguments. It is
    // not a public call back into charCodeAt, charAt, codePointAt, concat,
    // substring, slice or indexOf. Public length stays 1; the adapter reads
    // arity 2.
    tail = vec![branch(
        eq(var(code), int(super::intrinsics::STRING_LAST_INDEX_OF_CODE)),
        vec![assign(
            out,
            call(
                objects.char_code_at,
                vec![var(carrier), var(code), var(receiver), var(args)],
            ),
            line,
        )],
        tail,
        line,
    )];
    // Code 283 is the same private adapter and the same four arguments. The
    // adapter does not read those arguments. Public length is 0 and private
    // arity is 0. This is not a call back into charCodeAt, charAt,
    // codePointAt, concat, substring, slice, indexOf, lastIndexOf, or
    // Object.prototype.toString.
    tail = vec![branch(
        eq(var(code), int(super::intrinsics::STRING_TO_STRING_CODE)),
        vec![assign(
            out,
            call(
                objects.char_code_at,
                vec![var(carrier), var(code), var(receiver), var(args)],
            ),
            line,
        )],
        tail,
        line,
    )];
    // Codes 284 and 285 are the same private adapter and the same four
    // arguments. The adapter does not read those arguments. Public length is
    // 0 and private arity is 0. They are not the code-283 String-brand return
    // and not Object.prototype.toString.
    tail = vec![branch(
        or(
            eq(var(code), int(super::intrinsics::STRING_TO_LOWER_CASE_CODE)),
            eq(var(code), int(super::intrinsics::STRING_TO_UPPER_CASE_CODE)),
        ),
        vec![assign(
            out,
            call(
                objects.char_code_at,
                vec![var(carrier), var(code), var(receiver), var(args)],
            ),
            line,
        )],
        tail,
        line,
    )];
    tail = vec![branch(
        or(
            or(
                eq(var(code), int(super::intrinsics::STRING_TRIM_CODE)),
                eq(var(code), int(super::intrinsics::STRING_TRIM_END_CODE)),
            ),
            eq(var(code), int(super::intrinsics::STRING_TRIM_START_CODE)),
        ),
        vec![assign(
            out,
            call(
                objects.char_code_at,
                vec![var(carrier), var(code), var(receiver), var(args)],
            ),
            line,
        )],
        tail,
        line,
    )];
    tail = vec![branch(
        eq(var(code), int(super::intrinsics::STRING_REPLACE_CODE)),
        vec![assign(
            out,
            call(
                objects.char_code_at,
                vec![var(carrier), var(code), var(receiver), var(args)],
            ),
            line,
        )],
        tail,
        line,
    )];
    // Code 351 is the same private adapter and the same four arguments. It is
    // not a public call back into replace: the adapter distinguishes the two
    // codes and only 351 continues after its first match.
    tail = vec![branch(
        eq(var(code), int(super::intrinsics::STRING_REPLACE_ALL_CODE)),
        vec![assign(
            out,
            call(
                objects.char_code_at,
                vec![var(carrier), var(code), var(receiver), var(args)],
            ),
            line,
        )],
        tail,
        line,
    )];
    tail = vec![branch(
        eq(var(code), int(super::intrinsics::STRING_SPLIT_CODE)),
        vec![assign(
            out,
            call(
                objects.char_code_at,
                vec![var(carrier), var(code), var(receiver), var(args)],
            ),
            line,
        )],
        tail,
        line,
    )];
    tail = vec![branch(
        eq(var(code), int(super::intrinsics::STRING_REPEAT_CODE)),
        vec![assign(
            out,
            call(repeat, vec![var(carrier), var(receiver), var(args)]),
            line,
        )],
        tail,
        line,
    )];
    // Code 355 is the gated `String.prototype.search` row. It enters the same
    // private String adapter and the same four arguments; only that adapter's
    // search branch decides the admitted `/a/` argument and the match offset,
    // so this is not a public call back into `replace`, `split` or `match`.
    if e.lower.string_search_used.get() {
        tail = vec![branch(
            eq(var(code), int(super::intrinsics::STRING_SEARCH_CODE)),
            vec![assign(
                out,
                call(
                    objects.char_code_at,
                    vec![var(carrier), var(code), var(receiver), var(args)],
                ),
                line,
            )],
            tail,
            line,
        )];
    }
    // Code 356 is the gated `String.prototype.match` row. It enters the same
    // private String adapter as `search`: that adapter's match branch decides
    // the admitted `/a/` argument, the kernel status and the result Array.
    if e.lower.string_match_used.get() {
        tail = vec![branch(
            eq(var(code), int(super::intrinsics::STRING_MATCH_CODE)),
            vec![assign(
                out,
                call(
                    objects.char_code_at,
                    vec![var(carrier), var(code), var(receiver), var(args)],
                ),
                line,
            )],
            tail,
            line,
        )];
    }
    if let Some(from_code) = from_code {
        tail = vec![branch(
            or(
                eq(var(code), int(super::intrinsics::STRING_FROM_CHAR_CODE_CODE)),
                eq(var(code), int(super::intrinsics::STRING_FROM_CODE_POINT_CODE)),
            ),
            vec![assign(
                out,
                call(from_code, vec![var(carrier), var(code), var(args)]),
                line,
            )],
            tail,
            line,
        )];
    }
    tail = vec![branch(
        or(
            or(
                or(eq(var(code), int(super::intrinsics::ARRAY_PUSH_CODE)), eq(var(code), int(super::intrinsics::ARRAY_POP_CODE))),
                or(eq(var(code), int(super::intrinsics::ARRAY_SHIFT_CODE)), eq(var(code), int(super::intrinsics::ARRAY_UNSHIFT_CODE))),
            ),
            or(
                or(
                    eq(var(code), int(super::intrinsics::ARRAY_REVERSE_CODE)),
                    or(eq(var(code), int(super::intrinsics::ARRAY_INDEX_OF_CODE)), eq(var(code), int(super::intrinsics::ARRAY_LAST_INDEX_OF_CODE))),
                ),
                or(
                    or(
                        eq(var(code), int(super::intrinsics::ARRAY_SLICE_CODE)),
                        eq(var(code), int(super::intrinsics::ARRAY_CONCAT_CODE)),
                    ),
                    or(
                        or(is_array_callback_code(var(code)), super::object::is_array_join_code(var(code))),
                        or(
                            eq(var(code), int(super::intrinsics::ARRAY_SPLICE_CODE)),
                            eq(var(code), int(super::intrinsics::ARRAY_SORT_CODE)),
                        ),
                    ),
                ),
            ),
        ),
        vec![assign(out, call(objects.array_method, vec![var(carrier), var(code), var(receiver), var(args)]), line)],
        tail, line,
    )];
    // Admitted `JSON.parse`: the function found by ordinary Get on the
    // `JSON` global object's second own data row. The adapter receives the
    // real `JSON` object as receiver and the real argument container, and
    // admits only the one-argument `{"a":7}` shape.
    tail = vec![branch(
        eq(var(code), int(super::intrinsics::JSON_PARSE_CODE)),
        vec![assign(
            out,
            call(
                objects.json_parse,
                vec![var(carrier), var(code), var(receiver), var(args)],
            ),
            line,
        )],
        tail,
        line,
    )];
    tail = vec![branch(
        eq(var(code), int(256)),
        Vec::new(),
        vec![branch(
            and(
                not(lt(var(code), int(super::intrinsics::OBJECT_CODE))),
                lt(
                    var(code),
                    int(super::intrinsics::FIRST_METHOD_CODE
                        + super::intrinsics::METHODS.len() as i64),
                ),
            ),
            vec![assign(
                out,
                call(objects.api, vec![var(carrier), var(code), var(args)]),
                line,
            )],
            tail,
            line,
        )],
        line,
    )];
    let failure = vec![assign(
        out,
        value::result_err(call(intrinsic.type_error, vec![var(carrier)]), source_span),
        line,
    )];
    // `Number(value)`: no argument is `+0`; otherwise ToNumber of the first
    // argument through the same bounded conversion the numeric boundary uses
    // (Number, Null, Boolean, String, and NaN for everything else). The
    // carrier is a real callable now, so this branch replaces the previous
    // unconditional TypeError.
    if e.lower.number_namespace_used.get() {
        let argc = e.integer()?;
        let args_borrow = Expr::Borrow(Box::new(var(args)));
        let first = choose(
            lt(int(0), call(B::Len.call_index(), vec![args_borrow.clone()])),
            call(B::Get.call_index(), vec![args_borrow, int(0)]),
            value::undefined(),
        );
        tail = vec![branch(
            eq(var(code), int(super::intrinsics::NUMBER_NAMESPACE_CODE)),
            vec![
                assign(
                    argc,
                    call(B::Len.call_index(), vec![Expr::Borrow(Box::new(var(args)))]),
                    line,
                ),
                assign(
                    out,
                    value::result_ok(
                        value::completion_return(choose(
                            lt(var(argc), int(1)),
                            value::number(Expr::Float(0.0)),
                            value::number(number_argument(
                                e.lower.objects.expect("object runtime").numeric_default_value - 2,
                                carrier,
                                first,
                            )),
                        )),
                        source_span,
                    ),
                    line,
                ),
            ],
            tail,
            line,
        )];
    }
    // `String(value)`: no argument is the empty String; the helper performs the
    // bounded ToString (String identity, accepted Number producer, fixed
    // scalar producer, TypeError for an Object argument).
    if e.lower.string_namespace_used.get() {
        let string_call = string_call.expect("String call helper");
        tail = vec![branch(
            eq(var(code), int(super::intrinsics::STRING_NAMESPACE_CODE)),
            vec![assign(
                out,
                call(string_call, vec![var(carrier), var(args)]),
                line,
            )],
            tail,
            line,
        )];
    }
    // `Number.prototype.toString(radix)`: the receiver must be a primitive
    // Number (the prototype is reachable only through such a receiver), the
    // radix defaults to 10 and is ToInteger'd toward zero. Any radix outside
    // 2..=36, including a value that converts to NaN, is the guest RangeError
    // of 15.7.4.2 step 3 with the upstream message; an invalid receiver keeps
    // the ordinary TypeError.
    if e.lower.number_to_string_used.get() {
        let number_to_string = number_to_string.expect("Number.prototype.toString helper");
        let range_error = intrinsic
            .range_error
            .expect("Number.prototype.toString selects RangeError");
        let heap = e.lower.heap_ref(span)?.helpers;
        let number = e.temp(TypeSchema::Number, Expr::Float(0.0))?;
        let radix = e.temp(TypeSchema::Number, Expr::Float(10.0))?;
        let argc = e.integer()?;
        let raw = e.boxed()?;
        let frame = e.integer()?;
        let err = e.boxed()?;
        let args_borrow = Expr::Borrow(Box::new(var(args)));
        let parser = e.lower.objects.expect("object runtime").numeric_default_value - 2;
        // The RangeError exists only on the rejection path. Its two fixed
        // Strings are published into the fresh root frame before the instance
        // is allocated, so neither can be collected while the message row is
        // written; the instance reaches `out` before the frame is released.
        //
        // Every allocation below (this arm's own two-entry frame, the two
        // String nodes, the private allocator's one-entry frame and the Error
        // instance) can grow the heap. A growth that breaches the physical
        // ceiling is a host fatal, and a fatal bypasses every remaining
        // statement of this arm, so the trailing `root_leave` would never run
        // and `HDR_ACTIVE_ROOT_CAPACITY` would stay charged for a frame that no
        // longer exists. `HDR_HEAP_END` only grows, so the exact worst-case
        // extent of the whole sequence (including one directory page, since the
        // three fresh slots can cross at most one page boundary) is compared
        // with the ceiling *before* the frame is entered. The verdict is
        // conservative the safe way: a sequence a later first-fit could still
        // have placed is refused here, but no sequence that would grow past the
        // ceiling can be admitted.
        let message_text = "radix must be between 2 and 36";
        let rejection_cells = (layout::FRAME_FIXED_CELLS + 2 * layout::FRAME_CELLS_PER_ENTRY)
            + (layout::STRING_HEADER_CELLS + "message".len() as i64 + layout::RAW_HEADER_CELLS)
            + (layout::STRING_HEADER_CELLS
                + message_text.len() as i64
                + layout::RAW_HEADER_CELLS)
            + (layout::FRAME_FIXED_CELLS + layout::FRAME_CELLS_PER_ENTRY)
            + (layout::OBJECT_HEADER_CELLS
                + layout::OBJECT_CELLS_PER_PROPERTY
                + layout::RAW_HEADER_CELLS)
            + layout::DIR_PAGE_CELLS;
        let heap_end = e.decoded(int(layout::HDR_HEAP_END));
        let mut radix_failure = vec![
            assert_stmt(
                not(lt(
                    int(layout::PHYSICAL_CEILING),
                    add(heap_end, int(rejection_cells)),
                )),
                line,
            ),
            assign(
                frame,
                call(heap.root_enter, vec![var(carrier), int(2)]),
                line,
            ),
        ];
        let message_key = super::intrinsics::string(&mut e, "message", frame, 0, &mut radix_failure)?;
        let message_text = super::intrinsics::string(
            &mut e,
            message_text,
            frame,
            1,
            &mut radix_failure,
        )?;
        radix_failure.push(assign(
            err,
            call(range_error, vec![var(carrier), int(1)]),
            line,
        ));
        super::intrinsics::trusted_data(
            &mut e,
            var(err),
            0,
            var(message_key),
            var(message_text),
            (true, false, true),
            &mut radix_failure,
        )?;
        radix_failure.push(assign(out, value::result_err(var(err), source_span), line));
        radix_failure.push(e.effect(heap.root_leave, vec![var(carrier), var(frame)])?);
        let receiver_arm = vec![
            assign(number, num(field(var(receiver), "number")), line),
            assign(
                argc,
                call(B::Len.call_index(), vec![args_borrow.clone()]),
                line,
            ),
            assign(radix, Expr::Float(10.0), line),
            branch(
                lt(int(0), var(argc)),
                vec![
                    assign(
                        raw,
                        call(B::Get.call_index(), vec![args_borrow, int(0)]),
                        line,
                    ),
                    branch(
                        tag_is(var(raw), value::UNDEFINED),
                        Vec::new(),
                        vec![assign(
                            radix,
                            call(
                                B::MathTrunc.call_index(),
                                vec![number_argument(parser, carrier, var(raw))],
                            ),
                            line,
                        )],
                        line,
                    ),
                ],
                Vec::new(),
                line,
            ),
            branch(
                and(
                    and(
                        not(lt(var(radix), Expr::Float(2.0))),
                        not(lt(Expr::Float(36.0), var(radix))),
                    ),
                    eq(var(radix), var(radix)),
                ),
                vec![assign(
                    out,
                    call(
                        number_to_string,
                        vec![var(carrier), var(number), var(radix)],
                    ),
                    line,
                )],
                radix_failure,
                line,
            ),
        ];
        tail = vec![branch(
            eq(var(code), int(super::intrinsics::NUMBER_TO_STRING_CODE)),
            vec![branch(
                tag_is(var(receiver), value::NUMBER),
                receiver_arm,
                failure.clone(),
                line,
            )],
            tail,
            line,
        )];
    }
    if e.lower.array_namespace_used.get() {
        let input = e.boxed()?;
        let kind = e.integer()?;
        let is_array = e.boolean()?;
        let args_borrow = Expr::Borrow(Box::new(var(args)));
        tail = vec![branch(
            eq(var(code), int(super::intrinsics::ARRAY_IS_ARRAY_CODE)),
            vec![
                assign(
                    input,
                    choose(
                        lt(int(0), call(vm::BuiltinFunction::Len.call_index(), vec![args_borrow.clone()])),
                        call(vm::BuiltinFunction::Get.call_index(), vec![args_borrow, int(0)]),
                        value::undefined(),
                    ),
                    line,
                ),
                assign(is_array, Expr::Bool(false), line),
                branch(
                    tag_is(var(input), value::OBJECT),
                    vec![
                        assign(kind, e.decoded(e.base(var(input))), line),
                        assign(
                            is_array,
                            or(eq(var(kind), int(layout::KIND_ARRAY)), eq(var(kind), int(layout::KIND_TAGGED_ARRAY))),
                            line,
                        ),
                    ],
                    Vec::new(),
                    line,
                ),
                assign(
                    out,
                    value::result_ok(value::completion_return(value::boolean(var(is_array))), source_span),
                    line,
                ),
            ],
            tail,
            line,
        )];
    }
    if let Some(helper) = intrinsic.typed_array {
        use super::typed_array::{CALL, TO_STRING};
        tail = vec![branch(
            and(not(lt(var(code), int(CALL))), not(lt(int(TO_STRING), var(code)))),
            vec![assign(out, call(helper, vec![var(carrier), var(code), var(receiver), var(args)]), line)],
            tail,
            line,
        )];
    }
    if let Some(helper) = intrinsic.array_buffer {
        use super::array_buffer::{CALL, GET_LENGTH};
        tail = vec![branch(
            or(eq(var(code), int(CALL)), eq(var(code), int(GET_LENGTH))),
            vec![assign(out, call(helper, vec![var(carrier), var(code), var(receiver), var(args)]), line)],
            tail,
            line,
        )];
    }
    if let Some(array_constructor) = array_constructor {
        tail = vec![branch(
            eq(var(code), int(super::intrinsics::ARRAY_NAMESPACE_CODE)),
            vec![assign(out, call(array_constructor, vec![var(carrier), var(args)]), line)],
            tail,
            line,
        )];
    }
    if e.lower.boolean_used.get() {
        let argc = e.integer()?;
        let input = e.boxed()?;
        tail = vec![branch(
            eq(var(code), int(super::intrinsics::BOOLEAN_CODE)),
            vec![
                assign(
                    argc,
                    call(
                        vm::BuiltinFunction::Len.call_index(),
                        vec![Expr::Borrow(Box::new(var(args)))],
                    ),
                    line,
                ),
                assign(
                    input,
                    choose(
                        lt(int(0), var(argc)),
                        call(
                            vm::BuiltinFunction::Get.call_index(),
                            vec![Expr::Borrow(Box::new(var(args))), int(0)],
                        ),
                        value::undefined(),
                    ),
                    line,
                ),
                assign(
                    out,
                    value::result_ok(
                        value::completion_return(value::boolean(truth(&e, var(input)))),
                        source_span,
                    ),
                    line,
                ),
            ],
            tail,
            line,
        )];
    }
    callable.extend(tail);
    ordinary.push(branch(var(valid), callable, failure.clone(), line));
    let body = vec![branch(
        tag_is(var(function), value::OBJECT),
        ordinary,
        failure,
        line,
    )];
    let body = e.finish(body);
    declare(
        ir,
        statements,
        layout::DISPATCH_HELPER,
        index,
        vec![carrier, function, receiver, args],
        vec![
            carrier_schema.clone(),
            value::schema(),
            value::schema(),
            TypeSchema::Array(Box::new(value::schema())),
        ],
        value::logical_result_schema(),
        body,
        var(out),
        line,
    );
    Ok(index)
}

/// Bounded ES5 ToNumber for one call argument, matching the numeric boundary
/// already accepted for String and Math call sites: Number, Null, Boolean and
/// String convert through the installed StringNumericLiteral parser; every
/// other tag (Object, Undefined) is NaN. `Number(undefined)` and
/// `Number({})` are NaN upstream too; only an object with a `valueOf`/`toString`
/// primitive would differ, and that conversion is outside this domain.
fn number_argument(parser: u16, carrier: u16, payload: Expr) -> Expr {
    use super::object::{call, choose, field, int, tag_is, var};
    choose(
        tag_is(payload.clone(), value::NUMBER),
        field(payload.clone(), "number"),
        choose(
            tag_is(payload.clone(), value::NULL),
            Expr::Float(0.0),
            choose(
                tag_is(payload.clone(), value::BOOLEAN),
                choose(
                    field(payload.clone(), "flag"),
                    Expr::Float(1.0),
                    Expr::Float(0.0),
                ),
                choose(
                    tag_is(payload.clone(), value::STRING),
                    call(
                        parser,
                        vec![var(carrier), payload, int(0), Expr::Float(0.0)],
                    ),
                    Expr::Float(f64::NAN),
                ),
            ),
        ),
    )
}
