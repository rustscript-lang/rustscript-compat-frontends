mod array_lowering;
mod default_lowering;
mod generator_lowering;
mod parser;
mod validation;

use std::path::Path;
use std::sync::Arc;

use vm::{
    CompileSourceFileOptions, FrontendId, FrontendIr, FrontendTypeExport, FrontendTypingPolicy,
    ParseError, SourceFrontendContext, SourceFrontendDescriptor, SourcePathError, SourcePlugin,
};

pub struct TypeScriptPlugin;
pub struct TypeScriptReferenceArrayPlugin;
pub struct LuaPlugin;

pub static TYPESCRIPT_PLUGIN: TypeScriptPlugin = TypeScriptPlugin;
pub static TYPESCRIPT_REFERENCE_ARRAY_PLUGIN: TypeScriptReferenceArrayPlugin =
    TypeScriptReferenceArrayPlugin;
pub static LUA_PLUGIN: LuaPlugin = LuaPlugin;

pub fn plugins() -> [&'static dyn SourcePlugin; 2] {
    [&TYPESCRIPT_PLUGIN, &LUA_PLUGIN]
}

impl SourcePlugin for TypeScriptPlugin {
    fn descriptor(&self) -> SourceFrontendDescriptor {
        SourceFrontendDescriptor {
            id: FrontendId::new("typescript"),
            extensions: &["ts", "mts"],
            typing_policy: FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }

    fn parse_source(
        &self,
        source: &str,
        context: &SourceFrontendContext<'_>,
    ) -> Result<FrontendIr, ParseError> {
        parser::lower(source, context)
    }

    fn module_type_exports(
        &self,
        source: &str,
        context: &SourceFrontendContext<'_>,
        ir: &FrontendIr,
    ) -> Result<Vec<FrontendTypeExport>, ParseError> {
        parser::exports(source, context, ir)
    }

    fn validate_typed_ir(
        &self,
        typed_ir: &vm::compiler::FrontendTypedIr<'_>,
    ) -> Result<(), vm::compiler::CompileError> {
        validation::validate(typed_ir)
    }

    fn parse_module_imports(
        &self,
        source: &str,
        _path: &Path,
        context: &SourceFrontendContext<'_>,
    ) -> Result<Vec<vm::ModuleImport>, SourcePathError> {
        parser::parse_imports_for_plugin(source, context)
    }

    fn strip_import_directives(&self, source: &str) -> String {
        source.to_owned()
    }
}

/// Explicit opt-in TypeScript reference-array compilation mode.
///
/// This is the same TypeScript frontend identity (same `FrontendId`,
/// extensions and ownership policy), not a new language: an alternative
/// configuration of the same frontend. Ordinary `compile_options()` and
/// the plain `TypeScriptPlugin` stay source-compatible and unchanged.
/// In this mode logical `number[]` bindings lower to the private B1-backed
/// carrier described in `array_lowering`, while Core checks only the real
/// physical schemas.
impl SourcePlugin for TypeScriptReferenceArrayPlugin {
    fn descriptor(&self) -> SourceFrontendDescriptor {
        SourceFrontendDescriptor {
            id: FrontendId::new("typescript"),
            extensions: &["ts", "mts"],
            typing_policy: FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }

    fn parse_source(
        &self,
        source: &str,
        context: &SourceFrontendContext<'_>,
    ) -> Result<FrontendIr, ParseError> {
        parser::lower_reference_array(source, context)
    }

    fn module_type_exports(
        &self,
        source: &str,
        context: &SourceFrontendContext<'_>,
        ir: &FrontendIr,
    ) -> Result<Vec<FrontendTypeExport>, ParseError> {
        parser::exports(source, context, ir)
    }

    fn validate_typed_ir(
        &self,
        typed_ir: &vm::compiler::FrontendTypedIr<'_>,
    ) -> Result<(), vm::compiler::CompileError> {
        validation::validate(typed_ir)
    }

    fn parse_module_imports(
        &self,
        source: &str,
        _path: &Path,
        context: &SourceFrontendContext<'_>,
    ) -> Result<Vec<vm::ModuleImport>, SourcePathError> {
        parser::parse_reference_array_imports_for_plugin(source, context)
    }

    fn strip_import_directives(&self, source: &str) -> String {
        source.to_owned()
    }
}

impl SourcePlugin for LuaPlugin {
    fn descriptor(&self) -> SourceFrontendDescriptor {
        SourceFrontendDescriptor {
            id: FrontendId::new("lua"),
            extensions: &["lua"],
            typing_policy: FrontendTypingPolicy::Dynamic,
            supports_ownership_checking: false,
        }
    }

    fn parse_source(
        &self,
        source: &str,
        _context: &SourceFrontendContext<'_>,
    ) -> Result<FrontendIr, ParseError> {
        crate::lua::lower_to_ir(source)
    }

    fn parse_module_imports(
        &self,
        source: &str,
        _path: &Path,
        _context: &SourceFrontendContext<'_>,
    ) -> Result<Vec<vm::ModuleImport>, SourcePathError> {
        Ok(crate::source_loader::parse_lua_imports(source))
    }
}

pub fn compile_options() -> CompileSourceFileOptions {
    let options = plugins()
        .into_iter()
        .fold(CompileSourceFileOptions::new(), |options, plugin| {
            options.with_source_plugin(plugin)
        });
    options.with_host_api_catalog(crate::runtime::scalar_error::typescript_error_host_catalog())
}

pub fn compile_options_with_catalog(catalog: Arc<vm::HostApiCatalog>) -> CompileSourceFileOptions {
    compile_options().with_host_api_catalog(catalog)
}

/// Exact effective catalog for reference-array compilation and execution.
pub fn reference_array_effective_catalog() -> Arc<vm::HostApiCatalog> {
    array_lowering::reference_array_effective_catalog()
}

/// Fixed accepted B1 prelude for the JS numeric-array realm.
///
/// Parses only the fixed accepted prelude plus empty generated user input;
/// raw user JS never enters the TypeScript parser. The returned IR keeps its
/// real function declarations, implementations, host metadata, schemas,
/// resource provenance and catalog identity; JS appends generated IR, fresh
/// slots and root statements without rebuilding host candidate metadata.
pub(crate) fn js_realm_prelude_ir(
    context: &vm::SourceFrontendContext<'_>,
) -> Result<vm::FrontendIr, vm::ParseError> {
    parser::lower_reference_array("", context)
}

/// Prelude helper identities reused by the JS realm. Indices are resolved
/// from the generated `ir.functions` table, never from user identifier text.
pub(crate) use array_lowering::{
    HELPER_LENGTH, HELPER_MAKE, HELPER_READ, HELPER_RESIZE, HELPER_WRITE,
};

/// Compile options for the explicit reference-array mode: the alternative
/// same-identity TypeScript plugin plus Lua, with the exact effective
/// catalog. Ordinary options are unchanged.
pub fn reference_array_compile_options() -> CompileSourceFileOptions {
    CompileSourceFileOptions::new()
        .with_source_plugin(&TYPESCRIPT_REFERENCE_ARRAY_PLUGIN)
        .with_source_plugin(&LUA_PLUGIN)
        .with_host_api_catalog(reference_array_effective_catalog())
}

/// Like [`collect_typescript_type_hints_with_options`], but projects the
/// validated private carrier provenance back to the logical `number[]`
/// source type. Only the exact physical carrier schema maps back; user
/// callables, global slots and arbitrary shapes never qualify.
pub fn collect_reference_array_type_hints_with_options(
    source: &str,
    options: CompileSourceFileOptions,
) -> Result<Vec<vm::compiler::InferredLocalTypeHint>, SourcePathError> {
    let mut hints =
        vm::collect_inferred_local_type_hints_with_options(source, typescript_flavor(), options)?;
    format_typescript_type_hints(&mut hints);
    for hint in &mut hints {
        // Core renames guest declarations in compiled output
        // (`__CompatRefCarrier` becomes a `__guest_type_*` nominal), while
        // the provenance-backed source spelling stays on the label. Remap
        // only when both agree: a bare nominal schema with exactly the
        // carrier spelling. Callable shapes and names never qualify.
        let is_carrier = hint.inferred_type == array_lowering::TYPE_CARRIER
            && hint.schema.as_ref().is_some_and(|schema| {
                matches!(
                    schema,
                    vm::TypeSchema::Named(_, args) if args.is_empty()
                )
            });
        if is_carrier {
            hint.inferred_type = "number[]".to_owned();
            hint.schema = Some(vm::TypeSchema::Array(Box::new(vm::TypeSchema::Number)));
        }
    }
    Ok(hints)
}

/// Path-based variant of
/// [`collect_reference_array_type_hints_with_options`] for real file
/// dependencies: hints flow through the module graph, so a wrapper that
/// imports a sorting module projects the logical `number[]` through the
/// dependency and any import alias.
pub fn collect_reference_array_type_hints_at_path_with_options(
    path: impl AsRef<Path>,
    source: &str,
    options: CompileSourceFileOptions,
) -> Result<Vec<vm::compiler::InferredLocalTypeHint>, SourcePathError> {
    let mut hints = vm::collect_inferred_local_type_hints_at_path_with_options(
        path,
        source,
        typescript_flavor(),
        options,
    )?;
    format_typescript_type_hints(&mut hints);
    for hint in &mut hints {
        let is_carrier = hint.inferred_type == array_lowering::TYPE_CARRIER
            && hint.schema.as_ref().is_some_and(|schema| {
                matches!(
                    schema,
                    vm::TypeSchema::Named(_, args) if args.is_empty()
                )
            });
        if is_carrier {
            hint.inferred_type = "number[]".to_owned();
            hint.schema = Some(vm::TypeSchema::Array(Box::new(vm::TypeSchema::Number)));
        }
    }
    Ok(hints)
}

pub fn collect_typescript_type_hints(
    source: &str,
) -> Result<Vec<vm::compiler::InferredLocalTypeHint>, SourcePathError> {
    collect_typescript_type_hints_with_options(source, compile_options())
}

pub fn collect_typescript_type_hints_with_options(
    source: &str,
    options: CompileSourceFileOptions,
) -> Result<Vec<vm::compiler::InferredLocalTypeHint>, SourcePathError> {
    let mut hints =
        vm::collect_inferred_local_type_hints_with_options(source, typescript_flavor(), options)?;
    format_typescript_type_hints(&mut hints);
    Ok(hints)
}

pub fn collect_typescript_type_hints_at_path_with_options(
    path: impl AsRef<Path>,
    source: &str,
    options: CompileSourceFileOptions,
) -> Result<Vec<vm::compiler::InferredLocalTypeHint>, SourcePathError> {
    let mut hints = vm::collect_inferred_local_type_hints_at_path_with_options(
        path,
        source,
        typescript_flavor(),
        options,
    )?;
    format_typescript_type_hints(&mut hints);
    Ok(hints)
}

fn format_typescript_type_hints(hints: &mut [vm::compiler::InferredLocalTypeHint]) {
    for hint in hints {
        if let Some(schema) = &hint.schema {
            hint.inferred_type = format_typescript_schema(schema, &hint.inferred_type);
        }
    }
}

fn split_top_level(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut stack = Vec::new();
    let mut start = 0;
    for (offset, ch) in input.char_indices() {
        match ch {
            '<' | '(' | '[' | '{' => stack.push(ch),
            '>' if stack.last() == Some(&'<') => {
                stack.pop();
            }
            ')' if stack.last() == Some(&'(') => {
                stack.pop();
            }
            ']' if stack.last() == Some(&'[') => {
                stack.pop();
            }
            '}' if stack.last() == Some(&'{') => {
                stack.pop();
            }
            ',' if stack.is_empty() => {
                parts.push(input[start..offset].trim());
                start = offset + ch.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(input[start..].trim());
    if parts.len() == 1 && parts[0].is_empty() {
        Vec::new()
    } else {
        parts
    }
}

fn parenthesized<'a>(input: &'a str, prefix: &str) -> Option<(&'a str, &'a str)> {
    let rest = input.strip_prefix(prefix)?;
    let mut depth = 1usize;
    for (offset, ch) in rest.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some((&rest[..offset], &rest[offset + 1..]));
                }
            }
            _ => {}
        }
    }
    None
}

fn format_typescript_schema(schema: &vm::TypeSchema, source_label: &str) -> String {
    use vm::TypeSchema;

    match schema {
        TypeSchema::Unknown => "unknown".to_owned(),
        TypeSchema::Null => "null".to_owned(),
        TypeSchema::Int => "Int".to_owned(),
        TypeSchema::Float => "Float".to_owned(),
        TypeSchema::Number => "number".to_owned(),
        TypeSchema::Bool => "boolean".to_owned(),
        TypeSchema::String => "string".to_owned(),
        TypeSchema::Bytes => "Bytes".to_owned(),
        TypeSchema::Optional(inner) => {
            let inner_label = source_label.strip_suffix('?').unwrap_or(source_label);
            format!("{} | null", format_typescript_schema(inner, inner_label))
        }
        TypeSchema::GenericParam(name) => name.clone(),
        TypeSchema::Named(name, arguments) => {
            if let Some(open) = source_label.find('<')
                && source_label.ends_with('>')
            {
                let display_name = source_label[..open].trim();
                let arguments_label = &source_label[open + 1..source_label.len() - 1];
                let labels = split_top_level(arguments_label);
                if labels.len() == arguments.len() {
                    return format!(
                        "{display_name}<{}>",
                        arguments
                            .iter()
                            .zip(labels)
                            .map(|(argument, label)| format_typescript_schema(argument, label))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
            }
            if source_label.is_empty() {
                name.clone()
            } else {
                source_label.to_owned()
            }
        }
        TypeSchema::Array(inner) => {
            if let Some(contents) = source_label
                .strip_prefix('[')
                .and_then(|label| label.strip_suffix(']'))
            {
                format!("{}[]", format_typescript_schema(inner, contents))
            } else {
                format!("{}[]", format_typescript_schema(inner, source_label))
            }
        }
        TypeSchema::ArrayTuple(items) => {
            let labels = source_label
                .strip_prefix('[')
                .and_then(|label| label.strip_suffix(']'))
                .map(split_top_level)
                .unwrap_or_default();
            format!(
                "[{}]",
                items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| format_typescript_schema(
                        item,
                        labels.get(index).copied().unwrap_or_default(),
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        TypeSchema::ArrayTupleRest { prefix, rest } => {
            let labels = source_label
                .strip_prefix('[')
                .and_then(|label| label.strip_suffix(']'))
                .map(split_top_level)
                .unwrap_or_default();
            let mut items = prefix
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    format_typescript_schema(item, labels.get(index).copied().unwrap_or_default())
                })
                .collect::<Vec<_>>();
            let rest_label = labels
                .get(prefix.len())
                .copied()
                .unwrap_or_default()
                .strip_suffix("...")
                .unwrap_or_default();
            items.push(format!("...{}", format_typescript_schema(rest, rest_label)));
            format!("[{}]", items.join(", "))
        }
        TypeSchema::Map(value) => {
            let value_label = source_label
                .strip_prefix("map<")
                .and_then(|label| label.strip_suffix('>'))
                .unwrap_or(source_label);
            format!(
                "Record<string, {}>",
                format_typescript_schema(value, value_label)
            )
        }
        TypeSchema::Object(fields) => {
            let source_fields = source_label
                .strip_prefix('{')
                .and_then(|label| label.strip_suffix('}'))
                .map(split_top_level)
                .unwrap_or_default();
            let mut display_fields = fields.iter().collect::<Vec<_>>();
            display_fields.sort_unstable_by_key(|(left, _)| *left);
            let rendered = display_fields
                .into_iter()
                .map(|(name, field_schema)| {
                    let field_label = source_fields
                        .iter()
                        .find_map(|entry| {
                            let (field_name, field_type) = entry.split_once(':')?;
                            (field_name.trim() == name).then_some(field_type.trim())
                        })
                        .unwrap_or_default();
                    match field_schema {
                        TypeSchema::Optional(inner) => format!(
                            "{name}?: {}",
                            format_typescript_schema(
                                inner,
                                field_label.strip_suffix('?').unwrap_or(field_label),
                            )
                        ),
                        other => {
                            format!("{name}: {}", format_typescript_schema(other, field_label))
                        }
                    }
                })
                .collect::<Vec<_>>()
                .join("; ");
            format!("{{ {rendered} }}")
        }
        TypeSchema::Callable { params, result } => {
            let parsed = parenthesized(source_label, "fn(");
            let (param_labels, result_label) = parsed
                .and_then(|(params, rest)| {
                    Some((split_top_level(params), rest.strip_prefix(" -> ")?))
                })
                .unwrap_or_default();
            format!(
                "({}) => {}",
                params
                    .iter()
                    .enumerate()
                    .map(|(index, param)| format_typescript_schema(
                        param,
                        param_labels.get(index).copied().unwrap_or_default(),
                    ))
                    .collect::<Vec<_>>()
                    .join(", "),
                format_typescript_schema(result, result_label)
            )
        }
        TypeSchema::Resource(key) => format!("Resource<{}>", key.as_str()),
        TypeSchema::Result { ok, err } => {
            let labels = source_label
                .strip_prefix("Result<")
                .and_then(|label| label.strip_suffix('>'))
                .map(split_top_level)
                .filter(|labels| labels.len() == 2);
            let (ok_label, err_label) = match labels {
                Some(labels) => (labels[0], labels[1]),
                None => ("", ""),
            };
            format!(
                "Result<{}, {}>",
                format_typescript_schema(ok, ok_label),
                format_typescript_schema(err, err_label)
            )
        }
    }
}

pub fn typescript_flavor() -> vm::SourceFlavor {
    vm::SourceFlavor::Plugin(FrontendId::new("typescript"))
}

pub fn lua_flavor() -> vm::SourceFlavor {
    vm::SourceFlavor::Plugin(FrontendId::new("lua"))
}

#[cfg(test)]
mod sdk_result_owner_tests {
    use vm::{ResourceTypeKey, TypeSchema};

    fn result(ok: TypeSchema, err: TypeSchema) -> TypeSchema {
        TypeSchema::Result {
            ok: Box::new(ok),
            err: Box::new(err),
        }
    }

    #[test]
    fn result_formatter_is_canonical_and_uses_source_labels_only_when_bounded() {
        let key = ResourceTypeKey::new("io.file").expect("valid resource key");
        let scalar = result(TypeSchema::Int, TypeSchema::String);
        assert_eq!(
            super::format_typescript_schema(&scalar, ""),
            "Result<Int, string>"
        );
        assert_eq!(
            super::format_typescript_schema(&scalar, "Result<Int, string>"),
            "Result<Int, string>"
        );
        assert_eq!(
            super::format_typescript_schema(&scalar, "Result<Int>"),
            "Result<Int, string>"
        );
        assert_eq!(
            super::format_typescript_schema(&scalar, "Result<Int, string, extra>"),
            "Result<Int, string>"
        );
        assert_eq!(
            super::format_typescript_schema(&scalar, "not-a-result"),
            "Result<Int, string>"
        );

        let named = result(
            TypeSchema::Named("Profile".to_owned(), vec![TypeSchema::Int]),
            TypeSchema::String,
        );
        assert_eq!(
            super::format_typescript_schema(&named, "Result<Profile<Int>, string>"),
            "Result<Profile<Int>, string>"
        );
        assert_eq!(
            super::format_typescript_schema(&named, ""),
            "Result<Profile, string>"
        );
        assert_eq!(
            super::format_typescript_schema(&named, "Profile<Int>, string"),
            "Result<Profile, string>"
        );

        let nested = result(
            result(TypeSchema::Int, TypeSchema::String),
            TypeSchema::Bool,
        );
        assert_eq!(
            super::format_typescript_schema(&nested, "Result<Result<Int, string>, boolean>"),
            "Result<Result<Int, string>, boolean>"
        );
        assert_eq!(
            super::format_typescript_schema(&nested, ""),
            "Result<Result<Int, string>, boolean>"
        );

        let resource = result(
            TypeSchema::Resource(key),
            TypeSchema::GenericParam("E".to_owned()),
        );
        assert_eq!(
            super::format_typescript_schema(&resource, ""),
            "Result<Resource<io.file>, E>"
        );
        assert_eq!(
            super::format_typescript_schema(&TypeSchema::Map(Box::new(TypeSchema::String)), ""),
            "Record<string, string>"
        );
        assert_eq!(
            super::format_typescript_schema(
                &TypeSchema::Named(
                    "Result".to_owned(),
                    vec![TypeSchema::Int, TypeSchema::String]
                ),
                ""
            ),
            "Result"
        );
    }
}
