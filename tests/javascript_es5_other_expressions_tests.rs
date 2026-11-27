//! LANG:other_expressions contract: `typeof`, `void`, comma, conditional.
//!
//! Each source returns a numeric bit cookie over the observed outcomes. The
//! expected masks were pinned against MicroQuickJS `6d4d7eb6` (`mqjs`); see
//! the plan's `LANG:other_expressions` evidence section. A failure reports the
//! actual cookie so the failing bit can be traced to one source clause.

fn cookie(source: &str) -> f64 {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!("OTHER_EXPR_SOURCE {source:?}");
            let mut machine =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                    .unwrap_or_else(|error| panic!("source={source:?} {error:?}"));
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let status = machine.run().unwrap_or_else(|error| {
                panic!("source={source:?} run error={error:?}")
            });
            assert_eq!(status, vm::VmStatus::Halted, "source={source:?}");
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0),
                "source={source:?}"
            );
            let vm::Value::Map(map) = machine.stack().last().unwrap() else {
                panic!("cookie must be boxed: source={source:?} stack={:?}", machine.stack())
            };
            assert_eq!(
                map.get(&vm::Value::string("tag")),
                Some(&vm::Value::Int(3)),
                "cookie tag: source={source:?}"
            );
            let Some(vm::Value::Float(value)) = map.get(&vm::Value::string("number")) else {
                panic!("cookie payload: source={source:?}");
            };
            eprintln!("OTHER_EXPR_COOKIE {value}");
            *value
        })
        .unwrap()
        .join()
        .unwrap()
}

fn assert_cookie(source: &str, expected: f64) {
    let actual = cookie(source);
    assert_eq!(
        actual.to_bits(),
        expected.to_bits(),
        "cookie mismatch: source={source:?} actual={actual} expected={expected}"
    );
}

#[test]
fn other_expressions_typeof_names_and_values() {
    // Bits: 1 missing name; 2 hoisted function; 4/8/16 primitive strings;
    // 32 null; 64 object; 128 array; 256 regexp; 512 second missing;
    // 1024 comma operand.
    assert_cookie(
        r#"var r=0;
function f(){}
var s="x";
var n=1;
var b=true;
var nil=null;
var o={};
var a=[];
var re=/a/;
if (typeof notdefined === "undefined") r = r + 1;
if (typeof f === "function") r = r + 2;
if (typeof s === "string") r = r + 4;
if (typeof n === "number") r = r + 8;
if (typeof b === "boolean") r = r + 16;
if (typeof nil === "object") r = r + 32;
if (typeof o === "object") r = r + 64;
if (typeof a === "object") r = r + 128;
if (typeof re === "object") r = r + 256;
if (typeof missing2 === "undefined") r = r + 512;
if (typeof (0,1) === "number") r = r + 1024;
r;"#,
        2047.0,
    );
}

#[test]
fn other_expressions_typeof_arguments_and_scope() {
    // Bits: arguments in a closed function and at top level; declared-but-
    // uninitialized binding; undefined/NaN/Infinity; void operand; object and
    // array operands; missing name inside a function; anonymous function
    // expression.
    assert_cookie(
        r#"var r=0;
var u;
var o={};
var a=[];
function g(x){ return typeof arguments; }
function h(){ return typeof nope; }
function hs(){"use strict"; return typeof nope;}
if (g(1) === "object") r = r + 1;
if (typeof arguments === "undefined") r = r + 2;
if (typeof u === "undefined") r = r + 4;
if (typeof undefined === "undefined") r = r + 8;
if (typeof NaN === "number") r = r + 16;
if (typeof Infinity === "number") r = r + 32;
if (typeof (void 0) === "undefined") r = r + 64;
if (typeof (o) === "object") r = r + 128;
if (typeof (a) === "object") r = r + 256;
if (h() === "undefined") r = r + 512;
if (typeof function(){} === "function") r = r + 1024;
if (hs() === "undefined") r = r + 2048;
r;"#,
        4095.0,
    );
}

#[test]
fn other_expressions_typeof_dynamic_operand_tags() {
    // Conditional and comma results are inspected by the real tag, and a
    // single evaluation of the assignment supplies the observed type.
    assert_cookie(
        r#"var r=0;
var n=0;
var s="s";
if (typeof (1 ? 1 : "s") === "number") r = r + 1;
if (typeof (0 ? 1 : "s") === "string") r = r + 2;
if (typeof (0, s) === "string") r = r + 4;
if (typeof (n = n + 1) === "number") r = r + 8;
if (n === 1) r = r + 16;
if (typeof (1 ? s : 1) === "string") r = r + 32;
if (typeof (0 ? s : 1) === "number") r = r + 64;
r;"#,
        127.0,
    );
}

#[test]
fn other_expressions_void_evaluates_once_and_discards() {
    // Bits: `void 0`; `void f()` returns undefined after one call; side effect
    // count; typeof of void; void as the left arm of a comma.
    assert_cookie(
        r#"var r=0;
var box={n:0};
function f(o){ o.n = o.n + 1; return 7; }
if (void 0 === undefined) r = r + 1;
if (void f(box) === undefined) r = r + 2;
if (box.n === 1) r = r + 4;
if (typeof void 0 === "undefined") r = r + 8;
if ((void 0, 5) === 5) r = r + 16;
r;"#,
        31.0,
    );
}

#[test]
fn other_expressions_comma_value_and_side_effect_order() {
    // Bits: value of `(x=1,y=2,x+y)`; both assignments applied; repeated
    // assignment sequence; left-to-right callee order through an object state.
    assert_cookie(
        r#"var r=0;
var x=0;
var y=0;
var n=0;
var box={s:""};
function t(o,v){ o.s = o.s + v; return v; }
if ((x = 1, y = 2, x + y) === 3) r = r + 1;
if (x === 1) r = r + 2;
if (y === 2) r = r + 4;
if ((n = n + 1, n = n + 1, n) === 2) r = r + 8;
if ((t(box,"a"), t(box,"b"), t(box,"c")) === "c") r = r + 16;
if (box.s === "abc") r = r + 32;
r;"#,
        63.0,
    );
}

#[test]
fn other_expressions_comma_in_for_header() {
    // Comma is accepted in both the init and update clauses; both clauses run
    // left to right once per iteration and the loop exits with the expected
    // post-state.
    assert_cookie(
        r#"var r=0;
var i=0;
var j=3;
var n=0;
for (i = 0, j = 3; i < 2; i = i + 1, j = j - 1) { n = n + 1; }
if (i === 2) r = r + 1;
if (j === 1) r = r + 2;
if (n === 2) r = r + 4;
r;"#,
        7.0,
    );
}

#[test]
fn other_expressions_comma_in_argument_list() {
    // Parenthesized comma expressions are ordinary arguments; each argument
    // evaluates left to right exactly once and the resulting values are used.
    assert_cookie(
        r#"var r=0;
var box={s:""};
function t(o,v){ o.s = o.s + v; return v; }
function g(a,b,c){ return a * 100 + b * 10 + c; }
if (g((t(box,"a"), 1), (t(box,"b"), 2), (t(box,"c"), 3)) === 123) r = r + 1;
if (box.s === "abc") r = r + 2;
r;"#,
        3.0,
    );
}

#[test]
fn other_expressions_conditional_selects_one_branch() {
    // Bits: true arm only; false arm only; branch callee reference selection
    // without calling either; test expression evaluated once.
    assert_cookie(
        r#"var r=0;
var box={n:0};
function f(o){ o.n = o.n + 1; return "f"; }
function g(o){ o.n = o.n + 10; return "g"; }
if ((1 ? f(box) : g(box)) === "f") r = r + 1;
if (box.n === 1) r = r + 2;
box.n = 0;
if ((0 ? f(box) : g(box)) === "g") r = r + 4;
if (box.n === 10) r = r + 8;
if ((1 ? f : g) === f) r = r + 16;
if (box.n === 10) r = r + 32;
var n2=0;
var box2={n:0};
function f2(o){ o.n = o.n + 1; return "f"; }
function g2(o){ o.n = o.n + 10; return "g"; }
if (((n2 = n2 + 1) ? f2(box2) : g2(box2)) === "f") r = r + 64;
if (n2 === 1) r = r + 128;
if (box2.n === 1) r = r + 256;
r;"#,
        511.0,
    );
}

#[test]
fn other_expressions_conditional_toboolean_and_nesting() {
    // Bits: empty string/null/undefined/0 false; 1 true; object and "0" string
    // true; array true; nested conditionals; branch value identity for string
    // and object arms.
    assert_cookie(
        r#"var r=0;
var e="";
var nil=null;
var u;
if ((e ? 1 : 0) === 0) r = r + 1;
if ((nil ? 1 : 0) === 0) r = r + 2;
if ((u ? 1 : 0) === 0) r = r + 4;
if ((0 ? 1 : 0) === 0) r = r + 8;
if ((1 ? 1 : 0) === 1) r = r + 16;
var o={};
var s="0";
if ((o ? 1 : 0) === 1) r = r + 32;
if ((s ? 1 : 0) === 1) r = r + 64;
var a=[];
if ((a ? 1 : 0) === 1) r = r + 128;
var s3="x";
var o3={};
if ((1 ? (0 ? 1 : 2) : 3) === 2) r = r + 256;
if ((0 ? 1 : (1 ? 2 : 3)) === 2) r = r + 512;
if ((1 ? s3 : "y") === "x") r = r + 1024;
if ((0 ? "y" : s3) === "x") r = r + 2048;
if ((1 ? o3 : null) === o3) r = r + 4096;
if ((0 ? null : o3) === o3) r = r + 8192;
r;"#,
        16383.0,
    );
}

#[test]
fn other_expressions_conditional_assignment_branches() {
    // Assignment is permitted as a branch value; only the selected assignment
    // runs.
    assert_cookie(
        r#"var r=0;
var x=0;
var c=0;
if ((c ? (x = 1) : (x = 2)) === 2) r = r + 1;
if (x === 2) r = r + 2;
c = 1;
if ((c ? (x = 3) : (x = 4)) === 3) r = r + 4;
if (x === 3) r = r + 8;
r;"#,
        15.0,
    );
}
