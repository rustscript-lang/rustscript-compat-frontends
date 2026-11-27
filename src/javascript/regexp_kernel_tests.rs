//! Executed private matcher-kernel tests, not public JS RegExp object tests.
use super::*;
use std::sync::Arc;
use vm::{Expr, HostFunction, HostFunctionRegistry, SourcePlugin, Value};

fn worker(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}
struct Kernel {
    pattern: Vec<u16>,
    flags: String,
    start: i64,
    budget: i64,
    scratch: i64,
}
impl SourcePlugin for Kernel {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("es5-regexp-kernel-test"),
            extensions: &["rxkernel"],
            typing_policy: vm::FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }
    fn parse_source(
        &self,
        _: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<FrontendIr, vm::ParseError> {
        let mut ir = FrontendIr {
            stmts: vec![],
            locals: 0,
            local_bindings: vec![],
            struct_schemas: Default::default(),
            unknown_type_spans: vec![],
            functions: vec![],
            function_impls: Default::default(),
            stmt_sources: vec![],
            function_sources: Default::default(),
            use_declarations: vec![],
            implicit_extern_names: vec![],
            host_api_metadata: None,
            semantic_index: None,
            parsed_semantic_index: None,
            catalog_visibility: None,
            lexer_tokens: vec![],
        };
        let input = context.install_host_function(&mut ir, "rx_test::subject", 0)?;
        // An existing typed caller slot must survive private helper emission.
        ir.locals = 1;
        ir.stmts.push(vm::Stmt::Let {
            index: 0,
            declared_schema: Some(vm::TypeSchema::Bool),
            expr: Expr::Bool(true),
            line: 1,
        });
        let matcher = append_matcher(&mut ir, &self.pattern, &self.flags)
            .map_err(|e| vm::ParseError::new(e.to_string()))?;
        // Inspect the real producer, not a hand-built capture-only carrier.
        let decl = ir.functions.iter().find(|d| d.index == matcher).unwrap();
        assert_eq!(
            decl.arg_schemas,
            vec![
                Some(vm::TypeSchema::Array(Box::new(vm::TypeSchema::Int))),
                Some(vm::TypeSchema::Int),
                Some(vm::TypeSchema::Int),
                Some(vm::TypeSchema::Int)
            ]
        );
        assert_eq!(
            decl.return_schema,
            Some(vm::TypeSchema::Named(
                "__Es5RegExpKernelResult".into(),
                vec![]
            ))
        );
        ir.stmts.push(vm::Stmt::Expr {
            expr: Expr::Var(0),
            line: 1,
        });
        ir.stmts.push(vm::Stmt::Expr {
            expr: Expr::Call(
                matcher,
                vec![],
                vec![
                    Expr::Call(input, vec![], vec![], None, None),
                    Expr::Int(self.start),
                    Expr::Int(self.budget),
                    Expr::Int(self.scratch),
                ],
                None,
                None,
            ),
            line: 1,
        });
        Ok(ir)
    }
}
fn schema() -> vm::HostFunctionSchema {
    vm::HostFunctionSchema::with_return(
        "rx_test::subject",
        vec![],
        vm::HostTypeSchema::Array(Box::new(vm::HostTypeSchema::Int)),
    )
}
struct Subject(Vec<i64>);
impl HostFunction for Subject {
    fn call(&mut self, _: &mut vm::Vm, _: &[Value]) -> vm::VmResult<vm::CallOutcome> {
        Ok(vm::CallOutcome::Return(vm::CallReturn::One(Value::array(
            self.0.iter().copied().map(Value::Int).collect(),
        ))))
    }
}
fn compile_limits(
    pattern: Vec<u16>,
    flags: &str,
    start: i64,
    budget: i64,
    scratch: i64,
) -> (vm::Program, Arc<vm::HostApiCatalog>) {
    let plugin = Box::leak(Box::new(Kernel {
        pattern,
        flags: flags.into(),
        start,
        budget,
        scratch,
    }));
    let mut builder = vm::HostApiBuilder::new();
    builder.function(schema());
    let catalog = Arc::new(builder.build().unwrap());
    let compiled = vm::compile_source_with_flavor_and_options(
        "private runtime subject",
        vm::SourceFlavor::Plugin(vm::FrontendId::new("es5-regexp-kernel-test")),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(plugin)
            .with_host_api_catalog(catalog.clone()),
    )
    .unwrap();
    (compiled.program, catalog)
}
fn compile(pattern: &str, flags: &str) -> (vm::Program, Arc<vm::HostApiCatalog>) {
    compile_limits(
        pattern.encode_utf16().collect(),
        flags,
        0,
        TRANSITION_LIMIT,
        SCRATCH_LIMIT,
    )
}
fn run_int(program: &vm::Program, catalog: &vm::HostApiCatalog, subject: Vec<i64>) -> Value {
    let mut machine = vm::Vm::new(program.clone());
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..Default::default()
    });
    machine.set_fuel(10_000_000);
    let mut registry = HostFunctionRegistry::new();
    registry
        .register_catalog(
            vm::host_api::HostImportSchema::from_function(catalog, &schema()),
            move || Box::new(Subject(subject.clone())),
        )
        .unwrap();
    registry.bind_vm_cached(&mut machine).unwrap();
    let outcome = machine.run();
    assert_eq!(
        outcome.unwrap_or_else(|e| panic!(
            "guest error {e:?} ip={} stack={:?}",
            machine.ip(),
            machine.stack()
        )),
        vm::VmStatus::Halted
    );
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.stack().len(), 2);
    assert_eq!(
        machine.stack()[0],
        Value::Bool(true),
        "preexisting caller Bool slot"
    );
    let result = machine.stack().last().unwrap().clone();
    assert_eq!(machine.host_context().resource_count(), 0);
    machine.reset_for_reuse().unwrap();
    assert_eq!(machine.host_context().resource_count(), 0);
    result
}
fn run(program: &vm::Program, catalog: &vm::HostApiCatalog, subject: &[u16]) -> Value {
    run_int(
        program,
        catalog,
        subject.iter().map(|&u| i64::from(u)).collect(),
    )
}
fn field(v: &Value, key: &str) -> Value {
    let Value::Map(m) = v else {
        panic!("typed result: {v:?}")
    };
    m.get(&Value::string(key)).unwrap().clone()
}
fn integer(v: &Value, key: &str) -> i64 {
    let Value::Int(x) = field(v, key) else {
        panic!("Int field {key}")
    };
    x
}
fn pairs(v: &Value) -> Vec<i64> {
    field(v, "captures")
        .into_owned_array()
        .unwrap()
        .into_iter()
        .map(|v| {
            let Value::Int(x) = v else {
                panic!("Int capture")
            };
            x
        })
        .collect()
}
#[test]
fn p9_literal_runtime_varied_subjects() {
    worker(|| {
        let (p, c) = compile("ab", "g");
        assert_eq!(
            integer(
                &run(&p, &c, &"zzab!".encode_utf16().collect::<Vec<_>>()),
                "start"
            ),
            2
        );
        assert_eq!(
            integer(
                &run(&p, &c, &"zzac!".encode_utf16().collect::<Vec<_>>()),
                "start"
            ),
            -1
        );
    });
}
#[test]
fn p9_captures_backtrack_in_actual_guest() {
    worker(|| {
        let (p, c) = compile("(a|ab)c", "");
        let r = run(&p, &c, &[97, 98, 99]);
        assert_eq!(integer(&r, "status"), 1);
        assert_eq!(pairs(&r), vec![0, 3, 0, 2]);
    });
}
#[test]
fn p9_node_spec_capture_vectors() {
    worker(|| {
        let fixtures =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/regexp");
        let out = std::process::Command::new("node")
            .arg(fixtures.join("oracle.js"))
            .arg(fixtures.join("vectors.json"))
            .output()
            .unwrap();
        eprintln!(
            "P9_NODE rc={} stdout={} stderr={}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success());
        let raw: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let vectors = raw["vectors"].as_array().unwrap();
        assert_eq!(vectors.len(), 54);
        for row in vectors {
            let id = row["id"].as_str().unwrap();
            let (p, c) = compile(
                row["pattern"].as_str().unwrap(),
                row["flags"].as_str().unwrap(),
            );
            let subject = row["subject_units"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_u64().unwrap() as u16)
                .collect::<Vec<_>>();
            let r = run(&p, &c, &subject);
            let expected = &row["match"];
            assert_eq!(
                integer(&r, "status"),
                if expected.is_null() { 0 } else { 1 },
                "{id}: {r:?}"
            );
            if !expected.is_null() {
                let expected_pairs = expected["pairs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|p| p.as_array().unwrap().iter().map(|x| x.as_i64().unwrap()))
                    .collect::<Vec<_>>();
                assert_eq!(pairs(&r), expected_pairs, "{id}");
                assert_eq!(
                    integer(&r, "start"),
                    expected["index"].as_i64().unwrap(),
                    "{id}"
                );
                assert_eq!(
                    integer(&r, "end"),
                    expected["end"].as_i64().unwrap(),
                    "{id}"
                );
            } else {
                assert!(pairs(&r).iter().all(|&x| x == -1), "{id}");
            }
            assert_eq!(integer(&r, "search_start"), 0);
            assert!(integer(&r, "transitions") > 0);
            eprintln!(
                "P9_VECTOR id={id} status={} pairs={:?} trace=0 aot=0",
                integer(&r, "status"),
                pairs(&r)
            );
        }
    });
}
#[test]
fn p9_normative_vectors_and_modern_differences() {
    worker(|| {
        let fixtures =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/regexp");
        let source = std::fs::read(fixtures.join("spec_vectors.json")).unwrap();
        let cases: serde_json::Value = serde_json::from_slice(&source).unwrap();
        let out = std::process::Command::new("node")
            .arg(fixtures.join("oracle.js"))
            .arg(fixtures.join("spec_vectors.json"))
            .output()
            .unwrap();
        eprintln!(
            "P9_SPEC_NODE rc={} stdout={} stderr={}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success());
        let raw: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(cases.as_array().unwrap().len(), 7);
        assert_eq!(raw["vectors"].as_array().unwrap().len(), 7);
        for (case, reference) in cases
            .as_array()
            .unwrap()
            .iter()
            .zip(raw["vectors"].as_array().unwrap())
        {
            let id = case["id"].as_str().unwrap();
            assert_eq!(case["id"], reference["id"]);
            let expected = &case["normative_pairs"];
            let node_pairs = if reference["match"].is_null() {
                serde_json::Value::Null
            } else {
                reference["match"]["pairs"].clone()
            };
            assert_eq!(
                node_pairs != *expected,
                case["modern_difference"].as_bool().unwrap(),
                "{id}: version classification"
            );
            let (p, c) = compile(
                case["pattern"].as_str().unwrap(),
                case["flags"].as_str().unwrap(),
            );
            let subject = case["subject_units"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_u64().unwrap() as u16)
                .collect::<Vec<_>>();
            let r = run(&p, &c, &subject);
            assert_eq!(
                integer(&r, "status"),
                if expected.is_null() { 0 } else { 1 },
                "{id}"
            );
            if !expected.is_null() {
                let expected = expected
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|p| p.as_array().unwrap().iter().map(|n| n.as_i64().unwrap()))
                    .collect::<Vec<_>>();
                assert_eq!(pairs(&r), expected, "{id}");
            }
            eprintln!(
                "P9_SPEC id={id} status={} pairs={:?} citation={} modern_difference={}",
                integer(&r, "status"),
                pairs(&r),
                case["classification"],
                case["modern_difference"]
            );
        }
    });
}
#[test]
fn p9_pattern_rejections_exact_offsets() {
    for (p, f, offset) in [
        ("(?<a>x)", "", 2),
        ("(?<=a)b", "", 2),
        ("a", "u", 0),
        ("a", "y", 0),
        ("a", "s", 0),
        ("a", "gg", 1),
        ("[z-a]", "", 3),
        ("a{3,2}", "", 1),
        ("a**", "", 2),
        ("(", "", 0),
        ("a)", "", 1),
        ("\\xG0", "", 2),
        ("\\1", "", 0),
        ("\\q", "", 0),
        ("(?i)a", "", 2),
    ] {
        let e = compile_pattern(&p.encode_utf16().collect::<Vec<_>>(), f).unwrap_err();
        assert_eq!(e.offset, offset, "{p:?}/{f}: {e:?}");
        assert_eq!(
            e.part,
            if f.is_empty() {
                PatternPart::Pattern
            } else {
                PatternPart::Flags
            }
        );
    }
    assert!(compile_pattern(&"(?:a)(?=b)(?!c)".encode_utf16().collect::<Vec<_>>(), "gim").is_ok());
    assert!(compile_pattern(&vec![97; PATTERN_LIMIT], "").is_ok());
    assert_eq!(
        compile_pattern(&vec![97; PATTERN_LIMIT + 1], "")
            .unwrap_err()
            .offset,
        PATTERN_LIMIT
    );
    assert!(
        compile_pattern(
            &"()"
                .repeat(CAPTURE_LIMIT)
                .encode_utf16()
                .collect::<Vec<_>>(),
            ""
        )
        .is_ok()
    );
    assert!(
        compile_pattern(
            &"()"
                .repeat(CAPTURE_LIMIT + 1)
                .encode_utf16()
                .collect::<Vec<_>>(),
            ""
        )
        .unwrap_err()
        .message
        .contains("capture quota")
    );
}
#[test]
fn p9_resource_quota_is_distinct_from_no_match() {
    worker(|| {
        for (pattern, subject, budget, scratch, expected) in [
            ("(?:)*", vec![], 0, SCRATCH_LIMIT, 2),
            ("(?:a|aa)*b", vec![97; 20], 1000, SCRATCH_LIMIT, 2),
            ("(a|aa)*b", vec![97; 20], TRANSITION_LIMIT, 64, 3),
            ("a", vec![98], TRANSITION_LIMIT, SCRATCH_LIMIT, 0),
        ] {
            let (p, c) = compile_limits(pattern.encode_utf16().collect(), "", 0, budget, scratch);
            let r = run(&p, &c, &subject);
            assert_eq!(integer(&r, "status"), expected, "{pattern}: {r:?}");
            assert!(integer(&r, "transitions") <= budget);
            assert!(pairs(&r).iter().all(|&x| x == -1));
            eprintln!(
                "P9_QUOTA pattern={pattern:?} status={expected} transitions={}",
                integer(&r, "transitions")
            );
        }
    });
}
#[test]
fn p9_capture_256_boundary_executes() {
    worker(|| {
        let (p, c) = compile(&"(a)".repeat(CAPTURE_LIMIT), "");
        let r = run(&p, &c, &vec![97; CAPTURE_LIMIT]);
        assert_eq!(integer(&r, "status"), 1);
        let caps = pairs(&r);
        assert_eq!(caps.len(), 514);
        assert_eq!(&caps[..2], &[0, 256]);
        assert_eq!(&caps[512..], &[255, 256]);
    });
}
#[test]
fn p9_utf16_signed_unmatched_and_raw_surrogates() {
    worker(|| {
        let (p, c) = compile_limits(
            vec![40, 0xd800, 41, 40, 0xdc00, 41, 63],
            "",
            0,
            TRANSITION_LIMIT,
            SCRATCH_LIMIT,
        );
        let r = run(&p, &c, &[120, 0xd800]);
        assert_eq!(pairs(&r), vec![1, 2, 1, 2, -1, -1]);
        let r = run(&p, &c, &[0xd800, 0xdc00]);
        assert_eq!(pairs(&r), vec![0, 2, 0, 1, 1, 2]);
        assert_eq!(integer(&run(&p, &c, &[0xfffd]), "status"), 0);
    });
}
#[test]
fn p9_lookahead_quota_every_transition() {
    worker(|| {
        for budget in 0..100 {
            let (p, c) = compile_limits(
                "(?=(a|ab))a".encode_utf16().collect(),
                "",
                0,
                budget,
                SCRATCH_LIMIT,
            );
            let r = run(&p, &c, &[97, 98]);
            assert!(
                [1, 2].contains(&integer(&r, "status")),
                "budget={budget} result={r:?}"
            );
            assert!(integer(&r, "transitions") <= budget);
        }
    });
}
#[test]
fn p9_huge_quantifier_order_is_exact() {
    let pattern = "a{18446744073709551616,18446744073709551615}";
    let e = compile_pattern(&pattern.encode_utf16().collect::<Vec<_>>(), "").unwrap_err();
    assert_eq!(e.offset, 1);
    assert!(compile_pattern(&"a{0002,2}".encode_utf16().collect::<Vec<_>>(), "").is_ok());
}
#[test]
fn p9_search_start_and_empty_result_facts() {
    worker(|| {
        for start in [0, 1, 3, 4, -1] {
            let (p, c) = compile_limits(vec![], "g", start, TRANSITION_LIMIT, SCRATCH_LIMIT);
            let r = run(&p, &c, &[97, 98, 99]);
            assert_eq!(integer(&r, "search_start"), start);
            assert_eq!(
                integer(&r, "status"),
                if start < 0 {
                    4
                } else if start > 3 {
                    0
                } else {
                    1
                }
            );
            if (0..=3).contains(&start) {
                assert_eq!(pairs(&r), vec![start, start]);
            }
        }
        let (p, c) = compile(".", "");
        for subject in [vec![-1], vec![65536]] {
            assert_eq!(integer(&run_int(&p, &c, subject), "status"), 4);
        }
    });
}
fn kernel_shape(pattern: &str) -> (i64, i64, i64) {
    let compiled = compile_pattern(&pattern.encode_utf16().collect::<Vec<_>>(), "").unwrap();
    let caplen = (2 * (compiled.captures + 1)) as i64;
    let state = caplen + 2 * compiled.repeats as i64;
    (caplen, state, state + caplen)
}
fn scratch_case(pattern: &str, subject: &[u16], start: i64, budget: i64, scratch: i64) -> Value {
    let (program, catalog) =
        compile_limits(pattern.encode_utf16().collect(), "", start, budget, scratch);
    run(&program, &catalog, subject)
}
#[test]
fn p9_scratch_output_stays_inside_selected_limit() {
    worker(|| {
        let (cap0, state0, reserve0) = kernel_shape("a");
        assert_eq!((cap0, state0, reserve0), (2, 2, 4));
        let wide = "()".repeat(CAPTURE_LIMIT);
        let (cap256, state256, reserve256) = kernel_shape(&wide);
        assert_eq!((cap256, state256, reserve256), (514, 514, 1028));
        let mut unreserved = Vec::new();
        for (label, pattern, scratch) in [
            ("zero-scratch0", "a", 0i64),
            ("zero-scratch1", "a", 1),
            ("zero-caplen-minus-1", "a", cap0 - 1),
            ("zero-below-reserve", "a", reserve0 - 1),
            ("wide-scratch0", wide.as_str(), 0),
            ("wide-scratch1", wide.as_str(), 1),
            ("wide-caplen-minus-1", wide.as_str(), cap256 - 1),
            ("wide-below-reserve", wide.as_str(), reserve256 - 1),
        ] {
            let result = scratch_case(pattern, &[], 0, TRANSITION_LIMIT, scratch);
            let caps = pairs(&result);
            eprintln!(
                "P9_SCRATCH label={label} scratch={scratch} status={} caps_len={} start={} end={}",
                integer(&result, "status"),
                caps.len(),
                integer(&result, "start"),
                integer(&result, "end")
            );
            if integer(&result, "status") != 3
                || integer(&result, "start") != -1
                || integer(&result, "end") != -1
                || !caps.is_empty()
            {
                unreserved.push(format!(
                    "{label} scratch={scratch} status={} caps_len={} caps={caps:?}",
                    integer(&result, "status"),
                    caps.len()
                ));
            }
        }
        assert!(
            unreserved.is_empty(),
            "unreserved scratch still allocated capture cells or changed status: {unreserved:?}"
        );
        for (label, start, budget, scratch) in [
            ("neg-start-zero-scratch", -1, TRANSITION_LIMIT, 0),
            ("neg-budget-zero-scratch", 0, -1, 0),
            ("over-budget-zero-scratch", 0, TRANSITION_LIMIT + 1, 0),
            ("over-scratch", 0, TRANSITION_LIMIT, SCRATCH_LIMIT + 1),
            ("neg-start-ample", -1, TRANSITION_LIMIT, SCRATCH_LIMIT),
            ("neg-budget-ample", 0, -1, SCRATCH_LIMIT),
        ] {
            let result = scratch_case("a", &[], start, budget, scratch);
            assert_eq!(integer(&result, "status"), 4, "{label}: {result:?}");
            assert_eq!(pairs(&result), Vec::<i64>::new(), "{label}");
            assert_eq!(integer(&result, "start"), -1, "{label}");
            assert_eq!(integer(&result, "end"), -1, "{label}");
        }
        let (program, catalog) = compile_limits(
            ".".encode_utf16().collect(),
            "",
            0,
            TRANSITION_LIMIT,
            SCRATCH_LIMIT,
        );
        let invalid_subject = run_int(&program, &catalog, vec![-1]);
        assert_eq!(integer(&invalid_subject, "status"), 4);
        assert_eq!(pairs(&invalid_subject), Vec::<i64>::new());
        assert!(integer(&invalid_subject, "transitions") > 0);
        for scratch in [reserve0, reserve0 + 1, SCRATCH_LIMIT] {
            let matched = scratch_case("a", &[97], 0, TRANSITION_LIMIT, scratch);
            assert_eq!(integer(&matched, "status"), 1, "scratch={scratch}");
            assert_eq!(pairs(&matched), vec![0, 1], "scratch={scratch}");
            let missed = scratch_case("a", &[98], 0, TRANSITION_LIMIT, scratch);
            assert_eq!(integer(&missed, "status"), 0, "scratch={scratch}");
            assert_eq!(pairs(&missed), vec![-1, -1], "scratch={scratch}");
        }
        for scratch in [reserve256, reserve256 + 1] {
            let matched = scratch_case(&wide, &[], 0, TRANSITION_LIMIT, scratch);
            let caps = pairs(&matched);
            assert_eq!(
                integer(&matched, "status"),
                1,
                "scratch={scratch} caps={caps:?}"
            );
            assert_eq!(caps.len(), cap256 as usize, "scratch={scratch}");
            assert!(caps.iter().all(|&cell| cell == 0), "scratch={scratch}");
        }
        let (mid_caplen, _, mid_reserve) = kernel_shape("(a|aa)*b");
        assert!(64 >= mid_reserve);
        let mid = scratch_case("(a|aa)*b", &[97; 20], 0, TRANSITION_LIMIT, 64);
        assert_eq!(integer(&mid, "status"), 3, "{mid:?}");
        assert_eq!(pairs(&mid).len(), mid_caplen as usize);
        assert!(pairs(&mid).iter().all(|&cell| cell == -1));
        assert_eq!(integer(&mid, "start"), -1);
        let (transition_caplen, _, transition_reserve) = kernel_shape("(?:)*");
        assert!(SCRATCH_LIMIT >= transition_reserve);
        let transition = scratch_case("(?:)*", &[], 0, 0, SCRATCH_LIMIT);
        assert_eq!(integer(&transition, "status"), 2, "{transition:?}");
        assert_eq!(pairs(&transition).len(), transition_caplen as usize);
        assert!(pairs(&transition).iter().all(|&cell| cell == -1));
    });
}
