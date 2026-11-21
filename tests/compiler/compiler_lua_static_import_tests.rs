//! Candidate static-link evidence.
//!
//! A static link is a chunk-top, direct, unshadowed, single local whose value
//! is a `require` of a `.rss` spec or a relative extensionless spec. Ordinary
//! calls stay in the AST. The old lowerer is not a dynamic require engine:
//! Halted, or any non-I/O error, is not execution success.
//!
//! The candidate module is included with the same `#[path]` layout as the
//! LuaJIT oracle tests. It is not a library export.
//!
//! Not static edges, and not implemented as runtime require:
//! bare `present`, absolute `/present`, hidden `./.present`, trailing-dot
//! `./present.`, explicit `.lua`, host names, a backslash separator, a newline
//! inside the string, and `require` split across tokens. A newline inside the
//! call parentheses is covered.

#[allow(dead_code)]
#[path = "../common/mod.rs"]
mod common;
use common::*;

#[path = "../../src/frontends/lua/ast.rs"]
mod ast;
#[path = "../../src/frontends/lua/imports.rs"]
mod imports;
#[path = "../../src/frontends/lua/lexer.rs"]
mod lexer;
#[path = "../../src/frontends/lua/parser.rs"]
mod parser;

use std::fs;
use std::path::{Path, PathBuf};

use vm::{
    FrontendImportSyntax, FrontendIr, ImportClause, ModuleImport, ParseError, SourceError,
    SourceFlavor, SourcePathError, SourcePlugin,
};

/// Test-only hook. Production `LuaPlugin` stays on the line scanner.
struct CandidateLuaPlugin;

static CANDIDATE_LUA_PLUGIN: CandidateLuaPlugin = CandidateLuaPlugin;

impl SourcePlugin for CandidateLuaPlugin {
    fn flavor(&self) -> SourceFlavor {
        SourceFlavor::Lua
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["lua"]
    }

    fn import_syntax(&self) -> FrontendImportSyntax {
        FrontendImportSyntax::Lua
    }

    fn parse_source(&self, source: &str) -> Result<FrontendIr, ParseError> {
        pd_vm_compat_frontends::LUA_PLUGIN.parse_source(source)
    }

    fn parse_module_imports(
        &self,
        source: &str,
        _path: &Path,
    ) -> Result<Vec<ModuleImport>, SourcePathError> {
        imports::static_module_imports(source)
    }
}

fn candidate_options() -> CompileSourceFileOptions {
    CompileSourceFileOptions::new().with_source_plugin(&CANDIDATE_LUA_PLUGIN)
}

fn owned_root(label: &str) -> PathBuf {
    namespace_case_root(&format!("lua_static_import_{label}"))
}

fn write_present(root: &Path) {
    fs::write(root.join("present.rss"), "pub fn tag() { 41 }\n").expect("present module");
}

fn write_source(root: &Path, name: &str, source: &str) -> PathBuf {
    let path = root.join(name);
    fs::write(&path, source).expect("source");
    path
}

fn static_imports(source: &str) -> Vec<ModuleImport> {
    imports::static_module_imports(source).expect("static import scan")
}

fn assert_namespace(import: &ModuleImport, spec: &str, alias: &str, line: usize) {
    assert_eq!(import.spec, spec, "spec text is not a resolved path");
    assert_eq!(import.line, line);
    match &import.clause {
        ImportClause::Namespace(name) => assert_eq!(name, alias),
        other => panic!("expected namespace clause, got {other:?}"),
    }
}

fn assert_named(import: &ModuleImport, spec: &str, imported: &str, local: &str, line: usize) {
    assert_eq!(import.spec, spec, "spec text is not a resolved path");
    assert_eq!(import.line, line);
    match &import.clause {
        ImportClause::Named(named) => {
            assert_eq!(named.len(), 1);
            assert_eq!(named[0].imported, imported);
            assert_eq!(named[0].local, local);
        }
        other => panic!("expected named clause, got {other:?}"),
    }
}

fn specs(imports: &[ModuleImport]) -> Vec<&str> {
    imports.iter().map(|import| import.spec.as_str()).collect()
}

fn compile_candidate(path: &Path) -> Result<vm::CompiledProgram, SourcePathError> {
    compile_source_file_with_options(path, candidate_options())
}

fn compile_production(path: &Path) -> Result<vm::CompiledProgram, SourcePathError> {
    compile_source_file_with_options(path, pd_vm_compat_frontends::compile_options())
}

fn expect_load_error(
    result: Result<vm::CompiledProgram, SourcePathError>,
    what: &str,
) -> SourcePathError {
    match result {
        Ok(_) => panic!("{what}: expected failure, compile succeeded"),
        Err(err) => err,
    }
}

fn parse_message(err: &SourcePathError) -> &str {
    match err {
        SourcePathError::Source(SourceError::Parse(parse)) => parse.message.as_str(),
        SourcePathError::SourceWithMap {
            error: SourceError::Parse(parse),
            ..
        } => parse.message.as_str(),
        other => panic!("expected a parse error, got {other:?}"),
    }
}

fn assert_not_found(err: &SourcePathError, what: &str) {
    match err {
        SourcePathError::Io(io) => {
            assert_eq!(
                io.kind(),
                std::io::ErrorKind::NotFound,
                "{what}: path mapping belongs to Core; the failure is the module read, got {io}"
            );
        }
        other => panic!("{what}: expected Io(NotFound), got {other:?}"),
    }
}

fn halted_stack(compiled: vm::CompiledProgram, what: &str) -> Vec<Value> {
    let mut vm = Vm::new(compiled.program);
    let status = vm
        .run()
        .unwrap_or_else(|err| panic!("{what}: vm run failed: {err}"));
    assert_eq!(status, VmStatus::Halted, "{what}");
    vm.stack().to_vec()
}

fn run_candidate(path: &Path, what: &str) -> Vec<Value> {
    let compiled = compile_candidate(path).unwrap_or_else(|err| panic!("{what}: {err:?}"));
    halted_stack(compiled, what)
}

fn run_production(path: &Path, what: &str) -> Vec<Value> {
    let compiled = compile_production(path).unwrap_or_else(|err| panic!("{what}: {err:?}"));
    halted_stack(compiled, what)
}

fn parsed(source: &str) -> ast::Chunk {
    parser::parse_chunk(source.as_bytes()).expect("fixture parses")
}

fn require_spec(expr: &ast::Expr) -> Option<String> {
    let ast::Expr::SuffixChain { base, steps, .. } = expr else {
        return None;
    };
    let ast::Expr::Name { name, .. } = base.as_ref() else {
        return None;
    };
    if !name.is("require") {
        return None;
    }
    let [ast::Suffix::Call { args, .. }, ..] = steps.as_slice() else {
        return None;
    };
    let ast::Args::List(list) = args else {
        return None;
    };
    let [ast::Expr::String { bytes, .. }] = list.exprs.as_slice() else {
        return None;
    };
    String::from_utf8(bytes.clone()).ok()
}

fn collect_require_specs(stmts: &[ast::Stmt], out: &mut Vec<String>) {
    for stmt in stmts {
        match stmt {
            ast::Stmt::Local { values, .. } | ast::Stmt::Assign { values, .. } => {
                for expr in &values.exprs {
                    if let Some(spec) = require_spec(expr) {
                        out.push(spec);
                    }
                }
            }
            ast::Stmt::Call { call, .. } => {
                if let Some(spec) = require_spec(call) {
                    out.push(spec);
                }
            }
            ast::Stmt::Return { values, .. } => {
                for expr in &values.exprs {
                    if let Some(spec) = require_spec(expr) {
                        out.push(spec);
                    }
                }
            }
            ast::Stmt::LocalFunction { func, .. } => {
                collect_require_specs(&func.body.stmts, out);
            }
            ast::Stmt::Function { body, .. } => collect_require_specs(&body.body.stmts, out),
            ast::Stmt::Do { body, .. } => collect_require_specs(&body.stmts, out),
            _ => {}
        }
    }
}

fn require_specs(source: &str) -> Vec<String> {
    let chunk = parsed(source);
    let mut specs = Vec::new();
    collect_require_specs(&chunk.body.stmts, &mut specs);
    specs
}

const LOWERER_UNSUPPORTED: &str = "lua direct lowering does not yet support this construct";

#[test]
fn candidate_runs_present_rss_and_ignores_string_require() {
    let root = owned_root("present_string_pair");
    write_present(&root);
    let source = "local quoted = \"require('./absent.rss')\"\nlocal m = require(\"./present.rss\")\nm.tag()\n";
    let main_path = write_source(&root, "main.lua", source);

    let imports = static_imports(source);
    assert_eq!(specs(&imports), vec!["./present.rss"]);
    assert_namespace(&imports[0], "./present.rss", "m", 2);
    assert_eq!(
        require_specs(source),
        vec!["./present.rss"],
        "the quoted require is a string, not a call"
    );
    assert_eq!(
        run_candidate(&main_path, "namespace link"),
        vec![Value::Int(41)]
    );
}

#[test]
fn candidate_missing_rss_link_is_core_io_failure() {
    let root = owned_root("missing_rss");
    let source = "local m = require(\"./absent.rss\")\nm.tag()\n";
    let main_path = write_source(&root, "main.lua", source);

    let imports = static_imports(source);
    assert_eq!(specs(&imports), vec!["./absent.rss"]);
    assert_namespace(&imports[0], "./absent.rss", "m", 1);
    let err = expect_load_error(compile_candidate(&main_path), "missing static link");
    assert_not_found(&err, "missing static link");
}

#[test]
fn long_comment_stays_out_of_the_candidate_graph() {
    let source = "--[=[\nrequire(\"./absent.rss\")\n]=]\nlocal n = 41\n";
    assert!(static_imports(source).is_empty());
    assert!(
        require_specs(source).is_empty(),
        "the new parser consumes the long comment"
    );

    let root = owned_root("long_comment");
    let main_path = write_source(&root, "main.lua", source);
    let err = expect_load_error(compile_candidate(&main_path), "long comment lowerer");
    assert_eq!(
        parse_message(&err),
        LOWERER_UNSUPPORTED,
        "old lowerer does not understand --[=[; this is not execution success"
    );

    let production = expect_load_error(
        compile_production(&main_path),
        "old scanner mis-reads the long comment",
    );
    assert_not_found(
        &production,
        "production line scanner still treats the comment body as a require",
    );
}

#[test]
fn uncalled_function_require_stays_in_the_ast() {
    let source = "local function unused()\n  return require(\"./absent.rss\")\nend\nlocal m = require(\"./present.rss\")\nm.tag()\n";
    let imports = static_imports(source);
    assert_eq!(specs(&imports), vec!["./present.rss"]);
    assert_eq!(
        require_specs(source),
        vec!["./absent.rss", "./present.rss"],
        "the inner require remains an ordinary call"
    );

    let root = owned_root("uncalled");
    write_present(&root);
    let main_path = write_source(&root, "main.lua", source);
    let err = expect_load_error(compile_candidate(&main_path), "uncalled require");
    assert_eq!(
        parse_message(&err),
        "unknown function 'require'",
        "old lowerer cannot execute the inner require; do not treat this as a dynamic success"
    );
}

#[test]
fn shadowed_require_is_not_a_static_link() {
    let source = "local function require(name)\n  return name\nend\nlocal value = require(\"./absent.rss\")\nlocal n = 41\n";
    assert!(specs(&static_imports(source)).is_empty());
    assert_eq!(require_specs(source), vec!["./absent.rss"]);
    match &parsed(source).body.stmts[0] {
        ast::Stmt::LocalFunction { name, .. } => assert!(name.is("require")),
        other => panic!("shadow must be a local function, got {other:?}"),
    }

    let root = owned_root("shadow");
    let main_path = write_source(&root, "main.lua", source);
    let production =
        expect_load_error(compile_production(&main_path), "old scanner ignores shadow");
    assert_not_found(
        &production,
        "production line scanner still links a shadowed require",
    );
}

#[test]
fn require_name_alias_is_not_a_static_link() {
    let source = "local r = require\nlocal value = r(\"./absent.rss\")\nlocal n = 41\n";
    assert!(static_imports(source).is_empty());
    assert!(
        require_specs(source).is_empty(),
        "r(...) is not a require call in the AST"
    );

    let root = owned_root("require_alias");
    let main_path = write_source(&root, "main.lua", source);
    let err = expect_load_error(compile_candidate(&main_path), "require alias");
    assert_eq!(parse_message(&err), LOWERER_UNSUPPORTED);
}

#[test]
fn assignment_require_is_not_a_static_link() {
    let source = "m.tag = require(\"./absent.rss\")\n";
    assert!(static_imports(source).is_empty());
    match &parsed(source).body.stmts[0] {
        ast::Stmt::Assign {
            targets, values, ..
        } => {
            assert!(!targets[0].has_nav());
            assert!(targets[0].span().end > targets[0].span().start);
            assert!(values.tail_multi());
            assert!(values.exprs[0].is_multi());
            assert_eq!(
                require_spec(&values.exprs[0]).as_deref(),
                Some("./absent.rss")
            );
        }
        other => panic!("ordinary assignment must stay in the AST, got {other:?}"),
    }
}

#[test]
fn bare_require_stays_a_call_and_is_not_a_static_link() {
    let source = "require(\"./absent.rss\")\nlocal m = require(\"./present.rss\")\nm.tag()\n";
    let imports = static_imports(source);
    assert_eq!(specs(&imports), vec!["./present.rss"]);
    assert_eq!(require_specs(source), vec!["./absent.rss", "./present.rss"]);
    match &parsed(source).body.stmts[0] {
        ast::Stmt::Call { call, .. } => {
            assert_eq!(require_spec(call).as_deref(), Some("./absent.rss"));
        }
        other => panic!("bare require must remain a call statement, got {other:?}"),
    }

    let root = owned_root("bare");
    write_present(&root);
    let main_path = write_source(&root, "main.lua", source);
    let production = expect_load_error(compile_production(&main_path), "old scanner bare require");
    assert_not_found(
        &production,
        "production line scanner still eagerly links a bare require",
    );
}

#[test]
fn static_link_keeps_multiline_require_line() {
    let source = "\nlocal m = require(\n    \"./present.rss\")\n";
    let imports = static_imports(source);
    assert_eq!(imports.len(), 1);
    assert_namespace(&imports[0], "./present.rss", "m", 2);
}

#[test]
fn static_link_keeps_named_member_clause() {
    let source = "local tag = require(\"./left.rss\").tag\n";
    let imports = static_imports(source);
    assert_eq!(imports.len(), 1);
    assert_named(&imports[0], "./left.rss", "tag", "tag", 1);
}

#[test]
fn static_link_names_the_declared_alias_not_the_export() {
    let source = "local different = require(\"./present.rss\").tag\n";
    let imports = static_imports(source);
    assert_eq!(imports.len(), 1);
    assert_named(&imports[0], "./present.rss", "tag", "different", 1);
}

#[test]
fn candidate_named_missing_member_is_core_io_failure() {
    let root = owned_root("named_missing");
    let source = "local tag = require(\"./absent.rss\").tag\n";
    let main_path = write_source(&root, "main.lua", source);
    let err = expect_load_error(compile_candidate(&main_path), "named static link");
    assert_not_found(&err, "named missing module");
}

#[test]
fn named_missing_export_is_invalid_import_syntax() {
    let root = owned_root("missing_export");
    write_present(&root);
    let source = "local missing = require(\"./present.rss\").gone\n";
    let imports = static_imports(source);
    assert_named(&imports[0], "./present.rss", "gone", "missing", 1);
    let main_path = write_source(&root, "main.lua", source);
    let err = expect_load_error(compile_candidate(&main_path), "missing export");
    match err {
        SourcePathError::InvalidImportSyntax {
            line,
            message,
            path,
        } => {
            assert_eq!(line, 1);
            assert_eq!(
                message,
                "module './present.rss' has no public function 'gone'"
            );
            assert_eq!(
                path.file_name().and_then(|name| name.to_str()),
                Some("main.lua")
            );
        }
        other => panic!("expected InvalidImportSyntax from Core's export check, got {other:?}"),
    }
}

#[test]
fn alias_call_stops_in_the_old_lowerer() {
    let source = "local different = require(\"./present.rss\").tag\ndifferent()\n";
    assert_named(
        &static_imports(source)[0],
        "./present.rss",
        "tag",
        "different",
        1,
    );

    let root = owned_root("alias_call");
    write_present(&root);
    let main_path = write_source(&root, "main.lua", source);
    let err = expect_load_error(compile_candidate(&main_path), "alias call");
    assert_eq!(
        parse_message(&err),
        "unknown function 'different'",
        "old lowerer rejects the alias call before Core can bind it"
    );
    let production = expect_load_error(compile_production(&main_path), "production alias call");
    assert_eq!(parse_message(&production), "unknown function 'different'");
}

/// Equivalent Core consumer for `local different = require("./present.rss").tag`.
///
/// Not a Lua execution. Pinned Core `b1d6cffede77f49410bf63525f30b9a46b02dc01`
/// parses the alias in `src/compiler/parser/statements.rs:393-414` and binds
/// `NamedImport.local` in `src/compiler/source_loader/graph.rs:444-446`.
/// This test writes `alias_consumer.rss` next to `present.rss`.
#[test]
fn core_named_alias_consumer_runs_different_as_tag() {
    let root = owned_root("alias_consumer");
    write_present(&root);
    let source = "use self::present::{ tag as different };\ndifferent();\n";
    let consumer = write_source(&root, "alias_consumer.rss", source);
    assert_eq!(
        run_production(
            &consumer,
            "Core RSS consumer src/compiler/source_loader/graph.rs local binding"
        ),
        vec![Value::Int(41)]
    );
}

#[test]
fn ordinary_host_require_is_not_a_static_rss_link() {
    let source = "local runtime = require(\"runtime\")\nlocal json = require(\"json\")\n";
    assert!(
        static_imports(source).is_empty(),
        "host require stays off the file static graph"
    );
}

#[test]
fn extensionless_relative_spec_stays_written_and_runs() {
    let source = "local m = require(\"./present\")\nm.tag()\n";
    let imports = static_imports(source);
    assert_eq!(imports.len(), 1);
    assert_namespace(&imports[0], "./present", "m", 1);

    let root = owned_root("extensionless");
    write_present(&root);
    let main_path = write_source(&root, "main.lua", source);
    assert_eq!(
        run_candidate(&main_path, "extensionless relative RSS link"),
        vec![Value::Int(41)]
    );
    assert_eq!(
        run_production(&main_path, "production extensionless mapping"),
        vec![Value::Int(41)]
    );
}

#[test]
fn other_relative_extensionless_specs_keep_source_text() {
    let parent = static_imports("local m = require(\"../present\")\n");
    assert_namespace(&parent[0], "../present", "m", 1);
    let nested = static_imports("local m = require(\"./dir/present\")\n");
    assert_namespace(&nested[0], "./dir/present", "m", 1);
}

#[test]
fn short_forms_outside_the_relative_rss_rule_are_not_static_edges() {
    for source in [
        "local m = require(\"./present.lua\")\n",
        "local m = require(\"present\")\n",
        "local m = require(\"/present\")\n",
        "local m = require(\"./.present\")\n",
        "local m = require(\"./present.\")\n",
        "local m = require(\"./present.rss.lua\")\n",
    ] {
        assert!(
            static_imports(source).is_empty(),
            "not a static RSS link: {source:?}"
        );
    }
}

#[test]
fn explicit_lua_spec_is_not_a_candidate_static_edge() {
    let source = "local m = require(\"./present.lua\")\nm.tag()\n";
    assert!(static_imports(source).is_empty());

    let root = owned_root("explicit_lua");
    fs::write(root.join("present.lua"), "return 7\n").expect("lua sibling");
    let main_path = write_source(&root, "main.lua", source);
    let err = expect_load_error(compile_production(&main_path), "old scanner emits .lua");
    assert!(
        matches!(err, SourcePathError::NonRustScriptModule(_)),
        "old scanner emits explicit .lua and Core rejects it; the candidate must not copy that edge, got {err:?}"
    );
}

#[test]
fn incomplete_require_is_a_parser_error_not_a_module_read() {
    let source = "local m = require(\"./absent.rss\"\n";
    match imports::static_module_imports(source) {
        Err(SourcePathError::Source(SourceError::Parse(parse))) => {
            assert!(
                !parse.message.is_empty(),
                "parser should name the syntax error"
            );
        }
        other => panic!("expected a parser error, not a module read, got {other:?}"),
    }

    let root = owned_root("incomplete");
    let main_path = write_source(&root, "main.lua", source);
    let err = expect_load_error(compile_candidate(&main_path), "incomplete call");
    assert_eq!(
        parse_message(&err),
        "expected ')'",
        "old lowerer rejects the unclosed call; this is not a module read"
    );
}

#[test]
fn nested_called_require_is_the_old_contract_not_a_candidate_link() {
    // Production still executes this nested local. That is the old line-scanner
    // contract, not a license to scan nested requires into the candidate graph:
    // an uncalled nested require would become eager I/O. Before the production
    // switch this pair is a producer/consumer joint regression.
    let source = "local function use_tag()\n  local m = require(\"./present.rss\")\n  return m.tag()\nend\nuse_tag()\n";
    assert!(
        static_imports(source).is_empty(),
        "candidate graph must not lift the nested local"
    );
    assert_eq!(require_specs(source), vec!["./present.rss"]);

    let root = owned_root("nested_called");
    write_present(&root);
    let main_path = write_source(&root, "main.lua", source);
    assert_eq!(
        run_production(&main_path, "production nested called contract"),
        vec![Value::Int(41)]
    );
}

#[test]
fn nested_uncalled_require_is_eager_only_on_the_old_scanner() {
    let source =
        "local function unused()\n  local m = require(\"./absent.rss\")\n  return m.tag()\nend\n";
    assert!(static_imports(source).is_empty());
    assert_eq!(require_specs(source), vec!["./absent.rss"]);

    let root = owned_root("nested_absent");
    let main_path = write_source(&root, "main.lua", source);
    let production = expect_load_error(
        compile_production(&main_path),
        "old scanner eager-reads an uncalled nested require",
    );
    assert_not_found(&production, "old mis-scan");
}
