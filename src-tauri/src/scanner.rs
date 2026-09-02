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

use crate::domain::classify::{self, classify_listener};
use crate::domain::model::{Kind, ProjectAttribution, SelfPids};
use crate::ipc;
use crate::keeplist::Keeplist;
use crate::platform::{PortBinding, ProcessSource, RawListener, ResourceSample};
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
    /// What this Server was using at the latest scan, straight from the platform
    /// (`RawListener.usage`) — the listed process only, never its descendants.
    pub usage: ResourceSample,
    /// Whether that usage has been ELEVATED long enough to be worth a word on the
    /// row. Derived by `PressureHistory`, not read from a single sample — see
    /// `SUSTAIN_FOR`.
    pub pressure: Pressure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Responding,
    NotResponding,
    Unknown,
}

// ---------------------------------------------------------------------------------
// Resource pressure: sustained-elevation state, kept in the scanner.
//
// Purely observational. Nothing in this section feeds a stop decision, a cleanup
// suggestion, or the Keep Running mark — a Server using a lot of CPU is a fact worth
// showing the user, never a reason for the tool to act. That separation is why these
// live beside `Health` rather than anywhere near the stop flow.
// ---------------------------------------------------------------------------------

/// The floor for the CPU threshold: one whole core, in tenths of a percent. A process
/// pinning less than an entire core is not remarkable on any machine, however small.
pub const CPU_ELEVATED_FLOOR_TENTHS_PERCENT: u32 = 1000;

/// The share of the machine's TOTAL logical CPU capacity that counts as elevated once
/// that share exceeds the one-core floor: 15%, in tenths of a percent per CPU.
///
/// A fixed percentage cannot serve both a 4-core laptop and a 16-core desktop. 75% of
/// one core is most of a small machine and a rounding error on a large one, so the
/// threshold scales with what the machine actually has.
pub const CPU_ELEVATED_SHARE_TENTHS_PER_CPU: u32 = 150;

/// The CPU threshold for a machine with `logical_cpus` cores, in the same
/// tenths-of-a-percent-of-one-core unit `ResourceSample` carries.
///
/// `max(one core, 15% of total capacity)`. The floor keeps small machines sane (on 4
/// CPUs, 15% of capacity is 60% of a core — below the floor, so the floor wins); the
/// share keeps large machines meaningful (on 16 CPUs it is 240% of a core).
///
/// A named function rather than a scattered literal precisely because the answer is
/// machine-dependent and must be identical everywhere it is asked.
pub fn cpu_elevated_tenths_percent(logical_cpus: u32) -> u32 {
    // A zero core count is not a machine; treat it as one CPU rather than producing a
    // zero threshold that would badge everything.
    let cpus = logical_cpus.max(1);
    CPU_ELEVATED_FLOOR_TENTHS_PERCENT.max(cpus.saturating_mul(CPU_ELEVATED_SHARE_TENTHS_PER_CPU))
}

/// How many logical CPUs this machine has, for `cpu_elevated_tenths_percent`.
///
/// Read once and cached by `ScannerState`, not per scan: the count cannot change while
/// the process runs, and asking repeatedly would be work in the scan loop's hot path.
/// Falls back to 1 — the conservative direction, since it yields the one-core floor
/// rather than a threshold nothing could ever cross.
pub fn detect_logical_cpus() -> u32 {
    std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1)
}

/// At or above this, resident memory counts as elevated: 1 GiB, in bytes — the unit
/// `ResourceSample` normalises to at the platform boundary. Not machine-scaled: a
/// gigabyte held by one dev server is worth mentioning on any Mac.
pub const MEMORY_ELEVATED_BYTES: u64 = 1024 * 1024 * 1024;

/// How long CPU must stay elevated before it is reported as pressure.
///
/// This is the whole of "a brief CPU spike must not trigger a warning": a dev server
/// compiling, a bundler starting up, or a database answering one heavy query crosses
/// the threshold constantly and means nothing. Thirty seconds is longer than any of
/// those bursts, so what survives it is a process that is genuinely, continuously busy
/// rather than one doing its job.
///
/// Longer than the memory window on purpose. CPU is spiky by nature — the figure is an
/// instantaneous rate — while resident memory moves slowly and a process holding a
/// gigabyte for ten seconds is simply holding a gigabyte.
///
/// Measured in TIME, not in a count of readings, because the scan cadence is not
/// constant — 3s with the panel open, 15s closed, 60s idle. "N readings" would mean
/// 30 seconds in one tier and ten minutes in another.
pub const CPU_SUSTAIN_FOR: Duration = Duration::from_secs(30);

/// How long resident memory must stay elevated before it is reported. See
/// `CPU_SUSTAIN_FOR` for why this one is shorter.
pub const MEMORY_SUSTAIN_FOR: Duration = Duration::from_secs(10);

/// Which resources a Server has been sustainedly heavy on (docs/IPC.md v1.4
/// `ResourceUsage.pressure`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pressure {
    #[default]
    Normal,
    Cpu,
    Memory,
    Both,
}

impl Pressure {
    fn from_flags(cpu: bool, memory: bool) -> Pressure {
        match (cpu, memory) {
            (true, true) => Pressure::Both,
            (true, false) => Pressure::Cpu,
            (false, true) => Pressure::Memory,
            (false, false) => Pressure::Normal,
        }
    }
}

/// One metric's progress toward being called elevated.
///
/// Three states, not two: a reading can be below threshold (nothing pending), above
/// threshold but not yet for long enough (`since` set, not yet elevated), or elevated.
/// A reading below threshold clears both the pending and the elevated state
/// immediately — recovery is reported as fast as it is observed, while onset is
/// deliberately slow.
#[derive(Debug, Clone, Copy, Default)]
struct MetricPressure {
    /// When this metric was first seen above threshold in the current run of
    /// elevated readings. `None` while it is below threshold.
    elevated_since: Option<SystemTime>,
}

impl MetricPressure {
    /// Fold one reading in, and report whether the metric now counts as elevated.
    ///
    /// An ABSENT reading (`None` — the platform could not measure it) is treated the
    /// same as a below-threshold one: it clears the run. An unmeasurable metric must
    /// not silently hold a warning open on evidence that stopped arriving, which is
    /// the same N3 rule that keeps `Health::Unknown` out of `NotResponding`.
    fn observe(&mut self, is_elevated: bool, sustain_for: Duration, now: SystemTime) -> bool {
        if !is_elevated {
            self.elevated_since = None;
            return false;
        }
        let since = *self.elevated_since.get_or_insert(now);
        // A backwards clock yields Err here; treating it as "not yet sustained" keeps
        // the badge from appearing on a clock jump rather than on a measurement.
        now.duration_since(since).map(|held| held >= sustain_for).unwrap_or(false)
    }
}

/// One tracked Server's sustained-elevation state, plus the process identity it was
/// gathered for.
#[derive(Debug, Clone, Copy)]
struct TrackedPressure {
    /// The `start_time` of the process these metrics were observed against. Compared
    /// with `START_TIME_TOLERANCE` on every scan — see `PressureHistory::observe`.
    start_time: SystemTime,
    cpu: MetricPressure,
    memory: MetricPressure,
}

/// Per-Server sustained-elevation state, keyed by `ScannedServer.id` (pid plus first
/// port) AND checked against the process's start time.
///
/// The id alone is not sufficient identity. A pid can be recycled by the OS and the
/// new process can bind the same port, producing the identical id for a genuinely
/// different program — at which point an inherited ten-second run would put a badge on
/// a process that has been alive for one scan. So the start time is carried alongside
/// and compared with `START_TIME_TOLERANCE`, the same tolerance the stop flow's
/// identity gate uses and for the same reason: macos.rs derives `start_time` as
/// `now - etime` with one-second granularity, so an unchanged process legitimately
/// reads a second differently between two enumerations. Comparing exactly would reset
/// the window on almost every scan and no badge would ever appear.
///
/// Entries are also pruned against the live id set on every scan (`retain_ids`), so a
/// Server that disappears takes its history with it. The two mechanisms cover
/// different cases: pruning handles "gone", the start-time check handles "still here
/// under the same id, but not the same process".
#[derive(Debug)]
pub struct PressureHistory {
    by_id: std::collections::HashMap<String, TrackedPressure>,
    /// The CPU threshold for THIS machine, resolved once from its logical CPU count
    /// (see `cpu_elevated_tenths_percent`). Held here rather than recomputed per
    /// comparison so the count is read once for the process's whole life.
    cpu_elevated_tenths_percent: u32,
}

impl Default for PressureHistory {
    fn default() -> Self {
        PressureHistory::new()
    }
}

impl PressureHistory {
    pub fn new() -> Self {
        PressureHistory::for_logical_cpus(detect_logical_cpus())
    }

    /// The machine-independent constructor, so a test can pin a core count instead of
    /// asserting against whatever the machine running the suite happens to have.
    pub fn for_logical_cpus(logical_cpus: u32) -> Self {
        PressureHistory {
            by_id: std::collections::HashMap::new(),
            cpu_elevated_tenths_percent: cpu_elevated_tenths_percent(logical_cpus),
        }
    }

    /// Fold one Server's latest sample in and report its current pressure.
    ///
    /// `start_time` is the process's start time from the current scan. When it differs
    /// from the tracked one by more than `START_TIME_TOLERANCE`, every bit of state for
    /// this id — pending runs and established pressure alike — is discarded before the
    /// new sample is observed, so the new process serves the full window from scratch.
    pub fn observe(&mut self, id: &str, sample: &ResourceSample, start_time: SystemTime, now: SystemTime) -> Pressure {
        let cpu_threshold = self.cpu_elevated_tenths_percent;
        let entry = self.by_id.entry(id.to_string()).or_insert(TrackedPressure {
            start_time,
            cpu: MetricPressure::default(),
            memory: MetricPressure::default(),
        });

        if start_time_drift(entry.start_time, start_time) > START_TIME_TOLERANCE {
            // A different process is behind this id now. Replace the whole entry rather
            // than clearing fields one at a time, so a field added later cannot be
            // forgotten here and silently carry across an identity change.
            *entry = TrackedPressure { start_time, cpu: MetricPressure::default(), memory: MetricPressure::default() };
        }
        // The stored `start_time` is deliberately NOT rebased to the fresh reading when
        // the process is unchanged. Rebasing compares each reading against the previous
        // one, which turns alternating jitter into a hop of up to twice the tolerance
        // (-1.2s then +0.9s reads as 2.1s apart) and resets a window that nothing was
        // wrong with. Keeping the first reading as a fixed anchor means every comparison
        // is against one stable value, and etime jitter — bounded by ps's one-second
        // granularity — can never accumulate past it however long the Server runs.

        let cpu_elevated = sample.cpu_tenths_percent.map(|v| v >= cpu_threshold).unwrap_or(false);
        let memory_elevated = sample.memory_bytes.map(|v| v >= MEMORY_ELEVATED_BYTES).unwrap_or(false);
        Pressure::from_flags(
            entry.cpu.observe(cpu_elevated, CPU_SUSTAIN_FOR, now),
            entry.memory.observe(memory_elevated, MEMORY_SUSTAIN_FOR, now),
        )
    }

    /// Drop history for every id not in `live` — a Server that is gone, or one whose
    /// identity changed underneath the same slot.
    pub fn retain_ids<'a>(&mut self, live: impl IntoIterator<Item = &'a str>) {
        let live: std::collections::HashSet<&str> = live.into_iter().collect();
        self.by_id.retain(|id, _| live.contains(id.as_str()));
    }

    #[cfg(test)]
    fn tracked_ids(&self) -> usize {
        self.by_id.len()
    }
}

/// Apply the current samples to every Server in place and return whether any
/// PRESSURE verdict changed (as opposed to the raw numbers, which change constantly).
///
/// The distinction is the whole of docs/IPC.md v1.4's event split: raw figures ride
/// `resources:changed` and are patched into the existing DOM, while a pressure flip is
/// a structural change to what the row SAYS and goes through `servers:changed`.
fn apply_usage(servers: &mut [ScannedServer], now: SystemTime, history: &mut PressureHistory) -> bool {
    history.retain_ids(servers.iter().map(|s| s.id.as_str()));

    let mut pressure_changed = false;
    for server in servers.iter_mut() {
        let pressure = history.observe(&server.id, &server.usage, server.start_time, now);
        if server.pressure != pressure {
            pressure_changed = true;
        }
        server.pressure = pressure;
    }
    pressure_changed
}

/// Resolve the pids the self-guard protects.
///
/// **Only the DIRECT parent, and only in a debug build.** Both limits are deliberate:
///
/// - Walking further up the tree would reach `npm`, then the user's shell, then their
///   terminal emulator — none of which are Portside, and any of which could legitimately
///   be something the user wants listed. One hop is what "the process that launched this
///   binary" means; anything beyond it is a guess about a tree Portside does not own.
/// - In a release build Portside is launched by `launchd` or Finder, so the parent is a
///   system process that has nothing to do with the app. Guarding it there would
///   silently hide an unrelated listener from the user, which is the opposite of the
///   honesty N3 requires. Release therefore guards the own pid alone, exactly as before.
///
/// "Confidently identified" is the direct-parent pid reported by the OS, filtered to
/// exclude the values that carry no such meaning: 0 (no parent) and 1 (re-parented to
/// launchd — which is precisely what happens when the launching process has already
/// exited, so it identifies nothing).
pub fn self_pids() -> SelfPids {
    SelfPids { own: std::process::id(), dev_parent: dev_parent_pid() }
}

#[cfg(debug_assertions)]
fn dev_parent_pid() -> Option<u32> {
    // SAFETY: `getppid` is a plain libc call taking no arguments and returning an
    // integer — no pointers, no allocation, cannot fail.
    let ppid = unsafe { libc::getppid() } as u32;
    // 0 means no parent; 1 means re-parented to launchd, i.e. whatever launched this
    // process is already gone and the pid identifies nothing about Portside.
    if ppid <= 1 {
        None
    } else {
        Some(ppid)
    }
}

#[cfg(not(debug_assertions))]
fn dev_parent_pid() -> Option<u32> {
    // Release: launched by launchd or Finder, so the parent is an unrelated system
    // process. Guarding it would hide a listener the user is entitled to see.
    None
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

    // Resolved once per scan, not per listener: both pids are fixed for the process's
    // whole life, and `getppid` in the inner loop would be a syscall per listener.
    let self_pids = self_pids();

    let mut pending: Vec<Pending> = Vec::with_capacity(listeners.len());
    for listener in listeners {
        let Some(id) = server_id(listener.pid, &listener.ports) else {
            continue;
        };
        // Portside's own row is labelled by name rather than by whatever bundle or
        // command it happens to be running as — under `tauri dev` the listener is a
        // bare `node` running the Tauri dev host, which tells the user nothing about
        // what they are looking at.
        let belongs_to =
            if self_pids.covers(listener.pid) { Some(classify::SELF_LABEL.to_string()) } else { (deps.owning_app)(&listener.exe_path) };
        let (kind, attribution) = classify_listener(listener, self_pids, deps.owning_app, deps.path_exists);

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
            usage: p.listener.usage,
            // Sustained-elevation state is not derivable from one sample, and this
            // function has no clock or history. `scan_once` folds the sample into
            // `PressureHistory` immediately after, which is the only place `pressure`
            // is ever set to anything but this default.
            pressure: Pressure::Normal,
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

/// Drop one Server's cached title, so the next scan re-fetches it.
///
/// Called when a process's identity is detected to have changed underneath an
/// unchanged (pid, port) — the cache's own key. CONTEXT.md says a Title is "remembered
/// rather than repeated: what a Server is serving does not change while it keeps
/// running", and the premise there is *the same server keeps running*. A replacement
/// process breaks that premise, so the remembered answer is no longer about the thing
/// on screen.
fn forget_title(pid: u32, ports: &[PortBinding], cache: &mut TitleCache) {
    if let Some(first_port) = ports.first().map(|p| p.port) {
        cache.remove(&(pid, first_port));
    }
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
///
/// `usage` is excluded for exactly the same reason and it matters more: CPU and
/// resident memory move on virtually every scan of a live process, so hashing them
/// would make every single tick look structurally changed — the short-circuit would
/// never fire again and `servers:changed` would fire at the full scan cadence forever
/// (docs/IPC.md is explicit that it fires "never on every scan tick"). Raw usage
/// reaches the UI through `resources:changed` instead, and only a sustained PRESSURE
/// verdict — which changes rarely, by construction — counts as a structural change
/// (see `servers_differ`).
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
        // Every displayed kind carries usage (docs/IPC.md v1.4), so the UI can put a
        // placeholder on every row without needing to know which shapes can have
        // figures.
        let usage = ipc::ResourceUsageWire::new(&server.usage, server.pressure);

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
                    usage,
                };
                projects.entry(project_name).or_default().push(wire);
            }
            ipc::WireSection::WatchOnly(reason) => {
                let label = server.belongs_to.clone().unwrap_or_else(|| server.command.clone());
                watch_only.push(ipc::WatchOnlyServer { id: server.id.clone(), label, reason, ports, uptime_seconds, usage });
            }
            ipc::WireSection::Other(kind) => {
                let label = server.belongs_to.clone().unwrap_or_else(|| server.command.clone());
                let guessed_project = ipc::guessed_project_name(&server.attribution);
                others.push(ipc::OtherServer { id: server.id.clone(), label, kind, guessed_project, ports, usage });
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

/// Build the `resources:changed` payload (docs/IPC.md v1.4) from the same Servers
/// `snapshot_from` would use. Every Server appears, in every section, keyed by id —
/// the UI patches by id and does not care which section a row was drawn into.
pub fn resource_samples_from(servers: &[ScannedServer], now: SystemTime) -> ipc::ResourceSamples {
    ipc::ResourceSamples {
        samples: servers
            .iter()
            .map(|s| ipc::ResourceSampleEntry { id: s.id.clone(), usage: ipc::ResourceUsageWire::new(&s.usage, s.pressure) })
            .collect(),
        scanned_at: format_iso8601(now),
    }
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

/// How far apart two derivations of a process's `start_time` are, regardless of which
/// is later. The sign carries no information — etime jitter produces a fresh reading
/// that is earlier just as readily as one that is later — so only the magnitude is
/// compared against `START_TIME_TOLERANCE`. Shared by the stop flow's identity gate
/// and by `PressureHistory`, so "is this still the same process" is one rule with one
/// answer rather than two implementations that could drift apart.
fn start_time_drift(a: SystemTime, b: SystemTime) -> Duration {
    match a.duration_since(b) {
        Ok(d) => d,
        Err(e) => e.duration(),
    }
}

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

    if start_time_drift(current.start_time, target.start_time) > START_TIME_TOLERANCE {
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
    /// Sustained-elevation state per Server id (docs/IPC.md v1.4). Lives here, beside
    /// the scan loop's other cross-tick memory, because "elevated for ten seconds" is
    /// by definition not derivable from the single sample any one scan produces.
    pub pressure_history: PressureHistory,
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
            pressure_history: PressureHistory::new(),
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

/// What one scan cycle found worth telling the UI about (docs/IPC.md v1.4).
///
/// Two independent bits, because the two facts travel on two different events and
/// have opposite frequencies. `structural` is rare and rebuilds the list;
/// `resources` is near-constant and only patches numbers already on screen. Folding
/// them into one bool is exactly the bug this type exists to prevent: raw usage moves
/// on virtually every tick, so a single flag would emit `servers:changed` at the full
/// scan cadence and the UI would rebuild the list out from under the user's hover,
/// open disclosure and scroll position several times a minute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScanOutcome {
    /// The Server set, health, Kind, or sustained resource PRESSURE changed — the UI
    /// must rebuild. Emitted as `servers:changed`.
    pub structural: bool,
    /// Fresh CPU/memory figures are available. Emitted as `resources:changed`, which
    /// the UI applies in place.
    pub resources: bool,
}

/// Run one enumerate-classify-probe cycle against `state`, applying the hash
/// short-circuit (PLAN.md: "Unchanged hash => skip project derivation,
/// classification, and event emission entirely. Only liveness runs.").
///
/// `force_full` bypasses the short-circuit — used by `refresh_now`, which
/// docs/IPC.md says "also refetches titles", implying it always does real work
/// rather than potentially returning a stale liveness-only snapshot.
///
/// Resource samples are gathered on EVERY tick including the short-circuited one:
/// `enumerate()` (and therefore `ps`) has already run by the time the fingerprint is
/// compared, so the figures are in hand and free. The short-circuit skips project
/// derivation and classification, which are the expensive parts — not the reading of
/// data already fetched.
pub fn scan_once(
    state: &mut ScannerState,
    source: &dyn ProcessSource,
    deps: &ClassifyDeps,
    force_full: bool,
) -> Result<ScanOutcome, String> {
    let listeners = source.enumerate()?;
    let fingerprint = fingerprint(&listeners);
    let unchanged = !force_full && state.last_fingerprint == Some(fingerprint);
    let now = SystemTime::now();

    if unchanged {
        let mut changed = false;

        // Identity first, before anything reads or repopulates per-process state.
        //
        // The fingerprint being unchanged says the STRUCTURE is unchanged; it says
        // nothing about usage, which `fingerprint` deliberately excludes. Both fresh
        // fields are copied across by pid — the listener set is identical by definition
        // here, so this cannot mis-attribute a figure to the wrong Server.
        //
        // `start_time` is the load-bearing one: it is the only field distinguishing a
        // Server from a REPLACEMENT process that took the same pid, port, command and
        // path. Everything the fingerprint hashes is identical in that case, so the
        // replacement arrives down THIS branch rather than through full classification.
        // If the stale start time flowed into the pressure identity check, the new
        // process would inherit the dead one's sustain window and be badged on its very
        // first scan.
        let fresh_by_pid: std::collections::HashMap<u32, (ResourceSample, SystemTime)> =
            listeners.iter().map(|l| (l.pid, (l.usage, l.start_time))).collect();
        for server in state.servers.iter_mut() {
            let Some((sample, start_time)) = fresh_by_pid.get(&server.pid) else { continue };
            server.usage = *sample;
            // A drifted start time means a different process is wearing this identity.
            // The title cache keys on (pid, first port) — precisely what a replacement
            // preserves — so a cached title would survive the swap and label the new
            // process with the old one's page title. Dropped on the same signal that
            // resets the pressure window, so one detection clears every piece of state
            // keyed to the process that is gone. This runs BEFORE the liveness loop
            // below, which would otherwise re-read the stale entry straight back out.
            if start_time_drift(server.start_time, *start_time) > START_TIME_TOLERANCE {
                forget_title(server.pid, &server.ports, &mut state.title_cache);
                server.title = None;
                changed = true;
            }
            server.start_time = *start_time;
        }

        // Only liveness runs. Re-probe every current server's ports and update health
        // + (for DevServers) title in place, without re-deriving Kind/Project or
        // touching anything else about the existing ScannedServer list.
        let liveness_input: Vec<(usize, Vec<PortBinding>)> =
            state.servers.iter().enumerate().map(|(i, s)| (i, s.ports.clone())).collect();
        let results = probe::liveness_for_servers(&liveness_input);
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
        let pressure_changed = apply_usage(&mut state.servers, now, &mut state.pressure_history);
        return Ok(ScanOutcome { structural: changed || pressure_changed, resources: true });
    }

    if force_full {
        invalidate_all_titles(&mut state.title_cache);
    }

    let mut new_servers = classify_and_probe(&listeners, deps, &mut state.title_cache);
    // Pressure is folded in BEFORE the comparison, so `servers_differ` can see a
    // pressure flip as the structural change it is.
    apply_usage(&mut new_servers, now, &mut state.pressure_history);
    let changed = servers_differ(&state.servers, &new_servers);
    state.servers = new_servers;
    state.last_fingerprint = Some(fingerprint);
    Ok(ScanOutcome { structural: changed || force_full, resources: true })
}

/// Whether the set of Servers changed in a way the UI needs to know about: a
/// different id set, or any health or sustained-pressure difference for an id both
/// snapshots share. Ignores title/uptime churn on its own (uptime always changes;
/// that alone must not trigger an event on unrelated cycles) — this mirrors
/// `fingerprint`'s "what actually matters" judgment, applied to the classified result
/// instead of the raw one.
///
/// `pressure` belongs here and raw `usage` deliberately does not: a pressure flip
/// changes what the row SAYS (a badge appears or goes), which only a rebuild can
/// render, while the raw figures are text patched into a row that already exists. This
/// is the same distinction `fingerprint` draws, one layer up.
fn servers_differ(old: &[ScannedServer], new: &[ScannedServer]) -> bool {
    if old.len() != new.len() {
        return true;
    }
    let old_by_id: std::collections::HashMap<&str, &ScannedServer> = old.iter().map(|s| (s.id.as_str(), s)).collect();
    for server in new {
        match old_by_id.get(server.id.as_str()) {
            None => return true,
            Some(prev) => {
                if prev.health != server.health || prev.kind != server.kind || prev.pressure != server.pressure {
                    return true;
                }
            }
        }
    }
    false
}

/// The adaptive loop itself. Runs until `should_stop` returns true (used by tests to
/// bound execution; production wiring in `commands.rs`/`lib.rs` never stops it).
///
/// `on_change` is called with a fresh snapshot whenever `scan_once` reports a real
/// structural change — this is where `commands.rs` emits `servers:changed`.
/// `on_resources` is called with the latest CPU/memory figures on every successful
/// scan, and emits `resources:changed` (docs/IPC.md v1.4). The two are separate
/// callbacks for the same reason `ScanOutcome` has two bits: one rebuilds the list,
/// the other must not.
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
    mut on_resources: impl FnMut(ipc::ResourceSamples),
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
            let outcome = match scan_once(&mut guard, source, deps, false) {
                Ok(outcome) => {
                    let recovered = guard.scan_failed;
                    guard.scan_failed = false;
                    // A recovery is itself worth emitting: the UI is showing a "couldn't
                    // scan" note that is no longer true, even if the Server set is
                    // identical to what it was before the failure.
                    ScanOutcome { structural: outcome.structural || recovered, ..outcome }
                }
                Err(e) => {
                    eprintln!("scan failed, keeping the last good snapshot: {e}");
                    let newly_failed = !guard.scan_failed;
                    guard.scan_failed = true;
                    // No `resources` on a failed scan: the last figures are as stale as
                    // the rest of the kept snapshot, and re-emitting them would present
                    // an old reading as a current one (N3).
                    ScanOutcome { structural: newly_failed, resources: false }
                }
            };
            if outcome.structural {
                on_change(guard.snapshot(SystemTime::now()));
            } else if outcome.resources {
                // Only when NOT rebuilding: a snapshot already carries current usage
                // (docs/IPC.md v1.4), so emitting both would make the UI apply the same
                // figures twice — once by rebuild, once by patch.
                on_resources(resource_samples_from(&guard.servers, SystemTime::now()));
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
            usage: ResourceSample::default(),
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
            usage: ResourceSample::default(),
            pressure: Pressure::Normal,
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
            usage: ResourceSample::default(),
            pressure: Pressure::Normal,
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
            usage: ResourceSample::default(),
            pressure: Pressure::Normal,
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
            usage: ResourceSample::default(),
            pressure: Pressure::Normal,
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

    // ---- Resource pressure: the sustained-elevation rule (docs/IPC.md v1.4) ----

    fn sample(cpu_tenths: Option<u32>, memory_bytes: Option<u64>) -> ResourceSample {
        ResourceSample { cpu_tenths_percent: cpu_tenths, memory_bytes }
    }

    /// A fixed process start time for the pressure tests, so the identity dimension is
    /// held constant except where a test deliberately varies it.
    fn started_at() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    /// Ten logical CPUs — a plain Apple Silicon Mac, and the brief's worked example.
    /// Pinned rather than read from the machine so these assertions mean the same thing
    /// on every box the suite runs on.
    const TEST_CPUS: u32 = 10;

    /// The CPU threshold on `TEST_CPUS`: max(100%, 10 x 15%) = 150% of one core.
    fn hot_cpu() -> ResourceSample {
        sample(Some(cpu_elevated_tenths_percent(TEST_CPUS)), None)
    }

    fn history() -> PressureHistory {
        PressureHistory::for_logical_cpus(TEST_CPUS)
    }

    // ---- the adaptive threshold itself ----

    /// The floor and the share, at the sizes real Macs actually come in. A fixed
    /// percentage cannot serve both ends of this range, which is why the rule scales.
    #[test]
    fn cpu_threshold_scales_with_the_machine_but_never_below_one_core() {
        // 4 CPUs: 15% of capacity is 60% of a core, below the floor — floor wins.
        assert_eq!(cpu_elevated_tenths_percent(4), 1000, "4 CPUs -> 100% of one core");
        // 8 CPUs: 15% of capacity is 120%, above the floor — share wins.
        assert_eq!(cpu_elevated_tenths_percent(8), 1200, "8 CPUs -> 120%");
        assert_eq!(cpu_elevated_tenths_percent(10), 1500, "10 CPUs -> 150%");
        assert_eq!(cpu_elevated_tenths_percent(12), 1800, "12 CPUs -> 180%");
    }

    /// A machine reporting no CPUs is not a machine. Treating it as one core yields the
    /// floor; treating it literally would yield a zero threshold and badge everything.
    #[test]
    fn cpu_threshold_never_degrades_to_zero() {
        assert_eq!(cpu_elevated_tenths_percent(0), 1000);
        assert_eq!(cpu_elevated_tenths_percent(1), 1000);
    }

    /// The real machine's count must be usable, whatever it is — the fallback is 1, so
    /// the threshold can never come out below the one-core floor.
    #[test]
    fn detected_cpu_count_produces_a_sane_threshold_on_this_machine() {
        assert!(detect_logical_cpus() >= 1);
        assert!(cpu_elevated_tenths_percent(detect_logical_cpus()) >= CPU_ELEVATED_FLOOR_TENTHS_PERCENT);
    }

    // ---- the sustain windows ----

    /// The whole point of a sustain window: a dev server that spikes on one scan and
    /// settles on the next must never have produced a warning.
    #[test]
    fn a_brief_cpu_spike_never_reports_pressure() {
        let t0 = SystemTime::now();
        let mut history = history();
        let spike = sample(Some(4000), None); // 4 whole cores, far over threshold

        // Well over the threshold, but only for an instant.
        assert_eq!(history.observe("1:3000", &spike, started_at(), t0), Pressure::Normal);
        // Three seconds later (a panel-open tick) it is back to idle.
        assert_eq!(history.observe("1:3000", &sample(Some(20), None), started_at(), t0 + Duration::from_secs(3)), Pressure::Normal);
        // And even a much later spike starts its own window from scratch, rather than
        // resuming the first one.
        assert_eq!(history.observe("1:3000", &spike, started_at(), t0 + Duration::from_secs(60)), Pressure::Normal);
    }

    /// A compile or a bundler start-up pins several cores for many seconds. Twenty of
    /// them must still not be a warning — only continuous busyness past the full window
    /// is.
    #[test]
    fn a_twenty_second_burst_of_work_never_reports_pressure() {
        let t0 = SystemTime::now();
        let mut history = history();
        let hot = hot_cpu();
        history.observe("1:3000", &hot, started_at(), t0);
        assert_eq!(history.observe("1:3000", &hot, started_at(), t0 + Duration::from_secs(20)), Pressure::Normal);
        assert_eq!(history.observe("1:3000", &sample(Some(30), None), started_at(), t0 + Duration::from_secs(23)), Pressure::Normal);
    }

    #[test]
    fn cpu_reports_pressure_only_once_sustained_for_the_full_window() {
        let t0 = SystemTime::now();
        let mut history = history();
        let hot = hot_cpu();

        assert_eq!(history.observe("1:3000", &hot, started_at(), t0), Pressure::Normal);
        // One second short of the window: still not sustained.
        assert_eq!(history.observe("1:3000", &hot, started_at(), t0 + CPU_SUSTAIN_FOR - Duration::from_secs(1)), Pressure::Normal);
        // At the window: reported.
        assert_eq!(history.observe("1:3000", &hot, started_at(), t0 + CPU_SUSTAIN_FOR), Pressure::Cpu);
    }

    /// CPU is spiky and gets the longer window; resident memory moves slowly and gets
    /// the shorter one. The two must not be collapsed into one constant.
    #[test]
    fn cpu_and_memory_windows_are_different_durations() {
        assert_eq!(CPU_SUSTAIN_FOR, Duration::from_secs(30));
        assert_eq!(MEMORY_SUSTAIN_FOR, Duration::from_secs(10));

        let t0 = SystemTime::now();
        let mut history = history();
        let heavy = sample(Some(cpu_elevated_tenths_percent(TEST_CPUS)), Some(2 * MEMORY_ELEVATED_BYTES));

        history.observe("1:3000", &heavy, started_at(), t0);
        // At 10s memory has served its window but CPU has not.
        assert_eq!(history.observe("1:3000", &heavy, started_at(), t0 + MEMORY_SUSTAIN_FOR), Pressure::Memory);
        // At 30s both have.
        assert_eq!(history.observe("1:3000", &heavy, started_at(), t0 + CPU_SUSTAIN_FOR), Pressure::Both);
    }

    /// The threshold is "at or above", so exactly the threshold counts and one tenth
    /// under it does not.
    #[test]
    fn cpu_threshold_boundary_is_inclusive() {
        let t0 = SystemTime::now();
        let later = t0 + CPU_SUSTAIN_FOR;
        let threshold = cpu_elevated_tenths_percent(TEST_CPUS);

        let mut at = history();
        at.observe("a", &sample(Some(threshold), None), started_at(), t0);
        assert_eq!(at.observe("a", &sample(Some(threshold), None), started_at(), later), Pressure::Cpu);

        let mut just_under = history();
        just_under.observe("b", &sample(Some(threshold - 1), None), started_at(), t0);
        assert_eq!(just_under.observe("b", &sample(Some(threshold - 1), None), started_at(), later), Pressure::Normal);
    }

    #[test]
    fn memory_threshold_boundary_is_inclusive_at_one_gibibyte() {
        let t0 = SystemTime::now();
        let later = t0 + MEMORY_SUSTAIN_FOR;

        let mut at = history();
        at.observe("a", &sample(None, Some(MEMORY_ELEVATED_BYTES)), started_at(), t0);
        assert_eq!(at.observe("a", &sample(None, Some(MEMORY_ELEVATED_BYTES)), started_at(), later), Pressure::Memory);

        let mut just_under = history();
        just_under.observe("b", &sample(None, Some(MEMORY_ELEVATED_BYTES - 1)), started_at(), t0);
        assert_eq!(just_under.observe("b", &sample(None, Some(MEMORY_ELEVATED_BYTES - 1)), started_at(), later), Pressure::Normal);
    }

    #[test]
    fn both_metrics_sustained_reports_both() {
        let t0 = SystemTime::now();
        let mut history = history();
        let heavy = sample(Some(4000), Some(2 * MEMORY_ELEVATED_BYTES));
        history.observe("1:3000", &heavy, started_at(), t0);
        assert_eq!(history.observe("1:3000", &heavy, started_at(), t0 + CPU_SUSTAIN_FOR), Pressure::Both);
    }

    /// The two metrics run independent windows: memory sustained while CPU is quiet
    /// must report memory alone, not Both and not Normal.
    #[test]
    fn the_two_metrics_are_tracked_independently() {
        let t0 = SystemTime::now();
        let mut history = history();
        let heavy_memory_idle_cpu = sample(Some(10), Some(2 * MEMORY_ELEVATED_BYTES));
        history.observe("1:3000", &heavy_memory_idle_cpu, started_at(), t0);
        assert_eq!(history.observe("1:3000", &heavy_memory_idle_cpu, started_at(), t0 + CPU_SUSTAIN_FOR), Pressure::Memory);
    }

    /// "A reading below threshold clears the corresponding pending and elevated
    /// state" — recovery is reported as soon as it is seen, with no second window.
    #[test]
    fn a_reading_below_threshold_clears_an_established_pressure_immediately() {
        let t0 = SystemTime::now();
        let mut history = history();
        let hot = sample(Some(4000), None);
        history.observe("1:3000", &hot, started_at(), t0);
        assert_eq!(history.observe("1:3000", &hot, started_at(), t0 + CPU_SUSTAIN_FOR), Pressure::Cpu);

        // One calm reading is enough.
        assert_eq!(history.observe("1:3000", &sample(Some(5), None), started_at(), t0 + CPU_SUSTAIN_FOR + Duration::from_secs(3)), Pressure::Normal);
        // And it truly reset: the next hot reading must serve the full window again.
        assert_eq!(history.observe("1:3000", &hot, started_at(), t0 + CPU_SUSTAIN_FOR + Duration::from_secs(6)), Pressure::Normal);
    }

    /// An unmeasurable metric must not hold a warning open on evidence that stopped
    /// arriving — the same N3 rule that keeps `Health::Unknown` out of NotResponding.
    #[test]
    fn an_absent_reading_clears_pressure_rather_than_sustaining_it() {
        let t0 = SystemTime::now();
        let mut history = history();
        let hot = sample(Some(4000), None);
        history.observe("1:3000", &hot, started_at(), t0);
        assert_eq!(history.observe("1:3000", &hot, started_at(), t0 + CPU_SUSTAIN_FOR), Pressure::Cpu);
        assert_eq!(history.observe("1:3000", &sample(None, None), started_at(), t0 + CPU_SUSTAIN_FOR + Duration::from_secs(3)), Pressure::Normal);
    }

    /// Never-measurable metrics must never accumulate toward a threshold they were
    /// never observed to cross.
    #[test]
    fn absent_readings_alone_never_produce_pressure() {
        let t0 = SystemTime::now();
        let mut history = history();
        history.observe("1:3000", &sample(None, None), started_at(), t0);
        assert_eq!(history.observe("1:3000", &sample(None, None), started_at(), t0 + CPU_SUSTAIN_FOR * 10), Pressure::Normal);
    }

    /// History is keyed by Server id (pid + first port). A restarted server is a
    /// different id, so it must serve its own window rather than inherit the dead
    /// process's run and flash a badge on its first scan.
    #[test]
    fn history_does_not_leak_across_server_identities() {
        let t0 = SystemTime::now();
        let mut history = history();
        let hot = sample(Some(4000), None);
        history.observe("1:3000", &hot, started_at(), t0);
        assert_eq!(history.observe("1:3000", &hot, started_at(), t0 + CPU_SUSTAIN_FOR), Pressure::Cpu);

        // Same port, new pid — a restart.
        assert_eq!(history.observe("2:3000", &hot, started_at(), t0 + CPU_SUSTAIN_FOR), Pressure::Normal);
    }

    /// The recycled-pid case the id alone cannot catch: the OS reuses a pid and the new
    /// process binds the same port, so `server_id` produces the IDENTICAL id for a
    /// different program. Without the start-time check the new process would inherit
    /// the dead one's sustained run and be badged on its very first scan.
    #[test]
    fn history_resets_when_the_process_behind_the_same_id_changed() {
        let t0 = SystemTime::now();
        let mut history = history();
        let hot = sample(Some(4000), None);

        history.observe("1:3000", &hot, started_at(), t0);
        assert_eq!(history.observe("1:3000", &hot, started_at(), t0 + CPU_SUSTAIN_FOR), Pressure::Cpu);

        // Same id, but a process that started 45 seconds later — far outside the
        // tolerance. Established pressure must be discarded, not carried over.
        let recycled = started_at() + Duration::from_secs(45);
        assert_eq!(
            history.observe("1:3000", &hot, recycled, t0 + CPU_SUSTAIN_FOR),
            Pressure::Normal,
            "a different process behind the same id must serve the full window itself"
        );
        // And it genuinely restarted the window rather than merely skipping one reading.
        assert_eq!(history.observe("1:3000", &hot, recycled, t0 + CPU_SUSTAIN_FOR + Duration::from_secs(29)), Pressure::Normal);
        assert_eq!(history.observe("1:3000", &hot, recycled, t0 + CPU_SUSTAIN_FOR * 2), Pressure::Cpu);
    }

    /// The PENDING half of the same rule: an identity change must discard a run that is
    /// partway to the threshold too, not only an established verdict. Otherwise a new
    /// process could inherit twenty-nine of the thirty seconds it never served.
    #[test]
    fn history_resets_a_pending_run_when_identity_changes() {
        let t0 = SystemTime::now();
        let mut history = history();
        let hot = sample(Some(4000), None);

        // Twenty-nine seconds of elevation accrued — pending, not yet reported.
        history.observe("1:3000", &hot, started_at(), t0);
        assert_eq!(history.observe("1:3000", &hot, started_at(), t0 + Duration::from_secs(29)), Pressure::Normal);

        // A different process appears under the same id one second later. If the pending
        // run survived, this reading would cross the thirty-second mark and badge it.
        let recycled = started_at() + Duration::from_secs(45);
        assert_eq!(
            history.observe("1:3000", &hot, recycled, t0 + Duration::from_secs(30)),
            Pressure::Normal,
            "a pending run must not carry across an identity change"
        );
    }

    /// The MEMORY pending window must reset on an identity change too — the reset
    /// replaces the whole entry rather than clearing one metric.
    #[test]
    fn history_resets_a_pending_memory_run_when_identity_changes() {
        let t0 = SystemTime::now();
        let mut history = history();
        let heavy = sample(None, Some(2 * MEMORY_ELEVATED_BYTES));

        history.observe("1:3000", &heavy, started_at(), t0);
        assert_eq!(history.observe("1:3000", &heavy, started_at(), t0 + Duration::from_secs(9)), Pressure::Normal);

        let recycled = started_at() + Duration::from_secs(45);
        assert_eq!(history.observe("1:3000", &heavy, recycled, t0 + MEMORY_SUSTAIN_FOR), Pressure::Normal);
    }

    /// The tolerance that makes the check usable: `start_time` is derived as
    /// `now - etime` with one-second granularity, so an unchanged process reads a
    /// second differently between enumerations. Comparing exactly would reset the
    /// window on almost every scan and no badge would ever appear.
    #[test]
    fn ordinary_etime_jitter_does_not_reset_the_window() {
        let t0 = SystemTime::now();
        let mut history = history();
        let hot = sample(Some(4000), None);

        history.observe("1:3000", &hot, started_at(), t0);
        // Jitter in both directions, each within tolerance, across the whole window.
        history.observe("1:3000", &hot, started_at() + Duration::from_millis(1200), t0 + Duration::from_secs(10));
        history.observe("1:3000", &hot, started_at() - Duration::from_millis(1200), t0 + Duration::from_secs(20));
        assert_eq!(
            history.observe("1:3000", &hot, started_at() + Duration::from_millis(900), t0 + CPU_SUSTAIN_FOR),
            Pressure::Cpu,
            "etime jitter within tolerance must not restart the sustain window"
        );
    }

    /// A Server that disappears takes its history with it, so nothing can be inherited
    /// by whatever id is issued next, and the map cannot grow without bound.
    #[test]
    fn history_is_pruned_when_a_server_disappears() {
        let t0 = SystemTime::now();
        let mut history = history();
        let hot = sample(Some(4000), None);
        history.observe("1:3000", &hot, started_at(), t0);
        history.observe("2:4000", &hot, started_at(), t0);
        assert_eq!(history.tracked_ids(), 2);

        history.retain_ids(["2:4000"]);
        assert_eq!(history.tracked_ids(), 1);

        // The dropped id starts over rather than resuming where it left off.
        assert_eq!(history.observe("1:3000", &hot, started_at(), t0 + CPU_SUSTAIN_FOR), Pressure::Normal);
    }

    // ---- The self guard: Portside must never offer to stop itself ----

    /// Under `tauri dev` Portside's own cwd is the project root, so without rule 0 every
    /// Project-derived rule would classify it as the user's own dev server — visible,
    /// stoppable, and swept up by Stop Everything. This asserts the guard fires for the
    /// REAL current pid, not a stand-in, so it cannot pass while the wiring is wrong.
    #[test]
    fn this_process_is_classified_watch_only() {
        let me = raw(self_pids().own, 1, "portside", vec![binding(1420)]);
        // `path_exists: true` makes the cwd look like a project root, which is exactly
        // the condition that would otherwise produce DevServer.
        let (kind, attribution) = classify_listener(&me, self_pids(), |_: &Path| None, |_: &Path| true);

        assert_eq!(kind, Kind::YourOwnTool);
        assert!(kind.is_watch_only(), "Portside's own row must be Watch Only");
        assert_eq!(attribution, ProjectAttribution::None, "Portside must not claim the project it runs inside");
    }

    /// The guard must not catch anything else. A real dev server with the same shape,
    /// differing only in pid, stays a stoppable DevServer.
    #[test]
    fn another_process_with_the_same_shape_is_still_a_dev_server() {
        let guarded = self_pids();
        // Pick a pid the guard demonstrably does not cover, rather than assuming
        // `own + 1` is free — in dev that could collide with the parent.
        let unrelated = (1..10_000u32).map(|n| guarded.own + n).find(|p| !guarded.covers(*p)).expect("some nearby pid is unguarded");
        let other = raw(unrelated, 1, "node", vec![binding(3000)]);
        let (kind, _) = classify_listener(&other, guarded, |_: &Path| None, |_: &Path| true);
        assert_eq!(kind, Kind::DevServer);
        assert!(!kind.is_watch_only());
    }

    /// **The real `tauri dev` topology.** Observed live: `npm run tauri dev` (51521)
    /// spawns `node` (51539) which HOLDS the port, and that node process spawns the
    /// Portside binary (43241). So the listener the user sees is Portside's PARENT, and
    /// `pid == std::process::id()` is false for it — the guard as originally written
    /// missed the only row that actually appears.
    ///
    /// This models that exact three-level shape and asserts the full contract for the
    /// parent listener: Watch Only, labelled by name, out of bulk stop, refused by both
    /// stop paths — while an unrelated dev server beside it stays fully stoppable.
    #[test]
    fn the_tauri_dev_parent_listener_is_protected_and_labelled() {
        // The listener IS the parent; this process is the child it spawned.
        let dev_host_pid = 51539;
        let guarded = SelfPids { own: 43241, dev_parent: Some(dev_host_pid) };

        // cwd looks like a project root, which is what makes this a DevServer without
        // the guard — the condition that produced the bug.
        let dev_host = raw(dev_host_pid, 51521, "node", vec![binding(1430)]);
        let (kind, attribution) = classify_listener(&dev_host, guarded, |_: &Path| None, |_: &Path| true);
        assert_eq!(kind, Kind::YourOwnTool, "the tauri dev host must not be a stoppable dev server");
        assert!(kind.is_watch_only());
        assert_eq!(attribution, ProjectAttribution::None);

        // An unrelated dev server in the same scan is unaffected.
        let unrelated = raw(70_000, 1, "node", vec![binding(3000)]);
        let (other_kind, _) = classify_listener(&unrelated, guarded, |_: &Path| None, |_: &Path| true);
        assert_eq!(other_kind, Kind::DevServer, "the guard must protect the dev host only, not every listener");

        // Wire shape: labelled by name, in watchOnly, never in projects.
        let mut host_server = watch_only_server("51539:1430", Kind::YourOwnTool);
        host_server.pid = dev_host_pid;
        host_server.belongs_to = Some(classify::SELF_LABEL.to_string());
        let servers = vec![host_server, dev_server("70000:3000", "someone-elses-project")];

        let snapshot = snapshot_from(&servers, &Keeplist::default(), SystemTime::now());
        assert_eq!(snapshot.watch_only.len(), 1);
        assert_eq!(snapshot.watch_only[0].label, "Portside — this app");
        assert_eq!(snapshot.projects.len(), 1, "only the unrelated project is listed as stoppable");
        assert_eq!(snapshot.projects[0].servers[0].id, "70000:3000");

        // Bulk stop reaches the unrelated dev server and not the dev host.
        let eligible = eligible_for_bulk_stop(&servers);
        assert_eq!(eligible, vec!["70000:3000".to_string()]);

        // Both stop paths refuse it — the same guard `stop_server` and `force_stop`
        // each call before signaling anything.
        assert!(refuse_if_watch_only(servers[0].kind).is_some(), "stop_server must refuse the dev host");
        assert!(refuse_if_watch_only(Kind::YourOwnTool).is_some(), "force_stop must refuse it too");
    }

    /// The limits on "confidently identified": only the DIRECT parent, and only when
    /// that pid actually identifies a launching process. 0 (no parent) and 1
    /// (re-parented to launchd, i.e. the launcher already exited) identify nothing, so
    /// they must never be guarded — doing so would hide an unrelated listener.
    #[test]
    fn a_meaningless_parent_pid_is_never_guarded() {
        for ppid in [0, 1] {
            let pids = SelfPids { own: 4242, dev_parent: None };
            assert!(!pids.covers(ppid), "ppid {ppid} identifies no launching process and must stay listed");
        }
        // And a grandparent is not covered either: one hop is the whole rule.
        let pids = SelfPids { own: 43241, dev_parent: Some(51539) };
        assert!(!pids.covers(51521), "the grandparent (npm) is not Portside and must stay listed");
        assert!(pids.covers(51539));
        assert!(pids.covers(43241));
    }

    /// Release builds guard the own pid ALONE. Guarding a parent there would hide a
    /// listener belonging to launchd or Finder, which has nothing to do with the app.
    #[test]
    fn the_parent_guard_is_debug_only() {
        let resolved = self_pids();
        if cfg!(debug_assertions) {
            // The test suite is a debug build, so a real parent should be resolvable.
            assert!(resolved.dev_parent.is_some(), "a debug build must resolve its direct parent");
        } else {
            assert_eq!(resolved.dev_parent, None, "a release build must guard only its own pid");
        }
    }

    /// The guard's whole purpose, at the layer that enforces it: Stop Everything must
    /// not include Portside. `eligible_for_bulk_stop` filters on DevServer, and the
    /// guard is what keeps Portside out of that Kind.
    #[test]
    fn bulk_stop_excludes_this_app_but_still_includes_real_dev_servers() {
        let mut me = watch_only_server("self:1420", Kind::YourOwnTool);
        me.pid = self_pids().own;
        me.belongs_to = Some(classify::SELF_LABEL.to_string());
        let servers = vec![dev_server("100:3000", "myproject"), me];

        let eligible = eligible_for_bulk_stop(&servers);
        assert_eq!(eligible, vec!["100:3000".to_string()]);
        assert!(!eligible.contains(&"self:1420".to_string()));
    }

    /// And the single-stop guard refuses it for the same reason — two independent
    /// guards, the pattern the rest of this file already uses for Watch Only.
    #[test]
    fn stopping_this_app_is_refused() {
        assert!(refuse_if_watch_only(Kind::YourOwnTool).is_some());
    }

    /// The label is the one thing the user reads on that row, so it is asserted
    /// exactly rather than merely "not empty" — under `tauri dev` the fallback would be
    /// a cargo target path, which says nothing.
    #[test]
    fn this_app_is_labelled_by_name_on_the_wire() {
        let mut me = watch_only_server("self:1420", Kind::YourOwnTool);
        me.pid = self_pids().own;
        me.belongs_to = Some(classify::SELF_LABEL.to_string());

        let snapshot = snapshot_from(&[me], &Keeplist::default(), SystemTime::now());
        assert_eq!(snapshot.watch_only.len(), 1);
        assert_eq!(snapshot.watch_only[0].label, "Portside — this app");
        // Watch Only rows carry no stop affordance anywhere in the wire shape.
        assert!(snapshot.projects.is_empty(), "Portside must never appear among stoppable dev servers");
    }

    // ---- pressure vs raw usage: which event a change belongs on ----

    /// The invariant behind docs/IPC.md v1.4's two events: raw CPU/memory moving must
    /// NOT make the structural event fire, or the UI would rebuild the list — losing
    /// hover, open disclosures and scroll position — several times a minute.
    #[test]
    fn raw_usage_changes_alone_are_not_a_structural_change() {
        let a = vec![dev_server("1:100", "myproject")];
        let mut b = a.clone();
        b[0].usage = sample(Some(415), Some(700 * 1024 * 1024));
        assert!(!servers_differ(&a, &b), "raw usage must ride resources:changed, never servers:changed");
    }

    /// A pressure flip, by contrast, changes what the row SAYS — a badge appears — and
    /// only a rebuild can render that.
    #[test]
    fn a_pressure_flip_is_a_structural_change() {
        let a = vec![dev_server("1:100", "myproject")];
        let mut b = a.clone();
        b[0].pressure = Pressure::Cpu;
        assert!(servers_differ(&a, &b));
    }

    /// The same rule one layer down: `fingerprint` must ignore usage, or the hash
    /// short-circuit never fires again and every tick does full classification.
    #[test]
    fn fingerprint_ignores_resource_usage() {
        let idle = raw(100, 1, "node", vec![binding(3000)]);
        let mut busy = idle.clone();
        busy.usage = sample(Some(940), Some(3 * MEMORY_ELEVATED_BYTES));
        assert_eq!(fingerprint(&[idle]), fingerprint(&[busy]));
    }

    // ---- snapshot / samples wire shape ----

    #[test]
    fn snapshot_carries_current_usage_on_every_displayed_kind() {
        let mut servers = vec![
            dev_server("1:100", "myproject"),
            watch_only_server("2:200", Kind::PartOfMacOS),
            background_service("3:300"),
        ];
        for server in servers.iter_mut() {
            server.usage = sample(Some(825), Some(2 * MEMORY_ELEVATED_BYTES));
            server.pressure = Pressure::Both;
        }
        let snapshot = snapshot_from(&servers, &Keeplist::default(), SystemTime::now());

        // First render must have values, without waiting for a resources:changed.
        assert_eq!(snapshot.projects[0].servers[0].usage.cpu_percent, Some(82.5));
        assert_eq!(snapshot.projects[0].servers[0].usage.pressure, ipc::PressureWire::Both);
        assert_eq!(snapshot.watch_only[0].usage.cpu_percent, Some(82.5));
        assert_eq!(snapshot.others[0].usage.memory_bytes, Some(2 * MEMORY_ELEVATED_BYTES));
    }

    #[test]
    fn unavailable_metrics_reach_the_wire_as_null_not_zero() {
        let servers = vec![dev_server("1:100", "myproject")];
        let snapshot = snapshot_from(&servers, &Keeplist::default(), SystemTime::now());
        assert_eq!(snapshot.projects[0].servers[0].usage.cpu_percent, None);
        assert_eq!(snapshot.projects[0].servers[0].usage.memory_bytes, None);
        let json = serde_json::to_string(&snapshot.projects[0].servers[0].usage).unwrap();
        assert!(json.contains("\"cpuPercent\":null"), "{json}");
        assert!(!json.contains("\"cpuPercent\":0"), "{json}");
    }

    #[test]
    fn resource_samples_cover_every_section_keyed_by_id() {
        let servers = vec![
            dev_server("1:100", "myproject"),
            watch_only_server("2:200", Kind::PartOfMacOS),
            background_service("3:300"),
            part_of_app("4:400"),
        ];
        let samples = resource_samples_from(&servers, SystemTime::now());
        let ids: Vec<&str> = samples.samples.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["1:100", "2:200", "3:300", "4:400"]);
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

    // ---- scan_once's two outcome bits (docs/IPC.md v1.4) ----

    /// A `ProcessSource` returning whatever listeners it is currently set to, so a
    /// test can drive two scans with identical structure but different usage.
    struct FixedSource {
        listeners: std::sync::Mutex<Vec<RawListener>>,
    }

    impl ProcessSource for FixedSource {
        fn enumerate(&self) -> Result<Vec<RawListener>, String> {
            Ok(self.listeners.lock().unwrap().clone())
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

    /// The item-9/10 invariant, end to end through `scan_once`: a second scan whose
    /// only difference is CPU and memory must report fresh resources WITHOUT reporting
    /// a structural change — otherwise the UI rebuilds the list every few seconds and
    /// the user loses their hover, open row and scroll position.
    #[test]
    fn a_usage_only_change_reports_resources_but_not_a_structural_change() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut state = ScannerState::new(dir.path().to_path_buf());
        let deps = ClassifyDeps { owning_app: &|_| None, path_exists: &|_| true };

        let mut listener = raw(100, 1, "node", vec![binding(3000)]);
        listener.usage = sample(Some(120), Some(200 * 1024 * 1024));
        let source = FixedSource { listeners: std::sync::Mutex::new(vec![listener.clone()]) };

        // First scan establishes the baseline; it is structural because the Server set
        // went from empty to one.
        let first = scan_once(&mut state, &source, &deps, false).expect("first scan");
        assert!(first.structural);
        assert!(first.resources);

        // Second scan: same pid, ports, command and cwd — only the figures moved. That
        // is the short-circuited path (the fingerprint is unchanged by construction,
        // since `fingerprint` excludes usage).
        listener.usage = sample(Some(430), Some(640 * 1024 * 1024));
        *source.listeners.lock().unwrap() = vec![listener];
        let second = scan_once(&mut state, &source, &deps, false).expect("second scan");

        assert!(!second.structural, "raw usage must not trigger servers:changed");
        assert!(second.resources, "fresh figures must still be offered to the UI");
        // And the fresh figures genuinely reached the stored Servers, so the samples
        // built from them are current rather than the first scan's.
        assert_eq!(state.servers[0].usage.cpu_tenths_percent, Some(430));
    }

    /// **End-to-end regression for the short-circuit branch.** A hot process is replaced
    /// by one with the IDENTICAL pid, port, command and executable path — everything
    /// `fingerprint` hashes — differing only in start time. The fingerprint is therefore
    /// unchanged, so the replacement arrives down the short-circuit branch, which used
    /// to copy fresh usage while leaving the OLD start time in place. The new process
    /// inherited the dead one's sustain window and was badged on its first scan.
    ///
    /// Driven through `scan_once` rather than `PressureHistory` directly, because the
    /// bug was in the plumbing between them: the unit-level identity check was already
    /// correct and passing.
    #[test]
    fn a_replacement_process_behind_an_unchanged_fingerprint_serves_the_full_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut state = ScannerState::new(dir.path().to_path_buf());
        state.pressure_history = PressureHistory::for_logical_cpus(TEST_CPUS);
        let deps = ClassifyDeps { owning_app: &|_| None, path_exists: &|_| true };

        let hot = sample(Some(4000), None);
        let mut listener = raw(70_001, 1, "node", vec![binding(3100)]);
        listener.usage = hot;
        listener.start_time = started_at();
        let source = FixedSource { listeners: std::sync::Mutex::new(vec![listener.clone()]) };

        // Establish sustained CPU pressure on the original process. `scan_once` stamps
        // its own `SystemTime::now()`, so the window is served by repeating the scan
        // with the history's clock advanced through the helper below.
        scan_once(&mut state, &source, &deps, false).expect("first scan");
        let established = state
            .pressure_history
            .observe(&state.servers[0].id, &hot, started_at(), SystemTime::now() + CPU_SUSTAIN_FOR);
        assert_eq!(established, Pressure::Cpu, "precondition: the original process is badged");

        // The replacement: same pid, same port, same command, same exe path — only the
        // start time moves, by 45 seconds.
        let replaced_start = started_at() + Duration::from_secs(45);
        listener.start_time = replaced_start;
        *source.listeners.lock().unwrap() = vec![listener];

        let before_fingerprint = state.last_fingerprint;
        scan_once(&mut state, &source, &deps, false).expect("second scan");
        assert_eq!(state.last_fingerprint, before_fingerprint, "precondition: this must be the SHORT-CIRCUIT branch");

        // The fresh start time reached the stored Server...
        assert_eq!(state.servers[0].start_time, replaced_start, "the short circuit must carry the fresh start_time");
        // ...and the replacement is back to normal rather than inheriting the badge.
        assert_eq!(state.servers[0].pressure, Pressure::Normal, "a replacement process must not inherit the previous one's pressure");

        // And it truly serves the FULL window from scratch: still normal one second
        // short of it, badged only at it.
        let id = state.servers[0].id.clone();
        let restart = SystemTime::now();
        state.pressure_history.observe(&id, &hot, replaced_start, restart);
        assert_eq!(
            state.pressure_history.observe(&id, &hot, replaced_start, restart + CPU_SUSTAIN_FOR - Duration::from_secs(1)),
            Pressure::Normal,
            "the replacement must not be badged before its own full window elapses"
        );
        assert_eq!(
            state.pressure_history.observe(&id, &hot, replaced_start, restart + CPU_SUSTAIN_FOR),
            Pressure::Cpu,
            "and it must badge once it has served that window itself"
        );
    }

    /// The title cache keys on `(pid, first_port)` — exactly what a replacement process
    /// preserves — so a cached title survived the swap and labelled the new process with
    /// the dead one's page title. Dropped on the same identity-drift signal that resets
    /// the pressure window, so one detection clears every piece of state keyed to the
    /// process that is gone.
    /// A REAL listener backs this fixture, and that is essential rather than incidental:
    /// with nothing listening, liveness reports NotResponding and `title_cache_lookup`
    /// drops the title on its own, so the test would pass with the fix removed and prove
    /// nothing. Verified by deleting the invalidation and watching this test go red.
    #[test]
    fn a_replacement_process_does_not_inherit_the_previous_title() {
        // Accept connections for the whole test so every scan sees Responding, but
        // answer no HTTP — a title, once dropped, therefore cannot be silently
        // re-fetched, and an empty cache at the end is unambiguous evidence of the drop.
        let socket = std::net::TcpListener::bind("127.0.0.1:0").expect("bind must succeed");
        let port = socket.local_addr().unwrap().port();
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = done.clone();
        let accepter = std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                match socket.accept() {
                    Ok(_) => {} // Connection accepted and immediately dropped.
                    Err(_) => break,
                }
            }
        });

        let dir = tempfile::tempdir().expect("tempdir");
        let mut state = ScannerState::new(dir.path().to_path_buf());
        let deps = ClassifyDeps { owning_app: &|_| None, path_exists: &|_| true };

        let mut listener = raw(70_002, 1, "node", vec![binding(port)]);
        listener.start_time = started_at();
        let source = FixedSource { listeners: std::sync::Mutex::new(vec![listener.clone()]) };
        scan_once(&mut state, &source, &deps, false).expect("first scan");

        // Plant a title as though the original process had been probed successfully.
        let key = (listener.pid, port);
        state.title_cache.insert(key, Some("Old Project — Dev".to_string()));
        state.servers[0].title = Some("Old Project — Dev".to_string());

        // Sanity: an ordinary scan of the UNCHANGED process keeps that title, so the
        // assertion below is about the replacement and not about scanning at all.
        scan_once(&mut state, &source, &deps, false).expect("unchanged scan");
        assert_eq!(state.servers[0].health, Health::Responding, "fixture must be Responding, or the NotResponding path clears the cache for us");
        assert_eq!(state.servers[0].title.as_deref(), Some("Old Project — Dev"), "an unchanged process keeps its cached title");

        // Now replace the process behind the identical pid and port.
        listener.start_time = started_at() + Duration::from_secs(45);
        *source.listeners.lock().unwrap() = vec![listener];
        scan_once(&mut state, &source, &deps, false).expect("replacement scan");

        // The old title is gone. The entry is now `Some(None)` rather than absent
        // because the same scan's liveness pass re-probed the port for the NEW process
        // and cached "attempted, found no title" — which is the point: the title was
        // re-derived for the replacement, not inherited from its predecessor.
        assert_eq!(
            state.title_cache.get(&key),
            Some(&None),
            "the replacement's title must be re-derived, never the previous process's cached value"
        );
        assert_eq!(state.servers[0].title, None, "the replacement must not be shown the previous process's title");
        assert_eq!(state.servers[0].health, Health::Responding, "and it is still Responding, so this was not the NotResponding path clearing it");

        done.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(("127.0.0.1", port)); // unblock accept()
        let _ = accepter.join();
    }

    /// The other half of that rule, isolated to the identity check itself: ordinary
    /// etime jitter must NOT be treated as a replacement.
    ///
    /// Asserted against `start_time_drift` rather than through `scan_once`, deliberately.
    /// A `scan_once` fixture cannot isolate this: with nothing actually listening on the
    /// fixture's port, liveness reports NotResponding and `title_cache_lookup` already
    /// drops the title on its own (CONTEXT.md: "Renewed when a Server stops Responding")
    /// — so the cache would end up empty either way and the test would pass without
    /// proving anything about the new code. This asserts the one condition the new
    /// invalidation is actually gated on.
    #[test]
    fn ordinary_etime_jitter_is_not_treated_as_a_replacement() {
        let anchor = started_at();
        for jitter in [Duration::from_millis(1200), Duration::from_millis(1900), Duration::ZERO] {
            assert!(
                start_time_drift(anchor, anchor + jitter) <= START_TIME_TOLERANCE,
                "jitter of {jitter:?} must not read as a replacement and force a title re-fetch every tick"
            );
            assert!(start_time_drift(anchor, anchor - jitter) <= START_TIME_TOLERANCE, "jitter is sign-independent");
        }
        // A genuine replacement is well outside it.
        assert!(start_time_drift(anchor, anchor + Duration::from_secs(45)) > START_TIME_TOLERANCE);
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
                |_samples| {},
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
