//! Initial vertical source proofs for the independent ES5 frontend.
//! These tests do not establish the complete P0/profile acceptance.

use pd_vm_compat_frontends::{javascript_es5_compile_options, javascript_es5_flavor};
use vm::{Value, Vm, VmStatus, compile_source_with_flavor_and_options};

fn run_source(source: &str) -> Value {
    let compiled = compile_source_with_flavor_and_options(
        source,
        javascript_es5_flavor(),
        javascript_es5_compile_options(),
    )
    .unwrap_or_else(|error| panic!("raw JS source must compile: {error:?}"));
    let mut machine = Vm::new(compiled.program);
    assert_eq!(
        machine.run().expect("JS source should execute"),
        VmStatus::Halted
    );
    let value = machine.stack().last().cloned().expect("expression result");
    let Value::Map(fields) = value else {
        panic!("expected the actual boxed JS primitive, got {value:?}");
    };
    let tag = fields.get(&Value::string("tag")).expect("boxed tag");
    match tag {
        Value::Int(3) => {
            let number = fields
                .get(&Value::string("number"))
                .expect("number payload");
            assert!(
                matches!(number, Value::Float(_)),
                "JS number must remain Float"
            );
            number.clone()
        }
        Value::Int(2) => {
            let flag = fields.get(&Value::string("flag")).expect("boolean payload");
            assert!(matches!(flag, Value::Bool(_)), "JS Boolean payload");
            flag.clone()
        }
        Value::Int(0 | 1) => Value::Map(fields),
        other => panic!("unexpected primitive tag: {other:?}"),
    }
}

#[test]
fn division_preserves_javascript_fraction() {
    assert_eq!(run_source("1 / 2;"), Value::Float(0.5));
}

#[test]
fn variable_operands_are_computed_from_actual_source() {
    assert_eq!(
        run_source("var numerator = 15; var denominator = 6; numerator / denominator;"),
        Value::Float(2.5),
    );
}

#[test]
fn undefined_and_null_are_distinct_javascript_values() {
    assert_eq!(run_source("undefined === undefined;"), Value::Bool(true));
    assert_eq!(run_source("null === null;"), Value::Bool(true));
    assert_eq!(run_source("undefined === null;"), Value::Bool(false));
}

#[test]
fn undefined_variable_alias_is_strictly_undefined() {
    assert_eq!(
        run_source("var u = undefined; var v = u; v === undefined;"),
        Value::Bool(true)
    );
}

#[test]
fn undefined_and_null_variables_are_distinct() {
    assert_eq!(
        run_source("var u = undefined; var n = null; u === n;"),
        Value::Bool(false)
    );
}

#[test]
fn aliased_numeric_variable_is_strictly_equal() {
    assert_eq!(
        run_source("var x = 1 / 2; var y = x; x === y;"),
        Value::Bool(true)
    );
}

#[test]
fn distinct_numbers_are_not_strictly_equal() {
    assert_eq!(run_source("1 === 2;"), Value::Bool(false));
}

#[test]
fn boolean_identity_is_strict() {
    assert_eq!(run_source("true === true;"), Value::Bool(true));
    assert_eq!(run_source("false === true;"), Value::Bool(false));
}

#[test]
fn post_es5_binding_declarations_are_rejected() {
    for source in ["let x = 1; x;", "const x = 1; x;"] {
        assert!(
            compile_source_with_flavor_and_options(
                source,
                javascript_es5_flavor(),
                javascript_es5_compile_options(),
            )
            .is_err(),
            "post-ES5 declaration must not be admitted: {source}",
        );
    }
}

#[test]
fn ordinary_options_do_not_register_the_es5_frontend() {
    assert!(
        compile_source_with_flavor_and_options(
            "1 / 2;",
            javascript_es5_flavor(),
            pd_vm_compat_frontends::compile_options(),
        )
        .is_err(),
        "ES5 must remain explicitly selected",
    );
}

#[test]
fn primitive_source_executes_native_ssa() {
    let source = "var x = 1 / 2; var y = x; x === y;";
    let compiled = compile_source_with_flavor_and_options(
        source,
        javascript_es5_flavor(),
        javascript_es5_compile_options(),
    )
    .expect("primitive JS source compilation");
    let mut machine = Vm::new(compiled.program);
    machine.compile_aot().expect("primitive native compilation");
    let outcome = machine.run();
    let count = machine.aot_exec_count();
    let info = machine.dump_aot_info();
    eprintln!("ES5_PRIMITIVE_NATIVE count={count} outcome={outcome:?}\n{info}");
    assert_eq!(outcome.expect("native source execution"), VmStatus::Halted);
    let Some(Value::Map(fields)) = machine.stack().last() else {
        panic!(
            "native source must return actual boxed primitive: {:?}",
            machine.stack()
        );
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(2)));
    assert_eq!(fields.get(&Value::string("flag")), Some(&Value::Bool(true)));
    assert!(count > 0, "no actual native execution; info: {info}");
    assert!(
        info.contains("lowering=ssa"),
        "not SSA native; info: {info}"
    );
}

fn js_realm_catalog() -> std::sync::Arc<vm::HostApiCatalog> {
    pd_vm_compat_frontends::reference_array_effective_catalog()
}

fn js_realm_options(catalog: &std::sync::Arc<vm::HostApiCatalog>) -> vm::CompileSourceFileOptions {
    vm::CompileSourceFileOptions::new()
        .with_source_plugin(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN)
        .with_host_api_catalog(std::sync::Arc::clone(catalog))
}

fn compile_rejection(
    catalog: &std::sync::Arc<vm::HostApiCatalog>,
    source: &str,
    label: &str,
) -> String {
    match vm::compile_source_with_flavor_and_options(
        source,
        javascript_es5_flavor(),
        js_realm_options(catalog),
    ) {
        Ok(_) => panic!("{label} must reject, but compiled: {source:?}"),
        Err(error) => format!("{error:?}"),
    }
}

fn js_realm_vm(
    source: &str,
    limits: pd_vm_compat_frontends::runtime::array::OwnedArrayLimits,
    native: bool,
) -> (Vm, std::sync::Arc<vm::HostApiCatalog>) {
    let catalog = js_realm_catalog();
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        javascript_es5_flavor(),
        js_realm_options(&catalog),
    )
    .expect("JS realm compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        limits,
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = vm::HostFunctionRegistry::restricted();
    pd_vm_compat_frontends::runtime::array::typescript_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("manual arrays adapters should match the effective catalog");
    pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("owned adapters should match the effective catalog");
    pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("snapshot adapter should match the effective catalog");
    pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("scalar-math adapter should match the effective catalog");
    registry
        .allow_builtin("math::floor")
        .expect("math::floor should be allow-listable");
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    if native {
        machine
            .compile_aot()
            .expect("native compilation should succeed");
    }
    (machine, catalog)
}

fn js_realm_unboxed_number(machine: &mut Vm) -> Value {
    assert_eq!(
        machine.run().expect("JS realm run should finish"),
        VmStatus::Halted,
        "JS realm program should halt"
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
        Some(&Value::Int(3)),
        "realm numeric result must carry the Number tag"
    );
    let number = fields
        .get(&Value::string("number"))
        .expect("number payload")
        .clone();
    assert!(
        matches!(number, Value::Float(_)),
        "JS number must remain Float, got {number:?}"
    );
    number
}

#[test]
fn js_realm_alias_mutation_is_visible_through_one_owner() {
    let (mut machine, _) = js_realm_vm(
        "var a = [1, 2]; var b = a; b[0] = 7; a[0];",
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        false,
    );
    assert_eq!(js_realm_unboxed_number(&mut machine), Value::Float(7.0));
    let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("directory should exist");
    assert_eq!(stats.pending, 0, "factory must register before returning");
    assert_eq!(stats.registered, 1, "one shared owner for the alias pair");
}

#[test]
fn js_realm_repeated_writes_keep_alias_payload() {
    let (mut machine, _) = js_realm_vm(
        "var a=[1,2];var alias=a;a[0]=7;a[1]=9;alias[1];",
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        false,
    );
    assert_eq!(js_realm_unboxed_number(&mut machine), Value::Float(9.0));
    assert_eq!(machine.host_context().resource_count(), 1);
}

#[test]
fn js_numeric_array_root_plan_ignores_primitive_payload_count() {
    fn root_capacity(payload: usize) -> i64 {
        let source = format!("var a=[{}];a[0];", vec!["0"; payload].join(","));
        let catalog = js_realm_catalog();
        let context = vm::SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::compiler::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let ir = vm::SourcePlugin::parse_source(
            &pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN,
            &source,
            &context,
        )
        .expect("numeric-array root plan must lower real source");
        let enter = ir
            .functions
            .iter()
            .find(|function| function.name == "__compat_es5_root_enter")
            .expect("production root-enter helper")
            .index;
        let capacities: Vec<i64> = ir
            .stmts
            .iter()
            .filter_map(|statement| match statement {
                vm::Stmt::Let {
                    expr: vm::Expr::Call(target, _, args, _, _),
                    ..
                } if *target == enter => match args.get(1) {
                    Some(vm::Expr::Int(capacity)) => Some(*capacity),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(capacities.len(), 1, "one actual script frame");
        capacities[0]
    }
    let small = root_capacity(32);
    let large = root_capacity(8192);
    eprintln!("ARRAY_ROOT_PLAN small={small} large={large}");
    assert_eq!(
        small, large,
        "numeric elements contain no heap edges; root capacity must depend on the receiver code, not payload length"
    );
}

fn js_realm_unboxed_bool(machine: &mut Vm) -> Value {
    assert_eq!(
        machine.run().expect("JS realm run should finish"),
        VmStatus::Halted,
        "JS realm program should halt"
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
        "realm boolean result must carry the Boolean tag"
    );
    fields
        .get(&Value::string("flag"))
        .expect("boolean payload")
        .clone()
}

#[test]
fn js_realm_identity_uses_logical_ids_not_structure() {
    let (mut machine, _) = js_realm_vm(
        "var a = [1]; var b = [1]; var c = a; (a === c) !== (a === b);",
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        false,
    );
    assert_eq!(js_realm_unboxed_bool(&mut machine), Value::Bool(true));
    let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("directory should exist");
    assert_eq!(stats.pending, 0);
    assert_eq!(
        stats.registered, 1,
        "one realm owner holds both distinct logical arrays"
    );
}

#[test]
fn js_realm_ids_survive_backing_growth() {
    let (mut machine, _) = js_realm_vm(
        "var a = [3]; var b = a; var c = [4, 5, 6, 7, 8]; b[0] = 9; a[0];",
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        false,
    );
    assert_eq!(js_realm_unboxed_number(&mut machine), Value::Float(9.0));
    let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("directory should exist");
    assert_eq!(stats.pending, 0);
    assert_eq!(stats.registered, 1, "one owner for the whole realm");
}

#[test]
fn js_realm_alias_mutation_is_native_with_ssa_lowering() {
    let (mut machine, _) = js_realm_vm(
        "var a = [1, 2]; var b = a; b[0] = 7; a[0];",
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        true,
    );
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(js_realm_unboxed_number(&mut machine), Value::Float(7.0));
    let count = machine.aot_exec_count();
    let info = machine.dump_aot_info();
    eprintln!("JS_REALM_NATIVE count={count} outcome=Halted\n{info}");
    assert!(
        count > 1,
        "actual native execution must exceed one dispatch, got {count}"
    );
    assert!(
        info.contains("lowering=ssa"),
        "native lowering must stay ssa"
    );
    let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("directory should exist");
    assert_eq!(stats.pending, 0);
    assert_eq!(stats.registered, 1);
}

#[test]
fn js_realm_uncertified_program_fails_closed_before_allocation() {
    let catalog = js_realm_catalog();
    let uncertified = vm::compile_source_with_flavor_and_options(
        "var a = [1, 2]; var b = a; b[0] = 7; a[0];",
        javascript_es5_flavor(),
        js_realm_options(&catalog),
    )
    .expect("uncertified compile should succeed past source parsing");
    let mut machine = Vm::new(uncertified.program);
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("init should succeed");
    let mut registry = vm::HostFunctionRegistry::restricted();
    pd_vm_compat_frontends::runtime::array::typescript_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("manual arrays adapters should match the effective catalog");
    pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("owned adapters should match the effective catalog");
    pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("snapshot adapter should match the effective catalog");
    pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("scalar-math adapter should match the effective catalog");
    registry
        .allow_builtin("math::floor")
        .expect("math::floor should be allow-listable");
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    let outcome = machine.run();
    assert!(
        outcome.is_err(),
        "uncertified source must fail closed, got {outcome:?}"
    );
    let text = format!("{outcome:?}");
    assert!(
        text.contains("certif") || text.contains("source"),
        "refusal must cite source authority, got {text}"
    );
    if let Some(stats) =
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
    {
        assert_eq!(
            stats.pending, 0,
            "no pending allocation may survive refusal"
        );
        assert_eq!(
            stats.registered, 0,
            "no owner may register without authority"
        );
    }
}

#[test]
fn utf16_lone_surrogate_differs_from_replacement() {
    let (mut machine, _) = js_realm_vm(
        "var lone=\"\\uD800\";var replacement=\"\\uFFFD\";lone===replacement;",
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        false,
    );
    assert_eq!(js_realm_unboxed_bool(&mut machine), Value::Bool(false));
}

#[test]
fn utf16_same_lone_surrogate_at_distinct_bases_is_equal() {
    let (mut machine, _) = js_realm_vm(
        "var a=\"\\uD800\";var b=\"\\uD800\";a===b;",
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        false,
    );
    assert_eq!(js_realm_unboxed_bool(&mut machine), Value::Bool(true));
}

#[test]
fn utf16_lone_low_surrogate_differs_from_replacement() {
    let (mut machine, _) = js_realm_vm(
        "var low=\"\\uDC00\";var replacement=\"\\uFFFD\";low===replacement;",
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        false,
    );
    assert_eq!(js_realm_unboxed_bool(&mut machine), Value::Bool(false));
}

#[test]
fn utf16_escaped_pair_matches_raw_non_bmp_scalar() {
    let source = format!("var a=\"\\uD834\\uDD1E\";var b=\"{}\";a===b;", '\u{1D11E}');
    let (mut machine, _) = js_realm_vm(
        &source,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        false,
    );
    assert_eq!(js_realm_unboxed_bool(&mut machine), Value::Bool(true));
}

#[test]
fn utf16_composed_and_decomposed_contents_differ() {
    let (mut machine, _) = js_realm_vm(
        "var a=\"\\u00E9\";var b=\"e\\u0301\";a===b;",
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        false,
    );
    assert_eq!(js_realm_unboxed_bool(&mut machine), Value::Bool(false));
}

#[test]
fn utf16_empty_strings_are_equal() {
    let (mut machine, _) = js_realm_vm(
        "var a=\"\";var b=\"\";a===b;",
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        false,
    );
    assert_eq!(js_realm_unboxed_bool(&mut machine), Value::Bool(true));
}

#[test]
fn utf16_string_alias_copies_the_logical_value() {
    let (mut machine, _) = js_realm_vm(
        "var a=\"\\uD800\";var b=a;a===b;",
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        false,
    );
    assert_eq!(js_realm_unboxed_bool(&mut machine), Value::Bool(true));
}

#[test]
fn utf16_strings_survive_backing_growth_by_contents() {
    let (mut machine, _) = js_realm_vm(
        "var s=\"\\uD800\";var big=[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32];var t=\"\\uD800\";s===t;",
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        false,
    );
    assert_eq!(js_realm_unboxed_bool(&mut machine), Value::Bool(true));
    let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("directory should exist");
    assert_eq!(stats.pending, 0);
    assert_eq!(stats.registered, 1, "one owner for the whole realm");
}

#[test]
fn utf16_escaped_backslash_is_not_decoded_twice() {
    let (mut machine, _) = js_realm_vm(
        "var s=\"\\\\uD800\";var lone=\"\\uD800\";s===lone;",
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        false,
    );
    assert_eq!(js_realm_unboxed_bool(&mut machine), Value::Bool(false));
}

#[test]
fn utf16_mixed_string_number_and_array_tags_differ() {
    for source in ["var s=\"a\";var n=1;s===n;", "var s=\"a\";var a=[1];s===a;"] {
        let (mut machine, _) = js_realm_vm(
            source,
            pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
            false,
        );
        assert_eq!(
            js_realm_unboxed_bool(&mut machine),
            Value::Bool(false),
            "mixed tags must compare false: {source}"
        );
    }
}

#[test]
fn utf16_distinct_strings_are_not_strictly_equal() {
    let (mut machine, _) = js_realm_vm(
        "var a=\"x\";var b=\"y\";a!==b;",
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        false,
    );
    assert_eq!(js_realm_unboxed_bool(&mut machine), Value::Bool(true));
}

#[test]
fn utf16_strings_in_numeric_positions_are_rejected() {
    let catalog = js_realm_catalog();
    for source in [
        "var s=\"a\";var n=1;s+n;",
        "var s=\"a\";var a=[s];a;",
        "var s=\"a\";s[0];",
    ] {
        assert!(
            vm::compile_source_with_flavor_and_options(
                source,
                javascript_es5_flavor(),
                js_realm_options(&catalog),
            )
            .is_err(),
            "string in numeric position must stay unsupported: {source}",
        );
    }
}

#[test]
fn js_realm_distinct_allocations_keep_bases_native() {
    // Two realm allocations then cross reads: bases must not overlap.
    // Correct: 1.0 + 3.0 = 4.0; overlapping bases would read 3.0 + 3.0.
    let (mut machine, _) = js_realm_vm(
        "var a=[1,2];var b=[3,4];a[0]+b[0];",
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        true,
    );
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(js_realm_unboxed_number(&mut machine), Value::Float(4.0));
    let count = machine.aot_exec_count();
    let info = machine.dump_aot_info();
    eprintln!("JS_REALM_TWO_ALLOCS_NATIVE count={count}\n{info}");
    assert!(
        count > 1,
        "actual native execution must exceed one dispatch, got {count}"
    );
    assert!(
        info.contains("lowering=ssa"),
        "native lowering must stay ssa"
    );
}

#[test]
fn utf16_lone_strings_execute_native_with_ssa_lowering() {
    for (source, expected) in [
        (
            "var lone=\"\\uD800\";var replacement=\"\\uFFFD\";lone===replacement;",
            false,
        ),
        ("var a=\"\\uD800\";var b=\"\\uD800\";a===b;", true),
    ] {
        let (mut machine, _) = js_realm_vm(
            source,
            pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
            true,
        );
        assert_eq!(machine.aot_exec_count(), 0);
        assert_eq!(
            js_realm_unboxed_bool(&mut machine),
            Value::Bool(expected),
            "native observation must match: {source}"
        );
        let count = machine.aot_exec_count();
        let info = machine.dump_aot_info();
        eprintln!("JS_UTF16_NATIVE count={count} source={source}\n{info}");
        assert!(
            count > 1,
            "actual native execution must exceed one dispatch, got {count}"
        );
        assert!(
            info.contains("lowering=ssa"),
            "native lowering must stay ssa"
        );
    }
}

#[test]
fn utf16_oversized_literal_rejects_before_emission() {
    let catalog = js_realm_catalog();
    let source = format!("var s=\"{}\";s===s;", "a".repeat(16385));
    let text = compile_rejection(&catalog, &source, "oversized UTF-16 literal");
    eprintln!("UTF16_OVERSIZED_REJECTION: {text}");
    assert!(
        text.contains("16384"),
        "oversized literal must cite the unit bound, got {text}"
    );
}

#[test]
fn utf16_combined_initial_cells_reject_before_emission() {
    let catalog = js_realm_catalog();
    let array = format!("var a=[{}];", vec!["0"; 60000].join(","));
    let source = format!("{array}var s=\"{}\";s===s;", "a".repeat(6000));
    let text = compile_rejection(&catalog, &source, "combined cell budget excess");
    eprintln!("UTF16_BUDGET_REJECTION: {text}");
    assert!(
        text.contains("65536"),
        "budget excess must cite the cell budget, got {text}"
    );
}

#[test]
fn utf16_near_bound_literals_still_admit() {
    let source = format!(
        "var s=\"{}\";var t=\"{}\";s===t;",
        "a".repeat(16000),
        "a".repeat(16000)
    );
    let (mut machine, _) = js_realm_vm(
        &source,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        false,
    );
    assert_eq!(js_realm_unboxed_bool(&mut machine), Value::Bool(true));
}

#[test]
fn utf16_post_es5_spellings_are_rejected() {
    let catalog = js_realm_catalog();
    let braced = "var s=\"\\u{1D11E}\";s;";
    let text = compile_rejection(&catalog, braced, "braced Unicode escape");
    eprintln!("UTF16_BRACED_REJECTION: {text}");
    assert!(
        text.contains("braced"),
        "braced escape must hit the ES5 guard, got {text}"
    );
    let raw = format!("var s=\"a{}b\";s;", '\u{2028}');
    let text = compile_rejection(&catalog, &raw, "raw line separator");
    eprintln!("UTF16_RAW_SEPARATOR_REJECTION: {text}");
    assert!(
        text.contains("2028"),
        "raw separator must hit the ES5 guard, got {text}"
    );
}

#[test]
fn utf16_es5_lookalikes_stay_admitted() {
    for (source, label) in [
        (
            "var s=\"\\\\u{1D11E}\";s===s;",
            "escaped backslash before u-brace",
        ),
        ("var s=\"a\\\nb\";s===s;", "line continuation"),
        ("var s=\"\\\\u2028\";s===s;", "escaped separator"),
    ] {
        let (mut machine, _) = js_realm_vm(
            source,
            pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
            false,
        );
        assert_eq!(
            js_realm_unboxed_bool(&mut machine),
            Value::Bool(true),
            "{label} must stay admitted: {source:?}"
        );
    }
}

// ---- Private allocator IR stress (test-only) ----
// Replays the SAME production-emitted literal-allocation statement region
// twice inside an ordinary bounded guest loop and reports whether the two
// observed extents differ. The proxy below delegates the original real JS
// source to the REAL JS plugin (real B1 prelude, carrier, source policy,
// catalog and certification) and only appends a test-owned observation
// driver. This is allocator stress, not user-level loop support: P0 JS
// admits no loop syntax, so repetition is constructed at the IR level from
// the actual emitted statements. A compile-time constant-base emitter must
// fail this distinctness check; a genuine runtime allocator passes it.
struct AllocStressPlugin;

static ALLOC_STRESS_PLUGIN: AllocStressPlugin = AllocStressPlugin;

fn alloc_stress_flavor() -> vm::SourceFlavor {
    vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-alloc-stress"))
}

fn stress_rejection(message: String) -> vm::ParseError {
    vm::ParseError::at_span(vm::compiler::source_map::Span::new(0, 0, 0), message)
}

/// Replay the actual node initialization/publication at the same code site.
/// Resolve physical bases through checked logical identities, pin both
/// products in a real root frame, collect between iterations, then compare
/// their distinct extents and actual payloads. Source loop syntax stays
/// outside this explicitly test-owned IR driver.
fn append_distinctness_driver(ir: &mut vm::FrontendIr) -> Result<(), vm::ParseError> {
    let binding = ir
        .local_bindings
        .iter()
        .find(|(name, _)| name == "a")
        .map(|(_, slot)| *slot)
        .ok_or_else(|| {
            stress_rejection("stress fixture requires the real `a` binding".to_owned())
        })?;
    let boxed_at = ir
        .stmts
        .iter()
        .position(|stmt| matches!(stmt, vm::Stmt::Assign { index, .. } if *index == binding))
        .ok_or_else(|| stress_rejection("stress fixture requires the real `a` store".to_owned()))?;
    let helper = |name: &str| -> Result<u16, vm::ParseError> {
        ir.functions
            .iter()
            .find(|function| function.name == name)
            .map(|function| function.index)
            .ok_or_else(|| stress_rejection(format!("stress fixture requires '{name}'")))
    };
    let make = helper("__compat_ref_make")?;
    let begin = helper("__compat_es5_node_begin")?;
    let root_enter = helper("__compat_es5_root_enter")?;
    let root_write = helper("__compat_es5_root_write")?;
    let checked_base = helper("__compat_es5_checked_base")?;
    let collect = helper("__compat_es5_heap_collect")?;
    let read = helper("__compat_ref_read")?;
    let carrier = ir
        .stmts
        .iter()
        .find_map(|statement| match statement {
            vm::Stmt::Let {
                index,
                expr: vm::Expr::Call(target, _, args, _, _),
                ..
            } if *target == make && args.len() == 1 => Some(*index),
            _ => None,
        })
        .ok_or_else(|| stress_rejection("stress fixture requires the real carrier".to_owned()))?;
    // Replay the actual node initialization and publication, stopping at
    // the script frame entry. A frame pointer is not a node allocation.
    let mut run_start = boxed_at;
    while run_start > 0 {
        match &ir.stmts[run_start - 1] {
            vm::Stmt::Let { expr, .. } => {
                if matches!(expr, vm::Expr::Call(target, _, _, _, _) if *target == root_enter) {
                    break;
                }
                run_start -= 1;
            }
            _ => break,
        }
    }
    let mut region: Vec<vm::Stmt> = ir.stmts[run_start..=boxed_at].to_vec();
    if !region.iter().any(|statement| {
        matches!(statement,
        vm::Stmt::Let { expr: vm::Expr::Call(target, _, _, _, _), .. } if *target == begin)
    }) {
        return Err(stress_rejection(
            "stress replay must include actual node-begin".to_owned(),
        ));
    }
    if let Some(
        statement @ vm::Stmt::Let {
            expr: vm::Expr::Call(target, _, _, _, _),
            ..
        },
    ) = ir.stmts.get(boxed_at + 1)
        && *target == root_write
    {
        region.push(statement.clone());
    }
    let get = vm::BuiltinFunction::Get.call_index();
    let field = |name: &str| {
        vm::Expr::Call(
            get,
            Vec::new(),
            vec![vm::Expr::Var(binding), vm::Expr::String(name.to_owned())],
            None,
            None,
        )
    };
    let resolve = || {
        vm::Expr::Call(
            checked_base,
            Vec::new(),
            vec![
                vm::Expr::Var(carrier),
                field("ref_slot"),
                field("generation"),
            ],
            None,
            None,
        )
    };
    let fresh = |ir: &mut vm::FrontendIr| -> Result<u16, vm::ParseError> {
        let slot = u16::try_from(ir.locals)
            .map_err(|_| stress_rejection("stress fixture slot space exhausted".to_owned()))?;
        ir.locals += 1;
        Ok(slot)
    };
    // Native SSA requires balanced stacks across loop edges, while bare
    // `Stmt::Expr` calls leave their results on the stack. Replay those
    // value-producing calls (realm writes/resizes, all Number) into a
    // side-effect-free sink slot instead; calls still execute identically.
    let sink = fresh(ir)?;
    ir.stmts.push(vm::Stmt::Let {
        index: sink,
        declared_schema: Some(vm::TypeSchema::Number),
        expr: vm::Expr::Float(0.0),
        line: 1,
    });
    let mut replay = Vec::with_capacity(region.len());
    for stmt in &region {
        match stmt {
            vm::Stmt::Let {
                index, expr, line, ..
            } => replay.push(vm::Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: *index,
                expr: expr.clone(),
                line: *line,
            }),
            vm::Stmt::Expr { expr, line } => replay.push(vm::Stmt::Assign {
                kind: vm::AssignmentKind::Set,
                index: sink,
                expr: expr.clone(),
                line: *line,
            }),
            vm::Stmt::Assign {
                kind,
                index,
                expr,
                line,
            } => replay.push(vm::Stmt::Assign {
                kind: kind.clone(),
                index: *index,
                expr: expr.clone(),
                line: *line,
            }),
            other => {
                return Err(stress_rejection(format!(
                    "stress fixture supports only Let/Expr/Assign replay, got {other:?}"
                )));
            }
        }
    }
    let iter = fresh(ir)?;
    let first = fresh(ir)?;
    let second = fresh(ir)?;
    let frame = fresh(ir)?;
    let base_slot = fresh(ir)?;
    let ack = fresh(ir)?;
    ir.stmts.push(vm::Stmt::Let {
        index: frame,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: vm::Expr::Call(
            root_enter,
            Vec::new(),
            vec![vm::Expr::Var(carrier), vm::Expr::Int(2)],
            None,
            None,
        ),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Let {
        index: base_slot,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: vm::Expr::Int(0),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Let {
        index: ack,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: vm::Expr::Int(0),
        line: 1,
    });
    let mut body = replay;
    // Both replay products remain logical roots across a real collection;
    // a retained scalar address alone would not keep a node alive.
    body.push(vm::Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: ack,
        expr: vm::Expr::Call(
            root_write,
            Vec::new(),
            vec![
                vm::Expr::Var(carrier),
                vm::Expr::Var(frame),
                vm::Expr::Var(iter),
                vm::Expr::Var(binding),
            ],
            None,
            None,
        ),
        line: 1,
    });
    body.push(vm::Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: base_slot,
        expr: resolve(),
        line: 1,
    });
    body.push(vm::Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: ack,
        expr: vm::Expr::Call(
            collect,
            Vec::new(),
            vec![vm::Expr::Var(carrier)],
            None,
            None,
        ),
        line: 1,
    });
    body.push(vm::Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: first,
        expr: vm::Expr::IfElse {
            condition: Box::new(vm::Expr::Eq(
                Box::new(vm::Expr::Var(iter)),
                Box::new(vm::Expr::Int(0)),
            )),
            then_expr: Box::new(vm::Expr::Var(base_slot)),
            else_expr: Box::new(vm::Expr::Var(first)),
        },
        line: 1,
    });
    body.push(vm::Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: second,
        expr: vm::Expr::Var(base_slot),
        line: 1,
    });
    body.push(vm::Stmt::Assign {
        kind: vm::AssignmentKind::Set,
        index: iter,
        expr: vm::Expr::Add(Box::new(vm::Expr::Var(iter)), Box::new(vm::Expr::Int(1))),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Let {
        index: iter,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: vm::Expr::Int(0),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Let {
        index: first,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: vm::Expr::Int(0),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Let {
        index: second,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: vm::Expr::Int(0),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::While {
        condition: vm::Expr::Lt(
            Box::new(vm::Expr::Div(
                Box::new(vm::Expr::Var(iter)),
                Box::new(vm::Expr::Float(1.0)),
            )),
            Box::new(vm::Expr::Float(2.0)),
        ),
        body,
        line: 1,
    });
    let distinct = vm::Expr::Not(Box::new(vm::Expr::Eq(
        Box::new(vm::Expr::Var(first)),
        Box::new(vm::Expr::Var(second)),
    )));
    let payload_matches = |base: u16| {
        vm::Expr::Eq(
            Box::new(vm::Expr::Div(
                Box::new(vm::Expr::Call(
                    read,
                    Vec::new(),
                    vec![
                        vm::Expr::Var(carrier),
                        vm::Expr::Div(
                            Box::new(vm::Expr::Add(
                                Box::new(vm::Expr::Var(base)),
                                Box::new(vm::Expr::Int(5)),
                            )),
                            Box::new(vm::Expr::Float(1.0)),
                        ),
                    ],
                    None,
                    None,
                )),
                Box::new(vm::Expr::Float(1.0)),
            )),
            Box::new(vm::Expr::Float(1.0)),
        )
    };
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::And(
            Box::new(distinct),
            Box::new(vm::Expr::And(
                Box::new(payload_matches(first)),
                Box::new(payload_matches(second)),
            )),
        ),
        line: 1,
    });
    Ok(())
}

impl vm::SourcePlugin for AllocStressPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-alloc-stress"),
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
        let mut ir = pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        append_distinctness_driver(&mut ir)?;
        Ok(ir)
    }
}

fn alloc_stress_vm(source: &str, native: bool) -> Vm {
    let catalog = js_realm_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(&ALLOC_STRESS_PLUGIN)
        .with_host_api_catalog(std::sync::Arc::clone(&catalog));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        alloc_stress_flavor(),
        options,
    )
    .expect("stress compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified stress compilation should create a source-certified VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = vm::HostFunctionRegistry::restricted();
    pd_vm_compat_frontends::runtime::array::typescript_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("manual arrays adapters should match the effective catalog");
    pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("owned adapters should match the effective catalog");
    pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("snapshot adapter should match the effective catalog");
    pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("scalar-math adapter should match the effective catalog");
    registry
        .allow_builtin("math::floor")
        .expect("math::floor should be allow-listable");
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

fn alloc_stress_bool(machine: &mut Vm) -> Value {
    assert_eq!(
        machine.run().expect("stress run should finish"),
        VmStatus::Halted,
        "stress program should halt"
    );
    let value = machine
        .stack()
        .last()
        .cloned()
        .expect("stress program should leave a value");
    assert!(
        matches!(value, Value::Bool(_)),
        "stress driver must report a Bool, got {value:?}"
    );
    value
}

#[test]
fn js_alloc_stress_same_site_replays_distinct_extents() {
    // Genuine behavioral RED on compile-time constant bases: the same real
    // emission region runs twice, so a static base observes equal extents.
    // A length-backed runtime allocator observes distinct extents instead.
    let mut machine = alloc_stress_vm("var a=[1];a[0];", false);
    assert_eq!(
        alloc_stress_bool(&mut machine),
        Value::Bool(true),
        "same-site replay must observe distinct extents"
    );
}

#[test]
fn js_alloc_stress_same_site_replays_distinct_extents_native() {
    let mut machine = alloc_stress_vm("var a=[1];a[0];", true);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(
        alloc_stress_bool(&mut machine),
        Value::Bool(true),
        "same-site replay must observe distinct extents natively"
    );
    let count = machine.aot_exec_count();
    let info = machine.dump_aot_info();
    eprintln!("JS_ALLOC_STRESS_NATIVE count={count}\n{info}");
    assert!(
        count > 1,
        "actual native execution must exceed one dispatch, got {count}"
    );
    assert!(
        info.contains("lowering=ssa"),
        "native lowering must stay ssa"
    );
}

#[test]
fn js_realm_three_weighted_reads_keep_distinct_bases() {
    // Three allocations then weighted reads: 300 + 20 + 1 = 321.
    // Overlapping extents misread (a prior IR path observed 333 here).
    for native in [false, true] {
        let (mut machine, _) = js_realm_vm(
            "var a=[300];var b=[20];var c=[1];a[0]+b[0]+c[0];",
            pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
            native,
        );
        assert_eq!(
            js_realm_unboxed_number(&mut machine),
            Value::Float(321.0),
            "three weighted reads must keep distinct bases (native={native})"
        );
        if native {
            let count = machine.aot_exec_count();
            let info = machine.dump_aot_info();
            eprintln!("JS_REALM_THREE_ALLOCS_NATIVE count={count}\n{info}");
            assert!(
                count > 1,
                "actual native execution must exceed one dispatch, got {count}"
            );
            assert!(
                info.contains("lowering=ssa"),
                "native lowering must stay ssa"
            );
        }
    }
}

#[test]
fn utf16_multiunit_mismatch_at_tail_is_false_native() {
    // Length-1 comparisons cannot catch a unit-walk truncation: `ab` versus
    // `ac` differs only in the final unit and must compare false natively.
    let (mut machine, _) = js_realm_vm(
        "var a=\"ab\";var b=\"ac\";a===b;",
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        true,
    );
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(
        js_realm_unboxed_bool(&mut machine),
        Value::Bool(false),
        "multi-unit strings differing at the tail must compare false"
    );
    let count = machine.aot_exec_count();
    let info = machine.dump_aot_info();
    eprintln!("JS_UTF16_TAIL_MISMATCH_NATIVE count={count}\n{info}");
    assert!(
        count > 1,
        "actual native execution must exceed one dispatch, got {count}"
    );
    assert!(
        info.contains("lowering=ssa"),
        "native lowering must stay ssa"
    );
}

// ---- Private allocator guard probes (test-only) ----
// Each proxy below delegates the original real JS source to the REAL JS
// plugin, then appends one genuine call to the REAL product allocator with
// a crafted count. Invalid/exhausted requests must abort with the existing
// `assertion failed` guard BEFORE any resize: a zero count would resize to
// the live length (a silent no-op), while an over-budget extent would fail
// in B1 quota enforcement instead. The asserted error text discriminates
// both orderings.
fn append_guard_probe(ir: &mut vm::FrontendIr, count: i64) -> Result<(), vm::ParseError> {
    let make = ir
        .functions
        .iter()
        .find(|function| function.name == "__compat_ref_make")
        .map(|function| function.index)
        .ok_or_else(|| {
            stress_rejection("guard fixture requires the real make helper".to_owned())
        })?;
    let begin = ir
        .functions
        .iter()
        .find(|function| function.name == "__compat_es5_node_begin")
        .map(|function| function.index)
        .ok_or_else(|| {
            stress_rejection("guard fixture requires the real node-begin helper".to_owned())
        })?;
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
        return Err(stress_rejection(format!(
            "guard fixture requires exactly one real carrier, found {}",
            carriers.len()
        )));
    };
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Call(
            begin,
            Vec::new(),
            vec![
                vm::Expr::Var(*carrier),
                vm::Expr::Int(count),
                vm::Expr::Int(1),
            ],
            None,
            None,
        ),
        line: 1,
    });
    Ok(())
}

struct AllocGuardZeroPlugin;
struct AllocGuardExhaustedPlugin;

static ALLOC_GUARD_ZERO_PLUGIN: AllocGuardZeroPlugin = AllocGuardZeroPlugin;
static ALLOC_GUARD_EXHAUSTED_PLUGIN: AllocGuardExhaustedPlugin = AllocGuardExhaustedPlugin;

fn alloc_guard_zero_flavor() -> vm::SourceFlavor {
    vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-alloc-guard-zero"))
}

fn alloc_guard_exhausted_flavor() -> vm::SourceFlavor {
    vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-alloc-guard-exhausted"))
}

fn guard_descriptor(id: &'static str) -> vm::SourceFrontendDescriptor {
    vm::SourceFrontendDescriptor {
        id: vm::FrontendId::new(id),
        extensions: &["js"],
        typing_policy: vm::FrontendTypingPolicy::Strict,
        supports_ownership_checking: true,
    }
}

impl vm::SourcePlugin for AllocGuardZeroPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        guard_descriptor("javascript-es5-alloc-guard-zero")
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        append_guard_probe(&mut ir, 0)?;
        Ok(ir)
    }
}

impl vm::SourcePlugin for AllocGuardExhaustedPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        guard_descriptor("javascript-es5-alloc-guard-exhausted")
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        append_guard_probe(&mut ir, 65536)?;
        Ok(ir)
    }
}

fn alloc_probe_vm(
    source: &str,
    plugin: &'static dyn vm::SourcePlugin,
    flavor: vm::SourceFlavor,
) -> Vm {
    let catalog = js_realm_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(plugin)
        .with_host_api_catalog(std::sync::Arc::clone(&catalog));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source, flavor, options,
    )
    .expect("probe compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified probe compilation should create a source-certified VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = vm::HostFunctionRegistry::restricted();
    pd_vm_compat_frontends::runtime::array::typescript_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("manual arrays adapters should match the effective catalog");
    pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("owned adapters should match the effective catalog");
    pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("snapshot adapter should match the effective catalog");
    pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("scalar-math adapter should match the effective catalog");
    registry
        .allow_builtin("math::floor")
        .expect("math::floor should be allow-listable");
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    machine
}

fn expect_guard_refusal(machine: &mut Vm, label: &str) {
    let outcome = machine.run();
    assert!(outcome.is_err(), "{label} must refuse, got {outcome:?}");
    let text = format!("{outcome:?}");
    assert!(
        text.contains("assertion failed"),
        "{label} must abort in the allocator guard before resize, got {text}"
    );
    if let Some(stats) = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(machine)
    {
        assert_eq!(
            stats.pending, 0,
            "{label}: no pending allocation may survive refusal"
        );
        assert_eq!(
            stats.registered, 1,
            "{label}: the single realm owner predates the refused request"
        );
    }
}

#[test]
fn js_alloc_guard_zero_count_refuses_before_resize() {
    let mut machine = alloc_probe_vm(
        "var a=[1];a[0];",
        &ALLOC_GUARD_ZERO_PLUGIN,
        alloc_guard_zero_flavor(),
    );
    expect_guard_refusal(&mut machine, "zero-count allocation");
}

#[test]
fn js_alloc_guard_exhausted_extent_refuses_before_resize() {
    let mut machine = alloc_probe_vm(
        "var a=[1];a[0];",
        &ALLOC_GUARD_EXHAUSTED_PLUGIN,
        alloc_guard_exhausted_flavor(),
    );
    expect_guard_refusal(&mut machine, "exhausted-extent allocation");
}
