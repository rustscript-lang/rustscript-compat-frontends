//! Public Micro Math extensions on the certified, JIT-disabled interpreter.
use pd_vm_compat_frontends::{JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor};
use vm::{HostFunctionRegistry, Value, VmStatus};

fn free_floor() {
    let text = std::fs::read_to_string("/proc/meminfo").expect("meminfo");
    let kb = text
        .lines()
        .find_map(|line| line.strip_prefix("MemAvailable:"))
        .expect("MemAvailable")
        .split_whitespace()
        .next()
        .expect("kb")
        .parse::<u64>()
        .expect("kb number");
    assert!(
        kb.saturating_mul(1024) >= 8_589_934_592,
        "free floor below 8589934592 bytes: {kb}kB"
    );
}

fn field(value: &Value, name: &str) -> Value {
    let Value::Map(map) = value else {
        panic!("boxed value: {value:?}")
    };
    map.get(&Value::string(name))
        .unwrap_or_else(|| panic!("boxed field {name}"))
        .clone()
}

fn prepare(source: &str) -> vm::Vm {
    free_floor();
    eprintln!("MATH_EXT_SOURCE {source}");
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_host_api_catalog(catalog.clone())
        .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN);
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        javascript_es5_flavor(),
        options,
    )
    .unwrap_or_else(|error| panic!("source must compile: {source}\n{error:?}"));
    eprintln!("MATH_EXT_COMPILED");
    let mut machine = compiled.into_vm().expect("certified VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("owned arrays");
    let mut registry = HostFunctionRegistry::restricted();
    for module in [
        pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
        pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module(),
    ] {
        module
            .install_from_catalog(&mut registry, &catalog)
            .expect("exact catalog");
    }
    for name in [
        "abs",
        "acos",
        "asin",
        "atan",
        "atan2",
        "ceil",
        "copysign",
        "cos",
        "exp",
        "exp2",
        "floor",
        "is_finite",
        "is_infinite",
        "is_nan",
        "ln",
        "log10",
        "log2",
        "powf",
        "signum",
        "sin",
        "sqrt",
        "tan",
        "trunc",
    ] {
        registry
            .allow_builtin(format!("math::{name}"))
            .unwrap_or_else(|error| panic!("numeric builtin math::{name}: {error:?}"));
    }
    registry
        .allow_builtin("bytes::from_utf8")
        .expect("retained numeric formatter UTF-8 conversion");
    registry.bind_vm_cached(&mut machine).expect("binding");
    machine.set_fuel(10_000_000);
    machine
}

fn execute(source: &str) -> Value {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut machine = prepare(&source);
            let before = machine.get_fuel();
            assert!(
                matches!(before, Some(fuel) if fuel <= 10_000_000),
                "interpreter fuel {before:?}"
            );
            let status = machine.run();
            let after = machine.get_fuel();
            eprintln!(
                "MATH_EXT_VM status={status:?} jit_enabled={} aot={} jit_native={} fuel_before={before:?} fuel_after={after:?} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(
                status.unwrap_or_else(|error| panic!("{source}: {error:?}")),
                VmStatus::Halted,
                "{source}"
            );
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            assert!(
                matches!((before, after), (Some(left), Some(right)) if right <= left && right <= 10_000_000)
            );
            assert!(machine
                .capture_certification_stats()
                .is_some_and(|stats| stats.valid));
            let resources = machine.host_context().resource_count();
            assert!(resources <= 1, "at most one real B1 realm");
            let value = machine.stack().last().expect("completion").clone();
            machine.reset_for_reuse().expect("reset");
            assert_eq!(machine.host_context().resource_count(), 0);
            value
        })
        .expect("stack")
        .join()
        .expect("test thread")
}

fn number_bits(source: &str) -> (i64, u64) {
    let result = execute(source);
    let tag = match field(&result, "tag") {
        Value::Int(tag) => tag,
        other => panic!("tag: {other:?}"),
    };
    let bits = match field(&result, "number") {
        Value::Float(number) => number.to_bits(),
        Value::Int(number) => (number as f64).to_bits(),
        other => panic!("number: {other:?}"),
    };
    (tag, bits)
}

fn case_code(source: &str) -> i64 {
    let result = execute(source);
    match field(&result, "number") {
        Value::Float(number) => number as i64,
        Value::Int(number) => number,
        other => panic!("case: {other:?}\n{source}"),
    }
}

#[test]
fn math_ext_sign_public_value() {
    let code = case_code(
        "var n=0;var z=1/(-1/0);if(!(Math.sign(-2)===-1))n=1;else if(!(Math.sign(2)===1))n=2;else if(!(Math.sign(0)===0))n=3;else if(!(1/Math.sign(z)===1/z))n=4;else if(Math.sign(0/0)===Math.sign(0/0))n=5;else if(!(Math.sign(1/0)===1))n=6;else if(!(Math.sign(-1/0)===-1))n=7;else if(Math.sign()===Math.sign())n=8;else if(!(Math.sign(true)===1))n=9;else if(!(Math.sign(null)===0))n=10;else if(!(Math.sign('-2')===-1))n=11;n;",
    );
    assert_eq!(code, 0, "Math.sign case");
}

#[test]
fn math_ext_integer_ops() {
    let imul = case_code(
        "var n=0;if(!(Math.imul(2,3)===6))n=1;else if(!(Math.imul(-1,8)===-8))n=2;else if(!(Math.imul(4294967295,2)===-2))n=3;else if(!(Math.imul(2147483647,2)===-2))n=4;else if(!(Math.imul(-2147483648,-1)===-2147483648))n=5;else if(!(Math.imul()===0))n=6;else if(!(Math.imul(7)===0))n=7;else if(!(Math.imul(0.9,3)===0))n=8;n;",
    );
    assert_eq!(imul, 0, "Math.imul case");
    let clz = case_code(
        "var n=0;if(!(Math.clz32(0)===32))n=1;else if(!(Math.clz32(1)===31))n=2;else if(!(Math.clz32(2)===30))n=3;else if(!(Math.clz32(256)===23))n=4;else if(!(Math.clz32(2147483648)===0))n=5;else if(!(Math.clz32(-1)===0))n=6;else if(!(Math.clz32(0.9)===32))n=7;else if(!(Math.clz32()===32))n=8;n;",
    );
    assert_eq!(clz, 0, "Math.clz32 case");
}

#[test]
fn math_ext_float_ops() {
    let trunc = case_code(
        "var n=0;var z=1/(-1/0);if(!(Math.trunc(1.9)===1))n=1;else if(!(Math.trunc(-1.9)===-1))n=2;else if(!(1/Math.trunc(z)===1/z))n=3;else if(Math.trunc(0/0)===Math.trunc(0/0))n=4;else if(!(Math.trunc(1/0)===1/0))n=5;else if(!(Math.trunc(-1/0)===-1/0))n=6;n;",
    );
    assert_eq!(trunc, 0, "Math.trunc case");
    let logs = case_code(
        "var n=0;if(!(Math.log2(8)===3))n=1;else if(!(Math.log2(1)===0))n=2;else if(!(Math.log10(1000)===3))n=3;else if(!(Math.log10(1)===0))n=4;else if(Math.log2(-1)===Math.log2(-1))n=5;else if(!(Math.log2(1/0)===1/0))n=6;else if(!(Math.log2(0)===-1/0))n=7;n;",
    );
    assert_eq!(logs, 0, "Math.log2/log10 case");
    let rounded = case_code(
        "var n=0;var z=1/(-1/0);if(!(Math.fround(1)===1))n=1;else if(!(Math.fround(1.5)===1.5))n=2;else if(!(1/Math.fround(z)===1/z))n=3;else if(Math.fround(0/0)===Math.fround(0/0))n=4;else if(!(Math.fround(1/0)===1/0))n=5;else if(!(Math.fround(-1/0)===-1/0))n=6;else if(!(Math.fround(0)===0))n=7;n;",
    );
    assert_eq!(rounded, 0, "Math.fround case");
}

fn node_bits(expr: &str) -> u64 {
    let expr = expr.trim().trim_end_matches(';');
    let script = format!(
        "const u=new BigUint64Array(new Float64Array([{expr}]).buffer);process.stdout.write(u[0].toString());"
    );
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .expect("node");
    assert!(
        out.status.success(),
        "node {expr}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("node utf8")
        .trim()
        .parse()
        .expect("node bits")
}

fn expect_node(expr: &str) {
    let (tag, bits) = number_bits(expr);
    assert_eq!(tag, 3, "{expr}");
    assert_eq!(bits, node_bits(expr), "{expr}");
}

#[test]
fn math_ext_call_shape() {
    let shape = case_code(
        "var f=Math.sign;var g=Math.imul;var n=0;if(!(f.length===1))n=1;else if(!(Math.imul.length===2))n=2;else if(!(Math.clz32.length===1))n=3;else if(!(Math.fround.length===1))n=4;else if(!(Math.trunc.length===1))n=5;else if(!(Math.log2.length===1))n=6;else if(!(Math.log10.length===1))n=7;else if(!(f(-4)===-1))n=8;var s=0;Math.imul(2,3,s=s+1);if(!(n===0))n=n;else if(!(s===1))n=13;Math.sign=99;delete Math.imul;if(!(n===0))n=n;else if(!(f(-4)===-1))n=9;else if(!(Math.sign===99))n=10;else if('imul' in Math)n=11;else if(!(g(3,5)===15))n=12;n;",
    );
    assert_eq!(shape, 0, "Math extension call shape");
}

#[test]
fn math_ext_clz_bits() {
    for start in [0, 8, 16, 24] {
        let mut bits = String::from("var n=0;");
        let mut value = 1i64 << start;
        for bit in start..start + 8 {
            let expect = 31 - bit;
            let case = bit + 1;
            if bit == start {
                bits.push_str(&format!("if(!(Math.clz32({value})==={expect}))n={case};"));
            } else {
                bits.push_str(&format!(
                    "else if(!(Math.clz32({value})==={expect}))n={case};"
                ));
            }
            value = value.saturating_mul(2);
        }
        bits.push_str("n;");
        assert_eq!(case_code(&bits), 0, "clz32 bits from {start}");
    }
}

#[test]
fn math_ext_boundaries() {
    let code = case_code(
        "var n=0;if(!(Math.fround(Math.pow(2,128))===1/0))n=1;else if(!(Math.fround(-Math.pow(2,128))===-1/0))n=2;else if(!(Math.fround(Math.pow(2,-149))===Math.pow(2,-149)))n=3;else if(!(Math.fround(Math.pow(2,-150))===0))n=4;else if(!(1/Math.fround(-Math.pow(2,-150))===-1/0))n=5;else if(!(Math.imul(65535,65535)===-131071))n=6;n;",
    );
    assert_eq!(code, 0, "Math extension boundaries");
    expect_node("Math.fround(0.1);");
    expect_node("Math.fround(1.0000000596046448);");
    expect_node("Math.fround(1.0000001788139343);");
    expect_node("Math.log2(3);");
    expect_node("Math.log10(3);");
}

fn expect_bits(expr: &str, expected: f64) {
    let (tag, bits) = number_bits(expr);
    assert_eq!(tag, 3, "{expr}");
    assert_eq!(
        bits,
        expected.to_bits(),
        "{expr} got={bits:#x} expected={:#x}",
        expected.to_bits()
    );
}

fn expect_nan_class(expr: &str) {
    let (tag, bits) = number_bits(expr);
    assert_eq!(tag, 3, "{expr}");
    assert!(
        f64::from_bits(bits).is_nan(),
        "{expr} NaN class bits={bits:#x}"
    );
}

fn expect_object_values(cases: &[(&str, f64)]) {
    assert!(
        cases.len() <= 2,
        "object value shard exceeds timed source budget"
    );
    for (expr, expected) in cases {
        expect_bits(expr, *expected);
    }
}

/// sign and imul left. Two sources.
#[test]
fn math_ext_object_value_sign_imul() {
    expect_object_values(&[
        (
            "'use strict';var o={valueOf:function(){return -2;}};Math.sign(o);",
            -1.0,
        ),
        (
            "'use strict';var o={valueOf:function(){return 7;}};Math.imul(o,3);",
            21.0,
        ),
    ]);
}

/// imul right and clz32. Two sources.
#[test]
fn math_ext_object_value_imul_clz() {
    expect_object_values(&[
        (
            "'use strict';var o={valueOf:function(){return 7;}};Math.imul(3,o);",
            21.0,
        ),
        (
            "'use strict';var o={valueOf:function(){return 1;}};Math.clz32(o);",
            31.0,
        ),
    ]);
}

/// fround and trunc object consumption. Two sources.
#[test]
fn math_ext_object_value_fround_trunc() {
    assert_ne!(1.0000000596046448f64.to_bits(), 1.0f64.to_bits());
    expect_object_values(&[
        (
            "'use strict';var o={valueOf:function(){return 1.0000000596046448;}};Math.fround(o);",
            1.0,
        ),
        (
            "'use strict';var o={valueOf:function(){return 1.9;}};Math.trunc(o);",
            1.0,
        ),
    ]);
}

/// log2 and log10 object consumption. Two sources.
#[test]
fn math_ext_object_value_logs() {
    expect_object_values(&[
        (
            "'use strict';var o={valueOf:function(){return 8;}};Math.log2(o);",
            3.0,
        ),
        (
            "'use strict';var o={valueOf:function(){return 1000;}};Math.log10(o);",
            3.0,
        ),
    ]);
}

fn getter_order_source(checks: &[(&str, &str, i64)]) -> String {
    let mut source = String::from("'use strict';var n=0;");
    for (index, (call, returned, case)) in checks.iter().enumerate() {
        source.push_str(&format!(
            "var o{index}={{n:0,get valueOf(){{this.n=1;return this.hidden;}},hidden:function(){{this.n=2;return {returned};}},toString:function(){{this.n=99;return '9';}}}};if(n===0&&!(Math.{call}&&o{index}.n===2))n={case};"
        ));
    }
    source.push_str("n;");
    source
}

/// Getter Get, then valueOf call, then method consumption. Trace lives on this.
/// Two sources so each program stays inside the interpreter fuel cap.
#[test]
fn math_ext_object_getter_order() {
    let first = getter_order_source(&[
        ("sign(o0)===-1", "-2", 1),
        ("imul(o1,3)===21", "7", 2),
        ("clz32(o2)===31", "1", 3),
        ("fround(o3)===1", "1.0000000596046448", 4),
    ]);
    let second = getter_order_source(&[
        ("trunc(o0)===1", "1.9", 5),
        ("log2(o1)===3", "8", 6),
        ("log10(o2)===3", "1000", 7),
        ("imul(3,o3)===21", "7", 8),
    ]);
    assert_eq!(case_code(&first), 0, "getter order 1-4");
    assert_eq!(case_code(&second), 0, "getter order 5-8");
}

fn throw_source(kind: &str, rows: &[(&str, i64, i64)]) -> String {
    let mut source = String::from("'use strict';var n=0;");
    for (index, (method, payload, case)) in rows.iter().enumerate() {
        let body = if kind == "valueOf" {
            format!("valueOf:function(){{this.n=1;throw {payload};}}")
        } else {
            format!("get valueOf(){{this.n=1;throw {payload};}}")
        };
        let call = if *method == "imul" {
            format!("Math.imul(o{index},1)")
        } else {
            format!("Math.{method}(o{index})")
        };
        source.push_str(&format!(
            "var o{index}={{n:0,{body},toString:function(){{this.n=99;return '2';}}}};try{{{call};if(n===0)n={case};}}catch(e){{if(n===0&&!(e==={payload}&&o{index}.n===1))n={case};}}"
        ));
    }
    source.push_str("n;");
    source
}

/// valueOf throw. Caught payload and own-field side effect. Two sources.
#[test]
fn math_ext_object_throw_valueof() {
    let value_a = [
        ("sign", 11, 1),
        ("imul", 12, 2),
        ("clz32", 13, 3),
        ("fround", 14, 4),
    ];
    let value_b = [("trunc", 15, 5), ("log2", 16, 6), ("log10", 17, 7)];
    assert_eq!(
        case_code(&throw_source("valueOf", &value_a)),
        0,
        "valueOf throw 1-4"
    );
    assert_eq!(
        case_code(&throw_source("valueOf", &value_b)),
        0,
        "valueOf throw 5-7"
    );
}

/// getter throw. Caught payload and own-field side effect. Two sources.
#[test]
fn math_ext_object_throw_getter() {
    let getter_a = [
        ("sign", 21, 8),
        ("imul", 22, 9),
        ("clz32", 23, 10),
        ("fround", 24, 11),
    ];
    let getter_b = [("trunc", 25, 12), ("log2", 26, 13), ("log10", 27, 14)];
    assert_eq!(
        case_code(&throw_source("getter", &getter_a)),
        0,
        "getter throw 8-11"
    );
    assert_eq!(
        case_code(&throw_source("getter", &getter_b)),
        0,
        "getter throw 12-14"
    );
}

/// imul left-to-right conversion, first throw skips the second, extra argument
/// is evaluated and an unused object is not coerced. One source.
#[test]
fn math_ext_imul_argument_edges() {
    let source = "'use strict';var n=0;var t={n:0};var left={owner:t,valueOf:function(){this.owner.n=1;return 7;}};var right={owner:t,valueOf:function(){this.owner.n=2;return 3;}};if(!(Math.imul(left,right)===21&&t.n===2))n=1;var t2={n:0};var bad={owner:t2,valueOf:function(){this.owner.n=1;throw 7;}};var later={owner:t2,valueOf:function(){this.owner.n=2;return 3;}};if(n===0){try{Math.imul(bad,later);n=2;}catch(e){if(!(e===7&&t2.n===1))n=2;}}var extra={n:0,valueOf:function(){this.n=1;return 5;}};var seen={n:0};if(n===0&&!(Math.imul(2,3,(seen.n=1,extra))===6&&seen.n===1&&extra.n===0))n=3;if(n===0&&!(1/Math.imul(1,0/0)===1/0))n=4;if(n===0&&!(1/Math.imul(1,1/0)===1/0))n=5;if(n===0&&!(1/Math.imul(1,0)===1/0))n=6;if(n===0&&!(1/Math.imul(1,1/(-1/0))===1/0))n=7;n;";
    assert_eq!(case_code(source), 0, "imul argument edges");
}

/// imul NaN/Inf/+0/-0 and clz32 NaN/Inf/+0/-0. Exact +0 bits, tag 3. Eight sources.
#[test]
fn math_ext_special_imul_clz() {
    assert_eq!(0.0f64.to_bits(), 0);
    assert_ne!((-0.0f64).to_bits(), 0);
    let zeros = [
        "Math.imul(0/0,1);",
        "Math.imul(1/0,1);",
        "Math.imul(0,1);",
        "Math.imul(1/(-1/0),1);",
    ];
    assert_eq!(zeros.len(), 4);
    for expr in zeros {
        expect_bits(expr, 0.0);
    }
    let clz = [
        "Math.clz32(0/0);",
        "Math.clz32(1/0);",
        "Math.clz32(0);",
        "Math.clz32(1/(-1/0));",
    ];
    assert_eq!(clz.len(), 4);
    for expr in clz {
        expect_bits(expr, 32.0);
    }
}

/// Negative fround normal and both tie parities, fround(-0), log2(-0)/subnormal,
/// log10(0)/negative. Eight sources. Literals avoid the `+` formatter so this
/// harness does not need bytes::from_utf8. fround uses the f32 cast; logs use f64.
#[test]
fn math_ext_special_fround_log() {
    let tie_even: f64 = -1.0000000596046448;
    let tie_odd: f64 = -1.0000001788139343;
    let normal: f64 = -1.000000149011612;
    assert_eq!(tie_even.to_bits(), (-(1.0 + (-24f64).exp2())).to_bits());
    assert_eq!(
        tie_odd.to_bits(),
        (-(1.0 + (-23f64).exp2() + (-24f64).exp2())).to_bits()
    );
    assert_eq!(
        normal.to_bits(),
        (-(1.0 + (-23f64).exp2() + (-25f64).exp2())).to_bits()
    );
    let rounded = [
        (tie_even as f32 as f64, "Math.fround(-1.0000000596046448);"),
        (tie_odd as f32 as f64, "Math.fround(-1.0000001788139343);"),
        (normal as f32 as f64, "Math.fround(-1.000000149011612);"),
    ];
    assert_eq!(rounded[0].0.to_bits(), (-1.0f64).to_bits());
    assert_ne!(rounded[1].0.to_bits(), rounded[0].0.to_bits());
    assert_ne!(rounded[2].0.to_bits(), rounded[0].0.to_bits());
    assert_ne!(rounded[2].0.to_bits(), rounded[1].0.to_bits());
    for (expected, expr) in rounded {
        expect_bits(expr, expected);
    }
    expect_bits("Math.fround(1/(-1/0));", -0.0);
    let sub = (-1023f64).exp2();
    assert!(sub.is_subnormal());
    assert_eq!(sub.log2().to_bits(), (-1023.0f64).to_bits());
    assert_eq!((-0.0f64).log2().to_bits(), f64::NEG_INFINITY.to_bits());
    expect_bits("Math.log2(1/(-1/0));", (-0.0f64).log2());
    expect_bits("Math.log2(Math.pow(2,-1023));", sub.log2());
    assert!(0.0f64.log10().is_infinite() && 0.0f64.log10().is_sign_negative());
    assert!((-1.0f64).log10().is_nan());
    expect_bits("Math.log10(0);", 0.0f64.log10());
    expect_nan_class("Math.log10(-1);");
}

/// log10 NaN/Inf, missing fround/log2/log10/trunc, trunc string. Eight sources.
#[test]
fn math_ext_special_missing_trunc() {
    assert!(f64::NAN.log10().is_nan());
    assert_eq!(f64::INFINITY.log10().to_bits(), f64::INFINITY.to_bits());
    expect_nan_class("Math.log10(0/0);");
    expect_bits("Math.log10(1/0);", f64::INFINITY.log10());
    expect_nan_class("Math.fround();");
    expect_nan_class("Math.log2();");
    expect_nan_class("Math.log10();");
    expect_bits("Math.trunc('1.9');", 1.0);
    expect_bits("Math.trunc('-1.9');", -1.0);
    expect_nan_class("Math.trunc();");
}

/// clz32(-Infinity) is the remaining Inf sign. One source. 0/-1/0.9 stay in
/// math_ext_integer_ops; all 32 positions stay in math_ext_clz_bits.
#[test]
fn math_ext_special_clz_neg_inf() {
    expect_bits("Math.clz32(-1/0);", 32.0);
}

/// Binary `**` of two numbers must reach the interpreter as one exact Float box,
/// not the Int payload the legacy scalar wrappers also accept.
#[test]
fn micro_exponentiation_binary_public_value() {
    let source = "2 ** 8;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert_eq!(number.to_bits(), 256.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    assert_eq!(number.to_bits(), node_bits(source), "Node {source}");
}

/// A NaN exponent keeps the Float box and stays NaN. Only the NaN class is
/// pinned; payload bits are never compared against the reference engine.
#[test]
fn micro_exponentiation_binary_nan_exponent() {
    let source = "1 ** (0/0);";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert!(number.is_nan(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    assert!(f64::from_bits(node_bits(source)).is_nan(), "Node {source}");
}

/// `(-1) ** Infinity` is NaN in the reference engine. NaN class only, same
/// category as the NaN exponent case.
#[test]
fn micro_exponentiation_binary_unit_infinite() {
    let source = "(-1) ** (1/0);";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert!(number.is_nan(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    assert!(f64::from_bits(node_bits(source)).is_nan(), "Node {source}");
}

/// A NaN base with a zero exponent is exactly 1.0, not NaN.
#[test]
fn micro_exponentiation_binary_nan_zero() {
    let source = "(0/0) ** 0;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert_eq!(number.to_bits(), 1.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    assert_eq!(number.to_bits(), node_bits(source), "Node {source}");
}

/// `(-0) ** 3` is negative zero. The assertion is on bits, never numeric
/// equality, because zero compares equal to its own negation.
#[test]
fn micro_exponentiation_binary_negative_zero() {
    let source = "(-0) ** 3;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert_eq!(number.to_bits(), (-0.0_f64).to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    assert_eq!(number.to_bits(), node_bits(source), "Node {source}");
}

/// A boolean primitive is sealed to 1 before the binary power.
#[test]
fn micro_exponentiation_boolean() {
    let source = "true ** 3;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert_eq!(number.to_bits(), 1.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    assert_eq!(number.to_bits(), node_bits(source), "Node {source}");
}

/// Two string primitives reach the binary power as 2 and 3, giving exactly 8.
#[test]
fn micro_exponentiation_binary_string() {
    let source = "'2' ** '3';";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert_eq!(number.to_bits(), 8.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    assert_eq!(number.to_bits(), node_bits(source), "Node {source}");
}

/// Math.pow keeps its reference NaN classification after the shared kernel
/// refactor. This pins the public method, not a math namespace operator.
#[test]
fn micro_exponentiation_shared_math_pow_nan() {
    let source = "Math.pow(1,0/0);";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert!(number.is_nan(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    assert!(f64::from_bits(node_bits(source)).is_nan(), "Node {source}");
}

/// `**` is right associative, so `2 ** 3 ** 2` is 2 ** 9.
#[test]
fn micro_exponentiation_binary_right_associative() {
    let source = "2 ** 3 ** 2;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert_eq!(number.to_bits(), 512.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    assert_eq!(number.to_bits(), node_bits(source), "Node {source}");
}

/// Compound `**=` positive tracer: x starts at 2, so x **= 3 leaves 8. The
/// reference engine is fed a function expression because node_bits wraps its
/// argument in a Float64Array literal, which cannot hold statements.
#[test]
fn micro_exponentiation_compound_public_value() {
    let source = "var x=2; x **= 3;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert_eq!(number.to_bits(), 8.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    let reference = "(function(){var x=2;x **= 3;return x;})();";
    assert_eq!(number.to_bits(), node_bits(reference), "Node {source}");
}

/// Compound `**=` also stores: x becomes 8, so y*10+x is 88.
/// The reference engine is fed an explicit function expression because node_bits
/// wraps its argument in a Float64Array literal, which cannot hold statements.
#[test]
fn micro_exponentiation_compound_local_store() {
    let source = "var x=2; var y=(x **= 3); y*10+x;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert_eq!(number.to_bits(), 88.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    let reference = "(function(){var x=2;var y=(x **= 3);return y*10+x;})()";
    assert_eq!(number.to_bits(), node_bits(reference), "Node {source}");
}

/// The left value is read before the RHS, so `x **= (x=3)` is 2 ** 3, not 3
/// ** 3.
/// The reference engine is fed an explicit function expression because node_bits
/// wraps its argument in a Float64Array literal, which cannot hold statements.
#[test]
fn micro_exponentiation_compound_old_before_rhs() {
    let source = "var x=2; x **= (x=3); x;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert_eq!(number.to_bits(), 8.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    let reference = "(function(){var x=2;x **= (x=3);return x;})()";
    assert_eq!(number.to_bits(), node_bits(reference), "Node {source}");
}

/// Chained `**=` is right associative: y becomes 9 before x **= 9.
/// The reference engine is fed an explicit function expression because node_bits
/// wraps its argument in a Float64Array literal, which cannot hold statements.
#[test]
fn micro_exponentiation_compound_right_associative() {
    let source = "var x=2; var y=3; x **= y **= 2; x*100+y;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert_eq!(number.to_bits(), 51209.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    let reference = "(function(){var x=2;var y=3;x **= y **= 2;return x*100+y;})()";
    assert_eq!(number.to_bits(), node_bits(reference), "Node {source}");
}

/// An array computed key and the RHS each evaluate exactly once.
/// The reference engine is fed an explicit function expression because node_bits
/// wraps its argument in a Float64Array literal, which cannot hold statements.
#[test]
fn micro_exponentiation_compound_array_key_rhs_once() {
    let source = "var a=[2]; var k=0; var r=(a[k++] **= (k+=1)); r*1000+a[0]*10+k;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert_eq!(number.to_bits(), 4042.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    let reference =
        "(function(){var a=[2];var k=0;var r=(a[k++] **= (k+=1));return r*1000+a[0]*10+k;})()";
    assert_eq!(number.to_bits(), node_bits(reference), "Node {source}");
}

/// An ordinary computed key and the RHS each evaluate once, key first.
/// The reference engine is fed an explicit function expression because node_bits
/// wraps its argument in a Float64Array literal, which cannot hold statements.
#[test]
fn micro_exponentiation_compound_object_key_rhs_once() {
    let source = "var o={x:2,n:0}; var r=(o[(o.n+=1,'x')] **= (o.n+=1,3)); r*1000+o.x*10+o.n;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert_eq!(number.to_bits(), 8082.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    let reference = "(function(){var o={x:2,n:0};var r=(o[(o.n+=1,'x')] **= (o.n+=1,3));return r*1000+o.x*10+o.n;})()";
    assert_eq!(number.to_bits(), node_bits(reference), "Node {source}");
}

/// A throwing RHS leaves the target at 2 and never stores; the caught 7 is
/// added.
/// The reference engine is fed an explicit function expression because node_bits
/// wraps its argument in a Float64Array literal, which cannot hold statements.
#[test]
fn micro_exponentiation_compound_rhs_throw_no_put() {
    let source =
        "var o={x:2}; var c=0; try {o.x **= (function(){throw 7;})();}catch(e){c=e;} o.x*10+c;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert_eq!(number.to_bits(), 27.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    let reference = "(function(){var o={x:2};var c=0;try{o.x **= (function(){throw 7;})();}catch(e){c=e;}return o.x*10+c;})()";
    assert_eq!(number.to_bits(), node_bits(reference), "Node {source}");
}

/// A throwing getter skips the RHS, so o.n stays 0 and the caught 7 remains.
/// The reference engine is fed an explicit function expression because node_bits
/// wraps its argument in a Float64Array literal, which cannot hold statements.
#[test]
fn micro_exponentiation_compound_getter_throw_skips_rhs() {
    let source = "var o={n:0,get x(){\"use strict\";throw 7;}}; var c=0; try{o.x **= (o.n=9);}catch(e){c=e;} o.n*10+c;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert_eq!(number.to_bits(), 7.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    let reference = "(function(){var o={n:0,get x(){\"use strict\";throw 7;}};var c=0;try{o.x **= (o.n=9);}catch(e){c=e;}return o.n*10+c;})()";
    assert_eq!(number.to_bits(), node_bits(reference), "Node {source}");
}

/// Get, then RHS, then set: getter and setter append 1 and 3 around the RHS
/// digit 2.
/// The reference engine is fed an explicit function expression because node_bits
/// wraps its argument in a Float64Array literal, which cannot hold statements.
#[test]
fn micro_exponentiation_compound_get_rhs_set_order() {
    let source = "var o={n:0,v:0,get x(){\"use strict\";this.n=this.n*10+1;return 2;},set x(v){\"use strict\";this.n=this.n*10+3;this.v=v;}}; var r=(o.x **= (o.n=o.n*10+2,3)); r*1000000+o.v*1000+o.n;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert_eq!(number.to_bits(), 8008123.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    let reference = "(function(){var o={n:0,v:0,get x(){\"use strict\";this.n=this.n*10+1;return 2;},set x(v){\"use strict\";this.n=this.n*10+3;this.v=v;}};var r=(o.x **= (o.n=o.n*10+2,3));return r*1000000+o.v*1000+o.n;})()";
    assert_eq!(number.to_bits(), node_bits(reference), "Node {source}");
}

/// Compound `**=` keeps the negative-zero sign produced by a positive odd
/// power.
/// The reference engine is fed an explicit function expression because node_bits
/// wraps its argument in a Float64Array literal, which cannot hold statements.
#[test]
fn micro_exponentiation_compound_negative_zero() {
    let source = "var x=-0; x **= 3; x;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert_eq!(number.to_bits(), (-0.0_f64).to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    let reference = "(function(){var x=-0;x **= 3;return x;})()";
    assert_eq!(number.to_bits(), node_bits(reference), "Node {source}");
}

/// A NaN base with exponent zero is exactly 1.0 through `**=`.
/// The reference engine is fed an explicit function expression because node_bits
/// wraps its argument in a Float64Array literal, which cannot hold statements.
#[test]
fn micro_exponentiation_compound_nan_zero() {
    let source = "var x=0/0; x **= 0; x;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert_eq!(number.to_bits(), 1.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    let reference = "(function(){var x=0/0;x **= 0;return x;})()";
    assert_eq!(number.to_bits(), node_bits(reference), "Node {source}");
}

/// The dense-array receiver is captured once, so rebinding `a` on the RHS keeps the
/// saved place. The alias `b` qualifies the captured target identity only; it is
/// not proof of GC root isolation.
/// The reference engine is fed an explicit function expression because node_bits
/// wraps its argument in a Float64Array literal, which cannot hold statements.
#[test]
fn micro_array_compound_receiver_rebind_rhs() {
    let source = "var a=[2]; var b=a; var i=0; var r=(a[i] **= (a=[7],3)); r*1000+b[0]*10+a[0];";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert!(number.is_finite(), "{source} finite");
    assert_eq!(number.to_bits(), 8087.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    let reference = "(function(){var a=[2];var b=a;var i=0;var r=(a[i] **= (a=[7],3));return r*1000+b[0]*10+a[0];})()";
    assert_eq!(number.to_bits(), node_bits(reference), "Node {source}");
}

/// A key-position rebind of the source name must not retarget the captured
/// receiver. The alias `b` qualifies the captured target identity only; it is not
/// proof of GC root isolation.
/// The reference engine is fed an explicit function expression because node_bits
/// wraps its argument in a Float64Array literal, which cannot hold statements.
#[test]
fn micro_array_compound_receiver_rebind_key() {
    let source = "var a=[2]; var b=a; var i=0; var r=(a[(a=[7],i)] **= 3); r*1000+b[0]*10+a[0];";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert!(number.is_finite(), "{source} finite");
    assert_eq!(number.to_bits(), 8087.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    let reference = "(function(){var a=[2];var b=a;var i=0;var r=(a[(a=[7],i)] **= 3);return r*1000+b[0]*10+a[0];})()";
    assert_eq!(number.to_bits(), node_bits(reference), "Node {source}");
}

/// A throwing RHS leaves the element at 2 and never stores; the caught 7 is added.
/// The reference engine is fed an explicit function expression because node_bits
/// wraps its argument in a Float64Array literal, which cannot hold statements.
#[test]
fn micro_array_compound_rhs_throw_no_put() {
    let source = "var a=[2]; var i=0; var c=0; try {a[i] **= (function(){throw 7;})();}catch(e){c=e;} a[0]*10+c;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert!(number.is_finite(), "{source} finite");
    assert_eq!(number.to_bits(), 27.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    let reference = "(function(){var a=[2];var i=0;var c=0;try{a[i] **= (function(){throw 7;})();}catch(e){c=e;}return a[0]*10+c;})()";
    assert_eq!(number.to_bits(), node_bits(reference), "Node {source}");
}

/// A throwing key runs before the RHS, so n stays 0 and the caught 7 remains; both
/// discriminators stay in the result.
/// The reference engine is fed an explicit function expression because node_bits
/// wraps its argument in a Float64Array literal, which cannot hold statements.
#[test]
fn micro_array_compound_key_throw_skips_rhs() {
    let source = "var a=[2]; var n=0; var c=0; try {a[(function(){throw 7;})()] **= (n=9);}catch(e){c=e;} a[0]*100+n*10+c;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert!(number.is_finite(), "{source} finite");
    assert_eq!(number.to_bits(), 207.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    let reference = "(function(){var a=[2];var n=0;var c=0;try{a[(function(){throw 7;})()] **= (n=9);}catch(e){c=e;}return a[0]*100+n*10+c;})()";
    assert_eq!(number.to_bits(), node_bits(reference), "Node {source}");
}

/// An out-of-range index takes an ordinary Undefined Get, then the RHS throws
/// before any Put. This pins the Get-before-RHS order, not length growth and not a
/// NaN cast.
/// The reference engine is fed an explicit function expression because node_bits
/// wraps its argument in a Float64Array literal, which cannot hold statements.
#[test]
fn micro_array_compound_oob_get_before_rhs_throw() {
    let source = "var a=[2]; var i=1; var n=0; var c=0; try {a[i] **= (n=1,(function(){throw 7;})());}catch(e){c=e;} a[0]*100+n*10+c;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert!(number.is_finite(), "{source} finite");
    assert_eq!(number.to_bits(), 217.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    let reference = "(function(){var a=[2];var i=1;var n=0;var c=0;try{a[i] **= (n=1,(function(){throw 7;})());}catch(e){c=e;}return a[0]*100+n*10+c;})()";
    assert_eq!(number.to_bits(), node_bits(reference), "Node {source}");
}

/// `*=` shares the compound store machinery, so this checks a non-Exp consumer of
/// the same path.
/// The reference engine is fed an explicit function expression because node_bits
/// wraps its argument in a Float64Array literal, which cannot hold statements.
#[test]
fn micro_array_compound_shared_multiply() {
    let source = "var a=[3]; var i=0; var r=(a[i] *= 2); r*100+a[0];";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert!(number.is_finite(), "{source} finite");
    assert_eq!(number.to_bits(), 606.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    let reference = "(function(){var a=[3];var i=0;var r=(a[i] *= 2);return r*100+a[0];})()";
    assert_eq!(number.to_bits(), node_bits(reference), "Node {source}");
}

/// A dead branch lowers the compound store but never runs; only the live read
/// contributes. The runtime value here is not by itself proof of helper absence.
/// The reference engine is fed an explicit function expression because node_bits
/// wraps its argument in a Float64Array literal, which cannot hold statements.
#[test]
fn micro_array_compound_dead_branch() {
    let source = "var a=[2]; var i=0; if(false){a[i] **= 3;} a[0];";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert!(number.is_finite(), "{source} finite");
    assert_eq!(number.to_bits(), 2.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    let reference = "(function(){var a=[2];var i=0;if(false){a[i] **= 3;}return a[0];})()";
    assert_eq!(number.to_bits(), node_bits(reference), "Node {source}");
}

/// A statically throwing prefix leaves the store unreachable, so the index keeps 0
/// and only the caught payload survives.
/// The reference engine is fed an explicit function expression because node_bits
/// wraps its argument in a Float64Array literal, which cannot hold statements.
#[test]
fn micro_array_compound_known_throw_prefix() {
    let source = "var a=[2]; var i=0; var c=0; try {throw 7; a[i] **= (i=9);}catch(e){c=e;} a[0]*100+i*10+c;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert!(number.is_finite(), "{source} finite");
    assert_eq!(number.to_bits(), 207.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    let reference = "(function(){var a=[2];var i=0;var c=0;try{throw 7;a[i] **= (i=9);}catch(e){c=e;}return a[0]*100+i*10+c;})()";
    assert_eq!(number.to_bits(), node_bits(reference), "Node {source}");
}

/// A known String variable key cannot take the dense-array element route, so the
/// frontend refuses the compound store at compile time. Compile only: this case
/// never reaches a VM, a runtime, or a guest instruction.
#[test]
fn micro_array_compound_known_string_index_refused() {
    let source = "var a=[2]; var key='0'; a[key] **= 3;";
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let catalog =
                pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
            let options = vm::CompileSourceFileOptions::new()
                .with_host_api_catalog(catalog.clone())
                .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN);
            let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
                source,
                javascript_es5_flavor(),
                options,
            );
            let error = match compiled {
                Ok(_) => panic!("guest 0: {source} must not compile"),
                Err(error) => error,
            };
            let text = error.to_string();
            assert!(
                text.contains("P0 dense array indexes must be numeric in this scope"),
                "refusal for {source}: {text}"
            );
            eprintln!("MATH_ARRAY_KEY_REFUSED {error:?} guest_invocations=0");
        })
        .expect("stack")
        .join()
        .expect("test thread");
}

/// The computed key is itself a graph walk site. The key IIFE throws before the
/// compound store ever reaches its RHS, so the old target value and the pending
/// RHS binding are both untouched: `o.x` stays 2 and `n` stays 0 (the written
/// `n=9` never runs), leaving only the caught 7. Mission witness n=9 encodes
/// exactly that skip.
/// The reference engine is fed an explicit function expression because node_bits
/// wraps its argument in a Float64Array literal, which cannot hold statements.
#[test]
fn micro_scope_compound_object_key_throw() {
    let source = "var o={x:2}; var n=0; var c=0; try{o[(function(){throw 7;})()] **= (n=9);}catch(e){c=e;} o.x*100+n*10+c;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert!(number.is_finite(), "{source} finite");
    assert_eq!(number.to_bits(), 207.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    let reference = "(function(){var o={x:2};var n=0;var c=0;try{o[(function(){throw 7;})()] **= (n=9);}catch(e){c=e;}return o.x*100+n*10+c;})()";
    assert_eq!(number.to_bits(), node_bits(reference), "Node {source}");
}

/// Plain `=` shares the same computed-key walk: the throwing key keeps the store
/// from ever evaluating the RHS, so `o.x` keeps 2 and `n` keeps 0.
/// The reference engine is fed an explicit function expression because node_bits
/// wraps its argument in a Float64Array literal, which cannot hold statements.
#[test]
fn micro_scope_plain_object_key_throw() {
    let source = "var o={x:2}; var n=0; var c=0; try{o[(function(){throw 7;})()] = (n=9);}catch(e){c=e;} o.x*100+n*10+c;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert!(number.is_finite(), "{source} finite");
    assert_eq!(number.to_bits(), 207.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    let reference = "(function(){var o={x:2};var n=0;var c=0;try{o[(function(){throw 7;})()] = (n=9);}catch(e){c=e;}return o.x*100+n*10+c;})()";
    assert_eq!(number.to_bits(), node_bits(reference), "Node {source}");
}

/// Contrast case: an Update target's Member is already walked by the graph source
/// (base and computed key), so this pins control only. A throwing key means
/// neither the read nor the write of `o.x` happens and `o.x` keeps 2. It is not a
/// claim about the old runtime shape.
/// The reference engine is fed an explicit function expression because node_bits
/// wraps its argument in a Float64Array literal, which cannot hold statements.
#[test]
fn micro_scope_update_object_key_throw_control() {
    let source =
        "var o={x:2}; var c=0; try{o[(function(){throw 7;})()]++;}catch(e){c=e;} o.x*100+c;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert!(number.is_finite(), "{source} finite");
    assert_eq!(number.to_bits(), 207.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    let reference = "(function(){var o={x:2};var c=0;try{o[(function(){throw 7;})()]++;}catch(e){c=e;}return o.x*100+c;})()";
    assert_eq!(number.to_bits(), node_bits(reference), "Node {source}");
}

/// The key IIFE carries its own `"use strict"` directive body with one plain
/// formal `v` and throws that formal (7). The formal belongs to the IIFE's own
/// function scope, no outer binding is captured, and the computed-key walk still
/// precedes the RHS, so `a[0]` keeps 2 and `n` keeps 0.
/// The reference engine is fed an explicit function expression because node_bits
/// wraps its argument in a Float64Array literal, which cannot hold statements.
#[test]
fn micro_scope_strict_parameter_array_key_throw() {
    let source = "var a=[2]; var n=0; var c=0; try{a[(function(v){\"use strict\";throw v;})(7)] **= (n=9);}catch(e){c=e;} a[0]*100+n*10+c;";
    let value = execute(source);
    let Value::Map(fields) = &value else {
        panic!("Number box: {value:?}")
    };
    assert_eq!(fields.len(), 5, "Number box fields");
    assert_eq!(field(&value, "tag"), Value::Int(3), "Number tag");
    let Value::Float(number) = field(&value, "number") else {
        panic!("Float payload for {source}")
    };
    assert!(number.is_finite(), "{source} finite");
    assert_eq!(number.to_bits(), 207.0_f64.to_bits(), "{source} value");
    assert_eq!(field(&value, "flag"), Value::Bool(false), "hidden flag");
    assert_eq!(field(&value, "ref_slot"), Value::Int(0), "ref slot");
    assert_eq!(field(&value, "generation"), Value::Int(0), "generation");
    let reference = "(function(){var a=[2];var n=0;var c=0;try{a[(function(v){\"use strict\";throw v;})(7)] **= (n=9);}catch(e){c=e;}return a[0]*100+n*10+c;})()";
    assert_eq!(number.to_bits(), node_bits(reference), "Node {source}");
}
