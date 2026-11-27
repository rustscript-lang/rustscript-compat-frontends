//! Compat scalar-math host adapter.
//!
//! Core already provides exact numeric builtins (`math::floor`, ...), which
//! reference-array sources keep using directly. Only `Math.random` has no
//! Core builtin, so this optional module provides exactly one honest
//! randomness source, `compat_math::random() -> Float` in `[0, 1)`, backed
//! by a time-seeded xorshift64* with a process-wide atomic stream mixer.
//! No new dependency is introduced. Primary evidence always uses this real
//! randomness and compares sorted outputs, never pivot choices; any
//! deterministic override lives only in separately labeled diagnostics
//! with its own test catalog, never in this production path.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use pd_host_function::pd_host_function;
use vm::{HostFunctionDescriptor, HostModuleDescriptor, HostResourceTypeMeta, Value, VmResult};

static RANDOM_STREAM: AtomicU64 = AtomicU64::new(0x9E3779B97F4A7C15);

fn random_schema() -> vm::HostFunctionSchema {
    vm::HostFunctionSchema::with_return(
        "compat_math::random",
        Vec::new(),
        vm::HostTypeSchema::Number,
    )
}

fn next_random_unit() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos() as u64 ^ duration.as_secs())
        .unwrap_or(0x853C49E6748FEA9B);
    let stream = RANDOM_STREAM.fetch_add(0x9E3779B97F4A7C15, Ordering::Relaxed);
    let mut state = nanos.wrapping_add(stream).wrapping_add(0xBF58476D1CE4E5B9);
    if state == 0 {
        state = 0xACE1DCE1DCE1DCE1;
    }
    state ^= state >> 12;
    state ^= state << 25;
    state ^= state >> 27;
    let product = state.wrapping_mul(0x2545F4914F6CDD1D);
    let bits = product >> 11;
    // 53 significant bits over 2^53: a Float in [0, 1).
    const SCALE: f64 = 9_007_199_254_740_992.0;
    (bits as f64) / SCALE
}

mod generated {
    use super::*;

    #[allow(unused_imports)]
    pub use vm::{arg, borrow_arg, take_arg};

    pub mod compat_math_host {
        use super::*;

        /// Returns a Float in [0, 1) from the process randomness stream.
        #[pd_host_function(name = "compat_math::random", contract = super::super::random_schema)]
        fn random() -> VmResult<Value> {
            Ok(Value::Float(super::next_random_unit()))
        }
    }
}

/// Optional Compat scalar-math module.
///
/// Contributes only `compat_math::random`. Compose into the
/// reference-array effective catalog; the default CLI and ordinary
/// compilation never install it.
pub fn typescript_scalar_math_host_module() -> HostModuleDescriptor {
    const MATH_FUNCTIONS: &[fn() -> HostFunctionDescriptor] =
        &[generated::compat_math_host::random_descriptor];
    const MATH_RESOURCES: &[fn() -> HostResourceTypeMeta] = &[];
    HostModuleDescriptor {
        name: "compat_math",
        functions: MATH_FUNCTIONS,
        resources: MATH_RESOURCES,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_stays_inside_the_unit_interval() {
        for _ in 0..512 {
            let value = next_random_unit();
            assert!(
                (0.0..1.0).contains(&value),
                "random must stay in [0, 1), saw {value}"
            );
            assert!(value.is_finite(), "random must stay finite");
        }
    }

    #[test]
    fn random_descriptor_takes_no_arguments() {
        let descriptor = generated::compat_math_host::random_descriptor();
        assert_eq!(descriptor.schema.name, "compat_math::random");
        assert!(descriptor.schema.params.is_empty());
    }
}
