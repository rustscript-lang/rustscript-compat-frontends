use vm::{ImportClause, ModuleImport, NamedImport};

pub(crate) fn parse_js_imports(source: &str) -> Vec<ModuleImport> {
    let mut imports = Vec::new();
    let mut pending = String::new();
    let mut pending_line = 0usize;
    for (index, raw_line) in source.lines().enumerate() {
        let line_no = index + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        if pending.is_empty() {
            if !line.starts_with("import ") {
                continue;
            }
            pending_line = line_no;
        }
        if !pending.is_empty() {
            pending.push(' ');
        }
        pending.push_str(line);
        if line.ends_with(';') || line.contains(" from ") || is_side_effect_import(line) {
            if let Some(import) = parse_js_import_from_block(&pending, pending_line) {
                imports.push(import);
            }
            pending.clear();
        }
    }
    if !pending.is_empty()
        && let Some(import) = parse_js_import_from_block(&pending, pending_line)
    {
        imports.push(import);
    }
    imports
}

fn is_side_effect_import(line: &str) -> bool {
    line.strip_prefix("import ")
        .and_then(|tail| extract_quoted_literal(tail))
        .is_some()
}

fn parse_js_import_from_block(block: &str, line: usize) -> Option<ModuleImport> {
    if let Some(from_idx) = block.find(" from ") {
        let head = block[..from_idx].trim();
        let clause = parse_import_clause_head(head.strip_prefix("import")?.trim())?;
        let tail = &block[from_idx + " from ".len()..];
        let (spec, _) = extract_quoted_literal(tail)?;
        return Some(ModuleImport {
            spec: spec.to_string(),
            clause,
            line,
        });
    }
    let tail = block.strip_prefix("import ")?;
    extract_quoted_literal(tail).map(|(spec, _)| ModuleImport {
        spec: spec.to_string(),
        clause: ImportClause::AllPublic,
        line,
    })
}

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
            });
        }
    }
    imports
}

fn parse_import_clause_head(head: &str) -> Option<ImportClause> {
    let trimmed = head.trim();
    if let Some(rest) = trimmed.strip_prefix("*") {
        let alias = rest.trim().strip_prefix("as")?.trim();
        return is_valid_ident(alias).then(|| ImportClause::Namespace(alias.to_string()));
    }
    if let Some(rest) = trimmed.strip_prefix('{') {
        let inner = rest.strip_suffix('}')?.trim();
        let named = parse_named_imports(inner)?;
        return Some(ImportClause::Named(named));
    }
    is_valid_ident(trimmed).then(|| {
        ImportClause::Named(vec![NamedImport {
            imported: "default".to_string(),
            local: trimmed.to_string(),
        }])
    })
}

fn parse_named_imports(input: &str) -> Option<Vec<NamedImport>> {
    let mut named = Vec::new();
    for part in input.split(',') {
        let entry = part.trim();
        if entry.is_empty() {
            continue;
        }

        if let Some((imported, local)) = entry.split_once(" as ") {
            let imported = imported.trim();
            let local = local.trim();
            if !is_valid_ident(imported) || !is_valid_ident(local) {
                return None;
            }
            named.push(NamedImport {
                imported: imported.to_string(),
                local: local.to_string(),
            });
            continue;
        }

        if !is_valid_ident(entry) {
            return None;
        }
        named.push(NamedImport {
            imported: entry.to_string(),
            local: entry.to_string(),
        });
    }

    Some(named)
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
        });
    }

    if let Some(member) = rest.strip_prefix(").") {
        let member = member.trim();
        if is_valid_ident(member) {
            return Some(ModuleImport {
                spec: spec.to_string(),
                clause: ImportClause::Named(vec![NamedImport {
                    imported: member.to_string(),
                    local: name.to_string(),
                }]),
                line,
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
