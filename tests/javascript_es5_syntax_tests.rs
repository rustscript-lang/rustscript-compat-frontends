//! Syntax/admission integration tests over the existing public compile APIs.
//!
//! These tests distinguish parser-stage refusal (admission categories) from
//! later P0 lowering refusal: admission-refused sources carry `unsupported
//! syntax` / `SyntaxError` diagnostics, while grammar-admitted but
//! unlowered ES5 forms fail with the lowerer's own `P0` diagnostics. Parser
//! admission success is never reported as runtime success here.

use pd_vm_compat_frontends::{javascript_es5_compile_options, javascript_es5_flavor};
use vm::compile_source_with_flavor_and_options;

fn compile_err(source: &str) -> String {
    match compile_source_with_flavor_and_options(
        source,
        javascript_es5_flavor(),
        javascript_es5_compile_options(),
    ) {
        Ok(_) => panic!("source must be refused: {source:?}"),
        Err(error) => error.to_string(),
    }
}

fn compile_ok(source: &str) {
    compile_source_with_flavor_and_options(
        source,
        javascript_es5_flavor(),
        javascript_es5_compile_options(),
    )
    .unwrap_or_else(|error| panic!("P0 source must still compile: {source:?}\n{error:?}"));
}

#[test]
fn post_es5_declarations_fail_at_admission() {
    for source in [
        "let x = 1; x;",
        "const x = 1; x;",
        "() => 1;",
        "class C {}",
        "`hi`;",
        "2 ** 3;",
        "x ?? y;",
        "for (var x of y) {}",
        "var o = {a};",
        "function* g(){ yield 1; }",
        "0o17;",
        "1_000;",
    ] {
        let text = compile_err(source);
        assert!(
            text.contains("unsupported syntax"),
            "admission must refuse {source:?} before lowering, got: {text}",
        );
    }
}

#[test]
fn strict_early_errors_surface_through_compile() {
    let text = compile_err("\"use strict\"; ({a:1,a:2});");
    assert!(
        text.contains("SyntaxError") && text.to_ascii_lowercase().contains("duplicate"),
        "strict duplicate data properties must surface the early error, got: {text}",
    );
    let text = compile_err("\"use strict\"; function f(a,a){ return a; }");
    assert!(
        text.contains("SyntaxError"),
        "strict duplicate parameters must surface the early error, got: {text}",
    );
}

#[test]
fn admitted_forms_reach_lowering_instead() {
    // Sloppy `with`, functions and object literals pass grammar admission;
    // P0 lowering refuses them with its own diagnostics (no admission
    // category leaks into those errors).
    for source in [
        "with (x) { y; }",
        "function f(){ return 1; }",
        "({a:1});",
        "for (var k in obj) { k; }",
    ] {
        let text = compile_err(source);
        assert!(
            !text.contains("unsupported syntax") && !text.contains("SyntaxError"),
            "admitted source must reach lowering, not admission refusal: {source:?}\n{text}",
        );
    }
}

#[test]
fn raw_parse_errors_keep_their_category() {
    let text = compile_err("var = ;");
    assert!(
        text.contains("parse error"),
        "raw lexer failures stay parse errors, got: {text}",
    );
}

#[test]
fn spans_survive_public_compile() {
    let source = "\"use strict\"; ({a:1,a:2});";
    let error = compile_source_with_flavor_and_options(
        source,
        javascript_es5_flavor(),
        javascript_es5_compile_options(),
    )
    .err()
    .expect("strict duplicates must be refused");
    let text = error.to_string();
    assert!(
        text.contains("duplicate"),
        "the early error survives public compile, got: {text}",
    );
    let vm::SourcePathError::SourceWithMap { error, .. } = error else {
        panic!("admission refusal stays a source parse error, got: {error:?}");
    };
    let vm::SourceError::Parse(parse) = error else {
        panic!("admission refusal stays a parse error, got: {error:?}");
    };
    let span = parse.span.expect("admission diagnostics carry spans");
    assert!(
        span.hi <= source.len() && span.lo < span.hi,
        "span is a byte range inside the source, got: {span:?}",
    );
}

#[test]
fn p0_smoke_sources_still_compile() {
    compile_ok("1 / 2;");
    compile_ok("var x = 1 / 2; var y = x; x === y;");
}

#[test]
fn braced_unicode_identifiers_fail_at_admission() {
    // Public compile-path regression: post-ES5 braced escapes in identifier
    // positions once compiled cleanly end to end (the lowerer binds the
    // cooked name, so this is a lexical hole, not a future feature).
    for source in ["var \\u{41}=1;", "var A=1;var x=\\u{41};x;"] {
        let text = compile_err(source);
        assert!(
            text.contains("unsupported syntax"),
            "braced identifier escapes must fail at admission, got: {text}",
        );
    }
    // Fixed four-hex escapes stay legal, including end to end.
    compile_ok("var \\u0041=1;");
    compile_ok("var A = 1; var x = A; x === 1;");
}
