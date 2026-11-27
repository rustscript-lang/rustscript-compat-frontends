//! Private callee-owned default-callable representation.
//!
//! For reference-array mode only. Each top-level function with defaults
//! gets thin callee-defined arity thunks (one per accepted supplied arity)
//! plus a private hygienic carrier struct holding truthful callable fields
//! for the full arity and each accepted arity, and a real zero-argument
//! getter returning the carrier. Thunks evaluate only omitted defaults
//! left-to-right in the callee scope then invoke one full implementation;
//! the body is never duplicated. Ordinary mode never consults this module.

/// Hygienic prefixes. Documented as reserved in reference-array mode; a
/// user declaration colliding with one fails precisely as a duplicate.
pub(crate) const THUNK_PREFIX: &str = "__compat_default_thunk_";
pub(crate) const GETTER_PREFIX: &str = "__compat_default_get_";
pub(crate) const CARRIER_PREFIX: &str = "__CompatDefaultCarrier_";
/// Carrier field holding the full-arity callable.
pub(crate) const FIELD_FULL: &str = "full";

/// Field name for an accepted supplied arity.
pub(crate) fn arity_field(arity: u8) -> String {
    format!("arity{arity}")
}

/// Thunk function name for `func` with `supplied` provided arguments.
pub(crate) fn thunk_name(func: &str, supplied: u8) -> String {
    format!("{THUNK_PREFIX}{func}_{supplied}")
}

/// Zero-argument getter name for `func`.
pub(crate) fn getter_name(func: &str) -> String {
    format!("{GETTER_PREFIX}{func}")
}

/// Carrier type name for `func`.
pub(crate) fn carrier_name(func: &str) -> String {
    format!("{CARRIER_PREFIX}{func}")
}

/// True for a synthetic default-callable name (thunk/getter/carrier).
/// Used only to recognise our own generated declarations, never to grant
/// authority to user shapes.
pub(crate) fn is_synthetic_default_name(name: &str) -> bool {
    name.starts_with(THUNK_PREFIX)
        || name.starts_with(GETTER_PREFIX)
        || name.starts_with(CARRIER_PREFIX)
}
