//! Candidate file entry for weak-value collection.
//!
//! Production `compile_options()` stays a separate plugin. These tests do not
//! lock its current result. They read frozen `.lua` bytes from disk through
//! `compile_source_file_with_options`, lower with the verified candidate, and
//! bind that same typed catalog's exact bodies. `HostFunctionRegistry::empty()`
//! is not a fallback.
//!
//! The function-wrapped pair is the same frozen source. Its direct
//! `return rawlen(weak)` is the function-tail slice: one lexically unbound
//! builtin call, packed as one value. A name stored first and then returned
//! is the existing value path, not this slice. A lexically bound `rawlen`
//! is not the builtin and is not executed as a general callable.

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
    issued_alive_at_collect: Vec<bool>,
    callee_left: bool,
    frame_after: Option<(u32, u32)>,
    steps: Vec<candidate::HostStep>,
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
    let frame_after = heap::with_heap(&mut vm.host_context(), |heap| {
        Ok(heap.current_frame().map(|frame| frame.parts()))
    })
    .expect("frame after run");
    Arm {
        stack,
        imports,
        collections: stats.collections,
        live_nodes_after: stats.live_nodes,
        notes: witness.collects,
        issued: witness.issued.len(),
        issued_alive_at_collect: witness.issued_alive_at_collect,
        callee_left: witness.callee_left,
        frame_after,
        steps: witness.host_steps,
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

fn derived_lua(label: &str, source: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "lua52-file-return-rawlen-{label}-{}.lua",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write derived lua in the process temp dir");
    path
}

fn with_direct_rawlen_replaced(fixture_name: &str, replacement: &str) -> String {
    let frozen = std::fs::read_to_string(fixture(fixture_name)).expect("read frozen lua");
    let derived = frozen.replacen("  return rawlen(weak)\n", replacement, 1);
    assert_ne!(
        derived, frozen,
        "{fixture_name} did not contain the direct rawlen return"
    );
    derived
}

fn rawlen_import_count(imports: &[String]) -> usize {
    imports
        .iter()
        .filter(|name| name.as_str() == candidate::RAW_LEN)
        .count()
}

fn assert_one_rawlen_while_callee_then_retire(arm: &Arm) {
    assert_eq!(rawlen_import_count(&arm.imports), 1);
    let rawlen_at = arm
        .steps
        .iter()
        .position(|step| matches!(step, candidate::HostStep::RawLen { .. }))
        .expect("RAW_LEN did not run");
    let finish_at = arm
        .steps
        .iter()
        .position(|step| matches!(step, candidate::HostStep::FinishReturn { .. }))
        .expect("FINISH_RETURN did not run");
    let retire_at = arm
        .steps
        .iter()
        .position(|step| matches!(step, candidate::HostStep::RetireCallee { .. }))
        .expect("RETIRE_CALLEE did not run");
    assert!(
        rawlen_at < finish_at && finish_at < retire_at,
        "host order was not RAW_LEN then FINISH_RETURN then RETIRE_CALLEE: {:?}",
        arm.steps
    );
    assert_eq!(
        arm.steps
            .iter()
            .filter(|step| matches!(step, candidate::HostStep::RawLen { .. }))
            .count(),
        1,
        "RAW_LEN ran more than once: {:?}",
        arm.steps
    );
    let candidate::HostStep::RawLen { frame: Some(frame) } = arm.steps[rawlen_at] else {
        panic!(
            "RAW_LEN ran after the callee frame had left: {:?}",
            arm.steps
        );
    };
    match arm.steps[finish_at] {
        candidate::HostStep::FinishReturn { frame_alive: true } => {}
        other => panic!("FINISH_RETURN saw no live callee frame: {other:?}"),
    }
    match arm.steps[retire_at] {
        candidate::HostStep::RetireCallee {
            left: true,
            retired: Some(retired),
            frame_after: None,
        } => assert_eq!(retired, frame, "retired a different frame than RAW_LEN saw"),
        other => panic!("callee frame was not retired: {other:?}"),
    }
    assert!(arm.callee_left, "witness did not record callee retirement");
    assert_eq!(
        arm.frame_after, None,
        "root frame still current after the arm"
    );
}

/// Original file pair. The parent lowerer stops at the function-tail call
/// before Core compile or bind. A passing run returns one float, collects
/// only the collect arm, and leaves the callee frame retired.
#[test]
fn file_entry_function_rawlen_collects_weak_tail_and_omitted_keeps_both() {
    let collect_path = fixture("file-entry-weak-collect.lua");
    let omitted_path = fixture("file-entry-weak-collect-omitted.lua");
    assert_production_is_not_candidate(&collect_path);
    assert_production_is_not_candidate(&omitted_path);

    let collected = run_bound(&collect_path);
    let omitted = run_bound(&omitted_path);

    assert_eq!(one_float(&collected.stack), 1.0);
    assert_eq!(one_float(&omitted.stack), 2.0);
    assert_eq!(rawlen_import_count(&collected.imports), 1);
    assert_eq!(rawlen_import_count(&omitted.imports), 1);
    assert!(
        collected
            .imports
            .iter()
            .any(|name| name == candidate::COLLECT)
    );
    assert!(
        !omitted
            .imports
            .iter()
            .any(|name| name == candidate::COLLECT),
        "omitted arm imported collect"
    );

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
    assert_eq!(collected.issued_alive_at_collect, vec![true]);

    // Function locals are rooted. The weak-only anonymous tail is not one of
    // those roots; the length and the swept node are its live/dead evidence.
    assert_eq!(collected.issued, 3);
    assert_eq!(omitted.issued, 3);
    assert_one_rawlen_while_callee_then_retire(&collected);
    assert_one_rawlen_while_callee_then_retire(&omitted);
}

/// Same frozen bodies, except the length is stored and the name is returned.
/// That path already lowers. It is not the direct-return RED.
#[test]
fn file_entry_function_rawlen_local_store_returns_same_lengths() {
    let collect_path = derived_lua(
        "local-store-collect",
        &with_direct_rawlen_replaced(
            "file-entry-weak-collect.lua",
            "  local measured = rawlen(weak)\n  return measured\n",
        ),
    );
    let omitted_path = derived_lua(
        "local-store-omitted",
        &with_direct_rawlen_replaced(
            "file-entry-weak-collect-omitted.lua",
            "  local measured = rawlen(weak)\n  return measured\n",
        ),
    );
    let collected = run_bound(&collect_path);
    let omitted = run_bound(&omitted_path);
    let _ = std::fs::remove_file(&collect_path);
    let _ = std::fs::remove_file(&omitted_path);

    assert_eq!(one_float(&collected.stack), 1.0);
    assert_eq!(one_float(&omitted.stack), 2.0);
    assert_eq!(collected.collections, 1);
    assert_eq!(omitted.collections, 0);
    assert_eq!(collected.notes.len(), 1);
    assert!(omitted.notes.is_empty());
    assert_one_rawlen_while_callee_then_retire(&collected);
    assert_one_rawlen_while_callee_then_retire(&omitted);
}

/// Parentheses around the whole call are still the one builtin, not a new
/// return expression.
#[test]
fn file_entry_parenthesized_rawlen_return_is_one_float() {
    let collect_path = derived_lua(
        "paren-collect",
        &with_direct_rawlen_replaced("file-entry-weak-collect.lua", "  return (rawlen(weak))\n"),
    );
    let omitted_path = derived_lua(
        "paren-omitted",
        &with_direct_rawlen_replaced(
            "file-entry-weak-collect-omitted.lua",
            "  return (rawlen(weak))\n",
        ),
    );
    let collected = run_bound(&collect_path);
    let omitted = run_bound(&omitted_path);
    let _ = std::fs::remove_file(&collect_path);
    let _ = std::fs::remove_file(&omitted_path);

    assert_eq!(one_float(&collected.stack), 1.0);
    assert_eq!(one_float(&omitted.stack), 2.0);
    assert_eq!(collected.collections, 1);
    assert_eq!(omitted.collections, 0);
    assert_one_rawlen_while_callee_then_retire(&collected);
    assert_one_rawlen_while_callee_then_retire(&omitted);
}

/// A lexical binding hides the builtin. This slice refuses that return. It
/// does not run the bound name as a general callable.
#[test]
fn file_entry_shadowed_rawlen_return_does_not_call_builtin() {
    let path = derived_lua(
        "shadowed",
        "local function sample()\n  local weak = {}\n  local rawlen = weak\n  return rawlen(weak)\nend\nreturn sample()\n",
    );
    let error = match vm::compile_source_file_with_options(&path, candidate_options()) {
        Ok(_) => panic!("bound rawlen compiled as the builtin"),
        Err(err) => format!("{err:?}"),
    };
    let _ = std::fs::remove_file(&path);
    assert!(
        error.contains("Lua expression not lowered yet"),
        "bound rawlen did not use the existing unsupported branch: {error}"
    );
    assert!(
        error.contains("line: 4"),
        "refusal was not the return of the bound name: {error}"
    );
    assert!(
        !error.contains(candidate::RAW_LEN),
        "bound rawlen still selected the builtin import: {error}"
    );
}
