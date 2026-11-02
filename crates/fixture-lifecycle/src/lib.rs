//! Backend-only lifecycle for five disposable example-plan files.
//!
//! This deliberately has no path-taking mutation methods, no deletion method,
//! and no cleanup-on-drop behavior. The caller must be trusted backend code.

use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use disk_steward_core::{ActionId, AttemptId, ReceiptId, SessionId};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(test)]
thread_local! {
    static PURGE_CRASH_BOUNDARY: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn purge_crash_checkpoint(boundary: &'static str) {
    PURGE_CRASH_BOUNDARY.with(|selected| {
        if selected.get() == Some(boundary) {
            std::process::exit(86);
        }
    });
}

const FIXTURES: [(&str, u64); 5] = [
    ("compiler-output", 3_145_728),
    ("test-cache", 1_048_576),
    ("preview-cache", 524_288),
    ("package-index", 262_144),
    ("incremental-output", 131_072),
];
const MAX_FIXTURE_BYTES: u64 = 6 * 1024 * 1024;
const ROOT_PREFIX: &str = ".disk-steward-fixtures-";
const DB_NAME: &str = "lifecycle.sqlite3";
const DB_APPLICATION_ID: i64 = 0x4453_464c;
const DB_SCHEMA_VERSION: i64 = 2;
const LEGACY_SCHEMA_VERSION: i64 = 1;
const MANIFEST_SCHEMA: &str = "CREATE TABLE manifest(singleton INTEGER PRIMARY KEY CHECK(singleton=1), session_id TEXT NOT NULL, parent_dev INTEGER NOT NULL,parent_ino INTEGER NOT NULL,root_dev INTEGER NOT NULL,root_ino INTEGER NOT NULL,source_dev INTEGER NOT NULL,source_ino INTEGER NOT NULL,holding_dev INTEGER NOT NULL,holding_ino INTEGER NOT NULL) STRICT";
const FIXTURES_SCHEMA_V1: &str = "CREATE TABLE fixtures(action_id TEXT PRIMARY KEY,slot TEXT NOT NULL UNIQUE,bytes INTEGER NOT NULL,sha256 TEXT NOT NULL,dev INTEGER NOT NULL,ino INTEGER NOT NULL,status TEXT NOT NULL CHECK(status IN ('available','staged','restored','uncertain')),receipt_id TEXT UNIQUE) STRICT";
const ATTEMPTS_SCHEMA_V1: &str = "CREATE TABLE attempts(attempt_id TEXT PRIMARY KEY,action_id TEXT NOT NULL,operation TEXT NOT NULL CHECK(operation IN ('stage','restore')),receipt_id TEXT,outcome TEXT NOT NULL CHECK(outcome IN ('intent','staged','restored','collision','uncertain'))) STRICT";
const FIXTURES_SCHEMA: &str = "CREATE TABLE fixtures(action_id TEXT PRIMARY KEY,slot TEXT NOT NULL UNIQUE,bytes INTEGER NOT NULL,sha256 TEXT NOT NULL,dev INTEGER NOT NULL,ino INTEGER NOT NULL,status TEXT NOT NULL CHECK(status IN ('available','staged','restored','uncertain','permanently_removed')),receipt_id TEXT UNIQUE) STRICT";
const ATTEMPTS_SCHEMA: &str = "CREATE TABLE attempts(attempt_id TEXT PRIMARY KEY,action_id TEXT NOT NULL,operation TEXT NOT NULL CHECK(operation IN ('stage','restore','purge')),receipt_id TEXT,outcome TEXT NOT NULL CHECK(outcome IN ('intent','staged','restored','collision','uncertain','permanently_removed'))) STRICT";
const MAX_ATTEMPTS: i64 = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureStatus {
    Available,
    Staged,
    Restored,
    PermanentlyRemoved,
    Uncertain,
    Changed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureSnapshot {
    pub action_id: ActionId,
    pub receipt_id: Option<ReceiptId>,
    pub status: FixtureStatus,
    pub bytes: u64,
    pub sha256: String,
    pub identity_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub session_id: SessionId,
    pub fixtures: Vec<FixtureSnapshot>,
}

#[derive(Debug)]
pub enum LifecycleError {
    UnsupportedPlatform,
    Io(std::io::Error),
    Database(rusqlite::Error),
    InvalidBindings,
    InvalidAttempt,
    DuplicateAttempt,
    UnknownAction,
    UnknownReceipt,
    WrongState,
    Collision,
    Changed,
    Uncertain,
    InvalidSession,
}

impl From<std::io::Error> for LifecycleError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<rusqlite::Error> for LifecycleError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

/// Outcome of one receipt-addressed purge attempt (contract
/// `fixture-purge-v1`). `NotAttempted` is the batch-layer outcome for items
/// whose unlink intent was never claimed; this crate reports refusals as
/// typed errors instead.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PurgeItemOutcome {
    NotAttempted {
        receipt_id: ReceiptId,
    },
    PermanentlyRemoved {
        receipt_id: ReceiptId,
        logical_bytes: u64,
    },
    Uncertain {
        receipt_id: ReceiptId,
    },
}

/// A process-local handle to one internally created fixture session.
pub struct FixtureLifecycle {
    id: SessionId,
    parent_path: PathBuf,
    root_path: PathBuf,
    // Same gate as the tests module below: this field exists only where the
    // tests that read it are compiled, so the lib-test target on other
    // hosts does not carry an unread field.
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    root: PathBuf,
    parent: File,
    root_dir: File,
    source_dir: File,
    holding_dir: File,
    db: Connection,
}

impl FixtureLifecycle {
    /// Create exactly five fixed-size synthetic regular files under a fresh
    /// private session directory beneath an app-owned parent.
    pub fn create(
        parent: &Path,
        actions: [ActionId; 5],
    ) -> Result<(Self, SessionSnapshot), LifecycleError> {
        Self::create_for_session(parent, SessionId::new(), actions)
    }

    /// Create the fixed fixture set for a coordinator-persisted session ID.
    /// The destination root is always exclusive; an existing session is never
    /// reopened, overwritten, or cleaned up by this factory.
    pub fn create_for_session(
        parent: &Path,
        id: SessionId,
        actions: [ActionId; 5],
    ) -> Result<(Self, SessionSnapshot), LifecycleError> {
        supported()?;
        if !unique_actions(&actions) {
            return Err(LifecycleError::InvalidBindings);
        }
        let parent_path = fs::canonicalize(parent)?;
        let parent_file = File::open(&parent_path)?;
        if !parent_file.metadata()?.is_dir() {
            return Err(LifecycleError::InvalidSession);
        }

        let root = parent_path.join(format!("{ROOT_PREFIX}{}", typed_component(&id)?));
        create_private_dir(&root)?;
        let result = (|| {
            let source_path = root.join("source");
            let holding_path = root.join("holding");
            create_private_dir(&source_path)?;
            create_private_dir(&holding_path)?;
            let root_dir = File::open(&root)?;
            let source_dir = File::open(&source_path)?;
            let holding_dir = File::open(&holding_path)?;
            let db_path = root.join(DB_NAME);
            if fs::symlink_metadata(&db_path).is_ok() {
                return Err(LifecycleError::InvalidSession);
            }
            let database_file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&db_path)?;
            set_private_file(&database_file)?;
            let mut db = Connection::open_with_flags(
                &db_path,
                OpenFlags::SQLITE_OPEN_READ_WRITE
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX
                    | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )?;
            configure_db(&db, true)?;
            initialize_schema(&db)?;
            let (parent_dev, parent_ino) = identity(&parent_file)?;
            let (root_dev, root_ino) = identity(&root_dir)?;
            let (source_dev, source_ino) = identity(&source_dir)?;
            let (holding_dev, holding_ino) = identity(&holding_dir)?;
            db.execute(
                "INSERT INTO manifest VALUES(1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    typed_wire(&id)?,
                    parent_dev,
                    parent_ino,
                    root_dev,
                    root_ino,
                    source_dev,
                    source_ino,
                    holding_dev,
                    holding_ino
                ],
            )?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            for (index, ((slot, bytes), action)) in FIXTURES.iter().zip(actions.iter()).enumerate()
            {
                let file_path = source_path.join(slot);
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&file_path)?;
                set_private_file(&file)?;
                let digest = write_fixture(&mut file, index, *bytes)?;
                file.sync_all()?;
                let (dev, ino, nlink) = file_identity(&file)?;
                if nlink != 1 {
                    return Err(LifecycleError::Changed);
                }
                tx.execute("INSERT INTO fixtures(action_id, slot, bytes, sha256, dev, ino, status) VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'available')", params![encode_action(action)?, slot, *bytes as i64, digest, dev, ino])?;
            }
            tx.commit()?;
            sync_dir(&source_dir)?;
            sync_dir(&root_dir)?;
            Ok(Self {
                id,
                parent_path: parent_path.clone(),
                root_path: root.clone(),
                #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
                root,
                parent: parent_file,
                root_dir,
                source_dir,
                holding_dir,
                db,
            })
        })();
        // On setup failure preserve the partially created evidence for explicit
        // inspection; never recursively remove a session tree.
        let lifecycle = result?;
        sync_dir(&lifecycle.parent)?;
        let snapshot = lifecycle.snapshot()?;
        Ok((lifecycle, snapshot))
    }

    /// Reopen only the unique internally named session corresponding to id.
    /// Existing journal states are reported as stored; no operation is retried.
    pub fn reopen(parent: &Path, id: SessionId) -> Result<(Self, SessionSnapshot), LifecycleError> {
        supported()?;
        let parent_path = fs::canonicalize(parent)?;
        let parent = open_dir_path(&parent_path)?;
        let root = parent_path.join(format!("{ROOT_PREFIX}{}", typed_component(&id)?));
        let root_name = root.file_name().ok_or(LifecycleError::InvalidSession)?;
        let root_dir = open_dir_at(&parent, root_name)?;
        let source_dir = open_dir_at(&root_dir, OsStr::new("source"))?;
        let holding_dir = open_dir_at(&root_dir, OsStr::new("holding"))?;
        let db = Connection::open_with_flags(
            root.join(DB_NAME),
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        configure_db(&db, false)?;
        migrate_legacy_v1(&db, id, &parent, &root_dir, &source_dir, &holding_dir)?;
        validate_schema(&db)?;
        validate_manifest(&db, id, &parent, &root_dir, &source_dir, &holding_dir)?;
        let lifecycle = Self {
            id,
            parent_path,
            root_path: root.clone(),
            #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
            root,
            parent,
            root_dir,
            source_dir,
            holding_dir,
            db,
        };
        // Reconcile evidence into snapshot status only. No blind retries.
        let snapshot = lifecycle.snapshot()?;
        Ok((lifecycle, snapshot))
    }

    pub fn session_id(&self) -> SessionId {
        self.id
    }

    /// Whether an attempt intent is journaled for this action but its
    /// terminal outcome is not yet durable. The purge coordinator consults
    /// this when mapping errors: an intent-bearing failure is uncertainty,
    /// not a clean refusal.
    pub fn pending_intent_for(&self, action: ActionId) -> Result<bool, LifecycleError> {
        let encoded = encode_action(&action)?;
        self.has_pending_intent(&encoded)
    }

    /// Stage an internally registered fixture by ActionId. No path is accepted.
    pub fn stage(
        &mut self,
        action: ActionId,
        attempt: AttemptId,
    ) -> Result<ReceiptId, LifecycleError> {
        validate_schema(&self.db)?;
        validate_registry(&self.db)?;
        let fixture = self
            .lookup_action(action)?
            .ok_or(LifecycleError::UnknownAction)?;
        if self.has_pending_intent(&fixture.action_id)? {
            return Err(LifecycleError::Uncertain);
        }
        if fixture.status != "available" && fixture.status != "restored" {
            return Err(LifecycleError::WrongState);
        }
        self.verify_fixture(&fixture, &self.source_dir, &fixture.slot)?;
        self.verify_directories()?;
        let receipt = ReceiptId::new();
        self.record_intent(attempt, &fixture, "stage", Some(receipt))?;
        match rename_no_replace(
            &self.source_dir,
            &fixture.slot,
            &self.holding_dir,
            &receipt_slot(receipt)?,
        ) {
            Ok(()) => {
                if sync_dir(&self.source_dir).is_err() || sync_dir(&self.holding_dir).is_err() {
                    return Err(self.uncertain_error(attempt, &fixture.action_id));
                }
                let checked =
                    self.verify_fixture(&fixture, &self.holding_dir, &receipt_slot(receipt)?);
                if checked.is_err() {
                    return Err(self.uncertain_error(attempt, &fixture.action_id));
                }
                if self
                    .record_outcome(attempt, &fixture.action_id, "staged", Some(receipt))
                    .is_err()
                {
                    let _ = self.mark_uncertain(attempt, &fixture.action_id);
                    return Err(LifecycleError::Uncertain);
                }
                Ok(receipt)
            }
            Err(RenameFailure::Collision) => {
                self.record_outcome(attempt, &fixture.action_id, "collision", None)?;
                Err(LifecycleError::Collision)
            }
            Err(RenameFailure::Uncertain) => Err(self.uncertain_error(attempt, &fixture.action_id)),
            Err(RenameFailure::Unsupported) => {
                Err(self.uncertain_error(attempt, &fixture.action_id))
            }
            Err(RenameFailure::InvalidComponent) => {
                Err(self.uncertain_error(attempt, &fixture.action_id))
            }
        }
    }

    /// Restore a registered receipt to its fixed source slot, atomically
    /// refusing an occupied original name.
    pub fn restore(
        &mut self,
        receipt: ReceiptId,
        attempt: AttemptId,
    ) -> Result<(), LifecycleError> {
        validate_schema(&self.db)?;
        validate_registry(&self.db)?;
        let fixture = self
            .lookup_receipt(receipt)?
            .ok_or(LifecycleError::UnknownReceipt)?;
        if self.has_pending_intent(&fixture.action_id)? {
            return Err(LifecycleError::Uncertain);
        }
        if fixture.status != "staged" {
            return Err(LifecycleError::WrongState);
        }
        let from = receipt_slot(receipt)?;
        self.verify_fixture(&fixture, &self.holding_dir, &from)?;
        self.verify_directories()?;
        self.record_intent(attempt, &fixture, "restore", Some(receipt))?;
        match rename_no_replace(&self.holding_dir, &from, &self.source_dir, &fixture.slot) {
            Ok(()) => {
                if sync_dir(&self.source_dir).is_err() || sync_dir(&self.holding_dir).is_err() {
                    return Err(self.uncertain_error(attempt, &fixture.action_id));
                }
                if self
                    .verify_fixture(&fixture, &self.source_dir, &fixture.slot)
                    .is_err()
                {
                    return Err(self.uncertain_error(attempt, &fixture.action_id));
                }
                if self
                    .record_outcome(attempt, &fixture.action_id, "restored", Some(receipt))
                    .is_err()
                {
                    let _ = self.mark_uncertain(attempt, &fixture.action_id);
                    return Err(LifecycleError::Uncertain);
                }
                Ok(())
            }
            Err(RenameFailure::Collision) => {
                self.record_outcome(attempt, &fixture.action_id, "collision", Some(receipt))?;
                Err(LifecycleError::Collision)
            }
            Err(RenameFailure::Uncertain) => Err(self.uncertain_error(attempt, &fixture.action_id)),
            Err(RenameFailure::Unsupported) => {
                Err(self.uncertain_error(attempt, &fixture.action_id))
            }
            Err(RenameFailure::InvalidComponent) => {
                Err(self.uncertain_error(attempt, &fixture.action_id))
            }
        }
    }

    /// Permanently remove one staged fixture by its registered receipt.
    /// The only address is the receipt within this open session; the unlink
    /// is directory-relative through the already-opened holding-directory
    /// handle after a full identity re-verification, so no caller path is
    /// ever accepted.
    ///
    /// Residual same-user TOCTOU, per contract fixture-purge-v1: the
    /// verification and the directory-relative unlink are bound by name, not
    /// atomically by inode; portable filesystems provide no "unlink only if
    /// this name still has this inode" primitive. A same-user process able
    /// to mutate the private fixture directory can race the verification and
    /// the unlink. The mitigations are the restrictive directory
    /// ownership/mode, no-follow opened directory handles, and verification
    /// twice — once before the intent is journaled and again immediately
    /// before the unlink; when the outcome cannot be established, the
    /// receipt state becomes Uncertain rather than guessed.
    pub fn purge_receipt(
        &mut self,
        receipt: ReceiptId,
        attempt: AttemptId,
    ) -> Result<PurgeItemOutcome, LifecycleError> {
        // Belt-and-braces for the destructive operation: no lifecycle
        // instance can exist on unsupported hosts (create/reopen gate on
        // this), but the purge path refuses before anything else regardless.
        supported()?;
        validate_schema(&self.db)?;
        validate_registry(&self.db)?;
        let fixture = self
            .lookup_receipt(receipt)?
            .ok_or(LifecycleError::UnknownReceipt)?;
        if self.has_pending_intent(&fixture.action_id)? {
            return Err(LifecycleError::Uncertain);
        }
        if fixture.status != "staged" {
            return Err(LifecycleError::WrongState);
        }
        let slot = receipt_slot(receipt)?;
        self.verify_fixture(&fixture, &self.holding_dir, &slot)?;
        self.verify_directories()?;
        self.record_intent(attempt, &fixture, "purge", Some(receipt))?;
        #[cfg(test)]
        purge_crash_checkpoint("intent");
        self.complete_purge(receipt, attempt, &fixture, &slot)
    }

    /// Recheck, unlink, sync, and journal the terminal outcome. Split from
    /// `purge_receipt` so each platform compiles exactly its own variant;
    /// both are tail expressions, mirroring `open_regular`.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn complete_purge(
        &mut self,
        receipt: ReceiptId,
        attempt: AttemptId,
        fixture: &Expected,
        slot: &str,
    ) -> Result<PurgeItemOutcome, LifecycleError> {
        use rustix::fs::{unlinkat, AtFlags};
        // Contract: recheck immediately before the unlink. The intent write
        // takes time, so the verified identity is re-established here; a
        // failure means the receipt mutated between verification and unlink
        // and the outcome can no longer be established.
        if self
            .verify_fixture(fixture, &self.holding_dir, slot)
            .is_err()
        {
            return Err(self.uncertain_error(attempt, &fixture.action_id));
        }
        if unlinkat(&self.holding_dir, slot, AtFlags::empty()).is_err() {
            // The unlink may or may not have occurred; the result cannot be
            // established, so the receipt state becomes Uncertain.
            return Err(self.uncertain_error(attempt, &fixture.action_id));
        }
        #[cfg(test)]
        purge_crash_checkpoint("unlink");
        if sync_dir(&self.holding_dir).is_err() {
            return Err(self.uncertain_error(attempt, &fixture.action_id));
        }
        #[cfg(test)]
        purge_crash_checkpoint("directory_sync");
        if self
            .record_outcome(
                attempt,
                &fixture.action_id,
                "permanently_removed",
                Some(receipt),
            )
            .is_err()
        {
            return Err(self.uncertain_error(attempt, &fixture.action_id));
        }
        #[cfg(test)]
        purge_crash_checkpoint("terminal");
        Ok(PurgeItemOutcome::PermanentlyRemoved {
            receipt_id: receipt,
            logical_bytes: fixture.bytes,
        })
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn complete_purge(
        &mut self,
        receipt: ReceiptId,
        attempt: AttemptId,
        fixture: &Expected,
        slot: &str,
    ) -> Result<PurgeItemOutcome, LifecycleError> {
        // Unreachable in practice: no lifecycle instance can exist on
        // unsupported hosts. If one ever did, the journaled intent must
        // become uncertain — no platform may claim success it cannot
        // establish, and none of these hosts has an unlink implementation.
        let _ = (receipt, slot);
        Err(self.uncertain_error(attempt, &fixture.action_id))
    }

    pub fn snapshot(&self) -> Result<SessionSnapshot, LifecycleError> {
        validate_schema(&self.db)?;
        validate_registry(&self.db)?;
        self.verify_directories()?;
        let directory_bindings = self.directory_bindings()?;
        let mut stmt = self.db.prepare("SELECT action_id, slot, bytes, sha256, dev, ino, status, receipt_id FROM fixtures ORDER BY rowid")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? as u64,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })?;
        let mut fixtures = Vec::new();
        for row in rows {
            let (action, slot, bytes, sha, dev, ino, state, stored_receipt) = row?;
            let pending_receipt: Option<String> = self.db.query_row(
                "SELECT receipt_id FROM attempts WHERE action_id=?1 AND outcome='intent' ORDER BY rowid DESC LIMIT 1",
                [&action],
                |r| r.get(0),
            ).optional()?.flatten();
            let receipt = pending_receipt.or(stored_receipt);
            let action_id = decode_action(&action)?;
            let receipt_id = receipt
                .clone()
                .map(|v| {
                    serde_json::from_str::<ReceiptId>(&v)
                        .map_err(|_| LifecycleError::InvalidSession)
                })
                .transpose()?;
            let location = if state == "staged" {
                receipt_id
                    .map(receipt_slot)
                    .transpose()?
                    .unwrap_or_default()
            } else {
                slot.clone()
            };
            let dir = if state == "staged" {
                &self.holding_dir
            } else {
                &self.source_dir
            };
            let expected = Expected {
                slot,
                bytes,
                sha256: sha.clone(),
                dev,
                ino,
                status: state.clone(),
                action_id: action.clone(),
                receipt_id: receipt.clone(),
            };
            let status = if state == "uncertain" || self.has_pending_intent(&action)? {
                FixtureStatus::Uncertain
            } else if state == "permanently_removed" {
                // Terminal state: the holding file is expected to be gone,
                // so file verification must not run.
                FixtureStatus::PermanentlyRemoved
            } else if self.verify_fixture(&expected, dir, &location).is_err() {
                FixtureStatus::Changed
            } else {
                match state.as_str() {
                    "available" => FixtureStatus::Available,
                    "staged" => FixtureStatus::Staged,
                    "restored" => FixtureStatus::Restored,
                    _ => FixtureStatus::Changed,
                }
            };
            fixtures.push(FixtureSnapshot {
                identity_sha256: identity_digest(
                    self.id,
                    &action_id,
                    dev,
                    ino,
                    bytes,
                    &sha,
                    directory_bindings,
                )?,
                action_id,
                receipt_id,
                status,
                bytes,
                sha256: sha,
            });
        }
        Ok(SessionSnapshot {
            session_id: self.id,
            fixtures,
        })
    }

    fn lookup_action(&self, action: ActionId) -> Result<Option<Expected>, LifecycleError> {
        let encoded = encode_action(&action)?;
        Ok(self.db.query_row("SELECT action_id, slot, bytes, sha256, dev, ino, status, receipt_id FROM fixtures WHERE action_id=?1", [encoded], expected_row).optional()?)
    }
    fn lookup_receipt(&self, receipt: ReceiptId) -> Result<Option<Expected>, LifecycleError> {
        Ok(self.db.query_row("SELECT action_id, slot, bytes, sha256, dev, ino, status, receipt_id FROM fixtures WHERE receipt_id=?1", [typed_wire(&receipt)?], expected_row).optional()?)
    }
    fn has_pending_intent(&self, action: &str) -> Result<bool, LifecycleError> {
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM attempts WHERE action_id=?1 AND outcome='intent')",
            [action],
            |r| r.get(0),
        )?)
    }
    fn verify_fixture(
        &self,
        expected: &Expected,
        dir: &File,
        name: &str,
    ) -> Result<(), LifecycleError> {
        self.verify_directories()?;
        let file = open_regular(dir, name)?;
        let meta = file.metadata()?;
        let (dev, ino, nlink) = file_identity(&file)?;
        if !meta.is_file()
            || nlink != 1
            || meta.len() != expected.bytes
            || dev != expected.dev
            || ino != expected.ino
            || digest_file(file, expected.bytes)? != expected.sha256
        {
            return Err(LifecycleError::Changed);
        }
        Ok(())
    }
    fn verify_directories(&self) -> Result<(), LifecycleError> {
        let (dev, ino) = identity(&self.parent)?;
        let path_parent = open_dir_path(&self.parent_path).map_err(|_| LifecycleError::Changed)?;
        let root_name = self
            .root_path
            .file_name()
            .ok_or(LifecycleError::InvalidSession)?;
        let path_root =
            open_dir_at(&path_parent, root_name).map_err(|_| LifecycleError::Changed)?;
        let path_source =
            open_dir_at(&path_root, OsStr::new("source")).map_err(|_| LifecycleError::Changed)?;
        let path_holding =
            open_dir_at(&path_root, OsStr::new("holding")).map_err(|_| LifecycleError::Changed)?;
        let row: (i64,i64,i64,i64,i64,i64,i64,i64) = self.db.query_row("SELECT parent_dev,parent_ino,root_dev,root_ino,source_dev,source_ino,holding_dev,holding_ino FROM manifest WHERE singleton=1", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?)))?;
        if row.0 != dev
            || row.1 != ino
            || (row.2, row.3) != identity(&self.root_dir)?
            || (row.4, row.5) != identity(&self.source_dir)?
            || (row.6, row.7) != identity(&self.holding_dir)?
            || identity(&path_parent)? != identity(&self.parent)?
            || identity(&path_root)? != identity(&self.root_dir)?
            || identity(&path_source)? != identity(&self.source_dir)?
            || identity(&path_holding)? != identity(&self.holding_dir)?
        {
            return Err(LifecycleError::Changed);
        }
        Ok(())
    }
    fn directory_bindings(&self) -> Result<DirectoryBindings, LifecycleError> {
        Ok(self.db.query_row(
            "SELECT parent_dev,parent_ino,root_dev,root_ino,source_dev,source_ino,holding_dev,holding_ino FROM manifest WHERE singleton=1",
            [],
            |row| {
                Ok(DirectoryBindings {
                    parent_device: row.get(0)?,
                    parent_inode: row.get(1)?,
                    root_device: row.get(2)?,
                    root_inode: row.get(3)?,
                    source_device: row.get(4)?,
                    source_inode: row.get(5)?,
                    holding_device: row.get(6)?,
                    holding_inode: row.get(7)?,
                })
            },
        )?)
    }
    /// Journal an attempt intent. If the post-commit WAL checkpoint fails,
    /// the intent is already durable but the error is returned as-is — the
    /// stage/restore/purge callers treat this identically: any later
    /// operation on the same action converges to `Uncertain` via the
    /// pending-intent guard, and no purge unlink can follow an intent error.
    fn record_intent(
        &mut self,
        attempt: AttemptId,
        fixture: &Expected,
        op: &str,
        receipt: Option<ReceiptId>,
    ) -> Result<(), LifecycleError> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM attempts WHERE attempt_id=?1)",
            [typed_wire(&attempt)?],
            |r| r.get(0),
        )?;
        if exists {
            return Err(LifecycleError::DuplicateAttempt);
        }
        let current: Expected = tx.query_row("SELECT action_id, slot, bytes, sha256, dev, ino, status, receipt_id FROM fixtures WHERE action_id=?1", [&fixture.action_id], expected_row)?;
        if current.action_id != fixture.action_id
            || current.slot != fixture.slot
            || current.bytes != fixture.bytes
            || current.sha256 != fixture.sha256
            || current.dev != fixture.dev
            || current.ino != fixture.ino
            || current.status != fixture.status
            || current.receipt_id != fixture.receipt_id
        {
            return Err(LifecycleError::Changed);
        }
        let allowed_state = match op {
            "stage" => current.status == "available" || current.status == "restored",
            "restore" => {
                current.status == "staged"
                    && current.receipt_id == receipt.map(|v| typed_wire(&v)).transpose()?
            }
            "purge" => {
                current.status == "staged"
                    && current.receipt_id == receipt.map(|v| typed_wire(&v)).transpose()?
            }
            _ => false,
        };
        let pending: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM attempts WHERE action_id=?1 AND outcome='intent')",
            [&fixture.action_id],
            |r| r.get(0),
        )?;
        if !allowed_state || pending {
            return Err(if pending {
                LifecycleError::Uncertain
            } else {
                LifecycleError::WrongState
            });
        }
        if op == "stage" {
            let receipt_wire = receipt
                .map(|value| typed_wire(&value))
                .transpose()?
                .ok_or(LifecycleError::InvalidSession)?;
            let already_issued: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM attempts WHERE receipt_id=?1)",
                [&receipt_wire],
                |row| row.get(0),
            )?;
            if already_issued {
                return Err(LifecycleError::InvalidSession);
            }
        }
        tx.execute("INSERT INTO attempts(attempt_id, action_id, operation, receipt_id, outcome) VALUES(?1, ?2, ?3, ?4, 'intent')", params![typed_wire(&attempt)?, fixture.action_id, op, receipt.map(|v| typed_wire(&v)).transpose()?])?;
        tx.commit()?;
        self.db.execute_batch("PRAGMA wal_checkpoint(FULL);")?;
        Ok(())
    }
    fn record_outcome(
        &mut self,
        attempt: AttemptId,
        action: &str,
        outcome: &str,
        receipt: Option<ReceiptId>,
    ) -> Result<(), LifecycleError> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = tx.execute(
            "UPDATE attempts SET outcome=?1 WHERE attempt_id=?2",
            params![outcome, typed_wire(&attempt)?],
        )?;
        if updated != 1 {
            // The journal row must exist and match exactly one attempt;
            // otherwise the journal contract is not self-enforcing.
            return Err(LifecycleError::InvalidSession);
        }
        if outcome == "staged" || outcome == "restored" {
            tx.execute(
                "UPDATE fixtures SET status=?1, receipt_id=?2 WHERE action_id=?3",
                params![
                    outcome,
                    receipt.map(|v| typed_wire(&v)).transpose()?,
                    action
                ],
            )?;
        } else if outcome == "permanently_removed" {
            tx.execute(
                "UPDATE fixtures SET status='permanently_removed' WHERE action_id=?1",
                [&action],
            )?;
        }
        tx.commit()?;
        self.db.execute_batch("PRAGMA wal_checkpoint(FULL);")?;
        Ok(())
    }
    fn mark_uncertain(&mut self, attempt: AttemptId, action: &str) -> Result<(), LifecycleError> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE attempts SET outcome='uncertain' WHERE attempt_id=?1",
            [typed_wire(&attempt)?],
        )?;
        tx.execute(
            "UPDATE fixtures SET status='uncertain' WHERE action_id=?1",
            [action],
        )?;
        tx.commit()?;
        self.db.execute_batch("PRAGMA wal_checkpoint(FULL);")?;
        Ok(())
    }
    fn uncertain_error(&mut self, attempt: AttemptId, action: &str) -> LifecycleError {
        let _ = self.mark_uncertain(attempt, action);
        LifecycleError::Uncertain
    }
}

struct Expected {
    action_id: String,
    slot: String,
    bytes: u64,
    sha256: String,
    dev: i64,
    ino: i64,
    status: String,
    receipt_id: Option<String>,
}
#[derive(Serialize)]
struct IdentityDigestV1 {
    schema_version: u8,
    session_id: String,
    action_id: String,
    device: i64,
    inode: i64,
    bytes: u64,
    content_sha256: String,
    parent_device: i64,
    parent_inode: i64,
    root_device: i64,
    root_inode: i64,
    source_device: i64,
    source_inode: i64,
    holding_device: i64,
    holding_inode: i64,
}
#[derive(Clone, Copy)]
struct DirectoryBindings {
    parent_device: i64,
    parent_inode: i64,
    root_device: i64,
    root_inode: i64,
    source_device: i64,
    source_inode: i64,
    holding_device: i64,
    holding_inode: i64,
}
fn expected_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Expected> {
    Ok(Expected {
        action_id: r.get(0)?,
        slot: r.get(1)?,
        bytes: r.get::<_, i64>(2)? as u64,
        sha256: r.get(3)?,
        dev: r.get(4)?,
        ino: r.get(5)?,
        status: r.get(6)?,
        receipt_id: r.get(7)?,
    })
}
fn configure_db(db: &Connection, creating: bool) -> Result<(), rusqlite::Error> {
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.pragma_update(None, "foreign_keys", "ON")?;
    db.pragma_update(None, "synchronous", "FULL")?;
    if creating {
        db.pragma_update(None, "journal_mode", "WAL")?;
    }
    Ok(())
}
fn initialize_schema(db: &Connection) -> Result<(), rusqlite::Error> {
    db.execute_batch(&format!(
        "{MANIFEST_SCHEMA};{FIXTURES_SCHEMA};{ATTEMPTS_SCHEMA};"
    ))?;
    db.pragma_update(None, "application_id", DB_APPLICATION_ID)?;
    db.pragma_update(None, "user_version", DB_SCHEMA_VERSION)?;
    Ok(())
}
fn validate_registry(db: &Connection) -> Result<(), LifecycleError> {
    let mut actions = std::collections::HashSet::new();
    let mut slots = std::collections::HashSet::new();
    let mut fixture_state = std::collections::HashMap::new();
    let mut stmt = db.prepare(
        "SELECT action_id, slot, bytes, sha256, dev, ino, status, receipt_id FROM fixtures",
    )?;
    let rows = stmt.query_map([], expected_row)?;
    let mut count = 0;
    for row in rows {
        let fixture = row?;
        let action_id = decode_action(&fixture.action_id)?;
        if encode_action(&action_id)? != fixture.action_id {
            return Err(LifecycleError::InvalidSession);
        }
        if !actions.insert(fixture.action_id.clone())
            || !slots.insert(fixture.slot.clone())
            || fixture.dev < 0
            || fixture.ino <= 0
        {
            return Err(LifecycleError::InvalidSession);
        }
        let Some((index, (_, expected_bytes))) = FIXTURES
            .iter()
            .enumerate()
            .find(|(_, (slot, _))| *slot == fixture.slot)
        else {
            return Err(LifecycleError::InvalidSession);
        };
        if fixture.bytes != *expected_bytes
            || fixture.bytes > MAX_FIXTURE_BYTES
            || fixture.sha256 != fixture_digest(index, fixture.bytes)
        {
            return Err(LifecycleError::InvalidSession);
        }
        match fixture.status.as_str() {
            "available" if fixture.receipt_id.is_none() => {}
            "staged" | "restored" if fixture.receipt_id.is_some() => {}
            "permanently_removed" if fixture.receipt_id.is_some() => {}
            "uncertain" => {}
            _ => return Err(LifecycleError::InvalidSession),
        }
        if let Some(receipt) = &fixture.receipt_id {
            let parsed: ReceiptId =
                serde_json::from_str(receipt).map_err(|_| LifecycleError::InvalidSession)?;
            if typed_wire(&parsed)? != *receipt {
                return Err(LifecycleError::InvalidSession);
            }
        }
        fixture_state.insert(
            fixture.action_id.clone(),
            (fixture.status.clone(), fixture.receipt_id.clone()),
        );
        count += 1;
    }
    if count != FIXTURES.len() || slots.len() != FIXTURES.len() || actions.len() != FIXTURES.len() {
        return Err(LifecycleError::InvalidSession);
    }

    let mut pending_actions = std::collections::HashSet::new();
    let mut receipt_actions = std::collections::HashMap::new();
    let mut staged_receipts = std::collections::HashSet::new();
    let mut issued_receipts = std::collections::HashSet::new();
    let mut restored_receipts = std::collections::HashSet::new();
    let mut uncertain_actions = std::collections::HashSet::new();
    let mut purged_receipts = std::collections::HashSet::new();
    let mut attempts = db.prepare(
        "SELECT attempt_id, action_id, operation, receipt_id, outcome FROM attempts ORDER BY rowid",
    )?;
    let rows = attempts.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    for row in rows {
        let (attempt_id, action_id, operation, receipt_id, outcome) = row?;
        let parsed_attempt: AttemptId =
            serde_json::from_str(&attempt_id).map_err(|_| LifecycleError::InvalidSession)?;
        let action: ActionId = decode_action(&action_id)?;
        if typed_wire(&parsed_attempt)? != attempt_id
            || encode_action(&action)? != action_id
            || !fixture_state.contains_key(&action_id)
        {
            return Err(LifecycleError::InvalidSession);
        }
        if let Some(receipt) = &receipt_id {
            let parsed: ReceiptId =
                serde_json::from_str(receipt).map_err(|_| LifecycleError::InvalidSession)?;
            if typed_wire(&parsed)? != *receipt {
                return Err(LifecycleError::InvalidSession);
            }
            if receipt_actions
                .insert(receipt.clone(), action_id.clone())
                .is_some_and(|previous| previous != action_id)
            {
                return Err(LifecycleError::InvalidSession);
            }
        } else {
            return Err(LifecycleError::InvalidSession);
        }
        let valid_outcome = match operation.as_str() {
            "stage" => matches!(
                outcome.as_str(),
                "intent" | "staged" | "collision" | "uncertain"
            ),
            "restore" => matches!(
                outcome.as_str(),
                "intent" | "restored" | "collision" | "uncertain"
            ),
            "purge" => matches!(
                outcome.as_str(),
                "intent" | "permanently_removed" | "uncertain"
            ),
            _ => false,
        };
        if !valid_outcome || (outcome == "intent" && !pending_actions.insert(action_id.clone())) {
            return Err(LifecycleError::InvalidSession);
        }
        let receipt = receipt_id.ok_or(LifecycleError::InvalidSession)?;
        if outcome == "uncertain" {
            uncertain_actions.insert(action_id.clone());
        }
        match (operation.as_str(), outcome.as_str()) {
            ("stage", _) => {
                if !issued_receipts.insert((action_id.clone(), receipt.clone())) {
                    return Err(LifecycleError::InvalidSession);
                }
                if outcome == "staged" {
                    staged_receipts.insert((action_id.clone(), receipt.clone()));
                }
            }
            ("restore", "restored") => {
                let key = (action_id.clone(), receipt.clone());
                if !staged_receipts.contains(&key) {
                    return Err(LifecycleError::InvalidSession);
                }
                restored_receipts.insert(key);
            }
            ("restore", _) if staged_receipts.contains(&(action_id.clone(), receipt.clone())) => {}
            ("restore", _) => return Err(LifecycleError::InvalidSession),
            // A purge row is only credible for a receipt that a stage
            // attempt issued and staged in this session's history; anything
            // else is fabricated and refused.
            ("purge", _)
                if issued_receipts.contains(&(action_id.clone(), receipt.clone()))
                    && staged_receipts.contains(&(action_id.clone(), receipt.clone())) => {}
            _ => return Err(LifecycleError::InvalidSession),
        }
        if operation == "purge" && outcome == "permanently_removed" {
            purged_receipts.insert((action_id.clone(), receipt.clone()));
        }
    }
    for (action, (status, receipt)) in fixture_state {
        match status.as_str() {
            "staged" => {
                if !receipt
                    .as_ref()
                    .is_some_and(|id| staged_receipts.contains(&(action.clone(), id.clone())))
                {
                    return Err(LifecycleError::InvalidSession);
                }
            }
            "restored" => {
                let Some(ref id) = receipt else {
                    return Err(LifecycleError::InvalidSession);
                };
                if !staged_receipts.contains(&(action.clone(), id.clone()))
                    || !restored_receipts.contains(&(action.clone(), id.clone()))
                {
                    return Err(LifecycleError::InvalidSession);
                }
            }
            "permanently_removed" => {
                let Some(ref id) = receipt else {
                    return Err(LifecycleError::InvalidSession);
                };
                // Terminal fixture state requires the full credible history:
                // the receipt was issued, staged, and actually purged with a
                // durable terminal outcome — stage history alone is not a
                // removal claim.
                if !issued_receipts.contains(&(action.clone(), id.clone()))
                    || !staged_receipts.contains(&(action.clone(), id.clone()))
                    || !purged_receipts.contains(&(action.clone(), id.clone()))
                {
                    return Err(LifecycleError::InvalidSession);
                }
            }
            "uncertain"
                if !uncertain_actions.contains(&action) && !pending_actions.contains(&action) =>
            {
                return Err(LifecycleError::InvalidSession);
            }
            _ => {}
        }
        if pending_actions.contains(&action) {
            let (operation, receipt_id): (String, String) = db.query_row(
                "SELECT operation, receipt_id FROM attempts WHERE action_id=?1 AND outcome='intent'",
                [&action],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            match operation.as_str() {
                "stage" if status == "available" || status == "restored" => {}
                "restore"
                    if status == "staged" && receipt.as_deref() == Some(receipt_id.as_str()) => {}
                // A pending purge intent claims a still-staged receipt; the
                // crash leaves the outcome unknown until inspection.
                "purge"
                    if status == "staged" && receipt.as_deref() == Some(receipt_id.as_str()) => {}
                _ => return Err(LifecycleError::InvalidSession),
            }
        }
    }
    Ok(())
}
fn validate_schema(db: &Connection) -> Result<(), LifecycleError> {
    validate_schema_version(db, DB_SCHEMA_VERSION, false)
}
fn validate_schema_version(
    db: &Connection,
    expected_version: i64,
    legacy: bool,
) -> Result<(), LifecycleError> {
    let application_id: i64 = db.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let schema_version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let integrity: String = db.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    let journal_mode: String = db.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    let synchronous: i64 = db.query_row("PRAGMA synchronous", [], |r| r.get(0))?;
    let foreign_keys: i64 = db.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    let objects: i64 = db.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*'",
        [],
        |r| r.get(0),
    )?;
    let known_tables: i64 = db.query_row("SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name IN ('manifest','fixtures','attempts')", [], |r| r.get(0))?;
    let strict_tables: i64 = db.query_row("SELECT COUNT(*) FROM pragma_table_list WHERE schema='main' AND name IN ('manifest','fixtures','attempts') AND strict=1", [], |r| r.get(0))?;
    if objects != 3 || known_tables != 3 || strict_tables != 3 {
        return Err(LifecycleError::InvalidSession);
    }
    let oversized_schema: i64 = db.query_row("SELECT count(*) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*' AND (length(CAST(type AS BLOB))>64 OR length(CAST(name AS BLOB))>128 OR length(CAST(sql AS BLOB))>2048)", [], |r| r.get(0))?;
    if oversized_schema != 0 {
        return Err(LifecycleError::InvalidSession);
    }
    let mut statement = db.prepare("SELECT name, sql FROM sqlite_schema WHERE type='table' AND name NOT GLOB 'sqlite_*' ORDER BY name")?;
    let schema_rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut exact_schema = Vec::new();
    for row in schema_rows {
        exact_schema.push(row?);
    }
    let expected_schema = vec![
        (
            "attempts".to_owned(),
            if legacy {
                ATTEMPTS_SCHEMA_V1
            } else {
                ATTEMPTS_SCHEMA
            }
            .to_owned(),
        ),
        (
            "fixtures".to_owned(),
            if legacy {
                FIXTURES_SCHEMA_V1
            } else {
                FIXTURES_SCHEMA
            }
            .to_owned(),
        ),
        ("manifest".to_owned(), MANIFEST_SCHEMA.to_owned()),
    ];
    if application_id != DB_APPLICATION_ID
        || schema_version != expected_version
        || integrity != "ok"
        || !journal_mode.eq_ignore_ascii_case("wal")
        || synchronous != 2
        || foreign_keys != 1
        || objects != 3
        || known_tables != 3
        || strict_tables != 3
        || exact_schema != expected_schema
    {
        return Err(LifecycleError::InvalidSession);
    }
    Ok(())
}
fn validate_migration_row_bounds(db: &Connection) -> Result<(), LifecycleError> {
    let (manifest_count, fixture_count, attempt_count): (i64,i64,i64) = db.query_row(
        "SELECT (SELECT count(*) FROM manifest),(SELECT count(*) FROM fixtures),(SELECT count(*) FROM attempts)", [],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    if manifest_count != 1 || fixture_count != FIXTURES.len() as i64 || attempt_count > MAX_ATTEMPTS
    {
        return Err(LifecycleError::InvalidSession);
    }
    let oversized: i64 = db.query_row(
        "SELECT (SELECT count(*) FROM manifest WHERE length(CAST(session_id AS BLOB))>128) + (SELECT count(*) FROM fixtures WHERE length(CAST(action_id AS BLOB))>128 OR length(CAST(slot AS BLOB))>64 OR length(CAST(sha256 AS BLOB))>64 OR length(CAST(status AS BLOB))>32 OR length(CAST(receipt_id AS BLOB))>128) + (SELECT count(*) FROM attempts WHERE length(CAST(attempt_id AS BLOB))>128 OR length(CAST(action_id AS BLOB))>128 OR length(CAST(operation AS BLOB))>32 OR length(CAST(receipt_id AS BLOB))>128 OR length(CAST(outcome AS BLOB))>32)", [], |r| r.get(0))?;
    if oversized != 0 {
        return Err(LifecycleError::InvalidSession);
    }
    Ok(())
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MigrationFault {
    None,
    AfterRename,
    AfterCreate,
    AfterCopy,
    BeforeVersion,
}
fn migrate_legacy_v1(
    db: &Connection,
    id: SessionId,
    parent: &File,
    root: &File,
    source: &File,
    holding: &File,
) -> Result<(), LifecycleError> {
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version == DB_SCHEMA_VERSION {
        validate_schema(db)?;
        return Ok(());
    }
    if version != LEGACY_SCHEMA_VERSION {
        return Err(LifecycleError::InvalidSession);
    }
    migrate_legacy_v1_inner(db, id, parent, root, source, holding, MigrationFault::None)
}
fn migrate_legacy_v1_inner(
    db: &Connection,
    id: SessionId,
    parent: &File,
    root: &File,
    source: &File,
    holding: &File,
    fault: MigrationFault,
) -> Result<(), LifecycleError> {
    validate_schema_version(db, LEGACY_SCHEMA_VERSION, true)?;
    validate_manifest(db, id, parent, root, source, holding)?;
    validate_migration_row_bounds(db)?;
    validate_registry(db)?;
    let tx = rusqlite::Transaction::new_unchecked(db, TransactionBehavior::Immediate)?;
    validate_schema_version(&tx, LEGACY_SCHEMA_VERSION, true)?;
    validate_manifest(&tx, id, parent, root, source, holding)?;
    validate_migration_row_bounds(&tx)?;
    validate_registry(&tx)?;
    tx.execute_batch("ALTER TABLE fixtures RENAME TO fixtures_v1_backup; ALTER TABLE attempts RENAME TO attempts_v1_backup;")?;
    if fault == MigrationFault::AfterRename {
        return Err(LifecycleError::InvalidSession);
    }
    tx.execute_batch(FIXTURES_SCHEMA)?;
    tx.execute_batch(ATTEMPTS_SCHEMA)?;
    if fault == MigrationFault::AfterCreate {
        return Err(LifecycleError::InvalidSession);
    }
    tx.execute_batch("INSERT INTO fixtures(rowid,action_id,slot,bytes,sha256,dev,ino,status,receipt_id) SELECT rowid,action_id,slot,bytes,sha256,dev,ino,status,receipt_id FROM fixtures_v1_backup ORDER BY rowid; INSERT INTO attempts(rowid,attempt_id,action_id,operation,receipt_id,outcome) SELECT rowid,attempt_id,action_id,operation,receipt_id,outcome FROM attempts_v1_backup ORDER BY rowid;")?;
    if fault == MigrationFault::AfterCopy {
        return Err(LifecycleError::InvalidSession);
    }
    tx.execute_batch("DROP TABLE fixtures_v1_backup; DROP TABLE attempts_v1_backup;")?;
    validate_migration_row_bounds(&tx)?;
    validate_registry(&tx)?;
    if fault == MigrationFault::BeforeVersion {
        return Err(LifecycleError::InvalidSession);
    }
    tx.pragma_update(None, "user_version", DB_SCHEMA_VERSION)?;
    validate_schema(&tx)?;
    tx.commit()?;
    Ok(())
}

fn validate_manifest(
    db: &Connection,
    id: SessionId,
    parent: &File,
    root: &File,
    source: &File,
    holding: &File,
) -> Result<(), LifecycleError> {
    let recorded: String = db
        .query_row(
            "SELECT session_id FROM manifest WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .map_err(|_| LifecycleError::InvalidSession)?;
    if recorded != typed_wire(&id)? {
        return Err(LifecycleError::InvalidSession);
    }
    let (_, pdev, pino, rdev, rino, sdev, sino, hdev, hino): (String,i64,i64,i64,i64,i64,i64,i64,i64) = db.query_row("SELECT session_id,parent_dev,parent_ino,root_dev,root_ino,source_dev,source_ino,holding_dev,holding_ino FROM manifest WHERE singleton=1", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?)))?;
    if (pdev, pino) != identity(parent)?
        || (rdev, rino) != identity(root)?
        || (sdev, sino) != identity(source)?
        || (hdev, hino) != identity(holding)?
    {
        return Err(LifecycleError::Changed);
    }
    Ok(())
}
fn unique_actions(actions: &[ActionId; 5]) -> bool {
    actions
        .iter()
        .enumerate()
        .all(|(i, a)| !actions[..i].contains(a))
}
fn encode_action(id: &ActionId) -> Result<String, LifecycleError> {
    serde_json::to_string(id).map_err(|_| LifecycleError::InvalidBindings)
}
fn decode_action(id: &str) -> Result<ActionId, LifecycleError> {
    serde_json::from_str(id).map_err(|_| LifecycleError::InvalidSession)
}
fn typed_wire<T: Serialize>(id: &T) -> Result<String, LifecycleError> {
    serde_json::to_string(id).map_err(|_| LifecycleError::InvalidSession)
}
fn identity_digest(
    session_id: SessionId,
    action_id: &ActionId,
    device: i64,
    inode: i64,
    bytes: u64,
    content_sha256: &str,
    directories: DirectoryBindings,
) -> Result<String, LifecycleError> {
    let material = IdentityDigestV1 {
        schema_version: 1,
        session_id: typed_wire(&session_id)?,
        action_id: encode_action(action_id)?,
        device,
        inode,
        bytes,
        content_sha256: content_sha256.to_owned(),
        parent_device: directories.parent_device,
        parent_inode: directories.parent_inode,
        root_device: directories.root_device,
        root_inode: directories.root_inode,
        source_device: directories.source_device,
        source_inode: directories.source_inode,
        holding_device: directories.holding_device,
        holding_inode: directories.holding_inode,
    };
    let canonical = serde_json::to_vec(&material).map_err(|_| LifecycleError::InvalidSession)?;
    Ok(format!("{:x}", Sha256::digest(canonical)))
}
fn typed_component<T: Serialize>(id: &T) -> Result<String, LifecycleError> {
    let wire = typed_wire(id)?;
    let unquoted = wire
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .ok_or(LifecycleError::InvalidSession)?;
    let component = unquoted
        .rsplit(':')
        .next()
        .ok_or(LifecycleError::InvalidSession)?;
    if component.len() != 36
        || !component
            .bytes()
            .all(|b| b.is_ascii_hexdigit() || b == b'-')
    {
        return Err(LifecycleError::InvalidSession);
    }
    Ok(component.to_owned())
}
fn receipt_slot(id: ReceiptId) -> Result<String, LifecycleError> {
    typed_component(&id).map(|value| format!("receipt-{value}"))
}
fn supported() -> Result<(), LifecycleError> {
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        Ok(())
    } else {
        Err(LifecycleError::UnsupportedPlatform)
    }
}
fn create_private_dir(path: &Path) -> Result<(), LifecycleError> {
    fs::create_dir(path)?;
    set_private_dir(path)?;
    Ok(())
}
#[cfg(unix)]
fn set_private_dir(path: &Path) -> Result<(), LifecycleError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
#[cfg(not(unix))]
fn set_private_dir(_: &Path) -> Result<(), LifecycleError> {
    Err(LifecycleError::UnsupportedPlatform)
}
#[cfg(unix)]
fn set_private_file(file: &File) -> Result<(), LifecycleError> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(())
}
#[cfg(not(unix))]
fn set_private_file(_: &File) -> Result<(), LifecycleError> {
    Err(LifecycleError::UnsupportedPlatform)
}
#[cfg(unix)]
fn identity(file: &File) -> Result<(i64, i64), LifecycleError> {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata()?;
    Ok((m.dev() as i64, m.ino() as i64))
}
#[cfg(not(unix))]
fn identity(_: &File) -> Result<(i64, i64), LifecycleError> {
    Err(LifecycleError::UnsupportedPlatform)
}
#[cfg(unix)]
fn file_identity(file: &File) -> Result<(i64, i64, u64), LifecycleError> {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata()?;
    Ok((m.dev() as i64, m.ino() as i64, m.nlink()))
}
#[cfg(not(unix))]
fn file_identity(_: &File) -> Result<(i64, i64, u64), LifecycleError> {
    Err(LifecycleError::UnsupportedPlatform)
}
fn write_fixture(file: &mut File, index: usize, bytes: u64) -> Result<String, LifecycleError> {
    if bytes > MAX_FIXTURE_BYTES {
        return Err(LifecycleError::InvalidBindings);
    }
    let mut hash = Sha256::new();
    let mut remaining = bytes;
    let buffer = vec![(index as u8).wrapping_add(0x41); 64 * 1024];
    while remaining > 0 {
        let n = remaining.min(buffer.len() as u64) as usize;
        file.write_all(&buffer[..n])?;
        hash.update(&buffer[..n]);
        remaining -= n as u64;
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn fixture_digest(index: usize, bytes: u64) -> String {
    let mut hash = Sha256::new();
    let buffer = [(index as u8).wrapping_add(0x41); 64 * 1024];
    let mut remaining = bytes;
    while remaining > 0 {
        let amount = remaining.min(buffer.len() as u64) as usize;
        hash.update(&buffer[..amount]);
        remaining -= amount as u64;
    }
    format!("{:x}", hash.finalize())
}
fn digest_file(mut file: File, expected_bytes: u64) -> Result<String, LifecycleError> {
    if expected_bytes > MAX_FIXTURE_BYTES {
        return Err(LifecycleError::Changed);
    }
    let before = file.metadata()?;
    let before_identity = file_identity(&file)?;
    if before.len() != expected_bytes {
        return Err(LifecycleError::Changed);
    }
    file.seek(SeekFrom::Start(0))?;
    let digest = digest_reader(&mut file, expected_bytes)?;
    let after = file.metadata()?;
    let after_identity = file_identity(&file)?;
    if after.len() != expected_bytes || before_identity != after_identity {
        return Err(LifecycleError::Changed);
    }
    Ok(digest)
}
fn digest_reader(reader: &mut impl Read, expected_bytes: u64) -> Result<String, LifecycleError> {
    if expected_bytes > MAX_FIXTURE_BYTES {
        return Err(LifecycleError::Changed);
    }
    let mut hash = Sha256::new();
    let mut buf = [0; 65536];
    let mut total = 0u64;
    let mut bounded = reader.take(expected_bytes.saturating_add(1));
    loop {
        let n = bounded.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total = total.saturating_add(n as u64);
        hash.update(&buf[..n]);
    }
    if total != expected_bytes {
        return Err(LifecycleError::Changed);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn is_fixture_component(value: &str) -> bool {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.contains('\0')
    {
        return false;
    }
    if FIXTURES.iter().any(|(slot, _)| *slot == value) {
        return true;
    }
    let Some(uuid) = value.strip_prefix("receipt-") else {
        return false;
    };
    format!("disk-steward:receipt:v1:{uuid}")
        .parse::<ReceiptId>()
        .is_ok()
}
fn open_regular(dir: &File, name: &str) -> Result<File, LifecycleError> {
    if !is_fixture_component(name) {
        return Err(LifecycleError::Changed);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use rustix::fs::{openat, Mode, OFlags};
        let fd = openat(
            dir,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|e| LifecycleError::Io(std::io::Error::from(e)))?;
        let f = File::from(fd);
        if !f.metadata()?.is_file() {
            return Err(LifecycleError::Changed);
        }
        Ok(f)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (dir, name);
        Err(LifecycleError::UnsupportedPlatform)
    }
}
fn open_dir_path(path: &Path) -> Result<File, LifecycleError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use rustix::fs::{open, Mode, OFlags};
        let fd = open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| LifecycleError::Io(std::io::Error::from(e)))?;
        Ok(File::from(fd))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = path;
        Err(LifecycleError::UnsupportedPlatform)
    }
}
fn open_dir_at(parent: &File, name: &OsStr) -> Result<File, LifecycleError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use rustix::fs::{openat, Mode, OFlags};
        let fd = openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| LifecycleError::Io(std::io::Error::from(e)))?;
        Ok(File::from(fd))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (parent, name);
        Err(LifecycleError::UnsupportedPlatform)
    }
}
fn sync_dir(dir: &File) -> Result<(), LifecycleError> {
    dir.sync_all()?;
    Ok(())
}
// `Collision` and `Uncertain` are constructed only by the unix rename
// implementation below; other hosts fail closed with `Unsupported`.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
#[derive(Debug)]
enum RenameFailure {
    Collision,
    Uncertain,
    Unsupported,
    InvalidComponent,
}
fn rename_no_replace(
    from: &File,
    source: &str,
    to: &File,
    dest: &str,
) -> Result<(), RenameFailure> {
    if !is_fixture_component(source) || !is_fixture_component(dest) {
        return Err(RenameFailure::InvalidComponent);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use rustix::fs::{renameat_with, RenameFlags};
        use rustix::io::Errno;
        match renameat_with(from, source, to, dest, RenameFlags::NOREPLACE) {
            Ok(()) => Ok(()),
            Err(Errno::EXIST | Errno::NOTEMPTY) => Err(RenameFailure::Collision),
            Err(Errno::NOSYS | Errno::OPNOTSUPP | Errno::INVAL | Errno::XDEV) => {
                Err(RenameFailure::Unsupported)
            }
            Err(_) => Err(RenameFailure::Uncertain),
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (from, source, to, dest);
        Err(RenameFailure::Unsupported)
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use std::{
        process::{Child, Command, ExitStatus},
        thread,
        time::{Duration, Instant},
    };

    const PURGE_CRASH_CHILD: &str = "tests::purge_crash_child_runs_real_purge";

    #[test]
    #[ignore = "launched only by the bounded parent subprocess test"]
    fn purge_crash_child_runs_real_purge() {
        let args = std::env::args_os();
        let payload = args
            .skip_while(|arg| arg != "--")
            .nth(1)
            .expect("parent-provided payload");
        let (root, receipt, boundary): (PathBuf, ReceiptId, String) =
            serde_json::from_str(&payload.to_string_lossy()).unwrap();
        let selected = match boundary.as_str() {
            "intent" => "intent",
            "unlink" => "unlink",
            "directory_sync" => "directory_sync",
            "terminal" => "terminal",
            _ => panic!("unknown purge boundary"),
        };
        let session_id = fs::read_to_string(root.join("session-id")).unwrap();
        let (mut life, _) = FixtureLifecycle::reopen(
            root.parent().unwrap(),
            serde_json::from_str(&session_id).unwrap(),
        )
        .unwrap();
        PURGE_CRASH_BOUNDARY.with(|hook| hook.set(Some(selected)));
        let _ = life.purge_receipt(receipt, AttemptId::new());
        panic!("selected crash boundary was not reached");
    }

    fn wait_bounded(child: &mut Child) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("purge crash child exceeded 15 seconds");
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn crash_child(root: &Path, receipt: ReceiptId, boundary: &str) -> ExitStatus {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(PURGE_CRASH_CHILD)
            .arg("--ignored")
            .arg("--nocapture")
            .arg("--")
            .arg(serde_json::to_string(&(root, receipt, boundary)).unwrap())
            .spawn()
            .unwrap();
        wait_bounded(&mut child)
    }

    #[test]
    fn purge_subprocess_crashes_at_real_filesystem_and_journal_boundaries() {
        for (boundary, expected_exists, expected_outcome, expected_status) in [
            ("intent", true, "intent", FixtureStatus::Uncertain),
            ("unlink", false, "intent", FixtureStatus::Uncertain),
            ("directory_sync", false, "intent", FixtureStatus::Uncertain),
            (
                "terminal",
                false,
                "permanently_removed",
                FixtureStatus::PermanentlyRemoved,
            ),
        ] {
            let parent = tempfile::tempdir().unwrap();
            let ids = bindings();
            let (mut life, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
            let receipt = life.stage(ids[0], AttemptId::new()).unwrap();
            // The child receives the private root and derives its session ID
            // from a parent-owned marker, keeping fixture setup in this test.
            let sid = life.session_id();
            fs::write(
                life.root.join("session-id"),
                serde_json::to_vec(&sid).unwrap(),
            )
            .unwrap();
            let slot = receipt_slot(receipt).unwrap();
            let sentinel = life.root.join("unrelated-sentinel");
            fs::write(&sentinel, b"preserve this sentinel").unwrap();
            let root = life.root.clone();
            drop(life);

            let status = crash_child(&root, receipt, boundary);
            assert_eq!(status.code(), Some(86), "child did not crash at {boundary}");

            let (mut reopened, view) = FixtureLifecycle::reopen(parent.path(), sid).unwrap();
            assert_eq!(
                view.fixtures[0].status, expected_status,
                "boundary={boundary}"
            );
            assert_eq!(fs::read(&sentinel).unwrap(), b"preserve this sentinel");
            assert_eq!(
                open_regular(&reopened.holding_dir, &slot).is_ok(),
                expected_exists
            );
            let outcome: String = reopened
                .db
                .query_row(
                    "SELECT outcome FROM attempts WHERE operation='purge'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(outcome, expected_outcome, "boundary={boundary}");
            let journal_before_refusals = dump_all(&reopened.db);
            for fixture in &view.fixtures[1..] {
                assert_eq!(fixture.status, FixtureStatus::Available);
            }
            if expected_outcome == "intent" {
                assert!(matches!(
                    reopened.purge_receipt(receipt, AttemptId::new()),
                    Err(LifecycleError::Uncertain)
                ));
                assert!(matches!(
                    reopened.restore(receipt, AttemptId::new()),
                    Err(LifecycleError::Uncertain)
                ));
            } else {
                assert!(matches!(
                    reopened.purge_receipt(receipt, AttemptId::new()),
                    Err(LifecycleError::WrongState)
                ));
                assert!(matches!(
                    reopened.restore(receipt, AttemptId::new()),
                    Err(LifecycleError::WrongState)
                ));
            }
            assert_eq!(dump_all(&reopened.db), journal_before_refusals);
        }
    }

    fn downgrade_to_v1(db: &Connection) {
        let before = dump_all(db);
        let tx = db.unchecked_transaction().unwrap();
        tx.execute_batch("ALTER TABLE fixtures RENAME TO fixtures_v2_backup; ALTER TABLE attempts RENAME TO attempts_v2_backup;").unwrap();
        tx.execute_batch(FIXTURES_SCHEMA_V1).unwrap();
        tx.execute_batch(ATTEMPTS_SCHEMA_V1).unwrap();
        tx.execute_batch("INSERT INTO fixtures(rowid,action_id,slot,bytes,sha256,dev,ino,status,receipt_id) SELECT rowid,action_id,slot,bytes,sha256,dev,ino,status,receipt_id FROM fixtures_v2_backup ORDER BY rowid; INSERT INTO attempts(rowid,attempt_id,action_id,operation,receipt_id,outcome) SELECT rowid,attempt_id,action_id,operation,receipt_id,outcome FROM attempts_v2_backup ORDER BY rowid; DROP TABLE fixtures_v2_backup; DROP TABLE attempts_v2_backup;").unwrap();
        tx.pragma_update(None, "user_version", LEGACY_SCHEMA_VERSION)
            .unwrap();
        tx.commit().unwrap();
        assert_eq!(
            dump_all(db),
            before,
            "downgrade_to_v1 must preserve every rowid and column, so the migration baseline is not lossy"
        );
    }
    fn bindings() -> [ActionId; 5] {
        std::array::from_fn(|_| ActionId::new())
    }
    /// Complete contents of one table: every rowid with every column value,
    /// in rowid order. Used to prove migration and rollback preserve data
    /// exactly, not merely the fields the snapshot exposes.
    type RowDump = Vec<(i64, Vec<rusqlite::types::Value>)>;
    type DatabaseDump = Vec<(String, RowDump)>;
    fn dump_table(db: &Connection, table: &str) -> RowDump {
        let columns: Vec<String> = {
            let mut stmt = db
                .prepare(&format!(
                    "SELECT name FROM pragma_table_info('{table}') ORDER BY cid"
                ))
                .unwrap();
            stmt.query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert!(
            !columns.is_empty(),
            "table {table} missing or has no columns after operation"
        );
        let list = columns.join(",");
        let mut stmt = db
            .prepare(&format!("SELECT rowid,{list} FROM {table} ORDER BY rowid"))
            .unwrap_or_else(|e| panic!("table {table} unreadable after operation: {e}"));
        let mapped = stmt
            .query_map([], |row| {
                let rowid: i64 = row.get(0)?;
                let values = (0..columns.len())
                    .map(|i| row.get::<_, rusqlite::types::Value>(i + 1))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((rowid, values))
            })
            .unwrap();
        mapped.collect::<Result<Vec<_>, _>>().unwrap()
    }
    fn dump_all(db: &Connection) -> DatabaseDump {
        ["manifest", "fixtures", "attempts"]
            .into_iter()
            .map(|table| (table.to_owned(), dump_table(db, table)))
            .collect()
    }
    #[test]
    fn legacy_v1_migrates_with_rowid_gaps_and_old_receipt_remains_restorable() {
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (mut life, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        let first = life.stage(ids[0], AttemptId::new()).unwrap();
        life.restore(first, AttemptId::new()).unwrap();
        let staged = life.stage(ids[0], AttemptId::new()).unwrap();
        let staged_b = life.stage(ids[1], AttemptId::new()).unwrap();
        life.db
            .execute_batch(
                "UPDATE fixtures SET rowid=rowid+10000; UPDATE attempts SET rowid=rowid+10000;",
            )
            .unwrap();
        downgrade_to_v1(&life.db);
        let before = dump_all(&life.db);
        let session = life.session_id();
        drop(life);
        let (mut migrated, snapshot) = FixtureLifecycle::reopen(parent.path(), session).unwrap();
        assert_eq!(
            dump_all(&migrated.db),
            before,
            "migration must preserve every rowid and every column of manifest, fixtures and attempts"
        );
        assert_eq!(snapshot.fixtures[0].receipt_id, Some(staged));
        assert_eq!(snapshot.fixtures[0].status, FixtureStatus::Staged);
        assert_eq!(snapshot.fixtures[1].receipt_id, Some(staged_b));
        assert_eq!(snapshot.fixtures[1].status, FixtureStatus::Staged);
        migrated.restore(staged, AttemptId::new()).unwrap();
        assert_eq!(
            migrated.snapshot().unwrap().fixtures[0].status,
            FixtureStatus::Restored
        );

        let parent = tempfile::tempdir().unwrap();
        let (life, _) = FixtureLifecycle::create(parent.path(), bindings()).unwrap();
        downgrade_to_v1(&life.db);
        let before = dump_all(&life.db);
        let id = life.session_id();
        drop(life);
        let (reopened, empty) = FixtureLifecycle::reopen(parent.path(), id).unwrap();
        assert_eq!(
            dump_all(&reopened.db),
            before,
            "empty-session migration must also preserve the exact rowids and columns"
        );
        assert!(empty
            .fixtures
            .iter()
            .all(|f| f.status == FixtureStatus::Available));
    }

    #[test]
    fn reopen_refuses_future_schema_version_without_reset() {
        // A schema_version newer than this build understands must fail closed
        // (InvalidSession) and must never reset or rewrite the database. Only
        // the exact legacy v1 is migrated; any other value is refused.
        // user_version is a signed 32-bit field in the SQLite header, so the
        // extreme case is i32::MAX, not i64::MAX (which would truncate).
        for future in [0_i64, 3, 9, i32::MAX as i64] {
            let parent = tempfile::tempdir().unwrap();
            let ids = bindings();
            let (life, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
            life.db
                .execute_batch(&format!("PRAGMA user_version = {future};"))
                .unwrap();
            assert_eq!(
                life.db
                    .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                future,
                "setup must actually persist user_version={future}"
            );
            let baseline = dump_all(&life.db);
            let id = life.session_id();
            drop(life);
            assert!(
                matches!(
                    FixtureLifecycle::reopen(parent.path(), id),
                    Err(LifecycleError::InvalidSession)
                ),
                "future schema_version {future} must be refused, not migrated"
            );
            // Re-open raw to prove nothing was reset or rewritten.
            let raw = Connection::open_with_flags(
                parent
                    .path()
                    .join(format!("{ROOT_PREFIX}{}", typed_component(&id).unwrap()))
                    .join(DB_NAME),
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .unwrap();
            assert_eq!(
                raw.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                future,
                "refusal must not rewrite user_version for {future}"
            );
            assert_eq!(
                dump_all(&raw),
                baseline,
                "refusal must not rewrite any row for {future}"
            );
        }
    }

    #[test]
    fn migration_fault_rolls_back_and_open_schema_changes_block_file_operations() {
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (mut life, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        let staged = life.stage(ids[0], AttemptId::new()).unwrap();
        downgrade_to_v1(&life.db);
        let baseline = dump_all(&life.db);
        for fault in [
            MigrationFault::AfterRename,
            MigrationFault::AfterCreate,
            MigrationFault::AfterCopy,
            MigrationFault::BeforeVersion,
        ] {
            assert!(
                matches!(
                    migrate_legacy_v1_inner(
                        &life.db,
                        life.id,
                        &life.parent,
                        &life.root_dir,
                        &life.source_dir,
                        &life.holding_dir,
                        fault
                    ),
                    Err(LifecycleError::InvalidSession)
                ),
                "fault {fault:?} must fail closed with InvalidSession, not an unrelated error"
            );
            assert_eq!(
                life.db
                    .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                LEGACY_SCHEMA_VERSION
            );
            assert_eq!(
                dump_all(&life.db),
                baseline,
                "fault {fault:?} must roll back every rowid and column, not just user_version"
            );
        }
        validate_schema_version(&life.db, LEGACY_SCHEMA_VERSION, true).unwrap();
        let id = life.session_id();
        drop(life);
        let (mut migrated, view) = FixtureLifecycle::reopen(parent.path(), id).unwrap();
        assert_eq!(
            dump_all(&migrated.db),
            baseline,
            "successful migration after earlier faults must preserve every rowid and column"
        );
        assert_eq!(view.fixtures[0].receipt_id, Some(staged));
        migrated
            .db
            .execute_batch(
                "CREATE TRIGGER unexpected AFTER UPDATE ON fixtures BEGIN SELECT 1; END;",
            )
            .unwrap();
        assert!(matches!(
            migrated.restore(staged, AttemptId::new()),
            Err(LifecycleError::InvalidSession)
        ));
        assert!(matches!(
            migrated.snapshot(),
            Err(LifecycleError::InvalidSession)
        ));
        assert!(migrated
            .root_path
            .join("holding")
            .join(receipt_slot(staged).unwrap())
            .exists());
    }

    #[test]
    fn stored_permanently_removed_status_maps_to_snapshot_and_validates() {
        // Once the purge primitive (Task 2) records the terminal state, a
        // stored 'permanently_removed' fixture row must validate and map to
        // FixtureStatus::PermanentlyRemoved — never to Changed, because the
        // holding file is expected to be gone. Simulated here by direct SQL
        // on a staged session, exactly as the terminal outcome will store it.
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (mut life, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        let receipt = life.stage(ids[0], AttemptId::new()).unwrap();
        life.db
            .execute(
                "UPDATE fixtures SET status='permanently_removed' WHERE status='staged'",
                [],
            )
            .unwrap();
        life.db
            .execute(
                "INSERT INTO attempts(attempt_id,action_id,operation,receipt_id,outcome) VALUES(?1,?2,'purge',?3,'permanently_removed')",
                params![typed_wire(&AttemptId::new()).unwrap(), encode_action(&ids[0]).unwrap(), typed_wire(&receipt).unwrap()],
            )
            .unwrap();
        let snapshot = life.snapshot().unwrap();
        assert_eq!(
            snapshot.fixtures[0].status,
            FixtureStatus::PermanentlyRemoved
        );
        assert_eq!(snapshot.fixtures[0].receipt_id, Some(receipt));
        // The purge operation with a non-terminal stage outcome stays rejected.
        life.db
            .execute(
                "INSERT INTO attempts(attempt_id,action_id,operation,receipt_id,outcome) VALUES(?1,?2,'purge',?3,'staged')",
                params![typed_wire(&AttemptId::new()).unwrap(), encode_action(&ids[1]).unwrap(), Option::<String>::None],
            )
            .unwrap();
        assert!(matches!(
            life.snapshot(),
            Err(LifecycleError::InvalidSession)
        ));
    }

    #[test]
    fn purge_states_require_issued_staged_receipts_and_stay_preserved() {
        // Since the purge primitive activated parts of the v2 vocabulary,
        // these blocks pin the remaining refusals — each offending row must
        // survive untouched: no deletion, no reset, no rewrite.
        {
            // A purge attempt for a receipt that no stage attempt ever
            // issued is refused by the registry cross-check.
            let parent = tempfile::tempdir().unwrap();
            let ids = bindings();
            let (life, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
            life.db
                .execute(
                    "INSERT INTO attempts(attempt_id,action_id,operation,receipt_id,outcome) VALUES(?1,?2,'purge',?3,'intent')",
                    params![typed_wire(&AttemptId::new()).unwrap(), encode_action(&ids[0]).unwrap(), typed_wire(&ReceiptId::new()).unwrap()],
                )
                .unwrap();
            assert!(matches!(
                life.snapshot(),
                Err(LifecycleError::InvalidSession)
            ));
            assert_eq!(
                life.db
                    .query_row("SELECT operation FROM attempts LIMIT 1", [], |r| r
                        .get::<_, String>(0))
                    .unwrap(),
                "purge"
            );
        }
        {
            // outcome='permanently_removed'
            let parent = tempfile::tempdir().unwrap();
            let ids = bindings();
            let (life, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
            life.db
                .execute(
                    "INSERT INTO attempts(attempt_id,action_id,operation,receipt_id,outcome) VALUES(?1,?2,'stage',?3,'permanently_removed')",
                    params![typed_wire(&AttemptId::new()).unwrap(), encode_action(&ids[0]).unwrap(), typed_wire(&ReceiptId::new()).unwrap()],
                )
                .unwrap();
            assert!(matches!(
                life.snapshot(),
                Err(LifecycleError::InvalidSession)
            ));
            assert_eq!(
                life.db
                    .query_row("SELECT outcome FROM attempts LIMIT 1", [], |r| r
                        .get::<_, String>(0))
                    .unwrap(),
                "permanently_removed"
            );
        }
        {
            // status='permanently_removed' without any receipt: the status
            // requires the purged receipt to be registered on the row.
            let parent = tempfile::tempdir().unwrap();
            let ids = bindings();
            let (life, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
            let changed = life
                .db
                .execute("UPDATE fixtures SET status='permanently_removed'", [])
                .unwrap();
            assert_eq!(changed, 5, "fixed fixture set must be five rows");
            assert!(matches!(
                life.snapshot(),
                Err(LifecycleError::InvalidSession)
            ));
            let preserved: i64 = life
                .db
                .query_row(
                    "SELECT COUNT(*) FROM fixtures WHERE status='permanently_removed'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(preserved, 5, "all offending rows must be preserved");
        }
    }
    #[test]
    fn purge_receipt_removes_staged_fixture_and_records_terminal_outcome() {
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (mut life, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        let receipt = life.stage(ids[0], AttemptId::new()).unwrap();
        let logical_bytes = life.snapshot().unwrap().fixtures[0].bytes;
        let outcome = life.purge_receipt(receipt, AttemptId::new()).unwrap();
        assert_eq!(
            outcome,
            PurgeItemOutcome::PermanentlyRemoved {
                receipt_id: receipt,
                logical_bytes
            }
        );
        let slot = receipt_slot(receipt).unwrap();
        assert!(open_regular(&life.holding_dir, &slot).is_err());
        let recorded: String = life
            .db
            .query_row(
                "SELECT outcome FROM attempts WHERE operation='purge'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(recorded, "permanently_removed");
        let snap = life.snapshot().unwrap();
        assert_eq!(snap.fixtures[0].status, FixtureStatus::PermanentlyRemoved);
        assert_eq!(snap.fixtures[0].receipt_id, Some(receipt));
        assert_eq!(snap.fixtures[1].status, FixtureStatus::Available);
        let sid = life.session_id();
        drop(life);
        let (mut reopened, view) = FixtureLifecycle::reopen(parent.path(), sid).unwrap();
        assert_eq!(view.fixtures[0].status, FixtureStatus::PermanentlyRemoved);
        assert!(matches!(
            reopened.restore(receipt, AttemptId::new()),
            Err(LifecycleError::WrongState)
        ));
    }

    #[test]
    fn purge_receipt_refuses_non_staged_unknown_or_changed_targets_without_mutation() {
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (mut life, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        // Unknown receipt: refused, nothing journaled.
        assert!(matches!(
            life.purge_receipt(ReceiptId::new(), AttemptId::new()),
            Err(LifecycleError::UnknownReceipt)
        ));
        // Restored (not staged) receipt: refused as wrong state.
        let receipt = life.stage(ids[0], AttemptId::new()).unwrap();
        life.restore(receipt, AttemptId::new()).unwrap();
        assert!(matches!(
            life.purge_receipt(receipt, AttemptId::new()),
            Err(LifecycleError::WrongState)
        ));
        // Changed content: refused before any intent, file kept, state kept.
        let receipt2 = life.stage(ids[1], AttemptId::new()).unwrap();
        let slot = receipt_slot(receipt2).unwrap();
        {
            // Overwrite the first bytes in place (no truncate) via a
            // write-capable handle: same size, different digest.
            let path = life.root.join("holding").join(&slot);
            let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(0)).unwrap();
            std::io::Write::write_all(&mut file, b"TAMPERED").unwrap();
            file.sync_all().unwrap();
        }
        assert!(matches!(
            life.purge_receipt(receipt2, AttemptId::new()),
            Err(LifecycleError::Changed)
        ));
        let purge_attempts: i64 = life
            .db
            .query_row(
                "SELECT COUNT(*) FROM attempts WHERE operation='purge'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(purge_attempts, 0, "no purge intent may be journaled");
        assert!(open_regular(&life.holding_dir, &slot).is_ok());
        // The database row is unchanged; the snapshot honestly reports the
        // tampered file as Changed (the tamper is real and must be visible).
        let stored_status: String = life
            .db
            .query_row(
                "SELECT status FROM fixtures WHERE action_id=?1",
                [encode_action(&ids[1]).unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored_status, "staged");
        assert_eq!(
            life.snapshot().unwrap().fixtures[1].status,
            FixtureStatus::Changed
        );
    }

    #[test]
    fn purge_unlink_failure_is_uncertain_and_keeps_the_file() {
        use std::os::unix::fs::PermissionsExt;
        // Make the holding directory non-writable so unlinkat fails after
        // verification and intent: the result cannot be established, so the
        // receipt state must become Uncertain — never a success claim.
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (mut life, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        let receipt = life.stage(ids[0], AttemptId::new()).unwrap();
        life.holding_dir
            .set_permissions(std::fs::Permissions::from_mode(0o500))
            .unwrap();
        let outcome = life.purge_receipt(receipt, AttemptId::new());
        // Restore permissions so the tempdir can be cleaned up.
        life.holding_dir
            .set_permissions(std::fs::Permissions::from_mode(0o700))
            .unwrap();
        assert!(
            matches!(outcome, Err(LifecycleError::Uncertain)),
            "unlink failure must report uncertainty, got {outcome:?}"
        );
        // The file itself survived: unlink was refused, not silently done.
        let slot = receipt_slot(receipt).unwrap();
        assert!(open_regular(&life.holding_dir, &slot).is_ok());
        // The attempt journal shows uncertain; the fixture is uncertain.
        let recorded: String = life
            .db
            .query_row(
                "SELECT outcome FROM attempts WHERE operation='purge'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(recorded, "uncertain");
        let status: String = life
            .db
            .query_row(
                "SELECT status FROM fixtures WHERE action_id=?1",
                [encode_action(&ids[0]).unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "uncertain");
        // Restore is blocked by the uncertain state.
        assert!(matches!(
            life.restore(receipt, AttemptId::new()),
            Err(LifecycleError::WrongState)
        ));
    }

    #[test]
    fn purge_intent_crash_reopens_uncertain_without_retry_or_success_claim() {
        // Simulate a crash between the durable purge intent and the unlink:
        // journal the intent directly (the same private API purge_receipt
        // uses), drop the process handle, and reopen.
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (mut life, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        let receipt = life.stage(ids[0], AttemptId::new()).unwrap();
        let fixture = life.lookup_receipt(receipt).unwrap().unwrap();
        life.record_intent(AttemptId::new(), &fixture, "purge", Some(receipt))
            .unwrap();
        let sid = life.session_id();
        drop(life);

        let (mut reopened, view) = FixtureLifecycle::reopen(parent.path(), sid).unwrap();
        assert_eq!(view.fixtures[0].status, FixtureStatus::Uncertain);
        // The file was never unlinked and stays untouched in holding.
        let slot = receipt_slot(receipt).unwrap();
        assert!(open_regular(&reopened.holding_dir, &slot).is_ok());
        // No automatic resolution, retry, or restore until inspection.
        assert!(matches!(
            reopened.restore(receipt, AttemptId::new()),
            Err(LifecycleError::Uncertain)
        ));
        assert!(matches!(
            reopened.purge_receipt(receipt, AttemptId::new()),
            Err(LifecycleError::Uncertain)
        ));
        // The pending intent row is preserved exactly as the crash left it.
        let pending: i64 = reopened
            .db
            .query_row(
                "SELECT COUNT(*) FROM attempts WHERE outcome='intent' AND operation='purge'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pending, 1, "intent row must survive untouched");
    }

    #[test]
    fn purge_is_terminal_and_serial_batch_touches_only_selected_receipts() {
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (mut life, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        // Stage three, leave two available.
        let receipts: Vec<ReceiptId> = ids[..3]
            .iter()
            .map(|id| life.stage(*id, AttemptId::new()).unwrap())
            .collect();
        // Purge all three staged receipts serially.
        for (index, receipt) in receipts.iter().enumerate() {
            let outcome = life
                .purge_receipt(*receipt, AttemptId::new())
                .unwrap_or_else(|e| panic!("purge {index} failed: {e:?}"));
            assert!(matches!(
                outcome,
                PurgeItemOutcome::PermanentlyRemoved { logical_bytes, .. }
                    if logical_bytes == FIXTURES[index].1
            ));
        }
        // Terminal refusal, including after a reopen.
        let sid = life.session_id();
        drop(life);
        let (mut reopened, view) = FixtureLifecycle::reopen(parent.path(), sid).unwrap();
        for (index, receipt) in receipts.iter().enumerate() {
            assert_eq!(
                view.fixtures[index].status,
                FixtureStatus::PermanentlyRemoved
            );
            assert!(matches!(
                reopened.restore(*receipt, AttemptId::new()),
                Err(LifecycleError::WrongState)
            ));
            assert!(matches!(
                reopened.purge_receipt(*receipt, AttemptId::new()),
                Err(LifecycleError::WrongState)
            ));
        }
        // The two untouched fixtures are exactly as they were: available,
        // with byte-identical file content and database rows.
        for (index, id) in ids.iter().enumerate().skip(3) {
            assert_eq!(view.fixtures[index].status, FixtureStatus::Available);
            let expected = reopened.lookup_action(*id).unwrap().unwrap();
            reopened
                .verify_fixture(&expected, &reopened.source_dir, &expected.slot)
                .unwrap();
            let _ = id;
        }
        // Attempt history is append-only: every original row (stage
        // history) is preserved, with only purge rows added afterwards.
        // attempts columns: 0 attempt_id, 1 action_id, 2 operation,
        // 3 receipt_id, 4 outcome.
        let stage_rows = dump_table(&reopened.db, "attempts")
            .into_iter()
            .filter(|(_, row)| {
                !matches!(
                    row.get(2),
                    Some(rusqlite::types::Value::Text(operation)) if operation == "purge"
                )
            })
            .count();
        assert_eq!(stage_rows, 3, "three stage attempts must be preserved");
    }

    #[test]
    fn outcome_journaling_requires_a_live_attempt_row() {
        // The journal contract is self-enforcing: an outcome update must
        // match exactly one existing attempt row.
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (mut life, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        let receipt = life.stage(ids[0], AttemptId::new()).unwrap();
        let fixture = life.lookup_receipt(receipt).unwrap().unwrap();
        assert!(matches!(
            life.record_outcome(
                AttemptId::new(),
                &fixture.action_id,
                "staged",
                Some(receipt)
            ),
            Err(LifecycleError::InvalidSession)
        ));
    }

    #[test]
    fn purge_journal_rows_require_issued_and_staged_receipt_history() {
        // A hand-crafted database must not pass validation when its purge
        // rows have no matching stage history: the receipt was never issued
        // by a stage attempt, so the permanently_removed claim is fabricated.
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (life, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        life.db
            .execute(
                "UPDATE fixtures SET status='permanently_removed', receipt_id=?2 WHERE action_id=?1",
                params![encode_action(&ids[0]).unwrap(), typed_wire(&ReceiptId::new()).unwrap()],
            )
            .unwrap();
        life.db
            .execute(
                "INSERT INTO attempts(attempt_id,action_id,operation,receipt_id,outcome) VALUES(?1,?2,'purge',?3,'permanently_removed')",
                params![typed_wire(&AttemptId::new()).unwrap(), encode_action(&ids[0]).unwrap(), life.db.query_row("SELECT receipt_id FROM fixtures WHERE action_id=?1", [encode_action(&ids[0]).unwrap()], |r| r.get::<_, String>(0)).unwrap()],
            )
            .unwrap();
        let sid = life.session_id();
        drop(life);
        assert!(
            matches!(
                FixtureLifecycle::reopen(parent.path(), sid),
                Err(LifecycleError::InvalidSession)
            ),
            "a purge row without stage history must fail closed"
        );
    }

    #[test]
    fn permanently_removed_fixture_requires_a_terminal_purge_row() {
        // A terminal fixture state must be backed by its terminal journal
        // row: stage history alone cannot make a fixture permanently
        // removed.
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (mut life, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        let receipt = life.stage(ids[0], AttemptId::new()).unwrap();
        life.db
            .execute(
                "UPDATE fixtures SET status='permanently_removed' WHERE action_id=?1",
                [encode_action(&ids[0]).unwrap()],
            )
            .unwrap();
        let sid = life.session_id();
        drop(life);
        assert!(
            matches!(
                FixtureLifecycle::reopen(parent.path(), sid),
                Err(LifecycleError::InvalidSession)
            ),
            "permanently_removed without a purge journal row must fail closed"
        );
        let _ = receipt;
    }

    #[test]
    fn stage_restore_reopen_and_fixed_five_files() {
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (mut lifecycle, snapshot) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        assert_eq!(
            snapshot
                .fixtures
                .iter()
                .map(|f| f.bytes)
                .collect::<Vec<_>>(),
            FIXTURES.iter().map(|v| v.1).collect::<Vec<_>>()
        );
        let receipt = lifecycle.stage(ids[0], AttemptId::new()).unwrap();
        assert_eq!(
            lifecycle.snapshot().unwrap().fixtures[0].status,
            FixtureStatus::Staged
        );
        let sid = lifecycle.session_id();
        drop(lifecycle);
        let (mut reopened, view) = FixtureLifecycle::reopen(parent.path(), sid).unwrap();
        assert_eq!(view.fixtures[0].receipt_id, Some(receipt));
        reopened.restore(receipt, AttemptId::new()).unwrap();
        assert_eq!(
            reopened.snapshot().unwrap().fixtures[0].status,
            FixtureStatus::Restored
        );
    }
    #[test]
    fn coordinator_session_id_creation_is_exclusive_and_identity_binds_inode() {
        let parent = tempfile::tempdir().unwrap();
        let actions = bindings();
        let first_id = SessionId::new();
        let (first, first_snapshot) =
            FixtureLifecycle::create_for_session(parent.path(), first_id, actions).unwrap();
        let (second, second_snapshot) =
            FixtureLifecycle::create_for_session(parent.path(), SessionId::new(), actions).unwrap();
        assert_eq!(
            first_snapshot.fixtures[0].sha256,
            second_snapshot.fixtures[0].sha256
        );
        assert_ne!(
            first_snapshot.fixtures[0].identity_sha256,
            second_snapshot.fixtures[0].identity_sha256
        );
        let sentinel = first.root.join("source").join(FIXTURES[0].0);
        fs::write(&sentinel, b"preserve-existing-session").unwrap();
        assert!(FixtureLifecycle::create_for_session(parent.path(), first_id, actions).is_err());
        assert_eq!(fs::read(sentinel).unwrap(), b"preserve-existing-session");
        drop((first, second));
    }
    #[test]
    fn duplicate_attempt_is_rejected_without_second_move() {
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (mut x, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        let attempt = AttemptId::new();
        let receipt = x.stage(ids[0], attempt).unwrap();
        assert!(matches!(
            x.restore(receipt, attempt),
            Err(LifecycleError::DuplicateAttempt)
        ));
        assert_eq!(
            x.snapshot().unwrap().fixtures[0].status,
            FixtureStatus::Staged
        );
    }
    #[test]
    fn restore_collision_preserves_both_objects() {
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (mut x, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        let receipt = x.stage(ids[0], AttemptId::new()).unwrap();
        fs::write(x.root.join("source").join(FIXTURES[0].0), b"sentinel").unwrap();
        assert!(matches!(
            x.restore(receipt, AttemptId::new()),
            Err(LifecycleError::Collision)
        ));
        assert_eq!(
            fs::read(x.root.join("source").join(FIXTURES[0].0)).unwrap(),
            b"sentinel"
        );
        assert!(x
            .root
            .join("holding")
            .join(receipt_slot(receipt).unwrap())
            .exists());
    }
    #[test]
    fn changed_fixture_and_parent_replacement_detected() {
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (mut x, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        fs::write(x.root.join("source").join(FIXTURES[0].0), b"tampered").unwrap();
        assert!(matches!(
            x.stage(ids[0], AttemptId::new()),
            Err(LifecycleError::Changed)
        ));
    }
    #[test]
    fn parent_symlink_replacement_is_rejected() {
        let container = tempfile::tempdir().unwrap();
        let parent_path = container.path().join("app-state");
        fs::create_dir(&parent_path).unwrap();
        let ids = bindings();
        let (mut lifecycle, _) = FixtureLifecycle::create(&parent_path, ids).unwrap();
        let moved_parent = container.path().join("moved-app-state");
        fs::rename(&parent_path, &moved_parent).unwrap();
        std::os::unix::fs::symlink(&moved_parent, &parent_path).unwrap();
        assert!(matches!(
            lifecycle.stage(ids[0], AttemptId::new()),
            Err(LifecycleError::Changed)
        ));
    }
    #[test]
    fn changed_holding_fixture_blocks_restore() {
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (mut lifecycle, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        let receipt = lifecycle.stage(ids[0], AttemptId::new()).unwrap();
        let held = lifecycle
            .root
            .join("holding")
            .join(receipt_slot(receipt).unwrap());
        fs::write(&held, b"changed after isolation").unwrap();
        assert!(matches!(
            lifecycle.restore(receipt, AttemptId::new()),
            Err(LifecycleError::Changed)
        ));
    }
    #[test]
    fn interrupted_intent_reopens_uncertain_and_blocks_retry() {
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (mut lifecycle, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        let action = lifecycle.lookup_action(ids[0]).unwrap().unwrap();
        let receipt = ReceiptId::new();
        lifecycle
            .record_intent(AttemptId::new(), &action, "stage", Some(receipt))
            .unwrap();
        let session = lifecycle.session_id();
        drop(lifecycle);
        let (mut reopened, snapshot) = FixtureLifecycle::reopen(parent.path(), session).unwrap();
        assert_eq!(snapshot.fixtures[0].status, FixtureStatus::Uncertain);
        assert_eq!(snapshot.fixtures[0].receipt_id, Some(receipt));
        assert!(matches!(
            reopened.stage(ids[0], AttemptId::new()),
            Err(LifecycleError::Uncertain)
        ));
    }
    #[test]
    fn interruption_after_rename_reopens_uncertain_without_repeating_move() {
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (mut lifecycle, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        let action = lifecycle.lookup_action(ids[0]).unwrap().unwrap();
        let receipt = ReceiptId::new();
        lifecycle
            .record_intent(AttemptId::new(), &action, "stage", Some(receipt))
            .unwrap();
        rename_no_replace(
            &lifecycle.source_dir,
            FIXTURES[0].0,
            &lifecycle.holding_dir,
            &receipt_slot(receipt).unwrap(),
        )
        .unwrap();
        let session = lifecycle.session_id();
        drop(lifecycle);
        let (mut reopened, snapshot) = FixtureLifecycle::reopen(parent.path(), session).unwrap();
        assert_eq!(snapshot.fixtures[0].status, FixtureStatus::Uncertain);
        assert_eq!(snapshot.fixtures[0].receipt_id, Some(receipt));
        assert!(matches!(
            reopened.stage(ids[0], AttemptId::new()),
            Err(LifecycleError::Uncertain)
        ));
        assert!(reopened
            .root
            .join("holding")
            .join(receipt_slot(receipt).unwrap())
            .exists());
    }
    #[test]
    fn unknown_database_object_is_rejected_without_resetting() {
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (lifecycle, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        let session = lifecycle.session_id();
        let db_path = lifecycle.root.join(DB_NAME);
        drop(lifecycle);
        let tampered = Connection::open(&db_path).unwrap();
        tampered
            .execute_batch(
                "CREATE TRIGGER unexpected AFTER UPDATE ON fixtures BEGIN SELECT 1; END;",
            )
            .unwrap();
        drop(tampered);
        assert!(matches!(
            FixtureLifecycle::reopen(parent.path(), session),
            Err(LifecycleError::InvalidSession)
        ));
        let check = Connection::open(&db_path).unwrap();
        let trigger_count: i64 = check
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type='trigger'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(trigger_count, 1);
    }
    #[test]
    fn sqlite_lookalike_name_with_wildcard_character_is_not_hidden() {
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (lifecycle, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        let session = lifecycle.session_id();
        let db_path = lifecycle.root.join(DB_NAME);
        drop(lifecycle);
        let tampered = Connection::open(&db_path).unwrap();
        tampered
            .execute_batch("CREATE TABLE sqliteXevil(value TEXT NOT NULL);")
            .unwrap();
        drop(tampered);
        assert!(matches!(
            FixtureLifecycle::reopen(parent.path(), session),
            Err(LifecycleError::InvalidSession)
        ));
    }
    #[test]
    fn altered_slot_is_rejected_before_outside_sentinel_access() {
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (mut lifecycle, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        let sentinel = parent.path().join("sentinel");
        fs::write(&sentinel, b"outside fixture sentinel").unwrap();
        lifecycle
            .db
            .execute(
                "UPDATE fixtures SET slot='../../sentinel' WHERE action_id=?1",
                [encode_action(&ids[0]).unwrap()],
            )
            .unwrap();
        assert!(matches!(
            lifecycle.stage(ids[0], AttemptId::new()),
            Err(LifecycleError::InvalidSession)
        ));
        assert!(open_regular(&lifecycle.source_dir, "../../sentinel").is_err());
        assert!(matches!(
            rename_no_replace(
                &lifecycle.source_dir,
                "../../sentinel",
                &lifecycle.holding_dir,
                &receipt_slot(ReceiptId::new()).unwrap(),
            ),
            Err(RenameFailure::InvalidComponent)
        ));
        assert_eq!(fs::read(sentinel).unwrap(), b"outside fixture sentinel");
    }
    #[test]
    fn concurrent_handles_only_allow_one_action_claim() {
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (lifecycle, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        let session = lifecycle.session_id();
        drop(lifecycle);
        let (first, _) = FixtureLifecycle::reopen(parent.path(), session).unwrap();
        let (second, _) = FixtureLifecycle::reopen(parent.path(), session).unwrap();
        let first_fixture = first.lookup_action(ids[0]).unwrap().unwrap();
        let second_fixture = second.lookup_action(ids[0]).unwrap().unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let first_barrier = barrier.clone();
        let first_thread = std::thread::spawn(move || {
            let mut lifecycle = first;
            first_barrier.wait();
            lifecycle.record_intent(
                AttemptId::new(),
                &first_fixture,
                "stage",
                Some(ReceiptId::new()),
            )
        });
        let second_barrier = barrier.clone();
        let second_thread = std::thread::spawn(move || {
            let mut lifecycle = second;
            second_barrier.wait();
            lifecycle.record_intent(
                AttemptId::new(),
                &second_fixture,
                "stage",
                Some(ReceiptId::new()),
            )
        });
        let outcomes = [first_thread.join().unwrap(), second_thread.join().unwrap()];
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|result| matches!(result, Err(LifecycleError::Uncertain)))
                .count(),
            1
        );
    }
    #[test]
    fn changed_schema_constraints_are_rejected_without_reset() {
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (lifecycle, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        let session = lifecycle.session_id();
        let db_path = lifecycle.root.join(DB_NAME);
        drop(lifecycle);
        let tampered = Connection::open(&db_path).unwrap();
        tampered
            .execute_batch("PRAGMA writable_schema=ON; UPDATE sqlite_schema SET sql=replace(sql, \"CHECK(status IN ('available','staged','restored','uncertain','permanently_removed'))\", \"CHECK(status != 'forbidden')\") WHERE name='fixtures'; PRAGMA writable_schema=OFF;")
            .unwrap();
        drop(tampered);
        assert!(matches!(
            FixtureLifecycle::reopen(parent.path(), session),
            Err(LifecycleError::InvalidSession)
        ));
    }
    #[test]
    fn bounded_digest_stops_reading_a_stream_that_keeps_growing() {
        struct GrowingReader(u64);
        impl Read for GrowingReader {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                buffer.fill(0x5a);
                self.0 += buffer.len() as u64;
                Ok(buffer.len())
            }
        }
        let mut reader = GrowingReader(0);
        assert!(matches!(
            digest_reader(&mut reader, 19),
            Err(LifecycleError::Changed)
        ));
        assert_eq!(reader.0, 20);
    }
    #[test]
    fn database_symlink_target_is_not_followed() {
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (lifecycle, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        let session = lifecycle.session_id();
        let db_path = lifecycle.root.join(DB_NAME);
        let preserved = lifecycle.root.join("preserved.sqlite3");
        drop(lifecycle);
        fs::rename(&db_path, &preserved).unwrap();
        std::os::unix::fs::symlink(&preserved, &db_path).unwrap();
        assert!(FixtureLifecycle::reopen(parent.path(), session).is_err());
        assert!(fs::symlink_metadata(&db_path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(preserved.exists());
    }
    #[test]
    fn symlink_slot_is_rejected() {
        let parent = tempfile::tempdir().unwrap();
        let ids = bindings();
        let (mut x, _) = FixtureLifecycle::create(parent.path(), ids).unwrap();
        let path = x.root.join("source").join(FIXTURES[0].0);
        let data = fs::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        fs::write(x.root.join("payload"), data).unwrap();
        std::os::unix::fs::symlink(x.root.join("payload"), path).unwrap();
        assert!(matches!(
            x.stage(ids[0], AttemptId::new()),
            Err(LifecycleError::Io(_) | LifecycleError::Changed)
        ));
    }
}
