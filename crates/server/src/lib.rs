//! HTTP front door for edgebox.
//!
//! - `/<name>/...` runs the function named `<name>`.
//! - `/_api/...` is the admin API (deploy, list, delete).

mod api;
pub mod store;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use edgebox_runtime::{HttpFunction, Runtime};

pub use crate::api::ApiError;
use crate::store::{FunctionInfo, Store};

/// Largest `.wasm` the deploy API accepts.
const MAX_UPLOAD_BYTES: usize = 64 * 1024 * 1024;

/// Server state: the runtime, the function store and the live functions.
pub struct App {
    runtime: Runtime,
    store: Store,
    token: Option<String>,
    functions: RwLock<HashMap<String, HttpFunction>>,
}

impl App {
    /// Creates the app and loads the latest version of every stored function.
    ///
    /// When `token` is set, the admin API requires `Authorization: Bearer <token>`.
    pub fn new(runtime: Runtime, store: Store, token: Option<String>) -> Result<Arc<Self>> {
        let mut functions = HashMap::new();
        for info in store.list()? {
            let wasm = store.latest_wasm(&info.name)?.unwrap_or_default();
            let function = runtime
                .load_http_bytes(&wasm)
                .map_err(anyhow::Error::from)
                .with_context(|| format!("failed to load stored function {:?}", info.name))?;
            functions.insert(info.name, function);
        }
        Ok(Arc::new(Self {
            runtime,
            store,
            token,
            functions: RwLock::new(functions),
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
        validate_name(name).map_err(|err| ApiError::new(StatusCode::BAD_REQUEST, err))?;
        let function = self
            .runtime
            .load_http_bytes(wasm)
            .map_err(|err| ApiError::new(StatusCode::BAD_REQUEST, format!("{err:#}")))?;
        let info = self
            .store
            .save(name, wasm)
            .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}")))?;
        self.functions
            .write()
            .unwrap()
            .insert(name.to_string(), function);
        Ok(info)
    }

    fn delete(&self, name: &str) -> Result<bool> {
        let existed = self.store.delete(name)?;
        self.functions.write().unwrap().remove(name);
        Ok(existed)
    }

    fn function(&self, name: &str) -> Option<HttpFunction> {
        self.functions.read().unwrap().get(name).cloned()
    }
}

/// Builds the HTTP router for `app`.
pub fn router(app: Arc<App>) -> Router {
    let admin = Router::new()
        .route("/_api/functions", get(api::list))
        .route(
            "/_api/functions/{name}",
            put(api::deploy).delete(api::delete),
        )
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

    match function.handle(req).await {
        Ok(response) => response.map(Body::new),
        Err(err) if edgebox_runtime::is_timeout(&err) => {
            eprintln!("function {name:?} timed out: {err}");
            (StatusCode::GATEWAY_TIMEOUT, "function timed out\n").into_response()
        }
        Err(err) => {
            eprintln!("function {name:?} failed: {err:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, "function failed\n").into_response()
        }
    }
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
