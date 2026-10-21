//! Private ES5 Script syntax/admission unit tests.
//!
//! These tests exercise the crate-private parser stage directly: admission
//! success here is grammar acceptance only, never a claim that P0 lowering
//! can execute the admitted form. Refusals carry one of three categories,
//! asserted explicitly: `SyntaxError` (ES5 grammar/early-error violations),
//! `unsupported syntax` (post-ES5 forms refused at admission) or
//! `parse error` (raw SWC lexer/parser rejections that agree with ES5).

use super::parser::parse_script;
use super::string_literal::{
    StringCodecErrorKind, StringStrictness, decode_string_literal, narrowed_span,
};
use vm::ParseError;

const SOURCE_ID: u32 = 7;

fn admitted(source: &str) {
    if let Err(error) = parse_script(source, SOURCE_ID) {
        panic!("ES5 source must be admitted at the parser stage: {source:?}\n{error:?}");
    }
}

fn refused(source: &str) -> ParseError {
    parse_script(source, SOURCE_ID)
        .err()
        .unwrap_or_else(|| panic!("source must be refused at the parser stage: {source:?}"))
}

fn refused_syntax(source: &str) -> ParseError {
    let error = refused(source);
    assert!(
        error.message.contains("JavaScript ES5 SyntaxError"),
        "grammar/early-error refusals carry the SyntaxError category, got: {error:?}",
    );
    error
}

fn refused_unsupported(source: &str) -> ParseError {
    let error = refused(source);
    assert!(
        error.message.contains("JavaScript ES5 unsupported syntax"),
        "post-ES5 refusals carry the unsupported category, got: {error:?}",
    );
    error
}

fn check_span(source: &str, error: &ParseError) {
    let span = error
        .span
        .expect("admission diagnostics carry caller spans");
    assert_eq!(
        span.source_id, SOURCE_ID,
        "diagnostic keeps the caller source id"
    );
    assert!(
        span.lo < span.hi && span.hi <= source.len(),
        "diagnostic span is a byte range inside the source, got: {span:?} for {source:?}",
    );
}

/// Vertical tracer: strict duplicate data properties are an ES5.1 early
/// error the modern SWC parser silently admits.
#[test]
fn strict_duplicate_data_property_is_rejected() {
    let source = "\"use strict\"; ({a:1,a:2});";
    let error = refused_syntax(source);
    assert!(
        error.message.to_ascii_lowercase().contains("duplicate"),
        "diagnostic must identify the duplicate-property early error, got: {error:?}",
    );
    check_span(source, &error);
    assert_eq!(
        &source[error.span.unwrap().lo..error.span.unwrap().hi],
        "a:2",
        "the span points at the repeated property",
    );
}

/// The same duplicate keys are legal in sloppy code (ES5.1, not modern
/// Node leniency: only the strict early error is enforced).
#[test]
fn sloppy_duplicate_data_property_is_admitted() {
    admitted("({a:1,a:2});");
    admitted("({__proto__:1,__proto__:2});");
}

/// `__proto__` keys are ordinary data keys for grammar purposes.
#[test]
fn strict_duplicate_proto_data_keys_are_rejected() {
    refused_syntax("\"use strict\"; ({__proto__:1,__proto__:2});");
    refused_syntax("\"use strict\"; ({__proto__:1,\"__proto__\":2});");
}

/// Canonical property-name collisions: identifier, string and numeric
/// spellings (including escapes) compare by UTF-16 value.
#[test]
fn strict_canonical_key_collisions_are_rejected() {
    for source in [
        "\"use strict\"; ({1:1,\"1\":2});",
        "\"use strict\"; ({1:1,1.0:2});",
        "\"use strict\"; ({0.5:1,\"0.5\":2});",
        "\"use strict\"; ({1e1:1,\"10\":2});",
        "\"use strict\"; ({\"\\u0031\":1,1:2});",
        "\"use strict\"; ({\"1e+21\":1,1e21:2});",
        "\"use strict\"; ({a:1,\"a\":2});",
    ] {
        let error = refused_syntax(source);
        assert!(
            error.message.contains("duplicate data property"),
            "got: {error:?}",
        );
        check_span(source, &error);
    }
}

/// Near-miss spellings must not collide: `"01"` differs from `1`, and
/// `"1e21"` differs from the ES5 canonical `1e+21` (Rust would print `1e21`,
/// ES5 NumberToString prints `1e+21`).
#[test]
fn strict_distinct_key_spellings_are_admitted() {
    admitted("\"use strict\"; ({1:1,\"01\":2});");
    admitted("\"use strict\"; ({\"1e21\":1,1e21:2});");
    admitted("\"use strict\"; ({1:1,2:2});");
    admitted("\"use strict\"; ({\"NaN\":1});");
}

/// Accessor duplicate rules: get+set pairs are legal; every other repeat
/// (including data/accessor conflicts) is a strict early error.
#[test]
fn strict_accessor_duplicate_rules() {
    admitted("\"use strict\"; ({get a(){return 1;},set a(v){}});");
    admitted("\"use strict\"; ({set a(v){},get a(){return 1;}});");
    for (source, label) in [
        (
            "\"use strict\"; ({get a(){return 1;},get a(){return 2;}});",
            "duplicate getter",
        ),
        (
            "\"use strict\"; ({set a(v){},set a(w){}});",
            "duplicate setter",
        ),
        (
            "\"use strict\"; ({a:1,get a(){return 2;}});",
            "data and accessor property conflict",
        ),
        (
            "\"use strict\"; ({get a(){return 1;},a:2});",
            "data and accessor property conflict",
        ),
        (
            "\"use strict\"; ({a:1,set a(v){}});",
            "data and accessor property conflict",
        ),
        (
            "\"use strict\"; ({get a(){return 1;},set a(v){},get a(){return 3;}});",
            "duplicate getter",
        ),
    ] {
        let error = refused_syntax(source);
        assert!(
            error.message.contains(label),
            "expected '{label}', got: {error:?}",
        );
        check_span(source, &error);
    }
}

/// Sloppy code admits every accessor combination.
#[test]
fn sloppy_accessor_combinations_are_admitted() {
    admitted("({get a(){return 1;},get a(){return 2;}});");
    admitted("({set a(v){},set a(w){}});");
    admitted("({a:1,get a(){return 2;}});");
    admitted("({get a(){return 1;},set a(v){}});");
}

/// Strictness is inherited by nested function code, including getters.
/// Cases the locked parser already rejects stay layer-agnostic; the
/// own-directive pins below cover the validator's independent checks.
#[test]
fn nested_strictness_is_inherited() {
    refused("\"use strict\"; function f(){ with(x){} }");
    refused("function f(){ \"use strict\"; return 010; }");
    refused("\"use strict\"; ({get a(){ with(x){return 1;} }});");
    admitted("({get a(){ with(x){return 1;} }});");
}

/// A nested directive does not leak outward.
#[test]
fn nested_directive_does_not_leak_outward() {
    admitted("function f(){ \"use strict\"; } 010;");
    admitted("function f(){ \"use strict\"; } var eval = 1;");
}

/// A function's own directive validates its name and parameters, even
/// though the parameter list precedes the directive textually.
#[test]
fn own_directive_validates_name_and_parameters() {
    let error = refused_syntax("function f(eval){ \"use strict\"; }");
    assert!(error.message.contains("eval"), "got: {error:?}");
    refused_syntax("function f(a,a){ \"use strict\"; }");
    refused_syntax("function eval(){ \"use strict\"; }");
    admitted("function f(eval){ return eval; }");
}

/// Duplicate parameters are a strict-only early error.
#[test]
fn duplicate_parameter_strictness_pair() {
    refused_syntax("\"use strict\"; function f(a,a){ return a; }");
    refused_syntax("\"use strict\"; function f(a,a,a){ return a; }");
    admitted("function f(a,a){ return a; }");
}

/// `eval`/`arguments` and strict reserved words cannot be bound in strict
/// code; sloppy `var let` / `var yield` stay legal.
#[test]
fn strict_binding_names() {
    for source in [
        "\"use strict\"; var eval = 1;",
        "\"use strict\"; var arguments = 1;",
        "\"use strict\"; function eval(){}",
        "\"use strict\"; function f(){ var arguments = 1; }",
        "\"use strict\"; var let = 1;",
        "\"use strict\"; var static = 1;",
        "\"use strict\"; function f(let){}",
        "\"use strict\"; try{}catch(eval){}",
    ] {
        refused(source);
    }
    admitted("var let = 1;");
    admitted("var yield = 1;");
    admitted("var eval = 1;");
    admitted("function eval(){}");
    admitted("function f(static){ return static; }");
}

/// Reserved words are legal property names in both modes.
#[test]
fn reserved_word_property_keys_are_admitted() {
    admitted("({let:1, yield:2, static:3});");
    admitted("\"use strict\"; ({let:1, yield:2});");
    admitted("x.let; x.yield;");
}

/// The directive prologue follows actual source spelling only. The locked
/// lexer owns legacy-octal diagnostics once strictness is established, so
/// the `010` refusals below stay layer-agnostic.
#[test]
fn directive_prologue_spelling() {
    refused("'use strict'; 010;");
    refused("\"use strict\"; \"extra\"; 010;");
    refused("\"use strict\"; \"not strict\"; 010;");
    admitted("(\"use strict\"); 010;");
    admitted("\"use \\u0073trict\"; 010;");
    admitted("\"\\u0022use strict\\u0022\"; 010;");
    admitted("1; \"use strict\"; 010;");
}

/// Legacy octal numerics: strict early errors, sloppy legacy admissions.
#[test]
fn legacy_octal_numeric_pair() {
    admitted("010;");
    admitted("08;");
    admitted("00;");
    refused("\"use strict\"; 010;");
    refused("\"use strict\"; 08;");
    refused("\"use strict\"; 00;");
    admitted("0xFF;");
    admitted("\"use strict\"; 0xFF;");
    admitted("3.14; .5; 5.; 1e3; 1E-3;");
}

/// Legacy octal string escapes: `\0` alone is legal everywhere; every
/// other legacy octal spelling is a strict-mode early error, and decimal
/// `8`/`9` plus invalid octal lookaheads have no ES5/Annex-B derivation in
/// either mode (the locked lexer fires first on strict legacy octals once
/// it tracks strictness, so those refusals stay layer-agnostic).
#[test]
fn legacy_octal_string_escape_pair() {
    admitted("var s = \"\\0\";");
    admitted("\"use strict\"; var s = \"\\0\";");
    // Genuine Annex-B derivations stay admitted in sloppy code.
    admitted("var s = \"\\01\";");
    admitted("var s = \"\\1\";");
    admitted("var s = \"\\12\";");
    admitted("var s = \"\\123\";");
    admitted("var s = \"\\40\";");
    admitted("var s = \"\\400\";");
    refused("var s = \"\\8\";");
    refused("var s = \"\\9\";");
    refused("var s = \"\\08\";");
    refused("var s = \"\\09\";");
    refused("var s = \"\\118\";");
    refused("var s = \"\\48\";");
    refused("\"use strict\"; var s = \"\\01\";");
    refused("\"use strict\"; var s = \"\\8\";");
    refused("\"use strict\"; var s = \"\\00\";");
    refused("\"use strict\"; var s = \"\\08\";");
    // Identity and Unicode escapes are valid ES5 in both modes.
    admitted("var s = \"\\a\\q\";");
    admitted("\"use strict\"; var s = \"\\a\\q\";");
    admitted("var s = \"\\uD800\";");
    admitted("\"use strict\"; var s = \"\\uD800\";");
}

/// Explicit radix prefixes and separators are post-ES5 in every mode.
#[test]
fn modern_numeric_forms_are_rejected() {
    for source in ["0o17;", "0O17;", "0b11;", "0B11;", "1_000;", "5n;"] {
        refused_unsupported(source);
        refused_unsupported(&format!("\"use strict\"; {source}"));
    }
}

/// Ordinary ES5 statements and expressions are admitted at the grammar
/// stage (lowering them is separate P0 work, not claimed here).
#[test]
fn ordinary_es5_forms_are_admitted() {
    for source in [
        "var x = 1; var y;",
        "function f(){ return 1; }",
        "function f(a, b){ return a + b; }",
        "var f = function g(){ return g; };",
        "var f = function(){ return 1; };",
        "function outer(){ function inner(){ return 1; } return inner(); }",
        "if (a) { b; } else { c; }",
        "if (a) b; else c;",
        "while (a) { b; }",
        "do { a; } while (b);",
        "for (var i = 0; i < 9; i++) { a; }",
        "for (i = 0, j = 9; i < j; i++, j--) { a; }",
        "for (var k in obj) { a; }",
        "for (k in obj) { a; }",
        "for (obj.prop in obj) { a; }",
        "a: for (;;) { continue a; }",
        "a: { break a; }",
        "switch (a) { case 1: b; break; default: c; }",
        "try { a; } catch (e) { b; }",
        "try { a; } catch (e) { b; } finally { c; }",
        "try { a; } finally { c; }",
        "throw new Error();",
        "with (obj) { a; }",
        "debugger;",
        ";",
        "var hole = [,];",
        "var sparse = [1, , 3];",
        "var o = {};",
        "var o = {a: 1, \"b\": 2, 3: 4};",
        "var o = {get a(){ return 1; }, set a(v){}};",
        "var u = undefined; var n = null;",
        "a, b; c ? a : b;",
        "typeof x; void 0; -a; +a; !a; ~a;",
        "a in b; a instanceof B;",
        "new F(1, 2); new F;",
        "this.x; this;",
        "x++; x--; ++x; --x;",
        "x = 1; x += 2; x -= 1; x *= 2; x /= 2; x %= 2;",
        "x <<= 1; x >>= 1; x >>>= 1; x |= 1; x &= 1; x ^= 1;",
        "(x) = 1; (x)++;",
        "({a: 1}).a = 2;",
        "x[0]; x[\"k\"]; x[0] = 1;",
        "/a/g; /a/i; /a/m; /a/gim;",
        "/(?=a)b/; /(?!a)b/; /[()]/; /\\(/; /a|b/; /(a)(b)/;",
        "/(?:a)b/; /(?:a(?:b)c)/;",
        "eval(x); arguments[0];",
        "x.let; x[\"yield\"];",
    ] {
        admitted(source);
    }
}

/// Post-ES5 declarations and forms are refused with the unsupported
/// category, even when nested inside ordinary code.
#[test]
fn modern_forms_are_rejected() {
    for source in [
        "let x = 1;",
        "const x = 1;",
        "class C {}",
        "var C = class {};",
        "() => 1;",
        "function* g(){ yield 1; }",
        "async function f(){}",
        "var f = async function(){};",
        "`hi`;",
        "tag`hi`;",
        "var {a} = b;",
        "var [a] = b;",
        "function f(a = 1){ return a; }",
        "function f(...a){ return a; }",
        "function f([a]){ return a; }",
        "f(...a);",
        "[...a];",
        "var o = {...a};",
        "for (var x of y) {}",
        "for (x of y) {}",
        "2 ** 3;",
        "x **= 2;",
        "x ?? y;",
        "x &&= y; x ||= y; x ??= y;",
        "a?.b; a?.[b]; a?.();",
        "5n;",
        "var o = {a};",
        "var o = {m(){ return 1; }};",
        "var o = {[\"k\"]: 1};",
        "var o = {\"k\": 1, ...rest};",
        "try { x; } catch { y; }",
        "try { x; } catch ([e]) {}",
        "import(x);",
        "[...[]];",
        "function f(){ let x = 1; }",
        "if (a) { class C {} }",
    ] {
        refused_unsupported(source);
    }
    // The locked parser already rejects `new.target` and `super` outside
    // methods in scripts; those diagnostics stay rejections without an
    // admission category.
    refused("new.target;");
    refused("super.x;");
    refused("super();");
}

/// Illegal control-flow relationships stay true syntax rejections. The
/// locked parser owns duplicate/undefined label and break-position checks
/// (its diagnostics stay rejections); the validator independently pins the
/// `continue`-to-non-iteration-label rule and the `for-in` initializer rule
/// that the locked parser admits.
#[test]
fn illegal_control_flow_is_rejected() {
    refused("return 1;");
    refused("break;");
    refused("continue;");
    refused("function f(){ break; }");
    refused("function f(){ continue; }");
    refused("switch (a) { case 1: continue; }");
    refused("break missing;");
    refused("continue missing;");
    refused("a: a: 1;");
    refused("a: { a: 1; }");
    refused("function f(){ a: 1; } break a;");
    refused_syntax("for(;;){ a: { continue a; } }");
    refused_syntax("for (var x = 1 in y) {}");
    refused("f()++;");
    refused("1++;");
    admitted("a: b: 1;");
    admitted("a: for (;;) { break a; }");
    admitted("a: b: for (;;) { continue b; }");
    admitted("a: b: for (;;) { continue a; }");
    admitted("a: 1; a: 2;");
}

/// Function boundaries isolate control-flow contexts: `break`/`continue`
/// and labels never cross into or out of a function body. The locked parser
/// owns `break`/`continue` position and refuses function-nested crossings
/// before admission runs (those pins stay layer-agnostic, matching
/// `illegal_control_flow_is_rejected`); the validator owns label-set
/// isolation. Function expressions keep the label pins independent of the
/// sloppy block-function extension policy.
#[test]
fn function_boundaries_isolate_control_flow() {
    for source in [
        "while (x) { var f = function(){ break; }; }",
        "while (x) { var f = function(){ continue; }; }",
        "switch (x) { case 1: var f = function(){ break; }; }",
        "a: while (x) { var f = function(){ break a; }; }",
        "a: while (x) { var f = function(){ continue a; }; }",
        "while (x) { var o = { get a(){ break; return 1; } }; }",
        "while (x) { var o = { set a(v){ break; } }; }",
        "while (x) { function f(){ break; } }",
    ] {
        refused(source);
    }
    // The inner function owns its label set (shadowing the outer label is
    // legal) and the outer loop/labels stay usable after the inner function.
    // (`continue`/`break` targeting an outer label from inside a function is
    // refused by the locked parser first, like the crossings above; the
    // validator's own label work is this no-false-positive plus restore.)
    admitted("a: while (x) { var f = function(){ a: while (y) { break a; } }; break a; }");
    admitted("while (x) { var f = function(){ while (y) { break; } }; break; }");
    // An inner loop/`break` inside the function remains valid, including in
    // getters and in a sloppy block-function extension declaration (whose
    // position stays admitted while its body is checked independently).
    admitted("while (x) { var f = function(){ while (y) { break; } }; }");
    admitted("while (x) { var o = { get a(){ while (y) { break; } return 1; } }; }");
    admitted("while (x) { function f(){ while (y) { break; } } }");
}

/// Function-declaration position is separate from effective strictness.
/// Direct SourceElements (script bodies and function bodies) admit
/// declarations in both modes; block/`if`/loop statement positions reject
/// them under effective strictness with a SyntaxError. Sloppy statement
/// positions stay admitted only as an explicitly labeled implementation
/// extension outside normative ES5.1 (normative ES5.1 allows declarations
/// in SourceElements positions only; this is not an ES5.1 Annex B rule).
#[test]
fn statement_position_functions_track_enclosing_strictness() {
    // Strict root and direct nested declarations stay valid.
    admitted("\"use strict\"; function f(){ return 1; }");
    admitted("\"use strict\"; function f(){ function g(){ return 1; } return g(); }");
    admitted("function f(){ \"use strict\"; function g(){ return 1; } }");
    admitted("\"use strict\"; var f = function(){ function g(){ return 1; } };");
    // Strict block/`if`/loop statement positions reject.
    for source in [
        "\"use strict\"; { function f(){} }",
        "\"use strict\"; if (x) { function f(){} }",
        "\"use strict\"; if (x) { function f(){} } else { function g(){} }",
        "\"use strict\"; function f(){ { function g(){} } }",
        "function f(){ \"use strict\"; { function g(){} } }",
        "\"use strict\"; while (x) { function f(){} }",
    ] {
        refused_syntax(source);
    }
    // The locked parser owns strict labeled functions outright.
    refused("\"use strict\"; a: function f(){}");
    // Sloppy statement positions stay admitted as the labeled extension.
    admitted("{ function f(){} }");
    admitted("if (x) { function f(){} }");
    admitted("a: function f(){}");
    admitted("while (x) { function f(){} }");
    // An own strict directive governs the extension function's name, params
    // and body without making the outer block strict.
    admitted("{ function f(){ \"use strict\"; } }");
    // The locked lexer fires first on the strict octal below, so that pin
    // stays layer-agnostic; the `eval` name is the validator's own check.
    refused("{ function f(){ \"use strict\"; return 010; } }");
    refused_syntax("{ function eval(){ \"use strict\"; } }");
}

/// Post-ES5 braced Unicode escapes are rejected in every actual identifier
/// lexeme before the cooked SWC symbol loses the spelling. Fixed four-hex
/// escapes stay legal wherever the decoded identifier is valid.
#[test]
fn braced_unicode_identifier_lexemes_are_rejected() {
    for source in [
        "var \\u{41}=1;",
        "var A=1; var x=\\u{41}; x;",
        "var x=\\u{41};",
        "\\u{41} = 1;",
        "\\u{41}++;",
        "for (\\u{41} in obj) {}",
        "function \\u{41}(){}",
        "function f(\\u{41}){}",
        "try{}catch(\\u{41}){}",
        "x.\\u{41};",
        "x.\\u{41} = 1;",
        "({\\u{41}:1});",
        "\\u{41}: 1;",
        "A: while (x) { break \\u{41}; }",
    ] {
        let error = refused_unsupported(source);
        check_span(source, &error);
    }
    // Fixed ES5 escapes remain allowed, including reserved property names.
    admitted("var \\u0041=1;");
    admitted("var A=1; var x=A; x;");
    admitted("x.\\u0041; x.\\u006c\\u0065\\u0074;");
    admitted("({\\u0041:1});");
    admitted("function f(\\u0041){ return A; }");
}

/// Private member names are post-ES5 in every target position, exactly as
/// in read position (the read path already refuses them; the assignment,
/// update and `for-in` walks must not silently accept them).
#[test]
fn private_member_targets_are_rejected() {
    refused_unsupported("x.#p;");
    for source in ["x.#p = 1;", "++x.#p;", "x.#p++;", "for (x.#p in y) {}"] {
        let error = refused_unsupported(source);
        check_span(source, &error);
    }
    // Ordinary member writes stay admitted at the parser stage.
    admitted("x.p = 1;");
    admitted("for (x.p in y) {}");
}

/// Catch-all: assignment and `for-in` targets enforce reference roles and
/// strict `eval`/`arguments` rules (both layers agree on every form below;
/// member-expression property writes never count as `eval` bindings).
#[test]
fn assignment_target_rules() {
    refused("\"use strict\"; eval = 1;");
    refused("\"use strict\"; arguments = 1;");
    refused("\"use strict\"; eval++;");
    refused("\"use strict\"; --arguments;");
    refused("\"use strict\"; for (eval in obj) {}");
    refused("\"use strict\"; (eval) = 1;");
    refused("\"use strict\"; (eval)++;");
    refused("\"use strict\"; for ((eval) in obj) {}");
    admitted("\"use strict\"; x.eval = 1;");
    admitted("\"use strict\"; eval.x = 1;");
    admitted("\"use strict\"; eval(1);");
    admitted("\"use strict\"; var e = eval;");
    admitted("for (obj[k] in obj) {}");
}

/// `with` and unqualified `delete` are strict-only errors (both layers
/// agree; the locked parser fires first on the bare forms).
#[test]
fn with_and_delete_strictness_pair() {
    refused("\"use strict\"; with (x) { y; }");
    refused("\"use strict\"; delete x;");
    refused("\"use strict\"; delete (eval);");
    admitted("with (x) { y; }");
    admitted("delete x;");
    admitted("\"use strict\"; delete x.y;");
    admitted("\"use strict\"; delete x[0];");
    admitted("delete x.y;");
}

/// ES5 regexp flags and group syntax. ES5.1 section 15.10.1 admits lookahead
/// (`(?=...)`/`(?!...)`) and non-capturing (`(?:...)`) groups; named groups
/// and lookbehind stay post-ES5.
#[test]
fn regexp_admission_rules() {
    admitted("/a/g; /b/i; /c/m; /d/gim;");
    for source in ["/a/u;", "/a/y;", "/a/s;", "/a/d;", "/a/v;", "/a/gg;"] {
        refused(source);
    }
    refused_unsupported("/(?<n>a)/;");
    refused_unsupported("/(?<=a)b/;");
    refused_unsupported("/(?<!a)b/;");
    // Non-capturing groups are normative ES5, including nested ones; escaped
    // and class `(?:` lookalikes never reach the group gate.
    admitted("/(?:a)/; /(?:a(?:b)c)/;");
    admitted("/\\(\\?:a\\)/; /[(?:)]/; /(?=[(])/;");
    admitted("/(?=a)/; /(?!a)/;");
}

/// Error spans stay inside the source after UTF-8 prefixes and point into
/// nested functions with the caller source id.
#[test]
fn diagnostic_spans_track_source_bytes() {
    let source = "// h\u{00e9}llo\n\"use strict\"; function f(){ return ({a:1,a:2}); }";
    let error = refused_syntax(source);
    check_span(source, &error);
    let span = error.span.unwrap();
    assert!(
        source.as_bytes()[..span.lo].contains(&b'\n'),
        "the span sits after the non-ASCII comment line",
    );
}

/// The frozen audit-seed fixture pins the parser-stage classification: 91
/// primary rows plus 12 supplemental diagnostic copies, with original
/// id/category/supplemental/source preserved exactly and explicit
/// parser/compiled/executed/native phases (lowering and execution stay
/// deferred, never inferred from admission).
#[test]
fn audit_seed_fixture_matches_parser_stage() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/javascript_es5");
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(format!("{dir}/manifest.json")).expect("seed manifest"),
    )
    .expect("seed manifest JSON");
    assert_eq!(manifest["frozen_counts"]["rows"], 91 + 12);
    let text = std::fs::read_to_string(format!("{dir}/audit_seed.jsonl")).expect("seed fixture");
    let mut primary = 0;
    let mut supplemental = 0;
    let mut ids = std::collections::HashSet::new();
    let mut strict_negative_seen = false;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let row: serde_json::Value = serde_json::from_str(line).expect("seed row JSON");
        let id = row["id"].as_str().expect("seed row id").to_owned();
        assert!(ids.insert(id.clone()), "duplicate seed id: {id}");
        let source = row["source"].as_str().expect("seed row source");
        // The original oracle pointer is retained verbatim on every row
        // (joined by exact id, never regenerated); grammar-phase consumers
        // never substitute it.
        let oracle = row.get("oracle_source").unwrap_or_else(|| {
            panic!("seed '{id}' must retain its verbatim original oracle_source")
        });
        assert!(
            oracle.is_string(),
            "oracle_source stays a verbatim string pointer for '{id}', got: {oracle:?}",
        );
        let recorded = row["parser_admitted"].as_bool().expect("parser phase");
        let actual = parse_script(source, SOURCE_ID).is_ok();
        assert_eq!(
            actual, recorded,
            "parser outcome drift for seed '{id}': recorded {recorded}",
        );
        if row["supplemental"].as_bool().unwrap_or(false) {
            supplemental += 1;
            assert!(
                !recorded,
                "supplemental diagnostic copies stay negatives outside primary totals: {id}",
            );
        } else {
            primary += 1;
        }
        if id == "strict_duplicate_property" {
            strict_negative_seen = true;
            assert!(!recorded, "the strict-negative seed stays refused");
            assert_eq!(row["admission_kind"], "syntax");
        }
        // Lowering/execution phases stay explicitly deferred in this scope.
        assert!(row["compiled"].is_null() && row["executed"].is_null() && row["native"].is_null());
    }
    assert_eq!(primary, 91, "frozen primary count");
    assert_eq!(supplemental, 12, "frozen supplemental count");
    assert!(strict_negative_seen);
}

/// Decode one raw token with the shared codec (test helper).
fn decode(raw: &str, strict: bool) -> Result<Vec<u16>, super::string_literal::StringCodecError> {
    decode_string_literal(
        raw,
        if strict {
            StringStrictness::Strict
        } else {
            StringStrictness::Sloppy
        },
    )
}

fn decoded_ok(raw: &str, strict: bool) -> Vec<u16> {
    decode(raw, strict)
        .unwrap_or_else(|error| panic!("codec must accept {raw:?}: {}", error.message))
}

fn decoded_err_kind(raw: &str, strict: bool) -> (StringCodecErrorKind, usize, String) {
    match decode(raw, strict) {
        Ok(units) => panic!("codec must reject {raw:?}, got {units:?}"),
        Err(error) => (error.kind, error.relative_offset, error.message),
    }
}

/// Direct decoder coverage: ordinary characters, empty literals and the
/// ES5.1 section 7.8.4 single-escape table (no normalization involved).
#[test]
fn codec_plain_characters_and_single_escapes() {
    assert_eq!(decoded_ok("\"\"", false), Vec::<u16>::new());
    assert_eq!(decoded_ok("''", true), Vec::<u16>::new());
    assert_eq!(decoded_ok("\"abc\"", false), vec![0x61, 0x62, 0x63]);
    assert_eq!(decoded_ok("'abc'", false), vec![0x61, 0x62, 0x63]);
    // Raw non-BMP scalars encode to surrogate pairs without normalization.
    assert_eq!(decoded_ok("\"\u{1D11E}\"", false), vec![0xD834, 0xDD1E]);
    // Table 4, both quote styles.
    assert_eq!(
        decoded_ok("\"\\b\\t\\n\\v\\f\\r\"", false),
        vec![8, 9, 10, 11, 12, 13]
    );
    assert_eq!(decoded_ok("\"\\'\\\"\\\\\"", false), vec![0x27, 0x22, 0x5C]);
    assert_eq!(decoded_ok("'\\'\"\\\\'", false), vec![0x27, 0x22, 0x5C]);
    // NonEscapeCharacter identity escapes contribute the character itself.
    assert_eq!(
        decoded_ok("\"\\a\\q\\/\\$\\-\\_\\ \"", false),
        vec![0x61, 0x71, 0x2F, 0x24, 0x2D, 0x5F, 0x20]
    );
    // Escaped non-ASCII source characters contribute themselves.
    assert_eq!(decoded_ok("\"\\\u{00E9}\"", false), vec![0xE9]);
}

/// Decimal `8`/`9` are EscapeCharacters, never NonEscapeCharacters: no
/// ES5.1 or Annex-B derivation exists, in either mode.
#[test]
fn codec_decimal_escapes_rejected_in_both_modes() {
    for strict in [false, true] {
        for raw in ["\"\\8\"", "\"\\9\"", "\"a\\8b\""] {
            let (kind, offset, _) = decoded_err_kind(raw, strict);
            assert_eq!(kind, StringCodecErrorKind::Lexical, "for {raw:?}");
            assert_eq!(
                offset,
                raw.find('\\').unwrap(),
                "offset points at the backslash"
            );
        }
    }
}

/// Normative escape-zero plus the exact Annex B.1.2 octal productions with
/// their DecimalDigit lookaheads.
#[test]
fn codec_nul_and_sloppy_octal_productions() {
    // Normative `\0` (lookahead is not a DecimalDigit) in both modes.
    assert_eq!(decoded_ok("\"\\0\"", false), vec![0]);
    assert_eq!(decoded_ok("\"\\0\"", true), vec![0]);
    // Sloppy Annex-B derivations with exact values.
    assert_eq!(decoded_ok("\"\\1\"", false), vec![1]);
    assert_eq!(decoded_ok("\"\\7\"", false), vec![7]);
    assert_eq!(decoded_ok("\"\\01\"", false), vec![1]);
    assert_eq!(decoded_ok("\"\\00\"", false), vec![0]);
    assert_eq!(decoded_ok("\"\\12\"", false), vec![10]);
    assert_eq!(decoded_ok("\"\\123\"", false), vec![83]);
    assert_eq!(decoded_ok("\"\\40\"", false), vec![32]);
    // `\400` is the two-digit FourToSeven production plus ordinary `0`.
    assert_eq!(decoded_ok("\"\\400\"", false), vec![32, 48]);
    assert_eq!(decoded_ok("\"\\70\"", false), vec![56]);
    assert_eq!(decoded_ok("\"\\000\"", false), vec![0]);
    // Strict code rejects every legacy octal except normative `\0`.
    for raw in ["\"\\1\"", "\"\\01\"", "\"\\00\"", "\"\\12\"", "\"\\400\""] {
        let (kind, _, message) = decoded_err_kind(raw, true);
        assert_eq!(kind, StringCodecErrorKind::Lexical, "for {raw:?}");
        assert!(
            message.contains("strict mode"),
            "strict octal diagnostic, got: {message}"
        );
    }
    let (kind, _, _) = decoded_err_kind("\"\\08\"", true);
    assert_eq!(kind, StringCodecErrorKind::Lexical);
}

/// Forms without a permitted ES5/B.1.2 derivation: `\08`, `\09`, `\118`,
/// `\48` and friends are rejected, never given modern greedy-octal values.
#[test]
fn codec_invalid_octal_lookaheads_rejected() {
    for raw in [
        "\"\\08\"",
        "\"\\09\"",
        "\"\\118\"",
        "\"\\48\"",
        "\"\\78\"",
        "\"\\079\"",
    ] {
        let (kind, offset, _) = decoded_err_kind(raw, false);
        assert_eq!(kind, StringCodecErrorKind::Lexical, "for {raw:?}");
        assert_eq!(
            offset,
            raw.find('\\').unwrap(),
            "offset points at the backslash"
        );
    }
}

/// Hex and Unicode escapes consume exactly 2/4 hex digits and emit the u16
/// directly, preserving lone surrogates; braced escapes are post-ES5.
#[test]
fn codec_hex_and_unicode_escapes() {
    assert_eq!(decoded_ok("\"\\x41\"", false), vec![0x41]);
    assert_eq!(decoded_ok("\"\\x00\"", true), vec![0]);
    assert_eq!(decoded_ok("\"\\u0041\"", false), vec![0x41]);
    assert_eq!(decoded_ok("\"\\uD800\"", false), vec![0xD800]);
    assert_eq!(decoded_ok("\"\\uDC00\"", true), vec![0xDC00]);
    assert_eq!(decoded_ok("\"\\uFFFD\"", false), vec![0xFFFD]);
    assert_eq!(
        decoded_ok("\"\\uD834\\uDD1E\"", false),
        vec![0xD834, 0xDD1E]
    );
    for raw in [
        "\"\\x4\"",
        "\"\\xZZ\"",
        "\"\\x\"",
        "\"\\u123\"",
        "\"\\uZZZZ\"",
        "\"\\u\"",
    ] {
        let (kind, _, _) = decoded_err_kind(raw, false);
        assert_eq!(kind, StringCodecErrorKind::Lexical, "for {raw:?}");
    }
    for strict in [false, true] {
        let (kind, offset, message) = decoded_err_kind("\"\\u{41}\"", strict);
        assert_eq!(kind, StringCodecErrorKind::PostEs5, "braced is post-ES5");
        assert_eq!(offset, 1);
        assert!(
            message.contains("braced"),
            "braced guard wording, got: {message}"
        );
    }
    // An escaped backslash stays ordinary text: `\\u{...}` is six text units.
    assert_eq!(
        decoded_ok("\"\\\\u{41}\"", false),
        vec![0x5C, 0x75, 0x7B, 0x34, 0x31, 0x7D]
    );
}

/// Line continuations contribute nothing; unescaped line terminators and
/// raw separators reject; escaped `\u2028` stays a valid escape.
#[test]
fn codec_line_continuations_and_terminators() {
    assert_eq!(decoded_ok("\"a\\\nb\"", false), vec![0x61, 0x62]);
    assert_eq!(decoded_ok("\"a\\\rb\"", false), vec![0x61, 0x62]);
    assert_eq!(decoded_ok("\"a\\\r\nb\"", false), vec![0x61, 0x62]);
    assert_eq!(decoded_ok("\"a\\\u{2028}b\"", false), vec![0x61, 0x62]);
    assert_eq!(decoded_ok("\"a\\\u{2029}b\"", false), vec![0x61, 0x62]);
    assert_eq!(
        decoded_ok("\"\\\\u2028\"", false),
        vec![0x5C, 0x75, 0x32, 0x30, 0x32, 0x38]
    );
    assert_eq!(decoded_ok("\"\\u2028\"", false), vec![0x2028]);
    let (kind, _, message) = decoded_err_kind("\"a\u{2028}b\"", false);
    assert_eq!(kind, StringCodecErrorKind::Lexical);
    assert!(
        message.contains("2028"),
        "separator wording, got: {message}"
    );
    let (kind, _, message) = decoded_err_kind("\"a\u{2029}b\"", true);
    assert_eq!(kind, StringCodecErrorKind::Lexical);
    assert!(
        message.contains("2029"),
        "separator wording, got: {message}"
    );
    let (kind, _, _) = decoded_err_kind("\"a\nb\"", false);
    assert_eq!(kind, StringCodecErrorKind::Lexical);
}

/// Mixed single/hex/Unicode/identity escapes plus NUL decode to exact units.
#[test]
fn codec_mixed_escape_payload() {
    assert_eq!(
        decoded_ok("\"A\\0\\u00E9\\x42\\u0043\\q\"", false),
        vec![0x41, 0x00, 0xE9, 0x42, 0x43, 0x71]
    );
    // Mismatched delimiters and missing quotes are lexical errors.
    let (kind, _, _) = decoded_err_kind("\"abc'", false);
    assert_eq!(kind, StringCodecErrorKind::Lexical);
}

/// The unit bound stops decoding one past the limit as a resource error,
// distinct from lexical errors.
#[test]
fn codec_unit_bound_is_a_resource_error() {
    let bound = "a".repeat(super::string_literal::MAX_LITERAL_UTF16_UNITS);
    assert_eq!(
        decoded_ok(&format!("\"{bound}\""), false).len(),
        super::string_literal::MAX_LITERAL_UTF16_UNITS
    );
    let over = "a".repeat(super::string_literal::MAX_LITERAL_UTF16_UNITS + 1);
    let (kind, _, message) = decoded_err_kind(&format!("\"{over}\""), false);
    assert_eq!(kind, StringCodecErrorKind::Oversize);
    assert!(message.contains("16384"), "bound wording, got: {message}");
}

/// Codec offsets narrow to the offending escape inside the token span.
#[test]
fn codec_span_narrowing_stays_inside_the_token() {
    use swc_common::{BytePos, Span};
    let span = Span::new(BytePos(100), BytePos(110));
    let narrowed = narrowed_span(span, 3);
    assert_eq!((narrowed.lo.0, narrowed.hi.0), (103, 104));
    let clamped = narrowed_span(span, 500);
    assert!(clamped.lo.0 < clamped.hi.0 && clamped.hi.0 <= 110);
}

/// Post-ES5 braced escapes are rejected in every string token position the
/// admission walk visits, with the grammar-stage unsupported category.
#[test]
fn braced_escapes_rejected_in_all_string_positions() {
    for source in [
        "var s = \"\\u{41}\";",
        "\"\\u{41}\";",
        "function f(){ \"\\u{41}\"; }",
        "if (1) { \"\\u{41}\"; }",
        "while (1) { \"\\u{41}\"; }",
        "({\"\\u{41}\":1});",
        "var o = {a:1}; o[\"\\u{41}\"];",
        "var o = {a:1}; o[\"\\u{41}\"] = 2;",
        "switch (1) { case \"\\u{41}\": 2; }",
    ] {
        let error = refused_unsupported(source);
        assert!(
            error.message.contains("braced"),
            "braced guard wording for {source:?}, got: {error:?}"
        );
        check_span(source, &error);
    }
}

/// Forms without an ES5 derivation are SyntaxErrors in value, directive,
/// property-name, member-key and unlowered-body positions alike.
#[test]
fn lexical_string_errors_cover_all_positions() {
    for source in [
        "var s = \"\\8\";",
        "\"\\8\";",
        "function f(){ \"\\9\"; }",
        "if (1) { \"\\08\"; }",
        "({\"\\8\":1});",
        "var o = {a:1}; o[\"\\118\"];",
    ] {
        let error = refused_syntax(source);
        check_span(source, &error);
    }
    for source in [
        "var s = \"a\u{2028}b\";",
        "({\"a\u{2028}b\":1});",
        "var o = {a:1}; o[\"a\u{2029}b\"];",
    ] {
        let error = refused_syntax(source);
        assert!(
            error.message.contains("2028") || error.message.contains("2029"),
            "separator wording for {source:?}, got: {error:?}"
        );
        check_span(source, &error);
    }
}

/// Truncated hex/Unicode escapes are owned by the locked lexer
/// (`BadCharacterEscapeSequence` parse errors before admission runs). The
/// codec independently rejects the same spellings as lexical errors (see
/// `codec_hex_and_unicode_escapes`); these pins record the actual producer
/// layer so a future parser change fails loudly instead of silently
/// re-categorizing.
#[test]
fn truncated_hex_unicode_escapes_are_parser_owned() {
    for source in ["var s = \"\\x4\";", "var s = \"\\u12\";", "({\"\\x4\":1});"] {
        let error = refused(source);
        assert!(
            error.message.contains("parse error"),
            "locked lexer must own truncated escapes for {source:?}, got: {error:?}"
        );
        check_span(source, &error);
    }
}

/// Strictness is the whole containing code's resolved strictness: an early
/// legacy string plus a later exact directive still rejects, strictness
/// inherits into nested code, and a nested directive never leaks outward.
#[test]
fn string_strictness_follows_resolved_code() {
    refused("\"\\01\"; \"use strict\";");
    refused("\"use strict\"; function f(){ return \"\\01\"; }");
    refused("function f(){ \"\\01\"; \"use strict\"; }");
    refused("\"use strict\"; ({get a(){ return \"\\01\"; }});");
    // Escaped or parenthesized lookalikes never activate strict mode, so
    // the sloppy legacy spelling stays admitted around them.
    admitted("\"\\u0073trict\"; var s = \"\\01\";");
    admitted("(\"use strict\"); var s = \"\\01\";");
    admitted("1; \"use strict\"; var s = \"\\01\";");
    admitted("function f(){ \"use strict\"; } var s = \"\\01\";");
}
