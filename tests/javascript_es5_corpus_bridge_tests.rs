//! File-certified ES5 corpus completion bridge.
//! The observer must evaluate a real `.js` file and print only `Int(cookie)`.
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use pd_vm_compat_frontends::javascript::corpus::{
    CorpusFileError, observe_javascript_es5_corpus_file,
};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let base = PathBuf::from(std::env::var_os("TMPDIR").expect("assigned TMPDIR"));
        let path = base.join(format!(
            "p11-bridge-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn source(&self, name: &str, source: &str) -> String {
        let path = self.0.join(name);
        std::fs::write(&path, source).unwrap();
        path.to_str().unwrap().to_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn corpus() -> Command {
    Command::new(env!("CARGO_BIN_EXE_pd-vm-es5-corpus-run"))
}

fn run(args: &[&str]) -> Output {
    let result = corpus().args(args).stdin(Stdio::null()).output().unwrap();
    eprintln!(
        "P11_BRIDGE args={args:?} rc={:?}\nstdout={}\nstderr={}",
        result.status.code(),
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    result
}

fn assert_cookie(result: &Output, cookie: i64) {
    assert!(
        result.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, format!("Int({cookie})\n").into_bytes());
    assert!(
        result.stderr.is_empty(),
        "stderr={}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn assert_refused(result: &Output) {
    assert!(!result.status.success());
    assert!(
        result.stdout.is_empty(),
        "positive protocol stdout={}",
        String::from_utf8_lossy(&result.stdout)
    );
}

#[test]
fn literal_computed_and_closed_function_cookies_are_evaluated() {
    let scratch = Scratch::new();
    let literal = scratch.source("literal.js", "268435456;");
    let observed = observe_javascript_es5_corpus_file(literal.as_ref()).unwrap();
    assert_eq!(observed.cookie, 268435456);
    assert_eq!(observed.aot_exec_count, 0);
    assert_eq!(observed.jit_native_exec_count, 0);
    assert!(observed.certification_valid_before_reset);
    assert!(!observed.certification_valid_after_reset);
    assert_eq!(observed.resources_after_reset, 0);
    assert!(observed.owned_arrays_retired);
    assert_cookie(&run(&["--javascript-es5", &literal]), 268435456);

    let computed = scratch.source("computed.js", "var n=1000*1000+42;n;");
    let observed = observe_javascript_es5_corpus_file(computed.as_ref()).unwrap();
    assert_eq!(observed.cookie, 1_000_042);
    assert_eq!(observed.aot_exec_count, 0);
    assert_eq!(observed.jit_native_exec_count, 0);
    assert_cookie(&run(&["--javascript-es5", &computed]), 1_000_042);

    let function = scratch.source(
        "closed.js",
        "var f=function(){\"use strict\";return 30*30+13;};f();",
    );
    let observed = observe_javascript_es5_corpus_file(function.as_ref()).unwrap();
    assert_eq!(observed.cookie, 913);
    assert_cookie(&run(&["--javascript-es5", &function]), 913);
}

#[test]
fn source_int_token_and_expected_cookie_argument_are_not_authority() {
    let scratch = Scratch::new();
    let file = scratch.source("decoy.js", "var label=\"Int(999999)\";17;");
    let observed = observe_javascript_es5_corpus_file(file.as_ref()).unwrap();
    assert_eq!(observed.cookie, 17);
    assert_cookie(&run(&["--javascript-es5", &file]), 17);
    assert_refused(&run(&["--javascript-es5", &file, "17"]));
    assert_refused(&run(&["--javascript-es5", &file, "Int(17)"]));
}

#[test]
fn bool_string_object_spoof_fraction_and_range_refuse() {
    let scratch = Scratch::new();
    for (name, source) in [
        ("bool.js", "true;"),
        ("string.js", "\"Int(42)\";"),
        ("looking.js", "\"42\";"),
        (
            "spoof.js",
            "var o={tag:3,number:42,flag:false,ref_slot:0,generation:0};o;",
        ),
        ("fraction.js", "1.5;"),
        ("zero.js", "0;"),
        ("negative.js", "-1;"),
        ("limit.js", "1073741824;"),
        ("nan.js", "0/0;"),
        ("inf.js", "1/0;"),
        (
            "undefined.js",
            "var f=function(){\"use strict\";return;};f();",
        ),
    ] {
        let file = scratch.source(name, source);
        assert!(
            observe_javascript_es5_corpus_file(file.as_ref()).is_err(),
            "{name} must not become a cookie"
        );
        assert_refused(&run(&["--javascript-es5", &file]));
    }
}

#[test]
fn object_allocation_is_reset_on_the_same_run_before_print() {
    let scratch = Scratch::new();
    let file = scratch.source("owned.js", "var o={};42;");
    let observed = observe_javascript_es5_corpus_file(file.as_ref()).unwrap();
    assert_eq!(observed.cookie, 42);
    assert!(observed.resources_before_reset > 0);
    assert_eq!(observed.resources_after_reset, 0);
    assert!(observed.certification_valid_before_reset);
    assert!(!observed.certification_valid_after_reset);
    assert!(observed.owned_arrays_retired);
    assert_eq!(observed.aot_exec_count, 0);
    assert_eq!(observed.jit_native_exec_count, 0);
    assert_cookie(&run(&["--javascript-es5", &file]), 42);
}

#[test]
fn rss_vmbc_and_compile_error_keep_typed_context_without_stdout() {
    let scratch = Scratch::new();
    let rss = scratch.source("control.rss", "let n: int = 6 * 7; n;");
    match observe_javascript_es5_corpus_file(rss.as_ref()) {
        Err(CorpusFileError::Source { error, .. }) => {
            let vm::SourcePathError::SourceWithMap {
                error: vm::SourceError::Parse(parse),
                sources,
            } = *error
            else {
                panic!("mapped RSS refusal");
            };
            assert_eq!(
                parse.message,
                "JavaScript ES5 does not accept RustScript source"
            );
            let span = parse.span.unwrap();
            assert_eq!(
                &sources.source(span.source_id).unwrap()[span.lo..span.hi],
                "let n: int = 6 * 7; n;"
            );
            assert_eq!(sources.file(span.source_id).unwrap().name, rss);
        }
        other => panic!("typed RSS refusal, got {other:?}"),
    }
    let rss_run = run(&["--javascript-es5", &rss]);
    assert_refused(&rss_run);
    let rss_error = String::from_utf8_lossy(&rss_run.stderr);
    assert!(
        rss_error.contains("JavaScript ES5 does not accept RustScript source"),
        "{rss_error}"
    );
    assert!(rss_error.contains("control.rss"), "{rss_error}");

    let source = scratch.source("emit.js", "1;");
    let artifact = scratch.0.join("explicit.vmbc");
    let emitted = Command::new(env!("CARGO_BIN_EXE_pd-vm-compat-run"))
        .args([
            "--javascript-es5",
            "--emit-vmbc",
            artifact.to_str().unwrap(),
            &source,
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(emitted.status.success(), "ordinary emit remains available");
    assert!(artifact.is_file());
    match observe_javascript_es5_corpus_file(&artifact) {
        Err(CorpusFileError::Source { error, .. }) => {
            assert!(matches!(
                error.as_ref(),
                vm::SourcePathError::UnsupportedExtension(extension) if extension == "vmbc"
            ));
        }
        other => panic!("source-only VMBC refusal, got {other:?}"),
    }
    assert_refused(&run(&["--javascript-es5", artifact.to_str().unwrap()]));

    let bad = scratch.source("bad.js", "var text='ok';\nlet forbidden=1;");
    match observe_javascript_es5_corpus_file(bad.as_ref()) {
        Err(CorpusFileError::Source { error, .. }) => {
            let rendered = error.to_string();
            let vm::SourcePathError::SourceWithMap {
                error: vm::SourceError::Parse(parse),
                sources,
            } = *error
            else {
                panic!("mapped parse error: {rendered}");
            };
            let span = parse.span.unwrap();
            assert_eq!(
                &sources.source(span.source_id).unwrap()[span.lo..span.hi],
                "let forbidden=1;"
            );
            assert!(
                sources
                    .file(span.source_id)
                    .unwrap()
                    .name
                    .ends_with("bad.js")
            );
        }
        other => panic!("typed compile error, got {other:?}"),
    }
    let bad_run = run(&["--javascript-es5", &bad]);
    assert_refused(&bad_run);
    let bad_error = String::from_utf8_lossy(&bad_run.stderr);
    assert!(bad_error.contains("bad.js"), "{bad_error}");
    assert!(bad_error.contains("let forbidden=1;"), "{bad_error}");
}

#[test]
fn throw_and_fuel_yield_are_not_success() {
    let scratch = Scratch::new();
    let thrown = scratch.source("throw.js", "throw 7;");
    assert!(observe_javascript_es5_corpus_file(thrown.as_ref()).is_err());
    let thrown_run = run(&["--javascript-es5", &thrown]);
    assert_refused(&thrown_run);
    assert!(String::from_utf8_lossy(&thrown_run.stderr).contains("uncaught JavaScript throw"));

    let looping = scratch.source("fuel.js", "while(true){}");
    let error = observe_javascript_es5_corpus_file(looping.as_ref()).unwrap_err();
    let rendered = error.to_string();
    assert!(
        rendered.to_ascii_lowercase().contains("fuel"),
        "fuel control must be an execution limit, got {rendered}"
    );
    let looping_run = run(&["--javascript-es5", &looping]);
    assert_refused(&looping_run);
    assert!(
        String::from_utf8_lossy(&looping_run.stderr)
            .to_ascii_lowercase()
            .contains("fuel"),
        "stderr={}",
        String::from_utf8_lossy(&looping_run.stderr)
    );
}

#[test]
fn selector_is_exact_and_other_requests_are_refused() {
    let scratch = Scratch::new();
    let file = scratch.source("only.js", "5;");
    assert_cookie(&run(&["--javascript-es5", &file]), 5);
    for args in [
        &[][..],
        &["--javascript-es5"][..],
        &["--javascript-es5", "repl"][..],
        &["--javascript-es5", "--repl"][..],
        &["only.js"][..],
        &["--fuel", "10000000", "--javascript-es5", "only.js"][..],
        &["--javascript-es5", "--fuel", "10000000", "only.js"][..],
        &["--javascript-es5", "--aot-load", "only.js"][..],
        &["--javascript-es5", "--emit-vmbc", "out.vmbc", "only.js"][..],
        &["--javascript-es5", "--jit-dump", "only.js"][..],
        &["--javascript-es5", "--native", "only.js"][..],
    ] {
        let owned: Vec<&str> = args
            .iter()
            .map(|argument| {
                if *argument == "only.js" {
                    file.as_str()
                } else {
                    *argument
                }
            })
            .collect();
        assert_refused(&run(&owned));
    }
}
