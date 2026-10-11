mod parser;
mod validation;

use std::path::Path;
use std::sync::Arc;

use vm::{
    CompileSourceFileOptions, FrontendId, FrontendIr, FrontendTypeExport, FrontendTypingPolicy,
    ParseError, SourceFrontendContext, SourceFrontendDescriptor, SourcePathError, SourcePlugin,
};

pub struct TypeScriptPlugin;
pub struct LuaPlugin;

pub static TYPESCRIPT_PLUGIN: TypeScriptPlugin = TypeScriptPlugin;
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
    plugins()
        .into_iter()
        .fold(CompileSourceFileOptions::new(), |options, plugin| {
            options.with_source_plugin(plugin)
        })
}

pub fn compile_options_with_catalog(catalog: Arc<vm::HostApiCatalog>) -> CompileSourceFileOptions {
    compile_options().with_host_api_catalog(catalog)
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
    for hint in &mut hints {
        if let Some(schema) = &hint.schema {
            hint.inferred_type = format_typescript_schema(schema, &hint.inferred_type);
        }
    }
    Ok(hints)
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
            display_fields.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
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
    }
}

pub fn typescript_flavor() -> vm::SourceFlavor {
    vm::SourceFlavor::Plugin(FrontendId::new("typescript"))
}

pub fn lua_flavor() -> vm::SourceFlavor {
    vm::SourceFlavor::Plugin(FrontendId::new("lua"))
}
