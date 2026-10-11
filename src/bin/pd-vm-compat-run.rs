fn main() -> Result<(), Box<dyn std::error::Error>> {
    vm::cli::main(vm::cli::CliRuntime {
        binary_name: "pd-vm-compat-run",
        default_source: "examples/example.ts",
        compile_options: pd_vm_compat_frontends::compile_options,
    })
}
