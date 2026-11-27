//! Original ordinary intrinsics, anchored in the existing bottom root frame.
use super::super::lowerer::uri_calls;
use super::super::{
    heap as layout,
    lowerer::{Lowerer, assert_stmt},
    representation as value,
};
use super::object::*;
use vm::{Expr as E, FrontendIr, ParseError, Stmt as S, TypeSchema as T};

pub(crate) const ROOT_NAME: &str = "__compat_es5_intrinsic_root";
pub(crate) const ALLOC_NAME: &str = "__compat_es5_object_allocate";
pub(crate) const BOOTSTRAP_NAME: &str = "__compat_es5_intrinsic_bootstrap";
pub(crate) const TYPE_ERROR_NAME: &str = "__compat_es5_new_type_error";
pub(crate) const OBJECT_CODE: i64 = 257;
pub(crate) const FIRST_METHOD_CODE: i64 = 258;
/// `Function.prototype` itself is a function node with this fixed code. It is
/// deliberately outside both callable ranges: sources own `0..=255` and every
/// other intrinsic starts at `OBJECT_CODE`. Dispatch reserves this code as a
/// non-callable poison value, so `call`/`apply` must refuse it before any
/// forwarding or code-cell read is classified as a source id.
pub(crate) const FUNCTION_PROTOTYPE_CODE: i64 = 256;
/// Every other intrinsic function node carries one of the fixed codes in this
/// module, and all of them are at least `OBJECT_CODE`. A closed source
/// function instead carries its zero-based dispatch id (`0..function_count`,
/// i.e. `0..=255`) in the same code cell. The exclusive boundary lets the
/// `call`/`apply` adapters tell a source function — whose whole argument
/// vector may be forwarded — from an intrinsic method node, whose extra
/// arguments must stay ignored unless an explicit carve-out admits them.
/// `Function.prototype`'s 256 sits exactly on the source side of the old
/// `< OBJECT_CODE` test, which is why the boundary is `FUNCTION_PROTOTYPE_CODE`
/// and never `OBJECT_CODE`. The realm cell budget cannot reach 256 source
/// functions, so the two ranges never meet.
pub(crate) const SOURCE_FUNCTION_CODE_LIMIT: i64 = FUNCTION_PROTOTYPE_CODE;
pub(crate) const METHODS: [(&str, i64); 9] = [
    ("create", 2),
    ("defineProperty", 3),
    ("defineProperties", 2),
    ("getOwnPropertyDescriptor", 2),
    ("getPrototypeOf", 1),
    ("preventExtensions", 1),
    ("isExtensible", 1),
    ("setPrototypeOf", 2),
    ("keys", 1),
];
/// Stable dispatch code for the appended method. Earlier method codes stay put.
pub(crate) const SET_PROTOTYPE_OF_CODE: i64 = FIRST_METHOD_CODE + 7;
/// `Object.keys` keeps the next stable method code after `setPrototypeOf`.
pub(crate) const KEYS_CODE: i64 = FIRST_METHOD_CODE + 8;
/// `Function.prototype.call` is the first function-prototype method. Its code
/// sits right after `hasOwnProperty` and never overlaps a static, Math,
/// numeric-global, or URI code.
pub(crate) const FUNCTION_CALL_CODE: i64 = KEYS_CODE + 2;
/// `Function.prototype.apply` is the second function-prototype method. Code
/// 269 sits right after `call` and, like it, never overlaps a static
/// (257..267), Math (512..536), numeric-global, or URI (536..540) code.
pub(crate) const FUNCTION_APPLY_CODE: i64 = KEYS_CODE + 3;
/// `Function.prototype.bind` is the third function-prototype method. Code
/// 270 sits right after `apply` and, like `call` and `apply`, never overlaps
/// a static (257..267), Math (512..536), numeric-global (530..533), or URI
/// (536..540) code.
pub(crate) const FUNCTION_BIND_CODE: i64 = KEYS_CODE + 4;
/// Private code of the `Function.prototype.bind` method node itself. Code 270
/// stays the identity of the bound product because dispatch must route a
/// bound call back into the bind adapter; the method takes the next free code
/// after the gated `toString` so the two shapes never share an identity. The
/// code cell is written only by the allocator and is never projected to an
/// own property row, so adding/deleting rows or rewriting the prototype pair
/// cannot change it.
pub(crate) const FUNCTION_BIND_METHOD_CODE: i64 = FUNCTION_TO_STRING_CODE + 1;
/// `Function.prototype.name`'s private accessor getter. Code 352 follows the
/// last occupied primitive code (351, `replaceAll`) and stays clear of the
/// static 257..267, String 275..327, typed-array 328..345, error 346..350,
/// Math 512..546, numeric-global 530..533 and URI 536..539 windows. The
/// function node is reachable only through the Function.prototype accessor.
pub(crate) const FUNCTION_NAME_NAME: &str = "name";
pub(crate) const FUNCTION_NAME_GET_CODE: i64 = STRING_REPLACE_ALL_CODE + 1;
/// `Function.prototype.toString` is the fourth Function-prototype method and
/// the fifth data row. Code 353 follows the private name getter.
pub(crate) const FUNCTION_TO_STRING_NAME: &str = "toString";
pub(crate) const FUNCTION_TO_STRING_CODE: i64 = FUNCTION_NAME_GET_CODE + 1;
/// Every source function's `prototype` is one inherited accessor on
/// `Function.prototype`, never a per-function creation-time row. Code 357
/// (getter) and 358 (setter) follow the last occupied primitive code (356,
/// `String.prototype.match`) and stay clear of every static, String,
/// typed-array, error, Math, numeric-global and URI window. Both private
/// function nodes are reachable only through that accessor row.
pub(crate) const FUNCTION_PROTOTYPE_NAME: &str = "prototype";
/// The `constructor` data row on the private builtin prototypes and on the
/// Array method table. A read of this name selects the same gated row family
/// as `.prototype`: `(1).constructor` must work on its own, without a prior
/// `.prototype` touch opening the accessor gate.
pub(crate) const CONSTRUCTOR_NAME: &str = "constructor";
pub(crate) const FUNCTION_PROTOTYPE_GET_CODE: i64 = STRING_MATCH_CODE + 1;
pub(crate) const FUNCTION_PROTOTYPE_SET_CODE: i64 = FUNCTION_PROTOTYPE_GET_CODE + 1;
/// `JSON.stringify` is the one own method of the optional `JSON` global
/// object. Code 271 follows the three function-prototype methods and, like
/// them, never overlaps a static (257..267), Math (512..546), numeric-global
/// or URI (536..539) code.
pub(crate) const JSON_STRINGIFY_CODE: i64 = FUNCTION_BIND_CODE + 1;
/// `JSON.parse` is the second own data method of the same optional `JSON`
/// object. Code 272 follows `stringify` and, like it, never overlaps a static
/// (257..267), Math (512..546), numeric-global or URI (536..539) code.
pub(crate) const JSON_PARSE_CODE: i64 = JSON_STRINGIFY_CODE + 1;
/// `Date.prototype.getTime` on the one admitted `new Date(0)` value. Code 273
/// follows `JSON.parse` and, like it, never overlaps a static (257..267), Math
/// (512..546), numeric-global or URI (536..539) code. `Date` itself is not a
/// global binding and no other constructor or method is admitted, so this code
/// is reachable only through the function node the construction publishes.
pub(crate) const DATE_GETTIME_CODE: i64 = JSON_PARSE_CODE + 1;
/// The one admitted public `RegExp.prototype.test`-shaped adapter for the
/// admitted `/a/` literal. Code 274 follows `Date.prototype.getTime` and, like
/// it, never overlaps a static (257..267), Math (512..546), numeric-global or
/// URI (536..539) code. The code is reachable only through the function node
/// the admitted literal publishes, so a borrowed or ordinary object receiver
/// keeps the existing TypeError.
pub(crate) const REGEXP_TEST_CODE: i64 = DATE_GETTIME_CODE + 1;
/// `String.prototype.charCodeAt` is the first real String-prototype method.
/// Code 275 follows the admitted `/a/.test` adapter and never overlaps a
/// static (257..267), Math (512..546), numeric-global or URI (536..539) code.
pub(crate) const STRING_CHAR_CODE_AT_NAME: &str = "charCodeAt";
pub(crate) const STRING_CHAR_CODE_AT_CODE: i64 = REGEXP_TEST_CODE + 1;
/// `String.prototype.charAt` shares the charCodeAt adapter. Code 276 follows
/// 275 and never overlaps a static (257..267), Math (512..546), numeric-global
/// or URI (536..539) code. The name is six UTF-16 units.
pub(crate) const STRING_CHAR_AT_NAME: &str = "charAt";
pub(crate) const STRING_CHAR_AT_CODE: i64 = STRING_CHAR_CODE_AT_CODE + 1;
/// `String.prototype.codePointAt` shares the charCodeAt adapter. Code 277
/// follows 276 and never overlaps a static (257..267), Math (512..546),
/// numeric-global or URI (536..539) code. The name is eleven UTF-16 units.
pub(crate) const STRING_CODE_POINT_AT_NAME: &str = "codePointAt";
pub(crate) const STRING_CODE_POINT_AT_CODE: i64 = STRING_CHAR_AT_CODE + 1;
/// `String.prototype.concat` shares the charCodeAt adapter. Code 278 follows
/// 277 and never overlaps a static (257..267), Math (512..546), numeric-global
/// or URI (536..539) code. The name is six UTF-16 units.
pub(crate) const STRING_CONCAT_NAME: &str = "concat";
pub(crate) const STRING_CONCAT_CODE: i64 = STRING_CODE_POINT_AT_CODE + 1;
/// `String.prototype.substring` shares the charCodeAt adapter. Code 279 follows
/// 278 and never overlaps a static (257..267), Math (512..546), numeric-global
/// or URI (536..539) code. The name is nine UTF-16 units.
pub(crate) const STRING_SUBSTRING_NAME: &str = "substring";
pub(crate) const STRING_SUBSTRING_CODE: i64 = STRING_CONCAT_CODE + 1;
/// `String.prototype.slice` shares the substring range adapter. Code 280
/// follows 279 and never overlaps a static (257..267), Math (512..546),
/// numeric-global or URI (536..539) code. The name is five UTF-16 units.
pub(crate) const STRING_SLICE_NAME: &str = "slice";
pub(crate) const STRING_SLICE_CODE: i64 = STRING_SUBSTRING_CODE + 1;
/// `String.prototype.indexOf` shares the charCodeAt adapter. Code 281 follows
/// 280 and never overlaps a static (257..267), Math (512..546), numeric-global
/// or URI (536..539) code. The public length is 1; the private arity is 2.
/// The name is seven UTF-16 units.
pub(crate) const STRING_INDEXOF_NAME: &str = "indexOf";
pub(crate) const STRING_INDEXOF_CODE: i64 = STRING_SLICE_CODE + 1;
/// `String.prototype.lastIndexOf` shares the charCodeAt adapter. Code 282
/// follows 281 and never overlaps a static (257..267), Math (512..546),
/// numeric-global or URI (536..539) code. The public length is 1; the private
/// arity is 2. The name is eleven UTF-16 units.
pub(crate) const STRING_LAST_INDEX_OF_NAME: &str = "lastIndexOf";
pub(crate) const STRING_LAST_INDEX_OF_CODE: i64 = STRING_INDEXOF_CODE + 1;
/// `String.prototype.toString` shares the charCodeAt adapter. Code 283
/// follows 282 and never overlaps a static (257..267), Math (512..546),
/// numeric-global or URI (536..539) code. The public length is 0; the private
/// arity is 0. The name is eight UTF-16 units and is allocated on its own,
/// not shared with `Object.prototype.toString`.
pub(crate) const STRING_TO_STRING_NAME: &str = "toString";
pub(crate) const STRING_TO_STRING_CODE: i64 = STRING_LAST_INDEX_OF_CODE + 1;
/// `String.prototype.toLowerCase` shares the charCodeAt adapter. Code 284
/// follows 283 and never overlaps a static (257..267), Math (512..546),
/// numeric-global or URI (536..539) code. The public length is 0; the private
/// arity is 0. The name is eleven UTF-16 units. Mapping is the MicroQuickJS
/// ASCII rule, not a full ES5 Unicode case conversion.
pub(crate) const STRING_TO_LOWER_CASE_NAME: &str = "toLowerCase";
pub(crate) const STRING_TO_LOWER_CASE_CODE: i64 = STRING_TO_STRING_CODE + 1;
/// `String.prototype.toUpperCase` shares the charCodeAt adapter. Code 285
/// follows 284 and never overlaps a static (257..267), Math (512..546),
/// numeric-global or URI (536..539) code. The public length is 0; the private
/// arity is 0. The name is eleven UTF-16 units. Mapping is the same ASCII
/// rule in the other direction.
pub(crate) const STRING_TO_UPPER_CASE_NAME: &str = "toUpperCase";
pub(crate) const STRING_TO_UPPER_CASE_CODE: i64 = STRING_TO_LOWER_CASE_CODE + 1;
/// `Error.prototype.toString` is the realm's only Error-prototype method.
/// Code 286 fills the first slot left free by the String case-mapping pair
/// (codes 284/285) and keeps 287/288 free. Public length 0, private arity 0;
/// the function is installed on the reserved row 2 of the four-row realm
/// Error prototype and reuses the shared `toString` key String.
pub(crate) const ERROR_TO_STRING_CODE: i64 = STRING_TO_UPPER_CASE_CODE + 1;
/// Three MicroQuickJS UTF-16 whitespace methods use the String adapter.
/// Codes 289..291 remain outside static, Math, URI and numeric-global codes.
pub(crate) const STRING_TRIM_NAME: &str = "trim";
pub(crate) const STRING_TRIM_CODE: i64 = STRING_TO_UPPER_CASE_CODE + 4;
pub(crate) const STRING_TRIM_END_NAME: &str = "trimEnd";
pub(crate) const STRING_TRIM_END_CODE: i64 = STRING_TRIM_CODE + 1;
pub(crate) const STRING_TRIM_START_NAME: &str = "trimStart";
pub(crate) const STRING_TRIM_START_CODE: i64 = STRING_TRIM_END_CODE + 1;
/// ES5.1 string-search/string-replacement first-match subset, code 292.
pub(crate) const STRING_REPLACE_NAME: &str = "replace";
pub(crate) const STRING_REPLACE_CODE: i64 = STRING_TRIM_START_CODE + 1;
/// `String.prototype.replaceAll`: the all-matches sibling of `replace`.
/// Code 351 follows the last occupied primitive code (typed arrays end at
/// 345 and the public error constructors at 350) and stays clear of the
/// static 257..267, String 275..327, typed-array 328..345, error 346..350,
/// Math 512..546, numeric-global 530..533 and URI 536..539 windows.
/// Public length 2, private arity 2, one row on the private String prototype.
pub(crate) const STRING_REPLACE_ALL_NAME: &str = "replaceAll";
pub(crate) const STRING_REPLACE_ALL_CODE: i64 = 351;
/// `String.prototype.search` in the one admitted `/a/` RegExp domain. Code
/// 355 follows the last occupied primitive code (354, the private
/// `Function.prototype.bind` method) and stays clear of the reserved
/// 286..288/302..303 windows, the static 257..267, String 275..327,
/// typed-array 328..345, error 346..350, Math 512..546, numeric-global
/// 530..533 and URI 536..539 windows. Public length 1, private arity 1, one
/// gated row on the private String prototype.
pub(crate) const STRING_SEARCH_NAME: &str = "search";
pub(crate) const STRING_SEARCH_CODE: i64 = 355;
/// `String.prototype.match` in the same one admitted `/a/` RegExp domain as
/// `search`. Code 356 follows the last occupied primitive code (355) and
/// stays clear of the reserved 286..288/302..303 windows, the static
/// 257..267, String 275..327, typed-array 328..345, error 346..350, Math
/// 512..546, numeric-global 530..533 and URI 536..539 windows. Public length
/// 1, private arity 1, one gated row on the private String prototype.
pub(crate) const STRING_MATCH_NAME: &str = "match";
pub(crate) const STRING_MATCH_CODE: i64 = 356;
pub(crate) const STRING_SPLIT_NAME: &str = "split";
pub(crate) const STRING_SPLIT_CODE: i64 = STRING_REPLACE_CODE + 1;
/// `String.prototype.repeat`. Code 309 stays outside the Number-format
/// reservation 300..=303 and does not move 299/304/305/306/307/308.
pub(crate) const STRING_REPEAT_NAME: &str = "repeat";
pub(crate) const STRING_REPEAT_CODE: i64 = 309;
/// `String.fromCharCode` / `String.fromCodePoint`. Codes 310/311 stay outside
/// the Number-format reservation 300..=303 and do not move 299/304/305/306/307/308/309.
/// The function-shaped carrier uses 312 so a direct call rejects on the same path as Number.
pub(crate) const STRING_FROM_CHAR_CODE_NAME: &str = "fromCharCode";
pub(crate) const STRING_FROM_CHAR_CODE_CODE: i64 = 310;
pub(crate) const STRING_FROM_CODE_POINT_NAME: &str = "fromCodePoint";
pub(crate) const STRING_FROM_CODE_POINT_CODE: i64 = 311;
pub(crate) const STRING_NAMESPACE_CODE: i64 = 312;
/// `Array.prototype.slice`. Code 313 stays outside 299 and 300..312.
/// The private result array is not a constructor and has no code.
pub(crate) const ARRAY_SLICE_CODE: i64 = 313;
/// `Array.prototype.concat`. Code 314 stays outside 299 and 300..313.
/// The private result array is not a constructor and has no code.
pub(crate) const ARRAY_CONCAT_CODE: i64 = 314;
/// Function-shaped `Array` carrier and its one static method.
pub(crate) const ARRAY_NAMESPACE_CODE: i64 = 315;
pub(crate) const ARRAY_IS_ARRAY_CODE: i64 = 316;
const ARRAY_IS_ARRAY_NAME: &str = "isArray";
pub(crate) const ARRAY_EVERY_CODE: i64 = 317;
pub(crate) const ARRAY_SOME_CODE: i64 = 318;
pub(crate) const ARRAY_FOR_EACH_CODE: i64 = 319;
pub(crate) const ARRAY_MAP_CODE: i64 = 320;
pub(crate) const ARRAY_FILTER_CODE: i64 = 321;
pub(crate) const ARRAY_REDUCE_CODE: i64 = 322;
pub(crate) const ARRAY_REDUCE_RIGHT_CODE: i64 = 323;
pub(crate) const ARRAY_JOIN_CODE: i64 = 324;
pub(crate) const ARRAY_TO_STRING_CODE: i64 = 325;
pub(crate) const ARRAY_SPLICE_CODE: i64 = 326;
pub(crate) const ARRAY_SORT_CODE: i64 = 327;
const ARRAY_PROTOTYPE_METHODS: [(&str, i64, i64); 20] = [
    ("push", ARRAY_PUSH_CODE, 1),
    ("pop", ARRAY_POP_CODE, 0),
    ("shift", ARRAY_SHIFT_CODE, 0),
    ("unshift", ARRAY_UNSHIFT_CODE, 1),
    ("reverse", ARRAY_REVERSE_CODE, 0),
    ("indexOf", ARRAY_INDEX_OF_CODE, 1),
    ("lastIndexOf", ARRAY_LAST_INDEX_OF_CODE, 1),
    ("slice", ARRAY_SLICE_CODE, 2),
    ("concat", ARRAY_CONCAT_CODE, 1),
    ("every", ARRAY_EVERY_CODE, 1),
    ("some", ARRAY_SOME_CODE, 1),
    ("forEach", ARRAY_FOR_EACH_CODE, 1),
    ("map", ARRAY_MAP_CODE, 1),
    ("filter", ARRAY_FILTER_CODE, 1),
    ("reduce", ARRAY_REDUCE_CODE, 1),
    ("reduceRight", ARRAY_REDUCE_RIGHT_CODE, 1),
    ("join", ARRAY_JOIN_CODE, 1),
    ("toString", ARRAY_TO_STRING_CODE, 0),
    ("splice", ARRAY_SPLICE_CODE, 2),
    ("sort", ARRAY_SORT_CODE, 1),
];
const STRING_NAMESPACE_ROWS: i64 = 4;
const ARRAY_NAMESPACE_ROWS: i64 = 3;
/// Public zero-argument TypeError function; the private instance allocator has no code.
pub(crate) const ARRAY_PUSH_CODE: i64 = STRING_SPLIT_CODE + 1;
pub(crate) const ARRAY_POP_CODE: i64 = ARRAY_PUSH_CODE + 1;
pub(crate) const TYPE_ERROR_CODE: i64 = ARRAY_POP_CODE + 1;
pub(crate) const ARRAY_SHIFT_CODE: i64 = TYPE_ERROR_CODE + 1;
pub(crate) const ARRAY_UNSHIFT_CODE: i64 = ARRAY_SHIFT_CODE + 1;
/// `Array.prototype.reverse`. Code 304 is outside the Number formatting
/// reservation 300..303 and is not chained through the Number carrier at 299.
pub(crate) const ARRAY_REVERSE_CODE: i64 = 304;
/// Ordinary `Boolean(...)`. Code 305 stays outside 299 and 300..304.
pub(crate) const BOOLEAN_CODE: i64 = 305;
/// `Array.prototype.indexOf`. Code 306 stays outside 299 and 300..305.
pub(crate) const ARRAY_INDEX_OF_CODE: i64 = 306;
/// `Array.prototype.lastIndexOf`. Code 307 stays outside 299 and 300..306.
pub(crate) const ARRAY_LAST_INDEX_OF_CODE: i64 = 307;
/// Public zero-argument `RangeError`. Code 308 stays outside 299 and 300..307.
/// The private allocator has no code. This is the TypeError lease shape, not
/// Micro `ERROR_DEF`: the prototype holds `name` and `message=""`, and the
/// instance has no own message.
pub(crate) const RANGE_ERROR_CODE: i64 = 308;
pub(crate) const RANGE_ERROR_NAME: &str = "__compat_es5_new_range_error";

pub(crate) const EVAL_ERROR_CODE: i64 = 346;
pub(crate) const REFERENCE_ERROR_CODE: i64 = 347;
pub(crate) const SYNTAX_ERROR_CODE: i64 = 348;
pub(crate) const INTERNAL_ERROR_CODE: i64 = 349;
pub(crate) const URI_ERROR_CODE: i64 = 350;

/// Public error constructors whose bootstrap is selected independently.
#[derive(Clone, Copy, Debug)]
pub(crate) enum OptionalError {
    Eval,
    Reference,
    Syntax,
    Internal,
    Uri,
}

impl OptionalError {
    pub(crate) const ALL: [Self; 5] = [
        Self::Eval,
        Self::Reference,
        Self::Syntax,
        Self::Internal,
        Self::Uri,
    ];

    pub(crate) fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|error| error.name() == name)
    }

    fn name(self) -> &'static str {
        match self {
            Self::Eval => "EvalError",
            Self::Reference => "ReferenceError",
            Self::Syntax => "SyntaxError",
            Self::Internal => "InternalError",
            Self::Uri => "URIError",
        }
    }

    pub(crate) fn code(self) -> i64 {
        match self {
            Self::Eval => EVAL_ERROR_CODE,
            Self::Reference => REFERENCE_ERROR_CODE,
            Self::Syntax => SYNTAX_ERROR_CODE,
            Self::Internal => INTERNAL_ERROR_CODE,
            Self::Uri => URI_ERROR_CODE,
        }
    }

    pub(crate) fn bit(self) -> u8 {
        1 << self as u8
    }

    pub(crate) fn is_selected(self, lower: &Lowerer<'_>) -> bool {
        lower.optional_errors_used.get() & self.bit() != 0
    }

    pub(crate) fn selected(lower: &Lowerer<'_>) -> impl Iterator<Item = Self> + use<> {
        let mask = lower.optional_errors_used.get();
        Self::ALL.into_iter().filter(move |error| mask & error.bit() != 0)
    }

    pub(crate) fn constructor_root(self, lower: &Lowerer<'_>) -> i64 {
        typed_array_root_end(lower)
            + i64::from((lower.optional_errors_used.get() & (self.bit() - 1)).count_ones())
    }

    fn allocator_name(self) -> &'static str {
        match self {
            Self::Eval => "__compat_es5_new_eval_error",
            Self::Reference => "__compat_es5_new_reference_error",
            Self::Syntax => "__compat_es5_new_syntax_error",
            Self::Internal => "__compat_es5_new_internal_error",
            Self::Uri => "__compat_es5_new_public_uri_error",
        }
    }

    pub(crate) fn allocator(self, intrinsic: Intrinsics) -> Option<u16> {
        intrinsic.optional_errors[self as usize]
    }

    pub(crate) fn bootstrap_request(self, lower: &Lowerer<'_>) -> (u64, u64) {
        if matches!(self, Self::Uri) {
            // The URI adapter manifest owns the shared prototype when at least
            // one adapter is selected. The public constructor always adds one Function.
            let (cells, nodes) = if lower.uri_used.get() == 0 {
                uri_calls::PROTOTYPE_REQUEST
            } else {
                (0, 0)
            };
            (RANGE_ERROR_FUNCTION_CELLS + cells, 1 + nodes)
        } else {
            (
                RANGE_ERROR_FUNCTION_CELLS + RANGE_ERROR_PROTO_CELLS + 6 + self.name().len() as u64,
                RANGE_ERROR_EXTRA_NODES,
            )
        }
    }
}
/// Function node: header 3 + function payload 10 + two property rows.
pub(crate) const RANGE_ERROR_FUNCTION_CELLS: u64 = 3 + 10 + 14;
/// Ordinary error prototype: header 3 + object payload 5 + two property rows.
pub(crate) const RANGE_ERROR_PROTO_CELLS: u64 = 3 + 5 + 14;
/// Value string `"RangeError"` (10 UTF-16 units). Not a new key.
pub(crate) const RANGE_ERROR_NAME_CELLS: u64 = 6 + 10;
pub(crate) const RANGE_ERROR_EXTRA_CELLS: u64 =
    RANGE_ERROR_FUNCTION_CELLS + RANGE_ERROR_PROTO_CELLS + RANGE_ERROR_NAME_CELLS;
pub(crate) const RANGE_ERROR_EXTRA_NODES: u64 = 3;
/// Unused slot of the existing eight-entry bootstrap stage frame. Strings
/// publish into entries 0 and 1. The prototype stays here only until the
/// function's `prototype` data row is published; that row, not a second
/// permanent root, is what the collector traces.
pub(crate) const RANGE_ERROR_STAGE_ENTRY: i64 = 7;
/// Function node: header 3 + function payload 10 + two property rows
/// (`length` and the carrier's bootstrap `prototype` row). The shared
/// `length` key and the `prototype` key are already charged elsewhere.
pub(crate) const BOOLEAN_FUNCTION_CELLS: u64 = 3 + 10 + 2 * 7;
/// Function-shaped carrier for the Number constant namespace, with no call semantics.
/// `parseInt`/`parseFloat` are writable properties on this carrier, sharing codes
/// 530/531 with the bare globals. Calling `Number` itself still rejects.
pub(crate) const NUMBER_NAMESPACE_CODE: i64 = ARRAY_UNSHIFT_CODE + 1;
/// `Number.prototype.toString` (ES5.1 15.7.4.2). Code 300 fills the first slot
/// of the reserved 300..=303 Number-format window and never overlaps a static
/// (257..267), Math (512..546), numeric-global (530..533) or URI (536..540)
/// code. Public length 0, private arity 0, one property row on the private
/// Number prototype, which is only reachable through a primitive Number
/// receiver.
pub(crate) const NUMBER_TO_STRING_NAME: &str = "toString";
pub(crate) const NUMBER_TO_STRING_CODE: i64 = NUMBER_NAMESPACE_CODE + 1;
const NUMBER_CONSTANTS: [(&str, f64); 8] = [
    ("MAX_VALUE", f64::MAX),
    ("MIN_VALUE", f64::from_bits(1)),
    ("NaN", f64::NAN),
    ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
    ("POSITIVE_INFINITY", f64::INFINITY),
    ("EPSILON", f64::EPSILON),
    ("MAX_SAFE_INTEGER", 9_007_199_254_740_991.0),
    ("MIN_SAFE_INTEGER", -9_007_199_254_740_991.0),
];
/// length, eight constants, then parseInt and parseFloat, then the carrier's
/// own `prototype` data row.
const NUMBER_PARSE_METHODS: usize = 2;
const NUMBER_NAMESPACE_ROWS: i64 =
    1 + NUMBER_CONSTANTS.len() as i64 + NUMBER_PARSE_METHODS as i64 + 1;
/// Stage-frame entry that roots a minimal primitive carrier while bootstrap
/// publishes it. Slots 0..3 are taken by the key Strings and the numeric
/// method nodes, and the minimal carrier is only published into a permanent
/// prototype row, so it never needs a permanent root entry of its own.
const PRIMITIVE_CARRIER_STAGE_SLOT: i64 = 7;
/// The optional `JSON` object owns the permanent root entry immediately after
/// the three private primitive prototypes, so no earlier root index moves and
/// a source that never names `JSON` keeps the exact previous frame layout.
pub(crate) fn json_root(lower: &Lowerer<'_>) -> i64 {
    boolean_prototype_root(lower) + 1
}
/// The public constructor follows optional JSON without moving existing roots.
pub(crate) fn type_error_constructor_root(lower: &Lowerer<'_>) -> i64 {
    json_root(lower) + i64::from(lower.json_used.get())
}
/// Cells and allocation sites of the optional `JSON` object manifest: one
/// two-row object node (3 + 5 + 14), two one-row function nodes
/// (2 * (3 + 10 + 7)) and the five- and nine-unit `parse`/`stringify` key
/// Strings (6 + 5 + 6 + 9). It is charged only when the source names `JSON`,
/// exactly like the numeric and URI manifests.
pub(crate) fn json_request_cells() -> u64 {
    22 + 40 + 11 + 15
}
pub(crate) const JSON_REQUEST_NODES: u64 = 5;
/// The prototype-method key converter publishes its four roots into entries
/// 2..5 of the receiver-aware adapter's eight-entry frame: case 2 keeps the
/// coerced primitive, 3 the method-name key and payload, 4 the candidate get
/// and payload, and 5 the produced String or the throw payload.
pub(crate) const KEY_ENTRY: i64 = 2;
/// `Object.prototype.hasOwnProperty` is the first real prototype method.
/// It is installed on the prototype object, never as a static on `Object`.
pub(crate) const HAS_OWN_PROPERTY_NAME: &str = "hasOwnProperty";
pub(crate) const HAS_OWN_PROPERTY_CODE: i64 = KEYS_CODE + 1;
/// Ordinary object prototype rows: constructor, valueOf, toString,
/// hasOwnProperty. The two default conversion methods are always present so
/// the key coercion of the prototype method never depends on `Math` use.
pub(crate) const PROTO_ROWS: i64 = 4;
/// `Function.prototype.apply`'s allocator property-table capacity. `capacity`
/// is the fourth `allocate` argument and is only a row count; the function's
/// arity is the independent sixth argument and is never used as a row count.
/// The measured source value is two rows even though the node installs just
/// its own `length` row.
const FUNCTION_APPLY_CAPACITY: i64 = 2;
/// Allocator capacities of the six always-present prototype method functions,
/// in emitter installation order: Object.prototype `valueOf`, `toString`,
/// `hasOwnProperty`, then Function.prototype `call`, `apply`, `bind`. Both the
/// static bootstrap request accounting and the emitter below read this one
/// table, so an allocation capacity cannot leave the request budget stale.
const PROTO_METHOD_CAPACITIES: [i64; 6] = [1, 1, 1, 1, FUNCTION_APPLY_CAPACITY, 1];
/// Realm objects, method functions, eight fixed keys, one string per method.
/// Plus the three object-prototype methods' plus three function-prototype
/// methods' functions and names, the private
/// empty packed Array prototype node, the three private primitive prototype
/// objects that bridge `hasOwnProperty` for String/Number/Boolean, and the
/// String prototype's admitted `charCodeAt`, `charAt`, `codePointAt`,
/// `concat`, `substring`, `slice`, `indexOf`, `lastIndexOf`, `toString`,
/// `toLowerCase`, `toUpperCase`, `trim`, `trimEnd`, `trimStart`, `replace` and
/// `split` functions plus their key Strings.
/// Default-manifest node count used by the fixed budget witnesses.
#[allow(dead_code)]
pub(crate) fn bootstrap_node_count(
    number_namespace_used: bool,
    string_namespace_used: bool,
    array_namespace_used: bool,
    number_to_string_used: bool,
    string_replace_all_used: bool,
) -> u64 {
    bootstrap_node_count_for(
        number_namespace_used,
        string_namespace_used,
        array_namespace_used,
        number_to_string_used,
        string_replace_all_used,
        false,
        false,
        false,
    )
}

/// Extended manifest with the two `Function.prototype` gates. Keeping the
/// five-argument form above as the default manifest leaves every existing
/// budget witness unchanged; only a source that can spell `.name`/`.toString`
/// reaches the added nodes.
pub(crate) fn bootstrap_node_count_for(
    number_namespace_used: bool,
    string_namespace_used: bool,
    array_namespace_used: bool,
    number_to_string_used: bool,
    string_replace_all_used: bool,
    string_search_used: bool,
    function_names_used: bool,
    function_to_string_used: bool,
) -> u64 {
    bootstrap_node_count_gated(
        number_namespace_used,
        string_namespace_used,
        array_namespace_used,
        number_to_string_used,
        string_replace_all_used,
        string_search_used,
        false,
        function_names_used,
        function_to_string_used,
        false,
    )
}

/// Extended manifest with the gated `String.prototype.match` row. The
/// eight-argument form above keeps every existing budget witness unchanged;
/// only a source that can spell `.match` reaches the two added nodes (its
/// one-row function node and its five-unit key String).
#[allow(clippy::too_many_arguments)]
pub(crate) fn bootstrap_node_count_gated(
    number_namespace_used: bool,
    string_namespace_used: bool,
    array_namespace_used: bool,
    number_to_string_used: bool,
    string_replace_all_used: bool,
    string_search_used: bool,
    string_match_used: bool,
    function_names_used: bool,
    function_to_string_used: bool,
    primitive_constructor_read: bool,
) -> u64 {
    // The Number function, one String per constant, and the parseInt/parseFloat
    // keys. `length` reuses a fixed key. The function objects themselves are
    // charged with the numeric request when the bare global is absent.
    let number_namespace_nodes =
        1 + NUMBER_CONSTANTS.len() as u64 + NUMBER_PARSE_METHODS as u64;
    // Carrier, two method functions, two key Strings. `length` reuses a fixed key.
    let string_namespace_nodes = 1 + 2 + 2;
    // Carrier, one method function, one key String. `length` and `prototype` are fixed keys.
    let array_namespace_nodes = 1 + 1 + 1;
    // Array method table, plus a Function and key String per method.
    // +35: seventeen fixed String methods, each a function node and a key
    // String, plus the Error prototype's one-row `toString` function whose
    // key String is the shared `toString` fixed key. `replaceAll` adds its
    // own two nodes only when selected below.
    5 + METHODS.len() as u64 + 8 + METHODS.len() as u64 + 6 + 6 + 1 + 3 + 35 + 1 + (1 + ARRAY_PROTOTYPE_METHODS.len() as u64 * 2)
        + u64::from(number_namespace_used) * number_namespace_nodes
        + u64::from(string_namespace_used) * string_namespace_nodes
        // The minimal two-row primitive carriers a bare `.constructor` read
        // owns when the source never spells the global name: one function node
        // each, its `length` and `prototype` keys already charged above.
        + u64::from(primitive_constructor_read && !number_namespace_used)
        + u64::from(primitive_constructor_read && !string_namespace_used)
        + u64::from(array_namespace_used) * array_namespace_nodes
        // `Number.prototype.toString`: one function node. Its `toString` key
        // String is already among the fixed keys, so no string node is added.
        + u64::from(number_to_string_used)
        // `String.prototype.replaceAll`: one one-row function node and one
        // ten-unit key String, both gated on the source naming the method.
        + u64::from(string_replace_all_used) * 2
        // `String.prototype.search`: one one-row function node and one
        // six-unit key String, both gated on the source naming the method.
        + u64::from(string_search_used) * 2
        // `String.prototype.match`: one one-row function node and one
        // five-unit key String, both gated on the source naming the method.
        + u64::from(string_match_used) * 2
        // Function names: the private name accessor getter function node.
        // There is no separate key node: the accessor reuses the fixed `name`
        // key and the source function rows recover it from that accessor row.
        // The `bound` name String is allocated by the bind adapter at
        // creation time, so it is not part of the realm manifest.
        + u64::from(function_names_used)
        // `Function.prototype.toString`: one one-row function node whose
        // `toString` key is the already charged fixed String key.
        + u64::from(function_to_string_used)
}

#[cfg(test)]
thread_local! {
    static INTRINSIC_INIT_CELLS: std::cell::Cell<u64> = std::cell::Cell::new(0);
    static INTRINSIC_INIT_NODES: std::cell::Cell<u64> = std::cell::Cell::new(0);
}

#[cfg(test)]
pub(crate) fn record_intrinsic_budget(cells: u64, nodes: u64) {
    INTRINSIC_INIT_CELLS.with(|cell| cell.set(cells));
    INTRINSIC_INIT_NODES.with(|cell| cell.set(nodes));
}

#[cfg(test)]
pub(crate) fn intrinsic_budget() -> (u64, u64) {
    (
        INTRINSIC_INIT_CELLS.with(|cell| cell.get()),
        INTRINSIC_INIT_NODES.with(|cell| cell.get()),
    )
}
#[derive(Clone, Copy)]
pub(crate) struct Intrinsics {
    pub root: u16,
    pub allocate: u16,
    pub bootstrap: u16,
    pub type_error: u16,
    /// Private RangeError allocator, parameterized by reserved property rows.
    /// Array may need it even when the source does not name RangeError.
    pub range_error: Option<u16>,
    optional_errors: [Option<u16>; OptionalError::ALL.len()],
    pub length_key: u16,
    /// Private `name` key String producer, recovered from the Function
    /// prototype accessor row's key pair (never a second key node).
    pub function_name_key: Option<u16>,
    /// Shared empty String producer for anonymous functions, read from its
    /// own private permanent root (never from a guest-writable public
    /// property row). Declared only when a name gate is selected so the
    /// default helper index layout stays unchanged.
    pub function_name_empty: Option<u16>,
    /// Private combined `name` getter/`toString` adapter behind codes 352 and
    /// 353. Only a source that names either property pays it.
    pub function_name_api: Option<u16>,
    pub uri_error: Option<u16>,
    /// Private reader for the realm's empty packed Array prototype.
    pub array_proto: u16,
    pub array_buffer: Option<u16>,
    pub typed_array: Option<u16>,
}

/// A canonical literal primitive is `MapNew` plus the five known field sets.
/// Only that private record copy may be evaluated once. Existing variables,
/// `as_number` boxes, string/object tags, and any other call stay duplicated.
fn canonical_literal_primitive(payload: &E) -> bool {
    let map_new = vm::BuiltinFunction::MapNew.call_index();
    let set = vm::BuiltinFunction::Set.call_index();
    let Some((inner, generation)) = literal_field(payload, "generation", set) else {
        return false;
    };
    let Some((inner, ref_slot)) = literal_field(inner, "ref_slot", set) else {
        return false;
    };
    let Some((inner, flag)) = literal_field(inner, "flag", set) else {
        return false;
    };
    let Some((inner, number)) = literal_field(inner, "number", set) else {
        return false;
    };
    let Some((inner, tag)) = literal_field(inner, "tag", set) else {
        return false;
    };
    literal_builtin(inner, map_new, 0).is_some()
        && matches!(generation, E::Int(0))
        && matches!(ref_slot, E::Int(0))
        && matches!(flag, E::Bool(_))
        && matches!(number, E::Float(_))
        && matches!(tag, E::Int(0..=3))
}

fn literal_field<'a>(expr: &'a E, key: &str, set: u16) -> Option<(&'a E, &'a E)> {
    let args = literal_builtin(expr, set, 3)?;
    match &args[1] {
        E::String(found) if found == key => Some((&args[0], &args[2])),
        _ => None,
    }
}

fn literal_builtin(expr: &E, index: u16, argc: usize) -> Option<&[E]> {
    match expr {
        E::Call(got, type_args, args, None, None)
            if *got == index && type_args.is_empty() && args.len() == argc =>
        {
            Some(args)
        }
        _ => None,
    }
}

fn pack_bootstrap_static_data(tag: E, w: bool, en: bool, c: bool) -> E {
    add(
        mul(int(layout::OBJECT_TAG_FACTOR), tag),
        add(
            int(if w { 1 } else { 0 }),
            add(int(if en { 2 } else { 0 }), int(if c { 4 } else { 0 })),
        ),
    )
}

/// Write a complete data row before publishing its new logical count.
/// Caller owns receiver/key/value roots and has already reserved capacity.
pub(crate) fn trusted_data(
    e: &mut Emit<'_, '_>,
    object: E,
    position: i64,
    key: E,
    payload: E,
    attributes: (bool, bool, bool),
    body: &mut Vec<S>,
) -> Result<(), ParseError> {
    let (w, en, c) = attributes;
    let l = e.line;
    let base = e.integer()?;
    let row = e.integer()?;
    body.push(assign(base, e.base(object), l));
    body.push(assign(
        row,
        add(var(base), int(if position < 0 { 0 } else { position * 7 })),
        l,
    ));
    // Bootstrap contains no branded Error instances. At runtime the
    // ordinary object prefix also depends on the Error instance brand.
    let object_header = if e.bootstrap_row_suffix.is_some() {
        int(layout::OBJECT_HEADER_CELLS)
    } else {
        let flags = e.integer()?;
        body.push(assign(
            flags,
            E::Mod(
                Box::new(e.decoded(add(var(base), int(2)))),
                Box::new(int(layout::OBJECT_COUNT_FACTOR)),
            ),
            l,
        ));
        choose(
            lt(var(flags), int(layout::OBJECT_ERROR_INSTANCE_FLAG)),
            int(layout::OBJECT_HEADER_CELLS),
            int(layout::ERROR_HEADER_CELLS),
        )
    };
    body.push(assign(
        row,
        add(
            var(row),
            choose(
                eq(e.decoded(var(base)), int(layout::KIND_FUNCTION)),
                int(layout::FUNCTION_CELLS),
                object_header,
            ),
        ),
        l,
    ));
    if let Some(suffix) = e.bootstrap_row_suffix {
        body.push(e.write(add(var(row), int(0)), field(key.clone(), "ref_slot"))?);
        body.push(e.write(add(var(row), int(1)), field(key, "generation"))?);
        // Only this fixed bootstrap manifest has immutable, total projections.
        // Keep the canonical first-use Let after both key writes.
        let payload = if canonical_literal_primitive(&payload) {
            let slot = e.lower.slot(e.span)?;
            body.push(S::Let {
                index: slot,
                declared_schema: Some(value::schema()),
                expr: payload,
                line: l,
            });
            var(slot)
        } else {
            payload
        };
        body.push(e.effect(
            suffix,
            vec![
                var(e.carrier),
                var(row),
                var(base),
                int(layout::OBJECT_COUNT_FACTOR * (position + 1)),
                num(pack_bootstrap_static_data(
                    field(payload.clone(), "tag"),
                    w,
                    en,
                    c,
                )),
                num(field(payload.clone(), "number")),
                num(choose(field(payload.clone(), "flag"), int(1), int(0))),
                num(field(payload.clone(), "ref_slot")),
                num(field(payload, "generation")),
            ],
        )?);
        return Ok(());
    }
    if canonical_literal_primitive(&payload) {
        let slot = e.lower.slot(e.span)?;
        let flag = choose(field(var(slot), "flag"), int(1), int(0));
        body.push(e.write(add(var(row), int(0)), field(key.clone(), "ref_slot"))?);
        body.push(e.write(add(var(row), int(1)), field(key, "generation"))?);
        body.push(S::Let {
            index: slot,
            declared_schema: Some(value::schema()),
            expr: payload,
            line: l,
        });
        body.push(e.write(
            add(var(row), int(2)),
            pack_data(field(var(slot), "tag"), E::Bool(w), E::Bool(en), E::Bool(c)),
        )?);
        body.push(e.write(add(var(row), int(3)), field(var(slot), "number"))?);
        body.push(e.write(add(var(row), int(4)), flag)?);
        body.push(e.write(add(var(row), int(5)), field(var(slot), "ref_slot"))?);
        body.push(e.write(add(var(row), int(6)), field(var(slot), "generation"))?);
    } else {
        let flag = choose(field(payload.clone(), "flag"), int(1), int(0));
        for (offset, cell) in [
            (0, field(key.clone(), "ref_slot")),
            (1, field(key, "generation")),
            (
                2,
                pack_data(
                    field(payload.clone(), "tag"),
                    E::Bool(w),
                    E::Bool(en),
                    E::Bool(c),
                ),
            ),
            (3, field(payload.clone(), "number")),
            (4, flag),
            (5, field(payload.clone(), "ref_slot")),
            (6, field(payload, "generation")),
        ] {
            body.push(e.write(add(var(row), int(offset)), cell)?);
        }
    }
    let flags = e.integer()?;
    body.push(assign(flags, e.decoded(add(var(base), int(2))), l));
    body.push(assign(
        flags,
        E::Mod(Box::new(var(flags)), Box::new(int(layout::OBJECT_COUNT_FACTOR))),
        l,
    ));
    body.push(e.write(
        add(var(base), int(2)),
        add(int(layout::OBJECT_COUNT_FACTOR * (position + 1)), var(flags)),
    )?);
    Ok(())
}

/// Write one bootstrap accessor row (getter only, no setter) before
/// publishing the new logical count. Mirrors [`trusted_data`]'s header and
/// count handling but stores the validated callable pair in cells 3/4 and
/// the accessor metadata `8 | enumerable*2 | configurable*4` in cell 2.
pub(crate) fn trusted_accessor(
    e: &mut Emit<'_, '_>,
    object: E,
    position: i64,
    key: E,
    get: E,
    attributes: (bool, bool),
    body: &mut Vec<S>,
) -> Result<(), ParseError> {
    let (en, c) = attributes;
    let l = e.line;
    let base = e.integer()?;
    let row = e.integer()?;
    body.push(assign(base, e.base(object), l));
    body.push(assign(
        row,
        add(
            add(var(base), int(if position < 0 { 0 } else { position * 7 })),
            choose(
                eq(e.decoded(var(base)), int(layout::KIND_FUNCTION)),
                int(layout::FUNCTION_CELLS),
                int(layout::OBJECT_HEADER_CELLS),
            ),
        ),
        l,
    ));
    for (offset, payload) in [
        (0, field(key.clone(), "ref_slot")),
        (1, field(key, "generation")),
        (
            2,
            add(
                int(8),
                add(
                    int(if en { 2 } else { 0 }),
                    int(if c { 4 } else { 0 }),
                ),
            ),
        ),
        (3, num(field(get.clone(), "ref_slot"))),
        (4, field(get, "generation")),
        (5, int(0)),
        (6, int(0)),
    ] {
        body.push(e.write(add(var(row), int(offset)), payload)?);
    }
    let flags = e.integer()?;
    body.push(assign(flags, e.decoded(add(var(base), int(2))), l));
    body.push(assign(
        flags,
        E::Mod(Box::new(var(flags)), Box::new(int(layout::OBJECT_COUNT_FACTOR))),
        l,
    ));
    body.push(e.write(
        add(var(base), int(2)),
        add(int(layout::OBJECT_COUNT_FACTOR * (position + 1)), var(flags)),
    )?);
    Ok(())
}

/// Write one bootstrap accessor row carrying both halves of the callable
/// pair (getter in cells 3/4, setter in cells 5/6). Mirrors
/// [`trusted_accessor`]'s header and count handling; the accessor metadata
/// stays `8 | enumerable*2 | configurable*4` in cell 2.
pub(crate) fn trusted_accessor_pair(
    e: &mut Emit<'_, '_>,
    object: E,
    position: i64,
    key: E,
    get: E,
    set: E,
    attributes: (bool, bool),
    body: &mut Vec<S>,
) -> Result<(), ParseError> {
    let (en, c) = attributes;
    let l = e.line;
    let base = e.integer()?;
    let row = e.integer()?;
    body.push(assign(base, e.base(object), l));
    body.push(assign(
        row,
        add(
            add(var(base), int(if position < 0 { 0 } else { position * 7 })),
            choose(
                eq(e.decoded(var(base)), int(layout::KIND_FUNCTION)),
                int(layout::FUNCTION_CELLS),
                int(layout::OBJECT_HEADER_CELLS),
            ),
        ),
        l,
    ));
    for (offset, payload) in [
        (0, field(key.clone(), "ref_slot")),
        (1, field(key, "generation")),
        (
            2,
            add(
                int(8),
                add(
                    int(if en { 2 } else { 0 }),
                    int(if c { 4 } else { 0 }),
                ),
            ),
        ),
        (3, num(field(get.clone(), "ref_slot"))),
        (4, field(get, "generation")),
        (5, num(field(set.clone(), "ref_slot"))),
        (6, field(set, "generation")),
    ] {
        body.push(e.write(add(var(row), int(offset)), payload)?);
    }
    let flags = e.integer()?;
    body.push(assign(flags, e.decoded(add(var(base), int(2))), l));
    body.push(assign(
        flags,
        E::Mod(Box::new(var(flags)), Box::new(int(layout::OBJECT_COUNT_FACTOR))),
        l,
    ));
    body.push(e.write(
        add(var(base), int(2)),
        add(int(layout::OBJECT_COUNT_FACTOR * (position + 1)), var(flags)),
    )?);
    Ok(())
}

pub(crate) fn string(
    e: &mut Emit<'_, '_>,
    text: &str,
    frame: u16,
    entry: i64,
    body: &mut Vec<S>,
) -> Result<u16, ParseError> {
    #[cfg(test)]
    let text = ascii_tests::fixture_text(text);
    let h = e.lower.heap_ref(e.span)?.helpers;
    let l = e.line;
    let alloc = e.temp(
        value::allocation_schema(),
        value::allocation(int(0), int(0), int(0)),
    )?;
    let result = e.boxed()?;
    let base = e.integer()?;
    let units: Vec<u16> = text.encode_utf16().collect();
    body.push(assign(
        alloc,
        call(
            h.node_begin,
            vec![
                var(e.carrier),
                int(3 + units.len() as i64),
                int(layout::KIND_STRING),
            ],
        ),
        l,
    ));
    body.push(assign(base, field(var(alloc), "base"), l));
    if let Some(initialize) = e.bootstrap_ascii_init.filter(|_| text.is_ascii()) {
        body.push(e.effect(
            initialize,
            vec![
                var(e.carrier),
                var(base),
                E::Bytes(text.as_bytes().to_vec()),
            ],
        )?);
    } else {
        body.push(e.write(add(var(base), int(2)), int(units.len() as i64))?);
        for (i, u) in units.into_iter().enumerate() {
            body.push(e.write(add(var(base), int(3 + i as i64)), int(i64::from(u)))?);
        }
    }
    body.push(assign(
        result,
        call(
            h.node_publish,
            vec![
                var(e.carrier),
                field(var(alloc), "slot"),
                field(var(alloc), "generation"),
                var(base),
                var(frame),
                int(entry),
            ],
        ),
        l,
    ));
    #[cfg(test)]
    ascii_tests::observe_fixture(e, text, alloc, result, base, frame, entry, body)?;
    Ok(result)
}

/// Permanent root of the optional `Boolean` function. It follows every earlier
/// root, including optional Number, so no earlier index moves. A distinct
/// function cannot share an occupied entry: each permanent root holds one node.
pub(crate) fn boolean_constructor_root(lower: &Lowerer<'_>) -> i64 {
    number_namespace_root(lower) + i64::from(lower.number_namespace_used.get())
}

pub(crate) fn range_error_realm_used(lower: &Lowerer<'_>) -> bool {
    lower.range_error_used.get() || lower.array_constructor_used.get() || lower.array_buffer_used.get() || lower.typed_array_used()
}

fn range_error_base_root(lower: &Lowerer<'_>) -> i64 {
    boolean_constructor_root(lower) + i64::from(lower.boolean_used.get())
}

/// Preserve existing public root indices when Array alone needs RangeError.
/// In that case the RangeError function follows Array; its prototype is
/// retained by the function's data row, without a second permanent root.
pub(crate) fn range_error_constructor_root(lower: &Lowerer<'_>) -> i64 {
    if lower.range_error_used.get() {
        range_error_base_root(lower)
    } else {
        array_namespace_root(lower) + i64::from(lower.array_namespace_used.get())
    }
}

/// Permanent root of the optional `String` function. It follows RangeError so
/// no earlier index moves. Methods are property rows on this one carrier;
/// the prototype is not a second root.
pub(crate) fn string_namespace_root(lower: &Lowerer<'_>) -> i64 {
    range_error_base_root(lower) + i64::from(lower.range_error_used.get())
}

/// Permanent root of the optional `Array` carrier, after all earlier roots.
pub(crate) fn array_namespace_root(lower: &Lowerer<'_>) -> i64 {
    string_namespace_root(lower) + i64::from(lower.string_namespace_used.get())
}

/// Exact fresh raw requests for the fixed bootstrap manifest and its two
/// root frames. Reused unsplittable tails remain a runtime quota obligation.
pub(crate) fn array_buffer_constructor_root(lower: &Lowerer<'_>) -> i64 {
    array_namespace_root(lower)
        + i64::from(lower.array_namespace_used.get())
        + i64::from(range_error_realm_used(lower) && !lower.range_error_used.get())
}

fn typed_array_root_start(lower: &Lowerer<'_>) -> i64 {
    array_buffer_constructor_root(lower) + i64::from(lower.array_buffer_used.get())
}

pub(crate) fn typed_array_constructor_root(
    lower: &Lowerer<'_>,
    element: super::typed_array::Element,
) -> i64 {
    let earlier = lower.typed_array_elements.get() & (element.bit() - 1);
    typed_array_root_start(lower) + i64::from(earlier.count_ones())
}

fn typed_array_root_end(lower: &Lowerer<'_>) -> i64 {
    typed_array_root_start(lower) + i64::from(lower.typed_array_elements.get().count_ones())
}

/// First free permanent root entry. It follows every public realm root and
/// every selected optional-error constructor, so no earlier index moves.
fn base_root_capacity(lower: &Lowerer<'_>) -> i64 {
    typed_array_root_end(lower) + i64::from(lower.optional_errors_used.get().count_ones())
}

/// Private permanent root holding the shared empty String for anonymous
/// function names. It is appended after every existing root and exists only
/// when a `name`/`toString` gate is selected. No public property row aliases
/// it, so guest writes or deletes on `Error.prototype.message` can neither
/// change nor unpublish it.
pub(crate) fn function_name_empty_root(lower: &Lowerer<'_>) -> i64 {
    base_root_capacity(lower)
}

pub(crate) fn selected_root_capacity(lower: &Lowerer<'_>) -> i64 {
    base_root_capacity(lower)
        + i64::from(lower.function_name_used.get() || lower.function_to_string_used.get())
}

/// Ordinary five-row method table rooted after every public realm root. The
/// packed Array prototype retains its six-cell header and real prototype link.
pub(crate) fn array_method_table_root(lower: &Lowerer<'_>) -> i64 {
    type_error_constructor_root(lower) + 1
}

/// Append Number after all existing roots, including the Array method table.
pub(crate) fn number_namespace_root(lower: &Lowerer<'_>) -> i64 {
    array_method_table_root(lower) + 1
}

/// The private empty packed Array prototype owns the tail of the permanent
/// root frame, after every accepted Math/URI entry, so no earlier root index
/// moves.
pub(crate) fn array_prototype_root(lower: &Lowerer<'_>) -> i64 {
    uri_calls::prototype_root(lower)
        + i64::from(uri_calls::prototype_used(lower))
        + i64::from(lower.uri_used.get().count_ones())
}

/// The three private primitive prototypes follow the Array prototype. String
/// owns the shared `hasOwnProperty` row plus the admitted `charCodeAt`,
/// `charAt`, `codePointAt`, `concat`, `substring`, `slice`, `indexOf` and
/// `lastIndexOf` rows on the
/// already published code-267 function and the method functions; Number and
/// Boolean own only `hasOwnProperty`. Every earlier root index stays stable
/// and the method functions/key Strings are retained by the String prototype
/// rows rather than a new permanent root entry.
pub(crate) fn string_prototype_root(lower: &Lowerer<'_>) -> i64 {
    array_prototype_root(lower) + 1
}

pub(crate) fn number_prototype_root(lower: &Lowerer<'_>) -> i64 {
    string_prototype_root(lower) + 1
}

pub(crate) fn boolean_prototype_root(lower: &Lowerer<'_>) -> i64 {
    number_prototype_root(lower) + 1
}

pub(crate) fn bootstrap_request_cells(lower: &Lowerer<'_>) -> u64 {
    bootstrap_request_cells_with_function_api(
        lower,
        lower.function_name_used.get() || lower.function_to_string_used.get(),
        lower.function_to_string_used.get(),
    )
}

pub(crate) fn bootstrap_request_cells_with_function_api(
    lower: &Lowerer<'_>,
    function_names_used: bool,
    function_to_string_used: bool,
) -> u64 {
    bootstrap_request_cells_gated(
        lower.uri_used.get(),
        selected_root_capacity(lower),
        lower.number_namespace_used.get(),
        lower.string_namespace_used.get(),
        lower.array_namespace_used.get(),
        lower.number_to_string_used.get(),
        lower.string_replace_all_used.get(),
        lower.string_search_used.get(),
        lower.string_match_used.get(),
    ) + bootstrap_function_api_request_cells(function_names_used, function_to_string_used)
        + bootstrap_function_prototype_request_cells(lower.function_prototype_used.get())
        + bootstrap_primitive_carrier_request_cells(
            lower.primitive_constructor_read.get(),
            lower.number_namespace_used.get(),
            lower.string_namespace_used.get(),
        )
        + bootstrap_constructor_prototype_row_cells(
            lower.function_prototype_used.get(),
            lower.primitive_constructor_read.get(),
            lower.number_namespace_used.get(),
            lower.string_namespace_used.get(),
            lower.array_namespace_used.get(),
            lower.boolean_used.get(),
        )
}

/// The one gated `prototype` accessor row on `Function.prototype` and its two
/// private nodes. Kept separate from [`bootstrap_function_api_request_cells`]
/// so every earlier manifest witness stays byte-for-byte unchanged: only a
/// source that can spell `.prototype` (or a `new` site) pays the row, the
/// getter and the setter.
pub(crate) fn bootstrap_function_prototype_request_cells(used: bool) -> u64 {
    if !used {
        return 0;
    }
    // One 7-cell accessor row plus two private one-row function nodes.
    7 + 2 * (3 + layout::FUNCTION_CELLS as u64 + 7)
}

/// The gated `constructor` rows the four always-present builtin prototype
/// objects gain once the `.prototype` accessor is selected: one row each on
/// the private Number, String, Boolean and Array method-table nodes. They are
/// charged only when both the accessor and the carrier that owns the row are
/// selected, so a source that only reads a source function's `.prototype`
/// keeps the exact 47-cell accessor charge. No node, key String or permanent
/// root is added: every row reuses the fixed `constructor` String and an
/// existing prototype object.
pub(crate) fn bootstrap_constructor_prototype_row_cells(
    function_prototype_used: bool,
    primitive_constructor_read: bool,
    number_namespace_used: bool,
    string_namespace_used: bool,
    array_namespace_used: bool,
    boolean_used: bool,
) -> u64 {
    if !function_prototype_used {
        return 0;
    }
    // A `.constructor` read selects the three primitive carriers (the static
    // walk cannot tell which primitive the receiver holds), so their back-link
    // rows exist even without the matching global name.
    7 * (u64::from(number_namespace_used || primitive_constructor_read)
        + u64::from(string_namespace_used || primitive_constructor_read)
        + u64::from(array_namespace_used)
        + u64::from(boolean_used || primitive_constructor_read))
}

/// The three minimal primitive carrier nodes a bare `.constructor` read owns
/// when the source never spells `Number`, `String` or `Boolean`: one two-row
/// function node per class (`length` plus the carrier's own `prototype` row,
/// both on fixed key Strings). Each one is kept alive by the private
/// prototype's back-link row, so no node opens a permanent root entry. The
/// Boolean carrier is charged by the caller next to `BOOLEAN_FUNCTION_CELLS`,
/// which already has exactly this shape.
pub(crate) fn bootstrap_primitive_carrier_request_cells(
    primitive_constructor_read: bool,
    number_namespace_used: bool,
    string_namespace_used: bool,
) -> u64 {
    if !primitive_constructor_read {
        return 0;
    }
    let node = 3 + layout::FUNCTION_CELLS as u64 + 2 * 7;
    node * (u64::from(!number_namespace_used) + u64::from(!string_namespace_used))
}

/// The two private accessor function nodes of the gated `prototype` accessor.
pub(crate) fn bootstrap_function_prototype_nodes(used: bool) -> u64 {
    if used { 2 } else { 0 }
}

/// The two gated `Function.prototype` rows and their private nodes, kept
/// separate from `bootstrap_request_cells_for` so the five fixed-manifest
/// witnesses stay byte-for-byte unchanged.
pub(crate) fn bootstrap_function_api_request_cells(
    function_names_used: bool,
    function_to_string_used: bool,
) -> u64 {
    // Function.prototype gains one row per selected feature (7 cells each),
    // the name getter is one 20-cell one-row function node, and `toString`
    // is one 20-cell one-row function node.
    let prototype_rows = u64::from(function_names_used) + u64::from(function_to_string_used);
    let mut cells = 7 * prototype_rows;
    if function_names_used {
        cells += 3 + layout::FUNCTION_CELLS as u64 + 7;
    }
    if function_to_string_used {
        cells += 3 + layout::FUNCTION_CELLS as u64 + 7;
    }
    cells
}

/// Pure form of the static bootstrap request: the selected URI manifest, the
/// fixed object/function/String/Array-prototype extents, both root frames and
/// the first directory page, excluding the realm prefix. Runtime reservations
/// never read this; splitting it from the lowerer inputs keeps the default
/// request directly checkable by the capacity regression below.
#[cfg(test)]
fn bootstrap_request_cells_for(
    uri_used: u8,
    root_capacity: i64,
    number_namespace_used: bool,
    string_namespace_used: bool,
    array_namespace_used: bool,
    number_to_string_used: bool,
    string_replace_all_used: bool,
    string_search_used: bool,
) -> u64 {
    bootstrap_request_cells_gated(
        uri_used,
        root_capacity,
        number_namespace_used,
        string_namespace_used,
        array_namespace_used,
        number_to_string_used,
        string_replace_all_used,
        string_search_used,
        false,
    )
}

/// Extended static request with the gated `String.prototype.match` row. The
/// eight-argument form above keeps every existing budget witness unchanged.
#[allow(clippy::too_many_arguments)]
fn bootstrap_request_cells_gated(
    uri_used: u8,
    root_capacity: i64,
    number_namespace_used: bool,
    string_namespace_used: bool,
    array_namespace_used: bool,
    number_to_string_used: bool,
    string_replace_all_used: bool,
    string_search_used: bool,
    string_match_used: bool,
) -> u64 {
    // The realm Error prototype is a four-row node: the two published rows
    // (`name`, `message`) plus two reserved empty rows. Its capacity is
    // charged here exactly like the emitter allocates it; the reserved rows
    // are not published, so the node count is unchanged.
    // Three private primitive prototypes. String is an eighteen-row ordinary
    // object node (nineteen when `replaceAll` is selected) holding the
    // shared `hasOwnProperty` row plus `charCodeAt`, `charAt`, `codePointAt`,
    // `concat`, `substring`, `slice`, `indexOf`, `lastIndexOf`, `toString`,
    // `toLowerCase`, `toUpperCase`, `trim`, `trimEnd`, `trimStart`, `replace`,
    // `split` and `repeat`; Number and Boolean keep one row each. The shared `hasOwnProperty` key
    // String and function value are existing published nodes, not charged twice.
    // The String descriptor has 18 rows plus the gated `replaceAll`,
    // `search` and `match` rows. No new permanent root.
    let object_rows = [
        PROTO_ROWS as u64,
        4,
        2,
        18 + u64::from(string_replace_all_used)
            + u64::from(string_search_used)
            + u64::from(string_match_used),
        1 + u64::from(number_to_string_used),
        1,
        ARRAY_PROTOTYPE_METHODS.len() as u64,
    ];
    let method_count = METHODS.len();
    // Two headers, method rows, six prototype methods and sixteen String methods.
    // charCodeAt, charAt, codePointAt, concat, substring, slice, indexOf,
    // lastIndexOf, toString, toLowerCase, toUpperCase, trim, trimEnd,
    // trimStart, replace and split.
    let mut function_rows = Vec::with_capacity(2 + method_count + 22);
    function_rows.push(4);
    function_rows.push(2 + method_count as u64);
    function_rows.push(2); // TypeError: length and prototype, both existing key Strings.
    function_rows.extend(std::iter::repeat_n(1, method_count));
    // Object.prototype valueOf/toString/hasOwnProperty and Function.prototype
    // call/apply/bind take their capacities from the table the emitter also
    // reads, so the charged rows always match the actual allocations.
    function_rows.extend(
        PROTO_METHOD_CAPACITIES
            .into_iter()
            .map(|capacity| capacity as u64),
    );
    // `Error.prototype.toString`: one one-row function node. Its `toString`
    // key String is already the fixed String-prototype key, and its row is
    // inside the four-row Error prototype charged above.
    function_rows.push(1);
    // The admitted String.prototype methods through `repeat` are seventeen
    // fixed one-row nodes. The gated `replaceAll` node is charged below.
    // Their key Strings are charged below. No new root.
    function_rows.push(1);
    function_rows.push(1);
    function_rows.push(1);
    function_rows.push(1);
    function_rows.push(1);
    function_rows.push(1);
    function_rows.push(1);
    function_rows.push(1);
    function_rows.push(1);
    function_rows.push(1);
    function_rows.push(1);
    function_rows.extend([1; 5]);
    function_rows.push(1); // String.prototype.repeat
    if string_replace_all_used {
        function_rows.push(1); // String.prototype.replaceAll
    }
    if string_search_used {
        function_rows.push(1); // String.prototype.search
    }
    if string_match_used {
        function_rows.push(1); // String.prototype.match
    }
    function_rows.extend([1; ARRAY_PROTOTYPE_METHODS.len()]);
    if number_namespace_used {
        function_rows.push(NUMBER_NAMESPACE_ROWS as u64);
    }
    if string_namespace_used {
        function_rows.push(STRING_NAMESPACE_ROWS as u64);
        function_rows.push(1); // String.fromCharCode
        function_rows.push(1); // String.fromCodePoint
    }
    if array_namespace_used {
        function_rows.push(ARRAY_NAMESPACE_ROWS as u64);
        function_rows.push(1); // Array.isArray
    }
    if number_to_string_used {
        // `Number.prototype.toString`: one one-row function node. Its
        // `toString` key String is the fixed key already charged above, and
        // the extra Number-prototype row is charged through `object_rows`.
        function_rows.push(1);
    }
    let nodes = object_rows
        .into_iter()
        .map(|rows| 3 + 5 + 7 * rows)
        .sum::<u64>()
        + function_rows
            .into_iter()
            .map(|rows| 3 + 10 + 7 * rows)
            .sum::<u64>();
    let keys = [
        "length",
        "prototype",
        "constructor",
        "name",
        "message",
        "Error",
        "TypeError",
        "",
    ];
    let strings = keys
        .into_iter()
        .chain(METHODS.iter().map(|(name, _)| *name))
        .chain([
            "hasOwnProperty",
            "valueOf",
            "toString",
            "call",
            "apply",
            "bind",
            "charCodeAt",
            "charAt",
            "codePointAt",
            "concat",
            "substring",
            "slice",
            "indexOf",
            "lastIndexOf",
            STRING_TO_STRING_NAME,
            STRING_TO_LOWER_CASE_NAME,
            STRING_TO_UPPER_CASE_NAME,
            STRING_TRIM_NAME,
            STRING_TRIM_END_NAME,
            STRING_TRIM_START_NAME,
            STRING_REPLACE_NAME,
            STRING_SPLIT_NAME,
            STRING_REPEAT_NAME,
        ])
        .chain(string_replace_all_used.then_some(STRING_REPLACE_ALL_NAME))
        .chain(string_search_used.then_some(STRING_SEARCH_NAME))
        .chain(string_match_used.then_some(STRING_MATCH_NAME))
        .chain(ARRAY_PROTOTYPE_METHODS.iter().map(|(name, _, _)| *name))
        .chain(
            NUMBER_CONSTANTS
                .iter()
                .filter_map(|&(name, _)| number_namespace_used.then_some(name)),
        )
        .chain(
            super::math::GLOBALS[..NUMBER_PARSE_METHODS]
                .iter()
                .filter_map(|&(name, _)| number_namespace_used.then_some(name)),
        )
        .chain(
            [STRING_FROM_CHAR_CODE_NAME, STRING_FROM_CODE_POINT_NAME]
                .into_iter()
                .filter_map(|name| string_namespace_used.then_some(name)),
        )
        .chain(array_namespace_used.then_some(ARRAY_IS_ARRAY_NAME))
        .map(|name| 6 + name.encode_utf16().count() as u64)
        .sum::<u64>();
    nodes
        + uri_calls::manifest(uri_used).0
        + strings
        + (layout::FRAME_FIXED_CELLS
            + layout::FRAME_CELLS_PER_ENTRY * root_capacity) as u64
        + (4 + 3 * 8)
        + layout::DIR_PAGE_CELLS as u64
        // The private empty packed Array prototype: raw header plus the
        // six-cell Array header, no payload.
        + (layout::RAW_HEADER_CELLS + layout::ARRAY_HEADER_CELLS) as u64
}

fn publish_error_constructor(
    e: &mut Emit<'_, '_>,
    allocate: u16,
    function_proto: u16,
    permanent: u16,
    constructor_root: i64,
    code: i64,
    length_key: u16,
    prototype_key: u16,
    proto: u16,
    body: &mut Vec<S>,
) -> Result<(), ParseError> {
    let constructor = e.boxed()?;
    body.push(assign(
        constructor,
        call(
            allocate,
            vec![
                var(e.carrier),
                int(4),
                var(function_proto),
                int(2),
                int(code),
                int(1), int(0),
                var(permanent),
                int(constructor_root),
            ],
        ),
        e.line,
    ));
    trusted_data(
        e,
        var(constructor),
        0,
        var(length_key),
        value::number(E::Float(1.0)),
        (false, false, false),
        body,
    )?;
    trusted_data(
        e,
        var(constructor),
        1,
        var(prototype_key),
        var(proto),
        (false, false, false),
        body,
    )?;
    Ok(())
}

pub(crate) fn append(
    lower: &mut Lowerer<'_>,
    ir: &mut FrontendIr,
    carrier_schema: &T,
    span: swc_common::Span,
    stmts: &mut Vec<S>,
) -> Result<Intrinsics, ParseError> {
    let start = super::heap::fresh_index(lower, ir, span)?;
    let root = start;
    let allocate = start
        .checked_add(1)
        .ok_or_else(|| lower.error(span, "intrinsic index space"))?;
    let bootstrap = start
        .checked_add(2)
        .ok_or_else(|| lower.error(span, "intrinsic index space"))?;
    let type_error = start
        .checked_add(3)
        .ok_or_else(|| lower.error(span, "intrinsic index space"))?;
    let length_key = start
        .checked_add(4)
        .ok_or_else(|| lower.error(span, "intrinsic index space"))?;
    let suffix = start
        .checked_add(5)
        .ok_or_else(|| lower.error(span, "intrinsic index space"))?;
    let ascii_init = start
        .checked_add(6)
        .ok_or_else(|| lower.error(span, "intrinsic index space"))?;
    let h = lower.heap_ref(span)?.helpers;
    let line = lower.line(span);
    let root_capacity = selected_root_capacity(lower);
    // The two Function-prototype gates are compile-time facts; both the
    // bootstrap emitter and the private helpers read them.
    let function_names = lower.function_name_used.get() || lower.function_to_string_used.get();
    let function_to_string = lower.function_to_string_used.get();
    let function_prototype = lower.function_prototype_used.get();
    // Intrinsic reads never allocate. The pointer is a raw frame base with
    // exact bottom-frame topology, not a second tracing root.
    {
        let carrier = lower.slot(span)?;
        let entry = lower.slot(span)?;
        let mut e = Emit::new(lower, span, carrier)?;
        let frame = e.integer()?;
        let end = e.integer()?;
        let address = e.integer()?;
        let slot = e.integer()?;
        let generation = e.integer()?;
        let base = e.integer()?;
        let original_function_roots = or(eq(var(entry), int(1)), eq(var(entry), int(4)));
        let function_roots = if e.lower.numeric_globals.get() == 0 {
            original_function_roots
        } else {
            or(
                original_function_roots,
                not(lt(var(entry), int(5 + i64::from(e.lower.math_used.get())))),
            )
        };
        let function_roots = if uri_calls::prototype_used(e.lower) {
            or(
                and(
                    function_roots,
                    lt(var(entry), int(uri_calls::prototype_root(e.lower))),
                ),
                lt(int(uri_calls::prototype_root(e.lower)), var(entry)),
            )
        } else {
            function_roots
        };
        // The optional `JSON` object sits after every function range and is an
        // ordinary Object root, so its tail entry leaves the function set.
        let function_roots = if e.lower.json_used.get() {
            and(function_roots, not(eq(var(entry), int(json_root(e.lower)))))
        } else {
            function_roots
        };
        // The three private primitive prototypes are ordinary Object roots in
        // the permanent tail under every Math/numeric/URI/JSON combination, so
        // they leave the function set the same way. Their node kind is still
        // checked (Object, not Array) by the ordinary path below.
        let primitive_roots = or(
            or(
                eq(var(entry), int(string_prototype_root(e.lower))),
                eq(var(entry), int(number_prototype_root(e.lower))),
            ),
            eq(var(entry), int(boolean_prototype_root(e.lower))),
        );
        let function_roots = or(
            and(function_roots, not(primitive_roots)),
            eq(var(entry), int(type_error_constructor_root(e.lower))),
        );
        let function_roots = and(function_roots, not(eq(var(entry), int(array_method_table_root(e.lower)))));
        let function_roots = if e.lower.number_namespace_used.get() {
            or(function_roots, eq(var(entry), int(number_namespace_root(e.lower))))
        } else {
            function_roots
        };
        let function_roots = if e.lower.boolean_used.get() {
            or(
                function_roots,
                eq(var(entry), int(boolean_constructor_root(e.lower))),
            )
        } else {
            function_roots
        };
        let function_roots = if range_error_realm_used(e.lower) {
            or(
                function_roots,
                eq(var(entry), int(range_error_constructor_root(e.lower))),
            )
        } else {
            function_roots
        };
        let function_roots = if e.lower.string_namespace_used.get() {
            or(
                function_roots,
                eq(var(entry), int(string_namespace_root(e.lower))),
            )
        } else {
            function_roots
        };
        let function_roots = if e.lower.array_namespace_used.get() {
            or(function_roots, eq(var(entry), int(array_namespace_root(e.lower))))
        } else {
            function_roots
        };
        let function_roots = if e.lower.array_buffer_used.get() {
            or(function_roots, eq(var(entry), int(array_buffer_constructor_root(e.lower))))
        } else { function_roots };
        let function_roots = if e.lower.typed_array_used() {
            or(function_roots, and(
                not(lt(var(entry), int(typed_array_root_start(e.lower)))),
                lt(var(entry), int(typed_array_root_end(e.lower))),
            ))
        } else { function_roots };
        let mut function_roots = function_roots;
        for error in OptionalError::selected(e.lower) {
            function_roots = or(function_roots, eq(var(entry), int(error.constructor_root(e.lower))));
        }
        let array_entry = int(array_prototype_root(e.lower));
        let name_empty_entry = int(function_name_empty_root(e.lower));
        let mut body = vec![
            assign(frame, e.decoded(int(layout::HDR_INTRINSIC_FRAME)), line),
            assign(end, e.decoded(int(layout::HDR_HEAP_END)), line),
            assert_stmt(
                and(
                    not(lt(var(frame), int(layout::PREFIX_CELLS))),
                    not(lt(var(end), add(var(frame), int(4 + 3 * root_capacity)))),
                ),
                line,
            ),
            assert_stmt(
                eq(
                    e.decoded(add(var(frame), int(1))),
                    int(layout::RAW_CLASS_ROOT_FRAME),
                ),
                line,
            ),
            assert_stmt(eq(e.decoded(add(var(frame), int(2))), int(0)), line),
            assert_stmt(
                eq(e.decoded(add(var(frame), int(3))), int(root_capacity)),
                line,
            ),
            assert_stmt(
                not(lt(e.decoded(var(frame)), int(4 + 3 * root_capacity))),
                line,
            ),
            assert_stmt(
                not(lt(var(end), add(var(frame), e.decoded(var(frame))))),
                line,
            ),
            assert_stmt(
                and(
                    not(lt(var(entry), int(0))),
                    lt(var(entry), int(root_capacity)),
                ),
                line,
            ),
            assign(
                address,
                add(add(var(frame), int(4)), mul(int(3), var(entry))),
                line,
            ),
            branch(
                eq(var(entry), name_empty_entry.clone()),
                vec![assert_stmt(
                    eq(e.decoded(var(address)), int(layout::ROOT_STRING)),
                    line,
                )],
                vec![assert_stmt(
                    eq(e.decoded(var(address)), int(layout::ROOT_OBJECT)),
                    line,
                )],
                line,
            ),
            assign(slot, e.decoded(add(var(address), int(1))), line),
            assign(generation, e.generation(add(var(address), int(2))), line),
            assign(
                base,
                call(
                    h.checked_base,
                    vec![var(carrier), var(slot), var(generation)],
                ),
                line,
            ),
        ];
        // The private Array prototype entry is the only root whose node kind
        // is an Array. The private empty String root of anonymous function
        // names is the only String root. Every other accepted entry keeps the
        // original checked kind/header/error gates unchanged.
        let mut ordinary = vec![assert_stmt(
            eq(
                e.decoded(var(base)),
                choose(
                    function_roots,
                    int(layout::KIND_FUNCTION),
                    int(layout::KIND_OBJECT),
                ),
            ),
            line,
        )];
        // Realm roots are ordinary objects and functions: every prototype
        // shell is brand 0 and only runtime-allocated Error instances carry
        // the legacy|instance brand.
        let header = decode_header(&mut e, base, &mut ordinary)?;
        ordinary.push(assert_stmt(
            and(not(var(header.error)), not(var(header.error_instance))),
            line,
        ));
        body.push(branch(
            eq(var(entry), name_empty_entry),
            vec![
                assert_stmt(eq(e.decoded(var(base)), int(layout::KIND_STRING)), line),
                assert_stmt(eq(e.decoded(add(var(base), int(2))), int(0)), line),
            ],
            vec![branch(
                eq(var(entry), array_entry),
                vec![assert_stmt(
                    eq(e.decoded(var(base)), int(layout::KIND_ARRAY)),
                    line,
                )],
                ordinary,
                line,
            )],
            line,
        ));
        let value_tag = choose(
            eq(var(entry), int(function_name_empty_root(e.lower))),
            int(value::STRING),
            int(value::OBJECT),
        );
        let body = e.finish(body);
        declare(
            ir,
            stmts,
            ROOT_NAME,
            root,
            vec![carrier, entry],
            vec![carrier_schema.clone(), T::Int],
            value::schema(),
            body,
            value::dynamic_value(
                value_tag,
                E::Float(0.0),
                E::Bool(false),
                var(slot),
                var(generation),
            ),
            line,
        );
    }
    // Trusted shell allocation uses an existing caller-owned root entry.
    // No helper frame and no extra logical node identity are introduced.
    // The seventh argument is the object brand, not a boolean: plain objects,
    // prototype shells and functions pass 0, and guest-visible Error instances
    // pass legacy|instance. Only the instance brand extends the ordinary
    // five-cell header with the internal message String pair, so the physical
    // node grows by two cells and the first own row moves to
    // `ERROR_HEADER_CELLS`.
    {
        let params = (0..9)
            .map(|_| lower.slot(span))
            .collect::<Result<Vec<_>, _>>()?;
        let [
            carrier,
            kind,
            proto,
            capacity,
            code,
            length,
            brand,
            frame,
            entry,
        ] = params.as_slice()
        else {
            unreachable!()
        };
        let mut e = Emit::new(lower, span, *carrier)?;
        let alloc = e.temp(
            value::allocation_schema(),
            value::allocation(int(0), int(0), int(0)),
        )?;
        let base = e.integer()?;
        let result = e.boxed()?;
        // The brand is one of plain (0), legacy prototype (2, no longer
        // emitted by this bootstrap) and branded Error instance (2|4).
        // Only the instance brand extends the ordinary header with the internal
        // message String pair.
        let instance = not(lt(
            var(*brand),
            int(layout::OBJECT_ERROR_INSTANCE_FLAG),
        ));
        let mut body = vec![
            assert_stmt(
                or(
                    or(eq(var(*kind), int(3)), eq(var(*kind), int(4))),
                    eq(var(*kind), int(layout::KIND_REGEXP)),
                ),
                line,
            ),
            assert_stmt(not(lt(int(256), var(*capacity))), line),
            assert_stmt(
                or(
                    or(
                        eq(var(*brand), int(0)),
                        eq(var(*brand), int(layout::OBJECT_LEGACY_ERROR_FLAG)),
                    ),
                    eq(
                        var(*brand),
                        int(
                            layout::OBJECT_LEGACY_ERROR_FLAG
                                | layout::OBJECT_ERROR_INSTANCE_FLAG,
                        ),
                    ),
                ),
                line,
            ),
            assert_stmt(
                or(not(instance.clone()), eq(var(*kind), int(3))),
                line,
            ),
            assign(
                alloc,
                call(
                    h.node_begin,
                    vec![
                        var(*carrier),
                        add(
                            choose(
                                eq(var(*kind), int(4)),
                                int(10),
                                choose(
                                    instance.clone(),
                                    int(layout::ERROR_HEADER_CELLS),
                                    int(layout::OBJECT_HEADER_CELLS),
                                ),
                            ),
                            mul(int(7), var(*capacity)),
                        ),
                        var(*kind),
                    ],
                ),
                line,
            ),
            assign(base, field(var(alloc), "base"), line),
        ];
        for (offset, payload) in [
            (2, pack_brand(int(0), E::Bool(true), var(*brand))),
            (3, field(var(*proto), "ref_slot")),
            (4, field(var(*proto), "generation")),
        ] {
            body.push(e.write(add(var(base), int(offset)), payload)?);
        }
        // A branded instance's header names an internal String pair before its
        // first own row. The allocator has no message argument, so the pair is
        // the canonical absent edge: a zero slot with a zero generation, which
        // the collector's pair check and trace both treat as "no message".
        body.push(branch(
            instance,
            vec![
                e.write(add(var(base), int(layout::ERROR_MESSAGE_OFFSET)), int(0))?,
                e.write(
                    add(var(base), int(layout::ERROR_MESSAGE_OFFSET + 1)),
                    int(0),
                )?,
            ],
            Vec::new(),
            line,
        ));
        let mut function = Vec::new();
        for (offset, payload) in [
            (5, var(*code)),
            (6, int(0)),
            (7, int(0)),
            (8, int(1)),
            (9, var(*length)),
        ] {
            function.push(e.write(add(var(base), int(offset)), payload)?);
        }
        body.push(branch(eq(var(*kind), int(4)), function, Vec::new(), line));
        body.push(assign(
            result,
            call(
                h.node_publish,
                vec![
                    var(*carrier),
                    field(var(alloc), "slot"),
                    field(var(alloc), "generation"),
                    var(base),
                    var(*frame),
                    var(*entry),
                ],
            ),
            line,
        ));
        let body = e.finish(body);
        declare(
            ir,
            stmts,
            ALLOC_NAME,
            allocate,
            params.clone(),
            vec![
                carrier_schema.clone(),
                T::Int,
                value::schema(),
                T::Int,
                T::Int,
                T::Int,
                T::Int,
                T::Int,
                T::Int,
            ],
            value::schema(),
            body,
            var(result),
            line,
        );
    }
    {
        let carrier = lower.slot(span)?;
        let mut e = Emit::new(lower, span, carrier)?;
        e.bootstrap_row_suffix = Some(suffix);
        e.bootstrap_ascii_init = Some(ascii_init);
        let permanent = e.integer()?;
        let stage = e.integer()?;
        let objects = (0..8).map(|_| e.boxed()).collect::<Result<Vec<_>, _>>()?;
        // The separately rooted Array method table, captured once so the
        // Array carrier can publish the `constructor` row that makes
        // `Array.prototype.constructor === Array` (and `[].constructor`)
        // resolve through the same table that owns every Array method.
        let array_table = e.boxed()?;
        // The private empty packed Array prototype, captured for the Array
        // carrier's own `prototype` row.
        let array_proto_object = e.boxed()?;
        // The private primitive prototype rows reuse the already published
        // code-267 function and its `hasOwnProperty` key String. Both are
        // captured where they are installed and stay permanently reachable
        // from the Object.prototype row, so no extra node is allocated.
        let has_own_key = e.boxed()?;
        let has_own_fn = e.boxed()?;
        let mut body = vec![
            assert_stmt(
                eq(e.decoded(int(layout::HDR_INTRINSIC_FRAME)), int(0)),
                line,
            ),
            assert_stmt(eq(e.decoded(int(layout::HDR_ROOT_TOP)), int(0)), line),
            assign(
                permanent,
                call(h.root_enter, vec![var(carrier), int(root_capacity)]),
                line,
            ),
        ];
        body.push(e.write(int(layout::HDR_INTRINSIC_FRAME), var(permanent))?);
        body.push(assign(
            stage,
            call(h.root_enter, vec![var(carrier), int(8)]),
            line,
        ));
        let proto_rows = PROTO_ROWS;
        let object_capacity = 2 + METHODS.len() as i64;
        // `Function.prototype` starts with its four fixed rows (`length`,
        // `call`, `apply`, `bind`) and pays one more row per selected
        // Function-prototype feature: the `name` accessor, `toString`, and
        // the `prototype` get/set accessor.
        let function_proto_rows = 4
            + i64::from(function_names)
            + i64::from(function_to_string)
            + i64::from(function_prototype);
        // The only branded objects this bootstrap exposes are Error
        // *instances*; every prototype shell (Object, Function, Error,
        // TypeError) is a plain object, matching upstream
        // `Object.prototype.toString.call(Error.prototype) === "[object Object]"`.
        for (i, kind, capacity, code, length) in [
            (0, 3, proto_rows, 0, 0),
            (1, 4, function_proto_rows, FUNCTION_PROTOTYPE_CODE, 0),
            // Error.prototype is allocated with four rows from the start.
            // Only `name` and `message` are published at bootstrap; the two
            // reserved rows stay as regular empty capacity so no later write
            // can overrun the node tail.
            (2, 3, 4, 0, 0),
            (3, 3, 2, 0, 0),
            (4, 4, object_capacity, OBJECT_CODE, 1),
        ] {
            let proto = match i {
                0 => value::null(),
                1 | 2 => var(objects[0]),
                3 => var(objects[2]),
                _ => var(objects[1]),
            };
            body.push(assign(
                objects[i],
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(kind),
                        proto,
                        int(capacity),
                        int(code),
                        int(length),
                        int(0),
                        var(permanent),
                        int(i as i64),
                    ],
                ),
                line,
            ));
        }
        let length_key = string(&mut e, "length", stage, 0, &mut body)?;
        trusted_data(
            &mut e,
            var(objects[1]),
            0,
            var(length_key),
            value::number(E::Float(0.0)),
            (false, false, false),
            &mut body,
        )?;
        trusted_data(
            &mut e,
            var(objects[4]),
            0,
            var(length_key),
            value::number(E::Float(1.0)),
            (false, false, false),
            &mut body,
        )?;
        let constructor = string(&mut e, "constructor", stage, 0, &mut body)?;
        trusted_data(
            &mut e,
            var(objects[0]),
            0,
            var(constructor),
            var(objects[4]),
            (true, false, true),
            &mut body,
        )?;
        let prototype = string(&mut e, "prototype", stage, 0, &mut body)?;
        trusted_data(
            &mut e,
            var(objects[4]),
            1,
            var(prototype),
            var(objects[0]),
            (true, true, true),
            &mut body,
        )?;
        let name = string(&mut e, "name", stage, 0, &mut body)?;
        let error = string(&mut e, "Error", stage, 1, &mut body)?;
        trusted_data(
            &mut e,
            var(objects[2]),
            0,
            var(name),
            var(error),
            (true, false, true),
            &mut body,
        )?;
        let type_name = string(&mut e, "TypeError", stage, 1, &mut body)?;
        trusted_data(
            &mut e,
            var(objects[3]),
            0,
            var(name),
            var(type_name),
            (true, false, true),
            &mut body,
        )?;
        let message = string(&mut e, "message", stage, 0, &mut body)?;
        // The shared empty String for anonymous function names is published
        // into its own permanent root when a name gate is selected. The same
        // node still backs the public `message` rows below, but those rows are
        // guest-writable/configurable and never retain it exclusively.
        let empty = if function_names {
            let empty_entry = function_name_empty_root(e.lower);
            string(&mut e, "", permanent, empty_entry, &mut body)?
        } else {
            string(&mut e, "", stage, 1, &mut body)?
        };
        trusted_data(
            &mut e,
            var(objects[2]),
            1,
            var(message),
            var(empty),
            (true, false, true),
            &mut body,
        )?;
        trusted_data(
            &mut e,
            var(objects[3]),
            1,
            var(message),
            var(empty),
            (true, false, true),
            &mut body,
        )?;
        // Public TypeError is a Function, separate from the private allocator.
        // Its keys are already retained by Function/Object constructor rows.
        let type_constructor = e.boxed()?;
        body.push(assign(
            type_constructor,
            call(
                allocate,
                vec![
                    var(carrier),
                    int(4),
                    var(objects[1]),
                    int(2),
                    int(TYPE_ERROR_CODE),
                    int(1), int(0),
                    var(permanent),
                    int(type_error_constructor_root(e.lower)),
                ],
            ),
            line,
        ));
        trusted_data(
            &mut e,
            var(type_constructor),
            0,
            var(length_key),
            value::number(E::Float(1.0)),
            (false, false, false),
            &mut body,
        )?;
        trusted_data(
            &mut e,
            var(type_constructor),
            1,
            var(prototype),
            var(objects[3]),
            (false, false, false),
            &mut body,
        )?;
        let range_error = range_error_realm_used(e.lower).then(|| (
            "RangeError", RANGE_ERROR_CODE, range_error_constructor_root(e.lower),
        ));
        let optional_errors: Vec<_> = OptionalError::selected(e.lower)
            .filter(|error| !matches!(error, OptionalError::Uri))
            .map(|error| (error.name(), error.code(), error.constructor_root(e.lower)))
            .collect();
        for (error_name, code, constructor_root) in range_error.into_iter().chain(optional_errors) {
            // Reuse the length/prototype/name/message keys. The stage entry
            // holds each prototype through allocation of its constructor; the
            // constructor's data row retains it before that entry is reused.
            let proto = e.boxed()?;
            // RangeError/SyntaxError/... prototypes are plain object shells
            // (brand 0): upstream reports them as `[object Object]` and their
            // `.toString()` throws because only instances carry the instance
            // brand.
            body.push(assign(
                proto,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(3),
                        var(objects[2]),
                        int(2),
                        int(0),
                        int(0), int(0),
                        var(stage),
                        int(RANGE_ERROR_STAGE_ENTRY),
                    ],
                ),
                line,
            ));
            let error_name = string(&mut e, error_name, stage, 0, &mut body)?;
            trusted_data(
                &mut e,
                var(proto),
                0,
                var(name),
                var(error_name),
                (true, false, true),
                &mut body,
            )?;
            trusted_data(
                &mut e,
                var(proto),
                1,
                var(message),
                var(empty),
                (true, false, true),
                &mut body,
            )?;
            publish_error_constructor(
                &mut e, allocate, objects[1], permanent, constructor_root, code,
                length_key, prototype, proto, &mut body,
            )?;
        }
        if uri_calls::prototype_used(e.lower) {
            let proto_entry = uri_calls::prototype_root(e.lower);
            let proto = e.boxed()?;
            body.push(assign(
                proto,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(3),
                        var(objects[2]),
                        int(2),
                        int(0),
                        int(0), int(0),
                        var(permanent),
                        int(proto_entry),
                    ],
                ),
                line,
            ));
            // All reused keys and the empty String are retained by old permanent
            // intrinsic property rows, even when the stage entries are reused.
            let uri_name = string(&mut e, OptionalError::Uri.name(), stage, 0, &mut body)?;
            trusted_data(
                &mut e,
                var(proto),
                0,
                var(name),
                var(uri_name),
                (true, false, true),
                &mut body,
            )?;
            trusted_data(
                &mut e,
                var(proto),
                1,
                var(message),
                var(empty),
                (true, false, true),
                &mut body,
            )?;
            if OptionalError::Uri.is_selected(e.lower) {
                let constructor_root = OptionalError::Uri.constructor_root(e.lower);
                publish_error_constructor(
                    &mut e, allocate, objects[1], permanent, constructor_root, URI_ERROR_CODE,
                    length_key, prototype, proto, &mut body,
                )?;
            }
            let function = e.boxed()?;
            for kind in 0..uri_calls::KIND_COUNT {
                if e.lower.uri_used.get() & (1 << kind) == 0 {
                    continue;
                }
                let entry = uri_calls::function_root(e.lower, kind);
                body.push(assign(
                    function,
                    call(
                        allocate,
                        vec![
                            var(carrier),
                            int(4),
                            var(objects[1]),
                            int(1),
                            int(uri_calls::FIRST_CODE + i64::from(kind)),
                            int(1), int(0),
                            var(permanent),
                            int(entry),
                        ],
                    ),
                    line,
                ));
                trusted_data(
                    &mut e,
                    var(function),
                    0,
                    var(length_key),
                    value::number(E::Float(1.0)),
                    (false, false, false),
                    &mut body,
                )?;
            }
        }
        let function = e.boxed()?;
        for (i, (name, length)) in METHODS.iter().enumerate() {
            let key = string(&mut e, name, stage, 0, &mut body)?;
            body.push(assign(
                function,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(4),
                        var(objects[1]),
                        int(1),
                        int(FIRST_METHOD_CODE + i as i64),
                        int(*length), int(0),
                        var(stage),
                        int(1),
                    ],
                ),
                line,
            ));
            trusted_data(
                &mut e,
                var(function),
                0,
                var(length_key),
                value::number(E::Float(*length as f64)),
                (false, false, false),
                &mut body,
            )?;
            trusted_data(
                &mut e,
                var(objects[4]),
                2 + i as i64,
                var(key),
                var(function),
                (true, false, true),
                &mut body,
            )?;
        }
        // Object.prototype valueOf/toString plus hasOwnProperty. The default
        // conversion pair is part of the ordinary prototype regardless of
        // `Math` use: the prototype method's key coercion must not depend on a
        // hidden numeric realm. Codes and row positions are unchanged.
        // The `toString` key node stays retained by this row; the gated
        // `Function.prototype.toString` row reuses it instead of allocating a
        // second eight-unit String.
        let mut object_to_string_key = None;
        for (i, (position, name, code)) in [
            (1, "valueOf", super::math::OBJECT_VALUE_OF),
            (2, "toString", super::math::OBJECT_TO_STRING),
            (3, HAS_OWN_PROPERTY_NAME, HAS_OWN_PROPERTY_CODE),
        ]
        .into_iter()
        .enumerate()
        {
            let key = string(&mut e, name, stage, 0, &mut body)?;
            if name == FUNCTION_TO_STRING_NAME {
                object_to_string_key = Some(key);
            }
            body.push(assign(
                function,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(4),
                        var(objects[1]),
                        int(PROTO_METHOD_CAPACITIES[i]),
                        int(code),
                        int(if code == HAS_OWN_PROPERTY_CODE { 1 } else { 0 }), int(0),
                        var(stage),
                        int(1),
                    ],
                ),
                line,
            ));
            trusted_data(
                &mut e,
                var(function),
                0,
                var(length_key),
                value::number(E::Float(if code == HAS_OWN_PROPERTY_CODE {
                    1.0
                } else {
                    0.0
                })),
                (false, false, false),
                &mut body,
            )?;
            trusted_data(
                &mut e,
                var(objects[0]),
                position,
                var(key),
                var(function),
                (true, false, true),
                &mut body,
            )?;
            if code == HAS_OWN_PROPERTY_CODE {
                body.push(assign(has_own_key, var(key), line));
                body.push(assign(has_own_fn, var(function), line));
            }
        }
        // Function.prototype.call: the first function-prototype method. It is
        // an ordinary own data row on Function.prototype, so the unchanged
        // ordinary Get finds it through the real prototype chain of every
        // function value. Only the admitted null-thisArg, no-extra-argument
        // shape succeeds; anything else keeps the existing throw.
        {
            let call_key = string(&mut e, "call", stage, 0, &mut body)?;
            body.push(assign(
                function,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(4),
                        var(objects[1]),
                        int(PROTO_METHOD_CAPACITIES[3]),
                        int(FUNCTION_CALL_CODE),
                        int(1), int(0),
                        var(stage),
                        int(1),
                    ],
                ),
                line,
            ));
            trusted_data(
                &mut e,
                var(function),
                0,
                var(length_key),
                value::number(E::Float(1.0)),
                (false, false, false),
                &mut body,
            )?;
            trusted_data(
                &mut e,
                var(objects[1]),
                1,
                var(call_key),
                var(function),
                (true, false, true),
                &mut body,
            )?;
        }
        // `Function.prototype.apply`: the second function-prototype method. It
        // is another ordinary own data row on Function.prototype at position
        // 2 (beside `length` at 0 and `call` at 1), so the unchanged ordinary
        // Get finds it through the real prototype chain of every function
        // value. Only the admitted null-thisArg, empty-array shape succeeds;
        // anything else keeps the existing throw.
        {
            let apply_key = string(&mut e, "apply", stage, 0, &mut body)?;
            body.push(assign(
                function,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(4),
                        var(objects[1]),
                        int(PROTO_METHOD_CAPACITIES[4]),
                        int(FUNCTION_APPLY_CODE),
                        int(2), int(0),
                        var(stage),
                        int(1),
                    ],
                ),
                line,
            ));
            trusted_data(
                &mut e,
                var(function),
                0,
                var(length_key),
                value::number(E::Float(2.0)),
                (false, false, false),
                &mut body,
            )?;
            trusted_data(
                &mut e,
                var(objects[1]),
                2,
                var(apply_key),
                var(function),
                (true, false, true),
                &mut body,
            )?;
        }
        // `Function.prototype.bind`: the third function-prototype method. It
        // is another ordinary own data row on Function.prototype at position
        // 3 (beside `length` at 0, `call` at 1 and `apply` at 2), so the
        // unchanged ordinary Get finds it through the real prototype chain
        // of every function value. Only the admitted null-thisArg shape
        // succeeds; anything else keeps the existing throw.
        {
            let bind_key = string(&mut e, "bind", stage, 0, &mut body)?;
            body.push(assign(
                function,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(4),
                        var(objects[1]),
                        int(PROTO_METHOD_CAPACITIES[5]),
                        int(FUNCTION_BIND_METHOD_CODE),
                        int(1), int(0),
                        var(stage),
                        int(1),
                    ],
                ),
                line,
            ));
            trusted_data(
                &mut e,
                var(function),
                0,
                var(length_key),
                value::number(E::Float(1.0)),
                (false, false, false),
                &mut body,
            )?;
            trusted_data(
                &mut e,
                var(objects[1]),
                3,
                var(bind_key),
                var(function),
                (true, false, true),
                &mut body,
            )?;
        }
        // `Function.prototype.name` is an accessor, not a data row. Upstream
        // installs one getter on the prototype, so every function resolves its
        // own stored name and `Function.prototype.name` itself throws
        // `not a function`. The getter is one private one-row function node
        // published into the stage frame and retained by the accessor row.
        if function_names {
            let getter = e.boxed()?;
            body.push(assign(
                getter,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(4),
                        var(objects[1]),
                        int(1),
                        int(FUNCTION_NAME_GET_CODE),
                        int(0), int(0),
                        var(stage),
                        int(2),
                    ],
                ),
                line,
            ));
            trusted_data(
                &mut e,
                var(getter),
                0,
                var(length_key),
                value::number(E::Float(0.0)),
                (false, false, false),
                &mut body,
            )?;
            trusted_accessor(
                &mut e,
                var(objects[1]),
                4,
                var(name),
                var(getter),
                (false, false),
                &mut body,
            )?;
        }
        // `Function.prototype.toString`: an ordinary data row after the
        // gated accessor, sharing the already retained `toString` key. Only a
        // source that can spell `.toString` pays the row and its function.
        if function_to_string {
            let to_string = e.boxed()?;
            body.push(assign(
                to_string,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(4),
                        var(objects[1]),
                        int(1),
                        int(FUNCTION_TO_STRING_CODE),
                        int(0), int(0),
                        var(stage),
                        int(3),
                    ],
                ),
                line,
            ));
            trusted_data(
                &mut e,
                var(to_string),
                0,
                var(length_key),
                value::number(E::Float(0.0)),
                (false, false, false),
                &mut body,
            )?;
            trusted_data(
                &mut e,
                var(objects[1]),
                4 + i64::from(function_names),
                var(object_to_string_key.expect("Object.prototype.toString key")),
                var(to_string),
                (true, false, true),
                &mut body,
            )?;
        }
        // `Function.prototype.prototype` is one inherited get/set accessor,
        // never a per-function creation row. Reading it on a source function
        // lazily materializes `{constructor: F, [[Prototype]]: Object.prototype}`
        // as that function's own writable/enumerable/configurable data row;
        // deleting that row makes the next read materialize a fresh object.
        // The getter and setter are two private two-row function nodes (the
        // `length` row only) published into the stage frame and retained by
        // the one accessor row, which reuses the fixed `prototype` key that
        // the `Function` constructor's own row already owns.
        if function_prototype {
            let getter = e.boxed()?;
            body.push(assign(
                getter,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(4),
                        var(objects[1]),
                        int(1),
                        int(FUNCTION_PROTOTYPE_GET_CODE),
                        int(0), int(0),
                        var(stage),
                        int(4),
                    ],
                ),
                line,
            ));
            trusted_data(
                &mut e,
                var(getter),
                0,
                var(length_key),
                value::number(E::Float(0.0)),
                (false, false, false),
                &mut body,
            )?;
            let setter = e.boxed()?;
            body.push(assign(
                setter,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(4),
                        var(objects[1]),
                        int(1),
                        int(FUNCTION_PROTOTYPE_SET_CODE),
                        int(0), int(0),
                        var(stage),
                        int(5),
                    ],
                ),
                line,
            ));
            trusted_data(
                &mut e,
                var(setter),
                0,
                var(length_key),
                value::number(E::Float(0.0)),
                (false, false, false),
                &mut body,
            )?;
            trusted_accessor_pair(
                &mut e,
                var(objects[1]),
                4 + i64::from(function_names) + i64::from(function_to_string),
                var(prototype),
                var(getter),
                var(setter),
                // The pinned engine publishes the inherited `prototype`
                // accessor as an enumerable, configurable own row of
                // `Function.prototype`: `Object.keys(Object.getPrototypeOf(F))`
                // contains it and `delete` removes it.
                (true, true),
                &mut body,
            )?;
        }
        // The private empty packed Array prototype. Its prototype is the
        // existing Object.prototype, so an Array inherits the ordinary
        // prototype methods through one real, separately mutable chain.
        {
            let alloc = e.temp(
                value::allocation_schema(),
                value::allocation(int(0), int(0), int(0)),
            )?;
            let base = e.integer()?;
            let proto = e.boxed()?;
            body.push(assign(proto, call(root, vec![var(carrier), int(0)]), line));
            body.push(assign(
                alloc,
                call(
                    h.node_begin,
                    vec![
                        var(carrier),
                        int(layout::ARRAY_HEADER_CELLS),
                        int(layout::KIND_ARRAY),
                    ],
                ),
                line,
            ));
            body.push(assign(base, field(var(alloc), "base"), line));
            for (offset, payload) in [
                (2, int(0)),
                (3, num(value::field(proto, "ref_slot"))),
                (4, num(value::field(proto, "generation"))),
                (layout::ARRAY_PRESENT_OFFSET, int(1)),
            ] {
                body.push(e.write(add(var(base), int(offset)), payload)?);
            }
            body.push(assign(
                function,
                call(
                    h.node_publish,
                    vec![
                        var(carrier),
                        field(var(alloc), "slot"),
                        field(var(alloc), "generation"),
                        var(base),
                        var(permanent),
                        int(array_prototype_root(e.lower)),
                    ],
                ),
                line,
            ));
            body.push(assign(array_proto_object, var(function), line));
        }
        // A separately rooted ordinary object supplies the prototype's method
        // namespace. Its ordinary rows are traced by the object GC walker;
        // neither packed payload offsets nor the Array length cell move.
        {
            let table = e.boxed()?;
            body.push(assign(table, call(allocate, vec![var(carrier), int(3), value::null(), int(ARRAY_PROTOTYPE_METHODS.len() as i64 + i64::from(function_prototype && e.lower.array_namespace_used.get())), int(0), int(0), int(0), var(permanent), int(array_method_table_root(e.lower))]), line));
            body.push(assign(array_table, var(table), line));
            for (row, (name, code, arity)) in ARRAY_PROTOTYPE_METHODS.into_iter().enumerate() {
                let key = string(&mut e, name, stage, 0, &mut body)?;
                body.push(assign(function, call(allocate, vec![var(carrier), int(4), var(objects[1]), int(1), int(code), int(arity), int(0), var(stage), int(1)]), line));
                trusted_data(&mut e, var(function), 0, var(length_key), value::number(E::Float(arity as f64)), (false, false, false), &mut body)?;
                trusted_data(&mut e, var(table), row as i64, var(key), var(function), (true, false, true), &mut body)?;
            }
        }
        // Three private primitive prototypes. String owns the shared
        // `hasOwnProperty` row plus the admitted public `charCodeAt`,
        // `charAt`, `codePointAt`, `concat`, `substring`, `slice`, `indexOf`,
        // `lastIndexOf` and `toString` rows
        // installed below; Number and Boolean keep one own non-enumerable
        // `hasOwnProperty` row each, on the already published code-267
        // function and its key String. `[[Prototype]]` is null on purpose:
        // this is a bounded bridge for those inherited methods, not the
        // general String/Number/Boolean prototype surface, so the mutable
        // Object.prototype overrides are never exposed through a primitive
        // receiver. A missing name therefore stays Undefined.
        for (i, entry) in [
            string_prototype_root(e.lower),
            number_prototype_root(e.lower),
            boolean_prototype_root(e.lower),
        ]
        .into_iter()
        .enumerate()
        {
            // The Number prototype owns a second row, its `toString`, only for
            // a source that can name it. Every realm that never mentions
            // `toString` keeps the exact one-row Number bridge and the exact
            // previous bootstrap request. When the `.prototype` accessor is
            // selected each existing prototype also gains one `constructor`
            // row naming its carrier again, so `Number.prototype.constructor
            // === Number` holds without materializing a second object.
            // A `.constructor` read installs the three back-link rows even
            // when the source never spells the matching carrier name, because
            // the receiver may be any primitive; that is why the row's
            // carrier condition includes `primitive_constructor_read`.
            let primitive_carrier = e.lower.primitive_constructor_read.get();
            let rows = match i {
                0 => {
                     // String prototype rows: the eighteen fixed rows plus
                    // every selected gated row, in installation order
                    // `replaceAll`, `search`, `match`, plus the gated
                    // `constructor` row.
                    18 + i64::from(e.lower.string_replace_all_used.get())
                        + i64::from(e.lower.string_search_used.get())
                        + i64::from(e.lower.string_match_used.get())
                        + i64::from(
                            function_prototype
                                && (e.lower.string_namespace_used.get() || primitive_carrier),
                        )
                }
                1 => {
                    1 + i64::from(e.lower.number_to_string_used.get())
                        + i64::from(
                            function_prototype
                                && (e.lower.number_namespace_used.get() || primitive_carrier),
                        )
                }
                _ => {
                    1 + i64::from(
                        function_prototype && (e.lower.boolean_used.get() || primitive_carrier),
                    )
                }
            };
            body.push(assign(
                objects[5 + i],
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(3),
                        value::null(),
                        int(rows),
                        int(0),
                        int(0), int(0),
                        var(permanent),
                        int(entry),
                    ],
                ),
                line,
            ));
            trusted_data(
                &mut e,
                var(objects[5 + i]),
                0,
                var(has_own_key),
                var(has_own_fn),
                (true, false, true),
                &mut body,
            )?;
        }
        // `String.prototype.charCodeAt`: the one admitted public String
        // prototype method. Its ordinary writable, non-enumerable,
        // configurable data row is installed on the String prototype, so the
        // unchanged ordinary Get finds it for primitive String receivers and
        // the canonical four-argument dispatcher reaches the private adapter.
        let char_code_at_key = string(&mut e, STRING_CHAR_CODE_AT_NAME, stage, 0, &mut body)?;
        body.push(assign(
            function,
            call(
                allocate,
                vec![
                    var(carrier),
                    int(4),
                    var(objects[1]),
                    int(1),
                    int(STRING_CHAR_CODE_AT_CODE),
                    int(1), int(0),
                    var(stage),
                    int(1),
                ],
            ),
            line,
        ));
        trusted_data(
            &mut e,
            var(function),
            0,
            var(length_key),
            value::number(E::Float(1.0)),
            (false, false, false),
            &mut body,
        )?;
        trusted_data(
            &mut e,
            var(objects[5]),
            1,
            var(char_code_at_key),
            var(function),
            (true, false, true),
            &mut body,
        )?;
        // `String.prototype.charAt`: same writable, non-enumerable,
        // configurable data row, arity 1 and length 1. Retained by the
        // prototype row, not a new permanent root.
        let char_at_key = string(&mut e, STRING_CHAR_AT_NAME, stage, 0, &mut body)?;
        body.push(assign(
            function,
            call(
                allocate,
                vec![
                    var(carrier),
                    int(4),
                    var(objects[1]),
                    int(1),
                    int(STRING_CHAR_AT_CODE),
                    int(1), int(0),
                    var(stage),
                    int(1),
                ],
            ),
            line,
        ));
        trusted_data(
            &mut e,
            var(function),
            0,
            var(length_key),
            value::number(E::Float(1.0)),
            (false, false, false),
            &mut body,
        )?;
        trusted_data(
            &mut e,
            var(objects[5]),
            2,
            var(char_at_key),
            var(function),
            (true, false, true),
            &mut body,
        )?;
        // `String.prototype.codePointAt`: same writable, non-enumerable,
        // configurable data row, arity 1 and length 1. Retained by the
        // prototype row, not a new permanent root.
        let code_point_at_key = string(&mut e, STRING_CODE_POINT_AT_NAME, stage, 0, &mut body)?;
        body.push(assign(
            function,
            call(
                allocate,
                vec![
                    var(carrier),
                    int(4),
                    var(objects[1]),
                    int(1),
                    int(STRING_CODE_POINT_AT_CODE),
                    int(1), int(0),
                    var(stage),
                    int(1),
                ],
            ),
            line,
        ));
        trusted_data(
            &mut e,
            var(function),
            0,
            var(length_key),
            value::number(E::Float(1.0)),
            (false, false, false),
            &mut body,
        )?;
        trusted_data(
            &mut e,
            var(objects[5]),
            3,
            var(code_point_at_key),
            var(function),
            (true, false, true),
            &mut body,
        )?;
        // `concat` is the same one-row function shape as charAt: capacity 1,
        // arity 1, length 1, error false, length-row flags false/false/false,
        // prototype row writable/non-enumerable/configurable true/false/true.
        // The six-unit name publishes into the existing stage entry 0. No new
        // permanent root.
        let concat_key = string(&mut e, STRING_CONCAT_NAME, stage, 0, &mut body)?;
        body.push(assign(
            function,
            call(
                allocate,
                vec![
                    var(carrier),
                    int(4),
                    var(objects[1]),
                    int(1),
                    int(STRING_CONCAT_CODE),
                    int(1), int(0),
                    var(stage),
                    int(1),
                ],
            ),
            line,
        ));
        trusted_data(
            &mut e,
            var(function),
            0,
            var(length_key),
            value::number(E::Float(1.0)),
            (false, false, false),
            &mut body,
        )?;
        trusted_data(
            &mut e,
            var(objects[5]),
            4,
            var(concat_key),
            var(function),
            (true, false, true),
            &mut body,
        )?;
        // `substring` is the same one-row function shape: capacity 1, arity 2,
        // length 2, error false, length-row flags false/false/false, prototype
        // row writable/non-enumerable/configurable true/false/true. The
        // nine-unit name publishes into the existing stage entry 0. No new
        // permanent root.
        let substring_key = string(&mut e, STRING_SUBSTRING_NAME, stage, 0, &mut body)?;
        body.push(assign(
            function,
            call(
                allocate,
                vec![
                    var(carrier),
                    int(4),
                    var(objects[1]),
                    int(1),
                    int(STRING_SUBSTRING_CODE),
                    int(2), int(0),
                    var(stage),
                    int(1),
                ],
            ),
            line,
        ));
        trusted_data(
            &mut e,
            var(function),
            0,
            var(length_key),
            value::number(E::Float(2.0)),
            (false, false, false),
            &mut body,
        )?;
        trusted_data(
            &mut e,
            var(objects[5]),
            5,
            var(substring_key),
            var(function),
            (true, false, true),
            &mut body,
        )?;
        // `slice` reuses the same one-row function shape: capacity 1, arity 2,
        // length 2, error false, length-row flags false/false/false, prototype
        // row writable/non-enumerable/configurable true/false/true. The
        // five-unit name publishes into the existing stage entry 0. No new
        // permanent root.
        let slice_key = string(&mut e, STRING_SLICE_NAME, stage, 0, &mut body)?;
        body.push(assign(
            function,
            call(
                allocate,
                vec![
                    var(carrier),
                    int(4),
                    var(objects[1]),
                    int(1),
                    int(STRING_SLICE_CODE),
                    int(2), int(0),
                    var(stage),
                    int(1),
                ],
            ),
            line,
        ));
        trusted_data(
            &mut e,
            var(function),
            0,
            var(length_key),
            value::number(E::Float(2.0)),
            (false, false, false),
            &mut body,
        )?;
        trusted_data(
            &mut e,
            var(objects[5]),
            6,
            var(slice_key),
            var(function),
            (true, false, true),
            &mut body,
        )?;
        // `indexOf` is row 7 on the same String prototype. Capacity 1, private
        // arity 2, public length 1, error false. The seven-unit name publishes
        // into the existing stage entry 0. No new permanent root, and the
        // eight-entry stage frame is unchanged.
        let indexof_key = string(&mut e, STRING_INDEXOF_NAME, stage, 0, &mut body)?;
        body.push(assign(
            function,
            call(
                allocate,
                vec![
                    var(carrier),
                    int(4),
                    var(objects[1]),
                    int(1),
                    int(STRING_INDEXOF_CODE),
                    int(2), int(0),
                    var(stage),
                    int(1),
                ],
            ),
            line,
        ));
        trusted_data(
            &mut e,
            var(function),
            0,
            var(length_key),
            value::number(E::Float(1.0)),
            (false, false, false),
            &mut body,
        )?;
        trusted_data(
            &mut e,
            var(objects[5]),
            7,
            var(indexof_key),
            var(function),
            (true, false, true),
            &mut body,
        )?;
        // `lastIndexOf` is row 8 on the same String prototype. Capacity 1,
        // private arity 2, public length 1, error false. The eleven-unit name
        // publishes into the existing stage entry 0. No new permanent root,
        // and the eight-entry stage frame is unchanged.
        let last_index_of_key = string(&mut e, STRING_LAST_INDEX_OF_NAME, stage, 0, &mut body)?;
        body.push(assign(
            function,
            call(
                allocate,
                vec![
                    var(carrier),
                    int(4),
                    var(objects[1]),
                    int(1),
                    int(STRING_LAST_INDEX_OF_CODE),
                    int(2), int(0),
                    var(stage),
                    int(1),
                ],
            ),
            line,
        ));
        trusted_data(
            &mut e,
            var(function),
            0,
            var(length_key),
            value::number(E::Float(1.0)),
            (false, false, false),
            &mut body,
        )?;
        trusted_data(
            &mut e,
            var(objects[5]),
            8,
            var(last_index_of_key),
            var(function),
            (true, false, true),
            &mut body,
        )?;
        // `toString` is row 9 on the same String prototype. Capacity 1,
        // private arity 0, public length 0, error false. The eight-unit name
        // is a new stage-0 String, not the Object.prototype.toString key.
        // The function publishes at stage 1 and is then installed. No new
        // permanent root, and the eight-entry stage frame is unchanged.
        let to_string_key = string(&mut e, STRING_TO_STRING_NAME, stage, 0, &mut body)?;
        body.push(assign(
            function,
            call(
                allocate,
                vec![
                    var(carrier),
                    int(4),
                    var(objects[1]),
                    int(1),
                    int(STRING_TO_STRING_CODE),
                    int(0), int(0),
                    var(stage),
                    int(1),
                ],
            ),
            line,
        ));
        trusted_data(
            &mut e,
            var(function),
            0,
            var(length_key),
            value::number(E::Float(0.0)),
            (false, false, false),
            &mut body,
        )?;
        trusted_data(
            &mut e,
            var(objects[5]),
            9,
            var(to_string_key),
            var(function),
            (true, false, true),
            &mut body,
        )?;
        // `Error.prototype.toString` is row 2 on the realm Error prototype,
        // the first of the two reserved rows bought when the prototype was
        // allocated. Capacity 1, private arity 0, public length 0, error
        // false. It reuses the `toString` key String just published for the
        // String prototype row, so it adds one function node and no key.
        // The row is inside the existing four-row capacity, so the object
        // node itself is not republished or reallocated. No new root.
        body.push(assign(
            function,
            call(
                allocate,
                vec![
                    var(carrier),
                    int(4),
                    var(objects[1]),
                    int(1),
                    int(ERROR_TO_STRING_CODE),
                    int(0), int(0),
                    var(stage),
                    int(1),
                ],
            ),
            line,
        ));
        trusted_data(
            &mut e,
            var(function),
            0,
            var(length_key),
            value::number(E::Float(0.0)),
            (false, false, false),
            &mut body,
        )?;
        trusted_data(
            &mut e,
            var(objects[2]),
            2,
            var(to_string_key),
            var(function),
            (true, false, true),
            &mut body,
        )?;
        // `Number.prototype.toString` is row 1 on the private Number
        // prototype, directly after the shared `hasOwnProperty` row. Capacity
        // 1, private arity 0, public length 1 (ES5.1 15.7.4.2), error false:
        // the dispatcher reads the argument container, so the private arity
        // stays 0 while the observable `length` matches the specification. It
        // reuses the `toString` key String published for the String prototype
        // row above, so the row costs seven cells and the function node twenty,
        // never a second key. Only a source that names `toString` pays them.
        if e.lower.number_to_string_used.get() {
            body.push(assign(
                function,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(4),
                        var(objects[1]),
                        int(1),
                        int(NUMBER_TO_STRING_CODE),
                        int(0), int(0),
                        var(stage),
                        int(1),
                    ],
                ),
                line,
            ));
            trusted_data(
                &mut e,
                var(function),
                0,
                var(length_key),
                value::number(E::Float(1.0)),
                (false, false, false),
                &mut body,
            )?;
            trusted_data(
                &mut e,
                var(objects[6]),
                1,
                var(to_string_key),
                var(function),
                (true, false, true),
                &mut body,
            )?;
        }
        // `toLowerCase` is row 10. Capacity 1, private arity 0, public length
        // 0, error false. The eleven-unit name is a new stage-0 String. The
        // function publishes at stage 1 and is then installed. No new
        // permanent root, and the eight-entry stage frame is unchanged.
        let to_lower_key = string(&mut e, STRING_TO_LOWER_CASE_NAME, stage, 0, &mut body)?;
        body.push(assign(
            function,
            call(
                allocate,
                vec![
                    var(carrier),
                    int(4),
                    var(objects[1]),
                    int(1),
                    int(STRING_TO_LOWER_CASE_CODE),
                    int(0), int(0),
                    var(stage),
                    int(1),
                ],
            ),
            line,
        ));
        trusted_data(
            &mut e,
            var(function),
            0,
            var(length_key),
            value::number(E::Float(0.0)),
            (false, false, false),
            &mut body,
        )?;
        trusted_data(
            &mut e,
            var(objects[5]),
            10,
            var(to_lower_key),
            var(function),
            (true, false, true),
            &mut body,
        )?;
        // `toUpperCase` is row 11. Same capacity, arity, length and stage
        // reuse as `toLowerCase`.
        let to_upper_key = string(&mut e, STRING_TO_UPPER_CASE_NAME, stage, 0, &mut body)?;
        body.push(assign(
            function,
            call(
                allocate,
                vec![
                    var(carrier),
                    int(4),
                    var(objects[1]),
                    int(1),
                    int(STRING_TO_UPPER_CASE_CODE),
                    int(0), int(0),
                    var(stage),
                    int(1),
                ],
            ),
            line,
        ));
        trusted_data(
            &mut e,
            var(function),
            0,
            var(length_key),
            value::number(E::Float(0.0)),
            (false, false, false),
            &mut body,
        )?;
        trusted_data(
            &mut e,
            var(objects[5]),
            11,
            var(to_upper_key),
            var(function),
            (true, false, true),
            &mut body,
        )?;
        // `trim`, `trimEnd` and `trimStart` are rows 12..14. Same capacity,
        // arity, length and stage reuse as `toLowerCase`. Codes 289..291.
        for (row, code, name) in [
            (12_i64, STRING_TRIM_CODE, STRING_TRIM_NAME),
            (13_i64, STRING_TRIM_END_CODE, STRING_TRIM_END_NAME),
            (14_i64, STRING_TRIM_START_CODE, STRING_TRIM_START_NAME),
            (15_i64, STRING_REPLACE_CODE, STRING_REPLACE_NAME),
            (16_i64, STRING_SPLIT_CODE, STRING_SPLIT_NAME),
            (17_i64, STRING_REPEAT_CODE, STRING_REPEAT_NAME),
        ] {
            let key = string(&mut e, name, stage, 0, &mut body)?;
            body.push(assign(
                function,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(4),
                        var(objects[1]),
                        int(1),
                        int(code),
                        int(if code == STRING_REPLACE_CODE || code == STRING_SPLIT_CODE {
                            2
                        } else if code == STRING_REPEAT_CODE {
                            1
                        } else {
                            0
                        }), int(0),
                        var(stage),
                        int(1),
                    ],
                ),
                line,
            ));
            trusted_data(
                &mut e,
                var(function),
                0,
                var(length_key),
                value::number(E::Float(if code == STRING_REPLACE_CODE || code == STRING_SPLIT_CODE {
                    2.0
                } else if code == STRING_REPEAT_CODE {
                    1.0
                } else {
                    0.0
                })),
                (false, false, false),
                &mut body,
            )?;
            trusted_data(
                &mut e,
                var(objects[5]),
                row,
                var(key),
                var(function),
                (true, false, true),
                &mut body,
            )?;
        }
        // `String.prototype.replaceAll` is row 18. Like the optional
        // `Number.prototype.toString` row, it is installed only when the
        // source can name the method; every other program keeps the exact
        // eighteen-row String prototype and the previous bootstrap request.
        // Public length 2, private arity 2, one ordinary data row.
        if e.lower.string_replace_all_used.get() {
            let key = string(&mut e, STRING_REPLACE_ALL_NAME, stage, 0, &mut body)?;
            body.push(assign(
                function,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(4),
                        var(objects[1]),
                        int(1),
                        int(STRING_REPLACE_ALL_CODE),
                        int(2), int(0),
                        var(stage),
                        int(1),
                    ],
                ),
                line,
            ));
            trusted_data(
                &mut e,
                var(function),
                0,
                var(length_key),
                value::number(E::Float(2.0)),
                (false, false, false),
                &mut body,
            )?;
            trusted_data(
                &mut e,
                var(objects[5]),
                18,
                var(key),
                var(function),
                (true, false, true),
                &mut body,
            )?;
        }
        // `String.prototype.search` is the next free String-prototype row, at
        // 18 or 19 depending on `replaceAll`, and exactly what the capacity
        // expression above reserves. Like `replaceAll` it is installed only
        // when the source can name the method; every other program keeps the
        // exact previous String prototype and bootstrap request. Public
        // length 1, private arity 1, one ordinary data row.
        if e.lower.string_search_used.get() {
            let key = string(&mut e, STRING_SEARCH_NAME, stage, 0, &mut body)?;
            let row = 18 + i64::from(e.lower.string_replace_all_used.get());
            body.push(assign(
                function,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(4),
                        var(objects[1]),
                        int(1),
                        int(STRING_SEARCH_CODE),
                        int(1), int(0),
                        var(stage),
                        int(1),
                    ],
                ),
                line,
            ));
            trusted_data(
                &mut e,
                var(function),
                0,
                var(length_key),
                value::number(E::Float(1.0)),
                (false, false, false),
                &mut body,
            )?;
            trusted_data(
                &mut e,
                var(objects[5]),
                row,
                var(key),
                var(function),
                (true, false, true),
                &mut body,
            )?;
        }
        // `String.prototype.match` is the next free String-prototype row,
        // after `replaceAll` and `search`, and exactly what the capacity
        // expression above reserves. Like `search` it is installed only when
        // the source can name the method; every other program keeps the exact
        // previous String prototype and bootstrap request. Public length 1,
        // private arity 1, one ordinary data row.
        if e.lower.string_match_used.get() {
            let key = string(&mut e, STRING_MATCH_NAME, stage, 0, &mut body)?;
            let row = 18
                + i64::from(e.lower.string_replace_all_used.get())
                + i64::from(e.lower.string_search_used.get());
            body.push(assign(
                function,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(4),
                        var(objects[1]),
                        int(1),
                        int(STRING_MATCH_CODE),
                        int(1), int(0),
                        var(stage),
                        int(1),
                    ],
                ),
                line,
            ));
            trusted_data(
                &mut e,
                var(function),
                0,
                var(length_key),
                value::number(E::Float(1.0)),
                (false, false, false),
                &mut body,
            )?;
            trusted_data(
                &mut e,
                var(objects[5]),
                row,
                var(key),
                var(function),
                (true, false, true),
                &mut body,
            )?;
        }
        // `JSON`: one ordinary object at the tail permanent entry whose own
        // data rows are the admitted `stringify` and `parse` functions. The
        // unchanged ordinary Get finds each function on the real object, so no
        // call-site fold and no member-name special case is needed anywhere in
        // the lowerer. Only a source that names `JSON` installs it; every other
        // program keeps the exact previous manifest, frame capacity and root
        // indices.
        if e.lower.json_used.get() {
            let json = e.boxed()?;
            body.push(assign(
                json,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(3),
                        var(objects[0]),
                        int(2),
                        int(0),
                        int(0), int(0),
                        var(permanent),
                        int(json_root(e.lower)),
                    ],
                ),
                line,
            ));
            let stringify = string(&mut e, "stringify", stage, 0, &mut body)?;
            body.push(assign(
                function,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(4),
                        var(objects[1]),
                        int(1),
                        int(JSON_STRINGIFY_CODE),
                        int(1), int(0),
                        var(stage),
                        int(1),
                    ],
                ),
                line,
            ));
            trusted_data(
                &mut e,
                var(function),
                0,
                var(length_key),
                value::number(E::Float(1.0)),
                (false, false, false),
                &mut body,
            )?;
            trusted_data(
                &mut e,
                var(json),
                0,
                var(stringify),
                var(function),
                (true, false, true),
                &mut body,
            )?;
            // The second own row is `parse`, installed the same ordinary way:
            // its key String publishes into stage entry 0 and its one-row
            // function into stage entry 2, so the already written `stringify`
            // row is never disturbed.
            let parse = string(&mut e, "parse", stage, 0, &mut body)?;
            body.push(assign(
                function,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(4),
                        var(objects[1]),
                        int(1),
                        int(JSON_PARSE_CODE),
                        int(1), int(0),
                        var(stage),
                        int(2),
                    ],
                ),
                line,
            ));
            trusted_data(
                &mut e,
                var(function),
                0,
                var(length_key),
                value::number(E::Float(1.0)),
                (false, false, false),
                &mut body,
            )?;
            trusted_data(
                &mut e,
                var(json),
                1,
                var(parse),
                var(function),
                (true, false, true),
                &mut body,
            )?;
        }
        if e.lower.math_used.get() {
            super::math::bootstrap(
                &mut e,
                permanent,
                stage,
                var(objects[0]),
                var(objects[1]),
                allocate,
                var(length_key),
                &mut body,
            )?;
        }
        super::math::bootstrap_globals(
            &mut e,
            permanent,
            stage,
            var(objects[1]),
            allocate,
            var(length_key),
            &mut body,
        )?;
        // A `.constructor` read selects the Number carrier even when the
        // source never spells `Number`: the private Number prototype's
        // back-link row must name a real function of that class. The minimal
        // carrier owns only `length` and its own `prototype` row, is rooted in
        // the stage frame while bootstrap runs, and afterwards is kept alive
        // by the prototype row it is published into, so it opens no permanent
        // root entry and every earlier root index stays fixed.
        let primitive_carrier = e.lower.primitive_constructor_read.get();
        if e.lower.number_namespace_used.get() || primitive_carrier {
            let full = e.lower.number_namespace_used.get();
            let rows = if full { NUMBER_NAMESPACE_ROWS } else { 2 };
            let namespace = e.boxed()?;
            body.push(assign(
                namespace,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(4),
                        var(objects[1]),
                        int(rows),
                        int(NUMBER_NAMESPACE_CODE),
                        int(1), int(0),
                        if full { var(permanent) } else { var(stage) },
                        int(if full {
                            number_namespace_root(e.lower)
                        } else {
                            PRIMITIVE_CARRIER_STAGE_SLOT
                        }),
                    ],
                ),
                line,
            ));
            trusted_data(
                &mut e,
                var(namespace),
                0,
                var(length_key),
                value::number(E::Float(1.0)),
                (false, false, false),
                &mut body,
            )?;
            for (i, (name, number)) in NUMBER_CONSTANTS
                .into_iter()
                .enumerate()
                .filter(|_| full)
            {
                let key = string(&mut e, name, stage, 0, &mut body)?;
                trusted_data(
                    &mut e,
                    var(namespace),
                    i as i64 + 1,
                    var(key),
                    value::number(E::Float(number)),
                    (false, false, false),
                    &mut body,
                )?;
            }
            // Share the bare global's function object when that root exists.
            // Otherwise the stage frame owns the only copy; do not open a bare root.
            debug_assert_eq!(super::math::GLOBALS[0].0, "parseInt");
            debug_assert_eq!(super::math::GLOBALS[1].0, "parseFloat");
            let parse_fn = e.boxed()?;
            for (i, (name, length)) in super::math::GLOBALS
                .iter()
                .take(NUMBER_PARSE_METHODS)
                .enumerate()
                .filter(|_| full)
            {
                let key = string(&mut e, name, stage, 0, &mut body)?;
                if e.lower.numeric_globals.get() & (1 << i) != 0 {
                    body.push(assign(
                        parse_fn,
                        call(
                            root,
                            vec![
                                var(carrier),
                                int(super::math::global_root(e.lower, i as u8)),
                            ],
                        ),
                        line,
                    ));
                } else {
                    body.push(assign(
                        parse_fn,
                        call(
                            allocate,
                            vec![
                                var(carrier),
                                int(4),
                                var(objects[1]),
                                int(1),
                                int(super::math::global_code(i)),
                                int(*length), int(0),
                                var(stage),
                                int(2),
                            ],
                        ),
                        line,
                    ));
                    trusted_data(
                        &mut e,
                        var(parse_fn),
                        0,
                        var(length_key),
                        value::number(E::Float(*length as f64)),
                        (false, false, false),
                        &mut body,
                    )?;
                }
                trusted_data(
                    &mut e,
                    var(namespace),
                    NUMBER_CONSTANTS.len() as i64 + 1 + i as i64,
                    var(key),
                    var(parse_fn),
                    (true, false, true),
                    &mut body,
                )?;
            }
            // `Number.prototype.constructor === Number` on the private Number
            // prototype. The row reuses the fixed `constructor` key String the
            // Object prototype already retains, so only the row is charged.
            if function_prototype {
                let constructor_row = 1 + i64::from(e.lower.number_to_string_used.get());
                trusted_data(
                    &mut e,
                    var(objects[6]),
                    constructor_row,
                    var(constructor),
                    var(namespace),
                    (true, true, true),
                    &mut body,
                )?;
            }
            // The carrier's own `prototype` row exists from bootstrap, not
            // only after the inherited accessor's first read:
            // `Number.hasOwnProperty("prototype")` and `Object.keys(Number)`
            // observe it, and `delete Number.prototype` removes a real row
            // that the inherited accessor never resurrects.
            let number_proto_entry = number_prototype_root(e.lower);
            trusted_data(
                &mut e,
                var(namespace),
                rows - 1,
                var(prototype),
                call(root, vec![var(carrier), int(number_proto_entry)]),
                (true, true, true),
                &mut body,
            )?;
        }
        // The String carrier takes the same two shapes: the full namespace
        // when the source spells `String`, or the minimal two-row carrier a
        // `.constructor` read needs, rooted in the stage frame while the
        // private String prototype's back-link row is published.
        if e.lower.string_namespace_used.get() || primitive_carrier {
            let full = e.lower.string_namespace_used.get();
            let rows = if full { STRING_NAMESPACE_ROWS } else { 2 };
            let namespace = e.boxed()?;
            body.push(assign(
                namespace,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(4),
                        var(objects[1]),
                        int(rows),
                        int(STRING_NAMESPACE_CODE),
                        int(1), int(0),
                        if full { var(permanent) } else { var(stage) },
                        int(if full {
                            string_namespace_root(e.lower)
                        } else {
                            PRIMITIVE_CARRIER_STAGE_SLOT
                        }),
                    ],
                ),
                line,
            ));
            trusted_data(
                &mut e,
                var(namespace),
                0,
                var(length_key),
                value::number(E::Float(1.0)),
                (false, false, false),
                &mut body,
            )?;
            for (i, (name, code)) in [
                (STRING_FROM_CHAR_CODE_NAME, STRING_FROM_CHAR_CODE_CODE),
                (STRING_FROM_CODE_POINT_NAME, STRING_FROM_CODE_POINT_CODE),
            ]
            .into_iter()
            .enumerate()
            .filter(|_| full)
            {
                let key = string(&mut e, name, stage, 0, &mut body)?;
                let function = e.boxed()?;
                body.push(assign(
                    function,
                    call(
                        allocate,
                        vec![
                            var(carrier),
                            int(4),
                            var(objects[1]),
                            int(1),
                            int(code),
                            int(1), int(0),
                            var(stage),
                            int(1),
                        ],
                    ),
                    line,
                ));
                trusted_data(
                    &mut e,
                    var(function),
                    0,
                    var(length_key),
                    value::number(E::Float(1.0)),
                    (false, false, false),
                    &mut body,
                )?;
                trusted_data(
                    &mut e,
                    var(namespace),
                    1 + i as i64,
                    var(key),
                    var(function),
                    (true, false, true),
                    &mut body,
                )?;
            }
            // `String.prototype.constructor === String` on the private String
            // prototype, again reusing the fixed `constructor` key String.
            if function_prototype {
                let constructor_row = 18
                    + i64::from(e.lower.string_replace_all_used.get())
                    + i64::from(e.lower.string_search_used.get())
                    + i64::from(e.lower.string_match_used.get());
                trusted_data(
                    &mut e,
                    var(objects[5]),
                    constructor_row,
                    var(constructor),
                    var(namespace),
                    (true, true, true),
                    &mut body,
                )?;
            }
            // The String carrier owns its bootstrap `prototype` row exactly
            // like Number: it is an ordinary enumerable, writable and
            // configurable row, never a lazily materialized accessor answer.
            let string_proto_entry = string_prototype_root(e.lower);
            trusted_data(
                &mut e,
                var(namespace),
                rows - 1,
                var(prototype),
                call(root, vec![var(carrier), int(string_proto_entry)]),
                (true, true, true),
                &mut body,
            )?;
        }
        if e.lower.array_buffer_used.get() {
            super::array_buffer::bootstrap(&mut e, allocate, permanent, stage,
                (objects[0], objects[1]), (length_key, prototype, constructor), &mut body)?;
        }
        if e.lower.typed_array_used() {
            super::typed_array::bootstrap(&mut e, allocate, permanent, stage,
                (objects[0], objects[1]), (length_key, prototype, constructor), &mut body)?;
        }
        if e.lower.array_namespace_used.get() {
            let namespace = e.boxed()?;
            body.push(assign(
                namespace,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(4),
                        var(objects[1]),
                        int(ARRAY_NAMESPACE_ROWS),
                        int(ARRAY_NAMESPACE_CODE),
                        int(1), int(0),
                        var(permanent),
                        int(array_namespace_root(e.lower)),
                    ],
                ),
                line,
            ));
            trusted_data(
                &mut e,
                var(namespace),
                0,
                var(length_key),
                value::number(E::Float(1.0)),
                (false, false, false),
                &mut body,
            )?;
            // The carrier's own `prototype` row is the private empty packed
            // Array prototype itself, so `Array.prototype` and
            // `Object.getPrototypeOf(new Array())` are the same node. The row
            // is an ordinary bootstrap row: enumerable, writable and
            // configurable, so `hasOwnProperty`, `Object.keys` and `delete`
            // all observe the same row real engines do.
            trusted_data(
                &mut e,
                var(namespace),
                1,
                var(prototype),
                var(array_proto_object),
                (true, true, true),
                &mut body,
            )?;
            let key = string(&mut e, ARRAY_IS_ARRAY_NAME, stage, 0, &mut body)?;
            let function = e.boxed()?;
            body.push(assign(
                function,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(4),
                        var(objects[1]),
                        int(1),
                        int(ARRAY_IS_ARRAY_CODE),
                        int(1), int(0),
                        var(stage),
                        int(1),
                    ],
                ),
                line,
            ));
            trusted_data(
                &mut e,
                var(function),
                0,
                var(length_key),
                value::number(E::Float(1.0)),
                (false, false, false),
                &mut body,
            )?;
            trusted_data(
                &mut e,
                var(namespace),
                2,
                var(key),
                var(function),
                (true, false, true),
                &mut body,
            )?;
            // The method table's `constructor` row names the carrier again, so
            // `Array.prototype.constructor === Array` (through the table the
            // array path already consults) and `[].constructor === Array` hold.
            // The row is an ordinary enumerable, writable, configurable data
            // row; `Object.keys` and `delete` reach it through the same
            // Array-prototype property view as `hasOwnProperty`.
            if function_prototype {
                trusted_data(
                    &mut e,
                    var(array_table),
                    ARRAY_PROTOTYPE_METHODS.len() as i64,
                    var(constructor),
                    var(namespace),
                    (true, true, true),
                    &mut body,
                )?;
            }
        }
        // Boolean already owns the minimal two-row shape; only the owner
        // differs when a `.constructor` read selects it without the global.
        if e.lower.boolean_used.get() || primitive_carrier {
            let full = e.lower.boolean_used.get();
            let function = e.boxed()?;
            body.push(assign(
                function,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(4),
                        var(objects[1]),
                        int(2),
                        int(BOOLEAN_CODE),
                        int(1), int(0),
                        if full { var(permanent) } else { var(stage) },
                        int(if full {
                            boolean_constructor_root(e.lower)
                        } else {
                            PRIMITIVE_CARRIER_STAGE_SLOT
                        }),
                    ],
                ),
                line,
            ));
            trusted_data(
                &mut e,
                var(function),
                0,
                var(length_key),
                value::number(E::Float(1.0)),
                (false, false, false),
                &mut body,
            )?;
            // `Boolean.prototype.constructor === Boolean` on the private
            // Boolean prototype, reusing the fixed `constructor` key String.
            if function_prototype {
                trusted_data(
                    &mut e,
                    var(objects[7]),
                    1,
                    var(constructor),
                    var(function),
                    (true, true, true),
                    &mut body,
                )?;
            }
            // The Boolean carrier's own bootstrap `prototype` row.
            let boolean_proto_entry = boolean_prototype_root(e.lower);
            trusted_data(
                &mut e,
                var(function),
                1,
                var(prototype),
                call(root, vec![var(carrier), int(boolean_proto_entry)]),
                (true, true, true),
                &mut body,
            )?;
        }
        body.push(e.effect(h.root_leave, vec![var(carrier), var(stage)])?);
        let body = e.finish(body);
        declare(
            ir,
            stmts,
            BOOTSTRAP_NAME,
            bootstrap,
            vec![carrier],
            vec![carrier_schema.clone()],
            T::Null,
            body,
            E::Null,
            line,
        );
    }
    // Hold the empty String in entry 0 across node_begin. Error publication
    // replaces that root only after writing the internal String edge.
    // The intrinsics block `start..=ascii_init` is reserved before any of its
    // slots is declared, so this lazy allocation must clear the whole block:
    // `fresh_index` alone only sees the declared maximum and would hand out
    // `type_error` (start + 3) while that slot is still pending.
    // Hold the empty String in entry 0 across node_begin. Error publication
    // replaces that root only after writing the internal String edge.
    // The intrinsics block `start..=ascii_init` is reserved before any of its
    // slots is declared, so this lazy allocation must clear the whole block:
    // `fresh_index` alone only sees the declared maximum and would hand out
    // `type_error` (start + 3) while that slot is still pending.
    let empty_error = super::heap::fresh_index_above(lower, ir, span, ascii_init)?;
    {
        let carrier = lower.slot(span)?;
        let proto = lower.slot(span)?;
        let mut e = Emit::new(lower, span, carrier)?;
        let frame = e.integer()?;
        let alloc = e.temp(value::allocation_schema(), value::allocation(int(0), int(0), int(0)))?;
        let base = e.integer()?;
        let result = e.boxed()?;
        let mut body = vec![assign(frame, call(h.root_enter, vec![var(carrier), int(1)]), line)];
        let empty = string(&mut e, "", frame, 0, &mut body)?;
        body.extend([
            assign(alloc, call(h.node_begin, vec![var(carrier), int(layout::ERROR_HEADER_CELLS), int(layout::KIND_OBJECT)]), line),
            assign(base, field(var(alloc), "base"), line),
        ]);
        for (offset, payload) in [
            (2, int(layout::OBJECT_EXTENSIBLE_FLAG | layout::OBJECT_LEGACY_ERROR_FLAG | layout::OBJECT_ERROR_INSTANCE_FLAG)),
            (3, field(var(proto), "ref_slot")),
            (4, field(var(proto), "generation")),
            (layout::ERROR_MESSAGE_OFFSET, field(var(empty), "ref_slot")),
            (layout::ERROR_MESSAGE_OFFSET + 1, field(var(empty), "generation")),
        ] {
            body.push(e.write(add(var(base), int(offset)), payload)?);
        }
        body.extend([
            assign(result, call(h.node_publish, vec![var(carrier), field(var(alloc), "slot"), field(var(alloc), "generation"), var(base), var(frame), int(0)]), line),
            e.effect(h.root_leave, vec![var(carrier), var(frame)])?,
        ]);
        declare(
            ir, stmts, "__compat_es5_new_empty_error", empty_error,
            vec![carrier, proto], vec![carrier_schema.clone(), value::schema()],
            value::schema(), e.finish(body), var(result), line,
        );
    }
    {
        let carrier = lower.slot(span)?;
        declare(
            ir,
            stmts,
            TYPE_ERROR_NAME,
            type_error,
            vec![carrier],
            vec![carrier_schema.clone()],
            value::schema(),
            Vec::new(),
            call(empty_error, vec![var(carrier), call(root, vec![var(carrier), int(3)])]),
            line,
        );
    }
    {
        let carrier = lower.slot(span)?;
        let mut e = Emit::new(lower, span, carrier)?;
        let prototype = e.boxed()?;
        let base = e.integer()?;
        let row = e.integer()?;
        let key = e.boxed()?;
        let key_base = e.integer()?;
        let mut body = vec![
            assign(prototype, call(root, vec![var(carrier), int(1)]), line),
            assign(base, e.base(var(prototype)), line),
            assign(row, add(var(base), int(10)), line),
        ];
        let header = decode_header(&mut e, base, &mut body)?;
        body.push(assert_stmt(not(lt(var(header.count), int(1))), line));
        let (desc, _) = decode_row(&mut e, row, &mut body)?;
        for test in [
            eq(field(var(desc), "present"), int(51)),
            not(field(var(desc), "writable")),
            not(field(var(desc), "enumerable")),
            not(field(var(desc), "configurable")),
            tag_is(field(var(desc), "value"), value::NUMBER),
            eq(
                num(field(field(var(desc), "value"), "number")),
                E::Float(0.0),
            ),
        ] {
            body.push(assert_stmt(test, line));
        }
        body.extend([
            assign(
                key,
                value::dynamic_value(
                    int(value::STRING),
                    E::Float(0.0),
                    E::Bool(false),
                    e.decoded(var(row)),
                    e.generation(add(var(row), int(1))),
                ),
                line,
            ),
            assign(key_base, e.base(var(key)), line),
            assert_stmt(eq(e.read(add(var(key_base), int(2))), E::Float(6.0)), line),
        ]);
        for (i, unit) in "length".encode_utf16().enumerate() {
            body.push(assert_stmt(
                eq(
                    e.read(add(var(key_base), int(3 + i as i64))),
                    E::Float(f64::from(unit)),
                ),
                line,
            ));
        }
        let body = e.finish(body);
        declare(
            ir,
            stmts,
            "__compat_es5_function_length_key",
            length_key,
            vec![carrier],
            vec![carrier_schema.clone()],
            value::schema(),
            body,
            var(key),
            line,
        );
    }
    {
        let carrier = lower.slot(span)?;
        let row = lower.slot(span)?;
        let base = lower.slot(span)?;
        let new_count_bits = lower.slot(span)?;
        let packed_tag = lower.slot(span)?;
        let number = lower.slot(span)?;
        let flag_number = lower.slot(span)?;
        let ref_slot = lower.slot(span)?;
        let generation = lower.slot(span)?;
        let mut e = Emit::new(lower, span, carrier)?;
        let flags = e.integer()?;
        let mut body = Vec::new();
        for (offset, payload) in [
            (2, packed_tag),
            (3, number),
            (4, flag_number),
            (5, ref_slot),
            (6, generation),
        ] {
            body.push(e.write(add(var(row), int(offset)), var(payload))?);
        }
        body.push(assign(flags, e.decoded(add(var(base), int(2))), line));
        body.push(assign(
            flags,
            E::Mod(Box::new(var(flags)), Box::new(int(layout::OBJECT_COUNT_FACTOR))),
            line,
        ));
        body.push(e.write(add(var(base), int(2)), add(var(new_count_bits), var(flags)))?);
        let body = e.finish(body);
        declare(
            ir,
            stmts,
            "__compat_es5_bootstrap_row_suffix",
            suffix,
            vec![
                carrier,
                row,
                base,
                new_count_bits,
                packed_tag,
                number,
                flag_number,
                ref_slot,
                generation,
            ],
            vec![
                carrier_schema.clone(),
                T::Int,
                T::Int,
                T::Int,
                T::Number,
                T::Number,
                T::Number,
                T::Number,
                T::Number,
            ],
            T::Int,
            body,
            int(0),
            line,
        );
    }
    {
        let carrier = lower.slot(span)?;
        let base = lower.slot(span)?;
        let bytes = lower.slot(span)?;
        let mut e = Emit::new(lower, span, carrier)?;
        let length = e.integer()?;
        let index = e.integer()?;
        let body = vec![
            assign(
                length,
                call(
                    vm::BuiltinFunction::Len.call_index(),
                    vec![E::Borrow(Box::new(var(bytes)))],
                ),
                line,
            ),
            e.write(add(var(base), int(2)), var(length))?,
            S::While {
                condition: lt(var(index), var(length)),
                body: vec![
                    e.write(
                        add(add(var(base), int(3)), var(index)),
                        call(
                            vm::BuiltinFunction::Get.call_index(),
                            vec![E::Borrow(Box::new(var(bytes))), var(index)],
                        ),
                    )?,
                    assign(index, add(var(index), int(1)), line),
                ],
                line,
            },
        ];
        let body = e.finish(body);
        declare(
            ir,
            stmts,
            "__compat_es5_bootstrap_ascii_init",
            ascii_init,
            vec![carrier, base, bytes],
            vec![carrier_schema.clone(), T::Int, T::Bytes],
            T::Int,
            body,
            int(0),
            line,
        );
    }
    let uri_error = if lower.uri_used.get() != 0 {
        let index = super::heap::fresh_index(lower, ir, span)?;
        let proto_entry = uri_calls::prototype_root(lower);
        let carrier = lower.slot(span)?;
        let mut e = Emit::new(lower, span, carrier)?;
        let frame = e.integer()?;
        let result = e.boxed()?;
        let body = vec![
            assign(frame, call(h.root_enter, vec![var(carrier), int(1)]), line),
            assign(
                result,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(3),
                        call(root, vec![var(carrier), int(proto_entry)]),
                        int(0),
                        int(0),
                        int(0), int(6),
                        var(frame),
                        int(0),
                    ],
                ),
                line,
            ),
            // Return is allocation-free up to the adapter's immediate root write.
            e.effect(h.root_leave, vec![var(carrier), var(frame)])?,
        ];
        let body = e.finish(body);
        declare(
            ir,
            stmts,
            "__compat_es5_new_uri_error",
            index,
            vec![carrier],
            vec![carrier_schema.clone()],
            value::schema(),
            body,
            var(result),
            line,
        );
        Some(index)
    } else {
        None
    };
    // Array prototype reader: the private empty packed Array prototype at the
    // permanent frame tail. Reads never allocate; the returned handle carries
    // the checked slot/generation pair so a later allocation cannot leave a
    // stale physical base in guest hands.
    let array_proto = {
        let index = super::heap::fresh_index(lower, ir, span)?;
        let carrier = lower.slot(span)?;
        let entry = array_prototype_root(lower);
        let mut e = Emit::new(lower, span, carrier)?;
        let frame = e.integer()?;
        let address = e.integer()?;
        let slot = e.integer()?;
        let generation = e.integer()?;
        let base = e.integer()?;
        let body = vec![
            assign(frame, e.decoded(int(layout::HDR_INTRINSIC_FRAME)), line),
            assert_stmt(
                and(
                    not(lt(var(frame), int(layout::PREFIX_CELLS))),
                    eq(e.decoded(add(var(frame), int(3))), int(root_capacity)),
                ),
                line,
            ),
            assign(
                address,
                add(add(var(frame), int(4)), mul(int(3), int(entry))),
                line,
            ),
            assert_stmt(eq(e.decoded(var(address)), int(layout::ROOT_OBJECT)), line),
            assign(slot, e.decoded(add(var(address), int(1))), line),
            assign(generation, e.generation(add(var(address), int(2))), line),
            assign(
                base,
                call(
                    h.checked_base,
                    vec![var(carrier), var(slot), var(generation)],
                ),
                line,
            ),
            assert_stmt(eq(e.decoded(var(base)), int(layout::KIND_ARRAY)), line),
            assert_stmt(eq(e.decoded(add(var(base), int(2))), int(0)), line),
        ];
        let body = e.finish(body);
        declare(
            ir,
            stmts,
            "__compat_es5_array_prototype",
            index,
            vec![carrier],
            vec![carrier_schema.clone()],
            value::schema(),
            body,
            value::dynamic_value(
                int(value::OBJECT),
                E::Float(0.0),
                E::Bool(false),
                var(slot),
                var(generation),
            ),
            line,
        );
        index
    };
    let mut range_error = None;
    let mut optional_errors = [None; OptionalError::ALL.len()];
    let range = range_error_realm_used(lower).then_some(None);
    for error in range.into_iter().chain(OptionalError::selected(lower).map(Some)) {
        let index = super::heap::fresh_index(lower, ir, span)?;
        let ctor = error.map_or_else(|| range_error_constructor_root(lower), |error| error.constructor_root(lower));
        let carrier = lower.slot(span)?;
        let rows = lower.slot(span)?;
        let mut e = Emit::new(lower, span, carrier)?;
        let frame = e.integer()?;
        let function = e.boxed()?;
        let base = e.integer()?;
        let row = e.integer()?;
        let proto = e.boxed()?;
        let proto_base = e.integer()?;
        let result = e.boxed()?;
        let mut body = vec![
            assign(function, call(root, vec![var(carrier), int(ctor)]), line),
            assign(base, e.base(var(function)), line),
            assert_stmt(eq(e.decoded(var(base)), int(layout::KIND_FUNCTION)), line),
        ];
        let header = decode_header(&mut e, base, &mut body)?;
        body.push(assert_stmt(not(lt(var(header.count), int(2))), line));
        // Row 1 is the published non-writable, non-configurable `prototype`
        // data row. URIError points to the same permanently rooted prototype
        // as the private allocator; the other prototypes are retained by this edge.
        body.push(assign(
            row,
            add(
                var(base),
                int(layout::FUNCTION_CELLS + layout::OBJECT_CELLS_PER_PROPERTY),
            ),
            line,
        ));
        body.push(assert_stmt(
            eq(
                e.decoded(add(var(row), int(2))),
                int(value::OBJECT * layout::OBJECT_TAG_FACTOR),
            ),
            line,
        ));
        body.push(assign(
            proto,
            value::dynamic_value(
                int(value::OBJECT),
                E::Float(0.0),
                E::Bool(false),
                e.decoded(add(var(row), int(5))),
                e.generation(add(var(row), int(6))),
            ),
            line,
        ));
        body.push(assign(proto_base, e.base(var(proto)), line));
        body.push(assert_stmt(
            eq(e.decoded(var(proto_base)), int(layout::KIND_OBJECT)),
            line,
        ));
        let proto_header = decode_header(&mut e, proto_base, &mut body)?;
        // Every Error-family prototype is a plain object shell (brand 0).
        // Instances, not prototypes, carry legacy|instance.
        body.push(assert_stmt(
            and(
                not(var(proto_header.error)),
                not(var(proto_header.error_instance)),
            ),
            line,
        ));
        let legacy = vec![
            assign(frame, call(h.root_enter, vec![var(carrier), int(1)]), line),
            assign(
                result,
                call(
                    allocate,
                    vec![
                        var(carrier),
                        int(3),
                        var(proto),
                        var(rows),
                        int(0),
                        int(0), int(6),
                        var(frame),
                        int(0),
                    ],
                ),
                line,
            ),
            e.effect(h.root_leave, vec![var(carrier), var(frame)])?,
        ];
        if error.is_none() {
            body.push(branch(
                eq(var(rows), int(0)),
                vec![assign(result, call(empty_error, vec![var(carrier), var(proto)]), line)],
                legacy,
                line,
            ));
        } else {
            body.extend(legacy);
        }
        let body = e.finish(body);
        declare(
            ir,
            stmts,
            error.map_or(RANGE_ERROR_NAME, OptionalError::allocator_name),
            index,
            vec![carrier, rows],
            vec![carrier_schema.clone(), T::Int],
            value::schema(),
            body,
            var(result),
            line,
        );
        if let Some(error) = error {
            optional_errors[error as usize] = Some(index);
        } else {
            range_error = Some(index);
        }
    }
    // Shared empty String for anonymous function names. It lives in its own
    // permanent root, never behind the guest-writable `Error.prototype`
    // `message` row, so a name row for an anonymous function is a value copy
    // of that node and allocates no second payload while guest writes or
    // deletes on the public row cannot reach it. Declared only for a source
    // that can spell `.name`/`.toString`, keeping the default helper window
    // unchanged.
    let function_name_empty = if function_names {
        let index = super::heap::fresh_index(lower, ir, span)?;
        let carrier = lower.slot(span)?;
        let mut e = Emit::new(lower, span, carrier)?;
        let empty = e.boxed()?;
        let base = e.integer()?;
        let body = vec![
            assign(
                empty,
                call(
                    root,
                    vec![var(carrier), int(function_name_empty_root(e.lower))],
                ),
                line,
            ),
            assign(base, e.base(var(empty)), line),
            assert_stmt(eq(e.decoded(var(base)), int(layout::KIND_STRING)), line),
            assert_stmt(eq(e.decoded(add(var(base), int(2))), int(0)), line),
        ];
        let body = e.finish(body);
        declare(
            ir,
            stmts,
            "__compat_es5_function_name_empty",
            index,
            vec![carrier],
            vec![carrier_schema.clone()],
            value::schema(),
            body,
            var(empty),
            line,
        );
        Some(index)
    } else {
        None
    };
    // The `name` key String is the fixed `name` key already retained by the
    // Error prototype: the gated Function.prototype accessor row names it,
    // and every source function recovers the key pair from that row instead
    // of allocating a second String.
    let function_name_key = if function_names {
        let index = super::heap::fresh_index(lower, ir, span)?;
        let carrier = lower.slot(span)?;
        let mut e = Emit::new(lower, span, carrier)?;
        let prototype = e.boxed()?;
        let base = e.integer()?;
        let row = e.integer()?;
        let key = e.boxed()?;
        let key_base = e.integer()?;
        let mut body = vec![
            assign(prototype, call(root, vec![var(carrier), int(1)]), line),
            assign(base, e.base(var(prototype)), line),
            assert_stmt(eq(e.decoded(var(base)), int(layout::KIND_FUNCTION)), line),
        ];
        let header = decode_header(&mut e, base, &mut body)?;
        body.push(assert_stmt(not(lt(var(header.count), int(5))), line));
        body.push(assign(
            row,
            add(
                var(base),
                int(layout::FUNCTION_CELLS + 4 * layout::OBJECT_CELLS_PER_PROPERTY),
            ),
            line,
        ));
        let (desc, _) = decode_row(&mut e, row, &mut body)?;
        body.extend([assign(
            key,
            value::dynamic_value(
                int(value::STRING),
                E::Float(0.0),
                E::Bool(false),
                e.decoded(var(row)),
                e.generation(add(var(row), int(1))),
            ),
            line,
        )]);
        body.push(assign(key_base, e.base(var(key)), line));
        body.push(assert_stmt(
            eq(e.read(add(var(key_base), int(2))), E::Float(4.0)),
            line,
        ));
        for (i, unit) in FUNCTION_NAME_NAME.encode_utf16().enumerate() {
            body.push(assert_stmt(
                eq(
                    e.read(add(var(key_base), int(3 + i as i64))),
                        E::Float(f64::from(unit)),
                ),
                line,
            ));
        }
        body.push(assert_stmt(eq(field(var(desc), "present"), int(60)), line));
        let body = e.finish(body);
        declare(
            ir,
            stmts,
            "__compat_es5_function_name_key",
            index,
            vec![carrier],
            vec![carrier_schema.clone()],
            value::schema(),
            body,
            var(key),
            line,
        );
        Some(index)
    } else {
        None
    };
    // One private adapter backs both `Function.prototype.name` (the accessor
    // getter, code 352) and `Function.prototype.toString` (code 353). It
    // validates the receiver as a callable function node, resolves the stored
    // `name` row (empty for anonymous/bootstrap functions), and, for
    // `toString`, builds `function N() {\n    [native code]\n}` from that
    // name. Non-function receivers and the Function prototype itself throw
    // the realm TypeError, matching upstream `not a function`.
    let function_name_api = if function_names {
        let index = super::heap::fresh_index(lower, ir, span)?;
        let carrier = lower.slot(span)?;
        let code = lower.slot(span)?;
        let receiver = lower.slot(span)?;
        let args = lower.slot(span)?;
        let mut e = Emit::new(lower, span, carrier)?;
        let sp = e.lower.ir_span(span);
        let frame = e.integer()?;
        let base = e.integer()?;
        let kind = e.integer()?;
        let count = e.integer()?;
        let cursor = e.integer()?;
        let found = e.boolean()?;
        let equal = e.boolean()?;
        let index_slot = e.integer()?;
        let row = e.integer()?;
        let row_key_base = e.integer()?;
        let name = e.boxed()?;
        let name_base = e.integer()?;
        let name_len = e.integer()?;
        let total = e.integer()?;
        let out_base = e.integer()?;
        let err = e.boxed()?;
        let published = e.boxed()?;
        let alloc = e.temp(
            value::allocation_schema(),
            value::allocation(int(0), int(0), int(0)),
        )?;
        let out = e.temp(
            value::logical_result_schema(),
            value::result_ok(value::completion_normal(), sp),
        )?;
        let mut body = vec![
            assert_stmt(
                or(
                    eq(var(code), int(FUNCTION_NAME_GET_CODE)),
                    eq(var(code), int(FUNCTION_TO_STRING_CODE)),
                ),
                line,
            ),
            assign(frame, call(h.root_enter, vec![var(carrier), int(4)]), line),
            e.root(var(frame), int(0), var(receiver))?,
        ];
        let fail = vec![
            assign(err, call(type_error, vec![var(carrier)]), line),
            e.root(var(frame), int(3), var(err))?,
            assign(out, value::result_err(var(err), sp), line),
        ];
        let mut ordinary = vec![
            assign(
                name,
                call(
                    function_name_empty.expect("gated function name empty helper"),
                    vec![var(carrier)],
                ),
                line,
            ),
            e.root(var(frame), int(1), var(name))?,
        ];
        let header = decode_header(&mut e, base, &mut ordinary)?;
        ordinary.extend([
            assign(count, var(header.count), line),
            assign(cursor, int(0), line),
            assign(found, E::Bool(false), line),
        ]);
        let mut scan = vec![assign(
            row,
            add(
                add(var(base), int(layout::FUNCTION_CELLS)),
                mul(int(layout::OBJECT_CELLS_PER_PROPERTY), var(cursor)),
            ),
            line,
        )];
        let (desc, stored_key_base) = decode_row(&mut e, row, &mut scan)?;
        scan.push(assign(row_key_base, var(stored_key_base), line));
        scan.push(assign(
            equal,
            eq(
                e.read(add(var(row_key_base), int(2))),
                E::Float(4.0),
            ),
            line,
        ));
        for (i, unit) in FUNCTION_NAME_NAME.encode_utf16().enumerate() {
            scan.push(assign(
                equal,
                and(
                    var(equal),
                    eq(
                        e.read(add(var(row_key_base), int(3 + i as i64))),
                        E::Float(f64::from(unit)),
                    ),
                ),
                line,
            ));
        }
        scan.push(branch(
            var(equal),
            vec![
                assign(name, field(var(desc), "value"), line),
                e.root(var(frame), int(1), var(name))?,
                assign(found, E::Bool(true), line),
            ],
            vec![assign(cursor, add(var(cursor), int(1)), line)],
            line,
        ));
        ordinary.push(S::While {
            condition: and(not(var(found)), lt(var(cursor), var(count))),
            body: scan,
            line,
        });
        let name_answer = vec![assign(
            out,
            value::result_ok(value::completion_return(var(name)), sp),
            line,
        )];
        let mut print = vec![
            assign(name_base, e.base(var(name)), line),
            assert_stmt(
                eq(e.decoded(var(name_base)), int(layout::KIND_STRING)),
                line,
            ),
            assign(
                name_len,
                e.decoded(add(var(name_base), int(2))),
                line,
            ),
            assign(total, add(var(name_len), int(33)), line),
            assign(
                alloc,
                call(
                    h.node_begin,
                    vec![
                        var(carrier),
                        add(var(total), int(layout::STRING_HEADER_CELLS)),
                        int(layout::KIND_STRING),
                    ],
                ),
                line,
            ),
            assign(out_base, field(var(alloc), "base"), line),
        ];
        print.push(e.write(add(var(out_base), int(2)), var(total))?);
        for (i, unit) in "function ".encode_utf16().enumerate() {
            print.push(e.write(
                add(var(out_base), int(3 + i as i64)),
                int(i64::from(unit)),
            )?);
        }
        let mut copy = vec![e.write(
            add(add(var(out_base), int(12)), var(index_slot)),
            e.read(add(add(var(name_base), int(3)), var(index_slot))),
        )?];
        copy.push(assign(
            index_slot,
            add(var(index_slot), int(1)),
            line,
        ));
        print.push(assign(index_slot, int(0), line));
        print.push(S::While {
            condition: lt(var(index_slot), var(name_len)),
            body: copy,
            line,
        });
        for (i, unit) in "() {\n    [native code]\n}".encode_utf16().enumerate() {
            print.push(e.write(
                add(
                    add(add(var(out_base), int(3 + 9)), var(name_len)),
                    int(i as i64),
                ),
                int(i64::from(unit)),
            )?);
        }
        print.extend([
            assign(
                published,
                call(
                    h.node_publish,
                    vec![
                        var(carrier),
                        field(var(alloc), "slot"),
                        field(var(alloc), "generation"),
                        var(out_base),
                        var(frame),
                        int(2),
                    ],
                ),
                line,
            ),
            e.root(var(frame), int(2), var(published))?,
            assign(
                out,
                value::result_ok(value::completion_return(var(published)), sp),
                line,
            ),
        ]);
        ordinary.push(branch(
            eq(var(code), int(FUNCTION_NAME_GET_CODE)),
            name_answer,
            print,
            line,
        ));
        body.push(branch(
            tag_is(var(receiver), value::OBJECT),
            vec![
                assign(base, e.base(var(receiver)), line),
                assign(kind, e.decoded(var(base)), line),
                branch(
                    eq(var(kind), int(layout::KIND_FUNCTION)),
                    vec![branch(
                        eq(e.decoded(add(var(base), int(5))), int(256)),
                        fail.clone(),
                        ordinary,
                        line,
                    )],
                    fail.clone(),
                    line,
                ),
            ],
            fail.clone(),
            line,
        ));
        body.push(e.effect(h.root_leave, vec![var(carrier), var(frame)])?);
        let body = e.finish(body);
        declare(
            ir,
            stmts,
            "__compat_es5_function_name_api",
            index,
            vec![carrier, code, receiver, args],
            vec![
                carrier_schema.clone(),
                T::Int,
                value::schema(),
                T::Array(Box::new(value::schema())),
            ],
            value::logical_result_schema(),
            body,
            var(out),
            line,
        );
        Some(index)
    } else {
        None
    };
    Ok(Intrinsics {
        root,
        allocate,
        bootstrap,
        type_error,
        range_error,
        optional_errors,
        length_key,
        function_name_key,
        function_name_empty,
        function_name_api,
        uri_error,
        array_proto,
        array_buffer: None,
        typed_array: None,
    })
}

// Private emitter fixtures exist only in the library test build. No production
// option, public API, source selector, or generic UTF-16 pool is involved.
#[cfg(test)]
mod ascii_tests {
    use super::*;
    use std::cell::RefCell;
    use vm::{HostFunctionRegistry, SourcePlugin, Value, VmStatus};

    #[derive(Default)]
    struct Fixture {
        text: Option<&'static str>,
        selected: bool,
        observed: bool,
        typed: bool,
    }
    thread_local! {
        static FIXTURE: RefCell<Fixture> = RefCell::new(Fixture::default());
    }
    pub(super) fn fixture_text(text: &str) -> &str {
        FIXTURE.with(|f| {
            let mut f = f.borrow_mut();
            if text.is_empty()
                && !f.selected
                && let Some(input) = f.text
            {
                f.selected = true;
                input
            } else {
                text
            }
        })
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn observe_fixture(
        e: &mut Emit<'_, '_>,
        text: &str,
        alloc: u16,
        result: u16,
        base: u16,
        frame: u16,
        entry: i64,
        body: &mut Vec<S>,
    ) -> Result<(), ParseError> {
        let selected = FIXTURE.with(|f| {
            let mut f = f.borrow_mut();
            if f.selected && !f.observed && f.text == Some(text) {
                f.observed = true;
                Some(f.typed)
            } else {
                None
            }
        });
        let Some(typed) = selected else { return Ok(()) };
        let helper = e
            .bootstrap_ascii_init
            .expect("fixture must opt into bootstrap initializer");
        let calls: Vec<_> = body
            .iter()
            .enumerate()
            .filter_map(|(at, s)| match s {
                S::Assign {
                    expr: E::Call(i, _, aa, _, _),
                    ..
                } if *i == helper && matches!(aa.get(1),Some(E::Var(i)) if *i==base) => Some(at),
                _ => None,
            })
            .collect();
        assert_eq!(
            calls.len(),
            usize::from(text.is_ascii()),
            "actual emitter branch"
        );
        let units: Vec<_> = text.encode_utf16().collect();
        let l = e.line;
        let h = e.lower.heap_ref(e.span)?.helpers;
        if typed {
            assert!(text.is_ascii());
            let bytes = e.temp(T::Bytes, E::Bytes(text.as_bytes().to_vec()))?;
            let S::Assign {
                expr: E::Call(_, types, aa, meta, semantic),
                ..
            } = &mut body[calls[0]]
            else {
                unreachable!()
            };
            assert!(types.is_empty() && meta.is_none() && semantic.is_none());
            aa[2] = E::Borrow(Box::new(var(bytes)));
            body.push(assert_stmt(
                eq(
                    call(
                        vm::BuiltinFunction::Len.call_index(),
                        vec![E::Borrow(Box::new(var(bytes)))],
                    ),
                    int(units.len() as i64),
                ),
                l,
            ));
        }
        let root = add(
            var(frame),
            int(layout::FRAME_FIXED_CELLS + entry * layout::FRAME_CELLS_PER_ENTRY),
        );
        let check_payload = |e: &Emit<'_, '_>| {
            let mut checks = vec![
                assert_stmt(eq(field(var(result), "tag"), int(value::STRING)), l),
                assert_stmt(eq(e.base(var(result)), var(base)), l),
                assert_stmt(eq(e.decoded(var(base)), int(layout::KIND_STRING)), l),
                assert_stmt(
                    eq(e.decoded(add(var(base), int(2))), int(units.len() as i64)),
                    l,
                ),
                assert_stmt(eq(e.decoded(int(layout::HDR_PENDING_NODES)), int(0)), l),
                assert_stmt(eq(e.decoded(int(layout::HDR_ROOT_TOP)), var(frame)), l),
                assert_stmt(eq(e.decoded(root.clone()), int(layout::ROOT_STRING)), l),
                assert_stmt(
                    eq(
                        e.decoded(add(root.clone(), int(1))),
                        field(var(result), "ref_slot"),
                    ),
                    l,
                ),
                assert_stmt(
                    eq(
                        e.generation(add(root.clone(), int(2))),
                        field(var(result), "generation"),
                    ),
                    l,
                ),
                assert_stmt(eq(field(var(alloc), "base"), var(base)), l),
            ];
            for (i, u) in units.iter().enumerate() {
                checks.push(assert_stmt(
                    eq(
                        e.read(add(var(base), int(3 + i as i64))),
                        E::Float(f64::from(*u)),
                    ),
                    l,
                ));
            }
            checks
        };
        body.extend(check_payload(e));
        body.push(e.effect(h.heap_collect, vec![var(e.carrier)])?);
        body.extend(check_payload(e));
        Ok(())
    }
    struct Reset(Fixture);
    impl Drop for Reset {
        fn drop(&mut self) {
            FIXTURE.with(|f| *f.borrow_mut() = std::mem::take(&mut self.0));
        }
    }
    struct Invocation {
        text: &'static str,
        typed: bool,
        observations: std::sync::Mutex<Vec<(bool, bool)>>,
    }
    struct FixturePlugin {
        request: std::sync::Mutex<Option<std::sync::Arc<Invocation>>>,
    }
    static FIXTURE_PLUGIN: FixturePlugin = FixturePlugin {
        request: std::sync::Mutex::new(None),
    };
    static INVOCATION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    struct RequestScope;
    impl Drop for RequestScope {
        fn drop(&mut self) {
            FIXTURE_PLUGIN
                .request
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
        }
    }
    impl SourcePlugin for FixturePlugin {
        fn descriptor(&self) -> vm::SourceFrontendDescriptor {
            crate::JAVASCRIPT_ES5_PLUGIN.descriptor()
        }
        fn parse_source(
            &self,
            source: &str,
            context: &vm::SourceFrontendContext<'_>,
        ) -> Result<FrontendIr, ParseError> {
            let request = self
                .request
                .lock()
                .unwrap()
                .as_ref()
                .cloned()
                .expect("active scoped fixture compilation");
            let previous = FIXTURE.with(|f| {
                f.replace(Fixture {
                    text: Some(request.text),
                    typed: request.typed,
                    ..Fixture::default()
                })
            });
            let _reset = Reset(previous);
            let ir = crate::JAVASCRIPT_ES5_PLUGIN.parse_source(source, context);
            let observation = FIXTURE.with(|f| {
                let f = f.borrow();
                (f.selected, f.observed)
            });
            request.observations.lock().unwrap().push(observation);
            ir
        }
        fn module_type_exports(
            &self,
            source: &str,
            context: &vm::SourceFrontendContext<'_>,
            ir: &FrontendIr,
        ) -> Result<Vec<vm::FrontendTypeExport>, ParseError> {
            crate::JAVASCRIPT_ES5_PLUGIN.module_type_exports(source, context, ir)
        }
        fn validate_typed_ir(&self, ir: &vm::FrontendTypedIr<'_>) -> Result<(), vm::CompileError> {
            crate::JAVASCRIPT_ES5_PLUGIN.validate_typed_ir(ir)
        }
        fn parser_dialect(&self) -> Option<&'static dyn vm::ParserDialect> {
            crate::JAVASCRIPT_ES5_PLUGIN.parser_dialect()
        }
        fn parse_module_imports(
            &self,
            source: &str,
            path: &std::path::Path,
            context: &vm::SourceFrontendContext<'_>,
        ) -> Result<Vec<vm::ModuleImport>, vm::SourcePathError> {
            crate::JAVASCRIPT_ES5_PLUGIN.parse_module_imports(source, path, context)
        }
        fn strip_import_directives(&self, source: &str) -> String {
            crate::JAVASCRIPT_ES5_PLUGIN.strip_import_directives(source)
        }
    }
    fn execute_fixture(text: &'static str, typed: bool) {
        let _invocation = INVOCATION_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        FIXTURE.with(|f| {
            *f.borrow_mut() = Fixture {
                text: Some(text),
                typed,
                ..Fixture::default()
            }
        });
        let _reset = Reset(Fixture::default());
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = vm::SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::compiler::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let ir = crate::JAVASCRIPT_ES5_PLUGIN
            .parse_source("Math.random();", &context)
            .unwrap();
        let helper = ir
            .functions
            .iter()
            .find(|f| f.name == "__compat_es5_bootstrap_ascii_init")
            .expect("real private helper");
        assert_eq!(helper.arg_schemas.last(), Some(&Some(T::Bytes)));
        FIXTURE.with(|f| assert!(f.borrow().observed));
        // Each actual plugin invocation installs fresh worker-local fixture
        // state and returns its own evidence after the real parser emitted it.
        let request = std::sync::Arc::new(Invocation {
            text,
            typed,
            observations: std::sync::Mutex::new(Vec::new()),
        });
        {
            let mut slot = FIXTURE_PLUGIN.request.lock().unwrap();
            assert!(slot.is_none(), "isolated invocation lease");
            *slot = Some(request.clone());
        }
        let scope = RequestScope;
        let compiled = vm::compile_source_with_flavor_and_options_with_capture_certification(
            "Math.random();",
            crate::javascript_es5_flavor(),
            vm::CompileSourceFileOptions::new()
                .with_host_api_catalog(catalog.clone())
                .with_source_plugin(&FIXTURE_PLUGIN),
        )
        .expect("real typed synthetic emitter fixture");
        drop(scope);
        {
            let observations = request.observations.lock().unwrap();
            assert!(
                !observations.is_empty(),
                "actual compilation plugin invocation"
            );
            for (selected, observed) in observations.iter() {
                assert!(
                    *selected && *observed,
                    "actual compilation emitted and observed fixture"
                );
            }
        }
        let mut machine = compiled.into_vm().unwrap();
        machine.set_jit_config(vm::JitConfig {
            enabled: false,
            ..vm::JitConfig::default()
        });
        crate::runtime::array::initialize_typescript_owned_arrays(
            &mut machine,
            crate::runtime::array::OwnedArrayLimits::default(),
        )
        .unwrap();
        let mut registry = HostFunctionRegistry::restricted();
        for module in [
            crate::runtime::array::typescript_array_host_module(),
            crate::runtime::array::typescript_owned_array_host_module(),
            crate::runtime::array::typescript_array_snapshot_host_module(),
            crate::runtime::scalar_math::typescript_scalar_math_host_module(),
            crate::runtime::scalar_error::typescript_error_host_module(),
        ] {
            module
                .install_from_catalog(&mut registry, &catalog)
                .unwrap();
        }
        for name in [
            "floor",
            "abs",
            "acos",
            "asin",
            "atan",
            "atan2",
            "ceil",
            "cos",
            "exp",
            "ln",
            "powf",
            "sin",
            "sqrt",
            "tan",
            "trunc",
            "log2",
            "log10",
            "exp2",
            "signum",
            "copysign",
            "is_finite",
            "is_infinite",
        ] {
            registry.allow_builtin(format!("math::{name}")).unwrap();
        }
        registry.bind_vm_cached(&mut machine).unwrap();
        machine.set_fuel(10_000_000);
        assert_eq!(
            machine
                .run()
                .expect("actual helper/inline execution and guest payload/root assertions"),
            VmStatus::Halted
        );
        assert_eq!(
            (machine.aot_exec_count(), machine.jit_native_exec_count()),
            (0, 0)
        );
        assert!(!machine.jit_config().enabled);
        assert!(
            machine
                .capture_certification_stats()
                .is_some_and(|s| s.valid)
        );
        assert_eq!(machine.host_context().resource_count(), 1);
        let stats = crate::runtime::array::owned_array_runtime_stats(&mut machine).unwrap();
        assert_eq!((stats.pending, stats.registered), (0, 1));
        assert!(matches!(machine.stack().last(), Some(Value::Map(_))));
        machine.reset_for_reuse().unwrap();
        assert_eq!(machine.host_context().resource_count(), 0);
        eprintln!(
            "FIX20_FIXTURE synthetic=true units={:?} typed={typed} payload=true root=true collected_live=true reset=true aot=0 trace=0",
            text.encode_utf16().collect::<Vec<_>>()
        );
    }
    #[test]
    fn empty_ascii_executes_length_write() {
        execute_fixture("", false);
    }
    #[test]
    fn edge_ascii_executes_exact_units() {
        execute_fixture("\0AZ\u{7f}", false);
    }
    #[test]
    fn non_ascii_uses_inline_utf16() {
        execute_fixture("é水😀", false);
    }
    #[test]
    fn typed_bytes_preserves_roots_and_lifetime() {
        execute_fixture("\0typed\u{7f}", true);
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    /// The default manifest's static request must match the capacities the
    /// emitter passes to the allocator. Recomputing the parts independently
    /// makes this fail if the budget reverts to six one-row prototype methods
    /// while the shared table (and the emitter) keep `apply` at two rows.
    #[test]
    fn default_bootstrap_request_matches_emitter_capacity_table() {
        // Six ordinary object nodes: Object.prototype (4 rows), the two error
        // prototypes (Error 4 and TypeError 2), and the three
        // primitive-prototype bridges. String has 18 rows now. Forty-three
        // function nodes in the 42* header term plus the TypeError node:
        // sixteen previous String methods plus repeat.
        let object_cells = 7 * (3 + 5) + 7 * (4 + 4 + 2 + 18 + 1 + 1 + 20);
        // Fifty-five function nodes plus TypeError: Function.prototype (4
        // rows), Object (11), the nine statics (1 each), six Object/Function
        // prototype methods, seventeen String methods, twenty Array methods
        // and the Error prototype's one-row `toString`.
        let function_rows = 4
            + 11
            + 9
            + PROTO_METHOD_CAPACITIES.iter().sum::<i64>()
            + 1
            + 1
            + 1
            + 1
            + 1
            + 1
            + 1
            + 1
            + 1
            + 1
            + 1;
        let function_rows = function_rows + 6 + 20 + 1;
        let function_cells = 55 * (3 + 10) + 7 * function_rows + (3 + 10 + 7 * 2);
        let string_cells = [
            "length",
            "prototype",
            "constructor",
            "name",
            "message",
            "Error",
            "TypeError",
            "",
        ]
        .into_iter()
        .chain(METHODS.iter().map(|(name, _)| *name))
        .chain([
            "hasOwnProperty",
            "valueOf",
            "toString",
            "call",
            "apply",
            "bind",
            STRING_CHAR_CODE_AT_NAME,
            STRING_CHAR_AT_NAME,
            STRING_CODE_POINT_AT_NAME,
            STRING_CONCAT_NAME,
            STRING_SUBSTRING_NAME,
            STRING_SLICE_NAME,
            STRING_INDEXOF_NAME,
            STRING_LAST_INDEX_OF_NAME,
            STRING_TO_STRING_NAME,
            STRING_TO_LOWER_CASE_NAME,
            STRING_TO_UPPER_CASE_NAME,
            STRING_TRIM_NAME,
            STRING_TRIM_END_NAME,
            STRING_TRIM_START_NAME,
            STRING_REPLACE_NAME,
            STRING_SPLIT_NAME,
            STRING_REPEAT_NAME,
            "push",
            "pop",
            "shift",
            "unshift",
            "reverse",
            "indexOf",
            "lastIndexOf",
            "slice",
            "concat",
            "every",
            "some",
            "forEach",
            "map",
            "filter",
            "reduce",
            "reduceRight",
            "join",
            "toString",
            "splice",
            "sort",
        ])
        .map(|name| 6 + name.encode_utf16().count() as i64)
        .sum::<i64>();
        let node_cells = object_cells
            + function_cells
            + string_cells
            + layout::RAW_HEADER_CELLS
            + layout::ARRAY_HEADER_CELLS;
        // Nine permanent roots (five realm objects plus the Array/String/
        // Number/Boolean prototypes), the eight-entry stage frame, and the
        // first 64-row directory page. The realm prefix and fixed frame are
        // charged ahead of this request, not inside it.
        let frame_and_page_cells = layout::FRAME_FIXED_CELLS
            + layout::FRAME_CELLS_PER_ENTRY * 11
            + (4 + 3 * 8)
            + layout::DIR_PAGE_CELLS;
        assert_eq!(
            (object_cells, function_cells, string_cells, node_cells),
            (406, 1225, 826, 2466)
        );
        assert_eq!(node_cells + frame_and_page_cells, 2982);
        assert_eq!(
            bootstrap_request_cells_for(0, 11, false, false, false, false, false, false),
            (node_cells + frame_and_page_cells) as u64
        );
        // The helper frame table is charged once per realm. Its sixth entry is
        // the `defineProperties` slot bootstrap never enters, so that slot
        // pays its birth capacity 8 and not the 1032-entry runtime request.
        assert_eq!(
            super::super::object::ObjectHelpers::helper_frame_request_cells(),
            316
        );
        // Whole default-manifest request, the number `record_intrinsic_budget`
        // reports when a default-manifest source is lowered: the realm prefix
        // and fixed frame 84, the two object-bundle DefaultValue key Strings
        // 27, the bootstrap extents above 2982, the helper frame table 316,
        // the stage frame 7 and the second directory page 451. Locking the
        // total keeps a quiet re-budget of any table visible here instead of
        // only in the boundary witnesses.
        assert_eq!(
            (layout::PREFIX_CELLS + layout::FRAME_FIXED_CELLS) as u64
                + super::super::math::default_value_key_request().0
                + node_cells as u64
                + frame_and_page_cells as u64
                + super::super::object::ObjectHelpers::helper_frame_request_cells()
                + (layout::FRAME_FIXED_CELLS + layout::FRAME_CELLS_PER_ENTRY) as u64
                + layout::DIR_PAGE_CELLS as u64,
            3867
        );
    }
    /// A `.constructor` read is the one site that selects the three primitive
    /// carriers without their global names. The accessor family is unchanged
    /// (47 cells / 2 nodes) and the three back-link rows cost the same 21
    /// cells; the extra 81 cells / 3 nodes are the minimal two-row carriers,
    /// and at this node count their three nodes open one more directory page.
    /// A source that never reads `constructor` keeps the exact 3867 manifest.
    #[test]
    fn primitive_constructor_read_budget_and_gating() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = vm::SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let lower = |source: &'static str| {
            vm::SourcePlugin::parse_source(&crate::JAVASCRIPT_ES5_PLUGIN, source, &context)
                .expect("measured realm")
        };
        let measure = |source: &'static str| {
            let _ = lower(source);
            super::intrinsic_budget()
        };
        // The carrier charge only exists for a class whose global name the
        // source never spells, and never without the read.
        let node = 3 + layout::FUNCTION_CELLS as u64 + 2 * 7;
        assert_eq!(bootstrap_primitive_carrier_request_cells(false, false, false), 0);
        assert_eq!(bootstrap_primitive_carrier_request_cells(true, true, true), 0);
        assert_eq!(bootstrap_primitive_carrier_request_cells(true, false, false), 2 * node);
        assert_eq!(bootstrap_primitive_carrier_request_cells(true, false, true), node);
        assert_eq!(
            bootstrap_constructor_prototype_row_cells(false, true, false, false, false, false),
            0
        );
        assert_eq!(
            bootstrap_constructor_prototype_row_cells(true, true, false, false, false, false),
            21
        );
        let none = measure("var o={};");
        assert_eq!(none, (3867, 126));
        // `.prototype` keeps the plain accessor charge: no carrier is selected.
        let prototype_read = measure("function f(){};f.prototype;");
        assert_eq!(prototype_read.0 - none.0, 47);
        assert_eq!(prototype_read.1 - none.1, 2);
        let constructor_read = measure(
            "function read(){return typeof (1).constructor==='function';}read();",
        );
        let accessor = bootstrap_function_prototype_request_cells(true);
        let carriers = bootstrap_primitive_carrier_request_cells(true, false, false)
            + BOOLEAN_FUNCTION_CELLS;
        let rows = bootstrap_constructor_prototype_row_cells(true, true, false, false, false, false);
        assert_eq!(accessor, 47);
        assert_eq!(carriers, 81);
        assert_eq!(rows, 21);
        assert_eq!(
            constructor_read.0 - none.0,
            accessor + carriers + rows + layout::DIR_PAGE_CELLS as u64
        );
        assert_eq!(constructor_read.1 - none.1, 2 + 3);
        // Naming one carrier drops that class's minimal node and keeps the
        // three rows: the read only adds the two still-unspelled carriers.
        let number = measure("Number;");
        let number_read = measure(
            "Number;function read(){return typeof (1).constructor==='function';}read();",
        );
        assert_eq!(
            number_read.0 - number.0,
            accessor
                + bootstrap_primitive_carrier_request_cells(true, true, false)
                + BOOLEAN_FUNCTION_CELLS
                + rows
        );
        // No new permanent root: the carriers live in the private prototypes'
        // back-link rows, so the root frame keeps its eleven entries.
        let ir = lower("function read(){return typeof (1).constructor==='function';}read();");
        let bootstrap = ir
            .functions
            .iter()
            .find(|function| function.name == BOOTSTRAP_NAME)
            .expect("bootstrap")
            .index;
        let enter = ir
            .functions
            .iter()
            .find(|function| function.name == layout::ROOT_ENTER_HELPER)
            .expect("root enter")
            .index;
        let roots = ir.function_impls[&bootstrap]
            .body_stmts
            .iter()
            .find_map(|stmt| match stmt {
                S::Assign {
                    expr: E::Call(id, _, args, _, _),
                    ..
                } if *id == enter => match &args[1] {
                    E::Int(capacity) => Some(*capacity),
                    _ => panic!("nonconstant root capacity"),
                },
                _ => None,
            })
            .expect("permanent root frame");
        assert_eq!(roots, 11, "the carriers open no permanent root entry");
        eprintln!(
            "PRIMITIVE_CONSTRUCTOR_BUDGET none={none:?} prototype={prototype_read:?} constructor={constructor_read:?} number_read={number_read:?} accessor={accessor} carriers={carriers} rows={rows} page={} roots={roots}",
            layout::DIR_PAGE_CELLS
        );
    }

    #[test]
    fn number_to_string_row_budget_and_gating() {
        // The private Number prototype keeps one row (`hasOwnProperty`) unless a
        // source can name `toString`; the row and its function node cost seven
        // and twenty cells. The `toString` key String is a fixed key, so it is
        // not charged twice. Naming the method also selects the RangeError realm
        // (function, prototype and name: 65 cells, three nodes, one permanent
        // root) because an out-of-range radix throws it.
        let base_cells = bootstrap_request_cells_for(0, 11, false, false, false, false, false, false);
        let selected_cells = bootstrap_request_cells_for(0, 12, false, false, false, true, false, false);
        let nodes = bootstrap_node_count(false, false, false, true, false)
            - bootstrap_node_count(false, false, false, false, false);
        assert_eq!((base_cells, selected_cells), (2982, 2982 + 7 + 3 + 10 + 7 + 3));
        assert_eq!(selected_cells - base_cells, 7 + 3 + 10 + 7 + 3);
        assert_eq!(nodes, 1);
        eprintln!(
            "NUMBER_TO_STRING_BUDGET bootstrap={base_cells}->{selected_cells} row_cells=7 function_cells=20 string_cells=0 range_error_cells={} nodes_delta={nodes} range_error_nodes={RANGE_ERROR_EXTRA_NODES} roots_delta=1",
            RANGE_ERROR_EXTRA_CELLS
        );
        // The row coexists with every other selection without moving an
        // earlier root: the Number carrier, the String carrier and the row are
        // additive at the fixed root capacity the selection needs.
        let both = bootstrap_request_cells_for(0, 12, true, true, false, true, false, false);
        let parts = base_cells
            + (bootstrap_request_cells_for(0, 11, true, false, false, false, false, false) - base_cells)
            + (bootstrap_request_cells_for(0, 11, false, true, false, false, false, false) - base_cells)
            + (selected_cells - base_cells);
        assert_eq!(both, parts);
        assert_eq!(
            bootstrap_node_count(true, true, false, true, false),
            bootstrap_node_count(false, false, false, false, false)
                + (bootstrap_node_count(true, false, false, false, false)
                    - bootstrap_node_count(false, false, false, false, false))
                + (bootstrap_node_count(false, true, false, false, false)
                    - bootstrap_node_count(false, false, false, false, false))
                + 1
        );
        // Emission-level witness: naming `toString` adds exactly one row to one
        // permanent prototype and exactly one code-300 function node, and it
        // never moves the permanent root capacity.
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = vm::SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let lower =
            |source| vm::SourcePlugin::parse_source(&crate::JAVASCRIPT_ES5_PLUGIN, source, &context);
        let shape = |ir: &FrontendIr| {
            let bootstrap = ir
                .functions
                .iter()
                .find(|f| f.name == BOOTSTRAP_NAME)
                .unwrap()
                .index;
            let allocate = ir
                .functions
                .iter()
                .find(|f| f.name == ALLOC_NAME)
                .unwrap()
                .index;
            let body = &ir.function_impls[&bootstrap].body_stmts;
            let root_enter = ir
                .functions
                .iter()
                .find(|f| f.name == layout::ROOT_ENTER_HELPER)
                .unwrap()
                .index;
            let (permanent, capacity) = body
                .iter()
                .find_map(|stmt| match stmt {
                    S::Assign {
                        index,
                        expr: E::Call(id, _, args, _, _),
                        ..
                    } if *id == root_enter => {
                        let E::Int(capacity) = args[1] else {
                            panic!("nonconstant root capacity")
                        };
                        Some((index, capacity))
                    }
                    _ => None,
                })
                .expect("permanent root frame");
            let mut rows = Vec::new();
            let mut code300 = 0u64;
            for stmt in body {
                let S::Assign {
                    expr: E::Call(id, _, args, _, _),
                    ..
                } = stmt
                else {
                    continue;
                };
                if *id != allocate {
                    continue;
                }
                if let [_, E::Int(kind), _, E::Int(rows_), E::Int(code), _, _, E::Var(frame), E::Int(entry)] =
                    args.as_slice()
                {
                    if *code == super::NUMBER_TO_STRING_CODE {
                        code300 += 1;
                    }
                    if frame == permanent {
                        // Every permanent root is recorded, Function roots
                        // included: the RangeError realm appends a code-308
                        // Function constructor row and no existing root moves.
                        rows.push((*entry, *kind, *rows_, *code));
                    }
                }
            }
            (capacity, rows, code300)
        };
        let base = lower("var o={};").expect("default realm");
        let selected = lower("var o={};var s=o.toString;").expect("toString realm");
        let (base_capacity, mut base_rows, base_code300) = shape(&base);
        let (selected_capacity, mut selected_rows, selected_code300) = shape(&selected);
        // The gated empty String root is appended after the RangeError
        // constructor, so the permanent frame grows by one more entry.
        assert_eq!((base_capacity, selected_capacity), (11, 13));
        assert_eq!((base_code300, selected_code300), (0, 1));
        base_rows.sort();
        selected_rows.sort();
        // Complete permanent-root list, Function roots included. Naming
        // `toString` may only append the one public RangeError constructor
        // root (kind Function, code 308) after every earlier root: the identity
        // (entry, kind, code) of each existing root is unchanged, and only the
        // selected Number prototype's own row count grows, by the one
        // `toString` row it gains. The gated empty String root is published
        // through node_publish rather than allocate, so it is not part of this
        // allocation list; it occupies the appended entry `selected_capacity-1`.
        assert_eq!(
            selected_rows.len(),
            base_rows.len() + 1,
            "exactly one permanent root is added"
        );
        let identity = |row: &(i64, i64, i64, i64)| (row.0, row.1, row.3);
        assert_eq!(
            selected_rows[..base_rows.len()]
                .iter()
                .map(identity)
                .collect::<Vec<_>>(),
            base_rows.iter().map(identity).collect::<Vec<_>>(),
            "every existing permanent root keeps its entry, kind and code"
        );
        let added = *selected_rows.last().unwrap();
        assert_eq!(
            added,
            (
                base_capacity,
                layout::KIND_FUNCTION,
                2, // `length` and `prototype` data rows
                super::RANGE_ERROR_CODE
            ),
            "the only new permanent root is the code-308 RangeError constructor"
        );
        let changed = base_rows
            .iter()
            .zip(&selected_rows)
            .filter(|(left, right)| left.2 != right.2)
            .collect::<Vec<_>>();
        // `toString` now also selects the Function prototype's gated `name`
        // accessor and `toString` data row, so two prototypes grow: the Number
        // prototype by its one `toString` row and Function.prototype by two.
        assert_eq!(changed.len(), 2, "two prototype row counts change");
        let number_proto = changed
            .iter()
            .find(|(left, _)| left.1 == layout::KIND_OBJECT)
            .expect("Number prototype row");
        assert_eq!(
            number_proto.1.2,
            number_proto.0.2 + 1,
            "the Number prototype gains one row"
        );
        let function_proto = changed
            .iter()
            .find(|(left, _)| left.1 == layout::KIND_FUNCTION)
            .expect("Function prototype rows");
        assert_eq!(
            function_proto.1.2,
            function_proto.0.2 + 2,
            "Function.prototype gains the name accessor and toString row"
        );
        let (left, right) = number_proto;
        // Charged initial budget per selection: the three rows are independent
        // and each one pays nothing when the source never names it. Naming
        // `toString` also buys the optional RangeError realm (65 cells, three
        // nodes, one 3-cell permanent root) because an out-of-range radix
        // throws it, plus the private empty-String root entry (3 cells) for
        // anonymous function names. When those three nodes open the next
        // 64-row directory page the page charge (451 cells) follows; `row` is
        // the only measured selection here that crosses that boundary.
        let measure = |source: &'static str| {
            vm::SourcePlugin::parse_source(&crate::JAVASCRIPT_ES5_PLUGIN, source, &context)
                .expect("measured realm");
            super::intrinsic_budget()
        };
        let none = measure("var o={};");
        let number = measure("var o={};Number;");
        let text = measure("var o={};String;");
        let row = measure("var o={};var s=o.toString;");
        let all = measure("var o={};Number;String;var s=o.toString;");
        let number_row = measure("var o={};Number;var s=o.toString;");
        let string_row = measure("var o={};String;var s=o.toString;");
        eprintln!(
            "NUMBER_PROTO_MATRIX none={none:?} number={number:?} string={text:?} row={row:?} all={all:?} number_row={number_row:?} string_row={string_row:?}"
        );
        assert_eq!(none, (3867, 126));
        assert_eq!(number, (4695, 141));
        assert_eq!(text, (4439, 131));
        // Naming `toString` also buys the Function-prototype name accessor and
        // the `toString` row: 54 feature cells, two feature nodes and the
        // 3-cell private empty-String root entry.
        assert_eq!(row, (4470, 132));
        assert_eq!(number_row, (4847, 147));
        assert_eq!(string_row, (4591, 137));
        assert_eq!(all, (4968, 152));
        let range_charge = RANGE_ERROR_EXTRA_CELLS + layout::FRAME_CELLS_PER_ENTRY as u64;
        let function_api = bootstrap_function_api_request_cells(true, true);
        let name_empty_root = layout::FRAME_CELLS_PER_ENTRY as u64;
        assert_eq!(row.0 - none.0, 27 + name_empty_root + range_charge + layout::DIR_PAGE_CELLS as u64 + function_api);
        assert_eq!(number_row.0 - number.0, 27 + name_empty_root + range_charge + function_api);
        assert_eq!(string_row.0 - text.0, 27 + name_empty_root + range_charge + function_api);
        assert_eq!(row.1, none.1 + 1 + RANGE_ERROR_EXTRA_NODES + 2);
        // Node counts are additive over the three selections. Cells are not:
        // the carriers share the StringNumericLiteral parser, Number formatter
        // and dispatch helpers, so the joint sum is smaller than the parts.
        // `all` already paid the Number toString row and the RangeError realm
        // through the `Number`/`String` carriers before this feature existed:
        // naming `toString` adds only the 54-cell Function-prototype share,
        // its two nodes and the 3-cell private empty-String root entry.
        assert_eq!(all.0 - 4911, 54 + name_empty_root);
        assert_eq!(all.1 - 150, 2);
        eprintln!(
            "NUMBER_TO_STRING_EMIT capacity={base_capacity}->{selected_capacity} row_entry={} kind={} rows={}->{} code300={base_code300}->{selected_code300}",
            left.0, left.1, left.2, right.2
        );
    }
    #[test]
    fn number_manifest_budget_and_coexistence() {
        assert_eq!((bootstrap_node_count(false, false, false, false, false), bootstrap_node_count(true, false, false, false, false)), (124, 135));
        let base_cells = bootstrap_request_cells_for(0, 11, false, false, false, false, false, false);
        let selected_cells = bootstrap_request_cells_for(0, 12, true, false, false, false, false, false);
        let root_cells = layout::FRAME_CELLS_PER_ENTRY as u64;
        let fixed_overhead = 2 * layout::FRAME_FIXED_CELLS as u64 + 19 * root_cells + layout::DIR_PAGE_CELLS as u64;
        assert_eq!((base_cells, selected_cells, root_cells), (2982, 3254, 3));
        assert_eq!((base_cells - fixed_overhead, selected_cells - fixed_overhead - root_cells), (2466, 2735));
        eprintln!("NUMBER_MANIFEST nodes=124->135 node_cells=2446->2708 root_cells_delta={root_cells} bootstrap={base_cells}->{selected_cells}");
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = vm::SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let lower = |source| vm::SourcePlugin::parse_source(&crate::JAVASCRIPT_ES5_PLUGIN, source, &context);
        let allocations = |ir: &FrontendIr| {
            let bootstrap = ir.functions.iter().find(|f| f.name == BOOTSTRAP_NAME).unwrap().index;
            let allocate = ir.functions.iter().find(|f| f.name == ALLOC_NAME).unwrap().index;
            let body = &ir.function_impls[&bootstrap].body_stmts;
            let root_enter = ir.functions.iter().find(|f| f.name == layout::ROOT_ENTER_HELPER).unwrap().index;
            let (permanent, capacity) = body.iter().find_map(|stmt| match stmt {
                S::Assign { index, expr: E::Call(id, _, args, _, _), .. } if *id == root_enter => {
                    let E::Int(capacity) = args[1] else { panic!("nonconstant root capacity") };
                    Some((index, capacity))
                }
                _ => None,
            }).expect("permanent root frame");
            let roots = body.iter().filter_map(|stmt| {
                let S::Assign { expr: E::Call(id, _, args, _, _), .. } = stmt else { return None };
                if *id != allocate { return None; }
                match args.as_slice() {
                    [_, E::Int(kind), _, E::Int(rows), E::Int(code), _, _, E::Var(frame), E::Int(entry)] if frame == permanent => {
                        Some((*entry, *kind, *rows, *code))
                    }
                    _ => None,
                }
            }).collect::<Vec<_>>();
            (capacity, roots)
        };
        let base = lower("Object;").expect("default realm");
        let selected = lower("Number;").expect("Number realm");
        let (base_capacity, mut base_roots) = allocations(&base);
        let (capacity, roots) = allocations(&selected);
        assert_eq!((base_capacity, capacity), (11, 12));
        base_roots.push((11, 4, 12, 299));
        assert_eq!(roots, base_roots);

        let coexist = "Math;parseInt;parseFloat;encodeURI;JSON;";
        let base = lower(coexist).expect("previous manifest");
        let selected_source = format!("{coexist}Number;");
        let selected = lower(&selected_source).expect("coexisting manifest");
        let (base_capacity, mut base_roots) = allocations(&base);
        let (capacity, roots) = allocations(&selected);
        assert_eq!(capacity, base_capacity + 1);
        base_roots.push((base_capacity, 4, 12, 299));
        assert_eq!(roots, base_roots);

        let array_source = |name: &str, entries| {
            let array = std::iter::repeat_n("o", entries).collect::<Vec<_>>().join(",");
            format!("var o={{}};{name};[{array}];")
        };
        let accepts = |name, entries| {
            let source = array_source(name, entries);
            match vm::SourcePlugin::parse_source(&crate::JAVASCRIPT_ES5_PLUGIN, &source, &context) {
                Ok(_) => true,
                Err(err) => {
                    assert!(format!("{err:?}").contains("65536 cell budget"), "{err:?}");
                    false
                }
            }
        };
        // Measure adjacent edges with the same tagged-array source generator.
        let boundary = |name, mut refused| {
            let mut admitted = 0;
            assert!(accepts(name, admitted));
            assert!(!accepts(name, refused));
            while refused - admitted > 1 {
                let mid = admitted + (refused - admitted) / 2;
                if accepts(name, mid) {
                    admitted = mid;
                } else {
                    refused = mid;
                }
            }
            (admitted, refused)
        };
        let object = boundary("Object", 9000);
        let number = boundary("Number", 9000);
        assert!(number.0 < object.0);
        eprintln!("NUMBER_ARRAY_BOUNDARY base={}/{} selected={}/{}", object.0, object.1, number.0, number.1);
    }

    #[test]
    fn string_namespace_budget() {
        let base_cells = bootstrap_request_cells_for(0, 11, false, false, false, false, false, false);
        let selected_cells = bootstrap_request_cells_for(0, 12, false, true, false, false, false, false);
        let nodes = bootstrap_node_count(false, true, false, false, false) - bootstrap_node_count(false, false, false, false, false);
        let root_cells = layout::FRAME_CELLS_PER_ENTRY as u64;
        // Carrier 4 rows = 41, two one-row methods = 40, keys 18+19, one root = 3.
        assert_eq!((base_cells, selected_cells - base_cells, nodes, root_cells), (2982, 121, 5, 3));
        eprintln!(
            "STRING_NS_BUDGET roots=1 property_rows=3 function_nodes=3 key_nodes=2 cells_delta={} nodes_delta={nodes} carrier_code=312 fromCharCode=310 fromCodePoint=311 root_cells={root_cells}",
            selected_cells - base_cells
        );
    }

    #[test]
    fn array_callbacks_budget() {
        let baseline = 2945;
        let rows = 7;
        let functions = 3 + 10 + 7;
        let keys = 6 + 4;
        let base = bootstrap_request_cells_for(0, 11, false, false, false, false, false, false);
        let nodes = bootstrap_node_count(false, false, false, false, false);
        assert_eq!(base, baseline + rows + functions + keys);
        assert_eq!(nodes, 121 + 3);
        eprintln!("ARRAY_SORT_BUDGET bootstrap={baseline}->{base} row_cells={rows} function_cells={functions} key_cells={keys} nodes=121->{nodes} roots_delta=0 directory_delta=0");
    }

    fn heritage_buffer_budget(test: impl FnOnce(&dyn Fn(&str) -> (u64, u64, i64), &dyn Fn(&str) -> (usize, usize))) {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = vm::SourceFrontendContext {
            source_id: 0, original_path: None, phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog), imports: &[],
        };
        let measure = |source: &str| {
            let ir = vm::SourcePlugin::parse_source(&crate::JAVASCRIPT_ES5_PLUGIN, source, &context).unwrap();
            let (cells, nodes) = intrinsic_budget();
            let bootstrap = ir.functions.iter().find(|f| f.name == BOOTSTRAP_NAME).unwrap().index;
            let enter = ir.functions.iter().find(|f| f.name == layout::ROOT_ENTER_HELPER).unwrap().index;
            let capacity = ir.function_impls[&bootstrap].body_stmts.iter().find_map(|stmt| match stmt {
                S::Assign { expr: E::Call(target, _, args, ..), .. } if *target == enter => match args.as_slice() {
                    [_, E::Int(n)] => Some(*n), _ => None,
                }, _ => None,
            }).unwrap();
            (cells, nodes, capacity)
        };
        let boundary = |source: &str| {
            let accepts = |entries| {
                let array = std::iter::repeat_n("o", entries).collect::<Vec<_>>().join(",");
                match vm::SourcePlugin::parse_source(
                    &crate::JAVASCRIPT_ES5_PLUGIN,
                    &format!("var o={{}};{source};[{array}];"),
                    &context,
                ) {
                    Ok(_) => true,
                    Err(error) => {
                        assert!(format!("{error:?}").contains("65536 cell budget"), "{error:?}");
                        false
                    }
                }
            };
            let (mut lo, mut hi) = (0, 9000);
            assert!(accepts(lo));
            assert!(!accepts(hi));
            while lo + 1 < hi {
                let mid = lo + (hi - lo) / 2;
                if accepts(mid) { lo = mid; } else { hi = mid; }
            }
            (lo, hi)
        };
        test(&measure, &boundary);
    }

    #[test]
    fn typed_array176_budget() {
        heritage_buffer_budget(|measure, boundary| {
            let base = measure("ArrayBuffer;");
            let view = measure("ArrayBuffer; Uint8Array;");
            let limits = boundary("ArrayBuffer; Uint8Array");
            eprintln!("TYPED_ARRAY176_BUDGET base={base:?} view={view:?} accepts={limits:?}");
            assert_eq!(base, (4474, 133, 13));
            // Shared prototype adds four functions and four key strings:
            // 155 cells including the rows and raw headers, no permanent roots.
            assert_eq!(view, (4905, 153, 14));
            assert_eq!(limits, (7574, 7575));
        });
    }

    #[test]
    fn typed_array_integer_budget() {
        heritage_buffer_budget(|measure, boundary| {
            let all = "ArrayBuffer; Uint8Array; Int8Array; Uint8ClampedArray; Int16Array; Uint16Array; Int32Array; Uint32Array";
            let mixed = measure(all);
            let limits = boundary(all);
            eprintln!("TYPED_ARRAY_INTEGER_BUDGET all={mixed:?} accepts={limits:?}");
            // Six additional pairs at 56 cells each, plus six permanent root rows.
            assert_eq!(mixed, (5259, 165, 20));
            assert_eq!(limits, (7528, 7529));
        });
    }

    #[test]
    fn optional_error_budget() {
        heritage_buffer_budget(|measure, _| {
            let views = "ArrayBuffer; Uint8Array; Int8Array; Uint8ClampedArray; Int16Array; Uint16Array; Int32Array; Uint32Array; Float32Array; Float64Array;";
            let base = measure(views);
            let eval = measure(&format!("{views} EvalError;"));
            let reference = measure(&format!("{views} ReferenceError;"));
            let both = measure(&format!("{views} EvalError; ReferenceError;"));
            let shadowed = measure(&format!("{views} var EvalError = 1; var ReferenceError = 2; EvalError; ReferenceError;"));
            let dead_control = measure(&format!("{views} function f() {{ return 1; }}"));
            let dead = measure(&format!("{views} function f() {{ return 1; EvalError; ReferenceError(\"x\"); new EvalError(\"x\"); }}"));
            eprintln!("OPTIONAL_ERROR_BUDGET base={base:?} eval={eval:?} reference={reference:?} both={both:?} shadowed={shadowed:?} dead={dead:?} dead_control={dead_control:?}");
            // Per error: 27-cell Function + 22-cell prototype + a 6-cell
            // String header and its code units + a 3-cell permanent root.
            // All four cases occupy the same number of directory pages.
            assert_eq!(base, (5377, 169, 22));
            assert_eq!(eval, (5444, 172, 23));
            assert_eq!(reference, (5449, 172, 23));
            assert_eq!(both, (5516, 175, 24));
            assert_eq!(shadowed, base);
            assert_eq!(dead, dead_control);
        });
    }

    #[test]
    fn public_error_348_budget() {
        heritage_buffer_budget(|measure, _| {
            let views = "ArrayBuffer; Uint8Array; Int8Array; Uint8ClampedArray; Int16Array; Uint16Array; Int32Array; Uint32Array; Float32Array; Float64Array;";
            let m = |tail| measure(&format!("{views}\n{tail}"));
            let base = m("");
            let syntax = m("SyntaxError;");
            let internal = m("InternalError;");
            let uri = m("URIError;");
            let new_errors = m("SyntaxError; InternalError; URIError;");
            let all = m("EvalError; ReferenceError; SyntaxError; InternalError; URIError;");
            let both_old = m("EvalError; ReferenceError;");
            let decode = m("decodeURIComponent;");
            let mixed = m("decodeURIComponent; URIError;");
            let text = m("// SyntaxError InternalError URIError\nvar s = \"SyntaxError InternalError URIError\";");
            let shadowed = m("var SyntaxError = 1; var InternalError = 2; var URIError = 3; SyntaxError; InternalError; URIError;");
            let dead_control = m("function f() { return 1; }");
            let dead = m("function f() { return 1; SyntaxError; InternalError(\"x\"); new URIError(\"x\"); }");
            let delta = |a: (u64, u64, i64), b: (u64, u64, i64)| (a.0 - b.0, a.1 - b.1, a.2 - b.2);
            eprintln!("PUBLIC_ERROR_348_BUDGET base={base:?} syntax={syntax:?} internal={internal:?} uri={uri:?} all={all:?} both_old={both_old:?} decode={decode:?} mixed={mixed:?} text={text:?}");
            assert_eq!(base, (5377, 169, 22));
            assert_eq!(delta(syntax, base), (27 + 22 + 6 + 11 + 3, 3, 1));
            assert_eq!(delta(internal, base), (27 + 22 + 6 + 13 + 3, 3, 1));
            assert_eq!(delta(uri, base), (27 + 22 + 6 + 8 + 2 * 3, 3, 2));
            assert_eq!(delta(new_errors, base), (69 + 71 + 69, 9, 4));
            assert_eq!(delta(all, both_old), (69 + 71 + 69, 9, 4));
            assert_eq!(delta(mixed, decode), (27 + 3, 1, 1));
            assert_eq!(text, base);
            assert_eq!(shadowed, base);
            assert_eq!(dead, dead_control);
        });
    }

    #[test]
    fn optional_error_names_in_text_budget() {
        heritage_buffer_budget(|measure, _| {
            let views = "ArrayBuffer; Uint8Array; Int8Array; Uint8ClampedArray; Int16Array; Uint16Array; Int32Array; Uint32Array; Float32Array; Float64Array;";
            let source = format!("{views}\n// EvalError\nvar s = \"EvalError\";");
            let text = measure(&source);
            eprintln!("OPTIONAL_ERROR_TEXT_BUDGET cells={} nodes={} roots={}", text.0, text.1, text.2);
            assert_eq!(text, (5377, 169, 22));
        });
    }

    #[test]
    fn typed_array_float_budget() {
        heritage_buffer_budget(|measure, boundary| {
            let all = "ArrayBuffer; Uint8Array; Int8Array; Uint8ClampedArray; Int16Array; Uint16Array; Int32Array; Uint32Array; Float32Array; Float64Array";
            let mixed = measure(all);
            let limits = boundary(all);
            eprintln!("TYPED_ARRAY_FLOAT_BUDGET all={mixed:?} accepts={limits:?}");
            // Two additional pairs (56 cells each) and two permanent root rows.
            assert_eq!(mixed, (5377, 169, 22));
            assert_eq!(limits, (7512, 7513));
        });
    }

    #[test]
    fn array_buffer_budget() {
        heritage_buffer_budget(|measure, boundary| {
        let base = measure("Object;");
        let range = measure("RangeError;");
        let buffer = measure("ArrayBuffer;");
        let both = measure("RangeError; ArrayBuffer;");
        let array = measure("Array(1);");
        let arrays = measure("Array(1); ArrayBuffer;");
        let pages = |nodes| (nodes - 1) / layout::PAGE_ROWS as u64;
        for (old, new) in [(range, buffer), (range, both), (array, arrays)] {
            let dir = (pages(new.1) - pages(old.1)) * layout::DIR_PAGE_CELLS as u64;
            assert_eq!((new.0 - old.0, new.1 - old.1, new.2 - old.2), (85 + 3 + dir, 4, 1));
        }
        assert_eq!((buffer.1 - base.1, buffer.2 - base.2), (7, 2));
        let (base_limit, buffer_limit) = (boundary("Object"), boundary("ArrayBuffer"));
        assert_eq!(base_limit, (7704, 7705));
        assert!(buffer_limit.0 < base_limit.0);
        eprintln!("ARRAY_BUFFER176_ACCEPTS base={base_limit:?} buffer={buffer_limit:?}");
        eprintln!("ARRAY_BUFFER176_BUDGET base={base:?} range={range:?} buffer={buffer:?} both={both:?} array={array:?} arrays={arrays:?}");
        });
    }

    #[test]
    fn array_namespace_budget() {
        let base = bootstrap_request_cells_for(0, 11, false, false, false, false, false, false);
        let selected = bootstrap_request_cells_for(0, 12, false, false, true, false, false, false);
        let nodes = bootstrap_node_count(false, false, true, false, false) - bootstrap_node_count(false, false, false, false, false);
        let root_cells = layout::FRAME_CELLS_PER_ENTRY as u64;
        // Carrier header 13 + three 7-cell rows; method header 13 + one
        // 7-cell length row; key 6 + 7 UTF-16 units; one 3-cell root entry.
        assert_eq!((base, selected - base, nodes, root_cells), (2982, 70, 3, 3));
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = vm::SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let lower = |source: &str| vm::SourcePlugin::parse_source(&crate::JAVASCRIPT_ES5_PLUGIN, source, &context);
        lower("Object;").expect("default realm");
        let (base_cells, base_nodes) = intrinsic_budget();
        let namespace = lower("Array.isArray();").expect("Array namespace without constructor");
        let (namespace_cells, namespace_nodes) = intrinsic_budget();
        assert_eq!((namespace_cells - base_cells, namespace_nodes - base_nodes), (521, 3));
        assert!(!namespace.functions.iter().any(|f| f.name == "__compat_es5_array_constructor"));
        let constructor = lower("Array(1);").expect("Array constructor realm");
        assert!(constructor.functions.iter().any(|f| f.name == "__compat_es5_array_constructor"));
        let (selected_cells, selected_nodes) = intrinsic_budget();
        assert_eq!(selected_nodes - base_nodes, 3 + RANGE_ERROR_EXTRA_NODES);
        let pages = |count: u64| (count - 1) / layout::PAGE_ROWS as u64;
        let directory_delta = (pages(selected_nodes) - pages(base_nodes)) * layout::DIR_PAGE_CELLS as u64;
        let range_cells = RANGE_ERROR_EXTRA_CELLS + root_cells;
        assert_eq!(selected_cells - base_cells, 70 + range_cells + directory_delta);
        eprintln!("ARRAY_NS_BUDGET bootstrap={base}->{selected} realm_cells={base_cells}->{namespace_cells}->{selected_cells} realm_nodes={base_nodes}->{namespace_nodes}->{selected_nodes} roots=11->12->13 extra_range_cells={range_cells} directory_delta={directory_delta}");

        // A `.prototype` read on the Array carrier pays the gated accessor
        // (47 cells, two nodes) plus the Array method table's `constructor`
        // row (7 cells), and no new permanent root.
        let proto_cells = namespace_cells
            + bootstrap_function_prototype_request_cells(true)
            + bootstrap_constructor_prototype_row_cells(true, false, false, false, true, false);
        let proto_nodes = namespace_nodes + bootstrap_function_prototype_nodes(true);
        for (source, needs_constructor, prototype_read) in [
            ("Array.prototype;", false, true),
            ("Array.prototype.isArray;", false, true),
            (r#"Array["prototype"];"#, false, true),
            ("var p = Array.prototype; p;", false, true),
            ("var a = []; Array.isArray(a);", false, false),
            (r#"Array["isArray"]([]);"#, false, false),
            ("var a = []; var g = Array.isArray; g(a);", false, false),
            ("Array(1);", true, false),
            ("var f = Array; f(1);", true, false),
            ("new Array(3);", true, false),
        ] {
            let ir = lower(source).unwrap_or_else(|err| panic!("{source}: {err:?}"));
            let (cells, nodes) = intrinsic_budget();
            let helper = |name| ir.functions.iter().find(|f| f.name == name).unwrap().index;
            let enter = helper(layout::ROOT_ENTER_HELPER);
            let roots = ir.function_impls[&helper(BOOTSTRAP_NAME)]
                .body_stmts
                .iter()
                .find_map(|stmt| match stmt {
                    S::Assign { expr: E::Call(id, _, args, _, _), .. } if *id == enter => {
                        let E::Int(capacity) = args[1] else { panic!("nonconstant root capacity") };
                        Some(capacity)
                    }
                    _ => None,
                })
                .expect("permanent root frame");
            eprintln!("ARRAY_NS_FORM {source:?} cells={cells} nodes={nodes} roots={roots}");
            let expected = if needs_constructor {
                (selected_cells, selected_nodes, 13)
            } else if prototype_read {
                (proto_cells, proto_nodes, 12)
            } else {
                (namespace_cells, namespace_nodes, 12)
            };
            assert_eq!((cells, nodes, roots), expected, "{source}");
            for name in ["__compat_es5_array_constructor", "__compat_es5_new_range_error"] {
                assert_eq!(ir.functions.iter().any(|f| f.name == name), needs_constructor, "{source}: {name}");
            }
        }

        let accepts = |name: &str, entries| {
            let array = std::iter::repeat_n("o", entries).collect::<Vec<_>>().join(",");
            match lower(&format!("var o={{}};{name};[{array}];")) {
                Ok(_) => true,
                Err(error) => {
                    assert!(format!("{error:?}").contains("65536 cell budget"), "{error:?}");
                    false
                }
            }
        };
        let boundary = |name| {
            let mut admitted = 0;
            let mut refused = 9000;
            assert!(accepts(name, admitted));
            assert!(!accepts(name, refused));
            while refused - admitted > 1 {
                let mid = admitted + (refused - admitted) / 2;
                if accepts(name, mid) {
                    admitted = mid;
                } else {
                    refused = mid;
                }
            }
            (admitted, refused)
        };
        let object = boundary("Object");
        let namespace = boundary("Array.isArray()");
        let constructor = boundary("Array(1)");
        eprintln!("ARRAY_NS_ACCEPTS object={object:?} is_array={namespace:?} constructor={constructor:?}");
        assert_eq!(object, (7704, 7705));
        // The historical 7259/7260 used a bare `Array` expression. This
        // probe includes the source-side cost of a static method call.
        assert_eq!(namespace, (7634, 7635));
        assert!(constructor.0 < namespace.0);
    }

    #[test]
    fn boolean_call_budget() {
        assert_eq!((bootstrap_node_count(false, false, false, false, false), bootstrap_node_count(true, false, false, false, false)), (124, 135));
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = vm::SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let lower = |source| vm::SourcePlugin::parse_source(&crate::JAVASCRIPT_ES5_PLUGIN, source, &context);
        let allocations = |ir: &FrontendIr| {
            let bootstrap = ir.functions.iter().find(|f| f.name == BOOTSTRAP_NAME).unwrap().index;
            let allocate = ir.functions.iter().find(|f| f.name == ALLOC_NAME).unwrap().index;
            let body = &ir.function_impls[&bootstrap].body_stmts;
            let root_enter = ir.functions.iter().find(|f| f.name == layout::ROOT_ENTER_HELPER).unwrap().index;
            let (permanent, capacity) = body.iter().find_map(|stmt| match stmt {
                S::Assign { index, expr: E::Call(id, _, args, _, _), .. } if *id == root_enter => {
                    let E::Int(capacity) = args[1] else { panic!("nonconstant root capacity") };
                    Some((index, capacity))
                }
                _ => None,
            }).expect("permanent root frame");
            let roots = body.iter().filter_map(|stmt| {
                let S::Assign { expr: E::Call(id, _, args, _, _), .. } = stmt else { return None };
                if *id != allocate { return None; }
                match args.as_slice() {
                    [_, E::Int(kind), _, E::Int(rows), E::Int(code), _, _, E::Var(frame), E::Int(entry)] if frame == permanent => {
                        Some((*entry, *kind, *rows, *code))
                    }
                    _ => None,
                }
            }).collect::<Vec<_>>();
            (capacity, roots)
        };
        let base = lower("Object;").expect("default realm");
        let (base_cells, base_nodes) = intrinsic_budget();
        let selected = lower("Boolean;").expect("Boolean realm");
        let (cells, nodes) = intrinsic_budget();
        let (base_capacity, mut base_roots) = allocations(&base);
        let (capacity, roots) = allocations(&selected);
        assert_eq!(capacity, base_capacity + 1);
        base_roots.push((base_capacity, 4, 2, BOOLEAN_CODE));
        assert_eq!(roots, base_roots);
        assert_eq!(nodes, base_nodes + 1);
        assert_eq!(cells, base_cells + BOOLEAN_FUNCTION_CELLS + layout::FRAME_CELLS_PER_ENTRY as u64);
        eprintln!(
            "BOOLEAN_BUDGET cells={base_cells}->{cells} delta={} nodes={base_nodes}->{nodes} delta={} root={base_capacity}->{capacity} code={BOOLEAN_CODE}",
            cells - base_cells,
            nodes - base_nodes,
        );
    }

    #[test]
    fn range_error_budget() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = vm::SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let lower = |source| vm::SourcePlugin::parse_source(&crate::JAVASCRIPT_ES5_PLUGIN, source, &context);
        let allocations = |ir: &FrontendIr| {
            let bootstrap = ir.functions.iter().find(|f| f.name == BOOTSTRAP_NAME).unwrap().index;
            let allocate = ir.functions.iter().find(|f| f.name == ALLOC_NAME).unwrap().index;
            let body = &ir.function_impls[&bootstrap].body_stmts;
            let root_enter = ir.functions.iter().find(|f| f.name == layout::ROOT_ENTER_HELPER).unwrap().index;
            let (permanent, capacity) = body.iter().find_map(|stmt| match stmt {
                S::Assign { index, expr: E::Call(id, _, args, _, _), .. } if *id == root_enter => {
                    let E::Int(capacity) = args[1] else { panic!("nonconstant root capacity") };
                    Some((index, capacity))
                }
                _ => None,
            }).expect("permanent root frame");
            let roots = body.iter().filter_map(|stmt| {
                let S::Assign { expr: E::Call(id, _, args, _, _), .. } = stmt else { return None };
                if *id != allocate { return None; }
                match args.as_slice() {
                    [_, E::Int(kind), _, E::Int(rows), E::Int(code), _, _, E::Var(frame), E::Int(entry)] if frame == permanent => {
                        Some((*entry, *kind, *rows, *code))
                    }
                    _ => None,
                }
            }).collect::<Vec<_>>();
            (capacity, roots)
        };
        let base = lower("Object;").expect("default realm");
        let (base_cells, base_nodes) = intrinsic_budget();
        let selected = lower("RangeError;").expect("RangeError realm");
        let (cells, nodes) = intrinsic_budget();
        let (base_capacity, mut base_roots) = allocations(&base);
        let (capacity, roots) = allocations(&selected);
        assert_eq!(capacity, base_capacity + 1);
        base_roots.push((base_capacity, 4, 2, RANGE_ERROR_CODE));
        base_roots.sort();
        let mut roots = roots;
        roots.sort();
        assert_eq!(roots, base_roots);
        assert_eq!(nodes, base_nodes + RANGE_ERROR_EXTRA_NODES);
        let page = layout::PAGE_ROWS as u64;
        let dir = layout::DIR_PAGE_CELLS as u64;
        let frame = layout::FRAME_CELLS_PER_ENTRY as u64;
        let dir_delta = ((nodes - 1) / page) * dir - ((base_nodes - 1) / page) * dir;
        assert_eq!(cells, base_cells + RANGE_ERROR_EXTRA_CELLS + frame + dir_delta);
        let bootstrap = selected.functions.iter().find(|f| f.name == BOOTSTRAP_NAME).unwrap().index;
        let allocate = selected.functions.iter().find(|f| f.name == ALLOC_NAME).unwrap().index;
        let body = &selected.function_impls[&bootstrap].body_stmts;
        let root_enter = selected.functions.iter().find(|f| f.name == layout::ROOT_ENTER_HELPER).unwrap().index;
        let stage_frame = body.iter().filter_map(|stmt| match stmt {
            S::Assign { index, expr: E::Call(id, _, args, _, _), .. } if *id == root_enter => {
                let E::Int(8) = args[1] else { return None };
                Some(*index)
            }
            _ => None,
        }).next().expect("stage frame");
        let stage = body.iter().filter_map(|stmt| {
            let S::Assign { expr: E::Call(id, _, args, _, _), .. } = stmt else { return None };
            if *id != allocate { return None; }
            match args.as_slice() {
                [_, E::Int(kind), _, E::Int(rows), E::Int(code), _, _, E::Var(frame), E::Int(entry)] if *frame == stage_frame && *kind == 3 && *rows == 2 && *code == 0 => {
                    Some((*entry, *kind, *rows, *code))
                }
                _ => None,
            }
        }).collect::<Vec<_>>();
        assert_eq!(stage, vec![(RANGE_ERROR_STAGE_ENTRY, 3, 2, 0)]);
        eprintln!(
            "RANGE_ERROR_BUDGET function_node={RANGE_ERROR_FUNCTION_CELLS} property_rows=14 proto_node={RANGE_ERROR_PROTO_CELLS} property_rows=14 name_string={RANGE_ERROR_NAME_CELLS} roots=1 frame_cells={frame} saved_vs_dual_root=3 dir_delta={dir_delta} cells={base_cells}->{cells} delta={} nodes={base_nodes}->{nodes} delta={} roots={base_capacity}->{capacity} code={RANGE_ERROR_CODE} micro_divergence=prototype_holds_name_and_empty_message_instance_has_no_own_message_not_ERROR_DEF",
            cells - base_cells,
            nodes - base_nodes,
        );
    }

    #[test]
    fn number_parse_namespace_budget() {
        let property_keys: u64 = super::super::math::GLOBALS[..NUMBER_PARSE_METHODS]
            .iter()
            .map(|(name, _)| 6 + name.encode_utf16().count() as u64)
            .sum();
        let property_cells = 7 * 3 + property_keys;
        assert_eq!(property_cells, 51, "property rows plus parseInt/parseFloat keys");
        let (fn_cells, fn_nodes) = super::super::math::number_parse_static_request(0);
        assert_eq!((fn_cells, fn_nodes), (40, 2));
        assert_eq!(property_cells + fn_cells, 91);
        assert_eq!(fn_nodes + 2, 4, "two function nodes plus two key nodes");
        let (key_cells, _) = super::super::math::default_value_key_request();
        let path_delta = (4 + 3 * 6 + 4 + 3 * 4) as u64 + key_cells;
        assert_eq!(path_delta, 65);
        let increment = path_delta + fn_cells + property_cells;
        assert_eq!(increment, 156);
        let (shared_cells, _) = super::super::math::number_parse_static_request(0b11);
        assert_eq!(shared_cells, 0, "selected bare globals are not charged again");
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = vm::SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let parse = |source| {
            vm::SourcePlugin::parse_source(&crate::JAVASCRIPT_ES5_PLUGIN, source, &context)
                .unwrap_or_else(|err| panic!("{source}: {err:?}"));
            intrinsic_budget()
        };
        let (object_cells, object_nodes) = parse("Object;");
        let (number_cells, number_nodes) = parse("Number;");
        let (globals_cells, globals_nodes) = parse("parseInt;parseFloat;");
        let (both_cells, both_nodes) = parse("parseInt;parseFloat;Number;");
        let page = layout::PAGE_ROWS as u64;
        let directory = |nodes: u64| ((nodes - 1) / page) * layout::DIR_PAGE_CELLS as u64;
        let number_dir_delta = directory(number_nodes) - directory(object_nodes);
        let both_dir_delta = directory(both_nodes) - directory(globals_nodes);
        let namespace = bootstrap_request_cells_for(0, 12, true, false, false, false, false, false) - bootstrap_request_cells_for(0, 11, false, false, false, false, false, false);
        assert_eq!(namespace - property_cells, 221, "non-parse Number namespace is unchanged");
        assert_eq!(bootstrap_request_cells_for(0, 11, false, false, false, false, false, false), 2982, "no-Number control includes Array prototype methods");
        eprintln!(
            "NUMBER_PARSE_BUDGET property={property_cells} new_functions={fn_cells}/{fn_nodes} path_delta={path_delta} increment={increment} object_cells={object_cells} object_nodes={object_nodes} probe_number_minus_object={} nodes={} globals_plus_number={} expected_number_only={} expected_with_globals={} number_dir_delta={number_dir_delta} both_dir_delta={both_dir_delta}",
            number_cells - object_cells,
            number_nodes - object_nodes,
            both_cells - globals_cells,
            namespace - property_cells + increment + number_dir_delta,
            namespace + both_dir_delta,
        );
        assert_eq!(
            number_cells - object_cells,
            namespace - property_cells + increment + number_dir_delta,
            "Number-only cells include the namespace, parse support and directory-page delta"
        );
        assert_eq!(
            both_cells - globals_cells,
            namespace + both_dir_delta,
            "bare globals already supply the functions; Number adds the namespace and directory-page delta"
        );
    }

    #[test]
    fn json_manifest_request_counts_object_both_methods_and_keys() {
        assert_eq!(json_request_cells(), 22 + 2 * 20 + (6 + 5) + (6 + 9));
        assert_eq!(JSON_REQUEST_NODES, 1 + 2 + 2);
    }

    #[test]
    fn string_replace_all_row_budget_and_gating() {
        // `String.prototype.replaceAll` is a gated nineteenth String-prototype
        // row: 7 row cells, a 20-cell one-row function and a new ten-unit key
        // String (6 + 10). It adds no permanent root.
        let base = bootstrap_request_cells_for(0, 11, false, false, false, false, false, false);
        let selected = bootstrap_request_cells_for(0, 11, false, false, false, false, true, false);
        let nodes = bootstrap_node_count(false, false, false, false, true)
            - bootstrap_node_count(false, false, false, false, false);
        assert_eq!(selected - base, 7 + 20 + 6 + 10);
        assert_eq!(nodes, 2);

        // Emission witness: a source that can spell `replaceAll` pays exactly
        // those 43 cells and two nodes; one that cannot keeps the previous
        // manifest byte for byte.
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = vm::SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let measure = |source: &'static str| {
            vm::SourcePlugin::parse_source(&crate::JAVASCRIPT_ES5_PLUGIN, source, &context)
                .expect("measured realm");
            super::intrinsic_budget()
        };
        let none = measure("var o={};");
        let row = measure("var o={};var s=o.replaceAll;");
        assert_eq!(row.0 - none.0, 7 + 20 + 6 + 10);
        assert_eq!(row.1 - none.1, 2);
        eprintln!(
            "STRING_REPLACE_ALL_BUDGET bootstrap={}->{} row_cells=7 function_cells=20 string_cells=16 nodes_delta={nodes} roots_delta=0",
            none.0, row.0
        );
    }

    #[test]
    fn string_search_row_budget_and_gating() {
        // `String.prototype.search` is a gated String-prototype row in the one
        // admitted `/a/` RegExp domain: 7 row cells, a 20-cell one-row
        // function node and a new six-unit key String (6 + 6). It adds no
        // permanent root, and it reuses the realm's existing `/a/` matcher
        // kernel pair, so no helper index moves.
        let base = bootstrap_request_cells_for(0, 11, false, false, false, false, false, false);
        let selected = bootstrap_request_cells_for(0, 11, false, false, false, false, false, true);
        let nodes = bootstrap_node_count_for(false, false, false, false, false, true, false, false)
            - bootstrap_node_count_for(false, false, false, false, false, false, false, false);
        assert_eq!(selected - base, 7 + 20 + 6 + 6);
        assert_eq!(selected - base, 39);
        assert_eq!(nodes, 2);
        // Both gated String rows coexist: `replaceAll`'s ten-unit key plus
        // `search`'s six-unit key and each one-row function node are additive
        // over the same prototype, and the extra row capacity is one more
        // seven-cell row.
        let replace_all = bootstrap_request_cells_for(0, 11, false, false, false, false, true, false);
        let both = bootstrap_request_cells_for(0, 11, false, false, false, false, true, true);
        assert_eq!(both - replace_all, 39);
        assert_eq!(
            bootstrap_node_count_for(false, false, false, false, false, true, false, false)
                - bootstrap_node_count_for(false, false, false, false, false, false, false, false),
            2
        );
        // Emission witness: a source that can spell `search` pays exactly
        // those 39 cells and two nodes; one that cannot keeps the previous
        // manifest byte for byte.
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = vm::SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let measure = |source: &'static str| {
            vm::SourcePlugin::parse_source(&crate::JAVASCRIPT_ES5_PLUGIN, source, &context)
                .expect("measured realm");
            super::intrinsic_budget()
        };
        let none = measure("var o={};");
        let row = measure("var o={};var s=o.search;");
        assert_eq!(row.0 - none.0, 39);
        assert_eq!(row.1 - none.1, 2);
        // Three further spellings reach the same gate without a member read:
        // a `new` construction argument, the computed key of an assignment
        // target, and an `Update` inside a function body. Each still pays the
        // one seven-cell row, the one-row function node and the six-unit key
        // String (7 + 20 + 12 = 39) and its two nodes.
        let new_none = measure("var b=new ArrayBuffer(1);var r=/a/;");
        let new_arg = measure("var b=new ArrayBuffer(\"ba\".search(/a/));");
        let assign_none = measure("var o={};");
        let assign_key = measure("var o={};o[\"search\"]=0;");
        let update_none = measure("function f(o){o.x++;}var o={x:0};");
        let update_body = measure("function f(o){o.search++;}var o={x:0};");
        for (name, base, probe) in [
            ("new", new_none, new_arg),
            ("assign", assign_none, assign_key),
            ("update", update_none, update_body),
        ] {
            eprintln!(
                "STRING_SEARCH_POSITION {name} cells={}->{} delta={} nodes={}->{} delta={}",
                base.0,
                probe.0,
                probe.0 - base.0,
                base.1,
                probe.1,
                probe.1 - base.1
            );
            assert_eq!(probe.0 - base.0, 39, "{name} cells");
            assert_eq!(probe.1 - base.1, 2, "{name} nodes");
        }
        eprintln!(
            "STRING_SEARCH_BUDGET bootstrap={}->{} row_cells=7 function_cells=20 string_cells=12 nodes_delta={nodes} roots_delta=0",
            none.0, row.0
        );
    }

    #[test]
    fn string_match_row_budget_and_gating() {
        // `String.prototype.match` is the `search` sibling: one gated
        // String-prototype row in the one admitted `/a/` RegExp domain. It
        // costs 7 row cells, a 20-cell one-row function node and a new
        // five-unit key String (6 + 5). It adds no permanent root, and it
        // reuses the realm's existing `/a/` matcher kernel pair, so no helper
        // index moves.
        let base = bootstrap_request_cells_for(0, 11, false, false, false, false, false, false);
        let selected = bootstrap_request_cells_gated(
            0, 11, false, false, false, false, false, false, true,
        );
        let nodes = bootstrap_node_count_gated(
            false, false, false, false, false, false, true, false, false, false,
        ) - bootstrap_node_count(false, false, false, false, false);
        assert_eq!(selected - base, 7 + 20 + 6 + 5);
        assert_eq!(selected - base, 38);
        assert_eq!(nodes, 2);
        // The three gated String rows coexist: `replaceAll`'s ten-unit key,
        // `search`'s six-unit key and `match`'s five-unit key, each with its
        // one-row function node over the same prototype, and one more
        // seven-cell row per selection.
        let replace_all = bootstrap_request_cells_for(0, 11, false, false, false, false, true, false);
        let replace_all_search = bootstrap_request_cells_gated(
            0, 11, false, false, false, false, true, true, false,
        );
        let all = bootstrap_request_cells_gated(
            0, 11, false, false, false, false, true, true, true,
        );
        assert_eq!(all - replace_all_search, 38);
        assert_eq!(replace_all_search - replace_all, 39);
        // Emission witness: a source that can spell `match` pays exactly those
        // 38 cells and two nodes; one that cannot keeps the previous manifest
        // byte for byte.
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = vm::SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let measure = |source: &'static str| {
            vm::SourcePlugin::parse_source(&crate::JAVASCRIPT_ES5_PLUGIN, source, &context)
                .expect("measured realm");
            super::intrinsic_budget()
        };
        let none = measure("var o={};");
        let row = measure("var o={};var s=o.match;");
        assert_eq!(row.0 - none.0, 38);
        assert_eq!(row.1 - none.1, 2);
        // Three further spellings reach the same gate without a member read:
        // a `new` construction argument, the computed key of an assignment
        // target, and an `Update` inside a function body. Each still pays the
        // one seven-cell row, the one-row function node and the five-unit key
        // String (7 + 20 + 11 = 38) and its two nodes.
        let new_none = measure("var b=new ArrayBuffer(1);var r=/a/;");
        let new_arg = measure("var b=new ArrayBuffer(\"ba\".match(/a/));");
        let assign_none = measure("var o={};");
        let assign_key = measure("var o={};o[\"match\"]=0;");
        let update_none = measure("function f(o){o.x++;}var o={x:0};");
        let update_body = measure("function f(o){o.match++;}var o={x:0};");
        for (name, base, probe) in [
            ("new", new_none, new_arg),
            ("assign", assign_none, assign_key),
            ("update", update_none, update_body),
        ] {
            eprintln!(
                "STRING_MATCH_POSITION {name} cells={}->{} delta={} nodes={}->{} delta={}",
                base.0,
                probe.0,
                probe.0 - base.0,
                base.1,
                probe.1,
                probe.1 - base.1
            );
            assert_eq!(probe.0 - base.0, 38, "{name} cells");
            assert_eq!(probe.1 - base.1, 2, "{name} nodes");
        }
        eprintln!(
            "STRING_MATCH_BUDGET bootstrap={}->{} row_cells=7 function_cells=20 string_cells=11 nodes_delta={nodes} roots_delta=0",
            none.0, row.0
        );
    }
}

#[cfg(test)]
mod function_name_budget_tests {
    use super::*;

    fn measure(source: &'static str) -> (u64, u64) {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = vm::SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        vm::SourcePlugin::parse_source(&crate::JAVASCRIPT_ES5_PLUGIN, source, &context)
            .expect("measured realm");
        super::intrinsic_budget()
    }

    /// `.prototype` is one static member-name gate: one 7-cell get/set
    /// accessor row on `Function.prototype` plus two 20-cell one-row function
    /// nodes. It adds no permanent root entry, no key String and no directory
    /// page, and a source that never spells the name keeps the previous
    /// manifest byte for byte. `new F(...)` needs the same accessor, so it
    /// pays the same 47 cells / 2 nodes: the private construct helper is
    /// declared at runtime and owns no bootstrap node.
    #[test]
    fn function_prototype_accessor_budget_and_gating() {
        assert_eq!(bootstrap_function_prototype_request_cells(false), 0);
        assert_eq!(bootstrap_function_prototype_request_cells(true), 47);
        assert_eq!(bootstrap_function_prototype_nodes(false), 0);
        assert_eq!(bootstrap_function_prototype_nodes(true), 2);
        let none = measure("function f(){};f();");
        let read = measure("function f(){};f();f.prototype;");
        let assign = measure("function f(){};f();f.prototype=7;");
        let construct = measure("function f(){};f();new f();");
        for (name, probe) in [("read", read), ("assign", assign), ("new", construct)] {
            assert_eq!(probe.0 - none.0, 47, "{name} cells");
            assert_eq!(probe.1 - none.1, 2, "{name} nodes");
        }
        // The same gate reached from a function body must charge exactly the
        // one accessor family: a body-local `.prototype` spelling and a
        // body-only `new F()` (formal-held source function) both add no row,
        // root, key String or directory page beyond the 47 cells / 2 nodes.
        let body_none = measure("function f(){};function g(x){return 0;}g(f);");
        let body_read = measure("function f(){};function g(x){return x.prototype;}g(f);");
        let body_new_none = measure("function C(){};function make(F){return 0;}make(C);");
        let body_new = measure("function C(){};function make(F){return new F();}make(C);");
        for (name, base, probe) in [
            ("body_read", body_none, body_read),
            ("body_new", body_new_none, body_new),
        ] {
            assert_eq!(probe.0 - base.0, 47, "{name} cells");
            assert_eq!(probe.1 - base.1, 2, "{name} nodes");
        }
        eprintln!(
            "FUNCTION_PROTOTYPE_BUDGET bootstrap={}->{} row_cells=7 function_cells=40 nodes_delta={} roots_delta=0",
            none.0, read.0, 2
        );
    }

    /// The four builtin prototype `constructor` rows are gated twice: the
    /// `.prototype` accessor must be selected and the carrier that owns the
    /// row must exist. A source that only reads a source function's
    /// `.prototype` keeps the exact 47-cell accessor charge; naming `Number`
    /// or `Array` adds only that prototype's seven-cell row and no node, key
    /// String or permanent root.
    #[test]
    fn builtin_constructor_prototype_row_budget_and_gating() {
        assert_eq!(
            bootstrap_constructor_prototype_row_cells(false, false, true, true, true, true),
            0
        );
        assert_eq!(
            bootstrap_constructor_prototype_row_cells(true, false, false, false, false, false),
            0
        );
        assert_eq!(
            bootstrap_constructor_prototype_row_cells(true, false, true, true, true, true),
            28
        );
        let none = measure("var o={};");
        let source_function = measure("function f(){};f.prototype;");
        assert_eq!(
            source_function.0 - none.0,
            bootstrap_function_prototype_request_cells(true)
        );
        assert_eq!(source_function.0 - none.0, 47);
        let number = measure("Number;");
        let number_prototype = measure("Number.prototype;");
        assert_eq!(number_prototype.0 - number.0, 47 + 7);
        assert_eq!(number_prototype.1 - number.1, 2);
        let array = measure("Array.isArray();");
        let array_prototype = measure("Array.prototype;");
        assert_eq!(array_prototype.0 - array.0, 47 + 7);
        assert_eq!(array_prototype.1 - array.1, 2);
        eprintln!(
            "CONSTRUCTOR_PROTO_BUDGET source_fn_delta={} number_delta={} array_delta={} rows=4x7",
            source_function.0 - none.0,
            number_prototype.0 - number.0,
            array_prototype.0 - array.0
        );
    }

    /// Emission-level witness for the `prototype` family: the cells charged by
    /// `bootstrap_request_cells` must equal the cells the bootstrap IR actually
    /// allocates. The comparison reads the emitter's own `allocate` calls, so
    /// a charge predicate that disagrees with the row reservation (the 7-cell
    /// Array method-table row was the regression) fails here instead of only
    /// showing up as a runtime heap shortfall.
    #[test]
    fn prototype_manifest_request_matches_the_emitted_rows() {
        let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
        let context = vm::SourceFrontendContext {
            source_id: 0,
            original_path: None,
            phase: vm::ParsePhase::Standalone,
            host_api_catalog: Some(&catalog),
            imports: &[],
        };
        let parse = |source: &str| {
            vm::SourcePlugin::parse_source(&crate::JAVASCRIPT_ES5_PLUGIN, source, &context)
                .expect("measured realm")
        };
        // `(kind, rows, code, entry, frame)` of every bootstrap allocation
        // whose capacity is a compile-time constant.
        let allocations = |ir: &FrontendIr| {
            let bootstrap = ir
                .functions
                .iter()
                .find(|function| function.name == BOOTSTRAP_NAME)
                .expect("bootstrap")
                .index;
            let allocate = ir
                .functions
                .iter()
                .find(|function| function.name == ALLOC_NAME)
                .expect("allocator")
                .index;
            let body = &ir.function_impls[&bootstrap].body_stmts;
            body.iter()
                .filter_map(|statement| {
                    let S::Assign {
                        expr: E::Call(id, _, args, _, _),
                        ..
                    } = statement
                    else {
                        return None;
                    };
                    if *id != allocate {
                        return None;
                    }
                    match args.as_slice() {
                        [_, E::Int(kind), _, E::Int(rows), E::Int(code), _, _, E::Var(frame), E::Int(entry)] => {
                            Some((*kind, *rows, *code, *entry, *frame))
                        }
                        _ => None,
                    }
                })
                .collect::<Vec<_>>()
        };
        let actual_delta = |base: &FrontendIr, probe: &FrontendIr| -> u64 {
            let base_allocations = allocations(base);
            let probe_allocations = allocations(probe);
            let permanent = |list: &[(i64, i64, i64, i64, u16)], frame: u16| {
                list.iter()
                    .filter(|allocation| allocation.4 == frame)
                    .map(|allocation| (allocation.3, allocation.0, allocation.1, allocation.2))
                    .collect::<Vec<_>>()
            };
            let base_frame = base_allocations[0].4;
            let probe_frame = probe_allocations[0].4;
            let base_rows = permanent(&base_allocations, base_frame);
            let probe_rows = permanent(&probe_allocations, probe_frame);
            assert_eq!(base_rows.len(), probe_rows.len(), "permanent root count");
            let row_cells = base_rows
                .iter()
                .zip(&probe_rows)
                .map(|(base, probe)| {
                    assert_eq!(
                        (base.0, base.1, base.3),
                        (probe.0, probe.1, probe.3),
                        "existing allocation identity moves"
                    );
                    7 * (probe.2 - base.2)
                })
                .sum::<i64>();
            assert!(row_cells >= 0);
            // The accessor getter and setter publish into the stage frame, so
            // they are the only extra one-row function nodes.
            let node_cells = probe_allocations
                .iter()
                .filter(|allocation| {
                    allocation.2 == FUNCTION_PROTOTYPE_GET_CODE
                        || allocation.2 == FUNCTION_PROTOTYPE_SET_CODE
                })
                .map(|allocation| 3 + 10 + 7 * allocation.1 as u64)
                .sum::<u64>();
            row_cells as u64 + node_cells
        };
        let base = parse("var o={};");
        let source_fn = parse("function f(){};f.prototype;");
        let number = parse("Number;");
        let number_prototype = parse("Number.prototype;");
        let array = parse("Array.isArray();");
        let array_prototype = parse("Array.prototype;");
        let boolean = parse("Boolean;");
        let boolean_prototype = parse("Boolean.prototype;");
        let charged = |probe_source: &'static str, base_source: &'static str| {
            let (probe_cells, probe_nodes) = measure(probe_source);
            let (base_cells, base_nodes) = measure(base_source);
            let pages = |nodes: u64| {
                (nodes - 1) / layout::PAGE_ROWS as u64 * layout::DIR_PAGE_CELLS as u64
            };
            (probe_cells - base_cells) - (pages(probe_nodes) - pages(base_nodes))
        };
        let mut deltas = Vec::new();
        for (name, base_source, probe_source, base_ir, probe_ir) in [
            ("source_fn", "function f(){};f();", "function f(){};f.prototype;", &base, &source_fn),
            ("number", "Number;", "Number.prototype;", &number, &number_prototype),
            ("array", "Array.isArray();", "Array.prototype;", &array, &array_prototype),
            ("boolean", "Boolean;", "Boolean.prototype;", &boolean, &boolean_prototype),
        ] {
            let charged_delta = charged(probe_source, base_source);
            let actual = actual_delta(base_ir, probe_ir);
            deltas.push((name, charged_delta, actual));
            assert_eq!(
                actual, charged_delta,
                "{name}: emitted request must equal the charged manifest"
            );
        }
        eprintln!("PROTOTYPE_ACTUAL_REQUEST {deltas:?}");
    }

    /// `.name` and `.toString` are two static member-name gates. The accessor
    /// row, its private getter and (for `toString`) its data row are charged
    /// exactly, and a source that never spells either name keeps the previous
    /// manifest byte for byte.
    #[test]
    fn function_prototype_rows_budget_and_gating() {
        let none = measure("var o={};");
        let name = measure("var o={};var v=o.name;");
        let tostring = measure("var o={};var v=o.toString;");
        let both = measure("var o={};var v=o.name;var w=o.toString;");
        assert_eq!((none.0, none.1), (3867, 126));
        // One Function.prototype row (7), one 20-cell one-row getter node, and
        // the private empty-String permanent root entry (3 cells) that
        // anonymous names read instead of the public `message` row.
        assert_eq!(
            name.0 - none.0,
            7 + 20 + layout::FRAME_CELLS_PER_ENTRY as u64
        );
        assert_eq!(name.1 - none.1, 1);
        // `toString` implies the shared name storage: the name accessor plus
        // the `toString` row (7) and its 20-cell one-row function node. The
        // same static token also selects the pre-existing
        // `Number.prototype.toString` row + function (30 cells with its root
        // entry), the 65-cell RangeError realm (three nodes), the 3-cell
        // private empty-String root entry, and one new 451-cell directory
        // page. Its feature-specific share is still 54.
        assert_eq!(bootstrap_function_api_request_cells(true, true), 54);
        assert_eq!(
            tostring.0 - none.0,
            54 + layout::FRAME_CELLS_PER_ENTRY as u64
                + 30
                + RANGE_ERROR_EXTRA_CELLS
                + layout::DIR_PAGE_CELLS as u64
        );
        assert_eq!(tostring.1 - none.1, 6);
        // Both names share the one accessor: no second row or getter.
        assert_eq!(both.0, tostring.0);
        assert_eq!(both.1, tostring.1);
        eprintln!(
            "FUNCTION_PROTO_BUDGET none={:?} name={:?} tostring={:?} both={:?} name_delta={} tostring_delta={} tostring_nodes={} feature_cells=54 empty_root_cells={} page_cells={}",
            none,
            name,
            tostring,
            both,
            name.0 - none.0,
            tostring.0 - none.0,
            tostring.1 - none.1,
            layout::FRAME_CELLS_PER_ENTRY,
            layout::DIR_PAGE_CELLS
        );
    }

    /// Per-source-function cost: one extra 7-cell row. A named declaration
    /// also allocates its name String (6 + units); anonymous functions reuse
    /// the realm's single empty String and add no node.
    #[test]
    fn source_function_name_row_budget() {
        // `intrinsic_budget()` freezes before the script body (and therefore
        // before source-function nodes). The final `realm_budget_witness`
        // includes them, so it carries the per-site row and name String.
        let full = |source: &'static str| -> u64 {
            let catalog = crate::runtime::cli::reference_array_cli_effective_catalog();
            let context = vm::SourceFrontendContext {
                source_id: 0,
                original_path: None,
                phase: vm::ParsePhase::Standalone,
                host_api_catalog: Some(&catalog),
                imports: &[],
            };
            vm::SourcePlugin::parse_source(&crate::JAVASCRIPT_ES5_PLUGIN, source, &context)
                .expect("measured realm");
            super::super::super::lowerer::realm_budget_witness().0
        };
        let unnamed = full("function f(){};");
        let named = full("function f(){};var x=f.name;");
        let anonymous_off = full("var f=function(){};");
        let anonymous_gate = full("var f=function(){};f.name;");
        eprintln!(
            "SOURCE_FUNCTION_NAME_BUDGET decl_off={unnamed} decl_on={named} anon_off={anonymous_off} anon_on={anonymous_gate} decl_delta={} anon_delta={}",
            named as i64 - unnamed as i64,
            anonymous_gate as i64 - anonymous_off as i64
        );
        // Ten named declarations: the structural cost is 10 rows (70) and
        // ten one-unit name Strings (70), plus the 27-cell realm gate. Page
        // boundaries add one 451-cell directory page whenever the extra
        // nodes start a new page.
        let ten_off = full("function a(){}function b(){}function c(){}function d(){}function e(){}function j(){}function g(){}function h(){}function i(){}function k(){}");
        let ten_on = full("function a(){}function b(){}function c(){}function d(){}function e(){}function j(){}function g(){}function h(){}function i(){}function k(){}a.name;");
        eprintln!(
            "SOURCE_FUNCTION_NAME_TEN off={ten_off} on={ten_on} delta={} structural=27+10*7+10*(6+1)=167 page={}",
            ten_on as i64 - ten_off as i64,
            layout::DIR_PAGE_CELLS
        );
    }

    /// The default manifest has 55 function nodes plus the TypeError node.
    /// Every source-function site pays one extra row once a name gate is on.
    #[test]
    fn realm_function_counts() {
        let default_functions = bootstrap_node_count(false, false, false, false, false);
        let selected = bootstrap_node_count_for(false, false, false, false, false, false, true, true);
        assert_eq!(default_functions, 124);
        assert_eq!(selected - default_functions, 2);
        eprintln!(
            "REALM_FUNCTION_COUNT default_realm_nodes={default_functions} bootstrap_function_nodes=55 plus TypeError=1 source_sites=per_source; gated_extra_nodes={}",
            selected - default_functions
        );
    }
}
