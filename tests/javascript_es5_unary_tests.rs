//! Bounded unary operations through the actual public VM.
fn boolean(source: &str, expected: bool) {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!(
                "UN129_SOURCE {}",
                serde_json::json!({"source":source,"expected":expected})
            );
            let mut machine =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                    .unwrap_or_else(|error| {
                        panic!("source={source:?} guest_invocations=0 {error:?}")
                    });
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let status = machine.run();
            eprintln!("UN129_VM status={status:?} stack={:?}", machine.stack());
            assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            let vm::Value::Map(map) = machine.stack().last().unwrap() else {
                panic!("boxed Boolean")
            };
            assert_eq!(map.len(), 5);
            for (name, value) in [
                ("tag", vm::Value::Int(2)),
                ("number", vm::Value::Float(0.0)),
                ("flag", vm::Value::Bool(expected)),
                ("ref_slot", vm::Value::Int(0)),
                ("generation", vm::Value::Int(0)),
            ] {
                assert_eq!(map.get(&vm::Value::string(name)), Some(&value), "{source}");
            }
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
            eprintln!("UN129_RESET resources=0 owned_retired=true cert=false native=0");
            oracle(
                &source,
                serde_json::json!({"kind":"boolean","value":expected}),
            );
        })
        .unwrap()
        .join()
        .unwrap();
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
fn oracle(source: &str, expected: serde_json::Value) {
    let script = format!(
        "const source={};try{{let v=(0,eval)(source);console.log(JSON.stringify({{kind:typeof v,value:v}}));}}catch(e){{console.log(JSON.stringify({{kind:'throw',value:String(e)}}));}}",
        serde_json::to_string(source).unwrap()
    );
    let version = std::process::Command::new("timeout")
        .args(["15", "node", "--version"])
        .output()
        .unwrap();
    assert!(version.status.success());
    let output = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    eprintln!(
        "UN129_NODE {}",
        serde_json::json!({"source":source,"source_utf16":source.encode_utf16().collect::<Vec<_>>(),"argv":["timeout","15","node","-e",script],"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr),"version_rc":version.status.code(),"version_stdout":String::from_utf8_lossy(&version.stdout),"version_stderr":String::from_utf8_lossy(&version.stderr)})
    );
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        expected
    );
}
#[test]
fn unary129_void_red_green() {
    scalar("void 0;", 0, 0.0);
}
#[test]
fn unary129_plus_red_green() {
    scalar("+1;", 3, 1.0);
}
fn scalar(source: &str, tag: i64, expected: f64) {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!(
                "UN129_SOURCE {}",
                serde_json::json!({"source":source,"tag":tag,"expected":expected})
            );
            let mut m =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                    .unwrap_or_else(|e| panic!("source={source:?} guest_invocations=0 {e:?}"));
            m.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*m.jit_config()
            });
            m.set_fuel(10_000_000);
            let status = m.run();
            eprintln!("UN129_VM status={status:?} stack={:?}", m.stack());
            assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            let vm::Value::Map(b) = m.stack().last().unwrap() else {
                panic!("box")
            };
            assert_eq!(b.len(), 5);
            assert_eq!(b.get(&vm::Value::string("tag")), Some(&vm::Value::Int(tag)));
            let vm::Value::Float(n) = b.get(&vm::Value::string("number")).unwrap() else {
                panic!("number")
            };
            assert!(
                if expected.is_nan() {
                    n.is_nan()
                } else {
                    n.to_bits() == expected.to_bits()
                },
                "{source}: {n:?}"
            );
            for (k, v) in [
                ("flag", vm::Value::Bool(false)),
                ("ref_slot", vm::Value::Int(0)),
                ("generation", vm::Value::Int(0)),
            ] {
                assert_eq!(b.get(&vm::Value::string(k)), Some(&v));
            }
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
            assert_eq!(m.host_context().resource_count(), 0);
            assert!(!m.capture_certification_stats().unwrap().valid);
            assert!(
                pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut m).is_none()
            );
            eprintln!("UN129_RESET resources=0 owned_retired=true cert=false native=0");
            oracle_scalar(&source, tag, expected);
        })
        .unwrap()
        .join()
        .unwrap();
}
#[test]
fn unary129_ieee_numbers() {
    for (s, n) in [
        ("+0;", 0.0),
        ("+(-0);", -0.0),
        ("+1.25;", 1.25),
        ("+(-2.5);", -2.5),
        ("+(0/0);", f64::NAN),
        ("+(1/0);", f64::INFINITY),
        ("+(-1/0);", f64::NEG_INFINITY),
        ("+NaN;", f64::NAN),
        ("+Infinity;", f64::INFINITY),
    ] {
        scalar(s, 3, n);
    }
}
#[test]
fn unary129_void_kinds() {
    for s in [
        "void undefined;",
        "void null;",
        "void true;",
        "void -0;",
        "void 'a\\0\\ud800';",
        "void ({});",
        "void [1,2];",
        "void function(){};",
        "void (true?'a':'b');",
        "void (false?1:'a');",
        "void (0,(true?'a':'b'));",
        "void (true?0:(0,'b'));",
    ] {
        scalar(s, 0, 0.0);
    }
}
#[test]
fn unary129_dynamic_scopes() {
    for s in [
        "function f(x){return void x;}f('a')===undefined;",
        "function f(x){'use strict';return void x;}f({})===undefined;",
        "'use strict';function f(x){return void x;}f([1])===undefined;",
        "function f(x){try{throw x;}catch(e){return void e;}}f('a')===undefined;",
        "function f(){return +1;}f()===1;",
        "'use strict';function f(){return +2;}f()===2;",
    ] {
        boolean(s, true);
    }
}
#[test]
fn unary129_factflow_loops_nested() {
    for s in [
        "var x=1;void(x=2);+x===2;",
        "var x='a';void(x=2);+x===2;",
        "var x=0;var n=+(x=1,x+1);n===2&&x===1;",
        "var x=1;var n=+(true?x:2);n===1;",
        "var a=[1];var x=void a;x===undefined;",
        "var x=0;while(x<3){void(x=x+1);}for(var j=0;j<2;j=j+1){+j;}x===3&&j===2;",
        "void +(+(1));",
        "+(true?1:2)===1&&(void(void 1))===undefined;",
        "if(false){var x=+1;}1===1;",
    ] {
        if s == "void +(+(1));" {
            scalar(s, 0, 0.0);
        } else {
            boolean(s, true);
        }
    }
    scalar("if(false){var x=+1;}", 0, 0.0);
}
#[test]
fn unary129_once_getvalue_order() {
    for s in [
        "var n=0;void(n=n+1);n===1;",
        "var n=0;var v=+(n=n+1);n===1&&v===1;",
        "var o={n:0,get x(){'use strict';this.n=this.n+1;return 'discard';}};void o.x;o.n===1;",
        "var o={n:0,get x(){'use strict';this.n=this.n+1;return 7;}};var v=+o.x;o.n===1&&v===7;",
        "function f(o){o.n=o.n+1;return 7;}var o={n:0};var v=+f(o);void f(o);o.n===2&&v===7;",
        "function f(a,b,c){return a===1&&b===undefined&&c===3;}var n=0;var ok=f(+(n=n*10+1),void(n=n*10+2),+(n=n*10+3,n===123?3:0));ok&&n===123;",
        "var o={n:0,valueOf:function(){'use strict';this.n=99;return 1;}};void o;o.n===0;",
    ] {
        boolean(s, true);
    }
}
#[test]
fn unary129_guarded_numbers() {
    for s in [
        "function f(){return -0;}1/(+f())===-Infinity;",
        "var o={n:2};+o.n===2;",
        "+Math.abs(-2)===2;",
        "var a=Math.abs;+a(-2)===2;",
        "+('discard',1)===1;",
    ] {
        boolean(s, true);
    }
    let source =
        "function f(){return 'x';}var caught=false;try{+f();}catch(e){caught=true;}caught;";
    boolean(source, false);
    let source = "function f(){return {};}var caught=false;try{+f();}catch(e){caught=true;}caught;";
    worker(source, |m| {
        let status = m.run();
        eprintln!("UN129_FATAL_GUARD status={status:?}");
        assert!(
            format!("{:?}", status.expect_err("actual Number guard fatal"))
                .contains("assertion failed")
        );
    });
}
#[test]
fn unary129_source_refusals() {
    // ES5.1 12.5 returns Empty for a false If without else; 12.1 step 5
    // retains the preceding String. Modern Node instead returns Undefined.
    fn retained_string(source: &str) {
        static OBS: object_observer::Observer = object_observer::Observer {
            units: Some(9),
            force_gc: true,
            uncaught: false,
            type_error: false,
            realm_header: false,
            collect_phases: false,
            collect_stop: None,
        };
        let source = source.to_owned();
        std::thread::Builder::new().stack_size(33_554_432).spawn(move || {
            let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
            let issued = vm::compile_source_with_flavor_and_options_with_capture_certification(
                &source, vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-object-observer")),
                vm::CompileSourceFileOptions::new().with_host_api_catalog(catalog.clone()).with_source_plugin(&OBS),
            ).unwrap();
            let mut m = issued.into_vm().unwrap();
            m.set_jit_config(vm::JitConfig { enabled: false, ..*m.jit_config() });
            assert!(!m.jit_config().enabled);
            let before = (m.aot_exec_count(), m.jit_native_exec_count());
            assert_eq!(before, (0, 0));
            pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(&mut m, Default::default()).unwrap();
            // Match the public JS CLI's registry, including the private parser
            // builtins demanded by a validated String operand in a skipped arm.
            let mut registry = vm::HostFunctionRegistry::new();
            for module in [
                pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
                pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
                pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
                pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
                pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module(),
            ] { module.install_from_catalog(&mut registry, &catalog).unwrap(); }
            registry.bind_vm_cached(&mut m).unwrap();
            m.set_fuel(10_000_000);
            let status = m.run();
            let after = (m.aot_exec_count(), m.jit_native_exec_count());
            assert_eq!(after, (0, 0));
            assert!(!m.jit_config().enabled);
            eprintln!("TRANS138B_COMPLETION_VM source={source:?} status={status:?} before={before:?} after={after:?} stack={:?}", m.stack());
            assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            let at = m.stack().iter().position(|v| *v == vm::Value::string("P7_OBSERVED")).unwrap();
            let vm::Value::Map(boxed) = &m.stack()[at + 1] else { panic!("String box") };
            assert_eq!(boxed.get(&vm::Value::string("tag")), Some(&vm::Value::Int(4)));
            let units: Vec<_> = "unrelated".encode_utf16().collect();
            assert_eq!(m.stack()[at + 2], vm::Value::Float(units.len() as f64));
            for (i, unit) in units.iter().enumerate() {
                assert_eq!(m.stack()[at + 3 + i], vm::Value::Float(f64::from(*unit)));
            }
            reset(&mut m);
            oracle_units(&source, serde_json::json!({"kind":"undefined"}));
            eprintln!("LEGACY138B_COMPLETION_POLICY source={source:?} es51_string_units={units:?} modern_node=undefined rule=12.5-12.1");
        }).unwrap().join().unwrap();
    }

    for s in [
        "+true;",
        "+(void 0);",
        "void (+true);",
        "+'1';",
        "+null;",
        "+undefined;",
        "+({});",
        "+[];",
        "function f(x){return +x;}f(1);",
        "function f(){try{throw 1;}catch(e){return +e;}}f();",
        "var x=true?'a':1;+x;",
        "var x=1;void(x='a');+x;",
        "var x=1;while(x<2){void(x='a');}+x;",
        "if(false){var x=+true;}",
        "void missing;",
        "if(false){void missing;}",
        "void (1%true);",
        "if(false){void (1%true);}",
        "void (new Object());",
        // Same-context object refusals replace the promoted primitive families.
        "+({x:true});",
        "+(void 0,{});",
        "void (+({}));",
        "+({s:'1'});",
        "+({n:null});",
        "+({u:undefined});",
        "var x=1;void(x={});+x;",
        "if(false){var x=+({});}",
    ] {
        for prefix in ["", "'unrelated';"] {
            let source = format!("{prefix}{s}");
            if !prefix.is_empty() && s == "if(false){void (1%true);}" {
                retained_string(&source);
                continue;
            }
            // Preserve exact former primitive refusals as public VM/Node
            // positives. Other unary refusals and unrelated consumers stay unchanged.
            let positive = match s {
                "+true;" => Some((3, 1.0)),
                "+(void 0);"
                | "+undefined;"
                | "var x=1;void(x='a');+x;"
                | "var x=true?'a':1;+x;"
                | "var x=1;while(x<2){void(x='a');}+x;" => Some((3, f64::NAN)),
                "void (+true);" | "void (1%true);" | "if(false){void (1%true);}" => Some((0, 0.0)),
                "+'1';"
                | "function f(x){return +x;}f(1);"
                | "function f(){try{throw 1;}catch(e){return +e;}}f();" => Some((3, 1.0)),
                "+null;" => Some((3, 0.0)),
                _ => None,
            };
            if let Some((tag, number)) = positive {
                scalar(&source, tag, number);
                continue;
            }
            if s == "if(false){var x=+true;}" {
                parse(&source).expect("dead proved Boolean unary plus");
                if prefix.is_empty() {
                    scalar(&source, 0, 0.0);
                } else {
                    retained_string(&source);
                }
                // Separate binding-state companion, not the exact original.
                boolean(&format!("{source};x===undefined;"), true);
                continue;
            }
            let e = parse(&source).expect_err("valid source bounded refusal");
            eprintln!("UN129_REFUSED source={source:?} guest_invocations=0 error={e:?}");
            assert!(e.span.is_some());
            if s.starts_with('+')
                || s.contains("+x")
                || s.contains("+e")
                || s.contains("+true")
                || s.contains("%true")
            {
                assert!(format!("{e:?}").contains("numeric"));
            }
        }
    }
}
#[test]
fn unary129_abrupt_finally() {
    for op in ["void ", "+"] {
        for body in [
            "function fail(){throw 'L';}var n=0;var ok=false;try{OPfail();n=99;}catch(e){ok=e==='L';}finally{n=n+1;}ok&&n===1;",
            "var o={get x(){throw 'G';}};var ok=false;try{OPo.x;}catch(e){ok=e==='G';}ok;",
            "function f(){function fail(){throw 'L';}try{return OPfail();}finally{return 7;}}f()===7;",
            "function f(){function fail(){throw 'L';}try{return OPfail();}catch(e){return e==='L';}finally{var t='a'+'b';}}f();",
            "function f(){try{return OP1;}finally{throw 'F';}}var ok=false;try{f();}catch(e){ok=e==='F';}ok;",
        ] {
            boolean(&body.replace("OP", op), true);
        }
    }
}
#[test]
fn unary129_scalar_ir_no_placeholder() {
    let ir = parse("+1;").unwrap();
    assert!(ir.functions.is_empty());
    let debug = format!("{:?}", ir.stmts);
    let needle = format!("Call({}, [], []", vm::BuiltinFunction::MapNew.call_index());
    // One operand box and one result box; no normal-only placeholder.
    assert_eq!(debug.matches(&needle).count(), 2, "{debug}");
    let void_ir = parse("void 0;").unwrap();
    let undefined_ir = parse("undefined;").unwrap();
    assert_eq!(
        format!("{:?}", void_ir.functions),
        format!("{:?}", undefined_ir.functions)
    );
    assert!(
        void_ir
            .functions
            .iter()
            .all(|f| f.name == "__compat_es5_undefined")
    );
    let a = parse("var n=1;+n;").unwrap();
    let b = parse("var n=1;n;").unwrap();
    assert_eq!(format!("{:?}", a.functions), format!("{:?}", b.functions));
}
fn worker(source: &str, f: fn(&mut vm::Vm)) {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!("UN129_WORKER source={source:?}");
            let mut m =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                    .unwrap();
            m.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*m.jit_config()
            });
            m.set_fuel(10_000_000);
            f(&mut m);
            assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
            reset(&mut m);
        })
        .unwrap()
        .join()
        .unwrap();
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

#[test]
fn unary129_managed_throw_finally_gc() {
    static OBS: object_observer::Observer = object_observer::Observer {
        units: Some(3),
        force_gc: true,
        uncaught: true,
        type_error: false,
        realm_header: false,
        collect_phases: false,
        collect_stop: None,
    };
    let source = "function f(){throw 'P\\0\\ud800';}try{+f();}finally{var t='garbage'+'!';}";
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut m = probe_vm(source, &OBS, "javascript-es5-object-observer");
            let text = format!("{:?}", m.run().expect_err("uncaught payload"));
            assert!(text.contains("uncaught JavaScript throw"));
            let at = m
                .stack()
                .iter()
                .position(|v| *v == vm::Value::string("P7_OBSERVED"))
                .unwrap();
            assert_eq!(m.stack()[at + 2], vm::Value::Float(3.0));
            for (i, u) in [80, 0, 0xd800].iter().enumerate() {
                assert_eq!(m.stack()[at + 3 + i], vm::Value::Float(f64::from(*u)));
            }
            eprintln!(
                "UN129_THROW_GC source={source:?} units=[80,0,55296] stack={:?}",
                m.stack()
            );
            assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
            reset(&mut m);
            oracle_units_completion(source, Some([80, 0, 0xd800]));
        })
        .unwrap()
        .join()
        .unwrap();
}
#[test]
fn unary129_public_file_controls() {
    const BODY: &str = "var a=+1;var b=void 'a';if(a!==1||b!==undefined)$ERROR('M\\0\\udc00');";
    static OBS: object_observer::Observer = object_observer::Observer {
        units: Some(3),
        force_gc: false,
        uncaught: true,
        type_error: false,
        realm_header: false,
        collect_phases: false,
        collect_stop: None,
    };
    for (id, body, expected) in [
        ("positive", BODY.to_owned(), None),
        (
            "sentinel",
            format!("{BODY}$ERROR('S\\0\\ud800');"),
            Some([83, 0, 0xd800]),
        ),
        (
            "mutant",
            BODY.replacen("a!==1", "a!==2", 1),
            Some([77, 0, 0xdc00]),
        ),
    ] {
        let source = format!("function $ERROR(message){{throw message;}}{body}1;");
        let copy = source.clone();
        std::thread::Builder::new()
            .stack_size(33_554_432)
            .spawn(move || {
                let path =
                    std::env::temp_dir().join(format!("un129-{id}-{}.js", std::process::id()));
                std::fs::write(&path, &copy).unwrap();
                let result =
                    pd_vm_compat_frontends::javascript::corpus::observe_javascript_es5_corpus_file(
                        &path,
                    );
                eprintln!("UN129_FILE id={id} source={copy:?} result={result:?} path={path:?}");
                if let Some(units) = expected {
                    let text = format!("{:?}", result.expect_err("guest throw"));
                    assert!(text.contains("uncaught JavaScript throw") && !text.contains("Fuel"));
                    let mut m = probe_vm(&copy, &OBS, "javascript-es5-object-observer");
                    let text = format!("{:?}", m.run().expect_err("exact managed payload"));
                    assert!(text.contains("uncaught JavaScript throw"));
                    let at = m
                        .stack()
                        .iter()
                        .position(|v| *v == vm::Value::string("P7_OBSERVED"))
                        .unwrap();
                    assert_eq!(m.stack()[at + 2], vm::Value::Float(3.0));
                    for (i, u) in units.iter().enumerate() {
                        assert_eq!(m.stack()[at + 3 + i], vm::Value::Float(f64::from(*u)));
                    }
                    eprintln!(
                        "UN129_FILE_THROW id={id} units={units:?} stack={:?}",
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
        oracle_units_completion(&source, expected);
    }
}
fn oracle_units_completion(source: &str, expected: Option<[u16; 3]>) {
    oracle_units(
        source,
        if let Some(u) = expected {
            serde_json::json!({"kind":"throw","units":u})
        } else {
            serde_json::json!({"kind":"number","value":1})
        },
    );
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
        "UN129_NODE {}",
        serde_json::json!({"source":source,"source_utf16":source.encode_utf16().collect::<Vec<_>>(),"argv":["timeout","15","node","-e",script],"rc":o.status.code(),"stdout":String::from_utf8_lossy(&o.stdout),"stderr":String::from_utf8_lossy(&o.stderr),"version_rc":v.status.code(),"version_stdout":String::from_utf8_lossy(&v.stdout),"version_stderr":String::from_utf8_lossy(&v.stderr)})
    );
    assert!(v.status.success() && o.status.success() && o.stderr.is_empty());
    let actual: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(actual, expected);
}

fn oracle_scalar(source: &str, tag: i64, number: f64) {
    let script = format!(
        "const source={};let v=(0,eval)(source);console.log(JSON.stringify(typeof v==='undefined'?{{kind:'undefined'}}:{{kind:typeof v,number:String(v),negative_zero:Object.is(v,-0)}}));",
        serde_json::to_string(source).unwrap()
    );
    let version = std::process::Command::new("timeout")
        .args(["15", "node", "--version"])
        .output()
        .unwrap();
    let output = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    eprintln!(
        "UN129_NODE {}",
        serde_json::json!({"source":source,"source_utf16":source.encode_utf16().collect::<Vec<_>>(),"argv":["timeout","15","node","-e",script],"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr),"version_rc":version.status.code(),"version_stdout":String::from_utf8_lossy(&version.stdout),"version_stderr":String::from_utf8_lossy(&version.stderr)})
    );
    assert!(version.status.success() && output.status.success() && output.stderr.is_empty());
    let expected = if tag == 0 {
        serde_json::json!({"kind":"undefined"})
    } else {
        let text = if number.is_nan() {
            "NaN".to_owned()
        } else if number == f64::INFINITY {
            "Infinity".to_owned()
        } else if number == f64::NEG_INFINITY {
            "-Infinity".to_owned()
        } else if number == 0.0 {
            "0".to_owned()
        } else {
            number.to_string()
        };
        serde_json::json!({"kind":"number","number":text,"negative_zero":number==0.0&&number.is_sign_negative()})
    };
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        expected
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
        _ => {}
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
// Only test IR is changed: place a fatal marker inside the actual unary
// Number payload expression, behind its original pending-normal branch.
struct ProjectionObserver;
impl vm::SourcePlugin for ProjectionObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("unary129-projection");
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
        let mut changed = 0;
        visit_stmts(&mut ir.stmts, &mut |e| {
            let E::Block { stmts, expr } = e else { return };
            let Some(S::IfElse {
                then_branch,
                else_branch,
                ..
            }) = stmts.last_mut()
            else {
                return;
            };
            if !else_branch.is_empty() || then_branch.len() != 1 {
                return;
            }
            let S::Assign {
                index, expr: boxed, ..
            } = &mut then_branch[0]
            else {
                return;
            };
            if !matches!(expr.as_ref(),E::Var(v) if v==index) {
                return;
            }
            fn payload(e: &mut E) -> Option<&mut E> {
                let E::Call(i, _, a, _, _) = e else {
                    return None;
                };
                if *i != vm::BuiltinFunction::Set.call_index() || a.len() != 3 {
                    return None;
                }
                if matches!(&a[1],E::String(k) if k=="number") {
                    return Some(&mut a[2]);
                }
                payload(&mut a[0])
            }
            let Some(p) = payload(boxed) else { return };
            if matches!(p, E::Float(_)) {
                return;
            }
            *p = E::Block {
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
                expr: Box::new(p.clone()),
            };
            changed += 1;
        });
        assert_eq!(
            changed, 1,
            "exact actual unary projection mutation: {source}"
        );
        eprintln!(
            "UN129_PROJECTION_MUTANT source={source:?} actual_number_payload_fatal=true count={changed}"
        );
        Ok(ir)
    }
}
#[test]
fn unary129_abrupt_skips_projection() {
    static OBS: ProjectionObserver = ProjectionObserver;
    for (source, success) in [
        (
            "function f(){throw 'P';}var ok=false;try{+f();}catch(e){ok=e==='P';}ok;",
            true,
        ),
        (
            "var o={get n(){throw 'P';}};var ok=false;try{+o.n;}catch(e){ok=e==='P';}ok;",
            true,
        ),
        (
            "function f(){throw 'P';}var ok=false;try{+(f(),1);}catch(e){ok=e==='P';}ok;",
            true,
        ),
        ("function f(){return 1;}+f();", false),
        ("function f(){return '1';}+f();", false),
        ("function f(){return true;}+f();", false),
        ("var ok=false;try{+'1';}catch(e){ok=true;}ok;", false),
        ("var ok=false;try{+true;}catch(e){ok=true;}ok;", false),
    ] {
        let source = source.to_owned();
        std::thread::Builder::new()
            .stack_size(33_554_432)
            .spawn(move || {
                let mut m = probe_vm(&source, &OBS, "unary129-projection");
                let status = m.run();
                eprintln!("UN129_PROJECTION_RESULT source={source:?} status={status:?}");
                if success {
                    assert_eq!(status.unwrap(), vm::VmStatus::Halted);
                    let vm::Value::Map(b) = m.stack().last().unwrap() else {
                        panic!("bool")
                    };
                    assert_eq!(
                        b.get(&vm::Value::string("flag")),
                        Some(&vm::Value::Bool(true))
                    );
                    oracle(&source, serde_json::json!({"kind":"boolean","value":true}));
                } else {
                    assert!(
                        format!(
                            "{:?}",
                            status.expect_err("normal unary projection reaches marker")
                        )
                        .contains("assertion failed")
                    );
                }
                assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
                reset(&mut m);
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
#[test]
fn unary129_void_managed_roots_gc() {
    static OBS: object_observer::Observer = object_observer::Observer {
        units: Some(3),
        force_gc: true,
        uncaught: false,
        type_error: false,
        realm_header: false,
        collect_phases: false,
        collect_stop: None,
    };
    static ALIAS: AliasRootObserver = AliasRootObserver;
    for source in [
        "function id(x,y){return x;}var k='P\\0\\ud800';var a=k;void id(k,'garbage'+'!');if(k!==a)throw 1;k;",
        "function f(x){void x;var t='garbage'+'!';return x;}f('P\\0\\ud800');",
    ] {
        let source = source.to_owned();
        std::thread::Builder::new().stack_size(33_554_432).spawn(move||{
            let mut m=if source.contains("k!==a") {probe_vm(&source,&ALIAS,"unary129-alias-roots")} else {probe_vm(&source,&OBS,"javascript-es5-object-observer")};let status=m.run();assert_eq!(status.unwrap(),vm::VmStatus::Halted);
            let at=m.stack().iter().position(|v|*v==vm::Value::string("P7_OBSERVED")).unwrap();
            let vm::Value::Map(b)=&m.stack()[at+1]else{panic!("managed box")};
            assert_eq!(b.get(&vm::Value::string("tag")),Some(&vm::Value::Int(4)));
            for field in ["ref_slot","generation"]{let vm::Value::Int(n)=b.get(&vm::Value::string(field)).unwrap()else{panic!("identity")};assert!(*n>0);}
            assert_eq!(m.stack()[at+2],vm::Value::Float(3.0));for(i,u)in[80,0,0xd800].iter().enumerate(){assert_eq!(m.stack()[at+3+i],vm::Value::Float(f64::from(*u)));}
            eprintln!("UN129_MANAGED_GC source={source:?} current_generation_checked=true units=[80,0,55296] stack={:?}",m.stack());
            assert_eq!((m.aot_exec_count(),m.jit_native_exec_count()),(0,0));reset(&mut m);
            oracle_units(&source,serde_json::json!({"kind":"string","units":[80,0,55296]}));
        }).unwrap().join().unwrap();
    }
}
#[test]
fn unary129_void_managed_throw_finally_gc() {
    static OBS: object_observer::Observer = object_observer::Observer {
        units: Some(3),
        force_gc: true,
        uncaught: true,
        type_error: false,
        realm_header: false,
        collect_phases: false,
        collect_stop: None,
    };
    let source = "function f(){throw 'P\\0\\ud800';}try{void f();}finally{var t='garbage'+'!';}";
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut m = probe_vm(source, &OBS, "javascript-es5-object-observer");
            assert!(
                format!("{:?}", m.run().expect_err("uncaught payload"))
                    .contains("uncaught JavaScript throw")
            );
            let at = m
                .stack()
                .iter()
                .position(|v| *v == vm::Value::string("P7_OBSERVED"))
                .unwrap();
            assert_eq!(m.stack()[at + 2], vm::Value::Float(3.0));
            for (i, u) in [80, 0, 0xd800].iter().enumerate() {
                assert_eq!(m.stack()[at + 3 + i], vm::Value::Float(f64::from(*u)));
            }
            eprintln!(
                "UN129_VOID_THROW_GC source={source:?} units=[80,0,55296] stack={:?}",
                m.stack()
            );
            assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
            reset(&mut m);
            oracle_units_completion(source, Some([80, 0, 0xd800]));
        })
        .unwrap()
        .join()
        .unwrap();
}

struct AliasRootObserver;
impl vm::SourcePlugin for AliasRootObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("unary129-alias-roots");
        d
    }
    fn parse_source(
        &self,
        source: &str,
        cx: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S, TypeSchema as T};
        let obs = object_observer::Observer {
            units: Some(3),
            force_gc: true,
            uncaught: false,
            type_error: false,
            realm_header: false,
            collect_phases: false,
            collect_stop: None,
        };
        let mut ir = vm::SourcePlugin::parse_source(&obs, source, cx)?;
        let h = |name: &str| ir.functions.iter().find(|f| f.name == name).unwrap().index;
        let cmp = h("__compat_es5_string_equal");
        let read = h("__compat_ref_read");
        let decode = h("__compat_es5_decode_int");
        let checked = h("__compat_es5_checked_base");
        let p = ir.function_impls[&cmp].param_slots.clone();
        assert_eq!(p.len(), 3);
        let (c, l, r) = (p[0], p[1], p[2]);
        let mut fresh = || {
            let s = u16::try_from(ir.locals).unwrap();
            ir.locals += 1;
            s
        };
        let (top, cursor, row, lf, rf, lb, rb) = (
            fresh(),
            fresh(),
            fresh(),
            fresh(),
            fresh(),
            fresh(),
            fresh(),
        );
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
        let set = |index, expr| S::Assign {
            kind: vm::AssignmentKind::Set,
            index,
            expr,
            line: 1,
        };
        let let_ = |index, t, expr| S::Let {
            index,
            declared_schema: Some(t),
            expr,
            line: 1,
        };
        let matched = |v| {
            E::And(
                Box::new(eq(rd(E::Var(row)), E::Int(4))),
                Box::new(E::And(
                    Box::new(eq(rd(add(E::Var(row), E::Int(1))), get(v, "ref_slot"))),
                    Box::new(eq(rd(add(E::Var(row), E::Int(2))), get(v, "generation"))),
                )),
            )
        };
        let mut pre = vec![
            check(eq(get(l, "ref_slot"), get(r, "ref_slot"))),
            check(eq(get(l, "generation"), get(r, "generation"))),
            let_(top, T::Int, rd(E::Int(3))),
            let_(cursor, T::Int, E::Int(0)),
            let_(row, T::Int, E::Int(0)),
            let_(lf, T::Bool, E::Bool(false)),
            let_(rf, T::Bool, E::Bool(false)),
            let_(
                lb,
                T::Int,
                call(
                    checked,
                    vec![E::Var(c), get(l, "ref_slot"), get(l, "generation")],
                ),
            ),
            let_(
                rb,
                T::Int,
                call(
                    checked,
                    vec![E::Var(c), get(r, "ref_slot"), get(r, "generation")],
                ),
            ),
            check(E::Gt(Box::new(rd(E::Int(11))), Box::new(E::Int(0)))),
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
                    set(lf, E::Or(Box::new(E::Var(lf)), Box::new(matched(l)))),
                    set(rf, E::Or(Box::new(E::Var(rf)), Box::new(matched(r)))),
                    set(cursor, add(E::Var(cursor), E::Int(1))),
                ],
                line: 1,
            },
            check(E::Var(lf)),
            check(E::Var(rf)),
            check(eq(E::Var(lb), E::Var(rb))),
        ];
        let f = ir.function_impls.get_mut(&cmp).unwrap();
        pre.append(&mut f.body_stmts);
        f.body_stmts = pre;
        eprintln!(
            "UN129_ROOT_ASSERTED source={source:?} actual_alias_slot_generation_equal=true input_root_rows_tag_slot_generation=true checked_base_live=true prior_node_begin_collection_count_positive=true private_stack_not_exported=true"
        );
        Ok(ir)
    }
}
