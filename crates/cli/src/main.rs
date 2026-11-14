use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use edgebox_runtime::{Limits, Runtime};
use edgebox_server::store::Store;
use edgebox_server::{App, router, validate_name};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a WASI command component (.wasm) locally
    Run {
        /// Path to the component
        wasm: PathBuf,
        /// Arguments passed to the guest
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Start the server: `/<name>/...` is handled by the function `<name>`
    Serve {
        /// Functions to deploy at startup, as NAME=PATH (e.g. hello=./hello.wasm)
        #[arg(value_parser = parse_function)]
        functions: Vec<(String, PathBuf)>,
        /// Address to listen on
        #[arg(long, env = "EDGEBOX_ADDR", default_value = "127.0.0.1:8080")]
        addr: SocketAddr,
        /// SQLite database for deployed functions (`:memory:` for none)
        #[arg(long, env = "EDGEBOX_DATA", default_value = "edgebox.db")]
        data: PathBuf,
        /// Require this bearer token for the admin API
        #[arg(long, env = "EDGEBOX_TOKEN", hide_env_values = true)]
        token: Option<String>,
        #[command(flatten)]
        limits: LimitArgs,
    },
    /// Deploy a function to a running server
    Deploy {
        /// Function name (its URL is /<name>)
        name: String,
        /// Path to the component
        wasm: PathBuf,
        #[command(flatten)]
        remote: Remote,
    },
    /// List functions on a running server
    List {
        #[command(flatten)]
        remote: Remote,
    },
}

#[derive(clap::Args)]
struct LimitArgs {
    /// Max time per request, in milliseconds
    #[arg(long, env = "EDGEBOX_TIMEOUT_MS", default_value_t = 10_000)]
    timeout_ms: u64,
    /// Max memory per instance, in MiB
    #[arg(long, env = "EDGEBOX_MAX_MEMORY_MB", default_value_t = 128)]
    max_memory_mb: usize,
}

impl From<LimitArgs> for Limits {
    fn from(args: LimitArgs) -> Self {
        Limits {
            timeout: Duration::from_millis(args.timeout_ms),
            max_memory_bytes: args.max_memory_mb * 1024 * 1024,
        }
    }
}

#[derive(clap::Args)]
struct Remote {
    /// Server URL
    #[arg(long, env = "EDGEBOX_SERVER", default_value = "http://127.0.0.1:8080")]
    server: String,
    /// Admin API token
    #[arg(long, env = "EDGEBOX_TOKEN", hide_env_values = true)]
    token: Option<String>,
}

fn main() -> Result<ExitCode> {
    match Cli::parse().command {
        Command::Run { wasm, args } => {
            let code = Runtime::new()?.run(&wasm, &args)?;
            return Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)));
        }
        Command::Serve {
            functions,
            addr,
            data,
            token,
            limits,
        } => serve(functions, addr, data, token, limits.into())?,
        Command::Deploy { name, wasm, remote } => deploy(&name, &wasm, &remote)?,
        Command::List { remote } => list(&remote)?,
    }
    Ok(ExitCode::SUCCESS)
}

fn serve(
    functions: Vec<(String, PathBuf)>,
    addr: SocketAddr,
    data: PathBuf,
    token: Option<String>,
    limits: Limits,
) -> Result<()> {
    let store = if data.as_os_str() == ":memory:" {
        Store::in_memory()?
    } else {
        Store::open(&data)?
    };
    if token.is_none() && !addr.ip().is_loopback() {
        eprintln!("warning: admin API is open to the network; set --token or EDGEBOX_TOKEN");
    }

    let app = App::new(Runtime::with_limits(limits)?, store, token)?;
    for (name, path) in functions {
        let wasm =
            std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
        let info = app
            .deploy(&name, &wasm)
            .with_context(|| format!("failed to deploy {}", path.display()))?;
        println!("deployed {name} v{} from {}", info.version, path.display());
    }
    for name in app.function_names() {
        println!("  /{name}");
    }

    tokio::runtime::Runtime::new()?.block_on(async {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        println!("edgebox listening on http://{}", listener.local_addr()?);
        axum::serve(listener, router(app))
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        Ok(())
    })
}

fn deploy(name: &str, wasm: &PathBuf, remote: &Remote) -> Result<()> {
    validate_name(name).map_err(anyhow::Error::msg)?;
    let bytes =
        std::fs::read(wasm).with_context(|| format!("failed to read {}", wasm.display()))?;
    let url = format!(
        "{}/_api/functions/{name}",
        remote.server.trim_end_matches('/')
    );
    let info = remote.call(ureq_agent().put(&url), Some(&bytes))?;
    println!(
        "deployed {} v{} ({} bytes) → {}/{name}",
        info["name"].as_str().unwrap_or(name),
        info["version"],
        info["size"],
        remote.server.trim_end_matches('/'),
    );
    Ok(())
}

fn list(remote: &Remote) -> Result<()> {
    let url = format!("{}/_api/functions", remote.server.trim_end_matches('/'));
    let functions = remote.call(ureq_agent().get(&url).force_send_body(), None)?;
    let functions = functions.as_array().cloned().unwrap_or_default();
    if functions.is_empty() {
        println!("no functions deployed");
    }
    for f in functions {
        println!(
            "{:<24} v{:<4} {:>9} bytes",
            f["name"].as_str().unwrap_or("?"),
            f["version"],
            f["size"]
        );
    }
    Ok(())
}

fn ureq_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into()
}

impl Remote {
    /// Sends an admin request and returns the JSON response, or the server's error.
    fn call(
        &self,
        mut req: ureq::RequestBuilder<ureq::typestate::WithBody>,
        body: Option<&[u8]>,
    ) -> Result<serde_json::Value> {
        if let Some(token) = &self.token {
            req = req.header("authorization", format!("Bearer {token}"));
        }
        let mut res = req
            .send(body.unwrap_or_default())
            .with_context(|| format!("failed to reach {}", self.server))?;
        let status = res.status();
        let text = res.body_mut().read_to_string()?;
        let json: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        if !status.is_success() {
            let message = json["error"].as_str().unwrap_or(&text);
            bail!("server returned {status}: {message}");
        }
        Ok(json)
    }
}

/// Parses `NAME=PATH`.
fn parse_function(arg: &str) -> Result<(String, PathBuf), String> {
    let (name, path) = arg
        .split_once('=')
        .ok_or_else(|| format!("expected NAME=PATH, got {arg:?}"))?;
    validate_name(name)?;
    Ok((name.to_string(), PathBuf::from(path)))
}
