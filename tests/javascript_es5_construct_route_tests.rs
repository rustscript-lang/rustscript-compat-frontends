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
