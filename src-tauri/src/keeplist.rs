//! F10: persist Keep Running marks. The only state this app keeps across restarts —
//! CONTEXT.md "Right Now" rules out any history, and PLAN.md scopes this module to
//! exactly that one mark.
//!
//! Keyed by (project root path, command) — NEVER by pid, which a fresh process gets a
//! new one of on every restart (F10). Persisted as JSON in Tauri's app data dir.
//!
//! Takes a plain `&Path` for the storage directory rather than an `AppHandle`, so this
//! module's logic is unit-testable against a `tempfile::tempdir()` without spinning up
//! a real Tauri app — `commands.rs` is the only caller that resolves the real
//! `app_data_dir()` and passes it in.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "keeplist.json";

/// One Keep Running mark: project root plus the command that was marked. Both are
/// required — CONTEXT.md: "Remembered by what the Server is — its project and
/// command". A `BTreeSet` (not `HashMap`) because membership is the entire question
/// asked of this type ("is this Server marked?"), and a deterministic iteration order
/// makes the persisted JSON stable across saves (easier to diff/debug, and avoids
/// spurious rewrites of unrelated entries).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct KeepMark {
    pub project_root: PathBuf,
    pub command: String,
}

/// The in-memory Keep Running list, loaded from and saved to disk. Kept as a plain
/// struct rather than static/global state so `commands.rs` can own one instance behind
/// its own mutex alongside the rest of the scanner's shared state, rather than this
/// module inventing its own locking strategy.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Keeplist {
    marks: BTreeSet<KeepMark>,
}

impl Keeplist {
    /// Load from `dir/keeplist.json`. A missing file (first run) or unparseable file
    /// (corrupted by hand, or written by a future version) both start empty rather
    /// than erroring — losing Keep Running marks is an inconvenience the user can
    /// redo, not a reason to fail startup.
    pub fn load(dir: &Path) -> Self {
        let path = dir.join(FILE_NAME);
        match fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => Keeplist::default(),
        }
    }

    /// Save to `dir/keeplist.json`, creating `dir` if needed. Returns an error rather
    /// than silently discarding a failed write — a mark the user just set that
    /// silently fails to persist is exactly the kind of quiet data loss this module
    /// exists to prevent.
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        fs::create_dir_all(dir).map_err(|e| format!("failed to create app data dir {}: {e}", dir.display()))?;
        let json = serde_json::to_string_pretty(self).map_err(|e| format!("failed to serialize keeplist: {e}"))?;
        fs::write(dir.join(FILE_NAME), json).map_err(|e| format!("failed to write keeplist: {e}"))
    }

    pub fn is_marked(&self, project_root: &Path, command: &str) -> bool {
        self.marks.contains(&KeepMark { project_root: project_root.to_path_buf(), command: command.to_string() })
    }

    /// Set or clear the mark for (project_root, command). Idempotent either way —
    /// marking an already-marked Server, or clearing an unmarked one, is a no-op.
    pub fn set(&mut self, project_root: &Path, command: &str, keep: bool) {
        let mark = KeepMark { project_root: project_root.to_path_buf(), command: command.to_string() };
        if keep {
            self.marks.insert(mark);
        } else {
            self.marks.remove(&mark);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmarked_server_is_not_marked() {
        let list = Keeplist::default();
        assert!(!list.is_marked(Path::new("/proj"), "npm run dev"));
    }

    #[test]
    fn set_true_then_is_marked() {
        let mut list = Keeplist::default();
        list.set(Path::new("/proj"), "npm run dev", true);
        assert!(list.is_marked(Path::new("/proj"), "npm run dev"));
    }

    #[test]
    fn set_false_clears_mark() {
        let mut list = Keeplist::default();
        list.set(Path::new("/proj"), "npm run dev", true);
        list.set(Path::new("/proj"), "npm run dev", false);
        assert!(!list.is_marked(Path::new("/proj"), "npm run dev"));
    }

    #[test]
    fn keyed_by_project_and_command_not_pid() {
        // Same project, different command: not marked. Same command, different
        // project: not marked. This is the whole point of F10 — a mark must not
        // accidentally apply to an unrelated Server that merely restarted with a new
        // pid but otherwise matches neither key.
        let mut list = Keeplist::default();
        list.set(Path::new("/proj-a"), "npm run dev", true);
        assert!(!list.is_marked(Path::new("/proj-a"), "npm run build"));
        assert!(!list.is_marked(Path::new("/proj-b"), "npm run dev"));
    }

    #[test]
    fn survives_save_and_load_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut list = Keeplist::default();
        list.set(Path::new("/proj"), "npm run dev", true);
        list.save(dir.path()).expect("save must succeed");

        let reloaded = Keeplist::load(dir.path());
        assert!(reloaded.is_marked(Path::new("/proj"), "npm run dev"));
    }

    #[test]
    fn load_missing_file_starts_empty_not_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let list = Keeplist::load(dir.path());
        assert!(!list.is_marked(Path::new("/anything"), "anything"));
    }

    #[test]
    fn load_corrupted_file_starts_empty_not_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join(FILE_NAME), b"not valid json{{{").unwrap();
        let list = Keeplist::load(dir.path());
        assert!(!list.is_marked(Path::new("/anything"), "anything"));
    }
}
