//! B4 explicit Compat CLI selection with the four unchanged upstream
//! TypeScript algorithm modules run jointly through source entry/import.
//!
//! The thin `pd-vm-compat-run` binary keeps its default route unchanged.
//! A leading Compat-only `--typescript-reference-arrays` selector chooses the
//! accepted general typed reference-array/generator mode: the selector must
//! be first, only it is stripped, and every remaining argument reaches
//! Core's existing `vm::cli::run_with_args` unchanged with the exact
//! selected-mode compile options and a fresh per-VM production
//! host-services factory. Unknown/trailing selectors, file arguments,
//! output values, errors and default modes stay Core-owned.
//!
//! All four upstream modules are byte-unchanged canonical sources from the
//! accepted fixtures (upstream `19b4ced86c99815f142d4a46a028f55487b8038a`):
//! their actual exports are read, never guessed, and exercised through
//! normal importing callers plus one joint caller using all four in a
//! single source/module graph. Wrappers are ordinary callers, never
//! substituted algorithms.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const FACTORIAL_ORIGINAL: &[u8] = include_bytes!("fixtures/typescript/factorial.ts");
const FIBONACCI_ORIGINAL: &[u8] = include_bytes!("fixtures/typescript/fibonacci.ts");
const BUBBLE_SORT_ORIGINAL: &[u8] = include_bytes!("fixtures/typescript/bubble_sort.ts");
const QUICK_SORT_ORIGINAL: &[u8] = include_bytes!("fixtures/typescript/quick_sort.ts");
const NODE_FACTORIAL_VECTORS: &str =
    include_str!("fixtures/typescript/factorial-node-vectors.json");

/// Leading Compat-only selector. It must come first; Core owns everything
/// after it (and any selector-looking token elsewhere in the argument list).
const SELECTOR: &str = "--typescript-reference-arrays";

/// Default per-process CLI bound. Native codegen tests use their own
/// longer bound.
const CLI_BOUND: Duration = Duration::from_secs(60);

struct ScratchTree(PathBuf);

impl ScratchTree {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        loop {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("compat-ref-cli-{}-{id}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create scratch tree {}: {error}", path.display()),
            }
        }
    }
}

impl Drop for ScratchTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn runner_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pd-vm-compat-run"))
}

fn run_cli(args: &[String]) -> Output {
    run_cli_bound(args, &[], CLI_BOUND)
}

fn run_cli_with_stdin(args: &[String], input: &[u8]) -> Output {
    run_cli_bound(args, input, CLI_BOUND)
}

fn run_cli_bound(args: &[String], input: &[u8], bound: Duration) -> Output {
    let mut child = Command::new(runner_bin())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start pd-vm-compat-run");
    let mut stdin = child.stdin.take().expect("pipe CLI stdin");
    let input = input.to_vec();
    let input_writer = thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let mut stdout = child.stdout.take().expect("capture CLI stdout");
    let mut stderr = child.stderr.take().expect("capture CLI stderr");
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).expect("read CLI stdout");
        bytes
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).expect("read CLI stderr");
        bytes
    });
    let deadline = Instant::now() + bound;
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll CLI process") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("pd-vm-compat-run exceeded the test bound");
        }
        thread::sleep(Duration::from_millis(10));
    };
    input_writer.join().expect("join CLI stdin writer");
    Output {
        status,
        stdout: stdout_reader.join().expect("join stdout reader"),
        stderr: stderr_reader.join().expect("join stderr reader"),
    }
}

fn selected(args: &[String]) -> Vec<String> {
    let mut full = vec![SELECTOR.to_owned()];
    full.extend(args.iter().cloned());
    full
}

fn run_selected(args: &[String]) -> Output {
    run_cli(&selected(args))
}

/// Writes all four byte-unchanged upstream modules into `root`.
fn upstream_project(root: &Path) {
    for (dir, name, bytes) in [
        ("maths", "factorial.ts", FACTORIAL_ORIGINAL),
        ("maths", "fibonacci.ts", FIBONACCI_ORIGINAL),
        ("sorts", "bubble_sort.ts", BUBBLE_SORT_ORIGINAL),
        ("sorts", "quick_sort.ts", QUICK_SORT_ORIGINAL),
    ] {
        let module = root.join(dir).join(name);
        fs::create_dir_all(module.parent().expect("module parent"))
            .expect("create upstream module directory");
        fs::write(&module, bytes).expect("write unchanged upstream module");
        assert_eq!(
            fs::read(&module).expect("read upstream module").as_slice(),
            bytes,
            "imported calls must use exact upstream bytes"
        );
    }
}

fn write_main(root: &Path, source: &str) -> PathBuf {
    let main = root.join("main.ts");
    fs::write(&main, source).expect("write importing caller");
    main
}

fn run_selected_file(main: &Path) -> Output {
    run_selected(&[
        "--fuel".to_owned(),
        "100000".to_owned(),
        main.display().to_string(),
    ])
}

fn assert_selected_int1(output: &Output, context: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{context} failed: {stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("Int(1)"),
        "{context} proof missing: {stdout}"
    );
}

const BUBBLE_EXACT_CALLER: &str = "import { bubbleSort as sort } from \"./sorts/bubble_sort.ts\";\nconst sorted = sort([8, 3, 5, 1, 4, 2]);\nconst eq = sorted[0] === 1 && sorted[1] === 2 && sorted[2] === 3 && sorted[3] === 4 && sorted[4] === 5 && sorted[5] === 8;\neq ? 1 : 0;\n";

#[test]
fn scalar_default_control_passes_before_selection() {
    // The default route is unchanged: a scalar program succeeds without the
    // selector, both before and after the B4 integration.
    let scratch = ScratchTree::new();
    let main = scratch.0.join("scalar.ts");
    fs::write(&main, "const result: number = 40 + 2;\nresult;\n").expect("write scalar control");
    let output = run_cli(&[
        "--fuel".to_owned(),
        "100000".to_owned(),
        main.display().to_string(),
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "scalar default control failed: {stdout}\n{stderr}"
    );
    assert!(stdout.contains("42"), "scalar control result: {stdout}");
}

#[test]
fn selected_mode_imported_bubble_sort_sorts_the_exact_vector() {
    // Desired B4 behavior: the leading selector admits the accepted explicit
    // reference-array mode, so an unchanged-upstream importing caller sorts
    // the exact vector. On the unchanged binary this failed at admission
    // (`unknown flag`), not in any backend or executed business logic.
    let scratch = ScratchTree::new();
    upstream_project(&scratch.0);
    let main = write_main(&scratch.0, BUBBLE_EXACT_CALLER);
    assert_selected_int1(&run_selected_file(&main), "selected-mode imported sort");
}

#[test]
fn selector_must_be_first_and_core_owns_the_rest() {
    // A selector-looking token anywhere but first stays Core-owned: Core's
    // own `unknown flag` refusal, never the selected mode.
    let scratch = ScratchTree::new();
    upstream_project(&scratch.0);
    let main = write_main(&scratch.0, BUBBLE_EXACT_CALLER);
    for args in [
        vec![
            "--fuel".to_owned(),
            "100000".to_owned(),
            SELECTOR.to_owned(),
            main.display().to_string(),
        ],
        vec![main.display().to_string(), SELECTOR.to_owned()],
    ] {
        let output = run_cli(&args);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "non-first selector must not enter the selected mode: {stdout}\n{stderr}"
        );
        assert!(
            stderr.contains(SELECTOR),
            "Core refusal must identify the token: {stderr}"
        );
        assert!(
            !stdout.contains("Int(1)"),
            "selected-mode success must not leak through a non-first selector: {stdout}"
        );
    }
}

#[test]
fn selected_version_help_and_error_behavior_match_core() {
    // The selected route strips only the selector: `--version`, `--help`
    // and Core-owned errors behave exactly as on the default route.
    let default_version = run_cli(&["--version".to_owned()]);
    let selected_version = run_selected(&["--version".to_owned()]);
    assert!(default_version.status.success());
    assert!(selected_version.status.success());
    assert_eq!(
        selected_version.stdout, default_version.stdout,
        "selected --version must stay Core's version text"
    );
    assert!(
        String::from_utf8_lossy(&selected_version.stdout).contains("pd-vm-compat-run"),
        "version text identifies the binary"
    );
    let selected_help = run_selected(&["--help".to_owned()]);
    assert!(selected_help.status.success());
    assert!(
        String::from_utf8_lossy(&selected_help.stdout).contains("Usage:"),
        "selected --help keeps Core usage"
    );
    // Ordinary default errors are untouched: an unknown default flag still
    // fails with Core's refusal and exit 1.
    let unknown = run_cli(&["--no-such-compat-flag".to_owned()]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("unknown flag"),
        "default error text preserved"
    );
    // A selected guest throw preserves exit 1 and the exact adapter message.
    let scratch = ScratchTree::new();
    upstream_project(&scratch.0);
    let main = write_main(
        &scratch.0,
        "import { factorial as f } from \"./maths/factorial.ts\";\nconst r = f(-1);\nr;\n",
    );
    let thrown = run_selected_file(&main);
    assert_eq!(thrown.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&thrown.stderr).contains("only natural numbers are supported"),
        "selected route preserves the exact error text"
    );
}

#[test]
fn selected_factorial_import_matches_all_node_vectors() {
    // All nine recorded Node vectors (upstream
    // `19b4ced86c99815f142d4a46a028f55487b8038a`, Node v26.7.0) through a
    // normal importing caller: four returns including factorial(6) = 720,
    // five throws with the exact message.
    let vectors: serde_json::Value =
        serde_json::from_str(NODE_FACTORIAL_VECTORS).expect("Node vectors are JSON");
    assert_eq!(vectors["node"], "v26.7.0");
    assert_eq!(
        vectors["upstream_commit"],
        "19b4ced86c99815f142d4a46a028f55487b8038a"
    );
    let cases = vectors["cases"].as_array().expect("nine Node cases");
    assert_eq!(cases.len(), 9);
    let mut returns = 0;
    let mut throws = 0;
    for case in cases {
        let expression = case["expression"].as_str().expect("Node expression");
        let scratch = ScratchTree::new();
        upstream_project(&scratch.0);
        let main = write_main(
            &scratch.0,
            &format!(
                "import {{ factorial as f }} from \"./maths/factorial.ts\";\nconst r = f({expression});\nr;\n"
            ),
        );
        let output = run_selected_file(&main);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if case["outcome"] == "return" {
            returns += 1;
            let expected = case["value"].as_i64().expect("Node integral return");
            assert!(
                output.status.success(),
                "selected factorial({expression}) failed: {stdout}\n{stderr}"
            );
            // Reference-array numerics project to Float; accept either
            // spelling of the same integral value.
            assert!(
                stdout.contains(&format!("{expected}")),
                "factorial({expression}) != {expected}: {stdout}"
            );
        } else {
            throws += 1;
            let message = case["message"].as_str().expect("exact Node message");
            assert_eq!(message, "only natural numbers are supported");
            assert!(
                !output.status.success(),
                "selected factorial({expression}) must throw, not return: {stdout}"
            );
            assert!(
                stderr.contains(message),
                "exact invalid-input error missing for {expression}: {stderr}"
            );
            assert!(
                !stdout.contains("stack:"),
                "throw published a return stack for {expression}: {stdout}"
            );
        }
    }
    assert_eq!((returns, throws), (4, 5));
}

#[test]
fn selected_fibonacci_all_exports_agree_on_valid_vectors() {
    // All three actual exports from the unchanged module, through an
    // importing caller with aliases: 0/1/2/10/20 plus cross-export agreement.
    let scratch = ScratchTree::new();
    upstream_project(&scratch.0);
    let main = write_main(
        &scratch.0,
        "import { nthFibonacci as fib, nthFibonacciRecursively as rec, nthFibonacciUsingFormula as fom } from \"./maths/fibonacci.ts\";\nconst a0 = fib(0) === 0 && rec(0) === 0 && fom(0) === 0;\nconst a1 = fib(1) === 1 && rec(1) === 1 && fom(1) === 1;\nconst a2 = fib(2) === 1 && rec(2) === 1 && fom(2) === 1;\nconst a10 = fib(10) === 55 && rec(10) === 55 && fom(10) === 55;\nconst a20 = fib(20) === 6765 && rec(20) === 6765 && fom(20) === 6765;\nconst cross = fib(10) === rec(10) && rec(10) === fom(10);\nconst ok = a0 && a1 && a2 && a10 && a20 && cross;\nok ? 1 : 0;\n",
    );
    assert_selected_int1(
        &run_selected_file(&main),
        "selected fibonacci export agreement",
    );
}

#[test]
fn selected_fibonacci_primary_keeps_exact_invalid_input_errors() {
    // Primary-only errors: only `nthFibonacci` is pinned to exact
    // invalid-input diagnostics.
    for (input, message) in [
        ("Math.sqrt(-1)", "The input needs to be a number"),
        ("-1", "The input needs to be a non-negative integer"),
        ("1.5", "The input needs to be a non-negative integer"),
    ] {
        let scratch = ScratchTree::new();
        upstream_project(&scratch.0);
        let main = write_main(
            &scratch.0,
            &format!(
                "import {{ nthFibonacci as fib }} from \"./maths/fibonacci.ts\";\nconst r = fib({input});\nr;\n"
            ),
        );
        let output = run_selected_file(&main);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "selected fib({input}) must throw: {stdout}"
        );
        assert!(
            stderr.contains(message),
            "exact primary error missing for {input}: {stderr}"
        );
    }
}

#[test]
fn selected_bubble_sort_vectors_identity_and_alias() {
    // Empty, singleton, duplicates, negatives, sorted, reversed, plus the
    // returned-object identity the Node oracle pins (`r === a`).
    let scratch = ScratchTree::new();
    upstream_project(&scratch.0);
    let main = write_main(
        &scratch.0,
        "import { bubbleSort } from \"./sorts/bubble_sort.ts\";\nconst empty = bubbleSort([]);\nconst one = bubbleSort([7]);\nconst dup = bubbleSort([3, 1, 2, 1, 3]);\nconst neg = bubbleSort([-2, -8, 0, 5, -1]);\nconst fwd = bubbleSort([1, 2, 3, 4, 5]);\nconst rev = bubbleSort([5, 4, 3, 2, 1]);\nconst a = [10, 20];\nconst r = bubbleSort(a);\nconst ok = empty.length === 0 && one[0] === 7 && dup[0] === 1 && dup[4] === 3 && neg[0] === -8 && neg[4] === 5 && fwd[4] === 5 && rev[0] === 1 && r === a && r[1] === 20;\nok ? 1 : 0;\n",
    );
    assert_selected_int1(&run_selected_file(&main), "selected bubble vectors");
}

#[test]
fn selected_bubble_sort_alias_mutation_and_rebinding() {
    // Shared-carrier alias mutation is visible, and `let` rebinding replaces
    // only the carrier.
    let scratch = ScratchTree::new();
    upstream_project(&scratch.0);
    let main = write_main(
        &scratch.0,
        "import { bubbleSort } from \"./sorts/bubble_sort.ts\";\nconst c = [5, 1];\nconst d = c;\nbubbleSort(d);\nlet e = [3, 2, 1];\ne = [1, 2];\nconst f = bubbleSort(e);\nconst ok = c[0] === 1 && c[1] === 5 && d === c && e[0] === 1 && f === e;\nok ? 1 : 0;\n",
    );
    assert_selected_int1(&run_selected_file(&main), "selected bubble alias/rebind");
}

#[test]
fn selected_quicksort_exact_vector_and_returned_identity() {
    // Both sorting APIs on [8, 3, 5, 1, 4, 2]: `QuickSort` returns the same
    // (sorted) object it was given.
    let scratch = ScratchTree::new();
    upstream_project(&scratch.0);
    let main = write_main(
        &scratch.0,
        "import { QuickSort } from \"./sorts/quick_sort.ts\";\nconst input = [8, 3, 5, 1, 4, 2];\nconst sorted = QuickSort(input);\nconst eq = sorted[0] === 1 && sorted[1] === 2 && sorted[2] === 3 && sorted[3] === 4 && sorted[4] === 5 && sorted[5] === 8;\nconst ok = eq && sorted === input;\nok ? 1 : 0;\n",
    );
    assert_selected_int1(&run_selected_file(&main), "selected quicksort exact vector");
}

#[test]
fn selected_quicksort_vectors_duplicates_negatives_and_alias() {
    // Empty, singleton, duplicates, negatives, sorted, reversed, plus
    // alias mutation through a shared carrier. Sortedness only: the
    // primary randomness stays on, so pivot choices are never pinned.
    let scratch = ScratchTree::new();
    upstream_project(&scratch.0);
    let main = write_main(
        &scratch.0,
        "import { QuickSort } from \"./sorts/quick_sort.ts\";\nconst empty = QuickSort([]);\nconst one = QuickSort([7]);\nconst dup = QuickSort([3, 1, 2, 1, 3]);\nconst neg = QuickSort([-2, -8, 0, 5, -1]);\nconst c = [5, 1];\nconst d = c;\nQuickSort(d);\nconst ok = empty.length === 0 && one[0] === 7 && dup[0] === 1 && dup[4] === 3 && neg[0] === -8 && neg[4] === 5 && c[0] === 1 && c[1] === 5 && d === c;\nok ? 1 : 0;\n",
    );
    assert_selected_int1(
        &run_selected_file(&main),
        "selected quicksort vectors and alias",
    );
}

#[test]
fn selected_quicksort_sorted_reversed_and_partition_api() {
    // Already-sorted and reversed inputs, the direct `partition` API with
    // explicit bounds, and defaulted `QuickSort(input)` bounds.
    let scratch = ScratchTree::new();
    upstream_project(&scratch.0);
    let main = write_main(
        &scratch.0,
        "import { QuickSort, partition } from \"./sorts/quick_sort.ts\";\nconst fwd = QuickSort([1, 2, 3]);\nconst rev = QuickSort([3, 2, 1]);\nconst probe = [5, 1, 4];\nconst index = partition(probe, 0, 2);\nconst ok = fwd[2] === 3 && rev[0] === 1 && index >= 0 && index <= 2 && probe.length === 3;\nok ? 1 : 0;\n",
    );
    assert_selected_int1(
        &run_selected_file(&main),
        "selected quicksort sorted/reversed/partition",
    );
}

#[test]
fn selected_joint_caller_runs_all_four_modules_in_one_graph() {
    // One importing caller uses all four unchanged modules in a single
    // source/module graph: factorial(6) = 720, the three fibonacci exports
    // agreeing on 10, and both sorts on the exact vector.
    let scratch = ScratchTree::new();
    upstream_project(&scratch.0);
    let main = write_main(
        &scratch.0,
        "import { factorial as f } from \"./maths/factorial.ts\";\nimport { nthFibonacci as fib, nthFibonacciRecursively as rec, nthFibonacciUsingFormula as fom } from \"./maths/fibonacci.ts\";\nimport { bubbleSort } from \"./sorts/bubble_sort.ts\";\nimport { QuickSort } from \"./sorts/quick_sort.ts\";\nconst fact = f(6) === 720;\nconst fibs = fib(10) === 55 && rec(10) === 55 && fom(10) === 55;\nconst bub = bubbleSort([8, 3, 5, 1, 4, 2]);\nconst qck = QuickSort([8, 3, 5, 1, 4, 2]);\nconst sorts = bub[0] === 1 && bub[5] === 8 && qck[0] === 1 && qck[5] === 8;\nconst ok = fact && fibs && sorts;\nok ? 1 : 0;\n",
    );
    assert_selected_int1(
        &run_selected_file(&main),
        "selected joint four-module caller",
    );
}

#[test]
fn selected_bubble_sort_fraction_vector_matches_node_order() {
    // Fractions sort exactly like the Node oracle order; kept separate so
    // the main vector test stays within the live-carrier stack budget.
    let scratch = ScratchTree::new();
    upstream_project(&scratch.0);
    let main = write_main(
        &scratch.0,
        "import { bubbleSort } from \"./sorts/bubble_sort.ts\";\nconst fr = bubbleSort([0.5, -1.25, 2.5, 0.5]);\nconst ok = fr[0] === -1.25 && fr[1] === 0.5 && fr[2] === 0.5 && fr[3] === 2.5;\nok ? 1 : 0;\n",
    );
    assert_selected_int1(&run_selected_file(&main), "selected bubble fraction vector");
}

/// Bounded fresh Node oracle over the same unchanged modules and vectors.
/// Prints `NODE_ORACLE_ALL_PASS` only if every check holds; the Rust side
/// records the actual version, argv, rc and raw output.
const NODE_ORACLE: &str = r#"import { factorial } from "./maths/factorial.ts";
import { nthFibonacci, nthFibonacciRecursively, nthFibonacciUsingFormula } from "./maths/fibonacci.ts";
import { bubbleSort } from "./sorts/bubble_sort.ts";
import { QuickSort, partition } from "./sorts/quick_sort.ts";
let ok = true;
function check(name, cond) {
  console.log((cond ? "PASS " : "FAIL ") + name);
  if (!cond) ok = false;
}
function sortedEq(got, want) {
  return got.length === want.length && got.every((v, i) => Object.is(v, want[i]));
}
// factorial: return + exact errors
check("fact6", factorial(6) === 720);
for (const bad of [-1, 1.5, Math.sqrt(-1), 1e308 * 10, -1e308 * 10]) {
  try {
    factorial(bad);
    check("fact-throw " + bad, false);
  } catch (e) {
    check("fact-throw " + bad, e.message === "only natural numbers are supported");
  }
}
// fibonacci: all three exports on valid vectors
for (const [n, want] of [[0, 0], [1, 1], [2, 1], [10, 55], [20, 6765]]) {
  check(
    "fib " + n,
    nthFibonacci(n) === want &&
      nthFibonacciRecursively(n) === want &&
      nthFibonacciUsingFormula(n) === want
  );
}
for (const bad of [NaN]) {
  try {
    nthFibonacci(bad);
    check("fib-throw NaN", false);
  } catch (e) {
    check("fib-throw NaN", e.message === "The input needs to be a number");
  }
}
for (const bad of [-1, 1.5]) {
  try {
    nthFibonacci(bad);
    check("fib-throw " + bad, false);
  } catch (e) {
    check("fib-throw " + bad, e.message === "The input needs to be a non-negative integer");
  }
}
// sorts on shared vectors; sortedness only (primary randomness stays on)
const VECTORS = [[8, 3, 5, 1, 4, 2], [], [7], [3, 1, 3, 1, 0], [-4, 2, -1, 0], [1, 2, 3, 4], [4, 3, 2, 1], [0.5, -1.25, 2.5, 0.5]];
for (const v of VECTORS) {
  const want = [...v].sort((x, y) => x - y);
  check("bubble " + JSON.stringify(v), sortedEq(bubbleSort([...v]), want));
  check("quick " + JSON.stringify(v), sortedEq(QuickSort([...v]), want));
}
{
  const a = [10, 20];
  const r = bubbleSort(a);
  check("bubble-identity", r === a && r[1] === 20);
  const c = [5, 1];
  const d = c;
  QuickSort(d);
  check("quick-alias", d === c && c[0] === 1 && c[1] === 5);
  const input = [8, 3, 5, 1, 4, 2];
  check("quick-identity", QuickSort(input) === input);
  const probe = [5, 1, 4];
  const index = partition(probe, 0, 2);
  check("partition-range", index >= 0 && index <= 2 && probe.length === 3);
}
// joint line: the same values the selected CLI joint caller asserts
console.log("JOINT fact=720 fib=55,55,55 bubble=1,2,3,4,5,8 quick=1,2,3,4,5,8");
console.log(ok ? "NODE_ORACLE_ALL_PASS" : "NODE_ORACLE_FAIL");
process.exit(ok ? 0 : 1);
"#;

fn run_node(args: &[String], cwd: &Path) -> Output {
    let mut child = Command::new("node")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start node oracle");
    let mut stdout = child.stdout.take().expect("capture node stdout");
    let mut stderr = child.stderr.take().expect("capture node stderr");
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).expect("read node stdout");
        bytes
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).expect("read node stderr");
        bytes
    });
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll node process") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("node oracle exceeded the 120-second bound");
        }
        thread::sleep(Duration::from_millis(10));
    };
    Output {
        status,
        stdout: stdout_reader.join().expect("join node stdout reader"),
        stderr: stderr_reader.join().expect("join node stderr reader"),
    }
}

#[test]
fn fresh_node_oracle_agrees_with_cli_on_all_vectors() {
    // Bounded fresh Node over the same unchanged modules and vectors.
    // Records actual version, argv, rc and raw output; the CLI joint caller
    // asserts the same joint values, pinning agreement.
    let scratch = ScratchTree::new();
    upstream_project(&scratch.0);
    let oracle = scratch.0.join("oracle.mjs");
    fs::write(&oracle, NODE_ORACLE).expect("write node oracle");
    let version = run_node(&["--version".to_owned()], &scratch.0);
    let node_version = String::from_utf8_lossy(&version.stdout).trim().to_owned();
    eprintln!("node version: {node_version}");
    assert!(
        version.status.success() && node_version.starts_with('v'),
        "node version probe failed"
    );
    let output = run_node(&[oracle.display().to_string()], &scratch.0);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("node argv: [{}]", oracle.display());
    eprintln!("node rc: {}", output.status);
    eprintln!("node stdout:\n{stdout}");
    eprintln!("node stderr:\n{stderr}");
    assert!(
        output.status.success(),
        "fresh node oracle failed: {stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("NODE_ORACLE_ALL_PASS"),
        "node oracle did not pass: {stdout}"
    );
    assert!(
        stdout.contains("JOINT fact=720 fib=55,55,55 bubble=1,2,3,4,5,8 quick=1,2,3,4,5,8"),
        "node joint values: {stdout}"
    );
    // The same joint values through the selected CLI in one graph.
    let main = write_main(
        &scratch.0,
        "import { factorial as f } from \"./maths/factorial.ts\";\nimport { nthFibonacci as fib, nthFibonacciRecursively as rec, nthFibonacciUsingFormula as fom } from \"./maths/fibonacci.ts\";\nimport { bubbleSort } from \"./sorts/bubble_sort.ts\";\nimport { QuickSort } from \"./sorts/quick_sort.ts\";\nconst fact = f(6) === 720;\nconst fibs = fib(10) === 55 && rec(10) === 55 && fom(10) === 55;\nconst bub = bubbleSort([8, 3, 5, 1, 4, 2]);\nconst qck = QuickSort([8, 3, 5, 1, 4, 2]);\nconst sorts = bub[0] === 1 && bub[5] === 8 && qck[0] === 1 && qck[5] === 8;\nconst ok = fact && fibs && sorts;\nok ? 1 : 0;\n",
    );
    assert_selected_int1(
        &run_selected_file(&main),
        "CLI joint values matching the node oracle",
    );
}

const BUBBLE_SORT_FIXTURE_STR: &str = include_str!("fixtures/typescript/bubble_sort.ts");

#[test]
fn selected_imported_hint_is_logical_and_physical_is_truthful() {
    // Public logical type-hint projection through an imported alias plus
    // the actual physical compiled schema: hints say `number[]`, the
    // compiled prototype keeps the real carrier nominal.
    use std::sync::Arc;

    let scratch = ScratchTree::new();
    upstream_project(&scratch.0);
    let main = write_main(
        &scratch.0,
        "import { bubbleSort as sort } from \"./sorts/bubble_sort.ts\";\nconst sorted = sort([8, 3, 5, 1, 4, 2]);\nsorted[0];\n",
    );
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(&pd_vm_compat_frontends::TYPESCRIPT_REFERENCE_ARRAY_PLUGIN)
        .with_source_plugin(&pd_vm_compat_frontends::LUA_PLUGIN)
        .with_host_api_catalog(Arc::clone(&catalog));
    let source = fs::read_to_string(&main).expect("wrapper should read");
    let hints = pd_vm_compat_frontends::collect_reference_array_type_hints_at_path_with_options(
        &main, &source, options,
    )
    .expect("hints should collect");
    let hint = hints
        .iter()
        .find(|hint| hint.name == "sorted")
        .expect("missing hint for the imported call result");
    assert_eq!(hint.inferred_type, "number[]");
    assert_eq!(
        hint.schema,
        Some(vm::TypeSchema::Array(Box::new(vm::TypeSchema::Number)))
    );
    // Physical truth: the unchanged upstream export keeps the carrier
    // nominal in the compiled prototype; no Array schema covers it.
    let physical_options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(&pd_vm_compat_frontends::TYPESCRIPT_REFERENCE_ARRAY_PLUGIN)
        .with_source_plugin(&pd_vm_compat_frontends::LUA_PLUGIN)
        .with_host_api_catalog(catalog);
    let compiled = vm::compile_source_with_flavor_and_options(
        BUBBLE_SORT_FIXTURE_STR,
        pd_vm_compat_frontends::typescript_flavor(),
        physical_options,
    )
    .expect("physical prototype probe should compile");
    let mut carriers = compiled
        .program
        .callable_prototypes
        .iter()
        .filter_map(|prototype| match prototype.schema.clone()? {
            vm::TypeSchema::Callable { params, result }
                if params.len() == 1
                    && matches!(&params[0], vm::TypeSchema::Named(_, _))
                    && matches!(&*result, vm::TypeSchema::Named(_, _)) =>
            {
                Some((params.into_iter().next().unwrap(), *result))
            }
            _ => None,
        });
    let (param, result) = carriers
        .next()
        .expect("one carrier-sorted prototype should exist");
    assert_eq!(
        param, result,
        "param and result share the canonical dispatcher carrier"
    );
    assert!(
        !matches!(param, vm::TypeSchema::Array(_)),
        "no Array schema may cover the carrier"
    );
}

fn selected_test_runtime() -> vm::cli::CliRuntime {
    vm::cli::CliRuntime {
        binary_name: "pd-vm-compat-run",
        default_source: "examples/example.ts",
        compile_options: pd_vm_compat_frontends::runtime::cli::reference_array_cli_compile_options,
    }
}

#[test]
fn production_factory_enforces_quota_per_vm_and_resets_between_runs() {
    // Real production factory at its integration boundary: a two-array
    // guest exceeds the tight registered quota, while one-array guests keep
    // succeeding through the same services value before and after failures
    // (per-VM state, no cross-run poisoning).
    use pd_vm_compat_frontends::runtime::cli::typescript_reference_array_cli_host_services_with_limits;

    let limits = pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::new(8, 1, 1 << 20)
        .expect("tight but positive quotas");
    let services = typescript_reference_array_cli_host_services_with_limits(limits);
    let runtime = selected_test_runtime();
    let scratch = ScratchTree::new();
    let two = scratch.0.join("two.ts");
    fs::write(
        &two,
        "const a = [1];\nconst b = [2];\nconst ok = a[0] === 1 && b[0] === 2;\nok ? 1 : 0;\n",
    )
    .expect("write two-array guest");
    let one = scratch.0.join("one.ts");
    fs::write(
        &one,
        "const a = [1];\nconst ok = a[0] === 1;\nok ? 1 : 0;\n",
    )
    .expect("write one-array guest");
    let throwing = scratch.0.join("throwing.ts");
    fs::write(&throwing, "throw new Error(\"factory abort marker\");\n")
        .expect("write abort guest");

    let quota = vm::cli::run_with_args(
        &runtime,
        &[
            "--fuel".to_owned(),
            "100000".to_owned(),
            two.display().to_string(),
        ],
        &services,
    )
    .expect_err("second registration must exceed the registered quota of one");
    assert!(
        quota.to_string().contains("quota"),
        "quota refusal, not another failure: {quota}"
    );
    vm::cli::run_with_args(
        &runtime,
        &[
            "--fuel".to_owned(),
            "100000".to_owned(),
            one.display().to_string(),
        ],
        &services,
    )
    .expect("one-array guest succeeds through the same services");
    let abort = vm::cli::run_with_args(
        &runtime,
        &[
            "--fuel".to_owned(),
            "100000".to_owned(),
            throwing.display().to_string(),
        ],
        &services,
    )
    .expect_err("throwing guest must abort");
    assert!(
        abort.to_string().contains("factory abort marker"),
        "abort keeps the exact message: {abort}"
    );
    vm::cli::run_with_args(
        &runtime,
        &[
            "--fuel".to_owned(),
            "100000".to_owned(),
            one.display().to_string(),
        ],
        &services,
    )
    .expect("factory resets cleanly after quota refusal and abort");
}

#[test]
fn production_factory_reaches_init_with_source_facts_before_guest() {
    // Compiler-issued source facts must reach VM init before guest
    // execution on the selected route. This mirrors the production factory
    // composition exactly (same installs, same init call) with an observing
    // init; the unmodified factory itself drives every CLI subprocess test.
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let registry_catalog = Arc::clone(&catalog);
    let observed = Arc::new(AtomicBool::new(false));
    let during_init = Arc::clone(&observed);
    let services = vm::cli::CliHostServices::new(catalog, move || {
        let mut registry = vm::HostFunctionRegistry::new();
        pd_vm_compat_frontends::runtime::array::typescript_array_host_module()
            .install_from_catalog(&mut registry, &registry_catalog)?;
        pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module()
            .install_from_catalog(&mut registry, &registry_catalog)?;
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module()
            .install_from_catalog(&mut registry, &registry_catalog)?;
        pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module()
            .install_from_catalog(&mut registry, &registry_catalog)?;
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module()
            .install_from_catalog(&mut registry, &registry_catalog)?;
        for builtin in [
            "math::floor",
            "math::sqrt",
            "math::round",
            "math::powf",
            "math::is_nan",
            "math::is_finite",
        ] {
            registry.allow_builtin(builtin)?;
        }
        Ok(registry)
    })
    .with_capture_certification()
    .with_vm_init(move |machine| {
        during_init.store(
            machine
                .capture_certification_stats()
                .is_some_and(|stats| stats.valid),
            Ordering::SeqCst,
        );
        pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
            machine,
            pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        )
    });
    let scratch = ScratchTree::new();
    upstream_project(&scratch.0);
    let main = write_main(&scratch.0, BUBBLE_EXACT_CALLER);
    vm::cli::run_with_args(
        &selected_test_runtime(),
        &[
            "--fuel".to_owned(),
            "100000".to_owned(),
            main.display().to_string(),
        ],
        &services,
    )
    .expect("selected file/dependency import must execute");
    assert!(
        observed.load(Ordering::SeqCst),
        "production source facts must reach vm_init before guest execution"
    );
}

#[test]
fn live_owner_is_observed_in_a_real_callback_through_file_dependency_alias() {
    // A real host callback reached through the selected file/dependency +
    // import-alias path observes the actual owner state via the existing
    // public runtime query: one registered owner, no pending allocation,
    // valid source facts. CLI array success is therefore linked to the real
    // checked-owner registration callback, not merely to stats or to a
    // successful algorithm run. The probe composition mirrors production
    // exactly (same installs, same init call); accepted production handlers
    // are unchanged.
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use vm::{
        CallOutcome, CallReturn, HostApiBuilder, HostFunctionRegistry, HostFunctionSchema,
        HostParamSchema, HostTypeSchema, Value, Vm, VmError,
    };

    struct Invocation {
        initialized: AtomicBool,
        init_runs: AtomicUsize,
        observe_calls: AtomicUsize,
    }

    struct Observed {
        pending: usize,
        registered: usize,
        facts_valid: bool,
    }

    struct Observe {
        invocation: Arc<Invocation>,
        observed: Arc<Mutex<Option<Observed>>>,
    }

    impl vm::HostFunction for Observe {
        fn call(&mut self, machine: &mut Vm, args: &[Value]) -> vm::VmResult<CallOutcome> {
            if !args.is_empty() {
                return Err(VmError::HostError(
                    "probe::observe takes no arguments".to_owned(),
                ));
            }
            if !self.invocation.initialized.load(Ordering::SeqCst) {
                return Err(VmError::HostError(
                    "VM init did not precede probe::observe".to_owned(),
                ));
            }
            let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(machine)
                .expect("owner directory must exist inside the callback");
            let facts_valid = machine
                .capture_certification_stats()
                .is_some_and(|stats| stats.valid);
            *self.observed.lock().expect("record observation") = Some(Observed {
                pending: stats.pending,
                registered: stats.registered,
                facts_valid,
            });
            self.invocation.observe_calls.fetch_add(1, Ordering::SeqCst);
            Ok(CallOutcome::Return(CallReturn::one(Value::Null)))
        }
    }

    fn probe_catalog() -> Arc<vm::HostApiCatalog> {
        static CATALOG: OnceLock<Arc<vm::HostApiCatalog>> = OnceLock::new();
        Arc::clone(CATALOG.get_or_init(|| {
            let base =
                pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
            let mut builder = HostApiBuilder::new();
            for resource in base.resources() {
                builder.resource(resource.clone());
            }
            for function in base.functions() {
                builder.function(function.clone());
            }
            builder.function(HostFunctionSchema::with_return(
                "probe::observe",
                Vec::<HostParamSchema>::new(),
                HostTypeSchema::Null,
            ));
            Arc::new(builder.build().expect("probe catalog is valid"))
        }))
    }

    fn probe_compile_options() -> vm::CompileSourceFileOptions {
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(&pd_vm_compat_frontends::TYPESCRIPT_REFERENCE_ARRAY_PLUGIN)
            .with_source_plugin(&pd_vm_compat_frontends::LUA_PLUGIN)
            .with_host_api_catalog(probe_catalog())
    }

    fn probe_runtime() -> vm::cli::CliRuntime {
        vm::cli::CliRuntime {
            binary_name: "compat-ref-cli-probe",
            default_source: "unused.ts",
            compile_options: probe_compile_options,
        }
    }

    let catalog = probe_catalog();
    let observe_schema = vm::catalog_import_schemas(&catalog, "probe::observe")
        .into_iter()
        .next()
        .expect("probe schema is in the exact catalog");
    let invocation = Arc::new(Invocation {
        initialized: AtomicBool::new(false),
        init_runs: AtomicUsize::new(0),
        observe_calls: AtomicUsize::new(0),
    });
    let observed: Arc<Mutex<Option<Observed>>> = Arc::new(Mutex::new(None));
    let pending = Arc::new(Mutex::new(VecDeque::new()));
    let init_pending = Arc::clone(&pending);
    let registry_pending = Arc::clone(&pending);
    let registry_catalog = Arc::clone(&catalog);
    let host_invocation = Arc::clone(&invocation);
    let host_observed = Arc::clone(&observed);
    let init_facts = Arc::new(AtomicBool::new(false));
    let init_facts_writer = Arc::clone(&init_facts);
    let services = vm::cli::CliHostServices::new(catalog, move || {
        registry_pending
            .lock()
            .expect("registry invocation queue")
            .push_back(Arc::clone(&host_invocation));
        let callback_invocation = Arc::clone(&host_invocation);
        let callback_observed = Arc::clone(&host_observed);
        let mut registry = HostFunctionRegistry::new();
        pd_vm_compat_frontends::runtime::array::typescript_array_host_module()
            .install_from_catalog(&mut registry, &registry_catalog)?;
        pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module()
            .install_from_catalog(&mut registry, &registry_catalog)?;
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module()
            .install_from_catalog(&mut registry, &registry_catalog)?;
        pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module()
            .install_from_catalog(&mut registry, &registry_catalog)?;
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module()
            .install_from_catalog(&mut registry, &registry_catalog)?;
        for builtin in [
            "math::floor",
            "math::sqrt",
            "math::round",
            "math::powf",
            "math::is_nan",
            "math::is_finite",
        ] {
            registry.allow_builtin(builtin)?;
        }
        registry
            .register_catalog(observe_schema.clone(), move || {
                Box::new(Observe {
                    invocation: Arc::clone(&callback_invocation),
                    observed: Arc::clone(&callback_observed),
                })
            })
            .map_err(|error| VmError::HostError(error.to_string()))?;
        Ok(registry)
    })
    .with_capture_certification()
    .with_vm_init(move |machine| {
        let current = init_pending
            .lock()
            .expect("initializer invocation queue")
            .pop_front()
            .expect("registry factory created a per-VM invocation record");
        current.initialized.store(true, Ordering::SeqCst);
        current.init_runs.fetch_add(1, Ordering::SeqCst);
        init_facts_writer.store(
            machine
                .capture_certification_stats()
                .is_some_and(|stats| stats.valid),
            Ordering::SeqCst,
        );
        // The same production initialization after exact binding, before
        // any guest code.
        pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
            machine,
            pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
        )
    });

    let scratch = ScratchTree::new();
    upstream_project(&scratch.0);
    let main = write_main(
        &scratch.0,
        "import { bubbleSort as sort } from \"./sorts/bubble_sort.ts\";\nimport { observe } from \"probe\";\nconst a = [8, 3, 5, 1, 4, 2];\nconst b = a;\nconst sorted = sort(b);\nconst eq = sorted[0] === 1 && sorted[5] === 8 && a[0] === 1;\nobserve();\neq ? 1 : 0;\n",
    );
    vm::cli::run_with_args(
        &probe_runtime(),
        &[
            "--fuel".to_owned(),
            "100000".to_owned(),
            main.display().to_string(),
        ],
        &services,
    )
    .expect("file/dependency import with alias, sort and probe callback must run");
    assert!(invocation.initialized.load(Ordering::SeqCst));
    assert_eq!(invocation.init_runs.load(Ordering::SeqCst), 1);
    assert_eq!(invocation.observe_calls.load(Ordering::SeqCst), 1);
    assert!(
        init_facts.load(Ordering::SeqCst),
        "source facts must be valid at init"
    );
    let seen = observed
        .lock()
        .expect("read observation")
        .take()
        .expect("probe callback must have observed the owner state");
    assert_eq!(seen.pending, 0, "factory registers before returning");
    assert_eq!(
        seen.registered, 1,
        "one shared checked owner for the file-imported alias sort"
    );
    assert!(seen.facts_valid, "source facts stay valid in the callback");
}

#[test]
fn selected_array_vmbc_load_fails_safe_before_registration() {
    // Exact-binding array artifact without source facts: the owned-array
    // allocate gate refuses before any unauthorized registration or
    // collection. The failure is the owner/certification gate, never an
    // import/catalog setup refusal. Cleanup is checked independently.
    let scratch = ScratchTree::new();
    upstream_project(&scratch.0);
    let main = write_main(&scratch.0, BUBBLE_EXACT_CALLER);
    let artifact = scratch.0.join("array.vmbc");
    let emitted = run_selected(&[
        "--emit-vmbc".to_owned(),
        artifact.display().to_string(),
        "--fuel".to_owned(),
        "100000".to_owned(),
        main.display().to_string(),
    ]);
    assert!(
        emitted.status.success(),
        "array VMBC emission failed: {}",
        String::from_utf8_lossy(&emitted.stderr)
    );
    assert!(artifact.is_file(), "VMBC output exists");
    let loaded = run_selected(&[
        "--fuel".to_owned(),
        "100000".to_owned(),
        artifact.display().to_string(),
    ]);
    let stdout = String::from_utf8_lossy(&loaded.stdout);
    let stderr = String::from_utf8_lossy(&loaded.stderr);
    assert_eq!(loaded.status.code(), Some(1));
    assert!(
        stderr.contains("source-certified"),
        "array artifact must fail at the owner gate: {stderr}"
    );
    assert!(
        !stdout.contains("stack:"),
        "failed array load published a stack: {stdout}"
    );
    assert!(
        !stderr.contains("catalog") && !stderr.contains("host import"),
        "must not be an import/catalog setup refusal: {stderr}"
    );
    // Independent cleanup: a fresh selected run still succeeds, and the
    // whole scratch tree (sources plus artifact) is removed.
    let dir = scratch.0.clone();
    upstream_project(&dir);
    let retry = write_main(&dir, BUBBLE_EXACT_CALLER);
    assert_selected_int1(
        &run_selected_file(&retry),
        "fresh selected run after refusal",
    );
    drop(scratch);
    assert!(
        !dir.exists(),
        "scratch tree with sources and artifact removed"
    );
}

#[test]
fn selected_scalar_and_generator_vmbc_artifacts_execute_without_owner() {
    // Scalar and generator artifacts genuinely execute through selected
    // artifact loads without array-owner authority: factorial(6) = 720 and
    // the fibonacci triple agreement.
    let scratch = ScratchTree::new();
    upstream_project(&scratch.0);
    let fact_main = write_main(
        &scratch.0,
        "import { factorial as f } from \"./maths/factorial.ts\";\nconst r = f(6);\nr;\n",
    );
    let fact_artifact = scratch.0.join("fact.vmbc");
    assert!(
        run_selected(&[
            "--emit-vmbc".to_owned(),
            fact_artifact.display().to_string(),
            "--fuel".to_owned(),
            "100000".to_owned(),
            fact_main.display().to_string(),
        ])
        .status
        .success()
    );
    let fact_loaded = run_selected(&[
        "--fuel".to_owned(),
        "100000".to_owned(),
        fact_artifact.display().to_string(),
    ]);
    let fact_stdout = String::from_utf8_lossy(&fact_loaded.stdout);
    assert!(
        fact_loaded.status.success(),
        "scalar artifact load failed: {fact_stdout}"
    );
    assert!(
        fact_stdout.contains("720"),
        "scalar artifact result: {fact_stdout}"
    );
    let fib_main = write_main(
        &scratch.0,
        "import { nthFibonacci as fib, nthFibonacciRecursively as rec, nthFibonacciUsingFormula as fom } from \"./maths/fibonacci.ts\";\nconst ok = fib(10) === 55 && rec(10) === 55 && fom(10) === 55;\nok ? 1 : 0;\n",
    );
    let fib_artifact = scratch.0.join("fib.vmbc");
    assert!(
        run_selected(&[
            "--emit-vmbc".to_owned(),
            fib_artifact.display().to_string(),
            "--fuel".to_owned(),
            "100000".to_owned(),
            fib_main.display().to_string(),
        ])
        .status
        .success()
    );
    let fib_loaded = run_selected(&[
        "--fuel".to_owned(),
        "100000".to_owned(),
        fib_artifact.display().to_string(),
    ]);
    assert_selected_int1(&fib_loaded, "generator artifact load");
}

#[test]
fn selected_malformed_artifact_refusal_is_transactional() {
    // A truncated artifact is refused with a format diagnostic; the intact
    // artifact keeps its exact behavior afterwards (refusal poisons nothing).
    let scratch = ScratchTree::new();
    upstream_project(&scratch.0);
    let main = write_main(
        &scratch.0,
        "import { factorial as f } from \"./maths/factorial.ts\";\nconst r = f(6);\nr;\n",
    );
    let artifact = scratch.0.join("intact.vmbc");
    assert!(
        run_selected(&[
            "--emit-vmbc".to_owned(),
            artifact.display().to_string(),
            "--fuel".to_owned(),
            "100000".to_owned(),
            main.display().to_string(),
        ])
        .status
        .success()
    );
    let bytes = fs::read(&artifact).expect("read intact artifact");
    assert!(bytes.len() > 100, "artifact has substance to truncate");
    let truncated = scratch.0.join("truncated.vmbc");
    fs::write(&truncated, &bytes[..100]).expect("write truncated artifact");
    let refused = run_selected(&[
        "--fuel".to_owned(),
        "100000".to_owned(),
        truncated.display().to_string(),
    ]);
    let refused_stdout = String::from_utf8_lossy(&refused.stdout);
    let refused_stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success(),
        "truncated artifact must be refused: {refused_stdout}"
    );
    assert!(
        !refused_stdout.contains("stack:"),
        "refused artifact published a stack: {refused_stdout}"
    );
    assert!(
        !refused_stderr.is_empty(),
        "refusal must carry a diagnostic"
    );
    let again = run_selected(&[
        "--fuel".to_owned(),
        "100000".to_owned(),
        artifact.display().to_string(),
    ]);
    let again_stdout = String::from_utf8_lossy(&again.stdout);
    assert!(
        again.status.success() && again_stdout.contains("720"),
        "intact artifact unaffected by the refusal: {again_stdout}"
    );
}

#[test]
fn selected_repl_supports_scalar_and_rejects_array_declaration() {
    // The selected route retains the supported scalar REPL mode and
    // accurately rejects the unsupported selected TypeScript array
    // declaration form with a diagnostic instead of a new REPL design.
    let scalar = run_cli_with_stdin(&selected(&["--repl".to_owned()]), b"1 + 2\n");
    let scalar_out = String::from_utf8_lossy(&scalar.stdout);
    assert!(
        scalar.status.success(),
        "selected scalar REPL failed: {scalar_out}"
    );
    assert!(
        scalar_out.contains("=> 3"),
        "selected scalar REPL result: {scalar_out}"
    );
    let mixed = run_cli_with_stdin(
        &selected(&["--repl".to_owned()]),
        b"1 + 2\nconst a = [8, 3];\n1 + 2\n",
    );
    let mixed_out = String::from_utf8_lossy(&mixed.stdout);
    let mixed_err = String::from_utf8_lossy(&mixed.stderr);
    let combined = format!("{mixed_out}\n{mixed_err}");
    assert_eq!(
        mixed_out.matches("=> 3").count(),
        2,
        "REPL continues after the rejection: {combined}"
    );
    assert!(
        combined.contains("error"),
        "array declaration accurately rejected with a diagnostic: {combined}"
    );
}

#[test]
fn default_cli_regression_controls() {
    // The default route is byte-for-byte the old behavior: the shipped
    // TypeScript example still prints 42 and `--version` still works.
    let example = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/example.ts");
    let output = run_cli(&[
        "--fuel".to_owned(),
        "100000".to_owned(),
        example.display().to_string(),
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("42"),
        "default example.ts: {stdout}"
    );
    let version = run_cli(&["--version".to_owned()]);
    assert!(version.status.success());
    assert!(
        String::from_utf8_lossy(&version.stdout).contains("pd-vm-compat-run"),
        "default --version"
    );
}

fn assert_native_ssa(output: &Output, context: &str) {
    // Genuine native execution: SSA lowering plus a real dispatch count,
    // never a mere `--aot` success or interpreter fallback.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{context} native run failed: {stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("lowering=ssa"),
        "{context} is not SSA native (interpreter fallback?): {stdout}"
    );
    let count = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("aot executions: "))
        .and_then(|raw| raw.parse::<u64>().ok())
        .expect("actual native execution counter");
    assert!(
        count > 1,
        "{context} executed natively only {count} time(s): {stdout}"
    );
    eprintln!("{context}: native_count={count}");
}

/// Import-native codegen is slow (minutes per module graph); the bound
/// below reflects measured costs with headroom.
const NATIVE_BOUND: Duration = Duration::from_secs(1500);

fn run_selected_native(args: &[String]) -> Output {
    run_cli_bound(&selected(args), &[], NATIVE_BOUND)
}

#[test]
fn selected_factorial_import_runs_actual_native_ssa() {
    // Scalar/throw path through a normal importing caller, genuinely native.
    let scratch = ScratchTree::new();
    upstream_project(&scratch.0);
    let main = write_main(
        &scratch.0,
        "import { factorial as f } from \"./maths/factorial.ts\";\nconst r = f(6);\nr;\n",
    );
    let output = run_selected_native(&[
        "--aot".to_owned(),
        "--aot-dump".to_owned(),
        "--fuel".to_owned(),
        "100000".to_owned(),
        main.display().to_string(),
    ]);
    assert_native_ssa(&output, "selected factorial import");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("720"),
        "native factorial(6)"
    );
}

#[test]
fn selected_bubble_import_runs_actual_native_ssa() {
    // Array path through a normal importing caller, genuinely native.
    let scratch = ScratchTree::new();
    upstream_project(&scratch.0);
    let main = write_main(&scratch.0, BUBBLE_EXACT_CALLER);
    let output = run_selected_native(&[
        "--aot".to_owned(),
        "--aot-dump".to_owned(),
        "--fuel".to_owned(),
        "100000".to_owned(),
        main.display().to_string(),
    ]);
    assert_native_ssa(&output, "selected bubble import");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Int(1)"),
        "native exact-vector sortedness"
    );
}

#[test]
fn selected_fibonacci_import_runs_actual_native_ssa() {
    // Generator path through a normal importing caller, genuinely native.
    let scratch = ScratchTree::new();
    upstream_project(&scratch.0);
    let main = write_main(
        &scratch.0,
        "import { nthFibonacci as fib, nthFibonacciRecursively as rec, nthFibonacciUsingFormula as fom } from \"./maths/fibonacci.ts\";\nconst a10 = fib(10) === 55 && rec(10) === 55 && fom(10) === 55;\nconst a20 = fib(20) === 6765 && rec(20) === 6765 && fom(20) === 6765;\nconst cross = fib(10) === rec(10) && rec(10) === fom(10);\nconst ok = a10 && a20 && cross;\nok ? 1 : 0;\n",
    );
    let output = run_selected_native(&[
        "--aot".to_owned(),
        "--aot-dump".to_owned(),
        "--fuel".to_owned(),
        "100000".to_owned(),
        main.display().to_string(),
    ]);
    assert_native_ssa(&output, "selected fibonacci import");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Int(1)"),
        "native export agreement"
    );
}

#[test]
fn selected_quicksort_import_runs_actual_native_ssa() {
    // Random-pivot path through a normal importing caller, genuinely
    // native; sortedness only, never pivot choices.
    let scratch = ScratchTree::new();
    upstream_project(&scratch.0);
    let main = write_main(
        &scratch.0,
        "import { QuickSort } from \"./sorts/quick_sort.ts\";\nconst input = [8, 3, 5, 1, 4, 2];\nconst sorted = QuickSort(input);\nconst eq = sorted[0] === 1 && sorted[1] === 2 && sorted[2] === 3 && sorted[3] === 4 && sorted[4] === 5 && sorted[5] === 8;\nconst ok = eq && sorted === input;\nok ? 1 : 0;\n",
    );
    let output = run_selected_native(&[
        "--aot".to_owned(),
        "--aot-dump".to_owned(),
        "--fuel".to_owned(),
        "100000".to_owned(),
        main.display().to_string(),
    ]);
    assert_native_ssa(&output, "selected quicksort import");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Int(1)"),
        "native quicksort exact vector"
    );
}

#[test]
fn selected_array_aot_companion_and_standalone_refuse_safely() {
    // Source-companion `--aot-load` and source-deleted fresh-process
    // standalone AOT under the exact selected catalog: absent/revoked facts
    // surface as the actual owner Unavailable path (the allocate gate),
    // with cleanup independently checked. Single-file shape keeps the
    // one-time codegen near a minute; import-file AOT behavior is covered
    // by the native tests above.
    let scratch = ScratchTree::new();
    let source = format!(
        "{BUBBLE_SORT_FIXTURE_STR}\nconst s = bubbleSort([8, 3, 5, 1, 4, 2]);\nconst eq = s[0] === 1 && s[5] === 8;\neq ? 1 : 0;\n"
    );
    let main = scratch.0.join("inline_array.ts");
    fs::write(&main, &source).expect("write inline array program");
    let artifact = scratch.0.join("array.pat");
    let saved = run_cli_bound(
        &selected(&[
            "--aot-save".to_owned(),
            artifact.display().to_string(),
            "--fuel".to_owned(),
            "100000".to_owned(),
            main.display().to_string(),
        ]),
        &[],
        Duration::from_secs(900),
    );
    let saved_stdout = String::from_utf8_lossy(&saved.stdout);
    assert!(
        saved.status.success() && saved_stdout.contains("Int(1)"),
        "array aot-save failed: {saved_stdout}"
    );
    assert!(artifact.is_file(), "native artifact was saved");
    let companion = run_selected(&[
        "--aot-load".to_owned(),
        artifact.display().to_string(),
        "--fuel".to_owned(),
        "100000".to_owned(),
        main.display().to_string(),
    ]);
    let companion_stdout = String::from_utf8_lossy(&companion.stdout);
    let companion_stderr = String::from_utf8_lossy(&companion.stderr);
    assert_eq!(
        companion.status.code(),
        Some(1),
        "companion array AOT must refuse safely"
    );
    assert!(
        companion_stderr.contains("source-certified"),
        "companion must fail at the owner gate: {companion_stderr}"
    );
    assert!(
        !companion_stdout.contains("stack:"),
        "companion published a stack: {companion_stdout}"
    );
    fs::remove_file(&main).expect("delete sources before standalone load");
    let standalone = run_selected(&[
        "--aot-load".to_owned(),
        artifact.display().to_string(),
        "--fuel".to_owned(),
        "100000".to_owned(),
    ]);
    let standalone_stdout = String::from_utf8_lossy(&standalone.stdout);
    let standalone_stderr = String::from_utf8_lossy(&standalone.stderr);
    assert_eq!(standalone.status.code(), Some(1));
    assert!(
        standalone_stderr.contains("source-certified"),
        "standalone must fail at the owner gate: {standalone_stderr}"
    );
    assert!(
        !standalone_stdout.contains("stack:"),
        "standalone published a stack: {standalone_stdout}"
    );
    let dir = scratch.0.clone();
    drop(scratch);
    assert!(!dir.exists(), "artifact scratch tree removed");
}
