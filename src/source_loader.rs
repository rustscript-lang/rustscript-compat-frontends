use vm::{DeclarationNamespace, ImportClause, ModuleImport, NamedImport};

pub(crate) fn parse_lua_imports(source: &str) -> Vec<ModuleImport> {
    let mut imports = Vec::new();
    for (index, raw_line) in source.lines().enumerate() {
        let line_no = index + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with("--") {
            continue;
        }
        if let Some((name, rhs)) = parse_lua_local_assignment(line)
            && let Some(import) = parse_lua_require_binding(name, rhs, line_no)
        {
            imports.push(import);
            continue;
        }
        if let Some(spec) = parse_require_spec(line) {
            imports.push(ModuleImport {
                spec,
                clause: ImportClause::AllPublic,
                line: line_no,
                namespace: DeclarationNamespace::Value,
                span: None,
            });
        }
    }
    imports
}

fn parse_lua_local_assignment(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("local ")?;
    let (name, rhs) = rest.split_once('=')?;
    let name = name.trim();
    let rhs = rhs.trim();
    if is_valid_ident(name) {
        Some((name, rhs))
    } else {
        None
    }
}

fn parse_lua_require_binding(name: &str, rhs: &str, line: usize) -> Option<ModuleImport> {
    let require_idx = rhs.find("require(")?;
    let require_head = rhs[..require_idx].trim();
    if !require_head.is_empty() {
        return None;
    }

    let tail = &rhs[require_idx + "require(".len()..];
    let (spec, rest) = extract_quoted_literal(tail)?;
    let rest = rest.trim();
    if rest.is_empty() || rest == ")" {
        return Some(ModuleImport {
            spec: spec.to_string(),
            clause: ImportClause::Namespace(name.to_string()),
            line,
            namespace: DeclarationNamespace::Value,
            span: None,
        });
    }

    if let Some(member) = rest.strip_prefix(").") {
        let member = member.trim();
        if is_valid_ident(member) {
            return Some(ModuleImport {
                spec: spec.to_string(),
                clause: ImportClause::Named(vec![NamedImport::value(member, name)]),
                line,
                namespace: DeclarationNamespace::Value,
                span: None,
            });
        }
    }

    None
}

fn parse_require_spec(line: &str) -> Option<String> {
    let require_idx = line.find("require(")?;
    let tail = &line[require_idx + "require(".len()..];
    let (spec, _) = extract_quoted_literal(tail)?;
    Some(spec.to_string())
}

fn extract_quoted_literal(input: &str) -> Option<(&str, &str)> {
    let bytes = input.as_bytes();
    let mut start_idx = None;
    let mut quote = b'"';
    for (idx, byte) in bytes.iter().enumerate() {
        if *byte == b'"' || *byte == b'\'' {
            start_idx = Some(idx);
            quote = *byte;
            break;
        }
    }
    let start = start_idx?;
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == quote {
            return Some((&input[start + 1..i], &input[i + 1..]));
        }
        i += 1;
    }
    None
}

fn is_valid_ident(input: &str) -> bool {
    let mut chars = input.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !is_ident_start(first) {
        return false;
    }
    chars.all(is_ident_continue)
}

pub(crate) fn is_ident_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

pub(crate) fn is_ident_continue(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

pub(crate) fn is_file_module_spec(spec: &str) -> bool {
    spec.contains('/')
        || spec.starts_with('.')
        || spec.ends_with(".rss")
        || spec.ends_with(".ts")
        || spec.ends_with(".mts")
}
