use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use vm::{Value, Vm, VmStatus, compile_source_file_with_options};

struct Project(PathBuf);

impl Project {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        loop {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!(
                "typescript-mts-modules-{}-{id}",
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

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn nested_ts_and_mts_imports_preserve_type_identity_and_run() {
    let project = Project::new();
    let nested = project.0.join("nested");
    fs::create_dir(&nested).unwrap();
    fs::write(
        nested.join("types.mts"),
        "export interface Profile { name: string; }\n",
    )
    .unwrap();
    fs::write(
        nested.join("make.ts"),
        "import type { Profile } from \"./types.mts\";\nexport function make(name: string): Profile { return { name }; }\n",
    )
    .unwrap();
    let main = project.0.join("main.mts");
    fs::write(
        &main,
        "import { make as create } from \"./nested/make.ts\";\nimport type { Profile as UserProfile } from \"./nested/types.mts\";\nconst profile: UserProfile = create(\"Ada\");\nconst name = profile.name;\nname;\n",
    )
    .unwrap();

    let compiled =
        compile_source_file_with_options(&main, pd_vm_compat_frontends::compile_options())
            .expect("mixed .ts/.mts modules should resolve relative types from each owner");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(machine.run().unwrap(), VmStatus::Halted);
    assert_eq!(machine.stack(), &[Value::string("Ada")]);
}
