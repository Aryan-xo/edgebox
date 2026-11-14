//! Runs WebAssembly components inside a WASI sandbox.
//!
//! Guests get no filesystem, socket or outbound HTTP access unless granted.

mod http;
mod kv;
mod limits;
mod logs;

use std::sync::Arc;

use std::path::Path;

use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::error::Context;
use wasmtime::{
    Config, Engine, InstanceAllocationStrategy, PoolingAllocationConfig, Result, Store, StoreLimits,
};
use wasmtime_wasi::p2::bindings::sync::Command;
use wasmtime_wasi::p2::pipe::MemoryOutputPipe;
use wasmtime_wasi::{I32Exit, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use wasmtime_wasi_http::p2::bindings::ProxyPre;
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpCtxView, WasiHttpView};

use http::OutboundPolicy;
pub use http::{HttpFunction, ResponseBody, Settings};
pub use kv::{Kv, KvBackend, MAX_KEY_BYTES, MAX_VALUE_BYTES};
pub use limits::{Limits, Timeout, is_timeout};
pub use logs::{LogSink, LogStream};

/// Maximum stdout bytes kept by [`Runtime::run_captured`].
const CAPTURE_LIMIT: usize = 1024 * 1024;

/// Per-instance host state: the guest's WASI context and its resources.
struct State {
    wasi: WasiCtx,
    http: WasiHttpCtx,
    hooks: OutboundPolicy,
    table: ResourceTable,
    limits: StoreLimits,
    kv: Option<Kv>,
}

impl State {
    /// A store for one instance, with limits and settings applied.
    fn new_store(
        engine: &Engine,
        wasi: WasiCtx,
        limits: &Limits,
        settings: Arc<Settings>,
    ) -> Store<State> {
        let state = Self {
            wasi,
            http: WasiHttpCtx::new(),
            kv: settings.kv.clone(),
            hooks: OutboundPolicy(settings),
            table: ResourceTable::new(),
            limits: limits.store_limits(),
        };
        let mut store = Store::new(engine, state);
        store.limiter(|state| &mut state.limits);
        store.set_epoch_deadline(limits.epoch_ticks());
        store
    }
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl WasiHttpView for State {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        WasiHttpCtxView {
            ctx: &mut self.http,
            table: &mut self.table,
            hooks: &mut self.hooks,
        }
    }
}

/// Compiles and runs components.
///
/// Create one and reuse it: the engine and linkers are shared across runs.
pub struct Runtime {
    engine: Engine,
    command_linker: Linker<State>,
    http_linker: Linker<State>,
    limits: Limits,
}

impl Runtime {
    /// A runtime with [`Limits::default`].
    pub fn new() -> Result<Self> {
        Self::with_limits(Limits::default())
    }

    pub fn with_limits(limits: Limits) -> Result<Self> {
        let mut config = Config::new();
        config.epoch_interruption(true);

        // Pre-reserve memory slots once and reuse them, instead of mmap-ing
        // fresh memory for every instance.
        let mut pool = PoolingAllocationConfig::default();
        pool.max_memory_size(limits.max_memory_bytes);
        config.allocation_strategy(InstanceAllocationStrategy::Pooling(pool));

        let engine = Engine::new(&config)?;
        limits::start_epoch_ticker(&engine);

        let mut command_linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut command_linker)?;

        let mut http_linker = Linker::new(&engine);
        // Full WASI (Rust's std imports e.g. wasi:cli/environment) plus wasi:http.
        // Still sandboxed: no preopened dirs, and sockets are denied by default.
        wasmtime_wasi::p2::add_to_linker_async(&mut http_linker)?;
        wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut http_linker)?;
        kv::add_to_linker::<State, HasSelf<State>>(&mut http_linker, |state| state)?;

        Ok(Self {
            engine,
            command_linker,
            http_linker,
            limits,
        })
    }

    /// Runs a `wasi:cli/command` component using the host's stdio.
    ///
    /// Returns the guest's exit code.
    pub fn run(&self, wasm: &Path, args: &[String]) -> Result<i32> {
        let mut wasi = WasiCtxBuilder::new();
        wasi.inherit_stdio();
        self.run_with(wasm, args, wasi)
    }

    /// Runs a `wasi:cli/command` component and returns its exit code and stdout.
    pub fn run_captured(&self, wasm: &Path, args: &[String]) -> Result<(i32, Vec<u8>)> {
        let stdout = MemoryOutputPipe::new(CAPTURE_LIMIT);
        let mut wasi = WasiCtxBuilder::new();
        wasi.stdout(stdout.clone()).inherit_stderr();
        let code = self.run_with(wasm, args, wasi)?;
        Ok((code, stdout.contents().to_vec()))
    }

    /// Compiles a `wasi:http/proxy` component file into a function that serves requests.
    pub fn load_http(&self, wasm: &Path) -> Result<HttpFunction> {
        let component = self.load_component(wasm)?;
        self.prepare_http(&component)
            .with_context(|| format!("failed to prepare {}", wasm.display()))
    }

    /// Like [`Runtime::load_http`], from the component's bytes.
    pub fn load_http_bytes(&self, wasm: &[u8]) -> Result<HttpFunction> {
        let component =
            Component::new(&self.engine, wasm).context("not a valid WebAssembly component")?;
        self.prepare_http(&component)
    }

    /// Compiles a component to native code, for caching.
    ///
    /// Load the result with [`Runtime::load_http_precompiled`].
    pub fn precompile(&self, wasm: &[u8]) -> Result<Vec<u8>> {
        self.engine
            .precompile_component(wasm)
            .context("not a valid WebAssembly component")
    }

    /// Loads an HTTP function from [`Runtime::precompile`] output, skipping compilation.
    ///
    /// Artifacts from a different wasmtime version or engine config are
    /// rejected with an error; callers should then recompile from the `.wasm`.
    ///
    /// # Safety
    ///
    /// `compiled` must be bytes produced by [`Runtime::precompile`]. It is
    /// native code: wasmtime cannot fully validate it, so loading untrusted
    /// bytes can run arbitrary code.
    pub unsafe fn load_http_precompiled(&self, compiled: &[u8]) -> Result<HttpFunction> {
        // SAFETY: the caller guarantees `compiled` came from `precompile`.
        let component = unsafe { Component::deserialize(&self.engine, compiled) }
            .context("failed to load precompiled component")?;
        self.prepare_http(&component)
    }

    fn prepare_http(&self, component: &Component) -> Result<HttpFunction> {
        let pre = self
            .http_linker
            .instantiate_pre(component)
            .and_then(ProxyPre::new)
            .context("not an HTTP function (expected the wasi:http/proxy world)")?;
        Ok(HttpFunction::new(pre, self.limits))
    }

    fn load_component(&self, wasm: &Path) -> Result<Component> {
        Component::from_file(&self.engine, wasm)
            .with_context(|| format!("failed to load component {}", wasm.display()))
    }

    fn run_with(&self, wasm: &Path, args: &[String], mut wasi: WasiCtxBuilder) -> Result<i32> {
        let component = self.load_component(wasm)?;

        let program = wasm
            .file_name()
            .map_or("guest".into(), |name| name.to_string_lossy());
        wasi.arg(program).args(args);

        let mut store = State::new_store(&self.engine, wasi.build(), &self.limits, Arc::default());

        let command = Command::instantiate(&mut store, &component, &self.command_linker)
            .context("failed to instantiate component")?;

        match command.wasi_cli_run().call_run(&mut store) {
            Ok(Ok(())) => Ok(0),
            Ok(Err(())) => Ok(1),
            // `std::process::exit(n)` in the guest surfaces as an I32Exit trap.
            Err(err) => match err.downcast_ref::<I32Exit>() {
                Some(exit) => Ok(exit.0),
                None if limits::is_interrupt(&err) => {
                    Err(wasmtime::Error::new(Timeout(self.limits.timeout)))
                }
                None => Err(err.context("guest trapped")),
            },
        }
    }
}
