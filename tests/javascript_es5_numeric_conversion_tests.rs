//! Shared primitive numeric conversions through the public interpreter entry.
fn scalar(source: &str, expected: f64) {
    let script = format!(
        "const v=(0,eval)({});const b=Buffer.alloc(8);b.writeDoubleBE(v);console.log(Number.isNaN(v)?'NaN':b.toString('hex'));",
        serde_json::to_string(source).unwrap()
    );
    let node = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    eprintln!(
        "NUM138_NODE {}",
        serde_json::json!({"source":source,"argv":["timeout","15","node","-e",script],"rc":node.status.code(),"stdout":String::from_utf8_lossy(&node.stdout),"stderr":String::from_utf8_lossy(&node.stderr)})
    );
    assert!(node.status.success() && node.stderr.is_empty());
    let expected_bits = if expected.is_nan() {
        "NaN".to_owned()
    } else {
        format!("{:016x}", expected.to_bits())
    };
    assert_eq!(
        String::from_utf8(node.stdout).unwrap().trim(),
        expected_bits
    );
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut machine =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                    .unwrap_or_else(|e| {
                        panic!("NUM138_COMPILE source={source:?} guest_invocations=0 {e:?}")
                    });
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let status = machine.run();
            eprintln!(
                "NUM138_VM source={source:?} status={status:?} stack={:?}",
                machine.stack()
            );
            assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            let vm::Value::Map(boxed) = machine.stack().last().unwrap() else {
                panic!("Number box")
            };
            assert_eq!(
                boxed.get(&vm::Value::string("tag")),
                Some(&vm::Value::Int(3))
            );
            let vm::Value::Float(actual) = boxed.get(&vm::Value::string("number")).unwrap() else {
                panic!("Float payload")
            };
            assert!(
                if expected.is_nan() {
                    actual.is_nan()
                } else {
                    actual.to_bits() == expected.to_bits()
                },
                "{source}: actual={actual:?} expected={expected:?}"
            );
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            assert!(machine.capture_certification_stats().unwrap().valid);
            machine.reset_for_reuse().unwrap();
            if machine.scope_reset_pending() {
                let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
                assert!(matches!(
                    machine.poll_reset_for_reuse(&mut cx),
                    std::task::Poll::Ready(Ok(()))
                ));
            }
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            assert!(!machine.capture_certification_stats().unwrap().valid);
            assert!(
                pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
                    .is_none()
            );
            eprintln!("NUM138_RESET source={source:?} resources=0 native=0");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn numeric138_neg_boolean() {
    scalar("-true;", -1.0);
}

#[test]
fn numeric138_multiply_string() {
    scalar("'3'*2;", 6.0);
}

#[test]
fn numeric138_subtract_null() {
    scalar("null-1;", -1.0);
}

#[test]
fn numeric138_divide_string() {
    scalar("'8'/true;", 8.0);
}

macro_rules! conversion_case {
    ($name:ident, $source:literal, $expected:expr) => {
        #[test]
        fn $name() {
            scalar($source, $expected);
        }
    };
}
conversion_case!(numeric138_family_not, "~'1';", -2.0);
conversion_case!(numeric138_family_remainder, "'x'%1;", f64::NAN);
conversion_case!(numeric138_family_and, "'3'&true;", 1.0);
conversion_case!(numeric138_family_or, "'8'|true;", 9.0);
conversion_case!(numeric138_family_xor, "'3'^true;", 2.0);
conversion_case!(numeric138_family_left, "'8'<<true;", 16.0);
conversion_case!(numeric138_family_right, "'8'>>true;", 4.0);
conversion_case!(
    numeric138_family_unsigned_right,
    "'-1'>>>true;",
    2147483647.0
);

fn runtime_matrix(op: &str, unary: bool) {
    let inputs = [
        "3",
        "true",
        "false",
        "null",
        "undefined",
        "'3'",
        "'x'",
        "'-0'",
        "'Infinity'",
        "'1e308'",
        "'-5.75'",
        "'33'",
    ];
    let body = if unary {
        format!("{op}x")
    } else {
        format!("x{op}y")
    };
    let definition = format!("function f(x,y){{return {body};}}");
    let mut calls: Vec<String> = inputs.iter().map(|v| format!("f({v},2)")).collect();
    if !unary {
        calls.extend(inputs.iter().map(|v| format!("f('9',{v})")));
    }
    let script = format!(
        "{definition}const values=[{}];console.log(JSON.stringify(values.map(v=>{{const b=Buffer.alloc(8);b.writeDoubleBE(v);return Number.isNaN(v)?'NaN':b.toString('hex');}})));",
        calls.join(",")
    );
    let version = std::process::Command::new("node")
        .arg("--version")
        .output()
        .unwrap();
    let out = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    eprintln!(
        "NUM138_NODE_BATCH {}",
        serde_json::json!({"calls":calls,"script":script,"rc":out.status.code(),"stdout":String::from_utf8_lossy(&out.stdout),"stderr":String::from_utf8_lossy(&out.stderr),"version":String::from_utf8_lossy(&version.stdout)})
    );
    assert!(out.status.success() && out.stderr.is_empty() && version.status.success());
    let results: Vec<String> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(results.len(), calls.len());
    let mut source = definition;
    for (index, (call, bits)) in calls.iter().zip(results).enumerate() {
        let expected = if bits == "NaN" {
            f64::NAN
        } else {
            f64::from_bits(u64::from_str_radix(&bits, 16).unwrap())
        };
        let check = if expected.is_nan() {
            "n!==n".to_owned()
        } else if expected == 0.0 {
            format!(
                "n===0&&1/n==={}",
                if expected.is_sign_negative() {
                    "-Infinity"
                } else {
                    "Infinity"
                }
            )
        } else if expected == f64::INFINITY {
            "n===Infinity".to_owned()
        } else if expected == f64::NEG_INFINITY {
            "n===-Infinity".to_owned()
        } else {
            format!("n==={expected:?}")
        };
        source.push_str(&format!("var n={call};if(!({check}))throw {index};"));
    }
    source.push_str("1;");
    scalar(&source, 1.0);
}
macro_rules! runtime_case {
    ($name:ident,$op:literal,$unary:literal) => {
        #[test]
        fn $name() {
            runtime_matrix($op, $unary);
        }
    };
}
runtime_case!(numeric138_matrix_plus, "+", true);
runtime_case!(numeric138_matrix_neg, "-", true);
runtime_case!(numeric138_matrix_not, "~", true);
runtime_case!(numeric138_matrix_sub, "-", false);
runtime_case!(numeric138_matrix_mul, "*", false);
runtime_case!(numeric138_matrix_div, "/", false);
runtime_case!(numeric138_matrix_mod, "%", false);
runtime_case!(numeric138_matrix_and, "&", false);
runtime_case!(numeric138_matrix_or, "|", false);
runtime_case!(numeric138_matrix_xor, "^", false);
runtime_case!(numeric138_matrix_left, "<<", false);
runtime_case!(numeric138_matrix_right, ">>", false);
runtime_case!(numeric138_matrix_unsigned_right, ">>>", false);

#[test]
fn numeric138_runtime_multiply() {
    for (value, expected) in [
        ("3", 6.0),
        ("true", 2.0),
        ("false", 0.0),
        ("null", 0.0),
        ("undefined", f64::NAN),
        ("'3'", 6.0),
        ("'x'", f64::NAN),
    ] {
        scalar(
            &format!("function f(x){{return x*2;}}f({value});"),
            expected,
        );
    }
}

struct ParserObserver {
    gc: bool,
    fatal: bool,
}
impl vm::SourcePlugin for ParserObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("primitive135-parser-observer");
        d
    }
    fn parse_source(
        &self,
        source: &str,
        cx: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S, TypeSchema as T};
        let mut ir = vm::SourcePlugin::parse_source(
            &pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN,
            source,
            cx,
        )?;
        let h = |name: &str| ir.functions.iter().find(|f| f.name == name).unwrap().index;
        let scan = h("__compat_es5_numeric_scan");
        let checked = h("__compat_es5_checked_base");
        let read = h("__compat_ref_read");
        let decode = h("__compat_es5_decode_int");
        let collect = h("__compat_es5_heap_collect");
        let p = ir.function_impls[&scan].param_slots.clone();
        assert_eq!(p.len(), 4);
        let (c, input, mode, radix) = (p[0], p[1], p[2], p[3]);
        let mut fresh = || {
            let i = u16::try_from(ir.locals).unwrap();
            ir.locals += 1;
            i
        };
        let (before, base, top, cursor, row, found) =
            (fresh(), fresh(), fresh(), fresh(), fresh(), fresh());
        let call = |i, args| E::Call(i, vec![], args, None, None);
        let num = |e| E::Div(Box::new(e), Box::new(E::Float(1.0)));
        let add = |a, b| E::Add(Box::new(a), Box::new(b));
        let eq = |a, b| E::Eq(Box::new(a), Box::new(b));
        let get = |s, k: &str| {
            call(
                vm::BuiltinFunction::Get.call_index(),
                vec![E::Var(s), E::String(k.into())],
            )
        };
        let rd = |a| call(decode, vec![num(call(read, vec![E::Var(c), num(a)]))]);
        let check = |e| S::Expr {
            expr: call(vm::BuiltinFunction::Assert.call_index(), vec![e]),
            line: 1,
        };
        let let_ = |index, t, expr| S::Let {
            index,
            declared_schema: Some(t),
            expr,
            line: 1,
        };
        let set = |index, expr| S::Assign {
            kind: vm::AssignmentKind::Set,
            index,
            expr,
            line: 1,
        };
        let base_expr = || {
            call(
                checked,
                vec![E::Var(c), get(input, "ref_slot"), get(input, "generation")],
            )
        };
        let mut pre = vec![
            check(eq(E::Var(mode), E::Int(0))),
            check(eq(E::Var(radix), E::Float(0.0))),
        ];
        if self.fatal {
            pre.push(check(E::Bool(false)));
        }
        if self.gc {
            pre.extend([
                let_(before, T::Int, rd(E::Int(11))),
                let_(base, T::Int, base_expr()),
                let_(top, T::Int, rd(E::Int(3))),
                let_(cursor, T::Int, E::Int(0)),
                let_(row, T::Int, E::Int(0)),
                let_(found, T::Bool, E::Bool(false)),
                S::While {
                    condition: E::Lt(
                        Box::new(E::Var(cursor)),
                        Box::new(rd(add(E::Var(top), E::Int(3)))),
                    ),
                    body: vec![
                        set(
                            row,
                            add(
                                add(E::Var(top), E::Int(4)),
                                E::Mul(Box::new(E::Var(cursor)), Box::new(E::Int(3))),
                            ),
                        ),
                        set(
                            found,
                            E::Or(
                                Box::new(E::Var(found)),
                                Box::new(E::And(
                                    Box::new(eq(rd(E::Var(row)), E::Int(4))),
                                    Box::new(E::And(
                                        Box::new(eq(
                                            rd(add(E::Var(row), E::Int(1))),
                                            get(input, "ref_slot"),
                                        )),
                                        Box::new(eq(
                                            rd(add(E::Var(row), E::Int(2))),
                                            get(input, "generation"),
                                        )),
                                    )),
                                )),
                            ),
                        ),
                        set(cursor, add(E::Var(cursor), E::Int(1))),
                    ],
                    line: 1,
                },
                check(E::Var(found)),
                S::Expr {
                    expr: call(collect, vec![E::Var(c)]),
                    line: 1,
                },
                check(E::Gt(Box::new(rd(E::Int(11))), Box::new(E::Var(before)))),
                check(eq(base_expr(), E::Var(base))),
            ]);
        }
        let f = ir.function_impls.get_mut(&scan).unwrap();
        pre.append(&mut f.body_stmts);
        f.body_stmts = pre;
        Ok(ir)
    }
}

fn reset(machine: &mut vm::Vm) {
    assert!(machine.capture_certification_stats().unwrap().valid);
    machine.reset_for_reuse().unwrap();
    if machine.scope_reset_pending() {
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(matches!(
            machine.poll_reset_for_reuse(&mut cx),
            std::task::Poll::Ready(Ok(()))
        ));
    }
    assert!(!machine.scope_reset_pending());
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(!machine.capture_certification_stats().unwrap().valid);
    assert!(pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(machine).is_none());
}

fn probe_vm(source: &str, observer: &'static dyn vm::SourcePlugin, id: &'static str) -> vm::Vm {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let issued = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new(id)),
        vm::CompileSourceFileOptions::new()
            .with_host_api_catalog(catalog.clone())
            .with_source_plugin(observer),
    )
    .unwrap_or_else(|error| panic!("source={source:?} guest_invocations=0 {error:?}"));
    let mut machine = issued.into_vm().unwrap();
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..*machine.jit_config()
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        Default::default(),
    )
    .unwrap();
    let mut registry = vm::HostFunctionRegistry::restricted();
    for module in [
        pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
        pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module(),
    ] {
        module
            .install_from_catalog(&mut registry, &catalog)
            .unwrap();
    }
    for name in [
        "math::floor",
        "math::abs",
        "math::acos",
        "math::asin",
        "math::atan",
        "math::atan2",
        "math::ceil",
        "math::cos",
        "math::exp",
        "math::ln",
        "math::sin",
        "math::tan",
        "math::trunc",
        "math::is_infinite",
        "math::copysign",
        "math::sqrt",
        "math::round",
        "math::powf",
        "math::is_nan",
        "math::is_finite",
        "bytes::from_utf8",
    ] {
        registry.allow_builtin(name).unwrap();
    }
    registry.bind_vm_cached(&mut machine).unwrap();
    machine.set_fuel(10_000_000);
    machine
}

#[test]
fn numeric138_parser_gc_dynamic() {
    static OBS: ParserObserver = ParserObserver {
        gc: true,
        fatal: false,
    };
    for source in [
        "function f(x){return x*2;}var s='3';var a=s;var n=f(s);if(s!==a)throw 'alias';n;",
        "function f(){return '3';}f()*f();",
        "var o={n:0,get x(){'use strict';this.n=this.n+1;return '3';}};var n=o.x*o.x;if(o.n!==2)throw 'once';n;",
    ] {
        std::thread::Builder::new()
            .stack_size(33_554_432)
            .spawn(move || {
                let mut machine = probe_vm(source, &OBS, "primitive135-parser-observer");
                let status = machine.run();
                eprintln!(
                    "NUM138_GC_VM source={source:?} status={status:?} stack={:?}",
                    machine.stack()
                );
                assert_eq!(status.unwrap(), vm::VmStatus::Halted);
                let vm::Value::Map(boxed) = machine.stack().last().unwrap() else {
                    panic!("Number box")
                };
                let expected = if source.contains("var s=") { 6.0 } else { 9.0 };
                assert_eq!(
                    boxed.get(&vm::Value::string("number")),
                    Some(&vm::Value::Float(expected))
                );
                assert_eq!(
                    (machine.aot_exec_count(), machine.jit_native_exec_count()),
                    (0, 0)
                );
                reset(&mut machine);
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

#[test]
fn numeric138_parser_lazy_abrupt() {
    static OBS: ParserObserver = ParserObserver {
        gc: false,
        fatal: true,
    };
    for (source, entered) in [
        ("function f(x){return x*2;}f('3');", true),
        ("function f(x){return x*2;}f(true);", false),
        (
            "function left(){return '3';}function fail(){throw 'R';}var n=0;try{left()*fail();n=99;}catch(e){n=e==='R'?1:2;}n;",
            false,
        ),
        (
            "function fail(){throw 'L';}var o={n:0,get x(){'use strict';this.n=this.n+1;return '3';}};var n=0;try{fail()*o.x;}catch(e){n=e==='L'?1:2;}n+o.n;",
            false,
        ),
    ] {
        std::thread::Builder::new()
            .stack_size(33_554_432)
            .spawn(move || {
                let mut machine = probe_vm(source, &OBS, "primitive135-parser-observer");
                let status = machine.run();
                eprintln!(
                    "NUM138_PARSER_ENTRY_VM source={source:?} entered={entered} status={status:?}"
                );
                if entered {
                    assert!(
                        format!("{:?}", status.expect_err("parser marker reached"))
                            .contains("assertion failed")
                    );
                } else {
                    assert_eq!(status.unwrap(), vm::VmStatus::Halted);
                    let vm::Value::Map(boxed) = machine.stack().last().unwrap() else {
                        panic!("Number box")
                    };
                    let expected = if source.ends_with("f(true);") {
                        2.0
                    } else {
                        1.0
                    };
                    assert_eq!(
                        boxed.get(&vm::Value::string("number")),
                        Some(&vm::Value::Float(expected))
                    );
                }
                assert_eq!(
                    (machine.aot_exec_count(), machine.jit_native_exec_count()),
                    (0, 0)
                );
                reset(&mut machine);
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

#[test]
fn numeric138_order_completion() {
    for source in [
        "var o={n:0,get a(){'use strict';this.n=this.n*10+1;return '3';},get b(){'use strict';this.n=this.n*10+2;return true;}};var r=o.a*o.b;if(r!==3||o.n!==12)throw 'order';1;",
        "function bad(){return {};}function fail(){throw 'R';}var n=0;try{bad()*fail();}catch(e){n=e==='R'?1:2;}finally{n=n+1;}n-1;",
        "function fail(){throw 'P\\0\\ud800';}var n=0;try{-(fail(),true);}catch(e){n=e==='P\\0\\ud800'?1:2;}finally{var garbage='a'+'b';}n;",
        "function f(x){try{return x*2;}finally{var garbage='a'+'b';}}f('3')===6?1:0;",
        "function f(x){function fail(){throw 'R';}try{return x*fail();}finally{return 1;}}f({});",
        "var x=true?'3':1;var a=x;var n=x*2;if(x!==a||n!==6)throw 'alias';1;",
        "function outer(x){function inner(y){return y*2;}return inner(x);}outer('3')===6?1:0;",
    ] {
        scalar(source, 1.0);
    }
}

fn parse(source: &str) -> Result<vm::FrontendIr, vm::ParseError> {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    vm::SourcePlugin::parse_source(
        &pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN,
        source,
        &vm::SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::compiler::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        },
    )
}
fn guarded(op: &str, unary: bool) {
    let expression = if unary {
        format!("{op}x")
    } else {
        format!("x{op}2")
    };
    let source = format!(
        "function f(x){{return {expression};}}var caught=false;try{{f({{}});}}catch(e){{caught=true;}}caught;"
    );
    fatal_source(&source);
}
fn fatal_source(source: &str) {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut machine =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                    .unwrap();
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let status = machine.run();
            eprintln!("NUM138_GUARD_VM source={source:?} status={status:?}");
            assert!(
                format!(
                    "{:?}",
                    status.expect_err("bounded primitive numeric profile guard must be fatal")
                )
                .contains("assertion failed")
            );
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            reset(&mut machine);
        })
        .unwrap()
        .join()
        .unwrap();
}
macro_rules! guard_case {
    ($name:ident,$op:literal,$unary:literal) => {
        #[test]
        fn $name() {
            guarded($op, $unary);
        }
    };
}
guard_case!(numeric138_guard_plus, "+", true);
guard_case!(numeric138_guard_neg, "-", true);
guard_case!(numeric138_guard_not, "~", true);
guard_case!(numeric138_guard_sub, "-", false);
guard_case!(numeric138_guard_mul, "*", false);
guard_case!(numeric138_guard_div, "/", false);
guard_case!(numeric138_guard_mod, "%", false);
guard_case!(numeric138_guard_and, "&", false);
guard_case!(numeric138_guard_or, "|", false);
guard_case!(numeric138_guard_xor, "^", false);
guard_case!(numeric138_guard_left, "<<", false);
guard_case!(numeric138_guard_right, ">>", false);
guard_case!(numeric138_guard_unsigned_right, ">>>", false);
#[test]
fn numeric138_guard_array_function_right() {
    for source in [
        "function f(x){return x*2;}f([1]);",
        "function f(x){return x*2;}f(function(){});",
        "function f(x){return 2*x;}f({});",
    ] {
        fatal_source(source);
    }
}
#[test]
fn numeric138_source_nonprimitive_and_excluded() {
    fn relational_positive(source: &str, expected: bool) {
        let source = source.to_owned();
        std::thread::Builder::new().stack_size(33_554_432).spawn(move || {
            eprintln!("REL145_LEGACY_SOURCE source={source:?} expected={expected}");
            let mut m = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source).unwrap();
            m.set_jit_config(vm::JitConfig { enabled: false, ..*m.jit_config() });
            assert!(!m.jit_config().enabled);
            assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
            m.set_fuel(10_000_000);
            let status = m.run();
            eprintln!("REL145_LEGACY_VM source={source:?} status={status:?} stack={:?} aot={} jit={}", m.stack(), m.aot_exec_count(), m.jit_native_exec_count());
            assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            let vm::Value::Map(b) = m.stack().last().unwrap() else { panic!("Boolean box") };
            assert_eq!(b.len(), 5);
            for (key, value) in [("tag", vm::Value::Int(2)), ("flag", vm::Value::Bool(expected)), ("number", vm::Value::Float(0.0)), ("ref_slot", vm::Value::Int(0)), ("generation", vm::Value::Int(0))] {
                assert_eq!(b.get(&vm::Value::string(key)), Some(&value));
            }
            assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
            assert!(m.capture_certification_stats().unwrap().valid);
            m.reset_for_reuse().unwrap();
            assert!(!m.scope_reset_pending());
            assert_eq!(m.host_context().resource_count(), 0);
            assert!(!m.capture_certification_stats().unwrap().valid);
            assert!(pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut m).is_none());
            let script = format!("const v=(0,eval)({});console.log(JSON.stringify({{kind:typeof v,value:v}}));", serde_json::to_string(&source).unwrap());
            let version = std::process::Command::new("timeout").args(["15", "node", "--version"]).output().unwrap();
            let output = std::process::Command::new("timeout").args(["15", "node", "-e", &script]).output().unwrap();
            eprintln!("REL145_LEGACY_NODE {}", serde_json::json!({"source":source,"argv":["timeout","15","node","-e",script],"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr),"version":String::from_utf8_lossy(&version.stdout),"version_rc":version.status.code()}));
            assert!(version.status.success() && output.status.success() && output.stderr.is_empty());
            assert_eq!(serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(), serde_json::json!({"kind":"boolean","value":expected}));
        }).unwrap().join().unwrap();
    }

    for op in [
        "+", "-", "~", "*", "/", "%", "&", "|", "^", "<<", ">>", ">>>",
    ] {
        for operand in ["({})", "[1]", "(function(){})"] {
            for prefix in ["", "'unrelated';"] {
                let expression = if matches!(op, "+" | "-" | "~") {
                    format!("{op}{operand}")
                } else {
                    format!("{operand}{op}2")
                };
                let source = format!("{prefix}{expression};");
                let e = parse(&source).expect_err("proved nonprimitive numeric refusal");
                assert!(e.span.is_some() && format!("{e:?}").contains("numeric"));
                eprintln!("NUM138_REFUSED source={source:?} error={e:?}");
            }
        }
    }
    for source in [
        "function f(x){return x+1;}f(true);",
        "function f(x){return x<1;}f(true);",
        "var x=1;x+=true;",
        "var x=true;x++;",
        "true==1;",
    ] {
        if source == "true==1;" {
            relational_positive(source, true);
            continue;
        }
        if source == "function f(x){return x<1;}f(true);" {
            relational_positive(source, false);
            continue;
        }
        if source == "function f(x){return x+1;}f(true);" {
            let script = format!(
                "const v=(0,eval)({});const b=Buffer.alloc(8);b.writeDoubleBE(v);console.log(JSON.stringify({{kind:typeof v,bits:b.toString('hex')}}));",
                serde_json::to_string(source).unwrap()
            );
            let version = std::process::Command::new("timeout")
                .args(["15", "node", "--version"])
                .output()
                .unwrap();
            let node = std::process::Command::new("timeout")
                .args(["15", "node", "-e", &script])
                .output()
                .unwrap();
            eprintln!(
                "ADD140B_NODE {}",
                serde_json::json!({"source":source,"script":script,"rc":node.status.code(),"stdout":String::from_utf8_lossy(&node.stdout),"stderr":String::from_utf8_lossy(&node.stderr),"version_rc":version.status.code(),"version":String::from_utf8_lossy(&version.stdout)})
            );
            assert!(version.status.success() && node.status.success() && node.stderr.is_empty());
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&node.stdout).unwrap(),
                serde_json::json!({"kind":"number","bits":"4000000000000000"})
            );
            std::thread::Builder::new().stack_size(33_554_432).spawn(move || {
                let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source).unwrap();
                machine.set_jit_config(vm::JitConfig { enabled: false, ..*machine.jit_config() });
                assert!(!machine.jit_config().enabled);
                assert_eq!((machine.aot_exec_count(), machine.jit_native_exec_count()), (0, 0));
                machine.set_fuel(10_000_000);
                let status = machine.run();
                eprintln!("ADD140B_VM {}", serde_json::json!({"source":source,"status":format!("{status:?}"),"stack":format!("{:?}",machine.stack()),"jit_enabled":machine.jit_config().enabled,"aot":machine.aot_exec_count(),"jit":machine.jit_native_exec_count()}));
                assert_eq!(status.unwrap(), vm::VmStatus::Halted);
                let vm::Value::Map(boxed) = machine.stack().last().unwrap() else { panic!("Number box") };
                assert_eq!(boxed.len(), 5);
                assert_eq!(boxed.get(&vm::Value::string("tag")), Some(&vm::Value::Int(3)));
                let vm::Value::Float(value) = boxed.get(&vm::Value::string("number")).unwrap() else { panic!("Float payload") };
                assert_eq!(value.to_bits(), 0x4000_0000_0000_0000);
                assert!(!machine.jit_config().enabled);
                assert_eq!((machine.aot_exec_count(), machine.jit_native_exec_count()), (0, 0));
                assert!(machine.capture_certification_stats().unwrap().valid);
                machine.reset_for_reuse().unwrap();
                if machine.scope_reset_pending() {
                    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
                    assert!(matches!(machine.poll_reset_for_reuse(&mut cx), std::task::Poll::Ready(Ok(()))));
                }
                assert!(!machine.scope_reset_pending());
                assert_eq!(machine.host_context().resource_count(), 0);
                assert!(!machine.capture_certification_stats().unwrap().valid);
                assert!(pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).is_none());
                eprintln!("ADD140B_RESET source={source:?} resources=0 pending=false owned_retired=true cert=false");
            }).unwrap().join().unwrap();
            continue;
        }
        if source == "var x=1;x+=true;" {
            scalar(source, 2.0);
            scalar("var x=1;x+=true;x;", 2.0);
            eprintln!("NUM138_SUPERSEDED source={source:?} result=2 stored=2");
            continue;
        }
        if source == "var x=true;x++;" {
            scalar(source, 1.0);
            scalar("var x=true;x++;x;", 2.0);
            eprintln!("NUM138_SUPERSEDED source={source:?} postfix_converted_result=1 stored=2");
            continue;
        }
        let e = parse(source).expect_err("excluded consumer remains source refused");
        assert!(e.span.is_some());
        eprintln!("NUM138_EXCLUDED_REFUSED source={source:?} error={e:?}");
    }
}
#[test]
fn numeric138_demand_scope_and_reuse() {
    for (source, want) in [
        ("-true;", 0),
        ("null-1;", 0),
        ("~undefined;", 0),
        ("true&false;", 0),
        ("2*3;", 0),
        ("'3'*2;", 1),
        ("function f(x){return x*2;}f(true);", 1),
        (
            "function outer(){function inner(x){return x*2;}return inner('3');}outer();",
            1,
        ),
        ("function f(x){'use strict';return x*2;}f('3');'\\141';", 1),
        ("'use strict';function f(x){return x*2;}f('3');", 1),
        ("Math.abs(-1);'3'*2;", 1),
    ] {
        let ir = parse(source).unwrap_or_else(|e| panic!("{source}: {e:?}"));
        let count = |part: &str| {
            ir.functions
                .iter()
                .filter(|f| f.name.contains(part))
                .count()
        };
        assert_eq!(count("numeric_scan"), want, "{source}");
        assert_eq!(count("round_decimal"), want, "{source}");
        if want == 0 {
            assert!(
                ir.functions
                    .iter()
                    .all(|f| f.name == "__compat_es5_undefined")
            );
        }
        let indices: std::collections::HashSet<_> = ir.functions.iter().map(|f| f.index).collect();
        assert_eq!(indices.len(), ir.functions.len());
        assert!(
            ir.function_impls
                .values()
                .all(|f| f.param_slots.iter().all(|s| usize::from(*s) < ir.locals))
        );
        eprintln!(
            "NUM138_DEMAND source={source:?} parser_count={want} locals={}",
            ir.locals
        );
    }
    scalar(
        "function f(x){'use strict';return x*2;}f('3');'\\141'.length;",
        1.0,
    );
    scalar("'use strict';function f(x){return x*2;}f('3');", 6.0);
    scalar(
        "function f(x){try{throw x;}catch(e){return e*2;}}f('3');",
        6.0,
    );
}

fn visit_expr(e: &mut vm::Expr, f: &mut impl FnMut(&mut vm::Expr)) {
    use vm::Expr as E;
    f(e);
    match e {
        E::Add(a, b)
        | E::Sub(a, b)
        | E::Mul(a, b)
        | E::Div(a, b)
        | E::Mod(a, b)
        | E::Eq(a, b)
        | E::Lt(a, b)
        | E::Gt(a, b)
        | E::And(a, b)
        | E::Or(a, b) => {
            visit_expr(a, f);
            visit_expr(b, f);
        }
        E::Neg(a) | E::Not(a) | E::ToOwned(a) | E::Borrow(a) | E::BorrowMut(a) => visit_expr(a, f),
        E::Call(_, _, args, _, _) | E::LocalCall(_, _, args, _) | E::ModuleCall(_, _, args, _) => {
            for a in args {
                visit_expr(a, f);
            }
        }
        E::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            visit_expr(condition, f);
            visit_expr(then_expr, f);
            visit_expr(else_expr, f);
        }
        E::Block { stmts, expr } => {
            visit_stmts(stmts, f);
            visit_expr(expr, f);
        }
        E::ResultConstruct { payload, .. } => visit_expr(payload, f),
        E::ResultQuery { value, .. } | E::ResultTry { value, .. } => visit_expr(value, f),
        E::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            visit_expr(value, f);
            visit_expr(ok_expr, f);
            visit_expr(err_expr, f);
        }
        E::OptionalGet { container, key, .. } => {
            visit_expr(container, f);
            visit_expr(key, f);
        }
        E::OptionUnwrapOr {
            value, fallback, ..
        } => {
            visit_expr(value, f);
            visit_expr(fallback, f);
        }
        E::Match {
            value,
            arms,
            default,
            ..
        } => {
            visit_expr(value, f);
            for (_, a) in arms {
                visit_expr(a, f);
            }
            visit_expr(default, f);
        }
        E::Closure(c) => visit_expr(&mut c.body, f),
        E::ClosureCall(c, args) => {
            visit_expr(&mut c.body, f);
            for a in args {
                visit_expr(a, f);
            }
        }
        E::Null
        | E::Int(_)
        | E::Float(_)
        | E::Bool(_)
        | E::String(_)
        | E::Bytes(_)
        | E::Var(_)
        | E::MoveVar(_)
        | E::MoveField { .. }
        | E::MoveIndex { .. }
        | E::FunctionRef(..)
        | E::ModuleFunctionRef(..)
        | E::UnresolvedFunctionRef { .. } => {}
    }
}
fn visit_stmts(ss: &mut [vm::Stmt], f: &mut impl FnMut(&mut vm::Expr)) {
    use vm::Stmt as S;
    for s in ss {
        match s {
            S::Let { expr, .. } | S::Assign { expr, .. } | S::Expr { expr, .. } => {
                visit_expr(expr, f)
            }
            S::IfElse {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                visit_expr(condition, f);
                visit_stmts(then_branch, f);
                visit_stmts(else_branch, f);
            }
            S::While {
                condition, body, ..
            } => {
                visit_expr(condition, f);
                visit_stmts(body, f);
            }
            S::For {
                init,
                condition,
                post,
                body,
                ..
            } => {
                visit_stmts(std::slice::from_mut(init), f);
                visit_expr(condition, f);
                visit_stmts(std::slice::from_mut(post), f);
                visit_stmts(body, f);
            }
            S::ClosureLet { closure, .. } => visit_expr(&mut closure.body, f),
            S::Noop { .. }
            | S::FuncDecl { .. }
            | S::Break { .. }
            | S::Continue { .. }
            | S::Drop { .. } => {}
        }
    }
}

struct NegObserver;
impl vm::SourcePlugin for NegObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("primitive135-parser-observer");
        d
    }
    fn parse_source(
        &self,
        source: &str,
        cx: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S};
        let mut ir = vm::SourcePlugin::parse_source(
            &pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN,
            source,
            cx,
        )?;
        let mut count = 0;
        visit_stmts(&mut ir.stmts, &mut |e| {
            if count == 0 && matches!(e, E::Neg(_)) {
                *e = E::Block {
                    stmts: vec![S::Expr {
                        expr: E::Call(
                            vm::BuiltinFunction::Assert.call_index(),
                            vec![],
                            vec![E::Bool(false)],
                            None,
                            None,
                        ),
                        line: 1,
                    }],
                    expr: Box::new(e.clone()),
                };
                count += 1;
            }
        });
        assert_eq!(count, 1);
        Ok(ir)
    }
}
#[test]
fn numeric138_neg_pending_marker() {
    static OBS: NegObserver = NegObserver;
    for (source, normal) in [
        ("-(1);", true),
        (
            "function fail(){throw 'L';}var n=0;try{-(fail(),1);}catch(e){n=e==='L'?1:2;}n;",
            false,
        ),
    ] {
        std::thread::Builder::new()
            .stack_size(33_554_432)
            .spawn(move || {
                let mut machine = probe_vm(source, &OBS, "primitive135-parser-observer");
                let status = machine.run();
                eprintln!(
                    "NUM138_NEG_MARKER_VM source={source:?} normal={normal} status={status:?}"
                );
                if normal {
                    assert!(
                        format!("{:?}", status.expect_err("normal Neg reached"))
                            .contains("assertion failed")
                    );
                } else {
                    assert_eq!(status.unwrap(), vm::VmStatus::Halted);
                    let vm::Value::Map(b) = machine.stack().last().unwrap() else {
                        panic!("Number")
                    };
                    assert_eq!(
                        b.get(&vm::Value::string("number")),
                        Some(&vm::Value::Float(1.0))
                    );
                }
                assert_eq!(
                    (machine.aot_exec_count(), machine.jit_native_exec_count()),
                    (0, 0)
                );
                reset(&mut machine);
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

/// Reachable non-number unary minus, plus dead text after an abrupt completion,
/// must keep the admitted numeric conversion and the selected result.
#[test]
fn numeric138_q3_admitted_unary_and_dead_minus() {
    scalar("-true;", -1.0);
    scalar("-'1';", -1.0);
    scalar("var n=-true;n;", -1.0);
    scalar(
        "function f(){\"use strict\";return 1;var n=-true;}f();",
        1.0,
    );
    scalar("-null;", -0.0);
    scalar("-undefined;", f64::NAN);
    scalar(
        "function f(){\"use strict\";try{return 1;}finally{var n=-true;}}f();",
        1.0,
    );
}
