//! Integration tests for `domain::project::find_project` (F2).
//!
//! Lives in tests/, not inside domain/, because domain/ must contain ZERO #[cfg]
//! attributes (a hard constraint) and `#[cfg(test)]` is itself one.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use portwatch_lib::domain::project::find_project;

/// A fake filesystem: a fixed set of paths that "exist", so tests don't touch the
/// real disk and stay deterministic regardless of what's on the machine running
/// them. Where a case mirrors something actually observed on the development
/// machine, the comment says so; the fixture is still synthetic here for
/// determinism (real-disk versions of these same cases are exercised in
/// domain_classify_fixtures.rs against paths that exist on this machine right now).
fn fake_fs(existing: &[&str]) -> impl Fn(&Path) -> bool {
    let set: HashSet<PathBuf> = existing.iter().map(PathBuf::from).collect();
    move |p: &Path| set.contains(p)
}

#[test]
fn monorepo_project_is_repo_root_package_is_subdir() {
    // Matches this machine's real vala-platform layout: .git at the repo root,
    // package.json one level down inside apps/web.
    let exists = fake_fs(&[
        "/Users/purin/dev/vala-platform/.git",
        "/Users/purin/dev/vala-platform/apps/web/package.json",
    ]);
    let cwd = Path::new("/Users/purin/dev/vala-platform/apps/web");

    let (project, package) = find_project(cwd, exists).expect("must find a project");

    assert_eq!(project.name, "vala-platform");
    assert_eq!(project.root, PathBuf::from("/Users/purin/dev/vala-platform"));
    assert_eq!(
        package.expect("must find a package").relative_path,
        PathBuf::from("apps/web")
    );
}

#[test]
fn project_with_markers_at_same_level_has_no_package() {
    // Matches this machine's real purin-dev-site layout: .git and package.json both
    // directly at the project root.
    let exists = fake_fs(&[
        "/Users/purin/dev/purin-dev-site/.git",
        "/Users/purin/dev/purin-dev-site/package.json",
    ]);
    let cwd = Path::new("/Users/purin/dev/purin-dev-site");

    let (project, package) = find_project(cwd, exists).expect("must find a project");

    assert_eq!(project.name, "purin-dev-site");
    assert!(package.is_none());
}

#[test]
fn no_markers_anywhere_returns_none() {
    // Matches this machine's real ~/.openclaw layout: no project markers at all
    // walking up from it.
    let exists = fake_fs(&[]);
    let cwd = Path::new("/Users/purin/.openclaw");

    assert!(find_project(cwd, exists).is_none());
}

#[test]
fn no_git_falls_back_to_nearest_any_marker() {
    let exists = fake_fs(&["/Users/purin/dev/bare-go-tool/go.mod"]);
    let cwd = Path::new("/Users/purin/dev/bare-go-tool");

    let (project, package) = find_project(cwd, exists).expect("must find a project");

    assert_eq!(project.name, "bare-go-tool");
    assert!(package.is_none());
}

#[test]
fn walks_up_multiple_levels_to_find_git() {
    let exists = fake_fs(&[
        "/Users/purin/dev/vala-platform/.git",
        "/Users/purin/dev/vala-platform/apps/web/package.json",
    ]);
    // Deeper than the package.json level, still resolves to the same project.
    let cwd = Path::new("/Users/purin/dev/vala-platform/apps/web/src/components");

    let (project, package) = find_project(cwd, exists).expect("must find a project");

    assert_eq!(project.name, "vala-platform");
    assert_eq!(
        package.expect("must find a package").relative_path,
        PathBuf::from("apps/web")
    );
}

#[test]
fn git_marker_as_a_file_counts_as_a_project_root() {
    // A submodule's `.git` is a FILE containing "gitdir: ..." rather than a
    // directory — observed for real on this machine at
    // /Users/purin/dev/pawjai/pawjai-fe/.git. `find_project` only asks "does this
    // path exist", not "is this a directory", so it must still count.
    let exists = fake_fs(&[
        "/Users/purin/dev/pawjai/pawjai-fe/.git",
        "/Users/purin/dev/pawjai/pawjai-fe/package.json",
    ]);
    let cwd = Path::new("/Users/purin/dev/pawjai/pawjai-fe");

    let (project, _package) = find_project(cwd, exists).expect("must find a project");

    assert_eq!(project.name, "pawjai-fe");
}

#[test]
fn real_filesystem_predicate_finds_pawjai_fe_via_file_git() {
    // Same case as above, but against the REAL filesystem (Path::exists), which is
    // what production code actually passes. Proves the fallback isn't only correct
    // against a fake. Skips itself if the machine running this suite doesn't have
    // that path (e.g. CI) rather than failing on an environment difference outside
    // this deliverable's control.
    let real_path = Path::new("/Users/purin/dev/pawjai/pawjai-fe");
    if !real_path.exists() {
        eprintln!("skipping: {} not present on this machine", real_path.display());
        return;
    }

    let (project, _package) = find_project(real_path, Path::exists).expect("must find a project");
    assert_eq!(project.name, "pawjai-fe");
}
