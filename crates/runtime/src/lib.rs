//! Runs WebAssembly components inside a WASI sandbox.
//!
//! Guests get no filesystem, socket or outbound HTTP access unless granted.

mod http;

use std::path::Path;

use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::error::Context;
use wasmtime::{Engine, Result, Store};
use wasmtime_wasi::p2::bindings::sync::Command;
use wasmtime_wasi::p2::pipe::MemoryOutputPipe;
use wasmtime_wasi::{I32Exit, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use wasmtime_wasi_http::p2::bindings::ProxyPre;
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpCtxView, WasiHttpView};

use http::DenyOutbound;
pub use http::{HttpFunction, ResponseBody};

/// Maximum stdout bytes kept by [`Runtime::run_captured`].
const CAPTURE_LIMIT: usize = 1024 * 1024;

/// Per-instance host state: the guest's WASI context and its resources.
struct State {
    wasi: WasiCtx,
    http: WasiHttpCtx,
    hooks: DenyOutbound,
    table: ResourceTable,
}

impl State {
    fn new(wasi: WasiCtx) -> Self {
        Self {
            wasi,
            http: WasiHttpCtx::new(),
            hooks: DenyOutbound,
            table: ResourceTable::new(),
        }
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
}

impl Runtime {
    pub fn new() -> Result<Self> {
        let engine = Engine::default();

        let mut command_linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut command_linker)?;

        let mut http_linker = Linker::new(&engine);
        // Full WASI (Rust's std imports e.g. wasi:cli/environment) plus wasi:http.
        // Still sandboxed: no preopened dirs, and sockets are denied by default.
        wasmtime_wasi::p2::add_to_linker_async(&mut http_linker)?;
        wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut http_linker)?;

        Ok(Self {
            engine,
            command_linker,
            http_linker,
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

    /// Compiles a `wasi:http/proxy` component into a function that serves requests.
    pub fn load_http(&self, wasm: &Path) -> Result<HttpFunction> {
        let component = self.load_component(wasm)?;
        let pre = self
            .http_linker
            .instantiate_pre(&component)
            .and_then(ProxyPre::new)
            .with_context(|| {
                format!(
                    "{} is not an HTTP function (expected the wasi:http/proxy world)",
                    wasm.display()
                )
            })?;
        Ok(HttpFunction::new(pre))
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

        let mut store = Store::new(&self.engine, State::new(wasi.build()));

        let command = Command::instantiate(&mut store, &component, &self.command_linker)
            .context("failed to instantiate component")?;

        match command.wasi_cli_run().call_run(&mut store) {
            Ok(Ok(())) => Ok(0),
            Ok(Err(())) => Ok(1),
            // `std::process::exit(n)` in the guest surfaces as an I32Exit trap.
            Err(err) => match err.downcast_ref::<I32Exit>() {
                Some(exit) => Ok(exit.0),
                None => Err(err.context("guest trapped")),
            },
        }
    }
}
