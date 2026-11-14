mod common;

use axum::http::{Method, StatusCode};
use common::{app, app_with, deploy, guest, send, send_with_token};
use edgebox_runtime::Runtime;
use edgebox_server::store::Store;

#[tokio::test]
async fn deployed_function_serves_requests() {
    let app = app();
    deploy(&app, "hello", "http_hello.wasm").await;

    let (status, body) = send(&app, Method::GET, "/hello/world?x=1", vec![]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "Hello from WASM! You requested /hello/world?x=1\n");
}

#[tokio::test]
async fn deploy_returns_version_info_and_list_shows_it() {
    let app = app();
    let (_, body) = send(
        &app,
        Method::PUT,
        "/_api/functions/hello",
        guest("http_hello.wasm"),
    )
    .await;
    let info: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(info["name"], "hello");
    assert_eq!(info["version"], 1);

    let (status, body) = send(&app, Method::GET, "/_api/functions", vec![]).await;
    assert_eq!(status, StatusCode::OK);
    let list: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(list[0]["name"], "hello");
}

#[tokio::test]
async fn unknown_function_is_404() {
    let (status, body) = send(&app(), Method::GET, "/nope", vec![]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, "no function named \"nope\"\n");
}

#[tokio::test]
async fn delete_removes_function() {
    let app = app();
    deploy(&app, "hello", "http_hello.wasm").await;

    let (status, _) = send(&app, Method::DELETE, "/_api/functions/hello", vec![]).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&app, Method::GET, "/hello", vec![]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(&app, Method::DELETE, "/_api/functions/hello", vec![]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rejects_invalid_uploads() {
    let app = app();

    let (status, body) = send(&app, Method::PUT, "/_api/functions/x", b"nope".to_vec()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("not a valid WebAssembly component"), "{body}");

    let (status, body) = send(&app, Method::PUT, "/_api/functions/x", guest("hello.wasm")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("not an HTTP function"), "{body}");

    let (status, body) = send(
        &app,
        Method::PUT,
        "/_api/functions/_x",
        guest("http_hello.wasm"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("invalid function name"), "{body}");
}

#[tokio::test]
async fn admin_api_requires_token_when_configured() {
    let app = app_with(
        Runtime::new().unwrap(),
        Store::in_memory().unwrap(),
        Some("s3cret"),
    );

    let (status, _) = send(&app, Method::GET, "/_api/functions", vec![]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) =
        send_with_token(&app, Method::GET, "/_api/functions", vec![], Some("wrong")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) =
        send_with_token(&app, Method::GET, "/_api/functions", vec![], Some("s3cret")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn functions_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("edgebox.db");
    let open = || app_with(Runtime::new().unwrap(), Store::open(&db).unwrap(), None);

    deploy(&open(), "hello", "http_hello.wasm").await;

    let (status, _) = send(&open(), Method::GET, "/hello", vec![]).await;
    assert_eq!(status, StatusCode::OK);
}
