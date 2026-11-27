//! Existing JavaScript primitive binary addition, exact runtime observations.
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
    eprintln!("ADD140_RESET resources=0 pending=false owned_retired=true cert=false");
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

fn oracle_number(source: &str, expected: f64) {
    let script = format!(
        "let v=(0,eval)({});let b=Buffer.alloc(8);b.writeDoubleBE(v);console.log(JSON.stringify({{kind:typeof v,bits:Number.isNaN(v)?'NaN':b.toString('hex')}}));",
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
        "ADD140_NODE {}",
        serde_json::json!({"source":source,"argv":["timeout","15","node","-e",script],"rc":o.status.code(),"stdout":String::from_utf8_lossy(&o.stdout),"stderr":String::from_utf8_lossy(&o.stderr),"version_rc":v.status.code(),"version":String::from_utf8_lossy(&v.stdout)})
    );
    assert!(v.status.success() && o.status.success() && o.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap(),
        serde_json::json!({"kind":"number","bits":if expected.is_nan(){"NaN".to_owned()}else{format!("{:016x}",expected.to_bits())}})
    );
}
fn number(source: &str, expected: f64) {
    oracle_number(source, expected);
    let source = source.to_owned();
    std::thread::Builder::new().stack_size(33_554_432).spawn(move || {
        let mut m=pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source).unwrap_or_else(|e|panic!("ADD140_SOURCE_RED source={source:?} guest_invocations=0 {e:?}"));
        m.set_jit_config(vm::JitConfig{enabled:false,..*m.jit_config()});
        assert!(!m.jit_config().enabled);assert_eq!((m.aot_exec_count(),m.jit_native_exec_count()),(0,0));
        m.set_fuel(10_000_000);let status=m.run();
        eprintln!("ADD140_VM {}",serde_json::json!({"source":source,"status":format!("{status:?}"),"jit_enabled":m.jit_config().enabled,"aot":m.aot_exec_count(),"jit":m.jit_native_exec_count(),"stack":format!("{:?}",m.stack())}));
        assert_eq!(status.unwrap(),vm::VmStatus::Halted);
        let vm::Value::Map(b)=m.stack().last().unwrap()else{panic!("Number box")};
        assert_eq!(b.len(),5);assert_eq!(b.get(&vm::Value::string("tag")),Some(&vm::Value::Int(3)));
        let vm::Value::Float(n)=b.get(&vm::Value::string("number")).unwrap()else{panic!("Float")};
        assert!(if expected.is_nan(){n.is_nan()}else{n.to_bits()==expected.to_bits()},"{source} {n:?} expected {expected:?}");
        assert!(!m.jit_config().enabled);assert_eq!((m.aot_exec_count(),m.jit_native_exec_count()),(0,0));reset(&mut m);
    }).unwrap().join().unwrap();
}
fn string(source: &str, expected: &[u16]) {
    let source = source.to_owned();
    let expected = expected.to_vec();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!(
                "ADD140_SOURCE {}",
                serde_json::json!({"source":source,"expected_units":expected})
            );
            let observer = Box::leak(Box::new(object_observer::Observer {
                units: Some(expected.len()),
                ..Default::default()
            }));
            let mut machine = probe_vm(&source, observer, "javascript-es5-object-observer");
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            let status = machine.run();
            eprintln!(
                "ADD140_VM source={source:?} status={status:?} stack={:?}",
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
        "ADD140_NODE {}",
        serde_json::json!({"source":source,"version_rc":version.status.code(),"version":String::from_utf8_lossy(&version.stdout),"argv":["timeout","15","node","-e",script],"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr)})
    );
    assert!(output.status.success());
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        actual,
        serde_json::json!({"kind":"string","units":expected})
    );
}

#[test]
fn addition140_red_known_scalar() {
    number("true+null;", 1.0);
}
#[test]
fn addition140_red_runtime_string() {
    string("function f(){return 'x';}f()+1;", &[120, 49]);
}
#[test]
fn addition140_red_formal() {
    number("function f(x){return x+1;}f(true);", 2.0);
}

#[test]
fn addition140_matrix_00() {
    eprintln!("ADD140_MATRIX_ROW id=0");
    number("2+2;", 4.0);
    eprintln!("ADD140_MATRIX_ROW id=1");
    number("function id(x){return x;}id(2)+2;", 4.0);
    eprintln!("ADD140_MATRIX_ROW id=2");
    number("function id(x){return x;}2+id(2);", 4.0);
    eprintln!("ADD140_MATRIX_ROW id=3");
    number("function id(x){return x;}id(2)+id(2);", 4.0);
    eprintln!("ADD140_MATRIX_ROW id=4");
    number("2+true;", 3.0);
    eprintln!("ADD140_MATRIX_ROW id=5");
    number("function id(x){return x;}id(2)+true;", 3.0);
    eprintln!("ADD140_MATRIX_ROW id=6");
    number("function id(x){return x;}2+id(true);", 3.0);
    eprintln!("ADD140_MATRIX_ROW id=7");
    number("function id(x){return x;}id(2)+id(true);", 3.0);
}

#[test]
fn addition140_matrix_01() {
    eprintln!("ADD140_MATRIX_ROW id=8");
    number("2+null;", 2.0);
    eprintln!("ADD140_MATRIX_ROW id=9");
    number("function id(x){return x;}id(2)+null;", 2.0);
    eprintln!("ADD140_MATRIX_ROW id=10");
    number("function id(x){return x;}2+id(null);", 2.0);
    eprintln!("ADD140_MATRIX_ROW id=11");
    number("function id(x){return x;}id(2)+id(null);", 2.0);
    eprintln!("ADD140_MATRIX_ROW id=12");
    number("2+undefined;", f64::NAN);
    eprintln!("ADD140_MATRIX_ROW id=13");
    number("function id(x){return x;}id(2)+undefined;", f64::NAN);
    eprintln!("ADD140_MATRIX_ROW id=14");
    number("function id(x){return x;}2+id(undefined);", f64::NAN);
    eprintln!("ADD140_MATRIX_ROW id=15");
    number("function id(x){return x;}id(2)+id(undefined);", f64::NAN);
}

#[test]
fn addition140_matrix_02() {
    eprintln!("ADD140_MATRIX_ROW id=16");
    string("2+'x';", &[50, 120]);
    eprintln!("ADD140_MATRIX_ROW id=17");
    string("function id(x){return x;}id(2)+'x';", &[50, 120]);
    eprintln!("ADD140_MATRIX_ROW id=18");
    string("function id(x){return x;}2+id('x');", &[50, 120]);
    eprintln!("ADD140_MATRIX_ROW id=19");
    string("function id(x){return x;}id(2)+id('x');", &[50, 120]);
    eprintln!("ADD140_MATRIX_ROW id=20");
    number("true+2;", 3.0);
    eprintln!("ADD140_MATRIX_ROW id=21");
    number("function id(x){return x;}id(true)+2;", 3.0);
    eprintln!("ADD140_MATRIX_ROW id=22");
    number("function id(x){return x;}true+id(2);", 3.0);
    eprintln!("ADD140_MATRIX_ROW id=23");
    number("function id(x){return x;}id(true)+id(2);", 3.0);
}

#[test]
fn addition140_matrix_03() {
    eprintln!("ADD140_MATRIX_ROW id=24");
    number("true+true;", 2.0);
    eprintln!("ADD140_MATRIX_ROW id=25");
    number("function id(x){return x;}id(true)+true;", 2.0);
    eprintln!("ADD140_MATRIX_ROW id=26");
    number("function id(x){return x;}true+id(true);", 2.0);
    eprintln!("ADD140_MATRIX_ROW id=27");
    number("function id(x){return x;}id(true)+id(true);", 2.0);
    eprintln!("ADD140_MATRIX_ROW id=28");
    number("true+null;", 1.0);
    eprintln!("ADD140_MATRIX_ROW id=29");
    number("function id(x){return x;}id(true)+null;", 1.0);
    eprintln!("ADD140_MATRIX_ROW id=30");
    number("function id(x){return x;}true+id(null);", 1.0);
    eprintln!("ADD140_MATRIX_ROW id=31");
    number("function id(x){return x;}id(true)+id(null);", 1.0);
}

#[test]
fn addition140_matrix_04() {
    eprintln!("ADD140_MATRIX_ROW id=32");
    number("true+undefined;", f64::NAN);
    eprintln!("ADD140_MATRIX_ROW id=33");
    number("function id(x){return x;}id(true)+undefined;", f64::NAN);
    eprintln!("ADD140_MATRIX_ROW id=34");
    number("function id(x){return x;}true+id(undefined);", f64::NAN);
    eprintln!("ADD140_MATRIX_ROW id=35");
    number("function id(x){return x;}id(true)+id(undefined);", f64::NAN);
    eprintln!("ADD140_MATRIX_ROW id=36");
    string("true+'x';", &[116, 114, 117, 101, 120]);
    eprintln!("ADD140_MATRIX_ROW id=37");
    string(
        "function id(x){return x;}id(true)+'x';",
        &[116, 114, 117, 101, 120],
    );
    eprintln!("ADD140_MATRIX_ROW id=38");
    string(
        "function id(x){return x;}true+id('x');",
        &[116, 114, 117, 101, 120],
    );
    eprintln!("ADD140_MATRIX_ROW id=39");
    string(
        "function id(x){return x;}id(true)+id('x');",
        &[116, 114, 117, 101, 120],
    );
}

#[test]
fn addition140_matrix_05() {
    eprintln!("ADD140_MATRIX_ROW id=40");
    number("null+2;", 2.0);
    eprintln!("ADD140_MATRIX_ROW id=41");
    number("function id(x){return x;}id(null)+2;", 2.0);
    eprintln!("ADD140_MATRIX_ROW id=42");
    number("function id(x){return x;}null+id(2);", 2.0);
    eprintln!("ADD140_MATRIX_ROW id=43");
    number("function id(x){return x;}id(null)+id(2);", 2.0);
    eprintln!("ADD140_MATRIX_ROW id=44");
    number("null+true;", 1.0);
    eprintln!("ADD140_MATRIX_ROW id=45");
    number("function id(x){return x;}id(null)+true;", 1.0);
    eprintln!("ADD140_MATRIX_ROW id=46");
    number("function id(x){return x;}null+id(true);", 1.0);
    eprintln!("ADD140_MATRIX_ROW id=47");
    number("function id(x){return x;}id(null)+id(true);", 1.0);
}

#[test]
fn addition140_matrix_06() {
    eprintln!("ADD140_MATRIX_ROW id=48");
    number("null+null;", 0.0);
    eprintln!("ADD140_MATRIX_ROW id=49");
    number("function id(x){return x;}id(null)+null;", 0.0);
    eprintln!("ADD140_MATRIX_ROW id=50");
    number("function id(x){return x;}null+id(null);", 0.0);
    eprintln!("ADD140_MATRIX_ROW id=51");
    number("function id(x){return x;}id(null)+id(null);", 0.0);
    eprintln!("ADD140_MATRIX_ROW id=52");
    number("null+undefined;", f64::NAN);
    eprintln!("ADD140_MATRIX_ROW id=53");
    number("function id(x){return x;}id(null)+undefined;", f64::NAN);
    eprintln!("ADD140_MATRIX_ROW id=54");
    number("function id(x){return x;}null+id(undefined);", f64::NAN);
    eprintln!("ADD140_MATRIX_ROW id=55");
    number("function id(x){return x;}id(null)+id(undefined);", f64::NAN);
}

#[test]
fn addition140_matrix_07() {
    eprintln!("ADD140_MATRIX_ROW id=56");
    string("null+'x';", &[110, 117, 108, 108, 120]);
    eprintln!("ADD140_MATRIX_ROW id=57");
    string(
        "function id(x){return x;}id(null)+'x';",
        &[110, 117, 108, 108, 120],
    );
    eprintln!("ADD140_MATRIX_ROW id=58");
    string(
        "function id(x){return x;}null+id('x');",
        &[110, 117, 108, 108, 120],
    );
    eprintln!("ADD140_MATRIX_ROW id=59");
    string(
        "function id(x){return x;}id(null)+id('x');",
        &[110, 117, 108, 108, 120],
    );
    eprintln!("ADD140_MATRIX_ROW id=60");
    number("undefined+2;", f64::NAN);
    eprintln!("ADD140_MATRIX_ROW id=61");
    number("function id(x){return x;}id(undefined)+2;", f64::NAN);
    eprintln!("ADD140_MATRIX_ROW id=62");
    number("function id(x){return x;}undefined+id(2);", f64::NAN);
    eprintln!("ADD140_MATRIX_ROW id=63");
    number("function id(x){return x;}id(undefined)+id(2);", f64::NAN);
}

#[test]
fn addition140_matrix_08() {
    eprintln!("ADD140_MATRIX_ROW id=64");
    number("undefined+true;", f64::NAN);
    eprintln!("ADD140_MATRIX_ROW id=65");
    number("function id(x){return x;}id(undefined)+true;", f64::NAN);
    eprintln!("ADD140_MATRIX_ROW id=66");
    number("function id(x){return x;}undefined+id(true);", f64::NAN);
    eprintln!("ADD140_MATRIX_ROW id=67");
    number("function id(x){return x;}id(undefined)+id(true);", f64::NAN);
    eprintln!("ADD140_MATRIX_ROW id=68");
    number("undefined+null;", f64::NAN);
    eprintln!("ADD140_MATRIX_ROW id=69");
    number("function id(x){return x;}id(undefined)+null;", f64::NAN);
    eprintln!("ADD140_MATRIX_ROW id=70");
    number("function id(x){return x;}undefined+id(null);", f64::NAN);
    eprintln!("ADD140_MATRIX_ROW id=71");
    number("function id(x){return x;}id(undefined)+id(null);", f64::NAN);
}

#[test]
fn addition140_matrix_09() {
    eprintln!("ADD140_MATRIX_ROW id=72");
    number("undefined+undefined;", f64::NAN);
    eprintln!("ADD140_MATRIX_ROW id=73");
    number(
        "function id(x){return x;}id(undefined)+undefined;",
        f64::NAN,
    );
    eprintln!("ADD140_MATRIX_ROW id=74");
    number(
        "function id(x){return x;}undefined+id(undefined);",
        f64::NAN,
    );
    eprintln!("ADD140_MATRIX_ROW id=75");
    number(
        "function id(x){return x;}id(undefined)+id(undefined);",
        f64::NAN,
    );
    eprintln!("ADD140_MATRIX_ROW id=76");
    string(
        "undefined+'x';",
        &[117, 110, 100, 101, 102, 105, 110, 101, 100, 120],
    );
    eprintln!("ADD140_MATRIX_ROW id=77");
    string(
        "function id(x){return x;}id(undefined)+'x';",
        &[117, 110, 100, 101, 102, 105, 110, 101, 100, 120],
    );
    eprintln!("ADD140_MATRIX_ROW id=78");
    string(
        "function id(x){return x;}undefined+id('x');",
        &[117, 110, 100, 101, 102, 105, 110, 101, 100, 120],
    );
    eprintln!("ADD140_MATRIX_ROW id=79");
    string(
        "function id(x){return x;}id(undefined)+id('x');",
        &[117, 110, 100, 101, 102, 105, 110, 101, 100, 120],
    );
}

#[test]
fn addition140_matrix_10() {
    eprintln!("ADD140_MATRIX_ROW id=80");
    string("'x'+2;", &[120, 50]);
    eprintln!("ADD140_MATRIX_ROW id=81");
    string("function id(x){return x;}id('x')+2;", &[120, 50]);
    eprintln!("ADD140_MATRIX_ROW id=82");
    string("function id(x){return x;}'x'+id(2);", &[120, 50]);
    eprintln!("ADD140_MATRIX_ROW id=83");
    string("function id(x){return x;}id('x')+id(2);", &[120, 50]);
    eprintln!("ADD140_MATRIX_ROW id=84");
    string("'x'+true;", &[120, 116, 114, 117, 101]);
    eprintln!("ADD140_MATRIX_ROW id=85");
    string(
        "function id(x){return x;}id('x')+true;",
        &[120, 116, 114, 117, 101],
    );
    eprintln!("ADD140_MATRIX_ROW id=86");
    string(
        "function id(x){return x;}'x'+id(true);",
        &[120, 116, 114, 117, 101],
    );
    eprintln!("ADD140_MATRIX_ROW id=87");
    string(
        "function id(x){return x;}id('x')+id(true);",
        &[120, 116, 114, 117, 101],
    );
}

#[test]
fn addition140_matrix_11() {
    eprintln!("ADD140_MATRIX_ROW id=88");
    string("'x'+null;", &[120, 110, 117, 108, 108]);
    eprintln!("ADD140_MATRIX_ROW id=89");
    string(
        "function id(x){return x;}id('x')+null;",
        &[120, 110, 117, 108, 108],
    );
    eprintln!("ADD140_MATRIX_ROW id=90");
    string(
        "function id(x){return x;}'x'+id(null);",
        &[120, 110, 117, 108, 108],
    );
    eprintln!("ADD140_MATRIX_ROW id=91");
    string(
        "function id(x){return x;}id('x')+id(null);",
        &[120, 110, 117, 108, 108],
    );
    eprintln!("ADD140_MATRIX_ROW id=92");
    string(
        "'x'+undefined;",
        &[120, 117, 110, 100, 101, 102, 105, 110, 101, 100],
    );
    eprintln!("ADD140_MATRIX_ROW id=93");
    string(
        "function id(x){return x;}id('x')+undefined;",
        &[120, 117, 110, 100, 101, 102, 105, 110, 101, 100],
    );
    eprintln!("ADD140_MATRIX_ROW id=94");
    string(
        "function id(x){return x;}'x'+id(undefined);",
        &[120, 117, 110, 100, 101, 102, 105, 110, 101, 100],
    );
    eprintln!("ADD140_MATRIX_ROW id=95");
    string(
        "function id(x){return x;}id('x')+id(undefined);",
        &[120, 117, 110, 100, 101, 102, 105, 110, 101, 100],
    );
}

#[test]
fn addition140_matrix_12() {
    eprintln!("ADD140_MATRIX_ROW id=96");
    string("'x'+'x';", &[120, 120]);
    eprintln!("ADD140_MATRIX_ROW id=97");
    string("function id(x){return x;}id('x')+'x';", &[120, 120]);
    eprintln!("ADD140_MATRIX_ROW id=98");
    string("function id(x){return x;}'x'+id('x');", &[120, 120]);
    eprintln!("ADD140_MATRIX_ROW id=99");
    string("function id(x){return x;}id('x')+id('x');", &[120, 120]);
}

#[test]
fn addition140_extensions_string_0() {
    eprintln!("ADD140_EXTENSION numeric_looking");
    string("function id(x){return x;}id('12')+id(3);", &[49, 50, 51]);
    eprintln!("ADD140_EXTENSION empty");
    string(
        "function id(x){return x;}id('')+id(false);",
        &[102, 97, 108, 115, 101],
    );
    eprintln!("ADD140_EXTENSION false");
    string(
        "function id(x){return x;}id(false)+id('');",
        &[102, 97, 108, 115, 101],
    );
    eprintln!("ADD140_EXTENSION nul_surrogates");
    string(
        "function id(x){return x;}id('\\0\\ud800')+id('\\udc00');",
        &[0, 55296, 56320],
    );
}

#[test]
fn addition140_extensions_string_1() {
    eprintln!("ADD140_EXTENSION minus_zero");
    string("function id(x){return x;}id(-0)+id('');", &[48]);
    eprintln!("ADD140_EXTENSION nan");
    string("function id(x){return x;}id(NaN)+id('');", &[78, 97, 78]);
    eprintln!("ADD140_EXTENSION infinity");
    string(
        "function id(x){return x;}id(Infinity)+id('');",
        &[73, 110, 102, 105, 110, 105, 116, 121],
    );
    eprintln!("ADD140_EXTENSION negative_infinity");
    string(
        "function id(x){return x;}id(-Infinity)+id('');",
        &[45, 73, 110, 102, 105, 110, 105, 116, 121],
    );
}

#[test]
fn addition140_extensions_string_2() {
    eprintln!("ADD140_EXTENSION cutoff_small");
    string(
        "function id(x){return x;}id(0.000001)+id('');",
        &[48, 46, 48, 48, 48, 48, 48, 49],
    );
    eprintln!("ADD140_EXTENSION cutoff_exp");
    string(
        "function id(x){return x;}id(0.0000001)+id('');",
        &[49, 101, 45, 55],
    );
    eprintln!("ADD140_EXTENSION cutoff_large");
    string(
        "function id(x){return x;}id(1e21)+id('');",
        &[49, 101, 43, 50, 49],
    );
    eprintln!("ADD140_EXTENSION cutoff_fixed");
    string(
        "function id(x){return x;}id(1e20)+id('');",
        &[
            49, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48,
        ],
    );
}

#[test]
fn addition140_extensions_number() {
    eprintln!("ADD140_EXTENSION false");
    number("function id(x){return x;}id(false)+id(null);", 0.0);
    eprintln!("ADD140_EXTENSION negative_zero");
    number("function id(x){return x;}id(-0)+id(-0);", -0.0);
    eprintln!("ADD140_EXTENSION zero_sign");
    number("function id(x){return x;}id(-0)+id(null);", 0.0);
    eprintln!("ADD140_EXTENSION nan");
    number("function id(x){return x;}id(NaN)+id(2);", f64::NAN);
    eprintln!("ADD140_EXTENSION infinity");
    number(
        "function id(x){return x;}id(Infinity)+id(2);",
        f64::INFINITY,
    );
    eprintln!("ADD140_EXTENSION infinite_cancel");
    number(
        "function id(x){return x;}id(Infinity)+id(-Infinity);",
        f64::NAN,
    );
}

#[test]
fn addition140_provenance_0() {
    number("function f(x){var y=x;return y+true;}f(2);", 3.0);
    number("function f(x){return (true?x:null)+1;}f(2);", 3.0);
    number("function f(x){return (x||null)+1;}f(2);", 3.0);
    number("function f(x){var y;return (y=x)+1;}f(2);", 3.0);
}

#[test]
fn addition140_provenance_1() {
    number("function f(x){return (null,x)+1;}f(2);", 3.0);
    number("try{throw true;}catch(e){e+null;}", 1.0);
    string("var o={get x(){return 'x';}};o.x+1;", &[120, 49]);
    number(
        "function f(){var undefined=true;return undefined+null;}f();",
        1.0,
    );
}

#[test]
fn addition140_provenance_2() {
    number("'use strict';function f(x){return x+null;}f(true);", 1.0);
    number("function f(x){'use strict';return x+null;}f(true);", 1.0);
    number(
        "function f(x){function g(y){return y+null;}return g(x);}f(true);",
        1.0,
    );
    number(
        "function f(g,n){if(n===0)return true;return g(g,n-1)+null;}f(f,2);",
        1.0,
    );
}

#[test]
fn addition140_getvalue_order_and_abrupt() {
    for (source, expected) in [
        (
            "var o={n:0,get a(){this.n=this.n*10+1;return 'x';},get b(){this.n=this.n*10+2;return 1;}};var r=o.a+o.b;if(r!=='x1')throw 'bad';o.n;",
            12.0,
        ),
        (
            "var o={n:0,get a(){this.n=1;throw 'L';},get b(){this.n=99;return 1;}};try{o.a+o.b;}catch(e){if(e!=='L')throw 'bad';}o.n;",
            1.0,
        ),
        (
            "var o={get a(){return {};},get b(){throw 'R';}};var n=0;try{o.a+o.b;}catch(e){n=e==='R'?1:99;}n;",
            1.0,
        ),
        (
            "var o={n:0,f:function(){this.n=this.n*10+1;return 'x';},g:function(){this.n=this.n*10+2;return true;}};var r=o.f()+o.g();if(r!=='xtrue')throw 'bad';o.n;",
            12.0,
        ),
        (
            "function f(a,b){return a+b;}var o={n:0};var r=f((o.n=o.n*10+1,'x'),(o.n=o.n*10+2,true),(o.n=o.n*10+3,99));if(r!=='xtrue')throw 'bad';o.n;",
            123.0,
        ),
        (
            "var o={n:0,get a(){this.n=1;return 'x';},get b(){this.n=this.n+1;throw 'R';}};try{o.a+o.b;}catch(e){if(e!=='R')throw 'bad';}finally{o.n=o.n+1;}o.n;",
            3.0,
        ),
    ] {
        number(&format!("'use strict';{source}"), expected);
    }
}
#[test]
fn addition140_runtime_nonprimitive_guards() {
    for source in [
        "function id(x){return x;}id({})+id(1);",
        "function id(x){return x;}id([])+id(1);",
        "function id(x){return x;}id(function(){})+id(1);",
        "function id(x){return x;}id('x')+id({});",
        "function id(x){return x;}id('x')+id([]);",
        "function id(x){return x;}id('x')+id(function(){});",
    ] {
        std::thread::Builder::new().stack_size(33_554_432).spawn(move||{
            let mut m=pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source).unwrap();
            m.set_jit_config(vm::JitConfig{enabled:false,..*m.jit_config()});m.set_fuel(10_000_000);
            assert!(!m.jit_config().enabled);assert_eq!((m.aot_exec_count(),m.jit_native_exec_count()),(0,0));
            let status=m.run();assert!(matches!(&status,Err(vm::VmError::HostError(e)) if e=="assertion failed"));
            eprintln!("ADD140_GUARD_VM {}",serde_json::json!({"source":source,"status":format!("{status:?}"),"aot":m.aot_exec_count(),"jit":m.jit_native_exec_count(),"jit_enabled":m.jit_config().enabled}));
            assert_eq!((m.aot_exec_count(),m.jit_native_exec_count()),(0,0));reset(&mut m);
        }).unwrap().join().unwrap();
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
#[test]
fn addition140_demand_and_refusal_boundaries() {
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

    // Existing138 numeric consumers admit Dynamic and retain their own guards.
    number("function f(x){return (x+1)-2;}f(1);", 0.0);
    for (source, formatter, fixed, parser) in [
        ("true+null;", false, false, 0),
        ("function f(x){return x+true;}f(false);", false, true, 0),
        ("function f(x,y){return x+y;}f(false,true);", true, true, 0),
        (
            "+'1';function f(x,y){return x+y;}f(false,true);",
            true,
            true,
            1,
        ),
        (
            "Math.abs('1');function f(x,y){return x+y;}f(false,true);",
            true,
            true,
            1,
        ),
    ] {
        let ir = parse(source).unwrap();
        let count = |name| ir.functions.iter().filter(|f| f.name == name).count();
        assert_eq!(
            count("__compat_es5_number_string_managed"),
            usize::from(formatter)
        );
        assert_eq!(
            count("__compat_es5_fixed_scalar_string_managed"),
            usize::from(fixed)
        );
        assert_eq!(count("__compat_es5_numeric_scan"), parser);
        assert_eq!(
            ir.functions
                .iter()
                .map(|f| f.index)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            ir.functions.len()
        );
        assert!(
            ir.function_impls
                .values()
                .all(|f| f.param_slots.iter().all(|s| usize::from(*s) < ir.locals))
        );
        if source == "true+null;" {
            assert!(ir.functions.is_empty());
        }
        eprintln!(
            "ADD140_DEMAND source={source:?} formatter={formatter} fixed={fixed} parser={parser} unique_indices=true locals={}",
            ir.locals
        );
    }
    for source in [
        "({})+1;",
        "[]+1;",
        "(function(){})+1;",
        "function f(){var undefined={};return 'x'+undefined;}f();",
        "function f(x){return (x+1)<2;}f(1);",
    ] {
        if source == "function f(x){return (x+1)<2;}f(1);" {
            relational_positive(source, false);
            continue;
        }
        let error =
            parse(source).expect_err("proved nonprimitive or unproved downstream numeric profile");
        assert!(error.span.is_some());
        assert!(error.message.contains("numeric"));
        eprintln!("ADD140_REFUSAL source={source:?} guest_invocations=0 error={error:?}");
    }
}
#[test]
fn addition140_float_operand_typemap() {
    for source in ["true+null;", "false+undefined;"] {
        let m =
            pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source).unwrap();
        let p = m.program();
        let adds: Vec<_> = p
            .type_map
            .as_ref()
            .unwrap()
            .operand_types
            .iter()
            .filter(|(ip, _)| p.code[**ip] == vm::bytecode::OpCode::Add as u8)
            .collect();
        assert_eq!(adds.len(), 1);
        assert_eq!(*adds[0].1, (vm::ValueType::Float, vm::ValueType::Float));
        eprintln!(
            "ADD140_FLOAT_ADD source={source:?} actual_operands={adds:?} disassembly={}",
            vm::vmbc::disassemble_program(p)
        );
    }
}
#[test]
fn addition140_public_source_file_and_managed_throw() {
    let positive = "function f(a,b){return a+b;}if(f('x',true)!=='xtrue')throw 'bad';1;";
    number(positive, 1.0);
    for (label, source, throws) in [
        ("positive", positive, false),
        (
            "throw",
            "function f(a,b){return a+b;}throw f('X\\0\\ud800',true);",
            true,
        ),
    ] {
        let source = source.to_owned();
        std::thread::Builder::new()
            .stack_size(33_554_432)
            .spawn(move || {
                let path = std::env::temp_dir()
                    .join(format!("addition140-{label}-{}.js", std::process::id()));
                std::fs::write(&path, &source).unwrap();
                let result =
                    pd_vm_compat_frontends::javascript::corpus::observe_javascript_es5_corpus_file(
                        &path,
                    );
                eprintln!("ADD140_FILE source={source:?} path={path:?} result={result:?}");
                if throws {
                    assert!(
                        format!("{:?}", result.expect_err("uncaught managed throw"))
                            .contains("uncaught JavaScript throw")
                    );
                } else {
                    let o = result.unwrap();
                    assert_eq!(o.cookie, 1);
                    assert_eq!((o.aot_exec_count, o.jit_native_exec_count), (0, 0));
                    assert!(
                        o.certification_valid_before_reset
                            && !o.certification_valid_after_reset
                            && o.owned_arrays_retired
                    );
                    assert_eq!(o.resources_after_reset, 0);
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }
    static OBS: object_observer::Observer = object_observer::Observer {
        units: Some(7),
        force_gc: true,
        uncaught: true,
        type_error: false,
        realm_header: false,
    };
    let source = "function f(a,b){return a+b;}try{throw f('X\\0\\ud800',true);}finally{var o={x:'finally'};o.x;}";
    std::thread::Builder::new().stack_size(33_554_432).spawn(move||{
        let mut m=probe_vm(source,&OBS,"javascript-es5-object-observer");assert!(!m.jit_config().enabled);assert_eq!((m.aot_exec_count(),m.jit_native_exec_count()),(0,0));let status=m.run();assert!(format!("{:?}",status.as_ref().unwrap_err()).contains("uncaught JavaScript throw"));
        let at=m.stack().iter().position(|v|v==&vm::Value::string("P7_OBSERVED")).unwrap();
        assert_eq!(m.stack()[at+2],vm::Value::Float(7.0));for (i,u) in [88,0,0xd800,116,114,117,101].iter().enumerate(){assert_eq!(m.stack()[at+3+i],vm::Value::Float(f64::from(*u)));}
        eprintln!("ADD140_THROW_VM source={source:?} status={status:?} units=[88,0,55296,116,114,117,101] aot={} jit={} jit_enabled={}",m.aot_exec_count(),m.jit_native_exec_count(),m.jit_config().enabled);assert_eq!((m.aot_exec_count(),m.jit_native_exec_count()),(0,0));reset(&mut m);
    }).unwrap().join().unwrap();
}

// Runtime assertions in executed producer bodies, with real carrier counters.
struct AdditionGcObserver {
    units: usize,
    formatter: bool,
}
impl vm::SourcePlugin for AdditionGcObserver {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut d = vm::SourcePlugin::descriptor(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN);
        d.id = vm::FrontendId::new("addition140-gc-observer");
        d
    }
    fn parse_source(
        &self,
        source: &str,
        cx: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S, TypeSchema as T};
        let output = object_observer::Observer {
            units: Some(self.units),
            force_gc: true,
            ..Default::default()
        };
        let mut ir = vm::SourcePlugin::parse_source(&output, source, cx)?;
        let h = |n: &str| ir.functions.iter().find(|f| f.name == n).unwrap().index;
        let (read, decode, collect, checked, concat, to, enter, leave, from) = (
            h("__compat_ref_read"),
            h("__compat_es5_decode_int"),
            h("__compat_es5_heap_collect"),
            h("__compat_es5_checked_base"),
            h("__compat_es5_string_concat"),
            h("__compat_es5_managed_to_units"),
            h("__compat_es5_root_enter"),
            h("__compat_es5_root_leave"),
            h("__compat_es5_units_to_managed"),
        );
        let formatter = if self.formatter {
            Some(h("__compat_es5_number_string_managed"))
        } else {
            None
        };
        let mut next = ir.locals;
        let mut fresh = || {
            let n = u16::try_from(next).unwrap();
            next += 1;
            n
        };
        let call = |i, args| E::Call(i, vec![], args, None, None);
        let num = |e| E::Div(Box::new(e), Box::new(E::Float(1.0)));
        let get = |v, k: &str| {
            call(
                vm::BuiltinFunction::Get.call_index(),
                vec![E::Borrow(Box::new(E::Var(v))), E::String(k.into())],
            )
        };
        let add = |a, b| E::Add(Box::new(a), Box::new(b));
        let eq = |a, b| E::Eq(Box::new(num(a)), Box::new(num(b)));
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
        for id in std::iter::once(concat).chain(formatter) {
            let f = ir.function_impls.get_mut(&id).unwrap();
            let p = &f.param_slots;
            let c = p[0];
            let is_concat = id == concat;
            let frame = if is_concat {
                f.body_stmts
                    .iter()
                    .find_map(|s| match s {
                        S::Assign {
                            index,
                            expr: E::Call(i, _, _, _, _),
                            ..
                        } if *i == enter => Some(*index),
                        _ => None,
                    })
                    .unwrap()
            } else {
                p[2]
            };
            let (epoch, ack, cursor, found, row) = (fresh(), fresh(), fresh(), fresh(), fresh());
            let rd = |e| call(decode, vec![num(call(read, vec![E::Var(c), num(e)]))]);
            let base = |slot, generation| call(checked, vec![E::Var(c), slot, generation]);
            let rowexpr = |entry| {
                add(
                    add(E::Var(frame), E::Int(4)),
                    E::Mul(Box::new(entry), Box::new(E::Int(3))),
                )
            };
            let mut inject = vec![
                let_(epoch, T::Int, rd(E::Int(11))),
                let_(ack, T::Int, call(collect, vec![E::Var(c)])),
                check(eq(rd(E::Int(11)), add(E::Var(epoch), E::Int(1)))),
                let_(cursor, T::Int, E::Int(0)),
                let_(found, T::Int, E::Int(0)),
                let_(row, T::Int, E::Int(0)),
            ];
            inject.push(S::While {
                condition: E::Lt(
                    Box::new(E::Var(cursor)),
                    Box::new(rd(add(E::Var(frame), E::Int(3)))),
                ),
                body: vec![
                    set(row, rowexpr(E::Var(cursor))),
                    S::IfElse {
                        condition: eq(rd(E::Var(row)), E::Int(4)),
                        then_branch: vec![
                            check(E::Gt(
                                Box::new(base(
                                    rd(add(E::Var(row), E::Int(1))),
                                    rd(add(E::Var(row), E::Int(2))),
                                )),
                                Box::new(E::Int(0)),
                            )),
                            set(found, add(E::Var(found), E::Int(1))),
                        ],
                        else_branch: vec![],
                        line: 1,
                    },
                    set(cursor, add(E::Var(cursor), E::Int(1))),
                ],
                line: 1,
            });
            inject.push(check(E::Gt(
                Box::new(E::Var(found)),
                Box::new(E::Int(if is_concat { 1 } else { 0 })),
            )));
            if is_concat {
                // The original helper has installed both private input roots.
                let at = f
                    .body_stmts
                    .iter()
                    .position(|s| matches!(s,S::Let{expr:E::Call(i,_,_,_,_),..}if *i==to))
                    .unwrap();
                for (entry, input) in [(0, p[1]), (1, p[2])] {
                    let row = rowexpr(E::Int(entry));
                    inject.extend([
                        check(eq(rd(row.clone()), E::Int(4))),
                        check(eq(rd(add(row.clone(), E::Int(1))), get(input, "ref_slot"))),
                        check(eq(rd(add(row, E::Int(2))), get(input, "generation"))),
                    ]);
                }
                f.body_stmts.splice(at..at, inject);
                let out = f
                    .body_stmts
                    .iter()
                    .find_map(|s| match s {
                        S::Assign {
                            index,
                            expr: E::Call(i, _, _, _, _),
                            ..
                        } if *i == from => Some(*index),
                        _ => None,
                    })
                    .unwrap();
                let at = f
                    .body_stmts
                    .iter()
                    .position(|s| matches!(s,S::Assign{expr:E::Call(i,_,_,_,_),..}if *i==leave))
                    .unwrap();
                let caller = p[3];
                let entry = p[4];
                let a = add(
                    add(E::Var(caller), E::Int(4)),
                    E::Mul(Box::new(E::Var(entry)), Box::new(E::Int(3))),
                );
                // Assert publication with no collection in the return handoff.
                f.body_stmts.splice(
                    at..at,
                    [
                        check(eq(rd(a.clone()), E::Int(4))),
                        check(eq(rd(add(a.clone(), E::Int(1))), get(out, "ref_slot"))),
                        check(eq(rd(add(a, E::Int(2))), get(out, "generation"))),
                    ],
                );
            } else {
                f.body_stmts.splice(0..0, inject);
            }
        }
        ir.locals = next;
        Ok(ir)
    }
}
#[test]
fn addition140_real_gc_inputs_conversion_and_publication() {
    for (source, units, formatter) in [
        (
            "function f(a,b){return a+b;}f('A\\0\\ud800',1.5);",
            vec![65, 0, 0xd800, 49, 46, 53],
            true,
        ),
        (
            "function f(a,b){return a+b;}f(1.5,'B\\0\\udc00');",
            vec![49, 46, 53, 66, 0, 0xdc00],
            true,
        ),
        (
            "function f(a,b){return a+b;}f('A\\0\\ud800','B\\0\\udc00');",
            vec![65, 0, 0xd800, 66, 0, 0xdc00],
            false,
        ),
        (
            "function f(a,b){return a+b;}f(true,'B\\0\\udc00');",
            vec![116, 114, 117, 101, 66, 0, 0xdc00],
            false,
        ),
    ] {
        oracle_units(source, &units);
        std::thread::Builder::new().stack_size(33_554_432).spawn(move||{
            let observer=Box::leak(Box::new(AdditionGcObserver{units:units.len(),formatter}));
            let mut m=probe_vm(source,observer,"addition140-gc-observer");assert!(!m.jit_config().enabled);assert_eq!((m.aot_exec_count(),m.jit_native_exec_count()),(0,0));
            let status=m.run();assert_eq!(status.unwrap(),vm::VmStatus::Halted);
            let at=m.stack().iter().position(|v|v==&vm::Value::string("P7_OBSERVED")).unwrap();assert_eq!(m.stack()[at+2],vm::Value::Float(units.len() as f64));
            for(i,u)in units.iter().enumerate(){assert_eq!(m.stack()[at+3+i],vm::Value::Float(f64::from(*u)));}
            assert_eq!((m.aot_exec_count(),m.jit_native_exec_count()),(0,0));
            eprintln!("ADD140_GC_VM {}",serde_json::json!({"source":source,"units":units,"status":"Halted","formatter_window":formatter,"executed_concat_private_two_roots":true,"collect_counter_increment_asserted":true,"caller_publication_asserted":true,"aot":m.aot_exec_count(),"jit":m.jit_native_exec_count(),"jit_enabled":m.jit_config().enabled}));reset(&mut m);
        }).unwrap().join().unwrap();
    }
}
