//! Durable storage for deployed functions (SQLite). Every deploy is a new version.

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

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
                 PRIMARY KEY (name, version)
             );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Stores `wasm` as the next version of `name`.
    ///
    /// If it is identical to the latest version, nothing is stored and the
    /// latest version is returned.
    pub fn save(&self, name: &str, wasm: &[u8]) -> Result<FunctionInfo> {
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
            "INSERT INTO functions (name, version, wasm, deployed_at) VALUES (?1, ?2, ?3, ?4)",
            params![name, version, wasm, deployed_at],
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

    /// The `.wasm` of the latest version of `name`.
    pub fn latest_wasm(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT wasm FROM functions WHERE name = ?1 ORDER BY version DESC LIMIT 1",
                [name],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Deletes every version of `name`. Returns whether it existed.
    pub fn delete(&self, name: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute("DELETE FROM functions WHERE name = ?1", [name])? > 0)
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
        assert_eq!(store.save("a", b"1").unwrap().version, 1);
        assert_eq!(store.save("a", b"22").unwrap().version, 2);
        assert_eq!(store.save("b", b"1").unwrap().version, 1);

        let list = store.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(
            (list[0].name.as_str(), list[0].version, list[0].size),
            ("a", 2, 2)
        );
        assert_eq!(store.latest_wasm("a").unwrap().unwrap(), b"22");
    }

    #[test]
    fn identical_redeploy_keeps_version() {
        let store = Store::in_memory().unwrap();
        assert_eq!(store.save("a", b"same").unwrap().version, 1);
        assert_eq!(store.save("a", b"same").unwrap().version, 1);
        assert_eq!(store.save("a", b"new").unwrap().version, 2);
    }

    #[test]
    fn delete_removes_all_versions() {
        let store = Store::in_memory().unwrap();
        store.save("a", b"1").unwrap();
        store.save("a", b"2").unwrap();
        assert!(store.delete("a").unwrap());
        assert!(!store.delete("a").unwrap());
        assert!(store.list().unwrap().is_empty());
    }
}
