//! Equal-length parallel assignment of ordinary named fields.
//!
//! Lua text goes through the candidate SourcePlugin, Core, and the VM.
//! Each left side is a non-optional named field of a local table. Each
//! right side is a bool, string, nil, local, or ordinary field read, with
//! no call. Observation is one returned boolean from `rawequal`, so the
//! same source can be compared with stock Lua without a candidate host.

use std::sync::Arc;

use vm::{
    CompileSourceFileOptions, FrontendImportSyntax, FrontendIr, HostFunctionRegistry, ParseError,
    SourceFlavor, SourcePlugin, Value, VmStatus,
};

use super::candidate;

struct Plugin;

impl SourcePlugin for Plugin {
    fn flavor(&self) -> SourceFlavor {
        SourceFlavor::Lua
    }

    fn extensions(&self) -> &'static [&'static str] {
        &[]
    }

    fn import_syntax(&self) -> FrontendImportSyntax {
        FrontendImportSyntax::Lua
    }

    fn parse_source(&self, source: &str) -> Result<FrontendIr, ParseError> {
        candidate::lower(source, super::catalog())
    }
}

fn bool_result(source: &str) -> bool {
    let catalog = Arc::clone(super::catalog());
    let options = CompileSourceFileOptions::new()
        .with_source_plugin(&Plugin)
        .with_host_api_catalog(Arc::clone(&catalog));
    let mut vm = vm::compile_source_with_flavor_and_options(source, SourceFlavor::Lua, options)
        .unwrap_or_else(|error| panic!("lower Lua text: {error}"))
        .into_vm()
        .unwrap_or_else(|error| panic!("vm: {error}"));
    let mut registry = HostFunctionRegistry::empty();
    candidate::register(&mut registry, &catalog).expect("register candidate hosts");
    super::register_witnesses(&mut registry, &catalog);
    registry.bind_vm_cached(&mut vm).expect("bind hosts");
    let status = vm.run().unwrap_or_else(|error| panic!("run: {error}"));
    assert_eq!(status, VmStatus::Halted);
    match vm.stack() {
        [Value::Bool(value)] => *value,
        other => panic!("expected one bool, got {other:?}"),
    }
}

/// Two named fields exchange string values read from those same fields.
#[test]
fn named_fields_swap_strings_before_either_write() {
    assert!(
        bool_result(
            r#"local t = {}
t.a = "left"
t.b = "right"
t.a, t.b = t.b, t.a
return rawequal(t.a, "right")
"#,
        ),
        "the left field must receive the saved right string"
    );
    assert!(
        bool_result(
            r#"local t = {}
t.a = "left"
t.b = "right"
t.a, t.b = t.b, t.a
return rawequal(t.b, "left")
"#,
        ),
        "the right field must receive the saved left string"
    );
}

/// Bool fields use the same save-then-write order as strings.
#[test]
fn named_fields_swap_bools_before_either_write() {
    assert!(
        bool_result(
            r#"local t = {}
t.a = true
t.b = false
t.a, t.b = t.b, t.a
return rawequal(t.a, false)
"#,
        ),
        "the left field must receive the saved false"
    );
    assert!(
        bool_result(
            r#"local t = {}
t.a = true
t.b = false
t.a, t.b = t.b, t.a
return rawequal(t.b, true)
"#,
        ),
        "the right field must receive the saved true"
    );
}

/// Literal bool and string values are stored, not only field reads.
#[test]
fn literal_bool_and_string_are_stored_on_named_fields() {
    assert!(bool_result(
        r#"local t = {}
t.flag = false
t.text = "old"
t.flag, t.text = true, "new"
return rawequal(t.flag, true)
"#,
    ));
    assert!(bool_result(
        r#"local t = {}
t.flag = false
t.text = "old"
t.flag, t.text = true, "new"
return rawequal(t.text, "new")
"#,
    ));
}

/// Already issued tables move by field read. `rawequal` checks identity.
#[test]
fn field_read_moves_issued_tables_by_identity() {
    assert!(
        bool_result(
            r#"local left = {}
local right = {}
local t = {}
t.a = left
t.b = right
t.a, t.b = t.b, t.a
return rawequal(t.a, right)
"#,
        ),
        "the left field must be the original right table"
    );
    assert!(
        bool_result(
            r#"local left = {}
local right = {}
local t = {}
t.a = left
t.b = right
t.a, t.b = t.b, t.a
return rawequal(t.b, left)
"#,
        ),
        "the right field must be the original left table"
    );
}

/// Two local tables are different receivers. Reusing the first would drop
/// the write to the second.
#[test]
fn distinct_local_receivers_exchange_named_fields() {
    assert!(bool_result(
        r#"local a = {}
local b = {}
a.x = "from-a"
b.y = "from-b"
a.x, b.y = b.y, a.x
return rawequal(a.x, "from-b")
"#,
    ));
    assert!(bool_result(
        r#"local a = {}
local b = {}
a.x = "from-a"
b.y = "from-b"
a.x, b.y = b.y, a.x
return rawequal(b.y, "from-a")
"#,
    ));
}

/// A field read can move an already issued table onto another local.
#[test]
fn field_read_moves_an_issued_table_to_another_receiver() {
    assert!(
        bool_result(
            r#"local payload = {}
local holder = {}
holder.src = payload
local dest = {}
dest.out, holder.src = holder.src, nil
return rawequal(dest.out, payload)
"#,
        ),
        "the destination must keep the original table"
    );
    assert!(
        bool_result(
            r#"local payload = {}
local holder = {}
holder.src = payload
local dest = {}
dest.out, holder.src = holder.src, nil
return rawequal(holder.src, nil)
"#,
        ),
        "nil is stored only after the field read"
    );
}

/// The right value is written first. The left value is the one that remains.
#[test]
fn repeated_field_keeps_the_left_value() {
    assert!(
        bool_result(
            r#"local t = {}
t.a = "init"
t.a, t.a = "left", "right"
return rawequal(t.a, "left")
"#,
        ),
        "reverse writes must leave the leftmost value"
    );
}

/// The same two strings written forward leave the opposite value, so the
/// repeated-field result is not just the last source value.
#[test]
fn forward_writes_of_the_same_strings_keep_the_right_value() {
    assert!(
        bool_result(
            r#"local t = {}
t.a = "init"
t.a = "left"
t.a = "right"
return rawequal(t.a, "right")
"#,
        ),
        "explicit forward writes must be observable as the opposite result"
    );
}

/// `nil` is applied after every right-hand read. An unwritten field stays.
#[test]
fn nil_clear_uses_values_read_before_any_write() {
    assert!(
        bool_result(
            r#"local t = {}
t.keep = "stay"
t.src = "old"
t.dst = "other"
t.src, t.dst = nil, t.src
return rawequal(t.dst, "old")
"#,
        ),
        "the later read must see src before nil is stored"
    );
    assert!(
        bool_result(
            r#"local t = {}
t.keep = "stay"
t.src = "old"
t.dst = "other"
t.src, t.dst = nil, t.src
return rawequal(t.src, nil)
"#,
        ),
        "the saved nil must clear src"
    );
    assert!(
        bool_result(
            r#"local t = {}
t.keep = "stay"
t.src = "old"
t.dst = "other"
t.src, t.dst = nil, t.src
return rawequal(t.keep, "stay")
"#,
        ),
        "a field that is not a target must keep its value"
    );
}
