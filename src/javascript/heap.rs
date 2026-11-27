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
/// Raw base of the unique five-entry permanent bottom root frame, or zero.
pub(super) const HDR_INTRINSIC_FRAME: i64 = 15;

/// Realm header version written by bootstrap.
pub(super) const REALM_VERSION: i64 = 2;

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

/// Node kinds. Kinds 1..=9 are the closed private node-kind domain:
/// 1 packed numeric array, 2 string, 3 object, 4 function, 5 environment,
/// 6 tagged dense JsValue array, 7 ArrayBuffer backing, 8 TypedArray view,
/// 9 admitted `/a/` RegExp literal. Array element tags (Undefined..Object)
/// are a separate enum and never share these values.
pub(super) const KIND_ARRAY: i64 = 1;
pub(super) const KIND_STRING: i64 = 2;
pub(super) const KIND_OBJECT: i64 = 3;
pub(super) const KIND_FUNCTION: i64 = 4;
pub(super) const KIND_ENVIRONMENT: i64 = 5;
/// Dense array whose elements are five-cell JsValue entries. Same logical
/// Array and six-cell header as `KIND_ARRAY`; only the payload stride
/// differs. Not a new public type, profile or realm version.
pub(super) const KIND_TAGGED_ARRAY: i64 = 6;
pub(super) const KIND_BYTES: i64 = 7;
pub(super) const KIND_VIEW: i64 = 8;
/// The one admitted `/a/` literal object. The ordinary five-cell Object
/// header and the same own-row layout; cell 0 is the private brand guest
/// code can neither write nor forge. `String.prototype.search` admits only
/// this kind, so deleting or overwriting the public `test` row cannot change
/// the identity. Not a new public type, profile or realm version.
pub(super) const KIND_REGEXP: i64 = 9;
pub(super) const KIND_MAX: i64 = KIND_REGEXP;

// Kind, generation, reserved, prototype pair, buffer pair, byte offset,
// element count, element kind, and byte width. The backing owns all byte cells.
pub(super) const VIEW_CELLS: i64 = 11;
pub(super) const VIEW_BUFFER: i64 = 5;
pub(super) const VIEW_OFFSET: i64 = 7;
pub(super) const VIEW_LENGTH: i64 = 8;
pub(super) const VIEW_ELEMENT: i64 = 9;
pub(super) const VIEW_WIDTH: i64 = 10;

// Kind, generation, reserved, prototype slot/generation, byte length, then
// one Number cell per byte. No own property rows and no edges in the payload.
pub(super) const BYTES_HEADER_CELLS: i64 = 6;
pub(super) const BYTES_LENGTH_OFFSET: i64 = 5;
pub(super) const BYTES_MAX_LENGTH: i64 = 32_768;

/// Node data header cells per kind (payload follows).
/// Array cell 1 is the node generation publish checks. It is not present.
/// Cell 2 is the public length. Cells 3 and 4 are the prototype pair.
/// Cell 5 is physical present + 1, because the decoder rejects a raw 0.
pub(super) const ARRAY_HEADER_CELLS: i64 = 6;
pub(super) const ARRAY_PROTO_OFFSET: i64 = 3;
pub(super) const ARRAY_PRESENT_OFFSET: i64 = 5;
pub(super) const STRING_HEADER_CELLS: i64 = 3;
pub(super) const OBJECT_HEADER_CELLS: i64 = 5;
/// Only branded Error instances extend the Object header with a String pair.
pub(super) const ERROR_MESSAGE_OFFSET: i64 = OBJECT_HEADER_CELLS;
pub(super) const ERROR_HEADER_CELLS: i64 = OBJECT_HEADER_CELLS + 2;
pub(super) const FUNCTION_LENGTH: i64 = 9;
pub(super) const FUNCTION_CELLS: i64 = 10;
pub(super) const ENV_HEADER_CELLS: i64 = 5;

/// Object row cells per own data property (key pair + tagged payload).
pub(super) const OBJECT_CELLS_PER_PROPERTY: i64 = 7;
/// Compact ordinary header and row metadata; no legacy format is accepted.
pub(super) const OBJECT_COUNT_FACTOR: i64 = 8;
pub(super) const OBJECT_EXTENSIBLE_FLAG: i64 = 1;
pub(super) const OBJECT_LEGACY_ERROR_FLAG: i64 = 2;
pub(super) const OBJECT_ERROR_INSTANCE_FLAG: i64 = 4;
pub(super) const OBJECT_TAG_FACTOR: i64 = 16;
pub(super) const OBJECT_MAX_PACKED_HEADER: i64 = 256 * OBJECT_COUNT_FACTOR + 7;
pub(super) const OBJECT_MAX_DATA_META: i64 = 87;
/// Environment boxed-entry cells (tag/number/flag/slot/gen scalars).
pub(super) const ENV_CELLS_PER_ENTRY: i64 = 5;
/// Tagged dense-array element stride: the same tag/number/flag/slot/gen
/// scalar encoding used by environment entries, one entry per element.
pub(super) const ARRAY_TAGGED_CELLS_PER_ENTRY: i64 = ENV_CELLS_PER_ENTRY;

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
/// Canonical clear of one compiler-owned Undefined entry: no boxed payload.
pub(super) const ROOT_CLEAR_HELPER: &str = "__compat_es5_root_clear";
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
/// Dense-array element/length helpers shared by packed and tagged arrays.
pub(super) const ARRAY_GET_HELPER: &str = "__compat_es5_array_get";
pub(super) const ARRAY_PUT_HELPER: &str = "__compat_es5_array_put";
pub(super) const ARRAY_LENGTH_HELPER: &str = "__compat_es5_array_length";
pub(super) const ARRAY_SET_LENGTH_HELPER: &str = "__compat_es5_array_set_length";
/// Cell 2 is the plain public length. Cell 1 is the node generation, not
/// present. Cell 5 is physical present + 1. See `ARRAY_PRESENT_OFFSET`.
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
