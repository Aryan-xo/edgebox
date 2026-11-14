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
        /// Log output format (filter with RUST_LOG, default `info`)
        #[arg(long, env = "EDGEBOX_LOG_FORMAT", value_enum, default_value = "text")]
        log_format: LogFormat,
    },
    /// Deploy a function to a running server
    Deploy {
        /// Function name (its URL is /<name>)
        name: String,
        /// Path to the component
        wasm: PathBuf,
        /// Environment variable (e.g. a secret) for the function, as KEY=VALUE. Repeatable.
        #[arg(long = "env", value_name = "KEY=VALUE", value_parser = parse_env)]
        env: Vec<(String, serde_json::Value)>,
        /// Host the function may call over HTTP (`*.example.com` for subdomains). Repeatable.
        #[arg(long = "allow-host", value_name = "HOST")]
        allowed_hosts: Vec<String>,
        #[command(flatten)]
        remote: Remote,
    },
    /// List functions on a running server
    List {
        #[command(flatten)]
        remote: Remote,
    },
    /// Show a function's recent stdout/stderr
    Logs {
        /// Function name
        name: String,
        #[command(flatten)]
        remote: Remote,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum LogFormat {
    Text,
    Json,
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
            log_format,
        } => {
            init_logging(log_format);
            serve(functions, addr, data, token, limits.into())?
        }
        Command::Deploy {
            name,
            wasm,
            env,
            allowed_hosts,
            remote,
        } => deploy(&name, &wasm, env, allowed_hosts, &remote)?,
        Command::List { remote } => list(&remote)?,
        Command::Logs { name, remote } => logs(&name, &remote)?,
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
        tracing::warn!("admin API is open to the network; set --token or EDGEBOX_TOKEN");
    }

    let app = App::new(Runtime::with_limits(limits)?, store, token)?;
    for (name, path) in functions {
        let wasm =
            std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
        app.deploy(&name, &wasm)
            .with_context(|| format!("failed to deploy {}", path.display()))?;
    }
    tracing::info!(functions = ?app.function_names(), "loaded");

    tokio::runtime::Runtime::new()?.block_on(async {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!("edgebox listening on http://{}", listener.local_addr()?);
        axum::serve(listener, router(app))
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        Ok(())
    })
}

/// Uploads `wasm`. If `--env` or `--allow-host` is given, also replaces the
/// function's config; otherwise its existing config is kept.
fn deploy(
    name: &str,
    wasm: &PathBuf,
    env: Vec<(String, serde_json::Value)>,
    allowed_hosts: Vec<String>,
    remote: &Remote,
) -> Result<()> {
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

    if !env.is_empty() || !allowed_hosts.is_empty() {
        let config = serde_json::json!({
            "env": env.into_iter().collect::<serde_json::Map<_, _>>(),
            "allowed_hosts": allowed_hosts,
        });
        let req = ureq_agent()
            .put(format!("{url}/config"))
            .header("content-type", "application/json");
        let view = remote.call(req, Some(config.to_string().as_bytes()))?;
        println!("config: {view}");
    }
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

fn logs(name: &str, remote: &Remote) -> Result<()> {
    let url = format!(
        "{}/_api/functions/{name}/logs",
        remote.server.trim_end_matches('/')
    );
    let lines = remote.call(ureq_agent().get(&url).force_send_body(), None)?;
    for line in lines.as_array().cloned().unwrap_or_default() {
        println!(
            "{} {} {}",
            line["timestamp_ms"],
            line["stream"].as_str().unwrap_or("?"),
            line["message"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

fn init_logging(format: LogFormat) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    // Colors only on a terminal, not in `docker logs` or files.
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stdout());
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(ansi);
    match format {
        LogFormat::Text => builder.init(),
        LogFormat::Json => builder.json().init(),
    }
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

/// Parses `KEY=VALUE`.
fn parse_env(arg: &str) -> Result<(String, serde_json::Value), String> {
    let (key, value) = arg
        .split_once('=')
        .ok_or_else(|| format!("expected KEY=VALUE, got {arg:?}"))?;
    Ok((key.to_string(), value.into()))
}

/// Parses `NAME=PATH`.
fn parse_function(arg: &str) -> Result<(String, PathBuf), String> {
    let (name, path) = arg
        .split_once('=')
        .ok_or_else(|| format!("expected NAME=PATH, got {arg:?}"))?;
    validate_name(name)?;
    Ok((name.to_string(), PathBuf::from(path)))
}
