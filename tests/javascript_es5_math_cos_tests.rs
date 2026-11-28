//! Fixed common points with MicroQuickJS 6d4d7eb6b99011d5aab6e88a9b99e8fd4d86c6e9.
//! Run each original Script through the certified ES5 interpreter. These points
//! do not specify the rest of the finite domain or portable libm accuracy.

use pd_vm_compat_frontends::{JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor, runtime};
use vm::{HostFunctionRegistry, Value, VmStatus};

const METHOD: &str = "cos";
const FUEL: u64 = 10_000_000;
const ONE: u64 = 0x3ff0_0000_0000_0000;
const HALF_COS: u64 = 0x3fec_1528_065b_7d50;
const UNIT_COS: u64 = 0x3fe1_4a28_0fb5_068c;

// Each intermediate point is observed independently on the ordinary Script
// stack; the last direct call is also the Script completion.
const SPECIAL: &str = r#"
var point0 = Math.cos(); point0;
var point1 = Math.cos(0 / 0); point1;
var point2 = Math.cos(0); point2;
var point3 = Math.cos(-0); point3;
var point4 = Math.cos(1 / 0); point4;
Math.cos(-1 / 0);
"#;

const FINITE: &str = r#"
var point0 = Math.cos(5e-324); point0;
var point1 = Math.cos(-5e-324); point1;
var point2 = Math.cos(0.5); point2;
var point3 = Math.cos(-0.5); point3;
var point4 = Math.cos(1); point4;
Math.cos(-1);
"#;

const CALL_SHAPES: &str = r#"
'use strict';
var cos = Math.cos;
var holder = {
    cos: cos, hits: 0,
    valueOf: function () { this.hits = this.hits + 1; throw 11; },
    toString: function () { this.hits = this.hits + 1; throw 12; }
};
if (cos.length !== 1) throw 'arity';
var direct = Math.cos(0.5);
var extracted = cos(-0.5);
var borrowed = holder.cos(0.5);
if (direct !== 0.8775825618903728) throw 'direct';
if (extracted !== direct) throw 'extracted';
if (borrowed !== direct) throw 'borrowed';
if (holder.hits !== 0) throw 'receiver conversion';
borrowed;
"#;

// Separate Scripts keep the three checks within the same per-VM fuel bound.
const ARGUMENT_ORDER: &str = r#"
'use strict';
var cos = Math.cos;
var holder = { cos: cos };
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

var direct = Math.cos(mark(trace, 1, first), mark(trace, 2, tail));
if (trace.expr !== 12 || trace.first !== 1) throw 'direct order';
trace.expr = 0;
var extracted = cos(mark(trace, 1, first), mark(trace, 2, tail));
if (trace.expr !== 12 || trace.first !== 2) throw 'extracted order';
trace.expr = 0;
var borrowed = holder.cos(mark(trace, 1, first), mark(trace, 2, tail));
if (trace.expr !== 12 || trace.first !== 3) throw 'borrowed order';
if (trace.tail !== 0) throw 'extra actual converted';
if (direct !== 0.8775825618903728) throw 'valueOf result';
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
var converted = Math.cos(fallback);
if (fallback.phase !== 2 || converted !== 0.8775825618903728) throw 'toString fallback';

var bad = {
    valueOf: function () { return {}; },
    toString: function () { return {}; }
};
var caught = 0;
try { Math.cos(bad); } catch (badError) { caught = badError.name; }
if (caught !== 'TypeError') throw 'two object results';
converted;
"#;

const ABRUPT_ORDER: &str = r#"
'use strict';
var cos = Math.cos;
var holder = { cos: cos };
var trace = { expr: 0, tail: 0 };
var tail = {
    trace: trace,
    valueOf: function () { this.trace.tail = this.trace.tail + 1; throw 23; },
    toString: function () { this.trace.tail = this.trace.tail + 1; throw 24; }
};
function tailExpr(state, value) { state.expr = state.expr * 10 + 2; return value; }
function throwFirstExpr(state) { state.expr = state.expr * 10 + 1; throw 31; }
var caught = 0;
try { Math.cos(throwFirstExpr(trace), tailExpr(trace, tail)); } catch (exprError) { caught = exprError; }
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
try { cos(getter, tailExpr(trace, tail)); } catch (getterError) { caught = getterError; }
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
try { holder.cos(method, tailExpr(trace, tail)); } catch (methodError) { caught = methodError; }
if (caught !== 36 || trace.expr !== 2 || trace.tail !== 0) throw 'method throw';
Math.cos(0.5);
"#;

fn execute(label: &'static str, source: &'static str) -> Vec<Value> {
    // Keep the fixtures on one Math member, even though the real registry
    // must permit the whole shared numeric prelude.
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
            // Math demand installs the whole namespace and the shared numeric
            // helpers, even for a Script that reads only Math.cos.
            for name in [
                "abs", "acos", "asin", "atan", "atan2", "ceil", "copysign", "cos", "exp", "exp2",
                "floor", "is_finite", "is_infinite", "is_nan", "ln", "log10", "log2", "powf",
                "signum", "sin", "sqrt", "tan", "trunc",
            ] {
                registry
                    .allow_builtin(format!("math::{name}"))
                    .unwrap_or_else(|err| panic!("math::{name}: {err:?}"));
            }
            registry.allow_builtin("bytes::from_utf8").expect("UTF-8");
            registry.bind_vm_cached(&mut machine).expect("bind catalog");

            machine.set_fuel(FUEL);
            let status = machine.run();
            let remaining = machine.get_fuel();
            let resources = machine.host_context().resource_count();
            eprintln!(
                "COS_VM {label}: status={status:?} jit={} aot={} native={} fuel={FUEL}->{remaining:?} resources={resources}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
            );
            assert_eq!(status.expect("guest Script must complete"), VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!((machine.aot_exec_count(), machine.jit_native_exec_count()), (0, 0));
            assert!(matches!(remaining, Some(left) if left < FUEL));
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

fn expect_number(label: &str, value: &Value, bits: Option<u64>) {
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
    for (value, &(label, bits)) in stack.iter().zip(points) {
        expect_number(label, value, bits);
    }
}

fn expect_completion(label: &'static str, source: &'static str) {
    let stack = execute(label, source);
    // The exception-capable Script route may carry a raw success marker
    // before the boxed completion. It is not itself a guest observation.
    let completion = match stack.as_slice() {
        [value] | [Value::Bool(true), value] => value,
        _ => panic!("{label}: unexpected completion stack: {stack:?}"),
    };
    expect_number(label, completion, Some(HALF_COS));
}

#[test]
fn p8_math_cos_special_domain() {
    expect_points(
        "special domain",
        SPECIAL,
        &[
            ("missing", None),
            ("NaN", None),
            ("+0", Some(ONE)),
            ("-0", Some(ONE)),
            ("+Infinity", None),
            ("-Infinity", None),
        ],
    );
}

#[test]
fn p8_math_cos_finite_points() {
    expect_points(
        "finite points",
        FINITE,
        &[
            ("+minSub", Some(ONE)),
            ("-minSub", Some(ONE)),
            ("+0.5", Some(HALF_COS)),
            ("-0.5", Some(HALF_COS)),
            ("+1", Some(UNIT_COS)),
            ("-1", Some(UNIT_COS)),
        ],
    );
}

#[test]
fn p8_math_cos_call_shapes() {
    expect_completion("borrowed cos completion", CALL_SHAPES);
}

#[test]
fn p8_math_cos_conversion_and_order() {
    std::thread::scope(|scope| {
        scope.spawn(|| expect_completion("argument order", ARGUMENT_ORDER));
        scope.spawn(|| expect_completion("toString fallback", CONVERSION));
        scope.spawn(|| expect_completion("abrupt argument and conversion order", ABRUPT_ORDER));
    });
}
