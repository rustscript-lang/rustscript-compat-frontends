//! Non-mutating, test-only IR observation of the existing JS frontend.
//! No guest execution and no production frontend/profile is added.
use pd_vm_compat_frontends::JAVASCRIPT_ES5_PLUGIN;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use vm::SourcePlugin;

const AUDIT_ID: &str = "javascript-es5-realm-audit-test";

struct RealmAudit {
    ordinary: AtomicBool,
    functions: AtomicUsize,
}

impl SourcePlugin for RealmAudit {
    fn descriptor(&self) -> vm::SourceFrontendDescriptor {
        vm::SourceFrontendDescriptor {
            id: vm::FrontendId::new(AUDIT_ID),
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
        let ir = JAVASCRIPT_ES5_PLUGIN.parse_source(source, context)?;
        self.ordinary.store(
            ir.functions
                .iter()
                .any(|function| function.name == "__compat_es5_object_proto"),
            Ordering::SeqCst,
        );
        self.functions.store(ir.functions.len(), Ordering::SeqCst);
        Ok(ir)
    }
}

fn ordinary_helpers(source: &str) -> bool {
    eprintln!("REALM_AUDIT_SOURCE {source}");
    let plugin: &'static RealmAudit = Box::leak(Box::new(RealmAudit {
        ordinary: AtomicBool::new(false),
        functions: AtomicUsize::new(0),
    }));
    let catalog = pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog();
    let options = vm::CompileSourceFileOptions::new()
        .with_host_api_catalog(catalog)
        .with_source_plugin(plugin);
    vm::compile_source_with_flavor_and_options_with_capture_certification(
        source,
        vm::SourceFlavor::Plugin(vm::FrontendId::new(AUDIT_ID)),
        options,
    )
    .unwrap_or_else(|error| panic!("source must compile: {source}\n{error:?}"));
    let ordinary = plugin.ordinary.load(Ordering::SeqCst);
    eprintln!(
        "REALM_AUDIT_COMPILED ordinary={ordinary} functions={} guest_runs=0",
        plugin.functions.load(Ordering::SeqCst),
    );
    ordinary
}

#[test]
fn discarded_array_string_forward_preserves_heap_free_route() {
    assert!(!ordinary_helpers(
        "var a=[7];var key='0';var result=0;switch(1){case 1:key=0;case 2:result=a[key];}result===7;"
    ));
}

#[test]
fn discarded_array_string_reverse_preserves_heap_free_route() {
    assert!(!ordinary_helpers(
        "var a=[7];var key=0;var result=0;switch(2){case 1:key='0';case 2:result=a[key];}result===7;"
    ));
}

#[test]
fn dead_array_string_read_does_not_publish_realm_demand() {
    assert!(!ordinary_helpers("var a=[7];var key='0';throw 7;a[key];"));
}

#[test]
fn numeric_loop_control_keeps_heap_free_route() {
    assert!(!ordinary_helpers(
        "var a=[7];var key='0';var i=0;while(i<0){key=0;a[key];i++;}7;"
    ));
}

#[test]
fn uncaught_scalar_throw_keeps_heap_free_route() {
    assert!(!ordinary_helpers("var a=[7];var key='0';throw 7;"));
}

#[test]
fn surviving_array_string_call_requires_ordinary_helpers() {
    assert!(ordinary_helpers(
        "var a=[7];var key='hasOwnProperty';a[key]('0');"
    ));
}

#[test]
fn discarded_array_proof_keeps_independent_object_realm_demand() {
    assert!(ordinary_helpers(
        "var o={x:7};var a=[7];var key=0;var result=0;switch(2){case 1:key='0';case 2:result=a[key];}o.x===7&&result===7;"
    ));
}

#[test]
fn dead_array_string_operand_keeps_independent_nullish_realm() {
    assert!(ordinary_helpers("var a=[7];var key='0';null.x + a[key];"));
}

#[test]
fn dead_array_string_call_does_not_publish_realm_demand() {
    assert!(!ordinary_helpers(
        "var a=[7];var key='hasOwnProperty';throw 7;a[key]('0');"
    ));
}

#[test]
fn dead_object_string_read_preserves_independent_realm_demand() {
    assert!(ordinary_helpers("var o={a:1};var key='a';throw 7;o[key];"));
}

#[test]
fn dead_null_string_read_does_not_publish_realm_demand() {
    assert!(!ordinary_helpers(
        "var o=null;var key=\"a\";throw 7;o[key];"
    ));
}

#[test]
fn dead_null_string_call_does_not_publish_realm_demand() {
    assert!(!ordinary_helpers(
        "var o=null;var key=\"a\";throw 7;o[key]();"
    ));
}
