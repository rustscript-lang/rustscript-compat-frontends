//! Typed snapshot boundary for reference-array sources.
//!
//! This optional child exposes one exact descriptor,
//! `arrays::snapshot(candidate: Borrow Resource(compat.number_array))
//! -> Array(Number)`, returning an owned Core-array copy of the payload.
//! The copy is an `O(n)` boundary cost; ordinary element writes never copy.
//! The B1 four-descriptor owned module and the manual `arrays::*`
//! handlers are unchanged; this module only adds the snapshot boundary
//! used by reference-array dispatcher operation 6.

use pd_host_function::pd_host_function;
use vm::resource::ResourceRef;
use vm::{HostFunctionDescriptor, HostModuleDescriptor, HostResourceTypeMeta, Value, Vm, VmResult};

use super::{NumberArray, array_key};

fn snapshot_schema() -> vm::HostFunctionSchema {
    vm::HostFunctionSchema::with_return(
        "arrays::snapshot",
        vec![vm::HostParamSchema::with_passing(
            "candidate",
            vm::HostTypeSchema::Resource(array_key()),
            vm::HostParamPassing::Borrow,
        )],
        vm::HostTypeSchema::Array(Box::new(vm::HostTypeSchema::Number)),
    )
}

mod generated {
    use super::*;

    #[allow(unused_imports)]
    pub use vm::{arg, borrow_arg, take_arg};

    pub mod array_snapshot_host {
        use super::*;

        /// Copies one numeric payload into a fresh Core array value.
        #[pd_host_function(name = "arrays::snapshot", contract = super::super::snapshot_schema)]
        fn snapshot(
            #[pd_host_resource(passing = "borrow", key = "compat.number_array")] array: ResourceRef<
                '_,
                NumberArray,
            >,
        ) -> VmResult<Value> {
            Ok(Value::Array(array.0.to_vec().into()))
        }
    }
}

/// Optional snapshot boundary module for reference-array dispatchers.
///
/// Contributes only `arrays::snapshot` plus the shared
/// `compat.number_array` resource key. Compose with the unchanged manual
/// `arrays::*` module and the B1 owned module into one effective catalog.
pub fn typescript_array_snapshot_host_module() -> HostModuleDescriptor {
    const SNAPSHOT_FUNCTIONS: &[fn() -> HostFunctionDescriptor] =
        &[generated::array_snapshot_host::snapshot_descriptor];
    const SNAPSHOT_RESOURCES: &[fn() -> HostResourceTypeMeta] =
        &[HostResourceTypeMeta::of::<NumberArray>];
    HostModuleDescriptor {
        name: "compat-array-snapshot",
        functions: SNAPSHOT_FUNCTIONS,
        resources: SNAPSHOT_RESOURCES,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_descriptor_is_a_typed_borrow_boundary() {
        let descriptor = generated::array_snapshot_host::snapshot_descriptor();
        assert_eq!(descriptor.schema.name, "arrays::snapshot");
        assert_eq!(descriptor.schema.params.len(), 1);
        assert!(matches!(
            descriptor.schema.return_type,
            vm::HostTypeSchema::Array(_)
        ));
    }
}
