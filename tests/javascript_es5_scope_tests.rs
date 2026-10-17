//! P3-A declaration instantiation and bound-identifier References.
//!
//! These cases fail on cb706063 because the lowerer still rejects
//! uninitialized var, redeclaration, function declarations, assignment and
//! typeof. That rejection is feature absence, not a JS exception result.
//! Oracle ids refer to the parent Node reference; frontend sources never use
//! the mixed-array observation wrappers from oracle cases 1 and 7.

use pd_vm_compat_frontends::{JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor};
use vm::{HostFunctionRegistry, Value, Vm, VmStatus};

const OBSERVE_ID: &str = "javascript-es5-scope-observe";
const MAKE_NAME: &str = "__compat_ref_make";
const READ_NAME: &str = "__compat_ref_read";
const CHECKED_BASE_NAME: &str = "__compat_es5_checked_base";
const OBSERVE_NAME: &str = "__compat_es5_heap_observe";

struct ScopeObservePlugin {
    reads: Vec<String>,
    /// `(binding, unit count)` live UTF-16 read of an already-produced string.
    string_units: Option<(String, usize)>,
    /// Append production safepoint counters after the binding reads.
    safepoints: bool,
}

fn rejection(message: String) -> vm::ParseError {
    vm::ParseError::at_span(vm::compiler::source_map::Span::new(0, 0, 0), message)
}

fn helper_index(ir: &vm::FrontendIr, name: &str) -> Result<u16, vm::ParseError> {
    ir.functions
        .iter()
        .find(|function| function.name == name)
        .map(|function| function.index)
        .ok_or_else(|| rejection(format!("scope observer requires helper '{name}'")))
}

fn binding_slot(ir: &vm::FrontendIr, name: &str) -> Result<u16, vm::ParseError> {
    ir.local_bindings
        .iter()
        .rfind(|(binding, _)| binding == name)
        .map(|(_, slot)| *slot)
        .ok_or_else(|| rejection(format!("scope observer requires binding `{name}`")))
}

fn fresh_slot(ir: &mut vm::FrontendIr) -> Result<u16, vm::ParseError> {
    let slot = u16::try_from(ir.locals)
        .map_err(|_| rejection("scope observer slot space exhausted".to_owned()))?;
    ir.locals += 1;
    Ok(slot)
}

fn realm_carrier(ir: &vm::FrontendIr) -> Result<u16, vm::ParseError> {
    let make = helper_index(ir, MAKE_NAME)?;
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
        return Err(rejection(format!(
            "scope observer requires exactly one realm carrier, found {}",
            carriers.len()
        )));
    };
    Ok(*carrier)
}

fn append_binding_reads(ir: &mut vm::FrontendIr, reads: &[String]) -> Result<(), vm::ParseError> {
    for name in reads {
        let slot = binding_slot(ir, name)?;
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(slot),
            line: 1,
        });
    }
    Ok(())
}

fn append_string_units(
    ir: &mut vm::FrontendIr,
    binding: &str,
    units: usize,
) -> Result<(), vm::ParseError> {
    let binding = binding_slot(ir, binding)?;
    let carrier = realm_carrier(ir)?;
    let read = helper_index(ir, READ_NAME)?;
    let checked_base = helper_index(ir, CHECKED_BASE_NAME)?;
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
    let base_slot = fresh_slot(ir)?;
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
        vm::Expr::Div(
            Box::new(vm::Expr::Call(
                read,
                Vec::new(),
                vec![
                    vm::Expr::Var(carrier),
                    vm::Expr::Div(Box::new(physical), Box::new(vm::Expr::Float(1.0))),
                ],
                None,
                None,
            )),
            Box::new(vm::Expr::Float(1.0)),
        )
    };
    let length_slot = fresh_slot(ir)?;
    ir.stmts.push(vm::Stmt::Let {
        index: length_slot,
        declared_schema: Some(vm::TypeSchema::Number),
        expr: read_cell(2),
        line: 1,
    });
    let mut unit_slots = Vec::with_capacity(units);
    for position in 0..units {
        let slot = fresh_slot(ir)?;
        ir.stmts.push(vm::Stmt::Let {
            index: slot,
            declared_schema: Some(vm::TypeSchema::Number),
            expr: read_cell(3 + position as i64),
            line: 1,
        });
        unit_slots.push(slot);
    }
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

fn append_safepoints(ir: &mut vm::FrontendIr) -> Result<(), vm::ParseError> {
    let carrier = realm_carrier(ir)?;
    let observe = helper_index(ir, OBSERVE_NAME)?;
    for which in [5_i64, 0, 3] {
        let out = fresh_slot(ir)?;
        ir.stmts.push(vm::Stmt::Let {
            index: out,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: vm::Expr::Call(
                observe,
                Vec::new(),
                vec![
                    vm::Expr::Var(carrier),
                    vm::Expr::Int(which),
                    vm::Expr::Int(0),
                ],
                None,
                None,
            ),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::Var(out),
            line: 1,
        });
    }
    Ok(())
}

impl vm::SourcePlugin for ScopeObservePlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new(OBSERVE_ID),
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
        append_binding_reads(&mut ir, &self.reads)?;
        if let Some((binding, units)) = &self.string_units {
            if binding == "#expression" {
                // Test-only metadata names the real expression-result slot;
                // it adds no binding store or logical GC root.
                let output = ir
                    .stmts
                    .iter()
                    .find_map(|stmt| match stmt {
                        vm::Stmt::Expr {
                            expr: vm::Expr::Var(slot),
                            ..
                        } => Some(*slot),
                        _ => None,
                    })
                    .ok_or_else(|| rejection("expected real expression output".into()))?;
                ir.local_bindings.push((binding.clone(), output));
                let carrier = realm_carrier(&ir)?;
                let collect = helper_index(&ir, ADDRESS01_COLLECT)?;
                let ack = fresh_slot(&mut ir)?;
                ir.stmts.push(vm::Stmt::Let {
                    index: ack,
                    declared_schema: Some(vm::TypeSchema::Int),
                    expr: vm::Expr::Call(
                        collect,
                        Vec::new(),
                        vec![vm::Expr::Var(carrier)],
                        None,
                        None,
                    ),
                    line: 1,
                });
            }
            append_string_units(&mut ir, binding, *units)?;
        }
        if self.safepoints {
            append_safepoints(&mut ir)?;
        }
        Ok(ir)
    }
}

fn catalog() -> std::sync::Arc<vm::HostApiCatalog> {
    pd_vm_compat_frontends::reference_array_effective_catalog()
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

fn observe_plugin(
    reads: &[&str],
    string_units: Option<(&str, usize)>,
    safepoints: bool,
) -> &'static ScopeObservePlugin {
    Box::leak(Box::new(ScopeObservePlugin {
        reads: reads.iter().map(|name| (*name).to_owned()).collect(),
        string_units: string_units.map(|(name, units)| (name.to_owned(), units)),
        safepoints,
    }))
}

fn flavor() -> vm::SourceFlavor {
    vm::SourceFlavor::Plugin(vm::FrontendId::new(OBSERVE_ID))
}

fn run_heap_free(source: &str, reads: &[&str]) -> Vec<Value> {
    let plugin = observe_plugin(reads, None, false);
    let compiled = vm::compile_source_with_flavor_and_options(
        source,
        flavor(),
        vm::CompileSourceFileOptions::new().with_source_plugin(plugin),
    )
    .unwrap_or_else(|error| {
        panic!(
            "heap-free source must compile, got feature/setup refusal: {error:?}\nsource: {source}"
        )
    });
    let mut machine = Vm::new(compiled.program);
    machine.set_fuel(1_000_000);
    assert_eq!(
        machine
            .run()
            .expect("heap-free source should execute without B1"),
        VmStatus::Halted
    );
    let stack = machine.stack();
    assert!(
        stack.len() >= reads.len(),
        "expected {} binding reads, stack={stack:?}",
        reads.len()
    );
    stack[stack.len() - reads.len()..].to_vec()
}

fn run_realm(
    source: &str,
    reads: &[&str],
    string_units: Option<(&str, usize)>,
    safepoints: bool,
    native: bool,
) -> (Vec<Value>, Vec<Value>, Option<(u64, String)>) {
    let plugin = observe_plugin(reads, string_units, safepoints);
    let catalog = catalog();
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        flavor(),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(plugin)
            .with_host_api_catalog(std::sync::Arc::clone(&catalog)),
    )
    .unwrap_or_else(|error| {
        panic!("realm source must compile, got feature/setup refusal: {error:?}\nsource: {source}")
    });
    let mut machine = compiled
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
        assert_eq!(machine.aot_exec_count(), 0, "no execution before run");
    }
    machine.set_fuel(100_000_000);
    match machine.run() {
        Ok(status) => assert_eq!(status, VmStatus::Halted, "scope program should halt"),
        Err(error) => panic!(
            "scope run failed ip={} error={error:?} stack={:?} frames={:?}",
            machine.ip(),
            machine.stack(),
            machine.execution_frames()
        ),
    }
    let native_evidence = if native {
        let count = machine.aot_exec_count();
        let info = machine.dump_aot_info();
        assert!(count > 0, "no actual native execution; info: {info}");
        assert!(
            info.contains("lowering=ssa"),
            "not SSA native; info: {info}"
        );
        Some((count, info))
    } else {
        None
    };
    let stack = machine.stack().to_vec();
    let string_len = string_units.map(|(_, units)| units + 1).unwrap_or(0);
    let safe_len = if safepoints { 3 } else { 0 };
    let tail = string_len + safe_len;
    assert!(
        stack.len() >= reads.len() + tail,
        "stack too short: {stack:?}"
    );
    let end = stack.len() - tail;
    let bindings = stack[end - reads.len()..end].to_vec();
    let extra = stack[end..].to_vec();
    (bindings, extra, native_evidence)
}

fn tag_of(value: &Value) -> i64 {
    let Value::Map(fields) = value else {
        panic!("expected a boxed JsValue, got {value:?}");
    };
    match fields.get(&Value::string("tag")) {
        Some(Value::Int(tag)) => *tag,
        other => panic!("boxed tag must be Int, got {other:?}"),
    }
}

fn assert_undefined(value: &Value) {
    assert_eq!(tag_of(value), 0, "expected Undefined, got {value:?}");
}

fn assert_null(value: &Value) {
    assert_eq!(tag_of(value), 1, "expected Null, got {value:?}");
}

fn assert_number(value: &Value, expected: f64) {
    assert_eq!(tag_of(value), 3, "expected Number, got {value:?}");
    let Value::Map(fields) = value else {
        unreachable!()
    };
    match fields.get(&Value::string("number")) {
        Some(Value::Float(number)) => assert_eq!(*number, expected, "number payload"),
        other => panic!("JS number must remain Float, got {other:?}"),
    }
}

fn assert_bool(value: &Value, expected: bool) {
    assert_eq!(tag_of(value), 2, "expected Boolean, got {value:?}");
    let Value::Map(fields) = value else {
        unreachable!()
    };
    match fields.get(&Value::string("flag")) {
        Some(Value::Bool(flag)) => assert_eq!(*flag, expected, "boolean flag"),
        other => panic!("JS Boolean payload must be Bool, got {other:?}"),
    }
}

fn assert_objectish(value: &Value) {
    assert_eq!(tag_of(value), 5, "expected object-like tag, got {value:?}");
}

fn units_of(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn assert_string_tail(stack_len_hint: &[Value], expected: &str) {
    let expected_units = units_of(expected);
    let tail = stack_len_hint;
    assert_eq!(tail.len(), expected_units.len() + 1, "length plus units");
    match &tail[0] {
        Value::Float(length) => assert_eq!(*length, expected_units.len() as f64),
        other => panic!("string length must be Float, got {other:?}"),
    }
    for (index, unit) in expected_units.iter().enumerate() {
        match &tail[index + 1] {
            Value::Float(found) => assert_eq!(*found, f64::from(*unit), "unit {index}"),
            other => panic!("unit {index} must be Float, got {other:?}"),
        }
    }
}

fn compile_text(source: &str) -> Result<(), String> {
    let catalog = catalog();
    match vm::compile_source_with_flavor_and_options(
        source,
        javascript_es5_flavor(),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN)
            .with_host_api_catalog(catalog),
    ) {
        Ok(_) => Ok(()),
        Err(error) => Err(format!("{error:?}")),
    }
}

fn must_refuse(source: &str) -> String {
    match compile_text(source) {
        Ok(()) => panic!("source must stay outside this profile: {source}"),
        Err(text) => {
            assert!(
                !text.contains("ReferenceError") && !text.contains("TypeError"),
                "compile refusal must not be labeled as a JS exception: {text}"
            );
            text
        }
    }
}

#[test]
fn declaration_before_initializer_is_undefined_oracle1() {
    // frontend_source omits oracle case 1's mixed array wrapper.
    let values = run_heap_free("var before=x;var x=7;", &["before", "x"]);
    assert_undefined(&values[0]);
    assert_number(&values[1], 7.0);
}

#[test]
fn repeated_var_does_not_reset_oracle2() {
    let values = run_heap_free("var x=7;var x;x;", &["x"]);
    assert_number(&values[0], 7.0);
}

#[test]
fn forward_function_declaration_call_oracle3() {
    let (values, _, _) = run_realm(
        "var r=f();function f(){\"use strict\";return 7;}r;",
        &["r"],
        None,
        false,
        false,
    );
    assert_number(&values[0], 7.0);
}

#[test]
fn forward_function_declaration_call_native_ssa() {
    let (values, _, evidence) = run_realm(
        "var r=f();function f(){\"use strict\";return 7;}r;",
        &["r"],
        None,
        false,
        true,
    );
    assert_number(&values[0], 7.0);
    let (count, info) = evidence.expect("native evidence");
    eprintln!("P3_DECL_FORWARD_NATIVE count={count}\n{info}");
}

#[test]
fn var_then_function_declaration_is_callable_oracle4() {
    let (values, _, _) = run_realm(
        "var f;function f(){\"use strict\";return 7;}var r=f();",
        &["r"],
        None,
        false,
        false,
    );
    assert_number(&values[0], 7.0);
}

#[test]
fn function_then_var_initializer_overwrites_oracle5() {
    let (values, _, _) = run_realm(
        "function f(){\"use strict\";return 7;}var f=9;f;",
        &["f"],
        None,
        false,
        false,
    );
    assert_number(&values[0], 9.0);
}

#[test]
fn duplicate_function_declarations_last_wins_oracle6() {
    let (values, _, _) = run_realm(
        "var r=f();function f(){\"use strict\";return 1;}function f(){\"use strict\";return 2;}r;",
        &["r"],
        None,
        false,
        false,
    );
    assert_number(&values[0], 2.0);
}

#[test]
fn function_local_shadow_does_not_capture_outer_oracle7() {
    // One call. Observer reads the already-produced result and the outer binding.
    let (values, _, _) = run_realm(
        "var x=7;function f(){\"use strict\";return x;var x=2;}var r=f();",
        &["r", "x"],
        None,
        false,
        false,
    );
    assert_undefined(&values[0]);
    assert_number(&values[1], 7.0);
}

#[test]
fn function_local_shadow_and_assignment_native_ssa() {
    let (values, _, evidence) = run_realm(
        "var x=7;function f(){\"use strict\";return x;var x=2;}var r=f();var y=x;x=3;",
        &["r", "x", "y"],
        None,
        false,
        true,
    );
    assert_undefined(&values[0]);
    assert_number(&values[1], 3.0);
    assert_number(&values[2], 7.0);
    let (count, info) = evidence.expect("native evidence");
    eprintln!("P3_DECL_SHADOW_NATIVE count={count}\n{info}");
}

#[test]
fn dead_declaration_only_if_typeof_is_undefined_string_oracle8() {
    let (values, extra, _) = run_realm(
        "if(false){var x=1;}var t=typeof x;",
        &["x"],
        Some(("t", 9)),
        false,
        false,
    );
    assert_undefined(&values[0]);
    assert_string_tail(&extra, "undefined");
}

#[test]
fn selected_declaration_only_if_runs_initializer() {
    let (values, _, _) = run_realm("if(true){var x=1;}x;", &["x"], None, false, false);
    assert_number(&values[0], 1.0);
}

#[test]
fn nested_declaration_only_containers_keep_source_order() {
    let (values, _, _) = run_realm(
        "if(true){{if(false){var y=9;}var x=2;}}",
        &["x", "y"],
        None,
        false,
        false,
    );
    assert_number(&values[0], 2.0);
    assert_undefined(&values[1]);
}

#[test]
fn assignment_copies_value_not_binding_oracle9() {
    let values = run_heap_free("var x=1;var y=x;x=2;y;", &["x", "y"]);
    assert_number(&values[0], 2.0);
    assert_number(&values[1], 1.0);
}

#[test]
fn uninitialized_then_assignment_and_chained_rhs_once() {
    let values = run_heap_free("var x;x=7;var y;var r=(x=y=8);", &["x", "y", "r"]);
    assert_number(&values[0], 8.0);
    assert_number(&values[1], 8.0);
    assert_number(&values[2], 8.0);
}

#[test]
fn typeof_unresolved_is_undefined_string_without_getvalue() {
    let plugin = observe_plugin(&[], Some(("t", 9)), false);
    let catalog = catalog();
    let source = "var t=typeof missing;";
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        flavor(),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(plugin)
            .with_host_api_catalog(catalog.clone()),
    )
    .unwrap_or_else(|error| panic!("typeof missing must compile, got {error:?}"));
    let mut machine = compiled.into_vm().expect("vm");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("init");
    let mut registry = HostFunctionRegistry::restricted();
    install_production(&mut registry, &catalog);
    registry.bind_vm_cached(&mut machine).expect("bind");
    machine.set_fuel(10_000_000);
    assert_eq!(machine.run().expect("run"), VmStatus::Halted);
    let stack = machine.stack();
    assert_string_tail(&stack[stack.len() - 10..], "undefined");
}

fn assert_typeof(source: &str, expected: &str, native: bool) {
    let units = units_of(expected);
    let plugin = observe_plugin(&[], Some(("t", units.len())), false);
    let catalog = catalog();
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        flavor(),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(plugin)
            .with_host_api_catalog(std::sync::Arc::clone(&catalog)),
    )
    .unwrap_or_else(|error| panic!("typeof source must compile: {source}\n{error:?}"));
    let mut machine = compiled.into_vm().expect("vm");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("init");
    let mut registry = HostFunctionRegistry::restricted();
    install_production(&mut registry, &catalog);
    registry.bind_vm_cached(&mut machine).expect("bind");
    if native {
        machine.compile_aot().expect("native");
        assert_eq!(machine.aot_exec_count(), 0);
    }
    machine.set_fuel(10_000_000);
    assert_eq!(machine.run().expect("run"), VmStatus::Halted);
    if native {
        let count = machine.aot_exec_count();
        let info = machine.dump_aot_info();
        eprintln!("P3_DECL_TYPEOF_NATIVE count={count}\n{info}");
        assert!(count > 0, "no native execution; {info}");
        assert!(info.contains("lowering=ssa"), "not SSA; {info}");
    }
    let stack = machine.stack();
    let start = stack.len() - (units.len() + 1);
    assert_string_tail(&stack[start..], expected);
}

#[test]
fn typeof_reports_actual_runtime_tags() {
    assert_typeof("var t=typeof null;", "object", false);
    assert_typeof("var t=typeof 1;", "number", false);
    assert_typeof("var t=typeof true;", "boolean", false);
    assert_typeof("var t=typeof false;", "boolean", false);
    assert_typeof("var t=typeof \"ab\";", "string", false);
    assert_typeof("var t=typeof [1,2];", "object", false);
    assert_typeof("var t=typeof {n:1};", "object", false);
    assert_typeof(
        "function f(){\"use strict\";return 1;}var t=typeof f;",
        "function",
        false,
    );
    assert_typeof("var o={n:true};var t=typeof o.n;", "boolean", false);
    assert_typeof(
        "function f(){\"use strict\";return false;}var t=typeof f();",
        "boolean",
        true,
    );
}

#[test]
fn escaped_and_direct_spellings_share_one_binding() {
    let values = run_heap_free("var \\u0078=1;var x=2;x;", &["x"]);
    assert_number(&values[0], 2.0);
}

#[test]
fn function_local_escaped_name_shadows_outer() {
    let (values, _, _) = run_realm(
        "var x=1;function f(){\"use strict\";var \\u0078=2;return x;}var r=f();",
        &["r", "x"],
        None,
        false,
        false,
    );
    assert_number(&values[0], 2.0);
    assert_number(&values[1], 1.0);
}

#[test]
fn function_local_undefined_shadows_builtin() {
    let (values, _, _) = run_realm(
        "function f(){\"use strict\";return undefined;var undefined=1;}var early=f();function g(){\"use strict\";var undefined=1;return undefined;}var late=g();",
        &["early", "late"],
        None,
        false,
        false,
    );
    assert_undefined(&values[0]);
    assert_number(&values[1], 1.0);
}

#[test]
fn repeated_boolean_var_keeps_actual_flags() {
    let values = run_heap_free("var bt=true;var bf=false;var bt;var bf;", &["bt", "bf"]);
    assert_bool(&values[0], true);
    assert_bool(&values[1], false);
}

#[test]
fn assignment_preserves_alias_and_releases_overwritten_root() {
    let source = "var keep={n:7,s:\"\\uD800\\0\"};var alias=keep;var a=[7,8];var old=a;var a=[1];var f=function(){\"use strict\";return 1;};var f=9;";
    let (values, _, _) = run_realm(source, &["alias", "old", "a", "f"], None, false, false);
    assert_objectish(&values[0]);
    assert_objectish(&values[1]);
    assert_objectish(&values[2]);
    assert_number(&values[3], 9.0);
    let (read, _, _) = run_realm(
        "var keep={n:7};var alias=keep;keep=null;var n=alias.n;",
        &["keep", "n"],
        None,
        false,
        false,
    );
    assert_null(&read[0]);
    assert_number(&read[1], 7.0);
}

#[test]
fn overwritten_binding_survives_automatic_collection() {
    let mut source = String::from("var keep={n:7};var alias=keep;var x=1;");
    for i in 0..40 {
        source.push_str(&format!("var t{i}={{k:{i}}};"));
    }
    source.push_str("var x={n:8};var n=alias.n;");
    let (values, extra, _) = run_realm(&source, &["n", "x"], None, true, false);
    assert_number(&values[0], 7.0);
    assert_objectish(&values[1]);
    let collections = match extra.first() {
        Some(Value::Int(n)) => *n,
        Some(Value::Float(n)) => *n as i64,
        other => panic!("collection counter must be numeric, got {other:?}"),
    };
    assert!(
        collections > 0,
        "automatic collection did not run, counters={extra:?}"
    );
}

#[test]
fn strict_eval_binding_remains_an_early_error() {
    let text = must_refuse("function f(){\"use strict\";var eval=1;return eval;}");
    assert!(
        text.contains("eval") || text.contains("strict"),
        "strict eval binding must stay rejected, got {text}"
    );
}

#[test]
fn global_undefined_redeclaration_stays_unsupported() {
    let text = must_refuse("var undefined=1;undefined;");
    assert!(
        text.contains("undefined"),
        "global undefined redeclaration must stay refused, got {text}"
    );
}

#[test]
fn unresolved_read_is_not_a_js_reference_error() {
    let text = must_refuse("x;");
    assert!(
        text.contains("unknown")
            || text.contains("unresolved")
            || text.contains("P0")
            || text.contains("P3"),
        "unresolved read stays a compile refusal, got {text}"
    );
}

#[test]
fn outer_binding_is_p4_not_a_copied_local() {
    let text = must_refuse("var x=1;var f=function(){\"use strict\";return x;};f();");
    assert!(
        text.contains("P4") || text.contains("free"),
        "outer binding must be a labeled P4 refusal, got {text}"
    );
}

#[test]
fn nonliteral_if_and_exceptions_stay_outside_this_profile() {
    let if_text = must_refuse("if(1){var x=1;}");
    assert!(
        !if_text.contains("ReferenceError"),
        "non-literal if must not invent an exception result: {if_text}"
    );
    let _throw = must_refuse("throw 1;");
    let _catch = must_refuse("try{throw 1;}catch(e){e;}");
}

const ADDRESS01_RECLAIM_ID: &str = "javascript-es5-address01-reclaim";
const ADDRESS01_OWN_ID: &str = "javascript-es5-address01-own";
const ADDRESS01_COLLECT: &str = "__compat_es5_heap_collect";
const ADDRESS01_DECODE: &str = "__compat_es5_decode_int";
const STALE_PROBE_MARKER: &str = "address02-stale-probe";
const OBSERVE_LIVE_NODES: i64 = 0;
const OBSERVE_NODE_CELLS: i64 = 7;

struct ReclaimPlugin {
    binding: String,
}

impl vm::SourcePlugin for ReclaimPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new(ADDRESS01_RECLAIM_ID),
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
        append_reclaim_probe(&mut ir, &self.binding)?;
        Ok(ir)
    }
}

fn append_reclaim_probe(ir: &mut vm::FrontendIr, binding_name: &str) -> Result<(), vm::ParseError> {
    let binding = binding_slot(ir, binding_name)?;
    let carrier = realm_carrier(ir)?;
    let collect = helper_index(ir, ADDRESS01_COLLECT)?;
    let checked = helper_index(ir, CHECKED_BASE_NAME)?;
    let decode = helper_index(ir, ADDRESS01_DECODE)?;
    let get = vm::BuiltinFunction::Get.call_index();
    let assign_at = ir
        .stmts
        .iter()
        .position(|stmt| matches!(stmt, vm::Stmt::Assign { index, .. } if *index == binding))
        .ok_or_else(|| {
            rejection("reclaim probe requires an assignment to the overwritten binding".into())
        })?;
    let mut take = || -> Result<u16, vm::ParseError> {
        let slot = u16::try_from(ir.locals)
            .map_err(|_| rejection("reclaim probe slot space exhausted".into()))?;
        ir.locals += 1;
        Ok(slot)
    };
    let old_slot = take()?;
    let old_gen = take()?;
    let copied = take()?;
    let ack = take()?;
    let base = take()?;
    let box_schema = ir.stmts.iter().find_map(|stmt| match stmt {
        vm::Stmt::Let {
            index,
            declared_schema,
            ..
        } if *index == binding => declared_schema.clone(),
        _ => None,
    });
    let stash = |field: &str, out: u16| vm::Stmt::Let {
        index: out,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: vm::Expr::Call(
            decode,
            Vec::new(),
            vec![vm::Expr::Div(
                Box::new(vm::Expr::Call(
                    get,
                    Vec::new(),
                    vec![vm::Expr::Var(copied), vm::Expr::String(field.to_owned())],
                    None,
                    None,
                )),
                Box::new(vm::Expr::Float(1.0)),
            )],
            None,
            None,
        ),
        line: 1,
    };
    // Keep the production binding Assign/refresh contiguous. The following
    // unrooted copy and Int field extraction cannot allocate or collect.
    let root_write = helper_index(ir, "__compat_es5_root_write")?;
    if !matches!(ir.stmts.get(assign_at + 1), Some(vm::Stmt::Let {
        expr: vm::Expr::Call(target, _, _, _, _), ..
    }) if *target == root_write)
    {
        return Err(rejection(
            "stale probe requires actual binding refresh".into(),
        ));
    }
    ir.stmts.insert(
        assign_at + 2,
        vm::Stmt::Let {
            index: copied,
            declared_schema: box_schema,
            expr: vm::Expr::Var(binding),
            line: 1,
        },
    );
    ir.stmts.insert(assign_at + 3, stash("generation", old_gen));
    ir.stmts.insert(assign_at + 3, stash("ref_slot", old_slot));
    ir.stmts.push(vm::Stmt::Let {
        index: ack,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: vm::Expr::Call(
            collect,
            Vec::new(),
            vec![vm::Expr::Var(carrier)],
            None,
            None,
        ),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::String(STALE_PROBE_MARKER.to_owned()),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Let {
        index: base,
        declared_schema: Some(vm::TypeSchema::Int),
        expr: vm::Expr::Call(
            checked,
            Vec::new(),
            vec![
                vm::Expr::Var(carrier),
                vm::Expr::Var(old_slot),
                vm::Expr::Var(old_gen),
            ],
            None,
            None,
        ),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::Var(base),
        line: 1,
    });
    Ok(())
}

fn run_reclaim(source: &str, binding: &str) -> Result<(), String> {
    let plugin = Box::leak(Box::new(ReclaimPlugin {
        binding: binding.to_owned(),
    }));
    let catalog = catalog();
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new(ADDRESS01_RECLAIM_ID)),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(plugin)
            .with_host_api_catalog(std::sync::Arc::clone(&catalog)),
    )
    .map_err(|error| format!("compile refused before reclamation probe: {error:?}"))?;
    let mut machine = compiled
        .into_vm()
        .map_err(|error| format!("certified vm refused: {error:?}"))?;
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .map_err(|error| format!("owned-array init refused: {error:?}"))?;
    let mut registry = HostFunctionRegistry::restricted();
    install_production(&mut registry, &catalog);
    registry
        .bind_vm_cached(&mut machine)
        .map_err(|error| format!("adapter bind refused: {error:?}"))?;
    machine.set_fuel(100_000_000);
    match machine.run() {
        Err(error) => {
            let text = format!("{error:?}");
            let stack = format!("{:?}", machine.stack());
            let reached = stack.contains(STALE_PROBE_MARKER);
            // The only statement after the marker is the production checked-base
            // assert. An earlier assertion never sees the marker, so a generic
            // assertion text is not itself a pass.
            if reached && text.contains("assertion failed") {
                Ok(())
            } else if !reached {
                Err(format!(
                    "collector or source failed before the final stale probe; ip={} error={text} frames={:?}",
                    machine.ip(),
                    machine.execution_frames()
                ))
            } else {
                Err(format!(
                    "final probe was reached but was not the stale checked-base failure; ip={} error={text} frames={:?}",
                    machine.ip(),
                    machine.execution_frames()
                ))
            }
        }
        Ok(status) => Err(format!(
            "overwritten node stayed reachable after collect; status={status:?} stack={:?}",
            machine.stack()
        )),
    }
}

fn expect_reclaimed(source: &str, binding: &str) {
    match run_reclaim(source, binding) {
        Ok(()) => {}
        Err(text) => panic!("reclamation required for {source}: {text}"),
    }
}

struct OwnershipPlugin;

impl vm::SourcePlugin for OwnershipPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new(ADDRESS01_OWN_ID),
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
        let ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        let foreign = foreign_script_reads(&ir);
        if !foreign.is_empty() {
            return Err(rejection(format!(
                "script reads slots defined only in another activation: {foreign:?}"
            )));
        }
        Ok(ir)
    }
}

fn debug_var_slots(text: &str) -> std::collections::HashSet<u16> {
    let mut out = std::collections::HashSet::new();
    let mut rest = text;
    while let Some(pos) = rest.find("Var(") {
        rest = &rest[pos + 4..];
        let digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
        if let Ok(slot) = digits.parse::<u16>() {
            out.insert(slot);
        }
    }
    out
}

fn let_indices(stmts: &[vm::Stmt]) -> std::collections::HashSet<u16> {
    let mut out = std::collections::HashSet::new();
    fn walk(stmts: &[vm::Stmt], out: &mut std::collections::HashSet<u16>) {
        for stmt in stmts {
            if let vm::Stmt::Let { index, .. } = stmt {
                out.insert(*index);
            }
        }
    }
    walk(stmts, &mut out);
    out
}

fn foreign_script_reads(ir: &vm::FrontendIr) -> Vec<u16> {
    let script_lets = let_indices(&ir.stmts);
    let script_reads = debug_var_slots(&format!("{:?}", ir.stmts));
    let mut function_lets = std::collections::HashSet::new();
    for function in &ir.functions {
        if function.name.starts_with("__compat_es5_fn_")
            && let Some(body) = ir.function_impls.get(&function.index)
        {
            function_lets.extend(let_indices(&body.body_stmts));
        }
    }
    let mut foreign: Vec<u16> = script_reads
        .intersection(&function_lets)
        .copied()
        .filter(|slot| !script_lets.contains(slot))
        .collect();
    foreign.sort_unstable();
    foreign
}

fn assert_activation_local_typeof(source: &str) {
    let plugin = Box::leak(Box::new(OwnershipPlugin));
    let catalog = catalog();
    vm::compile_source_with_flavor_and_options(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new(ADDRESS01_OWN_ID)),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(plugin)
            .with_host_api_catalog(catalog),
    )
    .unwrap_or_else(|error| {
        panic!(
            "typeof slots must be defined in the consuming activation: {error:?}\nsource: {source}"
        )
    });
}

fn read_string(source: &str, binding: &str, expected: &str) {
    let (_, extra, _) = run_realm(
        source,
        &[],
        Some((binding, expected.encode_utf16().count())),
        false,
        false,
    );
    assert_string_tail(&extra, expected);
}

struct BindingCountPlugin {
    name: String,
}

impl vm::SourcePlugin for BindingCountPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-address01-bindcount"),
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
        let ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        let count = ir
            .local_bindings
            .iter()
            .filter(|(name, _)| name == &self.name)
            .count();
        if count != 1 {
            return Err(rejection(format!(
                "user binding '{}' must have exactly one metadata entry, found {count}",
                self.name
            )));
        }
        Ok(ir)
    }
}

fn assert_one_user_binding(source: &str, name: &str) {
    let plugin = Box::leak(Box::new(BindingCountPlugin {
        name: name.to_owned(),
    }));
    let catalog = catalog();
    vm::compile_source_with_flavor_and_options(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-address01-bindcount")),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(plugin)
            .with_host_api_catalog(catalog),
    )
    .unwrap_or_else(|error| {
        panic!("binding metadata must keep exactly one '{name}' entry: {error:?}")
    });
}

#[test]
fn address01_i_array_expr_user_flag_stays_true() {
    // Frozen from the test-owned next_slot diagnostic: both __array_expr_0
    // and this name printed next_slot=467. 194 production made r false.
    let (values, _, _) = run_realm(
        "var __array_expr_467=9;var a;a=[7];var r=(__array_expr_467===9);",
        &["r"],
        None,
        false,
        false,
    );
    assert_bool(&values[0], true);
}

#[test]
fn address01_i_array_expr_user_binding_is_unique() {
    assert_one_user_binding(
        "var __array_expr_467=9;var a;a=[7];var r=(__array_expr_467===9);",
        "__array_expr_467",
    );
}

#[test]
fn address01_i_array_expr_user_value_stays_nine() {
    let (values, _, _) = run_realm(
        "var __array_expr_467=9;var a;a=[7];var r=(__array_expr_467===9);",
        &["__array_expr_467"],
        None,
        false,
        false,
    );
    assert_number(&values[0], 9.0);
}

#[test]
fn address01_i_typeof_script_does_not_reuse_function_cache() {
    let source = "function f(){\"use strict\";return typeof 1;}var t=typeof 1;";
    assert_activation_local_typeof(source);
    read_string(source, "t", "number");
}

#[test]
fn address01_i_typeof_two_functions_keep_own_tags() {
    let source = "function f(){\"use strict\";return typeof 1;}function g(){\"use strict\";return typeof true;}var n=typeof 1;var b=typeof true;var fn=f();var gn=g();";
    assert_activation_local_typeof(source);
    read_string(source, "n", "number");
    read_string(source, "b", "boolean");
    read_string(source, "fn", "number");
    read_string(source, "gn", "boolean");
}

#[test]
fn address01_i_typeof_repeated_calls_survive_collection() {
    let mut source = String::from("function f(){\"use strict\";return typeof false;}var a=f();");
    for i in 0..40 {
        source.push_str(&format!("var t{i}={{k:{i}}};"));
    }
    source.push_str("var b=f();");
    assert_activation_local_typeof(&source);
    let (_, extra, _) = run_realm(&source, &[], Some(("a", 7)), true, false);
    assert_string_tail(&extra[..8], "boolean");
    let collections = match extra.get(8) {
        Some(Value::Int(n)) => *n,
        Some(Value::Float(n)) => *n as i64,
        other => panic!("collection counter must be numeric, got {other:?}"),
    };
    assert!(
        collections > 0,
        "collection did not run between typeof calls, extra={extra:?}"
    );
    read_string(&source, "b", "boolean");
}

#[test]
fn address01_i_overwritten_object_is_reclaimed() {
    expect_reclaimed("var x={};x=null;", "x");
}

#[test]
fn address01_i_overwritten_string_is_reclaimed() {
    expect_reclaimed("var x=\"ab\";x=null;", "x");
}

#[test]
fn address01_i_overwritten_function_is_reclaimed() {
    expect_reclaimed("var x=function(){\"use strict\";return 1;};x=null;", "x");
}

#[test]
fn address01_i_overwritten_nested_object_is_reclaimed() {
    expect_reclaimed("var x={inner:{n:1}};x=null;", "x");
}

#[test]
fn address01_i_alias_stays_valid_after_overwrite() {
    let (values, _, _) = run_realm(
        "var x={n:7};var y=x;x=null;var n=y.n;",
        &["x", "n"],
        None,
        false,
        false,
    );
    assert_null(&values[0]);
    assert_number(&values[1], 7.0);
}

#[test]
fn address01_i_array_expression_read_is_number_seven() {
    let (values, _, _) = run_realm("var a;a=[7];var r=a[0];", &["r"], None, false, false);
    assert_number(&values[0], 7.0);
}

#[test]
fn address01_i_array_expression_write_nine() {
    let (values, _, _) = run_realm("var a;a=[7];a[0]=9;var r=a[0];", &["r"], None, false, false);
    assert_number(&values[0], 9.0);
}

#[test]
fn address01_i_array_expression_alias_read() {
    let (values, _, _) = run_realm(
        "var a;a=[7];var b=a;var r=b[0];",
        &["r"],
        None,
        false,
        false,
    );
    assert_number(&values[0], 7.0);
}

#[test]
fn address01_i_parenthesized_array_initializer() {
    let (values, _, _) = run_realm("var a=([7]);var r=a[0];", &["r"], None, false, false);
    assert_number(&values[0], 7.0);
}

#[test]
fn address01_i_nested_assignment_keeps_array_length() {
    let (values, _, _) = run_realm(
        "var a;var b;a=b=[7];var r=a[0];var s=b[0];",
        &["r", "s"],
        None,
        false,
        false,
    );
    assert_number(&values[0], 7.0);
    assert_number(&values[1], 7.0);
}

#[test]
fn address01_i_dead_throw_after_return_is_refused() {
    let text = must_refuse("function f(){\"use strict\";return 1;if(1){throw 2;}}");
    assert!(
        text.contains("throw") || text.contains("P0") || text.contains("unsupported"),
        "dead throw must be a capability refusal, got {text}"
    );
}

#[test]
fn address01_i_dead_unary_plus_is_refused() {
    let text = must_refuse("if(false){var x=+1;}");
    assert!(
        text.contains("unary") || text.contains("P0") || text.contains("unsupported"),
        "dead unary plus must be a capability refusal, got {text}"
    );
}

#[test]
fn address01_i_dead_outer_reference_is_refused() {
    let text = must_refuse("var x=1;function f(){\"use strict\";return 1;var y=x;}");
    assert!(
        text.contains("P4") || text.contains("free") || text.contains("outer"),
        "dead outer reference must stay a P4 refusal, got {text}"
    );
}

#[test]
fn address01_i_dead_nonzero_call_is_refused() {
    let text = must_refuse(
        "function g(){\"use strict\";return 1;var f=function(){\"use strict\";return 1;};f(1);}",
    );
    assert!(
        text.contains("arity") || text.contains("argument") || text.contains("P0"),
        "dead nonzero call must be a capability refusal, got {text}"
    );
}

#[test]
fn address01_i_var_after_return_stays_undefined() {
    let (values, _, _) = run_realm(
        "function f(){\"use strict\";return x;var x=2;}var r=f();",
        &["r"],
        None,
        false,
        false,
    );
    assert_undefined(&values[0]);
}

#[test]
fn address01_i_assignment_chain_evaluates_rhs_once() {
    let values = run_heap_free(
        "var n=0;var x;var y;var r=(x=y=(n=n+1));",
        &["n", "x", "y", "r"],
    );
    assert_number(&values[0], 1.0);
    assert_number(&values[1], 1.0);
    assert_number(&values[2], 1.0);
    assert_number(&values[3], 1.0);
}

#[test]
fn address01_i_parenthesized_unresolved_typeof_is_undefined() {
    let source = "var t=typeof (missing);";
    read_string(source, "t", "undefined");
}

#[test]
fn address01_i_nested_parenthesized_unresolved_typeof_is_undefined() {
    let source = "var t=typeof (((missing)));";
    read_string(source, "t", "undefined");
}

#[test]
fn address01_i_ordinary_unresolved_read_stays_refused() {
    let text = must_refuse("missing;");
    assert!(
        text.contains("unknown") || text.contains("unresolved") || text.contains("P0"),
        "ordinary unresolved read must stay a compile refusal, got {text}"
    );
}

#[test]
fn address01_i_typeof_assignment_runs_once() {
    let source = "var n=0;var t=typeof (n=n+1);";
    let (values, extra, _) = run_realm(source, &["n"], Some(("t", 6)), false, false);
    assert_number(&values[0], 1.0);
    assert_string_tail(&extra, "number");
}

fn assert_array_native(source: &str, label: &str) {
    let (values, _, evidence) = run_realm(source, &["r"], None, false, true);
    assert_number(&values[0], if label.contains("write") { 9.0 } else { 7.0 });
    let (count, info) = evidence.expect("native evidence");
    assert!(count > 0, "{label} produced no SSA executions");
    eprintln!("P3_DECL_ADDRESS01_ARRAY_NATIVE {label} count={count}\n{info}");
}

#[test]
fn address01_n_array_expression_read() {
    assert_array_native("var a;a=[7];var r=a[0];", "read");
}

#[test]
fn address01_n_array_expression_write() {
    assert_array_native("var a;a=[7];a[0]=9;var r=a[0];", "write");
}

#[test]
fn address01_n_array_expression_alias() {
    assert_array_native("var a;a=[7];var b=a;var r=b[0];", "alias");
}

#[test]
fn address01_n_array_parenthesized_initializer() {
    assert_array_native("var a=([7]);var r=a[0];", "paren");
}

const ADDRESS02_ACCOUNT_ID: &str = "javascript-es5-address02-account";

struct AccountPlugin {
    collect_before_alias_read: bool,
}

impl vm::SourcePlugin for AccountPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new(ADDRESS02_ACCOUNT_ID),
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
        let carrier = realm_carrier(&ir)?;
        let collect = helper_index(&ir, ADDRESS01_COLLECT)?;
        let observe = helper_index(&ir, OBSERVE_NAME)?;
        if self.collect_before_alias_read {
            let x = binding_slot(&ir, "x")?;
            let overwritten = ir
                .stmts
                .iter()
                .rposition(|stmt| matches!(stmt, vm::Stmt::Assign { index, .. } if *index == x))
                .ok_or_else(|| rejection("alias probe needs the real x=null store".into()))?;
            let root_write = helper_index(&ir, "__compat_es5_root_write")?;
            if !matches!(ir.stmts.get(overwritten + 1), Some(vm::Stmt::Let {
                expr: vm::Expr::Call(target, _, _, _, _), ..
            }) if *target == root_write)
            {
                return Err(rejection(
                    "alias probe requires contiguous binding refresh".into(),
                ));
            }
            let collected = fresh_slot(&mut ir)?;
            // Source x=null and its actual root refresh complete first.
            // The following y.child.n is untouched production source IR.
            ir.stmts.insert(
                overwritten + 2,
                vm::Stmt::Let {
                    index: collected,
                    declared_schema: Some(vm::TypeSchema::Int),
                    expr: vm::Expr::Call(
                        collect,
                        Vec::new(),
                        vec![vm::Expr::Var(carrier)],
                        None,
                        None,
                    ),
                    line: 1,
                },
            );
        }
        let ack = fresh_slot(&mut ir)?;
        ir.stmts.push(vm::Stmt::Let {
            index: ack,
            declared_schema: Some(vm::TypeSchema::Int),
            expr: vm::Expr::Call(
                collect,
                Vec::new(),
                vec![vm::Expr::Var(carrier)],
                None,
                None,
            ),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: vm::Expr::String("address02-collect-done".to_owned()),
            line: 1,
        });
        for which in [OBSERVE_LIVE_NODES, OBSERVE_NODE_CELLS] {
            let out = fresh_slot(&mut ir)?;
            ir.stmts.push(vm::Stmt::Let {
                index: out,
                declared_schema: Some(vm::TypeSchema::Int),
                expr: vm::Expr::Call(
                    observe,
                    Vec::new(),
                    vec![
                        vm::Expr::Var(carrier),
                        vm::Expr::Int(which),
                        vm::Expr::Int(0),
                    ],
                    None,
                    None,
                ),
                line: 1,
            });
            ir.stmts.push(vm::Stmt::Expr {
                expr: vm::Expr::Var(out),
                line: 1,
            });
        }
        if self.collect_before_alias_read {
            append_binding_reads(&mut ir, &["n".to_owned()])?;
        }
        Ok(ir)
    }
}

fn observe_int(value: &Value) -> i64 {
    match value {
        Value::Int(n) => *n,
        Value::Float(n) => *n as i64,
        other => panic!("heap observe must return Int, got {other:?}"),
    }
}

/// Successful post-collect live-node and node-cell counts. A collector
/// diagnostic before the observe is a failure, not a stale-probe success.
fn post_collect_accounting(source: &str) -> (i64, i64) {
    post_collect_accounting_with_alias(source, false)
}

fn post_collect_accounting_with_alias(source: &str, collect_before_alias_read: bool) -> (i64, i64) {
    let plugin = Box::leak(Box::new(AccountPlugin {
        collect_before_alias_read,
    }));
    let catalog = catalog();
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new(ADDRESS02_ACCOUNT_ID)),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(plugin)
            .with_host_api_catalog(std::sync::Arc::clone(&catalog)),
    )
    .unwrap_or_else(|error| panic!("accounting compile refused: {error:?}\nsource: {source}"));
    let mut machine = compiled
        .into_vm()
        .unwrap_or_else(|error| panic!("accounting vm refused: {error:?}"));
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .unwrap_or_else(|error| panic!("owned-array init refused: {error:?}"));
    let mut registry = HostFunctionRegistry::restricted();
    install_production(&mut registry, &catalog);
    registry
        .bind_vm_cached(&mut machine)
        .unwrap_or_else(|error| panic!("adapter bind refused: {error:?}"));
    machine.set_fuel(100_000_000);
    match machine.run() {
        Ok(status) => assert_eq!(
            status,
            VmStatus::Halted,
            "accounting source must halt: {source}"
        ),
        Err(error) => panic!(
            "accounting failed before successful end state; ip={} error={error:?} stack={:?} frames={:?}",
            machine.ip(),
            machine.stack(),
            machine.execution_frames()
        ),
    }
    let stack = machine.stack();
    let marker = stack
        .iter()
        .position(|value| *value == Value::string("address02-collect-done"))
        .unwrap_or_else(|| panic!("collect did not complete for {source}; stack={stack:?}"));
    assert!(
        stack.len() >= marker + 3,
        "live-node and node-cell observations missing after collect; stack={stack:?}"
    );
    if collect_before_alias_read {
        assert_number(&stack[marker + 3], 7.0);
    }
    (
        observe_int(&stack[marker + 1]),
        observe_int(&stack[marker + 2]),
    )
}

fn expect_released_like(subject: &str, control: &str) {
    let (live, cells) = post_collect_accounting(subject);
    let (base_live, base_cells) = post_collect_accounting(control);
    assert_eq!(
        live, base_live,
        "dead parent/keys/children stayed live\nsubject={subject}\ncontrol={control}\nlive={live} baseline={base_live} cells={cells} baseline_cells={base_cells}"
    );
    assert_eq!(
        cells, base_cells,
        "dead node cells stayed charged\nsubject={subject}\ncontrol={control}\ncells={cells} baseline={base_cells} live={live}"
    );
}

/// Realm baseline: a numeric array that current owned-root code must release.
/// Primitive `null` sources have no carrier and cannot be an accounting control.
const REALM_BASELINE: &str = "var __base=[1];__base=null;";

#[test]
fn address02_i_object_key_transitive_release() {
    let source = "var x={n:1};x=null;";
    expect_reclaimed(source, "x");
    expect_released_like(source, REALM_BASELINE);
}

#[test]
fn address02_i_call_result_release() {
    let source = "function f(){\"use strict\";return {};}var x=f();x=null;";
    expect_reclaimed(source, "x");
    expect_released_like(source, "function f(){\"use strict\";return {};}var x=null;");
}

#[test]
fn address02_i_member_and_array_receiver_release() {
    let array = "var a=[7];var r=a[0];a=null;";
    let (values, _, _) = run_realm(array, &["r"], None, false, false);
    assert_number(&values[0], 7.0);
    expect_released_like(array, "var a=[1];a=null;var r=7;");
    let member = "var a={n:7};var r=a.n;a=null;";
    let (values, _, _) = run_realm(member, &["r"], None, false, false);
    assert_number(&values[0], 7.0);
    expect_released_like(member, "var __base=[1];__base=null;var r=7;");
}

#[test]
fn address02_i_typeof_fresh_string_and_operand_release() {
    let fresh = "var x=typeof missing;x=null;";
    read_string("var x=typeof missing;", "x", "undefined");
    expect_released_like(fresh, REALM_BASELINE);
    let operand = "var t=typeof {};t=null;";
    read_string("var t=typeof {};", "t", "object");
    expect_released_like(operand, "var t=typeof 1;t=null;");
}

#[test]
fn address02_i_comparison_and_method_keep_live_alias() {
    let compare = "var a={n:1};var b={n:2};var n=a.n;var eq=a===b;a=null;b=null;";
    let (values, _, _) = run_realm(compare, &["n", "eq"], None, false, false);
    assert_number(&values[0], 1.0);
    assert_bool(&values[1], false);
    expect_released_like(compare, "var a=[1];a=null;var n=1;var eq=false;");
    let method =
        "function f(){\"use strict\";return {n:9};}var o={m:f};var x=o.m();var n=x.n;x=null;";
    let (values, _, _) = run_realm(method, &["n"], None, false, false);
    assert_number(&values[0], 9.0);
    expect_released_like(
        method,
        "function f(){\"use strict\";return {n:9};}var o={m:f};var x=null;var n=9;",
    );
}

#[test]
fn address02_i_dead_and_reachable_profile_refusals() {
    for source in [
        "function f(){\"use strict\";return 1;(1)();}",
        "(1)();",
        "function f(){\"use strict\";return 1;var o={};o.x=1;}",
        "var o={};o.x=1;",
        "if(false){var a=[true];}",
        "var a=[true];",
        "if(false){var o={1:2};}",
        "var o={1:2};",
    ] {
        let text = must_refuse(source);
        assert!(
            text.contains("P0") || text.contains("admit") || text.contains("reject"),
            "profile refusal must name the capability guard, got {text} for {source}"
        );
    }
}

#[test]
fn p3_restart_i_expression_output_survives_collect_until_observed() {
    let (_, extra, _) = run_realm("\"alive\";", &[], Some(("#expression", 5)), false, false);
    assert_string_tail(&extra, "alive");
}

#[test]
fn p3_restart_i_alias_only_transitive_read_after_collect() {
    let source = "var x={child:{n:7}};var y=x;x=null;var n=y.child.n;";
    let counts = post_collect_accounting_with_alias(source, true);
    let control = post_collect_accounting("var y={child:{n:7}};var x=null;var n=y.child.n;");
    assert_eq!(
        counts, control,
        "only y and its real child/key edges survive"
    );
    assert_eq!(counts.0, 4, "two object nodes and two UTF-16 key nodes");
}

#[test]
fn p3_restart_i_callee_snapshot_release() {
    let source = "var f=function(){\"use strict\";return {};};var x=f();f=null;x=null;";
    expect_reclaimed(source, "x");
    expect_released_like(source, REALM_BASELINE);
}

#[test]
fn p3_restart_i_numeric_consumers_release_receiver_roots() {
    expect_released_like("var a=[7];var b=[-a[0]];a=null;b=null;", REALM_BASELINE);
}

#[test]
fn p3_restart_i_dead_binding_facts() {
    for source in [
        "function f(){\"use strict\";var x=1;return 1;x();}",
        "function f(){\"use strict\";var x=1;x();return 1;}",
        "if(false){var x=true;var a=[x];}",
        "var x=true;var a=[x];",
        "function f(){\"use strict\";var o={};return 1;o[0]=1;}",
        "var o={};o[0]=1;",
        "if(false){var a=[1];var r=a[true];}",
        "var a=[1];var r=a[true];",
        "function f(){\"use strict\";return 1;var o={a:[1]};}",
        "var o={a:[1]};",
        "if(false){var o={a:1,a:2};}",
        "var o={a:1,a:2};",
        "function f(){\"use strict\";return 1;var n=-true;}",
        "var n=-true;",
        "function f(){\"use strict\";return 1;var a=[1];var x=(a[0]=2);}",
        "var a=[1];var x=(a[0]=2);",
    ] {
        must_refuse(source);
    }
    for source in [
        "if(false){var n=1;var a=[n];}",
        "function f(){\"use strict\";var n=1;return 1;var a=[n];}",
        "if(true){var n=1;}else{var n=true;}var a=[n];",
        "function f(){\"use strict\";return 1;var x=g();function g(){\"use strict\";return 2;}}",
    ] {
        compile_text(source).expect("pure facts preserve admitted source");
    }
}

#[test]
fn address02_i_profile_positives_stay_admitted() {
    compile_text("function f(){\"use strict\";return 1;var x;}var r=f();")
        .expect("hoisted post-return var");
    compile_text("if(true){var x=1;}var y=x;").expect("selected initializer");
    compile_text("if(false){var z=9;}var x=2;").expect("unselected initializer stays out of facts");
    compile_text("var n=0;var x;var y;var r=(x=y=(n=n+1));").expect("assignment chain");
    compile_text("var n=0;var t=typeof (n=n+1);").expect("typeof side effect");
    compile_text("var x=1;function f(){\"use strict\";var x=2;return x;}").expect("own shadow");
    compile_text("var a=[1,2];a[0]=3;var r=a[0];").expect("numeric array variable");
}
