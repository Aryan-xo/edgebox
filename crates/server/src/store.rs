//! Durable storage for deployed functions (SQLite). Every deploy is a new version.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use edgebox_runtime::KvBackend;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// Metadata about one deployed version of a function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FunctionInfo {
    pub name: String,
    pub version: i64,
    /// Size of the `.wasm` in bytes.
    pub size: i64,
    /// Unix timestamp, seconds.
    pub deployed_at: i64,
}

/// Per-function settings that survive redeploys.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FunctionConfig {
    /// Environment variables, e.g. API keys. Never returned by the API.
    pub env: BTreeMap<String, String>,
    /// Hosts the function may call over HTTP (`*.example.com` for subdomains).
    pub allowed_hosts: Vec<String>,
}

impl FunctionConfig {
    pub fn validate(&self) -> Result<(), String> {
        for key in self.env.keys() {
            if key.is_empty() || key.contains(['=', '\0']) {
                return Err(format!("invalid environment variable name {key:?}"));
            }
        }
        for host in &self.allowed_hosts {
            let name = host.strip_prefix("*.").unwrap_or(host);
            let valid = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
            if !valid {
                return Err(format!(
                    "invalid host {host:?}: use a hostname like api.example.com"
                ));
            }
        }
        Ok(())
    }
}

/// The latest stored version of a function.
pub struct Stored {
    pub version: i64,
    pub wasm: Vec<u8>,
    /// Native code from `Runtime::precompile`, if cached.
    pub compiled: Option<Vec<u8>>,
}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open database {}", path.display()))?;
        Self::init(conn)
    }

    /// A throwaway store, for tests and `--data :memory:`.
    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS functions (
                 name        TEXT    NOT NULL,
                 version     INTEGER NOT NULL,
                 wasm        BLOB    NOT NULL,
                 deployed_at INTEGER NOT NULL,
                 compiled    BLOB,
                 PRIMARY KEY (name, version)
             );
             CREATE TABLE IF NOT EXISTS function_config (
                 name   TEXT PRIMARY KEY,
                 config TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS kv (
                 namespace TEXT NOT NULL,
                 key       TEXT NOT NULL,
                 value     BLOB NOT NULL,
                 PRIMARY KEY (namespace, key)
             );",
        )?;
        // Databases created before the compiled-code cache lack the column.
        let has_compiled: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('functions') WHERE name = 'compiled'",
            [],
            |row| row.get(0),
        )?;
        if !has_compiled {
            conn.execute("ALTER TABLE functions ADD COLUMN compiled BLOB", [])?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Stores `wasm` (and its compiled form) as the next version of `name`.
    ///
    /// If it is identical to the latest version, nothing is stored and the
    /// latest version is returned.
    pub fn save(&self, name: &str, wasm: &[u8], compiled: &[u8]) -> Result<FunctionInfo> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let latest = tx
            .query_row(
                "SELECT version, wasm = ?2, deployed_at FROM functions
                 WHERE name = ?1 ORDER BY version DESC LIMIT 1",
                params![name, wasm],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((version, true, deployed_at)) = latest {
            return Ok(FunctionInfo {
                name: name.to_string(),
                version,
                size: wasm.len() as i64,
                deployed_at,
            });
        }
        let version: i64 = tx.query_row(
            "SELECT COALESCE(MAX(version), 0) + 1 FROM functions WHERE name = ?1",
            [name],
            |row| row.get(0),
        )?;
        let deployed_at = now();
        tx.execute(
            "INSERT INTO functions (name, version, wasm, deployed_at, compiled)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![name, version, wasm, deployed_at, compiled],
        )?;
        tx.commit()?;
        Ok(FunctionInfo {
            name: name.to_string(),
            version,
            size: wasm.len() as i64,
            deployed_at,
        })
    }

    /// Latest version of every function, by name.
    pub fn list(&self) -> Result<Vec<FunctionInfo>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!("{LATEST} ORDER BY name"))?;
        let rows = stmt.query_map([], |row| {
            Ok(FunctionInfo {
                name: row.get(0)?,
                version: row.get(1)?,
                size: row.get(2)?,
                deployed_at: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The latest version of `name`.
    pub fn latest(&self, name: &str) -> Result<Option<Stored>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT version, wasm, compiled FROM functions
                 WHERE name = ?1 ORDER BY version DESC LIMIT 1",
                [name],
                |row| {
                    Ok(Stored {
                        version: row.get(0)?,
                        wasm: row.get(1)?,
                        compiled: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// Replaces the cached compiled code of one version.
    pub fn set_compiled(&self, name: &str, version: i64, compiled: &[u8]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE functions SET compiled = ?3 WHERE name = ?1 AND version = ?2",
            params![name, version, compiled],
        )?;
        Ok(())
    }

    /// Deletes every version of `name`, its config and its KV data.
    /// Returns whether it existed.
    pub fn delete(&self, name: &str) -> Result<bool> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let existed = tx.execute("DELETE FROM functions WHERE name = ?1", [name])? > 0;
        tx.execute("DELETE FROM function_config WHERE name = ?1", [name])?;
        tx.execute("DELETE FROM kv WHERE namespace = ?1", [name])?;
        tx.commit()?;
        Ok(existed)
    }

    /// The config of `name`, or the default if none was set.
    pub fn config(&self, name: &str) -> Result<FunctionConfig> {
        let conn = self.conn.lock().unwrap();
        let json: Option<String> = conn
            .query_row(
                "SELECT config FROM function_config WHERE name = ?1",
                [name],
                |row| row.get(0),
            )
            .optional()?;
        Ok(match json {
            Some(json) => serde_json::from_str(&json)?,
            None => FunctionConfig::default(),
        })
    }

    pub fn set_config(&self, name: &str, config: &FunctionConfig) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO function_config (name, config) VALUES (?1, ?2)
             ON CONFLICT (name) DO UPDATE SET config = excluded.config",
            params![name, serde_json::to_string(config)?],
        )?;
        Ok(())
    }
}

impl KvBackend for Store {
    fn get(&self, namespace: &str, key: &str) -> Result<Option<Vec<u8>>, String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT value FROM kv WHERE namespace = ?1 AND key = ?2",
            [namespace, key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|err| err.to_string())
    }

    fn set(&self, namespace: &str, key: &str, value: &[u8]) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO kv (namespace, key, value) VALUES (?1, ?2, ?3)
             ON CONFLICT (namespace, key) DO UPDATE SET value = excluded.value",
            params![namespace, key, value],
        )
        .map(drop)
        .map_err(|err| err.to_string())
    }

    fn delete(&self, namespace: &str, key: &str) -> Result<bool, String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM kv WHERE namespace = ?1 AND key = ?2",
            [namespace, key],
        )
        .map(|rows| rows > 0)
        .map_err(|err| err.to_string())
    }
}

const LATEST: &str = "SELECT name, version, length(wasm), deployed_at FROM functions f
     WHERE version = (SELECT MAX(version) FROM functions WHERE name = f.name)";

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_increment_per_function() {
        let store = Store::in_memory().unwrap();
        assert_eq!(store.save("a", b"1", b"").unwrap().version, 1);
        assert_eq!(store.save("a", b"22", b"").unwrap().version, 2);
        assert_eq!(store.save("b", b"1", b"").unwrap().version, 1);

        let list = store.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(
            (list[0].name.as_str(), list[0].version, list[0].size),
            ("a", 2, 2)
        );
        assert_eq!(store.latest("a").unwrap().unwrap().wasm, b"22");
    }

    #[test]
    fn migrates_databases_without_compiled_column() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE functions (name TEXT, version INTEGER, wasm BLOB, deployed_at INTEGER,
                                     PRIMARY KEY (name, version));
             INSERT INTO functions VALUES ('a', 1, x'00', 0);",
        )
        .unwrap();
        let store = Store::init(conn).unwrap();
        assert_eq!(store.latest("a").unwrap().unwrap().compiled, None);
        store.set_compiled("a", 1, b"native").unwrap();
        assert_eq!(
            store.latest("a").unwrap().unwrap().compiled.unwrap(),
            b"native"
        );
    }

    #[test]
    fn kv_namespaces_are_isolated() {
        let store = Store::in_memory().unwrap();
        KvBackend::set(&store, "a", "k", b"1").unwrap();
        KvBackend::set(&store, "a", "k", b"2").unwrap();
        assert_eq!(KvBackend::get(&store, "a", "k").unwrap().unwrap(), b"2");
        assert_eq!(KvBackend::get(&store, "b", "k").unwrap(), None);
        assert!(KvBackend::delete(&store, "a", "k").unwrap());
        assert!(!KvBackend::delete(&store, "a", "k").unwrap());
    }

    #[test]
    fn config_roundtrips_and_is_deleted_with_function() {
        let store = Store::in_memory().unwrap();
        assert_eq!(store.config("a").unwrap(), FunctionConfig::default());
        let config = FunctionConfig {
            env: [("KEY".into(), "v".into())].into(),
            allowed_hosts: vec!["api.example.com".into()],
        };
        store.save("a", b"1", b"").unwrap();
        store.set_config("a", &config).unwrap();
        KvBackend::set(&store, "a", "k", b"v").unwrap();
        assert_eq!(store.config("a").unwrap(), config);

        store.delete("a").unwrap();
        assert_eq!(store.config("a").unwrap(), FunctionConfig::default());
        assert_eq!(KvBackend::get(&store, "a", "k").unwrap(), None);
    }

    #[test]
    fn config_validation() {
        let bad_env = FunctionConfig {
            env: [("A=B".into(), "v".into())].into(),
            ..FunctionConfig::default()
        };
        assert!(bad_env.validate().is_err());
        let bad_host = FunctionConfig {
            allowed_hosts: vec!["http://x.com/path".into()],
            ..FunctionConfig::default()
        };
        assert!(bad_host.validate().is_err());
        let ok = FunctionConfig {
            allowed_hosts: vec!["*.example.com".into(), "127.0.0.1".into()],
            ..FunctionConfig::default()
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn identical_redeploy_keeps_version() {
        let store = Store::in_memory().unwrap();
        assert_eq!(store.save("a", b"same", b"").unwrap().version, 1);
        assert_eq!(store.save("a", b"same", b"").unwrap().version, 1);
        assert_eq!(store.save("a", b"new", b"").unwrap().version, 2);
    }

    #[test]
    fn delete_removes_all_versions() {
        let store = Store::in_memory().unwrap();
        store.save("a", b"1", b"").unwrap();
        store.save("a", b"2", b"").unwrap();
        assert!(store.delete("a").unwrap());
        assert!(!store.delete("a").unwrap());
        assert!(store.list().unwrap().is_empty());
    }
}
