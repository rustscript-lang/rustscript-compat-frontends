//! ES5 numeric intrinsics on the certified, JIT-disabled interpreter.
use pd_vm_compat_frontends::{JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor};
use vm::{HostFunctionRegistry, Value, VmStatus};

const P8_HISTORICAL_PREFIX: &str =
    "/mnt/TEMP/workspace/rustscript-compat-frontends/tmp/es51-frontend-7a1db979";
const P8_RETAINED_RELATIVE_PATHS: [&str; 3] = [
    "fix20-reissued/OUT/baseline-ordered874.effect",
    "fix20-reissued/rounder-factor10/OUT/proof/candidate-all-implementations.debug",
    "fix20-reissued/address07/OUT/proof/candidate-all-implementations.debug",
];

fn p8_map_retained_evidence_path(
    original: &std::path::Path,
    root: Option<&std::path::Path>,
) -> std::io::Result<std::path::PathBuf> {
    let Some(root) = root else {
        return Ok(original.to_path_buf());
    };
    let invalid = |message| std::io::Error::new(std::io::ErrorKind::InvalidInput, message);
    let relative = P8_RETAINED_RELATIVE_PATHS
        .iter()
        .find(|relative| {
            original.as_os_str()
                == std::path::Path::new(P8_HISTORICAL_PREFIX)
                    .join(relative)
                    .as_os_str()
        })
        .ok_or_else(|| invalid("unrecognized historical evidence path"))?;
    if !root.is_absolute()
        || root
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(invalid(
            "retained evidence root must be absolute without traversal",
        ));
    }
    let canonical_root = root.canonicalize()?;
    if !canonical_root.is_dir() {
        return Err(invalid(
            "retained evidence root must be an existing directory",
        ));
    }
    let mapped = root.join(relative);
    let canonical_input = mapped.canonicalize()?;
    if !canonical_input.starts_with(&canonical_root) || !canonical_input.is_file() {
        return Err(invalid(
            "retained evidence input must be a file inside its root",
        ));
    }
    Ok(mapped)
}

fn p8_retained_evidence_path(original: &std::path::Path) -> std::path::PathBuf {
    let root = std::env::var_os("P8_RETAINED_EVIDENCE_ROOT");
    p8_map_retained_evidence_path(original, root.as_deref().map(std::path::Path::new))
        .expect("valid retained evidence binding")
}

#[test]
fn p8_retained_evidence_resolver_controls() {
    use std::path::Path;
    let root = std::env::var_os("P8_RETAINED_EVIDENCE_ROOT")
        .expect("explicit actual retained input root for fixture controls");
    let root = Path::new(&root);
    let private = std::env::var_os("TMPDIR").expect("assigned private TEMP");
    let private = Path::new(&private);
    for relative in P8_RETAINED_RELATIVE_PATHS {
        let original = Path::new(P8_HISTORICAL_PREFIX).join(relative);
        assert_eq!(
            p8_map_retained_evidence_path(&original, None).unwrap(),
            original
        );
        let mapped = p8_map_retained_evidence_path(&original, Some(root)).unwrap();
        assert_eq!(mapped, root.join(relative));
        assert_eq!(
            std::fs::read(&mapped).unwrap(),
            std::fs::read(root.join(relative)).unwrap()
        );
        assert!(p8_map_retained_evidence_path(&original, Some(Path::new("relative"))).is_err());
        assert!(
            p8_map_retained_evidence_path(&original, Some(&private.join("missing-root"))).is_err()
        );
        assert!(p8_map_retained_evidence_path(&original, Some(private)).is_err());
        assert!(p8_map_retained_evidence_path(&original, Some(&root.join(".."))).is_err());
        assert!(p8_map_retained_evidence_path(&original, Some(&mapped)).is_err());
    }
    for foreign in [
        format!("/foreign/{}", P8_RETAINED_RELATIVE_PATHS[0]),
        format!("{P8_HISTORICAL_PREFIX}/foreign.effect"),
        format!(
            "{P8_HISTORICAL_PREFIX}/../{}",
            P8_RETAINED_RELATIVE_PATHS[0]
        ),
        format!("{P8_HISTORICAL_PREFIX}/./{}", P8_RETAINED_RELATIVE_PATHS[0]),
        format!(
            "{P8_HISTORICAL_PREFIX}/{}/extra",
            P8_RETAINED_RELATIVE_PATHS[0]
        ),
    ] {
        assert!(p8_map_retained_evidence_path(Path::new(&foreign), Some(root)).is_err());
        assert_eq!(
            p8_map_retained_evidence_path(Path::new(&foreign), None).unwrap(),
            Path::new(&foreign)
        );
    }
    eprintln!(
        "P8_RETAINED_RESOLVER_CONTROLS actual_inputs=3 fallback=true exact_binding=true foreign_prefix_suffix_traversal_rejected=true missing_root_input_rejected=true global_environment_mutated=false"
    );
}

fn field(value: &Value, name: &str) -> Value {
    let Value::Map(map) = value else {
        panic!("boxed value: {value:?}")
    };
    map.get(&Value::string(name)).expect("boxed field").clone()
}

fn prepare(source: &str, plugin: &'static dyn vm::SourcePlugin) -> vm::Vm {
    prepare_flavor(source, plugin, javascript_es5_flavor())
}
fn prepare_flavor(
    source: &str,
    plugin: &'static dyn vm::SourcePlugin,
    flavor: vm::SourceFlavor,
) -> vm::Vm {
    eprintln!("P8_SOURCE {source}");
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_host_api_catalog(catalog.clone())
        .with_source_plugin(plugin);
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source, flavor, options,
    )
    .unwrap_or_else(|error| panic!("source must compile: {source}\n{error:?}"));
    eprintln!("P8_COMPILED");
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
    for name in [
        "floor",
        "abs",
        "acos",
        "asin",
        "atan",
        "atan2",
        "ceil",
        "cos",
        "exp",
        "ln",
        "powf",
        "sin",
        "sqrt",
        "tan",
        "trunc",
        "log2",
        "log10",
        "exp2",
        "signum",
        "copysign",
        "is_finite",
        "is_infinite",
    ] {
        registry
            .allow_builtin(format!("math::{name}"))
            .expect("numeric builtin");
    }
    registry
        .allow_builtin("bytes::from_utf8")
        .expect("numeric builtin");
    registry.bind_vm_cached(&mut machine).expect("binding");
    machine.set_fuel(10_000_000);
    machine
}

fn execute(source: &str) -> Value {
    let mut machine = prepare(source, &JAVASCRIPT_ES5_PLUGIN);
    machine.set_fuel(10_000_000);
    assert_eq!(
        machine.run().unwrap_or_else(|e| {
            eprintln!("P8_INTERPRETER_FAILURE_DISASSEMBLY_BEGIN\n{}\nP8_INTERPRETER_FAILURE_DISASSEMBLY_END", vm::vmbc::disassemble_program(machine.program()));
            panic!(
                "{source}: {e:?} ip={} locals={:?} stack={:?} frames={:?} prototypes={:?}",
                machine.ip(),
                machine.locals(),
                machine.stack(),
                machine.execution_frames(),
                machine
                    .execution_frames()
                    .iter()
                    .filter_map(|f| f
                        .prototype_id
                        .map(|id| &machine.program().callable_prototypes[id as usize]))
                    .collect::<Vec<_>>()
            )
        }),
        VmStatus::Halted
    );
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    assert!(!machine.jit_config().enabled);
    assert!(
        machine
            .capture_certification_stats()
            .is_some_and(|f| f.valid)
    );
    let resources = machine.host_context().resource_count();
    assert!(resources <= 1, "at most one real B1 realm");
    let stats = pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine);
    if resources == 0 {
        assert!(stats.is_none());
    } else {
        let stats = stats.expect("real B1 stats");
        assert_eq!((stats.pending, stats.registered), (0, 1));
    }
    let value = machine.stack().last().expect("completion").clone();
    machine.reset_for_reuse().expect("reset");
    assert_eq!(machine.host_context().resource_count(), 0);
    value
}

fn number(source: &str) -> f64 {
    let result = execute(source);
    assert_eq!(field(&result, "tag"), Value::Int(3));
    match field(&result, "number") {
        Value::Float(n) => n,
        Value::Int(n) => n as f64,
        v => panic!("Number: {v:?}"),
    }
}

#[test]
fn p8_math_alias_survives_method_mutation_interpreter() {
    assert_eq!(
        number("'use strict'; var f = Math.abs; Math.abs = 99; f(-7);"),
        7.0
    );
}

#[test]
fn p8_original_alias_arithmetic_interpreter() {
    assert_eq!(
        number(
            "'use strict'; var f = Math.abs; Math.abs = function(x) { return 99; }; f(-7) + Math.abs(2);"
        ),
        106.0
    );
}

#[test]
fn p8_math_complete_member_smoke_interpreter() {
    let mut program = String::from("'use strict';");
    for (source, expected) in [
        ("Math.E;", std::f64::consts::E),
        ("Math.acos(1);", 0.0),
        ("Math.asin(0);", 0.0),
        ("Math.atan(0);", 0.0),
        ("Math.atan2(0,1);", 0.0),
        ("Math.ceil(1.2);", 2.0),
        ("Math.cos(0);", 1.0),
        ("Math.exp(0);", 1.0),
        ("Math.floor(1.2);", 1.0),
        ("Math.log(1);", 0.0),
        ("Math.max(1,7,3);", 7.0),
        ("Math.min(1,7,3);", 1.0),
        ("Math.pow(2,3);", 8.0),
        ("Math.round(-1.5);", -1.0),
        ("Math.sin(0);", 0.0),
        ("Math.sqrt(9);", 3.0),
        ("Math.tan(0);", 0.0),
    ] {
        program.push_str(&format!(
            "if (!({} === {:?})) {{throw 1;}}",
            source.trim_end_matches(';'),
            expected
        ));
    }
    program.push_str("true;");
    assert_eq!(field(&execute(&program), "flag"), Value::Bool(true));
}

#[test]
fn p8_numeric_globals_smoke_interpreter() {
    assert_eq!(number("parseInt('  -0x10tail', 0);"), -16.0);
    assert_eq!(number("parseFloat('  -1.25e2tail');"), -125.0);
    for source in ["isNaN(NaN);", "isFinite('12');", "Infinity === 1/0;"] {
        assert_eq!(
            field(&execute(source), "flag"),
            Value::Bool(true),
            "{source}"
        );
    }
}

#[test]
fn p8_vectors_a00_interpreter() {
    let result = execute(include_str!("javascript_es5/math-vectors-00.js"));
    assert_eq!(field(&result, "flag"), Value::Bool(true));
    eprintln!("P8_VECTORS batch=00 first=0 count=30 aot=0 trace=0");
}
#[test]
fn p8_vectors_a01_interpreter() {
    let result = execute(include_str!("javascript_es5/math-vectors-01.js"));
    assert_eq!(field(&result, "flag"), Value::Bool(true));
    eprintln!("P8_VECTORS batch=01 first=30 count=30 aot=0 trace=0");
}
#[test]
fn p8_vectors_a02_interpreter() {
    let result = execute(include_str!("javascript_es5/math-vectors-02.js"));
    assert_eq!(field(&result, "flag"), Value::Bool(true));
    eprintln!("P8_VECTORS batch=02 first=60 count=30 aot=0 trace=0");
}
#[test]
fn p8_vectors_a03_interpreter() {
    let result = execute(include_str!("javascript_es5/math-vectors-03.js"));
    assert_eq!(field(&result, "flag"), Value::Bool(true));
    eprintln!("P8_VECTORS batch=03 first=90 count=30 aot=0 trace=0");
}
#[test]
fn p8_vectors_a04_interpreter() {
    let result = execute(include_str!("javascript_es5/math-vectors-04.js"));
    assert_eq!(field(&result, "flag"), Value::Bool(true));
    eprintln!("P8_VECTORS batch=04 first=120 count=30 aot=0 trace=0");
}
#[test]
fn p8_vectors_a05_interpreter() {
    let result = execute(include_str!("javascript_es5/math-vectors-05.js"));
    assert_eq!(field(&result, "flag"), Value::Bool(true));
    eprintln!("P8_VECTORS batch=05 first=150 count=30 aot=0 trace=0");
}
#[test]
fn p8_vectors_b06_interpreter() {
    let result = execute(include_str!("javascript_es5/math-vectors-06.js"));
    assert_eq!(field(&result, "flag"), Value::Bool(true));
    eprintln!("P8_VECTORS batch=06 first=180 count=30 aot=0 trace=0");
}
#[test]
fn p8_vectors_b07_interpreter() {
    let result = execute(include_str!("javascript_es5/math-vectors-07.js"));
    assert_eq!(field(&result, "flag"), Value::Bool(true));
    eprintln!("P8_VECTORS batch=07 first=210 count=30 aot=0 trace=0");
}
#[test]
fn p8_vectors_b08_interpreter() {
    let result = execute(include_str!("javascript_es5/math-vectors-08.js"));
    assert_eq!(field(&result, "flag"), Value::Bool(true));
    eprintln!("P8_VECTORS batch=08 first=240 count=30 aot=0 trace=0");
}
#[test]
fn p8_vectors_b09_interpreter() {
    let result = execute(include_str!("javascript_es5/math-vectors-09.js"));
    assert_eq!(field(&result, "flag"), Value::Bool(true));
    eprintln!("P8_VECTORS batch=09 first=270 count=30 aot=0 trace=0");
}
#[test]
fn p8_vectors_b10_interpreter() {
    let result = execute(include_str!("javascript_es5/math-vectors-10.js"));
    assert_eq!(field(&result, "flag"), Value::Bool(true));
    eprintln!("P8_VECTORS batch=10 first=300 count=30 aot=0 trace=0");
}
#[test]
fn p8_vectors_b11_interpreter() {
    let result = execute(include_str!("javascript_es5/math-vectors-11.js"));
    assert_eq!(field(&result, "flag"), Value::Bool(true));
    eprintln!("P8_VECTORS batch=11 first=330 count=28 aot=0 trace=0");
}

#[derive(Clone, Copy)]
struct RootControl {
    fault: u8,
    force_gc: bool,
}
impl vm::SourcePlugin for RootControl {
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
        let mut ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
        let helper = |name: &str| {
            ir.functions
                .iter()
                .find(|f| f.name == name)
                .expect("real helper")
                .index
        };
        let read = helper("__compat_ref_read");
        let write = helper("__compat_ref_write");
        let root = helper("__compat_es5_intrinsic_root");
        let collect = helper("__compat_es5_heap_collect");
        let mut fresh = || {
            let n = u16::try_from(ir.locals).expect("observer slot");
            ir.locals += 1;
            n
        };
        if self.force_gc {
            let sink = fresh();
            let body = ir.function_impls.get_mut(&root).unwrap();
            let c = body.param_slots[0];
            body.body_stmts.insert(
                0,
                S::IfElse {
                    condition: E::Eq(
                        Box::new(number(call(read, vec![E::Var(c), E::Float(8.0)]))),
                        Box::new(E::Float(0.0)),
                    ),
                    then_branch: vec![S::Let {
                        index: sink,
                        declared_schema: Some(T::Int),
                        expr: call(collect, vec![E::Var(c)]),
                        line: 1,
                    }],
                    else_branch: Vec::new(),
                    line: 1,
                },
            );
        }
        if self.fault != 0 {
            let sink = fresh();
            let gc_sink = fresh();
            let body = ir.function_impls.get_mut(&root).unwrap();
            let c = body.param_slots[0];
            let permanent = number(call(read, vec![E::Var(c), E::Float(15.0)]));
            let offset = |n: f64| E::Add(Box::new(permanent.clone()), Box::new(E::Float(n)));
            let (address, payload) = match self.fault {
                1 => (offset(3.0), E::Float(5.0)),
                2 => (permanent.clone(), E::Float(19.0)),
                3 => (
                    E::Float(15.0),
                    number(call(read, vec![E::Var(c), E::Float(3.0)])),
                ),
                4 => (offset(2.0), permanent.clone()),
                5 => (offset(21.0), E::Float(0.0)),
                _ => unreachable!(),
            };
            let mut prefix = vec![S::Let {
                index: sink,
                declared_schema: Some(T::Number),
                expr: call(write, vec![E::Var(c), address, payload]),
                line: 1,
            }];
            if self.fault <= 3 {
                prefix.push(S::Let {
                    index: gc_sink,
                    declared_schema: Some(T::Int),
                    expr: call(collect, vec![E::Var(c)]),
                    line: 1,
                });
            }
            body.body_stmts.splice(0..0, prefix);
        }
        Ok(ir)
    }
}
#[test]
fn p8_root_selected_gc_and_alias_interpreter() {
    let source = "'use strict';var f=Math.abs;Math.abs=function(x){return 99;};var pad={a:1}; f(-7)+Math.abs(2);";
    let mut machine = prepare(
        source,
        Box::leak(Box::new(RootControl {
            fault: 0,
            force_gc: true,
        })),
    );
    let outcome = machine.run();
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    if let Err(error) = &outcome {
        let path = std::path::PathBuf::from(std::env::var_os("TMPDIR").unwrap())
            .join("fix20-reissued/rounder-factor10/bootstrap-constant-attributes18/address21/OUT/captures")
            .join(format!("p8-root-failure-{}.txt", std::process::id()));
        std::fs::write(path, vm::vmbc::disassemble_program(machine.program())).unwrap();
        panic!(
            "root positive: {error:?} ip={} frames={:?} locals={:?}",
            machine.ip(),
            machine.execution_frames(),
            machine.locals()
        );
    }
    assert_eq!(outcome.unwrap(), VmStatus::Halted);
    assert_eq!(
        field(machine.stack().last().unwrap(), "number"),
        Value::Float(106.0)
    );
    assert_eq!(machine.host_context().resource_count(), 1);
    let stats =
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).unwrap();
    assert_eq!((stats.pending, stats.registered), (0, 1));
    assert_eq!(
        (machine.aot_exec_count(), machine.jit_native_exec_count()),
        (0, 0)
    );
    assert!(!machine.jit_config().enabled);
    machine.reset_for_reuse().unwrap();
    assert_eq!(machine.host_context().resource_count(), 0);
}
#[test]
fn p8_root_wrong_capacity_extent_identity_topology_generation_interpreter() {
    for fault in 1..=5 {
        let mut machine = prepare(
            "Math.abs(-7);",
            Box::leak(Box::new(RootControl {
                fault,
                force_gc: false,
            })),
        );
        let status = machine.run();
        assert!(!machine.jit_config().enabled);
        assert_eq!(machine.aot_exec_count(), 0);
        assert_eq!(machine.jit_native_exec_count(), 0);
        assert!(
            matches!(&status,Err(vm::VmError::HostError(m)) if m=="assertion failed"),
            "fault={fault}: {status:?}"
        );
        assert_eq!(
            (machine.aot_exec_count(), machine.jit_native_exec_count()),
            (0, 0)
        );
        assert!(!machine.jit_config().enabled);
        machine.reset_for_reuse().unwrap();
        assert_eq!(machine.host_context().resource_count(), 0);
        eprintln!("P8_ROOT fault={fault} assertion=true aot=0 trace=0 reset=true");
    }
}
#[test]
fn p8_dynamic_numeric_boundary_float_order_abrupt_and_wrong_domain_interpreter() {
    #[path = "javascript_es5/object_observer.rs"]
    mod add_observer;
    fn add_vm(source: &str, observer: &'static dyn vm::SourcePlugin, id: &'static str) -> vm::Vm {
        let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
        let issued = vm::compile_source_with_flavor_and_options_with_capture_certification(
            source,
            vm::SourceFlavor::Plugin(vm::FrontendId::new(id)),
            vm::CompileSourceFileOptions::new()
                .with_host_api_catalog(catalog.clone())
                .with_source_plugin(observer),
        )
        .unwrap_or_else(|error| panic!("source={source:?} guest_invocations=0 {error:?}"));
        let mut machine = issued.into_vm().unwrap();
        machine.set_jit_config(vm::JitConfig {
            enabled: false,
            ..*machine.jit_config()
        });
        pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
            &mut machine,
            Default::default(),
        )
        .unwrap();
        let mut registry = vm::HostFunctionRegistry::restricted();
        for module in [
            pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
            pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
            pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
            pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
            pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module(),
        ] {
            module
                .install_from_catalog(&mut registry, &catalog)
                .unwrap();
        }
        for name in [
            "math::floor",
            "math::abs",
            "math::acos",
            "math::asin",
            "math::atan",
            "math::atan2",
            "math::ceil",
            "math::cos",
            "math::exp",
            "math::ln",
            "math::sin",
            "math::tan",
            "math::trunc",
            "math::is_infinite",
            "math::copysign",
            "math::sqrt",
            "math::round",
            "math::powf",
            "math::log2",
            "math::log10",
            "math::exp2",
            "math::signum",
            "math::is_nan",
            "math::is_finite",
            "bytes::from_utf8",
        ] {
            registry.allow_builtin(name).unwrap();
        }
        registry.bind_vm_cached(&mut machine).unwrap();
        machine.set_fuel(10_000_000);
        machine
    }

    fn prepare(source: &str, plugin: &'static dyn vm::SourcePlugin) -> vm::Vm {
        add_vm(source, plugin, "javascript-es5")
    }
    fn number(source: &str) -> f64 {
        let mut m = prepare(source, &JAVASCRIPT_ES5_PLUGIN);
        assert!(!m.jit_config().enabled);
        assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
        let status = m.run();
        assert_eq!(status.unwrap(), VmStatus::Halted);
        let Value::Float(n) = field(m.stack().last().unwrap(), "number") else {
            panic!("Float")
        };
        assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
        eprintln!(
            "ADD140_LEGACY_VM source={source:?} status=Halted aot=0 jit=0 jit_enabled=false number={n:?}"
        );
        m.reset_for_reuse().unwrap();
        assert!(!m.scope_reset_pending());
        assert_eq!(m.host_context().resource_count(), 0);
        n
    }
    for (source, expected) in [
        ("'use strict';function f(){return 1.25;} f()+f();", 2.5),
        (
            "'use strict';var o={n:0,f:function(){this.n=this.n+1;return this.n;}};o.f()+o.f();",
            3.0,
        ),
        (
            "'use strict';var o={n:0,f:function(){this.n=this.n+1;throw 7;},g:function(){this.n=99;return 1;}};try{o.f()+o.g();}catch(e){o.n;}",
            1.0,
        ),
    ] {
        assert_eq!(number(source), expected, "{source}");
    }
    for (row, source) in [
        "'use strict';function f(){return '7';}f()+1;",
        "'use strict';function f(){return {};}f()+1;",
    ]
    .into_iter()
    .enumerate()
    {
        let mut machine = prepare(source, &JAVASCRIPT_ES5_PLUGIN);
        if row == 0 {
            drop(machine);
            static OBS: add_observer::Observer = add_observer::Observer {
                units: Some(2),
                force_gc: false,
                uncaught: false,
                type_error: false,
                realm_header: false,
                collect_phases: false,
                collect_stop: None,
            };
            machine = add_vm(source, &OBS, "javascript-es5-object-observer");
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.aot_exec_count(), machine.jit_native_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.run().unwrap(), VmStatus::Halted);
            let at = machine
                .stack()
                .iter()
                .position(|v| v == &Value::string("P7_OBSERVED"))
                .unwrap();
            assert_eq!(
                &machine.stack()[at + 2..at + 5],
                &[Value::Float(2.0), Value::Float(55.0), Value::Float(49.0)]
            );
            let script = format!(
                "let v=(0,eval)({});console.log(JSON.stringify(Array.from({{length:v.length}},(_,i)=>v.charCodeAt(i))));",
                serde_json::to_string(source).unwrap()
            );
            let o = std::process::Command::new("node")
                .args(["-e", &script])
                .output()
                .unwrap();
            assert!(o.status.success());
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap(),
                serde_json::json!([55, 49])
            );
            eprintln!(
                "ADD140_LEGACY_NODE {}",
                serde_json::json!({"source":source,"argv":["node","-e",script],"rc":o.status.code(),"stdout":String::from_utf8_lossy(&o.stdout)})
            );
            eprintln!(
                "ADD140_LEGACY_VM source={source:?} status=Halted units=[55,49] aot=0 jit=0 jit_enabled=false"
            );
        } else {
            assert!(
                matches!(machine.run(),Err(vm::VmError::HostError(m)) if m=="assertion failed")
            );
        }
        assert_eq!(
            (machine.aot_exec_count(), machine.jit_native_exec_count()),
            (0, 0)
        );
        assert!(!machine.jit_config().enabled);
        machine.reset_for_reuse().unwrap();
        assert_eq!(machine.host_context().resource_count(), 0);
    }
}
#[test]
fn p8_numeric_protocol_properties_shadowing_random_interpreter() {
    let checks = [
        "Math.abs.length===1 && Math.atan2.length===2 && Math.random.length===0 && parseInt.length===2 && parseFloat.length===1 && isNaN.length===1 && isFinite.length===1",
        "Object.getOwnPropertyDescriptor(Math,'PI').writable===false && Object.getOwnPropertyDescriptor(Math,'PI').enumerable===false && Object.getOwnPropertyDescriptor(Math,'PI').configurable===false",
        "Object.getOwnPropertyDescriptor(Math,'abs').writable===true && Object.getOwnPropertyDescriptor(Math,'abs').enumerable===false && Object.getOwnPropertyDescriptor(Math,'abs').configurable===true",
        "Object.getOwnPropertyDescriptor(Math.abs,'length').writable===false && Object.getOwnPropertyDescriptor(Math.abs,'length').configurable===false",
        "Object.getPrototypeOf(Math)===Object.prototype && Object.getPrototypeOf(Math.abs)===Object.getPrototypeOf(Object)",
    ];
    for condition in checks {
        assert_eq!(
            field(&execute(&format!("'use strict';{condition};")), "flag"),
            Value::Bool(true)
        );
    }
    let source = "'use strict';var f=Math.abs;Math.abs=99;if(!(f(-4)===4 && Math.abs===99)){throw 9;}delete Math.cos;if('cos' in Math){throw 10;}true;";
    assert_eq!(field(&execute(source), "flag"), Value::Bool(true));
    // Real approved primitive, no deterministic override. Host assertions observe bounds.
    let x = number("Math.random();");
    assert!((0.0..1.0).contains(&x));
    let mut scalar = prepare(
        "'use strict';var Math=7;var parseInt=8;Math+parseInt;",
        &JAVASCRIPT_ES5_PLUGIN,
    );
    assert_eq!(scalar.run().unwrap(), VmStatus::Halted);
    assert_eq!(
        field(scalar.stack().last().unwrap(), "number"),
        Value::Float(15.0)
    );
    // The strict directive is an existing managed String; it creates one base realm.
    assert_eq!(scalar.host_context().resource_count(), 1);
    scalar.reset_for_reuse().unwrap();
    assert_eq!(scalar.host_context().resource_count(), 0);
}
#[test]
fn p8_numeric_object_conversion_number_hint_interpreter() {
    let source = r#"'use strict';var o={n:0,valueOf:function(){this.n=this.n+1;return -7;}};Math.abs(o)===7 && o.n===1;"#;
    assert_eq!(
        field(&execute(source), "flag"),
        Value::Bool(true),
        "{source}"
    );
}

#[test]
fn p8_numeric_object_conversion_string_hint_interpreter() {
    let source = r#"'use strict';var o={n:0,toString:function(){this.n=1;return '12';},valueOf:function(){this.n=99;return 4;}};parseInt(o)===12 && o.n===1;"#;
    assert_eq!(
        field(&execute(source), "flag"),
        Value::Bool(true),
        "{source}"
    );
}

#[test]
fn p8_numeric_object_conversion_getter_throw_interpreter() {
    let source = r#"'use strict';var o={n:0,get valueOf(){this.n=1;throw 7;},toString:function(){this.n=99;return '2';}};try{Math.abs(o);}catch(e){e===7 && o.n===1;}"#;
    assert_eq!(
        field(&execute(source), "flag"),
        Value::Bool(true),
        "{source}"
    );
}

#[test]
fn p8_numeric_object_conversion_second_attempt_interpreter() {
    let source = r#"'use strict';var o={n:0,valueOf:function(){this.n=1;return {};},toString:function(){this.n=this.n+1;return '2';}};Math.abs(o)===2 && o.n===2;"#;
    assert_eq!(
        field(&execute(source), "flag"),
        Value::Bool(true),
        "{source}"
    );
}

#[test]
fn p8_numeric_object_conversion_extra_args_interpreter() {
    let source = r#"'use strict';var o={n:0,a:function(){this.n=this.n+1;return this.n;}};Math.pow(o.a(),o.a(),o.a())===1 && o.n===3;"#;
    assert_eq!(
        field(&execute(source), "flag"),
        Value::Bool(true),
        "{source}"
    );
}

#[test]
fn p8_required_numeric_subnormal_string_rounding_interpreter() {
    assert_eq!(number("parseInt(5e-324);"), 5.0);
    assert_eq!(
        number("parseFloat('1.2345678901234567');"),
        1.2345678901234567
    );
}

/// ES5.1 15.1.2.2 Number value of a 19-digit radix-10 integer. The expected
/// bits are the host correctly-rounded binary64 of that integer, not a guest
/// result. The assertion is exact bits, not an epsilon.
#[test]
fn p8_decimal_parse_int_19_interpreter() {
    let got = number("parseInt('1234567890123456789');");
    let expected = 1234567890123456789.0_f64;
    eprintln!(
        "P8_BITS parseInt19 got_bits={:#018x} expected_bits={:#018x}",
        got.to_bits(),
        expected.to_bits()
    );
    assert_eq!(
        got.to_bits(),
        expected.to_bits(),
        "parseInt19 got_bits={:#018x} expected_bits={:#018x}",
        got.to_bits(),
        expected.to_bits()
    );
}

/// ES5.1 15.1.2.2 Number value of a 20-digit radix-10 integer. Nineteen
/// digits fit in the integer fast path; twenty do not, and binary64
/// `n * 10 + digit` is not correctly rounded. Expected bits are the host
/// correctly-rounded binary64, not a guest result.
#[test]
fn p8_decimal_parse_int_20_interpreter() {
    let got = number("parseInt('12345678901234567890');");
    let expected = 12345678901234567890.0_f64;
    eprintln!(
        "P8_BITS parseInt20 got_bits={:#018x} expected_bits={:#018x}",
        got.to_bits(),
        expected.to_bits()
    );
    assert_eq!(
        got.to_bits(),
        expected.to_bits(),
        "parseInt20 got_bits={:#018x} expected_bits={:#018x}",
        got.to_bits(),
        expected.to_bits()
    );
}

/// Compact workspace controls. Expected bits are host correctly-rounded
/// binary64 values, not guest results. The 400-digit cases need more than
/// the old 40-limb buffer; a Halted exact result is the admission proof.
#[test]
fn p8_decimal_compact_bounds_interpreter() {
    let cases = [
        ("parseFloat('-0');", (-0.0f64).to_bits()),
        ("parseFloat('5e-324');", 5e-324_f64.to_bits()),
        (
            "parseFloat('2.2250738585072014e-308');",
            2.2250738585072014e-308_f64.to_bits(),
        ),
        ("parseFloat('1e309');", f64::INFINITY.to_bits()),
        (
            "parseFloat('0.30000000000000001665334536937734810635447502136230468749');",
            0x3fd3333333333333,
        ),
        (
            "parseFloat('0.3000000000000000166533453693773481063544750213623046875');",
            0x3fd3333333333334,
        ),
        (
            "parseFloat('0.30000000000000001665334536937734810635447502136230468751');",
            0x3fd3333333333334,
        ),
    ];
    for (src, expected) in cases {
        let got = number(src);
        assert_eq!(
            got.to_bits(),
            expected,
            "{src} got_bits={:#018x} expected_bits={:#018x}",
            got.to_bits(),
            expected
        );
    }
    let nines = "9".repeat(400);
    let long = format!("parseFloat('{nines}e-399');");
    let got = number(&long);
    assert_eq!(
        got.to_bits(),
        0x4024000000000000,
        "400 nines e-399 {:#x}",
        got.to_bits()
    );
    let cancel = format!("parseFloat('{nines}e-723');");
    let got = number(&cancel);
    assert_eq!(got.to_bits(), 0x2, "400 nines e-723 {:#x}", got.to_bits());
    let hex = format!("parseInt('{}1{}', 16);", "0".repeat(256), "0".repeat(16));
    let got = number(&hex);
    assert_eq!(
        got.to_bits(),
        0x43f0000000000000,
        "hex lead {:#x}",
        got.to_bits()
    );
    let bin = format!("parseInt('1{}', 2);", "0".repeat(63));
    let got = number(&bin);
    assert_eq!(
        got.to_bits(),
        0x43e0000000000000,
        "bin 2^63 {:#x}",
        got.to_bits()
    );
}

/// ES5.1 15.1.2.3 / 9.3.1 Number value of '0.3'. Math.abs('0.3') is the
/// ToNumber numeric control; classification such as isFinite cannot see an ulp.
/// Expected bits are the host correctly-rounded binary64, not a guest result.
#[test]
fn p8_decimal_parse_float_0_3_interpreter() {
    let parsed = number("parseFloat('0.3');");
    let abs_control = number("Math.abs('0.3');");
    let expected = 0.3_f64;
    eprintln!(
        "P8_BITS parseFloat0.3 parse_bits={:#018x} abs_bits={:#018x} expected_bits={:#018x}",
        parsed.to_bits(),
        abs_control.to_bits(),
        expected.to_bits()
    );
    assert_eq!(
        (parsed.to_bits(), abs_control.to_bits()),
        (expected.to_bits(), expected.to_bits()),
        "parseFloat0.3 parse_bits={:#018x} abs_bits={:#018x} expected_bits={:#018x}",
        parsed.to_bits(),
        abs_control.to_bits(),
        expected.to_bits()
    );
}
/// Cases the i64 fast path must refuse: |scale| > 22, including a subnormal
/// and an overflow. Expected bits are host binary64, not a guest result.
#[test]
fn p8_decimal_parse_float_extreme_interpreter() {
    for (source, expected) in [
        ("parseFloat('5e-324');", 5e-324_f64),
        ("parseFloat('1e-323');", 1e-323_f64),
        ("parseFloat('1e308');", 1e308_f64),
        ("parseFloat('1e309');", f64::INFINITY),
    ] {
        let got = number(source);
        eprintln!(
            "P8_BITS {source} got_bits={:#018x} expected_bits={:#018x}",
            got.to_bits(),
            expected.to_bits()
        );
        assert_eq!(
            got.to_bits(),
            expected.to_bits(),
            "{source} got_bits={:#018x} expected_bits={:#018x}",
            got.to_bits(),
            expected.to_bits()
        );
    }
}

#[test]
fn p8_required_default_object_primitive_interpreter() {
    assert_eq!(field(&execute("isNaN({});"), "flag"), Value::Bool(true));
}

#[test]
fn p8_required_object_proto_methods_interpreter() {
    assert_eq!(
        field(
            &execute(r#"var o={};Math.abs(1);o.valueOf()===o&&o.toString()==="[object Object]";"#),
            "flag"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        field(
            &execute(
                r#"'use strict';var f=function(){};f.objectTag=Object.prototype.toString;Math.abs.valueOf()===Math.abs&&f.objectTag()==="[object Function]";"#
            ),
            "flag"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        field(
            &execute(
                r#"Math.abs(1);var v=Object.prototype.valueOf;try{v();false;}catch(e){e.name==="TypeError";}"#
            ),
            "flag"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        field(
            &execute(r#"Math.abs(1);var t=Object.prototype.toString;t()==="[object Undefined]";"#),
            "flag"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        field(
            &execute(
                r#"var o=Object.create(null);try{isNaN(o);false;}catch(e){e.name==="TypeError";}"#
            ),
            "flag"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        field(
            &execute(r#"Math.abs(1);Math.toString()==="[object Math]";"#),
            "flag"
        ),
        Value::Bool(true)
    );
}

/// High-radix numeric prefixes. Expectations are the ES5.1 9.8.1 character
/// sequence consumed by parseInt, not a Node tie digit and not a host formatter.
#[test]
fn p8_required_numeric_prefix_high_radix_interpreter() {
    assert_eq!(number("parseInt(1e100,36);"), 50.0);
    assert_eq!(number("parseInt(1e-100,36);"), 50.0);
    assert_eq!(number("parseInt(1e30,36);"), 50.0);
    assert_eq!(number("parseInt(1e-300,36);"), 50.0);
    assert_eq!(number("parseInt(-1e100,36);"), -50.0);
    assert_eq!(number("parseInt(1e21,36);"), 50.0);
    assert_eq!(number("parseInt(1e22,16);"), 30.0);
    assert_eq!(number("parseInt(1.5e21,36);"), 1.0);
    assert_eq!(number("parseInt(1e100);"), 1.0);
}

/// Step 5 admits either digit string for these eight ties. parseInt stops
/// before the differing fractional digit, so only the shared prefix is a
/// guest regression. Neither tie digit is asserted as an observed string.
#[test]
fn p8_required_numeric_tie_prefix_controls_interpreter() {
    assert_eq!(number("parseInt(0.000000029802322387695313);"), 2.0);
    assert_eq!(number("parseInt(-0.000000029802322387695313);"), -2.0);
    assert_eq!(number("parseInt(0.0000005960464477539063);"), 5.0);
    assert_eq!(number("parseInt(-0.0000005960464477539063);"), -5.0);
    assert_eq!(number("parseInt(562949953421312.3);"), 562949953421312.0);
    assert_eq!(number("parseInt(-562949953421312.3);"), -562949953421312.0);
    assert_eq!(number("parseInt(1125899906842624.3);"), 1125899906842624.0);
    assert_eq!(
        number("parseInt(-1125899906842624.3);"),
        -1125899906842624.0
    );
}

#[test]
fn p8_default_ts_and_reference_ts_isolation_interpreter() {
    for (source, plugin, answer, resources) in [
        (
            "const x: number = 3; x+4;",
            &pd_vm_compat_frontends::TYPESCRIPT_PLUGIN as &'static dyn vm::SourcePlugin,
            7,
            0,
        ),
        (
            "const a=[10];const b=a;b[0]=99;a[0];",
            &pd_vm_compat_frontends::TYPESCRIPT_REFERENCE_ARRAY_PLUGIN
                as &'static dyn vm::SourcePlugin,
            99,
            1,
        ),
    ] {
        let mut machine =
            prepare_flavor(source, plugin, pd_vm_compat_frontends::typescript_flavor());
        assert_eq!(machine.run().unwrap(), VmStatus::Halted);
        assert_eq!(machine.stack().last(), Some(&Value::Int(answer)));
        assert_eq!(
            (machine.aot_exec_count(), machine.jit_native_exec_count()),
            (0, 0)
        );
        assert!(!machine.jit_config().enabled);
        assert_eq!(machine.host_context().resource_count(), resources);
        machine.reset_for_reuse().unwrap();
        assert_eq!(machine.host_context().resource_count(), 0);
    }
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    for (source, plugin) in [
        (
            "const a: number[]=[10];const b: number[]=a;b[0]=99;a[0];",
            &pd_vm_compat_frontends::TYPESCRIPT_PLUGIN as &'static dyn vm::SourcePlugin,
        ),
        (
            "const a=[1,'bad'];a[0];",
            &pd_vm_compat_frontends::TYPESCRIPT_REFERENCE_ARRAY_PLUGIN
                as &'static dyn vm::SourcePlugin,
        ),
    ] {
        let result = vm::compile_source_with_flavor_and_options_with_capture_certification(
            source,
            pd_vm_compat_frontends::typescript_flavor(),
            vm::CompileSourceFileOptions::new()
                .with_source_plugin(plugin)
                .with_host_api_catalog(catalog.clone()),
        );
        assert!(result.is_err(), "typed negative control {source}");
    }
}

#[test]
fn p8_parse_int_primitive_tokens_all_radices_interpreter() {
    let result = execute(
        "'use strict';if (!(parseInt(true,36) === 1389110)) {throw 1;}if (!(parseInt(false,36) === 25689038)) {throw 2;}if (!(parseInt(null,36) === 1112745)) {throw 3;}if (!(parseInt(undefined,36) === 86464843759093)) {throw 4;}true;",
    );
    assert_eq!(field(&result, "flag"), Value::Bool(true));
}

#[test]
fn p8_bare_intrinsic_values_and_heap_free_shadowing_interpreter() {
    for source in [
        "Math === Math;",
        "parseInt === parseInt;",
        "parseFloat === parseFloat;",
        "isNaN === isNaN;",
        "isFinite === isFinite;",
    ] {
        assert_eq!(
            field(&execute(source), "flag"),
            Value::Bool(true),
            "{source}"
        );
    }
    let mut machine = prepare(
        "var Math=7;var parseInt=8;Math+parseInt;",
        &JAVASCRIPT_ES5_PLUGIN,
    );
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(
        field(machine.stack().last().unwrap(), "number"),
        Value::Float(15.0)
    );
    assert_eq!(machine.host_context().resource_count(), 0);
    assert_eq!(
        (machine.aot_exec_count(), machine.jit_native_exec_count()),
        (0, 0)
    );
    assert!(!machine.jit_config().enabled);
    machine.reset_for_reuse().unwrap();
    assert_eq!(machine.host_context().resource_count(), 0);
}

// Independent vector observations do not replace the original full batches:
// those tests remain enabled, with the original ten-million fuel budget.
fn vector_halves(source: &str, batch: usize, first: usize) {
    let mut segments = source.split("// vector ");
    let prefix = segments.next().unwrap();
    let rows = segments.collect::<Vec<_>>();
    for (half, chunk) in rows.chunks(15).enumerate() {
        let mut program = prefix.to_owned();
        for row in chunk {
            program.push_str("// vector ");
            program.push_str(row);
        }
        program.push_str("true;\n");
        assert_eq!(field(&execute(&program), "flag"), Value::Bool(true));
        eprintln!(
            "P8_VECTORS_PART batch={batch:02} first={} count={} aot=0 trace=0",
            first + half * 15,
            chunk.len()
        );
    }
}
#[test]
fn p8_vectors_b07_independent_halves_interpreter() {
    vector_halves(include_str!("javascript_es5/math-vectors-07.js"), 7, 210);
}
#[test]
fn p8_vectors_b08_independent_halves_interpreter() {
    vector_halves(include_str!("javascript_es5/math-vectors-08.js"), 8, 240);
}

/// Minimal parent-oracle subset, not a 76-case gate. Bits are the recorded
/// Node v26.7.0 values. `Number` is reached through `Math.min(string, Infinity)`,
/// not a public Number constructor.
#[test]
fn p8_decimal_oracle_subset_interpreter() {
    let cases = [
        (
            "parseFloat('0.30000000000000001665334536937734810635447502136230468749');",
            0x3fd3333333333333u64,
        ),
        (
            "parseFloat('0.3000000000000000166533453693773481063544750213623046875');",
            0x3fd3333333333334u64,
        ),
        (
            "Math.min('0.3000000000000000166533453693773481063544750213623046875', Infinity);",
            0x3fd3333333333334u64,
        ),
    ];
    for (source, expected) in cases {
        let got = number(source);
        eprintln!(
            "P8_BITS oracle source={source} got_bits={:#018x} expected_bits={:#018x}",
            got.to_bits(),
            expected
        );
        assert_eq!(got.to_bits(), expected, "{source}");
    }
}

/// Static IR of the SourcePlugin-emitted rounder, not a textual Rust count and
/// not a VM execution. The helper is identified by its real name, index, and
/// 8-argument Float ABI before While nodes are counted.
#[test]
fn p8_rounder_one_while_topology_static_ir() {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let context = vm::SourceFrontendContext {
        source_id: 0,
        original_path: None,
        phase: vm::compiler::ParsePhase::Standalone,
        host_api_catalog: Some(&catalog),
        imports: &[],
    };
    let ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, "parseInt('10', 2);", &context)
        .expect("rounder subject must parse");
    let declarations: Vec<_> = ir
        .functions
        .iter()
        .filter(|function| function.name == "__compat_es5_round_decimal")
        .collect();
    assert_eq!(declarations.len(), 1, "rounder declaration count");
    let declaration = declarations[0];
    let body = ir
        .function_impls
        .get(&declaration.index)
        .expect("rounder implementation");
    assert_eq!(declaration.arity, 8);
    assert!(!declaration.exported);
    assert_eq!(declaration.return_type, vm::ValueType::Float);
    assert_eq!(declaration.return_schema, Some(vm::TypeSchema::Float));
    assert_eq!(declaration.arg_schemas.len(), 8);
    assert!(
        declaration.arg_schemas[0].is_some()
            && declaration.arg_schemas[0] != Some(vm::TypeSchema::Int),
        "carrier schema must stay distinct from Int"
    );
    assert!(
        declaration.arg_schemas[1..]
            .iter()
            .all(|schema| schema == &Some(vm::TypeSchema::Int))
    );
    assert_eq!(body.param_slots.len(), 8);
    let mut count = 0usize;
    let mut max_depth = 0usize;
    count_rounder_whiles(&body.body_stmts, 0, &mut count, &mut max_depth);
    count_rounder_while_expr(&body.body_expr, 0, &mut count, &mut max_depth);
    eprintln!(
        "P8_ROUNDER_TOPOLOGY name={} index={} while_count={} max_depth={}",
        declaration.name, declaration.index, count, max_depth
    );
    assert_eq!(count, 1, "emitted rounder While count");
    assert_eq!(max_depth, 1, "emitted rounder max While depth");
}

fn count_rounder_whiles(
    stmts: &[vm::Stmt],
    depth: usize,
    count: &mut usize,
    max_depth: &mut usize,
) {
    for stmt in stmts {
        match stmt {
            vm::Stmt::While {
                condition, body, ..
            } => {
                *count += 1;
                let nested = depth + 1;
                *max_depth = (*max_depth).max(nested);
                count_rounder_while_expr(condition, nested, count, max_depth);
                count_rounder_whiles(body, nested, count, max_depth);
            }
            vm::Stmt::IfElse {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                count_rounder_while_expr(condition, depth, count, max_depth);
                count_rounder_whiles(then_branch, depth, count, max_depth);
                count_rounder_whiles(else_branch, depth, count, max_depth);
            }
            vm::Stmt::For {
                init,
                condition,
                post,
                body,
                ..
            } => {
                count_rounder_whiles(std::slice::from_ref(init.as_ref()), depth, count, max_depth);
                count_rounder_while_expr(condition, depth, count, max_depth);
                count_rounder_whiles(std::slice::from_ref(post.as_ref()), depth, count, max_depth);
                count_rounder_whiles(body, depth, count, max_depth);
            }
            vm::Stmt::Let { expr, .. }
            | vm::Stmt::Assign { expr, .. }
            | vm::Stmt::Expr { expr, .. } => {
                count_rounder_while_expr(expr, depth, count, max_depth)
            }
            vm::Stmt::ClosureLet { closure, .. } => {
                count_rounder_while_expr(&closure.body, depth, count, max_depth)
            }
            vm::Stmt::Noop { .. }
            | vm::Stmt::FuncDecl { .. }
            | vm::Stmt::Break { .. }
            | vm::Stmt::Continue { .. }
            | vm::Stmt::Drop { .. } => {}
        }
    }
}

fn count_rounder_while_expr(
    expr: &vm::Expr,
    depth: usize,
    count: &mut usize,
    max_depth: &mut usize,
) {
    match expr {
        vm::Expr::Call(_, _, args, _, _)
        | vm::Expr::ModuleCall(_, _, args, _)
        | vm::Expr::LocalCall(_, _, args, _) => {
            for arg in args {
                count_rounder_while_expr(arg, depth, count, max_depth);
            }
        }
        vm::Expr::ClosureCall(closure, args) => {
            count_rounder_while_expr(&closure.body, depth, count, max_depth);
            for arg in args {
                count_rounder_while_expr(arg, depth, count, max_depth);
            }
        }
        vm::Expr::Closure(closure) => {
            count_rounder_while_expr(&closure.body, depth, count, max_depth)
        }
        vm::Expr::Add(left, right)
        | vm::Expr::Sub(left, right)
        | vm::Expr::Mul(left, right)
        | vm::Expr::Div(left, right)
        | vm::Expr::Mod(left, right)
        | vm::Expr::And(left, right)
        | vm::Expr::Or(left, right)
        | vm::Expr::Eq(left, right)
        | vm::Expr::Lt(left, right)
        | vm::Expr::Gt(left, right) => {
            count_rounder_while_expr(left, depth, count, max_depth);
            count_rounder_while_expr(right, depth, count, max_depth);
        }
        vm::Expr::Neg(inner)
        | vm::Expr::Not(inner)
        | vm::Expr::ToOwned(inner)
        | vm::Expr::Borrow(inner)
        | vm::Expr::BorrowMut(inner) => count_rounder_while_expr(inner, depth, count, max_depth),
        vm::Expr::OptionalGet { container, key, .. } => {
            count_rounder_while_expr(container, depth, count, max_depth);
            count_rounder_while_expr(key, depth, count, max_depth);
        }
        vm::Expr::OptionUnwrapOr {
            value, fallback, ..
        } => {
            count_rounder_while_expr(value, depth, count, max_depth);
            count_rounder_while_expr(fallback, depth, count, max_depth);
        }
        vm::Expr::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            count_rounder_while_expr(condition, depth, count, max_depth);
            count_rounder_while_expr(then_expr, depth, count, max_depth);
            count_rounder_while_expr(else_expr, depth, count, max_depth);
        }
        vm::Expr::Match {
            value,
            arms,
            default,
            ..
        } => {
            count_rounder_while_expr(value, depth, count, max_depth);
            for (_, arm) in arms {
                count_rounder_while_expr(arm, depth, count, max_depth);
            }
            count_rounder_while_expr(default, depth, count, max_depth);
        }
        vm::Expr::Block { stmts, expr } => {
            count_rounder_whiles(stmts, depth, count, max_depth);
            count_rounder_while_expr(expr, depth, count, max_depth);
        }
        vm::Expr::ResultConstruct { payload, .. }
        | vm::Expr::ResultQuery { value: payload, .. } => {
            count_rounder_while_expr(payload, depth, count, max_depth)
        }
        vm::Expr::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            count_rounder_while_expr(value, depth, count, max_depth);
            count_rounder_while_expr(ok_expr, depth, count, max_depth);
            count_rounder_while_expr(err_expr, depth, count, max_depth);
        }
        vm::Expr::ResultTry { value, .. } => {
            count_rounder_while_expr(value, depth, count, max_depth)
        }
        vm::Expr::Null
        | vm::Expr::Int(_)
        | vm::Expr::Float(_)
        | vm::Expr::Bool(_)
        | vm::Expr::String(_)
        | vm::Expr::Bytes(_)
        | vm::Expr::FunctionRef(_, _)
        | vm::Expr::ModuleFunctionRef(_, _)
        | vm::Expr::UnresolvedFunctionRef { .. }
        | vm::Expr::Var(_)
        | vm::Expr::MoveVar(_)
        | vm::Expr::MoveField { .. }
        | vm::Expr::MoveIndex { .. } => {}
    }
}

fn bits_eq(source: &str, expected: f64) {
    let got = number(source);
    eprintln!(
        "P8_BITS boundary source={source} got_bits={:#018x} expected_bits={:#018x}",
        got.to_bits(),
        expected.to_bits()
    );
    assert_eq!(got.to_bits(), expected.to_bits(), "{source}");
}

/// Host-bit oracle for power-of-two ingestion and the retained magnitude controls.
/// `Number` is not constructed; values come from parseInt, parseFloat, or Math.min.
#[test]
fn p8_radix_rational_boundaries_interpreter() {
    let p2 = |shift: u32| f64::from_bits(0x3ff0_0000_0000_0000 + (u64::from(shift) << 52));
    let bin63 = format!("parseInt('{}1{}', 2);", "0".repeat(8), "0".repeat(63));
    bits_eq(&bin63, p2(63));
    bits_eq(&format!("parseInt('1{}', 2);", "0".repeat(53)), p2(53));
    // 2^63 + 2^10 is the binary64 midpoint; ties-to-even keeps 2^63.
    bits_eq(
        &format!("parseInt('1{}1{}', 2);", "0".repeat(52), "0".repeat(10)),
        p2(63),
    );
    // 2^63 + 2^11 + 2^10 is the next midpoint and ties to even 2^63 + 2^12.
    bits_eq(
        &format!("parseInt('1{}11{}', 2);", "0".repeat(51), "0".repeat(10)),
        p2(63) + p2(12),
    );
    // Strictly above 2^63 + 2^10, so this rounds away from 2^63.
    bits_eq(
        &format!("parseInt('1{}11{}', 2);", "0".repeat(52), "0".repeat(9)),
        p2(63) + p2(11),
    );
    bits_eq(&format!("parseInt('-1{}', 2);", "0".repeat(63)), -p2(63));
}

#[test]
fn p8_radix_power_of_two_boundaries_interpreter() {
    let p2 = |shift: u32| f64::from_bits(0x3ff0_0000_0000_0000 + (u64::from(shift) << 52));
    bits_eq(
        &format!("parseInt('{}2{}', 4);", "0".repeat(6), "0".repeat(31)),
        p2(63),
    );
    bits_eq(
        &format!("parseInt('2{}1{}', 4);", "0".repeat(25), "0".repeat(5)),
        p2(63),
    );
    bits_eq(&format!("parseInt('2{}', 4);", "0".repeat(26)), p2(53));
    bits_eq(
        &format!("parseInt('{}1{}', 8);", "0".repeat(5), "0".repeat(21)),
        p2(63),
    );
    bits_eq(&format!("parseInt('4{}', 8);", "0".repeat(17)), p2(53));
    bits_eq(
        &format!("parseInt('{}8{}', 16);", "0".repeat(4), "0".repeat(15)),
        p2(63),
    );
    bits_eq(&format!("parseInt('2{}', 16);", "0".repeat(13)), p2(53));
    bits_eq(
        &format!("parseInt('{}8{}', 32);", "0".repeat(3), "0".repeat(12)),
        p2(63),
    );
}

/// Host-bit controls that do not fit beside the radix matrix in one 300s gate.
#[test]
fn p8_radix_magnitude_controls_interpreter() {
    let p2 = |shift: u32| f64::from_bits(0x3ff0_0000_0000_0000 + (u64::from(shift) << 52));
    bits_eq(&format!("parseInt('8{}', 32);", "0".repeat(10)), p2(53));
    bits_eq(
        &format!("parseInt('{}g{}', 32);", "0".repeat(4), "0".repeat(12)),
        p2(64),
    );
    bits_eq(
        &format!("parseInt('{}G{}', 32);", "0".repeat(4), "0".repeat(12)),
        p2(64),
    );
    bits_eq("parseFloat('-0');", -0.0);
    bits_eq("parseFloat('5e-324');", 5e-324);
    bits_eq("parseFloat('2.2250738585072014e-308');", f64::MIN_POSITIVE);
    bits_eq("parseFloat('1e309');", f64::INFINITY);
    bits_eq("parseFloat('-1e309');", f64::NEG_INFINITY);
    bits_eq("Math.min('-0', 1);", -0.0);
}

#[test]
fn p8_long_decimal_tail_interpreter() {
    bits_eq(&format!("parseFloat('{}e-399');", "9".repeat(400)), 10.0);
}

fn p2_bits(shift: u32) -> f64 {
    f64::from_bits(0x3ff0_0000_0000_0000 + (u64::from(shift) << 52))
}

/// Radix 2/4/8 tokens of `1` followed by 309 zeros are finite `2^309` / `2^618`
/// / `2^927`. A decimal exponent gate misclassifies them as infinity.
#[test]
fn p8_pow2_radix_long_finite_interpreter() {
    bits_eq(
        &format!("parseInt('1{}', 2);", "0".repeat(309)),
        p2_bits(309),
    );
    bits_eq(
        &format!("parseInt('1{}', 4);", "0".repeat(309)),
        p2_bits(618),
    );
    bits_eq(
        &format!("parseInt('1{}', 8);", "0".repeat(309)),
        p2_bits(927),
    );
    bits_eq(
        &format!("parseInt('-1{}', 2);", "0".repeat(309)),
        -p2_bits(309),
    );
    bits_eq(
        &format!("parseInt('0001{}', 2);", "0".repeat(309)),
        p2_bits(309),
    );
}

/// Bit length 1024 is still finite and must enter the exact limb path.
/// `4^511 = 2^1022`, `8^341 = 2^1023`. The 53-ones token is `f64::MAX`.
#[test]
fn p8_pow2_radix_finite_endpoints_interpreter() {
    bits_eq(
        &format!("parseInt('1{}', 2);", "0".repeat(1023)),
        p2_bits(1023),
    );
    bits_eq(
        &format!("parseInt('1{}', 4);", "0".repeat(511)),
        p2_bits(1022),
    );
    bits_eq(
        &format!("parseInt('1{}', 8);", "0".repeat(341)),
        p2_bits(1023),
    );
    bits_eq(
        &format!("parseInt('{}{}', 2);", "1".repeat(53), "0".repeat(971)),
        f64::MAX,
    );
}

/// First-digit width is the digit's own bit length, not `log2(radix)`.
/// Over-estimating that width turns these finite endpoints into infinity.
#[test]
fn p8_pow2_radix16_32_partial_interpreter() {
    bits_eq(
        &format!("parseInt('1{}', 16);", "0".repeat(255)),
        p2_bits(1020),
    );
    bits_eq(
        &format!("parseInt('2{}', 16);", "0".repeat(255)),
        p2_bits(1021),
    );
    bits_eq(
        &format!("parseInt('8{}', 16);", "0".repeat(255)),
        p2_bits(1023),
    );
    bits_eq(
        &format!("parseInt('1{}', 32);", "0".repeat(204)),
        p2_bits(1020),
    );
    bits_eq(
        &format!("parseInt('8{}', 32);", "0".repeat(204)),
        p2_bits(1023),
    );
}

/// 1024-bit values enter the exact rounder (`2^1024 - 1` rounds to infinity).
/// Bit length >= 1025 is obvious overflow and must not be loaded into 128 limbs.
#[test]
fn p8_pow2_radix_overflow_class_interpreter() {
    bits_eq(
        &format!("parseInt('{}', 2);", "1".repeat(1024)),
        f64::INFINITY,
    );
    bits_eq(
        &format!("parseInt('1{}', 2);", "0".repeat(1024)),
        f64::INFINITY,
    );
    bits_eq(
        &format!("parseInt('-1{}', 2);", "0".repeat(1024)),
        f64::NEG_INFINITY,
    );
    bits_eq(
        &format!("parseInt('1{}', 16);", "0".repeat(256)),
        f64::INFINITY,
    );
    bits_eq(
        &format!("parseInt('g{}', 32);", "0".repeat(204)),
        f64::INFINITY,
    );
}

/// Zero stays signed by the caller. These short tokens use the fitting fast path.
#[test]
fn p8_pow2_radix_zero_sign_interpreter() {
    bits_eq("parseInt('0', 2);", 0.0);
    bits_eq("parseInt('-0', 2);", -0.0);
    bits_eq("parseInt('-000', 16);", -0.0);
    bits_eq("parseInt('000', 8);", 0.0);
}

/// `922337203685477580` plus digit 8 or 9 does not fit in i64. Digit 7 does.
/// The rounded magnitude of all three is `2^63`; a wrapped coefficient is negative.
#[test]
fn p8_decimal_i64_digit89_parsefloat_interpreter() {
    let two63 = f64::from_bits(0x43e0_0000_0000_0000);
    bits_eq("parseFloat('9223372036854775807');", two63);
    bits_eq("parseFloat('9223372036854775808');", two63);
    bits_eq("parseFloat('-9223372036854775808');", -two63);
    bits_eq("parseFloat('9223372036854775809');", two63);
    bits_eq("parseFloat('-9223372036854775809');", -two63);
}

/// The fraction loop has the same digit-8/9 hole. Digit 7 must stay on the fitting path.
#[test]
fn p8_decimal_i64_digit89_fraction_interpreter() {
    let rounded = f64::from_bits(0x43a9_9999_9999_999a);
    bits_eq("parseFloat('922337203685477580.7');", rounded);
    bits_eq("parseFloat('922337203685477580.8');", rounded);
    bits_eq("parseFloat('-922337203685477580.8');", -rounded);
    bits_eq("parseFloat('922337203685477580.9');", rounded);
    bits_eq("parseFloat('-922337203685477580.9');", -rounded);
}

/// String ToNumber through already-admitted consumers. `Math.abs` is not used:
/// it would hide a wrapped negative coefficient.
#[test]
fn p8_decimal_i64_digit89_tonumber_interpreter() {
    let two63 = f64::from_bits(0x43e0_0000_0000_0000);
    bits_eq("Math.floor('9223372036854775808');", two63);
    bits_eq("Math.floor('-9223372036854775808');", -two63);
    bits_eq("Math.ceil('9223372036854775809');", two63);
    bits_eq("Math.ceil('-9223372036854775809');", -two63);
    bits_eq("Math.max('9223372036854775808', 0);", two63);
    bits_eq("Math.max('-9223372036854775808', 0);", 0.0);
}

/// SourcePlugin IR for exact `Math.random();`. The bootstrap row is joined by
/// its declaration name, that declaration's index, and its schema. This is not
/// a textual Rust count, a constructed IR value, or the first arity-1 helper.
#[test]
fn p8_bootstrap_trusted_flags_remainder_zero_while_static_ir() {
    let source = "Math.random();";
    assert_eq!(source, "Math.random();");
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let context = vm::SourceFrontendContext {
        source_id: 0,
        original_path: None,
        phase: vm::compiler::ParsePhase::Standalone,
        host_api_catalog: Some(&catalog),
        imports: &[],
    };
    let ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, &context)
        .expect("bootstrap subject must parse");
    let declarations: Vec<_> = ir
        .functions
        .iter()
        .filter(|function| function.name == "__compat_es5_intrinsic_bootstrap")
        .collect();
    assert_eq!(declarations.len(), 1, "bootstrap declaration count");
    let declaration = declarations[0];
    assert_eq!(
        ir.functions
            .iter()
            .filter(|function| function.index == declaration.index)
            .count(),
        1,
        "bootstrap index must join one declaration"
    );
    let body = ir
        .function_impls
        .get(&declaration.index)
        .expect("bootstrap implementation joined by declaration index");
    let root_joins = ir
        .stmts
        .iter()
        .filter(|stmt| {
            matches!(
                stmt,
                vm::Stmt::FuncDecl {
                    name,
                    index,
                    has_impl: true,
                    exported: false,
                    ..
                } if name == "__compat_es5_intrinsic_bootstrap" && *index == declaration.index
            )
        })
        .count();
    assert_eq!(root_joins, 1, "bootstrap root FuncDecl index join");
    assert!(
        ir.functions
            .iter()
            .any(|function| function.arity == 1 && function.index != declaration.index),
        "arity 1 is not a unique join key"
    );
    assert_eq!(declaration.arity, 1);
    assert!(!declaration.exported);
    assert!(declaration.symbol.is_none());
    assert!(declaration.type_params.is_empty());
    assert_eq!(declaration.args, vec!["arg0".to_owned()]);
    assert_eq!(declaration.return_type, vm::ValueType::Null);
    assert_eq!(declaration.return_schema, Some(vm::TypeSchema::Null));
    assert_eq!(
        declaration.arg_schemas,
        vec![Some(vm::TypeSchema::Named(
            "__CompatRefCarrier".to_owned(),
            Vec::new()
        ))]
    );
    assert_eq!(body.param_slots.len(), 1);
    assert!(body.capture_copies.is_empty());
    assert!(matches!(body.body_expr, vm::Expr::Null));
    let mut count = 0usize;
    let mut max_depth = 0usize;
    count_rounder_whiles(&body.body_stmts, 0, &mut count, &mut max_depth);
    count_rounder_while_expr(&body.body_expr, 0, &mut count, &mut max_depth);
    eprintln!(
        "P8_BOOTSTRAP_TOPOLOGY source={source} name={} index={} while_count={} max_depth={}",
        declaration.name, declaration.index, count, max_depth
    );
    assert_eq!(count, 0, "emitted bootstrap While count");
}

/// Compare subtractive and modulo decoding for the three flag bits in the
/// packed header, with additional nonnegative UInt32 arithmetic controls.
#[test]
fn p8_trusted_header_remainder_arithmetic_domain_control() {
    const COUNT_FACTOR: u32 = 8;
    const MAX_PACKED_HEADER: u32 = 256 * COUNT_FACTOR + (COUNT_FACTOR - 1);

    fn subtract_count_factor(mut header: u32) -> u32 {
        while header >= COUNT_FACTOR {
            header -= COUNT_FACTOR;
        }
        header
    }
    fn int_remainder(header: u32) -> u32 {
        u32::try_from(i64::from(header) % i64::from(COUNT_FACTOR))
            .expect("nonnegative Int remainder")
    }
    for header in 0..=MAX_PACKED_HEADER {
        let by_sub = subtract_count_factor(header);
        let by_mod = int_remainder(header);
        assert_eq!(by_sub, by_mod, "valid packed header {header}");
        assert!(by_mod < COUNT_FACTOR, "valid packed header residue {header}");
    }
    for header in (MAX_PACKED_HEADER + 1..=MAX_PACKED_HEADER + COUNT_FACTOR)
        .chain(4096 - COUNT_FACTOR..=4096)
        .chain(65536 - COUNT_FACTOR..=65536)
    {
        assert_eq!(
            subtract_count_factor(header),
            int_remainder(header),
            "cheap UInt32 boundary {header}"
        );
    }
    for header in (0..=COUNT_FACTOR)
        .chain([MAX_PACKED_HEADER, 1 << 31, (1 << 31) - 1])
        .chain(u32::MAX - (COUNT_FACTOR - 1)..=u32::MAX)
    {
        let residue = int_remainder(header);
        let closed = header - COUNT_FACTOR * (header / COUNT_FACTOR);
        assert_eq!(residue, closed, "closed Int remainder {header}");
        assert!(residue < COUNT_FACTOR, "UInt32 endpoint residue {header}");
        for period in 0..=256 {
            assert_eq!(
                subtract_count_factor(residue + COUNT_FACTOR * period),
                residue,
                "residue-class subtract header={header} period={period}"
            );
        }
        eprintln!(
            "P8_REMAINDER_CONTROL header={header} int_mod={residue} closed={closed} label=arithmetic-domain-not-guest-bootstrap"
        );
    }
}

fn p8_f09_parse_math_random() -> vm::FrontendIr {
    let source = "Math.random();";
    assert_eq!(source, "Math.random();");
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let context = vm::SourceFrontendContext {
        source_id: 0,
        original_path: None,
        phase: vm::compiler::ParsePhase::Standalone,
        host_api_catalog: Some(&catalog),
        imports: &[],
    };
    vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, &context)
        .expect("actual Math.random(); must parse")
}

fn p8_f09_norm_slot(slot: u16, sinks: &[u16], map: &mut Vec<u16>) -> String {
    if sinks.contains(&slot) {
        return "S".to_owned();
    }
    if let Some(index) = map.iter().position(|item| *item == slot) {
        return format!("L{index}");
    }
    let index = map.len();
    map.push(slot);
    format!("L{index}")
}

fn p8_f09_alpha(expr: &vm::Expr, sinks: &[u16], map: &mut Vec<u16>) -> String {
    match expr {
        vm::Expr::Null => "Null".to_owned(),
        vm::Expr::Int(value) => format!("Int({value})"),
        vm::Expr::Float(value) => format!("Float({value:?})"),
        vm::Expr::Bool(value) => format!("Bool({value})"),
        vm::Expr::String(value) => format!("String({value:?})"),
        vm::Expr::Bytes(value) => format!("Bytes({})", value.len()),
        vm::Expr::FunctionRef(index, type_args) => {
            format!("FunctionRef({index},types={})", type_args.len())
        }
        vm::Expr::ModuleFunctionRef(_, type_args) => {
            format!("ModuleFunctionRef(types={})", type_args.len())
        }
        vm::Expr::UnresolvedFunctionRef { name, type_args } => {
            format!("Unresolved({name},types={})", type_args.len())
        }
        vm::Expr::Var(slot) => format!("Var({})", p8_f09_norm_slot(*slot, sinks, map)),
        vm::Expr::MoveVar(slot) => format!("MoveVar({})", p8_f09_norm_slot(*slot, sinks, map)),
        vm::Expr::MoveField { root, key } => {
            format!("MoveField({}, {key})", p8_f09_norm_slot(*root, sinks, map))
        }
        vm::Expr::MoveIndex { root, index } => {
            format!(
                "MoveIndex({}, {index})",
                p8_f09_norm_slot(*root, sinks, map)
            )
        }
        vm::Expr::Call(index, type_args, args, resolution, semantic_id) => format!(
            "Call({index},types={},args=[{}],res={},sid={})",
            type_args.len(),
            args.iter()
                .map(|arg| p8_f09_alpha(arg, sinks, map))
                .collect::<Vec<_>>()
                .join(","),
            resolution.is_some(),
            semantic_id.is_some()
        ),
        vm::Expr::ModuleCall(_, type_args, args, semantic_id) => format!(
            "ModuleCall(types={},args=[{}],sid={})",
            type_args.len(),
            args.iter()
                .map(|arg| p8_f09_alpha(arg, sinks, map))
                .collect::<Vec<_>>()
                .join(","),
            semantic_id.is_some()
        ),
        vm::Expr::LocalCall(slot, type_args, args, semantic_id) => format!(
            "LocalCall({},types={},args=[{}],sid={})",
            p8_f09_norm_slot(*slot, sinks, map),
            type_args.len(),
            args.iter()
                .map(|arg| p8_f09_alpha(arg, sinks, map))
                .collect::<Vec<_>>()
                .join(","),
            semantic_id.is_some()
        ),
        vm::Expr::Closure(closure) => {
            format!("Closure(body={})", p8_f09_alpha(&closure.body, sinks, map))
        }
        vm::Expr::ClosureCall(closure, args) => format!(
            "ClosureCall({},args=[{}])",
            p8_f09_alpha(&closure.body, sinks, map),
            args.iter()
                .map(|arg| p8_f09_alpha(arg, sinks, map))
                .collect::<Vec<_>>()
                .join(",")
        ),
        vm::Expr::Add(left, right)
        | vm::Expr::Sub(left, right)
        | vm::Expr::Mul(left, right)
        | vm::Expr::Div(left, right)
        | vm::Expr::Mod(left, right)
        | vm::Expr::And(left, right)
        | vm::Expr::Or(left, right)
        | vm::Expr::Eq(left, right)
        | vm::Expr::Lt(left, right)
        | vm::Expr::Gt(left, right) => {
            let name = match expr {
                vm::Expr::Add(_, _) => "Add",
                vm::Expr::Sub(_, _) => "Sub",
                vm::Expr::Mul(_, _) => "Mul",
                vm::Expr::Div(_, _) => "Div",
                vm::Expr::Mod(_, _) => "Mod",
                vm::Expr::And(_, _) => "And",
                vm::Expr::Or(_, _) => "Or",
                vm::Expr::Eq(_, _) => "Eq",
                vm::Expr::Lt(_, _) => "Lt",
                vm::Expr::Gt(_, _) => "Gt",
                _ => "Bin",
            };
            format!(
                "{name}({},{})",
                p8_f09_alpha(left, sinks, map),
                p8_f09_alpha(right, sinks, map)
            )
        }
        vm::Expr::Neg(inner)
        | vm::Expr::Not(inner)
        | vm::Expr::ToOwned(inner)
        | vm::Expr::Borrow(inner)
        | vm::Expr::BorrowMut(inner) => {
            let name = match expr {
                vm::Expr::Neg(_) => "Neg",
                vm::Expr::Not(_) => "Not",
                vm::Expr::ToOwned(_) => "ToOwned",
                vm::Expr::Borrow(_) => "Borrow",
                vm::Expr::BorrowMut(_) => "BorrowMut",
                _ => "Unary",
            };
            format!("{name}({})", p8_f09_alpha(inner, sinks, map))
        }
        vm::Expr::OptionalGet {
            container,
            key,
            container_slot,
            key_slot,
            ..
        } => format!(
            "OptionalGet({},{},{},{})",
            p8_f09_norm_slot(*container_slot, sinks, map),
            p8_f09_norm_slot(*key_slot, sinks, map),
            p8_f09_alpha(container, sinks, map),
            p8_f09_alpha(key, sinks, map)
        ),
        vm::Expr::OptionUnwrapOr {
            value,
            value_slot,
            fallback,
            ..
        } => format!(
            "OptionUnwrapOr({},{},{})",
            p8_f09_norm_slot(*value_slot, sinks, map),
            p8_f09_alpha(value, sinks, map),
            p8_f09_alpha(fallback, sinks, map)
        ),
        vm::Expr::IfElse {
            condition,
            then_expr,
            else_expr,
        } => format!(
            "IfElse({},{},{})",
            p8_f09_alpha(condition, sinks, map),
            p8_f09_alpha(then_expr, sinks, map),
            p8_f09_alpha(else_expr, sinks, map)
        ),
        vm::Expr::Match {
            value_slot,
            result_slot,
            value,
            arms,
            default,
        } => format!(
            "Match({},{},{},arms={},default={})",
            p8_f09_norm_slot(*value_slot, sinks, map),
            p8_f09_norm_slot(*result_slot, sinks, map),
            p8_f09_alpha(value, sinks, map),
            arms.len(),
            p8_f09_alpha(default, sinks, map)
        ),
        vm::Expr::Block { stmts, expr } => {
            format!(
                "Block(stmts={},expr={})",
                stmts.len(),
                p8_f09_alpha(expr, sinks, map)
            )
        }
        vm::Expr::ResultConstruct { payload, .. }
        | vm::Expr::ResultQuery { value: payload, .. } => {
            format!("Result({})", p8_f09_alpha(payload, sinks, map))
        }
        vm::Expr::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => format!(
            "ResultMatch({},{},{})",
            p8_f09_alpha(value, sinks, map),
            p8_f09_alpha(ok_expr, sinks, map),
            p8_f09_alpha(err_expr, sinks, map)
        ),
        vm::Expr::ResultTry { value, .. } => {
            format!("ResultTry({})", p8_f09_alpha(value, sinks, map))
        }
    }
}

struct P8F09EmitWrite {
    function: String,
    ordinal: usize,
    destination: u16,
    expr: vm::Expr,
    in_while: bool,
    in_if_arm: bool,
}

fn p8_f09_float0_let(stmt: &vm::Stmt) -> Option<u16> {
    match stmt {
        vm::Stmt::Let {
            index,
            declared_schema: Some(vm::TypeSchema::Number),
            expr: vm::Expr::Float(value),
            ..
        } if value.to_bits() == 0.0f64.to_bits() => Some(*index),
        _ => None,
    }
}

fn p8_f09_note_reads(expr: &vm::Expr, reads: &mut Vec<u16>, clobbers: &mut Vec<u16>) {
    match expr {
        vm::Expr::Var(slot) | vm::Expr::MoveVar(slot) => reads.push(*slot),
        vm::Expr::MoveField { root, .. } | vm::Expr::MoveIndex { root, .. } => reads.push(*root),
        vm::Expr::LocalCall(slot, _, args, _) => {
            reads.push(*slot);
            for arg in args {
                p8_f09_note_reads(arg, reads, clobbers);
            }
        }
        vm::Expr::Call(_, _, args, _, _) | vm::Expr::ModuleCall(_, _, args, _) => {
            for arg in args {
                p8_f09_note_reads(arg, reads, clobbers);
            }
        }
        vm::Expr::OptionalGet {
            container,
            key,
            container_slot,
            key_slot,
            ..
        } => {
            reads.push(*container_slot);
            reads.push(*key_slot);
            p8_f09_note_reads(container, reads, clobbers);
            p8_f09_note_reads(key, reads, clobbers);
        }
        vm::Expr::OptionUnwrapOr {
            value,
            value_slot,
            fallback,
            ..
        } => {
            reads.push(*value_slot);
            p8_f09_note_reads(value, reads, clobbers);
            p8_f09_note_reads(fallback, reads, clobbers);
        }
        vm::Expr::Closure(closure) | vm::Expr::ClosureCall(closure, _) => {
            p8_f09_note_reads(&closure.body, reads, clobbers);
            if let vm::Expr::ClosureCall(_, args) = expr {
                for arg in args {
                    p8_f09_note_reads(arg, reads, clobbers);
                }
            }
        }
        vm::Expr::Add(left, right)
        | vm::Expr::Sub(left, right)
        | vm::Expr::Mul(left, right)
        | vm::Expr::Div(left, right)
        | vm::Expr::Mod(left, right)
        | vm::Expr::And(left, right)
        | vm::Expr::Or(left, right)
        | vm::Expr::Eq(left, right)
        | vm::Expr::Lt(left, right)
        | vm::Expr::Gt(left, right) => {
            p8_f09_note_reads(left, reads, clobbers);
            p8_f09_note_reads(right, reads, clobbers);
        }
        vm::Expr::Neg(inner)
        | vm::Expr::Not(inner)
        | vm::Expr::ToOwned(inner)
        | vm::Expr::Borrow(inner)
        | vm::Expr::BorrowMut(inner) => p8_f09_note_reads(inner, reads, clobbers),
        vm::Expr::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            p8_f09_note_reads(condition, reads, clobbers);
            p8_f09_note_reads(then_expr, reads, clobbers);
            p8_f09_note_reads(else_expr, reads, clobbers);
        }
        vm::Expr::Match {
            value_slot,
            result_slot,
            value,
            arms,
            default,
        } => {
            reads.push(*value_slot);
            clobbers.push(*result_slot);
            p8_f09_note_reads(value, reads, clobbers);
            for (pattern, arm) in arms {
                if let vm::compiler::MatchPattern::SomeBinding(slot) = pattern {
                    reads.push(*slot);
                }
                p8_f09_note_reads(arm, reads, clobbers);
            }
            p8_f09_note_reads(default, reads, clobbers);
        }
        vm::Expr::Block { stmts, expr } => {
            p8_f09_note_stmt_reads(stmts, reads, clobbers);
            p8_f09_note_reads(expr, reads, clobbers);
        }
        vm::Expr::ResultConstruct { payload, .. }
        | vm::Expr::ResultQuery { value: payload, .. } => {
            p8_f09_note_reads(payload, reads, clobbers);
        }
        vm::Expr::ResultMatch {
            value,
            value_slot,
            ok_binding,
            ok_expr,
            err_binding,
            err_expr,
            ..
        } => {
            reads.push(*value_slot);
            if let Some(slot) = ok_binding {
                reads.push(*slot);
            }
            if let Some(slot) = err_binding {
                reads.push(*slot);
            }
            p8_f09_note_reads(value, reads, clobbers);
            p8_f09_note_reads(ok_expr, reads, clobbers);
            p8_f09_note_reads(err_expr, reads, clobbers);
        }
        vm::Expr::ResultTry {
            value,
            value_slot,
            payload_slot,
            ..
        } => {
            reads.push(*value_slot);
            reads.push(*payload_slot);
            p8_f09_note_reads(value, reads, clobbers);
        }
        vm::Expr::Null
        | vm::Expr::Int(_)
        | vm::Expr::Float(_)
        | vm::Expr::Bool(_)
        | vm::Expr::String(_)
        | vm::Expr::Bytes(_)
        | vm::Expr::FunctionRef(_, _)
        | vm::Expr::ModuleFunctionRef(_, _)
        | vm::Expr::UnresolvedFunctionRef { .. } => {}
    }
}

fn p8_f09_note_stmt_reads(stmts: &[vm::Stmt], reads: &mut Vec<u16>, clobbers: &mut Vec<u16>) {
    for stmt in stmts {
        match stmt {
            vm::Stmt::Let { expr, .. }
            | vm::Stmt::Assign { expr, .. }
            | vm::Stmt::Expr { expr, .. } => {
                p8_f09_note_reads(expr, reads, clobbers);
            }
            vm::Stmt::IfElse {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                p8_f09_note_reads(condition, reads, clobbers);
                p8_f09_note_stmt_reads(then_branch, reads, clobbers);
                p8_f09_note_stmt_reads(else_branch, reads, clobbers);
            }
            vm::Stmt::For {
                init,
                condition,
                post,
                body,
                ..
            } => {
                p8_f09_note_stmt_reads(std::slice::from_ref(init.as_ref()), reads, clobbers);
                p8_f09_note_reads(condition, reads, clobbers);
                p8_f09_note_stmt_reads(std::slice::from_ref(post.as_ref()), reads, clobbers);
                p8_f09_note_stmt_reads(body, reads, clobbers);
            }
            vm::Stmt::While {
                condition, body, ..
            } => {
                p8_f09_note_reads(condition, reads, clobbers);
                p8_f09_note_stmt_reads(body, reads, clobbers);
            }
            vm::Stmt::ClosureLet { closure, .. } => {
                p8_f09_note_reads(&closure.body, reads, clobbers)
            }
            vm::Stmt::Noop { .. }
            | vm::Stmt::FuncDecl { .. }
            | vm::Stmt::Break { .. }
            | vm::Stmt::Continue { .. }
            | vm::Stmt::Drop { .. } => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn p8_f09_walk(
    stmts: &[vm::Stmt],
    write_index: u16,
    function: &str,
    in_while: bool,
    in_if_arm: bool,
    writes: &mut Vec<P8F09EmitWrite>,
    let_form: &mut usize,
    escaped: &mut usize,
    clobber_assigns: &mut Vec<u16>,
    drops: &mut Vec<u16>,
) {
    for stmt in stmts {
        match stmt {
            vm::Stmt::Assign { index, expr, .. } => {
                if let vm::Expr::Call(target, _, _, _, _) = expr {
                    if *target == write_index {
                        writes.push(P8F09EmitWrite {
                            function: function.to_owned(),
                            ordinal: writes
                                .iter()
                                .filter(|item| item.function == function)
                                .count(),
                            destination: *index,
                            expr: expr.clone(),
                            in_while,
                            in_if_arm,
                        });
                    } else {
                        clobber_assigns.push(*index);
                    }
                } else {
                    clobber_assigns.push(*index);
                }
            }
            vm::Stmt::Let { expr, .. } => {
                if matches!(expr, vm::Expr::Call(target, _, _, _, _) if *target == write_index) {
                    *let_form += 1;
                }
            }
            vm::Stmt::Expr { expr, .. } => {
                if matches!(expr, vm::Expr::Call(target, _, _, _, _) if *target == write_index) {
                    *escaped += 1;
                }
            }
            vm::Stmt::Drop { index, .. } => drops.push(*index),
            vm::Stmt::IfElse {
                then_branch,
                else_branch,
                ..
            } => {
                p8_f09_walk(
                    then_branch,
                    write_index,
                    function,
                    in_while,
                    true,
                    writes,
                    let_form,
                    escaped,
                    clobber_assigns,
                    drops,
                );
                p8_f09_walk(
                    else_branch,
                    write_index,
                    function,
                    in_while,
                    true,
                    writes,
                    let_form,
                    escaped,
                    clobber_assigns,
                    drops,
                );
            }
            vm::Stmt::While { body, .. } => p8_f09_walk(
                body,
                write_index,
                function,
                true,
                in_if_arm,
                writes,
                let_form,
                escaped,
                clobber_assigns,
                drops,
            ),
            vm::Stmt::For {
                init, post, body, ..
            } => {
                p8_f09_walk(
                    std::slice::from_ref(init.as_ref()),
                    write_index,
                    function,
                    in_while,
                    in_if_arm,
                    writes,
                    let_form,
                    escaped,
                    clobber_assigns,
                    drops,
                );
                p8_f09_walk(
                    std::slice::from_ref(post.as_ref()),
                    write_index,
                    function,
                    true,
                    in_if_arm,
                    writes,
                    let_form,
                    escaped,
                    clobber_assigns,
                    drops,
                );
                p8_f09_walk(
                    body,
                    write_index,
                    function,
                    true,
                    in_if_arm,
                    writes,
                    let_form,
                    escaped,
                    clobber_assigns,
                    drops,
                );
            }
            vm::Stmt::Noop { .. }
            | vm::Stmt::FuncDecl { .. }
            | vm::Stmt::Break { .. }
            | vm::Stmt::Continue { .. }
            | vm::Stmt::ClosureLet { .. } => {}
        }
    }
}

fn p8_f09_entry_sink(body: &[vm::Stmt], slot: u16) -> bool {
    for stmt in body {
        if matches!(
            stmt,
            vm::Stmt::IfElse { .. }
                | vm::Stmt::For { .. }
                | vm::Stmt::While { .. }
                | vm::Stmt::Break { .. }
                | vm::Stmt::Continue { .. }
        ) {
            return false;
        }
        if p8_f09_float0_let(stmt) == Some(slot) {
            return true;
        }
    }
    false
}

#[test]
fn p8_f09_discarded_number_write_sink_static_ir() {
    let ir = p8_f09_parse_math_random();
    let bootstrap_decls: Vec<_> = ir
        .functions
        .iter()
        .filter(|function| function.name == "__compat_es5_intrinsic_bootstrap")
        .collect();
    assert_eq!(
        bootstrap_decls.len(),
        1,
        "bootstrap joined by declaration name"
    );
    let bootstrap = bootstrap_decls[0];
    assert_eq!(
        ir.functions
            .iter()
            .filter(|function| function.index == bootstrap.index)
            .count(),
        1,
        "bootstrap index joins one declaration"
    );
    let root_joins = ir
        .stmts
        .iter()
        .filter(|stmt| {
            matches!(
                stmt,
                vm::Stmt::FuncDecl { name, index, has_impl: true, .. }
                    if name == "__compat_es5_intrinsic_bootstrap" && *index == bootstrap.index
            )
        })
        .count();
    assert_eq!(root_joins, 1, "bootstrap root FuncDecl");
    assert_eq!(bootstrap.arity, 1);
    assert_eq!(bootstrap.return_schema, Some(vm::TypeSchema::Null));
    assert_eq!(bootstrap.return_type, vm::ValueType::Null);
    assert_eq!(
        bootstrap.arg_schemas,
        vec![Some(vm::TypeSchema::Named(
            "__CompatRefCarrier".to_owned(),
            Vec::new()
        ))]
    );
    let bootstrap_impl = ir
        .function_impls
        .get(&bootstrap.index)
        .expect("bootstrap impl joined by declaration index");

    let write_decls: Vec<_> = ir
        .functions
        .iter()
        .filter(|function| function.name == "__compat_ref_write")
        .collect();
    assert_eq!(
        write_decls.len(),
        1,
        "carrier write joined by declaration name"
    );
    let write = write_decls[0];
    let carrier = Some(vm::TypeSchema::Named(
        "__CompatRefCarrier".to_owned(),
        Vec::new(),
    ));
    let number = Some(vm::TypeSchema::Number);
    assert_eq!(write.arity, 3, "carrier write arity");
    assert_eq!(
        write.arg_schemas,
        vec![carrier, number.clone(), number.clone()],
        "carrier write parameter schema"
    );
    assert_eq!(
        write.return_schema, number,
        "helper write result is Number, not a resource or boxed value"
    );
    assert_ne!(
        write.return_schema,
        Some(vm::TypeSchema::Named(
            "__CompatEs5Primitive".to_owned(),
            Vec::new()
        ))
    );
    let same_arity = ir
        .functions
        .iter()
        .filter(|function| function.arity == write.arity)
        .count();
    assert!(
        same_arity > 1,
        "arity {same_arity} is unique; refusing a first-arity join"
    );
    let first_arity = ir
        .functions
        .iter()
        .find(|function| function.arity == write.arity)
        .expect("arity peer");
    eprintln!(
        "P8_F09_WRITE_HELPER name={} index={} arity={} return={:?} same_arity={same_arity} first_arity_name={} first_arity_index={} bootstrap={} bootstrap_index={}",
        write.name,
        write.index,
        write.arity,
        write.return_schema,
        first_arity.name,
        first_arity.index,
        bootstrap.name,
        bootstrap.index
    );

    let mut writes = Vec::new();
    let mut let_form = 0usize;
    let mut escaped = 0usize;
    let mut float0 = Vec::new();
    let mut uses: Vec<(String, u16, &'static str)> = Vec::new();
    for declaration in &ir.functions {
        let Some(body) = ir.function_impls.get(&declaration.index) else {
            continue;
        };
        for stmt in &body.body_stmts {
            if let Some(slot) = p8_f09_float0_let(stmt) {
                float0.push((declaration.name.clone(), slot));
            }
        }
        let mut clobber_assigns = Vec::new();
        let mut drops = Vec::new();
        let mut reads = Vec::new();
        let mut clobbers = Vec::new();
        p8_f09_walk(
            &body.body_stmts,
            write.index,
            &declaration.name,
            false,
            false,
            &mut writes,
            &mut let_form,
            &mut escaped,
            &mut clobber_assigns,
            &mut drops,
        );
        if matches!(&body.body_expr, vm::Expr::Call(target, _, _, _, _) if *target == write.index) {
            escaped += 1;
        }
        p8_f09_note_stmt_reads(&body.body_stmts, &mut reads, &mut clobbers);
        p8_f09_note_reads(&body.body_expr, &mut reads, &mut clobbers);
        let mut captures = Vec::new();
        for (from, to) in &body.capture_copies {
            captures.push(*from);
            captures.push(*to);
        }
        for slot in reads {
            uses.push((declaration.name.clone(), slot, "read"));
        }
        for slot in captures {
            uses.push((declaration.name.clone(), slot, "capture"));
        }
        for slot in drops {
            uses.push((declaration.name.clone(), slot, "drop"));
        }
        for slot in clobbers {
            uses.push((declaration.name.clone(), slot, "clobber"));
        }
        for slot in clobber_assigns {
            uses.push((declaration.name.clone(), slot, "other-assign"));
        }
    }

    let emit: Vec<_> = writes
        .iter()
        .filter(|item| {
            float0
                .iter()
                .any(|(name, slot)| name == &item.function && *slot == item.destination)
        })
        .collect();
    let mut lines = vec![format!(
        "BOOTSTRAP name={} index={} arity={}",
        bootstrap.name, bootstrap.index, bootstrap.arity
    )];
    lines.push(format!(
        "WRITE_HELPER name={} index={} arity={} same_arity={same_arity} first_arity={} escaped={escaped} let_form={let_form} assign_form={} emit_form={} locals={}",
        write.name,
        write.index,
        write.arity,
        first_arity.name,
        writes.len(),
        emit.len(),
        ir.locals
    ));
    let mut functions: Vec<_> = emit.iter().map(|item| item.function.clone()).collect();
    functions.sort();
    functions.dedup();
    for name in &functions {
        let group: Vec<_> = emit.iter().filter(|item| &item.function == name).collect();
        let mut destinations: Vec<_> = group.iter().map(|item| item.destination).collect();
        destinations.sort_unstable();
        destinations.dedup();
        lines.push(format!(
            "COUNT fn={name} emit_writes={} destinations={} float0_lets={} while={} arm={}",
            group.len(),
            destinations.len(),
            float0.iter().filter(|(owner, _)| owner == name).count(),
            group.iter().filter(|item| item.in_while).count(),
            group.iter().filter(|item| item.in_if_arm).count()
        ));
        for item in group {
            let mut map = Vec::new();
            lines.push(format!(
                "ALPHA fn={} ord={} {}",
                item.function,
                item.ordinal,
                p8_f09_alpha(&item.expr, &destinations, &mut map)
            ));
        }
    }
    let witness = lines.join("\n") + "\n";
    let witness_path = std::path::PathBuf::from(std::env::var("TMPDIR").expect("TMPDIR"))
        .join("fix20-reissued/rounder-factor10/bootstrap-constant-attributes18/address21/OUT/captures")
        .join("p8-f09-write-sink-witness.txt");
    std::fs::write(&witness_path, &witness).expect("persist write-sink witness");
    eprintln!("P8_F09_WITNESS {}", witness_path.display());
    for line in lines.iter().filter(|line| {
        line.starts_with("COUNT ")
            || line.starts_with("BOOTSTRAP ")
            || line.starts_with("WRITE_HELPER ")
    }) {
        eprintln!("{line}");
    }
    let _ = bootstrap_impl;

    assert!(
        emit.len() > 1,
        "Math.random(); IR has {} Emit writes",
        emit.len()
    );
    assert!(
        emit.iter().any(|item| item.in_while),
        "need a loop-contained write"
    );
    assert!(
        emit.iter().any(|item| item.in_if_arm),
        "need an arm-contained write"
    );
    for line in lines.iter().filter(|line| line.starts_with("ALPHA ")) {
        assert!(
            !line.contains("Var(S)")
                && !line.contains("MoveVar(S)")
                && !line.contains("MoveField(S"),
            "sink escaped into a write argument: {line}"
        );
    }
    assert_eq!(escaped, 0, "write call escaped into a value");
    let bootstrap_writes = emit
        .iter()
        .filter(|item| item.function == "__compat_es5_intrinsic_bootstrap")
        .count();
    assert_eq!(
        bootstrap_writes, 141,
        "bootstrap direct carrier-write call count"
    );
    let mut shared = Vec::new();
    let mut mismatches = Vec::new();
    for name in &functions {
        let group: Vec<_> = emit.iter().filter(|item| &item.function == name).collect();
        let pure_writes: Vec<_> = group
            .iter()
            .filter(|item| {
                !uses
                    .iter()
                    .any(|(owner, slot, _)| owner == &item.function && *slot == item.destination)
            })
            .collect();
        for item in group.iter().filter(|item| {
            uses.iter()
                .any(|(owner, slot, _)| owner == &item.function && *slot == item.destination)
        }) {
            if let Some((_, _, kind)) = uses
                .iter()
                .find(|(owner, slot, _)| owner == &item.function && *slot == item.destination)
            {
                shared.push((item.function.clone(), item.destination, *kind));
            }
        }
        if pure_writes.len() < 2 {
            continue;
        }
        let mut pure_dest: Vec<_> = pure_writes.iter().map(|item| item.destination).collect();
        pure_dest.sort_unstable();
        pure_dest.dedup();
        let owner = ir
            .functions
            .iter()
            .find(|function| function.name == *name)
            .expect("writing function");
        let body = ir.function_impls.get(&owner.index).expect("writing impl");
        if pure_dest.len() != 1 || !p8_f09_entry_sink(&body.body_stmts, pure_dest[0]) {
            mismatches.push(format!(
                "{name}:discarded_writes={} sinks={} entry={}",
                pure_writes.len(),
                pure_dest.len(),
                pure_dest
                    .first()
                    .is_some_and(|slot| p8_f09_entry_sink(&body.body_stmts, *slot))
            ));
        }
    }
    shared.sort();
    shared.dedup();
    eprintln!("P8_F09_SHARED {shared:?}");
    let mut witness_note = witness;
    witness_note.push_str(&format!("SHARED {shared:?}\n"));
    std::fs::write(&witness_path, &witness_note).expect("persist shared-sink note");
    assert!(
        mismatches.is_empty(),
        "discarded Number write sinks are not reused: {mismatches:?}"
    );
}

#[test]
fn p8_f09_sibling_arm_writes_interpreter() {
    let source = "'use strict'; var o={a:0,b:0}; var which=true; var n=0; if(which){ o.a=o.a+1; n=n+1; } else { o.b=o.b+1; n=n+10; } which=false; if(which){ o.a=o.a+1; n=n+1; } else { o.b=o.b+1; n=n+10; } o.a===1 && o.b===1 && n===11";
    assert_eq!(
        field(&execute(source), "flag"),
        Value::Bool(true),
        "{source}"
    );
}

#[test]
fn p8_f09_loop_repeated_writes_interpreter() {
    let source =
        "'use strict'; var o={n:0}; var i=0; while(i!==4){ o.n=o.n+1; i=i+1; } o.n===4 && i===4";
    assert_eq!(
        field(&execute(source), "flag"),
        Value::Bool(true),
        "{source}"
    );
}

#[test]
fn p8_f09_distinct_helper_activations_interpreter() {
    let source = "'use strict'; var a={n:0,bump:function(){this.n=this.n+1;this.n=this.n+1;return this.n;}}; var b={n:5,bump:function(){this.n=this.n+1;this.n=this.n+1;return this.n;}}; a.bump()===2 && b.bump()===7";
    assert_eq!(
        field(&execute(source), "flag"),
        Value::Bool(true),
        "{source}"
    );
}

#[test]
fn p8_midpoint_evidence08_808_digit_rounding_interpreter() {
    let rows = [
        (
            "below",
            "4450147717014402025081996672794991863585242658592605113516950912287262231249312640695305412711894243178380137008083052315457825154530323827726959236845743044099361970891187471508150509418060480375117378320411851935338796416115205148741308316327252012460602310586905362063117526562176521464664318142050516404363222266800647432605601171352829157964222745548968213347287383175484034139780984693415105561952938219198147300323410536617087922315108733541318804911055533902788485678121901775450062980622457102958163711745945687733011032421168917765671370549738710820782247758425096706189168706278216333529937613807511420088624997950527910187096634639440156449072973156593524412317153981022121322120184700358076162601635686458113584868315215636869197624037042260169982910156249999999999999999999999999999999999999999e-1115",
            0x001ffffffffffffeu64,
        ),
        (
            "tie",
            "4450147717014402025081996672794991863585242658592605113516950912287262231249312640695305412711894243178380137008083052315457825154530323827726959236845743044099361970891187471508150509418060480375117378320411851935338796416115205148741308316327252012460602310586905362063117526562176521464664318142050516404363222266800647432605601171352829157964222745548968213347287383175484034139780984693415105561952938219198147300323410536617087922315108733541318804911055533902788485678121901775450062980622457102958163711745945687733011032421168917765671370549738710820782247758425096706189168706278216333529937613807511420088624997950527910187096634639440156449072973156593524412317153981022121322120184700358076162601635686458113584868315215636869197624037042260169982910156250000000000000000000000000000000000000000e-1115",
            0x001ffffffffffffeu64,
        ),
        (
            "above",
            "4450147717014402025081996672794991863585242658592605113516950912287262231249312640695305412711894243178380137008083052315457825154530323827726959236845743044099361970891187471508150509418060480375117378320411851935338796416115205148741308316327252012460602310586905362063117526562176521464664318142050516404363222266800647432605601171352829157964222745548968213347287383175484034139780984693415105561952938219198147300323410536617087922315108733541318804911055533902788485678121901775450062980622457102958163711745945687733011032421168917765671370549738710820782247758425096706189168706278216333529937613807511420088624997950527910187096634639440156449072973156593524412317153981022121322120184700358076162601635686458113584868315215636869197624037042260169982910156250000000000000000000000000000000000000001e-1115",
            0x001fffffffffffffu64,
        ),
    ];
    for (id, decimal, expected) in rows {
        let coefficient = decimal.split_once('e').expect("exponent").0;
        assert!(
            coefficient.bytes().all(|byte| byte.is_ascii_digit()),
            "{id} coefficient"
        );
        let significant = coefficient.len();
        let got = number(&format!("parseFloat('{}');", decimal));
        let actual = got.to_bits();
        let line = format!(
            "CASE id={id} expect={expected:#018x} actual_bits={actual:#018x} significant_count={significant}"
        );
        println!("{line}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        eprintln!("{line}");
        assert_eq!(actual, expected, "{id}");
    }
}

/// Host algebra control: `(2^30-1)*5^13` occupies three 30-bit limbs.
/// This does not execute the guest rounder.
#[test]
fn p8_rounder_guard_extent_arithmetic_domain_control() {
    const BASE: u128 = 1 << 30;
    let factor = 5u128.pow(13);
    let product = (BASE - 1) * factor;
    let mut limbs = [0u128; 4];
    let mut rest = product;
    for limb in &mut limbs {
        *limb = rest % BASE;
        rest /= BASE;
    }
    let line = format!(
        "P8_GUARD_EXTENT factor={factor} product={product} product_bits={product:#x} limbs={limbs:?} rest={rest}"
    );
    println!("{line}");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    eprintln!("{line}");
    assert_eq!(factor, 1_220_703_125, "5^13");
    assert!(factor > BASE - 1, "31-bit factor does not fit in one limb");
    assert_ne!(limbs[2], 0, "third limb is live; one guard limb is unsound");
    assert_eq!(limbs[3], 0, "fourth limb is zero; two guard limbs suffice");
    assert_eq!(rest, 0, "no limb beyond the third");
    assert_eq!(limbs, [926_780_523, 146_961_299, 1, 0]);
}

#[test]
fn p8_bootstrap_literal_payloads_are_materialized_once_static_ir() {
    let source = "Math.random();";
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let context = vm::SourceFrontendContext {
        source_id: 0,
        original_path: None,
        phase: vm::compiler::ParsePhase::Standalone,
        host_api_catalog: Some(&catalog),
        imports: &[],
    };
    let ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, &context)
        .expect("Math.random static probe");
    let declarations: Vec<_> = ir
        .functions
        .iter()
        .filter(|function| function.name == "__compat_es5_intrinsic_bootstrap")
        .collect();
    assert_eq!(declarations.len(), 1, "bootstrap declaration count");
    let declaration = declarations[0];
    let bootstrap = ir
        .function_impls
        .get(&declaration.index)
        .expect("bootstrap implementation joined by declaration index");
    let map_new = vm::BuiltinFunction::MapNew.call_index();
    let set = vm::BuiltinFunction::Set.call_index();
    let get = vm::BuiltinFunction::Get.call_index();
    let mut counts = PayloadCounts::default();
    let mut bindings = std::collections::BTreeMap::<u16, vm::Expr>::new();
    walk_stmts(
        &bootstrap.body_stmts,
        map_new,
        set,
        get,
        &mut counts,
        &mut bindings,
    );
    walk_expr(
        &bootstrap.body_expr,
        map_new,
        set,
        get,
        &mut counts,
        &mut bindings,
        false,
    );
    let binding_ids: Vec<u16> = bindings.keys().copied().collect();
    let mut defined = std::collections::BTreeSet::<u16>::new();
    let mut dominance = Vec::<String>::new();
    check_stmts(
        &bootstrap.body_stmts,
        &bindings,
        &mut defined,
        &mut dominance,
    );
    check_expr_uses(&bootstrap.body_expr, &bindings, &defined, &mut dominance);
    for (slot, _) in &bootstrap.capture_copies {
        if bindings.contains_key(slot) {
            dominance.push(format!("binding {slot} captured"));
        }
    }
    let mut effect = String::new();
    emit_stmts(&bootstrap.body_stmts, &bindings, &binding_ids, &mut effect);
    emit_expr_line(
        "BODY",
        &bootstrap.body_expr,
        &bindings,
        &binding_ids,
        &mut effect,
    );
    let counts_line = format!(
        "expr={} stmt={} direct_call={} map_new={} set={} get={} eligible_ctor={} direct_get_reconstruction={} bound_let={} whole_use={} effect_bytes={}",
        counts.exprs,
        counts.stmts,
        counts.direct_calls,
        counts.map_new,
        counts.set_calls,
        counts.get_calls,
        counts.eligible_ctors,
        counts.direct_get_reconstructions,
        counts.bound_lets,
        counts.whole_uses,
        effect.len()
    );
    eprintln!("P8_LITERAL_PAYLOAD_COUNTS {counts_line}");
    if let Ok(dir) = std::env::var("TMPDIR") {
        let tag = if counts.direct_get_reconstructions == 0 {
            "after"
        } else {
            "before"
        };
        let path = std::path::Path::new(&dir)
            .join("fix20-reissued/rounder-factor10/bootstrap-constant-attributes18/address21/OUT/captures")
            .join(format!("p8-literal-payload-{tag}.effect"));
        std::fs::write(&path, &effect).expect("effect evidence");
        eprintln!("P8_LITERAL_PAYLOAD_EFFECT {}", path.display());
    }
    assert!(
        counts.eligible_ctors > 0,
        "visitor saw no canonical literal payload in bootstrap: {counts_line}"
    );
    assert_eq!(
        counts.direct_get_reconstructions, 0,
        "duplicate canonical primitive constructor reconstructed by field Get: {counts_line}"
    );
    assert!(
        counts.bound_lets > 0,
        "no immutable payload binding was introduced: {counts_line}"
    );
    assert_eq!(
        counts.eligible_ctors,
        counts.bound_lets + counts.direct_get_reconstructions + counts.whole_uses,
        "eligible constructors must be bindings, field reconstructions, or whole-value uses: {counts_line}"
    );
    assert!(
        dominance.is_empty(),
        "immutable payload binding does not dominate uses: {dominance:?}"
    );
    for (slot, ctor) in &bindings {
        let uses = binding_uses(&bootstrap.body_stmts, *slot)
            + binding_uses_expr(&bootstrap.body_expr, *slot);
        assert!(
            uses >= 5,
            "binding {slot} has {uses} uses for {}",
            ser_ctor_fields(ctor)
        );
        assert!(
            canonical_literal_primitive(ctor, map_new, set),
            "binding {slot} is not a canonical literal primitive"
        );
    }
}

#[derive(Default)]
struct PayloadCounts {
    exprs: u64,
    stmts: u64,
    direct_calls: u64,
    map_new: u64,
    set_calls: u64,
    get_calls: u64,
    eligible_ctors: u64,
    direct_get_reconstructions: u64,
    bound_lets: u64,
    whole_uses: u64,
}

fn canonical_literal_primitive(payload: &vm::Expr, map_new: u16, set: u16) -> bool {
    let Some((inner, generation)) = literal_set(payload, "generation", set) else {
        return false;
    };
    let Some((inner, ref_slot)) = literal_set(inner, "ref_slot", set) else {
        return false;
    };
    let Some((inner, flag)) = literal_set(inner, "flag", set) else {
        return false;
    };
    let Some((inner, number)) = literal_set(inner, "number", set) else {
        return false;
    };
    let Some((inner, tag)) = literal_set(inner, "tag", set) else {
        return false;
    };
    builtin_args(inner, map_new, 0).is_some()
        && matches!(generation, vm::Expr::Int(0))
        && matches!(ref_slot, vm::Expr::Int(0))
        && matches!(flag, vm::Expr::Bool(_))
        && matches!(number, vm::Expr::Float(_))
        && matches!(tag, vm::Expr::Int(0..=3))
}

fn literal_set<'a>(
    expr: &'a vm::Expr,
    key: &str,
    set: u16,
) -> Option<(&'a vm::Expr, &'a vm::Expr)> {
    let args = builtin_args(expr, set, 3)?;
    match &args[1] {
        vm::Expr::String(found) if found == key => Some((&args[0], &args[2])),
        _ => None,
    }
}

fn builtin_args(expr: &vm::Expr, index: u16, argc: usize) -> Option<&[vm::Expr]> {
    match expr {
        vm::Expr::Call(got, type_args, args, None, None)
            if *got == index && type_args.is_empty() && args.len() == argc =>
        {
            Some(args)
        }
        _ => None,
    }
}

fn walk_stmts(
    stmts: &[vm::Stmt],
    map_new: u16,
    set: u16,
    get: u16,
    counts: &mut PayloadCounts,
    bindings: &mut std::collections::BTreeMap<u16, vm::Expr>,
) {
    for stmt in stmts {
        counts.stmts += 1;
        match stmt {
            vm::Stmt::Let {
                index,
                expr,
                declared_schema,
                ..
            } => {
                let eligible = canonical_literal_primitive(expr, map_new, set);
                if eligible {
                    counts.bound_lets += 1;
                    assert!(
                        matches!(
                            declared_schema,
                            Some(vm::TypeSchema::Named(name, params))
                                if name == "__CompatEs5Primitive" && params.is_empty()
                        ),
                        "payload binding {index} lacks the primitive schema"
                    );
                    bindings.insert(*index, expr.clone());
                }
                walk_expr(expr, map_new, set, get, counts, bindings, eligible);
            }
            vm::Stmt::Assign { expr, .. } | vm::Stmt::Expr { expr, .. } => {
                walk_expr(expr, map_new, set, get, counts, bindings, false);
            }
            vm::Stmt::IfElse {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                walk_expr(condition, map_new, set, get, counts, bindings, false);
                walk_stmts(then_branch, map_new, set, get, counts, bindings);
                walk_stmts(else_branch, map_new, set, get, counts, bindings);
            }
            vm::Stmt::While {
                condition, body, ..
            } => {
                walk_expr(condition, map_new, set, get, counts, bindings, false);
                walk_stmts(body, map_new, set, get, counts, bindings);
            }
            vm::Stmt::For {
                init,
                condition,
                post,
                body,
                ..
            } => {
                walk_stmts(
                    std::slice::from_ref(init),
                    map_new,
                    set,
                    get,
                    counts,
                    bindings,
                );
                walk_expr(condition, map_new, set, get, counts, bindings, false);
                walk_stmts(
                    std::slice::from_ref(post),
                    map_new,
                    set,
                    get,
                    counts,
                    bindings,
                );
                walk_stmts(body, map_new, set, get, counts, bindings);
            }
            vm::Stmt::ClosureLet { closure, .. } => {
                walk_expr(&closure.body, map_new, set, get, counts, bindings, false);
            }
            vm::Stmt::Noop { .. }
            | vm::Stmt::FuncDecl { .. }
            | vm::Stmt::Break { .. }
            | vm::Stmt::Continue { .. }
            | vm::Stmt::Drop { .. } => {}
        }
    }
}

fn walk_expr(
    expr: &vm::Expr,
    map_new: u16,
    set: u16,
    get: u16,
    counts: &mut PayloadCounts,
    bindings: &mut std::collections::BTreeMap<u16, vm::Expr>,
    inside_bound_initializer: bool,
) {
    counts.exprs += 1;
    if canonical_literal_primitive(expr, map_new, set) {
        counts.eligible_ctors += 1;
    }
    match expr {
        vm::Expr::Call(index, type_args, args, _, _) => {
            counts.direct_calls += 1;
            if *index == map_new {
                counts.map_new += 1;
            } else if *index == set {
                counts.set_calls += 1;
            } else if *index == get {
                counts.get_calls += 1;
                if args
                    .first()
                    .is_some_and(|arg| canonical_literal_primitive(arg, map_new, set))
                {
                    counts.direct_get_reconstructions += 1;
                }
            }
            for arg in type_args {
                walk_schema(arg);
            }
            let nested =
                inside_bound_initializer || canonical_literal_primitive(expr, map_new, set);
            for (ordinal, arg) in args.iter().enumerate() {
                let get_receiver =
                    *index == get && ordinal == 0 && canonical_literal_primitive(arg, map_new, set);
                if !nested && !get_receiver && canonical_literal_primitive(arg, map_new, set) {
                    counts.whole_uses += 1;
                }
                walk_expr(arg, map_new, set, get, counts, bindings, nested);
            }
        }
        vm::Expr::Add(a, b)
        | vm::Expr::Sub(a, b)
        | vm::Expr::Mul(a, b)
        | vm::Expr::Div(a, b)
        | vm::Expr::Mod(a, b)
        | vm::Expr::And(a, b)
        | vm::Expr::Or(a, b)
        | vm::Expr::Eq(a, b)
        | vm::Expr::Lt(a, b)
        | vm::Expr::Gt(a, b) => {
            walk_expr(
                a,
                map_new,
                set,
                get,
                counts,
                bindings,
                inside_bound_initializer,
            );
            walk_expr(
                b,
                map_new,
                set,
                get,
                counts,
                bindings,
                inside_bound_initializer,
            );
        }
        vm::Expr::Neg(a)
        | vm::Expr::Not(a)
        | vm::Expr::ToOwned(a)
        | vm::Expr::Borrow(a)
        | vm::Expr::BorrowMut(a) => {
            walk_expr(
                a,
                map_new,
                set,
                get,
                counts,
                bindings,
                inside_bound_initializer,
            );
        }
        vm::Expr::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            walk_expr(
                condition,
                map_new,
                set,
                get,
                counts,
                bindings,
                inside_bound_initializer,
            );
            walk_expr(
                then_expr,
                map_new,
                set,
                get,
                counts,
                bindings,
                inside_bound_initializer,
            );
            walk_expr(
                else_expr,
                map_new,
                set,
                get,
                counts,
                bindings,
                inside_bound_initializer,
            );
        }
        vm::Expr::Block { stmts, expr } => {
            walk_stmts(stmts, map_new, set, get, counts, bindings);
            walk_expr(
                expr,
                map_new,
                set,
                get,
                counts,
                bindings,
                inside_bound_initializer,
            );
        }
        vm::Expr::LocalCall(_, type_args, args, _)
        | vm::Expr::ModuleCall(_, type_args, args, _) => {
            counts.direct_calls += 1;
            for arg in type_args {
                walk_schema(arg);
            }
            for arg in args {
                walk_expr(
                    arg,
                    map_new,
                    set,
                    get,
                    counts,
                    bindings,
                    inside_bound_initializer,
                );
            }
        }
        vm::Expr::Closure(closure) | vm::Expr::ClosureCall(closure, _) => {
            walk_expr(
                &closure.body,
                map_new,
                set,
                get,
                counts,
                bindings,
                inside_bound_initializer,
            );
            if let vm::Expr::ClosureCall(_, args) = expr {
                for arg in args {
                    walk_expr(
                        arg,
                        map_new,
                        set,
                        get,
                        counts,
                        bindings,
                        inside_bound_initializer,
                    );
                }
            }
        }
        vm::Expr::OptionalGet { container, key, .. } => {
            walk_expr(
                container,
                map_new,
                set,
                get,
                counts,
                bindings,
                inside_bound_initializer,
            );
            walk_expr(
                key,
                map_new,
                set,
                get,
                counts,
                bindings,
                inside_bound_initializer,
            );
        }
        vm::Expr::OptionUnwrapOr {
            value, fallback, ..
        } => {
            walk_expr(
                value,
                map_new,
                set,
                get,
                counts,
                bindings,
                inside_bound_initializer,
            );
            walk_expr(
                fallback,
                map_new,
                set,
                get,
                counts,
                bindings,
                inside_bound_initializer,
            );
        }
        vm::Expr::Match {
            value,
            arms,
            default,
            ..
        } => {
            walk_expr(
                value,
                map_new,
                set,
                get,
                counts,
                bindings,
                inside_bound_initializer,
            );
            for (_, arm) in arms {
                walk_expr(
                    arm,
                    map_new,
                    set,
                    get,
                    counts,
                    bindings,
                    inside_bound_initializer,
                );
            }
            walk_expr(
                default,
                map_new,
                set,
                get,
                counts,
                bindings,
                inside_bound_initializer,
            );
        }
        vm::Expr::ResultConstruct { payload, .. } => {
            walk_expr(
                payload,
                map_new,
                set,
                get,
                counts,
                bindings,
                inside_bound_initializer,
            );
        }
        vm::Expr::ResultQuery { value, .. } | vm::Expr::ResultTry { value, .. } => {
            walk_expr(
                value,
                map_new,
                set,
                get,
                counts,
                bindings,
                inside_bound_initializer,
            );
        }
        vm::Expr::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            walk_expr(
                value,
                map_new,
                set,
                get,
                counts,
                bindings,
                inside_bound_initializer,
            );
            walk_expr(
                ok_expr,
                map_new,
                set,
                get,
                counts,
                bindings,
                inside_bound_initializer,
            );
            walk_expr(
                err_expr,
                map_new,
                set,
                get,
                counts,
                bindings,
                inside_bound_initializer,
            );
        }
        vm::Expr::FunctionRef(_, type_args) | vm::Expr::ModuleFunctionRef(_, type_args) => {
            for arg in type_args {
                walk_schema(arg);
            }
        }
        vm::Expr::UnresolvedFunctionRef { type_args, .. } => {
            for arg in type_args {
                walk_schema(arg);
            }
        }
        vm::Expr::Null
        | vm::Expr::Int(_)
        | vm::Expr::Float(_)
        | vm::Expr::Bool(_)
        | vm::Expr::String(_)
        | vm::Expr::Bytes(_)
        | vm::Expr::Var(_)
        | vm::Expr::MoveVar(_)
        | vm::Expr::MoveField { .. }
        | vm::Expr::MoveIndex { .. } => {}
    }
}

fn walk_schema(schema: &vm::TypeSchema) {
    match schema {
        vm::TypeSchema::Array(inner)
        | vm::TypeSchema::Optional(inner)
        | vm::TypeSchema::Map(inner) => {
            walk_schema(inner);
        }
        vm::TypeSchema::Named(_, items) | vm::TypeSchema::ArrayTuple(items) => {
            for item in items {
                walk_schema(item);
            }
        }
        vm::TypeSchema::ArrayTupleRest { prefix, rest } => {
            for item in prefix {
                walk_schema(item);
            }
            walk_schema(rest);
        }
        vm::TypeSchema::Callable { params, result } => {
            for item in params {
                walk_schema(item);
            }
            walk_schema(result);
        }
        vm::TypeSchema::Result { ok, err } => {
            walk_schema(ok);
            walk_schema(err);
        }
        vm::TypeSchema::Object(fields) => {
            for item in fields.values() {
                walk_schema(item);
            }
        }
        _ => {}
    }
}

fn check_stmts(
    stmts: &[vm::Stmt],
    bindings: &std::collections::BTreeMap<u16, vm::Expr>,
    defined: &mut std::collections::BTreeSet<u16>,
    errors: &mut Vec<String>,
) {
    for stmt in stmts {
        match stmt {
            vm::Stmt::Let { index, expr, .. } => {
                check_expr_uses(expr, bindings, defined, errors);
                if bindings.contains_key(index) {
                    defined.insert(*index);
                }
            }
            vm::Stmt::Assign { index, expr, .. } => {
                if bindings.contains_key(index) {
                    errors.push(format!("binding {index} assigned"));
                }
                check_expr_uses(expr, bindings, defined, errors);
            }
            vm::Stmt::Drop { index, .. } => {
                if bindings.contains_key(index) {
                    errors.push(format!("binding {index} dropped"));
                }
            }
            vm::Stmt::Expr { expr, .. } => check_expr_uses(expr, bindings, defined, errors),
            vm::Stmt::IfElse {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                check_expr_uses(condition, bindings, defined, errors);
                check_stmts(then_branch, bindings, defined, errors);
                check_stmts(else_branch, bindings, defined, errors);
            }
            vm::Stmt::While {
                condition, body, ..
            } => {
                check_expr_uses(condition, bindings, defined, errors);
                check_stmts(body, bindings, defined, errors);
            }
            vm::Stmt::For {
                init,
                condition,
                post,
                body,
                ..
            } => {
                check_stmts(std::slice::from_ref(init), bindings, defined, errors);
                check_expr_uses(condition, bindings, defined, errors);
                check_stmts(std::slice::from_ref(post), bindings, defined, errors);
                check_stmts(body, bindings, defined, errors);
            }
            vm::Stmt::ClosureLet { closure, .. } => {
                check_expr_uses(&closure.body, bindings, defined, errors);
            }
            _ => {}
        }
    }
}

fn check_expr_uses(
    expr: &vm::Expr,
    bindings: &std::collections::BTreeMap<u16, vm::Expr>,
    defined: &std::collections::BTreeSet<u16>,
    errors: &mut Vec<String>,
) {
    let slots = used_slots(expr);
    for slot in slots {
        if bindings.contains_key(&slot) && !defined.contains(&slot) {
            errors.push(format!("binding {slot} used before its let"));
        }
    }
}

fn used_slots(expr: &vm::Expr) -> Vec<u16> {
    let mut slots = Vec::new();
    collect_slots(expr, &mut slots);
    slots
}

fn collect_slots(expr: &vm::Expr, slots: &mut Vec<u16>) {
    match expr {
        vm::Expr::Var(slot) | vm::Expr::MoveVar(slot) => slots.push(*slot),
        vm::Expr::MoveField { root, .. } | vm::Expr::MoveIndex { root, .. } => slots.push(*root),
        vm::Expr::Borrow(inner)
        | vm::Expr::BorrowMut(inner)
        | vm::Expr::ToOwned(inner)
        | vm::Expr::Neg(inner)
        | vm::Expr::Not(inner) => collect_slots(inner, slots),
        vm::Expr::Add(a, b)
        | vm::Expr::Sub(a, b)
        | vm::Expr::Mul(a, b)
        | vm::Expr::Div(a, b)
        | vm::Expr::Mod(a, b)
        | vm::Expr::And(a, b)
        | vm::Expr::Or(a, b)
        | vm::Expr::Eq(a, b)
        | vm::Expr::Lt(a, b)
        | vm::Expr::Gt(a, b) => {
            collect_slots(a, slots);
            collect_slots(b, slots);
        }
        vm::Expr::Call(_, _, args, _, _)
        | vm::Expr::LocalCall(_, _, args, _)
        | vm::Expr::ModuleCall(_, _, args, _)
        | vm::Expr::ClosureCall(_, args) => {
            for arg in args {
                collect_slots(arg, slots);
            }
        }
        vm::Expr::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_slots(condition, slots);
            collect_slots(then_expr, slots);
            collect_slots(else_expr, slots);
        }
        vm::Expr::Block { stmts, expr } => {
            for stmt in stmts {
                collect_stmt_slots(stmt, slots);
            }
            collect_slots(expr, slots);
        }
        vm::Expr::Closure(closure) => collect_slots(&closure.body, slots),
        vm::Expr::OptionalGet { container, key, .. } => {
            collect_slots(container, slots);
            collect_slots(key, slots);
        }
        vm::Expr::OptionUnwrapOr {
            value, fallback, ..
        } => {
            collect_slots(value, slots);
            collect_slots(fallback, slots);
        }
        vm::Expr::Match {
            value,
            arms,
            default,
            ..
        } => {
            collect_slots(value, slots);
            for (_, arm) in arms {
                collect_slots(arm, slots);
            }
            collect_slots(default, slots);
        }
        vm::Expr::ResultConstruct { payload, .. } => collect_slots(payload, slots),
        vm::Expr::ResultQuery { value, .. } | vm::Expr::ResultTry { value, .. } => {
            collect_slots(value, slots);
        }
        vm::Expr::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            collect_slots(value, slots);
            collect_slots(ok_expr, slots);
            collect_slots(err_expr, slots);
        }
        _ => {}
    }
}

fn collect_stmt_slots(stmt: &vm::Stmt, slots: &mut Vec<u16>) {
    match stmt {
        vm::Stmt::Let { expr, .. }
        | vm::Stmt::Assign { expr, .. }
        | vm::Stmt::Expr { expr, .. } => {
            collect_slots(expr, slots);
        }
        vm::Stmt::IfElse {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            collect_slots(condition, slots);
            for stmt in then_branch.iter().chain(else_branch) {
                collect_stmt_slots(stmt, slots);
            }
        }
        vm::Stmt::While {
            condition, body, ..
        } => {
            collect_slots(condition, slots);
            for stmt in body {
                collect_stmt_slots(stmt, slots);
            }
        }
        _ => {}
    }
}

fn binding_uses(stmts: &[vm::Stmt], slot: u16) -> u64 {
    stmts.iter().map(|stmt| binding_uses_stmt(stmt, slot)).sum()
}

fn binding_uses_stmt(stmt: &vm::Stmt, slot: u16) -> u64 {
    match stmt {
        vm::Stmt::Let { index, expr, .. } if *index == slot => binding_uses_expr(expr, slot),
        vm::Stmt::Let { expr, .. }
        | vm::Stmt::Assign { expr, .. }
        | vm::Stmt::Expr { expr, .. } => binding_uses_expr(expr, slot),
        vm::Stmt::IfElse {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            binding_uses_expr(condition, slot)
                + binding_uses(then_branch, slot)
                + binding_uses(else_branch, slot)
        }
        vm::Stmt::While {
            condition, body, ..
        } => binding_uses_expr(condition, slot) + binding_uses(body, slot),
        vm::Stmt::For {
            init,
            condition,
            post,
            body,
            ..
        } => {
            binding_uses_stmt(init, slot)
                + binding_uses_expr(condition, slot)
                + binding_uses_stmt(post, slot)
                + binding_uses(body, slot)
        }
        vm::Stmt::ClosureLet { closure, .. } => binding_uses_expr(&closure.body, slot),
        _ => 0,
    }
}

fn binding_uses_expr(expr: &vm::Expr, slot: u16) -> u64 {
    let mut count = 0;
    count_slot(expr, slot, &mut count);
    count
}

fn count_slot(expr: &vm::Expr, slot: u16, count: &mut u64) {
    match expr {
        vm::Expr::Var(found) | vm::Expr::MoveVar(found) if *found == slot => *count += 1,
        vm::Expr::Borrow(inner)
        | vm::Expr::BorrowMut(inner)
        | vm::Expr::ToOwned(inner)
        | vm::Expr::Neg(inner)
        | vm::Expr::Not(inner) => count_slot(inner, slot, count),
        vm::Expr::Add(a, b)
        | vm::Expr::Sub(a, b)
        | vm::Expr::Mul(a, b)
        | vm::Expr::Div(a, b)
        | vm::Expr::Mod(a, b)
        | vm::Expr::And(a, b)
        | vm::Expr::Or(a, b)
        | vm::Expr::Eq(a, b)
        | vm::Expr::Lt(a, b)
        | vm::Expr::Gt(a, b) => {
            count_slot(a, slot, count);
            count_slot(b, slot, count);
        }
        vm::Expr::Call(_, _, args, _, _)
        | vm::Expr::LocalCall(_, _, args, _)
        | vm::Expr::ModuleCall(_, _, args, _)
        | vm::Expr::ClosureCall(_, args) => {
            for arg in args {
                count_slot(arg, slot, count);
            }
        }
        vm::Expr::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            count_slot(condition, slot, count);
            count_slot(then_expr, slot, count);
            count_slot(else_expr, slot, count);
        }
        _ => {}
    }
}

fn emit_stmts(
    stmts: &[vm::Stmt],
    bindings: &std::collections::BTreeMap<u16, vm::Expr>,
    ids: &[u16],
    out: &mut String,
) {
    for stmt in stmts {
        match stmt {
            vm::Stmt::Let { index, expr, .. } if bindings.contains_key(index) => {
                let _ = expr;
            }
            vm::Stmt::Let { index, expr, .. } => {
                emit_expr_line(
                    &format!("LET {}", norm_slot(*index, ids)),
                    expr,
                    bindings,
                    ids,
                    out,
                );
            }
            vm::Stmt::Assign { index, expr, .. } => {
                emit_expr_line(
                    &format!("ASSIGN {}", norm_slot(*index, ids)),
                    expr,
                    bindings,
                    ids,
                    out,
                );
            }
            vm::Stmt::Drop { index, .. } => {
                out.push_str(&format!("DROP {}\n", norm_slot(*index, ids)));
            }
            vm::Stmt::Expr { expr, .. } => emit_expr_line("EXPR", expr, bindings, ids, out),
            vm::Stmt::IfElse {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                emit_expr_line("IF", condition, bindings, ids, out);
                emit_stmts(then_branch, bindings, ids, out);
                out.push_str("ELSE\n");
                emit_stmts(else_branch, bindings, ids, out);
                out.push_str("END\n");
            }
            vm::Stmt::While {
                condition, body, ..
            } => {
                emit_expr_line("WHILE", condition, bindings, ids, out);
                emit_stmts(body, bindings, ids, out);
                out.push_str("END\n");
            }
            vm::Stmt::For {
                init,
                condition,
                post,
                body,
                ..
            } => {
                emit_stmts(std::slice::from_ref(init), bindings, ids, out);
                emit_expr_line("FOR", condition, bindings, ids, out);
                emit_stmts(std::slice::from_ref(post), bindings, ids, out);
                emit_stmts(body, bindings, ids, out);
            }
            vm::Stmt::ClosureLet { closure, .. } => {
                emit_expr_line("CLOSURE", &closure.body, bindings, ids, out);
            }
            vm::Stmt::Noop { .. } => out.push_str("NOOP\n"),
            vm::Stmt::FuncDecl { name, index, .. } => {
                out.push_str(&format!("FUNC {name} {index}\n"));
            }
            vm::Stmt::Break { .. } => out.push_str("BREAK\n"),
            vm::Stmt::Continue { .. } => out.push_str("CONTINUE\n"),
        }
    }
}

fn emit_expr_line(
    kind: &str,
    expr: &vm::Expr,
    bindings: &std::collections::BTreeMap<u16, vm::Expr>,
    ids: &[u16],
    out: &mut String,
) {
    out.push_str(kind);
    out.push(' ');
    out.push_str(&ser_expr(expr, bindings, ids));
    out.push('\n');
}

fn ser_expr(
    expr: &vm::Expr,
    bindings: &std::collections::BTreeMap<u16, vm::Expr>,
    ids: &[u16],
) -> String {
    if let Some(text) = ser_bound_get(expr, bindings, ids) {
        return text;
    }
    if canonical_literal_primitive(
        expr,
        vm::BuiltinFunction::MapNew.call_index(),
        vm::BuiltinFunction::Set.call_index(),
    ) {
        return ser_ctor_fields(expr);
    }
    match expr {
        vm::Expr::Null => "null".to_string(),
        vm::Expr::Int(n) => format!("i{n}"),
        vm::Expr::Float(n) => format!("f{:x}", n.to_bits()),
        vm::Expr::Bool(b) => format!("b{b}"),
        vm::Expr::String(s) => format!("s{}", escape_text(s)),
        vm::Expr::Bytes(b) => format!("bytes{}", b.len()),
        vm::Expr::Var(slot) => format!("v{}", norm_slot(*slot, ids)),
        vm::Expr::MoveVar(slot) => format!("mv{}", norm_slot(*slot, ids)),
        vm::Expr::MoveField { root, key } => {
            format!("mf{}:{}", norm_slot(*root, ids), escape_text(key))
        }
        vm::Expr::MoveIndex { root, index } => format!("mi{}:{index}", norm_slot(*root, ids)),
        vm::Expr::Borrow(inner) => format!("&{}", ser_expr(inner, bindings, ids)),
        vm::Expr::BorrowMut(inner) => format!("&mut{}", ser_expr(inner, bindings, ids)),
        vm::Expr::ToOwned(inner) => format!("own{}", ser_expr(inner, bindings, ids)),
        vm::Expr::Neg(inner) => format!("neg{}", ser_expr(inner, bindings, ids)),
        vm::Expr::Not(inner) => format!("not{}", ser_expr(inner, bindings, ids)),
        vm::Expr::Add(a, b) => bin("add", a, b, bindings, ids),
        vm::Expr::Sub(a, b) => bin("sub", a, b, bindings, ids),
        vm::Expr::Mul(a, b) => bin("mul", a, b, bindings, ids),
        vm::Expr::Div(a, b) => bin("div", a, b, bindings, ids),
        vm::Expr::Mod(a, b) => bin("mod", a, b, bindings, ids),
        vm::Expr::And(a, b) => bin("and", a, b, bindings, ids),
        vm::Expr::Or(a, b) => bin("or", a, b, bindings, ids),
        vm::Expr::Eq(a, b) => bin("eq", a, b, bindings, ids),
        vm::Expr::Lt(a, b) => bin("lt", a, b, bindings, ids),
        vm::Expr::Gt(a, b) => bin("gt", a, b, bindings, ids),
        vm::Expr::IfElse {
            condition,
            then_expr,
            else_expr,
        } => format!(
            "if({},{},{})",
            ser_expr(condition, bindings, ids),
            ser_expr(then_expr, bindings, ids),
            ser_expr(else_expr, bindings, ids)
        ),
        vm::Expr::Call(index, type_args, args, _, _) => format!(
            "c{index}/{}({})",
            type_args.len(),
            args.iter()
                .map(|arg| ser_expr(arg, bindings, ids))
                .collect::<Vec<_>>()
                .join(",")
        ),
        vm::Expr::LocalCall(slot, type_args, args, _) => format!(
            "lc{}/{}({})",
            norm_slot(*slot, ids),
            type_args.len(),
            args.iter()
                .map(|arg| ser_expr(arg, bindings, ids))
                .collect::<Vec<_>>()
                .join(",")
        ),
        vm::Expr::ModuleCall(_, type_args, args, _) => format!(
            "mc{}({})",
            type_args.len(),
            args.iter()
                .map(|arg| ser_expr(arg, bindings, ids))
                .collect::<Vec<_>>()
                .join(",")
        ),
        vm::Expr::FunctionRef(index, type_args) => format!("fn{index}/{}", type_args.len()),
        vm::Expr::ModuleFunctionRef(_, type_args) => format!("mfn{}", type_args.len()),
        vm::Expr::UnresolvedFunctionRef { name, type_args } => {
            format!("ufn{}:{}", escape_text(name), type_args.len())
        }
        vm::Expr::Closure(closure) => format!("closure{}", ser_expr(&closure.body, bindings, ids)),
        vm::Expr::ClosureCall(closure, args) => format!(
            "ccall{}({})",
            ser_expr(&closure.body, bindings, ids),
            args.iter()
                .map(|arg| ser_expr(arg, bindings, ids))
                .collect::<Vec<_>>()
                .join(",")
        ),
        vm::Expr::OptionalGet { container, key, .. } => format!(
            "opt({},{})",
            ser_expr(container, bindings, ids),
            ser_expr(key, bindings, ids)
        ),
        vm::Expr::OptionUnwrapOr {
            value, fallback, ..
        } => format!(
            "unwrap({},{})",
            ser_expr(value, bindings, ids),
            ser_expr(fallback, bindings, ids)
        ),
        vm::Expr::Match {
            value,
            arms,
            default,
            ..
        } => format!(
            "match({},{},{})",
            ser_expr(value, bindings, ids),
            arms.len(),
            ser_expr(default, bindings, ids)
        ),
        vm::Expr::Block { expr, .. } => format!("block{}", ser_expr(expr, bindings, ids)),
        vm::Expr::ResultConstruct { payload, .. } => {
            format!("result{}", ser_expr(payload, bindings, ids))
        }
        vm::Expr::ResultQuery { value, .. } | vm::Expr::ResultTry { value, .. } => {
            format!("resultuse{}", ser_expr(value, bindings, ids))
        }
        vm::Expr::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => format!(
            "resultmatch({},{},{})",
            ser_expr(value, bindings, ids),
            ser_expr(ok_expr, bindings, ids),
            ser_expr(err_expr, bindings, ids)
        ),
    }
}

fn ser_bound_get(
    expr: &vm::Expr,
    bindings: &std::collections::BTreeMap<u16, vm::Expr>,
    ids: &[u16],
) -> Option<String> {
    let vm::Expr::Call(index, _, args, _, _) = expr else {
        return None;
    };
    if *index != vm::BuiltinFunction::Get.call_index() || args.len() != 2 {
        return None;
    }
    let vm::Expr::String(key) = &args[1] else {
        return None;
    };
    let slot = match &args[0] {
        vm::Expr::Var(slot) => Some(*slot),
        vm::Expr::Borrow(inner) => match inner.as_ref() {
            vm::Expr::Var(slot) => Some(*slot),
            _ => None,
        },
        _ => None,
    }?;
    let ctor = bindings.get(&slot)?;
    let _ = ids;
    Some(format!("F:{}:{}", escape_text(key), ser_ctor_fields(ctor)))
}

fn ser_ctor_fields(expr: &vm::Expr) -> String {
    let set = vm::BuiltinFunction::Set.call_index();
    let (inner, generation) = literal_set(expr, "generation", set).expect("generation");
    let (inner, ref_slot) = literal_set(inner, "ref_slot", set).expect("ref_slot");
    let (inner, flag) = literal_set(inner, "flag", set).expect("flag");
    let (inner, number) = literal_set(inner, "number", set).expect("number");
    let (_, tag) = literal_set(inner, "tag", set).expect("tag");
    format!(
        "P({},{},{},{},{})",
        ser_leaf(tag),
        ser_leaf(number),
        ser_leaf(flag),
        ser_leaf(ref_slot),
        ser_leaf(generation)
    )
}

fn ser_leaf(expr: &vm::Expr) -> String {
    match expr {
        vm::Expr::Int(n) => format!("i{n}"),
        vm::Expr::Float(n) => format!("f{:x}", n.to_bits()),
        vm::Expr::Bool(b) => format!("b{b}"),
        _ => "nonliteral".to_string(),
    }
}

fn bin(
    op: &str,
    a: &vm::Expr,
    b: &vm::Expr,
    bindings: &std::collections::BTreeMap<u16, vm::Expr>,
    ids: &[u16],
) -> String {
    format!(
        "{op}({},{})",
        ser_expr(a, bindings, ids),
        ser_expr(b, bindings, ids)
    )
}

fn norm_slot(slot: u16, ids: &[u16]) -> u16 {
    slot - ids.iter().filter(|id| **id < slot).count() as u16
}

fn escape_text(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace(',', "\\,")
        .replace(')', "\\)")
}

// Independent traversal: raw slots and complete Call metadata are never alpha-renamed.
mod p8_suffix_ir {
    use vm::{Expr as E, Stmt as S};

    pub fn stmts<'a>(items: &'a [S], out: &mut Vec<&'a E>) {
        for s in items {
            match s {
                S::Let { expr, .. } | S::Assign { expr, .. } | S::Expr { expr, .. } => {
                    exprs(expr, out)
                }
                S::ClosureLet { closure, .. } => exprs(&closure.body, out),
                S::IfElse {
                    condition,
                    then_branch,
                    else_branch,
                    ..
                } => {
                    exprs(condition, out);
                    stmts(then_branch, out);
                    stmts(else_branch, out);
                }
                S::While {
                    condition, body, ..
                } => {
                    exprs(condition, out);
                    stmts(body, out);
                }
                S::For {
                    init,
                    condition,
                    post,
                    body,
                    ..
                } => {
                    stmts(std::slice::from_ref(init), out);
                    exprs(condition, out);
                    stmts(body, out);
                    stmts(std::slice::from_ref(post), out);
                }
                S::Noop { .. }
                | S::FuncDecl { .. }
                | S::Break { .. }
                | S::Continue { .. }
                | S::Drop { .. } => {}
            }
        }
    }
    pub fn exprs<'a>(e: &'a E, out: &mut Vec<&'a E>) {
        out.push(e);
        match e {
            E::Call(_, _, args, _, _)
            | E::LocalCall(_, _, args, _)
            | E::ModuleCall(_, _, args, _) => {
                for a in args {
                    exprs(a, out);
                }
            }
            E::Add(a, b)
            | E::Sub(a, b)
            | E::Mul(a, b)
            | E::Div(a, b)
            | E::Mod(a, b)
            | E::And(a, b)
            | E::Or(a, b)
            | E::Eq(a, b)
            | E::Lt(a, b)
            | E::Gt(a, b) => {
                exprs(a, out);
                exprs(b, out);
            }
            E::Neg(a) | E::Not(a) | E::ToOwned(a) | E::Borrow(a) | E::BorrowMut(a) => exprs(a, out),
            E::IfElse {
                condition,
                then_expr,
                else_expr,
            } => {
                exprs(condition, out);
                exprs(then_expr, out);
                exprs(else_expr, out);
            }
            E::Block { stmts: body, expr } => {
                stmts(body, out);
                exprs(expr, out);
            }
            E::Closure(c) => exprs(&c.body, out),
            E::ClosureCall(c, args) => {
                exprs(&c.body, out);
                for a in args {
                    exprs(a, out);
                }
            }
            E::OptionalGet { container, key, .. } => {
                exprs(container, out);
                exprs(key, out);
            }
            E::OptionUnwrapOr {
                value, fallback, ..
            } => {
                exprs(value, out);
                exprs(fallback, out);
            }
            E::Match {
                value,
                arms,
                default,
                ..
            } => {
                exprs(value, out);
                for (_, a) in arms {
                    exprs(a, out);
                }
                exprs(default, out);
            }
            E::ResultConstruct { payload, .. } => exprs(payload, out),
            E::ResultQuery { value, .. } | E::ResultTry { value, .. } => exprs(value, out),
            E::ResultMatch {
                value,
                ok_expr,
                err_expr,
                ..
            } => {
                exprs(value, out);
                exprs(ok_expr, out);
                exprs(err_expr, out);
            }
            E::Null
            | E::Int(_)
            | E::Float(_)
            | E::Bool(_)
            | E::String(_)
            | E::Bytes(_)
            | E::Var(_)
            | E::MoveVar(_)
            | E::MoveField { .. }
            | E::MoveIndex { .. }
            | E::FunctionRef(_, _)
            | E::ModuleFunctionRef(_, _)
            | E::UnresolvedFunctionRef { .. } => {}
        }
    }
    pub fn nodes(body: &vm::compiler::FunctionImpl) -> Vec<&E> {
        let mut out = Vec::new();
        stmts(&body.body_stmts, &mut out);
        exprs(&body.body_expr, &mut out);
        out
    }
    pub fn id(ir: &vm::FrontendIr, name: &str) -> u16 {
        let found: Vec<_> = ir.functions.iter().filter(|f| f.name == name).collect();
        assert_eq!(found.len(), 1, "missing or duplicate helper {name}");
        let id = found[0].index;
        assert_eq!(ir.functions.iter().filter(|f| f.index == id).count(), 1);
        assert!(ir.function_impls.contains_key(&id));
        assert_eq!(ir.stmts.iter().filter(|s| matches!(s,S::FuncDecl { index,name: n,has_impl:true,exported:false,.. } if *index==id && n==name)).count(),1);
        id
    }
    pub fn call(id: u16, args: Vec<E>) -> E {
        E::Call(id, vec![], args, None, None)
    }
    pub fn num(e: E) -> E {
        E::Div(Box::new(e), Box::new(E::Float(1.0)))
    }
    pub fn add(a: E, b: E) -> E {
        E::Add(Box::new(a), Box::new(b))
    }
    pub fn same(a: &E, b: &E) {
        assert_eq!(
            format!("{a:?}"),
            format!("{b:?}"),
            "raw expression, including Call metadata and binding identity"
        );
    }
    pub fn args(e: &E, id: u16, n: usize) -> &[E] {
        match e {
            E::Call(i, t, a, None, None) if *i == id && t.is_empty() && a.len() == n => a,
            _ => panic!("exact call {id}/{n}: {e:?}"),
        }
    }
    pub fn assign(s: &S) -> (u16, &E) {
        match s {
            S::Assign {
                kind: vm::AssignmentKind::Set,
                index,
                expr,
                ..
            } => (*index, expr),
            _ => panic!("expected Set assignment: {s:?}"),
        }
    }
    pub fn unnum(e: &E) -> &E {
        match e {
            E::Div(v, one) if matches!(one.as_ref(),E::Float(n) if n.to_bits()==1.0f64.to_bits()) => {
                v
            }
            _ => panic!("physical Float projection: {e:?}"),
        }
    }
    pub fn get<'a>(e: &'a E, key: &str) -> &'a E {
        let a = args(e, vm::BuiltinFunction::Get.call_index(), 2);
        same(&a[1], &E::String(key.into()));
        &a[0]
    }
    pub fn write(s: &S, id: u16) -> (u16, &[E]) {
        let (sink, e) = assign(s);
        (sink, args(e, id, 3))
    }
    pub fn count(nodes: &[&E], id: u16) -> usize {
        nodes
            .iter()
            .filter(|e| matches!(e,E::Call(i,_,_,_,_) if *i==id))
            .count()
    }
}

#[test]
fn p8_bootstrap_scalar_row_suffix_dispatch_static_ir() {
    use p8_suffix_ir::*;
    use vm::{Expr as E, Stmt as S, TypeSchema as T};
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let context = vm::SourceFrontendContext {
        source_id: 0,
        original_path: None,
        phase: vm::compiler::ParsePhase::Standalone,
        host_api_catalog: Some(&catalog),
        imports: &[],
    };
    let parse = |source| {
        vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, &context)
            .expect("actual plugin and effective catalog")
    };
    let ir = parse("Math.random();");
    let bootstrap_id = id(&ir, "__compat_es5_intrinsic_bootstrap");
    let bootstrap = &ir.function_impls[&bootstrap_id];
    let bn = nodes(bootstrap);
    let bc = bn.iter().filter(|e| matches!(e, E::Call(..))).count();
    let evidence = std::path::PathBuf::from(std::env::var("TMPDIR").expect("assigned TMPDIR"))
        .join("fix20-reissued/rounder-factor10/bootstrap-constant-attributes18/address21/OUT/captures");
    let stage = if ir
        .functions
        .iter()
        .any(|f| f.name == "__compat_es5_bootstrap_row_suffix")
    {
        "outlined"
    } else {
        "baseline"
    };
    // Preserve actual baseline IR before the intended missing-helper RED.
    std::fs::write(
        evidence.join(format!("fix17-{stage}-bootstrap.debug")),
        format!("{bootstrap:#?}"),
    )
    .unwrap();
    let mut schemas: Vec<_> = ir.struct_schemas.iter().collect();
    schemas.sort_by_key(|(k, _)| *k);
    std::fs::write(
        evidence.join(format!("fix17-{stage}-metadata.debug")),
        format!(
            "{:?}\n{schemas:?}\n{:?}\n{:?}",
            ir.host_api_metadata, ir.use_declarations, ir.implicit_extern_names
        ),
    )
    .unwrap();
    eprintln!(
        "P8_SUFFIX_ACTUAL stage={stage} bootstrap_expr={} bootstrap_calls={bc}",
        bn.len()
    );
    let actual_counts = (bn.len(), bc);
    let actual_ir = ir;
    let ir = p8_inverse_bootstrap_attributes(&actual_ir);
    let bootstrap = &ir.function_impls[&bootstrap_id];
    let bn = nodes(bootstrap);
    let bc = bn.iter().filter(|e| matches!(e, E::Call(..))).count();
    let suffix = id(&ir, "__compat_es5_bootstrap_row_suffix");
    let decl = ir.functions.iter().find(|f| f.index == suffix).unwrap();
    let helper = &ir.function_impls[&suffix];
    let p = &helper.param_slots;
    assert_eq!(p.len(), 9);
    assert_eq!(p.iter().collect::<std::collections::BTreeSet<_>>().len(), 9);
    let carrier = T::Named("__CompatRefCarrier".into(), vec![]);
    assert_eq!(
        decl.arg_schemas,
        vec![
            Some(carrier.clone()),
            Some(T::Int),
            Some(T::Int),
            Some(T::Int),
            Some(T::Number),
            Some(T::Number),
            Some(T::Number),
            Some(T::Number),
            Some(T::Number)
        ]
    );
    assert_eq!(decl.arity, 9);
    assert_eq!(decl.return_schema, Some(T::Int));
    assert_eq!(decl.return_type, vm::ValueType::Int);
    assert!(
        !decl.exported
            && decl.symbol.is_none()
            && decl.type_params.is_empty()
            && helper.capture_copies.is_empty()
    );
    same(&helper.body_expr, &E::Int(0));
    assert!(
        ir.functions.iter().filter(|f| f.arity == 9).count() > 1,
        "arity cannot identify this helper"
    );
    let bd = ir
        .functions
        .iter()
        .find(|f| f.index == bootstrap_id)
        .unwrap();
    assert_eq!(bd.arg_schemas, vec![Some(carrier)]);
    assert_eq!(bd.return_schema, Some(T::Null));
    assert_eq!(bd.return_type, vm::ValueType::Null);
    same(&bootstrap.body_expr, &E::Null);
    let write_id = id(&ir, "__compat_ref_write");
    let read_id = id(&ir, "__compat_ref_read");
    let decoder = id(&ir, "__compat_es5_decode_int");
    let identity = id(&ir, "__compat_es5_int_identity");
    let hn = nodes(helper);
    assert_eq!(count(&hn, write_id), 6);
    assert_eq!(count(&bn, write_id), 141);
    assert_eq!(count(&bn, suffix), 70);
    let ascii = id(&ir, "__compat_es5_bootstrap_ascii_init");
    let mut ascii_calls = 0;
    let mut ascii_units = 0;
    let mut inline_exprs = 0;
    let mut inline_calls = 0;
    let mut outlined_exprs = 0;
    for s in &bootstrap.body_stmts {
        let S::Assign {
            index,
            expr: dispatch @ E::Call(target, _, _, _, _),
            ..
        } = s
        else {
            continue;
        };
        if *target != ascii {
            continue;
        }
        let a = args(dispatch, ascii, 3);
        let E::Bytes(bytes) = &a[2] else {
            panic!("exact bootstrap ASCII literal")
        };
        assert!(bytes.is_ascii());
        assert!(matches!((&a[0], &a[1]), (E::Var(_), E::Var(_))));
        assert_eq!(bootstrap.body_stmts.iter().filter(|s| matches!(s, S::Let { index: slot, declared_schema: Some(T::Int), expr: E::Int(0), .. } if slot == index)).count(), 1);
        let mut ns = Vec::new();
        exprs(dispatch, &mut ns);
        // The outlined call also adds one Int(0) effect-sink initializer.
        outlined_exprs += ns.len() + 1;
        // Reconstruct the unchanged inline length/unit expression inventory.
        for (offset, payload) in std::iter::once((2, bytes.len() as i64)).chain(
            bytes
                .iter()
                .enumerate()
                .map(|(i, b)| (3 + i as i64, i64::from(*b))),
        ) {
            let inline = call(
                write_id,
                vec![
                    a[0].clone(),
                    num(add(call(identity, vec![a[1].clone()]), E::Int(offset))),
                    num(E::Int(payload)),
                ],
            );
            let mut ns = Vec::new();
            exprs(&inline, &mut ns);
            inline_exprs += ns.len();
            inline_calls += ns.iter().filter(|e| matches!(e, E::Call(..))).count();
        }
        ascii_calls += 1;
        ascii_units += bytes.len();
    }
    assert_eq!((ascii_calls, ascii_units), (43, 270));
    let expanded_writes =
        count(&bn, write_id) + 70 * count(&hn, write_id) + ascii_calls + ascii_units;
    assert_eq!(
        expanded_writes, 874,
        "expanded manifest, not timed runtime counter"
    );
    assert_eq!(count(&hn, read_id), 1);
    assert_eq!(count(&hn, decoder), 1);
    assert_eq!(count(&hn, identity), 1);
    assert_eq!(hn.iter().filter(|e| matches!(e, E::Mod(..))).count(), 1);
    assert_eq!(bn.iter().filter(|e| matches!(e, E::Mod(..))).count(), 0);
    // Every implementation and root expression is traversed; never follow FuncDecl into a body twice.
    for (owner, body) in &ir.function_impls {
        for e in nodes(body) {
            if let E::Call(i, _, _, _, _) = e
                && *i == suffix
            {
                assert_eq!(*owner, bootstrap_id);
                args(e, suffix, 9);
            }
            assert!(
                !matches!(e,E::FunctionRef(i,_) if *i==suffix),
                "address-taken suffix"
            );
        }
    }
    let mut root = Vec::new();
    stmts(&ir.stmts, &mut root);
    assert_eq!(count(&root, suffix), 0);
    assert!(
        !root
            .iter()
            .any(|e| matches!(e,E::FunctionRef(i,_) if *i==suffix))
    );
    assert_eq!(
        helper.body_stmts.len(),
        10,
        "two initializers, five writes, decode, remainder, publication"
    );
    let body = &helper.body_stmts;
    let flags = match &body[0] {
        S::Let {
            index,
            declared_schema: Some(T::Int),
            expr: E::Int(0),
            ..
        } => *index,
        _ => panic!("flags initializer"),
    };
    let sink = match &body[1] {
        S::Let {
            index,
            declared_schema: Some(T::Number),
            expr: E::Float(n),
            ..
        } if n.to_bits() == 0 => *index,
        _ => panic!("Number Float0 sink"),
    };
    for (ordinal, s) in body[2..7].iter().enumerate() {
        let (destination, a) = write(s, write_id);
        assert_eq!(destination, sink);
        same(&a[0], &E::Var(p[0]));
        same(&a[1], &num(add(E::Var(p[1]), E::Int(ordinal as i64 + 2))));
        same(&a[2], &num(E::Var(p[ordinal + 4])));
    }
    let (f, decode) = assign(&body[7]);
    assert_eq!(f, flags);
    same(
        decode,
        &call(
            decoder,
            vec![num(call(
                read_id,
                vec![E::Var(p[0]), num(add(E::Var(p[2]), E::Int(2)))],
            ))],
        ),
    );
    let (f, remainder) = assign(&body[8]);
    assert_eq!(f, flags);
    same(
        remainder,
        &E::Mod(Box::new(E::Var(flags)), Box::new(E::Int(4))),
    );
    let (destination, a) = write(&body[9], write_id);
    assert_eq!(destination, sink);
    same(&a[0], &E::Var(p[0]));
    same(&a[1], &num(add(E::Var(p[2]), E::Int(2))));
    same(
        &a[2],
        &num(add(E::Var(p[3]), call(identity, vec![E::Var(flags)]))),
    );
    assert!(
        !hn.iter()
            .any(|e| matches!(e,E::Var(i)|E::MoveVar(i) if *i==sink))
    );
    assert!(
        hn.iter().all(
            |e| !matches!(e,E::Call(i,_,_,_,_) if ![write_id,read_id,decoder,identity].contains(i))
        ),
        "no allocation, resize, roots, collection or callback additions"
    );
    for implementation in [bootstrap, helper] {
        let all = nodes(implementation);
        let writes: Vec<_> = implementation
            .body_stmts
            .iter()
            .filter(|s| matches!(s, S::Assign { expr:E::Call(i,_,_,_,_),.. } if *i==write_id))
            .collect();
        let sinks: std::collections::BTreeSet<_> = writes.iter().map(|s| assign(s).0).collect();
        assert_eq!(sinks.len(), 1);
        let sink = *sinks.first().unwrap();
        assert_eq!(implementation.body_stmts.iter().filter(|s|matches!(s,S::Let {index,declared_schema:Some(T::Number),expr:E::Float(n),..} if *index==sink && n.to_bits()==0)).count(),1);
        assert!(
            !all.iter()
                .any(|e| matches!(e,E::Var(i)|E::MoveVar(i) if *i==sink))
        );
        assert!(
            !implementation
                .capture_copies
                .iter()
                .any(|(a, b)| *a == sink || *b == sink)
        );
        for s in &implementation.body_stmts {
            match s {
                S::Assign { index, expr, .. } if *index == sink => {
                    args(expr, write_id, 3);
                }
                S::Drop { index, .. } => assert_ne!(*index, sink),
                _ => {}
            }
        }
    }
    // All seventy rows retain the checked base/kind prefix and their two key writes.
    let mut rows = 0;
    let mut literal_bindings = 0;
    for (at, s) in bootstrap.body_stmts.iter().enumerate() {
        let S::Assign {
            expr: E::Call(i, _, _, _, _),
            ..
        } = s
        else {
            continue;
        };
        if *i != suffix {
            continue;
        }
        let (effect, dispatch) = assign(s);
        let a = args(dispatch, suffix, 9);
        assert!(bootstrap.body_stmts.iter().any(|s|matches!(s,S::Let {index,declared_schema:Some(T::Int),expr:E::Int(0),..} if *index==effect)));
        let (row, base) = match (&a[1], &a[2], &a[3]) {
            (E::Var(r), E::Var(b), E::Int(n)) if *n > 0 && *n % 4 == 0 => (*r, *b),
            _ => panic!("Int actual ABI"),
        };
        let binding = match &bootstrap.body_stmts[at - 1] {
            S::Let {
                index,
                expr,
                declared_schema: Some(T::Named(n, t)),
                ..
            } if n == "__CompatEs5Primitive" && t.is_empty() => Some((*index, expr)),
            _ => None,
        };
        let key_at = at - if binding.is_some() { 3 } else { 2 };
        let (_, key0) = write(&bootstrap.body_stmts[key_at], write_id);
        let (_, key1) = write(&bootstrap.body_stmts[key_at + 1], write_id);
        same(&key0[0], &a[0]);
        same(&key1[0], &a[0]);
        same(&key0[1], &num(add(E::Var(row), E::Int(0))));
        same(&key1[1], &num(add(E::Var(row), E::Int(1))));
        same(
            get(unnum(&key0[2]), "ref_slot"),
            get(unnum(&key1[2]), "generation"),
        );
        let (b, checked) = assign(&bootstrap.body_stmts[key_at - 3]);
        assert_eq!(b, base);
        assert!(
            matches!(checked,E::Call(target,t,args,None,None) if ir.functions.iter().any(|f|f.index==*target && f.name=="__compat_es5_checked_base") && t.is_empty() && args.len()==3)
        );
        let (r, offset) = assign(&bootstrap.body_stmts[key_at - 2]);
        assert_eq!(r, row);
        let E::Int(count_bits) = a[3] else {
            unreachable!()
        };
        same(offset, &add(E::Var(base), E::Int((count_bits / 4 - 1) * 7)));
        let (r, kind) = assign(&bootstrap.body_stmts[key_at - 1]);
        assert_eq!(r, row);
        let kind_read = call(read_id, vec![a[0].clone(), num(E::Var(base))]);
        let kind_decode = call(decoder, vec![num(kind_read)]);
        same(
            kind,
            &add(
                E::Var(row),
                E::IfElse {
                    condition: Box::new(E::Eq(Box::new(kind_decode), Box::new(E::Int(4)))),
                    then_expr: Box::new(E::Int(10)),
                    else_expr: Box::new(E::Int(5)),
                },
            ),
        );
        // Exactly one field Get per argument; all five receivers retain the SAME raw binding/producer.
        let mut receivers = Vec::new();
        for (ordinal, key) in ["tag", "number", "flag", "ref_slot", "generation"]
            .iter()
            .enumerate()
        {
            let payload = unnum(&a[ordinal + 4]);
            let mut tree = Vec::new();
            exprs(payload, &mut tree);
            let gets:Vec<_>=tree.iter().filter(|e|matches!(e,E::Call(i,_,aa,_,_) if *i==vm::BuiltinFunction::Get.call_index() && matches!(aa.get(1),Some(E::String(k)) if k==key))).collect();
            assert_eq!(gets.len(), 1);
            receivers.push(get(gets[0], key));
            assert!(!tree.iter().any(|e|matches!(e,E::Call(i,_,_,_,_) if *i!=identity && *i!=vm::BuiltinFunction::Get.call_index() && *i!=vm::BuiltinFunction::Set.call_index() && *i!=vm::BuiltinFunction::MapNew.call_index())),"caller projections are pure immutable records, not world reads");
            if *key == "flag" {
                assert!(
                    matches!(payload,E::IfElse {then_expr,else_expr,..} if matches!(then_expr.as_ref(),E::Int(1)) && matches!(else_expr.as_ref(),E::Int(0)))
                );
            }
        }
        for receiver in &receivers[1..] {
            same(receiver, receivers[0]);
        }
        let field = |key: &str| {
            call(
                vm::BuiltinFunction::Get.call_index(),
                vec![receivers[0].clone(), E::String(key.into())],
            )
        };
        let attributes =
            if rows == 2 || (4..8).contains(&rows) || ((8..62).contains(&rows) && rows % 2 == 1) {
                (true, false, true)
            } else {
                (false, false, false)
            };
        let choose = |b: bool, n: i64| E::IfElse {
            condition: Box::new(E::Bool(b)),
            then_expr: Box::new(E::Int(n)),
            else_expr: Box::new(E::Int(0)),
        };
        let packed = add(
            E::Mul(
                Box::new(E::Int(16)),
                Box::new(call(identity, vec![field("tag")])),
            ),
            add(
                choose(attributes.0, 1),
                add(choose(attributes.1, 2), choose(attributes.2, 4)),
            ),
        );
        same(&a[4], &num(packed));
        same(&a[5], &num(field("number")));
        same(
            &a[6],
            &num(E::IfElse {
                condition: Box::new(field("flag")),
                then_expr: Box::new(E::Int(1)),
                else_expr: Box::new(E::Int(0)),
            }),
        );
        same(&a[7], &num(field("ref_slot")));
        same(&a[8], &num(field("generation")));
        if let Some((slot, ctor)) = binding {
            literal_bindings += 1;
            assert!(super_canonical(ctor));
            same(receivers[0], &E::Borrow(Box::new(E::Var(slot))));
            assert_eq!(
                bn.iter()
                    .filter(|e| matches!(e,E::Var(i) if *i==slot))
                    .count(),
                5,
                "raw binding must not alias a normalized slot"
            );
            assert!(
                !bootstrap
                    .body_stmts
                    .iter()
                    .any(|s| matches!(s,S::Assign {index,..}|S::Drop {index,..} if *index==slot))
            );
            assert!(
                !bootstrap
                    .capture_copies
                    .iter()
                    .any(|(a, b)| *a == slot || *b == slot)
            );
        }
        rows += 1;
    }
    assert_eq!(rows, 70);
    assert_eq!(literal_bindings, 11);
    let hc = hn.iter().filter(|e| matches!(e, E::Call(..))).count();
    eprintln!(
        "P8_SUFFIX_ACTUAL bootstrap_expr={} bootstrap_calls={} helper_expr={} helper_calls={hc} pair_expr={} pair_calls={} expanded_writes={}",
        actual_counts.0,
        actual_counts.1,
        hn.len(),
        actual_counts.0 + hn.len(),
        actual_counts.1 + hc,
        expanded_writes
    );
    assert_eq!((hn.len(), hc), (79, 9));
    assert_eq!(
        (
            bn.len() + hn.len() + inline_exprs - outlined_exprs,
            bc + hc + inline_calls - ascii_calls,
        ),
        (15504, 3088),
        "expanded inline AST inventory; helper ABI and ordered effects are checked separately"
    );
    std::fs::write(
        evidence.join("fix17-outlined-helper.debug"),
        format!("{helper:#?}"),
    )
    .unwrap();
    let descriptor = parse("Object.getOwnPropertyDescriptor(Math,'PI');");
    let from = id(&descriptor, "__compat_es5_from_property_descriptor");
    let ds = id(&descriptor, "__compat_es5_bootstrap_row_suffix");
    let dn = nodes(&descriptor.function_impls[&from]);
    assert_eq!(count(&dn, ds), 0);
    assert!(
        count(&dn, id(&descriptor, "__compat_ref_write")) >= 8,
        "non-bootstrap trusted_data stays inline"
    );
    assert!(
        descriptor
            .function_impls
            .iter()
            .any(|(owner, b)| *owner != from && count(&nodes(b), from) > 0),
        "actual descriptor call chain"
    );
    fn super_canonical(e: &E) -> bool {
        canonical_literal_primitive(
            e,
            vm::BuiltinFunction::MapNew.call_index(),
            vm::BuiltinFunction::Set.call_index(),
        )
    }
}

// Reissued fix20: the original source100 probe is frozen by the genuine RED.
mod p8_ascii_proof {
    use std::collections::BTreeMap;
    use vm::{Expr as E, FrontendIr, Stmt as S};

    #[derive(Clone, Debug, PartialEq)]
    pub enum Term {
        Int(i64),
        Bytes(Vec<u8>),
        Node(String, Vec<Term>),
    }
    fn node(name: &str, args: Vec<Term>) -> Term {
        Term::Node(name.into(), args)
    }
    pub struct Trace<'a> {
        pub ir: &'a FrontendIr,
        pub env: BTreeMap<u16, Term>,
        pub effects: Vec<String>,
        pub bindings: Vec<String>,
        pub writes: usize,
        next: usize,
    }
    impl<'a> Trace<'a> {
        pub fn new(ir: &'a FrontendIr, carrier: u16) -> Self {
            Self {
                ir,
                env: BTreeMap::from([(carrier, node("CARRIER", vec![]))]),
                effects: vec![],
                bindings: vec![],
                writes: 0,
                next: 0,
            }
        }
        fn name(&self, index: u16) -> String {
            self.ir
                .functions
                .iter()
                .find(|d| d.index == index)
                .map(|d| d.name.clone())
                .unwrap_or_else(|| format!("builtin:{index}"))
        }
        pub fn expr(&mut self, e: &E) -> Term {
            match e {
                E::Int(n) => Term::Int(*n),
                E::Bytes(b) => Term::Bytes(b.clone()),
                E::Float(n) => node(&format!("Float:{:016x}", n.to_bits()), vec![]),
                E::Bool(b) => node(&format!("Bool:{b}"), vec![]),
                E::String(s) => node(&format!("String:{s:?}"), vec![]),
                E::Null => node("Null", vec![]),
                E::Var(i) | E::MoveVar(i) => self
                    .env
                    .get(i)
                    .unwrap_or_else(|| panic!("undominated raw slot {i}"))
                    .clone(),
                E::Borrow(a) => self.expr(a),
                E::Add(a, b) => {
                    let a = self.expr(a);
                    let b = self.expr(b);
                    match (a, b) {
                        (Term::Int(a), Term::Int(b)) => Term::Int(a.checked_add(b).unwrap()),
                        (Term::Node(n, mut a), Term::Int(b)) if n == "AddConst" => {
                            let Some(Term::Int(x)) = a.pop() else {
                                panic!("constant tail")
                            };
                            a.push(Term::Int(x.checked_add(b).unwrap()));
                            node("AddConst", a)
                        }
                        (a, Term::Int(b)) => node("AddConst", vec![a, Term::Int(b)]),
                        (a, b) => node("Add", vec![a, b]),
                    }
                }
                E::Div(a, b) if matches!(b.as_ref(),E::Float(n) if n.to_bits()==1.0f64.to_bits()) =>
                {
                    let a = self.expr(a);
                    if matches!(&a,Term::Node(n,_) if n=="Number") {
                        a
                    } else {
                        node("Number", vec![a])
                    }
                }
                E::Call(i, t, args, m, s) => {
                    let name = self.name(*i);
                    let aa: Vec<_> = args.iter().map(|a| self.expr(a)).collect();
                    if name == "__compat_es5_int_identity" {
                        assert_eq!(aa.len(), 1);
                        return aa[0].clone();
                    }
                    if *i == vm::BuiltinFunction::Len.call_index()
                        && let [Term::Bytes(b)] = aa.as_slice()
                    {
                        return Term::Int(b.len() as i64);
                    }
                    if *i == vm::BuiltinFunction::Get.call_index()
                        && let [Term::Bytes(b), Term::Int(i)] = aa.as_slice()
                    {
                        return Term::Int(i64::from(b[usize::try_from(*i).unwrap()]));
                    }
                    node(
                        &format!("Call:{name}:types={t:?}:meta={m:?}:semantic={s:?}"),
                        aa,
                    )
                }
                E::IfElse {
                    condition,
                    then_expr,
                    else_expr,
                } => node(
                    "IfElse",
                    vec![
                        self.expr(condition),
                        self.expr(then_expr),
                        self.expr(else_expr),
                    ],
                ),
                E::Eq(a, b) => node("Eq", vec![self.expr(a), self.expr(b)]),
                E::Lt(a, b) => node("Lt", vec![self.expr(a), self.expr(b)]),
                E::Mul(a, b) => node("Mul", vec![self.expr(a), self.expr(b)]),
                E::Sub(a, b) => node("Sub", vec![self.expr(a), self.expr(b)]),
                E::Mod(a, b) => node("Mod", vec![self.expr(a), self.expr(b)]),
                E::Not(a) => node("Not", vec![self.expr(a)]),
                _ => panic!("unproved bootstrap expression: {e:?}"),
            }
        }
        fn value(&mut self, e: &E) -> Term {
            if let E::Call(i, t, args, m, s) = e {
                let name = self.name(*i);
                if name == "__compat_es5_bootstrap_row_suffix"
                    || name == "__compat_es5_bootstrap_ascii_init"
                {
                    assert!(t.is_empty() && m.is_none() && s.is_none());
                    let h = self.ir.function_impls[i].clone();
                    assert_eq!(h.param_slots.len(), args.len());
                    let aa: Vec<_> = args.iter().map(|a| self.expr(a)).collect();
                    let saved = self.env.clone();
                    for (p, a) in h.param_slots.iter().zip(aa) {
                        self.env.insert(*p, a);
                    }
                    self.stmts(&h.body_stmts);
                    let result = self.expr(&h.body_expr);
                    self.env = saved;
                    return result;
                }
                let term = self.expr(e);
                if name == "__compat_ref_write" {
                    self.writes += 1;
                    self.effects.push(format!("WRITE {} {term:?}", self.writes));
                    return node("WRITE_RESULT", vec![]);
                }
                if [
                    "__compat_es5_root_enter",
                    "__compat_es5_root_leave",
                    "__compat_es5_node_begin",
                    "__compat_es5_node_publish",
                    "__compat_es5_object_allocate",
                ]
                .contains(&name.as_str())
                {
                    self.effects.push(format!("EFFECT {} {term:?}", self.next));
                    let result = node(&format!("EFFECT_RESULT:{}", self.next), vec![]);
                    self.next += 1;
                    return result;
                }
            }
            self.expr(e)
        }
        pub fn stmts(&mut self, ss: &[S]) {
            for s in ss {
                match s {
                    S::Let { index, expr, .. } | S::Assign { index, expr, .. } => {
                        let value = self.value(expr);
                        self.bindings.push(format!("RAW {s:?} => {value:?}"));
                        self.env.insert(*index, value);
                    }
                    S::Expr { expr, .. } => {
                        let v = self.value(expr);
                        self.effects.push(format!("EXPR {v:?}"));
                    }
                    S::While {
                        condition, body, ..
                    } => {
                        let mut iterations = 0;
                        loop {
                            let E::Lt(a, b) = condition else {
                                panic!("ASCII loop condition")
                            };
                            let (Term::Int(a), Term::Int(b)) = (self.expr(a), self.expr(b)) else {
                                panic!("static literal loop")
                            };
                            if a >= b {
                                break;
                            }
                            assert!(iterations < 128, "literal bootstrap bound");
                            self.stmts(body);
                            iterations += 1;
                        }
                    }
                    _ => panic!("unproved bootstrap statement: {s:?}"),
                }
            }
        }
    }
    pub fn capture(ir: &FrontendIr, stage: &str) -> (usize, usize) {
        let baseline = std::path::Path::new(
            "/mnt/TEMP/workspace/rustscript-compat-frontends/tmp/es51-frontend-7a1db979/fix20-reissued/OUT/baseline-ordered874.effect",
        );
        let out = std::path::PathBuf::from(std::env::var_os("TMPDIR").expect("assigned TMPDIR"))
            .join("fix20-reissued/rounder-factor10/bootstrap-constant-attributes18/address21/OUT/proof");
        std::fs::write(
            out.join(format!("{stage}-full-ir.debug")),
            format!("{ir:#?}"),
        )
        .unwrap();
        let mut decls = ir.functions.clone();
        decls.sort_by_key(|d| d.index);
        let impls: BTreeMap<_, _> = ir.function_impls.iter().collect();
        let schemas: BTreeMap<_, _> = ir.struct_schemas.iter().collect();
        let sources: BTreeMap<_, _> = ir.function_sources.iter().collect();
        std::fs::write(
            out.join(format!("{stage}-all-declarations.debug")),
            format!("{decls:#?}"),
        )
        .unwrap();
        std::fs::write(
            out.join(format!("{stage}-all-implementations.debug")),
            format!("{impls:#?}"),
        )
        .unwrap();
        std::fs::write(out.join(format!("{stage}-metadata.debug")),format!("host={:#?}\nschemas={schemas:#?}\nuses={:#?}\nextern={:#?}\nsources={sources:#?}\nstmt_sources={:#?}\nlocal_bindings={:#?}\nunknown_spans={:#?}\nsemantic={:#?}\nparsed={:#?}\nvisibility={:#?}\nlexer={:#?}\nlocals={}\n",ir.host_api_metadata,ir.use_declarations,ir.implicit_extern_names,ir.stmt_sources,ir.local_bindings,ir.unknown_type_spans,ir.semantic_index,ir.parsed_semantic_index,ir.catalog_visibility,ir.lexer_tokens,ir.locals)).unwrap();
        let historical = super::p8_inverse_bootstrap_attributes(ir);
        let b = p8_id(ir, "__compat_es5_intrinsic_bootstrap");
        let h = &ir.function_impls[&b];
        let mut actual_trace = Trace::new(ir, h.param_slots[0]);
        actual_trace.stmts(&h.body_stmts);
        assert_eq!(actual_trace.writes, 874, "actual complete ordered manifest");
        std::fs::write(
            out.join(format!("{stage}-actual-ordered874.effect")),
            actual_trace.effects.join("\n") + "\n",
        )
        .unwrap();
        std::fs::write(
            out.join(format!("{stage}-all-raw-bindings.debug")),
            actual_trace.bindings.join("\n"),
        )
        .unwrap();
        let historical_body = &historical.function_impls[&b];
        let mut trace = Trace::new(&historical, historical_body.param_slots[0]);
        trace.stmts(&historical_body.body_stmts);
        assert_eq!(trace.writes, 874, "complete ordered manifest");
        let text = trace.effects.join("\n") + "\n";
        std::fs::write(
            out.join(format!("{stage}-inverse-attributes-ordered874.effect")),
            &text,
        )
        .unwrap();
        std::fs::write(
            out.join(format!("{stage}-inverse-attributes-all-raw-bindings.debug")),
            trace.bindings.join("\n"),
        )
        .unwrap();
        if stage == "candidate" {
            assert_eq!(
                text,
                std::fs::read_to_string(super::p8_retained_evidence_path(baseline))
                    .expect("genuine RED manifest"),
                "ordered values, addresses and allocation/root/publication events"
            );
        }
        let ns = super::p8_suffix_ir::nodes(h);
        let calls = ns.iter().filter(|e| matches!(e, E::Call(..))).count();
        eprintln!(
            "FIX20_ACTUAL stage={stage} expr={} calls={calls} lets={} writes={}",
            ns.len(),
            h.body_stmts
                .iter()
                .filter(|s| matches!(s, S::Let { .. }))
                .count(),
            trace.writes
        );
        (ns.len(), calls)
    }
    fn p8_id(ir: &FrontendIr, name: &str) -> u16 {
        super::p8_suffix_ir::id(ir, name)
    }
}

#[test]
fn p8_bootstrap_ascii_bytes_initializer_ordered874_static_ir() {
    use p8_suffix_ir::*;
    use vm::{Expr as E, TypeSchema as T};
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let context = vm::SourceFrontendContext {
        source_id: 0,
        original_path: None,
        phase: vm::compiler::ParsePhase::Standalone,
        host_api_catalog: Some(&catalog),
        imports: &[],
    };
    let ir =
        vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, "Math.random();", &context).unwrap();
    let found = ir
        .functions
        .iter()
        .find(|f| f.name == "__compat_es5_bootstrap_ascii_init");
    let stage = if found.is_some() {
        "candidate"
    } else {
        "baseline"
    };
    p8_ascii_proof::capture(&ir, stage);
    let decl = found.expect("missing bootstrap-only ASCII Bytes initializer");
    let helper = &ir.function_impls[&decl.index];
    assert_eq!(decl.arity, 3);
    assert_eq!(
        decl.arg_schemas,
        vec![
            Some(T::Named("__CompatRefCarrier".into(), vec![])),
            Some(T::Int),
            Some(T::Bytes)
        ]
    );
    assert_eq!(decl.return_schema, Some(T::Int));
    assert_eq!(decl.return_type, vm::ValueType::Int);
    assert!(
        !decl.exported
            && decl.symbol.is_none()
            && decl.type_params.is_empty()
            && helper.capture_copies.is_empty()
    );
    same(&helper.body_expr, &E::Int(0));
    let bootstrap = id(&ir, "__compat_es5_intrinsic_bootstrap");
    let bn = nodes(&ir.function_impls[&bootstrap]);
    let hn = nodes(helper);
    assert_eq!(count(&bn, decl.index), 43);
    assert_eq!(count(&hn, id(&ir, "__compat_ref_write")), 2);
    let mut units = 0;
    for (owner, body) in &ir.function_impls {
        for e in nodes(body) {
            if let E::Call(i, _, _, _, _) = e
                && *i == decl.index
            {
                assert_eq!(*owner, bootstrap);
                let a = args(e, decl.index, 3);
                let E::Bytes(bytes) = &a[2] else {
                    panic!("exact immutable literal")
                };
                assert!(bytes.is_ascii());
                units += bytes.len();
            }
            assert!(!matches!(e,E::FunctionRef(i,_) if *i==decl.index));
        }
    }
    assert_eq!(units, 270);
    let mut root = vec![];
    stmts(&ir.stmts, &mut root);
    assert_eq!(count(&root, decl.index), 0);
    let allowed = [
        id(&ir, "__compat_ref_write"),
        vm::BuiltinFunction::Len.call_index(),
        vm::BuiltinFunction::Get.call_index(),
        id(&ir, "__compat_es5_int_identity"),
    ];
    assert!(
        hn.iter()
            .all(|e| !matches!(e,E::Call(i,_,_,_,_) if !allowed.contains(i)))
    );
    eprintln!(
        "FIX20_ASCII strings=43 units={units} helper_expr={} helper_calls={}",
        hn.len(),
        hn.iter().filter(|e| matches!(e, E::Call(..))).count()
    );
}

/// Actual post-normalization plugin IR: five preparation joins, with a lossless
/// inverse distribution against the retained, independently captured old IR.
#[test]
fn p8_rounder_common_suffix_factor_static_ir() {
    use std::collections::{BTreeMap, BTreeSet};
    use vm::{Expr as E, Stmt as S};

    fn inventory(ss: &[S], counts: &mut [usize; 5], slots: &mut BTreeSet<u16>) {
        for s in ss {
            match s {
                S::Let { index, .. } => {
                    counts[0] += 1;
                    assert!(slots.insert(*index), "unique rounder declaration");
                }
                S::Assign { .. } => counts[1] += 1,
                S::While { body, .. } => {
                    counts[2] += 1;
                    inventory(body, counts, slots);
                }
                S::IfElse {
                    then_branch,
                    else_branch,
                    ..
                } => {
                    counts[3] += 1;
                    inventory(then_branch, counts, slots);
                    inventory(else_branch, counts, slots);
                }
                S::Expr { .. } => counts[4] += 1,
                _ => panic!("unaccounted rounder statement {s:?}"),
            }
        }
    }
    fn distribute(ss: &mut [S], snap: u16, phases: &mut Vec<i64>) {
        for s in ss {
            match s {
                S::IfElse {
                    condition,
                    then_branch,
                    else_branch,
                    ..
                } => {
                    let phase = match condition {
                        E::Eq(a, b) if matches!(a.as_ref(), E::Var(i) if *i == snap) => {
                            match b.as_ref() {
                                E::Int(n) if (51..=55).contains(n) => Some(*n),
                                _ => None,
                            }
                        }
                        _ => None,
                    };
                    if let Some(phase) = phase {
                        assert_eq!(
                            then_branch.len(),
                            2,
                            "prepare then shared suffix phase {phase}"
                        );
                        let suffix = then_branch.pop().unwrap();
                        let S::IfElse {
                            condition: E::Lt(a, b),
                            then_branch: running,
                            ..
                        } = &suffix
                        else {
                            panic!("shared compare phase {phase}")
                        };
                        assert!(matches!((a.as_ref(), b.as_ref()), (E::Var(_), E::Var(_))));
                        let idx = a.as_ref().clone();
                        assert!(
                            matches!(&running[running.len()-2], S::Assign { expr: E::Add(a,b), .. }
                            if format!("{a:?}") == format!("{idx:?}") && matches!(b.as_ref(),E::Int(2)))
                        );
                        assert!(
                            matches!(running.last().unwrap(), S::Assign { expr: E::Int(n), .. } if *n == phase)
                        );
                        let S::IfElse {
                            condition: E::Eq(a, b),
                            then_branch: prepare,
                            else_branch: empty,
                            ..
                        } = &mut then_branch[0]
                        else {
                            panic!("idx-zero prepare-only join phase {phase}")
                        };
                        assert_eq!(format!("{a:?}"), format!("{idx:?}"));
                        assert!(matches!(b.as_ref(), E::Int(0)));
                        assert!(empty.is_empty(), "no duplicate suffix in phase {phase}");
                        assert!(!prepare.is_empty());
                        prepare.push(suffix.clone());
                        empty.push(suffix);
                        phases.push(phase);
                    } else {
                        distribute(then_branch, snap, phases);
                        distribute(else_branch, snap, phases);
                    }
                }
                S::While { body, .. } => distribute(body, snap, phases),
                _ => {}
            }
        }
    }
    let ir = p8_f09_parse_math_random();
    let id = p8_suffix_ir::id(&ir, "__compat_es5_round_decimal");
    let declaration = ir.functions.iter().find(|d| d.index == id).unwrap();
    let body = &ir.function_impls[&id];
    assert_eq!(declaration.arity, 8);
    assert_eq!(declaration.return_schema, Some(vm::TypeSchema::Float));
    assert_eq!(declaration.return_type, vm::ValueType::Float);
    assert_eq!(declaration.arg_schemas.len(), 8);
    assert_eq!(
        declaration.arg_schemas[0],
        Some(vm::TypeSchema::Named("__CompatRefCarrier".into(), vec![]))
    );
    assert!(
        declaration.arg_schemas[1..]
            .iter()
            .all(|s| s == &Some(vm::TypeSchema::Int))
    );
    assert!(
        !declaration.exported && declaration.symbol.is_none() && declaration.type_params.is_empty()
    );
    assert!(body.capture_copies.is_empty());
    assert_eq!(body.param_slots.len(), 8);
    let mut counts = [0; 5];
    let mut slots = BTreeSet::new();
    inventory(&body.body_stmts, &mut counts, &mut slots);
    assert_eq!((counts[0], counts[2]), (57, 1));
    assert_eq!(slots.len(), 57);
    for p in &body.param_slots {
        assert!(slots.insert(*p));
    }
    assert_eq!(slots.len(), 65);
    let ns = p8_suffix_ir::nodes(body);
    let calls = ns.iter().filter(|e| matches!(e, E::Call(..))).count();
    let actual = (counts.iter().sum::<usize>(), ns.len(), calls);
    eprintln!(
        "ROUNDER_FACTOR10 actual={actual:?} variants={counts:?} declared_plus_params={}",
        slots.len()
    );
    let out = std::path::PathBuf::from(std::env::var_os("TMPDIR").expect("assigned TMPDIR")).join(
        "fix20-reissued/rounder-factor10/bootstrap-constant-attributes18/address21/OUT/proof",
    );
    let stage = if actual == (1067, 4250, 425) {
        "red"
    } else {
        "green"
    };
    let mut impls: BTreeMap<_, _> = ir
        .function_impls
        .iter()
        .map(|(i, h)| (*i, h.clone()))
        .collect();
    std::fs::write(
        out.join(format!("{stage}-all-implementations.debug")),
        format!("{impls:#?}"),
    )
    .unwrap();
    assert_eq!(
        actual,
        (940, 3474, 317),
        "duplicated compare suffix IR must be removed"
    );
    assert_eq!(counts, [57, 532, 1, 252, 98]);
    let historical = p8_inverse_bootstrap_attributes(&ir);
    impls = historical.function_impls.into_iter().collect();
    let expanded = impls.get_mut(&id).unwrap();
    let loop_body = expanded
        .body_stmts
        .iter()
        .find_map(|s| match s {
            S::While { body, .. } => Some(body),
            _ => None,
        })
        .unwrap();
    assert_eq!(loop_body.len(), 2);
    let S::Assign {
        index: snap,
        expr: E::Var(_),
        ..
    } = loop_body[0]
    else {
        panic!("unchanged phase snapshot")
    };
    let mut phases = vec![];
    distribute(&mut expanded.body_stmts, snap, &mut phases);
    assert_eq!(phases, [51, 52, 53, 54, 55]);
    let mut old_counts = [0; 5];
    inventory(&expanded.body_stmts, &mut old_counts, &mut BTreeSet::new());
    let old_nodes = p8_suffix_ir::nodes(expanded);
    assert_eq!(
        (
            old_counts.iter().sum::<usize>(),
            old_nodes.len(),
            old_nodes
                .iter()
                .filter(|e| matches!(e, E::Call(..)))
                .count()
        ),
        (1067, 4250, 425)
    );
    let text = format!("{impls:#?}");
    std::fs::write(out.join("reexpanded-all-implementations.debug"), &text).unwrap();
    let baseline = std::path::Path::new(
        "/mnt/TEMP/workspace/rustscript-compat-frontends/tmp/es51-frontend-7a1db979/fix20-reissued/address07/OUT/proof/candidate-all-implementations.debug",
    );
    assert_eq!(
        text,
        std::fs::read_to_string(p8_retained_evidence_path(baseline)).unwrap(),
        "full lossless old IR: every implementation, guard, destination, operand, line, parameter and capture"
    );
    eprintln!(
        "ROUNDER_FACTOR10 phases={phases:?} full_old_implementation_map_equal=true removed=(127,776,108) loops=1 lets=57 slots=65"
    );
}

// Restore only selected attribute leaves, on a fresh historical-comparison copy.
// Validation completes on the actual view before any copy mutation or tracing.
fn p8_inverse_bootstrap_attributes(actual: &vm::FrontendIr) -> vm::FrontendIr {
    use p8_suffix_ir::*;
    use vm::{Expr as E, Stmt as S, TypeSchema as T};
    let bootstrap = id(actual, "__compat_es5_intrinsic_bootstrap");
    let suffix = id(actual, "__compat_es5_bootstrap_row_suffix");
    let declaration = actual.functions.iter().find(|d| d.index == suffix).unwrap();
    assert_eq!(declaration.arity, 9);
    assert_eq!(declaration.return_schema, Some(T::Int));
    assert_eq!(declaration.return_type, vm::ValueType::Int);
    assert_eq!(
        declaration.arg_schemas,
        vec![
            Some(T::Named("__CompatRefCarrier".into(), vec![])),
            Some(T::Int),
            Some(T::Int),
            Some(T::Int),
            Some(T::Number),
            Some(T::Number),
            Some(T::Number),
            Some(T::Number),
            Some(T::Number)
        ]
    );
    let b = &actual.function_impls[&bootstrap];
    let write_id = id(actual, "__compat_ref_write");
    let identity = id(actual, "__compat_es5_int_identity");
    let checked_base = id(actual, "__compat_es5_checked_base");
    let mut validated = vec![];
    for (at, s) in b.body_stmts.iter().enumerate() {
        let S::Assign {
            expr: dispatch @ E::Call(i, _, _, _, _),
            ..
        } = s
        else {
            continue;
        };
        if *i != suffix {
            continue;
        }
        let a = args(dispatch, suffix, 9);
        let ordinal = validated.len();
        let attributes = if ordinal == 2
            || (4..8).contains(&ordinal)
            || ((8..62).contains(&ordinal) && ordinal % 2 == 1)
        {
            (true, false, true)
        } else {
            (false, false, false)
        };
        let (row, base, count_bits) = match (&a[1], &a[2], &a[3]) {
            (E::Var(r), E::Var(b), E::Int(n)) if *n > 0 && *n % 4 == 0 => (*r, *b, *n),
            _ => panic!("attribute inverse row ordinal ABI"),
        };
        let binding = matches!(&b.body_stmts[at - 1], S::Let { declared_schema: Some(T::Named(n, t)), .. } if n == "__CompatEs5Primitive" && t.is_empty());
        let key_at = at - if binding { 3 } else { 2 };
        let (_, key0) = write(&b.body_stmts[key_at], write_id);
        let (_, key1) = write(&b.body_stmts[key_at + 1], write_id);
        same(&key0[0], &a[0]);
        same(&key1[0], &a[0]);
        same(&key0[1], &num(add(E::Var(row), E::Int(0))));
        same(&key1[1], &num(add(E::Var(row), E::Int(1))));
        same(
            get(unnum(&key0[2]), "ref_slot"),
            get(unnum(&key1[2]), "generation"),
        );
        let (slot, checked) = assign(&b.body_stmts[key_at - 3]);
        assert_eq!(slot, base);
        args(checked, checked_base, 3);
        let (slot, offset) = assign(&b.body_stmts[key_at - 2]);
        assert_eq!(slot, row);
        same(offset, &add(E::Var(base), E::Int((count_bits / 4 - 1) * 7)));
        let E::Add(tag, flags) = unnum(&a[4]) else {
            panic!("attribute inverse packed Add")
        };
        let E::Mul(factor, tag) = tag.as_ref() else {
            panic!("attribute inverse tag Mul")
        };
        same(factor, &E::Int(16));
        let receiver = get(&args(tag, identity, 1)[0], "tag");
        same(receiver, get(unnum(&a[5]), "number"));
        let E::IfElse {
            condition,
            then_expr,
            else_expr,
        } = unnum(&a[6])
        else {
            panic!("unchanged dynamic payload flag")
        };
        same(then_expr, &E::Int(1));
        same(else_expr, &E::Int(0));
        same(receiver, get(condition, "flag"));
        same(receiver, get(unnum(&a[7]), "ref_slot"));
        same(receiver, get(unnum(&a[8]), "generation"));
        if binding {
            let S::Let { index, expr, .. } = &b.body_stmts[at - 1] else {
                unreachable!()
            };
            assert!(canonical_literal_primitive(
                expr,
                vm::BuiltinFunction::MapNew.call_index(),
                vm::BuiltinFunction::Set.call_index()
            ));
            same(receiver, &E::Borrow(Box::new(E::Var(*index))));
        }
        let E::Add(w, tail) = flags.as_ref() else {
            panic!("attribute inverse first Add")
        };
        let E::Add(en, c) = tail.as_ref() else {
            panic!("attribute inverse tail Add")
        };
        for (leaf, selected, weight) in [
            (w, attributes.0, 1),
            (en, attributes.1, 2),
            (c, attributes.2, 4),
        ] {
            assert!(
                matches!(leaf.as_ref(), E::Int(n) if *n == if selected { weight } else { 0 }),
                "selected literal attribute leaf at independent row {ordinal}, weight {weight}; reject mixed or double inverse"
            );
        }
        validated.push((at, attributes));
    }
    assert_eq!(validated.len(), 70);
    let mut total_calls = 0;
    for (owner, body) in &actual.function_impls {
        for e in nodes(body) {
            if matches!(e, E::Call(i, _, _, _, _) if *i == suffix) {
                assert_eq!(*owner, bootstrap);
                total_calls += 1;
            }
            assert!(!matches!(e, E::FunctionRef(i, _) if *i == suffix));
        }
    }
    assert_eq!(total_calls, 70);
    let mut root = vec![];
    stmts(&actual.stmts, &mut root);
    assert_eq!(count(&root, suffix), 0);
    assert!(
        !root
            .iter()
            .any(|e| matches!(e, E::FunctionRef(i, _) if *i == suffix))
    );
    let mut historical = actual.clone();
    let body = historical.function_impls.get_mut(&bootstrap).unwrap();
    let mut restored = 0;
    for (at, attributes) in validated {
        let S::Assign {
            expr: E::Call(_, _, a, _, _),
            ..
        } = &mut body.body_stmts[at]
        else {
            unreachable!()
        };
        let E::Div(packed, _) = &mut a[4] else {
            unreachable!()
        };
        let E::Add(_, flags) = packed.as_mut() else {
            unreachable!()
        };
        let E::Add(w, tail) = flags.as_mut() else {
            unreachable!()
        };
        let E::Add(en, c) = tail.as_mut() else {
            unreachable!()
        };
        for (leaf, selected, weight) in [
            (w, attributes.0, 1),
            (en, attributes.1, 2),
            (c, attributes.2, 4),
        ] {
            **leaf = E::IfElse {
                condition: Box::new(E::Bool(selected)),
                then_expr: Box::new(E::Int(weight)),
                else_expr: Box::new(E::Int(0)),
            };
            restored += 1;
        }
    }
    assert_eq!(restored, 210);
    eprintln!(
        "BOOTSTRAP_ATTRIBUTES_INVERSE suffix_calls={total_calls} restored_leaves={restored} before_comparison_and_trace=true"
    );
    historical
}

#[test]
fn p8_bootstrap_literal_attribute_flags_static_ir() {
    use p8_suffix_ir::*;
    use vm::{Expr as E, Stmt as S, TypeSchema as T};
    let ir = p8_f09_parse_math_random();
    let bootstrap = id(&ir, "__compat_es5_intrinsic_bootstrap");
    let suffix = id(&ir, "__compat_es5_bootstrap_row_suffix");
    let declaration = ir.functions.iter().find(|d| d.index == suffix).unwrap();
    assert_eq!(declaration.arity, 9);
    assert_eq!(declaration.return_schema, Some(T::Int));
    assert_eq!(declaration.return_type, vm::ValueType::Int);
    assert_eq!(
        declaration.arg_schemas,
        vec![
            Some(T::Named("__CompatRefCarrier".into(), vec![])),
            Some(T::Int),
            Some(T::Int),
            Some(T::Int),
            Some(T::Number),
            Some(T::Number),
            Some(T::Number),
            Some(T::Number),
            Some(T::Number)
        ]
    );
    let body = &ir.function_impls[&bootstrap];
    let ns = nodes(body);
    let calls: Vec<_> = ns
        .iter()
        .filter(|e| matches!(e, E::Call(i, _, _, _, _) if *i == suffix))
        .collect();
    assert_eq!(calls.len(), 70);
    let choices = calls.iter().map(|e| {
        let mut packed = vec![];
        exprs(&args(e, suffix, 9)[4], &mut packed);
        packed.iter().filter(|e| matches!(e, E::IfElse { condition, then_expr, else_expr } if matches!(condition.as_ref(), E::Bool(_)) && matches!(then_expr.as_ref(), E::Int(1 | 2 | 4)) && matches!(else_expr.as_ref(), E::Int(0)))).count()
    }).sum::<usize>();
    let out = std::path::PathBuf::from(std::env::var_os("TMPDIR").expect("assigned TMPDIR")).join(
        "fix20-reissued/rounder-factor10/bootstrap-constant-attributes18/address21/OUT/proof",
    );
    std::fs::write(
        out.join(format!("literal-choice-{choices}-actual-bootstrap.debug")),
        format!("{body:#?}"),
    )
    .unwrap();
    eprintln!(
        "BOOTSTRAP_ATTRIBUTES_ACTUAL suffix_calls={} literal_choices={choices} expr={} calls={} ifelse={}",
        calls.len(),
        ns.len(),
        ns.iter().filter(|e| matches!(e, E::Call(..))).count(),
        ns.iter().filter(|e| matches!(e, E::IfElse { .. })).count()
    );
    assert_eq!(
        choices, 0,
        "bootstrap packed attributes must select literal weights before IR emission"
    );
    for dispatch in &calls {
        let E::Add(tag, attributes) = unnum(&args(dispatch, suffix, 9)[4]) else {
            panic!("preserved packed Add")
        };
        assert!(matches!(tag.as_ref(), E::Mul(factor, _) if matches!(factor.as_ref(), E::Int(16))));
        let E::Add(w, tail) = attributes.as_ref() else {
            panic!("attribute Add")
        };
        let E::Add(en, c) = tail.as_ref() else {
            panic!("attribute tail Add")
        };
        assert!(matches!(w.as_ref(), E::Int(0 | 1)));
        assert!(matches!(en.as_ref(), E::Int(0 | 2)));
        assert!(matches!(c.as_ref(), E::Int(0 | 4)));
    }
    let lets: Vec<_> = body
        .body_stmts
        .iter()
        .filter_map(|s| match s {
            S::Let { index, .. } => Some(*index),
            _ => None,
        })
        .collect();
    let mut slots: std::collections::BTreeSet<_> = lets.iter().copied().collect();
    assert_eq!(slots.len(), lets.len());
    for p in &body.param_slots {
        assert!(slots.insert(*p));
    }
    assert_eq!(
        (
            body.body_stmts.len(),
            ns.len(),
            ns.iter().filter(|e| matches!(e, E::Call(..))).count(),
            lets.len(),
            slots.len()
        ),
        (1036, 11567, 2496, 405, 406)
    );
    assert_eq!(
        ns.iter().filter(|e| matches!(e, E::IfElse { .. })).count(),
        140
    );
    let historical = p8_inverse_bootstrap_attributes(&ir);
    let implementations: std::collections::BTreeMap<_, _> =
        historical.function_impls.iter().collect();
    let text = format!("{implementations:#?}");
    std::fs::write(
        out.join("inverse-attributes-all-implementations.debug"),
        &text,
    )
    .unwrap();
    let baseline = std::path::Path::new(
        "/mnt/TEMP/workspace/rustscript-compat-frontends/tmp/es51-frontend-7a1db979/fix20-reissued/rounder-factor10/OUT/proof/candidate-all-implementations.debug",
    );
    assert_eq!(
        text,
        std::fs::read_to_string(p8_retained_evidence_path(baseline)).unwrap(),
        "full attribute inverse: every implementation, guard, operand, binding, line, parameter and capture"
    );
    assert_eq!(
        nodes(&ir.function_impls[&bootstrap]).len(),
        11567,
        "actual view remains unchanged"
    );
    eprintln!("BOOTSTRAP_ATTRIBUTES_FACTOR10_FULL_MAP_EQUAL true");
}

#[test]
fn p8_selected_key_string_hint_second_attempt_interpreter() {
    let source = r#"'use strict';var a={n:0,toString:0,valueOf:function(){this.n=this.n+1;return 21;}};var b={n:0,toString:function(){this.n=1;return {};},valueOf:function(){this.n=this.n*10+2;return 22;}};parseInt(a,10)===21&&a.n===1&&parseFloat(b)===22&&b.n===12;"#;
    assert_eq!(
        field(&execute(source), "flag"),
        Value::Bool(true),
        "{source}"
    );
}

#[test]
fn p8_selected_key_string_hint_throw_interpreter() {
    let source = r#"'use strict';var a={n:0,get toString(){this.n=1;throw 7;},valueOf:function(){this.n=99;return 9;}};var b={n:0,toString:function(){this.n=1;throw 8;},valueOf:function(){this.n=99;return 9;}};var x=false;var y=false;try{parseInt(a,10);}catch(e){x=e===7&&a.n===1;}try{parseFloat(b);}catch(e){y=e===8&&b.n===1;}x&&y;"#;
    assert_eq!(
        field(&execute(source), "flag"),
        Value::Bool(true),
        "{source}"
    );
}

#[test]
fn p8_selected_key_radix_number_hint_interpreter() {
    let source = r#"'use strict';var r={n:0,valueOf:function(){this.n=1;return 10;},toString:function(){this.n=99;return '2';}};parseInt('12',r)===12&&r.n===1;"#;
    assert_eq!(
        field(&execute(source), "flag"),
        Value::Bool(true),
        "{source}"
    );
}

#[test]
fn p8_selected_key_primitive_passthrough_interpreter() {
    let source = r#"var a=parseFloat(false);var b=parseFloat(null);var c=parseFloat(undefined);parseFloat(12)===12&&parseFloat('12')===12&&a!==a&&b!==b&&c!==c;"#;
    assert_eq!(
        field(&execute(source), "flag"),
        Value::Bool(true),
        "{source}"
    );
}

#[test]
fn p8_selected_key_string_hint_missing_method_interpreter() {
    let source = r#"'use strict';var a=Object.create(null);a.n=0;a.valueOf=function(){this.n=1;return 23;};parseInt(a,10)===23&&a.n===1;"#;
    assert_eq!(
        field(&execute(source), "flag"),
        Value::Bool(true),
        "{source}"
    );
}
