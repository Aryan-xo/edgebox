use std::path::Path;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use edgebox_runtime::Runtime;
use edgebox_server::{Functions, router};
use http_body_util::BodyExt;
use tower::ServiceExt;

fn app() -> axum::Router {
    let wasm = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/wasm32-wasip2/release/http_hello.wasm");
    assert!(
        wasm.exists(),
        "{} not found: run `cargo guests` first",
        wasm.display()
    );

    let hello = Runtime::new().unwrap().load_http(&wasm).unwrap();
    router(Functions::from([("hello".to_string(), hello)]))
}

async fn get(uri: &str) -> (StatusCode, String) {
    let req = Request::get(uri)
        .header("host", "localhost")
        .body(Body::empty())
        .unwrap();
    let res = app().oneshot(req).await.unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn routes_request_to_function() {
    let (status, body) = get("/hello").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "Hello from WASM! You requested /hello\n");
}

#[tokio::test]
async fn function_sees_full_path_and_query() {
    let (_, body) = get("/hello/world?x=1").await;
    assert_eq!(body, "Hello from WASM! You requested /hello/world?x=1\n");
}

#[tokio::test]
async fn unknown_function_is_404() {
    let (status, body) = get("/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, "no function named \"nope\"\n");
}

#[test]
fn command_component_is_rejected_as_http_function() {
    let wasm =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/wasm32-wasip2/release/hello.wasm");
    let err = Runtime::new().unwrap().load_http(&wasm).err().unwrap();
    assert!(err.to_string().contains("is not an HTTP function"), "{err}");
}
