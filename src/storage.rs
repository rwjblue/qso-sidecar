use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::adif::ImportDiagnostics;
use crate::log_source::LogSourceKind;
use crate::model::{Qso, RecordDiagnostic};

const STATE_VERSION: u8 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedState {
    version: u8,
    pub qsos: BTreeMap<String, Qso>,
    pub selected_operation: Option<String>,
    pub source_kind: Option<LogSourceKind>,
    pub source: String,
    pub source_freshness: Option<DateTime<Utc>>,
    pub import_diagnostics: ImportDiagnostics,
    #[serde(default)]
    pub source_diagnostics: Vec<RecordDiagnostic>,
}

impl PersistedState {
    pub fn new(
        qsos: BTreeMap<String, Qso>,
        selected_operation: Option<String>,
        source_kind: Option<LogSourceKind>,
        source: String,
        source_freshness: Option<DateTime<Utc>>,
        import_diagnostics: ImportDiagnostics,
        source_diagnostics: Vec<RecordDiagnostic>,
    ) -> Self {
        Self {
            version: STATE_VERSION,
            qsos,
            selected_operation,
            source_kind,
            source,
            source_freshness,
            import_diagnostics,
            source_diagnostics,
        }
    }
}

#[derive(Debug, Clone)]
pub struct StateStore {
    path: Arc<PathBuf>,
    write_lock: Arc<Mutex<()>>,
}

impl StateStore {
    pub fn for_app() -> Result<Self> {
        let project = ProjectDirs::from("net", "rwjblue", "qso-sidecar")
            .context("operating system has no application-data directory")?;
        Self::at(project.data_local_dir().join("last-good-state.json"))
    }

    pub fn at(path: PathBuf) -> Result<Self> {
        let parent = path
            .parent()
            .context("state path has no parent directory")?;
        secure_directory(parent)?;
        Ok(Self {
            path: Arc::new(path),
            write_lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn load(&self) -> Result<Option<PersistedState>> {
        if !self.path.exists() {
            return Ok(None);
        }
        let body = fs::read(self.path.as_ref()).context("reading last-good log state")?;
        let state: PersistedState =
            serde_json::from_slice(&body).context("decoding last-good log state")?;
        ensure!(
            state.version == STATE_VERSION,
            "unsupported last-good state version {}",
            state.version
        );
        Ok(Some(state))
    }

    pub fn save(&self, state: &PersistedState) -> Result<()> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("state writer lock is poisoned"))?;
        let body = serde_json::to_vec(state)?;
        write_atomic(self.path.as_ref(), &body)
    }
}

fn write_atomic(path: &Path, body: &[u8]) -> Result<()> {
    let temporary = path.with_extension("tmp");
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .context("opening temporary last-good state")?;
    file.write_all(body)
        .context("writing temporary last-good state")?;
    file.sync_all()
        .context("syncing temporary last-good state")?;
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(path).context("removing previous last-good state")?;
    }
    fs::rename(&temporary, path).context("installing last-good state")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn secure_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(source: &str) -> PersistedState {
        PersistedState::new(
            BTreeMap::new(),
            Some("operation".into()),
            Some(LogSourceKind::Lofi),
            source.into(),
            Some(Utc::now()),
            ImportDiagnostics::default(),
            Vec::new(),
        )
    }

    #[test]
    fn round_trips_last_good_state() {
        let directory = tempfile::tempdir().unwrap();
        let store = StateStore::at(directory.path().join("state.json")).unwrap();
        store.save(&state("first")).unwrap();

        let restored = store.load().unwrap().unwrap();

        assert_eq!(restored.source, "first");
        assert_eq!(restored.selected_operation.as_deref(), Some("operation"));
    }

    #[test]
    fn replaces_existing_state_without_leaving_a_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let store = StateStore::at(path.clone()).unwrap();
        store.save(&state("first")).unwrap();
        store.save(&state("second")).unwrap();

        assert_eq!(store.load().unwrap().unwrap().source, "second");
        assert!(!path.with_extension("tmp").exists());
    }

    #[test]
    fn failed_atomic_write_preserves_previous_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let store = StateStore::at(path.clone()).unwrap();
        store.save(&state("last good")).unwrap();
        fs::create_dir(path.with_extension("tmp")).unwrap();

        assert!(store.save(&state("replacement")).is_err());
        assert_eq!(store.load().unwrap().unwrap().source, "last good");
    }

    #[cfg(unix)]
    #[test]
    fn state_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let store = StateStore::at(path.clone()).unwrap();
        store.save(&state("private")).unwrap();

        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
