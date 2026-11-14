//! Misbehaving functions are stopped, and the server keeps serving.

mod common;

use std::time::{Duration, Instant};

use axum::Router;
use axum::http::{Method, StatusCode};
use common::{app_with, deploy, send};
use edgebox_runtime::{Limits, Runtime};
use edgebox_server::store::Store;

const TIMEOUT: Duration = Duration::from_millis(100);

async fn chaos_app() -> Router {
    let limits = Limits {
        timeout: TIMEOUT,
        max_memory_bytes: 32 * 1024 * 1024,
    };
    let app = app_with(
        Runtime::with_limits(limits).unwrap(),
        Store::in_memory().unwrap(),
        None,
    );
    deploy(&app, "chaos", "chaos.wasm").await;
    app
}

/// Sends a request and returns its status and how long it took.
async fn timed(app: &Router, uri: &str) -> (StatusCode, Duration) {
    let start = Instant::now();
    let (status, _) = send(app, Method::GET, uri, vec![]).await;
    (status, start.elapsed())
}

#[tokio::test(flavor = "multi_thread")]
async fn infinite_loop_is_interrupted() {
    let app = chaos_app().await;

    let (status, elapsed) = timed(&app, "/chaos/loop").await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert!(elapsed < TIMEOUT * 3, "took {elapsed:?}");

    let (status, _) = timed(&app, "/chaos/ok").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "server still serves after a timeout"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn blocked_function_times_out() {
    let app = chaos_app().await;

    let (status, elapsed) = timed(&app, "/chaos/sleep").await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert!(elapsed < TIMEOUT * 3, "took {elapsed:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_limit_is_enforced() {
    let app = chaos_app().await;

    let (status, _) = timed(&app, "/chaos/alloc").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    let (status, _) = timed(&app, "/chaos/ok").await;
    assert_eq!(status, StatusCode::OK, "server still serves after an OOM");
}
