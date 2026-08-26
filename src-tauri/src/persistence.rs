use crate::{
    history::{ConversationHistoryFilter, ConversationHistoryRecord},
    model::{BehaviorSampleV2, CompletedTurnSample},
    relay_audit::{RelayAuditReportV1, RelayProfile},
    relay_baseline::RelayBaselineSummary,
};
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
                   ON behavior_samples_v2(bucket, observed_at, json);
                 CREATE TABLE IF NOT EXISTS schema_migrations (
                   version INTEGER PRIMARY KEY,
                   applied_at TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS conversation_history (
                   thread_id TEXT NOT NULL,
                   turn_id TEXT NOT NULL,
                   parent_thread_id TEXT,
                   kind TEXT NOT NULL,
                   requested_model TEXT,
                   requested_effort TEXT,
                   origin_kind TEXT NOT NULL,
                   status_level TEXT NOT NULL,
                   started_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   ended_at TEXT,
                   active INTEGER NOT NULL DEFAULT 1,
                   json TEXT NOT NULL,
                   PRIMARY KEY(thread_id, turn_id)
                 );
                 CREATE INDEX IF NOT EXISTS idx_conversation_history_updated
                   ON conversation_history(updated_at DESC);
                 CREATE INDEX IF NOT EXISTS idx_conversation_history_filters
                   ON conversation_history(origin_kind, requested_model, requested_effort, status_level);
                 CREATE TABLE IF NOT EXISTS conversation_aliases (
                   thread_id TEXT PRIMARY KEY,
                   alias TEXT NOT NULL,
                   updated_at TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS conversation_relay_bindings (
                   thread_id TEXT NOT NULL,
                   turn_id TEXT NOT NULL,
                   profile_id TEXT NOT NULL,
                   bound_at TEXT NOT NULL,
                   PRIMARY KEY(thread_id, turn_id)
                 );
                 CREATE INDEX IF NOT EXISTS idx_conversation_relay_profile
                   ON conversation_relay_bindings(profile_id, bound_at DESC);
                 CREATE TABLE IF NOT EXISTS relay_profiles (
                   id TEXT PRIMARY KEY,
                   label TEXT NOT NULL,
                   normalized_base_url TEXT NOT NULL,
                   protocol TEXT NOT NULL,
                   default_model TEXT NOT NULL,
                   credential_ref TEXT,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   json TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_relay_profiles_updated
                   ON relay_profiles(updated_at DESC);
                 CREATE TABLE IF NOT EXISTS relay_audits (
                   audit_id TEXT PRIMARY KEY,
                   profile_id TEXT NOT NULL,
                   verdict TEXT NOT NULL,
                   started_at TEXT NOT NULL,
                   completed_at TEXT,
                   json TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_relay_audits_profile_time
                   ON relay_audits(profile_id, started_at DESC);
                 CREATE TABLE IF NOT EXISTS relay_baselines (
                   id TEXT PRIMARY KEY,
                   source TEXT NOT NULL,
                   version TEXT NOT NULL,
                   imported_at TEXT NOT NULL,
                   expires_at TEXT,
                   json TEXT NOT NULL
                 );",
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

    /// Upserts the content-free active turn view and marks turns missing from
    /// the new snapshot as ended. This is intentionally separate from rollout
    /// parsing so history writes can never hold the collector refresh lock.
    pub fn sync_conversation_history(
        &self,
        records: &[ConversationHistoryRecord],
        checked_at: &str,
    ) -> Result<(), String> {
        let encoded = records
            .iter()
            .map(|record| {
                serde_json::to_string(record)
                    .map(|json| (record, json))
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let active_keys = records
            .iter()
            .map(ConversationHistoryRecord::key)
            .collect::<HashSet<_>>();

        let connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| error.to_string())?;

        for (record, json) in encoded {
            if let Some(profile_id) = record.relay_profile_id.as_deref() {
                transaction
                    .execute(
                        "INSERT INTO conversation_relay_bindings(
                           thread_id, turn_id, profile_id, bound_at
                         ) VALUES(?1, ?2, ?3, ?4)
                         ON CONFLICT(thread_id, turn_id) DO UPDATE SET
                           profile_id=excluded.profile_id,
                           bound_at=excluded.bound_at",
                        params![record.thread_id, record.turn_id, profile_id, checked_at],
                    )
                    .map_err(|error| error.to_string())?;
            }
            transaction
                .execute(
                    "INSERT INTO conversation_history(
                       thread_id, turn_id, parent_thread_id, kind,
                       requested_model, requested_effort, origin_kind,
                       status_level, started_at, updated_at, ended_at, active, json
                     ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, 1, ?11)
                     ON CONFLICT(thread_id, turn_id) DO UPDATE SET
                       parent_thread_id=excluded.parent_thread_id,
                       kind=excluded.kind,
                       requested_model=excluded.requested_model,
                       requested_effort=excluded.requested_effort,
                       origin_kind=excluded.origin_kind,
                       status_level=excluded.status_level,
                       updated_at=excluded.updated_at,
                       ended_at=NULL,
                       active=1,
                       json=excluded.json",
                    params![
                        record.thread_id,
                        record.turn_id,
                        record.parent_thread_id,
                        format!("{:?}", record.kind).to_ascii_lowercase(),
                        record.requested_model,
                        record.requested_effort,
                        record.origin_kind,
                        format!("{:?}", record.status_level).to_ascii_lowercase(),
                        record.started_at,
                        record.updated_at,
                        json,
                    ],
                )
                .map_err(|error| error.to_string())?;
        }

        let mut active_statement = transaction
            .prepare("SELECT thread_id, turn_id, json FROM conversation_history WHERE active=1")
            .map_err(|error| error.to_string())?;
        let rows = active_statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|error| error.to_string())?;
        let mut ended = Vec::new();
        for row in rows {
            let (thread_id, turn_id, json) = row.map_err(|error| error.to_string())?;
            if !active_keys.contains(&format!("{thread_id}:{turn_id}")) {
                ended.push((thread_id, turn_id, json));
            }
        }
        drop(active_statement);
        for (thread_id, turn_id, json) in ended {
            let mut record = serde_json::from_str::<ConversationHistoryRecord>(&json)
                .map_err(|error| error.to_string())?;
            record.active = false;
            record.ended_at = Some(checked_at.to_owned());
            record.updated_at = checked_at.to_owned();
            let json = serde_json::to_string(&record).map_err(|error| error.to_string())?;
            transaction
                .execute(
                    "UPDATE conversation_history
                     SET active=0, ended_at=?3, updated_at=?3, json=?4
                     WHERE thread_id=?1 AND turn_id=?2",
                    params![thread_id, turn_id, checked_at, json],
                )
                .map_err(|error| error.to_string())?;
        }
        transaction.commit().map_err(|error| error.to_string())?;
        secure_database_sidecars(&self.root)?;
        Ok(())
    }

    pub fn list_conversation_history(
        &self,
        filter: &ConversationHistoryFilter,
    ) -> Result<Vec<ConversationHistoryRecord>, String> {
        self.list_conversation_history_with_total(filter)
            .map(|(records, _)| records)
    }

    pub fn list_conversation_history_with_total(
        &self,
        filter: &ConversationHistoryFilter,
    ) -> Result<(Vec<ConversationHistoryRecord>, usize), String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let query = filter.query.trim().to_ascii_lowercase();
        let model = filter.model.trim().to_ascii_lowercase();
        let effort = filter.effort.trim().to_ascii_lowercase();
        let origin = filter.origin_kind.trim().to_ascii_lowercase();
        let status = filter.status_level.trim().to_ascii_lowercase();
        let date_from = filter.date_from.trim();
        let date_to = filter.date_to.trim();
        let where_clause = "(?1='' OR instr(lower(h.thread_id), ?1)>0 OR instr(lower(h.json), ?1)>0 OR instr(lower(coalesce(a.alias,'')), ?1)>0)
             AND (?2='' OR lower(coalesce(h.requested_model,''))=?2)
             AND (?3='' OR lower(coalesce(h.requested_effort,''))=?3)
             AND (
               ?4=''
               OR (?4='official' AND lower(h.origin_kind) IN (
                  'officialchatgpt','officialopenaiapi','officialanthropicapi'
               ))
               OR (?4='custom' AND lower(h.origin_kind) IN (
                  'customendpoint','localendpoint','managedprovider'
               ))
               OR lower(h.origin_kind)=?4
             )
             AND (?5='' OR lower(h.status_level)=?5)
             AND (?6='' OR h.updated_at>=?6)
             AND (?7='' OR h.updated_at<=?7)";
        let total = connection
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM conversation_history h
                     LEFT JOIN conversation_aliases a ON a.thread_id=h.thread_id
                     WHERE {where_clause}"
                ),
                params![&query, &model, &effort, &origin, &status, date_from, date_to],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?;
        let limit = i64::try_from(filter.bounded_limit()).unwrap_or(200);
        let offset = i64::try_from(filter.offset).unwrap_or(i64::MAX);
        let mut statement = connection
            .prepare(&format!(
                "SELECT h.json, a.alias, b.profile_id FROM conversation_history h
                 LEFT JOIN conversation_aliases a ON a.thread_id=h.thread_id
                 LEFT JOIN conversation_relay_bindings b
                   ON b.thread_id=h.thread_id AND b.turn_id=h.turn_id
                 WHERE {where_clause}
                 ORDER BY h.updated_at DESC
                 LIMIT ?8 OFFSET ?9"
            ))
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(
                params![
                    &query, &model, &effort, &origin, &status, date_from, date_to, limit, offset
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .map_err(|error| error.to_string())?;
        let mut page = Vec::new();
        for row in rows {
            let (json, alias, profile_id) = row.map_err(|error| error.to_string())?;
            let mut record = serde_json::from_str::<ConversationHistoryRecord>(&json)
                .map_err(|error| error.to_string())?;
            record.apply_local_alias(alias);
            record.relay_profile_id = profile_id;
            page.push(record);
        }
        Ok((page, usize::try_from(total).unwrap_or(usize::MAX)))
    }

    pub fn get_conversation_history(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<Option<ConversationHistoryRecord>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let row = connection
            .query_row(
                "SELECT h.json, a.alias, b.profile_id FROM conversation_history h
                 LEFT JOIN conversation_aliases a ON a.thread_id=h.thread_id
                 LEFT JOIN conversation_relay_bindings b
                   ON b.thread_id=h.thread_id AND b.turn_id=h.turn_id
                 WHERE h.thread_id=?1 AND h.turn_id=?2",
                params![thread_id, turn_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| error.to_string())?;
        row.map(|(json, alias, profile_id)| {
            let mut record = serde_json::from_str::<ConversationHistoryRecord>(&json)
                .map_err(|error| error.to_string())?;
            record.apply_local_alias(alias);
            record.relay_profile_id = profile_id;
            Ok(record)
        })
        .transpose()
    }

    /// Creates, replaces, or clears a content-free local display alias for all
    /// turns in one thread. The alias table remains independent from rollout
    /// history so task titles and transcripts are never copied into it.
    pub fn set_conversation_alias(
        &self,
        thread_id: &str,
        alias: Option<&str>,
        updated_at: &str,
    ) -> Result<Option<String>, String> {
        let thread_id = thread_id.trim();
        if thread_id.is_empty() || thread_id.chars().count() > 256 {
            return Err("thread id must contain 1 to 256 characters".to_owned());
        }
        let normalized = alias.map(normalize_local_alias).transpose()?.flatten();
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        if let Some(alias) = normalized.as_ref() {
            connection
                .execute(
                    "INSERT INTO conversation_aliases(thread_id, alias, updated_at)
                     VALUES(?1, ?2, ?3)
                     ON CONFLICT(thread_id) DO UPDATE SET
                       alias=excluded.alias, updated_at=excluded.updated_at",
                    params![thread_id, alias, updated_at],
                )
                .map_err(|error| error.to_string())?;
        } else {
            connection
                .execute(
                    "DELETE FROM conversation_aliases WHERE thread_id=?1",
                    params![thread_id],
                )
                .map_err(|error| error.to_string())?;
        }
        secure_database_sidecars(&self.root)?;
        Ok(normalized)
    }

    /// Loads recent completed rows that were conservatively bound to a relay
    /// profile while the turn was active. Host hashes and URLs are never read
    /// from or written to this table.
    pub fn list_relay_bound_conversation_history(
        &self,
        profile_id: &str,
        cutoff_iso: &str,
        limit: usize,
    ) -> Result<Vec<ConversationHistoryRecord>, String> {
        let profile_id = profile_id.trim();
        if profile_id.is_empty() || profile_id.chars().count() > 128 {
            return Err("invalid relay profile id".to_owned());
        }
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT h.json FROM conversation_history h
                 INNER JOIN conversation_relay_bindings b
                   ON b.thread_id=h.thread_id AND b.turn_id=h.turn_id
                 WHERE h.active=0 AND h.updated_at>=?1 AND b.profile_id=?2
                 ORDER BY h.updated_at DESC LIMIT ?3",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(
                params![
                    cutoff_iso,
                    profile_id,
                    i64::try_from(limit.clamp(1, 1_000)).unwrap_or(1_000)
                ],
                |row| row.get::<_, String>(0),
            )
            .map_err(|error| error.to_string())?;
        let mut matches = Vec::new();
        for row in rows {
            let json = row.map_err(|error| error.to_string())?;
            let mut record = serde_json::from_str::<ConversationHistoryRecord>(&json)
                .map_err(|error| error.to_string())?;
            record.relay_profile_id = Some(profile_id.to_owned());
            matches.push(record);
        }
        Ok(matches)
    }

    pub fn prune_conversation_history(&self, cutoff_iso: &str) -> Result<usize, String> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| error.to_string())?;
        let changed = transaction
            .execute(
                "DELETE FROM conversation_history WHERE active=0 AND updated_at < ?1",
                params![cutoff_iso],
            )
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM conversation_relay_bindings
                 WHERE NOT EXISTS (
                   SELECT 1 FROM conversation_history h
                   WHERE h.thread_id=conversation_relay_bindings.thread_id
                     AND h.turn_id=conversation_relay_bindings.turn_id
                 )",
                [],
            )
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM conversation_aliases
                 WHERE NOT EXISTS (
                   SELECT 1 FROM conversation_history h
                   WHERE h.thread_id=conversation_aliases.thread_id
                 )",
                [],
            )
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())?;
        secure_database_sidecars(&self.root)?;
        Ok(changed)
    }

    pub fn upsert_relay_profile(&self, profile: &RelayProfile) -> Result<(), String> {
        self.upsert_relay_profile_with_setting(profile, None)
            .map(|_| ())
    }

    /// Atomically updates a relay profile and, when supplied, the schedule
    /// setting bound to it. The credential store is intentionally handled by
    /// the caller as a separate staged resource, but SQLite must never expose a
    /// new profile with the old schedule (or vice versa).
    pub fn upsert_relay_profile_with_setting(
        &self,
        profile: &RelayProfile,
        setting: Option<(&str, &str)>,
    ) -> Result<Option<String>, String> {
        let json = serde_json::to_string(profile).map_err(|error| error.to_string())?;
        let protocol = enum_wire(&profile.protocol)?;
        secure_database_sidecars(&self.root)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| error.to_string())?;
        if let Some((key, value)) = setting {
            transaction
                .execute(
                    "INSERT INTO settings(key, value) VALUES(?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    params![key, value],
                )
                .map_err(|error| error.to_string())?;
        }
        transaction
            .execute(
                "INSERT INTO relay_profiles(
                   id, label, normalized_base_url, protocol, default_model,
                   credential_ref, created_at, updated_at, json
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(id) DO UPDATE SET
                   label=excluded.label,
                   normalized_base_url=excluded.normalized_base_url,
                   protocol=excluded.protocol,
                   default_model=excluded.default_model,
                   credential_ref=excluded.credential_ref,
                   updated_at=excluded.updated_at,
                   json=excluded.json",
                params![
                    &profile.id,
                    &profile.label,
                    &profile.normalized_base_url,
                    protocol,
                    &profile.default_model,
                    &profile.credential_ref,
                    &profile.created_at,
                    &profile.updated_at,
                    json,
                ],
            )
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())?;
        Ok(secure_database_sidecars(&self.root)
            .err()
            .map(|_| "配置已提交，但数据库侧文件权限未能再次确认；请检查状态目录权限".to_owned()))
    }

    pub fn list_relay_profiles(&self) -> Result<Vec<RelayProfile>, String> {
        self.query_json_rows(
            "SELECT json FROM relay_profiles ORDER BY updated_at DESC",
            [],
        )
    }

    pub fn get_relay_profile(&self, profile_id: &str) -> Result<Option<RelayProfile>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let json = connection
            .query_row(
                "SELECT json FROM relay_profiles WHERE id=?1",
                params![profile_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        json.map(|value| serde_json::from_str(&value).map_err(|error| error.to_string()))
            .transpose()
    }

    pub fn delete_relay_profile(&self, profile_id: &str) -> Result<bool, String> {
        self.delete_relay_profile_with_setting(profile_id, None)
            .map(|(deleted, _)| deleted)
    }

    pub fn delete_relay_profile_with_setting(
        &self,
        profile_id: &str,
        setting: Option<(&str, &str)>,
    ) -> Result<(bool, Option<String>), String> {
        secure_database_sidecars(&self.root)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| error.to_string())?;
        if let Some((key, value)) = setting {
            transaction
                .execute(
                    "INSERT INTO settings(key, value) VALUES(?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    params![key, value],
                )
                .map_err(|error| error.to_string())?;
        }
        let changed = transaction
            .execute(
                "DELETE FROM relay_profiles WHERE id=?1",
                params![profile_id],
            )
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())?;
        let permission_warning = secure_database_sidecars(&self.root)
            .err()
            .map(|_| "配置已提交，但数据库侧文件权限未能再次确认；请检查状态目录权限".to_owned());
        Ok((changed > 0, permission_warning))
    }

    pub fn save_relay_audit(&self, report: &RelayAuditReportV1) -> Result<(), String> {
        let json = serde_json::to_string(report).map_err(|error| error.to_string())?;
        let verdict = enum_wire(&report.overall_verdict)?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        connection
            .execute(
                "INSERT INTO relay_audits(
                   audit_id, profile_id, verdict, started_at, completed_at, json
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(audit_id) DO UPDATE SET
                   verdict=excluded.verdict,
                   completed_at=excluded.completed_at,
                   json=excluded.json",
                params![
                    &report.audit_id,
                    &report.profile_id,
                    verdict,
                    &report.started_at,
                    &report.completed_at,
                    json,
                ],
            )
            .map_err(|error| error.to_string())?;
        secure_database_sidecars(&self.root)?;
        Ok(())
    }

    pub fn get_relay_audit(&self, audit_id: &str) -> Result<Option<RelayAuditReportV1>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let json = connection
            .query_row(
                "SELECT json FROM relay_audits WHERE audit_id=?1",
                params![audit_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        json.map(|value| serde_json::from_str(&value).map_err(|error| error.to_string()))
            .transpose()
    }

    pub fn list_relay_audits(&self, limit: usize) -> Result<Vec<RelayAuditReportV1>, String> {
        let limit = limit.clamp(1, 200) as i64;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let mut statement = connection
            .prepare("SELECT json FROM relay_audits ORDER BY started_at DESC LIMIT ?1")
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(params![limit], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?;
        decode_json_rows(rows)
    }

    pub fn delete_relay_audit(&self, audit_id: &str) -> Result<bool, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let changed = connection
            .execute(
                "DELETE FROM relay_audits WHERE audit_id=?1",
                params![audit_id],
            )
            .map_err(|error| error.to_string())?;
        secure_database_sidecars(&self.root)?;
        Ok(changed > 0)
    }

    pub fn upsert_relay_baseline(&self, baseline: &RelayBaselineSummary) -> Result<(), String> {
        if !matches!(baseline.source.as_str(), "official" | "community" | "user") {
            return Err("invalid relay baseline source".to_owned());
        }
        let json = serde_json::to_string(baseline).map_err(|error| error.to_string())?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        connection
            .execute(
                "INSERT INTO relay_baselines(id, source, version, imported_at, expires_at, json)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET
                   source=excluded.source,
                   version=excluded.version,
                   imported_at=excluded.imported_at,
                   expires_at=excluded.expires_at,
                   json=excluded.json",
                params![
                    &baseline.id,
                    &baseline.source,
                    &baseline.version,
                    &baseline.created_at,
                    &baseline.expires_at,
                    json,
                ],
            )
            .map_err(|error| error.to_string())?;
        secure_database_sidecars(&self.root)?;
        Ok(())
    }

    pub fn list_relay_baselines(&self) -> Result<Vec<RelayBaselineSummary>, String> {
        self.query_json_rows(
            "SELECT json FROM relay_baselines ORDER BY imported_at DESC",
            [],
        )
    }

    pub fn delete_user_relay_baseline(&self, baseline_id: &str) -> Result<bool, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let changed = connection
            .execute(
                "DELETE FROM relay_baselines WHERE id=?1 AND source='user'",
                params![baseline_id],
            )
            .map_err(|error| error.to_string())?;
        secure_database_sidecars(&self.root)?;
        Ok(changed > 0)
    }

    fn query_json_rows<T, P>(&self, sql: &str, params: P) -> Result<Vec<T>, String>
    where
        T: DeserializeOwned,
        P: rusqlite::Params,
    {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let mut statement = connection.prepare(sql).map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(params, |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?;
        decode_json_rows(rows)
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

fn normalize_local_alias(value: &str) -> Result<Option<String>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().count() > 80 {
        return Err("local alias must not exceed 80 characters".to_owned());
    }
    if value.chars().any(|character| {
        character.is_control()
            || matches!(
                character,
                '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
            )
    }) {
        return Err("local alias contains unsupported control characters".to_owned());
    }
    Ok(Some(value.to_owned()))
}

#[cfg(test)]
fn history_origin_matches(record_origin: &str, requested_group: &str) -> bool {
    let record_origin = record_origin.to_ascii_lowercase();
    match requested_group {
        "" => true,
        "official" => matches!(
            record_origin.as_str(),
            "officialchatgpt" | "officialopenaiapi" | "officialanthropicapi"
        ),
        "custom" => matches!(
            record_origin.as_str(),
            "customendpoint" | "localendpoint" | "managedprovider"
        ),
        expected => record_origin == expected,
    }
}

fn enum_wire<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_value(value)
        .map_err(|error| error.to_string())?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "enum did not serialize as a string".to_owned())
}

fn decode_json_rows<T, I>(rows: I) -> Result<Vec<T>, String>
where
    T: DeserializeOwned,
    I: IntoIterator<Item = rusqlite::Result<String>>,
{
    let mut values = Vec::new();
    for row in rows {
        let json = row.map_err(|error| error.to_string())?;
        let value = serde_json::from_str(&json).map_err(|error| error.to_string())?;
        values.push(value);
    }
    Ok(values)
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
    use crate::{
        connection::ConnectionOriginSnapshot,
        model::{
            ConversationSnapshot, QualityAssessment, RequestSnapshot, ServerRouteSnapshot,
            StatusLevel, StatusSnapshot, ThreadKind, TimingSnapshot, UsageSnapshot,
        },
        relay_audit::{
            AuditDetector, AuditMode, AuditParametersSnapshot, ConnectionEvidence,
            EvidenceConfidence, IdentityAssessment, IdentityAssessmentKind, OverallVerdict,
            PrivateProbePackReference, ProtocolAssessment, QualityAssessmentKind, RelayProtocol,
            RelayQualityAssessment, UsageAssessment, UsageAssessmentKind,
            RELAY_AUDIT_REPORT_SCHEMA_VERSION,
        },
        relay_baseline::RelayBaselineSummary,
    };
    use serde_json::json;

    fn assert_state_files_exclude(root: &Path, forbidden: &[&str]) {
        for entry in fs::read_dir(root).expect("read state directory") {
            let entry = entry.expect("state entry");
            if !entry.file_type().expect("state entry type").is_file() {
                continue;
            }
            let bytes = fs::read(entry.path()).expect("read state file");
            for marker in forbidden {
                assert!(
                    !bytes
                        .windows(marker.len())
                        .any(|window| window == marker.as_bytes()),
                    "{} leaked {marker}",
                    entry.path().display()
                );
            }
        }
    }

    fn relay_report_fixture(audit_id: &str, profile_id: &str) -> RelayAuditReportV1 {
        RelayAuditReportV1 {
            schema_version: RELAY_AUDIT_REPORT_SCHEMA_VERSION,
            audit_id: audit_id.to_owned(),
            profile_id: profile_id.to_owned(),
            claimed_model: "gpt-5.6-sol".to_owned(),
            protocol: RelayProtocol::OpenAiResponses,
            started_at: "2026-08-27T02:00:00Z".to_owned(),
            completed_at: Some("2026-08-27T02:01:00Z".to_owned()),
            parameters: AuditParametersSnapshot {
                mode: AuditMode::Quick,
                max_requests: 150,
                max_input_tokens: 50_000,
                max_output_tokens: 10_000,
                timeout_ms: 30_000,
                run_seed: [7; 32],
                enabled_detectors: vec![
                    AuditDetector::Protocol,
                    AuditDetector::Usage,
                    AuditDetector::Fingerprint,
                ],
                private_probe_pack: None,
            },
            connection_evidence: ConnectionEvidence {
                endpoint_class: "customEndpoint".to_owned(),
                protocol: RelayProtocol::OpenAiResponses,
                self_reported_model: Some("gpt-5.6-sol".to_owned()),
                evidence: vec!["responseEnvelope".to_owned()],
                limitations: vec!["physicalModelNotProven".to_owned()],
            },
            protocol_findings: ProtocolAssessment::normal(),
            usage_reconciliation: UsageAssessment {
                state: UsageAssessmentKind::InsufficientEvidence,
                factors: Vec::new(),
                reasons: vec!["pairedBaselineMissing".to_owned()],
                limitations: Vec::new(),
            },
            quality_findings: RelayQualityAssessment {
                state: QualityAssessmentKind::Learning,
                baseline_sample_count: 0,
                failed_domains: Vec::new(),
                factors: Vec::new(),
                reasons: vec!["learning".to_owned()],
                limitations: Vec::new(),
            },
            fingerprint_findings: IdentityAssessment {
                state: IdentityAssessmentKind::Unproven,
                eligible_cells: 0,
                mean_js_divergence: None,
                compared_reference: None,
                string_kernel_mmd: None,
                reasons: Vec::new(),
                limitations: vec!["physicalModelNotProven".to_owned()],
            },
            paired_baseline: None,
            community_baseline: None,
            selective_service_assessment: None,
            overall_verdict: OverallVerdict::InsufficientEvidence,
            confidence: EvidenceConfidence::Low,
            reasons: vec!["insufficientEvidence".to_owned()],
            limitations: vec!["blackBoxAuditCanBeEvaded".to_owned()],
        }
    }

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

    #[test]
    fn conversation_history_round_trip_and_end_transition_stay_content_free() {
        const PRIVATE_TITLE: &str = "PRIVATE_HISTORY_TITLE_MUST_NOT_PERSIST";
        const PRIVATE_PROMPT: &str = "PRIVATE_HISTORY_PROMPT_MUST_NOT_PERSIST";
        const PRIVATE_RESPONSE: &str = "PRIVATE_HISTORY_RESPONSE_MUST_NOT_PERSIST";
        const PRIVATE_CWD: &str = "PRIVATE_HISTORY_CWD_MUST_NOT_PERSIST";
        const PRIVATE_CREDENTIAL: &str = "PRIVATE_HISTORY_CREDENTIAL_MUST_NOT_PERSIST";
        let root =
            std::env::temp_dir().join(format!("xiaoli-history-contract-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let persistence = Persistence::open(&root).unwrap();
        let conversation = ConversationSnapshot {
            thread_id: "thread-history".to_owned(),
            turn_id: "turn-history".to_owned(),
            parent_thread_id: None,
            kind: ThreadKind::Root,
            title: format!(
                "{PRIVATE_TITLE} {PRIVATE_PROMPT} {PRIVATE_RESPONSE} {PRIVATE_CWD} {PRIVATE_CREDENTIAL}"
            ),
            source_timestamp: Some("2026-08-27T01:00:00Z".to_owned()),
            active_request: RequestSnapshot::new(
                Some("gpt-5.6-sol".to_owned()),
                Some("ultra".to_owned()),
                "turnContext",
            ),
            pending_next_turn: None,
            server_route: ServerRouteSnapshot::default(),
            usage: UsageSnapshot::default(),
            timing: TimingSnapshot::default(),
            quality_assessment: QualityAssessment::default(),
            connection_origin: ConnectionOriginSnapshot::unknown(),
            tool_activity: false,
            status: StatusSnapshot {
                level: StatusLevel::Green,
                code: "ok".to_owned(),
                explanation: "request configuration consistent".to_owned(),
            },
            anomalies: Vec::new(),
        };
        let record = ConversationHistoryRecord::from_live(&conversation, "2026-08-27T01:02:00Z");
        persistence
            .sync_conversation_history(std::slice::from_ref(&record), "2026-08-27T01:02:00Z")
            .unwrap();
        let active = persistence
            .get_conversation_history("thread-history", "turn-history")
            .unwrap()
            .expect("active history row");
        assert!(active.active);
        assert_eq!(active.display_label, "2026-08-27T01:02 · thread-h");

        persistence
            .sync_conversation_history(&[], "2026-08-27T01:03:00Z")
            .unwrap();
        let ended = persistence
            .get_conversation_history("thread-history", "turn-history")
            .unwrap()
            .expect("ended history row");
        assert!(!ended.active);
        assert_eq!(ended.ended_at.as_deref(), Some("2026-08-27T01:03:00Z"));
        let listed = persistence
            .list_conversation_history(&ConversationHistoryFilter {
                model: "gpt-5.6-sol".to_owned(),
                effort: "ultra".to_owned(),
                ..ConversationHistoryFilter::default()
            })
            .unwrap();
        assert_eq!(listed.len(), 1);

        assert_state_files_exclude(
            &root,
            &[
                PRIVATE_TITLE,
                PRIVATE_PROMPT,
                PRIVATE_RESPONSE,
                PRIVATE_CWD,
                PRIVATE_CREDENTIAL,
            ],
        );
        drop(persistence);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn relay_binding_and_local_alias_survive_refresh_without_exposing_host_evidence() {
        const HOST_SENTINEL: &str = "PRIVATE-RELAY-HOST-HASH-MUST-NOT-PERSIST";
        let root =
            std::env::temp_dir().join(format!("xiaoli-history-binding-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let persistence = Persistence::open(&root).unwrap();
        let conversation = ConversationSnapshot {
            thread_id: "thread-bound".to_owned(),
            turn_id: "turn-bound".to_owned(),
            parent_thread_id: None,
            kind: ThreadKind::Root,
            title: HOST_SENTINEL.to_owned(),
            source_timestamp: Some("2026-08-27T01:00:00Z".to_owned()),
            active_request: RequestSnapshot::new(
                Some("gpt-5.6-sol".to_owned()),
                Some("ultra".to_owned()),
                "turnContext",
            ),
            pending_next_turn: None,
            server_route: ServerRouteSnapshot::default(),
            usage: UsageSnapshot::default(),
            timing: TimingSnapshot::default(),
            quality_assessment: QualityAssessment::default(),
            connection_origin: ConnectionOriginSnapshot::unknown(),
            tool_activity: false,
            status: StatusSnapshot {
                level: StatusLevel::Yellow,
                code: "suspected_degradation".to_owned(),
                explanation: HOST_SENTINEL.to_owned(),
            },
            anomalies: vec![HOST_SENTINEL.to_owned()],
        };
        let mut bound = ConversationHistoryRecord::from_live(&conversation, "2026-08-27T01:01:00Z");
        bound.relay_profile_id = Some("relay-profile".to_owned());
        persistence
            .sync_conversation_history(std::slice::from_ref(&bound), "2026-08-27T01:01:00Z")
            .unwrap();

        // A later refresh can lose ephemeral hook evidence after a restart;
        // the separate local binding table must retain the earlier exact match.
        let unbound_refresh =
            ConversationHistoryRecord::from_live(&conversation, "2026-08-27T01:02:00Z");
        persistence
            .sync_conversation_history(
                std::slice::from_ref(&unbound_refresh),
                "2026-08-27T01:02:00Z",
            )
            .unwrap();
        persistence
            .sync_conversation_history(&[], "2026-08-27T01:03:00Z")
            .unwrap();

        persistence
            .set_conversation_alias(
                "thread-bound",
                Some("  本地测试别名  "),
                "2026-08-27T01:04:00Z",
            )
            .unwrap();
        let detail = persistence
            .get_conversation_history("thread-bound", "turn-bound")
            .unwrap()
            .unwrap();
        assert_eq!(detail.local_alias.as_deref(), Some("本地测试别名"));
        assert_eq!(detail.display_label, "本地测试别名");
        assert_eq!(detail.relay_profile_id.as_deref(), Some("relay-profile"));
        let bound_history = persistence
            .list_relay_bound_conversation_history("relay-profile", "2026-08-01T00:00:00Z", 100)
            .unwrap();
        assert_eq!(bound_history.len(), 1);
        assert_eq!(bound_history[0].thread_id, "thread-bound");
        assert_eq!(
            persistence
                .list_conversation_history(&ConversationHistoryFilter {
                    query: "本地测试别名".to_owned(),
                    ..ConversationHistoryFilter::default()
                })
                .unwrap()
                .len(),
            1
        );

        // Re-open the database to prove both overlays survive a process
        // restart, not merely the in-memory lifetime of this Persistence.
        drop(persistence);
        let persistence = Persistence::open(&root).unwrap();
        let reopened = persistence
            .get_conversation_history("thread-bound", "turn-bound")
            .unwrap()
            .unwrap();
        assert_eq!(reopened.local_alias.as_deref(), Some("本地测试别名"));
        assert_eq!(reopened.display_label, "本地测试别名");
        assert_eq!(reopened.relay_profile_id.as_deref(), Some("relay-profile"));
        assert_state_files_exclude(&root, &[HOST_SENTINEL]);

        assert!(persistence
            .set_conversation_alias(
                "thread-bound",
                Some("bad\u{202e}alias"),
                "2026-08-27T01:05:00Z"
            )
            .is_err());
        persistence
            .set_conversation_alias("thread-bound", Some("   "), "2026-08-27T01:06:00Z")
            .unwrap();
        assert!(persistence
            .get_conversation_history("thread-bound", "turn-bound")
            .unwrap()
            .unwrap()
            .local_alias
            .is_none());
        persistence
            .set_conversation_alias(
                "thread-bound",
                Some("即将随历史清理"),
                "2026-08-27T01:07:00Z",
            )
            .unwrap();
        assert_eq!(
            persistence
                .prune_conversation_history("2026-08-27T01:08:00Z")
                .unwrap(),
            1
        );
        assert!(persistence
            .get_conversation_history("thread-bound", "turn-bound")
            .unwrap()
            .is_none());
        let connection = persistence.connection.lock().unwrap();
        for table in ["conversation_aliases", "conversation_relay_bindings"] {
            let count = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap();
            assert_eq!(count, 0, "{table} must not retain orphan rows");
        }
        drop(connection);
        assert_state_files_exclude(&root, &[HOST_SENTINEL]);
        drop(persistence);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn history_origin_groups_match_the_workbench_filters() {
        assert!(history_origin_matches("officialChatGpt", "official"));
        assert!(history_origin_matches("officialOpenAiApi", "official"));
        assert!(history_origin_matches("officialAnthropicApi", "official"));
        assert!(history_origin_matches("customEndpoint", "custom"));
        assert!(history_origin_matches("localEndpoint", "custom"));
        assert!(history_origin_matches("managedProvider", "custom"));
        assert!(history_origin_matches("unknown", "unknown"));
        assert!(!history_origin_matches("customEndpoint", "official"));
    }

    #[test]
    fn history_filter_and_count_are_not_truncated_at_five_thousand_rows() {
        let root =
            std::env::temp_dir().join(format!("xiaoli-history-pagination-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let persistence = Persistence::open(&root).unwrap();
        let conversation = ConversationSnapshot {
            thread_id: "thread-template".to_owned(),
            turn_id: "turn-template".to_owned(),
            parent_thread_id: None,
            kind: ThreadKind::Root,
            title: String::new(),
            source_timestamp: Some("2026-08-01T00:00:00Z".to_owned()),
            active_request: RequestSnapshot::new(
                Some("gpt-5.6-sol".to_owned()),
                Some("ultra".to_owned()),
                "turnContext",
            ),
            pending_next_turn: None,
            server_route: ServerRouteSnapshot::default(),
            usage: UsageSnapshot::default(),
            timing: TimingSnapshot::default(),
            quality_assessment: QualityAssessment::default(),
            connection_origin: ConnectionOriginSnapshot::unknown(),
            tool_activity: false,
            status: StatusSnapshot {
                level: StatusLevel::Green,
                code: "ok".to_owned(),
                explanation: "healthy".to_owned(),
            },
            anomalies: Vec::new(),
        };
        let template = ConversationHistoryRecord::from_live(&conversation, "2026-08-01T00:00:00Z");
        {
            let mut connection = persistence.connection.lock().unwrap();
            let transaction = connection.transaction().unwrap();
            for index in 0..5_005_u32 {
                let mut record = template.clone();
                record.thread_id = format!("thread-{index:05}");
                record.turn_id = format!("turn-{index:05}");
                record.display_label = if index == 0 {
                    "needle-older-than-the-former-cap".to_owned()
                } else {
                    format!("history row {index}")
                };
                record.updated_at = format!("2026-08-01T00:{:02}:{:02}Z", index / 60, index % 60);
                let json = serde_json::to_string(&record).unwrap();
                transaction
                    .execute(
                        "INSERT INTO conversation_history(
                           thread_id, turn_id, parent_thread_id, kind,
                           requested_model, requested_effort, origin_kind,
                           status_level, started_at, updated_at, ended_at, active, json
                         ) VALUES(?1, ?2, NULL, 'root', ?3, ?4, ?5, 'green', ?6, ?7, NULL, 1, ?8)",
                        params![
                            record.thread_id,
                            record.turn_id,
                            record.requested_model,
                            record.requested_effort,
                            record.origin_kind,
                            record.started_at,
                            record.updated_at,
                            json,
                        ],
                    )
                    .unwrap();
            }
            transaction.commit().unwrap();
        }
        let (older_match, matched_total) = persistence
            .list_conversation_history_with_total(&ConversationHistoryFilter {
                query: "needle-older-than-the-former-cap".to_owned(),
                ..ConversationHistoryFilter::default()
            })
            .unwrap();
        assert_eq!(matched_total, 1);
        assert_eq!(older_match.len(), 1);
        assert_eq!(older_match[0].thread_id, "thread-00000");
        let (last_page, total) = persistence
            .list_conversation_history_with_total(&ConversationHistoryFilter {
                limit: 20,
                offset: 5_000,
                ..ConversationHistoryFilter::default()
            })
            .unwrap();
        assert_eq!(total, 5_005);
        assert_eq!(last_page.len(), 5);
        drop(persistence);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn relay_profile_report_and_baseline_persistence_contracts_round_trip() {
        const PRIVATE_API_KEY: &str = "sk-PRIVATE_API_KEY_MUST_NOT_PERSIST";
        const PRIVATE_PROMPT: &str = "PRIVATE_AUDIT_PROMPT_MUST_NOT_PERSIST";
        const PRIVATE_RESPONSE: &str = "PRIVATE_RAW_RELAY_RESPONSE_MUST_NOT_PERSIST";
        const PRIVATE_CWD: &str = "PRIVATE_AUDIT_CWD_MUST_NOT_PERSIST";
        let root = std::env::temp_dir().join(format!(
            "xiaoli-relay-persistence-contract-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let persistence = Persistence::open(&root).unwrap();
        let pack_reference = PrivateProbePackReference {
            path: root.join("user-probes.json").to_string_lossy().into_owned(),
            version: "private-v1".to_owned(),
            sha256: "cd".repeat(32),
        };

        let profile = RelayProfile {
            id: "relay-safe-profile".to_owned(),
            label: "Local relay audit".to_owned(),
            normalized_base_url: "https://relay.example/v1".to_owned(),
            protocol: RelayProtocol::OpenAiResponses,
            default_model: "gpt-5.6-sol".to_owned(),
            credential_ref: Some("keyring:XiaoLi:relay-safe-profile".to_owned()),
            private_probe_pack: Some(pack_reference.clone()),
            created_at: "2026-08-27T01:00:00Z".to_owned(),
            updated_at: "2026-08-27T01:00:00Z".to_owned(),
        };
        persistence.upsert_relay_profile(&profile).unwrap();
        assert_eq!(
            persistence.get_relay_profile(&profile.id).unwrap().as_ref(),
            Some(&profile)
        );
        assert_eq!(
            persistence.list_relay_profiles().unwrap(),
            vec![profile.clone()]
        );
        let profile_json = serde_json::to_value(&profile).unwrap();
        for forbidden_field in ["apiKey", "credential", "prompt", "response"] {
            assert!(profile_json.get(forbidden_field).is_none());
        }

        let mut report = relay_report_fixture("audit-safe", &profile.id);
        report.parameters.private_probe_pack = Some(pack_reference.clone());
        persistence.save_relay_audit(&report).unwrap();
        assert_eq!(
            persistence.get_relay_audit("audit-safe").unwrap(),
            Some(report.clone())
        );
        assert_eq!(persistence.list_relay_audits(20).unwrap(), vec![report]);
        assert!(persistence.delete_relay_audit("audit-safe").unwrap());
        assert!(persistence.get_relay_audit("audit-safe").unwrap().is_none());
        assert!(!persistence.delete_relay_audit("audit-safe").unwrap());

        let official = RelayBaselineSummary {
            id: "official-gpt-5.6-sol".to_owned(),
            label: "Official paired baseline".to_owned(),
            model: "gpt-5.6-sol".to_owned(),
            protocol: RelayProtocol::OpenAiResponses,
            source: "official".to_owned(),
            version: "2026-08-27".to_owned(),
            sample_count: 240,
            created_at: "2026-08-27T01:00:00Z".to_owned(),
            expires_at: Some("2026-09-27T01:00:00Z".to_owned()),
            signed: false,
            limitations: vec!["pairedToExactParameters".to_owned()],
        };
        let user = RelayBaselineSummary {
            id: "user-private-pack".to_owned(),
            label: "User private pack metadata".to_owned(),
            source: "user".to_owned(),
            signed: false,
            ..official.clone()
        };
        persistence.upsert_relay_baseline(&official).unwrap();
        persistence.upsert_relay_baseline(&user).unwrap();
        let baselines = persistence.list_relay_baselines().unwrap();
        assert_eq!(baselines.len(), 2);
        assert!(!persistence
            .delete_user_relay_baseline(&official.id)
            .unwrap());
        assert!(persistence.delete_user_relay_baseline(&user.id).unwrap());
        assert_eq!(persistence.list_relay_baselines().unwrap(), vec![official]);
        let invalid = RelayBaselineSummary {
            id: "relay-observation-must-not-be-baseline".to_owned(),
            source: "relay".to_owned(),
            ..user
        };
        assert!(persistence.upsert_relay_baseline(&invalid).is_err());

        // The selected probe path/version/hash are intentionally persisted for
        // reproducibility, while the file body never enters a persistence API.
        assert_eq!(
            persistence
                .get_relay_profile(&profile.id)
                .unwrap()
                .and_then(|value| value.private_probe_pack),
            Some(pack_reference.clone())
        );
        assert_state_files_exclude(
            &root,
            &[
                PRIVATE_API_KEY,
                PRIVATE_PROMPT,
                PRIVATE_RESPONSE,
                PRIVATE_CWD,
            ],
        );
        assert!(persistence.delete_relay_profile(&profile.id).unwrap());
        assert!(persistence
            .get_relay_profile(&profile.id)
            .unwrap()
            .is_none());
        drop(persistence);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn relay_profile_and_schedule_setting_roll_back_in_one_sqlite_transaction() {
        let root = std::env::temp_dir().join(format!(
            "xiaoli-relay-profile-transaction-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let persistence = Persistence::open(&root).unwrap();
        let profile = RelayProfile {
            id: "transaction-profile".to_owned(),
            label: "Transaction profile".to_owned(),
            normalized_base_url: "https://relay.example/v1".to_owned(),
            protocol: RelayProtocol::OpenAiResponses,
            default_model: "gpt-5.6-sol".to_owned(),
            credential_ref: None,
            private_probe_pack: None,
            created_at: "2026-08-27T01:00:00Z".to_owned(),
            updated_at: "2026-08-27T01:00:00Z".to_owned(),
        };
        persistence.set_setting("tx-schedule", "old").unwrap();
        {
            let connection = persistence.connection.lock().unwrap();
            connection
                .execute_batch(
                    "CREATE TRIGGER fail_profile_upsert
                     BEFORE INSERT ON relay_profiles
                     BEGIN SELECT RAISE(ABORT, 'forced upsert failure'); END;",
                )
                .unwrap();
        }
        assert!(persistence
            .upsert_relay_profile_with_setting(&profile, Some(("tx-schedule", "new-from-upsert")),)
            .is_err());
        assert_eq!(
            persistence.get_setting("tx-schedule").unwrap().as_deref(),
            Some("old")
        );
        assert!(persistence
            .get_relay_profile(&profile.id)
            .unwrap()
            .is_none());
        {
            let connection = persistence.connection.lock().unwrap();
            connection
                .execute_batch("DROP TRIGGER fail_profile_upsert;")
                .unwrap();
        }

        persistence.upsert_relay_profile(&profile).unwrap();
        {
            let connection = persistence.connection.lock().unwrap();
            connection
                .execute_batch(
                    "CREATE TRIGGER fail_profile_delete
                     BEFORE DELETE ON relay_profiles
                     BEGIN SELECT RAISE(ABORT, 'forced delete failure'); END;",
                )
                .unwrap();
        }
        assert!(persistence
            .delete_relay_profile_with_setting(
                &profile.id,
                Some(("tx-schedule", "new-from-delete")),
            )
            .is_err());
        assert_eq!(
            persistence.get_setting("tx-schedule").unwrap().as_deref(),
            Some("old")
        );
        assert!(persistence
            .get_relay_profile(&profile.id)
            .unwrap()
            .is_some());

        drop(persistence);
        fs::remove_dir_all(root).unwrap();
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
