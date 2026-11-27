//! Test-only catalog and exact bindings for the private Lua number operations.

use super::number::{self, BitwiseBinary};
use std::sync::{Arc, OnceLock};
use vm::{
    CallOutcome, CallReturn, HostApiBuilder, HostApiCatalog, HostFunctionRegistry,
    HostFunctionSchema, HostImportSchema, HostOwnedFunction, HostParamSchema, HostTypeSchema,
    OwnedHostCall, Value, Vm, VmError,
};

#[derive(Clone, Copy)]
enum BitwiseHost {
    Not,
    Binary(BitwiseBinary),
}

const OPERATIONS: [BitwiseHost; 7] = [
    BitwiseHost::Not,
    BitwiseHost::Binary(BitwiseBinary::And),
    BitwiseHost::Binary(BitwiseBinary::Or),
    BitwiseHost::Binary(BitwiseBinary::Xor),
    BitwiseHost::Binary(BitwiseBinary::Shl),
    BitwiseHost::Binary(BitwiseBinary::Shr),
    BitwiseHost::Binary(BitwiseBinary::Sar),
];

impl BitwiseHost {
    fn name(self) -> &'static str {
        match self {
            Self::Not => number::BIT_NOT,
            Self::Binary(op) => op.name(),
        }
    }
}

impl HostOwnedFunction for BitwiseHost {
    fn call(&mut self, call: &mut OwnedHostCall<'_>) -> Result<CallOutcome, VmError> {
        let result = match (*self, call.args()) {
            (Self::Not, [Value::Float(value)]) => number::bit_not(*value),
            (Self::Binary(op), [Value::Float(left), Value::Float(right)]) => {
                op.evaluate(*left, *right)
            }
            _ => return Err(VmError::HostError("expected classified Lua numbers".into())),
        };
        Ok(CallOutcome::Return(CallReturn::one(Value::Float(result))))
    }
}

pub(super) fn catalog() -> &'static Arc<HostApiCatalog> {
    static CATALOG: OnceLock<Arc<HostApiCatalog>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut builder = HostApiBuilder::new();
        for op in OPERATIONS {
            let names: &[&str] = match op {
                BitwiseHost::Not => &["value"],
                BitwiseHost::Binary(_) => &["left", "right"],
            };
            builder.function(HostFunctionSchema::with_return(
                op.name(),
                names
                    .iter()
                    .map(|name| HostParamSchema::value(*name, HostTypeSchema::Float))
                    .collect(),
                HostTypeSchema::Float,
            ));
        }
        Arc::new(builder.build().expect("Lua bitwise catalog"))
    })
}

pub(super) fn bind(vm: &mut Vm) -> Result<(), VmError> {
    let catalog = catalog();
    let mut registry = HostFunctionRegistry::empty();
    for op in OPERATIONS {
        let name = op.name();
        let function = catalog.function(name).expect("Lua bitwise schema");
        registry.register_exact_owned(
            name,
            function.params.len() as u8,
            HostImportSchema::from_function(catalog, function),
            move |_| Box::new(op),
        )?;
        registry.authorize_registered_builtin_import(name);
    }
    registry.bind_vm_cached(vm)
}
