// The one seam between the UI and the backend, per docs/IPC.md.
//
// Every function here matches an IPC.md command or event exactly. Nothing here
// invents a command IPC.md doesn't define. If the real Tauri runtime is present
// (window.__TAURI__, from withGlobalTauri in tauri.conf.json) we call through to
// it; otherwise we fall back to in-memory mock data shaped identically, so the
// rest of the app never knows which one it's talking to.
import { createMockBackend } from "./mock.js";

const tauri = window.__TAURI__;
const mock = tauri ? null : createMockBackend();

/**
 * @typedef {import('./mock.js').Snapshot} Snapshot
 * @typedef {import('./mock.js').StopOutcome} StopOutcome
 */

/** Switches backend scan cadence to 3s. Call when the panel becomes visible. */
export function panelOpened() {
  return tauri ? tauri.core.invoke("panel_opened") : mock.panelOpened();
}

/** Switches backend scan cadence to 15s. Call when the panel is hidden. */
export function panelClosed() {
  return tauri ? tauri.core.invoke("panel_closed") : mock.panelClosed();
}

/** Manual refresh; also refetches titles. Also used for the initial paint,
 * since servers:changed is push-only and there is no other read command. */
export function refreshNow() {
  return tauri ? tauri.core.invoke("refresh_now") : mock.refreshNow();
}

export function setKeepRunning(id, keep) {
  return tauri
    ? tauri.core.invoke("set_keep_running", { id, keep })
    : mock.setKeepRunning(id, keep);
}

/** @returns {Promise<StopOutcome>} */
export function stopServer(id) {
  return tauri ? tauri.core.invoke("stop_server", { id }) : mock.stopServer(id);
}

/** @returns {Promise<StopOutcome[]>} */
export function stopAllDevServers() {
  return tauri
    ? tauri.core.invoke("stop_all_dev_servers")
    : mock.stopAllDevServers();
}

/** Only valid after stop_server returned "still_running" for this id.
 * @returns {Promise<StopOutcome>} */
export function forceStop(id) {
  return tauri ? tauri.core.invoke("force_stop", { id }) : mock.forceStop(id);
}

/** docs/IPC.md v1.1: open a dev server's Project in the editor or Finder.
 * Copying the path is not here — it is UI-side clipboard work with no command.
 *
 * docs/IPC.md v1.3 adds the optional `editor`: which editor "editor" means. Omitted
 * (or ignored, when how is "finder") the core uses its Visual Studio Code chain, so
 * an older UI and a newer core still agree. The value is one of a closed set the
 * core matches against hardcoded commands — never a program name to run.
 *
 * @param {"editor"|"finder"} how
 * @param {"vscode"|"cursor"|"zed"|"sublime"} [editor]
 * @returns {Promise<boolean>} whether a launch actually succeeded */
export function openProject(id, how, editor) {
  return tauri
    ? tauri.core.invoke("open_project", { id, how, editor })
    : mock.openProject(id, how, editor);
}

/**
 * Subscribes to servers:changed. Returns an unsubscribe function.
 * @param {(snapshot: Snapshot) => void} handler
 */
export function onServersChanged(handler) {
  if (tauri) {
    // tauri.event.listen returns a Promise<UnlistenFn>.
    const unlistenPromise = tauri.event.listen("servers:changed", (event) => {
      handler(event.payload);
    });
    return () => unlistenPromise.then((unlisten) => unlisten());
  }
  return mock.onServersChanged(handler);
}
