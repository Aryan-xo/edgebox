//! HTTP front door for edgebox.
//!
//! - `/<name>/...` runs the function named `<name>`.
//! - `/_api/...` is the admin API (deploy, list, delete, config, logs, metrics).

mod api;
pub mod logs;
pub mod metrics;
pub mod store;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use anyhow::{Context, Result};
use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use edgebox_runtime::{HttpFunction, Kv, Runtime, Settings};

pub use crate::api::ApiError;
use crate::logs::FunctionLogs;
use crate::metrics::Metrics;
use crate::store::{FunctionConfig, FunctionInfo, Store};

/// Largest `.wasm` the deploy API accepts.
const MAX_UPLOAD_BYTES: usize = 64 * 1024 * 1024;

/// Server state: the runtime, the function store and the live functions.
pub struct App {
    runtime: Runtime,
    store: Arc<Store>,
    token: Option<String>,
    functions: RwLock<HashMap<String, HttpFunction>>,
    logs: Arc<FunctionLogs>,
    metrics: Metrics,
}

impl App {
    /// Creates the app and loads the latest version of every stored function.
    ///
    /// When `token` is set, the admin API requires `Authorization: Bearer <token>`.
    pub fn new(runtime: Runtime, store: Store, token: Option<String>) -> Result<Arc<Self>> {
        let store = Arc::new(store);
        let logs = Arc::new(FunctionLogs::default());
        let mut functions = HashMap::new();
        for info in store.list()? {
            let function = load_stored(&runtime, &store, &info.name)
                .with_context(|| format!("failed to load stored function {:?}", info.name))?;
            let config = store.config(&info.name)?;
            let settings = settings(&store, &logs, &info.name, config);
            functions.insert(info.name, function.with_settings(settings));
        }
        Ok(Arc::new(Self {
            runtime,
            store,
            token,
            functions: RwLock::new(functions),
            logs,
            metrics: Metrics::default(),
        }))
    }

    /// Names of the live functions, sorted.
    pub fn function_names(&self) -> Vec<String> {
        let mut names: Vec<_> = self.functions.read().unwrap().keys().cloned().collect();
        names.sort();
        names
    }

    /// Compiles, stores and hot-swaps in a new version of `name`.
    pub fn deploy(&self, name: &str, wasm: &[u8]) -> Result<FunctionInfo, ApiError> {
        let bad_request = |err| ApiError::new(StatusCode::BAD_REQUEST, format!("{err:#}"));
        validate_name(name).map_err(|err| ApiError::new(StatusCode::BAD_REQUEST, err))?;

        // Compile once, keep the native code: restarts load it without recompiling.
        let compiled = self.runtime.precompile(wasm).map_err(bad_request)?;
        // SAFETY: `compiled` was produced by `precompile` just above.
        let function =
            unsafe { self.runtime.load_http_precompiled(&compiled) }.map_err(bad_request)?;
        let internal = |err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}"));
        let info = self.store.save(name, wasm, &compiled).map_err(internal)?;
        let config = self.store.config(name).map_err(internal)?;
        let function = function.with_settings(settings(&self.store, &self.logs, name, config));
        self.functions
            .write()
            .unwrap()
            .insert(name.to_string(), function);
        tracing::info!(
            function = name,
            version = info.version,
            size = info.size,
            "deployed"
        );
        Ok(info)
    }

    /// Replaces the config of a live function. Takes effect on the next request.
    fn set_config(&self, name: &str, config: FunctionConfig) -> Result<(), ApiError> {
        config
            .validate()
            .map_err(|err| ApiError::new(StatusCode::BAD_REQUEST, err))?;
        let Some(function) = self.function(name) else {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                format!("no function named {name:?}"),
            ));
        };
        self.store
            .set_config(name, &config)
            .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}")))?;
        let function = function.with_settings(settings(&self.store, &self.logs, name, config));
        self.functions
            .write()
            .unwrap()
            .insert(name.to_string(), function);
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<bool> {
        let existed = self.store.delete(name)?;
        self.functions.write().unwrap().remove(name);
        self.logs.remove(name);
        self.metrics.remove(name);
        if existed {
            tracing::info!(function = name, "deleted");
        }
        Ok(existed)
    }

    fn function(&self, name: &str) -> Option<HttpFunction> {
        self.functions.read().unwrap().get(name).cloned()
    }
}

/// Runtime settings for `name`: its config, its own KV namespace and its log sink.
fn settings(
    store: &Arc<Store>,
    logs: &Arc<FunctionLogs>,
    name: &str,
    config: FunctionConfig,
) -> Settings {
    Settings {
        env: config.env.into_iter().collect(),
        allowed_hosts: config.allowed_hosts,
        kv: Some(Kv::new(store.clone(), name)),
        logs: Some(logs.sink(name)),
    }
}

/// Loads the latest version of `name`, from cached native code when possible.
fn load_stored(runtime: &Runtime, store: &Store, name: &str) -> Result<HttpFunction> {
    let stored = store.latest(name)?.context("function was deleted")?;
    if let Some(compiled) = &stored.compiled {
        // SAFETY: the cache is only written with `Runtime::precompile` output
        // (here and in `App::deploy`). Protect the database file accordingly.
        if let Ok(function) = unsafe { runtime.load_http_precompiled(compiled) } {
            return Ok(function);
        }
    }
    // No cache, or it was built by another wasmtime version: recompile and refresh it.
    let compiled = runtime.precompile(&stored.wasm)?;
    // SAFETY: produced by `precompile` just above.
    let function = unsafe { runtime.load_http_precompiled(&compiled) }?;
    store.set_compiled(name, stored.version, &compiled)?;
    Ok(function)
}

/// Builds the HTTP router for `app`.
pub fn router(app: Arc<App>) -> Router {
    let admin = Router::new()
        .route("/_api/functions", get(api::list))
        .route(
            "/_api/functions/{name}",
            put(api::deploy).delete(api::delete),
        )
        .route(
            "/_api/functions/{name}/config",
            get(api::get_config).put(api::put_config),
        )
        .route("/_api/functions/{name}/logs", get(api::logs))
        .route("/_api/metrics", get(api::metrics))
        .route_layer(middleware::from_fn_with_state(app.clone(), api::auth))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES));

    admin.fallback(invoke).with_state(app)
}

/// Function names become URL path segments.
pub fn validate_name(name: &str) -> Result<(), String> {
    let valid = (1..=64).contains(&name.len())
        && !name.starts_with('_')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "invalid function name {name:?}: use 1-64 letters, digits, '-' or '_', not starting with '_'"
        ))
    }
}

async fn invoke(State(app): State<Arc<App>>, req: Request) -> Response {
    let name = function_name(req.uri().path()).to_owned();
    let Some(function) = app.function(&name) else {
        return (
            StatusCode::NOT_FOUND,
            format!("no function named {name:?}\n"),
        )
            .into_response();
    };

    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let start = Instant::now();
    let response = match function.handle(req).await {
        Ok(response) => response.map(Body::new),
        Err(err) if edgebox_runtime::is_timeout(&err) => {
            tracing::warn!(function = %name, "{err}");
            (StatusCode::GATEWAY_TIMEOUT, "function timed out\n").into_response()
        }
        Err(err) => {
            tracing::error!(function = %name, "function failed: {err:#}");
            (StatusCode::INTERNAL_SERVER_ERROR, "function failed\n").into_response()
        }
    };

    let elapsed = start.elapsed();
    let status = response.status().as_u16();
    app.metrics.record(&name, status, elapsed);
    tracing::info!(
        function = %name,
        %method,
        path,
        status,
        duration_us = elapsed.as_micros() as u64,
        "request"
    );
    response
}

/// First segment of the path: `/hello/world` → `hello`.
fn function_name(path: &str) -> &str {
    path.trim_start_matches('/').split('/').next().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_name_is_first_path_segment() {
        assert_eq!(function_name("/hello"), "hello");
        assert_eq!(function_name("/hello/a/b"), "hello");
        assert_eq!(function_name("/"), "");
    }

    #[test]
    fn names_are_validated() {
        assert!(validate_name("hello-world_2").is_ok());
        for bad in ["", "_api", "a/b", "a b", &"x".repeat(65)] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
    }
}
