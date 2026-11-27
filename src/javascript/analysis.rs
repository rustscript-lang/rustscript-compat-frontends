//! Plugin-aware physical analysis. The current lowering supplies no authenticated
//! logical JS schema projection. Never rename a private Named carrier to JsValue.
use std::path::Path;

/// Public Core facts remain explicitly physical, with original source names/spans.
pub struct JavaScriptEs5Analysis {
    physical_model: vm::compiler::SemanticModel,
    physical_type_hints: Vec<vm::compiler::InferredLocalTypeHint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JavaScriptEs5ProjectionUnavailable;
impl std::fmt::Display for JavaScriptEs5ProjectionUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("logical JavaScript type projection unavailable: the frontend supplies physical carrier schemas without source-type provenance")
    }
}
impl std::error::Error for JavaScriptEs5ProjectionUnavailable {}

impl JavaScriptEs5Analysis {
    pub fn physical_model(&self) -> &vm::compiler::SemanticModel {
        &self.physical_model
    }
    pub fn physical_type_hints(&self) -> &[vm::compiler::InferredLocalTypeHint] {
        &self.physical_type_hints
    }
    /// No user spelling or private record name can manufacture source authority.
    pub fn logical_type_hints(
        &self,
    ) -> Result<&[vm::compiler::InferredLocalTypeHint], JavaScriptEs5ProjectionUnavailable> {
        Err(JavaScriptEs5ProjectionUnavailable)
    }
}

/// Analyze the current editor buffer through the public plugin-aware file and
/// typehint APIs, using exactly the executable source catalog and strict policy.
/// Dependencies resolve relative to `path`; the entry buffer overrides disk.
pub fn analyze_javascript_es5_at_path(
    path: impl AsRef<Path>,
    source: &str,
) -> Result<JavaScriptEs5Analysis, vm::SourcePathError> {
    let path = path.as_ref();
    let options = super::cli::javascript_es5_cli_compile_options();
    let descriptor = vm::SourcePlugin::descriptor(&super::JAVASCRIPT_ES5_PLUGIN);
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_none_or(|extension| {
            !descriptor
                .extensions
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(extension))
        })
    {
        return Err(vm::SourcePathError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "JavaScript ES5 analysis requires a .js source path",
        )));
    }
    let physical_model =
        vm::analyze_source_from_string_with_options(path, source, options.clone())?;
    let physical_type_hints = vm::collect_inferred_local_type_hints_at_path_with_options(
        path,
        source,
        super::javascript_es5_flavor(),
        options,
    )?;
    Ok(JavaScriptEs5Analysis {
        physical_model,
        physical_type_hints,
    })
}
