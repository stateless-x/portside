//! E: the Tauri IPC surface (docs/IPC.md, frozen — implemented exactly, not
//! reinterpreted). D: the stop flow (F8/F9/N2) lives here too, since verifying a stop
//! means re-enumerating through the same `ScannerState` these commands already hold.
//!
//! `stop_server`, `force_stop`, and `stop_all_dev_servers` all block for ~3 seconds by
//! design (F8's wait-then-verify sequence) using plain `std::thread::sleep` — this
//! crate has no async runtime of its own (docs/PLAN.md). Tauri v2 dispatches
//! `async fn` commands onto ITS OWN tokio runtime via `crate::async_runtime::spawn`
//! (confirmed by reading tauri 2.11.5's `src/ipc/mod.rs`), and a blocking sleep on a
//! tokio worker thread stalls that runtime's whole worker pool — including every
//! OTHER pending async command — not just the caller. So every blocking body here
//! runs inside `tauri::async_runtime::spawn_blocking`, which Tauri documents as
//! dispatching to a dedicated blocking-task pool rather than a worker thread. This is
//! why `AppState.source` is `Arc`, not `Box`: `spawn_blocking`'s closure must be
//! `'static`, so the command needs an owned, cheaply-cloned handle to move into it
//! rather than borrowing through `State<'_, AppState>`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use tauri::{AppHandle, Emitter, State};

use crate::ipc::{Snapshot, StopOutcome, StopResult};
use crate::platform::ProcessSource;
use crate::scanner::{self, ClassifyDeps, PanelState, ScannedServer, ScannerState, Waker};

/// How long to wait after a polite stop request before checking whether the port was
/// actually released. REQUIREMENTS.md F8: "Wait ~3 seconds." — measured/spec'd, not
/// tuned.
const POLITE_STOP_WAIT: Duration = Duration::from_secs(3);

/// Shared app state registered with Tauri's `.manage()`. `source` is `Arc<dyn
/// ProcessSource>` rather than the concrete `MacosProcessSource` so this module
/// compiles (and could be tested) against a fake source without touching platform/,
/// and specifically `Arc` (not `Box`) so a blocking command can clone a `'static`
/// handle to move into `tauri::async_runtime::spawn_blocking` — see the module doc
/// comment.
pub struct AppState {
    pub scanner: Arc<Mutex<ScannerState>>,
    pub source: Arc<dyn ProcessSource + Send + Sync>,
    pub waker: Arc<Waker>,
}

/// Resolve a Server id to its current `ScannedServer`, looking it up in the live
/// snapshot rather than parsing the id string apart. `Server.id` is `"{pid}:{port}"`
/// only by convention (see `scanner::server_id`) — treating it as data to parse back
/// into a raw pid would let a stale or hand-crafted id from the UI reach `kill`
/// directly. Looking it up here means an id that no longer exists in the current scan
/// simply resolves to `None`, which every stop path below turns into a safe "refused"
/// rather than ever calling into `ProcessSource` with an unverified pid.
fn resolve<'a>(servers: &'a [ScannedServer], id: &str) -> Option<&'a ScannedServer> {
    servers.iter().find(|s| s.id == id)
}

fn not_found_outcome(id: &str) -> StopOutcome {
    StopOutcome { id: id.to_string(), result: StopResult::Refused, message: "This Server is no longer listed — it may have already stopped.".to_string() }
}

fn watch_only_outcome(id: &str) -> StopOutcome {
    StopOutcome { id: id.to_string(), result: StopResult::Refused, message: "This Server is Watch Only and is never stopped through this tool.".to_string() }
}

/// The polite half of F8: re-verify the target is still itself, signal it, wait, then
/// verify by re-enumerating and checking none of the target's ports are still held.
/// This is the ONLY definition of "stopped" the app uses — a signal being accepted is
/// not enough (CONTEXT.md "Stopped": "Not the same as having asked").
///
/// Returns `Err(reason)` when the identity check refuses — the caller turns that into a
/// `Refused` outcome carrying the reason, so a target that changed underneath us is
/// reported honestly rather than being signaled or silently reported as stopped.
fn request_stop_and_verify(
    state: &Arc<Mutex<ScannerState>>,
    source: &dyn ProcessSource,
    deps: &ClassifyDeps,
    target: &ScannedServer,
) -> Result<StopResult, &'static str> {
    // A3: the resolved target came from the last scan's cache, which can be up to 60s
    // stale. Re-establish that the pid is still this Server before signaling anything
    // — a recycled pid would otherwise receive a signal meant for something else.
    let fresh = source.enumerate().map_err(|_| "Couldn't check this Server just now — nothing was stopped.")?;
    if let Some(reason) = scanner::refuse_if_identity_changed(&fresh, target) {
        return Err(reason);
    }

    if let Err(e) = source.request_stop(target.pid) {
        // A signal that could not even be sent (e.g. the pid is already gone) is not
        // itself a failure the user needs to act on — re-verifying below will
        // correctly report "stopped" if the process is in fact gone, or
        // "still_running" if something is wrong in a way that matters.
        eprintln!("request_stop for pid {}: {e}", target.pid);
    }

    std::thread::sleep(POLITE_STOP_WAIT);

    Ok(port_still_held(state, source, deps, target))
}

/// Re-enumerate and check whether any of `target`'s ports are still held by anything
/// — not specifically by `target.pid`, since CONTEXT.md is explicit that a surviving
/// child can keep the address held after its parent exits. Also refreshes
/// `state.servers` with the fresh enumeration so the caller's next snapshot reflects
/// reality immediately, rather than waiting for the scan loop's next tick.
fn port_still_held(state: &Arc<Mutex<ScannerState>>, source: &dyn ProcessSource, deps: &ClassifyDeps, target: &ScannedServer) -> StopResult {
    let Ok(listeners) = source.enumerate() else {
        // Cannot verify: treat as still running rather than optimistically reporting
        // success on a check that did not actually run (N3: never present a guess as
        // fact).
        return StopResult::StillRunning;
    };

    let still_held = listeners.iter().any(|l| l.ports.iter().any(|p| target.ports.iter().any(|tp| tp.port == p.port && tp.family == p.family)));

    let mut guard = match state.lock() {
        Ok(g) => g,
        Err(_) => return StopResult::StillRunning,
    };
    guard.servers = scanner::classify_and_probe(&listeners, deps, &mut guard.title_cache);
    guard.last_fingerprint = Some(scanner::fingerprint(&listeners));

    if still_held {
        StopResult::StillRunning
    } else {
        StopResult::Stopped
    }
}

fn stop_message(server: &ScannedServer, result: StopResult) -> String {
    match result {
        StopResult::Stopped => scanner::what_this_stops(server),
        StopResult::StillRunning => format!("{} It did not stop — you can Force Stop it.", scanner::what_this_stops(server)),
        StopResult::Refused => scanner::what_this_stops(server),
    }
}

// ---------------------------------------------------------------------------------
// Tauri commands — docs/IPC.md, implemented exactly.
// ---------------------------------------------------------------------------------

#[tauri::command]
pub fn panel_opened(state: State<AppState>) {
    if let Ok(mut guard) = state.scanner.lock() {
        guard.panel = PanelState::Open;
    }
    state.waker.wake();
}

#[tauri::command]
pub fn panel_closed(state: State<AppState>) {
    if let Ok(mut guard) = state.scanner.lock() {
        guard.panel = PanelState::Closed;
        guard.panel_closed_at = Some(SystemTime::now());
    }
    state.waker.wake();
}

#[tauri::command]
pub async fn refresh_now(state: State<'_, AppState>) -> Result<Snapshot, String> {
    // Shells out (lsof/ps via ProcessSource::enumerate) — blocking I/O, so it runs on
    // Tauri's blocking pool rather than a tokio worker thread, same reasoning as the
    // stop commands below (see module doc comment).
    let scanner_state = state.scanner.clone();
    let source = state.source.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let deps = macos_deps();
        let mut guard = scanner_state.lock().map_err(|_| "scanner state poisoned".to_string())?;
        scanner::scan_once(&mut guard, source.as_ref(), &deps, true)?;
        Ok(guard.snapshot(SystemTime::now()))
    })
    .await
    .map_err(|e| format!("refresh_now task panicked: {e}"))?
}

#[tauri::command]
pub fn set_keep_running(state: State<AppState>, id: String, keep: bool) -> Result<(), String> {
    let mut guard = state.scanner.lock().map_err(|_| "scanner state poisoned".to_string())?;
    let Some(server) = resolve(&guard.servers, &id) else {
        // Setting a mark on a Server that no longer exists is a no-op, not an error —
        // the UI may have a stale row it hasn't repainted yet.
        return Ok(());
    };
    let Some(root) = project_root(server) else {
        // F10's key is (project path, command). A Server with no Project has nothing
        // to key a mark by, so there is nothing to persist — this should only be
        // reachable if the UI offers Keep Running on a non-DevServer row, which it
        // must not (DevServers always have a Project per F3/classify.rs rule 4).
        return Ok(());
    };
    let (root, command) = (root.to_path_buf(), server.command.clone());
    guard.keeplist.set(&root, &command, keep);
    let app_data_dir = guard.app_data_dir.clone();
    guard.keeplist.save(&app_data_dir)
}

fn project_root(server: &ScannedServer) -> Option<&std::path::Path> {
    use crate::domain::model::ProjectAttribution;
    match &server.attribution {
        ProjectAttribution::Known(project, _) | ProjectAttribution::Guessed(project, _) => Some(project.root.as_path()),
        ProjectAttribution::None => None,
    }
}

/// docs/IPC.md amendment v1.1: open the Project this Server was started from, either
/// in the user's editor or in Finder. Returns whether a launch actually succeeded.
///
/// The id is resolved through the live snapshot for the same reason every stop path
/// does it (see `resolve`): the UI never passes a path, so nothing the frontend sends
/// can point this at an arbitrary directory. A Server whose id no longer resolves, or
/// which is not a DevServer, or which has no Project root, returns `false` rather than
/// launching anything.
///
/// Restricted to `Kind::DevServer` deliberately. `OtherServer` rows carry a *Guessed*
/// Project (CONTEXT.md: "shown as uncertain, never as fact, and never used to decide
/// something is safe to stop") — opening a folder on a guess would present that guess
/// as fact, so this command refuses it at the boundary rather than trusting the UI not
/// to offer it. Watch Only rows are excluded by the same Kind check.
/// docs/IPC.md amendment v1.3: which editor "editor" means.
///
/// A CLOSED SET, and the security boundary for this command. The value arriving from
/// the UI selects one of the hardcoded chains below — it is never itself executed,
/// interpolated into a command, or used as a program name. That is why this is a
/// match on an enum of ids rather than, say, an editor name or path from settings:
/// there is no input here that could become an executable.
///
/// An unrecognized value is NOT an error and NOT vscode — it degrades to Finder,
/// the one action that is always available and never runs a user-named program.
/// `None` (an older UI that predates this amendment, or `how: "finder"`) keeps the
/// v1.1 behaviour and uses the Visual Studio Code chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Editor {
    VsCode,
    Cursor,
    Zed,
    Sublime,
    /// Explicitly "no editor" — used for an unknown id. Opens the folder in Finder.
    FinderOnly,
}

impl Editor {
    /// Absent = the documented default (Visual Studio Code). Unknown = Finder.
    fn from_wire(value: Option<&str>) -> Self {
        match value {
            None | Some("vscode") => Editor::VsCode,
            Some("cursor") => Editor::Cursor,
            Some("zed") => Editor::Zed,
            Some("sublime") => Editor::Sublime,
            Some(other) => {
                eprintln!("open_project: unknown editor {other:?} — falling back to Finder");
                Editor::FinderOnly
            }
        }
    }

    /// (CLI binary, application name) for the two launch tiers. Both are string
    /// literals: nothing here is derived from the wire value beyond selecting which
    /// pair is returned.
    fn chain(self) -> Option<(&'static str, &'static str)> {
        match self {
            Editor::VsCode => Some(("code", "Visual Studio Code")),
            Editor::Cursor => Some(("cursor", "Cursor")),
            Editor::Zed => Some(("zed", "Zed")),
            Editor::Sublime => Some(("subl", "Sublime Text")),
            Editor::FinderOnly => None,
        }
    }
}

#[tauri::command]
pub async fn open_project(
    state: State<'_, AppState>,
    id: String,
    how: String,
    editor: Option<String>,
) -> Result<bool, String> {
    use crate::domain::model::Kind;

    let root = {
        let guard = state.scanner.lock().map_err(|_| "scanner state poisoned".to_string())?;
        let Some(server) = resolve(&guard.servers, &id) else {
            return Ok(false);
        };
        if server.kind != Kind::DevServer {
            return Ok(false);
        }
        match project_root(server) {
            Some(p) => p.to_path_buf(),
            None => return Ok(false),
        }
    };

    // `open`/`code` hand off to launchd and exit within milliseconds, and `spawn_ok`
    // caps its wait for that exit at LAUNCH_EXIT_CHECK per tier — so the worst case is
    // a fraction of a second, not the multi-second sleeps the stop commands take, and
    // unlike them this still does not need the blocking pool.
    Ok(launch_for_project(&root, &how, Editor::from_wire(editor.as_deref())))
}

/// Try each launcher in turn, reporting which one actually worked. Every failure is
/// logged rather than swallowed — a spawn error here is the difference between "your
/// editor opened" and "nothing happened", and the user gets `false` only when all
/// tiers failed.
///
/// The CLI binary is tried first but is EXPECTED to fail in a bundled app: an app
/// launched from Finder inherits a minimal PATH that usually excludes
/// /usr/local/bin and /opt/homebrew/bin where `code` and friends are installed. That
/// is why the `open -a` tier exists and why a failed CLI is logged at debug volume,
/// not surfaced as an error.
///
/// Every path is passed via `.arg()`, never interpolated into a shell string — no
/// shell ever parses it, so a directory name containing spaces, quotes, or `;` is
/// inert rather than injectable. The same is true of the program name: it comes from
/// `Editor::chain`'s string literals, never from the wire.
fn launch_for_project(root: &std::path::Path, how: &str, editor: Editor) -> bool {
    use std::process::Command;

    if how == "finder" {
        // `open -R` reveals the folder in its parent, which is what "Reveal in Finder"
        // means; if the root has no parent to reveal it in, fall back to opening it.
        if spawn_ok(Command::new("open").arg("-R").arg(root), "open -R") {
            return true;
        }
        return spawn_ok(Command::new("open").arg(root), "open");
    }

    // "editor" (and any unrecognized `how`, which degrades to the safest useful thing
    // rather than erroring — the contract only defines two values).
    if let Some((cli, app)) = editor.chain() {
        if spawn_ok(Command::new(cli).arg(root), cli) {
            return true;
        }
        if spawn_ok(Command::new("open").arg("-a").arg(app).arg(root), app) {
            return true;
        }
    }
    // Last resort, and the whole of the Finder-only path: hand the folder to Finder.
    // Honest degradation — the user asked to go to the source and still gets there,
    // just not in an editor.
    spawn_ok(Command::new("open").arg(root), "open (Finder fallback)")
}

/// How long `spawn_ok` polls a launcher for an early non-zero exit before accepting it
/// as launched. `open` and `code` both hand off and exit within milliseconds, and a
/// FAILING `open -a` (app not installed) exits just as fast — so this window is long
/// enough to catch the failure while keeping `open_project` fast enough that its
/// "needs no blocking pool" reasoning still holds.
const LAUNCH_EXIT_CHECK: Duration = Duration::from_millis(250);
const LAUNCH_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Spawn and report whether it actually launched, logging the failure reason.
///
/// Spawning successfully is NOT launching successfully: `open -a "Visual Studio Code"`
/// with that app missing forks fine and only then exits non-zero, so reporting
/// `spawn()`'s result would claim the editor opened when nothing did — and would also
/// stop the fallback chain in `launch_for_project` from ever reaching a tier that
/// works.
///
/// Polled with `try_wait` rather than a blocking `wait`: a launcher that legitimately
/// does not exit promptly (an editor started in the foreground) must be treated as
/// launched, not hang the command. Still running after the window = launched.
fn spawn_ok(command: &mut std::process::Command, label: &str) -> bool {
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            eprintln!("open_project: {label} unavailable: {e}");
            return false;
        }
    };

    let deadline = std::time::Instant::now() + LAUNCH_EXIT_CHECK;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                eprintln!("open_project: launched via {label}");
                return true;
            }
            Ok(Some(status)) => {
                eprintln!("open_project: {label} exited with {status} — treating as not launched");
                return false;
            }
            // Still running past the window: it launched and stayed up.
            Ok(None) if std::time::Instant::now() >= deadline => {
                eprintln!("open_project: launched via {label}");
                return true;
            }
            Ok(None) => std::thread::sleep(LAUNCH_POLL_INTERVAL),
            Err(e) => {
                eprintln!("open_project: could not check {label}: {e}");
                return false;
            }
        }
    }
}

#[tauri::command]
pub async fn stop_server(state: State<'_, AppState>, id: String) -> Result<StopOutcome, String> {
    // F8's wait-then-verify sleeps for ~3s. Run the whole thing on Tauri's blocking
    // pool (see module doc comment) — an async fn's body still executes on a tokio
    // worker up until its first real .await, and std::thread::sleep has none, so
    // without this the entire body would block that worker for the full 3s.
    let scanner_state = state.scanner.clone();
    let source = state.source.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let deps = macos_deps();

        let target = {
            let guard = scanner_state.lock().map_err(|_| "scanner state poisoned".to_string())?;
            let Some(server) = resolve(&guard.servers, &id) else {
                return Ok(not_found_outcome(&id));
            };
            // `refuse_if_watch_only` is the ONLY Kind gate here, deliberately. A
            // `part_of_app` or `background_service` row IS stoppable by design — F8 and
            // CONTEXT.md "What This Stops" describe exactly that case (the confirmation
            // names the application that will quit, or what the service holds up), and
            // docs/IPC.md scopes only `stop_all_dev_servers` to DevServer. Narrowing
            // `stop_server` to DevServer would delete a designed capability, not fix a
            // hazard. What made stopping an `others[]` row dangerous was the group
            // signal reaching processes outside this Server's tree — fixed at its
            // source in `macos::signal_target`, not by refusing the row.
            if scanner::refuse_if_watch_only(server.kind).is_some() {
                return Ok(watch_only_outcome(&id));
            }
            server.clone_for_stop()
        };
        // Lock dropped before the 3s wait — PLAN.md's N1 cost-control emphasis
        // extends to "don't block the whole app's state behind a sleep".

        let result = match request_stop_and_verify(&scanner_state, source.as_ref(), &deps, &target) {
            Ok(result) => result,
            // The identity check refused: nothing was signaled. Report that plainly
            // rather than as a success or as a stop that failed (N3) — and do NOT
            // record a polite-stop failure, since no polite stop ever happened, so
            // this must not become authorization to Force Stop.
            Err(reason) => {
                return Ok(StopOutcome { id: id.clone(), result: StopResult::Refused, message: reason.to_string() });
            }
        };
        record_polite_stop_result(&scanner_state, &id, &target, result);
        Ok(StopOutcome { id: id.clone(), result, message: stop_message(&target, result) })
    })
    .await
    .map_err(|e| format!("stop_server task panicked: {e}"))?
}

/// Record whether this polite stop attempt failed — the single fact that makes
/// `force_stop` valid for this id afterward (F8/N2). Stores `target.ports` alongside
/// the id, not just the id itself: CONTEXT.md "Stopping" is explicit that a
/// surviving child can keep the address held after the signaled parent exits, and
/// that child has a DIFFERENT pid, so a later `force_stop` must be able to find it by
/// port even though the original id no longer resolves in `state.servers` — see
/// `scanner::find_current_holder`. `Stopped` clears any prior failure record for the
/// id (the Server is gone; a later id reuse must not inherit stale permission to
/// force-kill something new). `Refused` never reaches here (the caller returns before
/// calling `request_stop_and_verify` in that case).
fn record_polite_stop_result(state: &Arc<Mutex<ScannerState>>, id: &str, target: &ScannedServer, result: StopResult) {
    let Ok(mut guard) = state.lock() else { return };
    match result {
        StopResult::StillRunning => {
            guard
                .failed_polite_stops
                .insert(id.to_string(), scanner::FailedPoliteStop::new(target.ports.clone(), SystemTime::now()));
        }
        StopResult::Stopped | StopResult::Refused => {
            guard.failed_polite_stops.remove(id);
        }
    }
}

#[tauri::command]
pub async fn force_stop(state: State<'_, AppState>, id: String) -> Result<StopOutcome, String> {
    // Same reasoning as stop_server: a ~3s std::thread::sleep must run on the
    // blocking pool, not a tokio worker (see module doc comment).
    let scanner_state = state.scanner.clone();
    let source = state.source.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let deps = macos_deps();

        // F8: "Never escalate to a forced stop automatically." force_stop is only
        // valid after a stop_server call on THIS id already returned still_running —
        // tracked as an explicit fact in ScannerState.failed_polite_stops (checked via
        // `refuse_force_stop_without_prior_failure`), not re-derived from "is the port
        // currently held". Port state alone cannot distinguish "a polite stop was
        // tried and failed" from "this Server is simply healthy and running" — both
        // hold the port — and the latter must never be enough to authorize SIGKILL.
        let target_ports = {
            let mut guard = scanner_state.lock().map_err(|_| "scanner state poisoned".to_string())?;
            if let Some(reason) = scanner::refuse_force_stop_without_prior_failure(&guard.failed_polite_stops, &id, SystemTime::now()) {
                // An expired authorization is dropped here rather than left to linger:
                // it can never authorize anything again, and keeping it would only
                // make the map grow for the app's lifetime.
                guard.failed_polite_stops.remove(&id);
                return Ok(StopOutcome { id: id.clone(), result: StopResult::Refused, message: reason.to_string() });
            }
            // Safe to index: the gate above just confirmed this id is an unexpired key.
            guard.failed_polite_stops.get(&id).map(|r| r.ports.clone()).unwrap_or_default()
        };

        let target = {
            let guard = scanner_state.lock().map_err(|_| "scanner state poisoned".to_string())?;
            // Prefer a direct id match (the common case: the same process still
            // holds the port). Fall back to resolving by the ports recorded at
            // `stop_server` time — CONTEXT.md "Stopping": a surviving child that kept
            // the port after the signaled parent exited has a NEW pid, so its id no
            // longer matches what was recorded, but it is still the honest target of
            // "the Server that did not fully stop".
            let found = resolve(&guard.servers, &id).or_else(|| scanner::find_current_holder(&guard.servers, &target_ports));
            let Some(server) = found else {
                return Ok(not_found_outcome(&id));
            };
            if scanner::refuse_if_watch_only(server.kind).is_some() {
                return Ok(watch_only_outcome(&id));
            }
            server.clone_for_stop()
        };

        // A3, same reasoning as stop_server: `target` came from `state.servers`, which
        // can be up to 60s stale, and SIGKILL to a recycled pid is unrecoverable. The
        // check is against a fresh enumeration of the pid as it is right now.
        //
        // Note this covers BOTH resolution paths above — the direct id match and the
        // `find_current_holder` fallback. The fallback deliberately matches on ports
        // alone (a surviving child has a different pid, which is the whole point), but
        // whichever ScannedServer it returns came from the same possibly-stale
        // snapshot, so it gets verified here just the same before being killed.
        {
            let fresh = source.enumerate().map_err(|_| "could not enumerate to verify force_stop target".to_string())?;
            if let Some(reason) = scanner::refuse_if_identity_changed(&fresh, &target) {
                if let Ok(mut guard) = scanner_state.lock() {
                    guard.failed_polite_stops.remove(&id);
                }
                return Ok(StopOutcome { id: id.clone(), result: StopResult::Refused, message: reason.to_string() });
            }
        }

        if let Err(e) = source.force_stop(target.pid) {
            eprintln!("force_stop for pid {}: {e}", target.pid);
        }
        std::thread::sleep(POLITE_STOP_WAIT);
        let result = port_still_held(&scanner_state, source.as_ref(), &deps, &target);
        // Consume the record regardless of outcome: a second SIGKILL attempt needs a
        // fresh polite-stop failure, not a leftover flag from the first attempt.
        if let Ok(mut guard) = scanner_state.lock() {
            guard.failed_polite_stops.remove(&id);
        }
        Ok(StopOutcome { id: id.clone(), result, message: stop_message(&target, result) })
    })
    .await
    .map_err(|e| format!("force_stop task panicked: {e}"))?
}

#[tauri::command]
pub async fn stop_all_dev_servers(state: State<'_, AppState>) -> Result<Vec<StopOutcome>, String> {
    // Sequential ~3s waits, N of them (see the loop below) — the most blocking of the
    // three stop commands, and exactly why it must not run on a tokio worker thread
    // (see module doc comment).
    let scanner_state = state.scanner.clone();
    let source = state.source.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let deps = macos_deps();

        let targets: Vec<ScannedServer> = {
            let guard = scanner_state.lock().map_err(|_| "scanner state poisoned".to_string())?;
            // F9/docs/IPC.md: "touches ONLY Kind::DevServer". `eligible_for_bulk_stop`
            // is the single enforced definition of that scope — this function does
            // not duplicate the Kind check inline, it calls the same function
            // `stop_all_dev_servers` tests below assert against.
            let eligible_ids = scanner::eligible_for_bulk_stop(&guard.servers);
            eligible_ids.iter().filter_map(|id| resolve(&guard.servers, id)).map(|s| s.clone_for_stop()).collect()
        };

        let mut outcomes = Vec::with_capacity(targets.len());
        for target in &targets {
            // Sequential, not parallel: each iteration signals a process group and
            // then re-enumerates the WHOLE machine to verify — running these
            // concurrently would have each verification racing the others'
            // re-enumeration writes to `state.servers`. F9 does not require these to
            // complete in parallel (unlike F4's liveness checks, which N1 explicitly
            // budgets); correctness over speed here.
            // Each target is identity-checked immediately before it is signaled (inside
            // request_stop_and_verify), not once for the batch — a bulk stop takes ~3s
            // per Server, so by the time the last one is reached the snapshot the batch
            // was built from is many seconds old.
            let result = match request_stop_and_verify(&scanner_state, source.as_ref(), &deps, target) {
                Ok(result) => result,
                Err(reason) => {
                    outcomes.push(StopOutcome { id: target.id.clone(), result: StopResult::Refused, message: reason.to_string() });
                    continue;
                }
            };
            record_polite_stop_result(&scanner_state, &target.id, target, result);
            outcomes.push(StopOutcome { id: target.id.clone(), result, message: stop_message(target, result) });
        }
        Ok(outcomes)
    })
    .await
    .map_err(|e| format!("stop_all_dev_servers task panicked: {e}"))?
}

/// Re-exported from `lib.rs` rather than duplicated here — `crate::classify_deps` is
/// the one place that resolves the platform-specific `owning_app` implementation
/// (`#[cfg(target_os = "macos")]` lives there, next to `make_process_source`), so this
/// module and the scan loop always build a `ClassifyDeps` the same way.
use crate::classify_deps as macos_deps;

// ---------------------------------------------------------------------------------
// Tray (F): DevServer count, plus a not_responding indicator. Never notifies, never
// interrupts (F7) — this only updates the tray's own title/icon, no OS notification
// API is ever called anywhere in this codebase.
// ---------------------------------------------------------------------------------

/// Render the tray title from a snapshot. A plain function so it is testable without
/// a real `TrayIcon`. `!` after the count is the entire "highest-value signal" (F4) —
/// deliberately not a popup, badge color change, or anything that could read as an
/// interruption; F7 forbids exactly that.
pub fn tray_title(snapshot: &Snapshot) -> String {
    let count: usize = snapshot.projects.iter().map(|g| g.servers.len()).sum();
    let any_not_responding = snapshot.projects.iter().flat_map(|g| &g.servers).any(|s| s.health == crate::ipc::HealthWire::NotResponding);
    if any_not_responding {
        format!("{count}!")
    } else {
        count.to_string()
    }
}

pub fn emit_snapshot(app: &AppHandle, snapshot: &Snapshot) {
    let _ = app.emit("servers:changed", snapshot);
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_title(Some(tray_title(snapshot)));
    }
}

/// docs/IPC.md v1.4 `resources:changed`. Deliberately does NOT touch the tray title:
/// the tray reports how many dev servers are running and whether any stopped
/// answering (F7), and resource usage is not a claim about either. F7 also says the
/// indicator never claims a Server should be stopped — surfacing CPU there would come
/// very close to exactly that.
pub fn emit_resources(app: &AppHandle, samples: &crate::ipc::ResourceSamples) {
    let _ = app.emit("resources:changed", samples);
}

impl ScannedServer {
    /// A cheap, explicit clone used only to release the scanner mutex before a
    /// multi-second stop wait. Named distinctly from `Clone::clone` (even though it
    /// is currently identical) so a future field addition that is expensive or
    /// unsafe to hold across the lock-release boundary gets a deliberate decision
    /// here rather than silently inheriting whatever `#[derive(Clone)]` does.
    fn clone_for_stop(&self) -> ScannedServer {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::{Kind, Project, ProjectAttribution};
    use crate::ipc::HealthWire;
    use crate::platform::{AddressFamily, PortBinding, Reachability};

    fn dev_server_wire(id: &str, health: HealthWire) -> crate::ipc::ServerWire {
        crate::ipc::ServerWire {
            id: id.to_string(),
            pid: 1,
            package: None,
            project_path: None,
            title: None,
            command: "npm run dev".into(),
            ports: vec![],
            uptime_seconds: 10,
            health,
            unattended: true,
            keep_running: false,
            usage: crate::ipc::ResourceUsageWire::new(&Default::default(), crate::scanner::Pressure::Normal),
        }
    }

    #[test]
    fn tray_title_shows_plain_count_when_all_responding() {
        let snapshot = Snapshot {
            projects: vec![crate::ipc::ProjectGroup {
                project: "a".into(),
                servers: vec![dev_server_wire("1:100", HealthWire::Responding), dev_server_wire("2:200", HealthWire::Responding)],
            }],
            watch_only: vec![],
            others: vec![],
            scanned_at: "2026-01-01T00:00:00Z".into(),
            scan_failed: false,
        };
        assert_eq!(tray_title(&snapshot), "2");
    }

    #[test]
    fn tray_title_marks_not_responding() {
        let snapshot = Snapshot {
            projects: vec![crate::ipc::ProjectGroup {
                project: "a".into(),
                servers: vec![dev_server_wire("1:100", HealthWire::Responding), dev_server_wire("2:200", HealthWire::NotResponding)],
            }],
            watch_only: vec![],
            others: vec![],
            scanned_at: "2026-01-01T00:00:00Z".into(),
            scan_failed: false,
        };
        assert_eq!(tray_title(&snapshot), "2!");
    }

    #[test]
    fn tray_title_zero_when_nothing_running() {
        let snapshot = Snapshot { projects: vec![], watch_only: vec![], others: vec![], scanned_at: "2026-01-01T00:00:00Z".into(), scan_failed: false };
        assert_eq!(tray_title(&snapshot), "0");
    }

    fn binding(port: u16) -> PortBinding {
        PortBinding { port, family: AddressFamily::V4, reachability: Reachability::LocalhostOnly }
    }

    fn scanned(id: &str, kind: Kind) -> ScannedServer {
        ScannedServer {
            id: id.to_string(),
            pid: 999,
            command: "npm run dev".into(),
            ports: vec![binding(3000)],
            start_time: SystemTime::now(),
            unattended: true,
            kind,
            attribution: ProjectAttribution::Known(Project { root: "/proj".into(), name: "proj".into() }, None),
            belongs_to: None,
            health: crate::scanner::Health::Responding,
            title: None,
            usage: Default::default(),
            pressure: crate::scanner::Pressure::Normal,
        }
    }

    #[test]
    fn resolve_finds_matching_id() {
        let servers = vec![scanned("1:3000", Kind::DevServer)];
        assert!(resolve(&servers, "1:3000").is_some());
    }

    #[test]
    fn resolve_none_for_unknown_id() {
        let servers = vec![scanned("1:3000", Kind::DevServer)];
        assert!(resolve(&servers, "nonexistent").is_none());
    }

    struct FakeSource {
        listeners: Vec<crate::platform::RawListener>,
        force_stop_called: std::sync::atomic::AtomicBool,
    }

    impl ProcessSource for FakeSource {
        fn enumerate(&self) -> Result<Vec<crate::platform::RawListener>, String> {
            Ok(self.listeners.clone())
        }
        fn owning_app(&self, _exe: &std::path::Path) -> Option<String> {
            None
        }
        fn request_stop(&self, _pid: u32) -> Result<(), String> {
            Ok(())
        }
        fn force_stop(&self, _pid: u32) -> Result<(), String> {
            self.force_stop_called.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    fn empty_scanner_state() -> Arc<Mutex<ScannerState>> {
        let dir = tempfile::tempdir().expect("tempdir");
        Arc::new(Mutex::new(ScannerState::new(dir.path().to_path_buf())))
    }

    /// `record_polite_stop_result` is the ONLY writer of `failed_polite_stops`, which
    /// is what `scanner::refuse_force_stop_without_prior_failure` (tested directly in
    /// scanner.rs, including the required "refused without a prior failure" and
    /// "permitted after a recorded StillRunning" cases) reads to gate `force_stop`.
    /// This test proves the writer side: a `StillRunning` result records the id AND
    /// its ports — the ports are what `find_current_holder` later needs to locate a
    /// surviving child that kept the port under a different pid (see the test below).
    #[test]
    fn record_polite_stop_result_still_running_stores_id_and_ports() {
        let state = empty_scanner_state();
        let target = scanned("999:3000", Kind::DevServer);

        record_polite_stop_result(&state, "999:3000", &target, StopResult::StillRunning);

        let guard = state.lock().unwrap();
        let record = guard.failed_polite_stops.get("999:3000").expect("a StillRunning result must record the id");
        assert_eq!(record.ports, target.ports);
        // The timestamp is what `refuse_force_stop_without_prior_failure` expires
        // against — a record written now must not already be expired.
        assert!(!record.is_expired(SystemTime::now()));
    }

    /// A `Stopped` result must clear any prior failure record — a Server that
    /// eventually stopped politely (e.g. a second stop_server call succeeded) must
    /// not leave a stale force_stop authorization behind for a future, unrelated
    /// process that reuses the same id.
    #[test]
    fn stopped_result_clears_the_failed_polite_stop_record() {
        let state = empty_scanner_state();
        let target = scanned("999:3000", Kind::DevServer);
        record_polite_stop_result(&state, "999:3000", &target, StopResult::StillRunning);
        record_polite_stop_result(&state, "999:3000", &target, StopResult::Stopped);

        assert!(!state.lock().unwrap().failed_polite_stops.contains_key("999:3000"));
    }

    /// A Server that is genuinely healthy and running (port held, but never asked to
    /// stop) must not itself be force_stop-able — `port_still_held` reporting
    /// `StillRunning` is necessary for `stop_server` to eventually authorize
    /// force_stop, but is not sufficient on its own without going through
    /// `record_polite_stop_result`. This exercises `port_still_held` directly to show
    /// the two are genuinely independent: the port check alone proves nothing about
    /// authorization (the real gate, `refuse_force_stop_without_prior_failure`, is
    /// tested directly in scanner.rs against exactly this scenario).
    #[test]
    fn port_still_held_alone_does_not_populate_failed_polite_stops() {
        let target = scanned("999:3000", Kind::DevServer);
        let still_listening = crate::platform::RawListener {
            pid: 12345,
            ppid: 1,
            command: "child".into(),
            exe_path: "/usr/local/bin/node".into(),
            cwd: None,
            ports: vec![binding(3000)],
            start_time: SystemTime::now(),
            user: "dev".into(),
            usage: Default::default(),
        };
        let source = FakeSource { listeners: vec![still_listening], force_stop_called: std::sync::atomic::AtomicBool::new(false) };
        let state = empty_scanner_state();
        let deps = ClassifyDeps { owning_app: &|_| None, path_exists: &|_| false };

        // The port genuinely is held...
        let result = port_still_held(&state, &source, &deps, &target);
        assert_eq!(result, StopResult::StillRunning);
        // ...but merely checking that does not itself record anything — only
        // `record_polite_stop_result` (called from `stop_server`, never from a bare
        // port check) does, which is what force_stop's real gate consults.
        assert!(!state.lock().unwrap().failed_polite_stops.contains_key("999:3000"), "checking port state must not, by itself, authorize force_stop");
    }

    /// Integration proof for the surviving-child case CONTEXT.md "Stopping" describes:
    /// after `stop_server` records a failure for the ORIGINAL id, and the process
    /// tree changes such that a different pid now holds the same port (the signaled
    /// parent exited, a child kept the port), `force_stop` must still be able to find
    /// a target — via `scanner::find_current_holder` on the recorded ports — even
    /// though `resolve` on the original id would fail.
    #[test]
    fn find_current_holder_recovers_the_target_after_the_original_id_stops_resolving() {
        let original_target = scanned("100:3000", Kind::DevServer);
        let state = empty_scanner_state();
        record_polite_stop_result(&state, "100:3000", &original_target, StopResult::StillRunning);

        // Simulate the current snapshot: the ORIGINAL id (100:3000) is gone (that pid
        // exited), but a surviving child now holds port 3000 under a new id.
        let mut survivor = scanned("101:3000", Kind::DevServer);
        survivor.pid = 101;
        {
            let mut guard = state.lock().unwrap();
            guard.servers = vec![survivor];
        }

        let guard = state.lock().unwrap();
        assert!(resolve(&guard.servers, "100:3000").is_none(), "the original id must no longer resolve, by construction");

        let recorded = guard.failed_polite_stops.get("100:3000").expect("must still be recorded");
        let found = scanner::find_current_holder(&guard.servers, &recorded.ports).expect("force_stop must be able to find the surviving child by port");
        assert_eq!(found.id, "101:3000");
    }

    // ---------- docs/IPC.md v1.3: the editor allowlist ----------

    /// Every documented id maps to its own chain. These pairs are the entire set of
    /// programs this command can ever launch.
    #[test]
    fn each_editor_id_maps_to_its_own_command_chain() {
        assert_eq!(Editor::from_wire(Some("vscode")).chain(), Some(("code", "Visual Studio Code")));
        assert_eq!(Editor::from_wire(Some("cursor")).chain(), Some(("cursor", "Cursor")));
        assert_eq!(Editor::from_wire(Some("zed")).chain(), Some(("zed", "Zed")));
        assert_eq!(Editor::from_wire(Some("sublime")).chain(), Some(("subl", "Sublime Text")));
    }

    /// An absent value is the v1.1 behaviour, not a failure: a UI that predates the
    /// amendment (or a `how: "finder"` call, which carries no editor) still gets the
    /// documented default rather than degrading.
    #[test]
    fn absent_editor_uses_the_vscode_chain() {
        assert_eq!(Editor::from_wire(None), Editor::VsCode);
    }

    /// The security property, stated as a test: a value outside the closed set never
    /// becomes a program. It is not passed through, not treated as a CLI name, and
    /// not defaulted to an editor — it resolves to FinderOnly, whose chain is None,
    /// so `launch_for_project` runs only its hardcoded `open` fallback.
    #[test]
    fn unknown_editor_degrades_to_finder_and_never_becomes_a_command() {
        for hostile in ["rm", "; rm -rf /", "/bin/sh", "Code", "", "vscode "] {
            let resolved = Editor::from_wire(Some(hostile));
            assert_eq!(resolved, Editor::FinderOnly, "{hostile:?} must not resolve to an editor");
            assert_eq!(resolved.chain(), None, "{hostile:?} must contribute no command to run");
        }
    }

    /// Unknown must be distinguishable from absent — they are different inputs with
    /// deliberately different outcomes, which a single "fall back to default" branch
    /// would have collapsed.
    #[test]
    fn unknown_editor_is_not_treated_as_absent() {
        assert_ne!(Editor::from_wire(Some("notanedito")), Editor::from_wire(None));
    }

    #[test]
    fn stop_message_never_contains_raw_port_number() {
        let target = scanned("999:3000", Kind::DevServer);
        let msg = stop_message(&target, StopResult::Stopped);
        assert!(!msg.contains("3000"));
    }
}
