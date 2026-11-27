//! Test-only flat-fixture ownership primitives shared by the two default-entry
//! CLI integration targets.
//!
//! Every helper-owned input or artifact is created with `O_CREAT|O_EXCL`
//! (`create_new`) directly inside the selected temporary root and registered
//! immediately after that creation succeeds. Names carry a decimal
//! `pid-counter-nanos` owner token, so two test processes sharing the same
//! temporary directory never plan or clean the same physical file. Nothing
//! here scans the shared directory: another process's `entry-qa-*` files are
//! never adopted, rejected, overwritten, followed or removed.
#![allow(dead_code)]

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const ENV_FLAT_DIR: &str = "ENTRY_QA_FLAT_DIR";
pub const MAX_OWNED_FILES: usize = 12;
pub const MAX_INPUT_BYTES: usize = 65_536;
pub const INPUT_EXTENSIONS: [&str; 9] =
    ["js", "mjs", "cjs", "ts", "mts", "lua", "rss", "RSS", "RsS"];
pub const ARTIFACT_KINDS: [&str; 3] = ["route", "bad", "authority"];

/// `ENTRY_QA_FLAT_DIR` when explicitly set (must already exist), otherwise the
/// platform temporary directory, which honors `TMPDIR`.
pub fn select_root() -> PathBuf {
    let root = std::env::var_os(ENV_FLAT_DIR)
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    assert!(
        root.is_dir(),
        "ENTRY_QA_FLAT_DIR is not a directory: {}",
        root.display()
    );
    root
}

/// Decimal `pid-counter-nanos`; never a hash. Counter is process-local and
/// monotonic; nanoseconds keep same-counter names distinct across processes
/// with distinct pids and make a pid reuse at the same counter vanish.
pub fn owner_token() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let counter = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    format!("{}-{counter}-{nanos}", std::process::id())
}

/// Validate one flat logical input label (`entry-qa-*`, one path component, an
/// authorized extension) and return `(stem, extension)`.
pub fn input_label(label: &str) -> (&str, &str) {
    let path = Path::new(label);
    assert!(
        path.components().count() == 1 && path.file_name() == Some(OsStr::new(label)),
        "flat CLI input must be one direct file name, not a path: {label}"
    );
    let stem = path
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or_else(|| panic!("flat CLI input has no UTF-8 stem: {label}"));
    assert!(
        stem.starts_with("entry-qa-"),
        "flat CLI input must be entry-qa-*: {label}"
    );
    let extension = path
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or_else(|| panic!("flat CLI input has no authorized extension: {label}"));
    assert!(
        INPUT_EXTENSIONS.contains(&extension),
        "flat CLI input must have an authorized extension: {label}"
    );
    (stem, extension)
}

/// Unique physical input name; the logical stem and extension are preserved so
/// frontend selection and diagnostic extension checks still bind to the
/// scenario rather than to the owner nonce.
pub fn input_name(label: &str) -> String {
    let (stem, extension) = input_label(label);
    format!("{stem}-{}.{extension}", owner_token())
}

/// Unique physical artifact name for one of the three authorized kinds.
pub fn artifact_name(kind: &str) -> String {
    assert!(
        ARTIFACT_KINDS.contains(&kind),
        "artifact kind is not parent-authorized: {kind}"
    );
    format!("entry-qa-{}-{kind}.vmbc", owner_token())
}

/// Map the historical logical artifact labels onto the three authorized kinds.
pub fn artifact_kind(label: &str) -> &'static str {
    match label {
        "default-js-route.vmbc" => "route",
        "default-js-route-bad.vmbc" => "bad",
        "entry-qa-authority.vmbc" => "authority",
        other => panic!("artifact name is not parent-authorized: {other}"),
    }
}

/// Enforce the bounded `entry-qa-<pid>-<counter>-<nanos>-<kind>.vmbc` schema so
/// a private acquisition entry point cannot be turned into a general output
/// path grant.
pub fn validate_artifact_path(path: &Path, kind: &str) -> io::Result<()> {
    assert!(
        ARTIFACT_KINDS.contains(&kind),
        "artifact kind is not parent-authorized: {kind}"
    );
    let name = path.file_name().and_then(OsStr::to_str).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact target has no UTF-8 file name",
        )
    })?;
    let suffix = format!("-{kind}.vmbc");
    let body = name
        .strip_prefix("entry-qa-")
        .and_then(|rest| rest.strip_suffix(&suffix))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "artifact target is not the authorized owner-token schema",
            )
        })?;
    let mut parts = body.split('-');
    let pid = parts.next();
    let counter = parts.next();
    let nanos = parts.next();
    let decimal = |value: Option<&str>| {
        value.is_some_and(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    };
    if parts.next().is_some() || !decimal(pid) || !decimal(counter) || !decimal(nanos) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact target is not the authorized owner-token schema",
        ));
    }
    Ok(())
}

/// A helper-owned target must be one direct child of the selected root.
pub fn validate_flat_target(root: &Path, path: &Path) -> io::Result<()> {
    if path.parent() != Some(root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "flat fixture target must be a direct child of {}: {}",
                root.display(),
                path.display()
            ),
        ));
    }
    let name = path.file_name().and_then(OsStr::to_str).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "flat fixture target has no UTF-8 file name",
        )
    })?;
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "flat fixture target is not one file name",
        ));
    }
    Ok(())
}

/// Atomic exclusive creation. `create_new` maps to `O_CREAT|O_EXCL`, so an
/// existing regular file or dangling symlink is refused without being followed,
/// truncated or replaced.
pub fn create_owned(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// Driver-owned foreign sentinel used only by the negative controls. Created
/// atomically and removed by its exact path on drop. While construction is in
/// progress the guard also owns the open descriptor, so an injected write
/// failure cannot leave the just-created file behind. The descriptor is closed
/// before unlink.
pub struct DriverFile {
    path: PathBuf,
    file: Option<File>,
}

impl DriverFile {
    pub fn create(root: &Path, label: &str, bytes: &[u8]) -> Self {
        Self::create_at(&root.join(label), bytes)
    }

    pub fn create_at(path: &Path, bytes: &[u8]) -> Self {
        use std::io::Write;
        Self::create_at_with(path, |file| {
            file.write_all(bytes)?;
            Ok(())
        })
    }

    /// Private seam for the post-acquisition fault controls. The closure runs
    /// only after `create_owned` has succeeded, so the guard already owns the
    /// path before any fallible write or deliberate unwind can occur.
    fn create_at_with(path: &Path, write: impl FnOnce(&mut File) -> io::Result<()>) -> Self {
        let file = create_owned(path)
            .unwrap_or_else(|error| panic!("create driver fixture {}: {error}", path.display()));
        let mut owned = Self {
            path: path.to_path_buf(),
            file: Some(file),
        };
        write(owned.file.as_mut().expect("driver fixture file is open"))
            .unwrap_or_else(|error| panic!("write driver fixture {}: {error}", path.display()));
        drop(owned.file.take());
        owned
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DriverFile {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Owns a spawned test worker from the instant `Command::spawn` succeeds,
/// before its stdout pipe is taken or a second child can be spawned. A panic
/// anywhere after acquisition drops this guard: stdin is closed so a blocked
/// worker can release its own input, and a worker that does not exit promptly
/// is killed and reaped. The guard removes only the exact `entry-qa-*` file
/// the worker announced as its own, and only if that file is still present.
pub struct WorkerGuard {
    root: PathBuf,
    child: Option<Child>,
    announced: Option<PathBuf>,
}

struct WorkerCleanup {
    pid: u32,
    announced: Option<PathBuf>,
    status: Option<ExitStatus>,
    killed: bool,
    removed: bool,
}

impl WorkerGuard {
    pub fn new(root: &Path, child: Child) -> Self {
        Self {
            root: root.to_path_buf(),
            child: Some(child),
            announced: None,
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.as_ref().map_or(0, Child::id)
    }

    pub fn child(&mut self) -> &mut Child {
        self.child
            .as_mut()
            .expect("worker guard child already reaped")
    }

    /// Record the exact owned input the worker announced. Failure cleanup is
    /// limited to this path; no directory scan or foreign file is ever used.
    pub fn record_acquired(&mut self, path: &Path) {
        self.announced = Some(path.to_path_buf());
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child().wait()
    }

    fn cleanup(&mut self) -> Option<WorkerCleanup> {
        let mut child = self.child.take()?;
        let pid = child.id();
        let announced = self.announced.take();
        drop(child.stdin.take());
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut status = None;
        loop {
            match child.try_wait() {
                Ok(Some(exit)) => {
                    status = Some(exit);
                    break;
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                _ => break,
            }
        }
        let killed = status.is_none();
        if killed {
            let _ = child.kill();
            status = child.wait().ok();
        }
        let removed = match announced.as_deref() {
            Some(path) if announced_fixture(&self.root, pid, path) => {
                std::fs::remove_file(path).is_ok()
            }
            _ => false,
        };
        Some(WorkerCleanup {
            pid,
            announced,
            status,
            killed,
            removed,
        })
    }
}

/// Failure cleanup must never touch another process's `entry-qa-*` file. The
/// worker's own decimal pid is part of the owner token, so the exact announced
/// name is bound to the child that printed it.
fn announced_fixture(root: &Path, pid: u32, path: &Path) -> bool {
    if path.parent() != Some(root) {
        return false;
    }
    let Some(name) = path.file_name().and_then(OsStr::to_str) else {
        return false;
    };
    let Some(extension) = path.extension().and_then(OsStr::to_str) else {
        return false;
    };
    name.starts_with("entry-qa-")
        && name.contains(&format!("-{pid}-"))
        && INPUT_EXTENSIONS.contains(&extension)
        && std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        let Some(cleanup) = self.cleanup() else {
            return;
        };
        use std::io::Write as _;
        let _ = writeln!(
            io::stderr(),
            "ENTRY_QA_WORKER_CLEANUP pid={} announced={:?} status={:?} code={:?} success={} killed={} removed={}",
            cleanup.pid,
            cleanup.announced,
            cleanup.status,
            cleanup.status.and_then(|status| status.code()),
            cleanup.status.is_some_and(|status| status.success()),
            cleanup.killed,
            cleanup.removed
        );
    }
}

/// Driver-owned dangling symlink for the ownership negative controls. The
/// target is never created by the driver or by the helper under test.
#[cfg(unix)]
pub struct DriverSymlink {
    path: PathBuf,
}

#[cfg(unix)]
impl DriverSymlink {
    pub fn create(path: &Path, target: &Path) -> Self {
        std::os::unix::fs::symlink(target, path)
            .unwrap_or_else(|error| panic!("create driver symlink {}: {error}", path.display()));
        Self {
            path: path.to_path_buf(),
        }
    }
}

#[cfg(unix)]
impl Drop for DriverSymlink {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::AssertUnwindSafe;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    fn assert_fault_removed(leaked: bool, path: &Path, existed: &AtomicBool, foreign: &DriverFile) {
        assert!(
            existed.load(Ordering::SeqCst),
            "the owned fixture did not exist at the injected fault point"
        );
        assert!(
            !leaked,
            "owned driver fixture leaked after the fault: {}",
            path.display()
        );
        assert!(foreign.path().is_file());
    }

    fn fault_path(root: &Path, purpose: &str) -> PathBuf {
        root.join(format!("entry-qa-driver-{purpose}-{}.js", owner_token()))
    }

    #[test]
    fn driver_write_fault_after_acquisition_removes_owned_fixture() {
        let root = select_root();
        let foreign = DriverFile::create(
            &root,
            &format!("entry-qa-foreign-driver-write-{}.js", owner_token()),
            b"foreign-driver-write",
        );
        let path = fault_path(&root, "write-fault");
        let existed = Arc::new(AtomicBool::new(false));
        let existed_in = Arc::clone(&existed);
        let path_arg = path.clone();
        let path_check = path.clone();
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _ = DriverFile::create_at_with(&path_arg, move |_file| {
                // Explicit unit fault injection at the post-acquisition
                // boundary; this is not represented as an OS disk failure.
                existed_in.store(
                    std::fs::symlink_metadata(&path_check).is_ok(),
                    Ordering::SeqCst,
                );
                Err(io::Error::other(
                    "injected driver write fault (unit seam, not an OS disk fault)",
                ))
            });
        }));
        let leaked = std::fs::symlink_metadata(&path).is_ok();
        let _ = std::fs::remove_file(&path);
        assert!(
            outcome.is_err(),
            "an injected write error must panic, never pass as success"
        );
        assert_fault_removed(leaked, &path, &existed, &foreign);
        assert_eq!(
            std::fs::read(foreign.path()).unwrap(),
            b"foreign-driver-write"
        );
    }

    #[test]
    fn driver_post_acquisition_unwind_removes_owned_fixture() {
        let root = select_root();
        let foreign = DriverFile::create(
            &root,
            &format!("entry-qa-foreign-driver-unwind-{}.js", owner_token()),
            b"foreign-driver-unwind",
        );
        let path = fault_path(&root, "unwind");
        let existed = Arc::new(AtomicBool::new(false));
        let existed_in = Arc::clone(&existed);
        let path_arg = path.clone();
        let path_check = path.clone();
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _ = DriverFile::create_at_with(&path_arg, move |_file| {
                existed_in.store(
                    std::fs::symlink_metadata(&path_check).is_ok(),
                    Ordering::SeqCst,
                );
                panic!("deliberate driver post-acquisition unwind");
            });
        }));
        let leaked = std::fs::symlink_metadata(&path).is_ok();
        let _ = std::fs::remove_file(&path);
        assert!(outcome.is_err(), "the deliberate unwind must propagate");
        assert_fault_removed(leaked, &path, &existed, &foreign);
        assert_eq!(
            std::fs::read(foreign.path()).unwrap(),
            b"foreign-driver-unwind"
        );
    }
}
