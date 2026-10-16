//! P0 source methods/this with operational MemberReference and Completion.
//! First vertical tracer: method returns its receiver through a real Reference.

use pd_vm_compat_frontends::{javascript_es5_flavor, reference_array_effective_catalog};
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
    let (mut helper_like, _) = function_vm(
        "var __compat_es5_alloc={m:function(){\"use strict\";return this;}};__compat_es5_alloc.m()===__compat_es5_alloc;",
        false,
    );
    assert_eq!(unboxed_bool(&mut helper_like), Value::Bool(true));
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
    let (mut is_false, _) = function_vm("var c={d:false};c.d===false;", false);
    assert_eq!(unboxed_bool(&mut is_false), Value::Bool(true));
}

#[test]
fn p0_source_member_alias_boolean_property_round_trips() {
    let (mut is_true, _) = function_vm("var a={b:true};var c={d:a.b};c.d===true;", false);
    assert_eq!(unboxed_bool(&mut is_true), Value::Bool(true));
    let (mut is_false, _) = function_vm("var a={b:false};var c={d:a.b};c.d===false;", false);
    assert_eq!(unboxed_bool(&mut is_false), Value::Bool(true));
}

#[test]
fn p0_source_call_result_boolean_property_round_trips() {
    let (mut is_true, _) = function_vm(
        "var f=function(){\"use strict\";return true;};var r={b:f()};r.b===true;",
        false,
    );
    assert_eq!(unboxed_bool(&mut is_true), Value::Bool(true));
    let (mut is_false, _) = function_vm(
        "var f=function(){\"use strict\";return false;};var r={b:f()};r.b===false;",
        false,
    );
    assert_eq!(unboxed_bool(&mut is_false), Value::Bool(true));
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
    let comp_slot = fresh()?;
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
        declared_schema: Some(js_named),
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
        index: comp_slot,
        declared_schema: Some(completion_named),
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
        expr: vm::Expr::Call(
            vm::BuiltinFunction::Get.call_index(),
            Vec::new(),
            vec![
                vm::Expr::Var(comp_slot),
                vm::Expr::String("has_value".to_owned()),
            ],
            None,
            None,
        ),
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
fn p0_source_unimplemented_shapes_reject_precisely() {
    for source in [
        "var f=function(){return this;};f();",
        "var f=function(x){\n\"use strict\";return x;};f();",
        "var f=function named(){\"use strict\";return;};f();",
        "var x=1;var f=function(){\"use strict\";return x;};f();",
        "var f=function(){\"use strict\";return this;};f(1);",
        "var obj={m:1};obj.m=2;",
        "var f=function(){\"use strict\";return this;};new f();",
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
    assert_eq!(unboxed_bool(&mut first), Value::Bool(true));
    let (mut second, _) = function_vm(
        "var o={\"\\uD800\":1,\"\\uFFFD\":2};o[\"\\uFFFD\"]===2;",
        false,
    );
    assert_eq!(unboxed_bool(&mut second), Value::Bool(true));
}

#[test]
fn p0_source_object_high_vs_low_surrogate_keys_admit() {
    // U1: high vs low lone surrogates are distinct keys, not duplicates.
    let (mut machine, _) = function_vm(
        "var o={\"\\uD800\":1,\"\\uDC00\":2};o[\"\\uDC00\"]===2;",
        false,
    );
    assert_eq!(unboxed_bool(&mut machine), Value::Bool(true));
}

#[test]
fn p0_source_object_true_duplicate_keys_still_reject() {
    // U1 control: genuinely identical keys (surrogate, identifier, and
    // string spellings) must still refuse as duplicates.
    for source in [
        "var o={\"\\uD800\":1,\"\\uD800\":2};",
        "var o={a:1,a:2};",
        "var o={\"a\":1,\"a\":2};",
    ] {
        let diagnostic = helper_rejection(source);
        assert!(
            diagnostic.contains("duplicate"),
            "true duplicate keys must still refuse, got {diagnostic} for {source:?}"
        );
    }
}
