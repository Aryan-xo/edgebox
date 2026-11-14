use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};
use edgebox_runtime::Runtime;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a WASI command component (.wasm)
    Run {
        /// Path to the component
        wasm: PathBuf,
        /// Arguments passed to the guest
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

fn main() -> Result<ExitCode> {
    match Cli::parse().command {
        Command::Run { wasm, args } => {
            let code = Runtime::new()?.run(&wasm, &args)?;
            Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
        }
    }
}
