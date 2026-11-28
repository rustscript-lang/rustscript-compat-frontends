//! Fixed common points with MicroQuickJS 6d4d7eb6b99011d5aab6e88a9b99e8fd4d86c6e9.
//! This is a pointwise witness set, not a claim about libm accuracy or the rest
//! of the finite domain. NaN is compared by category only.

use pd_vm_compat_frontends::{JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor, runtime};
use vm::{HostFunctionRegistry, Value, VmStatus};

const METHOD: &str = "atan";
const FUEL: u64 = 20_000_000;
const HALF_ATAN: u64 = 0x3fdd_ac67_0561_bb4f;

// Intermediate points are retained as individual boxed values by the
// straight-line Script route. The final direct call is the Script completion.
const SPECIAL: &str = r#"
var point0 = Math.atan(); point0;
var point1 = Math.atan(0 / 0); point1;
var point2 = Math.atan(0); point2;
var point3 = Math.atan(-0); point3;
var point4 = Math.atan(1 / 0); point4;
Math.atan(-1 / 0);
"#;

const FINITE: &str = r#"
var point0 = Math.atan(5e-324); point0;
var point1 = Math.atan(-5e-324); point1;
var point2 = Math.atan(1); point2;
var point3 = Math.atan(-1); point3;
var point4 = Math.atan(0.5); point4;
Math.atan(-0.5);
"#;

const CALL_SHAPES: &str = r#"
'use strict';
var atan = Math.atan;
var holder = {
    atan: atan, hits: 0,
    valueOf: function () { this.hits = this.hits + 1; throw 11; },
    toString: function () { this.hits = this.hits + 1; throw 12; }
};
if (atan.length !== 1) throw 'arity';
var direct = Math.atan(0.5);
var extracted = atan(0.5);
var borrowed = holder.atan(0.5);
if (direct !== 0.4636476090008061) throw 'direct';
if (extracted !== direct) throw 'extracted';
if (borrowed !== direct) throw 'borrowed';
if (holder.hits !== 0) throw 'receiver conversion';
borrowed;
"#;

// All three callers share the same tail object. The first conversion must
// observe the completed expression trace, not merely the same final counters.
const ARGUMENT_ORDER: &str = r#"
'use strict';
var atan = Math.atan;
var holder = { atan: atan };
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
var direct = Math.atan(mark(trace, 1, first), mark(trace, 2, tail));
if (trace.expr !== 12 || trace.first !== 1) throw 'direct order';
trace.expr = 0;
var extracted = atan(mark(trace, 1, first), mark(trace, 2, tail));
if (trace.expr !== 12 || trace.first !== 2) throw 'extracted order';
trace.expr = 0;
var borrowed = holder.atan(mark(trace, 1, first), mark(trace, 2, tail));
if (trace.expr !== 12 || trace.first !== 3) throw 'borrowed order';
if (trace.tail !== 0) throw 'extra argument converted';
if (direct !== 0.4636476090008061) throw 'valueOf result';
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
var converted = Math.atan(fallback);
if (fallback.phase !== 2 || converted !== 0.4636476090008061) throw 'toString fallback';
var bad = {
    valueOf: function () { return {}; },
    toString: function () { return {}; }
};
var caught = 0;
try { Math.atan(bad); } catch (badError) { caught = badError.name; }
if (caught !== 'TypeError') throw 'two object results';
converted;
"#;

const ABRUPT_ORDER: &str = r#"
'use strict';
var atan = Math.atan;
var holder = { atan: atan };
var trace = { expr: 0, tail: 0 };
var tail = {
    trace: trace,
    valueOf: function () { this.trace.tail = this.trace.tail + 1; throw 23; },
    toString: function () { this.trace.tail = this.trace.tail + 1; throw 24; }
};
function tailExpr(state, value) { state.expr = state.expr * 10 + 2; return value; }
function throwFirstExpr(state) { state.expr = state.expr * 10 + 1; throw 31; }
var caught = 0;
try { Math.atan(throwFirstExpr(trace), tailExpr(trace, tail)); } catch (exprError) { caught = exprError; }
if (caught !== 31 || trace.expr !== 1 || trace.tail !== 0) throw 'expression throw';
var getter = {
    trace: trace,
    get valueOf() {
        if (this.trace.expr !== 2) throw 32;
        throw 33;
    },
    toString: function () { throw 34; }
};
trace.expr = 0;
caught = 0;
try { atan(getter, tailExpr(trace, tail)); } catch (getterError) { caught = getterError; }
if (caught !== 33 || trace.expr !== 2 || trace.tail !== 0) throw 'getter throw';
var method = {
    trace: trace,
    valueOf: function () {
        if (this.trace.expr !== 2) throw 35;
        throw 36;
    },
    toString: function () { throw 37; }
};
trace.expr = 0;
caught = 0;
try { holder.atan(method, tailExpr(trace, tail)); } catch (methodError) { caught = methodError; }
if (caught !== 36 || trace.expr !== 2 || trace.tail !== 0) throw 'method throw';
Math.atan(0.5);
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
                "ATAN_VM {label}: status={status:?} jit={jit} aot={aot} native={native} fuel={before:?}->{after:?} resources={resources}"
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
        Some(bits) => assert_eq!(number.to_bits(), bits, "{label}: binary64 bits"),
        None => assert!(number.is_nan(), "{label}: expected NaN, got {number:?}"),
    }
}

fn expect_points(label: &'static str, source: &'static str, points: &[(&str, Option<u64>)]) {
    let stack = execute(label, source);
    assert_eq!(
        stack.len(),
        points.len(),
        "one observation per original call"
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
    expect_number(label, completion, Some(HALF_ATAN));
}

#[test]
fn p8_math_atan_special_domain() {
    expect_points(
        "special domain",
        SPECIAL,
        &[
            ("missing", None),
            ("NaN", None),
            ("+0", Some(0x0000_0000_0000_0000)),
            ("-0", Some(0x8000_0000_0000_0000)),
            ("+Infinity", Some(0x3ff9_21fb_5444_2d18)),
            ("-Infinity", Some(0xbff9_21fb_5444_2d18)),
        ],
    );
}

#[test]
fn p8_math_atan_finite_points() {
    expect_points(
        "finite points",
        FINITE,
        &[
            ("+minSub", Some(0x0000_0000_0000_0001)),
            ("-minSub", Some(0x8000_0000_0000_0001)),
            ("+1", Some(0x3fe9_21fb_5444_2d18)),
            ("-1", Some(0xbfe9_21fb_5444_2d18)),
            ("+0.5", Some(HALF_ATAN)),
            ("-0.5", Some(0xbfdd_ac67_0561_bb4f)),
        ],
    );
}

#[test]
fn p8_math_atan_call_shapes() {
    expect_completion("borrowed completion", CALL_SHAPES);
}

#[test]
fn p8_math_atan_conversion_and_order() {
    std::thread::scope(|scope| {
        scope.spawn(|| expect_completion("argument order", ARGUMENT_ORDER));
        scope.spawn(|| expect_completion("toString fallback", CONVERSION));
        scope.spawn(|| expect_completion("abrupt argument and conversion order", ABRUPT_ORDER));
    });
}
