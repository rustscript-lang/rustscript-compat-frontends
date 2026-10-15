use vm::{SourcePosition, TypeSchema, Value, Vm};

fn checked_program(source: &str) -> vm::CompiledProgram {
    let hints = pd_vm_compat_frontends::collect_typescript_type_hints(source)
        .expect("arrow locals should pass the public typehint pipeline");
    let model = vm::analyze_source_from_string_with_options(
        "main.ts",
        source,
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("arrow locals should pass public analysis");
    for hint in hints
        .iter()
        .filter(|hint| hint.schema == Some(TypeSchema::Number))
    {
        if let Some(span) = hint.span {
            assert_eq!(
                model.inferred_schema_at(SourcePosition::new(0, span.lo)),
                Some(TypeSchema::Number),
                "analysis and hints must agree for {}",
                hint.name
            );
        }
    }
    let compiled = vm::compile_source_with_flavor_and_options(
        source,
        pd_vm_compat_frontends::typescript_flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("locals must not become unavailable creation-time captures");
    for prototype in &compiled.program.callable_prototypes {
        assert_eq!(
            prototype.capture_slots.len(),
            prototype.capture_source_slots.len()
        );
        assert_eq!(prototype.capture_slots.len(), prototype.capture_modes.len());
    }
    compiled
}

fn execute(source: &str, answer: i64) {
    let compiled = checked_program(source);
    eprintln!(
        "arrow prototypes={:?}",
        compiled.program.callable_prototypes
    );
    let mut machine = Vm::new(compiled.program);
    let outcome = machine.run();
    if outcome.is_err() {
        eprintln!(
            "arrow failure ip={} locals={:?} stack={:?} frames={:?}",
            machine.ip(),
            machine.locals(),
            machine.stack(),
            machine.execution_frames()
        );
    }
    outcome.expect("checked arrow locals should execute");
    assert_eq!(machine.stack().last(), Some(&Value::Int(answer)));
}

#[test]
fn arrow_body_function_value_preserves_recursive_callable() {
    execute(
        r#"
function factorial(n: number): number { return n === 0 ? 1 : n * factorial(n - 1); }
function main(): number {
    const f: (n: number) => number = (n: number): number => {
        const callback: (n: number) => number = factorial;
        const answer: number = callback(n);
        return answer;
    };
    return f(4);
}
main();
"#,
        24,
    );
}

#[test]
fn arrow_block_tail_reads_its_body_local() {
    let source = "function main(): number {const f:(n:number)=>number=(n:number):number=>{const x=n+1;return x;};return f(1);} main();";
    let compiled = checked_program(source);
    assert!(
        compiled
            .program
            .callable_prototypes
            .iter()
            .all(|p| p.capture_slots.is_empty())
    );
    execute(source, 2);
}

#[test]
fn nested_arrow_owns_its_parameter_and_body_local() {
    let source = r#"
function main(): number {
    const outer: (n: number) => number = (n: number): number => {
        const parent = n + 1;
        const inner: (k: number) => number = (k: number): number => {
            const child = k + parent;
            return child;
        };
        return inner(3);
    };
    return outer(1);
}
main();
"#;
    let compiled = checked_program(source);
    let capture_counts = compiled
        .program
        .callable_prototypes
        .iter()
        .map(|p| p.capture_slots.len())
        .collect::<Vec<_>>();
    assert_eq!(capture_counts.iter().filter(|n| **n == 1).count(), 1);
    assert!(capture_counts.iter().all(|n| *n <= 1));
    execute(source, 5);
}

#[test]
fn arrow_block_shadowing_restores_outer_bindings() {
    execute(
        r#"
function main(): number {
    const x: number = 10;
    const f: (n: number) => number = (n: number): number => {
        const x: number = n + 1;
        { const x: number = 100; x; }
        const g: (k: number) => number = (k: number): number => {
            const x: number = k + 3;
            return x;
        };
        return x + g(1);
    };
    return x + f(1);
}
main();
"#,
        16,
    );
}

#[test]
fn nested_arrows_preserve_external_and_parent_mutable_aliases() {
    execute(
        r#"
function main(): number {
    let external: number = 10;
    const outer: (n: number) => number = (n: number): number => {
        let parent: number = n;
        const inner: (k: number) => number = (k: number): number => {
            parent = parent + k;
            return parent + external;
        };
        const first: number = inner(2);
        return first + inner(3) + parent;
    };
    const result: number = outer(1);
    return result + external;
}
main();
"#,
        45,
    );
}

#[test]
fn nested_arrow_reserved_self_value_belongs_to_parent() {
    execute(
        r#"
function main(): number {
    const outer: (n: number) => number = (n: number): number => {
        const recursive: (k: number) => number = (k: number): number => {
            const next: number = k - 1;
            next;
            return recursive === recursive ? 1 : 0;
        };
        return recursive(3);
    };
    return outer(2);
}
main();
"#,
        1,
    );
}

#[test]
fn nested_arrow_hidden_numeric_temporaries_remain_body_owned() {
    execute(
        r#"
function main(): number {
    const outer: (n: number) => number = (n: number): number => {
        const inner: (k: number) => number = (k: number): number => {
            Math.round(2.4);
            Number.isInteger(k);
            const child: number = k + 1;
            return child;
        };
        return inner(2);
    };
    return outer(1);
}
main();
"#,
        3,
    );
}
