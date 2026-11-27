//! Explicit JavaScript ES5 embedding and language-specific CLI helpers.
//!
//! The ordinary product entry is the shared `pd-vm-compat-run` file route and
//! public [`crate::compile_options`]; these helpers stay available for
//! JavaScript-focused embedding and corpus callers and install the same exact
//! catalog and per-VM runtime services. They do not select a second product
//! mode.
use std::sync::Arc;

use vm::{HostFunctionRegistry, cli::CliHostServices};

/// The existing descriptors are the complete host boundary; JS adds no host
/// handler. A JavaScript-only plugin list is used here because the caller has
/// already selected the JavaScript flavor explicitly.
pub fn javascript_es5_cli_compile_options() -> vm::CompileSourceFileOptions {
    super::javascript_es5_compile_options()
        .with_host_api_catalog(crate::runtime::cli::reference_array_cli_effective_catalog())
}

fn registry(catalog: &vm::HostApiCatalog) -> vm::VmResult<HostFunctionRegistry> {
    let mut registry = HostFunctionRegistry::new();
    crate::runtime::array::typescript_array_host_module()
        .install_from_catalog(&mut registry, catalog)?;
    crate::runtime::array::typescript_owned_array_host_module()
        .install_from_catalog(&mut registry, catalog)?;
    crate::runtime::array::typescript_array_snapshot_host_module()
        .install_from_catalog(&mut registry, catalog)?;
    crate::runtime::scalar_math::typescript_scalar_math_host_module()
        .install_from_catalog(&mut registry, catalog)?;
    crate::runtime::scalar_error::typescript_error_host_module()
        .install_from_catalog(&mut registry, catalog)?;
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
}

fn initialize(machine: &mut vm::Vm) -> vm::VmResult<()> {
    // Only the opted-in JS service changes trace policy. Explicit backend flags
    // remain Core-owned; disabling trace compilation does not select AOT.
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..*machine.jit_config()
    });
    crate::runtime::array::initialize_typescript_owned_arrays(
        machine,
        crate::runtime::array::OwnedArrayLimits::default(),
    )
}

/// Fresh exact registry per VM; Core retains source facts via `into_vm` before
/// binding and this initialization, including the ordinary VMBC emit route.
pub fn javascript_es5_cli_host_services() -> CliHostServices<'static> {
    let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
    let registry_catalog = Arc::clone(&catalog);
    CliHostServices::new(catalog, move || registry(&registry_catalog))
        .with_capture_certification()
        .with_vm_init(initialize)
}

/// Compile an in-memory Script, retain opaque source authority, and install the
/// same exact services as the file CLI. Compilation errors retain their concrete
/// `SourcePathError` type in the boxed embedding boundary.
pub fn compile_javascript_es5_source(source: &str) -> Result<vm::Vm, Box<dyn std::error::Error>> {
    let issued = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        super::javascript_es5_flavor(),
        javascript_es5_cli_compile_options(),
    )?;
    let mut machine = issued.into_vm()?;
    assemble(&mut machine)?;
    Ok(machine)
}

/// Certified file compilation. Source facts attach in `into_vm` before the
/// exact registry bind and owned-array initialization. This is not an
/// in-memory source or artifact loader.
pub(crate) fn compile_javascript_es5_file(
    path: &std::path::Path,
) -> Result<vm::Vm, CertifiedFileError> {
    let issued = vm::compile_source_file_with_capture_certification_and_options(
        path,
        javascript_es5_cli_compile_options(),
    )
    .map_err(|error| CertifiedFileError::Source(Box::new(error)))?;
    let mut machine = issued
        .into_vm()
        .map_err(|error| CertifiedFileError::Vm(Box::new(error)))?;
    assemble(&mut machine).map_err(|error| CertifiedFileError::Vm(Box::new(error)))?;
    Ok(machine)
}

#[derive(Debug)]
pub(crate) enum CertifiedFileError {
    Source(Box<vm::SourcePathError>),
    Vm(Box<vm::VmError>),
}

fn assemble(machine: &mut vm::Vm) -> vm::VmResult<()> {
    let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
    registry(&catalog)?.bind_vm_cached(machine)?;
    initialize(machine)
}
