//! The seam between "what the OS tells us" and "what it means" (see docs/PLAN.md).
//!
//! `ProcessSource` is the one trait every platform implements. Everything above this
//! module — domain/ — depends only on `RawListener` and `PortBinding`, never on how
//! a platform gathered them.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[cfg(target_os = "macos")]
pub mod macos;

/// Whether a bound address accepts connections from this machine only, or from the
/// whole network. Mirrors CONTEXT.md's "Reachable From".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reachability {
    LocalhostOnly,
    AllInterfaces,
}

/// Which IP address family a binding was made on. A single process commonly binds
/// the identical port on both families at once (observed on this machine: openclaw's
/// 18789, ControlCenter's 7000/5000, OrbStack's 5432/5433 each appear twice, once per
/// family) — those are two genuinely distinct sockets, not a duplicate to collapse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressFamily {
    V4,
    V6,
}

/// One (address family, port) pair a listener is bound to (CONTEXT.md "Port
/// Binding"). A single process commonly binds the same port on both IPv4 and IPv6,
/// or several different ports — each is its own binding, never deduplicated by port
/// number alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortBinding {
    pub port: u16,
    pub family: AddressFamily,
    pub reachability: Reachability,
}

/// What one process was consuming at the moment it was scanned.
///
/// Both fields are `Option` because resource figures are *optional enrichment*: a
/// figure the platform could not supply or could not parse makes that one metric
/// unavailable, and never makes the listener itself disappear or the scan fail. N3
/// again — "unavailable" and "zero" are different claims and must stay distinguishable.
///
/// **Measured for the listed process only.** No descendant or process-group
/// aggregation: a process group can contain entirely unrelated processes (the same
/// finding that narrowed `macos::signal_target` to the bare pid), so summing over one
/// would attribute another program's usage to this Server.
///
/// `cpu_tenths_percent` is an integer (tenths of a percent, so `825` == 82.5%) rather
/// than a float: every domain struct downstream derives `Eq`, and it is compared
/// against thresholds and for change detection, neither of which a float supports
/// honestly. It is NOT clamped to 1000 — `ps pcpu` legitimately exceeds 100% on a
/// multi-core machine for a multi-threaded process, and clamping would under-report a
/// real fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResourceSample {
    /// Tenths of a percent of one CPU: `825` == 82.5%. `None` when unavailable.
    pub cpu_tenths_percent: Option<u32>,
    /// Resident set size in BYTES. `None` when unavailable. Platforms report this in
    /// varying units (macOS `ps rss` is KiB) — the conversion happens at the platform
    /// boundary so everything above this line deals in bytes only.
    pub memory_bytes: Option<u64>,
}

/// A neutral description of one listening process, exactly as required by P1: no
/// platform-specific fields, nothing domain logic couldn't have on any OS.
///
/// `cwd` is `Option` because Windows often cannot supply it for another process —
/// every consumer of this struct must handle `None` rather than assume it is present.
#[derive(Debug, Clone)]
pub struct RawListener {
    pub pid: u32,
    pub ppid: u32,
    pub command: String,
    pub exe_path: PathBuf,
    pub cwd: Option<PathBuf>,
    pub ports: Vec<PortBinding>,
    pub start_time: SystemTime,
    pub user: String,
    /// What this process was using at the latest scan (see `ResourceSample`).
    pub usage: ResourceSample,
}

/// Platform boundary (P1). Every OS-specific fact the rest of the app needs comes
/// through this trait; nothing else is allowed to shell out or read `/proc`, `lsof`,
/// or any other OS-specific source.
pub trait ProcessSource {
    /// Every TCP listener owned by the current user, one entry per process, ports
    /// collapsed into that process's `RawListener` rather than duplicated per port.
    fn enumerate(&self) -> Result<Vec<RawListener>, String>;

    /// Resolve an executable path to the name of the outermost application bundle it
    /// lives inside (CONTEXT.md "Belongs To"), or `None` when it is not inside one.
    fn owning_app(&self, exe: &Path) -> Option<String>;

    /// Ask the process to stop, as widely as the platform can safely aim (on macOS,
    /// its process group when it leads one — see `macos::signal_target`). A request,
    /// not a guarantee: see CONTEXT.md "Stopping". Called by `commands::stop_server`.
    fn request_stop(&self, pid: u32) -> Result<(), String>;

    /// End the process without waiting, aimed the same way as `request_stop`. Called
    /// by `commands::force_stop`, and only after a polite stop was tried and failed.
    fn force_stop(&self, pid: u32) -> Result<(), String>;
}
