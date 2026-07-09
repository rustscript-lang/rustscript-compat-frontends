# pd-vm Compatibility Frontends

JavaScript and Lua compatibility frontends for `pd-vm`.

This crate owns the compatibility-language pieces that are intentionally outside the core `rustscript` repository:

- JavaScript parser dialect configuration and lowering rewrites
- Lua parser/lowering helpers
- JavaScript and Lua import scanning / import stripping for source-file loading
- compatibility frontend tests and fixtures

## Usage

```toml
pd-vm = { git = "https://github.com/rustscript-lang/rustscript", package = "pd-vm" }
pd-vm-compat-frontends = { git = "https://github.com/rustscript-lang/rustscript-compat-frontends" }
```

```rust
use pd_vm::{compile_source_file_with_options, CompileSourceFileOptions};

let options = CompileSourceFileOptions::new()
    .with_source_plugin(pd_vm_compat_frontends::plugin());
let compiled = compile_source_file_with_options("examples/example.js", options)?;
```

For convenience, this crate also exposes:

```rust
let compiled = pd_vm_compat_frontends::compile_source_file("examples/example.lua")?;
```

## Supported extensions

- `.js` -> JavaScript compatibility frontend
- `.lua` -> Lua compatibility frontend

Core RustScript (`.rss`) remains in the `pd-vm` crate.

## Development

```bash
cargo test --workspace --jobs 4
cargo fmt --all -- --check
```
