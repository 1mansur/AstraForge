use crate::error::{AppError, Result};
use fs2::FileExt;
use rusqlite::Connection;
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
pub const SCHEMA: &str = "CREATE TABLE repositories(id TEXT PRIMARY KEY,root TEXT NOT NULL UNIQUE,name TEXT NOT NULL,opened_at INTEGER NOT NULL); CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE patch_sets(id TEXT PRIMARY KEY,repository_id TEXT NOT NULL REFERENCES repositories(id),status TEXT NOT NULL,source TEXT NOT NULL,created_at INTEGER NOT NULL,changes TEXT NOT NULL); CREATE INDEX patch_repository ON patch_sets(repository_id,created_at DESC); CREATE TABLE patch_journal(patch_id TEXT PRIMARY KEY REFERENCES patch_sets(id),repository_id TEXT NOT NULL REFERENCES repositories(id),prior_status TEXT NOT NULL,operation TEXT NOT NULL,items TEXT NOT NULL,started_at INTEGER NOT NULL); CREATE INDEX journal_repository ON patch_journal(repository_id);";
pub struct Database {
    connection: Mutex<Connection>,
    _instance_lock: Option<File>,
}
impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if path != Path::new(":memory:") {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent)?;
            }
        }
        let instance_lock = if path == Path::new(":memory:") {
            None
        } else {
            let mut lock_path = path.as_os_str().to_os_string();
            lock_path.push(".lock");
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(lock_path)?;
            if let Err(error) = file.try_lock_exclusive() {
                if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
                    return Err(AppError::new("DB_INSTANCE_LOCKED", "This AstraForge data directory is already open in another application instance"));
                }
                return Err(AppError::new(
                    "DB_LOCK_FAILED",
                    "The application could not lock its data directory",
                )
                .context(serde_json::json!({"cause": format!("{:?}", error.kind())})));
            }
            Some(file)
        };
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > 1 {
            return Err(AppError::new(
                "DB_NEWER_SCHEMA",
                "The database requires a newer AstraForge version",
            ));
        }
        connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA trusted_schema=OFF;")?;
        if version == 0 {
            let transaction = connection.transaction()?;
            transaction.execute_batch(SCHEMA)?;
            transaction.pragma_update(None, "user_version", 1)?;
            transaction.commit()?;
        }
        Ok(Self {
            connection: Mutex::new(connection),
            _instance_lock: instance_lock,
        })
    }
    pub fn with<T>(&self, operation: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut connection = self.connection.lock().map_err(|_| {
            AppError::new(
                "DB_LOCK_POISONED",
                "The database lock is unavailable; restart the application",
            )
        })?;
        operation(&mut connection)
    }
}
pub fn timestamp_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}
