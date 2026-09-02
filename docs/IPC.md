# IPC Contract — frozen

The boundary between the Rust core and the UI. Both sides build against this; neither
side may change it unilaterally.

Current version: **v1.3**. See [Amendments](#amendments).

## Events (Rust -> UI)

`servers:changed` — emitted only when the set of Servers or their health actually
changes, never on every scan tick.

```ts
type Snapshot = {
  projects: ProjectGroup[]      // Kind == DevServer, grouped
  watchOnly: WatchOnlyServer[]  // never stoppable
  others: OtherServer[]         // PartOfApp, BackgroundService
  scannedAt: string             // ISO8601
  scanFailed: boolean           // v1.2 — true = every field above is the last
                                // snapshot that succeeded, not current fact
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
}

type OtherServer = {
  id: string
  label: string                 // Belongs To name, e.g. "Visual Studio Code"
  kind: "part_of_app" | "background_service"
  guessedProject: string | null // shown as uncertain, never as fact
  ports: Port[]
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

## Amendments

The contract is frozen against unilateral change, not against change. Every
amendment is recorded here.

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
