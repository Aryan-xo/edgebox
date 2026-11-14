//! Admin API under `/_api`: deploy, list and delete functions.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::App;
use crate::logs::LogLine;
use crate::store::{FunctionConfig, FunctionInfo};

/// `PUT /_api/functions/{name}` with the `.wasm` as the body.
pub async fn deploy(
    State(app): State<Arc<App>>,
    Path(name): Path<String>,
    wasm: Bytes,
) -> Result<Json<FunctionInfo>, ApiError> {
    // Compiling is CPU-heavy: keep it off the async workers.
    let info = tokio::task::spawn_blocking(move || app.deploy(&name, &wasm))
        .await
        .map_err(ApiError::internal)??;
    Ok(Json(info))
}

/// `GET /_api/functions`
pub async fn list(State(app): State<Arc<App>>) -> Result<Json<Vec<FunctionInfo>>, ApiError> {
    Ok(Json(app.store.list().map_err(ApiError::internal)?))
}

/// `DELETE /_api/functions/{name}`
pub async fn delete(
    State(app): State<Arc<App>>,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    if app.delete(&name).map_err(ApiError::internal)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::new(
            StatusCode::NOT_FOUND,
            format!("no function named {name:?}"),
        ))
    }
}

/// A function's config as the API shows it: secret values are never returned.
#[derive(Serialize)]
pub struct ConfigView {
    env: BTreeMap<String, &'static str>,
    allowed_hosts: Vec<String>,
}

impl From<FunctionConfig> for ConfigView {
    fn from(config: FunctionConfig) -> Self {
        Self {
            env: config.env.into_keys().map(|k| (k, "<redacted>")).collect(),
            allowed_hosts: config.allowed_hosts,
        }
    }
}

/// `GET /_api/functions/{name}/config`
pub async fn get_config(
    State(app): State<Arc<App>>,
    Path(name): Path<String>,
) -> Result<Json<ConfigView>, ApiError> {
    if app.function(&name).is_none() {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            format!("no function named {name:?}"),
        ));
    }
    let config = app.store.config(&name).map_err(ApiError::internal)?;
    Ok(Json(config.into()))
}

/// `PUT /_api/functions/{name}/config` with `{"env": {...}, "allowed_hosts": [...]}`.
/// Replaces the whole config.
pub async fn put_config(
    State(app): State<Arc<App>>,
    Path(name): Path<String>,
    Json(config): Json<FunctionConfig>,
) -> Result<Json<ConfigView>, ApiError> {
    app.set_config(&name, config.clone())?;
    Ok(Json(config.into()))
}

/// `GET /_api/functions/{name}/logs`: recent stdout/stderr lines, oldest first.
pub async fn logs(
    State(app): State<Arc<App>>,
    Path(name): Path<String>,
) -> Result<Json<Vec<LogLine>>, ApiError> {
    if app.function(&name).is_none() {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            format!("no function named {name:?}"),
        ));
    }
    Ok(Json(app.logs.recent(&name)))
}

/// `GET /_api/metrics` in the Prometheus text format.
pub async fn metrics(State(app): State<Arc<App>>) -> impl IntoResponse {
    let deployed = app.functions.read().unwrap().len();
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        app.metrics.render(deployed),
    )
}

/// Requires `Authorization: Bearer <token>` when the server has a token.
pub async fn auth(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
    if let Some(token) = &app.token {
        let given = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        if !constant_time_eq(given.as_bytes(), token.as_bytes()) {
            return ApiError::new(StatusCode::UNAUTHORIZED, "missing or invalid token")
                .into_response();
        }
    }
    next.run(req).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A JSON error: `{"error": "..."}`.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    fn internal(err: impl std::fmt::Display) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        #[derive(Serialize)]
        struct Body {
            error: String,
        }
        (
            self.status,
            Json(Body {
                error: self.message,
            }),
        )
            .into_response()
    }
}
