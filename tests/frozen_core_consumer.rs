//! Frozen dependency proof and the exact TypeScript/Lua example corpus.

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[path = "javascript_es5/flat_owner.rs"]
mod flat_owner;

use vm::{
    SourcePathError, Value, Vm, VmStatus, compile_source, compile_source_file_with_options,
    compile_source_with_flavor_and_options, decode_program, encode_program,
};

#[allow(clippy::duplicate_mod)]
#[path = "common/core_provenance.rs"]
mod core_provenance;
const FROZEN_STRINGS_RSS: &str = include_str!("fixtures/frozen_stdlib/strings.rss");
const FROZEN_STRINGS_BYTES: usize = 3522;
const EXPECTED_TS_LUA_EXAMPLES: usize = 5;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn locked_core_package() -> serde_json::Value {
    let root = manifest_dir();
    let output = Command::new("cargo")
        .args(["metadata", "--locked", "--format-version", "1"])
        .current_dir(&root)
        .output()
        .expect("run locked Cargo metadata");
    assert!(
        output.status.success(),
        "locked Cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("Cargo metadata JSON");
    core_provenance::assert_locked_core_provenance(&root, &metadata).clone()
}

fn scratch_root() -> PathBuf {
    std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

struct ScratchTree {
    root: PathBuf,
}

impl ScratchTree {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        loop {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let root = scratch_root().join(format!(
                "compat-frontends-corpus-{}-{id}",
                std::process::id()
            ));
            match fs::create_dir(&root) {
                Ok(()) => return Self { root },
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create scratch tree {}: {error}", root.display()),
            }
        }
    }
}

impl Drop for ScratchTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Flat synthetic CLI inputs: `entry-qa-*` files created directly inside one
/// temporary directory. `ENTRY_QA_FLAT_DIR` selects that directory when set
/// (it must then already exist); when unset, `std::env::temp_dir()` supplies
/// the platform temporary directory, including its normal `TMPDIR` handling.
///
/// These are tiny CLI negative fixtures, not a source tree: no subdirectory is
/// created and no pre-existing file is overwritten. The lock keeps selected
/// tests serialized so the flat directory never holds more than the permitted
/// simultaneous file count, and cleanup runs while unwinding.
struct FlatEntryQa {
    root: PathBuf,
    created: Vec<PathBuf>,
    _serial: std::sync::MutexGuard<'static, ()>,
}

static ENTRY_QA_FLAT_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl FlatEntryQa {
    fn new() -> Self {
        Self::from_root(flat_owner::select_root())
    }

    fn from_root(root: PathBuf) -> Self {
        assert!(
            root.is_dir(),
            "flat entry-qa directory is not a directory: {}",
            root.display()
        );
        let serial = ENTRY_QA_FLAT_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Self {
            root,
            created: Vec::new(),
            _serial: serial,
        }
    }

    fn owned_count(&self) -> usize {
        self.created.len()
    }

    /// Atomic exclusive acquisition at a concrete target. Registration happens
    /// immediately after `create_new` succeeds and before any fallible
    /// follow-up; on failure nothing is registered and nothing foreign is
    /// removed or replaced.
    fn try_acquire(&mut self, path: &Path) -> io::Result<fs::File> {
        flat_owner::validate_flat_target(&self.root, path)?;
        let leaf = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if !leaf.starts_with("entry-qa-") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("flat fixture target is not entry-qa-*: {}", path.display()),
            ));
        }
        if self.created.len() >= flat_owner::MAX_OWNED_FILES {
            return Err(io::Error::other(format!(
                "flat helper already owns {} files",
                self.created.len()
            )));
        }
        let file = flat_owner::create_owned(path)?;
        self.created.push(path.to_path_buf());
        Ok(file)
    }

    fn try_source_at(&mut self, label: &str, path: &Path, source: &str) -> io::Result<PathBuf> {
        let (_, expected_extension) = flat_owner::input_label(label);
        if source.len() > flat_owner::MAX_INPUT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("flat input exceeds {} bytes", flat_owner::MAX_INPUT_BYTES),
            ));
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let actual_extension = Path::new(name).extension().and_then(|ext| ext.to_str());
        if actual_extension != Some(expected_extension) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "flat input target extension does not match the logical label: {}",
                    path.display()
                ),
            ));
        }
        let mut file = self.try_acquire(path)?;
        file.write_all(source.as_bytes())?;
        let bytes = file.metadata()?.len();
        drop(file);
        if bytes != source.len() as u64 || bytes > flat_owner::MAX_INPUT_BYTES as u64 {
            return Err(io::Error::other(format!(
                "flat input wrote {bytes} bytes for {} source bytes",
                source.len()
            )));
        }
        eprintln!("ENTRY_QA_FLAT_INPUT path={} bytes={bytes}", path.display());
        Ok(path.to_path_buf())
    }

    fn source(&mut self, name: &str, source: &str) -> PathBuf {
        let path = self.root.join(flat_owner::input_name(name));
        self.try_source_at(name, &path, source)
            .unwrap_or_else(|error| {
                panic!("create flat entry-qa input {}: {error}", path.display())
            })
    }

    /// Unique `--emit-vmbc` target for a negative row whose expected outcome is
    /// that no artifact file is created. It is deliberately not reserved: this
    /// is the path whose absence proves the refusal created no VMBC.
    fn planned_artifact(&mut self, kind: &str) -> PathBuf {
        let path = self.root.join(flat_owner::artifact_name(kind));
        assert!(
            fs::symlink_metadata(&path).is_err(),
            "planned artifact target must not exist: {}",
            path.display()
        );
        eprintln!("ENTRY_QA_FLAT_PLANNED_ARTIFACT path={}", path.display());
        path
    }

    /// Remove only the exact paths acquired by this helper. No directory scan:
    /// another process's `entry-qa-*` files are not ours to inspect or reject.
    fn cleanup(&mut self) {
        let mut removed = 0usize;
        let mut remaining = Vec::new();
        let mut failure = None;
        for path in self.created.drain(..) {
            match fs::remove_file(&path) {
                Ok(()) => removed += 1,
                Err(error) => {
                    if std::thread::panicking() {
                        eprintln!(
                            "ENTRY_QA_FLAT_CLEANUP_FAILED path={} error={error}",
                            path.display()
                        );
                    } else {
                        failure = Some(format!("remove flat input {}: {error}", path.display()));
                    }
                    remaining.push(path);
                }
            }
        }
        eprintln!(
            "ENTRY_QA_FLAT_CLEANUP root={} removed={removed} remaining_owned={}",
            self.root.display(),
            remaining.len()
        );
        if !remaining.is_empty() {
            eprintln!("ENTRY_QA_FLAT_CLEANUP_REMAINING {remaining:?}");
        }
        if let Some(message) = failure {
            panic!("{message}");
        }
    }
}

impl Drop for FlatEntryQa {
    fn drop(&mut self) {
        self.cleanup();
    }
}

const FLAT_ENTRY_WORKER_TEST: &str = "flat_entry_qa_worker_process_holds_owned_input_until_release";

fn spawn_flat_entry_worker(
    root: &Path,
) -> (
    flat_owner::WorkerGuard,
    BufReader<std::process::ChildStdout>,
) {
    let exe = std::env::current_exe().expect("current test executable");
    spawn_flat_entry_worker_at(root, &exe)
}

fn spawn_flat_entry_worker_at(
    root: &Path,
    exe: &Path,
) -> (
    flat_owner::WorkerGuard,
    BufReader<std::process::ChildStdout>,
) {
    let child = Command::new(exe)
        .args(["--exact", FLAT_ENTRY_WORKER_TEST, "--nocapture"])
        .env("ENTRY_QA_FLAT_WORKER_ROOT", root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap_or_else(|error| panic!("spawn flat entry-qa worker process: {error}"));
    // The guard owns the child before the pipe take and before any later
    // second spawn or fallible follow-up.
    let mut worker = flat_owner::WorkerGuard::new(root, child);
    let stdout = worker.child().stdout.take().expect("worker stdout pipe");
    (worker, BufReader::new(stdout))
}

fn read_entry_acquired(reader: &mut BufReader<std::process::ChildStdout>) -> io::Result<String> {
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(io::Error::other(
                "worker exited before its ownership marker",
            ));
        }
        if let Some(path) = line.trim().strip_prefix("ENTRY_QA_WORKER_ACQUIRED path=") {
            return Ok(path.to_owned());
        }
    }
}

#[test]
fn flat_entry_qa_names_are_owner_scoped_and_foreign_files_are_not_owned() {
    let root = flat_owner::select_root();
    let foreign = flat_owner::DriverFile::create(
        &root,
        "entry-qa-foreign-metadata-sentinel.js",
        b"foreign-entry-metadata",
    );
    let observed = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
    let observed_in_closure = Arc::clone(&observed);
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut flat = FlatEntryQa::from_root(root.clone());
        let first = flat.source("entry-qa-meta.js", "1===1;");
        let second = flat.source("entry-qa-meta.js", "1===1;");
        assert_ne!(first, second, "owner token must separate repeated labels");
        let pid = std::process::id().to_string();
        for path in [&first, &second] {
            let name = path.file_name().unwrap().to_str().unwrap();
            assert!(name.starts_with("entry-qa-meta-"), "{name}");
            assert!(name.ends_with(".js"), "{name}");
            assert!(
                name.contains(&format!("-{pid}-")),
                "owner token must be decimal pid: {name}"
            );
            assert_eq!(path.parent(), Some(root.as_path()));
            observed_in_closure.lock().unwrap().push(path.clone());
        }
        let planned = flat.planned_artifact("bad");
        let planned_name = planned.file_name().unwrap().to_str().unwrap();
        assert!(planned_name.starts_with(&format!("entry-qa-{pid}-")));
        assert!(planned_name.ends_with("-bad.vmbc"), "{planned_name}");
        assert!(
            fs::symlink_metadata(&planned).is_err(),
            "a planned emit target must not be created"
        );
        observed_in_closure.lock().unwrap().push(planned.clone());
        assert_eq!(flat.owned_count(), 2);
    }));
    assert!(
        outcome.is_ok(),
        "a foreign entry-qa-* file must not fail this helper's cleanup"
    );
    for path in observed.lock().unwrap().iter() {
        assert!(
            fs::symlink_metadata(path).is_err(),
            "flat entry-qa path leaked: {}",
            path.display()
        );
    }
    assert_eq!(fs::read(foreign.path()).unwrap(), b"foreign-entry-metadata");
    assert!(foreign.path().is_file());
}

#[test]
fn flat_entry_qa_refuses_foreign_files_without_owning_them() {
    let root = flat_owner::select_root();
    let mut flat = FlatEntryQa::from_root(root.clone());
    let source_path = root.join("entry-qa-foreign-source.js");
    let foreign = flat_owner::DriverFile::create_at(&source_path, b"foreign-entry-source");
    let error = flat
        .try_source_at("entry-qa-foreign-source.js", &source_path, "1===1;")
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(flat.owned_count(), 0);
    assert_eq!(fs::read(foreign.path()).unwrap(), b"foreign-entry-source");

    let oversize = "x".repeat(flat_owner::MAX_INPUT_BYTES + 1);
    let oversize_path = root.join(flat_owner::input_name("entry-qa-oversize.js"));
    let error = flat
        .try_source_at("entry-qa-oversize.js", &oversize_path, &oversize)
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(fs::symlink_metadata(&oversize_path).is_err());

    let escape = root
        .join("..")
        .join(format!("entry-qa-escape-{}.js", std::process::id()));
    let error = flat
        .try_source_at("entry-qa-escape.js", &escape, "1===1;")
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(fs::symlink_metadata(&escape).is_err());
    assert_eq!(flat.owned_count(), 0);
}

#[cfg(unix)]
#[test]
fn flat_entry_qa_refuses_dangling_symlinks_without_following_them() {
    let root = flat_owner::select_root();
    let mut flat = FlatEntryQa::from_root(root.clone());
    let link = root.join("entry-qa-dangling-source.js");
    let target = root.join("entry-qa-target.js");
    assert!(fs::symlink_metadata(&target).is_err());
    let symlink = flat_owner::DriverSymlink::create(&link, &target);
    let error = flat
        .try_source_at("entry-qa-dangling-source.js", &link, "1===1;")
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(fs::symlink_metadata(&target).is_err());
    assert_eq!(flat.owned_count(), 0);
    drop(symlink);
}

#[test]
fn flat_entry_qa_owned_files_are_removed_during_unwind() {
    let root = flat_owner::select_root();
    let foreign = flat_owner::DriverFile::create(
        &root,
        "entry-qa-foreign-unwind-sentinel.js",
        b"foreign-entry-unwind",
    );
    let owned = Arc::new(Mutex::new(None::<PathBuf>));
    let owned_in_panic = Arc::clone(&owned);
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut flat = FlatEntryQa::from_root(root.clone());
        let file = flat.source("entry-qa-unwind.js", "1===1;");
        *owned_in_panic.lock().unwrap() = Some(file);
        panic!("deliberate post-acquisition unwind");
    }));
    assert!(outcome.is_err());
    let file = owned.lock().unwrap().take().expect("owned path recorded");
    assert!(!file.exists(), "owned entry-qa input leaked during unwind");
    assert_eq!(fs::read(foreign.path()).unwrap(), b"foreign-entry-unwind");

    let early = {
        let mut flat = FlatEntryQa::from_root(root.clone());
        let path = root.join(flat_owner::input_name("entry-qa-early.js"));
        let handle = flat.try_acquire(&path).expect("reserve owned path");
        drop(handle);
        assert!(path.is_file());
        path
    };
    assert!(
        !early.exists(),
        "owned entry-qa reservation leaked after early drop"
    );
    assert!(foreign.path().is_file());
}

#[test]
fn flat_entry_qa_worker_process_holds_owned_input_until_release() {
    let Some(root) = std::env::var_os("ENTRY_QA_FLAT_WORKER_ROOT") else {
        return;
    };
    let mut flat = FlatEntryQa::from_root(PathBuf::from(root));
    let file = flat.source("entry-qa-worker.js", "1===1;");
    println!("ENTRY_QA_WORKER_ACQUIRED path={}", file.display());
    std::io::stdout().flush().expect("flush acquisition marker");
    let mut release = [0u8; 1];
    let read = std::io::stdin()
        .read(&mut release)
        .expect("read parent release byte");
    assert_eq!(read, 1, "parent must release the held worker");
    drop(flat);
    println!("ENTRY_QA_WORKER_RELEASED path={}", file.display());
}

#[test]
fn two_os_processes_own_disjoint_flat_entry_qa_inputs_in_one_root() {
    let root = flat_owner::select_root();
    let foreign = flat_owner::DriverFile::create(
        &root,
        "entry-qa-foreign-process-sentinel.js",
        b"foreign-entry-process",
    );
    let (mut first, mut first_reader) = spawn_flat_entry_worker(&root);
    let first_path = read_entry_acquired(&mut first_reader)
        .unwrap_or_else(|error| panic!("first worker: {error}"));
    first.record_acquired(Path::new(&first_path));
    let (mut second, mut second_reader) = spawn_flat_entry_worker(&root);
    let second_path = read_entry_acquired(&mut second_reader)
        .unwrap_or_else(|error| panic!("second worker: {error}"));
    second.record_acquired(Path::new(&second_path));
    assert_ne!(
        first_path, second_path,
        "owner token must keep cross-process names disjoint"
    );
    assert!(Path::new(&first_path).is_file());
    assert!(Path::new(&second_path).is_file());
    assert_eq!(fs::read(foreign.path()).unwrap(), b"foreign-entry-process");

    let first_release = first
        .child()
        .stdin
        .as_mut()
        .expect("first worker stdin pipe")
        .write_all(b"release\n");
    first.child().stdin.take();
    if let Err(error) = first_release {
        panic!("release first worker: {error}");
    }
    let second_release = second
        .child()
        .stdin
        .as_mut()
        .expect("second worker stdin pipe")
        .write_all(b"release\n");
    second.child().stdin.take();
    if let Err(error) = second_release {
        panic!("release second worker: {error}");
    }
    let first_status = first
        .wait()
        .unwrap_or_else(|error| panic!("wait first worker: {error}"));
    let second_status = second
        .wait()
        .unwrap_or_else(|error| panic!("wait second worker: {error}"));
    assert!(first_status.success(), "first worker failed");
    assert!(second_status.success(), "second worker failed");
    assert!(!Path::new(&first_path).exists());
    assert!(!Path::new(&second_path).exists());
    assert_eq!(fs::read(foreign.path()).unwrap(), b"foreign-entry-process");
}

#[test]
fn flat_entry_worker_second_spawn_failure_reaps_first_worker_and_input() {
    let root = flat_owner::select_root();
    let bad_executable = flat_owner::DriverFile::create(
        &root,
        "entry-qa-entry-second-spawn-not-executable.js",
        b"not an executable; spawn must fail",
    );
    let foreign = flat_owner::DriverFile::create(
        &root,
        "entry-qa-foreign-entry-second-spawn-sentinel.js",
        b"foreign-entry-second-spawn",
    );
    let observed = Arc::new(Mutex::new(None::<(u32, PathBuf)>));
    let observed_in = Arc::clone(&observed);
    let spawn_error = Arc::new(Mutex::new(None::<io::ErrorKind>));
    let spawn_error_in = Arc::clone(&spawn_error);
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let (mut first, mut first_reader) = spawn_flat_entry_worker(&root);
        let pid = first.pid();
        let first_path = read_entry_acquired(&mut first_reader)
            .unwrap_or_else(|error| panic!("first worker: {error}"));
        first.record_acquired(Path::new(&first_path));
        *observed_in.lock().unwrap() = Some((pid, PathBuf::from(&first_path)));
        assert!(
            Path::new(&first_path).is_file(),
            "first worker must own its input before the second spawn fails"
        );
        match Command::new(bad_executable.path())
            .args(["--exact", FLAT_ENTRY_WORKER_TEST, "--nocapture"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
        {
            Ok(mut unexpected) => {
                let _ = unexpected.kill();
                let _ = unexpected.wait();
                panic!("the second spawn unexpectedly succeeded");
            }
            Err(error) => {
                *spawn_error_in.lock().unwrap() = Some(error.kind());
                panic!("second spawn failed with the real OS error: {error}");
            }
        }
    }));
    outcome.expect_err("the second spawn must fail after the first worker acquired");
    assert_eq!(
        *spawn_error.lock().unwrap(),
        Some(io::ErrorKind::PermissionDenied),
        "second spawn must fail with the real non-executable-file OS error"
    );
    let (pid, path) = observed
        .lock()
        .unwrap()
        .take()
        .expect("first worker pid and owned path recorded");
    assert!(
        !path.exists(),
        "first worker input leaked after second-spawn failure: {}",
        path.display()
    );
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "first worker survived second-spawn failure: pid={pid}"
    );
    assert_eq!(
        fs::read(foreign.path()).unwrap(),
        b"foreign-entry-second-spawn"
    );
}

#[test]
fn flat_entry_worker_assertion_unwind_reaps_owned_workers_and_inputs() {
    let root = flat_owner::select_root();
    let foreign = flat_owner::DriverFile::create(
        &root,
        "entry-qa-foreign-entry-unwind-workers-sentinel.js",
        b"foreign-entry-unwind-workers",
    );
    let observed = Arc::new(Mutex::new(Vec::<(u32, PathBuf)>::new()));
    let observed_in = Arc::clone(&observed);
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let (mut first, mut first_reader) = spawn_flat_entry_worker(&root);
        let first_pid = first.pid();
        let first_path = read_entry_acquired(&mut first_reader)
            .unwrap_or_else(|error| panic!("first worker: {error}"));
        first.record_acquired(Path::new(&first_path));
        let (mut second, mut second_reader) = spawn_flat_entry_worker(&root);
        let second_pid = second.pid();
        let second_path = read_entry_acquired(&mut second_reader)
            .unwrap_or_else(|error| panic!("second worker: {error}"));
        second.record_acquired(Path::new(&second_path));
        observed_in.lock().unwrap().extend([
            (first_pid, PathBuf::from(&first_path)),
            (second_pid, PathBuf::from(&second_path)),
        ]);
        assert!(Path::new(&first_path).is_file());
        assert!(Path::new(&second_path).is_file());
        assert_eq!(
            first_pid, second_pid,
            "deliberate assertion-phase unwind after both workers announced owned inputs"
        );
    }));
    assert!(outcome.is_err(), "the deliberate assertion must unwind");
    let observed = observed.lock().unwrap().clone();
    assert_eq!(observed.len(), 2, "both worker observations were recorded");
    for (pid, path) in observed {
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "worker survived assertion unwind: pid={pid}"
        );
        assert!(
            !path.exists(),
            "announced worker input leaked after assertion unwind: {}",
            path.display()
        );
    }
    assert_eq!(
        fs::read(foreign.path()).unwrap(),
        b"foreign-entry-unwind-workers"
    );
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
    {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if matches!(name, ".git" | "target") {
                continue;
            }
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn rust_sources(dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    collect_files(dir, &mut paths);
    paths.retain(|path| path.extension().and_then(|ext| ext.to_str()) == Some("rs"));
    paths.sort();
    paths
}

fn example_corpus() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    collect_files(&manifest_dir().join("examples"), &mut paths);
    paths.retain(|path| {
        matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("ts" | "mts" | "lua")
        )
    });
    paths.sort();
    paths
}

fn prepare_runnable_corpus(corpus: &[PathBuf]) -> (ScratchTree, PathBuf) {
    let scratch = ScratchTree::new();
    let examples_dir = scratch.root.join("compat").join("examples");
    fs::create_dir_all(&examples_dir).expect("corpus examples directory");
    let stdlib = scratch
        .root
        .join("rustscript")
        .join("stdlib")
        .join("rss")
        .join("strings.rss");
    fs::create_dir_all(stdlib.parent().expect("stdlib parent")).expect("stdlib directory");
    fs::write(&stdlib, FROZEN_STRINGS_RSS).expect("frozen strings.rss");

    for source in corpus {
        let name = source.file_name().expect("example file name");
        fs::copy(source, examples_dir.join(name))
            .unwrap_or_else(|error| panic!("copy {}: {error}", source.display()));
    }
    (scratch, examples_dir)
}

fn compile_example(path: &Path) -> vm::CompiledProgram {
    compile_source_file_with_options(path, pd_vm_compat_frontends::compile_options())
        .unwrap_or_else(|error| panic!("{} failed to compile: {error}", path.display()))
}

fn runner_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pd-vm-compat-run"))
}

fn run_example_cli(path: &Path) -> (String, String) {
    let output = Command::new(runner_bin())
        .arg(path)
        .output()
        .unwrap_or_else(|error| panic!("run {}: {error}", path.display()));
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "{} failed through pd-vm-compat-run ({}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
        path.display(),
        output.status
    );
    (stdout, stderr)
}

fn expected_corpus_output(name: &str) -> &'static str {
    match name {
        "example.ts" => "42",
        "example.mts" => "TypeScript module entry",
        "example_complex.ts" => "12",
        "example.lua" => "42",
        "example_complex.lua" => "12",
        other => panic!("unexpected corpus example {other}"),
    }
}

fn expected_corpus_value(name: &str) -> Value {
    match name {
        "example.ts" | "example.lua" => Value::Int(42),
        "example_complex.ts" | "example_complex.lua" => Value::Int(12),
        "example.mts" => Value::string("TypeScript module entry"),
        other => panic!("unexpected corpus example {other}"),
    }
}

#[test]
fn core_dependency_provenance_matches_the_selected_build_state() {
    let package = locked_core_package();
    assert_eq!(package["name"], "pd-vm");
}

fn forbidden_frontend_authority_token(source: &str) -> Option<&'static str> {
    [
        "HostApiBuilder",
        "HostFunctionRegistry",
        "HostModuleDescriptor",
        "HostFunctionDescriptor",
        "HostApiCatalog::new",
        "HostApiCatalog::standard",
        ".implicit_extern_names.push",
        "pd_host_function",
    ]
    .into_iter()
    .find(|token| source.contains(token))
}

#[test]
fn frontend_authority_guard_detects_catalog_and_descriptor_controls() {
    for token in [
        "HostApiBuilder",
        "HostFunctionRegistry",
        "HostModuleDescriptor",
        "HostFunctionDescriptor",
        "HostApiCatalog::new",
        "HostApiCatalog::standard",
        ".implicit_extern_names.push",
        "pd_host_function",
    ] {
        assert_eq!(
            forbidden_frontend_authority_token(token),
            Some(token),
            "frontend guard must detect authority creation token {token}"
        );
    }
}

#[test]
fn production_sources_consume_catalog_authority_without_creating_it() {
    let root = manifest_dir();
    let mut sources = rust_sources(&root.join("src/typescript"));
    sources.extend(rust_sources(&root.join("src/frontends")));
    sources.push(root.join("src/source_loader.rs"));
    sources.sort();
    sources.dedup();
    assert!(!sources.is_empty());
    for path in sources {
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        if let Some(token) = forbidden_frontend_authority_token(&source) {
            panic!(
                "{} must consume caller authority without creating it ({token})",
                path.display()
            );
        }
    }

    let runtime_owner = root.join("src/runtime/array.rs");
    assert!(
        runtime_owner.is_file(),
        "native array host authority must live under the explicit runtime owner"
    );
    let owner_source = fs::read_to_string(&runtime_owner)
        .unwrap_or_else(|error| panic!("read {}: {error}", runtime_owner.display()));
    assert!(owner_source.contains("pub fn typescript_array_host_module()"));
    let frontend_module = fs::read_to_string(root.join("src/typescript/mod.rs"))
        .expect("read TypeScript frontend module");
    assert!(!frontend_module.contains("typescript_array_host_module"));
}

#[test]
fn example_corpus_has_the_exact_checked_in_count() {
    let corpus = example_corpus();
    assert_eq!(
        corpus.len(),
        EXPECTED_TS_LUA_EXAMPLES,
        "TS/Lua example count drifted: {corpus:?}"
    );
}

#[test]
fn example_corpus_compiles_emits_vmbc_and_runs_through_the_runner() {
    let corpus = example_corpus();
    assert_eq!(corpus.len(), EXPECTED_TS_LUA_EXAMPLES);
    let (_scratch, runnable) = prepare_runnable_corpus(&corpus);
    for source in corpus {
        let name = source
            .file_name()
            .expect("example file name")
            .to_str()
            .expect("UTF-8 example name");
        let path = runnable.join(name);
        let compiled = compile_example(&path);
        let vmbc = encode_program(&compiled.program)
            .unwrap_or_else(|error| panic!("{} VMBC encode failed: {error}", source.display()));
        assert!(!vmbc.is_empty(), "{} produced empty VMBC", source.display());
        let decoded = decode_program(&vmbc)
            .unwrap_or_else(|error| panic!("{} VMBC decode failed: {error}", source.display()));
        if name != "example_complex.lua" {
            let mut machine = Vm::new(decoded);
            assert_eq!(
                machine.run().expect("decoded VMBC program should run"),
                VmStatus::Halted,
                "{} decoded VMBC status",
                source.display()
            );
            assert_eq!(
                machine.stack(),
                &[expected_corpus_value(name)],
                "{} decoded VMBC result",
                source.display()
            );
        }
        let (stdout, stderr) = run_example_cli(&path);
        assert!(
            stderr.trim().is_empty(),
            "{} produced unexpected stderr: {stderr}",
            source.display()
        );
        assert!(
            stdout.contains(expected_corpus_output(name)),
            "{} stdout should contain {}: {stdout:?}",
            source.display(),
            expected_corpus_output(name)
        );
    }
}

#[test]
fn lua_scalar_and_mutable_local_semantics_remain_available() {
    let compiled = compile_source_with_flavor_and_options(
        "local value = 40\nvalue = value + 2\nvalue\n",
        pd_vm_compat_frontends::lua_flavor(),
        pd_vm_compat_frontends::compile_options(),
    )
    .expect("accepted Lua fixture should compile");
    let mut machine = Vm::new(compiled.program);
    assert_eq!(
        machine.run().expect("Lua fixture should run"),
        VmStatus::Halted
    );
    assert_eq!(machine.stack(), &[Value::Int(42)]);
}

#[test]
fn existing_unsupported_script_entries_are_rejected() {
    // `.js` is already registered on the default route. These three entry rows
    // use post-ES5 Script syntax (`const`) and must fail at admission;
    // `.mjs`/`.cjs` remain unregistered extensions. The point is that the
    // existing unsupported forms are rejected, not that JavaScript entries
    // lack registration. The dependency rows are a separate exact test.
    let mut flat = FlatEntryQa::new();
    for extension in ["js", "mjs", "cjs"] {
        let entry = flat.source(
            &format!("entry-qa-entry.{extension}"),
            "const value = 1;\nvalue;\n",
        );
        let entry_vmbc = flat.planned_artifact("bad");
        let entry_output = Command::new(runner_bin())
            .args(["--emit-vmbc"])
            .arg(&entry_vmbc)
            .arg(&entry)
            .output()
            .expect("run JavaScript-family entry through the compatibility CLI");
        let entry_diagnostic = format!(
            "{}{}",
            String::from_utf8_lossy(&entry_output.stdout),
            String::from_utf8_lossy(&entry_output.stderr)
        );
        assert!(
            !entry_output.status.success(),
            ".{extension} entry unexpectedly succeeded: {entry_diagnostic}"
        );
        assert!(
            entry_diagnostic.contains(&format!(".{extension}")),
            ".{extension} entry diagnostic did not identify its extension: {entry_diagnostic}"
        );
        assert!(
            !entry_vmbc.exists(),
            ".{extension} entry failure created VMBC at {}",
            entry_vmbc.display()
        );
        if extension == "js" {
            assert!(
                entry_diagnostic.contains("JavaScript ES5"),
                ".js must be rejected by its registered frontend: {entry_diagnostic}"
            );
            assert!(
                !entry_diagnostic.contains("unsupported source extension '.js'"),
                ".js is registered; this negative must prove Script-form rejection: {entry_diagnostic}"
            );
        }
    }
}

#[test]
fn existing_unsupported_script_module_dependencies_are_rejected() {
    // Six dependency rows: three TypeScript importer analyses and the three
    // referenced `.js`/`.mjs`/`.cjs` module attempts. Same fixture cases as the
    // entry test; only the placement is flat (no project subdirectory).
    let mut flat = FlatEntryQa::new();
    for extension in ["js", "mjs", "cjs"] {
        let dependency_source = if extension == "cjs" {
            "module.exports = { value: 7 };\n"
        } else {
            "export function value(): number { return 7; }\n"
        };
        let dependency = flat.source(
            &format!("entry-qa-dep-{extension}.{extension}"),
            dependency_source,
        );
        let dependency_name = dependency.file_name().unwrap().to_str().unwrap();
        let main = flat.source(
            &format!("entry-qa-main-{extension}.ts"),
            &format!("import {{ value }} from \"./{dependency_name}\";\nvalue();\n"),
        );
        let dependency_vmbc = flat.planned_artifact("bad");
        let dependency_output = Command::new(runner_bin())
            .args(["--emit-vmbc"])
            .arg(&dependency_vmbc)
            .arg(&main)
            .output()
            .expect("run JavaScript-family dependency through the compatibility CLI");
        let dependency_diagnostic = format!(
            "{}{}",
            String::from_utf8_lossy(&dependency_output.stdout),
            String::from_utf8_lossy(&dependency_output.stderr)
        );
        assert!(
            !dependency_output.status.success(),
            ".{extension} dependency unexpectedly succeeded: {dependency_diagnostic}"
        );
        assert!(
            dependency_diagnostic.contains(&format!(".{extension}")),
            ".{extension} dependency diagnostic did not identify its extension: {dependency_diagnostic}"
        );
        assert!(
            !dependency_vmbc.exists(),
            ".{extension} dependency failure created VMBC at {}",
            dependency_vmbc.display()
        );
        if extension == "js" {
            assert!(
                dependency_diagnostic.contains("JavaScript ES5"),
                ".js dependency must be rejected by its registered frontend: {dependency_diagnostic}"
            );
            assert!(
                !dependency_diagnostic.contains("unsupported source extension '.js'"),
                ".js dependency is registered; the refusal must be a Script/module-form diagnostic: {dependency_diagnostic}"
            );
        }
    }
}

#[test]
fn unsupported_language_and_invalid_sources_produce_portable_diagnostics() {
    let scratch = ScratchTree::new();
    let python = scratch.root.join("probe.py");
    fs::write(&python, "print(1)\n").expect("write unsupported source");
    let error = match compile_source_file_with_options(
        &python,
        pd_vm_compat_frontends::compile_options(),
    ) {
        Ok(_) => panic!("Python is not a compatibility frontend"),
        Err(error) => error,
    };
    assert!(matches!(error, SourcePathError::UnsupportedExtension(ref ext) if ext == "py"));
    assert!(error.to_string().contains(".py"));
    let rss_error = match compile_source("fn broken(") {
        Ok(_) => panic!("invalid RSS should fail"),
        Err(error) => error,
    };
    assert!(!rss_error.to_string().is_empty());
}

#[test]
fn frozen_strings_fixture_matches_the_exact_locked_core_source_bytes() {
    let bytes = include_bytes!("fixtures/frozen_stdlib/strings.rss");
    assert_eq!(bytes.len(), FROZEN_STRINGS_BYTES);
    let core_package = locked_core_package();
    let pinned_core = core_provenance::locked_core_root(&core_package);
    let core_bytes =
        fs::read(pinned_core.join("stdlib/rss/strings.rss")).expect("read locked Core strings.rss");
    assert_eq!(
        bytes.as_slice(),
        core_bytes.as_slice(),
        "frozen stdlib bytes differ from exact locked Core source"
    );
    assert!(FROZEN_STRINGS_RSS.contains("pub fn non_empty(value: string) -> bool"));
}
