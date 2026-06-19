use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};

pub struct SnapshotManager {
    snap_dir: PathBuf,
    index: Mutex<Vec<SnapshotEntry>>,
}

impl fmt::Debug for SnapshotManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotManager")
            .field("snap_dir", &self.snap_dir)
            .field("index_len", &self.index.lock().unwrap().len())
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct SnapshotEntry {
    pub file_path: String,
    pub backup_path: String,
    pub timestamp: String,
}

pub struct SnapshotInfo {
    pub file_path: String,
    pub backup_path: String,
    pub timestamp: String,
    pub can_undo: bool,
}

impl SnapshotManager {
    pub fn new(session_id: &str) -> Result<Self> {
        let home = std::env::var("HOME").context("HOME not set")?;
        let snap_dir = PathBuf::from(&home)
            .join(".local")
            .join("share")
            .join("agent-engine")
            .join("snapshots")
            .join(sanitize_id(session_id));
        std::fs::create_dir_all(&snap_dir).context("Failed to create snapshots directory")?;
        Ok(Self {
            snap_dir,
            index: Mutex::new(Vec::new()),
        })
    }

    /// Save a snapshot of the file at `path` before modifying it.
    /// Returns the path to the backup file, or None if the file doesn't exist yet.
    pub fn snapshot(&self, path: &str) -> Result<Option<String>> {
        let src = Path::new(path);
        if !src.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(src)
            .context(format!("Failed to read file for snapshot: {}", path))?;
        let backup_name = format!(
            "{}_{}",
            chrono::Utc::now().timestamp_millis(),
            sanitize_filename(path)
        );
        let backup_path = self.snap_dir.join(&backup_name);
        std::fs::write(&backup_path, &content)
            .context(format!("Failed to write snapshot: {}", backup_path.display()))?;

        let entry = SnapshotEntry {
            file_path: path.to_string(),
            backup_path: backup_path.to_string_lossy().to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        self.index.lock().unwrap().push(entry);

        Ok(Some(backup_path.to_string_lossy().to_string()))
    }

    /// Undo the last snapshot for the given file path.
    /// Returns Ok(true) if restored, Ok(false) if no snapshot found.
    pub fn undo(&self, file_path: &str) -> Result<bool> {
        let mut index = self.index.lock().unwrap();
        if let Some(pos) = index.iter().rposition(|e| e.file_path == file_path) {
            let entry = index.remove(pos);
            let backup = std::fs::read_to_string(&entry.backup_path)
                .context(format!("Failed to read snapshot: {}", entry.backup_path))?;
            std::fs::write(&file_path, &backup)
                .context(format!("Failed to restore file: {}", file_path))?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// List all snapshots for inspection.
    pub fn list_snapshots(&self) -> Vec<SnapshotEntry> {
        self.index.lock().unwrap().clone()
    }

    /// Check if undo is available for a file.
    pub fn can_undo(&self, file_path: &str) -> bool {
        self.index
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.file_path == file_path)
    }
}

fn sanitize_id(id: &str) -> String {
    id.replace(|c: char| !c.is_alphanumeric(), "_")
}

fn sanitize_filename(path: &str) -> String {
    path.replace('/', "_")
        .replace(|c: char| c.is_whitespace(), "_")
}
