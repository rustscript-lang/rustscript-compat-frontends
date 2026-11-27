//! Private codec tests: dynamic host carriers, actual append-produced guest callable.
use super::prelude::uri;
use vm::{Expr as E, SourcePlugin, Stmt as S, Value, VmStatus};

struct KernelPlugin {
    trap_push: bool,
}
static PLUGIN: KernelPlugin = KernelPlugin { trap_push: false };
static TRAP_PLUGIN: KernelPlugin = KernelPlugin { trap_push: true };
impl SourcePlugin for KernelPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("uri-kernel-test"),
            extensions: &["uri-test"],
            typing_policy: vm::FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = crate::TYPESCRIPT_PLUGIN.parse_source(source, context)?;
        let old_locals = ir.locals;
        let old_functions = ir.functions.clone();
        let old_implementations: Vec<_> = ir
            .function_impls
            .iter()
            .map(|(i, f)| (*i, format!("{f:?}")))
            .collect();
        let old_bindings = ir.local_bindings.clone();
        let old_statements: Vec<_> = ir.stmts.iter().map(|s| format!("{s:?}")).collect();
        let old_structs: Vec<_> = ir
            .struct_schemas
            .iter()
            .map(|(n, s)| (n.clone(), format!("{s:?}")))
            .collect();
        let index = uri::append(&mut ir)?;
        assert!(ir.locals > old_locals);
        assert_eq!(&ir.functions[..old_functions.len()], &old_functions);
        for (i, f) in old_implementations {
            assert_eq!(format!("{:?}", ir.function_impls[&i]), f);
        }
        assert_eq!(ir.local_bindings, old_bindings);
        assert_eq!(
            ir.stmts[..old_statements.len()]
                .iter()
                .map(|s| format!("{s:?}"))
                .collect::<Vec<_>>(),
            old_statements
        );
        for (n, s) in old_structs {
            assert_eq!(format!("{:?}", ir.struct_schemas[&n]), s);
        }
        let declaration = ir.functions.iter().find(|f| f.index == index).unwrap();
        assert_eq!(declaration.name, "__compat_es5_uri_kernel");
        assert_eq!(declaration.arity, 3);
        assert!(!declaration.exported);
        assert_eq!(
            declaration.arg_schemas,
            vec![
                Some(vm::TypeSchema::Array(Box::new(vm::TypeSchema::Int))),
                Some(vm::TypeSchema::Int),
                Some(vm::TypeSchema::Int)
            ]
        );
        assert_eq!(declaration.return_schema, Some(uri::result_schema()));
        assert_eq!(ir.function_impls[&index].param_slots.len(), 3);
        if self.trap_push {
            fn trap(stmts: &mut [S]) -> usize {
                let mut count = 0;
                for s in stmts {
                    match s {
                        S::Assign { expr, .. } if matches!(&*expr,E::Call(i,_,_,_,_) if *i==vm::BuiltinFunction::ArrayPush.call_index()) =>
                        {
                            let original = expr.clone();
                            *expr = E::Block {
                                stmts: vec![S::Expr {
                                    expr: E::Call(
                                        vm::BuiltinFunction::Assert.call_index(),
                                        vec![],
                                        vec![E::Bool(false)],
                                        None,
                                        None,
                                    ),
                                    line: 1,
                                }],
                                expr: Box::new(original),
                            };
                            count += 1;
                        }
                        S::IfElse {
                            then_branch,
                            else_branch,
                            ..
                        } => {
                            count += trap(then_branch) + trap(else_branch);
                        }
                        S::While { body, .. } => {
                            count += trap(body);
                        }
                        _ => {}
                    }
                }
                count
            }
            let count = trap(&mut ir.function_impls.get_mut(&index).unwrap().body_stmts);
            assert!(count > 0);
            eprintln!("URI_ALLOCATION_TRAP actual_guest_push_sites={count}");
        }
        ir.stmts.push(S::Expr {
            expr: E::FunctionRef(index, vec![]),
            line: 1,
        });
        Ok(ir)
    }
}
fn machine() -> (vm::Vm, Value) {
    machine_with(&PLUGIN)
}
fn machine_with(plugin: &'static KernelPlugin) -> (vm::Vm, Value) {
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        "const sentinel: number = 73; function retained(x: number): number { return x + 1; }",
        vm::SourceFlavor::Plugin(vm::FrontendId::new("uri-kernel-test")),
        vm::CompileSourceFileOptions::new().with_source_plugin(plugin),
    )
    .expect("typed append producer compiles");
    let mut vm = compiled.into_vm().expect("VM");
    vm.set_jit_config(vm::JitConfig {
        enabled: false,
        ..Default::default()
    });
    vm.set_fuel(10_000_000);
    assert_eq!(vm.run().expect("producer execution"), VmStatus::Halted);
    let callable = vm.stack().last().expect("real function value").clone();
    assert!(matches!(callable, Value::Callable(_)));
    (vm, callable)
}
fn field(value: &Value, name: &str) -> Value {
    let Value::Map(m) = value else {
        panic!("record: {value:?}")
    };
    m.get(&Value::string(name)).expect("field").clone()
}
fn run(vm: &mut vm::Vm, callable: &Value, input: &[i64], mode: i64, limit: i64) -> (i64, Vec<i64>) {
    let raw = Value::array(input.iter().copied().map(Value::Int).collect());
    let before = raw.clone();
    vm.set_fuel(10_000_000);
    assert_eq!(
        vm.start_callable(
            callable.clone(),
            &[raw.clone(), Value::Int(mode), Value::Int(limit)]
        )
        .expect("guest codec"),
        VmStatus::Halted
    );
    let result = vm.take_callable_result().expect("typed result");
    assert_eq!(raw, before, "borrowed input is unchanged");
    assert!(!vm.jit_config().enabled);
    assert_eq!(vm.aot_exec_count(), 0);
    assert_eq!(vm.jit_native_exec_count(), 0);
    let Value::Int(status) = field(&result, "status") else {
        panic!("Int status")
    };
    let Value::Array(output) = field(&result, "output") else {
        panic!("Array(Int)")
    };
    let units = output
        .iter()
        .map(|v| {
            let Value::Int(n) = v else {
                panic!("unit {v:?}")
            };
            *n
        })
        .collect();
    if status == 0 {
        assert!(limit >= 0 && output.len() <= usize::try_from(limit).unwrap());
        assert!(
            output.capacity() <= 65536,
            "existing Vec growth stays within the private global ceiling"
        );
    } else {
        assert!(output.is_empty());
        assert_eq!(
            output.capacity(),
            0,
            "refusal never allocated output payload"
        );
    }
    assert_eq!(vm.host_context().resource_count(), 0);
    eprintln!(
        "URI_GUEST mode={mode} input_len={} limit={limit} status={status} output_len={} output_capacity={} halted=true aot={} native={} jit={}",
        input.len(),
        output.len(),
        output.capacity(),
        vm.aot_exec_count(),
        vm.jit_native_exec_count(),
        vm.jit_config().enabled
    );
    (status, units)
}
fn worker(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(f)
        .expect("private stack worker")
        .join()
        .unwrap();
}
#[test]
fn uri_limits_representation_and_empty() {
    worker(|| {
        let (mut vm, f) = machine();
        for mode in 0..4 {
            assert_eq!(run(&mut vm, &f, &[], mode, 0), (0, vec![]));
            assert_eq!(run(&mut vm, &f, &[65], mode, 0), (2, vec![]));
            assert_eq!(run(&mut vm, &f, &[65], mode, 1), (0, vec![65]));
            assert_eq!(run(&mut vm, &f, &[65, 66], mode, 1), (2, vec![]));
            for limit in [-1, 65537, i64::MIN, i64::MAX] {
                assert_eq!(run(&mut vm, &f, &[], mode, limit), (2, vec![]));
            }
            for unit in [-1, 65536, i64::MIN, i64::MAX] {
                assert_eq!(run(&mut vm, &f, &[65, unit], mode, 65536), (2, vec![]));
            }
            assert_eq!(run(&mut vm, &f, &vec![65; 65537], mode, 65536), (2, vec![]));
            assert_eq!(run(&mut vm, &f, &vec![65; 65536], mode, 65535), (2, vec![]));
            assert_eq!(
                run(&mut vm, &f, &vec![65; 65536], mode, 65536),
                (0, vec![65; 65536])
            );
        }
        for mode in [0, 1] {
            assert_eq!(run(&mut vm, &f, &[32], mode, 2), (2, vec![]));
            assert_eq!(run(&mut vm, &f, &[32], mode, 3), (0, vec![37, 50, 48]));
            assert_eq!(run(&mut vm, &f, &[0xd83d, 0xde00], mode, 11), (2, vec![]));
            assert_eq!(
                run(&mut vm, &f, &[0xd83d, 0xde00], mode, 12),
                (0, "%F0%9F%98%80".bytes().map(i64::from).collect())
            );
            let mut maximal = vec![32; 21845];
            maximal.push(65);
            let mut expected: Vec<_> = "%20".bytes().map(i64::from).cycle().take(65535).collect();
            expected.push(65);
            assert_eq!(run(&mut vm, &f, &maximal, mode, 65536), (0, expected));
            maximal.push(65);
            assert_eq!(run(&mut vm, &f, &maximal, mode, 65536), (2, vec![]));
        }
        assert_eq!(
            run(
                &mut vm,
                &f,
                &"%2f".bytes().map(i64::from).collect::<Vec<_>>(),
                2,
                2
            ),
            (2, vec![])
        );
        assert_eq!(
            run(
                &mut vm,
                &f,
                &"%2f".bytes().map(i64::from).collect::<Vec<_>>(),
                3,
                1
            ),
            (0, vec![47])
        );
        assert_eq!(run(&mut vm, &f, &[], -1, 65536), (2, vec![]));
        assert_eq!(run(&mut vm, &f, &[], 4, 65536), (2, vec![]));
    });
}
#[test]
fn uri_refusals_precede_all_payload_pushes() {
    worker(|| {
        let (mut vm, f) = machine_with(&TRAP_PLUGIN);
        for (input, mode, limit, status) in [
            (vec![65, 66], 0, 1, 2),
            (vec![65, 0xd800], 0, 65536, 1),
            (vec![65, 37], 2, 65536, 1),
            (vec![65, 65536], 3, 65536, 2),
            (vec![65], 1, -1, 2),
            (vec![], 3, 0, 0),
        ] {
            assert_eq!(run(&mut vm, &f, &input, mode, limit), (status, vec![]));
        }
        vm.set_fuel(10_000_000);
        let error = vm
            .start_callable(
                f,
                &[
                    Value::array(vec![Value::Int(65)]),
                    Value::Int(0),
                    Value::Int(1),
                ],
            )
            .expect_err("positive allocation trap must fire");
        assert!(
            format!("{error:?}").contains("assertion failed"),
            "actual push guard: {error:?}"
        );
        eprintln!("URI_ALLOCATION_TRAP positive_actual_push_refused={error:?}");
    });
}
#[test]
fn uri_callable_aliases_and_independent_results() {
    worker(|| {
        let (mut vm, f) = machine();
        let alias = f.clone();
        let raw = Value::array(vec![Value::Int(32), Value::Int(65)]);
        let snapshot = raw.clone();
        vm.set_fuel(10_000_000);
        assert_eq!(
            vm.start_callable(alias.clone(), &[raw.clone(), Value::Int(1), Value::Int(4)])
                .unwrap(),
            VmStatus::Halted
        );
        let first = vm.take_callable_result().unwrap();
        let first_snapshot = first.clone();
        assert_eq!(field(&first, "status"), Value::Int(0));
        assert_eq!(
            field(&first, "output"),
            Value::array(vec![
                Value::Int(37),
                Value::Int(50),
                Value::Int(48),
                Value::Int(65)
            ])
        );
        assert_eq!(
            run(&mut vm, &alias, &[66, 37], 0, 4),
            (0, vec![66, 37, 50, 53])
        );
        assert_eq!(first, first_snapshot);
        assert_eq!(raw, snapshot);
        assert_eq!(run(&mut vm, &f, &[37, 50, 48, 65], 3, 2), (0, vec![32, 65]));
        assert_eq!(raw, snapshot);
        assert_eq!(vm.aot_exec_count(), 0);
        assert_eq!(vm.jit_native_exec_count(), 0);
    });
}
#[test]
fn uri_unicode_encode_and_decode_spec() {
    worker(|| {
        let (mut vm, f) = machine();
        for mode in [0, 1] {
            assert_eq!(
                run(&mut vm, &f, &[0x80, 0xd83d, 0xde00], mode, 65536),
                (0, "%C2%80%F0%9F%98%80".bytes().map(i64::from).collect())
            );
            assert_eq!(run(&mut vm, &f, &[0xd800], mode, 65536), (1, vec![]));
        }
        for mode in [2, 3] {
            assert_eq!(
                run(
                    &mut vm,
                    &f,
                    &"%c2%80%F0%9f%98%80"
                        .bytes()
                        .map(i64::from)
                        .collect::<Vec<_>>(),
                    mode,
                    65536
                ),
                (0, vec![0x80, 0xd83d, 0xde00])
            );
            assert_eq!(
                run(
                    &mut vm,
                    &f,
                    &"%C0%80".bytes().map(i64::from).collect::<Vec<_>>(),
                    mode,
                    65536
                ),
                (1, vec![])
            );
            assert_eq!(
                run(&mut vm, &f, &[0xd800, 0xdc00, 0xd800], mode, 65536),
                (0, vec![0xd800, 0xdc00, 0xd800])
            );
        }
        let raw: Vec<_> = "%2f%3F%23".bytes().map(i64::from).collect();
        assert_eq!(run(&mut vm, &f, &raw, 2, 65536), (0, raw.clone()));
        assert_eq!(run(&mut vm, &f, &raw, 3, 65536), (0, vec![47, 63, 35]));
    });
}
#[test]
fn uri_independent_node_vectors() {
    worker(|| {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/uri");
        let argv = [root.join("oracle.js"), root.join("vectors.json")];
        let output = std::process::Command::new("node")
            .args(&argv)
            .output()
            .expect("independent Node reference is required");
        eprintln!(
            "URI_ORACLE command=node {argv:?} rc={:?} stdout={} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.status.success());
        let reference: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("reference JSON");
        let vectors: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/uri/vectors.json")).unwrap();
        let results = reference["results"].as_array().unwrap();
        assert_eq!(results.len(), vectors.as_array().unwrap().len());
        let mut ids = std::collections::HashSet::new();
        let (mut vm, f) = machine();
        for (row, original) in results.iter().zip(vectors.as_array().unwrap()) {
            assert!(ids.insert(row["id"].as_str().unwrap()));
            assert_eq!(row["id"], original["id"]);
            assert_eq!(row["input"], original["input"]);
            assert_eq!(row["mode"], original["mode"]);
            let input: Vec<_> = row["input"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_i64().unwrap())
                .collect();
            let expected: Vec<_> = row["output"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_i64().unwrap())
                .collect();
            let status = row["status"].as_i64().unwrap();
            if status == 1 {
                assert_eq!(row["error"], "URIError");
            }
            assert_eq!(
                run(&mut vm, &f, &input, row["mode"].as_i64().unwrap(), 65536),
                (status, expected),
                "reference id={}",
                row["id"]
            );
        }
        eprintln!(
            "URI_ORACLE_MATCHED unique_vectors={} node_version={} frontend_public_members=0",
            ids.len(),
            reference["version"]
        );
    });
}
#[test]
fn uri_dynamic_encode_percent() {
    worker(|| {
        let (mut vm, f) = machine();
        assert_eq!(
            run(&mut vm, &f, &[65, 32, 37], 0, 65536),
            (0, vec![65, 37, 50, 48, 37, 50, 53])
        );
        assert_eq!(
            run(&mut vm, &f, &[66, 37], 1, 65536),
            (0, vec![66, 37, 50, 53])
        );
    });
}
