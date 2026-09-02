// Renders a Snapshot (docs/IPC.md) into #panel-body.
//
// render() rebuilds the list from the snapshot; main.js saves and restores
// scrollTop around the call, so a servers:changed event arriving mid-glance does
// not throw the user's place away.
//
// Ephemeral per-row state (is a stop in flight, is Force Stop now offered, is
// this row's nerd block expanded) lives in rowState, keyed by id, NOT in the
// DOM — because a re-render recreates a row's contents and that state must
// survive it. rowState is exported so main.js can update it around stop/force
// calls and trigger a re-render.

import { icon } from "./icons.js";
import { emptyStateSlot } from "./pages.js";

/** @type {Map<string, {stopPending?: boolean, forceEligible?: boolean, stillRunningMessage?: string, keepRunningPending?: boolean, expanded?: boolean}>} */
export const rowState = new Map();

function stateFor(id) {
  if (!rowState.has(id)) rowState.set(id, {});
  return rowState.get(id);
}

// Drops ephemeral state for ids no longer present anywhere in the snapshot, so a
// stale forceEligible flag can't resurrect a Force button if an id is ever
// reused (e.g. a restarted server that happens to land on the same pid+port).
//
// All three arrays are walked, watchOnly included. Watch Only ids carry no
// forceEligible — there is no stop flow for them — but they do carry `expanded`,
// and omitting them here would delete a watch-only row's expand state on the
// very next paint, making its disclosure control look broken.
function pruneRowState(snapshot) {
  const liveIds = new Set();
  for (const group of snapshot.projects) {
    for (const server of group.servers) liveIds.add(server.id);
  }
  for (const other of snapshot.others) liveIds.add(other.id);
  for (const watch of snapshot.watchOnly) liveIds.add(watch.id);
  for (const id of rowState.keys()) {
    if (!liveIds.has(id)) rowState.delete(id);
  }
}

// Global "stats for nerds" density. Owned here because render() is the only
// reader; main.js sets it from localStorage at boot and on every toggle.
let nerdMode = false;

export function setNerdMode(on) {
  nerdMode = on;
}

export function isNerdMode() {
  return nerdMode;
}

function formatUptime(totalSeconds) {
  const days = Math.floor(totalSeconds / 86400);
  const hours = Math.floor((totalSeconds % 86400) / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${minutes}m`;
  if (minutes > 0) return `${minutes}m`;
  return "<1m";
}

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function iconButton(name, { label, danger = false, size = 13 } = {}) {
  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "icon-button" + (danger ? " is-danger" : "");
  btn.title = label;
  btn.setAttribute("aria-label", label);
  btn.appendChild(icon(name, size));
  return btn;
}

// ---------- Row pieces ----------

/** The port cell. A Server can hold several ports (CONTEXT.md) — the first is
 * shown and the rest counted beside it, with the full list always available in
 * the row's nerd block. */
function buildPort(ports) {
  const cell = el("span", "port");
  if (ports.length === 0) {
    cell.textContent = "—";
    return cell;
  }
  cell.textContent = String(ports[0].number);
  if (ports.length > 1) {
    const more = el("span", "port-more", `+${ports.length - 1}`);
    more.title = ports.map((p) => `${p.number}/${p.family}`).join(", ");
    cell.appendChild(more);
  }
  return cell;
}

/** Health: the dot never travels without the word. */
function buildHealth(health) {
  const wrap = el("span", `health health-${health}`);
  wrap.appendChild(el("span", "health-dot"));
  const word =
    health === "responding"
      ? "responding"
      : health === "not_responding"
        ? "not responding"
        : "unknown";
  wrap.appendChild(el("span", null, word));
  return wrap;
}

/** "on your network" — a security signal, and the user's only easy way to
 * notice it. Rendered once per row even when several ports are exposed: the
 * fact the user acts on is that this Server is reachable, not how many of its
 * addresses are. */
function buildNetworkBadge(ports) {
  if (!ports.some((p) => p.reachability === "all_interfaces")) return null;
  const badge = el("span", "badge-network");
  badge.appendChild(icon("global", 10));
  badge.appendChild(el("span", null, "on your network"));
  badge.title = "Reachable by other machines on the network";
  return badge;
}

/** Per-port line for the nerd block: `4399/v4 localhost`.
 * Family is annotated on every port, uniformly — not only when it disambiguates.
 * Selective annotation would require duplicate-detection, which is exactly the
 * thing CONTEXT.md/REQUIREMENTS.md say never to do (two servers can legitimately
 * share a number). */
function portLines(ports) {
  return ports.map((p) => `${p.number}/${p.family} ${p.reachability}`).join("\n");
}

/** The nerd block: Snapshot fields already on hand. Nothing here is fetched or
 * derived — docs/IPC.md is the whole source.
 * @param {[string, string][]} rows */
function buildNerds(rows) {
  const grid = document.createElement("dl");
  grid.className = "nerds";
  for (const [label, value] of rows) {
    grid.appendChild(el("dt", null, label));
    grid.appendChild(el("dd", null, value));
  }
  return grid;
}

/** Disclosure control for one row's nerd block, independent of the global
 * toggle. State lives in rowState so it survives the re-render the click
 * triggers. */
function buildExpandButton(id, expanded, onToggle) {
  const btn = iconButton(expanded ? "down" : "right", {
    label: expanded ? "Hide details" : "Show details",
    size: 10,
  });
  btn.setAttribute("aria-expanded", String(expanded));
  btn.addEventListener("click", () => {
    stateFor(id).expanded = !expanded;
    onToggle();
  });
  return btn;
}

// Shared by dev-server rows and Other rows: the "didn't stop, here's why,
// here's Force Stop" note shown once a stop attempt has returned still_running.
// Displays StopOutcome.message verbatim underneath the plain-language lead —
// that message is the one place in this flow where the *result* text (as
// opposed to the pre-confirmation text, which IPC.md has no command for) is
// meant to reach the screen.
function buildStillRunningNote(state, onForceStopRequested) {
  const note = el("div", "still-running-note");
  const lead = el("span", "still-running-lead");
  lead.appendChild(icon("warning", 11));
  lead.appendChild(el("span", null, "Didn't stop."));
  note.appendChild(lead);
  if (state.stillRunningMessage) {
    note.appendChild(el("span", "still-running-detail", state.stillRunningMessage));
  }
  const forceBtn = document.createElement("button");
  forceBtn.type = "button";
  forceBtn.className = "force-button";
  forceBtn.textContent = "Force Stop…";
  forceBtn.addEventListener("click", onForceStopRequested);
  note.appendChild(forceBtn);
  return note;
}

/** What This Stops, in plain language, for the pre-confirmation dialog.
 * IPC.md's StopOutcome.message is a *result* string (used after a stop attempt,
 * e.g. still_running) — there is no command that previews this before confirming,
 * so it is composed here from Snapshot fields already on hand. Never falls back
 * to command or pid: those are exactly what CONTEXT.md's "What This Stops" says
 * never to show. */
function whatThisStopsForDevServer(projectName, server) {
  const scope = server.package ? `${projectName} · ${server.package}` : projectName;
  const named = server.title || scope;
  return `This stops ${named}${server.title ? ` (${scope})` : ""}. Its ports will be released.`;
}

function whatThisStopsForOther(other) {
  if (other.kind === "part_of_app") {
    return `This quits ${other.label}, including any unsaved work in it.`;
  }
  return `This stops ${other.label}, which is holding up other things — not just this one address.`;
}

// ---------- Dev server row ----------

function buildServerRow(projectName, server, handlers, onToggleExpand) {
  const row = el("div", "row");
  row.dataset.id = server.id;

  const body = el("div", "row-body");

  const headline = el("div", "row-headline");
  headline.appendChild(buildPort(server.ports));
  const hasTitle = Boolean(server.title);
  const title = el(
    "span",
    "row-title" + (hasTitle ? "" : " is-command"),
    server.title || server.command,
  );
  title.title = server.title || server.command;
  headline.appendChild(title);
  body.appendChild(headline);

  const meta = el("div", "row-meta");
  meta.appendChild(buildHealth(server.health));
  meta.appendChild(el("span", "meta-text", formatUptime(server.uptimeSeconds)));

  if (server.unattended) {
    // Quiet, secondary hint — normal for agent-started servers, not an error.
    const unattended = el("span", "meta-text", "unattended");
    unattended.title = "The program that started this server has exited";
    meta.appendChild(unattended);
  }

  const network = buildNetworkBadge(server.ports);
  if (network) meta.appendChild(network);

  if (server.keepRunning) {
    // The row recedes when kept, and a faded row with no stated reason reads as
    // broken rather than as deliberate. This names the reason in the row itself, so
    // the explanation does not depend on hovering the control that caused it.
    const kept = el("span", "kept-tag", "kept");
    kept.title = "You marked this one to keep running";
    meta.appendChild(kept);
  }

  body.appendChild(meta);

  const state = stateFor(server.id);
  if (nerdMode || state.expanded) {
    body.appendChild(
      buildNerds([
        ["pid", String(server.pid)],
        ["cmd", server.command],
        ["pkg", server.package ?? "—"],
        ["path", server.projectPath ?? "—"],
        ["ports", portLines(server.ports)],
        ["uptime", `${server.uptimeSeconds}s`],
        ["health", server.health],
        ["unattended", String(server.unattended)],
        ["keep", String(server.keepRunning)],
        ["id", server.id],
      ]),
    );
  }

  // Gated on forceEligible, not stopPending: stopPending goes false the moment
  // the still_running outcome arrives, but IPC.md says force_stop is only valid
  // after stop_server returned still_running — forceEligible is exactly that flag.
  if (state.forceEligible) {
    body.appendChild(
      buildStillRunningNote(state, () =>
        handlers.onForceStopRequested(server, projectName),
      ),
    );
  }

  row.appendChild(body);

  const actions = el("div", "row-actions");
  // Controls are hover-revealed, but a row mid-stop or already expanded must
  // stay visible without a pointer resting on it.
  if (state.stopPending || state.expanded) actions.classList.add("is-pinned");

  actions.appendChild(
    buildExpandButton(server.id, Boolean(state.expanded), onToggleExpand),
  );

  // F11 "go to source" — dev servers only. Offered only when the Project root is
  // actually known; a null projectPath means the tool cannot say where this
  // lives, and an action that opens nothing would be a lie.
  if (server.projectPath) {
    const sourceBtn = iconButton("folder-open", { label: "Go to source" });
    sourceBtn.addEventListener("click", () =>
      handlers.onGoToSourceRequested(server, sourceBtn),
    );
    actions.appendChild(sourceBtn);
  }

  // "Keep running", not "Keep": on its own "Keep" reads as an imperative aimed at the
  // row ("keep this?") rather than naming the state it sets. The tooltip says what the
  // mark MEANS rather than restating the label, since the label alone cannot carry
  // CONTEXT.md's "Keep Running" idea — that the user is telling the tool this one is
  // deliberate, not asking it to do anything.
  const KEEP_EXPLANATION =
    "You meant to leave this on — Portside won't draw attention to it";
  const keepLabel = el("label", "keep-toggle");
  keepLabel.title = KEEP_EXPLANATION;
  const keepInput = document.createElement("input");
  keepInput.type = "checkbox";
  keepInput.checked = server.keepRunning;
  keepInput.disabled = Boolean(state.keepRunningPending);
  // On the label rather than the input so the accessible name covers the whole
  // control, and explicit because the visible text ("Keep running") names the state
  // without explaining it.
  keepInput.setAttribute("aria-label", `Keep running — ${KEEP_EXPLANATION}`);
  keepInput.addEventListener("change", () =>
    handlers.onKeepRunningToggled(server, keepInput.checked),
  );
  keepLabel.appendChild(keepInput);
  keepLabel.appendChild(el("span", null, "Keep running"));
  actions.appendChild(keepLabel);

  const stopBtn = iconButton("stop", {
    label: state.stopPending ? "Stopping…" : "Stop…",
    danger: true,
  });
  stopBtn.disabled = Boolean(state.stopPending);
  stopBtn.addEventListener("click", () =>
    handlers.onStopRequested(server, projectName),
  );
  actions.appendChild(stopBtn);

  row.appendChild(actions);

  if (server.health === "not_responding") row.classList.add("is-not-responding");
  if (server.keepRunning) row.classList.add("is-kept");

  return row;
}

// ---------- Watch Only row: deliberately no stop branch anywhere in this
// function. There is no code path in here that could grow a button later by
// accident — the layout has no action slot at all, only the disclosure control,
// which reveals what the row already carries and stops nothing. It also gets no
// "go to source": a WatchOnlyServer has no Project in docs/IPC.md. ----------

function buildWatchOnlyRow(server, onToggleExpand) {
  const row = el("div", "row");
  row.dataset.id = server.id;

  const body = el("div", "row-body");

  const headline = el("div", "row-headline");
  headline.appendChild(buildPort(server.ports));
  headline.appendChild(el("span", "row-title", server.label));
  body.appendChild(headline);

  const meta = el("div", "row-meta");
  meta.appendChild(
    el(
      "span",
      "watch-reason",
      server.reason === "part_of_macos" ? "part of macOS" : "your tool",
    ),
  );
  meta.appendChild(el("span", "meta-text", formatUptime(server.uptimeSeconds)));
  const network = buildNetworkBadge(server.ports);
  if (network) meta.appendChild(network);
  body.appendChild(meta);

  const state = stateFor(server.id);
  if (nerdMode || state.expanded) {
    // Only what a WatchOnlyServer actually carries (docs/IPC.md) — no pid, no
    // command, no health: this type has none, and inventing them would be a lie.
    body.appendChild(
      buildNerds([
        ["ports", portLines(server.ports)],
        ["uptime", `${server.uptimeSeconds}s`],
        ["reason", server.reason],
        ["id", server.id],
      ]),
    );
  }

  row.appendChild(body);

  // The disclosure control is the whole of this row's action slot. Appended
  // directly, not via a shared actions builder, so no future edit to the
  // dev-server action row can leak a stop control in here.
  const disclosure = el("div", "row-actions");
  if (state.expanded) disclosure.classList.add("is-pinned");
  disclosure.appendChild(
    buildExpandButton(server.id, Boolean(state.expanded), onToggleExpand),
  );
  row.appendChild(disclosure);

  return row;
}

// ---------- Other row (part_of_app / background_service) ----------

function buildOtherRow(other, handlers, onToggleExpand) {
  const row = el("div", "row");
  row.dataset.id = other.id;

  const body = el("div", "row-body");

  const headline = el("div", "row-headline");
  headline.appendChild(buildPort(other.ports));
  headline.appendChild(el("span", "row-title", other.label));
  body.appendChild(headline);

  const meta = el("div", "row-meta");
  meta.appendChild(
    el(
      "span",
      "meta-text",
      other.kind === "part_of_app" ? "part of an app" : "background service",
    ),
  );
  const network = buildNetworkBadge(other.ports);
  if (network) meta.appendChild(network);
  body.appendChild(meta);

  if (other.guessedProject) {
    // Hedged in the words themselves, not only in styling — styling alone isn't
    // honest at high contrast / for screen readers. This uncertainty is also
    // exactly why these rows get no "go to source" (F11): opening a folder on a
    // guess would present the guess as fact.
    body.appendChild(
      el(
        "div",
        "guessed-project",
        `possibly part of ${other.guessedProject} (uncertain)`,
      ),
    );
  }

  const state = stateFor(other.id);
  if (nerdMode || state.expanded) {
    // An OtherServer carries no pid, uptime or health in docs/IPC.md — this is
    // everything the type has.
    body.appendChild(
      buildNerds([
        ["ports", portLines(other.ports)],
        ["kind", other.kind],
        ["guess", other.guessedProject ?? "—"],
        ["id", other.id],
      ]),
    );
  }

  if (state.forceEligible) {
    body.appendChild(
      buildStillRunningNote(state, () => handlers.onOtherForceStopRequested(other)),
    );
  }

  row.appendChild(body);

  const actions = el("div", "row-actions");
  if (state.stopPending || state.expanded) actions.classList.add("is-pinned");
  actions.appendChild(
    buildExpandButton(other.id, Boolean(state.expanded), onToggleExpand),
  );
  const stopBtn = iconButton("stop", {
    label: state.stopPending ? "Stopping…" : "Stop…",
    danger: true,
  });
  stopBtn.disabled = Boolean(state.stopPending);
  stopBtn.addEventListener("click", () => handlers.onOtherStopRequested(other));
  actions.appendChild(stopBtn);
  row.appendChild(actions);

  return row;
}

// ---------- Empty state (dev servers only — others/watchOnly still show) ----------

function buildEmptyState(scanFailed) {
  const wrap = el("div", "empty-state");
  // An empty list means two completely different things depending on whether the
  // scan succeeded (docs/IPC.md v1.2). "Nothing running — that's the good outcome"
  // is a claim about the machine, and making it after a scan that never ran would
  // be a fabricated fact of exactly the kind N3 forbids. So a failed scan gets its
  // own words, and notably NOT the reassuring green check.
  if (scanFailed) {
    const glyph = icon("warning", 26);
    glyph.classList.add("empty-state-icon", "is-muted");
    wrap.appendChild(glyph);
    wrap.appendChild(el("div", "empty-state-title", "Couldn't check just now"));
    wrap.appendChild(
      el(
        "div",
        "empty-state-body",
        "Portside couldn't look at what's running. This isn't a report that nothing is.",
      ),
    );
    return wrap;
  }

  // The illustration slot sits where the check glyph is, and replaces it when the
  // asset exists — two marks saying the same thing would be one too many. Until
  // then the slot occupies nothing and the glyph carries the state alone.
  const slot = emptyStateSlot();
  wrap.appendChild(slot);
  const glyph = icon("check-circle", 26);
  glyph.classList.add("empty-state-icon");
  slot.addEventListener("portside:asset-loaded", () => glyph.remove(), { once: true });
  wrap.appendChild(glyph);
  wrap.appendChild(el("div", "empty-state-title", "All clear"));
  wrap.appendChild(
    el(
      "div",
      "empty-state-body",
      "No development servers are running right now. Nothing to tidy up.",
    ),
  );
  return wrap;
}

function buildSectionHeader(title) {
  const header = el("div", "section-header");
  header.appendChild(el("span", "section-title", title));
  return header;
}

/**
 * @param {import('./mock.js').Snapshot} snapshot
 * @param {HTMLElement} root
 * @param {{
 *   onStopRequested: (server, projectName) => void,
 *   onForceStopRequested: (server, projectName) => void,
 *   onKeepRunningToggled: (server, keep: boolean) => void,
 *   onOtherStopRequested: (other) => void,
 *   onOtherForceStopRequested: (other) => void,
 *   onStopAllRequested: () => void,
 *   onGoToSourceRequested: (server, anchorEl) => void,
 *   onRerenderRequested: () => void,
 * }} handlers
 */
export function render(snapshot, root, handlers) {
  root.textContent = "";
  pruneRowState(snapshot);

  const onToggleExpand = handlers.onRerenderRequested;

  const totalDevServers = snapshot.projects.reduce(
    (sum, g) => sum + g.servers.length,
    0,
  );

  // ---- Development servers ----
  const devSection = el("section", "section");

  const devHeader = buildSectionHeader("Development servers");
  if (totalDevServers > 0) {
    const stopAll = document.createElement("button");
    stopAll.type = "button";
    stopAll.className = "stop-all-button";
    // Names its own scope so the constraint holds even once this header has
    // scrolled away from the Other / Watch only sections below it.
    stopAll.textContent = "Stop all dev servers";
    stopAll.title = "Stops every development server. Does not touch anything else.";
    stopAll.addEventListener("click", () => handlers.onStopAllRequested());
    devHeader.appendChild(stopAll);
  }
  devSection.appendChild(devHeader);

  if (totalDevServers === 0) {
    devSection.appendChild(buildEmptyState(Boolean(snapshot.scanFailed)));
  } else {
    for (const group of snapshot.projects) {
      // Defensive: a real backend should never send a ProjectGroup with no
      // servers, but nothing in IPC.md forbids it, and rendering a bare
      // project heading with nothing under it is a visible bug either way.
      if (group.servers.length === 0) continue;

      const groupEl = el("div", "project-group");
      groupEl.appendChild(el("div", "project-heading", group.project));

      const list = el("div", "row-list");
      for (const server of group.servers) {
        list.appendChild(
          buildServerRow(group.project, server, handlers, onToggleExpand),
        );
      }
      groupEl.appendChild(list);
      devSection.appendChild(groupEl);
    }
  }
  root.appendChild(devSection);

  // ---- Other (part of app / background service) ----
  if (snapshot.others.length > 0) {
    const otherSection = el("section", "section");
    otherSection.appendChild(buildSectionHeader("Other"));
    const list = el("div", "row-list");
    for (const other of snapshot.others) {
      list.appendChild(buildOtherRow(other, handlers, onToggleExpand));
    }
    otherSection.appendChild(list);
    root.appendChild(otherSection);
  }

  // ---- Watch only ----
  if (snapshot.watchOnly.length > 0) {
    const watchSection = el("section", "section");
    const header = buildSectionHeader("Watch only");
    const hint = el("span", "section-hint");
    hint.appendChild(icon("eye", 11));
    hint.appendChild(el("span", null, "not stoppable here"));
    header.appendChild(hint);
    watchSection.appendChild(header);

    const list = el("div", "row-list");
    for (const server of snapshot.watchOnly) {
      list.appendChild(buildWatchOnlyRow(server, onToggleExpand));
    }
    watchSection.appendChild(list);
    root.appendChild(watchSection);
  }
}

/** The title bar's live count, and the Stats telemetry line. Both read the same
 * snapshot render() just drew, so they can never describe a different list than
 * the one on screen. Cadence wording is the contract in docs/IPC.md:
 * panel_opened -> 3s, panel_closed -> 15s. */
export function renderServerCount(snapshot, countEl) {
  const dev = snapshot.projects.reduce((sum, g) => sum + g.servers.length, 0);
  // docs/IPC.md v1.2. When the last scan failed, the count below describes the last
  // snapshot that succeeded, not what is running now — so it is replaced rather than
  // annotated. Stating a number and a doubt at once would leave the number looking
  // like current fact, which is the N3 violation this flag exists to prevent. Plain
  // words, in the count's own quiet slot: no colour, no icon, no interruption (F7).
  countEl.classList.toggle("is-scan-failed", Boolean(snapshot.scanFailed));
  if (snapshot.scanFailed) {
    countEl.textContent = "couldn't scan just now";
    countEl.title = "Showing what was running at the last successful scan.";
    return;
  }
  countEl.removeAttribute("title");
  countEl.textContent = dev === 1 ? "1 server" : `${dev} servers`;
}

export function renderTelemetry(snapshot, footerEl) {
  if (!nerdMode) {
    footerEl.hidden = true;
    footerEl.textContent = "";
    return;
  }
  const dev = snapshot.projects.reduce((sum, g) => sum + g.servers.length, 0);
  const scanned = new Date(snapshot.scannedAt);
  const time = Number.isNaN(scanned.getTime())
    ? snapshot.scannedAt
    : scanned.toLocaleTimeString([], {
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
        hour12: false,
      });
  footerEl.hidden = false;
  // The scan time is the last SUCCESSFUL one when scanFailed is set, so it is labelled
  // as such rather than reading as a scan that just happened.
  const scanLabel = snapshot.scanFailed ? `scan failed · last ok ${time}` : `scan ${time}`;
  footerEl.textContent = `${scanLabel} · ${dev} dev / ${snapshot.others.length} other / ${snapshot.watchOnly.length} watch · 3s open / 15s closed`;
}

export { whatThisStopsForDevServer, whatThisStopsForOther, formatUptime };
