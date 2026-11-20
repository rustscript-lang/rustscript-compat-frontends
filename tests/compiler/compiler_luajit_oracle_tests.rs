//! Differential syntax checks against a locked LuaJIT 2.1 binary.
//!
//! Ordinary Cargo does not spawn LuaJIT. Set LUAJIT_ORACLE to the luajit
//! built from 96f7b297 with XCFLAGS=-DLUAJIT_ENABLE_LUA52COMPAT to compare
//! load(source, "@probe", "t"). jit.version is a runtime baseline, not the
//! source identity. This is not a second public file entry.

#[path = "../../src/frontends/lua/ast.rs"]
mod ast;
#[path = "../../src/frontends/lua/lexer.rs"]
mod lexer;
#[path = "../../src/frontends/lua/parser.rs"]
mod parser;
#[path = "../../src/frontends/lua/resolve.rs"]
mod resolve;

use std::io::Write;
use std::process::{Command, Stdio};

use ast::Stmt;

const ORACLE_SHA: &str = "96f7b297326e9fd454b7186c6858b2773f94a0dc";
const ORACLE_VERSION_BASELINE: &str = "LuaJIT 2.1.1795119671";

const LOAD_DRIVER: &str = "\
local src = io.read('*a')
local fn = load(src, '@probe', 't')
if fn then
  io.write('ACCEPT\\n')
else
  io.write('REJECT\\n')
end
";

const BASELINE_DRIVER: &str = "\
io.write(jit.version, '\\n')
local g = load('local goto = 1', '@g', 't')
local b = load('for i=1,2 do break print(i) end', '@b', 't')
io.write(g and 'BAD_GOTO' or 'GOTO_KW', '\\n')
io.write(b and 'BREAK_OK' or 'BREAK_BAD', '\\n')
io.write(type(table.pack), '\\n')
";

fn configured_oracle() -> Option<String> {
    std::env::var("LUAJIT_ORACLE").ok()
}

fn syntax_ok(source: &[u8]) -> bool {
    match parser::parse_chunk(source) {
        Ok(chunk) => resolve::resolve(&chunk).errors.is_empty(),
        Err(_) => false,
    }
}

fn spawn_oracle(path: &str, args: &[&str], stdin_bytes: Option<&[u8]>) -> std::process::Output {
    let mut cmd = Command::new(path);
    cmd.args(args)
        .stdin(if stdin_bytes.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "linux")]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(limit_oracle_child);
    }
    let mut child = cmd.spawn().unwrap_or_else(|err| {
        panic!("LUAJIT_ORACLE at {path:?} failed to start: {err}");
    });
    if let Some(bytes) = stdin_bytes {
        let mut input = child
            .stdin
            .take()
            .unwrap_or_else(|| panic!("LUAJIT_ORACLE at {path:?} has no stdin pipe"));
        input.write_all(bytes).unwrap_or_else(|err| {
            panic!("LUAJIT_ORACLE at {path:?} rejected stdin: {err}");
        });
    }
    child.wait_with_output().unwrap_or_else(|err| {
        panic!("LUAJIT_ORACLE at {path:?} failed while waiting: {err}");
    })
}

#[cfg(target_os = "linux")]
fn limit_oracle_child() -> std::io::Result<()> {
    #[repr(C)]
    struct RLimit {
        cur: u64,
        max: u64,
    }
    unsafe extern "C" {
        fn setrlimit(resource: i32, rlim: *const RLimit) -> i32;
        fn alarm(seconds: u32) -> u32;
        fn _exit(code: i32) -> !;
    }
    const RLIMIT_CPU: i32 = 0;
    const RLIMIT_AS: i32 = 9;
    let cpu = RLimit { cur: 2, max: 2 };
    let address = RLimit {
        cur: 128 * 1024 * 1024,
        max: 128 * 1024 * 1024,
    };
    unsafe {
        if setrlimit(RLIMIT_CPU, &cpu) != 0 || setrlimit(RLIMIT_AS, &address) != 0 {
            _exit(126);
        }
        alarm(5);
    }
    Ok(())
}

fn require_success(path: &str, output: &std::process::Output, what: &str) {
    if output.status.success() && output.stderr.is_empty() {
        return;
    }
    panic!(
        "LUAJIT_ORACLE at {path:?} {what} failed: status={} stdout={:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_oracle_baseline(path: &str) {
    let output = spawn_oracle(path, &["-e", BASELINE_DRIVER], None);
    require_success(path, &output, "baseline");
    let text = String::from_utf8_lossy(&output.stdout);
    let expected = format!("{ORACLE_VERSION_BASELINE}\nGOTO_KW\nBREAK_OK\nfunction\n");
    assert_eq!(
        text, expected,
        "oracle flag/version baseline mismatch; jit.version is not source identity (source commit {ORACLE_SHA})"
    );
    eprintln!(
        "oracle baseline jit.version={ORACLE_VERSION_BASELINE} (version string is not source identity); source commit {ORACLE_SHA}"
    );
}

fn oracle_accepts(path: &str, source: &[u8]) -> bool {
    let output = spawn_oracle(path, &["-e", LOAD_DRIVER], Some(source));
    require_success(path, &output, "load protocol");
    match output.stdout.as_slice() {
        b"ACCEPT\n" => true,
        b"REJECT\n" => false,
        other => panic!(
            "LUAJIT_ORACLE at {path:?} broke the ACCEPT/REJECT protocol: {:?}",
            String::from_utf8_lossy(other)
        ),
    }
}

#[test]
fn oracle_config_accepts_52_markers() {
    let Some(path) = configured_oracle() else {
        eprintln!("oracle baseline not run: LUAJIT_ORACLE unset; not a differential GREEN");
        return;
    };
    assert_oracle_baseline(&path);
}

fn cases() -> Vec<(&'static str, &'static [u8])> {
    vec![
        ("empty", b""),
        ("semi", b";;;"),
        ("return_one", b"return 1"),
        ("bare_num", b"42"),
        ("floor_div", b"return 7//2"),
        ("elif", b"if true then 1 elif false then 2 end"),
        ("pub_fn", b"pub fn f() end"),
        ("local_goto", b"local goto = 1"),
        ("break_mid", b"for i=1,2 do break print(i) end"),
        ("break_out", b"break"),
        ("continue_out", b"continue"),
        ("continue_ok", b"while true do continue end"),
        ("continue_mid", b"while true do continue print(1) end"),
        ("continue_name", b"local continue = 1"),
        ("const_ok", b"const x = 1"),
        ("const_assign", b"const x = 1\nx = 2"),
        ("const_shadow", b"const x = 1\ndo local x = 2 end"),
        ("const_after", b"do const x = 1 end\nlocal x = 2"),
        ("const_field", b"const t = {}\nt.a = 1"),
        ("const_up", b"const x = 1\nlocal function f() x = 2 end"),
        ("const_for", b"const i = 1\nfor i = 1, 2 do end"),
        ("goto_fwd", b"goto a\nlocal x = 1\n::a::"),
        ("goto_into", b"goto L\nlocal x = 1\n::L::\nreturn x"),
        ("goto_back", b"::a::\nlocal x = 1\ngoto a"),
        ("goto_out", b"::a::\ndo local x = 1 goto a end"),
        ("dup_label", b"::a::\n::a::"),
        ("nested_label", b"::a::\ndo ::a:: end"),
        ("goto_undef", b"goto missing"),
        ("label_end", b"do ::a:: end"),
        ("goto_into_do", b"goto a\ndo local x ::a:: end"),
        (
            "repeat_into",
            b"repeat\n  goto a\n  local x = 1\n  ::a::\nuntil true",
        ),
        ("vararg_bad", b"function f() return ... end"),
        ("vararg_ok", b"function f(...) return ... end"),
        ("dots_main", b"return ..."),
        ("pow", b"return -2^2"),
        ("concat", b"return 'a'..'b'..'c'"),
        ("coal", b"return nil ?? 3"),
        ("nav", b"return a?.b"),
        ("nav_index", b"return a?.[1]"),
        ("nav_call", b"return a?.b()"),
        ("method_nav", b"return a:b?.()"),
        ("colon_nav", b"return a?.:b()"),
        ("nav_assign", b"a?.b = 1"),
        ("nav_parallel", b"a?.b, c = 1, 2"),
        ("compound", b"a += 1"),
        ("compound_space", b"a + = 1"),
        ("xor_assign", b"a ~= 1"),
        ("compound_pow", b"a ^= 2"),
        ("compound_coal", b"a ??= 1"),
        ("ne_expr", b"return a ~= b"),
        ("short", b"local f = |x| -> x + 1"),
        ("short_empty", b"local f = || -> 1"),
        ("short_do", b"local f = |x| -> do return x end"),
        ("name_arrow", b"local f = x -> x"),
        ("arrow_stmt", b"x -> x"),
        ("ternary", b"return 1 > 0 ? 7 : 8"),
        ("ternary_method", b"return a ? b:c() : d"),
        ("ternary_paren", b"return a ? (b:c()) : d"),
        ("and_or", b"return a or b and c"),
        ("shl", b"return 1 << 2"),
        ("sar", b"return 1 ~>> 2"),
        ("andand", b"return a && b"),
        ("not_eq", b"return a != b"),
        ("bang", b"return !a"),
        ("hexfloat", b"return 0x1.8p1"),
        ("bin", b"return 0b1010"),
        ("under", b"return 1_000"),
        ("ull", b"return 1ULL"),
        ("ll", b"return -1LL"),
        ("imag", b"return 1.5i"),
        ("bad_u", b"return 1u"),
        ("float_ll", b"return 1.0LL"),
        ("bad_number", b"return 1e"),
        ("hex_esc", b"return '\\xFF'"),
        ("bad_esc", b"return '\\q'"),
        ("z_esc", b"return 'a\\z  \nb'"),
        ("u_esc", b"return '\\u{1F600}'"),
        ("u_bad", b"return '\\u{110000}'"),
        ("bom_return", b"\xef\xbb\xbfreturn 1"),
        ("utf8_ident", "local caf\u{00e9} = 1".as_bytes()),
        ("nonutf8", b"local \xff = 1"),
        ("shebang", b"#!/usr/bin/env luajit\nreturn 1"),
        ("line_dir", b"# 10\nreturn 1"),
        ("slash_comment", b"// comment\nreturn 1"),
        ("comment_require", b"-- require('nope')\nreturn 1"),
        ("string_require", b"return \"require('nope')\""),
        ("require_call", b"require('mod')"),
        ("require_string", b"require 'mod.sub'"),
        ("require_field", b"foo.require('mod')"),
        ("paren_call", b"return (f())"),
        ("multi_pos", b"return f(), g(), ..."),
        ("local_multi", b"local a, b = f(), g()"),
        ("table_multi", b"return {f(), g()}"),
        ("if_calls", b"if a then b() elseif c then d() else e() end"),
        ("until_local", b"repeat local x = 1 until x"),
        ("local_rec", b"local function f() return f end"),
        ("method", b"function a.b:c() return self end"),
        ("attr_const", b"local x <const> = 1"),
        ("empty_table", b"return {}"),
        ("long_str", b"return [=[a]=]"),
        ("unclosed", b"return 'abc"),
        ("extra", b"return 1 end"),
        ("kw_end", b"local end = 1"),
        ("soft_const", b"const = 1"),
        ("local_dup", b"local a, a = 1, 2"),
        ("const_dup", b"const a, a = 1, 2"),
        ("dots_short", b"local f = |...| -> ..."),
        ("dots_short_bad", b"local f = |x| -> ..."),
        ("table_pack", b"return type(table.pack)"),
        ("high_call", b"\xff()"),
        ("crlf", b"local x = 1\r\nreturn x"),
        ("esc_nl", b"return 'a\\\nb'"),
        ("comment_between", b"local x --c\n= 1"),
        ("short_comma", b"local f = || -> 1, 2"),
        ("label_kw", b"::end::"),
        ("goto_fn", b"function f() goto a end\n::a::"),
        ("break_fn", b"function f() break end"),
        ("continue_semi", b"while true do continue; end"),
        ("break_semi", b"while true do break; local x end"),
        ("field_soft", b"return { const = 1, continue = 2 }"),
        ("assign_index", b"a[1], a.b = 2, 3"),
        ("compound_index", b"a[1] += 2"),
        ("nested_wrong", b"return [=[ [==[ ]=]"),
        ("leading_under", b"return _1"),
        ("double_under", b"return 1__2"),
        ("zero_x", b"return 0x"),
        ("bin_bad", b"return 0b2"),
        ("u_surrogate", b"return '\\u{D800}'"),
        ("empty_u", b"return '\\u{}'"),
        ("ident_digit", b"return 1abc"),
        ("empty_parens", b"return ()"),
        ("paren_assign", b"(a) = 1"),
        ("call_assign", b"f() = 1"),
        ("for_missing", b"for i do end"),
        ("hash_mid", b"local x = 1 # 2"),
        ("only_shebang", b"#!/bin/luajit\n"),
        ("bom_only", b"\xef\xbb\xbf"),
        ("bitwise", b"return ~a & b | c ~ d << 1 >> 2"),
        ("not_chain", b"return not not a"),
        ("and_call", b"return a and f()"),
        ("table_call", b"f{1}"),
        ("string_call", b"f'abc'"),
        ("method_string", b"a:b'c'"),
        (
            "self_inner",
            b"function o:m() local function n() return self end end",
        ),
        (
            "shadow_param",
            b"local function f(a) local a = 1 return a end",
        ),
        ("goto_same", b"goto a\n::a::"),
        ("two_gotos", b"goto a\ngoto a\n::a::"),
        ("inner_goto_out", b"do goto a end\n::a::"),
        ("label_semi", b"::a::;\ngoto a"),
        ("const_fn", b"const function f() end\nf = 1"),
        ("const_fn_ok", b"const function f() return f end"),
        ("func_missing", b"function f()"),
        ("bad_long", b"return [=a=]"),
        ("line_spaces", b"#   4\t\nreturn 1"),
        ("directive_eof", b"# 9"),
        ("not_bang_name", b"return not !a"),
        ("question_only", b"return a ?"),
        ("arrow_only", b"return -> 1"),
        ("space_shl", b"a << = 1"),
        ("imag_ll", b"return 1LLi"),
        ("hex_ll", b"return 0xFFLL"),
        ("plus_exp", b"return 1e+2"),
        ("hex_p", b"return 0x1p-1"),
        ("dot_num", b"return .5"),
        ("num_dot", b"return 5."),
        ("long_nl", b"return [[\nhello]]"),
        ("mismatch_long", b"return [[a]=]"),
        ("esc_quote", b"return '\\''"),
        ("dec_esc", b"return '\\000\\255'"),
        ("dec_bad", b"return '\\256'"),
        ("x_short", b"return '\\x4'"),
        ("unclosed_long", b"return [[abc"),
        ("long_comment", b"--[[ require('nope') ]]\nreturn 1"),
        ("eq_comment", b"--[=[ ]=]\nreturn 1"),
        ("comment_unclosed", b"--[[ abc"),
        ("repeat_empty", b"repeat until true"),
        ("while_empty", b"while false do end"),
        ("if_empty", b"if false then end"),
        ("do_empty", b"do end"),
        ("local_none", b"local a, b"),
        ("local_rhs", b"local a, b = 1"),
        ("generic_for", b"for k, v in pairs(t) do end"),
        ("for_step", b"for i=1,10,2 do end"),
        ("dotted_fn", b"function a.b.c() end"),
        ("ellipsis_mid", b"function f(..., a) end"),
        ("trailing_comma_params", b"function f(a,) end"),
        ("trailing_comma_call", b"f(1,)"),
        ("return_semi", b"do return 1; end"),
        ("return_then", b"do return 1 local x end"),
        (
            "continue_if",
            b"while true do if a then continue else break end end",
        ),
        ("nested_break", b"while a do while b do break end end"),
        ("nested_continue", b"while a do while b do continue end end"),
        ("const_method", b"const function o:m() end"),
        ("short_ws", b"local f = ||-> 1"),
        ("nav_bracket_bad", b"return a?[1]"),
        ("call_nav_stmt", b"a?.b()"),
        ("call_nav_comma", b"a?.b(), c"),
        ("field_nav", b"return a.b?.c"),
        ("table_nav", b"return { a?.b }"),
        ("name_arrow_call", b"f(x -> x)"),
        ("local_const_soft", b"local const = 1"),
        ("const_continue", b"local const = continue"),
        ("continue_call", b"continue()"),
        ("high_string", b"return '\xff'"),
        ("utf8_string", "return '\u{4f60}\u{597d}'".as_bytes()),
        ("utf8_comment", "local x = 1 -- \u{6ce8}\u{91ca}".as_bytes()),
        ("ident_stmt", b"abc"),
        ("call_stmt", b"f()"),
        ("index_call", b"return a[b](c)"),
        ("not_call", b"return not f()"),
        ("len_call", b"return #f()"),
        ("pow_neg", b"return 2^-2"),
        ("concat_num", b"return 1 .. 2"),
        ("and_symbol", b"return a and b && c"),
        ("or_symbol", b"return a or b || c"),
        ("nav_space", b"return a ? .b"),
        ("arrow_space", b"local f = |x| - > x"),
        ("label_space", b": :a: :"),
        ("shl_space", b"return 1 < < 2"),
        ("hash_bang_later", b"local x = 1\n#!/bin/luajit"),
        ("bad_param", b"function f(1) end"),
        ("dots_assign", b"... = 1"),
        ("bitwise_not", b"return ~a"),
        ("eq_chain", b"return a == b ~= c"),
        ("cmp_chain", b"return 1 < 2 < 3"),
        ("unclosed_fn", b"local function f() return 1"),
        ("until_missing", b"repeat local x = 1"),
        ("if_missing_then", b"if a b end"),
        ("goto_missing", b"goto"),
        ("label_missing", b"::name"),
        ("double_comma", b"local a,,b = 1"),
        ("comma_empty", b"return 1,"),
        ("func_in_assign", b"a = function() return 1 end"),
        ("if_return", b"if a then return 1 else return 2 end"),
        (
            "for_break_continue",
            b"for i=1,3 do if i == 1 then continue end break end",
        ),
        ("assign_up_ok", b"local x = 1\nlocal function f() x = 2 end"),
        ("global_assign", b"x = 1"),
        ("method_self", b"function o:m() return self end"),
        ("empty_return", b"return"),
        ("semi_return", b"return;"),
        ("return_after", b"return; local x"),
        ("do_return", b"do return end"),
        ("nested_do", b"do do end end"),
        ("comment_cr", b"-- c\rlocal x = 1"),
        ("ws_z_crlf", b"return 'a\\z\r\nb'"),
        ("string_hex", b"return '\\xff'"),
        ("esc_z_only", b"return '\\z'"),
        ("esc_backslash", b"return '\\\\'"),
        ("zero_ll", b"return 0LL"),
        ("bin_ll", b"return 0b11LL"),
        ("imag_hex", b"return 0x1p1i"),
        ("under_suffix", b"return 1_LL"),
        ("exp_under", b"return 1e_2"),
        ("u_ascii", b"return '\\u{41}'"),
        ("u_two", b"return '\\u{80}'"),
        ("u_four", b"return '\\u{10000}'"),
        ("cr_nl", b"local x = 1\rreturn x"),
        ("compound_concat", b"a ..= 'x'"),
        ("compound_shl", b"a <<= 1"),
        ("compound_sar", b"a ~>>= 1"),
        ("compound_mod", b"a %= 2"),
        ("compound_band", b"a &= 1"),
        ("compound_bor", b"a |= 1"),
        ("compound_shr", b"a >>= 1"),
        ("compound_div", b"a /= 2"),
        ("compound_mul", b"a *= 2"),
        ("compound_sub", b"a -= 1"),
        ("plus_assign_call", b"a.b += f()"),
        ("short_vararg_bad", b"function f(..., a) end"),
        ("empty_params_space", b"local f = | | -> 1"),
        ("bor_short", b"return |x, y| -> x | y"),
        ("or_or_short", b"return || -> 1"),
        ("question_question", b"return a ? ? b"),
        ("tilde_gt", b"return 1 ~> 2"),
        ("hash_number", b"return 1 #2"),
        ("directive_bad", b"# abc\nreturn 1"),
        ("byte_ff_string", b"return '\xff\xfe'"),
        ("mixed_assign", b"a.b, c[d], e = 1, 2, 3"),
        (
            "local_func_params",
            b"local function f(a, b, ...) return a end",
        ),
        ("while_return", b"while a do return 1 end"),
        ("repeat_continue", b"repeat continue until true"),
        ("label_before", b"::a::\ngoto a\nlocal x = 1"),
        ("const_in_for", b"for i=1,2 do const x = i end"),
        ("const_index", b"const a = {}\na[1] = 2"),
        ("double_colon_goto", b"::goto::"),
        ("local_fn_goto", b"local function goto() end"),
        ("continue_assign", b"continue = 1"),
        ("keyword_prefix", b"local functionx = 1"),
        ("name_digit", b"local a1 = 1"),
        ("true_false", b"return true, false, nil"),
        (
            "nested_fn_bad",
            b"local f = function(a, ...) return function() return a, ... end end",
        ),
        ("chunk_fn", b"function f() end\nlocal x = f"),
        ("table_trailing", b"return {1, a=2, [3]=4,}"),
        ("field_sep", b"return {1; 2, 3}"),
        ("index_field", b"return a[b].c"),
        ("paren_name", b"return (a)"),
        ("not_name", b"return not a"),
        ("len_neg", b"return -#a"),
        ("and_or_coal", b"return a ?? b or c and d"),
        ("ternary_right", b"return a ? b : c ? d : e"),
        ("short_or", b"local f = |a, b| -> a or b"),
        ("label_until", b"repeat ::a:: until false"),
        ("break_repeat", b"repeat break until false"),
        ("for_scope", b"for i=1,2 do end\nreturn i"),
        ("numeric_shadow", b"for i=1,2 do local i = i end"),
        ("assign_call", b"a, b = f(), g()"),
        ("multi_arg", b"f(g(), h())"),
        ("semi_between", b"local x = 1; local y = 2"),
        ("comment_line", b"-- hi\nlocal x = 1"),
        ("if_only", b"if a then end"),
        ("global_fn", b"function f() end"),
        ("empty_call", b"f()"),
        ("func_empty", b"function f() end"),
        ("while_break", b"while a do break end"),
        ("generic_one", b"for i in a do end"),
        ("local_no_init", b"local a"),
        ("return_nil", b"return nil"),
        ("do_block", b"do local x = 1 end"),
        ("eqeq", b"return a == b"),
        ("ge", b"return a >= b"),
        ("mod", b"return a % b"),
        ("div", b"return a / b"),
        ("xor", b"return a ~ b"),
        ("band", b"return a & b"),
        ("bor", b"return a | b"),
        ("hash", b"return #a"),
        ("method_call", b"a:b(1)"),
        ("empty_long", b"return [[]]"),
        ("eq_level", b"return [==[a]==]"),
        ("esc_x_upper", b"return '\\xFf'"),
        ("esc_decimal_short", b"return '\\9'"),
        ("llu", b"return 1LLu"),
        ("ull_mixed", b"return 1uLl"),
        ("imag_upper", b"return 2I"),
        ("nan_ident", b"return inf"),
        ("zero_b", b"return 0b"),
        ("hex_under", b"return 0x_ff"),
        ("paren_expr", b"(f)"),
        ("last_string_arg", b"return f 'a', g()"),
        ("nav_field", b"return a?.b.c"),
        ("nav_then_index", b"return a?.b[1]"),
        ("short_in_call", b"f(|x| -> x)"),
        ("short_in_table", b"return { |x| -> x }"),
        ("ternary_table", b"return { a ? 1 : 2 }"),
        ("line_then_goto", b"# 40\nlocal goto = 1"),
        ("require_method", b"foo:require('mod')"),
        (
            "require_in_comment_long",
            b"--[[\nrequire('hidden')\n]]\nreturn 1",
        ),
        ("assign_const_name", b"const.x = 1"),
        ("call_const", b"const()"),
        (
            "continue_else",
            b"while true do if a then else continue end end",
        ),
        ("return_in_short", b"local f = || -> do return 1, 2 end"),
        ("shebang_no_nl", b"#!/bin/luajit"),
        ("string_long_mismatch", b"return [=[a]]"),
        ("func_vararg", b"function f(a, ...) end"),
        ("nested_ternary", b"return a ? b ? c : d : e"),
        ("field_keyword", b"return { [end] = 1 }"),
        ("goto_keyword", b"goto end"),
        ("space_coal_assign", b"a ?? = 1"),
        ("compound_and", b"a &&= 1"),
        ("compound_or", b"a ||= 1"),
        ("short_do_empty", b"local f = || -> do end"),
        ("func_self_plain", b"local f = function() return self end"),
        (
            "dots_in_nested",
            b"function f(...) return function() return ... end end",
        ),
        ("label_in_fn", b"function f() ::a:: goto a end"),
        ("break_in_if", b"while true do if a then break end end"),
        ("local_ff", b"local \xff = 1"),
        ("ident_high", b"\x80\x81"),
        ("high_byte_src", b"local \x80x = 1"),
        ("bom_shebang", b"\xef\xbb\xbf#!/bin/luajit\nreturn 1"),
        ("bom_mid", b"return '\xef\xbb\xbf'"),
        ("str_crlf", b"return 'a\\\r\nb'"),
        ("z_ff", b"return 'a\\z\x0cb'"),
        ("for_const_body", b"for i=1,1 do const i = 2 end"),
        ("goto_out_fn", b"function f()\ngoto a\nend\n::a::"),
        ("label_then_local", b"do ::a:: goto a local x = 1 end"),
        ("missing_rhs", b"a +="),
        ("multi_label", b"::a:: ::b::\ngoto a\ngoto b"),
        ("break_label", b"while true do break ::a:: end"),
        ("nav_call2", b"return a?.b?.c()"),
        ("assign_compound_field", b"a.b += 2"),
        ("if_else_bare", b"if a then b elseif c then d else e end"),
        ("pow2", b"return 2^3^2"),
        ("and_or_prec", b"return a or b and c"),
        ("not_and", b"return not a and b"),
        ("shl_add", b"return 1 + 2 << 3"),
        ("eq_and", b"return a == b and c"),
        ("coal_right", b"return a ?? b ?? c"),
        ("and_right", b"return a and b and c"),
        ("concat_pow", b"return a .. b ^ c"),
        ("unclosed_comment_ok_after", b"-- ok\nreturn 1"),
        (
            "local_function_const_rec",
            b"const function f() return f end",
        ),
        ("shadow_nonconst", b"local x = 1\ndo local x = 2 end"),
        ("goto_across_do_local", b"goto a\ndo local x = 1 end\n::a::"),
        ("inner_label_same", b"::lab:: do ::lab:: end"),
        ("continue_in_numeric", b"for i=1,3 do continue end"),
        ("break_in_generic", b"for i in a do break end"),
        ("const_redecl_fn", b"const x = 1\nlocal function x() end"),
        (
            "const_fn_inner",
            b"const x = 1\nlocal function f()\nlocal x = 2\nend",
        ),
        ("assign_method_const", b"const f = 1\nfunction f() end"),
        ("compound_const", b"const a = 1\na += 2"),
        ("index_const_ok", b"const a = {}\na.b += 1"),
        ("short_assign_const", b"const f = 1\nf = || -> 1"),
        (
            "vararg_in_short_outer",
            b"function f(...) return |x| -> ... end",
        ),
        ("goto_repeat_out", b"::a::\nrepeat\n  goto a\nuntil false"),
        (
            "label_in_until_scope",
            b"repeat\n  local x = 1\n  ::a::\nuntil false",
        ),
    ]
}

#[test]
fn differential_matches_locked_luajit() {
    let Some(path) = configured_oracle() else {
        eprintln!(
            "oracle differential not run: LUAJIT_ORACLE unset; cases=0 accept=0 reject=0; not a differential GREEN"
        );
        return;
    };
    assert_oracle_baseline(&path);
    let mut mismatches = Vec::new();
    let mut accept = 0u32;
    let mut reject = 0u32;
    let mut compared = 0u32;
    for (name, source) in cases() {
        compared += 1;
        let ours = syntax_ok(source);
        let theirs = oracle_accepts(&path, source);
        if theirs {
            accept += 1;
        } else {
            reject += 1;
        }
        if ours != theirs {
            mismatches.push(format!(
                "{name}: parser={} oracle={}",
                if ours { "ACCEPT" } else { "REJECT" },
                if theirs { "ACCEPT" } else { "REJECT" }
            ));
        }
    }
    for (name, source, _) in boundary_samples() {
        compared += 1;
        let ours = syntax_ok(&source);
        let theirs = oracle_accepts(&path, &source);
        if theirs {
            accept += 1;
        } else {
            reject += 1;
        }
        if ours != theirs {
            mismatches.push(format!(
                "{name}: parser={} oracle={}",
                if ours { "ACCEPT" } else { "REJECT" },
                if theirs { "ACCEPT" } else { "REJECT" }
            ));
        }
    }
    eprintln!(
        "oracle differential cases={compared} accept={accept} reject={reject} scope=load(..., \"t\") source={ORACLE_SHA}"
    );
    assert!(
        mismatches.is_empty(),
        "syntax disagrees with LuaJIT {ORACLE_SHA}:\n{}",
        mismatches.join("\n")
    );
}

#[test]
fn string_escapes_and_spans_are_consumable() {
    let source = b"return '\\xFF\\z \n\\u{41}'";
    let chunk = parser::parse_chunk(source).expect("parse escapes");
    let Stmt::Return { values, .. } = &chunk.body.stmts[0] else {
        panic!("return");
    };
    let ast::Expr::String { bytes, span, .. } = &values.exprs[0] else {
        panic!("string, got {:?}", values.exprs[0]);
    };
    assert_eq!(bytes, b"\xffA");
    assert_eq!(span.start, 7);
    assert!(span.end > span.start);
}

#[test]
fn paren_discharges_multiple_return() {
    let chunk = parser::parse_chunk(b"return (f()), g(), ...").expect("parse");
    let Stmt::Return { values, .. } = &chunk.body.stmts[0] else {
        panic!("return");
    };
    assert!(!values.exprs[0].is_multi());
    assert!(matches!(values.exprs[0], ast::Expr::Paren { .. }));
    assert!(values.exprs[1].is_multi());
    assert!(values.exprs[2].is_multi());
    assert!(values.tail_multi());
}

#[test]
fn trailing_label_and_jump_into_scope() {
    let ok = parser::parse_chunk(b"goto a\nlocal x = 1\n::a::").expect("trailing");
    let resolution = resolve::resolve(&ok);
    assert!(resolution.errors.is_empty(), "{:?}", resolution.errors);
    assert!(resolution.labels[0].trailing);

    let bad = parser::parse_chunk(b"goto L\nlocal x = 1\n::L::\nreturn x").expect("parse");
    let resolution = resolve::resolve(&bad);
    assert!(
        resolution.errors.iter().any(|err| matches!(
            err,
            resolve::ResolveError::JumpIntoLocal { local_name, .. } if local_name == b"x"
        )),
        "{:?}",
        resolution.errors
    );
}

#[test]
fn const_binding_and_import_discovery() {
    let chunk = parser::parse_chunk(
        b"const x = 1\nrequire('mod.sub')\n-- require('hidden')\nreturn \"require('nope')\"",
    )
    .expect("parse");
    let resolution = resolve::resolve(&chunk);
    assert!(resolution.errors.is_empty(), "{:?}", resolution.errors);
    assert_eq!(resolution.bindings.len(), 1);
    assert!(resolution.bindings[0].const_binding);
    assert_eq!(resolution.bindings[0].name, b"x");
    assert!(matches!(
        &chunk.body.stmts[0],
        ast::Stmt::Local { names, .. } if names[0].name.is("x")
    ));
    assert_eq!(resolution.imports.len(), 1);
    assert_eq!(resolution.imports[0].spec, b"mod.sub");

    let nav = parser::parse_chunk(b"a?.b = 1").expect("nav assign");
    match &nav.body.stmts[0] {
        ast::Stmt::Assign { targets, .. } => {
            assert!(targets[0].has_nav());
            assert!(targets[0].span().end > targets[0].span().start);
        }
        other => panic!("expected assign, got {other:?}"),
    }
}

#[test]
fn bom_is_recorded_and_not_part_of_return() {
    let chunk = parser::parse_chunk(b"\xef\xbb\xbfreturn 1").expect("bom");
    assert!(chunk.has_bom);
    assert!(matches!(chunk.body.stmts[0], ast::Stmt::Return { .. }));
}

fn nested_do(n: usize) -> Vec<u8> {
    let mut source = String::new();
    for _ in 0..n {
        source.push_str("do ");
    }
    for _ in 0..n {
        source.push_str("end ");
    }
    source.into_bytes()
}

fn joined(prefix: &str, item: &str, sep: &str, count: usize) -> Vec<u8> {
    let mut source = String::from(prefix);
    for index in 0..count {
        if index > 0 {
            source.push_str(sep);
        }
        source.push_str(item);
    }
    source.into_bytes()
}

fn labels(count: usize) -> Vec<u8> {
    let mut source = String::new();
    for index in 0..count {
        if index > 0 {
            source.push(' ');
        }
        source.push_str(&format!("::l{index}::"));
    }
    source.into_bytes()
}

fn assignment(count: usize, wrap_do: bool) -> Vec<u8> {
    let mut names = String::new();
    let mut values = String::new();
    for index in 0..count {
        if index > 0 {
            names.push(',');
            values.push(',');
        }
        names.push_str(&format!("a{index}"));
        values.push('1');
    }
    let body = format!("{names} = {values}");
    if wrap_do {
        format!("do {body} end").into_bytes()
    } else {
        body.into_bytes()
    }
}

fn ternary(count: usize) -> Vec<u8> {
    let mut source = String::from("return ");
    for _ in 0..count {
        source.push_str("a?");
    }
    source.push('1');
    for _ in 0..count {
        source.push_str(":0");
    }
    source.into_bytes()
}

fn bits(prefix: &str, count: usize, suffix: &str) -> Vec<u8> {
    format!("return {prefix}{}{suffix}", "1".repeat(count)).into_bytes()
}

/// Boundaries measured against locked LuaJIT 96f7b297 with LUAJIT_ENABLE_LUA52COMPAT.
/// `expect` is load(..., "t") accept. These samples do not spawn that binary.
fn boundary_samples() -> Vec<(String, Vec<u8>, bool)> {
    let mut samples = Vec::new();
    let mut add = |name: &str, source: Vec<u8>, accept: bool| {
        samples.push((name.to_string(), source, accept));
    };
    add("triple_eq", b"a === 1".to_vec(), false);
    add("eq_stmt", b"a == 1".to_vec(), false);
    add("ne_stmt", b"a ~= 1".to_vec(), true);
    add("ne_expr", b"return a ~= b".to_vec(), true);
    add("add_assign", b"a += 1".to_vec(), true);
    add("add_assign_space", b"a + = 1".to_vec(), false);
    add(
        "repeat_continue_early",
        b"repeat do continue end local x = 1 until true".to_vec(),
        false,
    );
    add(
        "repeat_continue_late",
        b"repeat local x = 1 do continue end until true".to_vec(),
        true,
    );
    add(
        "while_continue_early",
        b"while true do do continue end local x = 1 end".to_vec(),
        true,
    );
    add(
        "repeat_if_continue_early",
        b"repeat if true then continue end local x = 1 until true".to_vec(),
        false,
    );
    add(
        "repeat_if_continue_late",
        b"repeat local x = 1 if true then continue end until true".to_vec(),
        true,
    );
    add(
        "repeat_continue_then_second_local",
        b"repeat local x = 1 do continue end local y = 2 until true".to_vec(),
        false,
    );
    add(
        "dec_ll_max",
        b"return 18446744073709551615LL".to_vec(),
        true,
    );
    add(
        "dec_ll_over",
        b"return 18446744073709551616LL".to_vec(),
        false,
    );
    add(
        "dec_ull_max",
        b"return 18446744073709551615ULL".to_vec(),
        true,
    );
    add(
        "dec_ull_over",
        b"return 18446744073709551616ULL".to_vec(),
        false,
    );
    add(
        "dec_ll_neg_max",
        b"return -18446744073709551615LL".to_vec(),
        true,
    );
    add(
        "dec_ll_neg_over",
        b"return -18446744073709551616LL".to_vec(),
        false,
    );
    add(
        "dec_ll_leading_zero_max",
        b"return 00018446744073709551615LL".to_vec(),
        true,
    );
    add(
        "dec_ll_leading_zero_over",
        b"return 00018446744073709551616LL".to_vec(),
        false,
    );
    add(
        "dec_ll_20_under",
        b"return 10000000000000000000LL".to_vec(),
        true,
    );
    add(
        "dec_unsuffixed_over",
        b"return 18446744073709551616".to_vec(),
        true,
    );
    add(
        "hex_ll_16",
        format!("return 0x{}LL", "F".repeat(16)).into_bytes(),
        true,
    );
    add(
        "hex_ll_17",
        format!("return 0x{}LL", "F".repeat(17)).into_bytes(),
        false,
    );
    add(
        "hex_ll_leading_zero_16",
        format!("return 0x0000{}LL", "F".repeat(16)).into_bytes(),
        true,
    );
    add(
        "hex_ll_leading_zero_17",
        format!("return 0x0000{}LL", "F".repeat(17)).into_bytes(),
        false,
    );
    add("bin_64", bits("0b", 64, ""), true);
    add("bin_65", bits("0b", 65, ""), false);
    add("bin_ll_64", bits("0b", 64, "LL"), true);
    add("bin_ll_65", bits("0b", 65, "LL"), false);
    add("bin_ll_leading_zero_64", bits("0b000", 64, "LL"), true);
    add("bin_ll_leading_zero_65", bits("0b000", 65, "LL"), false);
    add("do_198", nested_do(198), true);
    add("do_199", nested_do(199), false);
    add("add_400", joined("return ", "1", "+", 400), true);
    add("pow_198", joined("return ", "2", "^", 198), true);
    add("pow_199", joined("return ", "2", "^", 199), false);
    add("cat_198", joined("return ", "'a'", "..", 198), true);
    add("cat_199", joined("return ", "'a'", "..", 199), false);
    add("tern_197", ternary(197), true);
    add("tern_198", ternary(198), false);
    add("lab_199", labels(199), true);
    add("lab_200", labels(200), false);
    add(
        "lab_do_198",
        format!("do {} end", String::from_utf8(labels(198)).unwrap()).into_bytes(),
        true,
    );
    add(
        "lab_do_199",
        format!("do {} end", String::from_utf8(labels(199)).unwrap()).into_bytes(),
        false,
    );
    add("asg_199", assignment(199, false), true);
    add("asg_200", assignment(200, false), false);
    add("asg_do_198", assignment(198, true), true);
    add("asg_do_199", assignment(199, true), false);
    samples
}

#[test]
fn locked_luajit_syntax_boundaries() {
    let mut failed = Vec::new();
    for (name, source, expect) in boundary_samples() {
        let got = syntax_ok(&source);
        if got != expect {
            failed.push(format!(
                "{name}: parser {} expected {}",
                if got { "ACCEPT" } else { "REJECT" },
                if expect { "ACCEPT" } else { "REJECT" }
            ));
        }
    }
    assert!(
        failed.is_empty(),
        "locked syntax boundary mismatch (no external oracle):\n{}",
        failed.join("\n")
    );
}
