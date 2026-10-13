use pd_host_function::pd_host_function;
use std::sync::Arc;
use vm::{
    HostApiCatalog, HostFunctionDescriptor, HostFunctionSchema, HostModuleDescriptor,
    HostParamSchema, HostResourceTypeMeta, HostTypeSchema, Value, Vm, VmError, VmResult,
};

pub const THROW_ERROR_FUNCTION: &str = "typescript::throw_error";
pub(crate) const THROW_ERROR_BINDING: &str = "\0typescript_error_throw";

pub(crate) fn throw_error_schema() -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        THROW_ERROR_FUNCTION,
        vec![HostParamSchema::value("message", HostTypeSchema::String)],
        HostTypeSchema::Null,
    )
}

mod generated {
    use super::*;

    pub use vm::borrow_arg;

    pub mod error_host {
        use super::*;

        #[pd_host_function(
            name = "typescript::throw_error",
            contract = super::super::throw_error_schema
        )]
        /// Stop execution with the JavaScript Error message unchanged.
        fn throw_error(_vm: &mut Vm, message: String) -> VmResult<Value> {
            Err(VmError::HostError(message))
        }
    }
}

const ERROR_FUNCTIONS: &[fn() -> HostFunctionDescriptor] =
    &[generated::error_host::throw_error_descriptor];
const ERROR_RESOURCES: &[fn() -> HostResourceTypeMeta] = &[];

/// Supplies the typed runtime adapter for the supported `throw new Error(string)` form.
pub fn typescript_error_host_module() -> HostModuleDescriptor {
    HostModuleDescriptor {
        name: "compat-typescript-error",
        functions: ERROR_FUNCTIONS,
        resources: ERROR_RESOURCES,
    }
}

/// Builds the exact host catalog required by the TypeScript Error adapter.
pub fn typescript_error_host_catalog() -> Arc<HostApiCatalog> {
    Arc::new(
        typescript_error_host_module()
            .catalog()
            .expect("the static TypeScript Error host module is valid"),
    )
}
