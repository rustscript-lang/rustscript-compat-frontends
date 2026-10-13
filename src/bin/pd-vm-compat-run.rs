fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = vm::cli::CliRuntime {
        binary_name: "pd-vm-compat-run",
        default_source: "examples/example.ts",
        compile_options: pd_vm_compat_frontends::compile_options,
    };
    vm::cli::main_with_services(
        runtime,
        pd_vm_compat_frontends::runtime::cli::typescript_cli_host_services(),
    )
}
