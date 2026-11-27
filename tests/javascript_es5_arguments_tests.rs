//! P4 `arguments` observations: `.length`, `.callee`, numeric element reads
//! (including out-of-range Undefined), dynamic keys, and the pinned
//! no-mapping rule.
//!
//! The implicit binding is the activation's invocation vector. Pinned Micro
//! (mqjs 6d4d7eb6) materializes it as a real Array: `Array.isArray(arguments)`
//! is true and `typeof arguments` is `"object"`.

use vm::{Value, VmStatus};

fn run(source: &str) -> Value {
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| {
            panic!("compilation refused; source={source:?} error={error:?}")
        });
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..*machine.jit_config()
    });
    machine.set_fuel(10_000_000);
    assert_eq!(
        machine.run().expect("guest run"),
        VmStatus::Halted,
        "source={source:?}"
    );
    machine.stack().last().expect("completion").clone()
}

fn refuse(source: &str, diagnostic: &str) {
    let error = match pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source) {
        Ok(_) => panic!("must refuse source={source:?}"),
        Err(error) => error,
    };
    let text = format!("{error:?}");
    assert!(text.contains(diagnostic), "{source}: {text}");
}

fn field(value: &Value, name: &str) -> Value {
    let Value::Map(map) = value else {
        panic!("expected boxed JsValue completion, got {value:?}");
    };
    map.get(&Value::string(name))
        .cloned()
        .unwrap_or_else(|| panic!("missing field {name:?}: {value:?}"))
}

fn number(value: &Value) -> f64 {
    assert_eq!(field(value, "tag"), Value::Int(3));
    let Value::Float(number) = field(value, "number") else {
        panic!("Number payload missing: {value:?}");
    };
    number
}

fn flag(value: &Value) -> bool {
    assert_eq!(field(value, "tag"), Value::Int(2));
    let Value::Bool(flag) = field(value, "flag") else {
        panic!("Boolean payload missing: {value:?}");
    };
    flag
}

fn is_undefined(value: &Value) -> bool {
    field(value, "tag") == Value::Int(0)
}

#[test]
fn micro_arguments_length_with_formals() {
    assert_eq!(
        number(&run("function f(a,b){return arguments.length;}f(1,2);")),
        2.0
    );
}

#[test]
fn micro_arguments_length_actual_arity() {
    assert_eq!(
        number(&run("function h(){return arguments.length;}h(1,2,3);")),
        3.0
    );
    assert_eq!(number(&run("function h(){return arguments.length;}h();")), 0.0);
}

#[test]
fn micro_arguments_index_with_formals() {
    assert_eq!(number(&run("function f(a,b){return arguments[0];}f(7,8);")), 7.0);
    assert_eq!(number(&run("function f(a){return arguments[1];}f(7,8);")), 8.0);
}

#[test]
fn micro_arguments_index_out_of_bounds_is_undefined() {
    assert!(is_undefined(&run(
        "function f(a){return arguments[2];}f(7,8);"
    )));
    assert!(is_undefined(&run(
        "function f(){return arguments[5];}f(1,2);"
    )));
}

#[test]
fn micro_arguments_index_dynamic() {
    assert_eq!(
        number(&run("function f(i){return arguments[i];}f(2,5,6,7);")),
        6.0
    );
    assert!(is_undefined(&run(
        "function f(i){return arguments[i];}f(9,5,6,7);"
    )));
}

#[test]
fn micro_arguments_index_dynamic_from_length() {
    assert_eq!(
        number(&run(
            "function f(){return arguments[arguments.length-1];}f(1,2,3);"
        )),
        3.0
    );
}

#[test]
fn micro_arguments_fractional_index_is_undefined() {
    assert!(is_undefined(&run(
        "function f(){return arguments[1.5];}f(5,6);"
    )));
}

#[test]
fn micro_arguments_string_index_matches_element() {
    assert_eq!(
        number(&run("function f(){return arguments[\"0\"];}f(7);")),
        7.0
    );
    assert_eq!(
        number(&run("function f(a,b){return arguments[\"1\"];}f(7,8);")),
        8.0
    );
}

#[test]
fn micro_arguments_is_an_array() {
    // Pinned Micro: the implicit binding is a real Array value.
    assert!(flag(&run(
        "function f(){return Array.isArray(arguments);}f(1,2);"
    )));
}

#[test]
fn micro_arguments_array_is_array_control() {
    assert!(flag(&run(
        "function f(){var a=[1];return Array.isArray(a);}f();"
    )));
}

#[test]
fn micro_arguments_typeof_is_object() {
    assert!(flag(&run(
        "function f(){return typeof arguments===\"object\";}f(1);"
    )));
}

#[test]
fn micro_arguments_no_formal_mapping_on_write() {
    assert_eq!(
        number(&run("function g(a){arguments[0]=9;return a;}g(1);")),
        1.0
    );
    assert_eq!(
        number(&run("function g(a){a=7;return arguments[0];}g(1);")),
        1.0
    );
}

#[test]
fn micro_arguments_write_reads_back_from_vector() {
    assert_eq!(
        number(&run("function g(){arguments[0]=9;return arguments[0];}g(1);")),
        9.0
    );
}

#[test]
fn micro_arguments_strict_activation_reads() {
    assert_eq!(
        number(&run(
            "function f(a){\"use strict\";return arguments[0];}f(7);"
        )),
        7.0
    );
    assert_eq!(
        number(&run(
            "function f(a){\"use strict\";return arguments.length;}f(7,8);"
        )),
        2.0
    );
}

#[test]
fn micro_arguments_nested_activation_owns_its_vector() {
    assert_eq!(
        number(&run(
            "function f(){function g(){return arguments.length;}return g(1,2);}f();"
        )),
        2.0
    );
}

#[test]
fn micro_arguments_callee_is_undefined() {
    // Pinned Micro: a non-strict `arguments.callee` read yields Undefined; no
    // refusal and no TypeError.
    assert!(is_undefined(&run(
        "function f(){return arguments.callee;}f();"
    )));
}

#[test]
fn micro_arguments_length_write_is_admitted() {
    // Pinned Micro: the write is not a parse error; the function completes.
    assert_eq!(
        number(&run("function m(){arguments.length=1;return 1;}m(1);")),
        1.0
    );
}

#[test]
fn micro_arguments_length_write_truncates_extends_and_yields_rhs() {
    // Pinned `b1(5,6) -> "1|1|"`: the write really truncates, the removed
    // element is gone and the assignment yields the RHS value.
    assert_eq!(
        number(&run(
            "function b1(){var r=arguments.length=1;return r*100+arguments.length*10+(arguments[1]===undefined?1:0);}b1(5,6);"
        )),
        111.0
    );
    // Pinned `b3b(5) -> "2||"`: a longer length extends with holes that read
    // as Undefined (`[2]` is out of range, not a revived element).
    assert_eq!(
        number(&run(
            "function b3(){arguments.length=2;return arguments.length*10+(arguments[1]===undefined?2:0)+(arguments[2]===undefined?1:0);}b3(5);"
        )),
        23.0
    );
    // Pinned `b3(5,6,7) -> "1|2"`: the RHS side effect survives and the write
    // still lands.
    assert_eq!(
        number(&run(
            "function b3c(){var n=0;arguments.length=(n++,2);return n*10+arguments.length;}b3c(5,6,7);"
        )),
        12.0
    );
    // Truncation clears the physical tail permanently; growing again leaves a
    // hole, never the old element.
    assert_eq!(
        number(&run(
            "function b1b(){arguments.length=1;arguments.length=2;return arguments.length*10+(arguments[1]===undefined?1:0);}b1b(5,6);"
        )),
        21.0
    );
}

#[test]
fn micro_arguments_length_write_invalid_value_throws_guest_type_error() {
    // Pinned `b2(5,6) -> "TypeError:invalid array length"`, catchable by the
    // guest (not a VM-level host error).
    assert!(flag(&run(
        "function b2(){try{arguments.length=-1;}catch(e){return e.name===\"TypeError\"&&e.message===\"invalid array length\";}return false;}b2(5,6);"
    )));
    assert!(flag(&run(
        "function b2b(){try{arguments.length=1073741824;}catch(e){return e.name===\"TypeError\"&&e.message===\"invalid array length\";}return false;}b2b(5,6);"
    )));
}

#[test]
fn micro_arguments_jump_write_is_guest_type_error() {
    // Pinned `a7(1) -> "TypeError:invalid array subscript"`: a static index
    // beyond the current length is a catchable guest TypeError, never the
    // Core `Set` host trap.
    assert!(flag(&run(
        "function a7(){try{arguments[5]=9;}catch(e){return e.name===\"TypeError\"&&e.message===\"invalid array subscript\";}return false;}a7(1);"
    )));
    // `index == len` appends and `index < len` overwrites.
    assert_eq!(
        number(&run(
            "function a8(){arguments[1]=9;return arguments.length*10+arguments[1];}a8(1);"
        )),
        29.0
    );
    assert_eq!(
        number(&run(
            "function a8b(){arguments[0]=2;arguments[1]=3;return arguments.length*10+arguments[0];}a8b(1);"
        )),
        22.0
    );
}

#[test]
fn micro_arguments_array_is_array_observes_rewritten_method() {
    // Pinned: `Array.isArray` is writable, so the result is `false` after the
    // program replaces it with a function returning false. The call must stay
    // a real Get+Call; a constant-folded `true` bypasses the rewrite.
    assert!(!flag(&run(
        "function f(){Array.isArray=function(){return false;};return Array.isArray(arguments);}f(1,2);"
    )));
    // The untouched method still observes the real Array value.
    assert!(flag(&run(
        "function g(){return Array.isArray(arguments);}g(1,2);"
    )));
}

#[test]
fn micro_arguments_var_binding_is_syntax_error() {
    // Pinned Micro rejects non-strict `var arguments` ("invalid variable
    // name") in both script and function code.
    refuse("var arguments=3;", "'arguments' is an invalid variable name");
    refuse("var x, arguments;", "'arguments' is an invalid variable name");
    refuse(
        "function f(){var arguments;return 1;}f();",
        "'arguments' is an invalid variable name",
    );
}

#[test]
fn micro_arguments_function_declaration_stays_admitted() {
    // Pinned allows `function arguments(){}` and a declaration inside a
    // function never shadows the enclosing activation's own vector.
    assert_eq!(
        number(&run("function arguments(){return 4;}arguments();")),
        4.0
    );
    assert_eq!(
        number(&run(
            "function outer(){function arguments(){return 5;}return arguments[0];}outer(9);"
        )),
        9.0
    );
}


#[test]
fn micro_arguments_bare_binding_is_outside_subset() {
    // Known divergence from pinned Micro, which returns the arguments Array
    // for a bare `arguments`. This frontend admits only member observations
    // and keeps the unqualified-binding refusal as an explicit subset limit.
    refuse(
        "function f(){return arguments;}f();",
        "reject unqualified arguments/eval observations",
    );
}

// Pinned Micro (6d4d7eb6) passes the activation's own invocation vector to
// `Array.isArray`, not a per-call copy: a rewritten method that writes
// `a[0]` mutates the very vector `arguments[0]` reads, and a second pass
// observes both that write and any source write made in between. `===` is not
// available as evidence (a bare `arguments` reference is outside this
// subset), so identity is pinned through the write-through observations.

#[test]
fn micro_arguments_array_is_array_mutates_activation_vector() {
    // Pinned `h(7) -> 99`.
    assert_eq!(
        number(&run(
            "function h(){Array.isArray=function(a){a[0]=99;return false;};Array.isArray(arguments);return arguments[0];}h(7);"
        )),
        99.0
    );
}

#[test]
fn micro_arguments_array_is_array_second_pass_observes_source_write() {
    // Pinned same-object semantics: `q(7) -> 6` (first pass 7->8, source
    // write 5, second pass 5->6).
    assert_eq!(
        number(&run(
            "function q(){Array.isArray=function(a){a[0]=a[0]+1;return false;};Array.isArray(arguments);arguments[0]=5;Array.isArray(arguments);return arguments[0];}q(7);"
        )),
        6.0
    );
}

#[test]
fn micro_arguments_array_is_array_repeated_passes_share_one_vector() {
    // Pinned same-object semantics: `p(7) -> 9` (7->8->9). A per-call copy
    // resets to 7 on the second pass.
    assert_eq!(
        number(&run(
            "function p(){Array.isArray=function(a){a[0]=a[0]+1;return false;};Array.isArray(arguments);Array.isArray(arguments);return arguments[0];}p(7);"
        )),
        9.0
    );
}

#[test]
fn micro_arguments_array_is_array_object_actual_control() {
    // Reverse control: only the implicit activation binding is the vector. A
    // plain object actual is still not an Array, and passing a local Array
    // never turns it into the activation's arguments object.
    assert!(!flag(&run("function c(){var o={0:7};return Array.isArray(o);}c();")));
    assert_eq!(
        number(&run(
            "function c(){var a=[1];Array.isArray=function(x){x[0]=99;return false;};Array.isArray(a);return arguments[0];}c(7);"
        )),
        7.0
    );
}

#[test]
fn micro_arguments_outer_declaration_inner_reads_own_vector() {
    // Pinned `o() -> 9`: an enclosing `function arguments(){}` declaration
    // never redirects an inner activation's `arguments[0]` to the outer
    // binding; the inner function reads its own vector.
    assert_eq!(
        number(&run(
            "function o(){function arguments(){return 5;}function inner(){return arguments[0];}return inner(9);}o();"
        )),
        9.0
    );
}

#[test]
fn micro_arguments_outer_declaration_inner_length_is_own_activation() {
    // Pinned `p() -> 2`.
    assert_eq!(
        number(&run(
            "function p(){function arguments(){return 5;}function inner(){return arguments.length;}return inner(4,7);}p();"
        )),
        2.0
    );
}

#[test]
fn micro_arguments_array_is_array_local_array_reverse_control() {
    // Reverse control: a local Array actual of a rewritten `Array.isArray` is
    // the object the callee mutates, and it is not the activation's implicit
    // binding. (`catch(arguments)` is not a usable control: pinned Micro
    // rejects the catch parameter itself with a SyntaxError.)
    assert_eq!(
        number(&run(
            "function c(){var a=[1];Array.isArray=function(x){x[0]=99;return false;};Array.isArray(a);return a[0];}c();"
        )),
        99.0
    );
    assert!(flag(&run(
        "function c(){var a=[1];return Array.isArray(a);}c();"
    )));
}

#[test]
fn micro_arguments_dynamic_string_index_reads_element() {
    // Pinned: a runtime String key is an ordinary property get, so `"2"` is
    // the canonical index 2.
    assert_eq!(
        number(&run("function m(k){return arguments[k];}m(\"2\",5,6,7);")),
        6.0
    );
    // Pinned: a runtime `"length"` key is the length row.
    assert_eq!(
        number(&run("function m(k){return arguments[k];}m(\"length\",1,2);")),
        3.0
    );
}

#[test]
fn micro_arguments_dynamic_string_index_out_of_domain_is_undefined() {
    // Pinned: non-canonical Strings are absent own properties.
    assert!(is_undefined(&run(
        "function m(k){return arguments[k];}m(\"foo\",1,2);"
    )));
    assert!(is_undefined(&run(
        "function m(k){return arguments[k];}m(\"9\",5,6);"
    )));
    assert!(is_undefined(&run(
        "function m(k){return arguments[k];}m(\"01\",5,6,7);"
    )));
}

#[test]
fn micro_arguments_static_non_index_string_key_stays_refused() {
    // Reverse control: the static classification is unchanged, so a
    // non-numeric String literal key is still outside the admitted domain
    // instead of silently widening to the dynamic String route.
    refuse(
        "function m(){return arguments[\"foo\"];}m(1);",
        "P4 arguments observations admit only .length, .callee and numeric indexes",
    );
}

#[test]
fn micro_arguments_string_key_classifier_matches_pinned_table() {
    // Pinned Micro (6d4d7eb6) materializes the binding as a real Array, so a
    // runtime String subscript is an ordinary property get: only a canonical
    // decimal index names one element, `length` names the length row, and
    // every other String is an absent own property. A trailing unit, an
    // exponent suffix, a prefixed sign or a hex prefix must NOT be read as
    // its truncated numeric prefix. `probe` encodes each observation as one
    // base-8 digit (0..=4) and the script folds all fourteen into one Number,
    // so the whole table compiles and runs once.
    let cases: [(&str, i64); 14] = [
        ("\"0\"", 0),
        ("\"1\"", 1),
        ("\"2\"", 2),
        ("\"00\"", 3),
        ("\"01\"", 3),
        ("\" 1\"", 3),
        ("\"+1\"", 3),
        ("\"-0\"", 3),
        ("\"0x1\"", 3),
        ("\"1 \"", 3),
        ("\"1e1\"", 3),
        ("\"4294967295\"", 3),
        ("\"length\"", 4),
        ("\"Length\"", 3),
    ];
    let mut source = String::from(
        "function probe(a,b,c,k){var v=arguments[k];\
         if(v===undefined)return 3;if(v===11)return 0;if(v===22)return 1;\
         if(v===33)return 2;if(v===4)return 4;return 5;}var acc=0;",
    );
    let mut expected = 0i64;
    for (key, digit) in cases {
        source.push_str(&format!("acc=acc*8+probe(11,22,33,{key});"));
        expected = expected * 8 + digit;
    }
    source.push_str("acc;");
    let value = run(&source);
    let actual = number(&value) as i64;
    assert_eq!(
        actual, expected,
        "base-8 digit table (0->0, 1->1, 2->2, absent->3, length->4) source={source}"
    );
    // Decode the mismatch so a RED run names the offending key.
    let mut actual_digits = [0i64; 14];
    let mut rest = actual;
    for index in (0..14).rev() {
        actual_digits[index] = rest % 8;
        rest /= 8;
    }
    let mut expected_digits = [0i64; 14];
    let mut rest = expected;
    for index in (0..14).rev() {
        expected_digits[index] = rest % 8;
        rest /= 8;
    }
    for (index, (key, _)) in cases.iter().enumerate() {
        assert_eq!(
            actual_digits[index], expected_digits[index],
            "key={key} classified {expected} (0=0,1=1,2=2,3=absent,4=length), got {} for key={key}",
            actual_digits[index], expected = expected_digits[index]
        );
    }
}

fn frontend_ir(source: &str) -> vm::FrontendIr {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let context = vm::SourceFrontendContext {
        source_id: 0,
        original_path: None,
        phase: vm::ParsePhase::Standalone,
        host_api_catalog: Some(&catalog),
        imports: &[],
    };
    vm::SourcePlugin::parse_source(
        &pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN,
        source,
        &context,
    )
    .expect("frontend IR")
}

#[test]
fn micro_arguments_sync_growth_loop_rechecks_physical_base() {
    // The vector-to-object growth loop fills holes with `array_put`, which
    // relocates storage and releases the previous extent on the first hole.
    // A physical base cached once before the loop is dead for every later
    // iteration; the loop must re-read the directory row per entry.
    let ir = frontend_ir(
        "function f(){Array.isArray(arguments);arguments.length=4;Array.isArray(arguments);return 1;}f(1);",
    );
    let helper = |name: &str| -> u16 {
        ir.functions
            .iter()
            .find(|declaration| declaration.name == name)
            .unwrap_or_else(|| panic!("missing helper {name}"))
            .index
    };
    let sync = helper("__compat_es5_arguments_sync");
    let checked = helper("__compat_es5_checked_base");
    let put = helper("__compat_es5_array_put");
    fn walk(
        stmts: &[vm::Stmt],
        checked: u16,
        put: u16,
        refreshed: &mut Option<u16>,
    ) {
        for stmt in stmts {
            match stmt {
                vm::Stmt::While { body, .. } => {
                    let refresh = body.iter().find_map(|stmt| match stmt {
                        vm::Stmt::Assign {
                            index,
                            expr: vm::Expr::Call(id, _, _, _, _),
                            ..
                        } if *id == checked => Some(*index),
                        _ => None,
                    });
                    let put_base = body.iter().find_map(|stmt| match stmt {
                        vm::Stmt::Expr {
                            expr: vm::Expr::Call(id, _, args, _, _),
                            ..
                        } if *id == put => match args.as_slice() {
                            [_, vm::Expr::Var(base), _, _, _, _] => Some(*base),
                            _ => None,
                        },
                        _ => None,
                    });
                    if let (Some(refresh), Some(base)) = (refresh, put_base) {
                        assert_eq!(
                            refresh, base,
                            "array_put must consume the freshly checked base slot"
                        );
                        refreshed.get_or_insert(refresh);
                    }
                    walk(body, checked, put, refreshed);
                }
                vm::Stmt::IfElse {
                    then_branch,
                    else_branch,
                    ..
                } => {
                    walk(then_branch, checked, put, refreshed);
                    walk(else_branch, checked, put, refreshed);
                }
                _ => {}
            }
        }
    }
    let mut refreshed = None;
    walk(
        &ir.function_impls[&sync].body_stmts,
        checked,
        put,
        &mut refreshed,
    );
    assert!(
        refreshed.is_some(),
        "the growth loop must re-read checked_base inside the loop before array_put"
    );
}

#[test]
fn micro_arguments_array_is_array_growth_after_source_length_write() {
    // Minimal repro: start with one actual, pass it through the mirror once,
    // grow the invocation vector in source (`arguments.length=3`), then pass
    // it again. The second pass resizes the guest object from length 1 to 3
    // and must write indexes 1 and 2 into storage the resized node actually
    // owns; a cached pre-growth physical base would address a freed extent.
    // Pinned: the callback observes length 3 and first item 1.
    assert_eq!(
        number(&run(
            "function f(){Array.isArray(arguments);arguments.length=3;Array.isArray(arguments);return arguments.length*10+arguments[0];}f(1);"
        )),
        31.0
    );
    // Index 2 is the second hole of the same growth; reading it from the
    // vector stays Undefined, and a third pass after another growth must not
    // reuse a stale base either.
    assert_eq!(
        number(&run(
            "function f(){Array.isArray(arguments);arguments.length=4;Array.isArray(arguments);Array.isArray(arguments);return arguments.length*10+(arguments[2]===undefined?1:0);}f(1);"
        )),
        41.0
    );
}

// ---------------------------------------------------------------------------
// Real-GC witness for the source-write mirror root.
//
// `__compat_es5_node_begin` is the single heap reservation entry. Arming the
// existing allocation quantum (`HDR_ALLOCATIONS_SINCE_GC = 32`) at its entry
// makes every subsequent reservation force a real collection, exactly like
// the maintained heap safepoint fixtures. The source then writes a fresh
// object into `arguments[0]`, allocates unrelated garbage, and reads the
// written element back after the next mirror pass. Without a heap edge for
// the written value the collection sweeps it and the read fails; with the
// write-through edge it survives.
struct ArgumentsGcObserver;

impl vm::SourcePlugin for ArgumentsGcObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut descriptor =
            vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        descriptor.id = vm::FrontendId::new("javascript-es5-arguments-gc");
        descriptor
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S, TypeSchema as T};
        let mut ir = vm::SourcePlugin::parse_source(
            &pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN,
            source,
            context,
        )?;
        let helper = |ir: &vm::FrontendIr, name: &str| -> Result<u16, vm::ParseError> {
            ir.functions
                .iter()
                .find(|declaration| declaration.name == name)
                .map(|declaration| declaration.index)
                .ok_or_else(|| vm::ParseError::new(format!("missing helper {name}")))
        };
        let begin = helper(&ir, "__compat_es5_node_begin")?;
        let write = helper(&ir, "__compat_ref_write")?;
        let slot = u16::try_from(ir.locals).expect("fresh local");
        ir.locals += 1;
        let body = ir
            .function_impls
            .get_mut(&begin)
            .ok_or_else(|| vm::ParseError::new("node_begin body"))?;
        let carrier = body.param_slots[0];
        let kind = body.param_slots[2];
        body.body_stmts.insert(
            0,
            S::IfElse {
                // Only plain-object reservations force the collection, so the
                // realm bootstrap, the guest Array node itself and unrelated
                // array/string reservations never depend on it. `{x:1}` then
                // collects once before it exists and `{y:2}` collects again
                // while the mirrored value is live.
                condition: E::Or(
                    Box::new(E::Eq(Box::new(E::Var(kind)), Box::new(E::Int(3)))), // plain object
                    Box::new(E::Eq(Box::new(E::Var(kind)), Box::new(E::Int(6)))), // tagged array
                ),
                then_branch: vec![S::Let {
                    index: slot,
                    declared_schema: Some(T::Number),
                    expr: E::Call(
                        write,
                        Vec::new(),
                        vec![E::Var(carrier), E::Float(10.0), E::Float(32.0)],
                        None,
                        None,
                    ),
                    line: 1,
                }],
                else_branch: Vec::new(),
                line: 1,
            },
        );
        Ok(ir)
    }
}

static ARGUMENTS_GC_OBSERVER: ArgumentsGcObserver = ArgumentsGcObserver;

fn run_with_forced_gc(source: &str) -> Value {
    use vm::HostFunctionRegistry;
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let issued = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-arguments-gc")),
        vm::CompileSourceFileOptions::new()
            .with_host_api_catalog(catalog.clone())
            .with_source_plugin(&ARGUMENTS_GC_OBSERVER),
    )
    .unwrap_or_else(|error| panic!("GC witness source must compile: {source}\n{error:?}"));
    let mut machine = issued.into_vm().expect("GC witness VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..*machine.jit_config()
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        Default::default(),
    )
    .expect("realm initialization");
    let mut registry = HostFunctionRegistry::new();
    for module in [
        pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
        pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module(),
    ] {
        module
            .install_from_catalog(&mut registry, &catalog)
            .expect("host module");
    }
    for builtin in [
        "math::floor",
        "math::sqrt",
        "math::round",
        "math::powf",
        "math::is_nan",
        "math::is_finite",
    ] {
        registry.allow_builtin(builtin).expect("builtin grant");
    }
    registry.bind_vm_cached(&mut machine).expect("host binding");
    machine.set_fuel(10_000_000);
    assert_eq!(
        machine.run().expect("GC witness guest run"),
        VmStatus::Halted,
        "source={source:?}"
    );
    machine.stack().last().expect("completion").clone()
}

#[test]
fn micro_arguments_source_write_survives_real_gc_before_next_sync() {
    // `Array.isArray(arguments)` creates and roots the activation's single
    // guest Array. The source write then replaces `arguments[0]` with a fresh
    // object that currently lives only in the untraced Core vector; the
    // unrelated `{y:2}` reservation forces a real collection before the next
    // mirror pass, and the read must still observe the written object.
    let value = run_with_forced_gc(
        "function f(){Array.isArray(arguments);arguments[0]={x:1};var pad={y:2};Array.isArray(arguments);return arguments[0].x;}f(0);",
    );
    assert_eq!(number(&value), 1.0);
    // Comma expression: the temporary root of `arguments[0]={x:1}` is cleared
    // by the discarded first operand, so only a heap-side mirror edge can
    // keep the object alive across the forced collection.
    let value = run_with_forced_gc(
        "function f(){Array.isArray(arguments);var pad=(arguments[0]={x:1},0);var more={z:2};Array.isArray(arguments);return arguments[0].x;}f(0);",
    );
    assert_eq!(number(&value), 1.0);
}

#[test]
fn micro_bound_payload_survives_real_gc_before_next_sync() {
    // The bound product owns exactly one fixed `this` object and one
    // reference argument, and both source locals are cleared before the
    // forced collections. The two `{p:1}`/`{q:2}` reservations each start a
    // real collection, so the returned `this.x` and `a.y` prove the private
    // environment pair -> payload Array -> tagged entries edge is traced.
    // If the payload lived only in an untraced Core vector, or the
    // environment edge were missing, the collection would sweep both values
    // and the call would trap or read a retired pair.
    let value = run_with_forced_gc(
        "function f(a){\"use strict\";return [this===null,this.x,a.y].join(\"|\");}var box={x:1};var arg={y:2};var b=f.bind(box,arg);box=null;arg=null;var p1={p:1};var p2={q:2};var hit=0;if(b()===\"false|1|2\"){hit=7;}hit;",
    );
    assert_eq!(number(&value), 7.0);
}




