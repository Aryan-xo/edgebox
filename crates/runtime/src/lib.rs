//! Runs WebAssembly components inside a WASI sandbox.

use std::path::Path;

use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::error::Context;
use wasmtime::{Engine, Result, Store};
use wasmtime_wasi::p2::bindings::sync::Command;
use wasmtime_wasi::p2::pipe::MemoryOutputPipe;
use wasmtime_wasi::{I32Exit, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

/// Maximum stdout bytes kept by [`Runtime::run_captured`].
const CAPTURE_LIMIT: usize = 1024 * 1024;

/// Per-instance host state: the guest's WASI context and its resources.
struct State {
    wasi: WasiCtx,
    table: ResourceTable,
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// Compiles and runs `wasi:cli/command` components.
///
/// Create one and reuse it: the engine and linker are shared across runs.
pub struct Runtime {
    engine: Engine,
    linker: Linker<State>,
}

impl Runtime {
    pub fn new() -> Result<Self> {
        let engine = Engine::default();
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
        Ok(Self { engine, linker })
    }

    /// Runs the component using the host's stdin/stdout/stderr.
    ///
    /// Returns the guest's exit code.
    pub fn run(&self, wasm: &Path, args: &[String]) -> Result<i32> {
        let mut wasi = WasiCtxBuilder::new();
        wasi.inherit_stdio();
        self.run_with(wasm, args, wasi)
    }

    /// Runs the component and returns its exit code and stdout.
    pub fn run_captured(&self, wasm: &Path, args: &[String]) -> Result<(i32, Vec<u8>)> {
        let stdout = MemoryOutputPipe::new(CAPTURE_LIMIT);
        let mut wasi = WasiCtxBuilder::new();
        wasi.stdout(stdout.clone()).inherit_stderr();
        let code = self.run_with(wasm, args, wasi)?;
        Ok((code, stdout.contents().to_vec()))
    }

    fn run_with(&self, wasm: &Path, args: &[String], mut wasi: WasiCtxBuilder) -> Result<i32> {
        let component = Component::from_file(&self.engine, wasm)
            .with_context(|| format!("failed to load component {}", wasm.display()))?;

        let program = wasm
            .file_name()
            .map_or("guest".into(), |name| name.to_string_lossy());
        wasi.arg(program).args(args);

        let state = State {
            wasi: wasi.build(),
            table: ResourceTable::new(),
        };
        let mut store = Store::new(&self.engine, state);

        let command = Command::instantiate(&mut store, &component, &self.linker)
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
