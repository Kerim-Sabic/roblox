use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::Utc;
use nectarpilot_contracts::{EventEnvelope, PROFILE_SCHEMA_VERSION, Profile, RunRecord};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, backup::Backup, params};
use thiserror::Error;
use uuid::Uuid;

const DATABASE_VERSION: u32 = 4;
const MAX_CREDENTIAL_REF_LENGTH: usize = 128;
const MAX_CIPHERTEXT_LENGTH: usize = 64 * 1024;

#[derive(Debug)]
pub struct SqliteStore {
    connection: Mutex<Connection>,
    path: PathBuf,
    last_good_path: PathBuf,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let last_good_path = sidecar_path(&path, "last-good");

        if path.exists() && database_integrity(&path).is_err() {
            if !last_good_path.exists() || database_integrity(&last_good_path).is_err() {
                return Err(StoreError::CorruptDatabase(path));
            }
            fs::copy(&last_good_path, &path)?;
        }

        let mut connection = configured_connection(&path)?;
        let pre_migration = sidecar_path(&path, "pre-migration");
        if path.exists() {
            backup_connection(&connection, &pre_migration)?;
        }

        if let Err(error) = run_migrations(&mut connection) {
            drop(connection);
            if pre_migration.exists() {
                fs::copy(&pre_migration, &path)?;
            }
            return Err(error);
        }
        ensure_integrity(&connection)?;
        backup_connection(&connection, &last_good_path)?;

        Ok(Self {
            connection: Mutex::new(connection),
            path,
            last_good_path,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn save_profile(&self, profile: &Profile) -> Result<(), StoreError> {
        validate_profile(profile)?;
        let json = serde_json::to_string(profile)?;
        {
            let mut connection = self.connection.lock();
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute(
                "INSERT INTO profiles (id, schema_version, name, updated_at, document)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET
                   schema_version = excluded.schema_version,
                   name = excluded.name,
                   updated_at = excluded.updated_at,
                   document = excluded.document",
                params![
                    profile.id.to_string(),
                    profile.schema_version,
                    profile.name,
                    profile.updated_at,
                    json
                ],
            )?;
            transaction.commit()?;
            backup_connection(&connection, &self.last_good_path)?;
        }
        Ok(())
    }

    pub fn load_profile(&self, id: Uuid) -> Result<Option<Profile>, StoreError> {
        let connection = self.connection.lock();
        let document: Option<String> = connection
            .query_row(
                "SELECT document FROM profiles WHERE id = ?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        document
            .map(|json| serde_json::from_str(&json).map_err(StoreError::from))
            .transpose()
    }

    pub fn list_profiles(&self) -> Result<Vec<Profile>, StoreError> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT document FROM profiles ORDER BY updated_at DESC, name COLLATE NOCASE",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut profiles = Vec::new();
        for row in rows {
            profiles.push(serde_json::from_str(&row?)?);
        }
        Ok(profiles)
    }

    pub fn delete_profile(&self, id: Uuid) -> Result<bool, StoreError> {
        let connection = self.connection.lock();
        let changed = connection.execute("DELETE FROM profiles WHERE id = ?1", [id.to_string()])?;
        backup_connection(&connection, &self.last_good_path)?;
        Ok(changed > 0)
    }

    pub fn export_profile_json(&self, id: Uuid) -> Result<String, StoreError> {
        let profile = self
            .load_profile(id)?
            .ok_or(StoreError::ProfileNotFound(id))?;
        Ok(serde_json::to_string_pretty(&profile)?)
    }

    pub fn import_profile_json(&self, json: &str) -> Result<Profile, StoreError> {
        let profile: Profile = serde_json::from_str(json)?;
        self.save_profile(&profile)?;
        Ok(profile)
    }

    pub fn append_event(&self, event: &EventEnvelope) -> Result<(), StoreError> {
        let document = serde_json::to_string(event)?;
        let connection = self.connection.lock();
        connection.execute(
            "INSERT INTO event_log (run_id, sequence, timestamp, event_type, document)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                event.run_id.to_string(),
                i64::try_from(event.sequence).unwrap_or(i64::MAX),
                event.timestamp,
                event_type_name(event),
                document
            ],
        )?;
        Ok(())
    }

    pub fn recent_events(
        &self,
        run_id: Uuid,
        limit: usize,
    ) -> Result<Vec<EventEnvelope>, StoreError> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT document FROM event_log WHERE run_id = ?1 ORDER BY sequence DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![run_id.to_string(), i64::try_from(limit).unwrap_or(i64::MAX)],
            |row| row.get::<_, String>(0),
        )?;
        let mut events = Vec::new();
        for row in rows {
            events.push(serde_json::from_str(&row?)?);
        }
        events.reverse();
        Ok(events)
    }

    pub fn set_runtime_value(&self, key: &str, value: &str) -> Result<(), StoreError> {
        let connection = self.connection.lock();
        connection.execute(
            "INSERT INTO runtime_state (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn runtime_value(&self, key: &str) -> Result<Option<String>, StoreError> {
        let connection = self.connection.lock();
        Ok(connection
            .query_row(
                "SELECT value FROM runtime_state WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Stores only ciphertext produced by the platform secret protector.
    ///
    /// The persistence layer deliberately cannot encrypt or decrypt values, so
    /// credentials cannot leak into portable profile exports.
    pub fn store_encrypted_secret(
        &self,
        credential_ref: &str,
        ciphertext: &[u8],
    ) -> Result<(), StoreError> {
        validate_credential_ref(credential_ref)?;
        if ciphertext.is_empty() || ciphertext.len() > MAX_CIPHERTEXT_LENGTH {
            return Err(StoreError::InvalidSecret(
                "ciphertext must contain between 1 byte and 64 KiB".into(),
            ));
        }
        let mut connection = self.connection.lock();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO encrypted_credentials (credential_ref, ciphertext, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(credential_ref) DO UPDATE SET
               ciphertext = excluded.ciphertext,
               updated_at = excluded.updated_at",
            params![credential_ref, ciphertext, Utc::now().to_rfc3339()],
        )?;
        transaction.commit()?;
        backup_connection(&connection, &self.last_good_path)?;
        Ok(())
    }

    pub fn load_encrypted_secret(
        &self,
        credential_ref: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        validate_credential_ref(credential_ref)?;
        let connection = self.connection.lock();
        Ok(connection
            .query_row(
                "SELECT ciphertext FROM encrypted_credentials WHERE credential_ref = ?1",
                [credential_ref],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn delete_encrypted_secret(&self, credential_ref: &str) -> Result<bool, StoreError> {
        validate_credential_ref(credential_ref)?;
        let connection = self.connection.lock();
        let changed = connection.execute(
            "DELETE FROM encrypted_credentials WHERE credential_ref = ?1",
            [credential_ref],
        )?;
        if changed > 0 {
            backup_connection(&connection, &self.last_good_path)?;
        }
        Ok(changed > 0)
    }

    /// Creates a consistent `SQLite` backup suitable for pre-update rollback.
    pub fn backup_to(&self, destination: impl AsRef<Path>) -> Result<(), StoreError> {
        let connection = self.connection.lock();
        backup_connection(&connection, destination.as_ref())
    }

    pub fn record_run(&self, record: &RunRecord) -> Result<(), StoreError> {
        let connection = self.connection.lock();
        connection.execute(
            "INSERT INTO run_history (run_id, profile_id, kind, started_at, finished_at,\
             final_state, summary, steps_succeeded, steps_failed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(run_id) DO UPDATE SET
               finished_at = excluded.finished_at,
               final_state = excluded.final_state,
               summary = excluded.summary,
               steps_succeeded = excluded.steps_succeeded,
               steps_failed = excluded.steps_failed",
            params![
                record.run_id.to_string(),
                record.profile_id.to_string(),
                record.kind,
                record.started_at.to_rfc3339(),
                record.finished_at.to_rfc3339(),
                record.final_state,
                record.summary,
                record.steps_succeeded,
                record.steps_failed,
            ],
        )?;
        Ok(())
    }

    pub fn list_run_records(&self, limit: u32) -> Result<Vec<RunRecord>, StoreError> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT run_id, profile_id, kind, started_at, finished_at, final_state,\
             summary, steps_succeeded, steps_failed
             FROM run_history ORDER BY finished_at DESC LIMIT ?1",
        )?;
        let rows = statement.query_map([limit.min(500)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, u32>(7)?,
                row.get::<_, u32>(8)?,
            ))
        })?;
        let mut records = Vec::new();
        for row in rows {
            let (run_id, profile_id, kind, started_at, finished_at, final_state, summary, ok, bad) =
                row?;
            let (Ok(run_id), Ok(profile_id)) =
                (Uuid::parse_str(&run_id), Uuid::parse_str(&profile_id))
            else {
                continue;
            };
            let (Ok(started_at), Ok(finished_at)) = (
                chrono::DateTime::parse_from_rfc3339(&started_at),
                chrono::DateTime::parse_from_rfc3339(&finished_at),
            ) else {
                continue;
            };
            records.push(RunRecord {
                run_id,
                profile_id,
                kind,
                started_at: started_at.with_timezone(&Utc),
                finished_at: finished_at.with_timezone(&Utc),
                final_state,
                summary,
                steps_succeeded: ok,
                steps_failed: bad,
            });
        }
        Ok(records)
    }
}

fn validate_profile(profile: &Profile) -> Result<(), StoreError> {
    if profile.schema_version != PROFILE_SCHEMA_VERSION {
        return Err(StoreError::UnsupportedProfileVersion {
            received: profile.schema_version,
            supported: PROFILE_SCHEMA_VERSION,
        });
    }
    if profile.name.trim().is_empty() {
        return Err(StoreError::InvalidProfile("name cannot be empty".into()));
    }
    for (extension, hash) in &profile.trusted_extensions {
        if extension.trim().is_empty() {
            return Err(StoreError::InvalidProfile(
                "trusted extension identifier cannot be empty".into(),
            ));
        }
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(StoreError::InvalidProfile(format!(
                "trusted extension {extension:?} must have a full SHA-256 hash"
            )));
        }
    }
    Ok(())
}

fn validate_credential_ref(credential_ref: &str) -> Result<(), StoreError> {
    if credential_ref.is_empty()
        || credential_ref.len() > MAX_CREDENTIAL_REF_LENGTH
        || !credential_ref
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(StoreError::InvalidSecret(
            "credential reference must be 1-128 ASCII letters, digits, '.', '_', ':', or '-'"
                .into(),
        ));
    }
    Ok(())
}

fn configured_connection(path: &Path) -> Result<Connection, StoreError> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;\
         PRAGMA journal_mode = DELETE;\
         PRAGMA synchronous = FULL;",
    )?;
    Ok(connection)
}

fn run_migrations(connection: &mut Connection) -> Result<(), StoreError> {
    let current: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if current > DATABASE_VERSION {
        return Err(StoreError::FutureDatabaseVersion {
            received: current,
            supported: DATABASE_VERSION,
        });
    }
    for target in (current + 1)..=DATABASE_VERSION {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let migration_result = match target {
            1 => transaction.execute_batch(
                "CREATE TABLE profiles (\
                    id TEXT PRIMARY KEY NOT NULL,\
                    schema_version INTEGER NOT NULL,\
                    name TEXT NOT NULL,\
                    updated_at TEXT NOT NULL,\
                    document TEXT NOT NULL\
                 );\
                 CREATE TABLE event_log (\
                    id INTEGER PRIMARY KEY AUTOINCREMENT,\
                    run_id TEXT NOT NULL,\
                    sequence INTEGER NOT NULL,\
                    timestamp TEXT NOT NULL,\
                    event_type TEXT NOT NULL,\
                    document TEXT NOT NULL,\
                    UNIQUE(run_id, sequence)\
                 );",
            ),
            2 => transaction.execute_batch(
                "CREATE TABLE runtime_state (\
                    key TEXT PRIMARY KEY NOT NULL,\
                    value TEXT NOT NULL\
                 );\
                 CREATE INDEX profiles_updated_at_idx ON profiles(updated_at);\
                 CREATE INDEX event_log_timestamp_idx ON event_log(timestamp);",
            ),
            3 => transaction.execute_batch(
                "CREATE TABLE encrypted_credentials (\
                    credential_ref TEXT PRIMARY KEY NOT NULL,\
                    ciphertext BLOB NOT NULL,\
                    updated_at TEXT NOT NULL\
                 );",
            ),
            4 => transaction.execute_batch(
                "CREATE TABLE run_history (\
                    run_id TEXT PRIMARY KEY NOT NULL,\
                    profile_id TEXT NOT NULL,\
                    kind TEXT NOT NULL,\
                    started_at TEXT NOT NULL,\
                    finished_at TEXT NOT NULL,\
                    final_state TEXT NOT NULL,\
                    summary TEXT NOT NULL,\
                    steps_succeeded INTEGER NOT NULL,\
                    steps_failed INTEGER NOT NULL\
                 );\
                 CREATE INDEX run_history_finished_idx ON run_history(finished_at);",
            ),
            _ => unreachable!("bounded by DATABASE_VERSION"),
        };
        if let Err(source) = migration_result {
            return Err(StoreError::Migration {
                version: target,
                source,
            });
        }
        transaction.pragma_update(None, "user_version", target)?;
        transaction.commit()?;
    }
    Ok(())
}

fn ensure_integrity(connection: &Connection) -> Result<(), StoreError> {
    let result: String = connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if result == "ok" {
        Ok(())
    } else {
        Err(StoreError::IntegrityCheck(result))
    }
}

fn database_integrity(path: &Path) -> Result<(), StoreError> {
    let connection = Connection::open(path)?;
    ensure_integrity(&connection)
}

fn backup_connection(connection: &Connection, destination: &Path) -> Result<(), StoreError> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = destination.with_extension("backup-in-progress");
    if temporary.exists() {
        fs::remove_file(&temporary)?;
    }
    let mut output = Connection::open(&temporary)?;
    let backup = Backup::new(connection, &mut output)?;
    backup.run_to_completion(16, Duration::from_millis(10), None)?;
    drop(backup);
    drop(output);
    if destination.exists() {
        fs::remove_file(destination)?;
    }
    fs::rename(temporary, destination)?;
    Ok(())
}

fn sidecar_path(path: &Path, label: &str) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("nectarpilot.sqlite3");
    path.with_file_name(format!("{file_name}.{label}.sqlite3"))
}

fn event_type_name(event: &EventEnvelope) -> &'static str {
    use nectarpilot_contracts::DaemonEvent;
    match &event.event {
        DaemonEvent::CommandAccepted { .. } => "command_accepted",
        DaemonEvent::CommandRejected { .. } => "command_rejected",
        DaemonEvent::StateChanged { .. } => "state_changed",
        DaemonEvent::ActionCompleted(_) => "action_completed",
        DaemonEvent::ReconnectProgress(_) => "reconnect_progress",
        DaemonEvent::Log { .. } => "log",
        DaemonEvent::Snapshot(_) => "snapshot",
        DaemonEvent::ProfileSaved { .. } => "profile_saved",
        DaemonEvent::Profiles { .. } => "profiles",
        DaemonEvent::ProfileSelected { .. } => "profile_selected",
        DaemonEvent::ProfileDeleted { .. } => "profile_deleted",
        DaemonEvent::ProfileExported { .. } => "profile_exported",
        DaemonEvent::SafeModeEntered { .. } => "safe_mode_entered",
        DaemonEvent::ShutdownReady { .. } => "shutdown_ready",
        DaemonEvent::SessionProgress(_) => "session_progress",
        DaemonEvent::LegacyInspection(_) => "legacy_inspection",
        DaemonEvent::SecretStored { .. } => "secret_stored",
        DaemonEvent::RunHistory { .. } => "run_history",
        DaemonEvent::StatsSample(_) => "stats_sample",
    }
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQLite error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("database migration {version} failed: {source}")]
    Migration {
        version: u32,
        #[source]
        source: rusqlite::Error,
    },
    #[error("database integrity check failed: {0}")]
    IntegrityCheck(String),
    #[error("database is corrupt and no valid last-good backup exists: {0}")]
    CorruptDatabase(PathBuf),
    #[error("database version {received} is newer than supported version {supported}")]
    FutureDatabaseVersion { received: u32, supported: u32 },
    #[error("profile schema {received} is unsupported; expected {supported}")]
    UnsupportedProfileVersion { received: u32, supported: u32 },
    #[error("invalid profile: {0}")]
    InvalidProfile(String),
    #[error("profile {0} was not found")]
    ProfileNotFound(Uuid),
    #[error("invalid encrypted secret: {0}")]
    InvalidSecret(String),
}

#[cfg(test)]
mod tests {
    use std::fs;

    use nectarpilot_contracts::Profile;
    use rusqlite::Connection;
    use tempfile::tempdir;

    use super::SqliteStore;

    #[test]
    fn profiles_round_trip_and_export() {
        let directory = tempdir().expect("temp directory");
        let store = SqliteStore::open(directory.path().join("state.sqlite3")).expect("store");
        let profile = Profile::new("Main");
        store.save_profile(&profile).expect("save");
        assert_eq!(
            store.load_profile(profile.id).expect("load"),
            Some(profile.clone())
        );
        let exported = store.export_profile_json(profile.id).expect("export");
        assert_eq!(
            serde_json::from_str::<Profile>(&exported).expect("valid export"),
            profile
        );
    }

    #[test]
    fn failed_corrupt_migration_restores_pre_migration_database() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("state.sqlite3");
        {
            let connection = Connection::open(&path).expect("database");
            connection
                .execute_batch("CREATE TABLE unrelated(value TEXT); PRAGMA user_version = 1;")
                .expect("old malformed schema");
        }

        let error = SqliteStore::open(&path).expect_err("migration must fail");
        assert!(error.to_string().contains("migration 2 failed"));

        let restored = Connection::open(&path).expect("restored database");
        let version: u32 = restored
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, 1);
        let unrelated: String = restored
            .query_row(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='unrelated'",
                [],
                |row| row.get(0),
            )
            .expect("original content preserved");
        assert_eq!(unrelated, "unrelated");

        // The recovery file is itself a valid SQLite database, not a byte dump.
        let pre_migration = path.with_file_name("state.sqlite3.pre-migration.sqlite3");
        assert!(fs::metadata(pre_migration).is_ok());
    }

    #[test]
    fn extension_trust_requires_a_full_sha256_hash() {
        let directory = tempdir().expect("temp directory");
        let store = SqliteStore::open(directory.path().join("state.sqlite3")).expect("store");
        let mut profile = Profile::new("Main");
        profile
            .trusted_extensions
            .insert("my-extension".into(), "not-a-hash".into());
        let error = store
            .save_profile(&profile)
            .expect_err("invalid trust hash");
        assert!(error.to_string().contains("full SHA-256 hash"));
    }

    #[test]
    fn encrypted_credentials_round_trip_outside_profile_exports() {
        let directory = tempdir().expect("temp directory");
        let store = SqliteStore::open(directory.path().join("state.sqlite3")).expect("store");
        let profile = Profile::new("Main");
        store.save_profile(&profile).expect("save profile");

        let ciphertext = b"opaque-dpapi-ciphertext";
        store
            .store_encrypted_secret("discord:webhook", ciphertext)
            .expect("store ciphertext");
        assert_eq!(
            store
                .load_encrypted_secret("discord:webhook")
                .expect("load ciphertext"),
            Some(ciphertext.to_vec())
        );

        let exported = store
            .export_profile_json(profile.id)
            .expect("export profile");
        assert!(!exported.contains("discord:webhook"));
        assert!(!exported.contains("opaque-dpapi-ciphertext"));

        assert!(
            store
                .delete_encrypted_secret("discord:webhook")
                .expect("delete ciphertext")
        );
        assert_eq!(
            store
                .load_encrypted_secret("discord:webhook")
                .expect("load deleted ciphertext"),
            None
        );
    }

    #[test]
    fn encrypted_credentials_reject_unbounded_or_unsafe_values() {
        let directory = tempdir().expect("temp directory");
        let store = SqliteStore::open(directory.path().join("state.sqlite3")).expect("store");
        assert!(
            store
                .store_encrypted_secret("bad ref", b"ciphertext")
                .is_err()
        );
        assert!(
            store
                .store_encrypted_secret("discord:webhook", &[])
                .is_err()
        );
        let oversized = vec![0_u8; super::MAX_CIPHERTEXT_LENGTH + 1];
        assert!(
            store
                .store_encrypted_secret("discord:webhook", &oversized)
                .is_err()
        );
    }
}
