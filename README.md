# edgebox

[![CI](https://github.com/Aryan-xo/edgebox/actions/workflows/ci.yml/badge.svg)](https://github.com/Aryan-xo/edgebox/actions/workflows/ci.yml)

A tiny serverless runtime for WebAssembly, written in Rust. Think a mini Cloudflare Workers: give edgebox a function, and it serves HTTP requests with it in a sandbox.

> **Status:** early. Deploy and serve HTTP functions today; sandbox limits are next (see [Roadmap](#roadmap)).

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
| `edgebox run PATH [ARGS...]` | Run a CLI-style component locally |

### Admin API

Set `EDGEBOX_TOKEN` on the server and client to require `Authorization: Bearer <token>`.

| Method | Path | |
| --- | --- | --- |
| `PUT` | `/_api/functions/{name}` | Deploy (`.wasm` body). Returns `{name, version, size, deployed_at}` |
| `GET` | `/_api/functions` | List latest versions |
| `DELETE` | `/_api/functions/{name}` | Delete all versions |

## How it works

1. A function is a [WASI](https://wasi.dev) component (`wasm32-wasip2`) that exports `wasi:http/incoming-handler`.
2. On deploy, edgebox compiles it with [wasmtime](https://wasmtime.dev), stores it in SQLite as a new version, and swaps it in live.
3. Every request runs in a fresh, isolated instance.
4. Guests have no file, socket or outbound HTTP access unless granted.

## Project layout

| Path | Purpose |
| --- | --- |
| `crates/runtime` | Loads and runs components (wasmtime) |
| `crates/server` | HTTP router (axum), admin API, SQLite store |
| `crates/cli` | The `edgebox` command |
| `examples/` | Example functions |

## Development

```sh
cargo guests && cargo test --workspace
```

## Roadmap

- [x] Run a `.wasm` component from the CLI
- [x] HTTP in → WASM → HTTP out
- [x] Upload & deploy functions (versioned, hot-swapped)
- [ ] Sandbox limits (CPU time, memory)
- [ ] Sub-millisecond cold starts
- [ ] KV store, secrets, outbound fetch
- [ ] Metrics, logs, and JavaScript functions

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
