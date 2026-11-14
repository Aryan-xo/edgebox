//! Function logs, metrics, and a JavaScript function.

mod common;

use std::path::Path;

use axum::http::{Method, StatusCode};
use common::{app, deploy, send};

#[tokio::test]
async fn function_output_is_captured_per_function() {
    let app = app();
    deploy(&app, "hello", "http_hello.wasm").await;
    send(&app, Method::GET, "/hello/one", vec![]).await;
    send(&app, Method::GET, "/hello/two", vec![]).await;

    let (status, body) = send(&app, Method::GET, "/_api/functions/hello/logs", vec![]).await;
    assert_eq!(status, StatusCode::OK);
    let lines: serde_json::Value = serde_json::from_str(&body).unwrap();
    let messages: Vec<_> = lines
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            (
                l["stream"].as_str().unwrap(),
                l["message"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        messages,
        [
            ("stdout", "handled /hello/one"),
            ("stdout", "handled /hello/two")
        ]
    );
}

#[tokio::test]
async fn metrics_count_requests_per_function() {
    let app = app();
    deploy(&app, "hello", "http_hello.wasm").await;
    send(&app, Method::GET, "/hello", vec![]).await;
    send(&app, Method::GET, "/hello", vec![]).await;
    send(&app, Method::GET, "/random-name", vec![]).await;

    let (status, text) = send(&app, Method::GET, "/_api/metrics", vec![]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        text.contains(r#"edgebox_requests_total{function="hello",status="200"} 2"#),
        "{text}"
    );
    assert!(text.contains(r#"edgebox_request_duration_seconds_count{function="hello"} 2"#));
    assert!(text.contains("edgebox_functions 1"));
    assert!(
        !text.contains("random-name"),
        "unknown names are not recorded"
    );
}

/// Built by `npm --prefix examples/js-hello run build` (needs Node.js).
#[tokio::test(flavor = "multi_thread")]
async fn javascript_function_serves_requests() {
    let wasm = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/wasm32-wasip2/release/js_hello.wasm");
    if !wasm.exists() {
        eprintln!("skipping: build it with `npm --prefix examples/js-hello run build`");
        return;
    }
    let app = app();
    deploy(&app, "js", "js_hello.wasm").await;

    let (status, body) = send(&app, Method::GET, "/js/hi?x=1", vec![]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, "Hello from JavaScript! You requested /js/hi?x=1\n");

    let (_, logs) = send(&app, Method::GET, "/_api/functions/js/logs", vec![]).await;
    assert!(logs.contains("handling GET /js/hi"), "{logs}");
}
