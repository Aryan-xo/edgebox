//! KV store, secrets and outbound HTTP.

mod common;

use axum::Router;
use axum::http::{Method, StatusCode};
use common::{app, app_with, deploy, send, send_json};
use edgebox_runtime::Runtime;
use edgebox_server::store::Store;

async fn put_config(app: &Router, name: &str, config: serde_json::Value) -> (StatusCode, String) {
    let uri = format!("/_api/functions/{name}/config");
    send_json(app, Method::PUT, &uri, config).await
}

/// Starts a local server to call, returning its address.
async fn upstream() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new().fallback(|| async { "hello from upstream" });
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr.to_string()
}

#[tokio::test]
async fn kv_store_persists_between_requests() {
    let app = app();
    deploy(&app, "counter", "counter.wasm").await;

    for n in 1..=3 {
        let (status, body) = send(&app, Method::GET, "/counter", vec![]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, format!("You are visitor #{n}\n"));
    }
}

#[tokio::test]
async fn kv_store_is_private_to_each_function() {
    let app = app();
    deploy(&app, "a", "counter.wasm").await;
    deploy(&app, "b", "counter.wasm").await;

    send(&app, Method::GET, "/a", vec![]).await;
    send(&app, Method::GET, "/a", vec![]).await;
    let (_, body) = send(&app, Method::GET, "/b", vec![]).await;
    assert_eq!(body, "You are visitor #1\n");
}

#[tokio::test]
async fn kv_data_survives_restart_and_is_deleted_with_function() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("edgebox.db");
    let open = || app_with(Runtime::new().unwrap(), Store::open(&db).unwrap(), None);

    let app = open();
    deploy(&app, "counter", "counter.wasm").await;
    send(&app, Method::GET, "/counter", vec![]).await;

    let app = open();
    let (_, body) = send(&app, Method::GET, "/counter", vec![]).await;
    assert_eq!(body, "You are visitor #2\n");

    send(&app, Method::DELETE, "/_api/functions/counter", vec![]).await;
    deploy(&app, "counter", "counter.wasm").await;
    let (_, body) = send(&app, Method::GET, "/counter", vec![]).await;
    assert_eq!(body, "You are visitor #1\n");
}

#[tokio::test]
async fn secrets_are_readable_by_function_but_not_by_api() {
    let app = app();
    deploy(&app, "fetch", "fetch.wasm").await;

    let (_, body) = send(&app, Method::GET, "/fetch/env", vec![]).await;
    assert_eq!(body, "GREETING is not set\n");

    let (status, body) = put_config(
        &app,
        "fetch",
        serde_json::json!({"env": {"GREETING": "hi from a secret"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("hi from a secret"), "{body}");

    let (_, body) = send(&app, Method::GET, "/fetch/env", vec![]).await;
    assert_eq!(body, "hi from a secret\n");

    let (_, body) = send(&app, Method::GET, "/_api/functions/fetch/config", vec![]).await;
    assert_eq!(
        body,
        r#"{"env":{"GREETING":"<redacted>"},"allowed_hosts":[]}"#
    );
}

#[tokio::test]
async fn config_survives_redeploy() {
    let app = app();
    deploy(&app, "fetch", "fetch.wasm").await;
    put_config(
        &app,
        "fetch",
        serde_json::json!({"env": {"GREETING": "kept"}}),
    )
    .await;

    deploy(&app, "fetch", "fetch.wasm").await;
    let (_, body) = send(&app, Method::GET, "/fetch/env", vec![]).await;
    assert_eq!(body, "kept\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn outbound_http_is_denied_unless_host_is_allowed() {
    let upstream = upstream().await;
    let app = app();
    deploy(&app, "fetch", "fetch.wasm").await;
    let uri = format!("/fetch?url=http://{upstream}/");

    let (status, body) = send(&app, Method::GET, &uri, vec![]).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(body.contains("HttpRequestDenied"), "{body}");

    put_config(
        &app,
        "fetch",
        serde_json::json!({"allowed_hosts": ["127.0.0.1"]}),
    )
    .await;
    let (status, body) = send(&app, Method::GET, &uri, vec![]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, "hello from upstream");
}

#[tokio::test]
async fn config_api_validates_input() {
    let app = app();
    let (status, _) = put_config(&app, "nope", serde_json::json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    deploy(&app, "fetch", "fetch.wasm").await;
    let (status, body) = put_config(
        &app,
        "fetch",
        serde_json::json!({"allowed_hosts": ["http://evil.com/x"]}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _) = put_config(&app, "fetch", serde_json::json!({"unknown": 1})).await;
    assert!(status.is_client_error());
}
