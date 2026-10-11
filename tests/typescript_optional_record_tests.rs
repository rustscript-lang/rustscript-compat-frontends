use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use vm::{
    TypeSchema, Value, Vm, VmStatus, analyze_source_from_string_with_options,
    compile_source_file_with_options, compile_source_with_flavor_and_options,
    compiler::{FrontendId, SourceFlavor, analyze_source_file_with_options},
};

fn flavor() -> SourceFlavor {
    SourceFlavor::Plugin(FrontendId::new("typescript"))
}

fn profile_catalog() -> Arc<vm::HostApiCatalog> {
    let profile = vm::HostStructSchema::new(
        "Profile",
        vec![
            vm::HostStructField::new("name", vm::HostTypeSchema::String),
            vm::HostStructField::new(
                "nickname",
                vm::HostTypeSchema::Optional(Box::new(vm::HostTypeSchema::String)),
            ),
        ],
    );
    let mut builder = vm::HostApiBuilder::new();
    builder.named_struct(profile.clone());
    builder.function(vm::HostFunctionSchema::with_return(
        "profile::accept",
        vec![vm::HostParamSchema::value("profile", profile.as_type())],
        vm::HostTypeSchema::Bool,
    ));
    builder.function(vm::HostFunctionSchema::with_return(
        "profile::load",
        Vec::new(),
        profile.as_type(),
    ));
    Arc::new(builder.build().expect("valid optional Profile catalog"))
}

struct CountAccepted(Arc<AtomicUsize>);

impl vm::HostFunction for CountAccepted {
    fn call(&mut self, _machine: &mut Vm, args: &[Value]) -> vm::VmResult<vm::CallOutcome> {
        assert_eq!(args.len(), 1);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(vm::CallOutcome::Return(vm::CallReturn::one(Value::Bool(
            true,
        ))))
    }
}

fn bind_accept(
    catalog: &Arc<vm::HostApiCatalog>,
    calls: Arc<AtomicUsize>,
    program: vm::Program,
) -> Vm {
    let schema = vm::catalog_import_schemas(catalog, "profile::accept")
        .into_iter()
        .next()
        .expect("accept schema comes from the catalog");
    let mut registry = vm::HostFunctionRegistry::new();
    registry
        .register_catalog(schema, move || Box::new(CountAccepted(Arc::clone(&calls))))
        .expect("accept binding matches the catalog");
    let mut machine = Vm::new(program);
    registry.bind_vm_cached(&mut machine).unwrap();
    machine
}

struct CountLoaded(Arc<AtomicUsize>);

impl vm::HostFunction for CountLoaded {
    fn call(&mut self, _machine: &mut Vm, args: &[Value]) -> vm::VmResult<vm::CallOutcome> {
        assert!(args.is_empty());
        self.0.fetch_add(1, Ordering::SeqCst);
        let profile = Value::map(vec![(Value::string("name"), Value::string("Kara"))]);
        Ok(vm::CallOutcome::Return(vm::CallReturn::one(profile)))
    }
}

fn bind_load(
    catalog: &Arc<vm::HostApiCatalog>,
    calls: Arc<AtomicUsize>,
    program: vm::Program,
) -> Vm {
    let schema = vm::catalog_import_schemas(catalog, "profile::load")
        .into_iter()
        .next()
        .expect("load schema comes from the catalog");
    let mut registry = vm::HostFunctionRegistry::new();
    registry
        .register_catalog(schema, move || Box::new(CountLoaded(Arc::clone(&calls))))
        .expect("load binding matches the catalog");
    let mut machine = Vm::new(program);
    registry.bind_vm_cached(&mut machine).unwrap();
    machine
}

fn compile_options(catalog: Arc<vm::HostApiCatalog>) -> vm::CompileSourceFileOptions {
    pd_vm_compat_frontends::compile_options_with_catalog(catalog)
}

#[test]
fn inferred_host_record_optional_reads_keep_callable_and_vm_schema() {
    let source = r#"import { load } from "profile";
import type { Profile } from "profile";
const loaded = load();
const nickname = loaded.nickname;
nickname;
"#;
    let catalog = profile_catalog();
    let hints = pd_vm_compat_frontends::collect_typescript_type_hints_with_options(
        source,
        compile_options(Arc::clone(&catalog)),
    )
    .expect("host-returned record hints accept the catalog schema");
    let nickname_hint = hints
        .iter()
        .find(|hint| hint.name == "nickname")
        .expect("inferred optional property hint exists");
    assert_eq!(
        nickname_hint.schema,
        Some(TypeSchema::Optional(Box::new(TypeSchema::String)))
    );
    let model = analyze_source_from_string_with_options(
        "inferred-host-record.ts",
        source,
        compile_options(Arc::clone(&catalog)),
    )
    .expect("host-returned optional record analysis is available");
    assert!(model.diagnostics().is_empty());

    let compiled = compile_source_with_flavor_and_options(
        source,
        flavor(),
        compile_options(Arc::clone(&catalog)),
    )
    .expect("host-returned record compiles under the same catalog");
    let calls = Arc::new(AtomicUsize::new(0));
    let mut machine = bind_load(&catalog, Arc::clone(&calls), compiled.program);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::Null]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn optional_named_fields_accept_omission_null_and_values_and_read_missing_as_null() {
    let source = r#"import { accept } from "profile";
import type { Profile } from "profile";
const omittedForHost: Profile = { name: "Ada" };
const explicitNullForHost: Profile = { name: "Bea", nickname: null };
const presentForHost: Profile = { name: "Lin", nickname: "Linnie" };
const omitted: Profile = { name: "Ada" };
const explicitNull: Profile = { name: "Bea", nickname: null };
const present: Profile = { name: "Lin", nickname: "Linnie" };
const missingValue = omitted.nickname;
const nullValue = explicitNull.nickname;
const presentValue = present.nickname;
accept(omittedForHost);
accept(explicitNullForHost);
accept(presentForHost);
missingValue;
nullValue;
presentValue;
"#;
    let catalog = profile_catalog();
    let hints = pd_vm_compat_frontends::collect_typescript_type_hints_with_options(
        source,
        compile_options(Arc::clone(&catalog)),
    )
    .expect("public hints accept optional records through the supplied catalog");
    let hint = |name: &str| {
        hints
            .iter()
            .find(|hint| hint.name == name)
            .unwrap_or_else(|| panic!("missing TypeScript hint for {name}"))
    };
    assert_eq!(
        hint("missingValue").schema,
        Some(TypeSchema::Optional(Box::new(TypeSchema::String)))
    );
    assert_eq!(
        hint("nullValue").schema,
        Some(TypeSchema::Optional(Box::new(TypeSchema::String)))
    );
    assert_eq!(hint("presentValue").schema, hint("missingValue").schema);
    assert_eq!(
        hint("omitted").schema,
        Some(TypeSchema::Named("Profile".to_owned(), Vec::new()))
    );
    analyze_source_from_string_with_options(
        "optional-record.ts",
        source,
        compile_options(Arc::clone(&catalog)),
    )
    .expect("analysis accepts the same checked optional-record source");

    let compiled = compile_source_with_flavor_and_options(
        source,
        flavor(),
        compile_options(Arc::clone(&catalog)),
    )
    .expect("typed optional record calls should compile");
    let calls = Arc::new(AtomicUsize::new(0));
    let mut machine = bind_accept(&catalog, Arc::clone(&calls), compiled.program);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(
        machine.stack(),
        &[
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true),
            Value::Null,
            Value::Null,
            Value::string("Linnie"),
        ]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

struct ProjectDir(PathBuf);

impl ProjectDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .expect("test TMPDIR is assigned");
        loop {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!(
                "typescript-optional-modules-{}-{id}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create test project {}: {error}", path.display()),
            }
        }
    }
}

impl Drop for ProjectDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn in_memory_optional_record_reads_missing_as_vm_null() {
    let source = r#"interface Profile { name: string; nickname?: string; }
const profile: Profile = { name: "Ada" };
profile.nickname;
"#;
    let compiled = compile_source_with_flavor_and_options(
        source,
        flavor(),
        compile_options(profile_catalog()),
    )
    .expect("local TypeScript optional record should compile");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::Null]);
}

#[test]
fn file_source_optional_record_reads_missing_as_vm_null() {
    let project = ProjectDir::new();
    let path = project.0.join("main.ts");
    fs::write(
        &path,
        r#"interface Profile { name: string; nickname?: string; }
const profile: Profile = { name: "Ada" };
profile.nickname;
"#,
    )
    .unwrap();
    let compiled =
        compile_source_file_with_options(&path, pd_vm_compat_frontends::compile_options())
            .expect("TypeScript file optional record should compile");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::Null]);
}

#[test]
fn imported_nested_and_generic_optional_records_preserve_vm_null_semantics() {
    let project = ProjectDir::new();
    let nested = project.0.join("nested");
    fs::create_dir(&nested).unwrap();
    fs::write(
        nested.join("types.mts"),
        "export interface Profile { name: string; nickname?: string; }\n",
    )
    .unwrap();
    let main = project.0.join("main.mts");
    let cases = [
        (
            "imported named field",
            r#"const profile: Profile = { name: "Ada" };
profile.nickname;
"#,
        ),
        (
            "generic named field",
            r#"interface Envelope<T> { value: T; note?: string; }
const profile: Profile = { name: "Ada" };
const envelope: Envelope<Profile> = { value: profile };
envelope.note;
"#,
        ),
        (
            "nested generic named field",
            r#"interface Envelope<T> { value: T; note?: string; }
const profile: Profile = { name: "Ada" };
const envelope: Envelope<Profile> = { value: profile };
envelope.value.nickname;
"#,
        ),
    ];
    for (label, body) in cases {
        let source = format!(
            r#"import type {{ Profile }} from "./nested/types.mts";
{body}"#
        );
        fs::write(&main, source).unwrap();
        let compiled =
            compile_source_file_with_options(&main, pd_vm_compat_frontends::compile_options())
                .unwrap_or_else(|error| panic!("{label}: {error}"));
        let model =
            analyze_source_file_with_options(&main, pd_vm_compat_frontends::compile_options())
                .unwrap_or_else(|error| panic!("{label} analysis: {error}"));
        assert!(
            model.diagnostics().is_empty(),
            "{label} analysis diagnostics: {:?}",
            model.diagnostics()
        );
        let mut machine = Vm::new(compiled.program);
        assert_eq!(
            machine
                .run()
                .unwrap_or_else(|error| panic!("{label}: {error}")),
            VmStatus::Halted,
            "{label}"
        );
        assert_eq!(machine.stack(), &[Value::Null], "{label}");
    }
}

#[test]
fn wrong_present_and_missing_required_fields_reject_before_host_side_effects() {
    let cases = [
        (
            "wrong present type",
            "import { accept } from \"profile\";\naccept({ name: 7 });\n",
        ),
        (
            "missing required field",
            "import { accept } from \"profile\";\naccept({ nickname: \"Ada\" });\n",
        ),
    ];
    for (label, source) in cases {
        let catalog = profile_catalog();
        let calls = Arc::new(AtomicUsize::new(0));
        let options = compile_options(Arc::clone(&catalog));
        let compile_result = compile_source_with_flavor_and_options(source, flavor(), options);
        match compile_result {
            Err(error) => assert!(
                !error.to_string().is_empty(),
                "{label} should produce a source diagnostic"
            ),
            Ok(compiled) => {
                let mut machine = bind_accept(&catalog, Arc::clone(&calls), compiled.program);
                let _ = machine.run();
                panic!("{label} unexpectedly compiled and reached its host binding");
            }
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0, "{label} invoked the host");
        let model = analyze_source_from_string_with_options(
            "invalid-optional-record.ts",
            source,
            compile_options(Arc::clone(&catalog)),
        )
        .expect("analysis returns a model containing source diagnostics");
        assert!(
            !model.diagnostics().is_empty(),
            "{label} should produce a semantic diagnostic"
        );
        assert!(
            pd_vm_compat_frontends::collect_typescript_type_hints_with_options(
                source,
                compile_options(Arc::clone(&catalog)),
            )
            .is_err(),
            "{label} should also be rejected by public type hints"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0, "{label} invoked the host");
    }
}
