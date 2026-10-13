use std::sync::Arc;

use vm::{HostFunctionRegistry, cli::CliHostServices};

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
