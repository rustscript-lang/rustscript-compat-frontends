use pd_host_function::pd_host_function;
use vm::resource::{HostResource, ResourceMut, ResourceOwned, ResourceRef};
use vm::{
    HostFunctionDescriptor, HostFunctionSchema, HostModuleDescriptor, HostParamPassing,
    HostParamSchema, HostResourceType, HostResourceTypeMeta, HostTypeSchema, Resource,
    ResourceTypeKey, Value, Vm, VmError, VmResult,
};

#[derive(Debug)]
struct NumberArray(Vec<Value>);

impl HostResource for NumberArray {
    fn resource_type_key() -> Option<ResourceTypeKey> {
        Some(ResourceTypeKey::new(Self::KEY).expect("static TypeScript array key is valid"))
    }
}

impl HostResourceType for NumberArray {
    const KEY: &'static str = "compat.number_array";
    const DESCRIPTION: &'static str = "Compat-owned numeric array storage";
}

fn array_key() -> ResourceTypeKey {
    ResourceTypeKey::new(NumberArray::KEY).expect("static TypeScript array key is valid")
}

fn create_schema() -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        "arrays::create",
        vec![HostParamSchema::value(
            "initial",
            HostTypeSchema::Array(Box::new(HostTypeSchema::Number)),
        )],
        HostTypeSchema::Resource(array_key()),
    )
}

fn read_schema() -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        "arrays::read",
        vec![
            HostParamSchema::with_passing(
                "array",
                HostTypeSchema::Resource(array_key()),
                HostParamPassing::Borrow,
            ),
            HostParamSchema::value("index", HostTypeSchema::Number),
        ],
        HostTypeSchema::Number,
    )
}

fn try_read_schema() -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        "arrays::try_read",
        vec![
            HostParamSchema::with_passing(
                "array",
                HostTypeSchema::Resource(array_key()),
                HostParamPassing::Borrow,
            ),
            HostParamSchema::value("index", HostTypeSchema::Number),
        ],
        HostTypeSchema::Optional(Box::new(HostTypeSchema::Number)),
    )
}

fn has_index_schema() -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        "arrays::has_index",
        vec![
            HostParamSchema::with_passing(
                "array",
                HostTypeSchema::Resource(array_key()),
                HostParamPassing::Borrow,
            ),
            HostParamSchema::value("index", HostTypeSchema::Number),
        ],
        HostTypeSchema::Bool,
    )
}

fn write_schema() -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        "arrays::write",
        vec![
            HostParamSchema::with_passing(
                "array",
                HostTypeSchema::Resource(array_key()),
                HostParamPassing::BorrowMut,
            ),
            HostParamSchema::value("index", HostTypeSchema::Number),
            HostParamSchema::value("value", HostTypeSchema::Number),
        ],
        HostTypeSchema::Number,
    )
}

fn length_schema() -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        "arrays::length",
        vec![HostParamSchema::with_passing(
            "array",
            HostTypeSchema::Resource(array_key()),
            HostParamPassing::Borrow,
        )],
        HostTypeSchema::Number,
    )
}

fn close_schema() -> HostFunctionSchema {
    HostFunctionSchema::with_return(
        "arrays::close",
        vec![HostParamSchema::with_passing(
            "array",
            HostTypeSchema::Resource(array_key()),
            HostParamPassing::TakeOwned,
        )],
        HostTypeSchema::Number,
    )
}

fn is_number(value: &Value) -> bool {
    matches!(value, Value::Int(_) | Value::Float(_))
}

fn checked_index(value: &Value, len: usize) -> VmResult<usize> {
    let invalid_index =
        || VmError::HostError("array index must be integral and in bounds".to_owned());
    let index = match value {
        Value::Int(value) => usize::try_from(*value).map_err(|_| invalid_index())?,
        Value::Float(value)
            if value.is_finite()
                && value.fract() == 0.0
                && *value >= 0.0
                && *value < len as f64 =>
        {
            *value as usize
        }
        _ => return Err(invalid_index()),
    };
    if index < len {
        Ok(index)
    } else {
        Err(invalid_index())
    }
}

fn checked_optional_index(value: &Value, len: usize) -> VmResult<Option<usize>> {
    let invalid_index = || VmError::HostError("array index must be finite and integral".to_owned());
    match value {
        Value::Int(value) if *value < 0 => Ok(None),
        Value::Int(value) => {
            let Ok(index) = usize::try_from(*value) else {
                return Ok(None);
            };
            Ok((index < len).then_some(index))
        }
        Value::Float(value) if value.is_finite() && value.fract() == 0.0 => {
            if *value < 0.0 || *value >= len as f64 {
                Ok(None)
            } else {
                Ok(Some(*value as usize))
            }
        }
        _ => Err(invalid_index()),
    }
}

mod generated {
    use super::*;

    pub use vm::{arg, borrow_arg, take_arg};

    pub mod array_host {
        use super::*;

        /// Create one VM-owned numeric array from a checked Core array value.
        #[pd_host_function(name = "arrays::create", contract = super::super::create_schema)]
        fn create(vm: &mut Vm, initial: Value) -> VmResult<Resource<NumberArray>> {
            let Value::Array(values) = initial else {
                return Err(VmError::HostError("expected numeric array".to_owned()));
            };
            if values.iter().any(|value| !is_number(value)) {
                return Err(VmError::HostError("expected numeric array".to_owned()));
            }
            vm.host_context()
                .push_resource(NumberArray(values.iter().cloned().collect()))
                .map_err(|error| VmError::HostError(error.to_string()))
        }

        /// Read one checked numeric index through a scoped resource borrow.
        #[pd_host_function(name = "arrays::read", contract = super::super::read_schema)]
        fn read(
            #[pd_host_resource(passing = "borrow", key = "compat.number_array")] array: ResourceRef<
                '_,
                NumberArray,
            >,
            index: Value,
        ) -> VmResult<Value> {
            let index = checked_index(&index, array.0.len())?;
            Ok(array.0[index].clone())
        }

        /// Return `null` for integral but missing indices; reject invalid numbers.
        #[pd_host_function(name = "arrays::try_read", contract = super::super::try_read_schema)]
        fn try_read(
            #[pd_host_resource(passing = "borrow", key = "compat.number_array")] array: ResourceRef<
                '_,
                NumberArray,
            >,
            index: Value,
        ) -> VmResult<Value> {
            match checked_optional_index(&index, array.0.len())? {
                Some(index) => Ok(array.0[index].clone()),
                None => Ok(Value::Null),
            }
        }

        /// Test numeric-index presence without treating an integral miss as invalid.
        #[pd_host_function(name = "arrays::has_index", contract = super::super::has_index_schema)]
        fn has_index(
            #[pd_host_resource(passing = "borrow", key = "compat.number_array")] array: ResourceRef<
                '_,
                NumberArray,
            >,
            index: Value,
        ) -> VmResult<Value> {
            Ok(Value::Bool(
                checked_optional_index(&index, array.0.len())?.is_some(),
            ))
        }

        /// Mutate one checked numeric index through a scoped mutable borrow.
        #[pd_host_function(name = "arrays::write", contract = super::super::write_schema)]
        fn write(
            mut array: ResourceMut<'_, NumberArray>,
            index: Value,
            value: Value,
        ) -> VmResult<Value> {
            if !is_number(&value) {
                return Err(VmError::HostError("expected numeric value".to_owned()));
            }
            let index = checked_index(&index, array.0.len())?;
            array.0[index] = value.clone();
            Ok(value)
        }

        /// Return the current numeric array length through an immutable borrow.
        #[pd_host_function(name = "arrays::length", contract = super::super::length_schema)]
        fn length(
            #[pd_host_resource(passing = "borrow", key = "compat.number_array")] array: ResourceRef<
                '_,
                NumberArray,
            >,
        ) -> VmResult<Value> {
            let length = i64::try_from(array.0.len())
                .map_err(|_| VmError::HostError("array length exceeds numeric range".to_owned()))?;
            Ok(Value::Int(length))
        }

        /// Consume the explicitly transferred array owner and release its storage.
        #[pd_host_function(name = "arrays::close", contract = super::super::close_schema)]
        fn close(
            #[pd_host_resource(passing = "take_owned", key = "compat.number_array")]
            array: ResourceOwned<NumberArray>,
        ) -> VmResult<Value> {
            drop(array.into_inner());
            Ok(Value::Int(1))
        }
    }
}

const ARRAY_FUNCTIONS: &[fn() -> HostFunctionDescriptor] = &[
    generated::array_host::create_descriptor,
    generated::array_host::read_descriptor,
    generated::array_host::try_read_descriptor,
    generated::array_host::has_index_descriptor,
    generated::array_host::write_descriptor,
    generated::array_host::length_descriptor,
    generated::array_host::close_descriptor,
];

const ARRAY_RESOURCES: &[fn() -> HostResourceTypeMeta] = &[HostResourceTypeMeta::of::<NumberArray>];

/// Provides typed native storage operations for compat reference arrays.
///
/// The returned module owns no VM state. Each `arrays::create` call inserts a
/// normal Core resource into the executing VM's resource table; a single guest
/// dispatcher closure can move that resource into its shared callable
/// environment and expose read/try_read/has_index/write/length/close operations through typed
/// imports.
pub fn typescript_array_host_module() -> HostModuleDescriptor {
    HostModuleDescriptor {
        name: "compat-arrays",
        functions: ARRAY_FUNCTIONS,
        resources: ARRAY_RESOURCES,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_optional_index_separates_missing_from_invalid_numbers() {
        assert_eq!(checked_optional_index(&Value::Int(-1), 2).unwrap(), None);
        assert_eq!(checked_optional_index(&Value::Int(4), 2).unwrap(), None);
        assert_eq!(
            checked_optional_index(&Value::Float(-1.0), 2).unwrap(),
            None
        );
        assert_eq!(checked_optional_index(&Value::Float(2.0), 2).unwrap(), None);
        assert_eq!(
            checked_optional_index(&Value::Float(1.0), 2).unwrap(),
            Some(1)
        );

        for invalid in [
            Value::Float(0.5),
            Value::Float(f64::NAN),
            Value::Float(f64::INFINITY),
            Value::Float(f64::NEG_INFINITY),
        ] {
            assert!(
                checked_optional_index(&invalid, 2).is_err(),
                "invalid dynamic index should be rejected: {invalid:?}"
            );
        }
    }
}
