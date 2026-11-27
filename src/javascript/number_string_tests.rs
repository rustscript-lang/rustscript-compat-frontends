//! Private units-kernel evidence, separate from certified managed ES5 evidence.
use super::prelude::number_string;
use super::prelude::utf16_bridge;
use vm::{Expr as E, SourcePlugin, Stmt as S, TypeSchema as T, Value, VmStatus};

const MANAGED_HELPERS: &[&str] = &[
    "__compat_es5_number_string_units",
    "__compat_es5_number_string_managed",
    "__compat_es5_checked_base",
    "__compat_ref_write",
    "__compat_ref_snapshot_values",
    "__compat_es5_heap_collect",
    "__compat_es5_heap_observe",
    "__compat_es5_root_leave",
    "__compat_es5_units_to_managed",
];
fn call(index: u16, args: Vec<E>) -> E {
    E::Call(index, vec![], args, None, None)
}
fn helper(ir: &vm::FrontendIr, name: &str) -> u16 {
    ir.functions
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("missing {name}"))
        .index
}
fn fresh(ir: &mut vm::FrontendIr) -> u16 {
    let slot = u16::try_from(ir.locals).unwrap();
    ir.locals += 1;
    slot
}
fn bind(index: u16, schema: T, expr: E) -> S {
    S::Let {
        index,
        declared_schema: Some(schema),
        expr,
        line: 1,
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
fn text(units: &[i64]) -> String {
    String::from_utf8(units.iter().map(|i| u8::try_from(*i).unwrap()).collect()).unwrap()
}
struct Managed;
static MANAGED: Managed = Managed;
impl SourcePlugin for Managed {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("number-string-managed"),
            extensions: &["js"],
            typing_policy: vm::FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = super::JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        let bridge = utf16_bridge::append(&mut ir)?;
        let original = ir.clone();
        // Contract negatives exercise real completed ES5 IR and exact bridge entries.
        if source.contains("contract-rejections") {
            for case in 0..10 {
                let mut bad = original.clone();
                let mut supplied = utf16_bridge::Entries {
                    managed_to_units: bridge.managed_to_units,
                    units_to_managed: bridge.units_to_managed,
                };
                match case {
                    0 => supplied.units_to_managed = bridge.managed_to_units,
                    1 => {
                        bad.functions
                            .iter_mut()
                            .find(|f| f.index == bridge.units_to_managed)
                            .unwrap()
                            .arg_schemas[1] = Some(T::Array(Box::new(T::Number)))
                    }
                    2 => {
                        bad.functions
                            .iter_mut()
                            .find(|f| f.index == bridge.units_to_managed)
                            .unwrap()
                            .exported = true
                    }
                    3 => bad
                        .function_impls
                        .get_mut(&bridge.units_to_managed)
                        .unwrap()
                        .capture_copies
                        .push((0, 1)),
                    4 => {
                        bad.struct_schemas
                            .get_mut("__CompatRefCarrier")
                            .unwrap()
                            .body_schema = T::Int
                    }
                    5 => bad.locals = 65530,
                    6 => {
                        let mut f = bad.functions.last().unwrap().clone();
                        f.index = u16::MAX;
                        bad.functions.push(f);
                    }
                    7 => bad.functions.push(bad.functions.last().unwrap().clone()),
                    8 => {
                        let index = bad
                            .host_api_metadata
                            .as_ref()
                            .unwrap()
                            .function_indices()
                            .next()
                            .unwrap();
                        bad.functions.retain(|f| f.index != index);
                    }
                    _ => {
                        bad.functions
                            .iter_mut()
                            .find(|f| f.index == bridge.units_to_managed)
                            .unwrap()
                            .name = "__forged_bridge".into();
                    }
                }
                let before = format!("{bad:?}");
                assert!(
                    number_string::append(&mut bad, &supplied).is_err(),
                    "case {case}"
                );
                assert_eq!(format!("{bad:?}"), before, "transactional rejection {case}");
            }
            eprintln!("NUMBER_ABI rejection_cases=10 transactional=true");
        }
        let mut units_only = ir.clone();
        let units_entry = number_string::append_units(&mut units_only)?;
        let entries = number_string::append(&mut ir, &bridge)?;
        assert_eq!(entries.units, units_entry);
        assert_eq!(
            format!("{:?}", ir.function_impls[&entries.units]),
            format!("{:?}", units_only.function_impls[&units_entry]),
            "same semantic body"
        );
        let mut restored = ir.clone();
        restored.functions.truncate(original.functions.len());
        restored
            .function_impls
            .retain(|i, _| original.function_impls.contains_key(i));
        restored.stmts.truncate(original.stmts.len());
        restored.locals = original.locals;
        assert_eq!(
            format!("{restored:?}"),
            format!("{original:?}"),
            "all original metadata and code preserved"
        );
        let before = format!("{ir:?}");
        assert!(number_string::append(&mut ir, &bridge).is_err());
        assert_eq!(format!("{ir:?}"), before);
        for index in [entries.units, entries.managed] {
            let f = ir.functions.iter().find(|f| f.index == index).unwrap();
            assert!(!f.exported);
            assert!(ir.function_impls[&index].capture_copies.is_empty());
        }
        eprintln!(
            "NUMBER_ABI original_locals={} final_locals={} units={} managed={} same_body=true full_original_ir_preserved=true",
            original.locals, ir.locals, entries.units, entries.managed
        );
        let carrier_make = helper(&ir, "__compat_ref_make");
        let carriers: Vec<_> = ir
            .stmts
            .iter()
            .filter_map(|s| match s {
                S::Let {
                    index,
                    expr: E::Call(i, _, _, _, _),
                    ..
                } if *i == carrier_make => Some(*index),
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
        ir.stmts.push(bind(
            frame,
            T::Int,
            call(enter, vec![E::Var(carrier), E::Int(32)]),
        ));
        // An actual effectful guest argument. The wrapper cannot duplicate it.
        let count = fresh(&mut ir);
        ir.stmts.push(bind(count, T::Int, E::Int(0)));
        let result = fresh(&mut ir);
        ir.stmts.push(bind(
            result,
            super::representation::schema(),
            call(
                entries.managed,
                vec![
                    E::Var(carrier),
                    E::Block {
                        stmts: vec![S::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: count,
                            expr: E::Add(Box::new(E::Var(count)), Box::new(E::Int(1))),
                            line: 1,
                        }],
                        expr: Box::new(E::Float(1e21)),
                    },
                    E::Var(frame),
                    E::Int(0),
                ],
            ),
        ));
        for expr in [
            E::Var(carrier),
            E::Var(string),
            E::Var(object),
            E::Var(frame),
            E::Var(count),
            E::Var(result),
        ] {
            ir.stmts.push(S::Expr { expr, line: 1 });
        }
        for name in MANAGED_HELPERS {
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
    first: Value,
    functions: Vec<Value>,
}
impl Machine {
    fn new(source: &str) -> Self {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let issued = vm::compile_source_with_flavor_and_options_with_capture_certification(
            source,
            vm::SourceFlavor::Plugin(vm::FrontendId::new("number-string-managed")),
            vm::CompileSourceFileOptions::new()
                .with_source_plugin(&MANAGED)
                .with_host_api_catalog(catalog.clone()),
        )
        .expect("real current ES5 parser + strict/ownership certified compilation");
        let mut machine = issued.into_vm().unwrap();
        machine.set_jit_config(vm::JitConfig {
            enabled: false,
            ..Default::default()
        });
        crate::runtime::array::initialize_typescript_owned_arrays(
            &mut machine,
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
        // Existing exact builtin grants: no runtime/CLI registry modification.
        for builtin in ["math::floor", "bytes::from_utf8"] {
            registry.allow_builtin(builtin).unwrap();
        }
        registry.bind_vm_cached(&mut machine).unwrap();
        machine.set_fuel(10_000_000);
        assert_eq!(machine.run().unwrap(), VmStatus::Halted);
        let start = machine.stack().len() - MANAGED_HELPERS.len() - 6;
        let original_result = &machine.stack()[start - 1];
        assert_eq!(field(original_result, "tag"), Value::Int(2));
        assert_eq!(field(original_result, "flag"), Value::Bool(true));
        let tail = &machine.stack()[start..];
        assert_eq!(
            tail[4],
            Value::Int(1),
            "effectful numeric argument evaluated once"
        );
        Self {
            carrier: tail[0].clone(),
            string: tail[1].clone(),
            object: tail[2].clone(),
            frame: tail[3].clone(),
            first: tail[5].clone(),
            functions: tail[6..].to_vec(),
            vm: machine,
        }
    }
    fn invoke(&mut self, index: usize, args: &[Value]) -> Result<Value, vm::VmError> {
        self.vm.set_fuel(10_000_000);
        assert_eq!(
            self.vm
                .start_callable(self.functions[index].clone(), args)?,
            VmStatus::Halted
        );
        assert!(!self.vm.jit_config().enabled);
        assert_eq!(self.vm.aot_exec_count(), 0);
        assert_eq!(self.vm.jit_native_exec_count(), 0);
        assert!(
            self.vm
                .capture_certification_stats()
                .is_some_and(|s| s.valid)
        );
        Ok(self.vm.take_callable_result().unwrap())
    }
    fn publish(&mut self, number: Value, entry: i64) -> Value {
        self.invoke(
            1,
            &[
                self.carrier.clone(),
                number,
                self.frame.clone(),
                Value::Int(entry),
            ],
        )
        .unwrap()
    }
    fn publish_units(&mut self, units: &[i64], entry: i64) -> Value {
        self.invoke(
            8,
            &[
                self.carrier.clone(),
                raw(units),
                self.frame.clone(),
                Value::Int(entry),
            ],
        )
        .unwrap()
    }
    fn snapshot(&mut self) -> Vec<i64> {
        let carrier = self.carrier.clone();
        let v = self.invoke(4, std::slice::from_ref(&carrier)).unwrap();
        let Value::Array(values) = v else {
            panic!("snapshot")
        };
        values
            .iter()
            .map(|v| match v {
                Value::Int(i) => *i,
                Value::Float(f) if f.fract() == 0.0 => *f as i64,
                _ => panic!("numeric B1 cell"),
            })
            .collect()
    }
    fn base(&mut self, string: Value) -> i64 {
        let Value::Int(i) = self
            .invoke(
                2,
                &[
                    self.carrier.clone(),
                    field(&string, "ref_slot"),
                    field(&string, "generation"),
                ],
            )
            .unwrap()
        else {
            panic!("base")
        };
        i
    }
    fn observe(&mut self, string: Value) -> Vec<i64> {
        assert_eq!(field(&string, "tag"), Value::Int(4));
        let base = self.base(string.clone()) as usize;
        let raw = self.snapshot();
        assert_eq!(raw[base], 2);
        assert_eq!(raw[base + 1], int_field(&string, "generation"));
        let len = usize::try_from(raw[base + 2]).unwrap();
        assert!(base + 3 + len <= raw.len());
        raw[base + 3..base + 3 + len].to_vec()
    }
    fn metric(&mut self, which: i64) -> i64 {
        let Value::Int(i) = self
            .invoke(6, &[self.carrier.clone(), Value::Int(which), Value::Int(0)])
            .unwrap()
        else {
            panic!("metric")
        };
        i
    }
    fn force_gc(&mut self) {
        self.invoke(
            3,
            &[self.carrier.clone(), Value::Float(10.0), Value::Float(32.0)],
        )
        .unwrap();
    }
    fn reset(&mut self) {
        self.vm.reset_for_reuse().unwrap();
        assert_eq!(self.vm.host_context().resource_count(), 0);
        assert!(crate::runtime::array::owned_array_runtime_stats(&mut self.vm).is_none());
        assert!(
            self.vm
                .capture_certification_stats()
                .is_none_or(|s| !s.valid)
        );
        eprintln!(
            "NUMBER_RESET resources=0 scope_directory_absent=true certification_revoked=true"
        );
    }
}
#[test]
fn managed_publication_gc_keys_and_hygiene() {
    worker(|| {
        let mut m = Machine::new(
            r#"/*contract-rejections*/var s="keep\0\uD800";var o={1000000000000000.25:7,0.000001:8,1e21:9};var alias=o;alias["1000000000000000.3"]===7 && alias["0.000001"]===8 && alias["1e+21"]===9 && alias["1000000000000000.2"]===undefined && alias["1e21"]===undefined;"#,
        );
        assert_eq!(text(&m.observe(m.first.clone())), "1e+21");
        let original = m.observe(m.string.clone());
        assert_eq!(original, vec![107, 101, 101, 112, 0, 0xD800]);
        let base = m.base(m.object.clone()) as usize;
        let before = m.snapshot();
        let object_prefix = before[base..base + 26].to_vec();
        for input in [
            Value::Float(f64::NAN),
            Value::Float(0.0),
            Value::Float(-0.0),
            Value::Float(f64::INFINITY),
            Value::Float(f64::NEG_INFINITY),
            Value::Float(-1.25),
            Value::Float(1e20),
            Value::Float(1e21),
            Value::Float(1e-6),
            Value::Float(1e-7),
            Value::Float(1.2345678901234567e-6),
            Value::Float(f64::from_bits(1)),
            Value::Float(f64::MAX),
            Value::Float(f64::from_bits(0x430c6bf526340002)),
            Value::Int(9007199254740993),
            Value::Int(i64::MAX),
            Value::Int(i64::MIN),
        ] {
            let expected_value = m.invoke(0, std::slice::from_ref(&input)).unwrap();
            let Value::Array(a) = expected_value else {
                panic!("units")
            };
            let expected: Vec<i64> = a
                .iter()
                .map(|v| match v {
                    Value::Int(i) => *i,
                    _ => panic!("Int"),
                })
                .collect();
            let collections = m.metric(5);
            let roots = m.metric(4);
            m.force_gc();
            let result = m.publish(input.clone(), 1);
            assert_eq!(m.metric(5), collections + 1);
            assert_eq!(m.metric(4), roots);
            assert_eq!(m.metric(8), 0);
            assert_eq!(m.observe(result.clone()), expected);
            let result_alias = result.clone();
            assert_eq!(m.observe(result_alias), expected);
            assert_eq!(
                m.observe(m.first.clone()),
                b"1e+21".iter().copied().map(i64::from).collect::<Vec<_>>()
            );
            assert_eq!(m.observe(m.string.clone()), original);
            let raw = m.snapshot();
            let result_base = m.base(result) as usize;
            let occupied = raw[result_base - 3];
            let requested = expected.len() as i64 + 6;
            assert!(
                (requested..=requested + 3).contains(&occupied),
                "actual raw extent includes only an unsplittable tail"
            );
            let stats = crate::runtime::array::owned_array_runtime_stats(&mut m.vm).unwrap();
            eprintln!(
                "NUMBER_MANAGED input={input:?} text={:?} units={} occupied_cells={occupied} b1_end={} b1_reserved_bytes={} rooted=true forced_gc=true",
                text(&expected),
                expected.len(),
                raw[1],
                stats.reserved_bytes
            );
        }
        let after = m.snapshot();
        assert_eq!(&after[base..base + 26], object_prefix);
        // Follow the three actual object-row key handles, not an inventory
        // containing unrelated query/result String nodes with matching text.
        assert_eq!(after[base], super::heap::KIND_OBJECT);
        assert_eq!(after[base + 2] / super::heap::OBJECT_COUNT_FACTOR, 3);
        let mut keys = Vec::new();
        for (index, number) in [f64::from_bits(0x430c6bf526340002), 1e-6, 1e21]
            .into_iter()
            .enumerate()
        {
            let row = base
                + super::heap::OBJECT_HEADER_CELLS as usize
                + index * super::heap::OBJECT_CELLS_PER_PROPERTY as usize;
            let slot = after[row];
            let generation = after[row + 1];
            let Value::Int(key_base) = m
                .invoke(
                    2,
                    &[m.carrier.clone(), Value::Int(slot), Value::Int(generation)],
                )
                .unwrap()
            else {
                panic!("actual key checked_base")
            };
            let key_base = key_base as usize;
            let backing = m.snapshot();
            assert_eq!(backing[key_base], super::heap::KIND_STRING);
            assert_eq!(backing[key_base + 1], generation);
            let length = backing[key_base + 2] as usize;
            let key_units = backing[key_base + 3..key_base + 3 + length].to_vec();
            let s = m.publish(Value::Float(number), 2);
            assert_eq!(
                m.observe(s),
                key_units,
                "actual property-row key agrees with guest producer"
            );
            keys.push(text(&key_units));
            eprintln!(
                "NUMBER_KEY_NODE row={row} slot={slot} generation={generation} checked_base={key_base} text={:?}",
                text(&key_units)
            );
        }
        assert!(
            !keys
                .iter()
                .any(|key| key == "1000000000000000.2" || key == "1e21")
        );
        eprintln!(
            "NUMBER_KEYS actual_keys={keys:?} guest_identity=true alias_lookup=true original_script_result=true"
        );
        m.invoke(7, &[m.carrier.clone(), m.frame.clone()]).unwrap();
        m.reset();
    });
}
#[test]
fn managed_invalid_destination_and_capacity_refusal_reset() {
    worker(|| {
        let mut m = Machine::new(r#"var s="keep";var o={n:7};s===s;"#);
        let before = m.snapshot();
        let err = m
            .invoke(
                1,
                &[
                    m.carrier.clone(),
                    Value::Float(1e21),
                    Value::Int(0),
                    Value::Int(0),
                ],
            )
            .unwrap_err();
        assert!(format!("{err:?}").contains("assertion failed"));
        let after = m.snapshot();
        assert_eq!(after[8], 1);
        assert_eq!(after[6], before[6]);
        eprintln!("NUMBER_INVALID_DEST pending_inactive=1 live_unchanged=true error={err:?}");
        m.reset();
        let mut m = Machine::new(r#"var s="keep";var o={n:7};s===s;"#);
        m.invoke(5, &[m.carrier.clone()]).unwrap();
        let ceiling = 65536usize.min(
            crate::runtime::array::OwnedArrayLimits::default().reserved_bytes_quota()
                / std::mem::size_of::<Value>(),
        );
        let mut entry = 2;
        loop {
            let raw = m.snapshot();
            let end = raw[1] as usize;
            let mut p = 80;
            let mut largest = 0;
            while p < end {
                let size = raw[p] as usize;
                assert!(size >= 4);
                if raw[p + 1] == 0 {
                    largest = largest.max(size);
                }
                p += size;
            }
            assert_eq!(p, end);
            if largest < 7 {
                break;
            }
            assert!(entry < 30);
            let n = largest - 6;
            let result = m.publish_units(&vec![65; n], entry);
            assert_eq!(m.observe(result).len(), n);
            entry += 1;
        }
        let raw = m.snapshot();
        let end = raw[1] as usize;
        assert!(ceiling > end + 6);
        let result = m.publish_units(&vec![65; ceiling - end - 6], entry);
        assert_eq!(m.observe(result).len(), ceiling - end - 6);
        let full = m.snapshot();
        assert_eq!(full[1] as usize, ceiling);
        let stats = crate::runtime::array::owned_array_runtime_stats(&mut m.vm).unwrap();
        let err = m
            .invoke(
                1,
                &[
                    m.carrier.clone(),
                    Value::Float(f64::NEG_INFINITY),
                    m.frame.clone(),
                    Value::Int(entry + 1),
                ],
            )
            .unwrap_err();
        assert!(
            format!("{err:?}").contains("assertion failed") || format!("{err:?}").contains("quota")
        );
        let refused = m.snapshot();
        assert_eq!(refused[8], 0);
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
            "NUMBER_CAPACITY cells={ceiling} reserved_bytes={} pending=0 live_unchanged=true refusal={err:?}",
            stats.reserved_bytes
        );
        m.reset();
    });
}

struct Kernel;
static KERNEL: Kernel = Kernel;
impl SourcePlugin for Kernel {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("number-string-kernel"),
            extensions: &["number-test"],
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
        let entry = number_string::append_units(&mut ir)?;
        ir.stmts.push(S::Expr {
            expr: E::FunctionRef(entry, vec![]),
            line: 1,
        });
        Ok(ir)
    }
}
fn kernel() -> (vm::Vm, Value) {
    let issued = vm::compile_source_with_flavor_and_options_with_capture_certification(
        "const sentinel: number = 73; function retained(x: number): number { return x + 1; }",
        vm::SourceFlavor::Plugin(vm::FrontendId::new("number-string-kernel")),
        vm::CompileSourceFileOptions::new().with_source_plugin(&KERNEL),
    )
    .expect("actual units producer strict/ownership compile");
    let mut machine = issued.into_vm().unwrap();
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..Default::default()
    });
    machine.set_fuel(10_000_000);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    let callable = machine.stack().last().unwrap().clone();
    (machine, callable)
}
fn units(machine: &mut vm::Vm, callable: &Value, input: Value) -> String {
    machine.set_fuel(10_000_000);
    assert_eq!(
        machine.start_callable(callable.clone(), &[input]).unwrap(),
        VmStatus::Halted
    );
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    let Value::Array(values) = machine.take_callable_result().unwrap() else {
        panic!("Array(Int)")
    };
    let bytes: Vec<u8> = values
        .iter()
        .map(|v| match v {
            Value::Int(i) => u8::try_from(*i).unwrap(),
            _ => panic!("physical Int unit {v:?}"),
        })
        .collect();
    assert!(bytes.len() <= 25);
    eprintln!(
        "NUMBER_CORE_OUTPUT len={} capacity={} bytes={}",
        values.len(),
        values.capacity(),
        values.capacity() * std::mem::size_of::<Value>()
    );
    String::from_utf8(bytes).unwrap()
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
fn exact_bits_units_kernel() {
    worker(|| {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/number_string/vectors.json"
        ))
        .unwrap();
        assert_eq!(fixture["version"], "v26.7.0");
        let rows = fixture["vectors"].as_array().unwrap();
        let mut seen = std::collections::HashSet::new();
        let (mut machine, callable) = kernel();
        let mut differences = 0;
        for row in rows {
            let bits = u64::from_str_radix(row["args"]["bits"].as_str().unwrap(), 16).unwrap();
            assert!(seen.insert(bits));
            let actual = units(&mut machine, &callable, Value::Float(f64::from_bits(bits)));
            let reference = row["reference"]["raw_string"].as_str().unwrap();
            let expected = row
                .get("normative_alternative")
                .and_then(|v| v.as_str())
                .unwrap_or(reference);
            if actual != reference {
                differences += 1;
            }
            eprintln!(
                "NUMBER_VECTOR {}",
                serde_json::json!({"id":row["id"], "args":row["args"], "actual":actual, "reference":row["reference"]})
            );
            // Exact alternatives are populated only after the external exact-rational proof.
            assert_eq!(actual, expected, "exact mapped reference for {}", row["id"]);
            assert_eq!(machine.host_context().resource_count(), 0);
        }
        assert!(seen.len() >= 1000);
        let int_rows = fixture["int_vectors"].as_array().unwrap();
        assert_eq!(int_rows.len(), 7);
        let mut int_seen = std::collections::HashSet::new();
        for row in int_rows {
            let integer = row["args"]["integer"]
                .as_str()
                .unwrap()
                .parse::<i64>()
                .unwrap();
            assert!(int_seen.insert(integer));
            let actual = units(&mut machine, &callable, Value::Int(integer));
            let projected = units(&mut machine, &callable, Value::Float(integer as f64));
            assert_eq!(actual, projected, "guest Float projection of physical Int");
            assert_eq!(actual, row["reference"]["raw_string"].as_str().unwrap());
            eprintln!(
                "NUMBER_INT {}",
                serde_json::json!({"id":row["id"],"args":row["args"],"reference":row["reference"],"integer":integer.to_string(),"projected_bits":format!("{:016x}",(integer as f64).to_bits()),"actual":actual})
            );
        }
        eprintln!(
            "NUMBER_SWEEP unique_bits={} node_differences={differences} halted=true fuel_per_invocation=10000000 jit=false native=0 units_kernel_only=true",
            seen.len()
        );
    });
}
struct RawProbe;
static RAW_PROBE: RawProbe = RawProbe;
impl SourcePlugin for RawProbe {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("number-string-raw-capacity"),
            extensions: &["number-test"],
            typing_policy: vm::FrontendTypingPolicy::Strict,
            supports_ownership_checking: true,
        }
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir =
            crate::TYPESCRIPT_PLUGIN.parse_source("const sentinel: number = 73;", context)?;
        let entry = number_string::append_units(&mut ir)?;
        let bits = u64::from_str_radix(source.trim(), 16).unwrap();
        let input = f64::from_bits(bits);
        assert!(input.is_finite() && input != 0.0);
        // Separate raw-allocation probe: identical qualified Core primitive and
        // Bytes conversion, concurrently retained with the real units output.
        // This does not replace or modify the semantic producer implementation.
        let raw = fresh(&mut ir);
        let bytes = fresh(&mut ir);
        let output = fresh(&mut ir);
        ir.stmts.push(bind(
            raw,
            T::String,
            call(
                vm::BuiltinFunction::ToString.call_index(),
                vec![E::Div(
                    Box::new(E::Float(input.abs())),
                    Box::new(E::Float(1.0)),
                )],
            ),
        ));
        ir.stmts.push(bind(
            bytes,
            T::Bytes,
            call(
                vm::BuiltinFunction::BytesFromUtf8.call_index(),
                vec![E::Borrow(Box::new(E::Var(raw)))],
            ),
        ));
        ir.stmts.push(bind(
            output,
            T::Array(Box::new(T::Int)),
            call(entry, vec![E::Float(input)]),
        ));
        for slot in [raw, bytes, output] {
            ir.stmts.push(S::Expr {
                expr: E::Var(slot),
                line: 1,
            });
        }
        Ok(ir)
    }
}
#[test]
fn raw_core_concurrent_capacity_evidence() {
    worker(|| {
        for input in [
            f64::from_bits(1),
            f64::MIN_POSITIVE,
            f64::MAX,
            -1.2345678901234567e-6,
            f64::from_bits(0x430c6bf526340002),
            1e21,
        ] {
            let source = format!("{:016x}", input.to_bits());
            let issued = vm::compile_source_with_flavor_and_options_with_capture_certification(
                &source,
                vm::SourceFlavor::Plugin(vm::FrontendId::new("number-string-raw-capacity")),
                vm::CompileSourceFileOptions::new().with_source_plugin(&RAW_PROBE),
            )
            .unwrap();
            let mut machine = issued.into_vm().unwrap();
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..Default::default()
            });
            machine.set_fuel(10_000_000);
            assert_eq!(machine.run().unwrap(), VmStatus::Halted);
            assert_eq!(machine.host_context().resource_count(), 0);
            let tail = &machine.stack()[machine.stack().len() - 3..];
            let (Value::String(raw), Value::Bytes(bytes), Value::Array(output)) =
                (&tail[0], &tail[1], &tail[2])
            else {
                panic!("three actual concurrent Core allocations")
            };
            assert_eq!(raw.as_bytes(), bytes.as_slice());
            assert!(raw.len() <= 343 && output.len() <= 25);
            assert_eq!(machine.aot_exec_count(), 0);
            assert_eq!(machine.jit_native_exec_count(), 0);
            eprintln!(
                "NUMBER_RAW {}",
                serde_json::json!({"bits":source,"raw_length":raw.len(),"string_capacity":raw.capacity(),"bytes_length":bytes.len(),"bytes_capacity":bytes.capacity(),"output_length":output.len(),"output_capacity":output.capacity(),"value_bytes":std::mem::size_of::<Value>(),"concurrent_payload_capacity_bytes":raw.capacity()+bytes.capacity()+output.capacity()*std::mem::size_of::<Value>(),"b1_resources":0,"distinct_allocations":true})
            );
            machine.reset_for_reuse().unwrap();
            assert_eq!(machine.host_context().resource_count(), 0);
        }
    });
}

#[test]
fn notation_vertical() {
    worker(|| {
        let (mut machine, callable) = kernel();
        let actual = units(&mut machine, &callable, Value::Float(1e21));
        eprintln!(
            "NUMBER_VERTICAL bits={:016x} actual={actual:?} expected=\"1e+21\"",
            1e21f64.to_bits()
        );
        assert_eq!(actual, "1e+21");
        for (input, expected) in [
            (0.0, "0"),
            (-0.0, "0"),
            (f64::NAN, "NaN"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
            (-1.25, "-1.25"),
            (1e-6, "0.000001"),
            (1e-7, "1e-7"),
            (1e20, "100000000000000000000"),
            (1.2345678901234567e-6, "0.0000012345678901234567"),
            (f64::from_bits(1), "5e-324"),
            (f64::MAX, "1.7976931348623157e+308"),
        ] {
            assert_eq!(
                units(&mut machine, &callable, Value::Float(input)),
                expected,
                "bits={:016x}",
                input.to_bits()
            );
        }
    });
}

// Private completed-IR composition: actual UTC Number -> managed NumberString
// -> encodeURIComponent -> decodeURIComponent, on one certified B1 carrier.
// Digits are ASCII here; the retained URI oracle supplies Unicode/escape cases.
#[cfg(test)]
mod date_uri_shared_scope {
    use super::*;
    use crate::javascript::{
        JAVASCRIPT_ES5_PLUGIN, heap, javascript_es5_flavor, prelude, representation,
    };
    use std::sync::Arc;
    use vm::{FrontendIr, HostFunction, HostFunctionRegistry};
    // Exact original accepted ES5 caller, not a TS body or synthetic Return.
    const DATE_ES5_CALLER_SOURCE: &str =
        "function f(x){'use strict';return x;}var n=0;var answer=f(n=n+6);answer===6&&n===6;";
    struct Shared;
    static SHARED: Shared = Shared;
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
            let cert = machine.capture_certification_stats().unwrap();
            assert!(cert.valid && cert.capture_facts > 0);
            let stats = crate::runtime::array::owned_array_runtime_stats(machine).unwrap();
            assert_eq!((stats.pending, stats.registered), (0, 1));
            assert_eq!(machine.host_context().resource_count(), 1);
            assert_eq!(machine.aot_exec_count(), 0);
            assert_eq!(machine.jit_native_exec_count(), 0);
            eprintln!("NUMBER_URI_DATE_CALLBACK args={args:?} b1={stats:?} authority={cert:?}");
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
            fn declared_slots(stmts: &[S], out: &mut Vec<u16>) {
                for stmt in stmts {
                    match stmt {
                        S::Let { index, .. } => out.push(*index),
                        S::IfElse {
                            then_branch,
                            else_branch,
                            ..
                        } => {
                            declared_slots(then_branch, out);
                            declared_slots(else_branch, out);
                        }
                        S::While { body, .. } => declared_slots(body, out),
                        _ => {}
                    }
                }
            }
            let mut declared = body.param_slots.clone();
            // NumberString's finite path declares locals inside nested
            // branches. Count those actual declarations, not just top-level
            // statements, and retain exact uniqueness/range/coverage checks.
            declared_slots(&body.body_stmts, &mut declared);
            for slot in declared {
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
            // Actual new import declaration, with exact catalog annotations.
            // Metadata-only holes are not exercised by this integration.
            let imported = ir.functions.iter_mut().find(|d| d.index == input).unwrap();
            imported.arg_schemas = vec![Some(T::Int)];
            imported.return_schema = Some(T::Number);
            imported.return_type = vm::ValueType::Float;
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
            let before = ir.clone();
            let number = number_string::append(&mut ir, &bridge)?;
            preserved(&before, &ir, "number_string");
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
            signature(
                &ir,
                number.units,
                vec![T::Number],
                array.clone(),
                vm::ValueType::Array,
            );
            signature(
                &ir,
                number.managed,
                vec![carrier_schema, T::Number, T::Int, T::Int],
                representation::schema(),
                vm::ValueType::Map,
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
            // Modes from the accepted uri.rs contract: 1 encodeURIComponent,
            // 3 decodeURIComponent. Entry IDs come from actual append returns.
            const ENCODE_COMPONENT: i64 = 1;
            const DECODE_COMPONENT: i64 = 3;
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
                call(enter, vec![E::Var(carrier), E::Int(8)]),
            );
            // Four distinct output entries plus four explicit alias entries.
            let number_out = bind(
                &mut ir,
                representation::schema(),
                call(
                    number.managed,
                    vec![E::Var(carrier), E::Var(utc), E::Var(frame), E::Int(0)],
                ),
            );
            let number_units = bind(
                &mut ir,
                array.clone(),
                call(
                    bridge.managed_to_units,
                    vec![E::Var(carrier), E::Var(number_out)],
                ),
            );
            let encoded_result = bind(
                &mut ir,
                prelude::uri::result_schema(),
                call(
                    uri,
                    vec![
                        E::Var(number_units),
                        E::Int(ENCODE_COMPONENT),
                        E::Int(65536),
                    ],
                ),
            );
            let encoded = bind(
                &mut ir,
                representation::schema(),
                call(
                    bridge.units_to_managed,
                    vec![
                        E::Var(carrier),
                        prelude::object::field(E::Var(encoded_result), "output"),
                        E::Var(frame),
                        E::Int(1),
                    ],
                ),
            );
            let encoded_units = bind(
                &mut ir,
                array.clone(),
                call(
                    bridge.managed_to_units,
                    vec![E::Var(carrier), E::Var(encoded)],
                ),
            );
            let decoded_result = bind(
                &mut ir,
                prelude::uri::result_schema(),
                call(
                    uri,
                    vec![
                        E::Var(encoded_units),
                        E::Int(DECODE_COMPONENT),
                        E::Int(65536),
                    ],
                ),
            );
            let decoded = bind(
                &mut ir,
                representation::schema(),
                call(
                    bridge.units_to_managed,
                    vec![
                        E::Var(carrier),
                        prelude::object::field(E::Var(decoded_result), "output"),
                        E::Var(frame),
                        E::Int(2),
                    ],
                ),
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
                        E::Int(3),
                    ],
                ),
            );
            let root_write = helper(&ir, heap::ROOT_WRITE_HELPER);
            let originals = [number_out, encoded, decoded, control];
            let mut aliases = Vec::new();
            for (entry, original) in originals.into_iter().enumerate() {
                let alias = bind(&mut ir, representation::schema(), E::Var(original));
                ir.stmts.push(S::Expr {
                    expr: call(
                        root_write,
                        vec![
                            E::Var(carrier),
                            E::Var(frame),
                            E::Int(4 + entry as i64),
                            E::Var(alias),
                        ],
                    ),
                    line: 1,
                });
                aliases.push(alias);
            }
            // The helper acknowledgment expressions above are explicit, and
            // therefore accounted for separately from the retained Script.
            for expr in [E::Var(utc), E::Var(carrier), E::Var(frame)]
                .into_iter()
                .chain(originals.into_iter().map(E::Var))
                .chain(aliases.into_iter().map(E::Var))
                .chain([
                    prelude::object::field(E::Var(encoded_result), "status"),
                    prelude::object::field(E::Var(decoded_result), "status"),
                    E::Var(number_units),
                    E::Var(encoded_units),
                ])
            {
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
            let mut restored = ir.clone();
            restored.locals = parsed.locals;
            restored.stmts = parsed.stmts.clone();
            restored.functions = parsed.functions.clone();
            restored.function_impls = parsed.function_impls.clone();
            restored.struct_schemas = parsed.struct_schemas.clone();
            restored.host_api_metadata = parsed.host_api_metadata.clone();
            assert_eq!(
                format!("{restored:?}"),
                format!("{parsed:?}"),
                "all original code, binding/source/capture/schema sidecars preserved"
            );
            eprintln!(
                "NUMBER_URI_DATE_PRODUCER source={source:?} input={input} utc={} number_units={} number_managed={} uri={uri} bridge_to={} bridge_from={} carrier_local={carrier} frame_local={frame} encode_mode={ENCODE_COMPONENT} decode_mode={DECODE_COMPONENT} metadata_only_holes_exercised=false",
                date.utc,
                number.units,
                number.managed,
                bridge.managed_to_units,
                bridge.units_to_managed
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
        let requested = heap::RAW_HEADER_CELLS + heap::STRING_HEADER_CELLS + len;
        assert!((requested..requested + heap::MIN_RAW_BLOCK_CELLS).contains(&extent));
        assert!(base - heap::RAW_HEADER_CELLS + extent <= at(heap::HDR_HEAP_END));
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
    fn runtime_date_number_string_uri_roundtrip_gc_reset() {
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
            for (inputs, expected, expected_text) in [
                ([1970.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0], 0.0, "0"),
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
                    "20995200000",
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
                for name in [
                    "math::floor",
                    "math::trunc",
                    "math::abs",
                    "math::is_finite",
                    "bytes::from_utf8",
                ] {
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
                // Script, answer, n, four root-write acknowledgments, then
                // utc/carrier/frame, four outputs, four aliases, two statuses,
                // two actual extracted arrays, and five checked observers.
                assert_eq!(result.len(), 27);
                assert_eq!(
                    field(&result[0], "tag"),
                    Value::Int(representation::BOOLEAN)
                );
                assert_eq!(field(&result[0], "flag"), Value::Bool(true));
                for value in &result[1..3] {
                    assert_eq!(field(value, "tag"), Value::Int(representation::NUMBER));
                    assert!(matches!(field(value, "number"), Value::Float(6.0)));
                }
                assert_eq!(&result[3..7], vec![Value::Int(1); 4].as_slice());
                assert!(matches!(result[7], Value::Float(f) if f == expected));
                let carrier = &result[8];
                let dispatcher = field(carrier, "dispatcher");
                let Value::Callable(owner_callable) = &dispatcher else {
                    panic!("certified owner dispatcher")
                };
                assert!(owner_callable.env.is_some());
                let owner_identity = format!("{owner_callable:?}");
                let frame = integer(result[9].clone());
                for i in 0..4 {
                    assert_eq!(result[10 + i], result[14 + i]);
                }
                assert_eq!(result[18], Value::Int(0));
                assert_eq!(result[19], Value::Int(0));
                let expected_units: Vec<_> = expected_text.bytes().map(i64::from).collect();
                assert_eq!(cells(&result[20]), expected_units);
                assert_eq!(cells(&result[21]), expected_units);
                let functions = &result[22..];
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
                    for (i, string) in result[10..18].iter().enumerate() {
                        let payload = if i % 4 == 3 {
                            &[75, 0, 0xd800][..]
                        } else {
                            expected_units.as_slice()
                        };
                        observe(&mut machine, functions, carrier, string, payload);
                    }
                    let raw = cells(&invoke(
                        &mut machine,
                        &functions[1],
                        std::slice::from_ref(carrier),
                    ));
                    assert_eq!(raw[heap::HDR_PENDING_NODES as usize], 0);
                    assert_eq!(raw[heap::HDR_ROOT_TOP as usize], frame);
                    assert_eq!(raw[frame as usize + 3], 8);
                    for (entry, string) in result[10..18].iter().enumerate() {
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
                    let current_dispatcher = field(carrier, "dispatcher");
                    let Value::Callable(current_owner) = &current_dispatcher else {
                        panic!("owner identity")
                    };
                    assert!(Arc::ptr_eq(owner_callable, current_owner));
                    assert!(Arc::ptr_eq(
                        owner_callable.env.as_ref().unwrap(),
                        current_owner.env.as_ref().unwrap()
                    ));
                    assert_eq!(format!("{current_owner:?}"), owner_identity);
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
                    roots - 8
                );
                eprintln!(
                    "NUMBER_URI_DATE_EXEC utc={expected:?} actual_units={expected_units:?} encode_status=0 decode_status=0 callbacks={:?} script=true answer=Float(6) effect=Float(6) halted=true jit=false aot=0 native=0 fuel_used={fuel} caller_entries=8 owner={owner_identity}",
                    state.lock().unwrap().observed
                );
                eprintln!(
                    "NUMBER_URI_DATE_HOST_DEPENDENCIES CoreFloatToString=RustShortestDigits B1=owned_array_host_functions math_permissions=floor,trunc,abs,is_finite bytes=from_utf8 guest_native_counters_do_not_mean_host_free=true ascii_only=true"
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
