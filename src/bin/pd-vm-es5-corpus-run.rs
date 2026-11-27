//! Dedicated ES5 corpus observer.
//!
//! Exact argv is `--javascript-es5 <source.js>`. This is not the ordinary
//! Core CLI: other flags, REPL, native, AOT, and artifact requests are refused
//! before compilation, and success prints only the observed cookie.
use std::path::Path;
use std::process::ExitCode;

use pd_vm_compat_frontends::javascript::corpus::{
    CorpusFileError, observe_javascript_es5_corpus_file,
};

fn main() -> ExitCode {
    match observe_argv() {
        Ok(cookie) => {
            println!("Int({cookie})");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}

fn observe_argv() -> Result<i64, RunError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [selector, source] = args.as_slice() else {
        return Err(RunError::Usage);
    };
    if selector != "--javascript-es5" || rejected_source_request(source) {
        return Err(RunError::Usage);
    }
    observe_javascript_es5_corpus_file(Path::new(source))
        .map(|observed| observed.cookie)
        .map_err(RunError::Observe)
}

fn rejected_source_request(source: &str) -> bool {
    source == "repl" || source == "--repl" || source.starts_with('-')
}

enum RunError {
    Usage,
    Observe(CorpusFileError),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage => write!(
                formatter,
                "JavaScript ES5 corpus runner accepts only --javascript-es5 <source.js>"
            ),
            Self::Observe(error) => write!(formatter, "{error}"),
        }
    }
}
