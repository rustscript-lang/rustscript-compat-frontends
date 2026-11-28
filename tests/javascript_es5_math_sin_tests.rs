//! Fixed public Math.sin witnesses shared with MicroQuickJS at 6d4d7eb6.
//! Only the listed binary64 points are pinned; this is not a libm parity test.

use pd_vm_compat_frontends::compile_javascript_es5_source;
use vm::{Value, VmStatus};

const SPECIAL: &str = r#"var point0 = Math.sin(); point0;
var point1 = Math.sin(NaN); point1;
var point2 = Math.sin(0); point2;
var point3 = Math.sin(-0); point3;
var point4 = Math.sin(Infinity); point4;
Math.sin(-Infinity);
"#;

const FINITE: &str = r#"var point0 = Math.sin(5e-324); point0;
var point1 = Math.sin(-5e-324); point1;
var point2 = Math.sin(0.5); point2;
Math.sin(-0.5);
"#;

const SHAPES: &str = r#"'use strict';
var f = Math.sin;
var receiver = {
    sin: f, hooks: 0,
    valueOf: function() { this.hooks += 1; return 1; },
    toString: function() { this.hooks += 1; return '1'; }
};
if (f.length !== 1) { throw 'sin arity'; }
if (Math.sin(0.5) !== 0.479425538604203) { throw 'direct'; }
if (f(-0.5) !== -0.479425538604203) { throw 'extracted'; }
var result = receiver.sin(0.5);
if (receiver.hooks !== 0) { throw 'borrowed receiver coerced'; }
result;
"#;

const CONVERSION_AND_ORDER: &str = r#"'use strict';
var f = Math.sin;
var receiver = { sin: f };
var trace = { order: 0, head: 0, tail: 0 };
var head = {
    state: trace,
    valueOf: function() {
        this.state.head += 1;
        this.state.order = this.state.order * 10 + 4;
        return 0.5;
    },
    toString: function() { throw 'primitive valueOf must stop conversion'; }
};
var tail = {
    state: trace,
    valueOf: function() { this.state.tail += 1; return this; },
    toString: function() { this.state.tail += 1; return '0.5'; }
};
var direct = Math.sin((trace.order = 1, head), (trace.order = trace.order * 10 + 2, tail), (trace.order = trace.order * 10 + 3, tail));
if (direct !== 0.479425538604203) { throw 'direct valueOf'; }
if (trace.order !== 1234 || trace.head !== 1 || trace.tail !== 0) { throw 'direct order'; }
trace.order = 0;
var extracted = f((trace.order = 1, head), (trace.order = trace.order * 10 + 2, tail), (trace.order = trace.order * 10 + 3, tail));
if (extracted !== 0.479425538604203) { throw 'extracted valueOf'; }
if (trace.order !== 1234 || trace.head !== 2 || trace.tail !== 0) { throw 'extracted order'; }
trace.order = 0;
var borrowed = receiver.sin((trace.order = 1, head), (trace.order = trace.order * 10 + 2, tail), (trace.order = trace.order * 10 + 3, tail));
if (borrowed !== 0.479425538604203) { throw 'borrowed valueOf'; }
if (trace.order !== 1234 || trace.head !== 3 || trace.tail !== 0) { throw 'borrowed order'; }

var fallback = {
    order: 0,
    valueOf: function() { this.order = this.order * 10 + 1; return this; },
    toString: function() { this.order = this.order * 10 + 2; return '0.5'; }
};
var result = Math.sin(fallback);
if (fallback.order !== 12) { throw 'valueOf before toString'; }

var marker = {};
var throwing = {
    state: trace, marker: marker,
    argument: function() { this.state.order = 1; throw this.marker; },
    valueOf: function() { this.state.order = this.state.order * 10 + 3; throw this.marker; },
    toString: function() { throw 'throwing valueOf fell back'; }
};
trace.order = 0;
var argError;
try { Math.sin(throwing.argument(), (trace.order = 9, tail)); }
catch (argFailure) { argError = argFailure; }
if (argError !== marker || trace.order !== 1 || trace.tail !== 0) { throw 'abrupt first argument'; }
trace.order = 0;
var methodError;
try { Math.sin((trace.order = 1, throwing), (trace.order = trace.order * 10 + 2, tail)); }
catch (methodFailure) { methodError = methodFailure; }
if (methodError !== marker || trace.order !== 123 || trace.tail !== 0) { throw 'abrupt first conversion'; }

var getter = {
    state: trace, marker: marker,
    get valueOf() { this.state.order = this.state.order * 10 + 3; throw this.marker; },
    toString: function() { throw 'throwing getter fell back'; }
};
trace.order = 0;
var getterError;
try { Math.sin((trace.order = 1, getter), (trace.order = trace.order * 10 + 2, tail)); }
catch (getterFailure) { getterError = getterFailure; }
if (getterError !== marker || trace.order !== 123 || trace.tail !== 0) { throw 'abrupt valueOf lookup'; }

var neither = {
    order: 0,
    valueOf: function() { this.order = this.order * 10 + 1; return this; },
    toString: function() { this.order = this.order * 10 + 2; return this; }
};
var className;
try { Math.sin(neither); }
catch (primitiveFailure) { className = primitiveFailure.name; }
if (className !== 'TypeError' || neither.order !== 12) { throw 'two object results'; }
result;
"#;

#[derive(Clone, Copy)]
enum Expected {
    Bits(u64),
    Nan,
}

use Expected::{Bits, Nan};

const STANDARD_FUEL: u64 = 10_000_000;

// Literal bits from the pinned Micro build, never computed by the host sin.
const SIN_HALF: Expected = Bits(0x3fde_aee8_744b_05f0);
const SIN_NEG_HALF: Expected = Bits(0xbfde_aee8_744b_05f0);

fn check_number(value: &Value, expected: Expected, label: &str) {
    let Value::Map(fields) = value else {
        panic!("{label}: expected Number box, got {value:?}");
    };
    assert_eq!(
        fields.get(&Value::string("tag")),
        Some(&Value::Int(3)),
        "{label}: Number tag"
    );
    let Some(Value::Float(number)) = fields.get(&Value::string("number")) else {
        panic!("{label}: expected Float payload, got {value:?}");
    };
    match expected {
        Bits(bits) => assert_eq!(
            number.to_bits(),
            bits,
            "{label}: binary64 bits (actual {:#018x}, expected {bits:#018x})",
            number.to_bits()
        ),
        Nan => assert!(number.is_nan(), "{label}: expected NaN, got {number}"),
    }
}

fn check_script(
    source: &'static str,
    expected: &'static [(&str, Expected)],
    completion_only: bool,
    fuel: u64,
) {
    let mut math_members = source.match_indices("Math.").map(|(offset, prefix)| {
        source[offset + prefix.len()..]
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
            .next()
            .unwrap()
    });
    assert_eq!(math_members.next(), Some("sin"));
    assert!(math_members.all(|member| member == "sin"));

    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(move || {
            let mut machine = compile_javascript_es5_source(source)
                .unwrap_or_else(|error| panic!("ES5 compile failed: {error:?}"));
            assert!(!machine.jit_config().enabled);
            assert!(
                machine
                    .capture_certification_stats()
                    .is_some_and(|stats| stats.valid)
            );
            machine.set_fuel(fuel);
            assert_eq!(machine.get_fuel(), Some(fuel));
            let status = machine.run().unwrap_or_else(|error| {
                panic!("guest run failed: {error:?}\n{source}");
            });
            let fuel_left = machine.get_fuel();
            let aot = machine.aot_exec_count();
            let native = machine.jit_native_exec_count();
            let resources = machine.host_context().resource_count();
            assert_eq!(status, VmStatus::Halted, "fuel {fuel}->{fuel_left:?}");
            assert!(matches!(fuel_left, Some(left) if left < fuel));
            assert_eq!((aot, native), (0, 0));
            assert!(resources <= 1);

            // Exception-capable Scripts can have a raw success marker before the
            // boxed guest completion. It is not a guest test observation.
            let stack = match machine.stack() {
                [Value::Bool(true), rest @ ..] if completion_only => rest,
                stack => stack,
            };
            assert_eq!(stack.len(), expected.len(), "guest observations: {stack:?}");
            for (value, &(label, expectation)) in stack.iter().zip(expected) {
                check_number(value, expectation, label);
            }
            machine.reset_for_reuse().expect("reset");
            let after_reset = machine.host_context().resource_count();
            assert_eq!(after_reset, 0);
            eprintln!(
                "Math.sin: {status:?}; JIT disabled; AOT={aot}; native={native}; fuel {fuel}->{fuel_left:?}; resources {resources}->{after_reset}; {} Float observations",
                expected.len()
            );
        })
        .expect("test thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
}

#[test]
fn p8_math_sin_special_domain() {
    check_script(
        SPECIAL,
        &[
            ("missing", Nan),
            ("NaN", Nan),
            ("+0", Bits(0x0000_0000_0000_0000)),
            ("-0", Bits(0x8000_0000_0000_0000)),
            ("+Infinity", Nan),
            ("-Infinity", Nan),
        ],
        false,
        STANDARD_FUEL,
    );
}

#[test]
fn p8_math_sin_finite_points() {
    check_script(
        FINITE,
        &[
            ("minimum subnormal", Bits(0x0000_0000_0000_0001)),
            ("negative minimum subnormal", Bits(0x8000_0000_0000_0001)),
            ("+0.5", SIN_HALF),
            ("-0.5", SIN_NEG_HALF),
        ],
        false,
        STANDARD_FUEL,
    );
}

#[test]
fn p8_math_sin_call_shapes() {
    check_script(
        SHAPES,
        &[("borrowed result", SIN_HALF)],
        true,
        STANDARD_FUEL,
    );
}

#[test]
fn p8_math_sin_conversion_and_order() {
    check_script(
        CONVERSION_AND_ORDER,
        &[("toString fallback result", SIN_HALF)],
        true,
        // This Script exercises multiple callbacks and four caught errors.
        20_000_000,
    );
}
