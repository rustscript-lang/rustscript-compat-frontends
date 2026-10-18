//! P2-A value/literal fidelity: original-source ES5 decoding with direct
//! primitive/UTF-16 observations.
//!
//! The shared raw-UTF16 codec (ES5.1 7.8.4 plus the retained sloppy
//! Annex-B octal extension) feeds both admission and lowering. This target
//! pins: immediate-value payloads from actual boxed fields, runtime string
//! units/length read from the live guest realm carrier (interpreted plus
//! one native-SSA mixed observation), property-name decoding through
//! execution, admission negatives in every string position, and resolved
//! strict/sloppy policy. Earlier RED seams (`p2-value-red*` gates) failed
//! before the codec landed and pass with it; retained controls stayed green
//! throughout.

use pd_vm_compat_frontends::{JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor};
use vm::{HostFunctionRegistry, Value, Vm, VmStatus};

// ---------------------------------------------------------------------------
// Realm compile helpers (narrowly duplicated recipe, labeled).
// ---------------------------------------------------------------------------

fn realm_catalog() -> std::sync::Arc<vm::HostApiCatalog> {
    pd_vm_compat_frontends::reference_array_effective_catalog()
}

fn realm_options(
    plugin: &'static dyn vm::SourcePlugin,
    catalog: &std::sync::Arc<vm::HostApiCatalog>,
) -> vm::CompileSourceFileOptions {
    vm::CompileSourceFileOptions::new()
        .with_source_plugin(plugin)
        .with_host_api_catalog(std::sync::Arc::clone(catalog))
}

fn install_production(registry: &mut HostFunctionRegistry, catalog: &vm::HostApiCatalog) {
    pd_vm_compat_frontends::runtime::array::typescript_array_host_module()
        .install_from_catalog(registry, catalog)
        .expect("manual arrays adapters should match the effective catalog");
    pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module()
        .install_from_catalog(registry, catalog)
        .expect("owned adapters should match the effective catalog");
    pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module()
        .install_from_catalog(registry, catalog)
        .expect("snapshot adapter should match the effective catalog");
    pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module()
        .install_from_catalog(registry, catalog)
        .expect("scalar-math adapter should match the effective catalog");
    registry
        .allow_builtin("math::floor")
        .expect("math::floor should be allow-listable");
}

fn realm_vm(
    source: &str,
    plugin: &'static dyn vm::SourcePlugin,
    flavor: vm::SourceFlavor,
    native: bool,
) -> Vm {
    let catalog = realm_catalog();
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        flavor,
        realm_options(plugin, &catalog),
    )
    .expect("JS value compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = HostFunctionRegistry::restricted();
    install_production(&mut registry, &catalog);
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    if native {
        machine
            .compile_aot()
            .expect("native compilation should succeed");
    }
    machine
}

fn compile_outcome(source: &str) -> Result<(), String> {
    let catalog = realm_catalog();
    match vm::compile_source_with_flavor_and_options(
        source,
        javascript_es5_flavor(),
        realm_options(&JAVASCRIPT_ES5_PLUGIN, &catalog),
    ) {
        Ok(_) => Ok(()),
        Err(error) => Err(format!("{error:?}")),
    }
}

fn must_reject_with(source: &str, fragment: &str) -> String {
    match compile_outcome(source) {
        Ok(()) => panic!("P2-A: invalid ES5 string seam admitted: {source:?}"),
        Err(text) => {
            assert!(
                text.contains(fragment),
                "P2-A: {source:?} rejected at the wrong layer, want {fragment:?}, got: {text}"
            );
            text
        }
    }
}

// ---------------------------------------------------------------------------
// RED seams: genuinely failing before the shared codec, green with it.
// ---------------------------------------------------------------------------

#[test]
fn p2_red_sloppy_decimal_escapes_are_rejected() {
    // ES5.1 7.8.4: `8`/`9` are DecimalDigits, hence EscapeCharacters, hence
    // never NonEscapeCharacters. No ES5 or Annex-B production derives them.
    for source in ["var s = \"\\8\"; s===s;", "var s = \"\\9\"; s===s;"] {
        must_reject_with(source, "SyntaxError");
    }
}

#[test]
fn p2_red_sloppy_octal_lookahead_cases_are_rejected() {
    // Annex B.1.2 lookaheads: `\08`, `\09` and `\118` have no permitted
    // ES5/B.1.2 derivation (modern greedy-octal behavior must not apply).
    for source in [
        "var s = \"\\08\"; s===s;",
        "var s = \"\\09\"; s===s;",
        "var s = \"\\118\"; s===s;",
    ] {
        must_reject_with(source, "SyntaxError");
    }
}

#[test]
fn p2_red_braced_escape_in_unlowered_directive_is_rejected() {
    // The braced escape sits in a function directive prologue the lowerer
    // skips without decoding; admission must own this position.
    let source = "var f = function(){ \"\\u{41}\"; \"use strict\"; return 1; }; f()===1;";
    let text = must_reject_with(source, "unsupported syntax");
    assert!(
        text.contains("braced"),
        "must hit the braced guard, got: {text}"
    );
}

#[test]
fn p2_red_strict_octal_property_name_is_rejected() {
    // Sloppy decimal escapes in property names bypass the old
    // value-position guard (canonicalization reads the cooked payload
    // without validating it); ES5 has no derivation for `\8`.
    let source = "({\"\\8\":1});";
    must_reject_with(source, "SyntaxError");
}

#[test]
fn p2_red_post_es5_in_unlowered_branch_rejects_at_admission() {
    // `if` bodies are admitted-but-unlowered; the post-ES5 token must fail
    // at admission with the grammar-stage category, not the P0 lowering one.
    let source = "if (1) { \"\\u{41}\"; }";
    let text = must_reject_with(source, "unsupported syntax");
    assert!(
        text.contains("braced"),
        "must hit the braced guard, got: {text}"
    );
}

#[test]
fn p2_parser_owned_strict_octal_property_name() {
    // The locked lexer owns strict legacy-octal diagnostics outright
    // (`LegacyOctal` parse error before admission runs); recorded as
    // parser-owned coverage, never rewritten as a codec execution.
    let source = "\"use strict\"; ({\"\\01\":1});";
    match compile_outcome(source) {
        Ok(()) => panic!("locked parser must own strict legacy-octal property names: {source:?}"),
        Err(text) => assert!(
            text.contains("LegacyOctal") || text.contains("parse error"),
            "parser-owned strict-octal refusal, got: {text}"
        ),
    }
}

// ---- Retained controls: already correct, stay green ----

#[test]
fn p2_control_sloppy_two_digit_octal_stays_admitted() {
    // `\01` is a genuine Annex-B ZeroToThree+OctalDigit derivation.
    assert!(
        compile_outcome("var s = \"\\01\"; s===s;").is_ok(),
        "sloppy \\01 must stay admitted"
    );
}

#[test]
fn p2_control_strict_escape_zero_stays_admitted() {
    // Normative `EscapeSequence :: 0 [lookahead not DecimalDigit]`.
    assert!(
        compile_outcome("\"use strict\"; var s = \"\\0\"; s===s;").is_ok(),
        "strict \\0 must stay admitted"
    );
}

#[test]
fn p2_control_strict_canonical_collision_stays_rejected() {
    let text = must_reject_with("\"use strict\"; ({\"a\":1,\"\\u0061\":2});", "SyntaxError");
    assert!(
        text.contains("duplicate data property"),
        "must keep the duplicate-property early error, got: {text}"
    );
}

// ---------------------------------------------------------------------------
// Immediate values: actual boxed fields, never status/debug strings.
// ---------------------------------------------------------------------------

/// Run a heap-free source and return the raw boxed top-of-stack value for
/// direct field assertions.
fn run_boxed(source: &str) -> Value {
    let compiled = vm::compile_source_with_flavor_and_options(
        source,
        javascript_es5_flavor(),
        vm::CompileSourceFileOptions::new().with_source_plugin(&JAVASCRIPT_ES5_PLUGIN),
    )
    .unwrap_or_else(|error| panic!("primitive JS source must compile: {error:?}"));
    let mut machine = Vm::new(compiled.program);
    assert_eq!(
        machine.run().expect("JS source should execute"),
        VmStatus::Halted
    );
    machine.stack().last().cloned().expect("expression result")
}

/// Run a heap-free source and return the unboxed JS primitive value,
/// mirroring the bridge shape (boxed records unbox through tag/flag).
fn run_unboxed(source: &str) -> Value {
    let value = run_boxed(source);
    let Value::Map(fields) = &value else {
        panic!("expected the actual boxed JS primitive, got {value:?}");
    };
    match fields.get(&Value::string("tag")) {
        Some(Value::Int(3)) => field(&value, "number"),
        Some(Value::Int(2)) => field(&value, "flag"),
        Some(Value::Int(0)) | Some(Value::Int(1)) => value,
        other => panic!("unexpected primitive tag: {other:?}"),
    }
}

fn boxed_map(value: Value) -> Value {
    match &value {
        Value::Map(_) => value,
        other => panic!("expected the actual boxed JS primitive, got {other:?}"),
    }
}

/// Read one field from an actual boxed JS value (mirrors the bridge shape).
fn field(map: &Value, key: &str) -> Value {
    let Value::Map(fields) = map else {
        panic!("expected the actual boxed JS primitive, got {map:?}");
    };
    fields
        .get(&Value::string(key))
        .cloned()
        .unwrap_or_else(|| panic!("boxed JS value is missing field {key:?}"))
}

#[test]
fn p5_red_runtime_truthiness_scalars() {
    assert_eq!(run_unboxed("!undefined;"), Value::Bool(true));
    assert_eq!(run_unboxed("!null;"), Value::Bool(true));
    assert_eq!(run_unboxed("!false;"), Value::Bool(true));
    assert_eq!(run_unboxed("!true;"), Value::Bool(false));
    assert_eq!(run_unboxed("!-0;"), Value::Bool(true));
    assert_eq!(run_unboxed("!(0/0);"), Value::Bool(true));
    assert_eq!(run_unboxed("!(1/0);"), Value::Bool(false));
}

#[test]
fn p5_red_short_circuit_preserves_operand_and_side_effects() {
    assert_eq!(run_unboxed("1 || 7;"), Value::Float(1.0));
    assert_eq!(run_unboxed("0 && 7;"), Value::Float(0.0));
    assert_eq!(
        run_unboxed("var x=0; false && (x=1); x;"),
        Value::Float(0.0)
    );
    assert_eq!(
        run_unboxed("var x=0; (x=x+1) || (x=x+2); x;"),
        Value::Float(1.0)
    );
}

#[test]
fn p2_immediate_undefined_and_null_tags_differ() {
    let undefined = boxed_map(run_boxed("undefined;"));
    let null = boxed_map(run_boxed("null;"));
    assert_eq!(field(&undefined, "tag"), Value::Int(0));
    assert_eq!(field(&null, "tag"), Value::Int(1));
    assert_ne!(
        field(&undefined, "tag"),
        field(&null, "tag"),
        "Undefined and Null carry distinct tags"
    );
}

#[test]
fn p2_immediate_negative_zero_keeps_sign() {
    let negative = boxed_map(run_boxed("-0;"));
    assert_eq!(field(&negative, "tag"), Value::Int(3));
    let Value::Float(negative_zero) = field(&negative, "number") else {
        panic!("number payload must stay Float");
    };
    assert_eq!(negative_zero, 0.0);
    assert!(
        negative_zero.is_sign_negative(),
        "negative-zero sign must survive lowering, got {negative_zero:?}"
    );
    assert_eq!(run_unboxed("-0 === 0;"), Value::Bool(true));
}

#[test]
fn p2_immediate_nan_is_not_equal_to_itself() {
    let nan_value = boxed_map(run_boxed("0 / 0;"));
    assert_eq!(field(&nan_value, "tag"), Value::Int(3));
    let Value::Float(nan) = field(&nan_value, "number") else {
        panic!("number payload must stay Float");
    };
    assert!(nan.is_nan(), "0/0 must observe NaN, got {nan:?}");
    assert_eq!(run_unboxed("(0 / 0) === (0 / 0);"), Value::Bool(false));
}

#[test]
fn p2_immediate_infinities_carry_sign() {
    let positive = boxed_map(run_boxed("1 / 0;"));
    let Value::Float(inf) = field(&positive, "number") else {
        panic!("number payload must stay Float");
    };
    assert!(inf.is_infinite() && inf.is_sign_positive(), "got {inf:?}");
    let negative = boxed_map(run_boxed("-1 / 0;"));
    let Value::Float(neg_inf) = field(&negative, "number") else {
        panic!("number payload must stay Float");
    };
    assert!(
        neg_inf.is_infinite() && neg_inf.is_sign_negative(),
        "got {neg_inf:?}"
    );
    assert_eq!(run_unboxed("(1 / 0) === (1 / 0);"), Value::Bool(true));
    assert_eq!(run_unboxed("(1 / 0) === (-1 / 0);"), Value::Bool(false));
}

// ---------------------------------------------------------------------------
// Test-only UTF-16 observation driver (labeled instrumentation).
// ---------------------------------------------------------------------------
// Delegates the original real JS source to the REAL JS plugin (real B1
// prelude, carrier, source policy, catalog and certification) and appends
// guest reads of the ACTUAL generated string node through the EXISTING B1
// read helper while the carrier is still live. The string's logical slot
// and generation resolve through the production checked-base helper;
// the carrier is discovered structurally from its real emitted allocation;
// the helper index comes from the generated `ir.functions` table, never
// from user identifier text. No public observer API, no host JS callback,
// no product readback after teardown.

const P2_READ_NAME: &str = "__compat_ref_read";
const P2_MAKE_NAME: &str = "__compat_ref_make";
const P2_CHECKED_BASE_NAME: &str = "__compat_es5_checked_base";

fn p2_rejection(message: String) -> vm::ParseError {
    vm::ParseError::at_span(vm::compiler::source_map::Span::new(0, 0, 0), message)
}

fn p2_as_number(expr: vm::Expr) -> vm::Expr {
    vm::Expr::Div(Box::new(expr), Box::new(vm::Expr::Float(1.0)))
}

fn p2_helper_index(ir: &vm::FrontendIr, name: &str) -> Result<u16, vm::ParseError> {
    ir.functions
        .iter()
        .find(|function| function.name == name)
        .map(|function| function.index)
        .ok_or_else(|| p2_rejection(format!("observer fixture requires helper '{name}'")))
}

struct P2ObservePlugin {
    /// Unit cells to read after the length cell (probe `s` always).
    units: usize,
}

fn append_p2_observer(ir: &mut vm::FrontendIr, units: usize) -> Result<(), vm::ParseError> {
    let binding = ir
        .local_bindings
        .iter()
        .rfind(|(name, _)| name == "s")
        .map(|(_, slot)| *slot)
        .ok_or_else(|| p2_rejection("observer fixture requires the real `s` binding".to_owned()))?;
    // The single realm carrier: the `Let` calling the prelude make helper,
    // exactly like the lifecycle driver discovers it.
    let make = p2_helper_index(ir, P2_MAKE_NAME)?;
    let mut carriers = Vec::new();
    for stmt in &ir.stmts {
        if let vm::Stmt::Let {
            index,
            declared_schema: None,
            expr: vm::Expr::Call(target, _, args, _, _),
            ..
        } = stmt
            && *target == make
            && args.len() == 1
        {
            carriers.push(*index);
        }
    }
    let [carrier] = carriers.as_slice() else {
        return Err(p2_rejection(format!(
            "observer fixture requires exactly one realm carrier, found {}",
            carriers.len()
        )));
    };
    let carrier = *carrier;
    let read = p2_helper_index(ir, P2_READ_NAME)?;
    let checked_base = p2_helper_index(ir, P2_CHECKED_BASE_NAME)?;
    let mut fresh = || -> Result<u16, vm::ParseError> {
        let slot = u16::try_from(ir.locals)
            .map_err(|_| p2_rejection("observer fixture slot space exhausted".to_owned()))?;
        ir.locals += 1;
        Ok(slot)
    };
    // Resolve this rooted live string once using its complete logical
    // identity. A logical slot is never a physical backing address.
    // Nested allocation temporaries stay outside top-level type tracking.
    let get = vm::BuiltinFunction::Get.call_index();
    let field = |name: &str| -> vm::Expr {
        vm::Expr::Call(
            get,
            Vec::new(),
            vec![vm::Expr::Var(binding), vm::Expr::String(name.to_owned())],
            None,
            None,
        )
    };
    let base_slot = fresh()?;
    ir.stmts.push(vm::Stmt::Let {
        index: base_slot,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: vm::Expr::Call(
            checked_base,
            Vec::new(),
            vec![
                vm::Expr::Var(carrier),
                field("ref_slot"),
                field("generation"),
            ],
            None,
            None,
        ),
        line: 1,
    });
    let read_cell = |offset: i64| -> vm::Expr {
        let physical = vm::Expr::Add(
            Box::new(vm::Expr::Var(base_slot)),
            Box::new(vm::Expr::Int(offset)),
        );
        p2_as_number(vm::Expr::Call(
            read,
            Vec::new(),
            vec![vm::Expr::Var(carrier), p2_as_number(physical)],
            None,
            None,
        ))
    };
    // Stored length cell first, then each unit cell, mirroring the product
    // `base + 2` / `base + 3 + index` layout through the same read helper.
    let length_slot = fresh()?;
    ir.stmts.push(vm::Stmt::Let {
        index: length_slot,
        declared_schema: Some(vm::TypeSchema::Number),
        expr: read_cell(2),
        line: 1,
    });
    let mut unit_slots = Vec::with_capacity(units);
    for position in 0..units {
        let slot = fresh()?;
        ir.stmts.push(vm::Stmt::Let {
            index: slot,
            declared_schema: Some(vm::TypeSchema::Number),
            expr: read_cell(3 + position as i64),
            line: 1,
        });
        unit_slots.push(slot);
    }
    // Bare value-producing reads leave their results on the stack below no
    // other driver: length first, then units in order.
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Var(length_slot),
        line: 1,
    });
    for slot in unit_slots {
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(slot),
            line: 1,
        });
    }
    Ok(())
}

impl vm::SourcePlugin for P2ObservePlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-p2-observe"),
            extensions: &["js"],
            typing_policy: vm::FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        append_p2_observer(&mut ir, self.units)?;
        Ok(ir)
    }
}

fn p2_observe_flavor() -> vm::SourceFlavor {
    vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-p2-observe"))
}

/// Observe `[length, unit...]` of binding `s` from the live realm carrier.
/// One small test-only plugin box per observation (the options contract
/// requires `&'static`); the leak is labeled and bounded by test count.
fn observe_string_units(source: &str, units: usize, native: bool) -> (Vm, Vec<Value>) {
    let plugin: &'static P2ObservePlugin = Box::leak(Box::new(P2ObservePlugin { units }));
    let mut machine = realm_vm(source, plugin, p2_observe_flavor(), native);
    if native {
        assert_eq!(machine.aot_exec_count(), 0, "no execution before run");
    }
    assert_eq!(
        machine.run().expect("observed run should finish"),
        VmStatus::Halted,
        "observed program should halt"
    );
    let stack = machine.stack().to_vec();
    let want = units + 1;
    assert!(
        stack.len() >= want,
        "length plus {units} unit observations on the stack, got {}",
        stack.len()
    );
    (machine, stack[stack.len() - want..].to_vec())
}

fn assert_observed(source: &str, expected_units: &[u16]) {
    let (_, tail) = observe_string_units(source, expected_units.len(), false);
    let mut expected = vec![Value::Float(expected_units.len() as f64)];
    expected.extend(
        expected_units
            .iter()
            .map(|unit| Value::Float(f64::from(*unit))),
    );
    assert_eq!(tail, expected, "direct UTF-16 observation for {source:?}");
}

// ---------------------------------------------------------------------------
// Runtime string observations from the actual guest node (interpreted).
// ---------------------------------------------------------------------------

#[test]
fn p2_observe_empty_and_ascii_strings() {
    assert_observed("var s = \"\"; s===s;", &[]);
    assert_observed("var s = \"hello\"; s===s;", &[0x68, 0x65, 0x6C, 0x6C, 0x6F]);
}

#[test]
fn p2_observe_bmp_and_raw_nonbmp_scalars() {
    assert_observed("var s = \"\\u00E9\"; s===s;", &[0xE9]);
    // Raw non-BMP scalar (not an escape) encodes to a surrogate pair.
    assert_observed("var s = \"\u{1D11E}\"; s===s;", &[0xD834, 0xDD1E]);
    assert_observed("var s = \"\\uFFFD\"; s===s;", &[0xFFFD]);
}

#[test]
fn p2_observe_lone_surrogates_and_pairs() {
    assert_observed("var s = \"\\uD800\"; s===s;", &[0xD800]);
    assert_observed("var s = \"\\uDC00\"; s===s;", &[0xDC00]);
    assert_observed("var s = \"\\uD834\\uDD1E\"; s===s;", &[0xD834, 0xDD1E]);
}

#[test]
fn p2_observe_embedded_nul_both_modes() {
    assert_observed("var s = \"\\0\"; s===s;", &[0x00]);
    assert_observed("\"use strict\"; var s = \"\\0\"; s===s;", &[0x00]);
    assert_observed("var s = \"a\\0b\"; s===s;", &[0x61, 0x00, 0x62]);
}

#[test]
fn p2_observe_mixed_escapes_and_continuations() {
    assert_observed(
        "var s = \"A\\0\\u00E9\\x42\\u0043\\q\"; s===s;",
        &[0x41, 0x00, 0xE9, 0x42, 0x43, 0x71],
    );
    // Line continuations contribute no units.
    assert_observed("var s = \"a\\\nb\"; s===s;", &[0x61, 0x62]);
    assert_observed("var s = \"a\\\r\nb\"; s===s;", &[0x61, 0x62]);
}

#[test]
fn p2_observe_sloppy_octal_values() {
    // Genuine Annex-B derivations keep exact values through lowering.
    assert_observed("var s = \"\\01\"; s===s;", &[0x01]);
    assert_observed("var s = \"\\400\"; s===s;", &[0x20, 0x30]);
}

// ---------------------------------------------------------------------------
// One true mixed UTF-16 / NUL / surrogate observation under native SSA.
// ---------------------------------------------------------------------------

#[test]
fn p2_observe_mixed_string_native_ssa() {
    let source = "var s = \"A\\0\\u00E9\\uD834\\uDD1E\\uDC00z\"; s===s;";
    let expected_units: [u16; 7] = [0x41, 0x00, 0xE9, 0xD834, 0xDD1E, 0xDC00, 0x7A];
    let mut machine = realm_vm(
        source,
        Box::leak(Box::new(P2ObservePlugin {
            units: expected_units.len(),
        })),
        p2_observe_flavor(),
        true,
    );
    assert_eq!(machine.aot_exec_count(), 0, "no execution before run");
    assert_eq!(
        machine.run().expect("native observed run should finish"),
        VmStatus::Halted,
        "native observed program should halt"
    );
    let count = machine.aot_exec_count();
    let info = machine.dump_aot_info();
    eprintln!("P2_VALUE_NATIVE count={count} source={source}\n{info}");
    assert!(
        count > 0,
        "mixed UTF-16/NUL/surrogate observation must truly execute native code, got {count}"
    );
    assert!(
        info.contains("lowering=ssa"),
        "native observation must stay SSA lowering; info: {info}"
    );
    // Actual carrier reads land on the stack tail: length plus each unit.
    let stack = machine.stack().to_vec();
    let tail = &stack[stack.len() - (expected_units.len() + 1)..];
    let mut expected = vec![Value::Float(expected_units.len() as f64)];
    expected.extend(
        expected_units
            .iter()
            .map(|unit| Value::Float(f64::from(*unit))),
    );
    assert_eq!(
        tail.to_vec(),
        expected,
        "native observation must inspect actual unit/length values"
    );
}

// ---------------------------------------------------------------------------
// Property names and lookups through the same decoded units.
// ---------------------------------------------------------------------------

fn run_bool(source: &str) -> Value {
    let catalog = realm_catalog();
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        javascript_es5_flavor(),
        realm_options(&JAVASCRIPT_ES5_PLUGIN, &catalog),
    )
    .unwrap_or_else(|error| panic!("property JS source must compile: {source:?}: {error:?}"));
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = HostFunctionRegistry::restricted();
    install_production(&mut registry, &catalog);
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    assert_eq!(
        machine.run().expect("property run should finish"),
        VmStatus::Halted
    );
    let value = machine
        .stack()
        .last()
        .cloned()
        .expect("program should leave a value");
    let Value::Map(fields) = value else {
        panic!("expected the actual boxed JS value, got {value:?}");
    };
    assert_eq!(
        fields.get(&Value::string("tag")),
        Some(&Value::Int(2)),
        "property comparison must carry the Boolean tag"
    );
    fields
        .get(&Value::string("flag"))
        .expect("boolean payload")
        .clone()
}

#[test]
fn p2_property_same_spelling_through_different_escapes() {
    // Identical decoded units collide across spellings at runtime.
    assert_eq!(
        run_bool("var o = {\"a\":7}; o[\"\\u0061\"]===7;"),
        Value::Bool(true)
    );
    assert_eq!(
        run_bool("var o = {\"\\u0061\":7}; o[\"a\"]===7;"),
        Value::Bool(true)
    );
    assert_eq!(
        run_bool("var o = {a:7}; o[\"\\u0061\"]===7;"),
        Value::Bool(true)
    );
    // Octal-derived keys decode identically on both sides.
    assert_eq!(
        run_bool("var o = {\"\\101\":7}; o[\"A\"]===7;"),
        Value::Bool(true)
    );
}

#[test]
fn p2_property_lone_surrogate_keys_keep_both_payloads() {
    // Distinct lone-surrogate/replacement keys coexist with both payloads.
    assert_eq!(
        run_bool("var o = {\"\\uD800\":7, \"\\uFFFD\":8}; o[\"\\uD800\"]===7;"),
        Value::Bool(true)
    );
    assert_eq!(
        run_bool("var o = {\"\\uD800\":7, \"\\uFFFD\":8}; o[\"\\uFFFD\"]===8;"),
        Value::Bool(true)
    );
    assert_eq!(
        run_bool("var o = {\"a\":7, \"\\uD800\":8}; o[\"a\"]===7;"),
        Value::Bool(true)
    );
}

#[test]
fn p2_property_true_duplicate_refusal_remains() {
    // Sloppy duplicates stay admitted at grammar stage but the P0 object
    // path still refuses them; strict duplicates stay early errors.
    let text = must_reject_with("var o = {\"a\":1, \"a\":2}; o;", "duplicate keys");
    assert!(
        text.contains("P0 plain objects"),
        "duplicate refusal stays on the P0 object path, got: {text}"
    );
    must_reject_with(
        "\"use strict\"; var o = {\"a\":1, \"a\":2}; o;",
        "duplicate data property",
    );
}

// ---------------------------------------------------------------------------
// Admission negatives across positions plus resolved-strictness interplay.
// ---------------------------------------------------------------------------

#[test]
fn p2_admission_negatives_cover_positions() {
    // Post-ES5 braced escapes: value, directive, property, member and
    // unlowered-body positions share the unsupported category.
    for source in [
        "var s = \"\\u{1D11E}\"; s;",
        "\"\\u{1D11E}\";",
        "({\"\\u{1D11E}\":1});",
        "var o = {a:1}; o[\"\\u{1D11E}\"];",
        "while (1) { \"\\u{1D11E}\"; }",
        "function f(){ \"\\u{1D11E}\"; return 1; }",
    ] {
        let text = must_reject_with(source, "unsupported syntax");
        assert!(
            text.contains("braced"),
            "braced guard for {source:?}, got: {text}"
        );
    }
    // Raw separators and decimal/lookahead violations are SyntaxErrors.
    for source in [
        "var s = \"a\u{2028}b\"; s;",
        "({\"a\u{2028}b\":1});",
        "var o = {a:1}; o[\"a\u{2029}b\"];",
        "var s = \"\\9\"; s;",
        "function f(){ \"\\08\"; }",
        "({\"\\118\":1});",
        "var o = {a:1}; o[\"\\8\"];",
    ] {
        must_reject_with(source, "SyntaxError");
    }
    // Escaped-backslash text and valid continuations stay admitted.
    assert!(compile_outcome("var s = \"\\\\u{1D11E}\"; s===s;").is_ok());
    assert!(compile_outcome("var s = \"a\\\nb\"; s===s;").is_ok());
    assert!(compile_outcome("var s = \"\\\\u2028\"; s===s;").is_ok());
}

#[test]
fn p2_strictness_interplay_through_compilation() {
    // An early legacy string plus a later exact directive is strict code.
    must_reject_with("\"\\01\"; \"use strict\";", "SyntaxError");
    // Strict inheritance reaches nested code the locked lexer can already
    // see: once strict is established, SWC owns legacy-octal diagnostics
    // outright (LegacyOctal parse error), recorded here as parser-owned.
    match compile_outcome(
        "\"use strict\"; var f = function(){ \"use strict\"; return \"\\01\"; }; f();",
    ) {
        Ok(()) => panic!("nested strict legacy octal must not be admitted"),
        Err(text) => assert!(
            text.contains("LegacyOctal") || text.contains("SyntaxError"),
            "nested strict octal refusal, got: {text}"
        ),
    }
    // Escaped and parenthesized lookalikes never activate strictness.
    assert!(compile_outcome("\"\\u0073trict\"; var s = \"\\01\"; s===s;").is_ok());
    assert!(compile_outcome("(\"use strict\"); var s = \"\\01\"; s===s;").is_ok());
    // A nested directive does not leak outward.
    assert!(
        compile_outcome(
            "var f = function(){ \"use strict\"; return 1; }; var s = \"\\01\"; s===s;"
        )
        .is_ok()
    );
    // Exact directives still set strictness for execution scoping.
    assert_eq!(
        run_bool("\"use strict\"; var o = {\"a\":7}; o[\"\\u0061\"]===7;"),
        Value::Bool(true)
    );
}
