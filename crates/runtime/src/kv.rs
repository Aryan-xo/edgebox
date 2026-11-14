//! The `edgebox:kv/store` interface (see `wit/kv.wit`), implemented by the host.

use std::sync::Arc;

use crate::State;

wasmtime::component::bindgen!({
    path: "../../wit",
    world: "imports",
});

pub(crate) use edgebox::kv::store::add_to_linker;

/// Largest key a guest may store, in bytes.
pub const MAX_KEY_BYTES: usize = 512;
/// Largest value a guest may store, in bytes.
pub const MAX_VALUE_BYTES: usize = 1024 * 1024;

/// Storage behind the KV interface. `namespace` isolates functions from each other.
pub trait KvBackend: Send + Sync + 'static {
    fn get(&self, namespace: &str, key: &str) -> Result<Option<Vec<u8>>, String>;
    fn set(&self, namespace: &str, key: &str, value: &[u8]) -> Result<(), String>;
    fn delete(&self, namespace: &str, key: &str) -> Result<bool, String>;
}

/// A function's view of the KV store: a backend plus its namespace.
#[derive(Clone)]
pub struct Kv {
    backend: Arc<dyn KvBackend>,
    namespace: String,
}

impl Kv {
    pub fn new(backend: Arc<dyn KvBackend>, namespace: impl Into<String>) -> Self {
        Self {
            backend,
            namespace: namespace.into(),
        }
    }
}

impl State {
    fn kv(&self) -> Result<&Kv, String> {
        self.kv
            .as_ref()
            .ok_or_else(|| "the KV store is not available to this function".to_string())
    }
}

impl edgebox::kv::store::Host for State {
    fn get(&mut self, key: String) -> Result<Option<Vec<u8>>, String> {
        let kv = self.kv()?;
        kv.backend.get(&kv.namespace, &key)
    }

    fn set(&mut self, key: String, value: Vec<u8>) -> Result<(), String> {
        if key.len() > MAX_KEY_BYTES {
            return Err(format!("key is longer than {MAX_KEY_BYTES} bytes"));
        }
        if value.len() > MAX_VALUE_BYTES {
            return Err(format!("value is larger than {MAX_VALUE_BYTES} bytes"));
        }
        let kv = self.kv()?;
        kv.backend.set(&kv.namespace, &key, &value)
    }

    fn delete(&mut self, key: String) -> Result<bool, String> {
        let kv = self.kv()?;
        kv.backend.delete(&kv.namespace, &key)
    }
}
