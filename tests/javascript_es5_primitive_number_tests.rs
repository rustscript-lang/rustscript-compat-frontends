//! Proved primitive unary-plus conversion, with actual VM/Node results.
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
fn oracle(source: &str, expected: f64) {
    let script = format!(
        "(()=>{{let v=(0,eval)({});let b=Buffer.alloc(8);b.writeDoubleBE(v);console.log(Number.isNaN(v)?'NaN':b.toString('hex'));}})();",
        serde_json::to_string(source).unwrap()
    );
    let output = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    eprintln!(
        "PRIM135_NODE {}",
        serde_json::json!({"source":source,"argv":["timeout","15","node","-e",script],"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr)})
    );
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let want = if expected.is_nan() {
        "NaN".to_owned()
    } else {
        format!("{:016x}", expected.to_bits())
    };
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), want);
}
fn scalar(source: &str, expected: f64) {
    scalar_policy(source, expected, true);
}
fn scalar_policy(source: &str, expected: f64, node_common: bool) {
    if node_common {
        oracle(source, expected);
    }
    if let Err(e) = parse(source) {
        assert!(e.span.is_some());
        assert!(format!("{e:?}").contains("numeric"));
        eprintln!("PRIM135_SOURCE_RED source={source:?} guest_invocations=0 error={e:?}");
        panic!("proved primitive conversion must parse: {e:?}");
    }
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut m =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                    .unwrap_or_else(|e| {
                        panic!("VM compile source={source:?} guest_invocations=0: {e:?}")
                    });
            m.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*m.jit_config()
            });
            m.set_fuel(10_000_000);
            let status = m.run();
            eprintln!(
                "PRIM135_VM source={source:?} status={status:?} stack={:?}",
                m.stack()
            );
            assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            let vm::Value::Map(b) = m.stack().last().unwrap() else {
                panic!("Number box")
            };
            assert_eq!(b.len(), 5);
            assert_eq!(b.get(&vm::Value::string("tag")), Some(&vm::Value::Int(3)));
            let vm::Value::Float(n) = b.get(&vm::Value::string("number")).unwrap() else {
                panic!("Float payload")
            };
            assert!(
                if expected.is_nan() {
                    n.is_nan()
                } else {
                    n.to_bits() == expected.to_bits()
                },
                "{source}: {n:?} expected {expected:?}"
            );
            assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
            assert!(m.capture_certification_stats().unwrap().valid);
            m.reset_for_reuse().unwrap();
            if m.scope_reset_pending() {
                let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
                assert!(matches!(
                    m.poll_reset_for_reuse(&mut cx),
                    std::task::Poll::Ready(Ok(()))
                ));
            }
            assert!(!m.scope_reset_pending());
            assert_eq!(m.host_context().resource_count(), 0);
            assert!(!m.capture_certification_stats().unwrap().valid);
            assert!(
                pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut m).is_none()
            );
            eprintln!("PRIM135_RESET resources=0 owned_retired=true cert=false native=0");
        })
        .unwrap()
        .join()
        .unwrap();
}
#[test]
fn primitive135_boolean_red_green() {
    scalar("+true;", 1.0);
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
#[test]
fn primitive135_parser_root_gc() {
    static OBS: ParserObserver = ParserObserver {
        gc: true,
        fatal: false,
    };
    let source = "var k='1.00000000000000011102230246251565404236316680908203126';var a=k;var n=+k;if(k!==a)throw 'alias';n;";
    oracle(source, f64::from_bits(0x3ff0000000000001));
    std::thread::Builder::new().stack_size(33_554_432).spawn(move||{
        let mut m=probe_vm(source,&OBS,"primitive135-parser-observer");let status=m.run();
        assert_eq!(status.unwrap(),vm::VmStatus::Halted);
        let vm::Value::Map(b)=m.stack().last().unwrap()else{panic!("Number")};
        let vm::Value::Float(n)=b.get(&vm::Value::string("number")).unwrap()else{panic!("Float")};
        assert_eq!(n.to_bits(),0x3ff0000000000001);
        eprintln!("PRIM135_GC_RESULT source={source:?} status=NORMAL guest_input_root_row=true actual_collect_counter_increased=true checked_generation_live=true alias_preserved=true bits={:016x}",n.to_bits());
        assert_eq!((m.aot_exec_count(),m.jit_native_exec_count()),(0,0));reset(&mut m);
    }).unwrap().join().unwrap();
}
#[test]
fn primitive135_parser_entry_abrupt_skip() {
    static OBS: ParserObserver = ParserObserver {
        gc: false,
        fatal: true,
    };
    for (source, normal) in [
        ("+'1';", true),
        (
            "function fail(){throw 'X';}var n=0;try{+(fail(),'1');n=99;}catch(e){n=e==='X'?1:2;}finally{n=n+1;}n;",
            false,
        ),
    ] {
        std::thread::Builder::new().stack_size(33_554_432).spawn(move||{
            let mut m=probe_vm(source,&OBS,"primitive135-parser-observer");let status=m.run();
            if normal{assert!(format!("{:?}",status.expect_err("entered conversion marker")).contains("assertion failed"));}
            else{assert_eq!(status.unwrap(),vm::VmStatus::Halted);let vm::Value::Map(b)=m.stack().last().unwrap()else{panic!("Number")};assert_eq!(b.get(&vm::Value::string("number")),Some(&vm::Value::Float(2.0)));oracle(source,2.0);}
            eprintln!("PRIM135_ENTRY_RESULT source={source:?} status={} conversion_marker_reached={normal}",if normal{"FATAL"}else{"NORMAL"});
            assert_eq!((m.aot_exec_count(),m.jit_native_exec_count()),(0,0));reset(&mut m);
        }).unwrap().join().unwrap();
    }
}

#[path = "javascript_es5/object_observer.rs"]
mod object_observer;
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

fn oracle_units(source: &str, expected: serde_json::Value) {
    let script = format!(
        "const source={};try{{let v=(0,eval)(source);console.log(JSON.stringify(typeof v==='string'?{{kind:'string',units:Array.from({{length:v.length}},(_,i)=>v.charCodeAt(i))}}:{{kind:typeof v,value:v}}));}}catch(e){{console.log(JSON.stringify({{kind:'throw',units:Array.from({{length:e.length}},(_,i)=>e.charCodeAt(i))}}));}}",
        serde_json::to_string(source).unwrap()
    );
    let v = std::process::Command::new("node")
        .arg("--version")
        .output()
        .unwrap();
    let o = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    eprintln!(
        "PRIM135_NODE {}",
        serde_json::json!({"source":source,"source_utf16":source.encode_utf16().collect::<Vec<_>>(),"argv":["timeout","15","node","-e",script],"rc":o.status.code(),"stdout":String::from_utf8_lossy(&o.stdout),"stderr":String::from_utf8_lossy(&o.stderr),"version_rc":v.status.code(),"version_stdout":String::from_utf8_lossy(&v.stdout),"version_stderr":String::from_utf8_lossy(&v.stderr)})
    );
    assert!(v.status.success() && o.status.success() && o.stderr.is_empty());
    let actual: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn primitive135_raw_original_and_file() {
    const BODY: &str = include_str!("fixtures/test262135/S11.4.6_A3_T4.js");
    assert!(include_str!("fixtures/test262135/LICENSE").contains("Copyright"));
    let provenance: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/test262135/upstream.json")).unwrap();
    assert_eq!(
        provenance["commit"],
        "7da91bceb9ce7613f87db47ddd1292a2dda58b42"
    );
    for (id, body, expected) in [
        ("original", BODY.to_owned(), None),
        (
            "sentinel",
            format!("{BODY}\n$ERROR('S\\0\\ud800');"),
            Some(vec![83, 0, 0xd800]),
        ),
        (
            "mutant",
            BODY.replacen("+null !== 0", "+null !== 1", 1),
            Some(
                "#2: +null === 0. Actual: 0"
                    .encode_utf16()
                    .collect::<Vec<_>>(),
            ),
        ),
    ] {
        let source = format!("function $ERROR(message){{'use strict';throw message;}}\n{body}\n1;");
        let expected_node = expected.as_ref().map_or_else(
            || serde_json::json!({"kind":"number","value":1}),
            |units| serde_json::json!({"kind":"throw","units":units}),
        );
        oracle_units(&source, expected_node);
        if expected.is_none() {
            scalar(&source, 1.0);
        }
        let copy = source.clone();
        std::thread::Builder::new()
            .stack_size(33_554_432)
            .spawn(move || {
                let path =
                    std::env::temp_dir().join(format!("prim135-{id}-{}.js", std::process::id()));
                std::fs::write(&path, &copy).unwrap();
                let result =
                    pd_vm_compat_frontends::javascript::corpus::observe_javascript_es5_corpus_file(
                        &path,
                    );
                eprintln!("PRIM135_FILE id={id} source={copy:?} path={path:?} result={result:?}");
                if let Some(units) = expected {
                    let text = format!("{:?}", result.expect_err("uncaught ERROR"));
                    assert!(text.contains("uncaught JavaScript throw") && !text.contains("Fuel"));
                    let observer = Box::leak(Box::new(object_observer::Observer {
                        units: Some(units.len()),
                        force_gc: false,
                        uncaught: true,
                        type_error: false,
                        realm_header: false,
                        collect_phases: false,
                        collect_stop: None,
                    }));
                    let mut m = probe_vm(&copy, observer, "javascript-es5-object-observer");
                    let status = m.run();
                    assert!(
                        format!("{:?}", status.expect_err("managed throw"))
                            .contains("uncaught JavaScript throw")
                    );
                    let at = m
                        .stack()
                        .iter()
                        .position(|v| *v == vm::Value::string("P7_OBSERVED"))
                        .unwrap();
                    assert_eq!(m.stack()[at + 2], vm::Value::Float(units.len() as f64));
                    for (i, u) in units.iter().enumerate() {
                        assert_eq!(m.stack()[at + 3 + i], vm::Value::Float(f64::from(*u)));
                    }
                    eprintln!(
                        "PRIM135_FILE_THROW id={id} status=UNCAUGHT units={units:?} stack={:?}",
                        m.stack()
                    );
                    assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
                    reset(&mut m);
                } else {
                    let o = result.unwrap();
                    assert_eq!(o.cookie, 1);
                    assert_eq!((o.aot_exec_count, o.jit_native_exec_count), (0, 0));
                    assert!(
                        o.certification_valid_before_reset && !o.certification_valid_after_reset
                    );
                    assert_eq!(o.resources_after_reset, 0);
                    assert!(o.owned_arrays_retired);
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
#[test]
fn primitive135_actual_dynamic_guards() {
    for source in [
        "function f(){return '1';}+f();",
        "var o={n:'1'};+o.n;",
        "function f(){return true;}+f();",
    ] {
        scalar(source, 1.0);
    }
    for source in [
        "function f(){return {};}+f();",
        "function f(){return [1];}+f();",
        "function f(){return function(){};}+f();",
    ] {
        let source = source.to_owned();
        std::thread::Builder::new()
            .stack_size(33_554_432)
            .spawn(move || {
                let mut m =
                    pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                        .unwrap();
                m.set_jit_config(vm::JitConfig {
                    enabled: false,
                    ..*m.jit_config()
                });
                m.set_fuel(10_000_000);
                let status = m.run();
                eprintln!("PRIM135_FATAL source={source:?} status={status:?}");
                assert!(
                    format!("{:?}", status.expect_err("actual Number fatal guard"))
                        .contains("assertion failed")
                );
                assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
                reset(&mut m);
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

#[test]
fn primitive135_scalar_factflow() {
    for (source, expected) in [
        ("+false;", 0.0),
        ("var b=true;+b;", 1.0),
        ("var b=false;+(b=true);", 1.0),
        ("+(true?null:null);", 0.0),
        ("+(true?false:true);", 0.0),
        ("+(1,void 2);", f64::NAN),
        (
            "function f(){var undefined='12';return +undefined;}f();",
            12.0,
        ),
        ("var x=1;void(x='12');+x;", 12.0),
        ("function f(){return +true;}f();", 1.0),
        ("function f(){'use strict';return +'12';}f();", 12.0),
        ("'use strict';function f(){var s='13';return +s;}f();", 13.0),
        (
            "function f(){try{throw 'x';}catch(e){var s='14';return +s;}}f();",
            14.0,
        ),
        (
            "function f(){try{return +'15';}finally{var t='ignored';}}f();",
            15.0,
        ),
        ("var n=0;var v=+(n=n+1,'16');v+n;", 17.0),
        (
            "var o={n:0,get x(){'use strict';this.n=this.n+1;return 3;}};var v=+(o.x,'17');v+o.n;",
            18.0,
        ),
        (
            "var n=0;function fail(){throw 'X';}try{+(fail(),'19');n=99;}catch(e){n=e==='X'?1:2;}finally{n=n+1;}n;",
            2.0,
        ),
        (
            "function f(){function fail(){throw 'X';}try{return +(fail(),'19');}finally{return +true;}}f();",
            1.0,
        ),
        ("(+true)+(+'2')+(+null);", 3.0),
        ("(+'5')%2;", 1.0),
        ("(+'5')&3;", 1.0),
        ("~(+'5');", -6.0),
        ("(+'5')<<2;", 20.0),
        ("var n=+'2';(n<3)?1:0;", 1.0),
    ] {
        scalar(source, expected);
    }
}
#[test]
fn primitive135_string_grammar_common() {
    for (text, expected) in [
        ("", 0.0),
        (" ", 0.0),
        ("0", 0.0),
        ("-0", -0.0),
        ("+0", 0.0),
        ("  -0\t", -0.0),
        ("1", 1.0),
        ("+1", 1.0),
        ("-1", -1.0),
        (".5", 0.5),
        ("1.", 1.0),
        ("1.e2", 100.0),
        (".5e+2", 50.0),
        ("00012.5e-1", 1.25),
        ("Infinity", f64::INFINITY),
        ("+Infinity", f64::INFINITY),
        ("-Infinity", f64::NEG_INFINITY),
        ("0x10", 16.0),
        ("0Xff", 255.0),
        ("0x10000000000000000", 18446744073709551616.0),
        ("+0x1", f64::NAN),
        ("-0x1", f64::NAN),
        ("0x", f64::NAN),
        ("0x1g", f64::NAN),
        ("NaN", f64::NAN),
        ("undefined", f64::NAN),
        ("null", f64::NAN),
        ("true", f64::NAN),
        ("1x", f64::NAN),
        ("1 2", f64::NAN),
        ("1e", f64::NAN),
        ("1e+", f64::NAN),
        (".", f64::NAN),
        ("+", f64::NAN),
        ("--1", f64::NAN),
        ("inf", f64::NAN),
        ("infinity", f64::NAN),
        ("1\0", f64::NAN),
        ("1e309", f64::INFINITY),
        ("-1e309", f64::NEG_INFINITY),
        ("1e-999", 0.0),
        ("-1e-999", -0.0),
        ("5e-324", f64::from_bits(1)),
        ("2.4703282292062327e-324", 0.0),
        ("2.4703282292062328e-324", f64::from_bits(1)),
        (
            "1.00000000000000011102230246251565404236316680908203125",
            1.0,
        ),
        (
            "1.00000000000000011102230246251565404236316680908203126",
            f64::from_bits(0x3ff0000000000001),
        ),
        ("9007199254740993", 9007199254740992.0),
        (
            "1234567890123456789012345678901234567890",
            1.2345678901234568e39,
        ),
        ("1.7976931348623157e308", f64::MAX),
    ] {
        let literal = serde_json::to_string(text).unwrap();
        scalar(&format!("+{literal};"), expected);
    }
    for unit in [
        9, 10, 11, 12, 13, 32, 160, 5760, 8192, 8193, 8194, 8195, 8196, 8197, 8198, 8199, 8200,
        8201, 8202, 8232, 8233, 8239, 12288, 65279,
    ] {
        scalar(&format!("+'\\u{unit:04x}42\\u{unit:04x}';"), 42.0);
    }
}
#[test]
fn primitive135_es5_radix_policy() {
    for (source, node_expected) in [("+'0b10';", 2.0), ("+'0o10';", 8.0)] {
        oracle(source, node_expected);
        scalar_policy(source, f64::NAN, false);
    }
}
#[test]
fn primitive135_parser_demand_reuse() {
    for source in [
        "+'1';",
        "var s='2';+s;",
        "if(false){+'3';}",
        "function f(){return +'4';}f();",
        "function outer(){function inner(){return +'5';}return inner();}outer();",
    ] {
        let ir = parse(source).unwrap();
        let names: Vec<_> = ir.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names.iter().filter(|n| n.contains("numeric_scan")).count(),
            1,
            "{source}: {names:?}"
        );
        assert_eq!(
            names.iter().filter(|n| n.contains("round_decimal")).count(),
            1,
            "{source}: {names:?}"
        );
        assert!(
            !names
                .iter()
                .any(|n| n.contains("numeric_call") || n.contains("numeric_primitive")),
            "{source}: {names:?}"
        );
        let mut indices = std::collections::HashSet::new();
        assert!(
            ir.functions.iter().all(|f| indices.insert(f.index)),
            "{source}"
        );
        for f in ir.function_impls.values() {
            assert!(f.param_slots.iter().all(|s| usize::from(*s) < ir.locals));
        }
        eprintln!(
            "PRIM135_DEMAND source={source:?} names={names:?} locals={}",
            ir.locals
        );
    }
    for source in [
        "Math.abs(-1);+'2';",
        "isNaN(+undefined);+'3';",
        "function f(){return +'4';}Math.abs(-1);f();",
    ] {
        let ir = parse(source).unwrap();
        let names: Vec<_> = ir.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names.iter().filter(|n| n.contains("numeric_scan")).count(),
            1,
            "{source}: {names:?}"
        );
        assert_eq!(
            names.iter().filter(|n| n.contains("round_decimal")).count(),
            1,
            "{source}: {names:?}"
        );
        assert_eq!(
            names.iter().filter(|n| n.contains("numeric_call")).count(),
            1,
            "{source}: {names:?}"
        );
        eprintln!("PRIM135_REUSE source={source:?} names={names:?}");
    }
}
#[test]
fn primitive135_numeric_boundaries() {
    for source in [
        "function f(x){return +x;}f('1');",
        "function f(){try{throw '1';}catch(e){return +e;}}f();",
        "+(true?'1':1);",
        "+({});",
        "+[];",
        "-'1';",
        "~'1';",
        "'1'%2;",
        "'1'&2;",
    ] {
        for prefix in ["", "'unrelated';"] {
            let source = format!("{prefix}{source}");
            let expected = match source.strip_prefix(prefix).unwrap() {
                "function f(x){return +x;}f('1');"
                | "function f(){try{throw '1';}catch(e){return +e;}}f();"
                | "+(true?'1':1);"
                | "'1'%2;" => Some(1.0),
                "-'1';" => Some(-1.0),
                "~'1';" => Some(-2.0),
                "'1'&2;" => Some(0.0),
                _ => None,
            };
            if let Some(expected) = expected {
                scalar(&source, expected);
                continue;
            }
            let e = parse(&source).expect_err("nonprimitive numeric source refusal");
            assert_eq!(
                e.message,
                "Primitive numeric conversion requires numeric primitive operands"
            );
            let span = e.span.expect("source refusal span");
            let width = if source.ends_with("+({});") { 5 } else { 3 };
            assert_eq!(
                (span.source_id, span.lo, span.hi),
                (0, prefix.len(), prefix.len() + width)
            );
            eprintln!(
                "PRIM135_REFUSED class=SourceRefused source={source:?} guest_invocations=0 error={e:?}"
            );
        }
    }
}

#[test]
fn primitive135_string_red_green() {
    scalar("+'1';", 1.0);
}
#[test]
fn primitive135_null_red_green() {
    scalar("+null;", 0.0);
}
#[test]
fn primitive135_undefined_red_green() {
    scalar("+undefined;", f64::NAN);
}
#[test]
fn primitive135_legacy_whitespace_policy() {
    // Parent amendment135B corrects the old U+180E -> +0 premise explicitly:
    // ES5.1 7.2 requires Unicode3.0 WhiteSpace; 9.3.1 rejects invalid syntax.
    // UnicodeData3.0: U+180E is Cf, U+200B is Zs; FEFF is explicit BOM.
    // Keep the original one-JS-escape source; it never denoted backslash-u text.
    scalar("+'\\u180e';", f64::NAN);
    scalar("+'\u{180e}';", f64::NAN);
    scalar("+'\\ufeff';", 0.0);
    scalar("+'\u{feff}';", 0.0);
    // Genuine Unicode3 legacy control: modern Node (and pinned Micro evidence)
    // returns NaN for U+200B, while this unchanged ES5 parser returns +0.
    for source in ["+'\\u200b';", "+'\u{200b}';"] {
        oracle(source, f64::NAN);
        scalar_policy(source, 0.0, false);
    }
    // Escaped and literal codepoints each make exactly one guest UTF16 unit.
    // A doubled JS backslash instead makes six units and invalid numeric text.
    for source in [
        "'\\u180e'.length;",
        "'\u{180e}'.length;",
        "'\\u200b'.length;",
        "'\u{200b}'.length;",
        "'\\ufeff'.length;",
        "'\u{feff}'.length;",
    ] {
        scalar(source, 1.0);
    }
    scalar("'\\\\u200b'.length;", 6.0);
    scalar("+'\\\\u200b';", f64::NAN);
}
#[test]
fn primitive135_no_helper_for_scalar() {
    for source in ["+true;", "+false;", "+null;", "+undefined;", "+1;"] {
        let ir = parse(source).unwrap();
        assert!(
            ir.functions
                .iter()
                .all(|f| f.name == "__compat_es5_undefined"),
            "{source}"
        );
    }
}
