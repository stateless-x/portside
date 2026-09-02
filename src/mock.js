// Mock backend, shaped exactly like docs/IPC.md, so the UI develops and can be
// verified before the Rust side lands. Used only when window.__TAURI__ is absent
// (see api.js). Deleting this file and the mock branch in api.js is the entire
// cost of the real backend landing — nothing else should need to change.

/**
 * @typedef {{ number: number, family: "v4"|"v6", reachability: "localhost"|"all_interfaces" }} Port
 * @typedef {{ id: string, pid: number, package: string|null, projectPath: string|null,
 *   title: string|null, command: string, ports: Port[], uptimeSeconds: number,
 *   health: "responding"|"not_responding"|"unknown", unattended: boolean,
 *   keepRunning: boolean }} Server
 * @typedef {{ project: string, servers: Server[] }} ProjectGroup
 * @typedef {{ id: string, label: string, reason: "your_own_tool"|"part_of_macos",
 *   ports: Port[], uptimeSeconds: number }} WatchOnlyServer
 * @typedef {{ id: string, label: string, kind: "part_of_app"|"background_service",
 *   guessedProject: string|null, ports: Port[] }} OtherServer
 * @typedef {{ projects: ProjectGroup[], watchOnly: WatchOnlyServer[],
 *   others: OtherServer[], scannedAt: string, scanFailed: boolean }} Snapshot
 * @typedef {{ id: string, result: "stopped"|"still_running"|"refused",
 *   message: string }} StopOutcome
 */

function port(number, family, reachability = "localhost") {
  return { number, family, reachability };
}

/** @type {Snapshot} */
function buildInitialSnapshot() {
  return {
    scannedAt: new Date().toISOString(),
    // docs/IPC.md v1.2. The mock always scans successfully — there is no real `lsof`
    // to fail — so this is a constant here, present so the UI is built against the
    // full Snapshot shape rather than a field it only meets in production.
    scanFailed: false,
    projects: [
      {
        // Case: a project with two servers.
        project: "vala-platform",
        servers: [
          {
            id: "pid-1001:4399",
            pid: 1001,
            package: "apps/web",
            projectPath: "/Users/dev/code/vala-platform",
            title: "Vala Platform — Dev",
            command: "pnpm dev",
            ports: [port(4399, "v4")],
            uptimeSeconds: 3 * 3600 + 12 * 60,
            health: "responding",
            unattended: true,
            keepRunning: false,
          },
          {
            id: "pid-1002:4321",
            pid: 1002,
            package: "apps/api",
            projectPath: "/Users/dev/code/vala-platform",
            title: null,
            command: "cargo run --bin api",
            ports: [port(4321, "v4"), port(5555, "v4")],
            uptimeSeconds: 3 * 86400,
            // Case: the highest-value signal — held port, nothing answers.
            health: "not_responding",
            unattended: true,
            keepRunning: false,
          },
        ],
      },
      {
        // Case: two projects sharing port 4399 on different families — NOT a
        // duplicate, do not hide either row.
        project: "openclaw-worker",
        servers: [
          {
            id: "pid-2001:4399v6",
            pid: 2001,
            package: null,
            projectPath: "/Users/dev/code/openclaw-worker",
            title: "Worker Status",
            command: "node worker.js",
            ports: [port(4399, "v6")],
            uptimeSeconds: 41 * 60,
            health: "responding",
            unattended: false,
            keepRunning: false,
          },
        ],
      },
      {
        // Case: health "unknown" — must not read as good or bad.
        // Also: a server with a Keep Running mark — should recede visually.
        // Its port is deliberately all_interfaces so the kept + "on your
        // network" combination is covered: a Keep Running mark must never dim a
        // security signal, and without this fixture that interaction is untested.
        project: "scratch-experiment",
        servers: [
          {
            id: "pid-3001:8080",
            pid: 3001,
            package: null,
            projectPath: "/Users/dev/code/scratch-experiment",
            title: null,
            command: "python -m http.server 8080",
            ports: [port(8080, "v4", "all_interfaces")],
            uptimeSeconds: 6 * 86400,
            health: "unknown",
            unattended: true,
            keepRunning: true,
          },
        ],
      },
      {
        // Case: all_interfaces reachability — flag network exposure visibly.
        project: "db-sandbox",
        servers: [
          {
            id: "pid-4001:5432",
            pid: 4001,
            package: null,
            projectPath: "/Users/dev/code/db-sandbox",
            title: null,
            command: "postgres -D data",
            ports: [port(5432, "v4", "all_interfaces")],
            uptimeSeconds: 12 * 3600,
            health: "responding",
            unattended: false,
            keepRunning: false,
          },
        ],
      },
    ],
    watchOnly: [
      {
        // Case: watch-only tool. No stop control, ever.
        id: "watch-openclaw-agent",
        label: "openclaw",
        reason: "your_own_tool",
        ports: [port(9000, "v4")],
        uptimeSeconds: 9 * 86400,
      },
      {
        id: "watch-macos-airplay",
        label: "AirPlay Receiver",
        reason: "part_of_macos",
        ports: [port(7000, "v4"), port(7000, "v6")],
        uptimeSeconds: 30 * 86400,
      },
    ],
    others: [
      {
        // Case: background service with a guessed project — must render as
        // uncertain, never as fact.
        id: "other-docker-db",
        label: "Docker Desktop",
        kind: "background_service",
        guessedProject: "vala-platform",
        ports: [port(5433, "v4"), port(6379, "v4")],
      },
      {
        // Case: part of app, no guess at all.
        id: "other-vscode-helper",
        label: "Visual Studio Code",
        kind: "part_of_app",
        guessedProject: null,
        ports: [port(3000, "v4")],
      },
    ],
  };
}

export function createMockBackend() {
  let snapshot = buildInitialSnapshot();
  const keepRunningByKey = new Map(); // "project::command" -> boolean, mirrors F10 persistence semantics
  /** @type {Set<(s: Snapshot) => void>} */
  const listeners = new Set();
  let cadence = "closed"; // "open" (3s) | "closed" (15s) — only observable via log for now

  function clone(value) {
    return JSON.parse(JSON.stringify(value));
  }

  function findServer(id) {
    for (const group of snapshot.projects) {
      const found = group.servers.find((s) => s.id === id);
      if (found) return found;
    }
    return null;
  }

  function emitChanged() {
    snapshot = { ...snapshot, scannedAt: new Date().toISOString() };
    const payload = clone(snapshot);
    for (const listener of listeners) listener(payload);
  }

  // Simulate the backend noticing real change over time so the re-render path
  // (not just the initial paint) is exercised: after 4s, the not_responding
  // server on port 4321 gets stopped externally by the user and its group
  // updates uptime — verifies servers:changed drives re-render, not polling.
  let driftTimer = null;
  function startDrift() {
    if (driftTimer) return;
    driftTimer = setInterval(() => {
      for (const group of snapshot.projects) {
        for (const server of group.servers) {
          server.uptimeSeconds += 3;
        }
      }
      emitChanged();
    }, 4000);
  }

  return {
    panelOpened() {
      cadence = "open";
      startDrift();
      return Promise.resolve();
    },
    panelClosed() {
      cadence = "closed";
      if (driftTimer) {
        clearInterval(driftTimer);
        driftTimer = null;
      }
      return Promise.resolve();
    },
    refreshNow() {
      snapshot = { ...snapshot, scannedAt: new Date().toISOString() };
      return Promise.resolve(clone(snapshot));
    },
    setKeepRunning(id, keep) {
      const server = findServer(id);
      if (server) {
        server.keepRunning = keep;
        keepRunningByKey.set(id, keep);
        emitChanged();
      }
      return Promise.resolve();
    },
    stopServer(id) {
      const server = findServer(id);
      if (!server) {
        return Promise.resolve({
          id,
          result: "refused",
          message: "This server is no longer present.",
        });
      }
      // Simulate: two different servers refuse to let go on the first polite
      // attempt, so both the single-row Force Stop path and "Stop all" ending
      // with more than one still_running outcome (each needing its own,
      // separate Force confirmation — never a combined "force all") are
      // exercised.
      if (id === "pid-1002:4321") {
        return Promise.resolve({
          id,
          result: "still_running",
          message:
            "vala-platform · apps/api did not stop. It may still be finishing a request.",
        });
      }
      if (id === "pid-4001:5432") {
        return Promise.resolve({
          id,
          result: "still_running",
          message:
            "db-sandbox did not stop. It may still be finishing a request.",
        });
      }
      for (const group of snapshot.projects) {
        const idx = group.servers.findIndex((s) => s.id === id);
        if (idx !== -1) {
          const [stopped] = group.servers.splice(idx, 1);
          emitChanged();
          return Promise.resolve({
            id,
            result: "stopped",
            message: `${group.project}${stopped.package ? " · " + stopped.package : ""} stopped.`,
          });
        }
      }
      return Promise.resolve({
        id,
        result: "refused",
        message: "This server is no longer present.",
      });
    },
    stopAllDevServers() {
      const ids = snapshot.projects.flatMap((g) => g.servers.map((s) => s.id));
      return Promise.all(ids.map((id) => this.stopServer(id)));
    },
    forceStop(id) {
      for (const group of snapshot.projects) {
        const idx = group.servers.findIndex((s) => s.id === id);
        if (idx !== -1) {
          const [stopped] = group.servers.splice(idx, 1);
          emitChanged();
          return Promise.resolve({
            id,
            result: "stopped",
            message: `${group.project}${stopped.package ? " · " + stopped.package : ""} stopped.`,
          });
        }
      }
      return Promise.resolve({
        id,
        result: "refused",
        message: "This server is no longer present.",
      });
    },
    // docs/IPC.md v1.1. Mirrors the core's guard rather than assuming the UI
    // behaves: only a DevServer id with a known Project resolves, so the mock
    // returns false for an others[]/watchOnly[] id exactly as Rust does.
    // docs/IPC.md v1.3 adds `editor`. Mirrored here only so the log shows which
    // editor the UI asked for; the mock launches nothing, and the closed-set guard
    // that matters lives in the core, which is where an unknown value degrades to
    // Finder rather than becoming a command.
    openProject(id, how, editor) {
      const server = findServer(id);
      if (!server || !server.projectPath) {
        console.log(`[mock] open_project refused: ${id}`);
        return Promise.resolve(false);
      }
      const which = how === "editor" ? ` (${editor ?? "vscode"})` : "";
      console.log(`[mock] open_project ${how}${which}: ${server.projectPath}`);
      return Promise.resolve(true);
    },
    onServersChanged(handler) {
      listeners.add(handler);
      return () => listeners.delete(handler);
    },
  };
}
