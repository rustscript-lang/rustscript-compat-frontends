//! Ordinary-object semantics through the actual certified interpreter.
use pd_vm_compat_frontends::{JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor};
use vm::{BuiltinFunction, HostFunctionRegistry, Value, VmStatus};
#[path = "javascript_es5/object_observer.rs"]
mod object_observer;

fn field(value: &Value, name: &str) -> Value {
    let Value::Map(map) = value else {
        panic!("boxed value: {value:?}")
    };
    map.get(&Value::string(name)).expect("boxed field").clone()
}

struct Runner {
    native: bool,
}
impl Runner {
    fn execute(&self, source: &str) -> Value {
        self.run(source, None).last().expect("script value").clone()
    }
    fn run(&self, source: &str, observation: Option<object_observer::Observer>) -> Vec<Value> {
        run_mode(source, observation, self.native)
    }
}

enum MachineSource {
    Stock,
    Observed(object_observer::Observer),
    Tapped(&'static dyn vm::SourcePlugin),
}

fn build_machine(source: &str, plugin: MachineSource, native: bool) -> (vm::Vm, bool) {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let mut options = vm::CompileSourceFileOptions::new().with_host_api_catalog(catalog.clone());
    let uncaught = matches!(&plugin, MachineSource::Observed(o) if o.uncaught);
    let flavor = match plugin {
        MachineSource::Stock => {
            options = options.with_source_plugin(&JAVASCRIPT_ES5_PLUGIN);
            javascript_es5_flavor()
        }
        MachineSource::Observed(observation) => {
            options = options.with_source_plugin(Box::leak(Box::new(observation)));
            vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-object-observer"))
        }
        MachineSource::Tapped(plugin) => {
            options = options.with_source_plugin(plugin);
            javascript_es5_flavor()
        }
    };
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source, flavor, options,
    )
    .unwrap_or_else(|error| panic!("ordinary source must compile: {source}\n{error:?}"));
    let mut machine = compiled.into_vm().expect("certified VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("owned arrays");
    let mut registry = HostFunctionRegistry::restricted();
    pd_vm_compat_frontends::runtime::array::typescript_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("arrays");
    pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("owners");
    pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("snapshots");
    pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("math");
    pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .expect("error adapter");
    registry.allow_builtin("math::floor").expect("floor");
    // The ordinary realm's bootstrap publishes its fixed ASCII strings through
    // the Core bytes initializer, so this capability is part of the emitted
    // program (the Object.keys harness already allows the same builtin).
    registry
        .allow_builtin("bytes::from_utf8")
        .expect("bootstrap strings");
    // Explicit host-capability audit for the immutable certified program.
    // Installed host modules grant host imports only; the pure Core builtins
    // emitted by the realm bootstrap remain ungranted in a restricted
    // registry. The public namespaced builtin catalog is exactly the
    // generated explicit-host-capability surface, so every OpCode::Call
    // immediate and HostImport callable prototype that resolves through it
    // is a required capability. Audit before bind and grant only the actual
    // canonical gaps.
    let namespaced = {
        let mut catalog = std::collections::BTreeMap::new();
        for namespace in vm::builtin_namespace_specs() {
            for member in namespace.members {
                let builtin =
                    vm::resolve_builtin_namespace_call(namespace.namespace, member.name)
                        .unwrap_or_else(|| {
                            panic!(
                                "namespace member {}.{} is missing from the builtin catalog",
                                namespace.namespace, member.name
                            )
                        });
                catalog.insert(
                    builtin.call_index(),
                    format!("{}::{}", namespace.namespace, member.name),
                );
            }
        }
        catalog
    };
    let required: Vec<u16> = {
        let program = machine.program();
        let mut required = std::collections::BTreeSet::new();
        let mut ip = 0usize;
        while let Some(&raw) = program.code.get(ip) {
            let opcode = vm::OpCode::try_from(raw)
                .unwrap_or_else(|_| panic!("invalid opcode {raw} at program ip {ip}"));
            let end = ip
                .checked_add(1 + opcode.operand_len())
                .unwrap_or_else(|| panic!("opcode operand overflows at program ip {ip}"));
            assert!(
                end <= program.code.len(),
                "opcode operand exceeds program at ip {ip}"
            );
            if matches!(opcode, vm::OpCode::Call) {
                let index = u16::from_le_bytes(
                    program.code[ip + 1..ip + 3]
                        .try_into()
                        .expect("Call immediate is two bytes"),
                );
                if namespaced.contains_key(&index) {
                    required.insert(index);
                }
            }
            ip = end;
        }
        for prototype in &program.callable_prototypes {
            if let vm::CallableTarget::HostImport(index) = prototype.target {
                if namespaced.contains_key(&index) {
                    required.insert(index);
                }
            }
        }
        required.into_iter().collect()
    };
    let base_grants = ["math::floor", "bytes::from_utf8"];
    let granted: Vec<u16> = base_grants
        .iter()
        .map(|&name| {
            vm::BuiltinFunction::from_namespaced_name(name)
                .unwrap_or_else(|| panic!("unknown namespaced builtin {name}"))
                .call_index()
        })
        .collect();
    let missing: Vec<u16> = required
        .iter()
        .copied()
        .filter(|index| !granted.contains(index))
        .collect();
    // Current certified producer's narrow explicit-capability needs beyond
    // the two established grants. Any other missing canonical intrinsic is
    // a real, unexpected gap rather than a broad permit.
    const VERIFIED_CURRENT_PRODUCER_CAPABILITIES: &[(&str, vm::BuiltinFunction)] = &[
        ("math::trunc", vm::BuiltinFunction::MathTrunc),
        ("math::is_finite", vm::BuiltinFunction::MathIsFinite),
        ("math::powf", vm::BuiltinFunction::MathPowF),
    ];
    let refs = |indices: &[u16]| -> Vec<serde_json::Value> {
        indices
            .iter()
            .map(|index| {
                serde_json::json!({
                    "index": index,
                    "hex": format!("0x{index:04X}"),
                    "canonical": namespaced.get(index),
                })
            })
            .collect()
    };
    eprintln!(
        "P7_OBJECT_CAPABILITIES {}",
        serde_json::json!({
            "required": refs(&required),
            "granted": refs(&granted),
            "missing": refs(&missing),
        })
    );
    for &index in &missing {
        let canonical = namespaced
            .get(&index)
            .unwrap_or_else(|| panic!("required builtin index {index} is not namespaced"));
        let expected = VERIFIED_CURRENT_PRODUCER_CAPABILITIES
            .iter()
            .find(|entry| entry.0 == canonical.as_str())
            .unwrap_or_else(|| {
                panic!(
                    "unexpected explicit host capability gap: {}",
                    serde_json::json!({
                        "required": refs(&required),
                        "granted": refs(&granted),
                        "missing": refs(&missing),
                    })
                )
            });
        assert_eq!(
            vm::BuiltinFunction::from_namespaced_name(canonical.as_str()),
            Some(expected.1),
            "missing capability {canonical} must resolve to its canonical intrinsic"
        );
        assert_eq!(
            expected.1.call_index(),
            index,
            "namespaced catalog and canonical intrinsic disagree on {canonical}"
        );
    }
    for &index in &missing {
        let canonical = namespaced
            .get(&index)
            .expect("audited namespaced capability");
        registry
            .allow_builtin(canonical.as_str())
            .unwrap_or_else(|error| {
                panic!("audited capability {canonical} must be grantable: {error:?}")
            });
    }
    registry.bind_vm_cached(&mut machine).expect("bind");
    machine.set_fuel(10_000_000);
    if native {
        machine
            .compile_aot()
            .expect("native compilation after separate approval");
    }
    (machine, uncaught)
}

fn run_mode(
    source: &str,
    observation: Option<object_observer::Observer>,
    native: bool,
) -> Vec<Value> {
    let plugin = match observation {
        None => MachineSource::Stock,
        Some(observation) => MachineSource::Observed(observation),
    };
    let (mut machine, uncaught) = build_machine(source, plugin, native);
    let status = machine.run();
    if source.contains("getOwnPropertyDescriptor") {
        eprintln!(
            "P7_D08_FUEL status={status:?} fuel={:?} ip={}",
            machine.get_fuel(),
            machine.ip()
        );
    }
    if !native {
        assert!(!machine.jit_config().enabled);
        assert_eq!(machine.aot_exec_count(), 0);
        assert_eq!(machine.jit_native_exec_count(), 0);
    }
    if matches!(status, Ok(VmStatus::Yielded)) {
        let frames: Vec<_> = machine
            .execution_frames()
            .iter()
            .map(|f| {
                let prototype = f
                    .prototype_id
                    .map(|id| &machine.program().callable_prototypes[id as usize]);
                let params: Vec<_> = prototype
                    .into_iter()
                    .flat_map(|p| &p.parameter_slots)
                    .map(
                        |slot| match &machine.locals()[f.local_base + usize::from(*slot)] {
                            Value::Int(n) => format!("Int({n})"),
                            Value::Float(n) => format!("Float({n})"),
                            Value::Bool(n) => format!("Bool({n})"),
                            Value::Map(m) => format!("Map({m:?})"),
                            other => format!("{:?}", std::mem::discriminant(other)),
                        },
                    )
                    .collect();
                (
                    f.prototype_id,
                    prototype.map(|p| (&p.target, &p.schema)),
                    params,
                )
            })
            .collect();
        eprintln!(
            "P7_YIELD source={source} ip={} reason={:?} fuel={:?} frames={frames:?} stack={:?}",
            machine.ip(),
            machine.last_yield_reason(),
            machine.get_fuel(),
            machine.stack()
        );
        if source.contains("getOwnPropertyDescriptor") {
            let program = machine.program();
            let ip = machine.ip();
            let op = vm::OpCode::try_from(program.code[ip]).ok();
            let owner = program
                .script_functions
                .iter()
                .enumerate()
                .find(|(_, function)| {
                    function.entry_ip as usize <= ip && ip < function.end_ip as usize
                })
                .map(|(index, function)| (index, function.entry_ip, function.end_ip));
            eprintln!("P7_D08_SITE ip={ip} op={op:?} owner={owner:?}");
        }
    }
    if uncaught {
        assert!(
            matches!(&status,Err(vm::VmError::HostError(message)) if message=="uncaught JavaScript throw"),
            "actual uncaught adapter: {status:?}"
        );
    } else {
        assert_eq!(
            status.unwrap_or_else(|error| panic!(
                "interpreter run {source}: {error:?}; ip={}; stack={:?}; frames={:?}",
                machine.ip(),
                machine.stack(),
                machine.execution_frames()
            )),
            VmStatus::Halted
        );
    }
    let count = machine.aot_exec_count();
    if native {
        let info = machine.dump_aot_info();
        eprintln!("P7_OBJECT_NATIVE count={count}\n{info}");
        assert!(
            count > 0 && info.contains("lowering=ssa"),
            "real native SSA execution"
        );
    } else {
        assert_eq!(count, 0, "interpreter only");
        assert_eq!(machine.jit_native_exec_count(), 0, "no trace execution");
        assert!(!machine.jit_config().enabled);
    }
    assert!(
        machine
            .capture_certification_stats()
            .is_some_and(|facts| facts.valid)
    );
    assert_eq!(
        machine.host_context().resource_count(),
        1,
        "one actual backing owner"
    );
    let b1 = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("actual backing accounting");
    assert_eq!((b1.pending, b1.registered), (0, 1));
    let actual = machine.stack().to_vec();
    machine.reset_for_reuse().expect("reset");
    assert_eq!(
        machine.host_context().resource_count(),
        0,
        "reset releases the real owner"
    );
    assert!(
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).is_none()
    );
    assert!(
        !machine
            .capture_certification_stats()
            .expect("revoked certification")
            .valid
    );
    actual
}

#[test]
fn p7_compact_object_metadata_interpreter() {
    struct Metadata;
    impl vm::SourcePlugin for Metadata {
        fn descriptor(&self) -> vm::SourceFrontendDescriptor {
            let mut descriptor = vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN);
            descriptor.id = vm::FrontendId::new("javascript-es5-object-metadata");
            descriptor
        }
        fn parse_source(
            &self,
            source: &str,
            context: &vm::SourceFrontendContext<'_>,
        ) -> Result<vm::FrontendIr, vm::ParseError> {
            let mut ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
            let helper = |name: &str| ir.functions.iter().find(|f| f.name == name).unwrap().index;
            let read = helper("__compat_ref_read");
            let checked = helper("__compat_es5_checked_base");
            let carrier = ir
                .local_bindings
                .iter()
                .find(|(n, _)| n == "__compat_es5_realm")
                .map(|(_, i)| *i);
            let carrier = carrier.unwrap_or_else(|| {
                ir.stmts
                    .iter()
                    .find_map(|s| match s {
                        vm::Stmt::Let {
                            index,
                            expr: vm::Expr::Call(target, _, _, _, _),
                            ..
                        } if *target == helper("__compat_ref_make") => Some(*index),
                        _ => None,
                    })
                    .expect("real carrier")
            });
            let object = ir.local_bindings.iter().find(|(n, _)| n == "o").unwrap().1;
            let get = |field: &str| {
                vm::Expr::Call(
                    vm::BuiltinFunction::Get.call_index(),
                    Vec::new(),
                    vec![vm::Expr::Var(object), vm::Expr::String(field.into())],
                    None,
                    None,
                )
            };
            let base = ir.locals as u16;
            ir.locals += 1;
            ir.stmts.push(vm::Stmt::Let {
                index: base,
                declared_schema: Some(vm::TypeSchema::Int),
                expr: vm::Expr::Call(
                    checked,
                    Vec::new(),
                    vec![vm::Expr::Var(carrier), get("ref_slot"), get("generation")],
                    None,
                    None,
                ),
                line: 1,
            });
            for offset in [2, 7] {
                ir.stmts.push(vm::Stmt::Expr {
                    expr: vm::Expr::Call(
                        read,
                        Vec::new(),
                        vec![
                            vm::Expr::Var(carrier),
                            vm::Expr::Div(
                                Box::new(vm::Expr::Add(
                                    Box::new(vm::Expr::Var(base)),
                                    Box::new(vm::Expr::Int(offset)),
                                )),
                                Box::new(vm::Expr::Float(1.0)),
                            ),
                        ],
                        None,
                        None,
                    ),
                    line: 1,
                });
            }
            Ok(ir)
        }
    }
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        "var o={x:true};o.x;",
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-object-metadata")),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(&Metadata)
            .with_host_api_catalog(catalog.clone()),
    )
    .unwrap_or_else(|error| panic!("real metadata compilation: {error:?}"));
    let mut machine = compiled.into_vm().expect("certified");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        Default::default(),
    )
    .expect("owner");
    let mut registry = HostFunctionRegistry::restricted();
    pd_vm_compat_frontends::runtime::array::typescript_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .unwrap();
    pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .unwrap();
    pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .unwrap();
    pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .unwrap();
    pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .unwrap();
    registry.allow_builtin("math::floor").unwrap();
    registry.bind_vm_cached(&mut machine).unwrap();
    machine.set_fuel(10_000_000);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    let tail = &machine.stack()[machine.stack().len() - 2..];
    assert_eq!(
        tail,
        &[Value::Float(5.0), Value::Float(39.0)],
        "packed one-property extensible header; Boolean WEC data row"
    );
    machine.reset_for_reuse().unwrap();
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn p7_general_call_with_exact_adapter_stays_normal_interpreter() {
    oracle_p7_general_call_with_exact_adapter_stays_normal_interpreter(false);
}

fn oracle_p7_general_call_with_exact_adapter_stays_normal_interpreter(native: bool) {
    let runner = Runner { native };
    let actual =
        runner.execute("function f(){'use strict';return false;}var r=0;if(f()){r=1;}else{r=2;}r;");
    assert_eq!(field(&actual, "tag"), Value::Int(3));
    assert_eq!(field(&actual, "number"), Value::Float(2.0));
}

#[test]
fn p7_o01_inherited_get_has_and_missing_own_descriptor_interpreter() {
    oracle_p7_o01_inherited_get_has_and_missing_own_descriptor_interpreter(false);
}

fn oracle_p7_o01_inherited_get_has_and_missing_own_descriptor_interpreter(native: bool) {
    let runner = Runner { native };
    let actual = runner.execute(
        "var p={x:1};var o=Object.create(p);var a=o.x===1;var b=\"x\" in o;var c=Object.getOwnPropertyDescriptor(o,\"x\")===undefined;a&&b&&c;",
    );
    assert_eq!(field(&actual, "tag"), Value::Int(2));
    assert_eq!(field(&actual, "flag"), Value::Bool(true));
}

#[test]
fn p7_data_vectors_interpreter() {
    oracle_p7_data_vectors_interpreter(false);
}

fn oracle_p7_data_vectors_interpreter(native: bool) {
    let runner = Runner { native };
    for (id, source) in [
        (
            "O02",
            "var o=Object.create(null);o.x=undefined;(\"x\" in o)&&!(\"z\" in o)&&o.z===undefined&&Object.getPrototypeOf(o)===null;",
        ),
        (
            "O03",
            "var p={x:1};var o=Object.create(p);o.x=2;var a=o.x===2&&p.x===1;var b=delete o.x;a&&b&&o.x===1;",
        ),
        (
            "O04",
            "var o={};Object.defineProperty(o,\"x\",{value:1});o.x=2;var d=Object.getOwnPropertyDescriptor(o,\"x\");o.x===1&&d.writable===false&&d.enumerable===false&&d.configurable===false&&(delete o.x)===false;",
        ),
        (
            "O06",
            "var o={};Object.preventExtensions(o);o.x=1;var a=!(\"x\" in o);try{Object.defineProperty(o,\"x\",{value:1});}catch(e){a&&e.name===\"TypeError\";}",
        ),
        (
            "O14",
            "var o={};Object.defineProperty(o,\"x\",{value:0/0});Object.defineProperty(o,\"x\",{value:0/0});var a=true;Object.defineProperty(o,\"z\",{value:0});try{Object.defineProperty(o,\"z\",{value:-0});}catch(e){a&&e.name===\"TypeError\";}",
        ),
        (
            "O16",
            "var o={\"__proto__\":7,\"\\uD800\\0\":true};o.__proto__===7&&o[\"\\uD800\\0\"]===true;",
        ),
    ] {
        let actual = runner.execute(source);
        eprintln!("P7_VECTOR id={id} actual={actual:?}");
        assert_eq!(field(&actual, "tag"), Value::Int(2), "{id}");
        assert_eq!(field(&actual, "flag"), Value::Bool(true), "{id}");
    }
}

#[test]
fn p7_o07_strict_closed_getter_receiver_interpreter() {
    oracle_p7_o07_strict_closed_getter_receiver_interpreter(false);
}

fn oracle_p7_o07_strict_closed_getter_receiver_interpreter(native: bool) {
    let runner = Runner { native };
    let actual = runner
        .execute("var p={get x(){\"use strict\";return this;}};var o=Object.create(p);o.x===o;");
    assert_eq!(field(&actual, "tag"), Value::Int(2));
    assert_eq!(field(&actual, "flag"), Value::Bool(true));
}

#[test]
fn p7_type_error_name_interpreter() {
    oracle_p7_type_error_name_interpreter(false);
}

fn oracle_p7_type_error_name_interpreter(native: bool) {
    let runner = Runner { native };
    for source in [
        "\"use strict\";var o={};Object.defineProperty(o,\"x\",{value:1});try{o.x=2;}catch(e){e.name;}",
        "\"use strict\";try{null.x;}catch(e){e.name;}",
    ] {
        let actual = runner.run(
            source,
            Some(object_observer::Observer {
                units: Some(9),
                force_gc: false,
                ..Default::default()
            }),
        );
        let marker = actual
            .iter()
            .position(|v| *v == Value::string("P7_OBSERVED"))
            .expect("real completion observation");
        assert_eq!(field(&actual[marker + 1], "tag"), Value::Int(4));
        assert_eq!(actual[marker + 2], Value::Float(9.0));
        let units: Vec<_> = "TypeError"
            .encode_utf16()
            .map(|u| Value::Float(f64::from(u)))
            .collect();
        assert_eq!(&actual[marker + 3..marker + 12], &units);
        eprintln!("P7_TYPE_ERROR actual_utf16={units:?}");
    }
}

#[test]
fn p7_accessor_vectors_interpreter() {
    oracle_p7_accessor_vectors_interpreter(false);
}

fn oracle_p7_accessor_vectors_interpreter(native: bool) {
    let runner = Runner { native };
    for (id, source) in [
        (
            "O08",
            "var o={v:0,set x(v){\"use strict\";this.v=v;}};var a=(o.x=7);a===7&&o.v===7;",
        ),
        (
            "O10",
            "var o={set x(v){\"use strict\";throw v;}};var marker={k:true};try{o.x=marker;}catch(e){e===marker;}",
        ),
        (
            "O13",
            "var o={get x(){\"use strict\";return 1;}};o.x=2;var a=o.x===1;try{Object.defineProperty(o,\"z\",{get:1});}catch(e){a&&e.name===\"TypeError\";}",
        ),
        (
            "O15",
            "var o={};var d={get value(){\"use strict\";throw \"desc\";}};try{Object.defineProperty(o,\"x\",d);}catch(e){e===\"desc\"&&!(\"x\" in o);}",
        ),
    ] {
        let actual = runner.execute(source);
        eprintln!("P7_VECTOR id={id} actual={actual:?}");
        assert_eq!(field(&actual, "tag"), Value::Int(2), "{id}");
        assert_eq!(field(&actual, "flag"), Value::Bool(true), "{id}");
    }
}

#[test]
fn p7_accessor_utf16_and_order_interpreter() {
    oracle_p7_accessor_utf16_and_order_interpreter(false);
}

fn oracle_p7_accessor_utf16_and_order_interpreter(native: bool) {
    let runner = Runner { native };
    for (id, source, units) in [
        (
            "O09",
            "var o={get x(){\"use strict\";throw \"\\uD800\\0\";}};try{o.x;}catch(e){e;}",
            &[55296, 0][..],
        ),
        (
            "O12",
            "var o={get m(){\"use strict\";throw \"get\";},get a(){\"use strict\";throw \"arg\";}};try{o.m(o.a);}catch(e){e;}",
            &[103, 101, 116][..],
        ),
    ] {
        let actual = runner.run(
            source,
            Some(object_observer::Observer {
                units: Some(units.len()),
                ..Default::default()
            }),
        );
        let marker = actual
            .iter()
            .position(|v| *v == Value::string("P7_OBSERVED"))
            .unwrap();
        assert_eq!(field(&actual[marker + 1], "tag"), Value::Int(4), "{id}");
        assert_eq!(actual[marker + 2], Value::Float(units.len() as f64));
        let expected: Vec<_> = units.iter().map(|u| Value::Float(f64::from(*u))).collect();
        assert_eq!(
            &actual[marker + 3..marker + 3 + units.len()],
            &expected,
            "{id}"
        );
        eprintln!(
            "P7_UTF16 id={id} actual={:?}",
            &actual[marker + 3..marker + 3 + units.len()]
        );
    }
}

#[test]
fn p7_o11_uncaught_getter_original_payload_interpreter() {
    oracle_p7_o11_uncaught_getter_original_payload_interpreter(false);
}

fn oracle_p7_o11_uncaught_getter_original_payload_interpreter(native: bool) {
    let runner = Runner { native };
    let actual = runner.run(
        "var o={get x(){\"use strict\";throw \"\\uD800\\0\";}};o.x;",
        Some(object_observer::Observer {
            units: Some(2),
            uncaught: true,
            ..Default::default()
        }),
    );
    let marker = actual
        .iter()
        .position(|v| *v == Value::string("P7_OBSERVED"))
        .unwrap();
    assert_eq!(field(&actual[marker + 1], "tag"), Value::Int(4));
    assert_eq!(
        &actual[marker + 2..marker + 5],
        &[Value::Float(2.0), Value::Float(55296.0), Value::Float(0.0)]
    );
}

#[test]
fn p7_o18_uncaught_real_type_error_interpreter() {
    oracle_p7_o18_uncaught_real_type_error_interpreter(false);
}

fn oracle_p7_o18_uncaught_real_type_error_interpreter(native: bool) {
    let runner = Runner { native };
    let actual = runner.run(
        "var p={set x(v){\"use strict\";this.y=v;}};var o=Object.create(p);Object.preventExtensions(o);o.x=7;!(\"y\" in o);",
        Some(object_observer::Observer {
            uncaught: true,
            type_error: true,
            ..Default::default()
        }),
    );
    let marker = actual
        .iter()
        .position(|v| *v == Value::string("P7_OBSERVED"))
        .unwrap();
    assert_eq!(field(&actual[marker + 1], "tag"), Value::Int(5));
    assert_eq!(
        &actual[marker + 2..marker + 6],
        &[
            Value::Float(3.0),
            Value::Float(3.0),
            Value::Bool(true),
            Value::Bool(true)
        ]
    );
}

#[test]
fn p7_descriptor_phases_interpreter() {
    oracle_p7_descriptor_phases_interpreter(false);
}

fn oracle_p7_descriptor_phases_interpreter(native: bool) {
    let runner = Runner { native };
    for (id, source) in [
        (
            "D01",
            "var o={};try{Object.defineProperties(o,{a:{value:1},b:{get:1}});}catch(e){e.name===\"TypeError\"&&!(\"a\" in o);}",
        ),
        (
            "D02",
            "var o={};Object.defineProperty(o,\"b\",{value:0});try{Object.defineProperties(o,{a:{value:1},b:{value:2}});}catch(e){o.a===1&&o.b===0;}",
        ),
        (
            "D03",
            "var d=Object.create({value:7,writable:true});var o={};Object.defineProperty(o,\"x\",d);o.x=8;o.x===8;",
        ),
        (
            "D08",
            "var o=Object.create(null,{x:{value:7}});o.x===7&&Object.getOwnPropertyDescriptor(o,\"x\").writable===false;",
        ),
    ] {
        let actual = runner.execute(source);
        eprintln!("P7_SUPPLEMENT id={id} actual={actual:?}");
        assert_eq!(field(&actual, "tag"), Value::Int(2), "{id}");
        assert_eq!(field(&actual, "flag"), Value::Bool(true), "{id}");
    }
}

#[test]
fn p7_call_corrections_interpreter() {
    oracle_p7_call_corrections_interpreter(false);
}

fn oracle_p7_call_corrections_interpreter(native: bool) {
    let runner = Runner { native };
    for (id, source) in [
        ("D05", "var o={get x(){\"use strict\";1;}};o.x===undefined;"),
        (
            "D07",
            "var p={set x(v){\"use strict\";throw v;}};var o=Object.create(p);Object.preventExtensions(o);var marker={};try{o.x=marker;}catch(e){e===marker;}",
        ),
        (
            "D09",
            "var o={set x(v){\"use strict\";return 0;}};(o.x=7)===7;",
        ),
    ] {
        let actual = runner.execute(source);
        eprintln!("P7_SUPPLEMENT id={id} actual={actual:?}");
        assert_eq!(field(&actual, "tag"), Value::Int(2), "{id}");
        assert_eq!(field(&actual, "flag"), Value::Bool(true), "{id}");
    }
}

#[test]
fn p7_descriptor_field_and_noncallable_order_interpreter() {
    oracle_p7_descriptor_field_and_noncallable_order_interpreter(false);
}

fn oracle_p7_descriptor_field_and_noncallable_order_interpreter(native: bool) {
    let runner = Runner { native };
    for (id, source, text) in [
        (
            "D04",
            "var o={};var d={get enumerable(){\"use strict\";throw \"enum\";},get configurable(){\"use strict\";throw \"conf\";}};try{Object.defineProperty(o,\"x\",d);}catch(e){e;}",
            "enum",
        ),
        (
            "D06",
            "var o={m:1,get a(){\"use strict\";throw \"arg\";}};try{o.m(o.a);}catch(e){e;}",
            "arg",
        ),
    ] {
        let actual = runner.run(
            source,
            Some(object_observer::Observer {
                units: Some(text.len()),
                ..Default::default()
            }),
        );
        let marker = actual
            .iter()
            .position(|v| *v == Value::string("P7_OBSERVED"))
            .unwrap();
        assert_eq!(field(&actual[marker + 1], "tag"), Value::Int(4), "{id}");
        assert_eq!(actual[marker + 2], Value::Float(text.len() as f64));
        let units: Vec<_> = text
            .encode_utf16()
            .map(|u| Value::Float(f64::from(u)))
            .collect();
        assert_eq!(
            &actual[marker + 3..marker + 3 + units.len()],
            &units,
            "{id}"
        );
    }
}

#[test]
fn p7_forced_gc_accessor_and_descriptor_handoffs_interpreter() {
    oracle_p7_forced_gc_accessor_and_descriptor_handoffs_interpreter(false);
}

fn oracle_p7_forced_gc_accessor_and_descriptor_handoffs_interpreter(native: bool) {
    let runner = Runner { native };
    for (id, source) in [
        (
            "O07",
            "var p={get x(){\"use strict\";return this;}};var o=Object.create(p);o.x===o;",
        ),
        (
            "O08",
            "var o={v:0,set x(v){\"use strict\";this.v=v;}};var a=(o.x=7);a===7&&o.v===7;",
        ),
        (
            "D01",
            "var o={};try{Object.defineProperties(o,{a:{value:1},b:{get:1}});}catch(e){e.name===\"TypeError\"&&!(\"a\" in o);}",
        ),
        (
            "D08",
            "var o=Object.create(null,{x:{value:7}});o.x===7&&Object.getOwnPropertyDescriptor(o,\"x\").writable===false;",
        ),
    ] {
        let actual = runner.run(
            source,
            Some(object_observer::Observer {
                force_gc: true,
                ..Default::default()
            }),
        );
        let marker = actual
            .iter()
            .position(|v| *v == Value::string("P7_OBSERVED"))
            .unwrap();
        assert_eq!(field(&actual[marker + 1], "tag"), Value::Int(2), "{id}");
        assert_eq!(
            field(&actual[marker + 1], "flag"),
            Value::Bool(true),
            "{id}"
        );
        assert_eq!(
            actual[actual.len() - 2],
            Value::Int(0),
            "no pending node {id}"
        );
        eprintln!("P7_FORCED_GC id={id} tail={:?}", &actual[marker..]);
    }
}

const D08_FORCED_SOURCE: &str = "var o=Object.create(null,{x:{value:7}});o.x===7&&Object.getOwnPropertyDescriptor(o,\"x\").writable===false;";

#[derive(Debug)]
struct CollectProbe {
    fuel_before: Option<u64>,
    fuel_after: Option<u64>,
    jit_enabled: bool,
    aot_executions: u64,
    jit_native_executions: u64,
    status: Result<VmStatus, vm::VmError>,
    reason: Option<vm::VmYieldReason>,
    ip: usize,
    phase: Option<&'static str>,
    phase_label: Option<u32>,
    phase_site: &'static str,
    completed_at_entry: Option<i64>,
    helper_prototype: Option<u32>,
    helper_region: Option<(u32, u32, Option<u32>)>,
    frames: Vec<String>,
}

fn assertion_failed(status: &Result<VmStatus, vm::VmError>) -> bool {
    matches!(status, Err(vm::VmError::HostError(message)) if message == "assertion failed")
}

/// One real D08 VM in the default-off collector-diagnostic mode. Records the
/// machine state immediately after `run` and before any expected-status
/// assertion, so a fuel yield or deliberate stop is measured, never assumed.
fn collect_probe(source: &str, stop: Option<(i64, u32)>) -> CollectProbe {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_host_api_catalog(catalog.clone())
        .with_source_plugin(Box::leak(Box::new(object_observer::Observer {
            force_gc: true,
            collect_phases: true,
            collect_stop: stop,
            ..Default::default()
        })));
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-object-observer")),
        options,
    )
    .unwrap_or_else(|error| panic!("diagnostic D08 must compile:\n{error:?}"));
    let mut machine = compiled.into_vm().expect("certified VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("owned arrays");
    let mut registry = HostFunctionRegistry::restricted();
    for module in [
        pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
        pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module(),
    ] {
        module
            .install_from_catalog(&mut registry, &catalog)
            .expect("exact catalog");
    }
    registry.allow_builtin("math::floor").expect("floor");
    // The ordinary realm's bootstrap publishes its fixed ASCII strings through
    // the Core bytes initializer, so this capability is part of the emitted
    // program (the Object.keys harness already allows the same builtin).
    registry
        .allow_builtin("bytes::from_utf8")
        .expect("bootstrap strings");
    registry.bind_vm_cached(&mut machine).expect("bind");
    machine.set_fuel(10_000_000);
    let fuel_before = machine.get_fuel();
    let status = machine.run();
    let fuel_after = machine.get_fuel();
    let jit_enabled = machine.jit_config().enabled;
    let aot_executions = machine.aot_exec_count();
    let jit_native_executions = machine.jit_native_exec_count();
    let reason = machine.last_yield_reason();
    let ip = machine.ip();
    let frames = machine.execution_frames();
    let program = machine.program();
    let debug = machine.debug_info();
    // The diagnostic helper is bound only through its retagged INIT region
    // plus the exact arity-2 / two-slot topology. The older DebugInfo
    // function-name position fallback is removed: script-function numbering
    // is not declaration order, so it cannot identify the helper.
    let mut region_matches = Vec::new();
    for (id, prototype) in program.callable_prototypes.iter().enumerate() {
        if prototype.arity != 2 || prototype.parameter_slots.len() != 2 {
            continue;
        }
        let tagged = program.function_regions.iter().any(|region| {
            region.prototype_id == Some(id as u32)
                && debug
                    .and_then(|info| info.line_for_offset(region.start_ip as usize))
                    .is_some_and(|label| {
                        object_observer::diag_phase(label).is_some_and(|phase| phase == "init")
                            || object_observer::diag_stop_phase(label)
                                .is_some_and(|phase| phase == "init")
                    })
        });
        if tagged {
            region_matches.push(id as u32);
        }
    }
    assert_eq!(
        region_matches.len(),
        1,
        "D08_PROBE_REGION_AMBIGUOUS matches={:?} expected exactly one arity-2 INIT helper",
        region_matches
    );
    let helper_prototype = Some(region_matches[0]);
    let helper_param = helper_prototype
        .and_then(|prototype| {
            program.callable_prototypes[prototype as usize]
                .parameter_slots
                .get(1)
        })
        .copied();
    let helper_region = helper_prototype.and_then(|prototype| {
        program
            .function_regions
            .iter()
            .find(|region| region.prototype_id == Some(prototype))
            .map(|region| {
                (
                    region.start_ip,
                    region.end_ip,
                    debug.and_then(|info| info.line_for_offset(region.start_ip as usize)),
                )
            })
    });
    for (id, region) in program.function_regions.iter().enumerate() {
        if let Some(label) = debug.and_then(|info| info.line_for_offset(region.start_ip as usize))
            && object_observer::diag_phase(label).is_some()
        {
            eprintln!(
                "D08_PHASE_REGION {id} {}..{} prototype={:?} start_line={label}",
                region.start_ip, region.end_ip, region.prototype_id
            );
        }
    }
    if let Some(prototype) = helper_prototype {
        let bound = &program.callable_prototypes[prototype as usize];
        eprintln!(
            "D08_PROBE_HELPER prototype={prototype} target={:?} arity={} params={:?} region={:?}",
            bound.target, bound.arity, bound.parameter_slots, helper_region
        );
    } else {
        eprintln!("D08_PROBE_HELPER bound=false region={:?}", helper_region);
    }
    let mut completed_at_entry = None;
    let mut rendered_frames = Vec::with_capacity(frames.len() + 1);
    for frame in &frames {
        if frame.prototype_id == helper_prototype
            && let Some(slot) = helper_param
            && let Some(Value::Int(count)) =
                machine.locals().get(frame.local_base + usize::from(slot))
        {
            completed_at_entry = Some(*count);
        }
        let continuation = match &frame.continuation {
            vm::vm::VmFrameContinuation::Halt => "halt".to_owned(),
            vm::vm::VmFrameContinuation::ResumeBytecode { return_ip } => format!(
                "resume:{return_ip}@line{:?}",
                debug.and_then(|info| info.line_for_offset(*return_ip))
            ),
            vm::vm::VmFrameContinuation::ReturnToHost => "host".to_owned(),
        };
        rendered_frames.push(format!(
            "proto{:?}{}:{continuation}:base{}",
            frame.prototype_id,
            if frame.prototype_id == helper_prototype {
                "(diag)"
            } else {
                ""
            },
            frame.local_base
        ));
    }
    // Host post-run phase attribution is provenance-gated: the global stop ip
    // is outside any compiled helper body at exhaustion, so its own debug
    // label is a floor/wrapper artifact, never a collector phase. Accept an
    // ip phase only when the live ip sits inside the live bound helper
    // region; otherwise attribute only through the helper's verified direct
    // callee whose ResumeBytecode continuation re-enters the helper across a
    // real decoded CallScript boundary. Anything unproven stays unattributed.
    let (mut phase_label, mut phase_site): (Option<u32>, &'static str) = (None, "unknown");
    if let (Some(helper), Some((region_start, region_end, _))) = (helper_prototype, helper_region) {
        let start = region_start as usize;
        let end = region_end as usize;
        if start <= ip && ip < end {
            if let Some(label) = debug.and_then(|info| info.line_for_offset(ip))
                && object_observer::diag_phase(label)
                    .or_else(|| object_observer::diag_stop_phase(label))
                    .is_some()
            {
                phase_label = Some(label);
                phase_site = "ip";
            }
        } else if let Some(helper_index) = frames
            .iter()
            .position(|frame| frame.prototype_id == Some(helper))
            && let Some(callee) = frames.get(helper_index + 1)
            && let vm::vm::VmFrameContinuation::ResumeBytecode { return_ip } = &callee.continuation
            && start <= *return_ip
            && *return_ip < end
            && let Some(child) = callee.prototype_id
            && let Some(child_proto) = program.callable_prototypes.get(child as usize)
            && matches!(child_proto.target, vm::CallableTarget::ScriptFunction(_))
        {
            // Linear decode of the live helper body from its region start to
            // the exact continuation ip. Every read stays inside the helper
            // region, the code buffer, and the return ip; truncation, unknown
            // opcodes, or overshoot fail the proof without panicking.
            let mut cursor = start;
            let mut last: Option<(usize, vm::OpCode)> = None;
            let mut valid = start < end && end <= program.code.len();
            while valid && cursor < *return_ip {
                let Some(raw) = program.code.get(cursor).copied() else {
                    valid = false;
                    break;
                };
                let Ok(opcode) = vm::OpCode::try_from(raw) else {
                    valid = false;
                    break;
                };
                let next = cursor + 1 + opcode.operand_len();
                if next > *return_ip || next > end || next > program.code.len() {
                    valid = false;
                    break;
                }
                last = Some((cursor, opcode));
                cursor = next;
            }
            let call_len = vm::OpCode::CallScript.operand_len();
            if valid
                && cursor == *return_ip
                && let Some((call_ip, vm::OpCode::CallScript)) = last
                && call_ip + 1 + call_len == *return_ip
                && let Some(window) = program.code.get(call_ip + 1..call_ip + 1 + call_len)
                && window.len() == call_len
            {
                // Core CallScript semantics: `prototype_id:u32` little-endian
                // then `argc:u8`; the continuation is the next ip after the
                // operands. The operand is a prototype id, never a
                // CallableTarget::ScriptFunction index.
                let target = u32::from_le_bytes([window[0], window[1], window[2], window[3]]);
                let argc = window[4];
                if target == child
                    && argc == child_proto.arity
                    && let Some(label) = debug.and_then(|info| info.line_for_offset(call_ip))
                    && (object_observer::diag_phase(label).is_some()
                        || object_observer::diag_stop_phase(label).is_some())
                {
                    eprintln!(
                        "D01_PHASE_CALLSITE helper={helper} child={child} child_target={:?} region={start}..{end} call_start={call_ip} return_ip={} decoded_target={target} decoded_argc={argc} expected_arity={} label={label}",
                        child_proto.target, *return_ip, child_proto.arity,
                    );
                    phase_label = Some(label);
                    phase_site = "direct-callee";
                }
            }
        }
    }
    let phase = phase_label.and_then(|label| {
        object_observer::diag_phase(label).or_else(|| object_observer::diag_stop_phase(label))
    });
    if completed_at_entry.is_none() {
        let names: Vec<String> = debug
            .map(|info| info.functions.iter().map(|f| f.name.clone()).collect())
            .unwrap_or_default();
        eprintln!(
            "D08_PROBE_DEBUG functions={} diag_position={:?} names={names:?}",
            names.len(),
            names
                .iter()
                .position(|name| name == "__compat_es5_diag_collect")
        );
        for (id, prototype) in program.callable_prototypes.iter().enumerate() {
            eprintln!(
                "D08_PROBE_PROTO {id} target={:?} arity={} params={:?}",
                prototype.target, prototype.arity, prototype.parameter_slots
            );
        }
        for (id, region) in program.function_regions.iter().enumerate() {
            if region.start_ip as usize <= ip && ip < region.end_ip as usize {
                eprintln!(
                    "D08_PROBE_REGION {id} {}..{} prototype={:?}",
                    region.start_ip, region.end_ip, region.prototype_id
                );
            }
        }
    }
    CollectProbe {
        fuel_before,
        fuel_after,
        jit_enabled,
        aot_executions,
        jit_native_executions,
        status,
        reason,
        ip,
        phase,
        phase_label,
        phase_site,
        completed_at_entry,
        helper_prototype,
        helper_region,
        frames: rendered_frames,
    }
}

#[test]
fn p7_forced_gc_d08_collection_phase_measurement_interpreter() {
    let full = collect_probe(D08_FORCED_SOURCE, None);
    eprintln!(
        "D08_COLLECT_FULL fuel={:?}->{:?} jit={} aot={} native={} status={:?} reason={:?} ip={} phase={:?} label={:?} site={} completed_at_entry={:?} helper_prototype={:?} helper_region={:?} frames={:?}",
        full.fuel_before,
        full.fuel_after,
        full.jit_enabled,
        full.aot_executions,
        full.jit_native_executions,
        full.status,
        full.reason,
        full.ip,
        full.phase,
        full.phase_label,
        full.phase_site,
        full.completed_at_entry,
        full.helper_prototype,
        full.helper_region,
        full.frames
    );
    assert!(
        matches!(full.status, Ok(VmStatus::Yielded)),
        "the unchanged 10m D08 budget must yield, got {:?}",
        full.status
    );
    assert_eq!(
        full.reason,
        Some(vm::VmYieldReason::Fuel),
        "unfinished collection must be a fuel yield"
    );
    assert_eq!(
        full.fuel_after,
        Some(0),
        "the original 10m budget is exhausted, not recharged"
    );
    assert!(
        !full.jit_enabled,
        "diagnostic run must stay interpreter-only"
    );
    assert_eq!(full.aot_executions, 0, "no AOT execution");
    assert_eq!(full.jit_native_executions, 0, "no native trace execution");
    let phase = full
        .phase
        .expect("exhaustion must map to a collector phase");
    let phase_label = full
        .phase_label
        .expect("exhaustion must carry a collector phase label");
    let completed = full
        .completed_at_entry
        .expect("the completed-at-entry parameter must be live at exhaustion");
    assert!(completed >= 0, "completed collection count is nonnegative");
    let order = [
        object_observer::DIAG_PHASE_CLEAR,
        object_observer::DIAG_PHASE_ROOTS,
        object_observer::DIAG_PHASE_DRAIN,
        object_observer::DIAG_PHASE_SWEEP,
        object_observer::DIAG_PHASE_COALESCE,
        object_observer::DIAG_PHASE_COMMIT,
    ];
    let reached: Vec<u32> = order
        .iter()
        .copied()
        .filter(|boundary| *boundary <= phase_label)
        .collect();
    assert!(
        reached.len() >= 2,
        "need at least two reached adjacent phase boundaries, got {reached:?} in phase {phase}"
    );
    let picks = &reached[reached.len().saturating_sub(4)..];
    let mut stops = Vec::with_capacity(picks.len());
    for boundary in picks {
        let probe = collect_probe(D08_FORCED_SOURCE, Some((completed, *boundary)));
        eprintln!(
            "D08_PHASE_STOP phase={} fuel={:?}->{:?} status={:?} reason={:?} ip={} label={:?} site={} completed_at_entry={:?} frames={:?}",
            object_observer::diag_phase(*boundary).unwrap_or("?"),
            probe.fuel_before,
            probe.fuel_after,
            probe.status,
            probe.reason,
            probe.ip,
            probe.phase_label,
            probe.phase_site,
            probe.completed_at_entry,
            probe.frames
        );
        assert!(
            assertion_failed(&probe.status),
            "boundary {} must stop on the existing assert builtin, got {:?}",
            object_observer::diag_phase(*boundary).unwrap_or("?"),
            probe.status
        );
        assert_eq!(
            probe.completed_at_entry,
            Some(completed),
            "the completed-at-entry parameter must stay exact at the stop"
        );
        assert_eq!(
            probe.phase,
            object_observer::diag_phase(*boundary),
            "stop must be observed at its own boundary"
        );
        stops.push((*boundary, probe.fuel_after.expect("stop fuel")));
    }
    for pair in stops.windows(2) {
        assert!(
            pair[0].1 >= pair[1].1,
            "later prefix cannot have more remaining fuel: {stops:?}"
        );
    }
    let prefix = 10_000_000 - stops[0].1;
    eprintln!(
        "D08_PHASE_TABLE completed_cycle_entry={completed} exhausting_phase={phase} prefix_to_{}={prefix} stops={:?}",
        object_observer::diag_phase(stops[0].0).unwrap_or("?"),
        stops
            .iter()
            .map(|(boundary, fuel)| (
                object_observer::diag_phase(*boundary).unwrap_or("?"),
                10_000_000 - fuel
            ))
            .collect::<Vec<_>>()
    );
    for pair in stops.windows(2) {
        let cost = pair[0].1 - pair[1].1;
        eprintln!(
            "D08_PHASE_COST phase={} inclusive_fuel={cost}",
            object_observer::diag_phase(pair[0].0).unwrap_or("?")
        );
    }
    let tail = stops.last().expect("last stop").1;
    eprintln!(
        "D08_PHASE_COST phase={} inclusive_fuel={tail} (rest of exhaustion phase)",
        object_observer::diag_phase(stops.last().expect("last stop").0).unwrap_or("?")
    );
}

const D01_FORCED_SOURCE: &str = "var o={};try{Object.defineProperties(o,{a:{value:1},b:{get:1}});}catch(e){e.name===\"TypeError\"&&!(\"a\" in o);}";
/// Bounded descriptor-frame capacity for `D01_FORCED_SOURCE`: the fixed eight
/// root entries (0 target, 1 props, 3 value, 7 error plus the four reserved at
/// 2/4/5/6) plus four entries per validated props row. The source's props
/// object has exactly two rows, so the requested capacity is 8 + 4 * 2.
const D01_FRAME_CAPACITY: i64 = 16;

#[derive(Clone, Copy, Debug)]
struct D01Sample {
    collections: i64,
    live: i64,
    root_capacity: i64,
    pending: i64,
}

#[derive(Debug)]
struct D01Probe {
    fuel_after: Option<u64>,
    status: Result<VmStatus, vm::VmError>,
    reason: Option<vm::VmYieldReason>,
    ip: usize,
    phase: Option<&'static str>,
    phase_label: Option<u32>,
    root_prototype: Option<u32>,
    frame_capacity: Option<i64>,
    cursor: Option<i64>,
    sample: Option<D01Sample>,
    completed: Option<i64>,
    frames: Vec<String>,
    /// Owning script-function region of the terminal ip, from the public
    /// `script_functions`/`function_regions` metadata: region index, entry,
    /// end and the callable target of that region.
    site: Option<(usize, u32, u32, String)>,
    /// Every active frame at the stop, bound through the public callable
    /// prototype and the actual `frame.local_base`: prototype id, target,
    /// arity and the live values of the mapped parameter slots only.
    named_frames: Vec<String>,
    /// Source-level local bindings published by the public debug metadata.
    /// These are root-script bindings only; helper temporaries are not named.
    debug_local_names: Vec<String>,
    jit_enabled: bool,
    aot_executions: u64,
    jit_native_executions: u64,
}

fn diagnostic_label(machine: &vm::Vm, prototype: u32) -> Option<u32> {
    let debug = machine.debug_info()?;
    machine
        .program()
        .function_regions
        .iter()
        .find(|region| region.prototype_id == Some(prototype))
        .and_then(|region| debug.line_for_offset(region.start_ip as usize))
}

/// Locate an active test-generated diagnostic helper by its exact parameter
/// topology and, when its region start label is meaningful, its phase label.
/// Production helpers share arities and slot numbers, so an inactive
/// prototype is never chosen.
fn active_diagnostic_prototype(
    machine: &vm::Vm,
    frames: &[vm::vm::VmExecutionFrameSnapshot],
    arity: u8,
    params: usize,
    phases: &[&str],
) -> Option<u32> {
    let mut topology = Vec::new();
    let mut labelled = Vec::new();
    for (id, prototype) in machine.program().callable_prototypes.iter().enumerate() {
        if prototype.arity != arity || prototype.parameter_slots.len() != params {
            continue;
        }
        if !frames
            .iter()
            .any(|frame| frame.prototype_id == Some(id as u32))
        {
            continue;
        }
        topology.push(id as u32);
        if let Some(label) = diagnostic_label(machine, id as u32) {
            let phase = object_observer::diag_phase(label)
                .or_else(|| object_observer::diag_stop_phase(label));
            if phase.is_some_and(|phase| phases.contains(&phase)) {
                labelled.push(id as u32);
            }
        }
    }
    match (labelled.len(), topology.len()) {
        (1, _) => Some(labelled[0]),
        (0, 1) => Some(topology[0]),
        _ => None,
    }
}

fn frame_params(
    machine: &vm::Vm,
    frames: &[vm::vm::VmExecutionFrameSnapshot],
    prototype: u32,
) -> Option<Vec<Value>> {
    let frame = frames
        .iter()
        .find(|frame| frame.prototype_id == Some(prototype))?;
    let slots = &machine.program().callable_prototypes[prototype as usize].parameter_slots;
    Some(
        slots
            .iter()
            .map(|slot| machine.locals()[frame.local_base + usize::from(*slot)].clone())
            .collect(),
    )
}

fn int_param(params: &[Value], at: usize) -> Option<i64> {
    match params.get(at)? {
        Value::Int(value) => Some(*value),
        _ => None,
    }
}

fn sample_from(params: &[Value], at: usize) -> Option<D01Sample> {
    Some(D01Sample {
        collections: int_param(params, at)?,
        live: int_param(params, at + 1)?,
        root_capacity: int_param(params, at + 2)?,
        pending: int_param(params, at + 3)?,
    })
}

/// Render one live parameter value read through the public slot mapping.
fn render_parameter(value: &Value) -> String {
    match value {
        Value::Int(number) => format!("Int({number})"),
        Value::Float(number) => format!("Float({number})"),
        Value::Bool(flag) => format!("Bool({flag})"),
        other => format!("{:?}", std::mem::discriminant(other)),
    }
}

/// The private frame-entry clone bound to the named defineProperties helper.
/// Identity comes from the mapped diagnostic parameter slots and the actual
/// frame base: the clone has a private nine-parameter topology and the site
/// sentinel is passed only by the retargeted descriptor callsite, so no other
/// active frame and no capacity value can select it. The caller chain of the
/// selected frame is reported with the probe output for audit.
fn descriptor_frame(
    machine: &vm::Vm,
    frames: &[vm::vm::VmExecutionFrameSnapshot],
) -> Option<(u32, usize)> {
    let mut found: Option<(u32, usize)> = None;
    for (at, frame) in frames.iter().enumerate() {
        let Some(prototype) = frame.prototype_id else {
            continue;
        };
        let Some(proto) = machine
            .program()
            .callable_prototypes
            .get(prototype as usize)
        else {
            continue;
        };
        if proto.arity != 9 || proto.parameter_slots.len() != 9 {
            continue;
        }
        let params: Vec<Value> = proto
            .parameter_slots
            .iter()
            .map(|slot| machine.locals()[frame.local_base + usize::from(*slot)].clone())
            .collect();
        if int_param(&params, 8) != Some(object_observer::DIAG_DESCRIPTOR_SITE) {
            continue;
        }
        let innermost =
            found.is_none_or(|(_, previous)| frames[previous].local_base < frame.local_base);
        if innermost {
            found = Some((prototype, at));
        }
    }
    found
}

/// One real D01 VM in the default-off frame probe. The original source,
/// force-GC hook, 10M fuel and interpreter-only configuration are unchanged;
/// only the test-generated IR carries the diagnostic split. Machine state is
/// read immediately after `run`, before any expected-status assertion.
fn d01_probe(source: &str, stop: Option<(object_observer::D01Stop, i64)>) -> D01Probe {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_host_api_catalog(catalog.clone())
        .with_source_plugin(Box::leak(Box::new(object_observer::D01Observer {
            force_gc: true,
            stop,
        })));
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-object-observer")),
        options,
    )
    .unwrap_or_else(|error| panic!("diagnostic D01 must compile:\n{error:?}"));
    let mut machine = compiled.into_vm().expect("certified VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("owned arrays");
    let mut registry = HostFunctionRegistry::restricted();
    for module in [
        pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
        pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module(),
    ] {
        module
            .install_from_catalog(&mut registry, &catalog)
            .expect("exact catalog");
    }
    registry.allow_builtin("math::floor").expect("floor");
    registry
        .allow_builtin("bytes::from_utf8")
        .expect("bootstrap strings");
    registry.bind_vm_cached(&mut machine).expect("bind");
    machine.set_fuel(10_000_000);
    let status = machine.run();
    let fuel_after = machine.get_fuel();
    let jit_enabled = machine.jit_config().enabled;
    let aot_executions = machine.aot_exec_count();
    let jit_native_executions = machine.jit_native_exec_count();
    let reason = machine.last_yield_reason();
    let ip = machine.ip();
    let frames = machine.execution_frames();
    let phase_label = machine
        .debug_info()
        .and_then(|info| info.line_for_offset(ip));
    let phase = phase_label.and_then(|label| {
        object_observer::diag_phase(label).or_else(|| object_observer::diag_stop_phase(label))
    });
    let root_prototype = descriptor_frame(&machine, &frames).map(|(prototype, _)| prototype);
    let collector_prototype = active_diagnostic_prototype(&machine, &frames, 2, 2, &["init"]);
    let sample_phases: &[&str] = match stop {
        Some((object_observer::D01Stop::Bootstrap, _)) => &["typed-bootstrap-complete"],
        Some((object_observer::D01Stop::BootstrapCall, _)) => &["typed-bootstrap-call"],
        Some((object_observer::D01Stop::CollectEntry, _)) => &["init"],
        Some((object_observer::D01Stop::CollectCommit, _)) => &["counter-commit"],
        _ => &[],
    };
    let sample_prototype = active_diagnostic_prototype(&machine, &frames, 4, 4, sample_phases);
    let root_params =
        root_prototype.and_then(|prototype| frame_params(&machine, &frames, prototype));
    let collector_params =
        collector_prototype.and_then(|prototype| frame_params(&machine, &frames, prototype));
    let sample_params =
        sample_prototype.and_then(|prototype| frame_params(&machine, &frames, prototype));
    let frame_capacity = root_params
        .as_deref()
        .and_then(|params| int_param(params, 1));
    let cursor = root_params
        .as_deref()
        .and_then(|params| int_param(params, 2));
    let sample = root_params
        .as_deref()
        .and_then(|params| sample_from(params, 3))
        .or_else(|| {
            sample_params
                .as_deref()
                .and_then(|params| sample_from(params, 0))
        });
    let completed = collector_params
        .as_deref()
        .and_then(|params| int_param(params, 1));
    let rendered_frames = frames
        .iter()
        .map(|frame| format!("proto{:?}:base{}", frame.prototype_id, frame.local_base))
        .collect();
    let site = machine
        .program()
        .script_functions
        .iter()
        .enumerate()
        .find(|(_, function)| function.entry_ip as usize <= ip && ip < function.end_ip as usize)
        .map(|(index, function)| {
            let target = machine
                .program()
                .function_regions
                .iter()
                .find(|region| {
                    region.start_ip == function.entry_ip && region.end_ip == function.end_ip
                })
                .and_then(|region| region.prototype_id)
                .and_then(|id| machine.program().callable_prototypes.get(id as usize))
                .map(|prototype| format!("{:?}", prototype.target))
                .unwrap_or_else(|| "none".to_owned());
            (index, function.entry_ip, function.end_ip, target)
        });
    let debug_local_names: Vec<String> = machine
        .debug_info()
        .map(|info| {
            info.locals
                .iter()
                .map(|local| format!("{}@{}", local.name, local.index))
                .collect()
        })
        .unwrap_or_default();
    let named_frames = frames
        .iter()
        .map(|frame| {
            let prototype = frame
                .prototype_id
                .and_then(|id| machine.program().callable_prototypes.get(id as usize));
            let target = prototype
                .map(|prototype| format!("{:?}", prototype.target))
                .unwrap_or_else(|| "none".to_owned());
            let params: Vec<String> = prototype
                .map(|prototype| {
                    prototype
                        .parameter_slots
                        .iter()
                        .map(|slot| {
                            render_parameter(
                                &machine.locals()[frame.local_base + usize::from(*slot)],
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            format!(
                "base={} proto={:?} target={} arity={:?} params={:?}",
                frame.local_base,
                frame.prototype_id,
                target,
                prototype.map(|prototype| prototype.arity),
                params
            )
        })
        .collect();
    D01Probe {
        fuel_after,
        status,
        reason,
        ip,
        phase,
        phase_label,
        root_prototype,
        frame_capacity,
        cursor,
        sample,
        completed,
        frames: rendered_frames,
        site,
        named_frames,
        debug_local_names,
        jit_enabled,
        aot_executions,
        jit_native_executions,
    }
}

/// Compile-only structural probe for the descriptor-conversion helper. It
/// decodes the field names the production `ToPropertyDescriptor` body compares
/// against from the inlined UTF-16 constants of that same emitted IR, then
/// hands the IR through unchanged.
struct ToDescShapeProbe(std::sync::Mutex<(u16, usize, String)>);

impl vm::SourcePlugin for ToDescShapeProbe {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut descriptor = vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN);
        descriptor.id = vm::FrontendId::new("javascript-es5-object-observer");
        descriptor
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
        *self.0.lock().expect("shape cell") = object_observer::to_desc_constant_text(&ir);
        Ok(ir)
    }
}

#[test]
fn p7_d01_bootstrap_frame_fuel_probe_interpreter() {
    let full = d01_probe(D01_FORCED_SOURCE, None);
    eprintln!(
        "D01_PROBE full fuel_after={:?} status={:?} reason={:?} ip={} line={:?} phase={:?} root_proto={:?} capacity={:?} cursor={:?} sample={:?} frames={:?}",
        full.fuel_after,
        full.status,
        full.reason,
        full.ip,
        full.phase_label,
        full.phase,
        full.root_prototype,
        full.frame_capacity,
        full.cursor,
        full.sample,
        full.frames
    );
    assert!(
        matches!(full.status, Ok(VmStatus::Yielded)),
        "the unchanged 10M D01 run must yield, got {:?}",
        full.status
    );
    assert_eq!(
        full.reason,
        Some(vm::VmYieldReason::Fuel),
        "D01 exhaustion must be a fuel yield"
    );
    assert_eq!(
        full.fuel_after,
        Some(0),
        "the original 10M budget is exhausted, not recharged"
    );
    assert!(
        !full.jit_enabled,
        "diagnostic D01 run must stay interpreter-only"
    );
    assert_eq!(full.aot_executions, 0, "no AOT execution");
    assert_eq!(full.jit_native_executions, 0, "no native trace execution");
    // Report the actual failure site. A cheaper per-collection path can move
    // fuel exhaustion past the bounded descriptor frame; that frame
    // having returned before exhaustion says nothing about whether its earlier
    // conditional frame-entry prefix was reached. The prefix is therefore
    // measured by its own fresh force-32/10M run below and never inferred from
    // the presence or absence of this final active frame.
    if let Some(capacity) = full.frame_capacity {
        assert_eq!(
            capacity, D01_FRAME_CAPACITY,
            "the exhausting frame is the bounded descriptor frame"
        );
        let cursor = full
            .cursor
            .expect("the mutable clearing cursor must be live at exhaustion");
        assert!(
            (0..=D01_FRAME_CAPACITY).contains(&cursor),
            "clearing cursor {cursor} must be within the requested capacity"
        );
        let entry = full
            .sample
            .expect("frame-entry counters must be carried by the diagnostic parameters");
        assert!(
            entry.collections >= 0,
            "completed collections are nonnegative"
        );
        assert!(entry.live >= 0, "live nodes are nonnegative");
        assert!(
            entry.root_capacity >= 0,
            "the active root capacity before the frame push is nonnegative"
        );
        assert!(entry.pending >= 0, "pending nodes are nonnegative");
        eprintln!(
            "D01_FULL_SAMPLE (snapshot at frame entry, not current at exhaustion) collections={} live={} root_capacity={} pending={}",
            entry.collections, entry.live, entry.root_capacity, entry.pending
        );
        eprintln!(
            "D01_CURSOR actual_clearing_cursor_at_fuel_exhaustion={cursor} capacity={D01_FRAME_CAPACITY} remaining_fuel={:?}",
            full.fuel_after
        );
    } else {
        eprintln!(
            "D01_FULL_SITE frame_absent=true ip={} frames={:?} remaining_fuel={:?} (the bounded descriptor frame was not active at exhaustion; its frame-entry prefix is measured by its own fresh run below, not inferred from this final frame)",
            full.ip, full.frames, full.fuel_after
        );
    }

    // Terminal-frame provenance. Only public metadata is used here: the
    // owning script-function region of the final ip, and, for every live
    // frame, the callable prototype bound through the actual `frame.local_base`
    // and that prototype's `parameter_slots`.
    eprintln!(
        "D01_TERMINAL ip={} site={:?} named_frames={:?}",
        full.ip, full.site, full.named_frames
    );
    assert!(
        full.site.is_some(),
        "the terminal ip must fall inside a public script-function region"
    );
    assert!(
        !full.named_frames.is_empty(),
        "the terminal stop must expose at least one live frame"
    );
    // The conversion helper's own field names are decoded from the inlined
    // UTF-16 unit constants of the emitted `ToPropertyDescriptor` body, so the
    // named helper is identified structurally rather than by an instruction
    // size, a prototype id or the old ordinal.
    let shape_cell: &'static ToDescShapeProbe = Box::leak(Box::new(ToDescShapeProbe(
        std::sync::Mutex::new((0, 0, String::new())),
    )));
    let shape_catalog =
        pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    vm::compile_source_with_flavor_and_options_with_capture_certification(
        D01_FORCED_SOURCE,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-object-observer")),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(shape_cell)
            .with_host_api_catalog(shape_catalog),
    )
    .expect("descriptor-shape probe must compile");
    let (to_desc, statements, decoded) = shape_cell.0.lock().expect("shape cell").clone();
    eprintln!(
        "D01_TERMINAL_TODESC helper_index={to_desc} body_statements={statements} decoded_constants={decoded:?}"
    );
    // The conversion helper must compare exactly these inlined field names, in
    // the ES5 read order, and it is identified by that structure rather than by
    // an instruction size, a prototype id or the earlier ordinal.
    let mut previous = 0usize;
    for field in [
        "enumerable",
        "configurable",
        "value",
        "writable",
        "get",
        "set",
    ] {
        let at = decoded
            .find(field)
            .unwrap_or_else(|| panic!("ToPropertyDescriptor must compare {field:?}: {decoded:?}"));
        assert!(
            at >= previous,
            "ToPropertyDescriptor field order changed at {field:?}: {decoded:?}"
        );
        previous = at;
    }
    // Exact public-metadata gap for the conversion frame's temporaries.
    eprintln!(
        "D01_TERMINAL_LOCALS_GAP public_mapping=CallablePrototype.parameter_slots+frame.local_base; debug_locals={:?}; the literal-key/cursor/row/current-object temporaries are frame locals with no public physical-offset metadata, so their live values are neither read nor guessed and the earlier ordinal 46 is never reused as the current exhaustion collection",
        full.debug_local_names
    );
    eprintln!(
        "D01_TERMINAL_PROPOSAL one_minimum_test_only_private_outline: clone __compat_es5_to_property_descriptor into a private diagnostic helper whose literal-key, cursor, row and current-object temporaries become appended mapped Int parameters (the same shape install_d01_root_probe already uses for root_enter) and read them through parameter_slots; no production helper, host API, budget or checkpoint change"
    );

    let pre_bootstrap = d01_probe(
        D01_FORCED_SOURCE,
        Some((object_observer::D01Stop::BootstrapCall, 0)),
    );
    eprintln!(
        "D01_PROBE bootstrap_call fuel_after={:?} status={:?} reason={:?} ip={} line={:?} phase={:?} sample={:?} frames={:?}",
        pre_bootstrap.fuel_after,
        pre_bootstrap.status,
        pre_bootstrap.reason,
        pre_bootstrap.ip,
        pre_bootstrap.phase_label,
        pre_bootstrap.phase,
        pre_bootstrap.sample,
        pre_bootstrap.frames
    );
    assert!(
        matches!(&pre_bootstrap.status, Err(vm::VmError::HostError(message)) if message == "assertion failed"),
        "the pre-bootstrap boundary must stop on the existing assert builtin, got {:?}",
        pre_bootstrap.status
    );
    assert_eq!(
        pre_bootstrap.phase,
        Some("typed-bootstrap-call"),
        "the stop must be observed immediately before the typed bootstrap call"
    );
    assert!(
        !pre_bootstrap.jit_enabled,
        "the pre-bootstrap diagnostic run must stay interpreter-only"
    );
    assert_eq!(pre_bootstrap.aot_executions, 0, "no AOT execution");
    assert_eq!(
        pre_bootstrap.jit_native_executions, 0,
        "no native trace execution"
    );
    let pre_sample = pre_bootstrap
        .sample
        .expect("pre-bootstrap counters must be readable at the stop");
    assert!(pre_sample.collections >= 0);
    assert!(pre_sample.live >= 0);
    assert!(pre_sample.root_capacity >= 0);
    assert!(pre_sample.pending >= 0);
    assert_eq!(
        pre_sample.collections, 0,
        "no collection completes before the typed bootstrap call"
    );
    assert_eq!(
        pre_sample.live, 0,
        "no live heap node exists before the typed bootstrap call"
    );
    assert_eq!(
        pre_sample.root_capacity, 0,
        "no active root exists before the typed bootstrap call"
    );
    assert_eq!(
        pre_sample.pending, 0,
        "no pending node exists before the typed bootstrap call"
    );

    let bootstrap = d01_probe(
        D01_FORCED_SOURCE,
        Some((object_observer::D01Stop::Bootstrap, 0)),
    );
    eprintln!(
        "D01_PROBE bootstrap fuel_after={:?} status={:?} reason={:?} ip={} line={:?} phase={:?} sample={:?} frames={:?}",
        bootstrap.fuel_after,
        bootstrap.status,
        bootstrap.reason,
        bootstrap.ip,
        bootstrap.phase_label,
        bootstrap.phase,
        bootstrap.sample,
        bootstrap.frames
    );
    assert!(
        matches!(&bootstrap.status, Err(vm::VmError::HostError(message)) if message == "assertion failed"),
        "the bootstrap boundary must stop on the existing assert builtin, got {:?}",
        bootstrap.status
    );
    assert_eq!(
        bootstrap.phase,
        Some("typed-bootstrap-complete"),
        "the stop must be observed at the typed bootstrap completion boundary"
    );
    let bootstrap_sample = bootstrap
        .sample
        .expect("bootstrap counters must be readable at the stop");
    assert!(bootstrap_sample.collections >= 0);
    assert!(bootstrap_sample.live >= 0);
    assert!(bootstrap_sample.root_capacity >= 0);
    assert!(bootstrap_sample.pending >= 0);

    // The frame-entry prefix belongs to the earlier descriptor-frame
    // establishment, which this run reaches with the original source, force-32
    // hook, fresh 10M fuel and interpreter-only configuration. It is requested
    // unconditionally: the boundary exists whether or not any frame is still
    // active at the full run's exhaustion point.
    let frame = d01_probe(
        D01_FORCED_SOURCE,
        Some((object_observer::D01Stop::FrameEntry, 0)),
    );
    eprintln!(
        "D01_PROBE frame_entry fuel_after={:?} status={:?} reason={:?} ip={} line={:?} phase={:?} capacity={:?} cursor={:?} sample={:?} frames={:?}",
        frame.fuel_after,
        frame.status,
        frame.reason,
        frame.ip,
        frame.phase_label,
        frame.phase,
        frame.frame_capacity,
        frame.cursor,
        frame.sample,
        frame.frames
    );
    assert!(
        matches!(&frame.status, Err(vm::VmError::HostError(message)) if message == "assertion failed"),
        "the frame-entry boundary must stop on the existing assert builtin (a deliberate non-fuel stop), got {:?}",
        frame.status
    );
    assert!(
        !frame.jit_enabled,
        "the frame-entry diagnostic run must stay interpreter-only"
    );
    assert_eq!(frame.aot_executions, 0, "no AOT execution");
    assert_eq!(frame.jit_native_executions, 0, "no native trace execution");
    assert_eq!(
        frame.phase,
        Some("frame-entry"),
        "the stop must be observed at the bounded descriptor frame entry"
    );
    assert_eq!(
        frame.frame_capacity,
        Some(D01_FRAME_CAPACITY),
        "the stop must be inside the bounded descriptor frame"
    );
    assert_eq!(
        frame.cursor,
        Some(0),
        "the cursor parameter is still at its zero seed at frame entry"
    );
    assert!(
        frame.root_prototype.is_some(),
        "the frame-entry stop must be read from the mapped diagnostic prototype"
    );
    let fuel_frame_entry = frame
        .fuel_after
        .expect("the frame-entry stop must report the remaining VM execution budget");
    assert!(
        fuel_frame_entry > 0 && fuel_frame_entry < 10_000_000,
        "the frame entry must be reached with remaining budget, got {fuel_frame_entry}"
    );

    // The ordinal is derived from the independent frame-entry sample above:
    // `collections` is the completed count at that stop, so the most recent
    // collector entered with ordinal `collections - 1`. This never trusts the
    // final active frame and never uses a blind constant.
    let frame_sample = frame
        .sample
        .expect("frame-entry counters must be carried by the mapped diagnostic parameters");
    assert!(
        frame_sample.collections >= 0
            && frame_sample.live >= 0
            && frame_sample.root_capacity >= 0
            && frame_sample.pending >= 0,
        "frame-entry counters are nonnegative: {frame_sample:?}"
    );
    assert!(
        frame_sample.collections > 0,
        "the frame entry must follow at least one completed collection"
    );
    let completed = frame_sample
        .collections
        .checked_sub(1)
        .expect("frame entry follows at least one completed collection");
    eprintln!(
        "D01_FRAME_ENTRY_ACTUAL phase=frame-entry status=HostError(assertion failed) remaining_fuel={fuel_frame_entry} collections={} live={} root_capacity={} pending={} capacity={D01_FRAME_CAPACITY} cursor=0",
        frame_sample.collections,
        frame_sample.live,
        frame_sample.root_capacity,
        frame_sample.pending
    );
    eprintln!(
        "D01_FRAME_ENTRY_SAMPLE (independent of the final active frame; snapshot at the bounded descriptor frame entry) collections={} live={} root_capacity={} pending={}",
        frame_sample.collections,
        frame_sample.live,
        frame_sample.root_capacity,
        frame_sample.pending
    );
    eprintln!(
        "D01_FRAME_ENTRY_ORDINAL derived_from=frame_entry_sample.collections-1 completed_at_entry={completed}"
    );
    let collect_entry = {
        let collect_entry = d01_probe(
            D01_FORCED_SOURCE,
            Some((object_observer::D01Stop::CollectEntry, completed)),
        );
        eprintln!(
            "D01_PROBE collect_entry expected_completed={completed} fuel_after={:?} status={:?} reason={:?} ip={} line={:?} phase={:?} completed={:?} sample={:?} frames={:?}",
            collect_entry.fuel_after,
            collect_entry.status,
            collect_entry.reason,
            collect_entry.ip,
            collect_entry.phase_label,
            collect_entry.phase,
            collect_entry.completed,
            collect_entry.sample,
            collect_entry.frames
        );
        assert!(
            matches!(&collect_entry.status, Err(vm::VmError::HostError(message)) if message == "assertion failed"),
            "the collector-entry boundary must stop on the existing assert builtin, got {:?}",
            collect_entry.status
        );
        assert_eq!(
            collect_entry.phase,
            Some("init"),
            "the stop must be observed at collector entry"
        );
        assert_eq!(
            collect_entry.completed,
            Some(completed),
            "the completed-at-entry parameter must match the observed ordinal"
        );
        collect_entry
    };

    let collect_commit = {
        let collect_commit = d01_probe(
            D01_FORCED_SOURCE,
            Some((object_observer::D01Stop::CollectCommit, completed)),
        );
        eprintln!(
            "D01_PROBE collect_commit expected_completed={completed} fuel_after={:?} status={:?} reason={:?} ip={} line={:?} phase={:?} completed={:?} sample={:?} frames={:?}",
            collect_commit.fuel_after,
            collect_commit.status,
            collect_commit.reason,
            collect_commit.ip,
            collect_commit.phase_label,
            collect_commit.phase,
            collect_commit.completed,
            collect_commit.sample,
            collect_commit.frames
        );
        assert!(
            matches!(&collect_commit.status, Err(vm::VmError::HostError(message)) if message == "assertion failed"),
            "the counter-commit boundary must stop on the existing assert builtin, got {:?}",
            collect_commit.status
        );
        assert_eq!(
            collect_commit.phase,
            Some("counter-commit"),
            "the stop must be observed at the counter-commit boundary"
        );
        assert_eq!(
            collect_commit.completed,
            Some(completed),
            "the completed-at-entry parameter must stay exact at the commit stop"
        );
        let commit_sample = collect_commit
            .sample
            .expect("commit counters must be refreshed before the stop");
        assert_eq!(
            commit_sample.collections, completed,
            "the commit boundary is before the collection counter increments"
        );
        collect_commit
    };

    let fuel_bootstrap = bootstrap.fuel_after.expect("bootstrap stop fuel");
    let fuel_pre = pre_bootstrap.fuel_after.expect("pre-bootstrap stop fuel");
    assert!(fuel_pre > 0 && fuel_pre < 10_000_000);
    assert!(
        fuel_pre > fuel_bootstrap,
        "the pre-bootstrap boundary precedes bootstrap completion: pre={fuel_pre} bootstrap={fuel_bootstrap}"
    );
    assert!(
        pre_sample.collections <= bootstrap_sample.collections,
        "completed collections cannot decrease across bootstrap: pre={} bootstrap={}",
        pre_sample.collections,
        bootstrap_sample.collections
    );
    assert!(fuel_bootstrap > 0 && fuel_bootstrap < 10_000_000);
    eprintln!(
        "D01_PREFIX phase=typed-bootstrap-call remaining_fuel={fuel_pre} consumed_fuel={}",
        10_000_000 - fuel_pre
    );
    eprintln!(
        "D01_PREFIX phase=typed-bootstrap-complete remaining_fuel={fuel_bootstrap} consumed_fuel={}",
        10_000_000 - fuel_bootstrap
    );
    eprintln!(
        "D01_PREFIX_COST from=typed-bootstrap-call to=typed-bootstrap-complete inclusive_instrumented_fuel={} (typed bootstrap interval plus mode-dependent wrapper/sample/assert/compiler overhead; not per-GC cost)",
        fuel_pre - fuel_bootstrap
    );
    eprintln!(
        "D01_PRE_BOOTSTRAP_SAMPLE (immediately before the unique typed bootstrap call) collections={} live={} root_capacity={} pending={}",
        pre_sample.collections, pre_sample.live, pre_sample.root_capacity, pre_sample.pending
    );
    let fuel_entry = collect_entry.fuel_after.expect("collector-entry stop fuel");
    let fuel_commit = collect_commit.fuel_after.expect("commit stop fuel");
    assert!(
        fuel_bootstrap >= fuel_frame_entry,
        "the frame entry follows bootstrap completion: bootstrap={fuel_bootstrap} frame={fuel_frame_entry}"
    );
    assert!(
        fuel_entry >= fuel_commit,
        "the commit boundary follows collector entry: entry={fuel_entry} commit={fuel_commit}"
    );
    assert!(
        fuel_commit >= fuel_frame_entry,
        "the later descriptor-frame entry follows the last completed collection: commit={fuel_commit} frame={fuel_frame_entry}"
    );
    let table = [
        (
            "frame-entry-of-independent-descriptor-frame",
            fuel_frame_entry,
        ),
        ("last-completed-collector-entry", fuel_entry),
        ("last-completed-collector-commit", fuel_commit),
    ];
    for (phase, remaining) in table {
        eprintln!(
            "D01_PREFIX phase={phase} remaining_fuel={remaining} consumed_fuel={}",
            10_000_000 - remaining
        );
    }
    eprintln!(
        "D01_PREFIX_COST from=typed-bootstrap-complete to=frame-entry inclusive_instrumented_fuel={} (includes diagnostic wrapper/observe/assert overhead)",
        fuel_bootstrap - fuel_frame_entry
    );
    eprintln!(
        "D01_PREFIX_COST from=last-completed-collector-entry to=last-completed-collector-commit inclusive_instrumented_fuel={} (includes diagnostic observe/assert overhead)",
        fuel_entry - fuel_commit
    );
    let collect_entry_sample = collect_entry
        .sample
        .expect("entry counters must be carried by the diagnostic parameters");
    let commit_sample = collect_commit
        .sample
        .expect("commit counters must be refreshed before the stop");
    eprintln!(
        "D01_COLLECTOR_ENTRY_SAMPLE collections={} live={} root_capacity={} pending={}",
        collect_entry_sample.collections,
        collect_entry_sample.live,
        collect_entry_sample.root_capacity,
        collect_entry_sample.pending
    );
    eprintln!(
        "D01_COLLECTOR_COMMIT_SAMPLE (counter-commit boundary, before interval reset and collection increment) collections={} live={} root_capacity={} pending={}",
        commit_sample.collections,
        commit_sample.live,
        commit_sample.root_capacity,
        commit_sample.pending
    );
    eprintln!(
        "D01_PIPELINES actual=6 full=1 typed-bootstrap-call=1 typed-bootstrap-complete=1 frame-entry=1 last-completed-collector-entry=1 last-completed-collector-commit=1"
    );
}

/// One live ROOTS state read through the real mapped diagnostic parameters.
#[derive(Debug)]
struct RootsProbe {
    fuel_after: Option<u64>,
    status: Result<VmStatus, vm::VmError>,
    reason: Option<vm::VmYieldReason>,
    ip: usize,
    phase: Option<&'static str>,
    phase_label: Option<u32>,
    helper: Option<u32>,
    frame_f: Option<i64>,
    cursor_e: Option<i64>,
    capacity: Option<i64>,
    completed: Option<i64>,
    scanned: Option<i64>,
    nonempty: Option<i64>,
    mode: Option<i64>,
    target: Option<i64>,
    frames: Vec<String>,
    named_frames: Vec<String>,
    site: Option<(usize, u32, u32, String)>,
    code_len: usize,
    protos: Vec<(u8, usize)>,
    jit_enabled: bool,
    aot_executions: u64,
    jit_native_executions: u64,
}

/// One real D01 VM in the matched-layout ROOTS diagnostic. Source, force-GC
/// hook, 10M fuel, heap/root sizes and interpreter-only configuration match
/// the accepted D01 vehicle; only the mode/target seeds fed through the same
/// wrapper call shape select full versus prefix behavior.
fn roots_probe(source: &str, stop: (object_observer::D01Stop, i64)) -> RootsProbe {
    let (mode, target) = match stop {
        (object_observer::D01Stop::RootsFull, _) => (0, 0),
        (object_observer::D01Stop::RootsEntry, at) => (1, at),
        (object_observer::D01Stop::RootsCursor8, at) => (2, at),
        _ => panic!("roots vehicle takes only Roots* stops"),
    };
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_host_api_catalog(catalog.clone())
        .with_source_plugin(Box::leak(Box::new(object_observer::D01Observer {
            force_gc: true,
            stop: Some(stop),
        })));
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-object-observer")),
        options,
    )
    .unwrap_or_else(|error| panic!("matched-layout ROOTS must compile:\n{error:?}"));
    let mut machine = compiled.into_vm().expect("certified VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("owned arrays");
    let mut registry = HostFunctionRegistry::restricted();
    for module in [
        pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
        pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module(),
    ] {
        module
            .install_from_catalog(&mut registry, &catalog)
            .expect("exact catalog");
    }
    registry.allow_builtin("math::floor").expect("floor");
    registry
        .allow_builtin("bytes::from_utf8")
        .expect("bootstrap strings");
    registry.bind_vm_cached(&mut machine).expect("bind");
    machine.set_fuel(10_000_000);
    let status = machine.run();
    let fuel_after = machine.get_fuel();
    let jit_enabled = machine.jit_config().enabled;
    let aot_executions = machine.aot_exec_count();
    let jit_native_executions = machine.jit_native_exec_count();
    let reason = machine.last_yield_reason();
    let ip = machine.ip();
    let frames = machine.execution_frames();
    let phase_label = machine
        .debug_info()
        .and_then(|info| info.line_for_offset(ip));
    let phase = phase_label.and_then(|label| {
        object_observer::diag_phase(label).or_else(|| object_observer::diag_stop_phase(label))
    });
    // The matched-layout helper is the only active nine-parameter frame whose
    // live mode/target parameters match this run's seeds. No prototype id,
    // capacity literal or historical ordinal selects it.
    let mut helper: Option<u32> = None;
    for frame in frames.iter() {
        let Some(prototype) = frame.prototype_id else {
            continue;
        };
        let Some(proto) = machine
            .program()
            .callable_prototypes
            .get(prototype as usize)
        else {
            continue;
        };
        if proto.arity != 9 || proto.parameter_slots.len() != 9 {
            continue;
        }
        let params: Vec<Value> = proto
            .parameter_slots
            .iter()
            .map(|slot| machine.locals()[frame.local_base + usize::from(*slot)].clone())
            .collect();
        if int_param(&params, 7) == Some(mode) && int_param(&params, 8) == Some(target) {
            if helper.is_some() {
                helper = None;
                break;
            }
            helper = Some(prototype);
        }
    }
    let params = helper.and_then(|prototype| frame_params(&machine, &frames, prototype));
    let rendered_frames = frames
        .iter()
        .map(|frame| format!("proto{:?}:base{}", frame.prototype_id, frame.local_base))
        .collect();
    let site = machine
        .program()
        .script_functions
        .iter()
        .enumerate()
        .find(|(_, function)| function.entry_ip as usize <= ip && ip < function.end_ip as usize)
        .map(|(index, function)| {
            let target = machine
                .program()
                .function_regions
                .iter()
                .find(|region| {
                    region.start_ip == function.entry_ip && region.end_ip == function.end_ip
                })
                .and_then(|region| region.prototype_id)
                .and_then(|id| machine.program().callable_prototypes.get(id as usize))
                .map(|prototype| format!("{:?}", prototype.target))
                .unwrap_or_else(|| "none".to_owned());
            (index, function.entry_ip, function.end_ip, target)
        });
    let named_frames = frames
        .iter()
        .map(|frame| {
            let prototype = frame
                .prototype_id
                .and_then(|id| machine.program().callable_prototypes.get(id as usize));
            let target = prototype
                .map(|prototype| format!("{:?}", prototype.target))
                .unwrap_or_else(|| "none".to_owned());
            let params: Vec<String> = prototype
                .map(|prototype| {
                    prototype
                        .parameter_slots
                        .iter()
                        .map(|slot| {
                            render_parameter(
                                &machine.locals()[frame.local_base + usize::from(*slot)],
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            format!(
                "base={} proto={:?} target={} arity={:?} params={:?}",
                frame.local_base,
                frame.prototype_id,
                target,
                prototype.map(|prototype| prototype.arity),
                params
            )
        })
        .collect();
    RootsProbe {
        fuel_after,
        status,
        reason,
        ip,
        phase,
        phase_label,
        helper,
        frame_f: params.as_deref().and_then(|params| int_param(params, 2)),
        cursor_e: params.as_deref().and_then(|params| int_param(params, 3)),
        capacity: params.as_deref().and_then(|params| int_param(params, 4)),
        completed: params.as_deref().and_then(|params| int_param(params, 1)),
        scanned: params.as_deref().and_then(|params| int_param(params, 5)),
        nonempty: params.as_deref().and_then(|params| int_param(params, 6)),
        mode: params.as_deref().and_then(|params| int_param(params, 7)),
        target: params.as_deref().and_then(|params| int_param(params, 8)),
        frames: rendered_frames,
        named_frames,
        site,
        code_len: machine.program().code.len(),
        protos: machine
            .program()
            .callable_prototypes
            .iter()
            .map(|prototype| (prototype.arity, prototype.parameter_slots.len()))
            .collect(),
        jit_enabled,
        aot_executions,
        jit_native_executions,
    }
}

/// Matched-layout D01 ROOTS localization: one full run plus the ROOTS-entry
/// and eight-completed-slot prefixes, all three from this single focused test
/// so the target ordinal flows in memory from the fresh full run. Every
/// vehicle shares the same source, fuel, force-GC hook, interpreter-only
/// configuration and diagnostic IR; only the runtime mode/target seeds fed
/// through the same wrapper call shape differ.
#[test]
fn p7_d01_roots_matched_layout_interpreter() {
    object_observer::clear_roots_ledger();
    let full = roots_probe(D01_FORCED_SOURCE, (object_observer::D01Stop::RootsFull, 0));
    eprintln!(
        "D01_ROOTS_FULL status={:?} reason={:?} fuel_after={:?} ip={} line={:?} phase={:?} helper={:?} frame={:?} cursor={:?} capacity={:?} completed={:?} scanned={:?} nonempty={:?} mode={:?} target={:?} site={:?} frames={:?} named_frames={:?} code_len={} jit={} aot={} native={}",
        full.status,
        full.reason,
        full.fuel_after,
        full.ip,
        full.phase_label,
        full.phase,
        full.helper,
        full.frame_f,
        full.cursor_e,
        full.capacity,
        full.completed,
        full.scanned,
        full.nonempty,
        full.mode,
        full.target,
        full.site,
        full.frames,
        full.named_frames,
        full.code_len,
        full.jit_enabled,
        full.aot_executions,
        full.jit_native_executions
    );
    match &full.status {
        Ok(VmStatus::Halted) => {
            eprintln!(
                "D01_ROOTS_FULL_TERMINAL status=Halted ip={} site={:?} frames={:?} (guest termination is reported as termination, never as a JS true)",
                full.ip, full.site, full.frames
            );
            return;
        }
        Ok(VmStatus::Yielded) => {}
        other => panic!("unexpected guest end before ROOTS readout: {other:?}"),
    }
    assert_eq!(
        full.reason,
        Some(vm::VmYieldReason::Fuel),
        "the full ROOTS run must exhaust fuel"
    );
    assert_eq!(
        full.fuel_after,
        Some(0),
        "the original 10M budget is exhausted, not recharged"
    );
    assert!(
        !full.jit_enabled,
        "the full ROOTS run must stay interpreter-only"
    );
    assert_eq!(full.aot_executions, 0, "no AOT execution");
    assert_eq!(full.jit_native_executions, 0, "no native trace execution");
    let Some(helper) = full.helper else {
        eprintln!(
            "D01_ROOTS_FULL_UNKNOWN no active nine-parameter matched-layout helper at fuel exhaustion (phase={:?} line={:?}); target ordinal is unknown so no prefix vehicle runs",
            full.phase, full.phase_label
        );
        return;
    };
    assert_eq!(full.mode, Some(0), "the full run carries mode 0");
    assert_eq!(full.target, Some(0), "the full run carries target 0");
    let target = full
        .completed
        .expect("the completed-at-entry ordinal must be carried live at fuel stop");
    assert!(target >= 0, "the target ordinal is nonnegative");
    eprintln!(
        "D01_ROOTS_FULL_HELPER helper={helper} phase={:?} line={:?} frame={:?} cursor={:?} capacity={:?} completed={target} scanned={:?} nonempty={:?} site={:?}",
        full.phase,
        full.phase_label,
        full.frame_f,
        full.cursor_e,
        full.capacity,
        full.scanned,
        full.nonempty,
        full.site
    );
    let entry_label = object_observer::DIAG_STOP_BASE
        + (object_observer::DIAG_PHASE_ROOTS - object_observer::DIAG_PHASE_INIT);
    let entry = roots_probe(
        D01_FORCED_SOURCE,
        (object_observer::D01Stop::RootsEntry, target),
    );
    eprintln!(
        "D01_ROOTS_ENTRY status={:?} reason={:?} fuel_after={:?} ip={} line={:?} phase={:?} helper={:?} frame={:?} cursor={:?} capacity={:?} completed={:?} scanned={:?} nonempty={:?} mode={:?} target={:?} site={:?} frames={:?} code_len={}",
        entry.status,
        entry.reason,
        entry.fuel_after,
        entry.ip,
        entry.phase_label,
        entry.phase,
        entry.helper,
        entry.frame_f,
        entry.cursor_e,
        entry.capacity,
        entry.completed,
        entry.scanned,
        entry.nonempty,
        entry.mode,
        entry.target,
        entry.site,
        entry.frames,
        entry.code_len
    );
    match &entry.status {
        Err(vm::VmError::HostError(message)) if message == "assertion failed" => {}
        Ok(VmStatus::Yielded) => {
            eprintln!(
                "D01_ROOTS_ENTRY_UNREACHED target={target} fuel_after={:?} (the full run stopped before this target's root scan; the endpoint is reported, not moved)",
                entry.fuel_after
            );
            return;
        }
        other => panic!("the ROOTS-entry prefix must stop on its deliberate assert, got {other:?}"),
    }
    assert_eq!(
        entry.phase,
        Some("root-scan"),
        "the entry stop must attribute root-scan"
    );
    assert_eq!(
        entry.phase_label,
        Some(entry_label),
        "the entry stop must carry its own requested stop label"
    );
    assert_eq!(
        entry.completed,
        Some(target),
        "the entry stop must fire in the target collection"
    );
    assert_eq!(entry.mode, Some(1), "the entry run carries mode 1");
    assert_eq!(
        entry.target,
        Some(target),
        "the entry run carries the fresh target ordinal"
    );
    assert!(
        entry.helper.is_some(),
        "the entry stop must be read from the mapped diagnostic helper"
    );
    assert_eq!(
        entry.cursor_e,
        Some(0),
        "the cursor is still zero before the first slot of the target collection"
    );
    let entry_capacity = entry
        .capacity
        .expect("the capacity must be decoded before the first slot");
    assert!(
        entry_capacity > 0,
        "the reported capacity must be positive: {entry_capacity}"
    );
    let entry_frame = entry
        .frame_f
        .expect("the frame pointer must be valid at the entry stop");
    eprintln!(
        "D01_ROOTS_ENTRY_ACTUAL target={target} frame={entry_frame} capacity={entry_capacity} cursor=0 remaining_fuel={:?}",
        entry.fuel_after
    );
    let cursor = roots_probe(
        D01_FORCED_SOURCE,
        (object_observer::D01Stop::RootsCursor8, target),
    );
    eprintln!(
        "D01_ROOTS_CURSOR status={:?} reason={:?} fuel_after={:?} ip={} line={:?} phase={:?} helper={:?} frame={:?} cursor={:?} capacity={:?} completed={:?} scanned={:?} nonempty={:?} mode={:?} target={:?} site={:?} frames={:?} code_len={}",
        cursor.status,
        cursor.reason,
        cursor.fuel_after,
        cursor.ip,
        cursor.phase_label,
        cursor.phase,
        cursor.helper,
        cursor.frame_f,
        cursor.cursor_e,
        cursor.capacity,
        cursor.completed,
        cursor.scanned,
        cursor.nonempty,
        cursor.mode,
        cursor.target,
        cursor.site,
        cursor.frames,
        cursor.code_len
    );
    match &cursor.status {
        Err(vm::VmError::HostError(message)) if message == "assertion failed" => {}
        Ok(VmStatus::Yielded) => {
            eprintln!(
                "D01_ROOTS_CURSOR_UNREACHED target={target} fuel_after={:?} (fewer than eight slots complete after the full stop; the endpoint is reported unreached, the eight-slot bound is not moved)",
                cursor.fuel_after
            );
            return;
        }
        other => {
            panic!("the eight-slot cursor prefix must stop on its deliberate assert, got {other:?}")
        }
    }
    assert_eq!(
        cursor.phase,
        Some("root-scan"),
        "the cursor stop must attribute root-scan"
    );
    assert_eq!(
        cursor.phase_label,
        Some(object_observer::DIAG_STOP_ROOTS_CURSOR),
        "the cursor stop must carry its own requested stop label"
    );
    assert_eq!(
        cursor.completed,
        Some(target),
        "the cursor stop must fire in the target collection"
    );
    assert_eq!(cursor.mode, Some(2), "the cursor run carries mode 2");
    assert_eq!(
        cursor.target,
        Some(target),
        "the cursor run carries the fresh target ordinal"
    );
    assert!(
        cursor.helper.is_some(),
        "the cursor stop must be read from the mapped diagnostic helper"
    );
    assert_eq!(
        cursor.scanned,
        Some(object_observer::ROOTS_CURSOR_SLOTS),
        "exactly eight slots complete across the actual frame chain"
    );
    let cursor_e = cursor
        .cursor_e
        .expect("the cursor must be live at the cursor stop");
    let cursor_capacity = cursor
        .capacity
        .expect("the capacity must be live at the cursor stop");
    assert!(
        cursor_capacity > 0,
        "the current frame capacity is positive: {cursor_capacity}"
    );
    assert!(
        (0..=cursor_capacity).contains(&cursor_e),
        "the cursor stays within its frame: cursor={cursor_e} capacity={cursor_capacity}"
    );
    let cursor_nonempty = cursor
        .nonempty
        .expect("the nonempty counter must be carried live at the cursor stop");
    assert!(
        (0..=object_observer::ROOTS_CURSOR_SLOTS).contains(&cursor_nonempty),
        "nonempty completed slots are within the eight completed: {cursor_nonempty}"
    );
    let blank = object_observer::ROOTS_CURSOR_SLOTS - cursor_nonempty;
    eprintln!(
        "D01_ROOTS_CURSOR_ACTUAL target={target} frame={:?} cursor={:?} capacity={:?} scanned=8 nonempty={cursor_nonempty} blank={blank} remaining_fuel={:?}",
        cursor.frame_f, cursor.cursor_e, cursor.capacity, cursor.fuel_after
    );
    let entry_fuel = entry.fuel_after.expect("entry remaining fuel");
    let cursor_fuel = cursor.fuel_after.expect("cursor remaining fuel");
    eprintln!(
        "D01_ROOTS_PREFIX_COST entry_remaining={entry_fuel} cursor_remaining={cursor_fuel} inclusive_instrumented={} (matched-layout instrumented cost between the two prefix stops, not original per-frame performance)",
        entry_fuel.saturating_sub(cursor_fuel)
    );
    let ledger = object_observer::roots_ledger();
    assert_eq!(
        entry.protos, full.protos,
        "emitted prototype topology stays equal (full vs entry)"
    );
    assert_eq!(
        cursor.protos, full.protos,
        "emitted prototype topology stays equal (full vs cursor)"
    );
    assert_eq!(
        ledger.len(),
        3,
        "exactly the three matched-layout pipelines compile: {ledger:?}"
    );
    assert_eq!(
        ledger
            .iter()
            .map(|entry| (entry.mode, entry.target))
            .collect::<Vec<_>>(),
        vec![(0, 0), (1, target), (2, target)],
        "the seeds are the only expected control delta"
    );
    for entry in &ledger {
        assert_eq!(entry.diag_arity, 9, "nine mapped parameters");
        assert_eq!(entry.diag_params, 9, "nine parameter slots");
        assert_eq!(
            entry.diag_shape, ledger[0].diag_shape,
            "the diagnostic helper body keeps one shape across vehicles"
        );
        assert_eq!(
            entry.locals, ledger[0].locals,
            "local counts stay equal across vehicles"
        );
        assert_eq!(
            entry.functions, ledger[0].functions,
            "helper counts stay equal across vehicles"
        );
        assert_eq!(
            entry.protos, ledger[0].protos,
            "prototype topology stays equal across vehicles"
        );
    }
    eprintln!(
        "D01_ROOTS_LAYOUT_SHAPES locals={} functions={} protos={:?} seeds={:?} code_lens=[{},{},{}] (shapes/topology equal; seed constants and emitted code lengths are reported, never claimed byte-equal)",
        ledger[0].locals,
        ledger[0].functions,
        ledger[0].protos,
        ledger
            .iter()
            .map(|entry| entry.wrapper_seeds)
            .collect::<Vec<_>>(),
        full.code_len,
        entry.code_len,
        cursor.code_len
    );
    eprintln!("D01_PIPELINES actual=3 full=1 roots-entry=1 roots-cursor8=1 target={target}");
}

/// One descriptor-frame capacity case: every case stops at the descriptor
/// frame entry through the mapped flag parameter (never through a capacity
/// literal), so the stop itself cannot select any other frame, and reports the
/// capacity this helper actually requested. The expectation is the algorithm's
/// `8 + 4 * validated props rows`, which keeps this a real capacity assertion
/// independent of any fuel-prefix interpretation.
fn check_define_properties_frame(id: &str, source: &str, expected: i64) {
    let probe = d01_probe(source, Some((object_observer::D01Stop::FrameEntry, 0)));
    eprintln!(
        "DEFINE_PROPERTIES_FRAME id={id} expected_capacity={expected} actual_capacity={:?} remaining_fuel={:?} status={:?} phase={:?} cursor={:?} sample={:?} frames={:?}",
        probe.frame_capacity,
        probe.fuel_after,
        probe.status,
        probe.phase,
        probe.cursor,
        probe.sample,
        probe.frames
    );
    assert!(
        matches!(&probe.status, Err(vm::VmError::HostError(message)) if message == "assertion failed"),
        "{id}: the descriptor frame entry must stop on the existing assert builtin, got {:?}",
        probe.status
    );
    assert_eq!(
        probe.phase,
        Some("frame-entry"),
        "{id}: the stop must be observed at the descriptor frame entry"
    );
    assert_eq!(
        probe.frame_capacity,
        Some(expected),
        "{id}: the descriptor frame must reserve 8 + 4 * validated props rows"
    );
    assert_eq!(
        probe.cursor,
        Some(0),
        "{id}: the clearing cursor keeps its unchanged zero seed"
    );
    let sample = probe
        .sample
        .expect("entry counters must ride the mapped diagnostic parameters");
    assert!(
        sample.collections >= 0
            && sample.live >= 0
            && sample.root_capacity >= 0
            && sample.pending >= 0,
        "{id}: entry counters are nonnegative at the stop: {sample:?}"
    );
    assert!(
        probe.fuel_after.is_some_and(|fuel| fuel > 0),
        "{id}: the descriptor frame entry must be reached with remaining budget"
    );
    assert!(!probe.jit_enabled, "{id}: interpreter only");
    assert_eq!(probe.aot_executions, 0, "{id}: no AOT execution");
    assert_eq!(probe.jit_native_executions, 0, "{id}: no native trace");
}

#[test]
fn p7_define_properties_bounded_frame_capacity_interpreter() {
    for (id, source, expected) in [
        ("n0", "var o={};Object.defineProperties(o,{});", 8),
        (
            "n1",
            "var o={};Object.defineProperties(o,{a:{value:1}});",
            12,
        ),
        (
            "n2",
            "var o={};Object.defineProperties(o,{a:{value:1},b:{value:2}});",
            16,
        ),
        // The inner `enumerable:false` above is a target attribute of `a`'s own
        // descriptor; the outer row `a` is still enumerable in `props`. This
        // case therefore proves the reserved raw row count (n = 1) and nothing
        // about a non-enumerable outer row.
        (
            "inner-flag-n1",
            "var o={};Object.defineProperties(o,{a:{value:1,enumerable:false}});",
            12,
        ),
        (
            "function-target-n1",
            "var f=function(){};Object.defineProperties(f,{a:{value:1}});",
            12,
        ),
        (
            "invalid-props-tag",
            "var o={};try{Object.defineProperties(o,1);}catch(e){e;};",
            8,
        ),
    ] {
        check_define_properties_frame(id, source, expected);
    }
}

/// Reservation-edge rows that the earlier table did not cover.
///
/// * a genuinely non-enumerable outer props row: it exists in the props header
///   (raw count n = 1) but is excluded from the retained key snapshot (m = 0),
///   and its value is the primitive `1`, so any accidental
///   `Get`/`ToPropertyDescriptor` of that row during the prepass would throw;
/// * a Function used as the *props* object: its own rows are non-enumerable, so
///   the retained snapshot is empty while the raw header still counts them
///   (measured n = 1) and the Function still passes the props-only tag gate;
/// * an Array target with one props row: the descriptor frame is entered, so
///   there is no early target-kind/header rejection before the reservation.
#[test]
fn p7_define_properties_bounded_frame_reservation_edges_interpreter() {
    for (id, source, expected) in [
        (
            "outer-nonenum-row-n1",
            "var o={};var p={};Object.defineProperty(p,\"a\",{value:1,enumerable:false});Object.defineProperties(o,p);",
            12,
        ),
        (
            "function-props",
            "var f=function(a,b){};var o={};Object.defineProperties(o,f);",
            12,
        ),
        (
            "array-target-n1",
            "var a=[];Object.defineProperties(a,{a:{value:1}});",
            12,
        ),
    ] {
        check_define_properties_frame(id, source, expected);
    }
}

/// Public behaviour of the bounded frame: a regular Object, explicit
/// enumerable flags, a Function target, an Array target with empty props
/// (unchanged, never repaired into Array-props semantics), an Array target
/// whose only props row is an outer non-enumerable primitive row (a no-op that
/// must return the same array with `length` 0) and the Object.create
/// properties form all keep their observable results.
#[test]
fn p7_define_properties_bounded_frame_behavior_interpreter() {
    let runner = Runner { native: false };
    for (id, source) in [
        (
            "object-n2",
            "var o={};var r=Object.defineProperties(o,{a:{value:1},b:{value:2}});r===o&&o.a===1&&o.b===2;",
        ),
        (
            "nonenum-flags",
            "var t={};var n=Object.defineProperties(t,{a:{value:1,enumerable:false}});n===t&&t.a===1&&Object.getOwnPropertyDescriptor(t,\"a\").enumerable===false;",
        ),
        // Outer props row that is itself non-enumerable. ES5 excludes it from
        // the retained key snapshot, so nothing is defined on the target, and
        // the row value is the primitive `1`: an accidental
        // Get/ToPropertyDescriptor of that row during the props prepass would
        // throw instead of passing. The reservation still counts the raw row,
        // which the capacity probe asserts separately.
        (
            "outer-nonenum-row",
            "var o={};var p={};Object.defineProperty(p,\"a\",{value:1,enumerable:false});Object.defineProperties(o,p);!(\"a\" in o);",
        ),
        // Function used as the props object: every own row of a user function
        // is non-enumerable (`Object.keys` is empty), so nothing is copied to
        // the target, while the raw header rows are still reserved (capacity
        // probe). This proves the Function props gate plus the raw-count
        // reservation without inventing copied semantics.
        (
            "function-props-nonenumerable",
            "var f=function(a,b){};var o={};Object.defineProperties(o,f);!( \"length\" in o)&&Object.keys(f).length===0&&f.length===2;",
        ),
        (
            "function-target",
            "var f=function(){};Object.defineProperties(f,{a:{value:1}});f.a===1;",
        ),
        (
            "array-target-empty-props",
            "var a=[];var r=Object.defineProperties(a,{});r===a&&a.length===0;",
        ),
        (
            "create-properties",
            "var o=Object.create(null,{x:{value:7}});o.x===7;",
        ),
        // Array target with an outer non-enumerable props row whose value is
        // the primitive `1`. ES5 excludes that row from the retained key
        // snapshot, so the call returns the *same* array with `length` still 0
        // and defines nothing. An accidental Get/ToPropertyDescriptor of the
        // primitive row value would throw a TypeError instead of returning, so
        // reaching the return value at all proves the row was skipped.
        (
            "array-target-outer-nonenum-primitive-props",
            "var a=[];var props={};Object.defineProperty(props,\"a\",{value:1,enumerable:false});var r=Object.defineProperties(a,props);r===a&&a.length===0;",
        ),
    ] {
        let actual = runner.execute(source);
        eprintln!("DEFINE_PROPERTIES_BEHAVIOR id={id} actual={actual:?}");
        assert_eq!(
            field(&actual, "flag"),
            Value::Bool(true),
            "{id}: bounded descriptor frame keeps the public result"
        );
    }
}

/// Runtime proof that the initialization reservation drop (1032 → 8) does not
/// shrink `Object.defineProperties`. Twenty props rows complete and the last
/// row is readable (`flag=true`, 121s on the debug interpreter, 10_000_000
/// fuel). No root-frame capacity error is produced. Twenty is the count this
/// run pinned: 200 did not finish compiling inside 300s, and the force-GC
/// frame-entry stop does not finish a 32-row literal inside the test budget.
/// The ordinary-literal cap is 256 (`MAX_OBJECT_PROPERTIES`). The runtime
/// worst case remains `8 + 4 * 256 = 1032`, requested by the unchanged
/// `root_enter(8 + 4 * count)` in `object.rs`.
/// `p7_define_properties_bounded_frame_capacity_interpreter` already checks
/// that dynamic request at 0/1/2 rows (capacities 8/12/16).
#[test]
fn p7_define_properties_wide_object_runtime_interpreter() {
    const ROWS: i64 = 20;
    let wide = (0..ROWS)
        .map(|index| format!("p{index}:{{value:{index}}}"))
        .collect::<Vec<_>>()
        .join(",");
    let source = format!(
        "var o={{}};var r=Object.defineProperties(o,{{{wide}}});r===o&&o.p0===0&&o.p19===19;"
    );
    eprintln!("DEFINE_PROPERTIES_WIDE start rows={ROWS} bytes={}", source.len());
    let actual = Runner { native: false }.execute(&source);
    eprintln!("DEFINE_PROPERTIES_WIDE rows={ROWS} actual={actual:?}");
    assert_eq!(
        field(&actual, "flag"),
        Value::Bool(true),
        "20-row defineProperties must succeed without a root-frame capacity error"
    );
}

/// Callback and snapshot retention across the bounded frame. The retained key
/// set is fixed before any descriptor getter runs, and the conversion phase
/// completes before every definition, so a row added by a getter is never
/// defined and a row deleted by a getter still fails the conversion with the
/// earlier property untouched.
#[test]
fn p7_define_properties_bounded_frame_callback_snapshot_interpreter() {
    let runner = Runner { native: false };
    let growth = runner.execute(
        "var o={};var p={a:{get value(){\"use strict\";this.owner.z={value:9};return 1;}},b:{value:2}};p.a.owner=p;Object.defineProperties(o,p);o.a===1&&o.b===2&&!(\"z\" in o);",
    );
    eprintln!("DEFINE_PROPERTIES_SNAPSHOT id=growth actual={growth:?}");
    assert_eq!(
        field(&growth, "flag"),
        Value::Bool(true),
        "growth: the retained key set is not extended by a getter-defined row"
    );
    let deleted = runner.execute(
        "var o={};var p={a:{get value(){\"use strict\";delete this.owner.b;return 1;}},b:{value:2}};p.a.owner=p;try{Object.defineProperties(o,p);}catch(e){e.name===\"TypeError\"&&!(\"a\" in o);}",
    );
    eprintln!("DEFINE_PROPERTIES_SNAPSHOT id=deletion actual={deleted:?}");
    assert_eq!(
        field(&deleted, "flag"),
        Value::Bool(true),
        "deletion: the retained snapshot key survives and no earlier property is applied"
    );
}

/// Test-only precise collection control for descriptor callbacks. The emitted
/// program is the production frontend IR plus the callback-site
/// observe/collect/observe/assert block; everywhere else the ordinary
/// 32-allocation collection policy stays exactly as production runs it.
struct CallbackCollectPlugin;

impl vm::SourcePlugin for CallbackCollectPlugin {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        let mut descriptor = vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN);
        descriptor.id = vm::FrontendId::new("javascript-es5-object-observer");
        descriptor
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
        let sites = object_observer::install_callback_collect(&mut ir);
        eprintln!(
            "CALLBACK_COLLECT_SITE function={} name={} before_slot={} after_slot={}",
            sites.function, sites.name, sites.before, sites.after
        );
        Ok(ir)
    }
}

/// One real VM run of a source whose callback collects at the marked point.
/// Configuration, registry, fuel and interpreter-only settings match the
/// ordinary object runner; only the emitted callback block differs.
fn callback_collect_run(source: &str) -> (Result<VmStatus, vm::VmError>, Vec<Value>) {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-object-observer")),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(Box::leak(Box::new(CallbackCollectPlugin)))
            .with_host_api_catalog(catalog.clone()),
    )
    .unwrap_or_else(|error| panic!("collect control must compile:\n{source}\n{error:?}"));
    let mut machine = compiled.into_vm().expect("certified VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("owned arrays");
    let mut registry = HostFunctionRegistry::restricted();
    for module in [
        pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
        pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module(),
    ] {
        module
            .install_from_catalog(&mut registry, &catalog)
            .expect("exact catalog");
    }
    registry.allow_builtin("math::floor").expect("floor");
    registry
        .allow_builtin("bytes::from_utf8")
        .expect("bootstrap strings");
    registry.bind_vm_cached(&mut machine).expect("bind");
    machine.set_fuel(10_000_000);
    let status = machine.run();
    assert!(!machine.jit_config().enabled, "interpreter only");
    assert_eq!(machine.aot_exec_count(), 0, "no AOT execution");
    assert_eq!(
        machine.jit_native_exec_count(),
        0,
        "no native trace execution"
    );
    (status, machine.stack().to_vec())
}

/// Retained-root proof across a real collection inside the later descriptor's
/// field getter.
///
/// Conversion order is fixed by the snapshot: row `a` is converted first, so
/// its descriptor is already held in the descriptor-frame root entries. In the
/// `data-value` case that descriptor is the fresh managed object
/// `{value:{q:1}}` built by the props getter, and nothing outside the
/// conversion's retained entry refers to the returned `{q:1}`: no variable,
/// alias or props row holds it, so a traced data *reference* (not just a
/// primitive) is what must survive the collection. In the
/// `accessor-getter-setter` case the retained slots are fresh getter/setter
/// functions instead. The later
/// row `b` returns an attribute record whose `enumerable` getter deletes the
/// earlier props row, writes the authored marker `p.n = 1` through its own
/// `this` chain (the closed subset has no free-variable capture, so nothing is
/// smuggled in a closure), and then collects at that exact point.
///
/// At collection time the earlier attributes object has no owner other than
/// the conversion's own input temporary, which has already been overwritten by
/// the later attribute record, and the earlier props row no longer exists. The
/// retained key string and the retained value/get/set are therefore the only
/// owners of what the definition later uses. The later attribute record is the
/// receiver of the running getter, so it is *not* part of that exclusive claim.
/// Everything else here is ordinary public semantics: the target `o`, the
/// props object `p` and the running frames are normally rooted.
#[test]
fn p7_define_properties_callback_collect_retained_roots_interpreter() {
    for (id, source) in [
        (
            "data-value",
            r#"var o={};var p={};var x={};var ge=function(){"use strict";delete this.owner.a;this.owner.n=1;return true;};var ga=function(){"use strict";return {value:{q:1}};};Object.defineProperty(x,"enumerable",{get:ge});x.owner=p;Object.defineProperty(p,"a",{get:ga,enumerable:true,configurable:true});Object.defineProperty(p,"b",{value:x,enumerable:true,configurable:true});Object.defineProperties(o,p);o.a.q===1&&p.n===1&&!("a" in p);"#,
        ),
        (
            "accessor-getter-setter",
            r#"var o={};var p={};var x={};var ge=function(){"use strict";delete this.owner.a;this.owner.n=1;return true;};var ga=function(){"use strict";return {get:function(){"use strict";return 5;},set:function(v){"use strict";this.hit=v;}};};Object.defineProperty(x,"enumerable",{get:ge});x.owner=p;Object.defineProperty(p,"a",{get:ga,enumerable:true,configurable:true});Object.defineProperty(p,"b",{value:x,enumerable:true,configurable:true});Object.defineProperties(o,p);var r=o.a;o.a=9;r===5&&o.hit===9&&p.n===1&&!("a" in p);"#,
        ),
    ] {
        let (status, stack) = callback_collect_run(source);
        assert_eq!(
            status.expect("collect control halts"),
            VmStatus::Halted,
            "{id}: the in-callback collection must execute and the guest must halt"
        );
        let actual = stack.last().expect("script value");
        eprintln!("DEFINE_PROPERTIES_CALLBACK_COLLECT id={id} actual={actual:?}");
        assert_eq!(
            field(actual, "flag"),
            Value::Bool(true),
            "{id}: retained key/value/get/set survived the in-callback collection"
        );
    }
}

/// Nested `defineProperties` whose inner conversion collects while both
/// descriptor frames are alive. The inner snapshot key `q` outlives its own
/// deleted props row and the inner descriptor value is retained by the inner
/// frame at that collection. The outer frame is only *overlapping* here: its
/// own value has not been returned yet when the inner collection runs, so this
/// case does not claim that an already-converted outer value survives an inner
/// collect.
#[test]
fn p7_define_properties_nested_callback_collect_interpreter() {
    let (status, stack) = callback_collect_run(
        r#"var out={};var inn={};var pin={};var x={};var ge=function(){"use strict";delete this.owner.q;return true;};var ga=function(){"use strict";Object.defineProperties(this.inn,this.pin);return {value:1};};Object.defineProperty(x,"enumerable",{get:ge});Object.defineProperty(x,"value",{value:3});x.owner=pin;Object.defineProperty(pin,"q",{value:x,enumerable:true,configurable:true});var p={};Object.defineProperty(p,"a",{get:ga,enumerable:true,configurable:true});p.inn=inn;p.pin=pin;Object.defineProperties(out,p);out.a===1&&inn.q===3&&!("q" in pin);"#,
    );
    assert_eq!(
        status.expect("nested collect control halts"),
        VmStatus::Halted,
        "the nested frame must collect while the outer frame is alive and halt"
    );
    let actual = stack.last().expect("script value");
    eprintln!("DEFINE_PROPERTIES_NESTED_COLLECT actual={actual:?}");
    assert_eq!(
        field(actual, "flag"),
        Value::Bool(true),
        "both the nested and the outer public results survive the collection"
    );
}

/// Object payload, descriptor-field accessor and TypeError propagation through
/// `finally`, measured from the actual final boxed result.
///
/// The object-throwing conversion uses a real descriptor *field* accessor. The
/// later descriptor `b` is an object whose own `value` accessor increments its
/// own `entered` counter and throws its own `payload`; `Object.defineProperties`
/// reads `value` while converting `b`, so the accessor really runs, and it runs
/// after the earlier valid descriptor `a` (`{a: valid, b: accessor}`) was
/// already converted. Every descriptor is converted before any definition is
/// applied, so the throw must leave the target without the earlier property,
/// must be caught with the same payload object (its own field readable), and
/// must pass through `finally` exactly once.
///
/// Bits reported by the guest (one real VM run):
/// * 1    `finally` ran exactly once for the object-throwing conversion;
/// * 2    the descriptor-field accessor was entered exactly once;
/// * 4    the explicit `catch` block was entered;
/// * 8    the catch binding is the *same object* as the thrown payload;
/// * 16   the thrown object's own field is readable in the catch binding;
/// * 32   `finally` ran exactly once for the real TypeError case;
/// * 64   the caught error is the real `TypeError` (`e.name`);
/// * 128  a plain getter throw (no `defineProperties`) keeps the same identity;
/// * 256  that plain-getter `finally` witness ran exactly once;
/// * 512  no earlier target property was applied before the throw.
///
/// All of these are asserted exactly, so the object path is measured rather
/// than assumed: an unconverted descriptor or a replaced payload would show up
/// as a zero bit instead of passing silently.
#[test]
fn p7_define_properties_object_throw_finally_interpreter() {
    let runner = Runner { native: false };
    let actual = runner.execute(
        r#"var payload={v:42};var target={};var ok={value:7};var attrs={get value(){"use strict";this.entered=this.entered+1;throw this.payload;}};attrs.entered=0;attrs.payload=payload;var fw=0;var caught=0;var same=false;var body=false;try{try{Object.defineProperties(target,{a:ok,b:attrs});}finally{fw=fw+1;}}catch(e){caught=1;same=(e===payload);body=(e.v===42);}var applied=("a" in target);var t=0;var fwt=0;try{try{Object.defineProperties({},{a:{value:1},b:{get:1}});}finally{fwt=fwt+1;}}catch(e){t=(e.name==="TypeError")?1:0;}var d2={get x(){"use strict";throw this.payload;}};d2.payload=payload;var id2=false;var fw2=0;try{try{var q=d2.x;}finally{fw2=fw2+1;}}catch(e2){id2=(e2===payload);}var c=0;if(fw===1){c=c+1;}if(attrs.entered===1){c=c+2;}if(caught===1){c=c+4;}if(same){c=c+8;}if(body){c=c+16;}if(fwt===1){c=c+32;}if(t===1){c=c+64;}if(id2){c=c+128;}if(fw2===1){c=c+256;}if(applied===false){c=c+512;}c;"#,
    );
    eprintln!("DEFINE_PROPERTIES_THROW_FINALLY actual={actual:?}");
    let code = match field(&actual, "number") {
        Value::Float(number) => number as i64,
        other => panic!("throw/finally report must be a number, got {other:?}"),
    };
    eprintln!(
        "DEFINE_PROPERTIES_THROW_FINALLY bits={code} finally_object={} accessor_entries={} catch_entered={} object_identity={} payload_field={} finally_type_error={} type_error_name={} direct_getter_identity={} direct_finally={} earlier_applied={}",
        code & 1,
        (code >> 1) & 1,
        (code >> 2) & 1,
        (code >> 3) & 1,
        (code >> 4) & 1,
        (code >> 5) & 1,
        (code >> 6) & 1,
        (code >> 7) & 1,
        (code >> 8) & 1,
        (code >> 9) & 1
    );
    assert_eq!(
        code & 1,
        1,
        "the object-throw finally witness must run exactly once"
    );
    assert_eq!(
        (code >> 1) & 1,
        1,
        "the descriptor-field accessor must be entered exactly once"
    );
    assert_eq!(
        (code >> 2) & 1,
        1,
        "the object throw must be caught by the explicit catch block"
    );
    assert_eq!(
        (code >> 3) & 1,
        1,
        "the catch binding must be the same object as the thrown payload"
    );
    assert_eq!(
        (code >> 4) & 1,
        1,
        "the thrown object's own field must stay readable in the catch binding"
    );
    assert_eq!(code & 32, 32, "the TypeError finally witness must run");
    assert_eq!(code & 64, 64, "the caught error must be the real TypeError");
    assert_eq!(
        code & 128,
        128,
        "a plain getter throw must keep the payload identity"
    );
    assert_eq!(
        code & 256,
        256,
        "the plain-getter finally witness must run exactly once"
    );
    assert_eq!(
        code & 512,
        512,
        "no earlier target property may be applied before the throwing field"
    );
}

#[test]
fn p7_forced_gc_conversion_failure_interpreter() {
    oracle_p7_forced_gc_conversion_failure_interpreter(false);
}

fn oracle_p7_forced_gc_conversion_failure_interpreter(native: bool) {
    let runner = Runner { native };
    let actual = runner.run(
        "var o={};try{Object.defineProperties(o,{a:{value:1},b:{get:1}});}catch(e){e.name===\"TypeError\"&&!(\"a\" in o);}",
        Some(object_observer::Observer {
            force_gc: true,
            ..Default::default()
        }),
    );
    let marker = actual
        .iter()
        .position(|v| *v == Value::string("P7_OBSERVED"))
        .unwrap();
    assert_eq!(field(&actual[marker + 1], "flag"), Value::Bool(true));
}

#[test]
fn p7_accessor_edge_root_handoffs_interpreter() {
    oracle_accessor_edge_roots(false);
}

fn oracle_accessor_edge_roots(native: bool) {
    let runner = Runner { native };
    for source in [
        r#"var o={get x(){"use strict";return 7;}};var pad={};o.x===7;"#,
        r#"var o={get x(){"use strict";delete this.x;var pad={};return this;}};o.x===o&&!("x" in o);"#,
        r#"var o={set x(v){"use strict";delete this.x;var pad={};throw v;}};try{o.x={ok:true};}catch(e){e.ok&&!("x" in o);}"#,
        r#"var d={get get(){"use strict";delete this.get;var pad={};return function(){"use strict";return 9;};}};var o={};Object.defineProperty(o,"x",d);var p={};o.x===9;"#,
    ] {
        let actual = runner.run(
            source,
            Some(object_observer::Observer {
                force_gc: true,
                ..Default::default()
            }),
        );
        let marker = actual
            .iter()
            .position(|v| *v == Value::string("P7_OBSERVED"))
            .expect("actual forced collection observation");
        assert_eq!(field(&actual[marker + 1], "tag"), Value::Int(2));
        assert_eq!(
            field(&actual[marker + 1], "flag"),
            Value::Bool(true),
            "{source}"
        );
    }
}

#[test]
fn p7_specialized_realms_have_no_intrinsic_frame_interpreter() {
    let runner = Runner { native: false };
    for source in [
        r#"var a=[7];a[0]===7;"#,
        r#"var a=[7];var Object=8;Object===8&&a[0]===7;"#,
        r#"var Object=7;var s="x";Object===7&&s==="x";"#,
        r#"var s="x";var unused=null;s==="x";"#,
    ] {
        let actual = runner.run(
            source,
            Some(object_observer::Observer {
                realm_header: true,
                ..Default::default()
            }),
        );
        let marker = actual
            .iter()
            .position(|v| *v == Value::string("P7_OBSERVED"))
            .unwrap();
        assert_eq!(field(&actual[marker + 1], "tag"), Value::Int(2));
        assert_eq!(field(&actual[marker + 1], "flag"), Value::Bool(true));
        let header = actual
            .iter()
            .position(|v| *v == Value::string("P7_REALM_HEADER"))
            .unwrap();
        assert_eq!(
            &actual[header + 1..header + 3],
            &[Value::Float(2.0), Value::Float(0.0)],
            "{source}"
        );
    }
}

macro_rules! native_oracle {
    ($name:ident,$oracle:ident) => {
        #[test]
        #[ignore = "requires fresh parent native approval"]
        fn $name() {
            std::thread::Builder::new()
                .name(stringify!($name).into())
                .stack_size(33_554_432)
                .spawn(|| $oracle(true))
                .expect("real 32MiB worker")
                .join()
                .expect("native oracle worker");
        }
    };
}
native_oracle!(
    p7_general_call_with_exact_adapter_stays_normal_native_ssa,
    oracle_p7_general_call_with_exact_adapter_stays_normal_interpreter
);
native_oracle!(
    p7_o01_inherited_get_has_and_missing_own_descriptor_native_ssa,
    oracle_p7_o01_inherited_get_has_and_missing_own_descriptor_interpreter
);
native_oracle!(
    p7_data_vectors_native_ssa,
    oracle_p7_data_vectors_interpreter
);
native_oracle!(
    p7_o07_strict_closed_getter_receiver_native_ssa,
    oracle_p7_o07_strict_closed_getter_receiver_interpreter
);
native_oracle!(
    p7_type_error_name_native_ssa,
    oracle_p7_type_error_name_interpreter
);
native_oracle!(
    p7_accessor_vectors_native_ssa,
    oracle_p7_accessor_vectors_interpreter
);
native_oracle!(
    p7_accessor_utf16_and_order_native_ssa,
    oracle_p7_accessor_utf16_and_order_interpreter
);
native_oracle!(
    p7_o11_uncaught_getter_original_payload_native_ssa,
    oracle_p7_o11_uncaught_getter_original_payload_interpreter
);
native_oracle!(
    p7_o18_uncaught_real_type_error_native_ssa,
    oracle_p7_o18_uncaught_real_type_error_interpreter
);
native_oracle!(
    p7_descriptor_phases_native_ssa,
    oracle_p7_descriptor_phases_interpreter
);
native_oracle!(
    p7_call_corrections_native_ssa,
    oracle_p7_call_corrections_interpreter
);
native_oracle!(
    p7_descriptor_field_and_noncallable_order_native_ssa,
    oracle_p7_descriptor_field_and_noncallable_order_interpreter
);
native_oracle!(
    p7_forced_gc_accessor_and_descriptor_handoffs_native_ssa,
    oracle_p7_forced_gc_accessor_and_descriptor_handoffs_interpreter
);
native_oracle!(
    p7_forced_gc_conversion_failure_native_ssa,
    oracle_p7_forced_gc_conversion_failure_interpreter
);
native_oracle!(
    p7_accessor_edge_root_handoffs_native_ssa,
    oracle_accessor_edge_roots
);

#[test]
fn p7_ceq_numeric_member_call_and_boolean_interpreter() {
    // Dynamic member/call producers retain their actual Bool and Number payloads.
    // Counter checks also require once-only, left-to-right operand evaluation.
    let actual = Runner { native: false }.execute(
        "var n=0;var o={a:true,b:false,x:0/0,z:-0};function f(c,v){'use strict';return v;}var a=f(n=n+1,o.a)===true;var b=f(n=n+1,o.b)===false;var c=f(n=n+1,o.x)!==f(n=n+1,o.x);var d=f(n=n+1,o.z)===0;a&&b&&c&&d&&n===5;",
    );
    assert_eq!(field(&actual, "tag"), Value::Int(2));
    assert_eq!(field(&actual, "flag"), Value::Bool(true));
    let completion = Runner { native: false }
        .execute("function f(){'use strict';return false;}var r=0;if(f()){r=1;}else{r=2;}");
    assert_eq!(field(&completion, "number"), Value::Float(2.0));
}

#[test]
fn p7_not_boolean_short_circuit_return_and_getter_interpreter() {
    let runner = Runner { native: false };
    for source in [
        "var o={n:0,m:0,get f(){'use strict';if(this.n===1){throw 7;}this.n=1;return false;},get t(){'use strict';if(this.m===1){throw 8;}this.m=1;return true;}};var a=o.f&&o.t;var b=o.t||o.f;a===false&&b===true&&o.n===1&&o.m===1;",
        "function f(c,v){'use strict';return v;}function bad(){'use strict';throw 7;}var n=0;var a=f(n=n+1,false)&&bad();var b=f(n=n+1,true)||bad();var o={};Object.defineProperty(o,'x',{value:false});var d=Object.getOwnPropertyDescriptor(o,'x');a===false&&b===true&&n===2&&d.value===false&&d.writable===false&&(delete o.x)===false&&Object.getOwnPropertyDescriptor(o,'z')===undefined;",
    ] {
        let actual = runner.execute(source);
        assert_eq!(field(&actual, "tag"), Value::Int(2), "{source}");
        assert_eq!(field(&actual, "flag"), Value::Bool(true), "{source}");
    }
}

#[test]
fn p7_not_boolean_object_helper_program_structure() {
    struct Tap(std::sync::Mutex<Vec<(String, usize)>>);
    impl vm::SourcePlugin for Tap {
        fn descriptor(&self) -> vm::SourceFrontendDescriptor {
            vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN)
        }
        fn parse_source(
            &self,
            source: &str,
            context: &vm::SourceFrontendContext<'_>,
        ) -> Result<vm::FrontendIr, vm::ParseError> {
            let ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
            let has = ir
                .functions
                .iter()
                .find(|f| f.name == "__compat_es5_has_property")
                .unwrap();
            let body = &ir.function_impls[&has.index].body_stmts;
            let condition = body
                .iter()
                .find_map(|s| match s {
                    vm::Stmt::While {
                        condition: vm::Expr::And(left, _),
                        ..
                    } => Some(left.as_ref()),
                    _ => None,
                })
                .expect("actual has-property loop");
            assert!(
                matches!(condition, vm::Expr::IfElse { condition, then_expr, else_expr }
                if matches!(condition.as_ref(), vm::Expr::Var(_))
                && matches!(then_expr.as_ref(), vm::Expr::Bool(false))
                && matches!(else_expr.as_ref(), vm::Expr::Bool(true))),
                "Bool inversion must use one conditional evaluation: {condition:?}"
            );
            let mut indices: Vec<_> = ir.function_impls.keys().copied().collect();
            indices.sort_unstable();
            // These are the actual ordinary-object emitter family, not all
            // frontend opcodes. Core orders named script bodies by IR index.
            let first = ir
                .functions
                .iter()
                .find(|f| f.name == "__compat_es5_intrinsic_root")
                .unwrap()
                .index;
            let last = ir
                .functions
                .iter()
                .find(|f| f.name == "__compat_es5_dispatch")
                .unwrap()
                .index;
            *self.0.lock().unwrap() = ir
                .functions
                .iter()
                .filter(|f| first <= f.index && f.index <= last)
                .map(|f| (f.name.clone(), indices.binary_search(&f.index).unwrap()))
                .collect();
            Ok(ir)
        }
    }
    let tap = Box::leak(Box::new(Tap(Default::default())));
    let source = "var p={x:1};var o=Object.create(p);var a=o.x===1;var b=\"x\" in o;var c=Object.getOwnPropertyDescriptor(o,\"x\")===undefined;a&&b&&c;";
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        javascript_es5_flavor(),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(tap)
            .with_host_api_catalog(
                pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog(),
            ),
    )
    .unwrap()
    .into_uncertified();
    let p = &compiled.program;
    let mut instructions = Vec::new();
    let mut ip = 0;
    while ip < p.code.len() {
        let op = vm::OpCode::try_from(p.code[ip]).expect("instruction boundary");
        instructions.push((ip, op));
        ip += 1 + op.operand_len();
    }
    assert_eq!(ip, p.code.len());
    let helpers = tap.0.lock().unwrap();
    let pruned = [
        "__compat_es5_function_length_key",
        "__compat_es5_ref_put",
        "__compat_es5_delete_property",
    ];
    assert!(
        pruned
            .iter()
            .all(|name| !helpers.iter().any(|(retained, _)| retained == name))
    );
    let expected_names: std::collections::BTreeSet<_> = [
        "__compat_es5_intrinsic_root",
        "__compat_es5_object_allocate",
        "__compat_es5_intrinsic_bootstrap",
        "__compat_es5_new_type_error",
        "__compat_es5_get_own_property",
        "__compat_es5_has_property",
        "__compat_es5_same_value",
        "__compat_es5_object_reserve_rows",
        "__compat_es5_ref_get",
        "__compat_es5_define_own_property",
        "__compat_es5_to_property_descriptor",
        "__compat_es5_descriptor_row_validator",
        "__compat_es5_from_property_descriptor",
        "__compat_es5_define_properties",
        "__compat_es5_object_intrinsic_call",
        "__compat_es5_dispatch",
        "__compat_es5_bootstrap_row_suffix",
        "__compat_es5_bootstrap_ascii_init",
    ]
    .into_iter()
    .collect();
    let actual_names: std::collections::BTreeSet<_> =
        helpers.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        actual_names.len(),
        helpers.len(),
        "actual helper names must be unique: {helpers:?}"
    );
    assert_eq!(
        actual_names, expected_names,
        "complete selected helper family"
    );
    assert!(
        helpers
            .iter()
            .any(|(name, _)| name == "__compat_es5_descriptor_row_validator")
    );
    for (name, script) in helpers.iter() {
        let function = &p.script_functions[*script];
        let body: Vec<_> = instructions
            .iter()
            .filter(|(ip, _)| function.entry_ip as usize <= *ip && *ip < function.end_ip as usize)
            .collect();
        assert!(!body.is_empty(), "{name}");
        assert!(
            !body.iter().any(|(_, op)| *op == vm::OpCode::Not),
            "object Bool producer still emits Not: {name}"
        );
    }
    eprintln!(
        "P7_NOT_OBJECT_PROGRAM helpers={} instructions={} no_not=true",
        helpers.len(),
        instructions.len()
    );
}

#[test]
fn p7_ceq_numeric_tag_program_metadata() {
    let source = "var p={x:1};var o=Object.create(p);var a=o.x===1;var b=\"x\" in o;var c=Object.getOwnPropertyDescriptor(o,\"x\")===undefined;a&&b&&c;";
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        javascript_es5_flavor(),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(&JAVASCRIPT_ES5_PLUGIN)
            .with_host_api_catalog(catalog),
    )
    .unwrap()
    .into_uncertified();
    let program = &compiled.program;
    let types = program.type_map.as_ref().expect("actual physical metadata");
    let constant = |ip: usize| {
        let bytes = &program.code[ip + 1..ip + 5];
        &program.constants[u32::from_le_bytes(bytes.try_into().unwrap()) as usize]
    };
    let mut instructions = Vec::new();
    let mut ip = 0;
    while ip < program.code.len() {
        let op = vm::OpCode::try_from(program.code[ip]).expect("decoded opcode");
        instructions.push((ip, op));
        ip += 1 + op.operand_len();
    }
    let mut guards = 0;
    let mut typed = 0;
    let mut unhinted = 0;
    for window in instructions.windows(8) {
        let [
            (key, vm::OpCode::Ldc),
            (_, vm::OpCode::Call),
            (one, vm::OpCode::Ldc),
            (_, vm::OpCode::Div),
            (code, vm::OpCode::Ldc),
            (unit, vm::OpCode::Ldc),
            (_, vm::OpCode::Div),
            (site, vm::OpCode::Ceq),
        ] = window
        else {
            continue;
        };
        if constant(*key) != &Value::string("tag") {
            continue;
        }
        assert_eq!(constant(*one), &Value::Float(1.0));
        assert_eq!(constant(*unit), &Value::Float(1.0));
        assert!(matches!(constant(*code), Value::Int(0..=5)));
        // Some Get-derived helper comparisons have no operand hint. In both
        // cases the actual Div producers establish F64/F64, without editing
        // the physical TypeMap or projecting a Bool/arbitrary JS box.
        match types.operand_types.get(site) {
            Some((vm::ValueType::Float, vm::ValueType::Float)) => typed += 1,
            None => unhinted += 1,
            other => panic!("unexpected numeric-tag metadata at {site}: {other:?}"),
        }
        guards += 1;
    }
    assert!(guards > 0, "real numeric-tag guards must be selected");
    assert!(typed > 0 && unhinted > 0, "both actual metadata paths");
    eprintln!("P7_CEQ_TAG_METADATA guards={guards} float_pair_hints={typed} unhinted={unhinted}");
}

#[test]
fn p7_scalar_int_identity_pipeline_and_interpreter() {
    struct Tap(std::sync::Mutex<Option<vm::FrontendIr>>);
    impl vm::SourcePlugin for Tap {
        fn descriptor(&self) -> vm::SourceFrontendDescriptor {
            vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN)
        }
        fn parse_source(
            &self,
            source: &str,
            context: &vm::SourceFrontendContext<'_>,
        ) -> Result<vm::FrontendIr, vm::ParseError> {
            let mut ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
            let helper = ir
                .functions
                .iter()
                .find(|f| f.name == "__compat_es5_int_identity")
                .expect("private typed Int identity boundary must exist");
            assert_eq!(helper.arg_schemas, vec![Some(vm::TypeSchema::Int)]);
            assert_eq!(helper.return_schema, Some(vm::TypeSchema::Int));
            assert_eq!(helper.return_type, vm::ValueType::Int);
            assert!(!helper.exported);
            let index = helper.index;
            let implementation = &ir.function_impls[&index];
            assert!(implementation.body_stmts.is_empty());
            assert!(implementation.capture_copies.is_empty());
            assert_eq!(implementation.param_slots.len(), 1);
            assert!(
                matches!(implementation.body_expr, vm::Expr::Var(slot) if slot == implementation.param_slots[0])
            );
            let call = |target, args| vm::Expr::Call(target, Vec::new(), args, None, None);
            let count = ir.locals as u16;
            ir.locals += 1;
            ir.stmts.push(vm::Stmt::Let {
                index: count,
                declared_schema: Some(vm::TypeSchema::Int),
                expr: vm::Expr::Int(0),
                line: 1,
            });
            for value in [0, 5, 51, 255, 256, 4_294_967_295] {
                // A test-only private Int record. The production bridge body
                // does not construct a record, coerce or allocate anything.
                let record = call(
                    vm::BuiltinFunction::Set.call_index(),
                    vec![
                        call(vm::BuiltinFunction::MapNew.call_index(), vec![]),
                        vm::Expr::String("index".into()),
                        vm::Expr::Int(value),
                    ],
                );
                let get = call(
                    vm::BuiltinFunction::Get.call_index(),
                    vec![record, vm::Expr::String("index".into())],
                );
                let once = vm::Expr::Block {
                    stmts: vec![vm::Stmt::Assign {
                        kind: vm::AssignmentKind::Set,
                        index: count,
                        expr: vm::Expr::Add(
                            Box::new(vm::Expr::Var(count)),
                            Box::new(vm::Expr::Int(1)),
                        ),
                        line: 1,
                    }],
                    expr: Box::new(get),
                };
                let bridged = call(index, vec![once]);
                for expr in [
                    bridged.clone(),
                    vm::Expr::Add(Box::new(vm::Expr::Int(7)), Box::new(bridged.clone())),
                    vm::Expr::Mul(Box::new(vm::Expr::Int(16)), Box::new(bridged)),
                ] {
                    ir.stmts.push(vm::Stmt::Expr { expr, line: 1 });
                }
            }
            ir.stmts.push(vm::Stmt::Expr {
                expr: vm::Expr::Var(count),
                line: 1,
            });
            let binding = |name: &str| ir.local_bindings.iter().find(|(n, _)| n == name).unwrap().1;
            let make = ir
                .functions
                .iter()
                .find(|f| f.name == "__compat_ref_make")
                .unwrap()
                .index;
            let carrier = ir
                .stmts
                .iter()
                .find_map(|s| match s {
                    vm::Stmt::Let {
                        index,
                        expr: vm::Expr::Call(i, _, _, _, _),
                        ..
                    } if *i == make => Some(*index),
                    _ => None,
                })
                .unwrap();
            let own = ir
                .functions
                .iter()
                .find(|f| f.name == "__compat_es5_get_own_property")
                .unwrap()
                .index;
            ir.stmts.push(vm::Stmt::Expr {
                expr: call(
                    own,
                    vec![
                        vm::Expr::Var(carrier),
                        vm::Expr::Var(binding("o")),
                        vm::Expr::Var(binding("k")),
                    ],
                ),
                line: 1,
            });
            *self.0.lock().unwrap() = Some(ir.clone());
            Ok(ir)
        }
    }
    let tap = Box::leak(Box::new(Tap(Default::default())));
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        "var o={x:-0};var k='x';o;",
        javascript_es5_flavor(),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(tap)
            .with_host_api_catalog(catalog.clone()),
    )
    .unwrap();
    let mut machine = compiled.into_vm().unwrap();
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    let ir = tap.0.lock().unwrap();
    let ir = ir.as_ref().unwrap();
    let helper = ir
        .functions
        .iter()
        .find(|f| f.name == "__compat_es5_int_identity")
        .unwrap();
    let mut indices: Vec<_> = ir.function_impls.keys().copied().collect();
    indices.sort_unstable();
    let script = indices.binary_search(&helper.index).unwrap();
    let p = machine.program();
    let function = &p.script_functions[script];
    let prototype = p.callable_prototypes.iter().enumerate().find(|(_, p)|
        matches!(p.target, vm::CallableTarget::ScriptFunction(i) if i as usize == script)).unwrap();
    assert_eq!(
        prototype.1.schema,
        Some(vm::TypeSchema::Callable {
            params: vec![vm::TypeSchema::Int],
            result: Box::new(vm::TypeSchema::Int)
        })
    );
    assert_eq!(prototype.1.parameter_slots.len(), 1);
    let mut calls = 0;
    let mut body = Vec::new();
    let mut ip = 0;
    while ip < p.code.len() {
        let op = vm::OpCode::try_from(p.code[ip]).unwrap();
        if function.entry_ip as usize <= ip && ip < function.end_ip as usize {
            body.push(op);
        }
        if op == vm::OpCode::CallScript
            && u32::from_le_bytes(p.code[ip + 1..ip + 5].try_into().unwrap()) as usize
                == prototype.0
        {
            assert_eq!(
                p.type_map.as_ref().unwrap().operand_types.get(&ip),
                Some(&(vm::ValueType::Callable, vm::ValueType::Int))
            );
            assert_eq!(p.code[ip + 5], 1);
            calls += 1;
        }
        ip += 1 + op.operand_len();
    }
    assert!(calls >= 18, "real bridge calls cannot disappear: {calls}");
    assert!(body.contains(&vm::OpCode::Ret));
    assert!(!body.iter().any(|op| matches!(
        op,
        vm::OpCode::Call | vm::OpCode::CallScript | vm::OpCode::CallValue
    )));
    eprintln!(
        "P7_SCALAR_INT_PIPELINE prototype={} script={script} frame={} params={:?} calls={calls} body={body:?} entry=Int resume_hint=Int native=false",
        prototype.0, prototype.1.frame_local_count, prototype.1.parameter_slots
    );
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        Default::default(),
    )
    .unwrap();
    let mut registry = HostFunctionRegistry::restricted();
    pd_vm_compat_frontends::runtime::array::typescript_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .unwrap();
    pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .unwrap();
    pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .unwrap();
    pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .unwrap();
    pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .unwrap();
    registry.allow_builtin("math::floor").unwrap();
    registry.bind_vm_cached(&mut machine).unwrap();
    machine.set_fuel(10_000_000);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    assert!(!machine.jit_config().enabled);
    let mut expected = Vec::new();
    for value in [0, 5, 51, 255, 256, 4_294_967_295] {
        expected.extend([
            Value::Int(value),
            Value::Int(7 + value),
            Value::Int(16 * value),
        ]);
    }
    expected.push(Value::Int(18));
    let end = machine.stack().len() - 1;
    assert_eq!(&machine.stack()[end - expected.len()..end], expected);
    let own = machine.stack().last().unwrap();
    assert_eq!(field(own, "index"), Value::Int(0));
    assert_eq!(field(own, "found"), Value::Bool(true));
    let descriptor = field(own, "descriptor");
    assert_eq!(field(&descriptor, "present"), Value::Int(51));
    let payload = field(&descriptor, "value");
    assert_eq!(field(&payload, "tag"), Value::Int(3));
    assert!(
        matches!(field(&payload,"number"), Value::Float(n) if n == 0.0 && n.is_sign_negative())
    );
    assert!(matches!(field(&payload, "ref_slot"), Value::Int(_)));
    assert!(matches!(field(&payload, "generation"), Value::Int(_)));
    machine.reset_for_reuse().unwrap();
    assert_eq!(machine.host_context().resource_count(), 0);
    eprintln!(
        "P7_SCALAR_INT_INTERPRETER values=6 numeric_consumers=12 evaluations=18 wire=Int trace=0 aot=0"
    );
}

const P7_CALLER_O01: &str = "var p={x:1};var o=Object.create(p);var a=o.x===1;var b=\"x\" in o;var c=Object.getOwnPropertyDescriptor(o,\"x\")===undefined;a&&b&&c;";

fn caller_program(source: &str) -> (vm::FrontendIr, vm::Program) {
    struct Tap(std::sync::Mutex<Option<vm::FrontendIr>>);
    impl vm::SourcePlugin for Tap {
        fn descriptor(&self) -> vm::SourceFrontendDescriptor {
            vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN)
        }
        fn parse_source(
            &self,
            source: &str,
            context: &vm::SourceFrontendContext<'_>,
        ) -> Result<vm::FrontendIr, vm::ParseError> {
            let ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
            *self.0.lock().unwrap() = Some(ir.clone());
            Ok(ir)
        }
    }
    let tap = Box::leak(Box::new(Tap(Default::default())));
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        javascript_es5_flavor(),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(tap)
            .with_host_api_catalog(
                pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog(),
            ),
    )
    .unwrap()
    .into_uncertified();
    let ir = tap.0.lock().unwrap().take().unwrap();
    (ir, compiled.program)
}

fn caller_bridges(source: &str, key: &str, consumer: vm::OpCode) -> (usize, usize) {
    let (ir, p) = caller_program(source);
    let identities: Vec<_> = ir
        .functions
        .iter()
        .filter(|f| f.name == "__compat_es5_int_identity")
        .collect();
    assert_eq!(identities.len(), 1, "reuse one existing declaration");
    let identity = identities[0];
    assert_eq!(identity.arg_schemas, vec![Some(vm::TypeSchema::Int)]);
    assert_eq!(identity.return_schema, Some(vm::TypeSchema::Int));
    assert_eq!(identity.return_type, vm::ValueType::Int);
    let mut indices: Vec<_> = ir.function_impls.keys().copied().collect();
    indices.sort_unstable();
    let script = indices.binary_search(&identity.index).unwrap();
    let prototype = p
        .callable_prototypes
        .iter()
        .position(
            |p| matches!(p.target, vm::CallableTarget::ScriptFunction(i) if i as usize == script),
        )
        .unwrap();
    assert!(p.callable_prototypes[prototype].capture_slots.is_empty());
    let root_end = p
        .script_functions
        .iter()
        .map(|f| f.entry_ip as usize)
        .min()
        .unwrap();
    let mut ops = Vec::new();
    let mut ip = 0;
    while ip < p.code.len() {
        let op = vm::OpCode::try_from(p.code[ip]).unwrap();
        ops.push((ip, op));
        ip += 1 + op.operand_len();
    }
    let mut root = 0;
    let mut functions = 0;
    for window in ops.windows(5) {
        let [
            (key_ip, vm::OpCode::Ldc),
            (get, vm::OpCode::Call),
            (call, vm::OpCode::CallScript),
            (literal, vm::OpCode::Ldc),
            (site, op),
        ] = window
        else {
            continue;
        };
        let u32_at = |ip| u32::from_le_bytes(p.code[ip + 1..ip + 5].try_into().unwrap()) as usize;
        if *op != consumer
            || u32_at(*call) != prototype
            || p.constants[u32_at(*key_ip)] != Value::string(key)
        {
            continue;
        }
        assert_eq!(
            u16::from_le_bytes(p.code[get + 1..get + 3].try_into().unwrap()),
            vm::BuiltinFunction::Get.call_index()
        );
        assert_eq!(p.code[call + 5], 1);
        assert_eq!(
            p.type_map.as_ref().unwrap().operand_types.get(call),
            Some(&(vm::ValueType::Callable, vm::ValueType::Int))
        );
        assert_eq!(
            p.constants[u32_at(*literal)],
            Value::Int(if consumer == vm::OpCode::Ceq { 1 } else { 4 })
        );
        if *site < root_end {
            root += 1;
        } else if ir
            .functions
            .iter()
            .filter(|f| f.name.starts_with("__compat_es5_fn_"))
            .any(|f| {
                let body = &p.script_functions[indices.binary_search(&f.index).unwrap()];
                body.entry_ip as usize <= *site && *site < body.end_ip as usize
            })
        {
            functions += 1;
        }
        eprintln!(
            "P7_CALLER_INT key={key} get={get} call={call} consumer={site}:{op:?} prototype={prototype} resume=Int pair=I64/I64 root={} native=false",
            *site < root_end
        );
    }
    (root, functions)
}

#[test]
fn p7_caller_completion_return_selector_program() {
    assert_eq!(
        caller_bridges(P7_CALLER_O01, "kind", vm::OpCode::Ceq).0,
        2,
        "both original root ResultOk Completion RETURN selectors need Int calls"
    );
}

#[test]
fn p7_caller_descriptor_tag_packing_program() {
    assert_eq!(
        caller_bridges(P7_CALLER_O01, "tag", vm::OpCode::Shl).0,
        1,
        "inlined private descriptor tag must be Int before root Shl"
    );
}

#[test]
fn p7_caller_source_function_selectors_program() {
    let source = "function outer(x){'use strict';function inner(y){'use strict';return y;}return inner(x);}outer(7);";
    let (root, functions) = caller_bridges(source, "kind", vm::OpCode::Ceq);
    assert_eq!(
        (root, functions),
        (1, 1),
        "same emitter in both activations"
    );
    let source = "function make(){'use strict';return {x:1};}make();";
    assert_eq!(
        caller_bridges(source, "tag", vm::OpCode::Shl),
        (0, 1),
        "same inlined descriptor emitter inside source functions"
    );
}

#[test]
fn p7_caller_completion_handoffs_interpreter() {
    let r = Runner { native: false };
    for source in [
        "function normal(){'use strict';}function empty(){'use strict';return;}function value(){'use strict';return 7;}normal()===undefined&&empty()===undefined&&value()===7;",
        "function fail(x){'use strict';throw x;}var x={kind:1,tag:true};var answer=false;try{fail(x);}catch(e){answer=e===x;}answer;",
        "function outer(x){'use strict';function inner(y){'use strict';return y;}return inner(x);}outer(outer(7))===7;",
    ] {
        let result = r.execute(source);
        assert_eq!(field(&result, "tag"), Value::Int(2));
        assert_eq!(field(&result, "flag"), Value::Bool(true), "{source}");
    }
    eprintln!("P7_CALLER_HANDOFF normal return undefined throw nested multi-call trace=0 aot=0");
}

#[test]
fn p7_caller_single_evaluation_and_user_fields_interpreter() {
    let r = Runner { native: false };
    for source in [
        "var o={n:0,next:function(){'use strict';if(this.n===0){this.n=1;return 1;}if(this.n===1){this.n=2;return 2;}throw 3;},pick:function(a,b){'use strict';return a===1&&b===2;}};o.pick(o.next(),o.next())===true&&o.n===2;",
        "var o={kind:1.5,tag:true,has_value:false};o.kind===1.5&&o.tag===true&&o.has_value===false;",
        "var child={};var o={u:undefined,n:null,b:true,x:-0,s:'x',r:child};o.u===undefined&&o.n===null&&o.b===true&&o.x===0&&o.s==='x'&&o.r===child;",
    ] {
        let result = r.execute(source);
        assert_eq!(field(&result, "tag"), Value::Int(2));
        assert_eq!(field(&result, "flag"), Value::Bool(true), "{source}");
    }
    eprintln!(
        "P7_CALLER_VALUES receiver arg_order evaluations=2 private_tags=0..5 user_kind_Number user_tag_Bool trace=0 aot=0"
    );
}

#[test]
fn p7_caller_original_o01_actual_int_observer_interpreter() {
    struct Observe(std::sync::Mutex<Option<vm::FrontendIr>>);
    impl vm::SourcePlugin for Observe {
        fn descriptor(&self) -> vm::SourceFrontendDescriptor {
            vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN)
        }
        fn parse_source(
            &self,
            source: &str,
            context: &vm::SourceFrontendContext<'_>,
        ) -> Result<vm::FrontendIr, vm::ParseError> {
            let mut ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
            // Capture production bytes first, then add a test-only runtime
            // observer. No host catalog, source or producer contract changes.
            *self.0.lock().unwrap() = Some(ir.clone());
            let index = ir
                .functions
                .iter()
                .find(|f| f.name == "__compat_es5_int_identity")
                .unwrap()
                .index;
            let body = ir.function_impls.get_mut(&index).unwrap();
            let arg = body.param_slots[0];
            assert!(body.body_stmts.is_empty());
            assert!(matches!(body.body_expr, vm::Expr::Var(i) if i == arg));
            let call = |f, args| vm::Expr::Call(f, Vec::new(), args, None, None);
            body.body_stmts.push(vm::Stmt::Expr {
                expr: call(
                    vm::BuiltinFunction::Assert.call_index(),
                    vec![vm::Expr::Eq(
                        Box::new(call(
                            vm::BuiltinFunction::TypeOf.call_index(),
                            vec![vm::Expr::Var(arg)],
                        )),
                        Box::new(vm::Expr::String("int".into())),
                    )],
                ),
                line: 1,
            });
            Ok(ir)
        }
    }
    let tap = Box::leak(Box::new(Observe(Default::default())));
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        P7_CALLER_O01,
        javascript_es5_flavor(),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(tap)
            .with_host_api_catalog(catalog.clone()),
    )
    .unwrap();
    let mut machine = compiled.into_vm().unwrap();
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        Default::default(),
    )
    .unwrap();
    let mut registry = HostFunctionRegistry::restricted();
    pd_vm_compat_frontends::runtime::array::typescript_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .unwrap();
    pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .unwrap();
    pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .unwrap();
    pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .unwrap();
    pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .unwrap();
    registry.allow_builtin("math::floor").unwrap();
    registry.bind_vm_cached(&mut machine).unwrap();
    machine.set_fuel(10_000_000);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    assert!(!machine.jit_config().enabled);
    assert!(machine.capture_certification_stats().unwrap().valid);
    let answer = machine.stack().last().unwrap();
    assert_eq!(field(answer, "tag"), Value::Int(2));
    assert_eq!(field(answer, "flag"), Value::Bool(true));
    assert!(tap.0.lock().unwrap().is_some());
    machine.reset_for_reuse().unwrap();
    assert_eq!(machine.host_context().resource_count(), 0);
    eprintln!(
        "P7_CALLER_OBSERVER original_O01=true captured_before_observer=true every_executed_identity_TypeOf=int trace=0 aot=0"
    );
}

#[test]
fn p7_private_prune_live_and_observer_interfaces_program() {
    for (source, live, has_realm) in [
        (
            "var o={x:1};if(false){o.x=2;delete o.x;}o.x;",
            vec!["__compat_es5_ref_put", "__compat_es5_delete_property"],
            true,
        ),
        (
            "function f(){'use strict';return 1;}f;",
            vec!["__compat_es5_function_length_key"],
            true,
        ),
        ("var n=1;n;", vec![], false),
    ] {
        let (ir, _) = caller_program(source);
        for name in live {
            let f = ir.functions.iter().find(|f| f.name == name).expect(name);
            assert!(ir.function_impls.contains_key(&f.index));
        }
        for name in [
            "__compat_es5_root_write_node",
            "__compat_es5_node_publish_edge",
            "__compat_es5_heap_observe",
            "__compat_es5_set_prototype",
        ] {
            let f = ir.functions.iter().find(|f| f.name == name);
            assert_eq!(
                f.is_some(),
                has_realm,
                "observer provisioning changed: {name}"
            );
            if let Some(f) = f {
                assert!(ir.function_impls.contains_key(&f.index));
            }
        }
    }
}

#[test]
fn p7_row14_original_o01_global_frame_at_most_47() {
    let (_, program) = caller_program(P7_CALLER_O01);
    eprintln!(
        "P7_ROW14 bytes={} frame={} prototypes={} native=false",
        program.code.len(),
        program.local_count,
        program.callable_prototypes.len()
    );
    assert!(
        program.local_count <= 47,
        "ordinary O01 global frame {} exceeds 47",
        program.local_count
    );
    for prototype in &program.callable_prototypes {
        assert!(
            prototype.frame_local_count <= 47,
            "prototype: {prototype:?}"
        );
        assert_eq!(
            prototype.capture_slots.len(),
            prototype.capture_source_slots.len()
        );
        assert_eq!(prototype.capture_slots.len(), prototype.capture_modes.len());
        for slot in prototype
            .parameter_slots
            .iter()
            .chain(&prototype.capture_slots)
            .chain(&prototype.capture_source_slots)
            .chain(prototype.self_slot.iter())
        {
            assert!(
                usize::from(*slot) < program.local_count,
                "invalid frame slot {slot}"
            );
        }
    }
    let mut ip = 0;
    while ip < program.code.len() {
        let op = vm::OpCode::try_from(program.code[ip]).unwrap();
        let args = &program.code[ip + 1..ip + 1 + op.operand_len()];
        if matches!(op, vm::OpCode::Ldloc | vm::OpCode::Stloc) {
            assert!(usize::from(args[0]) < program.local_count);
        }
        if op == vm::OpCode::CallScript {
            let index = u32::from_le_bytes(args[..4].try_into().unwrap()) as usize;
            assert!(index < program.callable_prototypes.len());
        }
        ip += 1 + op.operand_len();
    }
}

#[test]
fn p7_row14_getter_order_and_abrupt_interpreter() {
    let runner = Runner { native: false };
    for source in [
        r#"var d={n:0,get enumerable(){'use strict';if(this.n!==0){throw 100;}this.n=1;return true;},get configurable(){'use strict';if(this.n!==1){throw 100;}this.n=2;return true;},get value(){'use strict';if(this.n!==2){throw 100;}this.n=3;return 7;},get writable(){'use strict';if(this.n!==3){throw 100;}this.n=4;return true;},get get(){'use strict';if(this.n!==4){throw 100;}this.n=5;return undefined;},get set(){'use strict';if(this.n!==5){throw 100;}this.n=6;return undefined;}};var o={};try{Object.defineProperty(o,'x',d);}catch(e){d.n===6&&e.name==='TypeError'&&!("x" in o);}"#,
        r#"var d={n:0,get enumerable(){'use strict';if(this.n!==0){throw 100;}this.n=1;return true;},get configurable(){'use strict';if(this.n!==1){throw 100;}this.n=2;return true;},get value(){'use strict';if(this.n!==2){throw 100;}this.n=3;throw 9;},get writable(){'use strict';this.n=999;return true;}};var o={};try{Object.defineProperty(o,'x',d);}catch(e){e===9&&d.n===3&&!("x" in o);}"#,
    ] {
        let answer = runner.execute(source);
        assert_eq!(field(&answer, "tag"), Value::Int(2));
        assert_eq!(field(&answer, "flag"), Value::Bool(true));
    }
    eprintln!("P7_ROW14 getter_order=1_2_3_4_5_6 abrupt_prefix=1_2_3 trace=0 aot=0");
}

#[test]
fn p7_row14_owner_handoff_interpreter() {
    let answer = Runner { native: false }.run(
        r#"var d={get get(){'use strict';delete this.get;var pad={};return function(){'use strict';return this;};}};var o={};Object.defineProperty(o,'x',d);var pad={};o.x===o;"#,
        Some(object_observer::Observer {
            force_gc: true,
            ..Default::default()
        }),
    );
    let marker = answer
        .iter()
        .position(|v| *v == Value::string("P7_OBSERVED"))
        .unwrap();
    assert_eq!(field(&answer[marker + 1], "tag"), Value::Int(2));
    assert_eq!(field(&answer[marker + 1], "flag"), Value::Bool(true));
    assert_eq!(answer[answer.len() - 2], Value::Int(0));
    eprintln!("P7_ROW14 owner_handoff forced_gc=true pending=0 reset_checked=true trace=0 aot=0");
}

#[test]
fn p7_row14_inactive_corruption_interpreter() {
    struct Corrupt(bool);
    impl vm::SourcePlugin for Corrupt {
        fn descriptor(&self) -> vm::SourceFrontendDescriptor {
            vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN)
        }
        fn parse_source(
            &self,
            source: &str,
            context: &vm::SourceFrontendContext<'_>,
        ) -> Result<vm::FrontendIr, vm::ParseError> {
            let mut ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
            let write = ir
                .functions
                .iter()
                .find(|f| f.name == "__compat_ref_write")
                .unwrap()
                .index;
            let validator = ir
                .functions
                .iter()
                .find(|f| f.name == "__compat_es5_descriptor_row_validator")
                .unwrap()
                .index;
            let sink = u16::try_from(ir.locals).unwrap();
            ir.locals += 1;
            let body = ir.function_impls.get_mut(&validator).unwrap();
            let carrier = body.param_slots[0];
            let row = body.param_slots[1];
            body.body_stmts.insert(
                0,
                vm::Stmt::Let {
                    index: sink,
                    declared_schema: Some(vm::TypeSchema::Number),
                    expr: vm::Expr::Call(
                        write,
                        vec![],
                        vec![
                            vm::Expr::Var(carrier),
                            vm::Expr::Div(
                                Box::new(vm::Expr::Add(
                                    Box::new(vm::Expr::Var(row)),
                                    Box::new(vm::Expr::Int(4)),
                                )),
                                Box::new(vm::Expr::Float(1.0)),
                            ),
                            vm::Expr::Float(if self.0 { 1.0 } else { 0.0 }),
                        ],
                        None,
                        None,
                    ),
                    line: 1,
                },
            );
            Ok(ir)
        }
    }
    for corrupt in [false, true] {
        let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
        let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
            "var o={};Object.defineProperty(o,'x',{value:7});o.x===7;",
            javascript_es5_flavor(),
            vm::CompileSourceFileOptions::new()
                .with_source_plugin(Box::leak(Box::new(Corrupt(corrupt))))
                .with_host_api_catalog(catalog.clone()),
        )
        .unwrap();
        let mut machine = compiled.into_vm().unwrap();
        machine.set_jit_config(vm::JitConfig {
            enabled: false,
            ..Default::default()
        });
        pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
            &mut machine,
            Default::default(),
        )
        .unwrap();
        let mut registry = HostFunctionRegistry::restricted();
        pd_vm_compat_frontends::runtime::array::typescript_array_host_module()
            .install_from_catalog(&mut registry, &catalog)
            .unwrap();
        pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module()
            .install_from_catalog(&mut registry, &catalog)
            .unwrap();
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module()
            .install_from_catalog(&mut registry, &catalog)
            .unwrap();
        pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module()
            .install_from_catalog(&mut registry, &catalog)
            .unwrap();
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module()
            .install_from_catalog(&mut registry, &catalog)
            .unwrap();
        registry.allow_builtin("math::floor").unwrap();
        registry.bind_vm_cached(&mut machine).unwrap();
        machine.set_fuel(10_000_000);
        let status = machine.run();
        assert!(!machine.jit_config().enabled);
        assert_eq!(machine.aot_exec_count(), 0);
        assert_eq!(machine.jit_native_exec_count(), 0);
        if corrupt {
            assert!(
                matches!(&status, Err(vm::VmError::HostError(message)) if message == "assertion failed"),
                "{status:?}"
            );
        } else {
            assert_eq!(status.unwrap(), VmStatus::Halted);
            assert_eq!(
                field(machine.stack().last().unwrap(), "flag"),
                Value::Bool(true)
            );
        }
        assert!(!machine.jit_config().enabled);
        assert_eq!(machine.jit_native_exec_count(), 0);
        assert_eq!(machine.aot_exec_count(), 0);
        assert!(machine.capture_certification_stats().unwrap().valid);
        machine.reset_for_reuse().unwrap();
        assert_eq!(machine.host_context().resource_count(), 0);
        eprintln!(
            "P7_ROW14 inactive_flag_corrupt={corrupt} exact_assertion_checked=true trace=0 aot=0"
        );
    }
}

// ---- Private descriptor-row validator: checked Int key-base contract ----
// The validator keeps its helper name/slot/two parameters, still runs the
// shared checked-row producer with no cache and empty consumer tails, and now
// returns the validated key base as Int instead of building a descriptor that
// this private caller discarded. These tests pin the new private contract:
// the analysis pin runs on the IR, the runtime pin proves the returned Int is
// the canonical checked String base of the controlled row with the exact
// length/content and that every scalar domain check still fails closed.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum KeyBaseFault {
    None,
    /// Fractional key slot: the exact numeric decoder must refuse before any
    /// checked_base/brand proof.
    KeyFrac,
    /// NaN key generation: same numeric-domain refusal.
    KeyNan,
    /// Metadata above the object tag domain.
    MetaMalformed,
    /// Key pair becomes the owner's live pair: resolves to OBJECT, not the
    /// required String brand.
    ForeignBrand,
}

struct KeyBaseProbe {
    fault: KeyBaseFault,
}

impl vm::SourcePlugin for KeyBaseProbe {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN)
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
        let (make, _) = probe_helper(&ir, "__compat_ref_make");
        let (read, _) = probe_helper(&ir, "__compat_ref_read");
        let (write, write_schema) = probe_helper(&ir, "__compat_ref_write");
        let (checked, _) = probe_helper(&ir, "__compat_es5_checked_base");
        let (decode_int, _) = probe_helper(&ir, "__compat_es5_decode_int");
        let (decode_u32, _) = probe_helper(&ir, "__compat_es5_decode_u32");
        let (validator, _) = probe_helper(&ir, "__compat_es5_descriptor_row_validator");
        let carrier = probe_carrier(&ir, make);
        let owner = probe_owner(&ir, "o");
        let base = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            probe_call(
                checked,
                vec![
                    vm::Expr::Var(carrier),
                    probe_get(owner, "ref_slot"),
                    probe_get(owner, "generation"),
                ],
            ),
        );
        let row = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            vm::Expr::Add(Box::new(vm::Expr::Var(base)), Box::new(vm::Expr::Int(5))),
        );
        probe_push(&mut ir, vm::Expr::Int(901));
        let cell = |offset: i64| {
            probe_num(vm::Expr::Add(
                Box::new(vm::Expr::Var(row)),
                Box::new(vm::Expr::Int(offset)),
            ))
        };
        let write_cell = |ir: &mut vm::FrontendIr, offset: i64, value: vm::Expr| {
            probe_let(
                ir,
                write_schema.clone(),
                probe_call(write, vec![vm::Expr::Var(carrier), cell(offset), value]),
            )
        };
        match self.fault {
            KeyBaseFault::None => {
                // The new private result must be the canonical checked String
                // base of this row's own key pair.
                let validated = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Int),
                    probe_call(validator, vec![vm::Expr::Var(carrier), vm::Expr::Var(row)]),
                );
                let key_slot = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Int),
                    probe_call(
                        decode_int,
                        vec![probe_call(read, vec![vm::Expr::Var(carrier), cell(0)])],
                    ),
                );
                let key_gen = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Int),
                    probe_call(
                        decode_u32,
                        vec![probe_call(read, vec![vm::Expr::Var(carrier), cell(1)])],
                    ),
                );
                let canonical = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Int),
                    probe_call(
                        checked,
                        vec![
                            vm::Expr::Var(carrier),
                            vm::Expr::Var(key_slot),
                            vm::Expr::Var(key_gen),
                        ],
                    ),
                );
                probe_push(
                    &mut ir,
                    vm::Expr::Eq(
                        Box::new(vm::Expr::Var(validated)),
                        Box::new(vm::Expr::Var(canonical)),
                    ),
                );
                // UTF-16 length and first unit of the controlled 'x' key,
                // read through the returned base.
                let length = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Number),
                    probe_call(
                        read,
                        vec![
                            vm::Expr::Var(carrier),
                            probe_num(vm::Expr::Add(
                                Box::new(vm::Expr::Var(validated)),
                                Box::new(vm::Expr::Int(2)),
                            )),
                        ],
                    ),
                );
                probe_push(
                    &mut ir,
                    vm::Expr::Eq(
                        Box::new(vm::Expr::Var(length)),
                        Box::new(vm::Expr::Float(1.0)),
                    ),
                );
                let unit = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Number),
                    probe_call(
                        read,
                        vec![
                            vm::Expr::Var(carrier),
                            probe_num(vm::Expr::Add(
                                Box::new(vm::Expr::Var(validated)),
                                Box::new(vm::Expr::Int(3)),
                            )),
                        ],
                    ),
                );
                probe_push(
                    &mut ir,
                    vm::Expr::Eq(
                        Box::new(vm::Expr::Var(unit)),
                        Box::new(vm::Expr::Float(120.0)),
                    ),
                );
            }
            KeyBaseFault::KeyFrac => {
                let _ = write_cell(
                    &mut ir,
                    0,
                    vm::Expr::Add(Box::new(cell(0)), Box::new(vm::Expr::Float(0.5))),
                );
            }
            KeyBaseFault::KeyNan => {
                let _ = write_cell(&mut ir, 1, vm::Expr::Float(f64::NAN));
            }
            KeyBaseFault::MetaMalformed => {
                let _ = write_cell(&mut ir, 2, vm::Expr::Float(1_000_000.0));
            }
            KeyBaseFault::ForeignBrand => {
                let _ = write_cell(&mut ir, 0, probe_num(probe_get(owner, "ref_slot")));
                let _ = write_cell(&mut ir, 1, probe_num(probe_get(owner, "generation")));
            }
        }
        if self.fault != KeyBaseFault::None {
            // The corrupted row must be refused inside the validator, so the
            // call never returns and this positive marker is never pushed.
            let _entered = probe_let(
                &mut ir,
                None,
                probe_call(validator, vec![vm::Expr::Var(carrier), vm::Expr::Var(row)]),
            );
            probe_push(&mut ir, vm::Expr::Int(902));
        }
        Ok(ir)
    }
}

#[test]
fn p7_descriptor_row_validator_checked_key_base_interpreter() {
    // Plain object literal row: no defineProperty/literal_key path, so this
    // isolates the validator's own returned Int from the caller wiring.
    const SOURCE: &str = "var o={x:7};o.x===7;";
    let outcome = run_row_contract(
        SOURCE,
        Box::leak(Box::new(KeyBaseProbe {
            fault: KeyBaseFault::None,
        })),
    );
    eprintln!(
        "P7_KEYBASE positive halted={} status={} ip={} ip_owner={} frames={:?} stack={:?}",
        outcome.halted, outcome.status, outcome.ip, outcome.ip_owner, outcome.frames, outcome.stack
    );
    assert!(
        outcome.halted,
        "positive key-base pipeline: {}",
        outcome.status
    );
    let tail = &outcome.stack[outcome.stack.len() - 4..];
    assert_eq!(tail[0], Value::Int(901), "pipeline marker");
    assert_eq!(
        tail[1..],
        [Value::Bool(true), Value::Bool(true), Value::Bool(true)],
        "returned Int must be the canonical checked String base with exact length/content"
    );
    for (fault, id) in [
        (KeyBaseFault::KeyFrac, 1),
        (KeyBaseFault::KeyNan, 2),
        (KeyBaseFault::MetaMalformed, 3),
        (KeyBaseFault::ForeignBrand, 4),
    ] {
        let outcome = run_row_contract(SOURCE, Box::leak(Box::new(KeyBaseProbe { fault })));
        eprintln!(
            "P7_KEYBASE negative={fault:?} id={id} halted={} status={} stack={:?}",
            outcome.halted, outcome.status, outcome.stack
        );
        assert!(
            !outcome.halted && outcome.status.contains("assertion failed"),
            "{fault:?} must refuse: {}",
            outcome.status
        );
        assert_eq!(
            outcome.stack.last(),
            Some(&Value::Bool(false)),
            "{fault:?}: the refused check's own condition is the last entry"
        );
        assert!(
            !outcome.stack.contains(&Value::Int(902)),
            "{fault:?}: the validator must be entered and never return"
        );
    }
}

#[test]
fn p7_descriptor_row_validator_key_base_contract_analysis() {
    struct Tap(std::sync::Mutex<Option<vm::FrontendIr>>);
    impl vm::SourcePlugin for Tap {
        fn descriptor(&self) -> vm::SourceFrontendDescriptor {
            vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN)
        }
        fn parse_source(
            &self,
            source: &str,
            context: &vm::SourceFrontendContext<'_>,
        ) -> Result<vm::FrontendIr, vm::ParseError> {
            let ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
            *self.0.lock().unwrap() = Some(ir.clone());
            Ok(ir)
        }
    }
    let tap = Box::leak(Box::new(Tap(Default::default())));
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    vm::compile_source_with_flavor_and_options_with_capture_certification(
        ROWDIFF_NUMBER_SOURCE,
        javascript_es5_flavor(),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(tap)
            .with_host_api_catalog(catalog),
    )
    .unwrap();
    let ir = tap.0.lock().unwrap();
    let ir = ir.as_ref().unwrap();
    let decl = ir
        .functions
        .iter()
        .find(|f| f.name == "__compat_es5_descriptor_row_validator")
        .expect("private validator");
    // Analysis-level pin: the private return is Int in both schema and
    // physical representation, never the old descriptor record.
    assert_eq!(decl.return_schema, Some(vm::TypeSchema::Int));
    assert_eq!(decl.return_type, vm::ValueType::Int);
    let (checked, _) = probe_helper(ir, "__compat_es5_checked_base");
    let body = ir.function_impls.get(&decl.index).expect("validator body");
    fn collect_lets(
        stmts: &[vm::Stmt],
        out: &mut std::collections::BTreeMap<u16, (vm::TypeSchema, vm::Expr)>,
    ) {
        for stmt in stmts {
            match stmt {
                vm::Stmt::Let {
                    index,
                    declared_schema: Some(schema),
                    expr,
                    ..
                } => {
                    assert!(
                        out.insert(*index, (schema.clone(), expr.clone())).is_none(),
                        "duplicate first definition"
                    );
                }
                vm::Stmt::IfElse {
                    then_branch,
                    else_branch,
                    ..
                } => {
                    collect_lets(then_branch, out);
                    collect_lets(else_branch, out);
                }
                vm::Stmt::While { body, .. } => collect_lets(body, out),
                _ => {}
            }
        }
    }
    let mut lets = std::collections::BTreeMap::new();
    collect_lets(&body.body_stmts, &mut lets);
    assert_eq!(
        lets.len(),
        15,
        "exactly the 15 checked scalar first definitions"
    );
    // The returned Int is the validated key base: defined as the checked
    // String identity's base local, placed after that base's own definition.
    let vm::Expr::Var(returned) = &body.body_expr else {
        panic!("validator returns a bare local: {:?}", body.body_expr);
    };
    let (_, def) = lets
        .get(returned)
        .unwrap_or_else(|| panic!("returned local has a first definition"));
    let vm::Expr::Var(checked_slot) = def else {
        panic!("returned key base snaps the checked identity local: {def:?}");
    };
    let (_, checked_def) = lets
        .get(checked_slot)
        .unwrap_or_else(|| panic!("checked identity local is defined"));
    assert!(
        matches!(checked_def, vm::Expr::Call(target, _, _, _, _) if *target == checked),
        "key base is assigned only after the String checked-base proof: {checked_def:?}"
    );
    eprintln!(
        "P7_KEYBASE_ANALYSIS schema=Int physical=Int first_defs={} returned={returned}",
        lets.len()
    );
}

// ---- P7 checked-row differential baseline (test-only; no production change) ----
// Baseline equivalence probe for the shared checked scalar row path: the
// ordinary descriptor-row validator (`__compat_es5_descriptor_row_validator`)
// and the real collector inline Object/Function row validation+tracing
// (`__compat_es5_heap_collect`, which reuses `decode_row` for every gray-drain
// property row) must accept and refuse the exact same physical row states.
// Every VM below compiles the same source text fresh, computes the real
// checked base/root handle from the actual JS value, mutates only the owned
// target row with the real `__compat_ref_write`, then invokes one existing
// consumer. Corrupt/drop runs are intentional negatives, never product
// failures: refusal must carry the exact consumer frame origin, not just a
// generic HostError.
const ROWDIFF_NUMBER_SOURCE: &str = "var o={};Object.defineProperty(o,'x',{value:7});o.x===7;";
const ROWDIFF_STRING_SOURCE: &str =
    "var o={};Object.defineProperty(o,'s',{value:'hi'});o.s==='hi';";
const ROWEDGE_SOURCE: &str = "var data={n:1};var g=function(){'use strict';this.__hits=1;return this;};var s=function(v){'use strict';this.n=v;};var owner={};Object.defineProperty(owner,'d',{value:data,enumerable:false});Object.defineProperty(owner,'x',{get:g,set:s,enumerable:false});var clean=owner.__hits===undefined;data=null;g=null;s=null;clean;";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RowDiffConsumer {
    Ordinary,
    Collector,
}

#[derive(Clone, Copy)]
struct RowDiffProbe {
    consumer: RowDiffConsumer,
    offset: i64,
    corrupt: bool,
}

#[derive(Clone, Copy)]
struct RowEdgeProbe;

fn probe_helper(ir: &vm::FrontendIr, name: &str) -> (u16, Option<vm::TypeSchema>) {
    let decl = ir
        .functions
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("missing production helper {name}"));
    (decl.index, decl.return_schema.clone())
}

fn probe_slot(ir: &mut vm::FrontendIr) -> u16 {
    let index = u16::try_from(ir.locals).expect("probe slot");
    ir.locals += 1;
    index
}

fn probe_let(ir: &mut vm::FrontendIr, schema: Option<vm::TypeSchema>, expr: vm::Expr) -> u16 {
    let index = probe_slot(ir);
    ir.stmts.push(vm::Stmt::Let {
        index,
        declared_schema: schema,
        expr,
        line: 1,
    });
    index
}

fn probe_push(ir: &mut vm::FrontendIr, expr: vm::Expr) {
    ir.stmts.push(vm::Stmt::Expr { expr, line: 1 });
}

fn probe_num(expr: vm::Expr) -> vm::Expr {
    vm::Expr::Div(Box::new(expr), Box::new(vm::Expr::Float(1.0)))
}

fn probe_call(index: u16, args: Vec<vm::Expr>) -> vm::Expr {
    vm::Expr::Call(index, Vec::new(), args, None, None)
}

fn probe_get(local: u16, name: &str) -> vm::Expr {
    vm::Expr::Call(
        BuiltinFunction::Get.call_index(),
        Vec::new(),
        vec![
            vm::Expr::Borrow(Box::new(vm::Expr::Var(local))),
            vm::Expr::String(name.into()),
        ],
        None,
        None,
    )
}

/// Get a field of an arbitrary record expression (the ordinary
/// `__compat_es5_get_own_property` result nests its descriptor record).
fn probe_field(record: vm::Expr, name: &str) -> vm::Expr {
    vm::Expr::Call(
        BuiltinFunction::Get.call_index(),
        Vec::new(),
        vec![record, vm::Expr::String(name.into())],
        None,
        None,
    )
}

fn probe_args(ir: &vm::FrontendIr, name: &str) -> Vec<Option<vm::TypeSchema>> {
    ir.functions
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("missing production helper {name}"))
        .arg_schemas
        .clone()
}

/// Build the private `__CompatEs5Primitive` String node value from the raw
/// numeric key pair of one controlled row. Both cells are read through the
/// real fixed carrier boundary and decoded with the production exact numeric
/// decoders; no unchecked cast or fresh JS string allocation is involved.
fn probe_row_key(
    ir: &mut vm::FrontendIr,
    read: u16,
    decode_int: u16,
    decode_u32: u16,
    carrier: u16,
    row: u16,
    schema: Option<vm::TypeSchema>,
) -> u16 {
    let pair_cell = |ir: &mut vm::FrontendIr, offset: i64| {
        probe_let(
            ir,
            Some(vm::TypeSchema::Number),
            probe_call(
                read,
                vec![
                    vm::Expr::Var(carrier),
                    probe_num(vm::Expr::Add(
                        Box::new(vm::Expr::Var(row)),
                        Box::new(vm::Expr::Int(offset)),
                    )),
                ],
            ),
        )
    };
    let slot_cell = pair_cell(ir, 0);
    let slot = probe_let(
        ir,
        Some(vm::TypeSchema::Int),
        probe_call(decode_int, vec![vm::Expr::Var(slot_cell)]),
    );
    let gen_cell = pair_cell(ir, 1);
    let generation = probe_let(
        ir,
        Some(vm::TypeSchema::Int),
        probe_call(decode_u32, vec![vm::Expr::Var(gen_cell)]),
    );
    let mut value = probe_call(BuiltinFunction::MapNew.call_index(), Vec::new());
    for (field, expr) in [
        ("tag", vm::Expr::Int(4)), // value::STRING
        ("number", vm::Expr::Float(0.0)),
        ("flag", vm::Expr::Bool(false)),
        ("ref_slot", vm::Expr::Var(slot)),
        ("generation", vm::Expr::Var(generation)),
    ] {
        value = probe_call(
            BuiltinFunction::Set.call_index(),
            vec![value, vm::Expr::String(field.into()), expr],
        );
    }
    probe_let(ir, schema, value)
}

/// Migrated descriptor observation. The original validator call still runs
/// first so every refusal origin is unchanged and its result is ignored
/// return-agnostically (an untyped local accepts both the old Descriptor and
/// the new Int contract). The descriptor then comes from the real ordinary
/// `__compat_es5_get_own_property`, which re-runs the unchanged `decode_row`
/// on the same raw row and returns its declared `{found, descriptor, index}`.
#[allow(clippy::too_many_arguments)]
fn probe_ordinary_descriptor(
    ir: &mut vm::FrontendIr,
    carrier: u16,
    owner: u16,
    row: u16,
    validator: u16,
    read: u16,
    decode_int: u16,
    decode_u32: u16,
    own: u16,
    own_schema: Option<vm::TypeSchema>,
    value_schema: Option<vm::TypeSchema>,
) -> u16 {
    let _ignored = probe_let(
        ir,
        None,
        probe_call(validator, vec![vm::Expr::Var(carrier), vm::Expr::Var(row)]),
    );
    let key = probe_row_key(ir, read, decode_int, decode_u32, carrier, row, value_schema);
    probe_let(
        ir,
        own_schema,
        probe_call(
            own,
            vec![
                vm::Expr::Var(carrier),
                vm::Expr::Var(owner),
                vm::Expr::Var(key),
            ],
        ),
    )
}

/// Push the selected descriptor fields from an ordinary own-property result.
fn probe_push_descriptor_fields(ir: &mut vm::FrontendIr, own_result: u16, names: &[&str]) {
    for name in names {
        probe_push(
            ir,
            probe_field(probe_field(vm::Expr::Var(own_result), "descriptor"), name),
        );
    }
}

/// Same migrated observation for a specifically addressed row: the original
/// validator still runs first, then the test-only fixed-row view (cloned from
/// the real ordinary helper) decodes exactly this row through the unchanged
/// `decode_row`, which is required when a raw duplicate-key rewrite would make
/// content lookup return an earlier row.
#[allow(clippy::too_many_arguments)]
fn probe_fixed_row_descriptor(
    ir: &mut vm::FrontendIr,
    carrier: u16,
    owner: u16,
    row_expr: vm::Expr,
    prop: i64,
    validator: u16,
    read: u16,
    decode_int: u16,
    decode_u32: u16,
    view: u16,
    view_schema: Option<vm::TypeSchema>,
    value_schema: Option<vm::TypeSchema>,
) -> u16 {
    let row = probe_let(ir, Some(vm::TypeSchema::Int), row_expr);
    let _ignored = probe_let(
        ir,
        None,
        probe_call(validator, vec![vm::Expr::Var(carrier), vm::Expr::Var(row)]),
    );
    let key = probe_row_key(ir, read, decode_int, decode_u32, carrier, row, value_schema);
    probe_let(
        ir,
        view_schema,
        probe_call(
            view,
            vec![
                vm::Expr::Var(carrier),
                vm::Expr::Var(owner),
                vm::Expr::Var(key),
                vm::Expr::Int(prop),
            ],
        ),
    )
}

/// Test-only fixed-row view derived structurally from the real
/// `__compat_es5_get_own_property` implementation: the body, header/key
/// decoding, row decoder, return fields and every check are preserved; only
/// the scan cursor's initial value changes from constant 0 to a fourth
/// validated row-index parameter, so a raw duplicate-key rewrite still reads
/// exactly the requested row instead of the first content match.
fn probe_row_view(
    ir: &mut vm::FrontendIr,
) -> (u16, Option<vm::TypeSchema>, Vec<Option<vm::TypeSchema>>) {
    let mut decl = ir
        .functions
        .iter()
        .find(|f| f.name == "__compat_es5_get_own_property")
        .expect("ordinary own-property helper")
        .clone();
    let mut body = ir
        .function_impls
        .get(&decl.index)
        .expect("ordinary own-property body")
        .clone();
    let index = ir
        .functions
        .iter()
        .map(|f| f.index)
        .max()
        .expect("declared helper")
        .checked_add(1)
        .expect("fresh helper index");
    let param = probe_slot(ir);
    let loop_at = body
        .body_stmts
        .iter()
        .position(|stmt| {
            matches!(
                stmt,
                vm::Stmt::While { condition: vm::Expr::And(_, right), .. }
                    if matches!(right.as_ref(), vm::Expr::Lt(left, _) if matches!(left.as_ref(), vm::Expr::Var(_)))
            )
        })
        .expect("row scan loop");
    let (cursor, count) = match &body.body_stmts[loop_at] {
        vm::Stmt::While { condition, .. } => match condition {
            vm::Expr::And(_, right) => match right.as_ref() {
                vm::Expr::Lt(left, count) => {
                    let vm::Expr::Var(cursor) = left.as_ref() else {
                        panic!("row scan cursor")
                    };
                    let vm::Expr::Var(count) = count.as_ref() else {
                        panic!("row scan count")
                    };
                    (*cursor, *count)
                }
                other => panic!("row scan bound: {other:?}"),
            },
            other => panic!("row scan guard: {other:?}"),
        },
        _ => unreachable!(),
    };
    let init = body
        .body_stmts
        .iter_mut()
        .find_map(|stmt| match stmt {
            vm::Stmt::Let { index, expr, .. }
                if *index == cursor && matches!(expr, vm::Expr::Int(0)) =>
            {
                Some(expr)
            }
            _ => None,
        })
        .expect("zero cursor initializer");
    *init = vm::Expr::Var(param);
    let ranged = vm::Expr::And(
        Box::new(vm::Expr::Not(Box::new(vm::Expr::Lt(
            Box::new(vm::Expr::Var(param)),
            Box::new(vm::Expr::Int(0)),
        )))),
        Box::new(vm::Expr::Lt(
            Box::new(vm::Expr::Var(param)),
            Box::new(vm::Expr::Var(count)),
        )),
    );
    body.body_stmts.insert(
        loop_at,
        vm::Stmt::Expr {
            expr: vm::Expr::Call(
                BuiltinFunction::Assert.call_index(),
                Vec::new(),
                vec![ranged],
                None,
                None,
            ),
            line: 1,
        },
    );
    body.param_slots.push(param);
    decl.index = index;
    decl.name = "__probe_row_view".into();
    decl.arity = 4;
    decl.args.push("row_index".into());
    decl.arg_schemas.push(Some(vm::TypeSchema::Int));
    let args = decl.args.clone();
    ir.functions.push(decl);
    ir.function_impls.insert(index, body);
    ir.stmts.push(vm::Stmt::FuncDecl {
        name: "__probe_row_view".into(),
        index,
        arity: 4,
        args,
        exported: false,
        has_impl: true,
        line: 1,
    });
    let decl = ir
        .functions
        .iter()
        .find(|f| f.index == index)
        .expect("row view declaration");
    (index, decl.return_schema.clone(), decl.arg_schemas.clone())
}

fn probe_carrier(ir: &vm::FrontendIr, make: u16) -> u16 {
    if let Some((_, slot)) = ir
        .local_bindings
        .iter()
        .find(|(name, _)| name.as_str() == "__compat_es5_realm")
    {
        return *slot;
    }
    ir.stmts
        .iter()
        .find_map(|stmt| match stmt {
            vm::Stmt::Let {
                index,
                expr: vm::Expr::Call(target, _, _, _, _),
                ..
            } if *target == make => Some(*index),
            _ => None,
        })
        .expect("real carrier")
}

fn probe_owner(ir: &vm::FrontendIr, name: &str) -> u16 {
    ir.local_bindings
        .iter()
        .find(|(binding, _)| binding.as_str() == name)
        .map(|(_, slot)| *slot)
        .unwrap_or_else(|| panic!("real owner binding {name}"))
}

impl vm::SourcePlugin for RowDiffProbe {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN)
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
        let (make, _) = probe_helper(&ir, "__compat_ref_make");
        let (read, _) = probe_helper(&ir, "__compat_ref_read");
        let (write, write_schema) = probe_helper(&ir, "__compat_ref_write");
        let (checked, _) = probe_helper(&ir, "__compat_es5_checked_base");
        let (decode_int, _) = probe_helper(&ir, "__compat_es5_decode_int");
        let (decode_u32, _) = probe_helper(&ir, "__compat_es5_decode_u32");
        let (observe, _) = probe_helper(&ir, "__compat_es5_heap_observe");
        let (validator, _) = probe_helper(&ir, "__compat_es5_descriptor_row_validator");
        let (own, own_schema) = probe_helper(&ir, "__compat_es5_get_own_property");
        let own_args = probe_args(&ir, "__compat_es5_get_own_property");
        let carrier = probe_carrier(&ir, make);
        let owner = probe_owner(&ir, "o");
        let base = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            probe_call(
                checked,
                vec![
                    vm::Expr::Var(carrier),
                    probe_get(owner, "ref_slot"),
                    probe_get(owner, "generation"),
                ],
            ),
        );
        // Production layout facts only (src/javascript/heap.rs):
        // OBJECT_HEADER_CELLS = 5 cells precede the first property row and
        // OBJECT_CELLS_PER_PROPERTY = 7 cells hold one property row. The
        // control validator call below proves the computed address at
        // runtime before any mutation happens.
        let row = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            vm::Expr::Add(Box::new(vm::Expr::Var(base)), Box::new(vm::Expr::Int(5))),
        );
        // Self-describing pipeline marker: offset*100 + consumer tag
        // (ordinary 10, collector 20) + mode (control 0, corrupt 1). Every
        // later stack value sits above it, so the stack itself records how
        // far the pipeline ran before any refusal.
        let marker = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            vm::Expr::Int(
                self.offset * 100
                    + match self.consumer {
                        RowDiffConsumer::Ordinary => 10,
                        RowDiffConsumer::Collector => 20,
                    }
                    + i64::from(self.corrupt),
            ),
        );
        probe_push(&mut ir, vm::Expr::Var(marker));
        // Second marker after the addressing proof and the matched write,
        // so a refusal stack shows whether the consumer call was reached.
        let marker2 = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            vm::Expr::Int(self.offset * 100 + 40 + i64::from(self.corrupt)),
        );
        if self.corrupt {
            // Addressing proof: the untouched row must validate before the
            // intentional corruption lands. After the write below, the next
            // consumer call is the first observer of the corrupted row.
            let _sanity = probe_let(
                &mut ir,
                None,
                probe_call(validator, vec![vm::Expr::Var(carrier), vm::Expr::Var(row)]),
            );
        }
        // Matched write in both modes: control stores the canonical inactive
        // value, corrupt stores the intended invalid one. Only this constant
        // differs between the two pipelines.
        let _wb = probe_let(
            &mut ir,
            write_schema.clone(),
            probe_call(
                write,
                vec![
                    vm::Expr::Var(carrier),
                    probe_num(vm::Expr::Add(
                        Box::new(vm::Expr::Var(row)),
                        Box::new(vm::Expr::Int(self.offset)),
                    )),
                    vm::Expr::Float(if self.corrupt { 1.0 } else { 0.0 }),
                ],
            ),
        );
        probe_push(&mut ir, vm::Expr::Var(marker2));
        match self.consumer {
            RowDiffConsumer::Ordinary => {
                let coll_before = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Int),
                    probe_call(
                        observe,
                        vec![vm::Expr::Var(carrier), vm::Expr::Int(5), vm::Expr::Int(0)],
                    ),
                );
                // The counter push precedes the consumer call, so a refusal
                // stack ending here proves the validator was entered and
                // never returned.
                probe_push(&mut ir, vm::Expr::Var(coll_before));
                let desc = probe_ordinary_descriptor(
                    &mut ir,
                    carrier,
                    owner,
                    row,
                    validator,
                    read,
                    decode_int,
                    decode_u32,
                    own,
                    own_schema.clone(),
                    own_args[1].clone(),
                );
                probe_push_descriptor_fields(
                    &mut ir,
                    desc,
                    &["present", "writable", "enumerable", "configurable"],
                );
                let coll_after = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Int),
                    probe_call(
                        observe,
                        vec![vm::Expr::Var(carrier), vm::Expr::Int(5), vm::Expr::Int(0)],
                    ),
                );
                probe_push(&mut ir, vm::Expr::Var(coll_after));
            }
            RowDiffConsumer::Collector => {
                // Suppress any incidental pressure collection before the
                // explicit one: HDR_ALLOCATIONS_SINCE_GC = 10 reset to zero,
                // so the only collection in this pipeline is the real
                // `__compat_es5_heap_collect` call below.
                let _pressure = probe_let(
                    &mut ir,
                    write_schema,
                    probe_call(
                        write,
                        vec![
                            vm::Expr::Var(carrier),
                            vm::Expr::Float(10.0),
                            vm::Expr::Float(0.0),
                        ],
                    ),
                );
                let coll_before = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Int),
                    probe_call(
                        observe,
                        vec![vm::Expr::Var(carrier), vm::Expr::Int(5), vm::Expr::Int(0)],
                    ),
                );
                let pending = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Int),
                    probe_call(
                        observe,
                        vec![vm::Expr::Var(carrier), vm::Expr::Int(8), vm::Expr::Int(0)],
                    ),
                );
                probe_push(&mut ir, vm::Expr::Var(coll_before));
                probe_push(&mut ir, vm::Expr::Var(pending));
                let (root_enter, _) = probe_helper(&ir, "__compat_es5_root_enter");
                let (root_write, root_write_schema) = probe_helper(&ir, "__compat_es5_root_write");
                let (root_leave, root_leave_schema) = probe_helper(&ir, "__compat_es5_root_leave");
                let (collect, collect_schema) = probe_helper(&ir, "__compat_es5_heap_collect");
                let frame = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Int),
                    probe_call(root_enter, vec![vm::Expr::Var(carrier), vm::Expr::Int(1)]),
                );
                let _ack = probe_let(
                    &mut ir,
                    root_write_schema,
                    probe_call(
                        root_write,
                        vec![
                            vm::Expr::Var(carrier),
                            vm::Expr::Var(frame),
                            vm::Expr::Int(0),
                            vm::Expr::Var(owner),
                        ],
                    ),
                );
                // Entered-collector marker: pushed after the owner root is
                // pinned and before the explicit collect, so a refusal stack
                // ending here proves the real collector was entered and
                // never returned.
                let marker3 = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Int),
                    vm::Expr::Int(self.offset * 100 + 60 + i64::from(self.corrupt)),
                );
                probe_push(&mut ir, vm::Expr::Var(marker3));
                let reclaimed = probe_let(
                    &mut ir,
                    collect_schema,
                    probe_call(collect, vec![vm::Expr::Var(carrier)]),
                );
                let coll_after = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Int),
                    probe_call(
                        observe,
                        vec![vm::Expr::Var(carrier), vm::Expr::Int(5), vm::Expr::Int(0)],
                    ),
                );
                probe_push(&mut ir, vm::Expr::Var(coll_after));
                probe_push(&mut ir, vm::Expr::Var(reclaimed));
                let desc = probe_ordinary_descriptor(
                    &mut ir,
                    carrier,
                    owner,
                    row,
                    validator,
                    read,
                    decode_int,
                    decode_u32,
                    own,
                    own_schema.clone(),
                    own_args[1].clone(),
                );
                probe_push_descriptor_fields(
                    &mut ir,
                    desc,
                    &["present", "writable", "enumerable", "configurable"],
                );
                let _leave = probe_let(
                    &mut ir,
                    root_leave_schema,
                    probe_call(
                        root_leave,
                        vec![vm::Expr::Var(carrier), vm::Expr::Var(frame)],
                    ),
                );
            }
        }
        Ok(ir)
    }
}

impl vm::SourcePlugin for RowEdgeProbe {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN)
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
        let (make, _) = probe_helper(&ir, "__compat_ref_make");
        let (read, _) = probe_helper(&ir, "__compat_ref_read");
        let (write, write_schema) = probe_helper(&ir, "__compat_ref_write");
        let (checked, _) = probe_helper(&ir, "__compat_es5_checked_base");
        let (decode_int, _) = probe_helper(&ir, "__compat_es5_decode_int");
        let (decode_u32, _) = probe_helper(&ir, "__compat_es5_decode_u32");
        let (observe, _) = probe_helper(&ir, "__compat_es5_heap_observe");
        let (validator, _) = probe_helper(&ir, "__compat_es5_descriptor_row_validator");
        let (own, own_schema) = probe_helper(&ir, "__compat_es5_get_own_property");
        let own_args = probe_args(&ir, "__compat_es5_get_own_property");
        let (root_enter, _) = probe_helper(&ir, "__compat_es5_root_enter");
        let (root_write, root_write_schema) = probe_helper(&ir, "__compat_es5_root_write");
        let (root_leave, root_leave_schema) = probe_helper(&ir, "__compat_es5_root_leave");
        let (collect, collect_schema) = probe_helper(&ir, "__compat_es5_heap_collect");
        let carrier = probe_carrier(&ir, make);
        let owner = probe_owner(&ir, "owner");
        let base = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            probe_call(
                checked,
                vec![
                    vm::Expr::Var(carrier),
                    probe_get(owner, "ref_slot"),
                    probe_get(owner, "generation"),
                ],
            ),
        );
        // Same production layout facts: header 5, row stride 7. Owner holds
        // exactly two properties, so 'd' lives at base+5 and 'x' at base+12.
        // Both validator calls below prove the addresses before collection.
        let row_d = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            vm::Expr::Add(Box::new(vm::Expr::Var(base)), Box::new(vm::Expr::Int(5))),
        );
        let row_x = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            vm::Expr::Add(Box::new(vm::Expr::Var(base)), Box::new(vm::Expr::Int(12))),
        );
        // Edge pipeline marker first: 701 identifies this retain probe.
        // Every later stack value sits above it.
        let edge_marker = probe_let(&mut ir, Some(vm::TypeSchema::Int), vm::Expr::Int(701));
        probe_push(&mut ir, vm::Expr::Var(edge_marker));
        let read_num = |ir: &mut vm::FrontendIr, addr: vm::Expr| {
            probe_let(
                ir,
                Some(vm::TypeSchema::Number),
                probe_call(read, vec![vm::Expr::Var(carrier), probe_num(addr)]),
            )
        };
        let packed_pre = {
            let cell = read_num(
                &mut ir,
                vm::Expr::Add(Box::new(vm::Expr::Var(base)), Box::new(vm::Expr::Int(2))),
            );
            probe_let(
                &mut ir,
                Some(vm::TypeSchema::Int),
                probe_call(decode_int, vec![vm::Expr::Var(cell)]),
            )
        };
        probe_push(&mut ir, vm::Expr::Var(packed_pre));
        let desc_d = probe_ordinary_descriptor(
            &mut ir,
            carrier,
            owner,
            row_d,
            validator,
            read,
            decode_int,
            decode_u32,
            own,
            own_schema.clone(),
            own_args[1].clone(),
        );
        let desc_x = probe_ordinary_descriptor(
            &mut ir,
            carrier,
            owner,
            row_x,
            validator,
            read,
            decode_int,
            decode_u32,
            own,
            own_schema.clone(),
            own_args[1].clone(),
        );
        for (desc, names) in [
            (
                desc_d,
                ["present", "writable", "enumerable", "configurable"].as_slice(),
            ),
            (desc_x, ["present", "enumerable", "configurable"].as_slice()),
        ] {
            probe_push_descriptor_fields(&mut ir, desc, names);
        }
        // Referent identities come from the raw row cells (Ints, never
        // roots): data slot/gen at row+5/6, getter at row+3/4, setter at
        // row+5/6, mirroring decode_row's cell map.
        let decode_cell = |ir: &mut vm::FrontendIr, decode: u16, row: u16, off: i64| {
            let cell = read_num(
                ir,
                vm::Expr::Add(Box::new(vm::Expr::Var(row)), Box::new(vm::Expr::Int(off))),
            );
            probe_let(
                ir,
                Some(vm::TypeSchema::Int),
                probe_call(decode, vec![vm::Expr::Var(cell)]),
            )
        };
        let d_slot = decode_cell(&mut ir, decode_int, row_d, 5);
        let d_gen = decode_cell(&mut ir, decode_u32, row_d, 6);
        let g_slot = decode_cell(&mut ir, decode_int, row_x, 3);
        let g_gen = decode_cell(&mut ir, decode_u32, row_x, 4);
        let s_slot = decode_cell(&mut ir, decode_int, row_x, 5);
        let s_gen = decode_cell(&mut ir, decode_u32, row_x, 6);
        let packed_mid = {
            let cell = read_num(
                &mut ir,
                vm::Expr::Add(Box::new(vm::Expr::Var(base)), Box::new(vm::Expr::Int(2))),
            );
            probe_let(
                &mut ir,
                Some(vm::TypeSchema::Int),
                probe_call(decode_int, vec![vm::Expr::Var(cell)]),
            )
        };
        probe_push(&mut ir, vm::Expr::Var(packed_mid));
        let coll_before = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            probe_call(
                observe,
                vec![vm::Expr::Var(carrier), vm::Expr::Int(5), vm::Expr::Int(0)],
            ),
        );
        let pending = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            probe_call(
                observe,
                vec![vm::Expr::Var(carrier), vm::Expr::Int(8), vm::Expr::Int(0)],
            ),
        );
        probe_push(&mut ir, vm::Expr::Var(coll_before));
        probe_push(&mut ir, vm::Expr::Var(pending));
        let _pressure = probe_let(
            &mut ir,
            write_schema,
            probe_call(
                write,
                vec![
                    vm::Expr::Var(carrier),
                    vm::Expr::Float(10.0),
                    vm::Expr::Float(0.0),
                ],
            ),
        );
        let frame = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            probe_call(root_enter, vec![vm::Expr::Var(carrier), vm::Expr::Int(1)]),
        );
        // Only the owner is pinned. The data/getter/setter boxes stay
        // reachable exclusively through the owned rows, so their survival
        // proves the collector traced the real edges.
        let _ack = probe_let(
            &mut ir,
            root_write_schema,
            probe_call(
                root_write,
                vec![
                    vm::Expr::Var(carrier),
                    vm::Expr::Var(frame),
                    vm::Expr::Int(0),
                    vm::Expr::Var(owner),
                ],
            ),
        );
        // Entered-collector marker, mirroring the row probes: pushed after
        // the owner root pinning and before the explicit collect.
        let edge_marker3 = probe_let(&mut ir, Some(vm::TypeSchema::Int), vm::Expr::Int(711));
        probe_push(&mut ir, vm::Expr::Var(edge_marker3));
        let reclaimed = probe_let(
            &mut ir,
            collect_schema,
            probe_call(collect, vec![vm::Expr::Var(carrier)]),
        );
        let coll_after = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            probe_call(
                observe,
                vec![vm::Expr::Var(carrier), vm::Expr::Int(5), vm::Expr::Int(0)],
            ),
        );
        probe_push(&mut ir, vm::Expr::Var(coll_after));
        probe_push(&mut ir, vm::Expr::Var(reclaimed));
        let desc_d2 = probe_ordinary_descriptor(
            &mut ir,
            carrier,
            owner,
            row_d,
            validator,
            read,
            decode_int,
            decode_u32,
            own,
            own_schema.clone(),
            own_args[1].clone(),
        );
        let desc_x2 = probe_ordinary_descriptor(
            &mut ir,
            carrier,
            owner,
            row_x,
            validator,
            read,
            decode_int,
            decode_u32,
            own,
            own_schema,
            own_args[1].clone(),
        );
        for (desc, names) in [
            (
                desc_d2,
                ["present", "writable", "enumerable", "configurable"].as_slice(),
            ),
            (
                desc_x2,
                ["present", "enumerable", "configurable"].as_slice(),
            ),
        ] {
            probe_push_descriptor_fields(&mut ir, desc, names);
        }
        // Liveness plus brand through the real checked base: KIND_OBJECT
        // for the data box, KIND_FUNCTION for both accessor boxes.
        for (slot, gen_) in [(d_slot, d_gen), (g_slot, g_gen), (s_slot, s_gen)] {
            let live = probe_let(
                &mut ir,
                Some(vm::TypeSchema::Int),
                probe_call(
                    checked,
                    vec![
                        vm::Expr::Var(carrier),
                        vm::Expr::Var(slot),
                        vm::Expr::Var(gen_),
                    ],
                ),
            );
            let kind_cell = read_num(&mut ir, vm::Expr::Var(live));
            let brand = probe_let(
                &mut ir,
                Some(vm::TypeSchema::Int),
                probe_call(decode_int, vec![vm::Expr::Var(kind_cell)]),
            );
            probe_push(&mut ir, vm::Expr::Var(brand));
        }
        let packed_post = {
            let cell = read_num(
                &mut ir,
                vm::Expr::Add(Box::new(vm::Expr::Var(base)), Box::new(vm::Expr::Int(2))),
            );
            probe_let(
                &mut ir,
                Some(vm::TypeSchema::Int),
                probe_call(decode_int, vec![vm::Expr::Var(cell)]),
            )
        };
        probe_push(&mut ir, vm::Expr::Var(packed_post));
        let _leave = probe_let(
            &mut ir,
            root_leave_schema,
            probe_call(
                root_leave,
                vec![vm::Expr::Var(carrier), vm::Expr::Var(frame)],
            ),
        );
        Ok(ir)
    }
}

struct RowContractOutcome {
    status: String,
    halted: bool,
    stack: Vec<Value>,
    frames: Vec<String>,
    ip: usize,
    ip_owner: String,
    /// Sampled Int parameters of an active test-only stop helper, when the
    /// pipeline installed one; `None` for every ordinary/collector pipeline.
    samples: Option<Vec<i64>>,
}

fn run_row_contract(source: &str, plugin: &'static dyn vm::SourcePlugin) -> RowContractOutcome {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        javascript_es5_flavor(),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(plugin)
            .with_host_api_catalog(catalog.clone()),
    )
    .unwrap_or_else(|error| panic!("contract source must compile: {source}\n{error:?}"));
    let mut machine = compiled.into_vm().expect("certified VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..Default::default()
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        Default::default(),
    )
    .expect("owner");
    let mut registry = HostFunctionRegistry::restricted();
    pd_vm_compat_frontends::runtime::array::typescript_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .unwrap();
    pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .unwrap();
    pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .unwrap();
    pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .unwrap();
    pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module()
        .install_from_catalog(&mut registry, &catalog)
        .unwrap();
    registry.allow_builtin("math::floor").unwrap();
    // The ordinary realm's bootstrap publishes its fixed ASCII strings through
    // the Core bytes initializer, so this capability is part of the emitted
    // program (the Object.keys and D08 harnesses already allow the same
    // builtin). Without it every row-contract pipeline fails at bind time.
    registry
        .allow_builtin("bytes::from_utf8")
        .expect("bootstrap strings");
    registry.bind_vm_cached(&mut machine).unwrap();
    machine.set_fuel(10_000_000);
    let status = machine.run();
    let halted = matches!(status, Ok(VmStatus::Halted));
    let status_text = format!("{status:?}");
    // A default-off test helper that stops through its own parameters carries
    // its sampled Int counters into this frame; every other pipeline has none.
    let frame_snapshots = machine.execution_frames();
    let samples = active_diagnostic_prototype(&machine, &frame_snapshots, 5, 5, &[])
        .and_then(|prototype| frame_params(&machine, &frame_snapshots, prototype))
        .map(|params| {
            (0..params.len())
                .filter_map(|at| int_param(&params, at))
                .collect::<Vec<i64>>()
        });
    let program = machine.program();
    let debug = machine.debug_info();
    let frames: Vec<String> = machine
        .execution_frames()
        .iter()
        .map(|frame| {
            frame
                .prototype_id
                .map(|id| {
                    let proto = &program.callable_prototypes[id as usize];
                    match &proto.target {
                        vm::CallableTarget::ScriptFunction(index) => debug
                            .and_then(|info| info.functions.get(*index as usize))
                            .map(|function| function.name.clone())
                            .unwrap_or_else(|| format!("script#{index}")),
                        target => format!("{target:?}"),
                    }
                })
                .unwrap_or_else(|| "<no-prototype>".to_owned())
        })
        .collect();
    let ip = machine.ip();
    let ip_owner = program
        .script_functions
        .iter()
        .position(|function| function.entry_ip as usize <= ip && ip < function.end_ip as usize)
        .and_then(|index| {
            debug
                .and_then(|info| info.functions.get(index))
                .map(|function| function.name.clone())
        })
        .unwrap_or_else(|| "<no-owner>".to_owned());
    let stack = machine.stack().to_vec();
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    assert!(
        machine
            .capture_certification_stats()
            .is_some_and(|facts| facts.valid)
    );
    machine.reset_for_reuse().unwrap();
    assert_eq!(machine.host_context().resource_count(), 0);
    RowContractOutcome {
        status: status_text,
        halted,
        stack,
        frames,
        ip,
        samples,
        ip_owner,
    }
}

fn check_rowdiff_case(source: &str, case: &str, offset: i64) {
    for consumer in [RowDiffConsumer::Ordinary, RowDiffConsumer::Collector] {
        for corrupt in [false, true] {
            let outcome = run_row_contract(
                source,
                Box::leak(Box::new(RowDiffProbe {
                    consumer,
                    offset,
                    corrupt,
                })),
            );
            let intentional = corrupt;
            let marker_want = offset * 100
                + match consumer {
                    RowDiffConsumer::Ordinary => 10,
                    RowDiffConsumer::Collector => 20,
                }
                + i64::from(corrupt);
            // Log-first: refusal unwinds guest frames, so ip/frames are
            // residual. The origin proof is the entered-callsite markers
            // plus the stack below, recorded even when a later assertion
            // fails.
            eprintln!(
                "P7_ROWDIFF case={case} consumer={consumer:?} corrupt={corrupt} intentional_negative={intentional} status={} ip={} frames={:?} ip_owner={} stack_len={} stack={:?}",
                outcome.status,
                outcome.ip,
                outcome.frames,
                outcome.ip_owner,
                outcome.stack.len(),
                outcome.stack,
            );
            let boxed_true = |value: &Value| {
                assert_eq!(field(value, "tag"), Value::Int(2), "boxed boolean tag");
                assert_eq!(
                    field(value, "flag"),
                    Value::Bool(true),
                    "script value stays true"
                );
            };
            let marker2_want = offset * 100 + 40 + i64::from(corrupt);
            let marker3_want = offset * 100 + 60 + i64::from(corrupt);
            // Refusal unwinds guest frames (ip/frames go residual, logged
            // above), so origin is proven structurally below: the entered
            // consumer call never returned. The trailing Bool(false) on
            // refusal stacks is the refused assert's own condition, left by
            // the unwind; every row-guard assert here fails closed as false.
            match (consumer, corrupt) {
                (RowDiffConsumer::Ordinary, false) => {
                    assert!(
                        outcome.halted,
                        "ordinary control must halt: {}",
                        outcome.status
                    );
                    let tail = &outcome.stack[outcome.stack.len() - 9..];
                    boxed_true(&tail[0]);
                    assert_eq!(tail[1], Value::Int(marker_want), "pipeline marker");
                    assert_eq!(tail[2], Value::Int(marker2_want), "post-write marker");
                    assert_eq!(tail[3], tail[8], "no incidental GC in ordinary path");
                    assert_eq!(tail[4], Value::Int(51), "data descriptor marker");
                    assert_eq!(
                        tail[5..8],
                        [Value::Bool(false), Value::Bool(false), Value::Bool(false)],
                        "default attributes"
                    );
                }
                (RowDiffConsumer::Ordinary, true) => {
                    assert!(
                        !outcome.halted && outcome.status.contains("assertion failed"),
                        "intentional-negative ordinary refusal: {}",
                        outcome.status
                    );
                    let tail = &outcome.stack[outcome.stack.len() - 5..];
                    boxed_true(&tail[0]);
                    assert_eq!(tail[1], Value::Int(marker_want), "pipeline marker");
                    assert_eq!(tail[2], Value::Int(marker2_want), "post-write marker");
                    assert!(
                        matches!(tail[3], Value::Int(_)),
                        "reached validator callsite: {tail:?}"
                    );
                    assert_eq!(
                        tail[4],
                        Value::Bool(false),
                        "refused assert condition debris: {tail:?}"
                    );
                }
                (RowDiffConsumer::Collector, false) => {
                    assert!(
                        outcome.halted,
                        "collector control must halt: {}",
                        outcome.status
                    );
                    let tail = &outcome.stack[outcome.stack.len() - 12..];
                    boxed_true(&tail[0]);
                    assert_eq!(tail[1], Value::Int(marker_want), "pipeline marker");
                    assert_eq!(tail[2], Value::Int(marker2_want), "post-write marker");
                    match (&tail[3], &tail[4], &tail[6]) {
                        (Value::Int(before), Value::Int(pending), Value::Int(after)) => {
                            assert_eq!(*pending, 0, "no pending nodes at explicit collect");
                            assert_eq!(
                                *after,
                                *before + 1,
                                "exactly one real collection, the explicit one"
                            );
                        }
                        other => panic!("collection counter evidence: {other:?}"),
                    }
                    assert_eq!(
                        tail[5],
                        Value::Int(marker3_want),
                        "entered-collector marker"
                    );
                    assert_eq!(tail[8], Value::Int(51), "data descriptor marker");
                    assert_eq!(
                        tail[9..12],
                        [Value::Bool(false), Value::Bool(false), Value::Bool(false)],
                        "default attributes"
                    );
                }
                (RowDiffConsumer::Collector, true) => {
                    assert!(
                        !outcome.halted && outcome.status.contains("assertion failed"),
                        "intentional-negative collector refusal: {}",
                        outcome.status
                    );
                    let tail = &outcome.stack[outcome.stack.len() - 7..];
                    boxed_true(&tail[0]);
                    assert_eq!(tail[1], Value::Int(marker_want), "pipeline marker");
                    assert_eq!(tail[2], Value::Int(marker2_want), "post-write marker");
                    assert!(
                        matches!((&tail[3], &tail[4]), (Value::Int(_), Value::Int(_))),
                        "reached collector callsite: {tail:?}"
                    );
                    assert_eq!(
                        tail[5],
                        Value::Int(marker3_want),
                        "entered-collector marker"
                    );
                    assert_eq!(
                        tail[6],
                        Value::Bool(false),
                        "refused assert condition debris: {tail:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn p7_rowdiff_flag_number_inactive_interpreter() {
    // NUMBER data row: the flag cell (row+4) must be 0 for the inactive
    // Boolean field; 1 is the minimal invalid state.
    check_rowdiff_case(ROWDIFF_NUMBER_SOURCE, "flag", 4);
}

#[test]
fn p7_rowdiff_stale_slot_number_interpreter() {
    // Same NUMBER fixture: a non-STRING/OBJECT data row must carry the
    // inactive slot 0 and gen 0 at row+5/6; 1.0 is a nonzero inactive
    // slot, refused by the inactivity rule rather than any brand check.
    check_rowdiff_case(ROWDIFF_NUMBER_SOURCE, "stale-slot", 5);
}

#[test]
fn p7_rowdiff_unused_number_string_interpreter() {
    // STRING data row: the unused Float cell (row+3) must be 0.0 for a
    // non-NUMBER tag; 1.0 is the minimal invalid state.
    check_rowdiff_case(ROWDIFF_STRING_SOURCE, "unused-number", 3);
}

#[test]
fn p7_rowedge_retained_edges_collector_interpreter() {
    // Positive edge probe only: the owner stays bound at top level, and a
    // top-level `var` remains reachable through the active script frame, so
    // an owner-drop variant cannot free the owner without deeper
    // frame/environment surgery. That surgery is out of scope for this
    // baseline contract (matched drop control only if minimal); the retain
    // probe below plus the row-case negatives already pin it.
    // The nulled data/getter/setter bindings show positive retention
    // through the owned rows under the active script-frame owner root; no exclusive-drop negative claimed.
    let outcome = run_row_contract(ROWEDGE_SOURCE, Box::leak(Box::new(RowEdgeProbe)));
    eprintln!(
        "P7_ROWEDGE retain=true intentional_negative=false status={} ip={} frames={:?} ip_owner={} stack_len={} stack={:?}",
        outcome.status,
        outcome.ip,
        outcome.frames,
        outcome.ip_owner,
        outcome.stack.len(),
        outcome.stack,
    );
    let edge_marker = Value::Int(701);
    let edge_marker3 = Value::Int(711);
    assert!(outcome.halted, "edge retain must halt: {}", outcome.status);
    let tail = &outcome.stack[outcome.stack.len() - 27..];
    assert_eq!(field(&tail[0], "tag"), Value::Int(2), "boxed boolean tag");
    assert_eq!(
        field(&tail[0], "flag"),
        Value::Bool(true),
        "getter never ran pre-probe"
    );
    assert_eq!(tail[1], edge_marker, "edge pipeline marker");
    // Packed header count identical before, between and after: no
    // invocation could have added the __hits row anywhere.
    assert_eq!(tail[2], tail[10], "no row added by pre-collect inspection");
    assert_eq!(tail[10], tail[26], "no row added by collector tracing");
    // Selected descriptor markers/attributes unchanged across the
    // collection; measured data marker 51, accessor marker 60, selected attrs only.
    assert_eq!(
        tail[3..10],
        tail[16..23],
        "descriptors unchanged by tracing"
    );
    assert_eq!(tail[3], Value::Int(51), "data marker");
    assert_eq!(
        tail[4..7],
        [Value::Bool(false), Value::Bool(false), Value::Bool(false)],
        "data attributes"
    );
    assert_eq!(tail[7], Value::Int(60), "accessor marker");
    assert_eq!(
        tail[8..10],
        [Value::Bool(false), Value::Bool(false)],
        "accessor attributes"
    );
    match (&tail[11], &tail[12], &tail[14]) {
        (Value::Int(before), Value::Int(pending), Value::Int(after)) => {
            assert_eq!(*pending, 0, "no pending nodes at explicit collect");
            assert_eq!(
                *after,
                *before + 1,
                "exactly one real collection, the explicit one"
            );
        }
        other => panic!("edge collection counter evidence: {other:?}"),
    }
    assert_eq!(tail[13], edge_marker3, "entered-collector marker");
    // Real checked bases plus brands: data OBJECT, both accessors
    // FUNCTION, all reached only through the owned rows.
    assert_eq!(tail[23], Value::Int(3), "data brand");
    assert_eq!(tail[24], Value::Int(4), "getter brand");
    assert_eq!(tail[25], Value::Int(4), "setter brand");
}

// ---- P7 shared-row reference-arm negatives (same apparatus, new arms) ----
// The flag/stale-slot/unused-number cases pin the data-inactive and primitive
// cells. The reference arms below pin the rest of the scalar contract: key
// identity/generation plus the key String brand, a data reference payload
// generation/brand failure, accessor Function generation/brand failures, and
// the absent-side 0/0 rule. Every case runs the same fresh matched
// ordinary-versus-real-collector control/corrupt pipelines: refusal must
// carry the exact consumer callsite markers, never an incidental bootstrap
// or ordinary-path failure before collection.
const ROWDIFF_ACCESSOR_SOURCE: &str = "var o={};var g=function(){'use strict';return 1;};var s=function(v){'use strict';};Object.defineProperty(o,'x',{get:g,set:s});g=null;s=null;true;";
const ROWDIFF_SETONLY_SOURCE: &str =
    "var o={};var s=function(v){'use strict';};Object.defineProperty(o,'x',{set:s});s=null;true;";
const ROWMIX_SOURCE: &str = "var data={n:1};var gf=function(){'use strict';return 1;};var sf=function(v){'use strict';};var go=function(){'use strict';return 2;};var so=function(v){'use strict';};var owner={};Object.defineProperty(owner,'r',{value:data,enumerable:false});Object.defineProperty(owner,'x',{get:gf,set:sf,enumerable:false});Object.defineProperty(owner,'p',{value:7});Object.defineProperty(owner,'g',{get:go});Object.defineProperty(owner,'s',{set:so});Object.defineProperty(owner,'u',{get:undefined,set:undefined});var clean=owner.__hits===undefined;data=null;gf=null;sf=null;go=null;so=null;clean;";

#[derive(Clone, Copy)]
enum ArmCorrupt {
    /// Overwrite one cell with a constant Number.
    Const(f64),
    /// Overwrite two cells (cell, cell+1) with the owner's own
    /// (ref_slot, generation): a brand-confusion negative that keeps a
    /// consistent live pair but the wrong brand.
    OwnerPair,
}

#[derive(Clone, Copy)]
struct RowArmProbe {
    owner: &'static str,
    prop: i64,
    cell: i64,
    corrupt: ArmCorrupt,
    consumer: RowDiffConsumer,
    case: i64,
    is_corrupt: bool,
}

impl vm::SourcePlugin for RowArmProbe {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN)
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
        let (make, _) = probe_helper(&ir, "__compat_ref_make");
        let (read, _) = probe_helper(&ir, "__compat_ref_read");
        let (write, write_schema) = probe_helper(&ir, "__compat_ref_write");
        let (checked, _) = probe_helper(&ir, "__compat_es5_checked_base");
        let (decode_int, _) = probe_helper(&ir, "__compat_es5_decode_int");
        let (decode_u32, _) = probe_helper(&ir, "__compat_es5_decode_u32");
        let (observe, _) = probe_helper(&ir, "__compat_es5_heap_observe");
        let (validator, _) = probe_helper(&ir, "__compat_es5_descriptor_row_validator");
        let (own, own_schema) = probe_helper(&ir, "__compat_es5_get_own_property");
        let own_args = probe_args(&ir, "__compat_es5_get_own_property");
        let carrier = probe_carrier(&ir, make);
        let owner = probe_owner(&ir, self.owner);
        let base = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            probe_call(
                checked,
                vec![
                    vm::Expr::Var(carrier),
                    probe_get(owner, "ref_slot"),
                    probe_get(owner, "generation"),
                ],
            ),
        );
        // Same production layout facts: header 5 cells, 7 cells per row.
        let row = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            vm::Expr::Add(
                Box::new(vm::Expr::Var(base)),
                Box::new(vm::Expr::Int(5 + 7 * self.prop)),
            ),
        );
        // Self-describing pipeline marker: 2000 + case*100 + consumer tag
        // (ordinary 10, collector 20) + mode (control 0, corrupt 1).
        let marker = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            vm::Expr::Int(
                2000 + self.case * 100
                    + match self.consumer {
                        RowDiffConsumer::Ordinary => 10,
                        RowDiffConsumer::Collector => 20,
                    }
                    + i64::from(self.is_corrupt),
            ),
        );
        probe_push(&mut ir, vm::Expr::Var(marker));
        let marker2 = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            vm::Expr::Int(2000 + self.case * 100 + 40 + i64::from(self.is_corrupt)),
        );
        // Touched cells: one cell for a constant fault, the pair for an
        // owner-pair brand confusion. Control writes each touched cell back
        // untouched (proving the address while preserving the row). Corrupt
        // runs are intentional negatives: the untouched row is validated
        // first, then the fault lands and the consumer below is the first
        // observer of the corrupted row.
        let touched: [i64; 2] = match self.corrupt {
            ArmCorrupt::Const(_) => [self.cell, -1],
            ArmCorrupt::OwnerPair => [self.cell, self.cell + 1],
        };
        if self.is_corrupt {
            let _sanity = probe_let(
                &mut ir,
                None,
                probe_call(validator, vec![vm::Expr::Var(carrier), vm::Expr::Var(row)]),
            );
        }
        for (at, cell) in touched.iter().enumerate() {
            if *cell < 0 {
                continue;
            }
            let addr = vm::Expr::Add(Box::new(vm::Expr::Var(row)), Box::new(vm::Expr::Int(*cell)));
            let value = if self.is_corrupt {
                match self.corrupt {
                    ArmCorrupt::Const(fault) => vm::Expr::Float(fault),
                    ArmCorrupt::OwnerPair if at == 0 => probe_num(probe_get(owner, "ref_slot")),
                    ArmCorrupt::OwnerPair => probe_num(probe_get(owner, "generation")),
                }
            } else {
                let back = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Number),
                    probe_call(read, vec![vm::Expr::Var(carrier), probe_num(addr.clone())]),
                );
                vm::Expr::Var(back)
            };
            let _wb = probe_let(
                &mut ir,
                write_schema.clone(),
                probe_call(write, vec![vm::Expr::Var(carrier), probe_num(addr), value]),
            );
        }
        probe_push(&mut ir, vm::Expr::Var(marker2));
        match self.consumer {
            RowDiffConsumer::Ordinary => {
                let coll_before = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Int),
                    probe_call(
                        observe,
                        vec![vm::Expr::Var(carrier), vm::Expr::Int(5), vm::Expr::Int(0)],
                    ),
                );
                // The counter push precedes the consumer call, so a refusal
                // stack ending here proves the validator was entered and
                // never returned.
                probe_push(&mut ir, vm::Expr::Var(coll_before));
                let desc = probe_ordinary_descriptor(
                    &mut ir,
                    carrier,
                    owner,
                    row,
                    validator,
                    read,
                    decode_int,
                    decode_u32,
                    own,
                    own_schema.clone(),
                    own_args[1].clone(),
                );
                probe_push_descriptor_fields(
                    &mut ir,
                    desc,
                    &["present", "writable", "enumerable", "configurable"],
                );
                let coll_after = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Int),
                    probe_call(
                        observe,
                        vec![vm::Expr::Var(carrier), vm::Expr::Int(5), vm::Expr::Int(0)],
                    ),
                );
                probe_push(&mut ir, vm::Expr::Var(coll_after));
            }
            RowDiffConsumer::Collector => {
                // Suppress any incidental pressure collection before the
                // explicit one, so the only collection in this pipeline is
                // the real `__compat_es5_heap_collect` call below.
                let _pressure = probe_let(
                    &mut ir,
                    write_schema,
                    probe_call(
                        write,
                        vec![
                            vm::Expr::Var(carrier),
                            vm::Expr::Float(10.0),
                            vm::Expr::Float(0.0),
                        ],
                    ),
                );
                let coll_before = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Int),
                    probe_call(
                        observe,
                        vec![vm::Expr::Var(carrier), vm::Expr::Int(5), vm::Expr::Int(0)],
                    ),
                );
                let pending = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Int),
                    probe_call(
                        observe,
                        vec![vm::Expr::Var(carrier), vm::Expr::Int(8), vm::Expr::Int(0)],
                    ),
                );
                probe_push(&mut ir, vm::Expr::Var(coll_before));
                probe_push(&mut ir, vm::Expr::Var(pending));
                let (root_enter, _) = probe_helper(&ir, "__compat_es5_root_enter");
                let (root_write, root_write_schema) = probe_helper(&ir, "__compat_es5_root_write");
                let (root_leave, root_leave_schema) = probe_helper(&ir, "__compat_es5_root_leave");
                let (collect, collect_schema) = probe_helper(&ir, "__compat_es5_heap_collect");
                let frame = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Int),
                    probe_call(root_enter, vec![vm::Expr::Var(carrier), vm::Expr::Int(1)]),
                );
                let _ack = probe_let(
                    &mut ir,
                    root_write_schema,
                    probe_call(
                        root_write,
                        vec![
                            vm::Expr::Var(carrier),
                            vm::Expr::Var(frame),
                            vm::Expr::Int(0),
                            vm::Expr::Var(owner),
                        ],
                    ),
                );
                // Entered-collector marker: pushed after the owner root is
                // pinned and before the explicit collect, so a refusal stack
                // ending here proves the real collector was entered and
                // never returned.
                let marker3 = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Int),
                    vm::Expr::Int(2000 + self.case * 100 + 60 + i64::from(self.is_corrupt)),
                );
                probe_push(&mut ir, vm::Expr::Var(marker3));
                let reclaimed = probe_let(
                    &mut ir,
                    collect_schema,
                    probe_call(collect, vec![vm::Expr::Var(carrier)]),
                );
                let coll_after = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Int),
                    probe_call(
                        observe,
                        vec![vm::Expr::Var(carrier), vm::Expr::Int(5), vm::Expr::Int(0)],
                    ),
                );
                probe_push(&mut ir, vm::Expr::Var(coll_after));
                probe_push(&mut ir, vm::Expr::Var(reclaimed));
                let desc = probe_ordinary_descriptor(
                    &mut ir,
                    carrier,
                    owner,
                    row,
                    validator,
                    read,
                    decode_int,
                    decode_u32,
                    own,
                    own_schema.clone(),
                    own_args[1].clone(),
                );
                probe_push_descriptor_fields(
                    &mut ir,
                    desc,
                    &["present", "writable", "enumerable", "configurable"],
                );
                let _leave = probe_let(
                    &mut ir,
                    root_leave_schema,
                    probe_call(
                        root_leave,
                        vec![vm::Expr::Var(carrier), vm::Expr::Var(frame)],
                    ),
                );
            }
        }
        Ok(ir)
    }
}

struct RowArmCase {
    name: &'static str,
    source: &'static str,
    owner: &'static str,
    prop: i64,
    cell: i64,
    corrupt: ArmCorrupt,
    present: i64,
    attrs: [bool; 3],
}

fn check_rowarm_case(case: &RowArmCase, id: i64) {
    for consumer in [RowDiffConsumer::Ordinary, RowDiffConsumer::Collector] {
        for is_corrupt in [false, true] {
            let outcome = run_row_contract(
                case.source,
                Box::leak(Box::new(RowArmProbe {
                    owner: case.owner,
                    prop: case.prop,
                    cell: case.cell,
                    corrupt: case.corrupt,
                    consumer,
                    case: id,
                    is_corrupt,
                })),
            );
            let intentional = is_corrupt;
            let marker_want = 2000
                + id * 100
                + match consumer {
                    RowDiffConsumer::Ordinary => 10,
                    RowDiffConsumer::Collector => 20,
                }
                + i64::from(is_corrupt);
            // Log-first: refusal unwinds guest frames, so ip/frames are
            // residual. The origin proof is the entered-callsite markers
            // plus the stack below, recorded even when a later assertion
            // fails.
            eprintln!(
                "P7_ROWARM case={} consumer={consumer:?} corrupt={is_corrupt} intentional_negative={intentional} status={} ip={} frames={:?} ip_owner={} stack_len={} stack={:?}",
                case.name,
                outcome.status,
                outcome.ip,
                outcome.frames,
                outcome.ip_owner,
                outcome.stack.len(),
                outcome.stack,
            );
            let boxed_true = |value: &Value| {
                assert_eq!(field(value, "tag"), Value::Int(2), "boxed boolean tag");
                assert_eq!(
                    field(value, "flag"),
                    Value::Bool(true),
                    "script value stays true"
                );
            };
            let marker2_want = 2000 + id * 100 + 40 + i64::from(is_corrupt);
            let marker3_want = 2000 + id * 100 + 60 + i64::from(is_corrupt);
            // Refusal unwinds guest frames, so origin is proven structurally
            // below: the entered consumer call never returned. The trailing
            // Bool(false) on refusal stacks is the refused assert's own
            // condition; every row-guard assert here fails closed as false.
            match (consumer, is_corrupt) {
                (RowDiffConsumer::Ordinary, false) => {
                    assert!(
                        outcome.halted,
                        "ordinary control must halt: {}",
                        outcome.status
                    );
                    let tail = &outcome.stack[outcome.stack.len() - 9..];
                    boxed_true(&tail[0]);
                    assert_eq!(tail[1], Value::Int(marker_want), "pipeline marker");
                    assert_eq!(tail[2], Value::Int(marker2_want), "post-write marker");
                    assert_eq!(tail[3], tail[8], "no incidental GC in ordinary path");
                    assert_eq!(tail[4], Value::Int(case.present), "descriptor marker");
                    assert_eq!(
                        tail[5..8],
                        [
                            Value::Bool(case.attrs[0]),
                            Value::Bool(case.attrs[1]),
                            Value::Bool(case.attrs[2]),
                        ],
                        "descriptor attributes"
                    );
                }
                (RowDiffConsumer::Ordinary, true) => {
                    assert!(
                        !outcome.halted && outcome.status.contains("assertion failed"),
                        "intentional-negative ordinary refusal: {}",
                        outcome.status
                    );
                    let tail = &outcome.stack[outcome.stack.len() - 5..];
                    boxed_true(&tail[0]);
                    assert_eq!(tail[1], Value::Int(marker_want), "pipeline marker");
                    assert_eq!(tail[2], Value::Int(marker2_want), "post-write marker");
                    assert!(
                        matches!(tail[3], Value::Int(_)),
                        "reached validator callsite: {tail:?}"
                    );
                    assert_eq!(
                        tail[4],
                        Value::Bool(false),
                        "refused assert condition debris: {tail:?}"
                    );
                }
                (RowDiffConsumer::Collector, false) => {
                    assert!(
                        outcome.halted,
                        "collector control must halt: {}",
                        outcome.status
                    );
                    let tail = &outcome.stack[outcome.stack.len() - 12..];
                    boxed_true(&tail[0]);
                    assert_eq!(tail[1], Value::Int(marker_want), "pipeline marker");
                    assert_eq!(tail[2], Value::Int(marker2_want), "post-write marker");
                    match (&tail[3], &tail[4], &tail[6]) {
                        (Value::Int(before), Value::Int(pending), Value::Int(after)) => {
                            assert_eq!(*pending, 0, "no pending nodes at explicit collect");
                            assert_eq!(
                                *after,
                                *before + 1,
                                "exactly one real collection, the explicit one"
                            );
                        }
                        other => panic!("collection counter evidence: {other:?}"),
                    }
                    assert_eq!(
                        tail[5],
                        Value::Int(marker3_want),
                        "entered-collector marker"
                    );
                    assert_eq!(tail[8], Value::Int(case.present), "descriptor marker");
                    assert_eq!(
                        tail[9..12],
                        [
                            Value::Bool(case.attrs[0]),
                            Value::Bool(case.attrs[1]),
                            Value::Bool(case.attrs[2]),
                        ],
                        "descriptor attributes"
                    );
                }
                (RowDiffConsumer::Collector, true) => {
                    assert!(
                        !outcome.halted && outcome.status.contains("assertion failed"),
                        "intentional-negative collector refusal: {}",
                        outcome.status
                    );
                    let tail = &outcome.stack[outcome.stack.len() - 7..];
                    boxed_true(&tail[0]);
                    assert_eq!(tail[1], Value::Int(marker_want), "pipeline marker");
                    assert_eq!(tail[2], Value::Int(marker2_want), "post-write marker");
                    assert!(
                        matches!((&tail[3], &tail[4]), (Value::Int(_), Value::Int(_))),
                        "reached collector callsite: {tail:?}"
                    );
                    assert_eq!(
                        tail[5],
                        Value::Int(marker3_want),
                        "entered-collector marker"
                    );
                    assert_eq!(
                        tail[6],
                        Value::Bool(false),
                        "refused assert condition debris: {tail:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn p7_rowarm_key_slot_identity_interpreter() {
    // Key slot cell (row+0): control writes the live slot back; 1.0 points
    // at a foreign slot with the key generation and must refuse in both
    // pipelines at the exact consumer callsite.
    check_rowarm_case(
        &RowArmCase {
            name: "key-slot",
            source: ROWDIFF_NUMBER_SOURCE,
            owner: "o",
            prop: 0,
            cell: 0,
            corrupt: ArmCorrupt::Const(1.0),
            present: 51,
            attrs: [false, false, false],
        },
        1,
    );
}

#[test]
fn p7_rowarm_key_generation_interpreter() {
    // Key generation cell (row+1): control writes the live generation back;
    // 0.0 detaches the key pair and must refuse in both pipelines.
    check_rowarm_case(
        &RowArmCase {
            name: "key-gen",
            source: ROWDIFF_NUMBER_SOURCE,
            owner: "o",
            prop: 0,
            cell: 1,
            corrupt: ArmCorrupt::Const(0.0),
            present: 51,
            attrs: [false, false, false],
        },
        2,
    );
}

#[test]
fn p7_rowarm_key_string_brand_interpreter() {
    // Key brand confusion: the key pair becomes the owner's own live
    // (slot, generation), which resolves to an OBJECT node, not the
    // required String brand. Both pipelines must refuse at the consumer.
    check_rowarm_case(
        &RowArmCase {
            name: "key-brand",
            source: ROWDIFF_NUMBER_SOURCE,
            owner: "o",
            prop: 0,
            cell: 0,
            corrupt: ArmCorrupt::OwnerPair,
            present: 51,
            attrs: [false, false, false],
        },
        3,
    );
}

#[test]
fn p7_rowarm_data_ref_generation_interpreter() {
    // Data reference payload generation (STRING row+6): detaching the live
    // 'hi' box generation must refuse in both pipelines.
    check_rowarm_case(
        &RowArmCase {
            name: "dataref-gen",
            source: ROWDIFF_STRING_SOURCE,
            owner: "o",
            prop: 0,
            cell: 6,
            corrupt: ArmCorrupt::Const(0.0),
            present: 51,
            attrs: [false, false, false],
        },
        4,
    );
}

#[test]
fn p7_rowarm_data_ref_brand_interpreter() {
    // Data reference payload brand confusion: the STRING payload pair
    // becomes the owner's live OBJECT pair, which the STRING brand check
    // must refuse in both pipelines.
    check_rowarm_case(
        &RowArmCase {
            name: "dataref-brand",
            source: ROWDIFF_STRING_SOURCE,
            owner: "o",
            prop: 0,
            cell: 5,
            corrupt: ArmCorrupt::OwnerPair,
            present: 51,
            attrs: [false, false, false],
        },
        5,
    );
}

#[test]
fn p7_rowarm_accessor_getter_generation_interpreter() {
    // Accessor getter generation (row+4): detaching the live getter pair
    // must refuse in both pipelines.
    check_rowarm_case(
        &RowArmCase {
            name: "getter-gen",
            source: ROWDIFF_ACCESSOR_SOURCE,
            owner: "o",
            prop: 0,
            cell: 4,
            corrupt: ArmCorrupt::Const(0.0),
            present: 60,
            attrs: [false, false, false],
        },
        6,
    );
}

#[test]
fn p7_rowarm_accessor_setter_brand_interpreter() {
    // Accessor setter brand confusion: the setter pair becomes the owner's
    // live OBJECT pair, which the FUNCTION brand check must refuse in both
    // pipelines.
    check_rowarm_case(
        &RowArmCase {
            name: "setter-brand",
            source: ROWDIFF_ACCESSOR_SOURCE,
            owner: "o",
            prop: 0,
            cell: 5,
            corrupt: ArmCorrupt::OwnerPair,
            present: 60,
            attrs: [false, false, false],
        },
        7,
    );
}

#[test]
fn p7_rowarm_absent_getter_slot_interpreter() {
    // Absent-side 0/0 rule on a set-only row: the getter slot (row+3) must
    // be 0 while its generation is 0; a nonzero slot must refuse the exact
    // absent-side check in both pipelines.
    check_rowarm_case(
        &RowArmCase {
            name: "absent-getter-slot",
            source: ROWDIFF_SETONLY_SOURCE,
            owner: "o",
            prop: 0,
            cell: 3,
            corrupt: ArmCorrupt::Const(1.0),
            present: 60,
            attrs: [false, false, false],
        },
        8,
    );
}

// Mixed-row retain probe: one owner holds a data reference row, a full
// accessor row, a primitive row, a get-only row, a set-only row and an
// undefined get/set row in that order. The collector must retain every
// referent through the owned rows as positive retention only: selected
// markers/attrs/brands below are the measured proof, no stale-edge
// negative claimed here. The owner stays pinned under the active
// script-frame root; no exclusive-drop negative and no getter runs
// anywhere in this pipeline.
struct MixedProbe;

impl vm::SourcePlugin for MixedProbe {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN)
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
        let (make, _) = probe_helper(&ir, "__compat_ref_make");
        let (read, _) = probe_helper(&ir, "__compat_ref_read");
        let (write, write_schema) = probe_helper(&ir, "__compat_ref_write");
        let (checked, _) = probe_helper(&ir, "__compat_es5_checked_base");
        let (decode_int, _) = probe_helper(&ir, "__compat_es5_decode_int");
        let (decode_u32, _) = probe_helper(&ir, "__compat_es5_decode_u32");
        let (observe, _) = probe_helper(&ir, "__compat_es5_heap_observe");
        let (validator, _) = probe_helper(&ir, "__compat_es5_descriptor_row_validator");
        let (own, own_schema) = probe_helper(&ir, "__compat_es5_get_own_property");
        let own_args = probe_args(&ir, "__compat_es5_get_own_property");
        let (root_enter, _) = probe_helper(&ir, "__compat_es5_root_enter");
        let (root_write, root_write_schema) = probe_helper(&ir, "__compat_es5_root_write");
        let (root_leave, root_leave_schema) = probe_helper(&ir, "__compat_es5_root_leave");
        let (collect, collect_schema) = probe_helper(&ir, "__compat_es5_heap_collect");
        let carrier = probe_carrier(&ir, make);
        let owner = probe_owner(&ir, "owner");
        let base = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            probe_call(
                checked,
                vec![
                    vm::Expr::Var(carrier),
                    probe_get(owner, "ref_slot"),
                    probe_get(owner, "generation"),
                ],
            ),
        );
        // Same production layout facts: header 5 cells, 7 cells per row.
        // Owner holds exactly six properties: 'r' data-ref, 'x' full
        // accessor, 'p' primitive, 'g' get-only, 's' set-only, 'u'
        // undefined get/set.
        let mut rows = Vec::with_capacity(6);
        for prop in 0..6 {
            rows.push(probe_let(
                &mut ir,
                Some(vm::TypeSchema::Int),
                vm::Expr::Add(
                    Box::new(vm::Expr::Var(base)),
                    Box::new(vm::Expr::Int(5 + 7 * prop)),
                ),
            ));
        }
        // Edge pipeline marker first: 801 identifies this mixed retain probe.
        let edge_marker = probe_let(&mut ir, Some(vm::TypeSchema::Int), vm::Expr::Int(801));
        probe_push(&mut ir, vm::Expr::Var(edge_marker));
        let read_num = |ir: &mut vm::FrontendIr, addr: vm::Expr| {
            probe_let(
                ir,
                Some(vm::TypeSchema::Number),
                probe_call(read, vec![vm::Expr::Var(carrier), probe_num(addr)]),
            )
        };
        let packed_pre = {
            let cell = read_num(
                &mut ir,
                vm::Expr::Add(Box::new(vm::Expr::Var(base)), Box::new(vm::Expr::Int(2))),
            );
            probe_let(
                &mut ir,
                Some(vm::TypeSchema::Int),
                probe_call(decode_int, vec![vm::Expr::Var(cell)]),
            )
        };
        probe_push(&mut ir, vm::Expr::Var(packed_pre));
        // Ordinary consumer first: every row must validate through the
        // shared emitter and build its exact descriptor record.
        let data_fields = ["present", "writable", "enumerable", "configurable"];
        let accessor_fields = ["present", "enumerable", "configurable"];
        let row_fields: [(&[&str], u16); 6] = [
            (&data_fields, rows[0]),
            (&accessor_fields, rows[1]),
            (&data_fields, rows[2]),
            (&accessor_fields, rows[3]),
            (&accessor_fields, rows[4]),
            (&accessor_fields, rows[5]),
        ];
        for (names, row) in row_fields {
            let desc = probe_ordinary_descriptor(
                &mut ir,
                carrier,
                owner,
                row,
                validator,
                read,
                decode_int,
                decode_u32,
                own,
                own_schema.clone(),
                own_args[1].clone(),
            );
            probe_push_descriptor_fields(&mut ir, desc, names);
        }
        // Referent identities come from the raw row cells (Ints, never
        // roots): data slot/gen at row+5/6 for 'r', getter at row+3/4 and
        // setter at row+5/6 for 'x'/'g'/'s', mirroring the scalar cell map.
        let decode_cell = |ir: &mut vm::FrontendIr, decode: u16, row: u16, off: i64| {
            let cell = read_num(
                ir,
                vm::Expr::Add(Box::new(vm::Expr::Var(row)), Box::new(vm::Expr::Int(off))),
            );
            probe_let(
                ir,
                Some(vm::TypeSchema::Int),
                probe_call(decode, vec![vm::Expr::Var(cell)]),
            )
        };
        let r_slot = decode_cell(&mut ir, decode_int, rows[0], 5);
        let r_gen = decode_cell(&mut ir, decode_u32, rows[0], 6);
        let gf_slot = decode_cell(&mut ir, decode_int, rows[1], 3);
        let gf_gen = decode_cell(&mut ir, decode_u32, rows[1], 4);
        let sf_slot = decode_cell(&mut ir, decode_int, rows[1], 5);
        let sf_gen = decode_cell(&mut ir, decode_u32, rows[1], 6);
        let go_slot = decode_cell(&mut ir, decode_int, rows[3], 3);
        let go_gen = decode_cell(&mut ir, decode_u32, rows[3], 4);
        let so_slot = decode_cell(&mut ir, decode_int, rows[4], 5);
        let so_gen = decode_cell(&mut ir, decode_u32, rows[4], 6);
        let packed_mid = {
            let cell = read_num(
                &mut ir,
                vm::Expr::Add(Box::new(vm::Expr::Var(base)), Box::new(vm::Expr::Int(2))),
            );
            probe_let(
                &mut ir,
                Some(vm::TypeSchema::Int),
                probe_call(decode_int, vec![vm::Expr::Var(cell)]),
            )
        };
        probe_push(&mut ir, vm::Expr::Var(packed_mid));
        let coll_before = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            probe_call(
                observe,
                vec![vm::Expr::Var(carrier), vm::Expr::Int(5), vm::Expr::Int(0)],
            ),
        );
        let pending = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            probe_call(
                observe,
                vec![vm::Expr::Var(carrier), vm::Expr::Int(8), vm::Expr::Int(0)],
            ),
        );
        probe_push(&mut ir, vm::Expr::Var(coll_before));
        probe_push(&mut ir, vm::Expr::Var(pending));
        let _pressure = probe_let(
            &mut ir,
            write_schema,
            probe_call(
                write,
                vec![
                    vm::Expr::Var(carrier),
                    vm::Expr::Float(10.0),
                    vm::Expr::Float(0.0),
                ],
            ),
        );
        let frame = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            probe_call(root_enter, vec![vm::Expr::Var(carrier), vm::Expr::Int(1)]),
        );
        // Only the owner is pinned. Every referent box stays reachable
        // exclusively through the owned rows, so its survival proves the
        // collector traced the real per-arm edges.
        let _ack = probe_let(
            &mut ir,
            root_write_schema,
            probe_call(
                root_write,
                vec![
                    vm::Expr::Var(carrier),
                    vm::Expr::Var(frame),
                    vm::Expr::Int(0),
                    vm::Expr::Var(owner),
                ],
            ),
        );
        // Entered-collector marker: pushed after the owner root pinning and
        // before the explicit collect.
        let edge_marker3 = probe_let(&mut ir, Some(vm::TypeSchema::Int), vm::Expr::Int(811));
        probe_push(&mut ir, vm::Expr::Var(edge_marker3));
        let reclaimed = probe_let(
            &mut ir,
            collect_schema,
            probe_call(collect, vec![vm::Expr::Var(carrier)]),
        );
        let coll_after = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            probe_call(
                observe,
                vec![vm::Expr::Var(carrier), vm::Expr::Int(5), vm::Expr::Int(0)],
            ),
        );
        probe_push(&mut ir, vm::Expr::Var(coll_after));
        probe_push(&mut ir, vm::Expr::Var(reclaimed));
        for (names, row) in row_fields {
            let desc = probe_ordinary_descriptor(
                &mut ir,
                carrier,
                owner,
                row,
                validator,
                read,
                decode_int,
                decode_u32,
                own,
                own_schema.clone(),
                own_args[1].clone(),
            );
            probe_push_descriptor_fields(&mut ir, desc, names);
        }
        // Liveness plus brand through the real checked base: KIND_OBJECT
        // for the data box, KIND_FUNCTION for all four accessor boxes, all
        // reached only through the owned rows.
        for (slot, gen_) in [
            (r_slot, r_gen),
            (gf_slot, gf_gen),
            (sf_slot, sf_gen),
            (go_slot, go_gen),
            (so_slot, so_gen),
        ] {
            let live = probe_let(
                &mut ir,
                Some(vm::TypeSchema::Int),
                probe_call(
                    checked,
                    vec![
                        vm::Expr::Var(carrier),
                        vm::Expr::Var(slot),
                        vm::Expr::Var(gen_),
                    ],
                ),
            );
            let kind_cell = read_num(&mut ir, vm::Expr::Var(live));
            let brand = probe_let(
                &mut ir,
                Some(vm::TypeSchema::Int),
                probe_call(decode_int, vec![vm::Expr::Var(kind_cell)]),
            );
            probe_push(&mut ir, vm::Expr::Var(brand));
        }
        let packed_post = {
            let cell = read_num(
                &mut ir,
                vm::Expr::Add(Box::new(vm::Expr::Var(base)), Box::new(vm::Expr::Int(2))),
            );
            probe_let(
                &mut ir,
                Some(vm::TypeSchema::Int),
                probe_call(decode_int, vec![vm::Expr::Var(cell)]),
            )
        };
        probe_push(&mut ir, vm::Expr::Var(packed_post));
        let _leave = probe_let(
            &mut ir,
            root_leave_schema,
            probe_call(
                root_leave,
                vec![vm::Expr::Var(carrier), vm::Expr::Var(frame)],
            ),
        );
        Ok(ir)
    }
}

#[test]
fn p7_rowmix_retained_edges_collector_interpreter() {
    // Six-arm retain probe: data-ref, full accessor, primitive, get-only,
    // set-only and undefined get/set rows must all validate, and the data
    // plus all four accessor referents must survive the real collection
    // with their exact brands. Any cross-arm scalar leak would trace the
    // wrong box and fail below.
    let outcome = run_row_contract(ROWMIX_SOURCE, Box::leak(Box::new(MixedProbe)));
    eprintln!(
        "P7_ROWMIX retain=true intentional_negative=false status={} ip={} frames={:?} ip_owner={} stack_len={} stack={:?}",
        outcome.status,
        outcome.ip,
        outcome.frames,
        outcome.ip_owner,
        outcome.stack.len(),
        outcome.stack,
    );
    let edge_marker = Value::Int(801);
    let edge_marker3 = Value::Int(811);
    assert!(outcome.halted, "mixed retain must halt: {}", outcome.status);
    let tail = &outcome.stack[outcome.stack.len() - 55..];
    assert_eq!(field(&tail[0], "tag"), Value::Int(2), "boxed boolean tag");
    assert_eq!(
        field(&tail[0], "flag"),
        Value::Bool(true),
        "getter never ran pre-probe"
    );
    assert_eq!(tail[1], edge_marker, "edge pipeline marker");
    // Packed header count identical before, between and after: no
    // invocation could have added a row anywhere.
    assert_eq!(tail[2], tail[23], "no row added by pre-collect inspection");
    assert_eq!(tail[23], tail[54], "no row added by collector tracing");
    // Selected descriptor markers/attributes unchanged across the
    // collection; measured data marker 51, accessor marker 60, selected attrs only.
    assert_eq!(
        tail[3..23],
        tail[29..49],
        "descriptors unchanged by tracing"
    );
    assert_eq!(tail[3], Value::Int(51), "data-ref marker");
    assert_eq!(
        tail[4..7],
        [Value::Bool(false), Value::Bool(false), Value::Bool(false)],
        "data-ref attributes"
    );
    assert_eq!(tail[7], Value::Int(60), "accessor marker");
    assert_eq!(
        tail[8..10],
        [Value::Bool(false), Value::Bool(false)],
        "accessor attributes"
    );
    assert_eq!(tail[10], Value::Int(51), "primitive marker");
    assert_eq!(
        tail[11..14],
        [Value::Bool(false), Value::Bool(false), Value::Bool(false)],
        "primitive attributes"
    );
    assert_eq!(tail[14], Value::Int(60), "get-only marker");
    assert_eq!(
        tail[15..17],
        [Value::Bool(false), Value::Bool(false)],
        "get-only attributes"
    );
    assert_eq!(tail[17], Value::Int(60), "set-only marker");
    assert_eq!(
        tail[18..20],
        [Value::Bool(false), Value::Bool(false)],
        "set-only attributes"
    );
    assert_eq!(tail[20], Value::Int(60), "undefined get/set marker");
    assert_eq!(
        tail[21..23],
        [Value::Bool(false), Value::Bool(false)],
        "undefined get/set attributes"
    );
    match (&tail[24], &tail[25], &tail[27]) {
        (Value::Int(before), Value::Int(pending), Value::Int(after)) => {
            assert_eq!(*pending, 0, "no pending nodes at explicit collect");
            assert_eq!(
                *after,
                *before + 1,
                "exactly one real collection, the explicit one"
            );
        }
        other => panic!("edge collection counter evidence: {other:?}"),
    }
    assert_eq!(tail[26], edge_marker3, "entered-collector marker");
    // Real checked bases plus brands: data OBJECT, all four accessors
    // FUNCTION, all reached only through the owned rows.
    assert_eq!(tail[49], Value::Int(3), "data brand");
    assert_eq!(tail[50], Value::Int(4), "full-accessor getter brand");
    assert_eq!(tail[51], Value::Int(4), "full-accessor setter brand");
    assert_eq!(tail[52], Value::Int(4), "get-only brand");
    assert_eq!(tail[53], Value::Int(4), "set-only brand");
}

#[test]
fn p7_private_prune_original_o01_program() {
    let source = "var p={x:1};var o=Object.create(p);var a=o.x===1;var b=\"x\" in o;var c=Object.getOwnPropertyDescriptor(o,\"x\")===undefined;a&&b&&c;";
    let (ir, program) = caller_program(source);
    for name in [
        "__compat_es5_function_length_key",
        "__compat_es5_ref_put",
        "__compat_es5_delete_property",
    ] {
        assert!(
            !ir.functions.iter().any(|f| f.name == name),
            "dead owned helper remains: {name}"
        );
    }
    for name in [
        "__compat_es5_root_write_node",
        "__compat_es5_node_publish_edge",
        "__compat_es5_heap_observe",
        "__compat_es5_set_prototype",
    ] {
        let decl = ir.functions.iter().find(|f| f.name == name).expect(name);
        assert!(ir.function_impls.contains_key(&decl.index));
    }
    assert!(
        program.local_count <= 47,
        "retained frame grew: {}",
        program.local_count
    );
    eprintln!(
        "P7_PRIVATE_PRUNE bytes={} frame={} prototypes={} native=false",
        program.code.len(),
        program.local_count,
        program.callable_prototypes.len()
    );
}

#[test]
fn p7_scalar_boundary_original_o01_program() {
    struct Tap(std::sync::Mutex<Option<vm::FrontendIr>>);
    impl vm::SourcePlugin for Tap {
        fn descriptor(&self) -> vm::SourceFrontendDescriptor {
            vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN)
        }
        fn parse_source(
            &self,
            source: &str,
            context: &vm::SourceFrontendContext<'_>,
        ) -> Result<vm::FrontendIr, vm::ParseError> {
            let ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
            *self.0.lock().unwrap() = Some(ir.clone());
            Ok(ir)
        }
    }
    let tap = Box::leak(Box::new(Tap(Default::default())));
    let source = "var p={x:1};var o=Object.create(p);var a=o.x===1;var b=\"x\" in o;var c=Object.getOwnPropertyDescriptor(o,\"x\")===undefined;a&&b&&c;";
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        javascript_es5_flavor(),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(tap)
            .with_host_api_catalog(
                pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog(),
            ),
    )
    .unwrap()
    .into_uncertified();
    let ir = tap.0.lock().unwrap();
    let ir = ir.as_ref().unwrap();
    let identity = ir
        .functions
        .iter()
        .find(|f| f.name == "__compat_es5_int_identity")
        .unwrap();
    let mut indices: Vec<_> = ir.function_impls.keys().copied().collect();
    indices.sort_unstable();
    let script = indices.binary_search(&identity.index).unwrap();
    let p = &compiled.program;
    let prototype = p
        .callable_prototypes
        .iter()
        .position(
            |p| matches!(p.target, vm::CallableTarget::ScriptFunction(i) if i as usize == script),
        )
        .unwrap();
    let mut ip = 0;
    let mut calls = 0;
    let mut shl_bridges = 0;
    let mut previous_bridge = false;
    let mut instructions = 0;
    let mut listing = String::new();
    while ip < p.code.len() {
        let op = vm::OpCode::try_from(p.code[ip]).unwrap();
        let args = &p.code[ip + 1..ip + 1 + op.operand_len()];
        listing.push_str(&format!("{ip}\t{op:?}\t{args:?}"));
        if op == vm::OpCode::Ldc {
            let index = u32::from_le_bytes(args.try_into().unwrap()) as usize;
            listing.push_str(&format!("\t{:?}", p.constants[index]));
        }
        listing.push('\n');
        let bridge = op == vm::OpCode::CallScript
            && u32::from_le_bytes(p.code[ip + 1..ip + 5].try_into().unwrap()) as usize == prototype;
        if bridge {
            assert_eq!(
                p.type_map.as_ref().unwrap().operand_types.get(&ip),
                Some(&(vm::ValueType::Callable, vm::ValueType::Int))
            );
            calls += 1;
        }
        if op == vm::OpCode::Shl && previous_bridge {
            shl_bridges += 1;
        }
        // Shl loads its shift count after the dynamic input.
        if op != vm::OpCode::Ldc {
            previous_bridge = bridge;
        }
        instructions += 1;
        ip += 1 + op.operand_len();
    }
    if let Some(path) = std::env::var_os("P7_SB5_CAPTURE") {
        let path = std::path::PathBuf::from(path);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("o01-source.js"), source).unwrap();
        std::fs::write(path.join("o01-ir.txt"), format!("{ir:#?}")).unwrap();
        std::fs::write(path.join("o01-program.txt"), format!(
            "regions={:#?}\nprototypes={:#?}\nscript_functions={:#?}\ntype_map={:#?}\n{listing}",
            p.function_regions, p.callable_prototypes, p.script_functions, p.type_map,
        )).unwrap();
    }
    assert!(
        calls >= 33,
        "bounded numeric policy must reach actual consumers: {calls}"
    );
    assert!(
        shl_bridges >= 31,
        "all diagnosed generic Get Shl inputs need Int restoration: {shl_bridges}"
    );
    eprintln!(
        "P7_SCALAR_BOUNDARY calls={calls} shl_bridges={shl_bridges} instructions={instructions} bytes={} locals={} prototypes={} native=false",
        p.code.len(),
        ir.locals,
        p.callable_prototypes.len()
    );
}

// ---- Collector last-key cache contract (default-off instrumentation) ------
// The synthetic control rewrites the *later* row's key pair to the pair of the
// immediately preceding row: both pairs then name the same live String node,
// which is exactly the pair reuse the production intrinsics already have
// across Function rows. Every other cell of both rows keeps its normal layout.
// This is a contract probe over the real emitted row scan, not a public JS
// key-semantics proof.

const CACHE_NUMBER_SOURCE: &str = "var o={};Object.defineProperty(o,'x',{value:7});Object.defineProperty(o,'y',{value:8});o.x===7&&o.y===8;";
const CACHE_STRING_SOURCE: &str = "var o={};Object.defineProperty(o,'x',{value:7});Object.defineProperty(o,'y',{value:'hi'});o.x===7&&o.y==='hi';";
const CACHE_ACCESSOR_SOURCE: &str = "var o={};var g=function(){'use strict';return 1;};var s=function(v){'use strict';};Object.defineProperty(o,'x',{value:7});Object.defineProperty(o,'y',{get:g,set:s,enumerable:false});true;";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CacheFault {
    None,
    /// Live pair shape with the stored generation moved off the cached one:
    /// the cache must miss and the original checked_base must refuse.
    KeyGenPlusOne,
    /// Fractional key slot cell: the original exact decoder must refuse
    /// before the cache decision is reached.
    KeyFrac,
    /// NaN key generation cell: same numeric-domain refusal.
    KeyNan,
    /// Foreign brand: the key pair becomes the owner's own live pair, so a
    /// miss reaches the original String-brand assert.
    KeyOwnerPair,
    /// The realm's first permanent node: slot 0 with its positive generation.
    /// Slot 0 is a legal key slot, so this must resolve through checked_base
    /// and then refuse only on the String brand.
    KeySlotZero,
    /// Metadata cell above the object tag domain.
    MetaMalformed,
    /// Inactive NUMBER row with a nonzero flag.
    InactiveNumberFlag,
    /// STRING data row whose value generation detaches.
    DataRefGen,
    /// Accessor row whose setter generation detaches.
    AccessorGen,
}

#[derive(Clone, Copy)]
struct CacheProbe {
    consumer: RowDiffConsumer,
    fault: CacheFault,
    dup: bool,
    collects: i64,
    stop_at: Option<i64>,
    case: i64,
}

fn cache_marker(probe: &CacheProbe) -> i64 {
    2000 + probe.case * 100
        + match probe.consumer {
            RowDiffConsumer::Ordinary => 10,
            RowDiffConsumer::Collector => 20,
        }
        + i64::from(probe.fault != CacheFault::None)
}

fn cache_read_cell(ir: &mut vm::FrontendIr, read: u16, carrier: u16, addr: vm::Expr) -> u16 {
    probe_let(
        ir,
        Some(vm::TypeSchema::Number),
        probe_call(read, vec![vm::Expr::Var(carrier), probe_num(addr)]),
    )
}

fn cache_write_cell(
    ir: &mut vm::FrontendIr,
    write: u16,
    schema: Option<vm::TypeSchema>,
    carrier: u16,
    addr: vm::Expr,
    value: vm::Expr,
) -> u16 {
    probe_let(
        ir,
        schema,
        probe_call(write, vec![vm::Expr::Var(carrier), probe_num(addr), value]),
    )
}

impl vm::SourcePlugin for CacheProbe {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN)
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let mut ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
        let (make, _) = probe_helper(&ir, "__compat_ref_make");
        let (read, _) = probe_helper(&ir, "__compat_ref_read");
        let (write, write_schema) = probe_helper(&ir, "__compat_ref_write");
        let (checked, _) = probe_helper(&ir, "__compat_es5_checked_base");
        let (decode_int, _) = probe_helper(&ir, "__compat_es5_decode_int");
        let (decode_u32, _) = probe_helper(&ir, "__compat_es5_decode_u32");
        let (observe, _) = probe_helper(&ir, "__compat_es5_heap_observe");
        let (validator, _) = probe_helper(&ir, "__compat_es5_descriptor_row_validator");
        let (view, view_schema, view_args) = probe_row_view(&mut ir);
        if let Some(stop_at) = self.stop_at {
            // Instrumented runs only; every other pipeline keeps the plain
            // emitted collector with no counters and no stop helper.
            let patched = object_observer::install_cache_probe(&mut ir, stop_at);
            assert_eq!(
                patched, 2,
                "the cache decision must exist in both the object and function row scans"
            );
        }
        let carrier = probe_carrier(&ir, make);
        let owner = probe_owner(&ir, "o");
        let base = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            probe_call(
                checked,
                vec![
                    vm::Expr::Var(carrier),
                    probe_get(owner, "ref_slot"),
                    probe_get(owner, "generation"),
                ],
            ),
        );
        let row = |prop: i64| {
            vm::Expr::Add(
                Box::new(vm::Expr::Var(base)),
                Box::new(vm::Expr::Int(5 + 7 * prop)),
            )
        };
        let cell =
            |prop: i64, at: i64| vm::Expr::Add(Box::new(row(prop)), Box::new(vm::Expr::Int(at)));
        if self.fault == CacheFault::KeySlotZero {
            // Slot 0 with its live generation must resolve; if it did not, the
            // pipeline marker would be missing and the refusal below would be
            // mis-attributed to the key brand.
            let _slot0 = probe_let(
                &mut ir,
                Some(vm::TypeSchema::Int),
                probe_call(
                    checked,
                    vec![vm::Expr::Var(carrier), vm::Expr::Int(0), vm::Expr::Int(1)],
                ),
            );
        }
        let marker = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            vm::Expr::Int(cache_marker(self)),
        );
        probe_push(&mut ir, vm::Expr::Var(marker));
        let marker2 = probe_let(
            &mut ir,
            Some(vm::TypeSchema::Int),
            vm::Expr::Int(cache_marker(self) + 40),
        );
        if self.dup {
            // Later row reuses the warmed pair (both cells, so the pair stays
            // consistent); the earlier row is untouched.
            let slot = cache_read_cell(&mut ir, read, carrier, cell(0, 0));
            let generation = cache_read_cell(&mut ir, read, carrier, cell(0, 1));
            let _ = cache_write_cell(
                &mut ir,
                write,
                write_schema.clone(),
                carrier,
                cell(1, 0),
                vm::Expr::Var(slot),
            );
            let _ = cache_write_cell(
                &mut ir,
                write,
                write_schema.clone(),
                carrier,
                cell(1, 1),
                vm::Expr::Var(generation),
            );
        }
        match self.fault {
            CacheFault::None => {}
            CacheFault::KeyGenPlusOne => {
                let live = cache_read_cell(&mut ir, read, carrier, cell(1, 1));
                let _ = cache_write_cell(
                    &mut ir,
                    write,
                    write_schema.clone(),
                    carrier,
                    cell(1, 1),
                    vm::Expr::Add(
                        Box::new(vm::Expr::Var(live)),
                        Box::new(vm::Expr::Float(1.0)),
                    ),
                );
            }
            CacheFault::KeyFrac => {
                let live = cache_read_cell(&mut ir, read, carrier, cell(1, 0));
                let _ = cache_write_cell(
                    &mut ir,
                    write,
                    write_schema.clone(),
                    carrier,
                    cell(1, 0),
                    vm::Expr::Add(
                        Box::new(vm::Expr::Var(live)),
                        Box::new(vm::Expr::Float(0.5)),
                    ),
                );
            }
            CacheFault::KeyNan => {
                let _ = cache_write_cell(
                    &mut ir,
                    write,
                    write_schema.clone(),
                    carrier,
                    cell(1, 1),
                    vm::Expr::Float(f64::NAN),
                );
            }
            CacheFault::KeyOwnerPair => {
                let _ = cache_write_cell(
                    &mut ir,
                    write,
                    write_schema.clone(),
                    carrier,
                    cell(1, 0),
                    probe_num(probe_get(owner, "ref_slot")),
                );
                let _ = cache_write_cell(
                    &mut ir,
                    write,
                    write_schema.clone(),
                    carrier,
                    cell(1, 1),
                    probe_num(probe_get(owner, "generation")),
                );
            }
            CacheFault::KeySlotZero => {
                let _ = cache_write_cell(
                    &mut ir,
                    write,
                    write_schema.clone(),
                    carrier,
                    cell(1, 0),
                    vm::Expr::Float(0.0),
                );
                let _ = cache_write_cell(
                    &mut ir,
                    write,
                    write_schema.clone(),
                    carrier,
                    cell(1, 1),
                    vm::Expr::Float(1.0),
                );
            }
            CacheFault::MetaMalformed => {
                let _ = cache_write_cell(
                    &mut ir,
                    write,
                    write_schema.clone(),
                    carrier,
                    cell(1, 2),
                    vm::Expr::Float(1_000_000.0),
                );
            }
            CacheFault::InactiveNumberFlag => {
                let _ = cache_write_cell(
                    &mut ir,
                    write,
                    write_schema.clone(),
                    carrier,
                    cell(1, 4),
                    vm::Expr::Float(1.0),
                );
            }
            CacheFault::DataRefGen => {
                let _ = cache_write_cell(
                    &mut ir,
                    write,
                    write_schema.clone(),
                    carrier,
                    cell(1, 6),
                    vm::Expr::Float(0.0),
                );
            }
            CacheFault::AccessorGen => {
                let _ = cache_write_cell(
                    &mut ir,
                    write,
                    write_schema.clone(),
                    carrier,
                    cell(1, 6),
                    vm::Expr::Float(0.0),
                );
            }
        }
        probe_push(&mut ir, vm::Expr::Var(marker2));
        match self.consumer {
            RowDiffConsumer::Ordinary => {
                // The ordinary consumer has no cache at all: the same rows are
                // decoded through the unconditional shared validation.
                for prop in [0, 1] {
                    let desc = probe_fixed_row_descriptor(
                        &mut ir,
                        carrier,
                        owner,
                        row(prop),
                        prop,
                        validator,
                        read,
                        decode_int,
                        decode_u32,
                        view,
                        view_schema.clone(),
                        view_args[1].clone(),
                    );
                    probe_push_descriptor_fields(
                        &mut ir,
                        desc,
                        &["present", "writable", "enumerable", "configurable"],
                    );
                }
            }
            RowDiffConsumer::Collector => {
                // Suppress incidental pressure collections: the only
                // collections in this pipeline are the explicit ones below.
                let _pressure = probe_let(
                    &mut ir,
                    write_schema.clone(),
                    probe_call(
                        write,
                        vec![
                            vm::Expr::Var(carrier),
                            vm::Expr::Float(10.0),
                            vm::Expr::Float(0.0),
                        ],
                    ),
                );
                let coll_before = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Int),
                    probe_call(
                        observe,
                        vec![vm::Expr::Var(carrier), vm::Expr::Int(5), vm::Expr::Int(0)],
                    ),
                );
                let pending = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Int),
                    probe_call(
                        observe,
                        vec![vm::Expr::Var(carrier), vm::Expr::Int(8), vm::Expr::Int(0)],
                    ),
                );
                probe_push(&mut ir, vm::Expr::Var(coll_before));
                probe_push(&mut ir, vm::Expr::Var(pending));
                let (root_enter, _) = probe_helper(&ir, "__compat_es5_root_enter");
                let (root_write, root_write_schema) = probe_helper(&ir, "__compat_es5_root_write");
                let (root_leave, root_leave_schema) = probe_helper(&ir, "__compat_es5_root_leave");
                let (collect, collect_schema) = probe_helper(&ir, "__compat_es5_heap_collect");
                let frame = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Int),
                    probe_call(root_enter, vec![vm::Expr::Var(carrier), vm::Expr::Int(1)]),
                );
                let _ack = probe_let(
                    &mut ir,
                    root_write_schema,
                    probe_call(
                        root_write,
                        vec![
                            vm::Expr::Var(carrier),
                            vm::Expr::Var(frame),
                            vm::Expr::Int(0),
                            vm::Expr::Var(owner),
                        ],
                    ),
                );
                let marker3 = probe_let(
                    &mut ir,
                    Some(vm::TypeSchema::Int),
                    vm::Expr::Int(cache_marker(self) + 60),
                );
                probe_push(&mut ir, vm::Expr::Var(marker3));
                for _ in 0..self.collects {
                    let reclaimed = probe_let(
                        &mut ir,
                        collect_schema.clone(),
                        probe_call(collect, vec![vm::Expr::Var(carrier)]),
                    );
                    let coll_after = probe_let(
                        &mut ir,
                        Some(vm::TypeSchema::Int),
                        probe_call(
                            observe,
                            vec![vm::Expr::Var(carrier), vm::Expr::Int(5), vm::Expr::Int(0)],
                        ),
                    );
                    probe_push(&mut ir, vm::Expr::Var(coll_after));
                    probe_push(&mut ir, vm::Expr::Var(reclaimed));
                }
                for prop in [0, 1] {
                    let desc = probe_fixed_row_descriptor(
                        &mut ir,
                        carrier,
                        owner,
                        row(prop),
                        prop,
                        validator,
                        read,
                        decode_int,
                        decode_u32,
                        view,
                        view_schema.clone(),
                        view_args[1].clone(),
                    );
                    probe_push_descriptor_fields(
                        &mut ir,
                        desc,
                        &["present", "writable", "enumerable", "configurable"],
                    );
                }
                let _leave = probe_let(
                    &mut ir,
                    root_leave_schema,
                    probe_call(
                        root_leave,
                        vec![vm::Expr::Var(carrier), vm::Expr::Var(frame)],
                    ),
                );
            }
        }
        Ok(ir)
    }
}

/// Everything the harness pushed after the pipeline marker.
fn cache_tail(outcome: &RowContractOutcome, marker: i64) -> &[Value] {
    let at = outcome
        .stack
        .iter()
        .position(|value| *value == Value::Int(marker))
        .unwrap_or_else(|| panic!("pipeline marker {marker} missing: {:?}", outcome.stack));
    &outcome.stack[at + 1..]
}

fn cache_samples(outcome: &RowContractOutcome) -> [i64; 5] {
    let samples = outcome
        .samples
        .as_ref()
        .unwrap_or_else(|| panic!("cache counters must be sampled: {:?}", outcome.stack));
    assert_eq!(samples.len(), 5, "five sampled counters");
    [samples[0], samples[1], samples[2], samples[3], samples[4]]
}

/// Run one synthetic row-cache pipeline and report the marker that indexes it.
fn cache_run(probe: CacheProbe, source: &'static str) -> (RowContractOutcome, i64) {
    let marker = cache_marker(&probe);
    let outcome = run_row_contract(source, Box::leak(Box::new(probe)));
    eprintln!(
        "P7_ROWcache case={} consumer={:?} fault={:?} dup={} collects={} stop_at={:?} halted={} status={} ip={} samples={:?} stack={:?}",
        probe.case,
        probe.consumer,
        probe.fault,
        probe.dup,
        probe.collects,
        probe.stop_at,
        outcome.halted,
        outcome.status,
        outcome.ip,
        outcome.samples,
        outcome.stack,
    );
    (outcome, marker)
}

/// The completed-collection value reported after each explicit collect.
fn cache_collects(tail: &[Value], collects: usize) -> Vec<(i64, i64)> {
    let mut out = Vec::with_capacity(collects);
    for at in 0..collects {
        match (&tail[4 + at * 2], &tail[5 + at * 2]) {
            (Value::Int(after), Value::Int(reclaimed)) => out.push((*after, *reclaimed)),
            other => panic!("collection evidence: {other:?}"),
        }
    }
    out
}

/// Control pipeline: both rows keep their default descriptors and only the
/// explicit collections ran.
fn cache_control(
    outcome: &RowContractOutcome,
    marker: i64,
    consumer: RowDiffConsumer,
    collects: usize,
    present: [i64; 2],
) {
    assert!(outcome.halted, "control must halt: {}", outcome.status);
    let tail = cache_tail(outcome, marker);
    let pairs = match consumer {
        RowDiffConsumer::Ordinary => 0,
        RowDiffConsumer::Collector => 3 + collects * 2,
    };
    assert_eq!(tail.len(), 1 + pairs + 8, "control stack shape: {tail:?}");
    assert_eq!(tail[0], Value::Int(marker + 40), "post-write marker");
    if consumer == RowDiffConsumer::Collector {
        assert_eq!(
            tail[2],
            Value::Int(0),
            "no pending nodes at explicit collect"
        );
        let collections = cache_collects(tail, collects);
        for pair in collections.windows(2) {
            assert_eq!(pair[1].0, pair[0].0 + 1, "exactly one real collection each");
        }
    }
    assert_eq!(
        &tail[tail.len() - 8..],
        [
            Value::Int(present[0]),
            Value::Bool(false),
            Value::Bool(false),
            Value::Bool(false),
            Value::Int(present[1]),
            Value::Bool(false),
            Value::Bool(false),
            Value::Bool(false),
        ],
        "both rows keep their default descriptors"
    );
}

/// Intentional negative: the consumer reached the corrupted row first and
/// refused there; nothing before the consumer observed the fault.
fn cache_refusal(outcome: &RowContractOutcome, marker: i64, consumer: RowDiffConsumer) {
    assert!(
        !outcome.halted && outcome.status.contains("assertion failed"),
        "intentional-negative refusal: {}",
        outcome.status
    );
    assert!(
        outcome.samples.is_none(),
        "a refused pipeline never reaches the sampling boundary"
    );
    let tail = cache_tail(outcome, marker);
    assert_eq!(
        tail[0],
        Value::Int(marker + 40),
        "post-write marker reached"
    );
    match consumer {
        RowDiffConsumer::Ordinary => {
            // The earlier row is decoded first and keeps its descriptor; the
            // corrupted later row is the first refusal.
            assert_eq!(
                tail[1],
                Value::Int(51),
                "the earlier row's descriptor was observed before the fault"
            );
        }
        RowDiffConsumer::Collector => {
            assert!(
                tail.len() >= 4,
                "the collector was entered and never returned: {tail:?}"
            );
            assert!(
                matches!(tail[1], Value::Int(_)),
                "collection counter debris: {tail:?}"
            );
            assert_eq!(
                tail[2],
                Value::Int(0),
                "no pending node at the explicit collect"
            );
            assert_eq!(tail[3], Value::Int(marker + 60), "entered-collector marker");
        }
    }
    assert_eq!(
        tail.last(),
        Some(&Value::Bool(false)),
        "the refused assert's own condition is the last stack entry"
    );
}

#[test]
fn p7_rowcache_key_hit_and_miss_interpreter() {
    // Same source, same collection; the duplicate key pair is the only
    // difference between the two collector pipelines.
    let (control_derive, m0) = cache_run(
        CacheProbe {
            consumer: RowDiffConsumer::Collector,
            fault: CacheFault::None,
            dup: false,
            collects: 1,
            stop_at: None,
            case: 1,
        },
        CACHE_NUMBER_SOURCE,
    );
    cache_control(&control_derive, m0, RowDiffConsumer::Collector, 1, [51, 51]);
    let (dup_derive, m1) = cache_run(
        CacheProbe {
            consumer: RowDiffConsumer::Collector,
            fault: CacheFault::None,
            dup: true,
            collects: 1,
            stop_at: None,
            case: 2,
        },
        CACHE_NUMBER_SOURCE,
    );
    cache_control(&dup_derive, m1, RowDiffConsumer::Collector, 1, [51, 51]);
    let control_at = cache_collects(cache_tail(&control_derive, m0), 1)[0].0;
    let dup_at = cache_collects(cache_tail(&dup_derive, m1), 1)[0].0;
    assert_eq!(
        control_at, dup_at,
        "the duplicate pair must not change the collection ordinal"
    );
    let (control, _mc) = cache_run(
        CacheProbe {
            consumer: RowDiffConsumer::Collector,
            fault: CacheFault::None,
            dup: false,
            collects: 1,
            stop_at: Some(control_at),
            case: 3,
        },
        CACHE_NUMBER_SOURCE,
    );
    let (dup, _md) = cache_run(
        CacheProbe {
            consumer: RowDiffConsumer::Collector,
            fault: CacheFault::None,
            dup: true,
            collects: 1,
            stop_at: Some(dup_at),
            case: 4,
        },
        CACHE_NUMBER_SOURCE,
    );
    let base = cache_samples(&control);
    let reused = cache_samples(&dup);
    eprintln!(
        "P7_ROWcache_HIT collections={} object_hits={} object_misses={} function_hits={} function_misses={}",
        reused[0], reused[1], reused[2], reused[3], reused[4]
    );
    eprintln!(
        "P7_ROWcache_MISS collections={} object_hits={} object_misses={} function_hits={} function_misses={}",
        base[0], base[1], base[2], base[3], base[4]
    );
    assert_eq!(base[0], control_at, "sampled at the measured boundary");
    assert_eq!(reused[0], dup_at, "sampled at the measured boundary");
    assert_eq!(
        reused[1],
        base[1] + 1,
        "the reused validated pair adds exactly one object-arm hit"
    );
    assert_eq!(
        reused[2],
        base[2] - 1,
        "the hit replaces exactly one object-arm miss"
    );
    assert_eq!(
        reused[1] + reused[2],
        base[1] + base[2],
        "the same number of object rows is still validated"
    );
    assert_eq!(reused[3], base[3], "function-arm hits unchanged");
    assert_eq!(reused[4], base[4], "function-arm misses unchanged");

    // The ordinary consumer has no cache: the same rows decode through the
    // unconditional shared validation with identical descriptors.
    let (ordinary_control, m2) = cache_run(
        CacheProbe {
            consumer: RowDiffConsumer::Ordinary,
            fault: CacheFault::None,
            dup: false,
            collects: 1,
            stop_at: None,
            case: 5,
        },
        CACHE_NUMBER_SOURCE,
    );
    let (ordinary_reuse, m3) = cache_run(
        CacheProbe {
            consumer: RowDiffConsumer::Ordinary,
            fault: CacheFault::None,
            dup: true,
            collects: 1,
            stop_at: None,
            case: 6,
        },
        CACHE_NUMBER_SOURCE,
    );
    cache_control(
        &ordinary_control,
        m2,
        RowDiffConsumer::Ordinary,
        1,
        [51, 51],
    );
    cache_control(&ordinary_reuse, m3, RowDiffConsumer::Ordinary, 1, [51, 51]);
    assert_eq!(
        cache_tail(&ordinary_control, m2)[1..],
        cache_tail(&ordinary_reuse, m3)[1..],
        "ordinary descriptors unchanged by the reused pair"
    );
}

#[test]
fn p7_rowcache_resets_each_collection_interpreter() {
    let (derive, m) = cache_run(
        CacheProbe {
            consumer: RowDiffConsumer::Collector,
            fault: CacheFault::None,
            dup: true,
            collects: 2,
            stop_at: None,
            case: 7,
        },
        CACHE_NUMBER_SOURCE,
    );
    cache_control(&derive, m, RowDiffConsumer::Collector, 2, [51, 51]);
    let collections = cache_collects(cache_tail(&derive, m), 2);
    let (first_at, second_at) = (collections[0].0, collections[1].0);
    assert_eq!(second_at, first_at + 1, "two completed collections");
    let (first, _) = cache_run(
        CacheProbe {
            consumer: RowDiffConsumer::Collector,
            fault: CacheFault::None,
            dup: true,
            collects: 2,
            stop_at: Some(first_at),
            case: 8,
        },
        CACHE_NUMBER_SOURCE,
    );
    let (second, _) = cache_run(
        CacheProbe {
            consumer: RowDiffConsumer::Collector,
            fault: CacheFault::None,
            dup: true,
            collects: 2,
            stop_at: Some(second_at),
            case: 9,
        },
        CACHE_NUMBER_SOURCE,
    );
    let first = cache_samples(&first);
    let second = cache_samples(&second);
    eprintln!(
        "P7_ROWcache_RESET first collections={} hits={} misses={} second collections={} hits={} misses={}",
        first[0], first[1], first[2], second[0], second[1], second[2]
    );
    assert_eq!(first[0], first_at);
    assert_eq!(second[0], second_at);
    assert!(
        first[2] >= 1 && second[2] >= 1,
        "every collection re-validates at least one key: {first:?} {second:?}"
    );
    assert_eq!(
        second[1], first[1],
        "the second collection starts invalid: its first row misses again instead of reusing the first collection's pair"
    );
    assert_eq!(second[2], first[2], "and re-validates the same pairs");
}

#[test]
fn p7_rowcache_function_rows_measured_interpreter() {
    // No synthetic rewrite at all: a plain collection over the live intrinsic
    // realm. If the emitted Function rows share allocated key strings, the
    // function arm hits on its own.
    let (derive, m) = cache_run(
        CacheProbe {
            consumer: RowDiffConsumer::Collector,
            fault: CacheFault::None,
            dup: false,
            collects: 1,
            stop_at: None,
            case: 10,
        },
        CACHE_NUMBER_SOURCE,
    );
    cache_control(&derive, m, RowDiffConsumer::Collector, 1, [51, 51]);
    let at = cache_collects(cache_tail(&derive, m), 1)[0].0;
    let (probe, _) = cache_run(
        CacheProbe {
            consumer: RowDiffConsumer::Collector,
            fault: CacheFault::None,
            dup: false,
            collects: 1,
            stop_at: Some(at),
            case: 11,
        },
        CACHE_NUMBER_SOURCE,
    );
    let sample = cache_samples(&probe);
    eprintln!(
        "P7_ROWcache_FUNCTION_ROWS collections={} object_hits={} object_misses={} function_hits={} function_misses={}",
        sample[0], sample[1], sample[2], sample[3], sample[4]
    );
    assert_eq!(sample[1], 0, "one object row with a unique key cannot hit");
    assert!(
        sample[4] > 0,
        "intrinsic Function rows must re-validate at least one key pair"
    );
    assert!(
        sample[3] > 0,
        "the measured intrinsic key reuse must show up as function-arm hits, not misses"
    );
}

#[test]
fn p7_rowcache_key_pair_corruption_interpreter() {
    for (fault, case) in [
        (CacheFault::KeyGenPlusOne, 12),
        (CacheFault::KeyOwnerPair, 14),
    ] {
        for consumer in [RowDiffConsumer::Ordinary, RowDiffConsumer::Collector] {
            let (outcome, marker) = cache_run(
                CacheProbe {
                    consumer,
                    fault,
                    dup: false,
                    collects: 1,
                    stop_at: None,
                    case: case + i64::from(consumer == RowDiffConsumer::Collector),
                },
                CACHE_NUMBER_SOURCE,
            );
            cache_refusal(&outcome, marker, consumer);
        }
    }
}

#[test]
fn p7_rowcache_key_decode_corruption_interpreter() {
    for (fault, case) in [(CacheFault::KeyFrac, 16), (CacheFault::KeyNan, 18)] {
        for consumer in [RowDiffConsumer::Ordinary, RowDiffConsumer::Collector] {
            let (outcome, marker) = cache_run(
                CacheProbe {
                    consumer,
                    fault,
                    dup: false,
                    collects: 1,
                    stop_at: None,
                    case: case + i64::from(consumer == RowDiffConsumer::Collector),
                },
                CACHE_NUMBER_SOURCE,
            );
            cache_refusal(&outcome, marker, consumer);
        }
    }
}

#[test]
fn p7_rowcache_payload_after_hit_number_interpreter() {
    // A warmed, still-valid cached pair on the later row, plus a payload or
    // metadata fault: the cache decision cannot hide the rest of the row.
    let (derive, m) = cache_run(
        CacheProbe {
            consumer: RowDiffConsumer::Collector,
            fault: CacheFault::None,
            dup: true,
            collects: 1,
            stop_at: None,
            case: 20,
        },
        CACHE_NUMBER_SOURCE,
    );
    cache_control(&derive, m, RowDiffConsumer::Collector, 1, [51, 51]);
    let at = cache_collects(cache_tail(&derive, m), 1)[0].0;
    let (hit, _) = cache_run(
        CacheProbe {
            consumer: RowDiffConsumer::Collector,
            fault: CacheFault::None,
            dup: true,
            collects: 1,
            stop_at: Some(at),
            case: 21,
        },
        CACHE_NUMBER_SOURCE,
    );
    let hit = cache_samples(&hit);
    eprintln!(
        "P7_ROWcache_PAYLOAD_CONTROL collections={} object_hits={} object_misses={} function_hits={} function_misses={}",
        hit[0], hit[1], hit[2], hit[3], hit[4]
    );
    assert!(
        hit[1] >= 1,
        "the matched control must take the object-arm hit the faulted rows take"
    );
    for (fault, case) in [
        (CacheFault::MetaMalformed, 22),
        (CacheFault::InactiveNumberFlag, 24),
    ] {
        for consumer in [RowDiffConsumer::Ordinary, RowDiffConsumer::Collector] {
            let (outcome, marker) = cache_run(
                CacheProbe {
                    consumer,
                    fault,
                    dup: true,
                    collects: 1,
                    stop_at: None,
                    case: case + i64::from(consumer == RowDiffConsumer::Collector),
                },
                CACHE_NUMBER_SOURCE,
            );
            cache_refusal(&outcome, marker, consumer);
        }
    }
}

#[test]
fn p7_rowcache_payload_after_hit_reference_interpreter() {
    for (fault, case, source) in [
        (CacheFault::DataRefGen, 26, CACHE_STRING_SOURCE),
        (CacheFault::AccessorGen, 28, CACHE_ACCESSOR_SOURCE),
    ] {
        // Matched control first: the same rewritten pair halts with the tag's
        // own data/accessor validation intact.
        let (control, mc) = cache_run(
            CacheProbe {
                consumer: RowDiffConsumer::Collector,
                fault: CacheFault::None,
                dup: true,
                collects: 1,
                stop_at: None,
                case,
            },
            source,
        );
        cache_control(
            &control,
            mc,
            RowDiffConsumer::Collector,
            1,
            if fault == CacheFault::DataRefGen {
                [51, 51]
            } else {
                [51, 60]
            },
        );
        for consumer in [RowDiffConsumer::Ordinary, RowDiffConsumer::Collector] {
            let (outcome, marker) = cache_run(
                CacheProbe {
                    consumer,
                    fault,
                    dup: true,
                    collects: 1,
                    stop_at: None,
                    case: case + 1 + i64::from(consumer == RowDiffConsumer::Collector),
                },
                source,
            );
            cache_refusal(&outcome, marker, consumer);
        }
    }
}

/// Compile-only probe: asserts the emitted production collector carries the
/// exact cache contract, then hands the IR through unchanged.
struct CacheShapeProbe;

impl vm::SourcePlugin for CacheShapeProbe {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN)
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        let ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
        object_observer::assert_cache_shape(&ir);
        Ok(ir)
    }
}

#[test]
fn p7_rowcache_slot_zero_and_shape_program() {
    // Compile-only: the production collector IR must carry the dedicated
    // validity Bool, a value-only slot/generation compare (no zero sentinel)
    // and the latch-after-proof order. This is a program-level contract, not a
    // JS key-semantics proof.
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        CACHE_NUMBER_SOURCE,
        javascript_es5_flavor(),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(Box::leak(Box::new(CacheShapeProbe)))
            .with_host_api_catalog(catalog),
    );
    match compiled {
        Ok(_) => {}
        Err(error) => panic!("cache shape contract must hold on the emitted IR:\n{error:?}"),
    }
    // Slot 0 is the realm's first permanent node, not a String. The pipeline
    // resolves it through the shared checked_base before the marker (so a
    // missing marker would mean slot 0 was not live), then the row key brand
    // must refuse it in both consumers.
    for consumer in [RowDiffConsumer::Ordinary, RowDiffConsumer::Collector] {
        let (outcome, marker) = cache_run(
            CacheProbe {
                consumer,
                fault: CacheFault::KeySlotZero,
                dup: false,
                collects: 1,
                stop_at: None,
                case: 30 + i64::from(consumer == RowDiffConsumer::Collector),
            },
            CACHE_NUMBER_SOURCE,
        );
        cache_refusal(&outcome, marker, consumer);
        eprintln!(
            "P7_ROWcache_SLOT_ZERO consumer={consumer:?} refused=true status={}",
            outcome.status
        );
    }
}

// ---- P7 production certification: literal descriptor key base ----
// The descriptor-row validator returns the validated Int key base and the
// literal-key HasProperty path assigns that result directly, so this public
// source exercises the real Object.defineProperty path with no IR rewrite.
// It is the current production contract; the retired A-E transform experiment
// and its collector apparatus are no longer executable here.

#[test]
fn p7_define_property_literal_key_source_interpreter() {
    let source = "var o={};Object.defineProperty(o,'x',{value:7});o.x===7;";
    let actual = Runner { native: false }.run(source, None);
    let last = actual.last().expect("script completion value");
    let truth = match last {
        Value::Bool(true) => true,
        Value::Map(map) => {
            map.get(&Value::string("tag")) == Some(&Value::Int(2))
                && map.get(&Value::string("flag")) == Some(&Value::Bool(true))
        }
        other => panic!("defineProperty source completion: {other:?}"),
    };
    assert!(truth, "defineProperty source must complete true: {last:?}");
    eprintln!("P7_LITERAL_KEY source=public certified=true halted=true truth=boxed");
}

/// Bounded D01 PHASE/HELPER diagnostic on the unchanged business source.
///
/// One fresh `collect_probe` full run under the original force-32/10M
/// interpreter-only budget first; every observation is dumped before any
/// assertion. Halted is accepted only as observed completion (report then
/// stop); Fuel yield with Fuel reason and zero fuel is the only accepted
/// exhaustion; any Err fails after its dump. An active collector ordinal is
/// read only from the live `parameter_slots[1]` + `frame.local_base` entry;
/// absent helper means unknown current completed count, never zero history.
/// Up to seven additional fresh phase prefixes from the observed ordinal
/// (INIT/CLEAR/ROOTS/DRAIN/SWEEP/COALESCE/COMMIT); otherwise at most one
/// existing `d01_probe` full run binds the terminal helper. Total <=8 VMs.
#[test]
fn p7_d01_collector_phase_binding_diagnostic_interpreter() {
    let probe = collect_probe(D01_FORCED_SOURCE, None);
    eprintln!(
        "D01_DIAG_FULL fuel={:?}->{:?} jit={} aot={} native={} status={:?} reason={:?} ip={} phase={:?} label={:?} site={} completed_at_entry={:?} helper_prototype={:?} helper_region={:?} frames={:?}",
        probe.fuel_before,
        probe.fuel_after,
        probe.jit_enabled,
        probe.aot_executions,
        probe.jit_native_executions,
        probe.status,
        probe.reason,
        probe.ip,
        probe.phase,
        probe.phase_label,
        probe.phase_site,
        probe.completed_at_entry,
        probe.helper_prototype,
        probe.helper_region,
        probe.frames
    );
    assert!(
        !probe.jit_enabled,
        "diagnostic D01 run must stay interpreter-only"
    );
    assert_eq!(probe.aot_executions, 0, "no AOT execution");
    assert_eq!(probe.jit_native_executions, 0, "no native trace execution");
    assert_eq!(
        probe.fuel_before,
        Some(10_000_000),
        "the diagnostic keeps the original 10M budget"
    );
    match &probe.status {
        Ok(VmStatus::Halted) => {
            eprintln!(
                "D01_DIAG_HALTED status=Halted ip={} phase={:?} label={:?} site={} completed_at_entry={:?} helper={:?} region={:?} frames={:?} (observed completion; stop, no error masked)",
                probe.ip,
                probe.phase,
                probe.phase_label,
                probe.phase_site,
                probe.completed_at_entry,
                probe.helper_prototype,
                probe.helper_region,
                probe.frames
            );
            eprintln!("D01_PIPELINES actual=1 full=1 collect_stop=0 halted=1");
            return;
        }
        Ok(VmStatus::Yielded) => {
            assert_eq!(
                probe.reason,
                Some(vm::VmYieldReason::Fuel),
                "fuel yield must carry the Fuel reason, got {:?}",
                probe.reason
            );
            assert_eq!(
                probe.fuel_after,
                Some(0),
                "actual exhaustion leaves zero fuel, got {:?}",
                probe.fuel_after
            );
        }
        Err(error) => {
            panic!(
                "D01_DIAG_UNEXPECTED_ERR status={:?} reason={:?} ip={} phase={:?} label={:?} site={} completed_at_entry={:?} helper={:?} region={:?} frames={:?}",
                error,
                probe.reason,
                probe.ip,
                probe.phase,
                probe.phase_label,
                probe.phase_site,
                probe.completed_at_entry,
                probe.helper_prototype,
                probe.helper_region,
                probe.frames
            );
        }
        other => {
            panic!(
                "D01_DIAG_UNEXPECTED_STATUS status={:?} reason={:?} ip={} phase={:?} label={:?}",
                other, probe.reason, probe.ip, probe.phase, probe.phase_label
            );
        }
    }
    if let Some(completed) = probe.completed_at_entry {
        eprintln!(
            "D01_DIAG_COLLECTOR active=true completed_at_entry={completed} phase={:?} label={:?} site={} helper={:?} region={:?} frames={:?}",
            probe.phase,
            probe.phase_label,
            probe.phase_site,
            probe.helper_prototype,
            probe.helper_region,
            probe.frames
        );
        let boundaries = [
            ("init", object_observer::DIAG_PHASE_INIT),
            ("clear-marks", object_observer::DIAG_PHASE_CLEAR),
            ("root-scan", object_observer::DIAG_PHASE_ROOTS),
            ("gray-drain", object_observer::DIAG_PHASE_DRAIN),
            ("sweep", object_observer::DIAG_PHASE_SWEEP),
            ("coalesce", object_observer::DIAG_PHASE_COALESCE),
            ("counter-commit", object_observer::DIAG_PHASE_COMMIT),
        ];
        let mut remaining = Vec::with_capacity(boundaries.len());
        for (name, label) in boundaries {
            let prefix = collect_probe(D01_FORCED_SOURCE, Some((completed, label)));
            eprintln!(
                "D01_PREFIX phase={name} expected_completed={completed} fuel_after={:?} status={:?} reason={:?} ip={} label={:?} site={} completed_at_entry={:?} frames={:?}",
                prefix.fuel_after,
                prefix.status,
                prefix.reason,
                prefix.ip,
                prefix.phase_label,
                prefix.phase_site,
                prefix.completed_at_entry,
                prefix.frames
            );
            assert!(
                !prefix.jit_enabled,
                "phase prefix {name} must stay interpreter-only"
            );
            assert_eq!(prefix.aot_executions, 0, "no AOT execution");
            assert_eq!(prefix.jit_native_executions, 0, "no native trace execution");
            match &prefix.status {
                Err(vm::VmError::HostError(message)) if message == "assertion failed" => {
                    let fuel = prefix.fuel_after.expect("phase stop must report fuel");
                    eprintln!(
                        "D01_PREFIX_REACHED phase={name} remaining_fuel={fuel} consumed_fuel={} completed_at_entry={:?}",
                        10_000_000 - fuel,
                        prefix.completed_at_entry
                    );
                    assert_eq!(
                        prefix.completed_at_entry,
                        Some(completed),
                        "phase {name} must observe the actual ordinal"
                    );
                    remaining.push((name, fuel));
                }
                Ok(VmStatus::Yielded)
                    if prefix.reason == Some(vm::VmYieldReason::Fuel)
                        && prefix.fuel_after == Some(0) =>
                {
                    eprintln!(
                        "D01_PREFIX_UNREACHED phase={name} expected_completed={completed} fuel exhausted before target"
                    );
                }
                other => {
                    panic!(
                        "D01_PREFIX_UNEXPECTED phase={name} status={:?} reason={:?} fuel={:?} ip={} label={:?}",
                        other, prefix.reason, prefix.fuel_after, prefix.ip, prefix.phase_label
                    );
                }
            }
        }
        for window in remaining.windows(2) {
            let ((prev_name, prev_fuel), (name, fuel)) = (window[0], window[1]);
            assert!(
                prev_fuel >= fuel,
                "fuel must not increase from {prev_name} to {name}: {prev_fuel} -> {fuel}"
            );
            eprintln!(
                "D01_PREFIX_COST from={prev_name} to={name} inclusive_instrumented_fuel={} (includes diagnostic wrapper/assert overhead; not per-GC cost)",
                prev_fuel - fuel
            );
        }
        eprintln!(
            "D01_PIPELINES actual={} full=1 collect_stop={} (bounded phase prefixes from observed ordinal {completed})",
            1 + boundaries.len(),
            boundaries.len()
        );
        return;
    }
    eprintln!(
        "D01_DIAG_COLLECTOR active=false completed_at_entry=None phase={:?} label={:?} site={} helper={:?} region={:?} frames={:?} (no active collector ordinal; current completed count unknown, never zero history)",
        probe.phase,
        probe.phase_label,
        probe.phase_site,
        probe.helper_prototype,
        probe.helper_region,
        probe.frames
    );
    let terminal = d01_probe(D01_FORCED_SOURCE, None);
    eprintln!(
        "D01_DIAG_TERMINAL fuel_after={:?} status={:?} reason={:?} ip={} line={:?} phase={:?} root_proto={:?} capacity={:?} cursor={:?} sample={:?} site={:?} named_frames={:?} frames={:?}",
        terminal.fuel_after,
        terminal.status,
        terminal.reason,
        terminal.ip,
        terminal.phase_label,
        terminal.phase,
        terminal.root_prototype,
        terminal.frame_capacity,
        terminal.cursor,
        terminal.sample,
        terminal.site,
        terminal.named_frames,
        terminal.frames
    );
    assert!(
        !terminal.jit_enabled,
        "the D01 descriptor run must stay interpreter-only"
    );
    assert_eq!(terminal.aot_executions, 0, "no AOT execution");
    assert_eq!(
        terminal.jit_native_executions, 0,
        "no native trace execution"
    );
    eprintln!(
        "D01_PIPELINES actual=2 full=2 collect_stop=0 (terminal helper bound, no guessed phase prefix)"
    );
}

/// Bounded D01 full-only out-of-region phase attribution counterexample.
///
/// Constructs and runs ONE fresh instrumented VM via `collect_probe` on the
/// unchanged business source under the original 10M interpreter-only budget.
/// This is not a compile-only check and makes no claim that full collection
/// completed, no net-cost claim, and no API-gate claim.
#[test]
fn p7_d01_full_out_of_region_phase_attribution_interpreter() {
    let probe = collect_probe(D01_FORCED_SOURCE, None);
    eprintln!(
        "D01_PHASE_FULL_ONLY fuel={:?}->{:?} jit={} aot={} native={} status={:?} reason={:?} ip={} phase={:?} label={:?} site={} completed_at_entry={:?} helper_prototype={:?} helper_region={:?} frames={:?}",
        probe.fuel_before,
        probe.fuel_after,
        probe.jit_enabled,
        probe.aot_executions,
        probe.jit_native_executions,
        probe.status,
        probe.reason,
        probe.ip,
        probe.phase,
        probe.phase_label,
        probe.phase_site,
        probe.completed_at_entry,
        probe.helper_prototype,
        probe.helper_region,
        probe.frames
    );
    assert_eq!(
        probe.fuel_before,
        Some(10_000_000),
        "diagnostic keeps the original 10M budget"
    );
    assert!(
        !probe.jit_enabled,
        "diagnostic D01 run must stay interpreter-only"
    );
    assert_eq!(probe.aot_executions, 0, "no AOT execution");
    assert_eq!(probe.jit_native_executions, 0, "no native trace execution");
    assert!(
        matches!(probe.status, Ok(VmStatus::Yielded)),
        "this regression fixture must yield, got {:?}",
        probe.status
    );
    assert_eq!(
        probe.reason,
        Some(vm::VmYieldReason::Fuel),
        "fuel yield must carry the Fuel reason"
    );
    assert_eq!(
        probe.fuel_after,
        Some(0),
        "actual exhaustion leaves zero fuel"
    );
    let helper = probe
        .helper_prototype
        .expect("bound helper prototype must exist");
    let (start, end, _start_label) = probe.helper_region.expect("bound helper region must exist");
    assert!(
        probe.ip < start as usize || probe.ip >= end as usize,
        "bug precondition: terminal ip {} must be outside bound helper region {}..{} (helper {helper:?})",
        probe.ip,
        start,
        end
    );
    let completed = probe
        .completed_at_entry
        .expect("completed-at-entry ordinal must be live");
    assert!(completed >= 0, "completed collection count is nonnegative");
    assert_eq!(
        probe.phase_site, "direct-callee",
        "D01_PHASE_REQUIRES_DIRECT_CALLEE: out-of-region ip must be attributed only via a direct-callee continuation, got site {} ip {} label {:?} phase {:?}",
        probe.phase_site, probe.ip, probe.phase_label, probe.phase
    );
    assert_eq!(
        probe.phase,
        Some("gray-drain"),
        "out-of-region attribution must resolve to gray-drain, got {:?}",
        probe.phase
    );
    assert_eq!(
        probe.phase_label,
        Some(object_observer::DIAG_PHASE_DRAIN),
        "phase label must be DIAG_PHASE_DRAIN"
    );
}

/// The collector ROOTS loop must decode the previous-frame link exactly once
/// per visited frame and reuse that single collector-local slot in both the
/// bottom-boundary guard and the frame advance. The pre-fix shape inlines two
/// decode-read producers (one inside the guard, one in the advance); this
/// structural contract fails on that shape and passes on the single-producer
/// shape. IR capture only, no VM execution.
#[test]
fn p7_roots_previous_link_single_producer_program_structure() {
    #[derive(Debug, Default)]
    struct Ctx {
        frame: u16,
        decode: u16,
        read: u16,
        carrier: u16,
    }
    #[derive(Debug, Default)]
    struct Summary {
        loops: usize,
        producers: usize,
        inner_index: Option<usize>,
        binder: Option<(usize, u16)>,
        guard: Option<usize>,
        advance: Option<usize>,
        bound_int_declared: bool,
    }
    /// The single original decode-read producer: the exact 17-bit slot
    /// decoder applied to the B1 read of the carrier at `frame + 2`
    /// (`RAW_FREE_NEXT`). Both helper identities and the carrier slot come
    /// from the actual IR; any second inline decode (the pre-fix
    /// guard/advance duplication) fails the single-producer count below.
    fn is_previous_producer(expr: &vm::Expr, ctx: &Ctx) -> bool {
        match expr {
            vm::Expr::Call(decode, _, args, _, _) if *decode == ctx.decode && args.len() == 1 => {
                match &args[0] {
                    vm::Expr::Call(read, _, inner, _, _)
                        if *read == ctx.read && inner.len() == 2 =>
                    {
                        matches!(&inner[0], vm::Expr::Var(v) if *v == ctx.carrier)
                            && matches!(
                                &inner[1],
                                vm::Expr::Div(addr, unit)
                                    if matches!(
                                        addr.as_ref(),
                                        vm::Expr::Add(base, off)
                                            if matches!(
                                                base.as_ref(),
                                                vm::Expr::Var(v) if *v == ctx.frame
                                            ) && matches!(off.as_ref(), vm::Expr::Int(2))
                                    ) && matches!(unit.as_ref(), vm::Expr::Float(f) if *f == 1.0)
                            )
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }
    fn count_closure_producers(closure: &vm::ClosureExpr, ctx: &Ctx) -> usize {
        count_producers(&closure.body, ctx)
    }
    fn count_producers(expr: &vm::Expr, ctx: &Ctx) -> usize {
        let here = usize::from(is_previous_producer(expr, ctx));
        here + match expr {
            vm::Expr::Call(_, _, args, _, _)
            | vm::Expr::ModuleCall(_, _, args, _)
            | vm::Expr::LocalCall(_, _, args, _) => {
                args.iter().map(|a| count_producers(a, ctx)).sum()
            }
            vm::Expr::ClosureCall(closure, args) => {
                count_closure_producers(closure, ctx)
                    + args.iter().map(|a| count_producers(a, ctx)).sum::<usize>()
            }
            vm::Expr::Closure(closure) => count_closure_producers(closure, ctx),
            vm::Expr::Add(l, r)
            | vm::Expr::Sub(l, r)
            | vm::Expr::Mul(l, r)
            | vm::Expr::Div(l, r)
            | vm::Expr::Mod(l, r)
            | vm::Expr::Eq(l, r)
            | vm::Expr::Lt(l, r)
            | vm::Expr::Gt(l, r)
            | vm::Expr::And(l, r)
            | vm::Expr::Or(l, r) => count_producers(l, ctx) + count_producers(r, ctx),
            vm::Expr::Neg(e)
            | vm::Expr::Not(e)
            | vm::Expr::ToOwned(e)
            | vm::Expr::Borrow(e)
            | vm::Expr::BorrowMut(e) => count_producers(e, ctx),
            vm::Expr::IfElse {
                condition,
                then_expr,
                else_expr,
            } => {
                count_producers(condition, ctx)
                    + count_producers(then_expr, ctx)
                    + count_producers(else_expr, ctx)
            }
            vm::Expr::Match {
                value,
                arms,
                default,
                ..
            } => {
                count_producers(value, ctx)
                    + arms
                        .iter()
                        .map(|(_, e)| count_producers(e, ctx))
                        .sum::<usize>()
                    + count_producers(default, ctx)
            }
            vm::Expr::Block { stmts, expr } => {
                stmts
                    .iter()
                    .map(|s| count_stmt_producers(s, ctx))
                    .sum::<usize>()
                    + count_producers(expr, ctx)
            }
            vm::Expr::ResultConstruct { payload, .. } => count_producers(payload, ctx),
            vm::Expr::ResultMatch {
                value,
                ok_expr,
                err_expr,
                ..
            } => {
                count_producers(value, ctx)
                    + count_producers(ok_expr, ctx)
                    + count_producers(err_expr, ctx)
            }
            vm::Expr::ResultQuery { value, .. } | vm::Expr::ResultTry { value, .. } => {
                count_producers(value, ctx)
            }
            vm::Expr::OptionalGet { container, key, .. } => {
                count_producers(container, ctx) + count_producers(key, ctx)
            }
            vm::Expr::OptionUnwrapOr {
                value, fallback, ..
            } => count_producers(value, ctx) + count_producers(fallback, ctx),
            vm::Expr::Null
            | vm::Expr::Int(_)
            | vm::Expr::Float(_)
            | vm::Expr::Bool(_)
            | vm::Expr::String(_)
            | vm::Expr::Bytes(_)
            | vm::Expr::FunctionRef(_, _)
            | vm::Expr::ModuleFunctionRef(_, _)
            | vm::Expr::Var(_)
            | vm::Expr::MoveVar(_)
            | vm::Expr::MoveField { .. }
            | vm::Expr::MoveIndex { .. } => 0,
            _ => panic!("unexpected expression inside the ROOTS loop: {expr:?}"),
        }
    }
    fn count_stmt_producers(stmt: &vm::Stmt, ctx: &Ctx) -> usize {
        match stmt {
            vm::Stmt::While {
                condition, body, ..
            } => {
                count_producers(condition, ctx)
                    + body
                        .iter()
                        .map(|s| count_stmt_producers(s, ctx))
                        .sum::<usize>()
            }
            vm::Stmt::For {
                init,
                condition,
                post,
                body,
                ..
            } => {
                count_stmt_producers(init, ctx)
                    + count_producers(condition, ctx)
                    + count_stmt_producers(post, ctx)
                    + body
                        .iter()
                        .map(|s| count_stmt_producers(s, ctx))
                        .sum::<usize>()
            }
            vm::Stmt::IfElse {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                count_producers(condition, ctx)
                    + then_branch
                        .iter()
                        .map(|s| count_stmt_producers(s, ctx))
                        .sum::<usize>()
                    + else_branch
                        .iter()
                        .map(|s| count_stmt_producers(s, ctx))
                        .sum::<usize>()
            }
            vm::Stmt::Assign { expr, .. }
            | vm::Stmt::Expr { expr, .. }
            | vm::Stmt::Let { expr, .. } => count_producers(expr, ctx),
            vm::Stmt::Noop { .. }
            | vm::Stmt::Break { .. }
            | vm::Stmt::Continue { .. }
            | vm::Stmt::Drop { .. }
            | vm::Stmt::FuncDecl { .. } => 0,
            _ => panic!("unexpected statement inside the ROOTS loop: {stmt:?}"),
        }
    }
    fn expr_uses(expr: &vm::Expr, slot: u16) -> bool {
        match expr {
            vm::Expr::Var(v) | vm::Expr::MoveVar(v) => *v == slot,
            vm::Expr::MoveField { root, .. } | vm::Expr::MoveIndex { root, .. } => *root == slot,
            vm::Expr::Call(_, _, args, _, _)
            | vm::Expr::ModuleCall(_, _, args, _)
            | vm::Expr::LocalCall(_, _, args, _) => args.iter().any(|a| expr_uses(a, slot)),
            vm::Expr::ClosureCall(closure, args) => {
                expr_uses(&closure.body, slot) || args.iter().any(|a| expr_uses(a, slot))
            }
            vm::Expr::Closure(closure) => expr_uses(&closure.body, slot),
            vm::Expr::Add(l, r)
            | vm::Expr::Sub(l, r)
            | vm::Expr::Mul(l, r)
            | vm::Expr::Div(l, r)
            | vm::Expr::Mod(l, r)
            | vm::Expr::Eq(l, r)
            | vm::Expr::Lt(l, r)
            | vm::Expr::Gt(l, r)
            | vm::Expr::And(l, r)
            | vm::Expr::Or(l, r) => expr_uses(l, slot) || expr_uses(r, slot),
            vm::Expr::Neg(e)
            | vm::Expr::Not(e)
            | vm::Expr::ToOwned(e)
            | vm::Expr::Borrow(e)
            | vm::Expr::BorrowMut(e) => expr_uses(e, slot),
            vm::Expr::IfElse {
                condition,
                then_expr,
                else_expr,
            } => {
                expr_uses(condition, slot)
                    || expr_uses(then_expr, slot)
                    || expr_uses(else_expr, slot)
            }
            vm::Expr::Match {
                value,
                arms,
                default,
                ..
            } => {
                expr_uses(value, slot)
                    || arms.iter().any(|(_, e)| expr_uses(e, slot))
                    || expr_uses(default, slot)
            }
            vm::Expr::Block { .. } => {
                panic!("unexpected block inside the ROOTS loop: {expr:?}")
            }
            vm::Expr::ResultConstruct { payload, .. } => expr_uses(payload, slot),
            vm::Expr::ResultMatch {
                value,
                ok_expr,
                err_expr,
                ..
            } => expr_uses(value, slot) || expr_uses(ok_expr, slot) || expr_uses(err_expr, slot),
            vm::Expr::ResultQuery { value, .. } | vm::Expr::ResultTry { value, .. } => {
                expr_uses(value, slot)
            }
            vm::Expr::OptionalGet { container, key, .. } => {
                expr_uses(container, slot) || expr_uses(key, slot)
            }
            vm::Expr::OptionUnwrapOr {
                value, fallback, ..
            } => expr_uses(value, slot) || expr_uses(fallback, slot),
            vm::Expr::Null
            | vm::Expr::Int(_)
            | vm::Expr::Float(_)
            | vm::Expr::Bool(_)
            | vm::Expr::String(_)
            | vm::Expr::Bytes(_)
            | vm::Expr::FunctionRef(_, _)
            | vm::Expr::ModuleFunctionRef(_, _) => false,
            _ => panic!("unexpected expression inside the ROOTS loop: {expr:?}"),
        }
    }
    /// Any write (local set or explicit drop) to `slot`, walking nested
    /// branches so a hidden rebinding between the binder and the advance
    /// cannot silently pass.
    fn stmt_writes(stmt: &vm::Stmt, slot: u16) -> bool {
        match stmt {
            vm::Stmt::Let { index, .. }
            | vm::Stmt::Assign { index, .. }
            | vm::Stmt::Drop { index, .. } => *index == slot,
            vm::Stmt::While { body, .. } => body.iter().any(|s| stmt_writes(s, slot)),
            vm::Stmt::For {
                init, post, body, ..
            } => {
                stmt_writes(init, slot)
                    || stmt_writes(post, slot)
                    || body.iter().any(|s| stmt_writes(s, slot))
            }
            vm::Stmt::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                then_branch.iter().any(|s| stmt_writes(s, slot))
                    || else_branch.iter().any(|s| stmt_writes(s, slot))
            }
            vm::Stmt::Noop { .. }
            | vm::Stmt::Expr { .. }
            | vm::Stmt::Break { .. }
            | vm::Stmt::Continue { .. }
            | vm::Stmt::FuncDecl { .. } => false,
            _ => panic!("unexpected statement inside the ROOTS loop: {stmt:?}"),
        }
    }
    struct Tap(std::sync::Mutex<Summary>);
    impl vm::SourcePlugin for Tap {
        fn descriptor(&self) -> vm::SourceFrontendDescriptor {
            vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN)
        }
        fn parse_source(
            &self,
            source: &str,
            context: &vm::SourceFrontendContext<'_>,
        ) -> Result<vm::FrontendIr, vm::ParseError> {
            let ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
            let collect = ir
                .functions
                .iter()
                .find(|f| f.name == "__compat_es5_heap_collect")
                .expect("actual heap collector")
                .index;
            let decode = ir
                .functions
                .iter()
                .find(|f| f.name == "__compat_es5_decode_int")
                .expect("actual slot decoder")
                .index;
            let read = ir
                .functions
                .iter()
                .find(|f| f.name == "__compat_ref_read")
                .expect("actual carrier read")
                .index;
            let carrier = ir.function_impls[&collect].param_slots[0];
            let body = &ir.function_impls[&collect].body_stmts;
            let mut roots = Vec::new();
            for stmt in body {
                if let vm::Stmt::While {
                    condition, body, ..
                } = stmt
                {
                    let frame = match condition {
                        vm::Expr::Not(inner) => match inner.as_ref() {
                            vm::Expr::Eq(fv, zero)
                                if matches!(fv.as_ref(), vm::Expr::Var(_))
                                    && matches!(zero.as_ref(), vm::Expr::Int(0)) =>
                            {
                                match fv.as_ref() {
                                    vm::Expr::Var(v) => *v,
                                    _ => continue,
                                }
                            }
                            _ => continue,
                        },
                        _ => continue,
                    };
                    let inner_at = body.iter().position(|s| {
                        matches!(
                            s,
                            vm::Stmt::While { condition, .. }
                                if matches!(condition, vm::Expr::Lt(_, _))
                        )
                    });
                    if inner_at.is_some() {
                        roots.push((frame, body));
                    }
                }
            }
            let loops = roots.len();
            let Some((frame, loop_body)) = roots.pop() else {
                *self.0.lock().unwrap() = Summary {
                    loops,
                    ..Default::default()
                };
                return Ok(ir);
            };
            let ctx = Ctx {
                frame,
                decode,
                read,
                carrier,
            };
            let producers = loop_body
                .iter()
                .map(|s| count_stmt_producers(s, &ctx))
                .sum();
            let inner_at = loop_body
                .iter()
                .position(|s| {
                    matches!(
                        s,
                        vm::Stmt::While { condition, .. }
                            if matches!(condition, vm::Expr::Lt(_, _))
                    )
                })
                .expect("inner slot-marking loop");
            let start = inner_at + 1;
            let mut binder = None;
            let mut binder_count = 0;
            let mut guard = None;
            let mut guard_count = 0;
            let mut advance = None;
            let mut advance_count = 0;
            for (pos, stmt) in loop_body.iter().enumerate().skip(start) {
                match stmt {
                    vm::Stmt::Assign { index, expr, .. } if is_previous_producer(expr, &ctx) => {
                        binder_count += 1;
                        binder = Some((pos, *index));
                    }
                    vm::Stmt::Assign { index, .. } if *index == frame => {
                        advance_count += 1;
                        advance = Some(pos);
                    }
                    vm::Stmt::Expr { .. } => {
                        guard_count += 1;
                        guard = Some(pos);
                    }
                    _ => {}
                }
            }
            let (binder_pos, bound) = binder.expect("single producer binding");
            let guard_pos = guard.expect("single bottom-boundary guard");
            let advance_pos = advance.expect("single frame advance");
            let bound_int_declared = ir.function_impls[&collect].body_stmts.iter().any(|s| {
                matches!(
                    s,
                    vm::Stmt::Let {
                        index,
                        declared_schema,
                        ..
                    } if *index == bound && matches!(declared_schema, Some(vm::TypeSchema::Int))
                )
            });
            assert_eq!(
                binder_count, 1,
                "exactly one previous-link binder, got {binder_count}"
            );
            assert_eq!(
                guard_count, 1,
                "exactly one bottom-boundary guard, got {guard_count}"
            );
            assert_eq!(
                advance_count, 1,
                "exactly one frame advance, got {advance_count}"
            );
            match &loop_body[guard_pos] {
                vm::Stmt::Expr { expr, .. } => {
                    assert!(
                        expr_uses(expr, bound),
                        "the guard must reuse the bound link"
                    );
                    assert_eq!(
                        count_producers(expr, &ctx),
                        0,
                        "the guard must not decode a second time"
                    );
                }
                other => panic!("the guard must stay a bare assertion expression: {other:?}"),
            }
            match &loop_body[advance_pos] {
                vm::Stmt::Assign { index, expr, .. } => {
                    assert_eq!(*index, frame, "the advance must target the roots frame");
                    assert!(
                        matches!(expr, vm::Expr::Var(v) if *v == bound),
                        "the advance must reuse the bound link"
                    );
                }
                other => panic!("the advance must stay a frame assignment: {other:?}"),
            }
            for (pos, stmt) in loop_body.iter().enumerate() {
                if pos > binder_pos && pos < advance_pos {
                    assert!(
                        !stmt_writes(stmt, bound),
                        "no rebinding of the link between binder and advance at {pos}"
                    );
                    assert!(
                        !stmt_writes(stmt, frame),
                        "no frame rewrite between binder and advance at {pos}"
                    );
                }
            }
            assert_eq!(
                loop_body.iter().filter(|s| stmt_writes(s, bound)).count(),
                1,
                "the bound link has a single writer in the loop body"
            );
            assert_eq!(
                loop_body.iter().filter(|s| stmt_writes(s, frame)).count(),
                1,
                "the roots frame has a single writer in the loop body"
            );
            *self.0.lock().unwrap() = Summary {
                loops,
                producers,
                inner_index: Some(inner_at),
                binder: Some((binder_pos, bound)),
                guard: Some(guard_pos),
                advance: Some(advance_pos),
                bound_int_declared,
            };
            Ok(ir)
        }
    }
    let tap = Box::leak(Box::new(Tap(Default::default())));
    let source = "var p={x:1};var o=Object.create(p);var a=o.x===1;var b=\"x\" in o;var c=Object.getOwnPropertyDescriptor(o,\"x\")===undefined;a&&b&&c;";
    let _compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        javascript_es5_flavor(),
        vm::CompileSourceFileOptions::new()
            .with_source_plugin(tap)
            .with_host_api_catalog(
                pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog(),
            ),
    )
    .unwrap();
    let summary = tap.0.lock().unwrap();
    assert_eq!(
        summary.loops, 1,
        "exactly one structural ROOTS loop: {summary:?}"
    );
    assert_eq!(
        summary.producers, 1,
        "exactly one decoded previous-link producer per visited frame: {summary:?}"
    );
    let inner_at = summary.inner_index.expect("inner slot-marking loop");
    let (binder_pos, bound) = summary.binder.expect("single producer binding");
    let guard_pos = summary.guard.expect("single bottom-boundary guard");
    let advance_pos = summary.advance.expect("single frame advance");
    assert!(
        inner_at < binder_pos && binder_pos < guard_pos && guard_pos < advance_pos,
        "inner < binder < guard < advance: {summary:?}"
    );
    assert!(
        summary.bound_int_declared,
        "the reused link slot is declared Int in the collector preamble: {summary:?}"
    );
    eprintln!(
        "P7_ROOTS_PREV_LINK loops=1 producers=1 binder_slot={bound} binder_pos={binder_pos} guard_pos={guard_pos} advance_pos={advance_pos} inner={inner_at}"
    );
}

/// Structural contract for the ROOTS exact-zero category slice: the single
/// original category-cell read must be bound once into the existing Number
/// scratch `rdn`, and the category value must be the exact Float +0
/// conditional `IfElse(Eq(rdn, 0.0), Int(0), decode_int(rdn))`. Every
/// non-empty branch statement (slot, generation, expectation, marker call)
/// and the entry cursor stay unchanged after that value. IR capture only,
/// no VM execution: the previous unconditional inline decode fails this
/// contract.
#[test]
fn p7_roots_exact_zero_category_program_structure() {
    struct Tap;
    impl vm::SourcePlugin for Tap {
        fn descriptor(&self) -> vm::SourceFrontendDescriptor {
            vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN)
        }
        fn parse_source(
            &self,
            source: &str,
            context: &vm::SourceFrontendContext<'_>,
        ) -> Result<vm::FrontendIr, vm::ParseError> {
            use vm::{Expr as E, Stmt as S};
            fn plain_call(expr: &E, target: u16, arity: usize) -> Option<&[E]> {
                match expr {
                    E::Call(call, _, args, _, _) if *call == target && args.len() == arity => {
                        Some(args)
                    }
                    _ => None,
                }
            }
            fn number_inner(expr: &E) -> Option<&E> {
                match expr {
                    E::Div(inner, unit) if matches!(unit.as_ref(), E::Float(f) if *f == 1.0) => {
                        Some(inner)
                    }
                    _ => None,
                }
            }
            fn decode_arg(expr: &E, decode: u16) -> Option<&E> {
                plain_call(expr, decode, 1).map(|args| &args[0])
            }
            fn is_decode_of(expr: &E, decode: u16, slot: u16) -> bool {
                matches!(decode_arg(expr, decode), Some(E::Var(v)) if *v == slot)
            }
            /// The certified frontend normalizes a Number operand through its
            /// pre-existing divide-by-one shape; both spellings name the same
            /// already-read scratch cell.
            fn is_scratch(expr: &E, slot: u16) -> bool {
                match expr {
                    E::Var(v) => *v == slot,
                    E::Div(inner, unit) => {
                        matches!(inner.as_ref(), E::Var(v) if *v == slot)
                            && matches!(unit.as_ref(), E::Float(f) if *f == 1.0)
                    }
                    _ => false,
                }
            }
            fn is_root_top_read(expr: &E, read: u16, carrier: u16) -> bool {
                let Some(args) = plain_call(expr, read, 2) else {
                    return false;
                };
                matches!(&args[0], E::Var(c) if *c == carrier)
                    && matches!(number_inner(&args[1]), Some(E::Int(3)))
            }
            fn is_next_slot_read(expr: &E, read: u16, carrier: u16) -> bool {
                let Some(args) = plain_call(expr, read, 2) else {
                    return false;
                };
                matches!(&args[0], E::Var(c) if *c == carrier)
                    && matches!(number_inner(&args[1]), Some(E::Int(4)))
            }
            fn entry_read_frame(
                expr: &E,
                read: u16,
                carrier: u16,
                offset: i64,
            ) -> Option<(u16, u16)> {
                let args = plain_call(expr, read, 2)?;
                if !matches!(&args[0], E::Var(c) if *c == carrier) {
                    return None;
                }
                let inner = number_inner(&args[1])?;
                let E::Add(base, tail) = inner else {
                    return None;
                };
                let E::Add(frame, fixed) = base.as_ref() else {
                    return None;
                };
                if !matches!(fixed.as_ref(), E::Int(4)) {
                    return None;
                }
                let step = if offset == 0 {
                    tail.as_ref()
                } else {
                    match tail.as_ref() {
                        E::Add(step, cell) if matches!(cell.as_ref(), E::Int(c) if *c == offset) => {
                            step.as_ref()
                        }
                        _ => return None,
                    }
                };
                let E::Mul(index, width) = step else {
                    return None;
                };
                if !matches!(width.as_ref(), E::Int(3)) {
                    return None;
                }
                match (frame.as_ref(), index.as_ref()) {
                    (E::Var(f), E::Var(e)) => Some((*f, *e)),
                    _ => None,
                }
            }
            let ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
            let helper = |name: &str| {
                ir.functions
                    .iter()
                    .find(|f| f.name == name)
                    .unwrap_or_else(|| panic!("missing actual helper {name}"))
                    .index
            };
            let collect = helper("__compat_es5_heap_collect");
            let decode = helper("__compat_es5_decode_int");
            let decode_u32 = helper("__compat_es5_decode_u32");
            let read = helper("__compat_ref_read");
            let mark = helper("__compat_es5_mark");
            let carrier = ir.function_impls[&collect].param_slots[0];
            let body = &ir.function_impls[&collect].body_stmts;
            let mut lists: Vec<&[S]> = Vec::new();
            fn collect_lists<'a>(body: &'a [S], out: &mut Vec<&'a [S]>) {
                out.push(body);
                for stmt in body {
                    match stmt {
                        S::While { body, .. } => collect_lists(body, out),
                        S::IfElse {
                            then_branch,
                            else_branch,
                            ..
                        } => {
                            collect_lists(then_branch, out);
                            collect_lists(else_branch, out);
                        }
                        S::For { body, .. } => collect_lists(body, out),
                        _ => {}
                    }
                }
            }
            collect_lists(body, &mut lists);
            let mut scratch = None;
            for list in &lists {
                for pair in list.windows(2) {
                    let S::Assign { index, expr, .. } = &pair[0] else {
                        continue;
                    };
                    if !is_root_top_read(expr, read, carrier) {
                        continue;
                    }
                    let S::Assign {
                        index: frame_slot,
                        expr: frame_expr,
                        ..
                    } = &pair[1]
                    else {
                        continue;
                    };
                    if is_decode_of(frame_expr, decode, *index) {
                        scratch = Some((*index, *frame_slot));
                    }
                }
            }
            let (rdn, frame) = scratch.unwrap_or_else(|| {
                panic!(
                    "the ROOTS top read binds the existing Number scratch before the frame walk: \
                     lists={}",
                    lists.len()
                )
            });
            let mut category: Option<(&[S], usize, u16, bool)> = None;
            let mut category_reads = 0usize;
            let mut entry = None;
            for list in &lists {
                for (pos, stmt) in list.iter().enumerate() {
                    let S::Assign { index, expr, .. } = stmt else {
                        continue;
                    };
                    let direct = entry_read_frame(expr, read, carrier, 0);
                    let wrapped = decode_arg(expr, decode)
                        .and_then(|arg| entry_read_frame(arg, read, carrier, 0));
                    if let Some((read_frame, read_entry)) = direct.or(wrapped) {
                        category_reads += 1;
                        assert_eq!(
                            read_frame, frame,
                            "the category cell belongs to the ROOTS frame"
                        );
                        entry = Some(read_entry);
                        category = Some((*list, pos, *index, direct.is_some()));
                    }
                }
            }
            assert_eq!(
                category_reads, 1,
                "exactly one category-cell read in the collector, got {category_reads}"
            );
            let entry = entry.expect("the category read names its ROOTS entry");
            assert_ne!(
                entry, frame,
                "the category entry is not the frame cursor itself"
            );
            // The entry binds header cell 4 once as Number, then decodes that
            // same local once as Int before phase 1. The marker carries this
            // decoded Int, not a reconstructed inline read.
            let first_phase = body
                .iter()
                .position(|stmt| matches!(stmt, S::While { .. }))
                .expect("phase 1 starts with the directory scan loop");
            let mut next_reads = Vec::new();
            for (pos, stmt) in body.iter().take(first_phase).enumerate() {
                if let S::Let {
                    index,
                    declared_schema: Some(vm::TypeSchema::Number),
                    expr,
                    ..
                } = stmt
                    && is_next_slot_read(expr, read, carrier)
                {
                    next_reads.push((pos, *index));
                }
            }
            assert_eq!(next_reads.len(), 1, "one entry HDR_NEXT_SLOT Number read");
            let (next_read_at, next_number) = next_reads[0];
            let mut next_decodes = Vec::new();
            for (pos, stmt) in body.iter().take(first_phase).enumerate() {
                if let S::Let {
                    index,
                    declared_schema: Some(vm::TypeSchema::Int),
                    expr,
                    ..
                } = stmt
                    && decode_arg(expr, decode).is_some_and(|arg| is_scratch(arg, next_number))
                {
                    next_decodes.push((pos, *index));
                }
            }
            assert_eq!(next_decodes.len(), 1, "one decode of that entry Number local");
            let (next_decode_at, next_slot) = next_decodes[0];
            assert!(next_read_at < next_decode_at && next_decode_at < first_phase);
            let (_, category_at, category_slot, direct_read) =
                category.expect("the collector reads its category cell");
            assert!(
                direct_read && category_slot == rdn,
                "the category read must be bound once into the existing Number scratch {rdn} \
                 (got slot {category_slot}, direct read={direct_read}) instead of the old \
                 unconditional inline decode"
            );
            // Locate the category conditional by its exact scratch/+0 shape,
            // independently of any surrounding statement ordering.
            let mut conditional = None;
            let mut condition_count = 0usize;
            let mut seen_conditions: Vec<String> = Vec::new();
            for list in &lists {
                for stmt in list.iter() {
                    let S::Assign {
                        index,
                        expr:
                            E::IfElse {
                                condition,
                                then_expr,
                                else_expr,
                            },
                        ..
                    } = stmt
                    else {
                        continue;
                    };
                    let E::Eq(lhs, rhs) = condition.as_ref() else {
                        continue;
                    };
                    if !is_scratch(lhs.as_ref(), rdn)
                        || !matches!(rhs.as_ref(), E::Float(f) if *f == 0.0 && !f.is_sign_negative())
                    {
                        if seen_conditions.len() < 40 {
                            seen_conditions.push(format!("{lhs:?} == {rhs:?}"));
                        }
                        continue;
                    }
                    condition_count += 1;
                    conditional = Some((*index, then_expr.as_ref(), else_expr.as_ref()));
                }
            }
            assert_eq!(
                condition_count, 1,
                "exactly one assignment compares the existing Number scratch {rdn} with exact \
                 Float +0.0, got {condition_count}; other conditions={seen_conditions:?}"
            );
            let (cat_slot, then_expr, else_expr) =
                conditional.expect("the exact-zero category conditional");
            assert!(
                matches!(then_expr, E::Int(0)),
                "the exact-zero branch yields Int(0), got {then_expr:?}"
            );
            assert!(
                decode_arg(else_expr, decode).is_some_and(|arg| is_scratch(arg, rdn)),
                "the non-zero fallback keeps the original decoder on the same read, got \
                 {else_expr:?}"
            );
            let mut nonempty = None;
            for list in &lists {
                for (pos, stmt) in list.iter().enumerate() {
                    let S::IfElse {
                        condition,
                        then_branch,
                        else_branch,
                        ..
                    } = stmt
                    else {
                        continue;
                    };
                    let E::Not(inner) = condition else {
                        continue;
                    };
                    let E::Eq(lhs, rhs) = inner.as_ref() else {
                        continue;
                    };
                    if matches!(lhs.as_ref(), E::Var(v) if *v == cat_slot)
                        && matches!(rhs.as_ref(), E::Int(0))
                    {
                        nonempty =
                            Some((*list, pos, then_branch.as_slice(), else_branch.as_slice()));
                    }
                }
            }
            let (nonempty_list, nonempty_at, then_branch, else_branch) =
                nonempty.expect("the non-empty category branch stays guarded by `cat != 0`");
            assert!(
                else_branch.is_empty(),
                "empty categories remain a bare skip, got {} else statement(s)",
                else_branch.len()
            );
            assert_eq!(
                then_branch.len(),
                4,
                "non-empty categories keep slot, generation, expectation and marker call"
            );
            let (eslot, eslot_expr) = match &then_branch[0] {
                S::Assign { index, expr, .. } => (*index, expr),
                other => panic!("non-empty branch slot statement changed: {other:?}"),
            };
            let (egen, egen_expr) = match &then_branch[1] {
                S::Assign { index, expr, .. } => (*index, expr),
                other => panic!("non-empty branch generation statement changed: {other:?}"),
            };
            let (expect, expect_expr) = match &then_branch[2] {
                S::Assign { index, expr, .. } => (*index, expr),
                other => panic!("non-empty branch expectation statement changed: {other:?}"),
            };
            let sink_expr = match &then_branch[3] {
                S::Assign { expr, .. } => expr,
                other => panic!("non-empty branch marker statement changed: {other:?}"),
            };
            assert_eq!(
                decode_arg(eslot_expr, decode)
                    .and_then(|arg| entry_read_frame(arg, read, carrier, 1)),
                Some((frame, entry)),
                "the non-empty branch still decodes the root slot cell"
            );
            assert_eq!(
                decode_arg(egen_expr, decode_u32)
                    .and_then(|arg| entry_read_frame(arg, read, carrier, 2)),
                Some((frame, entry)),
                "the non-empty branch still decodes the root generation cell"
            );
            let expected_expect = matches!(
                expect_expr,
                E::IfElse { condition, then_expr, else_expr }
                    if matches!(condition.as_ref(), E::Eq(l, r)
                        if matches!(l.as_ref(), E::Var(v) if *v == cat_slot)
                            && matches!(r.as_ref(), E::Int(4)))
                        && matches!(then_expr.as_ref(), E::Int(2))
                        && matches!(else_expr.as_ref(), E::IfElse { condition, then_expr, else_expr }
                            if matches!(condition.as_ref(), E::Eq(l, r)
                                if matches!(l.as_ref(), E::Var(v) if *v == cat_slot)
                                    && matches!(r.as_ref(), E::Int(5)))
                                && matches!(then_expr.as_ref(), E::Int(3))
                                && matches!(else_expr.as_ref(), E::Int(5)))
            );
            assert!(
                expected_expect,
                "the non-empty branch still maps string/object categories to marker expectations"
            );
            let marked = number_inner(sink_expr)
                .and_then(|inner| plain_call(inner, mark, 5))
                .is_some_and(|args| {
                    matches!(&args[0], E::Var(c) if *c == carrier)
                        && matches!(&args[1], E::Var(v) if *v == eslot)
                        && matches!(&args[2], E::Var(v) if *v == egen)
                        && matches!(&args[3], E::Var(v) if *v == expect)
                        && matches!(&args[4], E::Var(v) if *v == next_slot)
                });
            assert!(
                marked,
                "the non-empty branch still calls the real marker with carrier, slot, generation, \
                 expectation and the collector's decoded HDR_NEXT_SLOT"
            );
            assert!(
                matches!(
                    nonempty_list.get(nonempty_at + 1),
                    Some(S::Assign { index, expr: E::Add(lhs, step), .. })
                        if *index == entry
                            && matches!(lhs.as_ref(), E::Var(v) if *v == entry)
                            && matches!(step.as_ref(), E::Int(1))
                ),
                "the entry cursor still increments once after the category branch"
            );
            eprintln!(
                "P7_ROOTS_EXACT_ZERO_STRUCTURE frame={frame} entry={entry} scratch={rdn} \
                 cat={cat_slot} category_at={category_at} nonempty_at={nonempty_at}"
            );
            Ok(ir)
        }
    }
    let plugin: &'static dyn vm::SourcePlugin = Box::leak(Box::new(Tap));
    let (_machine, _) = build_machine("var o={v:1};o.v===1;", MachineSource::Tapped(plugin), false);
}

/// Test-only logical helper contracts captured from the actual IR: the
/// declared name, logical flat-function index, arity and parameter count of
/// every helper the refusal tests resolve to a runtime prototype.
#[derive(Debug, Clone, Copy)]
struct PrevLinkHelper {
    name: &'static str,
    index: u16,
    arity: u8,
    params: usize,
}

/// Fault selector for the shared post-source root seam. Exactly one numeric
/// cell is corrupted (or none for the clean control) after the original
/// fixture completes and its root is published; a real collection follows
/// immediately while the script frame stays active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootSeamFault {
    Clean,
    PrevLink,
    BottomCap,
    BottomExtent,
    BottomIdentity,
    StaleGen,
    CategoryFloatPosZero,
    CategoryFloatNegZero,
    CategoryRawIntZero,
    CategoryFractional,
    CategoryNegative,
    CategoryNaN,
    CategoryPosInf,
    CategoryNegInf,
    /// Phase-1 row-state controls: the sole write targets the permanent entry
    /// 0 directory row's own state cell.
    StateActiveFloat,
    StateActiveRawInt,
    StateFractional,
    StateNegative,
    StateNaN,
    StatePosInf,
    StateNegInf,
    StateDomainHigh,
    /// Direct-marker state controls: the sole write targets the validated
    /// permanent entry 0 row state cell, then the real mark helper runs
    /// instead of a collection.
    MarkerStateActiveFloat,
    MarkerStateActiveRawInt,
    MarkerStateFractional,
    MarkerStateNegative,
    MarkerStateNaN,
    MarkerStatePosInf,
    MarkerStateNegInf,
    MarkerStateInactive,
}

/// Shared post-source injection seam for every collector-root control.
///
/// The original fixture (`var o={v:1};o.v===1;`) compiles unchanged; this
/// plugin appends validation plus at most one fault write and one immediate
/// collection to the ORIGINAL `ir.stmts` tail, before the Core root return,
/// while the script frame stays active. Nothing is prepended to the
/// intrinsic-root prefix: the old prefix seam injected collection while a
/// node was still pending and proved nothing about the collector.
///
/// Fail-closed discovery (compilation fails instead of injecting blindly):
/// unique source binding `o`, the actual `__compat_ref_make` carrier, the
/// script `root_enter` after bootstrap, the initialized `o` assignment
/// followed by `root_write(carrier, frame, entry, Var(o))` in the same
/// guarded store, terminal completion with no premature `root_leave`.
struct RootSeam {
    fault: RootSeamFault,
    helpers: std::sync::Mutex<Vec<PrevLinkHelper>>,
    sorted_impl_keys: std::sync::Mutex<Vec<u16>>,
    carrier: std::sync::Mutex<Option<u16>>,
    frame: std::sync::Mutex<Option<u16>>,
    entry: std::sync::Mutex<Option<u16>>,
    object: std::sync::Mutex<Option<u16>>,
}
impl RootSeam {
    fn capture(ir: &vm::FrontendIr) -> Vec<PrevLinkHelper> {
        [
            "__compat_ref_read",
            "__compat_ref_write",
            "__compat_es5_intrinsic_root",
            "__compat_es5_heap_collect",
            "__compat_es5_decode_int",
            "__compat_es5_decode_u32",
            "__compat_es5_mark",
        ]
        .iter()
        .map(|name| {
            let found = ir
                .functions
                .iter()
                .find(|f| f.name == *name)
                .expect("real helper");
            let params = ir
                .function_impls
                .get(&found.index)
                .map_or_else(|| found.args.len(), |body| body.param_slots.len());
            PrevLinkHelper {
                name,
                index: found.index,
                arity: found.arity,
                params,
            }
        })
        .collect()
    }
    fn contract(helpers: &[PrevLinkHelper], name: &str) -> (u8, usize) {
        helpers.iter().find(|h| h.name == name).map_or_else(
            || panic!("missing logical decl {name}"),
            |h| (h.arity, h.params),
        )
    }
}
/// True when any expression in the scanned post-source statements calls the
/// given helper. Full recursion with a fail-closed panic arm: an unscanned
/// shape must stop the seam, never silently pass it.
fn seam_expr_has_call(expr: &vm::Expr, target: u16) -> bool {
    match expr {
        vm::Expr::Call(t, _, args, _, _) | vm::Expr::LocalCall(t, _, args, _) => {
            *t == target || args.iter().any(|a| seam_expr_has_call(a, target))
        }
        vm::Expr::ModuleCall(_, _, args, _) => args.iter().any(|a| seam_expr_has_call(a, target)),
        vm::Expr::ClosureCall(closure, args) => {
            seam_expr_has_call(&closure.body, target)
                || args.iter().any(|a| seam_expr_has_call(a, target))
        }
        vm::Expr::Closure(closure) => seam_expr_has_call(&closure.body, target),
        vm::Expr::Add(l, r)
        | vm::Expr::Sub(l, r)
        | vm::Expr::Mul(l, r)
        | vm::Expr::Div(l, r)
        | vm::Expr::Mod(l, r)
        | vm::Expr::Eq(l, r)
        | vm::Expr::Lt(l, r)
        | vm::Expr::Gt(l, r)
        | vm::Expr::And(l, r)
        | vm::Expr::Or(l, r) => seam_expr_has_call(l, target) || seam_expr_has_call(r, target),
        vm::Expr::Neg(e)
        | vm::Expr::Not(e)
        | vm::Expr::ToOwned(e)
        | vm::Expr::Borrow(e)
        | vm::Expr::BorrowMut(e) => seam_expr_has_call(e, target),
        vm::Expr::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            seam_expr_has_call(condition, target)
                || seam_expr_has_call(then_expr, target)
                || seam_expr_has_call(else_expr, target)
        }
        vm::Expr::Match {
            value,
            arms,
            default,
            ..
        } => {
            seam_expr_has_call(value, target)
                || arms.iter().any(|(_, e)| seam_expr_has_call(e, target))
                || seam_expr_has_call(default, target)
        }
        vm::Expr::Block { stmts, expr } => {
            seam_list_has_call(stmts, target) || seam_expr_has_call(expr, target)
        }
        vm::Expr::ResultConstruct { payload, .. } => seam_expr_has_call(payload, target),
        vm::Expr::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            seam_expr_has_call(value, target)
                || seam_expr_has_call(ok_expr, target)
                || seam_expr_has_call(err_expr, target)
        }
        vm::Expr::ResultQuery { value, .. } | vm::Expr::ResultTry { value, .. } => {
            seam_expr_has_call(value, target)
        }
        vm::Expr::OptionalGet { container, key, .. } => {
            seam_expr_has_call(container, target) || seam_expr_has_call(key, target)
        }
        vm::Expr::OptionUnwrapOr {
            value, fallback, ..
        } => seam_expr_has_call(value, target) || seam_expr_has_call(fallback, target),
        vm::Expr::Null
        | vm::Expr::Int(_)
        | vm::Expr::Float(_)
        | vm::Expr::Bool(_)
        | vm::Expr::String(_)
        | vm::Expr::Bytes(_)
        | vm::Expr::FunctionRef(_, _)
        | vm::Expr::ModuleFunctionRef(_, _)
        | vm::Expr::Var(_)
        | vm::Expr::MoveVar(_)
        | vm::Expr::MoveField { .. }
        | vm::Expr::MoveIndex { .. } => false,
        _ => panic!("unexpected expression on the post-source seam path: {expr:?}"),
    }
}

/// True when any statement in one statement list calls the given helper.
/// Loops are rejected: the unchanged fixture tail is straight-line code, and
/// a loop would invalidate the positional seam argument below.
fn seam_list_has_call(list: &[vm::Stmt], target: u16) -> bool {
    list.iter().any(|stmt| match stmt {
        vm::Stmt::Expr { expr, .. }
        | vm::Stmt::Let { expr, .. }
        | vm::Stmt::Assign { expr, .. } => seam_expr_has_call(expr, target),
        vm::Stmt::IfElse {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            seam_expr_has_call(condition, target)
                || seam_list_has_call(then_branch, target)
                || seam_list_has_call(else_branch, target)
        }
        vm::Stmt::While { .. } | vm::Stmt::For { .. } => {
            panic!("unexpected loop on the post-source seam path")
        }
        vm::Stmt::Noop { .. }
        | vm::Stmt::Break { .. }
        | vm::Stmt::Continue { .. }
        | vm::Stmt::Drop { .. }
        | vm::Stmt::FuncDecl { .. } => false,
        _ => panic!("unexpected statement on the post-source seam path: {stmt:?}"),
    })
}

impl vm::SourcePlugin for RootSeam {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN)
    }
    fn parse_source(
        &self,
        source: &str,
        context: &vm::SourceFrontendContext<'_>,
    ) -> Result<vm::FrontendIr, vm::ParseError> {
        use vm::{Expr as E, Stmt as S, TypeSchema as T};
        fn call(i: u16, a: Vec<E>) -> E {
            E::Call(i, vec![], a, None, None)
        }
        fn number(e: E) -> E {
            E::Div(Box::new(e), Box::new(E::Float(1.0)))
        }
        fn read_cell(read: u16, carrier: u16, addr: E) -> E {
            call(read, vec![E::Var(carrier), number(addr)])
        }
        fn decoded(decode: u16, x: E) -> E {
            call(decode, vec![x])
        }
        fn decoded_gen(decode_u32: u16, x: E) -> E {
            call(decode_u32, vec![x])
        }
        fn boxed_field(object: u16, name: &str) -> E {
            E::Call(
                vm::BuiltinFunction::Get.call_index(),
                vec![],
                vec![E::Borrow(Box::new(E::Var(object))), E::String(name.into())],
                None,
                None,
            )
        }
        fn assert_stmt(cond: E) -> S {
            S::Expr {
                expr: call(vm::BuiltinFunction::Assert.call_index(), vec![cond]),
                line: 1,
            }
        }
        fn assert_eq_expr(left: E, right: E) -> S {
            assert_stmt(E::Eq(Box::new(left), Box::new(right)))
        }
        let mut ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
        *self.helpers.lock().unwrap() = Self::capture(&ir);
        let mut sorted_impl_keys: Vec<u16> = ir.function_impls.keys().copied().collect();
        sorted_impl_keys.sort_unstable();
        *self.sorted_impl_keys.lock().unwrap() = sorted_impl_keys;
        let helper = |name: &str| {
            ir.functions
                .iter()
                .find(|f| f.name == name)
                .expect("real helper")
                .index
        };
        let make = helper("__compat_ref_make");
        let read = helper("__compat_ref_read");
        let write = helper("__compat_ref_write");
        let root_enter = helper("__compat_es5_root_enter");
        let root_write = helper("__compat_es5_root_write");
        let root_clear = helper("__compat_es5_root_clear");
        let root_leave = helper("__compat_es5_root_leave");
        let decode = helper("__compat_es5_decode_int");
        let decode_u32 = helper("__compat_es5_decode_u32");
        let collect = helper("__compat_es5_heap_collect");
        let mark = helper("__compat_es5_mark");
        // Unique source binding `o`: the published fixture object.
        let object = {
            let found: Vec<u16> = ir
                .local_bindings
                .iter()
                .filter(|(name, _)| name == "o")
                .map(|(_, slot)| *slot)
                .collect();
            assert_eq!(found.len(), 1, "unique source binding o, got {found:?}");
            found[0]
        };
        // Actual carrier construction: the single top-level realm make call.
        let (carrier_pos, carrier) = {
            let mut found = Vec::new();
            for (pos, stmt) in ir.stmts.iter().enumerate() {
                if let S::Let {
                    index,
                    expr: E::Call(target, _, _, _, _),
                    ..
                } = stmt
                    && *target == make
                {
                    found.push((pos, *index));
                }
            }
            assert_eq!(found.len(), 1, "actual carrier construction, got {found:?}");
            found[0]
        };
        // Script root_enter after bootstrap, on the same carrier. The literal
        // argument is the actual entry capacity this frame reserves and zeroes.
        let (enter_pos, frame, capacity) = {
            let mut found = Vec::new();
            for (pos, stmt) in ir.stmts.iter().enumerate() {
                if let S::Let {
                    index,
                    expr: E::Call(target, _, args, _, _),
                    ..
                } = stmt
                    && *target == root_enter
                    && let [E::Var(c), E::Int(capacity)] = &args[..]
                    && *c == carrier
                {
                    found.push((pos, *index, *capacity));
                }
            }
            assert_eq!(
                found.len(),
                1,
                "script root_enter after bootstrap, got {found:?}"
            );
            let (pos, frame, capacity) = found[0];
            assert!(pos > carrier_pos, "root_enter after the carrier bootstrap");
            (pos, frame, capacity)
        };
        // Initialized `o` assignment followed by its root_write in the same
        // guarded store: top-level adjacency or adjacency inside one IfElse
        // branch whose guard position also follows the frame entry.
        let entry = {
            let mut lists: Vec<(bool, usize, &[S])> = vec![(true, 0, &ir.stmts)];
            for (pos, stmt) in ir.stmts.iter().enumerate() {
                if let S::IfElse {
                    then_branch,
                    else_branch,
                    ..
                } = stmt
                {
                    lists.push((false, pos, then_branch));
                    lists.push((false, pos, else_branch));
                }
            }
            let mut found = Vec::new();
            for (is_top, base, list) in &lists {
                for (off, window) in list.windows(2).enumerate() {
                    let assign_ok = matches!(
                        &window[0],
                        S::Assign { index, expr: E::Var(dest), .. }
                        if *index == object && *dest != object
                    );
                    if !assign_ok {
                        continue;
                    }
                    if let S::Let {
                        expr: E::Call(target, _, args, _, _),
                        ..
                    } = &window[1]
                        && let [E::Var(c), E::Var(f), E::Int(entry), E::Var(o)] = &args[..]
                        && *target == root_write
                        && *c == carrier
                        && *f == frame
                        && *o == object
                    {
                        let at = if *is_top { off } else { *base };
                        found.push((at, u16::try_from(*entry).expect("seam root entry index")));
                    }
                }
            }
            assert_eq!(
                found.len(),
                1,
                "initialized o assignment plus its root_write in one guarded store, got {found:?}"
            );
            let (at, entry) = found[0];
            assert!(at > enter_pos, "o publication after script frame entry");
            entry
        };
        // Terminal completion. The source program's last statement is its
        // normal-completion value publication is followed by temporary-root
        // clears and the canonical uncaught-exception exit. Undefined factoring
        // then appends its pure helper declaration. These exact current shapes
        // are proven against this IR: the pending Completion/payload channels,
        // the real `typescript::throw_error` entry and the payload root entry
        // the canonical refresh names inside the script frame's reservation.
        let shape = |stmt: &S| match stmt {
            S::Let { .. } => "Let",
            S::Expr { .. } => "Expr",
            S::IfElse { .. } => "IfElse",
            _ => "other",
        };
        let throw_error = helper("typescript::throw_error");
        assert!(
            ir.stmts.len() >= 3,
            "script tail keeps the source prefix, got {} statement(s)",
            ir.stmts.len()
        );
        let undefined = helper("__compat_es5_undefined");
        assert!(
            matches!(
                ir.stmts.last(),
                Some(S::FuncDecl {
                    name,
                    index,
                    arity: 0,
                    args,
                    exported: false,
                    has_impl: true,
                    ..
                }) if name == "__compat_es5_undefined" && *index == undefined && args.is_empty()
            ),
            "only the factored undefined declaration follows the canonical exit: {:?}",
            ir.stmts.last()
        );
        let guard_at = ir.stmts.len() - 2;
        let S::IfElse {
            condition,
            then_branch,
            else_branch,
            ..
        } = &ir.stmts[guard_at]
        else {
            panic!(
                "canonical uncaught exit must close the script at {guard_at}, got {}",
                shape(&ir.stmts[guard_at])
            );
        };
        assert!(
            else_branch.is_empty(),
            "canonical uncaught exit has no else branch at {guard_at}, got {} statement(s)",
            else_branch.len()
        );
        let E::Eq(kind_field, throw_kind) = condition else {
            panic!("canonical uncaught exit must compare the Completion kind at {guard_at}");
        };
        let E::Call(get, _, kind_args, _, _) = kind_field.as_ref() else {
            panic!("canonical uncaught exit must read a Completion field at {guard_at}");
        };
        assert_eq!(
            *get,
            vm::BuiltinFunction::Get.call_index(),
            "canonical uncaught exit reads the kind field"
        );
        let [E::Var(completion), E::String(kind_name)] = &kind_args[..] else {
            panic!(
                "canonical uncaught exit must read <completion>.kind at {guard_at}, got {kind_args:?}"
            );
        };
        assert_eq!(
            kind_name.as_str(),
            "kind",
            "canonical uncaught exit compares the Completion kind"
        );
        let E::Int(throw_code) = throw_kind.as_ref() else {
            panic!(
                "canonical uncaught exit must compare an Int kind at {guard_at}, got {throw_kind:?}"
            );
        };
        assert_eq!(
            *throw_code, 2,
            "canonical uncaught exit compares COMPLETION_THROW"
        );
        assert_eq!(
            then_branch.len(),
            2,
            "canonical uncaught exit holds the host throw and its payload root refresh"
        );
        let (host, host_args) = match &then_branch[0] {
            S::Let {
                declared_schema: Some(T::Null),
                expr: E::Call(target, _, args, _, _),
                ..
            } => (target, args),
            other => panic!(
                "canonical uncaught exit binds a Null host throw, got {}",
                shape(other)
            ),
        };
        assert_eq!(
            *host, throw_error,
            "canonical uncaught exit calls typescript::throw_error"
        );
        let [E::String(message)] = &host_args[..] else {
            panic!("canonical uncaught exit passes one literal message, got {host_args:?}");
        };
        assert_eq!(
            message.as_str(),
            "uncaught JavaScript throw",
            "canonical uncaught exit reports the uncaught JavaScript throw"
        );
        let (refresh, refresh_args) = match &then_branch[1] {
            S::Let {
                declared_schema: Some(T::Int),
                expr: E::Call(target, _, args, _, _),
                ..
            } => (target, args),
            other => panic!(
                "canonical uncaught exit refreshes the payload root, got {}",
                shape(other)
            ),
        };
        assert_eq!(
            *refresh, root_write,
            "canonical uncaught exit refreshes through the script root_write"
        );
        let [
            E::Var(r_carrier),
            E::Var(r_frame),
            E::Int(r_entry),
            E::Var(payload),
        ] = &refresh_args[..]
        else {
            panic!("canonical uncaught exit rewrites one payload entry, got {refresh_args:?}");
        };
        assert_eq!(
            (*r_carrier, *r_frame),
            (carrier, frame),
            "canonical uncaught exit rewrites the seam payload in the script frame"
        );
        let declared = |slot: u16| {
            ir.stmts[..guard_at].iter().any(|stmt| {
                matches!(
                    stmt,
                    S::Let {
                        index,
                        declared_schema: Some(T::Named(_, _)),
                        ..
                    } if *index == slot
                )
            })
        };
        assert!(
            declared(*completion),
            "canonical uncaught exit compares a declared Completion channel"
        );
        assert!(
            declared(*payload),
            "canonical uncaught exit refreshes a declared payload channel"
        );
        // The current producer's reserve_root contract publishes the fresh
        // EMPTY payload through the script root_enter's zeroed reservation:
        // reserve_root only records the entry index and emits no IR, so no
        // obsolete initial payload root_write precedes the canonical uncaught
        // exit. The exit's refresh entry is instead bound by the actual
        // reserved capacity of the script frame, not by a removed write.
        let mut obsolete_payload_publications: Vec<(usize, i64)> = Vec::new();
        for (pos, stmt) in ir.stmts[..guard_at].iter().enumerate() {
            if let S::Let {
                expr: E::Call(target, _, args, _, _),
                ..
            } = stmt
                && *target == root_write
                && let [E::Var(c), E::Var(f), E::Int(publication_entry), E::Var(p)] = &args[..]
                && *c == carrier
                && *f == frame
                && *p == *payload
            {
                obsolete_payload_publications.push((pos, *publication_entry));
            }
        }
        assert!(
            obsolete_payload_publications.is_empty(),
            "the fresh EMPTY payload is published by root_enter zeroing, not an initial root_write: {obsolete_payload_publications:?}"
        );
        assert!(
            capacity > 0,
            "script root_enter reserves at least one payload entry, got capacity {capacity}"
        );
        assert!(
            *r_entry >= 0 && *r_entry < capacity,
            "canonical uncaught refresh targets a reserved script-frame entry: entry={} capacity={capacity}",
            *r_entry
        );
        eprintln!(
            "P7_ROOT_PAYLOAD_RESERVATION entry={} capacity={capacity} carrier={carrier} frame={frame} obsolete_publications={}",
            *r_entry,
            obsolete_payload_publications.len()
        );
        // Bind the final normal value, then admit only the original temporary
        // root clears between its publication and the uncaught exit.
        let value_at = ir.stmts[..guard_at]
            .iter()
            .rposition(|stmt| {
                matches!(
                    stmt,
                    S::Expr {
                        expr: E::Var(_),
                        ..
                    }
                )
            })
            .expect("the original terminal normal value is published");
        assert!(value_at > enter_pos, "normal value follows script entry");
        let (value_slot, source_at) = match (&ir.stmts[value_at], &ir.stmts[value_at - 1]) {
            (
                S::Expr {
                    expr: E::Var(value),
                    ..
                },
                S::Let {
                    index,
                    expr: E::Block { .. },
                    ..
                },
            ) if index == value => (*value, value_at - 1),
            (last, binding) => panic!(
                "script completion stays terminal at {guard_at}: expected a value publication \
                 bound immediately before it, got {} at {} then {} at {}",
                shape(binding),
                value_at - 1,
                shape(last),
                value_at
            ),
        };
        for (offset, stmt) in ir.stmts[value_at + 1..guard_at].iter().enumerate() {
            let S::Let {
                declared_schema: Some(T::Int),
                expr: E::Call(target, type_args, args, None, None),
                ..
            } = stmt
            else {
                panic!("only original temporary-root clears follow the value: {stmt:?}");
            };
            assert_eq!(*target, root_clear, "cleanup {offset} is root_clear");
            assert!(type_args.is_empty(), "root cleanup has no type arguments");
            let [E::Var(c), E::Var(f), E::Int(released)] = &args[..] else {
                panic!("cleanup {offset} uses the canonical three-argument clear: {args:?}");
            };
            assert_eq!((*c, *f), (carrier, frame), "cleanup owns the script frame");
            assert!(*released >= 0 && *released < capacity, "cleanup entry is reserved");
            assert_ne!(*released, i64::from(entry), "o stays rooted at the seam");
            assert_ne!(*released, *r_entry, "the pending payload stays rooted");
        }
        assert!(
            source_at > enter_pos,
            "terminal completion follows the script frame entry"
        );
        assert!(
            value_slot != carrier
                && value_slot != frame
                && value_slot != *completion
                && value_slot != *payload,
            "terminal completion publishes a computed value, got channel slot {value_slot}"
        );
        {
            let mut lists: Vec<&[S]> = vec![&ir.stmts];
            for stmt in ir.stmts.iter() {
                if let S::IfElse {
                    then_branch,
                    else_branch,
                    ..
                } = stmt
                {
                    lists.push(then_branch);
                    lists.push(else_branch);
                }
            }
            assert!(
                !lists
                    .iter()
                    .any(|list| seam_list_has_call(list, root_leave)),
                "no premature root_leave on the seam path"
            );
        }
        let mut fresh = || {
            let n = u16::try_from(ir.locals).expect("seam slot");
            ir.locals += 1;
            n
        };
        // Shared tail. Layout cells verified against javascript/heap.rs:
        // header 8 pending, 3 root top, 15 intrinsic frame, 16 page table
        // base; frame cell 2 previous link, cell 0 raw size, cell 3
        // capacity; entries start at cell 4 with 3 cells each; entry
        // category 5 marks a plain object; row state 2 is active.
        let mut tail: Vec<S> = Vec::new();
        let pending_num = fresh();
        tail.push(S::Let {
            index: pending_num,
            declared_schema: Some(T::Number),
            expr: read_cell(read, carrier, E::Float(8.0)),
            line: 1,
        });
        let pending = fresh();
        tail.push(S::Let {
            index: pending,
            declared_schema: Some(T::Int),
            expr: decoded(decode, E::Var(pending_num)),
            line: 1,
        });
        tail.push(assert_eq_expr(E::Var(pending), E::Int(0)));
        let top_num = fresh();
        tail.push(S::Let {
            index: top_num,
            declared_schema: Some(T::Number),
            expr: read_cell(read, carrier, E::Float(3.0)),
            line: 1,
        });
        let top = fresh();
        tail.push(S::Let {
            index: top,
            declared_schema: Some(T::Int),
            expr: decoded(decode, E::Var(top_num)),
            line: 1,
        });
        let perm_num = fresh();
        tail.push(S::Let {
            index: perm_num,
            declared_schema: Some(T::Number),
            expr: read_cell(read, carrier, E::Float(15.0)),
            line: 1,
        });
        let perm = fresh();
        tail.push(S::Let {
            index: perm,
            declared_schema: Some(T::Int),
            expr: decoded(decode, E::Var(perm_num)),
            line: 1,
        });
        tail.push(assert_stmt(E::Not(Box::new(E::Eq(
            Box::new(E::Var(top)),
            Box::new(E::Var(perm)),
        )))));
        let tprev = fresh();
        tail.push(S::Let {
            index: tprev,
            declared_schema: Some(T::Int),
            expr: decoded(
                decode,
                read_cell(
                    read,
                    carrier,
                    E::Add(Box::new(E::Var(top)), Box::new(E::Int(2))),
                ),
            ),
            line: 1,
        });
        tail.push(assert_eq_expr(E::Var(tprev), E::Var(perm)));
        let pprev = fresh();
        tail.push(S::Let {
            index: pprev,
            declared_schema: Some(T::Int),
            expr: decoded(
                decode,
                read_cell(
                    read,
                    carrier,
                    E::Add(Box::new(E::Var(perm)), Box::new(E::Int(2))),
                ),
            ),
            line: 1,
        });
        tail.push(assert_eq_expr(E::Var(pprev), E::Int(0)));
        let entry_cells = 4 + 3 * i64::from(entry);
        let cat = fresh();
        tail.push(S::Let {
            index: cat,
            declared_schema: Some(T::Int),
            expr: decoded(
                decode,
                read_cell(
                    read,
                    carrier,
                    E::Add(Box::new(E::Var(top)), Box::new(E::Int(entry_cells))),
                ),
            ),
            line: 1,
        });
        tail.push(assert_eq_expr(E::Var(cat), E::Int(5)));
        let slot = fresh();
        tail.push(S::Let {
            index: slot,
            declared_schema: Some(T::Int),
            expr: decoded(
                decode,
                read_cell(
                    read,
                    carrier,
                    E::Add(Box::new(E::Var(top)), Box::new(E::Int(entry_cells + 1))),
                ),
            ),
            line: 1,
        });
        let want_slot = fresh();
        tail.push(S::Let {
            index: want_slot,
            declared_schema: None,
            expr: boxed_field(object, "ref_slot"),
            line: 1,
        });
        tail.push(assert_eq_expr(E::Var(slot), E::Var(want_slot)));
        let entry_gen = fresh();
        tail.push(S::Let {
            index: entry_gen,
            declared_schema: Some(T::Int),
            expr: decoded_gen(
                decode_u32,
                read_cell(
                    read,
                    carrier,
                    E::Add(Box::new(E::Var(top)), Box::new(E::Int(entry_cells + 2))),
                ),
            ),
            line: 1,
        });
        let want_gen = fresh();
        tail.push(S::Let {
            index: want_gen,
            declared_schema: None,
            expr: boxed_field(object, "generation"),
            line: 1,
        });
        tail.push(assert_eq_expr(E::Var(entry_gen), E::Var(want_gen)));
        // Permanent entry 0 against its live directory identity. Non-empty
        // entries walk their real directory row (state active, generation
        // equal); empty entries must be fully zeroed raw memory.
        let cat0 = fresh();
        tail.push(S::Let {
            index: cat0,
            declared_schema: Some(T::Int),
            expr: decoded(
                decode,
                read_cell(
                    read,
                    carrier,
                    E::Add(Box::new(E::Var(perm)), Box::new(E::Int(4))),
                ),
            ),
            line: 1,
        });
        let slot0 = fresh();
        tail.push(S::Let {
            index: slot0,
            declared_schema: Some(T::Int),
            expr: decoded(
                decode,
                read_cell(
                    read,
                    carrier,
                    E::Add(Box::new(E::Var(perm)), Box::new(E::Int(5))),
                ),
            ),
            line: 1,
        });
        let gen0 = fresh();
        tail.push(S::Let {
            index: gen0,
            declared_schema: Some(T::Int),
            expr: decoded_gen(
                decode_u32,
                read_cell(
                    read,
                    carrier,
                    E::Add(Box::new(E::Var(perm)), Box::new(E::Int(6))),
                ),
            ),
            line: 1,
        });
        // Phase-1 row-state controls target the permanent entry 0's own live
        // directory row. Its address is a branch-local `Let`, so a test-only
        // outer Int is initialized to 0 here and receives the validated
        // address inside the branch below; it is read only after the branch
        // proved a non-empty live row.
        let state_target = matches!(
            self.fault,
            RootSeamFault::StateActiveFloat
                | RootSeamFault::StateActiveRawInt
                | RootSeamFault::StateFractional
                | RootSeamFault::StateNegative
                | RootSeamFault::StateNaN
                | RootSeamFault::StatePosInf
                | RootSeamFault::StateNegInf
                | RootSeamFault::StateDomainHigh
        );
        // Direct-marker controls reuse the same validated permanent entry 0
        // row, but the sole state write is followed by the real mark helper
        // instead of a collection. Every declaration and statement below is
        // gated on this selector so the older variants emit identical IR.
        let marker_target = matches!(
            self.fault,
            RootSeamFault::MarkerStateActiveFloat
                | RootSeamFault::MarkerStateActiveRawInt
                | RootSeamFault::MarkerStateFractional
                | RootSeamFault::MarkerStateNegative
                | RootSeamFault::MarkerStateNaN
                | RootSeamFault::MarkerStatePosInf
                | RootSeamFault::MarkerStateNegInf
                | RootSeamFault::MarkerStateInactive
        );
        let state_row_addr = if state_target || marker_target {
            let slot = fresh();
            tail.push(S::Let {
                index: slot,
                declared_schema: Some(T::Int),
                expr: E::Int(0),
                line: 1,
            });
            Some(slot)
        } else {
            None
        };
        // Test-only outer Ints for the direct-marker call: the validated row's
        // allocated slot and original generation are copied inside the
        // permanent entry 0 branch after every identity check, so the mark
        // call can run after the branch without reading a branch-local `Let`.
        // Older variants keep their exact emission.
        let marker_exports = if marker_target {
            let slot_out = fresh();
            tail.push(S::Let {
                index: slot_out,
                declared_schema: Some(T::Int),
                expr: E::Int(0),
                line: 1,
            });
            let gen_out = fresh();
            tail.push(S::Let {
                index: gen_out,
                declared_schema: Some(T::Int),
                expr: E::Int(0),
                line: 1,
            });
            Some((slot_out, gen_out))
        } else {
            None
        };
        let page = fresh();
        let row = fresh();
        let page_ptr_num = fresh();
        let page_ptr = fresh();
        let row_addr = fresh();
        let row_state = fresh();
        let row_gen = fresh();
        let mut permanent_row_guard = S::IfElse {
            condition: E::Not(Box::new(E::Eq(Box::new(E::Var(cat0)), Box::new(E::Int(0))))),
            then_branch: vec![
                S::Let {
                    index: page,
                    declared_schema: Some(T::Int),
                    expr: E::Int(0),
                    line: 1,
                },
                S::Let {
                    index: row,
                    declared_schema: Some(T::Int),
                    expr: E::Var(slot0),
                    line: 1,
                },
                S::While {
                    condition: E::Or(
                        Box::new(E::Gt(Box::new(E::Var(row)), Box::new(E::Int(64)))),
                        Box::new(E::Eq(Box::new(E::Var(row)), Box::new(E::Int(64)))),
                    ),
                    body: vec![
                        S::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: row,
                            expr: E::Sub(Box::new(E::Var(row)), Box::new(E::Int(64))),
                            line: 1,
                        },
                        S::Assign {
                            kind: vm::AssignmentKind::Set,
                            index: page,
                            expr: E::Add(Box::new(E::Var(page)), Box::new(E::Int(1))),
                            line: 1,
                        },
                    ],
                    line: 1,
                },
                S::Let {
                    index: page_ptr_num,
                    declared_schema: Some(T::Number),
                    expr: read_cell(
                        read,
                        carrier,
                        E::Add(Box::new(E::Int(16)), Box::new(E::Var(page))),
                    ),
                    line: 1,
                },
                S::Let {
                    index: page_ptr,
                    declared_schema: Some(T::Int),
                    expr: decoded(decode, E::Var(page_ptr_num)),
                    line: 1,
                },
                assert_stmt(E::Not(Box::new(E::Eq(
                    Box::new(E::Var(page_ptr)),
                    Box::new(E::Int(0)),
                )))),
                S::Let {
                    index: row_addr,
                    declared_schema: Some(T::Int),
                    expr: E::Add(
                        Box::new(E::Add(Box::new(E::Var(page_ptr)), Box::new(E::Int(3)))),
                        Box::new(E::Mul(Box::new(E::Var(row)), Box::new(E::Int(7)))),
                    ),
                    line: 1,
                },
                S::Let {
                    index: row_state,
                    declared_schema: Some(T::Int),
                    expr: decoded(decode, read_cell(read, carrier, E::Var(row_addr))),
                    line: 1,
                },
                assert_eq_expr(E::Var(row_state), E::Int(2)),
                S::Let {
                    index: row_gen,
                    declared_schema: Some(T::Int),
                    expr: decoded_gen(
                        decode_u32,
                        read_cell(
                            read,
                            carrier,
                            E::Add(Box::new(E::Var(row_addr)), Box::new(E::Int(1))),
                        ),
                    ),
                    line: 1,
                },
                assert_eq_expr(E::Var(row_gen), E::Var(gen0)),
            ],
            else_branch: vec![
                assert_eq_expr(E::Var(slot0), E::Int(0)),
                assert_eq_expr(E::Var(gen0), E::Int(0)),
            ],
            line: 1,
        };
        if let Some(row_slot) = state_row_addr {
            // Test-only: publish the branch-local row address to the outer
            // slot, and prove the row still sits inside the phase-1 scan
            // window [0, HDR_NEXT_SLOT). Header cell 4 is the allocated slot
            // count; every identity check above already passed.
            let S::IfElse { then_branch, .. } = &mut permanent_row_guard else {
                unreachable!("permanent entry row guard");
            };
            then_branch.push(S::Assign {
                kind: vm::AssignmentKind::Set,
                index: row_slot,
                expr: E::Var(row_addr),
                line: 1,
            });
            let next_slot_num = fresh();
            then_branch.push(S::Let {
                index: next_slot_num,
                declared_schema: Some(T::Number),
                expr: read_cell(read, carrier, E::Int(4)),
                line: 1,
            });
            let next_slot = fresh();
            then_branch.push(S::Let {
                index: next_slot,
                declared_schema: Some(T::Int),
                expr: decoded(decode, E::Var(next_slot_num)),
                line: 1,
            });
            then_branch.push(assert_stmt(E::Lt(
                Box::new(E::Var(slot0)),
                Box::new(E::Var(next_slot)),
            )));
        }
        if let Some((slot_out, gen_out)) = marker_exports {
            // Export the validated row's allocated slot and original
            // generation only after every identity check in the branch above
            // passed. The row address was exported there too.
            let S::IfElse { then_branch, .. } = &mut permanent_row_guard else {
                unreachable!("permanent entry row guard");
            };
            then_branch.push(S::Assign {
                kind: vm::AssignmentKind::Set,
                index: slot_out,
                expr: E::Var(slot0),
                line: 1,
            });
            then_branch.push(S::Assign {
                kind: vm::AssignmentKind::Set,
                index: gen_out,
                expr: E::Var(gen0),
                line: 1,
            });
        }
        tail.push(permanent_row_guard);
        if let Some(row_slot) = state_row_addr {
            // Read the outer slot only after the branch proved a non-empty
            // permanent entry 0 with a live page pointer and an active row.
            tail.push(assert_stmt(E::Not(Box::new(E::Eq(
                Box::new(E::Var(row_slot)),
                Box::new(E::Int(0)),
            )))));
            // Permanent entry 0 retains the bootstrap Object prototype; `o`
            // is allocated separately. This check concerns frame placement,
            // not the distinct object identities established by the fixture.
            tail.push(assert_stmt(E::Not(Box::new(E::Eq(
                Box::new(E::Var(perm)),
                Box::new(E::Var(top)),
            )))));
        }
        if marker_target {
            // Permanent entry 0 is the bootstrap Object prototype, a distinct
            // object from the separately allocated `o`. Prove its node kind
            // through the validated physical base; the frame placement
            // inequality above is not object identity proof.
            let row_slot = state_row_addr.expect("marker row slot");
            let base_num = fresh();
            tail.push(S::Let {
                index: base_num,
                declared_schema: Some(T::Number),
                expr: read_cell(
                    read,
                    carrier,
                    number(E::Add(Box::new(E::Var(row_slot)), Box::new(E::Int(2)))),
                ),
                line: 1,
            });
            let base = fresh();
            tail.push(S::Let {
                index: base,
                declared_schema: Some(T::Int),
                expr: decoded(decode, E::Var(base_num)),
                line: 1,
            });
            let kind_num = fresh();
            tail.push(S::Let {
                index: kind_num,
                declared_schema: Some(T::Number),
                expr: read_cell(read, carrier, E::Var(base)),
                line: 1,
            });
            let kind = fresh();
            tail.push(S::Let {
                index: kind,
                declared_schema: Some(T::Int),
                expr: decoded(decode, E::Var(kind_num)),
                line: 1,
            });
            tail.push(assert_eq_expr(E::Var(kind), E::Int(3)));
        }
        // Category controls target the canonical payload entry parsed above:
        // a distinct, in-bounds script-frame entry that is currently EMPTY.
        // These reads resolve and check the address before the sole fault
        // write, and nothing allocates between that write and the immediate
        // collection below.
        let category_target = matches!(
            self.fault,
            RootSeamFault::CategoryFloatPosZero
                | RootSeamFault::CategoryFloatNegZero
                | RootSeamFault::CategoryRawIntZero
                | RootSeamFault::CategoryFractional
                | RootSeamFault::CategoryNegative
                | RootSeamFault::CategoryNaN
                | RootSeamFault::CategoryPosInf
                | RootSeamFault::CategoryNegInf
        );
        let mut category_cells = 0i64;
        if category_target {
            let target = u16::try_from(*r_entry).expect("category target is a u16 entry");
            assert_ne!(
                target, entry,
                "category target is distinct from o's root entry"
            );
            let frame_cap_num = fresh();
            tail.push(S::Let {
                index: frame_cap_num,
                declared_schema: Some(T::Number),
                expr: read_cell(
                    read,
                    carrier,
                    E::Add(Box::new(E::Var(top)), Box::new(E::Int(3))),
                ),
                line: 1,
            });
            let frame_cap = fresh();
            tail.push(S::Let {
                index: frame_cap,
                declared_schema: Some(T::Int),
                expr: decoded(decode, E::Var(frame_cap_num)),
                line: 1,
            });
            tail.push(assert_stmt(E::Lt(
                Box::new(E::Int(i64::from(target))),
                Box::new(E::Var(frame_cap)),
            )));
            category_cells = 4 + 3 * i64::from(target);
            let cat_num = fresh();
            tail.push(S::Let {
                index: cat_num,
                declared_schema: Some(T::Number),
                expr: read_cell(
                    read,
                    carrier,
                    E::Add(Box::new(E::Var(top)), Box::new(E::Int(category_cells))),
                ),
                line: 1,
            });
            let cat_now = fresh();
            tail.push(S::Let {
                index: cat_now,
                declared_schema: Some(T::Int),
                expr: decoded(decode, E::Var(cat_num)),
                line: 1,
            });
            tail.push(assert_eq_expr(E::Var(cat_now), E::Int(0)));
        }
        // One selected numeric fault write (or none for the clean control),
        // then the immediate collection. No allocation, guest callback,
        // root rewrite or intrinsic call may sit between the write and the
        // collect; every payload below was resolved above.
        let top_addr = |cells: i64| number(E::Add(Box::new(E::Var(perm)), Box::new(E::Int(cells))));
        match self.fault {
            RootSeamFault::Clean => {}
            RootSeamFault::PrevLink => {
                let written = fresh();
                tail.push(S::Let {
                    index: written,
                    declared_schema: Some(T::Number),
                    expr: call(write, vec![E::Var(carrier), top_addr(2), E::Float(0.5)]),
                    line: 1,
                });
            }
            RootSeamFault::BottomCap => {
                let cap_num = fresh();
                tail.push(S::Let {
                    index: cap_num,
                    declared_schema: Some(T::Number),
                    expr: read_cell(
                        read,
                        carrier,
                        E::Add(Box::new(E::Var(perm)), Box::new(E::Int(3))),
                    ),
                    line: 1,
                });
                let cap_now = fresh();
                tail.push(S::Let {
                    index: cap_now,
                    declared_schema: Some(T::Int),
                    expr: decoded(decode, E::Var(cap_num)),
                    line: 1,
                });
                let written = fresh();
                tail.push(S::Let {
                    index: written,
                    declared_schema: Some(T::Number),
                    expr: call(
                        write,
                        vec![
                            E::Var(carrier),
                            top_addr(3),
                            number(E::Sub(Box::new(E::Var(cap_now)), Box::new(E::Int(1)))),
                        ],
                    ),
                    line: 1,
                });
            }
            RootSeamFault::BottomExtent => {
                let size_num = fresh();
                tail.push(S::Let {
                    index: size_num,
                    declared_schema: Some(T::Number),
                    expr: read_cell(
                        read,
                        carrier,
                        E::Add(Box::new(E::Var(perm)), Box::new(E::Int(0))),
                    ),
                    line: 1,
                });
                let size_now = fresh();
                tail.push(S::Let {
                    index: size_now,
                    declared_schema: Some(T::Int),
                    expr: decoded(decode, E::Var(size_num)),
                    line: 1,
                });
                let written = fresh();
                tail.push(S::Let {
                    index: written,
                    declared_schema: Some(T::Number),
                    expr: call(
                        write,
                        vec![
                            E::Var(carrier),
                            top_addr(0),
                            number(E::Sub(Box::new(E::Var(size_now)), Box::new(E::Int(3)))),
                        ],
                    ),
                    line: 1,
                });
            }
            RootSeamFault::BottomIdentity => {
                let written = fresh();
                tail.push(S::Let {
                    index: written,
                    declared_schema: Some(T::Number),
                    expr: call(
                        write,
                        vec![E::Var(carrier), E::Float(15.0), E::Var(top_num)],
                    ),
                    line: 1,
                });
            }
            RootSeamFault::StaleGen => {
                let gen_num = fresh();
                tail.push(S::Let {
                    index: gen_num,
                    declared_schema: Some(T::Number),
                    expr: read_cell(
                        read,
                        carrier,
                        E::Add(Box::new(E::Var(perm)), Box::new(E::Int(6))),
                    ),
                    line: 1,
                });
                let written = fresh();
                tail.push(S::Let {
                    index: written,
                    declared_schema: Some(T::Number),
                    expr: call(
                        write,
                        vec![
                            E::Var(carrier),
                            top_addr(6),
                            E::Add(Box::new(E::Var(gen_num)), Box::new(E::Float(1.0))),
                        ],
                    ),
                    line: 1,
                });
            }
            fault @ (RootSeamFault::StateActiveFloat
            | RootSeamFault::StateActiveRawInt
            | RootSeamFault::StateFractional
            | RootSeamFault::StateNegative
            | RootSeamFault::StateNaN
            | RootSeamFault::StatePosInf
            | RootSeamFault::StateNegInf
            | RootSeamFault::StateDomainHigh) => {
                // Exactly one phase-1 row-state write. The raw Int payloads
                // stay Int through the B1 numeric write (no explicit Float
                // cast), the Float cases keep their producer literal, and the
                // cell is the row's own state cell (heap layout ROW_STATE=0).
                let payload = match fault {
                    RootSeamFault::StateActiveFloat => E::Float(2.0),
                    RootSeamFault::StateActiveRawInt => E::Int(2),
                    RootSeamFault::StateFractional => E::Float(0.5),
                    RootSeamFault::StateNegative => E::Float(-1.0),
                    RootSeamFault::StateNaN => E::Float(f64::NAN),
                    RootSeamFault::StatePosInf => E::Float(f64::INFINITY),
                    RootSeamFault::StateNegInf => E::Float(f64::NEG_INFINITY),
                    RootSeamFault::StateDomainHigh => E::Int(5),
                    _ => unreachable!("state payload selector"),
                };
                let row = state_row_addr.expect("state row slot");
                let written = fresh();
                tail.push(S::Let {
                    index: written,
                    declared_schema: Some(T::Number),
                    expr: call(
                        write,
                        vec![
                            E::Var(carrier),
                            number(E::Add(Box::new(E::Var(row)), Box::new(E::Int(0)))),
                            payload,
                        ],
                    ),
                    line: 1,
                });
            }
            fault @ (RootSeamFault::CategoryFloatPosZero
            | RootSeamFault::CategoryFloatNegZero
            | RootSeamFault::CategoryRawIntZero
            | RootSeamFault::CategoryFractional
            | RootSeamFault::CategoryNegative
            | RootSeamFault::CategoryNaN
            | RootSeamFault::CategoryPosInf
            | RootSeamFault::CategoryNegInf) => {
                // Exactly one numeric category-cell write. The raw Int zero
                // stays an Int payload through the B1 numeric write; the
                // Float cases preserve their producer literal bit-for-bit.
                let payload = match fault {
                    RootSeamFault::CategoryFloatPosZero => E::Float(0.0),
                    RootSeamFault::CategoryFloatNegZero => E::Float(-0.0),
                    RootSeamFault::CategoryRawIntZero => E::Int(0),
                    RootSeamFault::CategoryFractional => E::Float(0.5),
                    RootSeamFault::CategoryNegative => E::Float(-1.0),
                    RootSeamFault::CategoryNaN => E::Float(f64::NAN),
                    RootSeamFault::CategoryPosInf => E::Float(f64::INFINITY),
                    RootSeamFault::CategoryNegInf => E::Float(f64::NEG_INFINITY),
                    _ => unreachable!("category payload selector"),
                };
                let written = fresh();
                tail.push(S::Let {
                    index: written,
                    declared_schema: Some(T::Number),
                    expr: call(
                        write,
                        vec![
                            E::Var(carrier),
                            number(E::Add(
                                Box::new(E::Var(top)),
                                Box::new(E::Int(category_cells)),
                            )),
                            payload,
                        ],
                    ),
                    line: 1,
                });
            }
            // Direct-marker variants write their state and call the existing
            // marker in the gated block below; this seam adds no fault write.
            RootSeamFault::MarkerStateActiveFloat
            | RootSeamFault::MarkerStateActiveRawInt
            | RootSeamFault::MarkerStateFractional
            | RootSeamFault::MarkerStateNegative
            | RootSeamFault::MarkerStateNaN
            | RootSeamFault::MarkerStatePosInf
            | RootSeamFault::MarkerStateNegInf
            | RootSeamFault::MarkerStateInactive => {}
        }
        if marker_target {
            // One B1 numeric ROW_STATE write at the validated row address,
            // then a fresh HDR_NEXT_SLOT read/decode for the marker's fifth
            // argument, then the existing real mark helper with the saved
            // slot, the original generation and expect=3. No allocation,
            // callback, collection or root rewrite sits between them.
            let payload = match self.fault {
                RootSeamFault::MarkerStateActiveFloat => E::Float(2.0),
                RootSeamFault::MarkerStateActiveRawInt => E::Int(2),
                RootSeamFault::MarkerStateFractional => E::Float(0.5),
                RootSeamFault::MarkerStateNegative => E::Float(-1.0),
                RootSeamFault::MarkerStateNaN => E::Float(f64::NAN),
                RootSeamFault::MarkerStatePosInf => E::Float(f64::INFINITY),
                RootSeamFault::MarkerStateNegInf => E::Float(f64::NEG_INFINITY),
                RootSeamFault::MarkerStateInactive => E::Int(5),
                _ => unreachable!("marker state payload selector"),
            };
            let row_slot = state_row_addr.expect("marker row slot");
            let (slot_out, gen_out) = marker_exports.expect("marker slot exports");
            let written = fresh();
            tail.push(S::Let {
                index: written,
                declared_schema: Some(T::Number),
                expr: call(
                    write,
                    vec![
                        E::Var(carrier),
                        number(E::Add(Box::new(E::Var(row_slot)), Box::new(E::Int(0)))),
                        payload,
                    ],
                ),
                line: 1,
            });
            // The direct unit call lives outside the collector, so it reads and
            // decodes this same carrier's HDR_NEXT_SLOT cell 4 afresh after the
            // old row-state write and before the marker call.
            let next_int = fresh();
            tail.push(S::Let {
                index: next_int,
                declared_schema: Some(T::Int),
                expr: decoded(decode, read_cell(read, carrier, E::Int(4))),
                line: 1,
            });
            let marked = fresh();
            tail.push(S::Let {
                index: marked,
                declared_schema: Some(T::Int),
                expr: call(
                    mark,
                    vec![
                        E::Var(carrier),
                        E::Var(slot_out),
                        E::Var(gen_out),
                        E::Int(3),
                        E::Var(next_int),
                    ],
                ),
                line: 1,
            });
        }
        if !marker_target {
            let collected = fresh();
            tail.push(S::Let {
                index: collected,
                declared_schema: Some(T::Int),
                expr: call(collect, vec![E::Var(carrier)]),
                line: 1,
            });
        }
        ir.stmts.extend(tail);
        *self.carrier.lock().unwrap() = Some(carrier);
        *self.frame.lock().unwrap() = Some(frame);
        *self.entry.lock().unwrap() = Some(entry);
        *self.object.lock().unwrap() = Some(object);
        Ok(ir)
    }
}

#[test]
fn p7_roots_previous_link_valid_multiframe_interpreter() {
    let source = "var s=\"edge\";var o={s:s};function f(x){function g(y){return y.s;} return g(x)+\"!\";} f(o)===\"edge!\"&&o.s===s;";
    let actual = run_mode(
        source,
        Some(object_observer::Observer {
            force_gc: true,
            ..Default::default()
        }),
        false,
    );
    let marker = actual
        .iter()
        .position(|v| *v == Value::string("P7_OBSERVED"))
        .expect("observed completion marker");
    assert_eq!(field(&actual[marker + 1], "tag"), Value::Int(2));
    assert_eq!(field(&actual[marker + 1], "flag"), Value::Bool(true));
    assert_eq!(actual[actual.len() - 2], Value::Int(0), "no pending node");
    eprintln!("P7_ROOTS_VALID_MULTIFRAME tail={:?}", &actual[marker..]);
}

/// Matched clean control for the post-source seam: the same unchanged fixture
/// with no corruption, so the forced collection runs over the intact
/// bootstrap chain after `o` is published and the script completes. The
/// truth representation observes the actual boxed completion on the stack,
/// never a printed rendering of a raw boolean.
#[test]
fn p7_roots_previous_link_clean_collect_control_interpreter() {
    let plugin: &'static RootSeam = Box::leak(Box::new(RootSeam {
        fault: RootSeamFault::Clean,
        helpers: std::sync::Mutex::new(Vec::new()),
        sorted_impl_keys: std::sync::Mutex::new(Vec::new()),
        carrier: std::sync::Mutex::new(None),
        frame: std::sync::Mutex::new(None),
        entry: std::sync::Mutex::new(None),
        object: std::sync::Mutex::new(None),
    }));
    let tapped: &'static dyn vm::SourcePlugin = plugin;
    let (mut machine, _) =
        build_machine("var o={v:1};o.v===1;", MachineSource::Tapped(tapped), false);
    let status = machine.run();
    assert!(
        !machine.jit_config().enabled,
        "interpreter only on the clean control"
    );
    assert_eq!(machine.aot_exec_count(), 0, "no AOT on the clean control");
    assert_eq!(
        machine.jit_native_exec_count(),
        0,
        "no trace execution on the clean control"
    );
    assert_eq!(
        status.unwrap_or_else(|error| panic!(
            "clean collect control must halt: {error:?} ip={} frames={:?}",
            machine.ip(),
            machine.execution_frames()
        )),
        VmStatus::Halted
    );
    assert_eq!(
        machine.host_context().resource_count(),
        1,
        "one actual backing owner"
    );
    let backing = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("actual backing accounting");
    assert_eq!((backing.pending, backing.registered), (0, 1));
    assert!(
        plugin.carrier.lock().unwrap().is_some()
            && plugin.frame.lock().unwrap().is_some()
            && plugin.entry.lock().unwrap().is_some()
            && plugin.object.lock().unwrap().is_some(),
        "post-source seam discovery bound carrier, frame, entry and object"
    );
    RootSeam::contract(&plugin.helpers.lock().unwrap(), "__compat_es5_heap_collect");
    let actual = machine.stack().to_vec();
    let last = actual.last().expect("script value");
    assert_eq!(field(last, "tag"), Value::Int(2));
    assert_eq!(field(last, "flag"), Value::Bool(true));
    machine.reset_for_reuse().expect("reset");
    assert_eq!(
        machine.host_context().resource_count(),
        0,
        "reset releases the real owner"
    );
    eprintln!("P7_ROOTS_CLEAN_COLLECT tail={actual:?}");
}

/// Regions linked to runtime prototypes, sorted by entry address so the
/// refusal ip resolves to its owning prototype deterministically, without
/// reading debug-info name positions.
fn prev_link_sorted_regions(program: &vm::Program) -> Vec<(u32, u32, u32)> {
    let mut linked: Vec<(u32, u32, u32)> = program
        .function_regions
        .iter()
        .filter_map(|region| {
            region
                .prototype_id
                .map(|id| (region.start_ip, region.end_ip, id))
        })
        .collect();
    linked.sort();
    linked
}

/// Runtime prototype bound through the actual sorted emission order and the
/// real code bounds: the prototype's target must be the script function
/// whose bounds equal its linked region, with the declared arity and
/// parameter count. Arity alone never identifies a helper.
fn assert_runtime_prototype(
    program: &vm::Program,
    linked: &[(u32, u32, u32)],
    helpers: &[PrevLinkHelper],
    name: &str,
    prototype_id: u32,
) {
    let (arity, params) = RootSeam::contract(helpers, name);
    let prototype = &program.callable_prototypes[prototype_id as usize];
    assert_eq!(
        (prototype.arity, prototype.parameter_slots.len()),
        (arity, params),
        "{name} runs inside its actual runtime prototype"
    );
    let script_id = match prototype.target {
        vm::CallableTarget::ScriptFunction(id) => id,
        other => panic!("{name} prototype must target a script function, got {other:?}"),
    };
    let script = &program.script_functions[script_id as usize];
    let region = linked
        .iter()
        .find(|region| region.2 == prototype_id)
        .unwrap_or_else(|| panic!("{name} prototype {prototype_id} has no linked region"));
    assert_eq!(
        (script.entry_ip, script.end_ip),
        (region.0, region.1),
        "{name} prototype target matches its linked region bounds"
    );
}

/// Every resolved helper must share one sorted-emission base (runtime
/// prototype id minus logical sorted position). A fixed historical index is
/// never hardcoded; only the common base is required.
fn assert_emission_base(helpers: &[PrevLinkHelper], sorted_keys: &[u16], resolved: &[(&str, u32)]) {
    let mut bases = Vec::new();
    for (name, prototype_id) in resolved {
        let helper = helpers
            .iter()
            .find(|helper| helper.name == *name)
            .unwrap_or_else(|| panic!("missing logical decl {name}"));
        let position = sorted_keys
            .iter()
            .position(|key| *key == helper.index)
            .unwrap_or_else(|| panic!("logical {name} missing from sorted emission"));
        bases.push((*prototype_id as i64) - (position as i64));
    }
    assert!(
        bases.windows(2).all(|pair| pair[0] == pair[1]),
        "one sorted-emission base for {resolved:?}, got {bases:?}"
    );
}

/// The single instruction ending exactly at `ip`, decoded linearly from the
/// owning region start so the boundary never starts mid-operand.
fn prev_link_instr_ending_at(
    code: &[u8],
    region_start: usize,
    ip: usize,
    what: &str,
) -> (usize, vm::OpCode) {
    let entries = prev_link_disasm_entries(code, region_start);
    for (addr, _) in &entries {
        if *addr > ip {
            break;
        }
        let op = vm::OpCode::try_from(code[*addr])
            .unwrap_or_else(|_| panic!("{what}: undecodable opcode at {addr}"));
        if *addr + 1 + op.operand_len() == ip {
            return (*addr, op);
        }
    }
    panic!("{what}: no instruction ends at {ip}");
}

/// Test-only tag for the exact fired guard: the instruction ending at the
/// failure ip must be the assert builtin call, debug-mapped, with its end at
/// the failure ip. Guard conditions are never changed to obtain this tag.
fn tag_guard_site(program: &vm::Program, region_start: u32, ip: usize, what: &str) -> usize {
    let code = &program.code;
    let (start, op) = prev_link_instr_ending_at(code, region_start as usize, ip, what);
    assert!(
        matches!(op, vm::OpCode::Call),
        "{what}: fired guard must be a builtin call, got {op:?}"
    );
    assert_eq!(op.operand_len(), 3, "{what}: assert call shape");
    let target = u16::from_le_bytes([code[start + 1], code[start + 2]]);
    assert_eq!(
        target,
        vm::BuiltinFunction::Assert.call_index(),
        "{what}: fired guard must call assert"
    );
    assert_eq!(code[start + 3], 1, "{what}: assert argc");
    let line = program
        .debug
        .as_ref()
        .expect("debug info")
        .line_for_offset(start);
    assert!(
        line.is_some(),
        "{what}: fired guard site must be debug-mapped"
    );
    eprintln!("P7_ROOTS_GUARD_TAG {what} assert_start={start} assert_end={ip} debug_line={line:?}");
    start
}

/// Test-only tag for the current decode helper's exact Int conversion: the
/// instruction ending at the failure ip must be the one
/// `__checked_int_from_number` builtin call in its owning decoder region,
/// debug-mapped, with exactly one argument. The same region must still carry
/// exactly one lower-bound and one upper-bound Assert domain guard, classified
/// by `category_guard`, and neither may be the failing instruction. This
/// proves the fractional input passed the real range guards and failed only at
/// the exact conversion.
fn tag_exact_conversion_site(
    program: &vm::Program,
    region_start: u32,
    region_end: u32,
    ip: usize,
    what: &str,
) -> usize {
    let code = &program.code;
    assert!(
        (region_start as usize) < (region_end as usize),
        "{what}: decoder region must be a non-empty range"
    );
    assert!(
        (region_start as usize) <= ip && ip <= (region_end as usize),
        "{what}: failure ip must sit inside the decoder region"
    );
    let (start, op) = prev_link_instr_ending_at(code, region_start as usize, ip, what);
    assert!(
        start < region_end as usize,
        "{what}: failing instruction must start inside the decoder region"
    );
    assert!(
        matches!(op, vm::OpCode::Call),
        "{what}: fired exact conversion must be a builtin call, got {op:?}"
    );
    assert_eq!(op.operand_len(), 3, "{what}: exact conversion call shape");
    let target = u16::from_le_bytes([code[start + 1], code[start + 2]]);
    let exact = vm::BuiltinFunction::CheckedIntFromNumber.call_index();
    assert_eq!(
        target, exact,
        "{what}: fired instruction must call __checked_int_from_number"
    );
    assert_eq!(code[start + 3], 1, "{what}: exact conversion argc");
    let line = program
        .debug
        .as_ref()
        .expect("debug info")
        .line_for_offset(start);
    assert!(
        line.is_some(),
        "{what}: fired exact conversion must be debug-mapped"
    );
    let ops = prev_link_ops(code, region_start as usize);
    let mut conversion_calls = Vec::new();
    let mut lower_bounds = Vec::new();
    let mut upper_bounds = Vec::new();
    for (addr, candidate, operands) in &ops {
        if *addr >= region_end as usize {
            break;
        }
        assert!(
            *addr + 1 + candidate.operand_len() <= region_end as usize,
            "{what}: instruction at {addr} crosses the decoder region end"
        );
        if !matches!(candidate, vm::OpCode::Call) {
            continue;
        }
        let call_target = u16::from_le_bytes([operands[0], operands[1]]);
        if call_target == exact {
            conversion_calls.push(*addr);
            assert_eq!(operands[2], 1, "{what}: exact conversion argc at {addr}");
            continue;
        }
        if call_target == vm::BuiltinFunction::Assert.call_index() {
            assert_eq!(operands[2], 1, "{what}: Assert argc at {addr}");
            match category_guard(program, region_start as usize, *addr) {
                CategoryGuard::LowerBound => lower_bounds.push(*addr),
                CategoryGuard::UpperBound => upper_bounds.push(*addr),
                CategoryGuard::ExactConversion => panic!(
                    "{what}: category_guard must not classify an Assert call as exact conversion"
                ),
            }
        }
    }
    assert_eq!(
        conversion_calls,
        vec![start],
        "{what}: decoder region must carry exactly the failing exact conversion call"
    );
    assert_eq!(
        lower_bounds.len(),
        1,
        "{what}: decoder region must keep exactly one lower-bound Assert guard, got {lower_bounds:?}"
    );
    assert_eq!(
        upper_bounds.len(),
        1,
        "{what}: decoder region must keep exactly one upper-bound Assert guard, got {upper_bounds:?}"
    );
    for bound in lower_bounds.iter().chain(upper_bounds.iter()) {
        assert!(
            *bound < start,
            "{what}: Assert domain guard at {bound} must precede the failing exact conversion"
        );
        assert!(
            *bound != start,
            "{what}: Assert domain guard at {bound} must not be the failing instruction"
        );
        assert!(
            *bound + 1 + vm::OpCode::Call.operand_len() != ip,
            "{what}: Assert domain guard at {bound} must not end at the failure ip"
        );
    }
    eprintln!(
        "P7_ROOTS_EXACT_CONVERSION {what} call_start={start} call_end={ip} debug_line={line:?} \
         bounds=({lower_bounds:?},{upper_bounds:?})"
    );
    start
}

/// The actual callsite that entered the refusing callee: the instruction
/// ending at the callee frame's own continuation return ip must be the
/// script call targeting the callee prototype with its arity. The
/// continuation belongs to the entered callee, never to its caller.
fn assert_call_site(
    program: &vm::Program,
    caller_region_start: u32,
    return_ip: usize,
    expect_proto: u32,
    expect_arity: u8,
    what: &str,
) {
    let code = &program.code;
    let (start, op) =
        prev_link_instr_ending_at(code, caller_region_start as usize, return_ip, what);
    assert!(
        matches!(op, vm::OpCode::CallScript),
        "{what}: callsite must be a script call, got {op:?}"
    );
    assert_eq!(op.operand_len(), 5, "{what}: script call shape");
    let target = u32::from_le_bytes([
        code[start + 1],
        code[start + 2],
        code[start + 3],
        code[start + 4],
    ]);
    assert_eq!(
        target, expect_proto,
        "{what}: callsite targets the entered callee"
    );
    assert_eq!(code[start + 5], expect_arity, "{what}: callsite arity");
    eprintln!("P7_ROOTS_CALLSITE {what} call_start={start} call_end={return_ip} target={target}");
}

/// Bounded frame dump: depth, prototype, base, continuation and the live
/// parameter slots read through the actual parameter slots and frame base.
fn eprint_seam_frames(machine: &vm::Vm, what: &str) {
    let program = machine.program();
    let locals = machine.locals();
    for (depth, frame) in machine.execution_frames().iter().enumerate() {
        match frame.prototype_id {
            Some(pid) => {
                let prototype = program.callable_prototypes.get(pid as usize);
                let mut args: Vec<String> = Vec::new();
                if let Some(prototype) = prototype {
                    for slot in prototype.parameter_slots.iter() {
                        let value = locals.get(frame.local_base.saturating_add(usize::from(*slot)));
                        args.push(format!("slot{slot}={value:?}"));
                    }
                }
                eprintln!(
                    "P7_ROOTS_FRAMES {what} depth={depth} proto={pid} base={} count={} continuation={:?} args=[{}]",
                    frame.local_base,
                    frame.local_count,
                    frame.continuation,
                    args.join(" ")
                );
            }
            None => {
                eprintln!(
                    "P7_ROOTS_FRAMES {what} depth={depth} root base={} count={} continuation={:?}",
                    frame.local_base, frame.local_count, frame.continuation
                );
            }
        }
    }
}

/// Owning region start for an ip, over all linked regions including root.
fn prev_link_owner_start(program: &vm::Program, target: usize) -> Option<usize> {
    program
        .function_regions
        .iter()
        .find(|region| (region.start_ip as usize) <= target && target < (region.end_ip as usize))
        .map(|region| region.start_ip as usize)
}

/// Linear decode from a region start using real operand lengths, so the
/// reported instruction boundaries never start mid-operand.
fn prev_link_disasm_entries(code: &[u8], start: usize) -> Vec<(usize, String)> {
    let mut entries = Vec::new();
    let mut addr = start;
    let mut guard = 0usize;
    while addr < code.len() && guard < 4_000_000 {
        guard += 1;
        let byte = code[addr];
        match vm::OpCode::try_from(byte) {
            Ok(op) => {
                let mut text = format!("{addr}: {op:?}");
                for i in 1..=op.operand_len() {
                    match code.get(addr.saturating_add(i)) {
                        Some(b) => text.push_str(&format!(" {b}")),
                        None => text.push_str(" TRUNC"),
                    }
                }
                let next = addr.saturating_add(1).saturating_add(op.operand_len());
                entries.push((addr, text));
                if next <= addr {
                    break;
                }
                addr = next;
            }
            Err(_) => {
                entries.push((addr, format!("{addr}: INVALID({byte})")));
                addr = addr.saturating_add(1);
            }
        }
    }
    entries
}

/// Decoded neighborhood around one ip with preceding instruction boundaries.
fn prev_link_dump_window(code: &[u8], program: &vm::Program, target: usize, label: &str) {
    if target >= code.len() {
        eprintln!(
            "P7_ROOTS_CLEAN_WINDOW {label} target={target} OUT_OF_CODE len={}",
            code.len()
        );
        return;
    }
    let Some(start) = prev_link_owner_start(program, target) else {
        eprintln!("P7_ROOTS_CLEAN_WINDOW {label} target={target} NO_OWNING_REGION");
        return;
    };
    let entries = prev_link_disasm_entries(code, start);
    let Some(pos) = entries.iter().rposition(|(addr, _)| *addr <= target) else {
        eprintln!("P7_ROOTS_CLEAN_WINDOW {label} target={target} EMPTY_BEFORE");
        return;
    };
    let next_addr = entries.get(pos + 1).map_or(code.len(), |(addr, _)| *addr);
    let boundary = if entries[pos].0 == target {
        "AT_INSTR_START"
    } else if target < next_addr {
        "INSIDE_OPERAND"
    } else {
        "PAST_END"
    };
    let begin = pos.saturating_sub(6);
    let end = pos.saturating_add(3).min(entries.len());
    eprintln!(
        "P7_ROOTS_CLEAN_WINDOW {label} target={target} region_start={start} pos={pos} boundary={boundary}"
    );
    for (addr, text) in entries.iter().take(end).skip(begin) {
        let mark = if *addr == entries[pos].0 { "=>" } else { "  " };
        eprintln!("P7_ROOTS_CLEAN_WINDOW {label} {mark} {text}");
    }
}

#[test]
fn p7_roots_previous_link_malformed_refusal_interpreter() {
    let plugin: &'static RootSeam = Box::leak(Box::new(RootSeam {
        fault: RootSeamFault::PrevLink,
        helpers: std::sync::Mutex::new(Vec::new()),
        sorted_impl_keys: std::sync::Mutex::new(Vec::new()),
        carrier: std::sync::Mutex::new(None),
        frame: std::sync::Mutex::new(None),
        entry: std::sync::Mutex::new(None),
        object: std::sync::Mutex::new(None),
    }));
    let tapped: &'static dyn vm::SourcePlugin = plugin;
    let (mut machine, _) =
        build_machine("var o={v:1};o.v===1;", MachineSource::Tapped(tapped), false);
    let status = machine.run();
    assert!(
        !machine.jit_config().enabled,
        "interpreter only on the refusal path"
    );
    assert_eq!(machine.aot_exec_count(), 0, "no AOT on the refusal path");
    assert_eq!(
        machine.jit_native_exec_count(),
        0,
        "no trace execution on the refusal path"
    );
    assert!(
        matches!(&status, Err(vm::VmError::HostError(message)) if message == "assertion failed"),
        "fractional previous link must fail at the first decode: {status:?}"
    );
    // Exact refusal origin: the failure ip must sit inside the actual numeric
    // decoder (the 17-bit slot decoder the collector binds the previous link
    // through), entered from the actual collector at the previous-link
    // binder callsite. A generic host error with no origin proof is not
    // accepted.
    let helpers = plugin.helpers.lock().unwrap();
    let sorted_keys = plugin.sorted_impl_keys.lock().unwrap();
    let ip = machine.ip();
    let frames = machine.execution_frames();
    let program = machine.program();
    assert!(
        frames.windows(2).all(|w| w[0].local_base < w[1].local_base),
        "outer-to-inner frame order: {frames:?}"
    );
    // The collection runs directly under the script root: the obsolete
    // intrinsic-root caller requirement is gone with the prefix seam.
    assert_eq!(
        frames.len(),
        3,
        "refusal stack is script root, collector, decoder: {frames:?}"
    );
    assert!(
        frames[0].prototype_id.is_none(),
        "outermost frame is the script root: {frames:?}"
    );
    let linked = prev_link_sorted_regions(program);
    let failing = linked
        .iter()
        .find(|region| (region.0 as usize) <= ip && ip < (region.1 as usize))
        .expect("refusal ip inside a linked region");
    assert_eq!(
        failing.2,
        frames[2].prototype_id.expect("decoder frame still active"),
        "refusal inside the innermost decoder frame"
    );
    assert_runtime_prototype(
        program,
        &linked,
        &helpers,
        "__compat_es5_decode_int",
        failing.2,
    );
    let failing_proto = &program.callable_prototypes[failing.2 as usize];
    let failing_frame = &frames[2];
    let caller_id = frames[1].prototype_id.expect("collector prototype");
    assert_runtime_prototype(
        program,
        &linked,
        &helpers,
        "__compat_es5_heap_collect",
        caller_id,
    );
    assert_emission_base(
        &helpers,
        &sorted_keys,
        &[
            ("__compat_es5_decode_int", failing.2),
            ("__compat_es5_heap_collect", caller_id),
        ],
    );
    // The continuation belongs to the entered decoder: its return ip decodes
    // to the actual previous-link binder callsite inside the collector.
    let return_ip = match &failing_frame.continuation {
        vm::vm::VmFrameContinuation::ResumeBytecode { return_ip } => *return_ip,
        other => panic!("the decoder frame must resume bytecode: {other:?}"),
    };
    let caller_region = linked
        .iter()
        .find(|region| region.2 == caller_id)
        .expect("collector linked region");
    assert!(
        (caller_region.0 as usize) <= return_ip && return_ip < (caller_region.1 as usize),
        "decoder return location stays inside the collector toward the prev binding"
    );
    assert_call_site(
        program,
        caller_region.0,
        return_ip,
        failing.2,
        failing_proto.arity,
        "PREV_LINK_BINDER",
    );
    tag_exact_conversion_site(program, failing.0, failing.1, ip, "DECODE_EXACT_INT");
    eprint_seam_frames(&machine, "MALFORMED");
    prev_link_dump_window(&program.code, program, ip, "failure");
    prev_link_dump_window(&program.code, program, return_ip, "binder-callsite");
    eprintln!(
        "P7_ROOTS_MALFORMED_ORIGIN failing={} caller={caller_id} ip={ip} return_ip={return_ip}",
        failing.2
    );
}

/// Bottom-guard controls: the same Number-domain fault semantics as the
/// read-only stdlib RootControl faults 1-3 (wrong capacity, wrong payload
/// extent, wrong permanent identity), injected at the shared post-source seam
/// after `o` is published and no node is pending. The previous-link cell of
/// the permanent frame stays zero so the bottom-boundary branch is actually
/// evaluated; every header cell except the one under test stays intact so an
/// earlier unrelated check cannot substitute. The expected origin is the
/// collector's own bottom guard: the refusal ip sits inside the collector
/// prototype with no decoder or marker frame above it, and the collector's
/// continuation decodes to the real script callsite that entered it.
fn run_prev_link_bottom_fault(fault: RootSeamFault, case: &'static str) {
    let plugin: &'static RootSeam = Box::leak(Box::new(RootSeam {
        fault,
        helpers: std::sync::Mutex::new(Vec::new()),
        sorted_impl_keys: std::sync::Mutex::new(Vec::new()),
        carrier: std::sync::Mutex::new(None),
        frame: std::sync::Mutex::new(None),
        entry: std::sync::Mutex::new(None),
        object: std::sync::Mutex::new(None),
    }));
    let tapped: &'static dyn vm::SourcePlugin = plugin;
    let (mut machine, _) =
        build_machine("var o={v:1};o.v===1;", MachineSource::Tapped(tapped), false);
    let status = machine.run();
    assert!(
        !machine.jit_config().enabled,
        "interpreter only on the {case} refusal"
    );
    assert_eq!(machine.aot_exec_count(), 0, "no AOT on the {case} refusal");
    assert_eq!(
        machine.jit_native_exec_count(),
        0,
        "no trace execution on the {case} refusal"
    );
    assert!(
        matches!(&status, Err(vm::VmError::HostError(message)) if message == "assertion failed"),
        "{case} must refuse at the bottom guard: {status:?}"
    );
    // Every corrupted value stays integral, so every decode succeeds and no
    // decoder or marker frame can be active: the innermost frame must be the
    // actual collector failing at its own loop-level bottom guard.
    let helpers = plugin.helpers.lock().unwrap();
    let sorted_keys = plugin.sorted_impl_keys.lock().unwrap();
    let ip = machine.ip();
    let frames = machine.execution_frames();
    let program = machine.program();
    assert!(
        frames.windows(2).all(|w| w[0].local_base < w[1].local_base),
        "outer-to-inner frame order: {frames:?}"
    );
    assert_eq!(
        frames.len(),
        2,
        "{case} refuses in the collector directly under the script root: {frames:?}"
    );
    assert!(
        frames[0].prototype_id.is_none(),
        "{case} outermost frame is the script root: {frames:?}"
    );
    // Every corrupted value stays integral, so every decode succeeds and no
    // decoder or marker frame can be active: the innermost frame is the
    // actual collector failing at its own loop-level bottom guard.
    let collector_id = frames[1].prototype_id.expect("collector prototype");
    let linked = prev_link_sorted_regions(program);
    let failing = linked
        .iter()
        .find(|region| (region.0 as usize) <= ip && ip < (region.1 as usize))
        .unwrap_or_else(|| panic!("{case}: refusal ip inside a linked region"));
    assert_eq!(
        failing.2, collector_id,
        "{case} refuses inside the collector's own bottom guard, not a nested decode or mark call"
    );
    assert_runtime_prototype(
        program,
        &linked,
        &helpers,
        "__compat_es5_heap_collect",
        collector_id,
    );
    assert_emission_base(
        &helpers,
        &sorted_keys,
        &[("__compat_es5_heap_collect", collector_id)],
    );
    // The fired guard is the actual assert instruction that ends exactly at
    // the failure ip, with its real debug mapping; no fixed ip or same-arity
    // guess is used.
    tag_guard_site(program, failing.0, ip, "COLLECTOR_BOTTOM");
    // The continuation belongs to the entered collector: its return ip
    // decodes to the actual script callsite in the root region that targeted
    // this collector prototype with its declared arity.
    let return_ip = match &frames[1].continuation {
        vm::vm::VmFrameContinuation::ResumeBytecode { return_ip } => *return_ip,
        other => panic!("{case}: the collector must resume bytecode: {other:?}"),
    };
    let root_region = program
        .function_regions
        .iter()
        .find(|region| region.prototype_id.is_none())
        .expect("script root region");
    assert!(
        (root_region.start_ip as usize) <= return_ip && return_ip < (root_region.end_ip as usize),
        "{case} collector return location stays inside the script root"
    );
    let collector_proto = &program.callable_prototypes[collector_id as usize];
    assert_call_site(
        program,
        root_region.start_ip,
        return_ip,
        collector_id,
        collector_proto.arity,
        "COLLECTOR_ENTRY",
    );
    eprint_seam_frames(&machine, case);
    eprintln!(
        "P7_ROOTS_BOTTOM_{case} ip={ip} failing={} frames={} return_ip={return_ip}",
        failing.2,
        frames.len()
    );
}

#[test]
fn p7_roots_previous_link_bottom_capacity_refusal_interpreter() {
    run_prev_link_bottom_fault(RootSeamFault::BottomCap, "CAPACITY");
}

#[test]
fn p7_roots_previous_link_bottom_extent_refusal_interpreter() {
    run_prev_link_bottom_fault(RootSeamFault::BottomExtent, "EXTENT");
}

#[test]
fn p7_roots_previous_link_bottom_identity_refusal_interpreter() {
    run_prev_link_bottom_fault(RootSeamFault::BottomIdentity, "IDENTITY");
}

/// Stale-generation control at the shared post-source seam: after the
/// unchanged fixture published `o`, the seam already validated pending zero,
/// the script-top/permanent chain, the published `o` entry and permanent
/// entry 0's live directory row. Only entry 0's generation cell
/// (`permanent + 6`) is raised to old-generation + 1 in the exact UInt32
/// domain; its category and slot cells, the directory row and every header
/// cell stay intact.
///
/// The bottom scan therefore passes (the permanent frame's previous link is
/// zero and the frame identity, capacity and extent are unchanged) and the
/// marker reaches its real directory identity comparison. The marker reads
/// the stored row state and generation, asserts the active state, then
/// asserts generation equality BEFORE it loads and checks the node kind, so
/// only that generation equality can refuse. The expected origin is the
/// actual marker and that generation assert: the failure ip sits inside the
/// marker prototype with its live slot/generation parameters read through the
/// real parameter slots and frame base, and the marker's own continuation
/// decodes to the actual collector callsite that entered it.
#[test]
fn p7_roots_previous_link_stale_generation_refusal_interpreter() {
    let plugin: &'static RootSeam = Box::leak(Box::new(RootSeam {
        fault: RootSeamFault::StaleGen,
        helpers: std::sync::Mutex::new(Vec::new()),
        sorted_impl_keys: std::sync::Mutex::new(Vec::new()),
        carrier: std::sync::Mutex::new(None),
        frame: std::sync::Mutex::new(None),
        entry: std::sync::Mutex::new(None),
        object: std::sync::Mutex::new(None),
    }));
    let tapped: &'static dyn vm::SourcePlugin = plugin;
    let (mut machine, _) =
        build_machine("var o={v:1};o.v===1;", MachineSource::Tapped(tapped), false);
    let status = machine.run();
    assert!(
        !machine.jit_config().enabled,
        "interpreter only on the stale refusal"
    );
    assert_eq!(machine.aot_exec_count(), 0, "no AOT on the stale refusal");
    assert_eq!(
        machine.jit_native_exec_count(),
        0,
        "no trace execution on the stale refusal"
    );
    assert!(
        matches!(&status, Err(vm::VmError::HostError(message)) if message == "assertion failed"),
        "stale root generation must refuse at the marker generation guard: {status:?}"
    );
    let helpers = plugin.helpers.lock().unwrap();
    let sorted_keys = plugin.sorted_impl_keys.lock().unwrap();
    let ip = machine.ip();
    let frames = machine.execution_frames();
    let program = machine.program();
    assert!(
        frames.windows(2).all(|w| w[0].local_base < w[1].local_base),
        "outer-to-inner frame order: {frames:?}"
    );
    assert_eq!(
        frames.len(),
        3,
        "stale refusal stack is script root, collector, marker: {frames:?}"
    );
    assert!(
        frames[0].prototype_id.is_none(),
        "outermost frame is the script root: {frames:?}"
    );
    let collector_id = frames[1].prototype_id.expect("collector prototype");
    let marker_id = frames[2].prototype_id.expect("marker prototype");
    let linked = prev_link_sorted_regions(program);
    let failing = linked
        .iter()
        .find(|region| (region.0 as usize) <= ip && ip < (region.1 as usize))
        .expect("refusal ip inside a linked region");
    assert_eq!(
        failing.2, marker_id,
        "stale generation refuses inside the innermost marker frame"
    );
    assert_runtime_prototype(program, &linked, &helpers, "__compat_es5_mark", marker_id);
    assert_runtime_prototype(
        program,
        &linked,
        &helpers,
        "__compat_es5_heap_collect",
        collector_id,
    );
    assert_emission_base(
        &helpers,
        &sorted_keys,
        &[
            ("__compat_es5_mark", marker_id),
            ("__compat_es5_heap_collect", collector_id),
        ],
    );
    // The fired guard is the actual assert instruction ending exactly at the
    // failure ip, with its real debug mapping; the marker's generation
    // equality is the first identity check after the active-state assert.
    tag_guard_site(program, failing.0, ip, "MARKER_GENERATION");
    // Live marker parameters read through the actual parameter slots and the
    // frame's own local base: the stale entry keeps a valid integral slot and
    // generation, and the object-like expectation proves the refusal came
    // from a real object root rather than a string or environment edge.
    let marker_frame = &frames[2];
    let marker_proto = &program.callable_prototypes[marker_id as usize];
    let live: Vec<Value> = marker_proto
        .parameter_slots
        .iter()
        .map(|slot| machine.locals()[marker_frame.local_base + usize::from(*slot)].clone())
        .collect();
    assert!(
        matches!(live.get(1), Some(Value::Int(slot)) if *slot >= 0),
        "the stale root keeps a valid slot: {live:?}"
    );
    assert!(
        matches!(live.get(2), Some(Value::Int(_))),
        "the stale generation stays integral through its decode: {live:?}"
    );
    assert_eq!(
        live.get(3),
        Some(&Value::Int(3)),
        "the stale edge is an actual object root: {live:?}"
    );
    // The marker's continuation belongs to the marker: its return ip decodes
    // to the actual mark callsite inside the collector prototype.
    let return_ip = match &marker_frame.continuation {
        vm::vm::VmFrameContinuation::ResumeBytecode { return_ip } => *return_ip,
        other => panic!("the marker frame must resume bytecode: {other:?}"),
    };
    let collector_region = linked
        .iter()
        .find(|region| region.2 == collector_id)
        .expect("collector linked region");
    assert!(
        (collector_region.0 as usize) <= return_ip && return_ip < (collector_region.1 as usize),
        "marker return location stays inside the collector toward the root loop"
    );
    assert_call_site(
        program,
        collector_region.0,
        return_ip,
        marker_id,
        marker_proto.arity,
        "ROOT_LOOP_MARK",
    );
    eprint_seam_frames(&machine, "STALE");
    eprintln!(
        "P7_ROOTS_STALE_GENERATION ip={ip} failing={} live={live:?} return_ip={return_ip}",
        failing.2
    );
}

/// Expected refusal selector for a decoder control: a strict Assert domain
/// guard resolved by `category_guard`, or the current helper's single exact
/// `__checked_int_from_number` conversion. The exact conversion is a
/// caller-side expectation only; `category_guard` never returns it because it
/// classifies Assert calls, and the host error alone is never sufficient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CategoryGuard {
    ExactConversion,
    LowerBound,
    UpperBound,
}

/// Linear decode with real operand lengths from a region start, so guard
/// classification never starts mid-operand.
fn prev_link_ops(code: &[u8], start: usize) -> Vec<(usize, vm::OpCode, Vec<u8>)> {
    let mut out = Vec::new();
    let mut addr = start;
    let mut guard = 0usize;
    while addr < code.len() && guard < 4_000_000 {
        guard += 1;
        let Ok(op) = vm::OpCode::try_from(code[addr]) else {
            break;
        };
        let mut operands = Vec::new();
        for i in 1..=op.operand_len() {
            operands.push(code.get(addr + i).copied().unwrap_or(0));
        }
        let next = addr + 1 + op.operand_len();
        out.push((addr, op, operands));
        if next <= addr {
            break;
        }
        addr = next;
    }
    out
}

/// Classify the fired decoder Assert by the instruction and constant that
/// produced its condition: the lower non-negative bound or the cell-budget
/// upper bound. The current exact Int conversion is checked separately.
fn category_guard(
    program: &vm::Program,
    region_start: usize,
    assert_start: usize,
) -> CategoryGuard {
    let ops = prev_link_ops(&program.code, region_start);
    let at = ops
        .iter()
        .position(|(addr, _, _)| *addr == assert_start)
        .expect("assert instruction starts at the failure boundary");
    let (_, prev, _) = ops.get(at.wrapping_sub(1)).expect("guard condition");
    match prev {
        vm::OpCode::Not => {
            let (_, cmp, _) = ops.get(at - 2).expect("lower-bound comparison");
            assert!(
                matches!(cmp, vm::OpCode::Clt),
                "lower-bound guard must compare with Clt, got {cmp:?}"
            );
            let (_, ldc, operand) = ops.get(at - 3).expect("lower-bound constant");
            assert!(
                matches!(ldc, vm::OpCode::Ldc),
                "lower-bound guard must load its constant"
            );
            let index = u16::from_le_bytes([operand[0], operand[1]]);
            match program.constants.get(index as usize) {
                Some(Value::Float(f)) if *f == 0.0 && !f.is_sign_negative() => {
                    CategoryGuard::LowerBound
                }
                other => panic!("lower-bound guard constant must be exact +0.0, got {other:?}"),
            }
        }
        vm::OpCode::Ceq => {
            let (_, ldc, operand) = ops.get(at - 2).expect("comparison constant");
            assert!(
                matches!(ldc, vm::OpCode::Ldc),
                "guarded comparison must load its constant"
            );
            let index = u16::from_le_bytes([operand[0], operand[1]]);
            match program.constants.get(index as usize) {
                Some(Value::Float(f)) if *f == 65536.0 => CategoryGuard::UpperBound,
                other => panic!(
                    "guarded constant must be the 65536 cell budget, got {other:?}"
                ),
            }
        }
        other => panic!("unexpected decoder guard opcode at {assert_start}: {other:?}"),
    }
}

fn leak_root_seam(fault: RootSeamFault) -> &'static RootSeam {
    Box::leak(Box::new(RootSeam {
        fault,
        helpers: std::sync::Mutex::new(Vec::new()),
        sorted_impl_keys: std::sync::Mutex::new(Vec::new()),
        carrier: std::sync::Mutex::new(None),
        frame: std::sync::Mutex::new(None),
        entry: std::sync::Mutex::new(None),
        object: std::sync::Mutex::new(None),
    }))
}

/// Zero-category controls: the sole category write is exact Float zero (or
/// raw Int zero), so the collection must run over the intact roots and halt
/// with the original boxed completion and one live backing owner.
fn run_category_zero_control(fault: RootSeamFault, case: &'static str) {
    eprintln!("P7_ROOTS_CATEGORY_BEGIN {case} fault={fault:?}");
    let plugin = leak_root_seam(fault);
    let tapped: &'static dyn vm::SourcePlugin = plugin;
    let (mut machine, _) =
        build_machine("var o={v:1};o.v===1;", MachineSource::Tapped(tapped), false);
    eprintln!("P7_ROOTS_CATEGORY_COMPILED {case}");
    let status = machine.run();
    eprintln!(
        "P7_ROOTS_CATEGORY_RESULT {case} status={status:?} ip={} fuel={:?}",
        machine.ip(),
        machine.get_fuel()
    );
    assert!(
        !machine.jit_config().enabled,
        "interpreter only on the {case} zero category"
    );
    assert_eq!(machine.aot_exec_count(), 0, "no AOT on the {case} zero");
    assert_eq!(
        machine.jit_native_exec_count(),
        0,
        "no trace execution on the {case} zero"
    );
    assert_eq!(
        status.unwrap_or_else(|error| panic!(
            "{case} zero category must halt: {error:?} ip={}",
            machine.ip()
        )),
        VmStatus::Halted
    );
    assert_eq!(
        machine.host_context().resource_count(),
        1,
        "{case} keeps one actual backing owner"
    );
    let backing = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("actual backing accounting");
    assert_eq!((backing.pending, backing.registered), (0, 1));
    assert!(
        plugin.carrier.lock().unwrap().is_some()
            && plugin.frame.lock().unwrap().is_some()
            && plugin.entry.lock().unwrap().is_some()
            && plugin.object.lock().unwrap().is_some(),
        "{case} post-source seam discovery bound carrier, frame, entry and object"
    );
    assert!(
        machine
            .capture_certification_stats()
            .is_some_and(|facts| facts.valid),
        "{case} keeps the certified capture facts"
    );
    let actual = machine.stack().to_vec();
    let last = actual.last().expect("script value");
    assert_eq!(field(last, "tag"), Value::Int(2), "{case} boxed completion");
    assert_eq!(
        field(last, "flag"),
        Value::Bool(true),
        "{case} boxed completion flag"
    );
    machine.reset_for_reuse().expect("reset");
    assert_eq!(
        machine.host_context().resource_count(),
        0,
        "{case} reset releases the real owner"
    );
    eprintln!(
        "P7_ROOTS_CATEGORY_ZERO {case} entry={:?} frame={:?} tail={actual:?}",
        plugin.entry.lock().unwrap(),
        plugin.frame.lock().unwrap()
    );
}

/// Non-zero category controls: the unchanged exact decoder runs under the
/// collector, and its own continuation decomposes to the category read in
/// the collector. The fired decoder site is classified: a strict Assert
/// domain guard with its real constant operand, or the single exact Int
/// conversion call for a fractional value; the generic HostError alone is
/// not accepted.
fn run_category_refusal(fault: RootSeamFault, case: &'static str, expect: CategoryGuard) {
    eprintln!("P7_ROOTS_CATEGORY_BEGIN {case} fault={fault:?} expect={expect:?}");
    let plugin = leak_root_seam(fault);
    let tapped: &'static dyn vm::SourcePlugin = plugin;
    let (mut machine, _) =
        build_machine("var o={v:1};o.v===1;", MachineSource::Tapped(tapped), false);
    eprintln!("P7_ROOTS_CATEGORY_COMPILED {case}");
    let status = machine.run();
    eprintln!(
        "P7_ROOTS_CATEGORY_RESULT {case} status={status:?} ip={} fuel={:?}",
        machine.ip(),
        machine.get_fuel()
    );
    assert!(
        matches!(&status, Err(vm::VmError::HostError(message)) if message == "assertion failed"),
        "{case} must refuse inside the exact decoder: {status:?}"
    );
    assert!(
        !machine.jit_config().enabled,
        "interpreter only on the {case} refusal"
    );
    assert_eq!(machine.aot_exec_count(), 0, "no AOT on the {case} refusal");
    assert_eq!(
        machine.jit_native_exec_count(),
        0,
        "no trace execution on the {case} refusal"
    );
    let helpers = plugin.helpers.lock().unwrap();
    let sorted_keys = plugin.sorted_impl_keys.lock().unwrap();
    let ip = machine.ip();
    let frames = machine.execution_frames();
    let program = machine.program();
    assert!(
        frames.windows(2).all(|w| w[0].local_base < w[1].local_base),
        "{case} outer-to-inner frame order: {frames:?}"
    );
    assert_eq!(
        frames.len(),
        3,
        "{case} refusal stack is script root, collector, decoder: {frames:?}"
    );
    assert!(
        frames[0].prototype_id.is_none(),
        "{case} outermost frame is the script root: {frames:?}"
    );
    let collector_id = frames[1].prototype_id.expect("collector prototype");
    let decoder_id = frames[2].prototype_id.expect("decoder prototype");
    let linked = prev_link_sorted_regions(program);
    let failing = linked
        .iter()
        .find(|region| (region.0 as usize) <= ip && ip < (region.1 as usize))
        .unwrap_or_else(|| panic!("{case} refusal ip inside a linked region"));
    assert_eq!(
        failing.2, decoder_id,
        "{case} refuses inside the actual category decoder, not the collector or marker"
    );
    assert_runtime_prototype(
        program,
        &linked,
        &helpers,
        "__compat_es5_decode_int",
        decoder_id,
    );
    assert_runtime_prototype(
        program,
        &linked,
        &helpers,
        "__compat_es5_heap_collect",
        collector_id,
    );
    assert_emission_base(
        &helpers,
        &sorted_keys,
        &[
            ("__compat_es5_decode_int", decoder_id),
            ("__compat_es5_heap_collect", collector_id),
        ],
    );
    let decoder_proto = &program.callable_prototypes[decoder_id as usize];
    let return_ip = match &frames[2].continuation {
        vm::vm::VmFrameContinuation::ResumeBytecode { return_ip } => *return_ip,
        other => panic!("{case}: the category decoder must resume bytecode: {other:?}"),
    };
    let collector_region = linked
        .iter()
        .find(|region| region.2 == collector_id)
        .expect("collector linked region");
    assert!(
        (collector_region.0 as usize) <= return_ip && return_ip < (collector_region.1 as usize),
        "{case} decoder returns toward the category read inside the collector"
    );
    assert_call_site(
        program,
        collector_region.0,
        return_ip,
        decoder_id,
        decoder_proto.arity,
        "CATEGORY_READ",
    );
    let guard = match expect {
        CategoryGuard::ExactConversion => {
            tag_exact_conversion_site(program, failing.0, failing.1, ip, case);
            CategoryGuard::ExactConversion
        }
        expect_guard => {
            let assert_start = tag_guard_site(program, failing.0, ip, case);
            let guard = category_guard(program, failing.0 as usize, assert_start);
            assert_eq!(
                guard, expect_guard,
                "{case} must refuse at its own decoder guard, got {guard:?}"
            );
            guard
        }
    };
    prev_link_dump_window(&program.code, program, ip, "category-failure");
    prev_link_dump_window(&program.code, program, return_ip, "category-callsite");
    eprint_seam_frames(&machine, case);
    eprintln!(
        "P7_ROOTS_CATEGORY_REFUSAL {case} decoder={decoder_id} collector={collector_id} \
         ip={ip} return_ip={return_ip} guard={guard:?} entry={:?}",
        plugin.entry.lock().unwrap()
    );
}

#[test]
fn p7_roots_category_float_pos_zero_interpreter() {
    run_category_zero_control(RootSeamFault::CategoryFloatPosZero, "CATEGORY_POS_ZERO");
}

#[test]
fn p7_roots_category_float_neg_zero_interpreter() {
    run_category_zero_control(RootSeamFault::CategoryFloatNegZero, "CATEGORY_NEG_ZERO");
}

#[test]
fn p7_roots_category_raw_int_zero_interpreter() {
    run_category_zero_control(RootSeamFault::CategoryRawIntZero, "CATEGORY_RAW_INT_ZERO");
}

#[test]
fn p7_roots_category_fraction_refusal_interpreter() {
    run_category_refusal(
        RootSeamFault::CategoryFractional,
        "CATEGORY_FRACTION",
        CategoryGuard::ExactConversion,
    );
}

#[test]
fn p7_roots_category_negative_refusal_interpreter() {
    run_category_refusal(
        RootSeamFault::CategoryNegative,
        "CATEGORY_NEGATIVE",
        CategoryGuard::LowerBound,
    );
}

#[test]
fn p7_roots_category_nan_refusal_interpreter() {
    run_category_refusal(
        RootSeamFault::CategoryNaN,
        "CATEGORY_NAN",
        CategoryGuard::UpperBound,
    );
}

#[test]
fn p7_roots_category_pos_inf_refusal_interpreter() {
    run_category_refusal(
        RootSeamFault::CategoryPosInf,
        "CATEGORY_POS_INF",
        CategoryGuard::UpperBound,
    );
}

#[test]
fn p7_roots_category_neg_inf_refusal_interpreter() {
    run_category_refusal(
        RootSeamFault::CategoryNegInf,
        "CATEGORY_NEG_INF",
        CategoryGuard::LowerBound,
    );
}

/// Structural contract for the collector's phase-1 active-state fast path.
///
/// The certified current IR must keep exactly one `ROW_STATE` read bound into
/// the pre-existing Number scratch `rdn`, then bind the Int state slot through
/// `IfElse(Eq(<normalized scratch>, Float(2.0)), Int(2), decode_int(<rdn>))`.
/// The adjacent `<= STATE_RETIRED` domain guard, the `st == STATE_ACTIVE`
/// clear of mark and gray-link, and the row/page/scanned-slot cursors stay
/// unchanged. No guest execution: GREEN constructs a VM but never runs it.
/// An unconditional decode fails this contract with a message that shows the
/// actual slot and expression.
#[test]
fn p7_phase1_active_state_fastpath_program_structure() {
    struct Tap;
    impl vm::SourcePlugin for Tap {
        fn descriptor(&self) -> vm::SourceFrontendDescriptor {
            vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN)
        }
        fn parse_source(
            &self,
            source: &str,
            context: &vm::SourceFrontendContext<'_>,
        ) -> Result<vm::FrontendIr, vm::ParseError> {
            use vm::{Expr as E, Stmt as S, TypeSchema as T};
            fn plain_call(expr: &E, target: u16, arity: usize) -> Option<&[E]> {
                match expr {
                    E::Call(call, _, args, _, _) if *call == target && args.len() == arity => {
                        Some(args)
                    }
                    _ => None,
                }
            }
            fn decode_arg(expr: &E, decode: u16) -> Option<&E> {
                plain_call(expr, decode, 1).map(|args| &args[0])
            }
            /// The certified frontend projects a Number operand through its
            /// pre-existing divide-by-one shape; both spellings name the same
            /// already-read scratch cell.
            fn scratch_slot(expr: &E) -> Option<u16> {
                match expr {
                    E::Var(slot) => Some(*slot),
                    E::Div(inner, unit) => {
                        if !matches!(unit.as_ref(), E::Float(unit) if *unit == 1.0) {
                            return None;
                        }
                        match inner.as_ref() {
                            E::Var(slot) => Some(*slot),
                            _ => None,
                        }
                    }
                    _ => None,
                }
            }
            fn is_zero_payload(expr: &E) -> bool {
                match expr {
                    E::Int(0) => true,
                    E::Div(inner, unit) => {
                        matches!(inner.as_ref(), E::Int(0))
                            && matches!(unit.as_ref(), E::Float(unit) if *unit == 1.0)
                    }
                    _ => false,
                }
            }
            /// Undo the single Number projection so an address keeps its
            /// actual `scanned local + cell offset` shape. A folded zero
            /// offset still names the row's own state cell (ROW_STATE=0).
            fn address(expr: &E) -> Option<(u16, i64)> {
                let inner = match expr {
                    E::Div(inner, unit) if matches!(unit.as_ref(), E::Float(unit) if *unit == 1.0) => {
                        inner.as_ref()
                    }
                    other => other,
                };
                if let E::Var(base) = inner {
                    return Some((*base, 0));
                }
                let E::Add(base, offset) = inner else {
                    return None;
                };
                let E::Var(base) = base.as_ref() else {
                    return None;
                };
                let E::Int(offset) = offset.as_ref() else {
                    return None;
                };
                Some((*base, *offset))
            }
            fn read_target(expr: &E, read: u16, carrier: u16) -> Option<(u16, i64)> {
                let args = plain_call(expr, read, 2)?;
                if !matches!(&args[0], E::Var(c) if *c == carrier) {
                    return None;
                }
                address(&args[1])
            }
            fn write_target(expr: &E, write: u16, carrier: u16) -> Option<(u16, i64, &E)> {
                let args = plain_call(expr, write, 3)?;
                if !matches!(&args[0], E::Var(c) if *c == carrier) {
                    return None;
                }
                let (base, offset) = address(&args[1])?;
                Some((base, offset, &args[2]))
            }
            let ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
            let helper = |name: &str| {
                ir.functions
                    .iter()
                    .find(|function| function.name == name)
                    .unwrap_or_else(|| panic!("missing actual helper {name}"))
                    .index
            };
            let collect = helper("__compat_es5_heap_collect");
            let decode = helper("__compat_es5_decode_int");
            let read = helper("__compat_ref_read");
            let write = helper("__compat_ref_write");
            let carrier = ir.function_impls[&collect].param_slots[0];
            let decode_returns_int = ir
                .functions
                .iter()
                .any(|function| function.index == decode && function.return_schema == Some(T::Int));
            assert!(
                decode_returns_int,
                "the original slot decoder keeps its Int result"
            );
            let body = &ir.function_impls[&collect].body_stmts;
            let mut lists: Vec<(&[S], usize)> = Vec::new();
            fn collect_lists<'a>(body: &'a [S], parent: usize, out: &mut Vec<(&'a [S], usize)>) {
                let index = out.len();
                out.push((body, parent));
                for stmt in body {
                    match stmt {
                        S::While { body, .. } => collect_lists(body, index, out),
                        S::IfElse {
                            then_branch,
                            else_branch,
                            ..
                        } => {
                            collect_lists(then_branch, index, out);
                            collect_lists(else_branch, index, out);
                        }
                        S::For { body, .. } => collect_lists(body, index, out),
                        _ => {}
                    }
                }
            }
            collect_lists(body, usize::MAX, &mut lists);
            // The phase-1 state domain guard is the collector's own
            // `assert(st < STATE_RETIRED || st == STATE_RETIRED)`.
            let mut domains: Vec<(usize, usize, u16, i64)> = Vec::new();
            for (list_index, (list, _)) in lists.iter().enumerate() {
                for (pos, stmt) in list.iter().enumerate() {
                    let S::Expr {
                        expr: E::Call(assert, _, args, _, _),
                        ..
                    } = stmt
                    else {
                        continue;
                    };
                    if *assert != vm::BuiltinFunction::Assert.call_index() || args.len() != 1 {
                        continue;
                    }
                    let E::Or(lower, upper) = &args[0] else {
                        continue;
                    };
                    let (E::Lt(left, left_bound), E::Eq(right, right_bound)) =
                        (lower.as_ref(), upper.as_ref())
                    else {
                        continue;
                    };
                    let (E::Var(left_slot), E::Int(bound)) = (left.as_ref(), left_bound.as_ref())
                    else {
                        continue;
                    };
                    let (E::Var(right_slot), E::Int(same_bound)) =
                        (right.as_ref(), right_bound.as_ref())
                    else {
                        continue;
                    };
                    if left_slot == right_slot && bound == same_bound && *bound == 4 {
                        domains.push((list_index, pos, *left_slot, *bound));
                    }
                }
            }
            assert_eq!(
                domains.len(),
                1,
                "exactly one row-state domain guard in the collector, got {domains:?}"
            );
            let (list_index, domain_at, st, retired) = domains[0];
            assert_eq!(
                retired, 4,
                "the row-state domain bound is STATE_RETIRED=4, got {retired}"
            );
            assert!(
                list_index != 0,
                "the phase-1 state scan keeps its own loop body"
            );
            let list = lists[list_index].0;
            let producer_at = domain_at.checked_sub(1).unwrap_or_else(|| {
                panic!("the row-state producer precedes the domain guard at {domain_at}")
            });
            let S::Assign {
                index: st_slot,
                expr: producer,
                ..
            } = &list[producer_at]
            else {
                panic!(
                    "the row-state producer immediately precedes the domain guard, got {:?}",
                    &list[producer_at]
                )
            };
            assert_eq!(
                *st_slot, st,
                "the producer writes the state slot the domain guard checks"
            );
            let E::IfElse {
                condition,
                then_expr,
                else_expr,
            } = producer
            else {
                panic!(
                    "phase-1 ROW_STATE must be the exact active-state conditional \
                     IfElse(Eq(<normalized rdn>, 2.0), Int(2), decode_int(<rdn>)); slot {st} \
                     still holds the unconditional shape {producer:?}"
                )
            };
            let E::Eq(condition_left, condition_right) = condition.as_ref() else {
                panic!(
                    "the active-state condition compares the state scratch with the exact \
                     active value, got {condition:?}"
                )
            };
            assert!(
                matches!(condition_right.as_ref(), E::Float(value) if *value == 2.0),
                "the active-state condition compares Float(2.0) (STATE_ACTIVE), got \
                 {condition_right:?}"
            );
            assert!(
                matches!(then_expr.as_ref(), E::Int(2)),
                "the exact active branch yields Int(2), got {then_expr:?}"
            );
            let Some(fallback) = decode_arg(else_expr, decode) else {
                panic!("the non-active fallback keeps the original decoder call, got {else_expr:?}")
            };
            let rdn = scratch_slot(condition_left)
                .filter(|slot| scratch_slot(fallback) == Some(*slot))
                .unwrap_or_else(|| {
                    panic!(
                        "the active-state comparison and the decoder fallback must name one \
                         existing Number scratch, got {condition_left:?} and {fallback:?}"
                    )
                });
            let scratch_at = producer_at
                .checked_sub(1)
                .unwrap_or_else(|| panic!("the single state read precedes the state assignment"));
            let S::Assign {
                index: read_slot,
                expr: read_expr,
                ..
            } = &list[scratch_at]
            else {
                panic!(
                    "the state read binds the Number scratch immediately before the state \
                     assignment, got {:?}",
                    &list[scratch_at]
                )
            };
            assert_eq!(
                *read_slot, rdn,
                "the state read binds the existing Number scratch {rdn}"
            );
            let Some((row_addr, state_offset)) = read_target(read_expr, read, carrier) else {
                panic!(
                    "the single row-state read keeps the actual row-cell address shape, got \
                     {read_expr:?}"
                )
            };
            let declared = |slot: u16, schema: &T| {
                lists.iter().any(|(list, _)| {
                    list.iter().any(|stmt| {
                        matches!(
                            stmt,
                            S::Let {
                                index,
                                declared_schema: Some(declared),
                                ..
                            } if *index == slot && declared == schema
                        )
                    })
                })
            };
            assert!(
                declared(rdn, &T::Number),
                "the state scratch {rdn} stays the pre-existing Number local"
            );
            assert!(
                declared(st, &T::Int),
                "the state slot {st} stays the Int local"
            );
            // Exactly one row-state read inside the phase-1 loop body. The
            // later free-slot sweep keeps the original decoder for the
            // non-Float2 fallback and is outside this contract.
            let phase1_reads = list
                .iter()
                .filter(|stmt| {
                    matches!(
                        stmt,
                        S::Assign { expr, .. }
                            if read_target(expr, read, carrier) == Some((row_addr, state_offset))
                    )
                })
                .count();
            assert_eq!(
                phase1_reads, 1,
                "exactly one phase-1 row-state read at address slot {row_addr} + \
                 {state_offset}, got {phase1_reads}"
            );
            let active_at = domain_at + 1;
            let S::IfElse {
                condition,
                then_branch,
                else_branch,
                ..
            } = &list[active_at]
            else {
                panic!(
                    "the active-state clear follows the domain guard, got {:?}",
                    list.get(active_at)
                )
            };
            assert!(
                matches!(
                    condition,
                    E::Eq(left, right)
                        if matches!(left.as_ref(), E::Var(slot) if *slot == st)
                            && matches!(right.as_ref(), E::Int(2))
                ),
                "the clear branch still tests st == STATE_ACTIVE(2), got {condition:?}"
            );
            assert!(
                else_branch.is_empty(),
                "the active clear keeps no else branch"
            );
            assert_eq!(
                then_branch.len(),
                2,
                "the active clear keeps exactly mark and gray-link resets"
            );
            let mut cleared: Vec<i64> = Vec::new();
            for stmt in then_branch {
                let S::Assign { expr, .. } = stmt else {
                    panic!("the active clear changed shape: {stmt:?}")
                };
                let Some((base, offset, payload)) = write_target(expr, write, carrier) else {
                    panic!("the active clear keeps raw row writes through B1, got {expr:?}")
                };
                assert_eq!(base, row_addr, "the active clear writes the scanned row");
                assert!(
                    is_zero_payload(payload),
                    "the active clear writes zero, got {payload:?}"
                );
                cleared.push(offset);
            }
            assert!(
                cleared.len() == 2 && cleared[0] != cleared[1],
                "the active clear resets the mark and gray-link cells, got {cleared:?}"
            );
            // The cursors live in the enclosing scan-loop body, one statement
            // group after the whole state branch.
            let parent_index = lists[list_index].1;
            assert!(
                parent_index != usize::MAX,
                "the state branch is not the function body"
            );
            let enclosing = lists[parent_index].0;
            let enclosing_at = enclosing
                .iter()
                .position(|stmt| {
                    matches!(
                        stmt,
                        S::IfElse { then_branch, .. }
                            if std::ptr::eq(then_branch.as_slice(), list)
                    )
                })
                .unwrap_or_else(|| panic!("the phase-1 state branch sits in its scan loop body"));
            assert_eq!(
                active_at + 1,
                list.len(),
                "the active clear closes the phase-1 state branch"
            );
            let row_at = enclosing_at + 1;
            let row_step = match enclosing.get(row_at) {
                Some(S::Assign {
                    index,
                    expr: E::Add(left, step),
                    ..
                }) if matches!(left.as_ref(), E::Var(slot) if *slot == *index)
                    && matches!(step.as_ref(), E::Int(1)) =>
                {
                    Some(*index)
                }
                other => panic!("the row cursor still increments once, got {other:?}"),
            };
            let wrap = match enclosing.get(row_at + 1) {
                Some(S::IfElse {
                    condition,
                    then_branch,
                    else_branch,
                    ..
                }) => (condition, then_branch, else_branch),
                other => panic!("the page wrap follows the row cursor, got {other:?}"),
            };
            assert!(
                matches!(
                    wrap.0,
                    E::Eq(left, right)
                        if matches!(left.as_ref(), E::Var(slot) if Some(*slot) == row_step)
                            && matches!(right.as_ref(), E::Int(rows) if *rows > 0)
                ),
                "the page wrap still tests the row cursor against PAGE_ROWS, got {:?}",
                wrap.0
            );
            assert_eq!(
                wrap.1.len(),
                2,
                "the page wrap resets the row and advances the page"
            );
            assert!(wrap.2.is_empty(), "the page wrap has no else branch");
            let slot_at = row_at + 2;
            let slot_step = match enclosing.get(slot_at) {
                Some(S::Assign {
                    index,
                    expr: E::Add(left, step),
                    ..
                }) if matches!(left.as_ref(), E::Var(slot) if *slot == *index)
                    && matches!(step.as_ref(), E::Int(1)) =>
                {
                    Some(*index)
                }
                other => panic!("the scanned-slot counter still increments once, got {other:?}"),
            };
            assert!(
                slot_step.is_some() && slot_step != row_step,
                "the scan advances a distinct slot counter, got {slot_step:?}"
            );
            eprintln!(
                "P7_PHASE1_ACTIVE_STATE_STRUCTURE carrier={carrier} st={st} rdn={rdn} \
                 retired={retired} row_addr={row_addr} state_cell={state_offset} \
                 cleared={cleared:?} row_step={row_step:?} slot_step={slot_step:?} lists={}",
                lists.len()
            );
            Ok(ir)
        }
    }
    let plugin: &'static dyn vm::SourcePlugin = Box::leak(Box::new(Tap));
    let (_machine, _) = build_machine("var o={v:1};o.v===1;", MachineSource::Tapped(plugin), false);
}

/// Structural contract for the marker's own active-state fast path.
///
/// The certified current IR must keep exactly one ROW_STATE read bound into
/// the pre-existing Number `field_num`, then bind the Int state slot through
/// `IfElse(Eq(<normalized field_num>, Float(2.0)), Int(2), decode_int(<field_num>))`.
/// The generation read keeps the unsigned generation decoder and its equality
/// check against the marker's generation parameter, and the physical-base,
/// kind and mark sequence stays unchanged. No guest execution: GREEN constructs
/// a VM but never runs it. The current unconditional decoder fails this
/// contract with the actual slot and expression.
#[test]
fn p7_marker_active_state_fastpath_program_structure() {
    struct Tap;
    impl vm::SourcePlugin for Tap {
        fn descriptor(&self) -> vm::SourceFrontendDescriptor {
            vm::SourcePlugin::descriptor(&JAVASCRIPT_ES5_PLUGIN)
        }
        fn parse_source(
            &self,
            source: &str,
            context: &vm::SourceFrontendContext<'_>,
        ) -> Result<vm::FrontendIr, vm::ParseError> {
            use vm::{Expr as E, Stmt as S, TypeSchema as T};
            fn plain_call(expr: &E, target: u16, arity: usize) -> Option<&[E]> {
                match expr {
                    E::Call(call, _, args, _, _) if *call == target && args.len() == arity => {
                        Some(args)
                    }
                    _ => None,
                }
            }
            fn decode_arg(expr: &E, decode: u16) -> Option<&E> {
                plain_call(expr, decode, 1).map(|args| &args[0])
            }
            /// The certified frontend projects a Number operand through its
            /// pre-existing divide-by-one shape; both spellings name the same
            /// already-read scratch cell.
            fn scratch_slot(expr: &E) -> Option<u16> {
                match expr {
                    E::Var(slot) => Some(*slot),
                    E::Div(inner, unit) => {
                        if !matches!(unit.as_ref(), E::Float(unit) if *unit == 1.0) {
                            return None;
                        }
                        match inner.as_ref() {
                            E::Var(slot) => Some(*slot),
                            _ => None,
                        }
                    }
                    _ => None,
                }
            }
            fn address(expr: &E) -> Option<(u16, i64)> {
                let inner = match expr {
                    E::Div(inner, unit) if matches!(unit.as_ref(), E::Float(unit) if *unit == 1.0) => {
                        inner.as_ref()
                    }
                    other => other,
                };
                if let E::Var(base) = inner {
                    return Some((*base, 0));
                }
                let E::Add(base, offset) = inner else {
                    return None;
                };
                let E::Var(base) = base.as_ref() else {
                    return None;
                };
                let E::Int(offset) = offset.as_ref() else {
                    return None;
                };
                Some((*base, *offset))
            }
            fn read_target(expr: &E, read: u16, carrier: u16) -> Option<(u16, i64)> {
                let args = plain_call(expr, read, 2)?;
                if !matches!(&args[0], E::Var(c) if *c == carrier) {
                    return None;
                }
                address(&args[1])
            }
            fn write_target(expr: &E, write: u16, carrier: u16) -> Option<(u16, i64, &E)> {
                let args = plain_call(expr, write, 3)?;
                if !matches!(&args[0], E::Var(c) if *c == carrier) {
                    return None;
                }
                let (base, offset) = address(&args[1])?;
                Some((base, offset, &args[2]))
            }
            /// A direct header cell keeps its folded Int spelling, optionally
            /// behind the same divide-by-one projection.
            fn header_cell(expr: &E) -> Option<i64> {
                match expr {
                    E::Int(cell) => Some(*cell),
                    E::Div(inner, unit) if matches!(unit.as_ref(), E::Float(unit) if *unit == 1.0) => {
                        match inner.as_ref() {
                            E::Int(cell) => Some(*cell),
                            _ => None,
                        }
                    }
                    _ => None,
                }
            }
            /// A stored Int literal keeps its spelling, optionally behind the
            /// same divide-by-one Number projection.
            fn int_payload(expr: &E, value: i64) -> bool {
                match expr {
                    E::Int(stored) => *stored == value,
                    E::Div(inner, unit) if matches!(unit.as_ref(), E::Float(unit) if *unit == 1.0) =>
                    {
                        matches!(inner.as_ref(), E::Int(stored) if *stored == value)
                    }
                    _ => false,
                }
            }
            fn uses_var(expr: &E, slot: u16) -> bool {
                match expr {
                    E::Var(var) => *var == slot,
                    E::Call(_, _, args, _, _) | E::LocalCall(_, _, args, _) => {
                        args.iter().any(|arg| uses_var(arg, slot))
                    }
                    E::Add(l, r)
                    | E::Sub(l, r)
                    | E::Mul(l, r)
                    | E::Div(l, r)
                    | E::Mod(l, r)
                    | E::Eq(l, r)
                    | E::Lt(l, r)
                    | E::Gt(l, r)
                    | E::And(l, r)
                    | E::Or(l, r) => uses_var(l, slot) || uses_var(r, slot),
                    E::Neg(e) | E::Not(e) => uses_var(e, slot),
                    _ => false,
                }
            }
            /// Count direct B1 reads of header cell 4 (`HDR_NEXT_SLOT`) in
            /// one expression tree, including conditional arms.
            fn next_slot_reads(expr: &E, read: u16) -> usize {
                let own = usize::from(matches!(
                    expr,
                    E::Call(call, _, args, _, _)
                        if *call == read
                            && args.len() == 2
                            && header_cell(&args[1]) == Some(4)
                ));
                own + match expr {
                    E::Call(_, _, args, _, _) | E::LocalCall(_, _, args, _) => {
                        args.iter().map(|arg| next_slot_reads(arg, read)).sum()
                    }
                    E::Add(l, r)
                    | E::Sub(l, r)
                    | E::Mul(l, r)
                    | E::Div(l, r)
                    | E::Mod(l, r)
                    | E::Eq(l, r)
                    | E::Lt(l, r)
                    | E::Gt(l, r)
                    | E::And(l, r)
                    | E::Or(l, r) => next_slot_reads(l, read) + next_slot_reads(r, read),
                    E::Neg(e) | E::Not(e) => next_slot_reads(e, read),
                    E::IfElse {
                        condition,
                        then_expr,
                        else_expr,
                    } => {
                        next_slot_reads(condition, read)
                            + next_slot_reads(then_expr, read)
                            + next_slot_reads(else_expr, read)
                    }
                    _ => 0,
                }
            }
            /// Count `HDR_NEXT_SLOT` rereads across one statement list and its
            /// nested branches.
            fn next_slot_reads_in(stmts: &[S], read: u16) -> usize {
                stmts
                    .iter()
                    .map(|stmt| match stmt {
                        S::Let { expr, .. } | S::Assign { expr, .. } | S::Expr { expr, .. } => {
                            next_slot_reads(expr, read)
                        }
                        S::IfElse {
                            condition,
                            then_branch,
                            else_branch,
                            ..
                        } => {
                            next_slot_reads(condition, read)
                                + next_slot_reads_in(then_branch, read)
                                + next_slot_reads_in(else_branch, read)
                        }
                        _ => 0,
                    })
                    .sum()
            }
            let ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
            let helper = |name: &str| {
                ir.functions
                    .iter()
                    .find(|function| function.name == name)
                    .unwrap_or_else(|| panic!("missing actual helper {name}"))
                    .index
            };
            let read = helper("__compat_ref_read");
            let write = helper("__compat_ref_write");
            let decode = helper("__compat_es5_decode_int");
            let decode_u32 = helper("__compat_es5_decode_u32");
            let mark = helper("__compat_es5_mark");
            let mark_fn = ir
                .functions
                .iter()
                .find(|function| function.index == mark)
                .expect("the actual marker helper");
            assert_eq!(
                mark_fn.arity, 5,
                "the marker takes the next-only five-argument contract"
            );
            assert_eq!(
                mark_fn.arg_schemas.len(),
                5,
                "the marker declaration keeps five argument schemas"
            );
            assert_eq!(
                mark_fn.arg_schemas.get(4),
                Some(&Some(T::Int)),
                "the marker fifth argument is the Int next slot"
            );
            let impl_body = &ir.function_impls[&mark];
            let params = impl_body.param_slots.clone();
            assert_eq!(
                params.len(),
                5,
                "the marker implementation keeps five parameter slots"
            );
            let carrier = params[0];
            let next = params[4];
            let body = &impl_body.body_stmts;
            assert_eq!(
                next_slot_reads_in(body, read) + next_slot_reads(&impl_body.body_expr, read),
                0,
                "the marker takes HDR_NEXT_SLOT from its fifth parameter and never rereads header cell 4"
            );
            let mut next_bounds = 0usize;
            for stmt in body {
                let S::Expr {
                    expr: E::Call(assert, _, args, _, _),
                    ..
                } = stmt
                else {
                    continue;
                };
                if *assert != vm::BuiltinFunction::Assert.call_index() || args.len() != 1 {
                    continue;
                }
                let E::And(lower, upper) = &args[0] else {
                    continue;
                };
                let E::Not(lower) = lower.as_ref() else {
                    continue;
                };
                let E::Lt(low_left, low_right) = lower.as_ref() else {
                    continue;
                };
                let E::Lt(high_left, high_right) = upper.as_ref() else {
                    continue;
                };
                if matches!(low_left.as_ref(), E::Var(slot) if slot == &params[1])
                    && matches!(low_right.as_ref(), E::Int(0))
                    && matches!(high_left.as_ref(), E::Var(slot) if slot == &params[1])
                    && matches!(high_right.as_ref(), E::Var(slot) if slot == &next)
                {
                    next_bounds += 1;
                }
            }
            assert_eq!(
                next_bounds, 1,
                "the marker slot bound tests the fifth next parameter, got {next_bounds}"
            );
            // Two generation bindings follow the state producer before the
            // unique `assert(state == STATE_ACTIVE(2))`.
            let mut state_producers: Vec<(usize, u16)> = Vec::new();
            for (pos, stmt) in body.iter().enumerate() {
                let S::Let {
                    index: state,
                    declared_schema: Some(T::Int),
                    ..
                } = stmt
                else {
                    continue;
                };
                let Some(S::Expr {
                    expr: E::Call(assert, _, args, _, _),
                    ..
                }) = body.get(pos + 3)
                else {
                    continue;
                };
                if *assert != vm::BuiltinFunction::Assert.call_index() || args.len() != 1 {
                    continue;
                }
                if matches!(
                    &args[0],
                    E::Eq(left, right)
                        if matches!(left.as_ref(), E::Var(slot) if slot == state)
                            && matches!(right.as_ref(), E::Int(2))
                ) {
                    state_producers.push((pos, *state));
                }
            }
            assert_eq!(
                state_producers.len(),
                1,
                "exactly one marker state producer followed by state == STATE_ACTIVE(2), got \
                 {state_producers:?}"
            );
            let (producer_at, st) = state_producers[0];
            let Some(S::Let {
                index: field_num,
                declared_schema: Some(T::Number),
                expr: read_expr,
                ..
            }) = producer_at.checked_sub(1).and_then(|at| body.get(at))
            else {
                panic!(
                    "the marker row-state read is the Number binding immediately before slot {st}"
                )
            };
            let Some((row_addr, state_cell)) = read_target(read_expr, read, carrier) else {
                panic!(
                    "the marker row-state read keeps the actual row-cell address shape, got \
                     {read_expr:?}"
                )
            };
            assert_eq!(
                state_cell, 0,
                "the marker state read targets the row's own ROW_STATE cell, got {state_cell}"
            );
            let S::Let {
                expr: slot_expr, ..
            } = &body[producer_at]
            else {
                unreachable!("state producer statement");
            };
            let E::IfElse {
                condition,
                then_expr,
                else_expr,
            } = slot_expr
            else {
                panic!(
                    "the marker ROW_STATE producer must be the exact active-state conditional \
                     IfElse(Eq(<normalized field_num>, 2.0), Int(2), decode_int(<field_num>)); \
                     slot {st} still holds the unconditional shape {slot_expr:?}"
                )
            };
            let E::Eq(condition_left, condition_right) = condition.as_ref() else {
                panic!(
                    "the marker active-state condition compares the state scratch with the exact \
                     active value, got {condition:?}"
                )
            };
            assert!(
                matches!(condition_right.as_ref(), E::Float(value) if *value == 2.0),
                "the marker active-state condition compares Float(2.0) (STATE_ACTIVE), got \
                 {condition_right:?}"
            );
            assert!(
                matches!(then_expr.as_ref(), E::Int(2)),
                "the exact active branch yields Int(2), got {then_expr:?}"
            );
            let Some(fallback) = decode_arg(else_expr, decode) else {
                panic!("the non-active fallback keeps the original decoder call, got {else_expr:?}")
            };
            assert_eq!(
                scratch_slot(condition_left),
                Some(*field_num),
                "the active-state comparison reads the existing Number scratch {field_num}, got \
                 {condition_left:?}"
            );
            assert_eq!(
                scratch_slot(fallback),
                Some(*field_num),
                "the decoder fallback keeps the same Number scratch {field_num}, got {fallback:?}"
            );
            // Generation read: the unsigned decoder and the marker parameter
            // equality survive the state shortcut unchanged.
            let gen_at = producer_at + 1;
            let Some(S::Let {
                index: gen_read_slot,
                declared_schema: Some(T::Number),
                expr: gen_read,
                ..
            }) = body.get(gen_at)
            else {
                panic!(
                    "the generation read immediately follows the marker state producer, got {:?}",
                    body.get(gen_at)
                )
            };
            assert_eq!(
                read_target(gen_read, read, carrier),
                Some((row_addr, 1)),
                "the generation read keeps the next ROW_GENERATION cell"
            );
            let Some(S::Let {
                index: stored_gen,
                declared_schema: Some(T::Int),
                expr: gen_decoder,
                ..
            }) = body.get(gen_at + 1)
            else {
                panic!(
                    "the marker generation binds an Int immediately after its read, got {:?}",
                    body.get(gen_at + 1)
                )
            };
            let Some(gen_arg) = decode_arg(gen_decoder, decode_u32) else {
                panic!(
                    "the generation stays on the unsigned generation decoder, got {gen_decoder:?}"
                )
            };
            assert_eq!(
                scratch_slot(gen_arg),
                Some(*gen_read_slot),
                "the generation decoder reads the generation scratch {gen_read_slot}, got \
                 {gen_arg:?}"
            );
            let Some(S::Expr {
                expr: E::Call(assert, _, args, _, _),
                ..
            }) = body.get(gen_at + 3)
            else {
                panic!(
                    "the marker generation equality follows the active-state guard, got {:?}",
                    body.get(gen_at + 3)
                )
            };
            assert_eq!(
                *assert,
                vm::BuiltinFunction::Assert.call_index(),
                "the generation equality stays an assert"
            );
            let E::Eq(gen_left, gen_right) = &args[0] else {
                panic!("the generation guard keeps its equality, got {:?}", args[0])
            };
            assert!(
                matches!(gen_left.as_ref(), E::Var(slot) if slot == stored_gen),
                "the generation guard tests the decoded generation slot {stored_gen}, got \
                 {gen_left:?}"
            );
            let E::Var(gen_param) = gen_right.as_ref() else {
                panic!(
                    "the generation guard keeps the marker generation parameter, got {gen_right:?}"
                )
            };
            assert!(
                params.contains(gen_param),
                "the generation guard reads an actual marker parameter, got {gen_param}"
            );
            // Physical base, kind and mark sequence: unchanged by the state
            // shortcut.
            let base_at = gen_at + 4;
            let Some(S::Let {
                index: base_read_slot,
                declared_schema: Some(T::Number),
                expr: base_read,
                ..
            }) = body.get(base_at)
            else {
                panic!(
                    "the physical-base read follows the generation guard, got {:?}",
                    body.get(base_at)
                )
            };
            assert_eq!(
                read_target(base_read, read, carrier),
                Some((row_addr, 2)),
                "the marker keeps its ROW_PHYSICAL_BASE read"
            );
            let Some(S::Let {
                index: base,
                declared_schema: Some(T::Int),
                expr: base_decoder,
                ..
            }) = body.get(base_at + 1)
            else {
                panic!(
                    "the physical base binds an Int, got {:?}",
                    body.get(base_at + 1)
                )
            };
            let Some(base_arg) = decode_arg(base_decoder, decode) else {
                panic!("the physical base keeps the slot decoder, got {base_decoder:?}")
            };
            assert_eq!(
                scratch_slot(base_arg),
                Some(*base_read_slot),
                "the physical-base decoder reads the base scratch {base_read_slot}, got \
                 {base_arg:?}"
            );
            let Some(S::Let {
                index: kind_read_slot,
                declared_schema: Some(T::Number),
                expr: kind_read,
                ..
            }) = body.get(base_at + 2)
            else {
                panic!(
                    "the node-kind read follows the physical base, got {:?}",
                    body.get(base_at + 2)
                )
            };
            let Some(kind_args) = plain_call(kind_read, read, 2) else {
                panic!("the node-kind read keeps a raw B1 read, got {kind_read:?}")
            };
            assert!(
                matches!(&kind_args[0], E::Var(c) if *c == carrier),
                "the node-kind read stays on the marker carrier"
            );
            assert_eq!(
                scratch_slot(&kind_args[1]),
                Some(*base),
                "the node-kind read addresses the decoded physical base {base}, got {:?}",
                kind_args[1]
            );
            let Some(S::Let {
                index: kind,
                declared_schema: Some(T::Int),
                expr: kind_decoder,
                ..
            }) = body.get(base_at + 3)
            else {
                panic!(
                    "the node kind binds an Int, got {:?}",
                    body.get(base_at + 3)
                )
            };
            let Some(kind_arg) = decode_arg(kind_decoder, decode) else {
                panic!("the node kind keeps the slot decoder, got {kind_decoder:?}")
            };
            assert_eq!(
                scratch_slot(kind_arg),
                Some(*kind_read_slot),
                "the kind decoder reads the kind scratch {kind_read_slot}, got {kind_arg:?}"
            );
            let Some(S::Expr {
                expr: E::Call(assert, _, args, _, _),
                ..
            }) = body.get(base_at + 4)
            else {
                panic!(
                    "the marker kind guard follows the kind decode, got {:?}",
                    body.get(base_at + 4)
                )
            };
            assert_eq!(
                *assert,
                vm::BuiltinFunction::Assert.call_index(),
                "the marker kind guard stays an assert"
            );
            assert!(
                args.len() == 1 && uses_var(&args[0], *kind),
                "the marker kind guard still tests the decoded kind slot {kind}, got {:?}",
                args.first()
            );
            let mark_at = base_at + 5;
            let Some(S::Let {
                index: marked_read_slot,
                declared_schema: Some(T::Number),
                expr: marked_read,
                ..
            }) = body.get(mark_at)
            else {
                panic!(
                    "the mark-cell read follows the kind guard, got {:?}",
                    body.get(mark_at)
                )
            };
            assert_eq!(
                read_target(marked_read, read, carrier),
                Some((row_addr, 4)),
                "the marker keeps its ROW_MARK read"
            );
            let Some(S::Let {
                index: marked,
                declared_schema: Some(T::Int),
                expr: marked_decoder,
                ..
            }) = body.get(mark_at + 1)
            else {
                panic!(
                    "the mark cell binds an Int, got {:?}",
                    body.get(mark_at + 1)
                )
            };
            let Some(marked_arg) = decode_arg(marked_decoder, decode) else {
                panic!("the mark cell keeps the slot decoder, got {marked_decoder:?}")
            };
            assert_eq!(
                scratch_slot(marked_arg),
                Some(*marked_read_slot),
                "the mark decoder reads the mark scratch {marked_read_slot}, got {marked_arg:?}"
            );
            let Some(S::Let {
                index: gray_num,
                expr: gray_read,
                ..
            }) = body.get(mark_at + 2)
            else {
                panic!(
                    "the gray head read follows the mark decode, got {:?}",
                    body.get(mark_at + 2)
                )
            };
            let Some(gray_args) = plain_call(gray_read, read, 2) else {
                panic!("the gray head keeps a raw B1 read, got {gray_read:?}")
            };
            assert_eq!(
                header_cell(&gray_args[1]),
                Some(13),
                "the gray head stays header cell 13, got {:?}",
                gray_args[1]
            );
            let Some(S::Let {
                index: gray,
                declared_schema: Some(T::Int),
                expr: gray_decoder,
                ..
            }) = body.get(mark_at + 3)
            else {
                panic!(
                    "the gray head binds an Int, got {:?}",
                    body.get(mark_at + 3)
                )
            };
            let Some(gray_arg) = decode_arg(gray_decoder, decode) else {
                panic!("the gray head keeps the slot decoder, got {gray_decoder:?}")
            };
            assert_eq!(
                scratch_slot(gray_arg),
                Some(*gray_num),
                "the gray decoder reads the gray scratch {gray_num}, got {gray_arg:?}"
            );
            let Some(S::IfElse {
                condition,
                then_branch,
                else_branch,
                ..
            }) = body.get(mark_at + 4)
            else {
                panic!(
                    "the already-marked branch closes the marker body, got {:?}",
                    body.get(mark_at + 4)
                )
            };
            assert!(
                matches!(
                    condition,
                    E::Eq(left, right)
                        if matches!(left.as_ref(), E::Var(slot) if slot == marked)
                            && matches!(right.as_ref(), E::Int(0))
                ),
                "the mark branch still tests marked == 0, got {condition:?}"
            );
            assert!(
                else_branch.is_empty(),
                "the mark branch keeps no else branch"
            );
            assert_eq!(
                then_branch.len(),
                3,
                "the mark branch keeps the mark, gray link and gray-head writes"
            );
            let mut writes: Vec<(u16, i64, &E)> = Vec::new();
            let mut head_writes = 0usize;
            for stmt in then_branch {
                let S::Assign { expr, .. } = stmt else {
                    panic!("the mark branch keeps raw B1 writes, got {stmt:?}")
                };
                match write_target(expr, write, carrier) {
                    Some((base, offset, payload)) => {
                        assert_eq!(base, row_addr, "the mark branch writes the scanned row");
                        writes.push((base, offset, payload));
                    }
                    None => {
                        let Some(args) = plain_call(expr, write, 3) else {
                            panic!("the mark branch writes through B1, got {expr:?}")
                        };
                        assert_eq!(
                            header_cell(&args[1]),
                            Some(13),
                            "the mark branch updates the gray head, got {:?}",
                            args[1]
                        );
                        head_writes += 1;
                    }
                }
            }
            assert_eq!(
                writes.len(),
                2,
                "the mark branch writes the mark and its link"
            );
            assert_eq!(writes[0].1, 4, "the first write sets ROW_MARK");
            assert!(
                int_payload(writes[0].2, 1),
                "the mark write stores Int(1), got {:?}",
                writes[0].2
            );
            assert_eq!(writes[1].1, 5, "the second write sets the gray link");
            assert!(
                uses_var(writes[1].2, *gray),
                "the gray link stores the scanned gray head {gray}, got {:?}",
                writes[1].2
            );
            assert_eq!(head_writes, 1, "the mark branch updates the gray head once");
            eprintln!(
                "P7_MARKER_ACTIVE_STATE_STRUCTURE carrier={carrier} st={st} \
                 field_num={field_num} row_addr={row_addr} gen={stored_gen} base={base} \
                 kind={kind} marked={marked} gray={gray}"
            );
            Ok(ir)
        }
    }
    let plugin: &'static dyn vm::SourcePlugin = Box::leak(Box::new(Tap));
    let (_machine, _) = build_machine("var o={v:1};o.v===1;", MachineSource::Tapped(plugin), false);
}

/// Phase-1 active-state positive controls: the sole row-state write is the
/// exact numeric active value (Float 2.0 or raw Int 2), so the collection runs
/// over the intact roots and halts with the original boxed completion.
fn run_state_active_control(fault: RootSeamFault, case: &'static str) {
    eprintln!("P7_ROOTS_STATE_BEGIN {case} fault={fault:?}");
    let plugin = leak_root_seam(fault);
    let tapped: &'static dyn vm::SourcePlugin = plugin;
    let (mut machine, _) =
        build_machine("var o={v:1};o.v===1;", MachineSource::Tapped(tapped), false);
    eprintln!("P7_ROOTS_STATE_COMPILED {case}");
    let status = machine.run();
    eprintln!(
        "P7_ROOTS_STATE_RESULT {case} status={status:?} ip={} fuel={:?}",
        machine.ip(),
        machine.get_fuel()
    );
    assert!(
        !machine.jit_config().enabled,
        "interpreter only on the {case} active state"
    );
    assert_eq!(
        machine.aot_exec_count(),
        0,
        "no AOT on the {case} active state"
    );
    assert_eq!(
        machine.jit_native_exec_count(),
        0,
        "no trace execution on the {case} active state"
    );
    assert_eq!(
        status.unwrap_or_else(|error| panic!(
            "{case} active state must halt: {error:?} ip={}",
            machine.ip()
        )),
        VmStatus::Halted
    );
    assert_eq!(
        machine.host_context().resource_count(),
        1,
        "{case} keeps one actual backing owner"
    );
    let backing = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("actual backing accounting");
    assert_eq!((backing.pending, backing.registered), (0, 1));
    assert!(
        plugin.carrier.lock().unwrap().is_some()
            && plugin.frame.lock().unwrap().is_some()
            && plugin.entry.lock().unwrap().is_some()
            && plugin.object.lock().unwrap().is_some(),
        "{case} post-source seam discovery bound carrier, frame, entry and object"
    );
    assert!(
        machine
            .capture_certification_stats()
            .is_some_and(|facts| facts.valid),
        "{case} keeps the certified capture facts"
    );
    let actual = machine.stack().to_vec();
    let last = actual.last().expect("script value");
    assert_eq!(field(last, "tag"), Value::Int(2), "{case} boxed completion");
    assert_eq!(
        field(last, "flag"),
        Value::Bool(true),
        "{case} boxed completion flag"
    );
    machine.reset_for_reuse().expect("reset");
    assert_eq!(
        machine.host_context().resource_count(),
        0,
        "{case} reset releases the real owner"
    );
    eprintln!(
        "P7_ROOTS_STATE_ACTIVE {case} entry={:?} frame={:?}",
        plugin.entry.lock().unwrap(),
        plugin.frame.lock().unwrap()
    );
}

/// Phase-1 state refusals that must come from the unchanged slot decoder under
/// the collector: the decoder's own continuation decomposes to the phase-1
/// state read callsite, and its fired decoder site is classified: a strict
/// Assert domain guard with its real constant operand, or the single exact Int
/// conversion call for a fractional value. A bare `assertion failed` host
/// error is never accepted.
fn run_state_refusal(fault: RootSeamFault, case: &'static str, expect: CategoryGuard) {
    eprintln!("P7_ROOTS_STATE_BEGIN {case} fault={fault:?} expect={expect:?}");
    let plugin = leak_root_seam(fault);
    let tapped: &'static dyn vm::SourcePlugin = plugin;
    let (mut machine, _) =
        build_machine("var o={v:1};o.v===1;", MachineSource::Tapped(tapped), false);
    eprintln!("P7_ROOTS_STATE_COMPILED {case}");
    let status = machine.run();
    eprintln!(
        "P7_ROOTS_STATE_RESULT {case} status={status:?} ip={} fuel={:?}",
        machine.ip(),
        machine.get_fuel()
    );
    assert!(
        matches!(&status, Err(vm::VmError::HostError(message)) if message == "assertion failed"),
        "{case} must refuse inside the exact slot decoder: {status:?}"
    );
    assert!(
        !machine.jit_config().enabled,
        "interpreter only on the {case} state refusal"
    );
    assert_eq!(machine.aot_exec_count(), 0, "no AOT on the {case} refusal");
    assert_eq!(
        machine.jit_native_exec_count(),
        0,
        "no trace execution on the {case} refusal"
    );
    let helpers = plugin.helpers.lock().unwrap();
    let sorted_keys = plugin.sorted_impl_keys.lock().unwrap();
    let ip = machine.ip();
    let frames = machine.execution_frames();
    let program = machine.program();
    assert!(
        frames.windows(2).all(|w| w[0].local_base < w[1].local_base),
        "{case} outer-to-inner frame order: {frames:?}"
    );
    assert_eq!(
        frames.len(),
        3,
        "{case} refusal stack is script root, collector, decoder: {frames:?}"
    );
    assert!(
        frames[0].prototype_id.is_none(),
        "{case} outermost frame is the script root: {frames:?}"
    );
    let collector_id = frames[1].prototype_id.expect("collector prototype");
    let decoder_id = frames[2].prototype_id.expect("decoder prototype");
    let linked = prev_link_sorted_regions(program);
    let failing = linked
        .iter()
        .find(|region| (region.0 as usize) <= ip && ip < (region.1 as usize))
        .unwrap_or_else(|| panic!("{case} refusal ip inside a linked region"));
    assert_eq!(
        failing.2, decoder_id,
        "{case} refuses inside the actual phase-1 state decoder, not a marker or the collector"
    );
    assert_runtime_prototype(
        program,
        &linked,
        &helpers,
        "__compat_es5_decode_int",
        decoder_id,
    );
    assert_runtime_prototype(
        program,
        &linked,
        &helpers,
        "__compat_es5_heap_collect",
        collector_id,
    );
    assert_emission_base(
        &helpers,
        &sorted_keys,
        &[
            ("__compat_es5_decode_int", decoder_id),
            ("__compat_es5_heap_collect", collector_id),
        ],
    );
    let decoder_proto = &program.callable_prototypes[decoder_id as usize];
    let return_ip = match &frames[2].continuation {
        vm::vm::VmFrameContinuation::ResumeBytecode { return_ip } => *return_ip,
        other => panic!("{case}: the state decoder must resume bytecode: {other:?}"),
    };
    let collector_region = linked
        .iter()
        .find(|region| region.2 == collector_id)
        .expect("collector linked region");
    assert!(
        (collector_region.0 as usize) <= return_ip && return_ip < (collector_region.1 as usize),
        "{case} decoder returns toward the phase-1 state read inside the collector"
    );
    assert_call_site(
        program,
        collector_region.0,
        return_ip,
        decoder_id,
        decoder_proto.arity,
        "STATE_READ",
    );
    let guard = match expect {
        CategoryGuard::ExactConversion => {
            tag_exact_conversion_site(program, failing.0, failing.1, ip, case);
            CategoryGuard::ExactConversion
        }
        expect_guard => {
            let assert_start = tag_guard_site(program, failing.0, ip, case);
            let guard = category_guard(program, failing.0 as usize, assert_start);
            assert_eq!(
                guard, expect_guard,
                "{case} must refuse at its own decoder guard, got {guard:?}"
            );
            guard
        }
    };
    prev_link_dump_window(&program.code, program, ip, "state-failure");
    prev_link_dump_window(&program.code, program, return_ip, "state-callsite");
    eprint_seam_frames(&machine, case);
    eprintln!(
        "P7_ROOTS_STATE_REFUSAL {case} decoder={decoder_id} collector={collector_id} \
         ip={ip} return_ip={return_ip} guard={guard:?} entry={:?}",
        plugin.entry.lock().unwrap()
    );
}

/// Phase-1 state domain refusal: the value decodes but is outside
/// `[0, STATE_RETIRED]`, so the collector's own domain guard must fire before
/// any nested decoder work. The stack stays script root then collector, the
/// fired assert ends exactly at the failure ip, and the bound is read back
/// from the actual constant pool.
fn run_state_domain_refusal(fault: RootSeamFault, case: &'static str, bound: i64) {
    eprintln!("P7_ROOTS_STATE_BEGIN {case} fault={fault:?} bound={bound}");
    let plugin = leak_root_seam(fault);
    let tapped: &'static dyn vm::SourcePlugin = plugin;
    let (mut machine, _) =
        build_machine("var o={v:1};o.v===1;", MachineSource::Tapped(tapped), false);
    eprintln!("P7_ROOTS_STATE_COMPILED {case}");
    let status = machine.run();
    eprintln!(
        "P7_ROOTS_STATE_RESULT {case} status={status:?} ip={} fuel={:?}",
        machine.ip(),
        machine.get_fuel()
    );
    assert!(
        matches!(&status, Err(vm::VmError::HostError(message)) if message == "assertion failed"),
        "{case} must refuse at the collector row-state domain guard: {status:?}"
    );
    assert!(
        !machine.jit_config().enabled,
        "interpreter only on the {case} domain refusal"
    );
    assert_eq!(machine.aot_exec_count(), 0, "no AOT on the {case} refusal");
    assert_eq!(
        machine.jit_native_exec_count(),
        0,
        "no trace execution on the {case} refusal"
    );
    let helpers = plugin.helpers.lock().unwrap();
    let ip = machine.ip();
    let frames = machine.execution_frames();
    let program = machine.program();
    assert!(
        frames.windows(2).all(|w| w[0].local_base < w[1].local_base),
        "{case} outer-to-inner frame order: {frames:?}"
    );
    assert_eq!(
        frames.len(),
        2,
        "{case} refuses in the collector without entering the decoder: {frames:?}"
    );
    assert!(
        frames[0].prototype_id.is_none(),
        "{case} outermost frame is the script root: {frames:?}"
    );
    let collector_id = frames[1].prototype_id.expect("collector prototype");
    let linked = prev_link_sorted_regions(program);
    let failing = linked
        .iter()
        .find(|region| (region.0 as usize) <= ip && ip < (region.1 as usize))
        .unwrap_or_else(|| panic!("{case} refusal ip inside a linked region"));
    assert_eq!(
        failing.2, collector_id,
        "{case} refuses inside the collector, not a decoder or marker"
    );
    assert_runtime_prototype(
        program,
        &linked,
        &helpers,
        "__compat_es5_heap_collect",
        collector_id,
    );
    let assert_start = tag_guard_site(program, failing.0, ip, "STATE_DOMAIN");
    let ops = prev_link_ops(&program.code, failing.0 as usize);
    let at = ops
        .iter()
        .position(|(addr, _, _)| *addr == assert_start)
        .expect("the state domain guard starts at the failure boundary");
    let begin = at.saturating_sub(12);
    let window = &ops[begin..at];
    let mut bounds: Vec<usize> = Vec::new();
    let mut compared = 0usize;
    for (addr, op, operands) in window {
        match op {
            vm::OpCode::Ldc => {
                let index = u16::from_le_bytes([operands[0], operands[1]]);
                let loaded = match program.constants.get(index as usize) {
                    Some(Value::Int(value)) => Some(*value),
                    Some(Value::Float(value)) if value.fract() == 0.0 => Some(*value as i64),
                    _ => None,
                };
                if loaded == Some(bound) {
                    bounds.push(*addr);
                }
            }
            vm::OpCode::Clt | vm::OpCode::Ceq => compared += 1,
            _ => {}
        }
    }
    assert!(
        !bounds.is_empty(),
        "{case} must load the actual domain bound {bound} from the constant pool at the fired \
         guard, window={window:?}"
    );
    assert!(
        compared > 0,
        "{case} the fired guard keeps its comparison against the domain bound, window={window:?}"
    );
    prev_link_dump_window(&program.code, program, ip, "state-domain-failure");
    eprint_seam_frames(&machine, case);
    eprintln!(
        "P7_ROOTS_STATE_DOMAIN {case} collector={collector_id} ip={ip} \
         guard_start={assert_start} bound={bound} bounds={bounds:?} window={window:?}"
    );
}

#[test]
fn p7_roots_phase1_active_state_float_control_interpreter() {
    run_state_active_control(RootSeamFault::StateActiveFloat, "STATE_ACTIVE_FLOAT");
}

#[test]
fn p7_roots_phase1_active_state_raw_int_control_interpreter() {
    run_state_active_control(RootSeamFault::StateActiveRawInt, "STATE_ACTIVE_RAW_INT");
}

#[test]
fn p7_roots_phase1_state_fraction_refusal_interpreter() {
    run_state_refusal(
        RootSeamFault::StateFractional,
        "STATE_FRACTION",
        CategoryGuard::ExactConversion,
    );
}

#[test]
fn p7_roots_phase1_state_negative_refusal_interpreter() {
    run_state_refusal(
        RootSeamFault::StateNegative,
        "STATE_NEGATIVE",
        CategoryGuard::LowerBound,
    );
}

#[test]
fn p7_roots_phase1_state_nan_refusal_interpreter() {
    run_state_refusal(
        RootSeamFault::StateNaN,
        "STATE_NAN",
        CategoryGuard::UpperBound,
    );
}

#[test]
fn p7_roots_phase1_state_pos_inf_refusal_interpreter() {
    run_state_refusal(
        RootSeamFault::StatePosInf,
        "STATE_POS_INF",
        CategoryGuard::UpperBound,
    );
}

#[test]
fn p7_roots_phase1_state_neg_inf_refusal_interpreter() {
    run_state_refusal(
        RootSeamFault::StateNegInf,
        "STATE_NEG_INF",
        CategoryGuard::LowerBound,
    );
}

#[test]
fn p7_roots_phase1_state_domain_refusal_interpreter() {
    run_state_domain_refusal(RootSeamFault::StateDomainHigh, "STATE_DOMAIN_HIGH", 4);
}

/// Direct-marker positive controls: the validated permanent entry 0 row state
/// is the exact numeric active value (Float 2.0 or raw Int 2), so the real
/// mark helper runs with expect=3 immediately after the single state write.
/// The row may already be marked or may queue gray work; no collection runs,
/// so the script still halts with the original boxed completion and one live
/// backing owner.
fn run_marker_active_control(fault: RootSeamFault, case: &'static str) {
    eprintln!("P7_ROOTS_MARKER_BEGIN {case} fault={fault:?}");
    let plugin = leak_root_seam(fault);
    let tapped: &'static dyn vm::SourcePlugin = plugin;
    let (mut machine, _) =
        build_machine("var o={v:1};o.v===1;", MachineSource::Tapped(tapped), false);
    eprintln!("P7_ROOTS_MARKER_COMPILED {case}");
    let status = machine.run();
    eprintln!(
        "P7_ROOTS_MARKER_RESULT {case} status={status:?} ip={} fuel={:?}",
        machine.ip(),
        machine.get_fuel()
    );
    assert!(
        !machine.jit_config().enabled,
        "interpreter only on the {case} marker state"
    );
    assert_eq!(
        machine.aot_exec_count(),
        0,
        "no AOT on the {case} marker state"
    );
    assert_eq!(
        machine.jit_native_exec_count(),
        0,
        "no trace execution on the {case} marker state"
    );
    assert_eq!(
        status.unwrap_or_else(|error| panic!(
            "{case} marker state must halt: {error:?} ip={}",
            machine.ip()
        )),
        VmStatus::Halted
    );
    assert_eq!(
        machine.host_context().resource_count(),
        1,
        "{case} keeps one actual backing owner"
    );
    let backing = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
        .expect("actual backing accounting");
    assert_eq!((backing.pending, backing.registered), (0, 1));
    assert!(
        plugin.carrier.lock().unwrap().is_some()
            && plugin.frame.lock().unwrap().is_some()
            && plugin.entry.lock().unwrap().is_some()
            && plugin.object.lock().unwrap().is_some(),
        "{case} post-source seam discovery bound carrier, frame, entry and object"
    );
    assert!(
        machine
            .capture_certification_stats()
            .is_some_and(|facts| facts.valid),
        "{case} keeps the certified capture facts"
    );
    let actual = machine.stack().to_vec();
    let last = actual.last().expect("script value");
    assert_eq!(field(last, "tag"), Value::Int(2), "{case} boxed completion");
    assert_eq!(
        field(last, "flag"),
        Value::Bool(true),
        "{case} boxed completion flag"
    );
    machine.reset_for_reuse().expect("reset");
    assert_eq!(
        machine.host_context().resource_count(),
        0,
        "{case} reset releases the real owner"
    );
    eprintln!(
        "P7_ROOTS_MARKER_ACTIVE {case} entry={:?} frame={:?}",
        plugin.entry.lock().unwrap(),
        plugin.frame.lock().unwrap()
    );
}

/// Direct-marker value refusals: the state value must decode but is not the
/// exact active value, so the real mark helper's own STATE fallback invokes the
/// unchanged slot decoder. The stack is script root, marker, decoder, with no
/// collector frame. The decoder continuation decomposes to the marker's
/// fallback callsite and the marker continuation to the direct script-root
/// callsite. The fired decoder site is classified: a strict Assert domain
/// guard with its real constant operand, or the single exact Int conversion
/// call for a fractional value; a bare `assertion failed` host error is never
/// accepted.
fn run_marker_state_refusal(fault: RootSeamFault, case: &'static str, expect: CategoryGuard) {
    eprintln!("P7_ROOTS_MARKER_BEGIN {case} fault={fault:?} expect={expect:?}");
    let plugin = leak_root_seam(fault);
    let tapped: &'static dyn vm::SourcePlugin = plugin;
    let (mut machine, _) =
        build_machine("var o={v:1};o.v===1;", MachineSource::Tapped(tapped), false);
    eprintln!("P7_ROOTS_MARKER_COMPILED {case}");
    let status = machine.run();
    eprintln!(
        "P7_ROOTS_MARKER_RESULT {case} status={status:?} ip={} fuel={:?}",
        machine.ip(),
        machine.get_fuel()
    );
    assert!(
        matches!(&status, Err(vm::VmError::HostError(message)) if message == "assertion failed"),
        "{case} must refuse inside the exact slot decoder under the marker: {status:?}"
    );
    assert!(
        !machine.jit_config().enabled,
        "interpreter only on the {case} refusal"
    );
    assert_eq!(machine.aot_exec_count(), 0, "no AOT on the {case} refusal");
    assert_eq!(
        machine.jit_native_exec_count(),
        0,
        "no trace execution on the {case} refusal"
    );
    let helpers = plugin.helpers.lock().unwrap();
    let sorted_keys = plugin.sorted_impl_keys.lock().unwrap();
    let ip = machine.ip();
    let frames = machine.execution_frames();
    let program = machine.program();
    assert!(
        frames.windows(2).all(|w| w[0].local_base < w[1].local_base),
        "{case} outer-to-inner frame order: {frames:?}"
    );
    assert_eq!(
        frames.len(),
        3,
        "{case} refusal stack is script root, marker, decoder with no collector: {frames:?}"
    );
    assert!(
        frames[0].prototype_id.is_none(),
        "{case} outermost frame is the script root: {frames:?}"
    );
    let marker_id = frames[1].prototype_id.expect("marker prototype");
    let decoder_id = frames[2].prototype_id.expect("decoder prototype");
    let linked = prev_link_sorted_regions(program);
    let failing = linked
        .iter()
        .find(|region| (region.0 as usize) <= ip && ip < (region.1 as usize))
        .unwrap_or_else(|| panic!("{case} refusal ip inside a linked region"));
    assert_eq!(
        failing.2, decoder_id,
        "{case} refuses inside the marker's STATE fallback decoder, not the marker"
    );
    assert_runtime_prototype(
        program,
        &linked,
        &helpers,
        "__compat_es5_decode_int",
        decoder_id,
    );
    assert_runtime_prototype(program, &linked, &helpers, "__compat_es5_mark", marker_id);
    assert_emission_base(
        &helpers,
        &sorted_keys,
        &[
            ("__compat_es5_decode_int", decoder_id),
            ("__compat_es5_mark", marker_id),
        ],
    );
    let decoder_proto = &program.callable_prototypes[decoder_id as usize];
    let return_ip = match &frames[2].continuation {
        vm::vm::VmFrameContinuation::ResumeBytecode { return_ip } => *return_ip,
        other => panic!("{case}: the state decoder must resume bytecode: {other:?}"),
    };
    let marker_region = linked
        .iter()
        .find(|region| region.2 == marker_id)
        .expect("marker linked region");
    assert!(
        (marker_region.0 as usize) <= return_ip && return_ip < (marker_region.1 as usize),
        "{case} decoder returns toward the marker's STATE fallback"
    );
    assert_call_site(
        program,
        marker_region.0,
        return_ip,
        decoder_id,
        decoder_proto.arity,
        "STATE_FALLBACK",
    );
    let marker_return_ip = match &frames[1].continuation {
        vm::vm::VmFrameContinuation::ResumeBytecode { return_ip } => *return_ip,
        other => panic!("{case}: the marker must resume bytecode: {other:?}"),
    };
    let root_region = program
        .function_regions
        .iter()
        .find(|region| region.prototype_id.is_none())
        .expect("script root region");
    assert!(
        (root_region.start_ip as usize) <= marker_return_ip
            && marker_return_ip < (root_region.end_ip as usize),
        "{case} marker return location stays inside the script root"
    );
    let marker_proto = &program.callable_prototypes[marker_id as usize];
    assert_call_site(
        program,
        root_region.start_ip,
        marker_return_ip,
        marker_id,
        marker_proto.arity,
        "DIRECT_ROOT_MARK",
    );
    let guard = match expect {
        CategoryGuard::ExactConversion => {
            tag_exact_conversion_site(program, failing.0, failing.1, ip, case);
            CategoryGuard::ExactConversion
        }
        expect_guard => {
            let assert_start = tag_guard_site(program, failing.0, ip, case);
            let guard = category_guard(program, failing.0 as usize, assert_start);
            assert_eq!(
                guard, expect_guard,
                "{case} must refuse at its own decoder guard, got {guard:?}"
            );
            guard
        }
    };
    prev_link_dump_window(&program.code, program, ip, "marker-state-failure");
    prev_link_dump_window(&program.code, program, return_ip, "marker-state-callsite");
    eprint_seam_frames(&machine, case);
    eprintln!(
        "P7_ROOTS_MARKER_REFUSAL {case} decoder={decoder_id} marker={marker_id} ip={ip} \
         return_ip={return_ip} marker_return_ip={marker_return_ip} guard={guard:?} entry={:?}",
        plugin.entry.lock().unwrap()
    );
}

/// Direct-marker inactive state: the value decodes as Int(5), which is not the
/// exact STATE_ACTIVE(2), so the mark helper's own `state == 2` assert must
/// fire. The stack is script root then marker only: no decoder and no
/// collector. The fired assert ends exactly at the failure ip and loads the
/// real Int(2) operand from the actual constant pool.
fn run_marker_state_guard_refusal(fault: RootSeamFault, case: &'static str) {
    eprintln!("P7_ROOTS_MARKER_BEGIN {case} fault={fault:?}");
    let plugin = leak_root_seam(fault);
    let tapped: &'static dyn vm::SourcePlugin = plugin;
    let (mut machine, _) =
        build_machine("var o={v:1};o.v===1;", MachineSource::Tapped(tapped), false);
    eprintln!("P7_ROOTS_MARKER_COMPILED {case}");
    let status = machine.run();
    eprintln!(
        "P7_ROOTS_MARKER_RESULT {case} status={status:?} ip={} fuel={:?}",
        machine.ip(),
        machine.get_fuel()
    );
    assert!(
        matches!(&status, Err(vm::VmError::HostError(message)) if message == "assertion failed"),
        "{case} must refuse at the marker's active-state guard: {status:?}"
    );
    assert!(
        !machine.jit_config().enabled,
        "interpreter only on the {case} guard"
    );
    assert_eq!(machine.aot_exec_count(), 0, "no AOT on the {case} guard");
    assert_eq!(
        machine.jit_native_exec_count(),
        0,
        "no trace execution on the {case} guard"
    );
    let helpers = plugin.helpers.lock().unwrap();
    let sorted_keys = plugin.sorted_impl_keys.lock().unwrap();
    let ip = machine.ip();
    let frames = machine.execution_frames();
    let program = machine.program();
    assert!(
        frames.windows(2).all(|w| w[0].local_base < w[1].local_base),
        "{case} outer-to-inner frame order: {frames:?}"
    );
    assert_eq!(
        frames.len(),
        2,
        "{case} refusal stack is script root and marker, with no decoder or collector: {frames:?}"
    );
    assert!(
        frames[0].prototype_id.is_none(),
        "{case} outermost frame is the script root: {frames:?}"
    );
    let marker_id = frames[1].prototype_id.expect("marker prototype");
    let linked = prev_link_sorted_regions(program);
    let failing = linked
        .iter()
        .find(|region| (region.0 as usize) <= ip && ip < (region.1 as usize))
        .unwrap_or_else(|| panic!("{case} refusal ip inside a linked region"));
    assert_eq!(
        failing.2, marker_id,
        "{case} refuses inside the marker's own guard, not a decoder or collector"
    );
    assert_runtime_prototype(program, &linked, &helpers, "__compat_es5_mark", marker_id);
    assert_emission_base(&helpers, &sorted_keys, &[("__compat_es5_mark", marker_id)]);
    let assert_start = tag_guard_site(program, failing.0, ip, "MARKER_ACTIVE");
    let ops = prev_link_ops(&program.code, failing.0 as usize);
    let at = ops
        .iter()
        .position(|(addr, _, _)| *addr == assert_start)
        .expect("the marker active guard starts at the failure boundary");
    let (_, compare, _) = ops
        .get(at.checked_sub(1).expect("the guard keeps its comparison"))
        .expect("the guard comparison");
    assert!(
        matches!(compare, vm::OpCode::Ceq),
        "{case} marker active guard must compare with Ceq, got {compare:?}"
    );
    let (_, ldc, operand) = ops
        .get(
            at.checked_sub(2)
                .expect("the guard keeps its constant load"),
        )
        .expect("the guard constant");
    assert!(
        matches!(ldc, vm::OpCode::Ldc),
        "{case} marker active guard must load its constant, got {ldc:?}"
    );
    let index = u16::from_le_bytes([operand[0], operand[1]]);
    let active = program.constants.get(index as usize);
    assert!(
        matches!(active, Some(Value::Int(2))),
        "{case} guard must load the real Int(2) STATE_ACTIVE constant, got {active:?}"
    );
    let begin = at.saturating_sub(4);
    let window = &ops[begin..at];
    let marker_return_ip = match &frames[1].continuation {
        vm::vm::VmFrameContinuation::ResumeBytecode { return_ip } => *return_ip,
        other => panic!("{case}: the marker must resume bytecode: {other:?}"),
    };
    let root_region = program
        .function_regions
        .iter()
        .find(|region| region.prototype_id.is_none())
        .expect("script root region");
    assert!(
        (root_region.start_ip as usize) <= marker_return_ip
            && marker_return_ip < (root_region.end_ip as usize),
        "{case} marker return location stays inside the script root"
    );
    let marker_proto = &program.callable_prototypes[marker_id as usize];
    assert_call_site(
        program,
        root_region.start_ip,
        marker_return_ip,
        marker_id,
        marker_proto.arity,
        "DIRECT_ROOT_MARK",
    );
    prev_link_dump_window(&program.code, program, ip, "marker-guard-failure");
    eprint_seam_frames(&machine, case);
    eprintln!(
        "P7_ROOTS_MARKER_GUARD {case} marker={marker_id} ip={ip} \
         marker_return_ip={marker_return_ip} active={active:?} window={window:?}"
    );
}

#[test]
fn p7_roots_marker_active_state_float_control_interpreter() {
    run_marker_active_control(
        RootSeamFault::MarkerStateActiveFloat,
        "MARKER_STATE_ACTIVE_FLOAT",
    );
}

#[test]
fn p7_roots_marker_active_state_raw_int_control_interpreter() {
    run_marker_active_control(
        RootSeamFault::MarkerStateActiveRawInt,
        "MARKER_STATE_ACTIVE_RAW_INT",
    );
}

#[test]
fn p7_roots_marker_state_fraction_refusal_interpreter() {
    run_marker_state_refusal(
        RootSeamFault::MarkerStateFractional,
        "MARKER_STATE_FRACTION",
        CategoryGuard::ExactConversion,
    );
}

#[test]
fn p7_roots_marker_state_negative_refusal_interpreter() {
    run_marker_state_refusal(
        RootSeamFault::MarkerStateNegative,
        "MARKER_STATE_NEGATIVE",
        CategoryGuard::LowerBound,
    );
}

#[test]
fn p7_roots_marker_state_nan_refusal_interpreter() {
    run_marker_state_refusal(
        RootSeamFault::MarkerStateNaN,
        "MARKER_STATE_NAN",
        CategoryGuard::UpperBound,
    );
}

#[test]
fn p7_roots_marker_state_pos_inf_refusal_interpreter() {
    run_marker_state_refusal(
        RootSeamFault::MarkerStatePosInf,
        "MARKER_STATE_POS_INF",
        CategoryGuard::UpperBound,
    );
}

#[test]
fn p7_roots_marker_state_neg_inf_refusal_interpreter() {
    run_marker_state_refusal(
        RootSeamFault::MarkerStateNegInf,
        "MARKER_STATE_NEG_INF",
        CategoryGuard::LowerBound,
    );
}

#[test]
fn p7_roots_marker_state_inactive_refusal_interpreter() {
    run_marker_state_guard_refusal(RootSeamFault::MarkerStateInactive, "MARKER_STATE_INACTIVE");
}

fn gray_decode_instructions(code: &[u8]) -> Result<Vec<(usize, vm::OpCode, Vec<u8>)>, String> {
    let mut out: Vec<(usize, vm::OpCode, Vec<u8>)> = Vec::new();
    let mut ip: usize = 0;
    while ip < code.len() {
        let raw: u8 = code[ip];
        let opcode: vm::OpCode = vm::OpCode::try_from(raw).map_err(|_| {
            format!("P7_GRAY_LAYOUT_DECODE: unknown opcode 0x{raw:02X} at offset {ip}")
        })?;
        let width: usize = opcode.operand_len();
        let end: usize = ip.saturating_add(1).saturating_add(width);
        if end > code.len() {
            return Err(format!(
                "P7_GRAY_LAYOUT_DECODE: truncated {opcode:?} at {ip} (code_len {})",
                code.len()
            ));
        }
        out.push((ip, opcode, code[ip + 1..end].to_vec()));
        ip = end;
    }
    Ok(out)
}

fn gray_ldc_pool_index(operands: &[u8], site: usize) -> Result<usize, String> {
    if operands.len() != 4 {
        return Err(format!(
            "P7_GRAY_LAYOUT_LDC: operand width {} != 4 at offset {site}",
            operands.len()
        ));
    }
    Ok(u32::from_le_bytes([operands[0], operands[1], operands[2], operands[3]]) as usize)
}

fn gray_const_at(program: &vm::Program, index: usize, site: usize) -> Result<&vm::Value, String> {
    program.constants.get(index).ok_or_else(|| {
        format!(
            "P7_GRAY_LAYOUT_LDC: pool index {index} out of range (pool {}) at {site}",
            program.constants.len()
        )
    })
}

fn gray_require_int_seed(value: &vm::Value, what: &str) -> Result<i64, String> {
    match value {
        vm::Value::Int(n) => Ok(*n),
        other => Err(format!(
            "P7_GRAY_LAYOUT_SEED: {what} must be Int, got {other:?}"
        )),
    }
}

fn gray_const_eq(expected: &vm::Value, actual: &vm::Value) -> Result<(), String> {
    match (expected, actual) {
        (vm::Value::Null, vm::Value::Null) => Ok(()),
        (vm::Value::Bool(a), vm::Value::Bool(b)) if a == b => Ok(()),
        (vm::Value::Int(a), vm::Value::Int(b)) if a == b => Ok(()),
        (vm::Value::Float(a), vm::Value::Float(b)) if a.to_bits() == b.to_bits() => Ok(()),
        (vm::Value::String(a), vm::Value::String(b)) if a == b => Ok(()),
        (vm::Value::Bytes(a), vm::Value::Bytes(b)) if a == b => Ok(()),
        (vm::Value::Array(_), _)
        | (_, vm::Value::Array(_))
        | (vm::Value::Map(_), _)
        | (_, vm::Value::Map(_))
        | (vm::Value::Callable(_), _)
        | (_, vm::Value::Callable(_)) => Err(format!(
            "P7_GRAY_LAYOUT_CONST: unsupported constant kind \
             (expected {expected:?}, actual {actual:?})"
        )),
        _ => Err(format!(
            "P7_GRAY_LAYOUT_CONST: constant mismatch \
             (expected {expected:?}, actual {actual:?})"
        )),
    }
}

fn gray_find_helper_callsite(
    insns: &[(usize, vm::OpCode, Vec<u8>)],
    helper_proto: u32,
    region_start: usize,
    region_end: usize,
) -> Result<usize, String> {
    let mut found: Option<usize> = None;
    for (i, (offset, opcode, operands)) in insns.iter().enumerate() {
        if *opcode != vm::OpCode::CallScript {
            continue;
        }
        if operands.len() != 5 {
            return Err(format!(
                "P7_GRAY_LAYOUT_CALL: bad CallScript width at offset {offset}"
            ));
        }
        let pid: u32 = u32::from_le_bytes([operands[0], operands[1], operands[2], operands[3]]);
        if pid != helper_proto {
            continue;
        }
        let argc: u8 = operands[4];
        let end: usize = offset.saturating_add(6);
        if *offset < region_start || end > region_end {
            return Err(format!(
                "P7_GRAY_LAYOUT_CALL: helper callsite at {offset} \
                 outside wrapper region [{region_start},{region_end})"
            ));
        }
        if argc != 4 {
            return Err(format!(
                "P7_GRAY_LAYOUT_CALL: helper callsite argc must be 4, got {argc} at {offset}"
            ));
        }
        if found.is_some() {
            return Err("P7_GRAY_LAYOUT_CALL: ambiguous duplicate helper callsites".to_string());
        }
        found = Some(i);
    }
    found.ok_or_else(|| "P7_GRAY_LAYOUT_CALL: no helper CallScript callsite in wrapper".to_string())
}

fn gray_compare_programs(
    full: &vm::Program,
    other: &vm::Program,
    wrapper_proto: u32,
    helper_proto: u32,
    mode: i64,
    target: i64,
) -> Result<(), String> {
    if mode != 1 && mode != 2 {
        return Err(format!(
            "P7_GRAY_LAYOUT_MODE: other mode must be 1 or 2, got {mode}"
        ));
    }
    if wrapper_proto == helper_proto {
        return Err(
            "P7_GRAY_LAYOUT_PROTO: wrapper and helper prototype ids must differ".to_string(),
        );
    }
    let wrapper_idx: usize = wrapper_proto as usize;
    let helper_idx: usize = helper_proto as usize;
    let full_wrapper = full.callable_prototypes.get(wrapper_idx).ok_or_else(|| {
        format!(
            "P7_GRAY_LAYOUT_PROTO: wrapper {wrapper_proto} out of range (full protos {})",
            full.callable_prototypes.len()
        )
    })?;
    let full_helper = full.callable_prototypes.get(helper_idx).ok_or_else(|| {
        format!(
            "P7_GRAY_LAYOUT_PROTO: helper {helper_proto} out of range (full protos {})",
            full.callable_prototypes.len()
        )
    })?;
    let other_wrapper = other.callable_prototypes.get(wrapper_idx).ok_or_else(|| {
        format!(
            "P7_GRAY_LAYOUT_PROTO: wrapper {wrapper_proto} out of range (other protos {})",
            other.callable_prototypes.len()
        )
    })?;
    let other_helper = other.callable_prototypes.get(helper_idx).ok_or_else(|| {
        format!(
            "P7_GRAY_LAYOUT_PROTO: helper {helper_proto} out of range (other protos {})",
            other.callable_prototypes.len()
        )
    })?;
    for (label, proto) in [("full-helper", full_helper), ("other-helper", other_helper)] {
        if proto.arity != 4 {
            return Err(format!(
                "P7_GRAY_LAYOUT_PROTO: {label} arity must be 4, got {}",
                proto.arity
            ));
        }
        if proto.parameter_slots.len() != 4 {
            return Err(format!(
                "P7_GRAY_LAYOUT_PROTO: {label} must carry 4 parameter slots, got {}",
                proto.parameter_slots.len()
            ));
        }
        if !matches!(proto.target, vm::CallableTarget::ScriptFunction(_)) {
            return Err(format!(
                "P7_GRAY_LAYOUT_PROTO: {label} target must be ScriptFunction"
            ));
        }
    }
    for (label, proto) in [
        ("full-wrapper", full_wrapper),
        ("other-wrapper", other_wrapper),
    ] {
        if !matches!(proto.target, vm::CallableTarget::ScriptFunction(_)) {
            return Err(format!(
                "P7_GRAY_LAYOUT_PROTO: {label} target must be ScriptFunction"
            ));
        }
    }
    if full.local_count != other.local_count {
        return Err("P7_GRAY_LAYOUT_META: local_count differs".to_string());
    }
    if full.imports != other.imports {
        return Err("P7_GRAY_LAYOUT_META: imports differ".to_string());
    }
    if full.host_import_schemas() != other.host_import_schemas() {
        return Err("P7_GRAY_LAYOUT_META: host import schemas differ".to_string());
    }
    if full.named_struct_decls() != other.named_struct_decls() {
        return Err("P7_GRAY_LAYOUT_META: named struct decls differ".to_string());
    }
    if full.debug != other.debug {
        return Err("P7_GRAY_LAYOUT_META: debug labels differ".to_string());
    }
    if full.type_map != other.type_map {
        return Err("P7_GRAY_LAYOUT_META: type-map hints differ".to_string());
    }
    if full.script_functions != other.script_functions {
        return Err("P7_GRAY_LAYOUT_META: script-function ranges differ".to_string());
    }
    if full.callable_prototypes != other.callable_prototypes {
        return Err("P7_GRAY_LAYOUT_META: callable prototypes differ".to_string());
    }
    if full.function_regions != other.function_regions {
        return Err("P7_GRAY_LAYOUT_META: function regions differ".to_string());
    }
    if full.root_callable_bindings != other.root_callable_bindings {
        return Err("P7_GRAY_LAYOUT_META: root callable bindings differ".to_string());
    }
    if full.exported_callables != other.exported_callables {
        return Err("P7_GRAY_LAYOUT_META: exported callables differ".to_string());
    }
    if full.code.len() != other.code.len() {
        return Err("P7_GRAY_LAYOUT_META: code length differs".to_string());
    }
    let wrapper_fn: usize = match full_wrapper.target {
        vm::CallableTarget::ScriptFunction(i) => i as usize,
        _ => {
            return Err(
                "P7_GRAY_LAYOUT_PROTO: full wrapper target must be ScriptFunction".to_string(),
            );
        }
    };
    let helper_fn: usize = match full_helper.target {
        vm::CallableTarget::ScriptFunction(i) => i as usize,
        _ => {
            return Err(
                "P7_GRAY_LAYOUT_PROTO: full helper target must be ScriptFunction".to_string(),
            );
        }
    };
    if wrapper_fn == helper_fn {
        return Err(
            "P7_GRAY_LAYOUT_PROTO: wrapper and helper must own distinct functions".to_string(),
        );
    }
    let wrapper_range = full.script_functions.get(wrapper_fn).ok_or_else(|| {
        format!("P7_GRAY_LAYOUT_PROTO: wrapper function {wrapper_fn} out of range")
    })?;
    let helper_range = full
        .script_functions
        .get(helper_fn)
        .ok_or_else(|| format!("P7_GRAY_LAYOUT_PROTO: helper function {helper_fn} out of range"))?;
    let ws: usize = wrapper_range.entry_ip as usize;
    let we: usize = wrapper_range.end_ip as usize;
    let hs: usize = helper_range.entry_ip as usize;
    let he: usize = helper_range.end_ip as usize;
    if ws >= we || we > full.code.len() {
        return Err("P7_GRAY_LAYOUT_REGION: wrapper range outside code".to_string());
    }
    if hs >= he || he > full.code.len() {
        return Err("P7_GRAY_LAYOUT_REGION: helper range outside code".to_string());
    }
    if !(we <= hs || he <= ws) {
        return Err("P7_GRAY_LAYOUT_REGION: wrapper and helper ranges overlap".to_string());
    }
    let wrapper_owned: Vec<&vm::FunctionRegion> = full
        .function_regions
        .iter()
        .filter(|r| r.prototype_id == Some(wrapper_proto))
        .collect();
    if wrapper_owned.len() != 1 {
        return Err(format!(
            "P7_GRAY_LAYOUT_REGION: wrapper {wrapper_proto} must own exactly one region, found {}",
            wrapper_owned.len()
        ));
    }
    if wrapper_owned[0].start_ip as usize != ws || wrapper_owned[0].end_ip as usize != we {
        return Err("P7_GRAY_LAYOUT_REGION: wrapper region must match script range".to_string());
    }
    let helper_owned: Vec<&vm::FunctionRegion> = full
        .function_regions
        .iter()
        .filter(|r| r.prototype_id == Some(helper_proto))
        .collect();
    if helper_owned.len() != 1 {
        return Err(format!(
            "P7_GRAY_LAYOUT_REGION: helper {helper_proto} must own exactly one region, found {}",
            helper_owned.len()
        ));
    }
    if helper_owned[0].start_ip as usize != hs || helper_owned[0].end_ip as usize != he {
        return Err("P7_GRAY_LAYOUT_REGION: helper region must match script range".to_string());
    }
    let full_insns = gray_decode_instructions(&full.code)?;
    let other_insns = gray_decode_instructions(&other.code)?;
    if full_insns.len() != other_insns.len() {
        return Err("P7_GRAY_LAYOUT_DECODE: decoded instruction counts differ".to_string());
    }
    let full_call = gray_find_helper_callsite(&full_insns, helper_proto, ws, we)?;
    let other_call = gray_find_helper_callsite(&other_insns, helper_proto, ws, we)?;
    let full_call_off: usize = full_insns[full_call].0;
    let other_call_off: usize = other_insns[other_call].0;
    if full_call_off != other_call_off {
        return Err("P7_GRAY_LAYOUT_CALL: helper callsite offsets differ".to_string());
    }
    if full_call < 2 || other_call < 2 {
        return Err("P7_GRAY_LAYOUT_SEED: callsite too early for two seed producers".to_string());
    }
    for (label, insns, call) in [
        ("full", &full_insns, full_call),
        ("other", &other_insns, other_call),
    ] {
        let (m_off, m_op, _) = &insns[call - 2];
        let (t_off, t_op, _) = &insns[call - 1];
        if *m_op != vm::OpCode::Ldc || *t_op != vm::OpCode::Ldc {
            return Err(format!(
                "P7_GRAY_LAYOUT_SEED: {label} final two producers must be Ldc"
            ));
        }
        if m_off.saturating_add(5) != *t_off || t_off.saturating_add(5) != full_call_off {
            return Err(format!(
                "P7_GRAY_LAYOUT_SEED: {label} seeds must be adjacent before callstart"
            ));
        }
        if *m_off < ws || *t_off < ws {
            return Err(format!(
                "P7_GRAY_LAYOUT_SEED: {label} seeds must sit inside the wrapper region"
            ));
        }
    }
    let full_mode_off: usize = full_insns[full_call - 2].0;
    let full_target_off: usize = full_insns[full_call - 1].0;
    let other_mode_off: usize = other_insns[other_call - 2].0;
    let other_target_off: usize = other_insns[other_call - 1].0;
    let full_mode_idx = gray_ldc_pool_index(&full_insns[full_call - 2].2, full_mode_off)?;
    let full_target_idx = gray_ldc_pool_index(&full_insns[full_call - 1].2, full_target_off)?;
    let other_mode_idx = gray_ldc_pool_index(&other_insns[other_call - 2].2, other_mode_off)?;
    let other_target_idx = gray_ldc_pool_index(&other_insns[other_call - 1].2, other_target_off)?;
    let full_mode_seed = gray_require_int_seed(
        gray_const_at(full, full_mode_idx, full_mode_off)?,
        "full mode",
    )?;
    let full_target_seed = gray_require_int_seed(
        gray_const_at(full, full_target_idx, full_target_off)?,
        "full target",
    )?;
    if full_mode_seed != 0 || full_target_seed != 0 {
        return Err("P7_GRAY_LAYOUT_SEED: full seeds must be exactly 0/0".to_string());
    }
    let other_mode_seed = gray_require_int_seed(
        gray_const_at(other, other_mode_idx, other_mode_off)?,
        "other mode",
    )?;
    let other_target_seed = gray_require_int_seed(
        gray_const_at(other, other_target_idx, other_target_off)?,
        "other target",
    )?;
    if other_mode_seed != mode || other_target_seed != target {
        return Err(format!(
            "P7_GRAY_LAYOUT_SEED: other seeds must be exactly {mode}/{target}"
        ));
    }
    for ((f_off, f_op, f_ops), (o_off, o_op, o_ops)) in full_insns.iter().zip(other_insns.iter()) {
        if f_off != o_off || f_op != o_op {
            return Err(format!(
                "P7_GRAY_LAYOUT_CODE: instruction mismatch at full offset {f_off} \
                 vs other offset {o_off}"
            ));
        }
        if *f_op == vm::OpCode::Ldc {
            if *f_off == full_mode_off || *f_off == full_target_off {
                continue;
            }
            let f_idx = gray_ldc_pool_index(f_ops, *f_off)?;
            let o_idx = gray_ldc_pool_index(o_ops, *o_off)?;
            let f_val = gray_const_at(full, f_idx, *f_off)?;
            let o_val = gray_const_at(other, o_idx, *o_off)?;
            gray_const_eq(f_val, o_val)
                .map_err(|e| format!("{e} at instruction offset {f_off}"))?;
        } else if f_ops != o_ops {
            return Err(format!(
                "P7_GRAY_LAYOUT_OPERAND: {f_op:?} operand bytes differ at offset {f_off}"
            ));
        }
    }
    Ok(())
}

fn p7_d01_gray_program_layout_fixture(mode: i64, target: i64, nonseed: i64) -> vm::Program {
    use vm::{CallableKind as Kind, CallableTarget as Target};
    let constants = vec![
        vm::Value::Int(mode),
        vm::Value::Int(target),
        vm::Value::Int(nonseed),
        vm::Value::Int(5),
    ];
    let mut code: Vec<u8> = vec![
        0x02, 2, 0, 0, 0, // Ldc const[2] (nonseed filler A)
        0x02, 3, 0, 0, 0, // Ldc const[3] (filler B, identical both sides)
        0x02, 0, 0, 0, 0, // Ldc const[0] (mode seed)
        0x02, 1, 0, 0, 0, // Ldc const[1] (target seed)
        0x1A, 1, 0, 0, 0, 4,    // CallScript helper_proto=1 argc=4
        0x01, // Ret
    ];
    let wrapper_end = code.len() as u32;
    code.extend_from_slice(&[
        0x02, 3, 0, 0, 0,    // Ldc const[3] (helper body, identical both sides)
        0x01, // Ret
    ]);
    let helper_start = wrapper_end;
    let helper_end = code.len() as u32;
    let helper_schema = vm::TypeSchema::Callable {
        params: vec![
            vm::TypeSchema::Int,
            vm::TypeSchema::Int,
            vm::TypeSchema::Int,
            vm::TypeSchema::Int,
        ],
        result: Box::new(vm::TypeSchema::Int),
    };
    let prototypes = vec![
        vm::CallablePrototype {
            kind: Kind::FunctionItem,
            target: Target::ScriptFunction(0),
            arity: 0,
            frame_local_count: 16,
            parameter_slots: Vec::new(),
            capture_source_slots: Vec::new(),
            capture_slots: Vec::new(),
            capture_modes: Vec::new(),
            self_slot: None,
            schema: None,
        },
        vm::CallablePrototype {
            kind: Kind::FunctionItem,
            target: Target::ScriptFunction(1),
            arity: 4,
            frame_local_count: 16,
            parameter_slots: vec![10, 11, 12, 13],
            capture_source_slots: Vec::new(),
            capture_slots: Vec::new(),
            capture_modes: Vec::new(),
            self_slot: None,
            schema: Some(helper_schema),
        },
    ];
    vm::Program::new(constants, code)
        .with_local_count(16)
        .with_callable_metadata(
            vec![
                vm::ScriptFunction {
                    entry_ip: 0,
                    end_ip: wrapper_end,
                },
                vm::ScriptFunction {
                    entry_ip: helper_start,
                    end_ip: helper_end,
                },
            ],
            prototypes,
            vec![
                vm::FunctionRegion {
                    start_ip: 0,
                    end_ip: wrapper_end,
                    prototype_id: Some(0),
                },
                vm::FunctionRegion {
                    start_ip: helper_start,
                    end_ip: helper_end,
                    prototype_id: Some(1),
                },
            ],
            Vec::new(),
        )
}

#[test]
fn p7_d01_gray_program_layout_rejects_nonseed_constant() {
    let full = p7_d01_gray_program_layout_fixture(0, 0, 42);
    let allowed = p7_d01_gray_program_layout_fixture(1, 7, 42);
    assert_eq!(full.code.len(), allowed.code.len(), "fixture topology");
    let allowed_verdict = gray_compare_programs(&full, &allowed, 0, 1, 1, 7);
    assert!(
        allowed_verdict.is_ok(),
        "P7_GRAY_LAYOUT_SEED_ONLY_MUST_ACCEPT: seed-only pair must be accepted, got {allowed_verdict:?}"
    );
    let other = p7_d01_gray_program_layout_fixture(1, 7, 99);
    assert_eq!(full.code.len(), other.code.len(), "fixture topology");
    assert_eq!(
        full.constants.len(),
        other.constants.len(),
        "fixture constants len"
    );
    let verdict = gray_compare_programs(&full, &other, 0, 1, 1, 7);
    assert!(
        verdict.is_err(),
        "P7_GRAY_LAYOUT_NONSEED_MUST_REJECT: identical-shape pair with changed nonseed constant must be rejected, got {verdict:?}"
    );
}

fn p7_d01_gray_swapped_pool_fixture(mode: i64, target: i64, nonseed: i64) -> vm::Program {
    let mut program = p7_d01_gray_program_layout_fixture(mode, target, nonseed);
    program.constants.swap(2, 3);
    for byte in [1usize, 6, 28] {
        if program.code[byte] == 2 {
            program.code[byte] = 3;
        } else if program.code[byte] == 3 {
            program.code[byte] = 2;
        }
    }
    program
}

fn p7_d01_gray_float_fixture(mode: i64, target: i64, filler_bits: u64) -> vm::Program {
    let mut program = p7_d01_gray_program_layout_fixture(mode, target, 0);
    program.constants[2] = vm::Value::Float(f64::from_bits(filler_bits));
    program.callable_prototypes[1].schema = Some(vm::TypeSchema::Callable {
        params: vec![
            vm::TypeSchema::Float,
            vm::TypeSchema::Int,
            vm::TypeSchema::Int,
            vm::TypeSchema::Int,
        ],
        result: Box::new(vm::TypeSchema::Int),
    });
    program
}

#[test]
fn p7_d01_gray_program_layout_controls() {
    let full = p7_d01_gray_program_layout_fixture(0, 0, 42);
    for (mode, target) in [(1i64, 7i64), (2i64, 7i64)] {
        let other = p7_d01_gray_program_layout_fixture(mode, target, 42);
        assert!(
            gray_compare_programs(&full, &other, 0, 1, mode, target).is_ok(),
            "P7_GRAY_LAYOUT_CONTROLS: seed-only mode={mode} target={target} must accept"
        );
    }
    {
        let other = p7_d01_gray_swapped_pool_fixture(1, 7, 42);
        assert!(
            gray_compare_programs(&full, &other, 0, 1, 1, 7).is_ok(),
            "P7_GRAY_LAYOUT_CONTROLS: pool-permutation must accept"
        );
    }
    {
        let bits: u64 = 0x7FF8_0000_0000_0001;
        let full_f = p7_d01_gray_float_fixture(0, 0, bits);
        let other_f = p7_d01_gray_float_fixture(1, 7, bits);
        assert!(
            gray_compare_programs(&full_f, &other_f, 0, 1, 1, 7).is_ok(),
            "P7_GRAY_LAYOUT_CONTROLS: float-same-nan-bits must accept"
        );
    }
    {
        let other = p7_d01_gray_program_layout_fixture(1, 7, 99);
        assert!(
            gray_compare_programs(&full, &other, 0, 1, 1, 7).is_err(),
            "P7_GRAY_LAYOUT_CONTROLS: nonseed-int-change must reject"
        );
    }
    {
        let full_f = p7_d01_gray_float_fixture(0, 0, 0x7FF8_0000_0000_0001);
        let other_f = p7_d01_gray_float_fixture(1, 7, 0x7FF8_0000_0000_0002);
        assert!(
            gray_compare_programs(&full_f, &other_f, 0, 1, 1, 7).is_err(),
            "P7_GRAY_LAYOUT_CONTROLS: float-different-nan-payload must reject"
        );
    }
    {
        let full_f = p7_d01_gray_float_fixture(0, 0, 0);
        let other_f = p7_d01_gray_float_fixture(1, 7, 0x8000_0000_0000_0000);
        assert!(
            gray_compare_programs(&full_f, &other_f, 0, 1, 1, 7).is_err(),
            "P7_GRAY_LAYOUT_CONTROLS: float-pos-zero-vs-neg-zero must reject"
        );
    }
    {
        let mut other = p7_d01_gray_program_layout_fixture(1, 7, 42);
        other.code[0] = 0x03;
        assert!(
            gray_compare_programs(&full, &other, 0, 1, 1, 7).is_err(),
            "P7_GRAY_LAYOUT_CONTROLS: opcode-ldc-to-add must reject"
        );
    }
    {
        let mut other = p7_d01_gray_program_layout_fixture(1, 7, 42);
        other.code[26] = 0x00;
        assert!(
            gray_compare_programs(&full, &other, 0, 1, 1, 7).is_err(),
            "P7_GRAY_LAYOUT_CONTROLS: opcode-ret-to-nop must reject"
        );
    }
    {
        let mut other = p7_d01_gray_program_layout_fixture(1, 7, 42);
        assert_eq!(other.code.pop(), Some(0x01));
        assert!(
            gray_compare_programs(&full, &other, 0, 1, 1, 7).is_err(),
            "P7_GRAY_LAYOUT_CONTROLS: truncation must reject"
        );
    }
    {
        let mut other = p7_d01_gray_program_layout_fixture(1, 7, 42);
        other.code[1] = 99;
        assert!(
            gray_compare_programs(&full, &other, 0, 1, 1, 7).is_err(),
            "P7_GRAY_LAYOUT_CONTROLS: pool-index-out-of-range must reject"
        );
    }
    {
        let mut other = p7_d01_gray_program_layout_fixture(1, 7, 42);
        other.code[25] = 3;
        assert!(
            gray_compare_programs(&full, &other, 0, 1, 1, 7).is_err(),
            "P7_GRAY_LAYOUT_CONTROLS: callscript-argc must reject"
        );
    }
    {
        let mut other = p7_d01_gray_program_layout_fixture(1, 7, 42);
        other.code[21] = 0;
        assert!(
            gray_compare_programs(&full, &other, 0, 1, 1, 7).is_err(),
            "P7_GRAY_LAYOUT_CONTROLS: callscript-target must reject"
        );
    }
    {
        let other = p7_d01_gray_program_layout_fixture(1, 8, 42);
        assert!(
            gray_compare_programs(&full, &other, 0, 1, 1, 7).is_err(),
            "P7_GRAY_LAYOUT_CONTROLS: wrong-seed-value must reject"
        );
    }
    {
        let mut other = p7_d01_gray_program_layout_fixture(1, 7, 42);
        other.code.swap(11, 16);
        assert!(
            gray_compare_programs(&full, &other, 0, 1, 1, 7).is_err(),
            "P7_GRAY_LAYOUT_CONTROLS: seed-order-swapped must reject"
        );
    }
    {
        let mut full_b = p7_d01_gray_program_layout_fixture(0, 0, 42);
        let mut other_b = p7_d01_gray_program_layout_fixture(1, 7, 42);
        full_b.code[0] = 0xFF;
        other_b.code[0] = 0xFF;
        assert!(
            gray_compare_programs(&full_b, &other_b, 0, 1, 1, 7).is_err(),
            "P7_GRAY_LAYOUT_CONTROLS: malformed-both-same must reject"
        );
    }
    {
        let mut other = p7_d01_gray_program_layout_fixture(1, 7, 42);
        other.code.insert(20, 0x00);
        assert!(
            gray_compare_programs(&full, &other, 0, 1, 1, 7).is_err(),
            "P7_GRAY_LAYOUT_CONTROLS: layout-extra-nop-before-call must reject"
        );
    }
    {
        let mut other = p7_d01_gray_program_layout_fixture(1, 7, 42);
        other.local_count = 999;
        assert!(
            gray_compare_programs(&full, &other, 0, 1, 1, 7).is_err(),
            "P7_GRAY_LAYOUT_CONTROLS: meta-local-count must reject"
        );
    }
    {
        let mut other = p7_d01_gray_program_layout_fixture(1, 7, 42);
        other.callable_prototypes[1].arity = 3;
        assert!(
            gray_compare_programs(&full, &other, 0, 1, 1, 7).is_err(),
            "P7_GRAY_LAYOUT_CONTROLS: meta-proto-arity must reject"
        );
    }
    {
        let mut other = p7_d01_gray_program_layout_fixture(1, 7, 42);
        other.callable_prototypes[1].parameter_slots[0] = 99;
        assert!(
            gray_compare_programs(&full, &other, 0, 1, 1, 7).is_err(),
            "P7_GRAY_LAYOUT_CONTROLS: meta-parameter-slot must reject"
        );
    }
    {
        let mut other = p7_d01_gray_program_layout_fixture(1, 7, 42);
        other.callable_prototypes[1].schema = None;
        assert!(
            gray_compare_programs(&full, &other, 0, 1, 1, 7).is_err(),
            "P7_GRAY_LAYOUT_CONTROLS: meta-schema must reject"
        );
    }
    {
        let mut other = p7_d01_gray_program_layout_fixture(1, 7, 42);
        other.function_regions[1].start_ip = 0;
        assert!(
            gray_compare_programs(&full, &other, 0, 1, 1, 7).is_err(),
            "P7_GRAY_LAYOUT_CONTROLS: meta-region must reject"
        );
    }
    {
        let mut other = p7_d01_gray_program_layout_fixture(1, 7, 42);
        other.script_functions[1].entry_ip = 0;
        assert!(
            gray_compare_programs(&full, &other, 0, 1, 1, 7).is_err(),
            "P7_GRAY_LAYOUT_CONTROLS: meta-script-function must reject"
        );
    }
    {
        let mut other = p7_d01_gray_program_layout_fixture(1, 7, 42);
        other.type_map = Some(vm::TypeMap::default());
        assert!(
            gray_compare_programs(&full, &other, 0, 1, 1, 7).is_err(),
            "P7_GRAY_LAYOUT_CONTROLS: meta-type-map must reject"
        );
    }
    {
        let mut other = p7_d01_gray_program_layout_fixture(1, 7, 42);
        other.debug = Some(vm::DebugInfo {
            source: Some("gray".to_string()),
            lines: Vec::new(),
            functions: Vec::new(),
            locals: Vec::new(),
        });
        assert!(
            gray_compare_programs(&full, &other, 0, 1, 1, 7).is_err(),
            "P7_GRAY_LAYOUT_CONTROLS: meta-debug must reject"
        );
    }
}

#[test]
fn p7_d01_gray_program_layout_allows_seed_pool_growth() {
    // Full seeds are both Int(0); an exact constant-pool dedup drops the
    // duplicate entry and renumbers every Ldc operand. Other stays untouched.
    let mut full = p7_d01_gray_program_layout_fixture(0, 0, 42);
    let other = p7_d01_gray_program_layout_fixture(1, 7, 42);
    let decoded = gray_decode_instructions(&full.code).expect("fixture decodes");
    full.constants.remove(1);
    for (offset, opcode, operands) in &decoded {
        if *opcode != vm::OpCode::Ldc {
            continue;
        }
        let old = gray_ldc_pool_index(operands, *offset).expect("fixture ldc");
        let new = old.saturating_sub(1);
        let bytes = (new as u32).to_le_bytes();
        let start = *offset + 1;
        full.code[start..start + 4].copy_from_slice(&bytes);
    }
    assert_eq!(full.code.len(), other.code.len(), "fixture code len");
    assert_eq!(
        full.constants.len() + 1,
        other.constants.len(),
        "fixture pool dedup"
    );
    let verdict = gray_compare_programs(&full, &other, 0, 1, 1, 7);
    assert!(
        verdict.is_ok(),
        "P7_GRAY_LAYOUT_SEED_POOL_GROWTH_MUST_ACCEPT: seed-induced constant-pool growth must be accepted, got {verdict:?}"
    );
}

// ---------------------------------------------------------------------------
// P7 D01 Gray step 2: bounded pure Assert-endpoint witness.
// Synthetic in-memory `vm::Program` metadata only: nothing here is compiled
// from source, installed in a host, or executed by a VM.
// ---------------------------------------------------------------------------

/// Bounded pure witness for the Gray drain stop.
///
/// The supplied range must be the unique `function_regions` owner (with a
/// concrete prototype) and must not be partially overlapped by another region;
/// `region_start < ip <= region_end`; the byte range `[region_start, ip)` must
/// decode as complete instructions whose last one ends exactly at `ip` and is
/// `Call(BuiltinFunction::Assert, argc 1)`. The debug label effective at the
/// *Assert start* (not at `ip`) must equal `expected_label` and must originate
/// inside the owned region, so a floor lookup cannot inherit a neighbouring
/// phase's label. The program is never mutated and no VM state is consulted.
fn gray_assert_site(
    program: &vm::Program,
    region_start: u32,
    region_end: u32,
    ip: usize,
    expected_label: u32,
) -> Result<usize, String> {
    let code = &program.code;
    let start = region_start as usize;
    let end = region_end as usize;
    if start >= end || end > code.len() || start >= ip || ip > end {
        return Err(format!(
            "P7_GRAY_ENDPOINT_BOUNDS: range [{start}, {end}) / ip {ip} violates \
             start < end <= code_len {} and start < ip <= end",
            code.len()
        ));
    }
    let mut owner: Option<&vm::FunctionRegion> = None;
    let mut owned = 0usize;
    for region in &program.function_regions {
        if region.start_ip as usize == start && region.end_ip as usize == end {
            owned += 1;
            owner = Some(region);
        }
    }
    let owner = match (owned, owner) {
        (1, Some(owner)) => owner,
        (0, _) => {
            return Err(format!(
                "P7_GRAY_ENDPOINT_OWNER: [{start}, {end}) is not an actual function region"
            ));
        }
        (count, _) => {
            return Err(format!(
                "P7_GRAY_ENDPOINT_OWNER: [{start}, {end}) matches {count} regions (ambiguous)"
            ));
        }
    };
    if owner.prototype_id.is_none() {
        return Err(format!(
            "P7_GRAY_ENDPOINT_OWNER: [{start}, {end}) has no callable prototype"
        ));
    }
    for region in &program.function_regions {
        if region.start_ip as usize == start && region.end_ip as usize == end {
            continue;
        }
        let other_start = region.start_ip as usize;
        let other_end = region.end_ip as usize;
        if other_start < end && start < other_end {
            return Err(format!(
                "P7_GRAY_ENDPOINT_OWNER: region [{other_start}, {other_end}) overlaps the \
                 supplied range [{start}, {end})"
            ));
        }
    }

    let mut cursor = start;
    let mut last: Option<(usize, vm::OpCode)> = None;
    while cursor < ip {
        let raw = code[cursor];
        let op = vm::OpCode::try_from(raw).map_err(|_| {
            format!("P7_GRAY_ENDPOINT_DECODE: unknown opcode 0x{raw:02X} at {cursor}")
        })?;
        let next = cursor
            .checked_add(1 + op.operand_len())
            .ok_or_else(|| format!("P7_GRAY_ENDPOINT_DECODE: width overflow at {cursor}"))?;
        if next > ip {
            return Err(format!(
                "P7_GRAY_ENDPOINT_DECODE: {op:?} at {cursor} crosses the endpoint ip {ip}"
            ));
        }
        last = Some((cursor, op));
        cursor = next;
    }
    let (assert_start, op) = last.ok_or_else(|| {
        format!("P7_GRAY_ENDPOINT_DECODE: no instruction ends at ip {ip} within [{start}, {end})")
    })?;
    if !matches!(op, vm::OpCode::Call) || op.operand_len() != 3 {
        return Err(format!(
            "P7_GRAY_ENDPOINT_SHAPE: last instruction at {assert_start} is {op:?}, not a \
             builtin Call with a 3-byte operand"
        ));
    }
    let target = u16::from_le_bytes([code[assert_start + 1], code[assert_start + 2]]);
    if target != vm::BuiltinFunction::Assert.call_index() {
        return Err(format!(
            "P7_GRAY_ENDPOINT_SHAPE: call target {target} is not BuiltinFunction::Assert ({})",
            vm::BuiltinFunction::Assert.call_index()
        ));
    }
    if code[assert_start + 3] != 1 {
        return Err(format!(
            "P7_GRAY_ENDPOINT_SHAPE: assert argc {} != 1 at {assert_start}",
            code[assert_start + 3]
        ));
    }

    let debug = program
        .debug
        .as_ref()
        .ok_or_else(|| "P7_GRAY_ENDPOINT_DEBUG: program has no debug info".to_string())?;
    let mapped = debug.line_for_offset(assert_start);
    if mapped != Some(expected_label) {
        return Err(format!(
            "P7_GRAY_ENDPOINT_DEBUG: Assert start {assert_start} maps to {mapped:?}, not the \
             expected label {expected_label}"
        ));
    }
    let entry = debug
        .lines
        .iter()
        .filter(|info| info.offset as usize <= assert_start)
        .max_by_key(|info| info.offset)
        .ok_or_else(|| {
            format!("P7_GRAY_ENDPOINT_DEBUG: Assert start {assert_start} has no line entry")
        })?;
    if entry.line != expected_label {
        return Err(format!(
            "P7_GRAY_ENDPOINT_DEBUG: effective entry at offset {} carries label {}, not \
             {expected_label}",
            entry.offset, entry.line
        ));
    }
    let origin = entry.offset as usize;
    if !(start..end).contains(&origin) {
        return Err(format!(
            "P7_GRAY_ENDPOINT_DEBUG: effective label {expected_label} originates at offset \
             {origin}, outside the owner region [{start}, {end})"
        ));
    }
    Ok(assert_start)
}

/// SYNTHETIC Gray Assert-endpoint fixture. It reuses the owned
/// `p7_d01_gray_program_layout_fixture(0, 0, 42)` base, preserving its wrapper,
/// prototype 1 arity 4 / frame 16 / parameter slots, and disjoint regions, and
/// replaces only the owned helper body with
/// `Ldc(bool false) -> Call(Assert, argc 1) -> Ldc(Int 5) -> Ret`.
///
/// The `bool false` argument is never executed; the trailing `Ldc(Int 5)`/`Ret`
/// keeps the helper prototype's Int result schema consistent. All pool indices
/// and instruction boundaries are derived from the actual constant pool and
/// code length. Debug lines intentionally floor-map the instruction after the
/// Assert to the sweep phase (900014), so a global floor lookup at the failure
/// IP would attribute the next phase to the Assert endpoint.
fn p7_d01_gray_assert_endpoint_fixture() -> (vm::Program, u32, u32, usize, usize) {
    let mut program = p7_d01_gray_program_layout_fixture(0, 0, 42);
    let helper_start = program.function_regions[1].start_ip as usize;
    let five_index = program
        .constants
        .iter()
        .position(|value| matches!(value, vm::Value::Int(5)))
        .expect("base fixture Int(5) constant") as u32;
    program.code.truncate(helper_start);
    let false_index = program.constants.len() as u32;
    program.constants.push(vm::Value::Bool(false));
    let mut code = std::mem::take(&mut program.code);
    code.push(vm::OpCode::Ldc as u8);
    code.extend_from_slice(&false_index.to_le_bytes());
    let assert_start = code.len();
    code.push(vm::OpCode::Call as u8);
    code.extend_from_slice(&vm::BuiltinFunction::Assert.call_index().to_le_bytes());
    code.push(1);
    let assert_end = code.len();
    code.push(vm::OpCode::Ldc as u8);
    code.extend_from_slice(&five_index.to_le_bytes());
    code.push(vm::OpCode::Ret as u8);
    program.code = code;
    let helper_end = program.code.len() as u32;
    program.script_functions[1].end_ip = helper_end;
    program.function_regions[1].end_ip = helper_end;
    program.debug = Some(vm::DebugInfo {
        source: Some("synthetic-gray-assert-endpoint".to_string()),
        lines: vec![
            vm::LineInfo {
                offset: assert_start as u32,
                line: 910003,
            },
            vm::LineInfo {
                offset: assert_end as u32,
                line: 900014,
            },
        ],
        functions: Vec::new(),
        locals: Vec::new(),
    });
    (
        program,
        helper_start as u32,
        helper_end,
        assert_start,
        assert_end,
    )
}

#[test]
fn p7_d01_gray_endpoint_witness_rejects_wrong_label() {
    let (program, region_start, region_end, assert_start, assert_end) =
        p7_d01_gray_assert_endpoint_fixture();
    let debug = program.debug.as_ref().expect("synthetic debug info");
    assert_eq!(
        debug.line_for_offset(assert_start),
        Some(910003),
        "fixture precondition: the actual Assert start is the gray-drain entry stop"
    );
    assert_eq!(
        debug.line_for_offset(assert_end),
        Some(900014),
        "fixture precondition: the post-Assert IP floor belongs to the sweep phase"
    );
    let accepted = gray_assert_site(&program, region_start, region_end, assert_end, 910003);
    assert!(
        accepted.is_ok(),
        "P7_GRAY_ENDPOINT_CORRECT_LABEL_MUST_ACCEPT: the actual Assert start must accept its own expected label, got {accepted:?}"
    );
    let rejected = gray_assert_site(&program, region_start, region_end, assert_end, 910101);
    assert!(
        rejected.is_err(),
        "P7_GRAY_ENDPOINT_WRONG_LABEL_MUST_REJECT: the sweep floor label must not satisfy the Assert-start witness, got {rejected:?}"
    );
}

/// Finite pure controls for the bounded Assert-endpoint witness. Every case
/// builds its own synthetic `vm::Program` in RAM; no source is parsed, no host
/// is installed and no VM is created or run.
#[test]
fn p7_d01_gray_endpoint_witness_controls() {
    fn bind_helper_end(program: &mut vm::Program, start: usize, end: usize) {
        let region = program
            .function_regions
            .iter()
            .position(|region| region.prototype_id == Some(1) && region.start_ip as usize == start)
            .expect("helper function region");
        program.function_regions[region].end_ip = end as u32;
        let function = program
            .script_functions
            .iter_mut()
            .find(|function| function.entry_ip as usize == start)
            .expect("helper script function");
        function.end_ip = end as u32;
    }

    let (program, region_start, region_end, assert_start, assert_end) =
        p7_d01_gray_assert_endpoint_fixture();
    let start = region_start as usize;

    let accepted = gray_assert_site(&program, region_start, region_end, assert_end, 910003);
    assert_eq!(
        accepted,
        Ok(assert_start),
        "P7_GRAY_ENDPOINT_CONTROL_ACCEPT: the correct entry label must accept and return the actual Assert start"
    );
    assert!(
        gray_assert_site(&program, region_start, region_end, assert_end, 910101).is_err(),
        "P7_GRAY_ENDPOINT_CONTROL_WRONG_LABEL: a foreign label must be rejected"
    );
    assert!(
        gray_assert_site(&program, region_start, region_end, assert_end, 900014).is_err(),
        "P7_GRAY_ENDPOINT_CONTROL_FLOOR_LABEL: the label floored at ip must not be accepted as the Assert-start label"
    );

    let (mut variant, ..) = p7_d01_gray_assert_endpoint_fixture();
    variant.debug = None;
    assert!(
        gray_assert_site(&variant, region_start, region_end, assert_end, 910003).is_err(),
        "P7_GRAY_ENDPOINT_CONTROL_MISSING_DEBUG: a program without debug info must be rejected"
    );

    let (mut variant, ..) = p7_d01_gray_assert_endpoint_fixture();
    variant.debug.as_mut().expect("fixture debug").lines = vec![vm::LineInfo {
        offset: assert_end as u32,
        line: 910003,
    }];
    assert!(
        gray_assert_site(&variant, region_start, region_end, assert_end, 910003).is_err(),
        "P7_GRAY_ENDPOINT_CONTROL_UNMAPPED: an Assert start with no effective line entry must be rejected"
    );

    let (mut variant, ..) = p7_d01_gray_assert_endpoint_fixture();
    variant
        .debug
        .as_mut()
        .expect("fixture debug")
        .lines
        .iter_mut()
        .find(|info| info.offset as usize == assert_start)
        .expect("assert-start line entry")
        .line = 900014;
    assert!(
        gray_assert_site(&variant, region_start, region_end, assert_end, 910003).is_err(),
        "P7_GRAY_ENDPOINT_CONTROL_WRONG_ENTRY_LABEL: a mismatched entry label at the Assert start must be rejected"
    );

    let (mut variant, ..) = p7_d01_gray_assert_endpoint_fixture();
    variant.debug.as_mut().expect("fixture debug").lines = vec![
        vm::LineInfo {
            offset: 0,
            line: 910003,
        },
        vm::LineInfo {
            offset: assert_end as u32,
            line: 900014,
        },
    ];
    assert!(
        gray_assert_site(&variant, region_start, region_end, assert_end, 910003).is_err(),
        "P7_GRAY_ENDPOINT_CONTROL_INHERITED_LABEL: a label floored in from before the helper start must be rejected"
    );

    let (mut variant, ..) = p7_d01_gray_assert_endpoint_fixture();
    assert_ne!(
        vm::BuiltinFunction::Get.call_index(),
        vm::BuiltinFunction::Assert.call_index(),
        "fixture precondition: a distinct builtin for the wrong-target control"
    );
    variant.code[assert_start + 1..assert_start + 3]
        .copy_from_slice(&vm::BuiltinFunction::Get.call_index().to_le_bytes());
    assert!(
        gray_assert_site(&variant, region_start, region_end, assert_end, 910003).is_err(),
        "P7_GRAY_ENDPOINT_CONTROL_WRONG_BUILTIN: a non-Assert call target must be rejected"
    );

    let (mut variant, ..) = p7_d01_gray_assert_endpoint_fixture();
    variant.code[assert_start + 3] = 2;
    assert!(
        gray_assert_site(&variant, region_start, region_end, assert_end, 910003).is_err(),
        "P7_GRAY_ENDPOINT_CONTROL_WRONG_ARGC: an Assert call with argc 2 must be rejected"
    );

    let (mut variant, ..) = p7_d01_gray_assert_endpoint_fixture();
    variant.code[assert_start] = vm::OpCode::Ret as u8;
    variant.code.truncate(assert_start + 1);
    bind_helper_end(&mut variant, start, assert_start + 1);
    variant.debug.as_mut().expect("fixture debug").lines = vec![vm::LineInfo {
        offset: assert_start as u32,
        line: 910003,
    }];
    assert!(
        gray_assert_site(
            &variant,
            region_start,
            (assert_start + 1) as u32,
            assert_start + 1,
            910003
        )
        .is_err(),
        "P7_GRAY_ENDPOINT_CONTROL_WRONG_OP: a non-Call instruction ending at ip must be rejected"
    );

    assert!(
        gray_assert_site(&program, region_start, region_end, assert_start + 1, 910003).is_err(),
        "P7_GRAY_ENDPOINT_CONTROL_IP_IN_OPERAND: an ip inside a Call operand must be rejected"
    );
    assert!(
        gray_assert_site(&program, region_start, region_end, assert_end + 1, 910003).is_err(),
        "P7_GRAY_ENDPOINT_CONTROL_IP_NOT_BOUNDARY: an ip that is not an instruction end must be rejected"
    );
    assert!(
        gray_assert_site(&program, region_start, region_end, start, 910003).is_err(),
        "P7_GRAY_ENDPOINT_CONTROL_IP_AT_START: an ip equal to region_start has no preceding instruction"
    );
    assert!(
        gray_assert_site(
            &program,
            region_start,
            region_end,
            region_end as usize + 1,
            910003
        )
        .is_err(),
        "P7_GRAY_ENDPOINT_CONTROL_IP_OUTSIDE: an ip past region_end must be rejected"
    );

    let (mut variant, ..) = p7_d01_gray_assert_endpoint_fixture();
    variant.code[start] = 0xFF;
    assert!(
        gray_assert_site(&variant, region_start, region_end, assert_end, 910003).is_err(),
        "P7_GRAY_ENDPOINT_CONTROL_UNKNOWN_OPCODE: an unknown opcode inside the prefix must be rejected"
    );
    assert!(
        gray_assert_site(&program, region_start, region_end, start + 3, 910003).is_err(),
        "P7_GRAY_ENDPOINT_CONTROL_TRUNCATED_PREFIX: an ip inside the first instruction must be rejected"
    );

    let (mut variant, ..) = p7_d01_gray_assert_endpoint_fixture();
    let short_end = assert_end - 1;
    bind_helper_end(&mut variant, start, short_end);
    variant.debug.as_mut().expect("fixture debug").lines = vec![vm::LineInfo {
        offset: assert_start as u32,
        line: 910003,
    }];
    assert!(
        gray_assert_site(&variant, region_start, short_end as u32, assert_end, 910003).is_err(),
        "P7_GRAY_ENDPOINT_CONTROL_SHORT_REGION: an owned range ending before the Assert must be rejected"
    );
    assert!(
        gray_assert_site(
            &program,
            region_start,
            (program.code.len() + 1) as u32,
            assert_end,
            910003
        )
        .is_err(),
        "P7_GRAY_ENDPOINT_CONTROL_OVERSIZED_REGION: a range past code.len() must be rejected"
    );
    assert!(
        gray_assert_site(&program, region_start + 1, region_end, assert_end, 910003).is_err(),
        "P7_GRAY_ENDPOINT_CONTROL_START_IN_OPERAND: a range start that is not an owner region must be rejected"
    );
    assert!(
        gray_assert_site(&program, region_start, region_start, assert_end, 910003).is_err(),
        "P7_GRAY_ENDPOINT_CONTROL_EMPTY_REGION: an empty range must be rejected"
    );

    let (mut variant, ..) = p7_d01_gray_assert_endpoint_fixture();
    variant.function_regions.push(vm::FunctionRegion {
        start_ip: region_start,
        end_ip: region_end,
        prototype_id: Some(1),
    });
    assert!(
        gray_assert_site(&variant, region_start, region_end, assert_end, 910003).is_err(),
        "P7_GRAY_ENDPOINT_CONTROL_DUPLICATE_OWNER: a duplicated owner region must be rejected as ambiguous"
    );

    let (mut variant, ..) = p7_d01_gray_assert_endpoint_fixture();
    variant.code.truncate(assert_end);
    bind_helper_end(&mut variant, start, assert_end);
    variant.debug.as_mut().expect("fixture debug").lines = vec![vm::LineInfo {
        offset: assert_start as u32,
        line: 910003,
    }];
    let bounded_end = assert_end as u32;
    assert_eq!(
        gray_assert_site(&variant, region_start, bounded_end, assert_end, 910003),
        Ok(assert_start),
        "P7_GRAY_ENDPOINT_CONTROL_END_AT_ASSERT: an Assert ending exactly at region_end must be accepted"
    );
    variant.code.extend_from_slice(&[0xFF, 0xFF, 0xFF]);
    assert!(
        variant.code.len() > assert_end,
        "fixture precondition: malformed bytes follow the owner region"
    );
    assert_eq!(
        gray_assert_site(&variant, region_start, bounded_end, assert_end, 910003),
        Ok(assert_start),
        "P7_GRAY_ENDPOINT_CONTROL_BOUNDED_READ: malformed bytes after the owner region must not be read"
    );
}

// ---------------------------------------------------------------------------
// P7 D01 Gray step 3: matched-layout producer/consumer vehicles.
// One virtual source, one diagnostic IR topology and three wrapper seed pairs.
// Every vehicle is compiled once, both endpoint programs are compared with the
// full program before either endpoint guest VM runs, and only then are the two
// endpoints executed.
// ---------------------------------------------------------------------------

/// One compiled, not yet executed Gray vehicle. The public `Program` snapshot
/// is taken before any guest execution; VM creation (host catalog, owned
/// arrays, restricted registry, JIT explicitly disabled) stays separate from
/// the single fresh `run`.
struct GrayVehicle {
    program: vm::Program,
    machine: vm::Vm,
}

/// The live helper frame at a Gray endpoint, read through the actual
/// `parameter_slots`/`local_base` mapping rather than a hardcoded physical
/// slot: `completed_at_entry`, `mode`, `target`, and the callee's own resume
/// continuation.
#[derive(Debug)]
struct GrayFrameSample {
    completed: Option<i64>,
    mode: Option<i64>,
    target: Option<i64>,
    return_ip: Option<usize>,
}

#[derive(Debug)]
struct GrayRun {
    status: Result<VmStatus, vm::VmError>,
    fuel_after: Option<u64>,
    reason: Option<vm::VmYieldReason>,
    ip: usize,
    label: Option<u32>,
    /// Every active callable prototype at the stop that declares arity 4 and
    /// four mapped parameters, found by topology alone: the independent
    /// evidence that exactly one such frame is live, not the id used to read.
    arity_four_frames: Vec<u32>,
    helper_frame: Option<GrayFrameSample>,
    wrapper_active: Option<u32>,
    named_frames: Vec<String>,
    jit_enabled: bool,
    aot_executions: u64,
    jit_native_executions: u64,
}

/// Compile one Gray vehicle exactly once. Source, force-GC hook, 10M fuel,
/// heap/root limits and interpreter-only configuration match the accepted D01
/// vehicle; only the wrapper's two Int seed constants differ between vehicles.
fn gray_vehicle(source: &str, stop: (object_observer::D01Stop, i64)) -> GrayVehicle {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_host_api_catalog(catalog.clone())
        .with_source_plugin(Box::leak(Box::new(object_observer::D01Observer {
            force_gc: true,
            stop: Some(stop),
        })));
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-object-observer")),
        options,
    )
    .unwrap_or_else(|error| panic!("matched-layout Gray must compile:\n{error:?}"));
    let mut machine = compiled.into_vm().expect("certified VM");
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("owned arrays");
    let mut registry = HostFunctionRegistry::restricted();
    for module in [
        pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
        pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module(),
    ] {
        module
            .install_from_catalog(&mut registry, &catalog)
            .expect("exact catalog");
    }
    registry.allow_builtin("math::floor").expect("floor");
    registry
        .allow_builtin("bytes::from_utf8")
        .expect("bootstrap strings");
    registry.bind_vm_cached(&mut machine).expect("bind");
    machine.set_fuel(10_000_000);
    let program = machine.program().clone();
    GrayVehicle { program, machine }
}

impl GrayVehicle {
    /// Execute this vehicle's single fresh guest VM and read every boundary
    /// value before any expected-status assertion.
    fn run(&mut self, helper: u32, wrapper: u32) -> GrayRun {
        use vm::vm::VmFrameContinuation;
        let status = self.machine.run();
        let fuel_after = self.machine.get_fuel();
        let jit_enabled = self.machine.jit_config().enabled;
        let aot_executions = self.machine.aot_exec_count();
        let jit_native_executions = self.machine.jit_native_exec_count();
        let reason = self.machine.last_yield_reason();
        let ip = self.machine.ip();
        let frames = self.machine.execution_frames();
        let label = self
            .machine
            .debug_info()
            .and_then(|info| info.line_for_offset(ip));
        let program = self.machine.program();
        let mut helper_frame: Option<GrayFrameSample> = None;
        if let Some(prototype) = program.callable_prototypes.get(helper as usize) {
            for frame in frames.iter() {
                if frame.prototype_id != Some(helper) {
                    continue;
                }
                let mut params: Vec<Value> = Vec::with_capacity(prototype.parameter_slots.len());
                for slot in &prototype.parameter_slots {
                    let cell = &self.machine.locals()[frame.local_base + usize::from(*slot)];
                    params.push(cell.clone());
                }
                helper_frame = Some(GrayFrameSample {
                    completed: int_param(&params, 1),
                    mode: int_param(&params, 2),
                    target: int_param(&params, 3),
                    return_ip: match &frame.continuation {
                        VmFrameContinuation::ResumeBytecode { return_ip } => Some(*return_ip),
                        _ => None,
                    },
                });
                break;
            }
        }
        let mut arity_four_frames: Vec<u32> = frames
            .iter()
            .filter_map(|frame| {
                let id = frame.prototype_id?;
                let prototype = program.callable_prototypes.get(id as usize)?;
                (prototype.arity == 4 && prototype.parameter_slots.len() == 4).then_some(id)
            })
            .collect();
        arity_four_frames.sort_unstable();
        let wrapper_active = frames
            .iter()
            .find(|frame| frame.prototype_id == Some(wrapper))
            .map(|_| wrapper);
        let named_frames = frames
            .iter()
            .map(|frame| {
                let prototype = frame
                    .prototype_id
                    .and_then(|id| program.callable_prototypes.get(id as usize));
                let params: Vec<String> = prototype
                    .map(|prototype| {
                        prototype
                            .parameter_slots
                            .iter()
                            .map(|slot| {
                                render_parameter(
                                    &self.machine.locals()[frame.local_base + usize::from(*slot)],
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                format!(
                    "base={} proto={:?} arity={:?} params={:?}",
                    frame.local_base,
                    frame.prototype_id,
                    prototype.map(|prototype| prototype.arity),
                    params
                )
            })
            .collect();
        GrayRun {
            status,
            fuel_after,
            reason,
            ip,
            label,
            arity_four_frames,
            helper_frame,
            wrapper_active,
            named_frames,
            jit_enabled,
            aot_executions,
            jit_native_executions,
        }
    }
}

/// The static binding of the matched-layout Gray producer/consumer pair,
/// derived from the actual compiled program and the captured IR emission list.
#[derive(Clone, Copy, Debug)]
struct GrayBinding {
    wrapper: u32,
    helper: u32,
    helper_fn: u32,
    wrapper_start: u32,
    wrapper_end: u32,
    helper_start: u32,
    helper_end: u32,
    call_start: usize,
    call_end: usize,
    emission_base: i64,
}

/// Bind the matched-layout helper and its wrapper from the actual compiled
/// program. The helper is the only 4-arity/4-parameter script prototype whose
/// captured declaration schema matches and whose single argc-4 `CallScript`
/// sits inside one callable region; that region's prototype is the wrapper,
/// which must keep the original collector's arity-1 carrier signature. Both
/// are then placed in the captured sorted IR emission list, which must yield
/// one common base; no prototype index is assumed to equal the logical index.
fn gray_bind(program: &vm::Program, entry: &object_observer::GrayLedgerEntry) -> GrayBinding {
    assert_eq!(
        entry.diag_arity, 4,
        "the captured Gray helper keeps arity 4"
    );
    assert_eq!(
        entry.diag_params, 4,
        "the captured Gray helper keeps four mapped parameters"
    );
    let insns = gray_decode_instructions(&program.code).expect("P7_GRAY_BIND_DECODE");
    let expected_result = entry
        .return_schema
        .clone()
        .unwrap_or(vm::TypeSchema::Unknown);
    let mut shaped: Vec<(u32, Option<vm::TypeSchema>)> = Vec::new();
    for (id, prototype) in program.callable_prototypes.iter().enumerate() {
        if prototype.arity != 4 || prototype.parameter_slots.len() != 4 {
            continue;
        }
        if !matches!(prototype.target, vm::CallableTarget::ScriptFunction(_)) {
            continue;
        }
        shaped.push((id as u32, prototype.schema.clone()));
    }
    assert!(
        !shaped.is_empty(),
        "P7_GRAY_BIND_SHAPE: no 4-arity/4-parameter script prototype exists in the compiled program"
    );
    let mut found: Vec<(u32, u32, usize)> = Vec::new();
    let mut rejected: Vec<(u32, Option<vm::TypeSchema>)> = Vec::new();
    for (id, schema) in &shaped {
        let schema_ok = matches!(
            schema.as_ref(),
            Some(vm::TypeSchema::Callable { params, result })
                if params.len() == 4
                    && params[1..]
                        .iter()
                        .all(|param| matches!(param, vm::TypeSchema::Int))
                    && result.as_ref() == &expected_result
        );
        if !schema_ok {
            rejected.push((*id, schema.clone()));
            continue;
        }
        let mut calls: Vec<usize> = Vec::new();
        for (offset, opcode, operands) in &insns {
            if *opcode != vm::OpCode::CallScript || operands.len() != 5 {
                continue;
            }
            let pid = u32::from_le_bytes([operands[0], operands[1], operands[2], operands[3]]);
            if pid == *id && operands[4] == 4 {
                calls.push(*offset);
            }
        }
        if calls.len() != 1 {
            continue;
        }
        // The callsite owner is the innermost callable region containing it, so
        // a wider enclosing script region can never be mistaken for the
        // wrapper; a tie between equal spans stays ambiguous and is rejected.
        let mut owner: Option<(u32, usize, u32)> = None;
        let mut ambiguous = false;
        for region in &program.function_regions {
            let Some(region_prototype) = region.prototype_id else {
                continue;
            };
            if region.start_ip as usize > calls[0] || calls[0] + 6 > region.end_ip as usize {
                continue;
            }
            let span = region.end_ip - region.start_ip;
            match owner {
                Some((_, _, best)) if span == best => ambiguous = true,
                Some((_, _, best)) if span > best => {}
                _ => owner = Some((region_prototype, calls[0], span)),
            }
        }
        if ambiguous {
            continue;
        }
        let Some((wrapper, _, _)) = owner else {
            continue;
        };
        found.push((*id, wrapper, calls[0]));
    }
    assert!(
        !found.is_empty(),
        "P7_GRAY_BIND_SCHEMA: no 4-parameter candidate {shaped:?} both carries the captured Callable{{[_, Int, Int, Int], {expected_result:?}}} declaration schema ({rejected:?} rejected) and owns exactly one argc-4 wrapper callsite"
    );
    assert_eq!(
        found.len(),
        1,
        "P7_GRAY_BIND_UNIQUE: exactly one 4-parameter matched-layout helper, got {found:?}"
    );
    let (helper, wrapper, call_start) = found[0];
    let call_end = call_start + 6;
    assert_ne!(
        wrapper, helper,
        "P7_GRAY_BIND_PROTO: wrapper and helper prototypes must differ"
    );
    let wrapper_prototype = &program.callable_prototypes[wrapper as usize];
    assert_eq!(
        wrapper_prototype.arity, 1,
        "P7_GRAY_BIND_WRAPPER: the original collector wrapper keeps arity 1"
    );
    assert_eq!(
        wrapper_prototype.parameter_slots.len(),
        1,
        "P7_GRAY_BIND_WRAPPER: the wrapper keeps its single carrier parameter"
    );
    let wrapper_fn = match wrapper_prototype.target {
        vm::CallableTarget::ScriptFunction(id) => id,
        other => {
            panic!("P7_GRAY_BIND_WRAPPER: wrapper must target a script function, got {other:?}")
        }
    };
    let helper_fn = match program.callable_prototypes[helper as usize].target {
        vm::CallableTarget::ScriptFunction(id) => id,
        other => panic!("P7_GRAY_BIND_HELPER: helper must target a script function, got {other:?}"),
    };
    let wrapper_range = program
        .script_functions
        .get(wrapper_fn as usize)
        .expect("P7_GRAY_BIND_WRAPPER: wrapper script function");
    let helper_range = program
        .script_functions
        .get(helper_fn as usize)
        .expect("P7_GRAY_BIND_HELPER: helper script function");
    let (wrapper_start, wrapper_end) = (wrapper_range.entry_ip, wrapper_range.end_ip);
    let (helper_start, helper_end) = (helper_range.entry_ip, helper_range.end_ip);
    assert!(
        helper_start < helper_end && helper_end as usize <= program.code.len(),
        "P7_GRAY_BIND_HELPER: helper range [{helper_start},{helper_end}) must lie inside the code"
    );
    assert!(
        wrapper_start as usize <= call_start && call_end <= wrapper_end as usize,
        "P7_GRAY_BIND_CALL: the helper callsite [{call_start},{call_end}) must sit inside the wrapper range [{wrapper_start},{wrapper_end})"
    );
    assert!(
        wrapper_end <= helper_start || helper_end <= wrapper_start,
        "P7_GRAY_BIND_REGION: wrapper and helper ranges must not overlap"
    );
    for (what, prototype, start, end) in [
        ("wrapper", wrapper, wrapper_start, wrapper_end),
        ("helper", helper, helper_start, helper_end),
    ] {
        let owned: Vec<&vm::FunctionRegion> = program
            .function_regions
            .iter()
            .filter(|region| region.prototype_id == Some(prototype))
            .collect();
        assert_eq!(
            owned.len(),
            1,
            "P7_GRAY_BIND_REGION: {what} {prototype} must own exactly one region, found {}",
            owned.len()
        );
        assert_eq!(
            (owned[0].start_ip, owned[0].end_ip),
            (start, end),
            "P7_GRAY_BIND_REGION: {what} region must match its script range"
        );
    }
    let index =
        gray_find_helper_callsite(&insns, helper, wrapper_start as usize, wrapper_end as usize)
            .unwrap_or_else(|error| panic!("P7_GRAY_BIND_CALL: {error}"));
    assert_eq!(
        insns[index].0, call_start,
        "P7_GRAY_BIND_CALL: the validated wrapper callsite must be the bound one"
    );
    let position = |key: u16| -> i64 {
        entry
            .sorted_keys
            .iter()
            .position(|candidate| *candidate == key)
            .unwrap_or_else(|| {
                panic!(
                    "P7_GRAY_BIND_EMISSION: logical function {key} is missing from the captured sorted emission list"
                )
            }) as i64
    };
    let wrapper_base = i64::from(wrapper) - position(entry.collect);
    let helper_base = i64::from(helper) - position(entry.diag);
    assert_eq!(
        wrapper_base, helper_base,
        "P7_GRAY_BIND_EMISSION: one sorted-emission base for wrapper {wrapper} (logical {}) and helper {helper} (logical {})",
        entry.collect, entry.diag
    );
    assert!(
        wrapper_base >= 0,
        "P7_GRAY_BIND_EMISSION: the common sorted-emission base must be nonnegative, got {wrapper_base}"
    );
    GrayBinding {
        wrapper,
        helper,
        helper_fn,
        wrapper_start,
        wrapper_end,
        helper_start,
        helper_end,
        call_start,
        call_end,
        emission_base: wrapper_base,
    }
}

/// Maps a fresh entry's completed-cycle count to the preceding completed
/// ordinal; a non-positive count is unknown and selects no target.
fn gray_target_from_entry(completed: i64) -> Option<i64> {
    if completed > 0 {
        completed.checked_sub(1)
    } else {
        None
    }
}

/// Matched-layout D01 Gray localization: one full run plus the Gray-entry and
/// Gray-exit endpoints, all from this single focused test so the target ordinal
/// flows in memory from the fresh full run. Each vehicle is compiled once, both
/// endpoint programs are compared with the full program before either endpoint
/// guest VM runs, and the reported interval is the instrumented Gray phase of
/// the chosen collection only - never a production net cost, a truncated cycle
/// or a global collection distribution.
#[test]
fn p7_d01_gray_matched_layout_interpreter() {
    object_observer::clear_gray_ledger();
    eprintln!(
        "P7_D01_GRAY_PIPELINE attempt=1 stop=GrayFull stage=compile-begin source=D01_FORCED_SOURCE"
    );
    let mut full = gray_vehicle(D01_FORCED_SOURCE, (object_observer::D01Stop::GrayFull, 0));
    let full_program = full.program.clone();
    let compiled = object_observer::gray_ledger();
    assert_eq!(
        compiled.len(),
        1,
        "the full vehicle must record exactly one Gray pipeline, got {}",
        compiled.len()
    );
    let full_entry = compiled.last().expect("full Gray ledger entry").clone();
    let binding = gray_bind(&full_program, &full_entry);
    eprintln!(
        "P7_D01_GRAY_PIPELINE attempt=1 stop=GrayFull stage=compile-ok pipelines={} code_len={} constants={} prototypes={} regions={} locals={}",
        compiled.len(),
        full_program.code.len(),
        full_program.constants.len(),
        full_program.callable_prototypes.len(),
        full_program.function_regions.len(),
        full_program.local_count
    );
    eprintln!(
        "P7_D01_GRAY_BINDING wrapper={} helper={} helper_fn={} call=[{},{}] wrapper_region=[{},{}] helper_region=[{},{}] emission_base={} collect_logical={} diag_logical={}",
        binding.wrapper,
        binding.helper,
        binding.helper_fn,
        binding.call_start,
        binding.call_end,
        binding.wrapper_start,
        binding.wrapper_end,
        binding.helper_start,
        binding.helper_end,
        binding.emission_base,
        full_entry.collect,
        full_entry.diag
    );
    eprintln!("P7_D01_GRAY_VM attempt=1 stop=GrayFull stage=run-begin target=0 mode=0");
    let full_run = full.run(binding.helper, binding.wrapper);
    eprintln!(
        "P7_D01_GRAY_FULL status={:?} reason={:?} fuel_after={:?} ip={} label={:?} helper={:?} frame={:?} wrapper={:?} jit={} aot={} native={} frames={:?}",
        full_run.status,
        full_run.reason,
        full_run.fuel_after,
        full_run.ip,
        full_run.label,
        full_run.arity_four_frames,
        full_run.helper_frame,
        full_run.wrapper_active,
        full_run.jit_enabled,
        full_run.aot_executions,
        full_run.jit_native_executions,
        full_run.named_frames
    );
    match &full_run.status {
        Ok(VmStatus::Halted) => {
            eprintln!(
                "P7_D01_GRAY_FULL_TERMINAL status=Halted ip={} frames={:?} (guest termination is reported as termination, never as a JS true)",
                full_run.ip, full_run.named_frames
            );
            return;
        }
        Ok(VmStatus::Yielded) => {}
        other => panic!("unexpected guest end before the Gray readout: {other:?}"),
    }
    assert_eq!(
        full_run.reason,
        Some(vm::VmYieldReason::Fuel),
        "the full Gray run must exhaust fuel"
    );
    assert_eq!(
        full_run.fuel_after,
        Some(0),
        "the original 10M budget is exhausted, not recharged"
    );
    assert!(
        !full_run.jit_enabled,
        "the full Gray run must stay interpreter-only"
    );
    assert_eq!(full_run.aot_executions, 0, "no AOT execution");
    assert_eq!(
        full_run.jit_native_executions, 0,
        "no native trace execution"
    );
    let Some(full_frame) = full_run.helper_frame.as_ref() else {
        eprintln!(
            "P7_D01_GRAY_FULL_UNKNOWN no active four-parameter matched-layout helper at fuel exhaustion (ip={} label={:?} arity_four_frames={:?}); the target ordinal is unknown so no endpoint vehicle runs and no interval is reported",
            full_run.ip, full_run.label, full_run.arity_four_frames
        );
        return;
    };
    assert!(
        full_run.arity_four_frames.contains(&binding.helper),
        "the bound helper prototype must be one of the live four-parameter frames {:?}",
        full_run.arity_four_frames
    );
    let (Some(mode), Some(seed), Some(completed)) =
        (full_frame.mode, full_frame.target, full_frame.completed)
    else {
        eprintln!(
            "P7_D01_GRAY_FULL_UNKNOWN the helper frame is active but its mapped parameters are not all live Int cells any more ({full_frame:?}); the target ordinal is unknown so no endpoint vehicle runs and no interval is reported"
        );
        return;
    };
    assert_eq!(mode, 0, "the full run carries mode 0");
    assert_eq!(seed, 0, "the full run carries target 0");
    // TEMP RED_ONLY: the ordinal is routed through the current (wrong) pure
    // mapping so the parent can observe the previous-completed RED without
    // changing endpoint semantics. No checked_sub is introduced here.
    let Some(target) = gray_target_from_entry(completed) else {
        object_observer::clear_gray_ledger();
        eprintln!(
            "P7_D01_GRAY_NO_INTERVAL ordinal=unknown completed={completed} (the fresh entry completed count selects no target cycle under the current mapping, so no endpoint vehicle runs and no interval is reported)"
        );
        return;
    };
    let inside_helper =
        (binding.helper_start as usize..binding.helper_end as usize).contains(&full_run.ip);
    let full_phase = if inside_helper {
        full_run.label.and_then(|label| {
            object_observer::diag_phase(label).or_else(|| object_observer::diag_stop_phase(label))
        })
    } else {
        None
    };
    eprintln!(
        "P7_D01_GRAY_FULL_HELPER helper={} target={target} inside_helper={inside_helper} phase={:?} label={:?} ip={} (a phase outside the proven helper region stays UNKNOWN; the ordinal is still the fresh entry sample)",
        binding.helper, full_phase, full_run.label, full_run.ip
    );
    eprintln!(
        "P7_D01_GRAY_PIPELINE attempt=2 stop=GrayEntry stage=compile-begin target={target} source=D01_FORCED_SOURCE"
    );
    let mut entry = gray_vehicle(
        D01_FORCED_SOURCE,
        (object_observer::D01Stop::GrayEntry, target),
    );
    let entry_program = entry.program.clone();
    let after_entry = object_observer::gray_ledger();
    assert_eq!(
        after_entry.len(),
        2,
        "the full and Gray-entry vehicles must record exactly two Gray pipelines, got {}",
        after_entry.len()
    );
    eprintln!(
        "P7_D01_GRAY_PIPELINE attempt=3 stop=GrayExit stage=compile-begin target={target} source=D01_FORCED_SOURCE"
    );
    let mut exit = gray_vehicle(
        D01_FORCED_SOURCE,
        (object_observer::D01Stop::GrayExit, target),
    );
    let exit_program = exit.program.clone();
    let ledger = object_observer::gray_ledger();
    assert_eq!(
        ledger.len(),
        3,
        "exactly the full, Gray-entry and Gray-exit vehicles must record three Gray pipelines, got {}",
        ledger.len()
    );
    let last_three = &ledger[ledger.len() - 3..];
    assert_eq!(
        last_three[0].wrapper_seeds,
        (0, 0),
        "the full vehicle seeds mode 0 / target 0"
    );
    assert_eq!(
        last_three[1].wrapper_seeds,
        (1, target),
        "the entry vehicle seeds mode 1 / the fresh target"
    );
    assert_eq!(
        last_three[2].wrapper_seeds,
        (2, target),
        "the exit vehicle seeds mode 2 / the fresh target"
    );
    for recorded in &ledger {
        assert_eq!(
            (recorded.locals, recorded.functions),
            (last_three[2].locals, last_three[2].functions),
            "all vehicles must carry one IR layout: locals/functions"
        );
        assert_eq!(
            recorded.diag_shape, last_three[2].diag_shape,
            "all vehicles must carry one helper body shape"
        );
        assert_eq!(
            (recorded.collect, recorded.diag),
            (last_three[2].collect, last_three[2].diag),
            "all vehicles must carry one helper logical identity"
        );
        assert_eq!(
            recorded.sorted_keys, last_three[2].sorted_keys,
            "all vehicles must carry one sorted IR emission list"
        );
        assert_eq!(
            recorded.protos, last_three[2].protos,
            "all vehicles must carry one prototype declaration table"
        );
        assert_eq!(
            (recorded.diag_arity, recorded.diag_params),
            (4, 4),
            "every vehicle keeps the four-parameter helper signature"
        );
        assert_eq!(
            recorded.arg_schemas, last_three[2].arg_schemas,
            "all vehicles must carry one declared parameter schema"
        );
        assert_eq!(
            recorded.return_schema, last_three[2].return_schema,
            "all vehicles must carry one declared result schema"
        );
        assert_eq!(
            (recorded.mode, recorded.target),
            recorded.wrapper_seeds,
            "each ledger row mode/target must equal its wrapper seed pair"
        );
    }
    eprintln!(
        "P7_D01_GRAY_LAYOUT_IR pipelines={} shape={} helper_logical={} sorted_keys={:?}",
        ledger.len(),
        last_three[2].diag_shape,
        last_three[2].diag,
        last_three[2].sorted_keys
    );
    let entry_verdict = gray_compare_programs(
        &full_program,
        &entry_program,
        binding.wrapper,
        binding.helper,
        1,
        target,
    );
    let exit_verdict = gray_compare_programs(
        &full_program,
        &exit_program,
        binding.wrapper,
        binding.helper,
        2,
        target,
    );
    eprintln!(
        "P7_D01_GRAY_LAYOUT wrapper={} helper={} target={target} entry={entry_verdict:?} exit={exit_verdict:?}",
        binding.wrapper, binding.helper
    );
    if entry_verdict.is_err() || exit_verdict.is_err() {
        // RAW receipt: the failing artifacts must be readable before any
        // assertion can discard them. Printed metadata is evidence, never the
        // comparison itself.
        for (what, program) in [
            ("full", &full_program),
            ("entry", &entry_program),
            ("exit", &exit_program),
        ] {
            eprintln!(
                "P7_D01_GRAY_LAYOUT_RAW {what} code_len={} code={:02X?} constants={:?} script_functions={:?} prototypes={:?} regions={:?}",
                program.code.len(),
                program.code,
                program.constants,
                program.script_functions,
                program.callable_prototypes,
                program.function_regions
            );
        }
    }
    entry_verdict.unwrap_or_else(|error| {
        panic!("the Gray-entry program must match the full layout: {error}")
    });
    exit_verdict.unwrap_or_else(|error| {
        panic!("the Gray-exit program must match the full layout: {error}")
    });
    eprintln!("P7_D01_GRAY_VM attempt=2 stop=GrayEntry stage=run-begin target={target} mode=1");
    let entry_run = entry.run(binding.helper, binding.wrapper);
    eprintln!(
        "P7_D01_GRAY_ENTRY status={:?} reason={:?} fuel_after={:?} ip={} label={:?} helper={:?} frame={:?} wrapper={:?} frames={:?}",
        entry_run.status,
        entry_run.reason,
        entry_run.fuel_after,
        entry_run.ip,
        entry_run.label,
        entry_run.arity_four_frames,
        entry_run.helper_frame,
        entry_run.wrapper_active,
        entry_run.named_frames
    );
    match &entry_run.status {
        Err(vm::VmError::HostError(message)) if message == "assertion failed" => {}
        Ok(VmStatus::Yielded) | Ok(VmStatus::Halted) => {
            eprintln!(
                "P7_D01_GRAY_ENTRY_UNREACHED target={target} status={:?} fuel_after={:?} (the full run stopped before this collection's Gray entry; the endpoint is reported unreached and no interval is reported)",
                entry_run.status, entry_run.fuel_after
            );
            return;
        }
        other => {
            panic!("the Gray-entry endpoint must stop on its deliberate assert, got {other:?}")
        }
    }
    assert!(
        !entry_run.jit_enabled,
        "the entry endpoint must stay interpreter-only"
    );
    assert_eq!(entry_run.aot_executions, 0, "no AOT execution");
    assert_eq!(
        entry_run.jit_native_executions, 0,
        "no native trace execution"
    );
    assert!(
        entry_run.arity_four_frames.contains(&binding.helper),
        "the bound helper prototype must be one of the live four-parameter frames {:?} at the entry stop",
        entry_run.arity_four_frames
    );
    assert_eq!(
        entry_run.wrapper_active,
        Some(binding.wrapper),
        "the wrapper frame must be active at the helper stop"
    );
    let entry_frame = entry_run
        .helper_frame
        .as_ref()
        .expect("the entry stop must be read from the mapped helper frame");
    assert_eq!(
        entry_frame.completed,
        Some(target),
        "the entry stop must fire in the target collection"
    );
    assert_eq!(entry_frame.mode, Some(1), "the entry run carries mode 1");
    assert_eq!(
        entry_frame.target,
        Some(target),
        "the entry run carries the fresh target ordinal"
    );
    assert_eq!(
        entry_frame.return_ip,
        Some(binding.call_end),
        "the helper's own continuation must be the wrapper's argc-4 CallScript end"
    );
    let entry_assert = gray_assert_site(
        &entry_program,
        binding.helper_start,
        binding.helper_end,
        entry_run.ip,
        object_observer::DIAG_STOP_GRAY_ENTRY,
    )
    .unwrap_or_else(|error| {
        panic!(
            "the Gray-entry endpoint must be an Assert ending at the failure ip inside the helper region: {error}"
        )
    });
    let entry_label = entry_program
        .debug
        .as_ref()
        .expect("debug info")
        .line_for_offset(entry_assert);
    assert_eq!(
        entry_label,
        Some(object_observer::DIAG_STOP_GRAY_ENTRY),
        "the entry Assert start must carry its own stop label"
    );
    assert_eq!(
        entry_label.and_then(|label| object_observer::diag_phase(label)
            .or_else(|| object_observer::diag_stop_phase(label))),
        Some("gray-drain"),
        "the entry Assert must attribute the gray-drain phase through the diagnostic label table"
    );
    assert_eq!(
        entry_label.and_then(object_observer::diag_stop_phase),
        Some("gray-drain"),
        "the entry stop label must map back to gray-drain"
    );
    eprintln!(
        "P7_D01_GRAY_ENTRY_ACTUAL target={target} assert_start={entry_assert} label={entry_label:?} phase=gray-drain remaining_fuel={:?}",
        entry_run.fuel_after
    );
    eprintln!("P7_D01_GRAY_VM attempt=3 stop=GrayExit stage=run-begin target={target} mode=2");
    let exit_run = exit.run(binding.helper, binding.wrapper);
    eprintln!(
        "P7_D01_GRAY_EXIT status={:?} reason={:?} fuel_after={:?} ip={} label={:?} helper={:?} frame={:?} wrapper={:?} frames={:?}",
        exit_run.status,
        exit_run.reason,
        exit_run.fuel_after,
        exit_run.ip,
        exit_run.label,
        exit_run.arity_four_frames,
        exit_run.helper_frame,
        exit_run.wrapper_active,
        exit_run.named_frames
    );
    match &exit_run.status {
        Err(vm::VmError::HostError(message)) if message == "assertion failed" => {}
        Ok(VmStatus::Yielded) | Ok(VmStatus::Halted) => {
            eprintln!(
                "P7_D01_GRAY_EXIT_UNREACHED target={target} status={:?} fuel_after={:?} (the target collection's Gray drain did not complete before the full stop; the endpoint is reported unreached and no interval is reported)",
                exit_run.status, exit_run.fuel_after
            );
            return;
        }
        other => panic!("the Gray-exit endpoint must stop on its deliberate assert, got {other:?}"),
    }
    assert!(
        !exit_run.jit_enabled,
        "the exit endpoint must stay interpreter-only"
    );
    assert_eq!(exit_run.aot_executions, 0, "no AOT execution");
    assert_eq!(
        exit_run.jit_native_executions, 0,
        "no native trace execution"
    );
    assert!(
        exit_run.arity_four_frames.contains(&binding.helper),
        "the bound helper prototype must be one of the live four-parameter frames {:?} at the exit stop",
        exit_run.arity_four_frames
    );
    assert_eq!(
        exit_run.wrapper_active,
        Some(binding.wrapper),
        "the wrapper frame must be active at the exit stop"
    );
    let exit_frame = exit_run
        .helper_frame
        .as_ref()
        .expect("the exit stop must be read from the mapped helper frame");
    assert_eq!(
        exit_frame.completed,
        Some(target),
        "the exit stop must fire in the target collection"
    );
    assert_eq!(exit_frame.mode, Some(2), "the exit run carries mode 2");
    assert_eq!(
        exit_frame.target,
        Some(target),
        "the exit run carries the fresh target ordinal"
    );
    assert_eq!(
        exit_frame.return_ip,
        Some(binding.call_end),
        "the helper's own continuation must be the wrapper's argc-4 CallScript end"
    );
    let exit_assert = gray_assert_site(
        &exit_program,
        binding.helper_start,
        binding.helper_end,
        exit_run.ip,
        object_observer::DIAG_STOP_GRAY_SWEEP,
    )
    .unwrap_or_else(|error| {
        panic!(
            "the Gray-exit endpoint must be an Assert ending at the failure ip inside the helper region: {error}"
        )
    });
    let exit_label = exit_program
        .debug
        .as_ref()
        .expect("debug info")
        .line_for_offset(exit_assert);
    assert_eq!(
        exit_label,
        Some(object_observer::DIAG_STOP_GRAY_SWEEP),
        "the exit Assert start must carry its own stop label"
    );
    assert_eq!(
        exit_label.and_then(object_observer::diag_stop_phase),
        Some("sweep"),
        "the exit stop label must map back to sweep"
    );
    eprintln!(
        "P7_D01_GRAY_EXIT_ACTUAL target={target} assert_start={exit_assert} label={exit_label:?} phase=sweep remaining_fuel={:?}",
        exit_run.fuel_after
    );
    let entry_fuel = entry_run
        .fuel_after
        .expect("the entry endpoint must report its remaining fuel");
    let exit_fuel = exit_run
        .fuel_after
        .expect("the exit endpoint must report its remaining fuel");
    match entry_fuel.checked_sub(exit_fuel) {
        Some(delta) => eprintln!(
            "P7_D01_GRAY_INTERVAL ordinal={target} inclusive_instrumented_gray_fuel={delta} entry_fuel={entry_fuel} exit_fuel={exit_fuel} (the instrumented Gray phase of the chosen collection only, never a production net cost, a truncated cycle or a global collection distribution)"
        ),
        None => eprintln!(
            "P7_D01_GRAY_NO_INTERVAL ordinal={target} entry_fuel={entry_fuel} exit_fuel={exit_fuel} (the Gray-exit endpoint did not consume less fuel than the Gray-entry endpoint, so no interval is reported and no business completion is claimed)"
        ),
    }
}

/// TEMP RED_ONLY: pure regression for the agreed previous-completed ordinal
/// contract. The inputs are synthetic pure integers only; no guest, GC, VM or
/// ledger data is involved, and no endpoint projection is changed here.
/// The first assertion is expected to fail under the current projection.
#[test]
fn p7_d01_gray_target_uses_previous_completed_entry() {
    assert_eq!(
        gray_target_from_entry(1),
        Some(0),
        "P7_GRAY_TARGET_PREVIOUS_COMPLETED_MUST_SELECT: a fresh completed count of 1 must select the previous completed cycle 0"
    );
    assert_eq!(
        gray_target_from_entry(0),
        None,
        "P7_GRAY_TARGET_PREVIOUS_COMPLETED_MUST_SELECT: a completed count of 0 has no previous cycle"
    );
    assert_eq!(
        gray_target_from_entry(-1),
        None,
        "P7_GRAY_TARGET_PREVIOUS_COMPLETED_MUST_SELECT: a negative completed count has no previous cycle"
    );
    assert_eq!(
        gray_target_from_entry(2),
        Some(1),
        "P7_GRAY_TARGET_PREVIOUS_COMPLETED_MUST_SELECT: a completed count of 2 must select 1"
    );
    assert_eq!(
        gray_target_from_entry(47),
        Some(46),
        "P7_GRAY_TARGET_PREVIOUS_COMPLETED_MUST_SELECT: a completed count of 47 must select 46"
    );
}
