#[allow(dead_code)]
#[path = "../common/mod.rs"]
mod common;
use common::*;
use std::fs;
use std::path::{Path, PathBuf};
use vm::{
    FrontendImportSyntax, FrontendIr, ImportClause, ModuleImport, ParseError, SourceFlavor,
    SourcePathError, SourcePlugin,
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
        pd_vm_compat_frontends::lua_static_module_imports(source)
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

fn static_imports(source: &str) -> Vec<ModuleImport> {
    pd_vm_compat_frontends::lua_static_module_imports(source).expect("static import scan")
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

fn assert_core_did_not_read_missing_module(err: &SourcePathError) {
    if let SourcePathError::Io(io) = err {
        panic!("Core attempted a module read ({io}); fake dependency entered the static graph");
    }
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

fn run_tag(path: &Path) -> Vec<Value> {
    let compiled = compile_candidate(path).expect("candidate hook should compile");
    let mut vm = Vm::new(compiled.program);
    let status = vm.run().expect("vm should run");
    assert_eq!(status, VmStatus::Halted);
    vm.stack().to_vec()
}

#[test]
fn candidate_runs_present_rss_and_ignores_string_require() {
    let root = owned_root("present_string_pair");
    write_present(&root);
    let main_path = root.join("main.lua");
    let source = "\
local quoted = \"require('./absent.rss')\"\n\
local m = require(\"./present.rss\")\n\
m.tag()\n";
    fs::write(&main_path, source).expect("lua source");

    let imports = static_imports(source);
    assert_eq!(specs(&imports), vec!["./present.rss"]);
    assert_namespace(&imports[0], "./present.rss", "m", 2);
    assert_eq!(run_tag(&main_path), vec![Value::Int(41)]);
}

#[test]
fn candidate_missing_rss_link_is_core_io_failure() {
    let root = owned_root("missing_rss");
    let main_path = root.join("main.lua");
    let source = "local m = require(\"./absent.rss\")\nm.tag()\n";
    fs::write(&main_path, source).expect("lua source");

    let imports = static_imports(source);
    assert_eq!(specs(&imports), vec!["./absent.rss"]);
    assert_namespace(&imports[0], "./absent.rss", "m", 1);

    let err = expect_load_error(compile_candidate(&main_path), "missing static link");
    match err {
        SourcePathError::Io(io) => {
            assert_eq!(
                io.kind(),
                std::io::ErrorKind::NotFound,
                "path mapping belongs to Core; the failure layer is the module read, got {io}"
            );
        }
        other => panic!("expected Core I/O, got {other}"),
    }
}

#[test]
fn candidate_does_not_load_long_comment_require() {
    let root = owned_root("long_comment");
    let main_path = root.join("main.lua");
    let source = "--[=[\nrequire(\"./absent.rss\")\n]=]\nlocal n = 41\n";
    fs::write(&main_path, source).expect("lua source");

    assert!(static_imports(source).is_empty());
    let err = expect_load_error(
        compile_candidate(&main_path),
        "long comment must not become a module read",
    );
    assert_core_did_not_read_missing_module(&err);
}

#[test]
fn candidate_does_not_load_require_inside_uncalled_function() {
    let root = owned_root("uncalled");
    write_present(&root);
    let main_path = root.join("main.lua");
    let source = "\
local function unused()\n\
  return require(\"./absent.rss\")\n\
end\n\
local m = require(\"./present.rss\")\n\
m.tag()\n";
    fs::write(&main_path, source).expect("lua source");

    let imports = static_imports(source);
    assert_eq!(specs(&imports), vec!["./present.rss"]);
    match compile_candidate(&main_path) {
        Ok(compiled) => {
            let mut vm = Vm::new(compiled.program);
            let status = vm.run().expect("vm should run");
            assert_eq!(status, VmStatus::Halted);
            assert_eq!(vm.stack(), &[Value::Int(41)]);
        }
        Err(err) => assert_core_did_not_read_missing_module(&err),
    }
}

#[test]
fn candidate_does_not_load_shadowed_require() {
    let root = owned_root("shadow");
    let main_path = root.join("main.lua");
    let source = "\
local function require(name)\n\
  return name\n\
end\n\
local value = require(\"./absent.rss\")\n\
local n = 41\n";
    fs::write(&main_path, source).expect("lua source");

    assert!(
        specs(&static_imports(source)).is_empty(),
        "shadowed require is a local call, not a static link"
    );
    match compile_candidate(&main_path) {
        Ok(compiled) => {
            let mut vm = Vm::new(compiled.program);
            let status = vm.run().expect("vm should run");
            assert_eq!(status, VmStatus::Halted);
        }
        Err(err) => assert_core_did_not_read_missing_module(&err),
    }
}

#[test]
fn candidate_does_not_load_require_alias() {
    let root = owned_root("alias");
    let main_path = root.join("main.lua");
    let source = "\
local r = require\n\
local value = r(\"./absent.rss\")\n\
local n = 41\n";
    fs::write(&main_path, source).expect("lua source");

    assert!(static_imports(source).is_empty());
    let err = expect_load_error(
        compile_candidate(&main_path),
        "alias call must not become a module read",
    );
    assert_core_did_not_read_missing_module(&err);
}

#[test]
fn candidate_does_not_load_ordinary_dynamic_require() {
    let root = owned_root("dynamic");
    write_present(&root);
    let main_path = root.join("main.lua");
    let source = "\
require(\"./absent.rss\")\n\
local m = require(\"./present.rss\")\n\
m.tag()\n";
    fs::write(&main_path, source).expect("lua source");

    let imports = static_imports(source);
    assert_eq!(specs(&imports), vec!["./present.rss"]);
    assert_eq!(run_tag(&main_path), vec![Value::Int(41)]);
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
fn candidate_named_missing_member_is_core_io_failure() {
    let root = owned_root("named_missing");
    let main_path = root.join("main.lua");
    let source = "local tag = require(\"./absent.rss\").tag\n";
    fs::write(&main_path, source).expect("lua source");

    let err = expect_load_error(compile_candidate(&main_path), "named static link");
    match err {
        SourcePathError::Io(io) => assert_eq!(io.kind(), std::io::ErrorKind::NotFound),
        other => panic!("expected Core I/O for the named link, got {other}"),
    }
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
fn incomplete_require_is_not_loaded_as_a_static_link() {
    let root = owned_root("incomplete");
    let main_path = root.join("main.lua");
    let source = "local m = require(\"./absent.rss\"\n";
    fs::write(&main_path, source).expect("lua source");

    assert!(pd_vm_compat_frontends::lua_static_module_imports(source).is_err());
    let err = expect_load_error(compile_candidate(&main_path), "incomplete call");
    assert_core_did_not_read_missing_module(&err);
}
