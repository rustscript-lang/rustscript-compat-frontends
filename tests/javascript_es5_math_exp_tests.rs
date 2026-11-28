//! Fixed common points with MicroQuickJS 6d4d7eb6b99011d5aab6e88a9b99e8fd4d86c6e9.
//! This is a pointwise witness set, not a claim about libm accuracy or the rest
//! of the finite domain. NaN is compared by category only. In particular,
//! exp(1) differs by 1 ULP (Micro: 0x4005bf0a8b14576a) and is not a shared point.

use pd_vm_compat_frontends::{JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor, runtime};
use vm::{HostFunctionRegistry, Value, VmStatus};

const METHOD: &str = "exp";
const FUEL: u64 = 20_000_000;
const HALF_EXP: u64 = 0x3ffa_6129_8e1e_069c;

// The straight-line Script retains each point as a box; the last expression
// provides the completion. The boundary inputs are themselves observed before
// calling exp, so a decimal-token rounding error cannot fake boundary parity.
const SPECIAL: &str = r#"
var point0 = Math.exp(); point0;
var point1 = Math.exp(0 / 0); point1;
var point2 = Math.exp(0); point2;
var point3 = Math.exp(-0); point3;
var point4 = Math.exp(1 / 0); point4;
Math.exp(-1 / 0);
"#;

const FINITE_AND_BOUNDARY: &str = r#"
var point0 = Math.exp(-1); point0;
var point1 = Math.exp(0.5); point1;
var point2 = Math.exp(-0.5); point2;
var point3 = 709.782712893384; point3;
var point4 = 709.7827128933841; point4;
var point5 = -745.1332191019411; point5;
var point6 = -745.1332191019412; point6;
var point7 = Math.exp(point3); point7;
var point8 = Math.exp(point4); point8;
var point9 = Math.exp(point5); point9;
Math.exp(point6);
"#;

const CALL_SHAPES: &str = r#"
'use strict';
var exp = Math.exp;
var holder = {
    exp: exp, hits: 0,
    valueOf: function () { this.hits = this.hits + 1; throw 11; },
    toString: function () { this.hits = this.hits + 1; throw 12; }
};
if (exp.length !== 1) throw 'arity';
var direct = Math.exp(0.5);
var extracted = exp(0.5);
var borrowed = holder.exp(0.5);
if (direct !== 1.6487212707001282) throw 'direct';
if (extracted !== direct) throw 'extracted';
if (borrowed !== direct) throw 'borrowed';
if (holder.hits !== 0) throw 'receiver conversion';
borrowed;
"#;

// All three callers share the same tail object. The first conversion must
// observe the completed expression trace, not merely the same final counters.
const ARGUMENT_ORDER: &str = r#"
'use strict';
var exp = Math.exp;
var holder = { exp: exp };
var trace = { expr: 0, first: 0, tail: 0 };
var first = {
    trace: trace,
    valueOf: function () {
        if (this.trace.expr !== 12) throw 21;
        this.trace.first = this.trace.first + 1;
        return 0.5;
    },
    toString: function () { throw 22; }
};
var tail = {
    trace: trace,
    valueOf: function () { this.trace.tail = this.trace.tail + 1; throw 23; },
    toString: function () { this.trace.tail = this.trace.tail + 1; throw 24; }
};
function mark(state, digit, value) {
    state.expr = state.expr * 10 + digit;
    return value;
}
var direct = Math.exp(mark(trace, 1, first), mark(trace, 2, tail));
if (trace.expr !== 12 || trace.first !== 1) throw 'direct order';
trace.expr = 0;
var extracted = exp(mark(trace, 1, first), mark(trace, 2, tail));
if (trace.expr !== 12 || trace.first !== 2) throw 'extracted order';
trace.expr = 0;
var borrowed = holder.exp(mark(trace, 1, first), mark(trace, 2, tail));
if (trace.expr !== 12 || trace.first !== 3) throw 'borrowed order';
if (trace.tail !== 0) throw 'extra argument converted';
if (direct !== 1.6487212707001282) throw 'valueOf result';
if (extracted !== direct || borrowed !== direct) throw 'forwarded valueOf result';
borrowed;
"#;

const CONVERSION: &str = r#"
'use strict';
var fallback = {
    phase: 0,
    valueOf: function () { this.phase = 1; return {}; },
    toString: function () {
        if (this.phase !== 1) throw 25;
        this.phase = 2;
        return '0.5';
    }
};
var converted = Math.exp(fallback);
if (fallback.phase !== 2 || converted !== 1.6487212707001282) throw 'toString fallback';
var bad = {
    valueOf: function () { return {}; },
    toString: function () { return {}; }
};
var caught = 0;
try { Math.exp(bad); } catch (badError) { caught = badError.name; }
if (caught !== 'TypeError') throw 'two object results';
converted;
"#;

const ABRUPT_ORDER: &str = r#"
'use strict';
var exp = Math.exp;
var holder = { exp: exp };
var trace = { expr: 0, first: 0, tail: 0 };
var tail = {
    trace: trace,
    valueOf: function () { this.trace.tail = this.trace.tail + 1; throw 23; },
    toString: function () { this.trace.tail = this.trace.tail + 1; throw 24; }
};
function tailExpr(state, value) { state.expr = state.expr * 10 + 2; return value; }
function throwFirstExpr(state) { state.expr = state.expr * 10 + 1; throw 31; }
var caught = 0;
try { Math.exp(throwFirstExpr(trace), tailExpr(trace, tail)); } catch (exprError) { caught = exprError; }
if (caught !== 31 || trace.expr !== 1 || trace.first !== 0 || trace.tail !== 0) throw 'expression throw';
var getter = {
    trace: trace,
    get valueOf() {
        if (this.trace.expr !== 2) throw 32;
        this.trace.first = this.trace.first + 1;
        throw 33;
    },
    toString: function () { throw 34; }
};
trace.expr = 0;
caught = 0;
try { exp(getter, tailExpr(trace, tail)); } catch (getterError) { caught = getterError; }
if (caught !== 33 || trace.expr !== 2 || trace.first !== 1 || trace.tail !== 0) throw 'getter throw';
var method = {
    trace: trace,
    valueOf: function () {
        if (this.trace.expr !== 2) throw 35;
        this.trace.first = this.trace.first + 1;
        throw 36;
    },
    toString: function () { throw 37; }
};
trace.expr = 0;
caught = 0;
try { holder.exp(method, tailExpr(trace, tail)); } catch (methodError) { caught = methodError; }
if (caught !== 36 || trace.expr !== 2 || trace.first !== 2 || trace.tail !== 0) throw 'method throw';
Math.exp(0.5);
"#;

fn execute(label: &'static str, source: &'static str) -> Vec<Value> {
    // A fixture selects just one Math member, even though initialization
    // installs the whole Math namespace and the shared numeric helpers.
    let mut members = source.match_indices("Math.").map(|(index, prefix)| {
        source[index + prefix.len()..]
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
            .next()
            .expect("member after Math.")
    });
    assert_eq!(members.next(), Some(METHOD));
    assert!(members.all(|member| member == METHOD));

    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(move || {
            let catalog = runtime::cli::reference_array_cli_effective_catalog();
            let options = vm::CompileSourceFileOptions::new()
                .with_host_api_catalog(catalog.clone())
                .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN);
            let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
                source,
                javascript_es5_flavor(),
                options,
            )
            .expect("certified ES5 compilation");
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
            for name in [
                "abs", "acos", "asin", "atan", "atan2", "ceil", "copysign", "cos", "exp", "exp2",
                "floor", "is_finite", "is_infinite", "is_nan", "ln", "log10", "log2", "powf",
                "signum", "sin", "sqrt", "tan", "trunc",
            ] {
                registry
                    .allow_builtin(format!("math::{name}"))
                    .unwrap_or_else(|error| panic!("math::{name}: {error:?}"));
            }
            registry.allow_builtin("bytes::from_utf8").expect("UTF-8");
            registry.bind_vm_cached(&mut machine).expect("bind catalog");

            machine.set_fuel(FUEL);
            let before = machine.get_fuel();
            let status = machine.run();
            let after = machine.get_fuel();
            let jit = machine.jit_config().enabled;
            let aot = machine.aot_exec_count();
            let native = machine.jit_native_exec_count();
            let resources = machine.host_context().resource_count();
            eprintln!(
                "EXP_VM {label}: status={status:?} jit={jit} aot={aot} native={native} fuel={before:?}->{after:?} resources={resources}"
            );
            assert_eq!(status.expect("guest Script must complete"), VmStatus::Halted);
            assert_eq!((jit, aot, native), (false, 0, 0));
            assert_eq!(before, Some(FUEL));
            assert!(matches!(after, Some(left) if left < FUEL));
            assert!(machine.capture_certification_stats().is_some_and(|stats| stats.valid));
            assert!(resources <= 1, "more than one host resource: {resources}");
            let stack = machine.stack().to_vec();
            machine.reset_for_reuse().expect("reset");
            assert_eq!(machine.host_context().resource_count(), 0);
            stack
        })
        .expect("interpreter thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

fn float_payload(label: &str, value: &Value) -> f64 {
    let Value::Map(boxed) = value else {
        panic!("{label}: expected a boxed Number, got {value:?}");
    };
    assert_eq!(
        boxed.get(&Value::string("tag")),
        Some(&Value::Int(3)),
        "{label}"
    );
    let payload = boxed.get(&Value::string("number"));
    let Some(Value::Float(number)) = payload else {
        panic!("{label}: expected a Float Number payload, got {payload:?}");
    };
    *number
}

fn expect_number(label: &str, value: &Value, bits: Option<u64>) {
    let number = float_payload(label, value);
    match bits {
        Some(bits) => assert_eq!(
            number.to_bits(),
            bits,
            "{label}: binary64 bits (got {:#018x}, expected {bits:#018x})",
            number.to_bits()
        ),
        None => assert!(number.is_nan(), "{label}: expected NaN, got {number:?}"),
    }
}

fn expect_points(label: &'static str, source: &'static str, points: &[(&str, Option<u64>)]) {
    let stack = execute(label, source);
    assert_eq!(
        stack.len(),
        points.len(),
        "one observation per original point"
    );
    for (value, &(point, bits)) in stack.iter().zip(points) {
        expect_number(point, value, bits);
    }
}

fn expect_completion(label: &'static str, source: &'static str) {
    let stack = execute(label, source);
    // The exception-capable Script route may leave a raw success marker.
    let completion = match stack.as_slice() {
        [value] | [Value::Bool(true), value] => value,
        _ => panic!("{label}: unexpected completion stack: {stack:?}"),
    };
    expect_number(label, completion, Some(HALF_EXP));
}

#[test]
fn p8_math_exp_special_domain() {
    expect_points(
        "special domain",
        SPECIAL,
        &[
            ("missing", None),
            ("NaN", None),
            ("+0", Some(0x3ff0_0000_0000_0000)),
            ("-0", Some(0x3ff0_0000_0000_0000)),
            ("+Infinity", Some(0x7ff0_0000_0000_0000)),
            ("-Infinity", Some(0x0000_0000_0000_0000)),
        ],
    );
}

#[test]
fn p8_math_exp_finite_and_boundary_points() {
    expect_points(
        "finite and boundary points",
        FINITE_AND_BOUNDARY,
        &[
            ("exp(-1)", Some(0x3fd7_8b56_362c_ef38)),
            ("exp(+0.5)", Some(HALF_EXP)),
            ("exp(-0.5)", Some(0x3fe3_68b2_fc6f_960a)),
            (
                "last finite-side overflow input",
                Some(0x4086_2e42_fefa_39ef),
            ),
            (
                "first infinite-side overflow input",
                Some(0x4086_2e42_fefa_39f0),
            ),
            (
                "last nonzero-side underflow input",
                Some(0xc087_4910_d52d_3051),
            ),
            (
                "first zero-side underflow input",
                Some(0xc087_4910_d52d_3052),
            ),
            ("exp just below overflow", Some(0x7fef_ffff_ffff_ff2a)),
            ("exp at overflow", Some(0x7ff0_0000_0000_0000)),
            (
                "exp just above rounding to zero",
                Some(0x0000_0000_0000_0001),
            ),
            ("exp rounded to +0", Some(0x0000_0000_0000_0000)),
        ],
    );
}

#[test]
fn p8_math_exp_call_shapes() {
    expect_completion("borrowed completion", CALL_SHAPES);
}

#[test]
fn p8_math_exp_conversion_and_order() {
    std::thread::scope(|scope| {
        scope.spawn(|| expect_completion("argument order", ARGUMENT_ORDER));
        scope.spawn(|| expect_completion("toString fallback", CONVERSION));
        scope.spawn(|| expect_completion("abrupt argument and conversion order", ABRUPT_ORDER));
    });
}
