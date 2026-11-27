//! Public embedding path: real guest evaluation, source facts, typed cleanup.
use pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source;
use vm::{Value, VmStatus};

fn field(value: &Value, name: &str) -> Value {
    let Value::Map(map) = value else {
        panic!("boxed completion: {value:?}")
    };
    map.get(&Value::string(name)).unwrap().clone()
}

#[test]
fn embedding_executes_closed_function_utf16_and_finally() {
    let source = "function f(x){'use strict';try{return x;}finally{var z=2;}} var text='\\ud800😀'; f(42)===42 && text==='\\ud800\\ud83d\\ude00';";
    let mut machine = compile_javascript_es5_source(source).unwrap();
    assert!(machine.capture_certification_stats().unwrap().valid);
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.host_context().resource_count(), 0);
    machine.set_fuel(10_000_000);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    let result = machine.stack().last().unwrap();
    assert_eq!(field(result, "tag"), Value::Int(2));
    assert_eq!(field(result, "flag"), Value::Bool(true));
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    assert_eq!(machine.host_context().resource_count(), 1);
    let stats =
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).unwrap();
    assert_eq!((stats.pending, stats.registered), (0, 1));
    machine.reset_for_reuse().unwrap();
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).is_none()
    );
    assert!(!machine.capture_certification_stats().unwrap().valid);
}

#[test]
fn two_embedded_vms_have_independent_backing_and_reset_lifecycles() {
    let source = "var o={x:7};o.x===7;";
    let mut first = compile_javascript_es5_source(source).unwrap();
    let mut second = compile_javascript_es5_source(source).unwrap();
    for machine in [&mut first, &mut second] {
        machine.set_fuel(10_000_000);
        assert_eq!(machine.run().unwrap(), VmStatus::Halted);
        assert!(!machine.jit_config().enabled);
        assert_eq!(machine.aot_exec_count(), 0);
        assert_eq!(machine.jit_native_exec_count(), 0);
        assert_eq!(machine.host_context().resource_count(), 1);
    }
    let before =
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut second).unwrap();
    first.reset_for_reuse().unwrap();
    assert_eq!(first.host_context().resource_count(), 0);
    assert_eq!(second.host_context().resource_count(), 1);
    assert!(second.capture_certification_stats().unwrap().valid);
    let after =
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut second).unwrap();
    assert_eq!(
        (before.pending, before.registered, before.reserved_bytes),
        (after.pending, after.registered, after.reserved_bytes)
    );
    second.reset_for_reuse().unwrap();
    assert_eq!(second.host_context().resource_count(), 0);
}

#[test]
fn embedding_compile_error_preserves_concrete_type_chain_and_exact_span() {
    let source = "function f(x){'use strict';try{return x+1;}finally{var z=2;}} var text='\\ud800😀'; f(41)===42 && text==='\\ud800\\ud83d\\ude00';";
    let relational_source = source.replace("return x+1;", "return x<1;");
    let equality_source = source.replace("return x+1;", "return x==1;");
    let equality_driver = format!("{equality_source}f(1);");
    for (source, expected) in [
        (source, true),
        (relational_source.as_str(), false),
        (equality_source.as_str(), false),
        (equality_driver.as_str(), true),
    ] {
        eprintln!("REL145_EMBED_SOURCE source={source:?} expected={expected}");
        let script = format!(
            "console.log(JSON.stringify({{kind:'boolean',value:(0,eval)({})}}));",
            serde_json::to_string(source).unwrap()
        );
        let version = std::process::Command::new("timeout")
            .args(["15", "node", "--version"])
            .output()
            .unwrap();
        let output = std::process::Command::new("timeout")
            .args(["15", "node", "-e", &script])
            .output()
            .unwrap();
        eprintln!(
            "MIG140C_NODE {}",
            serde_json::json!({"source":source,"script":script,"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr),"version":String::from_utf8_lossy(&version.stdout),"version_rc":version.status.code()})
        );
        assert!(version.status.success() && output.status.success() && output.stderr.is_empty());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
            serde_json::json!({"kind":"boolean","value":expected})
        );
        for api in ["embedding", "core-certified"] {
            let mut machine = if api == "embedding" {
                compile_javascript_es5_source(source).unwrap()
            } else {
                let issued = vm::compile_source_with_flavor_and_options_with_capture_certification(
                    source,
                    pd_vm_compat_frontends::javascript_es5_flavor(),
                    pd_vm_compat_frontends::javascript_es5_cli_compile_options(),
                )
                .unwrap();
                let mut machine = issued.into_vm().unwrap();
                let catalog =
                    pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
                let mut registry = vm::HostFunctionRegistry::restricted();
                for module in [
                pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
                pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
                pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
                pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
                pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module(),
            ] {
                module
                    .install_from_catalog(&mut registry, &catalog)
                    .unwrap();
            }
                registry.allow_builtin("math::floor").unwrap();
                // The existing numeric scanner has these exact static calls even
                // when this particular formal receives a Number at runtime.
                registry.allow_builtin("math::is_finite").unwrap();
                registry.allow_builtin("math::powf").unwrap();
                registry.allow_builtin("math::trunc").unwrap();
                registry.allow_builtin("bytes::from_utf8").unwrap();
                registry.bind_vm_cached(&mut machine).unwrap();
                pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
                    &mut machine,
                    Default::default(),
                )
                .unwrap();
                machine
            };
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            assert!(machine.capture_certification_stats().unwrap().valid);
            machine.set_fuel(10_000_000);
            let status = machine.run();
            eprintln!(
                "MIG140C_VM {}",
                serde_json::json!({"api":api,"source":source,"status":format!("{status:?}"),"stack":format!("{:?}",machine.stack()),"aot":machine.aot_exec_count(),"jit":machine.jit_native_exec_count(),"jit_enabled":machine.jit_config().enabled})
            );
            assert_eq!(status.unwrap(), VmStatus::Halted);
            let result = machine.stack().last().unwrap();
            assert_eq!(field(result, "tag"), Value::Int(2));
            assert_eq!(field(result, "flag"), Value::Bool(expected));
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            assert!(machine.capture_certification_stats().unwrap().valid);
            assert_eq!(machine.host_context().resource_count(), 1);
            let stats =
                pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
                    .unwrap();
            assert_eq!((stats.pending, stats.registered), (0, 1));
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            assert!(!machine.capture_certification_stats().unwrap().valid);
            assert!(
                pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
                    .is_none()
            );
            eprintln!(
                "MIG140C_RESET api={api} resources=0 pending=false cert=false owners_retired=true"
            );
        }
    }
    // Exact `return x in 1` is a managed TypeError, not a source refusal.
    // The mapped Source refusal is a separate static nonprimitive key.
    let invalid_source = source.replace("return x+1;", "return x in 1;");
    assert!(
        compile_javascript_es5_source(&invalid_source).is_ok(),
        "exact in-1 source must compile"
    );
    assert!(
        vm::compile_source_with_flavor_and_options_with_capture_certification(
            &invalid_source,
            pd_vm_compat_frontends::javascript_es5_flavor(),
            pd_vm_compat_frontends::javascript_es5_cli_compile_options(),
        )
        .is_ok(),
        "exact in-1 source must compile on the certified API"
    );
    let refused = "function f(x){'use strict';return ({}) in 1;}";
    let key_token = "({})";
    let key_at = refused.find(key_token).expect("static object key");
    for api in ["embedding", "core-certified"] {
        let error = if api == "embedding" {
            match compile_javascript_es5_source(refused) {
                Ok(_) => panic!("static object key must be refused"),
                Err(error) => error,
            }
        } else {
            match vm::compile_source_with_flavor_and_options_with_capture_certification(
                refused,
                pd_vm_compat_frontends::javascript_es5_flavor(),
                pd_vm_compat_frontends::javascript_es5_cli_compile_options(),
            ) {
                Ok(_) => panic!("static object key must be refused"),
                Err(error) => Box::new(error) as Box<dyn std::error::Error>,
            }
        };
        let concrete = error
            .downcast_ref::<vm::SourcePathError>()
            .expect("concrete compilation error");
        let vm::SourcePathError::SourceWithMap {
            error: vm::SourceError::Parse(parse),
            sources,
        } = concrete
        else {
            panic!("mapped parse error")
        };
        assert!(
            parse.message.contains("object ToPrimitive"),
            "{}",
            parse.message
        );
        let span = parse.span.unwrap();
        assert_eq!(
            (span.lo, span.hi),
            (key_at, key_at + key_token.len()),
            "producer span must cover the static object key, api={api}"
        );
        assert_eq!(&refused[span.lo..span.hi], key_token);
        assert_eq!(
            &sources.source(span.source_id).unwrap()[span.lo..span.hi],
            key_token
        );
    }
}

#[test]
fn exact_catalog_conflict_and_missing_throw_permission_refuse() {
    let empty = std::sync::Arc::new(vm::HostApiCatalog::builder().build().unwrap());
    let options = pd_vm_compat_frontends::javascript_es5_compile_options()
        .with_host_api_catalog(empty.clone());
    let error = match pd_vm_compat_frontends::javascript_es5_cli_host_services()
        .compile_options(options.clone())
    {
        Ok(_) => panic!("catalog conflict"),
        Err(error) => error,
    };
    assert_eq!(
        error.to_string(),
        "CLI compiler catalog conflicts with host services"
    );
    let error = match vm::compile_source_with_flavor_and_options_with_capture_certification(
        "throw 7;",
        pd_vm_compat_frontends::javascript_es5_flavor(),
        options,
    ) {
        Ok(_) => panic!("uncaught throw needs exact typed adapter"),
        Err(error) => error,
    };
    eprintln!("P11_EMPTY_CATALOG_ERROR {error:?}");
    assert!(
        error
            .to_string()
            .contains("host catalog does not declare '__compat_array_owner::allocate'")
    );
    let exact = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let mut builder = vm::HostApiCatalog::builder();
    for resource in exact.resources() {
        builder.resource(resource.clone());
    }
    for function in exact
        .functions()
        .iter()
        .filter(|function| function.name != "typescript::throw_error")
    {
        builder.function(function.clone());
    }
    let without_throw = std::sync::Arc::new(builder.build().unwrap());
    let options = pd_vm_compat_frontends::javascript_es5_compile_options()
        .with_host_api_catalog(without_throw);
    let error = match vm::compile_source_with_flavor_and_options_with_capture_certification(
        "throw 7;",
        pd_vm_compat_frontends::javascript_es5_flavor(),
        options,
    ) {
        Ok(_) => panic!("throw permission must be exact"),
        Err(error) => error,
    };
    assert!(error.to_string().contains(
        "requires the exact typescript::throw_error(message: string): null host catalog entry"
    ));
    let module = pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module();
    let mut denied = vm::HostFunctionRegistry::restricted();
    assert!(module.install_from_catalog(&mut denied, &empty).is_err());
}
