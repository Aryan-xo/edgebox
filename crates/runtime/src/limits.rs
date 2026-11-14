//! Per-request resource limits: wall-clock time and memory.

use std::fmt;
use std::time::Duration;

use wasmtime::{Engine, StoreLimits, StoreLimitsBuilder, Trap};

/// How often the engine's epoch advances. This is the precision of timeouts.
const EPOCH_TICK: Duration = Duration::from_millis(5);

/// Resource limits applied to every instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Max time a request may take, including time spent running wasm.
    pub timeout: Duration,
    /// Max linear memory per instance, in bytes.
    pub max_memory_bytes: usize,
}

impl Default for Limits {
    /// Mirrors common serverless defaults: 10 s per request, 128 MiB of memory.
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            max_memory_bytes: 128 * 1024 * 1024,
        }
    }
}

impl Limits {
    pub(crate) fn store_limits(&self) -> StoreLimits {
        StoreLimitsBuilder::new()
            .memory_size(self.max_memory_bytes)
            // Trap instead of returning -1 from memory.grow: a clearer failure.
            .trap_on_grow_failure(true)
            .build()
    }

    /// The timeout in epoch ticks, rounded up.
    pub(crate) fn epoch_ticks(&self) -> u64 {
        let ticks = self.timeout.as_nanos().div_ceil(EPOCH_TICK.as_nanos());
        u64::try_from(ticks).unwrap_or(u64::MAX).max(1)
    }
}

/// Advances `engine`'s epoch every tick until the engine is dropped.
///
/// Stores trap once their epoch deadline passes, which interrupts even a
/// guest stuck in a tight loop that never yields to the async runtime.
pub(crate) fn start_epoch_ticker(engine: &Engine) {
    let weak = engine.weak();
    std::thread::Builder::new()
        .name("edgebox-epoch".into())
        .spawn(move || {
            while let Some(engine) = weak.upgrade() {
                engine.increment_epoch();
                drop(engine);
                std::thread::sleep(EPOCH_TICK);
            }
        })
        .expect("failed to spawn epoch thread");
}

/// A function ran longer than [`Limits::timeout`].
#[derive(Debug)]
pub struct Timeout(pub Duration);

impl fmt::Display for Timeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "function exceeded its {}ms time limit",
            self.0.as_millis()
        )
    }
}

impl std::error::Error for Timeout {}

/// Whether `err` means the function ran out of time.
pub fn is_timeout(err: &wasmtime::Error) -> bool {
    err.downcast_ref::<Timeout>().is_some() || is_interrupt(err)
}

pub(crate) fn is_interrupt(err: &wasmtime::Error) -> bool {
    err.downcast_ref::<Trap>() == Some(&Trap::Interrupt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_ticks_round_up() {
        let limits = |ms| Limits {
            timeout: Duration::from_millis(ms),
            ..Limits::default()
        };
        assert_eq!(limits(50).epoch_ticks(), 10);
        assert_eq!(limits(51).epoch_ticks(), 11);
        assert_eq!(limits(0).epoch_ticks(), 1);
    }
}
