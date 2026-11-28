//! `String.prototype.search` in the one admitted `/a/` RegExp domain.
//!
//! The frontend admits only the `/a/` literal value, whose private brand is
//! the node kind its allocator writes, so this module pins that limited
//! domain: the first match offset for `/a/`, `-1` when there is no match, and
//! the managed TypeError for every argument that is not the admitted RegExp
//! value (including a String, a Number, a missing argument, an ordinary
//! object, an Array, a TypedArray view, an ArrayBuffer, the `/a/g` literal
//! and a null receiver). The brand survives every public own-row mutation:
//! deleting or overwriting `test`, and adding further rows, all still match,
//! while a copy of the `test` row on an ordinary object does not. The
//! upstream MicroQuickJS differential is recorded in the task receipts;
//! `/a/g` is an explicit, documented divergence because this frontend's
//! `/a/g` value carries no RegExp brand.
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

/// `"abc".search(/a/)` is `0`, `"ba".search(/a/)` is `1`, and a subject without
/// the admitted `/a/` character is `-1`. The value is the ES5 number offset,
/// never a Boolean, and the public length is 1.
#[test]
fn micro_string_search_returns_first_match_offsets() {
    let source = concat!(
        "var a=\"abc\".search(/a/);",
        "var b=\"ba\".search(/a/);",
        "var c=\"xyz\".search(/a/);",
        "var d=\"\".search(/a/);",
        "var e=\"aXa\".search(/a/);",
        "var r=/a/;",
        "var f=\"abc\".search(r);",
        "var g=\"abc\".search.length;",
        "var h=typeof \"abc\".search;",
        "a===0&&b===1&&c===-1&&d===-1&&e===0&&f===0&&g===1&&h===\"function\";"
    );
    assert_boolean(source, true);
}

/// The offset is a UTF-16 index: the astral `😀` occupies two units, and a
/// lone surrogate occupies one, exactly like the upstream kernel.
#[test]
fn micro_string_search_offsets_are_utf16_units() {
    let source = concat!(
        "var a=\"😀a\".search(/a/);",
        "var b=\"\\uD800a\".search(/a/);",
        "var c=\"a😀\".search(/a/);",
        "a===2&&b===1&&c===0;"
    );
    assert_boolean(source, true);
}

/// Every argument that is not the admitted `/a/` RegExp value raises the
/// managed TypeError: a String, a Number, a missing argument, `undefined`, an
/// ordinary object, and the `/a/g` literal (which this frontend's literal
/// lowering publishes without any RegExp brand). The borrowed function called
/// with a null receiver also raises: `Function.prototype.call`'s existing
/// one-argument gate forwards no argument, and the adapter's own nullish
/// receiver check is the same TypeError MicroQuickJS raises there.
#[test]
fn micro_string_search_non_regexp_arguments_raise_type_errors() {
    let source = concat!(
        "var t=0;",
        "try{\"abc\".search(\"b\");}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "try{\"abc\".search(1);}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "try{\"abc\".search();}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "try{\"abc\".search(undefined);}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "try{\"abc\".search({});}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "try{\"abc\".search(/a/g);}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "var f=\"abc\".search;",
        "try{f.call(null);}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "try{f.call(null,\"a\");}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "t===8;"
    );
    assert_boolean(source, true);
}

/// Arrays, TypedArray views and ArrayBuffer backings are OBJECT-tagged but not
/// the admitted literal. They must reach the managed TypeError through the
/// brand rejection instead of an internal kind assertion.
#[test]
fn micro_string_search_array_like_arguments_raise_type_errors() {
    let source = concat!(
        "var t=0;",
        "var view1=new Uint8Array(1);",
        "try{\"abc\".search([]);}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "try{\"abc\".search([\"a\"]);}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "try{\"abc\".search(view1);}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "try{\"abc\".search(new ArrayBuffer(1));}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "t===4;"
    );
    assert_boolean(source, true);
}

/// The brand is the private node kind the admitted literal's allocator
/// writes, not the copyable public `test` row. Deleting `test`, overwriting
/// it and adding further rows all keep the identity; a plain object that
/// copies `test` still raises the managed TypeError.
#[test]
fn micro_string_search_regexp_brand_survives_public_row_mutation() {
    let source = concat!(
        "var t=0;",
        "var r=/a/;",
        "delete r.test;",
        "if(\"abc\".search(r)===0)t=t+1;",
        "var q=/a/;",
        "q.test=5;",
        "if(\"ba\".search(q)===1)t=t+1;",
        "var s=/a/;",
        "s.extra=1;",
        "if(\"xyz\".search(s)===-1)t=t+1;",
        "var o={test:s.test};",
        "try{\"abc\".search(o);}catch(x){if(x.name===\"TypeError\")t=t+1;}",
        "t===4;"
    );
    assert_boolean(source, true);
}

/// A source that only spells `search` inside function bodies still installs
/// the gated prototype row and returns the real offsets.
#[test]
fn micro_string_search_function_body_gate_installs_row() {
    let source = concat!(
        "function f(s){return s.search(/a/);}",
        "var g=function(s){return s.search(/a/);};",
        "f(\"abc\")===0&&f(\"ba\")===1&&f(\"xyz\")===-1&&g(\"aXa\")===0;"
    );
    assert_boolean(source, true);
}

/// A source whose only spelling of `search` is a `new` construction argument
/// still installs the gated prototype row: the construction argument is an
/// ordinary expression, and the admitted `ArrayBuffer` byte length is the real
/// offset 1. Without the `new`-argument scan the call would reach an
/// uninstalled row and throw instead of producing the length.
#[test]
fn micro_string_search_new_argument_gate_installs_row() {
    let source = "new ArrayBuffer(\"ba\".search(/a/)).byteLength===1;";
    assert_boolean(source, true);
}

/// A source whose only spelling of `search` is the computed key of an
/// assignment target still installs the gated prototype row, so the public
/// method returns the real offset after the store. Any runtime observation of
/// the installed row has to spell the name again, so the row-installation half
/// of this position is pinned exactly by the paired budget witness in
/// `string_search_row_budget_and_gating`; the offset is pinned here.
#[test]
fn micro_string_search_assignment_computed_key_gate_installs_row() {
    let source = concat!(
        "var o={};",
        "o[\"search\"]=0;",
        "o[\"search\"]===0&&\"ba\".search(/a/)===1;"
    );
    assert_boolean(source, true);
}

/// A source whose only spelling of `search` is a function-body `Update` still
/// installs the gated prototype row; the update keeps its real numeric result
/// and the public method returns the real offset.
#[test]
fn micro_string_search_function_body_update_gate_installs_row() {
    let source = concat!(
        "function bump(o){o.search++;}",
        "var o={search:0};",
        "bump(o);",
        "o.search===1&&\"ba\".search(/a/)===1;"
    );
    assert_boolean(source, true);
}

/// Patterns other than the one admitted `/a/` literal stay outside the domain
/// at the existing admission boundary: `/b/` is not a literal this frontend
/// publishes at all, so it is refused before the realm is built. This is the
/// same refusal every non-`a` regexp already takes, not a `search`-specific
/// branch.
#[test]
fn micro_string_search_other_patterns_stay_outside_the_domain() {
    let source = "\"abc\".search(/b/);";
    match pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source) {
        Ok(_) => panic!("only the admitted `/a/` literal may reach String.prototype.search"),
        Err(error) => assert!(
            format!("{error:?}")
                .contains("unsupported JavaScript ES5 P0 primitive expression"),
            "expected the existing literal refusal, got {error:?}"
        ),
    }
}
