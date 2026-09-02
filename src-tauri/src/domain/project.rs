//! Derive a Project (and optional Package) from a working directory (F2).
//!
//! Pure function: no `#[cfg]`, no filesystem-existence assumption beyond what the
//! caller passes in via `marker_exists` (kept injectable so tests don't need to
//! touch the real filesystem — see the `find_project` tests below, which use a
//! fake).

use std::path::{Path, PathBuf};

use super::model::{Package, Project};

/// Marker files/directories that identify a package root (F2: package.json, go.mod,
/// Cargo.toml, pyproject.toml). `.git` is deliberately excluded here — it marks the
/// repository root, not a package, and is walked for separately in `find_project`.
const PACKAGE_MARKERS: [&str; 4] = ["package.json", "go.mod", "Cargo.toml", "pyproject.toml"];

/// A marker that identifies the repository root itself.
const REPO_ROOT_MARKER: &str = ".git";

/// Find the Project (and, when the working directory sits below the repo root, the
/// Package) for a given `cwd`.
///
/// Two passes, not one, because a single "nearest any-marker directory" walk gives
/// the wrong answer for a monorepo: from `apps/web` (which has its own
/// `package.json`), the nearest marker directory is `apps/web` itself, which would
/// wrongly name the Project "web" instead of the actual repository, "vala-platform".
///
/// So:
/// 1. Project root = nearest ancestor containing `.git` (a repository's one true
///    marker of "this is the root").
/// 2. Package = nearest ancestor at or below `cwd` containing a package marker,
///    expressed relative to the Project root — but only reported when it is
///    strictly below the Project root (same directory means no separate Package).
/// 3. If no `.git` exists anywhere up the chain, fall back to "nearest directory
///    with ANY marker" as the Project root, with no Package — this still covers
///    plain (non-monorepo, non-git, e.g. a bare Cargo/Go project) cases from F2's
///    literal wording.
///
/// `exists` abstracts "does this path exist on disk" so the function makes no direct
/// filesystem calls itself (kept easy to unit test, and consistent with domain/
/// being pure logic over data the caller supplies).
pub fn find_project<F>(cwd: &Path, exists: F) -> Option<(Project, Option<Package>)>
where
    F: Fn(&Path) -> bool,
{
    let repo_root = find_ancestor_with_marker(cwd, REPO_ROOT_MARKER, &exists);

    if let Some(repo_root) = repo_root {
        let package_dir = find_ancestor_with_any_marker(cwd, &PACKAGE_MARKERS, &exists);
        let package = match package_dir {
            Some(dir) if dir != repo_root => dir
                .strip_prefix(&repo_root)
                .ok()
                .map(|rel| Package { relative_path: rel.to_path_buf() }),
            _ => None,
        };
        let name = project_name(&repo_root);
        return Some((Project { root: repo_root, name }, package));
    }

    // No `.git` anywhere up the chain: fall back to nearest directory with any
    // marker at all, per F2's literal wording, with no separate Package.
    let fallback_root = find_ancestor_with_any_marker(cwd, &PACKAGE_MARKERS, &exists)?;
    let name = project_name(&fallback_root);
    Some((Project { root: fallback_root, name }, None))
}

fn project_name(root: &Path) -> String {
    root.file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
        // A root with no file name (e.g. "/") has no sensible project name; this is
        // an edge case that should not occur in practice, since a project marker
        // living directly at the filesystem root is not realistic, but the function
        // must not panic if it does.
        .unwrap_or_default()
}

fn find_ancestor_with_marker<F>(start: &Path, marker: &str, exists: &F) -> Option<PathBuf>
where
    F: Fn(&Path) -> bool,
{
    for ancestor in start.ancestors() {
        if exists(&ancestor.join(marker)) {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

fn find_ancestor_with_any_marker<F>(start: &Path, markers: &[&str], exists: &F) -> Option<PathBuf>
where
    F: Fn(&Path) -> bool,
{
    for ancestor in start.ancestors() {
        if markers.iter().any(|m| exists(&ancestor.join(m))) {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

// Tests live in src-tauri/tests/domain_project.rs, not here: domain/ must contain
// ZERO #[cfg] attributes (a hard constraint, checked in review), and `#[cfg(test)]`
// is itself a #[cfg]. Integration tests in tests/ exercise this module through its
// public API without needing one inside domain/ at all.
