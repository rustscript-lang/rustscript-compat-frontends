//! Primitive computed-key consumers. ES5.1 11.2.1 / 11.8.7 / 11.13.1.
//! Modern Node RHS-before-nullish-Put is a reference difference, not parity.
use vm::{Value, VmStatus};

fn field(value: &Value, name: &str) -> Value {
    let Value::Map(map) = value else {
        panic!("boxed value: {value:?}")
    };
    map.get(&Value::string(name)).expect("boxed field").clone()
}

fn execute_public(source: &str) -> Value {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!("PKEY151_SOURCE {source}");
            let mut machine =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                    .unwrap_or_else(|error| {
                        panic!(
                            "production compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                        )
                    });
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            assert_eq!(machine.run().unwrap(), VmStatus::Halted);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            let stack = machine.stack().to_vec();
            eprintln!("PKEY151_PAYLOAD stack={stack:?} native=0 aot=0");
            let result = stack.last().expect("completion").clone();
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("PKEY151_RESET resource_count=0 guest_invocations=1");
            result
        })
        .unwrap()
        .join()
        .unwrap()
}

fn assert_box(result: &Value, tag: i64, number: f64, flag: bool) {
    assert_eq!(field(result, "tag"), Value::Int(tag));
    assert_eq!(field(result, "number"), Value::Float(number));
    assert_eq!(field(result, "flag"), Value::Bool(flag));
    assert_eq!(field(result, "ref_slot"), Value::Int(0));
    assert_eq!(field(result, "generation"), Value::Int(0));
}

fn public_number(source: &str, expected: f64) {
    assert_box(&execute_public(source), 3, expected, false);
}

/// Tag 4 on the completion, then strict equality with the expected BMP literal.
/// Not a length or boolean cookie. Modern Node is not the oracle.
fn assert_string_units(source: &str, expected: &[u16]) {
    let value = execute_public(source);
    assert_eq!(
        field(&value, "tag"),
        Value::Int(4),
        "string tag source={source} value={value:?}"
    );
    let literal = String::from_utf16(expected).expect("expected BMP units");
    let compared = format!("{}===\"{literal}\";", source.trim_end_matches(';'));
    public_true(&compared);
}

fn public_true(source: &str) {
    assert_box(&execute_public(source), 2, 0.0, true);
}

#[test]
fn primitive_key151_bool_read() {
    public_number("var key=true;var o={\"true\":7};o[key];", 7.0);
}

#[test]
fn primitive_key151_number_write() {
    pkey151_number_pair(
        r#"var key=1.5;var o={};var result=(o[key]=7);var stored=o["1.5"];result+stored*1000"#,
        7.0,
        7.0,
    );
}

#[test]
fn primitive_key151_string_read() {
    public_number("var key=\"a\";var o={\"a\":4};o[key];", 4.0);
}

#[test]
fn primitive_key151_null_read() {
    public_number("var key=null;var o={\"null\":3};o[key];", 3.0);
}

#[test]
fn primitive_key151_string_write() {
    public_number("var key=\"a\";var o={};o[key]=9;o[\"a\"];", 9.0);
}

#[test]
fn primitive_key151_nullable_literal_put_order() {
    // ES5 evaluates the key, then CheckObjectCoercible, before RHS.
    // A passing existing guarantee would already keep n at 0.
    public_true(
        "var n=0;var ok=false;try{null.x=(n=1);}catch(e){ok=e.name===\"TypeError\";}ok&&n===0;",
    );
}

#[test]
fn primitive_key151_nullable_put_rhs_observed() {
    public_number("var n=0;try{null.x=(n=1);}catch(e){}n;", 0.0);
}

#[test]
fn primitive_key151_nullable_put_error_name() {
    public_true("var ok=false;try{null.x=1;}catch(e){ok=e.name===\"TypeError\";}ok;");
}

#[test]
fn primitive_key151_probe_call_this() {
    pkey151_number_pair(
        r#"'use strict';var key=true;var o={"true":function(){"use strict";return this.mark;},mark:5};var result=o[key]();var stored=o.mark;result+stored*1000"#,
        5.0,
        5.0,
    );
}

#[test]
fn primitive_key151_probe_delete() {
    pkey151_bool(
        r#"var key=true;var o={"true":7};var result=delete o[key];var stored=o["true"];if(!(stored===void 0)){throw stored;}result"#,
    );
}

#[test]
fn primitive_key151_probe_in() {
    pkey151_bool(
        r#"var key=true;var o={"true":1};var result=key in o;var stored=o["true"];if(!(stored===1)){throw stored;}result"#,
    );
}

#[test]
fn primitive_key151_probe_compound() {
    pkey151_number_pair(
        r#"var key=true;var o={"true":7};var result=(o[key]+=1);var stored=o["true"];result+stored*1000"#,
        8.0,
        8.0,
    );
}

#[test]
fn primitive_key151_probe_update() {
    pkey151_number_pair(
        r#"var key=true;var o={"true":7};var result=++o[key];var stored=o["true"];result+stored*1000"#,
        8.0,
        8.0,
    );
}

#[test]
fn primitive_key151_probe_literal_number_get() {
    public_number("var o={\"1.5\":7};o[1.5];", 7.0);
}

#[test]
fn primitive_key151_probe_dynamic_bool_get() {
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id(true);if(t)key=id(1);var o={"true":7};var result=o[key];var stored=o["true"];result+stored*1000"#,
        7.0,
        7.0,
    );
}

#[test]
fn primitive_key151_probe_strict_bool_get() {
    public_number("'use strict';var key=true;var o={\"true\":7};o[key];", 7.0);
}

fn pkey151_number_pair(source: &str, result: f64, store: f64) {
    let boxed = execute_public(source);
    assert_eq!(field(&boxed, "tag"), Value::Int(3), "{source}");
    assert_eq!(field(&boxed, "flag"), Value::Bool(false), "{source}");
    let Value::Float(observed) = field(&boxed, "number") else {
        panic!("number payload {source}");
    };
    let store_part = (observed / 1000.0).trunc();
    let result_part = observed - store_part * 1000.0;
    assert_eq!(result_part, result, "result {source}");
    assert_eq!(store_part, store, "literal store {source}");
}

fn pkey151_bool(source: &str) {
    let boxed = execute_public(source);
    assert_eq!(field(&boxed, "tag"), Value::Int(2), "{source}");
    assert_eq!(field(&boxed, "flag"), Value::Bool(true), "{source}");
    assert_eq!(field(&boxed, "number"), Value::Float(0.0), "{source}");
}

#[test]
fn pkey151_batch00() {
    // rows 0..7
    // 000 get Number proved sloppy
    pkey151_number_pair(
        r#"var key=1.5;var o={"1.5":7};var result=o[key];var stored=o["1.5"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 001 get Number proved strict
    pkey151_number_pair(
        r#"'use strict';var key=1.5;var o={"1.5":7};var result=o[key];var stored=o["1.5"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 002 get Number Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id(1.5);if(t)key=id("no");var o={"1.5":7};var result=o[key];var stored=o["1.5"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 003 get Number Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(1.5);if(t)key=id("no");var o={"1.5":7};var result=o[key];var stored=o["1.5"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 004 get String proved sloppy
    pkey151_number_pair(
        r#"var key="a";var o={"a":7};var result=o[key];var stored=o["a"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 005 get String proved strict
    pkey151_number_pair(
        r#"'use strict';var key="a";var o={"a":7};var result=o[key];var stored=o["a"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 006 get String Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id("a");if(t)key=id(1);var o={"a":7};var result=o[key];var stored=o["a"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 007 get String Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id("a");if(t)key=id(1);var o={"a":7};var result=o[key];var stored=o["a"];result+stored*1000"#,
        7.0,
        7.0,
    );
}

#[test]
fn pkey151_batch01() {
    // rows 8..15
    // 008 get Boolean proved sloppy
    pkey151_number_pair(
        r#"var key=true;var o={"true":7};var result=o[key];var stored=o["true"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 009 get Boolean proved strict
    pkey151_number_pair(
        r#"'use strict';var key=true;var o={"true":7};var result=o[key];var stored=o["true"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 010 get Boolean Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id(true);if(t)key=id(1);var o={"true":7};var result=o[key];var stored=o["true"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 011 get Boolean Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(true);if(t)key=id(1);var o={"true":7};var result=o[key];var stored=o["true"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 012 get Null proved sloppy
    pkey151_number_pair(
        r#"var key=null;var o={"null":7};var result=o[key];var stored=o["null"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 013 get Null proved strict
    pkey151_number_pair(
        r#"'use strict';var key=null;var o={"null":7};var result=o[key];var stored=o["null"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 014 get Null Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id(null);if(t)key=id(1);var o={"null":7};var result=o[key];var stored=o["null"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 015 get Null Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(null);if(t)key=id(1);var o={"null":7};var result=o[key];var stored=o["null"];result+stored*1000"#,
        7.0,
        7.0,
    );
}

#[test]
fn pkey151_batch02() {
    // rows 16..23
    // 016 get Undefined proved sloppy
    pkey151_number_pair(
        r#"var key=void 0;var o={"undefined":7};var result=o[key];var stored=o["undefined"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 017 get Undefined proved strict
    pkey151_number_pair(
        r#"'use strict';var key=void 0;var o={"undefined":7};var result=o[key];var stored=o["undefined"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 018 get Undefined Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id(void 0);if(t)key=id(1);var o={"undefined":7};var result=o[key];var stored=o["undefined"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 019 get Undefined Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(void 0);if(t)key=id(1);var o={"undefined":7};var result=o[key];var stored=o["undefined"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 020 put Number proved sloppy
    pkey151_number_pair(
        r#"var key=1.5;var o={};var result=(o[key]=7);var stored=o["1.5"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 021 put Number proved strict
    pkey151_number_pair(
        r#"'use strict';var key=1.5;var o={};var result=(o[key]=7);var stored=o["1.5"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 022 put Number Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id(1.5);if(t)key=id("no");var o={};var result=(o[key]=7);var stored=o["1.5"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 023 put Number Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(1.5);if(t)key=id("no");var o={};var result=(o[key]=7);var stored=o["1.5"];result+stored*1000"#,
        7.0,
        7.0,
    );
}

#[test]
fn pkey151_batch03() {
    // rows 24..31
    // 024 put String proved sloppy
    pkey151_number_pair(
        r#"var key="a";var o={};var result=(o[key]=7);var stored=o["a"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 025 put String proved strict
    pkey151_number_pair(
        r#"'use strict';var key="a";var o={};var result=(o[key]=7);var stored=o["a"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 026 put String Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id("a");if(t)key=id(1);var o={};var result=(o[key]=7);var stored=o["a"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 027 put String Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id("a");if(t)key=id(1);var o={};var result=(o[key]=7);var stored=o["a"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 028 put Boolean proved sloppy
    pkey151_number_pair(
        r#"var key=true;var o={};var result=(o[key]=7);var stored=o["true"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 029 put Boolean proved strict
    pkey151_number_pair(
        r#"'use strict';var key=true;var o={};var result=(o[key]=7);var stored=o["true"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 030 put Boolean Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id(true);if(t)key=id(1);var o={};var result=(o[key]=7);var stored=o["true"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 031 put Boolean Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(true);if(t)key=id(1);var o={};var result=(o[key]=7);var stored=o["true"];result+stored*1000"#,
        7.0,
        7.0,
    );
}

#[test]
fn pkey151_batch04() {
    // rows 32..39
    // 032 put Null proved sloppy
    pkey151_number_pair(
        r#"var key=null;var o={};var result=(o[key]=7);var stored=o["null"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 033 put Null proved strict
    pkey151_number_pair(
        r#"'use strict';var key=null;var o={};var result=(o[key]=7);var stored=o["null"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 034 put Null Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id(null);if(t)key=id(1);var o={};var result=(o[key]=7);var stored=o["null"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 035 put Null Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(null);if(t)key=id(1);var o={};var result=(o[key]=7);var stored=o["null"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 036 put Undefined proved sloppy
    pkey151_number_pair(
        r#"var key=void 0;var o={};var result=(o[key]=7);var stored=o["undefined"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 037 put Undefined proved strict
    pkey151_number_pair(
        r#"'use strict';var key=void 0;var o={};var result=(o[key]=7);var stored=o["undefined"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 038 put Undefined Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id(void 0);if(t)key=id(1);var o={};var result=(o[key]=7);var stored=o["undefined"];result+stored*1000"#,
        7.0,
        7.0,
    );
    // 039 put Undefined Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(void 0);if(t)key=id(1);var o={};var result=(o[key]=7);var stored=o["undefined"];result+stored*1000"#,
        7.0,
        7.0,
    );
}

#[test]
fn pkey151_batch05() {
    // rows 40..47
    // 040 call Number proved sloppy
    pkey151_number_pair(
        r#"var key=1.5;var o={"1.5":function(){"use strict";return this.mark;},mark:5};var result=o[key]();var stored=o.mark;result+stored*1000"#,
        5.0,
        5.0,
    );
    // 041 call Number proved strict
    pkey151_number_pair(
        r#"'use strict';var key=1.5;var o={"1.5":function(){"use strict";return this.mark;},mark:5};var result=o[key]();var stored=o.mark;result+stored*1000"#,
        5.0,
        5.0,
    );
    // 042 call Number Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id(1.5);if(t)key=id("no");var o={"1.5":function(){"use strict";return this.mark;},mark:5};var result=o[key]();var stored=o.mark;result+stored*1000"#,
        5.0,
        5.0,
    );
    // 043 call Number Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(1.5);if(t)key=id("no");var o={"1.5":function(){"use strict";return this.mark;},mark:5};var result=o[key]();var stored=o.mark;result+stored*1000"#,
        5.0,
        5.0,
    );
    // 044 call String proved sloppy
    pkey151_number_pair(
        r#"var key="a";var o={"a":function(){"use strict";return this.mark;},mark:5};var result=o[key]();var stored=o.mark;result+stored*1000"#,
        5.0,
        5.0,
    );
    // 045 call String proved strict
    pkey151_number_pair(
        r#"'use strict';var key="a";var o={"a":function(){"use strict";return this.mark;},mark:5};var result=o[key]();var stored=o.mark;result+stored*1000"#,
        5.0,
        5.0,
    );
    // 046 call String Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id("a");if(t)key=id(1);var o={"a":function(){"use strict";return this.mark;},mark:5};var result=o[key]();var stored=o.mark;result+stored*1000"#,
        5.0,
        5.0,
    );
    // 047 call String Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id("a");if(t)key=id(1);var o={"a":function(){"use strict";return this.mark;},mark:5};var result=o[key]();var stored=o.mark;result+stored*1000"#,
        5.0,
        5.0,
    );
}

#[test]
fn pkey151_batch06() {
    // rows 48..55
    // 048 call Boolean proved sloppy
    pkey151_number_pair(
        r#"var key=true;var o={"true":function(){"use strict";return this.mark;},mark:5};var result=o[key]();var stored=o.mark;result+stored*1000"#,
        5.0,
        5.0,
    );
    // 049 call Boolean proved strict
    pkey151_number_pair(
        r#"'use strict';var key=true;var o={"true":function(){"use strict";return this.mark;},mark:5};var result=o[key]();var stored=o.mark;result+stored*1000"#,
        5.0,
        5.0,
    );
    // 050 call Boolean Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id(true);if(t)key=id(1);var o={"true":function(){"use strict";return this.mark;},mark:5};var result=o[key]();var stored=o.mark;result+stored*1000"#,
        5.0,
        5.0,
    );
    // 051 call Boolean Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(true);if(t)key=id(1);var o={"true":function(){"use strict";return this.mark;},mark:5};var result=o[key]();var stored=o.mark;result+stored*1000"#,
        5.0,
        5.0,
    );
    // 052 call Null proved sloppy
    pkey151_number_pair(
        r#"var key=null;var o={"null":function(){"use strict";return this.mark;},mark:5};var result=o[key]();var stored=o.mark;result+stored*1000"#,
        5.0,
        5.0,
    );
    // 053 call Null proved strict
    pkey151_number_pair(
        r#"'use strict';var key=null;var o={"null":function(){"use strict";return this.mark;},mark:5};var result=o[key]();var stored=o.mark;result+stored*1000"#,
        5.0,
        5.0,
    );
    // 054 call Null Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id(null);if(t)key=id(1);var o={"null":function(){"use strict";return this.mark;},mark:5};var result=o[key]();var stored=o.mark;result+stored*1000"#,
        5.0,
        5.0,
    );
    // 055 call Null Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(null);if(t)key=id(1);var o={"null":function(){"use strict";return this.mark;},mark:5};var result=o[key]();var stored=o.mark;result+stored*1000"#,
        5.0,
        5.0,
    );
}

#[test]
fn pkey151_batch07() {
    // rows 56..63
    // 056 call Undefined proved sloppy
    pkey151_number_pair(
        r#"var key=void 0;var o={"undefined":function(){"use strict";return this.mark;},mark:5};var result=o[key]();var stored=o.mark;result+stored*1000"#,
        5.0,
        5.0,
    );
    // 057 call Undefined proved strict
    pkey151_number_pair(
        r#"'use strict';var key=void 0;var o={"undefined":function(){"use strict";return this.mark;},mark:5};var result=o[key]();var stored=o.mark;result+stored*1000"#,
        5.0,
        5.0,
    );
    // 058 call Undefined Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id(void 0);if(t)key=id(1);var o={"undefined":function(){"use strict";return this.mark;},mark:5};var result=o[key]();var stored=o.mark;result+stored*1000"#,
        5.0,
        5.0,
    );
    // 059 call Undefined Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(void 0);if(t)key=id(1);var o={"undefined":function(){"use strict";return this.mark;},mark:5};var result=o[key]();var stored=o.mark;result+stored*1000"#,
        5.0,
        5.0,
    );
    // 060 delete Number proved sloppy
    pkey151_bool(
        r#"var key=1.5;var o={"1.5":7};var result=delete o[key];var stored=o["1.5"];if(!(stored===void 0)){throw stored;}result"#,
    );
    // 061 delete Number proved strict
    pkey151_bool(
        r#"'use strict';var key=1.5;var o={"1.5":7};var result=delete o[key];var stored=o["1.5"];if(!(stored===void 0)){throw stored;}result"#,
    );
    // 062 delete Number Dynamic sloppy
    pkey151_bool(
        r#"function id(x){return x;}var t=0;var key=id(1.5);if(t)key=id("no");var o={"1.5":7};var result=delete o[key];var stored=o["1.5"];if(!(stored===void 0)){throw stored;}result"#,
    );
    // 063 delete Number Dynamic strict
    pkey151_bool(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(1.5);if(t)key=id("no");var o={"1.5":7};var result=delete o[key];var stored=o["1.5"];if(!(stored===void 0)){throw stored;}result"#,
    );
}

#[test]
fn pkey151_batch08() {
    // rows 64..71
    // 064 delete String proved sloppy
    pkey151_bool(
        r#"var key="a";var o={"a":7};var result=delete o[key];var stored=o["a"];if(!(stored===void 0)){throw stored;}result"#,
    );
    // 065 delete String proved strict
    pkey151_bool(
        r#"'use strict';var key="a";var o={"a":7};var result=delete o[key];var stored=o["a"];if(!(stored===void 0)){throw stored;}result"#,
    );
    // 066 delete String Dynamic sloppy
    pkey151_bool(
        r#"function id(x){return x;}var t=0;var key=id("a");if(t)key=id(1);var o={"a":7};var result=delete o[key];var stored=o["a"];if(!(stored===void 0)){throw stored;}result"#,
    );
    // 067 delete String Dynamic strict
    pkey151_bool(
        r#"'use strict';function id(x){return x;}var t=0;var key=id("a");if(t)key=id(1);var o={"a":7};var result=delete o[key];var stored=o["a"];if(!(stored===void 0)){throw stored;}result"#,
    );
    // 068 delete Boolean proved sloppy
    pkey151_bool(
        r#"var key=true;var o={"true":7};var result=delete o[key];var stored=o["true"];if(!(stored===void 0)){throw stored;}result"#,
    );
    // 069 delete Boolean proved strict
    pkey151_bool(
        r#"'use strict';var key=true;var o={"true":7};var result=delete o[key];var stored=o["true"];if(!(stored===void 0)){throw stored;}result"#,
    );
    // 070 delete Boolean Dynamic sloppy
    pkey151_bool(
        r#"function id(x){return x;}var t=0;var key=id(true);if(t)key=id(1);var o={"true":7};var result=delete o[key];var stored=o["true"];if(!(stored===void 0)){throw stored;}result"#,
    );
    // 071 delete Boolean Dynamic strict
    pkey151_bool(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(true);if(t)key=id(1);var o={"true":7};var result=delete o[key];var stored=o["true"];if(!(stored===void 0)){throw stored;}result"#,
    );
}

#[test]
fn pkey151_batch09() {
    // rows 72..79
    // 072 delete Null proved sloppy
    pkey151_bool(
        r#"var key=null;var o={"null":7};var result=delete o[key];var stored=o["null"];if(!(stored===void 0)){throw stored;}result"#,
    );
    // 073 delete Null proved strict
    pkey151_bool(
        r#"'use strict';var key=null;var o={"null":7};var result=delete o[key];var stored=o["null"];if(!(stored===void 0)){throw stored;}result"#,
    );
    // 074 delete Null Dynamic sloppy
    pkey151_bool(
        r#"function id(x){return x;}var t=0;var key=id(null);if(t)key=id(1);var o={"null":7};var result=delete o[key];var stored=o["null"];if(!(stored===void 0)){throw stored;}result"#,
    );
    // 075 delete Null Dynamic strict
    pkey151_bool(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(null);if(t)key=id(1);var o={"null":7};var result=delete o[key];var stored=o["null"];if(!(stored===void 0)){throw stored;}result"#,
    );
    // 076 delete Undefined proved sloppy
    pkey151_bool(
        r#"var key=void 0;var o={"undefined":7};var result=delete o[key];var stored=o["undefined"];if(!(stored===void 0)){throw stored;}result"#,
    );
    // 077 delete Undefined proved strict
    pkey151_bool(
        r#"'use strict';var key=void 0;var o={"undefined":7};var result=delete o[key];var stored=o["undefined"];if(!(stored===void 0)){throw stored;}result"#,
    );
    // 078 delete Undefined Dynamic sloppy
    pkey151_bool(
        r#"function id(x){return x;}var t=0;var key=id(void 0);if(t)key=id(1);var o={"undefined":7};var result=delete o[key];var stored=o["undefined"];if(!(stored===void 0)){throw stored;}result"#,
    );
    // 079 delete Undefined Dynamic strict
    pkey151_bool(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(void 0);if(t)key=id(1);var o={"undefined":7};var result=delete o[key];var stored=o["undefined"];if(!(stored===void 0)){throw stored;}result"#,
    );
}

#[test]
fn pkey151_batch10() {
    // rows 80..87
    // 080 in Number proved sloppy
    pkey151_bool(
        r#"var key=1.5;var o={"1.5":1};var result=key in o;var stored=o["1.5"];if(!(stored===1)){throw stored;}result"#,
    );
    // 081 in Number proved strict
    pkey151_bool(
        r#"'use strict';var key=1.5;var o={"1.5":1};var result=key in o;var stored=o["1.5"];if(!(stored===1)){throw stored;}result"#,
    );
    // 082 in Number Dynamic sloppy
    pkey151_bool(
        r#"function id(x){return x;}var t=0;var key=id(1.5);if(t)key=id("no");var o={"1.5":1};var result=key in o;var stored=o["1.5"];if(!(stored===1)){throw stored;}result"#,
    );
    // 083 in Number Dynamic strict
    pkey151_bool(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(1.5);if(t)key=id("no");var o={"1.5":1};var result=key in o;var stored=o["1.5"];if(!(stored===1)){throw stored;}result"#,
    );
    // 084 in String proved sloppy
    pkey151_bool(
        r#"var key="a";var o={"a":1};var result=key in o;var stored=o["a"];if(!(stored===1)){throw stored;}result"#,
    );
    // 085 in String proved strict
    pkey151_bool(
        r#"'use strict';var key="a";var o={"a":1};var result=key in o;var stored=o["a"];if(!(stored===1)){throw stored;}result"#,
    );
    // 086 in String Dynamic sloppy
    pkey151_bool(
        r#"function id(x){return x;}var t=0;var key=id("a");if(t)key=id(1);var o={"a":1};var result=key in o;var stored=o["a"];if(!(stored===1)){throw stored;}result"#,
    );
    // 087 in String Dynamic strict
    pkey151_bool(
        r#"'use strict';function id(x){return x;}var t=0;var key=id("a");if(t)key=id(1);var o={"a":1};var result=key in o;var stored=o["a"];if(!(stored===1)){throw stored;}result"#,
    );
}

#[test]
fn pkey151_batch11() {
    // rows 88..95
    // 088 in Boolean proved sloppy
    pkey151_bool(
        r#"var key=true;var o={"true":1};var result=key in o;var stored=o["true"];if(!(stored===1)){throw stored;}result"#,
    );
    // 089 in Boolean proved strict
    pkey151_bool(
        r#"'use strict';var key=true;var o={"true":1};var result=key in o;var stored=o["true"];if(!(stored===1)){throw stored;}result"#,
    );
    // 090 in Boolean Dynamic sloppy
    pkey151_bool(
        r#"function id(x){return x;}var t=0;var key=id(true);if(t)key=id(1);var o={"true":1};var result=key in o;var stored=o["true"];if(!(stored===1)){throw stored;}result"#,
    );
    // 091 in Boolean Dynamic strict
    pkey151_bool(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(true);if(t)key=id(1);var o={"true":1};var result=key in o;var stored=o["true"];if(!(stored===1)){throw stored;}result"#,
    );
    // 092 in Null proved sloppy
    pkey151_bool(
        r#"var key=null;var o={"null":1};var result=key in o;var stored=o["null"];if(!(stored===1)){throw stored;}result"#,
    );
    // 093 in Null proved strict
    pkey151_bool(
        r#"'use strict';var key=null;var o={"null":1};var result=key in o;var stored=o["null"];if(!(stored===1)){throw stored;}result"#,
    );
    // 094 in Null Dynamic sloppy
    pkey151_bool(
        r#"function id(x){return x;}var t=0;var key=id(null);if(t)key=id(1);var o={"null":1};var result=key in o;var stored=o["null"];if(!(stored===1)){throw stored;}result"#,
    );
    // 095 in Null Dynamic strict
    pkey151_bool(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(null);if(t)key=id(1);var o={"null":1};var result=key in o;var stored=o["null"];if(!(stored===1)){throw stored;}result"#,
    );
}

#[test]
fn pkey151_batch12() {
    // rows 96..103
    // 096 in Undefined proved sloppy
    pkey151_bool(
        r#"var key=void 0;var o={"undefined":1};var result=key in o;var stored=o["undefined"];if(!(stored===1)){throw stored;}result"#,
    );
    // 097 in Undefined proved strict
    pkey151_bool(
        r#"'use strict';var key=void 0;var o={"undefined":1};var result=key in o;var stored=o["undefined"];if(!(stored===1)){throw stored;}result"#,
    );
    // 098 in Undefined Dynamic sloppy
    pkey151_bool(
        r#"function id(x){return x;}var t=0;var key=id(void 0);if(t)key=id(1);var o={"undefined":1};var result=key in o;var stored=o["undefined"];if(!(stored===1)){throw stored;}result"#,
    );
    // 099 in Undefined Dynamic strict
    pkey151_bool(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(void 0);if(t)key=id(1);var o={"undefined":1};var result=key in o;var stored=o["undefined"];if(!(stored===1)){throw stored;}result"#,
    );
    // 100 compound Number proved sloppy
    pkey151_number_pair(
        r#"var key=1.5;var o={"1.5":7};var result=(o[key]+=1);var stored=o["1.5"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 101 compound Number proved strict
    pkey151_number_pair(
        r#"'use strict';var key=1.5;var o={"1.5":7};var result=(o[key]+=1);var stored=o["1.5"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 102 compound Number Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id(1.5);if(t)key=id("no");var o={"1.5":7};var result=(o[key]+=1);var stored=o["1.5"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 103 compound Number Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(1.5);if(t)key=id("no");var o={"1.5":7};var result=(o[key]+=1);var stored=o["1.5"];result+stored*1000"#,
        8.0,
        8.0,
    );
}

#[test]
fn pkey151_batch13() {
    // rows 104..111
    // 104 compound String proved sloppy
    pkey151_number_pair(
        r#"var key="a";var o={"a":7};var result=(o[key]+=1);var stored=o["a"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 105 compound String proved strict
    pkey151_number_pair(
        r#"'use strict';var key="a";var o={"a":7};var result=(o[key]+=1);var stored=o["a"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 106 compound String Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id("a");if(t)key=id(1);var o={"a":7};var result=(o[key]+=1);var stored=o["a"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 107 compound String Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id("a");if(t)key=id(1);var o={"a":7};var result=(o[key]+=1);var stored=o["a"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 108 compound Boolean proved sloppy
    pkey151_number_pair(
        r#"var key=true;var o={"true":7};var result=(o[key]+=1);var stored=o["true"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 109 compound Boolean proved strict
    pkey151_number_pair(
        r#"'use strict';var key=true;var o={"true":7};var result=(o[key]+=1);var stored=o["true"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 110 compound Boolean Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id(true);if(t)key=id(1);var o={"true":7};var result=(o[key]+=1);var stored=o["true"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 111 compound Boolean Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(true);if(t)key=id(1);var o={"true":7};var result=(o[key]+=1);var stored=o["true"];result+stored*1000"#,
        8.0,
        8.0,
    );
}

#[test]
fn pkey151_batch14() {
    // rows 112..119
    // 112 compound Null proved sloppy
    pkey151_number_pair(
        r#"var key=null;var o={"null":7};var result=(o[key]+=1);var stored=o["null"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 113 compound Null proved strict
    pkey151_number_pair(
        r#"'use strict';var key=null;var o={"null":7};var result=(o[key]+=1);var stored=o["null"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 114 compound Null Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id(null);if(t)key=id(1);var o={"null":7};var result=(o[key]+=1);var stored=o["null"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 115 compound Null Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(null);if(t)key=id(1);var o={"null":7};var result=(o[key]+=1);var stored=o["null"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 116 compound Undefined proved sloppy
    pkey151_number_pair(
        r#"var key=void 0;var o={"undefined":7};var result=(o[key]+=1);var stored=o["undefined"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 117 compound Undefined proved strict
    pkey151_number_pair(
        r#"'use strict';var key=void 0;var o={"undefined":7};var result=(o[key]+=1);var stored=o["undefined"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 118 compound Undefined Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id(void 0);if(t)key=id(1);var o={"undefined":7};var result=(o[key]+=1);var stored=o["undefined"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 119 compound Undefined Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(void 0);if(t)key=id(1);var o={"undefined":7};var result=(o[key]+=1);var stored=o["undefined"];result+stored*1000"#,
        8.0,
        8.0,
    );
}

#[test]
fn pkey151_batch15() {
    // rows 120..127
    // 120 update Number proved sloppy
    pkey151_number_pair(
        r#"var key=1.5;var o={"1.5":7};var result=++o[key];var stored=o["1.5"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 121 update Number proved strict
    pkey151_number_pair(
        r#"'use strict';var key=1.5;var o={"1.5":7};var result=++o[key];var stored=o["1.5"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 122 update Number Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id(1.5);if(t)key=id("no");var o={"1.5":7};var result=++o[key];var stored=o["1.5"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 123 update Number Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(1.5);if(t)key=id("no");var o={"1.5":7};var result=++o[key];var stored=o["1.5"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 124 update String proved sloppy
    pkey151_number_pair(
        r#"var key="a";var o={"a":7};var result=++o[key];var stored=o["a"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 125 update String proved strict
    pkey151_number_pair(
        r#"'use strict';var key="a";var o={"a":7};var result=++o[key];var stored=o["a"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 126 update String Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id("a");if(t)key=id(1);var o={"a":7};var result=++o[key];var stored=o["a"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 127 update String Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id("a");if(t)key=id(1);var o={"a":7};var result=++o[key];var stored=o["a"];result+stored*1000"#,
        8.0,
        8.0,
    );
}

#[test]
fn pkey151_batch16() {
    // rows 128..135
    // 128 update Boolean proved sloppy
    pkey151_number_pair(
        r#"var key=true;var o={"true":7};var result=++o[key];var stored=o["true"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 129 update Boolean proved strict
    pkey151_number_pair(
        r#"'use strict';var key=true;var o={"true":7};var result=++o[key];var stored=o["true"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 130 update Boolean Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id(true);if(t)key=id(1);var o={"true":7};var result=++o[key];var stored=o["true"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 131 update Boolean Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(true);if(t)key=id(1);var o={"true":7};var result=++o[key];var stored=o["true"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 132 update Null proved sloppy
    pkey151_number_pair(
        r#"var key=null;var o={"null":7};var result=++o[key];var stored=o["null"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 133 update Null proved strict
    pkey151_number_pair(
        r#"'use strict';var key=null;var o={"null":7};var result=++o[key];var stored=o["null"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 134 update Null Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id(null);if(t)key=id(1);var o={"null":7};var result=++o[key];var stored=o["null"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 135 update Null Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(null);if(t)key=id(1);var o={"null":7};var result=++o[key];var stored=o["null"];result+stored*1000"#,
        8.0,
        8.0,
    );
}

#[test]
fn pkey151_batch17() {
    // rows 136..139
    // 136 update Undefined proved sloppy
    pkey151_number_pair(
        r#"var key=void 0;var o={"undefined":7};var result=++o[key];var stored=o["undefined"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 137 update Undefined proved strict
    pkey151_number_pair(
        r#"'use strict';var key=void 0;var o={"undefined":7};var result=++o[key];var stored=o["undefined"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 138 update Undefined Dynamic sloppy
    pkey151_number_pair(
        r#"function id(x){return x;}var t=0;var key=id(void 0);if(t)key=id(1);var o={"undefined":7};var result=++o[key];var stored=o["undefined"];result+stored*1000"#,
        8.0,
        8.0,
    );
    // 139 update Undefined Dynamic strict
    pkey151_number_pair(
        r#"'use strict';function id(x){return x;}var t=0;var key=id(void 0);if(t)key=id(1);var o={"undefined":7};var result=++o[key];var stored=o["undefined"];result+stored*1000"#,
        8.0,
        8.0,
    );
}

#[test]
fn pkey151_ext_numeric_names() {
    // Canonical ES5.1 names, not a length or success cookie.
    pkey151_number_pair(
        r#"var key=-0;var o={"0":7};var result=o[key];var stored=o["0"];result+stored*1000"#,
        7.0,
        7.0,
    );
    pkey151_number_pair(
        r#"var key=0/0;var o={"NaN":7};var result=o[key];var stored=o["NaN"];result+stored*1000"#,
        7.0,
        7.0,
    );
    pkey151_number_pair(
        r#"var key=1/0;var o={"Infinity":7};var result=o[key];var stored=o["Infinity"];result+stored*1000"#,
        7.0,
        7.0,
    );
    pkey151_number_pair(
        r#"var key=-1/0;var o={"-Infinity":7};var result=o[key];var stored=o["-Infinity"];result+stored*1000"#,
        7.0,
        7.0,
    );
    pkey151_number_pair(
        r#"var key=1e21;var o={"1e+21":7};var result=o[key];var stored=o["1e+21"];result+stored*1000"#,
        7.0,
        7.0,
    );
    pkey151_number_pair(
        r#"var key=1e-7;var o={"1e-7":7};var result=o[key];var stored=o["1e-7"];result+stored*1000"#,
        7.0,
        7.0,
    );
    pkey151_number_pair(
        r#"var key=5e-324;var o={"5e-324":7};var result=o[key];var stored=o["5e-324"];result+stored*1000"#,
        7.0,
        7.0,
    );
}

#[test]
fn pkey151_ext_utf16_identity() {
    pkey151_number_pair(
        r#"var key="";var o={"":7};var result=o[key];var stored=o[""];result+stored*1000"#,
        7.0,
        7.0,
    );
    pkey151_number_pair(
        r#"var key="a\u0000b";var o={"a\u0000b":7};var result=o[key];var stored=o["a\u0000b"];result+stored*1000"#,
        7.0,
        7.0,
    );
    pkey151_number_pair(
        r#"var key="\uD800";var o={"\uD800":7};var result=o[key];var stored=o["\uD800"];result+stored*1000"#,
        7.0,
        7.0,
    );
    pkey151_number_pair(
        r#"var key="\uD83D\uDE00";var o={"\uD83D\uDE00":7};var result=o[key];var stored=o["\uD83D\uDE00"];result+stored*1000"#,
        7.0,
        7.0,
    );
}

#[test]
fn pkey151_ext_shadow_order_postfix() {
    pkey151_number_pair(
        r#"function f(undefined){var key=undefined;var o={"1":7,"undefined":9};var result=o[key];var stored=o["1"];return result+stored*1000;}f(1)"#,
        7.0,
        7.0,
    );
    pkey151_number_pair(
        r#"function f(undefined){var key=void 0;var o={"undefined":9};var result=o[key];var stored=o["undefined"];return result+stored*1000;}f(1)"#,
        9.0,
        9.0,
    );
    pkey151_number_pair(
        r#"var obj={"1.5":1};var slot=obj;var result=slot[(slot={"no":1},1.5)];var stored=obj["1.5"];result+stored*1000"#,
        1.0,
        1.0,
    );
    pkey151_number_pair(
        r#"var key=1.5;var o={"1.5":7};var result=o[key]++;var stored=o["1.5"];result+stored*1000"#,
        7.0,
        8.0,
    );
    pkey151_number_pair(
        r#"var key=1.5;var o={"1.5":7};var result=(o[key]-=1);var stored=o["1.5"];result+stored*1000"#,
        6.0,
        6.0,
    );
    pkey151_number_pair(
        r#"var o={"1.5":7};var result=o[1.5];var stored=o["1.5"];result+stored*1000"#,
        7.0,
        7.0,
    );
    public_number(r#"var a=[9];a[0];"#, 9.0);
}

#[test]
fn pkey151_ext_call_decay_loop() {
    pkey151_number_pair(
        r#"'use strict';var o={"a":function(x,y){"use strict";return this.mark+x+y;},mark:5};var result=o["a"](1,2);var stored=o.mark;result+stored*1000"#,
        8.0,
        5.0,
    );
    pkey151_number_pair(
        r#"'use strict';var o={"a":function(){"use strict";return this.mark;},mark:5};var result=(o["a"])();var stored=o.mark;result+stored*1000"#,
        5.0,
        5.0,
    );
    public_number(
        r#"function id(x){return x;}var o={"1.5":7,"true":7};var hits=0;var i=0;while(i<2){var key=id(i<1?1.5:true);if(o[key]===7){hits=hits+1;}i=i+1;}hits"#,
        2.0,
    );
}

#[test]
fn pkey151_ext_order_getter() {
    public_true(
        r#"function k(){throw "key";}function rhs(){throw "rhs";}var seen="";try{null[k()]=rhs();}catch(e){seen=e;}seen==="key""#,
    );
    public_true(
        r#"function id(x){return x;}var ok=false;try{id({}) in 1;}catch(e){ok=e.name==="TypeError";}ok"#,
    );
    pkey151_number_pair(
        r#"var key="a";var o={n:0,get a(){"use strict";this.n=this.n+1;return 7;}};var result=o[key];var stored=o.n;result+stored*1000"#,
        7.0,
        1.0,
    );
    public_number(
        r#"'use strict';var o={};Object.defineProperty(o,"a",{value:1});var n=0;try{o["a"]=2;}catch(e){if(!(e.name==="TypeError")){throw e;}n=1;}var stored=o["a"];if(!(stored===1)){throw stored;}n"#,
        1.0,
    );
}

fn pkey151_undefined(source: &str) {
    assert_box(&execute_public(source), 0, 0.0, false);
}

fn pkey151_catch_refuse(source: &str) {
    let error = match pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
    {
        Ok(_) => panic!("object catch key must refuse before any guest call: {source}"),
        Err(error) => error,
    };
    let text = format!("{error:?}");
    assert!(
        text.contains("primitive computed keys reject object ToPrimitive"),
        "{text}"
    );
    eprintln!("PKEY151_CATCH_REFUSE source={source:?} diagnostic={error:?} guest_invocations=0");
}

fn pkey151_catch_host(source: &str) {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!("PKEY151_SOURCE {source}");
            let mut machine =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                    .unwrap_or_else(|error| {
                        panic!(
                            "production compilation refused; guest_invocations=0 error={error:?}"
                        )
                    });
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let status = machine.run();
            let text = format!("{status:?}");
            eprintln!("PKEY151_HOST {text}");
            assert!(
                text.contains("assertion failed") && !text.contains("Fuel"),
                "{text}"
            );
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("PKEY151_RESET resource_count=0 guest_invocations=0");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn pkey151_catch_scope() {
    // 4 rows: script, function, nested shadow, inherited strict.
    pkey151_undefined(r#"try{throw 1.5;}catch(key){var o={};o[key];}"#);
    pkey151_undefined(r#"function f(){var o={};try{throw 1.5;}catch(key){return o[key];}}f();"#);
    public_number(
        r#"function f(){var key=9;var o={"1.5":7};try{throw 1.5;}catch(key){return o[key];}}f();"#,
        7.0,
    );
    public_number(
        r#"function f(){"use strict";function g(){var o={"1.5":7};try{throw 1.5;}catch(key){return o[key];}}return g();}f();"#,
        7.0,
    );
}

#[test]
fn pkey151_catch_roles() {
    // 7 rows: get, put, call, update, delete, in, compound.
    pkey151_number_pair(
        r#"function f(){var o={"1.5":7};try{throw 1.5;}catch(key){var result=o[key];var stored=o["1.5"];return result+stored*1000;}}f();"#,
        7.0,
        7.0,
    );
    pkey151_number_pair(
        r#"function f(){var o={};try{throw 1.5;}catch(key){var result=(o[key]=7);var stored=o["1.5"];return result+stored*1000;}}f();"#,
        7.0,
        7.0,
    );
    pkey151_number_pair(
        r#"function f(){var o={"1.5":function(){"use strict";return this.mark;},mark:5};try{throw 1.5;}catch(key){var result=o[key]();var stored=o.mark;return result+stored*1000;}}f();"#,
        5.0,
        5.0,
    );
    pkey151_number_pair(
        r#"function f(){var o={"1.5":7};try{throw 1.5;}catch(key){var result=++o[key];var stored=o["1.5"];return result+stored*1000;}}f();"#,
        8.0,
        8.0,
    );
    pkey151_bool(
        r#"function f(){var o={"1.5":7};try{throw 1.5;}catch(key){var result=delete o[key];var stored=o["1.5"];if(!(stored===void 0)){throw stored;}return result;}}f();"#,
    );
    pkey151_bool(
        r#"function f(){var o={"1.5":1};try{throw 1.5;}catch(key){var result=key in o;var stored=o["1.5"];if(!(stored===1)){throw stored;}return result;}}f();"#,
    );
    pkey151_number_pair(
        r#"function f(){var o={"1.5":7};try{throw 1.5;}catch(key){var result=(o[key]+=1);var stored=o["1.5"];return result+stored*1000;}}f();"#,
        8.0,
        8.0,
    );
}

#[test]
fn pkey151_catch_primitives() {
    // 8 rows: five primitives, then strict number/string/bool.
    pkey151_number_pair(
        r#"function f(){var o={"1.5":7};try{throw 1.5;}catch(key){var result=o[key];var stored=o["1.5"];return result+stored*1000;}}f();"#,
        7.0,
        7.0,
    );
    pkey151_number_pair(
        r#"function f(){var o={"a":7};try{throw "a";}catch(key){var result=o[key];var stored=o["a"];return result+stored*1000;}}f();"#,
        7.0,
        7.0,
    );
    pkey151_number_pair(
        r#"function f(){var o={"true":7};try{throw true;}catch(key){var result=o[key];var stored=o["true"];return result+stored*1000;}}f();"#,
        7.0,
        7.0,
    );
    pkey151_number_pair(
        r#"function f(){var o={"null":7};try{throw null;}catch(key){var result=o[key];var stored=o["null"];return result+stored*1000;}}f();"#,
        7.0,
        7.0,
    );
    pkey151_number_pair(
        r#"function f(){var o={"undefined":7};try{throw void 0;}catch(key){var result=o[key];var stored=o["undefined"];return result+stored*1000;}}f();"#,
        7.0,
        7.0,
    );
    pkey151_number_pair(
        r#"function f(){"use strict";var o={"1.5":7};try{throw 1.5;}catch(key){var result=o[key];var stored=o["1.5"];return result+stored*1000;}}f();"#,
        7.0,
        7.0,
    );
    pkey151_number_pair(
        r#"function f(){"use strict";var o={"a":7};try{throw "a";}catch(key){var result=o[key];var stored=o["a"];return result+stored*1000;}}f();"#,
        7.0,
        7.0,
    );
    pkey151_number_pair(
        r#"function f(){"use strict";var o={"true":7};try{throw true;}catch(key){var result=o[key];var stored=o["true"];return result+stored*1000;}}f();"#,
        7.0,
        7.0,
    );
}

#[test]
fn pkey151_catch_utf16_finally_loop() {
    // 5 rows: empty, NUL, surrogate, finally, mixed loop.
    pkey151_number_pair(
        r#"function f(){var o={"":7};try{throw "";}catch(key){var result=o[key];var stored=o[""];return result+stored*1000;}}f();"#,
        7.0,
        7.0,
    );
    pkey151_number_pair(
        r#"function f(){var o={"a\u0000b":7};try{throw "a\u0000b";}catch(key){var result=o[key];var stored=o["a\u0000b"];return result+stored*1000;}}f();"#,
        7.0,
        7.0,
    );
    pkey151_number_pair(
        r#"function f(){var o={"\uD800":7};try{throw "\uD800";}catch(key){var result=o[key];var stored=o["\uD800"];return result+stored*1000;}}f();"#,
        7.0,
        7.0,
    );
    public_number(
        r#"function f(){var box={n:0};var o={"1.5":7};try{throw 1.5;}catch(key){box.n=o[key];}finally{box.n=box.n+1;}return box.n;}f();"#,
        8.0,
    );
    public_number(
        r#"function f(){var o={"1.5":7,"true":7};var hits=0;var i=0;while(i<2){try{throw i<1?1.5:true;}catch(key){if(o[key]===7){hits=hits+1;}}i=i+1;}return hits;}f();"#,
        2.0,
    );
}

const S1: &[u16] = &[0x73, 0x31];

/// ES5 CheckObjectCoercible runs before RHS. Modern Node simple Put is not an oracle.
#[test]
fn pkey151_nullish_put_get_preserves_string() {
    assert_string_units(r#"function f(){return "s1";}f();"#, S1);
    for source in [
        r#"function f(){var x="s";try{null[1]=(x=7);}catch(e){}return x+1;}f();"#,
        r#"function f(){var x="s";try{null[1];x=7;}catch(e){}return x+1;}f();"#,
        r#"function f(){var x="s";try{undefined[1]=(x=7);}catch(e){}return x+1;}f();"#,
        r#"function f(){var x="s";try{(void 0)[1];x=7;}catch(e){}return x+1;}f();"#,
    ] {
        assert_string_units(source, S1);
    }
}

#[test]
fn pkey151_nullish_delete_call_compound_update_preserves_string() {
    for source in [
        r#"function f(){var x="s";try{delete null[1];x=7;}catch(e){}return x+1;}f();"#,
        r#"function f(){var x="s";try{null[1](x=7);}catch(e){}return x+1;}f();"#,
        r#"function f(){var x="s";try{null[1]+=(x=7);}catch(e){}return x+1;}f();"#,
        r#"function f(){var x="s";try{null[1]++;x=7;}catch(e){}return x+1;}f();"#,
        r#"function f(){var x="s";try{++undefined[1];x=7;}catch(e){}return x+1;}f();"#,
        r#"function f(){var x="s";try{undefined[1]-=(x=7);}catch(e){}return x+1;}f();"#,
    ] {
        assert_string_units(source, S1);
    }
}

#[test]
fn pkey151_nullish_key_effects_and_key_throw() {
    public_number(
        r#"function f(){var x="s";try{null[(x=7)];}catch(e){}return x+1;}f();"#,
        8.0,
    );
    public_number(
        r#"function f(){var x="s";try{++null[(x=7)];}catch(e){}return x+1;}f();"#,
        8.0,
    );
    assert_string_units(
        r#"function f(){function boom(){throw 1;}var x="s";var y="t";try{null[(x=7,boom())]=(y=9);}catch(e){}return x+y;}f();"#,
        &[0x37, 0x74],
    );
    assert_string_units(
        r#"function f(){function boom(){throw 1;}var x="s";try{null[boom()];x=7;}catch(e){}return x+1;}f();"#,
        S1,
    );
}

#[test]
fn pkey151_nullish_nested_flow_preserves_string() {
    for source in [
        r#"function f(){var x="s";try{null[1]=(x=7);}catch(e){return x+1;}return 0;}f();"#,
        r#"function f(){var x="s";try{null[1]=(x=7);}catch(e){}finally{x=x;}return x+1;}f();"#,
        r#"function f(){var x="s";try{return null[1]=(x=7);}catch(e){}return x+1;}f();"#,
        r#"function f(){var x="s";try{if(null[1]){x=7;}}catch(e){}return x+1;}f();"#,
        r#"function f(){var x="s";try{var y=(null[1]=(x=7));}catch(e){}return x+1;}f();"#,
        r#"function f(){var x="s";try{(null[1],x=7);}catch(e){}return x+1;}f();"#,
        r#"function f(){var x="s";try{while(null[1]){x=7;}}catch(e){}return x+1;}f();"#,
        r#"function f(){var x="s";try{for(;null[1];){x=7;}}catch(e){}return x+1;}f();"#,
    ] {
        assert_string_units(source, S1);
    }
}

#[test]
fn pkey151_nullish_facts_dynamic_and_object() {
    public_number(
        r#"function f(){var a=[1,2];try{null[1]=(a=[9]);}catch(e){}return a[1];}f();"#,
        2.0,
    );
    public_number(
        r#"function f(){var a=[1,2];try{null[1];a=[9];}catch(e){}return a[1];}f();"#,
        2.0,
    );
    assert_string_units(
        r#"function f(o){var x="s";try{o[1]=(x=7);}catch(e){}return x+"";}f(null);"#,
        &[0x73],
    );
    assert_string_units(
        r#"function f(o){var x="s";try{o[1]=(x=7);}catch(e){}return x+"";}f({});"#,
        &[0x37],
    );
    public_number(
        r#"function f(){var o={};var x="s";o[1]=(x=7);return x+1;}f();"#,
        8.0,
    );
    assert_string_units(
        r#"function f(){var o={};var x="s";o[1]=x;return x+1;}f();"#,
        S1,
    );
    assert_string_units(
        r#"function f(o){var x="s";try{o[1]=(x=7);}catch(e){return x+1;}return 0;}f(null);"#,
        S1,
    );
    public_number(
        r#"function f(){var o={"1":3};o[1]+=4;return o[1];}f();"#,
        7.0,
    );
}

#[test]
fn pkey151_nullish_dead_source_still_refuses() {
    for source in [
        r#"function f(){var x="s";try{null[{}]=(x=7);}catch(e){}return x+1;}f();"#,
        r#"function f(){try{null[1]=[1][true];}catch(e){}return 1;}f();"#,
    ] {
        let error =
            match pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source) {
                Err(error) => error,
                Ok(_) => panic!("dead unsupported source must refuse: {source}"),
            };
        let text = format!("{error:?}");
        assert!(
            text.contains("key")
                || text.contains("object")
                || text.contains("ToPrimitive")
                || text.contains("indexed")
                || text.contains("subscript")
                || text.contains("array"),
            "{source} {text}"
        );
    }
}

/// Conditional member throws must not be treated as definite. A falsy left
/// skips the throwing right, so the continuation runs and publishes its value.
#[test]
fn pkey151_q1_conditional_throw_does_not_skip_continuation() {
    public_number(
        r#"function f(a){var x=1;a&&null[1];x+=2;return x;}f(0);"#,
        3.0,
    );
    public_true(r#"function f(a){var x=(a&&null[1],7);return x===7;}f(0);"#);
    public_number(
        r#"function f(a){var x="s";a&&null[1];x=7;return x+1;}f(0);"#,
        8.0,
    );
    // Sibling `function g` is a pre-existing P4 capture refusal. Nested g is the
    // admitted form of the same actual-argument order check.
    public_number(
        r#"function f(a){function g(p,q){return q;}return g(a&&null[1],1);}f(0);"#,
        1.0,
    );
}

/// A throwing var initializer is a throw exit, so the catch store is a real
/// selected completion and changes the type seen by the continuation.
#[test]
fn pkey151_q2_var_initializer_throw_reaches_catch() {
    public_number(
        r#"function f(){var x="s";try{var y=(null[1]=(x=7));}catch(e){x=9;}return x+1;}f();"#,
        10.0,
    );
}

#[test]
fn pkey151_q1_or_cond_loop_and_selected_throw() {
    public_number(
        r#"function f(a){var x=1;a||null[1];x+=2;return x;}f(1);"#,
        3.0,
    );
    public_number(
        r#"function f(a){var x=1;try{a||null[1];x+=2;}catch(e){x=9;}return x;}f(0);"#,
        9.0,
    );
    public_number(
        r#"function f(a){var x=1;a?null[1]:0;x+=2;return x;}f(0);"#,
        3.0,
    );
    public_number(
        r#"function f(a){var x=1;try{a?null[1]:null[2];x+=2;}catch(e){x=9;}return x;}f(1);"#,
        9.0,
    );
    public_number(
        r#"function f(){var x=1;while(false){null[1];}x+=2;return x;}f();"#,
        3.0,
    );
    public_number(
        r#"function f(a){var x=1;try{a&&null[1];x+=2;}catch(e){x=9;}return x;}f(1);"#,
        9.0,
    );
}

#[test]
fn pkey151_q2_multi_for_and_abrupt_key() {
    public_number(
        r#"function f(){var x="s";try{var y=1,z=(null[1]=(x=7));}catch(e){x=9;}return x+1;}f();"#,
        10.0,
    );
    public_number(
        r#"function f(){var x="s";try{for(var y=(null[1]=(x=7));;){x=1;}}catch(e){x=9;}return x+1;}f();"#,
        10.0,
    );
    public_true(r#"function f(){var x="s";try{(null[1])[(x=7)]=1;}catch(e){}return x==="s";}f();"#);
}

/// A dead nullish probe after a real throw must not publish a compile-time
/// throw into the always-executed finally. The finalizer store runs and the
/// return overrides the pending throw.
#[test]
fn pkey151_b3_dead_nullish_probe_does_not_skip_finally_store() {
    public_number(
        r#"function f(){"use strict";var x=1;try{throw 1;null[2];}finally{x+=2;return x;}}f();"#,
        3.0,
    );
}

/// A dead `typeof` probe must not publish an uninitialized type-name slot.
/// The always-executed finally installs and reads its own cache.
#[test]
fn pkey151_b3_dead_typeof_probe_does_not_poison_finally_cache() {
    public_true(
        r#"function f(){"use strict";try{throw 1;var s=typeof 1;}finally{return typeof 1==="number";}}f();"#,
    );
}

/// Finally runs after an ordinary throw with no dead suffix. The return
/// overrides the pending throw.
#[test]
fn pkey151_b3_finally_runs_after_throw_without_dead_suffix() {
    public_number(
        r#"function f(){"use strict";var x=1;try{throw 1;}finally{x+=2;return x;}}f();"#,
        3.0,
    );
}

/// A live nullish throw is a real pending completion. It must not mark the
/// always-executed finally unreachable.
#[test]
fn pkey151_b3_finally_runs_after_known_nullish_throw() {
    public_number(
        r#"function f(){"use strict";var x=1;try{null[2];}finally{x+=2;return x;}}f();"#,
        3.0,
    );
}

/// A nested dead probe restores the outer dead mode and still does not
/// publish its throw into the live finally.
#[test]
fn pkey151_b3_nested_dead_probe_does_not_skip_finally_store() {
    public_number(
        r#"function f(){"use strict";var x=1;try{throw 1;{null[2];}}finally{x+=2;return x;}}f();"#,
        3.0,
    );
}

/// A real nullish throw before a dead probe stays a real throw. The probe
/// must not clear it, and the finally store still runs.
#[test]
fn pkey151_b3_preexisting_throw_survives_dead_probe() {
    public_number(
        r#"function f(){"use strict";var x=1;try{null[1];null[2];}finally{x+=2;return x;}}f();"#,
        3.0,
    );
}

/// Live `typeof` still installs a usable cache when no dead probe runs.
#[test]
fn pkey151_b3_live_typeof_cache_without_dead_suffix() {
    public_true(r#"function f(){"use strict";return typeof 1==="number";}f();"#);
}

#[test]
fn pkey151_object_key_get_converts_once() {
    public_true(
        r#"var key={n:0,toString:function(){"use strict";this.n=this.n+1;return "x";}};var o={x:7};var r=o[key];r===7&&key.n===1;"#,
    );
}

/// Post-fix functional qualifier: an object key converted once before the
/// call gate still yields the receiver method result, and a key throw keeps
/// the argument store, the catch payload and the finally counter.
#[test]
fn primitive_key151_call_common_cleanup_guest_regressions() {
    public_true(
        r#"var key={n:0,toString:function(){"use strict";this.n=this.n+1;return "m";}};var o={m:function(){"use strict";return this.tag;},tag:5};var r=o[key]();r===5&&o.tag===5&&key.n===1;"#,
    );
    public_true(
        r#"function f(){var key={toString:function(){throw "k";}};var o={m:function(){return 1;}};var x="s";var seen="";var n=0;try{o[key](x="a");}catch(e){seen=e;}finally{n=n+1;}return x==="s"&&seen==="k"&&n===1;}f();"#,
    );
}

/// Known-throw initializer guest regressions: the caught TypeError leaves the
/// declarator binding uninitialized, and the statement after the throw is skipped.
#[test]
fn primitive_key151_known_throw_initializer_guest_regressions() {
    public_true(
        r#"var ok=false;try{var result=null[{}]+=1;}catch(e){ok=e.name==="TypeError"&&typeof result==="undefined";}ok;"#,
    );
    public_true(r#"var ran=0;try{var a=null[{}]+=1;ran=1;}catch(e){}ran===0;"#);
}

#[test]
fn pkey151_object_key_put_value_once() {
    public_true(
        r#"var key={n:0,toString:function(){"use strict";this.n=this.n+1;return "x";}};var o={};var r=(o[key]=7);var s=o.x;r===7&&s===7&&key.n===1;"#,
    );
}

#[test]
fn pkey151_object_key_delete_once() {
    public_true(
        r#"var key={n:0,toString:function(){"use strict";this.n=this.n+1;return "x";}};var o={x:7};var r=delete o[key];r===true&&o.x===void 0&&key.n===1;"#,
    );
}

#[test]
fn pkey151_object_key_in_once() {
    public_true(
        r#"var key={n:0,toString:function(){"use strict";this.n=this.n+1;return "x";}};var o={x:1};var r=key in o;r===true&&o.x===1&&key.n===1;"#,
    );
}

#[test]
fn pkey151_object_key_unsupported_keys_still_refused() {
    pkey151_catch_refuse(r#"function f(){var key=[1];var o={};o[key]=7;return 1;}f();"#);
    pkey151_catch_refuse(r#"function g(){return 1;}var o={};var r=o[g];"#);
}

#[test]
fn pkey151_object_key_compound_once() {
    public_true(
        r#"var key={n:0,toString:function(){"use strict";this.n=this.n+1;return "x";}};var o={x:7};var r=(o[key]+=2);r===9&&o.x===9&&key.n===1;"#,
    );
}

#[test]
fn pkey151_object_key_postfix_update_once() {
    public_true(
        r#"var key={n:0,toString:function(){"use strict";this.n=this.n+1;return "x";}};var o={x:7};var r=o[key]++;r===7&&o.x===8&&key.n===1;"#,
    );
}

#[test]
fn pkey151_object_key_prefix_update_once() {
    public_true(
        r#"var key={n:0,toString:function(){"use strict";this.n=this.n+1;return "x";}};var o={x:7};var r=++o[key];r===8&&o.x===8&&key.n===1;"#,
    );
}

#[test]
fn pkey151_object_key_catch_default_get_is_undefined() {
    pkey151_undefined(r#"function f(){var o={};try{throw 1;}catch(key){return o[{}];}}f();"#);
}

#[test]
fn pkey151_object_key_conversion_getter_throw_before_rhs() {
    public_true(
        r#"function f(){var key={n:0,get toString(){"use strict";this.n=this.n+1;throw "k";}};var o={};var x="s";var n=0;var seen=false;try{o[key]=(x="a");}catch(e){seen=e==="k";}finally{n=n+1;}return seen&&key.n===1&&x==="s"&&n===1;}f();"#,
    );
}

#[test]
fn pkey151_object_key_numeric_tostring_result() {
    public_true(
        r#"var key={n:0,toString:function(){"use strict";this.n=this.n+1;return 42;}};var o={"42":9};var r=o[key];r===9&&o["42"]===9&&key.n===1;"#,
    );
}

#[test]
fn pkey151_object_key_valueof_fallback_order() {
    public_true(
        r#"var key={n:0,toString:function(){"use strict";this.n=this.n*10+1;return {};},valueOf:function(){"use strict";this.n=this.n*10+2;return "x";}};var o={x:9};var r=o[key];r===9&&o.x===9&&key.n===12;"#,
    );
}

#[test]
fn pkey151_object_key_both_objects_typeerror_before_rhs() {
    public_true(
        r#"function f(){var key={n:0,toString:function(){"use strict";this.n=this.n*10+1;return {};},valueOf:function(){"use strict";this.n=this.n*10+2;return {};}};var o={};var x="s";var seen=false;var fin=0;try{o[key]=(x="a");}catch(e){seen=e.name==="TypeError";}finally{fin=fin+1;}return seen&&key.n===12&&x==="s"&&fin===1;}f();"#,
    );
}

#[test]
fn pkey151_object_key_noncallable_tostring_valueof() {
    public_true(
        r#"var key={n:0,toString:7,valueOf:function(){"use strict";this.n=this.n+1;return "x";}};var o={x:9};var r=o[key];r===9&&o.x===9&&key.n===1;"#,
    );
}

#[test]
fn pkey151_object_key_method_throw_no_fallback() {
    public_true(
        r#"function f(){var key={n:0,toString:function(){"use strict";this.n=this.n+1;throw "m";},valueOf:function(){"use strict";this.n=this.n+10;return "x";}};var o={};var seen=false;var fin=0;try{o[key];}catch(e){seen=e==="m";}finally{fin=fin+1;}return seen&&key.n===1&&fin===1;}f();"#,
    );
}

#[test]
fn pkey151_object_key_nullish_put_no_conversion_rhs() {
    public_true(
        r#"function f(){var key={n:0,toString:function(){"use strict";this.n=this.n+1;return "x";}};var x="s";var seen=false;try{null[key]=(x="a");}catch(e){seen=e.name==="TypeError";}return seen&&key.n===0&&x==="s";}f();"#,
    );
}

#[test]
fn pkey151_object_key_in_number_rhs_no_conversion() {
    public_true(
        r#"function f(){var key={n:0,toString:function(){"use strict";this.n=this.n+1;return "x";}};var seen=false;try{key in 5;}catch(e){seen=e.name==="TypeError";}return seen&&key.n===0;}f();"#,
    );
}

#[test]
fn pkey151_object_key_in_null_rhs_no_conversion() {
    public_true(
        r#"function f(){var key={n:0,toString:function(){"use strict";this.n=this.n+1;return "x";}};var seen=false;try{key in null;}catch(e){seen=e.name==="TypeError";}return seen&&key.n===0;}f();"#,
    );
}

#[test]
fn pkey151_object_key_valueof_getter_throw_before_rhs() {
    public_true(
        r#"function f(){var key={n:0,toString:function(){"use strict";this.n=this.n*10+1;return {};},get valueOf(){"use strict";this.n=this.n*10+2;throw "v";}};var o={};var x="s";var seen=false;var fin=0;try{o[key]=(x="a");}catch(e){seen=e==="v";}finally{fin=fin+1;}return seen&&key.n===12&&x==="s"&&fin===1;}f();"#,
    );
}

#[test]
fn pkey151_object_key_empty_string_result() {
    public_true(
        r#"var key={n:0,toString:function(){"use strict";this.n=this.n+1;return "";}};var o={"":7,other:9};var r=o[key];r===7&&o.other===9&&key.n===1;"#,
    );
}

#[test]
fn pkey151_object_key_nul_string_result() {
    public_true(
        r#"var key={n:0,toString:function(){"use strict";this.n=this.n+1;return "a\u0000b";}};var o={"a":1,"a\u0000b":7,"a\u0000c":9};var r=o[key];r===7&&o["a"]===1&&o["a\u0000c"]===9&&key.n===1;"#,
    );
}

#[test]
fn pkey151_object_key_lone_surrogate_result() {
    public_true(
        r#"var key={n:0,toString:function(){"use strict";this.n=this.n+1;return "\uD800";}};var o={"\uD800":7,"\uFFFD":9};var r=o[key];r===7&&o["\uFFFD"]===9&&key.n===1;"#,
    );
}

#[test]
fn pkey151_object_key_undefined_get_no_conversion() {
    public_true(
        r#"function f(){var key={n:0,toString:function(){"use strict";this.n=this.n+1;return "x";}};var x="s";var seen=false;try{(void 0)[key];x="a";}catch(e){seen=e.name==="TypeError";}return seen&&key.n===0&&x==="s";}f();"#,
    );
}

#[test]
fn pkey151_object_key_base_getter_call_skips_args() {
    public_true(
        r#"function f(){var key={n:0,toString:function(){"use strict";this.n=this.n+1;return "m";}};var o={n:0,get m(){"use strict";this.n=this.n+1;throw "g";}};var x="s";var seen=false;var fin=0;try{o[key](x="a");}catch(e){seen=e==="g";}finally{fin=fin+1;}return seen&&key.n===1&&o.n===1&&x==="s"&&fin===1;}f();"#,
    );
}

#[test]
fn pkey151_object_key_base_getter_compound_skips_rhs() {
    public_true(
        r#"function f(){var key={n:0,toString:function(){"use strict";this.n=this.n+1;return "x";}};var o={n:0,get x(){"use strict";this.n=this.n+1;throw "g";}};var x="s";var seen=false;var fin=0;try{o[key]+=(x="a");}catch(e){seen=e==="g";}finally{fin=fin+1;}return seen&&key.n===1&&o.n===1&&x==="s"&&fin===1;}f();"#,
    );
}

#[test]
fn pkey151_object_key_sloppy_original_default_get_is_undefined() {
    pkey151_undefined(r#"function f(){var key={};var o={};return o[key];}f();"#);
}

#[test]
fn pkey151_dynamic_object_key_get_converts_once() {
    public_true(
        r#"function id(x){return x;}var key=id({n:0,toString:function(){"use strict";this.n=this.n+1;return "x";}});var o={x:7};var r=o[key];r===7&&key.n===1;"#,
    );
}

#[test]
fn pkey151_dynamic_object_key_throw_before_put_rhs() {
    public_true(
        r#"function id(x){return x;}var key=id({n:0,toString:function(){"use strict";this.n=this.n+1;throw "k";}});var o={};var x="s";var seen=false;var fin=0;try{o[key]=(x="a");}catch(e){seen=e==="k";}finally{fin=fin+1;}seen&&key.n===1&&x==="s"&&fin===1;"#,
    );
}

#[test]
fn pkey151_dynamic_object_key_throw_before_in_consumer() {
    public_true(
        r#"function id(x){return x;}var key=id({n:0,toString:function(){"use strict";this.n=this.n+1;throw "k";}});var o={x:1};var seen=false;var fin=0;try{key in o;}catch(e){seen=e==="k";}finally{fin=fin+1;}seen&&key.n===1&&fin===1;"#,
    );
}

#[test]
fn pkey151_dynamic_object_key_throw_before_call_args() {
    public_true(
        r#"function id(x){return x;}var key=id({n:0,toString:function(){"use strict";this.n=this.n+1;throw "k";}});var o={m:function(a){return a;}};var x="s";var seen=false;var fin=0;try{o[key](x="a");}catch(e){seen=e==="k";}finally{fin=fin+1;}seen&&key.n===1&&x==="s"&&fin===1;"#,
    );
}

#[test]
fn pkey151_dynamic_array_key_remains_unsupported() {
    pkey151_catch_host(r#"function id(x){return x;}var key=id([1,2]);var o={"1,2":7};o[key];"#);
}

#[test]
fn pkey151_dynamic_function_key_remains_unsupported() {
    pkey151_catch_host(
        r#"function id(x){return x;}function g(){return 1;}var key=id(g);var o={};o[key];"#,
    );
}

#[test]
fn pkey151_catch_object_key_get_is_undefined() {
    pkey151_undefined(r#"function f(){var o={};try{throw {};}catch(key){return o[key];}}f();"#);
}

#[test]
fn pkey151_dynamic_object_key_a_put_once() {
    public_true(
        r#"function id(x){return x;}var key=id({n:0,toString:function(){"use strict";this.n=this.n+1;return "x";}});var o={};var r=(o[key]=7);r===7&&o.x===7&&key.n===1;"#,
    );
}

#[test]
fn pkey151_dynamic_object_key_a_call_once() {
    public_true(
        r#"function id(x){return x;}var key=id({n:0,toString:function(){"use strict";this.n=this.n+1;return "m";}});var o={m:function(){"use strict";return this.mark;},mark:5};var r=o[key]();r===5&&o.mark===5&&key.n===1;"#,
    );
}

#[test]
fn pkey151_dynamic_object_key_a_delete_once() {
    public_true(
        r#"function id(x){return x;}var key=id({n:0,toString:function(){"use strict";this.n=this.n+1;return "x";}});var o={x:7};var r=delete o[key];r===true&&o.x===void 0&&!("x" in o)&&key.n===1;"#,
    );
}

#[test]
fn pkey151_dynamic_object_key_a_in_once() {
    public_true(
        r#"function id(x){return x;}var key=id({n:0,toString:function(){"use strict";this.n=this.n+1;return "x";}});var o={x:1};var r=key in o;r===true&&o.x===1&&key.n===1;"#,
    );
}

#[test]
fn pkey151_dynamic_object_key_b_compound_once() {
    public_true(
        r#"function id(x){return x;}var key=id({n:0,toString:function(){"use strict";this.n=this.n+1;return "x";}});var o={x:7};var r=(o[key]+=2);r===9&&o.x===9&&key.n===1;"#,
    );
}

#[test]
fn pkey151_dynamic_object_key_b_postfix_once() {
    public_true(
        r#"function id(x){return x;}var key=id({n:0,toString:function(){"use strict";this.n=this.n+1;return "x";}});var o={x:7};var r=o[key]++;r===7&&o.x===8&&key.n===1;"#,
    );
}

#[test]
fn pkey151_dynamic_object_key_b_prefix_once() {
    public_true(
        r#"function id(x){return x;}var key=id({n:0,toString:function(){"use strict";this.n=this.n+1;return "x";}});var o={x:7};var r=++o[key];r===8&&o.x===8&&key.n===1;"#,
    );
}

#[test]
fn pkey151_dynamic_object_key_in_dynamic_primitive_rhs_once() {
    public_true(
        r#"function id(x){return x;}var key=id({n:0,toString:function(){"use strict";this.n=this.n+1;return "x";}});var rhs=0;var after=false;var seen=false;var fin=0;try{key in id(rhs=rhs+1);after=true;}catch(e){seen=e.name==="TypeError";}finally{fin=fin+1;}seen&&rhs===1&&key.n===0&&after===false&&fin===1;"#,
    );
}
