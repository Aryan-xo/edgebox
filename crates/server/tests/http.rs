use std::path::Path;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use edgebox_runtime::Runtime;
use edgebox_server::store::Store;
use edgebox_server::{App, router};
use http_body_util::BodyExt;
use tower::ServiceExt;

fn guest(file: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/wasm32-wasip2/release")
        .join(file);
    std::fs::read(&path)
        .unwrap_or_else(|_| panic!("{} not found: run `cargo guests` first", path.display()))
}

fn app_with(store: Store, token: Option<&str>) -> Router {
    let app = App::new(Runtime::new().unwrap(), store, token.map(String::from)).unwrap();
    router(app)
}

fn app() -> Router {
    app_with(Store::in_memory().unwrap(), None)
}

async fn send(app: &Router, method: Method, uri: &str, body: Vec<u8>) -> (StatusCode, String) {
    send_with_token(app, method, uri, body, None).await
}

async fn send_with_token(
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

async fn deploy_hello(app: &Router) {
    let (status, body) = send(
        app,
        Method::PUT,
        "/_api/functions/hello",
        guest("http_hello.wasm"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn deployed_function_serves_requests() {
    let app = app();
    deploy_hello(&app).await;

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
    deploy_hello(&app).await;

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
    let app = app_with(Store::in_memory().unwrap(), Some("s3cret"));

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

    deploy_hello(&app_with(Store::open(&db).unwrap(), None)).await;

    let restarted = app_with(Store::open(&db).unwrap(), None);
    let (status, _) = send(&restarted, Method::GET, "/hello", vec![]).await;
    assert_eq!(status, StatusCode::OK);
}
