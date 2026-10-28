//! `kb-server` binary: one process, several jobs.
//!
//! With no arguments it serves HTTP and runs the ingest worker in the same
//! process, which is what `src/server.ts` does. The subcommands are the
//! operational scripts (`create-api-key`, `apply-schema`, `migrate`) plus
//! `check-boundary`; `kb-server --help` lists them.

use kb_server::{dispatch, init_tracing};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    init_tracing();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match dispatch(&args).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            // Usage mistakes and operation failures share the exit code, as
            // they do in the TypeScript scripts: an operator reads stderr.
            eprintln!("{err}");
            std::process::ExitCode::FAILURE
        }
    }
}
