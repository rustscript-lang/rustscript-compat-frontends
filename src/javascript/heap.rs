//! Managed logical heap layout, limits and private helper contracts.
//!
//! One numeric B1 backing holds a fixed realm header, a page table, lazily
//! allocated directory pages, reusable raw extents and compiler root frames.
//! Five-field JsValue records carry realm-relative directory slot/generation
//! pairs; the physical base stays a compiler-internal guest value, never a
//! user field. All cells are numeric (Float); slots/offsets/counts decode in
//! the exact 17-bit domain, generations in the exact UInt32 domain.

/// Physical ceiling in cells, shared with the whole-source static budget.
pub(super) const PHYSICAL_CEILING: i64 = 65536;
/// Fixed realm header cells, then the page-pointer table.
pub(super) const HEADER_CELLS: i64 = 16;
pub(super) const PAGE_POINTER_CELLS: i64 = 64;
pub(super) const PREFIX_CELLS: i64 = HEADER_CELLS + PAGE_POINTER_CELLS;

/// Header field offsets in order.
pub(super) const HDR_VERSION: i64 = 0;
pub(super) const HDR_HEAP_END: i64 = 1;
pub(super) const HDR_RAW_FREE_HEAD: i64 = 2;
pub(super) const HDR_ROOT_TOP: i64 = 3;
pub(super) const HDR_NEXT_SLOT: i64 = 4;
pub(super) const HDR_FREE_SLOT_HEAD_PLUS_ONE: i64 = 5;
pub(super) const HDR_LIVE_NODES: i64 = 6;
pub(super) const HDR_NODE_CELLS: i64 = 7;
pub(super) const HDR_PENDING_NODES: i64 = 8;
pub(super) const HDR_ACTIVE_ROOT_CAPACITY: i64 = 9;
pub(super) const HDR_ALLOCATIONS_SINCE_GC: i64 = 10;
pub(super) const HDR_COLLECTIONS: i64 = 11;
pub(super) const HDR_RETIRED_SLOTS: i64 = 12;
pub(super) const HDR_GRAY_HEAD_PLUS_ONE: i64 = 13;
pub(super) const HDR_DIRECTORY_PAGES: i64 = 14;
/// Reserved header cell, always zero; named so the fixed 16-cell header
/// stays explicit in layout audits.
#[allow(dead_code)]
pub(super) const HDR_RESERVED: i64 = 15;

/// Realm header version written by bootstrap.
pub(super) const REALM_VERSION: i64 = 1;

/// Directory geometry: 64 pages of 64 rows, 7 cells per row.
pub(super) const DIRECTORY_PAGES: i64 = 64;
pub(super) const PAGE_ROWS: i64 = 64;
pub(super) const ROW_CELLS: i64 = 7;
pub(super) const MAX_SLOTS: i64 = DIRECTORY_PAGES * PAGE_ROWS;
pub(super) const DIR_PAGE_CELLS: i64 = 3 + PAGE_ROWS * ROW_CELLS;

/// Directory row field offsets.
pub(super) const ROW_STATE: i64 = 0;
pub(super) const ROW_GENERATION: i64 = 1;
pub(super) const ROW_PHYSICAL_BASE: i64 = 2;
pub(super) const ROW_DATA_CELLS: i64 = 3;
pub(super) const ROW_MARK: i64 = 4;
pub(super) const ROW_GRAY_NEXT_PLUS_ONE: i64 = 5;
pub(super) const ROW_FREE_NEXT_PLUS_ONE: i64 = 6;

/// Row states.
#[allow(dead_code)]
pub(super) const STATE_NEVER: i64 = 0;
pub(super) const STATE_PENDING: i64 = 1;
pub(super) const STATE_ACTIVE: i64 = 2;
pub(super) const STATE_FREE: i64 = 3;
pub(super) const STATE_RETIRED: i64 = 4;

/// Raw block header cells and classes.
pub(super) const RAW_HEADER_CELLS: i64 = 3;
pub(super) const RAW_CLASS_FREE: i64 = 0;
pub(super) const RAW_CLASS_NODE: i64 = 1;
pub(super) const RAW_CLASS_DIR_PAGE: i64 = 2;
pub(super) const RAW_CLASS_ROOT_FRAME: i64 = 3;

/// Raw header field offsets.
#[allow(dead_code)]
pub(super) const RAW_SIZE: i64 = 0;
pub(super) const RAW_CLASS: i64 = 1;
pub(super) const RAW_FREE_NEXT: i64 = 2;

/// A freed remainder must still fit a valid nonempty block.
pub(super) const MIN_RAW_BLOCK_CELLS: i64 = RAW_HEADER_CELLS + 1;

/// Node kinds.
pub(super) const KIND_ARRAY: i64 = 1;
pub(super) const KIND_STRING: i64 = 2;
pub(super) const KIND_OBJECT: i64 = 3;
pub(super) const KIND_FUNCTION: i64 = 4;
pub(super) const KIND_ENVIRONMENT: i64 = 5;

/// Node data header cells per kind (payload follows).
pub(super) const ARRAY_HEADER_CELLS: i64 = 5;
pub(super) const STRING_HEADER_CELLS: i64 = 3;
pub(super) const OBJECT_HEADER_CELLS: i64 = 5;
pub(super) const FUNCTION_LENGTH: i64 = 9;
pub(super) const FUNCTION_CELLS: i64 = 10;
pub(super) const ENV_HEADER_CELLS: i64 = 5;

/// Object row cells per own data property (key pair + tagged payload).
pub(super) const OBJECT_CELLS_PER_PROPERTY: i64 = 7;
/// Environment boxed-entry cells (tag/number/flag/slot/gen scalars).
pub(super) const ENV_CELLS_PER_ENTRY: i64 = 5;

/// Root frame extra header cell (capacity) after the raw prefix; each entry
/// is 3 cells (category/slot/generation).
pub(super) const FRAME_FIXED_CELLS: i64 = 4;
pub(super) const FRAME_CELLS_PER_ENTRY: i64 = 3;

/// Root entry categories.
pub(super) const ROOT_EMPTY: i64 = 0;
pub(super) const ROOT_STRING: i64 = 4;
pub(super) const ROOT_OBJECT: i64 = 5;
pub(super) const ROOT_INTERNAL: i64 = 6;

/// Committed allocations between automatic collections.
pub(super) const GC_INTERVAL_ALLOCATIONS: i64 = 32;
/// Total active root entries across frames.
pub(super) const MAX_ACTIVE_ROOT_ENTRIES: i64 = 8192;

/// Exact exclusive generation bound: generations live in
/// `[0, MAX_GENERATION]`; the maximum retires its slot instead of wrapping.
pub(super) const MAX_GENERATION_F64: f64 = 4294967295.0;

/// Private guest helper names (typed, never user-callable).
pub(super) const DECODE_U32_HELPER: &str = "__compat_es5_decode_u32";
pub(super) const STRING_EQUALITY_HELPER: &str = "__compat_es5_string_equal";
pub(super) const ROOT_ENTER_HELPER: &str = "__compat_es5_root_enter";
pub(super) const ROOT_WRITE_HELPER: &str = "__compat_es5_root_write";
pub(super) const ROOT_WRITE_NODE_HELPER: &str = "__compat_es5_root_write_node";
pub(super) const ROOT_LEAVE_HELPER: &str = "__compat_es5_root_leave";
pub(super) const NODE_BEGIN_HELPER: &str = "__compat_es5_node_begin";
pub(super) const NODE_PUBLISH_HELPER: &str = "__compat_es5_node_publish";
pub(super) const NODE_PUBLISH_EDGE_HELPER: &str = "__compat_es5_node_publish_edge";
pub(super) const CHECKED_BASE_HELPER: &str = "__compat_es5_checked_base";
pub(super) const MARK_HELPER: &str = "__compat_es5_mark";
pub(super) const RAW_RESERVE_HELPER: &str = "__compat_es5_raw_reserve";
pub(super) const HEAP_COLLECT_HELPER: &str = "__compat_es5_heap_collect";
pub(super) const HEAP_OBSERVE_HELPER: &str = "__compat_es5_heap_observe";
pub(super) const SET_PROTOTYPE_HELPER: &str = "__compat_es5_set_prototype";
pub(super) const REF_GET_HELPER: &str = "__compat_es5_ref_get";
pub(super) const DISPATCH_HELPER: &str = "__compat_es5_dispatch";

/// Private guest Named allocation handle `{slot, generation, base}`.
pub(super) const TYPE_ALLOCATION: &str = "__CompatEs5Allocation";

/// `heap_observe(Carrier, which, slot)` selector codes. Global counters
/// ignore `slot`; per-row selectors use it.
pub(super) const OBSERVE_LIVE_NODES: i64 = 0;
pub(super) const OBSERVE_RETIRED_SLOTS: i64 = 1;
pub(super) const OBSERVE_NEXT_SLOT: i64 = 2;
pub(super) const OBSERVE_HEAP_END: i64 = 3;
pub(super) const OBSERVE_ACTIVE_ROOT_CAPACITY: i64 = 4;
pub(super) const OBSERVE_COLLECTIONS: i64 = 5;
pub(super) const OBSERVE_ALLOCATIONS_SINCE_GC: i64 = 6;
pub(super) const OBSERVE_NODE_CELLS: i64 = 7;
pub(super) const OBSERVE_PENDING_NODES: i64 = 8;
pub(super) const OBSERVE_DIRECTORY_PAGES: i64 = 9;
pub(super) const OBSERVE_BACKING_LENGTH: i64 = 10;
pub(super) const OBSERVE_FREE_SLOTS: i64 = 11;
pub(super) const OBSERVE_ACTIVE_ROOTS: i64 = 12;
pub(super) const OBSERVE_ROW_STATE: i64 = 13;
pub(super) const OBSERVE_ROW_GENERATION: i64 = 14;
