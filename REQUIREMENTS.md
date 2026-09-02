# Requirements: Portside — Localhost Dashboard

A macOS menu bar app that shows which local servers are running, which project each
belongs to, and lets the user stop the ones they have forgotten.

Terms in **bold** are defined in [CONTEXT.md](./CONTEXT.md).

## Problem

Coding agents start development servers and exit. The servers survive, **Unattended**,
sometimes for days, and the user has no way to notice. Some are still working; some
are holding a port while serving nothing. Both are invisible.

Verified on the author's machine during requirements gathering:

- Two projects both held port 4399 simultaneously, on different addresses, both fine.
- One project held two ports at once, both three days old.
- One development server had held port 4321 for three days while **refusing every
  connection** — dead, but indistinguishable from healthy by age or by usage.
- Every development server had been re-parented to the system: the launching session
  was gone in every case.

## Scope

Runs on any Mac, for any user. No path, project, or program name is hardcoded.

Built with Tauri v2: a Rust core with a web interface, giving a native tray icon and
a small resident footprint. Windows and Linux are not targets now but must stay
reachable — see Portability.

Covers TCP servers only. UDP was examined and holds nothing the user would clean up.

## Functional requirements

### F1 — Discover
Enumerate every listening TCP port and the program holding it, without requiring
elevated permission. Report both IPv4 and IPv6: a program answering on only one is
common, and enumerating one family under-reports.

A port held by another user's program is shown as **Not Yours** — never omitted,
never shown as free.

### F2 — Attribute to a project
Derive **Project** by walking up from the program's working directory to the nearest
directory containing `.git`, `package.json`, `go.mod`, `Cargo.toml`, or
`pyproject.toml`. Where the working directory sits below the repository root, show
the **Package** too.

Where the working directory cannot be trusted to describe what is being served, mark
the result a **Guessed Project** and never act on it. Background services are the
known case: one was observed reporting a working directory inside a project it had
no relationship to.

### F3 — Classify
Assign every Server exactly one **Kind**: development server, part of an app,
background service, your own tool, or part of macOS.

Programs inside an application bundle resolve to the outermost bundle for their
**Belongs To** name — the user recognises their editor, not the helper process
inside it.

A Server with no Project is never classified as a development server.

### F4 — Check responsiveness
Check **Responding** for every Server on every refresh, by opening a bare TCP
connection and carrying no protocol data. This is safe against databases and mail
services, which would otherwise log protocol errors.

This is the only check that identified a genuinely dead server during requirements
gathering, and it is the highest-value signal in the product.

### F5 — Identify
For development servers only, fetch **Title** with a single `GET /`.

Cache it against the program and port; do not re-fetch on routine refresh. A running
server's title does not change. Re-fetch when **Responding** changes, or on request.

Never send protocol requests to any other **Kind**.

### F6 — Display
Row per Server, grouped by Project. Port is a prominent, sortable column.

Rationale: stopping acts on a program, so the program is the row. One Server can
hold several ports, which would duplicate rows if port were the row. Grouping by
project surfaces the case the user cannot currently see — one project quietly
holding several forgotten servers.

Each row shows: **Title** or command, port(s), **Reachable From**, uptime,
**Responding** state, and **Unattended** state.

**Watch Only** Servers appear in their own section with no stop control.

### F7 — Indicate
A resident menu bar **Indicator** showing the number of running development servers,
and whether any has stopped **Responding**.

The tool must be resident. The user's problem is servers they have *forgotten*, so a
tool they must remember to open cannot solve it.

The Indicator never interrupts and never claims a Server should be stopped.
**Forgotten** is the user's judgement, and any automated claim would frequently be
wrong — a service observed running seven days was entirely deliberate.

### F8 — Stop one
Every stop is confirmed. The confirmation states **What This Stops** in the user's
words: the application that will quit, or what a background service is holding up.

Sequence:
1. Ask politely, targeting the Server and everything it started — a surviving child
   keeps the port held.
2. Wait ~3 seconds.
3. Re-check the port. Success is **Stopped** — the port is free — not that the
   request was sent.
4. If still held, say so plainly and offer **Force Stop** as a separate, separately
   confirmed action.

Never escalate to a forced stop automatically. The user agreed to a polite stop.

### F9 — Stop everything
**Stop Everything** covers development servers only. No other **Kind** is ever
included, because one confirmation cannot honestly describe several different
consequences at once.

### F10 — Remember what to keep
The user can mark a Server **Keep Running**. This is the only state persisted across
restarts.

Store by project path and command — never by process id, which changes on every
restart. Marked Servers stay visible but stop drawing attention.

### F11 — Go to the source
From a development server's row, open the **Project** it was started from: in the
user's editor, revealed in Finder, or copied as a path.

The dashboard answers "what is running and should it stop". The next question is
almost always "what *is* this, though" — and answering it currently means reading a
project name off the screen and hunting for the folder by hand. The tool already
knows the path from F2, so making the user find it again is work it created.

Development servers only. Every other **Kind** carries a **Guessed Project** or none
at all, and opening a folder on a guess would present that guess as fact, which N3
forbids. The core refuses a non-development id rather than trusting the interface to
withhold the action.

Opening is a request, like stopping: it can fail (no editor installed), and it says
so rather than reporting success it did not verify.

## Non-functional requirements

### N1 — Lightweight
Measured on the author's machine, 30 listeners: enumeration 43ms; a full cycle of
enumeration plus connection checks 191ms in shell, expected well under 50ms native
with parallel connections. At a 5-second refresh this is under 1% of one core.

Steady state must remain enumeration plus connection checks only. No HTTP traffic
reaches the user's servers while the dashboard merely sits open.

### N2 — Never destructive by surprise
No stop without confirmation. No forced stop without a second confirmation. No bulk
action outside development servers. No stopping of **Watch Only** Servers at all.

### N3 — Honest
The tool never presents a guess as fact. **Guessed Project** is shown as uncertain,
an unresponsive Server is shown as unresponsive, a failed stop is shown as still
running, and a port held by another user is shown as occupied rather than free.

## Explicitly out of scope

- **History.** Uptime already answers "how long has this been up". Storing what ran
  yesterday adds a database in service of a question the user has not asked.
- **Notifications.** They would require a confidence about **Forgotten** that the
  evidence does not support.
- **Attribution to a coding session.** Confirmed impossible: every development
  server observed had lost its parent, and that link is unrecoverable afterwards.
- **Starting servers.** This tool sees and stops; it does not launch.
- **Elevated permission.** No password prompts. The cost is that other users'
  programs are visible only as **Not Yours**.

## Portability

macOS is the only target for now. Windows and Linux are possible later, on request,
and the structure must not make them expensive.

### P1 — Platform boundary
All operating-system knowledge lives behind one interface that returns a neutral
description of a Server: its identifier, ports, working directory, executable path,
and start time.

Everything defined in CONTEXT.md — deriving a **Project** and **Package**, assigning
a **Kind**, deciding **Watch Only**, weighing evidence for **Forgotten** — operates
on that description alone and contains no platform-specific code. Those rules are
universal; only the gathering differs.

### P2 — Known platform differences
These are real and must not be assumed away:

- **Gathering ports and working directories** differs entirely per platform: on
  macOS the working directory needs a separate query, on Linux it is a symlink, and
  on Windows it is often **not obtainable at all** for another process. Since
  attributing a Server to a Project depends on it (F2), Windows will need a
  different strategy and will attribute less reliably.
- **Stopping politely** (F8) has no Windows equivalent. The polite-then-verify
  sequence is a signal on macOS and Linux and must be a window-close request on
  Windows. The requirement — ask first, verify **Stopped**, escalate only with
  separate confirmation — holds on all three; the mechanism does not.
- **Belongs To** is resolved from an application bundle on macOS, a desktop entry on
  Linux, and executable metadata on Windows.

### P3 — Verify per platform
Because the mechanisms differ, F4 and F8 must be re-verified on each new platform.
The measurements in N1 are macOS measurements.

## Open

- Refresh interval. 5 seconds is the assumption behind N1.
- Whether Windows is worth supporting at all, given it cannot reliably attribute a
  Server to a Project.
