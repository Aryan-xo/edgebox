# edgebox

[![CI](https://github.com/Aryan-xo/edgebox/actions/workflows/ci.yml/badge.svg)](https://github.com/Aryan-xo/edgebox/actions/workflows/ci.yml)

A tiny serverless runtime for WebAssembly, written in Rust. Think a mini Cloudflare Workers: upload a function, and edgebox runs it in a sandbox.

> **Status:** early. edgebox can run WASM components from the CLI today. HTTP functions, deploys and sandbox limits are next (see [Roadmap](#roadmap)).

## Quickstart

```sh
rustup target add wasm32-wasip2

cargo guests   # build the example functions to .wasm
cargo run -p edgebox -- run target/wasm32-wasip2/release/hello.wasm
# Hello from WASM!
```

Arguments after the file are passed to the function:

```sh
cargo run -p edgebox -- run target/wasm32-wasip2/release/hello.wasm foo bar
```

## How it works

1. A function is any program compiled to a [WASI](https://wasi.dev) component (`wasm32-wasip2`).
2. edgebox loads it with [wasmtime](https://wasmtime.dev) and runs it in a sandbox: no file or network access unless granted.
3. The function's output and exit code are returned to the caller.

## Project layout

| Path | Purpose |
| --- | --- |
| `crates/runtime` | Library that loads and runs components |
| `crates/cli` | The `edgebox` command |
| `examples/hello` | Example function |

## Development

```sh
cargo guests && cargo test --workspace
```

## Roadmap

- [x] Run a `.wasm` component from the CLI
- [ ] HTTP in → WASM → HTTP out
- [ ] Upload & deploy functions
- [ ] Sandbox limits (CPU time, memory)
- [ ] Sub-millisecond cold starts
- [ ] KV store, secrets, outbound fetch
- [ ] Metrics, logs, and JavaScript functions

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
