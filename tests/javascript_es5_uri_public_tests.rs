//! Bounded first-class primitive URI public surface.

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

fn public_string(source: &str, expected: &[u16]) {
    let source = source.to_owned();
    let expected = expected.to_vec();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!(
                "URI125_OLD_SOURCE {}",
                serde_json::json!({"source":source,"expected_units":expected})
            );
            let observer = Box::leak(Box::new(object_observer::Observer {
                units: Some(expected.len()),
                ..Default::default()
            }));
            let mut machine = probe_vm(&source, observer, "javascript-es5-object-observer");
            let status = machine.run();
            eprintln!(
                "URI125_OLD_VM source={source:?} status={status:?} stack={:?}",
                machine.stack()
            );
            assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            let stack = machine.stack();
            let at = stack
                .iter()
                .position(|v| v == &vm::Value::string("P7_OBSERVED"))
                .unwrap();
            let vm::Value::Map(boxed) = &stack[at + 1] else {
                panic!("managed box")
            };
            assert_eq!(boxed.len(), 5);
            for (key, value) in [
                ("tag", vm::Value::Int(4)),
                ("number", vm::Value::Float(0.0)),
                ("flag", vm::Value::Bool(false)),
            ] {
                assert_eq!(boxed.get(&vm::Value::string(key)), Some(&value));
            }
            for key in ["ref_slot", "generation"] {
                assert!(
                    matches!(boxed.get(&vm::Value::string(key)),Some(vm::Value::Int(n)) if *n>0)
                );
            }
            assert_eq!(stack[at + 2], vm::Value::Float(expected.len() as f64));
            for (i, u) in expected.iter().enumerate() {
                assert_eq!(stack[at + 3 + i], vm::Value::Float(f64::from(*u)));
            }
            reset(&mut machine);
            oracle_units(&source, &expected);
        })
        .unwrap()
        .join()
        .unwrap();
}
fn oracle_units(source: &str, expected: &[u16]) {
    let script = format!(
        "(()=>{{const source={};let v=(0,eval)(source);console.log(JSON.stringify({{kind:typeof v,units:Array.from({{length:v.length}},(_,i)=>v.charCodeAt(i))}}));}})();",
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
        "URI125_OLD_NODE {}",
        serde_json::json!({"source":source,"version_rc":version.status.code(),"version":String::from_utf8_lossy(&version.stdout),"argv":["timeout","15","node","-e",script],"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr)})
    );
    assert!(output.status.success());
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        actual,
        serde_json::json!({"kind":"string","units":expected})
    );
}

static FULL_DECODE_OBSERVER: UriObserver = UriObserver {
    full_decode: true,
    error_encode: false,
    resource: false,
    error: false,
    drop_result: false,
};
static FULL_ENCODE_ERROR_OBSERVER: UriObserver = UriObserver {
    full_decode: false,
    error_encode: true,
    resource: false,
    error: true,
    drop_result: false,
};
#[test]
fn full_uri123_roots_encode() {
    run_observer("var r=encodeURI('A B');r==='A%20B';", &SUCCESS_OBSERVER);
}
#[test]
fn full_uri123_roots_decode() {
    run_observer(
        "var r=decodeURI('A%2520B');r==='A%20B';",
        &FULL_DECODE_OBSERVER,
    );
}
#[test]
fn full_uri123_roots_decode_error() {
    run_observer(
        "var verdict=false;try{decodeURI('%');}catch(e){verdict=true;}verdict;",
        &ERROR_OBSERVER,
    );
}
#[test]
fn full_uri123_roots_encode_error() {
    run_observer(
        "var verdict=false;try{encodeURI('\\uD800');}catch(e){verdict=true;}verdict;",
        &FULL_ENCODE_ERROR_OBSERVER,
    );
}
fn full_boundary(name: &str) {
    for (prefix, callee) in [
        ("", name.to_owned()),
        (format!("var f={name};").as_str(), "f".to_owned()),
        (format!("var o={{f:{name}}};").as_str(), "o.f".to_owned()),
    ] {
        for (args, expected) in [("1", "1"), ("", "undefined"), ("'A','B'", "A")] {
            public_string(
                &format!("{prefix}try{{{callee}({args});}}catch(e){{true;}}"),
                &expected.encode_utf16().collect::<Vec<_>>(),
            );
        }
        // Separate first-value out-of-profile cases; these are not old arity rows.
        for value in ["{}", "function(){return 1;}"] {
            boundary(
                format!("{prefix}try{{{callee}({value});}}catch(e){{true;}}"),
                false,
            );
        }
    }
}
#[test]
fn full_uri123_runtime_boundaries_encode() {
    full_boundary("encodeURI");
}
#[test]
fn full_uri123_runtime_boundaries_decode() {
    full_boundary("decodeURI");
}
#[test]
fn uri_public_red_encode() {
    let source = "encodeURIComponent(\"A B\");";
    let result = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source);
    assert!(
        result.is_ok(),
        "public source={source:?} guest_invocations=0 error={:?}",
        result.err()
    );
}
#[test]
fn uri_public_red_decode() {
    let source = "decodeURIComponent(\"%41\");";
    let result = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source);
    assert!(
        result.is_ok(),
        "public source={source:?} guest_invocations=0 error={:?}",
        result.err()
    );
}

fn execute(source: &str) -> vm::Value {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!("URI101_SOURCE {source:?}");
            let mut machine =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                    .unwrap_or_else(|e| {
                        panic!("production source={source:?} guest_invocations=0 error={e:?}")
                    });
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let status = machine.run();
            eprintln!("URI101_VM status={status:?} stack={:?}", machine.stack());
            assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            let result = machine.stack().last().unwrap().clone();
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("URI101_RESET pending=false resources=0 guest_invocations=1");
            result
        })
        .unwrap()
        .join()
        .unwrap()
}
fn public_true(source: &str) {
    let result = execute(source);
    let vm::Value::Map(map) = result else {
        panic!("boxed {result:?}")
    };
    assert_eq!(map.len(), 5);
    for (name, expected) in [
        ("tag", vm::Value::Int(2)),
        ("number", vm::Value::Float(0.0)),
        ("flag", vm::Value::Bool(true)),
        ("ref_slot", vm::Value::Int(0)),
        ("generation", vm::Value::Int(0)),
    ] {
        assert_eq!(
            map.get(&vm::Value::string(name)),
            Some(&expected),
            "{source}"
        );
    }
    let version = std::process::Command::new("timeout")
        .args(["15", "node", "--version"])
        .output()
        .unwrap();
    let script = format!(
        "const source={};const value=eval(source);console.log(JSON.stringify({{value}}));",
        serde_json::to_string(source).unwrap()
    );
    let output = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    eprintln!(
        "URI101_NODE source={source:?} version_rc={:?} version={:?} argv={:?} rc={:?} stdout={:?} stderr={:?}",
        version.status.code(),
        String::from_utf8_lossy(&version.stdout),
        ["timeout", "15", "node", "-e", &script],
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(version.status.success() && output.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!({"value":true})
    );
}
#[test]
fn uri_public_component_ascii_unicode_initial() {
    public_true("encodeURIComponent('A B')==='A%20B'&&decodeURIComponent('%41')==='A';");
}
#[test]
fn uri_public_component_ascii_unicode_vectors() {
    for source in [
        "encodeURIComponent(\"AZaz09-_.!~*'()\")===\"AZaz09-_.!~*'()\"&&encodeURIComponent(';/?:@&=+$,#% ')==='%3B%2F%3F%3A%40%26%3D%2B%24%2C%23%25%20';",
        "encodeURIComponent('\\0é中\\uD83D\\uDE00')==='%00%C3%A9%E4%B8%AD%F0%9F%98%80'&&decodeURIComponent('%00%c3%a9%e4%b8%ad%f0%9f%98%80')==='\\0é中\\uD83D\\uDE00';",
        "encodeURIComponent('')===''&&decodeURIComponent('')===''&&decodeURIComponent('%2f%3F%23%25%41')==='/?#%A';",
    ] {
        public_true(source);
    }
}
#[test]
fn uri_public_component_malformed_urierror_decode() {
    public_true(
        "function f(s){'use strict';try{decodeURIComponent(s);}catch(e){return e.name==='URIError';}return false;}f('%')&&f('%0')&&f('%GG')&&f('%C0%AF')&&f('%E0%80%80')&&f('%E2%82')&&f('%ED%A0%80')&&f('%F4%90%80%80')&&f('%80');",
    );
}
#[test]
fn uri_public_component_malformed_urierror_encode() {
    public_true(
        "function f(s){'use strict';try{encodeURIComponent(s);}catch(e){return e.name==='URIError';}return false;}f('\\uD800')&&f('\\uDC00')&&f('x\\uD800x');",
    );
}
#[test]
fn uri_public_component_malformed_urierror_prototype() {
    public_true(
        "var a;var b;var t;try{decodeURIComponent('%');}catch(e){a=e;}try{encodeURIComponent('\\uD800');}catch(e){b=e;}try{var n=null;n.x;}catch(e){t=e;}var p=Object.getPrototypeOf(a);a!==b&&p===Object.getPrototypeOf(b)&&p!==Object.getPrototypeOf(t)&&Object.getPrototypeOf(p)===Object.getPrototypeOf(Object.getPrototypeOf(t))&&p.name==='URIError'&&p.message==='';",
    );
}
#[test]
fn uri_public_first_class_bindings_alias_member() {
    public_true(
        "var a=encodeURIComponent;var o={f:a};var b=o.f;var d=decodeURIComponent; a('A B')==='A%20B'&&o.f('/')==='%2F'&&b('%')==='%25'&&d('%2F')==='/'&&a.length===1&&d.length===1&&Object.getPrototypeOf(a)===Object.getPrototypeOf(Object);",
    );
}
#[test]
fn uri_public_first_class_bindings_function_shadow() {
    public_true(
        "function f(){'use strict';return encodeURIComponent('A B');}function g(encodeURIComponent){'use strict';return encodeURIComponent;}f()==='A%20B'&&g(7)===7;",
    );
}
#[test]
fn uri_public_first_class_bindings_canonical_write() {
    public_true(
        "var saved=encodeURIComponent;function read(){'use strict';return encodeURIComponent;}encodeURIComponent=decodeURIComponent;read()===decodeURIComponent&&read()('%41')==='A'&&saved('A B')==='A%20B';",
    );
}
#[test]
fn uri_public_first_class_bindings_once_and_abrupt() {
    public_true(
        "var n=0;var o={get x(){'use strict';throw 'argument';}};var caught=false;var r=encodeURIComponent((n=n+1,'A B'));try{encodeURIComponent(o.x);}catch(e){caught=e==='argument';}n===1&&r==='A%20B'&&caught;",
    );
}
fn boundary(source: String, resource: bool) {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut machine =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                    .unwrap_or_else(|e| {
                        panic!("boundary compile source_len={} error={e:?}", source.len())
                    });
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let status = machine.run();
            eprintln!(
                "URI101_BOUNDARY source_len={} resource={resource} status={status:?} native={}/{}",
                source.len(),
                machine.jit_native_exec_count(),
                machine.aot_exec_count()
            );
            assert!(
                status.is_err(),
                "fatal refusal required, not guest catch or fuel yield: {status:?}"
            );
            let text = format!("{:?}", status.unwrap_err());
            assert!(text.to_lowercase().contains("assert"), "{text}");
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("URI101_BOUNDARY_RESET pending=false resources=0");
        })
        .unwrap()
        .join()
        .unwrap();
}
#[test]
fn uri_public_unsupported_boundaries_type_alias() {
    for value in ["1", "true", "null", "undefined"] {
        let source = format!(
            "var f=encodeURIComponent;var caught=false;try{{f({value});}}catch(e){{caught=true;}}caught;"
        );
        let result = execute(&source);
        let vm::Value::Map(map) = result else {
            panic!("actual Boolean box")
        };
        assert_eq!(map.len(), 5);
        for (key, expected) in [
            ("tag", vm::Value::Int(2)),
            ("number", vm::Value::Float(0.0)),
            ("flag", vm::Value::Bool(false)),
            ("ref_slot", vm::Value::Int(0)),
            ("generation", vm::Value::Int(0)),
        ] {
            assert_eq!(
                map.get(&vm::Value::string(key)),
                Some(&expected),
                "{source}"
            );
        }
    }
    boundary(
        "var f=encodeURIComponent;var caught=false;try{f({});}catch(e){caught=true;}caught;".into(),
        false,
    );
}
#[test]
fn uri_public_standard_arity_member() {
    for (args, expected) in [("", "undefined"), ("'A','B'", "A")] {
        public_string(
            &format!(
                "var o={{f:decodeURIComponent}};var copied=o.f;try{{copied({args});}}catch(e){{true;}}"
            ),
            &expected.encode_utf16().collect::<Vec<_>>(),
        );
    }
}
#[test]
fn uri_public_unsupported_boundaries_resource65536() {
    let count = 65_536 / 9 + 1;
    assert!(count < 16_384 && count * 9 > 65_536);
    let source = format!(
        "var f=encodeURIComponent;f('{}');",
        "\u{0800}".repeat(count)
    );
    let root = std::path::PathBuf::from(std::env::var_os("TMPDIR").unwrap());
    std::fs::write(root.join("uri101-resource.js"), &source).unwrap();
    eprintln!(
        "URI101_RESOURCE input_units={count} exact_expansion={} fixed_cap=65536",
        count * 9
    );
    boundary(source, true);
}
#[test]
fn uri_public_bridge_demand_and_inactive_matrix() {
    for (name, source, bridge, number, uri) in [
        ("neither", "1;", 0, 0, 0),
        ("uri", "encodeURIComponent('x');", 1, 1, 1),
        ("number", "var k=1.5;var o={'1.5':7};o[k];", 1, 1, 0),
        (
            "combined",
            "var k=1.5;var o={'1.5':7};o[k];decodeURIComponent('%41');",
            1,
            1,
            1,
        ),
    ] {
        let ir = parse(source).unwrap();
        let count = |name| ir.functions.iter().filter(|f| f.name == name).count();
        assert_eq!(count("__compat_es5_managed_to_units"), bridge);
        assert_eq!(count("__compat_es5_units_to_managed"), bridge);
        assert_eq!(count("__compat_es5_number_string_managed"), number);
        assert_eq!(count("__compat_es5_fixed_scalar_string_managed"), uri);
        assert_eq!(count("__compat_es5_uri_kernel"), uri);
        assert_eq!(count("__compat_es5_uri_call"), uri);
        let ids: std::collections::HashSet<_> = ir.functions.iter().map(|f| f.index).collect();
        assert_eq!(ids.len(), ir.functions.len());
        for f in &ir.functions {
            if f.name == "__compat_es5_uri_call" {
                assert_eq!(f.arity, 3);
            }
            if f.name == "__compat_es5_dispatch" {
                assert_eq!(f.arity, 4);
            }
        }
        let root = std::path::PathBuf::from(std::env::var_os("TMPDIR").unwrap());
        std::fs::write(root.join(format!("uri101-{name}.debug")), snapshot(&ir)).unwrap();
        eprintln!(
            "URI101_DEMAND subject={name} bridge={bridge} number={number} uri={uri} functions={} locals={}",
            ir.functions.len(),
            ir.locals
        );
    }
}
fn call(index: u16, args: Vec<vm::Expr>) -> vm::Expr {
    vm::Expr::Call(index, vec![], args, None, None)
}
fn num(e: vm::Expr) -> vm::Expr {
    vm::Expr::Div(Box::new(e), Box::new(vm::Expr::Float(1.0)))
}
fn get(slot: u16, name: &str) -> vm::Expr {
    call(
        vm::BuiltinFunction::Get.call_index(),
        vec![
            vm::Expr::Borrow(Box::new(vm::Expr::Var(slot))),
            vm::Expr::String(name.into()),
        ],
    )
}
fn helper(ir: &vm::FrontendIr, name: &str) -> u16 {
    let found: Vec<_> = ir.functions.iter().filter(|f| f.name == name).collect();
    assert_eq!(found.len(), 1, "{name}");
    found[0].index
}
fn walk_stmts(ss: &mut [vm::Stmt], f: &mut impl FnMut(&mut Vec<vm::Stmt>)) {
    use vm::Stmt as S;
    for s in ss {
        match s {
            S::Let { expr, .. } | S::Assign { expr, .. } | S::Expr { expr, .. } => {
                walk_expr(expr, f)
            }
            S::IfElse {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                walk_expr(condition, f);
                walk_stmts(then_branch, f);
                walk_stmts(else_branch, f);
                f(then_branch);
                f(else_branch);
            }
            S::While {
                condition, body, ..
            } => {
                walk_expr(condition, f);
                walk_stmts(body, f);
            }
            S::For {
                init,
                condition,
                post,
                body,
                ..
            } => {
                walk_stmts(std::slice::from_mut(init), f);
                walk_expr(condition, f);
                walk_stmts(std::slice::from_mut(post), f);
                walk_stmts(body, f);
            }
            S::ClosureLet { closure, .. } => walk_expr(&mut closure.body, f),
            S::Noop { .. }
            | S::FuncDecl { .. }
            | S::Break { .. }
            | S::Continue { .. }
            | S::Drop { .. } => {}
        }
    }
}
fn walk_expr(e: &mut vm::Expr, f: &mut impl FnMut(&mut Vec<vm::Stmt>)) {
    use vm::Expr as E;
    match e {
        E::Block { stmts, expr } => {
            walk_stmts(stmts, f);
            walk_expr(expr, f);
            f(stmts);
        }
        E::Call(_, _, args, _, _) | E::LocalCall(_, _, args, _) | E::ModuleCall(_, _, args, _) => {
            for a in args {
                walk_expr(a, f);
            }
        }
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
            walk_expr(a, f);
            walk_expr(b, f);
        }
        E::Neg(a) | E::Not(a) | E::ToOwned(a) | E::Borrow(a) | E::BorrowMut(a) => walk_expr(a, f),
        E::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            walk_expr(condition, f);
            walk_expr(then_expr, f);
            walk_expr(else_expr, f);
        }
        E::ResultConstruct { payload, .. } => walk_expr(payload, f),
        E::ResultQuery { value, .. } | E::ResultTry { value, .. } => walk_expr(value, f),
        E::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            walk_expr(value, f);
            walk_expr(ok_expr, f);
            walk_expr(err_expr, f);
        }
        E::OptionalGet { container, key, .. } => {
            walk_expr(container, f);
            walk_expr(key, f);
        }
        E::OptionUnwrapOr {
            value, fallback, ..
        } => {
            walk_expr(value, f);
            walk_expr(fallback, f);
        }
        E::Match {
            value,
            arms,
            default,
            ..
        } => {
            walk_expr(value, f);
            for (_, a) in arms {
                walk_expr(a, f);
            }
            walk_expr(default, f);
        }
        E::Closure(c) => walk_expr(&mut c.body, f),
        E::ClosureCall(c, args) => {
            walk_expr(&mut c.body, f);
            for a in args {
                walk_expr(a, f);
            }
        }
        E::Null
        | E::Int(_)
        | E::Bool(_)
        | E::Float(_)
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

// Test-owned instrumentation: scalar assertions execute within real live frames.
struct UriObserver {
    full_decode: bool,
    error_encode: bool,
    resource: bool,
    error: bool,
    drop_result: bool,
}
static SUCCESS_OBSERVER: UriObserver = UriObserver {
    full_decode: false,
    error_encode: false,
    resource: false,
    error: false,
    drop_result: false,
};
static ERROR_OBSERVER: UriObserver = UriObserver {
    full_decode: false,
    error_encode: false,
    resource: false,
    error: true,
    drop_result: false,
};
static DROP_OBSERVER: UriObserver = UriObserver {
    full_decode: false,
    error_encode: false,
    resource: false,
    error: false,
    drop_result: true,
};
static RESOURCE_OBSERVER: UriObserver = UriObserver {
    full_decode: false,
    error_encode: false,
    resource: true,
    error: false,
    drop_result: false,
};
fn equal(a: vm::Expr, b: vm::Expr) -> vm::Expr {
    vm::Expr::Eq(Box::new(num(a)), Box::new(num(b)))
}
fn assertion(e: vm::Expr) -> vm::Stmt {
    vm::Stmt::Expr {
        expr: call(vm::BuiltinFunction::Assert.call_index(), vec![e]),
        line: 1,
    }
}
fn effect(slot: u16, e: vm::Expr) -> vm::Stmt {
    vm::Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: slot,
        expr: e,
        line: 1,
    }
}
impl vm::SourcePlugin for UriObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("uri101-observer");
        d
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
        let adapter = ir
            .functions
            .iter()
            .find(|f| f.name == "__compat_es5_uri_call")
            .map(|f| f.index)
            .ok_or_else(|| {
                vm::ParseError::at_span(
                    vm::Span::new(0, 0, 0),
                    "URI observer missing actual adapter",
                )
            })?;
        let read = helper(&ir, "__compat_ref_read");
        let base = helper(&ir, "__compat_es5_checked_base");
        let collect = helper(&ir, "__compat_es5_heap_collect");
        let observe = helper(&ir, "__compat_es5_heap_observe");
        let root = helper(&ir, "__compat_es5_intrinsic_root");
        let write = helper(&ir, "__compat_es5_root_write");
        let leave = helper(&ir, "__compat_es5_root_leave");
        let first = u16::try_from(ir.locals).unwrap();
        ir.locals += 4;
        let (sink, before, input_base, output_base) = (first, first + 1, first + 2, first + 3);
        let f = ir.function_impls.get_mut(&adapter).unwrap();
        let c = f.param_slots[0];
        let S::Assign {
            expr: E::Call(id, _, args, _, _),
            ..
        } = f.body_stmts.last().unwrap()
        else {
            panic!("actual adapter leave")
        };
        assert_eq!(*id, leave);
        let [E::Var(_), E::Var(frame)] = args.as_slice() else {
            panic!("actual leave ABI")
        };
        let frame = *frame;
        let inputs: Vec<_> = f
            .body_stmts
            .iter()
            .filter_map(|s| match s {
                S::Assign {
                    expr: E::Call(id, _, args, _, _),
                    ..
                } if *id == write => match args.as_slice() {
                    [E::Var(_), E::Var(fr), E::Int(0), E::Var(input)] if *fr == frame => {
                        Some(*input)
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(inputs.len(), 1);
        let input = inputs[0];
        let output = f
            .body_stmts
            .iter()
            .find_map(|s| match s {
                S::IfElse { then_branch, .. } => then_branch.iter().find_map(|s| match s {
                    S::Assign {
                        index,
                        expr: E::Call(_, _, args, _, _),
                        ..
                    } if args.len() == 4
                        && matches!(args.get(2),Some(E::Var(fr)) if *fr==frame)
                        && matches!(args.get(3), Some(E::Int(1))) =>
                    {
                        Some(*index)
                    }
                    _ => None,
                }),
                _ => None,
            })
            .expect("actual bridge output publication");
        if self.resource {
            let kernel = ir
                .functions
                .iter()
                .find(|f| f.name == "__compat_es5_uri_kernel")
                .unwrap()
                .index;
            let (position, record) = f
                .body_stmts
                .iter()
                .enumerate()
                .find_map(|(i, s)| match s {
                    S::Let {
                        index,
                        expr: E::Call(id, _, _, _, _),
                        ..
                    } if *id == kernel => Some((i, *index)),
                    _ => None,
                })
                .expect("real kernel return");
            let mut proof = vec![
                assertion(equal(get(record, "status"), E::Int(2))),
                assertion(equal(
                    call(
                        vm::BuiltinFunction::Len.call_index(),
                        vec![get(record, "output")],
                    ),
                    E::Int(0),
                )),
                assertion(equal(
                    call(observe, vec![E::Var(c), E::Int(8), E::Int(0)]),
                    E::Int(0),
                )),
            ];
            for o in 0..3 {
                proof.push(assertion(equal(
                    num(call(
                        read,
                        vec![
                            E::Var(c),
                            num(E::Add(Box::new(E::Var(frame)), Box::new(E::Int(7 + o)))),
                        ],
                    )),
                    E::Int(0),
                )));
            }
            proof.push(S::Expr {
                expr: E::String("URI101_REAL_STATUS2_NO_OUTPUT".into()),
                line: 1,
            });
            proof.push(S::Expr {
                expr: get(record, "status"),
                line: 1,
            });
            f.body_stmts.splice(position + 1..position + 1, proof);
            eprintln!(
                "URI101_RESOURCE_OBSERVER real_kernel={kernel} record={record} frame={frame} input={input} original_fatal_guard_retained=true"
            );
            return Ok(ir);
        }
        let read_cell = |address: E| num(call(read, vec![E::Var(c), num(address)]));
        let add = |a: E, n: i64| E::Add(Box::new(a), Box::new(E::Int(n)));
        let rooted = |entry: i64, payload: u16| -> Vec<S> {
            [
                (0, E::Int(if entry == 0 || !self.error { 4 } else { 5 })),
                (1, get(payload, "ref_slot")),
                (2, get(payload, "generation")),
            ]
            .into_iter()
            .map(|(offset, value)| {
                assertion(equal(
                    read_cell(add(E::Var(frame), 4 + entry * 3 + offset)),
                    value,
                ))
            })
            .collect()
        };
        let mut checks = vec![
            S::Let {
                index: sink,
                declared_schema: Some(T::Int),
                expr: E::Int(0),
                line: 1,
            },
            S::Let {
                index: before,
                declared_schema: Some(T::Int),
                expr: call(observe, vec![E::Var(c), E::Int(5), E::Int(0)]),
                line: 1,
            },
        ];
        checks.extend(rooted(0, input));
        checks.extend(rooted(1, output));
        if self.drop_result {
            // Deliberate loss of the actual result root; the later liveness check must fail.
            let undefined = [
                ("tag", E::Int(0)),
                ("number", E::Float(0.0)),
                ("flag", E::Bool(false)),
                ("ref_slot", E::Int(0)),
                ("generation", E::Int(0)),
            ]
            .into_iter()
            .fold(
                call(vm::BuiltinFunction::MapNew.call_index(), vec![]),
                |map, (key, value)| {
                    call(
                        vm::BuiltinFunction::Set.call_index(),
                        vec![map, E::String(key.into()), value],
                    )
                },
            );
            checks.push(effect(
                sink,
                call(write, vec![E::Var(c), E::Var(frame), E::Int(1), undefined]),
            ));
        }
        checks.push(effect(sink, call(collect, vec![E::Var(c)])));
        checks.push(assertion(equal(
            call(observe, vec![E::Var(c), E::Int(5), E::Int(0)]),
            add(E::Var(before), 1),
        )));
        for (slot, payload) in [(input_base, input), (output_base, output)] {
            checks.push(S::Let {
                index: slot,
                declared_schema: Some(T::Int),
                expr: call(
                    base,
                    vec![
                        E::Var(c),
                        get(payload, "ref_slot"),
                        get(payload, "generation"),
                    ],
                ),
                line: 1,
            });
            checks.push(assertion(equal(
                call(
                    observe,
                    vec![E::Var(c), E::Int(13), get(payload, "ref_slot")],
                ),
                E::Int(2),
            )));
            checks.push(assertion(equal(
                call(
                    observe,
                    vec![E::Var(c), E::Int(14), get(payload, "ref_slot")],
                ),
                get(payload, "generation"),
            )));
        }
        let input_units: Vec<u16> = if self.error_encode {
            vec![0xd800]
        } else if self.error {
            vec![37]
        } else if self.full_decode {
            "A%2520B".encode_utf16().collect()
        } else {
            "A B".encode_utf16().collect()
        };
        for (slot, units) in [
            (input_base, input_units),
            (
                output_base,
                if self.error {
                    vec![]
                } else {
                    "A%20B".encode_utf16().collect()
                },
            ),
        ] {
            if slot == output_base && self.error {
                continue;
            }
            checks.push(assertion(equal(read_cell(E::Var(slot)), E::Int(2))));
            checks.push(assertion(equal(
                read_cell(add(E::Var(slot), 2)),
                E::Int(units.len() as i64),
            )));
            for (i, u) in units.iter().enumerate() {
                checks.push(assertion(equal(
                    read_cell(add(E::Var(slot), 3 + i as i64)),
                    E::Int(i64::from(*u)),
                )));
            }
        }
        if self.error {
            checks.push(assertion(equal(get(output, "tag"), E::Int(5))));
            checks.push(assertion(equal(read_cell(E::Var(output_base)), E::Int(3))));
            checks.push(assertion(equal(
                read_cell(add(E::Var(output_base), 2)),
                E::Int(3),
            )));
            let proto = call(root, vec![E::Var(c), E::Int(5)]);
            for (offset, field) in [(3, "ref_slot"), (4, "generation")] {
                checks.push(assertion(equal(
                    read_cell(add(E::Var(output_base), offset)),
                    call(
                        vm::BuiltinFunction::Get.call_index(),
                        vec![proto.clone(), E::String(field.into())],
                    ),
                )));
            }
        }
        checks.extend(rooted(0, input));
        checks.extend(rooted(1, output));
        let position = f.body_stmts.len() - 1;
        f.body_stmts.splice(position..position, checks);
        // Bootstrap manifest witness before stage leave; only this selected URI
        // source has no Math/numeric demand: old31 + proto + name + one function.
        let bootstrap = helper(&ir, "__compat_es5_intrinsic_bootstrap");
        let bs = ir.function_impls.get_mut(&bootstrap).unwrap();
        let bc = bs.param_slots[0];
        let pos = bs.body_stmts.len() - 1;
        bs.body_stmts.insert(
            pos,
            assertion(equal(
                call(observe, vec![E::Var(bc), E::Int(0), E::Int(0)]),
                E::Int(34),
            )),
        );
        let mut pending_hits = 0;
        if self.error {
            walk_stmts(&mut ir.stmts, &mut |ss| {
                if !ss.iter().any(|s| {
                    matches!(
                        s,
                        S::Assign {
                            expr: E::Bool(false),
                            ..
                        }
                    )
                }) {
                    return;
                }
                let roots: Vec<_> = ss
                    .iter()
                    .filter_map(|s| match s {
                        S::Let {
                            expr: E::Call(id, _, args, _, _),
                            ..
                        }
                        | S::Assign {
                            expr: E::Call(id, _, args, _, _),
                            ..
                        } if *id == write => match args.as_slice() {
                            [E::Var(c), E::Var(fr), E::Int(entry), E::Var(payload)] => {
                                Some((*c, *fr, *entry, *payload))
                            }
                            _ => None,
                        },
                        _ => None,
                    })
                    .collect();
                if roots.len() != 2 {
                    return;
                }
                let (c, fr, entry, payload) = roots[1];
                pending_hits += 1;
                // Append within the exact Err block, after caller error/actual clears.
                ss.push(S::Let {
                    index: sink,
                    declared_schema: Some(T::Int),
                    expr: call(collect, vec![E::Var(c)]),
                    line: 1,
                });
                let addr = |o| E::Add(Box::new(E::Var(fr)), Box::new(E::Int(4 + entry * 3 + o)));
                for (o, value) in [
                    (0, E::Int(5)),
                    (1, get(payload, "ref_slot")),
                    (2, get(payload, "generation")),
                ] {
                    ss.push(assertion(equal(
                        num(call(read, vec![E::Var(c), num(addr(o))])),
                        value,
                    )));
                }
                ss.push(assertion(equal(
                    call(
                        observe,
                        vec![E::Var(c), E::Int(13), get(payload, "ref_slot")],
                    ),
                    E::Int(2),
                )));
                // The first caller error root and every transient clear are empty.
                for s in ss.clone() {
                    let args = match s {
                        S::Let {
                            expr: E::Call(id, _, args, _, _),
                            ..
                        }
                        | S::Assign {
                            expr: E::Call(id, _, args, _, _),
                            ..
                        } if id == write => args,
                        _ => continue,
                    };
                    if let [E::Var(cc), E::Var(ff), E::Int(ee), rhs] = args.as_slice()
                        && *cc == c
                        && *ff == fr
                        && *ee != entry
                        && !matches!(rhs, E::Var(_))
                    {
                        for o in 0..3 {
                            ss.push(assertion(equal(
                                num(call(
                                    read,
                                    vec![
                                        E::Var(c),
                                        num(E::Add(
                                            Box::new(E::Var(fr)),
                                            Box::new(E::Int(4 + ee * 3 + o)),
                                        )),
                                    ],
                                )),
                                E::Int(0),
                            )));
                        }
                    }
                }
            });
            assert_eq!(pending_hits, 1, "real ResultErr pending handoff");
        }
        eprintln!(
            "URI101_OBSERVER adapter={adapter} input={input} output={output} frame={frame} pending_hits={pending_hits} negative={} expected_bootstrap_nodes=34 live_frame_assertions=true",
            self.drop_result
        );
        Ok(ir)
    }
}
fn run_observer(source: &str, observer: &'static UriObserver) {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let catalog =
                pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
            let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
                &source,
                vm::SourceFlavor::Plugin(vm::FrontendId::new("uri101-observer")),
                vm::CompileSourceFileOptions::new()
                    .with_host_api_catalog(catalog.clone())
                    .with_source_plugin(observer),
            )
            .unwrap();
            let mut machine = compiled.into_vm().unwrap();
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
                &mut machine,
                pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
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
            for name in ["math::floor", "math::copysign", "bytes::from_utf8"] {
                registry.allow_builtin(name).unwrap();
            }
            registry.bind_vm_cached(&mut machine).unwrap();
            machine.set_fuel(10_000_000);
            let result = machine.run();
            eprintln!(
                "URI101_OBSERVER_VM source={source:?} negative={} result={result:?} stack={:?}",
                observer.drop_result,
                machine.stack()
            );
            if observer.resource {
                assert!(
                    format!("{:?}", result.unwrap_err())
                        .to_lowercase()
                        .contains("assert")
                );
                assert!(
                    machine.stack().windows(2).any(|w| w
                        == [
                            vm::Value::string("URI101_REAL_STATUS2_NO_OUTPUT"),
                            vm::Value::Int(2)
                        ]),
                    "actual status2 hit required before original fatal guard"
                );
            } else if observer.drop_result {
                assert!(
                    format!("{:?}", result.unwrap_err())
                        .to_lowercase()
                        .contains("assert")
                );
            } else {
                assert_eq!(result.unwrap(), vm::VmStatus::Halted);
            }
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("URI101_OBSERVER_RESET pending=false resources=0");
        })
        .unwrap()
        .join()
        .unwrap();
}
#[test]
fn uri_public_roots_and_collection_resource_status2() {
    let count = 65_536 / 9 + 1;
    let source = format!(
        "var f=encodeURIComponent;f('{}');",
        "\u{0800}".repeat(count)
    );
    run_observer(&source, &RESOURCE_OBSERVER);
}
#[test]
fn uri_public_roots_and_collection_success() {
    run_observer(
        "var r=encodeURIComponent('A B');r==='A%20B';",
        &SUCCESS_OBSERVER,
    );
}
#[test]
fn uri_public_roots_and_collection_error_pending() {
    run_observer(
        "var verdict=false;try{decodeURIComponent('%');}catch(e){verdict=true;}verdict;",
        &ERROR_OBSERVER,
    );
}
#[test]
fn uri_public_roots_and_collection_dropped_result_negative() {
    run_observer(
        "var r=encodeURIComponent('A B');r==='A%20B';",
        &DROP_OBSERVER,
    );
}
#[test]
fn uri_public_roots_and_collection_missing_observer_negative() {
    use vm::SourcePlugin;
    let context = vm::SourceFrontendContext {
        source_id: 0,
        original_path: None,
        phase: vm::compiler::ParsePhase::Standalone,
        host_api_catalog: None,
        imports: &[],
    };
    let error = SUCCESS_OBSERVER
        .parse_source("1;", &context)
        .expect_err("missing site must reject");
    assert!(error.message.contains("missing actual adapter"));
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
#[test]
fn uri_public_bridge_demand_and_inactive_baseline() {
    debug_observer::self_checks();
    let root =
        std::path::PathBuf::from(std::env::var_os("TMPDIR").unwrap()).join("uri101-inactive");
    std::fs::create_dir(&root).unwrap();
    for (i, source) in [
        "var x=1;x;",
        "'abc';",
        "var o={x:7};o.x;",
        "Math.sqrt(9);",
        "parseFloat('1.5');",
        "var key=1.5;var o={'1.5':7};o[key];",
        "function f(){'use strict';var key=1.5;var o={'1.5':7};return o[key];}f();",
        "var x=0;try{throw 'x';}catch(e){x=1;}finally{x=2;}x;",
        "function f(encodeURIComponent){'use strict';return encodeURIComponent;}f(7);",
    ]
    .iter()
    .enumerate()
    {
        let ir = parse(source).unwrap();
        let text = snapshot(&ir);
        let current = debug_observer::checked(text.as_bytes());
        std::fs::write(root.join(format!("{i:02}.js")), source).unwrap();
        std::fs::write(root.join(format!("{i:02}.debug")), &text).unwrap();
        std::fs::write(root.join(format!("{i:02}.observer")), current.receipt()).unwrap();
        if let Some(base) = std::env::var_os("URI101_INACTIVE_BASELINE") {
            let base = std::path::PathBuf::from(base);
            assert_eq!(
                std::fs::read_to_string(base.join(format!("{i:02}.js"))).unwrap(),
                *source
            );
            let raw = std::fs::read(base.join(format!("{i:02}.debug"))).unwrap();
            let previous = debug_observer::checked(&raw);
            if i == 0 {
                debug_observer::actual_checks(&raw, &previous);
            }
            assert_eq!(previous.data, current.data, "inactive {source}");
            eprintln!("URI101_INACTIVE_EQUAL subject={i:02} full_canonical_equal=true");
        }
        eprintln!("URI101_INACTIVE_CAPTURE subject={i:02}");
    }
}
// Complete parser output, with only unordered maps presented in key order.
// Float leaves additionally carry raw bits at their deterministic AST path.
fn snapshot(ir: &vm::FrontendIr) -> String {
    use std::collections::BTreeMap;
    use std::fmt::Write;
    let mut rest = ir.clone();
    let implementations: BTreeMap<_, _> = std::mem::take(&mut rest.function_impls)
        .into_iter()
        .collect();
    let schemas: BTreeMap<_, _> = std::mem::take(&mut rest.struct_schemas)
        .into_iter()
        .collect();
    let sources: BTreeMap<_, _> = std::mem::take(&mut rest.function_sources)
        .into_iter()
        .collect();
    let mut text = format!(
        "{rest:#?}\nimplementations={implementations:#?}\nschemas={schemas:#?}\nsources={sources:#?}\n"
    );
    fn expr(e: &vm::Expr, path: &str, out: &mut String) {
        use vm::Expr as E;
        match e {
            E::Float(n) => {
                writeln!(out, "{path} FloatBits={:016x}", n.to_bits()).unwrap();
            }
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
                expr(a, &format!("{path}/left"), out);
                expr(b, &format!("{path}/right"), out);
            }
            E::Neg(a) | E::Not(a) | E::ToOwned(a) | E::Borrow(a) | E::BorrowMut(a) => {
                expr(a, &format!("{path}/operand"), out)
            }
            E::Call(_, _, args, _, _)
            | E::LocalCall(_, _, args, _)
            | E::ModuleCall(_, _, args, _) => {
                for (i, a) in args.iter().enumerate() {
                    expr(a, &format!("{path}/arg{i}"), out);
                }
            }
            E::IfElse {
                condition,
                then_expr,
                else_expr,
            } => {
                expr(condition, &format!("{path}/condition"), out);
                expr(then_expr, &format!("{path}/then"), out);
                expr(else_expr, &format!("{path}/else"), out);
            }
            E::Block { stmts, expr: e } => {
                statements(stmts, path, out);
                expr(e, &format!("{path}/tail"), out);
            }
            E::ResultConstruct { payload, .. } => expr(payload, &format!("{path}/payload"), out),
            E::ResultQuery { value, .. } | E::ResultTry { value, .. } => {
                expr(value, &format!("{path}/value"), out)
            }
            E::ResultMatch {
                value,
                ok_expr,
                err_expr,
                ..
            } => {
                expr(value, &format!("{path}/value"), out);
                expr(ok_expr, &format!("{path}/ok"), out);
                expr(err_expr, &format!("{path}/err"), out);
            }
            E::OptionalGet { container, key, .. } => {
                expr(container, &format!("{path}/container"), out);
                expr(key, &format!("{path}/key"), out);
            }
            E::OptionUnwrapOr {
                value, fallback, ..
            } => {
                expr(value, &format!("{path}/value"), out);
                expr(fallback, &format!("{path}/fallback"), out);
            }
            E::Match {
                value,
                arms,
                default,
                ..
            } => {
                expr(value, &format!("{path}/value"), out);
                for (i, (_, a)) in arms.iter().enumerate() {
                    expr(a, &format!("{path}/arm{i}"), out);
                }
                expr(default, &format!("{path}/default"), out);
            }
            E::Null
            | E::Int(_)
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
            E::Closure(c) => expr(&c.body, &format!("{path}/closure/body"), out),
            E::ClosureCall(c, args) => {
                expr(&c.body, &format!("{path}/closure/body"), out);
                for (i, a) in args.iter().enumerate() {
                    expr(a, &format!("{path}/arg{i}"), out);
                }
            }
        }
    }
    fn statements(ss: &[vm::Stmt], path: &str, out: &mut String) {
        use vm::Stmt as S;
        for (i, s) in ss.iter().enumerate() {
            let p = format!("{path}/stmt{i}");
            match s {
                S::Let { expr: e, .. } | S::Assign { expr: e, .. } | S::Expr { expr: e, .. } => {
                    expr(e, &p, out)
                }
                S::IfElse {
                    condition,
                    then_branch,
                    else_branch,
                    ..
                } => {
                    expr(condition, &format!("{p}/condition"), out);
                    statements(then_branch, &format!("{p}/then"), out);
                    statements(else_branch, &format!("{p}/else"), out);
                }
                S::While {
                    condition, body, ..
                } => {
                    expr(condition, &format!("{p}/condition"), out);
                    statements(body, &format!("{p}/body"), out);
                }
                S::For {
                    init,
                    condition,
                    post,
                    body,
                    ..
                } => {
                    statements(std::slice::from_ref(init), &format!("{p}/init"), out);
                    expr(condition, &format!("{p}/condition"), out);
                    statements(std::slice::from_ref(post), &format!("{p}/post"), out);
                    statements(body, &format!("{p}/body"), out);
                }
                S::ClosureLet { closure, .. } => {
                    expr(&closure.body, &format!("{p}/closure/body"), out)
                }
                S::Noop { .. }
                | S::FuncDecl { .. }
                | S::Break { .. }
                | S::Continue { .. }
                | S::Drop { .. } => {}
            }
        }
    }
    statements(&ir.stmts, "script", &mut text);
    for (id, body) in implementations {
        statements(&body.body_stmts, &format!("function{id}"), &mut text);
        expr(&body.body_expr, &format!("function{id}/tail"), &mut text);
    }
    text
}

// Narrow byte-preserving observer for Rust Debug Object({quoted_key: value}).
// Only complete entries move. Commas/whitespace remain in their original slots;
// every emitted range refers to the input, so an exact inverse is checkable.
mod debug_observer {
    #[derive(Default)]
    struct Map {
        start: usize,
        end: usize,
        commas: Vec<usize>,
        children: Vec<usize>,
        entries: Vec<Entry>,
        tail: (usize, usize),
        permutation: Vec<usize>,
    }
    struct Entry {
        lo: usize,
        key: usize,
        end: usize,
        hi: usize,
        decoded: Vec<u8>,
    }
    struct Delimiter {
        kind: u8,
        map: Option<usize>,
        object: Option<usize>,
    }
    pub(super) struct Normalized {
        pub data: Vec<u8>,
        provenance: Vec<(usize, usize)>,
        maps: Vec<Map>,
        nonmap_objects: usize,
    }
    fn ws(b: u8) -> bool {
        matches!(b, b' ' | b'\t' | b'\r' | b'\n')
    }
    fn blank(data: &[u8]) -> bool {
        data.iter().all(|&b| ws(b))
    }
    fn quoted_end(data: &[u8], start: usize) -> Result<usize, String> {
        let mut i = start + 1;
        while i < data.len() {
            match data[i] {
                b'"' => return Ok(i + 1),
                b'\\' => i += 2,
                _ => i += 1,
            }
        }
        Err(format!("unterminated quoted string at {start}"))
    }
    fn decode(raw: &[u8]) -> Result<Vec<u8>, String> {
        let s = std::str::from_utf8(&raw[1..raw.len() - 1]).map_err(|e| e.to_string())?;
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                if (c as u32) < 32 {
                    return Err("unescaped key control character".into());
                }
                out.push(c);
                continue;
            }
            let c = match chars.next() {
                Some('n') => '\n',
                Some('r') => '\r',
                Some('t') => '\t',
                Some('0') => '\0',
                Some('\\') => '\\',
                Some('"') => '"',
                Some('\'') => '\'',
                Some('u') => {
                    if chars.next() != Some('{') {
                        return Err("missing unicode opener".into());
                    }
                    let mut digits = String::new();
                    loop {
                        match chars.next() {
                            Some('}') if !digits.is_empty() => break,
                            Some(d) if d.is_ascii_hexdigit() && digits.len() < 6 => digits.push(d),
                            _ => return Err("invalid Rust unicode escape".into()),
                        }
                    }
                    let n = u32::from_str_radix(&digits, 16).map_err(|e| e.to_string())?;
                    char::from_u32(n).ok_or("invalid Unicode scalar")?
                }
                _ => return Err("unsupported Rust key escape".into()),
            };
            out.push(c);
        }
        Ok(out.into_bytes())
    }
    fn entries(data: &[u8], node: &mut Map) -> Result<(), String> {
        let mut start = node.start + 1;
        let limit = node.end - 1;
        let mut ends: Vec<_> = node.commas.iter().map(|&p| (p, p + 1)).collect();
        if ends.last().is_none_or(|&(_, hi)| !blank(&data[hi..limit])) {
            ends.push((limit, limit));
        }
        let mut seen = std::collections::BTreeSet::new();
        for (end, hi) in ends {
            if blank(&data[start..end]) {
                if !node.commas.is_empty() {
                    return Err("missing map entry/key".into());
                }
                node.tail = (start, limit);
                return Ok(());
            }
            let mut left = start;
            let mut right = end;
            while ws(data[left]) {
                left += 1;
            }
            while ws(data[right - 1]) {
                right -= 1;
            }
            if data[left] != b'"' {
                return Err("Object map entry lacks quoted key".into());
            }
            let key_end = quoted_end(data, left)?;
            if key_end > right {
                return Err("key exceeds map entry".into());
            }
            let decoded = decode(&data[left..key_end])?;
            if !seen.insert(decoded.clone()) {
                return Err("duplicate decoded map key".into());
            }
            let mut colon = key_end;
            while colon < right && ws(data[colon]) {
                colon += 1;
            }
            if colon >= right || data[colon] != b':' || blank(&data[colon + 1..right]) {
                return Err("missing map colon/value".into());
            }
            node.entries.push(Entry {
                lo: start,
                key: left,
                end: right,
                hi,
                decoded,
            });
            start = hi;
        }
        node.tail = (start, limit);
        Ok(())
    }
    fn emit(pieces: &mut Vec<(usize, usize)>, a: usize, b: usize) {
        if a != b {
            pieces.push((a, b));
        }
    }
    fn region(
        a: usize,
        b: usize,
        children: &[usize],
        maps: &[Map],
        pieces: &mut Vec<(usize, usize)>,
    ) {
        let mut pos = a;
        for &id in children {
            let child = &maps[id];
            if a <= child.start && child.end <= b {
                emit(pieces, pos, child.start);
                render(id, maps, pieces);
                pos = child.end;
            }
        }
        emit(pieces, pos, b);
    }
    fn render(id: usize, maps: &[Map], pieces: &mut Vec<(usize, usize)>) {
        let node = &maps[id];
        emit(pieces, node.start, node.start + 1);
        for (slot, &original) in node.entries.iter().zip(&node.permutation) {
            let picked = &node.entries[original];
            emit(pieces, slot.lo, slot.key);
            region(picked.key, picked.end, &node.children, maps, pieces);
            emit(pieces, slot.end, slot.hi);
        }
        emit(pieces, node.tail.0, node.tail.1);
        emit(pieces, node.end - 1, node.end);
    }
    pub(super) fn normalize(data: &[u8], reverse: bool) -> Result<Normalized, String> {
        let mut stack: Vec<Delimiter> = Vec::new();
        let mut maps: Vec<Map> = Vec::new();
        let mut roots = Vec::new();
        let mut active: Vec<usize> = Vec::new();
        let mut recent: Vec<(u8, usize, usize)> = Vec::new();
        let mut objects = 0;
        let mut a = 0;
        while a < data.len() {
            let raw = data[a];
            let (kind, b) = if raw == b'"' {
                (b'Q', quoted_end(data, a)?)
            } else if data[a..].starts_with(b"Object")
                && (a == 0 || !(data[a - 1].is_ascii_alphanumeric() || data[a - 1] == b'_'))
                && data
                    .get(a + 6)
                    .is_none_or(|c| !(c.is_ascii_alphanumeric() || *c == b'_'))
            {
                (b'O', a + 6)
            } else if b"()[]{},:".contains(&raw) {
                (raw, a + 1)
            } else {
                a += 1;
                continue;
            };
            match kind {
                b'(' | b'[' | b'{' => {
                    if kind == b'('
                        && recent
                            .last()
                            .is_some_and(|&(k, _, end)| k == b'O' && blank(&data[end..a]))
                    {
                        objects += 1;
                    }
                    let mut map = None;
                    if kind == b'{'
                        && recent.len() == 2
                        && recent[0].0 == b'O'
                        && recent[1].0 == b'('
                        && blank(&data[recent[0].2..recent[1].1])
                        && blank(&data[recent[1].2..a])
                    {
                        let id = maps.len();
                        if let Some(&parent) = active.last() {
                            maps[parent].children.push(id);
                        } else {
                            roots.push(id);
                        }
                        maps.push(Map {
                            start: a,
                            ..Map::default()
                        });
                        active.push(id);
                        let payload = stack.last_mut().ok_or("missing Object payload opener")?;
                        if payload.kind != b'(' {
                            return Err("missing Object payload opener".into());
                        }
                        payload.object = Some(id);
                        map = Some(id);
                    }
                    stack.push(Delimiter {
                        kind,
                        map,
                        object: None,
                    });
                }
                b')' | b']' | b'}' => {
                    let opening = stack.pop().ok_or("unmatched closer")?;
                    if !matches!(
                        (opening.kind, kind),
                        (b'(', b')') | (b'[', b']') | (b'{', b'}')
                    ) {
                        return Err("mismatched delimiter".into());
                    }
                    if let Some(id) = opening.map {
                        maps[id].end = b;
                        assert_eq!(active.pop(), Some(id));
                        entries(data, &mut maps[id])?;
                    }
                    if let Some(id) = opening.object {
                        let gap = &data[maps[id].end..a];
                        let nonwhite: Vec<_> = gap.iter().copied().filter(|&c| !ws(c)).collect();
                        if !nonwhite.is_empty() && nonwhite != b"," {
                            return Err("ambiguous Object tuple payload".into());
                        }
                    }
                }
                b',' => {
                    if let Some(id) = stack.last().and_then(|d| d.map) {
                        maps[id].commas.push(a);
                    }
                }
                _ => {}
            }
            recent.push((kind, a, b));
            if recent.len() > 2 {
                recent.remove(0);
            }
            a = b;
        }
        if !stack.is_empty() {
            return Err("unclosed delimiter".into());
        }
        for map in &mut maps {
            let mut order: Vec<_> = (0..map.entries.len()).collect();
            order.sort_by(|&a, &b| map.entries[a].decoded.cmp(&map.entries[b].decoded));
            if reverse {
                order.reverse();
            }
            map.permutation = order;
        }
        let mut provenance = Vec::new();
        region(0, data.len(), &roots, &maps, &mut provenance);
        let mut canonical = Vec::with_capacity(data.len());
        for &(a, b) in &provenance {
            canonical.extend_from_slice(&data[a..b]);
        }
        assert_eq!(canonical.len(), data.len(), "byte length changed");
        let nonmap_objects = objects - maps.len();
        Ok(Normalized {
            data: canonical,
            provenance,
            maps,
            nonmap_objects,
        })
    }
    impl Normalized {
        fn inverse(&self) -> Vec<u8> {
            let mut chunks = Vec::new();
            let mut offset = 0;
            for &(a, b) in &self.provenance {
                assert!(b >= a);
                chunks.push((a, b, &self.data[offset..offset + b - a]));
                offset += b - a;
            }
            assert_eq!(offset, self.data.len());
            chunks.sort_by_key(|c| c.0);
            let mut expected = 0;
            let mut original = Vec::with_capacity(offset);
            for (a, b, bytes) in chunks {
                assert_eq!(a, expected, "not an exact disjoint input partition");
                expected = b;
                original.extend_from_slice(bytes);
            }
            assert_eq!(expected, offset);
            original
        }
        pub(super) fn receipt(&self) -> String {
            let permutations: Vec<_> = self.maps.iter().map(|m| &m.permutation).collect();
            format!(
                "bytes={} maps={} entries={} nonmap_objects={}\nprovenance={:?}\npermutations={permutations:?}\n",
                self.data.len(),
                self.maps.len(),
                self.maps.iter().map(|m| m.entries.len()).sum::<usize>(),
                self.nonmap_objects,
                self.provenance
            )
        }
    }
    pub(super) fn checked(data: &[u8]) -> Normalized {
        let normalized = normalize(data, false).expect("frozen Debug grammar");
        assert_eq!(normalized.inverse(), data, "exact byte inverse");
        assert_eq!(
            normalize(&normalized.data, false).unwrap().data,
            normalized.data,
            "idempotence"
        );
        let reversed = normalize(data, true).unwrap();
        assert_eq!(reversed.inverse(), data, "reverse exact inverse");
        assert_eq!(
            normalize(&reversed.data, false).unwrap().data,
            normalized.data,
            "reverse permutation invariance"
        );
        normalized
    }
    pub(super) fn self_checks() {
        let x = "pre [1, 2] Object({\"z\": Int, \"a\": Object({\"y\": Float, \"b\": Bool})}) post FloatBits=0000000000000000\n";
        let y = "pre [1, 2] Object({\"a\": Object({\"b\": Bool, \"y\": Float}), \"z\": Int}) post FloatBits=0000000000000000\n";
        let n = checked(x.as_bytes());
        assert_eq!(n.data, checked(y.as_bytes()).data);
        checked(br#"Object({"q\"}\\": Object({"\u{61}": Int, "\0": Float}), "a": Bool})"#);
        let mut count = 4;
        for (name, from, to) in [
            ("field_type", "\"z\": Int", "\"z\": Float"),
            ("field_name", "\"z\": Int", "\"zz\": Int"),
            ("field_value", "\"z\": Int", "\"z\": Int(9)"),
            ("missing_field", "\"z\": Int, ", ""),
            ("vector_order", "[1, 2]", "[2, 1]"),
            (
                "signed_zero_sidecar",
                "FloatBits=0000000000000000",
                "FloatBits=8000000000000000",
            ),
            (
                "nan_payload_sidecar",
                "FloatBits=0000000000000000",
                "FloatBits=7ff8123456789abc",
            ),
            ("whitespace", "pre ", "pre  "),
            ("nonmap_punctuation", "post ", "post; "),
        ] {
            assert_ne!(
                n.data,
                checked(x.replace(from, to).as_bytes()).data,
                "{name}"
            );
            count += 1;
        }
        for (name, a, b) in [
            ("parameter_order", "params: [2, 7]", "params: [7, 2]"),
            (
                "statement_order",
                "stmts: [Int(1), Int(2)]",
                "stmts: [Int(2), Int(1)]",
            ),
            (
                "argument_order",
                "Call(1, [Int(2), Int(7)])",
                "Call(1, [Int(7), Int(2)])",
            ),
            (
                "nan_payloads",
                "Float(NaN) FloatBits=7ff8123456789abc",
                "Float(NaN) FloatBits=7ff8123456789abd",
            ),
            (
                "nan_sign",
                "Float(NaN) FloatBits=7ff8123456789abc",
                "Float(NaN) FloatBits=fff8123456789abc",
            ),
            (
                "object_literal_vector",
                "Object([(\"z\",Int), (\"a\",Bool)])",
                "Object([(\"a\",Bool), (\"z\",Int)])",
            ),
            (
                "unrelated_map",
                "{\"z\": Int, \"a\": Bool}",
                "{\"a\": Bool, \"z\": Int}",
            ),
        ] {
            assert_ne!(
                checked(a.as_bytes()).data,
                checked(b.as_bytes()).data,
                "{name}"
            );
            count += 1;
        }
        for bad in [
            r#"Object({"a": Int, "a": Bool})"#,
            r#"Object({"a": Int, "\u{61}": Bool})"#,
            r#"Object({: Int})"#,
            r#"Object({"a": })"#,
            r#"Object({"a" Int})"#,
            r#"Object({,"a": Int})"#,
            r#"Object({"a": Int)"#,
            r#"Object({"a": Int}, 7)"#,
            r#"Object({"\w": Int})"#,
            r#"Object({"a: Int})"#,
            r#"Object({"\u{d800}": Int})"#,
            r#"Object({"\u{110000}": Int})"#,
            r#"Object({"\u{}": Int})"#,
        ] {
            assert!(
                normalize(bad.as_bytes(), false).is_err(),
                "must reject {bad}"
            );
            count += 1;
        }
        let opaque = br#"String("Object({\"b\": Int, \"a\": Bool})")"#;
        assert_eq!(checked(opaque).data, opaque);
        checked(b"Object( {  } , )");
        count += 2;
        eprintln!("GLOBAL40_OBSERVER_SELF_CHECKS count={count} all_passed=true");
    }
    pub(super) fn actual_checks(data: &[u8], n: &Normalized) {
        let text = std::str::from_utf8(data).unwrap();
        for (name, changed) in [
            (
                "field_type",
                text.replacen("\"number\": Float", "\"number\": Int", 1),
            ),
            (
                "missing_field",
                text.replacen("                \"number\": Float,\n", "", 1),
            ),
        ] {
            assert_ne!(changed.as_bytes(), data, "mutation applied {name}");
            assert_ne!(
                normalize(changed.as_bytes(), false).unwrap().data,
                n.data,
                "actual00 {name}"
            );
        }
        let at = text.find("FloatBits=").unwrap() + "FloatBits=".len();
        let bits = u64::from_str_radix(&text[at..at + 16], 16).unwrap();
        let changed = format!(
            "{}{:016x}{}",
            &text[..at],
            bits ^ (1 << 63),
            &text[at + 16..]
        );
        assert_ne!(
            normalize(changed.as_bytes(), false).unwrap().data,
            n.data,
            "actual00 FloatBits sign"
        );
        let map = &n.maps[0];
        assert_eq!(map.entries.len(), 5);
        fn permutations(order: &mut [usize], at: usize, f: &mut impl FnMut(&[usize])) {
            if at == order.len() {
                f(order);
            } else {
                for i in at..order.len() {
                    order.swap(at, i);
                    permutations(order, at + 1, f);
                    order.swap(at, i);
                }
            }
        }
        let mut count = 0;
        permutations(&mut [0, 1, 2, 3, 4], 0, &mut |order| {
            let mut permuted = data[..map.start + 1].to_vec();
            for (slot, &picked) in map.entries.iter().zip(order) {
                permuted.extend_from_slice(&data[slot.lo..slot.key]);
                let entry = &map.entries[picked];
                permuted.extend_from_slice(&data[entry.key..entry.end]);
                permuted.extend_from_slice(&data[slot.end..slot.hi]);
            }
            permuted.extend_from_slice(&data[map.tail.0..map.tail.1]);
            permuted.extend_from_slice(&data[map.end - 1..]);
            let p = normalize(&permuted, false).unwrap();
            assert_eq!(p.data, n.data);
            assert_eq!(p.inverse(), permuted);
            count += 1;
        });
        assert_eq!(count, 120);
        eprintln!("GLOBAL40_ACTUAL00 mutations=3 permutations={count} inverse_exact=true");
    }
}
