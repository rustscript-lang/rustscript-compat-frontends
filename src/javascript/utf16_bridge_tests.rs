//! Actual certified guest adapters; host observations never fabricate a B1 owner.
use super::prelude::utf16_bridge;
use vm::{Expr as E, FrontendIr, SourcePlugin, Stmt as S, TypeSchema as T, Value, VmStatus};

const HELPERS: &[&str] = &[
    "__compat_es5_managed_to_units",
    "__compat_es5_units_to_managed",
    "__compat_es5_checked_base",
    "__compat_ref_read",
    "__compat_ref_write",
    "__compat_ref_snapshot_values",
    "__compat_es5_heap_collect",
    "__compat_es5_heap_observe",
    "__compat_es5_root_leave",
    "__compat_es5_uri_kernel",
    "__utf16_test_uri_bridge",
];
struct BridgePlugin;
static PLUGIN: BridgePlugin = BridgePlugin;
fn call(index: u16, args: Vec<E>) -> E {
    E::Call(index, vec![], args, None, None)
}
fn helper(ir: &FrontendIr, name: &str) -> u16 {
    ir.functions
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("missing real helper {name}"))
        .index
}
fn fresh(ir: &mut FrontendIr) -> u16 {
    let slot = u16::try_from(ir.locals).unwrap();
    ir.locals += 1;
    slot
}
impl SourcePlugin for BridgePlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("utf16-bridge-test"),
            extensions: &["js"],
            typing_policy: vm::FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<FrontendIr, vm::ParseError> {
        let mut ir = super::JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        let functions = ir.functions.clone();
        let implementations = ir.function_impls.clone();
        let bindings = ir.local_bindings.clone();
        let statements = format!("{:?}", ir.stmts);
        let schemas = ir.struct_schemas.clone();
        let old_locals = ir.locals;
        if source.contains("contract-rejections") {
            let reject = |mut bad: FrontendIr, fragment: &str| {
                let before = format!("{bad:?}");
                let error = utf16_bridge::append(&mut bad)
                    .err()
                    .expect("reject invalid composition");
                assert!(format!("{error:?}").contains(fragment), "{error:?}");
                assert_eq!(
                    format!("{bad:?}"),
                    before,
                    "append rejection is transactional"
                );
            };
            let mut bad = ir.clone();
            bad.functions
                .iter_mut()
                .find(|f| f.name == "__compat_es5_checked_base")
                .unwrap()
                .arg_schemas[1] = Some(T::Number);
            reject(bad, "incompatible actual helper");
            let mut bad = ir.clone();
            bad.struct_schemas
                .get_mut("__CompatEs5Primitive")
                .unwrap()
                .body_schema = T::Int;
            reject(bad, "incompatible schema");
            let mut bad = ir.clone();
            bad.functions
                .retain(|f| f.name != "__compat_es5_node_publish");
            reject(bad, "missing actual helper");
            let mut bad = ir.clone();
            bad.locals = 65530;
            reject(bad, "local limit");
            let mut bad = ir.clone();
            let mut last = bad.functions.last().unwrap().clone();
            last.name = "__reserved_test".into();
            last.index = u16::MAX;
            bad.functions.push(last);
            reject(bad, "function limit");
            eprintln!("UTF16_COMPOSITION rejection_cases=5 unchanged_on_rejection=true");
        }
        let entries = utf16_bridge::append(&mut ir)?;
        assert_eq!(&ir.functions[..functions.len()], functions);
        for (index, body) in implementations {
            assert_eq!(
                format!("{:?}", ir.function_impls[&index]),
                format!("{body:?}")
            );
        }
        assert_eq!(ir.local_bindings, bindings);
        // Compare the complete original statement prefix, not just its length.
        let prefix_end = ir.stmts.len() - 2;
        assert_eq!(format!("{:?}", &ir.stmts[..prefix_end]), statements);
        for (name, schema) in schemas {
            assert_eq!(
                format!("{:?}", ir.struct_schemas[&name]),
                format!("{schema:?}")
            );
        }
        assert!(ir.locals > old_locals);
        assert_eq!(helper(&ir, HELPERS[0]), entries.managed_to_units);
        assert_eq!(helper(&ir, HELPERS[1]), entries.units_to_managed);
        for index in [entries.managed_to_units, entries.units_to_managed] {
            let f = ir.functions.iter().find(|f| f.index == index).unwrap();
            assert!(!f.exported);
            assert!(ir.function_impls[&index].capture_copies.is_empty());
            assert!(
                ir.function_impls[&index]
                    .param_slots
                    .iter()
                    .all(|s| usize::from(*s) >= old_locals)
            );
        }
        assert_eq!(ir.locals, old_locals + 20);
        assert!(
            utf16_bridge::append(&mut ir.clone()).is_err(),
            "duplicate append fails closed"
        );
        eprintln!(
            "UTF16_ABI original_locals={old_locals} final_locals={} to={} from={} statements_prefix_preserved=true captures_preserved=true",
            ir.locals, entries.managed_to_units, entries.units_to_managed
        );
        for index in [entries.managed_to_units, entries.units_to_managed] {
            let f = ir.functions.iter().find(|f| f.index == index).unwrap();
            eprintln!(
                "UTF16_DECL {f:?} params={:?}",
                ir.function_impls[&index].param_slots
            );
        }
        let kernel = super::prelude::uri::append(&mut ir)?;
        // Private test-only composed guest path. The host supplies dynamic
        // carrier/String/frame values, then observes the result independently.
        let wrapper = ir.functions.iter().map(|f| f.index).max().unwrap() + 1;
        let c = fresh(&mut ir);
        let s = fresh(&mut ir);
        let frame = fresh(&mut ir);
        let entry = fresh(&mut ir);
        let units = fresh(&mut ir);
        let result = fresh(&mut ir);
        let carrier_schema = ir
            .functions
            .iter()
            .find(|f| f.index == entries.managed_to_units)
            .unwrap()
            .arg_schemas[0]
            .clone()
            .unwrap();
        let mut declarations = vec![];
        let field = |slot, key: &str| super::prelude::object::field(E::Var(slot), key);
        super::prelude::heap::declare_helper(
            &mut ir,
            &mut declarations,
            "__utf16_test_uri_bridge",
            wrapper,
            4,
            vec![
                "carrier".into(),
                "string".into(),
                "frame".into(),
                "entry".into(),
            ],
            vec![
                Some(carrier_schema),
                Some(super::representation::schema()),
                Some(T::Int),
                Some(T::Int),
            ],
            Some(super::representation::schema()),
            vm::ValueType::Map,
            vec![c, s, frame, entry],
            vec![
                S::Let {
                    index: units,
                    declared_schema: Some(T::Array(Box::new(T::Int))),
                    expr: call(entries.managed_to_units, vec![E::Var(c), E::Var(s)]),
                    line: 1,
                },
                S::Let {
                    index: result,
                    declared_schema: Some(super::prelude::uri::result_schema()),
                    expr: call(kernel, vec![E::Var(units), E::Int(1), E::Int(65536)]),
                    line: 1,
                },
                super::lowerer::assert_stmt(
                    E::Eq(Box::new(field(result, "status")), Box::new(E::Int(0))),
                    1,
                ),
            ],
            call(
                entries.units_to_managed,
                vec![
                    E::Var(c),
                    field(result, "output"),
                    E::Var(frame),
                    E::Var(entry),
                ],
            ),
            1,
        );
        ir.stmts.extend(declarations);
        let make = helper(&ir, "__compat_ref_make");
        let carriers: Vec<_> = ir
            .stmts
            .iter()
            .filter_map(|s| match s {
                S::Let {
                    index,
                    expr: E::Call(target, _, _, _, _),
                    ..
                } if *target == make => Some(*index),
                _ => None,
            })
            .collect();
        assert_eq!(carriers.len(), 1);
        let carrier = carriers[0];
        let binding = |name: &str| {
            ir.local_bindings
                .iter()
                .rfind(|(n, _)| n == name)
                .unwrap()
                .1
        };
        let string = binding("s");
        let object = binding("o");
        let frame = fresh(&mut ir);
        let enter = helper(&ir, "__compat_es5_root_enter");
        ir.stmts.push(S::Let {
            index: frame,
            declared_schema: Some(T::Int),
            expr: call(enter, vec![E::Var(carrier), E::Int(8)]),
            line: 1,
        });
        // This is the caller's real frame. It remains live until explicit leave/reset.
        for expression in [
            E::Var(carrier),
            E::Var(string),
            E::Var(object),
            E::Var(frame),
        ] {
            ir.stmts.push(S::Expr {
                expr: expression,
                line: 1,
            });
        }
        for name in HELPERS {
            ir.stmts.push(S::Expr {
                expr: E::FunctionRef(helper(&ir, name), vec![]),
                line: 1,
            });
        }
        Ok(ir)
    }
}
struct Machine {
    vm: vm::Vm,
    carrier: Value,
    string: Value,
    object: Value,
    frame: Value,
    functions: Vec<Value>,
}
impl Machine {
    fn new(source: &str) -> Self {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let issued = vm::compile_source_with_flavor_and_options_with_capture_certification(
            source,
            vm::SourceFlavor::Plugin(vm::FrontendId::new("utf16-bridge-test")),
            vm::CompileSourceFileOptions::new()
                .with_source_plugin(&PLUGIN)
                .with_host_api_catalog(catalog.clone()),
        )
        .expect("actual completed JS IR, strict typing and ownership");
        let mut vm = issued.into_vm().expect("preserve issued capture authority");
        vm.set_jit_config(vm::JitConfig {
            enabled: false,
            ..Default::default()
        });
        crate::runtime::array::initialize_typescript_owned_arrays(
            &mut vm,
            crate::runtime::array::OwnedArrayLimits::default(),
        )
        .unwrap();
        let mut registry = vm::HostFunctionRegistry::restricted();
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
        registry.allow_builtin("math::floor").unwrap();
        registry.bind_vm_cached(&mut vm).unwrap();
        vm.set_fuel(10_000_000);
        assert_eq!(vm.run().expect("real JS producer"), VmStatus::Halted);
        let tail = &vm.stack()[vm.stack().len() - HELPERS.len() - 4..];
        let result = Self {
            carrier: tail[0].clone(),
            string: tail[1].clone(),
            object: tail[2].clone(),
            frame: tail[3].clone(),
            functions: tail[4..].to_vec(),
            vm,
        };
        assert!(
            result
                .vm
                .capture_certification_stats()
                .is_some_and(|s| s.valid)
        );
        result
    }
    fn invoke(&mut self, i: usize, args: &[Value]) -> Result<Value, vm::VmError> {
        self.vm.set_fuel(10_000_000);
        let status = self.vm.start_callable(self.functions[i].clone(), args)?;
        assert_eq!(status, VmStatus::Halted);
        assert_eq!(self.vm.aot_exec_count(), 0);
        assert_eq!(self.vm.jit_native_exec_count(), 0);
        assert!(!self.vm.jit_config().enabled);
        assert!(
            self.vm
                .capture_certification_stats()
                .is_some_and(|s| s.valid)
        );
        Ok(self.vm.take_callable_result().expect("guest result"))
    }
    fn read_units(&mut self, string: Value) -> Vec<i64> {
        let value = self
            .invoke(0, &[self.carrier.clone(), string])
            .expect("checked managed read");
        ints(&value)
    }
    fn publish_units(&mut self, units: Value, entry: i64) -> Value {
        self.invoke(
            1,
            &[
                self.carrier.clone(),
                units,
                self.frame.clone(),
                Value::Int(entry),
            ],
        )
        .expect("caller-rooted publication")
    }
    fn observe(&mut self, string: Value) -> Vec<i64> {
        assert_eq!(
            field(&string, "tag"),
            Value::Int(4),
            "actual managed String tag"
        );
        let base = self
            .invoke(
                2,
                &[
                    self.carrier.clone(),
                    field(&string, "ref_slot"),
                    field(&string, "generation"),
                ],
            )
            .expect("production checked handle");
        let Value::Int(base) = base else {
            panic!("Int base")
        };
        let carrier = self.carrier.clone();
        let raw = self
            .invoke(5, std::slice::from_ref(&carrier))
            .expect("production B1 snapshot while owner remains live");
        let Value::Array(raw) = raw else {
            panic!("snapshot")
        };
        let cell = |i: i64| match raw[usize::try_from(i).unwrap()] {
            Value::Int(i) => i,
            Value::Float(f) if f.fract() == 0.0 => f as i64,
            ref v => panic!("numeric cell {v:?}"),
        };
        assert_eq!(cell(base), 2);
        assert_eq!(cell(base + 1), int_field(&string, "generation"));
        let len = cell(base + 2);
        assert!(len >= 0 && base + 3 + len <= raw.len() as i64);
        (0..len).map(|i| cell(base + 3 + i)).collect()
    }
    fn snapshot(&mut self) -> Vec<i64> {
        let carrier = self.carrier.clone();
        let value = self.invoke(5, std::slice::from_ref(&carrier)).unwrap();
        let Value::Array(values) = value else {
            panic!("B1 snapshot")
        };
        values
            .iter()
            .map(|v| match v {
                Value::Int(i) => *i,
                Value::Float(f) if f.fract() == 0.0 => *f as i64,
                _ => panic!("numeric backing"),
            })
            .collect()
    }
    fn metric(&mut self, which: i64) -> i64 {
        let value = self
            .invoke(7, &[self.carrier.clone(), Value::Int(which), Value::Int(0)])
            .unwrap();
        let Value::Int(n) = value else {
            panic!("Int metric")
        };
        n
    }
    fn force_next_collection(&mut self) {
        self.invoke(
            4,
            &[self.carrier.clone(), Value::Float(10.0), Value::Float(32.0)],
        )
        .unwrap();
    }
    fn reset(&mut self) {
        self.vm.reset_for_reuse().unwrap();
        assert_eq!(self.vm.host_context().resource_count(), 0);
        assert!(crate::runtime::array::owned_array_runtime_stats(&mut self.vm).is_none());
    }
}
fn field(value: &Value, name: &str) -> Value {
    let Value::Map(map) = value else {
        panic!("record {value:?}")
    };
    map.get(&Value::string(name)).unwrap().clone()
}
fn int_field(value: &Value, name: &str) -> i64 {
    let Value::Int(i) = field(value, name) else {
        panic!("Int field")
    };
    i
}
fn raw(units: &[i64]) -> Value {
    Value::array(units.iter().copied().map(Value::Int).collect())
}
fn ints(value: &Value) -> Vec<i64> {
    let Value::Array(values) = value else {
        panic!("Array(Int): {value:?}")
    };
    values
        .iter()
        .map(|v| match v {
            Value::Int(i) => *i,
            _ => panic!("Int: {v:?}"),
        })
        .collect()
}
fn worker(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}
#[test]
fn copies_aliases_forced_gc_and_prior_results() {
    worker(|| {
        let mut m = Machine::new(r#"var s="keep\0\uD800";var o={n:7};s===s;"#);
        let original = m.observe(m.string.clone());
        let object_base = m
            .invoke(
                2,
                &[
                    m.carrier.clone(),
                    field(&m.object, "ref_slot"),
                    field(&m.object, "generation"),
                ],
            )
            .unwrap();
        let Value::Int(object_base) = object_base else {
            panic!("base")
        };
        let before = m.snapshot();
        let object_cells = before[object_base as usize..object_base as usize + 12].to_vec();
        let mut source = raw(&[65, 0, 0xdfff]);
        let source_before = source.clone();
        m.force_next_collection();
        let collections = m.metric(5);
        let roots = m.metric(4);
        let first = m.publish_units(source.clone(), 0);
        assert_eq!(source, source_before, "borrowed source is unchanged");
        assert_eq!(m.metric(5), collections + 1, "real allocation collected");
        assert_eq!(
            m.metric(4),
            roots,
            "adapter does not allocate/release caller frames"
        );
        assert_eq!(m.metric(8), 0, "complete publication clears pending");
        let Value::Array(array) = &mut source else {
            unreachable!()
        };
        std::sync::Arc::make_mut(array)[0] = Value::Int(90);
        assert_eq!(
            m.observe(first.clone()),
            [65, 0, 0xdfff],
            "copy independent of source mutation"
        );
        for units in [
            vec![],
            vec![65, 66],
            vec![0, 0xffff, 0x4e2d],
            vec![0xd800],
            vec![0xdc00],
            vec![0xd83d, 0xde00],
            (0..4096).map(|i| (i * 17) % 65536).collect(),
        ] {
            m.force_next_collection();
            let output = m.publish_units(raw(&units), 1);
            assert_eq!(
                m.observe(output.clone()),
                units,
                "independent stored UTF16 observations"
            );
            let accounting = crate::runtime::array::owned_array_runtime_stats(&mut m.vm).unwrap();
            let mut returned = m.invoke(0, &[m.carrier.clone(), output.clone()]).unwrap();
            assert_eq!(ints(&returned), units);
            let Value::Array(array) = &mut returned else {
                unreachable!()
            };
            let capacity = array.capacity();
            assert!(capacity <= 65536);
            assert_eq!(
                crate::runtime::array::owned_array_runtime_stats(&mut m.vm).unwrap(),
                accounting,
                "Core array growth has no B1 charge"
            );
            if !array.is_empty() {
                std::sync::Arc::make_mut(array)[0] = Value::Int(12345);
            }
            assert_eq!(
                m.observe(output),
                units,
                "returned Core array cannot alias managed storage"
            );
            assert_eq!(
                m.observe(first.clone()),
                [65, 0, 0xdfff],
                "prior rooted result retained"
            );
            assert_eq!(
                m.observe(m.string.clone()),
                original,
                "unrelated rooted input preserved"
            );
            eprintln!(
                "UTF16_COPY units={} core_capacity={capacity} core_capacity_bytes={} b1_reserved_bytes={} registered={} pending={}",
                units.len(),
                capacity * std::mem::size_of::<Value>(),
                accounting.reserved_bytes,
                accounting.registered,
                accounting.pending
            );
        }
        let after = m.snapshot();
        assert_eq!(
            &after[object_base as usize..object_base as usize + 12],
            object_cells,
            "rooted unrelated object payload survives forced GC"
        );
        assert_eq!(m.vm.host_context().resource_count(), 1);
        m.invoke(8, &[m.carrier.clone(), m.frame.clone()]).unwrap();
        m.reset();
    });
}
#[test]
fn invalid_internal_inputs_refuse_and_reset() {
    worker(|| {
        for case in 0..10 {
            let mut m = Machine::new(r#"var s="input";var o={n:7};s===s;"#);
            let mut before = m.snapshot();
            let stats = crate::runtime::array::owned_array_runtime_stats(&mut m.vm).unwrap();
            let error = match case {
                0 => m
                    .invoke(
                        1,
                        &[
                            m.carrier.clone(),
                            raw(&[-1]),
                            m.frame.clone(),
                            Value::Int(0),
                        ],
                    )
                    .unwrap_err(),
                1 => m
                    .invoke(
                        1,
                        &[
                            m.carrier.clone(),
                            raw(&[65, 65536]),
                            m.frame.clone(),
                            Value::Int(0),
                        ],
                    )
                    .unwrap_err(),
                2 => {
                    let mut s = m.string.clone();
                    let Value::Map(map) = &mut s else {
                        unreachable!()
                    };
                    std::sync::Arc::make_mut(map).insert(Value::string("tag"), Value::Int(3));
                    m.invoke(0, &[m.carrier.clone(), s]).unwrap_err()
                }
                3 => {
                    let mut s = m.object.clone();
                    let Value::Map(map) = &mut s else {
                        unreachable!()
                    };
                    std::sync::Arc::make_mut(map).insert(Value::string("tag"), Value::Int(4));
                    m.invoke(0, &[m.carrier.clone(), s]).unwrap_err()
                }
                4 => {
                    let mut s = m.string.clone();
                    let g = int_field(&s, "generation");
                    let Value::Map(map) = &mut s else {
                        unreachable!()
                    };
                    std::sync::Arc::make_mut(map)
                        .insert(Value::string("generation"), Value::Int(g + 1));
                    m.invoke(0, &[m.carrier.clone(), s]).unwrap_err()
                }
                5 => m
                    .invoke(
                        1,
                        &[m.carrier.clone(), raw(&[65]), Value::Int(0), Value::Int(0)],
                    )
                    .unwrap_err(),
                6 => m
                    .invoke(
                        1,
                        &[
                            m.carrier.clone(),
                            raw(&[65]),
                            m.frame.clone(),
                            Value::Int(8),
                        ],
                    )
                    .unwrap_err(),
                8 | 9 => {
                    let base = m
                        .invoke(
                            2,
                            &[
                                m.carrier.clone(),
                                field(&m.string, "ref_slot"),
                                field(&m.string, "generation"),
                            ],
                        )
                        .unwrap();
                    let Value::Int(base) = base else {
                        panic!("Int base")
                    };
                    let offset = if case == 8 { 2 } else { 3 };
                    m.invoke(
                        4,
                        &[
                            m.carrier.clone(),
                            Value::Float((base + offset) as f64),
                            Value::Float(65536.0),
                        ],
                    )
                    .unwrap();
                    before = m.snapshot();
                    m.invoke(0, &[m.carrier.clone(), m.string.clone()])
                        .unwrap_err()
                }
                _ => m
                    .invoke(
                        1,
                        &[
                            m.carrier.clone(),
                            raw(&vec![65; 65537]),
                            m.frame.clone(),
                            Value::Int(0),
                        ],
                    )
                    .unwrap_err(),
            };
            assert!(
                format!("{error:?}").contains("assertion failed"),
                "fatal internal refusal {case}: {error:?}"
            );
            let after = m.snapshot();
            if case <= 4 || case >= 7 {
                assert_eq!(
                    after, before,
                    "preflight/checked read must not mutate backing"
                );
                assert_eq!(
                    crate::runtime::array::owned_array_runtime_stats(&mut m.vm).unwrap(),
                    stats
                );
            } else {
                assert_eq!(
                    after[8], 1,
                    "invalid destination retains opaque pending transaction until reset"
                );
                assert_eq!(
                    after[6], before[6],
                    "invalid destination never activates output"
                );
            }
            eprintln!(
                "UTF16_FATAL case={case} error={error:?} pending={} live={}",
                after[8], after[6]
            );
            m.reset();
        }
    });
}
#[test]
fn actual_remaining_storage_boundary_and_refusal() {
    worker(|| {
        let mut m = Machine::new(r#"var s="keep";var o={n:7};s===s;"#);
        let limits = crate::runtime::array::OwnedArrayLimits::default();
        let ceiling = 65536usize.min(limits.reserved_bytes_quota() / std::mem::size_of::<Value>());
        let chunk = ceiling / 4 - 512;
        for entry in 0..3 {
            let s = m.publish_units(raw(&vec![65; chunk]), entry);
            assert_eq!(m.observe(s), vec![65; chunk]);
        }
        m.invoke(6, &[m.carrier.clone()]).unwrap();
        let before = m.snapshot();
        let end = before[1] as usize;
        let length = ceiling - end - 6;
        assert!(length > 0);
        let mut cursor = 80usize;
        let mut free_max = 0usize;
        while cursor < end {
            let size = before[cursor] as usize;
            assert!(size >= 4);
            if before[cursor + 1] == 0 {
                free_max = free_max.max(size);
            }
            cursor += size;
        }
        assert_eq!(cursor, end);
        assert!(
            length + 7 > free_max,
            "boundary request exceeds every reusable free extent"
        );
        let output = m.publish_units(raw(&vec![0xd800; length]), 3);
        assert_eq!(m.observe(output), vec![0xd800; length]);
        let full = m.snapshot();
        assert_eq!(full[1] as usize, ceiling);
        assert_eq!(full[8], 0);
        let stats = crate::runtime::array::owned_array_runtime_stats(&mut m.vm).unwrap();
        let error = m
            .invoke(
                1,
                &[
                    m.carrier.clone(),
                    raw(&vec![65; length + 1]),
                    m.frame.clone(),
                    Value::Int(4),
                ],
            )
            .unwrap_err();
        assert!(
            format!("{error:?}").contains("assertion failed")
                || format!("{error:?}").contains("quota"),
            "actual capacity refusal {error:?}"
        );
        let refused = m.snapshot();
        assert_eq!(
            refused[8], 0,
            "reservation refused before pending publication"
        );
        assert_eq!(refused[6], full[6]);
        assert_eq!(refused[7], full[7]);
        assert_eq!(refused[4], full[4]);
        assert_eq!(
            crate::runtime::array::owned_array_runtime_stats(&mut m.vm)
                .unwrap()
                .reserved_bytes,
            stats.reserved_bytes
        );
        eprintln!(
            "UTF16_BOUNDARY size_of_value={} policy_cells={ceiling} previous_end={end} units={length} occupied_node_cells={} b1_reserved_bytes={} largest_free={free_max} refusal={error:?}",
            std::mem::size_of::<Value>(),
            full[7],
            stats.reserved_bytes
        );
        m.reset();
    });
}
#[test]
fn executed_private_uri_bridge() {
    worker(|| {
        let mut m = Machine::new(r#"var s="A /\u4E2D\uD83D\uDE00\0";var o={n:7};s===s;"#);
        let input = m.invoke(0, &[m.carrier.clone(), m.string.clone()]).unwrap();
        let source_units = vec![65, 32, 47, 0x4e2d, 0xd83d, 0xde00, 0];
        assert_eq!(ints(&input), source_units);
        let result = m
            .invoke(9, &[input, Value::Int(1), Value::Int(65536)])
            .unwrap();
        assert_eq!(field(&result, "status"), Value::Int(0));
        let expected: Vec<i64> = b"A%20%2F%E4%B8%AD%F0%9F%98%80%00"
            .iter()
            .map(|b| i64::from(*b))
            .collect();
        let output_units = field(&result, "output");
        assert_eq!(ints(&output_units), expected);
        m.force_next_collection();
        let string = m.publish_units(output_units, 0);
        assert_eq!(m.observe(string), expected);
        m.force_next_collection();
        let composed = m
            .invoke(
                10,
                &[
                    m.carrier.clone(),
                    m.string.clone(),
                    m.frame.clone(),
                    Value::Int(1),
                ],
            )
            .expect("one actual guest String -> array -> URI -> rooted String callable");
        assert_eq!(m.observe(composed), expected);
        assert_eq!(m.observe(m.string.clone()), source_units);
        let malformed = m
            .invoke(9, &[raw(&[0xd800]), Value::Int(1), Value::Int(65536)])
            .unwrap();
        assert_eq!(field(&malformed, "status"), Value::Int(1));
        assert!(ints(&field(&malformed, "output")).is_empty());
        eprintln!(
            "UTF16_URI private_guest_chain=true expected_units={} malformed_status=1 public_globals=false",
            expected.len()
        );
        m.reset();
    });
}
#[test]
fn runtime_units_to_independently_observed_string() {
    worker(|| {
        let mut m = Machine::new(r#"var s="keep";var o={n:7};s===s;"#);
        let units = vec![0, 65, 0xffff, 0xd800, 0x4e2d, 0xdc00, 0xd83d, 0xde00];
        let output = m.publish_units(raw(&units), 0);
        assert_eq!(m.observe(output), units);
        m.reset();
    });
}
#[test]
fn managed_source_to_exact_units() {
    worker(|| {
        let mut m = Machine::new(r#"var s="A\0\u4E2D\uD800\uDC00\uDFFF";var o={n:7};s===s;"#);
        assert_eq!(
            m.read_units(m.string.clone()),
            vec![65, 0, 0x4e2d, 0xd800, 0xdc00, 0xdfff]
        );
        m.reset();
        for (source, expected) in [
            (r#"var s="";var o={n:7};s===s;"#, vec![]),
            (
                r#"var s="ASCII";var o={n:7};s===s;"#,
                vec![65, 83, 67, 73, 73],
            ),
        ] {
            let mut m = Machine::new(source);
            assert_eq!(m.read_units(m.string.clone()), expected);
            m.reset();
        }
    });
}
#[test]
fn completed_ir_rejects_incompatible_contracts_without_mutation() {
    worker(|| {
        let mut m = Machine::new(r#"/*contract-rejections*/var s="checked";var o={n:7};s===s;"#);
        assert_eq!(
            m.read_units(m.string.clone()),
            vec![99, 104, 101, 99, 107, 101, 100]
        );
        m.reset();
    });
}
