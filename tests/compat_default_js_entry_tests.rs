//! Normal default entry integration for the existing JavaScript ES5 frontend.
//!
//! These tests exercise the ordinary public `compile_options()` and the single
//! `pd-vm-compat-run` file route. No JavaScript selector, profile or separate
//! CLI service factory participates.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use vm::{Value, VmStatus};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/javascript_es5")
        .join(name)
}

fn cli(args: &[&str]) -> std::process::Output {
    let output = Command::new(env!("CARGO_BIN_EXE_pd-vm-compat-run"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run pd-vm-compat-run");
    eprintln!(
        "DEFAULT_ENTRY_CLI args={args:?} rc={:?}\nDEFAULT_ENTRY_LIMITS {}\nstdout={}\nstderr={}",
        output.status.code(),
        stack_limit_summary(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn stack_limit_summary() -> String {
    let limits = std::fs::read_to_string("/proc/self/limits").unwrap_or_default();
    let stack = limits
        .lines()
        .find(|line| line.trim_start().starts_with("Max stack size"))
        .unwrap_or("Max stack size: <unavailable>");
    format!(
        "{stack} RUST_MIN_STACK={:?}",
        std::env::var("RUST_MIN_STACK")
    )
}

fn field(value: &Value, name: &str) -> Value {
    let Value::Map(map) = value else {
        panic!("boxed completion: {value:?}")
    };
    map.get(&Value::string(name))
        .unwrap_or_else(|| panic!("boxed field {name}: {value:?}"))
        .clone()
}

/// Final boxed JavaScript completion read from the actual Core CLI stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FinalCliCompletion {
    tag: i64,
    flag: bool,
}

/// Splits on the literal `", "` separator only outside strings and brackets.
///
/// This is a deliberately narrow test helper for Core's actual `stack: [...]`
/// debug line. Anything outside the shapes this project emits is an error, so
/// format drift fails the assertion instead of silently matching a prefix.
fn split_top_level(input: &str) -> Result<Vec<&str>, String> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut start = 0usize;
    let bytes = input.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| "unbalanced closing bracket".to_owned())?;
            }
            b',' if depth == 0 && bytes.get(index + 1) == Some(&b' ') => {
                parts.push(&input[start..index]);
                index += 2;
                start = index;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    if in_string {
        return Err("unterminated string".to_owned());
    }
    if depth != 0 {
        return Err("unbalanced brackets".to_owned());
    }
    parts.push(&input[start..]);
    Ok(parts)
}

#[derive(Clone, Copy)]
enum CompletionScalar {
    Bool(bool),
    Int(i64),
    Other,
}

fn parse_completion_field(entry: &str) -> Result<(String, CompletionScalar), String> {
    let (key, value) = entry
        .split_once(": ")
        .ok_or_else(|| format!("completion field without `: `: {entry}"))?;
    let key = key
        .strip_prefix("String(\"")
        .and_then(|key| key.strip_suffix("\")"))
        .ok_or_else(|| format!("completion key is not a plain String: {key}"))?;
    if key.is_empty() || key.contains(['"', '\\']) {
        return Err(format!("unsupported completion key: {key}"));
    }
    let scalar = if let Some(raw) = value
        .strip_prefix("Bool(")
        .and_then(|value| value.strip_suffix(')'))
    {
        match raw {
            "true" => CompletionScalar::Bool(true),
            "false" => CompletionScalar::Bool(false),
            _ => return Err(format!("invalid Bool field value: {value}")),
        }
    } else if let Some(raw) = value
        .strip_prefix("Int(")
        .and_then(|value| value.strip_suffix(')'))
    {
        CompletionScalar::Int(
            raw.parse::<i64>()
                .map_err(|_| format!("invalid Int field value: {value}"))?,
        )
    } else if (value.starts_with("Float(") && value.ends_with(')'))
        || (value.starts_with("String(\"") && value.ends_with("\")"))
    {
        CompletionScalar::Other
    } else {
        return Err(format!("unsupported completion field value: {value}"));
    };
    Ok((key.to_owned(), scalar))
}

/// Requires the *final* stack value to be the boxed JavaScript completion map
/// with `tag == 2` (Boolean) and `flag == true`, independent of any earlier
/// stack or debug values. Fails closed on any unexpected stdout shape.
fn cli_true_completion(stdout: &str) -> Result<FinalCliCompletion, String> {
    let stack_line = stdout
        .lines()
        .rev()
        .find(|line| line.starts_with("stack: ["))
        .ok_or_else(|| "CLI stdout has no `stack: [` line".to_owned())?;
    let stack_line = stack_line.trim_end();
    let body = stack_line
        .strip_prefix("stack: [")
        .and_then(|rest| rest.strip_suffix(']'))
        .ok_or_else(|| format!("malformed stack line: {stack_line}"))?;
    let elements = split_top_level(body)?;
    let final_element = elements
        .last()
        .ok_or_else(|| "empty final stack".to_owned())?;
    let map_body = final_element
        .strip_prefix("Map({")
        .and_then(|rest| rest.strip_suffix("})"))
        .ok_or_else(|| {
            format!("final stack element is not the boxed completion map: {final_element}")
        })?;
    let mut tag = None;
    let mut flag = None;
    for entry in split_top_level(map_body)? {
        let (key, scalar) = parse_completion_field(entry)?;
        match (key.as_str(), scalar) {
            ("tag", CompletionScalar::Int(value)) => {
                if tag.replace(value).is_some() {
                    return Err("duplicate `tag` field in boxed completion".to_owned());
                }
            }
            ("flag", CompletionScalar::Bool(value)) => {
                if flag.replace(value).is_some() {
                    return Err("duplicate `flag` field in boxed completion".to_owned());
                }
            }
            ("tag", _) => return Err("boxed completion `tag` is not an Int".to_owned()),
            ("flag", _) => return Err("boxed completion `flag` is not a Bool".to_owned()),
            _ => {}
        }
    }
    let tag = tag.ok_or_else(|| "boxed completion has no `tag` field".to_owned())?;
    let flag = flag.ok_or_else(|| "boxed completion has no `flag` field".to_owned())?;
    if tag != 2 {
        return Err(format!(
            "final boxed completion tag {tag} is not the Boolean tag 2"
        ));
    }
    if !flag {
        return Err("final boxed completion flag is false".to_owned());
    }
    Ok(FinalCliCompletion { tag, flag })
}

#[test]
fn final_completion_helper_requires_the_final_true_boxed_record() {
    // Synthetic stdout fragments only: these are unit fixtures for the parser,
    // not claims about actual VM output.
    let negative_cases = [
        (
            "preceding Bool(true) with a false final flag",
            "vm halted\nstack: [Bool(true), Map({String(\"flag\"): Bool(false), String(\"tag\"): Int(2)})]\n",
            "flag is false",
        ),
        (
            "true final flag with the wrong final tag",
            "vm halted\nstack: [Bool(true), Map({String(\"flag\"): Bool(true), String(\"tag\"): Int(7)})]\n",
            "tag 7 is not the Boolean tag 2",
        ),
        (
            "no boxed completion",
            "vm halted\nstack: [Bool(true)]\n",
            "not the boxed completion map",
        ),
        (
            "malformed boxed completion",
            "vm halted\nstack: [Bool(true), Map({String(\"flag\"): Bool(true), String(\"tag\") Int(2)})]\n",
            "without `: `",
        ),
        ("missing stack line", "vm halted\n", "no `stack: [` line"),
    ];
    for (case, stdout, expected_error) in negative_cases {
        let error =
            cli_true_completion(stdout).expect_err(&format!("{case} must be rejected: {stdout}"));
        assert!(
            error.contains(expected_error),
            "{case} must fail for its own reason, got: {error}"
        );
    }

    let positive = "vm halted\nstack: [Bool(false), Map({String(\"flag\"): Bool(false), String(\"tag\"): Int(9)}), Bool(true), Map({String(\"number\"): Float(0.0), String(\"flag\"): Bool(true), String(\"generation\"): Int(0), String(\"ref_slot\"): Int(0), String(\"tag\"): Int(2)})]\ntrace-jit:\n  enabled: false\n";
    assert_eq!(
        cli_true_completion(positive).expect("final true boxed completion"),
        FinalCliCompletion { tag: 2, flag: true }
    );
}

#[test]
fn normal_default_compile_options_accept_persistent_js_entry() {
    let options = pd_vm_compat_frontends::compile_options();
    let issued = match vm::compile_source_file_with_capture_certification_and_options(
        fixture("default_entry_values.js"),
        options,
    ) {
        Ok(issued) => issued,
        Err(error) => panic!(
            "normal default compile options must accept the existing .js frontend; got {error:?}"
        ),
    };
    let mut machine = issued.into_vm().expect("certified JavaScript source");
    assert!(machine.capture_certification_stats().unwrap().valid);
    machine.set_fuel(10_000_000);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    let result = machine.stack().last().expect("completion value");
    assert_eq!(field(result, "tag"), Value::Int(2));
    assert_eq!(field(result, "flag"), Value::Bool(true));
    assert_eq!(machine.host_context().resource_count(), 0);
}

#[test]
fn normal_default_cli_executes_js_without_selector() {
    let output = cli(&[
        "--fuel",
        "10000000",
        "--jit-dump",
        fixture("default_entry_values.js").to_str().unwrap(),
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "default .js entry failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let completion = cli_true_completion(&stdout).unwrap_or_else(|error| {
        panic!(
            "default .js entry must end in the true boxed completion: {error}\nstdout:\n{stdout}"
        )
    });
    eprintln!(
        "DEFAULT_ENTRY_FINAL_COMPLETION tag={} flag={}",
        completion.tag, completion.flag
    );
    assert!(stderr.is_empty(), "guest panic or refusal: {stderr}");
    assert!(
        stdout.contains("enabled: true"),
        "the normal default route must keep Core's backend policy, not force a JavaScript mode: {stdout}"
    );
}

#[test]
fn normal_default_cli_executes_has_own_property_group() {
    let output = cli(&[
        "--fuel",
        "10000000",
        "--jit-dump",
        fixture("default_entry_has_own_property.js")
            .to_str()
            .unwrap(),
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "default hasOwnProperty .js entry failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let completion = cli_true_completion(&stdout).unwrap_or_else(|error| {
        panic!(
            "default hasOwnProperty entry must end in the true boxed completion: {error}\nstdout:\n{stdout}"
        )
    });
    eprintln!(
        "DEFAULT_ENTRY_FINAL_COMPLETION tag={} flag={}",
        completion.tag, completion.flag
    );
    assert!(stderr.is_empty(), "guest panic or refusal: {stderr}");
    // Core owns the backend policy; this records its actual decision for the
    // ordinary source instead of forcing a JavaScript mode.
    assert!(
        stdout.contains("enabled: "),
        "the normal default route must report Core's actual backend: {stdout}"
    );
}

#[test]
fn normal_default_cli_executes_object_keys_length_consumer() {
    let output = cli(&[
        "--fuel",
        "10000000",
        "--jit-dump",
        fixture("default_entry_object_keys.js").to_str().unwrap(),
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "default Object.keys .js entry failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let completion = cli_true_completion(&stdout).unwrap_or_else(|error| {
        panic!(
            "default Object.keys entry must end in the true boxed completion: {error}\nstdout:\n{stdout}"
        )
    });
    eprintln!(
        "DEFAULT_ENTRY_FINAL_COMPLETION tag={} flag={}",
        completion.tag, completion.flag
    );
    assert!(stderr.is_empty(), "guest panic or refusal: {stderr}");
    // The backend policy line is Core's actual measured decision for this
    // ordinary source; the test records it instead of forcing a JS mode.
    assert!(
        stdout.contains("enabled: "),
        "Core's default backend report must be present: {stdout}"
    );
}

#[test]
fn normal_default_cli_executes_heap_math_prototype_array_and_for_of() {
    let output = cli(&[
        "--fuel",
        "10000000",
        "--jit-dump",
        fixture("default_entry_heap.js").to_str().unwrap(),
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "default heap .js entry failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let completion = cli_true_completion(&stdout).unwrap_or_else(|error| {
        panic!("default heap .js entry must end in the true boxed completion: {error}\nstdout:\n{stdout}")
    });
    eprintln!(
        "DEFAULT_ENTRY_FINAL_COMPLETION tag={} flag={}",
        completion.tag, completion.flag
    );
    assert!(stderr.is_empty(), "guest panic or refusal: {stderr}");
    // Core's own wide-frame policy is the only reason this heap shape stops
    // tracing: one compile attempt is rejected and Core disables JIT for this
    // VM. Compat neither forces nor overrides that backend decision.
    assert!(
        stdout.contains("compile attempts: 1") && stdout.contains("nyi attempts: 1"),
        "actual backend attempt must be reported: {stdout}"
    );
    assert!(
        stdout.contains("nyi root_ip=") && stdout.contains("too many locals"),
        "the measured refusal reason must be visible: {stdout}"
    );
    assert!(
        stdout.contains("enabled: false"),
        "Core's measured wide-frame policy, not a Compat JavaScript mode: {stdout}"
    );
}

#[test]
fn normal_default_plugins_and_services_share_the_existing_js_identity_and_catalog() {
    let ids: Vec<&str> = pd_vm_compat_frontends::plugins()
        .iter()
        .map(|plugin| plugin.descriptor().id.as_str())
        .collect();
    assert_eq!(ids, ["typescript", "lua", "javascript-es5"]);
    let services = pd_vm_compat_frontends::runtime::cli::typescript_cli_host_services();
    assert!(
        services
            .compile_options(pd_vm_compat_frontends::compile_options())
            .is_ok(),
        "normal compile options and normal CLI services must be the same exact catalog"
    );
    assert!(
        services
            .compile_options(pd_vm_compat_frontends::javascript_es5_cli_compile_options())
            .is_ok(),
        "the existing JavaScript embedding helper shares the same catalog"
    );
    let empty = std::sync::Arc::new(vm::HostApiCatalog::builder().build().unwrap());
    let error = services
        .compile_options(pd_vm_compat_frontends::compile_options_with_catalog(empty))
        .expect_err("an explicit conflicting compiler catalog is never silently replaced");
    assert_eq!(
        error.to_string(),
        "CLI compiler catalog conflicts with host services"
    );
}

#[test]
fn normal_default_library_options_execute_heap_js_with_public_services() {
    eprintln!("DEFAULT_ENTRY_LIMITS {}", stack_limit_summary());
    let source = std::fs::read_to_string(fixture("default_entry_heap.js")).unwrap();
    let issued = vm::compile_source_with_flavor_and_options_with_capture_certification(
        &source,
        pd_vm_compat_frontends::javascript_es5_flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("public default options compile JavaScript in memory");
    let mut machine = issued.into_vm().expect("certified source");
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let mut registry = vm::HostFunctionRegistry::new();
    for module in [
        pd_vm_compat_frontends::runtime::array::typescript_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_owned_array_host_module(),
        pd_vm_compat_frontends::runtime::array::typescript_array_snapshot_host_module(),
        pd_vm_compat_frontends::runtime::scalar_math::typescript_scalar_math_host_module(),
        pd_vm_compat_frontends::runtime::scalar_error::typescript_error_host_module(),
    ] {
        module
            .install_from_catalog(&mut registry, &catalog)
            .expect("the public default catalog declares every accepted descriptor");
    }
    registry.bind_vm_cached(&mut machine).expect("exact bind");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("per-VM owned-array initialization");
    assert!(machine.capture_certification_stats().unwrap().valid);
    machine.set_fuel(10_000_000);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    let result = machine.stack().last().expect("completion value");
    assert_eq!(field(result, "tag"), Value::Int(2));
    assert_eq!(field(result, "flag"), Value::Bool(true));
    assert_eq!(machine.aot_exec_count(), 0);
    assert_eq!(machine.jit_native_exec_count(), 0);
    assert_eq!(machine.host_context().resource_count(), 1);
    machine.reset_for_reuse().unwrap();
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).is_none()
    );
}

#[test]
fn owned_backend_initialization_for_ordinary_ts_allocates_no_realm_or_resource() {
    eprintln!("DEFAULT_ENTRY_LIMITS {}", stack_limit_summary());
    let issued = vm::compile_source_with_flavor_and_options_with_capture_certification(
        "const n: number = 42; n;",
        pd_vm_compat_frontends::typescript_flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("ordinary TypeScript compiles with the shared default options");
    let mut machine = issued.into_vm().expect("certified TypeScript source");
    pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
        &mut machine,
        pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
    )
    .expect("the shared default init is safe for non-JavaScript programs");
    assert_eq!(machine.host_context().resource_count(), 0);
    assert!(
        pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine).is_none(),
        "initialization must not create a JavaScript realm or owned-array payload"
    );
    machine.set_fuel(10_000_000);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.stack().last(), Some(&Value::Int(42)));
    assert_eq!(machine.host_context().resource_count(), 0);
}
