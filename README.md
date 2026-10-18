# pd-vm Compatibility Frontends

[![pd-vm-compat-frontends on crates.io](https://img.shields.io/crates/v/pd-vm-compat-frontends.svg)](https://crates.io/crates/pd-vm-compat-frontends)

TypeScript and Lua compatibility frontends for `pd-vm`. RustScript (`.rss`) remains native to the core crate.

## Supported source formats

- `.ts` and `.mts`: a statically checked, VM-backed TypeScript subset.
- `.lua`: the existing Lua compatibility frontend.
- Default mode rejects `.js`, `.mjs`, and `.cjs` as both entry files and imported dependencies. There is no JavaScript fallback; `.js` has a separate explicit profile below.

The tested subset includes scalar values, simple null unions, arrays, required and optional record/interface fields, type aliases, callable values, simple generics, contextual callback parameters, and TypeScript-to-TypeScript / TypeScript-to-RustScript imports. An omitted optional field is accepted; reading it produces the VM `Null` value, matching an explicit `null`. Missing required fields and values of the wrong present type remain compile-time errors. Host functions and nominal host types must come from the caller's `HostApiCatalog`; `declare function` only checks an existing catalog entry. Host overloads, numeric ABI narrowing, and resource passing/ownership are checked before execution.

This is not a full TypeScript or JavaScript runtime. JSX, classes, enums, decorators, async/Promise, dynamic imports, CommonJS, `any`, `undefined`, unsupported assertions, and unsupported AST/type forms are rejected with source diagnostics. Optional-field reads compose with ordinary projections on the same record; a missing optional key evaluates to the VM `Null` value. An SWC parse success alone does not make a construct supported.

## Numeric standard-library subset

`Math.floor`, `Math.sqrt`, and `Math.round` accept one numeric argument. `Math.round` uses JavaScript's nearest-integer rule (half ties toward positive infinity), preserves negative zero, and evaluates its argument once. `Number.isInteger` is numeric-only and evaluates its argument once. `Number.isNaN` supports numeric values plus known scalar strings, booleans, and `null`; the latter three return `false` after evaluating the argument once. Non-scalar or unknown `Number.isNaN` inputs receive a source diagnostic. Global `isNaN` is intentionally limited to statically numeric inputs; this subset does not implement JavaScript's general `ToNumber` coercion.

## Explicit native numeric-array resources

`pd_vm_compat_frontends::runtime::array::typescript_array_host_module()` is an optional runtime-owned typed host module. It provides explicit `Resource<"compat.number_array">` create/read/write/length/close operations when the caller installs its catalog and registry. The TypeScript frontend does not silently grant this module, and ordinary `number[]` values are not automatically lowered to its resource representation.

A native array resource remains in the VM resource table until explicit `close` or VM teardown/reset. Dropping the final guest callable alias does not currently notify the host resource table. Callers using this adapter must close resources explicitly; transparent `number[]` resource lowering remains disabled until Core can safely reclaim a resource when its last alias disappears, including aliases retained by closures/callbacks and cleanup after abort/reset, without introducing strong-root cycles.

## Rust API

```rust
use vm::compile_source_file_with_options;

let options = pd_vm_compat_frontends::compile_options();
let compiled = compile_source_file_with_options("examples/example.ts", options)?;
```

For host-backed programs, supply the same typed `HostApiCatalog` to compile and analysis operations:

```rust
use std::sync::Arc;

let options = pd_vm_compat_frontends::compile_options_with_catalog(Arc::new(catalog));
let compiled = compile_source_file_with_options("examples/example.ts", options)?;
```

`collect_typescript_type_hints` and `collect_typescript_type_hints_with_options` use the checked TypeScript pipeline and expose structured schemas and declaration spans, including callable signatures. Use the corresponding `pd-vm` analysis APIs with `typescript_flavor()` for semantic definitions and signatures.

## CLI

```bash
cargo run --locked --bin pd-vm-compat-run -- examples/example.ts
cargo run --locked --bin pd-vm-compat-run -- examples/example.mts
cargo run --locked --bin pd-vm-compat-run -- examples/example.lua
cargo run --locked --bin pd-vm-compat-run -- --version
```

The CLI registers only TypeScript and Lua compatibility plugins. Core RustScript continues to handle `.rss` through `pd-vm`.

### Explicit reference-array mode

A leading Compat-only selector admits the accepted general typed
reference-array/generator mode for one invocation:

```bash
cargo run --locked --bin pd-vm-compat-run -- --typescript-reference-arrays entry.ts
```

The selector must come first: only that exact first token is stripped, and
every remaining argument reaches Core's existing CLI unchanged with the
selected-mode compile options and a fresh per-VM production host-services
factory (same effective catalog for compiler and registry, per-VM owned
array initialization). A selector-looking token anywhere else stays
Core-owned (file argument or Core's own `unknown flag` refusal). Default
invocations, `--help`/`--version`, output values, errors and artifact
authority behavior are unchanged. This documents the Compat-side selection
only; it adds no new Core flags.

## Explicit ES5.1 static-script profile

The Rust API's `JAVASCRIPT_ES5_PLUGIN` and `javascript_es5_flavor()` expose
an isolated, bounded JavaScript profile. Heap-backed programs require the
explicit reference-array catalog and per-VM runtime initialization. Default
`compile_options` and CLI invocations continue to select TypeScript/Lua;
there is no JavaScript CLI selector, and `.mjs`/`.cjs` remain rejected.

The profile supports ordinary closed strict source declarations and
anonymous function expressions with unique plain identifier parameters.
Strictness comes from an exact `"use strict"` directive or inheritance.
Parameters have activation-local bindings: bare `var` preserves the passed
value, function declarations install before statements, and explicit
initializers execute in source order. Missing actuals are `undefined`;
all actuals, including extras, evaluate once from left to right after the
callee and method receiver. Managed values retain identity through calls.
Function `length` and `["length"]` read the source formal count, independent
of actual count and the private physical ABI.

```javascript
function first(a, b) { "use strict"; var a; return a; }
var alias = first;
var result = alias(7, 9, 11); // 7; alias.length is 2
var missing = first();       // undefined
```

P5-A adds runtime truthiness for `!`, `&&` and `||` over the currently
representable primitives, strings, arrays, plain objects and functions.
Logical operators evaluate the left operand once, skip an unselected right
operand, and return the original selected boxed value. Scalars remain
heap-free when their source needs no realm. Conditional assignments merge
kind and exact array-length facts conservatively; later numeric, callable
or indexed operations may therefore receive a profile refusal even when
one branch is a constant. String truthiness reads checked UTF-16 length,
without coercion or user callbacks. This slice does not add `?:`, coercive
arithmetic/equality, general statement conditions, or JavaScript exceptions.

Each function and call is limited to 64 formals and 64 actuals respectively.
Existing bounds remain 256 source functions, 65536 initialization cells,
and 8192 active roots. The typed Core argument vector is separately bounded
by its actual count; it is not a numeric guest-array allocation, and its
construction carries no universal no-copy or linear-cost guarantee.
Unknown parameter kinds do not imply numeric/object/callable facts:
unsupported coercion, parameter-member and parameter-callee operations
receive profile diagnostics. Closures/captures, runtime EnvironmentRecords,
`arguments`, sloppy calls, named function expressions, constructors,
prototypes/descriptors, `call`/`apply`/`bind`, general control flow and JS
exception handling remain outside this intermediate profile. Arbitrary
function properties are not synthesized. This is not a complete ES5.1
engine or a complete function/exception implementation.

## Dependency and release status

The development manifest pins `pd-vm` to the accepted Git revision that provides the generic typed frontend API. A published registry `pd-vm` version compatible with that API has not yet been established. The release workflow requires an explicit registry version, validates publication and locked resolution, then runs the package build, tests, and dry run before publishing only this compat crate; it does not publish Core crates.

## Development

```bash
cargo fmt --all -- --check
cargo check --locked --workspace
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace -- --test-threads=1
```
