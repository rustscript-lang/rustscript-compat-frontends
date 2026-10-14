#![allow(dead_code)]
//! Reference-array storage, factory and projection logic.
//!
//! This private child of the TypeScript frontend owns the explicit
//! opt-in reference-array compilation mode. Ordinary compilation never
//! consults this module. In reference-array mode the Lowerer appends
//! [`REFERENCE_ARRAY_PRELUDE`] to the user source in memory (user bytes
//! and spans are unchanged; fixtures on disk stay byte-identical) and
//! rewrites logical `number[]` bindings into a private fully typed guest
//! record containing one owning dispatcher callable.
//!
//! Representation (proven by the B1 guest-factory pattern, extended here
//! to a fixed typed result record):
//!
//! - Logical `number[]` is distinct from physical storage. Storage is a
//!   private hygienic guest named record with exactly one typed field,
//!   `dispatcher`, holding the owning dispatcher callable. All aliases
//!   share the same dispatcher environment; the actual `Resource` moves
//!   exactly once into the owning dispatcher.
//! - The dispatcher has fixed operation numbers: 0 register, 1 read,
//!   2 write, 3 length, 4 presence, 5 resize, 6 snapshot, 7 explicit
//!   close, 8 safety point, plus 9 guarded `<`, 10 guarded `>`, 11
//!   two-sided guarded `<` and 12 two-sided guarded `>` (same carrier; the
//!   `value` parameter carries the second index). The guarded operations
//!   exist because the native SSA backend only branches on statically
//!   typed booleans: the presence-aware comparison must be computed from
//!   typed host calls inside the dispatcher body rather than by branching
//!   on a dynamically read record field in a helper. It returns a private
//!   fixed fully typed result record `{ number: Number, present: Bool,
//!   values: Array(Number) }`.
//!   Numeric operations return the scalar field, presence returns an
//!   explicitly consumed presence field, and snapshot uses `values`.
//!   Non-snapshot paths return an explicitly typed empty staging array,
//!   never a copy of the backing array.
//! - A generated source-IR factory takes a plain Core `Array(Number)`
//!   staging input, allocates through the production B1 runtime, builds
//!   the complete dispatcher, invokes register through it before return,
//!   and wraps/returns the carrier. Snapshot/init copies are the allowed
//!   `O(n)` boundary costs; ordinary writes never copy the payload.
//!
//! The Lowerer never synthesizes direct host calls. Every generated
//! operation is an ordinary guest call into the prelude helpers below,
//! so capture inference, Move semantics, host-call wrapping and schema
//! checking reuse the exact existing paths. Array identity compares the
//! canonical dispatcher identity; rebinding replaces only the source
//! variable's carrier. A user fake callable assigned to `number[]`
//! rejects by ordinary schema mismatch: carrier schemas only arise from
//! the factory, never from callable shapes, global slots or name
//! heuristics.

use std::sync::Arc;

use vm::TypeSchema;

/// Hygienic guest type names. Documented as reserved in reference-array
/// mode; a user declaration colliding with one fails precisely at
/// predeclare time like any other duplicate.
pub(crate) const TYPE_CARRIER: &str = "__CompatRefCarrier";
pub(crate) const TYPE_DISPATCHER: &str = "__CompatRefDispatcher";
pub(crate) const TYPE_RESULT: &str = "__CompatRefResult";

/// Prelude helper function names.
pub(crate) const HELPER_MAKE: &str = "__compat_ref_make";
pub(crate) const HELPER_READ: &str = "__compat_ref_read";
pub(crate) const HELPER_WRITE: &str = "__compat_ref_write";
pub(crate) const HELPER_LENGTH: &str = "__compat_ref_length";
pub(crate) const HELPER_HAS: &str = "__compat_ref_has";
pub(crate) const HELPER_SNAPSHOT_VALUES: &str = "__compat_ref_snapshot_values";
pub(crate) const HELPER_RESIZE: &str = "__compat_ref_resize";
pub(crate) const HELPER_CLOSE: &str = "__compat_ref_close";
pub(crate) const HELPER_COLLECT: &str = "__compat_ref_collect";
pub(crate) const HELPER_RAND: &str = "__compat_ref_rand";
pub(crate) const HELPER_LT: &str = "__compat_ref_lt";
pub(crate) const HELPER_GT: &str = "__compat_ref_gt";
pub(crate) const HELPER_LT2: &str = "__compat_ref_lt2";
pub(crate) const HELPER_GT2: &str = "__compat_ref_gt2";

/// Randomness helper called by the `Math.random` lowering.
pub(crate) const HELPER_RANDOM_ALIAS: &str = "__compat_ref_random";

/// Appended to the user source in memory before parsing. The prelude is
/// fixed guest code lowered through the ordinary paths: real B1 imports,
/// guest type aliases, the owning factory (which registers through its
/// dispatcher before returning), and small typed helpers. Because it is
/// appended, every user span keeps its original offset.
pub(crate) const REFERENCE_ARRAY_PRELUDE: &str = r#"
import { allocate, register, collect, resize } from "__compat_array_owner";
import { read, write, length, has_index, snapshot, close } from "arrays";
import { random as __compat_ref_random } from "compat_math";
import type { Resource } from "rustscript:types";
interface __CompatRefResult {
    number: number;
    present: boolean;
    values: number[];
}
type __CompatRefDispatcher = (operation: number, arg: number, value: number) => __CompatRefResult;
interface __CompatRefCarrier {
    dispatcher: __CompatRefDispatcher;
}
function __compat_ref_make(initial: number[]): __CompatRefCarrier {
    let owner: Resource<"compat.number_array"> = allocate(initial);
    const dispatcher: __CompatRefDispatcher = (operation: number, arg: number, value: number): __CompatRefResult => operation === 0
        ? { number: register(owner), present: true, values: [] }
        : operation === 1
        ? { number: read(owner, arg), present: true, values: [] }
        : operation === 2
        ? { number: write(owner, arg, value), present: true, values: [] }
        : operation === 3
        ? { number: length(owner), present: true, values: [] }
        : operation === 4
        ? { number: 0, present: has_index(owner, arg), values: [] }
        : operation === 5
        ? { number: resize(owner, arg), present: true, values: [] }
        : operation === 6
        ? { number: 0, present: true, values: snapshot(owner) }
        : operation === 7
        ? { number: close(owner), present: true, values: [] }
        : operation === 9
        ? { number: 0, present: has_index(owner, arg) && read(owner, arg) < value, values: [] }
        : operation === 10
        ? { number: 0, present: has_index(owner, arg) && read(owner, arg) > value, values: [] }
        : operation === 11
        ? { number: 0, present: has_index(owner, arg) && has_index(owner, value) && read(owner, arg) < read(owner, value), values: [] }
        : operation === 12
        ? { number: 0, present: has_index(owner, arg) && has_index(owner, value) && read(owner, arg) > read(owner, value), values: [] }
        : { number: collect(), present: true, values: [] };
    const registration = dispatcher(0, 0, 0);
    return { dispatcher: dispatcher };
}
function __compat_ref_read(c: __CompatRefCarrier, i: number): number {
    const d = c.dispatcher;
    return d(1, i, 0).number;
}
function __compat_ref_write(c: __CompatRefCarrier, i: number, v: number): number {
    const d = c.dispatcher;
    return d(2, i, v).number;
}
function __compat_ref_length(c: __CompatRefCarrier): number {
    const d = c.dispatcher;
    return d(3, 0, 0).number;
}
function __compat_ref_has(c: __CompatRefCarrier, i: number): boolean {
    const d = c.dispatcher;
    return d(4, i, 0).present;
}
function __compat_ref_snapshot_values(c: __CompatRefCarrier): number[] {
    const d = c.dispatcher;
    return d(6, 0, 0).values;
}
function __compat_ref_resize(c: __CompatRefCarrier, n: number): number {
    const d = c.dispatcher;
    return d(5, n, 0).number;
}
function __compat_ref_close(c: __CompatRefCarrier): number {
    const d = c.dispatcher;
    return d(7, 0, 0).number;
}
function __compat_ref_collect(): number {
    return collect();
}
function __compat_ref_rand(): number {
    return __compat_ref_random();
}
function __compat_ref_lt(c: __CompatRefCarrier, i: number, other: number): boolean {
    const d = c.dispatcher;
    return d(9, i, other).present;
}
function __compat_ref_gt(c: __CompatRefCarrier, i: number, other: number): boolean {
    const d = c.dispatcher;
    return d(10, i, other).present;
}
function __compat_ref_lt2(c: __CompatRefCarrier, i1: number, i2: number): boolean {
    const d = c.dispatcher;
    return d(11, i1, i2).present;
}
function __compat_ref_gt2(c: __CompatRefCarrier, i1: number, i2: number): boolean {
    const d = c.dispatcher;
    return d(12, i1, i2).present;
}
"#;

/// Logical `number[]`: exactly a Core array of numbers, nothing else.
/// Tuples, maps, optionals, unions and non-numeric element types never
/// qualify, so plain scalar, host-handle and structural-callable values
/// can never be mistaken for a source array.
pub(crate) fn is_logical_number_array(schema: TypeSchema) -> bool {
    matches!(schema, TypeSchema::Array(inner) if matches!(inner.as_ref(), TypeSchema::Number))
}

/// Physical carrier schema for one reference array: the private hygienic
/// guest named record holding the owning dispatcher.
pub(crate) fn carrier_schema() -> TypeSchema {
    TypeSchema::Named(TYPE_CARRIER.to_owned(), Vec::new())
}

/// True for the exact physical carrier schema, the only provenance a
/// reference-array binding can carry. Callable shapes, global slots and
/// names never qualify.
///
/// Cross-module decls arrive with Core-renamed nominals
/// (`__guest_type_*`, one rename namespace per module); those match here
/// too so linked aliases keep working. A renamed user nominal can only
/// reach a generated helper call, where Core still checks it against the
/// real carrier parameter and fails closed with a precise mismatch: the
/// name never grants B1 owner authority by itself.
pub(crate) fn is_carrier_schema(schema: TypeSchema) -> bool {
    match schema {
        TypeSchema::Named(name, args) if args.is_empty() => {
            name == TYPE_CARRIER || name.starts_with("__guest_type_")
        }
        _ => false,
    }
}

/// Exact effective catalog for reference-array compilation and execution:
/// the B1 owned module, the unchanged manual arrays module, the typed
/// snapshot boundary and the scalar-math randomness source. Caller
/// catalogs must be complete and exact; missing or conflicting required
/// entries reject instead of silently replacing schemas.
pub(crate) fn reference_array_effective_catalog() -> Arc<vm::HostApiCatalog> {
    let arrays = crate::runtime::array::typescript_array_host_module()
        .catalog()
        .expect("production arrays catalog should build");
    let owned = crate::runtime::array::typescript_owned_array_host_module()
        .catalog()
        .expect("production owned-array catalog should build");
    let snapshot = crate::runtime::array::typescript_array_snapshot_host_module()
        .catalog()
        .expect("snapshot boundary catalog should build");
    let math = crate::runtime::scalar_math::typescript_scalar_math_host_module()
        .catalog()
        .expect("scalar-math catalog should build");
    let mut builder = vm::HostApiCatalog::builder();
    for resource in arrays.resources() {
        builder.resource(resource.clone());
    }
    for function in arrays
        .functions()
        .iter()
        .chain(owned.functions())
        .chain(snapshot.functions())
        .chain(math.functions())
    {
        builder.function(function.clone());
    }
    Arc::new(
        builder
            .build()
            .expect("reference-array effective catalog should build"),
    )
}
