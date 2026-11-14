# edgebox

[![CI](https://github.com/Aryan-xo/edgebox/actions/workflows/ci.yml/badge.svg)](https://github.com/Aryan-xo/edgebox/actions/workflows/ci.yml)

A tiny serverless runtime for WebAssembly, written in Rust. Think a mini Cloudflare Workers: give edgebox a function, and it serves HTTP requests with it in a sandbox.

> **Status:** feature-complete for a v0.1: deploy, serve, sandbox limits, KV, secrets, outbound HTTP, logs, metrics, Rust and JavaScript functions (see [Roadmap](#roadmap)).

## Quickstart

```sh
rustup target add wasm32-wasip2
cargo guests                       # build the example functions to .wasm
cargo install --path crates/cli    # installs the `edgebox` binary

edgebox serve &                    # starts on 127.0.0.1:8080, stores functions in edgebox.db
edgebox deploy hello target/wasm32-wasip2/release/http_hello.wasm
curl localhost:8080/hello/world
# Hello from WASM! You requested /hello/world
```

`/<name>/...` is routed to the function named `<name>`. Functions use the standard [`wasi:http`](https://github.com/WebAssembly/wasi-http) interface, so components built for other WASI runtimes work too.

| Command | What it does |
| --- | --- |
| `edgebox serve [NAME=PATH...]` | Start the server (optionally deploying files at startup) |
| `edgebox deploy NAME PATH` | Upload a new version; it goes live immediately, no restart |
| `edgebox list` | Show deployed functions and versions |
| `edgebox logs NAME` | Show a function's recent stdout/stderr |
| `edgebox run PATH [ARGS...]` | Run a CLI-style component locally |

### Admin API

Set `EDGEBOX_TOKEN` on the server and client to require `Authorization: Bearer <token>`.

| Method | Path | |
| --- | --- | --- |
| `PUT` | `/_api/functions/{name}` | Deploy (`.wasm` body). Returns `{name, version, size, deployed_at}` |
| `GET` | `/_api/functions` | List latest versions |
| `DELETE` | `/_api/functions/{name}` | Delete all versions, config and KV data |
| `GET` | `/_api/functions/{name}/config` | Show config (secret values redacted) |
| `PUT` | `/_api/functions/{name}/config` | Replace config: `{"env": {...}, "allowed_hosts": [...]}` |
| `GET` | `/_api/functions/{name}/logs` | Recent stdout/stderr lines |
| `GET` | `/_api/metrics` | Prometheus metrics |

## What functions can use

Everything is off by default and granted per function.

| Feature | How | Example |
| --- | --- | --- |
| **KV store** | Import `edgebox:kv/store` ([`wit/kv.wit`](wit/kv.wit)). Durable, private to each function | [`counter`](examples/counter/src/lib.rs) |
| **Secrets** | Environment variables: `edgebox deploy NAME FILE --env API_KEY=...` | [`fetch`](examples/fetch/src/lib.rs) |
| **Outbound HTTP** | Standard `wasi:http/outgoing-handler`, only to allowed hosts: `--allow-host api.example.com` | [`fetch`](examples/fetch/src/lib.rs) |

```sh
edgebox deploy counter target/wasm32-wasip2/release/counter.wasm
curl localhost:8080/counter      # You are visitor #1
curl localhost:8080/counter      # You are visitor #2

edgebox deploy fetch target/wasm32-wasip2/release/fetch.wasm --allow-host example.com
curl "localhost:8080/fetch?url=https://example.com/"   # works; any other host → 502
```

Config is kept across redeploys and changes take effect on the next request, with no recompilation.

## Any language that compiles to a component

The examples are in Rust, but any [WASI component](https://component-model.bytecodealliance.org/) that exports `wasi:http/incoming-handler` works. [`examples/js-hello`](examples/js-hello/app.js) is a JavaScript function built with [jco](https://github.com/bytecodealliance/jco):

```js
addEventListener("fetch", (event) =>
  event.respondWith(new Response("Hello from JavaScript!\n")),
);
```

```sh
npm --prefix examples/js-hello ci && npm --prefix examples/js-hello run build
edgebox deploy js target/wasm32-wasip2/release/js_hello.wasm
```

## Observability

| What | Where |
| --- | --- |
| Request log: function, method, path, status, duration | server output (`--log-format json` for JSON; filter with `RUST_LOG`) |
| A function's stdout/stderr | `edgebox logs NAME`, or `GET /_api/functions/{name}/logs` (last 200 lines) |
| Prometheus metrics: requests by status, latency histogram, deployed functions | `GET /_api/metrics` |

```text
$ edgebox logs js
1794651284802 stdout handling GET /js/demo
```

## Performance

Every request gets a fresh, isolated instance, and it is still fast (Apple M4 Pro):

- **26.8 µs** per-request cold start
- **~70k req/s** on one machine
- **88 µs** to load a function on restart, from cached native code

Details and method in [BENCHMARKS.md](BENCHMARKS.md).

## Sandbox limits

Every request runs with a time and memory budget. A function that blows it is stopped, and the server keeps serving.

| Limit | Default | Flag / env | On violation |
| --- | --- | --- | --- |
| Time per request | 10 s | `--timeout-ms` / `EDGEBOX_TIMEOUT_MS` | `504 Gateway Timeout` |
| Memory per instance | 128 MiB | `--max-memory-mb` / `EDGEBOX_MAX_MEMORY_MB` | `500` (instance trapped) |

Timeouts use wasmtime's epoch interruption, so even `loop {}` is stopped: with `--timeout-ms 50`, an infinite loop returns 504 in ~52 ms and the CPU goes back to idle. Try it with `examples/chaos`.

## How it works

1. A function is a [WASI](https://wasi.dev) component (`wasm32-wasip2`) that exports `wasi:http/incoming-handler`.
2. On deploy, edgebox compiles it with [wasmtime](https://wasmtime.dev), stores it in SQLite as a new version, and swaps it in live.
3. Every request runs in a fresh, isolated instance.
4. Guests have no file or socket access, and outbound HTTP only to allowed hosts.

## Project layout

| Path | Purpose |
| --- | --- |
| `crates/runtime` | Loads and runs components (wasmtime) |
| `crates/server` | HTTP router (axum), admin API, SQLite store |
| `crates/cli` | The `edgebox` command |
| `wit/` | The `edgebox:kv` interface offered to functions |
| `examples/` | Example functions |

## Development

```sh
cargo guests && cargo test --workspace
```

## Roadmap

- [x] Run a `.wasm` component from the CLI
- [x] HTTP in → WASM → HTTP out
- [x] Upload & deploy functions (versioned, hot-swapped)
- [x] Sandbox limits (time, memory)
- [x] Sub-millisecond cold starts (26.8 µs)
- [x] KV store, secrets, outbound fetch
- [x] Metrics, logs, and JavaScript functions

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
