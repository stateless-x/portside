# IPC Contract — frozen

The boundary between the Rust core and the UI. Both sides build against this; neither
side may change it unilaterally.

Current version: **v1.4**. See [Amendments](#amendments).

## Events (Rust -> UI)

`servers:changed` — emitted only when the set of Servers, their health, or their
sustained resource `pressure` actually changes, never on every scan tick.

`resources:changed` — v1.4. Fresh CPU/memory figures for Servers already on screen.
The UI applies these IN PLACE and must never rebuild the list from them.

```ts
type Snapshot = {
  projects: ProjectGroup[]      // Kind == DevServer, grouped
  watchOnly: WatchOnlyServer[]  // never stoppable
  others: OtherServer[]         // PartOfApp, BackgroundService
  scannedAt: string             // ISO8601
  scanFailed: boolean           // v1.2 — true = every field above is the last
                                // snapshot that succeeded, not current fact
}

// v1.4 — payload of resources:changed.
type ResourceSamples = {
  samples: { id: string, usage: ResourceUsage }[]
  scannedAt: string             // ISO8601
}

// v1.4 — carried by every displayed Server kind, and by a Snapshot's own rows, so
// the first render already has values.
type ResourceUsage = {
  cpuPercent: number | null     // percent of ONE CPU; may exceed 100 (multi-threaded
                                // on a multi-core machine). null = unavailable, which
                                // is NOT the same as 0.
  memoryBytes: number | null    // resident set size, in bytes. null = unavailable.
  pressure: "normal" | "cpu" | "memory" | "both"
}

type ProjectGroup = {
  project: string               // "vala-platform"
  servers: Server[]
}

type Server = {
  id: string                    // stable across scans: pid + first port
  pid: number
  package: string | null        // "apps/web" in a monorepo, else null
  projectPath: string | null    // v1.1 — absolute Project root; null when unknown
  title: string | null          // page <title>, cached; null = not fetched yet
  command: string               // fallback label when title is null
  ports: Port[]
  uptimeSeconds: number
  health: "responding" | "not_responding" | "unknown"
  unattended: boolean           // parent gone
  keepRunning: boolean
  usage: ResourceUsage          // v1.4
}

type Port = {
  number: number
  family: "v4" | "v6"
  reachability: "localhost" | "all_interfaces"
}

type WatchOnlyServer = {
  id: string
  label: string                 // "openclaw" or the app name
  reason: "your_own_tool" | "part_of_macos"
  ports: Port[]
  uptimeSeconds: number
  usage: ResourceUsage          // v1.4 — seeing is what these rows are for
}

type OtherServer = {
  id: string
  label: string                 // Belongs To name, e.g. "Visual Studio Code"
  kind: "part_of_app" | "background_service"
  guessedProject: string | null // shown as uncertain, never as fact
  ports: Port[]
  usage: ResourceUsage          // v1.4
}
```

## Commands (UI -> Rust)

```ts
panel_opened(): void            // switches scan cadence to 3s
panel_closed(): void            // switches to 15s
refresh_now(): Snapshot         // manual refresh, also refetches titles
set_keep_running(id: string, keep: boolean): void

stop_server(id: string): StopOutcome
stop_all_dev_servers(): StopOutcome[]
force_stop(id: string): StopOutcome

// v1.1 — open the Project a Server was started from.
// Returns whether a launch actually succeeded.
// v1.3 — `editor` selects which editor "editor" means. Optional; a closed set.
open_project(
  id: string,
  how: "editor" | "finder",
  editor?: "vscode" | "cursor" | "zed" | "sublime",
): boolean

type StopOutcome = {
  id: string
  result: "stopped" | "still_running" | "refused"
  message: string               // user-facing, names What This Stops
}
```

## Rules binding both sides

- `stop_server` on a Watch Only id returns `refused` — the UI must never offer it,
  and the core must refuse it anyway. Two independent guards, by design.
- `stop_all_dev_servers` touches only `projects[]`. Never `watchOnly` or `others`.
- `result: "stopped"` means the port was verified released, not that a signal was
  sent.
- `force_stop` is only valid after a `stop_server` returned `still_running`, and only
  within two minutes of it — see the v1.2 amendment.
- `open_project` accepts only a DevServer id. An `others[]` id is refused (`false`)
  by the core, not merely hidden by the UI: those carry a *Guessed Project*, and
  opening a folder on a guess would present that guess as fact. A `watchOnly[]` id
  is refused by the same check. Two independent guards, as with `stop_server`.
- `open_project` never takes a path. The UI holds `projectPath` for display and for
  Copy path, but the core resolves the id through the live snapshot and uses its own
  derived root, so nothing the UI sends can point it at an arbitrary directory.
- `open_project`'s `editor` is never executed. It selects one of the core's own
  hardcoded command chains; a value outside the set runs no editor at all (see the
  v1.3 amendment). The UI cannot name a program for the core to run, by construction.
- **Portside itself is always Watch Only**, labelled `"Portside — this app"`. Enforced
  in the core, not by the UI: it is classified before every other rule, so `stop_server`
  and `force_stop` refuse it through the existing Watch Only guard and
  `stop_all_dev_servers` cannot reach it.

  "Itself" is two pids, not one. Always Portside's own; **in debug builds also its
  direct parent**, because under `tauri dev` the parent is what actually holds the port:

  ```text
  npm run tauri dev  (51521)
    └─ node …        (51539)  <- holds port 1430; Portside's direct parent
         └─ portside (43241)  <- std::process::id()
  ```

  Own-pid alone therefore misses the only row the user sees, and its working directory
  is the project root, so every Project-derived rule would classify the dev host as the
  user's own dev server and offer to stop the tree Portside runs inside.

  Strictly the **direct** parent, and strictly in debug. Walking further up reaches
  `npm`, the shell, then the terminal — none of which are Portside. In release, the
  parent is `launchd` or Finder, so guarding it would hide an unrelated listener, which
  N3 forbids; release guards the own pid alone. A parent pid of 0 or 1 identifies no
  launching process (1 means it already exited) and is never guarded.
- `usage` is **observational only**. No command's behaviour depends on it: it never
  triggers a stop, never changes what a stop does, never affects `keepRunning`, and
  never makes a Server appear or disappear. Both sides may show it; neither may act
  on it.
- Raw `usage` figures never make `servers:changed` fire. A change to `cpuPercent` or
  `memoryBytes` alone travels on `resources:changed`; only a change to `pressure` (or
  to the Server set / health / Kind) is structural.
- The two events are mutually exclusive per scan: a tick that emits `servers:changed`
  does not also emit `resources:changed`, because the Snapshot already carries current
  `usage`.

## Amendments

The contract is frozen against unilateral change, not against change. Every
amendment is recorded here.

### v1.4 — 2026-09-02 — CPU and memory, and a second event to carry them

Added:

- `ResourceUsage { cpuPercent, memoryBytes, pressure }` on `Server`, `WatchOnlyServer`
  and `OtherServer` — every displayed kind, so the UI can place a figure on every row
  without needing to know which shapes can have one.
- `resources:changed`, carrying `ResourceSamples { samples: {id, usage}[], scannedAt }`.

The user could see *that* a server was running but nothing about whether it was doing
anything expensive. The two questions that follow "what is this" are "is it working"
(already answered by `health`) and "is it costing me anything" — which nothing in the
contract answered.

**Why a second event, rather than more fields on `servers:changed`.** CPU and resident
memory move on virtually every scan of a live process. Carrying them structurally would
have made `servers:changed` fire at the full scan cadence — 3s with the panel open —
in direct contradiction of this document's own "never on every scan tick", and every
one of those events rebuilds the list: an open row snaps shut, a hover is lost
mid-click, scroll position resets. So the raw figures travel on their own event that
the UI applies in place, and the Snapshot carries `usage` too, purely so the first
render is not blank while it waits for a sample.

**Why `pressure` is separate from the figures.** A single reading over a threshold
means very little: a dev server compiling, or a database answering one heavy query,
crosses any CPU threshold constantly. So the core reports a *sustained* verdict, and
only that verdict is worth changing what a row says. It is therefore structural, and
rides `servers:changed`.

The rule, in full:

| Metric | Threshold | Must hold for |
| ------ | --------- | ------------- |
| CPU    | `max(100% of one core, 15% of total logical CPU capacity)` | 30 seconds |
| Memory | 1 GiB resident | 10 seconds |

**The CPU threshold scales with the machine.** A fixed percentage cannot serve both a
4-core laptop and a 16-core desktop — 75% of one core is most of a small machine and a
rounding error on a large one. The floor keeps small machines sane and the share keeps
large ones meaningful: 4 CPUs → 100%, 8 → 120%, 10 → 150%, 12 → 180%. The logical CPU
count is read once per process (`std::thread::available_parallelism`, falling back to 1,
which yields the floor).

**The two windows differ deliberately.** CPU is an instantaneous rate and is spiky by
nature, so it must stay high for a full 30 seconds — longer than a compile or a bundler
start-up. Resident memory moves slowly, and a process holding a gigabyte for ten seconds
is simply holding a gigabyte.

A reading back below threshold clears both the pending and the established state
immediately: onset is deliberately slow, recovery is not. Both windows are measured in
TIME rather than in a count of readings, because the scan cadence varies (3s / 15s /
60s) and "N readings" would mean a different duration in each tier. Thresholds are not
user-configurable; there is no command to change them.

**Per-process state is keyed by Server id AND process start time.** The id is pid plus
first port, and a recycled pid rebinding the same port produces the identical id for a
genuinely different program. When the start time for an id moves by more than the
tolerance the stop flow already uses for identity (2 seconds, absorbing `ps` etime's
one-second granularity), everything remembered about the old process is discarded:

- every pending window and established pressure verdict, so the replacement serves the
  full sustain window itself rather than being badged on its first scan; and
- its **cached title**, which is keyed on `(pid, first port)` — exactly what a
  replacement preserves — and would otherwise label the new process with its
  predecessor's page title. CONTEXT.md's "remembered rather than repeated" rests on
  *the same server still running*, a premise a replacement breaks.

This check runs on **both** scan paths. It matters most on the unchanged-fingerprint
short circuit: a replacement with identical pid, port, command and executable path
changes nothing the fingerprint hashes, so it arrives there rather than through full
classification, and the fresh start time is what reveals it.

**Scope: the listed process only.** No process-group or descendant aggregation. A
process group can contain entirely unrelated processes — the same finding that narrowed
the stop signal to the bare pid in an earlier audit — so summing over one would
attribute another program's usage to this Server. Any UI showing these figures says so:
"Measured for this process at the latest scan. Related child processes are not
included."

**Units and absence.** `cpuPercent` is a percentage of ONE CPU and may exceed 100 for a
multi-threaded process on a multi-core machine; it is not clamped, because clamping
would under-report a real measurement. `memoryBytes` is resident set size in bytes —
the platform layer converts from whatever the OS reports (macOS `ps rss` is KiB) so
nothing above that boundary handles a unit. `null` on either field means the figure was
not available and is deliberately distinguishable from `0`: a metric that could not be
read is not a metric that read zero (N3). An unreadable figure costs that one metric and
nothing else — the Server stays visible and the scan does not fail.

`usage` is observational, and the contract says so in the binding rules above: nothing
in this protocol acts on it. Consequently `resources:changed` also leaves the tray
indicator alone — F7 says the indicator never claims a Server should be stopped, and
putting CPU there would come very close to exactly that.

Internally the figures are integers (CPU in tenths of a percent, memory in bytes)
because every domain struct derives `Eq` and both values are compared against
thresholds and for change detection; the division to a fractional `cpuPercent` happens
only at the wire boundary. That is why `Snapshot` and the three Server types are now
`PartialEq` rather than `Eq` on the Rust side — a wire-level detail with no effect on
the JSON.

### v1.3 — 2026-09-02 — which editor "editor" means

Changed:

- `open_project(id, how)` gains an optional third argument:
  `editor?: "vscode" | "cursor" | "zed" | "sublime"`.

v1.1 hardcoded Visual Studio Code as the only editor the panel could ever open, which
silently made "Open in Editor" wrong for everyone using anything else. The editor is
now a user preference (stored UI-side in `localStorage`, not by the core — the
keeplist stays the only backend persistence) and is passed per call.

**This argument is an identifier, never a command.** The core matches it against a
closed set and maps each id to its own hardcoded chain — a CLI binary first, then
`open -a` with the application name, mirroring the v1.1 reasoning that a bundled app
inherits a PATH that usually excludes where these CLIs live:

| `editor`    | CLI      | Application         |
| ----------- | -------- | ------------------- |
| `"vscode"`  | `code`   | `Visual Studio Code` |
| `"cursor"`  | `cursor` | `Cursor`            |
| `"zed"`     | `zed`    | `Zed`               |
| `"sublime"` | `subl`   | `Sublime Text`      |

Absent and unknown are deliberately different:

- **Absent** (a UI predating this amendment, or any `how: "finder"` call) uses the
  Visual Studio Code chain — the v1.1 behaviour, unchanged.
- **Unknown** — any string outside the table — runs no editor at all and falls
  through to opening the folder in Finder. It is never passed to a shell, never used
  as a program name, and never defaulted into an editor. Nothing the UI sends can
  become an executable.

Every chain still ends with the v1.1 `open` fallback, so "go to source" continues to
get the user to their project even when no editor is installed.

"Finder only" is a UI-level preference with no value here on purpose: it means *don't
use an editor*, which the contract already expresses as `how: "finder"`.

### v1.2 — 2026-09-02 — honest scan failure

Added:

- `Snapshot.scanFailed: boolean` — `true` when the most recent scan attempt failed.

The scan loop previously discarded scan errors entirely, so a failing scan and a
machine with nothing running produced the identical UI: the last good snapshot, no
event, no indication. That is a fabricated fact, which N3 forbids.

The loop now keeps the last good snapshot rather than clearing it — clearing would
fabricate "nothing running", which is the same violation in the other direction — and
sets this flag so the UI can say plainly that it could not look just now. The flag
clears on the next scan that succeeds, and both the onset of a failure and the recovery
from one emit `servers:changed`, so the note appears and disappears on its own.

Also changed in the same pass, affecting observable `force_stop` behaviour but not the
contract's shape: the authorization a failed polite stop grants now **expires after two
minutes**. `force_stop` on an id whose polite-stop failure is older than that returns
`refused` (asking the user to try stopping it again first) rather than proceeding.
Previously the authorization lasted for the app's whole lifetime, which outlived the
decision the user actually made — F8/N2 rest on Force Stop being the deliberate,
immediate follow-up to a stop the user just watched fail.

### v1.1 — 2026-09-02 — "go to source" (user-approved)

Added, to support opening a dev server's project from its row:

- `Server.projectPath: string | null` — absolute path of the Project root, from the
  same F2 walk that already derives `package`. `null` when the Project is unknown.
- `open_project(id, how): boolean` — launches the editor or Finder for that Server's
  Project. `true` means a launch was spawned successfully.

Copying the path is deliberately *not* a command: the UI already has `projectPath`
and uses the clipboard directly, so no IPC round-trip exists for it.

Editor launch is a fallback chain (`code` CLI, then `open -a "Visual Studio Code"`,
then `open`), because a bundled app inherits a minimal PATH that usually excludes
where `code` is installed. Which tier succeeded is logged; only an all-tiers failure
returns `false`.
