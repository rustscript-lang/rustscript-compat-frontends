//! File-only ES5 corpus completion observer.
//!
//! The only entry is a source path compiled through the accepted JavaScript
//! file pipeline. Completion authority is the halted VM's final primitive
//! number box, copied out before owner reset. This module does not project
//! arbitrary maps, accept an expected cookie, or read guest heap after reset.
use std::path::{Path, PathBuf};
use std::task::{Context, Poll};

const CORPUS_FUEL: u64 = 10_000_000;
const COOKIE_MIN: f64 = 1.0;
const COOKIE_EXCLUSIVE_MAX: f64 = 1_073_741_824.0;
const BOX_FIELDS: [&str; 5] = ["tag", "number", "flag", "ref_slot", "generation"];

#[derive(Debug)]
pub struct CorpusCompletion {
    pub cookie: i64,
    pub aot_exec_count: u64,
    pub jit_native_exec_count: u64,
    pub resources_before_reset: usize,
    pub resources_after_reset: usize,
    pub certification_valid_before_reset: bool,
    pub certification_valid_after_reset: bool,
    pub owned_arrays_retired: bool,
}

#[derive(Debug)]
pub enum CorpusFileError {
    Source {
        path: PathBuf,
        error: Box<vm::SourcePathError>,
    },
    Execution {
        error: Box<vm::VmError>,
        rendered: String,
    },
    Protocol(String),
    Cleanup(Box<vm::VmError>),
}

impl std::fmt::Display for CorpusFileError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source { path, error } => {
                write!(
                    formatter,
                    "{}",
                    vm::render_source_path_error(path, error, false)
                )
            }
            Self::Execution { rendered, error } => {
                if rendered.is_empty() {
                    write!(formatter, "{error}")
                } else {
                    write!(formatter, "{rendered}")
                }
            }
            Self::Protocol(message) => write!(formatter, "{message}"),
            Self::Cleanup(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for CorpusFileError {}

struct Measured {
    cookie: Result<i64, CorpusFileError>,
    aot_exec_count: u64,
    jit_native_exec_count: u64,
    resources_before_reset: usize,
    certification_valid_before_reset: bool,
}

struct Retired {
    resources_after_reset: usize,
    certification_valid_after_reset: bool,
    owned_arrays_retired: bool,
}

pub fn observe_javascript_es5_corpus_file(
    path: &Path,
) -> Result<CorpusCompletion, CorpusFileError> {
    let mut machine = compile_certified_file(path)?;
    let measured = execute_and_measure(&mut machine);
    let retired = retire_corpus_vm(&mut machine);
    match (measured, retired) {
        (Ok(measured), Ok(retired)) => Ok(CorpusCompletion {
            cookie: measured.cookie?,
            aot_exec_count: measured.aot_exec_count,
            jit_native_exec_count: measured.jit_native_exec_count,
            resources_before_reset: measured.resources_before_reset,
            resources_after_reset: retired.resources_after_reset,
            certification_valid_before_reset: measured.certification_valid_before_reset,
            certification_valid_after_reset: retired.certification_valid_after_reset,
            owned_arrays_retired: retired.owned_arrays_retired,
        }),
        (_, Err(cleanup)) => Err(cleanup),
        (Err(error), Ok(_)) => Err(error),
    }
}

fn compile_certified_file(path: &Path) -> Result<vm::Vm, CorpusFileError> {
    match super::cli::compile_javascript_es5_file(path) {
        Ok(machine) => Ok(machine),
        Err(super::cli::CertifiedFileError::Source(error)) => Err(CorpusFileError::Source {
            path: path.to_path_buf(),
            error,
        }),
        Err(super::cli::CertifiedFileError::Vm(error)) => Err(CorpusFileError::Execution {
            rendered: error.to_string(),
            error,
        }),
    }
}

fn execute_and_measure(machine: &mut vm::Vm) -> Result<Measured, CorpusFileError> {
    if machine.jit_config().enabled {
        return Err(CorpusFileError::Protocol(
            "corpus observation refuses an enabled trace JIT".into(),
        ));
    }
    machine.set_fuel(CORPUS_FUEL);
    let status = machine.run();
    let aot_exec_count = machine.aot_exec_count();
    let jit_native_exec_count = machine.jit_native_exec_count();
    let resources_before_reset = machine.host_context().resource_count();
    let certification_valid_before_reset = machine
        .capture_certification_stats()
        .is_some_and(|facts| facts.valid);
    let cookie = completion_cookie(machine, status, aot_exec_count, jit_native_exec_count);
    Ok(Measured {
        cookie,
        aot_exec_count,
        jit_native_exec_count,
        resources_before_reset,
        certification_valid_before_reset,
    })
}

fn completion_cookie(
    machine: &vm::Vm,
    status: vm::VmResult<vm::VmStatus>,
    aot_exec_count: u64,
    jit_native_exec_count: u64,
) -> Result<i64, CorpusFileError> {
    match status {
        Err(error) => Err(CorpusFileError::Execution {
            rendered: vm::render_vm_error(machine, &error),
            error: Box::new(error),
        }),
        Ok(vm::VmStatus::Halted) if aot_exec_count == 0 && jit_native_exec_count == 0 => {
            cookie_from_completion(machine.stack().last()).map_err(CorpusFileError::Protocol)
        }
        Ok(vm::VmStatus::Halted) => Err(CorpusFileError::Protocol(
            "corpus observation refuses native execution".into(),
        )),
        Ok(vm::VmStatus::Yielded) => Err(CorpusFileError::Protocol(format!(
            "execution yielded before halt: {:?}",
            machine.last_yield_reason()
        ))),
        Ok(vm::VmStatus::Waiting(_)) => Err(CorpusFileError::Protocol(
            "execution waiting is not a corpus completion".into(),
        )),
    }
}

fn cookie_from_completion(value: Option<&vm::Value>) -> Result<i64, String> {
    let Some(vm::Value::Map(fields)) = value else {
        return Err("completion is not a primitive record".into());
    };
    if fields.len() != BOX_FIELDS.len()
        || BOX_FIELDS
            .iter()
            .any(|key| fields.get(&vm::Value::string(*key)).is_none())
    {
        return Err("completion record is not the primitive box".into());
    }
    let tag = match fields.get(&vm::Value::string("tag")) {
        Some(vm::Value::Int(tag)) => *tag,
        _ => return Err("completion tag is not an integer".into()),
    };
    if tag != super::es5_number_tag() {
        return Err("completion is not a JavaScript number".into());
    }
    let number = match fields.get(&vm::Value::string("number")) {
        Some(vm::Value::Float(number)) => *number,
        _ => return Err("completion number is not an observed float".into()),
    };
    let flag = match fields.get(&vm::Value::string("flag")) {
        Some(vm::Value::Bool(flag)) => *flag,
        _ => return Err("completion flag is not a boolean".into()),
    };
    let ref_slot = match fields.get(&vm::Value::string("ref_slot")) {
        Some(vm::Value::Int(slot)) => *slot,
        _ => return Err("completion ref_slot is not an integer".into()),
    };
    let generation = match fields.get(&vm::Value::string("generation")) {
        Some(vm::Value::Int(generation)) => *generation,
        _ => return Err("completion generation is not an integer".into()),
    };
    if flag || ref_slot != 0 || generation != 0 {
        return Err("completion number is not an inactive primitive".into());
    }
    safe_cookie(number)
}

fn safe_cookie(number: f64) -> Result<i64, String> {
    if !number.is_finite()
        || number.fract() != 0.0
        || !(COOKIE_MIN..COOKIE_EXCLUSIVE_MAX).contains(&number)
    {
        return Err("completion number is not a finite integral corpus cookie".into());
    }
    let cookie = number as i64;
    if cookie as f64 != number {
        return Err("completion number is not a safe integer".into());
    }
    Ok(cookie)
}

fn retire_corpus_vm(machine: &mut vm::Vm) -> Result<Retired, CorpusFileError> {
    machine
        .reset_for_reuse()
        .map_err(|error| CorpusFileError::Cleanup(Box::new(error)))?;
    if machine.scope_reset_pending() {
        let waker = std::task::Waker::noop();
        let mut context = Context::from_waker(waker);
        match machine.poll_reset_for_reuse(&mut context) {
            Poll::Ready(result) => {
                result.map_err(|error| CorpusFileError::Cleanup(Box::new(error)))?;
            }
            Poll::Pending => {
                return Err(CorpusFileError::Protocol(
                    "corpus owner reset did not reach quiescence".into(),
                ));
            }
        }
    }
    if machine.scope_reset_pending() {
        return Err(CorpusFileError::Protocol(
            "corpus owner reset remained pending".into(),
        ));
    }
    let resources_after_reset = machine.host_context().resource_count();
    if resources_after_reset != 0 {
        return Err(CorpusFileError::Protocol(format!(
            "corpus resources survived reset: {resources_after_reset}"
        )));
    }
    let certification_valid_after_reset = match machine.capture_certification_stats() {
        Some(facts) if !facts.valid => false,
        _ => {
            return Err(CorpusFileError::Protocol(
                "corpus certification was not retired".into(),
            ));
        }
    };
    let owned_arrays_retired = crate::runtime::array::owned_array_runtime_stats(machine).is_none();
    if !owned_arrays_retired {
        return Err(CorpusFileError::Protocol(
            "owned array directory survived reset".into(),
        ));
    }
    Ok(Retired {
        resources_after_reset,
        certification_valid_after_reset,
        owned_arrays_retired,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Scratch(PathBuf);

    impl Scratch {
        fn file(&self, name: &str, source: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, source).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch() -> Scratch {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let base = PathBuf::from(std::env::var_os("TMPDIR").expect("assigned TMPDIR"));
        let path = base.join(format!(
            "p11-bridge-unit-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Scratch(path)
    }

    fn assert_retired(machine: &mut vm::Vm) {
        assert_eq!(machine.host_context().resource_count(), 0);
        assert!(!machine.capture_certification_stats().unwrap().valid);
        assert!(crate::runtime::array::owned_array_runtime_stats(machine).is_none());
        assert!(!machine.scope_reset_pending());
    }

    #[test]
    fn success_and_throw_reset_the_same_vm() {
        let files = scratch();
        let mut machine =
            compile_certified_file(&files.file("owned.js", "var o={};42;")).expect("object source");
        machine.set_fuel(CORPUS_FUEL);
        assert_eq!(machine.run().unwrap(), vm::VmStatus::Halted);
        assert_eq!(machine.aot_exec_count(), 0);
        assert_eq!(machine.jit_native_exec_count(), 0);
        assert!(machine.host_context().resource_count() > 0);
        assert!(machine.capture_certification_stats().unwrap().valid);
        assert_eq!(cookie_from_completion(machine.stack().last()).unwrap(), 42);
        retire_corpus_vm(&mut machine).unwrap();
        assert_retired(&mut machine);

        let mut thrown =
            compile_certified_file(&files.file("throw.js", "throw 7;")).expect("throw source");
        thrown.set_fuel(CORPUS_FUEL);
        assert!(thrown.run().is_err(), "throw must not halt as success");
        assert!(
            thrown
                .capture_certification_stats()
                .is_some_and(|facts| facts.valid)
        );
        retire_corpus_vm(&mut thrown).unwrap();
        assert_retired(&mut thrown);
    }
}
