//! `Function.prototype.prototype` accessor and source `new F(...)`.
//!
//! The `prototype` property is one inherited get/set accessor on
//! `Function.prototype`, never a per-function creation-time row. Reading it
//! lazily materializes `{constructor: F, [[Prototype]]: Object.prototype}` as
//! F's own writable/enumerable/configurable data row; deleting that row makes
//! the next read materialize a genuinely new object.
//!
//! `new F(...)` evaluates callee, then actuals, then `Get(F, "prototype")`,
//! allocates the instance with that prototype, calls F with the instance as
//! receiver, and keeps the returned object or the instance.
//!
//! Support matrix for this slice:
//!
//! * closed — lazy materialization, `constructor`/`Object.prototype` shape,
//!   writable/enumerable/configurable row, `delete` + fresh identity, the
//!   setter storing any value, `in`/`hasOwnProperty`/`Object.keys` parity, and
//!   the guest `TypeError` for `Function.prototype` itself and non-function
//!   receivers.
//! * closed — the four `prototype` receiver classes: a source function
//!   materializes, `Function.prototype` (code 256) throws, a builtin
//!   constructor returns the prototype object the realm already owns
//!   (`Number.prototype.constructor === Number`, `Array.prototype` identity
//!   with `new Array()`), and every ordinary intrinsic method or bound
//!   product reads `undefined` instead of throwing.
//! * closed — `new F(...)` over a statically known *source* function value,
//!   including aliases (`var G = F`), arbitrary arity, returned-object
//!   selection, and the non-object-`prototype` fallback to `Object.prototype`.
//! * open — `instanceof`, `F.constructor`, a materialized prototype's
//!   `Object.prototype.isPrototypeOf` style chains beyond the one real link.
//! * open, compile-time — `new` over a non-static callee (`F.bind(null)`,
//!   `o.m`, a call result): the frontend refuses the shape.
//! * closed — `new` over a bare formal parameter: the private helper checks
//!   the runtime source-function kind and raises the guest TypeError for any
//!   other value; member and call callees keep their compile-time refusal.
//! * closed — `new` over a provably non-callable *object* callee (an object
//!   literal or `new Object()` value): admitted into the same runtime
//!   rejection, which runs after every actual and strictly before
//!   `Get(F, "prototype")`, so a throwing `prototype` getter on the callee is
//!   never consulted. `new ({}) (k=1)` is the pinned TypeError with `k === 1`.
//! * closed — `new` over a builtin-constructor alias (`var A = Array; new A()`):
//!   the binding, not the spelling, selects the builtin construction route;
//!   `var Array = Shadow` still constructs Shadow. `new Number/String/Boolean`
//!   is the one upstream runtime TypeError form.

const PROBE_FUEL: u64 = 40_000_000;

fn execute(source: &str) -> vm::Value {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut machine =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                    .unwrap_or_else(|error| {
                        panic!("source={source:?} guest_invocations=0 {error:?}")
                    });
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(PROBE_FUEL);
            let status = machine.run();
            eprintln!(
                "FNPROTO_VM source={source:?} status={status:?} stack={:?}",
                machine.stack()
            );
            match status {
                Ok(vm::VmStatus::Halted) => machine.stack().last().unwrap().clone(),
                other => vm::Value::string(format!("STATUS {other:?}")),
            }
        })
        .unwrap()
        .join()
        .unwrap()
}

/// `(tag, number, flag)` of the boxed JS value left on the stack.
fn boxed(source: &str) -> (i64, f64, bool) {
    let result = execute(source);
    let vm::Value::Map(map) = result else {
        panic!("full box: {result:?}")
    };
    let tag = match map.get(&vm::Value::string("tag")) {
        Some(vm::Value::Int(tag)) => *tag,
        other => panic!("tag: {other:?}"),
    };
    let number = match map.get(&vm::Value::string("number")) {
        Some(vm::Value::Float(n)) => *n,
        other => panic!("number: {other:?}"),
    };
    let flag = match map.get(&vm::Value::string("flag")) {
        Some(vm::Value::Bool(b)) => *b,
        other => panic!("flag: {other:?}"),
    };
    (tag, number, flag)
}

fn assert_true(source: &str) {
    let (tag, _, flag) = boxed(source);
    assert_eq!(tag, 2, "expected Boolean box: {source}");
    assert!(flag, "expression must be true: {source}");
}

fn assert_number(source: &str, expected: f64) {
    let (tag, number, _) = boxed(source);
    assert_eq!(tag, 3, "expected Number box: {source}");
    assert_eq!(number, expected, "number must match: {source}");
}

/// Compile-time refusal diagnostic.
fn refusal(source: &str) -> String {
    match pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(source) {
        Ok(_) => panic!("must refuse: {source}"),
        Err(error) => format!("{error:?}"),
    }
}

// ---------------------------------------------------------------- stage A --

#[test]
fn prototype_has_no_own_row_before_the_first_read() {
    // The accessor is inherited: `hasOwnProperty` is false and `Object.keys`
    // is empty until the getter materializes the row, but `in` already sees
    // the inherited accessor.
    // One `.prototype` spelling anywhere in the source installs the accessor;
    // the observations below still happen before any read materializes a row.
    assert_true(r#"function f(){};var before=f.hasOwnProperty("prototype");f.prototype;before===false;"#);
    assert_true(r#"function f(){};var n=Object.keys(f).length;f.prototype;n===0;"#);
    assert_true(r#"function f(){};var before="prototype" in f;f.prototype;before===true;"#);
    assert_true(
        r#"function f(){};var own=Object.getPrototypeOf(f).hasOwnProperty("prototype");f.prototype;own===true;"#,
    );
    // `in` sees the inherited accessor while `hasOwnProperty` still does not.
    assert_true(r#"function f(){};var before="prototype" in f;var own=f.hasOwnProperty("prototype");f.prototype;before===true&&own===false;"#);
}

#[test]
fn prototype_read_materializes_one_own_data_row() {
    assert_true(r#"function f(){};f.prototype;f.hasOwnProperty("prototype");"#);
    assert_true(r#"function f(){};f.prototype;Object.keys(f).length===1&&Object.keys(f)[0]==="prototype";"#);
    // The materialized object is `{constructor: F}` with Object.prototype.
    assert_true(r#"function f(){};f.prototype.constructor===f;"#);
    assert_true(
        r#"function f(){};Object.getPrototypeOf(f.prototype)===Object.prototype;"#,
    );
    assert_true(
        r#"function f(){};f.prototype;var d=Object.getOwnPropertyDescriptor(f,"prototype");d.writable&&d.enumerable&&d.configurable;"#,
    );
}

#[test]
fn deleting_the_materialized_row_yields_a_fresh_identity() {
    // `delete` removes the own row; the next read is one new materialization,
    // so the identity differs from the deleted object.
    assert_true(
        r#"function f(){};var a=f.prototype;var d=delete f.prototype;d===true&&f.hasOwnProperty("prototype")===false;"#,
    );
    assert_true(
        r#"function f(){};var a=f.prototype;delete f.prototype;var b=f.prototype;a!==b&&b.constructor===f;"#,
    );
    assert_true(
        r#"function f(){};var a=f.prototype;delete f.prototype;typeof f.prototype==="object"&&a!==f.prototype;"#,
    );
}

#[test]
fn prototype_setter_stores_any_value_and_reopens_materialization() {
    assert_true(r#"function f(){};f.prototype=7;f.prototype===7;"#);
    assert_true(r#"function f(){};f.prototype=7;f.hasOwnProperty("prototype");"#);
    assert_true(
        r#"function f(){};f.prototype=7;var d=delete f.prototype;d&&typeof f.prototype==="object"&&f.prototype.constructor===f;"#,
    );
    // The stored value replaces the materialized object for every reader.
    assert_true(r#"function f(){};var a=f.prototype;f.prototype=7;a!==f.prototype;"#);
    assert_true(r#"function f(){};var o={q:1};f.prototype=o;f.prototype===o;"#);
}

#[test]
fn prototype_receiver_must_be_a_source_function() {
    // `Function.prototype` itself (private code 256) is the one function node
    // that keeps the realm TypeError on both halves; it is not a constructor.
    assert_true(
        "var p=Object.getPrototypeOf(function(){});var c=false;try{p.prototype;}catch(e){c=e.name==='TypeError';}c;",
    );
    assert_true(
        "var p=Object.getPrototypeOf(function(){});var c=false;try{p.prototype=7;}catch(e){c=e.name==='TypeError';}c;",
    );
    // An ordinary intrinsic method node is not a constructor either, but
    // upstream reads `undefined` instead of throwing, and so does this realm.
    assert_true("Math.max.prototype===undefined;");
    // A bound product (`fn.bind` code 270) is the same class.
    assert_true("function f(){};f.bind(null).prototype===undefined;");
    // An ordinary object has no `prototype` in its chain at all.
    assert_true(r#"var o={};o.hasOwnProperty("prototype")===false&&o.prototype===undefined;"#);
}

#[test]
fn prototype_builtin_constructor_returns_the_existing_prototype() {
    // Four classes on one source: an intrinsic method and a bound product
    // read `undefined`, while the builtin constructors return the prototype
    // object the realm already owns. `Number.prototype.constructor` must name
    // the same carrier node, which is why the accessor never re-materializes.
    assert_true(
        "typeof Number.prototype==='object'&&Number.prototype.constructor===Number\
         &&typeof String.prototype==='object'&&String.prototype.constructor===String\
         &&typeof Boolean.prototype==='object'&&Boolean.prototype.constructor===Boolean\
         &&typeof Object.prototype==='object'&&Object.prototype.constructor===Object\
         &&Object.keys.prototype===undefined&&Math.max.prototype===undefined;",
    );
    assert_true(
        "function f(){};f.bind(null).prototype===undefined\
         &&typeof f.prototype==='object'&&f.prototype.constructor===f;",
    );
}

#[test]
fn array_prototype_is_the_constructed_array_prototype() {
    // `Array.prototype` is the private empty packed Array prototype itself:
    // `new Array()` links to it, the method table answers its `constructor`,
    // and the carrier keeps its own non-enumerable `prototype` row.
    assert_true(
        "typeof Array.prototype==='object'&&Array.prototype.length===0\
         &&Object.getPrototypeOf(new Array())===Array.prototype\
         &&Object.getPrototypeOf([])===Array.prototype\
         &&Array.prototype.constructor===Array&&[].constructor===Array\
         &&typeof Array.prototype.push==='function'&&typeof Array.prototype.sort==='function';",
    );
    assert_true("Array.hasOwnProperty('prototype')&&Array.prototype===Array.prototype&&Array.prototype!==undefined;");
}

#[test]
fn primitive_receivers_inherit_the_constructor_row() {
    // The constructor row lives on the private primitive prototype, so a
    // primitive receiver resolves it without exposing the general
    // Object.prototype surface those bridges deliberately omit. The
    // `.constructor` read itself selects that row: the source must not be
    // able to pass by first touching another property.
    assert_true(
        "(1).constructor===Number&&'x'.constructor===String&&true.constructor===Boolean;",
    );
}

#[test]
fn prototype_key_is_spellable_computed() {
    assert_true(r#"function f(){};f["prototype"].constructor===f;"#);
    assert_true(r#"function f(){};var before=f.hasOwnProperty("prototype");f["prototype"];!before&&f.hasOwnProperty("prototype");"#);
}

// ---------------------------------------------------------------- stage B --

#[test]
fn new_calls_the_source_function_with_the_instance_receiver() {
    assert_number("function F(a){\"use strict\";this.a=a;}new F(1).a;", 1.0);
    assert_number("function F(a){\"use strict\";this.a=a;}var o=new F(7);o.a;", 7.0);
    assert_true("function F(a){\"use strict\";this.a=a;}var o=new F(1);o.a===1;");
    assert_true("function F(a){\"use strict\";this.a=a;}var o=new F(1);o.hasOwnProperty(\"a\");");
    // A local alias of a source function is the same constructible value.
    assert_true(
        "function F(a){\"use strict\";this.a=a;}var G=F;var o=new G(3);o.a===3&&Object.getPrototypeOf(o)===F.prototype;",
    );
}

#[test]
fn new_installs_the_prototype_link_and_constructor() {
    assert_true("function F(){};var o=new F();Object.getPrototypeOf(o)===F.prototype;");
    assert_true("function F(){};var o=new F();o.constructor===F;");
    assert_number("function F(){};F.prototype.extra=1;(new F()).extra;", 1.0);
    assert_true("function F(){};new F();F.hasOwnProperty(\"prototype\");");
}

#[test]
fn new_keeps_a_returned_object_and_discards_a_primitive() {
    assert_number("function H(){return {z:1};}new H().z;", 1.0);
    assert_true("function K(){return 5;}typeof new K()===\"object\";");
    assert_true("function K(){return 5;}var o=new K();Object.getPrototypeOf(o)===K.prototype;");
    assert_true("function N(){return null;}typeof new N()===\"object\";");
}

#[test]
fn new_non_object_prototype_falls_back_to_object_prototype() {
    assert_true(
        "function L(){};L.prototype=7;var l=new L();Object.getPrototypeOf(l)===Object.prototype;",
    );
    assert_true("function L(){};L.prototype=7;var l=new L();typeof l===\"object\";");
}

#[test]
fn new_evaluates_the_prototype_get_after_the_actuals() {
    // The actual assignment runs before `Get(F,"prototype")`, so the instance
    // links to the object the actual installed.
    assert_number("function F(){return 7;}new F(F.prototype={q:1}).q;", 1.0);
    assert_true(
        "function F(){return 7;}var o=new F(F.prototype={q:1});Object.getPrototypeOf(o)===F.prototype;",
    );
}

#[test]
fn new_argument_arity_is_forwarded_verbatim() {
    assert_number("function F(a,b,c){\"use strict\";this.s=a+b+c;}new F(1,2,3).s;", 6.0);
    assert_true("function F(a){\"use strict\";this.a=a;}new F().a===undefined;");
    assert_true("function F(a,b){\"use strict\";this.b=b;}new F(1).b===undefined;");
}

#[test]
fn new_of_a_non_source_callee_is_out_of_this_slice() {
    // Bound products and member callees are known limits: the callee is not
    // a static source-function value, so the frontend refuses the shape at
    // compile time.
    let diagnostic = refusal("function F(){};new (F.bind(null))();");
    assert!(
        diagnostic.contains("unsupported") || diagnostic.contains("P0"),
        "bound new must stay a compile-time refusal: {diagnostic}"
    );
    // A builtin alias is still the builtin constructor: `var A=Array` is an
    // ordinary admitted construction now, never the guest TypeError limit.
    assert_true(
        "var A=Array;var a=new A(1,2);a.length===2&&Array.isArray(a);",
    );
    let diagnostic = refusal("var o={m:function(){}};new o.m();");
    assert!(
        diagnostic.contains("unsupported") || diagnostic.contains("P0"),
        "member callee new must stay a compile-time refusal: {diagnostic}"
    );
}

// ------------------------------------------------------------ review set --
//
// The five cases below pin the pinned engine's *property tables* and the
// exception/validation order of the construct slice. Each one fails on the
// previous candidate and passes only when the shape observation, the static
// gate, and the pending-Normal guard agree with the runtime helpers.

#[test]
fn review_property_tables_match_pinned_enumerability() {
    // `Function.prototype` carries the `prototype` get/set accessor as an
    // enumerable own row, and the materialized object's `constructor` row is
    // enumerable too. Both are ordinary rows in every later observation.
    // The trailing read only selects the compile-time accessor row; the
    // `Object.keys` observation still happens before any materialization.
    assert_true(
        r#"function f(){};var k=Object.keys(Object.getPrototypeOf(f));f.prototype;k.indexOf("prototype")>=0;"#,
    );
    assert_true(r#"function f(){};var k=Object.keys(f.prototype);k.length===1&&k[0]==="constructor";"#);
    // Deleting the materialized constructor row falls through the real
    // prototype chain to Object.prototype.constructor, and writing it back
    // replaces that observable for every reader of the prototype object.
    assert_true(
        r#"function f(){};var p=f.prototype;var d=delete p.constructor;d===true&&p.constructor===Object;"#,
    );
    assert_true(
        r#"function f(){};var p=f.prototype;p.constructor=7;p.constructor===7&&(new f()).constructor===7;"#,
    );
}

#[test]
fn review_in_and_has_own_property_gating_installs_the_accessor() {
    // Neither source below spells a `.prototype` member: the static gate has
    // to watch the `in` operator's "prototype" String key and the
    // `hasOwnProperty("prototype")` call instead.
    assert_true(r#"function f(){};"prototype" in f;"#);
    assert_true(r#"function f(){};Object.getPrototypeOf(f).hasOwnProperty("prototype");"#);
    assert_true(r#"function f(){};"prototype" in Object.getPrototypeOf(f);"#);
    // The same two observations still agree with a materialized row.
    assert_true(
        r#"function f(){};var before="prototype" in f;f.prototype;before&&f.hasOwnProperty("prototype");"#,
    );
}

#[test]
fn review_new_throw_paths_skip_later_actuals_and_use_the_channel() {
    // The first actual throws. The aborting construction must not continue:
    // the helper call (Get + allocation + F invocation) has to be suppressed
    // too, so F never runs and never writes through its second formal.
    assert_true(
        "function F(a,b){\"use strict\";b.hit=1;}function boom(){throw 7;}var o={hit:0};var caught=0;try{new F(boom(),o);}catch(e){caught=e;}caught===7&&o.hit===0;",
    );
    // A script whose only abrupt source is `new F()` still provisions the
    // guest throw channel: the constructor's Err must surface as the guest
    // uncaught-throw host error, never as a host assertion.
    let error = run_error("function F(){\"use strict\";throw 7;}new F();");
    assert!(
        error.contains("uncaught JavaScript throw"),
        "new F() must publish the guest throw, got {error}"
    );
}

#[test]
fn review_construct_helper_validates_shape_before_the_heap() {
    // Both private helpers used to call `checked_base` before the object-tag
    // check; a non-object receiver then hit a host assertion instead of the
    // guest TypeError. The emitted bodies must keep every base dereference
    // inside the tag branch.
    let ir = frontend_ir("function F(){};F.prototype;new F();");
    let checked_base = ir
        .functions
        .iter()
        .find(|function| function.name == "__compat_es5_checked_base")
        .expect("checked-base helper")
        .index;
    for name in ["__compat_es5_construct", "__compat_es5_function_prototype_api"] {
        let index = ir
            .functions
            .iter()
            .find(|function| function.name == name)
            .unwrap_or_else(|| panic!("missing helper {name}"))
            .index;
        let body = &ir.function_impls[&index].body_stmts;
        for statement in body {
            if matches!(statement, vm::Stmt::IfElse { .. }) {
                continue;
            }
            assert!(
                !stmt_calls_index(statement, checked_base),
                "{name} dereferences the base before validating the receiver shape: {statement:?}"
            );
        }
    }
}

#[test]
fn review_shadowed_object_construct_follows_the_binding() {
    // `var Object=F` makes the spelling `new Object()` a source construction
    // of F, exactly as validation resolves it. It must not fall into the
    // empty-object literal route.
    assert_true(
        "function F(){\"use strict\";this.a=7;}var Object=F;var o=new Object();o.a===7&&o.constructor===F;",
    );
    // The unshadowed spelling keeps its empty-object route.
    assert_true("var o=new Object();Object.getPrototypeOf(o)===Object.prototype;");
}

#[test]
fn review_shadowed_object_throw_reaches_the_guest_catch() {
    // The Script effect scan must decide "is this `new` a builtin construct"
    // by the binding, not by the `Object` spelling. `var Object=F` makes the
    // construction a source one, so its guest throw needs the Script pending
    // channel exactly like `new F()`.
    assert_number(
        "function F(){\"use strict\";throw 7;}var Object=F;var c=0;try{new Object();}catch(e){c=e;}c;",
        7.0,
    );
    // The same shadowed construction without a Script-level `try`: the Script
    // still owns the pending channel, so the throw is the uncaught guest throw
    // and never a host assertion.
    let uncaught = run_error("function F(){\"use strict\";throw 7;}var Object=F;new Object();");
    assert!(
        uncaught.contains("uncaught JavaScript throw"),
        "shadowed new Object() must publish the guest throw: {uncaught}"
    );
    // Non-throwing shadowed construction keeps the source route and its
    // object result (no over-widening of the builtin arm).
    assert_true(
        "function F(){\"use strict\";this.a=7;}var Object=F;var o=new Object();o.a===7&&o.constructor===F;",
    );
}

#[test]
fn review_builtin_new_object_still_skips_the_throw_channel() {
    // The true intrinsic keeps the catalog-free compile: only the source
    // construction route provisions `typescript::throw_error`, and this base
    // catalog does not carry it.
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN)
        .with_host_api_catalog(pd_vm_compat_frontends::reference_array_effective_catalog());
    vm::compile_source_with_flavor_and_options(
        "new Object();",
        pd_vm_compat_frontends::javascript::javascript_es5_flavor(),
        options,
    )
    .unwrap_or_else(|error| {
        panic!("builtin new Object() must compile without throw_error: {error:?}")
    });
}

fn frontend_ir(source: &str) -> vm::FrontendIr {
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    vm::SourcePlugin::parse_source(
        &pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN,
        source,
        &vm::SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::compiler::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        },
    )
    .unwrap_or_else(|error| panic!("source must parse: {source}\n{error:?}"))
}

fn expr_calls_index(expression: &vm::Expr, index: u16) -> bool {
    match expression {
        vm::Expr::Call(target, _, args, _, _) => {
            *target == index || args.iter().any(|argument| expr_calls_index(argument, index))
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
            expr_calls_index(left, index) || expr_calls_index(right, index)
        }
        vm::Expr::Neg(inner)
        | vm::Expr::Not(inner)
        | vm::Expr::ToOwned(inner)
        | vm::Expr::Borrow(inner)
        | vm::Expr::BorrowMut(inner) => expr_calls_index(inner, index),
        vm::Expr::IfElse {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_calls_index(condition, index)
                || expr_calls_index(then_expr, index)
                || expr_calls_index(else_expr, index)
        }
        vm::Expr::Block { stmts, expr } => {
            stmts.iter().any(|stmt| stmt_calls_index(stmt, index))
                || expr_calls_index(expr, index)
        }
        _ => false,
    }
}

fn stmt_calls_index(statement: &vm::Stmt, index: u16) -> bool {
    match statement {
        vm::Stmt::Let { expr, .. } | vm::Stmt::Assign { expr, .. } | vm::Stmt::Expr { expr, .. } => {
            expr_calls_index(expr, index)
        }
        vm::Stmt::IfElse {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            expr_calls_index(condition, index)
                || then_branch.iter().any(|stmt| stmt_calls_index(stmt, index))
                || else_branch.iter().any(|stmt| stmt_calls_index(stmt, index))
        }
        vm::Stmt::While {
            condition, body, ..
        } => {
            expr_calls_index(condition, index)
                || body.iter().any(|stmt| stmt_calls_index(stmt, index))
        }
        _ => false,
    }
}

/// Run a script expected to end abruptly and return the VM error text.
fn run_error(source: &str) -> String {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(33_554_432)
        .spawn(move || {
            let mut machine =
                pd_vm_compat_frontends::javascript::cli::compile_javascript_es5_source(&source)
                    .unwrap_or_else(|error| {
                        panic!("source={source:?} guest_invocations=0 {error:?}")
                    });
            machine.set_jit_config(vm::JitConfig {
                enabled: false,
                ..*machine.jit_config()
            });
            machine.set_fuel(PROBE_FUEL);
            match machine.run() {
                Ok(status) => format!("{status:?}"),
                Err(error) => format!("{error:?}"),
            }
        })
        .unwrap()
        .join()
        .unwrap()
}

// ------------------------------------------------- construct review set --

#[test]
fn rr1_callee_throw_suppresses_first_actual() {
    // The callee is evaluated before any actual. When it throws, the first
    // actual must not run and `Get(F, "prototype")` must not materialize the
    // lazy own row.
    assert_true(
        "function F(a){\"use strict\";this.a=a;}function boom(){throw 7;}var box={hits:0};function side(o){o.hits=1;}var caught=0;var o;try{o=new (boom(), F)(side(box));}catch(e){caught=e;}caught===7&&box.hits===0&&F.hasOwnProperty(\"prototype\")===false;",
    );
}

#[test]
fn rr2_callee_throw_suppresses_zero_actual_tail() {
    // With no actuals the whole tail (vector, `Get(F, "prototype")`, the
    // private helper) sits behind the same callee-normal guard.
    assert_true(
        "function F(){\"use strict\";this.a=1;}function boom(){throw 7;}var caught=0;var o;try{o=new (boom(), F)();}catch(e){caught=e;}caught===7&&F.hasOwnProperty(\"prototype\")===false;",
    );
}

#[test]
fn rr3_builtin_new_keeps_the_old_catalog_free_compile() {
    // A script whose only `new` forms are built-in constructs must not start
    // depending on `typescript::throw_error`. The base catalog lacks it.
    let options = vm::CompileSourceFileOptions::new()
        .with_source_plugin(&pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN)
        .with_host_api_catalog(pd_vm_compat_frontends::reference_array_effective_catalog());
    vm::compile_source_with_flavor_and_options(
        "new Object();new Error(\"boom\");",
        pd_vm_compat_frontends::javascript::javascript_es5_flavor(),
        options,
    )
    .unwrap_or_else(|error| {
        panic!("builtin new must compile without throw_error: {error:?}")
    });
}

#[test]
fn rr4_function_prototype_accessor_row_is_configurable() {
    // The inherited `prototype` accessor is an ordinary configurable own row,
    // so `delete` removes it and the spelling disappears from `in`.
    assert_true(
        r#"function F(){}var fp=Object.getPrototypeOf(F);var d=delete fp.prototype;d===true&&("prototype" in F)===false;"#,
    );
}

#[test]
fn rr5a_array_alias_constructs() {
    assert_true(
        r#"var A=Array;var a=new A(1,2);a.length===2&&Array.isArray(a);"#,
    );
}

#[test]
fn rr5b_typed_array_alias_constructs_buffer_view() {
    // The typed-array alias follows the admitted buffer-view construction.
    assert_true(
        r#"var b=new ArrayBuffer(4);var C=Uint8Array;var v=new C(b);v.length===4;"#,
    );
}

#[test]
#[ignore = "typed-array length construction is outside this slice, direct or aliased"]
fn rr5b_open_typed_array_length_form() {
    assert_true(r#"var T2=Uint8Array;var t=new T2(2);t.length===2;"#);
}

#[test]
fn rr5c_object_alias_constructs() {
    assert_true(
        r#"var O=Object;var o=new O();typeof o==="object"&&Object.getPrototypeOf(o)===Object.prototype;"#,
    );
}

#[test]
fn rr5d_local_shadow_wins_over_the_spelling() {
    // A local binding shadows the spelling, even when the name is a builtin.
    assert_true(
        r#"function Shadow(){"use strict";this.z=1;}var Array=Shadow;var s=new Array();s.z===1&&s.constructor===Shadow;"#,
    );
    assert_true(
        r#"function Error(){"use strict";this.a=7;}function Date(){"use strict";this.b=8;}var e=new Error();var d=new Date();e.a===7&&d.b===8;"#,
    );
    // A second initializer replaces the alias: the later Shadow wins.
    assert_true(
        r#"function Shadow(){"use strict";this.z=1;}var A=Array;var A=Shadow;var a=new A();a.z===1;"#,
    );
}

#[test]
fn rr5e_number_string_boolean_new_is_runtime_type_error() {
    assert_true(
        r#"var n=0;try{new Number(3);}catch(e){if(e.name==="TypeError")n=n+1;}try{new String("ab");}catch(e){if(e.name==="TypeError")n=n+1;}try{new Boolean(true);}catch(e){if(e.name==="TypeError")n=n+1;}n===3;"#,
    );
}

#[test]
fn rr5f_existing_builtin_constructs_stay() {
    assert_true(
        r#"var e=new Error("boom");var o=new Object();e.message==="boom"&&typeof o==="object";"#,
    );
    assert_true(r#"var TE=TypeError;var e=new TE();e.name==="TypeError";"#);
}

// ------------------------------------------------- review 3 acceptance set --

#[test]
fn review3_constructor_read_selects_the_constructor_row() {
    // The registry-style acceptance: the original source must pass without
    // opening the `.prototype` accessor as a side effect first.
    assert_true(
        "(1).constructor===Number&&'x'.constructor===String&&true.constructor===Boolean;",
    );
    // `[].constructor` reads the Array method table's real `constructor` row.
    assert_true(r#"var a=[];a.constructor===Array;"#);
}

#[test]
fn review3_builtin_prototype_rows_exist_before_any_read() {
    // Every in-scope carrier owns a real `prototype` data row from bootstrap:
    // `hasOwnProperty` is true before a read and `Object.keys` lists it.
    assert_true(
        "var n=Number.hasOwnProperty('prototype')&&Object.keys(Number).indexOf('prototype')>=0;\
         var s=String.hasOwnProperty('prototype')&&Object.keys(String).indexOf('prototype')>=0;\
         var b=Boolean.hasOwnProperty('prototype')&&Object.keys(Boolean).indexOf('prototype')>=0;\
         var a=Array.hasOwnProperty('prototype')&&Object.keys(Array).indexOf('prototype')>=0;\
         var o=Object.hasOwnProperty('prototype')&&Object.keys(Object).indexOf('prototype')>=0;\
         n&&s&&b&&a&&o;",
    );
}

#[test]
fn review3_deleting_a_builtin_prototype_row_does_not_resurrect() {
    // The row is an ordinary configurable own row; after `delete` the
    // inherited accessor must answer `undefined`, never the old root.
    assert_true(
        "var d=delete Number.prototype;d===true&&typeof Number.prototype==='undefined';",
    );
    assert_true(
        "var d=delete Object.prototype;d===true&&typeof Object.prototype==='undefined';",
    );
}

#[test]
fn review3_unsupported_new_evaluates_its_actuals_first() {
    // The upstream TypeError is raised only after every actual has run.
    assert_true(
        "var k=0;var c=false;try{new Number(k=1);}catch(e){c=e.name==='TypeError';}c&&k===1;",
    );
    // An earlier abrupt actual wins: the later actual never runs and the
    // construction TypeError is never published.
    assert_true(
        "function boom(){throw 7;}var k=0;var caught=0;try{new Number(boom(),k=1);}catch(e){caught=e;}caught===7&&k===0;",
    );
}

#[test]
fn review3_setter_splits_intrinsic_methods_from_bound_products() {
    // `Math.max` is an ordinary intrinsic method node: the pinned setter
    // rejects the write with a guest TypeError.
    assert_true(
        "var c=false;try{Math.max.prototype=7;}catch(e){c=e.name==='TypeError';}c;",
    );
    assert_true(
        "var c=false;try{Object.keys.prototype=1;}catch(e){c=e.name==='TypeError';}c;",
    );
    // A bound product is writable: the accessor stores an ordinary own row.
    assert_true(
        "function f(){};var b=f.bind(null);b.prototype=7;b.prototype===7;",
    );
    // Builtin constructor carriers are writable. The ordinary own row answers
    // first; after `delete` the inherited setter must still accept the write.
    assert_true(
        "Number.prototype=7;Number.prototype===7&&Number.hasOwnProperty('prototype');",
    );
    assert_true(
        "var d=delete Number.prototype;Number.prototype=7;d===true&&Number.prototype===7&&Number.hasOwnProperty('prototype');",
    );
}

// ------------------------------------------------- review 4 body gates --

#[test]
fn review4_function_body_prototype_read_selects_the_accessor() {
    // The function body is the only `prototype` site in the whole source: no
    // Script-side read opens the accessor gate. The receiver is a formal so
    // the closed-subset capture rule is not involved.
    assert_true(r#"function read(x){return x.prototype;}function f(){};typeof read(f)==='object';"#);
    // The same gate must reach a nested function body, not only the outermost
    // one. `inner` is the sole reader; `outer` and the Script spell no
    // `prototype` member at all.
    assert_true(
        r#"function outer(){function inner(x){return x.prototype;}function f(){}return typeof inner(f)==='object';}outer();"#,
    );
}

#[test]
fn review4_function_body_constructor_read_selects_the_backlink() {
    // `(1).constructor` in the body is the only feature site. The read must
    // select the private constructor back-link row by itself; nothing else in
    // the source spells `prototype` or `constructor`.
    assert_true(
        r#"function read(){return (1).constructor===Number;}var r=read();r;"#,
    );
}

#[test]
fn review4_function_body_only_new_installs_the_construct_helper() {
    // `new F()` in the body is the only construction site. `F` is a formal
    // holding a source function, admitted through the construct helper's
    // runtime source-function check.
    assert_true(
        r#"function C(){}function make(F){return new F();}var r=typeof make(C)==='object';r;"#,
    );
}

#[test]
fn review4_keys_of_function_prototype_needs_no_trailing_read() {
    // The `prototype` accessor row on `Function.prototype` must be present
    // for this enumeration without a later `f.prototype` read opening the
    // gate. Both halves are the only feature sites in the source.
    assert_true(
        r#"function f(){}var k=Object.keys(Object.getPrototypeOf(f));k.indexOf("prototype")>=0;"#,
    );
}

#[test]
fn review4_overapproximation_stays_at_the_accessor_cost() {
    // The gate is name-based, so an ordinary object's own `constructor` and a
    // non-function receiver's own `prototype` still read their ordinary rows.
    // Selecting the `Function.prototype` accessor must not rewrite those
    // observations: the over-approximation keeps the same runtime answer and
    // no extra per-object row.
    assert_true(r#"var o={};o.constructor===Object;"#);
    assert_true(r#"function read(x){return x.prototype;}var o={prototype:7};read(o)===7;"#);
    assert_true(r#"function read(x){return x.constructor;}var o={constructor:9};read(o)===9;"#);
}

// ------------------------------------- review 5 primitive carriers, binding --

#[test]
fn review5_constructor_read_selects_the_three_primitive_carriers() {
    // The receiver of `.constructor` may be any primitive, and the static walk
    // cannot tell which one, so one `constructor` read selects the Number,
    // String and Boolean carriers even when the source never spells any of
    // those names. Without them the private prototype bridges have no
    // `constructor` row to answer with.
    assert_true("function read(){return typeof (1).constructor==='function';}read();");
    assert_true("function read(){return typeof 'x'.constructor==='function';}read();");
    assert_true("function read(){return typeof true.constructor==='function';}read();");
    // Each class keeps its own carrier: same class, same identity; different
    // classes, different functions.
    assert_true("function read(){return (1).constructor===(2).constructor;}read();");
    assert_true("function read(){return (1).constructor!=='x'.constructor;}read();");
    assert_true("function read(){return (1).constructor!==true.constructor;}read();");
    // The carrier keeps its real own `prototype` row: the private Number
    // bridge object the primitive adapter already answers with.
    assert_true("function read(){return typeof (1).constructor.prototype==='object';}read();");
}

#[test]
fn review5_construct_formal_admission_uses_the_lowered_binding() {
    // The catch parameter shadows the formal. Lowering reads the catch
    // binding, so admission must judge that same BindingId: a catch binding
    // is not a formal of the activation, so the slice keeps its compile-time
    // refusal instead of admitting through the outer formal.
    let diagnostic =
        refusal("function make(F){try{throw F;}catch(F){return new F();}}make(function(){});");
    assert!(
        diagnostic.contains("unsupported JavaScript ES5"),
        "{diagnostic}"
    );
    // The unshadowed formal is still the admitted dynamic slice.
    assert_true("function C(){}function make(F){return new F();}typeof make(C)==='object';");
}

#[test]
fn review5_new_of_a_plain_object_callee_rejects_after_the_actuals() {
    // A provably non-callable callee is admitted and reaches the runtime
    // rejection, which runs after every actual: `new ({}) (k=1)` throws the
    // guest TypeError with `k === 1`.
    assert_true("var k=0;var c=false;try{new ({})(k=1);}catch(e){c=e.name==='TypeError';}c&&k===1;");
    // The rejection precedes `Get(F, "prototype")`: the callee's own throwing
    // `prototype` getter is never consulted, so the catch sees the TypeError
    // and not the getter's 7.
    assert_true(
        "var k=0;var got=0;var o={get prototype(){throw 7;}};try{new (o)(k=1);}catch(e){got=e.name==='TypeError'?2:1;}got===2&&k===1;",
    );
    // A formal-held non-function takes the same order.
    assert_true(
        "var got=0;function make(F){return new F();}var o={get prototype(){throw 7;}};try{make(o);}catch(e){got=e.name==='TypeError'?2:1;}got===2;",
    );
}
