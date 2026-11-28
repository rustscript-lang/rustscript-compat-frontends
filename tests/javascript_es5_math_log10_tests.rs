//! Fixed MicroQuickJS `6d4d7eb6b99011d5aab6e88a9b99e8fd4d86c6e9`
//! witnesses for `Math.log10` (intrinsic code 546), not full-domain parity.
//! `11` is excluded: Micro yields 0x3ff0a98b6050c56f, while the pinned Core
//! host path yields 0x3ff0a98b6050c56e. No parity claim for other untested
//! inputs, general string ToNumber, call/apply, descriptors or own length,
//! TypeScript, or native execution. Set `ES5_LOG10_MQJS` to the pinned binary
//! to replay exactly the same guest sources alongside the fixed expectations.

use std::fmt::Write as _;

use pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source;
use vm::{Value, VmStatus};

const METHOD: &str = "log10";
const FUEL: u64 = 10_000_000;
const ONE: Expected = Bits(0x3ff0_0000_0000_0000);
const THREE: Expected = Bits(0x4008_0000_0000_0000);
const NEG_INFINITY: Expected = Bits(0xfff0_0000_0000_0000);

#[derive(Clone, Copy)]
enum Expected {
    Bits(u64),
    Nan,
}

use Expected::{Bits, Nan};

fn assert_result(engine: &str, point: &str, actual: f64, expected: Expected) {
    match expected {
        Bits(bits) => assert_eq!(
            actual.to_bits(),
            bits,
            "{engine} result bits: {point}; got {:#018x}, expected {bits:#018x}",
            actual.to_bits(),
        ),
        Nan => assert!(
            actual.is_nan(),
            "{engine} expected NaN at {point}; got {actual}"
        ),
    }
}

fn compat_values(source: &str) -> Vec<f64> {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(move || {
            eprintln!("LOG10_SOURCE {}", serde_json::to_string(&source).unwrap());
            let mut machine = compile_javascript_es5_source(&source)
                .unwrap_or_else(|error| panic!("compile {source:?}: {error:?}"));
            assert!(machine.capture_certification_stats().is_some_and(|stats| stats.valid));
            assert!(!machine.jit_config().enabled);
            machine.set_fuel(FUEL);
            let before = machine.get_fuel();
            let status = machine.run();
            let after = machine.get_fuel();
            let resources = machine.host_context().resource_count();
            let aot = machine.aot_exec_count();
            let native = machine.jit_native_exec_count();
            assert_eq!(status.expect("interpreter run"), VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!((aot, native), (0, 0));
            assert_eq!(before, Some(FUEL));
            assert!(matches!(after, Some(left) if left < FUEL));
            assert!(resources <= 1, "realm resources: {resources}");

            // Some call routes put a raw success marker before the guest
            // completions. Strip that one prefix only, never a JS Number box.
            let stack = machine.stack();
            let points = stack.strip_prefix(&[Value::Bool(true)]).unwrap_or(stack);
            let values = points
                .iter()
                .enumerate()
                .map(|(i, value)| {
                    let Value::Map(boxed) = value else {
                        panic!("point {i}: expected boxed Number, got {value:?}");
                    };
                    assert_eq!(boxed.get(&Value::string("tag")), Some(&Value::Int(3)), "point {i}");
                    let Some(Value::Float(number)) = boxed.get(&Value::string("number")) else {
                        panic!("point {i}: expected Float payload, got {boxed:?}");
                    };
                    eprintln!("LOG10_COMPAT point{i} tag=3 float={:#018x}", number.to_bits());
                    *number
                })
                .collect();
            machine.reset_for_reuse().expect("reset completed VM");
            let reset_resources = machine.host_context().resource_count();
            assert_eq!(reset_resources, 0);
            eprintln!(
                "LOG10_VM status=Halted jit=false aot={aot} native={native} fuel={before:?}->{after:?} resources={resources}->{reset_resources}",
            );
            values
        })
        .expect("test thread")
        .join()
        .unwrap_or_else(|error| std::panic::resume_unwind(error))
}

fn micro_values(engine: &std::ffi::OsStr, source: &str, points: usize) -> Vec<f64> {
    // Indirect eval receives the unchanged Script. Only this observer uses
    // typed arrays, and it determines their word order with an exact constant.
    let script = format!(
        r#"var completion = (1, eval)({});
        var f = new Float64Array(1);
        var words = new Uint32Array(f.buffer);
        f[0] = 1;
        var hi = words[0] === 1072693248 ? 0 : 1;
        function observe(result) {{
            if (typeof result !== 'number') throw new TypeError('not a Number');
            if (result !== result) {{ print('NaN'); }} else {{
                f[0] = result;
                print(words[hi].toString(16) + ':' + words[1 - hi].toString(16));
            }}
        }}
        for (var i = 0; i < {}; i++) observe(globalThis['point' + i]);
        observe(completion);"#,
        serde_json::to_string(source).unwrap(),
        points - 1,
    );
    let output = std::process::Command::new("timeout")
        .arg("10")
        .arg(engine)
        .args(["-e", &script])
        .output()
        .expect("run pinned mqjs");
    let text = std::str::from_utf8(&output.stdout)
        .expect("mqjs UTF-8")
        .trim();
    eprintln!("LOG10_MICRO rc={:?} points={text:?}", output.status.code());
    assert!(
        output.status.success() && output.stderr.is_empty(),
        "mqjs {source:?}: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    text.lines()
        .map(|line| {
            if line == "NaN" {
                return f64::NAN;
            }
            let (hi, lo) = line.split_once(':').expect("binary64 words");
            let hi = u32::from_str_radix(hi, 16).expect("high word");
            let lo = u32::from_str_radix(lo, 16).expect("low word");
            f64::from_bits((u64::from(hi) << 32) | u64::from(lo))
        })
        .collect()
}

fn check_script(source: String, expected: &[(&str, Expected)]) {
    assert!(!expected.is_empty());
    let source = source.replace("__METHOD__", METHOD);
    let mut members = source.match_indices("Math.").peekable();
    assert!(members.peek().is_some());
    for (index, _) in members {
        let member = source[index + "Math.".len()..]
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
            .next();
        assert_eq!(member, Some(METHOD), "unexpected Math method");
    }

    let compare = |engine, values: Vec<_>| {
        assert_eq!(values.len(), expected.len(), "{engine}: observation count");
        for ((point, expected), value) in expected.iter().zip(values) {
            assert_result(engine, point, value, *expected);
        }
    };
    compare("Compat", compat_values(&source));
    if let Some(engine) = std::env::var_os("ES5_LOG10_MQJS") {
        compare("Micro", micro_values(&engine, &source, expected.len()));
    }
}

// Straight-line numeric scripts preserve every original guest Number box.
fn check_numeric(cases: &[(&str, &str, &str, Expected)]) {
    let mut source = String::from("'use strict';\n");
    for (i, (_, setup, expr, _)) in cases.iter().enumerate() {
        writeln!(source, "{setup}\nvar point{i} = {expr};\npoint{i};").unwrap();
    }
    let expected: Vec<_> = cases
        .iter()
        .map(|&(name, _, _, value)| (name, value))
        .collect();
    check_script(source, &expected);
}

// Exception-capable scripts publish only the final guest completion. Check
// earlier behavior predicates in the guest with distinct failure messages.
fn check(cases: &[(&str, &str, &str, Expected)]) {
    let mut source = String::from("'use strict';\n");
    let last = cases.last().expect("behavior cases");
    let final_index = cases.len() - 1;
    for (index, &(name, setup, expr, expected)) in cases.iter().enumerate() {
        writeln!(source, "{setup}\nvar point{index} = {expr};").unwrap();
        if index != final_index {
            let literal = match expected {
                Bits(0x3ff0_0000_0000_0000) => "1",
                Bits(0x4008_0000_0000_0000) => "3",
                _ => unreachable!("behavior cases have exact integer outcomes"),
            };
            let message = serde_json::to_string(name).unwrap();
            writeln!(source, "if (point{index} !== {literal}) throw {message};").unwrap();
        }
    }
    writeln!(source, "point{final_index};").unwrap();
    check_script(source, &[(last.0, last.3)]);
}

fn check_points(points: &[(&str, Expected)]) {
    let sources: Vec<_> = points
        .iter()
        .map(|(arg, _)| format!("Math.__METHOD__({arg})"))
        .collect();
    let cases: Vec<_> = points
        .iter()
        .zip(&sources)
        .map(|((arg, expected), source)| (*arg, "", source.as_str(), *expected))
        .collect();
    check_numeric(&cases);
}

#[test]
fn log10_special_domain() {
    check_points(&[
        ("", Nan),
        ("0 / 0", Nan),
        ("undefined", Nan),
        ("0", NEG_INFINITY),
        ("1 / (-1 / 0)", NEG_INFINITY),
        ("-1", Nan),
        ("-1 / 0", Nan),
        ("1", Bits(0)),
        ("1 / 0", Bits(0x7ff0_0000_0000_0000)),
    ]);
}

#[test]
fn log10_decimal_power_points() {
    check_numeric(&[
        ("10", "", "Math.__METHOD__(10)", ONE),
        // Only the binary64 token 0x3fb999999999999a, not a real-arithmetic identity.
        (
            "0.1 input",
            "var tenth = 0.1;",
            "tenth",
            Bits(0x3fb9_9999_9999_999a),
        ),
        (
            "0.1",
            "",
            "Math.__METHOD__(tenth)",
            Bits(0xbff0_0000_0000_0000),
        ),
        ("1000", "", "Math.__METHOD__(1000)", THREE),
    ]);
}

#[test]
fn log10_matching_finite_points() {
    check_points(&[
        ("5e-324", Bits(0xc074_34e6_420f_4374)),
        ("0.5", Bits(0xbfd3_4413_509f_79ff)),
        ("2", Bits(0x3fd3_4413_509f_79ff)),
    ]);
}

// The receiver is poison only when used; borrowing must not coerce `this`.
const BORROWED: &str =
    "var receiver = { log: Math.__METHOD__, valueOf: function() { throw 99; } };";

#[test]
fn log10_call_shapes() {
    check(&[
        ("direct", "", "Math.__METHOD__(1000)", THREE),
        (
            "extracted with length 1",
            "var f = Math.__METHOD__; var r = f(1000); if (f.length !== 1) r = -99;",
            "r",
            THREE,
        ),
        ("borrowed", BORROWED, "receiver.log(1000)", THREE),
    ]);
}

#[test]
fn log10_object_conversion() {
    check(&[
        (
            "valueOf getter then method",
            r#"var o = { n: 0,
                get valueOf() { this.n = this.n * 10 + 1; return this.convert; },
                convert: function() { this.n = this.n * 10 + 2; return 1000; },
                toString: function() { this.n = 99; return '1000'; }
            };
            var r = Math.__METHOD__(o);
            if (o.n !== 12) r = -99;"#,
            "r",
            THREE,
        ),
        (
            "valueOf object then toString",
            r#"var o = { n: 0,
                valueOf: function() { this.n = this.n * 10 + 1; return this; },
                toString: function() { this.n = this.n * 10 + 2; return '1000'; }
            };
            var r = Math.__METHOD__(o);
            if (o.n !== 12) r = -99;"#,
            "r",
            THREE,
        ),
    ]);
}

#[test]
fn log10_object_errors() {
    let throwing = |hook, error| {
        format!(
            r#"var o = {{ n: 0, {hook}, toString: function() {{ this.n = 99; return '1000'; }} }};
            var caught = 0;
            try {{ Math.__METHOD__(o); }} catch ({error}) {{ if ({error} === 73) caught = 1; }}
            if (o.n !== 1) caught = -99;"#,
        )
    };
    let getter = throwing(
        "get valueOf() { this.n = this.n * 10 + 1; throw 73; }",
        "getterError",
    );
    let method = throwing(
        "valueOf: function() { this.n = this.n * 10 + 1; throw 73; }",
        "methodError",
    );
    check(&[
        ("getter preserves thrown payload", &getter, "caught", ONE),
        ("method preserves thrown payload", &method, "caught", ONE),
        (
            "both hooks return objects before TypeError",
            r#"var o = { n: 0,
                valueOf: function() { this.n = this.n * 10 + 1; return this; },
                toString: function() { this.n = this.n * 10 + 2; return this; }
            };
            var caught = 0;
            try { Math.__METHOD__(o); } catch (typeError) {
                if (typeError.name === 'TypeError') caught = 1;
            }
            if (o.n !== 12) caught = -99;"#,
            "caught",
            ONE,
        ),
    ]);
}

#[test]
fn log10_tail_not_converted() {
    let setup = r#"var trace = { order: 0, first: 0 };
        var head = { trace: trace, valueOf: function() {
            this.trace.first = this.trace.first + 1;
            if (this.trace.order !== 12) return -1;
            return 1000;
        } };
        var tail = { v: 0, s: 0,
            valueOf: function() { this.v = this.v + 1; return this; },
            toString: function() { this.s = this.s + 1; throw 99; }
        };"#;
    let with_call = |prefix, callee| {
        format!(
            r#"{prefix}
            var r = (trace.order = 0, trace.first = 0,
                     {callee}((trace.order = trace.order * 10 + 1, head),
                              (trace.order = trace.order * 10 + 2, tail)));
            if (trace.order !== 12) r = -91;
            if (trace.first !== 1) r = -92;
            if (tail.v !== 0) r = -93;
            if (tail.s !== 0) r = -94;"#,
        )
    };
    let direct = with_call(setup, "Math.__METHOD__");
    let extracted = with_call("var f = Math.__METHOD__;", "f");
    let borrowed = with_call(BORROWED, "receiver.log");
    check(&[
        ("direct tail", &direct, "r", THREE),
        ("extracted tail", &extracted, "r", THREE),
        ("borrowed tail", &borrowed, "r", THREE),
    ]);
}

#[test]
fn log10_abrupt_first() {
    check(&[
        (
            "first expression throws before tail expression",
            r#"var tailExpr = 0;
            var caught = 0;
            function first() { throw 81; }
            try { Math.__METHOD__(first(), (tailExpr = 1, 1000)); } catch (exprError) {
                if (exprError === 81) caught = 1;
            }
            if (tailExpr !== 0) caught = -99;"#,
            "caught",
            ONE,
        ),
        (
            "first conversion throws after tail expression",
            r#"var trace = { order: 0, first: 0 };
            var head = { trace: trace, valueOf: function() {
                this.trace.first = this.trace.first + 1;
                if (this.trace.order === 12) throw 82;
                throw 99;
            } };
            var tail = { v: 0, s: 0,
                valueOf: function() { this.v = this.v + 1; return this; },
                toString: function() { this.s = this.s + 1; throw 99; }
            };
            var caught = 0;
            try {
                Math.__METHOD__((trace.order = trace.order * 10 + 1, head),
                                (trace.order = trace.order * 10 + 2, tail));
            } catch (conversionError) { if (conversionError === 82) caught = 1; }
            if (trace.order !== 12) caught = -91;
            if (trace.first !== 1) caught = -92;
            if (tail.v !== 0) caught = -93;
            if (tail.s !== 0) caught = -94;"#,
            "caught",
            ONE,
        ),
    ]);
}
