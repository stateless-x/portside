//! Domain types (CONTEXT.md glossary). Pure data — no OS calls, no `#[cfg]`.

use std::path::PathBuf;

use crate::platform::PortBinding;

/// The repository a Server was started from (CONTEXT.md "Project").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    /// The directory containing the project marker (`.git`, `package.json`, etc).
    pub root: PathBuf,
    /// The project's name, taken from `root`'s directory name.
    pub name: String,
}

/// The part of a Project a Server belongs to, when the Project holds several
/// (CONTEXT.md "Package") — e.g. "apps/web" inside "vala-platform".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// Path of the package directory, relative to the Project root.
    pub relative_path: PathBuf,
}

/// How confidently a Server's Project is known.
///
/// This exists instead of a plain `Option<Project>` plus a `guessed: bool` flag so a
/// caller cannot forget to check the guess: matching on this enum is the only way to
/// get at the `Project` inside, so "treat a Guessed Project as fact" (forbidden by
/// N3 and CONTEXT.md "Guessed Project") has to be done on purpose, not by omission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectAttribution {
    /// The working directory can be trusted to describe what the Server serves.
    Known(Project, Option<Package>),
    /// The working directory does not reliably describe what the Server serves (the
    /// known case: a background service reporting an unrelated directory). Must
    /// never be treated as fact or used to decide something is safe to stop.
    Guessed(Project, Option<Package>),
    /// No project markers were found walking up from the working directory, or the
    /// working directory itself is unknown.
    None,
}

/// What a Server is, which determines what stopping it destroys (CONTEXT.md "Kind").
/// Every Server is exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Belongs to a Project and serves only that Project.
    DevServer,
    /// Part of a larger application; stopping it quits the whole app.
    PartOfApp,
    /// Holds ports on behalf of other things; stopping it destroys those too.
    BackgroundService,
    /// A program the user runs on purpose that belongs to no Project.
    YourOwnTool,
    /// Belongs to the system; never stopped through this tool.
    PartOfMacOS,
}

impl Kind {
    /// CONTEXT.md "Watch Only": PartOfMacOS and YourOwnTool are shown but never
    /// offered a stop control, whatever the user clicks. BackgroundService is NOT
    /// Watch Only — it can still be stopped, just with an honest, uncertain warning
    /// about what it's holding up (via its Guessed Project).
    pub fn is_watch_only(self) -> bool {
        matches!(self, Kind::PartOfMacOS | Kind::YourOwnTool)
    }
}

/// Whether a bound address accepts connections from this machine only, or from the
/// whole network (CONTEXT.md "Reachable From"). Re-exported here so domain/ callers
/// don't need to reach into platform/ for it directly.
pub use crate::platform::Reachability;

/// A running program holding a local address (CONTEXT.md "Server"). The unit of
/// display and the unit of stopping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub pid: u32,
    pub command: String,
    pub ports: Vec<PortBinding>,
    pub attribution: ProjectAttribution,
    pub kind: Kind,
    /// CONTEXT.md "Belongs To": the outermost app bundle name, when the Server is
    /// part of one.
    pub belongs_to: Option<String>,
}

/// Whether a Server still answers when something connects to it (CONTEXT.md
/// "Responding"). Nothing computes this yet — phase 3 owns the probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Responding,
    NotResponding,
    /// Not yet checked this cycle.
    Unknown,
}

/// Which process ids the app must never offer to stop, because they ARE the app.
///
/// Always Portside's own pid. In a debug build it also includes Portside's DIRECT
/// parent, because that is where `tauri dev` actually holds the port.
///
/// The observed dev topology is a three-level chain, and the listener is not the app:
///
/// ```text
///   npm run tauri dev   (pid 51521)
///     └─ node …         (pid 51539)  <- HOLDS PORT 1430; Portside's direct parent
///          └─ portside   (pid 43241)  <- std::process::id()
/// ```
///
/// So `pid == std::process::id()` is false for the row the user actually sees, and the
/// Tauri development host was still presented as a stoppable dev server — its cwd being
/// the project root, every Project-derived rule classifies it as one. Stopping it kills
/// the process tree Portside is running inside, mid-scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelfPids {
    /// `std::process::id()`. Always guarded, in every build profile.
    pub own: u32,
    /// Portside's direct parent, guarded in debug builds only. `None` in release, and
    /// `None` when the parent cannot be identified confidently.
    pub dev_parent: Option<u32>,
}

impl SelfPids {
    /// Whether this listener is Portside itself, or the dev host running it.
    pub fn covers(&self, pid: u32) -> bool {
        pid == self.own || self.dev_parent == Some(pid)
    }
}
