use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use vm::{SourcePathError, Value, Vm, VmStatus, compile_source_file_with_options};

static NEXT_TEMP_FILE_ID: AtomicUsize = AtomicUsize::new(0);

struct TempSourceFile {
    path: PathBuf,
}

impl TempSourceFile {
    fn new(extension: &str, source: &str) -> Self {
        loop {
            let id = NEXT_TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "rustscript_typescript_contract_{}_{}.{}",
                std::process::id(),
                id,
                extension
            ));
            let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create temporary source file {}: {error}", path.display()),
            };
            if let Err(error) = file.write_all(source.as_bytes()) {
                drop(file);
                let _ = fs::remove_file(&path);
                panic!("write temporary source file {}: {error}", path.display());
            }
            return Self { path };
        }
    }
}

impl Drop for TempSourceFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn assert_extension_rejected(extension: &str) {
    let fixture = TempSourceFile::new(extension, "const result = 7; result;\n");
    let error = match compile_source_file_with_options(
        &fixture.path,
        pd_vm_compat_frontends::compile_options(),
    ) {
        Ok(_) => panic!("JavaScript-family extension .{extension} should be rejected"),
        Err(error) => error,
    };

    assert!(
        matches!(&error, SourcePathError::UnsupportedExtension(found) if found == extension)
            || matches!(&error, SourcePathError::MissingFrontendPlugin(_)),
        "expected an extension/plugin error for .{extension}, got {error:?}"
    );
}

fn assert_typescript_file_compiles_and_runs(extension: &str) {
    let fixture = TempSourceFile::new(extension, "const name: string = \"Ada\"; name;\n");
    let compiled =
        compile_source_file_with_options(&fixture.path, pd_vm_compat_frontends::compile_options())
            .unwrap_or_else(|error| panic!(".{extension} source should compile: {error:?}"));

    let mut vm = Vm::new(compiled.program);
    assert_eq!(
        vm.run().expect("compiled program should run"),
        VmStatus::Halted
    );
    assert_eq!(vm.stack(), &[Value::string("Ada")]);
}

#[test]
fn ts_file_compiles_and_runs_with_string_result() {
    assert_typescript_file_compiles_and_runs("ts");
}

#[test]
fn mts_file_compiles_and_runs_with_string_result() {
    assert_typescript_file_compiles_and_runs("mts");
}

#[test]
fn js_file_extension_is_rejected() {
    assert_extension_rejected("js");
}

#[test]
fn mjs_file_extension_is_rejected() {
    assert_extension_rejected("mjs");
}

#[test]
fn cjs_file_extension_is_rejected() {
    assert_extension_rejected("cjs");
}
