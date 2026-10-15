/// Leading Compat-only selector for the accepted explicit
/// reference-array/generator mode. It must come first: only this exact first
/// token is stripped, and every remaining argument reaches Core's existing
/// CLI parser unchanged. A selector-looking token anywhere else stays
/// Core-owned (file argument or Core's own `unknown flag` refusal).
const TYPESCRIPT_REFERENCE_ARRAYS: &str = "--typescript-reference-arrays";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args
        .first()
        .is_some_and(|first| first == TYPESCRIPT_REFERENCE_ARRAYS)
    {
        // Selected route: remove only the leading selector, pass the rest
        // through untouched, and run the exact per-VM production
        // reference-array services. Exit/error behavior stays Core's.
        args.remove(0);
        let runtime = vm::cli::CliRuntime {
            binary_name: "pd-vm-compat-run",
            default_source: "examples/example.ts",
            compile_options:
                pd_vm_compat_frontends::runtime::cli::reference_array_cli_compile_options,
        };
        if let Err(error) = vm::cli::run_with_args(
            &runtime,
            &args,
            &pd_vm_compat_frontends::runtime::cli::typescript_reference_array_cli_host_services(),
        ) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // Default route, unchanged.
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
