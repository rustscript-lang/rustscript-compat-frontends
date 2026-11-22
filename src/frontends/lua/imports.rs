//! Top-level static RSS links for the new Lua lowerer and file hook.
//!
//! Production `LuaPlugin` still uses the line scanner. This walk uses the
//! parsed chunk so a string, comment, uncalled function, shadow, or ordinary
//! call cannot enter the module graph. A named link binds the declared local
//! alias. Path mapping stays in Core, including an extensionless relative spec.

use super::ast::{Args, AssignTarget, Expr, Stmt, Suffix};
use super::lexer;
use super::parser;
use vm::{ImportClause, ModuleImport, NamedImport, ParseError, SourceError, SourcePathError};

pub(super) fn static_module_imports(source: &str) -> Result<Vec<ModuleImport>, SourcePathError> {
    let chunk = parser::parse_chunk(source.as_bytes()).map_err(syntax_to_path_error)?;
    let mut imports = Vec::new();
    let mut lines = PhysicalLines {
        source: source.as_bytes(),
        offset: 0,
        line: 1,
    };
    let mut require_shadowed = false;
    for stmt in &chunk.body.stmts {
        if !require_shadowed && let Some(import) = local_static_link(stmt, &mut lines) {
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

fn local_static_link(stmt: &Stmt, lines: &mut PhysicalLines<'_>) -> Option<ModuleImport> {
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
            // Core binds this local name. The export stays in `imported`,
            // matching the line scanner's declared-name split.
            imported: member,
            local: alias,
        }]),
    };
    Some(ModuleImport {
        spec: link.spec,
        line: lines.line_at(link.name_start),
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
    is_static_rss_spec(&spec).then_some(spec)
}

fn is_static_rss_spec(spec: &str) -> bool {
    if spec.ends_with(".rss") {
        return true;
    }
    // Pinned Core appends `.rss` when a relative specifier has no extension.
    // Keep the source text. Explicit `.lua`, host names, and other short
    // forms stay ordinary calls.
    let relative = spec.starts_with("./") || spec.starts_with("../");
    if !relative {
        return false;
    }
    let file = spec.rsplit('/').next().unwrap_or(spec);
    !file.is_empty() && !file.contains('.')
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

// Links arrive in source order. Count raw bytes once, independently of Lua
// line directives that remap the lexer token lines used for diagnostics.
struct PhysicalLines<'a> {
    source: &'a [u8],
    offset: usize,
    line: usize,
}

impl PhysicalLines<'_> {
    fn line_at(&mut self, name_start: u32) -> usize {
        while self.offset < name_start as usize {
            let width = lexer::newline_width(&self.source[self.offset..]);
            self.line += usize::from(width != 0);
            self.offset += width.max(1);
        }
        self.line
    }
}
