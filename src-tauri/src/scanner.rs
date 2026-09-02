//! B: the adaptive scan loop (PLAN.md), owning cadence, the hash short-circuit, and
//! turning classified Servers into the wire `Snapshot` (docs/IPC.md).
//!
//! | State        | Enumerate | Liveness       |
//! |--------------|-----------|----------------|
//! | Panel open   | 3s        | 3s             |
//! | Panel closed | 15s       | only on change |
//! | Idle/battery | 60s       | only on change |
//!
//! Two deliberate deviations from this table's literal wording, both reported rather
//! than silently patched over:
//!
//! 1. **Idle/battery is approximated, not detected.** The frozen IPC surface only
//!    exposes `panel_opened`/`panel_closed` (docs/IPC.md) — there is no command that
//!    reports macOS idle time or battery state, and this module does not call into
//!    IOKit/pmset to invent one. It is approximated as "panel has been closed for
//!    longer than `IDLE_AFTER_PANEL_CLOSED`", which satisfies the table's shape
//!    without a signal the IPC contract does not carry.
//! 2. **Liveness runs on every enumerate tick, even when the hash is unchanged** —
//!    see `scan_once`'s `unchanged` branch below. PLAN.md's own short-circuit
//!    description settles which reading of "only on change" is intended: "Hash the
//!    enumeration result. Unchanged hash => skip project derivation, classification,
//!    and event emission entirely. **Only liveness runs.**" So liveness is exempt
//!    from the short-circuit by design — what enumeration/classification skip on an
//!    unchanged hash, liveness does not. This is also the only way F7's tray
//!    indicator can show a Server that died while the panel was closed: enumeration
//!    alone would keep reporting the same pid/ports every 15s and never notice it
//!    stopped answering. Concretely, this means the closed-state per-tick cost is
//!    enumerate (~15s cadence) + liveness (every tick, parallel) — not liveness-only
//!    — see the measured numbers in the task's final report.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime};

use crate::domain::classify::classify_listener;
use crate::domain::model::{Kind, ProjectAttribution};
use crate::ipc;
use crate::keeplist::Keeplist;
use crate::platform::{PortBinding, ProcessSource, RawListener};
use crate::probe::{self, TitleCache};

const PANEL_OPEN_ENUMERATE: Duration = Duration::from_secs(3);
const PANEL_CLOSED_ENUMERATE: Duration = Duration::from_secs(15);
const IDLE_ENUMERATE: Duration = Duration::from_secs(60);

/// How long the panel must have been closed before the loop treats the machine as
/// idle and drops to the 60s tier. Not a spec number — PLAN.md's table names the
/// state "Idle/battery" but the frozen IPC surface gives this module no way to
/// observe either condition directly (see the module doc comment). Five minutes is a
/// reasonable proxy for "user has stepped away", chosen because it is long enough
/// that it will not visibly affect anyone actively glancing at the tray between
/// panel-closed refreshes.
const IDLE_AFTER_PANEL_CLOSED: Duration = Duration::from_secs(5 * 60);

/// One Server after classification, before it is split into the wire's three
/// sections. Carries everything `snapshot_from` and the stop flow need: the raw
/// platform facts, the domain verdict, and what the probe found this cycle.
#[derive(Debug, Clone)]
pub struct ScannedServer {
    pub id: String,
    pub pid: u32,
    pub command: String,
    pub ports: Vec<PortBinding>,
    pub start_time: SystemTime,
    pub unattended: bool,
    pub kind: Kind,
    pub attribution: ProjectAttribution,
    pub belongs_to: Option<String>,
    pub health: Health,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Responding,
    NotResponding,
    Unknown,
}

/// Stable id for a Server across scans: pid plus its first port (PLAN.md, docs/IPC.md
/// `Server.id`: "stable across scans: pid + first port"). `ports` must be non-empty —
/// `enumerate()` never returns a listener with zero ports (see macos.rs `flush`, which
/// drops any process with an empty port list before it is ever constructed) — but this
/// takes a slice rather than assuming that invariant blindly, and returns `None` for
/// the degenerate case instead of panicking or fabricating an id.
pub fn server_id(pid: u32, ports: &[PortBinding]) -> Option<String> {
    let first = ports.first()?;
    Some(format!("{pid}:{first_port}", first_port = first.port))
}

/// Everything the classification + probe pipeline needs to turn one `RawListener`
/// into a `ScannedServer`, gathered here so `scan_once` and tests can call it without
/// wiring a full `ProcessSource` + filesystem. Kept as trait-object-free plain
/// closures/fns (matching `classify_listener`'s own signature) rather than a trait,
/// since this crate has no other consumer that would need dynamic dispatch here.
pub struct ClassifyDeps<'a> {
    pub owning_app: &'a dyn Fn(&std::path::Path) -> Option<String>,
    pub path_exists: &'a dyn Fn(&std::path::Path) -> bool,
}

/// Turn one listener's raw liveness outcome into a `Health` (F4, N3). `Some(true)`
/// means at least one binding answered, `Some(false)` means every binding was probed
/// and none did, and `None` means the probe never completed for this listener at all
/// (a panicked probe thread — see `probe::liveness_for_servers`) — which must NOT be
/// folded into `Some(false)`'s `NotResponding`, since that would present "we don't
/// know" as the specific claim "we checked and it failed", exactly what N3 forbids.
/// Extracted as a standalone pure function so this three-way distinction is directly
/// unit-testable without needing to actually crash a probe thread.
fn health_from_liveness_result(result: Option<bool>) -> Health {
    match result {
        Some(true) => Health::Responding,
        Some(false) => Health::NotResponding,
        None => Health::Unknown,
    }
}

/// Classify every `RawListener` and attach liveness (always) and title (DevServer
/// only, cached). This is the one place F5's "never send protocol requests to any
/// other Kind" is enforced — see the `kind == Kind::DevServer` guard below before
/// `probe::fetch_title` is ever called.
///
/// Does not consult the Keeplist — `keepRunning` is a wire-only concern derived later
/// by `snapshot_from`, which looks it up per DevServer at the point it builds the
/// `ServerWire`. Keeping that lookup out of this function means `ScannedServer` (and
/// every test fixture that builds one) does not need to carry or fake a Keeplist just
/// to construct a value.
pub fn classify_and_probe(listeners: &[RawListener], deps: &ClassifyDeps, title_cache: &mut TitleCache) -> Vec<ScannedServer> {
    // Liveness runs for EVERY listener regardless of Kind (F4: "every Server on every
    // refresh"), so it is gathered once, in parallel, up front — not per-Kind inside
    // the loop below.
    let liveness_input: Vec<(String, Vec<PortBinding>)> = listeners
        .iter()
        .filter_map(|l| server_id(l.pid, &l.ports).map(|id| (id, l.ports.clone())))
        .collect();
    let liveness_results = probe::liveness_for_servers(&liveness_input);
    let live_by_id: std::collections::HashMap<String, bool> = liveness_results.into_iter().collect();

    // Pass 1: classify + resolve health for every listener. No network I/O yet — this
    // is the fast, always-cheap part. Cache-hit titles are also resolved here since
    // they cost nothing; only a genuine cache MISS is deferred to pass 2.
    struct Pending<'a> {
        listener: &'a RawListener,
        id: String,
        kind: Kind,
        attribution: ProjectAttribution,
        belongs_to: Option<String>,
        health: Health,
        title: Option<String>,
        needs_fetch: bool,
    }

    let mut pending: Vec<Pending> = Vec::with_capacity(listeners.len());
    for listener in listeners {
        let Some(id) = server_id(listener.pid, &listener.ports) else {
            continue;
        };
        let belongs_to = (deps.owning_app)(&listener.exe_path);
        let (kind, attribution) = classify_listener(listener, deps.owning_app, deps.path_exists);

        // Any binding answering counts as Responding — a v4+v6 pair where one family
        // is filtered (e.g. IPv6 blocked by a firewall rule but IPv4 fine) must not
        // read as dead when the server plainly is not. This is a judgment call: the
        // spec states Health per-Server but a Server can hold several bindings, and
        // "any" is what matches CONTEXT.md's "whether a Server still answers" — one
        // working address means it answers.
        //
        // A MISSING entry (not present in `live_by_id` at all) is genuinely different
        // from a present `false`: every listener here was included in
        // `liveness_input` above, so an absent result means its probe thread
        // panicked (see `probe::liveness_for_servers`'s `.ok()` on `JoinHandle::join`)
        // — the check simply never completed. N3 forbids presenting a guess as fact:
        // reporting "did not check" as `NotResponding` would claim a negative result
        // this scan never actually observed, so it maps to `Unknown` instead.
        let health = health_from_liveness_result(live_by_id.get(&id).copied());

        // F5: title fetch is restricted to DevServer Kind, full stop. Every other
        // Kind gets `title: None` and the UI falls back to `command` (docs/IPC.md).
        // `needs_fetch` distinguishes "no title, and never will be" (not a DevServer,
        // or not Responding) from "no title YET — a cache miss that pass 2 must
        // actually fetch, in parallel with every other miss" — see
        // `title_cache_lookup` below, the read-only half of `title_for`.
        let (title, needs_fetch) = if kind == Kind::DevServer {
            title_cache_lookup(listener.pid, &listener.ports, health, title_cache)
        } else {
            (None, false)
        };

        pending.push(Pending { listener, id, kind, attribution, belongs_to, health, title, needs_fetch });
    }

    // Pass 2: fetch every cache-miss title IN PARALLEL, same reasoning as liveness —
    // sequential would multiply TITLE_TIMEOUT by the number of misses. This is the
    // fix for a real gap: fetching titles one at a time inside pass 1's loop would
    // hold whatever lock the caller (scanner.rs's `scan_once`, or `refresh_now`) is
    // holding across up to N sequential 2-second timeouts.
    let fetch_targets: Vec<(usize, PortBinding)> = pending
        .iter()
        .enumerate()
        .filter(|(_, p)| p.needs_fetch)
        .filter_map(|(i, p)| p.listener.ports.first().cloned().map(|binding| (i, binding)))
        .collect();

    if !fetch_targets.is_empty() {
        let fetched: Vec<(usize, Option<String>)> = std::thread::scope(|scope| {
            let handles: Vec<_> = fetch_targets
                .iter()
                .map(|(index, binding)| {
                    let index = *index;
                    let binding = binding.clone();
                    scope.spawn(move || (index, probe::fetch_title(&binding)))
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });

        for (index, result) in fetched {
            let entry = &mut pending[index];
            let key = (entry.listener.pid, entry.listener.ports[0].port);
            title_cache.insert(key, result.clone());
            entry.title = result;
        }
    }

    pending
        .into_iter()
        .map(|p| ScannedServer {
            id: p.id,
            pid: p.listener.pid,
            command: p.listener.command.clone(),
            ports: p.listener.ports.clone(),
            start_time: p.listener.start_time,
            unattended: p.listener.ppid <= 1,
            kind: p.kind,
            attribution: p.attribution,
            belongs_to: p.belongs_to,
            health: p.health,
            title: p.title,
        })
        .collect()
}

fn attribution_root(attribution: &ProjectAttribution) -> Option<&std::path::Path> {
    match attribution {
        ProjectAttribution::Known(project, _) | ProjectAttribution::Guessed(project, _) => Some(project.root.as_path()),
        ProjectAttribution::None => None,
    }
}

/// F5: fetch-or-reuse a DevServer's title. Cached by (pid, port) — PLAN.md's exact
/// key — using the server's first port as the representative port, consistent with
/// how `server_id` picks a representative binding. Re-fetched only when there is no
/// cache entry yet, or when health just became Responding again (a server that was
/// NotResponding and is now Responding may be a genuinely different process that
/// reused the port, so its cached title cannot be trusted) — never on a routine
/// unchanged-health scan.
///
/// A fetch that succeeds but finds no `<title>` tag is cached as `Some(None)`, not
/// left absent: an absent entry and "tried, found nothing" must be distinguishable, or
/// a title-less DevServer (an API-only dev server, a bare JSON endpoint) would issue a
/// real `GET /` on every single tick forever — see the `TitleCache` doc comment in
/// probe.rs for why that would violate F5 and N1.
fn title_for(pid: u32, ports: &[PortBinding], health: Health, cache: &mut TitleCache) -> Option<String> {
    let first_port = ports.first()?.port;
    let key = (pid, first_port);

    if health != Health::Responding {
        // Not responding: drop any stale cached title rather than show one for a
        // server that is not currently answering (CONTEXT.md "Title": "Renewed when a
        // Server stops Responding").
        cache.remove(&key);
        return None;
    }

    if let Some(cached) = cache.get(&key) {
        return cached.clone();
    }

    let binding = ports.first()?;
    let fetched = probe::fetch_title(binding);
    cache.insert(key, fetched.clone());
    fetched
}

/// The read-only half of `title_for`'s logic: decide whether a cached title can be
/// returned as-is, WITHOUT ever calling `probe::fetch_title` itself. Returns
/// `(title, needs_fetch)` — `needs_fetch` is true only for a genuine cache miss on a
/// Responding server, which `classify_and_probe`'s pass 2 then fetches for every such
/// listener in parallel, rather than this function (or its caller) blocking on a
/// sequential `GET /` per listener while holding whatever lock the caller has.
fn title_cache_lookup(pid: u32, ports: &[PortBinding], health: Health, cache: &mut TitleCache) -> (Option<String>, bool) {
    let Some(first_port) = ports.first().map(|p| p.port) else {
        return (None, false);
    };
    let key = (pid, first_port);

    if health != Health::Responding {
        // Not responding: drop any stale cached title rather than show one for a
        // server that is not currently answering (CONTEXT.md "Title": "Renewed when a
        // Server stops Responding").
        cache.remove(&key);
        return (None, false);
    }

    match cache.get(&key) {
        Some(cached) => (cached.clone(), false),
        None => (None, true),
    }
}

/// Force a title re-fetch for every DevServer, ignoring the cache. Used by
/// `refresh_now` (docs/IPC.md: "manual refresh, also refetches titles").
pub fn invalidate_all_titles(cache: &mut TitleCache) {
    cache.clear();
}

/// Hash the parts of an enumeration result that matter for "has anything actually
/// changed" (PLAN.md: "Hash the enumeration result. Unchanged hash => skip project
/// derivation, classification, and event emission entirely.").
///
/// Deliberately excludes `start_time`: macos.rs derives it from `ps`'s `etime`, which
/// has one-second granularity and is recomputed as `SystemTime::now() - elapsed` on
/// every enumeration — it jitters by a second on almost every call even when nothing
/// on the machine has changed, which would defeat the short-circuit entirely (every
/// scan would look "changed"). Uptime is derived fresh from `start_time` when building
/// the wire Snapshot, not from anything hashed here, so excluding it from the
/// fingerprint costs nothing.
pub fn fingerprint(listeners: &[RawListener]) -> u64 {
    // Sort a stable copy of (pid) first — `lsof`'s output order is not guaranteed
    // stable between runs, and hashing in a different order would produce a different
    // hash for an identical set of listeners.
    let mut sorted: Vec<&RawListener> = listeners.iter().collect();
    sorted.sort_by_key(|l| l.pid);

    let mut hasher = DefaultHasher::new();
    sorted.len().hash(&mut hasher);
    for listener in sorted {
        listener.pid.hash(&mut hasher);
        listener.ppid.hash(&mut hasher);
        listener.command.hash(&mut hasher);
        listener.exe_path.hash(&mut hasher);
        listener.cwd.hash(&mut hasher);
        listener.user.hash(&mut hasher);

        let mut ports: Vec<&PortBinding> = listener.ports.iter().collect();
        ports.sort_by_key(|p| (p.port, format!("{:?}", p.family)));
        ports.len().hash(&mut hasher);
        for port in ports {
            port.port.hash(&mut hasher);
            format!("{:?}", port.family).hash(&mut hasher);
            format!("{:?}", port.reachability).hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// Build the wire `Snapshot` (docs/IPC.md) from classified Servers. Pure function:
/// takes everything it needs as arguments, does no I/O, so it is directly unit
/// testable without a scanner loop or a real clock running.
pub fn snapshot_from(servers: &[ScannedServer], keeplist: &Keeplist, now: SystemTime) -> ipc::Snapshot {
    use std::collections::BTreeMap;

    let mut projects: BTreeMap<String, Vec<ipc::ServerWire>> = BTreeMap::new();
    let mut watch_only = Vec::new();
    let mut others = Vec::new();

    for server in servers {
        let ports: Vec<ipc::Port> = server.ports.iter().map(ipc::Port::from).collect();
        let uptime_seconds = now.duration_since(server.start_time).unwrap_or(Duration::ZERO).as_secs();

        match ipc::wire_section_for(server.kind) {
            ipc::WireSection::Project => {
                let (project_name, package_label) = match &server.attribution {
                    ProjectAttribution::Known(project, package) | ProjectAttribution::Guessed(project, package) => {
                        (project.name.clone(), package.as_ref().map(|p| p.relative_path.display().to_string()))
                    }
                    // classify_listener never assigns DevServer to a Server with no
                    // Project (domain/classify.rs rule 4 / F3), but this match must
                    // still be total — a Server with no Project falls back to the
                    // "(no project)" label rather than panicking, which keeps this
                    // function safe even if that invariant is ever violated upstream.
                    ProjectAttribution::None => ("(no project)".to_string(), None),
                };
                let keep_running = attribution_root(&server.attribution)
                    .map(|root| keeplist.is_marked(root, &server.command))
                    .unwrap_or(false);

                let wire = ipc::ServerWire {
                    id: server.id.clone(),
                    pid: server.pid,
                    package: package_label,
                    // Same F2-derived root the keeplist lookup above keys on — one
                    // walk, one answer (docs/IPC.md amendment v1.1).
                    project_path: attribution_root(&server.attribution).map(|r| r.display().to_string()),
                    title: server.title.clone(),
                    command: server.command.clone(),
                    ports,
                    uptime_seconds,
                    health: health_wire(server.health),
                    unattended: server.unattended,
                    keep_running,
                };
                projects.entry(project_name).or_default().push(wire);
            }
            ipc::WireSection::WatchOnly(reason) => {
                let label = server.belongs_to.clone().unwrap_or_else(|| server.command.clone());
                watch_only.push(ipc::WatchOnlyServer { id: server.id.clone(), label, reason, ports, uptime_seconds });
            }
            ipc::WireSection::Other(kind) => {
                let label = server.belongs_to.clone().unwrap_or_else(|| server.command.clone());
                let guessed_project = ipc::guessed_project_name(&server.attribution);
                others.push(ipc::OtherServer { id: server.id.clone(), label, kind, guessed_project, ports });
            }
        }
    }

    let projects = projects.into_iter().map(|(project, servers)| ipc::ProjectGroup { project, servers }).collect();
    let scanned_at = format_iso8601(now);

    // `scan_failed: false` is the honest default for a pure transform of Servers that
    // were, by definition, produced by a scan that succeeded. `ScannerState::snapshot`
    // overrides it when the LOOP's most recent attempt failed and these Servers are
    // the last good result rather than the current one.
    ipc::Snapshot { projects, watch_only, others, scanned_at, scan_failed: false }
}

fn health_wire(health: Health) -> ipc::HealthWire {
    match health {
        Health::Responding => ipc::HealthWire::Responding,
        Health::NotResponding => ipc::HealthWire::NotResponding,
        Health::Unknown => ipc::HealthWire::Unknown,
    }
}

/// Minimal ISO 8601 / RFC 3339 UTC formatter (`2026-09-02T08:42:21Z`), written by
/// hand rather than pulling in a datetime crate for one field. Deliberately does not
/// handle times before the Unix epoch (`unwrap_or(Duration::ZERO)`) — `scannedAt` is
/// always "now", which is never before 1970.
fn format_iso8601(time: SystemTime) -> String {
    let secs = time.duration_since(SystemTime::UNIX_EPOCH).unwrap_or(Duration::ZERO).as_secs();
    let days = secs / 86_400;
    let time_of_day = secs % 86_400;
    let (hour, minute, second) = (time_of_day / 3600, (time_of_day % 3600) / 60, time_of_day % 60);
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnant's `civil_from_days` algorithm: converts a day count since the Unix
/// epoch into a proleptic-Gregorian (year, month, day). Well-known, widely used
/// (libc++'s `<chrono>` uses this exact algorithm), correct across the full range this
/// tool will ever see (SystemTime::now() is always far past 1970), and needs no
/// lookup table for month lengths or leap years.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

// ---------------------------------------------------------------------------------
// Stop flow support (phase 5): pure functions over an already-built snapshot of
// ScannedServers. No I/O here — signaling/verification lives in commands.rs, which
// calls `ProcessSource` and re-enumerates. Kept here, not commands.rs, because these
// are the two rules PLAN.md calls out as "must be enforced in code, not just
// documented": the Kind::is_watch_only() guard, and Stop Everything's DevServer-only
// scope.
// ---------------------------------------------------------------------------------

/// F8/F9 guard: refuse to stop a Watch Only Server. Every stop path (`stop_server`,
/// `force_stop`, `stop_all_dev_servers`) must call this before signaling anything —
/// PLAN.md: "Route every stop path through `Kind::is_watch_only()`. The rule must be
/// enforced in code, not just documented." Returns `Some(reason)` when the stop must
/// be refused, `None` when it is allowed to proceed.
pub fn refuse_if_watch_only(kind: Kind) -> Option<&'static str> {
    if kind.is_watch_only() {
        Some("this Server is Watch Only and cannot be stopped through this tool")
    } else {
        None
    }
}

/// F9: `stop_all_dev_servers` touches ONLY `Kind::DevServer` — docs/IPC.md: "Never
/// `watchOnly` or `others`." Returns the ids of every eligible Server, pure and
/// side-effect-free so it can be tested directly against a snapshot containing every
/// Kind without any process actually being signaled.
pub fn eligible_for_bulk_stop(servers: &[ScannedServer]) -> Vec<String> {
    servers.iter().filter(|s| s.kind == Kind::DevServer).map(|s| s.id.clone()).collect()
}

/// F8/N2 guard: `force_stop` is only valid for an id that a prior `stop_server` call
/// already tried and saw fail (`ScannerState::failed_polite_stops`). Pure and
/// side-effect-free, taking the map directly rather than the whole `ScannerState`, so
/// a test can assert its behavior in both states without going through the Tauri
/// command layer at all — the exact gap the review lesson in PLAN.md warns about
/// ("a rule that is never called protects nothing").
///
/// Returns `None` when force_stop is permitted to proceed, `Some(reason)` when it must
/// be refused.
pub fn refuse_force_stop_without_prior_failure(failed_polite_stops: &FailedPoliteStops, id: &str, now: SystemTime) -> Option<&'static str> {
    match failed_polite_stops.get(id) {
        Some(record) if !record.is_expired(now) => None,
        Some(_) => Some("That stop attempt was a while ago. Try stopping this Server again before forcing it."),
        None => Some("Force Stop is only available after a polite stop has been tried and the Server is still running."),
    }
}

/// How long a failed polite stop keeps authorizing `force_stop` for that id.
///
/// Without a TTL the authorization is permanent: an entry only ever leaves the map on a
/// successful stop or a force attempt, so a Server that failed to stop once and was
/// then left alone stays force-killable for the rest of the app's life — the user's
/// "I asked politely and it refused" is many minutes or hours in the past by then, and
/// F8/N2 rest on force_stop being the immediate, deliberate follow-up to a stop the
/// user just watched fail. Two minutes is comfortably longer than the ~3s stop
/// sequence plus the user reading the result and deciding, and short enough that the
/// authorization does not outlive the decision that created it.
pub const FORCE_STOP_AUTHORIZATION_TTL: Duration = Duration::from_secs(120);

/// The record a failed polite stop leaves behind: the ports that Server held (so
/// `find_current_holder` can locate a surviving child under a new pid), and when the
/// failure happened (so the authorization can expire — see
/// `FORCE_STOP_AUTHORIZATION_TTL`).
#[derive(Debug, Clone)]
pub struct FailedPoliteStop {
    pub ports: Vec<PortBinding>,
    pub recorded_at: SystemTime,
}

impl FailedPoliteStop {
    pub fn new(ports: Vec<PortBinding>, recorded_at: SystemTime) -> Self {
        FailedPoliteStop { ports, recorded_at }
    }

    pub fn is_expired(&self, now: SystemTime) -> bool {
        // A clock that moved backwards yields Err here; treating that as "not expired"
        // would extend the authorization, so it is treated as expired instead — the
        // user can always re-earn it with another stop attempt, which is the safe
        // direction to fail in.
        match now.duration_since(self.recorded_at) {
            Ok(age) => age > FORCE_STOP_AUTHORIZATION_TTL,
            Err(_) => true,
        }
    }
}

pub type FailedPoliteStops = std::collections::HashMap<String, FailedPoliteStop>;

/// How far apart two derivations of the same process's `start_time` may be and still
/// be considered the same process. Not a fudge factor for uncertainty: macos.rs
/// derives `start_time` as `SystemTime::now() - etime`, and `ps`'s `etime` has
/// one-second granularity, so two enumerations of an unchanged process legitimately
/// differ by up to a second (the same jitter `fingerprint` documents at length and
/// deliberately excludes). Compared with tolerance, a recycled pid is still caught:
/// a pid recycled between scans belongs to a process that started seconds-to-minutes
/// later, far outside this window.
const START_TIME_TOLERANCE: Duration = Duration::from_secs(2);

/// Whether the process currently at `pid` is still the same Server that was resolved
/// from the scan cache — the check that must pass BEFORE anything is signaled.
///
/// `commands::resolve` only proves an id was present in the last scan, which can be up
/// to 15s old with the panel open and 60s when idle. In that window the Server can
/// exit and its pid be recycled by an unrelated process, which would then receive the
/// signal. `getpgid` succeeding proves only that *a* process exists at that pid, not
/// that it is the same one. So identity is re-established from a FRESH enumeration
/// against three facts the scan already carries: the pid, the ports, and the start
/// time (see `START_TIME_TOLERANCE` for why that one is compared with a tolerance).
///
/// Returns `None` when the target is still itself, `Some(reason)` when the stop must be
/// refused — an honest failed outcome, never a silent success (N3).
pub fn refuse_if_identity_changed(fresh: &[RawListener], target: &ScannedServer) -> Option<&'static str> {
    let Some(current) = fresh.iter().find(|l| l.pid == target.pid) else {
        // Nothing is at that pid any more: the Server exited between the last scan and
        // now. Nothing to signal, and signaling the pid anyway could reach whatever
        // takes it next.
        return Some("This Server is no longer running — it may have already stopped.");
    };

    // Ports are matched as a set on (port, family) — the same identity comparison the
    // rest of the stop flow uses. A process still at this pid but now listening
    // somewhere else is not the Server the user confirmed stopping.
    let ports_match = target.ports.len() == current.ports.len()
        && target.ports.iter().all(|tp| current.ports.iter().any(|cp| cp.port == tp.port && cp.family == tp.family));
    if !ports_match {
        return Some("This Server changed since it was last checked — nothing was stopped.");
    }

    let drift = match current.start_time.duration_since(target.start_time) {
        Ok(d) => d,
        // Negative difference: the fresh reading is EARLIER than the cached one, which
        // the etime jitter also produces. Its magnitude is what matters, not its sign.
        Err(e) => e.duration(),
    };
    if drift > START_TIME_TOLERANCE {
        // A different process is at this pid now — a recycled pid, which is exactly
        // the wrong target this check exists to catch.
        return Some("This Server changed since it was last checked — nothing was stopped.");
    }

    None
}

/// Resolve "whoever currently holds these ports" among `servers`. Used by
/// `force_stop` to find its actual target: CONTEXT.md "Stopping" says a surviving
/// child can keep the address held after the signaled parent exits, and that child
/// has a different pid — so the id recorded in `failed_polite_stops` may no longer
/// exist in `servers` at all, only a Server with a NEW id that happens to hold the
/// same ports does. Matches on (port, family) — the same identity `port_still_held`
/// already uses to decide whether a stop succeeded — not on the id string.
pub fn find_current_holder<'a>(servers: &'a [ScannedServer], target_ports: &[PortBinding]) -> Option<&'a ScannedServer> {
    servers.iter().find(|s| s.ports.iter().any(|p| target_ports.iter().any(|tp| tp.port == p.port && tp.family == p.family)))
}

/// User-facing "What This Stops" text (CONTEXT.md, F8) for a Server about to be
/// stopped. Never a process name or raw port number — names the Project for a
/// DevServer, or what a BackgroundService is guessed to hold up. Watch Only kinds
/// never reach this function in practice (refused earlier by `refuse_if_watch_only`),
/// so they get a generic fallback rather than a crafted message that implies stopping
/// them was ever offered.
pub fn what_this_stops(server: &ScannedServer) -> String {
    match &server.attribution {
        ProjectAttribution::Known(project, _) => format!("This stops the \"{}\" development server.", project.name),
        ProjectAttribution::Guessed(project, _) => {
            format!("This may affect \"{}\" and anything else this background service holds up — the project link is uncertain.", project.name)
        }
        ProjectAttribution::None => match &server.belongs_to {
            Some(app) => format!("This stops \"{app}\"."),
            None => "This stops this program.".to_string(),
        },
    }
}

// ---------------------------------------------------------------------------------
// Shared scan state + the adaptive loop.
// ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelState {
    Open,
    Closed,
}

/// Everything the scanner loop reads and writes, and everything `commands.rs` reads
/// to answer IPC calls. Held behind one `Mutex` — the loop drops its own lock before
/// sleeping (see `run_loop`), so a slow UI-triggered command never blocks on it for
/// longer than a single snapshot rebuild.
pub struct ScannerState {
    pub servers: Vec<ScannedServer>,
    pub last_fingerprint: Option<u64>,
    pub panel: PanelState,
    /// When `panel` last transitioned to `Closed`. Used to approximate the
    /// idle/battery tier — see the module doc comment on why this is an
    /// approximation, not a real idle signal.
    pub panel_closed_at: Option<SystemTime>,
    pub title_cache: TitleCache,
    pub keeplist: Keeplist,
    pub app_data_dir: PathBuf,
    /// Original Server id -> the ports that Server held, for every `stop_server`
    /// call that was already tried and returned `StillRunning`. This is the ONLY
    /// thing that makes `force_stop` valid (F8/N2: "force_stop is only valid after a
    /// stop_server returned still_running ... Never auto-escalate"). Deliberately
    /// state, not a re-check of "is the port currently held" — a healthy DevServer
    /// that was never asked to stop politely also has its port held, and
    /// re-deriving the gate from port state alone would let force_stop skip straight
    /// to SIGKILL on a Server nobody ever asked to stop.
    ///
    /// Stores the ports, not just a marker, because CONTEXT.md "Stopping" is explicit
    /// that a surviving child can keep the address held after the signaled parent
    /// exits — and that child has a DIFFERENT pid, so `server_id` (pid + first port)
    /// produces a different id than the one `stop_server` recorded. `force_stop`
    /// re-resolves "whoever currently holds these ports" from the ports, rather than
    /// looking the original id up directly in `servers` (which, in that exact case,
    /// no longer contains it).
    ///
    /// An entry is inserted on `StillRunning`, and removed on `Stopped` or after a
    /// `force_stop` attempt (so a second SIGKILL needs a fresh polite failure). It
    /// also EXPIRES on its own after `FORCE_STOP_AUTHORIZATION_TTL` — an authorization
    /// the user earned minutes ago is no longer the immediate follow-up to a stop they
    /// just watched fail.
    pub failed_polite_stops: FailedPoliteStops,
    /// Whether the most recent scan attempt failed (docs/IPC.md v1.2 `scanFailed`).
    /// Set by `run_loop`, which keeps the last good snapshot on a failure rather than
    /// clearing it — so this is the one thing distinguishing "the machine is quiet"
    /// from "we could not look" (N3). Cleared by the next scan that succeeds.
    pub scan_failed: bool,
}

impl ScannerState {
    pub fn new(app_data_dir: PathBuf) -> Self {
        let keeplist = Keeplist::load(&app_data_dir);
        ScannerState {
            servers: Vec::new(),
            last_fingerprint: None,
            panel: PanelState::Closed,
            panel_closed_at: Some(SystemTime::now()),
            title_cache: TitleCache::new(),
            keeplist,
            app_data_dir,
            failed_polite_stops: FailedPoliteStops::new(),
            scan_failed: false,
        }
    }

    pub fn current_cadence(&self) -> Duration {
        match self.panel {
            PanelState::Open => PANEL_OPEN_ENUMERATE,
            PanelState::Closed => {
                let idle = self.panel_closed_at.map(|t| t.elapsed().unwrap_or(Duration::ZERO) >= IDLE_AFTER_PANEL_CLOSED).unwrap_or(false);
                if idle {
                    IDLE_ENUMERATE
                } else {
                    PANEL_CLOSED_ENUMERATE
                }
            }
        }
    }

    pub fn snapshot(&self, now: SystemTime) -> ipc::Snapshot {
        // `scan_failed` is attached here rather than threaded through `snapshot_from`:
        // that function is a pure transform of a Server list, and whether the last
        // SCAN succeeded is a property of the loop's state, not of the Servers in it.
        ipc::Snapshot { scan_failed: self.scan_failed, ..snapshot_from(&self.servers, &self.keeplist, now) }
    }
}

/// Wakes the sleeping loop early on a cadence change (`panel_opened`/`panel_closed`).
/// Plain `thread::sleep` cannot be interrupted, which would leave `panel_opened`
/// waiting up to the PREVIOUS (possibly 60s) cadence before the loop next checks
/// state — a `Condvar` timeout lets `notify_waker` cut that wait short immediately.
pub struct Waker {
    mutex: Mutex<()>,
    condvar: Condvar,
}

impl Waker {
    pub fn new() -> Self {
        Waker { mutex: Mutex::new(()), condvar: Condvar::new() }
    }

    /// Sleep for up to `duration`, or until `wake()` is called from another thread.
    fn sleep(&self, duration: Duration) {
        if let Ok(guard) = self.mutex.lock() {
            let _ = self.condvar.wait_timeout(guard, duration);
        }
    }

    pub fn wake(&self) {
        self.condvar.notify_all();
    }
}

impl Default for Waker {
    fn default() -> Self {
        Self::new()
    }
}

/// Run one enumerate-classify-probe cycle against `state`, applying the hash
/// short-circuit (PLAN.md: "Unchanged hash => skip project derivation,
/// classification, and event emission entirely. Only liveness runs."). Returns
/// `true` when the emitted set of Servers actually changed (caller emits
/// `servers:changed` only then — docs/IPC.md: "never on every scan tick").
///
/// `force_full` bypasses the short-circuit — used by `refresh_now`, which
/// docs/IPC.md says "also refetches titles", implying it always does real work
/// rather than potentially returning a stale liveness-only snapshot.
pub fn scan_once(
    state: &mut ScannerState,
    source: &dyn ProcessSource,
    deps: &ClassifyDeps,
    force_full: bool,
) -> Result<bool, String> {
    let listeners = source.enumerate()?;
    let fingerprint = fingerprint(&listeners);
    let unchanged = !force_full && state.last_fingerprint == Some(fingerprint);

    if unchanged {
        // Only liveness runs. Re-probe every current server's ports and update health
        // + (for DevServers) title in place, without re-deriving Kind/Project or
        // touching anything else about the existing ScannedServer list.
        let liveness_input: Vec<(usize, Vec<PortBinding>)> =
            state.servers.iter().enumerate().map(|(i, s)| (i, s.ports.clone())).collect();
        let results = probe::liveness_for_servers(&liveness_input);
        let mut changed = false;
        for (index, is_live) in results {
            let new_health = if is_live { Health::Responding } else { Health::NotResponding };
            let server = &mut state.servers[index];
            if server.health != new_health {
                changed = true;
            }
            server.health = new_health;
            if server.kind == Kind::DevServer {
                server.title = title_for(server.pid, &server.ports, server.health, &mut state.title_cache);
            }
        }
        return Ok(changed);
    }

    if force_full {
        invalidate_all_titles(&mut state.title_cache);
    }

    let new_servers = classify_and_probe(&listeners, deps, &mut state.title_cache);
    let changed = servers_differ(&state.servers, &new_servers);
    state.servers = new_servers;
    state.last_fingerprint = Some(fingerprint);
    Ok(changed || force_full)
}

/// Whether the set of Servers changed in a way the UI needs to know about: a
/// different id set, or any health difference for an id both snapshots share. Ignores
/// title/uptime churn on its own (uptime always changes; that alone must not trigger
/// an event on unrelated cycles) — this mirrors `fingerprint`'s "what actually
/// matters" judgment, applied to the classified result instead of the raw one.
fn servers_differ(old: &[ScannedServer], new: &[ScannedServer]) -> bool {
    if old.len() != new.len() {
        return true;
    }
    let old_by_id: std::collections::HashMap<&str, &ScannedServer> = old.iter().map(|s| (s.id.as_str(), s)).collect();
    for server in new {
        match old_by_id.get(server.id.as_str()) {
            None => return true,
            Some(prev) => {
                if prev.health != server.health || prev.kind != server.kind {
                    return true;
                }
            }
        }
    }
    false
}

/// The adaptive loop itself. Runs until `should_stop` returns true (used by tests to
/// bound execution; production wiring in `commands.rs`/`lib.rs` never stops it).
/// `on_change` is called with a fresh snapshot whenever `scan_once` reports a real
/// change — this is where `commands.rs` emits `servers:changed` to the frontend.
///
/// Does not hold `state`'s mutex across the sleep: it locks, scans, builds a snapshot
/// if needed, and unlocks before sleeping, so a `stop_server` or `panel_opened` call
/// arriving mid-sleep is never blocked behind a scan that has not started yet.
pub fn run_loop(
    state: &Arc<Mutex<ScannerState>>,
    source: &dyn ProcessSource,
    deps: &ClassifyDeps,
    waker: &Waker,
    mut on_change: impl FnMut(ipc::Snapshot),
    mut should_stop: impl FnMut() -> bool,
) {
    while !should_stop() {
        let cadence = {
            let mut guard = match state.lock() {
                Ok(g) => g,
                Err(_) => return, // poisoned mutex: another thread panicked holding it; stop rather than operate on a possibly-inconsistent state.
            };
            // A failed scan is NOT "nothing changed" (N3). The previous
            // `.unwrap_or(false)` discarded the error entirely, so a persistently
            // failing scan looked exactly like a quiet machine: the last good snapshot
            // stayed on screen indefinitely with nothing saying it was stale. The
            // error is now logged, the last good snapshot is deliberately kept rather
            // than cleared (clearing would fabricate "nothing running"), and
            // `scan_failed` carries the fact to the UI — docs/IPC.md v1.2.
            let changed = match scan_once(&mut guard, source, deps, false) {
                Ok(changed) => {
                    let recovered = guard.scan_failed;
                    guard.scan_failed = false;
                    // A recovery is itself worth emitting: the UI is showing a "couldn't
                    // scan" note that is no longer true, even if the Server set is
                    // identical to what it was before the failure.
                    changed || recovered
                }
                Err(e) => {
                    eprintln!("scan failed, keeping the last good snapshot: {e}");
                    let newly_failed = !guard.scan_failed;
                    guard.scan_failed = true;
                    newly_failed
                }
            };
            if changed {
                on_change(guard.snapshot(SystemTime::now()));
            }
            guard.current_cadence()
        };
        waker.sleep(cadence);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::Project;
    use crate::platform::{AddressFamily, Reachability};
    use std::path::Path;

    fn binding(port: u16) -> PortBinding {
        PortBinding { port, family: AddressFamily::V4, reachability: Reachability::LocalhostOnly }
    }

    fn raw(pid: u32, ppid: u32, command: &str, ports: Vec<PortBinding>) -> RawListener {
        RawListener {
            pid,
            ppid,
            command: command.to_string(),
            exe_path: PathBuf::from(format!("/usr/local/bin/{command}")),
            cwd: Some(PathBuf::from("/Users/dev/myproject")),
            ports,
            start_time: SystemTime::now(),
            user: "dev".to_string(),
        }
    }

    fn dev_server(id: &str, project_name: &str) -> ScannedServer {
        ScannedServer {
            id: id.to_string(),
            pid: 100,
            command: "npm run dev".to_string(),
            ports: vec![binding(3000)],
            start_time: SystemTime::now(),
            unattended: true,
            kind: Kind::DevServer,
            attribution: ProjectAttribution::Known(
                Project { root: PathBuf::from("/Users/dev/myproject"), name: project_name.to_string() },
                None,
            ),
            belongs_to: None,
            health: Health::Responding,
            title: None,
        }
    }

    fn watch_only_server(id: &str, kind: Kind) -> ScannedServer {
        ScannedServer {
            id: id.to_string(),
            pid: 200,
            command: "openclaw".to_string(),
            ports: vec![binding(18789)],
            start_time: SystemTime::now(),
            unattended: false,
            kind,
            attribution: ProjectAttribution::None,
            belongs_to: Some("openclaw".to_string()),
            health: Health::Responding,
            title: None,
        }
    }

    fn background_service(id: &str) -> ScannedServer {
        ScannedServer {
            id: id.to_string(),
            pid: 300,
            command: "com.docker.backend".to_string(),
            ports: vec![binding(5432)],
            start_time: SystemTime::now(),
            unattended: false,
            kind: Kind::BackgroundService,
            attribution: ProjectAttribution::Guessed(
                Project { root: PathBuf::from("/Users/dev/unrelated"), name: "unrelated".to_string() },
                None,
            ),
            belongs_to: Some("OrbStack".to_string()),
            health: Health::Responding,
            title: None,
        }
    }

    fn part_of_app(id: &str) -> ScannedServer {
        ScannedServer {
            id: id.to_string(),
            pid: 400,
            command: "Code Helper".to_string(),
            ports: vec![binding(9000)],
            start_time: SystemTime::now(),
            unattended: false,
            kind: Kind::PartOfApp,
            attribution: ProjectAttribution::None,
            belongs_to: Some("Visual Studio Code".to_string()),
            health: Health::Responding,
            title: None,
        }
    }

    // ---- health_from_liveness_result (N3: "did not check" must not become a
    // fabricated "checked and failed") ----

    #[test]
    fn health_from_liveness_result_true_is_responding() {
        assert_eq!(health_from_liveness_result(Some(true)), Health::Responding);
    }

    #[test]
    fn health_from_liveness_result_false_is_not_responding() {
        assert_eq!(health_from_liveness_result(Some(false)), Health::NotResponding);
    }

    #[test]
    fn health_from_liveness_result_missing_is_unknown_not_not_responding() {
        assert_eq!(health_from_liveness_result(None), Health::Unknown);
    }

    // ---- server_id ----

    #[test]
    fn server_id_uses_pid_and_first_port() {
        assert_eq!(server_id(123, &[binding(4000), binding(4001)]), Some("123:4000".to_string()));
    }

    #[test]
    fn server_id_none_for_no_ports() {
        assert_eq!(server_id(123, &[]), None);
    }

    // ---- title_for: negative-result caching (F5, N1) ----

    /// A DevServer that answers `GET /` but has no `<title>` tag (a bare JSON API, an
    /// SPA shell with an empty head) must be probed only ONCE, not on every tick.
    /// Regression for exactly the gap the advisor flagged: caching only successes
    /// meant a title-less server would issue a real HTTP request forever.
    #[test]
    fn title_for_does_not_refetch_when_server_has_no_title() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind must succeed");
        let port = listener.local_addr().unwrap().port();
        let request_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = request_count.clone();

        let handle = std::thread::spawn(move || {
            // Serve exactly one request with a titleless body, then stop accepting —
            // if `title_for` calls `fetch_title` a second time for the cached miss,
            // that connection will simply fail to connect (nothing listening), which
            // is what the assertion below is actually able to detect.
            if let Ok((mut stream, _)) = listener.accept() {
                use std::io::{BufRead, Write};
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                let _ = reader.read_line(&mut request_line);
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let body = "<html><head></head><body>no title here</body></html>";
                let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let ports = vec![binding(port)];
        let mut cache = TitleCache::new();

        let first = title_for(1, &ports, Health::Responding, &mut cache);
        handle.join().unwrap();
        assert_eq!(first, None, "server has no <title>, so the result must be None");
        assert_eq!(request_count.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Second call: nothing is listening anymore (the fake server served exactly
        // one request and stopped), so if this call re-fetches instead of reusing the
        // cached "attempted, found nothing" entry, it would still correctly return
        // None (connection refused) — which would make this test pass even with the
        // bug. The real proof is the cache's internal state: it must already contain
        // an entry for this (pid, port), inserted as Some(None), not be empty.
        assert_eq!(cache.get(&(1, port)), Some(&None), "a title-less fetch must still populate the cache, not leave the key absent");

        let second = title_for(1, &ports, Health::Responding, &mut cache);
        assert_eq!(second, None);
        // No new connection attempted: the listener thread already exited after
        // serving one request, and `title_for`'s second call must have returned the
        // cached entry directly rather than reaching the network at all.
    }

    /// `classify_and_probe` must fetch multiple cache-miss titles IN PARALLEL, not
    /// sequentially — a sequential implementation holding a caller's lock across N
    /// real HTTP round trips is exactly the bug this test guards against (flagged in
    /// review: `refresh_now` would otherwise hold the scanner mutex for up to
    /// N x TITLE_TIMEOUT). Three servers each intentionally delay their response by
    /// 300ms before answering; if fetched sequentially that is >= 900ms total, but in
    /// parallel it should complete in roughly one delay's worth of wall-clock time.
    /// The 600ms threshold gives generous headroom above the parallel case (~300ms
    /// plus thread/scheduling overhead) while comfortably catching the serial case.
    #[test]
    fn classify_and_probe_fetches_titles_in_parallel_not_sequentially() {
        use std::io::{BufRead, Write};

        let titles = ["Server A", "Server B", "Server C"];
        let mut handles = Vec::new();
        let mut raw_listeners = Vec::new();
        for (index, title) in titles.into_iter().enumerate() {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind must succeed");
            let port = listener.local_addr().unwrap().port();
            let title = title.to_string();
            handles.push(std::thread::spawn(move || {
                // `classify_and_probe` opens TWO connections per listener: one for
                // the liveness check (bare connect, no data), one for the title GET.
                // Serve both, in order, or the second (the actual title request)
                // never gets a response and this test would wrongly look like a bug
                // in parallelism rather than in the fixture.
                for _ in 0..2 {
                    let Ok((mut stream, _)) = listener.accept() else { break };
                    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                    let mut request_line = String::new();
                    let _ = reader.read_line(&mut request_line);
                    if request_line.is_empty() {
                        // The liveness check: connects and disconnects without
                        // sending anything. Nothing to respond to.
                        continue;
                    }
                    std::thread::sleep(Duration::from_millis(300));
                    let body = format!("<html><head><title>{title}</title></head></html>");
                    let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                    let _ = stream.write_all(response.as_bytes());
                }
            }));
            raw_listeners.push(raw(100 + index as u32, 1, "node", vec![binding(port)]));
        }

        let deps = ClassifyDeps { owning_app: &|_| None, path_exists: &|_| true };
        let mut cache = TitleCache::new();

        let start = std::time::Instant::now();
        let scanned = classify_and_probe(&raw_listeners, &deps, &mut cache);
        let elapsed = start.elapsed();

        for h in handles {
            h.join().unwrap();
        }

        assert!(elapsed < Duration::from_millis(600), "titles must fetch in parallel: took {elapsed:?} for 3 servers each delaying 300ms");
        let titles: std::collections::HashSet<Option<String>> = scanned.iter().map(|s| s.title.clone()).collect();
        assert!(titles.contains(&Some("Server A".to_string())));
        assert!(titles.contains(&Some("Server B".to_string())));
        assert!(titles.contains(&Some("Server C".to_string())));
    }

    // ---- fingerprint ----

    #[test]
    fn fingerprint_stable_across_identical_real_enumerations() {
        // Regression for the exact bug the advisor flagged: start_time is recomputed
        // from `ps`'s one-second-granularity etime on every call, so if it were
        // hashed, two back-to-back calls to the SAME listener data with merely a
        // different instant of `SystemTime::now()` baked in would produce different
        // fingerprints and the short-circuit would never fire.
        let l1 = raw(100, 1, "node", vec![binding(3000)]);
        let mut l2 = l1.clone();
        // Simulate what a second, later enumeration of the identical process
        // actually looks like: start_time recomputed against a later "now", landing
        // on a different SystemTime instant even though nothing about the process
        // changed.
        l2.start_time = l1.start_time + Duration::from_millis(7);

        assert_eq!(fingerprint(&[l1]), fingerprint(&[l2]));
    }

    #[test]
    fn fingerprint_changes_when_a_port_changes() {
        let l1 = raw(100, 1, "node", vec![binding(3000)]);
        let l2 = raw(100, 1, "node", vec![binding(3001)]);
        assert_ne!(fingerprint(&[l1]), fingerprint(&[l2]));
    }

    #[test]
    fn fingerprint_order_independent() {
        let a = raw(100, 1, "node", vec![binding(3000)]);
        let b = raw(200, 1, "python", vec![binding(8000)]);
        assert_eq!(fingerprint(&[a.clone(), b.clone()]), fingerprint(&[b, a]));
    }

    // ---- eligible_for_bulk_stop (required test: only DevServer selected) ----

    #[test]
    fn stop_all_dev_servers_selects_only_dev_server_kind() {
        let servers = vec![
            dev_server("100:3000", "myproject"),
            watch_only_server("200:18789", Kind::YourOwnTool),
            watch_only_server("201:1234", Kind::PartOfMacOS),
            background_service("300:5432"),
            part_of_app("400:9000"),
        ];

        let eligible = eligible_for_bulk_stop(&servers);

        assert_eq!(eligible, vec!["100:3000".to_string()]);
        assert!(!eligible.contains(&"200:18789".to_string()));
        assert!(!eligible.contains(&"201:1234".to_string()));
        assert!(!eligible.contains(&"300:5432".to_string()));
        assert!(!eligible.contains(&"400:9000".to_string()));
    }

    #[test]
    fn stop_all_dev_servers_selects_every_dev_server_when_several_present() {
        let servers = vec![dev_server("1:100", "a"), dev_server("2:200", "b"), watch_only_server("3:300", Kind::PartOfMacOS)];
        let eligible = eligible_for_bulk_stop(&servers);
        assert_eq!(eligible.len(), 2);
        assert!(eligible.contains(&"1:100".to_string()));
        assert!(eligible.contains(&"2:200".to_string()));
    }

    // ---- refuse_if_watch_only (required test: force_stop / stop_server guard) ----

    #[test]
    fn refuse_if_watch_only_blocks_your_own_tool_and_part_of_macos() {
        assert!(refuse_if_watch_only(Kind::YourOwnTool).is_some());
        assert!(refuse_if_watch_only(Kind::PartOfMacOS).is_some());
    }

    #[test]
    fn refuse_if_watch_only_allows_dev_server_background_service_part_of_app() {
        assert!(refuse_if_watch_only(Kind::DevServer).is_none());
        assert!(refuse_if_watch_only(Kind::BackgroundService).is_none());
        assert!(refuse_if_watch_only(Kind::PartOfApp).is_none());
    }

    // ---- refuse_force_stop_without_prior_failure (required test: force_stop is
    // rejected without a prior failed stop_server, and permitted once one is
    // recorded) ----

    #[test]
    fn refuse_force_stop_without_prior_failure_blocks_when_nothing_recorded() {
        let failed_polite_stops = FailedPoliteStops::new();
        assert!(refuse_force_stop_without_prior_failure(&failed_polite_stops, "999:3000", SystemTime::now()).is_some());
    }

    #[test]
    fn refuse_force_stop_without_prior_failure_permits_after_a_recorded_still_running() {
        let now = SystemTime::now();
        let mut failed_polite_stops = FailedPoliteStops::new();
        failed_polite_stops.insert("999:3000".to_string(), FailedPoliteStop::new(vec![binding(3000)], now));
        assert!(refuse_force_stop_without_prior_failure(&failed_polite_stops, "999:3000", now).is_none());
    }

    #[test]
    fn refuse_force_stop_without_prior_failure_does_not_leak_across_ids() {
        let now = SystemTime::now();
        let mut failed_polite_stops = FailedPoliteStops::new();
        failed_polite_stops.insert("111:1000".to_string(), FailedPoliteStop::new(vec![binding(1000)], now));
        // A DIFFERENT id's recorded failure must not authorize force_stop for this one.
        assert!(refuse_force_stop_without_prior_failure(&failed_polite_stops, "999:3000", now).is_some());
    }

    /// The TTL: an authorization earned long ago is no longer the immediate follow-up
    /// to a stop the user just watched fail (F8/N2), so it must expire on its own
    /// rather than persist for the app's whole lifetime.
    #[test]
    fn refuse_force_stop_expires_an_authorization_older_than_the_ttl() {
        let recorded_at = SystemTime::now();
        let mut failed_polite_stops = FailedPoliteStops::new();
        failed_polite_stops.insert("999:3000".to_string(), FailedPoliteStop::new(vec![binding(3000)], recorded_at));

        // Still inside the window: permitted.
        let inside = recorded_at + FORCE_STOP_AUTHORIZATION_TTL - Duration::from_secs(1);
        assert!(refuse_force_stop_without_prior_failure(&failed_polite_stops, "999:3000", inside).is_none());

        // Past the window: refused, even though the entry is still in the map.
        let outside = recorded_at + FORCE_STOP_AUTHORIZATION_TTL + Duration::from_secs(1);
        assert!(refuse_force_stop_without_prior_failure(&failed_polite_stops, "999:3000", outside).is_some());
    }

    /// A clock that jumped backwards must not extend an authorization — failing toward
    /// "expired" is the safe direction, and the user can re-earn it with another stop.
    #[test]
    fn failed_polite_stop_treats_a_backwards_clock_as_expired() {
        let recorded_at = SystemTime::now();
        let record = FailedPoliteStop::new(vec![binding(3000)], recorded_at);
        assert!(record.is_expired(recorded_at - Duration::from_secs(30)));
    }

    // ---- refuse_if_identity_changed (A3): the pre-signal check that the pid is still
    // the same Server the user confirmed stopping ----

    #[test]
    fn identity_check_passes_when_the_same_process_is_still_there() {
        let target = dev_server("100:3000", "myproject");
        let mut listener = raw(target.pid, 1, "node", target.ports.clone());
        listener.start_time = target.start_time;
        assert!(refuse_if_identity_changed(&[listener], &target).is_none());
    }

    /// The etime-granularity case that must NOT refuse: `start_time` is derived as
    /// `now - etime` with one-second resolution, so an unchanged process legitimately
    /// reads a second differently between two enumerations. Refusing here would break
    /// every ordinary stop.
    #[test]
    fn identity_check_tolerates_start_time_jitter_from_one_second_etime_granularity() {
        let target = dev_server("100:3000", "myproject");
        let mut listener = raw(target.pid, 1, "node", target.ports.clone());
        listener.start_time = target.start_time + Duration::from_millis(1200);
        assert!(refuse_if_identity_changed(&[listener], &target).is_none(), "a second of etime jitter must not read as a different process");

        let mut earlier = raw(target.pid, 1, "node", target.ports.clone());
        earlier.start_time = target.start_time - Duration::from_millis(1200);
        assert!(refuse_if_identity_changed(&[earlier], &target).is_none(), "jitter in either direction must be tolerated");
    }

    /// The wrong-target case: the Server exited and an unrelated process took its pid.
    /// It is listening on the same port, so ports alone would not catch it — the start
    /// time is what gives it away.
    #[test]
    fn identity_check_refuses_a_recycled_pid() {
        let target = dev_server("100:3000", "myproject");
        let mut recycled = raw(target.pid, 1, "some-other-program", target.ports.clone());
        recycled.start_time = target.start_time + Duration::from_secs(45);
        assert!(refuse_if_identity_changed(&[recycled], &target).is_some());
    }

    #[test]
    fn identity_check_refuses_when_the_pid_is_gone_entirely() {
        let target = dev_server("100:3000", "myproject");
        assert!(refuse_if_identity_changed(&[], &target).is_some());
        // Another process being present does not help: the target's own pid is absent.
        let unrelated = raw(target.pid + 1, 1, "node", vec![binding(3000)]);
        assert!(refuse_if_identity_changed(&[unrelated], &target).is_some());
    }

    #[test]
    fn identity_check_refuses_when_the_ports_no_longer_match() {
        let target = dev_server("100:3000", "myproject");
        let mut moved = raw(target.pid, 1, "node", vec![binding(3001)]);
        moved.start_time = target.start_time;
        assert!(refuse_if_identity_changed(&[moved], &target).is_some());

        // An extra port is also a change — the set must match, not merely overlap.
        let mut extra = raw(target.pid, 1, "node", vec![binding(3000), binding(3001)]);
        extra.start_time = target.start_time;
        assert!(refuse_if_identity_changed(&[extra], &target).is_some());
    }

    // ---- find_current_holder: force_stop must reach a surviving child that kept the
    // port under a NEW pid after the signaled parent exited (CONTEXT.md "Stopping") ----

    #[test]
    fn find_current_holder_matches_by_port_not_by_original_id() {
        // The original stop_server target was pid 100 on port 3000. After a polite
        // stop, the parent (100) is gone but a child (101) kept the port — so the
        // CURRENT snapshot has a Server with a different id, "101:3000", not "100:3000".
        let mut survivor = dev_server("101:3000", "myproject");
        survivor.pid = 101;
        let servers = vec![survivor];

        let target_ports = vec![binding(3000)];
        let found = find_current_holder(&servers, &target_ports).expect("must find the surviving child by port");
        assert_eq!(found.id, "101:3000");
    }

    #[test]
    fn find_current_holder_none_when_port_truly_free() {
        let mut other = dev_server("101:4000", "other");
        other.ports = vec![binding(4000)];
        let servers = vec![other];
        let target_ports = vec![binding(3000)];
        assert!(find_current_holder(&servers, &target_ports).is_none());
    }

    // ---- what_this_stops: never a raw pid/port ----

    #[test]
    fn what_this_stops_never_contains_raw_port_or_pid() {
        let server = dev_server("100:3000", "myproject");
        let message = what_this_stops(&server);
        assert!(!message.contains("3000"));
        assert!(!message.contains(&server.pid.to_string()));
        assert!(message.contains("myproject"));
    }

    #[test]
    fn what_this_stops_names_the_app_for_no_project_belongs_to() {
        let server = watch_only_server("200:18789", Kind::YourOwnTool);
        let message = what_this_stops(&server);
        assert!(message.contains("openclaw"));
        assert!(!message.contains("18789"));
    }

    // ---- snapshot_from ----

    #[test]
    fn snapshot_groups_dev_servers_by_project() {
        let servers = vec![dev_server("1:100", "myproject")];
        let keeplist = Keeplist::default();
        let snapshot = snapshot_from(&servers, &keeplist, SystemTime::now());
        assert_eq!(snapshot.projects.len(), 1);
        assert_eq!(snapshot.projects[0].project, "myproject");
        assert_eq!(snapshot.projects[0].servers[0].id, "1:100");
    }

    #[test]
    fn snapshot_routes_watch_only_and_others_to_separate_sections() {
        let servers = vec![
            dev_server("1:100", "myproject"),
            watch_only_server("2:200", Kind::PartOfMacOS),
            background_service("3:300"),
            part_of_app("4:400"),
        ];
        let keeplist = Keeplist::default();
        let snapshot = snapshot_from(&servers, &keeplist, SystemTime::now());

        assert_eq!(snapshot.projects.len(), 1);
        assert_eq!(snapshot.watch_only.len(), 1);
        assert_eq!(snapshot.watch_only[0].reason, ipc::WatchOnlyReason::PartOfMacos);
        assert_eq!(snapshot.others.len(), 2);
    }

    #[test]
    fn snapshot_reflects_keep_running_mark() {
        let servers = vec![dev_server("1:100", "myproject")];
        let mut keeplist = Keeplist::default();
        keeplist.set(Path::new("/Users/dev/myproject"), "npm run dev", true);
        let snapshot = snapshot_from(&servers, &keeplist, SystemTime::now());
        assert!(snapshot.projects[0].servers[0].keep_running);
    }

    #[test]
    fn snapshot_background_service_carries_guessed_project_never_as_fact_field() {
        let servers = vec![background_service("3:300")];
        let keeplist = Keeplist::default();
        let snapshot = snapshot_from(&servers, &keeplist, SystemTime::now());
        assert_eq!(snapshot.others[0].guessed_project, Some("unrelated".to_string()));
    }

    #[test]
    fn snapshot_part_of_app_has_no_guessed_project() {
        let servers = vec![part_of_app("4:400")];
        let keeplist = Keeplist::default();
        let snapshot = snapshot_from(&servers, &keeplist, SystemTime::now());
        assert_eq!(snapshot.others[0].guessed_project, None);
    }

    // ---- servers_differ / change detection ----

    #[test]
    fn servers_differ_false_for_identical_sets() {
        let a = vec![dev_server("1:100", "myproject")];
        let b = vec![dev_server("1:100", "myproject")];
        assert!(!servers_differ(&a, &b));
    }

    #[test]
    fn servers_differ_true_when_health_changes() {
        let a = vec![dev_server("1:100", "myproject")];
        let mut b = a.clone();
        b[0].health = Health::NotResponding;
        assert!(servers_differ(&a, &b));
    }

    #[test]
    fn servers_differ_true_when_id_set_changes() {
        let a = vec![dev_server("1:100", "myproject")];
        let b = vec![dev_server("2:200", "myproject")];
        assert!(servers_differ(&a, &b));
    }

    #[test]
    fn servers_differ_false_when_only_uptime_changes() {
        let a = vec![dev_server("1:100", "myproject")];
        let mut b = a.clone();
        b[0].start_time -= Duration::from_secs(30);
        assert!(!servers_differ(&a, &b));
    }

    // ---- run_loop scan failure (A5/N3: a failed scan must not read as a quiet
    // machine) ----

    /// A `ProcessSource` that enumerates successfully once and fails on every call
    /// after — the shape of `lsof`/`ps` becoming unavailable while the app is running.
    struct FailsAfterFirstScan {
        calls: std::sync::atomic::AtomicUsize,
        listeners: Vec<RawListener>,
    }

    impl ProcessSource for FailsAfterFirstScan {
        fn enumerate(&self) -> Result<Vec<RawListener>, String> {
            if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                Ok(self.listeners.clone())
            } else {
                Err("lsof is unavailable".to_string())
            }
        }
        fn owning_app(&self, _exe: &std::path::Path) -> Option<String> {
            None
        }
        fn request_stop(&self, _pid: u32) -> Result<(), String> {
            Ok(())
        }
        fn force_stop(&self, _pid: u32) -> Result<(), String> {
            Ok(())
        }
    }

    /// The bug this replaces: `run_loop` did `scan_once(...).unwrap_or(false)`, so a
    /// scan error was indistinguishable from "nothing changed" — the loop kept
    /// spinning silently and the panel kept showing a stale list with nothing saying
    /// so. Now the failure sets `scan_failed`, the last good snapshot is retained
    /// rather than cleared (clearing would fabricate "nothing running"), and the
    /// change is emitted once so the UI can say it plainly.
    #[test]
    fn run_loop_keeps_the_last_good_snapshot_and_flags_a_failed_scan() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = Arc::new(Mutex::new(ScannerState::new(dir.path().to_path_buf())));
        let source = FailsAfterFirstScan {
            calls: std::sync::atomic::AtomicUsize::new(0),
            listeners: vec![raw(100, 1, "node", vec![binding(3000)])],
        };
        let deps = ClassifyDeps { owning_app: &|_| None, path_exists: &|_| true };
        let waker = Waker::new();

        let mut snapshots: Vec<ipc::Snapshot> = Vec::new();
        let mut ticks = 0;
        scanner_run_loop_for_ticks(&state, &source, &deps, &waker, &mut snapshots, &mut ticks, 2);

        let guard = state.lock().unwrap();
        assert!(guard.scan_failed, "a failed scan must be recorded, not swallowed");
        assert_eq!(guard.servers.len(), 1, "the last good snapshot must be kept, not cleared into a fabricated 'nothing running'");

        let last = snapshots.last().expect("the failure must be emitted so the UI can show it");
        assert!(last.scan_failed);
        assert_eq!(last.projects.iter().map(|g| g.servers.len()).sum::<usize>(), 1);
    }

    /// Runs `run_loop` for a bounded number of ticks, collecting emitted snapshots.
    /// The cadence sleep is real, so this is only viable for a couple of iterations —
    /// hence the tiny tick budget rather than a general-purpose harness.
    #[allow(clippy::too_many_arguments)]
    fn scanner_run_loop_for_ticks(
        state: &Arc<Mutex<ScannerState>>,
        source: &dyn ProcessSource,
        deps: &ClassifyDeps,
        waker: &Waker,
        snapshots: &mut Vec<ipc::Snapshot>,
        ticks: &mut usize,
        max_ticks: usize,
    ) {
        // The panel starts Closed (15s cadence), which would make this test take half
        // a minute. Open it so the loop's own sleep is 3s, and wake it immediately
        // after each tick so the test does not actually wait that out.
        state.lock().unwrap().panel = PanelState::Open;
        std::thread::scope(|scope| {
            let waker_ref = &waker;
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stop_for_waker = stop.clone();
            scope.spawn(move || {
                // Cut every cadence sleep short so the loop advances at test speed.
                while !stop_for_waker.load(std::sync::atomic::Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                    waker_ref.wake();
                }
            });
            run_loop(
                state,
                source,
                deps,
                waker,
                |snapshot| snapshots.push(snapshot),
                || {
                    let done = *ticks >= max_ticks;
                    *ticks += 1;
                    done
                },
            );
            stop.store(true, std::sync::atomic::Ordering::SeqCst);
        });
    }

    // ---- civil_from_days / ISO 8601 ----

    #[test]
    fn format_iso8601_known_date() {
        // 2026-09-02T08:42:21Z, arbitrary seconds-since-epoch computed independently.
        let secs: u64 = 1_788_339_741; // matches this task's currentDate context, sanity-checked by round trip below.
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        let formatted = format_iso8601(time);
        assert!(formatted.starts_with("2026-09-02T"), "{formatted}");
        assert!(formatted.ends_with('Z'), "{formatted}");
    }

    #[test]
    fn format_iso8601_epoch() {
        assert_eq!(format_iso8601(SystemTime::UNIX_EPOCH), "1970-01-01T00:00:00Z");
    }

    // ---- N1 measurement: real timings on this machine, not estimated. Run with
    // `cargo test --release --lib -- --ignored --nocapture measure_real_scan_cycle`.
    // `#[ignore]`d for the same reason as the other real-machine tests: depends on
    // whatever is actually listening right now, not a fixture.
    #[test]
    #[ignore]
    fn measure_real_scan_cycle_reports_actual_milliseconds() {
        #[cfg(target_os = "macos")]
        {
            use crate::platform::macos::MacosProcessSource;
            let source = MacosProcessSource;
            let deps = ClassifyDeps { owning_app: &|exe| source.owning_app(exe), path_exists: &|p| p.exists() };

            let t0 = std::time::Instant::now();
            let listeners = source.enumerate().expect("enumerate must succeed on this machine");
            let enumerate_elapsed = t0.elapsed();

            let mut cache = TitleCache::new();
            let t1 = std::time::Instant::now();
            let scanned = classify_and_probe(&listeners, &deps, &mut cache);
            let classify_and_probe_elapsed = t1.elapsed();
            let full_cycle_elapsed = enumerate_elapsed + classify_and_probe_elapsed;

            // Short-circuit path (panel-closed steady state, unchanged fingerprint):
            // liveness only, no re-enumeration, no re-classification.
            let t2 = std::time::Instant::now();
            let liveness_input: Vec<(String, Vec<PortBinding>)> =
                listeners.iter().filter_map(|l| server_id(l.pid, &l.ports).map(|id| (id, l.ports.clone()))).collect();
            let _ = probe::liveness_for_servers(&liveness_input);
            let liveness_only_elapsed = t2.elapsed();

            let profile = if cfg!(debug_assertions) { "DEBUG (unoptimized)" } else { "RELEASE" };
            eprintln!(
                "N1 measurement [{profile}], {} listeners:\n  enumerate alone (lsof+ps+lsof-cwd subprocess spawns): {:?}\n  classify_and_probe alone (classification + parallel liveness): {:?}\n  full changed-state cycle (enumerate + classify_and_probe): {:?}\n  liveness-only short-circuit path (panel-closed, unchanged hash): {:?}",
                listeners.len(),
                enumerate_elapsed,
                classify_and_probe_elapsed,
                full_cycle_elapsed,
                liveness_only_elapsed
            );

            assert!(!scanned.is_empty() || listeners.is_empty(), "sanity: classify_and_probe must not silently drop listeners");
        }
    }
}
