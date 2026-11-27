use vm::{
    Value, Vm, VmStatus, compile_source_with_flavor_and_options,
    compiler::{FrontendId, SourceFlavor},
};

fn flavor() -> SourceFlavor {
    SourceFlavor::Plugin(FrontendId::new("typescript"))
}

#[test]
fn public_type_hints_match_executable_schema_and_source_bytes() {
    let source = "const amount: number = 42;\namount;\n";
    let hints = pd_vm_compat_frontends::collect_typescript_type_hints(source)
        .expect("typed hint collection should use the normal compiler pipeline");
    let hint = hints
        .iter()
        .find(|hint| hint.name == "amount")
        .expect("amount hint should be present");
    assert_eq!(hint.schema, Some(vm::TypeSchema::Number));
    assert_eq!(hint.inferred_type, "number");
    let start = source.find("amount").expect("amount identifier");
    assert_eq!(
        hint.span,
        Some(vm::compiler::source_map::Span::new(
            0,
            start,
            start + "amount".len(),
        ))
    );
}

#[test]
fn annotated_string_declaration_is_checked_and_executes() {
    let source = "const name: string = \"Ada\";\nname;\n";
    let compiled = compile_source_with_flavor_and_options(
        source,
        flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("valid annotated TypeScript should compile");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::string("Ada")]);
}

#[test]
fn incompatible_annotation_is_rejected_before_execution() {
    let error = match compile_source_with_flavor_and_options(
        "const name: string = 7;\nname;\n",
        flavor(),
        pd_vm_compat_frontends::compile_options(),
    ) {
        Ok(_) => panic!("string annotation must reject an integer initializer"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("type"));
}

fn ts_record_get_native(source: &str) -> Value {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!("TS_RECORD_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                &source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "typed record compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_RECORD_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_RECORD_GUEST_BEGIN");
            assert_eq!(machine.run().unwrap(), VmStatus::Halted);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack(), &[Value::Int(21)]);
            eprintln!(
                "TS_RECORD_RESULT stack={:?} native=0 aot=0",
                machine.stack()
            );
            let result = machine.stack().last().expect("completion").clone();
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_RECORD_RESET resource_count=0 guest_invocations=1");
            result
        })
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn typed_record_static_get_key_capture_control() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function readRow(): number { let reads: number = 0; const row: Row = { left: 1, right: 2 }; const key: () => string = (): string => { reads = reads + 1; return "right"; }; const selected: string = key(); const picked = row.right; return selected === "right" && picked === 2 && reads === 1 ? 21 : 0; } readRow();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn typed_record_computed_string_get_key_once() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function readRow(): number { let reads: number = 0; const row: Row = { left: 1, right: 2 }; const key: () => string = (): string => { reads = reads + 1; return "right"; }; const picked = row[key()]; return picked === 2 && reads === 1 ? 21 : 0; } readRow();"#,
    );
    assert_eq!(value, Value::Int(21));
}

fn ts_record_get_refused(source: &str, member: &str) {
    let source = source.to_owned();
    let member = member.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!("TS_RECORD_REFUSAL_SOURCE {source}");
            let error = match compile_source_with_flavor_and_options(
                &source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            ) {
                Ok(_) => panic!(
                    "computed record access must stay refused; guest_invocations=0 source={source:?}"
                ),
                Err(error) => error,
            };
            eprintln!("TS_RECORD_REFUSAL_DIAG {error:?}");
            assert!(
                error
                    .to_string()
                    .contains("unsupported TypeScript computed property access"),
                "unexpected refusal diagnostic: {error:?}"
            );
            let start = source.find(member.as_str()).expect("computed member expression");
            let end = start + member.len();
            assert!(
                format!("{error:?}").contains(&format!("lo: {start}, hi: {end}")),
                "refusal span mismatch: {error:?}"
            );
            eprintln!("TS_RECORD_REFUSAL_END guest_invocations=0");
        })
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn plain_array_computed_get_preserves_scalar_value_proof() {
    let value = ts_record_get_native(
        r#"function readArray(): number { const values: number[] = [1, 21]; const index = 1; const picked: number = values[index]; return picked; } readArray();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn generic_heterogeneous_record_alias_computed_string_get() {
    let value = ts_record_get_native(
        r#"type Row<T> = { left: T; right: string; }; type NamedRow = Row<number>; function readRow(): number { const row: NamedRow = { left: 21, right: "other" }; const key: string = "left"; return row[key] === 21 ? 21 : 0; } readRow();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn typed_record_computed_numeric_key_remains_refused() {
    ts_record_get_refused(
        r#"interface Row { left: number; right: number; } const row: Row = { left: 1, right: 2 }; const key: number = 0; row[key];"#,
        "row[key]",
    );
}

#[test]
fn optional_record_computed_string_key_remains_refused() {
    ts_record_get_refused(
        r#"interface Row { left: number; right: number; } type MaybeRow = Row | null; const maybe: MaybeRow = { left: 1, right: 2 }; const key: string = "right"; maybe[key];"#,
        "maybe[key]",
    );
}

#[test]
fn native_shared_scalar_cell_survives_alias_parameter_return() {
    let value = ts_record_get_native(
        r#"interface Accessors { write: (value: number) => number; read: () => number; } function makeAccessors(initial: number): Accessors { let state: number = initial; const write: (value: number) => number = (value: number): number => { state = value; return state; }; const read: () => number = (): number => state; return { write: write, read: read }; } function relay(accessors: Accessors): Accessors { return accessors; } function checkCells(): number { let active: Accessors = relay(makeAccessors(1)); const alias: Accessors = active; const prepared: Accessors = active; active = makeAccessors(100); const write: (value: number) => number = prepared.write; const read: () => number = alias.read; const replacement: () => number = active.read; const written: number = write(9); return written === 9 && read() === 9 && replacement() === 100 ? 21 : 0; } checkCells();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn native_shared_cell_publishes_cow_array_write_to_alias() {
    let value = ts_record_get_native(
        r#"interface Accessors { write: (value: number) => number; read: () => number; } function makeAccessors(initial: number): Accessors { let state: number[] = [initial]; const write: (value: number) => number = (value: number): number => { state[0] = value; return state[0]; }; const read: () => number = (): number => state[0]; return { write: write, read: read }; } function relay(accessors: Accessors): Accessors { return accessors; } function checkCells(): number { let active: Accessors = relay(makeAccessors(1)); const alias: Accessors = active; const prepared: Accessors = active; active = makeAccessors(100); const write: (value: number) => number = prepared.write; const read: () => number = alias.read; const replacement: () => number = active.read; const written: number = write(9); return written === 9 && read() === 9 && replacement() === 100 ? 21 : 0; } checkCells();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn native_map_cell_publishes_cow_writeback_and_preserves_snapshot() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(|| {
            use vm::{AssignmentKind, ClosureExpr, Compiler, Expr, Stmt, TypeSchema};
            fn builtin(name: &str, arguments: Vec<Expr>) -> Expr {
                Expr::Call(
                    vm::builtin_call_index(name).expect("existing map builtin"),
                    Vec::new(),
                    arguments,
                    None,
                    None,
                )
            }
            fn get(container: Expr) -> Expr {
                builtin("get", vec![container, Expr::String("value".into())])
            }
            fn callable(params: Vec<TypeSchema>) -> TypeSchema {
                TypeSchema::Callable { params, result: Box::new(TypeSchema::Number) }
            }
            let writer = ClosureExpr {
                param_slots: vec![10],
                capture_copies: vec![(0, 8)],
                body: Box::new(Expr::Block {
                    stmts: vec![Stmt::Assign {
                        kind: AssignmentKind::Set,
                        index: 8,
                        expr: builtin("set", vec![Expr::Var(8), Expr::String("value".into()), Expr::Var(10)]),
                        line: 1,
                    }],
                    expr: Box::new(get(Expr::Var(8))),
                }),
            };
            let reader = ClosureExpr {
                param_slots: Vec::new(),
                capture_copies: vec![(0, 9)],
                body: Box::new(get(Expr::Borrow(Box::new(Expr::Var(9))))),
            };
            let statements = vec![
                Stmt::Let {
                    index: 0,
                    declared_schema: Some(TypeSchema::Map(Box::new(TypeSchema::Number))),
                    expr: builtin("set", vec![builtin("map_new", Vec::new()), Expr::String("value".into()), Expr::Int(1)]),
                    line: 1,
                },
                Stmt::Let { index: 1, declared_schema: Some(callable(vec![TypeSchema::Number])), expr: Expr::Closure(writer), line: 1 },
                Stmt::Let { index: 2, declared_schema: Some(callable(Vec::new())), expr: Expr::Closure(reader), line: 1 },
                Stmt::Let { index: 3, declared_schema: Some(TypeSchema::Map(Box::new(TypeSchema::Number))), expr: Expr::Var(0), line: 1 },
                Stmt::Let { index: 4, declared_schema: Some(TypeSchema::Number), expr: Expr::LocalCall(1, Vec::new(), vec![Expr::Int(9)], None), line: 1 },
                Stmt::Let { index: 5, declared_schema: Some(TypeSchema::Number), expr: Expr::LocalCall(2, Vec::new(), Vec::new(), None), line: 1 },
                Stmt::Let { index: 6, declared_schema: None, expr: get(Expr::Var(3)), line: 1 },
                Stmt::Expr { expr: Expr::Var(4), line: 1 },
                Stmt::Expr { expr: Expr::Var(5), line: 1 },
                Stmt::Expr { expr: Expr::Var(6), line: 1 },
            ];
            let mut compiler = Compiler::new();
            compiler.set_root_local_count(12);
            eprintln!("TS_MAP_CELL_PUREIR_COMPILE_BEGIN public_source_invocations=0");
            let program = compiler.compile_program(&statements).expect("synthetic Map/cell IR");
            let strict = program.type_map.as_ref().is_some_and(|types| types.strict_types);
            assert!(!strict, "direct codegen is not a strict-typing proof");
            eprintln!("TS_MAP_CELL_PUREIR_COMPILE_OK strict_types={strict}");
            let mut machine = Vm::new(program);
            machine.set_jit_config(vm::JitConfig { enabled: false, ..*machine.jit_config() });
            machine.set_fuel(10_000_000);
            eprintln!("TS_MAP_CELL_PUREIR_GUEST_BEGIN");
            let outcome = machine.run();
            let snapshot = machine.stack().to_vec();
            let counts = (machine.jit_native_exec_count(), machine.aot_exec_count());
            eprintln!("TS_MAP_CELL_PUREIR_OBSERVED outcome={outcome:?} stack={snapshot:?} native={} aot={}", counts.0, counts.1);
            machine.reset_for_reuse().expect("reset after Map/cell proof");
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_MAP_CELL_PUREIR_RESET resource_count=0 guest_invocations=1");
            assert_eq!(outcome.expect("Map/cell runtime"), VmStatus::Halted);
            assert_eq!(snapshot, vec![Value::Int(9), Value::Int(9), Value::Int(1)]);
            assert_eq!(counts, (0, 0));
        })
        .unwrap()
        .join()
        .unwrap();
}

fn rss_strict_map_source_probe(source: &str, expect_refusal: bool) {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!("RSS_STRICT_MAP_SOURCE {source:?}");
            let result = vm::compile_source_with_flavor_and_options(
                &source,
                vm::SourceFlavor::RustScript,
                vm::CompileSourceFileOptions::new(),
            );
            let compiled = match result {
                Err(error) => {
                    eprintln!("RSS_STRICT_MAP_COMPILE_REFUSAL {error:?}");
                    assert!(
                        expect_refusal,
                        "positive strict Map source was refused: {error:?}"
                    );
                    let source_error = match &error {
                        vm::SourcePathError::Source(error)
                        | vm::SourcePathError::SourceWithMap { error, .. } => error,
                        other => panic!("wrong refusal layer: {other:?}"),
                    };
                    let vm::SourceError::Compile(vm::CompileError::InvalidFieldAccess {
                        line,
                        detail,
                        span,
                        ..
                    }) = source_error
                    else {
                        panic!("wrong element-store refusal category: {source_error:?}");
                    };
                    let fragment = "state[\"value\"] = \"wrong\";";
                    let start = source
                        .find(fragment)
                        .expect("adversarial element-store statement");
                    let expected_line = source[..start]
                        .bytes()
                        .filter(|byte| *byte == b'\n')
                        .count()
                        + 1;
                    assert_eq!(
                        *line,
                        Some(u32::try_from(expected_line).expect("source line fits u32"))
                    );
                    let detail = detail.to_ascii_lowercase();
                    assert!(
                        detail.contains("number") && detail.contains("string"),
                        "wrong type-proof diagnostic: {detail}"
                    );
                    assert!(
                        span.is_some(),
                        "element-store refusal must carry a source span"
                    );
                    eprintln!("RSS_STRICT_MAP_REFUSAL_END guest_invocations=0");
                    return;
                }
                Ok(compiled) => compiled,
            };
            let types = compiled
                .program
                .type_map
                .as_ref()
                .expect("source compiler type map");
            eprintln!(
                "RSS_STRICT_MAP_COMPILE_OK strict_types={} local_schemas={:?}",
                types.strict_types, types.local_schemas
            );
            assert!(types.strict_types, "RSS source must run strict typing");
            assert!(
                !expect_refusal,
                "strict map<number> accepted a String element store"
            );
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("RSS_STRICT_MAP_GUEST_BEGIN");
            let outcome = machine.run();
            let snapshot = machine.stack().to_vec();
            let counts = (machine.jit_native_exec_count(), machine.aot_exec_count());
            eprintln!(
                "RSS_STRICT_MAP_OBSERVED outcome={outcome:?} stack={snapshot:?} native={} aot={}",
                counts.0, counts.1
            );
            machine.reset_for_reuse().expect("reset RSS Map probe");
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("RSS_STRICT_MAP_RESET resource_count=0 guest_invocations=1");
            assert_eq!(outcome.expect("strict Map guest"), VmStatus::Halted);
            assert_eq!(snapshot, vec![Value::Int(9), Value::Int(1)]);
            assert_eq!(counts, (0, 0));
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn rss_strict_map_number_store_preserves_explicit_snapshot() {
    rss_strict_map_source_probe(
        "let mut state: map<number> = {value: 1};\nlet snapshot: map<number> = state.copy();\nstate[\"value\"] = 9;\nstate[\"value\"];\nsnapshot[\"value\"];\n",
        false,
    );
}

#[test]
fn rss_strict_map_number_rejects_string_element_store() {
    rss_strict_map_source_probe(
        "let mut state: map<number> = {value: 1};\nlet snapshot: map<number> = state.copy();\nstate[\"value\"] = \"wrong\";\nstate[\"value\"];\nsnapshot[\"value\"];\n",
        true,
    );
}

fn rss_scalar_map_control(source: &str, verify_stack: fn(&[Value])) {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!("RSS_MAP_CONTROL_SOURCE {source:?}");
            let compiled = vm::compile_source_with_flavor_and_options(
                &source,
                vm::SourceFlavor::RustScript,
                vm::CompileSourceFileOptions::new(),
            )
            .expect("scalar Map preservation control must compile");
            assert!(
                compiled
                    .program
                    .type_map
                    .as_ref()
                    .is_some_and(|types| types.strict_types)
            );
            eprintln!("RSS_MAP_CONTROL_COMPILE_OK strict_types=true");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("RSS_MAP_CONTROL_GUEST_BEGIN");
            let outcome = machine.run();
            let snapshot = machine.stack().to_vec();
            let counts = (machine.jit_native_exec_count(), machine.aot_exec_count());
            eprintln!(
                "RSS_MAP_CONTROL_OBSERVED outcome={outcome:?} stack={snapshot:?} native={} aot={}",
                counts.0, counts.1
            );
            machine
                .reset_for_reuse()
                .expect("reset preservation control");
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("RSS_MAP_CONTROL_RESET resource_count=0 guest_invocations=1");
            assert_eq!(outcome.expect("preservation guest"), VmStatus::Halted);
            assert_eq!(counts, (0, 0));
            verify_stack(&snapshot);
            eprintln!("RSS_MAP_CONTROL_ASSERTED");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn rss_scalar_map_number_accepts_float_store() {
    rss_scalar_map_control(
        "let mut state: map<number> = {value: 1};\nlet snapshot: map<number> = state.copy();\nstate[\"value\"] = 2.5;\nstate[\"value\"];\nsnapshot[\"value\"];\n",
        |stack| {
            assert_eq!(stack, &[Value::Float(2.5), Value::Int(1)]);
        },
    );
}

#[test]
fn rss_scalar_map_null_store_preserves_deletion() {
    rss_scalar_map_control(
        "let mut state: map<number> = {value: 1};\nstate[\"value\"] = null;\nstate;\n",
        |stack| {
            assert!(matches!(stack, [Value::Map(entries)] if entries.is_empty()));
        },
    );
}

#[test]
fn rss_scalar_map_guard_preserves_array_store() {
    rss_scalar_map_control(
        "let mut values: array<int> = [1];\nvalues[0] = 9;\nvalues[0];\n",
        |stack| {
            assert_eq!(stack, &[Value::Int(9)]);
        },
    );
}

#[test]
fn rss_scalar_map_guard_preserves_object_store() {
    rss_scalar_map_control(
        "let mut object = {value: 1};\nobject[\"value\"] = 9;\nobject[\"value\"];\n",
        |stack| {
            assert_eq!(stack, &[Value::Int(9)]);
        },
    );
}

#[test]
fn prepared_number_record_alias_read_survives_rebind() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function readPrepared(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; const key: () => string = (): string => { active = { left: 100, right: 200 }; return "right"; }; const before: number = alias.right; const picked = alias[key()]; return before === 2 && picked === 2 && active.right === 200 && prepared.right === 2 ? 21 : 0; } readPrepared();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_computed_put_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function rhs9(): number { return 9; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; let order: number = 0; let keyCalls: number = 0; let rhsCalls: number = 0; const key = (): string => { keyCalls = keyCalls + 1; active = { left: 100, right: 200 }; order = order * 10 + 1; return "right"; }; const rhs = (): number => { rhsCalls = rhsCalls + 1; order = order * 10 + 2; return 9; }; active[key()] = rhs(); return alias.right === 9 && prepared.right === 9 && active.right === 200 && keyCalls === 1 && rhsCalls === 1 && order === 12 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn relay_returned_number_record_dot_read() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function readPrepared(): number { const active: Row = relay({ left: 1, right: 2 }); return active.right === 2 ? 21 : 0; } readPrepared();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn relay_returned_number_record_alias_dot_read() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function readPrepared(): number { const active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; return alias.right === 2 && prepared.right === 2 && active.right === 2 ? 21 : 0; } readPrepared();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn relay_returned_number_record_rebind_preserves_alias_dot() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function readPrepared(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; const key: () => string = (): string => { active = { left: 100, right: 200 }; return "right"; }; const before: number = alias.right; const called: string = key(); return before === 2 && called === "right" && active.right === 100 && prepared.right === 2 && alias.right === 2 ? 21 : 0; } readPrepared();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn relay_returned_number_record_rebind_discriminator() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function readPrepared(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; const key: () => string = (): string => { active = { left: 100, right: 200 }; return "right"; }; const before: number = alias.right; const called: string = key(); const afterActive: number = active.right; const afterPrepared: number = prepared.right; const afterAlias: number = alias.right; return before === 2 ? (called === "right" ? (afterActive === 100 ? (afterPrepared === 2 ? (afterAlias === 2 ? 21 : 5) : 4) : 3) : 2) : 1; } readPrepared();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn relay_record_closure_rebind_then_active_dot() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function readPrepared(): number { let active: Row = relay({ left: 1, right: 2 }); const key: () => string = (): string => { active = { left: 100, right: 200 }; return "right"; }; const called: string = key(); const after: number = active.right; return called === "right" ? 1000 + after : 2; } readPrepared();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn relay_record_closure_rebind_then_prepared_dot() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function readPrepared(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; const key: () => string = (): string => { active = { left: 100, right: 200 }; return "right"; }; const called: string = key(); const seen: number = prepared.right; return called === "right" ? 1000 + seen : 2; } readPrepared();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn relay_record_closure_rebind_then_alias_dot() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function readPrepared(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; const key: () => string = (): string => { active = { left: 100, right: 200 }; return "right"; }; const called: string = key(); const seen: number = alias.right; return called === "right" ? 1000 + seen : 2; } readPrepared();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn relay_record_closure_rebind_then_active_dot_with_aliases() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function readPrepared(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; const key: () => string = (): string => { active = { left: 100, right: 200 }; return "right"; }; const called: string = key(); const seen: number = active.right; return called === "right" ? 1000 + seen : 2; } readPrepared();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn relay_record_alias_dot_before_and_after_rebind() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function readPrepared(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; const key: () => string = (): string => { active = { left: 100, right: 200 }; return "right"; }; const before: number = alias.right; const called: string = key(); const seen: number = alias.right; return called === "right" && before === 2 ? 1000 + seen : 2; } readPrepared();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn relay_record_dummy_local_then_alias_dot_after_rebind() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function readPrepared(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; const key: () => string = (): string => { active = { left: 100, right: 200 }; return "right"; }; const before: number = 2; const called: string = key(); const seen: number = alias.right; return called === "right" && before === 2 ? 1000 + seen : 2; } readPrepared();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn relay_record_alias_left_before_right_after_rebind() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function readPrepared(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; const key: () => string = (): string => { active = { left: 100, right: 200 }; return "right"; }; const before: number = alias.left; const called: string = key(); const seen: number = alias.right; return called === "right" && before === 1 ? 1000 + seen : 2; } readPrepared();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn relay_record_alias_right_twice_without_rebind() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function readPrepared(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const before: number = alias.right; const seen: number = alias.right; return before === 2 && seen === 2 ? 21 : 0; } readPrepared();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn relay_record_alias_right_before_left_after_rebind() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function readPrepared(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; const key: () => string = (): string => { active = { left: 100, right: 200 }; return "right"; }; const before: number = alias.right; const called: string = key(); const seen: number = alias.left; return called === "right" && before === 2 ? 1000 + seen : 2; } readPrepared();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn relay_record_same_key_get_disassembly() {
    let source = r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function readPrepared(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const before: number = alias.right; const seen: number = alias.right; return before === 2 ? 1000 + seen : 2; } readPrepared();"#;
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .expect("same-key fixture should compile");
            eprintln!("{}", vm::disassemble_program(&compiled.program));
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn relay_record_computed_string_key_twice() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function readPrepared(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const key: string = "right"; const before = alias[key]; const seen = alias[key]; return before === 2 ? 1000 + seen : 2; } readPrepared();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn exported_record_return_refuses_carrier() {
    let source = r#"interface Row { left: number; right: number; } export function leak(): Row { let active: Row = { left: 1, right: 2 }; return active; }"#;
    let error = match compile_source_with_flavor_and_options(
        source,
        flavor(),
        pd_vm_compat_frontends::compile_options(),
    ) {
        Ok(_) => panic!("exported carrier return must be refused; guest_invocations=0"),
        Err(error) => error,
    };
    let text = format!("{error:?}");
    assert!(
        text.contains("record carrier return value"),
        "guest_invocations=0 diagnostic={text}"
    );
}

#[test]
fn carrier_callback_ident_refuses_host_boundary() {
    let source = r#"interface Row { left: number; right: number; } function cb(): Row { let active: Row = { left: 1, right: 2 }; return active; } function take(fn: () => boolean): number { return 1; } take(cb);"#;
    let error = match compile_source_with_flavor_and_options(
        source,
        flavor(),
        pd_vm_compat_frontends::compile_options(),
    ) {
        Ok(_) => panic!("carrier callback identifier must be refused; guest_invocations=0"),
        Err(error) => error,
    };
    let text = format!("{error:?}");
    assert!(
        text.contains("record carrier argument beyond a guest row boundary"),
        "guest_invocations=0 diagnostic={text}"
    );
}

#[test]
fn prepared_number_record_dot_put_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; active.right = 9; return alias.right === 9 && prepared.right === 9 && active.right === 9 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_dot_add_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; active.right += 7; return alias.right === 9 && prepared.right === 9 && active.right === 9 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_computed_add_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; active["right"] += 7; return alias.right === 9 && prepared.right === 9 && active.right === 9 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_postfix_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; return active.right++ === 2 && alias.right === 3 && prepared.right === 3 && active.right === 3 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_prefix_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; return ++active.right === 3 && alias.right === 3 && prepared.right === 3 && active.right === 3 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_postfix_minus_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; return active.right-- === 2 && alias.right === 1 && prepared.right === 1 && active.right === 1 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_prefix_minus_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; return --active.right === 1 && alias.right === 1 && prepared.right === 1 && active.right === 1 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_stmt_postfix_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; active.right++; return alias.right === 3 && prepared.right === 3 && active.right === 3 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_computed_postfix_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; return active["right"]++ === 2 && alias.right === 3 && prepared.right === 3 && active.right === 3 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_computed_prefix_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; return ++active["right"] === 3 && alias.right === 3 && prepared.right === 3 && active.right === 3 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_computed_postfix_minus_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; return active["right"]-- === 2 && alias.right === 1 && prepared.right === 1 && active.right === 1 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_computed_prefix_minus_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; return --active["right"] === 1 && alias.right === 1 && prepared.right === 1 && active.right === 1 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_dot_sub_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; active.right -= 1; return alias.right === 1 && prepared.right === 1 && active.right === 1 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_computed_sub_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; active["right"] -= 1; return alias.right === 1 && prepared.right === 1 && active.right === 1 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_dot_mul_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; active.right *= 3; return alias.right === 6 && prepared.right === 6 && active.right === 6 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_computed_mul_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; active["right"] *= 3; return alias.right === 6 && prepared.right === 6 && active.right === 6 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_dot_div_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; active.right /= 2; return alias.right === 1 && prepared.right === 1 && active.right === 1 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_computed_div_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; active["right"] /= 2; return alias.right === 1 && prepared.right === 1 && active.right === 1 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_dot_rem_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; active.right %= 2; return alias.right === 0 && prepared.right === 0 && active.right === 0 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_computed_rem_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; active["right"] %= 2; return alias.right === 0 && prepared.right === 0 && active.right === 0 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_dot_assign_expr_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; return (active.right = 9) === 9 && alias.right === 9 && prepared.right === 9 && active.right === 9 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_computed_assign_expr_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; return (active["right"] = 9) === 9 && alias.right === 9 && prepared.right === 9 && active.right === 9 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_dot_add_expr_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; return (active.right += 7) === 9 && alias.right === 9 && prepared.right === 9 && active.right === 9 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_computed_add_expr_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const prepared: Row = active; return (active["right"] += 7) === 9 && alias.right === 9 && prepared.right === 9 && active.right === 9 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_computed_add_expr_refuses_non_numeric_rhs() {
    let source = r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const extra: string = "no"; return (active["right"] += extra) === 9 ? 21 : 0; } check();"#;
    let compiled = compile_source_with_flavor_and_options(
        source,
        flavor(),
        pd_vm_compat_frontends::compile_options(),
    );
    let Err(error) = compiled else {
        panic!("non-numeric computed add expression must stay refused");
    };
    let rendered = format!("{error:?}");
    assert!(
        rendered.contains("unsupported TypeScript expression"),
        "{rendered}"
    );
    assert!(!rendered.contains("guest_invocations=1"), "{rendered}");
}

#[test]
fn prepared_number_record_dot_sub_expr_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 16 }); const alias: Row = active; const prepared: Row = active; return (active.right -= 7) === 9 && alias.right === 9 && prepared.right === 9 && active.right === 9 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_computed_sub_expr_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 16 }); const alias: Row = active; const prepared: Row = active; return (active["right"] -= 7) === 9 && alias.right === 9 && prepared.right === 9 && active.right === 9 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_dot_mul_expr_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 3 }); const alias: Row = active; const prepared: Row = active; return (active.right *= 3) === 9 && alias.right === 9 && prepared.right === 9 && active.right === 9 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_computed_mul_expr_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 3 }); const alias: Row = active; const prepared: Row = active; return (active["right"] *= 3) === 9 && alias.right === 9 && prepared.right === 9 && active.right === 9 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_dot_div_expr_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 18 }); const alias: Row = active; const prepared: Row = active; return (active.right /= 2) === 9 && alias.right === 9 && prepared.right === 9 && active.right === 9 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_computed_div_expr_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 18 }); const alias: Row = active; const prepared: Row = active; return (active["right"] /= 2) === 9 && alias.right === 9 && prepared.right === 9 && active.right === 9 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_dot_rem_expr_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 23 }); const alias: Row = active; const prepared: Row = active; return (active.right %= 14) === 9 && alias.right === 9 && prepared.right === 9 && active.right === 9 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn prepared_number_record_computed_rem_expr_updates_alias() {
    let value = ts_record_get_native(
        r#"interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 23 }); const alias: Row = active; const prepared: Row = active; return (active["right"] %= 14) === 9 && alias.right === 9 && prepared.right === 9 && active.right === 9 ? 21 : 0; } check();"#,
    );
    assert_eq!(value, Value::Int(21));
}

#[test]
fn native_typescript_exponentiation_binary_public_value() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "const value = 2 ** 8;\nvalue;\n";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native exponentiation compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native exponentiation VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float exponentiation completion, got {:?}",
                    machine.stack()
                );
            };
            assert_eq!(number.to_bits(), 256.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Binary `**` is right associative, so `2 ** 3 ** 2` is 2 ** 9, exactly 512.
#[test]
fn native_typescript_exponentiation_right_associative() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "const value = 2 ** 3 ** 2;\nvalue;\n";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native exponentiation compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native exponentiation VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float exponentiation completion, got {:?}",
                    machine.stack()
                )
            };
            assert_eq!(number.to_bits(), 512.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// A NaN exponent keeps the exact Float box and stays NaN; only the NaN class is pinned.
#[test]
fn native_typescript_exponentiation_nan_exponent() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "const value = 1 ** ((-0) / (-0));\nvalue;\n";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native exponentiation compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native exponentiation VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float exponentiation completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_nan());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// A negative-infinite exponent on the unit negative base is NaN in the reference engine.
#[test]
fn native_typescript_exponentiation_unit_negative_infinite() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "const value = (-1) ** (1 / (-0));\nvalue;\n";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native exponentiation compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native exponentiation VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float exponentiation completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_nan());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// A NaN base with a zero exponent is exactly 1.0, not NaN.
#[test]
fn native_typescript_exponentiation_nan_zero() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "const value = ((-0) / (-0)) ** 0;\nvalue;\n";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native exponentiation compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native exponentiation VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float exponentiation completion, got {:?}",
                    machine.stack()
                )
            };
            assert_eq!(number.to_bits(), 1.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// A negative zero base with a positive odd exponent keeps the negative-zero sign.
#[test]
fn native_typescript_exponentiation_negative_zero() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "const value = (-0) ** 3;\nvalue;\n";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native exponentiation compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native exponentiation VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float exponentiation completion, got {:?}",
                    machine.stack()
                )
            };
            assert_eq!(number.to_bits(), (-0.0_f64).to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// The left operand is read before the RHS: 2 ** 3 with the RHS mutating x once
/// gives 8 * 100 + 3 * 10 + 1 = 831.
#[test]
fn native_typescript_exponentiation_operand_order() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function run(): number { let x: number = 2; let calls: number = 0; const rhs: () => number = (): number => { calls=calls+1; x=3; return 3; }; const result = x ** rhs(); return result*100+x*10+calls; } run();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native exponentiation compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native exponentiation VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float exponentiation completion, got {:?}",
                    machine.stack()
                )
            };
            assert_eq!(number.to_bits(), 831.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Positive local `**=` tracer: x starts at 2, so `x **= 3` leaves exactly 8.
#[test]
fn native_typescript_exponentiation_compound_local_public_value() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "let x: number = 2;\nx **= 3;\nx;\n";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native exponentiation compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native exponentiation VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float exponentiation completion, got {:?}",
                    machine.stack()
                )
            };
            assert_eq!(number.to_bits(), 8.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Positive local `**=` tracer: the right-hand side closure runs exactly once.
#[test]
fn native_local_power_rhs_once() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function run(): number { let calls: number = 0; const rhs: () => number = (): number => { calls=calls+1; return 3; }; let x: number = 2; x **= rhs(); return x*10+calls; } run();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native local power compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native local power VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float local power completion, got {:?}",
                    machine.stack()
                )
            };
            assert_eq!(number.to_bits(), 81.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Positive local `**=` tracer: the old value is captured before the right-hand side mutates it.
#[test]
fn native_local_power_captured_old_before_rhs() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function run(): number { let x: number = 2; let calls: number = 0; const rhs: () => number = (): number => { calls=calls+1; x=3; return 3; }; x **= rhs(); return x*10+calls; } run();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native local power compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native local power VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float local power completion, got {:?}",
                    machine.stack()
                )
            };
            assert_eq!(number.to_bits(), 81.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Positive local `**=` tracer: negative zero keeps its sign bit through the stored product.
#[test]
fn native_local_power_negative_zero() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "let x: number = -0;\nx **= 3;\nx;\n";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native local power compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native local power VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float local power completion, got {:?}",
                    machine.stack()
                )
            };
            assert_eq!(number.to_bits(), (-0.0_f64).to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Positive local `**=` tracer: each for-loop update qualifies the local once with the captured old value.
#[test]
fn native_local_power_for_update() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "let x: number = 2; for(let i=0; i<2; x **= 2){i+=1;} x;";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native local power compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native local power VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float local power completion, got {:?}",
                    machine.stack()
                )
            };
            assert_eq!(number.to_bits(), 16.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Positive local `**=` tracer: the for-loop init clause qualifies the local before the first test.
#[test]
fn native_local_power_for_init() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "let x: number = 2; for(x **= 3; false;){ } x;";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native local power compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native local power VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float local power completion, got {:?}",
                    machine.stack()
                )
            };
            assert_eq!(number.to_bits(), 8.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Negative local `**=` control: `**=` on a const binding is refused by the parser before any guest work.
#[test]
fn native_local_power_const_refused() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "const x: number = 2;\nx **= 3;\nx;\n";
            eprintln!("TS_POWER_SOURCE {source}");
            let error = match compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            ) {
                Ok(_) => panic!(
                    "local power source must be refused; guest_invocations=0 source={source:?}"
                ),
                Err(error) => error,
            };
            let message = error.to_string();
            eprintln!("TS_LOCAL_POWER_REFUSED {error:?} guest_invocations=0");
            assert!(
                message.contains("cannot assign to const binding 'x'"),
                "unexpected local power refusal {message:?}"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Negative local `**=` control: `**=` with a string right-hand side is refused before any guest work.
#[test]
fn native_local_power_string_rhs_refused() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "let x: number = 2;\nx **= '3';\nx;\n";
            eprintln!("TS_POWER_SOURCE {source}");
            let error = match compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            ) {
                Ok(_) => panic!(
                    "local power source must be refused; guest_invocations=0 source={source:?}"
                ),
                Err(error) => error,
            };
            let message = error.to_string();
            eprintln!("TS_LOCAL_POWER_REFUSED {error:?} guest_invocations=0");
            assert!(
                message.contains("unsupported TypeScript compound assignment operator"),
                "unexpected local power refusal {message:?}"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Negative local `**=` control: `**=` with a boolean right-hand side is refused before any guest work.
#[test]
fn native_local_power_bool_rhs_refused() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "let x: number = 2;\nx **= true;\nx;\n";
            eprintln!("TS_POWER_SOURCE {source}");
            let error = match compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            ) {
                Ok(_) => panic!(
                    "local power source must be refused; guest_invocations=0 source={source:?}"
                ),
                Err(error) => error,
            };
            let message = error.to_string();
            eprintln!("TS_LOCAL_POWER_REFUSED {error:?} guest_invocations=0");
            assert!(
                message.contains("unsupported TypeScript compound assignment operator"),
                "unexpected local power refusal {message:?}"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Negative local `**=` control: `**=` with an object literal right-hand side is refused before any guest work.
#[test]
fn native_local_power_object_rhs_refused() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "let x: number = 2;\nx **= ({a:3});\nx;\n";
            eprintln!("TS_POWER_SOURCE {source}");
            let error = match compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            ) {
                Ok(_) => panic!(
                    "local power source must be refused; guest_invocations=0 source={source:?}"
                ),
                Err(error) => error,
            };
            let message = error.to_string();
            eprintln!("TS_LOCAL_POWER_REFUSED {error:?} guest_invocations=0");
            assert!(
                message.contains("unsupported TypeScript compound assignment operator"),
                "unexpected local power refusal {message:?}"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Loop baseline: plain for-update `x+=0` with a body counter runs exactly 2 iterations
#[test]
fn native_loop_body_counter_plain_update_control() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "let x: number = 2; for(let i=0; i<2; x+=0){i+=1;} x ** 1;";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native local power compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native local power VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float plain-update loop baseline, got {:?}",
                    machine.stack()
                )
            };
            assert_eq!(number.to_bits(), 2.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Loop baseline: for-update counter with an `x **= 2` body halts exactly 2 iterations
#[test]
fn native_loop_power_body_counter_update_control() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "let x: number = 2; for(let i=0; i<2; i+=1){x **= 2;} x;";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native local power compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native local power VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float loop-body power baseline, got {:?}",
                    machine.stack()
                )
            };
            assert_eq!(number.to_bits(), 16.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Power-free control: `for`-let loop with `i += 1` updating `n` in the body; observes exactly two body increments through Bool `n===2`.
#[test]
fn native_power_free_for_let_add_counter_control() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "let n=0; for(let i=0; i<2; i+=1){n=n+1;} n===2;";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native power-free add compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native power-free add VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            assert_eq!(machine.stack(), &[Value::Bool(true)]);
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Power-free control: `for`-let loop with `i++` updating `n` in the body; observes exactly two body increments through Bool `n===2`.
#[test]
fn native_power_free_for_let_increment_counter_control() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "let n=0; for(let i=0; i<2; i++){n=n+1;} n===2;";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native power-free increment compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native power-free increment VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            assert_eq!(machine.stack(), &[Value::Bool(true)]);
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Direct ordinary `+=` on a local: integer 2 + 3 observed through Bool `x===5`.
#[test]
fn native_local_add_assign_integer_value() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "let x: number = 2; x += 3; x===5;";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native add-assign integer compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native add-assign integer VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            assert_eq!(machine.stack(), &[Value::Bool(true)]);
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Direct ordinary `+=` on a local: fractional 1.5 + 2.25 observed through Bool `x===3.75` (typed logical equality, no coercion).
#[test]
fn native_local_add_assign_float_value() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "let x: number = 1.5; x += 2.25; x===3.75;";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native add-assign float compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native add-assign float VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            assert_eq!(machine.stack(), &[Value::Bool(true)]);
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Direct ordinary `+=` captured-old semantics: RHS runs once (calls===1) and the old value 2 is used, observed through Bool `x===3 && calls===1`.
#[test]
fn native_local_add_assign_captured_old_before_rhs() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function run(): boolean { let x: number = 2; let calls: number = 0; const rhs: () => number = (): number => { calls=calls+1; x=9; return 1; }; x += rhs(); return x===3 && calls===1; } run();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native add-assign captured-old compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native add-assign captured-old VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            assert_eq!(machine.stack(), &[Value::Bool(true)]);
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Record ident-member `**=` statement: reading the stored member back yields the exact Float 8.
#[test]
fn native_typescript_exponentiation_record_ident_statement_public_value() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); active.right **= 3; return active.right; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native record ident-member power compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native record ident-member power VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float record ident-member power completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 8.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Record ident-member `**=` statement: an alias observes the updated member (808).
#[test]
fn native_record_ident_power_alias_visibility() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; active.right **= 3; return alias.right*100+active.right; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native record ident-member power compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native record ident-member power VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float record ident-member power completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 808.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Record ident-member `**=` statement: the RHS runs once and the old member value is read before it (81).
#[test]
fn native_record_ident_power_old_before_rhs() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let calls: number = 0; let active: Row = relay({ left: 1, right: 2 }); const rhs: () => number = (): number => { calls=calls+1; active.right=3; return 3; }; active.right **= rhs(); return active.right*10+calls; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native record ident-member power compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native record ident-member power VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float record ident-member power completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 81.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Record ident-member `**=` statement: a unit base raised to an infinite exponent yields Float NaN.
#[test]
fn native_record_ident_power_unit_infinite_nan() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: -1 }); active.right **= (1/(-0)); return active.right; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native record ident-member power compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native record ident-member power VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float record ident-member power completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_nan());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Negative record ident-member `**=` control: a string right-hand side is refused before any guest work.
#[test]
fn native_record_ident_power_string_rhs_refused() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); active.right **= '3'; return active.right; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let error = match compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            ) {
                Ok(_) => panic!(
                    "record ident-member power source must be refused; guest_invocations=0 source={source:?}"
                ),
                Err(error) => error,
            };
            let message = error.to_string();
            eprintln!("TS_LOCAL_POWER_REFUSED {error:?} guest_invocations=0");
            assert!(
                message.contains("unsupported TypeScript compound indexed assignment operator"),
                "unexpected record ident-member power refusal {message:?}"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Compile-only baseline: the original captured-row RHS shape with `*=` in place of `**=`; no VM is created.
#[test]
fn native_record_capture_rhs_multiply_compile_control() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let calls: number = 0; let active: Row = relay({ left: 1, right: 2 }); const rhs: () => number = (): number => { calls=calls+1; active.right=3; return 3; }; active.right *= rhs(); return active.right*10+calls; } check();";
            let _compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native record capture RHS multiply baseline compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_MEMBER_CAPTURE_BASELINE_COMPILE_OK guest_invocations=0");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Explicit Row-parameter RHS: the old member value is captured before `rhs(active)` mutates the receiver; completion is Float 81.
#[test]
fn native_record_ident_power_explicit_row_rhs_order() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function rhs(r: Row): number { r.left=r.left+1; r.right=3; return 3; } function check(): number { let active: Row = relay({ left: 0, right: 2 }); active.right **= rhs(active); return active.right*10+active.left; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native record ident-member power compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native record ident-member power VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float record ident-member power completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 81.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_typescript_exponentiation_record_computed_statement_public_value() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const key: string = \"right\"; active[key] **= 3; return active.right; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native record computed-String-key power compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native record computed-String-key power VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float record computed-String-key power completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 8.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_record_computed_power_alias_visibility() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const key: string = \"right\"; active[key] **= 3; return alias.right*100+active.right; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native record computed-String-key power compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native record computed-String-key power VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float record computed-String-key power completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 808.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_record_computed_power_key_once_before_get() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function key(r: Row): string { r.left=r.left+1; r.right=3; return \"right\"; } function check(): number { let active: Row = relay({ left: 0, right: 2 }); active[key(active)] **= 3; return active.right*10+active.left; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native record computed-String-key power compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native record computed-String-key power VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float record computed-String-key power completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 271.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_record_computed_power_old_before_rhs() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function rhs(r: Row): number { r.left=r.left+1; r.right=3; return 3; } function check(): number { let active: Row = relay({ left: 0, right: 2 }); const key: string = \"right\"; active[key] **= rhs(active); return active.right*10+active.left; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native record computed-String-key power compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native record computed-String-key power VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float record computed-String-key power completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 81.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_record_computed_power_numeric_key_refused() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const key: number = 1; active[key] **= 3; return active.right; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let error = match compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            ) {
                Ok(_) => panic!(
                    "record computed-String-key power source must be refused; guest_invocations=0 source={source:?}"
                ),
                Err(error) => error,
            };
            let message = error.to_string();
            eprintln!("TS_LOCAL_POWER_REFUSED {error:?} guest_invocations=0");
            assert!(
                message.contains("unsupported TypeScript compound indexed assignment operator"),
                "unexpected record computed-String-key power refusal {message:?}"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_record_computed_power_string_rhs_refused() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const key: string = \"right\"; active[key] **= \"3\"; return active.right; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let error = match compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            ) {
                Ok(_) => panic!(
                    "record computed-String-key power source must be refused; guest_invocations=0 source={source:?}"
                ),
                Err(error) => error,
            };
            let message = error.to_string();
            eprintln!("TS_LOCAL_POWER_REFUSED {error:?} guest_invocations=0");
            assert!(
                message.contains("unsupported TypeScript compound indexed assignment operator"),
                "unexpected record computed-String-key power refusal {message:?}"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_typescript_exponentiation_record_ident_expression_public_value() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); return (active.right **= 3)+active.right*10; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native record ident-member power expression compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native record ident-member power expression VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float record ident-member power expression completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 88.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_record_ident_power_expr_alias_visibility() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; return (active.right **= 3)*100+alias.right; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native record ident-member power expression compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native record ident-member power expression VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float record ident-member power expression completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 808.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_record_ident_power_expr_old_before_rhs() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let calls: number = 0; let active: Row = relay({ left: 1, right: 2 }); const rhs: () => number = (): number => { calls=calls+1; active.right=3; return 3; }; return (active.right **= rhs())*100+active.right*10+calls; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native record ident-member power expression compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native record ident-member power expression VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float record ident-member power expression completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 881.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_typescript_exponentiation_record_computed_expression_public_value() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const key: string = \"right\"; return (active[key] **= 3); } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native record computed-key power expression compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native record computed-key power expression VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float record computed-key power expression completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 8.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_typescript_exponentiation_local_expression_public_value() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): number { let x: number = 2; return (x **= 3); } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native local power expression compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native local power expression VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float local power expression completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 8.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_record_ident_power_expr_string_rhs_refused() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); return (active.right **= \"3\"); } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let error = match compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            ) {
                Ok(_) => panic!(
                    "record power expression boundary must be refused; guest_invocations=0 source={source:?}"
                ),
                Err(error) => error,
            };
            let message = error.to_string();
            eprintln!("TS_LOCAL_POWER_REFUSED {error:?} guest_invocations=0");
            assert!(
                message.contains("unsupported TypeScript expression"),
                "unexpected record power expression refusal {message:?}"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_record_computed_power_expr_numeric_key_refused() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const key: number = 1; return (active[key] **= 3); } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let error = match compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            ) {
                Ok(_) => panic!(
                    "record power expression boundary must be refused; guest_invocations=0 source={source:?}"
                ),
                Err(error) => error,
            };
            let message = error.to_string();
            eprintln!("TS_LOCAL_POWER_REFUSED {error:?} guest_invocations=0");
            assert!(
                message.contains("unsupported TypeScript expression"),
                "unexpected record power expression refusal {message:?}"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_record_computed_power_expr_alias_visibility() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const alias: Row = active; const key: string = \"right\"; return (active[key] **= 3)*100+alias.right; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native record computed-key power expression compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native record computed-key power expression VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float record computed-key power expression completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 808.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_record_computed_power_expr_key_once_before_get() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function key(r: Row): string { r.left=r.left+1; r.right=3; return \"right\"; } function check(): number { let active: Row = relay({ left: 0, right: 2 }); return (active[key(active)] **= 3)*10+active.left; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native record computed-key power expression compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native record computed-key power expression VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float record computed-key power expression completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 271.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_record_computed_power_expr_old_before_rhs() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let calls: number = 0; let active: Row = relay({ left: 1, right: 2 }); const key: string = \"right\"; const rhs: () => number = (): number => { calls=calls+1; active.right=3; return 3; }; return (active[key] **= rhs())*100+active.right*10+calls; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native record computed-key power expression compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native record computed-key power expression VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float record computed-key power expression completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 881.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_record_computed_power_expr_string_rhs_refused() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "interface Row { left: number; right: number; } function relay(r: Row): Row { return r; } function check(): number { let active: Row = relay({ left: 1, right: 2 }); const key: string = \"right\"; return (active[key] **= \"3\"); } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let error = match compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            ) {
                Ok(_) => panic!(
                    "record power expression boundary must be refused; guest_invocations=0 source={source:?}"
                ),
                Err(error) => error,
            };
            let message = error.to_string();
            eprintln!("TS_LOCAL_POWER_REFUSED {error:?} guest_invocations=0");
            assert!(
                message.contains("unsupported TypeScript expression"),
                "unexpected record power expression refusal {message:?}"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_local_power_expr_result_and_store() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): number { let x: number = 2; return (x **= 3)+x*10; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native local power expression compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native local power expression VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float local power expression completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 88.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_local_power_expr_old_before_rhs() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): number { let x: number = 2; let calls: number = 0; const rhs: () => number = (): number => { calls=calls+1; x=3; return 3; }; return (x **= rhs())*100+x*10+calls; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native local power expression compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native local power expression VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float local power expression completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 881.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_local_power_expr_captured_target() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): number { let x: number = 2; const power: () => number = (): number => { return (x **= 3); }; return power()+x*10; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native local power expression compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native local power expression VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float local power expression completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 88.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_local_power_expr_inferred_number() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): number { let x = 2; return (x **= 3)+x*10; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native local power expression compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native local power expression VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float local power expression completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 88.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_local_power_expr_const_refused() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): number { const x: number = 2; return (x **= 3); } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let error = match compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            ) {
                Ok(_) => panic!(
                    "local power expression boundary must be refused; guest_invocations=0 source={source:?}"
                ),
                Err(error) => error,
            };
            let message = error.to_string();
            eprintln!("TS_LOCAL_POWER_REFUSED {error:?} guest_invocations=0");
            assert!(
                message.contains("cannot assign to const binding 'x'"),
                "unexpected local power expression refusal {message:?}"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_local_power_expr_string_rhs_refused() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): number { let x: number = 2; return (x **= \"3\"); } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let error = match compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            ) {
                Ok(_) => panic!(
                    "local power expression boundary must be refused; guest_invocations=0 source={source:?}"
                ),
                Err(error) => error,
            };
            let message = error.to_string();
            eprintln!("TS_LOCAL_POWER_REFUSED {error:?} guest_invocations=0");
            assert!(
                message.contains("unsupported TypeScript compound assignment operator"),
                "unexpected local power expression refusal {message:?}"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_local_power_expr_string_target_refused() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): number { let x: string = \"2\"; return (x **= 3); } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let error = match compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            ) {
                Ok(_) => panic!(
                    "local power expression boundary must be refused; guest_invocations=0 source={source:?}"
                ),
                Err(error) => error,
            };
            let message = error.to_string();
            eprintln!("TS_LOCAL_POWER_REFUSED {error:?} guest_invocations=0");
            assert!(
                message.contains("unsupported TypeScript compound assignment operator"),
                "unexpected local power expression refusal {message:?}"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_local_inferred_power_statement_compile_control() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): number { let x = 2; x **= 3; return x+x*10; } check();";
            let _compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native local inferred numeric statement RHS multiply baseline compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_LOCAL_INFERRED_STATEMENT_BASELINE_COMPILE_OK guest_invocations=0");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_explicit_int_power_read_preserved() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "import type { Int } from \"rustscript:types\"; function check(): number { let x: Int = 2; return x ** 3; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native local power expression compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native local power expression VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float local power expression completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 8.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_inferred_int_array_index_preserved() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): boolean { const values: number[] = [21]; let index = 0; const observed: number = values[index]; return observed === 21; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native add-assign integer compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native add-assign integer VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            assert_eq!(machine.stack(), &[Value::Bool(true)]);
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_explicit_int_power_float_store_refused() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "import type { Int } from \"rustscript:types\"; function check(): number { let x: Int = 2; return (x **= 3)+x*10; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let error = match compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            ) {
                Ok(_) => panic!(
                    "local power expression boundary must be refused; guest_invocations=0 source={source:?}"
                ),
                Err(error) => error,
            };
            let message = error.to_string();
            eprintln!("TS_LOCAL_POWER_REFUSED {error:?} guest_invocations=0");
            assert!(
                message.contains("local is declared as schema type 'int' but was assigned float"),
                "unexpected local power expression refusal {message:?}"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_local_inferred_power_statement_runtime() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): number { let x = 2; x **= 3; return x+x*10; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native local power expression compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native local power expression VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float local power expression completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 88.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_local_power_expr_inferred_captured_target() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): number { let x = 2; const power: () => number = (): number => { return (x **= 3); }; return power()+x*10; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native local power expression compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native local power expression VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float local power expression completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 88.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_local_power_expr_inferred_old_before_rhs() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): number { let x = 2; let calls: number = 0; const rhs: () => number = (): number => { calls=calls+1; x=3; return 3; }; return (x **= rhs())*100+x*10+calls; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native local power expression compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native local power expression VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float local power expression completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 881.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_inferred_capture_power_after_outer_reassign() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): number { let x = 2; const power: () => number = (): number => { return (x **= 3); }; x = 2; return power()+x*10; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native local power expression compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native local power expression VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float local power expression completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 88.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_inferred_int_capture_index_preserved() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): boolean { const values: number[] = [21, 42]; let index = 0; const advance: () => number = (): number => { index=1; return index; }; advance(); const observed: number = values[index]; return observed === 42; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native add-assign integer compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native add-assign integer VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            assert_eq!(machine.stack(), &[Value::Bool(true)]);
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_inferred_capture_power_callable_alias() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): number { let x = 2; const power: () => number = (): number => { return (x **= 3); }; const alias = power; x = 2; return alias()+x*10; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native local power expression compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native local power expression VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float local power expression completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 88.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_inferred_capture_power_repeated_calls() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): number { let x = 2; const power: () => number = (): number => { return (x **= 3); }; power(); x = 2; power(); return x*10; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native local power expression compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native local power expression VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float local power expression completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 80.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_inferred_capture_power_power_of_two_mul() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): number { let x = 2; const power: () => number = (): number => { return (x **= 3); }; return power()+x*8; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native local power expression compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native local power expression VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float local power expression completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 72.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_inferred_capture_power_nested_target() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): number { let x = 2; const outer: () => number = (): number => { const inner: () => number = (): number => { return (x **= 3); }; return inner(); }; return outer()+x*10; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native local power expression compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            let before = machine.get_fuel();
            assert_eq!(before, Some(10_000_000));
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native local power expression VM");
            let after = machine.get_fuel();
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?} fuel_before={before:?} fuel_after={after:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack(),
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float local power expression completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 88.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_const_array_plain_set_control() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function main(): number { const values: number[] = [3]; values[0] = 9; return values[0]; } main();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native const array int compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native const array int VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Int(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Int const array plain set completion, got {:?}",
                    machine.stack()
                )
            };
            assert_eq!(*number, 9);
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_const_array_power_statement() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function main(): number { const values: number[] = [3]; values[0] **= 2; const observed: number = values[0]; return observed; } main();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native const array float compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native const array float VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float const array power completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 9.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}
#[test]
fn native_const_array_plain_set_key_rhs_once() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): boolean { const values: number[] = [3, 7]; let index = 0; let calls: number = 0; const rhs: () => number = (): number => { calls = calls + 1; index = 0; return 2; }; values[index++] = rhs(); return values[0] === 2 && values[1] === 7 && index === 0 && calls === 1; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native const array order compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native const array order VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            assert_eq!(machine.stack(), &[Value::Bool(true)]);
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_const_array_power_key_rhs_once() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): boolean { const values: number[] = [3, 7]; let index = 0; let calls: number = 0; const rhs: () => number = (): number => { calls = calls + 1; index = 0; return 2; }; values[index++] **= rhs(); return values[0] === 9 && values[1] === 7 && index === 0 && calls === 1; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native const array order compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native const array order VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            assert_eq!(machine.stack(), &[Value::Bool(true)]);
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_const_array_power_alias_refused() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "const a: number[] = [2]; const b: number[] = a; a[0] **= 2; b[0];";
            eprintln!("TS_POWER_SOURCE {source}");
            let error = match compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            ) {
                Ok(_) => panic!(
                    "array power boundary boundary must be refused; guest_invocations=0 source={source:?}"
                ),
                Err(error) => error,
            };
            let message = format!("{error:?}");
            eprintln!("TS_LOCAL_POWER_REFUSED {error:?} guest_invocations=0");
            assert!(
                message.contains("E_MUTATE_ALIASED_COLLECTION"),
                "unexpected array power boundary refusal {message:?}"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_mutable_array_power_rebind_refused() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): number { let values: number[] = [3]; const rhs: () => number = (): number => { values = [7]; return 2; }; values[0] **= rhs(); const observed: number = values[0]; return observed; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let error = match compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            ) {
                Ok(_) => panic!(
                    "array power boundary boundary must be refused; guest_invocations=0 source={source:?}"
                ),
                Err(error) => error,
            };
            let message = error.to_string();
            eprintln!("TS_LOCAL_POWER_REFUSED {error:?} guest_invocations=0");
            assert!(
                message.contains("unsupported TypeScript compound indexed assignment operator"),
                "unexpected array power boundary refusal {message:?}"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_const_array_power_string_array_refused() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "const values: string[] = ['2']; values[0] **= 2;";
            eprintln!("TS_POWER_SOURCE {source}");
            let error = match compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            ) {
                Ok(_) => panic!(
                    "array power boundary boundary must be refused; guest_invocations=0 source={source:?}"
                ),
                Err(error) => error,
            };
            let message = error.to_string();
            eprintln!("TS_LOCAL_POWER_REFUSED {error:?} guest_invocations=0");
            assert!(
                message.contains("unsupported TypeScript compound indexed assignment operator"),
                "unexpected array power boundary refusal {message:?}"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_const_array_power_string_rhs_refused() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "const values: number[] = [3]; values[0] **= '2';";
            eprintln!("TS_POWER_SOURCE {source}");
            let error = match compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            ) {
                Ok(_) => panic!(
                    "array power boundary boundary must be refused; guest_invocations=0 source={source:?}"
                ),
                Err(error) => error,
            };
            let message = error.to_string();
            eprintln!("TS_LOCAL_POWER_REFUSED {error:?} guest_invocations=0");
            assert!(
                message.contains("unsupported TypeScript compound indexed assignment operator"),
                "unexpected array power boundary refusal {message:?}"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_const_array_power_expression_refused() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function main(): number { const values: number[] = [3]; return (values[0] **= 2); } main();";
            eprintln!("TS_POWER_SOURCE {source}");
            let error = match compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            ) {
                Ok(_) => panic!(
                    "array power boundary boundary must be refused; guest_invocations=0 source={source:?}"
                ),
                Err(error) => error,
            };
            let message = error.to_string();
            eprintln!("TS_LOCAL_POWER_REFUSED {error:?} guest_invocations=0");
            assert!(
                message.contains("unsupported TypeScript expression"),
                "unexpected array power boundary refusal {message:?}"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}
#[test]
fn native_numeric_power_equality_control() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): boolean { return (3 ** 2) === 9; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native const array order compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native const array order VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            assert_eq!(machine.stack(), &[Value::Bool(true)]);
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_number_array_strict_equality_float_int() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): boolean { const values: number[] = [3 ** 2]; return values[0] === 9; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native const array order compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native const array order VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            assert_eq!(machine.stack(), &[Value::Bool(true)]);
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_number_array_strict_inequality_float_int() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): boolean { const values: number[] = [3 ** 2]; return !(values[0] !== 9); } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native const array order compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native const array order VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            assert_eq!(machine.stack(), &[Value::Bool(true)]);
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_int_array_strict_inequality_preserves_wide_int() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "import type { Int } from \"rustscript:types\"; function check(): boolean { const low: Int = 9007199254740992; const high: Int = low + 1; const values: Int[] = [low, high]; return values[0] !== values[1]; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native const array order compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native const array order VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            assert_eq!(machine.stack(), &[Value::Bool(true)]);
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}
#[test]
fn native_number_array_power_item_exact_float_control() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "function check(): number { const values: number[] = [3 ** 2]; return values[0]; } check();";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native const array float compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native const array float VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            let Value::Float(number) = &machine.stack()[0] else {
                panic!(
                    "expected exact Float const array power completion, got {:?}",
                    machine.stack()
                )
            };
            assert!(number.is_finite());
            assert_eq!(number.to_bits(), 9.0_f64.to_bits());
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}
#[test]
fn native_codepoint_scalar_pair_equality() {
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let source = "const scalar: string = \"\\u{1F600}\"; scalar === \"\\uD83D\\uDE00\";";
            eprintln!("TS_POWER_SOURCE {source}");
            let compiled = compile_source_with_flavor_and_options(
                source,
                flavor(),
                pd_vm_compat_frontends::compile_options(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "native codepoint scalar compilation refused; guest_invocations=0 source={source:?} error={error:?}"
                )
            });
            eprintln!("TS_POWER_COMPILE_OK");
            let mut machine = Vm::new(compiled.program);
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(10_000_000);
            eprintln!("TS_POWER_GUEST_BEGIN");
            let status = machine.run().expect("native codepoint scalar VM");
            eprintln!(
                "TS_POWER_VM status={status:?} jit_enabled={} aot={} jit_native={} stack={:?}",
                machine.jit_config().enabled,
                machine.aot_exec_count(),
                machine.jit_native_exec_count(),
                machine.stack()
            );
            assert_eq!(status, VmStatus::Halted);
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert_eq!(machine.stack().len(), 1);
            assert_eq!(machine.stack(), &[Value::Bool(true)]);
            machine.reset_for_reuse().unwrap();
            assert!(!machine.scope_reset_pending());
            assert_eq!(machine.host_context().resource_count(), 0);
            eprintln!("TS_POWER_RESET resource_count=0 guest_invocations=1");
        })
        .unwrap()
        .join()
        .unwrap();
}
