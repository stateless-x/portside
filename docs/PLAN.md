# Implementation Plan — Phase 1 (macOS)

Stack: Tauri v2, Rust core, web UI. Target: macOS. Windows/Linux must plug in later
without touching domain logic.

## Architecture

Ports-and-adapters. One trait is the seam between "what the OS tells us" and
"what it means".

```
src-tauri/src/
  platform/
    mod.rs         trait ProcessSource + RawListener + cfg dispatch
    macos.rs       lsof enumeration, cwd lookup, .app bundle walk, signals
  domain/          NO #[cfg], NO OS calls, pure functions
    model.rs       Server, Project, Kind, Ports, Health
    project.rs     cwd -> Project + Package (marker walk)
    classify.rs    RawListener + Project -> Kind
    diff.rs        previous + current -> changes
  probe.rs         TCP connect (liveness), HTTP GET (title, cached)
  scanner.rs       the adaptive loop; owns cadence + hash short-circuit
  keeplist.rs      persisted marks
  commands.rs      Tauri IPC surface
```

### The seam

```rust
pub struct RawListener {
    pub pid: u32,
    pub ppid: u32,
    pub command: String,
    pub exe_path: PathBuf,
    pub cwd: Option<PathBuf>,      // None on Windows - must be tolerated
    pub ports: Vec<PortBinding>,
    pub start_time: SystemTime,
    pub user: String,
}

pub trait ProcessSource {
    fn enumerate(&self) -> Result<Vec<RawListener>>;
    fn owning_app(&self, exe: &Path) -> Option<String>;
    fn request_stop(&self, pid: u32) -> Result<()>;   // SIGTERM / WM_CLOSE
    fn force_stop(&self, pid: u32) -> Result<()>;     // SIGKILL / TerminateProcess
}
```

Rules, enforced in review:
- `domain/` imports nothing from `platform/` except `RawListener`.
- No `#[cfg]` anywhere under `domain/`.
- `cwd: Option` - every consumer handles `None` (this is what makes Windows cheap
  later, and Windows genuinely cannot always provide it).

## Cadence (measured, not guessed)

Measurements on a 30-listener machine: enumeration 26ms; TCP connect 6ms live,
3.4ms dead; port state unchanged in 9 of 10 one-second samples.

| State        | Enumerate | Liveness        |
|--------------|-----------|-----------------|
| Panel open   | 3s        | 3s, parallel    |
| Panel closed | 15s       | only on change  |
| Idle/battery | 60s       | only on change  |

Short-circuit: hash the enumeration result. Unchanged hash -> skip derivation,
classification, and UI emit entirely. Only liveness runs.

Closed-state cost: 26ms / 15s = 0.17% of one core.

## Phases

### Status

Phases 1-5 COMPLETE (91 tests passing — 88 offline plus 3 live-machine tests run with
`cargo test -- --ignored` — clippy clean, app launches and stays resident).

Two defects were found in review and fixed:
- `PortBinding` lacked address family, so IPv4/IPv6 pairs of the same port collapsed
  into indistinguishable duplicates. Contradicted CONTEXT.md's definition of a Port
  Binding as an (address family, port) pair.
- A code editor's extension host classified as a **DevServer**, because its working
  directory contained a package marker but no repository marker, and the app-bundle
  rule that should have intercepted it was never wired up. It would have been offered
  for stopping and swept into **Stop Everything**.

The second is the more instructive: the classification rules were correct, but a rule
that is never called protects nothing. Phase 5 must not assume a rule is enforced
because it is written down — check that something calls it.

**1. Skeleton + seam** - Tauri v2 scaffold, `MenuBarExtra`-equivalent tray, trait
defined, `macos.rs` returning real data, domain empty.

**2. Domain** - project.rs, classify.rs, model.rs. Pure, unit-tested against
fixtures captured from a real machine. No OS access.

**3. Probe + scanner** - liveness, cached titles, the adaptive loop, hash
short-circuit.

**4. UI** - grouped rows, tray count, Watch Only section.

Rows are grouped by Project and show Title, ports with address family, uptime,
Responding state, and Reachable From. A port appearing twice under one Server is
correct when the families differ; show the family rather than hiding the duplicate.

**5. Stop** - confirm dialogs naming What This Stops, process-group targeting,
verify-by-port-release, separate force confirmation.

## Non-negotiables (from REQUIREMENTS.md)

- Liveness = bare TCP connect, no protocol data. Never HTTP to non-dev servers.
- Titles cached; never re-fetched on routine refresh.
- Watch Only servers have no stop path at all.
- Stop Everything = development servers only.
- Success = port released, not signal sent.
- Never auto-escalate to force.
- Route every stop path through `Kind::is_watch_only()`. The rule must be enforced in
  code, not just documented.
