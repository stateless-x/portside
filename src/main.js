import {
  panelOpened,
  panelClosed,
  refreshNow,
  setKeepRunning,
  stopServer,
  stopAllDevServers,
  forceStop,
  openProject,
  onServersChanged,
  onResourcesChanged,
} from "./api.js";
import {
  render,
  renderServerCount,
  renderTelemetry,
  updateResources,
  rowState,
  setNerdMode,
  isNerdMode,
  whatThisStopsForDevServer,
  whatThisStopsForOther,
} from "./render.js";
import { icon } from "./icons.js";
import { renderSettings, renderHelp, loadEditor } from "./pages.js";

// Vibrancy is opt-in, and only inside the real window. The Tauri window is
// transparent with an NSVisualEffectView behind it, so .panel tints over that
// material instead of painting an opaque ground; in a plain browser (mock
// backend, screenshots) there is no material to sit on, so the opaque grounds
// stay. Set here, before the first paint, so the panel never flashes opaque.
if (window.__TAURI__) document.body.classList.add("tauri");

// ---------- Theme (System / Light / Dark) ----------
// The CSS carries the whole strategy (see styles.css): System sets NO attribute and
// lets `prefers-color-scheme` decide, so the correct theme is painted by CSS on the
// first frame with no JS in the path and therefore no flash. This code's only job is
// to record the user's choice and stamp the attribute for the two explicit modes.
//
// Read and applied at module top, alongside the body.tauri gate above, for the same
// reason: both must land before the first paint.
const THEME_KEY = "portside.theme";
/** @typedef {"system"|"light"|"dark"} ThemeChoice */
const THEMES = ["system", "light", "dark"];

/** @returns {ThemeChoice} */
function loadTheme() {
  try {
    const stored = localStorage.getItem(THEME_KEY);
    return THEMES.includes(stored) ? stored : "system";
  } catch {
    // Same reasoning as the nerd-mode read below: a locked-down webview that
    // refuses storage should cost a preference, never the boot.
    return "system";
  }
}

function persistTheme(choice) {
  try {
    localStorage.setItem(THEME_KEY, choice);
  } catch {
    /* preference not persisted; the session still honours the choice */
  }
}

/** @param {ThemeChoice} choice */
function applyTheme(choice) {
  // System removes the attribute rather than setting one: the media-query half of
  // the CSS is what tracks the OS, and it keeps tracking it live — no matchMedia
  // listener is needed, and adding one could only ever re-derive what CSS already
  // knows.
  if (choice === "system") {
    document.documentElement.removeAttribute("data-theme");
  } else {
    document.documentElement.setAttribute("data-theme", choice);
  }
  syncWindowTheme(choice);
}

/** Ask the real window to match, so the vibrancy MATERIAL behind the panel flips
 * with the panel rather than staying on the OS appearance. Best-effort by design:
 *
 *   - Absent outside Tauri (a plain browser has no window to theme).
 *   - `setTheme` is gated behind the `core:window:allow-set-theme` capability
 *     permission, which this app's capabilities/default.json does NOT currently
 *     grant (it lists only `core:default` + `opener:default`, and core:window's
 *     default permission set excludes set-theme). Adding that permission is a
 *     capabilities change, deliberately out of scope here — so this is expected to
 *     reject in the real app until `core:window:allow-set-theme` is added.
 *
 * It returns a Promise, so the rejection is caught with .catch() — a try/catch
 * around the call alone would not see it. A failure costs the material match only;
 * the panel's own grounds have already flipped via data-theme. */
function syncWindowTheme(choice) {
  const api = window.__TAURI__?.window;
  if (!api?.getCurrentWindow) return;
  try {
    // null = follow the system, which is exactly what "system" means here.
    const result = api.getCurrentWindow().setTheme(choice === "system" ? null : choice);
    if (result?.catch) {
      result.catch((err) => console.warn("window setTheme unavailable:", err));
    }
  } catch (err) {
    console.warn("window setTheme unavailable:", err);
  }
}

let themeChoice = loadTheme();
applyTheme(themeChoice);

const body = document.getElementById("panel-body");
const listView = document.getElementById("list-view");
const settingsView = document.getElementById("settings-view");
const helpView = document.getElementById("help-view");
const overlayRoot = document.getElementById("overlay-root");
const refreshButton = document.getElementById("refresh-button");
const statsToggle = document.getElementById("stats-toggle");
const serverCount = document.getElementById("server-count");
const telemetry = document.getElementById("telemetry");

// Icons the static shell owns. Injected here rather than pasted into index.html
// so icons.js stays the single place any Ant path data lives.
refreshButton.appendChild(icon("reload", 13));
statsToggle.appendChild(icon("dashboard", 13));

// ---------- Stats for nerds ----------
// Read before the first paint so the panel never flashes friendly-then-dense.
// localStorage can throw in a locked-down webview; a lost preference is a far
// smaller failure than a panel that won't boot, so the read is guarded and the
// default (friendly) stands.
const NERD_KEY = "portside.nerdMode";

function loadNerdMode() {
  try {
    return localStorage.getItem(NERD_KEY) === "1";
  } catch {
    return false;
  }
}

function persistNerdMode(on) {
  try {
    localStorage.setItem(NERD_KEY, on ? "1" : "0");
  } catch {
    /* preference not persisted; the session still honours the toggle */
  }
}

// The ONE place the nerd-mode flag is applied, so the title bar toggle and the
// Settings switch can never disagree: both call this, and it updates render.js, the
// title bar's pressed state, and the Settings control if that page is mounted.
function applyNerdMode(on) {
  setNerdMode(on);
  statsToggle.setAttribute("aria-pressed", String(on));
  const settingsSwitch = settingsView.querySelector(".switch-row input");
  if (settingsSwitch) settingsSwitch.checked = on;
}

applyNerdMode(loadNerdMode());

function setNerdModeFromUser(on) {
  applyNerdMode(on);
  persistNerdMode(on);
  rerender();
}

statsToggle.addEventListener("click", () => setNerdModeFromUser(!isNerdMode()));

/** @type {import('./mock.js').Snapshot | null} */
let latestSnapshot = null;

// ---------- View state ----------
// Three views share #panel-body; only one is ever shown. The list is never torn
// down when a page opens — paint() keeps writing into #list-view underneath, so
// coming back shows current data with no refetch, and a servers:changed arriving
// while a page is open cannot yank the user out of it.

/** @type {"list"|"settings"|"help"} */
let view = "list";
/** The control that opened the current page, so focus can be handed back to it. */
let pageOpener = null;

function showView(next) {
  view = next;
  listView.hidden = next !== "list";
  settingsView.hidden = next !== "settings";
  helpView.hidden = next !== "help";
  // Each view owns its own scroll position; the shared scroller is reset on every
  // switch so a page never opens already scrolled from the list behind it.
  body.scrollTop = 0;
}

function goBack() {
  showView("list");
  // The list has been repainted underneath all along, but paint() skipped its
  // scroll restore while the page was open, so nothing here needs re-fetching.
  const opener = pageOpener;
  pageOpener = null;
  if (opener) opener.focus();
}

function openSettings() {
  // Opened from the tray menu, so there is no in-panel control to hand focus
  // back to; goBack() already handles a null opener.
  pageOpener = null;
  const focusTarget = renderSettings(settingsView, {
    theme: themeChoice,
    onThemeChange: (choice) => {
      themeChoice = choice;
      applyTheme(choice);
      persistTheme(choice);
    },
    nerdMode: isNerdMode(),
    onNerdModeChange: setNerdModeFromUser,
    onBack: goBack,
  });
  showView("settings");
  focusTarget.focus();
}

function openHelp() {
  pageOpener = null;
  const focusTarget = renderHelp(helpView, { onBack: goBack });
  showView("help");
  focusTarget.focus();
}

// Settings and Help open from the tray menu (lib.rs emits "navigate"; listener
// in the __TAURI__ block below) — the user chose the menu bar over titlebar
// buttons. ⌘, is the native Settings shortcut and ⌘? the native Help one; they
// also keep both pages reachable in the browser mock, which has no tray.
document.addEventListener("keydown", (e) => {
  if (!(e.metaKey || e.ctrlKey)) return;
  if (e.key === ",") {
    e.preventDefault();
    openSettings();
  } else if (e.key === "?" || e.key === "/") {
    e.preventDefault();
    openHelp();
  }
});

// Escape returns from a page — but only when nothing layered above it owns Escape.
// A dialog or the source menu carries its own Escape handler, and closing the page
// out from under one would be the wrong dismissal.
//
// Scoped to those two specifically, NOT to "is #overlay-root empty": a toast also
// lives there and lingers for 4 seconds, so the looser check would silently make
// Escape dead for 4s after any stop — a key that works only sometimes is worse than
// one that never works. A toast owns nothing and dismisses itself.
document.addEventListener("keydown", (e) => {
  if (e.key !== "Escape") return;
  if (view === "list") return;
  if (overlayRoot.querySelector(".dialog-overlay, .menu")) return;
  goBack();
});

/** Removes the open source menu and the listeners watching for its dismissal.
 * Declared here, above paint(), because paint() calls it on the very first
 * boot render — a `let` referenced before its declaration throws. */
let closeMenu = () => {};

// One entry point, fed identically by refresh_now()'s return value and every
// servers:changed payload, so the two paths can never draw the panel
// differently. Scroll position is saved/restored around the rebuild since
// replacing #panel-body's children clamps its scrollTop to 0.
function paint(snapshot) {
  latestSnapshot = snapshot;
  // A repaint replaces the row that anchors an open source menu, leaving the
  // menu floating beside a button that no longer exists. Dismiss it first.
  // Dialogs are deliberately NOT closed here — they are modal confirmations the
  // user is mid-decision on, which is exactly why they live outside .panel.
  closeMenu();
  // The list keeps painting while a page is open — that is what makes returning
  // show current data — but the scroll save/restore must NOT run then: #panel-body
  // is the shared scroller, so restoring the list's offset would scroll the page the
  // user is currently reading. A servers:changed arriving mid-page must be invisible.
  const onList = view === "list";
  const scrollTop = onList ? body.scrollTop : 0;
  render(snapshot, listView, {
    onStopRequested: handleStopRequested,
    onForceStopRequested: handleForceStopRequested,
    onKeepRunningToggled: handleKeepRunningToggled,
    onOtherStopRequested: handleOtherStopRequested,
    onOtherForceStopRequested: handleOtherForceStopRequested,
    onStopAllRequested: handleStopAllRequested,
    onGoToSourceRequested: handleGoToSourceRequested,
    onRerenderRequested: rerender,
  });
  if (onList) body.scrollTop = scrollTop;
  // Count and telemetry read the snapshot render() just drew, so they can never
  // describe a different list than the one on screen.
  renderServerCount(snapshot, serverCount);
  renderTelemetry(snapshot, telemetry);
}

function rerender() {
  if (latestSnapshot) paint(latestSnapshot);
}

// docs/IPC.md v1.4. The deliberate counterpart to paint(): resources:changed arrives
// every couple of seconds, and running it through paint() would rebuild the list that
// often — closing any open row, dropping the user's hover mid-click and resetting
// scroll. So it patches the drawn text in place and touches nothing else.
//
// The cached snapshot is updated alongside the DOM, and that is not optional: the next
// servers:changed or rerender() (a nerd-mode toggle, an expand click) repaints from
// latestSnapshot, and without this the fresh figures would visibly snap back to
// whatever the last structural event carried.
function applyResources(samples) {
  if (latestSnapshot) {
    const byId = new Map((samples.samples ?? []).map((s) => [s.id, s.usage]));
    const everyRow = [
      ...latestSnapshot.projects.flatMap((g) => g.servers),
      ...latestSnapshot.watchOnly,
      ...latestSnapshot.others,
    ];
    for (const row of everyRow) {
      const usage = byId.get(row.id);
      if (usage) row.usage = usage;
    }
    // The scan that produced these figures is the most recent one that happened, so
    // the cached snapshot's timestamp is now out of date. Without this the Stats
    // footer would keep showing the last STRUCTURAL scan's time while live numbers
    // moved beside it — a clock that says one thing while the data says another.
    if (samples.scannedAt) latestSnapshot.scannedAt = samples.scannedAt;
  }
  // Only the list view holds resource nodes; while a page is open there is nothing
  // on screen to patch, and the cached snapshot above already carries the values for
  // when the user comes back.
  if (view === "list") {
    updateResources(samples, listView);
    // In place, like everything else here: renderTelemetry only rewrites the footer's
    // own text and never touches the list, so the open rows, hover and scroll the rest
    // of this function protects are unaffected.
    if (latestSnapshot) renderTelemetry(latestSnapshot, telemetry);
  }
}

// A stopped row simply stops being in the next snapshot, and the list redraws
// without it. There is deliberately no exit animation: a system panel does not
// choreograph its own list, and the row's real feedback is the "Stopping…"
// state on the button followed by the row's absence.

// ---------- Dialogs ----------

function closeDialog() {
  overlayRoot.textContent = "";
}

/**
 * @param {{title: string, body: string, confirmLabel: string, danger?: boolean, onConfirm: () => void}} opts
 */
function openConfirmDialog(opts) {
  closeDialog();
  const overlay = document.createElement("div");
  overlay.className = "dialog-overlay";

  const dialog = document.createElement("div");
  dialog.className = "dialog";
  dialog.setAttribute("role", "alertdialog");
  dialog.setAttribute("aria-modal", "true");

  const title = document.createElement("h2");
  title.className = "dialog-title";
  title.textContent = opts.title;
  dialog.appendChild(title);

  const bodyEl = document.createElement("p");
  bodyEl.className = "dialog-body";
  bodyEl.textContent = opts.body;
  dialog.appendChild(bodyEl);

  const actions = document.createElement("div");
  actions.className = "dialog-actions";

  const cancelBtn = document.createElement("button");
  cancelBtn.type = "button";
  // is-default carries the default-button affordance structurally, so Cancel
  // reads as the default even when a pointer-opened dialog suppresses the focus
  // ring. Without it the red confirm was the only weighted button on screen.
  cancelBtn.className = "dialog-button is-default";
  cancelBtn.textContent = "Cancel";
  cancelBtn.addEventListener("click", closeDialog);

  const confirmBtn = document.createElement("button");
  confirmBtn.type = "button";
  confirmBtn.className = "dialog-button" + (opts.danger ? " is-danger" : "");
  confirmBtn.textContent = opts.confirmLabel;
  confirmBtn.addEventListener("click", () => {
    closeDialog();
    opts.onConfirm();
  });

  // macOS alert grammar: the default button sits in the trailing corner, with
  // the other choice to its left. Cancel is the default here (every dialog in
  // this panel confirms a stop), so Cancel goes last — appended second, not
  // first. Focus still lands on Cancel below; only the order changes.
  actions.appendChild(confirmBtn);
  actions.appendChild(cancelBtn);

  dialog.appendChild(actions);
  overlay.appendChild(dialog);
  overlayRoot.appendChild(overlay);

  // N2: never destructive by surprise. Every dialog here confirms a stop, so
  // the default focus and the default key (Enter/Space on the focused button)
  // must land on Cancel, not on the destructive action. Escape also dismisses.
  cancelBtn.focus();
  overlay.addEventListener("keydown", (e) => {
    if (e.key === "Escape") closeDialog();
  });
}

function showToast(message) {
  const toast = document.createElement("div");
  toast.className = "toast";
  toast.textContent = message;
  overlayRoot.appendChild(toast);
  setTimeout(() => toast.remove(), 4000);
}

// ---------- Go to source (F11) ----------
// The menu is built into #overlay-root and positioned `fixed`, never inside the
// row that opened it: #panel-body scrolls, so an anchored popover placed within
// it would be clipped, and a mid-flow servers:changed re-render rebuilds those
// rows and would destroy an open menu. Same reasoning as the dialogs.

function openSourceMenu(server, anchorEl) {
  closeMenu();

  const menu = document.createElement("div");
  menu.className = "menu";
  menu.setAttribute("role", "menu");

  // The path is shown, not just acted on: "go to source" is only trustworthy if
  // the user can see where it would take them before choosing.
  const path = document.createElement("div");
  path.className = "menu-path";
  path.textContent = server.projectPath;
  menu.appendChild(path);

  /** @param {"editor"|"finder"|"copy"} action */
  const addItem = (iconName, label, action) => {
    const item = document.createElement("button");
    item.type = "button";
    item.className = "menu-item";
    item.setAttribute("role", "menuitem");
    item.appendChild(icon(iconName, 12));
    item.appendChild(document.createTextNode(label));
    item.addEventListener("click", () => {
      closeMenu();
      runSourceAction(server, action);
    });
    menu.appendChild(item);
    return item;
  };

  // The editor item is omitted entirely when the user has chosen "Finder only" in
  // Settings — rather than kept and quietly pointed at Finder. A menu item that says
  // "Open in Editor" and opens Finder would be the panel telling a small lie, and
  // "Reveal in Finder" below already does that job under its own name.
  const editorChoice = loadEditor();
  const first =
    editorChoice === "finder"
      ? addItem("folder-open", "Reveal in Finder", "finder")
      : addItem("code", "Open in Editor", "editor");
  if (editorChoice !== "finder") addItem("folder-open", "Reveal in Finder", "finder");
  addItem("copy", "Copy path", "copy");

  overlayRoot.appendChild(menu);

  // Anchor to the trigger, then keep the menu inside the 380×520 panel: flip
  // above when it would overflow the bottom, and clamp horizontally. A menu
  // that opens off-screen is the same as no menu.
  const rect = anchorEl.getBoundingClientRect();
  const { offsetWidth: mw, offsetHeight: mh } = menu;
  const margin = 6;
  let left = rect.right - mw;
  let top = rect.bottom + 4;
  if (top + mh > window.innerHeight - margin) top = rect.top - mh - 4;
  menu.style.left = `${Math.max(margin, Math.min(left, window.innerWidth - mw - margin))}px`;
  menu.style.top = `${Math.max(margin, top)}px`;

  const onKeydown = (e) => {
    if (e.key === "Escape") {
      closeMenu();
      anchorEl.focus();
    }
  };
  const onPointerDown = (e) => {
    if (!menu.contains(e.target)) closeMenu();
  };

  document.addEventListener("keydown", onKeydown);
  // Capture phase: an outside click must dismiss the menu before it can
  // activate whatever it landed on.
  document.addEventListener("pointerdown", onPointerDown, true);

  closeMenu = () => {
    document.removeEventListener("keydown", onKeydown);
    document.removeEventListener("pointerdown", onPointerDown, true);
    menu.remove();
    closeMenu = () => {};
  };

  first.focus();
}

async function runSourceAction(server, action) {
  if (action === "copy") {
    // UI-side by design — docs/IPC.md v1.1 defines no clipboard command. The
    // fallback covers webviews where the async clipboard API is unavailable or
    // refused; if both fail the user is told rather than left guessing.
    try {
      await navigator.clipboard.writeText(server.projectPath);
      showToast("Path copied.");
    } catch {
      if (copyViaFallback(server.projectPath)) {
        showToast("Path copied.");
      } else {
        showToast("Couldn't copy the path.");
      }
    }
    return;
  }

  // The editor id rides along only for the editor action; "finder" is not an editor
  // value in the contract (docs/IPC.md v1.3), it is the `how`.
  const launched = await openProject(
    server.id,
    action,
    action === "editor" ? loadEditor() : undefined,
  );
  if (!launched) {
    // The core reports honestly when nothing could be launched (no editor
    // installed, or the id no longer resolves). Never claim it worked.
    showToast(
      action === "editor"
        ? "Couldn't open an editor for this project."
        : "Couldn't open this project in Finder.",
    );
  }
}

/** execCommand("copy") via a temporary selection — deprecated, but it is the
 * only clipboard path that works without the async API's permission model. */
function copyViaFallback(text) {
  const field = document.createElement("textarea");
  field.value = text;
  field.setAttribute("readonly", "");
  field.style.position = "fixed";
  field.style.opacity = "0";
  document.body.appendChild(field);
  field.select();
  let ok = false;
  try {
    ok = document.execCommand("copy");
  } catch {
    ok = false;
  }
  field.remove();
  return ok;
}

function handleGoToSourceRequested(server, anchorEl) {
  openSourceMenu(server, anchorEl);
}

// ---------- Stop flows (dev server) ----------

function handleStopRequested(server, projectName) {
  openConfirmDialog({
    title: "Stop this server?",
    body: whatThisStopsForDevServer(projectName, server),
    confirmLabel: "Stop",
    danger: true,
    onConfirm: () => {
      performStop(server.id);
    },
  });
}

async function performStop(id) {
  const state = rowState.get(id) || {};
  state.stopPending = true;
  rowState.set(id, state);
  rerender();

  const outcome = await stopServer(id);

  const current = rowState.get(id) || {};
  current.stopPending = false;
  if (outcome.result === "still_running") {
    // F8 step 4: show plainly, offer Force Stop as a separate, separately
    // confirmed action. Never auto-escalate.
    current.forceEligible = true;
    current.stillRunningMessage = outcome.message;
  } else if (outcome.result === "refused") {
    current.forceEligible = false;
    showToast(outcome.message);
  } else {
    // "stopped" — the server disappears from the next snapshot; nothing to keep.
    current.forceEligible = false;
  }
  rowState.set(id, current);
  rerender();
}

function handleForceStopRequested(server, projectName) {
  openConfirmDialog({
    title: "Force stop?",
    body: `${whatThisStopsForDevServer(projectName, server)} It will not get a chance to finish or save anything.`,
    confirmLabel: "Force Stop",
    danger: true,
    onConfirm: () => {
      performForceStop(server.id);
    },
  });
}

async function performForceStop(id) {
  const state = rowState.get(id) || {};
  state.stopPending = true;
  rowState.set(id, state);
  rerender();

  const outcome = await forceStop(id);

  const current = rowState.get(id) || {};
  current.stopPending = false;
  if (outcome.result !== "stopped") {
    // Still refuses even to a force stop — surface it plainly, never hide it.
    current.forceEligible = outcome.result === "still_running";
    showToast(outcome.message);
  } else {
    current.forceEligible = false;
  }
  rowState.set(id, current);
  rerender();
}

// ---------- Stop flow (Other: part_of_app / background_service) ----------

function handleOtherStopRequested(other) {
  openConfirmDialog({
    title: "Stop this?",
    body: whatThisStopsForOther(other),
    confirmLabel: "Stop",
    danger: true,
    onConfirm: () => performOtherStop(other),
  });
}

async function performOtherStop(other) {
  const state = rowState.get(other.id) || {};
  state.stopPending = true;
  rowState.set(other.id, state);
  rerender();

  const outcome = await stopServer(other.id);

  const current = rowState.get(other.id) || {};
  current.stopPending = false;
  if (outcome.result === "still_running") {
    current.forceEligible = true;
    current.stillRunningMessage = outcome.message;
  } else if (outcome.result === "refused") {
    current.forceEligible = false;
    showToast(outcome.message);
  } else {
    current.forceEligible = false;
  }
  rowState.set(other.id, current);
  rerender();
}

function handleOtherForceStopRequested(other) {
  openConfirmDialog({
    title: "Force stop?",
    body: `${whatThisStopsForOther(other)} It will not get a chance to finish or save anything.`,
    confirmLabel: "Force Stop",
    danger: true,
    onConfirm: () => performOtherForceStop(other),
  });
}

async function performOtherForceStop(other) {
  const state = rowState.get(other.id) || {};
  state.stopPending = true;
  rowState.set(other.id, state);
  rerender();

  const outcome = await forceStop(other.id);

  const current = rowState.get(other.id) || {};
  current.stopPending = false;
  if (outcome.result !== "stopped") {
    current.forceEligible = outcome.result === "still_running";
    showToast(outcome.message);
  } else {
    current.forceEligible = false;
  }
  rowState.set(other.id, current);
  rerender();
}

// ---------- Keep Running ----------

async function handleKeepRunningToggled(server, keep) {
  const state = rowState.get(server.id) || {};
  state.keepRunningPending = true;
  rowState.set(server.id, state);
  rerender();

  await setKeepRunning(server.id, keep);

  const current = rowState.get(server.id) || {};
  current.keepRunningPending = false;
  rowState.set(server.id, current);

  // servers:changed only fires when the Server set or health actually changes
  // (docs/IPC.md) — keepRunning is neither, so the backend is not guaranteed to
  // emit for this. Patch latestSnapshot directly rather than depending on an
  // event that may never arrive. If the backend does emit anyway, the next
  // payload just confirms the same value.
  if (latestSnapshot) {
    for (const group of latestSnapshot.projects) {
      const match = group.servers.find((s) => s.id === server.id);
      if (match) match.keepRunning = keep;
    }
  }
  rerender();
}

// ---------- Stop all dev servers ----------
// Built from snapshot.projects only — there is no flattened "all servers" array
// anywhere in this codebase for a bulk action to accidentally reach into.

function handleStopAllRequested() {
  if (!latestSnapshot) return;
  const servers = latestSnapshot.projects.flatMap((g) =>
    g.servers.map((s) => ({ server: s, projectName: g.project })),
  );
  if (servers.length === 0) return;

  const projectNames = [...new Set(servers.map((s) => s.projectName))];
  openConfirmDialog({
    title: "Stop all development servers?",
    body: `This stops ${servers.length} development server${servers.length === 1 ? "" : "s"} across ${projectNames.length} project${projectNames.length === 1 ? "" : "s"}: ${projectNames.join(", ")}. Nothing outside development servers is touched.`,
    confirmLabel: "Stop all",
    danger: true,
    onConfirm: performStopAll,
  });
}

async function performStopAll() {
  if (!latestSnapshot) return;
  for (const group of latestSnapshot.projects) {
    for (const server of group.servers) {
      const state = rowState.get(server.id) || {};
      state.stopPending = true;
      rowState.set(server.id, state);
    }
  }
  rerender();

  const outcomes = await stopAllDevServers();

  // Each still_running outcome gets its own, separate Force confirmation —
  // never an auto "force all". We just surface the affordance per row here;
  // the user drives each Force Stop individually via the normal row flow.
  for (const outcome of outcomes) {
    const current = rowState.get(outcome.id) || {};
    current.stopPending = false;
    if (outcome.result === "still_running") {
      current.forceEligible = true;
      current.stillRunningMessage = outcome.message;
    } else if (outcome.result === "refused") {
      current.forceEligible = false;
      showToast(outcome.message);
    } else {
      current.forceEligible = false;
    }
    rowState.set(outcome.id, current);
  }
  rerender();
}

// ---------- Cadence coupling (D) ----------
// Edge-triggered, idempotent: panel_opened() fires only on a false->true
// transition, panel_closed() only on true->false. Several browser signals are
// OR'd together because it's unclear which one the eventual window-show/hide
// wiring in src-tauri will actually produce (see report) — but the edge-trigger
// guarantees that no matter how many signals fire for one real transition, the
// backend hears exactly one call per transition.
let panelIsVisible = document.visibilityState === "visible" && document.hasFocus();

function setPanelVisible(visible) {
  if (visible === panelIsVisible) return;
  panelIsVisible = visible;
  if (visible) {
    panelOpened();
  } else {
    panelClosed();
  }
}

document.addEventListener("visibilitychange", () => {
  setPanelVisible(document.visibilityState === "visible");
});
window.addEventListener("focus", () => setPanelVisible(true));
window.addEventListener("blur", () => setPanelVisible(false));

if (window.__TAURI__) {
  // Belt-and-suspenders: also listen for the Tauri window's own focus event,
  // in case the webview's DOM focus/blur don't fire reliably inside a
  // tray-toggled panel window. Wrapped defensively because it must degrade
  // rather than break: if this API is absent or throws, the DOM focus/blur
  // listeners above still cover the common case, and a failure here must not
  // blank the whole panel.
  try {
    window.__TAURI__.window
      .getCurrentWindow()
      .onFocusChanged(({ payload: focused }) => setPanelVisible(focused));
  } catch (err) {
    console.warn("Tauri window focus listener unavailable:", err);
  }

  // The tray menu's "Settings…" and "Help" items (lib.rs) show the window and
  // emit "navigate". Wrapped like the focus listener above: if the event API is
  // absent the tray items still show the panel, and ⌘,/⌘? remain as the way in.
  try {
    window.__TAURI__.event.listen("navigate", ({ payload }) => {
      if (payload === "settings") openSettings();
      else if (payload === "help") openHelp();
    });
  } catch (err) {
    console.warn("Tauri navigate listener unavailable:", err);
  }
}

// ---------- Boot ----------

async function boot() {
  refreshButton.addEventListener("click", async () => {
    refreshButton.classList.add("is-spinning");
    try {
      const snapshot = await refreshNow();
      paint(snapshot);
    } catch (err) {
      // refresh_now rejects when the scan itself failed (lsof/ps unavailable). The
      // spinner stopping with nothing else changing would read as "refreshed, still
      // the same" — the silent-failure shape N3 forbids. Say it instead, and mark
      // the list already on screen as no longer verified (docs/IPC.md v1.2).
      console.warn("refresh failed:", err);
      if (latestSnapshot) paint({ ...latestSnapshot, scanFailed: true });
      showToast("Couldn't scan just now. Showing the last result.");
    } finally {
      refreshButton.classList.remove("is-spinning");
    }
  });

  onServersChanged((snapshot) => paint(snapshot));
  // Separate subscription for the separate event (docs/IPC.md v1.4) — see
  // applyResources for why this must not go through paint().
  onResourcesChanged((samples) => applyResources(samples));

  // servers:changed is push-only; refresh_now() is the only pull command, so
  // the initial paint has no choice but to call it (it also refetches titles —
  // an N1 cost worth flagging, see report, but there is no lighter read in
  // IPC.md).
  //
  // If that very first scan fails, this must NOT throw: an unhandled rejection here
  // aborts the rest of boot (panel_opened never fires, so the cadence never rises)
  // and leaves a blank panel explaining nothing — the worst version of the failure
  // A5 exists to make honest, in the path a user is most likely to meet it. Paint an
  // empty snapshot flagged scanFailed instead, which says "couldn't scan just now"
  // rather than showing an empty list that would read as "nothing running".
  try {
    paint(await refreshNow());
  } catch (err) {
    console.warn("initial scan failed:", err);
    paint({
      projects: [],
      others: [],
      watchOnly: [],
      scannedAt: new Date().toISOString(),
      scanFailed: true,
    });
  }

  if (panelIsVisible) {
    panelOpened();
  }
}

boot();
