//! Helpers shared by the server integration tests.
#![allow(dead_code)]

use std::path::Path;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use edgebox_runtime::Runtime;
use edgebox_server::store::Store;
use edgebox_server::{App, router};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// Bytes of an example guest built by `cargo guests`.
pub fn guest(file: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/wasm32-wasip2/release")
        .join(file);
    std::fs::read(&path)
        .unwrap_or_else(|_| panic!("{} not found: run `cargo guests` first", path.display()))
}

pub fn app_with(runtime: Runtime, store: Store, token: Option<&str>) -> Router {
    router(App::new(runtime, store, token.map(String::from)).unwrap())
}

/// An app with default limits, an in-memory store and no token.
pub fn app() -> Router {
    app_with(Runtime::new().unwrap(), Store::in_memory().unwrap(), None)
}

pub async fn send(app: &Router, method: Method, uri: &str, body: Vec<u8>) -> (StatusCode, String) {
    send_with_token(app, method, uri, body, None).await
}

pub async fn send_with_token(
    app: &Router,
    method: Method,
    uri: &str,
    body: Vec<u8>,
    token: Option<&str>,
) -> (StatusCode, String) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost");
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// Deploys an example guest under `name`, asserting success.
pub async fn deploy(app: &Router, name: &str, file: &str) {
    let uri = format!("/_api/functions/{name}");
    let (status, body) = send(app, Method::PUT, &uri, guest(file)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}
