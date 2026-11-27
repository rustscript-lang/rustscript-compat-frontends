//! B3 lazy numeric generators through existing typed closures/continuations.
//!
//! Reference-array (explicit TypeScript) mode only: `function*` constructors
//! lower to an ordinary typed callable returning a private iterator record
//! whose zero-argument `next` closure runs source work to the next yield or
//! return over persistent capture cells. Ordinary compilation keeps its
//! current generator refusal. No new Core API, schema, IR, wire or ABI.

use std::sync::Arc;

use vm::{CompileSourceFileOptions, HostApiCatalog, HostFunctionRegistry, Value, Vm, VmStatus};

fn ref_catalog() -> Arc<HostApiCatalog> {
    pd_vm_compat_frontends::reference_array_effective_catalog()
}

fn ref_options(catalog: &Arc<HostApiCatalog>) -> CompileSourceFileOptions {
    CompileSourceFileOptions::new()
        .with_source_plugin(&pd_vm_compat_frontends::TYPESCRIPT_REFERENCE_ARRAY_PLUGIN)
        .with_source_plugin(&pd_vm_compat_frontends::LUA_PLUGIN)
        .with_host_api_catalog(Arc::clone(catalog))
}

fn install_ref_production(registry: &mut HostFunctionRegistry, catalog: &HostApiCatalog) {
    pd_vm_compat_frontends::runtime::array::typescript_array_host_module()
        .install_from_catalog(registry, catalog)
        .expect("manual arrays adapters should match the effective catalog");
    pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module()
        .install_from_catalog(registry, catalog)
        .expect("owned adapters should match the effective catalog");
    pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module()
        .install_from_catalog(registry, catalog)
        .expect("snapshot adapter should match the effective catalog");
    pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module()
        .install_from_catalog(registry, catalog)
        .expect("scalar-math adapter should match the effective catalog");
    registry
        .allow_builtin("math::floor")
        .expect("math::floor should be allow-listable");
}

fn ref_vm(source: &str, native: bool) -> Vm {
    let catalog = ref_catalog();
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        pd_vm_compat_frontends::typescript_flavor(),
        ref_options(&catalog),
    )
    .expect("reference-array compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = HostFunctionRegistry::restricted();
    install_ref_production(&mut registry, &catalog);
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    if native {
        machine
            .compile_aot()
            .expect("native compilation should succeed");
    }
    machine
}

fn run_value(machine: &mut Vm) -> Value {
    assert_eq!(
        machine.run().expect("generator run should finish"),
        VmStatus::Halted,
        "generator program should halt"
    );
    machine
        .stack()
        .last()
        .cloned()
        .expect("program should leave a value")
}

/// Effective catalog plus the production `throw new Error` adapter, for
/// sources that validate inputs by throwing. The reference-array lowering
/// needs are unchanged; the error module only authorizes the exact
/// `typescript::throw_error` entry, mirroring the factorial proof pattern.
fn ref_error_catalog() -> Arc<HostApiCatalog> {
    let base = pd_vm_compat_frontends::reference_array_effective_catalog();
    let error_module =
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module();
    let error_catalog = error_module
        .catalog()
        .expect("error module catalog should build");
    let mut builder = vm::HostApiCatalog::builder();
    for resource in base.resources() {
        builder.resource(resource.clone());
    }
    for function in base.functions().iter().chain(error_catalog.functions()) {
        builder.function(function.clone());
    }
    Arc::new(builder.build().expect("error catalog should build"))
}

fn ref_error_vm(source: &str, native: bool) -> Vm {
    let catalog = ref_error_catalog();
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&pd_vm_compat_frontends::TYPESCRIPT_REFERENCE_ARRAY_PLUGIN)
        .with_source_plugin(&pd_vm_compat_frontends::LUA_PLUGIN)
        .with_host_api_catalog(Arc::clone(&catalog));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        pd_vm_compat_frontends::typescript_flavor(),
        options,
    )
    .expect("reference-array compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = HostFunctionRegistry::restricted();
    install_ref_production(&mut registry, &catalog);
    pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("error adapter should match the catalog");
    // The upstream formula/validation surface needs the exact Core numeric
    // builtins it lowers to; data builtins stay governed by the restricted
    // profile as before.
    for builtin in [
        "math::sqrt",
        "math::round",
        "math::powf",
        "math::is_nan",
        "math::is_finite",
    ] {
        registry
            .allow_builtin(builtin)
            .unwrap_or_else(|error| panic!("{builtin} should be allow-listable: {error}"));
    }
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    if native {
        machine
            .compile_aot()
            .expect("native compilation should succeed");
    }
    machine
}

/// Smallest complete continuation slice: lazy body, two yields, shared alias
/// state and independent factory instances.
#[test]
fn generator_first_next_is_lazy_and_alias_shares_state() {
    let source = r#"
function* two(a: number): Generator<number> {
  yield a;
  yield a + 1;
}
const it = two(10);
const alias = it;
const r1 = alias.next();
const other = two(20);
const ro = other.next();
const r2 = it.next();
[r1.value, r1.done, ro.value, r2.value, r2.done];
"#;
    let mut machine = ref_vm(source, false);
    // `number`-annotated parameters cross the call boundary through the
    // existing floating projection, so yields over them arrive as Float;
    // alias sharing and instance independence hold either way.
    assert_eq!(
        run_value(&mut machine),
        Value::Array(
            vec![
                Value::Float(10.0),
                Value::Bool(false),
                Value::Float(20.0),
                Value::Float(11.0),
                Value::Bool(false),
            ]
            .into()
        )
    );
}

/// Default-at-creation versus body-at-first-next: a test-only host clock
/// observes exactly when each phase runs, mirroring the Node oracle
/// lifecycle. Guest code cannot count evaluations (deterministic, no shared
/// mutable state is visible inside bodies), so the counter lives outside
/// the VM; defaults and bodies only *call* it, never capture ambient
/// state. Runs under the effective catalog plus this observation-only
/// module; every other test keeps the exact effective catalog.
mod gen_clock {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};

    use pd_host_function::pd_host_function;
    use vm::{HostApiCatalog, Value, VmResult};

    static TICKS: AtomicI64 = AtomicI64::new(0);

    fn tick_schema(name: &'static str) -> vm::HostFunctionSchema {
        vm::HostFunctionSchema::with_return(name, Vec::new(), vm::HostTypeSchema::Int)
    }

    fn read_schema() -> vm::HostFunctionSchema {
        tick_schema("gen_clock::read")
    }

    fn default_schema() -> vm::HostFunctionSchema {
        tick_schema("gen_clock::tick_default")
    }

    fn body_schema() -> vm::HostFunctionSchema {
        tick_schema("gen_clock::tick_body")
    }

    fn resume_schema() -> vm::HostFunctionSchema {
        tick_schema("gen_clock::tick_resume")
    }

    fn reset_schema() -> vm::HostFunctionSchema {
        tick_schema("gen_clock::reset")
    }

    mod generated {
        use super::*;

        #[allow(unused_imports)]
        pub use vm::{arg, borrow_arg, take_arg};

        pub mod gen_clock_host {
            use super::*;

            /// Observation tick for default evaluation.
            #[pd_host_function(name = "gen_clock::tick_default", contract = super::super::default_schema)]
            fn tick_default() -> VmResult<Value> {
                Ok(Value::Int(
                    super::super::TICKS.fetch_add(1, Ordering::SeqCst) + 1,
                ))
            }

            /// Observation tick for first-next body entry.
            #[pd_host_function(name = "gen_clock::tick_body", contract = super::super::body_schema)]
            fn tick_body() -> VmResult<Value> {
                Ok(Value::Int(
                    super::super::TICKS.fetch_add(1, Ordering::SeqCst) + 1,
                ))
            }

            /// Observation tick for resumption past the first yield.
            #[pd_host_function(name = "gen_clock::tick_resume", contract = super::super::resume_schema)]
            fn tick_resume() -> VmResult<Value> {
                Ok(Value::Int(
                    super::super::TICKS.fetch_add(1, Ordering::SeqCst) + 1,
                ))
            }

            /// Pure clock read (no tick).
            #[pd_host_function(name = "gen_clock::read", contract = super::super::read_schema)]
            fn read() -> VmResult<Value> {
                Ok(Value::Int(super::super::TICKS.load(Ordering::SeqCst)))
            }

            /// Test isolation reset.
            #[pd_host_function(name = "gen_clock::reset", contract = super::super::reset_schema)]
            fn reset() -> VmResult<Value> {
                super::super::TICKS.store(0, Ordering::SeqCst);
                Ok(Value::Int(0))
            }
        }
    }

    pub fn module() -> vm::HostModuleDescriptor {
        const FNS: &[fn() -> vm::HostFunctionDescriptor] = &[
            generated::gen_clock_host::tick_default_descriptor,
            generated::gen_clock_host::tick_body_descriptor,
            generated::gen_clock_host::tick_resume_descriptor,
            generated::gen_clock_host::read_descriptor,
            generated::gen_clock_host::reset_descriptor,
        ];
        vm::HostModuleDescriptor {
            name: "gen_clock",
            functions: FNS,
            resources: &[],
        }
    }

    pub fn catalog() -> Arc<HostApiCatalog> {
        let base = pd_vm_compat_frontends::reference_array_effective_catalog();
        let clock = module().catalog().expect("clock catalog should build");
        let mut builder = vm::HostApiCatalog::builder();
        for resource in base.resources() {
            builder.resource(resource.clone());
        }
        for function in base.functions().iter().chain(clock.functions()) {
            builder.function(function.clone());
        }
        Arc::new(
            builder
                .build()
                .expect("extended clock catalog should build"),
        )
    }

    static CLOCK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub fn lock() -> std::sync::MutexGuard<'static, ()> {
        CLOCK_LOCK
            .lock()
            .expect("clock lock should not be poisoned")
    }
}

fn clock_vm(source: &str, native: bool) -> Vm {
    let catalog = gen_clock::catalog();
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&pd_vm_compat_frontends::TYPESCRIPT_REFERENCE_ARRAY_PLUGIN)
        .with_source_plugin(&pd_vm_compat_frontends::LUA_PLUGIN)
        .with_host_api_catalog(Arc::clone(&catalog));
    let compilation = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        pd_vm_compat_frontends::typescript_flavor(),
        options,
    )
    .expect("clock compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = HostFunctionRegistry::restricted();
    install_ref_production(&mut registry, &catalog);
    gen_clock::module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("clock adapters should match the extended catalog");
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    if native {
        machine
            .compile_aot()
            .expect("native compilation should succeed");
    }
    machine
}

/// Default-at-creation versus body-at-first-next: the host clock observes
/// exactly when each phase runs, mirroring the Node oracle lifecycle.
#[test]
fn generator_default_at_create_body_at_first_next() {
    let _guard = gen_clock::lock();
    // Host member names are snake_case; guest aliases keep camelCase.
    let source = r#"
import { tick_default as tickDefault, tick_body as tickBody, tick_resume as tickResume, read as readClock, reset as resetClock } from "gen_clock";
resetClock();
function* fixture(a: number = tickDefault()): Generator<number, number> {
  tickBody();
  yield a;
  tickResume();
  return a + 1;
}
const it = fixture();
const afterCreate = readClock();
const r1 = it.next();
const afterFirst = readClock();
const r2 = it.next();
const afterSecond = readClock();
const r3 = it.next();
const afterThird = readClock();
[afterCreate, r1.value, r1.done, afterFirst, r2.value, r2.done, afterSecond, r3.done, afterThird];
"#;
    let mut machine = clock_vm(source, false);
    assert_eq!(
        run_value(&mut machine),
        Value::Array(
            vec![
                // default ticked exactly once at creation; body/resume silent.
                Value::Int(1),
                // first yield carries the default's value (projected Float).
                Value::Float(1.0),
                Value::Bool(false),
                // body ticked once on first next; resume silent.
                Value::Int(2),
                Value::Float(2.0),
                Value::Bool(true),
                // resume ticked exactly once on second next.
                Value::Int(3),
                Value::Bool(true),
                // third next ticks nothing; done without a value.
                Value::Int(3),
            ]
            .into()
        )
    );
}

/// Finite completion: the return value arrives once with done, later done
/// calls carry an honest missing value (read as Null, never a numeric
/// stand-in).
#[test]
fn generator_finite_return_then_missing_value() {
    let source = r#"
function* finite(): Generator<number, number> {
  yield 1;
  yield 2;
  return 3;
}
const it = finite();
const r1 = it.next();
const r2 = it.next();
const r3 = it.next();
const r4 = it.next();
[r1.value, r1.done, r2.value, r2.done, r3.value, r3.done, r4.value, r4.done];
"#;
    let mut machine = ref_vm(source, false);
    assert_eq!(
        run_value(&mut machine),
        Value::Array(
            vec![
                Value::Int(1),
                Value::Bool(false),
                Value::Int(2),
                Value::Bool(false),
                Value::Int(3),
                Value::Bool(true),
                Value::Null,
                Value::Bool(true),
            ]
            .into()
        )
    );
}

/// Canonical while(true) yield-before-update loop over persistent body
/// locals, stepped several times through one iterator.
#[test]
fn generator_while_true_yield_loop_steps() {
    let source = r#"
function* counter(): Generator<number> {
  let a = 0;
  let b = 1;
  while (true) {
    yield a;
    const c = a + b;
    a = b;
    b = c;
  }
}
const it = counter();
const r0 = it.next();
const r1 = it.next();
const r2 = it.next();
const r3 = it.next();
const r4 = it.next();
const r5 = it.next();
[r0.value, r1.value, r2.value, r3.value, r4.value, r5.value, r5.done];
"#;
    let mut machine = ref_vm(source, false);
    assert_eq!(
        run_value(&mut machine),
        Value::Array(
            vec![
                Value::Int(0),
                Value::Int(1),
                Value::Int(1),
                Value::Int(2),
                Value::Int(3),
                Value::Int(5),
                Value::Bool(false),
            ]
            .into()
        )
    );
}

/// General control flow: if/else branches, bounded while, counted for with
/// break/continue, and lexical shadowing with initialization points.
#[test]
fn generator_branches_loops_and_shadowing() {
    let source = r#"
function* flow(n: number): Generator<number, number> {
  let total: number = 0;
  if (n > 0) {
    total = 10;
  } else {
    total = -10;
  }
  yield total;
  let i: number = 0;
  while (i < 3) {
    i = i + 1;
    if (i === 2) {
      continue;
    }
    yield i * 100;
  }
  for (let k: number = 0; k < 5; k = k + 1) {
    if (k === 3) {
      break;
    }
    const shadow: number = k + 1;
    {
      const doubled: number = shadow * 10;
      yield doubled;
    }
  }
  return total + i;
}
const it = flow(7);
const a = it.next();
const b = it.next();
const c = it.next();
const d = it.next();
const e = it.next();
const f = it.next();
const g = it.next();
[a.value, a.done, b.value, c.value, d.value, e.value, f.value, g.value, g.done];
"#;
    let mut machine = ref_vm(source, false);
    // Direct value asserts with deterministic representations: plain Int
    // literals stay Int; arithmetic over `number` bindings runs through the
    // existing floating projection. Sequence proves branch selection (10),
    // continue-skip (100, 300, no 200), shadowed for-yields (10, 20, 30),
    // break at k=3, and return total + i = 10 + 3 with done.
    assert_eq!(
        run_value(&mut machine),
        Value::Array(
            vec![
                Value::Int(10),
                Value::Bool(false),
                Value::Float(100.0),
                Value::Float(300.0),
                Value::Float(10.0),
                Value::Float(20.0),
                Value::Float(30.0),
                Value::Float(13.0),
                Value::Bool(true),
            ]
            .into()
        )
    );
}

const FIBONACCI_FIXTURE: &str = include_str!("fixtures/typescript/fibonacci.ts");

fn fib_file_vm(main: &std::path::Path, native: bool) -> (Vm, Arc<HostApiCatalog>) {
    let catalog = ref_error_catalog();
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&pd_vm_compat_frontends::TYPESCRIPT_REFERENCE_ARRAY_PLUGIN)
        .with_source_plugin(&pd_vm_compat_frontends::LUA_PLUGIN)
        .with_host_api_catalog(Arc::clone(&catalog));
    let compilation = vm::compile_source_file_with_capture_certification_and_options(main, options)
        .expect("fibonacci file compilation should succeed");
    let mut machine = compilation
        .into_vm()
        .expect("certified compilation should create a source-certified VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("explicit owned-array initialization should succeed");
    let mut registry = HostFunctionRegistry::restricted();
    install_ref_production(&mut registry, &catalog);
    pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("error adapter should match the catalog");
    for builtin in [
        "math::sqrt",
        "math::round",
        "math::powf",
        "math::is_nan",
        "math::is_finite",
    ] {
        registry
            .allow_builtin(builtin)
            .unwrap_or_else(|error| panic!("{builtin} should be allow-listable: {error}"));
    }
    registry
        .bind_vm_cached(&mut machine)
        .expect("production adapters should bind to this VM");
    if native {
        machine
            .compile_aot()
            .expect("native compilation should succeed");
    }
    (machine, catalog)
}

mod fib_project {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub struct Project(PathBuf);

    impl Project {
        pub fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let base = std::env::var_os("TMPDIR")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir);
            loop {
                let id = NEXT.fetch_add(1, Ordering::Relaxed);
                let path = base.join(format!("typescript-gen-fib-{}-{id}", std::process::id()));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create wrapper project {}: {error}", path.display()),
                }
            }
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// A distinct entry importing all three public exports from the unchanged
/// module file (byte-identical copy): the import path executes with the
/// same agreement as the inline source path.
#[test]
fn fibonacci_imported_exports_agree_interpreted() {
    let project = fib_project::Project::new();
    let fixture_path = project.path().join("fibonacci.ts");
    std::fs::write(&fixture_path, FIBONACCI_FIXTURE.as_bytes()).expect("fixture copy should write");
    assert_eq!(
        std::fs::read(&fixture_path).expect("fixture copy should read"),
        FIBONACCI_FIXTURE.as_bytes(),
        "wrapper must import a byte-identical fixture"
    );
    let main = project.path().join("main.ts");
    std::fs::write(
        &main,
        "import { nthFibonacci as fib, nthFibonacciRecursively as rec, nthFibonacciUsingFormula as fom } from \"./fibonacci.ts\";\nconst a10 = fib(10) === 55 && rec(10) === 55 && fom(10) === 55;\nconst a20 = fib(20) === 6765 && rec(20) === 6765 && fom(20) === 6765;\nconst cross = fib(10) === rec(10) && rec(10) === fom(10);\n[a10, a20, cross];\n",
    )
    .expect("entry should write");
    let (mut machine, _) = fib_file_vm(&main, false);
    assert_eq!(
        run_value(&mut machine),
        Value::Array(vec![Value::Bool(true), Value::Bool(true), Value::Bool(true)].into())
    );
}

/// The unchanged upstream module executes through the source path: all
/// three public exports agree on every valid vector (numeric `===` bridges
/// the iterative-Int versus recursive/formula-Float representations).
#[test]
fn fibonacci_all_exports_agree_on_valid_vectors_interpreted() {
    let source = format!(
        "{FIBONACCI_FIXTURE}\nconst a0 = nthFibonacci(0) === 0 && nthFibonacciRecursively(0) === 0 && nthFibonacciUsingFormula(0) === 0;\nconst a1 = nthFibonacci(1) === 1 && nthFibonacciRecursively(1) === 1 && nthFibonacciUsingFormula(1) === 1;\nconst a2 = nthFibonacci(2) === 1 && nthFibonacciRecursively(2) === 1 && nthFibonacciUsingFormula(2) === 1;\nconst a10 = nthFibonacci(10) === 55 && nthFibonacciRecursively(10) === 55 && nthFibonacciUsingFormula(10) === 55;\nconst a20 = nthFibonacci(20) === 6765 && nthFibonacciRecursively(20) === 6765 && nthFibonacciUsingFormula(20) === 6765;\nconst cross0 = nthFibonacci(10) === nthFibonacciRecursively(10) && nthFibonacciRecursively(10) === nthFibonacciUsingFormula(10);\n[a0, a1, a2, a10, a20, cross0];\n"
    );
    let mut machine = ref_error_vm(&source, false);
    assert_eq!(
        run_value(&mut machine),
        Value::Array(
            vec![
                Value::Bool(true),
                Value::Bool(true),
                Value::Bool(true),
                Value::Bool(true),
                Value::Bool(true),
                Value::Bool(true),
            ]
            .into()
        )
    );
}

/// The primary export keeps its exact invalid-input errors. Each invalid
/// call runs in its own program (a throw aborts the VM), and the recorded
/// messages match the Node oracle verbatim.
#[test]
fn fibonacci_primary_keeps_exact_invalid_input_errors() {
    // NaN comes from Math.sqrt(-1) and infinities from float overflow
    // (division truncates and division by zero traps before the call); each
    // invalid call runs in its own program since a throw aborts the VM,
    // which also proves later effects are skipped.
    for (input, message) in [
        ("Math.sqrt(-1)", "The input needs to be a number"),
        ("-1", "The input needs to be a non-negative integer"),
        ("1.5", "The input needs to be a non-negative integer"),
        ("1e308 * 10", "The input needs to be a non-negative integer"),
        (
            "-1e308 * 10",
            "The input needs to be a non-negative integer",
        ),
    ] {
        let source = format!("{FIBONACCI_FIXTURE}\nnthFibonacci({input});\n");
        let mut machine = ref_error_vm(&source, false);
        let outcome = machine.run();
        assert!(
            matches!(&outcome, Err(vm::VmError::HostError(actual)) if actual == message),
            "input {input} should throw {message:?}, outcome was {outcome:?}"
        );
    }
}

/// Generator stepping executes natively: the canonical yield loop with
/// persistent state runs under source SSA lowering with a positive native
/// dispatch count. (Whole-module native coverage lives in the
/// `fibonacci_*_native` tests below; this proves the generator execution
/// itself over the same integer backend path.)
#[test]
fn generator_stepping_executes_natively_with_ssa_lowering() {
    let source = r#"
function* counter(): Generator<number> {
  let a = 0;
  let b = 1;
  while (true) {
    yield a;
    const c = a + b;
    a = b;
    b = c;
  }
}
const it = counter();
const r0 = it.next();
const r1 = it.next();
const r2 = it.next();
const r3 = it.next();
const v0: number = r0.value;
const v1: number = r1.value;
const v2: number = r2.value;
const v3: number = r3.value;
const ok = v0 === 0 ? (v1 === 1 ? (v2 === 1 ? (v3 === 2 ? 1 : 0) : 0) : 0) : 0;
ok;
"#;
    let mut machine = ref_vm(source, true);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(run_value(&mut machine), Value::Int(1));
    assert!(
        machine.aot_exec_count() > 1,
        "actual native execution must exceed one dispatch, got {}",
        machine.aot_exec_count()
    );
    assert!(
        machine.dump_aot_info().contains("lowering=ssa"),
        "native lowering must stay ssa"
    );
}

/// The formula export executes natively standalone: exponentiation, float
/// arithmetic and rounding under SSA with a positive native count.
#[test]
fn fibonacci_formula_executes_natively_standalone() {
    let source = r#"
const sqrt5 = Math.sqrt(5);
const phi = (1 + sqrt5) / 2;
const psi = (1 - sqrt5) / 2;
const fom = (n: number) => Math.round((phi ** n - psi ** n) / sqrt5);
const v: number = fom(10);
v === 55 ? 1 : 0;
"#;
    let mut machine = ref_error_vm(source, true);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(run_value(&mut machine), Value::Int(1));
    assert!(
        machine.aot_exec_count() > 1,
        "actual native execution must exceed one dispatch, got {}",
        machine.aot_exec_count()
    );
    assert!(
        machine.dump_aot_info().contains("lowering=ssa"),
        "native lowering must stay ssa"
    );
}

#[test]
fn b3_resume_original_fibonacci_all_exports_native() {
    let source = format!(
        "{FIBONACCI_FIXTURE}\nconst ok = nthFibonacci(10) === 55 && nthFibonacciRecursively(10) === 55 && nthFibonacciUsingFormula(10) === 55;\nok ? 1 : 0;\n"
    );
    let mut machine = ref_error_vm(&source, true);
    assert_eq!(run_value(&mut machine), Value::Int(1));
    assert!(machine.aot_exec_count() > 1);
    assert!(machine.dump_aot_info().contains("lowering=ssa"));
}

#[test]
fn b3_resume_recursive_numeric_add_native() {
    let source = "function g(n: number): number {\n  if (n === 0) {\n    return 0;\n  }\n  return g(n - 1) + 1;\n}\ng(2);\n";
    let mut machine = ref_vm(source, true);
    let result = run_value(&mut machine);
    match result {
        Value::Int(value) => assert_eq!(value, 2),
        Value::Float(value) => assert_eq!(value, 2.0),
        other => panic!("recursive numeric result must be numeric, got {other:?}"),
    }
    assert!(machine.aot_exec_count() > 1);
    assert!(machine.dump_aot_info().contains("lowering=ssa"));
}

/// The unchanged upstream module executes natively through the source path:
/// all three public exports agree on every valid vector (n = 0, 1, 2, 10,
/// 20) under source SSA lowering with a positive native dispatch count.
#[test]
fn fibonacci_all_vectors_execute_natively_inline() {
    let source = format!(
        "{FIBONACCI_FIXTURE}\nconst a0 = nthFibonacci(0) === 0 && nthFibonacciRecursively(0) === 0 && nthFibonacciUsingFormula(0) === 0;\nconst a1 = nthFibonacci(1) === 1 && nthFibonacciRecursively(1) === 1 && nthFibonacciUsingFormula(1) === 1;\nconst a2 = nthFibonacci(2) === 1 && nthFibonacciRecursively(2) === 1 && nthFibonacciUsingFormula(2) === 1;\nconst a10 = nthFibonacci(10) === 55 && nthFibonacciRecursively(10) === 55 && nthFibonacciUsingFormula(10) === 55;\nconst a20 = nthFibonacci(20) === 6765 && nthFibonacciRecursively(20) === 6765 && nthFibonacciUsingFormula(20) === 6765;\nconst cross0 = nthFibonacci(10) === nthFibonacciRecursively(10) && nthFibonacciRecursively(10) === nthFibonacciUsingFormula(10);\n[a0, a1, a2, a10, a20, cross0];\n"
    );
    let mut machine = ref_error_vm(&source, true);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(
        run_value(&mut machine),
        Value::Array(
            vec![
                Value::Bool(true),
                Value::Bool(true),
                Value::Bool(true),
                Value::Bool(true),
                Value::Bool(true),
                Value::Bool(true),
            ]
            .into()
        )
    );
    assert!(
        machine.aot_exec_count() > 1,
        "actual native execution must exceed one dispatch, got {}",
        machine.aot_exec_count()
    );
    assert!(
        machine.dump_aot_info().contains("lowering=ssa"),
        "native lowering must stay ssa"
    );
}

/// A distinct native entry importing all three public exports from the
/// unchanged module file (byte-identical copy): the import path executes
/// natively with the same agreement as the inline source path.
#[test]
fn fibonacci_imported_exports_agree_natively() {
    let project = fib_project::Project::new();
    let fixture_path = project.path().join("fibonacci.ts");
    std::fs::write(&fixture_path, FIBONACCI_FIXTURE.as_bytes()).expect("fixture copy should write");
    assert_eq!(
        std::fs::read(&fixture_path).expect("fixture copy should read"),
        FIBONACCI_FIXTURE.as_bytes(),
        "wrapper must import a byte-identical fixture"
    );
    let main = project.path().join("main.ts");
    std::fs::write(
        &main,
        "import { nthFibonacci as fib, nthFibonacciRecursively as rec, nthFibonacciUsingFormula as fom } from \"./fibonacci.ts\";\nconst a10 = fib(10) === 55 && rec(10) === 55 && fom(10) === 55;\nconst a20 = fib(20) === 6765 && rec(20) === 6765 && fom(20) === 6765;\nconst cross = fib(10) === rec(10) && rec(10) === fom(10);\n[a10, a20, cross];\n",
    )
    .expect("entry should write");
    let (mut machine, _) = fib_file_vm(&main, true);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(
        run_value(&mut machine),
        Value::Array(vec![Value::Bool(true), Value::Bool(true), Value::Bool(true)].into())
    );
    assert!(
        machine.aot_exec_count() > 1,
        "actual native execution must exceed one dispatch, got {}",
        machine.aot_exec_count()
    );
    assert!(
        machine.dump_aot_info().contains("lowering=ssa"),
        "native lowering must stay ssa"
    );
}

/// The primary export keeps its exact invalid-input errors on the native
/// path. One natively compiled module serves all five oracle inputs through
/// its exported callable; each invalid call throws the verbatim message and
/// native dispatches stay positive.
#[test]
fn fibonacci_primary_invalid_inputs_error_natively() {
    let source = format!("{FIBONACCI_FIXTURE}\n0;\n");
    let mut machine = ref_error_vm(&source, true);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(run_value(&mut machine), Value::Int(0));
    let primary = machine
        .resolve_exported_callable("nthFibonacci")
        .expect("primary export should resolve");
    for (input, message) in [
        (Value::Float(f64::NAN), "The input needs to be a number"),
        (
            Value::Int(-1),
            "The input needs to be a non-negative integer",
        ),
        (
            Value::Float(1.5),
            "The input needs to be a non-negative integer",
        ),
        (
            Value::Float(f64::INFINITY),
            "The input needs to be a non-negative integer",
        ),
        (
            Value::Float(f64::NEG_INFINITY),
            "The input needs to be a non-negative integer",
        ),
    ] {
        let outcome = machine.invoke_callable(primary.clone(), &[input]);
        assert!(
            matches!(&outcome, Err(vm::VmError::HostError(actual)) if actual == message),
            "invalid input should throw {message:?}, outcome was {outcome:?}"
        );
    }
    assert!(
        machine.aot_exec_count() > 1,
        "actual native execution must exceed one dispatch, got {}",
        machine.aot_exec_count()
    );
    assert!(
        machine.dump_aot_info().contains("lowering=ssa"),
        "native lowering must stay ssa"
    );
}

/// Hundreds of dead factories stay bounded: 400 iterators are constructed
/// and stepped once each through a single reused slot (proving each ran via
/// the running total), while one live iterator plus its alias survives the
/// churn and steps exactly afterwards. Identity metadata stays at the same
/// small constants as the 30-factory case, certifying history independence.
#[test]
fn generator_many_dead_factories_stay_bounded() {
    let source = r#"
function* counter(start: number): Generator<number> {
  let a = start;
  while (true) {
    yield a;
    a = a + 1;
  }
}
const live = counter(1000);
const liveAlias = live;
const first = liveAlias.next();
const fv: number = first.value;
let total: number = 0;
for (let k = 0; k < 400; ++k) {
  const it = counter(k);
  const r = it.next();
  const v: number = r.value;
  total = total + v;
}
const second = live.next();
const sv: number = second.value;
const okTotal = total === 79800 ? 1 : 0;
const okFirst = fv === 1000 ? 1 : 0;
const okSecond = sv === 1001 ? 1 : 0;
[okTotal, okFirst, okSecond];
"#;
    let mut machine = ref_vm(source, false);
    machine.set_callable_identity_maintenance(Some(
        vm::CallableIdentityMaintenanceConfig::new(4).expect("budget 4 is valid"),
    ));
    // 0 + 1 + ... + 399 = 79800; the live iterator yields 1000 then 1001.
    assert_eq!(
        run_value(&mut machine),
        Value::Array(vec![Value::Int(1), Value::Int(1), Value::Int(1)].into())
    );
    let identity = machine
        .callable_identity_maintenance_stats()
        .expect("identity maintenance was enabled");
    assert!(
        identity.entries <= 32 && identity.capacity <= 64,
        "live identity metadata must stay bounded across 400 dead factories, got {identity:?}"
    );
}

/// A long single history stays bounded: 500 nexts on one live iterator sum
/// exactly, metadata stays at the same small constants (no per-yield
/// history), and a VM reset replays the same history identically.
#[test]
fn generator_long_history_stays_bounded() {
    let source = r#"
function* counter(): Generator<number> {
  let a = 0;
  while (true) {
    yield a;
    a = a + 1;
  }
}
const it = counter();
let acc: number = 0;
for (let k = 0; k < 500; ++k) {
  acc = acc + it.next().value;
}
acc;
"#;
    let mut machine = ref_vm(source, false);
    machine.set_callable_identity_maintenance(Some(
        vm::CallableIdentityMaintenanceConfig::new(4).expect("budget 4 is valid"),
    ));
    // 0 + 1 + ... + 499 = 124750 (Float through the accumulator's
    // floating projection).
    assert_eq!(run_value(&mut machine), Value::Float(124750.0));
    let identity = machine
        .callable_identity_maintenance_stats()
        .expect("identity maintenance was enabled");
    assert!(
        identity.entries <= 32 && identity.capacity <= 64,
        "live identity metadata must stay bounded across a 500-next history, got {identity:?}"
    );
    machine.reset_for_reuse().expect("reset should succeed");
    assert_eq!(run_value(&mut machine), Value::Float(124750.0));
    let identity = machine
        .callable_identity_maintenance_stats()
        .expect("identity maintenance survives reset");
    assert!(
        identity.entries <= 32 && identity.capacity <= 64,
        "live identity metadata must stay bounded after reset, got {identity:?}"
    );
}

fn ref_rejects(source: &str, fragment: &str) {
    let catalog = ref_error_catalog();
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&pd_vm_compat_frontends::TYPESCRIPT_REFERENCE_ARRAY_PLUGIN)
        .with_source_plugin(&pd_vm_compat_frontends::LUA_PLUGIN)
        .with_host_api_catalog(Arc::clone(&catalog));
    let result = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        pd_vm_compat_frontends::typescript_flavor(),
        options,
    );
    let error = match result {
        Ok(_) => panic!("source should be rejected, want {fragment:?}:\n{source}"),
        Err(error) => format!("{error:?}"),
    };
    assert!(
        error.contains(fragment),
        "rejection should mention {fragment:?}, got {error:?}"
    );
}

fn plain_rejects(source: &str, fragment: &str) {
    let result = vm::compile_source_with_flavor_and_options(
        source,
        pd_vm_compat_frontends::typescript_flavor(),
        pd_vm_compat_frontends::compile_options(),
    );
    let error = match result {
        Ok(_) => panic!("ordinary source should be rejected, want {fragment:?}:\n{source}"),
        Err(error) => format!("{error:?}"),
    };
    assert!(
        error.contains(fragment),
        "ordinary rejection should mention {fragment:?}, got {error:?}"
    );
}

#[test]
fn ordinary_mode_module_const_capture_keeps_baseline_rejection() {
    let source = "const captured: number = 7;\nexport function value(): number { return captured; }\nvalue();\n";
    plain_rejects(source, "unresolved TypeScript value 'captured'");
}

#[test]
fn reference_mode_module_const_capture_remains_available() {
    let source = "const captured: number = 7;\nexport function value(): number { return captured; }\nvalue();\n";
    let mut machine = ref_vm(source, false);
    assert_eq!(run_value(&mut machine), Value::Int(7));
}

/// Ordinary compilation keeps its current generator refusal, and async
/// generators stay rejected in both modes.
#[test]
fn ordinary_mode_keeps_generator_refusal() {
    plain_rejects(
        "function* two(a: number): Generator<number> {\n  yield a;\n}\n",
        "async or generator function",
    );
    ref_rejects(
        "async function* two(a: number): Generator<number> {\n  yield a;\n}\n",
        "async or generator function",
    );
}

/// Unsupported suspension shapes fail precisely before execution: yield
/// delegation, sent-value yields, bare yields, throwing across suspension,
/// non-numeric yields/returns, and unreachable code.
#[test]
fn generator_unsupported_shapes_reject_precisely() {
    ref_rejects(
        "function* d(): Generator<number> {\n  yield* [1];\n}\n",
        "yield delegation",
    );
    ref_rejects(
        "function* s(): Generator<number> {\n  const x = yield 1;\n}\n",
        "sent-value yield",
    );
    ref_rejects(
        "function* b(): Generator<number> {\n  yield;\n}\n",
        "bare yield",
    );
    ref_rejects(
        "function* t(): Generator<number> {\n  try {\n    yield 1;\n  } catch (e) {\n  }\n}\n",
        "try/finally",
    );
    ref_rejects(
        "function* s(): Generator<number> {\n  yield \"hi\";\n}\n",
        "must be a number",
    );
    ref_rejects(
        "function* s(): Generator<number, number> {\n  yield 1;\n  return \"hi\";\n}\n",
        "must be a number",
    );
    ref_rejects(
        "function* u(): Generator<number, number> {\n  yield 1;\n  return 2;\n  yield 3;\n}\n",
        "unreachable",
    );
    ref_rejects(
        "function* g<T>(a: T): Generator<number> {\n  yield 1;\n}\n",
        "generic generator",
    );
}

/// The admitted protocol is exactly plain `.next()`: arguments, iterator
/// result/throw methods, unknown fields and result identity comparisons
/// fail precisely; unbound extraction keeps working through the closure.
#[test]
fn generator_protocol_boundaries_reject_precisely() {
    ref_rejects(
        "function* two(): Generator<number> {\n  yield 1;\n}\nconst it = two();\nit.next(1);\n",
        "with an argument",
    );
    ref_rejects(
        "function* two(): Generator<number> {\n  yield 1;\n}\nconst it = two();\nit.return();\n",
        "other than next()",
    );
    ref_rejects(
        "function* two(): Generator<number> {\n  yield 1;\n}\nconst it = two();\nconst r = it.next();\nconst same = r === r;\n",
        "identity comparison",
    );
    ref_rejects(
        "function* two(): Generator<number> {\n  yield 1;\n}\nconst it = two();\nconst done = it.done;\n",
        "has no field",
    );
}

/// Ordinary records with a `next` field keep working: field reads resolve
/// normally and never take the generator path.
#[test]
fn ordinary_record_with_next_field_is_untouched() {
    let source = "interface Box {\n  next: number;\n}\nconst box: Box = { next: 7 };\nconst n: number = box.next;\nn;\n";
    let mut machine = ref_vm(source, false);
    assert_eq!(run_value(&mut machine), Value::Int(7));
}

/// The `.next()` receiver evaluates exactly once, even when it constructs
/// a fresh iterator inline; defaults run once per construction.
#[test]
fn generator_receiver_and_default_evaluate_once() {
    let _guard = gen_clock::lock();
    let source = r#"
import { tick_default as tickDefault, read as readClock, reset as resetClock } from "gen_clock";
resetClock();
function* one(a: number = tickDefault()): Generator<number, number> {
  yield a;
  return a;
}
const r1 = one().next();
const afterDirect = readClock();
const it = one();
const afterCreate = readClock();
const r2 = it.next();
const afterFirst = readClock();
[r1.value, afterDirect, r2.value, afterCreate, afterFirst];
"#;
    let mut machine = clock_vm(source, false);
    assert_eq!(
        run_value(&mut machine),
        Value::Array(
            vec![
                // inline construction ticks the default exactly once.
                Value::Float(1.0),
                Value::Int(1),
                // separate construction ticks once more; first next is silent.
                Value::Float(2.0),
                Value::Int(2),
                Value::Int(2),
            ]
            .into()
        )
    );
}

/// Default initializers evaluate left-to-right and short-circuit: the
/// first throwing default wins and later ones never run.
#[test]
fn generator_defaults_evaluate_left_to_right() {
    let source = r#"
function boom(message: string): number {
  throw new Error(message);
  return 0;
}
function* m(a: number = boom("first"), b: number = boom("second")): Generator<number, number> {
  yield a + b;
  return a;
}
m();
"#;
    let mut machine = ref_error_vm(source, false);
    let outcome = machine.run();
    assert!(
        matches!(&outcome, Err(vm::VmError::HostError(message)) if message == "first"),
        "first default should win, outcome was {outcome:?}"
    );
}

/// Iterator alias identity holds (same cells) while independent instances
/// compare unequal; unbound `next` extraction still advances the shared
/// iterator through the closure value.
#[test]
fn generator_alias_identity_and_unbound_next() {
    let source = r#"
function* two(): Generator<number, number> {
  yield 1;
  return 2;
}
const it = two();
const alias = it;
const same = it === alias ? 1 : 0;
const other = two();
const diff = it === other ? 1 : 0;
const n = it.next;
const r1 = n();
const r2 = alias.next();
const r3 = other.next();
[same, diff, r1.value, r2.value, r2.done, r3.value];
"#;
    let mut machine = ref_vm(source, false);
    assert_eq!(
        run_value(&mut machine),
        Value::Array(
            vec![
                Value::Int(1),
                Value::Int(0),
                Value::Int(1),
                Value::Int(2),
                Value::Bool(true),
                Value::Int(1),
            ]
            .into()
        )
    );
}

/// A throw inside a generator body aborts the whole program with its
/// message when that suspension point is reached.
#[test]
fn generator_body_throw_aborts_with_message() {
    let source = r#"
function* checked(n: number): Generator<number, number> {
  if (n < 0) {
    throw new Error("negative step");
  }
  yield n;
  return n;
}
const it = checked(-3);
it.next();
"#;
    let mut machine = ref_error_vm(source, false);
    let outcome = machine.run();
    assert!(
        matches!(&outcome, Err(vm::VmError::HostError(message)) if message == "negative step"),
        "body throw should abort, outcome was {outcome:?}"
    );
}

/// Physical schemas stay truthful: iterator/result locals carry the private
/// nominals, finite values project `number | null` over `Optional(Number)`
/// while proven-infinite values stay plain `number`.
#[test]
fn generator_physical_schemas_and_hints_are_truthful() {
    let source = "function* finite(): Generator<number, number> {\n  yield 1;\n  return 2;\n}\nfunction* endless(): Generator<number> {\n  while (true) {\n    yield 3;\n  }\n}\nconst finit = finite();\nconst endlessIt = endless();\nconst r = finit.next();\nconst value = r.value;\nconst done = r.done;\nconst e = endlessIt.next();\nconst evalue = e.value;\n";
    let catalog = ref_error_catalog();
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&pd_vm_compat_frontends::TYPESCRIPT_REFERENCE_ARRAY_PLUGIN)
        .with_source_plugin(&pd_vm_compat_frontends::LUA_PLUGIN)
        .with_host_api_catalog(Arc::clone(&catalog));
    let hints =
        pd_vm_compat_frontends::collect_reference_array_type_hints_with_options(source, options)
            .expect("generator hints should collect");
    let hint = |name: &str| {
        hints
            .iter()
            .find(|hint| hint.name == name)
            .unwrap_or_else(|| panic!("missing hint for {name}"))
    };
    // Core renames guest nominals per module (`__guest_type_*`); the source
    // labels keep the private nominal spellings, and every generator schema
    // is a bare nominal (never a callable shape, slot or bare name). The
    // expected spellings below are asserted through the public hints only;
    // no public naming API exposes these internals.
    let finite_iter = "__CompatGenIterator_finite";
    let finite_result = "__CompatGenResult_finite";
    let endless_iter = "__CompatGenIterator_endless";
    let endless_result = "__CompatGenResult_endless";
    let bare_nominal = |hint: &vm::compiler::InferredLocalTypeHint| {
        matches!(
            hint.schema,
            Some(vm::TypeSchema::Named(_, ref args)) if args.is_empty()
        )
    };
    assert_eq!(hint("finit").inferred_type, finite_iter);
    assert!(bare_nominal(hint("finit")));
    assert_eq!(hint("endlessIt").inferred_type, endless_iter);
    assert!(bare_nominal(hint("endlessIt")));
    assert_eq!(hint("r").inferred_type, finite_result);
    assert!(bare_nominal(hint("r")));
    assert_eq!(
        hint("value").schema,
        Some(vm::TypeSchema::Optional(Box::new(vm::TypeSchema::Int)))
    );
    assert_eq!(hint("value").inferred_type, "Int | null");
    assert_eq!(hint("done").schema, Some(vm::TypeSchema::Bool));
    assert_eq!(hint("e").inferred_type, endless_result);
    assert!(bare_nominal(hint("e")));
    assert_eq!(hint("evalue").schema, Some(vm::TypeSchema::Int));
    assert_eq!(hint("evalue").inferred_type, "Int");
}

/// Exported generator constructors are precisely rejected: iterator/result
/// nominals are module-private, so cross-module identity would need loader
/// support that does not exist here. Same-module private generators cover
/// the admitted scope; the unchanged Fibonacci module keeps its private
/// generator for the same reason.
#[test]
fn generator_exported_constructor_rejects_precisely() {
    ref_rejects(
        "export function* counter(): Generator<number> {\n  yield 1;\n}\n",
        "exported generator function",
    );
}

/// Numeric `**` lowers to the existing floating power builtin in the
/// explicit mode and stays rejected in the ordinary mode.
#[test]
fn numeric_power_operator_uses_existing_builtin() {
    let source = "const x = 2.0 ** 10.0;\nconst y: number = x;\ny === 1024 ? 1 : 0;\n";
    let mut machine = ref_error_vm(source, false);
    assert_eq!(run_value(&mut machine), Value::Int(1));
    plain_rejects("const x = 2.0 ** 10.0;\n", "binary operator");
}

/// Importing a generator constructor fails at the module boundary: since
/// generator constructors cannot be exported, the name is not exported and
/// the loader reports it precisely.
#[test]
fn generator_imported_constructor_call_rejects_precisely() {
    let project = fib_project::Project::new();
    let module = project.path().join("gen.ts");
    std::fs::write(
        &module,
        "export function* maker(): Generator<number> {\n  yield 1;\n}\n",
    )
    .expect("module should write");
    // The exporter itself is already precisely rejected; the importer can
    // never observe a generator export.
    let catalog = ref_error_catalog();
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&pd_vm_compat_frontends::TYPESCRIPT_REFERENCE_ARRAY_PLUGIN)
        .with_source_plugin(&pd_vm_compat_frontends::LUA_PLUGIN)
        .with_host_api_catalog(Arc::clone(&catalog));
    let result = vm::compile_source_file_with_capture_certification_and_options(&module, options);
    let error = match result {
        Ok(_) => panic!("exported generator should be rejected"),
        Err(error) => format!("{error:?}"),
    };
    assert!(
        error.contains("exported generator function"),
        "should report the export boundary, got {error:?}"
    );
}

/// Long next histories keep only live state: repeated construction, deep
/// stepping, drops and aliases stay bounded with no per-yield growth. The
/// opted-in callable identity maintenance shows a small live set after
/// thirty factory evaluations, not thirty retained histories.
#[test]
fn generator_lifetimes_stay_bounded() {
    let source = r#"
function* counter(): Generator<number> {
  let a = 0;
  while (true) {
    yield a;
    a = a + 1;
  }
}
let total: number = 0;
for (let k = 0; k < 30; ++k) {
  const it = counter();
  const alias = it;
  const r = alias.next();
  const v: number = r.value;
  total = total + v;
}
total;
"#;
    let mut machine = ref_vm(source, false);
    machine.set_callable_identity_maintenance(Some(
        vm::CallableIdentityMaintenanceConfig::new(4).expect("budget 4 is valid"),
    ));
    assert_eq!(run_value(&mut machine), Value::Float(0.0));
    // Thirty fresh iterators each yield their first value (0) through a
    // shared alias, proving repeated factory evaluation stays bounded.
    let identity = machine
        .callable_identity_maintenance_stats()
        .expect("identity maintenance was enabled");
    assert!(
        identity.entries <= 32 && identity.capacity <= 64,
        "live identity metadata must stay bounded, got {identity:?}"
    );
}

/// Construction arguments evaluate once, left-to-right: two clock ticks as
/// arguments arrive in order with no repeats.
#[test]
fn generator_construction_args_evaluate_once_in_order() {
    let _guard = gen_clock::lock();
    let source = r#"
import { tick_default as first, tick_body as second, read as readClock, reset as resetClock } from "gen_clock";
resetClock();
function* ab(a: number, b: number): Generator<number, number> {
  yield a;
  yield b;
  return a + b;
}
const it = ab(first(), second());
const afterCreate = readClock();
const r1 = it.next();
const r2 = it.next();
const r3 = it.next();
const afterAll = readClock();
[r1.value, r2.value, r3.value, afterCreate, afterAll];
"#;
    let mut machine = clock_vm(source, false);
    assert_eq!(
        run_value(&mut machine),
        Value::Array(
            vec![
                // left-to-right ticks arrive as (1, 2); the return sums them.
                Value::Float(1.0),
                Value::Float(2.0),
                Value::Float(3.0),
                Value::Int(2),
                Value::Int(2),
            ]
            .into()
        )
    );
}

/// Completion through loop exits and bare returns: falling off the end or
/// returning without a value both complete without a numeric value, and an
/// empty body completes immediately.
#[test]
fn generator_loop_exit_bare_return_and_empty_complete() {
    let source = r#"
function* drain(n: number): Generator<number, number> {
  let i: number = 0;
  while (i < n) {
    yield i;
    i = i + 1;
  }
}
function* stopearly(): Generator<number, number> {
  yield 1;
  return;
}
function* empty(): Generator<number, void> {
}
const d = drain(2);
const d0 = d.next();
const d1 = d.next();
const d2 = d.next();
const s = stopearly();
const s0 = s.next();
const s1 = s.next();
const e = empty();
const e0 = e.next();
[d0.value, d0.done, d1.value, d1.done, d2.done, s0.value, s0.done, s1.done, e0.done];
"#;
    let mut machine = ref_vm(source, false);
    assert_eq!(
        run_value(&mut machine),
        Value::Array(
            vec![
                Value::Int(0),
                Value::Bool(false),
                Value::Float(1.0),
                Value::Bool(false),
                Value::Bool(true),
                Value::Int(1),
                Value::Bool(false),
                Value::Bool(true),
                Value::Bool(true),
            ]
            .into()
        )
    );
}

/// Yields inside branches and nested loops resume correctly; `continue` in
/// a `for` still runs the update.
#[test]
fn generator_branch_yields_nested_loops_and_for_continue() {
    let source = r#"
function* pick(c: boolean): Generator<number, number> {
  if (c) {
    yield 1;
  } else {
    yield 2;
  }
  yield 3;
  return 4;
}
function* nest(): Generator<number> {
  let i: number = 0;
  while (i < 2) {
    let j: number = 0;
    while (j < 2) {
      yield i * 10 + j;
      j = j + 1;
    }
    i = i + 1;
  }
}
function* skippy(): Generator<number, number> {
  for (let k: number = 0; k < 4; k = k + 1) {
    if (k === 1) {
      continue;
    }
    yield k;
  }
  return 9;
}
const t = pick(true);
const t0 = t.next();
const t1 = t.next();
const t2 = t.next();
const f = pick(false);
const f0 = f.next();
const n = nest();
const n0 = n.next();
const n1 = n.next();
const n2 = n.next();
const n3 = n.next();
const s = skippy();
const s0 = s.next();
const s1 = s.next();
const s2 = s.next();
const s3 = s.next();
[t0.value, t1.value, t2.value, t2.done, f0.value, n0.value, n1.value, n2.value, n3.value, s0.value, s1.value, s2.value, s3.value, s3.done];
"#;
    let mut machine = ref_vm(source, false);
    assert_eq!(
        run_value(&mut machine),
        Value::Array(
            vec![
                Value::Int(1),
                Value::Int(3),
                Value::Int(4),
                Value::Bool(true),
                Value::Int(2),
                Value::Float(0.0),
                Value::Float(1.0),
                Value::Float(10.0),
                Value::Float(11.0),
                Value::Int(0),
                Value::Float(2.0),
                Value::Float(3.0),
                Value::Int(9),
                Value::Bool(true),
            ]
            .into()
        )
    );
}

/// Body bindings shadow parameters with fresh persistent slots, and two
/// different generators interleave independently.
#[test]
fn generator_param_shadowing_and_interleaving() {
    let source = r#"
function* shadow(a: number): Generator<number, number> {
  let a = 5;
  yield a;
  return a;
}
function* evens(): Generator<number> {
  let x = 0;
  while (true) {
    yield x;
    x = x + 2;
  }
}
function* odds(): Generator<number> {
  let y = 1;
  while (true) {
    yield y;
    y = y + 2;
  }
}
const s = shadow(99);
const sv = s.next();
const e = evens();
const o = odds();
const e0 = e.next();
const o0 = o.next();
const e1 = e.next();
const o1 = o.next();
[sv.value, e0.value, o0.value, e1.value, o1.value];
"#;
    let mut machine = ref_vm(source, false);
    assert_eq!(
        run_value(&mut machine),
        Value::Array(
            vec![
                Value::Int(5),
                Value::Int(0),
                Value::Int(1),
                Value::Int(2),
                Value::Int(3),
            ]
            .into()
        )
    );
}

/// A long single-iterator history steps correctly far past the early
/// states, with storage proportional to locals, not steps.
#[test]
fn generator_long_single_history() {
    let source = r#"
function* counter(): Generator<number> {
  let a = 0;
  let b = 1;
  while (true) {
    yield a;
    const c = a + b;
    a = b;
    b = c;
  }
}
const it = counter();
let acc: number = 0;
acc = acc + it.next().value;
acc = acc + it.next().value;
acc = acc + it.next().value;
acc = acc + it.next().value;
acc = acc + it.next().value;
acc = acc + it.next().value;
acc = acc + it.next().value;
acc = acc + it.next().value;
acc;
"#;
    let mut machine = ref_vm(source, false);
    // 0 + 1 + 1 + 2 + 3 + 5 + 8 + 13 = 33 (Float through the Number
    // accumulator's floating projection).
    assert_eq!(run_value(&mut machine), Value::Float(33.0));
}

/// Mismatched return-type promises fail precisely: a void promise with a
/// valued return, and a non-Generator annotation. (A promised numeric
/// return without a valued return is vacuously satisfied and stays valid.)
#[test]
fn generator_return_annotation_mismatches_reject() {
    ref_rejects(
        "function* g(): Generator<number, void> {\n  yield 1;\n  return 2;\n}\n",
        "declares a void return",
    );
    ref_rejects(
        "function* g(): number {\n  yield 1;\n}\n",
        "Generator<number>",
    );
}

/// An entry can import a defaulted arrow that uses a private generator:
/// omitted defaults still evaluate in callee scope while stepping stays
/// lazy and independent per construction.
#[test]
fn generator_imported_defaulted_arrow_steps_lazily() {
    let project = fib_project::Project::new();
    let module = project.path().join("calc.ts");
    std::fs::write(
        &module,
        "function* gen(start: number): Generator<number> {\n  let a = start;\n  while (true) {\n    yield a;\n    a = a + 1;\n  }\n}\nexport const first = (count: number = 2): number => {\n  const it = gen(10);\n  let total: number = 0;\n  for (let i: number = 0; i < count; i = i + 1) {\n    const r = it.next();\n    const v: number = r.value;\n    total = total + v;\n  }\n  return total;\n};\n",
    )
    .expect("module should write");
    let main = project.path().join("main.ts");
    std::fs::write(
        &main,
        "import { first } from \"./calc.ts\";\nconst a = first();\nconst b = first(3);\n[a, b];\n",
    )
    .expect("entry should write");
    let (mut machine, _) = fib_file_vm(&main, false);
    // first() sums 10 + 11 = 21 (Float through the call projection);
    // first(3) sums 10 + 11 + 12 = 33.
    assert_eq!(
        run_value(&mut machine),
        Value::Array(vec![Value::Float(21.0), Value::Float(33.0)].into())
    );
}
