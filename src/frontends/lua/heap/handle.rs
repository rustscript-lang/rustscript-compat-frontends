//! Private table handle. It is not a VM `Value` and has no bare-integer ABI.

/// Kind carried by a private handle. This milestone only allocates tables;
/// every operation still rejects a different kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleKind {
    /// A table node.
    Table,
}

/// Domain-checked reference to one table node.
///
/// `domain` ties the handle to the heap that allocated it. `slot` and
/// `generation` are checked against that heap on every use. A copied handle
/// is an alias only when those fields still match a live node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableRef {
    domain: u64,
    slot: u32,
    generation: u32,
    kind: HandleKind,
}

impl TableRef {
    pub(super) fn table(domain: u64, slot: u32, generation: u32) -> Self {
        Self {
            domain,
            slot,
            generation,
            kind: HandleKind::Table,
        }
    }

    /// Heap domain that allocated this handle.
    pub fn domain(self) -> u64 {
        self.domain
    }

    /// Stable slot index. Collection does not move it.
    pub fn slot(self) -> u32 {
        self.slot
    }

    /// Generation observed when the handle was issued.
    pub fn generation(self) -> u32 {
        self.generation
    }

    /// Handle kind. Table operations reject any other kind.
    pub fn kind(self) -> HandleKind {
        self.kind
    }
}
