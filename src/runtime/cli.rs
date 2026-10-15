use std::sync::Arc;

use vm::{HostFunctionRegistry, cli::CliHostServices};

use super::array::OwnedArrayLimits;

/// Exact effective catalog for the selected CLI mode: the accepted
/// reference-array effective catalog plus the accepted scalar
/// `typescript::throw_error` adapter the unchanged `factorial.ts` and
/// `fibonacci.ts` modules genuinely contain. Compiler and registry share
/// this one catalog; it never overwrites a conflicting explicit catalog
/// (Core still refuses those).
pub fn reference_array_cli_effective_catalog() -> Arc<vm::HostApiCatalog> {
    let base = crate::reference_array_effective_catalog();
    let error = crate::runtime::scalar_error::typescript_error_host_module()
        .catalog()
        .expect("scalar error adapter catalog should build");
    let mut builder = vm::HostApiCatalog::builder();
    for resource in base.resources() {
        builder.resource(resource.clone());
    }
    for function in base.functions().iter().chain(error.functions()) {
        builder.function(function.clone());
    }
    Arc::new(
        builder
            .build()
            .expect("selected CLI effective catalog should build"),
    )
}

/// Compile options for the selected CLI route: the same explicit
/// reference-array source plugins with [`reference_array_cli_effective_catalog`].
pub fn reference_array_cli_compile_options() -> vm::CompileSourceFileOptions {
    vm::CompileSourceFileOptions::new()
        .with_source_plugin(&crate::TYPESCRIPT_REFERENCE_ARRAY_PLUGIN)
        .with_source_plugin(&crate::LUA_PLUGIN)
        .with_host_api_catalog(reference_array_cli_effective_catalog())
}

/// Builds fresh standard hosts plus the exact TypeScript scalar Error adapter for each CLI VM.
pub fn typescript_cli_host_services() -> CliHostServices<'static> {
    let catalog = crate::runtime::scalar_error::typescript_error_host_catalog();
    let registry_catalog = Arc::clone(&catalog);
    CliHostServices::new(catalog, move || {
        let mut registry = HostFunctionRegistry::new();
        crate::runtime::scalar_error::typescript_error_host_module()
            .install_from_catalog(&mut registry, &registry_catalog)?;
        Ok(registry)
    })
    .with_capture_certification()
}

/// Builds the exact production reference-array/generator host services for
/// one selected-mode CLI VM: the same effective catalog the explicit
/// reference-array compiler uses, a fresh registry per VM installing the
/// accepted production descriptors with Core's exact `math::floor` builtin
/// retained, source capture certification, and the accepted per-VM
/// owned-array initialization under the given quotas.
///
/// Beyond the four array/m randomness descriptors proven by the
/// reference-array tests (manual arrays, B1 owned arrays, typed snapshot
/// boundary, scalar-math randomness), this also installs the accepted
/// scalar `typescript::throw_error` adapter and the exact Core numeric
/// builtins proven by the generator tests (`math::sqrt`, `math::round`,
/// `math::powf`, `math::is_nan`, `math::is_finite`). The four unchanged
/// upstream modules genuinely need them: `factorial.ts` and `fibonacci.ts`
/// contain `throw new Error(...)`, and `fibonacci.ts` calls `Math.sqrt`,
/// `**`, `Math.round` and `isNaN`. Without the identical descriptors the
/// generator tests already accept, those byte-unchanged modules cannot even
/// compile in the selected mode, which would defeat the joint four-module
/// goal. Compiler and registry share this one exact catalog.
///
/// The factory owns no global state; each VM gets its own registry binding
/// and scope-local owner directory. Conflicting explicit compiler catalogs
/// keep Core's existing refusal.
pub fn typescript_reference_array_cli_host_services_with_limits(
    limits: OwnedArrayLimits,
) -> CliHostServices<'static> {
    let catalog = reference_array_cli_effective_catalog();
    let registry_catalog = Arc::clone(&catalog);
    CliHostServices::new(catalog, move || {
        let mut registry = HostFunctionRegistry::new();
        crate::runtime::array::typescript_array_host_module()
            .install_from_catalog(&mut registry, &registry_catalog)?;
        crate::runtime::array::typescript_owned_array_host_module()
            .install_from_catalog(&mut registry, &registry_catalog)?;
        crate::runtime::array::typescript_array_snapshot_host_module()
            .install_from_catalog(&mut registry, &registry_catalog)?;
        crate::runtime::scalar_math::typescript_scalar_math_host_module()
            .install_from_catalog(&mut registry, &registry_catalog)?;
        crate::runtime::scalar_error::typescript_error_host_module()
            .install_from_catalog(&mut registry, &registry_catalog)?;
        // The exact Core numeric builtins the accepted generator tests
        // already prove: reference-array sources keep using Core for
        // `Math.floor`, `Math.sqrt`, `**`, `Math.round` and `isNaN`; only
        // `Math.random` comes from the Compat adapter.
        for builtin in [
            "math::floor",
            "math::sqrt",
            "math::round",
            "math::powf",
            "math::is_nan",
            "math::is_finite",
        ] {
            registry.allow_builtin(builtin)?;
        }
        Ok(registry)
    })
    .with_capture_certification()
    .with_vm_init(move |machine| {
        crate::runtime::array::initialize_typescript_owned_arrays(machine, limits)
    })
}

/// Same production selected-mode services with the default owned-array
/// quotas. The binary consumes this; tests needing tighter budgets use
/// [`typescript_reference_array_cli_host_services_with_limits`].
pub fn typescript_reference_array_cli_host_services() -> CliHostServices<'static> {
    typescript_reference_array_cli_host_services_with_limits(OwnedArrayLimits::default())
}
