# pd-vm Compatibility Frontends

[![pd-vm-compat-frontends on crates.io](https://img.shields.io/crates/v/pd-vm-compat-frontends.svg)](https://crates.io/crates/pd-vm-compat-frontends)

TypeScript and Lua compatibility frontends for `pd-vm`. RustScript (`.rss`) remains native to the core crate.

## Supported source formats

- `.ts` and `.mts`: a statically checked, VM-backed TypeScript subset.
- `.lua`: the existing Lua compatibility frontend.
- `.js`, `.mjs`, and `.cjs`: rejected as both entry files and imported dependencies. There is no JavaScript fallback.

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

## Dependency and release status

The development manifest pins `pd-vm` to the accepted Git revision that provides the generic typed frontend API. A published registry `pd-vm` version compatible with that API has not yet been established. The release workflow requires an explicit registry version, validates publication and locked resolution, then runs the package build, tests, and dry run before publishing only this compat crate; it does not publish Core crates.

## Development

```bash
cargo fmt --all -- --check
cargo check --locked --workspace
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace -- --test-threads=1
```
