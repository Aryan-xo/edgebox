//! HTTP front door: routes `/<name>/...` to the function named `<name>`.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use edgebox_runtime::HttpFunction;

/// Functions being served, by name.
pub type Functions = HashMap<String, HttpFunction>;

/// Builds the router. The function sees the full request path, e.g. `/hello/world`.
pub fn router(functions: Functions) -> Router {
    Router::new()
        .fallback(invoke)
        .with_state(Arc::new(functions))
}

async fn invoke(State(functions): State<Arc<Functions>>, req: Request) -> Response {
    let name = function_name(req.uri().path()).to_owned();
    let Some(function) = functions.get(&name) else {
        return (
            StatusCode::NOT_FOUND,
            format!("no function named {name:?}\n"),
        )
            .into_response();
    };

    match function.handle(req).await {
        Ok(response) => response.map(Body::new),
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
    use super::function_name;

    #[test]
    fn function_name_is_first_path_segment() {
        assert_eq!(function_name("/hello"), "hello");
        assert_eq!(function_name("/hello/a/b"), "hello");
        assert_eq!(function_name("/"), "");
    }
}
