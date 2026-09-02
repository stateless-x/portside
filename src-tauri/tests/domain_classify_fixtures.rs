//! End-to-end domain tests (deliverable E) over `RawListener`s built from real data
//! captured on this machine (see src-tauri/tests/fixtures/*_raw.txt — captured with
//! `lsof`/`ps`, not invented). Each test exercises `domain::classify::classify_listener`,
//! the actual entry point `RawListener -> (Kind, ProjectAttribution)`, not individual
//! helper functions, because the required cases are outcomes ("must be
//! BackgroundService with a Guessed Project"), not internals.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use portwatch_lib::domain::classify::classify_listener;
use portwatch_lib::domain::model::{Kind, ProjectAttribution};
use portwatch_lib::platform::{AddressFamily, PortBinding, Reachability, RawListener};

/// A fake filesystem for `find_project`'s marker checks, built from the real
/// directory listings captured on this machine (see the comment at each call site
/// for which real `ls` output backs it).
fn fake_fs(existing: &[&str]) -> impl Fn(&Path) -> bool {
    let set: HashSet<PathBuf> = existing.iter().map(PathBuf::from).collect();
    move |p: &Path| set.contains(p)
}

fn listener(pid: u32, exe_path: &str, cwd: Option<&str>, ports: Vec<PortBinding>) -> RawListener {
    RawListener {
        pid,
        ppid: 1,
        command: "test".to_string(),
        exe_path: PathBuf::from(exe_path),
        cwd: cwd.map(PathBuf::from),
        ports,
        start_time: SystemTime::now(),
        user: "purin".to_string(),
    }
}

fn localhost_v4(port: u16) -> PortBinding {
    PortBinding { port, family: AddressFamily::V4, reachability: Reachability::LocalhostOnly }
}

fn localhost_v6(port: u16) -> PortBinding {
    PortBinding { port, family: AddressFamily::V6, reachability: Reachability::LocalhostOnly }
}

fn all_interfaces_v4(port: u16) -> PortBinding {
    PortBinding { port, family: AddressFamily::V4, reachability: Reachability::AllInterfaces }
}

fn all_interfaces_v6(port: u16) -> PortBinding {
    PortBinding { port, family: AddressFamily::V6, reachability: Reachability::AllInterfaces }
}

/// Mirrors `platform::macos::MacosProcessSource::owning_app`'s outermost-bundle walk
/// (walk every ancestor, remember the LAST ".app" seen — i.e. the outermost one).
/// Reimplemented here rather than imported because `platform::macos` is
/// `#[cfg(target_os = "macos")]`-gated and these are domain/ tests: they should not
/// need to run on macOS specifically to prove `classify_listener` wires an
/// `owning_app` resolver in correctly. Any implementation satisfying the same
/// contract (outermost bundle wins) is equivalent for this file's purposes.
fn owning_app(exe: &Path) -> Option<String> {
    let mut outermost: Option<String> = None;
    for ancestor in exe.ancestors() {
        if let Some(name) = ancestor.file_name().and_then(|n| n.to_str()) {
            if let Some(app_name) = name.strip_suffix(".app") {
                outermost = Some(app_name.to_string());
            }
        }
    }
    outermost
}

/// Case: two different projects both listening on port 4399, one on [::1] and one
/// on 127.0.0.1. Both must survive as separate Servers, both classified DevServer.
///
/// Real data (tests/fixtures/lsof_listen_fields_raw.txt / cwd_snapshot_raw.txt):
/// pid 68829, cwd /Users/purin/dev/purin-dev-site, [::1]:4399
/// pid 98027, cwd /Users/purin/dev/vala-platform/apps/web, 127.0.0.1:4399
#[test]
fn same_port_different_address_families_are_two_distinct_dev_servers() {
    let purin_dev_site_fs = fake_fs(&[
        "/Users/purin/dev/purin-dev-site/.git",
        "/Users/purin/dev/purin-dev-site/package.json",
    ]);
    let raw_a = listener(
        68829,
        "/Users/purin/.local/share/fnm/node-versions/v22.23.1/installation/bin/node",
        Some("/Users/purin/dev/purin-dev-site"),
        vec![localhost_v6(4399)],
    );
    let (kind_a, attribution_a) = classify_listener(&raw_a, owning_app, purin_dev_site_fs);
    assert_eq!(kind_a, Kind::DevServer);
    match attribution_a {
        ProjectAttribution::Known(project, _) => assert_eq!(project.name, "purin-dev-site"),
        other => panic!("expected Known(purin-dev-site), got {other:?}"),
    }

    let vala_fs = fake_fs(&[
        "/Users/purin/dev/vala-platform/.git",
        "/Users/purin/dev/vala-platform/apps/web/package.json",
    ]);
    let raw_b = listener(
        98027,
        "/Users/purin/.local/share/fnm/node-versions/v22.23.1/installation/bin/node",
        Some("/Users/purin/dev/vala-platform/apps/web"),
        vec![localhost_v4(4399)],
    );
    let (kind_b, attribution_b) = classify_listener(&raw_b, owning_app, vala_fs);
    assert_eq!(kind_b, Kind::DevServer);
    match attribution_b {
        ProjectAttribution::Known(project, package) => {
            assert_eq!(project.name, "vala-platform");
            assert_eq!(package.expect("must have a package").relative_path, PathBuf::from("apps/web"));
        }
        other => panic!("expected Known(vala-platform, apps/web), got {other:?}"),
    }

    // Different pids, different projects — the two never collapse into one Server
    // just because the port number matches.
    assert_ne!(raw_a.pid, raw_b.pid);
}

/// Case: one project (purin-dev-site) holding two ports, backed by two real
/// processes on this machine that share the same cwd — pid 68829 (port 4399) and
/// pid 78944 (port 4321). Grouping these into one row is a display concern (F6,
/// phase 4); what this deliverable proves is that both resolve to the SAME Project,
/// which is what makes that later grouping possible.
#[test]
fn one_project_two_servers_resolve_to_the_same_project() {
    let exists = fake_fs(&[
        "/Users/purin/dev/purin-dev-site/.git",
        "/Users/purin/dev/purin-dev-site/package.json",
    ]);

    let raw_4399 = listener(
        68829,
        "/Users/purin/.local/share/fnm/node-versions/v22.23.1/installation/bin/node",
        Some("/Users/purin/dev/purin-dev-site"),
        vec![localhost_v6(4399)],
    );
    let raw_4321 = listener(
        78944,
        "/Users/purin/.local/share/fnm/node-versions/v22.23.1/installation/bin/node",
        Some("/Users/purin/dev/purin-dev-site"),
        vec![localhost_v6(4321)],
    );

    let (kind_a, attribution_a) = classify_listener(&raw_4399, owning_app, &exists);
    let (kind_b, attribution_b) = classify_listener(&raw_4321, owning_app, &exists);

    assert_eq!(kind_a, Kind::DevServer);
    assert_eq!(kind_b, Kind::DevServer);

    let project_a = match attribution_a {
        ProjectAttribution::Known(project, _) => project,
        other => panic!("expected Known, got {other:?}"),
    };
    let project_b = match attribution_b {
        ProjectAttribution::Known(project, _) => project,
        other => panic!("expected Known, got {other:?}"),
    };
    assert_eq!(project_a, project_b);
    assert_eq!(project_a.name, "purin-dev-site");
}

/// Case: OrbStack holds Postgres ports 5432/5433 (real data: pid 2979, 7
/// PortBindings across v4/v6, cwd reported as ~/dev/pawjai/pawjai-fe — see
/// tests/fixtures/lsof_listen_fields_raw.txt and cwd_snapshot_raw.txt) while living
/// inside the OrbStack.app bundle. It must classify as BackgroundService with a
/// Guessed Project, never DevServer, however plausible the cwd looks.
#[test]
fn background_service_with_unrelated_cwd_is_never_a_dev_server() {
    let exists = fake_fs(&[
        "/Users/purin/dev/pawjai/pawjai-fe/.git",
        "/Users/purin/dev/pawjai/pawjai-fe/package.json",
    ]);
    let raw = listener(
        2979,
        "/Applications/OrbStack.app/Contents/Frameworks/OrbStack Helper.app/Contents/MacOS/OrbStack Helper",
        Some("/Users/purin/dev/pawjai/pawjai-fe"),
        vec![
            localhost_v4(32222),
            localhost_v6(32222),
            localhost_v4(60252),
            all_interfaces_v4(5432),
            all_interfaces_v4(5433),
            all_interfaces_v6(5432),
            all_interfaces_v6(5433),
        ],
    );

    let (kind, attribution) = classify_listener(&raw, owning_app, exists);

    assert_eq!(kind, Kind::BackgroundService);
    assert!(!kind.is_watch_only(), "BackgroundService must remain stoppable, not Watch Only");
    match attribution {
        ProjectAttribution::Guessed(project, _) => assert_eq!(project.name, "pawjai-fe"),
        other => panic!("expected Guessed(pawjai-fe), got {other:?}; a BackgroundService's Project must never be Known"),
    }
}

/// Case: the openclaw gateway, port 18789, cwd ~/.openclaw, which has NO project
/// markers at all. Real data: pid 54082, /opt/homebrew/bin/node — not inside any
/// .app bundle, not under a system path. Must be YourOwnTool and Watch Only.
/// "openclaw" itself is never a literal in the classification logic; this falls out
/// purely of the no-project rule plus not being a system/bundle path.
#[test]
fn no_project_markers_and_no_bundle_is_your_own_tool_and_watch_only() {
    let exists = fake_fs(&[]); // ~/.openclaw has no .git/package.json/etc, verified for real above
    let raw = listener(
        54082,
        "/opt/homebrew/bin/node",
        Some("/Users/purin/.openclaw"),
        vec![localhost_v4(18789), localhost_v6(18789)],
    );

    let (kind, attribution) = classify_listener(&raw, owning_app, exists);

    assert_eq!(kind, Kind::YourOwnTool);
    assert!(kind.is_watch_only());
    assert_eq!(attribution, ProjectAttribution::None);
}

/// Case: cwd of None (Windows-shaped input, or any pid whose cwd lsof could not
/// report) must not panic and must not become a DevServer — a Server with no
/// Project is never a DevServer (F3).
#[test]
fn cwd_none_does_not_panic_and_is_never_a_dev_server() {
    let raw = listener(99999, "/opt/homebrew/bin/node", None, vec![localhost_v4(3000)]);

    let (kind, attribution) = classify_listener(&raw, owning_app, |_p: &Path| true);

    assert_eq!(attribution, ProjectAttribution::None);
    assert_ne!(kind, Kind::DevServer);
    assert_eq!(kind, Kind::YourOwnTool);
}

/// A system daemon (rapportd, real data: pid 627, /usr/libexec/rapportd, binds
/// `*:49152` and more) must be PartOfMacOS and Watch Only, and must NOT be
/// misclassified as BackgroundService just because it also binds all-interfaces —
/// the system-path rule has to run first.
#[test]
fn system_daemon_binding_all_interfaces_is_part_of_macos_not_background_service() {
    let raw = listener(
        627,
        "/usr/libexec/rapportd",
        Some("/"),
        vec![all_interfaces_v4(49152), all_interfaces_v6(63015), all_interfaces_v6(63016)],
    );

    let (kind, attribution) = classify_listener(&raw, owning_app, |_p: &Path| false);

    assert_eq!(kind, Kind::PartOfMacOS);
    assert!(kind.is_watch_only());
    assert_eq!(attribution, ProjectAttribution::None);
}

/// A VS Code helper process must resolve to the outer "Visual Studio Code" bundle
/// (Belongs To) and classify PartOfApp — it's localhost-only, so it must not trip
/// the BackgroundService rule despite living in a bundle. Real data: pid 10515.
#[test]
fn app_helper_process_is_part_of_app_not_background_service() {
    let raw = listener(
        10515,
        "/Applications/Visual Studio Code.app/Contents/Frameworks/Code Helper (Plugin).app/Contents/MacOS/Code Helper (Plugin)",
        Some("/"),
        vec![localhost_v4(59142), localhost_v4(55151)],
    );

    let (kind, attribution) = classify_listener(&raw, owning_app, |_p: &Path| false);

    assert_eq!(kind, Kind::PartOfApp);
    assert!(!kind.is_watch_only());
    assert_eq!(attribution, ProjectAttribution::None);
}

/// The dangerous case flagged in review: a VS Code EXTENSION HOST (pid 10820, real
/// data) whose cwd happens to sit inside the Pylance extension's own directory,
/// which has a `package.json` but no `.git` anywhere above it. `find_project`'s
/// fallback branch (no `.git` found -> nearest any-marker dir) resolves that to a
/// "Project" named after the extension folder, with attribution Known.
///
/// Without `belongs_to` wired in, `classify_listener` would see: not a system path,
/// has a (bogus) Project, no bundle info -> DevServer. That would present a VS Code
/// internal process as the user's own dev server, stoppable and included in Stop
/// Everything — a straight violation of F3 ("a Server with no Project is never a
/// DevServer" is the wrong rule to lean on here; the real problem is that this
/// Server has no BUSINESS having a Project attributed to it at all, being a bundled
/// helper) and N2 (no bulk action should be able to reach into an editor's internals).
///
/// The fix: `classify_listener` now calls `owning_app` itself (via the caller-
/// supplied resolver) and applies the bundle rules BEFORE the Project-derived rules
/// ever get a say, exactly as `classify`'s rule ordering (bundle checks at rules 2-3,
/// before rule 4's DevServer check) always intended. Once `belongs_to` resolves to
/// "Visual Studio Code", rule 3 (bundle, not all-interfaces) fires first and this
/// becomes PartOfApp, never DevServer.
#[test]
fn extension_host_with_project_marker_cwd_is_part_of_app_not_dev_server() {
    let exists = fake_fs(&[
        "/Users/purin/.vscode/extensions/ms-python.vscode-pylance-2026.3.1/dist/package.json",
    ]);
    let raw = listener(
        10820,
        "/Applications/Visual Studio Code.app/Contents/Frameworks/Code Helper (Plugin).app/Contents/MacOS/Code Helper (Plugin)",
        Some("/Users/purin/.vscode/extensions/ms-python.vscode-pylance-2026.3.1/dist"),
        vec![localhost_v4(50219)],
    );

    let (kind, attribution) = classify_listener(&raw, owning_app, exists);

    assert_eq!(kind, Kind::PartOfApp, "must resolve via the bundle, not the bogus fallback Project");
    assert!(!kind.is_watch_only());
    assert_ne!(kind, Kind::DevServer, "a VS Code extension host must never be presented as the user's dev server");
    // The cwd's fallback marker walk still finds a "Project" here (the extension's
    // own package.json — see the doc comment above) and `attribution` legitimately
    // carries it as `Known`, same as it would for any other PartOfApp Server whose
    // cwd happens to sit inside a directory with a marker. That is not the bug: Kind
    // is what drives F3/F9 (whether this is offered as a DevServer, whether it's
    // included in Stop Everything), and Kind is correctly PartOfApp, asserted above.
    // A future caller must not read `attribution` as license to treat this as a
    // DevServer just because it happens to be `Known` — `kind` is the source of
    // truth for that, which is exactly what this test pins down.
    match attribution {
        ProjectAttribution::Known(project, _) => assert_eq!(project.name, "dist"),
        other => panic!("expected Known(dist) from the fallback marker walk, got {other:?}"),
    }
}
