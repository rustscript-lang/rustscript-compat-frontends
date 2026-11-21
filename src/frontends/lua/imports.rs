//! Top-level static `.rss` links for the new Lua lowerer and file hook.
//!
//! Production `LuaPlugin` still uses the line scanner. This walk uses the
//! parsed chunk so a string, comment, uncalled function, shadow, or alias
//! cannot enter the module graph. Path mapping stays in Core.

use super::ast::{Args, AssignTarget, Expr, Stmt, Suffix};
use super::lexer;
use super::parser;
use vm::{ImportClause, ModuleImport, NamedImport, ParseError, SourceError, SourcePathError};

pub(crate) fn static_module_imports(source: &str) -> Result<Vec<ModuleImport>, SourcePathError> {
    let chunk = parser::parse_chunk(source.as_bytes()).map_err(syntax_to_path_error)?;
    let mut imports = Vec::new();
    let mut require_shadowed = false;
    for stmt in &chunk.body.stmts {
        if !require_shadowed && let Some(import) = local_static_link(source, stmt) {
            imports.push(import);
        }
        if binds_require(stmt) {
            require_shadowed = true;
        }
    }
    Ok(imports)
}

fn syntax_to_path_error(err: lexer::SyntaxError) -> SourcePathError {
    SourcePathError::Source(SourceError::Parse(ParseError::at_line(
        usize::try_from(err.line).unwrap_or(usize::MAX),
        err.message,
    )))
}

fn local_static_link(source: &str, stmt: &Stmt) -> Option<ModuleImport> {
    let Stmt::Local { names, values, .. } = stmt else {
        return None;
    };
    let [local_name] = names.as_slice() else {
        return None;
    };
    let [value, ..] = values.exprs.as_slice() else {
        return None;
    };
    let link = require_link(value)?;
    let alias = utf8_bytes(&local_name.name.bytes)?;
    let clause = match link.member {
        None => ImportClause::Namespace(alias),
        Some(member) => ImportClause::Named(vec![NamedImport {
            imported: member.clone(),
            local: member,
        }]),
    };
    Some(ModuleImport {
        spec: link.spec,
        line: line_at(source, link.name_start),
        clause,
    })
}

struct RequireLink {
    name_start: u32,
    spec: String,
    member: Option<String>,
}

fn require_link(expr: &Expr) -> Option<RequireLink> {
    let Expr::SuffixChain { base, steps, .. } = expr else {
        return None;
    };
    let Expr::Name { name, span, .. } = base.as_ref() else {
        return None;
    };
    if !name.is("require") {
        return None;
    }
    match steps.as_slice() {
        [
            Suffix::Call {
                args, nav: false, ..
            },
        ] => Some(RequireLink {
            name_start: span.start,
            spec: rss_spec(args)?,
            member: None,
        }),
        [
            Suffix::Call {
                args, nav: false, ..
            },
            Suffix::Field {
                name, nav: false, ..
            },
        ] => Some(RequireLink {
            name_start: span.start,
            spec: rss_spec(args)?,
            member: Some(utf8_bytes(&name.bytes)?),
        }),
        _ => None,
    }
}

fn rss_spec(args: &Args) -> Option<String> {
    let Args::List(list) = args else {
        return None;
    };
    let [Expr::String { bytes, .. }] = list.exprs.as_slice() else {
        return None;
    };
    let spec = utf8_bytes(bytes)?;
    spec.ends_with(".rss").then_some(spec)
}

fn binds_require(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Local { names, .. } => names.iter().any(|local| local.name.is("require")),
        Stmt::LocalFunction { name, .. } => name.is("require"),
        Stmt::Function { name, .. } => {
            name.method.is_none() && name.segments.len() == 1 && name.segments[0].is("require")
        }
        Stmt::Assign { targets, .. } => targets.iter().any(assign_target_is_require),
        _ => false,
    }
}

fn assign_target_is_require(target: &AssignTarget) -> bool {
    matches!(target, AssignTarget::Name(name) if name.is("require"))
}

fn utf8_bytes(bytes: &[u8]) -> Option<String> {
    String::from_utf8(bytes.to_vec()).ok()
}

fn line_at(source: &str, offset: u32) -> usize {
    let end = usize::try_from(offset).unwrap_or(source.len());
    let prefix = source.get(..end.min(source.len())).unwrap_or("");
    prefix.bytes().filter(|byte| *byte == b'\n').count() + 1
}
