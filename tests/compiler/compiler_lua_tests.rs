#[path = "../common/mod.rs"]
mod common;
use common::*;
use std::fs;
use vm::disassemble_program;

#[test]
fn lua_runtime_cases_work() {
    let cases = vec![
        RuntimeCase {
            name: "assignment_and_arithmetic",
            source: r#"
                local a = 1
                a = a + 41
                a
            "#,
            flavor: SourceFlavor::Lua,
            expected_stack: vec![Value::Int(42)],
            expected_locals: Some(1),
        },
        RuntimeCase {
            name: "if_else_and_logic",
            source: r#"
                local a = 2
                if a > 1 and a < 3 then
                    42
                else
                    0
                end
            "#,
            flavor: SourceFlavor::Lua,
            expected_stack: vec![Value::Int(42)],
            expected_locals: None,
        },
        RuntimeCase {
            name: "while_loop",
            source: r#"
                local i = 0
                while i < 3 do
                    i = i + 1
                end
                i
            "#,
            flavor: SourceFlavor::Lua,
            expected_stack: vec![Value::Int(3)],
            expected_locals: None,
        },
        RuntimeCase {
            name: "do_end_block",
            source: r#"
                local value = 1
                do
                    value = value + 41
                end
                value
            "#,
            flavor: SourceFlavor::Lua,
            expected_stack: vec![Value::Int(42)],
            expected_locals: None,
        },
        RuntimeCase {
            name: "float_char_and_hex_escape_literals",
            source: r#"
                local f = 1.25
                local c = '\x41'
                local s = "\x42"
                f
                c
                s
            "#,
            flavor: SourceFlavor::Lua,
            expected_stack: vec![Value::Float(1.25), Value::string("A"), Value::string("B")],
            expected_locals: None,
        },
        RuntimeCase {
            name: "regex namespace accepts inline flags argument",
            source: r#"
                local re = require("re")
                re.match("^lua$", "LUA", "i")
            "#,
            flavor: SourceFlavor::Lua,
            expected_stack: vec![Value::Bool(true)],
            expected_locals: None,
        },
        RuntimeCase {
            name: "elseif_and_elif_alias",
            source: r#"
                local a = 2
                if a == 1 then
                    0
                elseif a == 2 then
                    1
                else
                    2
                end

                if a == 1 then
                    0
                elif a == 2 then
                    42
                else
                    0
                end
            "#,
            flavor: SourceFlavor::Lua,
            expected_stack: vec![Value::Int(1), Value::Int(42)],
            expected_locals: None,
        },
        RuntimeCase {
            name: "empty param closure captures outer value",
            source: r#"
                local x = 41
                local f = function() return x + 1 end
                f()
            "#,
            flavor: SourceFlavor::Lua,
            expected_stack: vec![Value::Int(42)],
            expected_locals: None,
        },
        RuntimeCase {
            name: "multi_return_locals_unpack_in_order",
            source: r#"
                local function x()
                    return 1, 2
                end
                local a, b = x()
                a
                b
            "#,
            flavor: SourceFlavor::Lua,
            expected_stack: vec![Value::Int(1), Value::Int(2)],
            expected_locals: None,
        },
        RuntimeCase {
            name: "multi_return_single_local_keeps_first_value",
            source: r#"
                local function x()
                    return 1, 2
                end
                local a = x()
                a
            "#,
            flavor: SourceFlavor::Lua,
            expected_stack: vec![Value::Int(1)],
            expected_locals: None,
        },
        RuntimeCase {
            name: "multi_return_missing_locals_are_null_padded",
            source: r#"
                local function x()
                    return 1, 2
                end
                local a, b, c = x()
                a
                b
                c
            "#,
            flavor: SourceFlavor::Lua,
            expected_stack: vec![Value::Int(1), Value::Int(2), Value::Null],
            expected_locals: None,
        },
        RuntimeCase {
            name: "conditional_multi_return_pads_short_branch_with_null",
            source: r#"
                local function x()
                    if true then
                        return 1
                    else
                        return 1, 2
                    end
                end
                local a, b, c = x()
                a
                b
                c
            "#,
            flavor: SourceFlavor::Lua,
            expected_stack: vec![Value::Int(1), Value::Null, Value::Null],
            expected_locals: None,
        },
        RuntimeCase {
            name: "pcall_prefixes_success_and_forwards_multi_return",
            source: r#"
                local function x()
                    return 1, 2
                end
                local ok, a, b = pcall(x)
                ok
                a
                b
            "#,
            flavor: SourceFlavor::Lua,
            expected_stack: vec![Value::Bool(true), Value::Int(1), Value::Int(2)],
            expected_locals: None,
        },
        RuntimeCase {
            name: "pcall_scalar_context_keeps_success_flag",
            source: r#"
                local function x()
                    return 1, 2
                end
                local ok = pcall(x)
                ok
            "#,
            flavor: SourceFlavor::Lua,
            expected_stack: vec![Value::Bool(true)],
            expected_locals: None,
        },
        RuntimeCase {
            name: "xpcall_ignores_handler_and_forwards_args",
            source: r#"
                local function add_pair(a, b)
                    return a + b, b
                end
                local function handler(err)
                    return err
                end
                local ok, sum, rhs = xpcall(add_pair, handler, 3, 4)
                ok
                sum
                rhs
            "#,
            flavor: SourceFlavor::Lua,
            expected_stack: vec![Value::Bool(true), Value::Int(7), Value::Int(4)],
            expected_locals: None,
        },
        RuntimeCase {
            name: "inline_function_literal_empty_body_returns_null",
            source: r#"
                local f = function() end
                f()
            "#,
            flavor: SourceFlavor::Lua,
            expected_stack: vec![Value::Null],
            expected_locals: None,
        },
        RuntimeCase {
            name: "inline_function_literal_empty_return_returns_null",
            source: r#"
                local f = function() return end
                f()
            "#,
            flavor: SourceFlavor::Lua,
            expected_stack: vec![Value::Null],
            expected_locals: None,
        },
    ];

    run_runtime_cases(&cases);
}

#[test]
fn lua_do_block_lowers_without_synthetic_true_guard() {
    let compiled = compile_source_with_flavor_and_options(
        r#"
            local value = 1
            do
                value = value + 41
            end
            value
        "#,
        SourceFlavor::Lua,
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("lua source should compile");

    let disasm = disassemble_program(&compiled.program);
    assert!(
        !disasm.contains("Bool(true)"),
        "do block should not materialize a synthetic true guard:\n{disasm}"
    );
    assert!(
        !disasm.contains("brfalse"),
        "do block should not materialize a synthetic branch:\n{disasm}"
    );
}

#[test]
fn lua_rejection_cases_work() {
    let parse_cases = [ParseErrorCase {
        name: "assignment_to_undeclared_local",
        source: r#"
                value = 1
            "#,
        flavor: SourceFlavor::Lua,
        expected_contains_all: &["unknown local 'value'"],
    }];

    for case in &parse_cases {
        expect_parse_error_case(case);
    }
}

#[test]
fn lua_complex_fixture_runs() {
    let path = staged_example_path("example_complex.lua");
    let compiled =
        compile_source_file_with_options(path.as_path(), pd_vm_compat_frontends::compile_options())
            .expect("compile should succeed");
    let mut vm = Vm::new(compiled.program);
    for func in &compiled.functions {
        match func.name.as_str() {
            "add_one" => {
                vm.register_function(Box::new(AddOne));
            }
            "print" => {
                vm.register_function(Box::new(PrintBuiltin));
            }
            "runtime::sleep" => {
                vm.register_function(Box::new(RuntimeSleep));
            }
            other => panic!("unexpected function {other}"),
        }
    }
    loop {
        match vm.run().expect("vm should run") {
            VmStatus::Halted => break,
            VmStatus::Yielded => continue,
            VmStatus::Waiting(_op_id) => vm
                .wait_for_host_op_blocking()
                .expect("vm should complete host operation"),
        }
    }
    assert_eq!(vm.stack(), &[Value::Int(12)]);
}

#[test]
fn lua_runtime_namespace_host_calls_are_supported() {
    let case = RuntimeCase {
        name: "runtime namespace host calls are supported",
        source: r#"
            local runtime = require("runtime")
            runtime.sleep(1)
        "#,
        flavor: SourceFlavor::Lua,
        expected_stack: vec![Value::Bool(true)],
        expected_locals: None,
    };
    let bindings = [HostBindingCase {
        name: "runtime::sleep",
        factory: make_runtime_sleep,
    }];
    run_runtime_case_with_bindings(&case, &bindings);
}

#[test]
fn lua_non_strict_comparisons_treat_nan_as_false() {
    let case = RuntimeCase {
        name: "non strict comparisons treat nan as false",
        source: r#"
            local nan = 0.0 / 0.0
            nan <= 1.0
            nan >= 1.0
        "#,
        flavor: SourceFlavor::Lua,
        expected_stack: vec![Value::Bool(false), Value::Bool(false)],
        expected_locals: None,
    };
    run_runtime_case(&case);
}

#[test]
fn lua_file_module_namespace_calls_keep_qualified_provenance() {
    let root = namespace_case_root("lua_qualified_namespace");
    fs::write(root.join("left.rss"), "pub fn tag() { 1 }\n").expect("left module");
    fs::write(root.join("right.rss"), "pub fn tag() { 2 }\n").expect("right module");
    fs::write(
        root.join("strings.rss"),
        r#"
        pub fn non_empty(value) {
            value.length != 0;
        }
        "#,
    )
    .expect("strings module");
    let main_path = root.join("main.lua");
    fs::write(
        &main_path,
        r#"
        local left = require("./left.rss")
        local right = require("./right.rss")
        local string = require("./strings.rss")

        local function non_empty(value)
            return false
        end

        local box = { non_empty = 9 }
        -- string.non_empty("no")
        local quoted = "string.non_empty("
        local utf = "是"

        local local_flag = 0
        if non_empty("x") then
            local_flag = 1
        end
        local module_flag = 0
        if string.non_empty("rss") then
            module_flag = 1
        end
        if quoted ~= nil and utf ~= nil then
            left.tag() + right.tag() + box.non_empty + local_flag + module_flag
        else
            0
        end
        "#,
    )
    .expect("lua source");

    let compiled = compile_source_file_with_options(
        main_path.as_path(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("qualified lua namespace fixture should compile");
    let mut vm = Vm::new(compiled.program);
    let status = vm.run().expect("vm should run");
    assert_eq!(status, VmStatus::Halted);
    assert_eq!(vm.stack(), &[Value::Int(13)]);
}

#[test]
fn lua_file_module_unknown_member_keeps_mapped_span() {
    let root = namespace_case_root("lua_mapped_span");
    fs::write(
        root.join("strings.rss"),
        "pub fn non_empty(value) { value.length != 0; }\n",
    )
    .expect("strings module");
    let main_path = root.join("main.lua");
    let source = concat!(
        "local string = require(\"./strings.rss\")\n",
        "local single = 'string.does_not_exist('\n",
        "local double = \"string.does_not_exist(\"\n",
        "-- string.does_not_exist(\"no\")\n",
        "local utf_before = \"是\"\n",
        "   string.does_not_exist(\"rss\")\n",
        "local utf_after = \"後\"\n",
    );
    fs::write(&main_path, source).expect("lua source");

    let error = match compile_source_file_with_options(
        main_path.as_path(),
        pd_vm_compat_frontends::compile_options(),
    ) {
        Ok(_) => panic!("unknown file-module member should fail"),
        Err(error) => error,
    };
    match error {
        vm::SourcePathError::SourceWithMap { error, sources } => {
            let message = error.to_string();
            assert!(
                message.contains("unknown namespace call 'string::does_not_exist'"),
                "diagnostic must identify the qualified namespace member, got {message}"
            );
            let parse = match &error {
                vm::SourceError::Parse(parse) => parse,
                other => panic!("expected parse diagnostic, got {other:?}"),
            };
            assert_eq!(
                parse.line, 6,
                "mapped diagnostic must use the call-site line, got {} ({message})",
                parse.line
            );
            let span = parse
                .span
                .expect("failing file-module call must keep a mapped span");
            let call_line = source.lines().nth(5).expect("call-site line");
            let lo = source.find(call_line).expect("call-site offset");
            assert_eq!(
                (span.lo, span.hi),
                (lo, lo + call_line.len()),
                "core source-loader maps unknown namespace calls to the full call-site line"
            );
            let text = sources
                .span_text(span)
                .expect("mapped span must resolve against the kept source map");
            assert_eq!(text, call_line);
            assert!(
                text.contains("string.does_not_exist"),
                "mapped span should cover the call expression, got {text:?}"
            );
            assert!(
                !text.contains("require") && !text.contains("是") && !text.contains("後"),
                "mapped span must not be the import line or surrounding literals, got {text:?}"
            );
        }
        other => panic!("expected SourceWithMap, got {other}"),
    }
}
