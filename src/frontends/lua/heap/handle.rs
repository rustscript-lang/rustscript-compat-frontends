//! Private handles. They are not VM values and have no bare-integer ABI.

/// Kind carried by a private handle. Table, closure, and upvalue nodes share
/// one heap. An operation rejects a live node of a different kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleKind {
    /// A table node.
    Table,
    /// A closure node. It is not a callable value.
    Closure,
    /// A shared upvalue node. Its current edge holds a table, a closure, or a
    /// scalar (boolean, binary64, string, or bytes), or is absent (nil). It has
    /// no open or closed flag.
    Upvalue,
}

/// Domain-checked reference to one table node.
///
/// `domain` ties the handle to the heap that allocated it. `slot` and
/// `generation` are checked against that heap on every use. A copied handle
/// is an alias only when those fields still match a live node. Guest values
/// are not built from these fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableRef {
    domain: u64,
    slot: u32,
    generation: u32,
    kind: HandleKind,
}

impl TableRef {
    /// Field construction stays inside the heap. A guest `Value` is not a
    /// `TableRef` and cannot be assembled from these fields.
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

/// Domain-checked reference to one closure node.
///
/// Construction stays inside the heap. A guest `Value` is not a `ClosureRef`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClosureRef {
    domain: u64,
    slot: u32,
    generation: u32,
    kind: HandleKind,
}

impl ClosureRef {
    pub(super) fn closure(domain: u64, slot: u32, generation: u32) -> Self {
        Self {
            domain,
            slot,
            generation,
            kind: HandleKind::Closure,
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

    /// Handle kind. Closure operations reject any other kind.
    pub fn kind(self) -> HandleKind {
        self.kind
    }
}

/// Domain-checked reference to one shared upvalue node.
///
/// Construction stays inside the heap. The node has no open or closed flag.
/// Its current edge is a table, a closure, a scalar (boolean, binary64, string,
/// or bytes), or missing (nil).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpvalueRef {
    domain: u64,
    slot: u32,
    generation: u32,
    kind: HandleKind,
}

impl UpvalueRef {
    pub(super) fn upvalue(domain: u64, slot: u32, generation: u32) -> Self {
        Self {
            domain,
            slot,
            generation,
            kind: HandleKind::Upvalue,
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

    /// Handle kind. Upvalue operations reject any other kind.
    pub fn kind(self) -> HandleKind {
        self.kind
    }
}
