//! `String.prototype.match` in the one admitted `/a/` RegExp domain.
//!
//! The frontend admits only the `/a/` literal value, whose private brand is
//! the node kind its allocator writes, so this module pins that limited
//! domain: a real Array whose single element is the matched substring, `null`
//! when the subject does not contain the admitted character, and the managed
//! TypeError for every first argument that is not the admitted RegExp value
//! (including a String, a Number, a missing argument, `undefined`, `null`, an
//! ordinary object, an Array, a TypedArray view, an ArrayBuffer and the `/a/g`
//! literal). Upstream MicroQuickJS ignores extra arguments, and so does this
//! adapter. The brand survives every public own-row mutation: deleting or
//! overwriting `test`, and adding further rows, all still match, while a copy
//! of the `test` row on an ordinary object does not.
//!
//! The upstream MicroQuickJS `6d4d7eb6` differential is recorded in the task
//! receipts (`tmp/es51-abuf176/match-probe*.log`). One documented divergence:
//! upstream's result Array also carries the enumerable own data rows `index`
//! and `input`, while this realm's dense tagged Array has no named-row
//! directory at all (`reference.rs`: "the Array itself has no named own
//! rows"), so the match result is the real Array only. `micro_string_match_
//! result_has_no_named_rows` pins that documented shape instead of silently
//! drifting.
use vm::{Value, VmStatus};

fn compile(source: &str) -> vm::Vm {
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| panic!("production compilation refused; guest_invocations=0 source={source:?} error={error:?}"));
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..*machine.jit_config()
    });
    machine.set_fuel(10_000_000);
    machine
}

fn assert_boolean(source: &str, expected: bool) {
    let mut machine = compile(source);
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted, "source={source}");
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion source={source} value={value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(2)), "source={source}");
    assert_eq!(
        fields.get(&Value::string("flag")),
        Some(&Value::Bool(expected)),
        "source={source}"
    );
}

/// `"abc".match(/a/)` is the real Array `["a"]`: length one, element `"a"`,
/// `Array.isArray` true and `typeof` object, which is the upstream shape for
/// the array half of the result. (`JSON.stringify` on an Array is outside this
/// frontend's admitted `{"a":1}`-only JSON domain, a pre-existing limitation
/// that is not part of the match domain.) The public length is 1 and the row
/// is a real function.
#[test]
fn micro_string_match_returns_real_array() {
    let source = concat!(
        "var a=\"abc\".match(/a/);",
        "a.length===1&&a[0]===\"a\"&&Array.isArray(a)&&typeof a===\"object\"",
        "&&\"abc\".match.length===1&&typeof \"abc\".match===\"function\";"
    );
    assert_boolean(source, true);
}

/// A subject without the admitted character, and the empty String, are both
/// `null`; a subject with two occurrences reports the first match only.
#[test]
fn micro_string_match_null_and_first_match() {
    let source = concat!(
        "var a=\"xyz\".match(/a/);",
        "var b=\"\".match(/a/);",
        "var c=\"aXa\".match(/a/);",
        "a===null&&b===null&&c.length===1&&c[0]===\"a\";"
    );
    assert_boolean(source, true);
}

/// The admitted literal reached through a variable is the same brand, extra
/// arguments are ignored exactly like upstream `"abc".match(/a/, 7)`, and the
/// result length of an extra-argument call is still one.
#[test]
fn micro_string_match_regexp_variable_and_extra_arguments() {
    let source = concat!(
        "var r=/a/;",
        "var a=\"abc\".match(r);",
        "var b=\"abc\".match(/a/,7);",
        "a[0]===\"a\"&&b.length===1&&b[0]===\"a\";"
    );
    assert_boolean(source, true);
}

/// The matched element is a real managed String produced from the admitted
/// UTF-16 units: it concatenates and compares like any other String, and the
/// astral receiver still locates the admitted ASCII unit.
#[test]
fn micro_string_match_element_is_a_real_string() {
    let source = concat!(
        "var a=\"😀a\".match(/a/);",
        "var b=\"\\uD800a\".match(/a/);",
        "var c=\"aa\".match(/a/);",
        "a[0]===\"a\"&&typeof a[0]===\"string\"&&a[0]+\"!\"===\"a!\"",
        "&&b[0]===\"a\"&&c[0]===\"a\"&&c.length===1",
        "&&\"😀\".match(/a/)===null&&\"😀a😀\".match(/a/)[0]===\"a\";"
    );
    assert_boolean(source, true);
}

/// The result is a fresh Array per call and never aliases the receiver or the
/// literal: two calls produce distinct objects with independent elements.
#[test]
fn micro_string_match_result_is_fresh_per_call() {
    let source = concat!(
        "var r=/a/;",
        "var a=\"abc\".match(r);",
        "var b=\"abc\".match(r);",
        "a!==b&&a[0]===b[0]&&a.length===b.length;"
    );
    assert_boolean(source, true);
}

/// Documented divergence pinned: this realm's dense Array has no named-row
/// directory, so the admitted match result carries the element rows only.
/// Upstream also publishes enumerable own `index`/`input` rows; here
/// `Object.keys` lists exactly the index and both names read `undefined`.
#[test]
fn micro_string_match_result_has_no_named_rows() {
    let source = concat!(
        "var m=\"aXa\".match(/a/);",
        "Object.keys(m).join(\",\")===\"0\"",
        "&&m.hasOwnProperty(\"0\")",
        "&&!m.hasOwnProperty(\"index\")&&!m.hasOwnProperty(\"input\")",
        "&&m.index===undefined&&m.input===undefined;"
    );
    assert_boolean(source, true);
}

/// Every first argument that is not the admitted `/a/` RegExp value raises the
/// managed TypeError: a String, a Number, a missing argument, `undefined`,
/// `null`, an ordinary object and the `/a/g` literal (which this frontend's
/// literal lowering publishes without any RegExp brand). The borrowed function
/// called with a null receiver also raises, and an out-of-range call under
/// `call` keeps the same TypeError.
#[test]
fn micro_string_match_non_regexp_arguments_raise_type_errors() {
    let source = concat!(
        "var t=0;",
        "try{\"abc\".match(\"b\");}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "try{\"abc\".match(1);}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "try{\"abc\".match();}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "try{\"abc\".match(undefined);}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "try{\"abc\".match(null);}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "try{\"abc\".match({});}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "try{\"abc\".match(/a/g);}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "var f=\"abc\".match;",
        "try{f.call(null);}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "try{f.call(null,\"a\");}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "t===9;"
    );
    assert_boolean(source, true);
}

/// Arrays, TypedArray views and ArrayBuffer backings are OBJECT-tagged but not
/// the admitted literal. They must reach the managed TypeError through the
/// brand rejection instead of an internal kind assertion.
#[test]
fn micro_string_match_array_like_arguments_raise_type_errors() {
    let source = concat!(
        "var t=0;",
        "var view1=new Uint8Array(1);",
        "try{\"abc\".match([]);}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "try{\"abc\".match([\"a\"]);}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "try{\"abc\".match(view1);}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "try{\"abc\".match(new ArrayBuffer(1));}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "t===4;"
    );
    assert_boolean(source, true);
}

/// The brand is the private node kind the admitted literal's allocator
/// writes, not the copyable public `test` row. Deleting `test`, overwriting
/// it and adding further rows all keep the identity; a plain object that
/// copies `test` still raises the managed TypeError.
#[test]
fn micro_string_match_regexp_brand_survives_public_row_mutation() {
    let source = concat!(
        "var t=0;",
        "var r=/a/;",
        "delete r.test;",
        "if(\"abc\".match(r)[0]===\"a\")t=t+1;",
        "var q=/a/;",
        "q.test=5;",
        "if(\"ba\".match(q)[0]===\"a\")t=t+1;",
        "var s=/a/;",
        "s.extra=1;",
        "if(\"xyz\".match(s)===null)t=t+1;",
        "var o={test:s.test};",
        "try{\"abc\".match(o);}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "t===4;"
    );
    assert_boolean(source, true);
}

/// A source that only spells `match` inside function bodies still installs the
/// gated prototype row and returns the real result.
#[test]
fn micro_string_match_function_body_gate_installs_row() {
    let source = concat!(
        "function f(s){return s.match(/a/);}",
        "var g=function(s){return s.match(/a/);};",
        "f(\"abc\")[0]===\"a\"&&f(\"ba\")[0]===\"a\"&&f(\"xyz\")===null&&g(\"aXa\")[0]===\"a\";"
    );
    assert_boolean(source, true);
}

/// A source whose only spelling of `match` is a `new` construction argument
/// still installs the gated prototype row: the construction argument is an
/// ordinary expression, and the admitted `ArrayBuffer` byte length is the real
/// matched element length.
#[test]
fn micro_string_match_new_argument_gate_installs_row() {
    let source = "new ArrayBuffer(\"ba\".match(/a/)[0].length).byteLength===1;";
    assert_boolean(source, true);
}

/// A source whose only spelling of `match` is the computed key of an
/// assignment target still installs the gated prototype row, so the public
/// method returns the real result after the store. Any runtime observation of
/// the installed row has to spell the name again, so the row-installation half
/// of this position is pinned exactly by the paired budget witness in
/// `string_match_row_budget_and_gating`; the result is pinned here.
#[test]
fn micro_string_match_assignment_computed_key_gate_installs_row() {
    let source = concat!(
        "var o={};",
        "o[\"match\"]=0;",
        "o[\"match\"]===0&&\"ba\".match(/a/)[0]===\"a\";"
    );
    assert_boolean(source, true);
}

/// A source whose only spelling of `match` is a function-body `Update` still
/// installs the gated prototype row; the update keeps its real numeric result
/// and the public method returns the real array.
#[test]
fn micro_string_match_function_body_update_gate_installs_row() {
    let source = concat!(
        "function bump(o){o.match++;}",
        "var o={match:0};",
        "bump(o);",
        "o.match===1&&\"ba\".match(/a/)[0]===\"a\";"
    );
    assert_boolean(source, true);
}

/// Patterns other than the one admitted `/a/` literal stay outside the domain
/// at the existing admission boundary: `/b/` is not a literal this frontend
/// publishes at all, so it is refused before the realm is built. This is the
/// same refusal every non-`a` regexp already takes, not a `match`-specific
/// branch.
#[test]
fn micro_string_match_other_patterns_stay_outside_the_domain() {
    let source = "\"abc\".match(/b/);";
    match pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source) {
        Ok(_) => panic!("only the admitted `/a/` literal may reach String.prototype.match"),
        Err(error) => assert!(
            format!("{error:?}")
                .contains("unsupported JavaScript ES5 P0 primitive expression"),
            "expected the existing literal refusal, got {error:?}"
        ),
    }
}
