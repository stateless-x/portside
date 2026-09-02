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
import { emptyStateSlot, friendlyGuideSlot } from "./pages.js";

/** @type {Map<string, {stopPending?: boolean, forceEligible?: boolean, stillRunningMessage?: string, keepRunningPending?: boolean, expanded?: boolean, showName?: boolean}>} */
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

// ---------- Resource usage (docs/IPC.md v1.4) ----------

/** Every place the panel explains what a CPU or memory figure covers. One constant,
 * because the scope is the honest part: the number is for the listed process alone,
 * and a user who assumes it includes the worker processes their dev server spawned
 * would read it as much lower than the machine's real load. */
export const RESOURCE_SCOPE_NOTE =
  "Measured for this process at the latest scan. Related child processes are not included.";

/** How many logical CPUs this Mac has, or null when the browser will not say.
 *
 * Read ONCE at module load, not per render: it cannot change while the panel is open,
 * and it is only ever used to translate a figure the backend already measured.
 * `hardwareConcurrency` is absent in some webviews and can report 0, both of which
 * must degrade to "don't translate" rather than to a division by zero. */
const LOGICAL_CPUS = (() => {
  const n = navigator.hardwareConcurrency;
  return Number.isFinite(n) && n > 0 ? n : null;
})();

/** "3% CPU", "82% CPU", "276% CPU" — Activity Monitor's convention, where 100% is one
 * fully used core, so a process on several cores exceeds 100%. Kept raw on purpose:
 * this is the number a developer can compare against Activity Monitor directly.
 *
 * Whole percent below 10 gets one decimal, because the difference between 0.4% and 3%
 * is the difference between "asleep" and "working", while the difference between 82%
 * and 82.4% is noise. Null is "not available" — never 0%, which would be a measurement
 * the tool did not make. */
function formatCpu(cpuPercent) {
  if (cpuPercent === null || cpuPercent === undefined) return null;
  const rounded = cpuPercent < 10 ? Math.round(cpuPercent * 10) / 10 : Math.round(cpuPercent);
  return `${rounded}% CPU`;
}

/** The sentence that makes a >100% reading make sense, e.g. "100% equals one CPU core.
 * 276% is about 2.8 cores." Only worth saying when the figure actually exceeds one
 * core — below that it explains nothing the number did not already say. */
function cpuCoresNote(cpuPercent) {
  if (cpuPercent === null || cpuPercent === undefined || cpuPercent <= 100) return null;
  const cores = Math.round((cpuPercent / 100) * 10) / 10;
  return `100% equals one CPU core. ${Math.round(cpuPercent)}% is about ${cores} cores.`;
}

/** The same measurement as a share of the WHOLE machine: 276% on a 10-core Mac is 28%
 * of everything it has.
 *
 * This is the figure for the Quick read, where the reader is asking "is this a problem"
 * rather than "what does Activity Monitor say" — 276% reads alarming until you know how
 * many cores there are, and 28% answers the question directly. Returns null when the
 * core count is unavailable, so the caller falls back to the raw wording rather than
 * inventing a denominator. */
function cpuShareOfMachine(cpuPercent) {
  if (cpuPercent === null || cpuPercent === undefined || !LOGICAL_CPUS) return null;
  return Math.round(cpuPercent / LOGICAL_CPUS);
}

/** "84 MB", "1.2 GB". Binary divisors throughout (1024, not 1000) — this is memory,
 * and it is what every other tool on the Mac showing a resident set reports. The
 * labels stay MB/GB rather than MiB/GiB deliberately: CONTEXT.md's rule is that the
 * plain word wins, and the audience reads MB. Null is "not available". */
function formatMemory(memoryBytes) {
  if (memoryBytes === null || memoryBytes === undefined) return null;
  const KB = 1024;
  const MB = KB * 1024;
  const GB = MB * 1024;
  // Round FIRST, then decide the unit. Picking the tier from the raw value and
  // rounding inside it lets a figure just under a boundary round up past it and print
  // in the smaller unit — 1 GiB minus one byte became "1024 MB", which is not a size
  // anyone writes. Each tier is therefore only used while its own rounded value still
  // fits below the next boundary.
  const rounded = (value, decimals) => {
    const factor = 10 ** decimals;
    return Math.round(value * factor) / factor;
  };
  // One decimal at GB scale: 1.2 GB and 1.9 GB are meaningfully different sizes, and
  // rounding both to "2 GB" would throw away the distinction the user reads.
  if (rounded(memoryBytes / GB, 1) >= 1) return `${rounded(memoryBytes / GB, 1)} GB`;
  if (rounded(memoryBytes / MB, 0) >= 1) return `${rounded(memoryBytes / MB, 0)} MB`;
  if (rounded(memoryBytes / KB, 0) >= 1) return `${rounded(memoryBytes / KB, 0)} KB`;
  return `${memoryBytes} B`;
}

/** The words for a sustained-pressure badge, or null when nothing is elevated.
 * Words, never colour alone — the same rule as the health dot's word. */
function pressureWords(pressure) {
  if (pressure === "cpu") return "High CPU";
  if (pressure === "memory") return "High memory";
  if (pressure === "both") return "High CPU and memory";
  return null;
}

/** The amber pressure badge, or null on a normal row.
 *
 * Caution vocabulary, never danger red: high usage is something to notice, not
 * something wrong. It is observational only — nothing about this badge changes what
 * stopping the Server does, and Portside never acts on it.
 *
 * Deliberately NOT hidden on a kept row: the user marked the Server to be left alone,
 * which does not make what it is consuming irrelevant. (`.is-kept`'s recede names each
 * receding element individually, so this badge holds full strength without needing an
 * exemption — see the Recede-By-Element-Color Rule in DESIGN.md.) */
function buildPressureBadge(usage) {
  const words = pressureWords(usage?.pressure);
  if (!words) return null;
  const badge = el("span", "badge-pressure");
  badge.appendChild(el("span", null, words));
  // States the actual duration rather than "the last few scans": the cadence varies
  // (3s open, 15s closed, 60s idle), so a count of scans is not a length of time and
  // saying it that way would be vague where the rule is exact.
  const held =
    usage.pressure === "cpu"
      ? "CPU usage stayed high for at least 30 seconds."
      : usage.pressure === "memory"
        ? "Memory usage stayed high for at least 10 seconds."
        : "CPU usage stayed high for at least 30 seconds, and memory for at least 10 seconds.";
  badge.title = `${held} ${RESOURCE_SCOPE_NOTE}`;
  return badge;
}

/** The Quick read's usage sentence, in the reader's terms rather than Activity
 * Monitor's.
 *
 * CPU is stated as a share of the whole Mac ("about 28% of this Mac's CPU"), because
 * the question the summary answers is "should I worry", and a raw 276% reads alarming
 * until you know the machine has ten cores. When the core count is unavailable the raw
 * figure is used instead — a slightly harder number to read is better than a
 * denominator the panel guessed.
 *
 * Exported so main.js can rebuild exactly this string from a resources:changed sample
 * without re-rendering the list. */
export function quickReadUsageSentence(usage) {
  const share = cpuShareOfMachine(usage?.cpuPercent);
  const cpu = share !== null ? `about ${share}% of this Mac's CPU` : formatCpu(usage?.cpuPercent);
  const memory = formatMemory(usage?.memoryBytes);
  const figure =
    usage?.pressure === "memory"
      ? memory
      : usage?.pressure === "both"
        ? [cpu, memory].filter(Boolean).join(" and ")
        : cpu;
  return figure
    ? `It is using ${figure} right now. Portside will not stop it automatically.`
    : "It has been running heavy for a while. Portside will not stop it automatically.";
}

/** The exact-figures line: visible only when the row is expanded or Stats mode is on
 * (docs/IPC.md v1.4 / the product rule that normal usage adds nothing to the friendly
 * row). Every row type gets one, including rows whose figures are unavailable — an
 * absent line and an unmeasurable metric would otherwise look identical.
 *
 * The `data-resource` hooks are what `updateResources` patches in place, so a fresh
 * reading never costs a list rebuild. */
function buildResourceLine(usage) {
  const line = el("div", "resource-line");
  line.title = RESOURCE_SCOPE_NOTE;

  const cpu = el("span", "resource-figure");
  cpu.dataset.resource = "cpu";
  cpu.textContent = formatCpu(usage?.cpuPercent) ?? "CPU not available";
  // Only present above one core, where the raw number needs explaining. Attached as a
  // tooltip on the figure itself rather than as visible text, so the dense line stays
  // dense — the explanation is there for the reader who stops on the number.
  const cores = cpuCoresNote(usage?.cpuPercent);
  cpu.title = cores ? `${cores} ${RESOURCE_SCOPE_NOTE}` : RESOURCE_SCOPE_NOTE;

  const memory = el("span", "resource-figure");
  memory.dataset.resource = "memory";
  memory.textContent = formatMemory(usage?.memoryBytes) ?? "memory not available";

  line.appendChild(cpu);
  line.appendChild(memory);
  return line;
}

/** Apply fresh figures to rows already on screen (docs/IPC.md v1.4
 * `resources:changed`).
 *
 * Patches text nodes ONLY. It never adds, removes or reorders a row, and never
 * touches the badge — a badge appearing means the sustained PRESSURE verdict changed,
 * which arrives as `servers:changed` and gets a normal rebuild. That division is the
 * whole reason this function exists: a rebuild every few seconds would close open
 * disclosures, drop the user's hover and reset scroll, several times a minute.
 *
 * @param {import('./mock.js').ResourceSamples} samples
 * @param {HTMLElement} root
 */
export function updateResources(samples, root) {
  for (const sample of samples.samples ?? []) {
    // CSS.escape: an id is backend-issued ("1234:4399") and goes into a selector.
    const escaped = CSS.escape(sample.id);
    const row = root.querySelector(`[data-id="${escaped}"]`);
    if (row) {
      const cpu = row.querySelector('[data-resource="cpu"]');
      if (cpu) {
        cpu.textContent = formatCpu(sample.usage?.cpuPercent) ?? "CPU not available";
        const cores = cpuCoresNote(sample.usage?.cpuPercent);
        cpu.title = cores ? `${cores} ${RESOURCE_SCOPE_NOTE}` : RESOURCE_SCOPE_NOTE;
      }
      const memory = row.querySelector('[data-resource="memory"]');
      if (memory) memory.textContent = formatMemory(sample.usage?.memoryBytes) ?? "memory not available";
    }

    // The Quick read quotes a live figure for one specific server. Matching on the id
    // is what keeps it honest: a sample for any OTHER row leaves the summary alone
    // rather than rewriting it with a figure that belongs to something else.
    const summary = root.querySelector(`[data-resource-summary="${escaped}"]`);
    if (summary) summary.textContent = quickReadUsageSentence(sample.usage);
  }
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
  // A long process name is useful context, not an advanced-only detail. Making
  // the visible (possibly ellipsised) name a small disclosure keeps the list
  // scannable while giving every user a clear way to read it in full.
  const title = document.createElement("button");
  title.type = "button";
  title.className = "row-title" + (hasTitle ? "" : " is-command");
  title.textContent = server.title || server.command;
  const state = stateFor(server.id);
  title.title = state.showName ? "Hide full process name" : "Show full process name";
  title.setAttribute("aria-expanded", String(Boolean(state.showName)));
  title.setAttribute(
    "aria-label",
    `${state.showName ? "Hide" : "Show"} full process name: ${server.title || server.command}`,
  );
  title.addEventListener("click", () => {
    state.showName = !state.showName;
    onToggleExpand();
  });
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

  // Sustained high usage only. Normal usage adds nothing to the friendly row — its
  // figures live in the details below, and in Stats mode.
  const pressure = buildPressureBadge(server.usage);
  if (pressure) meta.appendChild(pressure);

  if (server.keepRunning) {
    // The row recedes when kept, and a faded row with no stated reason reads as
    // broken rather than as deliberate. This names the reason in the row itself, so
    // the explanation does not depend on hovering the control that caused it.
    const kept = el("span", "kept-tag", "kept");
    kept.title = "You marked this one to keep running";
    meta.appendChild(kept);
  }

  body.appendChild(meta);

  if (state.showName) {
    body.appendChild(el("div", "full-process-name", server.title || server.command));
  }

  if (nerdMode || state.expanded) {
    body.appendChild(buildResourceLine(server.usage));
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
        ["pressure", server.usage?.pressure ?? "—"],
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
  if (state.stopPending || state.expanded || state.showName) actions.classList.add("is-pinned");

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
  // A Watch Only row carries the badge like any other: the user opened this panel
  // partly to see that these are behaving, and "using a lot" is exactly the kind of
  // thing they would want to know about something they cannot stop from here. It adds
  // no control — this row still has no action slot at all.
  const pressure = buildPressureBadge(server.usage);
  if (pressure) meta.appendChild(pressure);
  body.appendChild(meta);

  const state = stateFor(server.id);
  if (nerdMode || state.expanded) {
    // Only what a WatchOnlyServer actually carries (docs/IPC.md) — no pid, no
    // command, no health: this type has none, and inventing them would be a lie.
    body.appendChild(buildResourceLine(server.usage));
    body.appendChild(
      buildNerds([
        ["ports", portLines(server.ports)],
        ["uptime", `${server.uptimeSeconds}s`],
        ["reason", server.reason],
        ["pressure", server.usage?.pressure ?? "—"],
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
  const pressure = buildPressureBadge(other.usage);
  if (pressure) meta.appendChild(pressure);
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
    body.appendChild(buildResourceLine(other.usage));
    body.appendChild(
      buildNerds([
        ["ports", portLines(other.ports)],
        ["kind", other.kind],
        ["guess", other.guessedProject ?? "—"],
        ["pressure", other.usage?.pressure ?? "—"],
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

/** Turns the scan into one calm, truthful sentence. It does not make a decision
 * for the user or expose a destructive shortcut; its job is only to explain what
 * deserves attention first. */
function buildQuickRead(snapshot, totalDevServers) {
  if (totalDevServers === 0) return null;

  const wrap = el("aside", "quick-read");
  wrap.setAttribute("aria-label", "Quick read of your servers");
  wrap.appendChild(friendlyGuideSlot());
  const copy = el("div", "quick-read-copy");
  copy.appendChild(el("div", "quick-read-label", "Quick read"));

  const servers = snapshot.projects.flatMap((group) => group.servers);
  const notResponding = servers.find((server) => server.health === "not_responding");
  // Sustained high usage ranks BELOW "not answering" — a server that has stopped
  // responding is broken, while one working hard is merely working hard. A kept
  // server is included: the user asked Portside not to nag them about it running,
  // which is not the same as not wanting to know it is eating a core.
  const heavy = servers.find((server) => server.usage && server.usage.pressure !== "normal");
  const unattended = servers.find((server) => server.unattended && !server.keepRunning);
  let title;
  let body;
  if (snapshot.scanFailed) {
    title = "Showing the last good scan";
    body = "Portside could not refresh just now. Try Refresh before making a decision.";
  } else if (notResponding) {
    title = `Port ${notResponding.ports[0]?.number ?? "—"} needs a look`;
    body = "It is still held but is not answering. Check it first; Portside will never stop it automatically.";
  } else if (heavy) {
    const port = heavy.ports[0]?.number ?? "—";
    const subject =
      heavy.usage.pressure === "memory"
        ? "a lot of memory"
        : heavy.usage.pressure === "both"
          ? "a lot of CPU and memory"
          : "a lot of CPU";
    title = `Port ${port} is using ${subject}`;
    // The reassurance matters as much as the fact: a warning that does not say the
    // tool will leave it alone reads as a prompt to act.
    body = quickReadUsageSentence(heavy.usage);
  } else if (unattended) {
    title = "A server is running on its own";
    body = "Its terminal or coding agent has closed. If you no longer need it, you can choose to stop it here.";
  } else {
    title = "Everything looks steady";
    body = `${totalDevServers} development ${totalDevServers === 1 ? "server is" : "servers are"} running. Green means they are answering.`;
  }
  copy.appendChild(el("div", "quick-read-title", title));
  const bodyEl = el("p", "quick-read-body", body);
  if (heavy && !snapshot.scanFailed && !notResponding) {
    // The one Quick read variant that quotes a live figure, so it is the one that goes
    // stale between structural events. Tagging it with the server it describes lets
    // updateResources rewrite this sentence in place — and ONLY when the sample it
    // receives is for that same server, so a different row's reading can never rewrite
    // someone else's summary.
    bodyEl.dataset.resourceSummary = heavy.id;
  }
  copy.appendChild(bodyEl);
  wrap.appendChild(copy);
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

  const quickRead = buildQuickRead(snapshot, totalDevServers);
  if (quickRead) devSection.appendChild(quickRead);

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
