//! P0 source methods/this with operational MemberReference and Completion.
//! First vertical tracer: method returns its receiver through a real Reference.

use pd_vm_compat_frontends::javascript_es5_flavor;
use pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog as reference_array_effective_catalog;
use vm::{HostFunctionRegistry, Value, Vm, VmStatus};

fn function_catalog() -> std::sync::Arc<vm::HostApiCatalog> {
    reference_array_effective_catalog()
}

fn function_options(catalog: &std::sync::Arc<vm::HostApiCatalog>) -> vm::CompileSourceFileOptions {
    vm::CompileSourceFileOptions::new()
        .with_source_plugin(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN)
        .with_host_api_catalog(std::sync::Arc::clone(catalog))
}

fn function_vm(source: &str, native: bool) -> (Vm, std::sync::Arc<vm::HostApiCatalog>) {
    let catalog = function_catalog();
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        javascript_es5_flavor(),
        function_options(&catalog),
    )
    .expect("JS source-function compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    if !native {
        machine.set_jit_config(vm::JitConfig {
            enabled: false,
            ..vm::JitConfig::default()
        });
        let budget = machine
            .get_fuel()
            .map_or(10_000_000, |old| old.min(10_000_000));
        machine.set_fuel(budget);
    }
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = HostFunctionRegistry::restricted();
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
    pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("scalar-error adapter should match the effective catalog");
    registry
        .allow_builtin("math::floor")
        .expect("math::floor should be allow-listable");
    registry
        .allow_builtin("math::copysign")
        .expect("math::copysign should be allow-listable");
    // The source-`new` slice reuses the realm bootstrap strings and numeric
    // guards, so this harness allows the same scalar capabilities as the
    // other production frontend harnesses.
    registry
        .allow_builtin("bytes::from_utf8")
        .expect("bootstrap strings");
    registry
        .allow_builtin("math::is_finite")
        .expect("numeric guards");
    registry
        .allow_builtin("math::trunc")
        .expect("numeric guards");
    registry
        .allow_builtin("math::powf")
        .expect("numeric guards");
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

fn unboxed_bool(machine: &mut Vm) -> Value {
    assert_eq!(
        machine.run().expect("JS source-function run should finish"),
        VmStatus::Halted,
        "JS source-function program should halt"
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
        "method-receiver result must carry the Boolean tag"
    );
    fields
        .get(&Value::string("flag"))
        .expect("boolean payload")
        .clone()
}

#[test]
fn p0_source_method_returns_receiver_through_reference() {
    let (mut machine, _) = function_vm(
        "var obj={m:function(){\"use strict\";return this;}};obj.m()===obj;",
        false,
    );
    assert_eq!(unboxed_bool(&mut machine), Value::Bool(true));
}

#[test]
fn p0_source_method_returns_receiver_native_ssa() {
    let (mut machine, _) = function_vm(
        "var obj={m:function(){\"use strict\";return this;}};obj.m()===obj;",
        true,
    );
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(unboxed_bool(&mut machine), Value::Bool(true));
    let count = machine.aot_exec_count();
    let info = machine.dump_aot_info();
    eprintln!("P0_SOURCE_FUNCTION_NATIVE count={count}\n{info}");
    assert!(count > 0, "no actual native execution; info: {info}");
    assert!(
        info.contains("lowering=ssa"),
        "not SSA native; info: {info}"
    );
}

#[test]
fn p0_source_bare_function_returns_undefined_completion() {
    let (mut machine, _) = function_vm(
        "var f=function(){\"use strict\";return;};f()===undefined;",
        false,
    );
    assert_eq!(unboxed_bool(&mut machine), Value::Bool(true));
}

#[test]
fn p0_source_bare_function_native_ssa() {
    let (mut machine, _) = function_vm(
        "var f=function(){\"use strict\";return;};f()===undefined;",
        true,
    );
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(unboxed_bool(&mut machine), Value::Bool(true));
    let count = machine.aot_exec_count();
    let info = machine.dump_aot_info();
    eprintln!("P0_BARE_NATIVE count={count}\n{info}");
    assert!(count > 0, "no actual native execution; info: {info}");
    assert!(
        info.contains("lowering=ssa"),
        "not SSA native; info: {info}"
    );
}

#[test]
fn p0_source_method_returns_field_through_this() {
    let (mut machine, _) = function_vm(
        "var obj={n:7,m:function(){\"use strict\";return this.n;}};obj.m();",
        false,
    );
    assert_eq!(unboxed_number(&mut machine), Value::Float(7.0));
}

#[test]
fn p0_source_shared_method_uses_second_receiver() {
    let (mut machine, _) = function_vm(
        "var a={m:function(){\"use strict\";return this;}};var b={m:a.m};b.m()===b;",
        false,
    );
    assert_eq!(unboxed_bool(&mut machine), Value::Bool(true));
}

#[test]
fn p0_source_bare_strict_alias_is_undefined() {
    let (mut machine, _) = function_vm(
        "var obj={m:function(){\"use strict\";return this;}};var f=obj.m;f()===undefined;",
        false,
    );
    assert_eq!(unboxed_bool(&mut machine), Value::Bool(true));
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
}

#[test]
fn p0_source_parenthesized_method_keeps_receiver() {
    let (mut machine, _) = function_vm(
        "var obj={m:function(){\"use strict\";return this;}};(obj.m)()===obj;",
        false,
    );
    assert_eq!(unboxed_bool(&mut machine), Value::Bool(true));
}

#[test]
fn p0_source_factory_returns_fresh_objects() {
    let (mut machine, _) = function_vm(
        "var make=function(){\"use strict\";return {};};var a=make();var b=make();a!==b;",
        false,
    );
    assert_eq!(unboxed_bool(&mut machine), Value::Bool(true));
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
}

#[test]
fn p0_source_factory_freshness_is_native_with_ssa() {
    let (mut machine, _) = function_vm(
        "var make=function(){\"use strict\";return {};};var a=make();var b=make();a!==b;",
        true,
    );
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(unboxed_bool(&mut machine), Value::Bool(true));
    let count = machine.aot_exec_count();
    let info = machine.dump_aot_info();
    eprintln!("P0_FACTORY_NATIVE count={count}\n{info}");
    assert!(count > 0, "no actual native execution; info: {info}");
    assert!(
        info.contains("lowering=ssa"),
        "not SSA native; info: {info}"
    );
}

#[test]
fn p0_source_empty_and_explicit_return_yield_undefined() {
    let (mut empty, _) = function_vm("var f=function(){\"use strict\";};f()===undefined;", false);
    assert_eq!(unboxed_bool(&mut empty), Value::Bool(true));
    let (mut explicit, _) = function_vm(
        "var g=function(){\"use strict\";return;};g()===undefined;",
        false,
    );
    assert_eq!(unboxed_bool(&mut explicit), Value::Bool(true));
}

#[test]
fn p0_source_own_tag_property_is_data_not_projection() {
    let (mut machine, _) = function_vm("var obj={tag:7};obj.tag;", false);
    assert_eq!(unboxed_number(&mut machine), Value::Float(7.0));
}

#[test]
fn p0_source_arbitrary_names_and_helper_like_bindings() {
    let (mut machine, _) = function_vm(
        "var foo={bar:7,baz:function(){\"use strict\";return this.bar;}};foo.baz();",
        false,
    );
    assert_eq!(unboxed_number(&mut machine), Value::Float(7.0));
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    let (mut helper_like, _) = function_vm(
        "var __compat_es5_alloc={m:function(){\"use strict\";return this;}};__compat_es5_alloc.m()===__compat_es5_alloc;",
        false,
    );
    assert_eq!(unboxed_bool(&mut helper_like), Value::Bool(true));
    assert!(!helper_like.jit_config().enabled);
    assert_eq!(helper_like.aot_exec_count(), 0);
    assert_eq!(helper_like.jit_native_exec_count(), 0);
}

#[test]
fn p0_source_strings_and_arrays_survive_calls() {
    let (mut strings, _) = function_vm(
        "var s=\"a\";var obj={m:function(){\"use strict\";return this;}};obj.m();s===\"a\";",
        false,
    );
    assert_eq!(unboxed_bool(&mut strings), Value::Bool(true));
    let (mut arrays, _) = function_vm(
        "var a=[1,2];var obj={m:function(){\"use strict\";return this;}};obj.m();a[0]+a[1];",
        false,
    );
    assert_eq!(unboxed_number(&mut arrays), Value::Float(3.0));
}

#[test]
fn p0_source_static_boolean_property_round_trips() {
    let (mut is_true, _) = function_vm("var c={d:true};c.d===true;", false);
    assert_eq!(unboxed_bool(&mut is_true), Value::Bool(true));
    assert!(!is_true.jit_config().enabled);
    assert_eq!(is_true.aot_exec_count(), 0);
    assert_eq!(is_true.jit_native_exec_count(), 0);
    let (mut is_false, _) = function_vm("var c={d:false};c.d===false;", false);
    assert_eq!(unboxed_bool(&mut is_false), Value::Bool(true));
    assert!(!is_false.jit_config().enabled);
    assert_eq!(is_false.aot_exec_count(), 0);
    assert_eq!(is_false.jit_native_exec_count(), 0);
}

#[test]
fn p0_source_member_alias_boolean_property_round_trips() {
    let (mut is_true, _) = function_vm("var a={b:true};var c={d:a.b};c.d===true;", false);
    assert_eq!(unboxed_bool(&mut is_true), Value::Bool(true));
    assert!(!is_true.jit_config().enabled);
    assert_eq!(is_true.aot_exec_count(), 0);
    assert_eq!(is_true.jit_native_exec_count(), 0);
    let (mut is_false, _) = function_vm("var a={b:false};var c={d:a.b};c.d===false;", false);
    assert_eq!(unboxed_bool(&mut is_false), Value::Bool(true));
    assert!(!is_false.jit_config().enabled);
    assert_eq!(is_false.aot_exec_count(), 0);
    assert_eq!(is_false.jit_native_exec_count(), 0);
}

#[test]
fn p0_source_call_result_boolean_property_round_trips() {
    let (mut is_true, _) = function_vm(
        "var f=function(){\"use strict\";return true;};var r={b:f()};r.b===true;",
        false,
    );
    assert_eq!(unboxed_bool(&mut is_true), Value::Bool(true));
    assert!(!is_true.jit_config().enabled);
    assert_eq!(is_true.aot_exec_count(), 0);
    assert_eq!(is_true.jit_native_exec_count(), 0);
    let (mut is_false, _) = function_vm(
        "var f=function(){\"use strict\";return false;};var r={b:f()};r.b===false;",
        false,
    );
    assert_eq!(unboxed_bool(&mut is_false), Value::Bool(true));
    assert!(!is_false.jit_config().enabled);
    assert_eq!(is_false.aot_exec_count(), 0);
    assert_eq!(is_false.jit_native_exec_count(), 0);
}

#[test]
fn p0_source_member_alias_boolean_property_is_native_with_ssa() {
    let (mut machine, _) = function_vm("var a={b:true};var c={d:a.b};c.d===true;", true);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(unboxed_bool(&mut machine), Value::Bool(true));
    let count = machine.aot_exec_count();
    let info = machine.dump_aot_info();
    eprintln!("P0_BOOL_MEMBER_NATIVE count={count}\n{info}");
    assert!(count > 0, "no actual native execution; info: {info}");
    assert!(
        info.contains("lowering=ssa"),
        "not SSA native; info: {info}"
    );
}

#[test]
fn p0_source_method_field_through_this_is_native_with_ssa() {
    let (mut machine, _) = function_vm(
        "var obj={n:7,m:function(){\"use strict\";return this.n;}};obj.m();",
        true,
    );
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(unboxed_number(&mut machine), Value::Float(7.0));
    let count = machine.aot_exec_count();
    let info = machine.dump_aot_info();
    eprintln!("P0_FIELD_NATIVE count={count}\n{info}");
    assert!(count > 0, "no actual native execution; info: {info}");
    assert!(
        info.contains("lowering=ssa"),
        "not SSA native; info: {info}"
    );
}

// ---- Private Completion record observer (test-only) ----
// A dedicated SourcePlugin delegates the original real JS source to the
// REAL JS plugin (real prelude, carrier, source policy, catalog) and only
// appends genuine calls to the EXISTING generated dispatch helper through
// the ACTUAL carrier discovered structurally. It leaves the actual
// Completion `has_value` Bool on the stack. Normal empty observes false;
// explicit `return;` observes true. Labeled helper-side plumbing; the
// has_value assertions below are the business proofs.

fn helper_completion_descriptor() -> vm::SourceFrontendDescriptor {
    vm::SourceFrontendDescriptor {
        id: vm::FrontendId::new("javascript-es5-completion-observer"),
        extensions: &["js"],
        typing_policy: vm::FrontendTypingPolicy::Strict,
        supports_ownership_checking: true,
    }
}

struct HelperCompletionPlugin;

static HELPER_COMPLETION_PLUGIN: HelperCompletionPlugin = HelperCompletionPlugin;

fn helper_completion_rejection(message: String) -> vm::ParseError {
    vm::ParseError::at_span(vm::compiler::source_map::Span::new(0, 0, 0), message)
}

fn helper_append_completion_observer(ir: &mut vm::FrontendIr) -> Result<(), vm::ParseError> {
    let make = ir
        .functions
        .iter()
        .find(|f| f.name == "__compat_ref_make")
        .map(|f| f.index)
        .ok_or_else(|| {
            helper_completion_rejection("helper observer requires the realm make helper".to_owned())
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
        return Err(helper_completion_rejection(format!(
            "helper observer requires exactly one realm carrier, found {}",
            carriers.len()
        )));
    };
    let carrier = *carrier;
    let (_, func_slot) = ir.local_bindings.first().cloned().ok_or_else(|| {
        helper_completion_rejection("helper observer requires a function binding".to_owned())
    })?;
    let dispatch = ir
        .functions
        .iter()
        .find(|f| f.name == "__compat_es5_dispatch")
        .map(|f| f.index)
        .ok_or_else(|| {
            helper_completion_rejection("helper observer requires the dispatch helper".to_owned())
        })?;
    // Fresh slots beyond the current locals.
    let mut next = u16::try_from(ir.locals).map_err(|_| {
        helper_completion_rejection("helper observer slot space exhausted".to_owned())
    })?;
    let mut fresh = || -> Result<u16, vm::ParseError> {
        let slot = next;
        next = next.checked_add(1).ok_or_else(|| {
            helper_completion_rejection("helper observer slot space exhausted".to_owned())
        })?;
        Ok(slot)
    };
    let undef_slot = fresh()?;
    let result_slot = fresh()?;
    let comp_slot = fresh()?;
    let err_slot = fresh()?;
    ir.locals = usize::from(next);
    let js_named = vm::TypeSchema::Named("__CompatEs5Primitive".to_owned(), Vec::new());
    let completion_named = vm::TypeSchema::Named("__CompatEs5Completion".to_owned(), Vec::new());
    // Undefined JsValue record (tag 0, others zero/false).
    let mut undefined = vm::Expr::Call(
        vm::BuiltinFunction::MapNew.call_index(),
        Vec::new(),
        Vec::new(),
        None,
        None,
    );
    for (key, value) in [
        ("tag", vm::Expr::Int(0)),
        ("number", vm::Expr::Float(0.0)),
        ("flag", vm::Expr::Bool(false)),
        ("ref_slot", vm::Expr::Int(0)),
        ("generation", vm::Expr::Int(0)),
    ] {
        undefined = vm::Expr::Call(
            vm::BuiltinFunction::Set.call_index(),
            Vec::new(),
            vec![undefined, vm::Expr::String(key.to_owned()), value],
            None,
            None,
        );
    }
    ir.stmts.push(vm::Stmt::Let {
        index: undef_slot,
        declared_schema: Some(js_named.clone()),
        expr: undefined,
        line: 1,
    });
    let empty_args = vm::Expr::Call(
        vm::BuiltinFunction::ArrayNew.call_index(),
        Vec::new(),
        Vec::new(),
        None,
        None,
    );
    ir.stmts.push(vm::Stmt::Let {
        index: result_slot,
        declared_schema: Some(vm::TypeSchema::Result {
            ok: Box::new(completion_named.clone()),
            err: Box::new(js_named.clone()),
        }),
        expr: vm::Expr::Call(
            dispatch,
            Vec::new(),
            vec![
                vm::Expr::Var(carrier),
                vm::Expr::Var(func_slot),
                vm::Expr::Var(undef_slot),
                empty_args,
            ],
            None,
            None,
        ),
        line: 1,
    });
    ir.stmts.push(vm::Stmt::Expr {
        expr: vm::Expr::ResultMatch {
            value: Box::new(vm::Expr::Var(result_slot)),
            value_slot: result_slot,
            ok_binding: Some(comp_slot),
            ok_expr: Box::new(vm::Expr::Call(
                vm::BuiltinFunction::Get.call_index(),
                Vec::new(),
                vec![
                    vm::Expr::Var(comp_slot),
                    vm::Expr::String("has_value".to_owned()),
                ],
                None,
                None,
            )),
            err_binding: Some(err_slot),
            err_expr: Box::new(vm::Expr::Bool(false)),
            scrutinee_schema: vm::TypeSchema::Result {
                ok: Box::new(completion_named),
                err: Box::new(js_named),
            },
            span: vm::compiler::source_map::Span::new(0, 0, 0),
            semantic_id: None,
        },
        line: 1,
    });
    Ok(())
}

impl vm::SourcePlugin for HelperCompletionPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        helper_completion_descriptor()
    }

    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        helper_append_completion_observer(&mut ir)?;
        Ok(ir)
    }
}

fn helper_completion_has_value(source: &str) -> Value {
    let catalog = function_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(&HELPER_COMPLETION_PLUGIN)
        .with_host_api_catalog(std::sync::Arc::clone(&catalog));
    let flavor =
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-completion-observer"));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source, flavor, options,
    )
    .expect("helper observer compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    machine.set_fuel(10_000_000);
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = HostFunctionRegistry::restricted();
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
    pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("scalar-error adapter should match the effective catalog");
    registry
        .allow_builtin("math::floor")
        .expect("math::floor should be allow-listable");
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    assert_eq!(
        machine.run().expect("helper observer run should finish"),
        VmStatus::Halted,
        "helper observer program should halt"
    );
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    machine
        .stack()
        .last()
        .cloned()
        .expect("observer should leave has_value")
}

#[test]
fn p0_source_completion_normal_vs_return_has_value() {
    // Empty body produces Normal (has_value=false); explicit `return;`
    // produces Return with Undefined present (has_value=true). Observed
    // through the real dispatch table and the actual Completion record.
    assert_eq!(
        helper_completion_has_value("var f=function(){\"use strict\";};"),
        Value::Bool(false)
    );
    assert_eq!(
        helper_completion_has_value("var g=function(){\"use strict\";return;};"),
        Value::Bool(true)
    );
}

fn helper_rejection(source: &str) -> String {
    let catalog = function_catalog();
    match vm::compile_source_with_flavor_and_options(
        source,
        javascript_es5_flavor(),
        function_options(&catalog),
    ) {
        Ok(_) => panic!("capability case must reject, but compiled: {source:?}"),
        Err(error) => format!("{error:?}"),
    }
}

#[test]
fn p0_source_new_expression_constructs_and_returns_the_instance() {
    // The removed refusal vector is now a positive construction: `return
    // this` observes the instance the construct helper passed as receiver,
    // and the instance links to the materialized `f.prototype`.
    let (mut machine, _) = function_vm(
        "var f=function(){\"use strict\";return this;};var o=new f();o.constructor===f&&Object.getPrototypeOf(o)===f.prototype;",
        false,
    );
    assert_eq!(unboxed_bool(&mut machine), Value::Bool(true));
}

#[test]
fn p0_source_unimplemented_shapes_reject_precisely() {
    for source in [
        "var f=function(){return this;};f();",
        "var f=function named(){\"use strict\";return;};f();",
        "var x=1;var f=function(){\"use strict\";return x;};f();",
        "var obj={m:1};obj.m=2;",
    ] {
        let diagnostic = helper_rejection(source);
        assert!(
            diagnostic.contains("P0")
                || diagnostic.contains("p0")
                || diagnostic.contains("strict")
                || diagnostic.contains("parameter")
                || diagnostic.contains("anonymous")
                || diagnostic.contains("free")
                || diagnostic.contains("unknown")
                || diagnostic.contains("assignment")
                || diagnostic.contains("call")
                || diagnostic.contains("reject"),
            "rejection must be a precise capability diagnostic, got {diagnostic} for {source:?}"
        );
    }
}

fn unboxed_number(machine: &mut Vm) -> Value {
    assert_eq!(
        machine.run().expect("JS source-function run should finish"),
        VmStatus::Halted,
        "JS source-function program should halt"
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
        "function-result must carry the Number tag"
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
fn p0_source_object_distinct_surrogate_keys_admit_both_values() {
    // U1: lone-surrogate `"\uD800"` and replacement `"�"` are distinct
    // UTF-16 keys; both properties must compile and read back exactly.
    let (mut first, _) = function_vm(
        "var o={\"\\uD800\":1,\"\\uFFFD\":2};o[\"\\uD800\"]===1;",
        false,
    );
    eprintln!(
        "NUMBER_STRING_MODE case=distinct_surrogate_keys instance=first phase=before jit_enabled={} fuel={:?} aot={} jit_native={}",
        first.jit_config().enabled,
        first.get_fuel(),
        first.aot_exec_count(),
        first.jit_native_exec_count()
    );
    assert!(
        !first.jit_config().enabled,
        "interpreter policy: JIT must be disabled before execution"
    );
    assert!(
        matches!(first.get_fuel(), Some(fuel) if fuel <= 10_000_000),
        "interpreter policy: finite fuel at most 10m before execution"
    );
    assert_eq!(unboxed_bool(&mut first), Value::Bool(true));
    assert_eq!(first.aot_exec_count(), 0);
    assert_eq!(first.jit_native_exec_count(), 0);
    eprintln!(
        "NUMBER_STRING_MODE case=distinct_surrogate_keys instance=first phase=after jit_enabled={} fuel={:?} aot={} jit_native={}",
        first.jit_config().enabled,
        first.get_fuel(),
        first.aot_exec_count(),
        first.jit_native_exec_count()
    );
    let (mut second, _) = function_vm(
        "var o={\"\\uD800\":1,\"\\uFFFD\":2};o[\"\\uFFFD\"]===2;",
        false,
    );
    eprintln!(
        "NUMBER_STRING_MODE case=distinct_surrogate_keys instance=second phase=before jit_enabled={} fuel={:?} aot={} jit_native={}",
        second.jit_config().enabled,
        second.get_fuel(),
        second.aot_exec_count(),
        second.jit_native_exec_count()
    );
    assert!(
        !second.jit_config().enabled,
        "interpreter policy: JIT must be disabled before execution"
    );
    assert!(
        matches!(second.get_fuel(), Some(fuel) if fuel <= 10_000_000),
        "interpreter policy: finite fuel at most 10m before execution"
    );
    assert_eq!(unboxed_bool(&mut second), Value::Bool(true));
    assert_eq!(second.aot_exec_count(), 0);
    assert_eq!(second.jit_native_exec_count(), 0);
    eprintln!(
        "NUMBER_STRING_MODE case=distinct_surrogate_keys instance=second phase=after jit_enabled={} fuel={:?} aot={} jit_native={}",
        second.jit_config().enabled,
        second.get_fuel(),
        second.aot_exec_count(),
        second.jit_native_exec_count()
    );
}

#[test]
fn p0_source_object_high_vs_low_surrogate_keys_admit() {
    // U1: high vs low lone surrogates are distinct keys, not duplicates.
    let (mut machine, _) = function_vm(
        "var o={\"\\uD800\":1,\"\\uDC00\":2};o[\"\\uDC00\"]===2;",
        false,
    );
    eprintln!(
        "NUMBER_STRING_MODE case=high_vs_low_surrogate_keys instance=machine phase=before jit_enabled={} fuel={:?} aot={} jit_native={}",
        machine.jit_config().enabled,
        machine.get_fuel(),
        machine.aot_exec_count(),
        machine.jit_native_exec_count()
    );
    assert!(
        !machine.jit_config().enabled,
        "interpreter policy: JIT must be disabled before execution"
    );
    assert!(
        matches!(machine.get_fuel(), Some(fuel) if fuel <= 10_000_000),
        "interpreter policy: finite fuel at most 10m before execution"
    );
    assert_eq!(unboxed_bool(&mut machine), Value::Bool(true));
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    eprintln!(
        "NUMBER_STRING_MODE case=high_vs_low_surrogate_keys instance=machine phase=after jit_enabled={} fuel={:?} aot={} jit_native={}",
        machine.jit_config().enabled,
        machine.get_fuel(),
        machine.aot_exec_count(),
        machine.jit_native_exec_count()
    );
}

#[test]
fn p0_source_object_true_duplicate_keys_still_reject() {
    // U1 control: genuinely identical keys (surrogate, identifier, and
    // string spellings) must still refuse in explicit ES5 strict variants.
    for source in [
        "var o={\"\\uD800\":1,\"\\uD800\":2};",
        "var o={a:1,a:2};",
        "var o={\"a\":1,\"a\":2};",
    ] {
        let strict_source = format!("\"use strict\";{source}");
        let diagnostic = helper_rejection(&strict_source);
        assert!(
            diagnostic.contains("duplicate"),
            "strict duplicate keys must still refuse, got {diagnostic} for {strict_source:?}"
        );
    }
}

#[test]
fn p0_source_sloppy_duplicate_data_last_wins() {
    // Verbatim sloppy literals plus the exact last-definition observations.
    for (original_source, observation) in [
        (
            "var o={\"\\uD800\":1,\"\\uD800\":2};",
            "o[\"\\uD800\"]===2;",
        ),
        ("var o={a:1,a:2};", "o.a===2;"),
        ("var o={\"a\":1,\"a\":2};", "o[\"a\"]===2;"),
    ] {
        let source = format!("{original_source}{observation}");
        let (mut machine, _) = function_vm(&source, false);
        let before_fuel = machine.get_fuel();
        assert!(!machine.jit_config().enabled);
        assert!(matches!(before_fuel, Some(fuel) if fuel <= 10_000_000));
        assert_eq!(machine.aot_exec_count(), 0);
        assert_eq!(machine.jit_native_exec_count(), 0);
        assert_eq!(unboxed_bool(&mut machine), Value::Bool(true));
        let after_fuel = machine.get_fuel();
        assert!(
            matches!((before_fuel, after_fuel), (Some(before), Some(after)) if after <= before && after <= 10_000_000)
        );
        assert!(!machine.jit_config().enabled);
        assert_eq!(machine.aot_exec_count(), 0);
        assert_eq!(machine.jit_native_exec_count(), 0);
        machine.reset_for_reuse().expect("actual VM reset");
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(matches!(
            machine.poll_reset_for_reuse(&mut cx),
            std::task::Poll::Ready(Ok(()))
        ));
        assert!(!machine.scope_reset_pending());
        assert_eq!(machine.host_context().resource_count(), 0);
        assert!(!machine.jit_config().enabled);
        assert_eq!(machine.aot_exec_count(), 0);
        assert_eq!(machine.jit_native_exec_count(), 0);
        println!(
            "PROFILE67_BOOL source={source:?} halted=true boolean=true before_fuel={before_fuel:?} after_fuel={after_fuel:?} jit=false aot=0 jit_native=0 reset_resources=0"
        );
    }
}

#[test]
fn p0_source_shared_env_postreturn_effect_skipped() {
    // Supplemental observable control: return precedes the shared Object write.
    let source = "var saved=Object;function f(){\"use strict\";return 1;Object=false;var o={};o.x=1;}var result=f();result===1&&Object===saved;";
    let (mut machine, _) = function_vm(source, false);
    let before_fuel = machine.get_fuel();
    assert!(!machine.jit_config().enabled);
    assert!(matches!(before_fuel, Some(fuel) if fuel <= 10_000_000));
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    assert_eq!(unboxed_bool(&mut machine), Value::Bool(true));
    let after_fuel = machine.get_fuel();
    assert!(
        matches!((before_fuel, after_fuel), (Some(before), Some(after)) if after <= before && after <= 10_000_000)
    );
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    machine.reset_for_reuse().expect("actual VM reset");
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(matches!(
        machine.poll_reset_for_reuse(&mut cx),
        std::task::Poll::Ready(Ok(()))
    ));
    assert!(!machine.scope_reset_pending());
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    println!(
        "PROFILE67_BOOL source={source:?} halted=true boolean=true before_fuel={before_fuel:?} after_fuel={after_fuel:?} jit=false aot=0 jit_native=0 reset_resources=0"
    );
}

#[test]
fn function_prototype_call_null_returns_seven() {
    let source = "var f=function(){\"use strict\";return 7;};f.call(null)===7;";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    assert_eq!(machine.jit_native_exec_count(), 0);
    assert_eq!(machine.aot_exec_count(), 0);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(2)));
    assert_eq!(fields.get(&Value::string("flag")), Some(&Value::Bool(true)));
    machine.reset_for_reuse().expect("actual VM reset");
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn function_prototype_call_inventory_declaration_returns_seven() {
    let source = "function f(){return 7;} f.call(null);";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(3)));
    assert_eq!(fields.get(&Value::string("number")), Some(&Value::Float(7.0)));
}

#[test]
fn function_prototype_apply_inventory_empty_returns_seven() {
    let source = "function f(){return 7;} f.apply(null,[]);";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(3)));
    assert_eq!(fields.get(&Value::string("number")), Some(&Value::Float(7.0)));
}

#[test]
fn function_prototype_bind_inventory_null_returns_seven() {
    let source = "function f(){return 7;} f.bind(null)();";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(3)));
    assert_eq!(fields.get(&Value::string("number")), Some(&Value::Float(7.0)));
}

#[test]
fn json_stringify_inventory_object_equals_text() {
    let source = "JSON.stringify({a:1})===\"{\\\"a\\\":1}\";";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(2)));
    assert_eq!(fields.get(&Value::string("flag")), Some(&Value::Bool(true)));
}

#[test]
fn json_stringify_regexp_with_one_added_row_equals_text() {
    // The admitted `/a/` value is a real guest object whose own rows the
    // guest can mutate. Deleting the non-enumerable `test` row and adding the
    // one enumerable data row `a` is exactly the ordinary single-row shape, so
    // the private node kind must not be rejected before that shape is read.
    let source = r#"var r=/a/;delete r.test;r.a=1;JSON.stringify(r)==='{"a":1}';"#;
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted, "source={source:?}");
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(2)));
    assert_eq!(
        fields.get(&Value::string("flag")),
        Some(&Value::Bool(true))
    );
}

#[test]
fn json_stringify_regexp_without_own_rows_equals_empty_text() {
    // Once the non-enumerable `test` row is deleted the admitted `/a/` value
    // owns no row at all, so the empty-text shape applies instead of the
    // one-row shape and no row cell is ever read.
    let source = "var r=/a/;delete r.test;JSON.stringify(r)==='{}';";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted, "source={source:?}");
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(2)));
    assert_eq!(
        fields.get(&Value::string("flag")),
        Some(&Value::Bool(true))
    );
}

#[test]
fn json_stringify_borrowed_receiver_throws() {
    let source = "var o={stringify:JSON.stringify};o.stringify({a:1});";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    let status = machine.run();
    let text = format!("{status:?}");
    assert!(
        text.contains("uncaught JavaScript throw"),
        "{text}"
    );
}

#[test]
fn json_parse_inventory_object_field_returns_seven() {
    let source = "JSON.parse(\"{\\\"a\\\":7}\").a;";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(3)));
    assert_eq!(fields.get(&Value::string("number")), Some(&Value::Float(7.0)));
}

#[test]
fn object_get_own_property_descriptor_inventory_value_returns_one() {
    let source = "Object.getOwnPropertyDescriptor({a:1},\"a\").value;";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(3)));
    assert_eq!(fields.get(&Value::string("number")), Some(&Value::Float(1.0)));
}

#[test]
fn date_constructor_inventory_epoch_time_returns_zero() {
    let source = "new Date(0).getTime();";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(3)));
    assert_eq!(fields.get(&Value::string("number")), Some(&Value::Float(0.0)));
}

#[test]
fn function_extra_argument_inventory_returns_seven() {
    let source = "function f(){return 7;}f(1);";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(3)));
    assert_eq!(fields.get(&Value::string("number")), Some(&Value::Float(7.0)));
}

#[test]
fn function_hoist_inventory_call_before_declaration_returns_seven() {
    let source = "f();function f(){return 7;}";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(3)));
    assert_eq!(fields.get(&Value::string("number")), Some(&Value::Float(7.0)));
}

#[test]
fn unicode_surrogate_inventory_not_equal_replacement() {
    let source = "\"\\uD800\"===\"\\uFFFD\";";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(2)));
    assert_eq!(fields.get(&Value::string("flag")), Some(&Value::Bool(false)));
}

#[test]
fn arguments_index_inventory_returns_seven() {
    let source = "function f(){return arguments[0];} f(7);";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(3)));
    assert_eq!(fields.get(&Value::string("number")), Some(&Value::Float(7.0)));
}

#[test]
fn string_numeric_subtraction_inventory_returns_one() {
    let source = "\"2\"-1;";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(3)));
    assert_eq!(fields.get(&Value::string("number")), Some(&Value::Float(1.0)));
}

#[test]
fn number_wrapping_bitshift_inventory_returns_one() {
    let source = "1>>>0;";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(3)));
    assert_eq!(fields.get(&Value::string("number")), Some(&Value::Float(1.0)));
}

#[test]
fn regexp_literal_inventory_global_compiles() {
    let source = "/a/g;";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert!(
        fields.get(&Value::string("ref_slot")).is_some(),
        "regexp inventory value={value:?}"
    );
    assert_ne!(fields.get(&Value::string("tag")), Some(&Value::Int(2)));
    assert_ne!(fields.get(&Value::string("tag")), Some(&Value::Int(3)));
}

#[test]
fn ordinary_object_identity_inventory_is_false() {
    let source = "({a:1})===({a:1});";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(2)));
    assert_eq!(fields.get(&Value::string("flag")), Some(&Value::Bool(false)));
}

#[test]
fn object_prototype_tostring_inventory_is_function() {
    let source = "({}).toString;";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert!(
        fields.get(&Value::string("ref_slot")).is_some(),
        "toString inventory value={value:?}"
    );
    assert_ne!(fields.get(&Value::string("tag")), Some(&Value::Int(2)));
    assert_ne!(fields.get(&Value::string("tag")), Some(&Value::Int(3)));
}

#[test]
fn for_in_inventory_returns_seven() {
    let source = "for(var k in {a:1}){} 7;";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(3)));
    assert_eq!(fields.get(&Value::string("number")), Some(&Value::Float(7.0)));
}

#[test]
fn switch_inventory_returns_seven() {
    let source = "switch(1){case 1: 7; break; default: 8;}";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(3)));
    assert_eq!(fields.get(&Value::string("number")), Some(&Value::Float(7.0)));
}

#[test]
fn try_catch_inventory_returns_seven() {
    let source = "try{throw 1;}catch(e){7;}";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(3)));
    assert_eq!(fields.get(&Value::string("number")), Some(&Value::Float(7.0)));
}

#[test]
fn new_object_inventory_compiles() {
    let source = "new Object();";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
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
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert!(
        fields.get(&Value::string("ref_slot")).is_some(),
        "new Object inventory value={value:?}"
    );
    assert_ne!(fields.get(&Value::string("tag")), Some(&Value::Int(2)));
    assert_ne!(fields.get(&Value::string("tag")), Some(&Value::Int(3)));
}

#[test]
fn new_error_inventory_compiles() {
    let source = "new Error(\"boom\");";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| {
            panic!("production compilation refused; guest_invocations=0 source={source:?} error={error:?}")
        });
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..*machine.jit_config()
    });
    machine.set_fuel(10_000_000);
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert!(fields.get(&Value::string("ref_slot")).is_some(), "new Error inventory value={value:?}");
    assert_ne!(fields.get(&Value::string("tag")), Some(&Value::Int(2)));
    assert_ne!(fields.get(&Value::string("tag")), Some(&Value::Int(3)));
}

#[test]
fn new_error_message_and_inherited_name_are_preserved() {
    for source in [
        "(new Error(\"boom\")).message===\"boom\";",
        "(new Error(\"boom\")).name===\"Error\";",
    ] {
        let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
            .unwrap_or_else(|error| panic!("production compilation refused; guest_invocations=0 source={source:?} error={error:?}"));
        machine.set_jit_config(vm::JitConfig { enabled: false, ..*machine.jit_config() });
        machine.set_fuel(10_000_000);
        assert_eq!(machine.run().expect("guest run"), VmStatus::Halted, "source={source}");
        let value = machine.stack().last().expect("completion").clone();
        let Value::Map(fields) = &value else {
            panic!("expected boxed completion, source={source} value={value:?}");
        };
        assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(2)), "source={source}");
        assert_eq!(fields.get(&Value::string("flag")), Some(&Value::Bool(true)), "source={source}");
    }
}

#[test]
fn new_error_unsupported_constructor_inputs_stay_refused() {
    for source in [
        "new Error();",
        "new Error(\"other\");",
        "var Error=1;new Error(\"boom\");",
    ] {
        match pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source) {
            Ok(_) => panic!("unsupported constructor was admitted, source={source}"),
            Err(error) => assert!(
                format!("{error:?}").contains("unsupported JavaScript ES5 P0 primitive expression"),
                "unexpected refusal source={source} error={error:?}"
            ),
        }
    }
}

#[test]
fn throw_new_error_inventory_reports_uncaught_throw() {
    let source = "throw new Error(\"boom\");";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| panic!("production compilation refused; guest_invocations=0 source={source:?} error={error:?}"));
    machine.set_jit_config(vm::JitConfig { enabled: false, ..*machine.jit_config() });
    machine.set_fuel(10_000_000);
    let error = machine.run().expect_err("uncaught throw must not halt normally");
    let vm::VmError::HostError(message) = error else {
        panic!("expected uncaught guest throw host boundary, got {error:?}");
    };
    assert_eq!(message, "uncaught JavaScript throw");
}

#[test]
fn regexp_test_supported_inputs_have_runtime_results() {
    for (source, expected) in [
        ("/a/.test(\"b\");", false),
        ("/a/.test(\"ba\");", true),
        ("var r=/a/;r.test(\"a\");", true),
    ] {
        let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
            .unwrap_or_else(|error| panic!("production compilation refused; guest_invocations=0 source={source:?} error={error:?}"));
        machine.set_jit_config(vm::JitConfig { enabled: false, ..*machine.jit_config() });
        machine.set_fuel(10_000_000);
        assert_eq!(machine.run().expect("guest run"), VmStatus::Halted, "source={source}");
        let value = machine.stack().last().expect("completion").clone();
        let Value::Map(fields) = &value else {
            panic!("expected boxed completion source={source} value={value:?}");
        };
        assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(2)), "source={source}");
        assert_eq!(fields.get(&Value::string("flag")), Some(&Value::Bool(expected)), "source={source}");
    }
}

#[test]
fn regexp_test_borrowed_method_rejects_plain_receiver() {
    let source = "var r=/a/;var o={test:r.test};var caught=0;try{o.test(\"a\");}catch(e){caught=1;}caught===1;";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| panic!("production compilation refused; guest_invocations=0 source={source:?} error={error:?}"));
    machine.set_jit_config(vm::JitConfig { enabled: false, ..*machine.jit_config() });
    machine.set_fuel(10_000_000);
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(2)));
    assert_eq!(fields.get(&Value::string("flag")), Some(&Value::Bool(true)));
}

#[test]
fn regexp_test_argument_mutation_preserves_callee() {
    let source = "var r=/a/;var caught=0;try{caught=r.test(r.extra=\"a\")===true?0:1;}catch(e){caught=2;}caught===0;";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| panic!("production compilation refused; guest_invocations=0 source={source:?} error={error:?}"));
    machine.set_jit_config(vm::JitConfig { enabled: false, ..*machine.jit_config() });
    machine.set_fuel(10_000_000);
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(2)));
    assert_eq!(fields.get(&Value::string("flag")), Some(&Value::Bool(true)));
}

#[test]
fn regexp_test_short_key_plain_receiver_throws() {
    let source = "var r=/a/;var o={t:r.test};var caught=0;try{o.t(\"a\");}catch(e){caught=1;}caught===1;";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| panic!("production compilation refused; guest_invocations=0 source={source:?} error={error:?}"));
    machine.set_jit_config(vm::JitConfig { enabled: false, ..*machine.jit_config() });
    machine.set_fuel(10_000_000);
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(2)));
    assert_eq!(fields.get(&Value::string("flag")), Some(&Value::Bool(true)));
}

#[test]
fn regexp_literal_static_requests_enforce_cell_budget() {
    // Compile-only: every literal requests key, method, object and private ENV.
    // This must be refused by the existing static cell precheck, not deferred
    // to a guest resource failure. No VM is run and no limit is changed.
    let source = "/a/;".repeat(1024);
    match pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source) {
        Ok(_) => panic!("RegExp allocation requests must exceed the static cell budget"),
        Err(error) => assert!(
            error.to_string().contains("realm initialization exceeds the 65536 cell budget"),
            "expected the precise static cell-budget refusal, got {error:?}"
        ),
    }
}

#[test]
fn has_own_property_inventory_returns_true() {
    let source = "({a:1}).hasOwnProperty(\"a\");";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| panic!("production compilation refused; guest_invocations=0 source={source:?} error={error:?}"));
    machine.set_jit_config(vm::JitConfig { enabled: false, ..*machine.jit_config() });
    machine.set_fuel(10_000_000);
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(2)));
    assert_eq!(fields.get(&Value::string("flag")), Some(&Value::Bool(true)));
}

#[test]
fn regexp_test_inventory_returns_true() {
    let source = "/a/.test(\"a\");";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| panic!("production compilation refused; guest_invocations=0 source={source:?} error={error:?}"));
    machine.set_jit_config(vm::JitConfig { enabled: false, ..*machine.jit_config() });
    machine.set_fuel(10_000_000);
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(2)));
    assert_eq!(fields.get(&Value::string("flag")), Some(&Value::Bool(true)));
}

#[test]
fn sparse_array_inventory_length_returns_three() {
    let source = "[1,,3].length;";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| panic!("production compilation refused; guest_invocations=0 source={source:?} error={error:?}"));
    machine.set_jit_config(vm::JitConfig { enabled: false, ..*machine.jit_config() });
    machine.set_fuel(10_000_000);
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(3)));
    assert_eq!(fields.get(&Value::string("number")), Some(&Value::Float(3.0)));
}

#[test]
fn duplicate_object_key_inventory_returns_seven() {
    let source = "({a:1,a:7}).a;";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| panic!("production compilation refused; guest_invocations=0 source={source:?} error={error:?}"));
    machine.set_jit_config(vm::JitConfig { enabled: false, ..*machine.jit_config() });
    machine.set_fuel(10_000_000);
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(3)));
    assert_eq!(fields.get(&Value::string("number")), Some(&Value::Float(7.0)));
}

#[test]
fn numeric_object_key_inventory_compiles() {
    let source = "({1:7});";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| panic!("production compilation refused; guest_invocations=0 source={source:?} error={error:?}"));
    machine.set_jit_config(vm::JitConfig { enabled: false, ..*machine.jit_config() });
    machine.set_fuel(10_000_000);
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert!(fields.get(&Value::string("ref_slot")).is_some(), "numeric key inventory value={value:?}");
    assert_ne!(fields.get(&Value::string("tag")), Some(&Value::Int(2)));
    assert_ne!(fields.get(&Value::string("tag")), Some(&Value::Int(3)));
}

#[test]
fn setter_literal_inventory_compiles() {
    let source = "({set a(v){}});";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| panic!("production compilation refused; guest_invocations=0 source={source:?} error={error:?}"));
    machine.set_jit_config(vm::JitConfig { enabled: false, ..*machine.jit_config() });
    machine.set_fuel(10_000_000);
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert!(fields.get(&Value::string("ref_slot")).is_some(), "setter inventory value={value:?}");
    assert_ne!(fields.get(&Value::string("tag")), Some(&Value::Int(2)));
    assert_ne!(fields.get(&Value::string("tag")), Some(&Value::Int(3)));
}

#[test]
fn getter_literal_inventory_returns_seven() {
    let source = "({get a(){return 7;}}).a;";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| panic!("production compilation refused; guest_invocations=0 source={source:?} error={error:?}"));
    machine.set_jit_config(vm::JitConfig { enabled: false, ..*machine.jit_config() });
    machine.set_fuel(10_000_000);
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(3)));
    assert_eq!(fields.get(&Value::string("number")), Some(&Value::Float(7.0)));
}

#[test]
fn throw_new_error_catch_preserves_message_and_name() {
    let source = "try{throw new Error(\"boom\");}catch(e){e.message===\"boom\"&&e.name===\"Error\";}";
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| panic!("production compilation refused; guest_invocations=0 source={source:?} error={error:?}"));
    machine.set_jit_config(vm::JitConfig { enabled: false, ..*machine.jit_config() });
    machine.set_fuel(10_000_000);
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(fields.get(&Value::string("tag")), Some(&Value::Int(2)));
    assert_eq!(fields.get(&Value::string("flag")), Some(&Value::Bool(true)));
}


/// Compiles and runs a boolean-cookie source through the same production
/// compiler the inventory tests use, and returns the guest Boolean payload.
fn guarded_bool(source: &str) -> bool {
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| {
            panic!("production compilation refused; source={source:?} error={error:?}")
        });
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..*machine.jit_config()
    });
    machine.set_fuel(10_000_000);
    assert_eq!(machine.run().expect("guest run"), VmStatus::Halted);
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    assert_eq!(
        fields.get(&Value::string("tag")),
        Some(&Value::Int(2)),
        "cookie must be a guest Boolean: {source:?}"
    );
    match fields.get(&Value::string("flag")) {
        Some(Value::Bool(flag)) => *flag,
        other => panic!("expected Boolean flag, got {other:?}: {source:?}"),
    }
}

/// `f.call(receiver, a1, a2, ...)` binds `this` to the receiver and forwards
/// every argument after it into the target's activation vector.
#[test]
fn es5_call_apply_call_forwards_full_argument_vector() {
    let source = r#"
function j(a,b){"use strict";return [this.x,a,b].join(":");}
function n(){"use strict";return arguments.length;}
var o={x:5};
var full=j.call(o,2,3)==="5:2:3";
var one=j.call(o,2)==="5:2:";
var none=j.call(o)==="5::";
var three=n.call(o,1,2,3)===3;
var zero=n.call(o)===0;
full&&one&&none&&three&&zero;
"#;
    assert!(guarded_bool(source), "call must forward its full argument vector");
}

/// `f.apply(receiver, denseArray)` expands the array in order; elements past
/// the array's length do not exist, and `null`/`undefined` thisArg are kept
/// verbatim (never replaced by a global object).
#[test]
fn es5_call_apply_apply_expands_dense_array_in_order() {
    let source = r#"
function j(a,b){"use strict";return [this===null?"N":(this===undefined?"U":this.x),a,b].join(":");}
var o={x:5};
var two=j.apply(o,[2,3])==="5:2:3";
var one=j.apply(o,[2])==="5:2:";
var zero=j.apply(o,[])==="5::";
var extra=j.apply(o,[2,3],9)==="5:2:3";
var null_this=j.apply(null,[2,3])==="N:2:3";
var undef_this=j.apply(undefined,[2,3])==="U:2:3";
two&&one&&zero&&extra&&null_this&&undef_this;
"#;
    assert!(guarded_bool(source), "apply must expand the dense array");
}

/// `this` travels unchanged for source-function targets: no boxing of
/// primitives, `undefined` stays undefined, and objects keep identity.
#[test]
fn es5_call_apply_call_this_binding_is_verbatim() {
    let source = r#"
function t(){"use strict";return this;}
var o={};
var num=t.call(1)===1;
var str=t.call("x")==="x";
var bool=t.call(true)===true;
var undef=t.call()===undefined;
var nul=t.call(null)===null;
var object=t.call(o)===o;
num&&str&&bool&&undef&&nul&&object;
"#;
    assert!(guarded_bool(source), "this must reach the callee verbatim");
}

/// `arguments` really is the forwarded vector: the extra arguments are
/// observable in order, not just ignored.
#[test]
fn es5_call_apply_call_forwarded_arguments_are_observable() {
    let source = r#"
function sum(){"use strict";var s=0;for(var i=0;i<arguments.length;i++){s=s+arguments[i];}return s;}
var direct=sum.call(null,1,2,3,4)===10;
var applied=sum.apply(null,[1,2,3,4])===10;
var mixed=sum.call(null,1,2)===3;
direct&&applied&&mixed;
"#;
    assert!(guarded_bool(source), "forwarded arguments must be readable in order");
}

/// A missing or non-array `argArray` keeps the pinned refusal instead of
/// quietly invoking the target with no arguments.
#[test]
fn es5_call_apply_apply_without_array_still_throws_type_error() {
    let source = r#"
function j(a){"use strict";return a;}
var hit=0;
try{j.apply({x:5});}catch(e){if(e.name==="TypeError"){hit=hit+1;}}
try{j.apply({x:5},null);}catch(e){if(e.name==="TypeError"){hit=hit+1;}}
try{j.apply({x:5},undefined);}catch(e){if(e.name==="TypeError"){hit=hit+1;}}
try{j.apply({x:5},"ab");}catch(e){if(e.name==="TypeError"){hit=hit+1;}}
hit===4;
"#;
    assert!(guarded_bool(source), "apply without a dense array must throw");
}

/// The shared `.call` gate keeps its intrinsic carve-outs: `Function.prototype
/// .toString` (353) still admits extra arguments and a non-function receiver
/// still throws, while a bound product (270) now forwards its whole vector
/// after the bound arguments instead of refusing it.
#[test]
fn es5_call_apply_call_intrinsic_carve_outs_and_bound_refusal_unchanged() {
    let source = r#"
function f(){return 7;}
var t=Object.getPrototypeOf(f).toString;
var native_text=t.call(f,1,2)==="function f() {\n    [native code]\n}";
var b=f.bind(null);
var forward=b.call(null,1)===7;
var receiver=0;try{t.call({});}catch(e){if(e.name==="TypeError"){receiver=1;}}
var zero=b.call(null)===7;
native_text&&forward&&receiver===1&&zero;
"#;
    assert!(
        guarded_bool(source),
        "intrinsic carve-outs must stay while bound products forward arguments"
    );
}

/// Pinned `.call`/`.apply` witness: a closed source function binds `this`
/// verbatim. Identity assertions, never `typeof`: `null` and `undefined` keep
/// their own identities (no global-object substitution), primitives are never
/// boxed, objects keep identity, and every argument after thisArg is
/// forwarded in order.
#[test]
fn es5_call_apply_pinned_call_forwards_this_verbatim() {
    let forwarding = r#"
function f(a,b){"use strict";return [this.x,a,b].join("|");}
function t(){"use strict";return this;}
var o={};
var full=f.call({x:1},2,3)==="1|2|3";
var short=f.call({x:1},2)==="1|2|";
var nul=t.call(null)===null;
var undef=t.call(undefined)===undefined;
var missing=t.call()===undefined;
var num=t.call(1)===1;
var str=t.call("x")==="x";
var bool=t.call(true)===true;
var object=t.call(o)===o;
var an=t.apply(null,[])===null;
var au=t.apply(undefined,[])===undefined;
full&&short&&nul&&undef&&missing&&num&&str&&bool&&object&&an&&au;
"#;
    assert!(
        guarded_bool(forwarding),
        "call/apply must bind this verbatim and forward the full argument vector"
    );
}

/// Pinned `.apply` witness: only a real dense Array is accepted. An
/// array-like object, `null`, a missing second argument and a String all keep
/// the guest `TypeError` (`not an array`) instead of a zero-argument call.
#[test]
fn es5_call_apply_pinned_apply_accepts_only_dense_array() {
    let array_only = r#"
function g(a,b){"use strict";return [this.x,a,b].join(":");}
var like={length:2};like[0]=2;like[1]=3;
var hit=0;
try{g.apply({x:5},like);}catch(e){if(e.name==="TypeError"){hit=hit+1;}}
try{g.apply({x:5},null);}catch(e){if(e.name==="TypeError"){hit=hit+1;}}
try{g.apply({x:5});}catch(e){if(e.name==="TypeError"){hit=hit+1;}}
try{g.apply({x:5},"ab");}catch(e){if(e.name==="TypeError"){hit=hit+1;}}
hit===4;
"#;
    assert!(
        guarded_bool(array_only),
        "apply must accept only a real dense array and keep the guest TypeError"
    );
}

/// Pinned bound-product `.call` witness: a bound product (code 270) accepts
/// `.call` with a thisArg, returns the bound target's value and ignores
/// `this` because bind fixed it. Later arguments now append to the bound
/// vector (the pinned `bind` shape), so `.call(null, 9, 10)` is exactly the
/// `b(9, 10)` invocation.
#[test]
fn es5_call_apply_pinned_bound_product_call_ignores_this() {
    let bound_call = r#"
function b1(){"use strict";return 1;}
var b=b1.bind(null);
var object_this=b.call({x:9})===1;
var null_this=b.call(null)===1;
object_this&&null_this;
"#;
    assert!(
        guarded_bool(bound_call),
        "a bound product must accept call and ignore this"
    );
}

/// Pinned full `bind` shape: `f.bind(receiver, a1, a2, ...)` fixes `this` and
/// prepends the bound arguments to every later vector. A missing thisArg is
/// `undefined`, a primitive thisArg stays verbatim, and a bound product is a
/// legal receiver (re-binding appends while the first thisArg stays fixed).
#[test]
fn es5_bind_pinned_bound_arguments_and_fixed_this() {
    let source = r#"
function f(a,b,c){"use strict";return [this===null,this===undefined,a,b,c].join("|");}
function g(){"use strict";return arguments.length;}
var bound=f.bind({x:1},2)(3,4)==="false|false|2|3|4";
var nul=f.bind(null)(7)==="true|false|7||";
var undef=f.bind(undefined)(8)==="false|true|8||";
var missing=f.bind()(1,2,3)==="false|true|1|2|3";
var primitive=(function(){"use strict";return [this===5,this].join("|");}).bind(5)()==="true|5";
var counted=g.bind(null,1,2)(3)===3;
var zero=f.bind(null)()==="true|false|||";
bound&&nul&&undef&&missing&&primitive&&counted&&zero;
"#;
    assert!(
        guarded_bool(source),
        "bind must fix this and prepend the bound argument vector"
    );
}

/// Pinned `length` witness: every bound product reports `0`, never
/// `target.length - boundArgs`, in both the zero-row and the name-gated row
/// shape, before and after a re-bind. The zero is inherited from
/// `Function.prototype.length`, so the product has no own `length` row:
/// `hasOwnProperty("length")` is false, `Object.keys` stays empty and
/// `delete` succeeds.
#[test]
fn es5_bind_bound_length_is_always_zero() {
    let source = r#"
function f(a,b,c){return 0;}
function z(){}
var target_length=f.length===3;
var plain=f.bind(null).length===0;
var four=f.bind({},1,2,3,4).length===0;
var empty_target=z.bind(null,1).length===0;
var rebound=f.bind({x:1},2).bind({y:2},5).length===0;
var missing=f.bind().length===0;
var q=f.bind(null);
var not_own=q.hasOwnProperty("length")===false;
var no_keys=Object.keys(q).length===0;
var dropped=delete q.length;
var inherited=q.length===0;
target_length&&plain&&four&&empty_target&&rebound&&missing&&not_own&&no_keys&&dropped&&inherited;
"#;
    assert!(
        guarded_bool(source),
        "a bound product length must be inherited zero, not an own row"
    );
}

/// Pinned re-bind witness: the second `bind` ignores its thisArg, keeps the
/// first one, and appends its own arguments after the already bound ones.
#[test]
fn es5_bind_rebind_keeps_first_this_and_appends_arguments() {
    let source = r#"
function f(a,b,c){"use strict";return [this===null,this===undefined,a,b,c].join("|");}
function h(){"use strict";return arguments.length+":"+(this===null)+":"+(this===undefined);}
var chained=f.bind({x:1},2).bind({y:2},5)(6)==="false|false|2|5|6";
var no_args=f.bind({x:1},2).bind()(7)==="false|false|2|7|";
var null_second=f.bind({x:1},2).bind(null,9)(10,11)==="false|false|2|9|10";
var counted=h.bind(null,1).bind({z:1},2,3)(4,5)==="5:true:false";
chained&&no_args&&null_second&&counted;
"#;
    assert!(
        guarded_bool(source),
        "re-binding must accumulate arguments without changing the first this"
    );
}

/// Pinned `.call`/`.apply` combination: both ignore the thisArg of the bound
/// product and append their own vector after the bound arguments, while a
/// missing or non-dense `apply` array keeps the guest TypeError. The bind
/// method itself is a legal target (`f.bind.bind(f, null)()()` reaches the
/// original function), while an ordinary zero-row object receiver is refused
/// with the guest TypeError instead of a header read past its extent.
#[test]
fn es5_bind_bound_call_and_apply_append_arguments() {
    let source = r#"
function f(a,b,c){"use strict";return [this===null,this===undefined,a,b,c].join("|");}
function seven(){return 7;}
var b=f.bind({x:1},2);
var called=b.call(null,9,10)==="false|false|2|9|10";
var applied=b.apply(null,[9,10])==="false|false|2|9|10";
var empty_call=b.call()==="false|false|2||";
var empty_apply=b.apply(null,[])==="false|false|2||";
var plain=f.bind({x:1}).call(null,2,3)==="false|false|2|3|";
var hit=0;try{b.apply(null);}catch(q1){if(q1.name==="TypeError")hit=hit+1;}
var hit2=0;try{b.apply(null,{length:1});}catch(q2){if(q2.name==="TypeError")hit2=hit2+1;}
var chain=seven.bind.bind(seven,null)()()===7;
var chained=seven.bind.bind(seven,1)()()===7;
var ordinary={};
var refused=0;
try{seven.bind.call(ordinary);}catch(q3){if(q3.name==="TypeError")refused=refused+1;}
called&&applied&&empty_call&&empty_apply&&plain&&hit===1&&hit2===1&&chain&&chained&&refused===1;
"#;
    assert!(
        guarded_bool(source),
        "bound products must append call/apply vectors and accept bind itself as a target"
    );
}

/// The bound payload is not an own row: deleting `length`, adding rows or
/// rewriting `name` cannot drop a bound argument, and the product keeps its
/// fixed `this`. The private environment pair and the tagged Array behind it
/// are the only carriers. The prototype pair names the realm
/// `Function.prototype`, never the target, so a guest `setPrototypeOf` can
/// only change the inherited row chain and never retarget the call.
#[test]
fn es5_bind_bound_arguments_survive_row_mutation() {
    let source = r#"
function f(a,b,c){"use strict";return [this===null,this===undefined,a,b,c].join("|");}
var b=f.bind({x:1},2);
var before=b(3,4)==="false|false|2|3|4";
var fp=Object.getPrototypeOf(function(){});
var proto_ok=Object.getPrototypeOf(b)===fp;
delete b.length;
b.name="shadow";
b.extra=1;
var after=b(3,4)==="false|false|2|3|4";
var applied=b.apply(null,[9])==="false|false|2|9|";
var other=function(){return 9;};
Object.setPrototypeOf(b,other);
var retarget=b(3,4)==="false|false|2|3|4";
before&&proto_ok&&after&&applied&&retarget;
"#;
    assert!(
        guarded_bool(source),
        "bound arguments and the call target must live outside the guest-writable row set"
    );
}

/// `Function.prototype` is a function node whose private code is 256, but it
/// is not callable. `.call` and `.apply` on it must raise the guest TypeError
/// for both the one-argument and extra-argument shapes, and must never forward
/// arguments into the non-callable target.
#[test]
fn es5_call_apply_function_prototype_call_is_refused() {
    let source = r#"
function f(){return 7;}
var proto=Object.getPrototypeOf(f);
var hit=0;
try{proto.call(null,1);}catch(e){if(e.name==="TypeError"){hit=hit+1;}}
try{proto.call(null);}catch(e){if(e.name==="TypeError"){hit=hit+1;}}
var applied=0;
try{proto.apply(null,[]);}catch(e){if(e.name==="TypeError"){applied=1;}}
hit===2&&applied===1;
"#;
    assert!(
        guarded_bool(source),
        "Function.prototype must refuse .call and .apply with the guest TypeError"
    );
}

/// A non-array second argument must keep the pinned refusal. An ordinary
/// object, an array-like with a zero length and a zero-length typed array all
/// leave the decoded `arg_length` at zero; none of them may invoke the target
/// with an empty argument vector.
#[test]
fn es5_call_apply_apply_rejects_non_array_likes() {
    let source = r#"
function j(a){"use strict";return a;}
var hit=0;
try{j.apply(null,{});}catch(e){if(e.name==="TypeError"){hit=hit+1;}}
try{j.apply(null,{length:0});}catch(e){if(e.name==="TypeError"){hit=hit+1;}}
var typed0=new Uint8Array(0);
try{j.apply(null,typed0);}catch(e){if(e.name==="TypeError"){hit=hit+1;}}
try{j.apply(null);}catch(e){if(e.name==="TypeError"){hit=hit+1;}}
try{j.apply(null,"ab");}catch(e){if(e.name==="TypeError"){hit=hit+1;}}
try{j.apply(null,null);}catch(e){if(e.name==="TypeError"){hit=hit+1;}}
hit===6;
"#;
    assert!(
        guarded_bool(source),
        "apply must refuse every non-dense argument container"
    );
}

/// Pinned `apply` argument-vector bound: 65535 entries expand and answer
/// `arguments.length === 65535`; 65536 refuses with the guest TypeError at the
/// gate, before the expansion loop. Sparse `new Array(n)` carries the public
/// length without 65k payload cells, so the boundary is cheap to construct.
#[test]
fn es5_call_apply_apply_argument_length_limit() {
    let source = r#"
function h(){"use strict";return arguments.length;}
var r=0;
if(h.apply(null,new Array(3))===3){r=r+1;}
if(h.apply(null,new Array(65535))===65535){r=r+2;}
var over=0;
try{h.apply(null,new Array(65536));}catch(e){if(e.name==="TypeError"){over=1;}}
if(over===1){r=r+4;}
r;
"#;
    let mut machine = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| {
            panic!("production compilation refused; source={source:?} error={error:?}")
        });
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..*machine.jit_config()
    });
    // The accepted 65535-entry expansion is the one long path in this file;
    // the budget must cover it plus the refused 65536 gate. The resume loop
    // drains epoch/fuel slice yields between runs.
    machine.set_fuel(120_000_000);
    let mut status = machine.run().expect("guest run");
    let mut rounds = 0;
    while matches!(status, VmStatus::Yielded) && rounds < 64 {
        status = machine.run().expect("guest run resume");
        rounds += 1;
    }
    assert_eq!(
        status,
        VmStatus::Halted,
        "the guest must halt after apply; rounds={rounds} reason={:?} fuel={:?}",
        machine.last_yield_reason(),
        machine.get_fuel()
    );
    let value = machine.stack().last().expect("completion").clone();
    let Value::Map(fields) = &value else {
        panic!("expected boxed completion, got {value:?}");
    };
    let bits = match (
        fields.get(&Value::string("tag")),
        fields.get(&Value::string("number")),
    ) {
        (Some(Value::Int(3)), Some(Value::Float(number))) => *number,
        other => panic!("expected a guest Number bitmask, got {other:?}: {source:?}"),
    };
    assert_eq!(
        bits, 7.0,
        "bit0=small, bit1=65535 accepted, bit2=65536 TypeError; source={source:?}"
    );
}
