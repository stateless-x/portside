//! Assign a Kind to a Server (F3). Pure function: `RawListener` + `ProjectAttribution`
//! in, `Kind` out. No `#[cfg]`, no OS calls.

use std::path::Path;

use crate::platform::{PortBinding, RawListener};

use super::model::{Kind, ProjectAttribution, Reachability};

/// Executable path prefixes that belong to the operating system itself, never to
/// something the user runs on purpose or a project they wrote. These are OS
/// structural paths (where macOS itself lives), not user- or machine-specific — the
/// same set applies to every Mac, unlike a literal username or project path.
///
/// Deliberately excludes `/usr/bin`: it holds daemons but ALSO user-invocable
/// interpreters (`python3`, `ruby`, `php`). A dev server started as
/// `/usr/bin/python3 -m http.server` from inside a project must still be able to
/// reach the DevServer rule below — classifying it PartOfMacOS would make it
/// Watch Only (F3/F9) and permanently unstoppable through this tool, which is wrong
/// for a Server that plainly belongs to a Project.
const SYSTEM_EXE_PREFIXES: [&str; 4] = ["/System", "/usr/libexec", "/usr/sbin", "/sbin"];

/// Assign exactly one Kind to a Server, following CONTEXT.md's "Kind" section.
///
/// Order matters and is load-bearing, not cosmetic:
/// 1. System executable path -> PartOfMacOS. Checked first because some system
///    daemons (rapportd, ControlCenter, sharingd on this machine) also bind `*`,
///    which would otherwise be caught by rule 2.
/// 2. Inside an app bundle AND holding at least one all-interfaces binding ->
///    BackgroundService. CONTEXT.md: a background service "holds ports on behalf of
///    other things" — publishing a port to the whole network rather than just this
///    machine is evidence of that, not proof (a heuristic, not a certainty; see the
///    project's report for why). This is gated on ALSO being inside a bundle so a
///    plain dev server started with `--host` (all-interfaces, no bundle) does not
///    get misclassified — that stays a DevServer if it has a Project.
/// 3. Inside an app bundle -> PartOfApp.
/// 4. Has a Known or Guessed Project -> DevServer. Note: rule 2 already intercepted
///    every bundled+all-interfaces case, so by the time this rule can fire on a
///    Guessed attribution, the process is NOT in a bundle — a bundled background
///    service never reaches here. A Server with no Project (ProjectAttribution::None)
///    is NEVER a DevServer (F3, CONTEXT.md), matched by the fallthrough to rule 5.
/// 5. Otherwise -> YourOwnTool: no Project, not a system path, not in a bundle. A
///    program the user runs on purpose.
///
/// Private: takes a plain `ProjectAttribution` and would happily map a `Guessed`
/// straight to `DevServer` if called directly with one, which is exactly what
/// `ProjectAttribution` exists to prevent (see model.rs). `classify_listener` below
/// is the only caller, and always passes a provisional `Known`/`None` — never a
/// `Guessed` — so that mistake can't happen through this module's public API.
fn classify(exe_path: &Path, ports: &[PortBinding], belongs_to: Option<&str>, attribution: &ProjectAttribution) -> Kind {
    if is_system_path(exe_path) {
        return Kind::PartOfMacOS;
    }

    let in_bundle = belongs_to.is_some();
    let has_all_interfaces_binding = ports.iter().any(|p| p.reachability == Reachability::AllInterfaces);

    if in_bundle && has_all_interfaces_binding {
        return Kind::BackgroundService;
    }
    if in_bundle {
        return Kind::PartOfApp;
    }

    match attribution {
        ProjectAttribution::Known(_, _) | ProjectAttribution::Guessed(_, _) => Kind::DevServer,
        ProjectAttribution::None => Kind::YourOwnTool,
    }
}

fn is_system_path(exe_path: &Path) -> bool {
    let Some(path_str) = exe_path.to_str() else {
        return false;
    };
    SYSTEM_EXE_PREFIXES.iter().any(|prefix| path_str.starts_with(prefix))
}

/// Decide whether a Server's Project attribution should be trusted as fact or shown
/// as a guess (F2, CONTEXT.md "Guessed Project").
///
/// The only case defined by the spec: a Server classified as BackgroundService
/// carries a Guessed Project, because its working directory records where it was
/// started long ago rather than what it is currently serving. Every other Kind's
/// Project (when it has one) is trusted as-is.
///
/// Private for the same reason as `classify`: `classify_listener` is the intended
/// single entry point for this module.
fn attribution_for(kind: Kind, project: Option<(super::model::Project, Option<super::model::Package>)>) -> ProjectAttribution {
    match project {
        None => ProjectAttribution::None,
        Some((proj, pkg)) if kind == Kind::BackgroundService => ProjectAttribution::Guessed(proj, pkg),
        Some((proj, pkg)) => ProjectAttribution::Known(proj, pkg),
    }
}

/// Build the full classification for one `RawListener`: resolve Belongs To, derive
/// Project attribution, then Kind, in the order CONTEXT.md requires (Guessed Project
/// is a consequence of being a BackgroundService, so Kind must be known before the
/// final attribution is decided). This is the single entry point the rest of the app
/// (and this deliverable's tests) should call.
///
/// `owning_app` is injected rather than hardwired to `platform::MacosProcessSource`
/// for the same reason `exists` is: domain/ makes zero OS calls itself (P1), so
/// anything that needs one comes in as a parameter. But unlike a truly optional
/// convenience, THIS parameter is not optional to call — every caller must resolve
/// and pass it, because rules 2-3 in `classify` (the bundle checks) depend on it to
/// intercept an app-bundle helper before the Project-derived rules ever see it. A
/// caller that skips this (e.g. always passes `|_| None`) reintroduces the exact bug
/// this signature exists to prevent: a VS Code extension host whose cwd happens to
/// contain a `package.json` (observed for real on this machine: pid 10820, the
/// Pylance extension's own directory) would fall through to the DevServer rule and
/// get presented as the user's own server, stoppable and included in Stop Everything
/// — see tests/domain_classify_fixtures.rs,
/// `extension_host_with_project_marker_cwd_is_part_of_app_not_dev_server`.
pub fn classify_listener<F, O>(raw: &RawListener, owning_app: O, exists: F) -> (Kind, ProjectAttribution)
where
    F: Fn(&Path) -> bool,
    O: Fn(&Path) -> Option<String>,
{
    let belongs_to = owning_app(&raw.exe_path);
    let project = raw.cwd.as_deref().and_then(|cwd| super::project::find_project(cwd, exists));

    // `classify`'s rules only need to know "does this Server have a Project at all",
    // not whether it will end up Known or Guessed — that distinction is decided
    // afterwards, once Kind (specifically: is it a BackgroundService) is settled.
    let provisional_attribution = match &project {
        Some((proj, pkg)) => ProjectAttribution::Known(proj.clone(), pkg.clone()),
        None => ProjectAttribution::None,
    };
    let kind = classify(&raw.exe_path, &raw.ports, belongs_to.as_deref(), &provisional_attribution);

    // Now that Kind is settled, downgrade to Guessed if this Server turned out to be
    // a BackgroundService (CONTEXT.md: "Carries a Guessed Project").
    let attribution = attribution_for(kind, project);

    (kind, attribution)
}
