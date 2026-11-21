//! Typed resource token for a private table handle.
//!
//! The token crosses the host boundary as `resource<lua.table>`. Its physical
//! VM carrier is the existing resource handle, validated by arena, type, and
//! key. It does not store table fields and is not a GC root.

use vm::ResourceTypeKey;
use vm::host_extension::HostResourceType;
use vm::resource::HostResource;

use super::handle::TableRef;

/// Public object capability for one private table node.
#[derive(Debug)]
pub struct LuaTableResource {
    table: TableRef,
}

impl LuaTableResource {
    pub(super) fn new(table: TableRef) -> Self {
        Self { table }
    }

    /// The private handle stored in this token.
    pub fn table(&self) -> TableRef {
        self.table
    }

    /// Catalog key used by `typed_resource_with_key`.
    pub fn key() -> ResourceTypeKey {
        ResourceTypeKey::new(Self::KEY).expect("lua.table is a valid resource key")
    }
}

impl HostResource for LuaTableResource {
    fn resource_type_key() -> Option<ResourceTypeKey> {
        Some(Self::key())
    }
}

impl HostResourceType for LuaTableResource {
    const KEY: &'static str = "lua.table";
    const DESCRIPTION: &'static str = "Lua table";
}
