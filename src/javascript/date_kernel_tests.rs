//! Private arithmetic producer and actual strict/ownership compiler/VM tests.
use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
static MAX_FUEL_USED: AtomicU64 = AtomicU64::new(0);
use vm::{Expr as E, HostFunction, HostFunctionRegistry, Stmt as S, TypeSchema as T, Value};

fn worker(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}
struct Kernel(usize);
static KERNELS: [Kernel; 11] = [
    Kernel(0),
    Kernel(1),
    Kernel(2),
    Kernel(3),
    Kernel(4),
    Kernel(5),
    Kernel(6),
    Kernel(7),
    Kernel(8),
    Kernel(9),
    Kernel(10),
];
impl SourcePlugin for Kernel {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("es5-date-kernel-test"),
            extensions: &["datekernel"],
            typing_policy: vm::FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }
    fn parse_source(
        &self,
        _: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<FrontendIr, vm::ParseError> {
        let (mut ir, sentinel) = if matches!(self.0, 6 | 10) {
            // Separate real parsed caller/capture/schema preservation control.
            let ir = crate::typescript::TYPESCRIPT_PLUGIN.parse_source(
                "type CallerRecord = { value: number }; function caller(x: number): number { const c: number = 7; const f = (y: number): number => c + y; return f(x); } const sentinel: boolean = caller(0) === 7;",
                context,
            )?;
            let sentinel = ir
                .local_bindings
                .iter()
                .find(|(name, _)| name == "sentinel")
                .expect("parsed caller local")
                .1;
            assert!(
                ir.function_impls.values().flat_map(|f| &f.body_stmts).any(|s| matches!(s, S::Let { expr: E::Closure(c), .. } if !c.capture_copies.is_empty())),
                "actual parsed capture producer"
            );
            (ir, sentinel)
        } else {
            // Original behavioral TDD fixture remains the same caller shape.
            let mut ir = FrontendIr {
                stmts: vec![],
                locals: 1,
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
            ir.stmts.push(S::Let {
                index: 0,
                declared_schema: Some(T::Bool),
                expr: E::Bool(true),
                line: 1,
            });
            (ir, 0)
        };
        let input = context.install_host_function(&mut ir, "date_test::input", 1)?;
        if matches!(self.0, 6 | 10) {
            prelude::date::append(&mut ir)?;
        }
        let prior = ir.clone();
        let entries = prelude::date::append(&mut ir)?;
        let declared_slots: usize = ir
            .function_impls
            .iter()
            .filter(|(id, _)| !prior.function_impls.contains_key(id))
            .map(|(_, f)| {
                f.param_slots.len()
                    + f.body_stmts
                        .iter()
                        .filter(|s| matches!(s, S::Let { .. }))
                        .count()
            })
            .sum();
        assert_eq!(ir.locals, prior.locals + declared_slots);
        for (id, f) in &prior.function_impls {
            assert_eq!(format!("{:?}", ir.function_impls[id]), format!("{f:?}"));
        }
        for (key, s) in &prior.struct_schemas {
            assert_eq!(format!("{:?}", ir.struct_schemas[key]), format!("{s:?}"));
        }
        let mut restored = ir.clone();
        restored.locals = prior.locals;
        restored.functions.truncate(prior.functions.len());
        restored.function_impls = prior.function_impls.clone();
        restored.struct_schemas = prior.struct_schemas.clone();
        assert_eq!(
            format!("{restored:?}"),
            format!("{prior:?}"),
            "all original caller IR and sidecars preserved"
        );
        let ids = [
            entries.time_clip,
            entries.make_time,
            entries.make_day,
            entries.make_date,
            entries.utc,
            entries.parts,
        ];
        let arities = [1, 4, 3, 2, 7, 1];
        for (&id, &arity) in ids.iter().zip(&arities) {
            let d = ir.functions.iter().find(|d| d.index == id).unwrap();
            assert_eq!(d.arg_schemas, vec![Some(T::Number); arity]);
            assert_eq!(d.arity as usize, arity);
            assert!(!d.exported);
            assert_eq!(ir.function_impls[&id].param_slots.len(), arity);
            assert!(ir.function_impls[&id].capture_copies.is_empty());
            if id == entries.make_day || id == entries.make_date {
                assert_eq!(d.return_schema, Some(entries.exact.schema.clone()));
                assert_eq!(d.return_type, vm::ValueType::Map);
            } else if id != entries.parts {
                assert_eq!(d.return_schema, Some(T::Number));
                assert_eq!(d.return_type, vm::ValueType::Float);
            } else {
                let Some(T::Named(name, args)) = &d.return_schema else {
                    panic!("parts schema")
                };
                assert!(args.is_empty());
                let T::Object(fields) = &ir.struct_schemas[name].body_schema else {
                    panic!("parts fields")
                };
                assert_eq!(fields.len(), 9);
                assert_eq!(fields["valid"], T::Bool);
                for key in [
                    "year",
                    "month",
                    "date",
                    "weekday",
                    "hour",
                    "minute",
                    "second",
                    "millisecond",
                ] {
                    assert_eq!(fields[key], T::Number);
                }
                assert_eq!(d.return_type, vm::ValueType::Map);
            }
        }
        let exact = &entries.exact;
        let T::Named(exact_name, _) = &exact.schema else {
            panic!("exact named schema")
        };
        let T::Object(exact_fields) = &ir.struct_schemas[exact_name].body_schema else {
            panic!("exact fields")
        };
        assert_eq!(exact_fields.len(), 4);
        assert_eq!(exact_fields["valid"], T::Bool);
        assert_eq!(exact_fields["negative"], T::Bool);
        assert_eq!(exact_fields["exponent"], T::Int);
        assert_eq!(exact_fields["limbs"], T::Array(Box::new(T::Int)));
        for (id, args, result) in [
            (exact.decode, vec![T::Number], exact.schema.clone()),
            (
                exact.add,
                vec![exact.schema.clone(); 2],
                exact.schema.clone(),
            ),
            (
                exact.negate,
                vec![exact.schema.clone()],
                exact.schema.clone(),
            ),
            (
                exact.multiply,
                vec![exact.schema.clone(), T::Int],
                exact.schema.clone(),
            ),
            (
                exact.quotient,
                vec![exact.schema.clone(), T::Int],
                exact.schema.clone(),
            ),
            (exact.modulo, vec![exact.schema.clone(), T::Int], T::Int),
            (exact.to_number, vec![exact.schema.clone()], T::Number),
            (
                entries.compose,
                vec![exact.schema.clone(), T::Number],
                exact.schema.clone(),
            ),
        ] {
            let decl = ir.functions.iter().find(|d| d.index == id).unwrap();
            assert_eq!(
                decl.arg_schemas,
                args.into_iter().map(Some).collect::<Vec<_>>()
            );
            assert_eq!(decl.return_schema, Some(result));
            assert!(!decl.exported);
            assert!(ir.function_impls[&id].capture_copies.is_empty());
        }
        let unique: std::collections::BTreeSet<_> = ir.functions.iter().map(|d| d.index).collect();
        assert_eq!(
            unique.len(),
            ir.functions.len(),
            "unique actual function IDs"
        );
        if self.0 == 10 {
            let mut exhausted = ir.clone();
            exhausted.locals = usize::MAX;
            let before = format!("{exhausted:?}");
            assert!(prelude::date::append(&mut exhausted).is_err());
            assert_eq!(
                format!("{exhausted:?}"),
                before,
                "local-capacity refusal is transactional"
            );
            let mut exhausted = ir.clone();
            let mut last = exhausted.functions.last().unwrap().clone();
            last.index = u16::MAX;
            exhausted.functions.push(last);
            let before = format!("{exhausted:?}");
            assert!(prelude::date::append(&mut exhausted).is_err());
            assert_eq!(
                format!("{exhausted:?}"),
                before,
                "function-capacity refusal is transactional"
            );
            let mut collision = ir.clone();
            let next = collision
                .functions
                .iter()
                .map(|d| d.index)
                .chain(collision.function_impls.keys().copied())
                .max()
                .unwrap()
                + 1;
            let name = format!("__Es5DateExact_{next}");
            let mut schema = collision.struct_schemas[exact_name].clone();
            schema.name = name.clone();
            collision.struct_schemas.insert(name, schema);
            let before = format!("{collision:?}");
            assert!(prelude::date::append(&mut collision).is_err());
            assert_eq!(
                format!("{collision:?}"),
                before,
                "schema-collision refusal is transactional"
            );
        }
        ir.stmts.push(S::Expr {
            expr: E::Var(sentinel),
            line: 1,
        });
        if self.0 >= 7 {
            let input_expr = |k| E::Call(input, vec![], vec![E::Int(k)], None, None);
            let decoded = entries.exact.decode_number(input_expr(0));
            let expr = match self.0 {
                7 => decoded,
                8 => entries.exact.number(decoded),
                9 => entries.exact.number(
                    entries
                        .exact
                        .sum(decoded, entries.exact.decode_number(input_expr(1))),
                ),
                10 => {
                    let slot = u16::try_from(ir.locals).unwrap();
                    ir.locals += 1;
                    ir.stmts.push(S::Let {
                        index: slot,
                        declared_schema: Some(entries.exact.schema.clone()),
                        expr: decoded,
                        line: 1,
                    });
                    let alias = u16::try_from(ir.locals).unwrap();
                    ir.locals += 1;
                    ir.stmts.push(S::Let {
                        index: alias,
                        declared_schema: Some(entries.exact.schema.clone()),
                        expr: E::Var(slot),
                        line: 1,
                    });
                    let e = &entries.exact;
                    let fields = [
                        (
                            "sum",
                            e.number(e.sum(E::Var(slot), e.decode_number(input_expr(1)))),
                        ),
                        ("negated", e.number(e.neg(E::Var(alias)))),
                        ("scaled", e.number(e.times(E::Var(slot), 86400000))),
                        ("floor", e.number(e.floor(E::Var(alias), 12))),
                        (
                            "modulo",
                            E::Call(e.modulo, vec![], vec![E::Var(slot), E::Int(12)], None, None),
                        ),
                        ("original", E::Var(slot)),
                        ("alias", E::Var(alias)),
                    ];
                    fields.into_iter().fold(
                        E::Call(
                            vm::BuiltinFunction::MapNew.call_index(),
                            vec![],
                            vec![],
                            None,
                            None,
                        ),
                        |r, (key, x)| {
                            E::Call(
                                vm::BuiltinFunction::Set.call_index(),
                                vec![],
                                vec![r, E::String(key.into()), x],
                                None,
                                None,
                            )
                        },
                    )
                }
                _ => unreachable!(),
            };
            ir.stmts.push(S::Expr { expr, line: 1 });
            if self.0 == 10 {
                ir.stmts.push(ir.stmts.last().unwrap().clone());
            }
            return Ok(ir);
        }
        let op = self.0 % 6;
        let result = E::Call(
            ids[op],
            vec![],
            (0..arities[op])
                .map(|i| E::Call(input, vec![], vec![E::Int(i as i64)], None, None))
                .collect(),
            None,
            None,
        );
        // MakeDay/MakeDate now return Exact. The old scalar observer is an
        // explicitly named private §8.5 image, never an intermediate UTC input.
        let result = if op == 2 || op == 3 {
            let slot = u16::try_from(ir.locals).unwrap();
            ir.locals += 1;
            ir.stmts.push(S::Let {
                index: slot,
                declared_schema: Some(entries.exact.schema.clone()),
                expr: result,
                line: 1,
            });
            let map = E::Call(
                vm::BuiltinFunction::MapNew.call_index(),
                vec![],
                vec![],
                None,
                None,
            );
            let map = E::Call(
                vm::BuiltinFunction::Set.call_index(),
                vec![],
                vec![
                    map,
                    E::String("number".into()),
                    entries.exact.number(E::Var(slot)),
                ],
                None,
                None,
            );
            E::Call(
                vm::BuiltinFunction::Set.call_index(),
                vec![],
                vec![map, E::String("exact".into()), E::Var(slot)],
                None,
                None,
            )
        } else {
            result
        };
        ir.stmts.push(S::Expr {
            expr: result,
            line: 1,
        });
        if matches!(self.0, 6 | 10) {
            ir.stmts.push(ir.stmts.last().unwrap().clone());
        }
        println!(
            "DATE_PRODUCER op={op} locals={} functions={} real_schemas=true strict=true ownership=true",
            ir.locals,
            ir.functions.len()
        );
        Ok(ir)
    }
}
fn schema() -> vm::HostFunctionSchema {
    vm::HostFunctionSchema::with_return(
        "date_test::input",
        vec![vm::HostParamSchema::value("index", vm::HostTypeSchema::Int)],
        vm::HostTypeSchema::Number,
    )
}
struct Input(Vec<Value>);
impl HostFunction for Input {
    fn call(&mut self, _: &mut vm::Vm, args: &[Value]) -> vm::VmResult<vm::CallOutcome> {
        let Value::Int(i) = args[0] else {
            panic!("input index")
        };
        Ok(vm::CallOutcome::Return(vm::CallReturn::One(
            self.0[usize::try_from(i).unwrap()].clone(),
        )))
    }
}
fn compile(op: usize) -> (vm::Program, Arc<vm::HostApiCatalog>) {
    let mut b = vm::HostApiBuilder::new();
    b.function(schema());
    let catalog = Arc::new(b.build().unwrap());
    let compiled = vm::compile_source_with_flavor_and_options(
        "dynamic Number inputs",
        vm::SourceFlavor::Plugin(vm::FrontendId::new("es5-date-kernel-test")),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(&KERNELS[op])
            .with_host_api_catalog(catalog.clone()),
    )
    .unwrap();
    (compiled.program, catalog)
}
fn run(p: &vm::Program, c: &vm::HostApiCatalog, inputs: Vec<f64>) -> Value {
    run_values(p, c, inputs.into_iter().map(Value::Float).collect())
}
fn run_values(p: &vm::Program, c: &vm::HostApiCatalog, inputs: Vec<Value>) -> Value {
    let mut machine = vm::Vm::new(p.clone());
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..Default::default()
    });
    machine.set_fuel(10_000_000);
    let mut r = HostFunctionRegistry::new();
    r.register_catalog(
        vm::host_api::HostImportSchema::from_function(c, &schema()),
        move || Box::new(Input(inputs.clone())),
    )
    .unwrap();
    r.bind_vm_cached(&mut machine).unwrap();
    let result = machine.run();
    assert_eq!(
        result.unwrap_or_else(|e| panic!(
            "guest {e:?} ip={} stack={:?}",
            machine.ip(),
            machine.stack()
        )),
        vm::VmStatus::Halted
    );
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    assert!(!machine.jit_config().enabled);
    assert!([2, 3].contains(&machine.stack().len()));
    assert_eq!(machine.stack()[0], Value::Bool(true));
    let out = machine.stack().last().unwrap().clone();
    if machine.stack().len() == 3 {
        if matches!(out, Value::Map(_)) {
            assert_eq!(
                machine.stack()[1],
                out,
                "same-VM repeated exact helper invocation"
            );
        } else {
            assert_number(
                number(machine.stack()[1].clone()),
                number(out.clone()),
                "same-VM repeated invocation",
            );
        }
    }
    MAX_FUEL_USED.fetch_max(10_000_000 - machine.get_fuel().unwrap(), Ordering::Relaxed);
    assert_eq!(machine.host_context().resource_count(), 0);
    machine.reset_for_reuse().unwrap();
    out
}
fn number(v: Value) -> f64 {
    match v {
        Value::Float(x) => x,
        Value::Int(x) => x as f64,
        value @ Value::Map(_) => number(field(&value, "number")),
        _ => panic!("Number: {v:?}"),
    }
}
fn decode(s: &str) -> f64 {
    match s {
        "NaN" => f64::NAN,
        "+Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        _ => s.parse().unwrap(),
    }
}
fn assert_number(actual: f64, want: f64, id: &str) {
    if want.is_nan() {
        assert!(actual.is_nan(), "{id}: {actual:?} must be NaN");
    } else {
        assert_eq!(
            actual.to_bits(),
            want.to_bits(),
            "{id}: actual={actual:?} want={want:?}"
        );
    }
}
fn field(value: &Value, key: &str) -> Value {
    let Value::Map(map) = value else {
        panic!("parts map {value:?}")
    };
    map.get(&Value::string(key)).unwrap().clone()
}
fn assert_exact(actual: &Value, expected: &serde_json::Value, id: &str) {
    let Value::Map(map) = actual else {
        panic!("{id}: Exact record {actual:?}")
    };
    assert_eq!(map.len(), 4, "{id}: exact field count");
    assert_eq!(
        field(actual, "valid"),
        Value::Bool(expected["valid"].as_bool().unwrap()),
        "{id}: valid"
    );
    assert_eq!(
        field(actual, "negative"),
        Value::Bool(expected["negative"].as_bool().unwrap()),
        "{id}: sign"
    );
    let exponent = field(actual, "exponent");
    assert_eq!(
        exponent,
        Value::Int(expected["exponent"].as_i64().unwrap()),
        "{id}: exponent"
    );
    let Value::Array(limbs) = field(actual, "limbs") else {
        panic!("{id}: limbs")
    };
    assert!(limbs.len() <= 80, "{id}: bounded active allocation");
    let expected = expected["limbs"].as_array().unwrap();
    assert_eq!(limbs.len(), expected.len(), "{id}: limb count");
    for (a, e) in limbs.iter().zip(expected) {
        let Value::Int(x) = a else {
            panic!("{id}: integer limb")
        };
        assert!((0..(1 << 30)).contains(x));
        assert_eq!(*x, e.as_i64().unwrap(), "{id}: exact payload");
    }
    if limbs.is_empty() {
        assert_eq!(field(actual, "negative"), Value::Bool(false));
        assert_eq!(exponent, Value::Int(0));
    } else {
        assert!(matches!(limbs.first(), Some(Value::Int(x)) if x % 2 == 1));
        assert!(matches!(limbs.last(), Some(Value::Int(x)) if *x != 0));
    }
}
fn assert_parts(actual: &Value, expected: &serde_json::Value, id: &str) {
    assert_eq!(
        field(actual, "valid"),
        Value::Bool(expected["valid"].as_bool().unwrap()),
        "{id}: validity"
    );
    for key in [
        "year",
        "month",
        "date",
        "weekday",
        "hour",
        "minute",
        "second",
        "millisecond",
    ] {
        assert_number(
            number(field(actual, key)),
            decode(expected[key].as_str().unwrap()),
            &format!("{id}: {key}"),
        );
    }
}
#[test]
fn node_reference_and_primary_vectors() {
    worker(|| {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/date");
        let script = root.join("utc-oracle.js");
        let fixture = root.join("utc-vectors.json");
        let out = std::process::Command::new("node")
            .arg(&script)
            .arg(&fixture)
            .output()
            .unwrap();
        println!(
            "DATE_NODE rc={:?} argv=node {:?} {:?} stderr={}",
            out.status.code(),
            script,
            fixture,
            String::from_utf8_lossy(&out.stderr)
        );
        println!("DATE_NODE_RAW {}", String::from_utf8_lossy(&out.stdout));
        assert!(out.status.success());
        let oracle: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let cases: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../../tests/fixtures/date/utc-vectors.json"))
                .unwrap();
        let observations = oracle["observations"].as_array().unwrap();
        assert_eq!(cases.len(), 4225);
        assert_eq!(observations.len(), cases.len());
        let programs: Vec<_> = (0..6).map(compile).collect();
        let mut compared = 0;
        let mut derived = 0;
        let mut differences = 0;
        for (case, reference) in cases.iter().zip(observations) {
            assert_eq!(case["id"], reference["id"]);
            assert_eq!(case["args"], reference["args"]);
            let id = case["id"].as_str().unwrap();
            let op = case["op"].as_u64().unwrap() as usize;
            let input = case["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| decode(a.as_str().unwrap()))
                .collect();
            let actual = run(&programs[op].0, &programs[op].1, input);
            if op == 2 || op == 3 {
                assert_exact(&field(&actual, "exact"), &case["exact"], id);
            }
            if op == 5 {
                assert_parts(&actual, &reference["result"], id);
            } else {
                let raw = number(actual);
                let node = decode(reference["result"].as_str().unwrap());
                let want = case
                    .get("primary")
                    .map_or(node, |v| decode(v.as_str().unwrap()));
                println!(
                    "DATE_VECTOR id={id} actual={raw:?} bits={:016x} node={} primary={:?}",
                    raw.to_bits(),
                    reference["result"],
                    case.get("primary")
                );
                assert_number(raw, want, id);
                if case.get("primary").is_some()
                    && !(raw.to_bits() == node.to_bits() || (raw.is_nan() && node.is_nan()))
                {
                    differences += 1;
                    println!(
                        "DATE_REFERENCE_DIFFERENCE id={id} private_unclipped_or_ES51_primary_control=true"
                    );
                }
            }
            compared += 1;
            if op == 4 {
                // Feed a real numeric oracle observation, not a host calendar calculation.
                let input = decode(reference["result"].as_str().unwrap());
                let parts = run(&programs[5].0, &programs[5].1, vec![input]);
                assert_parts(&parts, &reference["utcParts"], id);
                derived += 1;
            }
        }
        println!(
            "DATE_EXECUTION vectors={compared} derived_parts={derived} primary_reference_differences={differences} max_fuel_used={} fuel_limit=10000000 worker_stack=33554432 status=Halted jit_enabled=false jit_native=0 aot_native=0 public_date_coverage=false",
            MAX_FUEL_USED.load(Ordering::Relaxed)
        );
    });
}
#[test]
fn append_preserves_parsed_caller_and_repeat_calls() {
    worker(|| {
        let (p, c) = compile(6);
        for x in [-1.9, -0.0, 0.0, f64::NAN, 8.64e15] {
            let _ = run(&p, &c, vec![x]);
        }
    });
}
#[test]
fn number_integer_carriers_do_not_overflow() {
    worker(|| {
        let (p, c) = compile(1);
        assert_number(
            number(run_values(
                &p,
                &c,
                vec![Value::Int(1), Value::Int(-1), Value::Int(1), Value::Int(-1)],
            )),
            3_540_999.0,
            "Int Number carriers",
        );
        assert!(
            number(run_values(
                &p,
                &c,
                vec![
                    Value::Int(i64::MAX),
                    Value::Int(0),
                    Value::Int(0),
                    Value::Int(0)
                ]
            ))
            .is_finite()
        );
    });
}
#[test]
fn make_time_dynamic_ieee() {
    worker(|| {
        let (p, c) = compile(1);
        assert_eq!(number(run(&p, &c, vec![1.9, -1.9, 1.9, -1.9])), 3_540_999.0);
    });
}
#[test]
fn make_day_dynamic_calendar() {
    worker(|| {
        let (p, c) = compile(2);
        assert_eq!(number(run(&p, &c, vec![1970.0, 0.0, 1.0])), 0.0);
    });
}
#[test]
fn make_day_exact_primary_witness() {
    worker(|| {
        let (p, c) = compile(2);
        let actual = run(&p, &c, vec![1e15, 0.0, 1.0]);
        assert_exact(
            &field(&actual, "exact"),
            &serde_json::json!({"valid":true,"negative":false,"exponent":3,"limbs":[195732907,42519823]}),
            "parent mathematical witness exact day365242499999280472",
        );
        assert_number(
            number(actual),
            365242499999280448.0,
            "explicit private RNE image only",
        );
    });
}
#[test]
fn make_date_exact_primary_intermediate() {
    worker(|| {
        let (p, c) = compile(3);
        assert_number(
            number(run(&p, &c, vec![1e300, -8.64e307])),
            8.537504156112112e291,
            "explicit private §8.5 RNE image of exact MakeDate; not a MakeDate conversion step",
        );
    });
}
#[test]
fn make_date_dynamic_ieee() {
    worker(|| {
        let (p, c) = compile(3);
        assert_eq!(number(run(&p, &c, vec![-1.0, 1.0])), -86_399_999.0);
    });
}
#[test]
fn utc_dynamic_construction() {
    worker(|| {
        let (p, c) = compile(4);
        assert_eq!(
            number(run(&p, &c, vec![1970.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0])),
            0.0
        );
    });
}
#[test]
fn parts_dynamic_pre_epoch() {
    worker(|| {
        let (p, c) = compile(5);
        let r = run(&p, &c, vec![-1.9]);
        assert_eq!(field(&r, "valid"), Value::Bool(true));
        for (key, want) in [
            ("year", 1969.0),
            ("month", 11.0),
            ("date", 31.0),
            ("weekday", 3.0),
            ("hour", 23.0),
            ("minute", 59.0),
            ("second", 59.0),
            ("millisecond", 999.0),
        ] {
            assert_eq!(number(field(&r, key)), want, "{key}");
        }
    });
}
#[test]
fn exact_signed_floor_modulo_alias_and_repeat() {
    worker(|| {
        let (p, c) = compile(10);
        for a in [
            -9007199254740992_i64,
            -13,
            -12,
            -1,
            0,
            1,
            12,
            13,
            9007199254740992,
        ] {
            let result = run(&p, &c, vec![a as f64, 18.0]);
            assert_number(number(field(&result, "sum")), a as f64 + 18.0, "signed sum");
            assert_number(
                number(field(&result, "negated")),
                if a == 0 { 0.0 } else { -(a as f64) },
                "negation",
            );
            assert_number(
                number(field(&result, "scaled")),
                a as f64 * 86400000.0,
                "small multiply explicit RNE image",
            );
            assert_number(
                number(field(&result, "floor")),
                a.div_euclid(12) as f64,
                "signed mathematical floor",
            );
            assert_eq!(field(&result, "modulo"), Value::Int(a.rem_euclid(12)));
            let mut coefficient = a.unsigned_abs();
            let mut exponent = 0;
            if coefficient != 0 {
                while coefficient % 2 == 0 {
                    coefficient /= 2;
                    exponent += 1;
                }
            }
            let mut limbs = vec![];
            while coefficient != 0 {
                limbs.push(coefficient % 1073741824);
                coefficient /= 1073741824;
            }
            let expected =
                serde_json::json!({"valid":true,"negative":a<0,"exponent":exponent,"limbs":limbs});
            assert_exact(
                &field(&result, "original"),
                &expected,
                "input after all helpers",
            );
            assert_exact(
                &field(&result, "alias"),
                &expected,
                "alias after all helpers",
            );
        }
    });
}
#[test]
fn exact_mathematical_witness_utc_cancellation() {
    worker(|| {
        let (p, c) = compile(4);
        assert_number(
            number(run(
                &p,
                &c,
                vec![
                    1e15,
                    0.0,
                    1.0,
                    f64::from_bits(0xc3de699cb62fff10),
                    0.0,
                    0.0,
                    0.0,
                ],
            )),
            257662976.0,
            "exact day stays exact through IEEE MakeTime cancellation",
        );
    });
}
#[test]
fn exact_decoder_and_rounding_dynamic() {
    worker(|| {
        let (p, c) = compile(8);
        for x in [
            0.0,
            -0.0,
            1.0,
            -1.5,
            9007199254740992.0,
            f64::MAX,
            -f64::MAX,
            f64::MIN_POSITIVE,
            f64::from_bits(1),
            -f64::from_bits(1),
            f64::INFINITY,
            f64::NAN,
        ] {
            assert_number(
                number(run(&p, &c, vec![x])),
                if x == 0.0 {
                    0.0
                } else if x.is_finite() {
                    x
                } else {
                    f64::NAN
                },
                "guest decode/explicit RNE",
            );
        }
        let (raw, rc) = compile(7);
        for (x, negative, exponent, limbs) in [
            (0.0, false, 0, vec![]),
            (-0.0, false, 0, vec![]),
            (-1.5, true, -1, vec![3]),
            (f64::from_bits(1), false, -1074, vec![1]),
            (-f64::from_bits(1), true, -1074, vec![1]),
            (f64::MIN_POSITIVE, false, -1022, vec![1]),
            (f64::MAX, false, 971, vec![1073741823, 8388607]),
        ] {
            assert_exact(
                &run(&raw, &rc, vec![x]),
                &serde_json::json!({"valid":true,"negative":negative,"exponent":exponent,"limbs":limbs}),
                "actual guest decoder payload",
            );
        }
        for x in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_exact(
                &run(&raw, &rc, vec![x]),
                &serde_json::json!({"valid":false,"negative":false,"exponent":0,"limbs":[]}),
                "invalid decoder payload",
            );
        }
        let (p, c) = compile(9);
        for (a, b, expected) in [
            (9007199254740992.0, 1.0, 9007199254740992.0),
            (9007199254740992.0, 3.0, 9007199254740996.0),
            (f64::MAX, f64::MAX, f64::INFINITY),
            (1.0, -1.0, 0.0),
            (1.0, f64::from_bits(1), 1.0),
            (9007199254740992.0, 1.5, 9007199254740994.0),
            (-9007199254740992.0, -3.0, -9007199254740996.0),
            (
                f64::MIN_POSITIVE,
                -f64::from_bits(1),
                f64::from_bits(0x000fffffffffffff),
            ),
            (f64::from_bits(1), f64::from_bits(1), f64::from_bits(2)),
            (f64::MAX, f64::from_bits(0x7c90000000000000), f64::INFINITY),
            (f64::MAX, f64::from_bits(0x7c8fffffffffffff), f64::MAX),
        ] {
            assert_number(
                number(run(&p, &c, vec![a, b])),
                expected,
                "guest signed add/RNE",
            );
        }
    });
}
#[test]
fn exact_in_range_month_year_cancellation() {
    worker(|| {
        let (p, c) = compile(4);
        let actual = number(run(
            &p,
            &c,
            vec![
                9007199254740992.0,
                f64::from_bits(0xc377fffffffffa3a),
                1.0,
                0.0,
                0.0,
                0.0,
                0.0,
            ],
        ));
        println!(
            "DATE_NEW_RED month_year actual={actual:?} bits={:016x} expected=20995200000",
            actual.to_bits()
        );
        assert_number(
            actual,
            20995200000.0,
            "exact month/year in-range cancellation",
        );
    });
}
#[test]
fn exact_in_range_make_date_cancellation() {
    worker(|| {
        let (p, c) = compile(3);
        let date = number(run(
            &p,
            &c,
            vec![1099511627777.0, f64::from_bits(0xc414997000001499)],
        ));
        let (clip, cc) = compile(0);
        let actual = number(run(&clip, &cc, vec![date]));
        println!(
            "DATE_NEW_RED make_date actual={actual:?} bits={:016x} expected=7168",
            actual.to_bits()
        );
        assert_number(actual, 7168.0, "exact multiply-add in-range cancellation");
    });
}
#[test]
fn time_clip_dynamic_boundaries() {
    worker(|| {
        let (p, c) = compile(0);
        assert!(
            number(run(&p, &c, vec![8_640_000_000_000_001.0])).is_nan(),
            "TimeClip must reject the first positive out-of-range Number"
        );
        assert_eq!(number(run(&p, &c, vec![-1.9])), -1.0);
        assert_eq!(
            number(run(&p, &c, vec![8_640_000_000_000_000.0])),
            8_640_000_000_000_000.0
        );
    });
}

// Coverage-only control: the original Kernel(6/10) above remains a TypeScript
// capture control. This separate producer forwards the actual current ES5
// plugin, then augments its completed IR (never its reserved lowering window).
const DATE_ES5_CALLER_SOURCE: &str =
    "function f(x){'use strict';return x;}var n=0;var answer=f(n=n+6);answer===6&&n===6;";
struct DateEs5Caller;
static DATE_ES5_CALLER: DateEs5Caller = DateEs5Caller;
impl SourcePlugin for DateEs5Caller {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        JAVASCRIPT_ES5_PLUGIN.descriptor()
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<FrontendIr, vm::ParseError> {
        // No alternate source, IR producer, fallback, TLS or leaked plugin.
        let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        assert_eq!(source, DATE_ES5_CALLER_SOURCE);
        assert!(ir.functions.iter().any(|d| {
            ir.function_impls.contains_key(&d.index)
                && d.return_schema == Some(representation::logical_result_schema())
        }));
        let parsed = ir.clone();
        let input = context.install_host_function(&mut ir, "date_es5_caller::input", 1)?;
        assert!(!parsed.functions.iter().any(|d| d.index == input));
        assert!(!parsed.function_impls.contains_key(&input));
        let metadata = ir.host_api_metadata.as_ref().unwrap();
        assert_eq!(
            metadata.candidates(input),
            Some(&[date_es5_input_schema()][..])
        );
        if let Some(old) = &parsed.host_api_metadata {
            for index in old.function_indices() {
                assert_eq!(metadata.candidates(index), old.candidates(index));
            }
        }
        // The context API adds exactly the declared host import and metadata.
        let mut restored = ir.clone();
        restored.functions.truncate(parsed.functions.len());
        restored.host_api_metadata = parsed.host_api_metadata.clone();
        assert_eq!(format!("{restored:?}"), format!("{parsed:?}"));

        let prior = ir.clone();
        let entries = prelude::date::append(&mut ir)?;
        let old_ids: std::collections::BTreeSet<_> = prior
            .functions
            .iter()
            .map(|d| d.index)
            .chain(prior.function_impls.keys().copied())
            .chain(prior.host_api_metadata.as_ref().unwrap().function_indices())
            .collect();
        let added = &ir.functions[prior.functions.len()..];
        assert!(!added.is_empty());
        let mut new_ids = std::collections::BTreeSet::new();
        let mut slots = std::collections::BTreeSet::new();
        for declaration in added {
            assert!(!old_ids.contains(&declaration.index));
            assert!(new_ids.insert(declaration.index));
            assert!(!declaration.exported);
            let body = &ir.function_impls[&declaration.index];
            assert!(body.capture_copies.is_empty());
            assert_eq!(body.param_slots.len(), usize::from(declaration.arity));
            assert_eq!(declaration.arg_schemas.len(), body.param_slots.len());
            assert!(declaration.arg_schemas.iter().all(Option::is_some));
            assert!(declaration.return_schema.is_some());
            for slot in body
                .param_slots
                .iter()
                .copied()
                .chain(body.body_stmts.iter().filter_map(|s| match s {
                    S::Let { index, .. } => Some(*index),
                    _ => None,
                }))
            {
                assert!(usize::from(slot) >= prior.locals);
                assert!(usize::from(slot) < ir.locals);
                assert!(slots.insert(slot), "new slots cannot alias each other");
            }
        }
        assert_eq!(slots.len(), ir.locals - prior.locals);
        assert_eq!(
            ir.function_impls.len(),
            prior.function_impls.len() + added.len()
        );
        assert_eq!(new_ids.len(), added.len());
        for (id, body) in &prior.function_impls {
            assert_eq!(format!("{:?}", ir.function_impls[id]), format!("{body:?}"));
        }
        for (name, schema) in &prior.struct_schemas {
            assert_eq!(
                format!("{:?}", ir.struct_schemas[name]),
                format!("{schema:?}")
            );
        }
        let mut restored = ir.clone();
        restored.locals = prior.locals;
        restored.functions.truncate(prior.functions.len());
        restored.function_impls = prior.function_impls.clone();
        restored.struct_schemas = prior.struct_schemas.clone();
        assert_eq!(
            format!("{restored:?}"),
            format!("{prior:?}"),
            "complete caller statements/functions/captures/bindings/source/semantics/host metadata"
        );
        let utc = added.iter().find(|d| d.index == entries.utc).unwrap();
        assert_eq!(utc.arity, 7);
        assert_eq!(utc.arg_schemas, vec![Some(T::Number); 7]);
        assert_eq!(utc.return_schema, Some(T::Number));
        assert_eq!(utc.return_type, vm::ValueType::Float);
        // Observe real boxed source payloads after its unchanged Script result.
        // They are primitive records; no managed slot is decoded after reset.
        for name in ["answer", "n"] {
            let index = prior
                .local_bindings
                .iter()
                .rfind(|(n, _)| n == name)
                .unwrap()
                .1;
            ir.stmts.push(S::Expr {
                expr: E::Var(index),
                line: 1,
            });
        }
        ir.stmts.push(S::Expr {
            expr: E::Call(
                entries.utc,
                vec![],
                (0..7)
                    .map(|i| E::Call(input, vec![], vec![E::Int(i)], None, None))
                    .collect(),
                None,
                None,
            ),
            line: 1,
        });
        assert_eq!(
            format!("{:?}", &ir.stmts[..prior.stmts.len()]),
            format!("{:?}", prior.stmts)
        );
        eprintln!(
            "DATE_ACTUAL_ES5_PRODUCER source={source:?} old_functions={} new_functions={} old_locals={} new_locals={} input={input} utc={} complete_ir_preserved=true typed_number7=true",
            prior.functions.len(),
            added.len(),
            prior.locals,
            ir.locals,
            entries.utc
        );
        Ok(ir)
    }
    fn module_type_exports(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
        ir: &FrontendIr,
    ) -> Result<Vec<vm::FrontendTypeExport>, vm::ParseError> {
        JAVASCRIPT_ES5_PLUGIN.module_type_exports(source, context, ir)
    }
    fn validate_typed_ir(
        &self,
        typed_ir: &vm::compiler::FrontendTypedIr<'_>,
    ) -> Result<(), vm::compiler::CompileError> {
        JAVASCRIPT_ES5_PLUGIN.validate_typed_ir(typed_ir)
    }
    fn parser_dialect(&self) -> Option<&'static dyn vm::compiler::ParserDialect> {
        JAVASCRIPT_ES5_PLUGIN.parser_dialect()
    }
    fn parse_module_imports(
        &self,
        source: &str,
        path: &std::path::Path,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<Vec<vm::ModuleImport>, vm::SourcePathError> {
        JAVASCRIPT_ES5_PLUGIN.parse_module_imports(source, path, context)
    }
    fn strip_import_directives(&self, source: &str) -> String {
        JAVASCRIPT_ES5_PLUGIN.strip_import_directives(source)
    }
}
fn date_es5_input_schema() -> vm::HostFunctionSchema {
    vm::HostFunctionSchema::with_return(
        "date_es5_caller::input",
        vec![vm::HostParamSchema::value("index", vm::HostTypeSchema::Int)],
        vm::HostTypeSchema::Number,
    )
}
#[derive(Default)]
struct DateEs5Inputs {
    values: Option<[f64; 7]>,
    observed: Vec<usize>,
}
struct DateEs5Input(Arc<std::sync::Mutex<DateEs5Inputs>>);
impl HostFunction for DateEs5Input {
    fn call(&mut self, machine: &mut vm::Vm, args: &[Value]) -> vm::VmResult<vm::CallOutcome> {
        assert!(machine.capture_certification_stats().unwrap().valid);
        let [Value::Int(index)] = args else {
            panic!("typed Int callback index")
        };
        let index = usize::try_from(*index).unwrap();
        let mut state = self.0.lock().unwrap();
        assert_eq!(
            index,
            state.observed.len(),
            "once-only left-to-right UTC arguments"
        );
        let value = state.values.expect("inputs filled after compilation")[index];
        state.observed.push(index);
        Ok(vm::CallOutcome::Return(vm::CallReturn::One(Value::Float(
            value,
        ))))
    }
}
#[test]
fn date_append_preserves_actual_es5_caller_and_executes() {
    worker(|| {
        let base = crate::runtime::cli::reference_array_cli_effective_catalog();
        let mut builder = vm::HostApiCatalog::builder();
        for resource in base.resources() {
            builder.resource(resource.clone());
        }
        for function in base.functions() {
            builder.function(function.clone());
        }
        builder.function(date_es5_input_schema());
        let catalog = Arc::new(builder.build().unwrap());
        let options = || {
            vm::CompileSourceFileOptions::new()
                .with_source_plugin(&DATE_ES5_CALLER)
                .with_host_api_catalog(catalog.clone())
        };
        let descriptor = DATE_ES5_CALLER.descriptor();
        assert_eq!(descriptor.id, JAVASCRIPT_ES5_PLUGIN.descriptor().id);
        assert_eq!(descriptor.extensions, &["js"]);
        assert_eq!(descriptor.typing_policy, vm::FrontendTypingPolicy::Strict);
        assert!(descriptor.supports_ownership_checking);
        // This admission refusal is producer identity, not a product RED.
        let typed_source = "var n: number = 6;n;";
        let rejection = match vm::compile_source_with_flavor_and_options_with_capture_certification(
            typed_source,
            javascript_es5_flavor(),
            options(),
        ) {
            Ok(_) => panic!("actual ES5 parser must reject TypeScript annotations"),
            Err(error) => error,
        };
        assert!(format!("{rejection:?}").contains("JavaScript ES5 parse error"));
        eprintln!("DATE_ACTUAL_ES5_IDENTITY typed_source={typed_source:?} rejected={rejection:?}");
        for (inputs, expected) in [
            ([1970.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0], 0.0),
            (
                [
                    9007199254740992.0,
                    f64::from_bits(0xc377fffffffffa3a),
                    1.0,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                ],
                20995200000.0,
            ),
        ] {
            let state = Arc::new(std::sync::Mutex::new(DateEs5Inputs::default()));
            let issued = vm::compile_source_with_flavor_and_options_with_capture_certification(
                DATE_ES5_CALLER_SOURCE,
                javascript_es5_flavor(),
                options(),
            )
            .expect("actual ES5 source plus completed-IR Date append, strict and ownership");
            assert!(state.lock().unwrap().values.is_none());
            assert!(state.lock().unwrap().observed.is_empty());
            let mut machine = issued.into_vm().expect("retain issued capture authority");
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..Default::default()
            });
            assert!(machine.capture_certification_stats().unwrap().valid);
            let mut registry = HostFunctionRegistry::restricted();
            registry.set_capability_profile(
                vm::CapabilityProfile::builder()
                    .allow_host_import("date_es5_caller::input")
                    .build(),
            );
            crate::runtime::array::typescript_array_host_module()
                .install_from_catalog(&mut registry, &catalog)
                .unwrap();
            crate::runtime::array::typescript_owned_array_host_module()
                .install_from_catalog(&mut registry, &catalog)
                .unwrap();
            crate::runtime::array::typescript_array_snapshot_host_module()
                .install_from_catalog(&mut registry, &catalog)
                .unwrap();
            crate::runtime::scalar_math::typescript_scalar_math_host_module()
                .install_from_catalog(&mut registry, &catalog)
                .unwrap();
            crate::runtime::scalar_error::typescript_error_host_module()
                .install_from_catalog(&mut registry, &catalog)
                .unwrap();
            for builtin in ["math::floor", "math::trunc", "math::abs", "math::is_finite"] {
                registry.allow_builtin(builtin).unwrap();
            }
            let callback_state = state.clone();
            registry
                .register_catalog(
                    vm::host_api::HostImportSchema::from_function(
                        &catalog,
                        &date_es5_input_schema(),
                    ),
                    move || Box::new(DateEs5Input(callback_state.clone())),
                )
                .unwrap();
            registry.bind_vm_cached(&mut machine).unwrap();
            crate::runtime::array::initialize_typescript_owned_arrays(
                &mut machine,
                crate::runtime::array::OwnedArrayLimits::default(),
            )
            .unwrap();
            state.lock().unwrap().values = Some(inputs);
            machine.set_fuel(10_000_000);
            assert_eq!(
                machine.run().unwrap_or_else(|error| panic!(
                    "ES5/Date execution {error:?} ip={} stack={:?}",
                    machine.ip(),
                    machine.stack()
                )),
                vm::VmStatus::Halted
            );
            assert!(!machine.jit_config().enabled);
            assert_eq!(machine.aot_exec_count(), 0);
            assert_eq!(machine.jit_native_exec_count(), 0);
            assert!(machine.capture_certification_stats().unwrap().valid);
            assert_eq!(state.lock().unwrap().observed, [0, 1, 2, 3, 4, 5, 6]);
            let result = machine.stack();
            assert_eq!(
                result.len(),
                4,
                "original Script result plus answer/effect/UTC"
            );
            assert_eq!(
                field(&result[0], "tag"),
                Value::Int(representation::BOOLEAN)
            );
            assert_eq!(field(&result[0], "flag"), Value::Bool(true));
            for (value, expected) in [(&result[1], 6.0), (&result[2], 6.0)] {
                assert_eq!(field(value, "tag"), Value::Int(representation::NUMBER));
                assert_eq!(
                    field(value, "number"),
                    Value::Float(expected),
                    "original boxed Number payload"
                );
            }
            assert_number(
                number(result[3].clone()),
                expected,
                "genuine ES5 caller with dynamic UTC",
            );
            let resources = machine.host_context().resource_count();
            let b1 = crate::runtime::array::owned_array_runtime_stats(&mut machine)
                .expect("actual B1 scope");
            assert_eq!(b1.pending, 0);
            assert!(b1.registered > 0);
            assert_eq!(resources, b1.registered);
            eprintln!(
                "DATE_ACTUAL_ES5_EXEC inputs={inputs:?} script_bool=true answer=Float(6) effect=Float(6) utc={expected:?} callback_order={:?} certified=true resources={resources} b1={b1:?} halted=true jit=false native=0 fuel_used={}",
                state.lock().unwrap().observed,
                10_000_000 - machine.get_fuel().unwrap()
            );
            machine.reset_for_reuse().unwrap();
            assert_eq!(machine.host_context().resource_count(), 0);
            assert!(crate::runtime::array::owned_array_runtime_stats(&mut machine).is_none());
            assert!(!machine.capture_certification_stats().unwrap().valid);
            eprintln!(
                "DATE_ACTUAL_ES5_RESET resources=0 scope_directory=false certification_valid=false"
            );
        }
    });
}

// One completed real-ES5 realm: runtime Date control -> managed input -> URI
// decode -> managed output. The caller alone owns the three additional roots.
#[cfg(test)]
mod uri_date_shared_scope {
    use super::*;

    struct Shared;
    static SHARED: Shared = Shared;
    fn call(id: u16, args: Vec<E>) -> E {
        E::Call(id, vec![], args, None, None)
    }
    fn helper(ir: &FrontendIr, name: &str) -> u16 {
        ir.functions.iter().find(|d| d.name == name).unwrap().index
    }
    fn bind(ir: &mut FrontendIr, schema: T, expr: E) -> u16 {
        let index = u16::try_from(ir.locals).unwrap();
        ir.locals += 1;
        ir.stmts.push(S::Let {
            index,
            declared_schema: Some(schema),
            expr,
            line: 1,
        });
        index
    }
    fn units(values: &[i64]) -> E {
        values.iter().fold(
            call(vm::BuiltinFunction::ArrayNew.call_index(), vec![]),
            |array, &unit| {
                call(
                    vm::BuiltinFunction::ArrayPush.call_index(),
                    vec![array, E::Int(unit)],
                )
            },
        )
    }
    fn preserved(prior: &FrontendIr, ir: &FrontendIr, stage: &str) {
        assert_eq!(&ir.functions[..prior.functions.len()], prior.functions);
        assert_eq!(
            format!("{:?}", &ir.stmts[..prior.stmts.len()]),
            format!("{:?}", prior.stmts)
        );
        for (id, body) in &prior.function_impls {
            assert_eq!(format!("{:?}", ir.function_impls[id]), format!("{body:?}"));
        }
        for (name, schema) in &prior.struct_schemas {
            assert_eq!(
                format!("{:?}", ir.struct_schemas[name]),
                format!("{schema:?}")
            );
        }
        let occupied: std::collections::BTreeSet<_> = prior
            .functions
            .iter()
            .map(|d| d.index)
            .chain(prior.function_impls.keys().copied())
            .chain(prior.host_api_metadata.as_ref().unwrap().function_indices())
            .collect();
        let mut added = std::collections::BTreeSet::new();
        let mut slots = std::collections::BTreeSet::new();
        for d in &ir.functions[prior.functions.len()..] {
            assert!(!occupied.contains(&d.index));
            assert!(added.insert(d.index));
            assert!(!d.exported);
            let body = &ir.function_impls[&d.index];
            assert!(body.capture_copies.is_empty());
            assert_eq!(body.param_slots.len(), usize::from(d.arity));
            assert_eq!(d.arg_schemas.len(), body.param_slots.len());
            assert!(d.arg_schemas.iter().all(Option::is_some));
            assert!(d.return_schema.is_some());
            for slot in body
                .param_slots
                .iter()
                .copied()
                .chain(body.body_stmts.iter().filter_map(|s| {
                    if let S::Let { index, .. } = s {
                        Some(*index)
                    } else {
                        None
                    }
                }))
            {
                assert!(usize::from(slot) >= prior.locals && usize::from(slot) < ir.locals);
                assert!(slots.insert(slot));
            }
        }
        assert_eq!(slots.len(), ir.locals - prior.locals);
        assert_eq!(
            ir.function_impls.len(),
            prior.function_impls.len() + added.len()
        );
        let mut restored = ir.clone();
        restored.locals = prior.locals;
        restored.stmts.truncate(prior.stmts.len());
        restored.functions.truncate(prior.functions.len());
        restored.function_impls = prior.function_impls.clone();
        restored.struct_schemas = prior.struct_schemas.clone();
        assert_eq!(
            format!("{restored:?}"),
            format!("{prior:?}"),
            "{stage}: complete retained IR and sidecars"
        );
        eprintln!(
            "URI_DATE_APPEND stage={stage} old_locals={} new_locals={} added_ids={added:?} retained_ir=true",
            prior.locals, ir.locals
        );
    }
    fn signature(ir: &FrontendIr, id: u16, args: Vec<T>, result: T, physical: vm::ValueType) {
        let d = ir.functions.iter().find(|d| d.index == id).unwrap();
        assert_eq!(usize::from(d.arity), args.len());
        assert_eq!(
            d.arg_schemas,
            args.into_iter().map(Some).collect::<Vec<_>>()
        );
        assert_eq!(d.return_schema, Some(result));
        assert_eq!(d.return_type, physical);
        assert!(!d.exported);
        eprintln!("URI_DATE_ENTRY {d:?}");
    }
    impl SourcePlugin for Shared {
        fn descriptor(&self) -> vm::SourceFrontendDescriptor {
            JAVASCRIPT_ES5_PLUGIN.descriptor()
        }
        fn parse_source(
            &self,
            source: &str,
            context: &vm::SourceFrontendContext<'_>,
        ) -> Result<FrontendIr, vm::ParseError> {
            let mut ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
            assert_eq!(source, DATE_ES5_CALLER_SOURCE);
            let parsed = ir.clone();
            let input = context.install_host_function(&mut ir, "date_es5_caller::input", 1)?;
            assert!(!parsed.functions.iter().any(|d| d.index == input));
            assert!(!parsed.function_impls.contains_key(&input));
            assert_eq!(
                ir.host_api_metadata.as_ref().unwrap().candidates(input),
                Some(&[date_es5_input_schema()][..])
            );
            // install_host_function supplies an actual FunctionDecl as well as
            // metadata. This does not exercise Date's metadata-only-hole case.
            let imported = ir.functions.iter_mut().find(|d| d.index == input).unwrap();
            // Core installs an unannotated import; annotate this new fixture
            // declaration with the exact catalog signature before composition.
            imported.arg_schemas = vec![Some(T::Int)];
            imported.return_schema = Some(T::Number);
            imported.return_type = vm::ValueType::Float;
            assert_eq!(imported.arg_schemas, vec![Some(T::Int)]);
            assert_eq!(imported.return_schema, Some(T::Number));
            assert!(!ir.function_impls.contains_key(&input));
            if let Some(metadata) = &parsed.host_api_metadata {
                for id in metadata.function_indices() {
                    assert_eq!(
                        ir.host_api_metadata.as_ref().unwrap().candidates(id),
                        metadata.candidates(id)
                    );
                }
            }
            let mut restored = ir.clone();
            restored.functions.truncate(parsed.functions.len());
            restored.host_api_metadata = parsed.host_api_metadata.clone();
            assert_eq!(format!("{restored:?}"), format!("{parsed:?}"));
            let before = ir.clone();
            let bridge = prelude::utf16_bridge::append(&mut ir)?;
            preserved(&before, &ir, "utf16_bridge");
            let before = ir.clone();
            let uri = prelude::uri::append(&mut ir)?;
            preserved(&before, &ir, "uri");
            let before = ir.clone();
            let date = prelude::date::append(&mut ir)?;
            preserved(&before, &ir, "date");
            let carrier_schema = T::Named("__CompatRefCarrier".into(), vec![]);
            let array = T::Array(Box::new(T::Int));
            signature(
                &ir,
                bridge.managed_to_units,
                vec![carrier_schema.clone(), representation::schema()],
                array.clone(),
                vm::ValueType::Array,
            );
            signature(
                &ir,
                bridge.units_to_managed,
                vec![carrier_schema.clone(), array.clone(), T::Int, T::Int],
                representation::schema(),
                vm::ValueType::Map,
            );
            signature(
                &ir,
                uri,
                vec![array.clone(), T::Int, T::Int],
                prelude::uri::result_schema(),
                vm::ValueType::Map,
            );
            signature(
                &ir,
                date.utc,
                vec![T::Number; 7],
                T::Number,
                vm::ValueType::Float,
            );
            let T::Named(name, _) = prelude::uri::result_schema() else {
                unreachable!()
            };
            let T::Object(fields) = &ir.struct_schemas[&name].body_schema else {
                unreachable!()
            };
            assert_eq!(fields.len(), 2);
            assert_eq!(fields["status"], T::Int);
            assert_eq!(fields["output"], array);
            let make = helper(&ir, "__compat_ref_make");
            let carriers: Vec<_> = parsed
                .stmts
                .iter()
                .filter_map(|s| match s {
                    S::Let {
                        index,
                        expr: E::Call(id, _, _, _, _),
                        ..
                    } if *id == make => Some(*index),
                    _ => None,
                })
                .collect();
            assert_eq!(carriers.len(), 1);
            let carrier = carriers[0];
            for name in ["answer", "n"] {
                let index = parsed
                    .local_bindings
                    .iter()
                    .rfind(|(n, _)| n == name)
                    .unwrap()
                    .1;
                ir.stmts.push(S::Expr {
                    expr: E::Var(index),
                    line: 1,
                });
            }
            let utc = bind(
                &mut ir,
                T::Number,
                call(
                    date.utc,
                    (0..7).map(|i| call(input, vec![E::Int(i)])).collect(),
                ),
            );
            let enter = helper(&ir, heap::ROOT_ENTER_HELPER);
            let frame = bind(
                &mut ir,
                T::Int,
                call(enter, vec![E::Var(carrier), E::Int(3)]),
            );
            let control = bind(
                &mut ir,
                representation::schema(),
                call(
                    bridge.units_to_managed,
                    vec![
                        E::Var(carrier),
                        units(&[75, 0, 0xd800]),
                        E::Var(frame),
                        E::Int(2),
                    ],
                ),
            );
            let selected = bind(
                &mut ir,
                array.clone(),
                E::IfElse {
                    condition: Box::new(E::Eq(Box::new(E::Var(utc)), Box::new(E::Float(0.0)))),
                    then_expr: Box::new(units(&[37, 52, 49])),
                    else_expr: Box::new(units(&[37, 70, 48, 37, 57, 68, 37, 56, 52, 37, 57, 69])),
                },
            );
            let encoded = bind(
                &mut ir,
                representation::schema(),
                call(
                    bridge.units_to_managed,
                    vec![E::Var(carrier), E::Var(selected), E::Var(frame), E::Int(0)],
                ),
            );
            let decoded_input = bind(
                &mut ir,
                array.clone(),
                call(
                    bridge.managed_to_units,
                    vec![E::Var(carrier), E::Var(encoded)],
                ),
            );
            let decoded = bind(
                &mut ir,
                prelude::uri::result_schema(),
                call(uri, vec![E::Var(decoded_input), E::Int(3), E::Int(65536)]),
            );
            let output = bind(
                &mut ir,
                representation::schema(),
                call(
                    bridge.units_to_managed,
                    vec![
                        E::Var(carrier),
                        prelude::object::field(E::Var(decoded), "output"),
                        E::Var(frame),
                        E::Int(1),
                    ],
                ),
            );
            let encoded_alias = bind(&mut ir, representation::schema(), E::Var(encoded));
            let output_alias = bind(&mut ir, representation::schema(), E::Var(output));
            for expr in [
                E::Var(utc),
                E::Var(carrier),
                E::Var(frame),
                E::Var(encoded),
                E::Var(output),
                E::Var(control),
                E::Var(encoded_alias),
                E::Var(output_alias),
                prelude::object::field(E::Var(decoded), "status"),
                E::Var(selected),
                E::Var(decoded_input),
            ] {
                ir.stmts.push(S::Expr { expr, line: 1 });
            }
            for name in [
                heap::CHECKED_BASE_HELPER,
                "__compat_ref_snapshot_values",
                heap::HEAP_COLLECT_HELPER,
                heap::HEAP_OBSERVE_HELPER,
                heap::ROOT_LEAVE_HELPER,
            ] {
                ir.stmts.push(S::Expr {
                    expr: E::FunctionRef(helper(&ir, name), vec![]),
                    line: 1,
                });
            }
            assert_eq!(
                format!("{:?}", &ir.stmts[..parsed.stmts.len()]),
                format!("{:?}", parsed.stmts)
            );
            eprintln!(
                "URI_DATE_PRODUCER source={source:?} input={input} utc={} uri={uri} bridge_to={} bridge_from={} carrier_local={carrier} frame_local={frame} original_script_preserved=true metadata_only_holes_exercised=false",
                date.utc, bridge.managed_to_units, bridge.units_to_managed
            );
            Ok(ir)
        }
        fn module_type_exports(
            &self,
            source: &str,
            context: &vm::SourceFrontendContext<'_>,
            ir: &FrontendIr,
        ) -> Result<Vec<vm::FrontendTypeExport>, vm::ParseError> {
            JAVASCRIPT_ES5_PLUGIN.module_type_exports(source, context, ir)
        }
        fn validate_typed_ir(
            &self,
            typed_ir: &vm::compiler::FrontendTypedIr<'_>,
        ) -> Result<(), vm::compiler::CompileError> {
            JAVASCRIPT_ES5_PLUGIN.validate_typed_ir(typed_ir)
        }
        fn parser_dialect(&self) -> Option<&'static dyn vm::compiler::ParserDialect> {
            JAVASCRIPT_ES5_PLUGIN.parser_dialect()
        }
        fn parse_module_imports(
            &self,
            source: &str,
            path: &std::path::Path,
            context: &vm::SourceFrontendContext<'_>,
        ) -> Result<Vec<vm::ModuleImport>, vm::SourcePathError> {
            JAVASCRIPT_ES5_PLUGIN.parse_module_imports(source, path, context)
        }
        fn strip_import_directives(&self, source: &str) -> String {
            JAVASCRIPT_ES5_PLUGIN.strip_import_directives(source)
        }
    }
    fn invoke(machine: &mut vm::Vm, function: &Value, args: &[Value]) -> Value {
        machine.set_fuel(10_000_000);
        assert_eq!(
            machine.start_callable(function.clone(), args).unwrap(),
            vm::VmStatus::Halted
        );
        assert!(!machine.jit_config().enabled);
        assert_eq!(machine.aot_exec_count(), 0);
        assert_eq!(machine.jit_native_exec_count(), 0);
        assert!(machine.capture_certification_stats().unwrap().valid);
        let fuel = 10_000_000 - machine.get_fuel().unwrap();
        eprintln!("URI_DATE_INVOKE fuel_used={fuel}");
        machine.take_callable_result().unwrap()
    }
    fn cells(value: &Value) -> Vec<i64> {
        let Value::Array(values) = value else {
            panic!("actual Core array: {value:?}")
        };
        values
            .iter()
            .map(|v| match v {
                Value::Int(i) => *i,
                Value::Float(f) if f.is_finite() && f.fract() == 0.0 => *f as i64,
                _ => panic!("numeric cell {v:?}"),
            })
            .collect()
    }
    fn integer(value: Value) -> i64 {
        let Value::Int(i) = value else {
            panic!("Int {value:?}")
        };
        i
    }
    fn observe(
        machine: &mut vm::Vm,
        functions: &[Value],
        carrier: &Value,
        string: &Value,
        expected: &[i64],
    ) {
        assert_eq!(field(string, "tag"), Value::Int(representation::STRING));
        let generation = integer(field(string, "generation"));
        let slot = integer(field(string, "ref_slot"));
        let base = integer(invoke(
            machine,
            &functions[0],
            &[carrier.clone(), Value::Int(slot), Value::Int(generation)],
        ));
        let raw = cells(&invoke(
            machine,
            &functions[1],
            std::slice::from_ref(carrier),
        ));
        let at = |index: i64| raw[usize::try_from(index).unwrap()];
        assert_eq!(at(base), heap::KIND_STRING);
        assert_eq!(at(base + 1), generation);
        let extent = at(base - heap::RAW_HEADER_CELLS);
        assert_eq!(
            at(base - heap::RAW_HEADER_CELLS + heap::RAW_CLASS),
            heap::RAW_CLASS_NODE
        );
        let len = at(base + 2);
        assert!(len >= 0 && extent >= heap::RAW_HEADER_CELLS + heap::STRING_HEADER_CELLS + len);
        assert!(base + heap::STRING_HEADER_CELLS + len <= raw.len() as i64);
        let payload: Vec<_> = (0..len)
            .map(|i| at(base + heap::STRING_HEADER_CELLS + i))
            .collect();
        assert_eq!(payload, expected);
        assert!(payload.iter().all(|n| (0..65536).contains(n)));
        eprintln!(
            "URI_DATE_MANAGED tag=4 slot={slot} generation={generation} base={base} kind=2 extent={extent} length={len} units={payload:?} backing_cells={}",
            raw.len()
        );
    }
    #[test]
    fn runtime_utc_selects_managed_uri_in_same_certified_realm() {
        worker(|| {
            let base = crate::runtime::cli::reference_array_cli_effective_catalog();
            let mut builder = vm::HostApiCatalog::builder();
            for resource in base.resources() {
                builder.resource(resource.clone());
            }
            for function in base.functions() {
                builder.function(function.clone());
            }
            builder.function(date_es5_input_schema());
            let catalog = Arc::new(builder.build().unwrap());
            let options = || {
                vm::CompileSourceFileOptions::new()
                    .with_source_plugin(&SHARED)
                    .with_host_api_catalog(catalog.clone())
            };
            assert_eq!(
                SHARED.descriptor().id,
                JAVASCRIPT_ES5_PLUGIN.descriptor().id
            );
            assert_eq!(SHARED.descriptor().extensions, &["js"]);
            assert_eq!(
                SHARED.descriptor().typing_policy,
                vm::FrontendTypingPolicy::Strict
            );
            assert!(SHARED.descriptor().supports_ownership_checking);
            let rejection = vm::compile_source_with_flavor_and_options_with_capture_certification(
                "var n: number = 6;n;",
                javascript_es5_flavor(),
                options(),
            )
            .err()
            .expect("ES5 rejects TS");
            assert!(format!("{rejection:?}").contains("JavaScript ES5 parse error"));
            for (inputs, expected, percent, decoded) in [
                ([1970.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0], 0.0, "%41", vec![65]),
                (
                    [
                        9007199254740992.0,
                        f64::from_bits(0xc377fffffffffa3a),
                        1.0,
                        0.0,
                        0.0,
                        0.0,
                        0.0,
                    ],
                    20995200000.0,
                    "%F0%9D%84%9E",
                    vec![55348, 56606],
                ),
            ] {
                let state = Arc::new(std::sync::Mutex::new(DateEs5Inputs::default()));
                let issued = vm::compile_source_with_flavor_and_options_with_capture_certification(
                    DATE_ES5_CALLER_SOURCE,
                    javascript_es5_flavor(),
                    options(),
                )
                .expect("completed real ES5 bridge/URI/Date strict ownership");
                assert!(state.lock().unwrap().values.is_none());
                let mut machine = issued.into_vm().unwrap();
                machine.set_jit_config(vm::JitConfig {
                    enabled: false,
                    ..Default::default()
                });
                let mut registry = HostFunctionRegistry::restricted();
                registry.set_capability_profile(
                    vm::CapabilityProfile::builder()
                        .allow_host_import("date_es5_caller::input")
                        .build(),
                );
                for module in [
                    crate::runtime::array::typescript_array_host_module(),
                    crate::runtime::array::typescript_owned_array_host_module(),
                    crate::runtime::array::typescript_array_snapshot_host_module(),
                    crate::runtime::scalar_math::typescript_scalar_math_host_module(),
                    crate::runtime::scalar_error::typescript_error_host_module(),
                ] {
                    module
                        .install_from_catalog(&mut registry, &catalog)
                        .unwrap();
                }
                for name in ["math::floor", "math::trunc", "math::abs", "math::is_finite"] {
                    registry.allow_builtin(name).unwrap();
                }
                let callback = state.clone();
                registry
                    .register_catalog(
                        vm::host_api::HostImportSchema::from_function(
                            &catalog,
                            &date_es5_input_schema(),
                        ),
                        move || Box::new(DateEs5Input(callback.clone())),
                    )
                    .unwrap();
                registry.bind_vm_cached(&mut machine).unwrap();
                crate::runtime::array::initialize_typescript_owned_arrays(
                    &mut machine,
                    crate::runtime::array::OwnedArrayLimits::default(),
                )
                .unwrap();
                assert!(machine.capture_certification_stats().unwrap().valid);
                assert!(state.lock().unwrap().observed.is_empty());
                state.lock().unwrap().values = Some(inputs);
                machine.set_fuel(10_000_000);
                assert_eq!(machine.run().unwrap(), vm::VmStatus::Halted);
                assert!(!machine.jit_config().enabled);
                assert_eq!(machine.aot_exec_count(), 0);
                assert_eq!(machine.jit_native_exec_count(), 0);
                assert!(machine.capture_certification_stats().unwrap().valid);
                assert_eq!(state.lock().unwrap().observed, [0, 1, 2, 3, 4, 5, 6]);
                let fuel = 10_000_000 - machine.get_fuel().unwrap();
                let result = machine.stack().to_vec();
                assert_eq!(result.len(), 19);
                assert_eq!(
                    field(&result[0], "tag"),
                    Value::Int(representation::BOOLEAN)
                );
                assert_eq!(field(&result[0], "flag"), Value::Bool(true));
                for value in &result[1..3] {
                    assert_eq!(field(value, "tag"), Value::Int(representation::NUMBER));
                    assert_eq!(field(value, "number"), Value::Float(6.0));
                }
                assert_eq!(result[3], Value::Float(expected));
                let carrier = &result[4];
                let frame = integer(result[5].clone());
                assert_eq!(result[6], result[9]);
                assert_eq!(result[7], result[10]);
                assert_eq!(result[11], Value::Int(0));
                let encoded: Vec<_> = percent.bytes().map(i64::from).collect();
                assert_eq!(cells(&result[12]), encoded);
                assert_eq!(cells(&result[13]), encoded);
                let functions = &result[14..];
                let metric = |machine: &mut vm::Vm, code| {
                    integer(invoke(
                        machine,
                        &functions[3],
                        &[carrier.clone(), Value::Int(code), Value::Int(0)],
                    ))
                };
                let roots = metric(&mut machine, heap::OBSERVE_ACTIVE_ROOT_CAPACITY);
                let collections = metric(&mut machine, heap::OBSERVE_COLLECTIONS);
                let stats = crate::runtime::array::owned_array_runtime_stats(&mut machine).unwrap();
                for phase in ["before_gc", "after_gc"] {
                    for (string, payload) in [
                        (&result[6], encoded.as_slice()),
                        (&result[7], decoded.as_slice()),
                        (&result[8], &[75, 0, 0xd800][..]),
                        (&result[9], encoded.as_slice()),
                        (&result[10], decoded.as_slice()),
                    ] {
                        observe(&mut machine, functions, carrier, string, payload);
                    }
                    let raw = cells(&invoke(
                        &mut machine,
                        &functions[1],
                        std::slice::from_ref(carrier),
                    ));
                    assert_eq!(raw[heap::HDR_PENDING_NODES as usize], 0);
                    assert_eq!(raw[heap::HDR_ROOT_TOP as usize], frame);
                    assert_eq!(raw[frame as usize + 3], 3);
                    for (entry, string) in [0, 1, 2].into_iter().zip(&result[6..9]) {
                        let address = frame as usize + 4 + entry * 3;
                        assert_eq!(raw[address], heap::ROOT_STRING);
                        assert_eq!(raw[address + 1], integer(field(string, "ref_slot")));
                        assert_eq!(raw[address + 2], integer(field(string, "generation")));
                    }
                    assert_eq!(
                        metric(&mut machine, heap::OBSERVE_ACTIVE_ROOT_CAPACITY),
                        roots
                    );
                    assert_eq!(
                        crate::runtime::array::owned_array_runtime_stats(&mut machine).unwrap(),
                        stats,
                        "temporary Core arrays do not charge B1"
                    );
                    assert_eq!(machine.host_context().resource_count(), 1);
                    assert_eq!((stats.pending, stats.registered), (0, 1));
                    eprintln!(
                        "URI_DATE_LIVE phase={phase} carrier={carrier:?} frame={frame} roots={roots} b1={stats:?} resources=1 certified={:?}",
                        machine.capture_certification_stats().unwrap()
                    );
                    if phase == "before_gc" {
                        invoke(&mut machine, &functions[2], std::slice::from_ref(carrier));
                        assert_eq!(
                            metric(&mut machine, heap::OBSERVE_COLLECTIONS),
                            collections + 1
                        );
                    }
                }
                invoke(
                    &mut machine,
                    &functions[4],
                    &[carrier.clone(), Value::Int(frame)],
                );
                assert_eq!(
                    metric(&mut machine, heap::OBSERVE_ACTIVE_ROOT_CAPACITY),
                    roots - 3
                );
                eprintln!(
                    "URI_DATE_EXEC utc={expected:?} input={percent:?} output={decoded:?} status=0 callbacks={:?} script=true answer=Float(6) effect=Float(6) halted=true jit=false native=0 fuel_used={fuel}",
                    state.lock().unwrap().observed
                );
                machine.reset_for_reuse().unwrap();
                assert_eq!(machine.host_context().resource_count(), 0);
                assert!(crate::runtime::array::owned_array_runtime_stats(&mut machine).is_none());
                assert!(!machine.capture_certification_stats().unwrap().valid);
                eprintln!(
                    "URI_DATE_RESET resources=0 scope_directory=false certification_valid=false"
                );
            }
        });
    }
}
