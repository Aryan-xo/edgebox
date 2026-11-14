//! How fast is a "cold start"? edgebox creates a fresh instance for every
//! request, so per-request latency *is* the cold-start time.
//!
//! Run with: `cargo guests && cargo bench -p edgebox-runtime`

use std::path::Path;

use criterion::{Criterion, criterion_group, criterion_main};
use edgebox_runtime::Runtime;
use http_body_util::{BodyExt, Empty};
use hyper::body::Bytes;

fn http_hello() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/wasm32-wasip2/release/http_hello.wasm");
    std::fs::read(&path)
        .unwrap_or_else(|_| panic!("{} not found: run `cargo guests` first", path.display()))
}

fn request() -> hyper::Request<Empty<Bytes>> {
    hyper::Request::get("http://localhost/hello")
        .body(Empty::new())
        .unwrap()
}

fn cold_start(c: &mut Criterion) {
    let wasm = http_hello();
    let runtime = Runtime::new().unwrap();
    let tokio = tokio::runtime::Runtime::new().unwrap();

    let function = runtime.load_http_bytes(&wasm).unwrap();
    c.bench_function("request: fresh instance + handle", |b| {
        b.iter(|| {
            tokio.block_on(async {
                let response = function.handle(request()).await.unwrap();
                response.into_body().collect().await.unwrap()
            })
        })
    });

    let mut group = c.benchmark_group("load");
    group.sample_size(10);
    group.bench_function("compile from .wasm", |b| {
        b.iter(|| runtime.load_http_bytes(&wasm).unwrap())
    });
    let compiled = runtime.precompile(&wasm).unwrap();
    group.bench_function("load precompiled", |b| {
        // SAFETY: `compiled` comes from `precompile`.
        b.iter(|| unsafe { runtime.load_http_precompiled(&compiled) }.unwrap())
    });
    group.finish();
}

criterion_group!(benches, cold_start);
criterion_main!(benches);
