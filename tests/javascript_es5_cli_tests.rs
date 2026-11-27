//! Default-file CLI integration for every registered frontend.
//!
//! The ordinary `pd-vm-compat-run` route is one Core file-loader path. `.ts`,
//! `.mts`, `.lua` and `.js` resolve through their registered source plugins;
//! Core-native `.rss` keeps its fallback. The retired `--javascript-es5`
//! selector is not an alias: Core owns it as an unknown flag. `--jit-dump`
//! reports Core's actual backend decisions: the hot-loop control compiles and
//! executes native traces, while the heap wide-frame control records a Core
//! refusal. Those fixture observations are not a universal native/release
//! qualification, and no test forces JIT off to manufacture agreement.
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[path = "javascript_es5/flat_owner.rs"]
mod flat_owner;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let path = base.join(format!(
            "p11-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn source(&self, name: &str, source: &str) -> String {
        let path = self.0.join(name);
        std::fs::write(&path, source).unwrap();
        path.to_str().unwrap().to_owned()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

static ENTRY_QA_FLAT_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Flat CLI inputs in one temporary directory selected by `ENTRY_QA_FLAT_DIR`
/// when set (it must then already exist), otherwise by `std::env::temp_dir()`
/// with its normal platform/`TMPDIR` semantics: tiny `entry-qa-*` files (no
/// subdirectory, no source tree copy) plus only the three permitted transient
/// artifact names. Files are removed on drop, including while unwinding.
struct FlatInputs {
    dir: PathBuf,
    created: Vec<PathBuf>,
    _serial: std::sync::MutexGuard<'static, ()>,
}

impl FlatInputs {
    fn new() -> Self {
        Self::from_root(flat_owner::select_root())
    }

    fn from_root(dir: PathBuf) -> Self {
        assert!(
            dir.is_dir(),
            "flat CLI input directory is not a directory: {}",
            dir.display()
        );
        let serial = ENTRY_QA_FLAT_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Self {
            dir,
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
        flat_owner::validate_flat_target(&self.dir, path)?;
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

    fn try_source_at(&mut self, label: &str, path: &Path, source: &str) -> io::Result<String> {
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
        Ok(path.to_str().expect("flat path is UTF-8").to_owned())
    }

    fn source(&mut self, name: &str, source: &str) -> String {
        let path = self.dir.join(flat_owner::input_name(name));
        self.try_source_at(name, &path, source)
            .unwrap_or_else(|error| panic!("create flat CLI input {}: {error}", path.display()))
    }

    fn try_artifact_at(&mut self, kind: &str, path: &Path) -> io::Result<String> {
        flat_owner::validate_artifact_path(path, kind)?;
        let file = self.try_acquire(path)?;
        drop(file);
        eprintln!(
            "ENTRY_QA_FLAT_ARTIFACT path={} reservation=create_new",
            path.display()
        );
        Ok(path.to_str().expect("flat path is UTF-8").to_owned())
    }

    fn artifact(&mut self, name: &str) -> String {
        let kind = flat_owner::artifact_kind(name);
        let path = self.dir.join(flat_owner::artifact_name(kind));
        self.try_artifact_at(kind, &path)
            .unwrap_or_else(|error| panic!("create flat CLI artifact {}: {error}", path.display()))
    }

    fn remove_owned(&mut self, path: &Path) {
        let index = self
            .created
            .iter()
            .position(|owned| owned == path)
            .unwrap_or_else(|| panic!("refusing to remove unowned flat path {}", path.display()));
        fs::remove_file(path).unwrap_or_else(|error| {
            panic!("remove owned transient file {}: {error}", path.display())
        });
        self.created.remove(index);
        assert!(
            !path.exists(),
            "owned transient file still exists: {}",
            path.display()
        );
        eprintln!(
            "ENTRY_QA_FLAT_CLEANUP_EARLY path={} remaining_owned={}",
            path.display(),
            self.created.len()
        );
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
                        failure = Some(format!(
                            "remove owned flat file {}: {error}",
                            path.display()
                        ));
                    }
                    remaining.push(path);
                }
            }
        }
        eprintln!(
            "ENTRY_QA_FLAT_CLEANUP root={} removed={removed} remaining_owned={}",
            self.dir.display(),
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

impl Drop for FlatInputs {
    fn drop(&mut self) {
        self.cleanup();
    }
}

const FLAT_WORKER_TEST: &str = "flat_inputs_worker_process_holds_owned_input_until_release";

fn spawn_flat_worker(
    root: &Path,
) -> (
    flat_owner::WorkerGuard,
    BufReader<std::process::ChildStdout>,
) {
    let exe = std::env::current_exe().expect("current test executable");
    spawn_flat_worker_at(root, &exe)
}

fn spawn_flat_worker_at(
    root: &Path,
    exe: &Path,
) -> (
    flat_owner::WorkerGuard,
    BufReader<std::process::ChildStdout>,
) {
    let child = Command::new(exe)
        .args(["--exact", FLAT_WORKER_TEST, "--nocapture"])
        .env("ENTRY_QA_FLAT_WORKER_ROOT", root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap_or_else(|error| panic!("spawn flat input worker process: {error}"));
    // The guard owns the child before any fallible follow-up, including the
    // stdout-pipe take below and any later second spawn.
    let mut worker = flat_owner::WorkerGuard::new(root, child);
    let stdout = worker.child().stdout.take().expect("worker stdout pipe");
    (worker, BufReader::new(stdout))
}

fn read_acquired(reader: &mut BufReader<std::process::ChildStdout>) -> io::Result<String> {
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
fn flat_input_names_are_owner_scoped_and_foreign_files_are_not_owned() {
    let root = flat_owner::select_root();
    let foreign = flat_owner::DriverFile::create(
        &root,
        "entry-qa-foreign-metadata-sentinel.js",
        b"foreign-metadata",
    );
    let observed = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
    let observed_in_closure = Arc::clone(&observed);
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut flat = FlatInputs::from_root(root.clone());
        let first = flat.source("entry-qa-meta.js", "1===1;");
        let second = flat.source("entry-qa-meta.js", "1===1;");
        assert_ne!(first, second, "owner token must separate repeated labels");
        let pid = std::process::id().to_string();
        for path in [&first, &second] {
            let path = Path::new(path);
            let name = path.file_name().unwrap().to_str().unwrap();
            assert!(name.starts_with("entry-qa-meta-"), "{name}");
            assert!(name.ends_with(".js"), "{name}");
            assert!(
                name.contains(&format!("-{pid}-")),
                "owner token must be decimal pid: {name}"
            );
            assert_eq!(path.parent(), Some(root.as_path()));
            observed_in_closure.lock().unwrap().push(path.to_owned());
        }
        let artifact = flat.artifact("default-js-route.vmbc");
        let artifact_path = Path::new(&artifact);
        let artifact_name = artifact_path.file_name().unwrap().to_str().unwrap();
        assert!(artifact_name.starts_with(&format!("entry-qa-{pid}-")));
        assert!(artifact_name.ends_with("-route.vmbc"), "{artifact_name}");
        assert!(
            artifact_path.is_file(),
            "artifact must be an atomic owned reservation"
        );
        observed_in_closure
            .lock()
            .unwrap()
            .push(artifact_path.to_owned());
        assert_eq!(flat.owned_count(), 3);
    }));
    assert!(
        outcome.is_ok(),
        "a foreign entry-qa-* file must not fail this helper's cleanup"
    );
    for path in observed.lock().unwrap().iter() {
        assert!(!path.exists(), "owned file leaked: {}", path.display());
    }
    assert_eq!(fs::read(foreign.path()).unwrap(), b"foreign-metadata");
    assert!(foreign.path().is_file());
}

#[test]
fn flat_acquisition_refuses_foreign_files_without_owning_them() {
    let root = flat_owner::select_root();
    let mut flat = FlatInputs::from_root(root.clone());
    let source_path = root.join("entry-qa-foreign-source.js");
    let foreign_source = flat_owner::DriverFile::create_at(&source_path, b"foreign-source");
    let error = flat
        .try_source_at("entry-qa-foreign-source.js", &source_path, "1===1;")
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(flat.owned_count(), 0);
    assert_eq!(fs::read(foreign_source.path()).unwrap(), b"foreign-source");

    let artifact_path = root.join(flat_owner::artifact_name("route"));
    let foreign_artifact = flat_owner::DriverFile::create_at(&artifact_path, b"foreign-artifact");
    let error = flat.try_artifact_at("route", &artifact_path).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(flat.owned_count(), 0);
    assert_eq!(
        fs::read(foreign_artifact.path()).unwrap(),
        b"foreign-artifact"
    );

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
fn flat_acquisition_refuses_dangling_symlinks_without_following_them() {
    let root = flat_owner::select_root();
    let mut flat = FlatInputs::from_root(root.clone());

    let source_link = root.join("entry-qa-dangling-source.js");
    let source_target = root.join("entry-qa-target.js");
    assert!(fs::symlink_metadata(&source_target).is_err());
    let source_symlink = flat_owner::DriverSymlink::create(&source_link, &source_target);
    let error = flat
        .try_source_at("entry-qa-dangling-source.js", &source_link, "1===1;")
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert!(
        fs::symlink_metadata(&source_link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(fs::symlink_metadata(&source_target).is_err());
    assert_eq!(flat.owned_count(), 0);

    let artifact_link = root.join(flat_owner::artifact_name("bad"));
    let artifact_target = root.join("entry-qa-target-artifact.js");
    let artifact_symlink = flat_owner::DriverSymlink::create(&artifact_link, &artifact_target);
    let error = flat.try_artifact_at("bad", &artifact_link).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert!(
        fs::symlink_metadata(&artifact_link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(fs::symlink_metadata(&artifact_target).is_err());
    assert_eq!(flat.owned_count(), 0);
    drop(source_symlink);
    drop(artifact_symlink);
}

#[test]
fn flat_owned_files_are_removed_during_unwind_and_after_early_failure() {
    let root = flat_owner::select_root();
    let foreign = flat_owner::DriverFile::create(
        &root,
        "entry-qa-foreign-unwind-sentinel.js",
        b"foreign-unwind",
    );
    let owned = Arc::new(Mutex::new(None::<String>));
    let owned_in_panic = Arc::clone(&owned);
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut flat = FlatInputs::from_root(root.clone());
        let file = flat.source("entry-qa-unwind.js", "1===1;");
        *owned_in_panic.lock().unwrap() = Some(file);
        panic!("deliberate post-acquisition unwind");
    }));
    assert!(outcome.is_err());
    let file = owned.lock().unwrap().take().expect("owned path recorded");
    assert!(
        !Path::new(&file).exists(),
        "owned input leaked during unwind"
    );
    assert_eq!(fs::read(foreign.path()).unwrap(), b"foreign-unwind");

    let early = {
        let mut flat = FlatInputs::from_root(root.clone());
        let path = root.join(flat_owner::input_name("entry-qa-early.js"));
        let handle = flat.try_acquire(&path).expect("reserve owned path");
        drop(handle);
        assert!(path.is_file());
        path
    };
    assert!(!early.exists(), "owned reservation leaked after early drop");
    assert!(foreign.path().is_file());
}

#[test]
fn flat_inputs_worker_process_holds_owned_input_until_release() {
    let Some(root) = std::env::var_os("ENTRY_QA_FLAT_WORKER_ROOT") else {
        return;
    };
    let mut flat = FlatInputs::from_root(PathBuf::from(root));
    let file = flat.source("entry-qa-worker.js", "1===1;");
    println!("ENTRY_QA_WORKER_ACQUIRED path={file}");
    std::io::stdout().flush().expect("flush acquisition marker");
    let mut release = [0u8; 1];
    let read = std::io::stdin()
        .read(&mut release)
        .expect("read parent release byte");
    assert_eq!(read, 1, "parent must release the held worker");
    drop(flat);
    println!("ENTRY_QA_WORKER_RELEASED path={file}");
}

#[test]
fn two_os_processes_own_disjoint_flat_inputs_in_one_root() {
    let root = flat_owner::select_root();
    let foreign = flat_owner::DriverFile::create(
        &root,
        "entry-qa-foreign-process-sentinel.js",
        b"foreign-process",
    );
    let (mut first, mut first_reader) = spawn_flat_worker(&root);
    let first_path =
        read_acquired(&mut first_reader).unwrap_or_else(|error| panic!("first worker: {error}"));
    first.record_acquired(Path::new(&first_path));
    let (mut second, mut second_reader) = spawn_flat_worker(&root);
    let second_path =
        read_acquired(&mut second_reader).unwrap_or_else(|error| panic!("second worker: {error}"));
    second.record_acquired(Path::new(&second_path));
    assert_ne!(
        first_path, second_path,
        "owner token must keep cross-process names disjoint"
    );
    assert!(Path::new(&first_path).is_file());
    assert!(Path::new(&second_path).is_file());
    assert_eq!(fs::read(foreign.path()).unwrap(), b"foreign-process");

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
    assert_eq!(fs::read(foreign.path()).unwrap(), b"foreign-process");
}

#[test]
fn worker_second_spawn_failure_reaps_first_worker_and_input() {
    let root = flat_owner::select_root();
    let bad_executable = flat_owner::DriverFile::create(
        &root,
        "entry-qa-second-spawn-not-executable.js",
        b"not an executable; spawn must fail",
    );
    let foreign = flat_owner::DriverFile::create(
        &root,
        "entry-qa-foreign-second-spawn-sentinel.js",
        b"foreign-second-spawn",
    );
    let observed = Arc::new(Mutex::new(None::<(u32, PathBuf)>));
    let observed_in = Arc::clone(&observed);
    let spawn_error = Arc::new(Mutex::new(None::<io::ErrorKind>));
    let spawn_error_in = Arc::clone(&spawn_error);
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let (mut first, mut first_reader) = spawn_flat_worker(&root);
        let pid = first.pid();
        let first_path = read_acquired(&mut first_reader)
            .unwrap_or_else(|error| panic!("first worker: {error}"));
        first.record_acquired(Path::new(&first_path));
        *observed_in.lock().unwrap() = Some((pid, PathBuf::from(&first_path)));
        assert!(
            Path::new(&first_path).is_file(),
            "first worker must own its input before the second spawn fails"
        );
        match Command::new(bad_executable.path())
            .args(["--exact", FLAT_WORKER_TEST, "--nocapture"])
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
    assert_eq!(fs::read(foreign.path()).unwrap(), b"foreign-second-spawn");
}

#[test]
fn worker_assertion_unwind_reaps_owned_workers_and_inputs() {
    let root = flat_owner::select_root();
    let foreign = flat_owner::DriverFile::create(
        &root,
        "entry-qa-foreign-unwind-workers-sentinel.js",
        b"foreign-unwind-workers",
    );
    let observed = Arc::new(Mutex::new(Vec::<(u32, PathBuf)>::new()));
    let observed_in = Arc::clone(&observed);
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let (mut first, mut first_reader) = spawn_flat_worker(&root);
        let first_pid = first.pid();
        let first_path = read_acquired(&mut first_reader)
            .unwrap_or_else(|error| panic!("first worker: {error}"));
        first.record_acquired(Path::new(&first_path));
        let (mut second, mut second_reader) = spawn_flat_worker(&root);
        let second_pid = second.pid();
        let second_path = read_acquired(&mut second_reader)
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
    assert_eq!(fs::read(foreign.path()).unwrap(), b"foreign-unwind-workers");
}

fn cli(args: &[&str]) -> std::process::Output {
    let result = Command::new(env!("CARGO_BIN_EXE_pd-vm-compat-run"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    eprintln!(
        "P11_CLI args={args:?} rc={:?}\nstdout={}\nstderr={}",
        result.status.code(),
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    result
}

#[test]
fn retired_javascript_selector_is_core_unknown_flag() {
    for args in [
        vec!["--javascript-es5"],
        vec!["--javascript-es5", "repl"],
        vec!["--javascript-es5", "--repl"],
        vec!["--javascript-es5", "repl", "--repl"],
        vec!["--javascript-es5", "--repl", "--repl"],
        vec!["--javascript-es5", "--help"],
    ] {
        let result = cli(&args);
        assert!(
            !result.status.success(),
            "retired selector must not run: {args:?}"
        );
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(
            error.contains("unknown flag '--javascript-es5'"),
            "Core must own the retired selector: {error}"
        );
        assert!(
            !error.contains("JavaScript ES5 requires a source file"),
            "no hidden JavaScript alias or fallback remains: {error}"
        );
        assert!(result.stdout.is_empty(), "no guest output: {args:?}");
    }
    // Core's own version pre-scan wins before unknown-flag handling; the
    // retired selector contributes nothing and the output is identical.
    assert_eq!(
        cli(&["--javascript-es5", "--version"]).stdout,
        cli(&["--version"]).stdout
    );
}

#[test]
fn default_entry_executes_actual_primitive_file() {
    let mut flat = FlatInputs::new();
    let file = flat.source(
        "entry-qa-primitive.js",
        "var answer = 6 * 7; answer === 42;",
    );
    let result = cli(&["--fuel", "10000000", &file]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("true"));
}

#[test]
fn default_rss_source_executes_through_core_native_fallback() {
    let mut flat = FlatInputs::new();
    let file = flat.source("entry-qa-control.rss", "let n: int = 6 * 7; n;");
    assert!(std::path::Path::new(&file).is_absolute());
    let result = cli(&["--fuel", "10000000", "--jit-dump", &file]);
    assert!(
        result.status.success(),
        "normal .rss must execute through the Core-native path: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("stack: [Int(42)]"), "{stdout}");
    assert!(stdout.contains("enabled: true"), "{stdout}");
    assert!(
        stdout.contains("compile attempts: 0") && stdout.contains("native trace executions: 0")
    );
}

#[test]
fn rss_case_variants_with_default_fuel_execute_through_core_native_fallback() {
    let scratch = Scratch::new();
    for name in ["control.rss", "control.RSS", "control.RsS"] {
        let source = "let n: int = 6 * 7; n;";
        let file = scratch.source(name, source);
        let result = cli(&[&file]);
        assert!(
            result.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("Int(42)"));
    }
}

#[test]
fn rss_case_variants_with_explicit_fuel_execute_through_core_native_fallback() {
    let mut flat = FlatInputs::new();
    for name in [
        "entry-qa-control.rss",
        "entry-qa-control.RSS",
        "entry-qa-control.RsS",
    ] {
        let source = "let n: int = 6 * 7; n;";
        let file = flat.source(name, source);
        let result = cli(&["--fuel", "10000000", &file]);
        assert!(
            result.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("Int(42)"));
    }
}

#[test]
fn rss_case_variants_emit_through_core_native_fallback() {
    let mut flat = FlatInputs::new();
    for name in [
        "entry-qa-control.rss",
        "entry-qa-control.RSS",
        "entry-qa-control.RsS",
    ] {
        let source = "let n: int = 6 * 7; n;";
        let file = flat.source(name, source);
        let output = flat.artifact("default-js-route.vmbc");
        let result = cli(&["--emit-vmbc", &output, &file]);
        assert!(
            result.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            std::path::Path::new(&output).is_file(),
            "ordinary RSS emit remains Core-owned"
        );
        let artifact = std::fs::read(&output).unwrap();
        assert!(vm::decode_program(&artifact).is_ok());
        flat.remove_owned(std::path::Path::new(&output));
    }
}

#[test]
fn default_compile_error_retains_source_and_never_initializes_vm() {
    use std::error::Error;
    let mut flat = FlatInputs::new();
    let source = "var text='😀';\nlet forbidden=1;";
    let file = flat.source("entry-qa-admission.js", source);
    let error = match vm::compile_source_file_with_capture_certification_and_options(
        &file,
        pd_vm_compat_frontends::compile_options(),
    ) {
        Ok(_) => panic!("the JavaScript admission error must fail before issuing a program"),
        Err(error) => error,
    };
    eprintln!("P11_JS_SOURCE_ERROR {error:?}");
    let rendered = error.to_string();
    assert!(
        rendered.contains("'let' declarations are post-ES5"),
        "{rendered}"
    );
    let boxed: Box<dyn Error> = Box::new(error);
    let concrete = boxed.downcast_ref::<vm::SourcePathError>().unwrap();
    assert_eq!(boxed.to_string(), concrete.to_string());
    assert_eq!(
        boxed.source().map(ToString::to_string),
        concrete.source().map(ToString::to_string)
    );
    let error = *boxed.downcast::<vm::SourcePathError>().unwrap();
    let vm::SourcePathError::SourceWithMap {
        error: vm::SourceError::Parse(parse),
        sources,
    } = error
    else {
        panic!("mapped parse error")
    };
    let span = parse.span.unwrap();
    assert_eq!(&source[span.lo..span.hi], "let forbidden=1;");
    assert_eq!(sources.file(span.source_id).unwrap().name, file);
    let initialized = std::cell::Cell::new(false);
    let services = pd_vm_compat_frontends::runtime::cli::typescript_cli_host_services()
        .with_vm_init(|_| {
            initialized.set(true);
            panic!("source refusal must precede VM initialization")
        });
    let runtime = vm::cli::CliRuntime {
        binary_name: "p11-source-observer",
        default_source: "",
        compile_options: pd_vm_compat_frontends::compile_options,
    };
    assert!(vm::cli::run_with_args(&runtime, &[file], &services).is_err());
    assert!(!initialized.get());
}

#[test]
fn rss_looking_operands_are_literal_and_explicit_vmbc_remains_core_owned() {
    let scratch = Scratch::new();
    let file = scratch.source("source.rss.js", "1===1;");
    let recording = scratch.0.join("literal-record.rss");
    let result = cli(&[
        "--record",
        recording.to_str().unwrap(),
        "--fuel",
        "10000000",
        &file,
    ]);
    assert!(result.status.success());
    assert!(recording.is_file(), "record value remains literal");
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("Bool(true)") && stdout.contains("recording saved to"));
    let output = scratch.0.join("literal-output.RsS");
    let result = cli(&["--emit-vmbc", output.to_str().unwrap(), &file]);
    assert!(result.status.success());
    let bytes = std::fs::read(&output).unwrap();
    assert!(
        vm::decode_program(&bytes).is_ok(),
        "output suffix is not a source producer"
    );
    let result = cli(&["--fuel", "literal-value.rss", &file]);
    assert!(!result.status.success());
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(
        error.contains("invalid --fuel value 'literal-value.rss'"),
        "{error}"
    );
    assert!(!error.contains("does not accept RustScript source"));
    let file = scratch.source("control.rss", "let n: int = 6 * 7; n;");
    let artifact = scratch.0.join("explicit.vmbc");
    assert!(
        cli(&["--emit-vmbc", artifact.to_str().unwrap(), &file])
            .status
            .success()
    );
    std::fs::remove_file(file).unwrap();
    let result = cli(&["--jit-dump", artifact.to_str().unwrap()]);
    assert!(
        result.status.success(),
        "Core-owned ordinary VMBC remains executable without source",
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("stack: [Int(42)]") && stdout.contains("enabled: true"));
    assert!(
        stdout.contains("compile attempts: 0") && stdout.contains("native trace executions: 0")
    );
}

#[test]
fn file_executes_utf16_functions_finally_and_existing_object_descriptors() {
    let scratch = Scratch::new();
    for (name, source) in [
        (
            "utf16.js",
            "var text='\\ud800😀';text==='\\ud800\\ud83d\\ude00';",
        ),
        (
            "function.js",
            "function f(x){'use strict';return x;}f(42)===42;",
        ),
        (
            "finally.js",
            "var n=0;var caught=false;try{throw 7;}catch(e){caught=e===7;}finally{n=8;}n===8 && caught;",
        ),
        (
            "object.js",
            "var o={};Object.defineProperty(o,'x',{value:7,writable:false});o.x===7 && Object.getOwnPropertyDescriptor(o,'x').writable===false;",
        ),
    ] {
        let file = scratch.source(name, source);
        let result = cli(&["--fuel", "10000000", &file]);
        assert!(
            result.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("Bool(true)"));
    }
}

#[test]
fn file_errors_retain_admission_location_and_uncaught_throw_exit() {
    let mut flat = FlatInputs::new();
    let file = flat.source("entry-qa-bad.js", "var text='😀';\nlet forbidden=1;");
    let result = cli(&[&file]);
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(!result.status.success());
    let basename = Path::new(&file).file_name().unwrap().to_str().unwrap();
    assert!(
        error.contains(&format!("{basename}:2:")) && error.contains("let forbidden=1;"),
        "{error}"
    );
    let file = flat.source("entry-qa-throw.js", "throw 7;");
    let result = cli(&["--fuel", "10000000", &file]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("uncaught JavaScript throw"));
}

#[test]
fn help_version_and_flag_operands_remain_core_owned() {
    for flag in ["--help", "--version"] {
        let result = cli(&[flag]);
        assert!(result.status.success(), "{flag}");
        assert!(!result.stdout.is_empty());
    }
    assert!(cli(&["repl", "--help"]).status.success());
    let scratch = Scratch::new();
    let file = scratch.source("ordinary.js", "1===1;");
    let result = cli(&["--fuel", "1", "--javascript-es5", &file]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("unknown flag '--javascript-es5'"));
    let result = cli(&["--fuel", "--repl", &file]);
    assert!(String::from_utf8_lossy(&result.stderr).contains("invalid --fuel value '--repl'"));
    let result = cli(&["--fuel=1", &file]);
    assert!(String::from_utf8_lossy(&result.stderr).contains("unknown flag '--fuel=1'"));
    let result = cli(&["--fuel", "1", "--fuel", "10000000", &file]);
    assert!(result.status.success(), "last Core-owned value wins");
}

#[test]
fn normal_default_js_keeps_core_backend_policy() {
    let mut flat = FlatInputs::new();
    let file = flat.source("entry-qa-loop.js", "var i=0;while(i!==3){i=i+1;}i===3;");
    let result = cli(&[
        "--jit-hot-loop",
        "1",
        "--jit-dump",
        "--fuel",
        "10000000",
        &file,
    ]);
    assert!(result.status.success());
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("Bool(true)"), "{stdout}");
    assert!(
        stdout.contains("enabled: true"),
        "the default route must not globally disable Core JIT for JavaScript: {stdout}"
    );
    assert!(
        stdout.contains("compile attempts:") && stdout.contains("native trace executions:"),
        "actual backend counters must be reported: {stdout}"
    );
}

#[test]
fn normal_default_ts_reference_ts_lua_rss_and_js_controls_execute() {
    let mut flat = FlatInputs::new();
    for (name, source, selector) in [
        ("entry-qa-ordinary.ts", "const n: number = 6 * 7; n;", None),
        (
            "entry-qa-reference.ts",
            "const a: number[]=[1,2];const b=a;b[0]=7;a[0];",
            Some("--typescript-reference-arrays"),
        ),
        (
            "entry-qa-ordinary.lua",
            "local value = 40\nvalue = value + 2\nvalue\n",
            None,
        ),
        ("entry-qa-ordinary.rss", "let n: int = 6 * 7; n;", None),
        ("entry-qa-ordinary.js", "1===1;", None),
    ] {
        let file = flat.source(name, source);
        let mut args = vec![];
        if let Some(selector) = selector {
            args.push(selector);
        }
        args.extend(["--fuel", "10000000", &file]);
        let result = cli(&args);
        assert!(
            result.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let stdout = String::from_utf8_lossy(&result.stdout);
        let expected = match name {
            "entry-qa-reference.ts" => "Int(7)",
            "entry-qa-ordinary.js" => "Bool(true)",
            _ => "Int(42)",
        };
        assert!(
            stdout.contains(expected),
            "{name} must produce the actual control result: {stdout}"
        );
    }
}

#[test]
fn unsupported_script_extensions_and_reference_selector_keep_their_plugin_sets() {
    let mut flat = FlatInputs::new();
    for (name, source) in [
        ("entry-qa-unsupported.mjs", "1===1;"),
        ("entry-qa-unsupported.cjs", "1===1;"),
    ] {
        let file = flat.source(name, source);
        let result = cli(&[&file]);
        assert!(
            !result.status.success(),
            "{name} must remain unregistered in the default route"
        );
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(&format!(
                "unsupported source extension '.{}'",
                name.rsplit_once('.').unwrap().1
            )),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let file = flat.source("entry-qa-ordinary.js", "1===1;");
    let result = cli(&["--typescript-reference-arrays", &file]);
    assert!(
        !result.status.success(),
        "the explicit reference-array configuration keeps its own plugin set"
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("unsupported source extension '.js'"));
}

#[test]
fn rss_jit_control_remains_core_owned() {
    let scratch = Scratch::new();
    let file = scratch.source("jit-control.rss", "42;");
    let result = cli(&["--jit-dump", &file]);
    assert!(result.status.success());
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("enabled: true"));
    assert!(
        stdout.contains("compile attempts: 0") && stdout.contains("native trace executions: 0")
    );
}

#[test]
fn ordinary_vmbc_companion_and_source_deleted_fresh_process_have_no_authority() {
    let mut flat = FlatInputs::new();
    let source = "var o={x:7};o.x===7;";
    let file = flat.source("entry-qa-artifact.js", source);
    let artifact = flat.artifact("default-js-route.vmbc");
    let artifact = artifact.as_str();
    assert!(
        cli(&["--emit-vmbc", artifact, &file]).status.success(),
        "normal default emit must succeed"
    );
    let bytes = std::fs::read(artifact).unwrap();
    let compiled = vm::compile_source_file_with_capture_certification_and_options(
        &file,
        pd_vm_compat_frontends::compile_options(),
    )
    .unwrap()
    .into_uncertified();
    assert_eq!(
        bytes,
        vm::encode_program(&compiled.program.with_local_count(compiled.locals)).unwrap()
    );
    for source_present in [true, false] {
        if !source_present {
            flat.remove_owned(std::path::Path::new(&file));
        }
        let result = cli(&["--fuel", "10000000", artifact]);
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr)
                .contains("owned array allocation requires a source-certified program")
        );
    }
    let observed = std::cell::Cell::new(false);
    let services =
        pd_vm_compat_frontends::runtime::cli::typescript_cli_host_services().with_vm_init(
            |machine| {
                machine.set_jit_config(vm::JitConfig {
                    enabled: false,
                    ..vm::JitConfig::default()
                });
                pd_vm_compat_frontends::runtime::array::initialize_typescript_owned_arrays(
                    machine,
                    pd_vm_compat_frontends::runtime::array::OwnedArrayLimits::default(),
                )?;
                assert!(machine.capture_certification_stats().is_none());
                assert_eq!(machine.host_context().resource_count(), 0);
                machine.set_fuel(10_000_000);
                let error = machine.run().unwrap_err();
                assert!(
                    matches!(error, vm::VmError::HostError(ref message) if message == "owned array allocation requires a source-certified program")
                );
                assert_eq!(
                    machine.host_context().resource_count(),
                    0,
                    "authority denied before actual payload allocation"
                );
                assert!(
                    pd_vm_compat_frontends::runtime::array::owned_array_runtime_stats(machine)
                        .is_none()
                );
                assert_eq!(machine.aot_exec_count(), 0);
                assert_eq!(machine.jit_native_exec_count(), 0);
                observed.set(true);
                Err(vm::VmError::HostError("P11 observed preallocation refusal".into()))
            },
        );
    let runtime = vm::cli::CliRuntime {
        binary_name: "p11-observer",
        default_source: "",
        compile_options: pd_vm_compat_frontends::compile_options,
    };
    let error = vm::cli::run_with_args(&runtime, &[artifact.to_owned()], &services).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("P11 observed preallocation refusal")
    );
    assert!(observed.get());
}

#[test]
fn damaged_vmbc_keeps_concrete_wire_error_across_default_and_emit_routes() {
    use std::error::Error;
    let scratch = Scratch::new();
    let artifact = scratch.source("damaged.vmbc", "not VMBC");
    for emit in [false, true] {
        let runtime = vm::cli::CliRuntime {
            binary_name: "p11-errors",
            default_source: "",
            compile_options: pd_vm_compat_frontends::compile_options,
        };
        let services = pd_vm_compat_frontends::runtime::cli::typescript_cli_host_services();
        let output = scratch
            .0
            .join("damaged-output.vmbc")
            .to_str()
            .unwrap()
            .to_owned();
        let args = if emit {
            vec!["--emit-vmbc".to_owned(), output.clone(), artifact.clone()]
        } else {
            vec![artifact.clone()]
        };
        let error = vm::cli::run_with_args(&runtime, &args, &services).unwrap_err();
        let wire = error
            .downcast_ref::<vm::WireError>()
            .expect("original decoder error, not io wrapping");
        assert_eq!(error.to_string(), wire.to_string());
        assert_eq!(
            error.source().map(ToString::to_string),
            wire.source().map(ToString::to_string)
        );
        assert!(!std::path::Path::new(&output).exists());
    }
}
