use crate::model::{BehaviorSampleV2, CompletedTurnSample};
use atomicwrites::{AllowOverwrite, AtomicFile};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

const LOG_ROTATE_BYTES: u64 = 5 * 1024 * 1024;
const LOG_BACKUPS: usize = 3;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyImportSummary {
    pub settings: usize,
    pub behavior_samples: usize,
    pub behavior_samples_v2: usize,
}

pub struct Persistence {
    root: PathBuf,
    connection: Mutex<Connection>,
}

impl Persistence {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, String> {
        let root = root.as_ref().to_path_buf();
        prepare_private_state_root(&root)?;
        let database_path = root.join("monitor.db");
        let connection = Connection::open(&database_path).map_err(|error| error.to_string())?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=NORMAL;
                 CREATE TABLE IF NOT EXISTS latest_snapshot (
                   id INTEGER PRIMARY KEY CHECK (id = 1),
                   checked_at TEXT NOT NULL,
                   json TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS collector_cache (
                   id INTEGER PRIMARY KEY CHECK (id = 1),
                   saved_at TEXT NOT NULL,
                   json TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS settings (
                   key TEXT PRIMARY KEY,
                   value TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS behavior_samples (
                   bucket TEXT NOT NULL,
                   observed_at TEXT NOT NULL,
                   json TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_behavior_bucket_time
                   ON behavior_samples(bucket, observed_at DESC);
                 CREATE UNIQUE INDEX IF NOT EXISTS idx_behavior_sample_unique
                   ON behavior_samples(bucket, observed_at, json);
                 CREATE TABLE IF NOT EXISTS behavior_samples_v2 (
                   bucket TEXT NOT NULL,
                   observed_at TEXT NOT NULL,
                   json TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_behavior_v2_bucket_time
                   ON behavior_samples_v2(bucket, observed_at DESC);
                 CREATE UNIQUE INDEX IF NOT EXISTS idx_behavior_v2_sample_unique
                   ON behavior_samples_v2(bucket, observed_at, json);",
            )
            .map_err(|error| error.to_string())?;
        secure_state_file(&database_path)?;
        secure_database_sidecars(&root)?;
        Ok(Self {
            root,
            connection: Mutex::new(connection),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn latest_snapshot_path(&self) -> PathBuf {
        self.root.join("latest-snapshot.json")
    }

    pub fn save_snapshot<T: Serialize>(
        &self,
        snapshot: &T,
        checked_at: &str,
    ) -> Result<String, String> {
        let json = serde_json::to_string(snapshot).map_err(|error| error.to_string())?;
        {
            let connection = self
                .connection
                .lock()
                .map_err(|_| "database lock poisoned".to_string())?;
            connection
                .execute(
                    "INSERT INTO latest_snapshot(id, checked_at, json) VALUES(1, ?1, ?2)
                     ON CONFLICT(id) DO UPDATE SET checked_at=excluded.checked_at, json=excluded.json",
                    params![checked_at, json],
                )
                .map_err(|error| error.to_string())?;
        }
        let snapshot_path = self.latest_snapshot_path();
        AtomicFile::new(snapshot_path, AllowOverwrite)
            .write(|file| file.write_all(json.as_bytes()))
            .map_err(|error| error.to_string())?;
        secure_state_file(&self.latest_snapshot_path())?;
        secure_database_sidecars(&self.root)?;
        Ok(json)
    }

    pub fn load_snapshot_json(&self) -> Result<Option<String>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        connection
            .query_row("SELECT json FROM latest_snapshot WHERE id=1", [], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|error| error.to_string())
    }

    pub fn save_collector_cache<T: Serialize>(
        &self,
        state: &T,
        saved_at: &str,
    ) -> Result<(), String> {
        let json = serde_json::to_string(state).map_err(|error| error.to_string())?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        connection
            .execute(
                "INSERT INTO collector_cache(id, saved_at, json) VALUES(1, ?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET saved_at=excluded.saved_at, json=excluded.json",
                params![saved_at, json],
            )
            .map_err(|error| error.to_string())?;
        secure_database_sidecars(&self.root)?;
        Ok(())
    }

    pub fn load_collector_cache_json(&self) -> Result<Option<String>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        connection
            .query_row("SELECT json FROM collector_cache WHERE id=1", [], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|error| error.to_string())
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        connection
            .execute(
                "INSERT INTO settings(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            )
            .map_err(|error| error.to_string())?;
        secure_database_sidecars(&self.root)?;
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        connection
            .query_row(
                "SELECT value FROM settings WHERE key=?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())
    }

    /// Imports only compatible, sanitized derived state from an older XiaoLi
    /// prototype database. Rollout cursors, snapshots, logs, message content,
    /// and source paths are deliberately excluded so the new collector always
    /// rebuilds from the rollout source of truth.
    pub fn import_legacy_derived_state(
        &self,
        legacy_database: &Path,
    ) -> Result<LegacyImportSummary, String> {
        let mut summary = self.import_legacy_preferences(legacy_database)?;
        let behavior = self.import_legacy_behavior_state(legacy_database)?;
        summary.behavior_samples = behavior.behavior_samples;
        summary.behavior_samples_v2 = behavior.behavior_samples_v2;
        Ok(summary)
    }

    /// Imports at most four tiny preference rows. This is the only legacy
    /// work allowed on the cold-start path because window construction needs
    /// these values immediately.
    pub fn import_legacy_preferences(
        &self,
        legacy_database: &Path,
    ) -> Result<LegacyImportSummary, String> {
        let source = Connection::open_with_flags(
            legacy_database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| format!("open legacy database: {error}"))?;
        let mut summary = LegacyImportSummary::default();

        if sqlite_table_exists(&source, "settings")? {
            for key in ["uiPreferencesV2", "theme", "topmost", "autostart"] {
                let value = source
                    .query_row("SELECT value FROM settings WHERE key=?1", [key], |row| {
                        row.get::<_, String>(0)
                    })
                    .optional()
                    .map_err(|error| error.to_string())?;
                if let Some(value) = value {
                    if self.get_setting(key)?.is_none() {
                        self.set_setting(key, &value)?;
                        summary.settings += 1;
                    }
                }
            }
        }

        Ok(summary)
    }

    /// Imports sanitized behavior history in bounded bulk transactions. It is
    /// intentionally called from a background worker after the window and tray
    /// are ready; importing 20k legacy rows can never delay first paint.
    pub fn import_legacy_behavior_state(
        &self,
        legacy_database: &Path,
    ) -> Result<LegacyImportSummary, String> {
        let source = Connection::open_with_flags(
            legacy_database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| format!("open legacy database: {error}"))?;
        let mut summary = LegacyImportSummary::default();

        if sqlite_table_exists(&source, "behavior_samples")? {
            let samples =
                read_sanitized_samples::<CompletedTurnSample>(&source, "behavior_samples")?;
            summary.behavior_samples =
                self.import_behavior_samples_bulk("behavior_samples", samples)?;
        }
        if sqlite_table_exists(&source, "behavior_samples_v2")? {
            let samples =
                read_sanitized_samples::<BehaviorSampleV2>(&source, "behavior_samples_v2")?;
            summary.behavior_samples_v2 =
                self.import_behavior_samples_bulk("behavior_samples_v2", samples)?;
        }
        Ok(summary)
    }

    fn import_behavior_samples_bulk<T: Serialize>(
        &self,
        table: &str,
        samples: Vec<(String, String, T)>,
    ) -> Result<usize, String> {
        let insert_sql = match table {
            "behavior_samples" => {
                "INSERT OR IGNORE INTO behavior_samples(bucket, observed_at, json) VALUES(?1, ?2, ?3)"
            }
            "behavior_samples_v2" => {
                "INSERT OR IGNORE INTO behavior_samples_v2(bucket, observed_at, json) VALUES(?1, ?2, ?3)"
            }
            _ => return Err("unsupported legacy sample table".to_owned()),
        };
        let prune_sql = match table {
            "behavior_samples" => {
                "DELETE FROM behavior_samples WHERE rowid IN (
                   SELECT rowid FROM behavior_samples WHERE bucket=?1
                   ORDER BY observed_at DESC LIMIT -1 OFFSET 100
                 )"
            }
            "behavior_samples_v2" => {
                "DELETE FROM behavior_samples_v2 WHERE rowid IN (
                   SELECT rowid FROM behavior_samples_v2 WHERE bucket=?1
                   ORDER BY observed_at DESC LIMIT -1 OFFSET 100
                 )"
            }
            _ => unreachable!("validated table"),
        };
        let encoded = samples
            .into_iter()
            .map(|(bucket, observed_at, sample)| {
                serde_json::to_string(&sample)
                    .map(|json| (bucket, observed_at, json))
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let count = encoded.len();
        if encoded.is_empty() {
            return Ok(0);
        }
        let buckets = encoded
            .iter()
            .map(|(bucket, _, _)| bucket.clone())
            .collect::<HashSet<_>>();
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| error.to_string())?;
        for (bucket, observed_at, json) in encoded {
            transaction
                .execute(insert_sql, params![bucket, observed_at, json])
                .map_err(|error| error.to_string())?;
        }
        for bucket in buckets {
            transaction
                .execute(prune_sql, params![bucket])
                .map_err(|error| error.to_string())?;
        }
        transaction.commit().map_err(|error| error.to_string())?;
        secure_database_sidecars(&self.root)?;
        Ok(count)
    }

    pub fn append_behavior_sample<T: Serialize>(
        &self,
        bucket: &str,
        observed_at: &str,
        sample: &T,
    ) -> Result<(), String> {
        let json = serde_json::to_string(sample).map_err(|error| error.to_string())?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO behavior_samples(bucket, observed_at, json) VALUES(?1, ?2, ?3)",
                params![bucket, observed_at, json],
            )
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM behavior_samples
                 WHERE rowid IN (
                   SELECT rowid FROM behavior_samples WHERE bucket=?1
                   ORDER BY observed_at DESC LIMIT -1 OFFSET 100
                 )",
                params![bucket],
            )
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())?;
        secure_database_sidecars(&self.root)?;
        Ok(())
    }

    pub fn load_behavior_samples<T: DeserializeOwned>(
        &self,
        bucket: &str,
    ) -> Result<Vec<T>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT json FROM behavior_samples WHERE bucket=?1
                 ORDER BY observed_at DESC LIMIT 100",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(params![bucket], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?;
        let mut values = Vec::new();
        for row in rows {
            let json = row.map_err(|error| error.to_string())?;
            if let Ok(value) = serde_json::from_str::<T>(&json) {
                values.push(value);
            }
        }
        Ok(values)
    }

    /// Stores sanitized V2 behavior samples independently. The legacy table is
    /// deliberately retained so an upgrade never destroys prior local data.
    pub fn append_behavior_sample_v2<T: Serialize>(
        &self,
        bucket: &str,
        observed_at: &str,
        sample: &T,
    ) -> Result<(), String> {
        let json = serde_json::to_string(sample).map_err(|error| error.to_string())?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO behavior_samples_v2(bucket, observed_at, json) VALUES(?1, ?2, ?3)",
                params![bucket, observed_at, json],
            )
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM behavior_samples_v2
                 WHERE rowid IN (
                   SELECT rowid FROM behavior_samples_v2 WHERE bucket=?1
                   ORDER BY observed_at DESC LIMIT -1 OFFSET 100
                 )",
                params![bucket],
            )
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())?;
        secure_database_sidecars(&self.root)?;
        Ok(())
    }

    pub fn load_behavior_samples_v2<T: DeserializeOwned>(
        &self,
        bucket: &str,
    ) -> Result<Vec<T>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT json FROM behavior_samples_v2 WHERE bucket=?1
                 ORDER BY observed_at DESC LIMIT 100",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(params![bucket], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?;
        let mut values = Vec::new();
        for row in rows {
            let json = row.map_err(|error| error.to_string())?;
            if let Ok(value) = serde_json::from_str::<T>(&json) {
                values.push(value);
            }
        }
        Ok(values)
    }

    pub fn append_monitor_log<T: Serialize>(&self, record: &T) -> Result<(), String> {
        let line = serde_json::to_string(record).map_err(|error| error.to_string())?;
        let path = self.root.join("monitor.jsonl");
        rotate_log_if_needed(&path).map_err(|error| error.to_string())?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| error.to_string())?;
        file.write_all(line.as_bytes())
            .map_err(|error| error.to_string())?;
        file.write_all(b"\n").map_err(|error| error.to_string())?;
        secure_state_file(&path)?;
        Ok(())
    }
}

fn prepare_private_state_root(root: &Path) -> Result<(), String> {
    fs::create_dir_all(root).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("chmod {}: {error}", root.display()))?;
    }
    Ok(())
}

fn secure_state_file(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if path.exists() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .map_err(|error| format!("chmod {}: {error}", path.display()))?;
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn secure_database_sidecars(root: &Path) -> Result<(), String> {
    for name in ["monitor.db", "monitor.db-wal", "monitor.db-shm"] {
        secure_state_file(&root.join(name))?;
    }
    Ok(())
}

fn sqlite_table_exists(connection: &Connection, table: &str) -> Result<bool, String> {
    connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1 LIMIT 1",
            [table],
            |_| Ok(()),
        )
        .optional()
        .map(|value| value.is_some())
        .map_err(|error| error.to_string())
}

fn read_sanitized_samples<T>(
    connection: &Connection,
    table: &str,
) -> Result<Vec<(String, String, T)>, String>
where
    T: DeserializeOwned,
{
    let sql = match table {
        "behavior_samples" => {
            "SELECT bucket, observed_at, json FROM behavior_samples ORDER BY observed_at DESC LIMIT 10000"
        }
        "behavior_samples_v2" => {
            "SELECT bucket, observed_at, json FROM behavior_samples_v2 ORDER BY observed_at DESC LIMIT 10000"
        }
        _ => return Err("unsupported legacy sample table".to_owned()),
    };
    let mut statement = connection.prepare(sql).map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    let mut result = Vec::new();
    for row in rows {
        let (bucket, observed_at, json) = row.map_err(|error| error.to_string())?;
        if let Ok(sample) = serde_json::from_str::<T>(&json) {
            result.push((bucket, observed_at, sample));
        }
    }
    Ok(result)
}

fn rotate_log_if_needed(path: &Path) -> io::Result<()> {
    let length = match fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if length < LOG_ROTATE_BYTES {
        return Ok(());
    }

    let oldest = path.with_extension(format!("jsonl.{LOG_BACKUPS}"));
    if oldest.exists() {
        fs::remove_file(&oldest)?;
    }
    for index in (1..LOG_BACKUPS).rev() {
        let source = path.with_extension(format!("jsonl.{index}"));
        let destination = path.with_extension(format!("jsonl.{}", index + 1));
        if source.exists() {
            fs::rename(source, destination)?;
        }
    }
    fs::rename(path, path.with_extension("jsonl.1"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn saves_snapshot_to_sqlite_and_atomic_json() {
        let root = std::env::temp_dir().join(format!("xiaoli-persistence-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let persistence = Persistence::open(&root).unwrap();
        persistence
            .save_snapshot(&json!({"schemaVersion": 4}), "2026-08-25T00:00:00Z")
            .unwrap();
        assert_eq!(
            persistence.load_snapshot_json().unwrap().unwrap(),
            "{\"schemaVersion\":4}"
        );
        assert_eq!(
            fs::read_to_string(persistence.latest_snapshot_path()).unwrap(),
            "{\"schemaVersion\":4}"
        );
        drop(persistence);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn behavior_v2_is_separate_and_legacy_table_is_preserved() {
        let root =
            std::env::temp_dir().join(format!("xiaoli-persistence-v2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let persistence = Persistence::open(&root).unwrap();
        persistence
            .append_behavior_sample("legacy", "2026-08-25T00:00:00Z", &json!({"version": 1}))
            .unwrap();
        persistence
            .append_behavior_sample_v2("v2", "2026-08-25T00:00:01Z", &json!({"version": 2}))
            .unwrap();
        let legacy = persistence
            .load_behavior_samples::<serde_json::Value>("legacy")
            .unwrap();
        let v2 = persistence
            .load_behavior_samples_v2::<serde_json::Value>("v2")
            .unwrap();
        assert_eq!(legacy[0]["version"], 1);
        assert_eq!(v2[0]["version"], 2);
        assert!(persistence
            .load_behavior_samples_v2::<serde_json::Value>("legacy")
            .unwrap()
            .is_empty());
        drop(persistence);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_import_keeps_only_preferences_and_typed_behavior_samples() {
        let base =
            std::env::temp_dir().join(format!("xiaoli-legacy-import-{}", std::process::id()));
        let source_root = base.join("source");
        let target_root = base.join("target");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&source_root).unwrap();
        let source_path = source_root.join("monitor.db");
        let source = Connection::open(&source_path).unwrap();
        source
            .execute_batch(
                "CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE behavior_samples(bucket TEXT, observed_at TEXT, json TEXT);
                 CREATE TABLE behavior_samples_v2(bucket TEXT, observed_at TEXT, json TEXT);
                 CREATE TABLE collector_cache(id INTEGER, saved_at TEXT, json TEXT);",
            )
            .unwrap();
        source
            .execute(
                "INSERT INTO settings(key,value) VALUES('theme','minimal'),('private','do-not-copy')",
                [],
            )
            .unwrap();
        let legacy = CompletedTurnSample {
            thread_id: "synthetic-thread".to_owned(),
            turn_id: "synthetic-turn".to_owned(),
            kind: crate::model::ThreadKind::Root,
            model: Some("gpt-5.6-sol".to_owned()),
            effort: Some("ultra".to_owned()),
            input_bucket: "8k-32k".to_owned(),
            tool_activity: false,
            ttft_ms: Some(900),
            duration_ms: Some(4_000),
            input_tokens: 12_000,
            output_tokens: 320,
            reasoning_output_tokens: 120,
            cache_input_share: Some(0.5),
            completed_at: "2030-01-01T00:00:00Z".to_owned(),
        };
        source
            .execute(
                "INSERT INTO behavior_samples(bucket,observed_at,json) VALUES(?1,?2,?3)",
                params![
                    "legacy-bucket",
                    legacy.completed_at,
                    serde_json::to_string(&legacy).unwrap()
                ],
            )
            .unwrap();
        source
            .execute(
                "INSERT INTO behavior_samples(bucket,observed_at,json) VALUES('bad','2030-01-01','{\"prompt\":\"PRIVATE\"}')",
                [],
            )
            .unwrap();
        source
            .execute(
                "INSERT INTO collector_cache(id,saved_at,json) VALUES(1,'2030-01-01','{\"prompt\":\"PRIVATE\"}')",
                [],
            )
            .unwrap();
        drop(source);

        let target = Persistence::open(&target_root).unwrap();
        let summary = target.import_legacy_derived_state(&source_path).unwrap();
        assert_eq!(summary.settings, 1);
        assert_eq!(summary.behavior_samples, 1);
        assert_eq!(summary.behavior_samples_v2, 0);
        assert_eq!(
            target.get_setting("theme").unwrap().as_deref(),
            Some("minimal")
        );
        assert!(target.get_setting("private").unwrap().is_none());
        assert!(target.load_collector_cache_json().unwrap().is_none());
        let imported = target
            .load_behavior_samples::<CompletedTurnSample>("legacy-bucket")
            .unwrap();
        assert_eq!(imported, vec![legacy]);
        drop(target);
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn state_directory_and_derived_files_are_private_without_ipc_startup() {
        use std::os::unix::fs::PermissionsExt;

        let root =
            std::env::temp_dir().join(format!("xiaoli-private-state-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let persistence = Persistence::open(&root).unwrap();
        persistence
            .save_snapshot(
                &serde_json::json!({"schemaVersion":4}),
                "2030-01-01T00:00:00Z",
            )
            .unwrap();
        persistence
            .append_monitor_log(&serde_json::json!({"event":"fixture"}))
            .unwrap();

        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for name in ["monitor.db", "latest-snapshot.json", "monitor.jsonl"] {
            assert_eq!(
                fs::metadata(root.join(name)).unwrap().permissions().mode() & 0o777,
                0o600,
                "{name} must remain current-user only"
            );
        }
        drop(persistence);
        fs::remove_dir_all(root).unwrap();
    }
}
