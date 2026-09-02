# PRODUCT.md — Portside

## What this is
Portside (formerly "portwatch") is an open-source macOS menu-bar app that shows
every local server this machine is holding — which project each belongs to,
whether it still answers, and which are safe to stop. Its unique mechanism:
coding agents start dev servers and exit; the servers survive unattended for
days, and Portside is the only place the user can see them all and stop the
forgotten ones safely.

## Audience & scene
Developers who work with coding agents ("vibe coders", agentic developers).
The scene: a glance at the menu bar between tasks, panel open for 5–30 seconds,
on a Mac, in whatever light they work in — the panel must respect system
light/dark appearance. They are technical: pids, cwds, and IPv6 don't scare
them, but the default reading must stay in plain words (CONTEXT.md's rule:
where a plain word and a technical word compete, the plain word wins).

## Product truth (binding — from CONTEXT.md / REQUIREMENTS.md)
- The Server (program) is the row, never the port. Grouped by Project.
- Health ("responding" / "not responding" / "unknown") is the highest-value
  signal and must be unmistakable.
- The tool never claims a server is forgotten; it shows evidence, user judges.
- Never destructive by surprise: every stop confirmed, force stop separately
  confirmed, never auto-escalated. Watch Only rows have no stop affordance at all.
- Honest: guesses shown as uncertain; "Not Yours" ports shown as occupied.
- Stop Everything covers development servers only.
- "Keep Running" rows stay visible but recede.

## Brand commitments
- Name: **Portside** (nautical: ports + at your side). Free and open source,
  GPL-3.0 (user-chosen 2026-09-02 over MIT/Apache/PolyForm-NC: community-open,
  but forks must stay open — no closed commercial clones).
- Icons: **Ant Design icon set** (inline SVG; no framework dependency).
- Frontend stays dependency-light: vanilla JS/CSS inside Tauri v2 webview,
  380×520-ish panel. IPC contract in docs/IPC.md changes only by recorded
  amendment, never unilaterally.
- **Standing visual preference (user-revised, 2026-09-02): native macOS
  structure with a friendly warmth layer.** Keep the first-party panel bones —
  SF Pro/system type, quiet controls, native affordances, vibrancy — but the
  surface should feel warm and approachable, not corporate-gray. Illustrated
  assets (3D-clay nautical style, user-produced) carry the friendliness in the
  empty state, help page, and about spots; the palette warms around the ocean
  blue accent. Craft bar for structure stays Control Center / Things 3. A prior
  "ATC strip board" world was built and rejected as ugly — still anti-reference.
- **Theme: user-controlled** — System / Light / Dark switcher in Settings
  (default System). Both renditions stay first-class.

## New capability in scope
"Stats for nerds": a global header toggle flips the panel into nerd density
(pid, cwd/exe-derived detail, raw port/family/reachability, scannedAt, scan
cadence); each row can also expand individually. Friendly mode remains default.

## Platform
macOS menu-bar panel (web tech inside Tauri). Mode: **Operate**.
