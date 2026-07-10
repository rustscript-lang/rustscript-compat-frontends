# pd-vm Compatibility Frontends

[![pd-vm-compat-frontends on crates.io](https://img.shields.io/crates/v/pd-vm-compat-frontends.svg)](https://crates.io/crates/pd-vm-compat-frontends)

JavaScript and Lua compatibility frontends for `pd-vm`.

This crate owns the compatibility-language pieces that are intentionally outside the core `rustscript` repository:

- JavaScript parser dialect configuration and lowering rewrites
- Lua parser/lowering helpers
- JavaScript and Lua import scanning / import stripping for source-file loading
- compatibility frontend tests and fixtures

## Usage

```toml
pd-vm = "0.22.2"
pd-vm-compat-frontends = "0.1.0"
```

```rust
use vm::compile_source_file_with_options;

let options = pd_vm_compat_frontends::compile_options();
let compiled = compile_source_file_with_options("examples/example.js", options)?;
```

The crate also ships a CLI binary with the same entry surface as `pd-vm-run`, but with the JavaScript and Lua source plugins pre-registered:

```bash
cargo run --bin pd-vm-compat-run -- examples/example.js
cargo run --bin pd-vm-compat-run -- examples/example.lua
pd-vm-compat-run fmt --check examples/example.js
pd-vm-compat-run --emit-vmbc out.vmbc examples/example.lua
```

## Supported extensions

- `.js` -> JavaScript compatibility frontend
- `.lua` -> Lua compatibility frontend

Core RustScript (`.rss`) remains in the `pd-vm` crate.

## Development

```bash
cargo test --workspace --jobs 4
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```
