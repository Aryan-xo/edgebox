# Benchmarks

edgebox creates a **fresh instance for every request**, so per-request latency is the cold-start time. There are no warm instances to hide behind.

Measured on an Apple M4 Pro, release build, with the `http-hello` example function.

## Cold start (criterion)

```sh
cargo guests && cargo bench -p edgebox-runtime
```

| | Time |
| --- | --- |
| Fresh instance + handle one request | **26.8 µs** |
| Load a function from cached native code | **88 µs** |
| Compile a function from `.wasm` | 8.7 ms |

## Throughput (ApacheBench)

```sh
edgebox serve --data :memory: hello=target/wasm32-wasip2/release/http_hello.wasm
ab -k -n 50000 -c 50 http://127.0.0.1:8080/hello
```

| Allocator | Requests/sec | p50 | p99 |
| --- | --- | --- | --- |
| On-demand (wasmtime default) | 23.7k – 26.5k | 2 ms | 4 ms |
| **Pooling (edgebox)** | **66.6k – 77.9k** | **1 ms** | **2–3 ms** |

Two runs each, same build, only the allocation strategy changed. `ab` is single-threaded, so these numbers are a floor, not a ceiling.

## What made it fast

1. **Pre-instantiation.** Imports are resolved once per function (`InstancePre`), not per request. This is why even the baseline is ~31 µs.
2. **Pooling allocator.** Memory slots are reserved once and reused. The default allocator maps and unmaps memory for every instance, which contends under concurrency: pooling cut per-request time by 13% and gave **2.8× the throughput**.
3. **Cached native code.** Deploys store the compiled machine code in SQLite next to the `.wasm`. On restart, loading takes 88 µs instead of 8.7 ms per function (~100×). If the cache was built by a different wasmtime version, edgebox recompiles and refreshes it.
