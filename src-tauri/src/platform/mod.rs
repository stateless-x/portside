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
