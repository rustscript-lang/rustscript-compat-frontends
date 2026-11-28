//! `Math.trunc` on the public ES5 interpreter, checked against MicroQuickJS 6d4d7eb6.
use pd_vm_compat_frontends::runtime::{array, cli, scalar_error, scalar_math};
use pd_vm_compat_frontends::{JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor};
use vm::{HostFunctionRegistry, Value, VmStatus};

const FUEL: u64 = 10_000_000;

fn number(source: &str) -> f64 {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn_scoped(scope, || run_number(source))
            .expect("interpreter thread")
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

fn run_number(source: &str) -> f64 {
    let catalog = cli::reference_array_cli_effective_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_host_api_catalog(catalog.clone())
        .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN);
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        javascript_es5_flavor(),
        options,
    )
    .unwrap_or_else(|error| panic!("ES5 compilation failed: {error:?}\n{source}"));
    let mut machine = compiled.into_vm().expect("certified VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    array::initialize_typescript_owned_arrays(&mut machine, array::OwnedArrayLimits::default())
        .expect("owned arrays");
    let mut registry = HostFunctionRegistry::restricted();
    for module in [
        array::typescript_array_host_module(),
        array::typescript_owned_array_host_module(),
        array::typescript_array_snapshot_host_module(),
        scalar_math::typescript_scalar_math_host_module(),
        scalar_error::typescript_error_host_module(),
    ] {
        module
            .install_from_catalog(&mut registry, &catalog)
            .expect("production host catalog");
    }
    // Math demand installs the whole namespace, not just the tested member.
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
            .expect("Math builtin");
    }
    registry
        .allow_builtin("bytes::from_utf8")
        .expect("numeric formatting builtin");
    registry.bind_vm_cached(&mut machine).expect("VM binding");

    machine.set_fuel(FUEL);
    assert_eq!(machine.get_fuel(), Some(FUEL));
    let status = machine
        .run()
        .unwrap_or_else(|error| panic!("interpreter failed: {error:?}\n{source}"));
    assert_eq!(status, VmStatus::Halted, "{source}");
    assert!(matches!(machine.get_fuel(), Some(left) if left < FUEL));
    assert!(!machine.jit_config().enabled);
    assert_eq!(
        (machine.aot_exec_count(), machine.jit_native_exec_count()),
        (0, 0)
    );
    assert!(
        machine
            .capture_certification_stats()
            .is_some_and(|stats| stats.valid)
    );
    assert_eq!(machine.host_context().resource_count(), 1);
    let stats = array::owned_array_runtime_stats(&mut machine).expect("real JS realm");
    assert_eq!((stats.pending, stats.registered), (0, 1));

    let Some(Value::Map(fields)) = machine.stack().last() else {
        panic!(
            "expected a boxed completion, got {:?}\n{source}",
            machine.stack()
        );
    };
    assert_eq!(
        fields.get(&Value::string("tag")),
        Some(&Value::Int(3)),
        "Number tag\n{source}"
    );
    let payload = fields.get(&Value::string("number"));
    let Some(Value::Float(result)) = payload else {
        panic!("expected Float payload, got {payload:?}\n{source}");
    };
    let result = *result;
    machine.reset_for_reuse().expect("reset interpreter");
    assert_eq!(machine.host_context().resource_count(), 0);
    result
}

fn expect_bits(source: &str, expected: f64) {
    let actual = number(source);
    assert_eq!(
        actual.to_bits(),
        expected.to_bits(),
        "Float bits: got {:#018x}, expected {:#018x}\n{source}",
        actual.to_bits(),
        expected.to_bits(),
    );
}

#[test]
fn trunc_toward_zero() {
    for (source, expected) in [
        ("Math.trunc(-1.9);", -1.0),
        ("Math.trunc(1.9);", 1.0),
        ("Math.trunc(9007199254740991);", 9007199254740991.0),
    ] {
        expect_bits(source, expected);
    }
}

#[test]
fn trunc_signed_zero() {
    for (source, expected) in [
        ("Math.trunc(-0);", -0.0),
        ("Math.trunc(0);", 0.0),
        ("Math.trunc(-0.75);", -0.0),
        ("Math.trunc(0.75);", 0.0),
        ("Math.trunc(-5e-324);", -0.0),
        ("Math.trunc(5e-324);", 0.0),
    ] {
        expect_bits(source, expected);
    }
}

#[test]
fn trunc_non_finite() {
    for source in ["Math.trunc();", "Math.trunc(0 / 0);"] {
        assert!(number(source).is_nan(), "expected NaN\n{source}");
    }
    expect_bits("Math.trunc(1 / 0);", f64::INFINITY);
    expect_bits("Math.trunc(-1 / 0);", f64::NEG_INFINITY);
}

const CALL_SHAPES: &str = r#"
'use strict';
var trunc = Math.trunc;
var borrower = {run: trunc, valueOf: function () { throw 'receiver coerced'; }};
if (trunc.length !== 1) throw 'length';
if (Math.trunc(2.75) !== 2) throw 'direct';
if (trunc(-2.75) !== -2) throw 'extracted';
borrower.run(-0.75);
"#;

#[test]
fn trunc_call_shapes() {
    expect_bits(CALL_SHAPES, -0.0);
}

const OBJECT_CONVERSION: &str = r#"
'use strict';
var first = {
    trace: 0,
    get valueOf() { this.trace = this.trace * 10 + 1; return this.convert; },
    convert: function () { this.trace = this.trace * 10 + 2; return -0.75; },
    toString: function () { throw 'unexpected fallback'; }
};
if (1 / Math.trunc(first) !== -1 / 0) throw 'primitive result';
if (first.trace !== 12) throw 'valueOf lookup and invocation';
var fallback = {
    trace: 0,
    valueOf: function () { this.trace = this.trace * 10 + 1; return {}; },
    toString: function () { this.trace = this.trace * 10 + 2; return '-1.9'; }
};
var result = Math.trunc(fallback);
if (fallback.trace !== 12) throw 'toString fallback order';
result;
"#;

#[test]
fn trunc_object_conversion() {
    expect_bits(OBJECT_CONVERSION, -1.0);
}

const OBJECT_ERRORS: &str = r#"
'use strict';
var getter = {
    marker: {}, hits: 0,
    get valueOf() { this.hits = this.hits + 1; throw this.marker; },
    toString: function () { this.hits = 99; return '9'; }
};
var caught;
try { Math.trunc(getter); } catch (getterError) { caught = getterError; }
if (caught !== getter.marker) throw 'getter marker';
if (getter.hits !== 1) throw 'getter must stop conversion';
var method = {
    marker: {}, hits: 0,
    valueOf: function () { this.hits = this.hits + 1; throw this.marker; },
    toString: function () { this.hits = 99; return '9'; }
};
caught = undefined;
try { Math.trunc(method); } catch (methodError) { caught = methodError; }
if (caught !== method.marker) throw 'method marker';
if (method.hits !== 1) throw 'method must stop conversion';
var invalid = {
    trace: 0,
    valueOf: function () { this.trace = this.trace * 10 + 1; return {}; },
    toString: function () { this.trace = this.trace * 10 + 2; return {}; }
};
caught = undefined;
try { Math.trunc(invalid); } catch (invalidError) { caught = invalidError.name; }
if (caught !== 'TypeError') throw 'two object results must throw TypeError';
if (invalid.trace !== 12) throw 'two object results must try both hooks';
Math.trunc(-1.9);
"#;

#[test]
fn trunc_object_errors() {
    expect_bits(OBJECT_ERRORS, -1.0);
}

const IGNORED_TAILS: &str = r#"
'use strict';
var state = {order: 0, valueHits: 0, stringHits: 0};
var first = {
    state: state,
    valueOf: function () {
        if (this.state.order !== 12) throw 'conversion before both expressions';
        this.state.order = 123;
        return -0.75;
    }
};
var tail = {
    state: state,
    valueOf: function () { this.state.valueHits = this.state.valueHits + 1; return {}; },
    toString: function () { this.state.stringHits = this.state.stringHits + 1; return '7'; }
};
var trunc = Math.trunc;
var borrower = {run: trunc};
var result = Math.trunc((state.order = 1, first), (state.order = state.order * 10 + 2, tail));
if (1 / result !== -1 / 0) throw 'direct result';
if (state.order !== 123) throw 'direct order';
state.order = 0;
result = trunc((state.order = 1, first), (state.order = state.order * 10 + 2, tail));
if (1 / result !== -1 / 0) throw 'extracted result';
if (state.order !== 123) throw 'extracted order';
state.order = 0;
result = borrower.run((state.order = 1, first), (state.order = state.order * 10 + 2, tail));
if (state.order !== 123) throw 'borrowed order';
if (state.valueHits !== 0) throw 'tail valueOf ran';
if (state.stringHits !== 0) throw 'tail toString ran';
result;
"#;

#[test]
fn trunc_evaluates_tails_without_coercing_them() {
    expect_bits(IGNORED_TAILS, -0.0);
}

const ABRUPT_FIRST: &str = r#"
'use strict';
var state = {order: 0, valueHits: 0, stringHits: 0};
var tail = {
    state: state,
    valueOf: function () { this.state.valueHits = this.state.valueHits + 1; return {}; },
    toString: function () { this.state.stringHits = this.state.stringHits + 1; return '7'; }
};
function fail() { throw 81; }
var caught;
try { Math.trunc((state.order = 1, fail()), (state.order = 12, tail)); }
catch (expressionError) { caught = expressionError; }
if (caught !== 81) throw 'first expression marker';
if (state.order !== 1) throw 'tail expression ran after abrupt first expression';
var first = {
    state: state,
    valueOf: function () {
        if (this.state.order !== 12) throw 'conversion before tail expression';
        this.state.order = 123;
        throw 82;
    },
    toString: function () { throw 'unexpected fallback'; }
};
state.order = 0;
caught = undefined;
try { Math.trunc((state.order = 1, first), (state.order = state.order * 10 + 2, tail)); }
catch (conversionError) { caught = conversionError; }
if (caught !== 82) throw 'first conversion marker';
if (state.order !== 123) throw 'tail expression must precede abrupt conversion';
if (state.valueHits !== 0) throw 'tail valueOf ran';
if (state.stringHits !== 0) throw 'tail toString ran';
Math.trunc(-1.9);
"#;

#[test]
fn trunc_preserves_abrupt_first_argument() {
    expect_bits(ABRUPT_FIRST, -1.0);
}
