//! Writable canonical builtin bindings, through issued interpreter VMs.
use pd_vm_compat_frontends::{JAVASCRIPT_ES5_PLUGIN, javascript_es5_flavor};
use vm::{HostFunctionRegistry, Value, VmStatus};

fn full_upper_case(source: &str) {
    let result = run(source);
    assert_eq!(field(&result, "tag"), Value::Int(2));
    assert_eq!(field(&result, "flag"), Value::Bool(true));
    let script = format!(
        "const source={};console.log(JSON.stringify({{value:(0,eval)(source)}}));",
        serde_json::to_string(source).unwrap()
    );
    let version = std::process::Command::new("node")
        .arg("--version")
        .output()
        .unwrap();
    let output = std::process::Command::new("timeout")
        .args(["15", "node", "-e", &script])
        .output()
        .unwrap();
    eprintln!(
        "URI123_NODE {}",
        serde_json::json!({"source":source,"version_rc":version.status.code(),"version":String::from_utf8_lossy(&version.stdout),"argv":["timeout","15","node","-e",script],"rc":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr)})
    );
    assert!(version.status.success() && output.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!({"value":true})
    );
}
#[test]
fn full_uri123_upper_canonical_00() {
    full_upper_case("encodeURI=7;encodeURI===7;");
}
#[test]
fn full_uri123_upper_canonical_01() {
    full_upper_case("decodeURI=9;decodeURI===9;");
}
#[test]
fn full_uri123_upper_canonical_02() {
    full_upper_case(
        "var saved=encodeURI;encodeURI=decodeURI;saved('A B')==='A%20B'&&encodeURI('%41')==='A';",
    );
}
#[test]
fn full_uri123_upper_canonical_03() {
    full_upper_case(
        "var saved=decodeURI;decodeURI=encodeURI;saved('%2f%41')==='%2fA'&&decodeURI('A B')==='A%20B';",
    );
}
#[test]
fn full_uri123_upper_canonical_04() {
    full_upper_case(
        "var a=encodeURI;var b=decodeURI;encodeURI=b;decodeURI=a;encodeURI('%41')==='A'&&decodeURI('A B')==='A%20B';",
    );
}
#[test]
fn full_uri123_upper_canonical_05() {
    full_upper_case(
        "function read(){return encodeURI;}var saved=read();encodeURI=decodeURI;read()===decodeURI&&read()('%41')==='A'&&saved('A B')==='A%20B';",
    );
}
#[test]
fn full_uri123_upper_canonical_06() {
    full_upper_case(
        "function read(){return decodeURI;}var saved=read();decodeURI=encodeURI;read()===encodeURI&&read()('A B')==='A%20B'&&saved('%2f')==='%2f';",
    );
}
#[test]
fn full_uri123_upper_canonical_07() {
    full_upper_case(
        "function f(){return encodeURI('A B')==='A%20B'&&decodeURI('%41')==='A';}function g(encodeURI,decodeURI){encodeURI=7;decodeURI=9;return encodeURI===7&&decodeURI===9;}f()&&g(1,2);",
    );
}
#[test]
fn full_uri123_upper_canonical_08() {
    full_upper_case(
        "var O=Object;var M=Math;var e=encodeURIComponent;var d=decodeURIComponent;var a=encodeURI;var b=decodeURI;function read(){return encodeURI('%41')==='A'&&decodeURI('A B')==='A%20B';}Object=1;Math=2;encodeURIComponent=3;decodeURIComponent=4;encodeURI=b;decodeURI=a;read()&&e('/')==='%2F'&&d('%2f')==='/'&&O.getPrototypeOf(a)===O.getPrototypeOf(O)&&M.abs(-3)===3;",
    );
}

fn field(value: &Value, name: &str) -> Value {
    let Value::Map(map) = value else {
        panic!("box: {value:?}")
    };
    map.get(&Value::string(name)).expect("field").clone()
}
#[derive(Debug)]
struct Execution {
    stack: Vec<Value>,
    resources: usize,
    refused: bool,
}
fn run(source: &str) -> Value {
    execute(source, None)
        .stack
        .last()
        .expect("completion")
        .clone()
}
fn execute(source: &str, probe: Option<Probe>) -> Execution {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            eprintln!("GLOBAL36_SOURCE {source}");
            let catalog =
                pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
            let plugin: &'static dyn vm::SourcePlugin = if let Some(p) = &probe {
                Box::leak(Box::new(p.clone()))
            } else {
                &JAVASCRIPT_ES5_PLUGIN
            };
            let options = vm::CompileSourceFileOptions::new()
                .with_host_api_catalog(catalog.clone())
                .with_source_plugin(plugin);
            let flavor = if probe.is_some() {
                vm::SourceFlavor::Plugin(vm::FrontendId::new("javascript-es5-global-observer"))
            } else {
                javascript_es5_flavor()
            };
            let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
                &source, flavor, options,
            )
            .unwrap_or_else(|e| panic!("source must compile: {source}\n{e:?}"));
            let mut machine = compiled.into_vm().expect("issued VM");
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..vm::JitConfig::default()
            });
            pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
                &mut machine,
                pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
            )
            .unwrap();
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
            for name in [
                "copysign",
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
                "is_finite",
                "is_infinite",
            ] {
                registry.allow_builtin(format!("math::{name}")).unwrap();
            }
            registry.allow_builtin("bytes::from_utf8").unwrap();
            registry.bind_vm_cached(&mut machine).unwrap();
            machine.set_fuel(10_000_000);
            let status = machine.run();
            let refused = status.is_err();
            eprintln!("GLOBAL38_STATUS {status:?}");
            if probe.as_ref().is_some_and(|p| p.int_negative || p.stale || p.bad_transport.is_some()) {
                assert!(refused, "actual guarded consumer must reject: {status:?}");
                assert!(matches!(&status,Err(vm::VmError::HostError(message)) if message=="assertion failed"));
            } else {
                assert_eq!(
                    status.unwrap_or_else(|e| panic!("{source}: {e:?}")),
                    VmStatus::Halted
                );
            }
            assert!(!machine.jit_config().enabled);
            assert_eq!(
                (machine.jit_native_exec_count(), machine.aot_exec_count()),
                (0, 0)
            );
            assert!(
                machine
                    .capture_certification_stats()
                    .is_some_and(|s| s.valid)
            );
            let result = Execution {
                stack: machine.stack().to_vec(),
                resources: machine.host_context().resource_count(),
                refused,
            };
            eprintln!("GLOBAL38_RESULT {result:?} native=0 jit=false");
            machine.reset_for_reuse().unwrap();
            assert_eq!(machine.host_context().resource_count(), 0);
            assert!(
                pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(&mut machine)
                    .is_none()
            );
            assert!(
                machine
                    .capture_certification_stats()
                    .is_some_and(|s| !s.valid)
            );
            result
        })
        .unwrap()
        .join()
        .unwrap()
}

// Complete parser output, with only unordered maps presented in key order.
// Float leaves additionally carry raw bits at their deterministic AST path.
fn snapshot(ir: &vm::FrontendIr) -> String {
    use std::collections::BTreeMap;
    use std::fmt::Write;
    let mut rest = ir.clone();
    let implementations: BTreeMap<_, _> = std::mem::take(&mut rest.function_impls)
        .into_iter()
        .collect();
    let schemas: BTreeMap<_, _> = std::mem::take(&mut rest.struct_schemas)
        .into_iter()
        .collect();
    let sources: BTreeMap<_, _> = std::mem::take(&mut rest.function_sources)
        .into_iter()
        .collect();
    let mut text = format!(
        "{rest:#?}\nimplementations={implementations:#?}\nschemas={schemas:#?}\nsources={sources:#?}\n"
    );
    fn expr(e: &vm::Expr, path: &str, out: &mut String) {
        use vm::Expr as E;
        match e {
            E::Float(n) => {
                writeln!(out, "{path} FloatBits={:016x}", n.to_bits()).unwrap();
            }
            E::Add(a, b)
            | E::Sub(a, b)
            | E::Mul(a, b)
            | E::Div(a, b)
            | E::Mod(a, b)
            | E::Eq(a, b)
            | E::Lt(a, b)
            | E::Gt(a, b)
            | E::And(a, b)
            | E::Or(a, b) => {
                expr(a, &format!("{path}/left"), out);
                expr(b, &format!("{path}/right"), out);
            }
            E::Neg(a) | E::Not(a) | E::ToOwned(a) | E::Borrow(a) | E::BorrowMut(a) => {
                expr(a, &format!("{path}/operand"), out)
            }
            E::Call(_, _, args, _, _)
            | E::LocalCall(_, _, args, _)
            | E::ModuleCall(_, _, args, _) => {
                for (i, a) in args.iter().enumerate() {
                    expr(a, &format!("{path}/arg{i}"), out);
                }
            }
            E::IfElse {
                condition,
                then_expr,
                else_expr,
            } => {
                expr(condition, &format!("{path}/condition"), out);
                expr(then_expr, &format!("{path}/then"), out);
                expr(else_expr, &format!("{path}/else"), out);
            }
            E::Block { stmts, expr: e } => {
                statements(stmts, path, out);
                expr(e, &format!("{path}/tail"), out);
            }
            E::ResultConstruct { payload, .. } => expr(payload, &format!("{path}/payload"), out),
            E::ResultQuery { value, .. } | E::ResultTry { value, .. } => {
                expr(value, &format!("{path}/value"), out)
            }
            E::ResultMatch {
                value,
                ok_expr,
                err_expr,
                ..
            } => {
                expr(value, &format!("{path}/value"), out);
                expr(ok_expr, &format!("{path}/ok"), out);
                expr(err_expr, &format!("{path}/err"), out);
            }
            E::OptionalGet { container, key, .. } => {
                expr(container, &format!("{path}/container"), out);
                expr(key, &format!("{path}/key"), out);
            }
            E::OptionUnwrapOr {
                value, fallback, ..
            } => {
                expr(value, &format!("{path}/value"), out);
                expr(fallback, &format!("{path}/fallback"), out);
            }
            E::Match {
                value,
                arms,
                default,
                ..
            } => {
                expr(value, &format!("{path}/value"), out);
                for (i, (_, a)) in arms.iter().enumerate() {
                    expr(a, &format!("{path}/arm{i}"), out);
                }
                expr(default, &format!("{path}/default"), out);
            }
            E::Null
            | E::Int(_)
            | E::Bool(_)
            | E::String(_)
            | E::Bytes(_)
            | E::Var(_)
            | E::MoveVar(_)
            | E::MoveField { .. }
            | E::MoveIndex { .. }
            | E::FunctionRef(..)
            | E::ModuleFunctionRef(..)
            | E::UnresolvedFunctionRef { .. } => {}
            E::Closure(c) => expr(&c.body, &format!("{path}/closure/body"), out),
            E::ClosureCall(c, args) => {
                expr(&c.body, &format!("{path}/closure/body"), out);
                for (i, a) in args.iter().enumerate() {
                    expr(a, &format!("{path}/arg{i}"), out);
                }
            }
        }
    }
    fn statements(ss: &[vm::Stmt], path: &str, out: &mut String) {
        use vm::Stmt as S;
        for (i, s) in ss.iter().enumerate() {
            let p = format!("{path}/stmt{i}");
            match s {
                S::Let { expr: e, .. } | S::Assign { expr: e, .. } | S::Expr { expr: e, .. } => {
                    expr(e, &p, out)
                }
                S::IfElse {
                    condition,
                    then_branch,
                    else_branch,
                    ..
                } => {
                    expr(condition, &format!("{p}/condition"), out);
                    statements(then_branch, &format!("{p}/then"), out);
                    statements(else_branch, &format!("{p}/else"), out);
                }
                S::While {
                    condition, body, ..
                } => {
                    expr(condition, &format!("{p}/condition"), out);
                    statements(body, &format!("{p}/body"), out);
                }
                S::For {
                    init,
                    condition,
                    post,
                    body,
                    ..
                } => {
                    statements(std::slice::from_ref(init), &format!("{p}/init"), out);
                    expr(condition, &format!("{p}/condition"), out);
                    statements(std::slice::from_ref(post), &format!("{p}/post"), out);
                    statements(body, &format!("{p}/body"), out);
                }
                S::ClosureLet { closure, .. } => {
                    expr(&closure.body, &format!("{p}/closure/body"), out)
                }
                S::Noop { .. }
                | S::FuncDecl { .. }
                | S::Break { .. }
                | S::Continue { .. }
                | S::Drop { .. } => {}
            }
        }
    }
    statements(&ir.stmts, "script", &mut text);
    for (id, body) in implementations {
        statements(&body.body_stmts, &format!("function{id}"), &mut text);
        expr(&body.body_expr, &format!("function{id}/tail"), &mut text);
    }
    text
}

// Narrow byte-preserving observer for Rust Debug Object({quoted_key: value}).
// Only complete entries move. Commas/whitespace remain in their original slots;
// every emitted range refers to the input, so an exact inverse is checkable.
mod debug_observer {
    #[derive(Default)]
    struct Map {
        start: usize,
        end: usize,
        commas: Vec<usize>,
        children: Vec<usize>,
        entries: Vec<Entry>,
        tail: (usize, usize),
        permutation: Vec<usize>,
    }
    struct Entry {
        lo: usize,
        key: usize,
        end: usize,
        hi: usize,
        decoded: Vec<u8>,
    }
    struct Delimiter {
        kind: u8,
        map: Option<usize>,
        object: Option<usize>,
    }
    pub(super) struct Normalized {
        pub data: Vec<u8>,
        provenance: Vec<(usize, usize)>,
        maps: Vec<Map>,
        nonmap_objects: usize,
    }
    fn ws(b: u8) -> bool {
        matches!(b, b' ' | b'\t' | b'\r' | b'\n')
    }
    fn blank(data: &[u8]) -> bool {
        data.iter().all(|&b| ws(b))
    }
    fn quoted_end(data: &[u8], start: usize) -> Result<usize, String> {
        let mut i = start + 1;
        while i < data.len() {
            match data[i] {
                b'"' => return Ok(i + 1),
                b'\\' => i += 2,
                _ => i += 1,
            }
        }
        Err(format!("unterminated quoted string at {start}"))
    }
    fn decode(raw: &[u8]) -> Result<Vec<u8>, String> {
        let s = std::str::from_utf8(&raw[1..raw.len() - 1]).map_err(|e| e.to_string())?;
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                if (c as u32) < 32 {
                    return Err("unescaped key control character".into());
                }
                out.push(c);
                continue;
            }
            let c = match chars.next() {
                Some('n') => '\n',
                Some('r') => '\r',
                Some('t') => '\t',
                Some('0') => '\0',
                Some('\\') => '\\',
                Some('"') => '"',
                Some('\'') => '\'',
                Some('u') => {
                    if chars.next() != Some('{') {
                        return Err("missing unicode opener".into());
                    }
                    let mut digits = String::new();
                    loop {
                        match chars.next() {
                            Some('}') if !digits.is_empty() => break,
                            Some(d) if d.is_ascii_hexdigit() && digits.len() < 6 => digits.push(d),
                            _ => return Err("invalid Rust unicode escape".into()),
                        }
                    }
                    let n = u32::from_str_radix(&digits, 16).map_err(|e| e.to_string())?;
                    char::from_u32(n).ok_or("invalid Unicode scalar")?
                }
                _ => return Err("unsupported Rust key escape".into()),
            };
            out.push(c);
        }
        Ok(out.into_bytes())
    }
    fn entries(data: &[u8], node: &mut Map) -> Result<(), String> {
        let mut start = node.start + 1;
        let limit = node.end - 1;
        let mut ends: Vec<_> = node.commas.iter().map(|&p| (p, p + 1)).collect();
        if ends.last().is_none_or(|&(_, hi)| !blank(&data[hi..limit])) {
            ends.push((limit, limit));
        }
        let mut seen = std::collections::BTreeSet::new();
        for (end, hi) in ends {
            if blank(&data[start..end]) {
                if !node.commas.is_empty() {
                    return Err("missing map entry/key".into());
                }
                node.tail = (start, limit);
                return Ok(());
            }
            let mut left = start;
            let mut right = end;
            while ws(data[left]) {
                left += 1;
            }
            while ws(data[right - 1]) {
                right -= 1;
            }
            if data[left] != b'"' {
                return Err("Object map entry lacks quoted key".into());
            }
            let key_end = quoted_end(data, left)?;
            if key_end > right {
                return Err("key exceeds map entry".into());
            }
            let decoded = decode(&data[left..key_end])?;
            if !seen.insert(decoded.clone()) {
                return Err("duplicate decoded map key".into());
            }
            let mut colon = key_end;
            while colon < right && ws(data[colon]) {
                colon += 1;
            }
            if colon >= right || data[colon] != b':' || blank(&data[colon + 1..right]) {
                return Err("missing map colon/value".into());
            }
            node.entries.push(Entry {
                lo: start,
                key: left,
                end: right,
                hi,
                decoded,
            });
            start = hi;
        }
        node.tail = (start, limit);
        Ok(())
    }
    fn emit(pieces: &mut Vec<(usize, usize)>, a: usize, b: usize) {
        if a != b {
            pieces.push((a, b));
        }
    }
    fn region(
        a: usize,
        b: usize,
        children: &[usize],
        maps: &[Map],
        pieces: &mut Vec<(usize, usize)>,
    ) {
        let mut pos = a;
        for &id in children {
            let child = &maps[id];
            if a <= child.start && child.end <= b {
                emit(pieces, pos, child.start);
                render(id, maps, pieces);
                pos = child.end;
            }
        }
        emit(pieces, pos, b);
    }
    fn render(id: usize, maps: &[Map], pieces: &mut Vec<(usize, usize)>) {
        let node = &maps[id];
        emit(pieces, node.start, node.start + 1);
        for (slot, &original) in node.entries.iter().zip(&node.permutation) {
            let picked = &node.entries[original];
            emit(pieces, slot.lo, slot.key);
            region(picked.key, picked.end, &node.children, maps, pieces);
            emit(pieces, slot.end, slot.hi);
        }
        emit(pieces, node.tail.0, node.tail.1);
        emit(pieces, node.end - 1, node.end);
    }
    pub(super) fn normalize(data: &[u8], reverse: bool) -> Result<Normalized, String> {
        let mut stack: Vec<Delimiter> = Vec::new();
        let mut maps: Vec<Map> = Vec::new();
        let mut roots = Vec::new();
        let mut active: Vec<usize> = Vec::new();
        let mut recent: Vec<(u8, usize, usize)> = Vec::new();
        let mut objects = 0;
        let mut a = 0;
        while a < data.len() {
            let raw = data[a];
            let (kind, b) = if raw == b'"' {
                (b'Q', quoted_end(data, a)?)
            } else if data[a..].starts_with(b"Object")
                && (a == 0 || !(data[a - 1].is_ascii_alphanumeric() || data[a - 1] == b'_'))
                && data
                    .get(a + 6)
                    .is_none_or(|c| !(c.is_ascii_alphanumeric() || *c == b'_'))
            {
                (b'O', a + 6)
            } else if b"()[]{},:".contains(&raw) {
                (raw, a + 1)
            } else {
                a += 1;
                continue;
            };
            match kind {
                b'(' | b'[' | b'{' => {
                    if kind == b'('
                        && recent
                            .last()
                            .is_some_and(|&(k, _, end)| k == b'O' && blank(&data[end..a]))
                    {
                        objects += 1;
                    }
                    let mut map = None;
                    if kind == b'{'
                        && recent.len() == 2
                        && recent[0].0 == b'O'
                        && recent[1].0 == b'('
                        && blank(&data[recent[0].2..recent[1].1])
                        && blank(&data[recent[1].2..a])
                    {
                        let id = maps.len();
                        if let Some(&parent) = active.last() {
                            maps[parent].children.push(id);
                        } else {
                            roots.push(id);
                        }
                        maps.push(Map {
                            start: a,
                            ..Map::default()
                        });
                        active.push(id);
                        let payload = stack.last_mut().ok_or("missing Object payload opener")?;
                        if payload.kind != b'(' {
                            return Err("missing Object payload opener".into());
                        }
                        payload.object = Some(id);
                        map = Some(id);
                    }
                    stack.push(Delimiter {
                        kind,
                        map,
                        object: None,
                    });
                }
                b')' | b']' | b'}' => {
                    let opening = stack.pop().ok_or("unmatched closer")?;
                    if !matches!(
                        (opening.kind, kind),
                        (b'(', b')') | (b'[', b']') | (b'{', b'}')
                    ) {
                        return Err("mismatched delimiter".into());
                    }
                    if let Some(id) = opening.map {
                        maps[id].end = b;
                        assert_eq!(active.pop(), Some(id));
                        entries(data, &mut maps[id])?;
                    }
                    if let Some(id) = opening.object {
                        let gap = &data[maps[id].end..a];
                        let nonwhite: Vec<_> = gap.iter().copied().filter(|&c| !ws(c)).collect();
                        if !nonwhite.is_empty() && nonwhite != b"," {
                            return Err("ambiguous Object tuple payload".into());
                        }
                    }
                }
                b',' => {
                    if let Some(id) = stack.last().and_then(|d| d.map) {
                        maps[id].commas.push(a);
                    }
                }
                _ => {}
            }
            recent.push((kind, a, b));
            if recent.len() > 2 {
                recent.remove(0);
            }
            a = b;
        }
        if !stack.is_empty() {
            return Err("unclosed delimiter".into());
        }
        for map in &mut maps {
            let mut order: Vec<_> = (0..map.entries.len()).collect();
            order.sort_by(|&a, &b| map.entries[a].decoded.cmp(&map.entries[b].decoded));
            if reverse {
                order.reverse();
            }
            map.permutation = order;
        }
        let mut provenance = Vec::new();
        region(0, data.len(), &roots, &maps, &mut provenance);
        let mut canonical = Vec::with_capacity(data.len());
        for &(a, b) in &provenance {
            canonical.extend_from_slice(&data[a..b]);
        }
        assert_eq!(canonical.len(), data.len(), "byte length changed");
        let nonmap_objects = objects - maps.len();
        Ok(Normalized {
            data: canonical,
            provenance,
            maps,
            nonmap_objects,
        })
    }
    impl Normalized {
        fn inverse(&self) -> Vec<u8> {
            let mut chunks = Vec::new();
            let mut offset = 0;
            for &(a, b) in &self.provenance {
                assert!(b >= a);
                chunks.push((a, b, &self.data[offset..offset + b - a]));
                offset += b - a;
            }
            assert_eq!(offset, self.data.len());
            chunks.sort_by_key(|c| c.0);
            let mut expected = 0;
            let mut original = Vec::with_capacity(offset);
            for (a, b, bytes) in chunks {
                assert_eq!(a, expected, "not an exact disjoint input partition");
                expected = b;
                original.extend_from_slice(bytes);
            }
            assert_eq!(expected, offset);
            original
        }
        pub(super) fn receipt(&self) -> String {
            let permutations: Vec<_> = self.maps.iter().map(|m| &m.permutation).collect();
            format!(
                "bytes={} maps={} entries={} nonmap_objects={}\nprovenance={:?}\npermutations={permutations:?}\n",
                self.data.len(),
                self.maps.len(),
                self.maps.iter().map(|m| m.entries.len()).sum::<usize>(),
                self.nonmap_objects,
                self.provenance
            )
        }
    }
    pub(super) fn checked(data: &[u8]) -> Normalized {
        let normalized = normalize(data, false).expect("frozen Debug grammar");
        assert_eq!(normalized.inverse(), data, "exact byte inverse");
        assert_eq!(
            normalize(&normalized.data, false).unwrap().data,
            normalized.data,
            "idempotence"
        );
        let reversed = normalize(data, true).unwrap();
        assert_eq!(reversed.inverse(), data, "reverse exact inverse");
        assert_eq!(
            normalize(&reversed.data, false).unwrap().data,
            normalized.data,
            "reverse permutation invariance"
        );
        normalized
    }
    pub(super) fn self_checks() {
        let x = "pre [1, 2] Object({\"z\": Int, \"a\": Object({\"y\": Float, \"b\": Bool})}) post FloatBits=0000000000000000\n";
        let y = "pre [1, 2] Object({\"a\": Object({\"b\": Bool, \"y\": Float}), \"z\": Int}) post FloatBits=0000000000000000\n";
        let n = checked(x.as_bytes());
        assert_eq!(n.data, checked(y.as_bytes()).data);
        checked(br#"Object({"q\"}\\": Object({"\u{61}": Int, "\0": Float}), "a": Bool})"#);
        let mut count = 4;
        for (name, from, to) in [
            ("field_type", "\"z\": Int", "\"z\": Float"),
            ("field_name", "\"z\": Int", "\"zz\": Int"),
            ("field_value", "\"z\": Int", "\"z\": Int(9)"),
            ("missing_field", "\"z\": Int, ", ""),
            ("vector_order", "[1, 2]", "[2, 1]"),
            (
                "signed_zero_sidecar",
                "FloatBits=0000000000000000",
                "FloatBits=8000000000000000",
            ),
            (
                "nan_payload_sidecar",
                "FloatBits=0000000000000000",
                "FloatBits=7ff8123456789abc",
            ),
            ("whitespace", "pre ", "pre  "),
            ("nonmap_punctuation", "post ", "post; "),
        ] {
            assert_ne!(
                n.data,
                checked(x.replace(from, to).as_bytes()).data,
                "{name}"
            );
            count += 1;
        }
        for (name, a, b) in [
            ("parameter_order", "params: [2, 7]", "params: [7, 2]"),
            (
                "statement_order",
                "stmts: [Int(1), Int(2)]",
                "stmts: [Int(2), Int(1)]",
            ),
            (
                "argument_order",
                "Call(1, [Int(2), Int(7)])",
                "Call(1, [Int(7), Int(2)])",
            ),
            (
                "nan_payloads",
                "Float(NaN) FloatBits=7ff8123456789abc",
                "Float(NaN) FloatBits=7ff8123456789abd",
            ),
            (
                "nan_sign",
                "Float(NaN) FloatBits=7ff8123456789abc",
                "Float(NaN) FloatBits=fff8123456789abc",
            ),
            (
                "object_literal_vector",
                "Object([(\"z\",Int), (\"a\",Bool)])",
                "Object([(\"a\",Bool), (\"z\",Int)])",
            ),
            (
                "unrelated_map",
                "{\"z\": Int, \"a\": Bool}",
                "{\"a\": Bool, \"z\": Int}",
            ),
        ] {
            assert_ne!(
                checked(a.as_bytes()).data,
                checked(b.as_bytes()).data,
                "{name}"
            );
            count += 1;
        }
        for bad in [
            r#"Object({"a": Int, "a": Bool})"#,
            r#"Object({"a": Int, "\u{61}": Bool})"#,
            r#"Object({: Int})"#,
            r#"Object({"a": })"#,
            r#"Object({"a" Int})"#,
            r#"Object({,"a": Int})"#,
            r#"Object({"a": Int)"#,
            r#"Object({"a": Int}, 7)"#,
            r#"Object({"\w": Int})"#,
            r#"Object({"a: Int})"#,
            r#"Object({"\u{d800}": Int})"#,
            r#"Object({"\u{110000}": Int})"#,
            r#"Object({"\u{}": Int})"#,
        ] {
            assert!(
                normalize(bad.as_bytes(), false).is_err(),
                "must reject {bad}"
            );
            count += 1;
        }
        let opaque = br#"String("Object({\"b\": Int, \"a\": Bool})")"#;
        assert_eq!(checked(opaque).data, opaque);
        checked(b"Object( {  } , )");
        count += 2;
        eprintln!("GLOBAL40_OBSERVER_SELF_CHECKS count={count} all_passed=true");
    }
    pub(super) fn actual_checks(data: &[u8], n: &Normalized) {
        let text = std::str::from_utf8(data).unwrap();
        for (name, changed) in [
            (
                "field_type",
                text.replacen("\"number\": Float", "\"number\": Int", 1),
            ),
            (
                "missing_field",
                text.replacen("                \"number\": Float,\n", "", 1),
            ),
        ] {
            assert_ne!(changed.as_bytes(), data, "mutation applied {name}");
            assert_ne!(
                normalize(changed.as_bytes(), false).unwrap().data,
                n.data,
                "actual00 {name}"
            );
        }
        let at = text.find("FloatBits=").unwrap() + "FloatBits=".len();
        let bits = u64::from_str_radix(&text[at..at + 16], 16).unwrap();
        let changed = format!(
            "{}{:016x}{}",
            &text[..at],
            bits ^ (1 << 63),
            &text[at + 16..]
        );
        assert_ne!(
            normalize(changed.as_bytes(), false).unwrap().data,
            n.data,
            "actual00 FloatBits sign"
        );
        let map = &n.maps[0];
        assert_eq!(map.entries.len(), 5);
        fn permutations(order: &mut [usize], at: usize, f: &mut impl FnMut(&[usize])) {
            if at == order.len() {
                f(order);
            } else {
                for i in at..order.len() {
                    order.swap(at, i);
                    permutations(order, at + 1, f);
                    order.swap(at, i);
                }
            }
        }
        let mut count = 0;
        permutations(&mut [0, 1, 2, 3, 4], 0, &mut |order| {
            let mut permuted = data[..map.start + 1].to_vec();
            for (slot, &picked) in map.entries.iter().zip(order) {
                permuted.extend_from_slice(&data[slot.lo..slot.key]);
                let entry = &map.entries[picked];
                permuted.extend_from_slice(&data[entry.key..entry.end]);
                permuted.extend_from_slice(&data[slot.end..slot.hi]);
            }
            permuted.extend_from_slice(&data[map.tail.0..map.tail.1]);
            permuted.extend_from_slice(&data[map.end - 1..]);
            let p = normalize(&permuted, false).unwrap();
            assert_eq!(p.data, n.data);
            assert_eq!(p.inverse(), permuted);
            count += 1;
        });
        assert_eq!(count, 120);
        eprintln!("GLOBAL40_ACTUAL00 mutations=3 permutations={count} inverse_exact=true");
    }
}

#[test]
fn inactive_ir_lossless() {
    debug_observer::self_checks();
    let source = || {
        let mut v = vec!["1+2;".to_owned(), "NaN; Infinity; -0;".to_owned(), "'use strict'; function f(Math){var parseInt;try {throw 1;} catch(Object) {Object;}return Math;} f(true);".to_owned(), "Object.prototype.constructor === Object;".to_owned(), "'use strict'; var f = function(x){return x;}; f(1);".to_owned()];
        for mask in 0..32 {
            let mut s = String::new();
            for (i, name) in ["Math", "parseInt", "parseFloat", "isNaN", "isFinite"]
                .iter()
                .enumerate()
            {
                if mask & (1 << i) != 0 {
                    s.push_str(name);
                    s.push(';');
                }
            }
            if s.is_empty() {
                s.push_str("0;");
            }
            v.push(s);
        }
        v
    };
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let context = vm::SourceFrontendContext {
        source_id: 0,
        original_path: None,
        phase: vm::compiler::ParsePhase::Standalone,
        host_api_catalog: Some(&catalog),
        imports: &[],
    };
    let root = std::path::PathBuf::from(std::env::var_os("TMPDIR").expect("assigned TMPDIR"))
        .join("inactive-ir");
    std::fs::create_dir(&root).unwrap();
    for (i, s) in source().iter().enumerate() {
        let ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, s, &context)
            .unwrap_or_else(|e| panic!("{s}: {e:?}"));
        let text = snapshot(&ir);
        std::fs::write(root.join(format!("{i:02}.debug")), &text).unwrap();
        std::fs::write(root.join(format!("{i:02}.js")), s).unwrap();
        let current = debug_observer::checked(text.as_bytes());
        std::fs::write(
            root.join(format!("{i:02}.current.observer")),
            current.receipt(),
        )
        .unwrap();
        if let Some(before) = std::env::var_os("GLOBAL36_INACTIVE_BASELINE") {
            let before = std::path::PathBuf::from(before);
            assert_eq!(
                std::fs::read_to_string(before.join(format!("{i:02}.js"))).unwrap(),
                *s,
                "same original subject source"
            );
            let raw = std::fs::read(before.join(format!("{i:02}.debug"))).unwrap();
            let original = debug_observer::checked(&raw);
            std::fs::write(
                root.join(format!("{i:02}.before.observer")),
                original.receipt(),
            )
            .unwrap();
            if i == 0 {
                debug_observer::actual_checks(&raw, &original);
            }
            assert_eq!(original.data, current.data, "inactive full IR {s}");
            eprintln!(
                "GLOBAL40_INACTIVE_EQUAL subject={i:02} before_bytes={} current_bytes={} inverse_exact=true idempotent=true reverse_permutation=true full_canonical_equal=true",
                raw.len(),
                text.len()
            );
        }
    }
    eprintln!(
        "GLOBAL36_INACTIVE_COUNT {} path={}",
        source().len(),
        root.display()
    );
}

#[test]
fn direct_builtin_assignment() {
    assert_eq!(field(&run("Math = true; Math;"), "flag"), Value::Bool(true));
}
#[test]
fn bare_var_preserves_original_object() {
    assert_eq!(field(&run("var Object; Object;"), "tag"), Value::Int(5));
}
#[test]
fn deferred_same_canonical_binding() {
    assert_eq!(
        field(
            &run("'use strict'; var Math = false; function set(){ Math = true; } set(); Math;"),
            "flag"
        ),
        Value::Bool(true)
    );
}

#[derive(Default, Clone)]
struct Probe {
    bits: Option<Vec<u64>>,
    int_negative: bool,
    gc: bool,
    stale: bool,
    bad_transport: Option<i64>,
}
fn call(id: u16, args: Vec<vm::Expr>) -> vm::Expr {
    vm::Expr::Call(id, Vec::new(), args, None, None)
}
fn helper(ir: &vm::FrontendIr, name: &str) -> u16 {
    ir.functions
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("missing {name}"))
        .index
}
fn visit_expr(e: &mut vm::Expr, f: &mut impl FnMut(&mut vm::Expr)) {
    use vm::Expr as E;
    f(e);
    match e {
        E::Add(a, b)
        | E::Sub(a, b)
        | E::Mul(a, b)
        | E::Div(a, b)
        | E::Mod(a, b)
        | E::Eq(a, b)
        | E::Lt(a, b)
        | E::Gt(a, b)
        | E::And(a, b)
        | E::Or(a, b) => {
            visit_expr(a, f);
            visit_expr(b, f);
        }
        E::Neg(a) | E::Not(a) | E::ToOwned(a) | E::Borrow(a) | E::BorrowMut(a) => visit_expr(a, f),
        E::Call(_, _, args, _, _) | E::LocalCall(_, _, args, _) | E::ModuleCall(_, _, args, _) => {
            for a in args {
                visit_expr(a, f);
            }
        }
        E::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            visit_expr(condition, f);
            visit_expr(then_expr, f);
            visit_expr(else_expr, f);
        }
        E::Block { stmts, expr } => {
            visit_stmts(stmts, f);
            visit_expr(expr, f);
        }
        E::ResultConstruct { payload, .. } => visit_expr(payload, f),
        E::ResultQuery { value, .. } | E::ResultTry { value, .. } => visit_expr(value, f),
        E::ResultMatch {
            value,
            ok_expr,
            err_expr,
            ..
        } => {
            visit_expr(value, f);
            visit_expr(ok_expr, f);
            visit_expr(err_expr, f);
        }
        E::OptionalGet { container, key, .. } => {
            visit_expr(container, f);
            visit_expr(key, f);
        }
        E::OptionUnwrapOr {
            value, fallback, ..
        } => {
            visit_expr(value, f);
            visit_expr(fallback, f);
        }
        E::Match {
            value,
            arms,
            default,
            ..
        } => {
            visit_expr(value, f);
            for (_, a) in arms {
                visit_expr(a, f);
            }
            visit_expr(default, f);
        }
        E::Closure(c) => visit_expr(&mut c.body, f),
        E::ClosureCall(c, args) => {
            visit_expr(&mut c.body, f);
            for a in args {
                visit_expr(a, f);
            }
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
        | E::FunctionRef(..)
        | E::ModuleFunctionRef(..)
        | E::UnresolvedFunctionRef { .. } => {}
    }
}
fn visit_stmts(ss: &mut [vm::Stmt], f: &mut impl FnMut(&mut vm::Expr)) {
    use vm::Stmt as S;
    for s in ss {
        match s {
            S::Let { expr, .. } | S::Assign { expr, .. } | S::Expr { expr, .. } => {
                visit_expr(expr, f)
            }
            S::IfElse {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                visit_expr(condition, f);
                visit_stmts(then_branch, f);
                visit_stmts(else_branch, f);
            }
            S::While {
                condition, body, ..
            } => {
                visit_expr(condition, f);
                visit_stmts(body, f);
            }
            S::For {
                init,
                condition,
                post,
                body,
                ..
            } => {
                visit_stmts(std::slice::from_mut(init), f);
                visit_expr(condition, f);
                visit_stmts(std::slice::from_mut(post), f);
                visit_stmts(body, f);
            }
            S::ClosureLet { closure, .. } => visit_expr(&mut closure.body, f),
            S::Noop { .. }
            | S::FuncDecl { .. }
            | S::Break { .. }
            | S::Continue { .. }
            | S::Drop { .. } => {}
        }
    }
}
impl vm::SourcePlugin for Probe {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new("javascript-es5-global-observer"),
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
        use vm::{Expr as E, Stmt as S};
        let mut ir = vm::SourcePlugin::parse_source(&JAVASCRIPT_ES5_PLUGIN, source, context)?;
        if let Some(bits) = &self.bits {
            // Controlled payload injection: only the Script assignment literals,
            // in source order; one real source compilation drives every roundtrip.
            // All source functions, setter/getter, B1 and capture authority stay real.
            assert_eq!(source, bits_source(bits.len()));
            let mut count = 0;
            visit_stmts(&mut ir.stmts, &mut |e| {
                if matches!(e,E::Float(n) if n.to_bits()==1.25f64.to_bits()) {
                    *e = E::Float(f64::from_bits(bits[count]));
                    count += 1;
                }
            });
            assert_eq!(count, bits.len(), "qualify exact source producers");
            eprintln!(
                "GLOBAL38_INJECT source-producer=Script/Math=1.25 number_bits={bits:x?} matches={count}"
            );
        }
        if self.int_negative {
            // Corrupt the number cell at the actual shared getter entry, using
            // the actual checked ENV input. No boxed Int claims a Float schema.
            let get = helper(&ir, "__compat_es5_global_env_get");
            let checked = helper(&ir, "__compat_es5_global_env_checked");
            let write = helper(&ir, "__compat_ref_write");
            let body = ir.function_impls.get_mut(&get).unwrap();
            let [c, p, entry] = body.param_slots[..] else {
                panic!("getter ABI")
            };
            let address = E::Add(
                Box::new(call(checked, vec![E::Var(c), E::Var(p)])),
                Box::new(E::Add(
                    Box::new(E::Int(6)),
                    Box::new(E::Mul(Box::new(E::Var(entry)), Box::new(E::Int(5)))),
                )),
            );
            body.body_stmts.insert(
                0,
                S::Expr {
                    expr: call(
                        write,
                        vec![
                            E::Var(c),
                            E::Div(Box::new(address), Box::new(E::Float(1.0))),
                            E::Int(7),
                        ],
                    ),
                    line: 1,
                },
            );
        }
        if self.gc {
            append_gc_transport_probe(&mut ir, self.stale);
        }
        if let Some(n) = self.bad_transport {
            let pack = helper(&ir, "__compat_es5_global_env_pack");
            let slot = helper(&ir, "__compat_es5_global_env_slot");
            let env = ir
                .stmts
                .iter()
                .find_map(|s| {
                    if let S::Let {
                        index,
                        expr: E::Call(id, _, _, _, _),
                        ..
                    } = s
                    {
                        (*id == pack).then_some(*index)
                    } else {
                        None
                    }
                })
                .expect("actual env");
            let late = bind(
                &mut ir,
                E::Add(
                    Box::new(E::Sub(Box::new(E::Var(env)), Box::new(E::Var(env)))),
                    Box::new(E::Int(n)),
                ),
            );
            push(&mut ir, call(slot, vec![E::Var(late)]));
        }
        Ok(ir)
    }
}
fn fresh(ir: &mut vm::FrontendIr) -> u16 {
    let n = u16::try_from(ir.locals).unwrap();
    ir.locals += 1;
    n
}
fn bind(ir: &mut vm::FrontendIr, expr: vm::Expr) -> u16 {
    let index = fresh(ir);
    ir.stmts.push(vm::Stmt::Let {
        index,
        declared_schema: None,
        expr,
        line: 1,
    });
    index
}
fn push(ir: &mut vm::FrontendIr, expr: vm::Expr) {
    ir.stmts.push(vm::Stmt::Expr { expr, line: 1 });
}
fn boxed_field(slot: u16, name: &str) -> vm::Expr {
    call(
        vm::BuiltinFunction::Get.call_index(),
        vec![
            vm::Expr::Borrow(Box::new(vm::Expr::Var(slot))),
            vm::Expr::String(name.into()),
        ],
    )
}
fn append_gc_transport_probe(ir: &mut vm::FrontendIr, stale: bool) {
    use vm::{Expr as E, Stmt as S};
    let pack = helper(ir, "__compat_es5_global_env_pack");
    let slot = helper(ir, "__compat_es5_global_env_slot");
    let generation = helper(ir, "__compat_es5_global_env_generation");
    let checked = helper(ir, "__compat_es5_global_env_checked");
    let get = helper(ir, "__compat_es5_global_env_get");
    let set = helper(ir, "__compat_es5_global_env_set");
    let publish = helper(ir, "__compat_es5_node_publish_edge");
    let root_write = helper(ir, "__compat_es5_root_write");
    let collect = helper(ir, "__compat_es5_heap_collect");
    let observe = helper(ir, "__compat_es5_heap_observe");
    let (env, c) = ir
        .stmts
        .iter()
        .find_map(|s| {
            if let S::Let {
                index,
                expr: E::Call(id, _, args, _, _),
                ..
            } = s
            {
                if *id == pack
                    && let E::Var(c) = args[0]
                {
                    return Some((*index, c));
                }
                None
            } else {
                None
            }
        })
        .expect("real ENV producer");
    let mut roots = Vec::new();
    visit_stmts(&mut ir.stmts, &mut |e| {
        if let E::Call(id, _, args, _, _) = e
            && *id == publish
            && matches!(args[6], E::Int(6))
        {
            roots.push((args[4].clone(), args[5].clone()));
        }
    });
    assert_eq!(roots.len(), 1);
    let closed = ir
        .local_bindings
        .iter()
        .find(|(n, _)| n == "closed")
        .unwrap()
        .1;
    // Remove ONLY the direct Script ENV root. Real source function header6/7
    // edges must keep that same ENV and its sole managed payload reachable.
    bind(
        ir,
        call(
            root_write,
            vec![
                E::Var(c),
                roots[0].0.clone(),
                roots[0].1.clone(),
                E::Var(closed),
            ],
        ),
    );
    bind(ir, call(collect, vec![E::Var(c)]));
    let old = bind(ir, call(get, vec![E::Var(c), E::Var(env), E::Int(0)]));
    let oldslot = bind(ir, boxed_field(old, "ref_slot"));
    let oldgen = bind(ir, boxed_field(old, "generation"));
    push(ir, E::Var(old));
    push(ir, E::Var(oldslot));
    push(ir, E::Var(oldgen));
    push(ir, call(observe, vec![E::Var(c), E::Int(5), E::Int(0)]));
    // Actual consumed transport helpers receive late runtime Ints derived from
    // the issued ENV, so no constant-only replacement arithmetic is tested.
    for n in [0, 17_592_186_040_320, 17_592_186_044_415] {
        let late = bind(
            ir,
            E::Add(
                Box::new(E::Sub(Box::new(E::Var(env)), Box::new(E::Var(env)))),
                Box::new(E::Int(n)),
            ),
        );
        push(ir, call(slot, vec![E::Var(late)]));
        push(ir, call(generation, vec![E::Var(late)]));
    }
    let real_s = call(slot, vec![E::Var(env)]);
    let real_g = call(generation, vec![E::Var(env)]);
    push(
        ir,
        E::Eq(
            Box::new(call(pack, vec![E::Var(c), real_s, real_g])),
            Box::new(E::Var(env)),
        ),
    );
    bind(
        ir,
        call(set, vec![E::Var(c), E::Var(env), E::Int(0), E::Var(closed)]),
    );
    bind(ir, call(collect, vec![E::Var(c)]));
    push(
        ir,
        call(observe, vec![E::Var(c), E::Int(13), E::Var(oldslot)]),
    );
    push(
        ir,
        call(observe, vec![E::Var(c), E::Int(14), E::Var(oldslot)]),
    );
    push(ir, call(get, vec![E::Var(c), E::Var(env), E::Int(0)]));
    if stale {
        push(
            ir,
            call(
                checked,
                vec![
                    E::Var(c),
                    E::Add(Box::new(E::Var(env)), Box::new(E::Int(4096))),
                ],
            ),
        );
    }
}
fn bits_source(n: usize) -> String {
    format!(
        "'use strict'; function get(){{return Math;}} {}",
        "Math=1.25; get();".repeat(n)
    )
}
#[test]
fn shared_env_float_bits_and_physical_int_refusal() {
    let bits = [
        0,
        0x8000_0000_0000_0000,
        0x7ff0_0000_0000_0000,
        0xfff0_0000_0000_0000,
        1,
        0x8000_0000_0000_0001,
        0x000f_ffff_ffff_ffff,
        0x800f_ffff_ffff_ffff,
        0x0010_0000_0000_0000,
        0x8010_0000_0000_0000,
        0x7fef_ffff_ffff_ffff,
        0xffef_ffff_ffff_ffff,
        0x7ff8_1234_5678_9abc,
        0xfff8_2345_6789_abcd,
        0x7ff0_1234_5678_9abc,
        0xfff0_2345_6789_abcd,
    ];
    let result = execute(
        &bits_source(bits.len()),
        Some(Probe {
            bits: Some(bits.to_vec()),
            ..Probe::default()
        }),
    );
    assert!(!result.refused);
    assert_eq!(result.resources, 1);
    assert_eq!(result.stack.len(), 2 * bits.len());
    for (i, bits) in bits.into_iter().enumerate() {
        let b = &result.stack[2 * i + 1];
        assert_eq!(field(b, "tag"), Value::Int(3));
        let Value::Float(n) = field(b, "number") else {
            panic!("actual physical Float required")
        };
        assert_eq!(
            n.to_bits(),
            bits,
            "actual ENV setter/B1/getter/source-return bits"
        );
        assert_eq!(field(b, "flag"), Value::Bool(false));
        assert_eq!(field(b, "ref_slot"), Value::Int(0));
        assert_eq!(field(b, "generation"), Value::Int(0));
        eprintln!(
            "GLOBAL38_ROUNDTRIP bits={bits:016x} tag=3 physicalFloat=true flag=false refslot=0 generation=0"
        );
    }
    let result = execute(
        &bits_source(1),
        Some(Probe {
            bits: Some(vec![1.25f64.to_bits()]),
            int_negative: true,
            ..Probe::default()
        }),
    );
    assert!(result.refused);
    assert_eq!(
        result
            .stack
            .iter()
            .filter(|v| matches!(v, Value::Map(_)))
            .count(),
        1,
        "only earlier assignment result, no successful getter box published"
    );
}

const SIX: [&str; 6] = [
    "Object",
    "Math",
    "parseInt",
    "parseFloat",
    "isNaN",
    "isFinite",
];
#[test]
fn canonical_six_full_boxes() {
    let mut source = String::from(
        "'use strict'; function put(v){Object=v;Math=v;parseInt=v;parseFloat=v;isNaN=v;isFinite=v;} var obj={a:true};var arr=[1,2];var fun=function(x){return x;};",
    );
    let fixtures = [
        ("true", 2),
        ("false", 2),
        ("null", 1),
        ("undefined", 0),
        ("-0", 3),
        ("NaN", 3),
        ("Infinity", 3),
        ("'payload'", 4),
        ("obj", 5),
        ("arr", 5),
        ("fun", 5),
    ];
    for (expr, _) in fixtures {
        source.push_str(&format!("put({expr});"));
        for name in SIX {
            source.push_str(name);
            source.push(';');
        }
    }
    let result = execute(&source, None);
    assert_eq!(result.resources, 1);
    assert_eq!(result.stack.len(), fixtures.len() * 7);
    for (i, (expr, tag)) in fixtures.into_iter().enumerate() {
        let boxes = &result.stack[i * 7 + 1..i * 7 + 7];
        for b in boxes {
            assert_eq!(field(b, "tag"), Value::Int(tag));
            assert!(matches!(field(b, "number"), Value::Float(_)));
            assert_eq!(field(b, "flag"), Value::Bool(expr == "true"));
        }
        // Check every physical field, including references, against the actual
        // canonical first getter; no debug-text/JSON replacement for the payload.
        for b in &boxes[1..] {
            for key in ["tag", "flag", "ref_slot", "generation"] {
                assert_eq!(field(b, key), field(&boxes[0], key));
            }
            let Value::Float(n) = field(b, "number") else {
                unreachable!()
            };
            let Value::Float(first) = field(&boxes[0], "number") else {
                unreachable!()
            };
            assert_eq!(n.to_bits(), first.to_bits());
        }
        if tag >= 4 {
            assert!(matches!(field(&boxes[0],"generation"),Value::Int(g) if g>0));
        } else {
            assert_eq!(field(&boxes[0], "ref_slot"), Value::Int(0));
            assert_eq!(field(&boxes[0], "generation"), Value::Int(0));
        }
        if expr == "-0" {
            let Value::Float(n) = field(&boxes[0], "number") else {
                unreachable!()
            };
            assert_eq!(n.to_bits(), (-0.0f64).to_bits());
        }
        if expr == "NaN" {
            assert!(matches!(field(&boxes[0],"number"),Value::Float(n) if n.is_nan()));
        }
        eprintln!("GLOBAL38_SIX_FULLBOX expr={expr} tag={tag} actualRuntimeFields=true");
    }
}

#[test]
fn canonical_declarations_aliases_effects_and_abrupt() {
    let source = r#"'use strict';
        function isFinite(){return false;} function isFinite(){return true;} var isFinite;
        var O=Object;var M=Math;var P=parseInt;var Math;var parseInt;
        var before=Math===M&&parseInt===P&&isFinite();
        var parseFloat=[1,2];var arrayvarok=typeof parseFloat==='object';
        function read(){return Math;} function write(v){Math=v;return Math;}
        function factory(){return function(){return Math;};}
        function local(){var Math;return Math===undefined;}
        function formal(Math){return Math;}
        function replacement(x){return x;}
        var saved=parseInt((parseInt=replacement,'12'),(Math=true,10));
        var savedok=saved===12&&parseInt(false)===false&&Math===true;
        var nest=factory();var setok=write(false)===false&&read()===false&&nest()===false;
        try{throw 9;}catch(Math){Math=true;} var shadowok=read()===false&&local()&&formal(true);
        Object=false;Math={};var o={a:1};
        var originalok=O.getPrototypeOf(o)===O.prototype&&O.prototype.constructor===O&&M.toString()==='[object Math]'&&Math.toString()==='[object Object]';
        function fail(){isNaN=true;throw 7;} var old=Math;
        try{Math=fail();}catch(e){}finally{parseFloat=true;}
        before&&arrayvarok&&savedok&&setok&&shadowok&&originalok&&Math===old&&isNaN===true&&parseFloat===true;
    "#;
    assert_eq!(field(&run(source), "flag"), Value::Bool(true));
}

#[test]
fn local_specialization_and_script_var_timing() {
    let scalar = execute("var Math=7;var parseInt=8;Math+parseInt;", None);
    assert_eq!(scalar.resources, 0);
    assert_eq!(
        field(scalar.stack.last().unwrap(), "number"),
        Value::Float(15.0)
    );
    assert_eq!(
        field(&run("var Object=7;var s='x';Object===7&&s==='x';"), "flag"),
        Value::Bool(true)
    );
    assert_eq!(
        field(
            &run("var old=Math;var Math;var a=Math===old;var Math=true;var Math;a&&Math===true;"),
            "flag"
        ),
        Value::Bool(true)
    );
}

#[test]
fn gc_function_env_reachability_replacement_and_private_44bit_transport() {
    let result = execute(
        "'use strict'; function set(){Math={k:'retained'};} function get(){return Math;} var closed=null;set();",
        Some(Probe {
            gc: true,
            stale: true,
            ..Probe::default()
        }),
    );
    assert!(result.refused);
    assert_eq!(result.resources, 1);
    let values = &result.stack[1..];
    assert_eq!(field(&values[0], "tag"), Value::Int(5));
    assert!(matches!(values[2],Value::Int(g) if g>0));
    assert!(matches!(values[3],Value::Int(n) if n>0));
    assert_eq!(
        &values[4..10],
        &[
            Value::Int(0),
            Value::Int(0),
            Value::Int(0),
            Value::Int(4_294_967_295),
            Value::Int(4095),
            Value::Int(4_294_967_295)
        ]
    );
    assert_eq!(values[10], Value::Bool(true));
    assert_eq!(
        values[11],
        Value::Int(3),
        "old object released after canonical replacement"
    );
    assert_eq!(
        values[12], values[2],
        "released row retains measured generation"
    );
    assert_eq!(field(&values[13], "tag"), Value::Int(1));
    eprintln!(
        "GLOBAL38_GC actual ENV-only payload survives temporary release and direct ENV-root removal via source function edges; replacement frees measured old row; actual full44bit split/closed0/validpair/stale rejection"
    );
}

#[test]
fn private_transport_negative_and_overmax_new_vm_initialization() {
    for input in [-1, 17_592_186_044_416] {
        let result = execute(
            "'use strict'; function read(){return Math;} var initial=typeof read()==='object'; Math=true;initial;",
            Some(Probe {
                bad_transport: Some(input),
                ..Probe::default()
            }),
        );
        assert!(result.refused);
        assert_eq!(
            field(&result.stack[1], "flag"),
            Value::Bool(true),
            "each independent issued VM starts with original Math"
        );
    }
}
