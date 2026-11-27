//! Demand-gated primitive URI calls. Guest codes are not IR indices.
use super::super::prelude::{heap as heap_emit, object::*, uri, utf16_bridge};
use super::*;
use vm::{BuiltinFunction as B, Expr as E, Stmt as S, TypeSchema as T};

pub(in crate::javascript) const FIRST_CODE: i64 = 536;
pub(in crate::javascript) const KIND_COUNT: u8 = 4;

// Keep the legacy two-code expression byte-identical for component-only demand.
fn code_guard(code: u16, mask: u8) -> E {
    let component = or(
        eq(var(code), int(FIRST_CODE)),
        eq(var(code), int(FIRST_CODE + 1)),
    );
    if mask & 0b1100 == 0 {
        component
    } else {
        or(
            component,
            or(
                eq(var(code), int(FIRST_CODE + 2)),
                eq(var(code), int(FIRST_CODE + 3)),
            ),
        )
    }
}
fn kernel_mode(code: u16, mask: u8) -> E {
    let component = choose(eq(var(code), int(FIRST_CODE)), int(1), int(3));
    if mask & 0b1100 == 0 {
        component
    } else {
        choose(
            lt(var(code), int(FIRST_CODE + 2)),
            component,
            choose(eq(var(code), int(FIRST_CODE + 2)), int(0), int(2)),
        )
    }
}
/// Shared two-row error prototype and its eight-unit name String.
pub(in crate::javascript) const PROTOTYPE_REQUEST: (u64, u64) = ((3 + 5 + 7 * 2) + (6 + 8), 2);

pub(in crate::javascript) fn prototype_used(lower: &Lowerer<'_>) -> bool {
    lower.uri_used.get() != 0
        || super::super::prelude::intrinsics::OptionalError::Uri.is_selected(lower)
}

/// Shared prototype and selected one-row functions for URI adapter demand.
/// Root growth is already included by selected_root_capacity.
pub(in crate::javascript) fn manifest(mask: u8) -> (u64, u64) {
    if mask == 0 {
        return (0, 0);
    }
    let functions = u64::from(mask.count_ones());
    (
        PROTOTYPE_REQUEST.0 + functions * (3 + 10 + 7),
        PROTOTYPE_REQUEST.1 + functions,
    )
}
pub(in crate::javascript) fn prototype_root(lower: &Lowerer<'_>) -> i64 {
    5 + i64::from(lower.math_used.get()) + i64::from(lower.numeric_globals.get().count_ones())
}
pub(in crate::javascript) fn function_root(lower: &Lowerer<'_>, kind: u8) -> i64 {
    prototype_root(lower) + 1 + i64::from((lower.uri_used.get() & ((1 << kind) - 1)).count_ones())
}
impl Lowerer<'_> {
    pub(super) fn ensure_utf16_bridge(
        &mut self,
        ir: &mut FrontendIr,
        span: swc_common::Span,
    ) -> Result<(), ParseError> {
        if self.utf16_bridge.is_some() {
            return Ok(());
        }
        for declaration in [value::declaration(), value::allocation_declaration()] {
            if let Some(existing) = ir.struct_schemas.get(&declaration.name) {
                if existing.name != declaration.name
                    || existing.type_params != declaration.type_params
                    || existing.body_schema != declaration.body_schema
                {
                    return Err(self.error(
                        span,
                        "Number key requires the exact managed value/allocation schema",
                    ));
                }
            } else {
                ir.struct_schemas
                    .insert(declaration.name.clone(), declaration);
            }
        }
        ir.locals = ir.locals.max(usize::from(self.next_slot));
        self.utf16_bridge = Some(utf16_bridge::append(ir)?);
        self.next_slot = u16::try_from(ir.locals)
            .map_err(|_| self.error(span, "UTF16 bridge local slot space exhausted"))?;
        Ok(())
    }
    pub(super) fn append_uri_prerequisites(
        &mut self,
        ir: &mut FrontendIr,
        carrier_schema: &T,
        span: swc_common::Span,
        stmts: &mut Vec<S>,
    ) -> Result<(), ParseError> {
        if self.uri_used.get() == 0 {
            return Ok(());
        }
        // First-class URI values may receive any primitive, regardless of the
        // current source argument facts. Share the existing producers once.
        self.ensure_number_string_prerequisites(ir, span)?;
        self.append_fixed_scalar_string_helper(ir, carrier_schema, span, stmts)?;
        let number_string = self
            .number_string_entries
            .as_ref()
            .expect("ensured Number")
            .managed;
        let fixed_string = self
            .helper_fixed_scalar_string
            .expect("ensured fixed scalar");
        // Reused prerequisites may return early without syncing reservations.
        // Both bounds must agree before the private kernel reserves its slots.
        ir.locals = ir.locals.max(usize::from(self.next_slot));
        self.next_slot = u16::try_from(ir.locals)
            .map_err(|_| self.error(span, "URI local slot space exhausted"))?;
        let bridge = self.utf16_bridge.as_ref().expect("ensured bridge");
        let (to, from) = (bridge.managed_to_units, bridge.units_to_managed);
        // The immutable private kernel has a transactional integration boundary.
        let mut staged = ir.clone();
        let kernel = uri::append(&mut staged)?;
        *ir = staged;
        self.next_slot = u16::try_from(ir.locals)
            .map_err(|_| self.error(span, "URI local slot space exhausted"))?;
        let declaration = value::completion_declaration();
        if let Some(existing) = ir.struct_schemas.get(&declaration.name) {
            if existing.body_schema != declaration.body_schema
                || existing.type_params != declaration.type_params
            {
                return Err(self.error(span, "URI requires exact Completion schema"));
            }
        } else {
            ir.struct_schemas
                .insert(declaration.name.clone(), declaration);
        }
        let index = heap_emit::fresh_index(self, ir, span)?;
        let h = self.heap_ref(span)?.helpers;
        let uri_error = self
            .intrinsics
            .expect("URI realm")
            .uri_error
            .expect("URI error helper");
        let c = self.slot(span)?;
        let code = self.slot(span)?;
        let args = self.slot(span)?;
        let mut e = Emit::new(self, span, c)?;
        let l = e.line;
        let frame = e.integer()?;
        let input = e.boxed()?;
        let output = e.boxed()?;
        let out = e.temp(
            value::logical_result_schema(),
            value::result_ok(value::completion_normal(), vm::Span::new(0, 0, 0)),
        )?;
        let units = e.lower.slot(span)?;
        let record = e.lower.slot(span)?;
        let body = vec![
            assert_stmt(code_guard(code, e.lower.uri_used.get()), l),
            assign(
                input,
                choose(
                    lt(
                        int(0),
                        call(B::Len.call_index(), vec![E::Borrow(Box::new(var(args)))]),
                    ),
                    call(
                        B::Get.call_index(),
                        vec![E::Borrow(Box::new(var(args))), int(0)],
                    ),
                    value::undefined(),
                ),
                l,
            ),
            // This is an actual-tag profile guard, not guest ToPrimitive.
            assert_stmt(
                or(
                    tag_is(var(input), value::STRING),
                    or(
                        tag_is(var(input), value::NUMBER),
                        or(
                            tag_is(var(input), value::BOOLEAN),
                            or(
                                tag_is(var(input), value::NULL),
                                tag_is(var(input), value::UNDEFINED),
                            ),
                        ),
                    ),
                ),
                l,
            ),
            assign(frame, call(h.root_enter, vec![var(c), int(2)]), l),
            e.root(var(frame), int(0), var(input))?,
            branch(
                tag_is(var(input), value::STRING),
                vec![],
                vec![
                    branch(
                        tag_is(var(input), value::NUMBER),
                        vec![assign(
                            input,
                            call(
                                number_string,
                                vec![
                                    var(c),
                                    Self::as_number(super::super::prelude::math::checked_number(
                                        var(input),
                                    )),
                                    var(frame),
                                    int(0),
                                ],
                            ),
                            l,
                        )],
                        vec![assign(
                            input,
                            call(fixed_string, vec![var(c), var(input), var(frame), int(0)]),
                            l,
                        )],
                        l,
                    ),
                    // Each producer already published into input0. No allocation
                    // or collection between its return and this reaffirmation.
                    e.root(var(frame), int(0), var(input))?,
                ],
                l,
            ),
            S::Let {
                index: units,
                declared_schema: Some(T::Array(Box::new(T::Int))),
                expr: call(to, vec![var(c), var(input)]),
                line: l,
            },
            S::Let {
                index: record,
                declared_schema: Some(uri::result_schema()),
                expr: call(
                    kernel,
                    vec![
                        E::MoveVar(units),
                        kernel_mode(code, e.lower.uri_used.get()),
                        int(65_536),
                    ],
                ),
                line: l,
            },
            // Resource refusal never reaches either managed publication arm.
            assert_stmt(
                or(
                    eq(field(var(record), "status"), int(0)),
                    eq(field(var(record), "status"), int(1)),
                ),
                l,
            ),
            branch(
                eq(field(var(record), "status"), int(0)),
                vec![
                    assign(
                        output,
                        call(
                            from,
                            vec![var(c), field(var(record), "output"), var(frame), int(1)],
                        ),
                        l,
                    ),
                    assign(
                        out,
                        value::result_ok(
                            value::completion_return(var(output)),
                            vm::Span::new(0, 0, 0),
                        ),
                        l,
                    ),
                ],
                vec![
                    assign(output, call(uri_error, vec![var(c)]), l),
                    e.root(var(frame), int(1), var(output))?,
                    assign(
                        out,
                        value::result_err(var(output), vm::Span::new(0, 0, 0)),
                        l,
                    ),
                ],
                l,
            ),
            // No managed allocation from this leave through dispatch and caller
            // Result publication. The caller publishes before clearing actuals.
            e.effect(h.root_leave, vec![var(c), var(frame)])?,
        ];
        let body = e.finish(body);
        declare(
            ir,
            stmts,
            "__compat_es5_uri_call",
            index,
            vec![c, code, args],
            vec![
                carrier_schema.clone(),
                T::Int,
                T::Array(Box::new(value::schema())),
            ],
            value::logical_result_schema(),
            body,
            var(out),
            l,
        );
        self.uri_call = Some(index);
        ir.locals = ir.locals.max(usize::from(self.next_slot));
        Ok(())
    }
}
/// Extend the fresh dispatch only inside its checked Object/Function branch.
pub(in crate::javascript) fn extend_dispatch(
    ir: &mut FrontendIr,
    dispatch: u16,
    adapter: u16,
    mask: u8,
) {
    let f = ir
        .function_impls
        .get_mut(&dispatch)
        .expect("fresh dispatcher");
    let [carrier, _, _, args] = f.param_slots.as_slice() else {
        unreachable!("dispatch ABI4")
    };
    let E::Var(out) = f.body_expr else {
        unreachable!("dispatch result")
    };
    let Some(S::IfElse {
        then_branch: ordinary,
        ..
    }) = f.body_stmts.last_mut()
    else {
        unreachable!("dispatch tag")
    };
    let Some(S::IfElse {
        then_branch: callable,
        ..
    }) = ordinary.last_mut()
    else {
        unreachable!("dispatch kind")
    };
    let S::Assign { index: code, .. } = callable[0] else {
        unreachable!("dispatch code")
    };
    let original = callable.pop().expect("dispatch tail");
    let line = f.body_expr_line;
    callable.push(branch(
        code_guard(code, mask),
        vec![assign(
            out,
            call(adapter, vec![var(*carrier), var(code), var(*args)]),
            line,
        )],
        vec![original],
        line,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uri_manifest_host_costs_and_codes() {
        assert_eq!(
            [manifest(0), manifest(1), manifest(2), manifest(3)],
            [(0, 0), (56, 3), (56, 3), (76, 4)]
        );
        assert_eq!(
            [manifest(4), manifest(8), manifest(12), manifest(15)],
            [(56, 3), (56, 3), (76, 4), (116, 6)]
        );
        assert_eq!(FIRST_CODE, 536);
        assert_eq!(KIND_COUNT, 4);
        let mut occupied: std::collections::HashSet<i64> = (0..=256).collect();
        occupied.extend(
            super::super::super::prelude::intrinsics::OBJECT_CODE
                ..super::super::super::prelude::intrinsics::FIRST_METHOD_CODE
                    + super::super::super::prelude::intrinsics::METHODS.len() as i64,
        );
        let codes = super::super::super::prelude::math::occupied_codes();
        for code in &codes {
            assert!(occupied.insert(*code), "guest code collision {code}");
        }
        assert_eq!(
            codes
                .iter()
                .filter(|code| (512..534).contains(*code))
                .count(),
            22,
            "legacy math and numeric-global codes stay 512..534"
        );
        assert_eq!(
            super::super::super::prelude::math::EXTENSIONS
                .iter()
                .map(|entry| entry.2)
                .collect::<Vec<_>>(),
            vec![540, 541, 542, 543, 544, 545, 546]
        );
        assert_eq!(
            (0..super::super::super::prelude::math::GLOBALS.len())
                .map(super::super::super::prelude::math::global_code)
                .collect::<Vec<_>>(),
            vec![530, 531, 532, 533]
        );
        assert!(
            codes.iter().all(|code| !(536..540).contains(code)),
            "extensions must not occupy URI codes"
        );
        for code in FIRST_CODE..FIRST_CODE + i64::from(KIND_COUNT) {
            assert!(occupied.insert(code), "guest code collision {code}");
        }
        for code in super::super::super::prelude::intrinsics::HAS_OWN_PROPERTY_CODE
            ..=super::super::super::prelude::intrinsics::REGEXP_TEST_CODE
        {
            assert!(occupied.insert(code), "guest code collision {code}");
        }
        for code in super::super::super::prelude::intrinsics::STRING_CHAR_CODE_AT_CODE
            ..=super::super::super::prelude::intrinsics::STRING_TO_UPPER_CASE_CODE
        {
            assert!(occupied.insert(code), "guest code collision {code}");
        }
        for code in super::super::super::prelude::intrinsics::STRING_TRIM_CODE
            ..=super::super::super::prelude::intrinsics::ARRAY_UNSHIFT_CODE
        {
            assert!(occupied.insert(code), "guest code collision {code}");
        }
        let number_code = super::super::super::prelude::intrinsics::NUMBER_NAMESPACE_CODE;
        assert_eq!(number_code, 299);
        assert!(occupied.insert(number_code), "guest code collision {number_code}");
        assert_eq!(
            codes.iter().filter(|code| **code == 530 || **code == 531).count(),
            2,
            "parseInt/parseFloat stay the unique 530/531 pair"
        );
        let reverse_code = super::super::super::prelude::intrinsics::ARRAY_REVERSE_CODE;
        assert_eq!(reverse_code, 304);
        assert!(occupied.insert(reverse_code), "guest code collision {reverse_code}");
        let index_of_code = super::super::super::prelude::intrinsics::ARRAY_INDEX_OF_CODE;
        assert_eq!(index_of_code, 306);
        assert!(occupied.insert(index_of_code), "guest code collision {index_of_code}");
        let last_index_of_code = super::super::super::prelude::intrinsics::ARRAY_LAST_INDEX_OF_CODE;
        assert_eq!(last_index_of_code, 307);
        assert!(occupied.insert(last_index_of_code), "guest code collision {last_index_of_code}");
        let boolean_code = super::super::super::prelude::intrinsics::BOOLEAN_CODE;
        assert_eq!(boolean_code, 305);
        assert!(occupied.insert(boolean_code), "guest code collision {boolean_code}");
        let range_error_code = super::super::super::prelude::intrinsics::RANGE_ERROR_CODE;
        assert_eq!(range_error_code, 308);
        assert!(occupied.insert(range_error_code), "guest code collision {range_error_code}");
        let repeat_code = super::super::super::prelude::intrinsics::STRING_REPEAT_CODE;
        assert_eq!(repeat_code, 309);
        assert!(occupied.insert(repeat_code), "guest code collision {repeat_code}");
        let from_char_code = super::super::super::prelude::intrinsics::STRING_FROM_CHAR_CODE_CODE;
        assert_eq!(from_char_code, 310);
        assert!(occupied.insert(from_char_code), "guest code collision {from_char_code}");
        let from_code_point = super::super::super::prelude::intrinsics::STRING_FROM_CODE_POINT_CODE;
        assert_eq!(from_code_point, 311);
        assert!(occupied.insert(from_code_point), "guest code collision {from_code_point}");
        let string_namespace = super::super::super::prelude::intrinsics::STRING_NAMESPACE_CODE;
        assert_eq!(string_namespace, 312);
        assert!(occupied.insert(string_namespace), "guest code collision {string_namespace}");
        let array_slice_code = super::super::super::prelude::intrinsics::ARRAY_SLICE_CODE;
        assert_eq!(array_slice_code, 313);
        assert!(occupied.insert(array_slice_code), "guest code collision {array_slice_code}");
        let array_concat_code = super::super::super::prelude::intrinsics::ARRAY_CONCAT_CODE;
        assert_eq!(array_concat_code, 314);
        assert!(occupied.insert(array_concat_code), "guest code collision {array_concat_code}");
        use super::super::super::prelude::intrinsics::{
            ARRAY_EVERY_CODE, ARRAY_FILTER_CODE, ARRAY_FOR_EACH_CODE, ARRAY_JOIN_CODE, ARRAY_MAP_CODE,
            ARRAY_REDUCE_CODE, ARRAY_REDUCE_RIGHT_CODE, ARRAY_SOME_CODE, ARRAY_SPLICE_CODE,
            ARRAY_SORT_CODE, ARRAY_TO_STRING_CODE,
        };
        for (code, expected) in [
            (ARRAY_EVERY_CODE, 317),
            (ARRAY_SOME_CODE, 318),
            (ARRAY_FOR_EACH_CODE, 319),
            (ARRAY_MAP_CODE, 320),
            (ARRAY_FILTER_CODE, 321),
            (ARRAY_REDUCE_CODE, 322),
            (ARRAY_REDUCE_RIGHT_CODE, 323),
            (ARRAY_JOIN_CODE, 324),
            (ARRAY_TO_STRING_CODE, 325),
            (ARRAY_SPLICE_CODE, 326),
            (ARRAY_SORT_CODE, 327),
        ] {
            assert_eq!(code, expected);
            assert!(occupied.insert(code), "guest code collision {code}");
        }
        let array_namespace = super::super::super::prelude::intrinsics::ARRAY_NAMESPACE_CODE;
        assert_eq!(array_namespace, 315);
        assert!(occupied.insert(array_namespace), "guest code collision {array_namespace}");
        let array_is_array = super::super::super::prelude::intrinsics::ARRAY_IS_ARRAY_CODE;
        assert_eq!(array_is_array, 316);
        assert!(occupied.insert(array_is_array), "guest code collision {array_is_array}");
        use super::super::super::prelude::array_buffer::{CALL, GET_LENGTH};
        assert_eq!((CALL, GET_LENGTH), (300, 301));
        assert!(occupied.insert(CALL) && occupied.insert(GET_LENGTH));
        use super::super::super::prelude::typed_array as view;
        let view_codes = [view::ABSTRACT, view::GET_LENGTH, view::GET_BYTE_LENGTH,
            view::GET_BYTE_OFFSET, view::GET_BUFFER]
            .into_iter().chain(view::Element::ALL.map(view::Element::call_code));
        let view_codes = view_codes.chain([view::SUBARRAY, view::SET, view::JOIN, view::TO_STRING]);
        let expected = (329..=333).chain([328]).chain(334..=345);
        assert_eq!(view_codes.clone().count(), expected.clone().count());
        for (code, expected) in view_codes.zip(expected)
        {
            assert_eq!(code, expected);
            assert!(occupied.insert(code), "guest code collision {code}");
        }
        use super::super::super::prelude::intrinsics::OptionalError;
        for (error, expected) in OptionalError::ALL.into_iter().zip(346..=350) {
            let code = error.code();
            assert_eq!(code, expected);
            assert!(occupied.insert(code), "guest code collision {code}");
        }
        let replace_all_code = super::super::super::prelude::intrinsics::STRING_REPLACE_ALL_CODE;
        assert_eq!(replace_all_code, 351);
        assert!(occupied.insert(replace_all_code), "guest code collision {replace_all_code}");
        assert!(
            occupied.contains(&351),
            "replaceAll must be registered at guest code 351"
        );
        // The tail of the primitive code space: the private Function.prototype
        // name getter (352), `Function.prototype.toString` (353), the private
        // `Function.prototype.bind` method node (354), the gated
        // `String.prototype.search` row (355), the gated
        // `String.prototype.match` row (356) and the gated
        // `Function.prototype.prototype` accessor pair (357/358). They never overlap a static,
        // String, typed-array, error, Math, numeric-global or URI code.
        use super::super::super::prelude::intrinsics::{
            FUNCTION_BIND_METHOD_CODE, FUNCTION_NAME_GET_CODE, FUNCTION_PROTOTYPE_GET_CODE,
            FUNCTION_PROTOTYPE_SET_CODE, FUNCTION_TO_STRING_CODE, STRING_MATCH_CODE,
            STRING_SEARCH_CODE,
        };
        for (code, expected, label) in [
            (FUNCTION_NAME_GET_CODE, 352_i64, "Function.prototype.name getter"),
            (FUNCTION_TO_STRING_CODE, 353, "Function.prototype.toString"),
            (FUNCTION_BIND_METHOD_CODE, 354, "bind method node"),
            (STRING_SEARCH_CODE, 355, "String.prototype.search"),
            (STRING_MATCH_CODE, 356, "String.prototype.match"),
            (FUNCTION_PROTOTYPE_GET_CODE, 357, "Function.prototype.prototype getter"),
            (FUNCTION_PROTOTYPE_SET_CODE, 358, "Function.prototype.prototype setter"),
        ] {
            assert_eq!(code, expected, "{label}");
            assert!(occupied.insert(code), "guest code collision {code}");
        }
        for code in (286..=288).chain(302..=303) {
            assert!(!occupied.contains(&code), "reserved guest code {code}");
        }
        assert!(occupied.contains(&299) && occupied.contains(&304) && occupied.contains(&305) && occupied.contains(&306) && occupied.contains(&307) && occupied.contains(&308));
        eprintln!(
            "URI_CODES number={number_code} reverse={reverse_code} boolean={boolean_code} indexOf={index_of_code} lastIndexOf={last_index_of_code} rangeError={range_error_code} repeat={repeat_code} fromCharCode={from_char_code} fromCodePoint={from_code_point} string={string_namespace} map={ARRAY_MAP_CODE} filter={ARRAY_FILTER_CODE} reduce={ARRAY_REDUCE_CODE} reduceRight={ARRAY_REDUCE_RIGHT_CODE} join={ARRAY_JOIN_CODE} toString={ARRAY_TO_STRING_CODE} splice={ARRAY_SPLICE_CODE} sort={ARRAY_SORT_CODE} array_buffer=300,301 typed_array=328..=345 reserved_free=302..=303 unchanged=299,304..=326"
        );
    }
}
