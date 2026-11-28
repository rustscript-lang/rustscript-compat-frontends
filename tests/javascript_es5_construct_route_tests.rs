//! `new` construction routing: the Script throw channel follows the route,
//! and the route itself only opens when the value cannot be proven safe.
//!
//! The three stages that must agree are the scan (`SourceScan`), the
//! validation walk (`validate_new_expr`) and the emitter (`lower_new_expr`).
//! All three consume one `ConstructRoute`, so a builtin spelling is never
//! "channel-free" by itself:
//!
//! * `new Array(-1)` / `new ArrayBuffer(-1)` / `new Uint8Array(-1)` answer the
//!   Script with a guest `Err`; without a Script-level `try` that is the
//!   uncaught guest throw, never a host assertion, and with one it is the
//!   ordinary guest error value.
//! * `new Object()`, `new Error("boom")` and `new Date(0)` keep the
//!   catalog-free compile: no `typescript::throw_error` entry is required.
//! * No TypedArray construction is provable. Uint8Array now admits an owned
//!   numeric length while the other eight classes still answer a numeric
//!   actual with the guest TypeError "unsupported object class". Every one
//!   of the nine routes therefore keeps the channel.
//! * A proven Array literal (`new Array()`, `new Array(1,2)`) and a proven
//!   ArrayBuffer length (`new ArrayBuffer(8)`) keep the catalog-free compile,
//!   because the route's `Err` branch is unreachable for them. A variable,
//!   expression, negative or out-of-range actual keeps the channel.
//! * The per-buffer ArrayBuffer quota is a fatal host error, never a guest
//!   catch value. A function-local `var B = ArrayBuffer` must not let the
//!   scan mistake `new B(32769)` for a source construction and so degrade
//!   that fatal error into a catchable RangeError.

const PROBE_FUEL: u64 = 40_000_000;

use pd_vm_compat_frontends::{
    JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor, reference_array_effective_catalog,
};

fn with_big_stack<T: Send + 'static>(body: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(body)
        .unwrap()
        .join()
        .unwrap()
}

/// The catalog this suite must stay independent of: it carries no
/// `typescript::throw_error` entry.
fn base_catalog() -> std::sync::Arc<vm::HostApiCatalog> {
    reference_array_effective_catalog()
}

fn base_options() -> vm::CompileSourceFileOptions {
    vm::CompileSourceFileOptions::new()
        .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN)
        .with_host_api_catalog(base_catalog())
}

/// Compile through the frontend only (parse + lower, no VM program build).
/// The `typescript::throw_error` install happens here, so this is the exact
/// refusal boundary the requirement names.
fn parse_with_catalog(source: &str, catalog: &std::sync::Arc<vm::HostApiCatalog>) -> Result<(), String> {
    let context = vm::SourceFrontendContext {
        source_id: 0,
        original_path: None,
        phase: vm::ParsePhase::Standalone,
        host_api_catalog: Some(catalog),
        imports: &[],
    };
    vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, &context)
        .map(|_| ())
        .map_err(|error| format!("{error:?}"))
}

fn parse_without_throw_error(source: &str) -> Result<(), String> {
    parse_with_catalog(source, &base_catalog())
}

/// The full production compile against a catalog without the adapter: the
/// same certified route the CLI and the embedding tests use.
fn compile_without_throw_error(source: &str) -> Result<(), String> {
    let source = source.to_owned();
    with_big_stack(move || {
        vm::compile_source_with_flavor_and_options_with_capture_certification(
            &source,
            javascript_es5_flavor(),
            base_options(),
        )
        .map(|_| ())
        .map_err(|error| format!("{error:?}"))
    })
}

/// One CLI-catalog run under the exact production compile/bind services.
/// `Ok` is the halted boxed Number, `Err` the exact refusal text.
fn run_outcome(source: &str) -> Result<f64, String> {
    let source = source.to_owned();
    with_big_stack(move || {
        let mut machine =
            pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                .unwrap_or_else(|error| panic!("source={source:?} {error:?}"));
        machine.set_jit_config(vm::JitConfig {
            enabled: false,
            ..*machine.jit_config()
        });
        machine.set_fuel(PROBE_FUEL);
        match machine.run() {
            Ok(vm::VmStatus::Halted) => {
                let last = machine
                    .stack()
                    .last()
                    .cloned()
                    .unwrap_or_else(|| panic!("source={source:?} halted with an empty stack"));
                let vm::Value::Map(map) = last else {
                    panic!("source={source:?} non-box result {last:?}");
                };
                match map.get(&vm::Value::string("number")) {
                    Some(vm::Value::Float(number)) => Ok(*number),
                    other => panic!("source={source:?} non-number box {other:?}"),
                }
            }
            Ok(other) => Err(format!("{other:?}")),
            Err(error) => Err(format!("{error:?}")),
        }
    })
}

fn run_number(source: &str) -> f64 {
    match run_outcome(source) {
        Ok(number) => number,
        Err(error) => panic!("source={source:?} must halt with a Number: {error}"),
    }
}

fn run_error(source: &str) -> String {
    match run_outcome(source) {
        Ok(number) => panic!("source={source:?} must not halt, got {number}"),
        Err(error) => error,
    }
}

/// Encode the observed `catch (e)` name/message pair as a bit mask so one
/// halted Number carries the whole guest throw through the box boundary.
/// Bits: 1 `RangeError` name, 2 "invalid array length", 4 "invalid array
/// index", 8 `TypeError` name, 16 "unsupported object class", 32
/// (RangeError, "invalid length").
fn catch_mask(construct: &str) -> String {
    format!(
        "var out=0;\
         try{{{construct}}}catch(e){{\
         if(e.name===\"RangeError\")out=out+1;\
         if(e.message===\"invalid array length\")out=out+2;\
         if(e.message===\"invalid array index\")out=out+4;\
         if(e.name===\"TypeError\")out=out+8;\
         if(e.message===\"unsupported object class\")out=out+16;\
         if(e.name===\"RangeError\"&&e.message===\"invalid length\")out=out+32;\
         }}out;"
    )
}

fn assert_catch_mask(construct: &str, expected: f64) {
    let source = catch_mask(construct);
    let observed = run_number(&source);
    assert_eq!(
        observed, expected,
        "guest throw for {construct}: expected mask {expected}, saw {observed}\n{source}"
    );
}

// --------------------------------------------------------------- probe --

const CATALOG_FREE_SOURCES: [&str; 12] = [
    "new Array();",
    "new Array(1,2);",
    "new ArrayBuffer(8);",
    "new Object();",
    "new Error(\"boom\");",
    "new Date(0);",
    "var n=1;new Array(n);",
    "new Array(-1);",
    "new ArrayBuffer(-1);",
    "new Uint8Array();",
    "new Uint8Array(4);",
    "new Uint8Array(-1);",
];

/// Print the whole matrix. Kept as its own test so `--nocapture` shows the
/// pre-fix and post-fix truth next to the strict assertions below.
#[test]
fn probe_construct_route_matrix() {
    eprintln!(
        "PROBE base catalog throw_error entries = {}",
        base_catalog().functions_named("typescript::throw_error").len()
    );
    for construct in ["new Array(-1);", "new ArrayBuffer(-1);", "new Uint8Array(-1);"] {
        eprintln!("PROBE uncaught {construct} -> {:?}", run_outcome(construct));
        eprintln!(
            "PROBE caught   {construct} -> {:?}",
            run_outcome(&catch_mask(construct))
        );
    }
    let brief = |outcome: &Result<(), String>| -> String {
        match outcome {
            Ok(()) => "ok".to_owned(),
            Err(error) => error.chars().take(110).collect(),
        }
    };
    for source in CATALOG_FREE_SOURCES {
        let parse = parse_without_throw_error(source);
        // A parse refusal is the same refusal the full compile reports, so the
        // VM-level build is only interesting for the accepted rows.
        let compile = if parse.is_ok() {
            brief(&compile_without_throw_error(source))
        } else {
            "skipped".to_owned()
        };
        eprintln!(
            "PROBE catalog-free {source:<24} parse={:<12} compile={compile}",
            brief(&parse),
        );
    }
}

// ------------------------------------------------------- route judgement --

#[test]
fn a1_uncaught_array_length_throw_is_the_script_guest_throw() {
    // Pinned: `new Array(-1)` outside `try` prints the guest RangeError; the
    // route may return a guest `Err`, so the Script owns the pending channel.
    let error = run_error("new Array(-1);");
    assert!(
        error.contains("uncaught JavaScript throw"),
        "new Array(-1) must publish the guest throw: {error}"
    );
}

#[test]
fn a2_uncaught_array_buffer_length_throw_is_the_script_guest_throw() {
    let error = run_error("new ArrayBuffer(-1);");
    assert!(
        error.contains("uncaught JavaScript throw"),
        "new ArrayBuffer(-1) must publish the guest throw: {error}"
    );
}

#[test]
fn a3_uncaught_typed_array_argument_throw_is_the_script_guest_throw() {
    let error = run_error("new Uint8Array(-1);");
    assert!(
        error.contains("uncaught JavaScript throw"),
        "new Uint8Array(-1) must publish the guest throw: {error}"
    );
}

#[test]
fn a4_caught_array_length_is_the_guest_range_error() {
    // RangeError + "invalid array length".
    assert_catch_mask("new Array(-1);", 3.0);
}

#[test]
fn a5_caught_array_buffer_length_is_the_guest_range_error() {
    // RangeError + "invalid array index".
    assert_catch_mask("new ArrayBuffer(-1);", 5.0);
}

#[test]
fn a6_caught_typed_array_argument_is_the_guest_error() {
    // Uint8Array now converts a primitive first actual as an owned length, so
    // -1 is the catchable RangeError "invalid array index". The route
    // judgement is unaffected: either way the construction answers the Script
    // with `Err`.
    assert_catch_mask("new Uint8Array(-1);", 5.0);
    // The other eight classes still reject a numeric actual with the guest
    // TypeError "unsupported object class" (8 + 16).
    assert_catch_mask("new Int8Array(-1);", 24.0);
}

#[test]
fn a7_object_error_date_need_no_throw_error_entry() {
    // The closed routes must not start depending on `typescript::throw_error`:
    // this catalog does not carry it, and these three keep compiling.
    assert!(
        base_catalog()
            .functions_named("typescript::throw_error")
            .is_empty(),
        "the base catalog used for this comparison must not carry the adapter"
    );
    for source in ["new Object();", "new Error(\"boom\");", "new Date(0);"] {
        parse_without_throw_error(source)
            .unwrap_or_else(|error| panic!("{source} must stay catalog-free: {error}"));
    }
}

// ------------------------------------------------------- proven values --

#[test]
fn b1_empty_array_construct_is_proven() {
    parse_without_throw_error("new Array();")
        .unwrap_or_else(|error| panic!("new Array() must stay catalog-free: {error}"));
    compile_without_throw_error("new Array();")
        .unwrap_or_else(|error| panic!("new Array() must stay catalog-free: {error}"));
}

#[test]
fn b2_literal_element_list_is_proven() {
    parse_without_throw_error("new Array(1,2);")
        .unwrap_or_else(|error| panic!("new Array(1,2) must stay catalog-free: {error}"));
    compile_without_throw_error("new Array(1,2);")
        .unwrap_or_else(|error| panic!("new Array(1,2) must stay catalog-free: {error}"));
}

#[test]
fn b3_literal_typed_array_argument_keeps_the_channel() {
    // No TypedArray shape is a proven-safe construction: Uint8Array still
    // answers a negative or oversized owned length with a guest RangeError,
    // and the other eight classes still answer any numeric actual with the
    // guest TypeError "unsupported object class" (prelude/typed_array.rs).
    // The route must keep the Script channel for all nine element classes.
    for element in [
        "Uint8Array",
        "Int8Array",
        "Uint8ClampedArray",
        "Int16Array",
        "Uint16Array",
        "Int32Array",
        "Uint32Array",
        "Float32Array",
        "Float64Array",
    ] {
        for arguments in ["", "4"] {
            let source = format!("new {element}({arguments});");
            let error = parse_without_throw_error(&source)
                .expect_err("a TypedArray route must keep the Script channel");
            assert!(
                error.contains("typescript::throw_error"),
                "{source} must name the adapter: {error}"
            );
        }
    }
    // The full production compile refuses the same two named shapes; a parse
    // refusal is the same boundary, so one full compile pins it.
    for source in ["new Uint8Array();", "new Uint8Array(4);"] {
        let error = compile_without_throw_error(source)
            .expect_err("the full compile must refuse without the adapter");
        assert!(
            error.contains("typescript::throw_error"),
            "{source} must name the adapter: {error}"
        );
    }
}

#[test]
fn b4_literal_array_buffer_length_is_proven() {
    parse_without_throw_error("new ArrayBuffer(8);")
        .unwrap_or_else(|error| panic!("new ArrayBuffer(8) must stay catalog-free: {error}"));
    compile_without_throw_error("new ArrayBuffer(8);")
        .unwrap_or_else(|error| panic!("new ArrayBuffer(8) must stay catalog-free: {error}"));
}

#[test]
fn b5_proven_array_construct_binds_and_halts() {
    // The catalog-free program must still bind and run: the length is a Number
    // result, not an Err that needs the adapter.
    assert_eq!(run_number("var a=new Array(1,2);a.length;"), 2.0);
}

#[test]
fn b6_unproven_actual_keeps_the_channel() {
    let error = parse_without_throw_error("var n=1;new Array(n);")
        .expect_err("a variable actual must keep the Script channel");
    assert!(
        error.contains("typescript::throw_error"),
        "the unproven route must name the adapter: {error}"
    );
    let compile = compile_without_throw_error("var n=1;new Array(n);")
        .expect_err("the full compile must refuse without the adapter");
    assert!(
        compile.contains("typescript::throw_error"),
        "the full compile must name the adapter: {compile}"
    );
}

#[test]
fn b7_negative_and_out_of_range_literals_keep_the_channel() {
    for source in [
        "new Array(-1);",
        "new ArrayBuffer(-1);",
        "new Uint8Array(-1);",
        "new Array(4294967295);",
        "new ArrayBuffer(32769);",
    ] {
        let error = parse_without_throw_error(source)
            .err()
            .unwrap_or_else(|| panic!("{source} must keep the channel"));
        assert!(
            error.contains("typescript::throw_error"),
            "{source} must name the adapter: {error}"
        );
    }
}

// -------------------------------------- fatal ArrayBuffer quota / scope --

/// Print the pre-fix and post-fix truth for the two review blockers next to
/// the strict assertions below.
#[test]
fn probe_fatal_quota_and_typed_array_matrix() {
    for construct in ["new Uint8Array();", "new Uint8Array(4);"] {
        eprintln!(
            "PROBE typed-array uncaught {construct:<20} -> {:?}",
            run_outcome(construct)
        );
        eprintln!(
            "PROBE typed-array caught   {construct:<20} -> {:?}",
            run_outcome(&catch_mask(construct))
        );
    }
    for source in [
        "function f(){var B=ArrayBuffer;try{new B(32769);}catch(e){return 1;}return 0;}f();",
        "function f(){try{new ArrayBuffer(32769);}catch(e){return 1;}return 0;}f();",
    ] {
        eprintln!("PROBE fatal-quota {source} -> {:?}", run_outcome(source));
    }
}

#[test]
fn b8_function_local_array_buffer_alias_keeps_the_fatal_quota() {
    // The function-local alias is still `ArrayBuffer` at the emitter, but the
    // scan walks the body without the function's lexical scope. If it treats
    // the site as a source construction the helper is built without the
    // adapter, and the fatal quota error silently becomes a catchable guest
    // RangeError. The catch must therefore never run.
    let source =
        "function f(){var B=ArrayBuffer;try{new B(32769);}catch(e){return 1;}return 0;}f();";
    let error = run_error(source);
    assert!(
        error.contains("array buffer length exceeds host quota"),
        "a function-local ArrayBuffer alias must keep the fatal quota error: {error}"
    );
}

#[test]
fn b9_function_local_direct_array_buffer_quota_is_fatal() {
    // The direct spelling is the control for b8: its route is already
    // ArrayBuffer, so the adapter must be installed and the same guest catch
    // must not see the error.
    let source =
        "function f(){try{new ArrayBuffer(32769);}catch(e){return 1;}return 0;}f();";
    let error = run_error(source);
    assert!(
        error.contains("array buffer length exceeds host quota"),
        "the direct ArrayBuffer quota must stay a fatal host error: {error}"
    );
}

#[test]
fn c1_typed_array_constructs_use_the_script_throw_channel() {
    // With the adapter installed, a negative owned Uint8 length publishes the
    // ordinary uncaught guest RangeError and is catchable as "invalid array
    // index" (1 + 4). The other eight classes keep the guest TypeError
    // "unsupported object class" (8 + 16). Without the route's channel both
    // would abort with a host assertion instead.
    for (source, mask) in [
        ("new Uint8Array(-1);", 5.0),
        ("new Int8Array(4);", 24.0),
        ("new Float64Array(4);", 24.0),
    ] {
        let error = run_error(source);
        assert!(
            error.contains("uncaught JavaScript throw"),
            "{source} must publish the guest throw: {error}"
        );
        assert_catch_mask(source, mask);
    }
}

#[test]
fn c2_array_length_proof_boundary_is_4096() {
    // 4096 is the last count every guest route admits inside the ArrayBuffer
    // byte budget; 4097 is one past it and must not be proven.
    for source in ["new Array(4096);", "new Array(4096,4096);"] {
        parse_without_throw_error(source)
            .unwrap_or_else(|error| panic!("{source} must stay catalog-free: {error}"));
        compile_without_throw_error(source)
            .unwrap_or_else(|error| panic!("{source} must stay catalog-free: {error}"));
    }
    let source = "new Array(4097);";
    let error = parse_without_throw_error(source)
        .expect_err("4097 must keep the Script channel");
    assert!(
        error.contains("typescript::throw_error"),
        "{source} must name the adapter: {error}"
    );
    let compile = compile_without_throw_error(source)
        .expect_err("the full compile must refuse without the adapter");
    assert!(
        compile.contains("typescript::throw_error"),
        "{source} must name the adapter: {compile}"
    );
}

// -------------------------------- owned Uint8 quota vs. realm demand --
#[test]
fn b10_owned_uint8_safe_length_ignores_unknown_tail_for_the_quota() {
    // The owned backing quota only reads the first actual. The safe literal 4
    // proves the backing inside the byte budget, and a trailing actual is
    // evaluated without being converted, so it cannot reach the fatal quota.
    // The ArrayBuffer realm is still installed for the owned backing's
    // prototype pair; that realm fact must not demand the fatal adapter by
    // itself.
    // The runtime `.buffer.byteLength === 4` receipt lives in the owned
    // interpreter suite: a Script-level call would open the uncaught-throw
    // channel and legitimately require the adapter for another reason.
    let source = "function f(x){ return new Uint8Array(4, x); }";
    parse_without_throw_error(source)
        .unwrap_or_else(|error| panic!("{source} must stay catalog-free: {error}"));
    compile_without_throw_error(source)
        .unwrap_or_else(|error| panic!("{source} must stay catalog-free: {error}"));
}

#[test]
fn b11_owned_uint8_first_actual_keeps_the_quota_gate() {
    // The first actual is the only owned-length input: a proven 32769 and an
    // unknown variable both need the fatal quota adapter, and the separate
    // owned route must name its own adapter classification.
    for source in [
        "function f(x){ return new Uint8Array(32769, x); }",
        "function f(x){ return new Uint8Array(x); }",
    ] {
        let error = compile_without_throw_error(source)
            .expect_err("an unproven owned length needs the fatal quota adapter");
        assert!(
            error.contains("JavaScript Uint8Array quota handling"),
            "{source} must classify the owned quota: {error}"
        );
        assert!(
            !error.contains("JavaScript ArrayBuffer quota handling"),
            "{source} must not classify the realm install as a public ArrayBuffer quota: {error}"
        );
    }
    // A function-local ArrayBuffer alias still needs the public ArrayBuffer
    // quota adapter when its first actual is unproven.
    let source = "function f(x){ var B = ArrayBuffer; return new B(x); }";
    let error = compile_without_throw_error(source)
        .expect_err("a function-local ArrayBuffer alias needs the fatal quota adapter");
    assert!(
        error.contains("JavaScript ArrayBuffer quota handling"),
        "{source} must classify the public ArrayBuffer quota: {error}"
    );
    // A script-level alias used from a function body stays a conservative
    // refusal (free-variable capture), never a catalog-free construction.
    let source = "var B = ArrayBuffer; function f(x){ return new B(x); }";
    compile_without_throw_error(source)
        .expect_err("a captured ArrayBuffer alias must stay refused");
}

#[test]
fn b12_owned_uint8_first_actual_domain_matches_the_runtime() {
    // The owned route converts only the first actual, truncates toward zero,
    // and decides the guest index domain before the byte quota. These domains
    // therefore keep the catalog-free compile in a deferred body.
    for source in [
        "function f(){ return new Uint8Array().length; }",
        "function f(){ return new Uint8Array(undefined).length; }",
        "function f(){ return new Uint8Array(null).length; }",
        "function f(){ return new Uint8Array(false).length; }",
        "function f(){ return new Uint8Array(true).length; }",
        "function f(){ return new Uint8Array(NaN).length; }",
        "function f(){ return new Uint8Array(\"junk\").length; }",
        "function f(){ return new Uint8Array(32768).length; }",
        "function f(){ return new Uint8Array(32768.9).length; }",
        "function f(){ return new Uint8Array(-0.9).length; }",
        // The guest index domain wins before the quota branch.
        "function f(){ return new Uint8Array(-1).length; }",
        "function f(){ return new Uint8Array(-1.9).length; }",
        "function f(){ return new Uint8Array(Infinity).length; }",
        "function f(){ return new Uint8Array(-Infinity).length; }",
        "function f(){ return new Uint8Array(1073741824).length; }",
        "function f(){ return new Uint8Array(1073741825).length; }",
    ] {
        parse_without_throw_error(source)
            .unwrap_or_else(|error| panic!("{source} must stay catalog-free: {error}"));
    }
    // The fatal quota range still demands its own adapter classification.
    for source in [
        "function f(){ return new Uint8Array(32769).length; }",
        "function f(){ return new Uint8Array(32769.9).length; }",
        "function f(){ return new Uint8Array(40000).length; }",
        "function f(){ return new Uint8Array(1073741823).length; }",
        "function f(x){ return new Uint8Array(x).length; }",
    ] {
        let error = parse_without_throw_error(source)
            .expect_err("a quota-range owned length needs the fatal adapter");
        assert!(
            error.contains("JavaScript Uint8Array quota handling"),
            "{source} must classify the owned quota: {error}"
        );
    }
}

// ------------------- catch handler lexical scope vs. the fatal quota --

/// The catch parameter shadows the builtin `NaN`, so the first actual is the
/// captured number and emission reads a reachable fatal quota. The scan must
/// enter the same catch scope validation and emission use; answering from
/// the enclosing function scope would mistake the spelling for the builtin
/// NaN and conclude that the quota branch is unreachable.
const CATCH_SHADOW_NAN_QUOTA: &str = "function f() {\
     try { throw 32769; }\
     catch (NaN) {\
       try { new ArrayBuffer(NaN); }\
       catch (e) { return 1; }\
     }\
     return 0;\
   }";

#[test]
fn d1_catch_shadowed_nan_needs_the_public_array_buffer_quota_adapter() {
    // No Script-position call or throw: the only adapter demand is the
    // reachable public ArrayBuffer quota, so the missing-catalog refusal
    // carries that classification and not the uncaught-throw one.
    let error = compile_without_throw_error(CATCH_SHADOW_NAN_QUOTA)
        .expect_err("the captured NaN must make the public ArrayBuffer quota reachable");
    assert!(
        error.contains("JavaScript ArrayBuffer quota handling"),
        "the refusal must be classified as the public ArrayBuffer quota: {error}"
    );
    assert!(
        !error.contains("JavaScript Uint8Array quota handling"),
        "the public route must not be classified as the owned Uint8 quota: {error}"
    );
}

#[test]
fn d2_catch_shadowed_nan_quota_is_a_fatal_host_error() {
    let source = format!("{CATCH_SHADOW_NAN_QUOTA}\nf();");
    let error = run_error(&source);
    assert!(
        error.contains("HostError(\"array buffer length exceeds host quota\")"),
        "the captured 32769 must reach the fatal host quota: {error}"
    );
    assert!(
        !error.contains("uncaught JavaScript throw"),
        "no guest catch may observe the fatal quota: {error}"
    );
}

#[test]
fn d3_catch_handler_scope_restores_after_the_handler() {
    // After the handler the function scope is back: `NaN` is the builtin
    // again, so this construction cannot reach the fatal quota. A scan that
    // leaks the catch scope past the handler refuses this source.
    let source = "function f() {\
        try { throw 1; } catch (NaN) { }\
        new ArrayBuffer(NaN);\
        return 0;\
    }";
    compile_without_throw_error(source)
        .unwrap_or_else(|error| panic!("the handler scope must end at the handler: {error}"));
}

#[test]
fn d4_nested_catch_handlers_resolve_their_own_scopes() {
    // The inner handler body resolves `NaN` through its own catch scope and
    // then the outer catch binding, so the first actual is unproven. The
    // enclosing function scope would still spell the builtin NaN.
    let source = "function f() {\
        try { throw 1; } catch (NaN) {\
          try { throw 2; } catch (e) { new ArrayBuffer(NaN); }\
        }\
        return 0;\
    }";
    let error = compile_without_throw_error(source)
        .expect_err("the inner handler must resolve NaN through the outer catch binding");
    assert!(
        error.contains("JavaScript ArrayBuffer quota handling"),
        "the nested handler must reach the public ArrayBuffer quota: {error}"
    );
}

#[test]
fn d5_catch_parameter_shadowing_array_buffer_is_not_the_builtin_route() {
    // The inner catch parameter shadows the callee spelling, so the scan must
    // not classify this `new` as the builtin public ArrayBuffer route. The
    // source-construction slice itself refuses a catch parameter as callee,
    // but the refusal must stay that admitted-shape diagnostic and must not
    // become a false fatal-quota demand for the builtin route.
    let source = "function f(x) {\
        try { throw 1; } catch (e) {\
          try { throw 2; } catch (ArrayBuffer) { new ArrayBuffer(x); }\
        }\
        return 0;\
    }";
    let error = compile_without_throw_error(source)
        .expect_err("a catch parameter is not an admitted source constructor");
    assert!(
        error.contains("unsupported JavaScript ES5 P0 primitive expression"),
        "the shadowed callee must keep the source-construct refusal: {error}"
    );
    assert!(
        !error.contains("JavaScript ArrayBuffer quota handling"),
        "the shadowed callee must not be classified as the builtin public route: {error}"
    );
}
