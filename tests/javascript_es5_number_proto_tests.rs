//! `Number(value)`, `Number.prototype.toString(radix)` and `String(value)` on
//! the certified, JIT-disabled interpreter.
//!
//! The three call carriers used to reject every call with a TypeError. These
//! tests pin the bounded ES5 semantics that replaced those rejections:
//!
//! * `Number(value)` is `+0` with no argument, otherwise the bounded ToNumber
//!   (Number, Null, Boolean, String through the accepted StringNumericLiteral
//!   parser, and NaN for Undefined and Object).
//! * `Number.prototype.toString(radix)` reuses the accepted ES5 9.8.1
//!   shortest-digit producer for radix 10 and converts the integer part by
//!   exact repeated division for radix 2..=36.
//! * `String(value)` is the empty String with no argument, otherwise the
//!   bounded ToString through the accepted Number and fixed-scalar producers.
//!
//! Every value is compared inside the guest, so the assertion never depends on
//! a host-side projection of a guest String.

use vm::Value;

fn machine(source: &str) -> vm::Vm {
    pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| panic!("source={source:?} {error:?}"))
}

fn run(source: &str) -> Value {
    let mut machine = machine(source);
    machine.set_fuel(10_000_000);
    let status = machine.run();
    eprintln!(
        "NUMBER_PROTO_VM source={source:?} status={status:?} stack={:?}",
        machine.stack()
    );
    assert_eq!(
        status.unwrap_or_else(|error| panic!("source={source:?} {error:?}")),
        vm::VmStatus::Halted,
        "{source}"
    );
    machine.stack().last().expect("completion").clone()
}

fn code(source: &str) -> i64 {
    let result = run(source);
    let Value::Map(map) = result else {
        panic!("boxed value: {result:?}");
    };
    match map.get(&Value::string("number")) {
        Some(Value::Float(number)) => *number as i64,
        Some(Value::Int(number)) => *number,
        other => panic!("case: {other:?}\nsource={source:?}"),
    }
}

/// One guest program that walks every accepted value and reports the index of
/// the first mismatch. Each `else if` keeps a single failing case visible.
#[test]
fn number_string_calls_and_number_to_string_values() {
    let source = "var n=0;\
if(!(Number()===0))n=1;\
else if(!(Number(\"12\")===12))n=2;\
else if(!(Number(\" 3.5 \")===3.5))n=3;\
else if(!(Number(null)===0))n=4;\
else if(!(Number(undefined)!==Number(undefined)))n=5;\
else if(!(Number(\"abc\")!==Number(\"abc\")))n=6;\
else if(!(Number(true)===1))n=7;\
else if(!(Number(\"\")===0))n=8;\
else if(!((255).toString(16)===\"ff\"))n=9;\
else if(!((255).toString(2)===\"11111111\"))n=10;\
else if(!((255).toString(36)===\"73\"))n=11;\
else if(!((-255).toString(16)===\"-ff\"))n=12;\
else if(!((9007199254740991).toString(16)===\"1fffffffffffff\"))n=13;\
else if(!((0.1).toString()===\"0.1\"))n=14;\
else if(!((123.456).toString()===\"123.456\"))n=15;\
else if(!((-0).toString()===\"0\"))n=16;\
else if(!((1e21).toString()===\"1e+21\"))n=17;\
else if(!((1e20).toString()===\"100000000000000000000\"))n=18;\
else if(!((1e-7).toString()===\"1e-7\"))n=19;\
else if(!((0.000001).toString()===\"0.000001\"))n=20;\
else if(!((0/0).toString()===\"NaN\"))n=21;\
else if(!((1/0).toString()===\"Infinity\"))n=22;\
else if(!((-1/0).toString()===\"-Infinity\"))n=23;\
else if(!((5).toString()===\"5\"))n=24;\
else if(!((5).toString(undefined)===\"5\"))n=25;\
else if(!((5).toString(10)===\"5\"))n=26;\
else if(!((5).toString(10.9)===\"5\"))n=27;\
else if(!(String()===\"\"))n=28;\
else if(!(String(12)===\"12\"))n=29;\
else if(!(String(null)===\"null\"))n=30;\
else if(!(String(undefined)===\"undefined\"))n=31;\
else if(!(String(\"x\")===\"x\"))n=32;\
else if(!(String(true)===\"true\"))n=33;\
else if(!(String(0.1)===\"0.1\"))n=34;\
n;";
    assert_eq!(code(source), 0, "first failing index");
}

/// 15.7.4.2 step 3: `ToInteger(radix)` outside `2..=36` throws the guest
/// RangeError with the upstream message. `NaN` is not inside the domain even
/// though both bound comparisons are false, and `null` converts to `+0`. The
/// value formatting never runs first, so a NaN/Infinity receiver with a bad
/// radix throws as well.
#[test]
fn number_to_string_out_of_range_radix_is_range_error() {
    let source = "var n=0;\
try{(5).toString(1);}catch(e){if(e.name===\"RangeError\"&&e.message===\"radix must be between 2 and 36\")n=n+1;}\
try{(5).toString(37);}catch(e){if(e.name===\"RangeError\"&&e.message===\"radix must be between 2 and 36\")n=n+1;}\
try{(5).toString(0);}catch(e){if(e.name===\"RangeError\"&&e.message===\"radix must be between 2 and 36\")n=n+1;}\
try{(5).toString(-1);}catch(e){if(e.name===\"RangeError\"&&e.message===\"radix must be between 2 and 36\")n=n+1;}\
try{(5).toString(null);}catch(e){if(e.name===\"RangeError\"&&e.message===\"radix must be between 2 and 36\")n=n+1;}\
try{(5).toString(\"abc\");}catch(e){if(e.name===\"RangeError\"&&e.message===\"radix must be between 2 and 36\")n=n+1;}\
try{(5).toString(0/0);}catch(e){if(e.name===\"RangeError\"&&e.message===\"radix must be between 2 and 36\")n=n+1;}\
try{(0).toString(0/0);}catch(e){if(e.name===\"RangeError\"&&e.message===\"radix must be between 2 and 36\")n=n+1;}\
try{(0/0).toString(1);}catch(e){if(e.name===\"RangeError\"&&e.message===\"radix must be between 2 and 36\")n=n+1;}\
try{(1/0).toString(1);}catch(e){if(e.name===\"RangeError\"&&e.message===\"radix must be between 2 and 36\")n=n+1;}\
n;";
    assert_eq!(code(source), 10, "every invalid radix throws the RangeError");
}

/// In-domain fractional radix is `ToInteger`'d toward zero, so both ends of
/// the accepted interval keep working. A non-Number receiver still keeps the
/// ordinary TypeError: the receiver check precedes the radix check.
#[test]
fn number_to_string_radix_truncation_and_receiver_type_error() {
    let source = "var n=0;\
if((5).toString(10.9)===\"5\")n=n+1;\
if((5).toString(36.9)===\"5\")n=n+1;\
if((35).toString(36.9)===\"z\")n=n+1;\
try{(5).toString.call(null);}catch(e){if(e.name===\"TypeError\")n=n+1;}\
if(Number.prototype.toString.call(5)===\"5\")n=n+1;\
n;";
    assert_eq!(code(source), 5, "truncated radix accepted, receiver TypeError kept");
}

/// The bounded part of 15.7.4.2 refuses digit conversion it cannot compute
/// exactly. Those shapes are outside the admitted domain and keep the older
/// TypeError, which is also why they are listed apart from the radix rule.
#[test]
fn number_to_string_unimplemented_formatting_stays_type_error() {
    let source = "var n=0;\
try{(0.1).toString(16);}catch(e){if(e.name===\"TypeError\")n=n+1;}\
try{(5.5).toString(2);}catch(e){if(e.name===\"TypeError\")n=n+1;}\
try{(1e21).toString(16);}catch(e){if(e.name===\"TypeError\")n=n+1;}\
n;";
    assert_eq!(code(source), 3, "out-of-domain formatting keeps TypeError");
}

/// `new Number(...)` / `new String(...)` are upstream runtime TypeError
/// forms, not frontend refusals. The pinned engine allocates no wrapper
/// object; it throws a catchable TypeError, and this slice keeps only the
/// name (`TypeError`), not the exact message text.
#[test]
fn number_and_string_construction_runtime_type_error() {
    for source in [
        "var c=0;try{new Number(1);}catch(e){if(e.name===",
        "var c=0;try{new String(\"x\");}catch(e){if(e.name===",
    ] {
        let script = format!("{} \"TypeError\")c=1;}}c;", source);
        assert_eq!(code(&script), 1, "construction must throw TypeError: {script}");
        eprintln!("NUMBER_PROTO_CTOR source={script:?} runtime=TypeError");
    }
}
