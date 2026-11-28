//! Fixed MicroQuickJS 6d4d7eb6 witnesses for Math.log2 (code 545).
//!
//! `ES5_LOG2_MQJS` optionally names the pinned executable for a differential
//! run of the same guest source. Neither engine generates the expected bits.
use std::{ffi::OsStr, path::PathBuf, process::Command};

use pd_vm_compat_frontends::{JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor, runtime};
use vm::{HostFunctionRegistry, Value, VmStatus};

const METHOD: &str = "log2";
const FUEL: u64 = 16_000_000;
const MIN_FREE: u64 = 8 * 1024 * 1024 * 1024;
const THREE: Expected = Expected::Bits(0x4008_0000_0000_0000);

#[derive(Clone, Copy)]
enum Expected {
    Bits(u64),
    Nan,
}

impl Expected {
    fn check(self, engine: &str, name: &str, value: f64) {
        let bits = value.to_bits();
        match self {
            Self::Bits(expected) => assert_eq!(
                bits, expected,
                "{engine}: {name} (got {bits:#018x}, expected {expected:#018x})"
            ),
            Self::Nan => assert!(
                value.is_nan(),
                "{engine}: {name} (expected NaN, got {bits:#018x})"
            ),
        }
        eprintln!("{engine}: {name}: {bits:#018x}");
    }
}

fn check_resources() {
    let meminfo = std::fs::read_to_string("/proc/meminfo").expect("meminfo");
    let available_kib = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemAvailable:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|field| field.parse::<u64>().ok())
        .expect("MemAvailable in KiB");
    assert!(
        available_kib >= MIN_FREE / 1024,
        "available memory: {available_kib} KiB"
    );

    let paths: Vec<_> = ["CARGO_TARGET_DIR", "TMPDIR"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .collect();
    for path in paths {
        let output = Command::new("df")
            .args(["-Pk", "--"])
            .env("LC_ALL", "C")
            .arg(&path)
            .output()
            .expect("df");
        assert!(
            output.status.success(),
            "df {}: {:?}",
            path.display(),
            output
        );
        let text = String::from_utf8(output.stdout).expect("df UTF-8");
        let available_kib = text
            .lines()
            .nth(1)
            .and_then(|line| line.split_whitespace().nth(3))
            .and_then(|field| field.parse::<u64>().ok())
            .expect("df available KiB");
        assert!(
            available_kib >= MIN_FREE / 1024,
            "{}: {available_kib} KiB free",
            path.display()
        );
    }
}

fn execute(name: &str, source: &str, check: impl FnOnce(&[Value])) {
    let catalog = runtime::cli::reference_array_cli_effective_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN)
        .with_host_api_catalog(catalog.clone());
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        javascript_es5_flavor(),
        options,
    )
    .unwrap_or_else(|err| panic!("{name}: compilation failed: {err:?}\n{source}"));
    let mut machine = compiled.into_vm().expect("certified VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        runtime::array::OwnedArrayLimits::default(),
    )
    .expect("owned arrays");
    let mut registry = HostFunctionRegistry::restricted();
    for module in [
        runtime::array::typescript_array_host_module(),
        runtime::array::typescript_owned_array_host_module(),
        runtime::array::typescript_array_snapshot_host_module(),
        runtime::scalar_math::typescript_scalar_math_host_module(),
        runtime::scalar_error::typescript_error_host_module(),
    ] {
        module
            .install_from_catalog(&mut registry, &catalog)
            .expect("exact catalog");
    }
    // The selected Math realm retains a shared dispatcher, including the
    // other numeric builtins. Only METHOD is selected by the guest fixtures.
    for builtin in [
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
            .allow_builtin(format!("math::{builtin}"))
            .expect("retained Math builtin");
    }
    registry
        .allow_builtin("bytes::from_utf8")
        .expect("numeric formatter builtin");
    registry.bind_vm_cached(&mut machine).expect("bind catalog");
    machine.set_fuel(FUEL);
    assert_eq!(machine.get_fuel(), Some(FUEL));
    let status = machine
        .run()
        .unwrap_or_else(|err| panic!("{name}: VM failed: {err:?}\n{source}"));
    let fuel_after = machine.get_fuel().expect("bounded fuel");
    assert_eq!(status, VmStatus::Halted, "{name}");
    assert!(!machine.jit_config().enabled, "{name}");
    assert_eq!(
        (machine.aot_exec_count(), machine.jit_native_exec_count()),
        (0, 0),
        "{name}"
    );
    assert!(
        fuel_after < FUEL,
        "{name}: fuel after execution: {fuel_after}"
    );
    assert!(
        machine
            .capture_certification_stats()
            .is_some_and(|stats| stats.valid),
        "{name}"
    );
    let resources = machine.host_context().resource_count();
    assert!(resources <= 1, "{name}: {resources} resources");

    eprintln!(
        "VM: {name}: {status:?}, jit=false, aot=0, native=0, fuel={FUEL}->{fuel_after}, resources={resources}"
    );
    let stack = machine.stack().to_vec();
    machine.reset_for_reuse().expect("reset VM");
    assert_eq!(
        machine.host_context().resource_count(),
        0,
        "{name}: resources after reset"
    );
    eprintln!("VM: {name}: resources after reset=0");
    check(&stack);
}

fn float(name: &str, value: &Value) -> f64 {
    let Value::Map(boxed) = value else {
        panic!("{name}: expected boxed result, got {value:?}");
    };
    assert_eq!(
        boxed.get(&Value::string("tag")),
        Some(&Value::Int(3)),
        "{name}: numeric tag"
    );
    let Some(Value::Float(number)) = boxed.get(&Value::string("number")) else {
        panic!(
            "{name}: expected Value::Float, got {:?}",
            boxed.get(&Value::string("number"))
        );
    };
    *number
}

fn micro_values(mqjs: &OsStr, name: &str, source: &str, points: usize) -> Vec<f64> {
    // Only the Micro-side observer uses typed arrays. Indirect eval receives
    // exactly the same guest bytes passed to the certified frontend.
    let guest = serde_json::to_string(source).expect("encode guest source");
    let probe = format!(
        "var completion = (1, eval)({guest});\n\
         var f = new Float64Array(1); var u = new Uint32Array(f.buffer);\n\
         function observe(result) {{\n\
             if (typeof result !== 'number') throw 'expected number';\n\
             f[0] = result; print(u[1].toString(16), u[0].toString(16));\n\
         }}\n\
         for (var i = 0; i < {points}; i++) observe(globalThis['point' + i]);\n\
         observe(completion);"
    );
    let output = Command::new(mqjs)
        .arg("-e")
        .arg(probe)
        .output()
        .expect("execute pinned mqjs");
    assert!(
        output.status.success(),
        "Micro {name}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let words = String::from_utf8(output.stdout).expect("Micro UTF-8");
    let mut words = words.split_whitespace();
    let mut next_word =
        || u32::from_str_radix(words.next().expect("Micro word"), 16).expect("Micro hex word");
    let values = (0..=points)
        .map(|_| f64::from_bits((u64::from(next_word()) << 32) | u64::from(next_word())))
        .collect();
    assert_eq!(words.next(), None, "Micro {name}: extra output");
    values
}

// All constants and operations below are exact binary64 values, including
// the intermediate 2^-1024. Do not obtain inputs via another Math method.
const POWER_INPUTS: &str = r#"
var s16 = 1 / 65536;
var s32 = s16 * s16;
var s64 = s32 * s32;
var s128 = s64 * s64;
var s256 = s128 * s128;
var s512 = s256 * s256;
var s1024 = s512 * s512;
var minNormal = s1024 * 4;
var p32 = 65536 * 65536;
var p64 = p32 * p32;
var p128 = p64 * p64;
var p256 = p128 * p128;
var p512 = p256 * p256;
var big = p512 * (p512 / 2);
"#;

// Excluded (input bits -> fixed Micro bits): the Core host path differs by
// one ULP at each point. The finite domain is not qualified beyond the table.
// 0x3fe1d82e81949669 -> 0xbfeaf68b3d3622eb
// 0x40079399fc7c9ea5 -> 0x3ff8f2d1844a0338
// 0x3ff136eb3619290a -> 0x3fbb05a046b3b5a7
// 0x119ad12a7496cbd1 -> 0xc0872a0a1418b087

const CALL_ORDER: &str = r#"
'use strict';
var extracted = Math.__METHOD__;
if (extracted.length !== 1) throw 1;
var state = { order: 0, tailHooks: 0, firstHooks: 0 };
var borrower = {
    method: extracted,
    valueOf: function () { throw 2; }
};
var first = { state: state, valueOf: function () {
    if (this.state.order !== 12) throw 3;
    this.state.firstHooks = this.state.firstHooks + 1;
    return 8;
} };
var fallback = {
    state: state,
    valueOf: function () {
        if (this.state.order !== 12) throw 4;
        this.state.order = 123;
        return this;
    },
    toString: function () {
        if (this.state.order !== 123) throw 5;
        this.state.firstHooks = this.state.firstHooks + 1;
        return '8';
    }
};
var tail = {
    state: state,
    get valueOf() { this.state.tailHooks = this.state.tailHooks + 1; throw 6; },
    toString: function () { this.state.tailHooks = this.state.tailHooks + 1; throw 7; }
};
if (Math.__METHOD__((state.order = 1, first), (state.order = state.order * 10 + 2, tail)) !== 3) throw 8;
if (extracted((state.order = 1, fallback), (state.order = state.order * 10 + 2, tail)) !== 3) throw 9;
if (borrower.method((state.order = 1, first), (state.order = state.order * 10 + 2, tail)) !== 3) throw 10;
if (state.firstHooks !== 3) throw 11;
if (state.tailHooks !== 0) throw 12;
Math.__METHOD__(8);
"#;

const ABRUPT_ORDER: &str = r#"
'use strict';
var state = { order: 0, tailHooks: 0 };
var tail = {
    state: state,
    get valueOf() { this.state.tailHooks = this.state.tailHooks + 1; throw 1; },
    toString: function () { this.state.tailHooks = this.state.tailHooks + 1; throw 2; }
};
var token = {};
var firstExpression = {
    state: state,
    token: token,
    run: function () { this.state.order = 1; throw this.token; }
};
var caught = false;
try {
    Math.__METHOD__(firstExpression.run(), (state.order = 12, tail));
} catch (expressionError) {
    if (expressionError !== token) throw 3;
    caught = true;
}
if (!caught) throw 4;
if (state.order !== 1) throw 5;

var getterToken = {};
var getter = {
    state: state,
    token: getterToken,
    get valueOf() { if (this.state.order !== 12) throw 6; this.state.order = 123; throw this.token; },
    toString: function () { throw 7; }
};
caught = false;
try {
    Math.__METHOD__((state.order = 1, getter), (state.order = state.order * 10 + 2, tail));
} catch (getterError) {
    if (getterError !== getterToken) throw 8;
    caught = true;
}
if (!caught) throw 9;
if (state.order !== 123) throw 10;

var methodToken = {};
var method = {
    state: state,
    token: methodToken,
    valueOf: function () { if (this.state.order !== 12) throw 11; this.state.order = 123; throw this.token; },
    toString: function () { throw 12; }
};
caught = false;
try {
    Math.__METHOD__((state.order = 1, method), (state.order = state.order * 10 + 2, tail));
} catch (methodError) {
    if (methodError !== methodToken) throw 13;
    caught = true;
}
if (!caught) throw 14;
if (state.order !== 123) throw 15;
if (state.tailHooks !== 0) throw 16;
Math.__METHOD__(8);
"#;

const NON_PRIMITIVE: &str = r#"
'use strict';
var value = {
    order: 0,
    valueOf: function () { this.order = 1; return this; },
    toString: function () { if (this.order !== 1) throw 1; this.order = 12; return this; }
};
var caught = false;
try {
    Math.__METHOD__(value);
} catch (e) {
    if (e.name !== 'TypeError') throw 2;
    caught = true;
}
if (!caught) throw 3;
if (value.order !== 12) throw 4;
Math.__METHOD__(8);
"#;

#[test]
fn p8_math_log2_interpreter() {
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            check_resources();
            let mqjs = std::env::var_os("ES5_LOG2_MQJS");
            use Expected::{Bits, Nan};
            let points = [
                ("3", "3", Bits(0x3ff9_5c01_a39f_bd68)),
                ("missing", "", Nan),
                ("NaN", "0 / 0", Nan),
                ("negative finite", "-2", Nan),
                ("-Infinity", "-1 / 0", Nan),
                ("+0", "0", Bits(0xfff0_0000_0000_0000)),
                ("-0", "1 / (-1 / 0)", Bits(0xfff0_0000_0000_0000)),
                ("1", "1", Bits(0)),
                ("+Infinity", "1 / 0", Bits(0x7ff0_0000_0000_0000)),
                (
                    "2^-1074",
                    "minNormal / 4503599627370496",
                    Bits(0xc090_c800_0000_0000),
                ),
                ("2^-1022", "minNormal", Bits(0xc08f_f000_0000_0000)),
                ("0.5", "1 / 2", Bits(0xbff0_0000_0000_0000)),
                ("2", "2", Bits(0x3ff0_0000_0000_0000)),
                (
                    "2^53",
                    "65536 * 65536 * 2097152",
                    Bits(0x404a_8000_0000_0000),
                ),
                ("2^1023", "big", Bits(0x408f_f800_0000_0000)),
                ("0.75", "3 / 4", Bits(0xbfda_8ff9_7181_0a5e)),
                (
                    "1+2^-52",
                    "1 + 1 / 4503599627370496",
                    Bits(0x3cb7_1547_652b_82fd),
                ),
                (
                    "1-2^-53",
                    "1 - 1 / 9007199254740992",
                    Bits(0xbca7_1547_652b_82fe),
                ),
            ];
            // Straight-line expression statements expose their original boxes
            // on the Core stack. The Micro observer reads the corresponding
            // globals, so one certification per script covers every point.
            use std::fmt::Write as _;
            let mut numeric = POWER_INPUTS.to_owned();
            for (i, (_, arg, _)) in points.iter().enumerate() {
                writeln!(numeric, "var point{i} = Math.{METHOD}({arg});\npoint{i};").unwrap();
            }
            writeln!(numeric, "Math.{METHOD}(8);").unwrap();
            execute("fixed points", &numeric, |stack| {
                let (completion, values) = stack.split_last().expect("numeric completion");
                THREE.check("Compat", "8", float("8", completion));
                assert_eq!(values.len(), points.len(), "boxed numeric observations");
                for ((name, _, expected), value) in points.iter().zip(values) {
                    expected.check("Compat", name, float(name, value));
                }
            });
            if let Some(mqjs) = mqjs.as_deref() {
                let results = micro_values(mqjs, "fixed points", &numeric, points.len());
                for ((name, _, expected), value) in points.iter().zip(&results) {
                    expected.check("Micro", name, *value);
                }
                THREE.check("Micro", "8", results[points.len()]);
            }
            for (name, fixture) in [
                ("direct, extracted, borrowed", CALL_ORDER),
                ("abrupt evaluation and conversion", ABRUPT_ORDER),
                ("both hooks return objects", NON_PRIMITIVE),
            ] {
                let source = fixture.replace("__METHOD__", METHOD);
                execute(name, &source, |stack| {
                    THREE.check(
                        "Compat",
                        name,
                        float(name, stack.last().expect("completion")),
                    );
                });
                if let Some(mqjs) = mqjs.as_deref() {
                    THREE.check("Micro", name, micro_values(mqjs, name, &source, 0)[0]);
                }
            }
        })
        .expect("spawn bounded-stack interpreter test")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
}
