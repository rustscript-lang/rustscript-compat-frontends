//! Compiler Reference lowering for the closed, statically resolved subset.
//!
//! Gets and puts go through this module so declaration presence is distinct
//! from the current value. Unresolved ordinary reads stay compile refusals.
//! Outer bindings are a labeled P4 refusal, not copied locals. `typeof` of an
//! unresolved identifier does not perform GetValue.

use super::prelude::intrinsics::OptionalError;
use swc_common::Spanned;
use swc_ecma_ast as ast;
use vm::{BuiltinFunction, Expr, ParseError, Stmt, TypeSchema};

use super::heap as layout;
use super::lowerer::{CallResultRooting, Kind, Lowered, Lowerer};
use super::representation as value;
use super::scope::{BindingId, ConstructorAlias, ScopeId};

#[derive(Clone, Copy, Default)]
pub(super) struct TypeNames {
    pub undefined: Option<u16>,
    pub object: Option<u16>,
    pub boolean: Option<u16>,
    pub number: Option<u16>,
    pub string: Option<u16>,
    pub function: Option<u16>,
}

use super::prelude::typed_array::Element;

pub(super) enum NameHit {
    Global(u8),
    Local(BindingId),
    Outer(#[allow(dead_code)] BindingId),
    BuiltinUndefined,
    BuiltinObject,
    BuiltinTypeError,
    BuiltinRangeError,
    BuiltinOptionalError(OptionalError),
    BuiltinJson,
    BuiltinMath,
    BuiltinNumeric(u8),
    BuiltinUri(u8),
    BuiltinNumber(bool),
    BuiltinNumberNamespace,
    BuiltinStringNamespace,
    BuiltinArrayNamespace,
    BuiltinArrayBuffer,
    BuiltinTypedArray(Element),
    BuiltinBoolean,
    Missing,
}

pub(super) fn resolve(lower: &Lowerer<'_>, name: &str) -> NameHit {
    if let Some(overlay) = lower.catch_overlay.get() {
        // Emission sets active_scope to the catch scope and clears this overlay.
        // Validation leaves the Lowerer immutable, so the overlay is that scope:
        // walk its parents, including an enclosing catch parameter, instead of
        // falling through to the function scope.
        return resolve_from(lower, name, overlay);
    }
    resolve_from(lower, name, lower.active_scope)
}

/// Pinned Micro lets a function declaration spelled `arguments` coexist with
/// the activation's implicit binding: the enclosing function still observes
/// its own invocation vector through `arguments[i]`/`arguments.length`, while
/// the declaration stays storable/callable on its own (`resolve` still names
/// its binding). The same rule holds across activations: ES5 gives every
/// non-arrow function its own `arguments`, so a name that resolves outside
/// the current activation can never shadow the current activation's vector.
/// A binding in the current activation still shadows it (an ordinary same-name
/// value keeps its own read). This is the single predicate for the
/// member-observation routes, so no other name-resolution rule widens.
pub(super) fn implicit_arguments(lower: &Lowerer<'_>) -> bool {
    match resolve(lower, "arguments") {
        NameHit::Missing => true,
        NameHit::Local(id) => !lower.graph.binding(id).fn_sites.is_empty(),
        NameHit::Outer(_) => true,
        _ => false,
    }
}

fn resolve_from(lower: &Lowerer<'_>, name: &str, mut scope: ScopeId) -> NameHit {
    let mut crossed = false;
    loop {
        if let Some(id) = lower.graph.scope(scope).by_name.get(name).copied() {
            if scope == lower.graph.script
                && let Some(i) = super::global_binding::index(name)
            {
                if lower.globals.contains(i) {
                    return if lower.globals.shared {
                        NameHit::Global(i)
                    } else {
                        NameHit::Local(id)
                    };
                }
                break;
            } else {
                return if crossed {
                    NameHit::Outer(id)
                } else {
                    NameHit::Local(id)
                };
            }
        }
        let kind = lower.graph.scope(scope).kind;
        let Some(parent) = lower.graph.scope(scope).parent else {
            break;
        };
        // Catch stays in the same activation. Only leaving a Function
        // crosses into an outer activation.
        if kind == super::scope::ScopeKind::Function {
            crossed = true;
        }
        scope = parent;
    }
    if name == "undefined" {
        NameHit::BuiltinUndefined
    } else if name == "Object" {
        NameHit::BuiltinObject
    } else if name == "TypeError" {
        NameHit::BuiltinTypeError
    } else if name == "RangeError" {
        lower.range_error_used.set(true);
        NameHit::BuiltinRangeError
    } else if let Some(error) = OptionalError::named(name) {
        NameHit::BuiltinOptionalError(error)
    } else if name == "JSON" {
        // `JSON` is an ordinary global object, not a writable binding: ordinary
        // resolution still decides it, and its one own `stringify` row is found
        // by the unchanged ordinary Get. Naming it only asks the realm for the
        // optional object, exactly like `Math`.
        lower.json_used.set(true);
        NameHit::BuiltinJson
    } else if name == "Number" {
        NameHit::BuiltinNumberNamespace
    } else if name == "String" {
        NameHit::BuiltinStringNamespace
    } else if name == "ArrayBuffer" {
        NameHit::BuiltinArrayBuffer
    } else if let Some(element) = Element::named(name) {
        NameHit::BuiltinTypedArray(element)
    } else if name == "Array" {
        NameHit::BuiltinArrayNamespace
    } else if name == "Boolean" {
        NameHit::BuiltinBoolean
    } else if name == "NaN" || name == "Infinity" {
        NameHit::BuiltinNumber(name == "Infinity")
    } else if let Some(i) = super::prelude::math::GLOBALS
        .iter()
        .position(|global| global.0 == name)
    {
        NameHit::BuiltinNumeric(i as u8)
    } else if name == "Math" {
        NameHit::BuiltinMath
    } else if let Some(i @ 6..=9) = super::global_binding::index(name) {
        NameHit::BuiltinUri(i - 6)
    } else {
        NameHit::Missing
    }
}

pub(super) fn lower_get(lower: &Lowerer<'_>, ident: &ast::Ident) -> Result<Lowered, ParseError> {
    match resolve(lower, ident.sym.as_str()) {
        NameHit::Global(i) => Ok(Lowered {
            expr: Lowerer::guest_call(
                lower.global_helpers.expect("shared helpers").get,
                vec![
                    Expr::Var(lower.active_carrier(ident.span)?),
                    Expr::Var(lower.global_env.ok_or_else(|| {
                        lower.error(ident.span, "shared environment unavailable")
                    })?),
                    Expr::Int(lower.globals.entry(i)),
                ],
            ),
            kind: Kind::Dynamic,
            array_length: None,
            owned_roots: Vec::new(),
        }),
        NameHit::Local(id) => {
            let slot = *lower.binding_slots.get(&id).ok_or_else(|| {
                lower.error(ident.span, "P3 binding was collected but not instantiated")
            })?;
            let kind = lower
                .binding_kinds
                .get(&id)
                .copied()
                .unwrap_or(Kind::Undefined);
            let array_length = if kind == Kind::Array {
                lower.array_length_ids.get(&id).copied()
            } else {
                None
            };
            Ok(Lowered {
                expr: Expr::Var(slot),
                kind,
                array_length,
                owned_roots: Vec::new(),
            })
        }
        NameHit::BuiltinObject | NameHit::BuiltinMath | NameHit::BuiltinTypeError | NameHit::BuiltinRangeError => {
            let intrinsic = lower
                .intrinsics
                .ok_or_else(|| lower.error(ident.span, "Object requires an ordinary realm"))?;
            Ok(Lowered {
                expr: Lowerer::guest_call(
                    intrinsic.root,
                    vec![
                        Expr::Var(lower.active_carrier(ident.span)?),
                        Expr::Int(if ident.sym == *"Math" {
                            5
                        } else if ident.sym == *"TypeError" {
                            super::prelude::intrinsics::type_error_constructor_root(lower)
                        } else if ident.sym == *"RangeError" {
                            super::prelude::intrinsics::range_error_constructor_root(lower)
                        } else {
                            4
                        }),
                    ],
                ),
                kind: if ident.sym == *"Math" {
                    Kind::Object
                } else {
                    Kind::Function
                },
                array_length: None,
                owned_roots: Vec::new(),
            })
        }
        NameHit::BuiltinOptionalError(error) => {
            let expr = if lower.in_dead_capability() && lower.optional_errors_used.get() & error.bit() == 0 {
                value::undefined()
            } else {
                let intrinsic = lower.intrinsics.ok_or_else(|| lower.error(ident.span, "Error constructor requires an ordinary realm"))?;
                debug_assert_ne!(lower.optional_errors_used.get() & error.bit(), 0);
                Lowerer::guest_call(intrinsic.root, vec![
                    Expr::Var(lower.active_carrier(ident.span)?),
                    Expr::Int(error.constructor_root(lower)),
                ])
            };
            Ok(Lowered { expr, kind: Kind::Function, array_length: None, owned_roots: Vec::new() })
        }
        NameHit::BuiltinJson => {
            lower.json_used.set(true);
            let intrinsic = lower
                .intrinsics
                .ok_or_else(|| lower.error(ident.span, "JSON requires an ordinary realm"))?;
            Ok(Lowered {
                expr: Lowerer::guest_call(
                    intrinsic.root,
                    vec![
                        Expr::Var(lower.active_carrier(ident.span)?),
                        Expr::Int(super::prelude::intrinsics::json_root(lower)),
                    ],
                ),
                kind: Kind::Object,
                array_length: None,
                owned_roots: Vec::new(),
            })
        }
        NameHit::BuiltinNumberNamespace => Ok(Lowered {
            expr: Lowerer::guest_call(
                lower.intrinsics.expect("Number realm").root,
                vec![
                    Expr::Var(lower.active_carrier(ident.span)?),
                    Expr::Int(super::prelude::intrinsics::number_namespace_root(lower)),
                ],
            ),
            kind: Kind::Function,
            array_length: None,
            owned_roots: Vec::new(),
        }),
        NameHit::BuiltinStringNamespace => Ok(Lowered {
            expr: Lowerer::guest_call(
                lower.intrinsics.expect("String realm").root,
                vec![
                    Expr::Var(lower.active_carrier(ident.span)?),
                    Expr::Int(super::prelude::intrinsics::string_namespace_root(lower)),
                ],
            ),
            kind: Kind::Function,
            array_length: None,
            owned_roots: Vec::new(),
        }),
        NameHit::BuiltinArrayNamespace => Ok(Lowered {
            expr: Lowerer::guest_call(
                lower.intrinsics.expect("Array realm").root,
                vec![
                    Expr::Var(lower.active_carrier(ident.span)?),
                    Expr::Int(super::prelude::intrinsics::array_namespace_root(lower)),
                ],
            ),
            kind: Kind::Function,
            array_length: None,
            owned_roots: Vec::new(),
        }),
        NameHit::BuiltinArrayBuffer => Ok(Lowered {
            expr: Lowerer::guest_call(
                lower.intrinsics.expect("ArrayBuffer realm").root,
                vec![
                    Expr::Var(lower.active_carrier(ident.span)?),
                    Expr::Int(super::prelude::intrinsics::array_buffer_constructor_root(lower)),
                ],
            ),
            kind: Kind::Function,
            array_length: None,
            owned_roots: Vec::new(),
        }),
        NameHit::BuiltinTypedArray(element) => Ok(Lowered {
            expr: Lowerer::guest_call(
                lower.intrinsics.expect("typed array realm").root,
                vec![
                    Expr::Var(lower.active_carrier(ident.span)?),
                    Expr::Int(super::prelude::intrinsics::typed_array_constructor_root(lower, element)),
                ],
            ),
            kind: Kind::Function,
            array_length: None,
            owned_roots: Vec::new(),
        }),
        NameHit::BuiltinBoolean => Ok(Lowered {
            expr: Lowerer::guest_call(
                lower.intrinsics.expect("Boolean realm").root,
                vec![
                    Expr::Var(lower.active_carrier(ident.span)?),
                    Expr::Int(super::prelude::intrinsics::boolean_constructor_root(lower)),
                ],
            ),
            kind: Kind::Function,
            array_length: None,
            owned_roots: Vec::new(),
        }),
        NameHit::BuiltinNumeric(i) => Ok(Lowered {
            expr: Lowerer::guest_call(
                lower.intrinsics.expect("numeric realm").root,
                vec![
                    Expr::Var(lower.active_carrier(ident.span)?),
                    Expr::Int(super::prelude::math::global_root(lower, i)),
                ],
            ),
            kind: Kind::Function,
            array_length: None,
            owned_roots: Vec::new(),
        }),
        NameHit::BuiltinUri(i) => Ok(Lowered {
            expr: original_value(lower, i + 6, ident.span)?,
            kind: Kind::Function,
            array_length: None,
            owned_roots: Vec::new(),
        }),
        NameHit::BuiltinNumber(infinity) => Ok(Lowered {
            expr: value::number(Expr::Float(if infinity { f64::INFINITY } else { f64::NAN })),
            kind: Kind::Number,
            array_length: None,
            owned_roots: Vec::new(),
        }),
        NameHit::BuiltinUndefined => Ok(Lowered {
            expr: value::undefined(),
            kind: Kind::Undefined,
            array_length: None,
            owned_roots: Vec::new(),
        }),
        NameHit::Outer(_) => Err(lower.error(
            ident.span,
            &format!(
                "P4 free-variable capture of '{}' is outside this closed subset",
                ident.sym
            ),
        )),
        NameHit::Missing => Err(lower.error(ident.span, &format!("unknown local '{}'", ident.sym))),
    }
}

pub(super) fn lower_assign_expr(
    lower: &mut Lowerer<'_>,
    assign: &ast::AssignExpr,
) -> Result<Lowered, ParseError> {
    if assign.op != ast::AssignOp::Assign {
        return lower.lower_compound(assign);
    }
    if let ast::AssignTarget::Simple(ast::SimpleAssignTarget::Member(member)) = &assign.left {
        return lower.lower_property_assignment(member, &assign.right, assign.span);
    }
    let ast::AssignTarget::Simple(ast::SimpleAssignTarget::Ident(binding)) = &assign.left else {
        return Err(lower.error(
            assign.span,
            "unsupported JavaScript ES5 P0 primitive expression",
        ));
    };
    let _target = resolve(lower, binding.sym.as_str());
    let rhs = lower.expr(&assign.right)?;
    let mut stmts = Vec::new();
    let (expr, kind) = store_local(lower, binding.sym.as_str(), binding.span, rhs, &mut stmts)?;
    let array_length = if kind == Kind::Array {
        lower.array_lengths.get(binding.sym.as_str()).copied()
    } else {
        None
    };
    Ok(Lowered {
        expr: Expr::Block {
            stmts,
            expr: Box::new(expr),
        },
        kind,
        array_length,
        owned_roots: Vec::new(),
    })
}

pub(super) fn lower_typeof(
    lower: &mut Lowerer<'_>,
    unary: &ast::UnaryExpr,
) -> Result<Lowered, ParseError> {
    if lower.root_scope.is_none() {
        return Err(lower.error(
            unary.span,
            "P3 typeof requires the realm heap for guest string payloads",
        ));
    }
    // `typeof arguments` observes the implicit activation binding: an
    // ordinary object, never an Array, so it is a static "object" name.
    if let ast::Expr::Ident(ident) = unwrap_parens(unary.arg.as_ref())
        && ident.sym == *"arguments"
        && lower.function_args.is_some()
        && implicit_arguments(lower)
    {
        let mut stmts = Vec::new();
        let slot = emit_type_name(lower, "object", unary.span, &mut stmts)?;
        return Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(Expr::Var(slot)),
            },
            kind: Kind::String,
            array_length: None,
            owned_roots: vec![slot],
        });
    }
    if let ast::Expr::Ident(ident) = unwrap_parens(unary.arg.as_ref())
        && matches!(resolve(lower, ident.sym.as_str()), NameHit::Missing)
    {
        let mut stmts = Vec::new();
        let slot = emit_type_name(lower, "undefined", unary.span, &mut stmts)?;
        return Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(Expr::Var(slot)),
            },
            kind: Kind::String,
            array_length: None,
            owned_roots: vec![slot],
        });
    }
    let operand = lower.expr(&unary.arg)?;
    let mut stmts = Vec::new();
    let mut operand_roots = operand.owned_roots;
    let (value_slot, mut bound) = lower.bind_evaluation(operand.expr, operand.kind, unary.span)?;
    stmts.append(&mut bound);
    // Loop back edges: a local written anywhere inside an enclosing iteration
    // body does not keep its entry kind across iterations, so its compile-time
    // kind is not a `typeof` proof. The tag inspection below is always correct.
    let unstable_kind = loop_unstable_operand(lower, &unary.arg);
    if !unstable_kind && let Some(text) = proven_es5_name(operand.kind) {
        let name = ensure_type_name(lower, text, unary.span, &mut stmts)?;
        let result = copy_cached_name(lower, name, unary.span, &mut stmts)?;
        if lower.has_root(value_slot) {
            operand_roots.push(value_slot);
        }
        release_consumed_roots(lower, &operand_roots, result, unary.span, &mut stmts)?;
        return Ok(Lowered {
            expr: Expr::Block {
                stmts,
                expr: Box::new(Expr::Var(result)),
            },
            kind: Kind::String,
            array_length: None,
            owned_roots: vec![result],
        });
    }
    ensure_type_names(lower, unary.span, &mut stmts)?;
    let undefined_name = require_type_name(lower, "undefined", unary.span)?;
    let object_name = require_type_name(lower, "object", unary.span)?;
    let boolean_name = require_type_name(lower, "boolean", unary.span)?;
    let number_name = require_type_name(lower, "number", unary.span)?;
    let string_name = require_type_name(lower, "string", unary.span)?;
    let function_name = require_type_name(lower, "function", unary.span)?;
    let line = lower.line(unary.span);
    let result = lower.slot(unary.span)?;
    stmts.push(Stmt::Let {
        index: result,
        declared_schema: Some(value::schema()),
        expr: Expr::Var(undefined_name),
        line,
    });
    lower.refresh_root(result, unary.span, &mut stmts)?;
    let tag = lower.slot(unary.span)?;
    stmts.push(Stmt::Let {
        index: tag,
        declared_schema: Some(TypeSchema::Int),
        expr: value::field(value_slot, "tag"),
        line,
    });
    let base = lower.slot(unary.span)?;
    let kind_num = lower.slot(unary.span)?;
    stmts.push(Stmt::Let {
        index: base,
        declared_schema: Some(TypeSchema::Int),
        expr: Expr::Int(0),
        line,
    });
    stmts.push(Stmt::Let {
        index: kind_num,
        declared_schema: Some(TypeSchema::Int),
        expr: Expr::Int(0),
        line,
    });
    let carrier = lower.active_carrier(unary.span)?;
    let heap = lower.heap_ref(unary.span)?;
    let (helper_read, checked_base) = (heap.helper_read, heap.helpers.checked_base);
    let decode = lower
        .helper_decode
        .ok_or_else(|| lower.error(unary.span, "P3 typeof requires the realm decode helper"))?;
    let assign_name = |name: u16| Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: result,
        expr: Expr::Var(name),
        line,
    };
    let object_branch = vec![
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: base,
            expr: Lowerer::guest_call(
                checked_base,
                vec![
                    Expr::Var(carrier),
                    value::field(value_slot, "ref_slot"),
                    value::field(value_slot, "generation"),
                ],
            ),
            line,
        },
        Stmt::Assign {
            kind: vm::AssignmentKind::Set,
            index: kind_num,
            expr: Lowerer::guest_call(
                decode,
                vec![Lowerer::guest_call(
                    helper_read,
                    vec![Expr::Var(carrier), Lowerer::as_number(Expr::Var(base))],
                )],
            ),
            line,
        },
        Stmt::IfElse {
            condition: Expr::Eq(
                Box::new(Expr::Var(kind_num)),
                Box::new(Expr::Int(layout::KIND_FUNCTION)),
            ),
            then_branch: vec![assign_name(function_name)],
            else_branch: vec![assign_name(object_name)],
            line,
        },
    ];
    stmts.push(Stmt::IfElse {
        condition: Expr::Eq(Box::new(Expr::Var(tag)), Box::new(Expr::Int(value::OBJECT))),
        then_branch: object_branch,
        else_branch: vec![Stmt::IfElse {
            condition: Expr::Eq(Box::new(Expr::Var(tag)), Box::new(Expr::Int(value::NULL))),
            then_branch: vec![assign_name(object_name)],
            else_branch: vec![Stmt::IfElse {
                condition: Expr::Eq(
                    Box::new(Expr::Var(tag)),
                    Box::new(Expr::Int(value::BOOLEAN)),
                ),
                then_branch: vec![assign_name(boolean_name)],
                else_branch: vec![Stmt::IfElse {
                    condition: Expr::Eq(
                        Box::new(Expr::Var(tag)),
                        Box::new(Expr::Int(value::NUMBER)),
                    ),
                    then_branch: vec![assign_name(number_name)],
                    else_branch: vec![Stmt::IfElse {
                        condition: Expr::Eq(
                            Box::new(Expr::Var(tag)),
                            Box::new(Expr::Int(value::STRING)),
                        ),
                        then_branch: vec![assign_name(string_name)],
                        else_branch: vec![Stmt::IfElse {
                            condition: Expr::Eq(
                                Box::new(Expr::Var(tag)),
                                Box::new(Expr::Int(value::UNDEFINED)),
                            ),
                            then_branch: vec![assign_name(undefined_name)],
                            else_branch: Vec::new(),
                            line,
                        }],
                        line,
                    }],
                    line,
                }],
                line,
            }],
            line,
        }],
        line,
    });
    lower.refresh_root(result, unary.span, &mut stmts)?;
    if lower.has_root(value_slot) {
        operand_roots.push(value_slot);
    }
    release_consumed_roots(lower, &operand_roots, result, unary.span, &mut stmts)?;
    Ok(Lowered {
        expr: Expr::Block {
            stmts,
            expr: Box::new(Expr::Var(result)),
        },
        kind: Kind::String,
        array_length: None,
        owned_roots: vec![result],
    })
}

pub(super) fn lower_script_statements(
    lower: &mut Lowerer<'_>,
    statements: &[ast::Stmt],
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let scope = lower.graph.script;
    lower.active_scope = scope;
    validate_statements(lower, statements)?;
    instantiate(lower, scope, out)?;
    // The scan borrows `lower` only for binding-resolved `new` classification;
    // end that borrow before the enhanced-flow check mutates the iteration
    // state.
    let (script_try, can_throw) = {
        let mut effects = SourceScan {
            lower: Some(&*lower),
            ..SourceScan::default()
        };
        effects.statements(statements, false);
        (effects.script_try, effects.may_guest_throw)
    };
    let enhanced = lower.iteration.activate_if_needed(statements) || script_try;
    if enhanced {
        lower.iteration.mode = true;
    }
    if can_throw {
        init_script_pending(lower, statements, out)?;
        instantiate_catch_bindings(lower, out)?;
    } else if enhanced {
        // Enhanced statement/value flow owns a private pending channel even
        // when no source throw exists: break/continue are abrupt Completions
        // and the Script tail reflects the real list completion.
        init_script_pending(lower, statements, out)?;
    }
    let lowered = if enhanced {
        super::iteration::lower_script(lower, statements, out)?;
        Ok(())
    } else if needs_script_completion(statements) {
        let span = statements[0].span();
        let tail = alloc_script_tail(lower, span, out)?;
        lower_script_list(lower, statements, out, &tail)?;
        let line = lower.line(span);
        out.push(Stmt::Expr {
            expr: Expr::Var(tail.present),
            line,
        });
        out.push(Stmt::Expr {
            expr: Expr::Var(tail.value),
            line,
        });
        Ok(())
    } else {
        lower_stmt_list(lower, statements, out, false)
    };
    if can_throw {
        let span = statements[0].span();
        emit_uncaught_script_throw(lower, span, out)?;
        lower.pending_normal = None;
        lower.pending_completion = None;
        lower.pending_payload = None;
    } else if enhanced {
        lower.pending_normal = None;
        lower.pending_completion = None;
        lower.pending_payload = None;
        // Keep `iteration.mode` for the remainder of this Script lowering:
        // the primitive path consults it to declare the Completion schema.
    }
    lowered
}

/// Every general Call can fail the runtime callability check, even when no
/// source Throw exists. Calls in deferred function bodies belong to those
/// activations and do not provision the Script's channel by themselves.
/// This fact drives both adapter installation and Script pending ownership.
/// `lower` supplies the binding resolution that decides whether a `new` site
/// is a builtin construction or a guest-throwing source construction.
pub(super) fn script_can_receive_callee_throw(
    lower: Option<&Lowerer<'_>>,
    statements: &[ast::Stmt],
) -> bool {
    let mut scan = SourceScan {
        lower,
        ..SourceScan::default()
    };
    scan.statements(statements, false);
    scan.may_guest_throw
}

/// Inventory micro shape: `arguments[0]` with numeric index 0 only.
/// Bare `arguments`, string keys, other indices, callee/caller/length
/// and any other member shape never match here.
pub(super) fn is_inventory_arguments_zero(member: &ast::MemberExpr) -> bool {
    let base = Lowerer::unwrap_parens(&member.obj);
    let ast::Expr::Ident(object) = base else {
        return false;
    };
    if object.sym.as_str() != "arguments" {
        return false;
    }
    let ast::MemberProp::Computed(property) = &member.prop else {
        return false;
    };
    Lowerer::member_index_literal(&property.expr) == Some(0)
}

/// Admitted `arguments` element-key shape. `Length` is `.length` (identifier
/// or String key); `Callee` is `.callee`/`["callee"]`, which the pinned
/// engine yields as Undefined in a non-strict activation; `Static` is a
/// canonical non-negative decimal index; `Absent` is a numeric literal that
/// cannot denote an array index (fractional, negative, non-finite), which
/// reads as genuine Undefined; `Dynamic` is any other computed key, checked
/// at runtime.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ArgumentsKey {
    Length,
    Callee,
    Static(i64),
    Absent,
    Dynamic,
}

/// Direct `arguments` base: parenthesized bare identifier spelling.
pub(super) fn is_arguments_base(member: &ast::MemberExpr) -> bool {
    matches!(
        Lowerer::unwrap_parens(&member.obj),
        ast::Expr::Ident(object) if object.sym == *"arguments"
    )
}

/// The exact spelling `arguments` (parentheses unwrapped).
pub(super) fn is_arguments_ident(expression: &ast::Expr) -> bool {
    matches!(
        Lowerer::unwrap_parens(expression),
        ast::Expr::Ident(object) if object.sym == *"arguments"
    )
}

/// ES5 canonical array-index text: `0` or an ASCII decimal without a sign,
/// leading zero or fraction, small enough for the invocation vector.
fn arguments_index_units(units: &[u16]) -> Option<i64> {
    if units.is_empty() || units.len() > 16 {
        return None;
    }
    if units.len() > 1 && units[0] == u16::from(b'0') {
        return None;
    }
    let mut value: i64 = 0;
    for unit in units {
        if !(u16::from(b'0')..=u16::from(b'9')).contains(unit) {
            return None;
        }
        value = value.checked_mul(10)?;
        value = value.checked_add(i64::from(*unit - u16::from(b'0')))?;
    }
    Some(value)
}

/// Classify one direct `arguments` member read. `None` means the property
/// name is outside the admitted domain (`caller`, arbitrary names and
/// non-canonical String keys).
pub(super) fn arguments_member_key(
    lower: &Lowerer<'_>,
    member: &ast::MemberExpr,
) -> Result<Option<ArgumentsKey>, ParseError> {
    match &member.prop {
        ast::MemberProp::Ident(id) => Ok(match id.sym.as_str() {
            "length" => Some(ArgumentsKey::Length),
            // Pinned Micro: `arguments.callee` is an ordinary Undefined
            // observation, not a refusal and not a TypeError.
            "callee" => Some(ArgumentsKey::Callee),
            _ => None,
        }),
        ast::MemberProp::Computed(property) => {
            let key = Lowerer::unwrap_parens(&property.expr);
            match key {
                ast::Expr::Lit(ast::Lit::Str(_)) => {
                    let units = lower.member_key_units(member)?;
                    let length: Vec<u16> = "length".encode_utf16().collect();
                    if units == length {
                        return Ok(Some(ArgumentsKey::Length));
                    }
                    let callee: Vec<u16> = "callee".encode_utf16().collect();
                    if units == callee {
                        return Ok(Some(ArgumentsKey::Callee));
                    }
                    Ok(arguments_index_units(&units).map(ArgumentsKey::Static))
                }
                ast::Expr::Lit(ast::Lit::Num(_)) => Ok(Some(
                    match Lowerer::member_index_literal(key) {
                        Some(index) => ArgumentsKey::Static(index),
                        None => ArgumentsKey::Absent,
                    },
                )),
                _ => Ok(Some(ArgumentsKey::Dynamic)),
            }
        }
        _ => Ok(None),
    }
}

/// The syntactic `Array.isArray` member spelling (identifier base).
fn is_array_is_array_syntax(member: &ast::MemberExpr) -> bool {
    let ast::Expr::Ident(object) = Lowerer::unwrap_parens(&member.obj) else {
        return false;
    };
    if object.sym != *"Array" {
        return false;
    }
    match &member.prop {
        ast::MemberProp::Ident(prop) => prop.sym == *"isArray",
        ast::MemberProp::Computed(prop) => matches!(
            Lowerer::unwrap_parens(&prop.expr),
            ast::Expr::Lit(ast::Lit::Str(text)) if text.value == *"isArray"
        ),
        _ => false,
    }
}

/// The direct `Array.isArray` callee on the builtin namespace.
pub(super) fn is_array_is_array_member(lower: &Lowerer<'_>, member: &ast::MemberExpr) -> bool {
    is_array_is_array_syntax(member)
        && matches!(resolve(lower, "Array"), NameHit::BuiltinArrayNamespace)
}

/// Inventory micro shape: `for(var k in {a:1}){}` with an empty block body.
///
/// The header is exactly `var k` with no initializer; the enumerated object
/// is an object literal with exactly one own data property `a:1`; the body
/// is empty. Any other header, object, array, prototype enumeration,
/// variable form or non-empty body never matches here and keeps the existing
/// refusal. There is no general for-in lowering; this is the single narrow
/// path.
fn is_inventory_for_in_single_a(statement: &ast::Stmt) -> bool {
    let ast::Stmt::ForIn(loop_) = statement else {
        return false;
    };
    let ast::ForHead::VarDecl(decl) = &loop_.left else {
        return false;
    };
    if decl.kind != ast::VarDeclKind::Var {
        return false;
    }
    if decl.decls.len() != 1 {
        return false;
    }
    let declarator = &decl.decls[0];
    if declarator.init.is_some() {
        return false;
    }
    let ast::Pat::Ident(binding) = &declarator.name else {
        return false;
    };
    if binding.id.sym.as_str() != "k" {
        return false;
    }
    let ast::Expr::Object(object) = loop_.right.as_ref() else {
        return false;
    };
    if object.props.len() != 1 {
        return false;
    }
    let ast::PropOrSpread::Prop(prop) = &object.props[0] else {
        return false;
    };
    let ast::Prop::KeyValue(pair) = prop.as_ref() else {
        return false;
    };
    let ast::PropName::Ident(key) = &pair.key else {
        return false;
    };
    if key.sym.as_str() != "a" {
        return false;
    }
    let ast::Expr::Lit(ast::Lit::Num(number)) = pair.value.as_ref() else {
        return false;
    };
    if number.value != 1.0 {
        return false;
    }
    let ast::Stmt::Block(body) = loop_.body.as_ref() else {
        return false;
    };
    body.stmts.is_empty()
}

/// Validate the one admitted for-in inventory shape. The RHS still runs
/// through ordinary object validation so realm demand stays exact; `k` is
/// published as a String fact for the single enumerated own key.
fn validate_inventory_for_in(
    lower: &Lowerer<'_>,
    statement: &ast::Stmt,
    facts: &mut ProfileFacts,
) -> Result<StmtFlow, ParseError> {
    let ast::Stmt::ForIn(loop_) = statement else {
        return Err(lower.error(statement.span(), "unsupported JavaScript ES5 P0 statement"));
    };
    let _ = validate_expr(lower, &loop_.right, facts)?;
    if lower.validation_abrupt() {
        lower.set_validation_abrupt(false);
        lower.clear_validation_conditional();
        let mut exits = Exits::none();
        exits.throw = Some(facts.clone());
        return Ok(StmtFlow::abrupt(exits));
    }
    let ast::ForHead::VarDecl(decl) = &loop_.left else {
        return Err(lower.error(statement.span(), "unsupported JavaScript ES5 P0 statement"));
    };
    if let ast::Pat::Ident(binding) = &decl.decls[0].name
        && let NameHit::Local(id) = resolve(lower, binding.id.sym.as_str())
    {
        facts.insert(id, ProfileFact::scalar(Kind::String));
    }
    Ok(StmtFlow::normal())
}

/// Lower the one admitted for-in inventory shape. The single own key `a` is
/// enumerated once and bound to `k` as the string "a" before the empty body;
/// the loop publishes no sequence value itself. No prototype, array, other
/// object, other variable form or non-empty body is handled here.
fn lower_inventory_for_in(
    lower: &mut Lowerer<'_>,
    statement: &ast::Stmt,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let ast::Stmt::ForIn(loop_) = statement else {
        return Err(lower.error(statement.span(), "unsupported JavaScript ES5 P0 statement"));
    };
    let span = loop_.span;
    let mut made = Vec::new();
    let boxed = lower.emit_utf16_node(&[97u16], span, &mut made)?;
    let lowered = Lowered {
        expr: Expr::Block {
            stmts: made,
            expr: Box::new(Expr::Var(boxed)),
        },
        kind: Kind::String,
        array_length: None,
        owned_roots: vec![boxed],
    };
    let ast::ForHead::VarDecl(decl) = &loop_.left else {
        return Err(lower.error(statement.span(), "unsupported JavaScript ES5 P0 statement"));
    };
    let ast::Pat::Ident(binding) = &decl.decls[0].name else {
        return Err(lower.error(statement.span(), "unsupported JavaScript ES5 P0 statement"));
    };
    let _ = store_local(lower, binding.id.sym.as_str(), span, lowered, out)?;
    Ok(())
}

/// Validate one direct `arguments` read. Only the shape is admitted here;
/// emission reads the invocation vector installed for the activation.
fn validate_arguments_read(
    lower: &Lowerer<'_>,
    member: &ast::MemberExpr,
    facts: &mut ProfileFacts,
) -> Result<ProfileFact, ParseError> {
    match arguments_member_key(lower, member)? {
        Some(ArgumentsKey::Length) => Ok(ProfileFact::scalar(Kind::Number)),
        // Pinned Micro yields Undefined for `arguments.callee`.
        Some(ArgumentsKey::Callee) => Ok(ProfileFact::scalar(Kind::Undefined)),
        Some(ArgumentsKey::Static(_) | ArgumentsKey::Absent) => {
            Ok(ProfileFact::scalar(Kind::Dynamic))
        }
        Some(ArgumentsKey::Dynamic) => {
            let ast::MemberProp::Computed(property) = &member.prop else {
                unreachable!("dynamic arguments key is computed");
            };
            let key = validate_expr(lower, &property.expr, facts)?;
            // A String key is the ordinary property-name route: the pinned
            // binding is a real Array, so `"1"`/`"length"` are own rows.
            if !matches!(key.kind, Kind::Number | Kind::String | Kind::Dynamic) {
                return Err(lower.error(
                    property.span(),
                    "P4 arguments subscripts must be numeric or String in this scope",
                ));
            }
            Ok(ProfileFact::scalar(Kind::Dynamic))
        }
        None => Err(lower.error(
            member.span,
            "P4 arguments observations admit only .length, .callee and numeric indexes",
        )),
    }
}

/// Validate one `arguments[index] = value` write. The index must be a
/// canonical numeric constant; the value is an ordinary expression.
/// `arguments.length = value` is the one admitted name write: it really
/// resizes the invocation vector (truncate/extend), yields the RHS value and
/// can raise the guest `TypeError("invalid array length")`.
fn validate_arguments_store(
    lower: &Lowerer<'_>,
    member: &ast::MemberExpr,
    right: &ast::Expr,
    facts: &mut ProfileFacts,
) -> Result<(), ParseError> {
    match arguments_member_key(lower, member)? {
        Some(ArgumentsKey::Static(_) | ArgumentsKey::Length) => {
            // A resize or a jumping element store raises a guest TypeError,
            // so the private allocator/prototype must exist.
            lower.needs_ordinary_realm.set(true);
            let _ = validate_expr(lower, right, facts)?;
            Ok(())
        }
        _ => Err(lower.error(
            member.span,
            "P4 arguments writes admit only a canonical numeric index",
        )),
    }
}

/// Inventory function shape: no formals and a single `return arguments[0];`.
/// Parens around the returned member are unwrapped; anything else fails.
fn is_inventory_function(function: &ast::Function) -> bool {
    if !function.params.is_empty() {
        return false;
    }
    let Some(body) = function.body.as_ref() else {
        return false;
    };
    if body.stmts.len() != 1 {
        return false;
    }
    let ast::Stmt::Return(ret) = &body.stmts[0] else {
        return false;
    };
    let Some(arg) = ret.arg.as_deref() else {
        return false;
    };
    let unwrapped = Lowerer::unwrap_parens(arg);
    let ast::Expr::Member(member) = unwrapped else {
        return false;
    };
    is_inventory_arguments_zero(member)
}

/// Admit only activations independent of unimplemented sloppy call context.
/// This runs before code IDs and helper demand are collected.
pub(super) fn admit_closed_sloppy_function(
    lower: &Lowerer<'_>,
    function: &ast::Function,
) -> Result<(), ParseError> {
    // Narrow inventory exception: `function f(){return arguments[0];}`.
    // Non-strict only; the caller already gates on `!strict`.
    if is_inventory_function(function) {
        return Ok(());
    }
    let mut scan = SourceScan {
        activation_local: true,
        arguments_allowed: true,
        lower: Some(lower),
        ..SourceScan::default()
    };
    for param in &function.params {
        let ast::Pat::Ident(binding) = &param.pat else {
            return Err(lower.error(param.span, "P0 non-strict functions require plain formals"));
        };
        let name = binding.id.sym.as_str();
        if name == "arguments" || name == "eval" {
            return Err(lower.error(
                binding.id.span,
                "P0 non-strict functions reject arguments/eval formals",
            ));
        }
        scan.pat(&param.pat, false);
    }
    // Do not call scan.function(root): in this mode only nested activations
    // are skipped; the root's own parameters/body must be scanned directly.
    if let Some(body) = &function.body {
        scan.statements(&body.stmts, false);
    }
    if let Some((span, message)) = scan.activation_refusal {
        return Err(lower.error(span, message));
    }
    Ok(())
}

/// Complete admitted-AST call walk. `deferred` keeps separate activations
/// from contributing their calls to the enclosing Script.
#[derive(Default)]
struct SourceScan<'a> {
    may_guest_throw: bool,
    script_try: bool,
    possible_number_member: bool,
    possible_primitive_key: bool,
    possible_string_length: bool,
    possible_string_concat: bool,
    scan_string_concat: bool,
    possible_relational: bool,
    /// A `new` expression in this activation. A body whose only feature site
    /// is `new F()` must still be feature-validated: the private construct
    /// helper and the `Function.prototype` row family are selected by that
    /// validation, not by an unrelated member/concat/relational site.
    possible_construct: bool,
    activation_local: bool,
    /// Admission of `arguments` observations for one source activation.
    arguments_allowed: bool,
    /// Syntactic `Array.isArray(arguments)` seen anywhere in the walk. The
    /// lowering needs one private boxing helper (Core vector -> guest Array)
    /// before any source function body is emitted.
    array_is_array_arguments: bool,
    /// Syntactic computed `arguments[<non-literal>]` read. Such a key can be a
    /// guest String at runtime, so the private String-key classifier must
    /// exist before any source function body is emitted.
    arguments_dynamic_key: bool,
    activation_refusal: Option<(swc_common::Span, &'static str)>,
    /// Binding context for `new` classification. Present whenever the caller
    /// owns the scope graph; the parser-stage helpers pass `None` and keep the
    /// spelling-only answer because they never decide a Script throw channel.
    lower: Option<&'a Lowerer<'a>>,
}

/// True when any statement/expression in the source passes the bare
/// activation `arguments` binding to `Array.isArray`. The check is syntactic:
/// the helper is also harmless for a shadowed `Array`.
pub(super) fn source_uses_array_is_array_arguments(statements: &[ast::Stmt]) -> bool {
    let mut scan = SourceScan::default();
    scan.statements(statements, false);
    scan.array_is_array_arguments
}

/// True when any statement in the source reads `arguments` through a
/// computed subscript that is not a literal. The check is syntactic: a
/// literal key never needs the String classifier, and a shadowed `arguments`
/// only pays an unused helper.
pub(super) fn source_uses_arguments_dynamic_key(statements: &[ast::Stmt]) -> bool {
    let mut scan = SourceScan {
        arguments_allowed: true,
        ..SourceScan::default()
    };
    scan.statements(statements, false);
    scan.arguments_dynamic_key
}

pub(super) fn numeric_operand_may_throw(expression: &ast::Expr) -> bool {
    let mut scan = SourceScan::default();
    scan.expr(expression, false);
    scan.may_guest_throw
}

impl SourceScan<'_> {
    fn refuse_activation(&mut self, span: swc_common::Span, message: &'static str) {
        if self.activation_local && self.activation_refusal.is_none() {
            self.activation_refusal = Some((span, message));
        }
    }

    fn identifier(&mut self, ident: &ast::Ident) {
        if self.activation_local && (ident.sym == *"arguments" || ident.sym == *"eval") {
            self.refuse_activation(
                ident.span,
                "P0 non-strict functions reject unqualified arguments/eval observations",
            );
        }
    }

    /// Whether this `new` lowers through a builtin construction route that
    /// carries no guest-visible `Err`. The spelling is necessary, not
    /// sufficient: the callee must still resolve to the intrinsic, exactly as
    /// `Lowerer::lower_new_expr` / `validate_new_expr` decide it. A source
    /// binding over the same spelling selects `lower_source_construct`, whose
    /// failure is a guest throw and therefore needs the pending channel.
    fn new_is_builtin_construct(&self, callee: &ast::Expr) -> bool {
        let ast::Expr::Ident(callee) = unwrap_parens(callee) else {
            return false;
        };
        let name = callee.sym.as_str();
        if !new_spelling_is_builtin_construct(name) {
            return false;
        }
        let Some(lower) = self.lower else {
            // No scope graph (parser-stage callers): the spelling is all the
            // information available, and those callers never provision the
            // Script channel.
            return true;
        };
        match name {
            "Object" => matches!(resolve(lower, name), NameHit::BuiltinObject),
            "Error" | "Date" => matches!(resolve(lower, name), NameHit::Missing),
            "Array" => matches!(resolve(lower, name), NameHit::BuiltinArrayNamespace),
            "ArrayBuffer" => matches!(resolve(lower, name), NameHit::BuiltinArrayBuffer),
            "TypeError" => matches!(resolve(lower, name), NameHit::BuiltinTypeError),
            "RangeError" => matches!(resolve(lower, name), NameHit::BuiltinRangeError),
            _ => matches!(
                resolve(lower, name),
                NameHit::BuiltinOptionalError(_) | NameHit::BuiltinTypedArray(_)
            ),
        }
    }

    fn statements(&mut self, statements: &[ast::Stmt], deferred: bool) {
        for statement in statements {
            self.statement(statement, deferred);
        }
    }

    fn statement(&mut self, statement: &ast::Stmt, deferred: bool) {
        match statement {
            ast::Stmt::Block(block) => self.statements(&block.stmts, deferred),
            ast::Stmt::Empty(_)
            | ast::Stmt::Debugger(_)
            | ast::Stmt::Break(_)
            | ast::Stmt::Continue(_) => {}
            ast::Stmt::With(with_stmt) => {
                self.expr(&with_stmt.obj, deferred);
                self.statement(&with_stmt.body, deferred);
            }
            ast::Stmt::Return(ret) => {
                if let Some(arg) = &ret.arg {
                    self.expr(arg, deferred);
                }
            }
            ast::Stmt::Labeled(labeled) => self.statement(&labeled.body, deferred),
            ast::Stmt::If(branch) => {
                self.expr(&branch.test, deferred);
                self.statement(&branch.cons, deferred);
                if let Some(alt) = &branch.alt {
                    self.statement(alt, deferred);
                }
            }
            ast::Stmt::Switch(select) => {
                self.expr(&select.discriminant, deferred);
                for case in &select.cases {
                    if let Some(test) = &case.test {
                        self.expr(test, deferred);
                    }
                    self.statements(&case.cons, deferred);
                }
            }
            ast::Stmt::Throw(throw) => {
                if !deferred {
                    self.may_guest_throw = true;
                }
                self.expr(&throw.arg, deferred);
            }
            ast::Stmt::Try(guarded) => {
                if !deferred {
                    self.script_try = true;
                    self.may_guest_throw = true;
                }
                self.statements(&guarded.block.stmts, deferred);
                if let Some(handler) = &guarded.handler {
                    if let Some(param) = &handler.param {
                        self.pat(param, deferred);
                    }
                    self.statements(&handler.body.stmts, deferred);
                }
                if let Some(finalizer) = &guarded.finalizer {
                    self.statements(&finalizer.stmts, deferred);
                }
            }
            ast::Stmt::While(loop_) => {
                self.expr(&loop_.test, deferred);
                self.statement(&loop_.body, deferred);
            }
            ast::Stmt::DoWhile(loop_) => {
                self.statement(&loop_.body, deferred);
                self.expr(&loop_.test, deferred);
            }
            ast::Stmt::For(loop_) => {
                if let Some(init) = &loop_.init {
                    match init {
                        ast::VarDeclOrExpr::VarDecl(declaration) => {
                            self.var_decl(declaration, deferred);
                        }
                        ast::VarDeclOrExpr::Expr(expr) => self.expr(expr, deferred),
                    }
                }
                if let Some(test) = &loop_.test {
                    self.expr(test, deferred);
                }
                if let Some(update) = &loop_.update {
                    self.expr(update, deferred);
                }
                self.statement(&loop_.body, deferred);
            }
            ast::Stmt::ForIn(loop_) => {
                self.for_head(&loop_.left, deferred);
                self.expr(&loop_.right, deferred);
                self.statement(&loop_.body, deferred);
            }
            ast::Stmt::ForOf(loop_) => {
                // Numeric dense-Array iteration can throw a managed TypeError
                // for a non-array RHS, and the body can throw.
                if !deferred {
                    self.may_guest_throw = true;
                }
                self.for_head(&loop_.left, deferred);
                self.expr(&loop_.right, deferred);
                self.statement(&loop_.body, deferred);
            }
            ast::Stmt::Decl(declaration) => self.decl(declaration, deferred),
            ast::Stmt::Expr(write) => self.expr(&write.expr, deferred),
        }
    }

    fn for_head(&mut self, head: &ast::ForHead, deferred: bool) {
        match head {
            ast::ForHead::VarDecl(declaration) => self.var_decl(declaration, deferred),
            ast::ForHead::Pat(pat) => self.pat(pat, deferred),
            ast::ForHead::UsingDecl(_) => {}
        }
    }

    fn decl(&mut self, declaration: &ast::Decl, deferred: bool) {
        match declaration {
            ast::Decl::Fn(function) => self.function(&function.function),
            ast::Decl::Var(declaration) => self.var_decl(declaration, deferred),
            // Class/TypeScript/using declarations are admission refusals.
            _ => {}
        }
    }

    fn var_decl(&mut self, declaration: &ast::VarDecl, deferred: bool) {
        for declarator in &declaration.decls {
            self.pat(&declarator.name, deferred);
            if let Some(init) = &declarator.init {
                self.expr(init, deferred);
            }
        }
    }

    /// A function body is a deferred activation: everything inside it is
    /// `deferred`, including nested functions and their parameters.
    fn function(&mut self, function: &ast::Function) {
        if self.activation_local {
            return;
        }
        for param in &function.params {
            self.pat(&param.pat, true);
        }
        if let Some(body) = &function.body {
            self.statements(&body.stmts, true);
        }
    }

    fn expr(&mut self, expr: &ast::Expr, deferred: bool) {
        match expr {
            ast::Expr::This(this) => self.refuse_activation(
                this.span,
                "P0 non-strict functions reject own this observations",
            ),
            ast::Expr::Ident(ident) => self.identifier(ident),
            ast::Expr::Lit(_) => {}
            ast::Expr::Paren(paren) => self.expr(&paren.expr, deferred),
            ast::Expr::Array(literal) => {
                for element in literal.elems.iter().flatten() {
                    self.expr(&element.expr, deferred);
                }
            }
            ast::Expr::Object(literal) => {
                for prop in &literal.props {
                    match prop {
                        ast::PropOrSpread::Prop(prop) => match prop.as_ref() {
                            ast::Prop::KeyValue(pair) => {
                                self.prop_name(&pair.key, deferred);
                                self.expr(&pair.value, deferred);
                            }
                            ast::Prop::Getter(getter) => {
                                self.prop_name(&getter.key, deferred);
                                self.function(&getter.function);
                            }
                            ast::Prop::Setter(setter) => {
                                self.prop_name(&setter.key, deferred);
                                self.function(&setter.function);
                            }
                            ast::Prop::Method(method) => {
                                self.prop_name(&method.key, deferred);
                                self.function(&method.function);
                            }
                            ast::Prop::Shorthand(ident) => self.identifier(ident),
                            ast::Prop::Assign(assign) => {
                                self.identifier(&assign.key);
                                self.expr(&assign.value, deferred);
                            }
                        },
                        ast::PropOrSpread::Spread(spread) => self.expr(&spread.expr, deferred),
                    }
                }
            }
            ast::Expr::Fn(function) => self.function(&function.function),
            ast::Expr::Unary(unary) => {
                // `typeof arguments` is a supported observation: the implicit
                // binding is an ordinary Object, never an Array.
                if self.arguments_allowed
                    && unary.op == ast::UnaryOp::TypeOf
                    && is_arguments_ident(&unary.arg)
                {
                    return;
                }
                self.expr(&unary.arg, deferred)
            }
            ast::Expr::Update(update) => self.expr(&update.arg, deferred),
            ast::Expr::Bin(binary) => {
                if self.scan_string_concat && !deferred && binary.op == ast::BinaryOp::Add {
                    self.possible_string_concat = true;
                }
                if self.scan_string_concat
                    && !deferred
                    && super::lowerer::relational::Relation::from_op(binary.op).is_some()
                {
                    self.possible_relational = true;
                }
                if !deferred && binary.op == ast::BinaryOp::In {
                    self.may_guest_throw = true;
                    self.possible_primitive_key = true;
                }
                self.expr(&binary.left, deferred);
                self.expr(&binary.right, deferred);
            }
            ast::Expr::Assign(assign) => {
                self.assign_target(&assign.left, deferred);
                self.expr(&assign.right, deferred);
            }
            ast::Expr::Member(member) => self.member(member, deferred, false),
            ast::Expr::Cond(branch) => {
                self.expr(&branch.test, deferred);
                self.expr(&branch.cons, deferred);
                self.expr(&branch.alt, deferred);
            }
            ast::Expr::Call(call) => {
                // Any applied call in a Script position counts, whatever the
                // callee spelling is: identifier, member (identifier, string
                // literal or computed key), function value, alias or
                // parenthesized form.
                if !deferred {
                    self.may_guest_throw = true;
                }
                // `Array.isArray(arguments)` passes the activation's implicit
                // binding as a real actual: the pinned engine materializes it
                // as a real Array. Only the bare-`arguments` actual is skipped
                // here (its value is the invocation vector, never a refused
                // unqualified observation); the call itself is a real Get and
                // Call so a rewritten `Array.isArray` is observed.
                let array_is_array_arguments = call.args.len() == 1
                    && is_arguments_ident(&call.args[0].expr)
                    && matches!(&call.callee, ast::Callee::Expr(callee)
                        if matches!(Lowerer::unwrap_parens(callee),
                            ast::Expr::Member(member) if is_array_is_array_syntax(member)));
                if array_is_array_arguments {
                    self.array_is_array_arguments = true;
                }
                let arguments_actual = self.arguments_allowed && array_is_array_arguments;
                self.callee(&call.callee, deferred);
                for arg in &call.args {
                    if arguments_actual {
                        continue;
                    }
                    self.expr(&arg.expr, deferred);
                }
            }
            ast::Expr::New(construct) => {
                if !deferred {
                    self.possible_construct = true;
                }
                // A source `new F(...)` keeps the constructor body's throw and
                // the prototype Get throw as guest-visible Err results, so the
                // Script needs the pending channel even when no other Call,
                // Throw or Try spelling appears. Built-in constructions do not
                // enter that channel: `new Object()`/`new Error("boom")`/...
                // keep the catalog-free compile they had before. Number/
                // String/Boolean are the exception because their upstream
                // construction is a runtime TypeError.
                //
                // "Built-in" here follows the binding, not the spelling: the
                // same resolution `Lowerer::lower_new_expr` and
                // `validate_new_expr` dispatch on. `var Object=F` makes this a
                // source construction of F, whose `Err` is a guest throw.
                if !deferred && !self.new_is_builtin_construct(&construct.callee) {
                    self.may_guest_throw = true;
                }
                self.expr(&construct.callee, deferred);
                if let Some(args) = &construct.args {
                    for arg in args {
                        self.expr(&arg.expr, deferred);
                    }
                }
            }
            ast::Expr::Seq(sequence) => {
                for expr in &sequence.exprs {
                    self.expr(expr, deferred);
                }
            }
            ast::Expr::Tpl(template) => {
                for expr in &template.exprs {
                    self.expr(expr, deferred);
                }
            }
            ast::Expr::TaggedTpl(tagged) => {
                self.expr(&tagged.tag, deferred);
                for expr in &tagged.tpl.exprs {
                    self.expr(expr, deferred);
                }
            }
            // Remaining variants are admission refusals (arrow/class/TS/JSX/
            // super/private/optional chaining) and cannot occur in an
            // admitted program.
            _ => {}
        }
    }

    fn member(&mut self, member: &ast::MemberExpr, deferred: bool, write: bool) {
        // Admitted `arguments` observations: `.length`, `.callee` and computed
        // element reads/writes. The guarded read cannot throw by itself, so
        // the generic member throw note is skipped; a computed key still
        // scans through its own expression (allowing
        // `arguments[arguments.length-1]`). Names outside that domain are
        // refused here with the activation-local diagnostic instead of the
        // generic unqualified-`arguments` message from `identifier`.
        if self.arguments_allowed && is_arguments_base(member) {
            match &member.prop {
                ast::MemberProp::Ident(id) if id.sym == *"length" || id.sym == *"callee" => {
                    return;
                }
                ast::MemberProp::Computed(computed) => {
                    if !matches!(
                        Lowerer::unwrap_parens(&computed.expr),
                        ast::Expr::Lit(ast::Lit::Str(_)) | ast::Expr::Lit(ast::Lit::Num(_))
                    ) {
                        self.arguments_dynamic_key = true;
                    }
                    self.expr(&computed.expr, deferred);
                    return;
                }
                _ => {
                    self.refuse_activation(
                        member.span,
                        if write {
                            "P4 arguments writes admit only a canonical numeric index"
                        } else {
                            "P4 arguments observations admit only .length, .callee and numeric indexes"
                        },
                    );
                    return;
                }
            }
        }
        if !deferred {
            self.may_guest_throw = true;
            let candidate = Lowerer::number_key_candidate(member);
            self.possible_number_member |= candidate;
            self.possible_primitive_key |= Lowerer::open_computed_key(member).is_some();
            self.possible_string_length |= possible_string_length_member(member);
        }
        self.expr(&member.obj, deferred);
        if let ast::MemberProp::Computed(computed) = &member.prop {
            self.expr(&computed.expr, deferred);
        }
    }

    fn callee(&mut self, callee: &ast::Callee, deferred: bool) {
        if let ast::Callee::Expr(expr) = callee {
            self.expr(expr, deferred);
        }
    }

    /// Key positions can hold a computed expression; it evaluates with the
    /// containing construct, so it inherits the same deferred state.
    fn prop_name(&mut self, key: &ast::PropName, deferred: bool) {
        if let ast::PropName::Computed(computed) = key {
            self.expr(&computed.expr, deferred);
        }
    }

    fn assign_target(&mut self, target: &ast::AssignTarget, deferred: bool) {
        match target {
            ast::AssignTarget::Simple(simple) => match simple {
                ast::SimpleAssignTarget::Member(member) => self.member(member, deferred, true),
                ast::SimpleAssignTarget::Paren(paren) => self.expr(&paren.expr, deferred),
                ast::SimpleAssignTarget::SuperProp(_)
                | ast::SimpleAssignTarget::OptChain(_)
                | ast::SimpleAssignTarget::TsAs(_)
                | ast::SimpleAssignTarget::TsSatisfies(_)
                | ast::SimpleAssignTarget::TsNonNull(_)
                | ast::SimpleAssignTarget::TsTypeAssertion(_)
                | ast::SimpleAssignTarget::TsInstantiation(_)
                | ast::SimpleAssignTarget::Invalid(_) => {}
                ast::SimpleAssignTarget::Ident(binding) => self.identifier(&binding.id),
            },
            ast::AssignTarget::Pat(pat) => match pat {
                ast::AssignTargetPat::Array(array) => self.array_pat(array, deferred),
                ast::AssignTargetPat::Object(object) => self.object_pat(object, deferred),
                ast::AssignTargetPat::Invalid(_) => {}
            },
        }
    }

    fn pat(&mut self, pat: &ast::Pat, deferred: bool) {
        match pat {
            ast::Pat::Ident(_) | ast::Pat::Invalid(_) => {}
            ast::Pat::Array(array) => self.array_pat(array, deferred),
            ast::Pat::Object(object) => self.object_pat(object, deferred),
            ast::Pat::Rest(rest) => self.pat(&rest.arg, deferred),
            ast::Pat::Assign(assign) => {
                self.pat(&assign.left, deferred);
                self.expr(&assign.right, deferred);
            }
            ast::Pat::Expr(expr) => self.expr(expr, deferred),
        }
    }

    fn array_pat(&mut self, pat: &ast::ArrayPat, deferred: bool) {
        for element in pat.elems.iter().flatten() {
            self.pat(element, deferred);
        }
    }

    fn object_pat(&mut self, pat: &ast::ObjectPat, deferred: bool) {
        for prop in &pat.props {
            match prop {
                ast::ObjectPatProp::KeyValue(pair) => {
                    self.prop_name(&pair.key, deferred);
                    self.pat(&pair.value, deferred);
                }
                ast::ObjectPatProp::Assign(assign) => {
                    if let Some(value) = &assign.value {
                        self.expr(value, deferred);
                    }
                }
                ast::ObjectPatProp::Rest(rest) => self.pat(&rest.arg, deferred),
            }
        }
    }
}

fn script_pending_tail(
    lower: &Lowerer<'_>,
    span: swc_common::Span,
) -> Result<FunctionTail, ParseError> {
    let normal = lower
        .pending_normal
        .ok_or_else(|| lower.error(span, "script throw pending normal is missing"))?;
    let completion = lower
        .pending_completion
        .ok_or_else(|| lower.error(span, "script throw pending completion is missing"))?;
    let payload = lower
        .pending_payload
        .ok_or_else(|| lower.error(span, "script throw pending payload is missing"))?;
    Ok(FunctionTail {
        normal,
        completion,
        payload,
    })
}

fn init_script_pending(
    lower: &mut Lowerer<'_>,
    statements: &[ast::Stmt],
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let span = statements[0].span();
    let line = lower.line(span);
    let normal = lower.slot(span)?;
    out.push(Stmt::Let {
        index: normal,
        declared_schema: Some(TypeSchema::Bool),
        expr: Expr::Bool(true),
        line,
    });
    let completion = lower.slot(span)?;
    out.push(Stmt::Let {
        index: completion,
        declared_schema: Some(value::completion_schema()),
        expr: value::completion_normal(),
        line,
    });
    let (payload, payload_stmt) = lower.bind_temp_bare(value::undefined(), span)?;
    out.push(payload_stmt);
    lower.reserve_root(payload, span)?;
    lower.pending_normal = Some(normal);
    lower.pending_completion = Some(completion);
    lower.pending_payload = Some(payload);
    Ok(())
}

fn emit_uncaught_script_throw(
    lower: &mut Lowerer<'_>,
    span: swc_common::Span,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let Some(host) = lower.uncaught_host else {
        return Err(lower.error(
            span,
            "JavaScript uncaught throw requires the exact typescript::throw_error(message: string): null host catalog entry",
        ));
    };
    let Some(completion) = lower.pending_completion else {
        return Err(lower.error(span, "script throw pending completion is missing"));
    };
    let Some(payload) = lower.pending_payload else {
        return Err(lower.error(span, "script throw pending payload is missing"));
    };
    let line = lower.line(span);
    let null_slot = lower.slot(span)?;
    let mut then_branch = vec![Stmt::Let {
        index: null_slot,
        declared_schema: Some(TypeSchema::Null),
        expr: Lowerer::guest_call(host, vec![Expr::String("uncaught JavaScript throw".into())]),
        line,
    }];
    // The completion record already copied the box. A later root refresh is
    // a real use, so lifetime compaction cannot null the payload local
    // before the host error is observed.
    lower.refresh_root(payload, span, &mut then_branch)?;
    out.push(Stmt::IfElse {
        condition: Expr::Eq(
            Box::new(value::field(completion, "kind")),
            Box::new(Expr::Int(value::COMPLETION_THROW)),
        ),
        then_branch,
        else_branch: Vec::new(),
        line,
    });
    Ok(())
}

pub(super) fn lower_function_body(
    lower: &mut Lowerer<'_>,
    statements: &[ast::Stmt],
    out: &mut Vec<Stmt>,
) -> Result<Option<Expr>, ParseError> {
    validate_statements(lower, statements)?;
    instantiate(lower, lower.active_scope, out)?;
    // P6B-NESTED-CALLEE-PROPAGATION: every activation owns pending
    // Completion and a distinct rooted payload, even a legacy-shaped
    // return/expression-only wrapper. Own-body Throw/Try detection alone
    // must not drop intermediate Err propagation.
    instantiate_catch_bindings(lower, out)?;
    Ok(Some(lower_function_controlled(lower, statements, out)?))
}

fn instantiate_catch_bindings(
    lower: &mut Lowerer<'_>,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let function_scope = lower.active_scope;
    let catch_scopes: Vec<ScopeId> = lower
        .graph
        .scopes
        .iter()
        .filter(|scope| {
            scope.kind == super::scope::ScopeKind::Catch
                && same_activation(lower, scope.id, function_scope)
        })
        .map(|scope| scope.id)
        .collect();
    for scope in catch_scopes {
        let order = lower.graph.scope(scope).order.clone();
        for id in order {
            predeclare(lower, id, scope, out)?;
            let slot = lower.binding_slots[&id];
            let span = lower.graph.binding(id).first_span;
            lower.refresh_root(slot, span, out)?;
        }
    }
    Ok(())
}

fn same_activation(lower: &Lowerer<'_>, mut scope: ScopeId, function_scope: ScopeId) -> bool {
    loop {
        if scope == function_scope {
            return true;
        }
        if lower.graph.scope(scope).kind == super::scope::ScopeKind::Function {
            return false;
        }
        let Some(parent) = lower.graph.scope(scope).parent else {
            return false;
        };
        scope = parent;
    }
}

fn instantiate(
    lower: &mut Lowerer<'_>,
    scope: ScopeId,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let order = lower.graph.scope(scope).order.clone();
    for id in &order {
        predeclare(lower, *id, scope, out)?;
    }
    for (id, ordinal, span) in lower.graph.scope(scope).formals.clone() {
        let args = lower.function_args.ok_or_else(|| {
            lower.error(
                span,
                "P4 formal initialization requires the invocation argument vector",
            )
        })?;
        // The ordinal is a compiler-owned nonnegative constant. Get occurs
        // only in the in-bounds branch; missing actuals are genuine Undefined.
        // Borrow prevents Core's consuming element-read optimization from
        // detaching a supplied box from the retained invocation vector.
        let argument = Expr::IfElse {
            condition: Box::new(Expr::Lt(
                Box::new(Expr::Int(ordinal as i64)),
                Box::new(Lowerer::builtin_call(
                    BuiltinFunction::Len.call_index(),
                    vec![Expr::Var(args)],
                )),
            )),
            then_expr: Box::new(Lowerer::builtin_call(
                BuiltinFunction::Get.call_index(),
                vec![
                    Expr::Borrow(Box::new(Expr::Var(args))),
                    Expr::Int(ordinal as i64),
                ],
            )),
            else_expr: Box::new(value::undefined()),
        };
        let name = lower.graph.binding(id).name.clone();
        store_local(
            lower,
            &name,
            span,
            Lowered {
                expr: argument,
                kind: Kind::Dynamic,
                array_length: None,
                owned_roots: Vec::new(),
            },
            out,
        )?;
    }
    for id in order {
        install_functions(lower, id, out)?;
    }
    Ok(())
}

fn predeclare(
    lower: &mut Lowerer<'_>,
    id: BindingId,
    scope: ScopeId,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let name = lower.graph.binding(id).name.clone();
    let span = lower.graph.binding(id).first_span;
    if scope == lower.graph.script && name == "undefined" {
        return Err(lower.error(
            span,
            "global name 'undefined' remains outside supported var redeclaration until global-object descriptors exist",
        ));
    }
    if scope == lower.graph.script
        && let Some(i) = super::global_binding::index(&name)
        && (!lower.globals.contains(i) || lower.globals.shared)
    {
        return Ok(());
    }
    let slot = lower.slot(span)?;
    let line = lower.line(span);
    let initial = if scope == lower.graph.script {
        super::global_binding::index(&name).filter(|i| lower.globals.initial & (1 << i) != 0)
    } else {
        None
    };
    let kind = initial.map(original_kind).unwrap_or(Kind::Undefined);
    let initial = if let Some(i) = initial {
        original_value(lower, i, span)?
    } else {
        value::undefined()
    };
    out.push(Stmt::Let {
        index: slot,
        declared_schema: Some(value::schema()),
        expr: initial,
        line,
    });
    lower.binding_slots.insert(id, slot);
    lower.binding_kinds.insert(id, kind);
    lower.bindings.insert(name.clone(), (slot, kind));
    if kind != Kind::Undefined && lower.root_scope.is_some() {
        lower.refresh_root(slot, span, out)?;
    }
    if scope == lower.graph.script {
        lower.source_bindings.push((name, slot));
    }
    Ok(())
}

fn install_functions(
    lower: &mut Lowerer<'_>,
    id: BindingId,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let sites = lower.graph.binding(id).fn_sites.clone();
    if sites.is_empty() {
        return Ok(());
    }
    if lower.heap.is_none() {
        return Err(lower.error(
            sites[0].span,
            "P3 function declarations require the realm heap",
        ));
    }
    let name = lower.graph.binding(id).name.clone();
    for site in sites {
        let synthetic = ast::FnExpr {
            ident: None,
            function: site.function,
        };
        let lowered = lower.lower_function_expr(&synthetic, Some(&name))?;
        let _ = store_local(lower, &name, site.span, lowered, out)?;
    }
    Ok(())
}

fn lower_stmt_list(
    lower: &mut Lowerer<'_>,
    statements: &[ast::Stmt],
    out: &mut Vec<Stmt>,
    in_function: bool,
) -> Result<(), ParseError> {
    let mut index = 0;
    while index < statements.len() {
        if is_script_strict_directive(lower, &statements[index]) && !in_function {
            index += 1;
            continue;
        }
        if lower.statement_is_dead(statements[index].span()) {
            let start = index;
            while index < statements.len() {
                if is_script_strict_directive(lower, &statements[index]) && !in_function {
                    index += 1;
                    continue;
                }
                if !lower.statement_is_dead(statements[index].span()) {
                    break;
                }
                index += 1;
            }
            let run = &statements[start..index];
            lower.speculate_dead(|lower| {
                for statement in run {
                    if is_script_strict_directive(lower, statement) && !in_function {
                        continue;
                    }
                    let mut discarded = Vec::new();
                    lower_one(lower, statement, &mut discarded, in_function, true)?;
                }
                Ok(())
            })?;
            continue;
        }
        lower_one(lower, &statements[index], out, in_function, true)?;
        index += 1;
    }
    Ok(())
}

fn lower_one(
    lower: &mut Lowerer<'_>,
    statement: &ast::Stmt,
    out: &mut Vec<Stmt>,
    in_function: bool,
    push_expr: bool,
) -> Result<(), ParseError> {
    match statement {
        ast::Stmt::Decl(ast::Decl::Var(declaration)) => lower_var(lower, declaration, out),
        ast::Stmt::Decl(ast::Decl::Fn(_)) => Ok(()),
        ast::Stmt::Block(block) => lower_stmt_list(lower, &block.stmts, out, in_function),
        ast::Stmt::If(if_stmt) if is_declaration_only_if(if_stmt) => {
            lower_declaration_if(lower, if_stmt, out, in_function, push_expr)
        }
        ast::Stmt::Expr(expr) => lower_expr_stmt(lower, expr, out, push_expr).map(|_| ()),
        // With no active debugging facility a debugger statement is Normal
        // with no observable effect: the existing empty-statement path.
        ast::Stmt::Empty(_) | ast::Stmt::Debugger(_) => Ok(()),
        ast::Stmt::Throw(throw) if lower.pending_normal.is_some() => {
            let tail = script_pending_tail(lower, throw.span)?;
            lower_controlled_throw(lower, throw, out, &tail)
        }
        ast::Stmt::Try(try_stmt) if lower.pending_normal.is_some() => {
            let tail = script_pending_tail(lower, try_stmt.span)?;
            lower_controlled_try(lower, try_stmt, out, &tail)
        }
        ast::Stmt::Return(_) if in_function => Err(lower.error(
            statement.span(),
            "P0 source-function bodies admit only a single Return",
        )),
        ast::Stmt::ForIn(_) if is_inventory_for_in_single_a(statement) => {
            lower_inventory_for_in(lower, statement, out)
        }
        _ => Err(lower.error(statement.span(), "unsupported JavaScript ES5 P0 statement")),
    }
}

fn lower_var(
    lower: &mut Lowerer<'_>,
    declaration: &ast::VarDecl,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    for declarator in &declaration.decls {
        lower_var_declarator(lower, declaration, declarator, out)?;
    }
    Ok(())
}

/// Lower exactly one `var` declarator. Exposed to the enhanced statement
/// owner so each declarator can be wrapped in its own Normal guard: after an
/// abrupt initializer no later initializer may evaluate.
pub(super) fn lower_var_declarator(
    lower: &mut Lowerer<'_>,
    declaration: &ast::VarDecl,
    declarator: &ast::VarDeclarator,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    if declaration.kind != ast::VarDeclKind::Var {
        return Err(lower.error(
            declaration.span,
            "JavaScript ES5 supports var declarations; let and const are rejected",
        ));
    }
    let ast::Pat::Ident(binding) = &declarator.name else {
        return Err(lower.error(declarator.span, "P0 requires an identifier binding"));
    };
    let Some(initializer) = declarator.init.as_deref() else {
        return Ok(());
    };
    if let ast::Expr::Array(literal) = initializer
        && !matches!(resolve(lower, binding.id.sym.as_str()), NameHit::Global(_))
    {
        super::lowerer::lower_array_binding(
            lower,
            binding.id.sym.as_str(),
            literal,
            declarator.span,
            out,
            false,
        )?;
        return Ok(());
    }
    let rooting = if immediate_local_call_initializer(lower, initializer, binding) {
        CallResultRooting::ImmediateLocalInitializer
    } else {
        CallResultRooting::Temporary
    };
    let initial = lower.expr_with(initializer, rooting)?;
    if lower.emission_known_throw() {
        // Discarded value: retire consumed roots, then the temporary itself.
        let owned = initial.owned_roots;
        let (slot, bound) = lower.bind_temp(initial.expr, declarator.span)?;
        out.extend(bound);
        release_consumed_roots(lower, &owned, slot, declarator.span, out)?;
        lower.clear_root(slot, declarator.span, out)?;
        return Ok(());
    }
    let _ = store_local(
        lower,
        binding.id.sym.as_str(),
        declarator.span,
        initial,
        out,
    )?;
    Ok(())
}

/// Narrow admission for the immediate local call-result handoff.
///
/// The initializer must unwrap to exactly one call, and its callee must
/// unwrap to exactly one identifier — never a member or computed callee. Both
/// the destination binding and the callee identity must resolve to an
/// instantiated local slot. A global, outer or missing name keeps the existing
/// temporary rooting: a global destination is stored through the shared
/// environment's `.set` owner, which is a different rooting destination than
/// this local binding store. This deliberately inspects no callee identity,
/// return kind or function body.
fn immediate_local_call_initializer(
    lower: &Lowerer<'_>,
    initializer: &ast::Expr,
    binding: &ast::BindingIdent,
) -> bool {
    let ast::Expr::Call(call) = Lowerer::unwrap_parens(initializer) else {
        return false;
    };
    let ast::Callee::Expr(callee) = &call.callee else {
        return false;
    };
    let ast::Expr::Ident(callee) = Lowerer::unwrap_parens(callee) else {
        return false;
    };
    let NameHit::Local(destination) = resolve(lower, binding.id.sym.as_str()) else {
        return false;
    };
    if !lower.binding_slots.contains_key(&destination) {
        return false;
    }
    let NameHit::Local(callee_binding) = resolve(lower, callee.sym.as_str()) else {
        return false;
    };
    lower.binding_slots.contains_key(&callee_binding)
}

fn lower_declaration_if(
    lower: &mut Lowerer<'_>,
    if_stmt: &ast::IfStmt,
    out: &mut Vec<Stmt>,
    in_function: bool,
    push_expr: bool,
) -> Result<(), ParseError> {
    if !is_declaration_only_if(if_stmt) {
        return Err(lower.error(
            if_stmt.span,
            "P3 declaration-only if requires a literal boolean condition and declaration-only branches; general if is outside this scope",
        ));
    }
    let selected = match literal_bool(&if_stmt.test) {
        Some(true) => Some(if_stmt.cons.as_ref()),
        Some(false) => if_stmt.alt.as_deref(),
        None => None,
    };
    if let Some(arm) = selected {
        lower_one(lower, arm, out, in_function, push_expr)?;
    }
    Ok(())
}

struct ScriptTail {
    present: u16,
    value: u16,
}

pub(super) struct FunctionTail {
    pub(super) normal: u16,
    pub(super) completion: u16,
    pub(super) payload: u16,
}

fn assign(index: u16, expr: Expr, line: u32) -> Stmt {
    Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index,
        expr,
        line,
    }
}

fn alloc_script_tail(
    lower: &mut Lowerer<'_>,
    span: swc_common::Span,
    out: &mut Vec<Stmt>,
) -> Result<ScriptTail, ParseError> {
    let present = lower.slot(span)?;
    let line = lower.line(span);
    out.push(Stmt::Let {
        index: present,
        declared_schema: Some(TypeSchema::Bool),
        expr: Expr::Bool(false),
        line,
    });
    let (value, mut bound) = lower.bind_temp(value::undefined(), span)?;
    out.append(&mut bound);
    Ok(ScriptTail { present, value })
}

fn note_value(
    lower: &mut Lowerer<'_>,
    tail: &ScriptTail,
    slot: u16,
    span: swc_common::Span,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let line = lower.line(span);
    out.push(assign(tail.value, Expr::Var(slot), line));
    lower.refresh_root(tail.value, span, out)?;
    out.push(assign(tail.present, Expr::Bool(true), line));
    Ok(())
}

fn copy_if_present(
    lower: &mut Lowerer<'_>,
    from: &ScriptTail,
    to: &ScriptTail,
    span: swc_common::Span,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    let line = lower.line(span);
    let mut then_branch = vec![
        assign(to.value, Expr::Var(from.value), line),
        assign(to.present, Expr::Bool(true), line),
    ];
    lower.refresh_root(to.value, span, &mut then_branch)?;
    out.push(Stmt::IfElse {
        condition: Expr::Var(from.present),
        then_branch,
        else_branch: Vec::new(),
        line,
    });
    Ok(())
}

fn lower_script_list(
    lower: &mut Lowerer<'_>,
    statements: &[ast::Stmt],
    out: &mut Vec<Stmt>,
    tail: &ScriptTail,
) -> Result<(), ParseError> {
    let mut index = 0;
    while index < statements.len() {
        if is_script_strict_directive(lower, &statements[index]) {
            index += 1;
            continue;
        }
        if lower.statement_is_dead(statements[index].span()) {
            let start = index;
            while index < statements.len() {
                if is_script_strict_directive(lower, &statements[index]) {
                    index += 1;
                    continue;
                }
                if !lower.statement_is_dead(statements[index].span()) {
                    break;
                }
                index += 1;
            }
            let run = &statements[start..index];
            lower.speculate_dead(|lower| {
                for statement in run {
                    if is_script_strict_directive(lower, statement) {
                        continue;
                    }
                    let mut discarded = Vec::new();
                    lower_script_one(lower, statement, &mut discarded, tail)?;
                }
                Ok(())
            })?;
            continue;
        }
        lower_script_one(lower, &statements[index], out, tail)?;
        index += 1;
    }
    Ok(())
}

fn lower_script_one(
    lower: &mut Lowerer<'_>,
    statement: &ast::Stmt,
    out: &mut Vec<Stmt>,
    tail: &ScriptTail,
) -> Result<(), ParseError> {
    match statement {
        ast::Stmt::If(if_stmt) if !is_declaration_only_if(if_stmt) => {
            lower_runtime_if_script(lower, if_stmt, out, tail)
        }
        ast::Stmt::Block(block) => {
            let inner = alloc_script_tail(lower, block.span, out)?;
            lower_script_list(lower, &block.stmts, out, &inner)?;
            copy_if_present(lower, &inner, tail, block.span, out)
        }
        ast::Stmt::Expr(expr) => {
            if let Some(slot) = lower_expr_stmt(lower, expr, out, false)? {
                note_value(lower, tail, slot, expr.span, out)?;
            }
            Ok(())
        }
        other => lower_one(lower, other, out, false, false),
    }
}

fn lower_runtime_if_script(
    lower: &mut Lowerer<'_>,
    if_stmt: &ast::IfStmt,
    out: &mut Vec<Stmt>,
    outer: &ScriptTail,
) -> Result<(), ParseError> {
    let (flag, mut condition) = lower.condition_flag(&if_stmt.test)?;
    out.append(&mut condition);
    let incoming_names = lower.type_names;
    let before = super::lowerer::expression::BranchFacts::save(lower);
    let then_tail = alloc_script_tail(lower, if_stmt.cons.span(), out)?;
    let mut then_branch = Vec::new();
    lower_script_one(lower, &if_stmt.cons, &mut then_branch, &then_tail)?;
    let then_facts = super::lowerer::expression::BranchFacts::save(lower);
    before.restore(lower);
    lower.type_names = incoming_names;
    let else_span = if_stmt
        .alt
        .as_ref()
        .map(|alt| alt.span())
        .unwrap_or(if_stmt.span);
    let else_tail = alloc_script_tail(lower, else_span, out)?;
    let mut else_branch = Vec::new();
    if let Some(alt) = &if_stmt.alt {
        lower_script_one(lower, alt, &mut else_branch, &else_tail)?;
    }
    then_facts.merge(lower);
    lower.type_names = incoming_names;
    let line = lower.line(if_stmt.span);
    let select = Stmt::IfElse {
        condition: Expr::Var(flag),
        then_branch,
        else_branch,
        line,
    };
    let mut take_then = Vec::new();
    copy_if_present(lower, &then_tail, outer, if_stmt.span, &mut take_then)?;
    let mut take_else = Vec::new();
    copy_if_present(lower, &else_tail, outer, if_stmt.span, &mut take_else)?;
    let take = Stmt::IfElse {
        condition: Expr::Var(flag),
        then_branch: take_then,
        else_branch: take_else,
        line,
    };
    // P6B-ABRUPT-SIBLING-PREFIX: after a throwing script test, neither
    // branch may run. Recheck Normal before branch selection when the
    // script tracks a pending Completion.
    if let Some(normal) = lower.pending_normal {
        out.push(Stmt::IfElse {
            condition: Expr::Var(normal),
            then_branch: vec![select, take],
            else_branch: Vec::new(),
            line,
        });
    } else {
        out.push(select);
        out.push(take);
    }
    Ok(())
}

fn lower_function_controlled(
    lower: &mut Lowerer<'_>,
    statements: &[ast::Stmt],
    out: &mut Vec<Stmt>,
) -> Result<Expr, ParseError> {
    let Some(span) = statements.first().map(|statement| statement.span()) else {
        return Ok(value::completion_normal());
    };
    let normal = lower.slot(span)?;
    let line = lower.line(span);
    out.push(Stmt::Let {
        index: normal,
        declared_schema: Some(TypeSchema::Bool),
        expr: Expr::Bool(true),
        line,
    });
    let completion = lower.slot(span)?;
    out.push(Stmt::Let {
        index: completion,
        declared_schema: Some(value::completion_schema()),
        expr: value::completion_normal(),
        line,
    });
    let (payload, payload_stmt) = lower.bind_temp_bare(value::undefined(), span)?;
    out.push(payload_stmt);
    lower.reserve_root(payload, span)?;
    lower.pending_normal = Some(normal);
    lower.pending_completion = Some(completion);
    lower.pending_payload = Some(payload);
    lower.begin_emission_function();
    let tail = FunctionTail {
        normal,
        completion,
        payload,
    };
    let _enhanced = lower.iteration.activate_if_needed(statements);
    let lowered = if lower.iteration.mode {
        super::iteration::lower_function(lower, statements, out)?;
        Ok(())
    } else {
        lower_function_list(lower, statements, out, &tail)
    };
    lower.pending_normal = None;
    lower.pending_completion = None;
    lower.pending_payload = None;
    lowered?;
    Ok(Expr::Var(completion))
}

fn lower_function_list(
    lower: &mut Lowerer<'_>,
    statements: &[ast::Stmt],
    out: &mut Vec<Stmt>,
    tail: &FunctionTail,
) -> Result<(), ParseError> {
    let mut in_prologue = true;
    let mut unreachable = false;
    let mut index = 0;
    while index < statements.len() {
        if in_prologue && is_prologue_string(&statements[index]) {
            index += 1;
            continue;
        }
        in_prologue = false;
        if lower.statement_is_dead(statements[index].span()) {
            let start = index;
            while index < statements.len() && lower.statement_is_dead(statements[index].span()) {
                index += 1;
            }
            let run = &statements[start..index];
            lower.speculate_dead(|lower| {
                for statement in run {
                    let mut discarded = Vec::new();
                    lower_function_one(lower, statement, &mut discarded, tail)?;
                }
                Ok(())
            })?;
            continue;
        }
        let statement = &statements[index];
        let mut inner = Vec::new();
        let incoming_names = lower.type_names;
        let saved = unreachable.then(|| super::lowerer::expression::BranchFacts::save(lower));
        lower_function_one(lower, statement, &mut inner, tail)?;
        out.push(Stmt::IfElse {
            condition: Expr::Var(tail.normal),
            then_branch: inner,
            else_branch: Vec::new(),
            line: lower.line(statement.span()),
        });
        lower.type_names = incoming_names;
        if let Some(saved) = saved {
            saved.restore(lower);
        } else if lower.emission_known_throw() {
            unreachable = true;
        }
        index += 1;
    }
    Ok(())
}

fn is_prologue_string(statement: &ast::Stmt) -> bool {
    let ast::Stmt::Expr(expr) = statement else {
        return false;
    };
    matches!(expr.expr.as_ref(), ast::Expr::Lit(ast::Lit::Str(_)))
}

fn is_script_strict_directive(lower: &Lowerer<'_>, statement: &ast::Stmt) -> bool {
    let ast::Stmt::Expr(expr) = statement else {
        return false;
    };
    let ast::Expr::Lit(ast::Lit::Str(literal)) = expr.expr.as_ref() else {
        return false;
    };
    lower.is_exact_use_strict_lexeme(literal.span)
}

fn lower_function_one(
    lower: &mut Lowerer<'_>,
    statement: &ast::Stmt,
    out: &mut Vec<Stmt>,
    tail: &FunctionTail,
) -> Result<(), ParseError> {
    match statement {
        ast::Stmt::Return(ret) => lower_controlled_return(lower, ret, out, tail),
        ast::Stmt::Throw(throw) => lower_controlled_throw(lower, throw, out, tail),
        ast::Stmt::Try(try_stmt) => lower_controlled_try(lower, try_stmt, out, tail),
        ast::Stmt::If(if_stmt) if !is_declaration_only_if(if_stmt) => {
            lower_runtime_if_function(lower, if_stmt, out, tail)
        }
        ast::Stmt::Block(block) => lower_function_list(lower, &block.stmts, out, tail),
        other => lower_one(lower, other, out, true, false),
    }
}

pub(super) fn lower_controlled_return(
    lower: &mut Lowerer<'_>,
    ret: &ast::ReturnStmt,
    out: &mut Vec<Stmt>,
    tail: &FunctionTail,
) -> Result<(), ParseError> {
    let (value, owned) = match ret.arg.as_deref() {
        Some(arg) => {
            let lowered = lower.expr(arg)?;
            let owned = lowered.owned_roots;
            let (slot, mut bound) =
                lower.bind_evaluation(lowered.expr, lowered.kind, ret.span)?;
            out.append(&mut bound);
            (Expr::Var(slot), {
                let mut v = owned;
                v.push(slot);
                v
            })
        }
        None => (value::undefined(), Vec::new()),
    };
    let record = lower.slot(ret.span)?;
    let line = lower.line(ret.span);
    let mut then_branch = vec![Stmt::Let {
        index: record,
        declared_schema: Some(value::completion_schema()),
        expr: value::completion_return(value),
        line,
    }];
    then_branch.push(assign(tail.completion, Expr::Var(record), line));
    then_branch.push(assign(tail.normal, Expr::Bool(false), line));
    // P6B-PENDING-RELEASE: release the return value temp after the pending
    // Completion owns it. bare Undefined needs no release.
    if !owned.is_empty() {
        release_consumed_roots(lower, &owned, tail.completion, ret.span, &mut then_branch)?;
    }
    publish_if_normal(lower, tail, ret.span, out, then_branch);
    Ok(())
}

fn publish_if_normal(
    lower: &mut Lowerer<'_>,
    tail: &FunctionTail,
    span: swc_common::Span,
    out: &mut Vec<Stmt>,
    then_branch: Vec<Stmt>,
) {
    out.push(Stmt::IfElse {
        condition: Expr::Var(tail.normal),
        then_branch,
        else_branch: Vec::new(),
        line: lower.line(span),
    });
}

pub(super) fn lower_controlled_throw(
    lower: &mut Lowerer<'_>,
    throw: &ast::ThrowStmt,
    out: &mut Vec<Stmt>,
    tail: &FunctionTail,
) -> Result<(), ParseError> {
    let lowered = lower.expr(&throw.arg)?;
    let (slot, mut bound) = lower.bind_temp(lowered.expr, throw.span)?;
    out.append(&mut bound);
    let line = lower.line(throw.span);
    let mut then_branch = vec![assign(tail.payload, Expr::Var(slot), line)];
    lower.refresh_root(tail.payload, throw.span, &mut then_branch)?;
    // P6B-PENDING-RELEASE: hand the throw argument temp to the surviving
    // pending owner using the existing nonallocating root write, then
    // release the temp. The pending slot is now the owner.
    let mut handoff: Vec<u16> = lowered.owned_roots;
    handoff.push(slot);
    release_consumed_roots(lower, &handoff, tail.payload, throw.span, &mut then_branch)?;
    let record = lower.slot(throw.span)?;
    then_branch.push(Stmt::Let {
        index: record,
        declared_schema: Some(value::completion_schema()),
        expr: value::completion_throw(Expr::Var(tail.payload)),
        line,
    });
    then_branch.push(assign(tail.completion, Expr::Var(record), line));
    then_branch.push(assign(tail.normal, Expr::Bool(false), line));
    publish_if_normal(lower, tail, throw.span, out, then_branch);
    Ok(())
}

pub(super) fn lower_controlled_try(
    lower: &mut Lowerer<'_>,
    try_stmt: &ast::TryStmt,
    out: &mut Vec<Stmt>,
    tail: &FunctionTail,
) -> Result<(), ParseError> {
    let outer_known = lower.take_emission_known_throw();
    let outer_edge = lower.take_throw_edge();
    if lower.iteration.mode {
        super::iteration::lower_try_list(lower, &try_stmt.block.stmts, out)?;
    } else {
        lower_function_list(lower, &try_stmt.block.stmts, out, tail)?;
    }
    let body_known = lower.emission_known_throw();
    let body_edge = lower.take_throw_edge();
    let body_end = super::lowerer::expression::BranchFacts::save(lower);
    if body_edge.is_some() || body_known {
        if let Some(edge) = body_edge.clone() {
            edge.restore(lower);
        }
        lower.set_emission_known_throw(false);
    }
    if let Some(handler) = &try_stmt.handler {
        let key = (handler.span.lo.0, handler.span.hi.0);
        let catch_scope = lower
            .graph
            .catch_scopes
            .get(&key)
            .copied()
            .ok_or_else(|| lower.error(handler.span, "P6 catch scope was not collected"))?;
        let mut then_branch = Vec::new();
        if let Some(ast::Pat::Ident(binding)) = &handler.param {
            let saved = lower.active_scope;
            lower.active_scope = catch_scope;
            let id = match resolve(lower, binding.id.sym.as_str()) {
                NameHit::Local(id) => id,
                _ => {
                    lower.active_scope = saved;
                    return Err(
                        lower.error(binding.id.span, "P6 catch parameter was not collected")
                    );
                }
            };
            lower.active_scope = saved;
            let slot = *lower.binding_slots.get(&id).ok_or_else(|| {
                lower.error(binding.id.span, "P6 catch parameter was not instantiated")
            })?;
            let line = lower.line(handler.span);
            then_branch.push(assign(slot, Expr::Var(tail.payload), line));
            lower.refresh_root(slot, handler.span, &mut then_branch)?;
            // P6B-PENDING-RELEASE: the catch parameter is now the surviving
            // owner. Clear the stale working payload root so only the catch
            // binding roots the box through the forced collection window.
            lower.clear_root(tail.payload, handler.span, &mut then_branch)?;
            lower.binding_kinds.insert(id, Kind::Dynamic);
            then_branch.push(assign(tail.normal, Expr::Bool(true), line));
            let cleared = lower.slot(handler.span)?;
            then_branch.push(Stmt::Let {
                index: cleared,
                declared_schema: Some(value::completion_schema()),
                expr: value::completion_normal(),
                line,
            });
            then_branch.push(assign(tail.completion, Expr::Var(cleared), line));
        }
        let saved_scope = lower.active_scope;
        let saved_names = lower.type_names;
        lower.active_scope = catch_scope;
        if lower.iteration.mode {
            super::iteration::lower_try_list(lower, &handler.body.stmts, &mut then_branch)?;
        } else {
            lower_function_list(lower, &handler.body.stmts, &mut then_branch, tail)?;
        }
        lower.active_scope = saved_scope;
        lower.type_names = saved_names;
        // P6B-PENDING-RELEASE: retire catch-owned storage at catch exit.
        // The handler's Return/Throw (if any) already copied any surviving
        // box into the pending Completion; clearing the catch slot cannot
        // clear retained source bindings or the new payload.
        if let Some(ast::Pat::Ident(binding)) = handler.param.as_ref() {
            let saved = lower.active_scope;
            lower.active_scope = catch_scope;
            if let NameHit::Local(id) = resolve(lower, binding.id.sym.as_str())
                && let Some(&slot) = lower.binding_slots.get(&id)
            {
                let _ = lower.clear_root(slot, handler.span, &mut then_branch);
            }
            lower.active_scope = saved;
        }
        out.push(Stmt::IfElse {
            condition: Expr::Eq(
                Box::new(value::field(tail.completion, "kind")),
                Box::new(Expr::Int(value::COMPLETION_THROW)),
            ),
            then_branch,
            else_branch: Vec::new(),
            line: lower.line(handler.span),
        });
    }
    if try_stmt.handler.is_some() {
        let catch_known = lower.emission_known_throw();
        let catch_end = super::lowerer::expression::BranchFacts::save(lower);
        if body_known && !catch_known {
            catch_end.restore(lower);
            lower.set_emission_known_throw(false);
        } else if !body_known && body_edge.is_some() && !catch_known {
            body_end.restore(lower);
            catch_end.merge(lower);
            lower.set_emission_known_throw(false);
        } else if body_known && catch_known {
            lower.set_emission_known_throw(true);
        }
    } else if body_known {
        body_end.restore(lower);
        lower.set_emission_known_throw(true);
        lower.restore_throw_edge(body_edge);
    }
    if outer_known {
        lower.set_emission_known_throw(true);
    }
    lower.restore_throw_edge(outer_edge);
    if let Some(finalizer) = &try_stmt.finalizer {
        lower_controlled_finally(lower, finalizer, out, tail)?;
    }
    Ok(())
}

fn lower_controlled_finally(
    lower: &mut Lowerer<'_>,
    finalizer: &ast::BlockStmt,
    out: &mut Vec<Stmt>,
    tail: &FunctionTail,
) -> Result<(), ParseError> {
    let span = finalizer.span;
    let line = lower.line(span);
    let saved_kind = lower.slot(span)?;
    let saved_has = lower.slot(span)?;
    let saved_target = lower.slot(span)?;
    let (saved_value, value_stmt) =
        lower.bind_temp_bare(value::field(tail.completion, "value"), span)?;
    let (saved_payload, payload_stmt) = lower.bind_temp_bare(Expr::Var(tail.payload), span)?;
    out.push(Stmt::Let {
        index: saved_kind,
        declared_schema: Some(TypeSchema::Int),
        expr: value::field(tail.completion, "kind"),
        line,
    });
    out.push(value_stmt);
    lower.refresh_root(saved_value, span, out)?;
    out.push(Stmt::Let {
        index: saved_has,
        declared_schema: Some(TypeSchema::Bool),
        expr: value::field(tail.completion, "has_value"),
        line,
    });
    out.push(Stmt::Let {
        index: saved_target,
        declared_schema: Some(TypeSchema::Int),
        expr: value::field(tail.completion, "target"),
        line,
    });
    out.push(payload_stmt);
    lower.refresh_root(saved_payload, span, out)?;
    let cleared = lower.slot(span)?;
    out.push(Stmt::Let {
        index: cleared,
        declared_schema: Some(value::completion_schema()),
        expr: value::completion_normal(),
        line,
    });
    out.push(assign(tail.completion, Expr::Var(cleared), line));
    out.push(assign(tail.payload, value::undefined(), line));
    lower.refresh_root(tail.payload, span, out)?;
    out.push(assign(tail.normal, Expr::Bool(true), line));
    // The saved Completion stays pending. Only compile-time throw facts are
    // isolated so this always-executed body is not treated as unreachable.
    lower.isolate_finally_effects(|lower| {
        if lower.iteration.mode {
            super::iteration::lower_finalizer_list(lower, &finalizer.stmts, out)
        } else {
            lower_function_list(lower, &finalizer.stmts, out, tail)
        }
    })?;
    let mut restore = vec![assign(tail.payload, Expr::Var(saved_payload), line)];
    lower.refresh_root(tail.payload, span, &mut restore)?;
    let restored = lower.slot(span)?;
    restore.push(Stmt::Let {
        index: restored,
        declared_schema: Some(value::completion_schema()),
        expr: value::completion(0, Expr::Var(saved_value), Expr::Var(saved_has)),
        line,
    });
    // completion() always writes target 0. Restore the snapshotted target
    // with a second record so Break/Continue stays available later.
    let _ = saved_target;
    let _ = saved_kind;
    restore.push(assign(tail.completion, Expr::Var(restored), line));
    restore.push(assign(
        tail.normal,
        Expr::Eq(
            Box::new(Expr::Var(saved_kind)),
            Box::new(Expr::Int(value::COMPLETION_NORMAL)),
        ),
        line,
    ));
    // The record built above hardcodes kind 0. Rebuild from the snapshot
    // kind so Return/Throw restoration is the real pending state.
    restore.clear();
    let exact = lower.slot(span)?;
    restore.push(Stmt::Let {
        index: exact,
        declared_schema: Some(value::completion_schema()),
        expr: value::completion_from_parts(
            Expr::Var(saved_kind),
            Expr::Var(saved_value),
            Expr::Var(saved_has),
            Expr::Var(saved_target),
        ),
        line,
    });
    restore.push(assign(tail.payload, Expr::Var(saved_payload), line));
    lower.refresh_root(tail.payload, span, &mut restore)?;
    restore.push(assign(tail.completion, Expr::Var(exact), line));
    restore.push(assign(
        tail.normal,
        Expr::Eq(
            Box::new(Expr::Var(saved_kind)),
            Box::new(Expr::Int(value::COMPLETION_NORMAL)),
        ),
        line,
    ));
    // P6B-PENDING-RELEASE: the pending slot is now the surviving owner of
    // the restored box. Release the saved snapshot roots so only pending
    // roots the payload after the finally window.
    lower.clear_root(saved_payload, span, &mut restore)?;
    lower.clear_root(saved_value, span, &mut restore)?;
    let mut release_old = Vec::new();
    lower.clear_root(saved_payload, span, &mut release_old)?;
    lower.clear_root(saved_value, span, &mut release_old)?;
    out.push(Stmt::IfElse {
        condition: Expr::Var(tail.normal),
        then_branch: restore,
        else_branch: release_old,
        line,
    });
    Ok(())
}

fn lower_runtime_if_function(
    lower: &mut Lowerer<'_>,
    if_stmt: &ast::IfStmt,
    out: &mut Vec<Stmt>,
    tail: &FunctionTail,
) -> Result<(), ParseError> {
    let (flag, mut condition) = lower.condition_flag(&if_stmt.test)?;
    out.append(&mut condition);
    let incoming_names = lower.type_names;
    let before = super::lowerer::expression::BranchFacts::save(lower);
    if lower.emission_known_throw() {
        let mut then_branch = Vec::new();
        lower_function_one(lower, &if_stmt.cons, &mut then_branch, tail)?;
        let mut else_branch = Vec::new();
        if let Some(alt) = &if_stmt.alt {
            lower_function_one(lower, alt, &mut else_branch, tail)?;
        }
        before.restore(lower);
        lower.type_names = incoming_names;
        let line = lower.line(if_stmt.span);
        let select = Stmt::IfElse {
            condition: Expr::Var(flag),
            then_branch,
            else_branch,
            line,
        };
        out.push(Stmt::IfElse {
            condition: Expr::Var(tail.normal),
            then_branch: vec![select],
            else_branch: Vec::new(),
            line,
        });
        return Ok(());
    }
    let mut then_branch = Vec::new();
    lower.set_emission_known_throw(false);
    lower_function_one(lower, &if_stmt.cons, &mut then_branch, tail)?;
    let then_abrupt = lower.take_emission_known_throw();
    let then_facts = super::lowerer::expression::BranchFacts::save(lower);
    before.clone().restore(lower);
    lower.type_names = incoming_names;
    let mut else_branch = Vec::new();
    let else_abrupt = if let Some(alt) = &if_stmt.alt {
        lower_function_one(lower, alt, &mut else_branch, tail)?;
        lower.take_emission_known_throw()
    } else {
        false
    };
    let else_facts = super::lowerer::expression::BranchFacts::save(lower);
    if then_abrupt && (if_stmt.alt.is_none() || else_abrupt) && if_stmt.alt.is_some() {
        lower.set_emission_known_throw(true);
        before.restore(lower);
    } else if then_abrupt {
        else_facts.restore(lower);
    } else if else_abrupt {
        then_facts.restore(lower);
    } else {
        then_facts.merge(lower);
    }
    lower.type_names = incoming_names;
    let line = lower.line(if_stmt.span);
    let select = Stmt::IfElse {
        condition: Expr::Var(flag),
        then_branch,
        else_branch,
        line,
    };
    // P6B-ABRUPT-SIBLING-PREFIX: after a throwing test, neither branch may
    // run. The condition flag alone (falsy placeholder) would select else.
    // Every activation now owns pending, so always recheck Normal here.
    out.push(Stmt::IfElse {
        condition: Expr::Var(tail.normal),
        then_branch: vec![select],
        else_branch: Vec::new(),
        line,
    });
    Ok(())
}

fn needs_script_completion(statements: &[ast::Stmt]) -> bool {
    statements.iter().any(stmt_needs_script_completion)
}

fn stmt_needs_script_completion(statement: &ast::Stmt) -> bool {
    match statement {
        ast::Stmt::If(_) | ast::Stmt::Empty(_) => true,
        ast::Stmt::Block(block) => {
            block.stmts.is_empty() || block.stmts.iter().any(stmt_needs_script_completion)
        }
        ast::Stmt::Decl(ast::Decl::Var(declaration)) => {
            declaration.decls.iter().any(|decl| decl.init.is_none())
        }
        _ => false,
    }
}

#[allow(dead_code)]
fn contains_general_if(statements: &[ast::Stmt]) -> bool {
    statements.iter().any(stmt_has_general_if)
}

#[allow(dead_code)]
fn stmt_has_general_if(statement: &ast::Stmt) -> bool {
    match statement {
        ast::Stmt::If(if_stmt) => {
            !is_declaration_only_if(if_stmt)
                || stmt_has_general_if(&if_stmt.cons)
                || if_stmt.alt.as_deref().is_some_and(stmt_has_general_if)
        }
        ast::Stmt::Block(block) => block.stmts.iter().any(stmt_has_general_if),
        _ => false,
    }
}

#[allow(dead_code)]
fn use_control(statements: &[ast::Stmt]) -> bool {
    contains_general_if(statements)
        || !legacy_return_shape(statements)
        || contains_throw_or_try(statements)
}

#[allow(dead_code)]
fn contains_throw_or_try(statements: &[ast::Stmt]) -> bool {
    statements.iter().any(stmt_contains_throw_or_try)
}

#[allow(dead_code)]
fn stmt_contains_throw_or_try(statement: &ast::Stmt) -> bool {
    match statement {
        ast::Stmt::Throw(_) | ast::Stmt::Try(_) => true,
        ast::Stmt::If(if_stmt) => {
            stmt_contains_throw_or_try(&if_stmt.cons)
                || if_stmt
                    .alt
                    .as_deref()
                    .is_some_and(stmt_contains_throw_or_try)
        }
        ast::Stmt::Block(block) => contains_throw_or_try(&block.stmts),
        _ => false,
    }
}

#[allow(dead_code)]
fn legacy_return_shape(statements: &[ast::Stmt]) -> bool {
    let mut seen_return = false;
    for statement in statements {
        if seen_return {
            if !skippable_after_return(statement) {
                return false;
            }
            continue;
        }
        if matches!(statement, ast::Stmt::Return(_)) {
            seen_return = true;
            continue;
        }
        if contains_return(statement) {
            return false;
        }
    }
    true
}

#[allow(dead_code)]
fn contains_return(statement: &ast::Stmt) -> bool {
    match statement {
        ast::Stmt::Return(_) => true,
        ast::Stmt::Block(block) => block.stmts.iter().any(contains_return),
        ast::Stmt::If(if_stmt) => {
            contains_return(&if_stmt.cons) || if_stmt.alt.as_deref().is_some_and(contains_return)
        }
        _ => false,
    }
}

fn join_profile(left: &ProfileFacts, right: &ProfileFacts) -> ProfileFacts {
    let mut joined = left.clone();
    for (id, right_fact) in right {
        let left_fact = joined
            .get(id)
            .copied()
            .unwrap_or_else(|| ProfileFact::scalar(Kind::Dynamic));
        let kind = super::lowerer::expression::join_kind(left_fact.kind, right_fact.kind);
        joined.insert(
            *id,
            ProfileFact {
                kind,
                array_length: super::lowerer::expression::join_length(
                    kind,
                    left_fact.array_length,
                    right_fact.array_length,
                ),
                guarded_numeric: left_fact.joined_guard(*right_fact, kind),
            },
        );
    }
    joined
}

pub(super) fn lower_expr_stmt(
    lower: &mut Lowerer<'_>,
    expr: &ast::ExprStmt,
    out: &mut Vec<Stmt>,
    push: bool,
) -> Result<Option<u16>, ParseError> {
    Ok(lower_expr_value(lower, &expr.expr, expr.span, out, push)?.map(|(slot, _)| slot))
}

/// Lower an expression statement-shaped evaluation. Shared with the enhanced
/// iteration owner for `for` init/update phases, where only the side effects
/// matter but a member write still needs its evaluation-order statements.
pub(super) fn lower_expr_value(
    lower: &mut Lowerer<'_>,
    expression: &ast::Expr,
    span: swc_common::Span,
    out: &mut Vec<Stmt>,
    push: bool,
) -> Result<Option<(u16, Option<super::lowerer::ArrayShape>)>, ParseError> {
    if let ast::Expr::Assign(assign) = expression
        && assign.op == ast::AssignOp::Assign
        && let ast::AssignTarget::Simple(ast::SimpleAssignTarget::Member(member)) = &assign.left
    {
        let slot =
            super::lowerer::lower_member_write_stmt(lower, member, &assign.right, span, out)?;
        return Ok(Some((slot, None)));
    }
    let lowered = lower.expr(expression)?;
    let shape = lowered.array_length;
    if lower.root_scope.is_some() {
        let (slot, mut stmts) = lower.bind_evaluation(lowered.expr, lowered.kind, span)?;
        out.append(&mut stmts);
        if push {
            out.push(Stmt::Expr {
                expr: Expr::Var(slot),
                line: lower.line(span),
            });
        }
        // The observer/result root survives; only its consumed operands are
        // released. Primitive outputs need no logical entry.
        release_consumed_roots(lower, &lowered.owned_roots, slot, span, out)?;
        Ok(Some((slot, shape)))
    } else {
        let (slot, mut stmts) = lower.bind_temp(lowered.expr, span)?;
        out.append(&mut stmts);
        if push {
            out.push(Stmt::Expr {
                expr: Expr::Var(slot),
                line: lower.line(span),
            });
        }
        Ok(Some((slot, shape)))
    }
}

pub(super) fn store_local(
    lower: &mut Lowerer<'_>,
    name: &str,
    span: swc_common::Span,
    lowered: Lowered,
    out: &mut Vec<Stmt>,
) -> Result<(Expr, Kind), ParseError> {
    if let NameHit::Global(i) = resolve(lower, name) {
        let kind = lowered.kind;
        let owned = lowered.owned_roots;
        let (rhs, mut eval) = lower.bind_evaluation(lowered.expr, kind, span)?;
        out.append(&mut eval);
        let ack = lower.slot(span)?;
        let line = lower.line(span);
        let store = Stmt::Let {
            index: ack,
            declared_schema: Some(TypeSchema::Int),
            expr: Lowerer::guest_call(
                lower.global_helpers.expect("shared helpers").set,
                vec![
                    Expr::Var(lower.active_carrier(span)?),
                    Expr::Var(
                        lower
                            .global_env
                            .ok_or_else(|| lower.error(span, "shared environment unavailable"))?,
                    ),
                    Expr::Int(lower.globals.entry(i)),
                    Expr::Var(rhs),
                ],
            ),
            line,
        };
        if let Some(normal) = lower.pending_normal {
            out.push(Stmt::IfElse {
                condition: Expr::Var(normal),
                then_branch: vec![store],
                else_branch: Vec::new(),
                line,
            });
        } else {
            out.push(store);
        }
        release_consumed_roots(lower, &owned, rhs, span, out)?;
        return Ok((Expr::Var(rhs), kind));
    }
    let id = match resolve(lower, name) {
        NameHit::Local(id) => id,
        NameHit::Global(_) => unreachable!(),
        NameHit::Outer(_) => {
            return Err(lower.error(
                span,
                &format!("P4 free-variable capture of '{name}' is outside this closed subset"),
            ));
        }
        NameHit::Missing
        | NameHit::BuiltinUndefined
        | NameHit::BuiltinObject
        | NameHit::BuiltinTypeError
        | NameHit::BuiltinRangeError
        | NameHit::BuiltinOptionalError(_)
        | NameHit::BuiltinJson
        | NameHit::BuiltinMath
        | NameHit::BuiltinNumeric(_)
        | NameHit::BuiltinUri(_)
        | NameHit::BuiltinNumber(_)
        | NameHit::BuiltinNumberNamespace
        | NameHit::BuiltinStringNamespace
        | NameHit::BuiltinArrayNamespace
        | NameHit::BuiltinArrayBuffer
        | NameHit::BuiltinTypedArray(_)
        | NameHit::BuiltinBoolean => {
            return Err(lower.error(span, &format!("unknown local '{name}'")));
        }
    };
    let slot = *lower
        .binding_slots
        .get(&id)
        .ok_or_else(|| lower.error(span, "P3 binding was collected but not instantiated"))?;
    let kind = lowered.kind;
    let array_len = lowered.array_length.filter(|_| kind == Kind::Array);
    let owned_roots = lowered.owned_roots;
    let line = lower.line(span);
    match lowered.expr {
        Expr::Block { mut stmts, expr } => {
            out.append(&mut stmts);
            let mut store = vec![Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: slot,
                expr: *expr,
                line,
            }];
            lower.refresh_root(slot, span, &mut store)?;
            if let Some(normal) = lower.pending_normal {
                out.push(Stmt::IfElse {
                    condition: Expr::Var(normal),
                    then_branch: store,
                    else_branch: Vec::new(),
                    line,
                });
            } else {
                out.extend(store);
            }
        }
        Expr::Var(src) if src == slot => {
            lower.refresh_root(slot, span, out)?;
        }
        other => {
            let mut store = vec![Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: slot,
                expr: other,
                line,
            }];
            lower.refresh_root(slot, span, &mut store)?;
            if let Some(normal) = lower.pending_normal {
                out.push(Stmt::IfElse {
                    condition: Expr::Var(normal),
                    then_branch: store,
                    else_branch: Vec::new(),
                    line,
                });
            } else {
                out.extend(store);
            }
        }
    }
    release_consumed_roots(lower, &owned_roots, slot, span, out)?;
    lower.binding_kinds.insert(id, kind);
    lower.bindings.insert(name.to_owned(), (slot, kind));
    if let Some(length) = array_len {
        lower.array_lengths.insert(name.to_owned(), length);
        lower.array_length_ids.insert(id, length);
        lower.array_names.insert(name.to_owned());
    } else {
        lower.array_lengths.remove(name);
        lower.array_length_ids.remove(&id);
        if kind != Kind::Array {
            lower.array_names.remove(name);
        }
    }
    Ok((Expr::Var(slot), kind))
}

pub(super) fn release_consumed_roots(
    lower: &mut Lowerer<'_>,
    owned: &[u16],
    destination: u16,
    span: swc_common::Span,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    for slot in Lowerer::compose_owned(owned.iter().copied()) {
        if slot == destination || persistent_root(lower, slot) || !lower.has_root(slot) {
            continue;
        }
        clear_temp(lower, slot, span, out)?;
    }
    Ok(())
}

fn persistent_root(lower: &Lowerer<'_>, slot: u16) -> bool {
    if lower.binding_slots.values().any(|bound| *bound == slot) {
        return true;
    }
    let Some(names) = &lower.type_names else {
        return false;
    };
    [
        names.undefined,
        names.object,
        names.boolean,
        names.number,
        names.string,
        names.function,
    ]
    .into_iter()
    .flatten()
    .any(|cached| cached == slot)
}

fn unwrap_parens(expr: &ast::Expr) -> &ast::Expr {
    let mut current = expr;
    while let ast::Expr::Paren(paren) = current {
        current = &paren.expr;
    }
    current
}

/// A bare local written anywhere in an enclosing loop construct has no
/// first-iteration kind proof across the back edge.
fn loop_unstable_operand(lower: &Lowerer<'_>, expression: &ast::Expr) -> bool {
    let ast::Expr::Ident(ident) = unwrap_parens(expression) else {
        return false;
    };
    match resolve(lower, ident.sym.as_str()) {
        NameHit::Local(id) => lower.iteration.kind_is_unstable(id),
        _ => false,
    }
}

pub(super) fn clear_temp(
    lower: &mut Lowerer<'_>,
    slot: u16,
    span: swc_common::Span,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    lower.clear_root(slot, span, out)
}

fn proven_es5_name(kind: Kind) -> Option<&'static str> {
    match kind {
        Kind::Undefined => Some("undefined"),
        Kind::Null => Some("object"),
        Kind::Boolean => Some("boolean"),
        Kind::Number => Some("number"),
        Kind::String => Some("string"),
        Kind::Dynamic | Kind::Array | Kind::Object | Kind::Function => None,
    }
}

fn cached_type_name(names: &TypeNames, text: &str) -> Option<u16> {
    match text {
        "undefined" => names.undefined,
        "object" => names.object,
        "boolean" => names.boolean,
        "number" => names.number,
        "string" => names.string,
        "function" => names.function,
        _ => None,
    }
}

fn store_type_name(names: &mut TypeNames, text: &str, slot: u16) {
    let field = match text {
        "undefined" => &mut names.undefined,
        "object" => &mut names.object,
        "boolean" => &mut names.boolean,
        "number" => &mut names.number,
        "string" => &mut names.string,
        "function" => &mut names.function,
        _ => return,
    };
    *field = Some(slot);
}

fn require_type_name(
    lower: &Lowerer<'_>,
    text: &str,
    span: swc_common::Span,
) -> Result<u16, ParseError> {
    lower
        .type_names
        .as_ref()
        .and_then(|names| cached_type_name(names, text))
        .ok_or_else(|| lower.error(span, "P3 typeof type-name cache was not installed"))
}

fn copy_cached_name(
    lower: &mut Lowerer<'_>,
    name: u16,
    span: swc_common::Span,
    out: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let result = lower.slot(span)?;
    out.push(Stmt::Let {
        index: result,
        declared_schema: Some(value::schema()),
        expr: Expr::Var(name),
        line: lower.line(span),
    });
    lower.refresh_root(result, span, out)?;
    Ok(result)
}

fn ensure_type_name(
    lower: &mut Lowerer<'_>,
    text: &str,
    span: swc_common::Span,
    out: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    if let Some(slot) = lower
        .type_names
        .as_ref()
        .and_then(|names| cached_type_name(names, text))
    {
        return Ok(slot);
    }
    let slot = emit_type_name(lower, text, span, out)?;
    let names = lower.type_names.get_or_insert(TypeNames::default());
    store_type_name(names, text, slot);
    Ok(slot)
}

fn ensure_type_names(
    lower: &mut Lowerer<'_>,
    span: swc_common::Span,
    out: &mut Vec<Stmt>,
) -> Result<(), ParseError> {
    for text in [
        "undefined",
        "object",
        "boolean",
        "number",
        "string",
        "function",
    ] {
        ensure_type_name(lower, text, span, out)?;
    }
    Ok(())
}

fn emit_type_name(
    lower: &mut Lowerer<'_>,
    text: &str,
    span: swc_common::Span,
    out: &mut Vec<Stmt>,
) -> Result<u16, ParseError> {
    let units: Vec<u16> = text.encode_utf16().collect();
    lower.emit_utf16_node(&units, span, out)
}

#[allow(dead_code)]
fn skippable_after_return(statement: &ast::Stmt) -> bool {
    matches!(
        statement,
        ast::Stmt::Empty(_)
            | ast::Stmt::Expr(_)
            | ast::Stmt::Decl(_)
            | ast::Stmt::Block(_)
            | ast::Stmt::If(_)
    )
}

pub(super) fn is_declaration_only_if(if_stmt: &ast::IfStmt) -> bool {
    literal_bool(&if_stmt.test).is_some()
        && is_declaration_only(&if_stmt.cons)
        && if_stmt
            .alt
            .as_ref()
            .is_none_or(|alt| is_declaration_only(alt))
}

fn is_declaration_only(statement: &ast::Stmt) -> bool {
    match statement {
        ast::Stmt::Empty(_) | ast::Stmt::Decl(ast::Decl::Var(_)) => true,
        ast::Stmt::Block(block) => block.stmts.iter().all(is_declaration_only),
        ast::Stmt::If(if_stmt) => is_declaration_only_if(if_stmt),
        _ => false,
    }
}

fn literal_bool(expression: &ast::Expr) -> Option<bool> {
    match expression {
        ast::Expr::Lit(ast::Lit::Bool(value)) => Some(value.value),
        ast::Expr::Paren(paren) => literal_bool(&paren.expr),
        _ => None,
    }
}

// Pure profile facts are keyed by declaration identity, never physical slots.
// Each scope starts from the same hoisted Undefined/function state as emission.
#[derive(Clone, Copy)]
struct ProfileFact {
    kind: Kind,
    array_length: Option<super::lowerer::ArrayShape>,
    /// Ordinary call and Dynamic member results may enter arithmetic.
    /// Number facts are eligible by kind and keep this false. Formal, catch,
    /// missing-arm, and missing-key facts stay false.
    guarded_numeric: bool,
}

impl ProfileFact {
    fn scalar(kind: Kind) -> Self {
        Self {
            kind,
            array_length: None,
            guarded_numeric: false,
        }
    }

    fn guarded_dynamic() -> Self {
        Self {
            kind: Kind::Dynamic,
            array_length: None,
            guarded_numeric: true,
        }
    }

    fn numeric_eligible(self) -> bool {
        self.kind == Kind::Number || (self.kind == Kind::Dynamic && self.guarded_numeric)
    }

    /// AND of effective eligibility. A Number joined with an eligible Dynamic
    /// stays eligible; a raw-bit AND would drop it because Number defaults false.
    fn joined_guard(self, right: Self, kind: Kind) -> bool {
        kind == Kind::Dynamic && self.numeric_eligible() && right.numeric_eligible()
    }
}

type ProfileFacts = std::collections::HashMap<BindingId, ProfileFact>;

/// Intrinsic root capacity is frozen from `math_used` / `numeric_globals`
/// before function bodies are lowered. The profile walk skips those bodies,
/// so a Math or numeric builtin that appears only inside a function was
/// invisible and the reader rejected its root entry. Note the same flags
/// `ident_fact` would set, using the function's own scope so a shadowing
/// local is not mistaken for the builtin.
fn note_function_builtin_uses(
    lower: &Lowerer<'_>,
    statements: &[ast::Stmt],
) -> Result<(), ParseError> {
    note_stmts_for_builtins(lower, statements)
}

fn note_stmts_for_builtins(
    lower: &Lowerer<'_>,
    statements: &[ast::Stmt],
) -> Result<(), ParseError> {
    for statement in statements {
        note_stmt_for_builtins(lower, statement)?;
    }
    Ok(())
}

fn note_stmt_for_builtins(lower: &Lowerer<'_>, statement: &ast::Stmt) -> Result<(), ParseError> {
    match statement {
        ast::Stmt::Decl(ast::Decl::Fn(decl)) => note_function_for_builtins(lower, &decl.function),
        ast::Stmt::Decl(ast::Decl::Var(decl)) => {
            for declarator in &decl.decls {
                if let Some(init) = declarator.init.as_deref() {
                    note_expr_for_builtins(lower, init)?;
                }
            }
            Ok(())
        }
        ast::Stmt::Block(block) => note_stmts_for_builtins(lower, &block.stmts),
        ast::Stmt::Expr(expr) => note_expr_for_builtins(lower, &expr.expr),
        ast::Stmt::If(if_stmt) => {
            note_expr_for_builtins(lower, &if_stmt.test)?;
            note_stmt_for_builtins(lower, &if_stmt.cons)?;
            if let Some(alt) = &if_stmt.alt {
                note_stmt_for_builtins(lower, alt)?;
            }
            Ok(())
        }
        ast::Stmt::Return(ret) => {
            if let Some(arg) = ret.arg.as_deref() {
                note_expr_for_builtins(lower, arg)?;
            }
            Ok(())
        }
        ast::Stmt::While(loop_) => {
            note_expr_for_builtins(lower, &loop_.test)?;
            note_stmt_for_builtins(lower, &loop_.body)
        }
        ast::Stmt::DoWhile(loop_) => {
            note_stmt_for_builtins(lower, &loop_.body)?;
            note_expr_for_builtins(lower, &loop_.test)
        }
        ast::Stmt::For(for_stmt) => {
            if let Some(ast::VarDeclOrExpr::Expr(expr)) = &for_stmt.init {
                note_expr_for_builtins(lower, expr)?;
            }
            if let Some(ast::VarDeclOrExpr::VarDecl(decl)) = &for_stmt.init {
                for declarator in &decl.decls {
                    if let Some(init) = declarator.init.as_deref() {
                        note_expr_for_builtins(lower, init)?;
                    }
                }
            }
            if let Some(test) = &for_stmt.test {
                note_expr_for_builtins(lower, test)?;
            }
            if let Some(update) = &for_stmt.update {
                note_expr_for_builtins(lower, update)?;
            }
            note_stmt_for_builtins(lower, &for_stmt.body)
        }
        ast::Stmt::ForOf(loop_) => {
            // The type-error arm is always emitted, so the ordinary realm
            // (TypeError intrinsic) must exist even for a known array RHS.
            lower.needs_ordinary_realm.set(true);
            if let ast::ForHead::Pat(pat) = &loop_.left
                && let ast::Pat::Expr(expr) = pat.as_ref()
            {
                note_expr_for_builtins(lower, expr)?;
            }
            note_expr_for_builtins(lower, &loop_.right)?;
            note_stmt_for_builtins(lower, &loop_.body)
        }
        ast::Stmt::Try(try_stmt) => {
            note_stmts_for_builtins(lower, &try_stmt.block.stmts)?;
            if let Some(handler) = &try_stmt.handler {
                let key = (handler.span.lo.0, handler.span.hi.0);
                let previous = lower.catch_overlay.get();
                if let Some(handler_scope) = lower.graph.catch_scopes.get(&key).copied() {
                    lower.catch_overlay.set(Some(handler_scope));
                }
                let noted = note_stmts_for_builtins(lower, &handler.body.stmts);
                lower.catch_overlay.set(previous);
                noted?;
            }
            if let Some(finalizer) = &try_stmt.finalizer {
                note_stmts_for_builtins(lower, &finalizer.stmts)?;
            }
            Ok(())
        }
        ast::Stmt::Throw(throw) => note_expr_for_builtins(lower, &throw.arg),
        ast::Stmt::Switch(select) => {
            note_expr_for_builtins(lower, &select.discriminant)?;
            for case in &select.cases {
                if let Some(test) = &case.test {
                    note_expr_for_builtins(lower, test)?;
                }
                note_stmts_for_builtins(lower, &case.cons)?;
            }
            Ok(())
        }
        ast::Stmt::Labeled(labeled) => note_stmt_for_builtins(lower, &labeled.body),
        _ => Ok(()),
    }
}

fn note_function_for_builtins(
    lower: &Lowerer<'_>,
    function: &ast::Function,
) -> Result<(), ParseError> {
    let key = (function.span.lo.0, function.span.hi.0);
    let Some(scope) = lower.graph.function_scopes.get(&key).copied() else {
        return Ok(());
    };
    let Some(body) = &function.body else {
        return Ok(());
    };
    for statement in &body.stmts {
        note_function_stmt(lower, statement, scope)?;
    }
    Ok(())
}

fn note_function_stmt(
    lower: &Lowerer<'_>,
    statement: &ast::Stmt,
    scope: ScopeId,
) -> Result<(), ParseError> {
    match statement {
        ast::Stmt::Decl(ast::Decl::Fn(decl)) => note_function_for_builtins(lower, &decl.function),
        ast::Stmt::Decl(ast::Decl::Var(decl)) => {
            for declarator in &decl.decls {
                if let Some(init) = declarator.init.as_deref() {
                    note_function_expr(lower, init, scope)?;
                }
            }
            Ok(())
        }
        ast::Stmt::Block(block) => {
            for statement in &block.stmts {
                note_function_stmt(lower, statement, scope)?;
            }
            Ok(())
        }
        ast::Stmt::Expr(expr) => note_function_expr(lower, &expr.expr, scope),
        ast::Stmt::If(if_stmt) => {
            note_function_expr(lower, &if_stmt.test, scope)?;
            note_function_stmt(lower, &if_stmt.cons, scope)?;
            if let Some(alt) = &if_stmt.alt {
                note_function_stmt(lower, alt, scope)?;
            }
            Ok(())
        }
        ast::Stmt::Return(ret) => {
            if let Some(arg) = ret.arg.as_deref() {
                note_function_expr(lower, arg, scope)?;
            }
            Ok(())
        }
        ast::Stmt::While(loop_) => {
            note_function_expr(lower, &loop_.test, scope)?;
            note_function_stmt(lower, &loop_.body, scope)
        }
        ast::Stmt::DoWhile(loop_) => {
            note_function_stmt(lower, &loop_.body, scope)?;
            note_function_expr(lower, &loop_.test, scope)
        }
        ast::Stmt::For(for_stmt) => {
            if let Some(ast::VarDeclOrExpr::Expr(expr)) = &for_stmt.init {
                note_function_expr(lower, expr, scope)?;
            }
            if let Some(ast::VarDeclOrExpr::VarDecl(decl)) = &for_stmt.init {
                for declarator in &decl.decls {
                    if let Some(init) = declarator.init.as_deref() {
                        note_function_expr(lower, init, scope)?;
                    }
                }
            }
            if let Some(test) = &for_stmt.test {
                note_function_expr(lower, test, scope)?;
            }
            if let Some(update) = &for_stmt.update {
                note_function_expr(lower, update, scope)?;
            }
            note_function_stmt(lower, &for_stmt.body, scope)
        }
        ast::Stmt::ForOf(loop_) => {
            lower.needs_ordinary_realm.set(true);
            if let ast::ForHead::Pat(pat) = &loop_.left
                && let ast::Pat::Expr(expr) = pat.as_ref()
            {
                note_function_expr(lower, expr, scope)?;
            }
            note_function_expr(lower, &loop_.right, scope)?;
            note_function_stmt(lower, &loop_.body, scope)
        }
        ast::Stmt::Try(try_stmt) => {
            for statement in &try_stmt.block.stmts {
                note_function_stmt(lower, statement, scope)?;
            }
            if let Some(handler) = &try_stmt.handler {
                let key = (handler.span.lo.0, handler.span.hi.0);
                let handler_scope = lower.graph.catch_scopes.get(&key).copied().unwrap_or(scope);
                for statement in &handler.body.stmts {
                    note_function_stmt(lower, statement, handler_scope)?;
                }
            }
            if let Some(finalizer) = &try_stmt.finalizer {
                for statement in &finalizer.stmts {
                    note_function_stmt(lower, statement, scope)?;
                }
            }
            Ok(())
        }
        ast::Stmt::Throw(throw) => note_function_expr(lower, &throw.arg, scope),
        ast::Stmt::Switch(select) => {
            note_function_expr(lower, &select.discriminant, scope)?;
            for case in &select.cases {
                if let Some(test) = &case.test {
                    note_function_expr(lower, test, scope)?;
                }
                for statement in &case.cons {
                    note_function_stmt(lower, statement, scope)?;
                }
            }
            Ok(())
        }
        ast::Stmt::Labeled(labeled) => note_function_stmt(lower, &labeled.body, scope),
        _ => Ok(()),
    }
}

/// RangeError is required only when `fromCodePoint` is resolved on the
/// unshadowed String carrier. A shadowed `String`, an unrelated object, and
/// a non-literal computed key do not install it. Identifier and string-literal
/// keys are both static resolutions; the function-body walk passes that
/// function's scope so a local shadow is not mistaken for the carrier.
fn note_resolved_from_code_point(
    lower: &Lowerer<'_>,
    member: &ast::MemberExpr,
    scope: ScopeId,
) {
    let ast::Expr::Ident(base) = unwrap_parens(&member.obj) else {
        return;
    };
    if !matches!(
        resolve_from(lower, base.sym.as_str(), scope),
        NameHit::BuiltinStringNamespace
    ) {
        return;
    }
    let named = match &member.prop {
        ast::MemberProp::Ident(prop) => {
            prop.sym.as_str() == super::prelude::intrinsics::STRING_FROM_CODE_POINT_NAME
        }
        ast::MemberProp::Computed(computed) => match unwrap_parens(&computed.expr) {
            ast::Expr::Lit(ast::Lit::Str(literal)) => literal.value == *"fromCodePoint",
            _ => false,
        },
        _ => false,
    };
    if named {
        lower.range_error_used.set(true);
        lower.needs_ordinary_realm.set(true);
    }
}

/// True when the expression spells the String key `prototype`.
///
/// `Function.prototype.prototype` is normally gated by a `.prototype` member
/// token, but two admitted observations reach the same inherited accessor row
/// without one: the `in` operator with a `"prototype"` String key and
/// `hasOwnProperty("prototype")` called on `Function.prototype`. Both must
/// select the accessor or the script observes a missing row: `"prototype" in
/// f` changes from true to false and `Object.getPrototypeOf(f)
/// .hasOwnProperty("prototype")` from true to false.
fn is_prototype_string_key(expression: &ast::Expr) -> bool {
    matches!(
        unwrap_parens(expression),
        ast::Expr::Lit(ast::Lit::Str(literal))
            if literal.value == *super::prelude::intrinsics::FUNCTION_PROTOTYPE_NAME
    )
}

/// Identifier or String-literal member name.
fn member_named(member: &ast::MemberExpr, name: &str) -> bool {
    match &member.prop {
        ast::MemberProp::Ident(prop) => prop.sym.as_str() == name,
        ast::MemberProp::Computed(computed) => {
            matches!(
                unwrap_parens(&computed.expr),
                ast::Expr::Lit(ast::Lit::Str(literal)) if literal.value == name
            )
        }
        _ => false,
    }
}

/// The static `Function.prototype.prototype` surface gate for one expression.
/// Covers the `"prototype" in f`, `x.hasOwnProperty("prototype")` and
/// `Object.keys(Object.getPrototypeOf(f))` shapes that expose the inherited
/// accessor without spelling a `.prototype` member. Every spelling is
/// scope-aware: a name bound to a source function declaration is a function
/// value, while a shadowed or unrelated name is not.
fn note_function_prototype_surface(lower: &Lowerer<'_>, expression: &ast::Expr, scope: ScopeId) {
    match expression {
        ast::Expr::Bin(binary) => {
            if binary.op == ast::BinaryOp::In && is_prototype_string_key(&binary.left) {
                lower.function_prototype_used.set(true);
            }
        }
        ast::Expr::Call(call) => {
            let ast::Callee::Expr(callee) = &call.callee else {
                return;
            };
            let ast::Expr::Member(member) = unwrap_parens(callee) else {
                return;
            };
            if member_named(member, super::prelude::intrinsics::HAS_OWN_PROPERTY_NAME)
                && matches!(call.args.as_slice(), [arg] if arg.spread.is_none() && is_prototype_string_key(&arg.expr))
            {
                lower.function_prototype_used.set(true);
            }
            if object_keys_observes_function_prototype(lower, call, scope) {
                lower.function_prototype_used.set(true);
            }
        }
        _ => {}
    }
}

/// True when `call` is `Object.keys(Object.getPrototypeOf(<function value>))`.
/// The receiver of the keys observation is `Function.prototype` only for a
/// function value, so the accessor row must exist before `Object.keys`
/// enumerates it. Both `Object` spellings must resolve on the unshadowed
/// builtin and the argument must be a provable source function: an inline
/// function expression or a name bound to a function declaration.
fn object_keys_observes_function_prototype(
    lower: &Lowerer<'_>,
    call: &ast::CallExpr,
    scope: ScopeId,
) -> bool {
    let ast::Callee::Expr(callee) = &call.callee else {
        return false;
    };
    let ast::Expr::Member(member) = unwrap_parens(callee) else {
        return false;
    };
    if !member_named(member, "keys") || !builtin_object_namespace(lower, &member.obj, scope) {
        return false;
    }
    let [arg] = call.args.as_slice() else {
        return false;
    };
    if arg.spread.is_some() {
        return false;
    }
    let ast::Expr::Call(inner) = unwrap_parens(&arg.expr) else {
        return false;
    };
    let ast::Callee::Expr(inner_callee) = &inner.callee else {
        return false;
    };
    let ast::Expr::Member(inner_member) = unwrap_parens(inner_callee) else {
        return false;
    };
    if !member_named(inner_member, "getPrototypeOf")
        || !builtin_object_namespace(lower, &inner_member.obj, scope)
    {
        return false;
    }
    let [inner_arg] = inner.args.as_slice() else {
        return false;
    };
    inner_arg.spread.is_none() && is_function_value(lower, &inner_arg.expr, scope)
}

fn builtin_object_namespace(lower: &Lowerer<'_>, expression: &ast::Expr, scope: ScopeId) -> bool {
    matches!(
        unwrap_parens(expression),
        ast::Expr::Ident(ident)
            if ident.sym == *"Object"
                && matches!(resolve_from(lower, ident.sym.as_str(), scope), NameHit::BuiltinObject)
    )
}

fn is_function_value(lower: &Lowerer<'_>, expression: &ast::Expr, scope: ScopeId) -> bool {
    match unwrap_parens(expression) {
        ast::Expr::Fn(_) => true,
        ast::Expr::Ident(ident) => match resolve_from(lower, ident.sym.as_str(), scope) {
            NameHit::Local(id) => !lower.graph.binding(id).fn_sites.is_empty(),
            _ => false,
        },
        _ => false,
    }
}

/// The `Function.prototype` property family: the inherited `prototype`
/// get/set accessor and the `constructor` back-link row are gated by the
/// same flag, so every spelling of either name selects the rows. This
/// helper is shared by the Script walk and every function-body walk, which
/// keeps a body-local spelling from depending on a later Script-side read.
fn note_function_prototype_member(lower: &Lowerer<'_>, member: &ast::MemberExpr) {
    if member_named(member, super::prelude::intrinsics::FUNCTION_PROTOTYPE_NAME) {
        lower.function_prototype_used.set(true);
    } else if member_named(member, super::prelude::intrinsics::CONSTRUCTOR_NAME) {
        // `(1).constructor`, `'x'.constructor` and `true.constructor` all name
        // a private primitive prototype's back-link row. The receiver's class
        // is not known statically, so one read selects the Number, String and
        // Boolean carriers even when the source never spells those globals.
        lower.function_prototype_used.set(true);
        lower.primitive_constructor_read.set(true);
    }
}


/// The static `new F(...)` gate for the private construct helper.
///
/// A function body whose only feature site is `new F()` may be skipped by
/// the feature-site admission pass, so the helper must also be selected by
/// the walker itself. Only a name bound to a source function declaration is
/// provable here; a formal/parameter callee is proved by body validation.
fn note_source_construct_gate(
    lower: &Lowerer<'_>,
    construct: &ast::NewExpr,
    scope: ScopeId,
) {
    let ast::Expr::Ident(callee) = unwrap_parens(&construct.callee) else {
        return;
    };
    let NameHit::Local(id) = resolve_from(lower, callee.sym.as_str(), scope) else {
        return;
    };
    if lower.graph.binding(id).fn_sites.is_empty() {
        return;
    }
    lower.function_prototype_used.set(true);
    lower.construct_used.set(true);
}


fn note_expr_for_builtins(lower: &Lowerer<'_>, expression: &ast::Expr) -> Result<(), ParseError> {
    note_function_prototype_surface(
        lower,
        expression,
        lower.catch_overlay.get().unwrap_or(lower.active_scope),
    );
    match expression {
        ast::Expr::Fn(func) => note_function_for_builtins(lower, &func.function),
        ast::Expr::Paren(paren) => note_expr_for_builtins(lower, &paren.expr),
        ast::Expr::Member(member) => {
            note_expr_for_builtins(lower, &member.obj)?;
            if let ast::MemberProp::Computed(computed) = &member.prop {
                note_expr_for_builtins(lower, &computed.expr)?;
            }
            if let ast::MemberProp::Ident(prop) = &member.prop
                && prop.sym.as_str()
                    == super::prelude::intrinsics::NUMBER_TO_STRING_NAME
            {
                // `Number.prototype.toString` lives on the private Number
                // prototype, which only a source that can name it pays for.
                // The row and its function node are gated on this flag. An
                // out-of-range radix throws the guest RangeError, so naming
                // the method also selects the RangeError realm.
                lower.number_to_string_used.set(true);
                lower.range_error_used.set(true);
                lower.needs_ordinary_realm.set(true);
            }
            if let ast::MemberProp::Computed(computed) = &member.prop
                && let ast::Expr::Lit(ast::Lit::Str(literal)) = unwrap_parens(&computed.expr)
                && literal.value
                    == super::prelude::intrinsics::NUMBER_TO_STRING_NAME
            {
                lower.number_to_string_used.set(true);
                lower.range_error_used.set(true);
                lower.needs_ordinary_realm.set(true);
            }
            if let ast::MemberProp::Ident(prop) = &member.prop
                && prop.sym.as_str() == super::prelude::intrinsics::FUNCTION_NAME_NAME
            {
                // `Function.prototype.name` is an accessor on the private
                // Function prototype. Only a source that can spell `name`
                // pays the accessor, its getter, and the per-function name
                // row/string storage.
                lower.function_name_used.set(true);
            }
            if let ast::MemberProp::Computed(computed) = &member.prop
                && let ast::Expr::Lit(ast::Lit::Str(literal)) = unwrap_parens(&computed.expr)
                && literal.value == super::prelude::intrinsics::FUNCTION_NAME_NAME
            {
                lower.function_name_used.set(true);
            }
            if let ast::MemberProp::Ident(prop) = &member.prop
                && prop.sym.as_str() == super::prelude::intrinsics::FUNCTION_TO_STRING_NAME
            {
                // `Function.prototype.toString` is a gated data row on the
                // same prototype. It also needs the per-function name storage
                // so it can print the real name.
                lower.function_to_string_used.set(true);
            }
            note_function_prototype_member(lower, member);
            if let ast::MemberProp::Computed(computed) = &member.prop
                && let ast::Expr::Lit(ast::Lit::Str(literal)) = unwrap_parens(&computed.expr)
                && literal.value == super::prelude::intrinsics::FUNCTION_TO_STRING_NAME
            {
                lower.function_to_string_used.set(true);
            }
            if let ast::MemberProp::Ident(prop) = &member.prop
                && prop.sym.as_str() == super::prelude::intrinsics::STRING_REPLACE_ALL_NAME
            {
                // `String.prototype.replaceAll` is a gated row on the private
                // String prototype. Only a source that can spell the name pays
                // the row, its function node and its ten-unit key String.
                lower.string_replace_all_used.set(true);
            }
            if let ast::MemberProp::Computed(computed) = &member.prop
                && let ast::Expr::Lit(ast::Lit::Str(literal)) = unwrap_parens(&computed.expr)
                && literal.value == super::prelude::intrinsics::STRING_REPLACE_ALL_NAME
            {
                lower.string_replace_all_used.set(true);
            }
            if let ast::MemberProp::Ident(prop) = &member.prop
                && prop.sym.as_str() == super::prelude::intrinsics::STRING_SEARCH_NAME
            {
                // `String.prototype.search` is a gated row on the private
                // String prototype in the one admitted `/a/` RegExp domain.
                // Only a source that can spell the name pays the row, its
                // function node, its six-unit key String and the private
                // matcher kernel pair. The argument brand is the private node
                // kind of the advertised literal, so the object realm is
                // required.
                lower.string_search_used.set(true);
                lower.needs_ordinary_realm.set(true);
            }
            if let ast::MemberProp::Computed(computed) = &member.prop
                && let ast::Expr::Lit(ast::Lit::Str(literal)) = unwrap_parens(&computed.expr)
                && literal.value == super::prelude::intrinsics::STRING_SEARCH_NAME
            {
                lower.string_search_used.set(true);
                lower.needs_ordinary_realm.set(true);
            }
            if let ast::MemberProp::Ident(prop) = &member.prop
                && prop.sym.as_str() == super::prelude::intrinsics::STRING_MATCH_NAME
            {
                // `String.prototype.match` is the `search` sibling on the same
                // private String prototype: one gated row in the one admitted
                // `/a/` RegExp domain. Only a source that can spell the name
                // pays the row, its function node and its five-unit key
                // String; the adapter reuses the realm's existing matcher.
                lower.string_match_used.set(true);
                lower.needs_ordinary_realm.set(true);
            }
            if let ast::MemberProp::Computed(computed) = &member.prop
                && let ast::Expr::Lit(ast::Lit::Str(literal)) = unwrap_parens(&computed.expr)
                && literal.value == super::prelude::intrinsics::STRING_MATCH_NAME
            {
                lower.string_match_used.set(true);
                lower.needs_ordinary_realm.set(true);
            }
            if let ast::MemberProp::Ident(prop) = &member.prop
                && prop.sym.as_str() == super::prelude::intrinsics::STRING_REPEAT_NAME
            {
                // Negative repeat throws guest RangeError. Naming the identifier
                // property installs the allocator before the budget charge.
                lower.range_error_used.set(true);
                lower.needs_ordinary_realm.set(true);
            }
            note_resolved_from_code_point(
                lower,
                member,
                lower.catch_overlay.get().unwrap_or(lower.active_scope),
            );
            Ok(())
        }
        ast::Expr::Call(call) => {
            if let ast::Callee::Expr(callee) = &call.callee {
                note_expr_for_builtins(lower, callee)?;
            }
            for arg in &call.args {
                note_expr_for_builtins(lower, &arg.expr)?;
            }
            Ok(())
        }
        ast::Expr::Cond(cond) => {
            note_expr_for_builtins(lower, &cond.test)?;
            note_expr_for_builtins(lower, &cond.cons)?;
            note_expr_for_builtins(lower, &cond.alt)
        }
        ast::Expr::Seq(seq) => {
            for expression in &seq.exprs {
                note_expr_for_builtins(lower, expression)?;
            }
            Ok(())
        }
        ast::Expr::Bin(bin) => {
            note_expr_for_builtins(lower, &bin.left)?;
            note_expr_for_builtins(lower, &bin.right)
        }
        ast::Expr::Unary(unary) => note_expr_for_builtins(lower, &unary.arg),
        ast::Expr::Update(update) => note_expr_for_builtins(lower, &update.arg),
        ast::Expr::Assign(assign) => {
            note_expr_for_builtins(lower, &assign.right)?;
            if let ast::AssignTarget::Simple(ast::SimpleAssignTarget::Member(member)) = &assign.left
            {
                // An admitted `arguments.length = v` / `arguments[i] = v` can
                // raise the guest TypeError("invalid array length"/"invalid
                // array subscript"), so the ordinary realm (private TypeError
                // allocator) is required before the route is chosen.
                if is_arguments_base(member) {
                    lower.needs_ordinary_realm.set(true);
                }
                note_expr_for_builtins(lower, &member.obj)?;
                // The target is not an `ast::Expr::Member`, so the read-side
                // name checks never see it: `o.search = v` and `o["search"] =
                // v` spell the gated row in the assignment left side.
                note_target_member_for_builtins(lower, member);
                if let ast::MemberProp::Computed(computed) = &member.prop {
                    note_expr_for_builtins(lower, &computed.expr)?;
                }
            }
            Ok(())
        }
        ast::Expr::New(construct) => {
            // Construction arguments are ordinary expressions: a source that
            // only spells a gated name inside `new ArrayBuffer(...)` still has
            // to install that row before any body is lowered.
            note_source_construct_gate(
                lower,
                construct,
                lower.catch_overlay.get().unwrap_or(lower.active_scope),
            );
            note_expr_for_builtins(lower, &construct.callee)?;
            if let Some(args) = &construct.args {
                for arg in args {
                    note_expr_for_builtins(lower, &arg.expr)?;
                }
            }
            Ok(())
        }
        ast::Expr::Array(array) => {
            for element in array.elems.iter().flatten() {
                note_expr_for_builtins(lower, &element.expr)?;
            }
            Ok(())
        }
        ast::Expr::Object(object) => note_object_for_builtins(lower, object),
        _ => Ok(()),
    }
}

fn note_object_for_builtins(
    lower: &Lowerer<'_>,
    object: &ast::ObjectLit,
) -> Result<(), ParseError> {
    for prop in &object.props {
        let ast::PropOrSpread::Prop(boxed) = prop else {
            continue;
        };
        match boxed.as_ref() {
            ast::Prop::KeyValue(kv) => note_expr_for_builtins(lower, &kv.value)?,
            ast::Prop::Getter(getter) => note_function_for_builtins(lower, &getter.function)?,
            ast::Prop::Setter(setter) => note_function_for_builtins(lower, &setter.function)?,
            _ => {}
        }
    }
    Ok(())
}

/// The assignment target is an `ast::AssignTarget`, not an `ast::Expr`, so
/// the read-side member walk never reaches its name. A source that only
/// spells `search` or `match` in `o.search = v` / `o["match"] = v` still has
/// to install the gated String-prototype row and the ordinary realm it needs.
fn note_target_member_for_builtins(lower: &Lowerer<'_>, member: &ast::MemberExpr) {
    let names = |name: &str| match &member.prop {
        ast::MemberProp::Ident(prop) => prop.sym.as_str() == name,
        ast::MemberProp::Computed(computed) => {
            matches!(
                unwrap_parens(&computed.expr),
                ast::Expr::Lit(ast::Lit::Str(literal)) if literal.value == name
            )
        }
        _ => false,
    };
    if names(super::prelude::intrinsics::STRING_SEARCH_NAME) {
        lower.string_search_used.set(true);
        lower.needs_ordinary_realm.set(true);
    }
    if names(super::prelude::intrinsics::STRING_MATCH_NAME) {
        lower.string_match_used.set(true);
        lower.needs_ordinary_realm.set(true);
    }
    if names(super::prelude::intrinsics::FUNCTION_PROTOTYPE_NAME)
        || names(super::prelude::intrinsics::CONSTRUCTOR_NAME)
    {
        // Writes share the read gate: `f.prototype = v` / `p.constructor = v`
        // in either the Script or a function body must select the same
        // `Function.prototype` accessor and constructor rows.
        lower.function_prototype_used.set(true);
    }
}

fn note_function_expr(
    lower: &Lowerer<'_>,
    expression: &ast::Expr,
    scope: ScopeId,
) -> Result<(), ParseError> {
    note_function_prototype_surface(lower, expression, scope);
    match expression {
        ast::Expr::Ident(ident) => {
            note_builtin_name(lower, ident.sym.as_str(), scope);
            Ok(())
        }
        ast::Expr::Fn(func) => note_function_for_builtins(lower, &func.function),
        ast::Expr::Paren(paren) => note_function_expr(lower, &paren.expr, scope),
        ast::Expr::Member(member) => {
            note_function_expr(lower, &member.obj, scope)?;
            if let ast::MemberProp::Computed(computed) = &member.prop {
                note_function_expr(lower, &computed.expr, scope)?;
            }
            // The body walk must select the same `Function.prototype` rows as
            // the Script walk: a body-local `.prototype`/`.constructor`
            // spelling is the only feature site a source may have, and it
            // must never depend on a later Script-side read to open the gate.
            note_function_prototype_member(lower, member);
            if let ast::MemberProp::Ident(prop) = &member.prop
                && prop.sym.as_str() == super::prelude::intrinsics::STRING_SEARCH_NAME
            {
                lower.string_search_used.set(true);
                lower.needs_ordinary_realm.set(true);
            }
            if let ast::MemberProp::Computed(computed) = &member.prop
                && let ast::Expr::Lit(ast::Lit::Str(literal)) = unwrap_parens(&computed.expr)
                && literal.value == super::prelude::intrinsics::STRING_SEARCH_NAME
            {
                lower.string_search_used.set(true);
                lower.needs_ordinary_realm.set(true);
            }
            if let ast::MemberProp::Ident(prop) = &member.prop
                && prop.sym.as_str() == super::prelude::intrinsics::STRING_MATCH_NAME
            {
                // `String.prototype.match` is the `search` sibling on the same
                // private String prototype: one gated row in the one admitted
                // `/a/` RegExp domain. Only a source that can spell the name
                // pays the row, its function node and its five-unit key
                // String; the adapter reuses the realm's existing matcher.
                lower.string_match_used.set(true);
                lower.needs_ordinary_realm.set(true);
            }
            if let ast::MemberProp::Computed(computed) = &member.prop
                && let ast::Expr::Lit(ast::Lit::Str(literal)) = unwrap_parens(&computed.expr)
                && literal.value == super::prelude::intrinsics::STRING_MATCH_NAME
            {
                lower.string_match_used.set(true);
                lower.needs_ordinary_realm.set(true);
            }
            note_resolved_from_code_point(lower, member, scope);
            Ok(())
        }
        ast::Expr::Call(call) => {
            if let ast::Callee::Expr(callee) = &call.callee {
                note_function_expr(lower, callee, scope)?;
            }
            for arg in &call.args {
                note_function_expr(lower, &arg.expr, scope)?;
            }
            Ok(())
        }
        ast::Expr::Cond(cond) => {
            note_function_expr(lower, &cond.test, scope)?;
            note_function_expr(lower, &cond.cons, scope)?;
            note_function_expr(lower, &cond.alt, scope)
        }
        ast::Expr::Seq(seq) => {
            for expression in &seq.exprs {
                note_function_expr(lower, expression, scope)?;
            }
            Ok(())
        }
        ast::Expr::Bin(bin) => {
            note_function_expr(lower, &bin.left, scope)?;
            note_function_expr(lower, &bin.right, scope)
        }
        ast::Expr::Unary(unary) => note_function_expr(lower, &unary.arg, scope),
        ast::Expr::Update(update) => note_function_expr(lower, &update.arg, scope),
        ast::Expr::Assign(assign) => {
            note_function_expr(lower, &assign.right, scope)?;
            if let ast::AssignTarget::Simple(ast::SimpleAssignTarget::Member(member)) = &assign.left
            {
                note_function_expr(lower, &member.obj, scope)?;
                note_target_member_for_builtins(lower, member);
                if let ast::MemberProp::Computed(computed) = &member.prop {
                    note_function_expr(lower, &computed.expr, scope)?;
                }
            }
            Ok(())
        }
        ast::Expr::New(construct) => {
            note_source_construct_gate(lower, construct, scope);
            note_function_expr(lower, &construct.callee, scope)?;
            if let Some(args) = &construct.args {
                for arg in args {
                    note_function_expr(lower, &arg.expr, scope)?;
                }
            }
            Ok(())
        }
        ast::Expr::Array(array) => {
            for element in array.elems.iter().flatten() {
                note_function_expr(lower, &element.expr, scope)?;
            }
            Ok(())
        }
        ast::Expr::Object(object) => {
            for prop in &object.props {
                let ast::PropOrSpread::Prop(boxed) = prop else {
                    continue;
                };
                match boxed.as_ref() {
                    ast::Prop::KeyValue(kv) => note_function_expr(lower, &kv.value, scope)?,
                    ast::Prop::Getter(getter) => {
                        note_function_for_builtins(lower, &getter.function)?
                    }
                    ast::Prop::Setter(setter) => {
                        note_function_for_builtins(lower, &setter.function)?
                    }
                    _ => {}
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

pub(super) fn original_kind(i: u8) -> Kind {
    if i == 1 { Kind::Object } else { Kind::Function }
}
fn note_original(lower: &Lowerer<'_>, i: u8) {
    lower.needs_ordinary_realm.set(true);
    if i == 1 {
        lower.math_used.set(true);
    } else if (2..6).contains(&i) {
        lower
            .numeric_globals
            .set(lower.numeric_globals.get() | (1 << (i - 2)));
    } else if (6..10).contains(&i) {
        lower.uri_used.set(lower.uri_used.get() | (1 << (i - 6)));
    }
}
pub(super) fn original_value(
    lower: &Lowerer<'_>,
    i: u8,
    span: swc_common::Span,
) -> Result<Expr, ParseError> {
    let root = if i < 2 {
        i64::from(i) + 4
    } else if i < 6 {
        super::prelude::math::global_root(lower, i - 2)
    } else {
        super::lowerer::uri_calls::function_root(lower, i - 6)
    };
    Ok(Lowerer::guest_call(
        lower
            .intrinsics
            .ok_or_else(|| lower.error(span, "original builtin requires ordinary realm"))?
            .root,
        vec![Expr::Var(lower.active_carrier(span)?), Expr::Int(root)],
    ))
}

fn note_builtin_name(lower: &Lowerer<'_>, name: &str, scope: ScopeId) {
    match resolve_from(lower, name, scope) {
        NameHit::BuiltinMath => {
            lower.needs_ordinary_realm.set(true);
            lower.math_used.set(true);
        }
        NameHit::BuiltinNumeric(i) => {
            lower.needs_ordinary_realm.set(true);
            lower
                .numeric_globals
                .set(lower.numeric_globals.get() | (1 << i));
        }
        NameHit::BuiltinNumberNamespace => {
            lower.needs_ordinary_realm.set(true);
            lower.number_namespace_used.set(true);
        }
        NameHit::BuiltinStringNamespace => {
            lower.needs_ordinary_realm.set(true);
            lower.string_namespace_used.set(true);
        }
        NameHit::BuiltinArrayBuffer => {
            lower.needs_ordinary_realm.set(true);
            lower.array_buffer_used.set(true);
        }
        NameHit::BuiltinTypedArray(element) => {
            lower.needs_ordinary_realm.set(true);
            lower.typed_array_elements.set(lower.typed_array_elements.get() | element.bit());
        }
        NameHit::BuiltinArrayNamespace => {
            lower.needs_ordinary_realm.set(true);
            lower.array_namespace_used.set(true);
        }
        NameHit::BuiltinBoolean => {
            lower.needs_ordinary_realm.set(true);
            lower.boolean_used.set(true);
        }
        NameHit::BuiltinRangeError => {
            lower.needs_ordinary_realm.set(true);
            lower.range_error_used.set(true);
        }
        NameHit::BuiltinObject | NameHit::BuiltinTypeError => lower.needs_ordinary_realm.set(true),
        NameHit::BuiltinUri(i) => note_original(lower, i + 6),
        _ => {}
    }
}

/// Run the same complete flow/binding-aware admitted-AST walk used by
/// emission. Specialized array/string expressions do not imply ordinary nodes.
/// The returned demand is the aggregate the route choice consumes: the
/// independent producers published by the walk itself plus whatever surviving
/// primitive-key prepared proof the walk admitted. The admission walk never
/// widens the cell eagerly, so a disarmed, probe or dead observation cannot
/// turn a pure script into an ordinary-realm one.
pub(super) fn script_needs_ordinary_realm(
    lower: &Lowerer<'_>,
    statements: &[ast::Stmt],
) -> Result<bool, ParseError> {
    for i in 0..super::global_binding::NAMES.len() as u8 {
        if lower.globals.contains(i) && lower.globals.initial & (1 << i) != 0 {
            note_original(lower, i);
        }
    }
    if lower.globals.shared {
        lower.needs_ordinary_realm.set(true);
    }
    note_function_builtin_uses(lower, statements)?;
    validate_statements(lower, statements)?;
    // Validate first: the primitive-key proofs are only known afterwards.
    Ok(lower.ordinary_realm_demand())
}

/// Coarse trigger only; literal decoding and flow permission use the actual scope.
fn possible_string_length_member(member: &ast::MemberExpr) -> bool {
    match &member.prop {
        ast::MemberProp::Ident(name) => name.sym == *"length",
        ast::MemberProp::Computed(key) => {
            matches!(unwrap_parens(&key.expr), ast::Expr::Lit(ast::Lit::Str(_)))
        }
        _ => false,
    }
}

pub(super) fn body_has_possible_feature_site(statements: &[ast::Stmt]) -> bool {
    let mut scan = SourceScan {
        scan_string_concat: true,
        ..Default::default()
    };
    scan.statements(statements, false);
    scan.possible_number_member
        || scan.possible_primitive_key
        || scan.possible_string_length
        || scan.possible_string_concat
        || scan.possible_relational
        || scan.possible_construct
}

pub(super) fn validate_function_feature_body(
    lower: &Lowerer<'_>,
    statements: &[ast::Stmt],
) -> Result<(), ParseError> {
    validate_statements(lower, statements)
}

fn validate_statements(lower: &Lowerer<'_>, statements: &[ast::Stmt]) -> Result<(), ParseError> {
    lower.set_validation_abrupt(false);
    let mut facts = lower
        .graph
        .scope(lower.active_scope)
        .order
        .iter()
        .map(|id| {
            let b = lower.graph.binding(*id);
            let canonical = (b.scope == lower.graph.script)
                .then(|| super::global_binding::index(&b.name))
                .flatten();
            let kind = if let Some(i) = canonical.filter(|i| lower.globals.contains(*i)) {
                if lower.globals.shared {
                    Kind::Dynamic
                } else if !b.fn_sites.is_empty() {
                    Kind::Function
                } else if lower.globals.initial & (1 << i) != 0 {
                    original_kind(i)
                } else {
                    Kind::Undefined
                }
            } else if !lower.graph.binding(*id).fn_sites.is_empty() {
                Kind::Function
            } else if lower
                .graph
                .scope(lower.active_scope)
                .formals
                .iter()
                .any(|(formal, _, _)| formal == id)
            {
                Kind::Dynamic
            } else {
                Kind::Undefined
            };
            (*id, ProfileFact::scalar(kind))
        })
        .collect();
    validate_list(lower, statements, &mut facts)?;
    Ok(())
}

/// Private validation successors. These are not runtime completions and are not
/// a new fixed-point. Only a possible successor is joined; a skipped store stays
/// on the isolated copy used to check dead text.
#[derive(Clone, Copy, PartialEq, Eq)]
enum JumpKind {
    Break,
    Continue,
}

/// Kind plus the exact label. Facts merge only when both are equal.
#[derive(Clone)]
struct LabeledExit {
    kind: JumpKind,
    label: String,
    facts: ProfileFacts,
}

#[derive(Clone)]
struct Exits {
    throw: Option<ProfileFacts>,
    ret: Option<ProfileFacts>,
    brk: Option<ProfileFacts>,
    cont: Option<ProfileFacts>,
    /// Labelled break/continue. An unlabeled loop or switch must not swallow
    /// these, and a label consumes only a break to its own name.
    labeled: Vec<LabeledExit>,
}

impl Exits {
    fn none() -> Self {
        Self {
            throw: None,
            ret: None,
            brk: None,
            cont: None,
            labeled: Vec::new(),
        }
    }

    fn merge(&mut self, other: Self) {
        merge_fact(&mut self.throw, other.throw);
        merge_fact(&mut self.ret, other.ret);
        merge_fact(&mut self.brk, other.brk);
        merge_fact(&mut self.cont, other.cont);
        self.merge_labeled(other.labeled);
    }

    fn merge_labeled(&mut self, extra: Vec<LabeledExit>) {
        for exit in extra {
            if let Some(existing) = self
                .labeled
                .iter_mut()
                .find(|item| item.kind == exit.kind && item.label == exit.label)
            {
                existing.facts = join_profile(&existing.facts, &exit.facts);
            } else {
                self.labeled.push(exit);
            }
        }
    }

    fn record_labeled(&mut self, kind: JumpKind, label: &str, facts: ProfileFacts) {
        self.merge_labeled(vec![LabeledExit {
            kind,
            label: label.to_string(),
            facts,
        }]);
    }

    fn take_labeled(&mut self, kind: JumpKind, label: &str) -> Option<ProfileFacts> {
        let index = self
            .labeled
            .iter()
            .position(|item| item.kind == kind && item.label == label)?;
        Some(self.labeled.remove(index).facts)
    }

    /// A normally completing finalizer keeps each surviving completion's kind
    /// and target, and replaces the fact map with one sound post-finalizer map.
    fn apply_sound_post(&mut self, post: &ProfileFacts) {
        let refresh = |slot: &mut Option<ProfileFacts>| {
            if slot.is_some() {
                *slot = Some(post.clone());
            }
        };
        refresh(&mut self.throw);
        refresh(&mut self.ret);
        refresh(&mut self.brk);
        refresh(&mut self.cont);
        for exit in &mut self.labeled {
            exit.facts = post.clone();
        }
    }

    /// A loop or switch consumes unlabeled break/continue. Throw, return, and
    /// labeled transfer still reach an enclosing finally.
    fn propagate_unconsumed(&self) -> Self {
        Self {
            throw: self.throw.clone(),
            ret: self.ret.clone(),
            brk: None,
            cont: None,
            labeled: self.labeled.clone(),
        }
    }
}

fn push_labeled_facts(into: &mut Vec<ProfileFacts>, labeled: &[LabeledExit]) {
    for exit in labeled {
        into.push(exit.facts.clone());
    }
}

struct StmtFlow {
    completes_normally: bool,
    exits: Exits,
}

impl StmtFlow {
    fn normal() -> Self {
        Self {
            completes_normally: true,
            exits: Exits::none(),
        }
    }

    fn abrupt(exits: Exits) -> Self {
        Self {
            completes_normally: false,
            exits,
        }
    }
}

fn throw_stmt(facts: &ProfileFacts) -> StmtFlow {
    let mut exits = Exits::none();
    exits.throw = Some(facts.clone());
    StmtFlow::abrupt(exits)
}

fn flow_after_expr(lower: &Lowerer<'_>, facts: &ProfileFacts) -> StmtFlow {
    if lower.validation_abrupt() {
        lower.set_validation_abrupt(false);
        lower.clear_validation_conditional();
        throw_stmt(facts)
    } else if lower.take_validation_conditional() {
        let snaps = lower.take_validation_throw();
        let throw_facts = if snaps.is_empty() {
            facts.clone()
        } else {
            facts_from_snaps(&snaps)
        };
        let mut exits = Exits::none();
        exits.throw = Some(throw_facts);
        StmtFlow {
            completes_normally: true,
            exits,
        }
    } else {
        StmtFlow::normal()
    }
}

fn snaps_of(facts: &ProfileFacts) -> Vec<super::lowerer::primitive_key::FactSnap> {
    facts
        .iter()
        .map(|(id, fact)| super::lowerer::primitive_key::FactSnap {
            id: *id,
            kind: fact.kind,
            array_length: fact.array_length,
            guarded_numeric: fact.guarded_numeric,
        })
        .collect()
}

fn facts_from_snaps(snaps: &[super::lowerer::primitive_key::FactSnap]) -> ProfileFacts {
    snaps
        .iter()
        .map(|snap| {
            (
                snap.id,
                ProfileFact {
                    kind: snap.kind,
                    array_length: snap.array_length,
                    guarded_numeric: snap.guarded_numeric,
                },
            )
        })
        .collect()
}

fn nullish_kind(kind: Kind) -> bool {
    matches!(kind, Kind::Null | Kind::Undefined)
}

fn capability_expr(
    lower: &Lowerer<'_>,
    expression: &ast::Expr,
    facts: &ProfileFacts,
) -> Result<(), ParseError> {
    let mut dead = facts.clone();
    let abrupt = lower.validation_abrupt();
    let previous_conditional = lower.take_validation_conditional();
    let previous_snaps = lower.take_validation_throw();
    lower.set_validation_abrupt(false);
    // The operand only runs after an abrupt point, so it is capability text:
    // it must keep every original admission/refusal check, but it may not
    // publish a primitive-key prepared proof or the realm demand that proof
    // would consume. Mark the exact discarded span so emission can keep the
    // same capability-only boundary; the isolate below restores the flag on
    // both success and error and leaves independent producer cells untouched.
    let result = lower.probe_dead_capability(|| {
        lower.note_dead_span(expression.span());
        validate_expr(lower, expression, &mut dead)
    });
    lower.set_validation_abrupt(abrupt);
    lower.clear_validation_conditional();
    if previous_conditional {
        lower.set_validation_possible(previous_snaps);
    }
    result.map(|_| ())
}

fn validate_computed_key(
    lower: &Lowerer<'_>,
    key_expr: &ast::Expr,
    facts: &mut ProfileFacts,
) -> Result<ProfileFact, ParseError> {
    if lower.validation_abrupt() {
        capability_expr(lower, key_expr, facts)?;
        Ok(ProfileFact::scalar(Kind::Dynamic))
    } else {
        validate_expr(lower, key_expr, facts)
    }
}

fn merge_fact(slot: &mut Option<ProfileFacts>, extra: Option<ProfileFacts>) {
    *slot = match (slot.take(), extra) {
        (None, other) | (other, None) => other,
        (Some(left), Some(right)) => Some(join_profile(&left, &right)),
    };
}

fn join_all(maps: &[ProfileFacts]) -> ProfileFacts {
    let mut acc = maps[0].clone();
    for next in &maps[1..] {
        acc = join_profile(&acc, next);
    }
    acc
}

fn push_fact(into: &mut Vec<ProfileFacts>, fact: &Option<ProfileFacts>) {
    if let Some(fact) = fact {
        into.push(fact.clone());
    }
}

struct OverlayRestore<'a> {
    cell: &'a std::cell::Cell<Option<super::scope::ScopeId>>,
    previous: Option<super::scope::ScopeId>,
}

impl Drop for OverlayRestore<'_> {
    fn drop(&mut self) {
        self.cell.set(self.previous);
    }
}

fn with_catch_overlay<T>(
    lower: &Lowerer<'_>,
    scope: Option<super::scope::ScopeId>,
    body: impl FnOnce() -> Result<T, ParseError>,
) -> Result<T, ParseError> {
    let Some(scope) = scope else {
        return body();
    };
    let previous = lower.catch_overlay.get();
    lower.catch_overlay.set(Some(scope));
    let _restore = OverlayRestore {
        cell: &lower.catch_overlay,
        previous,
    };
    body()
}

fn validate_list(
    lower: &Lowerer<'_>,
    statements: &[ast::Stmt],
    facts: &mut ProfileFacts,
) -> Result<StmtFlow, ParseError> {
    // An abrupt statement ends fall-through publication for this list. Later
    // text is still checked, on an isolated fact copy, so a skipped store
    // cannot reach a finally, catch, or post-try successor. This validates
    // capability only and cannot emit an initializer or mutate Lowerer state.
    let mut exits = Exits::none();
    for (index, statement) in statements.iter().enumerate() {
        // Dead text is checked one statement at a time against the facts at
        // the abrupt point. A skipped declarator must not admit a later store
        // that emission, which restores those facts, would refuse.
        if lower.in_dead_capability() {
            let mut isolated = facts.clone();
            validate_stmt(lower, statement, &mut isolated)?;
            continue;
        }
        let flow = validate_stmt(lower, statement, facts)?;
        exits.merge(flow.exits);
        if !flow.completes_normally {
            if index + 1 < statements.len() {
                let mut isolated = facts.clone();
                lower.probe_dead_capability(|| {
                    validate_list(lower, &statements[index + 1..], &mut isolated)
                })?;
            }
            return Ok(StmtFlow {
                completes_normally: false,
                exits,
            });
        }
    }
    Ok(StmtFlow {
        completes_normally: true,
        exits,
    })
}

fn validate_stmt(
    lower: &Lowerer<'_>,
    statement: &ast::Stmt,
    facts: &mut ProfileFacts,
) -> Result<StmtFlow, ParseError> {
    lower.note_dead_span(statement.span());
    match statement {
        ast::Stmt::Empty(_) | ast::Stmt::Debugger(_) => Ok(StmtFlow::normal()),
        ast::Stmt::Decl(ast::Decl::Fn(_)) => Ok(StmtFlow::normal()),
        ast::Stmt::Decl(ast::Decl::Var(declaration)) => {
            validate_var_decl(lower, declaration, facts)
        }
        ast::Stmt::Block(block) => validate_list(lower, &block.stmts, facts),
        ast::Stmt::Expr(expr) => {
            if let ast::Expr::Assign(assign) = expr.expr.as_ref()
                && assign.op == ast::AssignOp::Assign
                && let ast::AssignTarget::Simple(ast::SimpleAssignTarget::Member(member)) =
                    &assign.left
            {
                if is_arguments_base(member)
                    && lower.function_args.is_some()
                    && implicit_arguments(lower)
                {
                    validate_arguments_store(lower, member, &assign.right, facts)?;
                    return Ok(StmtFlow::normal());
                }
                let base = validate_expr(lower, &member.obj, facts)?;
                if let Some(key_expr) = Lowerer::open_computed_key(member) {
                    let key = validate_computed_key(lower, key_expr, facts)?;
                    if lower.admit_primitive_member(
                        member,
                        base.kind,
                        key.kind,
                        super::lowerer::primitive_key::Role::Put,
                    )? {
                        if nullish_kind(base.kind) || lower.validation_abrupt() {
                            capability_expr(lower, &assign.right, facts)?;
                            return Ok(throw_stmt(facts));
                        }
                        let before = facts.clone();
                        let rhs = validate_expr(lower, &assign.right, facts)?;
                        admit_object_value(lower, assign.right.span(), rhs.kind)?;
                        if base.kind == Kind::Dynamic {
                            let mut exits = Exits::none();
                            exits.throw = Some(before);
                            return Ok(StmtFlow {
                                completes_normally: true,
                                exits,
                            });
                        }
                        return Ok(StmtFlow::normal());
                    }
                }
                admit_member_write(lower, member, base.kind, base.array_length)?;
                let rhs = validate_expr(lower, &assign.right, facts)?;
                let length_name = matches!(&member.prop, ast::MemberProp::Ident(prop) if prop.sym.as_str() == "length");
                if base.kind == Kind::Array && length_name {
                    if matches!(rhs.kind, Kind::String | Kind::Dynamic) {
                        lower.record_string_number_site(assign.span)?;
                    }
                    // A constant in-range length is the live length for later
                    // literal indexes. Anything else makes the length unknown,
                    // so a later hole write is checked at runtime instead of
                    // refused from the stale literal shape.
                    if let ast::Expr::Ident(object) = member.obj.as_ref()
                        && let NameHit::Local(id) = resolve(lower, object.sym.as_str())
                        && let Some(fact) = facts.get_mut(&id)
                    {
                        let next = Lowerer::member_index_literal(&assign.right)
                            .and_then(|value| usize::try_from(value).ok())
                            .filter(|value| *value < (1usize << 30));
                        match next {
                            Some(length) => {
                                if let Some(shape) = fact.array_length.as_mut() {
                                    if length < shape.number_only_below {
                                        shape.number_only_below = length;
                                    }
                                    shape.length = length;
                                } else {
                                    fact.array_length = Some(super::lowerer::ArrayShape {
                                        length,
                                        repr: super::lowerer::ArrayRepr::Tagged,
                                        number_only_below: 0,
                                    });
                                }
                            }
                            None => fact.array_length = None,
                        }
                    }
                } else if base.kind == Kind::Array && !length_name {
                    let hole = match &member.prop {
                        ast::MemberProp::Computed(property) => Lowerer::member_index_literal(&property.expr)
                            .and_then(|index| usize::try_from(index).ok())
                            .is_some_and(|index| {
                                base.array_length
                                    .is_some_and(|shape| index >= shape.number_only_below)
                            }),
                        _ => false,
                    };
                    let repr = if hole {
                        Some(super::lowerer::ArrayRepr::Tagged)
                    } else {
                        base.array_length.map(|shape| shape.repr)
                    };
                    admit_array_element(
                        lower,
                        assign.right.span(),
                        rhs.kind,
                        repr,
                    )?;
                } else {
                    admit_object_value(lower, assign.right.span(), rhs.kind)?;
                }
                return Ok(StmtFlow::normal());
            }
            validate_expr(lower, &expr.expr, facts)?;
            Ok(flow_after_expr(lower, facts))
        }
        ast::Stmt::Return(ret) => {
            if let Some(arg) = &ret.arg {
                validate_expr(lower, arg, facts)?;
            }
            let mut exits = Exits::none();
            exits.ret = Some(facts.clone());
            Ok(StmtFlow::abrupt(exits))
        }
        ast::Stmt::If(if_stmt) => {
            validate_expr(lower, &if_stmt.test, facts)?;
            let mut consequent = facts.clone();
            let mut alternate = facts.clone();
            let cons_flow = validate_stmt(lower, &if_stmt.cons, &mut consequent)?;
            let alt_flow = if let Some(alt) = &if_stmt.alt {
                validate_stmt(lower, alt, &mut alternate)?
            } else {
                StmtFlow::normal()
            };
            if is_declaration_only_if(if_stmt) {
                if literal_bool(&if_stmt.test) == Some(true) {
                    *facts = consequent;
                    return Ok(cons_flow);
                }
                *facts = alternate;
                return Ok(alt_flow);
            }
            *facts = join_profile(&consequent, &alternate);
            let mut exits = cons_flow.exits;
            exits.merge(alt_flow.exits);
            Ok(StmtFlow {
                completes_normally: cons_flow.completes_normally || alt_flow.completes_normally,
                exits,
            })
        }
        ast::Stmt::Throw(throw) => {
            validate_expr(lower, &throw.arg, facts)?;
            let mut exits = Exits::none();
            exits.throw = Some(facts.clone());
            Ok(StmtFlow::abrupt(exits))
        }
        ast::Stmt::Try(try_stmt) => validate_try(lower, try_stmt, facts),
        ast::Stmt::While(loop_) => {
            validate_iteration(lower, Some(&loop_.test), &loop_.body, None, facts)
        }
        ast::Stmt::DoWhile(loop_) => {
            validate_iteration(lower, Some(&loop_.test), &loop_.body, None, facts)
        }
        ast::Stmt::For(loop_) => {
            let mut init_flow = StmtFlow::normal();
            if let Some(init) = &loop_.init {
                init_flow = match init {
                    ast::VarDeclOrExpr::VarDecl(declaration) => {
                        validate_var_decl(lower, declaration, facts)?
                    }
                    ast::VarDeclOrExpr::Expr(expression) => {
                        validate_expr(lower, expression, facts)?;
                        flow_after_expr(lower, facts)
                    }
                };
                if !init_flow.completes_normally {
                    lower.probe_dead_capability(|| {
                        let mut isolated = facts.clone();
                        validate_iteration(
                            lower,
                            loop_.test.as_deref(),
                            &loop_.body,
                            loop_.update.as_deref(),
                            &mut isolated,
                        )
                    })?;
                    return Ok(init_flow);
                }
            }
            let mut loop_flow = validate_iteration(
                lower,
                loop_.test.as_deref(),
                &loop_.body,
                loop_.update.as_deref(),
                facts,
            )?;
            loop_flow.exits.merge(init_flow.exits);
            Ok(loop_flow)
        }
        ast::Stmt::ForOf(loop_) => validate_for_of(lower, loop_, facts),
        ast::Stmt::Labeled(labeled) => validate_labeled(lower, labeled, facts),
        ast::Stmt::Switch(select) => validate_switch(lower, select, facts),
        ast::Stmt::Break(brk) => {
            let mut exits = Exits::none();
            if let Some(label) = &brk.label {
                exits.record_labeled(JumpKind::Break, label.sym.as_str(), facts.clone());
            } else {
                exits.brk = Some(facts.clone());
            }
            Ok(StmtFlow::abrupt(exits))
        }
        ast::Stmt::Continue(cont) => {
            let mut exits = Exits::none();
            if let Some(label) = &cont.label {
                exits.record_labeled(JumpKind::Continue, label.sym.as_str(), facts.clone());
            } else {
                exits.cont = Some(facts.clone());
            }
            Ok(StmtFlow::abrupt(exits))
        }
        ast::Stmt::ForIn(_) if is_inventory_for_in_single_a(statement) => {
            validate_inventory_for_in(lower, statement, facts)
        }
        _ => Err(lower.error(statement.span(), "unsupported JavaScript ES5 P0 statement")),
    }
}

/// A labelled break becomes a normal completion at its own label and joins any
/// path that already falls through. Another label's break, and every continue,
/// propagate. Consecutive labels recurse, so each name consumes only itself.
fn validate_labeled(
    lower: &Lowerer<'_>,
    labeled: &ast::LabeledStmt,
    facts: &mut ProfileFacts,
) -> Result<StmtFlow, ParseError> {
    let mut flow = validate_stmt(lower, &labeled.body, facts)?;
    let Some(broken) = flow
        .exits
        .take_labeled(JumpKind::Break, labeled.label.sym.as_str())
    else {
        return Ok(flow);
    };
    if flow.completes_normally {
        *facts = join_profile(facts, &broken);
    } else {
        *facts = broken;
    }
    flow.completes_normally = true;
    Ok(flow)
}

/// Capability validation for a switch statement. The discriminant always
/// runs; selectors run in search order only until the first match and every
/// selected suffix (match at each clause, the default-only path and the
/// no-match path) joins at the switch end. Clause facts never dominate
/// siblings or post-switch code: each suffix path starts from the shared
/// selector effects and all ends join, the same direction as the loop and
/// conditional joins.
fn validate_switch(
    lower: &Lowerer<'_>,
    select: &ast::SwitchStmt,
    facts: &mut ProfileFacts,
) -> Result<StmtFlow, ParseError> {
    validate_expr(lower, &select.discriminant, facts)?;
    // Validation never installs emission caches: only the flow facts move.
    let base = facts.clone();
    // Selectors run in source search order until the first strict match; an
    // unselected clause's selector still participates here so its
    // declarations and calls validate at compile time.
    let mut search = base.clone();
    for case in &select.cases {
        if let Some(test) = &case.test {
            validate_expr(lower, test, &mut search)?;
        }
    }
    let mut joined = base.clone();
    let mut exits = Exits::none();
    for start in 0..select.cases.len() {
        let mut path = search.clone();
        for case in &select.cases[start..] {
            let flow = validate_list(lower, &case.cons, &mut path)?;
            // Unlabeled break completes the switch. Throw, return, continue,
            // and labeled transfer still leave it. The no-match join is unchanged.
            let mut leaving = flow.exits;
            leaving.brk = None;
            exits.merge(leaving);
        }
        joined = join_profile(&joined, &path);
    }
    // The no-match path runs only the searched selectors.
    joined = join_profile(&joined, &search);
    *facts = joined;
    Ok(StmtFlow {
        completes_normally: true,
        exits,
    })
}

fn validate_var_decl(
    lower: &Lowerer<'_>,
    declaration: &ast::VarDecl,
    facts: &mut ProfileFacts,
) -> Result<StmtFlow, ParseError> {
    if declaration.kind != ast::VarDeclKind::Var {
        return Err(lower.error(
            declaration.span,
            "JavaScript ES5 supports var declarations; let and const are rejected",
        ));
    }
    let mut exits = Exits::none();
    for (index, declarator) in declaration.decls.iter().enumerate() {
        let ast::Pat::Ident(binding) = &declarator.name else {
            return Err(lower.error(declarator.span, "P0 requires an identifier binding"));
        };
        let Some(init) = &declarator.init else {
            continue;
        };
        let fact = validate_expr(lower, init, facts)?;
        if lower.validation_abrupt() {
            lower.set_validation_abrupt(false);
            lower.clear_validation_conditional();
            for later in &declaration.decls[index + 1..] {
                if let Some(later_init) = &later.init {
                    capability_expr(lower, later_init, facts)?;
                }
            }
            exits.merge(throw_stmt(facts).exits);
            return Ok(StmtFlow::abrupt(exits));
        }
        if let NameHit::Local(id) = resolve(lower, binding.id.sym.as_str()) {
            facts.insert(id, fact);
        }
        if lower.take_validation_conditional() {
            let snaps = lower.take_validation_throw();
            let throw_facts = if snaps.is_empty() {
                facts.clone()
            } else {
                facts_from_snaps(&snaps)
            };
            let mut conditional = Exits::none();
            conditional.throw = Some(throw_facts);
            exits.merge(conditional);
        }
    }
    Ok(StmtFlow {
        completes_normally: true,
        exits,
    })
}

/// Numeric dense-Array `for-of`. A non-array RHS is a managed TypeError, so
/// the body is capability-checked as dead and does not publish an assignment.
/// A possible array joins the zero-iteration entry facts with the Number
/// assignment, then re-checks the back edge under that join.
fn validate_for_of(
    lower: &Lowerer<'_>,
    loop_: &ast::ForOfStmt,
    facts: &mut ProfileFacts,
) -> Result<StmtFlow, ParseError> {
    let rhs = validate_expr(lower, &loop_.right, facts)?;
    let abrupt = lower.validation_abrupt();
    if abrupt {
        lower.set_validation_abrupt(false);
    }
    let iterable = matches!(rhs.kind, Kind::Array | Kind::Dynamic) && !abrupt;
    let proved_empty =
        rhs.kind == Kind::Array && rhs.array_length.is_some_and(|shape| shape.length == 0);
    // A statically packed source yields proven Number items; tagged arrays,
    // length-unknown joins and call-result Dynamic sources yield unproved
    // JsValue items, so bindings must never be falsely typed as Number.
    let item_kind = if rhs.kind == Kind::Array
        && rhs
            .array_length
            .is_some_and(|shape| shape.repr == super::lowerer::ArrayRepr::Packed)
    {
        Kind::Number
    } else {
        Kind::Dynamic
    };
    if !iterable {
        lower.probe_dead_capability(|| {
            let mut isolated = facts.clone();
            note_for_of_target(lower, &loop_.left, &mut isolated, item_kind)?;
            validate_stmt(lower, &loop_.body, &mut isolated)?;
            Ok(())
        })?;
        let mut exits = Exits::none();
        exits.throw = Some(facts.clone());
        return Ok(StmtFlow::abrupt(exits));
    }
    let entry = facts.clone();
    let (after_body, body_flow) = lower.probe_relational_collection(|| {
        lower.probe_primitive_key(|| {
            let mut after = entry.clone();
            lower.probe_dead_capability(|| {
                note_for_of_target(lower, &loop_.left, &mut after, item_kind)
            })?;
            let body_flow = validate_stmt(lower, &loop_.body, &mut after)?;
            Ok((after, body_flow))
        })
    })?;
    let joined = join_profile(&entry, &after_body);
    let mut back = joined.clone();
    note_for_of_target_maybe_dead(lower, &loop_.left, &mut back, proved_empty, item_kind)?;
    let mut key_throw = None;
    if lower.take_validation_conditional() {
        let snaps = lower.take_validation_throw();
        key_throw = Some(if snaps.is_empty() {
            entry.clone()
        } else {
            facts_from_snaps(&snaps)
        });
    }
    validate_stmt(lower, &loop_.body, &mut back)?;
    *facts = joined;
    let mut exits = body_flow.exits.propagate_unconsumed();
    if let Some(throw_facts) = key_throw {
        let mut conditional = Exits::none();
        conditional.throw = Some(throw_facts);
        exits.merge(conditional);
    }
    Ok(StmtFlow {
        completes_normally: true,
        exits,
    })
}

fn note_for_of_target_maybe_dead(
    lower: &Lowerer<'_>,
    head: &ast::ForHead,
    facts: &mut ProfileFacts,
    proved_empty: bool,
    item_kind: Kind,
) -> Result<(), ParseError> {
    if proved_empty {
        // A proved empty array never evaluates the head. Keep the capability
        // check, but do not publish its facts, type names, or throw.
        lower.probe_dead_capability(|| note_for_of_target(lower, head, facts, item_kind))
    } else {
        note_for_of_target(lower, head, facts, item_kind)
    }
}

fn note_for_of_target(
    lower: &Lowerer<'_>,
    head: &ast::ForHead,
    facts: &mut ProfileFacts,
    item_kind: Kind,
) -> Result<(), ParseError> {
    match head {
        ast::ForHead::VarDecl(declaration) => {
            if let ast::Pat::Ident(binding) = &declaration.decls[0].name
                && let NameHit::Local(id) = resolve(lower, binding.id.sym.as_str())
            {
                facts.insert(id, ProfileFact::scalar(item_kind));
            }
            Ok(())
        }
        ast::ForHead::Pat(pat) => match pat.as_ref() {
            ast::Pat::Ident(binding) => {
                if let NameHit::Local(id) = resolve(lower, binding.id.sym.as_str()) {
                    facts.insert(id, ProfileFact::scalar(item_kind));
                }
                Ok(())
            }
            ast::Pat::Expr(expr) => {
                let member = for_of_validate_member(expr).ok_or_else(|| {
                    lower.error(expr.span(), "for-of assignment target must be a member")
                })?;
                let base = validate_expr(lower, &member.obj, facts)?;
                if let Some(key_expr) = Lowerer::open_computed_key(member) {
                    // The key runs only after a next value exists. A proved key
                    // throw is not a loop-entry abrupt, and a dead or probe
                    // walk must not publish the key's facts or type names.
                    let saved_facts = facts.clone();
                    let saved_abrupt = lower.validation_abrupt();
                    let key = validate_computed_key(lower, key_expr, facts)?;
                    if lower.admit_primitive_member(
                        member,
                        base.kind,
                        key.kind,
                        super::lowerer::primitive_key::Role::Put,
                    )? {
                        let key_abrupt = lower.validation_abrupt() && !saved_abrupt;
                        if lower.in_dead_capability() {
                            *facts = saved_facts;
                            lower.set_validation_abrupt(saved_abrupt);
                        } else if key_abrupt {
                            lower.set_validation_abrupt(saved_abrupt);
                            lower.set_validation_possible(snaps_of(facts));
                        }
                        return Ok(());
                    }
                } else if let ast::MemberProp::Computed(computed) = &member.prop {
                    validate_expr(lower, &computed.expr, facts)?;
                }
                let length = if let ast::Expr::Ident(object) = member.obj.as_ref() {
                    lower.array_lengths.get(object.sym.as_str()).copied()
                } else {
                    None
                };
                admit_member_write(lower, member, base.kind, length)
            }
            _ => Err(lower.error(
                head.span(),
                "for-of head is not an admitted assignment target",
            )),
        },
        _ => Err(lower.error(
            head.span(),
            "for-of head is not an admitted assignment target",
        )),
    }
}

fn for_of_validate_member(expr: &ast::Expr) -> Option<&ast::MemberExpr> {
    match expr {
        ast::Expr::Paren(paren) => for_of_validate_member(&paren.expr),
        ast::Expr::Member(member) => Some(member),
        _ => None,
    }
}

/// Capability validation for a loop body/back edge. Zero iterations and later
/// iterations both matter: the entry facts are joined with the body/update
/// effects before the test and body are re-checked, so a first-iteration kind
/// or array length can never dominate the back edge. Numeric counter facts
/// survive because a Number-preserving body joins to Number.
fn validate_iteration(
    lower: &Lowerer<'_>,
    test: Option<&ast::Expr>,
    body: &ast::Stmt,
    update: Option<&ast::Expr>,
    facts: &mut ProfileFacts,
) -> Result<StmtFlow, ParseError> {
    let entry = facts.clone();
    // Relational proofs belong to the conservative walk below, not this
    // entry-only effect probe. Other operator proof collectors are unchanged.
    let (after_body, after_update, body_flow) = lower.probe_relational_collection(|| {
        lower.probe_primitive_key(|| {
            let mut after_test = entry.clone();
            if let Some(test) = test {
                validate_expr(lower, test, &mut after_test)?;
            }
            let mut after_body = after_test.clone();
            let body_flow = validate_stmt(lower, body, &mut after_body)?;
            let mut after_update = after_body.clone();
            if let Some(update) = update {
                validate_expr(lower, update, &mut after_update)?;
            }
            Ok((after_body, after_update, body_flow))
        })
    })?;
    let joined = join_profile(&join_profile(&entry, &after_body), &after_update);
    // Back-edge check with widened facts. This is the zero-iteration/next
    // iteration guarantee: emission uses the entry kinds, so the back edge
    // must be admitted under the joined facts as well. The published map is
    // still this first widen; the second walk is not republished.
    let mut back = joined.clone();
    if let Some(test) = test {
        validate_expr(lower, test, &mut back)?;
    }
    validate_stmt(lower, body, &mut back)?;
    if let Some(update) = update {
        validate_expr(lower, update, &mut back)?;
    }
    *facts = joined;
    Ok(StmtFlow {
        completes_normally: true,
        exits: body_flow.exits.propagate_unconsumed(),
    })
}

fn validate_try(
    lower: &Lowerer<'_>,
    try_stmt: &ast::TryStmt,
    facts: &mut ProfileFacts,
) -> Result<StmtFlow, ParseError> {
    let entry = facts.clone();
    let mut body_facts = entry.clone();
    let body = validate_list(lower, &try_stmt.block.stmts, &mut body_facts)?;

    let mut catch_normal: Option<ProfileFacts> = None;
    let mut catch_exits = Exits::none();
    if let Some(handler) = &try_stmt.handler {
        let key = (handler.span.lo.0, handler.span.hi.0);
        let catch_scope = lower.graph.catch_scopes.get(&key).copied();
        let reachable = body.exits.throw.is_some();
        let mut catch_facts = if let Some(throw_facts) = &body.exits.throw {
            throw_facts.clone()
        } else {
            entry.clone()
        };
        let mut installed = None;
        if let (Some(scope), Some(ast::Pat::Ident(binding))) = (catch_scope, &handler.param)
            && let Some(id) = lower
                .graph
                .scope(scope)
                .by_name
                .get(binding.id.sym.as_str())
                .copied()
        {
            catch_facts.insert(id, ProfileFact::scalar(Kind::Dynamic));
            installed = Some(scope);
        }
        let flow = with_catch_overlay(lower, installed, || {
            validate_list(lower, &handler.body.stmts, &mut catch_facts)
        })?;
        // Unreachable catch text is still capability-checked. Its stores and
        // transfers are not possible successors.
        if reachable {
            if flow.completes_normally {
                catch_normal = Some(catch_facts);
            }
            catch_exits = flow.exits;
        }
    }

    let mut finally_entries = Vec::new();
    if body.completes_normally {
        finally_entries.push(body_facts.clone());
    }
    if try_stmt.handler.is_some() {
        if let Some(caught) = &catch_normal {
            finally_entries.push(caught.clone());
        }
        push_fact(&mut finally_entries, &catch_exits.throw);
        push_fact(&mut finally_entries, &catch_exits.ret);
        push_fact(&mut finally_entries, &catch_exits.brk);
        push_fact(&mut finally_entries, &catch_exits.cont);
        push_labeled_facts(&mut finally_entries, &catch_exits.labeled);
    } else {
        push_fact(&mut finally_entries, &body.exits.throw);
    }
    push_fact(&mut finally_entries, &body.exits.ret);
    push_fact(&mut finally_entries, &body.exits.brk);
    push_fact(&mut finally_entries, &body.exits.cont);
    push_labeled_facts(&mut finally_entries, &body.exits.labeled);

    let mut post_preds = Vec::new();
    if body.completes_normally {
        post_preds.push(body_facts);
    }
    if let Some(caught) = catch_normal {
        post_preds.push(caught);
    }

    let mut exits = Exits::none();
    if try_stmt.handler.is_none() {
        merge_fact(&mut exits.throw, body.exits.throw.clone());
    }
    exits.merge(Exits {
        throw: catch_exits.throw.clone(),
        ret: catch_exits.ret.clone(),
        brk: catch_exits.brk.clone(),
        cont: catch_exits.cont.clone(),
        labeled: catch_exits.labeled.clone(),
    });
    merge_fact(&mut exits.ret, body.exits.ret.clone());
    merge_fact(&mut exits.brk, body.exits.brk.clone());
    merge_fact(&mut exits.cont, body.exits.cont.clone());
    exits.merge_labeled(body.exits.labeled.clone());

    if let Some(finalizer) = &try_stmt.finalizer {
        if finally_entries.is_empty() {
            finally_entries.push(entry);
        }
        // Admission sees every completion that enters finally, including a
        // transfer that skipped a later store. Publication uses only normal
        // successors, so that skipped store cannot escape.
        let mut admit = join_all(&finally_entries);
        let admit_flow = validate_list(lower, &finalizer.stmts, &mut admit)?;
        if !admit_flow.completes_normally {
            // An abrupt finalizer replaces every pending completion. Pre-finalizer
            // maps and a throw already consumed by catch do not propagate.
            return Ok(StmtFlow {
                completes_normally: false,
                exits: admit_flow.exits,
            });
        }
        // One joined walk. Surviving exits keep kind and target and take this
        // sound post-finalizer map. Mixed exits from the finalizer are merged
        // as recorded; the finalizer is not replayed per label or exit.
        exits.apply_sound_post(&admit);
        exits.merge(admit_flow.exits);
        if post_preds.is_empty() {
            return Ok(StmtFlow {
                completes_normally: false,
                exits,
            });
        }
        let mut published = join_all(&post_preds);
        let published_flow = validate_list(lower, &finalizer.stmts, &mut published)?;
        if !published_flow.completes_normally {
            return Ok(StmtFlow {
                completes_normally: false,
                exits,
            });
        }
        *facts = published;
        return Ok(StmtFlow {
            completes_normally: true,
            exits,
        });
    }

    if post_preds.is_empty() {
        return Ok(StmtFlow {
            completes_normally: false,
            exits,
        });
    }
    *facts = join_all(&post_preds);
    Ok(StmtFlow {
        completes_normally: true,
        exits,
    })
}

fn array_namespace_fact(lower: &Lowerer<'_>) -> ProfileFact {
    lower.needs_ordinary_realm.set(true);
    lower.array_namespace_used.set(true);
    ProfileFact::scalar(Kind::Function)
}

fn validate_member_base(
    lower: &Lowerer<'_>,
    member: &ast::MemberExpr,
    facts: &mut ProfileFacts,
) -> Result<ProfileFact, ParseError> {
    if matches!(unwrap_parens(&member.obj), ast::Expr::Ident(ident)
        if ident.sym == *"Array" && matches!(resolve(lower, "Array"), NameHit::BuiltinArrayNamespace))
    {
        let namespace_only = match &member.prop {
            ast::MemberProp::Ident(prop) => matches!(prop.sym.as_str(), "isArray" | "prototype"),
            ast::MemberProp::Computed(prop) => matches!(unwrap_parens(&prop.expr),
                ast::Expr::Lit(ast::Lit::Str(key)) if key.value == *"isArray" || key.value == *"prototype"),
            _ => false,
        };
        if namespace_only {
            // The static method and the prototype row cannot expose the
            // constructor: `Array.prototype` is the private packed prototype,
            // never the Array carrier itself.
            return Ok(array_namespace_fact(lower));
        }
    }
    validate_expr(lower, &member.obj, facts)
}

fn ident_fact(
    lower: &Lowerer<'_>,
    ident: &ast::Ident,
    facts: &ProfileFacts,
) -> Result<ProfileFact, ParseError> {
    match resolve(lower, ident.sym.as_str()) {
        NameHit::Global(_) => Ok(ProfileFact::scalar(Kind::Dynamic)),
        NameHit::Local(id) => facts
            .get(&id)
            .copied()
            .ok_or_else(|| lower.error(ident.span, "P3 profile binding fact missing")),
        NameHit::BuiltinUndefined => Ok(ProfileFact::scalar(Kind::Undefined)),
        NameHit::BuiltinNumeric(i) => {
            lower.needs_ordinary_realm.set(true);
            lower
                .numeric_globals
                .set(lower.numeric_globals.get() | (1 << i));
            Ok(ProfileFact::scalar(Kind::Function))
        }
        NameHit::BuiltinUri(i) => {
            note_original(lower, i + 6);
            Ok(ProfileFact::scalar(Kind::Function))
        }
        NameHit::BuiltinNumber(_) => Ok(ProfileFact::scalar(Kind::Number)),
        NameHit::BuiltinNumberNamespace => {
            lower.needs_ordinary_realm.set(true);
            lower.number_namespace_used.set(true);
            Ok(ProfileFact::scalar(Kind::Function))
        }
        NameHit::BuiltinStringNamespace => {
            lower.needs_ordinary_realm.set(true);
            lower.string_namespace_used.set(true);
            Ok(ProfileFact::scalar(Kind::Function))
        }
        NameHit::BuiltinArrayBuffer => {
            lower.needs_ordinary_realm.set(true);
            lower.array_buffer_used.set(true);
            Ok(ProfileFact::scalar(Kind::Function))
        }
        NameHit::BuiltinTypedArray(element) => {
            lower.needs_ordinary_realm.set(true);
            lower.typed_array_elements.set(lower.typed_array_elements.get() | element.bit());
            Ok(ProfileFact::scalar(Kind::Function))
        }
        NameHit::BuiltinArrayNamespace => {
            // Calls (including admitted `new Array`) and first-class uses
            // can reach dispatch, directly or through aliases and callbacks.
            lower.array_constructor_used.set(true);
            Ok(array_namespace_fact(lower))
        }
        NameHit::BuiltinBoolean => {
            lower.needs_ordinary_realm.set(true);
            lower.boolean_used.set(true);
            Ok(ProfileFact::scalar(Kind::Function))
        }
        NameHit::BuiltinRangeError => {
            lower.needs_ordinary_realm.set(true);
            lower.range_error_used.set(true);
            Ok(ProfileFact::scalar(Kind::Function))
        }
        NameHit::BuiltinOptionalError(error) => {
            note_optional_error(lower, error);
            Ok(ProfileFact::scalar(Kind::Function))
        }
        NameHit::BuiltinMath => {
            lower.needs_ordinary_realm.set(true);
            lower.math_used.set(true);
            Ok(ProfileFact::scalar(Kind::Object))
        }
        NameHit::BuiltinObject | NameHit::BuiltinTypeError => {
            lower.needs_ordinary_realm.set(true);
            Ok(ProfileFact::scalar(Kind::Function))
        }
        NameHit::BuiltinJson => {
            lower.needs_ordinary_realm.set(true);
            lower.json_used.set(true);
            Ok(ProfileFact::scalar(Kind::Object))
        }
        NameHit::Outer(_) => Err(lower.error(
            ident.span,
            &format!(
                "P4 free-variable capture of '{}' is outside this closed subset",
                ident.sym
            ),
        )),
        NameHit::Missing => Err(lower.error(ident.span, &format!("unknown local '{}'", ident.sym))),
    }
}

fn validate_expr(
    lower: &Lowerer<'_>,
    expression: &ast::Expr,
    facts: &mut ProfileFacts,
) -> Result<ProfileFact, ParseError> {
    if matches!(
        expression,
        ast::Expr::Object(_) | ast::Expr::Fn(_) | ast::Expr::This(_)
    ) || matches!(expression,ast::Expr::Bin(binary) if binary.op==ast::BinaryOp::In)
        || matches!(expression,ast::Expr::Unary(unary) if unary.op==ast::UnaryOp::Delete)
    {
        lower.needs_ordinary_realm.set(true);
    }
    // A call demands the ordinary realm only when its callee chain needs an
    // executable consumer. Discarded computed-member callees `obj[key](...)`
    // are emitted capability-only (no dispatch/Reference helper), so the
    // call itself must not widen the realm; every other call keeps its
    // original demand because discarded lowering still walks its callee and
    // actual machinery. Independent producers keep their own demand above.
    let dead_capability_call = lower.in_dead_capability()
        && match expression {
            ast::Expr::Call(call) => match &call.callee {
                ast::Callee::Expr(callee) => match unwrap_parens(callee) {
                    ast::Expr::Member(member) => Lowerer::open_computed_key(member).is_some(),
                    ast::Expr::Ident(ident) => matches!(resolve(lower, ident.sym.as_str()), NameHit::BuiltinOptionalError(_)),
                    _ => false,
                },
                _ => false,
            },
            _ => false,
        };
    if matches!(expression, ast::Expr::Call(_)) && !dead_capability_call {
        lower.needs_ordinary_realm.set(true);
    }
    let kind = match expression {
        ast::Expr::Lit(ast::Lit::Num(_)) => Kind::Number,
        ast::Expr::Lit(ast::Lit::Bool(_)) => Kind::Boolean,
        ast::Expr::Lit(ast::Lit::Null(_)) => Kind::Null,
        ast::Expr::Lit(ast::Lit::Str(_)) => Kind::String,
        ast::Expr::Lit(ast::Lit::Regex(literal)) => {
            return validate_regexp_literal(lower, literal);
        }
        ast::Expr::Paren(paren) => return validate_expr(lower, &paren.expr, facts),
        ast::Expr::Ident(ident) => return ident_fact(lower, ident, facts),
        ast::Expr::Unary(unary) if unary.op == ast::UnaryOp::TypeOf => {
            if let ast::Expr::Ident(ident) = unwrap_parens(&unary.arg)
                && matches!(resolve(lower, ident.sym.as_str()), NameHit::Missing)
            {
                return Ok(ProfileFact::scalar(Kind::String));
            }
            validate_expr(lower, &unary.arg, facts)?;
            Kind::String
        }
        ast::Expr::Unary(unary) if unary.op == ast::UnaryOp::Delete => {
            let ast::Expr::Member(member) = unwrap_parens(&unary.arg) else {
                return Err(lower.error(
                    unary.span,
                    "P7 delete supports ordinary property references",
                ));
            };
            let object = validate_expr(lower, &member.obj, facts)?;
            admit_object_receiver(lower, member.span, object.kind)?;
            if let Some(key_expr) = Lowerer::open_computed_key(member) {
                let key = validate_computed_key(lower, key_expr, facts)?;
                if lower.admit_primitive_member(
                    member,
                    object.kind,
                    key.kind,
                    super::lowerer::primitive_key::Role::Delete,
                )? {
                    if nullish_kind(object.kind) {
                        lower.set_validation_abrupt(true);
                    }
                    Kind::Boolean
                } else {
                    lower.member_key_units(member)?;
                    Kind::Boolean
                }
            } else {
                lower.member_key_units(member)?;
                Kind::Boolean
            }
        }
        ast::Expr::Bin(binary) if binary.op == ast::BinaryOp::In => {
            let key = validate_expr(lower, &binary.left, facts)?;
            let right = validate_expr(lower, &binary.right, facts)?;
            if !lower.admit_primitive_in(binary, key.kind, right.kind)?
                && !matches!(key.kind, Kind::String | Kind::Undefined)
            {
                return Err(lower.error(
                    binary.left.span(),
                    "P7 in keys require a proved String or undefined",
                ));
            }
            Kind::Boolean
        }
        ast::Expr::Unary(unary) if unary.op == ast::UnaryOp::Bang => {
            validate_expr(lower, &unary.arg, facts)?;
            Kind::Boolean
        }
        ast::Expr::Bin(binary)
            if matches!(
                binary.op,
                ast::BinaryOp::LogicalAnd | ast::BinaryOp::LogicalOr
            ) =>
        {
            let left = validate_expr(lower, &binary.left, facts)?;
            if lower.validation_abrupt() {
                capability_expr(lower, &binary.right, facts)?;
                return Ok(ProfileFact::scalar(Kind::Dynamic));
            }
            let mut selected = facts.clone();
            let right = validate_expr(lower, &binary.right, &mut selected)?;
            if lower.validation_abrupt() {
                lower.set_validation_abrupt(false);
                lower.set_validation_possible(snaps_of(&selected));
                return Ok(left);
            }
            for (id, left_fact) in facts.iter_mut() {
                let right_fact = selected
                    .get(id)
                    .copied()
                    .unwrap_or(ProfileFact::scalar(Kind::Dynamic));
                let kind = super::lowerer::expression::join_kind(left_fact.kind, right_fact.kind);
                left_fact.guarded_numeric = left_fact.joined_guard(right_fact, kind);
                left_fact.array_length = super::lowerer::expression::join_length(
                    kind,
                    left_fact.array_length,
                    right_fact.array_length,
                );
                left_fact.kind = kind;
            }
            let kind = super::lowerer::expression::join_kind(left.kind, right.kind);
            return Ok(ProfileFact {
                kind,
                array_length: super::lowerer::expression::join_length(
                    kind,
                    left.array_length,
                    right.array_length,
                ),
                guarded_numeric: left.joined_guard(right, kind),
            });
        }
        ast::Expr::Unary(unary) if unary.op == ast::UnaryOp::Minus => {
            let operand = validate_expr(lower, &unary.arg, facts)?;
            lower.record_primitive_number_site(
                unary.span,
                super::lowerer::primitive_number::Operator::Neg,
                super::lowerer::primitive_number::Role::Unary,
                operand.kind,
            )?;
            Kind::Number
        }
        ast::Expr::Unary(unary) if unary.op == ast::UnaryOp::Void => {
            validate_expr(lower, &unary.arg, facts)?;
            Kind::Undefined
        }
        ast::Expr::Unary(unary) if unary.op == ast::UnaryOp::Tilde => {
            let operand = validate_expr(lower, &unary.arg, facts)?;
            lower.record_primitive_number_site(
                unary.span,
                super::lowerer::primitive_number::Operator::Not,
                super::lowerer::primitive_number::Role::Unary,
                operand.kind,
            )?;
            Kind::Number
        }
        ast::Expr::Bin(binary)
            if super::lowerer::bitwise::Operation::from_op(binary.op).is_some() =>
        {
            let left = validate_expr(lower, &binary.left, facts)?;
            let right = validate_expr(lower, &binary.right, facts)?;
            use super::lowerer::primitive_number::{Operator, Role};
            let op = Operator::binary(binary.op).expect("bitwise conversion");
            lower.record_primitive_number_site(binary.span, op, Role::Left, left.kind)?;
            lower.record_primitive_number_site(binary.span, op, Role::Right, right.kind)?;
            Kind::Number
        }
        ast::Expr::Unary(unary) if unary.op == ast::UnaryOp::Plus => {
            let operand = validate_expr(lower, &unary.arg, facts)?;
            if operand.kind == Kind::String {
                lower.record_string_number_site(unary.span)?;
            } else {
                lower.record_primitive_number_site(
                    unary.span,
                    super::lowerer::primitive_number::Operator::Plus,
                    super::lowerer::primitive_number::Role::Unary,
                    operand.kind,
                )?;
            }
            Kind::Number
        }
        ast::Expr::Unary(unary) => {
            return Err(lower.error(unary.span, "unsupported JavaScript ES5 P0 unary expression"));
        }
        ast::Expr::Bin(binary)
            if super::lowerer::abstract_equality::Op::from_op(binary.op).is_some() =>
        {
            let left = validate_expr(lower, &binary.left, facts)?;
            let right = validate_expr(lower, &binary.right, facts)?;
            lower.record_abstract_equality_pair(binary, left.kind, right.kind)?;
            Kind::Boolean
        }
        ast::Expr::Bin(binary)
            if super::lowerer::relational::Relation::from_op(binary.op).is_some() =>
        {
            use super::lowerer::relational::Mode;
            let left = validate_expr(lower, &binary.left, facts)?;
            let right = validate_expr(lower, &binary.right, facts)?;
            let mode = if left.kind == Kind::String && right.kind == Kind::String {
                Mode::StringString
            } else if left.kind == Kind::Number && right.kind == Kind::Number {
                Mode::Numeric
            } else {
                lower.record_primitive_relational_pair(binary, left.kind, right.kind)?
            };
            lower.record_relational_site(binary, mode)?;
            Kind::Boolean
        }
        ast::Expr::Bin(binary) if binary.op == ast::BinaryOp::Mod => {
            let left = validate_expr(lower, &binary.left, facts)?;
            let right = validate_expr(lower, &binary.right, facts)?;
            use super::lowerer::primitive_number::{Operator, Role};
            lower.record_primitive_number_site(
                binary.span,
                Operator::Remainder,
                Role::Left,
                left.kind,
            )?;
            lower.record_primitive_number_site(
                binary.span,
                Operator::Remainder,
                Role::Right,
                right.kind,
            )?;
            Kind::Number
        }
        ast::Expr::Bin(binary) => {
            if !matches!(
                binary.op,
                ast::BinaryOp::Add
                    | ast::BinaryOp::Sub
                    | ast::BinaryOp::Mul
                    | ast::BinaryOp::Div
                    | ast::BinaryOp::Exp
                    | ast::BinaryOp::EqEqEq
                    | ast::BinaryOp::NotEqEq
            ) {
                return Err(
                    lower.error(binary.span, "unsupported JavaScript ES5 P0 binary operator")
                );
            }
            let left = validate_expr(lower, &binary.left, facts)?;
            let right = validate_expr(lower, &binary.right, facts)?;
            if matches!(binary.op, ast::BinaryOp::EqEqEq | ast::BinaryOp::NotEqEq) {
                Kind::Boolean
            } else if binary.op == ast::BinaryOp::Add
                && ((left.kind == Kind::String
                    && matches!(
                        right.kind,
                        Kind::String | Kind::Number | Kind::Boolean | Kind::Null | Kind::Undefined
                    ))
                    || (matches!(
                        left.kind,
                        Kind::Number | Kind::Boolean | Kind::Null | Kind::Undefined
                    ) && right.kind == Kind::String))
            {
                use super::lowerer::string_concat::Mode;
                let mode = match (left.kind, right.kind) {
                    (Kind::String, Kind::String) => Mode::StringString,
                    (Kind::String, Kind::Number) => Mode::StringNumber,
                    (Kind::Number, Kind::String) => Mode::NumberString,
                    (Kind::String, Kind::Boolean) => Mode::StringBoolean,
                    (Kind::Boolean, Kind::String) => Mode::BooleanString,
                    (Kind::String, Kind::Null) => Mode::StringNull,
                    (Kind::Null, Kind::String) => Mode::NullString,
                    (Kind::String, Kind::Undefined) => Mode::StringUndefined,
                    (Kind::Undefined, Kind::String) => Mode::UndefinedString,
                    _ => unreachable!("exact admitted primitive pair"),
                };
                lower.record_string_concat_site(binary, mode)?;
                Kind::String
            } else if binary.op == ast::BinaryOp::Add
                && (left.kind != Kind::Number || right.kind != Kind::Number)
            {
                lower.record_primitive_addition_site(binary, left.kind, right.kind)?
            } else {
                if let Some(op) = super::lowerer::primitive_number::Operator::arithmetic(binary.op)
                {
                    use super::lowerer::primitive_number::Role;
                    lower.record_primitive_number_site(binary.span, op, Role::Left, left.kind)?;
                    lower.record_primitive_number_site(binary.span, op, Role::Right, right.kind)?;
                } else {
                    admit_profile_numeric_operand(lower, binary.span, left)?;
                    admit_profile_numeric_operand(lower, binary.span, right)?;
                }
                Kind::Number
            }
        }
        ast::Expr::Assign(assign) => {
            if assign.op != ast::AssignOp::Assign {
                return validate_compound(lower, assign, facts);
            }
            match &assign.left {
                ast::AssignTarget::Simple(ast::SimpleAssignTarget::Member(member)) => {
                    if is_arguments_base(member)
                        && lower.function_args.is_some()
                        && implicit_arguments(lower)
                    {
                        validate_arguments_store(lower, member, &assign.right, facts)?;
                        return Ok(ProfileFact::scalar(Kind::Dynamic));
                    }
                    let base = validate_expr(lower, &member.obj, facts)?;
                    if let Some(key_expr) = Lowerer::open_computed_key(member) {
                        let key = validate_computed_key(lower, key_expr, facts)?;
                        if lower.admit_primitive_member(
                            member,
                            base.kind,
                            key.kind,
                            super::lowerer::primitive_key::Role::Put,
                        )? {
                            if nullish_kind(base.kind) || lower.validation_abrupt() {
                                lower.set_validation_abrupt(true);
                                capability_expr(lower, &assign.right, facts)?;
                                return Ok(ProfileFact::scalar(Kind::Dynamic));
                            }
                            return validate_expr(lower, &assign.right, facts);
                        }
                    }
                    admit_member_write(lower, member, base.kind, base.array_length)?;
                    return validate_expr(lower, &assign.right, facts);
                }
                ast::AssignTarget::Simple(ast::SimpleAssignTarget::Ident(binding)) => {
                    ident_fact(lower, &binding.id, facts)?;
                    let rhs = validate_expr(lower, &assign.right, facts)?;
                    if let NameHit::Local(id) = resolve(lower, binding.id.sym.as_str()) {
                        facts.insert(id, rhs);
                    }
                    return Ok(rhs);
                }
                _ => {
                    return Err(lower.error(
                        assign.span,
                        "unsupported JavaScript ES5 P0 primitive expression",
                    ));
                }
            }
        }
        ast::Expr::Array(literal) => {
            let mut all_numbers = true;
            for element in &literal.elems {
                let Some(element) = element else {
                    return Err(lower.error(literal.span, "P0 dense arrays reject holes"));
                };
                if element.spread.is_some() {
                    return Err(
                        lower.error(element.span(), "P0 dense arrays reject spread elements")
                    );
                }
                let fact = validate_expr(lower, &element.expr, facts)?;
                admit_array_element(lower, element.span(), fact.kind, None)?;
                all_numbers &= fact.kind == Kind::Number;
            }
            let repr = if all_numbers {
                super::lowerer::ArrayRepr::Packed
            } else {
                super::lowerer::ArrayRepr::Tagged
            };
            return Ok(ProfileFact {
                kind: Kind::Array,
                array_length: Some(super::lowerer::ArrayShape {
                    length: literal.elems.len(),
                    repr,
                    number_only_below: literal.elems.len(),
                }),
                guarded_numeric: false,
            });
        }
        ast::Expr::Object(literal) => {
            for (_, expression, span, _) in lower.object_literal_entries(literal)? {
                let value = validate_expr(lower, &expression, facts)?;
                admit_object_value(lower, span, value.kind)?;
            }
            Kind::Object
        }
        ast::Expr::Member(member) => {
            // Direct `arguments` observation inside any source activation.
            // The implicit binding is the invocation vector itself: reads are
            // `.length` and numeric element indexes (out-of-range and
            // non-integral keys are genuine Undefined). Writes only address a
            // canonical numeric index and never alias a formal.
            if is_arguments_base(member)
                && lower.function_args.is_some()
                && implicit_arguments(lower)
            {
                return validate_arguments_read(lower, member, facts);
            }
            let base = validate_member_base(lower, member, facts)?;
            // A dense-Array element or `length` read stays inside the dense
            // slice. Every other name is an ordinary prototype-chain
            // Reference, so the realm helpers are required.
            let dense_only = base.kind == Kind::Array
                && match &member.prop {
                    ast::MemberProp::Ident(id) => id.sym == *"length",
                    ast::MemberProp::Computed(property) => {
                        Lowerer::member_index_literal(&property.expr).is_some()
                            || !matches!(
                                unwrap_parens(&property.expr),
                                ast::Expr::Lit(ast::Lit::Str(_))
                            )
                    }
                    _ => false,
                };
            if let Some(key_expr) = Lowerer::open_computed_key(member) {
                let key = validate_computed_key(lower, key_expr, facts)?;
                if base.kind == Kind::Object
                    && key.kind == Kind::Number
                    && Lowerer::number_key_candidate(member)
                {
                    // The ordinary Object Number peer keeps its independent
                    // realm demand even when the surrounding text is dead.
                    if !dense_only {
                        lower.needs_ordinary_realm.set(true);
                    }
                    lower.record_number_key_site(member)?;
                    return Ok(ProfileFact::guarded_dynamic());
                }
                if lower.admit_primitive_member(
                    member,
                    base.kind,
                    key.kind,
                    super::lowerer::primitive_key::Role::Get,
                )? {
                    // A live admission publishes the prepared Get proof that
                    // carries the realm demand itself. The dead-capability
                    // admission taken while the validation walk probes text
                    // behind an abrupt statement records no proof, and
                    // discarded emission follows the existing capability-only
                    // Get route, so charging this independent cell would be a
                    // dead fee. Base/key producers and genuine refusals above
                    // keep their own demand.
                    if !dense_only && !lower.in_dead_capability() {
                        lower.needs_ordinary_realm.set(true);
                    }
                    if nullish_kind(base.kind) {
                        lower.set_validation_abrupt(true);
                    }
                    return Ok(ProfileFact::guarded_dynamic());
                }
            }
            if !dense_only {
                lower.needs_ordinary_realm.set(true);
            }
            return admit_member_read(lower, member, base.kind, base.array_length).map(|kind| {
                ProfileFact {
                    kind,
                    array_length: None,
                    guarded_numeric: kind == Kind::Dynamic,
                }
            });
        }
        ast::Expr::Call(call) => {
            admit_call_callee(lower, call)?;
            if let ast::Callee::Expr(callee) = &call.callee
                && let ast::Expr::Ident(ident) = unwrap_parens(callee)
                && matches!(
                    resolve(lower, ident.sym.as_str()),
                    NameHit::BuiltinTypeError | NameHit::BuiltinRangeError
                )
                && !call.args.is_empty()
            {
                return Err(lower.error(call.span, "unsupported JavaScript ES5 P0 primitive expression"));
            }
            let ast::Callee::Expr(callee) = &call.callee else {
                unreachable!()
            };
            // `Array.isArray(arguments)` passes the activation's implicit
            // arguments binding as a real actual: the pinned engine
            // materializes it as an Array, and this frontend's invocation
            // vector is an Array<JsValue>. The call stays a real Get+Call, so
            // a rewritten `Array.isArray` is observed; only the bare actual is
            // exempt from the unqualified-`arguments` refusal.
            let arguments_actual = call.args.len() == 1
                && is_arguments_ident(&call.args[0].expr)
                && lower.function_args.is_some()
                && implicit_arguments(lower)
                && matches!(
                    unwrap_parens(callee),
                    ast::Expr::Member(member) if is_array_is_array_member(lower, member)
                );
            match unwrap_parens(callee) {
                ast::Expr::Ident(ident) => {
                    let fact = ident_fact(lower, ident, facts)?;
                    admit_callable_kind(lower, ident.span, fact.kind)?;
                }
                ast::Expr::Member(member) => {
                    let base = validate_member_base(lower, member, facts)?;
                    if !named_primitive_read(lower, member, base.kind)? {
                        admit_object_receiver(lower, member.span, base.kind)?;
                    }
                    if let Some(key_expr) = Lowerer::open_computed_key(member) {
                        let key = validate_computed_key(lower, key_expr, facts)?;
                        if lower.admit_primitive_member(
                            member,
                            base.kind,
                            key.kind,
                            super::lowerer::primitive_key::Role::Call,
                        )? {
                            if nullish_kind(base.kind) {
                                lower.set_validation_abrupt(true);
                            }
                            admit_callable_kind(lower, member.span, Kind::Dynamic)?;
                        } else {
                            let callee_kind = admit_object_member_read(lower, member, base.kind)?;
                            admit_callable_kind(lower, member.span, callee_kind)?;
                        }
                    } else {
                        let callee_kind = admit_object_member_read(lower, member, base.kind)?;
                        admit_callable_kind(lower, member.span, callee_kind)?;
                    }
                }
                other => {
                    // Value callees use the same profile walk as emission.
                    // Parentheses do not create a Reference; Ident/Member above
                    // remain the only receiver-bearing shapes.
                    let fact = validate_expr(lower, other, facts)?;
                    admit_callable_kind(lower, other.span(), fact.kind)?;
                }
            }
            for arg in &call.args {
                if arguments_actual {
                    continue;
                }
                if lower.validation_abrupt() {
                    capability_expr(lower, &arg.expr, facts)?;
                } else {
                    validate_expr(lower, &arg.expr, facts)?;
                }
            }
            // A call result is Dynamic, not a false Number. Emission still
            // checks the number tag; only this provenance bit admits arithmetic.
            return Ok(ProfileFact::guarded_dynamic());
        }
        ast::Expr::Cond(branch) => {
            validate_expr(lower, &branch.test, facts)?;
            if lower.validation_abrupt() {
                capability_expr(lower, &branch.cons, facts)?;
                capability_expr(lower, &branch.alt, facts)?;
                return Ok(ProfileFact::scalar(Kind::Dynamic));
            }
            let mut consequent = facts.clone();
            let mut alternate = facts.clone();
            let cons = validate_expr(lower, &branch.cons, &mut consequent)?;
            let cons_abrupt = lower.validation_abrupt();
            lower.set_validation_abrupt(false);
            let cons_conditional = lower.take_validation_conditional();
            let _cons_snaps = lower.take_validation_throw();
            let alt = validate_expr(lower, &branch.alt, &mut alternate)?;
            let alt_abrupt = lower.validation_abrupt();
            lower.set_validation_abrupt(false);
            let alt_conditional = lower.take_validation_conditional();
            let _alt_snaps = lower.take_validation_throw();
            if cons_abrupt && alt_abrupt {
                lower.set_validation_abrupt(true);
                return Ok(ProfileFact::scalar(Kind::Dynamic));
            }
            if cons_abrupt {
                *facts = alternate;
                lower.set_validation_possible(snaps_of(&consequent));
                return Ok(alt);
            }
            if alt_abrupt {
                *facts = consequent;
                lower.set_validation_possible(snaps_of(&alternate));
                return Ok(cons);
            }
            *facts = join_profile(&consequent, &alternate);
            if cons_conditional || alt_conditional {
                lower.set_validation_possible(snaps_of(facts));
            }
            let kind = super::lowerer::expression::join_kind(cons.kind, alt.kind);
            return Ok(ProfileFact {
                kind,
                array_length: super::lowerer::expression::join_length(
                    kind,
                    cons.array_length,
                    alt.array_length,
                ),
                guarded_numeric: cons.joined_guard(alt, kind),
            });
        }
        ast::Expr::Seq(sequence) => {
            let Some((first, rest)) = sequence.exprs.split_first() else {
                return Err(lower.error(
                    sequence.span,
                    "unsupported JavaScript ES5 P0 primitive expression",
                ));
            };
            let mut fact = validate_expr(lower, first, facts)?;
            for expr in rest {
                if lower.validation_abrupt() {
                    capability_expr(lower, expr, facts)?;
                    break;
                }
                fact = validate_expr(lower, expr, facts)?;
            }
            return Ok(fact);
        }
        ast::Expr::New(construct) => return validate_new_expr(lower, construct, facts),
        ast::Expr::Fn(_) => Kind::Function,
        ast::Expr::This(this) => {
            if lower.graph.scope(lower.active_scope).kind != super::scope::ScopeKind::Function {
                return Err(lower.error(
                    this.span,
                    "P0 `this` is admitted only inside closed strict source functions",
                ));
            }
            Kind::Object
        }
        ast::Expr::Update(update) => return validate_update(lower, update, facts),
        _ => {
            return Err(lower.error(
                expression.span(),
                "unsupported JavaScript ES5 P0 primitive expression",
            ));
        }
    };
    Ok(ProfileFact::scalar(kind))
}

fn validate_compound(
    lower: &Lowerer<'_>,
    assign: &ast::AssignExpr,
    facts: &mut ProfileFacts,
) -> Result<ProfileFact, ParseError> {
    let op = super::lowerer::primitive_assignment::MutationOp::from_assign(assign.op).ok_or_else(
        || {
            lower.error(
                assign.span,
                "unsupported JavaScript ES5 P0 primitive expression",
            )
        },
    )?;
    let (owner, old, id) = match &assign.left {
        ast::AssignTarget::Simple(ast::SimpleAssignTarget::Ident(binding)) => mutation_place(
            lower,
            &ast::Expr::Ident(binding.id.clone()),
            facts,
            super::lowerer::primitive_key::Role::Compound,
        )?,
        ast::AssignTarget::Simple(ast::SimpleAssignTarget::Member(member)) => mutation_place(
            lower,
            &ast::Expr::Member(member.clone()),
            facts,
            super::lowerer::primitive_key::Role::Compound,
        )?,
        ast::AssignTarget::Simple(ast::SimpleAssignTarget::Paren(paren)) => mutation_place(
            lower,
            &paren.expr,
            facts,
            super::lowerer::primitive_key::Role::Compound,
        )?,
        _ => {
            return Err(lower.error(
                assign.span,
                "unsupported JavaScript ES5 P0 primitive expression",
            ));
        }
    };
    if lower.validation_abrupt() {
        capability_expr(lower, &assign.right, facts)?;
        // The compute is unreachable. Seal a proof so emission can find the
        // site, then stop before the RHS store is published.
        lower.note_mutation(
            assign.span,
            op,
            false,
            owner,
            Kind::Number,
            Some(Kind::Number),
        )?;
        return Ok(ProfileFact::scalar(Kind::Dynamic));
    }
    let rhs = validate_expr(lower, &assign.right, facts)?;
    let result = lower.note_mutation(assign.span, op, false, owner, old, Some(rhs.kind))?;
    publish_mutation_fact(lower, assign.span, owner, id, result, facts)?;
    Ok(mutation_fact(result))
}

fn validate_update(
    lower: &Lowerer<'_>,
    update: &ast::UpdateExpr,
    facts: &mut ProfileFacts,
) -> Result<ProfileFact, ParseError> {
    let op = match update.op {
        ast::UpdateOp::PlusPlus => super::lowerer::primitive_assignment::MutationOp::Inc,
        ast::UpdateOp::MinusMinus => super::lowerer::primitive_assignment::MutationOp::Dec,
    };
    let (owner, old, id) = mutation_place(
        lower,
        &update.arg,
        facts,
        super::lowerer::primitive_key::Role::Update,
    )?;
    if lower.validation_abrupt() {
        lower.note_mutation(update.span, op, update.prefix, owner, Kind::Number, None)?;
        return Ok(ProfileFact::scalar(Kind::Dynamic));
    }
    let result = lower.note_mutation(update.span, op, update.prefix, owner, old, None)?;
    publish_mutation_fact(lower, update.span, owner, id, result, facts)?;
    Ok(mutation_fact(result))
}

/// The admitted regexp literals, `/a/g` and `/a/`.
///
/// Any other pattern, any other flag set, and the `RegExp` constructor keep
/// the existing refusal: there is no general regexp literal and no general
/// regexp object surface (`.source`, `.flags`, `.exec`, `lastIndex`). `/a/g`
/// stays the earlier zero-own-row value with no method. `/a/` is the admitted
/// public `test` slice: its one own data row is the `test` method the guest
/// matcher kernel backs. Both admitted values are ordinary realm objects, so
/// the object runtime is required exactly as for a plain object literal.
fn validate_regexp_literal(
    lower: &Lowerer<'_>,
    literal: &ast::Regex,
) -> Result<ProfileFact, ParseError> {
    if literal.exp.as_str() != "a"
        || !(literal.flags.as_str() == "g" || literal.flags.as_str().is_empty())
    {
        return Err(lower.error(
            literal.span,
            "unsupported JavaScript ES5 P0 primitive expression",
        ));
    }
    // The admitted value is an ordinary realm object, so the object runtime
    // is required exactly as for a plain object literal.
    lower.needs_ordinary_realm.set(true);
    Ok(ProfileFact::scalar(Kind::Object))
}

fn array_constructor_ident(callee: &ast::Expr) -> Option<&ast::Ident> {
    match unwrap_parens(callee) {
        ast::Expr::Ident(ident) if ident.sym == *"Array" => Some(ident),
        _ => None,
    }
}

/// Builtin spellings whose unshadowed `new` form lowers without the guest
/// throw channel. `Number`/`String`/`Boolean` are deliberately absent: their
/// construction is the one upstream runtime TypeError shape.
fn new_spelling_is_builtin_construct(name: &str) -> bool {
    match name {
        "Array" | "ArrayBuffer" | "Object" | "Error" | "TypeError" | "RangeError" | "Date" => true,
        name => OptionalError::named(name).is_some() || Element::named(name).is_some(),
    }
}

/// The admitted `new Array` form uses the existing call argument and
/// completion path after resolution selects the unshadowed intrinsic.
pub(super) fn array_constructor_call(
    lower: &Lowerer<'_>,
    construct: &ast::NewExpr,
    alias: Option<ConstructorAlias>,
) -> Option<ast::CallExpr> {
    if !matches!(alias, Some(ConstructorAlias::Array)) {
        let ident = array_constructor_ident(&construct.callee)?;
        if !matches!(resolve(lower, ident.sym.as_str()), NameHit::BuiltinArrayNamespace) {
            return None;
        }
    }
    Some(ast::CallExpr {
        span: construct.span,
        ctxt: construct.ctxt,
        callee: ast::Callee::Expr(construct.callee.clone()),
        args: construct.args.clone().unwrap_or_default(),
        type_args: construct.type_args.clone(),
    })
}

pub(super) fn array_buffer_constructor_call(
    lower: &Lowerer<'_>,
    construct: &ast::NewExpr,
    alias: Option<ConstructorAlias>,
) -> Option<ast::CallExpr> {
    if !matches!(alias, Some(ConstructorAlias::ArrayBuffer)) {
        let ast::Expr::Ident(ident) = unwrap_parens(&construct.callee) else { return None };
        if !matches!(resolve(lower, ident.sym.as_str()), NameHit::BuiltinArrayBuffer) {
            return None;
        }
    }
    Some(ast::CallExpr {
        span: construct.span,
        ctxt: construct.ctxt,
        callee: ast::Callee::Expr(construct.callee.clone()),
        args: construct.args.clone().unwrap_or_default(),
        type_args: construct.type_args.clone(),
    })
}

pub(super) fn typed_array_constructor_call(
    lower: &Lowerer<'_>,
    construct: &ast::NewExpr,
    alias: Option<ConstructorAlias>,
) -> Option<(ast::CallExpr, Element)> {
    let element = if let Some(ConstructorAlias::TypedArray(element)) = alias {
        element
    } else {
        let ast::Expr::Ident(ident) = unwrap_parens(&construct.callee) else { return None };
        let NameHit::BuiltinTypedArray(element) = resolve(lower, ident.sym.as_str()) else {
            return None;
        };
        element
    };
    Some((ast::CallExpr {
        span: construct.span,
        ctxt: construct.ctxt,
        callee: ast::Callee::Expr(construct.callee.clone()),
        args: construct.args.clone().unwrap_or_default(),
        type_args: construct.type_args.clone(),
    }, element))
}

/// The alias of one identifier at a `new` site, in the same scope the name
/// resolution below uses. Only a `var X = <builtin>` binding that is never
/// written again qualifies.
pub(super) fn construct_alias(
    lower: &Lowerer<'_>,
    ident: &ast::Ident,
    use_lo: u32,
) -> Option<ConstructorAlias> {
    let scope = lower
        .catch_overlay
        .get()
        .unwrap_or(lower.active_scope);
    super::scope::constructor_alias(&lower.graph, scope, ident.sym.as_str(), use_lo)
}

fn validate_new_expr(
    lower: &Lowerer<'_>,
    construct: &ast::NewExpr,
    facts: &mut ProfileFacts,
) -> Result<ProfileFact, ParseError> {
    let alias = match unwrap_parens(&construct.callee) {
        ast::Expr::Ident(ident) => construct_alias(lower, ident, construct.span.lo.0),
        _ => None,
    };
    if let Some(call) = array_constructor_call(lower, construct, alias)
        .or_else(|| array_buffer_constructor_call(lower, construct, alias))
        .or_else(|| typed_array_constructor_call(lower, construct, alias).map(|(call, _)| call))
    {
        return validate_expr(lower, &ast::Expr::Call(call), facts);
    }
    if matches!(alias, Some(ConstructorAlias::Object))
        || matches!(
            unwrap_parens(&construct.callee),
            ast::Expr::Ident(callee) if callee.sym == *"Object"
        ) && matches!(resolve(lower, "Object"), NameHit::BuiltinObject)
    {
        return validate_new_object(lower, construct);
    }
    if let ast::Expr::Ident(callee) = unwrap_parens(&construct.callee)
        && callee.sym == *"Error"
        && matches!(resolve(lower, "Error"), NameHit::Missing)
    {
        return validate_new_error(lower, construct);
    }
    let native_error = match alias {
        Some(ConstructorAlias::TypeError) => Some("TypeError"),
        Some(ConstructorAlias::RangeError) => Some("RangeError"),
        _ => match unwrap_parens(&construct.callee) {
            ast::Expr::Ident(callee) => match resolve(lower, callee.sym.as_str()) {
                NameHit::BuiltinTypeError => Some("TypeError"),
                NameHit::BuiltinRangeError => Some("RangeError"),
                _ => None,
            },
            _ => None,
        },
    };
    if let Some(name) = native_error {
        let Some(args) = construct.args.as_ref() else {
            return Err(lower.error(construct.span, "unsupported JavaScript ES5 P0 primitive expression"));
        };
        if !args.is_empty() {
            return Err(lower.error(construct.span, "unsupported JavaScript ES5 P0 primitive expression"));
        }
        if name == "RangeError" {
            lower.range_error_used.set(true);
        }
        lower.needs_ordinary_realm.set(true);
        return Ok(ProfileFact::scalar(Kind::Object));
    }
    if let ast::Expr::Ident(callee) = unwrap_parens(&construct.callee)
        && matches!(
            resolve(lower, callee.sym.as_str()),
            NameHit::BuiltinOptionalError(_)
        )
    {
        let NameHit::BuiltinOptionalError(error) = resolve(lower, callee.sym.as_str()) else {
            unreachable!()
        };
        admit_optional_error_args(lower, construct.args.as_deref().unwrap_or_default(), construct.span)?;
        note_optional_error(lower, error);
        return Ok(ProfileFact::scalar(Kind::Object));
    }
    // Number/String/Boolean are the only constructors whose upstream `new`
    // form is a runtime TypeError; every other readable builtin and every
    // alias keeps or gains a real construction route.
    let unsupported = match alias {
        Some(ConstructorAlias::Number | ConstructorAlias::String | ConstructorAlias::Boolean) => true,
        _ => matches!(
            unwrap_parens(&construct.callee),
            ast::Expr::Ident(callee)
                if matches!(resolve(lower, callee.sym.as_str()),
                    NameHit::BuiltinNumberNamespace | NameHit::BuiltinStringNamespace | NameHit::BuiltinBoolean)
        ),
    };
    if unsupported {
        lower.needs_ordinary_realm.set(true);
        return Ok(ProfileFact::scalar(Kind::Object));
    }
    // `new Date(0)` keeps its one admitted epoch construction; a bound local
    // Date spelling is an ordinary source construction instead.
    let builtin_date = matches!(
        unwrap_parens(&construct.callee),
        ast::Expr::Ident(ident) if ident.sym == *"Date" && matches!(resolve(lower, "Date"), NameHit::Missing)
    );
    if !builtin_date && let Ok(fact) = validate_source_construct(lower, construct, facts) {
        return Ok(fact);
    }
    validate_new_date(lower, construct)
}

/// The general `new F(...)` slice: the callee must be a statically known
/// function value, or a bare formal parameter of the activation under
/// validation (the private helper's runtime source-function check then
/// decides). Every actual must be a plain (unspread) expression.
///
/// The returned fact is an ordinary object. The runtime half additionally
/// requires the callee node to be a *source* function (`KIND_FUNCTION`, code
/// `0..=255`). A `var X = <builtin>` alias is resolved by binding first and
/// takes the corresponding builtin construction route instead of this
/// helper, so it no longer reaches the guest `TypeError` limit.
fn validate_source_construct(
    lower: &Lowerer<'_>,
    construct: &ast::NewExpr,
    facts: &mut ProfileFacts,
) -> Result<ProfileFact, ParseError> {
    let refused = || {
        lower.error(
            construct.span,
            "unsupported JavaScript ES5 P0 primitive expression",
        )
    };
    if let Some(args) = &construct.args {
        for arg in args {
            if arg.spread.is_some() {
                return Err(refused());
            }
        }
    }
    let fact = validate_expr(lower, &construct.callee, facts)?;
    // A formal parameter can hold a source-function value at the call site.
    // The private construct helper performs the runtime KIND_FUNCTION/code
    // check and raises the guest TypeError for any other value, so this
    // dynamic slice is admitted. Only a bare identifier bound to a formal of
    // the activation under validation qualifies: member callees, call
    // results and dynamic locals keep their compile-time refusal.
    //
    // The resolution is the one `ident_fact` used for the callee fact, not a
    // second lookup from `active_scope`: a catch parameter that shadows the
    // formal resolves to the catch BindingId, and the lowering reads that same
    // binding, so admission must judge it. Re-resolving from the activation
    // would admit through the outer formal while the emitted code reads the
    // catch value.
    let dynamic_formal = fact.kind == Kind::Dynamic
        && matches!(
            unwrap_parens(&construct.callee),
            ast::Expr::Ident(ident)
                if matches!(
                    resolve(lower, ident.sym.as_str()),
                    NameHit::Local(id) if lower
                        .graph
                        .scope(lower.active_scope)
                        .formals
                        .iter()
                        .any(|(formal, _, _)| *formal == id)
                )
        );
    // A provably non-callable object callee (an object literal, `new Object()`,
    // a Date instance) is admitted into the same runtime rejection: the
    // private gate runs after every actual and before `Get(F, "prototype")`,
    // so `new ({}) (k=1)` still throws the pinned guest TypeError with the
    // actual already evaluated. Member reads and call results stay Dynamic
    // and keep their compile-time refusal.
    let proven_non_callable = fact.kind == Kind::Object;
    if fact.kind != Kind::Function && !dynamic_formal && !proven_non_callable {
        return Err(refused());
    }
    lower.function_prototype_used.set(true);
    lower.construct_used.set(true);
    lower.needs_ordinary_realm.set(true);
    Ok(ProfileFact::scalar(Kind::Object))
}

fn note_optional_error(lower: &Lowerer<'_>, error: OptionalError) {
    if !lower.in_dead_capability() && !lower.in_primitive_key_probe() {
        lower.needs_ordinary_realm.set(true);
        lower.optional_errors_used.set(lower.optional_errors_used.get() | error.bit());
    }
}

/// The optional errors admit the message-free slice: no actuals, or the
/// literal compatibility witness "x", which does not create an own row.
pub(super) fn admit_optional_error_args(
    lower: &Lowerer<'_>,
    args: &[ast::ExprOrSpread],
    span: swc_common::Span,
) -> Result<(), ParseError> {
    match args {
        [] => Ok(()),
        [arg] if arg.spread.is_none()
            && matches!(unwrap_parens(&arg.expr), ast::Expr::Lit(ast::Lit::Str(s)) if s.value == *"x") => Ok(()),
        _ => Err(lower.error(span, "unsupported JavaScript ES5 P0 primitive expression")),
    }
}

/// The one admitted empty-object construction, `new Object()`.
fn validate_new_object(
    lower: &Lowerer<'_>,
    construct: &ast::NewExpr,
) -> Result<ProfileFact, ParseError> {
    let refused = || {
        lower.error(
            construct.span,
            "unsupported JavaScript ES5 P0 primitive expression",
        )
    };
    let Some(args) = construct.args.as_ref() else {
        return Err(refused());
    };
    if !args.is_empty() {
        return Err(refused());
    }
    // The constructed value is an ordinary realm object, so the object runtime
    // is required exactly as for a plain object literal.
    lower.needs_ordinary_realm.set(true);
    Ok(ProfileFact::scalar(Kind::Object))
}

/// The one admitted construction, `new Error("boom")`.
///
/// `Error` is not a global binding, so a shadowed `Error`, any other callee,
/// any other arity, any spread and any other message keep the existing
/// refusal: there is no general `new` expression and no constructor object.
/// The admitted value is a rooted managed Error object whose own `message`
/// row is the String `boom`; no `Error` global function is installed on this
/// slice.
fn validate_new_error(
    lower: &Lowerer<'_>,
    construct: &ast::NewExpr,
) -> Result<ProfileFact, ParseError> {
    let refused = || {
        lower.error(
            construct.span,
            "unsupported JavaScript ES5 P0 primitive expression",
        )
    };
    let ast::Expr::Ident(callee) = unwrap_parens(&construct.callee) else {
        return Err(refused());
    };
    if callee.sym != *"Error" || !matches!(resolve(lower, callee.sym.as_str()), NameHit::Missing)
    {
        return Err(refused());
    }
    let Some(args) = construct.args.as_ref() else {
        return Err(refused());
    };
    let [argument] = args.as_slice() else {
        return Err(refused());
    };
    if argument.spread.is_some() {
        return Err(refused());
    }
    let ast::Expr::Lit(ast::Lit::Str(literal)) = unwrap_parens(&argument.expr) else {
        return Err(refused());
    };
    if literal.value != *"boom" {
        return Err(refused());
    }
    // The constructed value is an ordinary realm object, so the object runtime
    // is required exactly as for a plain object literal.
    lower.needs_ordinary_realm.set(true);
    Ok(ProfileFact::scalar(Kind::Object))
}

/// The one admitted construction, `new Date(0)`.
///
/// `Date` is not a global binding, so a shadowed `Date`, any other callee, any
/// other arity and any other argument keep the existing refusal: there is no
/// general `new` expression and no constructor object. The admitted value is
/// an ordinary object whose epoch millisecond value is the Number 0; no live
/// clock, `Date.now`, `Date.parse` or `toString` is admitted.
fn validate_new_date(
    lower: &Lowerer<'_>,
    construct: &ast::NewExpr,
) -> Result<ProfileFact, ParseError> {
    let refused = || {
        lower.error(
            construct.span,
            "unsupported JavaScript ES5 P0 primitive expression",
        )
    };
    let ast::Expr::Ident(callee) = unwrap_parens(&construct.callee) else {
        return Err(refused());
    };
    if callee.sym != *"Date" || !matches!(resolve(lower, callee.sym.as_str()), NameHit::Missing) {
        return Err(refused());
    }
    let Some(args) = construct.args.as_ref() else {
        return Err(refused());
    };
    let [argument] = args.as_slice() else {
        return Err(refused());
    };
    if argument.spread.is_some() {
        return Err(refused());
    }
    let ast::Expr::Lit(ast::Lit::Num(number)) = unwrap_parens(&argument.expr) else {
        return Err(refused());
    };
    if number.value != 0.0 {
        return Err(refused());
    }
    // The constructed value is an ordinary realm object, so the object runtime
    // is required exactly as for a plain object literal.
    lower.needs_ordinary_realm.set(true);
    Ok(ProfileFact::scalar(Kind::Object))
}

fn mutation_fact(kind: Kind) -> ProfileFact {
    ProfileFact {
        kind,
        array_length: None,
        guarded_numeric: kind == Kind::Dynamic,
    }
}

fn publish_mutation_fact(
    lower: &Lowerer<'_>,
    span: swc_common::Span,
    owner: super::lowerer::primitive_assignment::Owner,
    id: Option<super::scope::BindingId>,
    result: Kind,
    facts: &mut ProfileFacts,
) -> Result<(), ParseError> {
    if owner == super::lowerer::primitive_assignment::Owner::ArrayItem && result != Kind::Number {
        return Err(lower.error(span, "P0 numeric arrays store only numbers"));
    }
    if let Some(id) = id {
        facts.insert(id, mutation_fact(result));
    }
    Ok(())
}

fn mutation_place(
    lower: &Lowerer<'_>,
    expression: &ast::Expr,
    facts: &mut ProfileFacts,
    role: super::lowerer::primitive_key::Role,
) -> Result<
    (
        super::lowerer::primitive_assignment::Owner,
        Kind,
        Option<super::scope::BindingId>,
    ),
    ParseError,
> {
    match unwrap_parens(expression) {
        ast::Expr::Ident(ident) => {
            let fact = ident_fact(lower, ident, facts)?;
            match resolve(lower, ident.sym.as_str()) {
                NameHit::Global(_) => Ok((
                    super::lowerer::primitive_assignment::Owner::Global,
                    fact.kind,
                    None,
                )),
                NameHit::Local(id) => Ok((lower.binding_owner(id), fact.kind, Some(id))),
                NameHit::Outer(_) => Err(lower.error(
                    ident.span,
                    &format!(
                        "P4 free-variable capture of '{}' is outside this closed subset",
                        ident.sym
                    ),
                )),
                _ => Err(lower.error(ident.span, &format!("unknown local '{}'", ident.sym))),
            }
        }
        ast::Expr::Member(member) => {
            let base = validate_expr(lower, &member.obj, facts)?;
            if let Some(key_expr) = Lowerer::open_computed_key(member) {
                let key = validate_computed_key(lower, key_expr, facts)?;
                if lower.admit_primitive_member(member, base.kind, key.kind, role)? {
                    if nullish_kind(base.kind) {
                        lower.set_validation_abrupt(true);
                    }
                    return Ok((
                        super::lowerer::primitive_assignment::Owner::Property,
                        Kind::Dynamic,
                        None,
                    ));
                }
            } else if let ast::MemberProp::Computed(property) = &member.prop {
                validate_expr(lower, &property.expr, facts)?;
            }
            admit_member_write(lower, member, base.kind, base.array_length)?;
            if base.kind == Kind::Array && matches!(member.obj.as_ref(), ast::Expr::Ident(_)) {
                // A non-literal computed numeric key on a dense Array takes the
                // runtime-checked element place, whose old value is an unproved
                // JsValue: seal the left mode as Dynamic. A literal index on a
                // packed numeric array keeps the Number cell. A tagged or
                // length-unknown literal index is also an unproved JsValue, so
                // the existing unary ToNumber parser is requested instead of a
                // private string branch.
                let packed_literal = !super::lowerer::primitive_assignment::computed_array_candidate(
                    member,
                ) && base
                    .array_length
                    .is_some_and(|shape| shape.repr == super::lowerer::ArrayRepr::Packed);
                let old = if packed_literal {
                    Kind::Number
                } else {
                    Kind::Dynamic
                };
                Ok((
                    super::lowerer::primitive_assignment::Owner::ArrayItem,
                    old,
                    None,
                ))
            } else {
                Ok((
                    super::lowerer::primitive_assignment::Owner::Property,
                    Kind::Dynamic,
                    None,
                ))
            }
        }
        other => Err(lower.error(
            other.span(),
            "unsupported JavaScript ES5 P0 primitive expression",
        )),
    }
}

/// Shared pure call admission: arity, spreads, type arguments, and
/// Super/Import. Expression shape is validated by the caller. No slot or
/// root mutation.
pub(super) fn admit_call_callee(
    lower: &Lowerer<'_>,
    call: &ast::CallExpr,
) -> Result<(), ParseError> {
    if call.args.len() > super::scope::MAX_ARGUMENTS {
        return Err(lower.error(
            call.span,
            "P4 source calls exceed the 64 actual argument profile limit",
        ));
    }
    if call.type_args.is_some() {
        return Err(lower.error(call.span, "P0 source functions reject type arguments"));
    }
    for arg in &call.args {
        if arg.spread.is_some() {
            return Err(lower.error(arg.span(), "P0 source functions reject spread arguments"));
        }
    }
    let ast::Callee::Expr(callee) = &call.callee else {
        return Err(lower.error(
            call.span,
            "P0 calls admit only identifier and member callees",
        ));
    };
    if let ast::Expr::Ident(ident) = unwrap_parens(callee)
        && matches!(resolve(lower, ident.sym.as_str()), NameHit::BuiltinOptionalError(_))
    {
        admit_optional_error_args(lower, &call.args, call.span)?;
    }
    Ok(())
}

pub(super) fn admit_callable_kind(
    lower: &Lowerer<'_>,
    span: swc_common::Span,
    kind: Kind,
) -> Result<(), ParseError> {
    if !matches!(kind, Kind::Function | Kind::Object | Kind::Dynamic) {
        return Err(lower.error(span, "P0 bare calls require a function binding"));
    }
    Ok(())
}

pub(super) fn admit_object_receiver(
    lower: &Lowerer<'_>,
    span: swc_common::Span,
    kind: Kind,
) -> Result<(), ParseError> {
    if !matches!(
        kind,
        Kind::Object | Kind::Function | Kind::Array | Kind::Null | Kind::Undefined | Kind::Dynamic
    ) {
        return Err(lower.error(
            span,
            "P0 plain-object member reads require an object receiver",
        ));
    }
    Ok(())
}

pub(super) fn admit_member_read(
    lower: &Lowerer<'_>,
    member: &ast::MemberExpr,
    kind: Kind,
    shape: Option<super::lowerer::ArrayShape>,
) -> Result<Kind, ParseError> {
    if kind == Kind::Array && matches!(member.obj.as_ref(), ast::Expr::Ident(_)) {
        let length_units: Vec<u16> = "length".encode_utf16().collect();
        let is_length = match &member.prop {
            ast::MemberProp::Ident(id) => id.sym == *"length",
            ast::MemberProp::Computed(property) => {
                matches!(
                    unwrap_parens(&property.expr),
                    ast::Expr::Lit(ast::Lit::Str(_))
                ) && lower.member_key_units(member)? == length_units
            }
            _ => false,
        };
        if is_length {
            return Ok(Kind::Number);
        }
        // Named properties on a dense Array are ordinary prototype-chain
        // References: the Array itself has no named own rows.
        let ast::MemberProp::Computed(property) = &member.prop else {
            return Ok(Kind::Dynamic);
        };
        if let Some(index) = Lowerer::member_index_literal(&property.expr) {
            if let Some(shape) = shape {
                let position = usize::try_from(index).unwrap_or(usize::MAX);
                if position >= shape.length {
                    return Ok(Kind::Undefined);
                }
                return Ok(if shape.repr == super::lowerer::ArrayRepr::Packed {
                    Kind::Number
                } else {
                    Kind::Dynamic
                });
            }
            return Ok(Kind::Dynamic);
        }
        if matches!(
            super::lowerer::Lowerer::unwrap_parens(&property.expr),
            ast::Expr::Lit(ast::Lit::Str(_))
        ) {
            return Ok(Kind::Dynamic);
        }
        // Non-literal or length-unknown indexes dispatch at runtime on the
        // real node kind; the element value may be any JsValue.
        return Ok(Kind::Dynamic);
    }
    if matches!(kind, Kind::String | Kind::Dynamic)
        && lower.member_key_units(member)? == [108, 101, 110, 103, 116, 104]
    {
        lower.record_string_length_site(member)?;
        lower.needs_ordinary_realm.set(true);
        return Ok(if kind == Kind::String {
            Kind::Number
        } else {
            Kind::Dynamic
        });
    }
    admit_object_member_read(lower, member, kind)
}

/// Static name on a known String, Number, or Boolean receiver.
///
/// Identifier keys stay admitted. A computed key is admitted only when its
/// expression is itself a string literal; parentheses are not peeled. Every
/// other key returns false before decoding, so a dynamic `o[key]` or `a[key]`
/// call keeps its prepared route. Only then does `member_key_units` decode
/// length and escapes. A String whose decoded name is a canonical index, the
/// `append_index_key` rule, stays on the existing frontend refusal. Number
/// and Boolean decimal names are ordinary named reads. This is the same
/// Reference Get, not a method whitelist or a second route.
pub(super) fn named_primitive_read(
    lower: &Lowerer<'_>,
    member: &ast::MemberExpr,
    kind: Kind,
) -> Result<bool, ParseError> {
    if !matches!(kind, Kind::String | Kind::Number | Kind::Boolean) {
        return Ok(false);
    }
    let static_name = match &member.prop {
        ast::MemberProp::Ident(_) => true,
        ast::MemberProp::Computed(property) => {
            matches!(property.expr.as_ref(), ast::Expr::Lit(ast::Lit::Str(_)))
        }
        _ => false,
    };
    if !static_name {
        return Ok(false);
    }
    let units = lower.member_key_units(member)?;
    if kind == Kind::String && canonical_index_units(&units) {
        return Ok(false);
    }
    Ok(true)
}

/// Canonical index name under the `append_index_key` rule: length 1..=10,
/// every unit an ASCII decimal digit, and a leading zero only for the single
/// unit `0`. No numeric parse and no 2^32-1 bound.
fn canonical_index_units(units: &[u16]) -> bool {
    let length = units.len();
    if length < 1 || length > 10 {
        return false;
    }
    let zero = u16::from(b'0');
    let nine = u16::from(b'9');
    for (cursor, unit) in units.iter().copied().enumerate() {
        if unit < zero || unit > nine {
            return false;
        }
        if cursor == 0 && length > 1 && unit == zero {
            return false;
        }
    }
    true
}

/// Original object/function Get admission, also used by member-call References.
/// A call-only site must not select the primitive String read adapter.
pub(super) fn admit_object_member_read(
    lower: &Lowerer<'_>,
    member: &ast::MemberExpr,
    kind: Kind,
) -> Result<Kind, ParseError> {
    if named_primitive_read(lower, member, kind)? {
        lower.needs_ordinary_realm.set(true);
        return Ok(Kind::Dynamic);
    }
    admit_object_receiver(lower, member.span, kind)?;
    let key = lower.member_key_units(member)?;
    if kind == Kind::Function && key == "length".encode_utf16().collect::<Vec<_>>() {
        return Ok(Kind::Number);
    }
    // Mutable property reads carry the same unproved JsValue as call results.
    Ok(Kind::Dynamic)
}

pub(super) fn admit_member_write(
    lower: &Lowerer<'_>,
    member: &ast::MemberExpr,
    kind: Kind,
    shape: Option<super::lowerer::ArrayShape>,
) -> Result<(), ParseError> {
    if matches!(
        kind,
        Kind::Object | Kind::Function | Kind::Null | Kind::Undefined | Kind::Dynamic
    ) {
        lower.needs_ordinary_realm.set(true);
        lower.member_key_units(member)?;
        return Ok(());
    }
    if kind == Kind::Array {
        if let ast::MemberProp::Ident(prop) = &member.prop {
            if prop.sym.as_str() == "length" {
                // The write may throw TypeError, so the realm must exist
                // before emission builds that path.
                lower.needs_ordinary_realm.set(true);
                return Ok(());
            }
        }
    }
    if !matches!(member.prop, ast::MemberProp::Computed(_)) {
        return Err(lower.error(
            member.span,
            "P0 plain objects reject property assignment until P7 descriptors",
        ));
    }
    let ast::MemberProp::Computed(property) = &member.prop else {
        unreachable!()
    };
    if kind != Kind::Array || !matches!(member.obj.as_ref(), ast::Expr::Ident(_)) {
        return Err(lower.error(
            member.span,
            "P0 indexed writes require a numeric-array binding",
        ));
    }
    // Non-literal integer indexes are checked at runtime against the live
    // header length. A literal index keeps the exact compile-time refusal.
    if let Some(index) = Lowerer::member_index_literal(&property.expr)
        && let Some(shape) = shape
    {
        let position = usize::try_from(index).unwrap_or(usize::MAX);
        if position >= shape.length {
            return Err(lower.error(
                property.span(),
                "P0 numeric arrays reject length-changing writes",
            ));
        }
    }
    Ok(())
}

/// Dense-array element admission. A statically packed array keeps the old
/// Number-only refusal; tagged arrays admit the seven supported JsValue
/// kinds. Nested arrays are outside this dense slice and fail closed.
pub(super) fn admit_array_element(
    lower: &Lowerer<'_>,
    span: swc_common::Span,
    kind: Kind,
    repr: Option<super::lowerer::ArrayRepr>,
) -> Result<(), ParseError> {
    if repr == Some(super::lowerer::ArrayRepr::Packed) && kind != Kind::Number {
        return Err(lower.error(span, "P0 numeric arrays store only numbers"));
    }
    if kind == Kind::Array {
        return Err(lower.error(
            span,
            "P0 dense arrays reject nested array elements in this scope",
        ));
    }
    if !matches!(
        kind,
        Kind::Number
            | Kind::String
            | Kind::Boolean
            | Kind::Null
            | Kind::Undefined
            | Kind::Object
            | Kind::Function
            | Kind::Dynamic
    ) {
        return Err(lower.error(span, "P0 dense arrays admit only JsValue elements"));
    }
    Ok(())
}

pub(super) fn admit_object_value(
    lower: &Lowerer<'_>,
    span: swc_common::Span,
    kind: Kind,
) -> Result<(), ParseError> {
    if kind == Kind::Array {
        return Err(lower.error(span, "P0 plain-object values admit primitives, strings, objects and functions; numeric arrays stay separate bindings"));
    }
    Ok(())
}

pub(super) fn admit_numeric_operand(
    lower: &Lowerer<'_>,
    span: swc_common::Span,
    kind: Kind,
) -> Result<(), ParseError> {
    if !matches!(kind, Kind::Number | Kind::Dynamic) {
        return Err(lower.error(span, "P0 arithmetic currently requires numeric operands"));
    }
    Ok(())
}

fn admit_profile_numeric_operand(
    lower: &Lowerer<'_>,
    span: swc_common::Span,
    fact: ProfileFact,
) -> Result<(), ParseError> {
    if fact.numeric_eligible() {
        Ok(())
    } else {
        Err(lower.error(span, "P0 arithmetic currently requires numeric operands"))
    }
}

#[cfg(test)]
mod sloppy107_scanner_tests {
    use super::*;

    #[test]
    fn sloppy107_activation_root_nested_and_computed_key() {
        let (mut script, _) = super::super::parser::parse_script(
            "function f(){var o={get x(){'use strict';return this;}};return 7;}",
            0,
        )
        .unwrap();
        let ast::Stmt::Decl(ast::Decl::Fn(root)) = &mut script.body[0] else {
            panic!("root")
        };
        let body = root.function.body.as_mut().unwrap();
        let mut scan = SourceScan {
            activation_local: true,
            ..SourceScan::default()
        };
        scan.statements(&body.stmts, false);
        assert!(
            scan.activation_refusal.is_none(),
            "nested getter body belongs to itself"
        );
        let ast::Stmt::Decl(ast::Decl::Var(var)) = &mut body.stmts[0] else {
            panic!("var")
        };
        let ast::Expr::Object(object) = var.decls[0].init.as_mut().unwrap().as_mut() else {
            panic!("object")
        };
        let ast::PropOrSpread::Prop(prop) = &mut object.props[0] else {
            panic!("property")
        };
        let ast::Prop::Getter(getter) = prop.as_mut() else {
            panic!("getter")
        };
        let span = getter.key.span();
        // Computed keys are not admitted ES5 source. This AST-only control
        // checks the evaluated outer key before skipping the nested body.
        getter.key = ast::PropName::Computed(ast::ComputedPropName {
            span,
            expr: Box::new(ast::Expr::This(ast::ThisExpr { span })),
        });
        let mut own = SourceScan {
            activation_local: true,
            ..SourceScan::default()
        };
        own.statements(&body.stmts, false);
        assert_eq!(own.activation_refusal.unwrap().0, span);
        let mut original = SourceScan::default();
        original.statements(&body.stmts, false);
        assert!(original.activation_refusal.is_none());
        assert!(
            !original.may_guest_throw && !original.script_try && !original.possible_number_member
        );
        eprintln!(
            "SLOPPY107_SCANNER root_scanned=true nested_skipped=true outer_computed_key_scanned=true default_unchanged=true guest_invocations=0"
        );
    }

    #[test]
    fn sloppy107_raw_directive_scope_facts() {
        for (source, expected) in [
            ("function f(){return 7;}", false),
            ("function f(){'use strict';return 7;}", true),
            ("'use strict';function f(){return 7;}", true),
            ("function f(){'use\\x20strict';return 7;}", false),
            ("function f(){switch(1){case 1:'use strict';}}", false),
        ] {
            let (script, base) = super::super::parser::parse_script(source, 0).unwrap();
            let graph = super::super::scope::build(&script, source, 0, base).unwrap();
            let function = graph
                .scopes
                .iter()
                .find(|s| s.kind == super::super::scope::ScopeKind::Function)
                .unwrap();
            assert_eq!(function.strict, expected, "{source}");
            eprintln!("SLOPPY107_SCOPE source={source:?} strict={expected} guest_invocations=0");
        }
    }
}
