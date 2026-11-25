//! Candidate file entry for weak-value collection.
//!
//! Production `compile_options()` stays a separate plugin. These tests do not
//! lock its current result. They read frozen `.lua` bytes from disk through
//! `compile_source_file_with_options`, lower with the verified candidate, and
//! bind that same typed catalog's exact bodies. `HostFunctionRegistry::empty()`
//! is not a fallback.
//!
//! The function-wrapped pair (`file-entry-weak-collect.lua` and its omitted
//! twin) is copied verbatim and is not asserted here. The candidate lowerer
//! rejects `return rawlen(weak)` inside `local function` at
//! `candidate.rs` `unsupported`. That RED stays in the external receipt. These
//! tests do not freeze that refusal, and they do not rewrite the frozen source.

#[allow(dead_code)]
#[path = "../../src/frontends/lua/ast.rs"]
mod ast;
#[allow(dead_code)]
#[path = "../luajit_heap_runtime/candidate.rs"]
mod candidate;
#[allow(dead_code)]
#[path = "../../src/frontends/lua/lexer.rs"]
mod lexer;
#[allow(dead_code)]
#[path = "../../src/frontends/lua/parser.rs"]
mod parser;
#[allow(dead_code)]
#[path = "../../src/frontends/lua/resolve.rs"]
mod resolve;

use super::heap;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use vm::{
    CompileSourceFileOptions, FrontendImportSyntax, FrontendIr, HostFunctionRegistry, ParseError,
    SourceFlavor, SourcePlugin, Value, VmStatus,
};

struct CandidateFilePlugin;

impl SourcePlugin for CandidateFilePlugin {
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
        candidate::lower(source, catalog())
    }
}

fn catalog() -> &'static Arc<vm::HostApiCatalog> {
    static CATALOG: OnceLock<Arc<vm::HostApiCatalog>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut builder = vm::HostApiBuilder::new();
        candidate::install_schemas(&mut builder);
        Arc::new(builder.build().expect("candidate catalog"))
    })
}

fn candidate_options() -> CompileSourceFileOptions {
    CompileSourceFileOptions::new()
        .with_source_plugin(&CandidateFilePlugin)
        .with_host_api_catalog(Arc::clone(catalog()))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/luajit")
        .join(name)
}

fn import_names(program: &vm::Program) -> Vec<String> {
    program
        .imports
        .iter()
        .map(|import| import.name.clone())
        .collect()
}

fn compile_file(path: &Path) -> vm::CompiledProgram {
    vm::compile_source_file_with_options(path, candidate_options()).unwrap_or_else(|err| {
        panic!(
            "candidate file entry failed for {}: {err:?}",
            path.display()
        )
    })
}

fn assert_production_is_not_candidate(path: &Path) {
    if let Ok(compiled) =
        vm::compile_source_file_with_options(path, pd_vm_compat_frontends::compile_options())
    {
        let names = import_names(&compiled.program);
        assert!(
            !names.iter().any(|name| name == candidate::COLLECT),
            "production file entry emitted the candidate collect import: {names:?}"
        );
    }
}

fn assert_empty_registry_does_not_bind(program: &vm::Program) {
    let mut vm = vm::Vm::new(program.clone());
    let registry = HostFunctionRegistry::empty();
    let error = registry
        .bind_vm_cached(&mut vm)
        .expect_err("empty registry must not bind this program");
    assert!(
        matches!(error, vm::VmError::UnboundImport(_)),
        "empty registry did not fail closed: {error}"
    );
}

fn bind_exact(vm: &mut vm::Vm) {
    let mut registry = HostFunctionRegistry::empty();
    candidate::register(&mut registry, catalog()).expect("exact catalog register");
    registry.bind_vm_cached(vm).expect("exact catalog bind");
}

struct Arm {
    stack: Vec<Value>,
    imports: Vec<String>,
    collections: u32,
    live_nodes_after: u32,
    notes: Vec<candidate::CollectNote>,
    issued: usize,
}

fn run_bound(path: &Path) -> Arm {
    let compiled = compile_file(path);
    let imports = import_names(&compiled.program);
    assert_empty_registry_does_not_bind(&compiled.program);
    let mut vm = vm::Vm::new(compiled.program);
    bind_exact(&mut vm);
    vm.host_context()
        .scope_state_or_insert_with(candidate::ReturnWitness::default)
        .expect("install collect witness");
    let status = vm
        .run()
        .unwrap_or_else(|err| panic!("{}: vm run failed: {err}", path.display()));
    assert_eq!(status, VmStatus::Halted, "{}", path.display());
    let stack = vm.stack().to_vec();
    let witness = vm
        .host_context()
        .scope_state::<candidate::ReturnWitness>()
        .expect("collect witness")
        .clone();
    let stats = heap::stats(&mut vm.host_context()).expect("heap stats");
    Arm {
        stack,
        imports,
        collections: stats.collections,
        live_nodes_after: stats.live_nodes,
        notes: witness.collects,
        issued: witness.issued.len(),
    }
}

fn one_float(stack: &[Value]) -> f64 {
    match stack {
        [Value::Float(number)] => *number,
        other => panic!("expected one float return, got {other:?}"),
    }
}

#[test]
fn file_entry_weak_self_cycle_collects_orphan_and_omitted_keeps_both_slots() {
    let collect_path = fixture("file-entry-weak-self-cycle.lua");
    let omitted_path = fixture("file-entry-weak-self-cycle-omitted.lua");
    assert_production_is_not_candidate(&collect_path);
    assert_production_is_not_candidate(&omitted_path);

    let collected = run_bound(&collect_path);
    let omitted = run_bound(&omitted_path);

    assert!(
        collected
            .imports
            .iter()
            .any(|name| name == candidate::COLLECT),
        "collect file did not import {}: {:?}",
        candidate::COLLECT,
        collected.imports
    );
    assert!(
        !omitted
            .imports
            .iter()
            .any(|name| name == candidate::COLLECT),
        "omitted file imported collect: {:?}",
        omitted.imports
    );
    assert_eq!(one_float(&collected.stack), 1.0);
    assert_eq!(one_float(&omitted.stack), 2.0);
    assert_eq!(collected.collections, 1);
    assert_eq!(omitted.collections, 0);
    assert_eq!(collected.notes.len(), 1);
    assert!(omitted.notes.is_empty());

    let note = &collected.notes[0];
    assert_eq!(note.swept, 1);
    assert_eq!(note.live_nodes, 3);
    assert_eq!(note.marked_nodes, 3);
    assert_eq!(collected.live_nodes_after, 3);
    assert_eq!(omitted.live_nodes_after, 4);

    // Chunk locals do not emit `root_issued`. An empty issued list is not a
    // per-node identity, so this test does not claim a TableRef for the orphan.
    assert_eq!(collected.issued, 0);
    assert_eq!(omitted.issued, 0);
}

#[test]
fn file_entry_collect_body_absent_rejects_at_bind() {
    let path = fixture("file-entry-weak-self-cycle.lua");
    let compiled = compile_file(&path);
    assert!(
        import_names(&compiled.program)
            .iter()
            .any(|name| name == candidate::COLLECT),
        "program did not import collect"
    );
    let mut vm = vm::Vm::new(compiled.program);
    let mut registry = HostFunctionRegistry::empty();
    candidate::register_except(&mut registry, catalog(), Some(candidate::COLLECT))
        .expect("register without collect");
    let error = registry
        .bind_vm_cached(&mut vm)
        .expect_err("missing collect body must reject before guest run");
    match error {
        vm::VmError::UnboundImport(name) => assert_eq!(name, candidate::COLLECT),
        other => panic!("bind did not reject the missing collect body: {other}"),
    }
}
