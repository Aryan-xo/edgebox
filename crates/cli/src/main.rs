use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use edgebox_runtime::Runtime;
use edgebox_server::{Functions, router};

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
    /// Serve HTTP functions: `/<name>/...` is handled by the function `<name>`
    Serve {
        /// Functions to serve, as NAME=PATH (e.g. hello=./hello.wasm)
        #[arg(required = true, value_parser = parse_function)]
        functions: Vec<(String, PathBuf)>,
        /// Address to listen on
        #[arg(long, default_value = "127.0.0.1:8080")]
        addr: SocketAddr,
    },
}

fn main() -> Result<ExitCode> {
    match Cli::parse().command {
        Command::Run { wasm, args } => {
            let code = Runtime::new()?.run(&wasm, &args)?;
            Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
        }
        Command::Serve { functions, addr } => {
            serve(functions, addr)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn serve(functions: Vec<(String, PathBuf)>, addr: SocketAddr) -> Result<()> {
    let runtime = Runtime::new()?;
    let mut loaded = Functions::new();
    for (name, path) in functions {
        let function = runtime.load_http(&path)?;
        if loaded.insert(name.clone(), function).is_some() {
            bail!("function {name:?} given more than once");
        }
        println!("  /{name} → {}", path.display());
    }

    tokio::runtime::Runtime::new()?.block_on(async {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        println!("edgebox listening on http://{}", listener.local_addr()?);
        axum::serve(listener, router(loaded))
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        Ok(())
    })
}

/// Parses `NAME=PATH`. Names become URL path segments, so they can't contain `/`.
fn parse_function(arg: &str) -> Result<(String, PathBuf), String> {
    let (name, path) = arg
        .split_once('=')
        .ok_or_else(|| format!("expected NAME=PATH, got {arg:?}"))?;
    if name.is_empty() || name.contains('/') {
        return Err(format!("invalid function name {name:?}"));
    }
    Ok((name.to_string(), PathBuf::from(path)))
}
