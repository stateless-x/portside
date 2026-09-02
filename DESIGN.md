---
name: Portside
description: A first-party-style macOS menu-bar panel for every local server your machine is holding.
colors:
  bg: "#efedea"
  bg-raised: "#fffefc"
  bg-chrome: "#f4f2ef"
  label: "#1d1d1f"
  label-2: "#55565a"
  label-3: "#636469"
  accent: "#0064d2"
  accent-text: "#0a5bb8"
  accent-fill: "#0a71e0"
  on-accent: "#ffffff"
  good: "#1d7a44"
  unknown: "#66676b"
  danger: "#b0241d"
  danger-hover: "#a22019"
  caution: "#7a5300"
typography:
  title:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'SF Pro Text', 'Helvetica Neue', sans-serif"
    fontSize: "13px"
    fontWeight: 600
    lineHeight: 1.4
    letterSpacing: "-0.01em"
  body:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'SF Pro Text', 'Helvetica Neue', sans-serif"
    fontSize: "12.5px"
    fontWeight: 400
    lineHeight: 1.4
  meta:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'SF Pro Text', 'Helvetica Neue', sans-serif"
    fontSize: "11px"
    fontWeight: 500
    lineHeight: 1.4
  data:
    fontFamily: "ui-monospace, 'SF Mono', Menlo, monospace"
    fontSize: "12.5px"
    fontWeight: 600
    lineHeight: 1.4
rounded:
  control: "5px"
  row: "8px"
  popover: "7px"
  dialog: "11px"
spacing:
  xs: "3px"
  sm: "7px"
  md: "10px"
  lg: "14px"
components:
  row:
    backgroundColor: "{colors.bg-raised}"
    textColor: "{colors.label}"
    rounded: "{rounded.row}"
    padding: "7px 8px 7px 10px"
  segment-selected:
    backgroundColor: "{colors.bg-raised}"
    textColor: "{colors.label}"
    rounded: "{rounded.control}"
  back-button:
    backgroundColor: "transparent"
    textColor: "{colors.accent-text}"
    rounded: "{rounded.control}"
  titlebar-button:
    backgroundColor: "transparent"
    textColor: "{colors.label-3}"
    rounded: "{rounded.control}"
    height: "22px"
  titlebar-button-pressed:
    backgroundColor: "{colors.accent-fill}"
    textColor: "{colors.on-accent}"
    rounded: "{rounded.control}"
  dialog-button-default:
    backgroundColor: "{colors.accent-fill}"
    textColor: "{colors.on-accent}"
    rounded: "6px"
    padding: "5px 12px"
  stop-button:
    backgroundColor: "transparent"
    textColor: "{colors.danger}"
    rounded: "{rounded.control}"
---

# Design System: Portside

## Overview

**Creative North Star: "The First-Party Panel"**

Portside is a native macOS menu-bar panel, played straight. It borrows nothing from a designed metaphor — no board, no paper, no stamped tickets — and everything from the first-party Apple vocabulary: system type, system grays, hairline separators, quiet controls that only surface on hover, one accent color used sparingly. The craft bar is Apple's own Control Center, Things 3, and iStat Menus. The tool's ambition is to disappear into the task: the user glances at the menu bar between other work, and the panel should look like it always belonged there.

Light and dark are both first-class renditions of the same system, not one inverted from the other — a menu-bar panel that ignores system appearance reads as broken. Every text token is verified at ≥4.5:1 on its real composited ground, in both modes, including on the receded "kept" ground and on the tinted "not responding" row.

**The structure is native; the warmth is a layer over it, never a replacement for it.** Light's grounds carry a slight warm cast rather than a pure neutral gray, the row cards are a point softer, and the empty state is roomier and more plainly worded — but the bones stay first-party: system type, quiet controls, hairline separators, one accent. There is no cartoon chrome, no colored section background, and no second typeface. Warmth here is a hue and a spacing decision, not a costume.

**A prior direction, an "ATC strip board" world (paper strips clipped to a controller's board, holder-edge color coding, cocked/rotated alarm strips, letterspaced monospace stamps, a beige paper surface), was fully built and then rejected by the user as ugly.** It is the anti-reference for this system: no paper strips, no holder edges, no cocked rotation, no letterspaced stamps, no beige. Nothing from that world is carried forward here.

**Key Characteristics:**
- Native materials only: system sans, system grays, hairline dividers, native-weight vibrancy — no invented surface metaphor.
- One accent, used only for the pressed/selected state and the dialog's default action.
- Controls are quiet by default and reveal on hover/focus — the Control Center habit.
- Status is never color-alone: the health word always ships beside the dot.
- Recede is per-element color, never container opacity.
- Monospace is reserved for data (ports, pids, nerd grid, telemetry); system sans carries every name and every sentence.

## Colors

A two-layer neutral system (a slightly recessed panel ground and raised white/card content) carries almost the whole panel; a single accent and a small status vocabulary (good/unknown/danger/caution) do the rest.

### Primary
- **Accent** (`#0064d2` light / `#4da2ff` dark): the one color in the panel. Used only for a pressed titlebar toggle, the Keep checkbox's native accent-color, the focus ring, and text selection.
- **Accent Text** (`#0a5bb8` light / `#4da2ff` dark): the accent used as *reading text* on the panel ground — currently the pages' back button only. A separate token because `--accent` is tuned as a hairline/focus color and `--accent-fill` as a ground behind white text; neither reads well as small text (see Contrast).
- **Accent Fill** (`#0a71e0` light / `#0a6ad0` dark): the filled/selected-state variant — the pressed Stats toggle, the dialog's default button, the hover highlight in the popover menu. Darker in dark mode specifically because it carries white text on a selected menu item and must clear 4.5:1 there.

### Neutral
- **Bg** (`#efedea` light / `#1c1c1e` dark): the panel's own recessed ground. Also the ground a "kept" row recedes onto.
- **Bg Raised** (`#fffefc` light / `#2c2c2e` dark): the ground every ordinary row, dialog, popover menu, and toast sits on. Rows stay opaque even where the surrounding chrome goes translucent (see Elevation & Depth).
- **Bg Chrome** (`#f4f2ef` light / `#232325` dark): the title bar and telemetry footer strip.

**Light's three grounds carry a warm cast** (a few points of red/green lead over blue) instead of the pure neutral grays they started as. The move was made *upward* in lightness as well as warmer, and that direction was chosen from measurement rather than taste: warming downward to `#eeebe7` drops `--label-3` on the panel ground to 4.96:1, under the floor, while the values above lift the same pair from 5.00:1 to **5.05:1**. Every changed pair improved or held (see Contrast below). **Dark's neutrals are deliberately untouched** — a warm dark ground stops reading as warm and starts reading as a color cast.

### Contrast (light, after the warmth pass)

Measured on real composited grounds, not computed from tokens — the two grounds that matter most are composites (`--danger-tint` over `--bg`, and the kept row painting with `--bg` directly), so token math would not have described what is actually painted.

| Pair | Before | After |
| --- | --- | --- |
| Kept row title / meta / "kept" tag on kept ground | 5.00 | **5.05** |
| Project heading, section hint on panel ground | 5.00 | **5.05** |
| "not responding" word on tinted row | 5.10 | **5.15** |
| Network badge on row ground | 5.80 | **5.86** |
| Row title, port on raised row | 16.83 | 16.70 |
| Meta text, health word, nerd value on raised row | 7.33 | 7.27 |
| Nerd label on raised row | 5.90 | 5.86 |
| Titlebar count, telemetry on chrome | 5.27 | **5.28** |
| Section title on panel ground | 6.20 | **6.27** |

Nothing fell below 4.5:1; the tightest pair in the list is the kept-row text at 5.05:1. Dark is unchanged, its tightest pair being the nerd label at 4.90:1.

The pages add these pairs (light / dark), measured the same way:

| Pair | Light | Dark |
| --- | --- | --- |
| Back button (`--accent-text` on ground) | **5.61** | 6.42 |
| Segment, unselected | 5.26 | 5.53 |
| Segment, selected | 16.70 | 12.80 |
| Field label, help term | 14.40 | 15.63 |
| Page body, help definition, subhead, footer name | 6.27 | 7.94 |
| Field hint, footer note, sponsor row | 5.05 | 5.98 |
| Select text | 16.70 | 12.80 |

**Accent as text needed its own token.** The back button is the one place the accent is used as reading text on the panel ground, and `--accent` measures only 4.79:1 there on the warmed light ground — passing, but the tightest pair in the panel for a control reached on every page. `--accent-fill` was tried and is worse (it is tuned as a *fill* behind white text, so as text it reads 4.04 light / 3.23 dark). `--accent-text` (`#0a5bb8` light / `#4da2ff` dark) is the accent hue set for reading on the panel ground, and lifts the pair to 5.61:1. Dark's value is `--accent` unchanged, because its lighter accent already clears 6.42:1 and deepening it would move the wrong way against a dark ground.
- **Label** (`#1d1d1f` light / `#f5f5f7` dark): primary text — titles, port numbers, dialog titles.
- **Label 2** (`#55565a` light / `#b0b1b5` dark): secondary text — most repeated meta (uptime, health word, dialog body), held above 4.5:1 because it carries so much of the panel's small print.
- **Label 3** (`#636469` light / `#98999e` dark): tertiary text — receded-row text, nerd-grid labels, title-bar count, chevron icons. Verified ≥4.5:1 on every ground it lands on (raised card, recessed kept row, chrome), not just one.

### Status
- **Good** (`#1d7a44` light / `#37c85f` dark): the "responding" health dot.
- **Unknown** (`#66676b` light / `#96979c` dark): the "unknown" health dot.
- **Danger** (`#b0241d` light / `#ff6961` dark): "not responding" text and dot, Stop/Force Stop controls, the still-running note. Held dark enough in light mode to clear 4.5:1 as text on the tinted `not_responding` row, which composites `--danger-tint` over `--bg`, not over raised white.
- **Caution** (`#7a5300` light / `#e8b23c` dark): the network-exposure signal only. Amber deliberately, so it reads as "pay attention" without borrowing the danger channel, which means "something is wrong."

### Named Rules
**The Beige Is the Anti-Reference Rule.** The network badge is a transparent hairline chip (`--caution-border` outline) on the row's own ground, never a tinted/filled amber patch. A filled beige-toned badge was the panel's last surviving trace of the rejected strip-board world; the hairline treatment also reads stronger against the row than a tint of itself would.

**The One Ground, Two Chrome Strips Rule.** Only `--bg` (via `.panel`) and the title bar / telemetry chrome go translucent under vibrancy. `--bg-raised` (every row, dialog, menu, toast) stays opaque in both a plain browser and the real window — see Elevation & Depth.

## Typography

**System Font:** `-apple-system, BlinkMacSystemFont, "SF Pro Text", "Helvetica Neue", sans-serif`
**Data Font:** `ui-monospace, "SF Mono", Menlo, monospace`

**Character:** Two voices with a firm boundary between them. System sans carries every name, sentence, and label in the friendly view — sentence case throughout, no letterspacing anywhere except the −0.01em on the panel title. Monospace is confined to data that is measured or counted: port numbers, the nerd-stats grid, the telemetry footer, and the "go to source" menu's path line. Nothing is stamped, tracked, or uppercased for effect — that vocabulary belonged to the rejected world.

### Hierarchy
- **Title** (600, 13px, −0.01em): "Portside" in the title bar. The only letterspacing value in the system.
- **Port** (600, 12.5px, mono, tabular-nums): the port number — the one figure a user scans a row for. Monospace and tabular so digits line up down the column; the sole routine use of monospace in the friendly view.
- **Row title** (400, 12.5px, sans): the server/service/watch-only label. Recolors to Label 2 (`.is-command`) when no page title exists yet and the raw command stands in instead — lighter because it is evidence, not a claimed name.
- **Section title** (600, 11px, sans): "Development servers," "Other," "Watch only."
- **Meta / label** (500, 11px, sans): uptime, health word, "unattended," project heading, dialog body copy.
- **Secondary tag** (400–500, 10.5px, sans): the smallest sans size, for labels that sit beside meta text without competing with it — the network badge, the "kept" tag, and the "Keep running" toggle's label. Its use on the toggle is what lets a two-word label fit the action cluster at 380px.
- **Nerd data** (400, 10.5px, mono, tabular-nums): the per-row and global nerd-stats grid, and the telemetry footer at 10px — the panel's only other sustained use of monospace. Shares the secondary tag's size but never its font: mono here always means measured data (The Data-Only Mono Rule).

### Named Rules
**The Data-Only Mono Rule.** Monospace is reserved for port numbers, the nerd grid, telemetry, and the source-menu path — measurements and machine-legible values. Everything else, including every section title and every button label, is system sans in sentence case. No letterspacing anywhere except the −0.01em title.

## Theming (System / Light / Dark)

The panel's appearance is a user preference with three values, defaulting to System, chosen in Settings and stored client-side (`localStorage`, key `portside.theme`).

**Named Rule — The CSS-Decides-The-Theme Rule (system rule).** The theme is resolved entirely in CSS, from three token blocks in this exact source order:

1. `:root` — the complete **light** palette.
2. `@media (prefers-color-scheme: dark) { :root:not([data-theme="light"]) }` — dark, when the OS says dark **and** the user has not forced light.
3. `:root[data-theme="dark"]` — dark, forced.

System mode sets **no attribute at all**, so rule 2 alone decides and the OS appearance is honored on the very first frame with no JavaScript in the path — there is no flash of the wrong theme to prevent, because the wrong theme is never painted. It also keeps tracking the OS live, which is why no `matchMedia` listener exists: JS could only re-derive what CSS already knows. The two explicit modes stamp `data-theme` and win — `"light"` is excluded from rule 2 by its `:not()`, and `"dark"` is picked up by rule 3.

Two consequences bind anyone editing this system:

- **Rules 2 and 3 compute to identical specificity**, so their source order is the only tiebreak: rule 3 must stay below rule 2. They are a **matched pair** — a token added to one must be added to the other, byte-identical, or explicit Dark quietly diverges from system Dark.
- **`color-scheme` is narrowed per explicit mode** (`:root[data-theme="light"] { color-scheme: light }` and its dark twin), and left at `light dark` in System. This is not decoration: the Keep running checkbox is a genuinely native, system-rendered control, and the scrollbar, caret, and focus surfaces are engine-drawn too. Without the narrowing, an explicit Light theme on a dark-appearance Mac renders dark native controls on a light panel.

JavaScript's only jobs are recording the choice and stamping the attribute. It additionally asks the real window to match via `getCurrentWindow().setTheme()`, so the vibrancy *material* flips with the panel — best-effort and defensively wrapped, because `setTheme` returns a Promise (a bare `try`/`catch` would not see its rejection) and is gated behind the `core:window:allow-set-theme` capability, which this app's `capabilities/default.json` does not currently grant. Until that permission is added, the call rejects and is warn-logged; the panel's own grounds have already flipped regardless, so the only cost is the material.

## Layout

The panel is a fixed 380×520px webview with no responsive breakpoints; the frontend never resizes. Structure is a vertical flex column: a fixed-height title bar (drag region), a scrolling `.panel-body`, and an optional fixed telemetry footer shown only in Stats mode.

Sections stack Development Servers → Other → Watch Only, each rendered only when it has content (Other and Watch Only are omitted entirely when empty; Development Servers shows an empty state instead). Within a section, rows group under a project heading with an 8px gap between groups and a 3px gap between rows in a group. Sections are separated by a hairline (`--separator`) plus top padding — never a filled section background.

**Three views share `#panel-body`.** The list (`#list-view`) and the two pages (`#settings-view`, `#help-view`) live *inside* the one scroll container rather than replacing it, so `#panel-body` keeps sole ownership of the scrollbar and its theming, and only one view is ever unhidden. The list is never torn down when a page opens: `render()` keeps painting into `#list-view` underneath, which is what makes returning show current data with no refetch.

That shared scroller is also the reason `paint()`'s scroll save/restore is **guarded on the list being the active view**. Restoring the list's saved offset while a page is open would scroll the page the user is reading — a `servers:changed` arriving mid-page must be completely invisible, and never pull the user back to the list.

Dialogs, the popover menu, and toasts render as siblings of `.panel` inside `#overlay-root`, never inside a row: `#panel-body` is a scroll container that would clip an anchored popover at its edges, and a mid-flow `servers:changed` re-render rebuilds `#panel-body`'s contents and would destroy anything living inside it.

## Elevation & Depth

Flat by default. Depth comes from two tonal grounds (`--bg` recessed, `--bg-raised` raised) and hairline borders/separators, not from shadows — rows, sections, and the title bar carry no shadow at rest. The shadow token (`--shadow-popover`) is reserved for the three elements that float above the flow: the "go to source" popover menu, the confirmation dialog, and the toast.

One further shadow exists: the **selected segment** of the Appearance control carries a `0 1px 2px rgba(0,0,0,0.1)`. It is deliberate rather than decorative — a segment lifted out of a recessed track is a depth statement, and it carries a real offset and blur rather than being a zero-offset halo. It is intentionally not `--shadow-popover`, which is tuned for elements floating well above the panel; this one sits a single point proud of its track.

**Vibrancy (real window only, provisional).** The Tauri window is transparent with an `NSVisualEffectView` behind it (`windowEffects: "popover"`, gated behind the `macos-private-api` Cargo feature), so in the real app `.panel` and the two chrome strips (title bar, telemetry) tint over that material as alpha layers instead of painting opaque grounds. This is scoped to `body.tauri`, set in `main.js` before first paint when `window.__TAURI__` exists, so a plain browser (mock backend, screenshots) — which has no material behind it — keeps the opaque fallback automatically. Rows stay opaque in both cases: Control Center's own tiles are opaque over vibrancy too, and `--bg` itself must stay opaque because `.row.is-kept` paints with it directly. **The alpha values currently in use (panel 0.72, chrome 0.6) are provisional, tuned by eye against a screenshot rather than the real windowed material, and are pending a pass against the actual compiled window.**

Those translucent grounds are **tokens** (`--vibrancy-panel`, `--vibrancy-chrome`) defined alongside every other token in all three theme blocks, and the `body.tauri` rules read them. They were previously hardcoded `rgba()` literals whose dark variant was gated on `prefers-color-scheme` alone — which meant an explicit Light theme on a dark-appearance Mac painted a *dark* translucent panel under light text. One pair of rules reading tokens is also why the theme blocks are the only place a color is defined: duplicating the `body.tauri` rules per theme would triple them and drift.

### Named Rules
**The Recede-By-Element-Color Rule (system rule).** A row that should recede — the `is-kept` state — is never dimmed with `opacity` on the row container. `opacity` composites the entire subtree into one layer, so a child cannot opt back out of it by setting its own `opacity: 1`; that exemption would silently do nothing. Instead, only the specific elements allowed to recede are named individually (`--label-3` on meta text, row title, watch-reason, the "kept" tag, icon buttons, and the Keep running toggle), and `.health`'s word and `.badge-network` are never touched by any recede rule in any mode, because fading either would leave color carrying status or the network signal alone. Anything added to a receding row joins this list explicitly or it is not styled by the recede at all — a new element that inherits a dim from a container is the failure this rule exists to prevent.

This is a container-opacity prohibition specifically, not a blanket ban on the CSS property: `.row-actions` legitimately uses `opacity: 0 → 1` as a hover/focus-visibility toggle for the action buttons (not a recede), and menu-item icons sit at a fixed `opacity: 0.75` as a decorative dim. Both are reveal/dim devices on elements with nothing beneath them to hide; neither composites a subtree that a descendant needs to escape.

## Shapes

Radii are small and uniform: 5px on titlebar/icon buttons, the keep-toggle, the back button and the segmented control's segments; 8px on rows; 7px on the popover menu; 11px on the dialog. Rows moved from 7px to 8px in the warmth pass — at this row height one point is the difference between a card reading square-ish and reading soft, and 8px is the ceiling before it stops looking like a macOS list row. Nothing in the panel uses a pill shape or a hard square corner. Borders are hairlines (`--border`, `--separator`) at roughly 11–15% black/white alpha, never a heavier stroke. The one exception is the network badge's border, which is intentionally more visible (`--caution-border` at higher alpha) because it is the whole of that chip's affordance — it has no fill to lean on.

## Components

### Title bar
Drag region (`-webkit-app-region: drag`) holding the panel title, a live server count, and two icon buttons (Stats toggle, Refresh) that opt out of the drag region individually. Settings and Help deliberately do NOT live here: the user chose the tray menu (src-tauri/src/lib.rs — "Settings…" / "Help" items that show the panel and emit "navigate") as their home, with ⌘, and ⌘? as the native shortcuts and the browser mock's only path in. The titlebar keeps only controls that act on the list itself. When the last scan failed (`Snapshot.scanFailed`, docs/IPC.md v1.2) the count is *replaced* by the plain words "couldn't scan just now" rather than annotated: stating a number and a doubt at once leaves the number reading as current fact, and the list below it is the last successful scan, not the present moment. Styled no louder than the count it replaces — no color, no icon, no motion — because F7 forbids the indicator ever reading as an interruption. Icon buttons are quiet (transparent, `--label-3`) until hovered (`--bg-hover`, `--label`) or pressed (`--bg-active`); the Stats toggle additionally fills solid `--accent-fill` when `aria-pressed="true"`, reading like a selected segmented control. Refresh spins its icon while a refresh is in flight; the spin is suppressed under `prefers-reduced-motion`.

### Rows
- **Shape:** 7px radius, 1px `--border`, `--bg-raised` fill; border darkens to `--label-3` on hover.
- **Anatomy:** a leading monospace port cell, a title, a health indicator (dot + word), uptime, an optional "unattended" hint, an optional network badge, an optional resource-pressure badge, an optional "kept" tag — then a hover-revealed action cluster (expand chevron, optional "go to source," the "Keep running" checkbox, Stop). At the panel's fixed 380px, the fullest version of this row — multi-port, go-to-source present, all four controls — fits on one line with the action cluster at 157px and no wrap. The meta line itself wraps (`flex-wrap`), which is what absorbs the widest real case: network badge + "High CPU and memory" + "kept" on one row, verified at 380px with no horizontal overflow.
- **Resource figures** ("5.3% CPU  148 MB") appear only inside an expanded row or in Stats mode — never on the friendly row, where ordinary usage would be noise. The row builders simply do not create the line otherwise, so it needs no visibility rule.
- **not_responding state:** the row tints (`--danger-tint` fill, `--danger-border` outline) and the health word/dot turn `--danger`. Unmistakable but native — a tint and red text, no rotation, no displacement. This is the direct rejection of the former "cocked strip" treatment.
- **is-kept state:** the row's fill drops to `--bg` (the panel's own recessed ground) and meta text, title, watch-reason, the "kept" tag, and the action icons recede to `--label-3` — see the Recede-By-Element-Color Rule. The health word, the network badge and the resource-pressure badge hold full strength. Keeping the pressure badge bright on a kept row is deliberate: the mark says the user meant to leave the server on, which is not a statement that what it consumes stopped mattering. Hovering a kept row restores `--bg-raised` and full-strength title color. A small "kept" tag sits on the meta line so the fade states its own reason: a receded row with no explanation reads as broken rather than deliberate, and the explanation must not depend on hovering the control that caused it.
- **Watch-only rows** carry no action slot at all — not a disabled button, no reserved space for one. The only control is the expand chevron. They also never render a "go to source" action, since a `WatchOnlyServer` carries no project path. Portside's own process, when it holds a port (i.e. under `tauri dev`), appears here as "Portside — this app": shown honestly rather than hidden, and never stoppable.
- **Row titles** share typography and truncation, but only the dev-server title is interactive — it is a real `<button>` that discloses the full process name. Watch Only and Other titles are plain spans and carry no pointer cursor, hover underline or focus treatment, because there is nothing behind them to click.
- **Dev rows only** offer "go to source" (the folder-open icon), and only when `server.projectPath` is non-null — an action that would open nothing is treated as a lie the panel must not tell.

### Network badge
A transparent hairline chip: `--caution` text, 1px `--caution-border`, no fill. Rendered once per row even when several ports are exposed, because the fact the user acts on is that the server is reachable at all, not how many addresses carry that exposure. See the Beige Is the Anti-Reference Rule.

### Resource-pressure badge
The same hairline-chip construction and the same `--caution` vocabulary as the network badge, and for the same reason: both are things to notice, neither is a thing that is wrong. It reads "High CPU," "High memory," or "High CPU and memory" — words, never a coloured dot, so it survives high contrast and reaches a screen reader intact, exactly as the health word does.

Deliberately **not** `--danger`. A server working hard is working. Red would push the user to act on a fact the panel itself never acts on: the badge is observational, it never appears because Portside decided something, and nothing about stopping changes when it is present. It carries no icon — the words are the whole affordance, and a second glyph beside the network badge's would read as two warnings rather than one.

It appears only on *sustained* elevation, which is a backend verdict (see docs/IPC.md v1.4), never derived in the UI from a single figure. That is what stops a compile from flashing a warning at someone: CPU must stay high for 30 seconds and memory for 10, and the CPU threshold scales with the machine's core count. The tooltip states those durations exactly ("CPU usage stayed high for at least 30 seconds.") rather than saying "the last few scans" — the scan cadence varies between 3s and 60s, so a count of scans is not a length of time.

### CPU figures
Two presentations of one measurement, chosen by who is reading.

- **Expanded rows and Stats mode** keep Activity Monitor's convention: 100% is one fully used core, so a multi-core process reads "276% CPU". This is the number a developer can compare directly against Activity Monitor, and it is what the wire carries. Above one core the figure's tooltip explains it ("100% equals one CPU core. 276% is about 2.8 cores.").
- **The Quick read** translates to a share of the whole Mac — "about 28% of this Mac's CPU" — because that summary answers "should I worry", and 276% reads alarming until you know the machine has ten cores. The core count comes from `navigator.hardwareConcurrency`; when it is unavailable the raw wording is used instead, rather than the panel inventing a denominator.

Memory is presented one way everywhere: binary units, MB/GB labels.

### Buttons
- **Icon buttons** (expand, go-to-source, Refresh, Stats, Stop): 22×22px, 5px radius, transparent at rest, `--bg-hover`/`--bg-active` on interaction. Stop additionally carries `.is-danger`, hovering to `--danger-tint` fill and `--danger` text/icon.
- **Stop all dev servers:** text-only, `--danger` on transparent, hovers to a `--danger-tint` background. Deliberately small and never competing visually with the rows it acts on.
- **Force Stop…:** a bordered `--danger-border` button with `--danger` text, offered only after a Stop attempt returns `still_running` — never a first action.
- **Dialog buttons:** see Dialogs below.

### Controls
The only form control in the panel is the Keep running checkbox: a native, system-rendered 12×12px checkbox styled only via `accent-color: var(--accent-fill)`, wrapped in a `.keep-toggle` label that highlights `--bg-hover` on hover. No custom checkbox graphic exists anywhere in the system.

Its label reads **"Keep running"**, not "Keep". "Keep" alone reads as an imperative aimed at the row — asking the user whether to keep *this* — when the mark actually names a state the user is reporting: this one is deliberate, leave it alone (CONTEXT.md "Keep Running"). The two extra words make it the widest control in the action cluster, which is paid for by dropping its type to 10.5px (the secondary-tag size) and tightening its gap and padding rather than by shortening the label — the visible words are the part that has to be right. The tooltip and `aria-label` carry the meaning the label cannot ("You meant to leave this on — Portside won't draw attention to it") instead of restating it.

### Nerd grid
A `dl` of `dt`/`dd` pairs in monospace at 10.5px, appended under a row's meta line either globally (the Stats toggle) or per-row (the expand chevron). Every field is a Snapshot field already on hand — nothing is derived or invented for display, and a `WatchOnlyServer`'s grid only ever shows the fields that type actually carries (no pid, no command, no health).

### Popover menu ("go to source")
Rendered into `#overlay-root`, `position: fixed`, anchored to its trigger button and clamped inside the 380×520 panel (flips above when it would overflow the bottom). Shows the destination path as its own line before the action list, so the user can see where an action goes before choosing it. Menu items highlight `--accent-fill` on hover and keyboard focus alike, matching real menu behavior.

### Dialogs
Centered over a `rgba(0,0,0,0.32)` scrim, 11px radius, `--shadow-popover`. Title, body copy, then a two-button row.

**Named Rule — The Cancel-Is-Default Rule.** Every dialog in this panel confirms a stop, so the panel inverts the usual confirm-button placement: **the rightmost button is the default, and it is Cancel, styled with the accent fill (`is-default`)**, never the destructive action. The destructive button (Stop / Force Stop / Stop all) is never rightmost and never carries the default's accent weight — it sits to Cancel's left, bordered and colored `--danger` only. `main.js` also focuses Cancel programmatically on open, so Enter/Space activate the safe action; because a pointer-opened dialog suppresses `:focus-visible`, the default affordance is carried structurally by the `is-default` class, not by focus state alone. This is a deliberate safety-first inversion of the platform's usual "primary action on the right" convention, made because every dialog here is a destructive confirmation.

### Pages (Settings, Help)
In-panel views, not windows: the body swaps to the page and a back affordance returns. Each opens with a `page-header` — a quiet chevron-plus-"Back" control in `--accent` on transparent — followed by the page title at the panel-title's own size and weight. **Escape returns too, but only when `#overlay-root` is empty**, so a dialog or the source menu keeps ownership of Escape while it is open. Focus moves to Back on open and returns to the title-bar button that opened the page on the way out.

A page is another room in the same panel, not a different surface: same grounds, same type ramp, same native controls, no page-specific chrome.

- **Segmented control** (Appearance): one recessed `--bg-active` track with the selected segment lifted onto `--bg-raised` and a 1px shadow — the macOS pattern. Deliberately **not** accent-filled: the accent belongs to pressed toggles and the dialog default, and a three-way appearance picker is a choice among peers rather than an on/off state. `role="radiogroup"` with `aria-checked`, so the selection is named rather than carried by color alone — the same principle as the health dot never traveling without its word.
- **Checkbox** (Stats for nerds): the same native 13px `accent-color` checkbox vocabulary as the row's Keep running control. There is still no custom checkbox graphic anywhere in the system.
- **Select** (Open in Editor uses): a native `<select>` on `--bg-raised` with a hairline border. A fixed five-item allowlist — Visual Studio Code (default), Cursor, Zed, Sublime Text, Finder only — never a free-text field, because the value is passed to `open_project` (docs/IPC.md v1.3) and a program name the user typed is exactly what the core must never be handed.
- **Footers:** hairline-separated, `--label-2`/`--label-3` only. Settings carries the app name and version plus "Portside never touches the network."; Help carries "Made with care. Free and open source." and a deliberately **disabled** sponsor row — inert until there is a real destination, so it never reads as a link that goes nowhere.

Help's copy is sourced from CONTEXT.md's glossary rather than written fresh: one plain line each for responding, not responding, unknown, unattended, "on your network", kept, and watch only, plus the Watch Only reasoning for why some servers cannot be stopped here.

### Illustration slots
Two reserved containers — the top of the Help page (`assets/help-header.png`, **686×216 @2x → 343×108**) and the empty state (`assets/empty-harbor.png`, 480×360 @2x → 240×180) — for the user's 3D-clay nautical artwork.

The header is sized to the page's content column rather than bled to the panel edges, and the number is measured rather than assumed: the Help page scrolls, so the 9px scrollbar reduces `#panel-body`'s content box to 371px, and `.page`'s 14px padding either side leaves 343px. A full-bleed 380px slot would have been clipped on the right by `overflow-x: hidden` — and would have meant the user producing artwork to a spec the panel could not honor.

**A slot with no asset occupies nothing.** The container starts `hidden` and the file is probed with a detached `Image()`; the `<img>` is appended and the container unhidden only once it has decoded. That ordering is the whole design: inserting the image first and hiding it on error would flash a broken-image glyph, and reserving height ahead of the load would shift the layout when the file is absent. The empty state additionally drops its check glyph when its artwork arrives, so the two never say the same thing twice.

### Toast
A single transient `--bg-raised` card, bottom-anchored, `--shadow-popover`, auto-dismissing after 4 seconds. No holder or accent coloring — it carries no status meaning of its own, only a plain-language result message.

## Do's and Don'ts

### Do:
- **Do** ship the health word next to the dot always — color is never the only carrier of status.
- **Do** recede a row by naming individual elements (`--label-3`), never by setting `opacity` on the row container (The Recede-By-Element-Color Rule).
- **Do** keep `.health`'s word, `.badge-network` and `.badge-pressure` at full strength under every recede state, in both modes.
- **Do** render the network and resource-pressure badges as transparent hairline chips on the row's own ground, never a filled tint.
- **Do** put the words in the resource-pressure badge ("High CPU"), and keep it `--caution` — never `--danger`, which would read as a demand to act on something Portside itself never acts on.
- **Do** make Cancel the rightmost, accent-filled default button in every confirmation dialog, and the destructive action bordered and never rightmost (The Cancel-Is-Default Rule).
- **Do** confine monospace to port numbers, the nerd grid, telemetry, and the source-menu path; everything else is system sans, sentence case.
- **Do** author light and dark as two first-class renditions, both verified ≥4.5:1 on their real composited grounds.
- **Do** treat vibrancy alphas (currently 0.72 panel / 0.6 chrome) as provisional until checked against the real compiled window.
- **Do** define every color — vibrancy grounds included — in all three theme blocks, and keep the two dark blocks byte-identical with `[data-theme="dark"]` below the media query (The CSS-Decides-The-Theme Rule).
- **Do** narrow `color-scheme` on the explicit themes, so the engine-drawn controls follow the panel rather than the OS.
- **Do** keep an illustration slot at zero height until its asset actually decodes.

### Don't:
- **Don't** add a `matchMedia` listener to track the OS theme — System mode is CSS-only by design, and JS could only re-derive what the media query already resolves.
- **Don't** restore the list's scroll position while a page is open; `#panel-body` is shared, and doing so scrolls the page the user is reading.
- **Don't** accent-fill the Appearance segmented control — the accent is reserved for pressed toggles and the dialog default.
- **Don't** let the editor preference reach the core as anything but one of the four documented ids; it selects a hardcoded chain and is never a program name.
- **Don't** reintroduce anything from the rejected "ATC strip board" world: no paper strips, no holder-edge color coding, no cocked/rotated alarm state, no letterspaced stamps, no beige.
- **Don't** dim a receding row (or any subtree) with container `opacity` — it composites the whole subtree into one layer, so a child cannot opt back out. (This does not prohibit `opacity` as a hover/focus reveal device, which `.row-actions` uses legitimately.)
- **Don't** give a watch-only row a disabled Stop button as a stand-in for "no action" — the layout has no action slot for that row kind at all.
- **Don't** offer "go to source" on a row whose project path is null, or on a watch-only row — both would either lie or point nowhere.
- **Don't** letterspace or uppercase section titles or button labels; sentence case throughout, with the panel title's −0.01em as the system's one tracking value.
