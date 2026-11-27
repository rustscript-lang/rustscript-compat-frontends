//! Expression-valued callees through the existing strict-function dispatcher.
//! Number cookies use the accepted file observer. Box, string, and root
//! evidence uses the existing private arity observer. No native execution.
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use pd_vm_compat_frontends::javascript::corpus::observe_javascript_es5_corpus_file;
use pd_vm_compat_frontends::javascript_es5_flavor;
use pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog;
use vm::{BuiltinFunction, HostFunctionRegistry, OpCode, Value, VmError, VmStatus};

#[path = "javascript_es5/arity_observer.rs"]
mod observer;

const BOOLEAN: i64 = 2;
const STRING: i64 = 4;
const OBJECT_TAG: i64 = 5;
const OBJECT_COUNT_FACTOR: i64 = 8;
const OBJECT_MAX_PACKED_HEADER: i64 = 256 * OBJECT_COUNT_FACTOR + (OBJECT_COUNT_FACTOR - 1);
const ROW_ACTIVE: i64 = 2;
const ROW_FREE: i64 = 3;
const ROW_RETIRED: i64 = 4;
const STATS_SUFFIX: usize = 3;
const STATS_COLLECTIONS: usize = 2;
const CHECKED_BASE: &str = "__compat_es5_checked_base";
const ROW_STATE_SELECTOR: i64 = 13;
const ROW_GENERATION_SELECTOR: i64 = 14;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let base = PathBuf::from(std::env::var_os("TMPDIR").expect("assigned TMPDIR"));
        let path = base.join(format!(
            "p7-expression-call-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn cookie(source: &str) -> i64 {
    let scratch = Scratch::new();
    let path = scratch.0.join("source.js");
    std::fs::write(&path, source).unwrap();
    let observed = observe_javascript_es5_corpus_file(&path)
        .unwrap_or_else(|error| panic!("expression call must evaluate: {source}\n{error:?}"));
    assert_eq!(observed.aot_exec_count, 0, "{source}");
    assert_eq!(observed.jit_native_exec_count, 0, "{source}");
    assert!(observed.certification_valid_before_reset, "{source}");
    assert!(!observed.certification_valid_after_reset, "{source}");
    assert_eq!(observed.resources_after_reset, 0, "{source}");
    assert!(observed.owned_arrays_retired, "{source}");
    observed.cookie
}

fn refusal(source: &str) -> String {
    let scratch = Scratch::new();
    let path = scratch.0.join("source.js");
    std::fs::write(&path, source).unwrap();
    match observe_javascript_es5_corpus_file(&path) {
        Ok(observed) => panic!("expected a profile refusal, evaluated {observed:?} for {source}"),
        Err(error) => format!("{error:?}"),
    }
}

fn field(value: &Value, name: &str) -> Value {
    let Value::Map(map) = value else {
        panic!("boxed value: {value:?}")
    };
    map.get(&Value::string(name))
        .unwrap_or_else(|| panic!("missing {name} in {value:?}"))
        .clone()
}

fn bind_expression_machine(source: &str, probe: observer::Observer) -> vm::Vm {
    let catalog = reference_array_cli_effective_catalog();
    let leaked = Box::leak(Box::new(probe));
    let options = vm::CompileSourceFileOptions::new()
        .with_host_api_catalog(catalog.clone())
        .with_source_plugin(leaked);
    let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-arity-observe")),
        options,
    )
    .unwrap_or_else(|error| panic!("source must compile: {source}\n{error:?}"));
    let mut machine = compiled.into_vm().expect("certified VM");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("realm");
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
        .expect("errors");
    registry.allow_builtin("math::floor").expect("floor");
    registry.bind_vm_cached(&mut machine).expect("bind");
    machine.set_fuel(10_000_000);
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..vm::JitConfig::default()
    });
    machine
}

fn observe(source: &str, probe: observer::Observer) -> Vec<Value> {
    let mut machine = bind_expression_machine(source, probe);
    assert_eq!(
        machine
            .run()
            .unwrap_or_else(|error| panic!("run {source}: {error:?}")),
        VmStatus::Halted,
        "{source}"
    );
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    assert_eq!(machine.aot_exec_count(), 0, "{source}");
    assert_eq!(machine.jit_native_exec_count(), 0, "{source}");
    assert!(
        machine
            .capture_certification_stats()
            .is_some_and(|facts| facts.valid),
        "{source}"
    );
    let values = machine.stack().to_vec();
    machine.reset_for_reuse().expect("reset");
    assert_eq!(machine.host_context().resource_count(), 0, "{source}");
    let _ = javascript_es5_flavor();
    values
}

struct FrameFact {
    operand_stack_base: usize,
    prototype_id: Option<u32>,
    return_ip: Option<usize>,
    region_start: u32,
    region_end: u32,
}

struct ExpressionEvidence {
    status: Result<VmStatus, VmError>,
    stack: Vec<Value>,
    ip: usize,
    frames: Vec<FrameFact>,
    code: Vec<u8>,
    constants: Vec<Value>,
    regions: Vec<(u32, u32, Option<u32>)>,
    imports: Vec<String>,
    debug_functions: Vec<(String, Vec<String>)>,
    prototype_arity: Vec<u8>,
    line: Option<u32>,
    aot: u64,
    jit: u64,
    resources_after: usize,
    cert_after_invalid: bool,
    owners_retired: bool,
}

fn run_expression(source: &str, probe: observer::Observer) -> ExpressionEvidence {
    let mut machine = bind_expression_machine(source, probe);
    let status = machine.run();
    assert!(!machine.jit_config().enabled);
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    let stack = machine.stack().to_vec();
    let ip = machine.ip();
    let frames = machine
        .execution_frames()
        .into_iter()
        .map(|frame| {
            let region = machine.program().function_regions.iter().find(|region| {
                region.prototype_id == frame.prototype_id
                    && u32::try_from(ip)
                        .is_ok_and(|current| region.start_ip <= current && current <= region.end_ip)
            });
            FrameFact {
                operand_stack_base: frame.operand_stack_base,
                prototype_id: frame.prototype_id,
                return_ip: match frame.continuation {
                    vm::vm::VmFrameContinuation::ResumeBytecode { return_ip } => Some(return_ip),
                    _ => None,
                },
                region_start: region.map(|item| item.start_ip).unwrap_or(0),
                region_end: region.map(|item| item.end_ip).unwrap_or(0),
            }
        })
        .collect();
    let program = machine.program();
    let code = program.code.clone();
    let constants = program.constants.clone();
    let regions = program
        .function_regions
        .iter()
        .map(|region| (region.start_ip, region.end_ip, region.prototype_id))
        .collect();
    let imports = program
        .imports
        .iter()
        .map(|item| item.name.clone())
        .collect();
    let debug_functions = program
        .debug
        .as_ref()
        .map(|debug| {
            debug
                .functions
                .iter()
                .map(|function| {
                    (
                        function.name.clone(),
                        function.args.iter().map(|arg| arg.name.clone()).collect(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let prototype_arity = program
        .callable_prototypes
        .iter()
        .map(|prototype| prototype.arity)
        .collect();
    let line = program
        .debug
        .as_ref()
        .and_then(|debug| debug.line_for_offset(ip));
    let aot = machine.aot_exec_count();
    let jit = machine.jit_native_exec_count();
    let cert_before = machine
        .capture_certification_stats()
        .is_some_and(|facts| facts.valid);
    assert_eq!(aot, 0, "{source}");
    assert_eq!(jit, 0, "{source}");
    assert!(cert_before, "certification valid before reset: {source}");
    machine.reset_for_reuse().expect("reset");
    let resources_after = machine.host_context().resource_count();
    let cert_after_invalid = matches!(
        machine.capture_certification_stats(),
        Some(facts) if !facts.valid
    );
    let owners_retired =
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).is_none();
    assert_eq!(resources_after, 0, "{source}");
    assert!(
        cert_after_invalid,
        "certification invalid after reset: {source}"
    );
    assert!(owners_retired, "owned-array owners retired: {source}");
    let _ = javascript_es5_flavor();
    ExpressionEvidence {
        status,
        stack,
        ip,
        frames,
        code,
        constants,
        regions,
        imports,
        debug_functions,
        prototype_arity,
        line,
        aot,
        jit,
        resources_after,
        cert_after_invalid,
        owners_retired,
    }
}

#[test]
fn expression_iife_returns_its_actual() {
    assert_eq!(cookie("(function(x){'use strict';return x;})(42);"), 42);
}

#[test]
fn parameter_addition_stays_an_inherited_numeric_refusal() {
    // Preserve the original numeric-only refusal helper unchanged. Its retired
    // call sites are positives; the live x==1 control below has another diagnostic.
    let _legacy_numeric_refusal_contract: fn(&str, &str) = numeric_operand_refusal;
    fn relational_positive(source: &str, expected: bool) {
        let source = source.to_owned();
        std::thread::Builder::new().stack_size(33_554_432).spawn(move || {
            eprintln!("REL145_LEGACY_SOURCE source={source:?} expected={expected}");
            let mut m = pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source).unwrap();
            m.set_jit_config(vm::JitConfig { enabled: false, ..*m.jit_config() });
            assert!(!m.jit_config().enabled);
            assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
            m.set_fuel(10_000_000);
            let status = m.run();
            eprintln!("REL145_LEGACY_VM source={source:?} status={status:?} stack={:?} aot={} jit={}", m.stack(), m.aot_exec_count(), m.jit_native_exec_count());
            assert_eq!(status.unwrap(), vm::VmStatus::Halted);
            let vm::Value::Map(b) = m.stack().last().unwrap() else { panic!("Boolean box") };
            assert_eq!(b.len(), 5);
            for (key, value) in [("tag", vm::Value::Int(2)), ("flag", vm::Value::Bool(expected)), ("number", vm::Value::Float(0.0)), ("ref_slot", vm::Value::Int(0)), ("generation", vm::Value::Int(0))] {
                assert_eq!(b.get(&vm::Value::string(key)), Some(&value));
            }
            assert_eq!((m.aot_exec_count(), m.jit_native_exec_count()), (0, 0));
            assert!(m.capture_certification_stats().unwrap().valid);
            m.reset_for_reuse().unwrap();
            assert!(!m.scope_reset_pending());
            assert_eq!(m.host_context().resource_count(), 0);
            assert!(!m.capture_certification_stats().unwrap().valid);
            assert!(pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut m).is_none());
            let script = format!("const v=(0,eval)({});console.log(JSON.stringify({{kind:typeof v,value:v}}));", serde_json::to_string(&source).unwrap());
            let version = std::process::Command::new("timeout").args(["15", "node", "--version"]).output().unwrap();
            let output = std::process::Command::new("timeout").args(["15", "node", "-e", &script]).output().unwrap();
            eprintln!("REL145_LEGACY_NODE {}", serde_json::json!({"source":source,"argv":["timeout","15","node","-e",script],"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr),"version":String::from_utf8_lossy(&version.stdout),"version_rc":version.status.code()}));
            assert!(version.status.success() && output.status.success() && output.stderr.is_empty());
            assert_eq!(serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(), serde_json::json!({"kind":"boolean","value":expected}));
        }).unwrap().join().unwrap();
    }

    // Keep declaration-only lifecycle separate from append-only calls: a
    // dormant body has normal Undefined completion and proves no result kind.
    fn witness(source: &str, expected: Option<f64>) {
        let script = format!(
            "let v=(0,eval)({});let out={{kind:typeof v}};if(typeof v==='number'){{let b=Buffer.alloc(8);b.writeDoubleBE(v);out.bits=Number.isNaN(v)?'NaN':b.toString('hex');}}console.log(JSON.stringify(out));",
            serde_json::to_string(source).unwrap()
        );
        let version = std::process::Command::new("timeout")
            .args(["15", "node", "--version"])
            .output()
            .unwrap();
        let output = std::process::Command::new("timeout")
            .args(["15", "node", "-e", &script])
            .output()
            .unwrap();
        eprintln!(
            "MIG140C_NODE {}",
            serde_json::json!({"source":source,"script":script,"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr),"version":String::from_utf8_lossy(&version.stdout),"version_rc":version.status.code()})
        );
        assert!(version.status.success() && output.status.success() && output.stderr.is_empty());
        let wanted = match expected {
            Some(n) => {
                serde_json::json!({"kind":"number","bits":if n.is_nan(){"NaN".to_owned()}else{format!("{:016x}",n.to_bits())}})
            }
            None => serde_json::json!({"kind":"undefined"}),
        };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
            wanted,
            "{source}"
        );
        let mut machine =
            pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source).unwrap();
        machine.set_jit_config(vm::JitConfig {
            enabled: false,
            ..*machine.jit_config()
        });
        assert!(!machine.jit_config().enabled);
        assert_eq!(
            (machine.aot_exec_count(), machine.jit_native_exec_count()),
            (0, 0)
        );
        assert!(machine.capture_certification_stats().unwrap().valid);
        machine.set_fuel(10_000_000);
        let status = machine.run();
        eprintln!(
            "MIG140C_VM {}",
            serde_json::json!({"source":source,"status":format!("{status:?}"),"stack":format!("{:?}",machine.stack()),"aot":machine.aot_exec_count(),"jit":machine.jit_native_exec_count(),"jit_enabled":machine.jit_config().enabled})
        );
        assert_eq!(status.unwrap(), VmStatus::Halted, "{source}");
        match expected {
            Some(n) => {
                let result = machine.stack().last().unwrap();
                assert_eq!(field(result, "tag"), Value::Int(3));
                let Value::Float(actual) = field(result, "number") else {
                    panic!("Float result")
                };
                assert!(
                    if n.is_nan() {
                        actual.is_nan()
                    } else {
                        actual.to_bits() == n.to_bits()
                    },
                    "{source}: {actual:?} != {n:?}"
                );
            }
            None => assert!(
                machine.stack().is_empty(),
                "declaration-only normal empty completion evaluates to Undefined: {source}"
            ),
        }
        assert!(!machine.jit_config().enabled);
        assert_eq!(
            (machine.aot_exec_count(), machine.jit_native_exec_count()),
            (0, 0)
        );
        assert!(machine.capture_certification_stats().unwrap().valid);
        machine.reset_for_reuse().unwrap();
        assert!(!machine.scope_reset_pending());
        assert_eq!(machine.host_context().resource_count(), 0);
        assert!(!machine.capture_certification_stats().unwrap().valid);
        assert!(
            pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
                .is_none()
        );
        eprintln!(
            "MIG140C_RESET source={source:?} resources=0 pending=false cert=false owners_retired=true"
        );
    }
    // Successful compilation also proves the expression-callee shape is admitted.
    witness("(function(x){'use strict';return x+1;})(41);", Some(42.0));
    relational_positive("(function(x){'use strict';return x<1;})(41);", false);
    relational_positive("(function(x){'use strict';return x==1;})(41);", false);
    let unsupported = "(function(x){'use strict';return x in 1;})(41);";
    assert!(
        pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(unsupported).is_ok(),
        "exact x in 1 is a managed TypeError, not a source refusal"
    );
    let refused = "(function(x){'use strict';return ({}) in 1;})(41);";
    let error =
        match pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(refused) {
            Ok(_) => panic!("static object key remains excluded"),
            Err(error) => error,
        };
    let concrete = error.downcast_ref::<vm::SourcePathError>().unwrap();
    let vm::SourcePathError::SourceWithMap {
        error: vm::SourceError::Parse(error),
        sources,
    } = concrete
    else {
        panic!("mapped parse error")
    };
    assert!(
        error.message.contains("object ToPrimitive"),
        "{}",
        error.message
    );
    let span = error.span.unwrap();
    let key_token = "({})";
    let lo = refused.find(key_token).unwrap();
    assert_eq!((span.lo, span.hi), (lo, lo + key_token.len()));
    assert_eq!(&refused[lo..lo + key_token.len()], key_token);
    assert_eq!(
        &sources.source(span.source_id).unwrap()[span.lo..span.hi],
        key_token
    );
    eprintln!("REL145_OBJECT_KEY_REFUSED source={refused:?} error={error:?}");
    let source = "(function(x){'use strict';return x+1;})(41);throw 7;";
    let script = format!(
        "try{{(0,eval)({});throw Error('missing throw');}}catch(e){{console.log(JSON.stringify({{kind:typeof e,value:e}}));}}",
        serde_json::to_string(source).unwrap()
    );
    let output = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    eprintln!(
        "MIG140C_NODE {}",
        serde_json::json!({"source":source,"script":script,"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr)})
    );
    assert!(output.status.success() && output.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!({"kind":"number","value":7})
    );
    let mut machine =
        pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source).unwrap();
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..*machine.jit_config()
    });
    assert!(!machine.jit_config().enabled);
    assert_eq!(
        (machine.aot_exec_count(), machine.jit_native_exec_count()),
        (0, 0)
    );
    assert!(machine.capture_certification_stats().unwrap().valid);
    machine.set_fuel(10_000_000);
    let status = machine.run();
    eprintln!(
        "MIG140C_VM {}",
        serde_json::json!({"source":source,"status":format!("{status:?}"),"aot":machine.aot_exec_count(),"jit":machine.jit_native_exec_count(),"jit_enabled":machine.jit_config().enabled})
    );
    assert!(
        matches!(&status,Err(VmError::HostError(message)) if message=="uncaught JavaScript throw")
    );
    assert!(!machine.jit_config().enabled);
    assert_eq!(
        (machine.aot_exec_count(), machine.jit_native_exec_count()),
        (0, 0)
    );
    assert!(machine.capture_certification_stats().unwrap().valid);
    machine.reset_for_reuse().unwrap();
    assert!(!machine.scope_reset_pending());
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(!machine.capture_certification_stats().unwrap().valid);
    assert!(
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).is_none()
    );
    eprintln!(
        "MIG140C_RESET source={source:?} resources=0 pending=false cert=false owners_retired=true"
    );
}

#[test]
fn returned_strict_function_chains() {
    assert_eq!(
        cookie(
            "function maker(){'use strict';return function(x){'use strict';return x;};}maker()(42);"
        ),
        42
    );
}

#[test]
fn parenthesized_member_keeps_the_object_receiver() {
    assert_eq!(
        cookie("function m(){'use strict';return this.n;}var o={m:m,n:7};(o.m)();"),
        7
    );
}

#[test]
fn comma_member_decays_and_strict_this_is_undefined() {
    assert_eq!(
        cookie(
            "function m(){'use strict';if(this===undefined){return 1;}return 2;}var o={m:m,n:7};(0,o.m)();"
        ),
        1
    );
}

#[test]
fn conditional_selects_one_function() {
    assert_eq!(
        cookie(
            "function a(){'use strict';return 2;}function b(){'use strict';return 3;}var flag=1;(flag?a:b)();"
        ),
        2
    );
    assert_eq!(
        cookie(
            "function a(){'use strict';return 2;}function b(){'use strict';return 3;}var flag=0;(flag?a:b)();"
        ),
        3
    );
}

#[test]
fn conditional_member_decays_to_undefined_this() {
    assert_eq!(
        cookie(
            "function m(){'use strict';if(this===undefined){return 1;}return 2;}var o={m:m};var flag=1;(flag?o.m:o.m)();"
        ),
        1
    );
}

#[test]
fn assignment_result_is_called_with_undefined_this() {
    assert_eq!(
        cookie(
            "function m(){'use strict';if(this===undefined){return 4;}return 5;}var o={m:m};var f;(f=o.m)();"
        ),
        4
    );
}

#[test]
fn callee_runs_once_before_left_to_right_actuals() {
    assert_eq!(
        cookie(
            "var n=1;(function(k){'use strict';return function(x){'use strict';return x;};})(n=n+1)(n);"
        ),
        2,
        "the callee's actual increments once, then the outer actual reads that value"
    );
    assert_eq!(
        cookie("var n=1;(function(a,b){'use strict';return a;})(n=n+1,n=n+10);"),
        2
    );
}

#[test]
fn extra_actual_still_runs() {
    assert_eq!(
        cookie("var n=1;(function(a){'use strict';return a;})(7,n=n+1);n;"),
        2
    );
}

#[test]
fn saved_callee_ignores_a_later_replacement() {
    assert_eq!(
        cookie(
            "var f=function(){'use strict';return 7;};var pick=function(h){'use strict';return h;};pick(f)(f=function(){'use strict';return 9;});"
        ),
        7
    );
    assert_eq!(
        cookie(
            "var o={m:function(){'use strict';return 7;}};var pick=function(h){'use strict';return h;};pick(o.m)(o.m=function(){'use strict';return 9;});"
        ),
        7
    );
}

#[test]
fn noncallable_object_throws_after_the_actual() {
    assert_eq!(
        cookie(
            "var n=1;var hit=1;try{({})(n=n+1);}catch(e){if(e.name===\"TypeError\"){hit=3;}}hit+n;"
        ),
        5,
        "actual effect (n=2) plus TypeError name (hit=3)"
    );
}

#[test]
fn callee_throw_skips_later_actuals_and_finally_still_runs() {
    assert_eq!(
        cookie("function boom(){'use strict';throw 7;}var n=2;try{boom()(n=1);}catch(e){}n;"),
        2
    );
    assert_eq!(
        cookie("var n=1;try{try{(function(){'use strict';throw 7;})();}finally{n=4;}}catch(e){}n;"),
        4
    );
}

#[test]
fn primitive_callee_stays_a_static_refusal() {
    for source in ["(1)();", "function f(){'use strict';return 1;(1)();}f();"] {
        let text = refusal(source);
        assert!(
            text.contains("P0 bare calls require a function binding"),
            "{source} -> {text}"
        );
    }
}

#[test]
fn boolean_and_string_results_keep_their_real_boxes() {
    let values = observe(
        "var b=(function(){'use strict';return true;})();var s=(function(){'use strict';return \"ab\";})();",
        observer::Observer {
            reads: vec!["b".into(), "s".into()],
            string: Some(("s".into(), 2)),
            ..Default::default()
        },
    );
    assert_eq!(field(&values[0], "tag"), Value::Int(BOOLEAN));
    assert_eq!(field(&values[0], "flag"), Value::Bool(true));
    assert_eq!(field(&values[1], "tag"), Value::Int(STRING));
    assert_eq!(values[2], Value::Float(2.0), "string length cell");
    assert_eq!(
        &values[3..5],
        &[
            Value::Float(f64::from(u16::from(b'a'))),
            Value::Float(f64::from(u16::from(b'b')))
        ]
    );
}

#[test]
fn saved_anonymous_callee_survives_a_later_actual_allocation_then_reclaims() {
    let live = observe(
        "var r=(function(){'use strict';return function(x){'use strict';return 1;};})()({s:\"payload\"});",
        observer::Observer {
            reads: vec!["r".into()],
            force_gc: true,
            collect: true,
            stats: true,
            ..Default::default()
        },
    );
    assert_eq!(field(&live[0], "number"), Value::Float(1.0));
    assert!(
        matches!(live.last(), Some(Value::Int(collections)) if *collections > 0),
        "later actual allocation must hit a real collection, got {live:?}"
    );
    let released = observe(
        "var r=(function(){'use strict';return function(x){'use strict';return {s:\"payload\"};};})()(1);r=null;",
        observer::Observer {
            force_gc: true,
            collect: true,
            stats: true,
            ..Default::default()
        },
    );
    let tail = &released[released.len() - 3..];
    assert!(
        matches!(tail[2], Value::Int(collections) if collections > 0),
        "reclamation arm collected, got {tail:?}"
    );
}

fn integer(value: &Value) -> i64 {
    match value {
        Value::Int(number) => *number,
        other => panic!("producer integer required, got {other:?}"),
    }
}

fn collections_of(values: &[Value]) -> i64 {
    assert!(
        values.len() >= STATS_SUFFIX,
        "selector 5 is the third stats value, got {values:?}"
    );
    let stats = &values[values.len() - STATS_SUFFIX..];
    assert!(
        stats.iter().all(|value| matches!(value, Value::Int(_))),
        "stats suffix must be the three Int counters, got {stats:?}"
    );
    integer(&stats[STATS_COLLECTIONS])
}

#[derive(Clone, Copy)]
struct Insn {
    ip: usize,
    end: usize,
    opcode: OpCode,
}

fn decode_range(code: &[u8], start: usize, end: usize) -> Vec<Insn> {
    let mut cursor = start;
    let mut decoded = Vec::new();
    while cursor < end && cursor < code.len() {
        let Ok(opcode) = OpCode::try_from(code[cursor]) else {
            break;
        };
        let insn_end = cursor + 1 + opcode.operand_len();
        if insn_end > code.len() {
            break;
        }
        decoded.push(Insn {
            ip: cursor,
            end: insn_end,
            opcode,
        });
        cursor = insn_end;
    }
    decoded
}

fn ldc_at(code: &[u8], constants: &[Value], ip: usize) -> Option<Value> {
    let index = u32::from_le_bytes(code[ip + 1..ip + 5].try_into().ok()?) as usize;
    constants.get(index).cloned()
}

fn call_operands(code: &[u8], ip: usize) -> (u16, u8) {
    (
        u16::from_le_bytes([code[ip + 1], code[ip + 2]]),
        code[ip + 3],
    )
}

fn call_script_operands(code: &[u8], ip: usize) -> (u32, u8) {
    (
        u32::from_le_bytes(code[ip + 1..ip + 5].try_into().expect("callscript")),
        code[ip + 5],
    )
}

fn format_insns(code: &[u8], constants: &[Value], insns: &[Insn]) -> String {
    insns
        .iter()
        .map(|insn| {
            let extra = match insn.opcode {
                OpCode::Ldc => format!(" {:?}", ldc_at(code, constants, insn.ip)),
                OpCode::Call => {
                    let (index, argc) = call_operands(code, insn.ip);
                    format!(" index={index} argc={argc}")
                }
                OpCode::CallScript => {
                    let (prototype, argc) = call_script_operands(code, insn.ip);
                    format!(" prototype={prototype} argc={argc}")
                }
                OpCode::CallValue => format!(" argc={}", code[insn.ip + 1]),
                _ => String::new(),
            };
            format!(
                "{}..{} {}{extra}",
                insn.ip,
                insn.end,
                insn.opcode.mnemonic()
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn implemented_debug_name(
    evidence: &ExpressionEvidence,
    prototype_id: u32,
) -> &(String, Vec<String>) {
    let imports: std::collections::HashSet<&str> =
        evidence.imports.iter().map(String::as_str).collect();
    let implemented: Vec<_> = evidence
        .debug_functions
        .iter()
        .filter(|function| !imports.contains(function.0.as_str()))
        .collect();
    let names: Vec<_> = implemented
        .iter()
        .map(|function| function.0.as_str())
        .collect();
    let function = implemented.get(prototype_id as usize).unwrap_or_else(|| {
        panic!("prototype {prototype_id} has no implemented debug function; implemented={names:?}")
    });
    assert_eq!(
        function.0, CHECKED_BASE,
        "prototype {prototype_id} maps to {}, implemented={names:?}",
        function.0
    );
    assert_eq!(
        function.1,
        vec![
            "carrier".to_string(),
            "slot".to_string(),
            "generation".to_string()
        ]
    );
    function
}

#[derive(Debug)]
enum RefusedAssert {
    State,
    Generation,
}

fn refused_assert(evidence: &ExpressionEvidence, frame: &FrameFact) -> RefusedAssert {
    let insns = decode_range(
        &evidence.code,
        frame.region_start as usize,
        frame.region_end as usize,
    );
    let assert_fn = BuiltinFunction::Assert;
    let assert_index = assert_fn.call_index();
    let assert_arity = assert_fn.arity();
    let position = insns
        .iter()
        .position(|insn| insn.end == evidence.ip)
        .or_else(|| insns.iter().position(|insn| insn.ip == evidence.ip))
        .unwrap_or_else(|| {
            panic!(
                "failure ip {} is not an instruction in the checked_base region {}..{}; window {}",
                evidence.ip,
                frame.region_start,
                frame.region_end,
                format_insns(&evidence.code, &evidence.constants, &insns)
            )
        });
    let failed = &insns[position];
    assert_eq!(
        failed.opcode,
        OpCode::Call,
        "failure ip {} decoded as {}",
        evidence.ip,
        format_insns(
            &evidence.code,
            &evidence.constants,
            std::slice::from_ref(failed),
        )
    );
    let (index, argc) = call_operands(&evidence.code, failed.ip);
    assert_eq!(index, assert_index, "canonical Assert call index");
    assert_eq!(argc, assert_arity, "canonical Assert arity");
    let window_start = position.saturating_sub(6);
    let window = &insns[window_start..position];
    let ceq = window
        .iter()
        .rfind(|insn| insn.opcode == OpCode::Ceq)
        .unwrap_or_else(|| {
            panic!(
                "Assert call has no preceding Ceq; window {}",
                format_insns(&evidence.code, &evidence.constants, window)
            )
        });
    let before_ceq = insns
        .iter()
        .position(|insn| insn.ip == ceq.ip)
        .expect("ceq position");
    let producers = &insns[before_ceq.saturating_sub(4)..before_ceq];
    let loads_active = producers.iter().any(|insn| {
        insn.opcode == OpCode::Ldc
            && ldc_at(&evidence.code, &evidence.constants, insn.ip) == Some(Value::Int(ROW_ACTIVE))
    });
    if loads_active {
        RefusedAssert::State
    } else if producers
        .iter()
        .filter(|insn| insn.opcode == OpCode::Ldloc)
        .count()
        >= 2
    {
        RefusedAssert::Generation
    } else {
        panic!(
            "cannot correlate Assert with state/generation producers; window {}",
            format_insns(
                &evidence.code,
                &evidence.constants,
                &insns[window_start..=position]
            )
        )
    }
}

fn assert_checked_base_call(evidence: &ExpressionEvidence, frame: &FrameFact) {
    let prototype_id = frame.prototype_id.expect("checked_base frame prototype");
    let function = implemented_debug_name(evidence, prototype_id);
    let arity = evidence
        .prototype_arity
        .get(prototype_id as usize)
        .copied()
        .unwrap_or_else(|| panic!("missing prototype arity {prototype_id}"));
    assert_eq!(usize::from(arity), function.1.len(), "prototype arity");
    let return_ip = frame.return_ip.expect("checked_base resumes bytecode");
    let root = evidence
        .regions
        .iter()
        .find(|region| region.2.is_none())
        .expect("root region");
    let root_insns = decode_range(&evidence.code, root.0 as usize, root.1 as usize);
    let call = root_insns
        .iter()
        .find(|insn| insn.end == return_ip)
        .unwrap_or_else(|| {
            panic!(
                "return ip {return_ip} is not a root instruction; root {}",
                format_insns(&evidence.code, &evidence.constants, &root_insns)
            )
        });
    assert_eq!(
        call.opcode,
        OpCode::CallScript,
        "observer tail calls checked_base"
    );
    let (called, argc) = call_script_operands(&evidence.code, call.ip);
    assert_eq!(called, prototype_id, "root call enters this frame");
    assert_eq!(argc, arity, "call arity is the prototype arity");
    let selectors = root_insns
        .iter()
        .filter(|insn| insn.end <= return_ip)
        .any(|insn| {
            insn.opcode == OpCode::Ldc
                && ldc_at(&evidence.code, &evidence.constants, insn.ip)
                    == Some(Value::Int(ROW_STATE_SELECTOR))
        })
        && root_insns
            .iter()
            .filter(|insn| insn.end <= return_ip)
            .any(|insn| {
                insn.opcode == OpCode::Ldc
                    && ldc_at(&evidence.code, &evidence.constants, insn.ip)
                        == Some(Value::Int(ROW_GENERATION_SELECTOR))
            });
    assert!(
        selectors,
        "row selectors 13 and 14 must be emitted before checked_base; root {}",
        format_insns(&evidence.code, &evidence.constants, &root_insns)
    );
}

fn assert_token_refused(evidence: &ExpressionEvidence, slot: i64, generation: i64) {
    let message = match &evidence.status {
        Err(VmError::HostError(text)) => text.clone(),
        other => panic!(
            "checked_base must refuse the measured token, got {other:?}; ip {} stack {:?}",
            evidence.ip, evidence.stack
        ),
    };
    assert_eq!(
        message, "assertion failed",
        "ip {} stack {:?}",
        evidence.ip, evidence.stack
    );
    let prototypes: Vec<_> = evidence
        .frames
        .iter()
        .map(|frame| frame.prototype_id)
        .collect();
    assert!(
        prototypes.len() == 2 && prototypes[0].is_none(),
        "root frame plus checked_base, got {prototypes:?}"
    );
    let frame = evidence.frames.last().expect("checked_base frame");
    let failure_ip = u32::try_from(evidence.ip).expect("failure ip");
    assert!(
        frame.region_start <= failure_ip && failure_ip <= frame.region_end,
        "failure ip {} outside frame {}..{}",
        evidence.ip,
        frame.region_start,
        frame.region_end
    );
    assert!(
        frame.operand_stack_base >= 2,
        "row pair missing below frame: {:?}",
        evidence.stack
    );
    assert!(
        evidence.stack.len() >= frame.operand_stack_base,
        "{:?}",
        evidence.stack
    );
    let row_state = integer(&evidence.stack[frame.operand_stack_base - 2]);
    let row_generation = integer(&evidence.stack[frame.operand_stack_base - 1]);
    let retained = &evidence.stack[frame.operand_stack_base..];
    assert_checked_base_call(evidence, frame);
    let kind = refused_assert(evidence, frame);
    let invalid = match row_state {
        ROW_FREE | ROW_RETIRED => true,
        ROW_ACTIVE => row_generation != generation,
        _ => false,
    };
    assert!(
        invalid,
        "row does not invalidate slot {slot} gen {generation}: state={row_state} row_gen={row_generation} retained={retained:?}"
    );
    match (&kind, row_state) {
        (RefusedAssert::State, ROW_FREE | ROW_RETIRED) => {}
        (RefusedAssert::Generation, ROW_ACTIVE) if row_generation != generation => {}
        _ => panic!(
            "assert {kind:?} does not match row state {row_state} row_gen {row_generation} presented {generation}"
        ),
    }
    eprintln!(
        "P7_GC_EVIDENCE refusal error={message} ip={} line={:?} assert={kind:?} slot={slot} presented_gen={generation} row_state={row_state} row_gen={row_generation} retained_len={} aot={} jit={} resources={} cert_after_invalid={} owners_retired={}",
        evidence.ip,
        evidence.line,
        retained.len(),
        evidence.aot,
        evidence.jit,
        evidence.resources_after,
        evidence.cert_after_invalid,
        evidence.owners_retired
    );
}

fn assert_live_object(values: &[Value]) -> (i64, i64, f64) {
    assert!(
        values.len() >= 7,
        "reads, packed header, row, and identity must trail the stack, got {values:?}"
    );
    let tail = &values[values.len() - 7..];
    assert_eq!(field(&tail[0], "tag"), Value::Int(OBJECT_TAG), "{tail:?}");
    assert_eq!(field(&tail[1], "tag"), Value::Int(BOOLEAN), "{tail:?}");
    assert_eq!(
        field(&tail[1], "flag"),
        Value::Bool(true),
        "r.s==='payload'"
    );
    let packed = match &tail[2] {
        Value::Float(number) => *number,
        other => panic!("packed object header must be the scalar probe, got {other:?}"),
    };
    assert!(packed.is_finite() && packed.fract() == 0.0, "{packed}");
    let packed_bits = packed as i64;
    let count = packed_bits / OBJECT_COUNT_FACTOR;
    let remainder = packed_bits % OBJECT_COUNT_FACTOR;
    assert_eq!(count, 1, "one own property, not a string length");
    assert_eq!(remainder, 1, "only the extensible flag is set");
    assert!(packed_bits <= OBJECT_MAX_PACKED_HEADER, "{packed_bits}");
    assert_eq!(tail[3], Value::Int(ROW_ACTIVE), "{tail:?}");
    let slot = integer(&tail[5]);
    let generation = integer(&tail[6]);
    assert_eq!(tail[4], Value::Int(generation), "row generation");
    assert_eq!(field(&tail[0], "ref_slot"), Value::Int(slot));
    assert_eq!(field(&tail[0], "generation"), Value::Int(generation));
    (slot, generation, packed)
}

#[test]
fn gc_evidence_liveness_collects_during_the_allocating_actual() {
    let returned = "(function(){'use strict';return function(x){'use strict';return 1;};})";
    let control_source = format!("{returned}();");
    let full_source = format!("var r={returned}()({{s:\"payload\"}});");
    let control = run_expression(
        &control_source,
        observer::Observer {
            force_gc: true,
            collect: false,
            stats: true,
            ..Default::default()
        },
    );
    let full = run_expression(
        &full_source,
        observer::Observer {
            reads: vec!["r".into()],
            force_gc: true,
            collect: false,
            stats: true,
            ..Default::default()
        },
    );
    assert!(
        matches!(control.status, Ok(VmStatus::Halted)),
        "control {control_source}: {:?}",
        control.status
    );
    assert!(
        matches!(full.status, Ok(VmStatus::Halted)),
        "full {full_source}: {:?}",
        full.status
    );
    let control_collections = collections_of(&control.stack);
    let full_collections = collections_of(&full.stack);
    let result = &full.stack[full.stack.len() - STATS_SUFFIX - 1];
    assert_eq!(field(result, "number"), Value::Float(1.0), "{full_source}");
    assert!(
        full_collections > control_collections,
        "selector 5 collections during the allocating actual ({full_collections}) must exceed the returned-function control ({control_collections})"
    );
    eprintln!(
        "P7_GC_EVIDENCE liveness control_collections={control_collections} full_collections={full_collections} result=1 aot={} jit={} resources={} cert_after_invalid={} owners_retired={}",
        full.aot, full.jit, full.resources_after, full.cert_after_invalid, full.owners_retired
    );
}

#[test]
fn gc_evidence_reclaims_the_returned_object_token() {
    let producer = "var r=(function(){'use strict';return function(x){'use strict';return {s:'payload'};};})()(1);var eq=r.s==='payload';";
    let positive = run_expression(
        producer,
        observer::Observer {
            reads: vec!["r".into(), "eq".into()],
            string: Some(("r".into(), 0)),
            row_of_binding: Some("r".into()),
            push_binding_identity: Some("r".into()),
            force_gc: true,
            collect: false,
            ..Default::default()
        },
    );
    assert!(
        matches!(positive.status, Ok(VmStatus::Halted)),
        "positive producer must halt before any refusal is evidence: {:?}",
        positive.status
    );
    let (slot, generation, packed) = assert_live_object(&positive.stack);
    eprintln!(
        "P7_GC_EVIDENCE live tag={OBJECT_TAG} packed_header={packed} count=1 extensible=1 slot={slot} generation={generation} row_state={ROW_ACTIVE} aot={} jit={} resources={} cert_after_invalid={} owners_retired={}",
        positive.aot,
        positive.jit,
        positive.resources_after,
        positive.cert_after_invalid,
        positive.owners_retired
    );
    let released = format!("{producer}r=null;var sink={{s:'zz'}};");
    let negative = run_expression(
        &released,
        observer::Observer {
            old_identity: Some((slot, generation, 0)),
            force_gc: true,
            collect: false,
            ..Default::default()
        },
    );
    assert_token_refused(&negative, slot, generation);
}

#[test]
fn numeric_alias_returned_callee_preserves_actual_order() {
    // choose(effect)(actual(effect)) evaluates the returned Math.abs once,
    // then the actual, left to right. Script-level log=log*10+digit is the
    // admitted global effect: a strict function cannot capture log, and
    // parameter arithmetic stays refused.
    let call = "var log=0;function choose(k){'use strict';return Math.abs;}function actual(k){'use strict';return -7;}choose(log=log*10+1)(actual(log=log*10+2));";
    assert_eq!(
        cookie(&format!("{call}log;")),
        12,
        "choose's digit 1 then actual's digit 2"
    );
    assert_eq!(cookie(call), 7, "Math.abs(-7) after one left-to-right call");
}

#[test]
fn numeric_conditional_sequence_assignment_callees() {
    // One bootstrap per program. Separate cookies do not finish under the
    // 300s recorder: each Math realm start is about thirty seconds.
    // Place values keep a wrong branch, a skipped effect, or a swapped
    // actual visible. 7_871_771 is abs, floor, sequence, sequence-log,
    // assignment, coerced string, coerced-log.
    assert_eq!(
        cookie(
            "var flag=1;var abs=(flag?Math.abs:Math.floor)(-7);flag=0;var floor=(flag?Math.abs:Math.floor)(8.9);var log=0;var seq=(log=log+1,Math.abs)(-7);var seqlog=log;var f;var assigned=(f=Math.abs)(-7);flag=1;log=0;var coerced=(flag?Math.abs:Math.floor)((log=log+1,'-7'));var coercedlog=log;abs*1000000+floor*100000+seq*10000+seqlog*1000+assigned*100+coerced*10+coercedlog;"
        ),
        7_871_771
    );
    let check = "function check(){'use strict';if(this===undefined){return 1;}return 2;}";
    assert_eq!(
        cookie(&format!(
            "{check}var flag=0;var a=(flag?Math.abs:check)();var b=(0,check)();var f;var c=(f=check)();a*100+b*10+c;"
        )),
        111,
        "conditional, sequence, and assignment callees keep undefined this"
    );
    assert_eq!(
        cookie(
            "var log=0;var flag=1;var chosen=(flag?Math.max:Math.min)(log=log*10+1,log=log*10+2);chosen*100+log;"
        ),
        1212,
        "unselected Math.min would return 1; right-to-left would leave log 21"
    );
}

fn numeric_operand_refusal(source: &str, span_text: &str) {
    let error = match pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
    {
        Ok(_) => panic!("unproved numeric operand must be refused: {source}"),
        Err(error) => error,
    };
    let concrete = error
        .downcast_ref::<vm::SourcePathError>()
        .expect("compiler error keeps a concrete type");
    let vm::SourcePathError::SourceWithMap {
        error: vm::SourceError::Parse(parse),
        sources,
    } = concrete
    else {
        panic!("mapped parse error, got {concrete:?}");
    };
    assert_eq!(
        parse.message, "P0 arithmetic currently requires numeric operands",
        "{source}"
    );
    let span = parse.span.expect("operand span");
    assert_eq!(
        &sources.source(span.source_id).expect("source")[span.lo..span.hi],
        span_text,
        "{source}"
    );
}

fn numeric_operand_admitted(source: &str) {
    pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source)
        .unwrap_or_else(|error| panic!("legal numeric operand must compile: {source}\n{error}"));
}

#[test]
fn numeric_provenance_rejects_unproved_bindings() {
    // Keep declaration-only lifecycle separate from append-only calls: a
    // dormant body has normal Undefined completion and proves no result kind.
    fn witness(source: &str, expected: Option<f64>) {
        let script = format!(
            "let v=(0,eval)({});let out={{kind:typeof v}};if(typeof v==='number'){{let b=Buffer.alloc(8);b.writeDoubleBE(v);out.bits=Number.isNaN(v)?'NaN':b.toString('hex');}}console.log(JSON.stringify(out));",
            serde_json::to_string(source).unwrap()
        );
        let version = std::process::Command::new("timeout")
            .args(["15", "node", "--version"])
            .output()
            .unwrap();
        let output = std::process::Command::new("timeout")
            .args(["15", "node", "-e", &script])
            .output()
            .unwrap();
        eprintln!(
            "MIG140C_NODE {}",
            serde_json::json!({"source":source,"script":script,"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr),"version":String::from_utf8_lossy(&version.stdout),"version_rc":version.status.code()})
        );
        assert!(version.status.success() && output.status.success() && output.stderr.is_empty());
        let wanted = match expected {
            Some(n) => {
                serde_json::json!({"kind":"number","bits":if n.is_nan(){"NaN".to_owned()}else{format!("{:016x}",n.to_bits())}})
            }
            None => serde_json::json!({"kind":"undefined"}),
        };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
            wanted,
            "{source}"
        );
        let mut machine =
            pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source).unwrap();
        machine.set_jit_config(vm::JitConfig {
            enabled: false,
            ..*machine.jit_config()
        });
        assert!(!machine.jit_config().enabled);
        assert_eq!(
            (machine.aot_exec_count(), machine.jit_native_exec_count()),
            (0, 0)
        );
        assert!(machine.capture_certification_stats().unwrap().valid);
        machine.set_fuel(10_000_000);
        let status = machine.run();
        eprintln!(
            "MIG140C_VM {}",
            serde_json::json!({"source":source,"status":format!("{status:?}"),"stack":format!("{:?}",machine.stack()),"aot":machine.aot_exec_count(),"jit":machine.jit_native_exec_count(),"jit_enabled":machine.jit_config().enabled})
        );
        assert_eq!(status.unwrap(), VmStatus::Halted, "{source}");
        match expected {
            Some(n) => {
                let result = machine.stack().last().unwrap();
                assert_eq!(field(result, "tag"), Value::Int(3));
                let Value::Float(actual) = field(result, "number") else {
                    panic!("Float result")
                };
                assert!(
                    if n.is_nan() {
                        actual.is_nan()
                    } else {
                        actual.to_bits() == n.to_bits()
                    },
                    "{source}: {actual:?} != {n:?}"
                );
            }
            None => assert!(
                machine.stack().is_empty(),
                "declaration-only normal empty completion evaluates to Undefined: {source}"
            ),
        }
        assert!(!machine.jit_config().enabled);
        assert_eq!(
            (machine.aot_exec_count(), machine.jit_native_exec_count()),
            (0, 0)
        );
        assert!(machine.capture_certification_stats().unwrap().valid);
        machine.reset_for_reuse().unwrap();
        assert!(!machine.scope_reset_pending());
        assert_eq!(machine.host_context().resource_count(), 0);
        assert!(!machine.capture_certification_stats().unwrap().valid);
        assert!(
            pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
                .is_none()
        );
        eprintln!(
            "MIG140C_RESET source={source:?} resources=0 pending=false cert=false owners_retired=true"
        );
    }
    fn migrated(source: &str, span: &str, suffix: &str, expected: f64) {
        assert!(source.contains(span), "original operator spelling");
        witness(source, None);
        witness(&format!("{source}{suffix}"), Some(expected));
    }
    migrated(
        "function f(x){'use strict';return x+1;}",
        "x+1",
        "f(true);",
        2.0,
    );
    migrated(
        "function f(x){'use strict';return -x;}",
        "-x",
        "f(true);",
        -1.0,
    );
    migrated(
        "function f(x){'use strict';var y=x;return y+1;}",
        "y+1",
        "f(true);",
        2.0,
    );
    migrated(
        "function f(x){'use strict';var y=1;y=x;return y+1;}",
        "y+1",
        "f(true);",
        2.0,
    );
    migrated(
        "function f(x){'use strict';if(x){x=1;}return x+1;}",
        "x+1",
        "f(true);",
        2.0,
    );
    migrated(
        "function f(x){'use strict';var n=1;while(x){n=x;}return n+1;}",
        "n+1",
        "f(false);",
        2.0,
    );
    migrated(
        "function f(){'use strict';try{throw 1;}catch(e){return e+1;}}",
        "e+1",
        "f();",
        2.0,
    );
    migrated(
        "function f(){'use strict';try{throw 1;}catch(e){var y=e;return y+1;}}",
        "y+1",
        "f();",
        2.0,
    );
    migrated(
        "function f(x){'use strict';var c=1;return (c&&x)+1;}",
        "(c&&x)+1",
        "f(true);",
        2.0,
    );
    numeric_operand_admitted("function f(x){'use strict';x=1;return x+1;}");
    numeric_operand_admitted("function f(x){'use strict';if(x){x=1;}else{x=2;}return x+1;}");
    numeric_operand_admitted("function f(){'use strict';return Math.abs(-2)+1;}");
    numeric_operand_admitted("function f(){'use strict';return -Math.abs(-2);}");
    // This original Dynamic Add body also executes its String branch. Runtime
    // Float observations above establish no static Number fact for formals.
    let original = "function f(x){'use strict';return x+1;}";
    witness(&format!("{original}f();"), Some(f64::NAN));
    let source = format!("{original}var r=f('A\\0\\ud800');r;");
    let expected = [65u16, 0, 55296, 49];
    let script = format!(
        "let v=(0,eval)({});console.log(JSON.stringify({{kind:typeof v,units:Array.from({{length:v.length}},(_,i)=>v.charCodeAt(i))}}));",
        serde_json::to_string(&source).unwrap()
    );
    let output = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    eprintln!(
        "MIG140C_NODE {}",
        serde_json::json!({"source":source,"script":script,"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr)})
    );
    assert!(output.status.success() && output.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!({"kind":"string","units":expected})
    );
    let catalog = reference_array_cli_effective_catalog();
    let probe = Box::leak(Box::new(observer::Observer {
        reads: vec!["r".into()],
        string: Some(("r".into(), expected.len())),
        ..Default::default()
    }));
    let issued = vm::compile_source_with_flavor_and_options_with_capture_certification(
        &source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-arity-observe")),
        vm::CompileSourceFileOptions::new()
            .with_host_api_catalog(catalog.clone())
            .with_source_plugin(probe),
    )
    .unwrap();
    let mut machine = issued.into_vm().unwrap();
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
            .unwrap();
    }
    registry.allow_builtin("math::floor").unwrap();
    registry.allow_builtin("bytes::from_utf8").unwrap();
    registry.bind_vm_cached(&mut machine).unwrap();
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        Default::default(),
    )
    .unwrap();
    machine.set_jit_config(vm::JitConfig {
        enabled: false,
        ..*machine.jit_config()
    });
    assert!(!machine.jit_config().enabled);
    assert_eq!(
        (machine.aot_exec_count(), machine.jit_native_exec_count()),
        (0, 0)
    );
    assert!(machine.capture_certification_stats().unwrap().valid);
    machine.set_fuel(10_000_000);
    let status = machine.run();
    eprintln!(
        "MIG140C_VM {}",
        serde_json::json!({"source":source,"status":format!("{status:?}"),"stack":format!("{:?}",machine.stack()),"aot":machine.aot_exec_count(),"jit":machine.jit_native_exec_count(),"jit_enabled":machine.jit_config().enabled})
    );
    assert_eq!(status.unwrap(), VmStatus::Halted);
    let stack = machine.stack();
    let at = stack.len() - expected.len() - 2;
    assert_eq!(field(&stack[at], "tag"), Value::Int(4));
    assert_eq!(stack[at + 1], Value::Float(expected.len() as f64));
    for (i, u) in expected.iter().enumerate() {
        assert_eq!(stack[at + 2 + i], Value::Float(f64::from(*u)));
    }
    assert!(!machine.jit_config().enabled);
    assert_eq!(
        (machine.aot_exec_count(), machine.jit_native_exec_count()),
        (0, 0)
    );
    assert!(machine.capture_certification_stats().unwrap().valid);
    machine.reset_for_reuse().unwrap();
    assert!(!machine.scope_reset_pending());
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(!machine.capture_certification_stats().unwrap().valid);
    assert!(
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).is_none()
    );
    eprintln!(
        "MIG140C_RESET source={source:?} resources=0 pending=false cert=false owners_retired=true"
    );
}

#[test]
fn numeric_provenance_preserves_guarded_results() {
    // One realm bootstrap. The file observer accepts only an inactive
    // primitive number box, so the cookie is the boxing proof as well as
    // the value. Digits: dominating call assignment, member, alias,
    // sequence copy, Number+eligible Dynamic join, once-order call,
    // reassignment, effect count.
    assert_eq!(
        cookie(
            "function f(x){'use strict';x=Math.abs(-2);return x+1;}var o={n:4};var y=Math.abs(-2);var seq=(0,y);var flag=0;var joined=flag?1:Math.abs(-3);var log=0;var once=(log=log+1,Math.abs)(-5);var w=1;w=Math.abs(-4);f(9)*10000000+(o.n+1)*1000000+(y+1)*100000+(seq+1)*10000+(joined+3)*1000+(once+1)*100+(w+1)*10+log;"
        ),
        35_336_651
    );
}

fn walk_expr(expr: &vm::Expr, visit: &mut dyn FnMut(&vm::Expr)) {
    visit(expr);
    use vm::Expr::*;
    match expr {
        Null
        | Int(_)
        | Float(_)
        | Bool(_)
        | String(_)
        | Bytes(_)
        | Var(_)
        | MoveVar(_)
        | MoveField { .. }
        | MoveIndex { .. }
        | FunctionRef(_, _)
        | ModuleFunctionRef(_, _)
        | UnresolvedFunctionRef { .. } => {}
        OptionalGet { container, key, .. } => {
            walk_expr(container, visit);
            walk_expr(key, visit);
        }
        OptionUnwrapOr {
            value, fallback, ..
        } => {
            walk_expr(value, visit);
            walk_expr(fallback, visit);
        }
        Call(_, _, args, _, _) | ModuleCall(_, _, args, _) | LocalCall(_, _, args, _) => {
            for arg in args {
                walk_expr(arg, visit);
            }
        }
        Closure(closure) => walk_expr(&closure.body, visit),
        ClosureCall(closure, args) => {
            walk_expr(&closure.body, visit);
            for arg in args {
                walk_expr(arg, visit);
            }
        }
        Add(left, right)
        | Sub(left, right)
        | Mul(left, right)
        | Div(left, right)
        | Mod(left, right)
        | And(left, right)
        | Or(left, right)
        | Eq(left, right)
        | Lt(left, right)
        | Gt(left, right) => {
            walk_expr(left, visit);
            walk_expr(right, visit);
        }
        Neg(inner) | Not(inner) | ToOwned(inner) | Borrow(inner) | BorrowMut(inner) => {
            walk_expr(inner, visit);
        }
        IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            walk_expr(condition, visit);
            walk_expr(then_expr, visit);
            walk_expr(else_expr, visit);
        }
        Match {
            value,
            arms,
            default,
            ..
        } => {
            walk_expr(value, visit);
            for (_, arm) in arms {
                walk_expr(arm, visit);
            }
            walk_expr(default, visit);
        }
        Block { stmts, expr } => {
            for statement in stmts {
                walk_stmt(statement, visit);
            }
            walk_expr(expr, visit);
        }
        ResultConstruct { payload, .. }
        | ResultQuery { value: payload, .. }
        | ResultTry { value: payload, .. } => {
            walk_expr(payload, visit);
        }
        ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            walk_expr(value, visit);
            walk_expr(ok_expr, visit);
            walk_expr(err_expr, visit);
        }
        _ => {}
    }
}

fn walk_stmt(statement: &vm::Stmt, visit: &mut dyn FnMut(&vm::Expr)) {
    match statement {
        vm::Stmt::Let { expr, .. }
        | vm::Stmt::Assign { expr, .. }
        | vm::Stmt::Expr { expr, .. } => {
            walk_expr(expr, visit);
        }
        vm::Stmt::IfElse {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            walk_expr(condition, visit);
            for item in then_branch.iter().chain(else_branch) {
                walk_stmt(item, visit);
            }
        }
        vm::Stmt::For {
            init,
            condition,
            post,
            body,
            ..
        } => {
            walk_stmt(init, visit);
            walk_expr(condition, visit);
            walk_stmt(post, visit);
            for item in body {
                walk_stmt(item, visit);
            }
        }
        vm::Stmt::While {
            condition, body, ..
        } => {
            walk_expr(condition, visit);
            for item in body {
                walk_stmt(item, visit);
            }
        }
        vm::Stmt::ClosureLet { closure, .. } => walk_expr(&closure.body, visit),
        vm::Stmt::Noop { .. }
        | vm::Stmt::FuncDecl { .. }
        | vm::Stmt::Break { .. }
        | vm::Stmt::Continue { .. }
        | vm::Stmt::Drop { .. } => {}
    }
}

struct RootReader {
    capacity: i64,
    entries: Vec<i64>,
}

fn selected_root_reader(source: &str) -> Option<RootReader> {
    use vm::SourcePlugin;
    let catalog = reference_array_cli_effective_catalog();
    let context = vm::SourceFrontendContext {
        source_id: 1,
        original_path: None,
        phase: vm::compiler::ParsePhase::Standalone,
        host_api_catalog: Some(&catalog),
        imports: &[],
    };
    let ir = pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN
        .parse_source(source, &context)
        .unwrap_or_else(|error| panic!("source must lower: {source}\n{error:?}"));
    let readers: Vec<_> = ir
        .functions
        .iter()
        .filter(|function| function.name == "__compat_es5_intrinsic_root")
        .collect();
    if readers.is_empty() {
        return None;
    }
    assert_eq!(readers.len(), 1, "root reader name is unique: {source}");
    let reader = readers[0];
    assert_eq!(reader.arity, 2, "{source}");
    assert_eq!(
        reader.args,
        ["arg0".to_owned(), "arg1".to_owned()],
        "{source}"
    );
    assert!(
        matches!(reader.arg_schemas.get(1), Some(Some(vm::TypeSchema::Int))),
        "entry schema: {source}"
    );
    assert!(
        !matches!(reader.arg_schemas.first(), Some(Some(vm::TypeSchema::Int))),
        "carrier schema is not Int: {source}"
    );
    assert_eq!(
        reader.return_schema,
        Some(vm::TypeSchema::Named(
            "__CompatEs5Primitive".to_owned(),
            Vec::new()
        )),
        "{source}"
    );
    assert_eq!(reader.return_type, vm::ValueType::Map, "{source}");
    let implementation = ir
        .function_impls
        .get(&reader.index)
        .unwrap_or_else(|| panic!("reader index {} has no impl: {source}", reader.index));
    assert_eq!(implementation.param_slots.len(), 2, "{source}");
    let entry_slot = implementation.param_slots[1];
    let mut bounds = Vec::new();
    let mut stored = Vec::new();
    let mut extents = Vec::new();
    let mut consider = |expr: &vm::Expr| {
        if let vm::Expr::Lt(left, right) = expr
            && let vm::Expr::Var(slot) = left.as_ref()
            && *slot == entry_slot
            && let vm::Expr::Int(bound) = right.as_ref()
            && *bound > 0
        {
            bounds.push(*bound);
        }
        if let vm::Expr::Eq(_, right) = expr
            && let vm::Expr::Int(value) = right.as_ref()
        {
            stored.push(*value);
        }
        if let vm::Expr::Int(value) = expr {
            extents.push(*value);
        }
    };
    for statement in &implementation.body_stmts {
        walk_stmt(statement, &mut consider);
    }
    walk_expr(&implementation.body_expr, &mut consider);
    let capacity = *bounds
        .iter()
        .max()
        .unwrap_or_else(|| panic!("entry bound: {source}"));
    assert!(
        stored.contains(&capacity),
        "stored frame capacity {capacity} missing in {stored:?}: {source}"
    );
    assert!(
        extents.contains(&(4 + 3 * capacity)),
        "extent 4+3*{capacity} missing: {source}"
    );
    let mut entries = Vec::new();
    let mut collect = |expr: &vm::Expr| {
        if let vm::Expr::Call(index, _, args, _, _) = expr
            && *index == reader.index
            && let Some(vm::Expr::Int(entry)) = args.get(1)
        {
            entries.push(*entry);
        }
    };
    for statement in &ir.stmts {
        walk_stmt(statement, &mut collect);
    }
    for (index, implementation) in &ir.function_impls {
        if *index == reader.index {
            continue;
        }
        for statement in &implementation.body_stmts {
            walk_stmt(statement, &mut collect);
        }
        walk_expr(&implementation.body_expr, &mut collect);
    }
    entries.sort_unstable();
    entries.dedup();
    assert!(
        entries.iter().all(|entry| *entry >= 0 && *entry < capacity),
        "root entries {entries:?} exceed frozen capacity {capacity}: {source}"
    );
    Some(RootReader { capacity, entries })
}

fn expect_roots(source: &str, capacity: i64, required: &[i64]) {
    let reader =
        selected_root_reader(source).unwrap_or_else(|| panic!("root reader missing: {source}"));
    assert_eq!(reader.capacity, capacity, "{source}");
    for entry in required {
        assert!(
            reader.entries.contains(entry),
            "missing root entry {entry} in {:?}: {source}",
            reader.entries
        );
    }
}

#[test]
fn function_builtin_demand_control_flow_topology() {
    expect_roots(
        "function f(){'use strict';try{return Math.abs(-1);}catch(e){return 0;}}",
        6,
        &[5],
    );
    expect_roots("function f(){'use strict';throw Math.abs(-1);}", 6, &[5]);
    expect_roots(
        "function f(){'use strict';switch(Math.abs(-1)){case parseInt('1',10):return isNaN(0);default:return 0;}}",
        8,
        &[5, 6, 7],
    );
    expect_roots(
        "function f(){'use strict';outer:return Math.abs(-1);}",
        6,
        &[5],
    );
    expect_roots(
        "function outer(){'use strict';try{var inner=function(){'use strict';return Math.abs(-1);};return inner();}catch(e){return 0;}}",
        6,
        &[5],
    );
    expect_roots(
        "try{var inner=function(){'use strict';return Math.abs(-1);};}catch(e){}",
        6,
        &[5],
    );
    expect_roots(
        "function f(){'use strict';try{throw 1;}catch(Math){return 0;}finally{var n=Math.abs(-1);}}",
        6,
        &[5],
    );
    expect_roots(
        "function f(){'use strict';try{return parseInt('11',10);}catch(e){return 0;}}",
        6,
        &[5],
    );
    let shadowed = selected_root_reader(
        "var o={};function f(){'use strict';try{throw 1;}catch(Math){return Math.abs(-1);}}",
    )
    .unwrap_or_else(|| panic!("object still selects an ordinary realm"));
    assert_eq!(shadowed.capacity, 5, "catch parameter must hide Math");
    assert!(
        !shadowed.entries.contains(&5),
        "catch Math must not allocate entry 5: {:?}",
        shadowed.entries
    );
    let hoisted = selected_root_reader(
        "var o={};function f(){'use strict';var Math=1;try{return Math;}catch(e){return 0;}}",
    )
    .unwrap_or_else(|| panic!("object still selects an ordinary realm"));
    assert_eq!(
        hoisted.capacity, 5,
        "hoisted var Math must hide the builtin"
    );
    assert!(!hoisted.entries.contains(&5), "{:?}", hoisted.entries);
    let catch_only =
        selected_root_reader("function f(){'use strict';try{throw 1;}catch(Math){return Math;}}")
            .unwrap_or_else(|| panic!("a source function still selects the ordinary root reader"));
    assert_eq!(
        catch_only.capacity, 5,
        "catch parameter must not select Math"
    );
    assert!(
        !catch_only.entries.contains(&5),
        "shadowed catch Math must not allocate entry 5: {:?}",
        catch_only.entries
    );
}

#[test]
fn function_builtin_demand_try_throw_interpreter() {
    assert_eq!(
        cookie(
            "function abrupt(){'use strict';var seen=0;try{seen=seen+Math.abs(-2);throw parseInt('7',10);}catch(e){if(e===7){seen=seen+10;}}finally{seen=seen+Math.floor(3.2);}return seen;}function normal(){'use strict';try{return Math.abs(-4);}catch(e){return 0;}finally{var z=parseInt('1',10);}}function shadow(){'use strict';try{return Math.abs(-6);}catch(Math){return 0;}}abrupt()*1000+normal()*10+shadow();"
        ),
        15_046
    );
}

#[test]
fn function_builtin_demand_switch_label_interpreter() {
    assert_eq!(
        cookie(
            "function f(n){'use strict';var r=0;switch(n){case parseInt('1',10):r=Math.abs(-8);break;case 2:r=Math.floor(1.9);break;default:r=0;}return r;}function labeled(){'use strict';var r=0;outer:{r=Math.abs(-5);break outer;r=99;}return r;}f(1)*100+f(2)*10+labeled();"
        ),
        815
    );
}

#[test]
fn numeric_provenance_rejects_throw_skipped_assignment() {
    // Keep declaration-only lifecycle separate from append-only calls: a
    // dormant body has normal Undefined completion and proves no result kind.
    fn witness(source: &str, expected: Option<f64>) {
        let script = format!(
            "let v=(0,eval)({});let out={{kind:typeof v}};if(typeof v==='number'){{let b=Buffer.alloc(8);b.writeDoubleBE(v);out.bits=Number.isNaN(v)?'NaN':b.toString('hex');}}console.log(JSON.stringify(out));",
            serde_json::to_string(source).unwrap()
        );
        let version = std::process::Command::new("timeout")
            .args(["15", "node", "--version"])
            .output()
            .unwrap();
        let output = std::process::Command::new("timeout")
            .args(["15", "node", "-e", &script])
            .output()
            .unwrap();
        eprintln!(
            "MIG140C_NODE {}",
            serde_json::json!({"source":source,"script":script,"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr),"version":String::from_utf8_lossy(&version.stdout),"version_rc":version.status.code()})
        );
        assert!(version.status.success() && output.status.success() && output.stderr.is_empty());
        let wanted = match expected {
            Some(n) => {
                serde_json::json!({"kind":"number","bits":if n.is_nan(){"NaN".to_owned()}else{format!("{:016x}",n.to_bits())}})
            }
            None => serde_json::json!({"kind":"undefined"}),
        };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
            wanted,
            "{source}"
        );
        let mut machine =
            pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source).unwrap();
        machine.set_jit_config(vm::JitConfig {
            enabled: false,
            ..*machine.jit_config()
        });
        assert!(!machine.jit_config().enabled);
        assert_eq!(
            (machine.aot_exec_count(), machine.jit_native_exec_count()),
            (0, 0)
        );
        assert!(machine.capture_certification_stats().unwrap().valid);
        machine.set_fuel(10_000_000);
        let status = machine.run();
        eprintln!(
            "MIG140C_VM {}",
            serde_json::json!({"source":source,"status":format!("{status:?}"),"stack":format!("{:?}",machine.stack()),"aot":machine.aot_exec_count(),"jit":machine.jit_native_exec_count(),"jit_enabled":machine.jit_config().enabled})
        );
        assert_eq!(status.unwrap(), VmStatus::Halted, "{source}");
        match expected {
            Some(n) => {
                let result = machine.stack().last().unwrap();
                assert_eq!(field(result, "tag"), Value::Int(3));
                let Value::Float(actual) = field(result, "number") else {
                    panic!("Float result")
                };
                assert!(
                    if n.is_nan() {
                        actual.is_nan()
                    } else {
                        actual.to_bits() == n.to_bits()
                    },
                    "{source}: {actual:?} != {n:?}"
                );
            }
            None => assert!(
                machine.stack().is_empty(),
                "declaration-only normal empty completion evaluates to Undefined: {source}"
            ),
        }
        assert!(!machine.jit_config().enabled);
        assert_eq!(
            (machine.aot_exec_count(), machine.jit_native_exec_count()),
            (0, 0)
        );
        assert!(machine.capture_certification_stats().unwrap().valid);
        machine.reset_for_reuse().unwrap();
        assert!(!machine.scope_reset_pending());
        assert_eq!(machine.host_context().resource_count(), 0);
        assert!(!machine.capture_certification_stats().unwrap().valid);
        assert!(
            pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
                .is_none()
        );
        eprintln!(
            "MIG140C_RESET source={source:?} resources=0 pending=false cert=false owners_retired=true"
        );
    }
    fn migrated(source: &str, span: &str, suffix: &str, expected: f64) {
        assert!(source.contains(span), "original operator spelling");
        witness(source, None);
        witness(&format!("{source}{suffix}"), Some(expected));
    }
    migrated(
        "function f(x){'use strict';var y=x;try{if(x){throw 1;}y=1;}finally{return y+1;}}",
        "y+1",
        "f(true);",
        2.0,
    );
    migrated(
        "function f(x){'use strict';var y=x;try{throw 1;y=1;}finally{return y+1;}}",
        "y+1",
        "f(true);",
        2.0,
    );
    migrated(
        "function f(x){'use strict';var y=x;try{throw 1;y=1;}catch(e){return y+1;}}",
        "y+1",
        "f(true);",
        2.0,
    );
    migrated(
        "function f(x){'use strict';var y=x;try{if(x){throw 1;}else{throw 2;}y=1;}catch(e){}return y+1;}",
        "y+1",
        "f(true);",
        2.0,
    );
    migrated(
        "function f(x){'use strict';var y=x;try{if(x){return 0;}y=1;}finally{return y+1;}}",
        "y+1",
        "f(true);",
        2.0,
    );
    migrated(
        "function f(x){'use strict';var y=x;while(x){try{if(x){break;}y=1;}finally{return y+1;}}return 0;}",
        "y+1",
        "f(true);",
        2.0,
    );
    migrated(
        "function f(x){'use strict';var y=x;while(x){try{if(x){continue;}y=1;}finally{return y+1;}}return 0;}",
        "y+1",
        "f(true);",
        2.0,
    );
}

#[test]
fn numeric_provenance_rejects_nested_catch_shadow() {
    // Keep declaration-only lifecycle separate from append-only calls: a
    // dormant body has normal Undefined completion and proves no result kind.
    fn witness(source: &str, expected: Option<f64>) {
        let script = format!(
            "let v=(0,eval)({});let out={{kind:typeof v}};if(typeof v==='number'){{let b=Buffer.alloc(8);b.writeDoubleBE(v);out.bits=Number.isNaN(v)?'NaN':b.toString('hex');}}console.log(JSON.stringify(out));",
            serde_json::to_string(source).unwrap()
        );
        let version = std::process::Command::new("timeout")
            .args(["15", "node", "--version"])
            .output()
            .unwrap();
        let output = std::process::Command::new("timeout")
            .args(["15", "node", "-e", &script])
            .output()
            .unwrap();
        eprintln!(
            "MIG140C_NODE {}",
            serde_json::json!({"source":source,"script":script,"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr),"version":String::from_utf8_lossy(&version.stdout),"version_rc":version.status.code()})
        );
        assert!(version.status.success() && output.status.success() && output.stderr.is_empty());
        let wanted = match expected {
            Some(n) => {
                serde_json::json!({"kind":"number","bits":if n.is_nan(){"NaN".to_owned()}else{format!("{:016x}",n.to_bits())}})
            }
            None => serde_json::json!({"kind":"undefined"}),
        };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
            wanted,
            "{source}"
        );
        let mut machine =
            pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source).unwrap();
        machine.set_jit_config(vm::JitConfig {
            enabled: false,
            ..*machine.jit_config()
        });
        assert!(!machine.jit_config().enabled);
        assert_eq!(
            (machine.aot_exec_count(), machine.jit_native_exec_count()),
            (0, 0)
        );
        assert!(machine.capture_certification_stats().unwrap().valid);
        machine.set_fuel(10_000_000);
        let status = machine.run();
        eprintln!(
            "MIG140C_VM {}",
            serde_json::json!({"source":source,"status":format!("{status:?}"),"stack":format!("{:?}",machine.stack()),"aot":machine.aot_exec_count(),"jit":machine.jit_native_exec_count(),"jit_enabled":machine.jit_config().enabled})
        );
        assert_eq!(status.unwrap(), VmStatus::Halted, "{source}");
        match expected {
            Some(n) => {
                let result = machine.stack().last().unwrap();
                assert_eq!(field(result, "tag"), Value::Int(3));
                let Value::Float(actual) = field(result, "number") else {
                    panic!("Float result")
                };
                assert!(
                    if n.is_nan() {
                        actual.is_nan()
                    } else {
                        actual.to_bits() == n.to_bits()
                    },
                    "{source}: {actual:?} != {n:?}"
                );
            }
            None => assert!(
                machine.stack().is_empty(),
                "declaration-only normal empty completion evaluates to Undefined: {source}"
            ),
        }
        assert!(!machine.jit_config().enabled);
        assert_eq!(
            (machine.aot_exec_count(), machine.jit_native_exec_count()),
            (0, 0)
        );
        assert!(machine.capture_certification_stats().unwrap().valid);
        machine.reset_for_reuse().unwrap();
        assert!(!machine.scope_reset_pending());
        assert_eq!(machine.host_context().resource_count(), 0);
        assert!(!machine.capture_certification_stats().unwrap().valid);
        assert!(
            pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
                .is_none()
        );
        eprintln!(
            "MIG140C_RESET source={source:?} resources=0 pending=false cert=false owners_retired=true"
        );
    }
    fn migrated(source: &str, span: &str, suffix: &str, expected: f64) {
        assert!(source.contains(span), "original operator spelling");
        witness(source, None);
        witness(&format!("{source}{suffix}"), Some(expected));
    }
    migrated(
        "function f(){'use strict';var x=1;try{throw 1;}catch(x){try{throw 2;}catch(e){}return x+1;}}",
        "x+1",
        "f();",
        2.0,
    );
    migrated(
        "function f(){'use strict';var x=1;try{throw 1;}catch(x){try{throw 2;}catch(e){return x+1;}return 0;}}",
        "x+1",
        "f();",
        2.0,
    );
}

#[test]
fn numeric_provenance_dominating_try_interpreter() {
    assert_eq!(
        cookie("function f(){'use strict';var y;try{y=1;}finally{return y+1;}}f();"),
        2
    );
}

#[test]
fn numeric_provenance_nested_catch_guarded_interpreter() {
    assert_eq!(
        cookie(
            "function f(){'use strict';try{throw 1;}catch(x){x=Math.abs(-2);try{throw 2;}catch(e){return x+1;}return 0;}}f();"
        ),
        3
    );
}

#[test]
fn numeric_provenance_rejects_labeled_break_successor() {
    // Keep declaration-only lifecycle separate from append-only calls: a
    // dormant body has normal Undefined completion and proves no result kind.
    fn witness(source: &str, expected: Option<f64>) {
        let script = format!(
            "let v=(0,eval)({});let out={{kind:typeof v}};if(typeof v==='number'){{let b=Buffer.alloc(8);b.writeDoubleBE(v);out.bits=Number.isNaN(v)?'NaN':b.toString('hex');}}console.log(JSON.stringify(out));",
            serde_json::to_string(source).unwrap()
        );
        let version = std::process::Command::new("timeout")
            .args(["15", "node", "--version"])
            .output()
            .unwrap();
        let output = std::process::Command::new("timeout")
            .args(["15", "node", "-e", &script])
            .output()
            .unwrap();
        eprintln!(
            "MIG140C_NODE {}",
            serde_json::json!({"source":source,"script":script,"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr),"version":String::from_utf8_lossy(&version.stdout),"version_rc":version.status.code()})
        );
        assert!(version.status.success() && output.status.success() && output.stderr.is_empty());
        let wanted = match expected {
            Some(n) => {
                serde_json::json!({"kind":"number","bits":if n.is_nan(){"NaN".to_owned()}else{format!("{:016x}",n.to_bits())}})
            }
            None => serde_json::json!({"kind":"undefined"}),
        };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
            wanted,
            "{source}"
        );
        let mut machine =
            pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source).unwrap();
        machine.set_jit_config(vm::JitConfig {
            enabled: false,
            ..*machine.jit_config()
        });
        assert!(!machine.jit_config().enabled);
        assert_eq!(
            (machine.aot_exec_count(), machine.jit_native_exec_count()),
            (0, 0)
        );
        assert!(machine.capture_certification_stats().unwrap().valid);
        machine.set_fuel(10_000_000);
        let status = machine.run();
        eprintln!(
            "MIG140C_VM {}",
            serde_json::json!({"source":source,"status":format!("{status:?}"),"stack":format!("{:?}",machine.stack()),"aot":machine.aot_exec_count(),"jit":machine.jit_native_exec_count(),"jit_enabled":machine.jit_config().enabled})
        );
        assert_eq!(status.unwrap(), VmStatus::Halted, "{source}");
        match expected {
            Some(n) => {
                let result = machine.stack().last().unwrap();
                assert_eq!(field(result, "tag"), Value::Int(3));
                let Value::Float(actual) = field(result, "number") else {
                    panic!("Float result")
                };
                assert!(
                    if n.is_nan() {
                        actual.is_nan()
                    } else {
                        actual.to_bits() == n.to_bits()
                    },
                    "{source}: {actual:?} != {n:?}"
                );
            }
            None => assert!(
                machine.stack().is_empty(),
                "declaration-only normal empty completion evaluates to Undefined: {source}"
            ),
        }
        assert!(!machine.jit_config().enabled);
        assert_eq!(
            (machine.aot_exec_count(), machine.jit_native_exec_count()),
            (0, 0)
        );
        assert!(machine.capture_certification_stats().unwrap().valid);
        machine.reset_for_reuse().unwrap();
        assert!(!machine.scope_reset_pending());
        assert_eq!(machine.host_context().resource_count(), 0);
        assert!(!machine.capture_certification_stats().unwrap().valid);
        assert!(
            pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
                .is_none()
        );
        eprintln!(
            "MIG140C_RESET source={source:?} resources=0 pending=false cert=false owners_retired=true"
        );
    }
    fn migrated(source: &str, span: &str, suffix: &str, expected: f64) {
        assert!(source.contains(span), "original operator spelling");
        witness(source, None);
        witness(&format!("{source}{suffix}"), Some(expected));
    }
    migrated(
        "function f(x){'use strict';var y=1;try{L:{break L;}y=x;}finally{return y+1;}}",
        "y+1",
        "f(true);",
        2.0,
    );
    migrated(
        "function f(x){'use strict';var y=1;L:{y=x;break L;}return y+1;}",
        "y+1",
        "f(true);",
        2.0,
    );
    migrated(
        "function f(x){'use strict';var y=1;outer:inner:{break outer;}y=x;return y+1;}",
        "y+1",
        "f(true);",
        2.0,
    );
    migrated(
        "function f(x){'use strict';var y=1;outer:inner:{break inner;}y=x;return y+1;}",
        "y+1",
        "f(true);",
        2.0,
    );
    migrated(
        "function f(x){'use strict';var y=1;outer:{inner:{break inner;}y=x;}return y+1;}",
        "y+1",
        "f(true);",
        2.0,
    );
    migrated(
        "function f(x){'use strict';var y=1;outer:{inner:{y=x;break outer;}}return y+1;}",
        "y+1",
        "f(true);",
        2.0,
    );
}

#[test]
fn numeric_provenance_rejects_finally_abrupt_stores() {
    // Keep declaration-only lifecycle separate from append-only calls: a
    // dormant body has normal Undefined completion and proves no result kind.
    fn witness(source: &str, expected: Option<f64>) {
        let script = format!(
            "let v=(0,eval)({});let out={{kind:typeof v}};if(typeof v==='number'){{let b=Buffer.alloc(8);b.writeDoubleBE(v);out.bits=Number.isNaN(v)?'NaN':b.toString('hex');}}console.log(JSON.stringify(out));",
            serde_json::to_string(source).unwrap()
        );
        let version = std::process::Command::new("timeout")
            .args(["15", "node", "--version"])
            .output()
            .unwrap();
        let output = std::process::Command::new("timeout")
            .args(["15", "node", "-e", &script])
            .output()
            .unwrap();
        eprintln!(
            "MIG140C_NODE {}",
            serde_json::json!({"source":source,"script":script,"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr),"version":String::from_utf8_lossy(&version.stdout),"version_rc":version.status.code()})
        );
        assert!(version.status.success() && output.status.success() && output.stderr.is_empty());
        let wanted = match expected {
            Some(n) => {
                serde_json::json!({"kind":"number","bits":if n.is_nan(){"NaN".to_owned()}else{format!("{:016x}",n.to_bits())}})
            }
            None => serde_json::json!({"kind":"undefined"}),
        };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
            wanted,
            "{source}"
        );
        let mut machine =
            pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source).unwrap();
        machine.set_jit_config(vm::JitConfig {
            enabled: false,
            ..*machine.jit_config()
        });
        assert!(!machine.jit_config().enabled);
        assert_eq!(
            (machine.aot_exec_count(), machine.jit_native_exec_count()),
            (0, 0)
        );
        assert!(machine.capture_certification_stats().unwrap().valid);
        machine.set_fuel(10_000_000);
        let status = machine.run();
        eprintln!(
            "MIG140C_VM {}",
            serde_json::json!({"source":source,"status":format!("{status:?}"),"stack":format!("{:?}",machine.stack()),"aot":machine.aot_exec_count(),"jit":machine.jit_native_exec_count(),"jit_enabled":machine.jit_config().enabled})
        );
        assert_eq!(status.unwrap(), VmStatus::Halted, "{source}");
        match expected {
            Some(n) => {
                let result = machine.stack().last().unwrap();
                assert_eq!(field(result, "tag"), Value::Int(3));
                let Value::Float(actual) = field(result, "number") else {
                    panic!("Float result")
                };
                assert!(
                    if n.is_nan() {
                        actual.is_nan()
                    } else {
                        actual.to_bits() == n.to_bits()
                    },
                    "{source}: {actual:?} != {n:?}"
                );
            }
            None => assert!(
                machine.stack().is_empty(),
                "declaration-only normal empty completion evaluates to Undefined: {source}"
            ),
        }
        assert!(!machine.jit_config().enabled);
        assert_eq!(
            (machine.aot_exec_count(), machine.jit_native_exec_count()),
            (0, 0)
        );
        assert!(machine.capture_certification_stats().unwrap().valid);
        machine.reset_for_reuse().unwrap();
        assert!(!machine.scope_reset_pending());
        assert_eq!(machine.host_context().resource_count(), 0);
        assert!(!machine.capture_certification_stats().unwrap().valid);
        assert!(
            pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
                .is_none()
        );
        eprintln!(
            "MIG140C_RESET source={source:?} resources=0 pending=false cert=false owners_retired=true"
        );
    }
    fn migrated(source: &str, span: &str, suffix: &str, expected: f64) {
        assert!(source.contains(span), "original operator spelling");
        witness(source, None);
        witness(&format!("{source}{suffix}"), Some(expected));
    }
    migrated(
        "function f(x){'use strict';var y=1;try{try{throw 1;}finally{y=x;}}finally{return y+1;}}",
        "y+1",
        "f(true);",
        2.0,
    );
    migrated(
        "function f(x){'use strict';var y=1;try{try{return 0;}finally{y=x;}}finally{return y+1;}}",
        "y+1",
        "f(true);",
        2.0,
    );
    migrated(
        "function f(x){'use strict';var y=1;while(x){try{try{break;}finally{y=x;}}finally{return y+1;}}return 0;}",
        "y+1",
        "f(true);",
        2.0,
    );
    migrated(
        "function f(x){'use strict';var y=1;while(x){try{try{continue;}finally{y=x;}}finally{return y+1;}}return 0;}",
        "y+1",
        "f(true);",
        2.0,
    );
    migrated(
        "function f(x){'use strict';var y=1;L:{try{try{break L;}finally{y=x;}}finally{return y+1;}}return 0;}",
        "y+1",
        "f(true);",
        2.0,
    );
}

#[test]
fn numeric_provenance_labeled_break_interpreter() {
    assert_eq!(
        cookie("function f(){'use strict';var y;outer:inner:{y=2;break outer;}return y+1;}f();"),
        3
    );
    numeric_operand_admitted(
        "function f(x){'use strict';var y=1;outer:while(x){L:{continue outer;}y=x;}return y+1;}",
    );
    numeric_operand_admitted(
        "function f(x){'use strict';var y=1;outer:{inner:{break outer;}y=x;}return y+1;}",
    );
}

#[test]
fn numeric_provenance_finally_abrupt_guarded_interpreter() {
    assert_eq!(
        cookie(
            "function f(){'use strict';var y;try{try{throw 1;}finally{y=Math.abs(-2);}}finally{return y+1;}}f();"
        ),
        3
    );
}
