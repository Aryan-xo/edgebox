# edgebox

[![CI](https://github.com/Aryan-xo/edgebox/actions/workflows/ci.yml/badge.svg)](https://github.com/Aryan-xo/edgebox/actions/workflows/ci.yml)

A tiny serverless runtime for WebAssembly, written in Rust. Think a mini Cloudflare Workers: give edgebox a function, and it serves HTTP requests with it in a sandbox.

> **Status:** early. HTTP functions work; deploys and sandbox limits are next (see [Roadmap](#roadmap)).

## Quickstart

```sh
rustup target add wasm32-wasip2
cargo guests   # build the example functions to .wasm

cargo run -p edgebox -- serve hello=target/wasm32-wasip2/release/http_hello.wasm
curl localhost:8080/hello/world
# Hello from WASM! You requested /hello/world
```

`/<name>/...` is routed to the function named `<name>`. Functions use the standard [`wasi:http`](https://github.com/WebAssembly/wasi-http) interface, so components built for other WASI runtimes work too.

edgebox can also run CLI-style components:

```sh
cargo run -p edgebox -- run target/wasm32-wasip2/release/hello.wasm foo bar
```

## How it works

1. A function is a [WASI](https://wasi.dev) component (`wasm32-wasip2`) that exports `wasi:http/incoming-handler`.
2. edgebox compiles it once with [wasmtime](https://wasmtime.dev), then runs every request in a fresh, isolated instance.
3. Guests have no file, socket or outbound HTTP access unless granted.

## Project layout

| Path | Purpose |
| --- | --- |
| `crates/runtime` | Loads and runs components (wasmtime) |
| `crates/server` | HTTP router (axum): `/<name>` → function |
| `crates/cli` | The `edgebox` command |
| `examples/` | Example functions |

## Development

```sh
cargo guests && cargo test --workspace
```

## Roadmap

- [x] Run a `.wasm` component from the CLI
- [x] HTTP in → WASM → HTTP out
- [ ] Upload & deploy functions
- [ ] Sandbox limits (CPU time, memory)
- [ ] Sub-millisecond cold starts
- [ ] KV store, secrets, outbound fetch
- [ ] Metrics, logs, and JavaScript functions

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
