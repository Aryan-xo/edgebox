# A fresh WebAssembly sandbox for every request, in 27 µs

*How edgebox, a small serverless runtime in Rust, isolates every HTTP request and still serves ~70k requests per second.*

Most serverless platforms keep "warm" instances around and reuse them across requests, because starting an isolated runtime is expensive. That reuse is a trade-off: state can leak from one request to the next, and one bad request can poison an instance for everyone after it.

edgebox takes the strict route. **Every request gets a brand-new WebAssembly instance**, which is thrown away afterwards. This post covers the four things that made that fast and safe enough to be practical:

1. Doing the expensive work once per deploy, not once per request
2. Reusing memory instead of mapping it fresh
3. Interrupting code that never yields
4. Caching native code across restarts

All numbers are from an Apple M4 Pro, release build. The method is in [BENCHMARKS.md](../../BENCHMARKS.md).

## The setup

A function is a [WASI component](https://component-model.bytecodealliance.org/) that exports the standard `wasi:http/incoming-handler` interface, so it can be written in Rust, JavaScript or any language with a component toolchain. edgebox embeds [wasmtime](https://wasmtime.dev) to run it.

The naive request path looks like this:

```text
read .wasm → compile to native code → link imports → instantiate → handle request
```

Compiling the 130 KB example function takes **8.7 ms**. Doing that per request would cap a core at roughly 100 requests per second.

## 1. Compile and link once per deploy

Compilation obviously belongs at deploy time. The less obvious step is **linking**: resolving every import the component needs (WASI clocks, streams, HTTP types, edgebox's KV store) to host functions. wasmtime can do that ahead of time too, producing an `InstancePre`, which is a template that only needs memory to become a live instance.

With both done at deploy time, each request only pays for instantiation and the handler itself: **31.5 µs**. That's already sub-millisecond, but under load something else got in the way.

## 2. Pool memory instead of mapping it

Load testing with `ab -k -c 50` gave about **24k req/s**, with a p99 of 4 ms. That's suspiciously low for a 31 µs operation on a 12-core machine.

By default, wasmtime maps fresh virtual memory for every instance and unmaps it afterwards. Each instance reserves a large region (with guard pages), so every request costs `mmap`/`munmap` system calls, and those contend on the process's memory map when many threads do it at once.

wasmtime's **pooling allocator** reserves a fixed pool of memory slots up front and recycles them. Switching to it was one config change:

```rust
let mut pool = PoolingAllocationConfig::default();
pool.max_memory_size(limits.max_memory_bytes);
config.allocation_strategy(InstanceAllocationStrategy::Pooling(pool));
```

Results, same build, only the allocator changed:

| Allocator | Requests/sec | p99 |
| --- | --- | --- |
| On-demand (default) | 23.7k – 26.5k | 4 ms |
| Pooling | **66.6k – 77.9k** | **2–3 ms** |

That's **2.8× the throughput**, and single-request latency also dropped 13% to **26.8 µs**. The pool doubles as a memory limit: no instance can grow beyond its slot.

## 3. Stop code that never yields

Isolation is only useful if a bad function can't take the server down. Memory is capped by the pool slot and a `StoreLimits` limiter. Time is harder.

edgebox runs functions on Tokio, so the obvious approach is `tokio::time::timeout`. That works for a function waiting on I/O, but not for this:

```rust
loop {}
```

A tight loop never returns to the async runtime, so the timeout future never gets polled. The fix is wasmtime's **epoch interruption**. A background thread bumps a global "epoch" counter every 5 ms, and the compiled code checks it at loop back-edges and function entries. Each request's store gets a deadline in epochs; when it passes, the guest traps.

```rust
store.set_epoch_deadline(limits.epoch_ticks()); // e.g. 50 ms / 5 ms = 10 ticks
```

edgebox uses both: epochs for running code, and a Tokio timeout for code that's blocked (for example, sleeping). With a 50 ms limit, an infinite loop returns **504 in ~52 ms**, and the server's CPU usage goes straight back to idle, so the loop is really stopped, not just abandoned.

## 4. Cache native code across restarts

Restarting the server meant recompiling every function: 8.7 ms each for a small Rust function, and **0.8 s** for a 13 MB JavaScript function (which carries a whole JS engine).

wasmtime can serialize compiled code. On deploy, edgebox stores it in SQLite next to the `.wasm`, and loads it on startup with `Component::deserialize`: **88 µs instead of 8.7 ms, about 100× faster**.

Two details matter:

- Deserializing native code is `unsafe`, because wasmtime can't fully validate machine code. edgebox only loads bytes that it produced itself, and documents that the database is trusted.
- Artifacts are tied to the wasmtime version and engine config. If loading fails (say, after an upgrade), edgebox recompiles from the stored `.wasm` and refreshes the cache. A test corrupts the cache on purpose to check this.

## Gotchas along the way

- **HTTP/1.0 keep-alive.** `ab` speaks HTTP/1.0, which can't use chunked transfer encoding. The example function streamed its body without a `Content-Length`, so the server had to close the connection after every response, which looked like random connection resets. Setting `Content-Length` fixed it.
- **Rust's std imports more than you'd think.** The minimal `wasi:http/proxy` interface set wasn't enough for a Rust function, because std pulls in `wasi:cli/environment` even if you never read an env var. edgebox links all of WASI and keeps it sandboxed by configuration instead: no preopened directories, and sockets denied.
- **Outbound HTTP is on by default in wasmtime-wasi-http.** The default hooks let guests make real network requests. edgebox replaces them with a per-function allowlist and denies everything else.

## The result

| | |
| --- | --- |
| Cold start per request | **26.8 µs** |
| Throughput | **~70k req/s** |
| Infinite loop stopped (50 ms limit) | **~52 ms** |
| Function load on restart | **88 µs** |

Every request runs in its own sandbox with its own memory, a hard deadline, and only the capabilities it was granted. And it's still fast enough that warm instances aren't needed.

The code is at [github.com/Aryan-xo/edgebox](https://github.com/Aryan-xo/edgebox).
