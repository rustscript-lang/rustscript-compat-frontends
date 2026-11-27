//! Selected pinned legacy Test262 bodies with only a throwing $ERROR adapter.
//! This is not the complete Test262 harness or a whole-suite conformance claim.
use pd_vm_compat_frontends::javascript::corpus::{
    CorpusCompletion, CorpusFileError, observe_javascript_es5_corpus_file,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

const PREFIX: &str = "function $ERROR(message) { \"use strict\"; throw message; }\n";
const SUFFIX: &str = "\n;1;\n";
const ENCODE_HASH: &str = include_str!("fixtures/test262104/S15.1.3.4_A3.3_T1.js");

struct Case {
    id: &'static str,
    upstream_path: &'static str,
    local_path: &'static str,
    body: &'static str,
    bytes: usize,
}

const CASES: [Case; 8] = [
    Case {
        id: "S15.1.3.4_A3.3_T1",
        upstream_path: "test/suite/ch15/15.1/15.1.3/15.1.3.4/S15.1.3.4_A3.3_T1.js",
        local_path: "tests/fixtures/test262104/S15.1.3.4_A3.3_T1.js",
        body: ENCODE_HASH,
        bytes: 407,
    },
    Case {
        id: "S15.1.3.4_A4_T1",
        upstream_path: "test/suite/ch15/15.1/15.1.3/15.1.3.4/S15.1.3.4_A4_T1.js",
        local_path: "tests/fixtures/test262104/S15.1.3.4_A4_T1.js",
        body: include_str!("fixtures/test262104/S15.1.3.4_A4_T1.js"),
        bytes: 743,
    },
    Case {
        id: "S15.1.3.4_A4_T3",
        upstream_path: "test/suite/ch15/15.1/15.1.3/15.1.3.4/S15.1.3.4_A4_T3.js",
        local_path: "tests/fixtures/test262104/S15.1.3.4_A4_T3.js",
        body: include_str!("fixtures/test262104/S15.1.3.4_A4_T3.js"),
        bytes: 1230,
    },
    Case {
        id: "S15.1.3.4_A4_T4",
        upstream_path: "test/suite/ch15/15.1/15.1.3/15.1.3.4/S15.1.3.4_A4_T4.js",
        local_path: "tests/fixtures/test262104/S15.1.3.4_A4_T4.js",
        body: include_str!("fixtures/test262104/S15.1.3.4_A4_T4.js"),
        bytes: 994,
    },
    Case {
        id: "S15.1.3.2_A4_T1",
        upstream_path: "test/suite/ch15/15.1/15.1.3/15.1.3.2/S15.1.3.2_A4_T1.js",
        local_path: "tests/fixtures/test262104/S15.1.3.2_A4_T1.js",
        body: include_str!("fixtures/test262104/S15.1.3.2_A4_T1.js"),
        bytes: 799,
    },
    Case {
        id: "S15.1.3.2_A4_T2",
        upstream_path: "test/suite/ch15/15.1/15.1.3/15.1.3.2/S15.1.3.2_A4_T2.js",
        local_path: "tests/fixtures/test262104/S15.1.3.2_A4_T2.js",
        body: include_str!("fixtures/test262104/S15.1.3.2_A4_T2.js"),
        bytes: 1104,
    },
    Case {
        id: "S15.1.3.2_A4_T3",
        upstream_path: "test/suite/ch15/15.1/15.1.3/15.1.3.2/S15.1.3.2_A4_T3.js",
        local_path: "tests/fixtures/test262104/S15.1.3.2_A4_T3.js",
        body: include_str!("fixtures/test262104/S15.1.3.2_A4_T3.js"),
        bytes: 830,
    },
    Case {
        id: "S15.1.3.2_A4_T4",
        upstream_path: "test/suite/ch15/15.1/15.1.3/15.1.3.2/S15.1.3.2_A4_T4.js",
        local_path: "tests/fixtures/test262104/S15.1.3.2_A4_T4.js",
        body: include_str!("fixtures/test262104/S15.1.3.2_A4_T4.js"),
        bytes: 988,
    },
];

fn wrapper(body: &str) -> String {
    format!("{PREFIX}{body}{SUFFIX}")
}

struct Subject(PathBuf);

impl Subject {
    fn new(id: &str, source: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "test262104-{id}-{}-{}.js",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("owned unique .js subject");
        file.write_all(source.as_bytes()).unwrap();
        eprintln!(
            "TEST262104_SUBJECT {}",
            serde_json::json!({"id":id,"path":path,"source":source})
        );
        Self(path)
    }
}

impl Drop for Subject {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).expect("retire owned subject file");
        eprintln!(
            "TEST262104_CLEANUP {}",
            serde_json::json!({"path":self.0,"removed":!self.0.exists()})
        );
    }
}

fn observe(id: &'static str, source: String) -> Result<CorpusCompletion, CorpusFileError> {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let subject = Subject::new(id, &source);
            let result = observe_javascript_es5_corpus_file(&subject.0);
            eprintln!(
                "TEST262104_RESULT {}",
                serde_json::json!({"id":id,"path":subject.0,"debug":format!("{result:?}")})
            );
            result
        })
        .unwrap()
        .join()
        .unwrap()
}

fn positive(id: &'static str, body: &str) {
    let observed = observe(id, wrapper(body)).expect("original assertions must finish");
    assert_eq!(observed.cookie, 1);
    assert_eq!(observed.aot_exec_count, 0);
    assert_eq!(observed.jit_native_exec_count, 0);
    assert!(observed.certification_valid_before_reset);
    assert!(!observed.certification_valid_after_reset);
    assert_eq!(observed.resources_after_reset, 0);
    assert!(observed.owned_arrays_retired);
    eprintln!(
        "TEST262104_POSITIVE {}",
        serde_json::json!({
            "id":id,"cookie":observed.cookie,"aot_exec_count":observed.aot_exec_count,
            "jit_native_exec_count":observed.jit_native_exec_count,
            "certification_valid_before_reset":observed.certification_valid_before_reset,
            "certification_valid_after_reset":observed.certification_valid_after_reset,
            "resources_before_reset":observed.resources_before_reset,
            "resources_after_reset":observed.resources_after_reset,
            "owned_arrays_retired":observed.owned_arrays_retired
        })
    );
}

fn negative(id: &'static str, body: &str) {
    let error = observe(id, wrapper(body)).expect_err("throw must precede success cookie");
    // reference.rs::emit_uncaught_script_throw sends this fixed message through
    // typescript::throw_error. Guest string payloads are not exposed by this API.
    // An Execution error alone would also admit setup/fuel/bind failures.
    match error {
        CorpusFileError::Execution { error, rendered } => {
            assert!(
                matches!(error.as_ref(), vm::VmError::HostError(message) if message == "uncaught JavaScript throw"),
                "expected the exact uncaught guest throw path, got {error:?}"
            );
            eprintln!(
                "TEST262104_NEGATIVE {}",
                serde_json::json!({"id":id,"variant":"Execution","vm_error":format!("{error:?}"),"rendered":rendered,"cookie_returned":false,"guest_payload_exposed":false})
            );
        }
        other => panic!("expected runtime uncaught throw, got {other:?}"),
    }
}

fn mutate_expected(body: &str, old: &str, new: &str) -> String {
    assert_eq!(body.matches(old).count(), 1, "one expected RHS only");
    assert!(!body.contains(new), "new RHS must be distinct");
    let mutated = body.replacen(old, new, 1);
    assert_eq!(mutated.replacen(new, old, 1), body);
    eprintln!(
        "TEST262104_MUTATION {}",
        serde_json::json!({"original_body":body,"mutant_body":mutated,"old":old,"new":new,"single_replacement":true})
    );
    mutated
}

#[test]
fn test262104_error_trap() {
    negative("error_trap", "$ERROR(\"TEST262104_FAIL_SENTINEL\");");
}

#[test]
fn test262104_encode_hash() {
    positive(CASES[0].id, CASES[0].body);
}

#[test]
fn test262104_encode_ascii() {
    positive(CASES[1].id, CASES[1].body);
}

#[test]
fn test262104_encode_line_terminators() {
    positive(CASES[2].id, CASES[2].body);
}

#[test]
fn test262104_encode_urls() {
    positive(CASES[3].id, CASES[3].body);
}

#[test]
fn test262104_decode_ascii() {
    positive(CASES[4].id, CASES[4].body);
}

#[test]
fn test262104_decode_cyrillic() {
    positive(CASES[5].id, CASES[5].body);
}

#[test]
fn test262104_decode_line_terminators() {
    positive(CASES[6].id, CASES[6].body);
}

#[test]
fn test262104_decode_urls() {
    positive(CASES[7].id, CASES[7].body);
}

#[test]
fn test262104_mutant_encode() {
    let body = mutate_expected(ENCODE_HASH, "!== \"%23\"", "!== \"%24\"");
    negative("mutant_encode", &body);
}

#[test]
fn test262104_mutant_decode() {
    let body = mutate_expected(
        CASES[4].body,
        "!== \"ABCDEFGHIJKLMNOPQRSTUVWXYZ\"",
        "!== \"ABCDEFGHIJKLMNOPQRSTUVWXY!\"",
    );
    negative("mutant_decode", &body);
}

#[test]
fn test262104_fixture_integrity() {
    let provenance: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/test262104/upstream.json")).unwrap();
    assert_eq!(provenance["repository"], "https://github.com/tc39/test262");
    assert_eq!(
        provenance["commit"],
        "7da91bceb9ce7613f87db47ddd1292a2dda58b42"
    );
    assert_eq!(provenance["adapter_prefix"], PREFIX);
    assert_eq!(provenance["completion_suffix"], SUFFIX);
    assert_eq!(provenance["strictness"], "original sloppy Script unchanged");
    assert_eq!(provenance["full_test262_harness"], false);
    let entries = provenance["cases"].as_array().unwrap();
    assert_eq!(entries.len(), CASES.len());
    let mut ids = std::collections::BTreeSet::new();
    for (case, entry) in CASES.iter().zip(entries) {
        assert!(ids.insert(case.id));
        assert_eq!(entry["id"], case.id);
        assert_eq!(entry["upstream_path"], case.upstream_path);
        assert_eq!(entry["local_path"], case.local_path);
        assert_eq!(entry["bytes"], case.bytes);
        assert_eq!(case.body.len(), case.bytes);
        assert_eq!(
            std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(case.local_path)).unwrap(),
            case.body.as_bytes()
        );
        let source = wrapper(case.body);
        assert_eq!(source.matches(case.body).count(), 1);
        assert_eq!(
            source.strip_prefix(PREFIX).unwrap().strip_suffix(SUFFIX),
            Some(case.body)
        );
        assert!(
            case.body
                .starts_with("// Copyright 2009 the Sputnik authors.")
        );
        assert!(!case.body.contains("use strict"));
        assert_eq!(source.matches("\"use strict\"").count(), 1);
    }
    let license_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/test262104/LICENSE");
    assert_eq!(
        std::fs::read(license_path).unwrap(),
        include_bytes!("fixtures/test262104/LICENSE")
    );
    let _ = mutate_expected(ENCODE_HASH, "!== \"%23\"", "!== \"%24\"");
    let _ = mutate_expected(
        CASES[4].body,
        "!== \"ABCDEFGHIJKLMNOPQRSTUVWXYZ\"",
        "!== \"ABCDEFGHIJKLMNOPQRSTUVWXY!\"",
    );
    eprintln!("TEST262104_INTEGRITY host_only=true original_bodies=8 guest_invocations=0");
}
