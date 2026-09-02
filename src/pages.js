// The Settings and Help pages.
//
// Both render into a container inside #panel-body (see index.html) that main.js
// swaps in place of the list. They are built here rather than in main.js so main.js
// stays the wiring/flow file it already is, and pure of copy.
//
// Everything Settings stores is client-side (localStorage). The backend keeplist
// remains the only backend persistence in the app — no preference here crosses IPC,
// except the editor choice, which is passed per call as an argument to open_project
// (docs/IPC.md v1.3) rather than stored on the Rust side.

import { icon } from "./icons.js";

/** Shown in the Settings footer. Must match src-tauri/Cargo.toml's `version` and
 * tauri.conf.json's `version` — one constant here rather than a sync mechanism,
 * because a menu-bar panel has no runtime that could read Cargo.toml. */
export const APP_NAME = "Portside";
export const APP_VERSION = "0.0.1";

// ---------- Editor preference ----------

/** The closed set the "Open in Editor" action may use. Fixed allowlist, by design:
 * the value chosen here is passed to open_project (docs/IPC.md v1.3), and the Rust
 * side maps each id to a hardcoded command chain. A value outside this set is never
 * a program name the user typed — it degrades to Finder in the core.
 *
 * "finder" is a UI-level choice with no IPC editor value of its own: it means "don't
 * use an editor at all", and is sent as open_project's `how: "finder"` instead. */
export const EDITORS = [
  { id: "vscode", label: "Visual Studio Code" },
  { id: "cursor", label: "Cursor" },
  { id: "zed", label: "Zed" },
  { id: "sublime", label: "Sublime Text" },
  { id: "finder", label: "Finder only" },
];

const EDITOR_KEY = "portside.editor";

export function loadEditor() {
  try {
    const stored = localStorage.getItem(EDITOR_KEY);
    return EDITORS.some((e) => e.id === stored) ? stored : "vscode";
  } catch {
    return "vscode";
  }
}

function persistEditor(id) {
  try {
    localStorage.setItem(EDITOR_KEY, id);
  } catch {
    /* preference not persisted; the session still honours the choice */
  }
}

// ---------- Asset slots ----------

/** An illustration slot that stays invisible unless the file is actually there.
 *
 * The container starts `hidden` and the image is probed with a detached Image() —
 * appended only once it has decoded. That ordering is the whole point: inserting an
 * <img> first and hiding it on error would flash a broken-image glyph, and reserving
 * height before the load would shift the layout when the file is absent. With no
 * asset present the slot occupies nothing at all.
 *
 * @param {string} src
 * @param {string} alt
 * @param {string} className
 */
function assetSlot(src, alt, className) {
  const slot = document.createElement("div");
  slot.className = className;
  slot.hidden = true;

  const probe = new Image();
  probe.onload = () => {
    const img = document.createElement("img");
    img.src = src;
    img.alt = alt;
    slot.appendChild(img);
    slot.hidden = false;
    // Lets a caller stand something else down once the real artwork arrives — the
    // empty state uses this to drop its glyph rather than show both.
    slot.dispatchEvent(new CustomEvent("portside:asset-loaded"));
  };
  // No onerror branch on purpose: absent is the expected state until the user drops
  // the file in, and it is not a failure to report.
  probe.src = src;

  return slot;
}

/** Help header illustration. Expected asset: src/assets/help-header.png,
 * 686×216 @2x → 343×108 on screen. That is the page's content column, not the full
 * 380px panel: the Help page scrolls, so the scrollbar takes 9px and the page's own
 * padding another 14px each side. See .asset-slot-help in styles.css. */
export function helpHeaderSlot() {
  return assetSlot("assets/help-header.png", "", "asset-slot asset-slot-help");
}

/** Empty-state illustration. Expected asset: src/assets/empty-harbor.png,
 * 480×360 @2x → 240×180 on screen. */
export function emptyStateSlot() {
  return assetSlot("assets/empty-harbor.png", "", "asset-slot asset-slot-empty");
}

// ---------- Shared page pieces ----------

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

/** The back affordance every page carries. Escape does the same thing (wired in
 * main.js) — this is the visible half of the same one way out. */
function pageHeader(title, onBack) {
  const header = el("div", "page-header");

  const back = document.createElement("button");
  back.type = "button";
  back.className = "back-button";
  back.appendChild(icon("left", 11));
  back.appendChild(el("span", null, "Back"));
  back.addEventListener("click", onBack);
  header.appendChild(back);

  header.appendChild(el("h2", "page-title", title));
  return { header, back };
}

function fieldRow(labelText, control, hintText) {
  const row = el("div", "field");
  const label = el("div", "field-label", labelText);
  row.appendChild(label);
  row.appendChild(control);
  if (hintText) {
    const hint = el("div", "field-hint", hintText);
    row.appendChild(hint);
  }
  return row;
}

// ---------- Settings ----------

/**
 * @param {HTMLElement} root
 * @param {{
 *   theme: "system"|"light"|"dark",
 *   onThemeChange: (choice: "system"|"light"|"dark") => void,
 *   nerdMode: boolean,
 *   onNerdModeChange: (on: boolean) => void,
 *   onBack: () => void,
 * }} opts
 * @returns {HTMLElement} the element to focus on open
 */
export function renderSettings(root, opts) {
  root.textContent = "";
  const { header, back } = pageHeader("Settings", opts.onBack);
  root.appendChild(header);

  // --- Appearance ---
  const segmented = el("div", "segmented");
  segmented.setAttribute("role", "radiogroup");
  segmented.setAttribute("aria-label", "Appearance");
  const THEME_OPTIONS = [
    ["system", "System"],
    ["light", "Light"],
    ["dark", "Dark"],
  ];
  for (const [value, label] of THEME_OPTIONS) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "segment";
    btn.textContent = label;
    btn.setAttribute("role", "radio");
    // The selected segment is named for assistive tech, not left to colour alone —
    // the same reason the health dot never travels without its word.
    btn.setAttribute("aria-checked", String(opts.theme === value));
    btn.addEventListener("click", () => {
      opts.onThemeChange(value);
      for (const other of segmented.children) {
        other.setAttribute("aria-checked", String(other === btn));
      }
    });
    segmented.appendChild(btn);
  }
  root.appendChild(
    fieldRow("Appearance", segmented, "System follows your Mac's light or dark setting."),
  );

  // --- Stats for nerds ---
  // Same store as the title bar's toggle (main.js owns the key and both call sites
  // route through one applyNerdMode), so the two controls can never disagree.
  const statsLabel = el("label", "switch-row");
  const statsInput = document.createElement("input");
  statsInput.type = "checkbox";
  statsInput.checked = opts.nerdMode;
  statsInput.addEventListener("change", () => opts.onNerdModeChange(statsInput.checked));
  statsLabel.appendChild(statsInput);
  statsLabel.appendChild(el("span", null, "Show stats for nerds"));
  root.appendChild(
    fieldRow(
      "Detail",
      statsLabel,
      "Adds pids, paths and raw port details to every row.",
    ),
  );

  // --- Editor ---
  const select = document.createElement("select");
  select.className = "select";
  select.setAttribute("aria-label", "Open in Editor uses");
  const current = loadEditor();
  for (const editor of EDITORS) {
    const option = document.createElement("option");
    option.value = editor.id;
    option.textContent = editor.label;
    option.selected = editor.id === current;
    select.appendChild(option);
  }
  select.addEventListener("change", () => persistEditor(select.value));
  root.appendChild(
    fieldRow(
      "Open in Editor uses",
      select,
      "Only these apps. Portside never runs a command you type.",
    ),
  );

  // --- Footer ---
  const footer = el("div", "page-footer");
  footer.appendChild(el("div", "footer-name", `${APP_NAME} ${APP_VERSION}`));
  footer.appendChild(el("div", "footer-note", "Portside never touches the network."));
  root.appendChild(footer);

  return back;
}

// ---------- Help ----------

/** Every word below is sourced from CONTEXT.md's glossary, shortened to one plain
 * line each — the page is read by people who are not debugging networking. Nothing
 * here introduces a meaning the glossary does not already carry. */
const WORDS = [
  ["responding", "It answers when something connects to it. It's working."],
  [
    "not responding",
    "It's still holding its address but answers nothing. Usually it's stuck.",
  ],
  ["unknown", "Portside couldn't tell either way this time."],
  [
    "unattended",
    "Whatever started it has exited. Normal for servers a coding agent left behind.",
  ],
  [
    "on your network",
    "Other machines nearby can reach it, not just this one.",
  ],
  ["kept", "You told Portside you meant to leave this one on."],
  [
    "watch only",
    "Portside shows it but won't stop it. See below for why.",
  ],
];

/**
 * @param {HTMLElement} root
 * @param {{ onBack: () => void }} opts
 * @returns {HTMLElement} the element to focus on open
 */
export function renderHelp(root, opts) {
  root.textContent = "";
  const { header, back } = pageHeader("Help", opts.onBack);
  root.appendChild(header);

  root.appendChild(helpHeaderSlot());

  const intro = el("p", "page-text");
  intro.textContent =
    "Portside shows every local server your Mac is currently holding, which project each one came from, and whether it still answers. Servers outlive the terminal or agent that started them, and this is where you can see them all in one place. When one is safe to stop, you can stop it here.";
  root.appendChild(intro);

  root.appendChild(el("h3", "page-subhead", "What the words mean"));
  const list = el("dl", "word-list");
  for (const [term, meaning] of WORDS) {
    list.appendChild(el("dt", null, term));
    list.appendChild(el("dd", null, meaning));
  }
  root.appendChild(list);

  root.appendChild(el("h3", "page-subhead", "Why some servers can't be stopped here"));
  const why = el("p", "page-text");
  // CONTEXT.md "Watch Only": not a judgement that these matter more — an admission
  // that the tool cannot know what stopping them would cost.
  why.textContent =
    "Some servers belong to macOS, or are your own tools running on purpose rather than part of any project. Portside can't know what stopping one of those would cost you, and a panel you opened to tidy up dev servers is the wrong place to end something holding up your day. So it shows them and leaves them alone.";
  root.appendChild(why);

  const footer = el("div", "page-footer");
  footer.appendChild(el("div", "footer-name", "Made with care. Free and open source."));

  // Sponsor row, deliberately inert until there is a real destination.
  // TODO: put the sponsor URL here (e.g. https://buymeacoffee.com/<handle>) and turn
  // this into an anchor that opens externally; disabled until then so it never looks
  // like a link that goes nowhere.
  const sponsor = document.createElement("button");
  sponsor.type = "button";
  sponsor.className = "sponsor-row";
  sponsor.disabled = true;
  sponsor.appendChild(icon("coffee", 12));
  sponsor.appendChild(el("span", null, "Buy me a coffee"));
  sponsor.title = "Coming soon";
  footer.appendChild(sponsor);

  root.appendChild(footer);

  return back;
}
